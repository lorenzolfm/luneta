use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tonic::Status;

pub struct Outbox<T> {
    said: mpsc::Sender<T>,
    untaken: mpsc::Sender<()>,
}

impl<T> Outbox<T> {
    pub fn say(&self, saying: T) {
        match self.said.try_send(saying) {
            Ok(()) | Err(TrySendError::Closed(_)) => {}
            Err(TrySendError::Full(_)) => self.untaken(),
        }
    }

    fn untaken(&self) {
        match self.untaken.try_send(()) {
            Ok(()) => {}
            Err(TrySendError::Full(_untaken_already)) => {}
            Err(TrySendError::Closed(_ended_already)) => {}
        }
    }
}

pub struct Inbox<T> {
    said: mpsc::Receiver<T>,
    untaken: mpsc::Receiver<()>,
}

pub enum Next<Heard, Said> {
    Heard(Result<Heard, Status>),
    LetGo,
    Said(Said),
    Untaken,
    Dropped,
}

pub struct Dropped;

impl<T> Inbox<T> {
    pub fn merged<Heard>(
        self,
        hearing: impl Stream<Item = Result<Heard, Status>>,
    ) -> impl Stream<Item = Next<Heard, T>> {
        let hearing = hearing
            .map(Next::Heard)
            .chain(tokio_stream::once(Next::LetGo));
        let dropped = tokio_stream::once(Next::Dropped);
        let said = ReceiverStream::new(self.said)
            .map(Next::Said)
            .chain(dropped);
        let untaken = ReceiverStream::new(self.untaken).map(|()| Next::Untaken);
        hearing.merge(said).merge(untaken)
    }
}

pub fn boxes<T>(said_max: usize) -> (Outbox<T>, Inbox<T>) {
    assert!(said_max > 0, "a stream takes at least one word at a time");
    let (said, saying) = mpsc::channel(said_max);
    let (untaken, untaken_heard) = mpsc::channel(1);
    (
        Outbox { said, untaken },
        Inbox {
            said: saying,
            untaken: untaken_heard,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::future::Future as _;
    use std::pin::pin;
    use std::task::{Context, Waker};

    use tokio::runtime;
    use tokio_stream::StreamExt as _;
    use tonic::Status;

    use super::{Next, boxes};

    #[test]
    fn a_dropped_outbox_ends_a_stream_that_still_hears() {
        let runtime = runtime::Builder::new_current_thread().build().unwrap();
        let (outbox, inbox) = boxes::<u32>(1);
        let hearing = tokio_stream::pending::<Result<(), Status>>();
        let mut next = pin!(inbox.merged(hearing));
        let held = pin!(next.next()).poll(&mut Context::from_waker(Waker::noop()));
        assert!(held.is_pending(), "a stream whose outbox is held goes on");
        outbox.say(7);
        drop(outbox);
        let Some(Next::Said(said)) = runtime.block_on(next.next()) else {
            panic!("what was said before the drop comes first");
        };
        assert_eq!(said, 7);
        let Some(Next::Dropped) = runtime.block_on(next.next()) else {
            panic!("a dropped outbox ends the stream");
        };
    }
}
