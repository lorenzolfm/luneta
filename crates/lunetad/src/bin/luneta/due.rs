//! What a box luneta waits on is owed: the answer of the thread, or the
//! action, it waits on, once, however that ends. A box that waits has a
//! spinner turning, which says that what it waits on is still at work: one
//! that ended with no answer, as a thread that panics does, or as a value
//! dropped on a path that answers nothing, would have it turn for good.

use std::sync::mpsc::Sender;

use crate::host::Event;

/// Why a box has no answer: what it waited on ended with none.
pub const UNANSWERED: &str = "it ended with no answer";

/// The answer owed to a box that waits, as the event it goes in: sent once,
/// as it is paid, or else as it is dropped unpaid, with `UNANSWERED`, as a
/// thread that panics drops it, so that no box waits for good. One owed to a
/// box that was closed goes to none, as any answer to it does.
pub struct Due<T> {
    events: Sender<Event>,
    /// The event the answer goes in, until it is sent.
    event: Option<Box<Answers<T>>>,
}

/// What makes the event an answer goes in.
type Answers<T> = dyn FnOnce(Result<T, String>) -> Event + Send;

impl<T> Due<T> {
    /// The answer owed on `events`, in the event `event` makes of it.
    pub fn new(
        events: &Sender<Event>,
        event: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
    ) -> Self {
        Due { events: events.clone(), event: Some(Box::new(event)) }
    }

    /// Sends `answer`, as it is owed.
    pub fn pay(mut self, answer: Result<T, String>) {
        self.send(answer);
    }

    fn send(&mut self, answer: Result<T, String>) {
        if let Some(event) = self.event.take() {
            // luneta is on its way out.
            let _ = self.events.send(event(answer));
        }
    }
}

impl<T> Drop for Due<T> {
    /// What it was owed for ended with no answer.
    fn drop(&mut self) {
        self.send(Err(UNANSWERED.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    /// The box hears what it is paid, once; and where the thread it waits on
    /// ends with no answer, panicking, that it ended with none.
    #[test]
    fn a_box_hears_its_answer_once_or_that_there_was_none() {
        let (events, heard) = mpsc::channel();
        let opened = |menu| move |opened| Event::Opened { menu, opened };
        Due::new(&events, opened(7)).pay(Ok(4));
        let due = Due::<u32>::new(&events, opened(8));
        let panicked = thread::spawn(move || {
            let _due = due;
            panic!("a thread that panics");
        });
        assert!(panicked.join().is_err());
        let heard: Vec<Event> = heard.try_iter().collect();
        assert_eq!(heard.len(), 2);
        assert!(matches!(heard[0], Event::Opened { menu: 7, opened: Ok(4) }));
        let Event::Opened { menu: 8, opened: Err(why) } = &heard[1] else {
            panic!("the box waiting on the thread that panicked hears");
        };
        assert_eq!(why, UNANSWERED);
    }
}
