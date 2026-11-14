use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

pub use lunetad::state::{path, Unset};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::host::Event;

pub fn unfinished() -> String {
    format!("{:016x}.new", RandomState::new().hash_one(()))
}

pub fn watch(
    dirs: &[&Path],
    events: Sender<Event>,
    event: fn() -> Event,
) -> notify::Result<RecommendedWatcher> {
    watching(dirs, |_| true, events, event)
}

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
        let _ = events.send(event());
    })?;
    for dir in dirs {
        watcher.watch(dir, RecursiveMode::NonRecursive)?;
    }
    Ok(watcher)
}
