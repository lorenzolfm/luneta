use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use lune_domain::projects::{Ask, BARE, Dir, Id, Listed, PROJECTS_MAX};
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::api;
use crate::core::{self, Command};
use crate::layouts;
use crate::watch::{self, Troubles};

mod file;
pub(crate) mod spot;

pub use file::{Changed, Elsewhere, Failed, Forgotten, Lock, Revision, forget, holder, kept, take};
pub use file::{Opened, Store, Unheld};
pub use spot::{LOOK as SPOT_LOOK, Unlisted, clone_of, subfolders, subfolders_checked};

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
    Err(io::Error::other(format!(
        "/dev/urandom gave {RANDOM_TRIES} zeros in a row"
    )))
}

pub fn found(path: &str) -> Result<Dir, String> {
    found_in(path, std::env::var("HOME").ok().as_deref())
}

fn found_in(path: &str, home: Option<&str>) -> Result<Dir, String> {
    let home = || {
        let unset = format!("{path}: HOME is not set to an absolute path");
        home.filter(|home| home.starts_with('/'))
            .map(str::to_owned)
            .ok_or(unset)
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
    let text = found
        .to_str()
        .ok_or_else(|| format!("{} is not UTF-8", found.display()))?;
    Dir::parse(text)
}

pub fn asked(change: api::change_projects_request::Change) -> Result<Ask, String> {
    Ask::of_api(change, |folder| {
        found(folder).and_then(|it| spot::of(&it, SPOT_LOOK))
    })
}

pub fn follow(
    core: SyncSender<Command>,
    layouts: SyncSender<layouts::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new()
        .name("folders".to_owned())
        .spawn(move || {
            let mut troubles = Troubles::default();
            let mut watches = WorktreeWatches::default();
            loop {
                let Some(folders) = core::ask(&core, Command::ProjectFolders) else {
                    return;
                };
                let mut missing = BTreeSet::new();
                for folder in folders {
                    match gone(&folder) {
                        Ok(true) => {
                            missing.insert(folder);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let folder = folder.as_str();
                            let why =
                                format!("{folder}: {error}, and so it is not said to be missing");
                            troubles.say(why);
                        }
                    }
                }
                let Some(plain) = core::ask(&core, Command::Plain) else {
                    return;
                };
                for root in plain
                    .into_iter()
                    .filter(|it| !missing.contains(it) && spot::holds_bare(it))
                {
                    let spot = match spot::of(&root, SPOT_LOOK) {
                        Ok(spot) => spot,
                        Err(why) => {
                            let root = root.as_str();
                            troubles.say(format!("{root}: whose clone it is is unknown: {why}"));
                            continue;
                        }
                    };
                    let held = |it: &&Listed| *spot.dir() == root && !it.subfolders().is_empty();
                    let Some(listed) = spot.listed().filter(held).cloned() else {
                        continue;
                    };
                    if layouts
                        .send(layouts::Command::Rooted { root, listed })
                        .is_err()
                    {
                        return;
                    }
                }
                let Some(clones) = core::ask(&core, Command::Clones) else {
                    return;
                };
                assert!(
                    clones.len() <= PROJECTS_MAX,
                    "clones never exceed kept projects"
                );
                let present = clones.keys().filter(|root| !missing.contains(*root));
                watches.sync(present, &wake, &mut troubles);
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
                        }
                    };
                    listings.insert(root.clone(), core::Listing::Listed(listed.clone()));
                    if layouts
                        .send(layouts::Command::Worktrees { root, listed, seen })
                        .is_err()
                    {
                        return;
                    }
                }
                assert!(
                    listings.len() <= PROJECTS_MAX,
                    "a sweep lists the worktrees of kept clones"
                );
                troubles.done();
                if core.send(Command::Missing(missing)).is_err() {
                    return;
                }
                if core.send(Command::Listed(listings)).is_err() {
                    return;
                }
                match woken.recv_timeout(sweep) {
                    Ok(()) => watch::settle(&woken),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        unreachable!("follow holds its own wake sender");
                    }
                }
            }
        })?;
    Ok(())
}

const WORKTREES_FOLDER: &str = "worktrees";

const WATCHED_PATHS_MAX: usize = 2 * PROJECTS_MAX;

const _: () = assert!(WATCHED_PATHS_MAX == 2 * PROJECTS_MAX);

struct WatchedFolders {
    bare: BTreeSet<PathBuf>,
    worktrees: BTreeSet<PathBuf>,
}

enum PathState {
    Watched,
    Failed(String),
}

#[derive(Default)]
enum WorktreeWatches {
    #[default]
    Idle,
    Watching {
        _watcher: RecommendedWatcher,
        rescan: Arc<AtomicBool>,
        paths: BTreeMap<PathBuf, PathState>,
    },
    Unwatched {
        failed: BTreeMap<PathBuf, String>,
    },
}

