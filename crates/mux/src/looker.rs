use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread;

use lune_protocol::shell::Front;

use crate::api;
use crate::front;
use crate::id::TerminalId;
use crate::panics::{self, Panicked};
use crate::process::{self, Group};
use crate::pty::Modes;

pub struct Asked {
    pub program: u32,
    pub group: Option<Group>,
    pub going: bool,
    pub modes: Option<Modes>,
    pub device: Option<u64>,
    pub marked: u64,
}

pub struct Seen {
    pub foreground: Option<api::Foreground>,
    pub front: Option<(Group, Front)>,
    pub marked: u64,
}

pub struct Gone;

pub struct Looker {
    asks: SyncSender<Asked>,
    seen: Receiver<Seen>,
}

impl Looker {
    pub fn started(
        terminal: TerminalId,
        woken: impl Fn() + Send + 'static,
    ) -> io::Result<Looker> {
        let (asks, asking) = mpsc::sync_channel::<Asked>(1);
        let (told, seen) = mpsc::sync_channel(1);
        thread::Builder::new().name(format!("terminal {terminal} looks")).spawn(move || {
            let looked = panics::contained(|| {
                while let Ok(asked) = asking.recv() {
                    if told.send(asked.seen()).is_err() {
                        return;
                    }
                    woken();
                }
            });
            if let Err(Panicked) = looked {
                eprintln!("lune-mux: terminal {terminal}'s looks panicked");
                drop(told);
                woken();
            }
        })?;
        Ok(Looker { asks, seen })
    }

    pub fn ask(&self, asked: Asked) -> Result<(), Gone> {
        let sent = self.asks.try_send(asked);
        assert!(!matches!(sent, Err(TrySendError::Full(_))), "a terminal asks one look at a time");
        sent.map_err(|_| Gone)
    }

    pub fn seen(&self) -> Result<Option<Seen>, Gone> {
        match self.seen.try_recv() {
            Ok(seen) => Ok(Some(seen)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(Gone),
        }
    }
}

impl Asked {
    fn seen(self) -> Seen {
        let Asked { program, group, going, modes, device, marked } = self;
        let foreground = group.and_then(|group| {
            foreground(group.leader()).or_else(|| foreground(program))
        });
        let front = group.filter(|_| going).map(|group| {
            (group, front::of_terminal(modes, device, program, group))
        });
        Seen { foreground, front, marked }
    }
}

fn foreground(pid: u32) -> Option<api::Foreground> {
    Some(api::Foreground { pid, argv: process::argv(pid)? })
}
