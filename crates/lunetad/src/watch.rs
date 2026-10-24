//! A folder watched for whoever reads it, to read it again as it changes.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// How long a folder is let settle after a change before it is read: a file
/// written in a few pieces makes a word for each, and one read covers them all.
const SETTLE: Duration = Duration::from_millis(25);

/// Reads with `read`, and hands what it read to `tell`: once before this
/// returns, so that what the daemon knows is whole before it answers anyone,
/// and then from a thread of its own whenever something in `dir` changes, and
/// at least every `sweep` besides, for what changes without a word to the
/// folder, as a process that dies. The thread ends once `tell` says that no
/// one listens.
///
/// A folder that is not there yet is read on the sweep alone, and watched once
/// it turns up.
pub fn watch<T: 'static>(
    name: &str,
    dir: PathBuf,
    mode: RecursiveMode,
    sweep: Duration,
    mut read: impl FnMut() -> T + Send + 'static,
    tell: impl Fn(T) -> bool + Send + 'static,
) -> io::Result<()> {
    if !tell(read()) {
        return Ok(());
    }
    thread::Builder::new().name(name.to_owned()).spawn(move || {
        let (changed, heard) = mpsc::channel();
        let mut watcher = None;
        loop {
            if watcher.is_none() {
                watcher = watching(&dir, mode, changed.clone());
            }
            match heard.recv_timeout(sweep) {
                Ok(()) => settle(&heard),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {},
            }
            if !tell(read()) {
                return;
            }
        }
    })?;
    Ok(())
}

/// A watcher that says so on `changed` when anything in `dir` does, but for a
/// read: the reads that follow a change would make changes of their own.
fn watching(dir: &Path, mode: RecursiveMode, changed: Sender<()>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event.is_ok_and(|event| !event.kind.is_access()) {
            let _ = changed.send(());
        }
    })
    .ok()?;
    watcher.watch(dir, mode).ok()?;
    Some(watcher)
}

fn settle(heard: &Receiver<()>) {
    while heard.recv_timeout(SETTLE).is_ok() {}
}