impl WorktreeWatches {
    #[cfg(test)]
    fn is_watching(&self, path: &Path) -> bool {
        matches!(
            self,
            WorktreeWatches::Watching { paths, .. }
                if matches!(paths.get(path), Some(PathState::Watched))
        )
    }

    fn sync<'a>(
        &mut self,
        clones: impl IntoIterator<Item = &'a Dir>,
        wake: &SyncSender<()>,
        troubles: &mut Troubles,
    ) {
        let mut folders = WatchedFolders {
            bare: BTreeSet::new(),
            worktrees: BTreeSet::new(),
        };
        let mut desired = BTreeSet::new();
        let mut clones_seen = 0_usize;
        for root in clones {
            clones_seen += 1;
            assert!(
                clones_seen <= PROJECTS_MAX,
                "clones watched never exceed kept projects"
            );
            let bare_at = Path::new(root.as_str()).join(BARE);
            let bare = match fs::canonicalize(&bare_at) {
                Ok(bare) => bare,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    troubles.say(format!(
                        "{} is read on the sweep alone: {error}",
                        bare_at.display()
                    ));
                    continue;
                }
            };
            let worktrees_at = bare.join(WORKTREES_FOLDER);
            let worktrees = match fs::canonicalize(&worktrees_at) {
                Ok(worktrees) => worktrees,
                Err(error) if error.kind() == ErrorKind::NotFound => worktrees_at,
                Err(error) => {
                    troubles.say(format!(
                        "{} is read on the sweep alone: {error}",
                        worktrees_at.display()
                    ));
                    worktrees_at
                }
            };
            folders.bare.insert(bare.clone());
            folders.worktrees.insert(worktrees.clone());
            desired.insert(bare);
            if worktrees.is_dir() {
                desired.insert(worktrees);
            }
        }
        assert!(
            desired.len() <= WATCHED_PATHS_MAX,
            "at most bare and worktrees folders for each kept clone"
        );
        if desired.is_empty() {
            *self = WorktreeWatches::Idle;
            return;
        }
        match self {
            WorktreeWatches::Idle => {}
            WorktreeWatches::Watching { rescan, paths, .. } => {
                let stale = rescan.swap(false, Ordering::SeqCst);
                if !stale && paths.keys().eq(desired.iter()) {
                    for state in paths.values() {
                        if let PathState::Failed(trouble) = state {
                            troubles.say(trouble.clone());
                        }
                    }
                    return;
                }
            }
            WorktreeWatches::Unwatched { failed } => {
                if failed.keys().eq(desired.iter()) {
                    for trouble in failed.values() {
                        troubles.say(trouble.clone());
                    }
                    return;
                }
            }
        }
        *self = WorktreeWatches::Idle;
        let rescan = Arc::new(AtomicBool::new(false));
        let handler_rescan = Arc::clone(&rescan);
        let wake = wake.clone();
        let handler = move |event: notify::Result<Event>| {
            match worktree_word(&event, &folders) {
                WorktreeWord::Quiet => return,
                WorktreeWord::Changed => {}
                WorktreeWord::Rescan => handler_rescan.store(true, Ordering::SeqCst),
            }
            match wake.try_send(()) {
                Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
            }
        };
        let config = notify::Config::default().with_follow_symlinks(false);
        let mut watcher = match RecommendedWatcher::new(handler, config) {
            Ok(watcher) => watcher,
            Err(error) => {
                troubles.say(format!("worktrees are read on the sweep alone: {error}"));
                return;
            }
        };
        let mut paths = BTreeMap::new();
        let mut watched_any = false;
        let mut adding = watcher.paths_mut();
        for path in &desired {
            match adding.add(path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    paths.insert(path.clone(), PathState::Watched);
                    watched_any = true;
                }
                Err(error) if watch::missing(&error) => {}
                Err(error) => {
                    let why = format!("{} is read on the sweep alone: {error}", path.display());
                    paths.insert(path.clone(), PathState::Failed(why));
                }
            }
        }
        if let Err(error) = adding.commit() {
            troubles.say(format!("worktrees are read on the sweep alone: {error}"));
            return;
        }
        assert!(
            paths.len() <= WATCHED_PATHS_MAX,
            "watched and unwatched folders stay within bound"
        );
        for state in paths.values() {
            if let PathState::Failed(trouble) = state {
                troubles.say(trouble.clone());
            }
        }
        *self = if watched_any {
            WorktreeWatches::Watching {
                _watcher: watcher,
                rescan,
                paths,
            }
        } else if paths.is_empty() {
            WorktreeWatches::Idle
        } else {
            let failed = paths
                .into_iter()
                .filter_map(|(path, state)| match state {
                    PathState::Failed(why) => Some((path, why)),
                    PathState::Watched => None,
                })
                .collect();
            WorktreeWatches::Unwatched { failed }
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorktreeWord {
    Quiet,
    Changed,
    Rescan,
}

fn worktree_word(event: &notify::Result<Event>, folders: &WatchedFolders) -> WorktreeWord {
    let Ok(event) = event else {
        return WorktreeWord::Rescan;
    };
    if event.need_rescan() {
        return WorktreeWord::Rescan;
    }
    if event.kind.is_access() {
        return WorktreeWord::Quiet;
    }
    if event.paths.is_empty() {
        return WorktreeWord::Changed;
    }
    let structural = matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    );
    let mut word = WorktreeWord::Quiet;
    for path in &event.paths {
        if folders.worktrees.contains(path) || (structural && folders.bare.contains(path)) {
            return WorktreeWord::Rescan;
        }
        if path
            .parent()
            .is_some_and(|parent| folders.worktrees.contains(parent))
        {
            word = WorktreeWord::Changed;
        }
    }
    word
}

