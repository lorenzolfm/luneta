//! How the work tree stands in each directory a pane works in, as `git status`
//! says it.
//!
//! git tells no one as a work tree changes, but each change is to a file: in
//! the folders git keeps the work tree in, as a commit, a checkout, a stage, a
//! stash or a fetch writes there, or in a directory of the work tree, as a
//! file is written, made or taken away. Those folders are watched, and the
//! directories of the work tree that hold a path git tracks, but none that
//! holds only what git ignores, as a `target` that a build writes in all
//! along. Where the work tree is, and which folders git keeps it in, git is
//! asked each time, as `git init` can make a directory of one a work tree of
//! its own, and git can come to keep one in other folders. A directory is
//! asked about again as its work tree changes, once git has rested from it
//! for as long as it took over it, and once git has rested from it for a
//! sweep besides, for what changes with no word to a watched folder: a file
//! in a directory new to git, or a work tree across the network. One
//! directory at a time, and one git has said nothing of yet first.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::git::{self, WorkTree};
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::core;
use crate::program::{self, Ran};
use crate::watch::{self, Troubles};

/// How long git has to say how a work tree stands. A large one takes it a
/// while, and one across the network longer; one that takes more than a
/// minute is stuck.
const LIMIT: Duration = Duration::from_mins(1);

/// The most directories of a work tree that are watched. Past it, only the
/// folders git keeps it in are, and what changes in the work tree itself waits
/// for the sweep: each takes a watch of the kernel's, which it lets a user have
/// only so many of, 8192 in Linux before 5.11.
const TREE_DIRS_MAX: usize = 4096;

// Each count goes up by two at the most for a line of what git says, and a
// line has more than two bytes: no count passes the most git is read for.
const _: () = assert!(program::SAID_MAX <= u32::MAX as u64);

/// The variables that tell git where a repository is, in the place of the
/// directory it runs in.
const REPOSITORY_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

/// Has `command`, which runs git, or a program that does, find the repository
/// by the directory it runs in. The variables that would have it read some
/// other repository, as a git hook that started the daemon would pass them
/// on, are left out.
pub fn local(command: &mut Command) -> &mut Command {
    for repository in REPOSITORY_VARS {
        command.env_remove(repository);
    }
    command
}

/// What git says of the work tree in `dir`: how it stands, or that `dir` is
/// outside one, where git fails. When git did not say, as it did not answer
/// in time, what is to be said of it instead.
fn status(dir: &str) -> Result<WorkTree, String> {
    let [program, args @ ..] = git::command(dir);
    let mut git = Command::new(program);
    local(git.args(args));
    let ran = program::run(&mut git, LIMIT)
        .map_err(|error| format!("git status in {dir} could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            WorkTree::parse(&String::from_utf8_lossy(&stdout))
                .map_err(|why| format!("git status in {dir} said {why}"))
        },
        // git ran there and failed: the directory is in no repository, or in
        // one git cannot read.
        Ran::Ended { .. } => Ok(WorkTree::Outside),
        Ran::Stuck => Err(format!("git status in {dir} did not answer in time")),
    }
}

/// The repository the work tree in `dir` is of, by its common directory,
/// which every work tree of a repository shares. None where git does not say
/// one, as of a directory in no repository. `safe.bareRepository` is explicit
/// as it is for `git status`, so that git finds the repository `status` does.
pub fn repository(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]))
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let said = program::run(&mut git, LIMIT).ok()?.answer().ok()?;
    let common = String::from_utf8(said).ok()?;
    Some(common.strip_suffix('\n')?.to_owned()).filter(|common| !common.is_empty())
}

/// The branch checked out in `dir` now, named as `git status` names it: its
/// ref without `refs/heads/`, which `--short` would keep a part of for a name
/// a tag has too. None for a detached `HEAD`, or where git does not say.
pub fn branch(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]))
        .args(["symbolic-ref", "--quiet", "HEAD"]);
    let said = program::run(&mut git, LIMIT).ok()?.answer().ok()?;
    let head = String::from_utf8(said).ok()?;
    Some(head.strip_suffix('\n')?.strip_prefix("refs/heads/")?.to_owned())
}

