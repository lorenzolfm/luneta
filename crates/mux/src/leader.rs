use std::future::Future;
use std::io;
use std::process::Child;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use rustix::io::Errno;
use rustix::process::{self, Pid, Signal, WaitId, WaitIdOptions, WaitIdStatus};
use tokio::sync::watch;

use crate::id::TerminalId;
use crate::panics::Shared;
use crate::terminal::Exit;

pub const GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Leading {
    Runs,
    Ends,
    Reaped,
    Lost,
}

pub struct Leader {
    terminal: TerminalId,
    pid: Pid,
    leading: Arc<Shared<Leading>>,
    exits: Receiver<Exit>,
    let_go: SyncSender<()>,
}

impl Leader {
    pub fn waited(
        terminal: TerminalId,
        mut child: Child,
        unreaped: Unreaped,
        woken: impl FnOnce() + Send + 'static,
    ) -> io::Result<Leader> {
        let pid = Pid::from_child(&child);
        if let Ok(group) = process::getpgid(Some(pid)) {
            assert_eq!(group, pid, "terminal {terminal}'s program leads a group of its own");
        }
        let leading = Arc::new(Shared::new(Leading::Runs));
        let (exited, exits) = mpsc::sync_channel(1);
        let (let_go, held) = mpsc::sync_channel(1);
        let reaps = Arc::clone(&leading);
        thread::Builder::new().name(format!("terminal {terminal} waits")).spawn(move || {
            match ended(terminal, pid) {
                Some(exit) => {
                    if exited.try_send(exit).is_ok() {
                        woken();
                    }
                    let _ = held.recv();
                    reaped(terminal, &mut child, &reaps, exit);
                },
                None => *reaps.lock() = Leading::Lost,
            }
            drop(unreaped);
        })?;
        Ok(Leader { terminal, pid, leading, exits, let_go })
    }

    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_nonzero().get().cast_unsigned()
    }

    pub fn exited(&self) -> Option<Exit> {
        self.exits.try_recv().ok()
    }

    pub fn end(&self) {
        if !self.ending() {
            return;
        }
        thread::sleep(GRACE);
        self.killed();
        match self.let_go.try_send(()) {
            Ok(()) | Err(TrySendError::Disconnected(())) => {},
            Err(TrySendError::Full(())) => {
                unreachable!("terminal {}'s waiter is let go once", self.terminal)
            },
        }
    }

    fn ending(&self) -> bool {
        let mut leading = self.leading.lock();
        match *leading {
            Leading::Reaped | Leading::Lost => return false,
            Leading::Runs => *leading = Leading::Ends,
            Leading::Ends => unreachable!("terminal {}'s programs are ended once", self.terminal),
        }
        self.signalled(Signal::TERM, "SIGTERM");
        true
    }

    fn killed(&self) {
        let leading = self.leading.lock();
        match *leading {
            Leading::Ends => {},
            Leading::Lost => return,
            Leading::Runs | Leading::Reaped => {
                unreachable!("a leader that ends is reaped only once let go")
            },
        }
        if !has_ended(self.pid) {
            let terminal = self.terminal;
            let grace = GRACE.as_secs();
            eprintln!("lune-mux: terminal {terminal}'s program outlasted {grace}s, so it is killed");
        }
        self.signalled(Signal::KILL, "SIGKILL");
    }

    fn signalled(&self, signal: Signal, name: &str) {
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

fn ended(terminal: TerminalId, pid: Pid) -> Option<Exit> {
    let ended = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    loop {
        match process::waitid(WaitId::Pid(pid), ended) {
            Ok(status) => {
                let exit = status.as_ref().and_then(end_of);
                if exit.is_none() {
                    eprintln!("lune-mux: terminal {terminal}'s program was waited for, not to its end");
                }
                return exit;
            },
            Err(Errno::INTR) => {},
            Err(error) => {
                eprintln!("lune-mux: terminal {terminal}'s program could not be waited for: {error}");
                return None;
            },
        }
    }
}

fn reaped(terminal: TerminalId, child: &mut Child, leading: &Shared<Leading>, exit: Exit) {
    let mut reaping = leading.lock();
    assert!(
        matches!(*reaping, Leading::Runs | Leading::Ends),
        "terminal {terminal}'s program is reaped once, and only while its pid is held"
    );
    let status = child.wait();
    *reaping = Leading::Reaped;
    drop(reaping);
    match status {
        Ok(status) => {
            assert_eq!(Exit::of(status), exit, "terminal {terminal}'s program ends as it was seen to");
        },
        Err(error) => eprintln!("lune-mux: terminal {terminal}'s program could not be reaped: {error}"),
    }
}

fn has_ended(pid: Pid) -> bool {
    let now = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG;
    matches!(process::waitid(WaitId::Pid(pid), now), Ok(Some(status)) if end_of(&status).is_some())
}

fn end_of(status: &WaitIdStatus) -> Option<Exit> {
    if let Some(code) = status.exit_status() {
        return Some(Exit::Code(code));
    }
    status.terminating_signal().map(Exit::Signal)
}

pub struct Leaders(watch::Sender<usize>);

impl Leaders {
    pub fn new() -> Leaders {
        Leaders(watch::Sender::new(0))
    }

    pub fn unreaped(&self) -> Unreaped {
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

pub struct Unreaped(watch::Sender<usize>);

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
    use std::io::{BufRead as _, BufReader};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use lune_waits::{Began, eventually};

    use super::*;

    fn led(script: &str) -> (Child, String) {
        let mut child = Command::new("/bin/sh")
            .args(["-c", script])
            .process_group(0)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut said = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut said).unwrap();
        (child, said)
    }

    fn reaped(leaders: &Leaders) {
        eventually("its waiter ends", || (*leaders.0.borrow() == 0).then_some(()));
    }

    #[test]
    fn a_program_that_ignores_hangups_and_terms_is_killed_after_the_grace_and_its_waiter_ends() {
        let (child, said) = led("trap '' HUP TERM; echo trapped; while :; do sleep 1; done");
        assert_eq!(said, "trapped\n");
        let leaders = Leaders::new();
        let (woke, woken) = mpsc::sync_channel(1);
        let terminal = TerminalId::next(&mut None);
        let woken_by = move || woke.send(()).unwrap();
        let leader = Leader::waited(terminal, child, leaders.unreaped(), woken_by).unwrap();
        let began = Began::now();
        leader.end();
        began.lasted().at_least(GRACE, "it was given its grace");
        reaped(&leaders);
        assert_eq!(woken.try_recv(), Ok(()));
        assert_eq!(leader.exited(), Some(Exit::Signal(Signal::KILL.as_raw())));
        assert!(!leader.ending(), "a reaped leader is not signalled");
    }

    #[test]
    fn a_program_that_ends_on_a_term_ends_as_it_chooses() {
        let (child, said) = led("trap 'exit 3' TERM; echo trapped; while :; do sleep 1; done");
        assert_eq!(said, "trapped\n");
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let leader = Leader::waited(terminal, child, leaders.unreaped(), || {}).unwrap();
        leader.end();
        reaped(&leaders);
        assert_eq!(leader.exited(), Some(Exit::Code(3)));
    }

    #[test]
    fn what_outlives_its_leader_in_the_group_is_killed_after_the_grace() {
        let outliving = r#"sh -c 'trap "" HUP TERM; echo $$; while :; do sleep 1; done' & wait"#;
        let (child, said) = led(outliving);
        let outlives = Pid::from_raw(said.trim().parse().unwrap()).unwrap();
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let leader = Leader::waited(terminal, child, leaders.unreaped(), || {}).unwrap();
        leader.end();
        reaped(&leaders);
        assert_eq!(leader.exited(), Some(Exit::Signal(Signal::TERM.as_raw())));
        eventually("what outlived its leader ends", || {
            (process::test_kill_process(outlives) == Err(Errno::SRCH)).then_some(())
        });
    }

    #[test]
    fn what_outlives_a_leader_that_ended_by_itself_is_killed_once_it_is_let_go() {
        let outliving = r#"sh -c 'trap "" HUP TERM; echo $$; while :; do sleep 1; done' & exit 4"#;
        let (child, said) = led(outliving);
        let outlives = Pid::from_raw(said.trim().parse().unwrap()).unwrap();
        let leaders = Leaders::new();
        let terminal = TerminalId::next(&mut None);
        let leader = Leader::waited(terminal, child, leaders.unreaped(), || {}).unwrap();
        let exit = eventually("its leader is seen to end", || leader.exited());
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
