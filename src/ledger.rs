//! A record for each pane of each session, the same in every session's
//! sidebar.
//!
//! Each session has a sidebar of its own, and each sees only some of what
//! goes on: you, in a pane of its own session, and nothing at all while its
//! session has no client. What one sees, the others read from the cache
//! zellij gives the plugin, where the folds are kept too: a folder for each
//! session, and a file in it for each pane, named for its id. Two sidebars
//! never write over each other's panes, and a file is written whole before it
//! takes its pane's name, so that no sidebar reads one halfway written.
//!
//! A session's name holds no `/`, and is neither `.` nor `..`, so it names a
//! folder.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

/// What a ledger keeps of a pane, as its file spells it.
pub trait Record: Sized {
    fn read(text: &str) -> Option<Self>;
    fn write(&self) -> String;
}

/// The name a file takes while it is written, before it takes its pane's.
const UNFINISHED: &str = "new";

pub struct Ledger<T> {
    records: BTreeMap<String, BTreeMap<u32, T>>,
    /// The folder they are shared through. Without one, the records are this
    /// sidebar's alone: a test's, or one whose folder failed it.
    dir: Option<PathBuf>,
}

impl<T> Default for Ledger<T> {
    fn default() -> Self {
        Ledger { records: BTreeMap::new(), dir: None }
    }
}

impl<T: Record> Ledger<T> {
    /// The records every session's sidebar keeps in `dir`, as they stand.
    pub fn within(dir: PathBuf) -> Self {
        let dir = match fs::create_dir_all(&dir) {
            Ok(()) => Some(dir),
            Err(error) => {
                eprintln!("luneta: {} stays in this session: {error}", dir.display());
                None
            },
        };
        let mut ledger = Ledger { records: BTreeMap::new(), dir };
        ledger.reload();
        ledger
    }

    pub fn get(&self, session: &str, pane: u32) -> Option<&T> {
        self.records.get(session)?.get(&pane)
    }

    /// Keeps `record` for `pane` of `session`, for every sidebar, or with none
    /// takes the pane's away. A folder that fails a write is given up, and the
    /// records are this sidebar's from then on, rather than undone by the
    /// next `reload`.
    pub fn set(&mut self, session: &str, pane: u32, record: Option<T>) {
        if let Some(dir) = &self.dir {
            let folder = dir.join(session);
            let file = folder.join(pane.to_string());
            let kept = match &record {
                Some(record) => {
                    let unfinished = folder.join(format!("{pane}.{UNFINISHED}"));
                    fs::create_dir_all(&folder)
                        .and_then(|()| fs::write(&unfinished, record.write()))
                        .and_then(|()| fs::rename(&unfinished, &file))
                },
                // Another sidebar may have taken it away first.
                None => fs::remove_file(&file).or_else(|error| match error.kind() {
                    ErrorKind::NotFound => Ok(()),
                    _ => Err(error),
                }),
            };
            if let Err(error) = kept {
                eprintln!("luneta: {} stays in this session: {error}", dir.display());
                self.dir = None;
            }
        }
        match record {
            Some(record) => {
                self.records.entry(session.to_owned()).or_default().insert(pane, record);
            },
            None => {
                if let Some(panes) = self.records.get_mut(session) {
                    panes.remove(&pane);
                }
            },
        }
    }

    /// Takes up what the other sidebars wrote since. A folder that cannot be
    /// read leaves the records as they were, and a file that does not read as
    /// a record is no record.
    pub fn reload(&mut self) {
        let Some(dir) = &self.dir else {
            return;
        };
        let Ok(sessions) = fs::read_dir(dir) else {
            return;
        };
        let mut records = BTreeMap::new();
        for session in sessions.flatten() {
            let Ok(name) = session.file_name().into_string() else {
                continue;
            };
            let Ok(panes) = fs::read_dir(session.path()) else {
                continue;
            };
            let read = panes.flatten().filter_map(|pane| {
                let id = pane.file_name().to_str()?.parse().ok()?;
                let record = T::read(&fs::read_to_string(pane.path()).ok()?)?;
                Some((id, record))
            });
            records.insert(name, read.collect());
        }
        self.records = records;
    }

