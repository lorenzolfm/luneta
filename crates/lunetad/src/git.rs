//! How the work tree stands in each directory a pane works in, as `git status`
//! says it.
//!
//! git tells no one as a work tree changes, but each change is to a file: in
//! the folders git keeps the work tree in, as a commit, a checkout, a stage, a
//! stash or a fetch writes there, or in a directory of the work tree, as a
//! file is written, made or taken away. Those folders are watched, and the
//! directories of the work tree that hold a path git tracks, but none that
//! holds only what git ignores, as a `target` that a build writes in all
//! along. A directory is asked about again as its work tree changes, once git
//! has rested from it for as long as it took over it, and once git has rested
//! from it for a sweep besides, for what changes with no word to a watched
//! folder: a file in a directory new to git, or a work tree across the
//! network. One directory at a time, and one git has said nothing of yet
//! first.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::git::{self, WorkTree};
use notify::event::{CreateKind, RemoveKind};
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
    let said = String::from_utf8(said).ok()?;
    // A path with a newline in it makes more lines than three.
    let [top, own, common]: [&str; 3] = said.lines().collect::<Vec<_>>().try_into().ok()?;
    let real = |path: &str| fs::canonicalize(path).ok();
    Some(Place { top: real(top)?, own: real(own)?, common: real(common)? })
}

/// What git tracks in the work tree at `top`, as `git ls-files -z` says it:
/// each path, after a NUL.
fn listed(top: &Path) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C"]).arg(top)).args(["ls-files", "-z"]);
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
/// too; and the directories. Only those there now.
fn watched(place: &Place, tracked: &BTreeSet<PathBuf>) -> BTreeMap<PathBuf, RecursiveMode> {
    use RecursiveMode::{NonRecursive, Recursive};
    let folders = [&place.own, &place.common].map(|folder| (folder.clone(), NonRecursive));
    let refs = [
        (place.common.join("refs"), Recursive),
        (place.common.join("reftable"), NonRecursive),
        (place.own.join("reftable"), NonRecursive),
    ];
    let dirs = tracked.iter().map(|dir| (dir.clone(), NonRecursive));
    folders.into_iter().chain(refs).chain(dirs).filter(|(path, _)| path.is_dir()).collect()
}

/// The index at `path` as it stands: its size, and when it was last written.
/// None where there is none, as before a first `git add`.
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let index = fs::metadata(path).ok()?;
    Some((index.len(), index.modified().ok()?))
}

/// A watcher of `paths` that sets `heard`, and wakes the thread on `wake`, as
/// anything changes there that a read could find, and sets `came_or_went` too
/// as a directory does. One that goes wrong has heard a change, as a read is
/// how the daemon finds out what it missed.
fn watcher(
    paths: &BTreeMap<PathBuf, RecursiveMode>,
    heard: Arc<AtomicBool>,
    came_or_went: Arc<AtomicBool>,
    wake: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
        if let Ok(event) = &event {
            // A read changes nothing, git's own among them.
            if event.kind.is_access() {
                return;
            }
            if matches!(
                event.kind,
                EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
            ) {
                came_or_went.store(true, Ordering::SeqCst);
            }
        }
        heard.store(true, Ordering::SeqCst);
        match wake.try_send(()) {
            // A full channel holds a wake-up already, and one that no one
            // hears is the thread's that has ended.
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
        }
    })?;
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
struct Tree {
    place: Place,
    /// Set as the work tree changes, and taken as the directories in it are
    /// due again.
    heard: Arc<AtomicBool>,
    /// Set as a directory comes or goes. A watched one that goes takes its
    /// watch with it, and one made in its place, even much later, is not
    /// watched, so the work tree is watched anew.
    came_or_went: Arc<AtomicBool>,
    /// None before the work tree is watched.
    watched: Option<Watched>,
    /// What went wrong as it was watched last, if anything did.
    trouble: Option<String>,
}

/// How a work tree is watched.
struct Watched {
    /// The index as it stood when the work tree began to be watched. A
    /// checkout can bring directories that the index did not have then.
    index: Option<(u64, SystemTime)>,
    /// Held for as long as the work tree is watched so. None where it could
    /// not be, which leaves it to the sweep until its index changes.
    _watcher: Option<RecommendedWatcher>,
}

impl Tree {
    fn new(place: Place) -> Self {
        Tree {
            place,
            heard: Arc::new(AtomicBool::new(false)),
            came_or_went: Arc::new(AtomicBool::new(false)),
            watched: None,
            trouble: None,
        }
    }