fn gone(folder: &Dir) -> io::Result<bool> {
    match fs::metadata(folder.as_str()) {
        Ok(metadata) => Ok(!metadata.is_dir()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(true)
        }
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
        assert_eq!(
            found(link.to_str().unwrap()).unwrap().as_str(),
            real.to_str().unwrap()
        );
        fs::write(real.join("file"), "").unwrap();
        let file = real.join("file");
        assert!(
            found(file.to_str().unwrap())
                .unwrap_err()
                .ends_with("is no folder")
        );
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
        assert!(
            found(nowhere.to_str().unwrap())
                .unwrap_err()
                .contains("No such file")
        );
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

    #[test]
    fn only_worktrees_changes_wake_a_clone_s_watch() {
        use notify::event::{AccessKind, CreateKind, DataChange, ModifyKind, RemoveKind};

        let bare = PathBuf::from("/w/lune/.bare");
        let worktrees = bare.join(WORKTREES_FOLDER);
        let folders = WatchedFolders {
            bare: BTreeSet::from([bare.clone()]),
            worktrees: BTreeSet::from([worktrees.clone()]),
        };
        let read = |kind, path: PathBuf| {
            let event = Ok(Event::new(kind).add_path(path));
            worktree_word(&event, &folders)
        };
        assert_eq!(
            read(
                EventKind::Create(CreateKind::Folder),
                worktrees.join("feat")
            ),
            WorktreeWord::Changed
        );
        assert_eq!(
            read(
                EventKind::Remove(RemoveKind::Folder),
                worktrees.join("feat")
            ),
            WorktreeWord::Changed
        );
        assert_eq!(
            read(EventKind::Create(CreateKind::Folder), worktrees.clone()),
            WorktreeWord::Rescan
        );
        assert_eq!(
            read(EventKind::Remove(RemoveKind::Folder), worktrees.clone()),
            WorktreeWord::Rescan
        );
        assert_eq!(
            read(EventKind::Remove(RemoveKind::Folder), bare.clone()),
            WorktreeWord::Rescan
        );
        assert_eq!(
            read(
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                worktrees.join("main/index")
            ),
            WorktreeWord::Quiet
        );
        assert_eq!(
            read(
                EventKind::Modify(ModifyKind::Data(DataChange::Any)),
                bare.join("refs/heads/main")
            ),
            WorktreeWord::Quiet
        );
        assert_eq!(
            read(EventKind::Access(AccessKind::Any), worktrees.join("feat")),
            WorktreeWord::Quiet
        );
        assert_eq!(
            worktree_word(&Err(notify::Error::generic("full")), &folders),
            WorktreeWord::Rescan
        );
    }

    #[test]
    fn a_clone_s_worktrees_folder_wakes_as_worktrees_come_and_go() {
        use std::sync::mpsc;

        let scratch = tempfile::tempdir().unwrap();
        let clone = fs::canonicalize(scratch.path()).unwrap().join("lune");
        let bare = clone.join(BARE);
        fs::create_dir_all(&bare).unwrap();
        let root = dir(clone.to_str().unwrap());
        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = WorktreeWatches::default();
        let mut troubles = Troubles::default();
        watches.sync([&root], &wake, &mut troubles);

        let worktrees = bare.join(WORKTREES_FOLDER);
        fs::create_dir(&worktrees).unwrap();
        let () = lune_waits::heard("creating worktrees wakes the watch", &woken);
        watches.sync([&root], &wake, &mut troubles);
        assert!(watches.is_watching(&worktrees));

        let feat = worktrees.join("feat");
        fs::create_dir(&feat).unwrap();
        let () = lune_waits::heard("adding a worktree wakes the watch", &woken);
        fs::write(feat.join("index"), "staged").unwrap();
        lune_waits::never("an edit inside a worktree does not wake the watch", || {
            woken.try_recv().ok()
        });
        fs::remove_dir_all(&feat).unwrap();
        let () = lune_waits::heard("removing a worktree wakes the watch", &woken);

        watches.sync([], &wake, &mut troubles);
        assert!(matches!(watches, WorktreeWatches::Idle));
    }
}
