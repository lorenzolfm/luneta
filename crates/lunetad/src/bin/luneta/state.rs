//! What luneta keeps for every luneta on the host, and for the next, whatever
//! runs then: in `luneta` of `$XDG_STATE_HOME`, or of `~/.local/state` where
//! that is not set, a folder for each kind of thing kept, as the folds, or a
//! file, as the sidebar's width and the order of its rows. What is kept of the
//! sessions that run is kept beside the daemon instead, as the news is. Each
//! luneta watches the folders, and the order's file, and takes up what another
//! keeps in them as it keeps it.

use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub use lunetad::state::path;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::host::Event;

/// What a file kept is called after its own name while it is written, before
/// it takes that name, so that no luneta reads one halfway written: a name no
/// other luneta's file takes, as each `RandomState` is seeded from the
/// system's randomness.
pub fn unfinished() -> String {
    format!("{:016x}.new", RandomState::new().hash_one(()))
}

/// Sends luneta's loop `event` each time something changes in one of `dirs`,
/// for as long as the watcher it returns is kept. A read's own access to a
/// folder is no change, or each read would bring on another.
pub fn watch(
    dirs: &[&Path],
    events: Sender<Event>,
    event: fn() -> Event,
) -> notify::Result<RecommendedWatcher> {
    watching(dirs, |_| true, events, event)
}

/// Sends luneta's loop `event` each time `file` changes, for as long as the
/// watcher it returns is kept. Its folder is watched, and what else changes
/// there passed over: a file written whole under another name, and then
/// given its own, is another file than the one there before. A change is
/// told by the file's name alone, as macOS tells it under the folder's real
/// path, past a symbolic link. One that names no file is not passed over:
/// the watcher lost track, as of more than the kernel could queue, and the
/// file may have changed meanwhile.
pub fn watch_file(
    file: &Path,
    events: Sender<Event>,
    event: fn() -> Event,
) -> notify::Result<RecommendedWatcher> {
    let dir = file.parent().ok_or_else(|| notify::Error::generic("a file in no folder"))?;
    let name = file.file_name().ok_or_else(|| notify::Error::generic("a file with no name"))?;
    let name = name.to_owned();
    let wanted = move |change: &notify::Event| {
        let named = |path: &PathBuf| path.file_name() == Some(name.as_os_str());
        change.paths.is_empty() || change.paths.iter().any(named)
    };
    watching(&[dir], wanted, events, event)
}

/// Sends luneta's loop `event` for each change in one of `dirs` that is
/// `wanted`, and for each error, but for a read's own access.
fn watching(
    dirs: &[&Path],
    wanted: impl Fn(&notify::Event) -> bool + Send + 'static,
    events: Sender<Event>,
    event: fn() -> Event,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |change: notify::Result<notify::Event>| {
        if change.is_ok_and(|change| change.kind.is_access() || !wanted(&change)) {
            return;
        }
        // luneta is on its way out.
        let _ = events.send(event());
    })?;
    for dir in dirs {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    Ok(watcher)
}
