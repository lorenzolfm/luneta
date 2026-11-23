use std::sync::mpsc::Sender;

use crate::host::Event;

pub const UNANSWERED: &str = "it ended with no answer";

pub struct Due<T> {
    events: Sender<Event>,
    event: Option<Box<Answers<T>>>,
}

type Answers<T> = dyn FnOnce(Result<T, String>) -> Event + Send;

impl<T> Due<T> {
    pub fn new(
        events: &Sender<Event>,
        event: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
    ) -> Self {
        Due { events: events.clone(), event: Some(Box::new(event)) }
    }

    pub fn pay(mut self, answer: Result<T, String>) {
        self.send(answer);
    }

    fn send(&mut self, answer: Result<T, String>) {
        if let Some(event) = self.event.take() {
            let _ = self.events.send(event(answer));
        }
    }
}

impl<T> Drop for Due<T> {
    fn drop(&mut self) {
        self.send(Err(UNANSWERED.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    #[test]
    fn a_box_hears_its_answer_once_or_that_there_was_none() {
        let (events, heard) = mpsc::channel();
        let voiced = |number| move |ended| Event::Voiced { number, ended };
        Due::new(&events, voiced(7)).pay(Ok(()));
        let due = Due::<()>::new(&events, voiced(8));
        let panicked = thread::spawn(move || {
            let _due = due;
            panic!("a thread that panics");
        });
        assert!(panicked.join().is_err());
        let heard: Vec<Event> = heard.try_iter().collect();
        assert_eq!(heard.len(), 2);
        assert!(matches!(heard[0], Event::Voiced { number: 7, ended: Ok(()) }));
        let Event::Voiced { number: 8, ended: Err(why) } = &heard[1] else {
            panic!("the box waiting on the thread that panicked hears");
        };
        assert_eq!(why, UNANSWERED);
    }
}
