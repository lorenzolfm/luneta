use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use lunetad::projects::{ProjectId, WorkstreamId};
use lunetad::Destination;
use notify::RecommendedWatcher;

use crate::host::Event;
use crate::machines::Machine;
use crate::state;

#[derive(Clone, Copy, Debug)]
pub enum Fold<'a> {
    Project(&'a Machine, ProjectId),
    Workstream(&'a Machine, WorkstreamId),
}

impl Fold<'_> {
    fn file(self) -> (Kept, String) {
        let (machine, name) = match self {
            Fold::Project(machine, project) => (machine, format!("project-{:016x}", project.get())),
            Fold::Workstream(machine, workstream) => {
                (machine, format!("workstream-{:016x}", workstream.get()))
            },
        };
        match machine {
            Machine::Local => (Kept::Sessions, name),
            Machine::Remote(destination) => {
                (Kept::Machines, format!("{}:{name}", spelled(destination)))
            },
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

    fn project(id: u64) -> Fold<'static> {
        Fold::Project(&Machine::Local, ProjectId::of(id).unwrap())
    }

    fn workstream(id: u64) -> Fold<'static> {
        Fold::Workstream(&Machine::Local, WorkstreamId::of(id).unwrap())
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

        assert!(here.set(project(1), true));
        assert!(here.set(workstream(2), true));
        assert!(there.reload());
        assert!(there.contains(project(1)) && there.contains(workstream(2)));
        assert!(within(&root).contains(project(1)));

        assert!(there.set(project(1), false));
        assert!(here.reload());
        assert!(!here.contains(project(1)) && here.contains(workstream(2)));
        assert!(!here.reload());

        assert!(!here.set(project(1), false));
        assert!(here.set(project(3), true));
        there.reload();
        assert!(there.contains(project(3)));
        assert_eq!((here.lost(), there.lost()), (None, None));
    }

    #[test]
    fn a_project_and_a_workstream_of_one_id_and_another_machine_s_fold_apart() {
        let scratch = tempfile::tempdir().unwrap();
        let mut here = within(scratch.path());
        let odd: Destination = "ssh://me@vps:22".parse().unwrap();
        let (vps, dot) = (Machine::Remote(odd), Machine::Remote(".".parse().unwrap()));
        let seven = ProjectId::of(7).unwrap();

        assert!(here.set(Fold::Project(&vps, seven), true));
        assert!(here.set(Fold::Workstream(&dot, WorkstreamId::of(7).unwrap()), true));
        let next = within(scratch.path());
        assert!(next.contains(Fold::Project(&vps, seven)));
        assert!(!next.contains(project(7)));
        assert!(!next.contains(workstream(7)));
        assert!(!next.contains(Fold::Project(&dot, seven)));
        assert!(next.contains(Fold::Workstream(&dot, WorkstreamId::of(7).unwrap())));
        assert_eq!(next.lost(), None);
    }

    #[test]
    fn a_folder_that_fails_keeps_the_folds_here() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("not a folder");
        fs::write(&file, "").unwrap();
        let mut folds = within(&file);
        assert!(folds.lost().is_some_and(|lost| lost.contains("not a folder")));
        assert!(folds.set(project(1), true));
        assert!(folds.contains(project(1)));
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

        within(scratch.path()).set(project(1), true);
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Folds)));
        assert!(here.reload() && here.contains(project(1)));
        let vps = Machine::Remote("vps".parse().unwrap());
        within(scratch.path()).set(Fold::Workstream(&vps, WorkstreamId::of(2).unwrap()), true);
        assert!(matches!(heard.recv_timeout(Duration::from_secs(5)), Ok(Event::Folds)));
        let two = WorkstreamId::of(2).unwrap();
        assert!(here.reload() && here.contains(Fold::Workstream(&vps, two)));
    }
}
