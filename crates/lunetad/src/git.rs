use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
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

use luneta_protocol::git::{self, Changed, GitStatus, Head};
use luneta_protocol::said::Said;
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};

use crate::core;
use crate::program::{self, Ran};
use crate::watch::{self, Troubles};

const LIMIT: Duration = Duration::from_mins(1);

const TREE_DIRS_MAX: usize = 4096;

const _: () = assert!(program::SAID_MAX <= u32::MAX as u64);

pub const REPOSITORY_VARS: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
];

pub fn local(command: &mut Command) -> &mut Command {
    for repository in REPOSITORY_VARS {
        command.env_remove(repository);
    }
    command
}

pub fn configured<'a>(command: &'a mut Command, pairs: &[(&str, &str)]) -> &'a mut Command {
    let count = env::var("GIT_CONFIG_COUNT").ok();
    let mut next = count.and_then(|count| count.parse().ok()).unwrap_or(0_u32);
    for (key, value) in pairs {
        command.env(format!("GIT_CONFIG_KEY_{next}"), key);
        command.env(format!("GIT_CONFIG_VALUE_{next}"), value);
        next = next.saturating_add(1);
    }
    command.env("GIT_CONFIG_COUNT", next.to_string())
}

fn status(dir: &str, placed: &Placed, common: Option<&Path>) -> Stood {
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
        return Ok((GitStatus::Outside, None));
    };
    match (said, placed) {
        (Ok(stdout), _) => GitStatus::parse(&String::from_utf8_lossy(&stdout), repository)
            .map(|mut tree| {
                let uncounted = match &mut tree {
                    GitStatus::Checkout { head: Head::Branch { name, changed, .. }, .. } => {
                        counted(dir, name.as_str(), changed)
                    },
                    _ => None,
                };
                (tree, uncounted)
            })
            .map_err(|why| format!("git status in {dir} said {why}")),
        (Err(_), Placed::Outside(_)) => Ok((GitStatus::NoCheckout { repository }, None)),
        (Err(why), Placed::In(_) | Placed::Unsaid) => {
            Err(format!("git status in {dir} said {why}"))
        },
    }
}

type Stood = Result<(GitStatus, Option<String>), String>;

fn counted(dir: &str, branch: &str, count: &mut Option<Changed>) -> Option<String> {
    let (changed, why) = match changed(dir, branch) {
        Ok(changed) => (changed, None),
        Err(why) => (None, Some(format!("git in {dir} did not count {branch}'s lines: {why}"))),
    };
    *count = changed;
    why
}

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

const IN_NO_REPOSITORY: &str = "fatal: not a git repository (or any ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Common {
    Of(String),
    NoRepository,
    Unsaid(String),
}

impl Common {
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

#[must_use]
pub fn common(dir: &str) -> Common {
    common_within(dir, LIMIT)
}

#[must_use]
pub fn common_within(dir: &str, limit: Duration) -> Common {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]))
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .env("LC_ALL", "C");
    let ran = program::run(&mut git, limit);
    Common::of(ran.map_err(|error| format!("it could not be run: {error}")).and_then(Ran::answer))
}

#[must_use]
pub fn directory_gone(stderr: &[u8]) -> bool {
    stderr.starts_with(b"fatal: cannot change to ")
        && stderr.trim_ascii_end().ends_with(b": No such file or directory")
}

#[must_use]
pub fn one_line(said: Vec<u8>) -> Option<String> {
    let said = String::from_utf8(said).ok()?;
    Some(said.strip_suffix('\n')?.to_owned()).filter(|said| !said.is_empty())
}

pub fn holder(common: &str) -> Option<String> {
    let common = Path::new(common);
    let hidden = common.file_name()?.to_str()?.starts_with('.');
    let holder = if hidden { common.parent()? } else { common };
    holder.to_str().map(str::to_owned)
}

pub fn branch(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    local(git.args(["-c", "safe.bareRepository=explicit", "-C", dir]))
        .args(["symbolic-ref", "--quiet", "HEAD"]);
    let said = program::run(&mut git, LIMIT).ok()?.answer().ok()?;
    let head = String::from_utf8(said).ok()?;
    Some(head.strip_suffix('\n')?.strip_prefix("refs/heads/")?.to_owned())
}