/// Where a work tree is, and the folders git keeps it in, each as the
/// filesystem names it, as a watcher names what changes in it.
#[derive(PartialEq, Eq)]
struct Place {
    /// The top of the work tree.
    top: PathBuf,
    /// The work tree's own folder: its `HEAD` and its index.
    own: PathBuf,
    /// The repository's folder, which each of its work trees shares: its
    /// branches, its stashes and its config.
    common: PathBuf,
}

/// Where the work tree that `dir` is in is, or none where git does not say,
/// as of a directory in no repository, or in a repository's own folder.
fn place(dir: &str) -> Option<Place> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir])).args([
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ]);
    let said = program::run(&mut git, LIMIT).ok()?.answer().ok()?;
    // Each path as the bytes it is, as a folder of git's needs to be UTF-8
    // no more than a directory does. One with a newline in it makes more
    // lines than three.
    let lines = said.strip_suffix(b"\n")?.split(|byte| *byte == b'\n');
    let [top, own, common]: [&[u8]; 3] = lines.collect::<Vec<_>>().try_into().ok()?;
    let real = |path: &[u8]| fs::canonicalize(OsStr::from_bytes(path)).ok();
    Some(Place { top: real(top)?, own: real(own)?, common: real(common)? })
}

/// What git tracks in the work tree at `top`, as `git ls-files -z` says it:
/// each path, after a NUL. git reads the index for it, and `core.fsmonitor`
/// is off as it is for `git status`, or a repository's own config could name
/// a command for each read to run.
fn listed(top: &Path) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    git.args(["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit", "-C"]).arg(top);
    local(&mut git).args(["ls-files", "-z"]);
    let ran = program::run(&mut git, LIMIT).map_err(|error| {
        format!("git ls-files in {} could not be run: {error}", top.display())
    })?;
    ran.answer().map_err(|why| format!("git ls-files in {}: {why}", top.display()))
}

/// The directories of the work tree at `top` that hold a path git tracks,
/// `top` among them, as `listed` names those paths, as `git ls-files -z` does.
/// One that holds none, as one that holds only what git ignores, is not among
/// them. None for more than `TREE_DIRS_MAX`.
fn tracked(top: &Path, listed: &[u8]) -> Option<BTreeSet<PathBuf>> {
    let mut dirs = BTreeSet::from([top.to_owned()]);
    for path in listed.split(|byte| *byte == 0) {
        let path = Path::new(OsStr::from_bytes(path));
        // The directory each is in, and those it is in, up to the first among
        // them already, which those it is in are too.
        for dir in path.ancestors().skip(1).take_while(|dir| !dir.as_os_str().is_empty()) {
            if !dirs.insert(top.join(dir)) {
                break;
            }
        }
        if dirs.len() > TREE_DIRS_MAX {
            return None;
        }
    }
    Some(dirs)
}

/// What is watched of the work tree at `place`, with `tracked` the
/// directories of it that hold a path git tracks: git's folders, for the
/// work tree's `HEAD` and index, and the repository's packed refs and config;
/// the repository's refs, each in a file under `refs`, the stashes' among
/// them, or in the tables in `reftable`, where the work tree keeps its `HEAD`
/// too; the directories; and a `.git` folder in any of them, as `git init`
/// makes one there. Only those there now.
fn watched(place: &Place, tracked: &BTreeSet<PathBuf>) -> BTreeMap<PathBuf, RecursiveMode> {
    use RecursiveMode::{NonRecursive, Recursive};
    let folders = [&place.own, &place.common].map(|folder| (folder.clone(), NonRecursive));
    let refs = place.common.join("refs");
    let refs = [
        // Where the refs really are: a watcher follows no link under them,
        // and they can be one themselves, into another repository's folder.
        (fs::canonicalize(&refs).unwrap_or(refs), Recursive),
        (place.common.join("reftable"), NonRecursive),
        (place.own.join("reftable"), NonRecursive),
    ];
    let dirs = tracked.iter().map(|dir| (dir.clone(), NonRecursive));
    // git writes the folder of a work tree that `git init` makes in a
    // directory of this one in pieces, and asked about before the folder is
    // whole, the directory is still in this work tree. Nothing else changes
    // as git makes it whole.
    let made = tracked.iter().map(|dir| (dir.join(".git"), NonRecursive));
    let all = folders.into_iter().chain(refs).chain(dirs).chain(made);
    all.filter(|(path, _)| path.is_dir()).collect()
}

