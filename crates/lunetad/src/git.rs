//! How the work tree stands in each directory a pane works in, as `git status`
//! says it.
//!
//! git tells no one as a work tree changes, but each change is to a file: in
//! the folders git keeps the work tree in, as a commit, a checkout, a stage, a
//! stash or a fetch writes there, or in a directory of the work tree, as a
//! file is written, made or taken away. Those folders are watched, and the
//! directories of the work tree that hold a path git tracks, but none that
//! holds only what git ignores, as a `target` that a build writes in all
//! along. A directory in no work tree is watched for a `.git` to come in it,
//! as `git init` or `git clone` makes one, and watched anew as it is made
//! again. Where the work tree is, and which folders git keeps it in, git is
//! asked each time, as `git init` can make a directory of one a work tree of
//! its own, and git can come to keep one in other folders. A directory is
//! asked about again as its work tree changes, once git has rested from it
//! for as long as it took over it, and once git has rested from it for a
//! sweep besides, for what changes with no word to a watched folder: a file
//! in a directory new to git; a work tree across the network; or one whose
//! `.git` is a file, as `git worktree add` makes, which git writes before the
//! folder it names is whole, where git is asked about it in between. One
//! directory at a time, and one git has said nothing of yet first.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::fmt;
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
use luneta_protocol::said::Said;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::core;
use crate::program::{self, Ran};
use crate::watch::{self, Troubles};

/// How long git has to say how a work tree stands. A large one takes it a
/// while, and one across the network longer; one that takes more than a
/// minute is stuck.
const LIMIT: Duration = Duration::from_mins(1);

/// The most directories of a work tree that hold a path git tracks that are
/// watched, each with a `.git` in it too where there is one, as a work tree
/// made in it has. Past it, only the folders git keeps it in are, and what
/// changes in the work tree itself waits for the sweep: each takes a watch of
/// the kernel's, which it lets a user have only so many of, 8192 in Linux
/// before 5.11.
const TREE_DIRS_MAX: usize = 4096;

// Each count goes up by two at the most for a line of what git says, and a
// line has more than two bytes: no count passes the most git is read for.
const _: () = assert!(program::SAID_MAX <= u32::MAX as u64);

/// The variables that tell git where a repository is, in the place of the
/// directory it runs in.
pub const REPOSITORY_VARS: [&str; 6] = [
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

/// What git says of the work tree in `dir`: how it stands, that `dir` is in a
/// repository and in none of its work trees, as git `placed` it, or that it is
/// in no repository. When git did not say, as it did not answer in time, or
/// would not read the repository or the work tree, what is to be said of it
/// instead.
fn status(dir: &str, placed: &Placed, common: Option<&Path>) -> Result<WorkTree, String> {
    let [program, args @ ..] = git::command(dir);
    let mut git = Command::new(program);
    local(git.args(args));
    let ran = program::run(&mut git, LIMIT)
        .map_err(|error| format!("git status in {dir} could not be run: {error}"))?;
    let said = match ran {
        Ran::Stuck => return Err(format!("git status in {dir} did not answer in time")),
        ended @ Ran::Ended { .. } => ended.answer(),
    };
    let told = common.and_then(Path::to_str).filter(|_| said.is_ok());
    let repository = match told {
        Some(common) => Some(holder_said(common)?),
        None => holder_of(dir)?,
    };
    let Some(repository) = repository else {
        return Ok(WorkTree::Outside);
    };
    match (said, placed) {
        (Ok(stdout), _) => WorkTree::parse(&String::from_utf8_lossy(&stdout), repository)
            .map_err(|why| format!("git status in {dir} said {why}")),
        (Err(_), Placed::Outside(_)) => Ok(WorkTree::NoCheckout { repository }),
        (Err(why), Placed::In(_) | Placed::Unsaid) => {
            Err(format!("git status in {dir} said {why}"))
        },
    }
}

/// The directory that holds the repository `dir` is in, or None where it is in
/// none, or is gone. Why not, where git did not say.
fn holder_of(dir: &str) -> Result<Option<Said>, String> {
    match common(dir) {
        Common::Of(common) => holder_said(&common).map(Some),
        Common::NoRepository => Ok(None),
        Common::Unsaid(why) if directory_gone(why.as_bytes()) => Ok(None),
        Common::Unsaid(why) => Err(format!("git did not say what repository {dir} is in: {why}")),
    }
}

fn holder_said(common: &str) -> Result<Said, String> {
    holder(common)
        .and_then(Said::new)
        .ok_or_else(|| format!("no directory holds the repository {common}"))
}

/// What git starts with, in its own language, where a directory is in no
/// repository: `(or any of the parent directories)`, or `(or any parent up to
/// mount point /x)`. Nothing else tells that from a repository it refuses: it
/// ends with one status for all of them.
const IN_NO_REPOSITORY: &str = "fatal: not a git repository (or any ";

/// What git says of the repository a directory is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Common {
    /// It is in one: the directory all its worktrees have in common, by its
    /// real path, which tells that repository from another.
    Of(String),
    /// It is in none.
    NoRepository,
    /// git did not say, for this reason, as it refused the repository it found,
    /// one of a newer format, or of another user, or it did not answer in time.
    /// That says nothing of whether the directory is in one.
    Unsaid(String),
}

