use tokio::sync::mpsc;

use crate::mux::own::server::Terminals;
use crate::mux::own::TerminalKey;
use crate::projects::{Line, Size, TerminalViewId, VIEWS_MAX};
use crate::tasks;

pub const ERRANDS_MAX: usize = VIEWS_MAX + 1;

const _: () = assert!(ERRANDS_MAX > VIEWS_MAX, "one settle's sizes and a paste wait at once");

pub const UNTAKEN: &str = "lune-mux has yet to take what was asked of it before";

enum Errand {
    Size { key: TerminalKey, size: Size },
    Paste(Vec<(TerminalViewId, TerminalKey, Line)>),
}

pub enum Unrun {
    Sized { key: TerminalKey, size: Size, why: String },
    Pasted { terminals: Vec<TerminalViewId>, why: String },
}

pub struct Errands {
    sent: mpsc::Sender<Errand>,
    unrun: mpsc::Receiver<Unrun>,
}

impl Errands {
    pub fn start() -> Result<Errands, String> {
        let tasks = tasks::shared()?;
        let (sent, errands) = mpsc::channel(ERRANDS_MAX);
        let (failed, unrun) = mpsc::channel(ERRANDS_MAX);
        drop(tasks.spawn(run(errands, failed)));
        Ok(Errands { sent, unrun })
    }

    pub fn size(&self, key: TerminalKey, size: Size) -> bool {
        self.sent.try_send(Errand::Size { key, size }).is_ok()
    }

    pub fn paste(&self, pastes: Vec<(TerminalViewId, TerminalKey, Line)>) -> bool {
        assert!(!pastes.is_empty(), "a paste is asked for");
        assert!(pastes.len() <= VIEWS_MAX, "no more pastes than terminals");
        self.sent.try_send(Errand::Paste(pastes)).is_ok()
    }

    pub fn unrun(&mut self) -> Option<Unrun> {
        self.unrun.try_recv().ok()
    }
}

async fn run(mut errands: mpsc::Receiver<Errand>, failed: mpsc::Sender<Unrun>) {
    while let Some(errand) = errands.recv().await {
        let left = match errand {
            Errand::Size { key, size } => {
                let sized = Terminals.sized(key, size).await;
                sized.err().map(|failure| Unrun::Sized { key, size, why: failure.to_string() })
            },
            Errand::Paste(pastes) => {
                let typed: Vec<(TerminalKey, &Line)> =
                    pastes.iter().map(|(_, key, line)| (*key, line)).collect();
                let untyped = Terminals.typed(&typed).await.err();
                untyped.map(|failure| Unrun::Pasted {
                    terminals: pastes.iter().map(|(terminal, ..)| *terminal).collect(),
                    why: failure.to_string(),
                })
            },
        };
        if let Some(left) = left
            && failed.send(left).await.is_err()
        {
            return;
        }
    }
}