    /// The sessions it keeps records of.
    pub fn sessions(&self) -> impl Iterator<Item = &str> {
        self.records.keys().map(String::as_str)
    }

    /// Takes away every record of `session`, for every sidebar.
    pub fn forget(&mut self, session: &str) {
        self.records.remove(session);
        let Some(dir) = &self.dir else {
            return;
        };
        match fs::remove_dir_all(dir.join(session)) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => eprintln!("luneta: {}: {error}", dir.join(session).display()),
        }
    }

    /// Hands the records of `from` to `to`, for a session renamed: its panes
    /// keep their ids. Whatever `to` had before is another session's.
    pub fn rename(&mut self, from: &str, to: &str) {
        self.forget(to);
        if let Some(panes) = self.records.remove(from) {
            self.records.insert(to.to_owned(), panes);
        }
        let Some(dir) = &self.dir else {
            return;
        };
        match fs::rename(dir.join(from), dir.join(to)) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => eprintln!("luneta: {}: {error}", dir.join(from).display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Record for String {
        fn read(text: &str) -> Option<Self> {
            Some(text.trim().to_owned()).filter(|text| !text.is_empty())
        }

        fn write(&self) -> String {
            format!("{self}\n")
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("luneta-ledger-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn get(ledger: &Ledger<String>, session: &str, pane: u32) -> Option<String> {
        ledger.get(session, pane).cloned()
    }

    /// Two sessions' sidebars: what one keeps or takes away, the other has
    /// once it reloads, and a sidebar loaded later starts with it.
    #[test]
    fn a_record_one_sidebar_keeps_is_read_by_every_other() {
        let dir = scratch("shared");
        let mut here: Ledger<String> = Ledger::within(dir.clone());
        let mut there: Ledger<String> = Ledger::within(dir.clone());

        here.set("work", 3, Some("seen".to_owned()));
        here.set("my notes", 0, Some("ran".to_owned()));
        there.reload();
        assert_eq!(get(&there, "work", 3).as_deref(), Some("seen"));
        assert_eq!(get(&there, "my notes", 0).as_deref(), Some("ran"));
        assert_eq!(get(&Ledger::within(dir.clone()), "work", 3).as_deref(), Some("seen"));

        there.set("work", 3, None);
        // Taken away twice, as two sidebars may before they hear of each other.
        here.set("work", 3, None);
        here.reload();
        assert_eq!(get(&here, "work", 3), None);
        assert_eq!(get(&here, "my notes", 0).as_deref(), Some("ran"));

        fs::remove_dir_all(dir).unwrap();
    }

    /// A file that does not read as a record, or whose name is no pane's id,
    /// as one still being written, is no record.
    #[test]
    fn only_a_pane_s_file_that_reads_as_a_record_is_one() {
        let dir = scratch("unread");
        let mut ledger: Ledger<String> = Ledger::within(dir.clone());
        ledger.set("work", 1, Some("seen".to_owned()));
        fs::write(dir.join("work").join("2"), "\n").unwrap();
        fs::write(dir.join("work").join("3.new"), "half").unwrap();
        ledger.reload();
        let panes: Vec<u32> = (0..4).filter(|pane| ledger.get("work", *pane).is_some()).collect();
        assert_eq!(panes, [1]);

        fs::remove_dir_all(dir).unwrap();
    }

    /// A session forgotten takes its records with it, for every sidebar, and
    /// one renamed takes them to its new name, over whatever was there.
    #[test]
    fn a_session_s_records_go_with_it() {
        let dir = scratch("sessions");
        let mut here: Ledger<String> = Ledger::within(dir.clone());
        here.set("old", 1, Some("seen".to_owned()));
        here.set("new", 1, Some("stale".to_owned()));
        here.set("gone", 4, Some("ran".to_owned()));

        here.rename("old", "new");
        here.forget("gone");
        for ledger in [&here, &Ledger::within(dir.clone())] {
            assert_eq!(ledger.sessions().collect::<Vec<_>>(), ["new"]);
            assert_eq!(get(ledger, "new", 1).as_deref(), Some("seen"));
        }

        fs::remove_dir_all(dir).unwrap();
    }
}
