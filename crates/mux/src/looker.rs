use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;

use lune_protocol::shell::Front;

use crate::api;
use crate::front;
use crate::id::TerminalId;
use crate::panics::{self, Panicked};
use crate::process::{self, Argv, Group, ProcessId};

const IN_FLIGHT_MAX: usize = 1;

const _: () = assert!(IN_FLIGHT_MAX == 1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foreground {
    pub pid: ProcessId,
    pub argv: Argv,
}

impl Foreground {
    #[must_use]
    pub fn said(&self) -> api::Foreground {
        api::Foreground { pid: self.pid.get(), argv: self.argv.as_slice().to_vec() }
    }
}

pub struct Looker {
    requests: SyncSender<Job>,
}

pub struct InFlight {
    requests: SyncSender<Job>,
    shell_version: u64,
    reply: Reply,
}

enum Reply {
    Foreground(Receiver<Option<Foreground>>),
    WithFront { group: Group, answers: Receiver<(Option<Foreground>, Front)> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Foreground {
        program: ProcessId,
        group: Group,
    },
    WithFront {
        program: ProcessId,
        group: Group,
        snapshot: front::Snapshot,
    },
}

enum Job {
    Foreground {
        program: ProcessId,
        group: Group,
        reply: SyncSender<Option<Foreground>>,
    },
    WithFront {
        program: ProcessId,
        group: Group,
        snapshot: front::Snapshot,
        reply: SyncSender<(Option<Foreground>, Front)>,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    Foreground(Option<Foreground>),
    WithFront { foreground: Option<Foreground>, group: Group, front: Front },
}

pub enum Taken {
    Pending(InFlight),
    Ready { looker: Looker, shell_version: u64, seen: Seen },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Gone;

impl Looker {
    pub fn started(
        terminal: TerminalId,
        woken: impl Fn() + Send + 'static,
    ) -> io::Result<Looker> {
        Self::started_with(terminal, woken, walk_foreground, front::of_terminal)
    }

    fn started_with(
        terminal: TerminalId,
        woken: impl Fn() + Send + 'static,
        walk_foreground: impl Fn(ProcessId, Group) -> Option<Foreground> + Send + 'static,
        walk_front: impl Fn(front::Snapshot, ProcessId, Group) -> Front + Send + 'static,
    ) -> io::Result<Looker> {
        let (requests, inbox) = mpsc::sync_channel::<Job>(IN_FLIGHT_MAX);
        thread::Builder::new().name(format!("terminal {terminal} looks")).spawn(move || {
            while let Ok(job) = inbox.recv() {
                let ran = panics::contained(|| run_job(job, &walk_foreground, &walk_front));
                match ran {
                    Ok(true) => {
                        if panics::contained(&woken).is_err() {
                            eprintln!("lune-mux: terminal {terminal}'s look wake panicked");
                            drop(inbox);
                            return;
                        }
                    },
                    Ok(false) => return,
                    Err(Panicked) => {
                        eprintln!("lune-mux: terminal {terminal}'s looks panicked");
                        drop(inbox);
                        if panics::contained(&woken).is_err() {
                            eprintln!("lune-mux: terminal {terminal}'s look wake panicked");
                        }
                        return;
                    },
                }
            }
            assert!(matches!(inbox.try_recv(), Err(TryRecvError::Disconnected)));
        })?;
        Ok(Looker { requests })
    }

    pub fn ask(self, ask: Ask, shell_version: u64) -> Result<InFlight, Gone> {
        let (job, reply) = match ask {
            Ask::Foreground { program, group } => {
                let (tx, rx) = mpsc::sync_channel(IN_FLIGHT_MAX);
                (Job::Foreground { program, group, reply: tx }, Reply::Foreground(rx))
            },
            Ask::WithFront { program, group, snapshot } => {
                let (tx, rx) = mpsc::sync_channel(IN_FLIGHT_MAX);
                (
                    Job::WithFront { program, group, snapshot, reply: tx },
                    Reply::WithFront { group, answers: rx },
                )
            },
        };
        match self.requests.try_send(job) {
            Ok(()) => Ok(InFlight { requests: self.requests, shell_version, reply }),
            Err(TrySendError::Disconnected(_)) => Err(Gone),
            Err(TrySendError::Full(_)) => unreachable!("an idle looker has no look in flight"),
        }
    }
}

impl InFlight {
    pub fn take_seen(self) -> Result<Taken, Gone> {
        let InFlight { requests, shell_version, reply } = self;
        match reply {
            Reply::Foreground(answers) => match answers.try_recv() {
                Ok(foreground) => Ok(Taken::Ready {
                    looker: Looker { requests },
                    shell_version,
                    seen: Seen::Foreground(foreground),
                }),
                Err(TryRecvError::Empty) => Ok(Taken::Pending(InFlight {
                    requests,
                    shell_version,
                    reply: Reply::Foreground(answers),
                })),
                Err(TryRecvError::Disconnected) => Err(Gone),
            },
            Reply::WithFront { group, answers } => match answers.try_recv() {
                Ok((foreground, front)) => Ok(Taken::Ready {
                    looker: Looker { requests },
                    shell_version,
                    seen: Seen::WithFront { foreground, group, front },
                }),
                Err(TryRecvError::Empty) => Ok(Taken::Pending(InFlight {
                    requests,
                    shell_version,
                    reply: Reply::WithFront { group, answers },
                })),
                Err(TryRecvError::Disconnected) => Err(Gone),
            },
        }
    }
}

fn run_job(
    job: Job,
    walk_foreground: &(impl Fn(ProcessId, Group) -> Option<Foreground> + ?Sized),
    walk_front: &(impl Fn(front::Snapshot, ProcessId, Group) -> Front + ?Sized),
) -> bool {
    match job {
        Job::Foreground { program, group, reply } => {
            let found = walk_foreground(program, group);
            if let Some(found) = &found {
                assert!(found.pid == group.leader() || found.pid == program);
            }
            send_once(&reply, found)
        },
        Job::WithFront { program, group, snapshot, reply } => {
            let found = walk_foreground(program, group);
            if let Some(found) = &found {
                assert!(found.pid == group.leader() || found.pid == program);
            }
            let front = walk_front(snapshot, program, group);
            send_once(&reply, (found, front))
        },
    }
}

fn send_once<T>(reply: &SyncSender<T>, value: T) -> bool {
    match reply.try_send(value) {
        Ok(()) => true,
        Err(TrySendError::Disconnected(_)) => false,
        Err(TrySendError::Full(_)) => unreachable!("a fresh single-use reply channel is empty"),
    }
}

fn walk_foreground(program: ProcessId, group: Group) -> Option<Foreground> {
    foreground(group.leader()).or_else(|| foreground(program))
}

fn foreground(pid: ProcessId) -> Option<Foreground> {
    Some(Foreground { pid, argv: process::argv(pid)? })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use lune_waits::heard;

    use super::*;
    use crate::asked::Size;
    use crate::pty::{Modes, Pty};

    struct OnDrop(SyncSender<bool>);

    impl Drop for OnDrop {
        fn drop(&mut self) {
            let _ = self.0.send(thread::panicking());
        }
    }

    fn self_ids() -> (ProcessId, Group) {
        let pid = ProcessId::new(std::process::id()).unwrap();
        (pid, Group::of_process(pid))
    }

    fn ask_foreground() -> Ask {
        let (program, group) = self_ids();
        Ask::Foreground { program, group }
    }

    #[test]
    fn a_look_asked_returns_what_was_seen_and_wakes_the_caller() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started(terminal, move || wake.send(()).unwrap()).unwrap();
        let waiting = looker.ask(ask_foreground(), 7).unwrap();
        let () = heard("the looker wakes the caller", &woken);
        let Taken::Ready { looker, shell_version, seen } = waiting.take_seen().unwrap() else {
            panic!("the answer is ready");
        };
        assert_eq!(shell_version, 7);
        let Seen::Foreground(Some(foreground)) = seen else {
            panic!("this process reads its argv: {seen:?}");
        };
        assert_eq!(foreground.pid, ProcessId::new(std::process::id()).unwrap());
        drop(looker);
    }

    #[test]
    fn a_with_front_look_returns_its_asked_group_and_front() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || wake.send(()).unwrap(),
            |_, _| None,
            |_, _, _| Front::Asking,
        )
        .unwrap();
        let size = Size {
            columns: NonZeroU16::new(80).unwrap(),
            rows: NonZeroU16::new(24).unwrap(),
        };
        let pty = Pty::opened(size).unwrap();
        let (program, group) = self_ids();
        let snapshot = front::Snapshot {
            modes: Modes { lines: true, echo: false },
            device: pty.device(),
        };
        let waiting = looker.ask(Ask::WithFront { program, group, snapshot }, 11).unwrap();
        let () = heard("the looker wakes the caller", &woken);
        let Taken::Ready { shell_version, seen, .. } = waiting.take_seen().unwrap() else {
            panic!("the answer is ready");
        };
        assert_eq!(shell_version, 11);
        assert_eq!(seen, Seen::WithFront { foreground: None, group, front: Front::Asking });
    }

    #[test]
    fn a_panicking_look_disconnects_before_waking_and_reports_gone() {
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || wake.send(()).unwrap(),
            |_, _| panic!("the walk panicked"),
            |_, _, _| Front::Working,
        )
        .unwrap();
        let waiting = looker.ask(ask_foreground(), 1).unwrap();
        let () = heard("the panicked looker wakes the caller", &woken);
        assert!(matches!(waiting.take_seen(), Err(Gone)));
    }

    #[test]
    fn an_in_flight_look_stays_pending_until_the_walk_answers() {
        let (entered, inside) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        let (wake, woken) = mpsc::sync_channel(4);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || wake.send(()).unwrap(),
            move |_, _| {
                let _ = entered.send(());
                let _ = released.recv();
                None
            },
            |_, _, _| Front::Working,
        )
        .unwrap();
        let waiting = looker.ask(ask_foreground(), 3).unwrap();
        let () = heard("the look is running", &inside);
        let Taken::Pending(waiting) = waiting.take_seen().unwrap() else {
            panic!("the look is still pending");
        };
        release.send(()).unwrap();
        let () = heard("the look answered", &woken);
        let Taken::Ready { shell_version, seen, .. } = waiting.take_seen().unwrap() else {
            panic!("the look is ready");
        };
        assert_eq!((shell_version, seen), (3, Seen::Foreground(None)));
    }

    #[test]
    fn dropping_the_looker_ends_its_thread() {
        let (ended, done) = mpsc::sync_channel(1);
        let guard = OnDrop(ended);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || {
                let _ = &guard;
            },
            |_, _| None,
            |_, _, _| Front::Working,
        )
        .unwrap();
        drop(looker);
        assert!(!heard("the looker thread exits on drop", &done));
    }

    #[test]
    fn a_panicking_wake_is_contained_called_once_and_ends_the_looker() {
        let (ended, done) = mpsc::sync_channel(1);
        let guard = OnDrop(ended);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || {
                let _ = &guard;
                counted.fetch_add(1, Ordering::Relaxed);
                panic!("the wake panicked");
            },
            |_, _| None,
            |_, _, _| Front::Working,
        )
        .unwrap();
        let waiting = looker.ask(ask_foreground(), 5).unwrap();
        assert!(!heard("the looker thread exits after a panicking wake", &done));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        let Taken::Ready { looker, shell_version, seen } = waiting.take_seen().unwrap() else {
            panic!("the answer was sent before the wake panicked");
        };
        assert_eq!((shell_version, seen), (5, Seen::Foreground(None)));
        assert!(matches!(looker.ask(ask_foreground(), 6), Err(Gone)));
    }

    #[test]
    fn a_panicking_walk_whose_wake_also_panics_stays_contained() {
        let (ended, done) = mpsc::sync_channel(1);
        let guard = OnDrop(ended);
        let terminal = TerminalId::next(&mut None);
        let looker = Looker::started_with(
            terminal,
            move || {
                let _ = &guard;
                panic!("the wake panicked");
            },
            |_, _| panic!("the walk panicked"),
            |_, _, _| Front::Working,
        )
        .unwrap();
        let waiting = looker.ask(ask_foreground(), 1).unwrap();
        assert!(!heard("the looker thread exits after both walk and wake panic", &done));
        assert!(matches!(waiting.take_seen(), Err(Gone)));
    }
}
