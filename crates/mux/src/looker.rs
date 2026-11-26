use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;

use lune_protocol::shell::Front;

use crate::api;
use crate::front;
use crate::id::TerminalId;
use crate::panics::{self, Panicked};
use crate::process::{self, Group};

const IN_FLIGHT_MAX: usize = 1;

const _: () = assert!(IN_FLIGHT_MAX == 1);

pub struct Looker {
    requests: SyncSender<Asked>,
    answers: Receiver<Seen>,
}

pub struct Asked {
    pub program: u32,
    pub group: Group,
    pub front: Option<front::Snapshot>,
    pub shell_version: u64,
}

#[derive(Debug, PartialEq)]
pub struct Seen {
    pub foreground: Option<api::Foreground>,
    pub front: Option<(Group, Front)>,
    pub shell_version: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Gone;

impl Looker {
    pub fn started(
        terminal: TerminalId,
        woken: impl Fn() + Send + 'static,
    ) -> io::Result<Looker> {
        Self::started_with(terminal, woken, Asked::walk)
    }

    fn started_with(
        terminal: TerminalId,
        woken: impl Fn() + Send + 'static,
        walk: impl Fn(Asked) -> Seen + Send + 'static,
    ) -> io::Result<Looker> {
        let (requests, inbox) = mpsc::sync_channel(IN_FLIGHT_MAX);
        let (outbox, answers) = mpsc::sync_channel(IN_FLIGHT_MAX);
        thread::Builder::new().name(format!("terminal {terminal} looks")).spawn(move || {
            let looked = panics::contained(|| {
                while let Ok(asked) = inbox.recv() {
                    let seen = walk(asked);
                    match outbox.try_send(seen) {
                        Ok(()) => {},
                        Err(TrySendError::Disconnected(_)) => return,
                        Err(TrySendError::Full(_)) => {
                            unreachable!("a terminal asks one look at a time");
                        },
                    }
                    woken();
                }
                assert!(matches!(inbox.try_recv(), Err(TryRecvError::Disconnected)));
            });
            if let Err(Panicked) = looked {
                eprintln!("lune-mux: terminal {terminal}'s looks panicked");
                drop(inbox);
                drop(outbox);
                woken();
            }
        })?;
        Ok(Looker { requests, answers })
    }

    pub fn ask(&self, asked: Asked) -> Result<(), Gone> {
        assert!(asked.program > 0);
        assert!(asked.group.leader() > 0);
        if let Some(front) = asked.front {
            assert!(front.device != 0);
        }
        match self.requests.try_send(asked) {
            Ok(()) => Ok(()),
            Err(TrySendError::Disconnected(_)) => Err(Gone),
            Err(TrySendError::Full(_)) => unreachable!("a terminal asks one look at a time"),
        }
    }

    pub fn take_seen(&self) -> Result<Option<Seen>, Gone> {
        match self.answers.try_recv() {
            Ok(seen) => {
                if let Some(foreground) = &seen.foreground {
                    assert!(foreground.pid > 0);
                }
                if let Some((group, _)) = seen.front {
                    assert!(group.leader() > 0);
                }
                Ok(Some(seen))
            },
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(Gone),
        }
    }
}

impl Asked {
    fn walk(self) -> Seen {
        let Asked { program, group, front, shell_version } = self;
        assert!(program > 0);
        assert!(group.leader() > 0);
        let foreground = foreground(group.leader()).or_else(|| foreground(program));
        let front = front.map(|snapshot| {
            assert!(snapshot.device != 0);
            (group, front::of_terminal(snapshot, program, group))
        });
        if let Some(foreground) = &foreground {
            assert!(foreground.pid == group.leader() || foreground.pid == program);
        }
        Seen { foreground, front, shell_version }
    }
}

fn foreground(pid: u32) -> Option<api::Foreground> {
    assert!(pid > 0);
    let argv = process::argv(pid)?;
    assert!(argv.iter().map(|arg| arg.len() + 1).sum::<usize>() <= process::ARGV_BYTES_MAX);
    Some(api::Foreground { pid, argv })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use lune_waits::{eventually, heard};

    use super::*;

    fn asked(shell_version: u64) -> Asked {
        let pid = std::process::id();
        Asked { program: pid, group: Group(pid), front: None, shell_version }
    }

    #[test]
    fn a_look_asked_returns_what_was_seen_and_wakes_the_caller() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started(terminal, move || wake.send(()).unwrap()).unwrap();
        assert_eq!(looker.take_seen(), Ok(None));
        looker.ask(asked(7)).unwrap();
        let () = heard("the looker wakes the caller", &woken);
        let seen = looker.take_seen().unwrap().expect("the answer is ready");
        assert_eq!(seen.shell_version, 7);
        assert_eq!(seen.front, None);
        let foreground = seen.foreground.expect("this process reads its argv");
        assert_eq!(foreground.pid, std::process::id());
        assert_eq!(looker.take_seen(), Ok(None));
    }

    #[test]
    fn a_panicking_look_disconnects_before_waking_and_reports_gone() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || wake.send(()).unwrap(),
            |_| panic!("the walk panicked"),
        )
        .unwrap();
        looker.ask(asked(1)).unwrap();
        let () = heard("the panicked looker wakes the caller", &woken);
        assert_eq!(looker.take_seen(), Err(Gone));
        assert_eq!(looker.ask(asked(2)), Err(Gone));
    }

    #[test]
    #[should_panic(expected = "a terminal asks one look at a time")]
    fn asking_twice_while_a_look_is_in_flight_panics() {
        let (entered, inside) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            || {},
            move |asked| {
                let _ = entered.send(());
                let _ = released.recv();
                Seen { foreground: None, front: None, shell_version: asked.shell_version }
            },
        )
        .unwrap();
        looker.ask(asked(1)).unwrap();
        let () = heard("the first look is running", &inside);
        looker.ask(asked(2)).unwrap();
        let third = looker.ask(asked(3));
        let _ = release.send(());
        let _ = third;
    }

    #[test]
    fn answering_into_an_uncollected_outbox_fails_fast_as_gone() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || wake.send(()).unwrap(),
            |asked| Seen { foreground: None, front: None, shell_version: asked.shell_version },
        )
        .unwrap();
        looker.ask(asked(1)).unwrap();
        let () = heard("the first look answered", &woken);
        looker.ask(asked(2)).unwrap();
        let () = heard("the second look failed fast on a full outbox", &woken);
        let first = looker.take_seen().unwrap().expect("the first answer is still queued");
        assert_eq!(first.shell_version, 1);
        assert_eq!(looker.take_seen(), Err(Gone));
    }

    #[test]
    fn dropping_the_looker_ends_its_thread() {
        struct OnDrop(SyncSender<()>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                let _ = self.0.send(());
            }
        }
        let (ended, done) = mpsc::sync_channel(1);
        let guard = OnDrop(ended);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || {
                let _ = &guard;
            },
            |asked| Seen { foreground: None, front: None, shell_version: asked.shell_version },
        )
        .unwrap();
        drop(looker);
        eventually("the looker thread exits on drop", || done.try_recv().ok());
    }
}
