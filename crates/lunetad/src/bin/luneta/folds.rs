//! The sessions folded in the sidebar, the same in every luneta on the host,
//! and kept for the next.
//!
//! A folded session is a file named for it, in the folder `luneta/folded` of
//! `$XDG_STATE_HOME`, or of `~/.local/state` where that is not set: a fold is
//! a file to make or take away, so that two lunetas that fold at once never
//! write over each other, as a file of every fold would have them. A
//! `SessionName` holds no `/`, and is neither `.` nor `..`, so it names a file.
//! Each luneta watches the folder, and takes up a fold another makes as it
//! makes it.
//!
//! A fold keeps to the name, so that a session started again under it comes
//! back folded, and the sessions that share one fold together.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::RecommendedWatcher;

use crate::host::Event;
use crate::state;
use crate::zellij::SessionName;

#[derive(Default)]
pub struct Folds {
    names: BTreeSet<String>,
    sharing: Sharing,
}

/// What the folds are shared through.
#[derive(Default)]
enum Sharing {
    /// A folder, which every luneta watches.
    Folder(PathBuf),
    /// Nothing: the folds are this luneta's alone, as a test's are.
    #[default]
    Alone,
    /// Nothing, as a folder failed them for this reason: the folds are this
    /// luneta's alone from then on.
    Lost(String),
}

impl Folds {
    /// The folds every luneta on the host shares, as they stand.
    pub fn shared() -> Self {
        match state::path("folded") {
            Some(dir) => Folds::within(dir),
            None => Folds {
                sharing: Sharing::Lost(
                    "neither XDG_STATE_HOME nor HOME is set, to keep them in".to_owned(),
                ),
                ..Folds::default()
            },
        }
    }

    fn within(dir: PathBuf) -> Self {
        let sharing = match fs::create_dir_all(&dir) {
            Ok(()) => Sharing::Folder(dir),
            Err(error) => Sharing::Lost(format!("{}: {error}", dir.display())),
        };
        let mut folds = Folds { sharing, ..Folds::default() };
        folds.reload();
        folds
    }

    /// The folder they are shared through, while they are.
    pub fn dir(&self) -> Option<&Path> {
        match &self.sharing {
            Sharing::Folder(dir) => Some(dir),
            Sharing::Alone | Sharing::Lost(_) => None,
        }
    }

    /// Why the folds are this luneta's alone, where a folder failed them.
    pub fn lost(&self) -> Option<&str> {
        match &self.sharing {
            Sharing::Lost(why) => Some(why),
            Sharing::Folder(_) | Sharing::Alone => None,
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Folds or unfolds `name`, for every luneta, and says whether that
    /// changed anything in this one. A folder that fails a write is given up,
    /// and the folds are this luneta's from then on, rather than undone by
    /// the next `reload`.
    pub fn set(&mut self, name: &SessionName, folded: bool) -> bool {
        if let Sharing::Folder(dir) = &self.sharing {
            let path = dir.join(name.as_str());
            let kept = if folded {
                fs::write(&path, "")
            } else {
                // Another luneta may have unfolded it first.
                fs::remove_file(&path).or_else(|error| match error.kind() {
                    ErrorKind::NotFound => Ok(()),
                    _ => Err(error),
                })
            };
            if let Err(error) = kept {
                self.sharing = Sharing::Lost(format!("{}: {error}", path.display()));
            }
        }
        let name = name.as_str();
        if folded { self.names.insert(name.to_owned()) } else { self.names.remove(name) }
    }

    /// Takes up what the other lunetas folded and unfolded since, and says
    /// whether that changed anything. A folder that cannot be read leaves the
    /// folds as they were.
    pub fn reload(&mut self) -> bool {
        let Sharing::Folder(dir) = &self.sharing else {
            return false;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let names: BTreeSet<String> =
            entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect();
        if names == self.names {
            return false;
        }
        self.names = names;
        true
    }
}

/// Tells luneta's loop each time a fold is made or taken away in `dir`, for
/// as long as the watcher it returns is kept.
pub fn watch(dir: &Path, events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch(&[dir], events, || Event::Folds)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    /// Two lunetas: what one folds or unfolds, the other shows once it
    /// reloads, and a luneta started later starts with it.
    #[test]
    fn a_fold_in_one_luneta_shows_in_every_other_and_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path().join("luneta").join("folded");
        let mut here = Folds::within(dir.clone());
        let mut there = Folds::within(dir.clone());

        assert!(here.set(&name("test"), true));
        assert!(here.set(&name("my work"), true));
        assert!(there.reload());
        assert!(there.contains("test") && there.contains("my work"));
        assert!(Folds::within(dir.clone()).contains("test"));

        assert!(there.set(&name("test"), false));
        assert!(here.reload());
        assert!(!here.contains("test") && here.contains("my work"));
        assert!(!here.reload());

        // Unfolded twice, as two lunetas may before they hear of each other:
        // the folder still shares what comes after.
        assert!(!here.set(&name("test"), false));
        assert!(here.set(&name("later"), true));
        there.reload();
        assert!(there.contains("later"));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    /// A folder luneta cannot keep the folds in leaves them to this luneta,
    /// and says why.
    #[test]
    fn a_folder_that_fails_keeps_the_folds_here() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("not a folder");
        fs::write(&file, "").unwrap();
        let mut folds = Folds::within(file.join("folded"));
        assert!(folds.lost().is_some_and(|lost| lost.contains("not a folder")));
        assert!(folds.set(&name("work"), true));
        assert!(folds.contains("work"));
    }

    /// What another luneta folds wakes this one's loop, and a read of the
    /// folder does not.
    #[test]
    fn a_fold_made_elsewhere_is_heard() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = Folds::within(scratch.path().to_owned());
        let (events, heard) = mpsc::channel();
        let _watcher = watch(scratch.path(), events).unwrap();
        // macOS can tell of the folder's own making once the watch has
        // started: FSEvents numbers a change as it hears of it, which can be
        // after. What comes before the folder goes quiet is that.
        while heard.recv_timeout(Duration::from_millis(200)).is_ok() {}
        here.reload();
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());

        Folds::within(scratch.path().to_owned()).set(&name("work"), true);
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Folds)));
        assert!(here.reload() && here.contains("work"));
    }
}
