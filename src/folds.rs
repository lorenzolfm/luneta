//! The sessions folded in the sidebar, the same in every session's.
//!
//! Each session has a sidebar of its own, and zellij gives a plugin one folder
//! that every session shares and that outlives them, `/cache`: the same folder
//! for each session that loads luneta from the same location. A folded session
//! is a file there named for it, so that a fold is a file to make or take
//! away, and two sidebars never write over each other. A session's name holds
//! no `/`, and is neither `.` nor `..`, so it names a file.

use std::collections::BTreeSet;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

const DIR: &str = "/cache/folded";

#[derive(Default)]
pub struct Folds {
    names: BTreeSet<String>,
    /// The folder they are shared through. Without one, the folds are this
    /// sidebar's alone: a test's, or one whose folder failed it.
    dir: Option<PathBuf>,
}

impl Folds {
    /// The folds every session's sidebar shares, as they stand.
    pub fn shared() -> Self {
        Folds::within(PathBuf::from(DIR))
    }

    fn within(dir: PathBuf) -> Self {
        let dir = match fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(error) => {
                eprintln!("luneta: folds stay in this session: {}: {error}", dir.display());
                None
            },
        };
        let mut folds = Folds { names: BTreeSet::new(), dir };
        folds.reload();
        folds
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Folds or unfolds `name`, for every sidebar, and says whether that
    /// changed anything in this one. A folder that fails a write is given up,
    /// and the folds are this sidebar's from then on, rather than undone by
    /// the next `reload`.
    pub fn set(&mut self, name: &str, folded: bool) -> bool {
        if let Some(dir) = &self.dir {
            let path = dir.join(name);
            let kept = if folded {
                fs::write(&path, "")
            } else {
                // Another sidebar may have unfolded it first.
                fs::remove_file(&path).or_else(|error| match error.kind() {
                    ErrorKind::NotFound => Ok(()),
                    _ => Err(error),
                })
            };
            if let Err(error) = kept {
                eprintln!("luneta: folds stay in this session: {}: {error}", path.display());
                self.dir = None;
            }
        }
        if folded {
            self.names.insert(name.to_owned())
        } else {
            self.names.remove(name)
        }
    }

    /// Takes up what the other sidebars folded and unfolded since. A folder
    /// that cannot be read leaves the folds as they were.
    pub fn reload(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        self.names =
            entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sessions' sidebars: what one folds or unfolds, the other shows
    /// once it reloads, and a sidebar loaded later starts with it.
    #[test]
    fn a_fold_in_one_session_s_sidebar_shows_in_every_other() {
        let dir = std::env::temp_dir().join(format!("luneta-folds-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut here = Folds::within(dir.clone());
        let mut there = Folds::within(dir.clone());

        assert!(here.set("test", true));
        assert!(here.set("my work", true));
        there.reload();
        assert!(there.contains("test") && there.contains("my work"));
        assert!(Folds::within(dir.clone()).contains("test"));

        assert!(there.set("test", false));
        here.reload();
        assert!(!here.contains("test") && here.contains("my work"));

        // Unfolded twice, as two sidebars may before they hear of each other:
        // the folder still shares what comes after.
        assert!(!here.set("test", false));
        assert!(here.set("later", true));
        there.reload();
        assert!(there.contains("later"));

        fs::remove_dir_all(dir).unwrap();
    }
}