/// The index at `path` as it stands: the file it is, its size, and when it
/// was last written. None where there is none, as before a first `git add`.
/// git writes the index anew and moves it into the place of the one before,
/// so the file tells one write from the next where two of the same size are
/// written in the same tick of the clock.
fn stamp(path: &Path) -> Option<(u64, u64, SystemTime)> {
    let index = fs::metadata(path).ok()?;
    Some((index.ino(), index.len(), index.modified().ok()?))
}

/// What a word from the watcher of a work tree tells.
enum Word {
    /// Nothing: a read changes nothing, git's own among them.
    Read,
    /// That something changed that git could find.
    Change,
    /// That a directory could have come or gone: one made or taken away, one
    /// moved in or out, or words the kernel dropped, which could have been
    /// any of those. A watched directory that goes, or moves away, takes its
    /// watch with it, and one that comes in its place is not watched.
    Dirs,
}

/// What `event` tells, from a watcher of `watched`. One that went wrong could
/// have missed anything, as a read is how the daemon finds out what it missed.
fn word(event: &notify::Result<Event>, watched: &BTreeMap<PathBuf, RecursiveMode>) -> Word {
    let Ok(event) = event else {
        return Word::Dirs;
    };
    match event.kind {
        _ if event.need_rescan() => Word::Dirs,
        kind if kind.is_access() => Word::Read,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => Word::Dirs,
        // A file moves all along, as an editor saves one, or git writes its
        // index or a ref: only a directory that moves counts.
        EventKind::Modify(ModifyKind::Name(_))
            if event.paths.iter().any(|path| watched.contains_key(path) || path.is_dir()) =>
        {
            Word::Dirs
        },
        _ => Word::Change,
    }
}

/// A watcher of `paths` that sets `heard`, and wakes the thread on `wake`, as
/// anything changes there that a read could find, and sets `came_or_went` too
/// as a directory could have come or gone.
fn watcher(
    paths: &BTreeMap<PathBuf, RecursiveMode>,
    heard: Arc<AtomicBool>,
    came_or_went: Arc<AtomicBool>,
    wake: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let watched_paths = paths.clone();
    let handler = move |event: notify::Result<Event>| {
        match word(&event, &watched_paths) {
            Word::Read => return,
            Word::Dirs => came_or_went.store(true, Ordering::SeqCst),
            Word::Change => {},
        }
        heard.store(true, Ordering::SeqCst);
        match wake.try_send(()) {
            // A full channel holds a wake-up already, and one that no one
            // hears is the thread's that has ended.
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
        }
    };
    // No link is followed under what is watched with all under it: one among
    // the refs could lead anywhere, as to `/`, and each directory there would
    // take a watch of the kernel's.
    let config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = RecommendedWatcher::new(handler, config)?;
    let mut adding = watcher.paths_mut();
    for (path, mode) in paths {
        match adding.add(path, *mode) {
            Ok(()) => {},
            // One that went since it was found.
            Err(error) if watch::missing(&error) => {},
            Err(error) => return Err(error),
        }
    }
    adding.commit()?;
    Ok(watcher)
}

/// A work tree that a pane works in, as it is watched.
#[derive(Default)]
struct Tree {
    /// Set as the work tree changes, and taken as the directories in it are
    /// due again.
    heard: Arc<AtomicBool>,
    /// Set as a directory could have come or gone, so that the work tree is
    /// watched anew.
    came_or_went: Arc<AtomicBool>,
    /// None before the work tree is watched.
    watched: Option<Watched>,
}