impl Common {
    /// What git's `answer` says of it: what it said, where it ended well, or
    /// else why not.
    #[must_use]
    pub fn of(answer: Result<Vec<u8>, String>) -> Common {
        match answer.map(one_line) {
            Ok(Some(common)) => Common::Of(common),
            Ok(None) => Common::Unsaid("it named no directory".to_owned()),
            Err(why) if why.starts_with(IN_NO_REPOSITORY) => Common::NoRepository,
            Err(why) => Common::Unsaid(why),
        }
    }
}

/// What git says of the repository `dir` is in. `safe.bareRepository` is
/// explicit as it is for `git status`, so that git finds the repository
/// `status` does.
#[must_use]
pub fn common(dir: &str) -> Common {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]))
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env("LC_ALL", "C");
    let ran = program::run(&mut git, LIMIT);
    Common::of(ran.map_err(|error| format!("it could not be run: {error}")).and_then(Ran::answer))
}

/// Whether `stderr` is git saying, in its own language, that a directory it was
/// told to work in is gone, as a pane's can be. One it cannot go into for
/// another reason, as a mount gone stale, may be there all the same.
#[must_use]
pub fn directory_gone(stderr: &[u8]) -> bool {
    stderr.starts_with(b"fatal: cannot change to ")
        && stderr.trim_ascii_end().ends_with(b": No such file or directory")
}

/// `said`, where it is a line of text.
#[must_use]
pub fn one_line(said: Vec<u8>) -> Option<String> {
    let said = String::from_utf8(said).ok()?;
    Some(said.strip_suffix('\n')?.to_owned()).filter(|said| !said.is_empty())
}

/// The directory that holds the repository whose common directory is `common`,
/// as wt keeps its worktrees: beside a hidden `.bare`, or a clone's `.git`, or in
/// it, where it is not hidden.
pub fn holder(common: &str) -> Option<String> {
    let common = Path::new(common);
    let hidden = common.file_name()?.to_str()?.starts_with('.');
    let holder = if hidden { common.parent()? } else { common };
    holder.to_str().map(str::to_owned)
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

pub enum NamedHead {
    OnRemote,
    PullRef { remote_url: String },
}

pub fn named_head(dir: &str, branch: &str) -> Result<Option<NamedHead>, String> {
    let head = format!("refs/heads/{branch}");
    let push = says(dir, &["for-each-ref", "--format=%(push:remotename)", &head])?;
    if push.is_empty() {
        return Ok(None);
    }
    let config = |name: &str| {
        says(dir, &["config", "--default", "", "--get", &format!("branch.{branch}.{name}")])
    };
    let (merge, remote) = (config("merge")?, config("remote")?);
    if merge.starts_with("refs/pull/") {
        if remote.is_empty() {
            return Ok(None);
        }
        let remote_url = says(dir, &["ls-remote", "--get-url", "--", &remote])?;
        return Ok(Some(NamedHead::PullRef { remote_url }));
    }
    Ok((push != remote || merge == head).then_some(NamedHead::OnRemote))
}

fn says(dir: &str, args: &[&str]) -> Result<String, String> {
    let asked = args.join(" ");
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]).args(args));
    let ran = program::run(&mut git, LIMIT)
        .map_err(|error| format!("git {asked} in {dir} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("git {asked} in {dir}: {why}"))?;
    Ok(String::from_utf8_lossy(&said).trim_end().to_owned())
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
/// Unanswered where git said nothing at all.
fn place(dir: &str) -> Result<Option<Place>, Unanswered> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir])).args([
        "rev-parse",
        "--path-format=absolute",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ]);
    match program::run(&mut git, LIMIT) {
        Ok(Ran::Ended { status, stdout, .. }) if status.success() => Ok(Place::parse(&stdout)),
        // git ran there and failed: the directory is in no work tree, or in
        // one git cannot read.
        Ok(Ran::Ended { .. }) => Ok(None),
        Ok(Ran::Stuck) | Err(_) => Err(Unanswered),
    }
}

