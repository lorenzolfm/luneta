//! The host as the daemon says it stands, each time it changes, for the
//! sidebar: a thread of its own that waits on the daemon's `Watch`, and hands
//! each answer to luneta's loop.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use lunetad::api::{Version, WatchRequest};

use crate::commands::causes;
use crate::daemon::{self, Reach};
use crate::host::Event;

/// How long luneta waits to ask again a daemon that did not answer.
const AGAIN: Duration = Duration::from_secs(1);

/// Watches the daemon in `folder`, or says why it cannot, until luneta ends.
pub fn start(folder: PathBuf, events: Sender<Event>) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                // luneta is on its way out.
                let _ = events.send(Event::Daemon(Err(format!("no runtime: {error}"))));
                return;
            },
        };
        runtime.block_on(watch(folder, events));
    });
}

/// Asks again each time the daemon answers, for the host once it is no longer
/// as last seen, until luneta's loop is gone. A daemon that goes away is
/// looked for again, each `AGAIN`, and started where none runs, or in the
/// place of an older one.
async fn watch(folder: PathBuf, events: Sender<Event>) {
    loop {
        // A daemon found afresh is asked for the whole host, as one started
        // again counts its versions afresh.
        let mut after: Option<Version> = None;
        let failure = match daemon::reach(&folder, Reach::Current).await {
            Ok(mut daemon) => loop {
                match daemon.watch(WatchRequest { after }).await {
                    Ok(host) => {
                        let host = host.into_inner();
                        // One with no version would be asked after again at
                        // once, as a first look, for as long as it answers so.
                        let Some(version) = host.version else {
                            break "an answer with no version, which this luneta does not read"
                                .to_owned();
                        };
                        after = Some(version);
                        // luneta is on its way out.
                        if events.send(Event::Daemon(Ok(host))).is_err() {
                            return;
                        }
                    },
                    Err(status) => break causes(&status),
                }
            },
            Err(why) => why,
        };
        if events.send(Event::Daemon(Err(failure))).is_err() {
            return;
        }
        tokio::time::sleep(AGAIN).await;
    }
}
