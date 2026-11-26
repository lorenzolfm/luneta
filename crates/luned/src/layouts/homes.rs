use std::collections::BTreeSet;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

use lune_domain::projects::{Dir, TerminalViewId, VIEWS_MAX};

use super::Command;
use crate::projects::spot;

pub const LOOKS_MAX: usize = 4;

const _: () = assert!(LOOKS_MAX > 0);
const _: () = assert!(LOOKS_MAX <= VIEWS_MAX);

struct Look {
    terminal: TerminalViewId,
    folder: Dir,
}

pub struct Homes {
    sent: SyncSender<Look>,
    looking: BTreeSet<TerminalViewId>,
}

impl Homes {
    pub fn start(layouts: SyncSender<Command>, look: Duration) -> io::Result<Homes> {
        let (sent, looks) = mpsc::sync_channel(LOOKS_MAX);
        let thread = thread::Builder::new().name("homes".to_owned());
        thread.spawn(move || run(&looks, &layouts, look))?;
        Ok(Homes { sent, looking: BTreeSet::new() })
    }

    pub fn looking(&self, terminal: TerminalViewId) -> bool {
        self.looking.contains(&terminal)
    }

    pub fn look(&mut self, terminal: TerminalViewId, folder: Dir) -> bool {
        assert!(!self.looking(terminal), "terminal {terminal} is looked for once at a time");
        if self.looking.len() >= LOOKS_MAX {
            return false;
        }
        let sent = self.sent.try_send(Look { terminal, folder });
        assert!(sent.is_ok(), "the homes take every look there is room for");
        self.looking.insert(terminal);
        assert!(self.looking.len() <= LOOKS_MAX, "no more looks than the homes take");
        true
    }

    pub fn looked(&mut self, terminal: TerminalViewId) {
        let was = self.looking.remove(&terminal);
        assert!(was, "terminal {terminal} is spotted only once it is looked for");
    }
}

fn run(looks: &Receiver<Look>, layouts: &SyncSender<Command>, look: Duration) {
    while let Ok(Look { terminal, folder }) = looks.recv() {
        let spot = spot::of(&folder, look);
        if layouts.send(Command::Spotted { terminal, folder, spot }).is_err() {
            return;
        }
    }
}