/// That git said nothing of where a directory is: it did not answer in time,
/// as on a mount that does not answer, or it could not be run.
struct Unanswered;

impl Place {
    /// The place that `said`, as `place` has git say it: the top, the work
    /// tree's own folder and the repository's, a line each. None where it
    /// says something else, or one of them is not there.
    fn parse(said: &[u8]) -> Option<Place> {
        // Each path as the bytes it is, as a folder of git's needs to be UTF-8
        // no more than a directory does. One with a newline in it makes more
        // lines than three.
        let lines = said.strip_suffix(b"\n")?.split(|byte| *byte == b'\n');
        let [top, own, common]: [&[u8]; 3] = lines.collect::<Vec<_>>().try_into().ok()?;
        let real = |path: &[u8]| fs::canonicalize(OsStr::from_bytes(path)).ok();
        Some(Place { top: real(top)?, own: real(own)?, common: real(common)? })
    }
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
/// directories of it that hold a path git tracks, where git listed them: git's
/// folders, for the work tree's `HEAD` and index, and the repository's packed
/// refs and config; the repository's refs, each in a file under `refs`, the
/// stashes' among them, or in the tables in `reftable`, where the work tree
/// keeps its `HEAD` too; the directories; and a `.git` folder in any of them,
/// as `git init` makes one there. Only those there now.
fn watched(place: &Place, tracked: Option<&BTreeSet<PathBuf>>) -> BTreeMap<PathBuf, RecursiveMode> {
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
    // git writes the folder of a work tree that `git init` makes in a
    // directory of this one in pieces, and asked about before the folder is
    // whole, the directory is still in this work tree. Nothing else changes
    // as git makes it whole.
    let dirs = tracked.into_iter().flatten();
    let dirs = dirs.flat_map(|dir| [(dir.clone(), NonRecursive), (dir.join(".git"), NonRecursive)]);
    let all = folders.into_iter().chain(refs).chain(dirs);
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

/// Whether `error` is of a path the user may not read.
fn denied(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::Io(error) => error.kind() == io::ErrorKind::PermissionDenied,
        _ => false,
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
        hear(&heard, &wake);
    };
    // No link is followed under what is watched with all under it: one among
    // the refs could lead anywhere, as to `/`, and each directory there would
    // take a watch of the kernel's.
    let config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = RecommendedWatcher::new(handler, config)?;
    add(&mut watcher, paths)?;
    Ok(watcher)
}

/// A watcher of the directory at `ground`, which git placed in no work tree,
/// and of the `.git` folder in it too where `ground` has one, that sets
/// `heard`, and wakes the thread on `wake`, as a `.git` comes in it, changes
/// or goes, and as git writes in that; and sets `went` too as the directory
/// could have gone or moved away, which takes its watch with it, or words the
/// kernel dropped, which could have been that.
fn beginning(
    ground: &Ground,
    heard: Arc<AtomicBool>,
    went: Arc<AtomicBool>,
    wake: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let (dir, git) = (ground.real.clone(), ground.real.join(".git"));
    // Each by where it really is, as a watcher names a path, so that git
    // writing in a directory that is a `.git` itself, as a repository's own
    // folder that a pane works in, wakes no one.
    let of_git = {
        let git = git.clone();
        move |path: &Path| path == git || path.parent() == Some(git.as_path())
    };
    let left = |kind: EventKind| {
        matches!(kind, EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)))
    };
    let itself = dir.clone();
    let handler = move |event: notify::Result<Event>| {
        let gone = match &event {
            Ok(event) => {
                event.need_rescan() || (left(event.kind) && event.paths.contains(&itself))
            },
            Err(_) => true,
        };
        if gone {
            went.store(true, Ordering::SeqCst);
        } else if event.is_ok_and(|event| !watch::worth_a_read(&event, &of_git)) {
            return;
        }
        hear(&heard, &wake);
    };
    let mut watcher = notify::recommended_watcher(handler)?;
    let mut paths = BTreeMap::from([(dir, RecursiveMode::NonRecursive)]);
    if ground.git {
        paths.insert(git, RecursiveMode::NonRecursive);
    }
    add(&mut watcher, &paths)?;
    Ok(watcher)
}