pub fn changed(dir: &str, branch: &str) -> Result<Option<Changed>, String> {
    let git = |args: &[&str]| {
        let mut git = Command::new("git");
        local(git.args(["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"]))
            .args(["-C", dir])
            .args(args)
            .env("GIT_NO_LAZY_FETCH", "1");
        program::run(&mut git, LIMIT).map_err(|error| format!("git could not be run: {error}"))
    };
    let tip = format!("refs/heads/{branch}");
    let mut named = vec![tip.clone()];
    named.extend(DEFAULT_REMOTES.map(|remote| format!("refs/remotes/{remote}/HEAD")));
    for name in ["main", "master"] {
        named.extend(DEFAULT_REMOTES.map(|remote| format!("refs/remotes/{remote}/{name}")));
        named.push(format!("refs/heads/{name}"));
    }
    let mut listing = vec!["for-each-ref", "--format=%(refname) %(objectname) %(symref)"];
    listing.extend(named.iter().map(String::as_str));
    let listed = git(&listing)?.answer().map_err(|why| format!("git for-each-ref said {why}"))?;
    let listed = String::from_utf8_lossy(&listed);
    let listed: HashMap<&str, (&str, &str)> = listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ' ');
            Some((fields.next()?, (fields.next()?, fields.next().unwrap_or(""))))
        })
        .collect();
    let Some(&(tip, _)) = listed.get(tip.as_str()) else {
        return Ok(None);
    };
    let Some((base, default)) = named[1..].iter().find_map(|name| {
        let &(base, symref) = listed.get(name.as_str())?;
        Some((base, default_name(if symref.is_empty() { name } else { symref })))
    }) else {
        return Ok(None);
    };
    if default == Some(branch) {
        return Ok(None);
    }
    let fork = match git(&["merge-base", tip, base])? {
        Ran::Ended { status, .. } if status.code() == Some(1) => return Ok(None),
        ran => ran.answer().map_err(|why| format!("git merge-base said {why}"))?,
    };
    let fork = one_line(fork).ok_or("git merge-base named no commit")?;
    let said = git(&["diff-index", "-M", "--numstat", "--no-textconv", &fork])?
        .answer()
        .map_err(|why| format!("git diff-index said {why}"))?;
    let (mut added, mut deleted) = (0u32, 0u32);
    for line in String::from_utf8_lossy(&said).lines() {
        let mut fields = line.split('\t');
        let Some(more) = fields.next().and_then(|count| count.parse::<u32>().ok()) else {
            continue;
        };
        let Some(fewer) = fields.next().and_then(|count| count.parse::<u32>().ok()) else {
            continue;
        };
        let room = "git diff-index said more lines than there is room to count";
        added = added.checked_add(more).ok_or(room)?;
        deleted = deleted.checked_add(fewer).ok_or(room)?;
    }
    Ok(Changed::new(added, deleted))
}

const DEFAULT_REMOTES: [&str; 3] = ["upstream", "github", "origin"];

fn default_name(target: &str) -> Option<&str> {
    target.strip_prefix("refs/heads/").or_else(|| {
        let remote = target.strip_prefix("refs/remotes/")?;
        DEFAULT_REMOTES.iter().find_map(|name| remote.strip_prefix(name)?.strip_prefix('/'))
    })
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

#[derive(PartialEq, Eq)]
struct Place {
    top: PathBuf,
    own: PathBuf,
    common: PathBuf,
}

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
        Ok(Ran::Ended { .. }) => Ok(None),
        Ok(Ran::Stuck) | Err(_) => Err(Unanswered),
    }
}

struct Unanswered;

impl Place {
    fn parse(said: &[u8]) -> Option<Place> {
        let lines = said.strip_suffix(b"\n")?.split(|byte| *byte == b'\n');
        let [top, own, common]: [&[u8]; 3] = lines.collect::<Vec<_>>().try_into().ok()?;
        let real = |path: &[u8]| fs::canonicalize(OsStr::from_bytes(path)).ok();
        Some(Place { top: real(top)?, own: real(own)?, common: real(common)? })
    }
}

