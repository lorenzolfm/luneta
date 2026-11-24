use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use lune_domain::projects::{Ask, Dir, Id, Listed, PROJECTS_MAX};

use crate::api;
use crate::core::{self, Command};
use crate::layouts;
use crate::watch::Troubles;

mod file;
pub(crate) mod spot;

pub use file::{forget, holder, kept, take, Changed, Elsewhere, Failed, Forgotten, Lock, Revision};
pub use file::{Opened, Store, Unheld};
pub use spot::{clone_of, subfolders, LOOK as SPOT_LOOK};

const RANDOM_TRIES: u32 = 8;

pub fn random_id() -> io::Result<Id> {
    let mut urandom = File::open("/dev/urandom")?;
    for _ in 0..RANDOM_TRIES {
        let mut bytes = [0; 8];
        urandom.read_exact(&mut bytes)?;
        if let Some(id) = Id::of(u64::from_ne_bytes(bytes)) {
            return Ok(id);
        }
    }
    Err(io::Error::other(format!("/dev/urandom gave {RANDOM_TRIES} zeros in a row")))
}

pub fn found(path: &str) -> Result<Dir, String> {
    found_in(path, std::env::var("HOME").ok().as_deref())
}

fn found_in(path: &str, home: Option<&str>) -> Result<Dir, String> {
    let home = || {
        let unset = format!("{path}: HOME is not set to an absolute path");
        home.filter(|home| home.starts_with('/')).map(str::to_owned).ok_or(unset)
    };
    let path = match Dir::below_home(path) {
        Some(below) => format!("{}{below}", home()?),
        None => path.to_owned(),
    };
    let path = path.as_str();
    if !path.starts_with('/') {
        return Err(format!("{path:?} is no absolute path"));
    }
    let found = fs::canonicalize(path).map_err(|error| format!("{path}: {error}"))?;
    if !found.is_dir() {
        return Err(format!("{path} is no folder"));
    }
    let text = found.to_str().ok_or_else(|| format!("{} is not UTF-8", found.display()))?;
    Dir::parse(text)
}

pub fn asked(change: api::change_projects_request::Change) -> Result<Ask, String> {
    Ask::of_api(change, |folder| found(folder).and_then(|it| spot::of(&it, SPOT_LOOK)))
}

pub fn follow(
    core: SyncSender<Command>,
    layouts: SyncSender<layouts::Command>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("folders".to_owned()).spawn(move || {
        let mut troubles = Troubles::default();
        loop {
            let Some(folders) = core::ask(&core, Command::ProjectFolders) else {
                return;
            };
            let mut missing = BTreeSet::new();
            for folder in folders {
                match gone(&folder) {
                    Ok(true) => {
                        missing.insert(folder);
                    },
                    Ok(false) => {},
                    Err(error) => {
                        let folder = folder.as_str();
                        let why = format!("{folder}: {error}, and so it is not said to be missing");
                        troubles.say(why);
                    },
                }
            }
            let Some(plain) = core::ask(&core, Command::Plain) else {
                return;
            };
            for root in plain.into_iter().filter(|it| !missing.contains(it) && spot::holds_bare(it)) {
                let spot = match spot::of(&root, SPOT_LOOK) {
                    Ok(spot) => spot,
                    Err(why) => {
                        let root = root.as_str();
                        troubles.say(format!("{root}: whose clone it is is unknown: {why}"));
                        continue;
                    },
                };
                let held = |it: &&Listed| *spot.dir() == root && !it.subfolders().is_empty();
                let Some(listed) = spot.listed().filter(held).cloned() else {
                    continue;
                };
                if layouts.send(layouts::Command::Rooted { root, listed }).is_err() {
                    return;
                }
            }
            let Some(clones) = core::ask(&core, Command::Clones) else {
                return;
            };
            let mut listings = BTreeMap::new();
            for (root, seen) in clones {
                if missing.contains(&root) {
                    let gone = format!("{} is missing", root.as_str());
                    listings.insert(root, core::Listing::Unlisted(gone));
                    continue;
                }
                let listed = match spot::subfolders(&root, SPOT_LOOK) {
                    Ok(listed) => listed,
                    Err(why) => {
                        let at = root.as_str();
                        troubles.say(format!("{at}: its worktrees are not listed: {why}"));
                        listings.insert(root, core::Listing::Unlisted(why));
                        continue;
                    },
                };
                listings.insert(root.clone(), core::Listing::Listed(listed.clone()));
                if layouts.send(layouts::Command::Worktrees { root, listed, seen }).is_err() {
                    return;
                }
            }
            assert!(listings.len() <= PROJECTS_MAX, "a sweep lists the worktrees of kept clones");
            troubles.done();
            if core.send(Command::Missing(missing)).is_err() {
                return;
            }
            if core.send(Command::Listed(listings)).is_err() {
                return;
            }
            thread::sleep(sweep);
        }
    })?;
    Ok(())
}

fn gone(folder: &Dir) -> io::Result<bool> {
    match fs::metadata(folder.as_str()) {
        Ok(metadata) => Ok(!metadata.is_dir()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(true)
        },
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;

    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    #[test]
    fn a_random_id_reads_back_as_it_is_written() {
        let id = random_id().unwrap();
        assert_eq!(id.to_string().parse::<Id>(), Ok(id));
    }

    #[test]
    fn a_folder_asked_for_is_found_where_it_really_is() {
        let scratch = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(scratch.path()).unwrap().join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, real.with_file_name("link")).unwrap();
        let link = real.with_file_name("link");
        assert_eq!(found(link.to_str().unwrap()).unwrap().as_str(), real.to_str().unwrap());
        fs::write(real.join("file"), "").unwrap();
        let file = real.join("file");
        assert!(found(file.to_str().unwrap()).unwrap_err().ends_with("is no folder"));
        assert!(found("real").unwrap_err().contains("no absolute path"));
        assert!(found("~me/real").unwrap_err().contains("no absolute path"));
        let home = real.to_str();
        for at in ["~", "~/"] {
            assert_eq!(found_in(at, home).unwrap().as_str(), real.to_str().unwrap());
        }
        let below = found_in("~/file", home).unwrap_err();
        assert!(below.ends_with("is no folder"), "{below}");
        let unset = found_in("~", None).unwrap_err();
        assert!(unset.contains("HOME is not set"), "{unset}");
        let nowhere = real.join("nowhere");
        assert!(found(nowhere.to_str().unwrap()).unwrap_err().contains("No such file"));
    }

    #[test]
    fn a_folder_is_gone_only_when_nothing_or_no_folder_is_there() {
        let scratch = tempfile::tempdir().unwrap();
        let at = |path: &Path| dir(path.to_str().unwrap());
        let real = fs::canonicalize(scratch.path()).unwrap();
        let (file, locked) = (real.join("file"), real.join("locked"));
        fs::write(&file, "").unwrap();
        fs::create_dir_all(locked.join("inner")).unwrap();
        assert!(!gone(&at(&real)).unwrap());
        assert!(gone(&at(&file)).unwrap());
        assert!(gone(&at(&real.join("nowhere"))).unwrap());
        assert!(gone(&at(&file.join("below"))).unwrap());
        let permissions = |mode| fs::Permissions::from_mode(mode);
        fs::set_permissions(&locked, permissions(0o000)).unwrap();
        let unread = gone(&at(&locked.join("inner")));
        fs::set_permissions(&locked, permissions(0o755)).unwrap();
        assert_eq!(unread.unwrap_err().kind(), ErrorKind::PermissionDenied);
    }
}
