use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvError, Sender};
use std::thread;
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus};
use tokio::sync::watch;

use crate::id::TerminalId;
use crate::pty::Led;
use crate::terminal::Exit;

pub const GRACE: Duration = Duration::from_secs(2);

struct Unwaited;

pub struct Leader {
    terminal: TerminalId,
    pid: Pid,
    lost: Arc<AtomicBool>,
    _kept: Sender<Infallible>,
}

impl Leader {
    pub fn waited(
        terminal: TerminalId,
        led: Led,
        leaders: &Leaders,
        woken: impl FnOnce() + Send + 'static,
    ) -> io::Result<(Leader, Receiver<Exit>)> {
        let mut child = led.child();
        let pid = Pid::from_child(&child);
        let lost = Arc::new(AtomicBool::new(false));
        let (exited, exits) = mpsc::sync_channel(1);
        let (kept, let_go) = mpsc::channel::<Infallible>();
        let loses = Arc::clone(&lost);
        let unreaped = leaders.unreaped();
        let leader = Leader { terminal, pid, lost, _kept: kept };
        thread::Builder::new().name(format!("terminal {terminal} waits")).spawn(move || {
            match ended(terminal, pid) {
                Ok(exit) => {
                    if exited.try_send(exit).is_ok() {
                        woken();
                    }
                    let Err(RecvError) = let_go.recv();
                    reaped(terminal, &mut child, exit);
                },
                Err(Unwaited) => loses.store(true, Ordering::Release),
            }
            drop(unreaped);
        })?;
        Ok((leader, exits))
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_nonzero().get().cast_unsigned()
    }

    pub fn end(self) {
        self.signalled(Signal::TERM, "SIGTERM");
        thread::sleep(GRACE);
        if self.holds() && !has_ended(self.pid) {
            let terminal = self.terminal;
            let grace = GRACE.as_secs();
            eprintln!("lune-mux: terminal {terminal}'s program outlasted {grace}s, so it is killed");
        }
    }

    fn holds(&self) -> bool {
        !self.lost.load(Ordering::Acquire)
    }

    fn signalled(&self, signal: Signal, name: &str) {
        if !self.holds() {
            return;
        }
        match process::kill_process_group(self.pid, signal) {
            Ok(()) | Err(Errno::SRCH) => {},
            Err(Errno::PERM) if has_ended(self.pid) => {},
            Err(error) => {
                let terminal = self.terminal;
                eprintln!("lune-mux: terminal {terminal}'s programs were not sent {name}: {error}");
            },
        }
    }
}

impl Drop for Leader {
    fn drop(&mut self) {
        self.signalled(Signal::KILL, "SIGKILL");
    }
}

fn ended(terminal: TerminalId, pid: Pid) -> Result<Exit, Unwaited> {
    let ended = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    loop {
        match process::waitid(WaitId::Pid(pid), ended) {
            Ok(Some(status)) => return Ok(end_of(&status)),
            Ok(None) => unreachable!("a wait for terminal {terminal}'s program without NOHANG waits"),
            Err(Errno::INTR) => {},
            Err(error) => {
                eprintln!("lune-mux: terminal {terminal}'s program could not be waited for: {error}");
                return Err(Unwaited);
            },
        }
    }
}

fn reaped(terminal: TerminalId, child: &mut Child, exit: Exit) {
    match child.wait() {
        Ok(status) => {
            assert_eq!(Exit::of(status), exit, "terminal {terminal}'s program ends as it was seen to");
        },
        Err(error) => eprintln!("lune-mux: terminal {terminal}'s program could not be reaped: {error}"),
    }
}

fn has_ended(pid: Pid) -> bool {
    let now = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    matches!(process::waitid(WaitId::Pid(pid), now), Ok(Some(_)))
}

fn end_of(status: &WaitIdStatus) -> Exit {
    if let Some(code) = status.exit_status() {
        return Exit::Code(code);
    }
    match status.terminating_signal() {
        Some(signal) => Exit::Signal(signal),
        None => unreachable!("a program waited for to its end ends by a code or a signal"),
    }
}

pub struct Leaders(watch::Sender<usize>);

impl Leaders {
    pub fn new() -> Leaders {
        Leaders(watch::Sender::new(0))
    }

    fn unreaped(&self) -> Unreaped {
        self.0.send_modify(|count| *count += 1);
        Unreaped(self.0.clone())
    }

    pub fn all_reaped(&self) -> impl Future<Output = ()> + 'static {
        let mut counted = self.0.subscribe();
        async move {
            let _ = counted.wait_for(|count| *count == 0).await;
        }
    }
}

struct Unreaped(watch::Sender<usize>);

