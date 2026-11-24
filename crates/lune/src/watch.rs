use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use luned::api::{Version, WatchRequest};
use tokio::time::Instant;

use crate::commands::{self, causes};
use crate::daemon::{self, Current};
use crate::host::Event;

const AGAIN: Duration = Duration::from_secs(1);

const MAKING_WAY: Duration = Duration::from_millis(50);

const _: () = assert!(daemon::LOOK.as_millis() < MAKING_WAY.as_millis());
const _: () = assert!(MAKING_WAY.as_millis() < AGAIN.as_millis());

pub fn start(folder: PathBuf, events: Sender<Event>) {
    thread::spawn(move || {
        let tasks = match commands::tasks() {
            Ok(tasks) => tasks,
            Err(why) => {
                let _ = events.send(Event::Daemon(Err(why)));
                return;
            },
        };
        tasks.block_on(watch(folder, events));
    });
}

async fn watch(folder: PathBuf, events: Sender<Event>) {
    loop {
        let reaching = Instant::now();
        let mut after: Option<Version> = None;
        let failure = match daemon::current(&folder).await {
            Ok(Current { mut daemon, info, .. }) => {
                let claude_folder = info
                    .claude_folder
                    .map(|folder| PathBuf::from(OsString::from_vec(folder)))
                    .or_else(luned::claude_folder);
                if events.send(Event::Reached { claude_folder }).is_err() {
                    return;
                }
                loop {
                    match daemon.watch(WatchRequest { after, kept: None, shown: true }).await {
                        Ok(host) => {
                            let host = host.into_inner();
                            let Some(version) = host.version else {
                                break "an answer with no version, which this lune does not read"
                                    .to_owned();
                            };
                            after = Some(version);
                            if events.send(Event::Daemon(Ok(host))).is_err() {
                                return;
                            }
                        },
                        Err(status) => break causes(&status),
                    }
                }
            },
            Err(why) => why,
        };
        if events.send(Event::Daemon(Err(failure))).is_err() {
            return;
        }
        tokio::time::sleep_until((reaching + AGAIN).max(Instant::now() + MAKING_WAY)).await;
    }
}
