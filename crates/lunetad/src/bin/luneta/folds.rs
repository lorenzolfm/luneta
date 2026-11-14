use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use lunetad::Destination;
use notify::RecommendedWatcher;

use crate::host::Event;
use crate::machines::Machine;
use crate::state;
use crate::zellij::SessionName;

const LOCAL: &str = "-";

#[derive(Clone, Copy, Debug)]
pub enum Fold<'a> {
    Machine(&'a Machine),
    Session(&'a Machine, &'a SessionName),
}

impl Fold<'_> {
    fn file(self) -> (Kept, String) {
        match self {
            Fold::Session(Machine::Local, name) => (Kept::Sessions, name.as_str().to_owned()),
            Fold::Session(Machine::Remote(destination), name) => {
                (Kept::Machines, format!("{}:{name}", spelled(destination)))
            },
            Fold::Machine(Machine::Local) => (Kept::Machines, LOCAL.to_owned()),
            Fold::Machine(Machine::Remote(destination)) => (Kept::Machines, spelled(destination)),
        }
    }
}

#[derive(Clone, Copy)]
enum Kept {
    Sessions,
    Machines,
}

fn spelled(destination: &Destination) -> String {
    let mut spelled = String::new();
    for (at, c) in destination.to_string().char_indices() {
        if matches!(c, '%' | '/' | ':') || (at == 0 && c == '.') {
            write!(spelled, "%{:02X}", u32::from(c)).expect("a string takes what is written");
        } else {
            spelled.push(c);
        }
    }
    spelled
}

#[derive(Default)]
pub struct Folds {
    sessions: BTreeSet<String>,
    machines: BTreeSet<String>,
    sharing: Sharing,
}

#[derive(Default)]
enum Sharing {
    Folders { sessions: PathBuf, machines: PathBuf },
    #[default]
    Alone,
    Lost(String),
}

impl Folds {
    pub fn shared() -> Self {
        let dirs = state::path("folded").and_then(|sessions| {
            Ok((sessions, state::path("folded-machines")?))
        });
        match dirs {
            Ok((sessions, machines)) => Folds::within(sessions, machines),
            Err(unset) => Folds { sharing: Sharing::Lost(unset.to_string()), ..Folds::default() },
        }
    }

    fn within(sessions: PathBuf, machines: PathBuf) -> Self {
        let made = [&sessions, &machines]
            .into_iter()
            .try_for_each(|dir| fs::create_dir_all(dir).map_err(|error| (dir.clone(), error)));
        let sharing = match made {
            Ok(()) => Sharing::Folders { sessions, machines },
            Err((dir, error)) => Sharing::Lost(format!("{}: {error}", dir.display())),
        };
        let mut folds = Folds { sharing, ..Folds::default() };
        folds.reload();
        folds
    }

    pub fn dirs(&self) -> Vec<&Path> {
        match &self.sharing {
            Sharing::Folders { sessions, machines } => vec![sessions, machines],
            Sharing::Alone | Sharing::Lost(_) => Vec::new(),
        }
    }

    pub fn lost(&self) -> Option<&str> {
        match &self.sharing {
            Sharing::Lost(why) => Some(why),
            Sharing::Folders { .. } | Sharing::Alone => None,
        }
    }

    pub fn contains(&self, fold: Fold) -> bool {
        let (kept, name) = fold.file();
        self.names(kept).contains(&name)
    }

    pub fn set(&mut self, fold: Fold, folded: bool) -> bool {
        let (kept, name) = fold.file();
        if let Sharing::Folders { sessions, machines } = &self.sharing {
            let dir = match kept {
                Kept::Sessions => sessions,
                Kept::Machines => machines,
            };
            let path = dir.join(&name);
            let written = if folded {
                fs::write(&path, "")
            } else {
                fs::remove_file(&path).or_else(|error| match error.kind() {
                    ErrorKind::NotFound => Ok(()),
                    _ => Err(error),
                })
            };
            if let Err(error) = written {
                self.sharing = Sharing::Lost(format!("{}: {error}", path.display()));
            }
        }
        let names = match kept {
            Kept::Sessions => &mut self.sessions,
            Kept::Machines => &mut self.machines,
        };
        if folded { names.insert(name) } else { names.remove(&name) }
    }

