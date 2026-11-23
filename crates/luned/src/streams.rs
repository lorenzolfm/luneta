use std::{io, thread};

use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt as _};
use tonic::Status;

pub struct Outbox<T> {
    said: mpsc::Sender<T>,
    giving_up: mpsc::Sender<String>,
    untaken: fn() -> String,
}

impl<T> Outbox<T> {
    pub fn say(&self, saying: T) {
        match self.said.try_send(saying) {
            Ok(()) | Err(TrySendError::Closed(_)) => {},
            Err(TrySendError::Full(_)) => self.give_up((self.untaken)()),
        }
    }

    pub fn give_up(&self, why: String) {
        match self.giving_up.try_send(why) {
            Ok(()) => {},
            Err(TrySendError::Full(_giving_up_already)) => {},
            Err(TrySendError::Closed(_ended_already)) => {},
        }
    }
}

pub struct Inbox<T> {
    said: mpsc::Receiver<T>,
    given_up: mpsc::Receiver<String>,
}

pub enum Next<Heard, Said> {
    Heard(Result<Heard, Status>),
    LetGo,
    Said(Said),
    GaveUp(String),
}

impl<T> Inbox<T> {
    pub fn merged<Heard>(
        self,
        hearing: impl Stream<Item = Result<Heard, Status>>,
    ) -> impl Stream<Item = Next<Heard, T>> {
        let hearing = hearing.map(Next::Heard).chain(tokio_stream::once(Next::LetGo));
        let said = ReceiverStream::new(self.said).map(Next::Said);
        let given_up = ReceiverStream::new(self.given_up).map(Next::GaveUp);
        hearing.merge(said).merge(given_up)
    }
}

pub fn boxes<T>(said_max: usize, untaken: fn() -> String) -> (Outbox<T>, Inbox<T>) {
    assert!(said_max > 0, "a stream takes at least one word at a time");
    let (said, saying) = mpsc::channel(said_max);
    let (giving_up, given_up) = mpsc::channel(1);
    (Outbox { said, giving_up, untaken }, Inbox { said: saying, given_up })
}

pub fn on_thread<Told: Send + 'static>(
    name: &str,
    told: Told,
    run: impl FnOnce(Told) + Send + 'static,
) -> Result<(), (Told, io::Error)> {
    let (handing, handed) = std::sync::mpsc::sync_channel::<Told>(1);
    let running = move || run(handed.recv().expect("`on_thread` hands its thread `told` at once"));
    match thread::Builder::new().name(name.to_owned()).spawn(running) {
        Ok(_) => {
            handing.send(told).expect("the thread waits on `told` before all else");
            Ok(())
        },
        Err(error) => Err((told, error)),
    }
}