impl Drop for Unreaped {
    fn drop(&mut self) {
        self.0.send_modify(|count| {
            assert!(*count > 0, "a leader is counted before it is reaped");
            *count -= 1;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::num::NonZeroU16;

    use lune_waits::{Began, eventually};

    use super::*;
    use crate::asked::{AbsoluteCwd, Ends, Opens, Size};
    use crate::panics::Shared;
    use crate::pty::Pty;

    fn led(script: &str) -> (Led, String) {
        let columns = NonZeroU16::new(80).unwrap();
        let rows = NonZeroU16::new(24).unwrap();
        let pty = Pty::opened(Size { columns, rows }).unwrap();
        let opens = Opens {
            cwd: AbsoluteCwd::of_path("/".to_owned()).unwrap(),
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned()],
            ends: Ends::Closes,
        };
        let led = pty.spawned(&opens, &[]).unwrap();
        let mut said = Vec::new();
        let said = eventually("the program says it is ready", || {
            let mut read = [0; 64];
            if let Ok(count) = (&pty.near).read(&mut read) {
                said.extend_from_slice(&read[..count]);
            }
            said.ends_with(b"\n").then(|| String::from_utf8_lossy(&said).trim().to_owned())
        });
        drop(pty);
        (led, said)
    }

    fn reaped(leaders: &Leaders) {
        eventually("its waiter ends", || (*leaders.0.borrow() == 0).then_some(()));
    }

    #[test]
    fn a_program_that_ignores_hangups_and_terms_is_killed_after_the_grace_and_its_waiter_ends() {
        let (led, said) = led("trap '' HUP TERM; echo trapped; while :; do sleep 1; done");
        assert_eq!(said, "trapped");
        let leaders = Leaders::new();
        let (woke, woken) = mpsc::sync_channel(1);
        let terminal = TerminalId::next(&mut None);
        let woken_by = move || woke.send(()).unwrap();
        let (leader, exits) = Leader::waited(terminal, led, &leaders, woken_by).unwrap();
        let began = Began::now();
        leader.end();
        began.lasted().at_least(GRACE, "it was given its grace");
        reaped(&leaders);
        assert_eq!(woken.try_recv(), Ok(()));
        assert_eq!(exits.try_recv(), Ok(Exit::Signal(Signal::KILL.as_raw())));
    }

    #[test]
    fn a_program_that_ends_on_a_term_ends_as_it_chooses() {
        let ends_on_term = "trap '' HUP; trap 'exit 3' TERM; echo trapped; \
                            while :; do sleep 1; done";
        let (led, said) = led(ends_on_term);
        assert_eq!(said, "trapped");
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let (leader, exits) = Leader::waited(terminal, led, &leaders, || {}).unwrap();
        leader.end();
        reaped(&leaders);
        assert_eq!(exits.try_recv(), Ok(Exit::Code(3)));
    }

    #[test]
    fn a_leader_let_go_without_its_end_while_the_model_is_held_is_killed_at_once() {
        let (led, said) = led("trap '' HUP TERM; echo trapped; while :; do sleep 1; done");
        assert_eq!(said, "trapped");
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let (leader, exits) = Leader::waited(terminal, led, &leaders, || {}).unwrap();
        let model = Shared::new(());
        let held = model.lock();
        drop(leader);
        drop(held);
        reaped(&leaders);
        assert_eq!(exits.try_recv(), Ok(Exit::Signal(Signal::KILL.as_raw())));
    }

    #[test]
    fn what_outlives_its_leader_in_the_group_is_killed_after_the_grace() {
        let outliving = r#"trap '' HUP
            sh -c 'trap "" TERM; echo $$; while :; do sleep 1; done' & wait"#;
        let (led, said) = led(outliving);
        let outlives = Pid::from_raw(said.parse().unwrap()).unwrap();
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let (leader, exits) = Leader::waited(terminal, led, &leaders, || {}).unwrap();
        leader.end();
        reaped(&leaders);
        assert_eq!(exits.try_recv(), Ok(Exit::Signal(Signal::TERM.as_raw())));
        eventually("what outlived its leader ends", || {
            (process::test_kill_process(outlives) == Err(Errno::SRCH)).then_some(())
        });
    }

    #[test]
    fn what_outlives_a_leader_that_ended_by_itself_is_killed_once_it_is_let_go() {
        let outliving = r#"trap '' HUP; said=$(mktemp -u); mkfifo "$said"
            sh -c 'trap "" TERM; echo $$ > "$1"; while :; do sleep 1; done' _ "$said" &
            read -r pid < "$said"; rm -f "$said"; echo "$pid"; exit 4"#;
        let (led, said) = led(outliving);
        let outlives = Pid::from_raw(said.parse().unwrap()).unwrap();
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let (leader, exits) = Leader::waited(terminal, led, &leaders, || {}).unwrap();
        let exit = eventually("its leader is seen to end", || exits.try_recv().ok());
        assert_eq!(exit, Exit::Code(4));
        assert_eq!(process::test_kill_process(outlives), Ok(()));
        assert_eq!(*leaders.0.borrow(), 1, "an ended leader is held until it is let go");
        leader.end();
        reaped(&leaders);
        eventually("what outlived its leader ends", || {
            (process::test_kill_process(outlives) == Err(Errno::SRCH)).then_some(())
        });
    }
}
