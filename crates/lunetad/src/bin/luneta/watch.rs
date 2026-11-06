//! The host as the daemon says it stands, each time it changes, for the
//! sidebar: a thread of its own that waits on the daemon's `Watch`, and hands
//! each answer to luneta's loop, after the folder each daemon it reaches reads
//! Claude Code's sessions in.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use lunetad::api::{Version, WatchRequest};
use tokio::time::Instant;

use crate::commands::causes;
use crate::daemon::{self, Current};
use crate::host::Event;

/// How often luneta reaches for a daemon at the most, from when it last began
/// to: a lunetad that does not start, or one that fails as it answers, is
/// started and asked no more often than this. One that answered for longer has
/// waited this out, and leaves luneta only `MAKING_WAY` to wait.
const AGAIN: Duration = Duration::from_secs(1);

/// How long luneta leaves the folder to whoever stopped the daemon it lost,
/// before it reaches for one again. A newer luneta stops an older daemon and
/// looks again a `daemon::LOOK` later: reached for at once, a sidebar of the
/// upgrade before it would start another older one first, for that luneta to
/// stop in turn. Longer than that look and the moments its lunetad takes to
/// hold the folder.
const MAKING_WAY: Duration = Duration::from_millis(50);

// The look it waits out has to fit in it, and it has to leave `AGAIN` something
// to limit. These hold within one build alone: that look is the newer luneta's.
const _: () = assert!(daemon::LOOK.as_millis() < MAKING_WAY.as_millis());
const _: () = assert!(MAKING_WAY.as_millis() < AGAIN.as_millis());

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
/// looked for again, as `AGAIN` and `MAKING_WAY` let it, and started where none
/// runs, or in the place of an older one. Each daemon found is asked first where
/// it reads Claude Code's sessions, which are those of the agents it lists.
async fn watch(folder: PathBuf, events: Sender<Event>) {
    loop {
        let reaching = Instant::now();
        // A daemon found afresh is asked for the whole host, as one started
        // again counts its versions afresh.
        let mut after: Option<Version> = None;
        let failure = match daemon::current(&folder).await {
            Ok(Current { mut daemon, info, .. }) => {
                // A daemon that says no folder, as one older than the field, is
                // taken to read luneta's, as before: one whose environment named
                // none lists no agents to look for.
                let claude_folder = info
                    .claude_folder
                    .map(|folder| PathBuf::from(OsString::from_vec(folder)))
                    .or_else(lunetad::claude_folder);
                // luneta is on its way out.
                if events.send(Event::Reached { claude_folder }).is_err() {
                    return;
                }
                loop {
                    match daemon.watch(WatchRequest { after, kept: None }).await {
                        Ok(host) => {
                            let host = host.into_inner();
                            // One with no version would be asked after again at
                            // once, as a first look, for as long as it answers so.
                            let Some(version) = host.version else {
                                break "an answer with no version, which this luneta does not read"
                                    .to_owned();
                            };
                            if version.pid != info.pid {
                                break format!(
                                    "lunetad (pid {}) answered in the place of the one luneta \
                                     reached (pid {})",
                                    version.pid, info.pid
                                );
                            }
                            after = Some(version);
                            // luneta is on its way out.
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