/// How a work tree is watched.
struct Watched {
    /// Where git kept the work tree when it began to be watched. git can come
    /// to keep one in other folders, as a submodule's are absorbed into its
    /// superproject's.
    place: Place,
    /// The index as it stood when the work tree began to be watched. A
    /// checkout can bring directories that the index did not have then.
    index: Option<(u64, u64, SystemTime)>,
    watching: Watching,
}

/// What of a work tree is watched, with the watcher held for as long as it
/// is watched so.
enum Watching {
    /// git's folders, and each directory of the work tree that holds a path
    /// git tracks.
    Whole { _watcher: RecommendedWatcher },
    /// git's folders alone, and why not the directories.
    Folders { _watcher: RecommendedWatcher, why: String },
    /// Nothing, which leaves the work tree to the sweep until its index
    /// changes, and why.
    Sweep { why: String },
}

impl Watching {
    /// Why the work tree is not watched whole, where it is not.
    fn trouble(&self) -> Option<&String> {
        match self {
            Watching::Whole { .. } => None,
            Watching::Folders { why, .. } | Watching::Sweep { why } => Some(why),
        }
    }
}

impl Tree {
    /// Watches the work tree, which git keeps at `place`, and wakes the
    /// thread on `wake` as it changes: anew where git kept it elsewhere when
    /// it began to be watched, or its index changed since, or a directory
    /// could have come or gone. What goes wrong, `troubles` says.
    fn watch(&mut self, place: Place, wake: &SyncSender<()>, troubles: &mut Troubles) {
        let index = stamp(&place.own.join("index"));
        let moved = self.came_or_went.swap(false, Ordering::SeqCst);
        let same = |watched: &Watched| watched.place == place && watched.index == index;
        if !moved && self.watched.as_ref().is_some_and(same) {
            return;
        }
        // The watcher before lets go of its watches first.
        self.watched = None;
        let top = place.top.display();
        let tracked = listed(&place.top).and_then(|listed| {
            let many = || {
                format!(
                    "{top} has more than {TREE_DIRS_MAX} directories that git tracks a path \
                     in, and only git's folders are watched"
                )
            };
            tracked(&place.top, &listed).ok_or_else(many)
        });
        let (tracked, why) = match tracked {
            Ok(tracked) => (tracked, None),
            Err(why) => (BTreeSet::new(), Some(why)),
        };
        let (heard, moved) = (self.heard.clone(), self.came_or_went.clone());
        let watcher = watcher(&watched(&place, &tracked), heard, moved, wake.clone());
        let watching = match (watcher, why) {
            (Ok(watcher), None) => Watching::Whole { _watcher: watcher },
            (Ok(watcher), Some(why)) => Watching::Folders { _watcher: watcher, why },
            (Err(error), _) => {
                Watching::Sweep { why: format!("{top} is read on the sweep alone: {error}") }
            },
        };
        if let Some(trouble) = watching.trouble() {
            troubles.say(trouble.clone());
        }
        self.watched = Some(Watched { place, index, watching });
    }
}

/// Where git stands with a directory it has been asked about.
struct Asked {
    /// When git was done with it last.
    ended: Instant,
    /// How long git took over it then.
    took: Duration,
    /// Whether its work tree changed since.
    changed: bool,
    /// The top of the work tree git placed it in then, if any.
    top: Option<PathBuf>,
    /// What went wrong then, if anything did.
    trouble: Option<String>,
}

impl Asked {
    /// When it is due again: on a word that its work tree changed, once git
    /// has rested from it for as long as it took over it, so that a directory
    /// of a work tree that changes all along takes up half of git's time at
    /// the most; with no word, once git has rested from it for a `sweep`, or
    /// for as long as it took if that is longer.
    fn due(&self, sweep: Duration) -> Instant {
        self.ended + if self.changed { self.took } else { self.took.max(sweep) }
    }

    /// Whether git placed it in the work tree with this top.
    fn within(&self, top: &Path) -> bool {
        self.top.as_deref() == Some(top)
    }
}