/// Has `watcher` watch each of `paths` as its mode says, but for any that
/// went since it was found.
fn add(
    watcher: &mut RecommendedWatcher,
    paths: &BTreeMap<PathBuf, RecursiveMode>,
) -> notify::Result<()> {
    let mut adding = watcher.paths_mut();
    for (path, mode) in paths {
        match adding.add(path, *mode) {
            Ok(()) => {},
            // One that went since it was found.
            Err(error) if watch::missing(&error) => {},
            // A `.git` folder that does not read, as one another user made:
            // git passes it by, and finds no work tree in it.
            Err(error) if path.ends_with(".git") && denied(&error) => {},
            Err(error) => return Err(error),
        }
    }
    adding.commit()
}

/// Sets `heard`, and wakes the thread on `wake`.
fn hear(heard: &AtomicBool, wake: &SyncSender<()>) {
    heard.store(true, Ordering::SeqCst);
    match wake.try_send(()) {
        // A full channel holds a wake-up already, and one that no one hears
        // is the thread's that has ended.
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

/// A work tree that a pane works in, as it is watched.
struct Tree {
    /// Set as the work tree changes, and taken as the directories in it are
    /// due again.
    heard: Arc<AtomicBool>,
    /// Set as a directory could have come or gone, so that the work tree is
    /// watched anew.
    came_or_went: Arc<AtomicBool>,
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
    /// The work tree that git keeps at `place`, watched, and whether anew:
    /// `before` as it is, where git kept it there when it began to be
    /// watched, its index has not changed since, and no directory could have
    /// come or gone; else watched anew, with what it heard before kept, and
    /// waking the thread on `wake` as it changes. What goes wrong, `troubles`
    /// says.
    fn watch(
        before: Option<Tree>,
        place: Place,
        wake: &SyncSender<()>,
        troubles: &mut Troubles,
    ) -> (Tree, bool) {
        let index = stamp(&place.own.join("index"));
        let (heard, came_or_went) = match before {
            Some(tree)
                if !tree.came_or_went.swap(false, Ordering::SeqCst)
                    && tree.place == place
                    && tree.index == index =>
            {
                return (tree, false);
            },
            // The watcher before lets go of its watches first.
            Some(Tree { heard, came_or_went, watching, .. }) => {
                drop(watching);
                (heard, came_or_went)
            },
            None => (Arc::default(), Arc::default()),
        };
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
        let paths = watched(&place, tracked.as_ref().ok());
        let watcher = watcher(&paths, heard.clone(), came_or_went.clone(), wake.clone());
        let watching = match (watcher, tracked) {
            (Ok(watcher), Ok(_)) => Watching::Whole { _watcher: watcher },
            (Ok(watcher), Err(why)) => Watching::Folders { _watcher: watcher, why },
            (Err(error), _) => {
                Watching::Sweep { why: format!("{top} is read on the sweep alone: {error}") }
            },
        };
        if let Some(trouble) = watching.trouble() {
            troubles.say(trouble.clone());
        }
        (Tree { heard, came_or_went, place, index, watching }, true)
    }
}

/// A directory that git placed in no work tree, as it is watched for one to
/// begin there: for a `.git` that `git init` or `git clone` makes in it, and
/// in that `.git` once it is there, as git makes the folder first and writes
/// the rest in it after, and asked about before the folder is whole, the
/// directory is still in none.
struct Outside {
    /// Set as the directory goes or moves away, or a `.git` comes in it or
    /// changes, and taken as the directory is due again.
    heard: Arc<AtomicBool>,
    watching: Beginning,
}

/// A directory that git placed in no work tree, as it was when it began to be
/// watched: where it really is, as a watcher names what changes in it, and
/// whether a `.git` folder is in it.
#[derive(PartialEq, Eq)]
struct Ground {
    real: PathBuf,
    git: bool,
}

impl Ground {
    /// `dir` as it is now, or none where there is no directory there. What
    /// else goes wrong in finding it, as a folder above it that does not
    /// read, the error says.
    fn of(dir: &Path) -> io::Result<Option<Ground>> {
        let real = match fs::canonicalize(dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            real => real?,
        };
        let git = real.join(".git").is_dir();
        Ok(Some(Ground { real, git }))
    }
}

/// What of a directory that git placed in no work tree is watched, with the
/// watcher held for as long as it is watched so.
enum Beginning {
    /// The directory as `ground` has it, and the `.git` folder in it where
    /// `ground` has one and it reads, until `went` is set: one made in the
    /// place of one that went can be the same file to the filesystem, with
    /// the inode of the one before, which no watch of that one sees.
    Watched { ground: Ground, went: Arc<AtomicBool>, _watcher: RecommendedWatcher },
    /// Nothing, as there is no directory there: one made there later waits
    /// for the sweep.
    Gone,
    /// Nothing, which leaves the directory to the sweep, and why: one whose
    /// watch went wrong, until it no longer stands as `ground` has it, as one
    /// made again would go wrong again; or, with no `ground`, one that was not
    /// found where it really is.
    Sweep { ground: Option<Ground>, why: String },
}

impl Beginning {
    /// Whether it is what is to be watched of the directory as `now` has it:
    /// not where the directory went, moved away or was made anew, a `.git`
    /// came in it or went from it since, or it was not found; and where it
    /// was left to the sweep, only while it stands as it did then.
    fn covers(&self, now: &io::Result<Option<Ground>>) -> bool {
        match (self, now) {
            (Beginning::Watched { ground, went, .. }, Ok(Some(now))) => {
                !went.load(Ordering::SeqCst) && ground == now
            },
            (Beginning::Sweep { ground: Some(ground), .. }, Ok(Some(now))) => ground == now,
            (Beginning::Gone, Ok(None)) => true,
            (Beginning::Watched { .. } | Beginning::Gone | Beginning::Sweep { .. }, _) => false,
        }
    }

    /// Why the directory is not watched, where it is there and is not.
    fn trouble(&self) -> Option<&String> {
        match self {
            Beginning::Watched { .. } | Beginning::Gone => None,
            Beginning::Sweep { why, .. } => Some(why),
        }
    }
}

impl Outside {
    /// `dir`, which git placed in no work tree, watched, and whether a watch
    /// of it began anew: `before` as it is, where it is what is watched of
    /// `dir` as it is now; else watched anew, with what it heard before kept,
    /// and waking the thread on `wake`. What goes wrong, `troubles` says.
    fn watch(
        dir: &Path,
        before: Option<Outside>,
        wake: &SyncSender<()>,
        troubles: &mut Troubles,
    ) -> (Outside, bool) {
        let now = Ground::of(dir);
        let heard = match before {
            Some(outside) if outside.watching.covers(&now) => return (outside, false),
            // The watcher before lets go of its watches first.
            Some(Outside { heard, watching }) => {
                drop(watching);
                heard
            },
            None => Arc::default(),
        };
        let sweep = |ground: Option<Ground>, error: &dyn fmt::Display| Beginning::Sweep {
            ground,
            why: format!("{} is read on the sweep alone: {error}", dir.display()),
        };
        let watching = match now {
            Ok(Some(ground)) => {
                let went = Arc::new(AtomicBool::new(false));
                match beginning(&ground, heard.clone(), went.clone(), wake.clone()) {
                    Ok(watcher) => Beginning::Watched { ground, went, _watcher: watcher },
                    Err(error) => sweep(Some(ground), &error),
                }
            },
            Ok(None) => Beginning::Gone,
            Err(error) => sweep(None, &error),
        };
        if let Some(trouble) = watching.trouble() {
            troubles.say(trouble.clone());
        }
        let anew = matches!(watching, Beginning::Watched { .. });
        (Outside { heard, watching }, anew)
    }
}

/// Where git placed a directory it was asked about, and so what wakes it.
enum Placed {
    /// In the work tree with this top, which `trees` watches.
    In(PathBuf),
    /// In none that git would say, as of a directory in no repository.
    Outside(Outside),
    /// Nowhere git has said, as git has not answered yet when asked: nothing
    /// wakes it but the sweep.
    Unsaid,
}

impl Placed {
    /// The top of the work tree it is in, if any.
    fn top(&self) -> Option<&Path> {
        match self {
            Placed::In(top) => Some(top),
            Placed::Outside(_) | Placed::Unsaid => None,
        }
    }
}

/// Where git stands with a directory it has been asked about.
struct Asked {
    /// When git was done with it last.
    ended: Instant,
    /// How long git took over it then.
    took: Duration,
    /// Whether its work tree changed since, or git placed it in another as
    /// it was asked.
    changed: bool,
    /// Where git placed it then.
    placed: Placed,
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
        self.placed.top() == Some(top)
    }
}

/// Asks git where the work tree that `dir` is in is, and how it stands. What
/// wakes `dir` is watched first, so that a change while git reads it is
/// heard: its work tree, or `dir` itself where it is in none, as `before`
/// watched it where it was in none then too. What to watch hangs on where the
/// work tree is, so git is asked that before it is watched; where it is
/// watched anew, git is asked again after, as `dir` could have come to be in
/// another unheard in between: one that `git init` makes in `dir` can be
/// whole by the time its folder is watched. Where git does not answer,
/// nothing of `dir` is read here, and where git placed it before stands.
/// Where git placed `dir`, with the work tree in `trees` where it is in one;
/// what git said; and whether git placed `dir` elsewhere since, which has it
/// due again.
fn ask(
    dir: &str,
    before: Option<Placed>,
    trees: &mut HashMap<PathBuf, Tree>,
    wake: &SyncSender<()>,
    troubles: &mut Troubles,
) -> (Placed, Result<WorkTree, String>, bool) {
    let (placed, anew, common) = match place(dir) {
        Ok(Some(place)) => {
            let (top, common) = (place.top.clone(), place.common.clone());
            let (tree, anew) = Tree::watch(trees.remove(&top), place, wake, troubles);
            trees.insert(top.clone(), tree);
            (Placed::In(top), anew, Some(common))
        },
        Ok(None) => {
            let before = match before {
                Some(Placed::Outside(outside)) => Some(outside),
                Some(Placed::In(_) | Placed::Unsaid) | None => None,
            };
            let (outside, anew) = Outside::watch(Path::new(dir), before, wake, troubles);
            (Placed::Outside(outside), anew, None)
        },
        // git is killed once it has not answered in time, but a read here of
        // a mount that does not answer would hold this thread, and each
        // directory with it, for as long as the mount does not.
        Err(Unanswered) => (before.unwrap_or(Placed::Unsaid), false, None),
    };
    let said = status(dir, &placed, common.as_deref());
    let elsewhere = |now: Option<Place>| now.map(|place| place.top).as_deref() != placed.top();
    let moved = anew && place(dir).is_ok_and(elsewhere);
    (placed, said, moved)
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
            for was in asked.values_mut() {
                if let Placed::Outside(outside) = &was.placed
                    && outside.heard.swap(false, Ordering::SeqCst)
                {
                    was.changed = true;
                }
            }
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|was| was.due(sweep) <= now);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|dir| asked.get(dir).map(|was| was.due(sweep)));
            for dir in due {
                let started = Instant::now();
                let before = asked.remove(&dir).map(|was| was.placed);
                let (placed, said, moved) = ask(&dir, before, &mut trees, &wake, &mut troubles);
                let (took, ended) = (started.elapsed(), Instant::now());
                let trouble = said.as_ref().err().cloned();
                asked.insert(dir.clone(), Asked { ended, took, changed: moved, placed, trouble });
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
            let outside = asked.values().filter_map(|was| match &was.placed {
                Placed::Outside(outside) => outside.watching.trouble().cloned(),
                Placed::In(_) | Placed::Unsaid => None,
            });
            let watching = trees.values().filter_map(|tree| tree.watching.trouble().cloned());
            for trouble in asking.chain(outside).chain(watching) {
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

    use super::{stamp, Common};

    /// git tells no repository from one it refuses by its words alone, as it
    /// ends with one status for all of them: as it says them near a mount
    /// point too. Anything else it says, or a time it did not answer in, says
    /// nothing of a repository.
    #[test]
    fn only_git_s_own_words_say_a_directory_is_in_no_repository() {
        let said = |why: &str| Common::of(Err(why.to_owned()));
        for why in [
            "fatal: not a git repository (or any of the parent directories): .git",
            "fatal: not a git repository (or any parent up to mount point /run/user)",
        ] {
            assert_eq!(said(why), Common::NoRepository, "{why}");
        }
        for why in [
            "fatal: Expected git repo version <= 1, found 99",
            "fatal: unknown repository extension found:",
            "fatal: detected dubious ownership in repository at '/w'",
            "fatal: gitfile does not point to a valid repository: /w/.git",
            "fatal: not a git repository: /w/.bare/worktrees/gone",
            "fatal: cannot change to '/w': No such file or directory",
            "it did not answer in time",
        ] {
            assert_eq!(said(why), Common::Unsaid(why.to_owned()), "{why}");
        }
        assert_eq!(Common::of(Ok(b"/w/.bare\n".to_vec())), Common::Of("/w/.bare".to_owned()));
        assert!(matches!(Common::of(Ok(Vec::new())), Common::Unsaid(_)));
    }

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