fn listed(top: &Path) -> Result<Vec<u8>, String> {
    let mut git = Command::new("git");
    git.args(["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit", "-C"]).arg(top);
    local(&mut git).args(["ls-files", "-z"]);
    let ran = program::run(&mut git, LIMIT).map_err(|error| {
        format!("git ls-files in {} could not be run: {error}", top.display())
    })?;
    ran.answer().map_err(|why| format!("git ls-files in {}: {why}", top.display()))
}

fn tracked(top: &Path, listed: &[u8]) -> Option<BTreeSet<PathBuf>> {
    let mut dirs = BTreeSet::from([top.to_owned()]);
    for path in listed.split(|byte| *byte == 0) {
        let path = Path::new(OsStr::from_bytes(path));
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

fn watched(place: &Place, tracked: Option<&BTreeSet<PathBuf>>) -> BTreeMap<PathBuf, RecursiveMode> {
    use RecursiveMode::{NonRecursive, Recursive};
    let folders = [&place.own, &place.common].map(|folder| (folder.clone(), NonRecursive));
    let refs = place.common.join("refs");
    let refs = [
        (fs::canonicalize(&refs).unwrap_or(refs), Recursive),
        (place.common.join("reftable"), NonRecursive),
        (place.own.join("reftable"), NonRecursive),
    ];
    let dirs = tracked.into_iter().flatten();
    let dirs = dirs.flat_map(|dir| [(dir.clone(), NonRecursive), (dir.join(".git"), NonRecursive)]);
    let all = folders.into_iter().chain(refs).chain(dirs);
    all.filter(|(path, _)| path.is_dir()).collect()
}

fn stamp(path: &Path) -> Option<(u64, u64, SystemTime)> {
    let index = fs::metadata(path).ok()?;
    Some((index.ino(), index.len(), index.modified().ok()?))
}

enum Word {
    Read,
    Change,
    Dirs,
}

fn word(event: &notify::Result<Event>, watched: &BTreeMap<PathBuf, RecursiveMode>) -> Word {
    let Ok(event) = event else {
        return Word::Dirs;
    };
    match event.kind {
        _ if event.need_rescan() => Word::Dirs,
        kind if kind.is_access() => Word::Read,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder) => Word::Dirs,
        EventKind::Modify(ModifyKind::Name(_))
            if event.paths.iter().any(|path| watched.contains_key(path) || path.is_dir()) =>
        {
            Word::Dirs
        },
        _ => Word::Change,
    }
}

fn denied(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::Io(error) => error.kind() == io::ErrorKind::PermissionDenied,
        _ => false,
    }
}

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
    let config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = RecommendedWatcher::new(handler, config)?;
    add(&mut watcher, paths)?;
    Ok(watcher)
}

