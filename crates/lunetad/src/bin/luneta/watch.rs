//! The host as the daemon says it stands, each time it changes, for the
//! sidebar: a thread of its own that waits on the daemon's `Watch`, and hands
//! each answer to luneta's loop.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use lunetad::api::{Version, WatchRequest};

use crate::commands::causes;
use crate::host::Event;

/// How long luneta waits to ask again a daemon that did not answer.
const AGAIN: Duration = Duration::from_secs(1);

/// Watches the daemon on `socket`, or says why it cannot, until luneta ends.
pub fn start(socket: PathBuf, events: Sender<Event>) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(error) => {
                // luneta is on its way out.
                let _ = events.send(Event::Daemon(Err(format!("no runtime: {error}"))));
                return;
            },
        };
        runtime.block_on(watch(socket, events));
    });
}

/// Asks again each time the daemon answers, for the host once it is no longer
/// as last seen, until luneta's loop is gone. A daemon that goes away is
/// looked for again, each `AGAIN`.
async fn watch(socket: PathBuf, events: Sender<Event>) {
    loop {
        // A daemon found afresh is asked for the whole host, as one started
        // again counts its versions afresh.
        let mut after: Option<Version> = None;
        let failure = match lunetad::connect(&socket).await {
            Ok(mut daemon) => loop {
                match daemon.watch(WatchRequest { after }).await {
                    Ok(host) => {
                        let host = host.into_inner();
                        after = host.version;
                        // luneta is on its way out.
                        if events.send(Event::Daemon(Ok(host))).is_err() {
                            return;
                        }
                    },
                    Err(status) => break causes(&status),
                }
            },
            Err(error) => format!("{}: {}", socket.display(), causes(&error)),
        };
        if events.send(Event::Daemon(Err(failure))).is_err() {
            return;
        }
        tokio::time::sleep(AGAIN).await;
    }
}