    /// Watches the work tree, and wakes the thread on `wake` as it changes:
    /// anew where its index changed since it began to be watched, or a
    /// directory came or went. What goes wrong, `troubles` says.
    fn watch(&mut self, wake: &SyncSender<()>, troubles: &mut Troubles) {
        let index = stamp(&self.place.own.join("index"));
        let moved = self.came_or_went.swap(false, Ordering::SeqCst);
        if !moved && self.watched.as_ref().is_some_and(|watched| watched.index == index) {
            return;
        }
        // The watcher before lets go of its watches first.
        self.watched = None;
        let top = self.place.top.display();
        let tracked = listed(&self.place.top).and_then(|listed| {
            let many = || {
                format!(
                    "{top} has more than {TREE_DIRS_MAX} directories that git tracks a path \
                     in, and only git's folders are watched"
                )
            };
            tracked(&self.place.top, &listed).ok_or_else(many)
        });
        let (tracked, mut trouble) = match tracked {
            Ok(tracked) => (tracked, None),
            Err(trouble) => (BTreeSet::new(), Some(trouble)),
        };
        let (heard, moved) = (self.heard.clone(), self.came_or_went.clone());
        let watcher = watcher(&watched(&self.place, &tracked), heard, moved, wake.clone());
        let alone = |error: &notify::Error| format!("{top} is read on the sweep alone: {error}");
        let watcher = watcher.inspect_err(|error| trouble = Some(alone(error))).ok();
        if let Some(trouble) = &trouble {
            troubles.say(trouble.clone());
        }
        self.trouble = trouble;
        self.watched = Some(Watched { index, _watcher: watcher });
    }
}

/// Where a directory is, as git said it last.
enum Within {
    /// The work tree with this top.
    Tree(PathBuf),
    /// No work tree.
    Nothing,
    /// git has not said.
    Unsaid,
}

impl Within {
    fn is(&self, top: &Path) -> bool {
        matches!(self, Within::Tree(it) if it == top)
    }
}

/// Where git stands with a directory it has been asked about.
struct Asked {
    /// When it is due again with no word that its work tree changed: once
    /// git has rested from it for a sweep, or for as long as it took over it
    /// if that is longer.
    next: Instant,
    /// When it is due again on a word that its work tree changed: once git
    /// has rested from it for as long as it took over it, so that a work tree
    /// that changes all along takes up half of git's time at the most.
    rested: Instant,
    /// Whether its work tree changed since it was asked about.
    changed: bool,
    within: Within,
    /// What went wrong the last time it was asked about, if anything did.
    trouble: Option<String>,
}

impl Asked {
    fn due(&self) -> Instant {
        if self.changed { self.rested } else { self.next }
    }
}

/// Asks git how the work tree in `dir` stands, as git said last that it is
/// `within` one, where it was asked before. The work tree is watched first,
/// so that a change while git reads it is heard. What git said, and where
/// `dir` is now: in a work tree that `trees` has, where it is in one.
fn ask(
    dir: &str,
    within: Option<&Within>,
    trees: &mut HashMap<PathBuf, Tree>,
    wake: &SyncSender<()>,
    troubles: &mut Troubles,
) -> (Within, Result<WorkTree, String>) {
    let top = match within {
        Some(Within::Tree(top)) if trees.contains_key(top) => Some(top.clone()),
        Some(Within::Nothing) => None,
        Some(Within::Tree(_) | Within::Unsaid) | None => place(dir).map(|place| {
            let top = place.top.clone();
            trees.entry(top.clone()).or_insert_with(|| Tree::new(place));
            top
        }),
    };
    if let Some(tree) = top.as_ref().and_then(|top| trees.get_mut(top)) {
        tree.watch(wake, troubles);
    }
    let said = status(dir);
    let within = match (&said, top) {
        (Ok(WorkTree::Outside), _) => Within::Nothing,
        (_, Some(top)) => Within::Tree(top),
        (_, None) => Within::Unsaid,
    };
    (within, said)
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
            trees.retain(|top, _| asked.values().any(|was| was.within.is(top)));
            for (top, tree) in &trees {
                if tree.heard.swap(false, Ordering::SeqCst) {
                    let within = asked.values_mut().filter(|was| was.within.is(top));
                    within.for_each(|was| was.changed = true);
                }
            }
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|was| was.due() <= now);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|dir| asked.get(dir).map(Asked::due));
            for dir in due {
                let started = Instant::now();
                let within = asked.get(&dir).map(|was| &was.within);
                let (within, said) = ask(&dir, within, &mut trees, &wake, &mut troubles);
                let (took, now) = (started.elapsed(), Instant::now());
                let trouble = said.as_ref().err().cloned();
                let next = now + took.max(sweep);
                let was = Asked { next, rested: now + took, changed: false, within, trouble };
                asked.insert(dir.clone(), was);
                match said {
                    Ok(tree) => {
                        if core.send(core::Command::WorkTree { dir, tree }).is_err() {
                            return;
                        }
                    },
                    Err(trouble) => troubles.say(trouble),
                }
                if woken.try_recv().is_ok() {
                    watch::settle(&woken);
                    continue 'sweep;
                }
            }
            // What went wrong with one that git rests from has not gone right
            // since, and is not said again.
            let asking = asked.values().filter_map(|was| was.trouble.clone());
            let watching = trees.values().filter_map(|tree| tree.trouble.clone());
            for trouble in asking.chain(watching) {
                troubles.say(trouble);
            }
            troubles.done();
            let now = Instant::now();
            let next = asked.values().map(|was| was.due().saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) => watch::settle(&woken),
                Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}