    fn names(&self, kept: Kept) -> &BTreeSet<String> {
        match kept {
            Kept::Sessions => &self.sessions,
            Kept::Machines => &self.machines,
        }
    }

    pub fn reload(&mut self) -> bool {
        let Sharing::Folders { sessions, machines } = &self.sharing else {
            return false;
        };
        let read = |dir: &Path| -> Option<BTreeSet<String>> {
            let entries = fs::read_dir(dir).ok()?;
            Some(entries.filter_map(|entry| entry.ok()?.file_name().into_string().ok()).collect())
        };
        let read = [(&mut self.sessions, read(sessions)), (&mut self.machines, read(machines))];
        let mut changed = false;
        for (names, read) in read {
            if let Some(read) = read.filter(|read| read != names) {
                *names = read;
                changed = true;
            }
        }
        changed
    }
}

pub fn watch(dirs: &[&Path], events: Sender<Event>) -> notify::Result<RecommendedWatcher> {
    state::watch(dirs, events, || Event::Folds)
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    fn session(name: &SessionName) -> Fold<'_> {
        Fold::Session(&Machine::Local, name)
    }

    fn within(root: &Path) -> Folds {
        Folds::within(root.join("folded"), root.join("folded-machines"))
    }

    #[test]
    fn a_fold_in_one_luneta_shows_in_every_other_and_the_next() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("luneta");
        let mut here = within(&root);
        let mut there = within(&root);
        let (test, work) = (name("test"), name("my work"));

        assert!(here.set(session(&test), true));
        assert!(here.set(session(&work), true));
        assert!(there.reload());
        assert!(there.contains(session(&test)) && there.contains(session(&work)));
        assert!(within(&root).contains(session(&test)));

        assert!(there.set(session(&test), false));
        assert!(here.reload());
        assert!(!here.contains(session(&test)) && here.contains(session(&work)));
        assert!(!here.reload());

        assert!(!here.set(session(&test), false));
        let later = name("later");
        assert!(here.set(session(&later), true));
        there.reload();
        assert!(there.contains(session(&later)));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    #[test]
    fn a_machine_and_its_sessions_fold_apart_from_this_one_s() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = within(scratch.path());
        let odd: Destination = "ssh://me@vps:22".parse().unwrap();
        let (vps, dot) = (Machine::Remote(odd), Machine::Remote(".".parse().unwrap()));
        let main = name("main");

        assert!(here.set(Fold::Session(&vps, &main), true));
        assert!(here.set(Fold::Machine(&dot), true));
        assert!(here.set(Fold::Machine(&Machine::Local), true));
        let next = within(scratch.path());
        assert!(next.contains(Fold::Session(&vps, &main)));
        assert!(!next.contains(session(&main)));
        assert!(!next.contains(Fold::Machine(&vps)));
        assert!(next.contains(Fold::Machine(&dot)));
        assert!(next.contains(Fold::Machine(&Machine::Local)));
        assert_eq!(next.lost(), None);
    }

    #[test]
    fn a_folder_that_fails_keeps_the_folds_here() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("not a folder");
        fs::write(&file, "").unwrap();
        let mut folds = within(&file);
        assert!(folds.lost().is_some_and(|lost| lost.contains("not a folder")));
        let work = name("work");
        assert!(folds.set(session(&work), true));
        assert!(folds.contains(session(&work)));
    }

    #[test]
    fn a_fold_made_elsewhere_is_heard() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = within(scratch.path());
        let (events, heard) = mpsc::channel();
        let _watcher = watch(&here.dirs(), events).unwrap();
        while heard.recv_timeout(Duration::from_millis(200)).is_ok() {}
        here.reload();
        assert!(heard.recv_timeout(Duration::from_millis(200)).is_err());

        let work = name("work");
        within(scratch.path()).set(session(&work), true);
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Folds)));
        assert!(here.reload() && here.contains(session(&work)));
        let vps = Machine::Remote("vps".parse().unwrap());
        within(scratch.path()).set(Fold::Machine(&vps), true);
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Folds)));
        assert!(here.reload() && here.contains(Fold::Machine(&vps)));
    }
}
