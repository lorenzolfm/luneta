use std::collections::BTreeMap;

use tokio::sync::mpsc::{self, error::TrySendError};

use crate::mux::own::server::Terminals;
use crate::mux::own::TerminalKey;
use crate::projects::{Line, Size, TerminalViewId, VIEWS_MAX};
use crate::tasks;

pub const PASTES_PER_WAKE_MAX: usize = 1;

pub const ERRANDS_MAX: usize = VIEWS_MAX + PASTES_PER_WAKE_MAX;

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
    pub fn start() -> Errands {
        let (sent, errands) = mpsc::channel(ERRANDS_MAX);
        let (failed, unrun) = mpsc::channel(ERRANDS_MAX);
        drop(tasks::shared().spawn(run(errands, failed)));
        Errands { sent, unrun }
    }

    pub fn size(&self, key: TerminalKey, size: Size) -> bool {
        taken(&self.sent.try_send(Errand::Size { key, size }))
    }

    pub fn paste(&self, pastes: Vec<(TerminalViewId, TerminalKey, Line)>) -> bool {
        assert!(!pastes.is_empty(), "a paste is asked for");
        assert!(pastes.len() <= VIEWS_MAX, "no more pastes than terminals");
        taken(&self.sent.try_send(Errand::Paste(pastes)))
    }

    pub fn unrun(&mut self) -> Option<Unrun> {
        self.unrun.try_recv().ok()
    }
}

fn taken(sent: &Result<(), TrySendError<Errand>>) -> bool {
    let closed = matches!(sent, Err(TrySendError::Closed(_)));
    assert!(!closed, "the errands run as long as layouts hand them over");
    sent.is_ok()
}

async fn run(mut errands: mpsc::Receiver<Errand>, failed: mpsc::Sender<Unrun>) {
    let mut batch = Vec::with_capacity(ERRANDS_MAX);
    while errands.recv_many(&mut batch, ERRANDS_MAX).await > 0 {
        assert!(batch.len() <= ERRANDS_MAX, "a batch is at most the errands that wait");
        latest_kept(&mut batch);
        for errand in batch.drain(..) {
            if let Some(left) = ran(errand).await
                && failed.send(left).await.is_err()
            {
                return;
            }
        }
        assert!(batch.is_empty(), "a batch is run whole before the next");
    }
}

async fn ran(errand: Errand) -> Option<Unrun> {
    match errand {
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
    }
}

fn latest_kept(batch: &mut Vec<Errand>) {
    let mut last: BTreeMap<TerminalKey, usize> = BTreeMap::new();
    for (index, errand) in batch.iter().enumerate() {
        if let Errand::Size { key, .. } = errand {
            last.insert(*key, index);
        }
    }
    let pastes = batch.iter().filter(|it| matches!(it, Errand::Paste(_))).count();
    let mut index = 0;
    batch.retain(|errand| {
        let kept = match errand {
            Errand::Size { key, .. } => last[key] == index,
            Errand::Paste(_) => true,
        };
        index += 1;
        kept
    });
    assert_eq!(batch.len(), last.len() + pastes, "one size per terminal and every paste are kept");
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::{latest_kept, Errand};
    use crate::mux::own::{OwnMade, TerminalKey};
    use crate::projects::Size;

    fn key(id: u32) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(1) }
    }

    fn sized(key: TerminalKey, columns: u32) -> Errand {
        Errand::Size { key, size: Size::of(columns, 24).unwrap() }
    }

    fn said(batch: &[Errand]) -> Vec<String> {
        let said = |errand: &Errand| match errand {
            Errand::Size { key, size } => format!("{} at {}", key.id, size.get().0),
            Errand::Paste(_) => "paste".to_owned(),
        };
        batch.iter().map(said).collect()
    }

    #[test]
    fn only_the_last_size_asked_of_a_terminal_is_run() {
        let (one, two) = (key(1), key(2));
        let mut batch = vec![
            sized(one, 80),
            Errand::Paste(Vec::new()),
            sized(two, 70),
            sized(one, 60),
            sized(one, 50),
        ];
        latest_kept(&mut batch);
        assert_eq!(said(&batch), ["paste", "2 at 70", "1 at 50"]);
    }
}