/// Asks git where the work tree that `dir` is in is, and how it stands. The
/// work tree is watched first, so that a change while git reads it is heard.
/// The top of the work tree, which `trees` has, where `dir` is in one, and
/// what git said.
fn ask(
    dir: &str,
    trees: &mut HashMap<PathBuf, Tree>,
    wake: &SyncSender<()>,
    troubles: &mut Troubles,
) -> (Option<PathBuf>, Result<WorkTree, String>) {
    let top = place(dir).map(|place| {
        let top = place.top.clone();
        trees.entry(top.clone()).or_default().watch(place, wake, troubles);
        top
    });
    (top, status(dir))
}

/// Asks git how the work tree stands in each directory a pane works in, in a
/// thread of its own, and tells the core: about each as its work tree changes,
/// once git has rested from it for as long as it took over it, and once it
/// has rested for a `sweep`, or as long as it took if that is longer, with no
/// word of a change; and about a directory it has said nothing of yet as soon
/// as it is `woken`, before those left. The core wakes it on `woken`, and so
/// does a work tree that changes, which it watches with `wake`. What git said
/// of a directory it could not answer for stands until it can. The thread
/// ends once the core has.
pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("git".to_owned()).spawn(move || {
        let mut asked: HashMap<String, Asked> = HashMap::new();
        let mut trees: HashMap<PathBuf, Tree> = HashMap::new();
        let mut troubles = Troubles::default();
        'sweep: loop {
            let Some(dirs) = core::ask(&core, core::Command::Dirs) else {
                return;
            };
            asked.retain(|dir, _| dirs.contains(dir));
            trees.retain(|top, _| asked.values().any(|was| was.within(top)));
            for (top, tree) in &trees {
                if tree.heard.swap(false, Ordering::SeqCst) {
                    let within = asked.values_mut().filter(|was| was.within(top));
                    within.for_each(|was| was.changed = true);
                }
            }
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|was| was.due(sweep) <= now);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|dir| asked.get(dir).map(|was| was.due(sweep)));
            for dir in due {
                let started = Instant::now();
                let (top, said) = ask(&dir, &mut trees, &wake, &mut troubles);
                let (took, ended) = (started.elapsed(), Instant::now());
                let trouble = said.as_ref().err().cloned();
                asked.insert(dir.clone(), Asked { ended, took, changed: false, top, trouble });
                match said {
                    Ok(tree) => {
                        if core.send(core::Command::WorkTree { dir, tree }).is_err() {
                            return;
                        }
                    },
                    Err(trouble) => troubles.say(trouble),
                }
                // Not let settle, as it is below: with a work tree that
                // changes all along, git would ask about one directory each
                // time it settles, and the others would wait.
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            // What went wrong with one that git rests from has not gone right
            // since, and is not said again.
            let asking = asked.values().filter_map(|was| was.trouble.clone());
            let watched = trees.values().filter_map(|tree| tree.watched.as_ref());
            let watching = watched.filter_map(|watched| watched.watching.trouble().cloned());
            for trouble in asking.chain(watching) {
                troubles.say(trouble);
            }
            troubles.done();
            let now = Instant::now();
            let next = asked.values().map(|was| was.due(sweep).saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) => watch::settle(&woken),
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};

    use super::stamp;

    /// git writes its index anew, and moves it into the place of the one
    /// before. One written so with the size and the mtime of the one before
    /// is stamped anew all the same.
    #[test]
    fn an_index_written_anew_is_stamped_anew() {
        let dir = tempfile::tempdir().unwrap();
        let (index, lock) = (dir.path().join("index"), dir.path().join("index.lock"));
        fs::write(&index, "before").unwrap();
        let modified = fs::metadata(&index).unwrap().modified().unwrap();
        let before = stamp(&index);
        fs::write(&lock, "after!").unwrap();
        File::options().write(true).open(&lock).unwrap().set_modified(modified).unwrap();
        fs::rename(&lock, &index).unwrap();
        assert_ne!(stamp(&index), before);
    }
}
