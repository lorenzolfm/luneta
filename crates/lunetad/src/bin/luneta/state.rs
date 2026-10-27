//! What luneta keeps for every luneta on the host, and for the next, whatever
//! runs then: in `luneta` of `$XDG_STATE_HOME`, or of `~/.local/state` where
//! that is not set, a folder for each kind of thing kept, as the folds, or a
//! file, as the sidebar's width. What is kept of the sessions that run is kept
//! beside the daemon instead, as the news is. Each luneta watches the folders,
//! and takes up what another keeps in them as it keeps it.

use std::env;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::host::Event;

/// Where `name` is kept, by this process's variables: in `luneta` of
/// `$XDG_STATE_HOME`, or of `~/.local/state`. A variable set to nothing is not
/// set, and the spec has a relative `XDG_STATE_HOME` passed over.
pub fn path(name: &str) -> Option<PathBuf> {
    let set = |name: &str| env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let state = set("XDG_STATE_HOME")
        .filter(|state| state.is_absolute())
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))?;
    Some(state.join("luneta").join(name))
}

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
    let mut watcher = notify::recommended_watcher(move |change: notify::Result<notify::Event>| {
        if change.is_ok_and(|change| change.kind.is_access()) {
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