fn beginning(
    ground: &Ground,
    heard: Arc<AtomicBool>,
    went: Arc<AtomicBool>,
    wake: SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let (dir, git) = (ground.real.clone(), ground.real.join(".git"));
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

fn add(
    watcher: &mut RecommendedWatcher,
    paths: &BTreeMap<PathBuf, RecursiveMode>,
) -> notify::Result<()> {
    let mut adding = watcher.paths_mut();
    for (path, mode) in paths {
        match adding.add(path, *mode) {
            Ok(()) => {},
            Err(error) if watch::missing(&error) => {},
            Err(error) if path.ends_with(".git") && denied(&error) => {},
            Err(error) => return Err(error),
        }
    }
    adding.commit()
}

fn hear(heard: &AtomicBool, wake: &SyncSender<()>) {
    heard.store(true, Ordering::SeqCst);
    match wake.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

struct Tree {
    heard: Arc<AtomicBool>,
    came_or_went: Arc<AtomicBool>,
    place: Place,
    index: Option<(u64, u64, SystemTime)>,
    watching: Watching,
}

enum Watching {
    Whole { _watcher: RecommendedWatcher },
    Folders { _watcher: RecommendedWatcher, why: String },
    Sweep { why: String },
}

impl Watching {
    fn trouble(&self) -> Option<&String> {
        match self {
            Watching::Whole { .. } => None,
            Watching::Folders { why, .. } | Watching::Sweep { why } => Some(why),
        }
    }
}

impl Tree {
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

struct Outside {
    heard: Arc<AtomicBool>,
    watching: Beginning,
}

#[derive(PartialEq, Eq)]
struct Ground {
    real: PathBuf,
    git: bool,
}

impl Ground {
    fn of(dir: &Path) -> io::Result<Option<Ground>> {
        let real = match fs::canonicalize(dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            real => real?,
        };
        let git = real.join(".git").is_dir();
        Ok(Some(Ground { real, git }))
    }
}

enum Beginning {
    Watched { ground: Ground, went: Arc<AtomicBool>, _watcher: RecommendedWatcher },
    Gone,
    Sweep { ground: Option<Ground>, why: String },
}

impl Beginning {
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

    fn trouble(&self) -> Option<&String> {
        match self {
            Beginning::Watched { .. } | Beginning::Gone => None,
            Beginning::Sweep { why, .. } => Some(why),
        }
    }
}

impl Outside {
    fn watch(
        dir: &Path,
        before: Option<Outside>,
        wake: &SyncSender<()>,
        troubles: &mut Troubles,
    ) -> (Outside, bool) {
        let now = Ground::of(dir);
        let heard = match before {
            Some(outside) if outside.watching.covers(&now) => return (outside, false),
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

enum Placed {
    In(PathBuf),
    Outside(Outside),
    Unsaid,
}

impl Placed {
    fn top(&self) -> Option<&Path> {
        match self {
            Placed::In(top) => Some(top),
            Placed::Outside(_) | Placed::Unsaid => None,
        }
    }
}

struct Asked {
    ended: Instant,
    took: Duration,
    changed: bool,
    placed: Placed,
    trouble: Option<String>,
}

impl Asked {
    fn due(&self, sweep: Duration) -> Instant {
        self.ended + if self.changed { self.took } else { self.took.max(sweep) }
    }

    fn within(&self, top: &Path) -> bool {
        self.placed.top() == Some(top)
    }
}

fn ask(
    dir: &str,
    before: Option<Placed>,
    trees: &mut HashMap<PathBuf, Tree>,
    wake: &SyncSender<()>,
    troubles: &mut Troubles,
) -> (Placed, Stood, bool) {
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
        Err(Unanswered) => (before.unwrap_or(Placed::Unsaid), false, None),
    };
    let said = status(dir, &placed, common.as_deref());
    let elsewhere = |now: Option<Place>| now.map(|place| place.top).as_deref() != placed.top();
    let moved = anew && place(dir).is_ok_and(elsewhere);
    (placed, said, moved)
}

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
            due.sort_by_key(|dir| asked.get(dir).map(|was| was.due(sweep)));
            for dir in due {
                let started = Instant::now();
                let before = asked.remove(&dir).map(|was| was.placed);
                let (placed, said, moved) = ask(&dir, before, &mut trees, &wake, &mut troubles);
                let (took, ended) = (started.elapsed(), Instant::now());
                let trouble = match &said {
                    Ok((_, uncounted)) => uncounted.clone(),
                    Err(why) => Some(why.clone()),
                };
                if let Some(trouble) = &trouble {
                    troubles.say(trouble.clone());
                }
                asked.insert(dir.clone(), Asked { ended, took, changed: moved, placed, trouble });
                if let Ok((tree, _)) = said
                    && core.send(core::Command::GitStatus { dir, tree }).is_err()
                {
                    return;
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
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
    use std::path::Path;
    use std::time::{Duration, SystemTime};

    use super::{Changed, Common, changed, stamp};

    fn git(dir: &Path, args: &[&str]) -> bool {
        let mut git = std::process::Command::new("git");
        for repository in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
            git.env_remove(repository);
        }
        git.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(["-c", "maintenance.auto=false"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn git_counts_the_lines_changed_on_a_branch_against_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| git(dir.path(), args);
        let commit = |file: &str, text: &str| {
            fs::write(dir.path().join(file), text).unwrap();
            assert!(git(&["add", file]));
            assert!(git(&["commit", "-q", "-m", file]));
        };
        assert!(git(&["init", "-q", "-b", "main"]));
        commit("base", "a\nb\nc\n");
        commit("same", "s\n");
        assert!(git(&["switch", "-q", "-c", "issue-299"]));
        commit("base", "a\nB\n\n");
        fs::write(dir.path().join("new"), "d\n").unwrap();
        assert!(git(&["add", "-N", "new"]));
        let at = dir.path().to_str().unwrap();
        let later = SystemTime::now() + Duration::from_secs(60);
        let same = File::options().write(true).open(dir.path().join("same")).unwrap();
        same.set_modified(later).unwrap();
        let index = dir.path().join(".git").join("index");
        let before = stamp(&index);
        assert_eq!(changed(at, "issue-299"), Ok(Changed::new(3, 2)));
        assert_eq!(stamp(&index), before);

        assert!(git(&["switch", "-q", "main"]));
        assert_eq!(changed(at, "main"), Ok(None));
        assert!(git(&["switch", "-q", "-c", "fresh"]));
        assert_eq!(changed(at, "fresh"), Ok(Changed::new(1, 0)));
        assert!(git(&["switch", "-q", "main"]));

        assert!(git(&["rm", "-q", "-f", "new"]));
        assert!(git(&["switch", "-q", "-c", "trunk"]));
        commit("trunk", "t\n");
        assert!(git(&["update-ref", "refs/remotes/origin/trunk", "HEAD"]));
        assert!(git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/trunk"]));
        assert!(git(&["switch", "-q", "-c", "fold"]));
        commit("fold", "f\n");
        assert_eq!(changed(at, "fold"), Ok(Changed::new(1, 0)));
        assert_eq!(changed(at, "trunk"), Ok(None));
        assert!(git(&["update-ref", "refs/remotes/upstream/next", "HEAD"]));
        assert!(git(&["symbolic-ref", "refs/remotes/upstream/HEAD", "refs/remotes/upstream/next"]));
        assert!(git(&["switch", "-q", "-c", "next"]));
        assert_eq!(changed(at, "next"), Ok(None));
        commit("next", "n\n");
        assert_eq!(changed(at, "next"), Ok(None));
        assert!(git(&["switch", "-q", "-c", "onto-next"]));
        commit("onto-next", "o\n");
        assert_eq!(changed(at, "onto-next"), Ok(Changed::new(2, 0)));

        assert!(git(&["switch", "-q", "--orphan", "lone"]));
        commit("lone", "l\n");
        assert_eq!(changed(at, "lone"), Ok(None));
        assert_eq!(changed(at, "unborn"), Ok(None));
    }

    #[test]
    fn git_fetches_nothing_to_count_the_lines_changed_in_a_partial_clone() {
        let dir = tempfile::tempdir().unwrap();
        let (origin, clone) = (dir.path().join("origin"), dir.path().join("clone"));
        fs::create_dir(&origin).unwrap();
        let lines = |from: u32, to: u32| (from..=to).map(|line| line.to_string() + "\n").collect();
        let commit = |text: String| {
            fs::write(origin.join("lines"), text).unwrap();
            assert!(git(&origin, &["add", "lines"]));
            assert!(git(&origin, &["commit", "-q", "-m", "lines"]));
        };
        assert!(git(&origin, &["init", "-q", "-b", "main"]));
        assert!(git(&origin, &["config", "uploadpack.allowFilter", "true"]));
        commit(lines(1, 100));
        commit(lines(1, 50));
        assert!(git(&origin, &["switch", "-q", "-c", "fold"]));
        commit(lines(1, 70));
        assert!(git(&origin, &["switch", "-q", "main"]));
        let url = format!("file://{}", origin.display());
        let cloned = ["clone", "-q", "--filter=blob:none", "--no-checkout", &url, "clone"];
        assert!(git(dir.path(), &cloned));
        assert!(git(&clone, &["checkout", "-q", "-b", "fold", "origin/fold"]));

        let at = clone.to_str().unwrap();
        let said = changed(at, "fold");
        assert!(said.as_ref().is_err_and(|why| why.starts_with("git diff-index said")), "{said:?}");
    }

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
