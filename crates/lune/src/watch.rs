use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use luned::LocalWatch;
use tokio::time::Instant;

use crate::commands::causes;
use crate::daemon::{self, Current};
use crate::host::Event;

const AGAIN: Duration = Duration::from_secs(1);

const MAKING_WAY: Duration = Duration::from_millis(50);

const _: () = assert!(MAKING_WAY.as_millis() < AGAIN.as_millis());

pub fn start(folder: PathBuf, events: Sender<Event>) {
    thread::spawn(move || {
        luned::tasks::shared().block_on(watch(folder, events));
    });
}

async fn watch(folder: PathBuf, events: Sender<Event>) {
    loop {
        let reaching = Instant::now();
        let failure = match daemon::current(&folder).await {
            Ok(Current { daemon, .. }) => match daemon.watching(luned::ANSWER, LocalWatch::Window) {
                Ok(mut watching) => loop {
                    match watching.next().await {
                        Ok(watched) => {
                            if events.send(Event::Daemon(Ok(watched.host))).is_err() {
                                return;
                            }
                        },
                        Err(error) => break causes(&error),
                    }
                },
                Err(unsent) => unsent.to_string(),
            },
            Err(why) => why,
        };
        if events.send(Event::Daemon(Err(failure))).is_err() {
            return;
        }
        tokio::time::sleep_until((reaching + AGAIN).max(Instant::now() + MAKING_WAY)).await;
    }
}
