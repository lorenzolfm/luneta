//! A folder watched for whoever reads it, to read it again as it changes.

use std::collections::HashSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::mem;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};

/// How long a folder is let settle after a change before it is read: a file
/// written in a few pieces makes a word for each, and one read covers them all.
const SETTLE: Duration = Duration::from_millis(25);

/// The longest a folder is let settle: one that never stops changing is read
/// anyway, as often as this.
const SETTLE_MAX: Duration = Duration::from_millis(250);

const _: () = assert!(SETTLE.as_millis() < SETTLE_MAX.as_millis());

/// Reads with `read`, and hands what it read to `tell`: once before this
/// returns, so that what the daemon knows is whole before it answers anyone,
/// and then from a thread of its own whenever something in `dir` changes at
/// a path that `wanted` takes, and at least every `sweep` besides, for what
/// changes without a word to the folder, as a process that dies. The thread
/// ends once `tell` says that no one listens.
///
/// A folder that is not there yet is read on the sweep alone, and watched once
/// it turns up.
pub fn watch<T: 'static>(
    name: &str,
    dir: PathBuf,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    sweep: Duration,
    mut read: impl FnMut() -> T + Send + 'static,
    mut tell: impl FnMut(T) -> bool + Send + 'static,
) -> io::Result<()> {
    if !tell(read()) {
        return Ok(());
    }
    thread::Builder::new().name(name.to_owned()).spawn(move || {
        // One wake-up is all a read needs, however many changes made it, so
        // the channel holds one at most.
        let (changed, heard) = mpsc::sync_channel(1);
        let mut watcher = None;
        let mut troubles = Troubles::default();
        loop {
            if watcher.is_none() {
                watcher = watching(&dir, mode, wanted, &changed, &mut troubles);
                troubles.done();
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

/// A watcher of `dir`, or none, which leaves the folder to the sweep. One
/// that cannot watch a folder that is there says why.
fn watching(
    dir: &Path,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    changed: &SyncSender<()>,
    troubles: &mut Troubles,
) -> Option<RecommendedWatcher> {
    match watcher(dir, mode, wanted, changed.clone()) {
        Ok(watcher) => Some(watcher),
        Err(error) if missing(&error) => None,
        Err(error) => {
            troubles.say(format!("{} is read on the sweep alone: {error}", dir.display()));
            None
        },
    }
}

/// A watcher that says so on `changed` when anything in `dir` changes that a
/// read could find. A watcher that goes wrong is a change too, as a read is
/// how the daemon finds out what it missed.
fn watcher(
    dir: &Path,
    mode: RecursiveMode,
    wanted: fn(&Path) -> bool,
    changed: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if event.is_ok_and(|event| !worth_a_read(&event, wanted)) {
            return;
        }
        match changed.try_send(()) {
            // A full channel holds a wake-up already, and one that no one
            // hears is the thread's that has ended.
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
        }
    })?;
    watcher.watch(dir, mode)?;
    Ok(watcher)
}

/// Whether `event` can change what a read finds. A read's own access to the
/// folder cannot, or the reads that follow a change would make changes of
/// their own; nor can a change at paths that `wanted` does not take. One that
/// names no path could be of anything.
fn worth_a_read(event: &Event, wanted: fn(&Path) -> bool) -> bool {
    if event.kind.is_access() {
        return false;
    }
    event.paths.is_empty() || event.paths.iter().any(|path| wanted(path))
}

/// Whether `error` is the folder's not being there yet.
fn missing(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::PathNotFound => true,
        notify::ErrorKind::Io(error) => error.kind() == ErrorKind::NotFound,
        _ => false,
    }
}

/// Waits out a burst of changes, until `SETTLE` has passed without one, but no
/// longer than `SETTLE_MAX` in all.
fn settle(heard: &Receiver<()>) {
    let deadline = Instant::now() + SETTLE_MAX;
    while Instant::now() < deadline {
        match heard.recv_timeout(SETTLE) {
            Ok(()) => {},
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// The paths in `dir`. A folder that is not there yet holds none.
pub fn listed(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    entries.map(|entry| Ok(entry?.path())).collect()
}

/// What went wrong in reading a folder, each said once while it keeps going
/// wrong: a folder is read again every sweep.
#[derive(Default)]
pub struct Troubles {
    /// What went wrong in the read before, and was said then.
    said: HashSet<String>,
    /// What has gone wrong in this read so far.
    now: HashSet<String>,
}

impl Troubles {
    /// Says that `what` went wrong, unless it went wrong in the read before,
    /// or has been said in this one.
    pub fn say(&mut self, what: String) {
        if !self.said.contains(&what) && !self.now.contains(&what) {
            eprintln!("lunetad: {what}");
        }
        self.now.insert(what);
    }

    /// Ends a read. What went wrong in it is said again once it has gone right
    /// in between.
    pub fn done(&mut self) {
        self.said = mem::take(&mut self.now);
    }
}
