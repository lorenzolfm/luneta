use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::mem;
use std::ops::Bound;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lune_domain::flows::GitOid;
use lune_domain::projects::{Dir, PATH_BYTES_MAX};
pub use lune_protocol::PATCH_BYTES_MAX;
use lune_protocol::diff::{ReadDiff, RelativePath, UntrackedFile};
use lune_protocol::git::{self, Changed, GitStatus, Head};
use lune_protocol::said::Said;
use lune_protocol::{UNTRACKED_BYTES_MAX, UNTRACKED_FILES_MAX, UNTRACKED_TOTAL_BYTES_MAX};
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, PathsMut, RecommendedWatcher, RecursiveMode, Watcher as _};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::core::{self, FOLLOW_DIRS_MAX};
use crate::directory::Directory;
use crate::log;
use crate::program::{self, Ran};
use crate::watch::{self, Troubles};

const LIMIT: Duration = Duration::from_mins(1);

const PUSH_LIMIT: Duration = Duration::from_mins(3);

const TREE_DIRS_MAX: usize = 4096;

const UNTRACKED_PATHS_INSPECTED_MAX: usize = 1024;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const GIT_FOLDERS_MAX: usize = 5;

const TREE_PATHS_MAX: usize = TREE_DIRS_MAX * 2 + GIT_FOLDERS_MAX;

const DIR_EVENTS_MAX: usize = 256;

const EVENT_PATHS_MAX: usize = 256;

const WATCHES_MAX: usize = FOLLOW_DIRS_MAX;

const PATH_ANCESTORS_MAX: usize = PATH_BYTES_MAX / 2 + 2;

const RECURSIVE_ADD_RETRIES: usize = 4;

const _: () = assert!(program::SAID_MAX <= u32::MAX as u64);
const _: () = assert!(UNTRACKED_PATHS_INSPECTED_MAX >= UNTRACKED_FILES_MAX);
const _: () =
    assert!(PATCH_BYTES_MAX + UNTRACKED_TOTAL_BYTES_MAX < lune_protocol::RPC_MESSAGE_BYTES_MAX);
const _: () = assert!(TREE_DIRS_MAX > 0);
const _: () = assert!(GIT_FOLDERS_MAX == 5);
const _: () = assert!(TREE_PATHS_MAX > TREE_DIRS_MAX);
const _: () = assert!(DIR_EVENTS_MAX > 0);
const _: () = assert!(EVENT_PATHS_MAX > 0);
const _: () = assert!(WATCHES_MAX == FOLLOW_DIRS_MAX);
const _: () = assert!(WATCHES_MAX <= u32::MAX as usize);
const _: () = assert!(PATH_ANCESTORS_MAX > 0);
const _: () = assert!(RECURSIVE_ADD_RETRIES > 0);

pub const REPOSITORY_VARS: [&str; 13] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_GRAFT_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_SHALLOW_FILE",
    "GIT_PREFIX",
];

pub fn local(command: &mut Command) -> &mut Command {
    for repository in REPOSITORY_VARS {
        command.env_remove(repository);
    }
    command
}

const HARDENED: [&str; 4] = [
    "-c",
    "core.fsmonitor=false",
    "-c",
    "safe.bareRepository=explicit",
];

fn hardened() -> Command {
    let mut git = Command::new("git");
    local(git.args(HARDENED)).env("GIT_TERMINAL_PROMPT", "0");
    git
}

fn read_by_luned(git: &mut Command) -> &mut Command {
    git.env("GIT_OPTIONAL_LOCKS", "0").env("LC_ALL", "C")
}

pub struct Git {
    command: Command,
    words: Vec<OsString>,
}

impl Git {
    #[must_use]
    pub fn at(dir: impl AsRef<OsStr>, command: &'static str) -> Git {
        let mut git = hardened();
        git.arg("-C").arg(dir);
        Git {
            command: git,
            words: vec![command.into()],
        }
    }

    #[must_use]
    pub fn of(common: &Directory, command: &'static str) -> Git {
        let mut git = hardened();
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(common.as_str());
        git.arg(git_dir).current_dir("/");
        Git {
            command: git,
            words: vec![command.into()],
        }
    }

    pub fn arg(&mut self, word: &'static str) -> &mut Git {
        self.words.push(word.into());
        self
    }

    pub fn args(&mut self, words: impl IntoIterator<Item = &'static str>) -> &mut Git {
        self.words.extend(words.into_iter().map(OsString::from));
        self
    }

    pub fn branch_ref(&mut self, branch: &str) -> &mut Git {
        self.words.push(format!("refs/heads/{branch}").into());
        self
    }

    pub fn leased(&mut self, branch: &str, at: &str) -> &mut Git {
        self.words
            .push(format!("--force-with-lease=refs/heads/{branch}:{at}").into());
        self
    }

    pub fn editing_nothing(&mut self) -> &mut Git {
        self.command
            .env("GIT_EDITOR", ":")
            .env("GIT_SEQUENCE_EDITOR", ":");
        self
    }

    fn fetching_nothing_lazily(&mut self) -> &mut Git {
        self.command.env("GIT_NO_LAZY_FETCH", "1");
        self
    }

    pub fn configured(&mut self, pairs: &[(&'static str, &'static str)]) -> &mut Git {
        for (key, value) in pairs {
            self.command.arg("-c").arg(format!("{key}={value}"));
        }
        self
    }

    #[must_use]
    pub fn named<I, S>(mut self, names: I) -> Named
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(&self.words);
        let mut names = names.into_iter().peekable();
        if names.peek().is_some() {
            self.command.arg("--end-of-options").args(names);
        }
        Named(self.command)
    }

    pub fn run(self, limit: Duration) -> io::Result<Ran> {
        self.named(std::iter::empty::<&str>()).run(limit)
    }
}

pub struct Named(Command);

impl Named {
    pub fn run(mut self, limit: Duration) -> io::Result<Ran> {
        program::run(read_by_luned(&mut self.0), limit)
    }

    pub fn run_prefix(mut self, limit: Duration, most: u64) -> io::Result<(Ran, u64)> {
        program::run_prefix(read_by_luned(&mut self.0), limit, most)
    }

    pub fn run_alone(mut self, limit: Duration) -> io::Result<Ran> {
        program::run_alone(&mut self.0, limit)
    }
}

#[cfg(test)]
pub(crate) fn scratch(dir: impl AsRef<OsStr>, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut git = hardened();
    read_by_luned(&mut git)
        .arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "maintenance.auto=false"])
        .args(args);
    let ran = program::run(&mut git, LIMIT).map_err(|error| format!("git {args:?}: {error}"))?;
    ran.answer().map_err(|why| format!("git {args:?}: {why}"))
}

pub fn configured<'a>(command: &'a mut Command, pairs: &[(&str, &str)]) -> &'a mut Command {
    let Some(first) = config_count(env::var_os("GIT_CONFIG_COUNT").as_deref()) else {
        return command;
    };
    for (at, (key, value)) in pairs.iter().enumerate() {
        command.env(format!("GIT_CONFIG_KEY_{}", first + at), key);
        command.env(format!("GIT_CONFIG_VALUE_{}", first + at), value);
    }
    command.env("GIT_CONFIG_COUNT", (first + pairs.len()).to_string())
}

fn config_count(inherited: Option<&OsStr>) -> Option<usize> {
    match inherited {
        None => Some(0),
        Some(count) if count.is_empty() => Some(0),
        Some(count) => count.to_str()?.parse().ok(),
    }
}

fn status(dir: &Dir, placed: &Placed, common: Option<&AbsPath>) -> Stood {
    let dir = dir.as_str();
    let mut git = Git::at(dir, "status");
    git.args(git::STATUS);
    let ran = git
        .run(LIMIT)
        .map_err(|error| format!("git status in {dir} could not be run: {error}"))?;
    let said = match ran {
        Ran::Stuck => return Err(format!("git status in {dir} did not answer in time")),
        ended @ Ran::Ended { .. } => ended.answer(),
    };
    let told = common.and_then(AbsPath::to_str).filter(|_| said.is_ok());
    let repository = match told {
        Some(common) => Some(holder_said(common)?),
        None => holder_of(dir)?,
    };
    let Some(repository) = repository else {
        return Ok((GitStatus::Outside, None));
    };
    match (said, placed) {
        (Ok(stdout), _) => GitStatus::parse(&String::from_utf8_lossy(&stdout), repository)
            .map(|mut status| {
                let uncounted = match &mut status {
                    GitStatus::Checkout {
                        head: Head::Branch { name, changed, .. },
                        ..
                    } => counted(dir, name.as_str(), changed),
                    _ => None,
                };
                (status, uncounted)
            })
            .map_err(|why| format!("git status in {dir} said {why}")),
        (Err(_), Placed::Outside(_)) => Ok((GitStatus::NoCheckout { repository }, None)),
        (Err(why), Placed::In(_) | Placed::Unsaid) => {
            Err(format!("git status in {dir} said {why}"))
        }
    }
}

type Stood = Result<(GitStatus, Option<String>), String>;

fn counted(dir: &str, branch: &str, count: &mut Option<Changed>) -> Option<String> {
    let (changed, why) = match changed(dir, branch) {
        Ok(changed) => (changed, None),
        Err(why) => (
            None,
            Some(format!(
                "git in {dir} did not count {branch}'s lines: {why}"
            )),
        ),
    };
    *count = changed;
    why
}

fn holder_of(dir: &str) -> Result<Option<Said>, String> {
    match common(dir) {
        Common::Of(common) => holder_said(common.as_str()).map(Some),
        Common::NoRepository | Common::Gone => Ok(None),
        Common::BareUnread(bare) => Err(format!("{dir} is in {}", Common::bare_unread(&bare))),
        Common::Unsaid(why) => Err(format!(
            "git did not say what repository {dir} is in: {why}"
        )),
    }
}

fn holder_said(common: &str) -> Result<Said, String> {
    holder(common)
        .and_then(Said::new)
        .ok_or_else(|| format!("no directory holds the repository {common}"))
}

const IN_NO_REPOSITORY: &str = "fatal: not a git repository (or any ";

pub const BARE_UNREAD: &str = "fatal: cannot use bare repository ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Common {
    Of(Directory),
    NoRepository,
    BareUnread(Directory),
    Gone,
    Unsaid(String),
}

impl Common {
    #[must_use]
    pub fn of(answer: Result<Vec<u8>, String>) -> Common {
        match answer.map(one_line) {
            Ok(Some(common)) => Directory::parse(common).map_or_else(Common::Unsaid, Common::Of),
            Ok(None) => Common::Unsaid("it named no directory".to_owned()),
            Err(why) if why.starts_with(IN_NO_REPOSITORY) => Common::NoRepository,
            Err(why) if directory_gone(why.as_bytes()) => Common::Gone,
            Err(why) => bare_unread(&why).map_or(Common::Unsaid(why), Common::BareUnread),
        }
    }

    #[must_use]
    pub fn bare_unread(bare: &Directory) -> String {
        format!("the bare repository {bare}, which git opens only when named")
    }
}

fn bare_unread(why: &str) -> Option<Directory> {
    let named = why.strip_prefix(BARE_UNREAD)?.strip_prefix('\'')?;
    let (bare, _) = named.rsplit_once("' (")?;
    Directory::parse(bare.to_owned()).ok()
}

#[must_use]
pub fn common(dir: &str) -> Common {
    common_within(dir, LIMIT)
}

#[must_use]
pub fn common_within(dir: &str, limit: Duration) -> Common {
    let mut git = Git::at(dir, "rev-parse");
    git.args(["--path-format=absolute", "--git-common-dir"]);
    let ran = git.run(limit);
    Common::of(
        ran.map_err(|error| format!("it could not be run: {error}"))
            .and_then(Ran::answer),
    )
}

#[must_use]
pub fn directory_gone(stderr: &[u8]) -> bool {
    stderr.starts_with(b"fatal: cannot change to ")
        && stderr
            .trim_ascii_end()
            .ends_with(b": No such file or directory")
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

pub fn branch(dir: &str) -> Result<Option<String>, String> {
    branch_within(dir, LIMIT)
}

pub fn branch_within(dir: &str, limit: Duration) -> Result<Option<String>, String> {
    pointed(|command| Git::at(dir, command), Symbolic::Head, limit)
        .map_err(|why| format!("in {dir}: {why}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbolic<'a> {
    Head,
    RemoteHead(&'a str),
}

impl Symbolic<'_> {
    fn name(self) -> String {
        match self {
            Symbolic::Head => "HEAD".to_owned(),
            Symbolic::RemoteHead(remote) => format!("refs/remotes/{remote}/HEAD"),
        }
    }

    fn folder(self) -> String {
        match self {
            Symbolic::Head => "refs/heads/".to_owned(),
            Symbolic::RemoteHead(remote) => format!("refs/remotes/{remote}/"),
        }
    }
}

pub fn pointed(
    git: impl FnOnce(&'static str) -> Git,
    symbolic: Symbolic<'_>,
    limit: Duration,
) -> Result<Option<String>, String> {
    let name = symbolic.name();
    let mut git = git("symbolic-ref");
    git.arg("--quiet");
    let asked = format!("git symbolic-ref {name}");
    let ran = git
        .named([&name])
        .run(limit)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    let said = match ran {
        Ran::Ended { status, .. } if status.code() == Some(1) => return Ok(None),
        ran => ran.answer().map_err(|why| format!("{asked}: {why}"))?,
    };
    let said = one_line(said).ok_or_else(|| format!("{asked} said no one line"))?;
    let pointed = said
        .strip_prefix(&symbolic.folder())
        .filter(|pointed| !pointed.is_empty());
    Ok(pointed.map(str::to_owned))
}

pub fn changed(dir: &str, branch: &str) -> Result<Option<Changed>, String> {
    let git = |command: &'static str, args: &[&'static str], names: &[&str]| {
        let mut git = Git::at(dir, command);
        git.args(args.iter().copied()).fetching_nothing_lazily();
        git.named(names)
            .run(LIMIT)
            .map_err(|error| format!("git could not be run: {error}"))
    };
    let tip = format!("refs/heads/{branch}");
    let bases = defaults();
    let mut listing = vec![tip.as_str()];
    listing.extend(bases.iter().map(String::as_str));
    let listed = git("for-each-ref", &[LISTING], &listing)?;
    let listed = listed
        .answer()
        .map_err(|why| format!("git for-each-ref said {why}"))?;
    let listed = String::from_utf8_lossy(&listed);
    let listed = refs(&listed);
    let Some(&(tip, _)) = listed.get(tip.as_str()) else {
        return Ok(None);
    };
    let Some((base, target)) = first_default(&bases, &listed) else {
        return Ok(None);
    };
    if DefaultBranch::of(target).is_some_and(|default| default.branch() == branch) {
        return Ok(None);
    }
    let fork = match git("merge-base", &[], &[tip, base])? {
        Ran::Ended { status, .. } if status.code() == Some(1) => return Ok(None),
        ran => ran
            .answer()
            .map_err(|why| format!("git merge-base said {why}"))?,
    };
    let fork = one_line(fork).ok_or("git merge-base named no commit")?;
    let said = git(
        "diff-index",
        &["-M", "--numstat", "--no-textconv"],
        &[&fork],
    )?
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

pub fn patch(dir: &Dir) -> Result<ReadDiff, String> {
    let dir = dir.as_str();
    let git = |command: &'static str, args: &[&'static str], names: &[&str]| {
        let mut git = Git::at(dir, command);
        git.args(args.iter().copied()).fetching_nothing_lazily();
        git.named(names)
            .run(LIMIT)
            .map_err(|error| format!("git could not be run: {error}"))
    };
    let head_branch = branch(dir)?;
    let head = head_branch
        .clone()
        .and_then(Said::new)
        .unwrap_or_else(|| Said::new("HEAD".to_owned()).expect("HEAD is non-empty"));
    let (fork, base) = patch_base(dir, head_branch.as_deref(), &git)?;
    let patch_limit = u64::try_from(PATCH_BYTES_MAX).expect("PATCH_BYTES_MAX fits u64");
    let mut diff_index = Git::at(dir, "diff-index");
    diff_index
        .args(["-p", "-M", "--no-textconv", "--no-color", "-U3"])
        .fetching_nothing_lazily();
    let (ran, extra_bytes) = diff_index
        .named([fork.as_str()])
        .run_prefix(LIMIT, patch_limit)
        .map_err(|error| format!("git could not be run: {error}"))?;
    let mut raw = ran
        .answer()
        .map_err(|why| format!("git diff-index said {why}"))?;
    let mut truncated = extra_bytes > 0 || raw.len() > PATCH_BYTES_MAX;
    if truncated {
        raw.truncate(PATCH_BYTES_MAX);
        if let Some(newline) = raw.iter().rposition(|&byte| byte == b'\n') {
            raw.truncate(newline + 1);
        }
    }
    let (untracked, untracked_truncated) = read_untracked(dir, &git)?;
    truncated |= untracked_truncated;
    ReadDiff::new(base, head, raw, truncated, untracked)
}

fn patch_base(
    dir: &str,
    head_branch: Option<&str>,
    git: &impl Fn(&'static str, &[&'static str], &[&str]) -> Result<Ran, String>,
) -> Result<(String, Said), String> {
    if let Some(branch) = head_branch {
        let tip = format!("refs/heads/{branch}");
        let bases = defaults();
        let mut listing = vec![tip.as_str()];
        listing.extend(bases.iter().map(String::as_str));
        let listed = git("for-each-ref", &[LISTING], &listing)?;
        let listed = listed
            .answer()
            .map_err(|why| format!("git for-each-ref said {why}"))?;
        let listed = String::from_utf8_lossy(&listed);
        let listed = refs(&listed);
        if let Some(&(tip_oid, _)) = listed.get(tip.as_str())
            && let Some((base_oid, target)) = first_default(&bases, &listed)
            && let Some(default) = DefaultBranch::of(target)
            && default.branch() != branch
        {
            match git("merge-base", &[], &[tip_oid, base_oid])? {
                Ran::Ended { status, .. } if status.code() == Some(1) => {}
                ran => {
                    let fork = ran
                        .answer()
                        .map_err(|why| format!("git merge-base said {why}"))?;
                    if let Some(fork) = one_line(fork)
                        && let Some(label) = Said::new(default.to_string())
                    {
                        return Ok((fork, label));
                    }
                }
            }
        }
    }
    match git("rev-parse", &["--verify", "--quiet"], &["HEAD"])? {
        Ran::Ended { status, stdout, .. } if status.success() => {
            if let Some(head_oid) = one_line(stdout) {
                return Ok((
                    head_oid,
                    Said::new("HEAD".to_owned()).expect("HEAD is non-empty"),
                ));
            }
        }
        Ran::Ended { .. } => {}
        Ran::Stuck => return Err(format!("git rev-parse in {dir} did not answer in time")),
    }
    Ok((
        EMPTY_TREE.to_owned(),
        Said::new("empty".to_owned()).expect("empty is non-empty"),
    ))
}

fn open_untracked_at(root: &OwnedFd, rel_path: &RelativePath) -> Result<OwnedFd, Errno> {
    let mut parts = rel_path.as_str().split('/');
    let Some(mut current_name) = parts.next() else {
        return Err(Errno::INVAL);
    };
    let mut held: Option<OwnedFd> = None;
    for next_name in parts {
        let parent = held.as_ref().unwrap_or(root);
        let dir = rustix::fs::openat(
            parent,
            current_name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        held = Some(dir);
        current_name = next_name;
    }
    let parent = held.as_ref().unwrap_or(root);
    rustix::fs::openat(
        parent,
        current_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
}

fn read_untracked(
    dir: &str,
    git: &impl Fn(&'static str, &[&'static str], &[&str]) -> Result<Ran, String>,
) -> Result<(Vec<UntrackedFile>, bool), String> {
    let listed = git("ls-files", &["--others", "--exclude-standard", "-z"], &[])?
        .answer()
        .map_err(|why| format!("git ls-files said {why}"))?;
    let root = rustix::fs::open(
        Path::new(dir),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("worktree {dir} could not be opened: {error}"))?;
    let file_limit = u64::try_from(UNTRACKED_BYTES_MAX).expect("UNTRACKED_BYTES_MAX fits u64");
    let mut files = Vec::new();
    let mut total_bytes = 0usize;
    let mut inspected = 0usize;
    let mut truncated = false;
    for raw_path in listed.split(|&byte| byte == 0) {
        if raw_path.is_empty() {
            continue;
        }
        if inspected >= UNTRACKED_PATHS_INSPECTED_MAX {
            truncated = true;
            break;
        }
        inspected += 1;
        if files.len() >= UNTRACKED_FILES_MAX {
            truncated = true;
            break;
        }
        let Ok(rel) = std::str::from_utf8(raw_path) else {
            continue;
        };
        let Ok(rel_path) = RelativePath::parse(rel.to_owned()) else {
            truncated = true;
            continue;
        };
        let fd = match open_untracked_at(&root, &rel_path) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR | Errno::ISDIR | Errno::NXIO) => {
                continue;
            }
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        let file = File::from(fd);
        let Ok(meta) = file.metadata() else {
            truncated = true;
            continue;
        };
        if !meta.file_type().is_file() {
            continue;
        }
        if meta.len() > file_limit {
            truncated = true;
            continue;
        }
        let mut bytes = Vec::new();
        if file.take(file_limit + 1).read_to_end(&mut bytes).is_err() {
            truncated = true;
            continue;
        }
        if bytes.len() > UNTRACKED_BYTES_MAX {
            truncated = true;
            continue;
        }
        if bytes.contains(&0) {
            continue;
        }
        if total_bytes.saturating_add(bytes.len()) > UNTRACKED_TOTAL_BYTES_MAX {
            truncated = true;
            break;
        }
        let Ok(content) = String::from_utf8(bytes) else {
            continue;
        };
        let Ok(untracked) = UntrackedFile::new(rel_path, content) else {
            truncated = true;
            continue;
        };
        total_bytes += untracked.content().len();
        files.push(untracked);
    }
    assert!(
        inspected <= UNTRACKED_PATHS_INSPECTED_MAX,
        "inspected untracked paths stay bounded"
    );
    Ok((files, truncated))
}

const DEFAULT_REMOTES: [&str; 3] = ["upstream", "github", "origin"];

const LISTING: &str = "--format=%(refname) %(objectname) %(symref)";

fn defaults() -> Vec<String> {
    let mut named = DEFAULT_REMOTES
        .map(|remote| format!("refs/remotes/{remote}/HEAD"))
        .to_vec();
    for name in ["main", "master"] {
        named.extend(DEFAULT_REMOTES.map(|remote| format!("refs/remotes/{remote}/{name}")));
        named.push(format!("refs/heads/{name}"));
    }
    named
}

fn refs(listed: &str) -> HashMap<&str, (&str, &str)> {
    listed
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ' ');
            Some((
                fields.next()?,
                (fields.next()?, fields.next().unwrap_or("")),
            ))
        })
        .collect()
}

fn first_default<'a>(
    named: &'a [String],
    listed: &HashMap<&'a str, (&'a str, &'a str)>,
) -> Option<(&'a str, &'a str)> {
    named.iter().find_map(|name| {
        let &(base, symref) = listed.get(name.as_str())?;
        Some((
            base,
            if symref.is_empty() {
                name.as_str()
            } else {
                symref
            },
        ))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultBranch {
    Remote {
        remote: &'static str,
        branch: String,
    },
    Local {
        branch: String,
    },
}

impl DefaultBranch {
    fn of(target: &str) -> Option<DefaultBranch> {
        let named = |branch: &&str| !branch.is_empty();
        if let Some(branch) = target.strip_prefix("refs/heads/").filter(named) {
            return Some(DefaultBranch::Local {
                branch: branch.to_owned(),
            });
        }
        let remote = target.strip_prefix("refs/remotes/")?;
        DEFAULT_REMOTES.into_iter().find_map(|name| {
            let branch = remote.strip_prefix(name)?.strip_prefix('/').filter(named)?;
            Some(DefaultBranch::Remote {
                remote: name,
                branch: branch.to_owned(),
            })
        })
    }

    #[must_use]
    pub fn branch(&self) -> &str {
        match self {
            DefaultBranch::Remote { branch, .. } | DefaultBranch::Local { branch } => branch,
        }
    }

    #[must_use]
    pub fn reference(&self) -> String {
        match self {
            DefaultBranch::Remote { remote, branch } => format!("refs/remotes/{remote}/{branch}"),
            DefaultBranch::Local { branch } => format!("refs/heads/{branch}"),
        }
    }
}

impl fmt::Display for DefaultBranch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DefaultBranch::Remote { remote, branch } => write!(f, "{remote}/{branch}"),
            DefaultBranch::Local { branch } => f.write_str(branch),
        }
    }
}

pub fn default_branch(dir: &str, limit: Duration) -> Result<DefaultBranch, String> {
    let named = defaults();
    let listed = says_within(dir, "for-each-ref", &[LISTING], &named, limit)?;
    let listed = refs(&listed);
    let remotes = DEFAULT_REMOTES.join(", ");
    let Some((_, target)) = first_default(&named, &listed) else {
        let none = "and no main or master of its own";
        return Err(format!("no HEAD, main or master of {remotes}, {none}"));
    };
    let unknown = || format!("git says {target} is the default branch, of none of {remotes}");
    DefaultBranch::of(target).ok_or_else(unknown)
}

pub enum NamedHead {
    OnRemote,
    PullRef { remote_url: String },
}

pub fn named_head(dir: &str, branch: &str) -> Result<Option<NamedHead>, String> {
    let head = format!("refs/heads/{branch}");
    let push = says(
        dir,
        "for-each-ref",
        &["--format=%(push:remotename)"],
        &[&head],
    )?;
    if push.is_empty() {
        return Ok(None);
    }
    let config = |name: &str| {
        says(
            dir,
            "config",
            &["--default", "", "--get"],
            &[format!("branch.{branch}.{name}")],
        )
    };
    let (merge, remote) = (config("merge")?, config("remote")?);
    if merge.starts_with("refs/pull/") {
        if remote.is_empty() {
            return Ok(None);
        }
        let remote_url = says(dir, "ls-remote", &["--get-url"], &[&remote])?;
        return Ok(Some(NamedHead::PullRef { remote_url }));
    }
    Ok((push != remote || merge == head).then_some(NamedHead::OnRemote))
}

pub(crate) fn says<S: AsRef<OsStr>>(
    dir: &str,
    command: &'static str,
    args: &[&'static str],
    names: &[S],
) -> Result<String, String> {
    says_within(dir, command, args, names, LIMIT)
}

pub(crate) fn says_within<S: AsRef<OsStr>>(
    dir: &str,
    command: &'static str,
    args: &[&'static str],
    names: &[S],
    limit: Duration,
) -> Result<String, String> {
    let asked = [command]
        .iter()
        .chain(args)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    let mut git = Git::at(dir, command);
    git.args(args.iter().copied());
    let ran = git
        .named(names)
        .run(limit)
        .map_err(|error| format!("git {asked} in {dir} could not be run: {error}"))?;
    let said = ran
        .answer()
        .map_err(|why| format!("git {asked} in {dir}: {why}"))?;
    Ok(String::from_utf8_lossy(&said).trim_end().to_owned())
}

pub fn is_clean(dir: &str) -> Result<bool, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let asked = [
        "--porcelain",
        "--untracked-files=normal",
        "--ignore-submodules",
    ];
    let said = says(dir, "status", &asked, &[] as &[&str])?;
    Ok(said.is_empty())
}

pub fn head_oid(dir: &str) -> Result<GitOid, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let said = says(dir, "rev-parse", &["--verify"], &["HEAD"])?;
    GitOid::parse(&said).map_err(|why| format!("git rev-parse HEAD in {dir}: {why}"))
}

pub fn is_ancestor(dir: &str, ancestor: &str, descendant: &str) -> Result<bool, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    assert!(!ancestor.is_empty(), "ancestor ref is non-empty");
    assert!(!descendant.is_empty(), "descendant ref is non-empty");
    let mut git = Git::at(dir, "merge-base");
    git.arg("--is-ancestor");
    let ran = git
        .named([ancestor, descendant])
        .run(LIMIT)
        .map_err(|error| {
            format!("git merge-base --is-ancestor in {dir} could not be run: {error}")
        })?;
    match ran {
        Ran::Ended { status, .. } if status.code() == Some(0) => Ok(true),
        Ran::Ended { status, .. } if status.code() == Some(1) => Ok(false),
        other => match other.answer() {
            Ok(_) => unreachable!("git merge-base --is-ancestor succeeded with an exit code not 0"),
            Err(why) => Err(format!("git merge-base --is-ancestor in {dir}: {why}")),
        },
    }
}

pub fn rebase_in_progress(dir: &str) -> Result<bool, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let said = says(
        dir,
        "rev-parse",
        &["--git-path", "rebase-merge", "--git-path", "rebase-apply"],
        &[] as &[&str],
    )?;
    let paths: Vec<&str> = said.lines().collect();
    if paths.len() != 2 {
        return Err(format!(
            "git rev-parse --git-path in {dir} returned {} paths of 2",
            paths.len()
        ));
    }
    for rel in paths {
        let full = Path::new(dir).join(rel);
        if full
            .try_exists()
            .map_err(|error| format!("{}: {error}", full.display()))?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn upstream_oid(dir: &str, branch: &str) -> Result<Option<String>, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    assert!(!branch.is_empty(), "branch name is non-empty");
    let local = format!("refs/heads/{branch}");
    let upstream = says(dir, "for-each-ref", &["--format=%(upstream)"], &[local])?;
    let upstream = upstream.trim();
    if upstream.is_empty() {
        return Ok(None);
    }
    let commit = format!("{upstream}^{{commit}}");
    let mut git = Git::at(dir, "rev-parse");
    git.args(["--verify", "--quiet"]);
    let ran = git
        .named([commit.as_str()])
        .run(LIMIT)
        .map_err(|error| format!("git rev-parse {upstream} in {dir} could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => match one_line(stdout) {
            Some(oid) => Ok(Some(oid)),
            None => Err(format!("git rev-parse {upstream} in {dir} said no oid")),
        },
        Ran::Ended { status, .. } if status.code() == Some(1) => Ok(None),
        other => match other.answer() {
            Ok(_) => unreachable!("git rev-parse --verify succeeded with an exit code not 0"),
            Err(why) => Err(format!("git rev-parse {upstream} in {dir}: {why}")),
        },
    }
}

fn push_remote(dir: &str, branch: &str) -> Result<String, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    assert!(!branch.is_empty(), "branch name is non-empty");
    for key in [
        format!("branch.{branch}.pushRemote"),
        "remote.pushDefault".to_owned(),
        format!("branch.{branch}.remote"),
    ] {
        let value = says(dir, "config", &["--default", "", "--get"], &[key])?;
        if !value.is_empty() {
            return Ok(value);
        }
    }
    let listed = says(dir, "remote", &[], &[] as &[&str])?;
    if let Some(first) = listed.lines().map(str::trim).find(|line| !line.is_empty()) {
        return Ok(first.to_owned());
    }
    Ok("origin".to_owned())
}

pub fn push_if_ahead(dir: &str) -> Result<GitOid, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    if !is_clean(dir)? {
        return Err(
            "working tree is not clean; commit or discard changes before pushing".to_owned(),
        );
    }
    let Some(branch) = branch(dir)? else {
        return Err(format!("{dir} is not on a branch"));
    };
    let head = head_oid(dir)?;
    let remote = push_remote(dir, &branch)?;
    let tracked = upstream_oid(dir, &branch)?.is_some();
    if tracked {
        let ahead_raw = says(dir, "rev-list", &["--count"], &[format!("@{{u}}..{head}")])?;
        let ahead: u64 = ahead_raw
            .parse()
            .map_err(|_| format!("git rev-list --count said {ahead_raw:?}"))?;
        if ahead == 0 {
            return Ok(head);
        }
    }
    let refspec = pushed_refspec(&head, &branch);
    let ran = Git::at(dir, "push")
        .named([remote.as_str(), refspec.as_str()])
        .run_alone(PUSH_LIMIT)
        .map_err(|error| format!("git push in {dir} could not be run: {error}"))?;
    ran.answer()
        .map_err(|why| format!("git push in {dir}: {why}"))?;
    if !tracked {
        track(dir, &branch, &remote)?;
    }
    Ok(head)
}

pub fn push_force_with_lease(dir: &str, expected_head: &GitOid) -> Result<(), String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    if !is_clean(dir)? {
        return Err(
            "working tree is not clean; commit or discard changes before pushing".to_owned(),
        );
    }
    let Some(branch) = branch(dir)? else {
        return Err(format!("{dir} is not on a branch"));
    };
    let head = head_oid(dir)?;
    if head != *expected_head {
        return Err(format!(
            "HEAD moved to {head} after the rebase was checked at {expected_head}; not pushing"
        ));
    }
    let remote = push_remote(dir, &branch)?;
    let refspec = pushed_refspec(expected_head, &branch);
    let mut git = Git::at(dir, "push");
    let upstream = upstream_oid(dir, &branch)?;
    git.leased(&branch, upstream.as_deref().unwrap_or(""));
    let ran = git
        .named([remote.as_str(), refspec.as_str()])
        .run_alone(PUSH_LIMIT)
        .map_err(|error| {
            format!("git push --force-with-lease in {dir} could not be run: {error}")
        })?;
    ran.answer()
        .map_err(|why| format!("git push --force-with-lease in {dir}: {why}"))?;
    if upstream.is_none() {
        track(dir, &branch, &remote)?;
    }
    Ok(())
}

fn pushed_refspec(head: &GitOid, branch: &str) -> String {
    assert!(!branch.is_empty(), "branch name is non-empty");
    format!("{head}:refs/heads/{branch}")
}

fn track(dir: &str, branch: &str, remote: &str) -> Result<(), String> {
    assert!(!branch.is_empty(), "branch name is non-empty");
    assert!(!remote.is_empty(), "remote name is non-empty");
    let merge = format!("refs/heads/{branch}");
    for (key, value) in [
        (format!("branch.{branch}.remote"), remote),
        (format!("branch.{branch}.merge"), merge.as_str()),
    ] {
        says(dir, "config", &[], &[key.as_str(), value])?;
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct AbsPath(PathBuf);

impl AbsPath {
    fn canonical(path: &Path) -> io::Result<Self> {
        let real = fs::canonicalize(path)?;
        assert!(real.is_absolute() && !real.as_os_str().is_empty());
        Ok(AbsPath(real))
    }

    #[cfg(test)]
    fn from_absolute(path: PathBuf) -> Option<Self> {
        path.is_absolute().then_some(AbsPath(path))
    }

    fn join(&self, part: impl AsRef<Path>) -> Self {
        let joined = self.0.join(part);
        assert!(joined.is_absolute() && !joined.as_os_str().is_empty());
        AbsPath(joined)
    }

    fn as_path(&self) -> &Path {
        &self.0
    }

    fn to_str(&self) -> Option<&str> {
        self.0.to_str()
    }

    fn is_dir(&self) -> bool {
        self.0.is_dir()
    }

    fn starts_with(&self, base: &Path) -> bool {
        self.0.starts_with(base)
    }

    fn ends_with(&self, child: impl AsRef<Path>) -> bool {
        self.0.ends_with(child)
    }

    fn display(&self) -> std::path::Display<'_> {
        self.0.display()
    }
}

impl Borrow<Path> for AbsPath {
    fn borrow(&self) -> &Path {
        &self.0
    }
}

impl AsRef<OsStr> for AbsPath {
    fn as_ref(&self) -> &OsStr {
        self.0.as_os_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct NonEmptyPathBuf(PathBuf);

impl NonEmptyPathBuf {
    fn new(path: PathBuf) -> Option<Self> {
        (!path.as_os_str().is_empty()).then_some(NonEmptyPathBuf(path))
    }

    fn as_path(&self) -> &Path {
        &self.0
    }

    fn starts_with(&self, base: &Path) -> bool {
        self.0.starts_with(base)
    }

    fn parent(&self) -> Option<&Path> {
        self.0.parent()
    }
}

impl Borrow<Path> for NonEmptyPathBuf {
    fn borrow(&self) -> &Path {
        &self.0
    }
}

#[derive(PartialEq, Eq)]
struct Place {
    top: AbsPath,
    own: AbsPath,
    common: AbsPath,
}

fn place(dir: &str) -> Result<Option<Place>, Unanswered> {
    let mut git = Git::at(dir, "rev-parse");
    git.args([
        "--path-format=absolute",
        "--show-toplevel",
        "--git-dir",
        "--git-common-dir",
    ]);
    match git.run(LIMIT) {
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
        let real = |path: &[u8]| AbsPath::canonical(Path::new(OsStr::from_bytes(path))).ok();
        Some(Place {
            top: real(top)?,
            own: real(own)?,
            common: real(common)?,
        })
    }
}

fn listed(top: &AbsPath) -> Result<Vec<u8>, String> {
    let mut git = Git::at(top, "ls-files");
    git.arg("-z");
    let ran = git.run(LIMIT).map_err(|error| {
        format!(
            "git ls-files in {} could not be run: {error}",
            top.display()
        )
    })?;
    ran.answer()
        .map_err(|why| format!("git ls-files in {}: {why}", top.display()))
}

#[derive(Clone, PartialEq, Eq)]
struct TrackedDirs(BTreeSet<AbsPath>);

impl TrackedDirs {
    #[cfg(test)]
    fn from_paths(dirs: BTreeSet<AbsPath>) -> Self {
        assert!(!dirs.is_empty() && dirs.len() <= TREE_DIRS_MAX);
        TrackedDirs(dirs)
    }

    fn get(&self, path: &Path) -> Option<&AbsPath> {
        self.0.get(path)
    }

    fn iter(&self) -> impl Iterator<Item = &AbsPath> {
        self.0.iter()
    }

    fn difference<'a>(&'a self, other: &'a TrackedDirs) -> impl Iterator<Item = &'a AbsPath> {
        self.0.difference(&other.0)
    }

    fn range_under<'a>(&'a self, root: &'a Path) -> impl Iterator<Item = &'a AbsPath> {
        self.0
            .range::<Path, _>((Bound::Included(root), Bound::Unbounded))
            .take_while(move |dir| dir.starts_with(root))
    }
}

fn tracked(top: &AbsPath, listed: &[u8]) -> Option<TrackedDirs> {
    let mut dirs = BTreeSet::from([top.clone()]);
    for path in listed.split(|byte| *byte == 0) {
        let path = Path::new(OsStr::from_bytes(path));
        for dir in path
            .ancestors()
            .skip(1)
            .take_while(|dir| !dir.as_os_str().is_empty())
        {
            if !dirs.insert(top.join(dir)) {
                break;
            }
        }
        if dirs.len() > TREE_DIRS_MAX {
            return None;
        }
    }
    Some(TrackedDirs(dirs))
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("git watch routing mutex poisoned")
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_dir())
}

fn resolve_tracked_path(path: &AbsPath) -> Option<AbsPath> {
    if !is_real_dir(path.as_path()) {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        AbsPath::canonical(path.as_path()).ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(path.clone())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct WatchAliases(BTreeMap<AbsPath, AbsPath>);

impl WatchAliases {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn get(&self, logical: &AbsPath) -> Option<&AbsPath> {
        self.0.get(logical)
    }

    fn watched_for<'a>(&'a self, logical: &'a AbsPath) -> &'a AbsPath {
        self.0.get(logical).unwrap_or(logical)
    }

    fn set(&mut self, logical: AbsPath, actual: AbsPath) {
        if actual == logical {
            self.0.remove(&logical);
        } else {
            self.0.insert(logical, actual);
        }
    }

    fn remove(&mut self, logical: &AbsPath) -> Option<AbsPath> {
        self.0.remove(logical)
    }

    fn contains_actual(&self, actual: &Path) -> bool {
        self.0.values().any(|path| path.as_path() == actual)
    }

    fn iter(&self) -> impl Iterator<Item = (&AbsPath, &AbsPath)> {
        self.0.iter()
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

fn git_folders(place: &Place) -> BTreeMap<AbsPath, RecursiveMode> {
    use RecursiveMode::{NonRecursive, Recursive};
    let folders = [&place.own, &place.common].map(|folder| (folder.clone(), NonRecursive));
    let refs = place.common.join("refs");
    let refs_real = AbsPath::canonical(refs.as_path()).unwrap_or(refs);
    let refs = [
        (refs_real, Recursive),
        (place.common.join("reftable"), NonRecursive),
        (place.own.join("reftable"), NonRecursive),
    ];
    let paths: BTreeMap<AbsPath, RecursiveMode> = folders
        .into_iter()
        .chain(refs)
        .filter(|(path, _)| path.is_dir())
        .collect();
    assert!(paths.len() <= GIT_FOLDERS_MAX);
    paths
}

fn watched_with(
    place: &Place,
    tracked: Option<&TrackedDirs>,
    resolve: impl Fn(&AbsPath) -> Option<AbsPath>,
) -> (BTreeMap<AbsPath, RecursiveMode>, WatchAliases) {
    let mut paths = BTreeMap::new();
    let mut aliases = WatchAliases::default();
    if let Some(tracked) = tracked {
        for dir in tracked.iter() {
            for candidate in [dir.clone(), dir.join(".git")] {
                if let Some(actual) = resolve(&candidate) {
                    aliases.set(candidate, actual.clone());
                    paths.insert(actual, RecursiveMode::NonRecursive);
                }
            }
        }
    }
    paths.extend(git_folders(place));
    assert!(paths.len() <= TREE_PATHS_MAX);
    (paths, aliases)
}

fn watched(
    place: &Place,
    tracked: Option<&TrackedDirs>,
) -> (BTreeMap<AbsPath, RecursiveMode>, WatchAliases) {
    watched_with(place, tracked, resolve_tracked_path)
}

fn stamp(path: &Path) -> Option<(u64, u64, SystemTime)> {
    let index = fs::metadata(path).ok()?;
    Some((index.ino(), index.len(), index.modified().ok()?))
}

fn denied(error: &notify::Error) -> bool {
    match &error.kind {
        notify::ErrorKind::Io(error) => error.kind() == io::ErrorKind::PermissionDenied,
        _ => false,
    }
}

fn wake_up(wake: &SyncSender<()>) {
    match wake.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct TreeId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct OutsideId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum WatchKey {
    Tree(TreeId),
    Outside(OutsideId),
}

#[derive(Debug)]
struct TreeWatch {
    id: TreeId,
    paths: BTreeMap<AbsPath, RecursiveMode>,
    #[cfg(debug_assertions)]
    armed: bool,
}

impl TreeWatch {
    fn new(id: TreeId, paths: BTreeMap<AbsPath, RecursiveMode>) -> Self {
        TreeWatch {
            id,
            paths,
            #[cfg(debug_assertions)]
            armed: true,
        }
    }

    fn id(&self) -> TreeId {
        self.id
    }

    fn paths(&self) -> &BTreeMap<AbsPath, RecursiveMode> {
        &self.paths
    }

    fn disarm(mut self) -> (TreeId, BTreeMap<AbsPath, RecursiveMode>) {
        #[cfg(debug_assertions)]
        {
            self.armed = false;
        }
        (self.id, mem::take(&mut self.paths))
    }
}

#[cfg(debug_assertions)]
impl Drop for TreeWatch {
    fn drop(&mut self) {
        assert!(
            !self.armed || thread::panicking(),
            "tree watch dropped without being removed from Watches"
        );
    }
}

#[derive(Debug)]
struct OutsideWatch {
    id: OutsideId,
    paths: BTreeMap<AbsPath, RecursiveMode>,
    #[cfg(debug_assertions)]
    armed: bool,
}

impl OutsideWatch {
    fn new(id: OutsideId, paths: BTreeMap<AbsPath, RecursiveMode>) -> Self {
        OutsideWatch {
            id,
            paths,
            #[cfg(debug_assertions)]
            armed: true,
        }
    }

    fn id(&self) -> OutsideId {
        self.id
    }

    fn disarm(mut self) -> (OutsideId, BTreeMap<AbsPath, RecursiveMode>) {
        #[cfg(debug_assertions)]
        {
            self.armed = false;
        }
        (self.id, mem::take(&mut self.paths))
    }
}

#[cfg(debug_assertions)]
impl Drop for OutsideWatch {
    fn drop(&mut self) {
        assert!(
            !self.armed || thread::panicking(),
            "outside watch dropped without being removed from Watches"
        );
    }
}

struct DirEvents(BTreeSet<NonEmptyPathBuf>);

impl DirEvents {
    fn single(path: NonEmptyPathBuf) -> Self {
        DirEvents(BTreeSet::from([path]))
    }

    fn insert(&mut self, path: &NonEmptyPathBuf) -> bool {
        if self.0.contains(path) {
            return true;
        }
        if self.0.len() < DIR_EVENTS_MAX {
            self.0.insert(path.clone());
            true
        } else {
            false
        }
    }

    fn iter(&self) -> impl Iterator<Item = &NonEmptyPathBuf> {
        self.0.iter()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.0.len()
    }

    #[cfg(test)]
    fn paths(&self) -> &BTreeSet<NonEmptyPathBuf> {
        &self.0
    }
}

enum Dirs {
    None,
    Under(DirEvents),
    All,
}

impl Dirs {
    fn record(&mut self, path: &NonEmptyPathBuf) {
        match self {
            Dirs::All => {}
            Dirs::None => *self = Dirs::Under(DirEvents::single(path.clone())),
            Dirs::Under(events) => {
                if !events.insert(path) {
                    *self = Dirs::All;
                }
            }
        }
    }

    fn all(&mut self) {
        *self = Dirs::All;
    }

    fn take(&mut self) -> Dirs {
        mem::replace(self, Dirs::None)
    }
}

#[derive(Default)]
struct PathIndex(BTreeMap<AbsPath, BTreeSet<WatchKey>>);

impl PathIndex {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn len(&self) -> usize {
        self.0.len()
    }

    fn contains_path(&self, path: &Path) -> bool {
        self.0.contains_key(path)
    }

    fn contains(&self, path: &Path, key: WatchKey) -> bool {
        self.0.get(path).is_some_and(|keys| keys.contains(&key))
    }

    fn get(&self, path: &Path) -> Option<&BTreeSet<WatchKey>> {
        self.0.get(path)
    }

    fn keys(&self) -> impl Iterator<Item = &AbsPath> {
        self.0.keys()
    }

    fn range_under<'a>(
        &'a self,
        root: &'a Path,
    ) -> impl Iterator<Item = (&'a AbsPath, &'a BTreeSet<WatchKey>)> {
        self.0
            .range::<Path, _>((Bound::Included(root), Bound::Unbounded))
            .take_while(move |(path, _)| path.starts_with(root))
    }

    fn strict_descendants<'a>(
        &'a self,
        root: &'a Path,
    ) -> impl Iterator<Item = (&'a AbsPath, &'a BTreeSet<WatchKey>)> {
        self.0
            .range::<Path, _>((Bound::Excluded(root), Bound::Unbounded))
            .take_while(move |(path, _)| path.starts_with(root))
    }

    fn insert(&mut self, key: WatchKey, path: &AbsPath) {
        let inserted = self.0.entry(path.clone()).or_default().insert(key);
        assert!(inserted);
    }

    fn remove(&mut self, key: WatchKey, path: &AbsPath) {
        let keys = self
            .0
            .get_mut(path)
            .expect("removed watch path must be indexed");
        assert!(keys.remove(&key));
        if keys.is_empty() {
            self.0.remove(path);
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }
}

struct TreeTarget {
    heard: bool,
    dirs: Dirs,
}

struct OutsideTarget {
    real: AbsPath,
    git: AbsPath,
    heard: bool,
    went: bool,
}

#[derive(Default)]
struct Routing {
    trees: HashMap<TreeId, TreeTarget>,
    outsides: HashMap<OutsideId, OutsideTarget>,
    direct: PathIndex,
    recursive: PathIndex,
    matched: Vec<WatchKey>,
    descendants: Vec<WatchKey>,
}

fn rescan_target(
    trees: &mut HashMap<TreeId, TreeTarget>,
    outsides: &mut HashMap<OutsideId, OutsideTarget>,
    key: WatchKey,
) {
    match key {
        WatchKey::Tree(id) => {
            let target = trees
                .get_mut(&id)
                .expect("indexed tree id must have a target");
            target.heard = true;
            target.dirs.all();
        }
        WatchKey::Outside(id) => {
            let target = outsides
                .get_mut(&id)
                .expect("indexed outside id must have a target");
            target.heard = true;
            target.went = true;
        }
    }
}

impl Routing {
    fn target_count(&self) -> usize {
        self.trees.len() + self.outsides.len()
    }

    fn is_empty(&self) -> bool {
        self.trees.is_empty() && self.outsides.is_empty()
    }

    fn mode(&self, path: &Path) -> Option<RecursiveMode> {
        if self.recursive.contains_path(path) {
            Some(RecursiveMode::Recursive)
        } else if self.direct.contains_path(path) {
            Some(RecursiveMode::NonRecursive)
        } else {
            None
        }
    }

    fn has_recursive_ancestor(&self, path: &Path) -> bool {
        if self.recursive.is_empty() {
            return false;
        }
        for ancestor in path.ancestors().skip(1) {
            if self.recursive.contains_path(ancestor) {
                return true;
            }
        }
        false
    }

    fn effective_mode(&self, path: &Path) -> Option<RecursiveMode> {
        if self.has_recursive_ancestor(path) {
            return None;
        }
        self.mode(path)
    }

    fn desired_paths(&self) -> BTreeMap<AbsPath, RecursiveMode> {
        let mut desired = BTreeMap::new();
        for path in self.recursive.keys() {
            if !self.has_recursive_ancestor(path.as_path()) {
                desired.insert(path.clone(), RecursiveMode::Recursive);
            }
        }
        for path in self.direct.keys() {
            if !desired.contains_key(path) && !self.has_recursive_ancestor(path.as_path()) {
                desired.insert(path.clone(), RecursiveMode::NonRecursive);
            }
        }
        desired
    }

    fn watches_exact(&self, key: WatchKey, path: &Path) -> bool {
        self.direct.contains(path, key) || self.recursive.contains(path, key)
    }

    fn add_path(&mut self, key: WatchKey, path: &AbsPath, mode: RecursiveMode) {
        match key {
            WatchKey::Tree(id) => assert!(self.trees.contains_key(&id)),
            WatchKey::Outside(id) => assert!(self.outsides.contains_key(&id)),
        }
        match mode {
            RecursiveMode::NonRecursive => self.direct.insert(key, path),
            RecursiveMode::Recursive => self.recursive.insert(key, path),
        }
    }

    fn remove_path(&mut self, key: WatchKey, path: &AbsPath, mode: RecursiveMode) {
        match mode {
            RecursiveMode::NonRecursive => self.direct.remove(key, path),
            RecursiveMode::Recursive => self.recursive.remove(key, path),
        }
    }

    fn rescan_subscribers(&mut self, path: &Path) -> bool {
        self.matched.clear();
        if let Some(keys) = self.direct.get(path) {
            self.matched.extend(keys.iter().copied());
        }
        if let Some(keys) = self.recursive.get(path) {
            self.matched.extend(keys.iter().copied());
        }
        if self.matched.is_empty() {
            return false;
        }
        self.matched.sort_unstable();
        self.matched.dedup();
        let Routing {
            trees,
            outsides,
            matched,
            ..
        } = self;
        for &key in &*matched {
            rescan_target(trees, outsides, key);
        }
        true
    }

    fn rescan_subtree(&mut self, root: &Path) -> bool {
        self.matched.clear();
        for (_, keys) in self.direct.range_under(root) {
            self.matched.extend(keys.iter().copied());
        }
        for (_, keys) in self.recursive.range_under(root) {
            self.matched.extend(keys.iter().copied());
        }
        if self.matched.is_empty() {
            return false;
        }
        self.matched.sort_unstable();
        self.matched.dedup();
        let Routing {
            trees,
            outsides,
            matched,
            ..
        } = self;
        for &key in &*matched {
            rescan_target(trees, outsides, key);
        }
        true
    }

    fn rescan_all(&mut self) -> bool {
        assert!(self.target_count() <= WATCHES_MAX);
        for target in self.trees.values_mut() {
            target.heard = true;
            target.dirs.all();
        }
        for target in self.outsides.values_mut() {
            target.heard = true;
            target.went = true;
        }
        !self.is_empty()
    }

    fn hear_all(&mut self, dir_changed: bool) -> bool {
        assert!(self.target_count() <= WATCHES_MAX);
        for target in self.trees.values_mut() {
            target.heard = true;
            if dir_changed {
                target.dirs.all();
            }
        }
        for target in self.outsides.values_mut() {
            target.heard = true;
        }
        !self.is_empty()
    }

    fn clear(&mut self) {
        self.trees.clear();
        self.outsides.clear();
        self.direct.clear();
        self.recursive.clear();
        self.matched.clear();
        self.descendants.clear();
    }
}

fn collect_matches(
    direct: &PathIndex,
    recursive: &PathIndex,
    path: &NonEmptyPathBuf,
    matched: &mut Vec<WatchKey>,
) -> bool {
    matched.clear();
    if let Some(keys) = direct.get(path.as_path()) {
        matched.extend(keys.iter().copied());
    }
    if let Some(parent) = path.parent()
        && let Some(keys) = direct.get(parent)
    {
        matched.extend(keys.iter().copied());
    }
    if !recursive.is_empty() {
        let mut depth = 0usize;
        for ancestor in path.as_path().ancestors() {
            if depth == PATH_ANCESTORS_MAX {
                matched.clear();
                return false;
            }
            depth += 1;
            if let Some(keys) = recursive.get(ancestor) {
                matched.extend(keys.iter().copied());
            }
        }
        assert!(depth <= PATH_ANCESTORS_MAX);
    }
    matched.sort_unstable();
    matched.dedup();
    true
}

struct EventPath {
    path: NonEmptyPathBuf,
    renamed_dir: bool,
}

enum ParsedEvent {
    Ignore,
    RescanAll,
    HearAll {
        dir_changed: bool,
    },
    Paths {
        folder_event: bool,
        rename: bool,
        removal: bool,
        paths: Vec<EventPath>,
    },
}

impl ParsedEvent {
    fn of(event: notify::Result<Event>) -> ParsedEvent {
        let Ok(event) = event else {
            return ParsedEvent::RescanAll;
        };
        if event.need_rescan() {
            return ParsedEvent::RescanAll;
        }
        if event.kind.is_access() {
            return ParsedEvent::Ignore;
        }
        if event.paths.len() > EVENT_PATHS_MAX {
            return ParsedEvent::RescanAll;
        }
        if event.paths.is_empty() {
            let dir_changed = matches!(
                event.kind,
                EventKind::Create(CreateKind::Folder)
                    | EventKind::Remove(RemoveKind::Folder)
                    | EventKind::Modify(ModifyKind::Name(_))
            );
            return ParsedEvent::HearAll { dir_changed };
        }
        let folder_event = matches!(
            event.kind,
            EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
        );
        let rename = matches!(event.kind, EventKind::Modify(ModifyKind::Name(_)));
        let removal = matches!(event.kind, EventKind::Remove(_));
        let paths: Vec<EventPath> = event
            .paths
            .into_iter()
            .filter_map(NonEmptyPathBuf::new)
            .map(|path| EventPath {
                renamed_dir: rename && path.as_path().is_dir(),
                path,
            })
            .collect();
        if paths.is_empty() {
            return ParsedEvent::Ignore;
        }
        assert!(paths.len() <= EVENT_PATHS_MAX);
        ParsedEvent::Paths {
            folder_event,
            rename,
            removal,
            paths,
        }
    }
}

fn handle_event(event: notify::Result<Event>, routing: &Mutex<Routing>, wake: &SyncSender<()>) {
    let (folder_event, rename, removal, paths) = match ParsedEvent::of(event) {
        ParsedEvent::Ignore => return,
        ParsedEvent::RescanAll => {
            if locked(routing).rescan_all() {
                wake_up(wake);
            }
            return;
        }
        ParsedEvent::HearAll { dir_changed } => {
            if locked(routing).hear_all(dir_changed) {
                wake_up(wake);
            }
            return;
        }
        ParsedEvent::Paths {
            folder_event,
            rename,
            removal,
            paths,
        } => (folder_event, rename, removal, paths),
    };
    let outside_left = removal || rename;
    let mut notified = false;
    {
        let mut routing = locked(routing);
        assert!(routing.target_count() <= WATCHES_MAX);
        for EventPath { path, renamed_dir } in paths {
            let too_deep = {
                let Routing {
                    direct,
                    recursive,
                    matched,
                    ..
                } = &mut *routing;
                !collect_matches(direct, recursive, &path, matched)
            };
            if too_deep {
                if routing.rescan_all() {
                    notified = true;
                }
                break;
            }
            let Routing {
                trees,
                outsides,
                direct,
                recursive,
                matched,
                descendants,
            } = &mut *routing;
            for &key in &*matched {
                match key {
                    WatchKey::Tree(id) => {
                        let target = trees
                            .get_mut(&id)
                            .expect("indexed tree id must have a target");
                        target.heard = true;
                        let exact = direct.contains(path.as_path(), key)
                            || recursive.contains(path.as_path(), key);
                        let record_dir =
                            folder_event || renamed_dir || ((rename || removal) && exact);
                        if record_dir {
                            target.dirs.record(&path);
                        }
                        notified = true;
                    }
                    WatchKey::Outside(id) => {
                        let target = outsides
                            .get_mut(&id)
                            .expect("indexed outside id must have a target");
                        if outside_left && path.as_path() == target.real.as_path() {
                            target.went = true;
                            target.heard = true;
                            notified = true;
                        } else if path.as_path() == target.git.as_path()
                            || path.parent() == Some(target.git.as_path())
                        {
                            target.heard = true;
                            notified = true;
                        }
                    }
                }
            }
            if removal || rename {
                descendants.clear();
                for (_, keys) in direct.strict_descendants(path.as_path()) {
                    descendants.extend(keys.iter().copied());
                }
                for (_, keys) in recursive.strict_descendants(path.as_path()) {
                    descendants.extend(keys.iter().copied());
                }
                if !descendants.is_empty() {
                    descendants.sort_unstable();
                    descendants.dedup();
                    for &key in &*descendants {
                        match key {
                            WatchKey::Tree(id) => {
                                let target = trees
                                    .get_mut(&id)
                                    .expect("indexed tree id must have a target");
                                target.heard = true;
                                target.dirs.record(&path);
                                notified = true;
                            }
                            WatchKey::Outside(id) => {
                                let target = outsides
                                    .get_mut(&id)
                                    .expect("indexed outside id must have a target");
                                target.went = true;
                                target.heard = true;
                                notified = true;
                            }
                        }
                    }
                }
            }
        }
    }
    if notified {
        wake_up(wake);
    }
}

fn make_watcher(
    routing: &Arc<Mutex<Routing>>,
    wake: &SyncSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let routing = routing.clone();
    let wake = wake.clone();
    let handler = move |event: notify::Result<Event>| {
        handle_event(event, &routing, &wake);
    };
    let config = notify::Config::default().with_follow_symlinks(false);
    RecommendedWatcher::new(handler, config)
}

enum AddOutcome {
    Watched,
    Skipped,
    Failed {
        error: notify::Error,
        needs_reset: bool,
    },
}

fn active_has_descendant(active: &BTreeMap<AbsPath, RecursiveMode>, root: &AbsPath) -> bool {
    active
        .range::<Path, _>((Bound::Excluded(root.as_path()), Bound::Unbounded))
        .next()
        .is_some_and(|(path, _)| path.starts_with(root.as_path()))
}

fn error_is_root_only(error: &notify::Error, root: &AbsPath) -> bool {
    error.paths.is_empty() || error.paths.iter().all(|path| path == root.as_path())
}

fn add_one(
    edits: &mut dyn PathsMut,
    active: &mut BTreeMap<AbsPath, RecursiveMode>,
    path: &AbsPath,
    mode: RecursiveMode,
) -> AddOutcome {
    match mode {
        RecursiveMode::NonRecursive => match edits.add(path.as_path(), mode) {
            Ok(()) => {
                active.insert(path.clone(), mode);
                assert_eq!(active.get(path), Some(&mode));
                AddOutcome::Watched
            }
            Err(error) if watch::missing(&error) => AddOutcome::Skipped,
            Err(error) if path.ends_with(".git") && denied(&error) => AddOutcome::Skipped,
            Err(error) => AddOutcome::Failed {
                error,
                needs_reset: false,
            },
        },
        RecursiveMode::Recursive => {
            for attempt in 0..RECURSIVE_ADD_RETRIES {
                match edits.add(path.as_path(), mode) {
                    Ok(()) => {
                        active.insert(path.clone(), mode);
                        assert_eq!(active.get(path), Some(&mode));
                        return AddOutcome::Watched;
                    }
                    Err(error) => {
                        let needs_reset = if active_has_descendant(active, path) {
                            true
                        } else {
                            remove_one(edits, path).is_err()
                        };
                        let root_only = error_is_root_only(&error, path);
                        if watch::missing(&error) && root_only && !path.is_dir() && !needs_reset {
                            return AddOutcome::Skipped;
                        }
                        if path.ends_with(".git") && denied(&error) && root_only && !needs_reset {
                            return AddOutcome::Skipped;
                        }
                        if watch::missing(&error)
                            && !needs_reset
                            && attempt + 1 < RECURSIVE_ADD_RETRIES
                        {
                            continue;
                        }
                        return AddOutcome::Failed { error, needs_reset };
                    }
                }
            }
            unreachable!("RECURSIVE_ADD_RETRIES is non-zero")
        }
    }
}

fn remove_one(edits: &mut dyn PathsMut, path: &AbsPath) -> notify::Result<()> {
    match edits.remove(path.as_path()) {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(error.kind, notify::ErrorKind::WatchNotFound) || watch::missing(&error) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
struct SyncRequest<'a> {
    caller: Option<WatchKey>,
    touched: &'a BTreeSet<AbsPath>,
    refresh: &'a BTreeSet<AbsPath>,
}

#[derive(Clone, Copy)]
struct Readd {
    mode: RecursiveMode,
    caller_owns: bool,
}

#[must_use]
struct RemovedOutside {
    went: bool,
    synced: notify::Result<()>,
}

struct Heard {
    trees: HashSet<TreeId>,
    outsides: HashSet<OutsideId>,
}

impl Heard {
    fn is_empty(&self) -> bool {
        self.trees.is_empty() && self.outsides.is_empty()
    }

    fn contains_tree(&self, watch: &TreeWatch) -> bool {
        self.trees.contains(&watch.id())
    }

    fn contains_outside(&self, watch: &OutsideWatch) -> bool {
        self.outsides.contains(&watch.id())
    }
}

enum WatchEngine {
    Idle,
    Running {
        watcher: RecommendedWatcher,
        active: BTreeMap<AbsPath, RecursiveMode>,
    },
    NeedsRebuild,
}

struct Watches {
    engine: WatchEngine,
    routing: Arc<Mutex<Routing>>,
    wake: SyncSender<()>,
    next_id: u64,
}

impl Watches {
    fn new(wake: SyncSender<()>) -> Self {
        Watches {
            engine: WatchEngine::Idle,
            routing: Arc::new(Mutex::new(Routing::default())),
            wake,
            next_id: 0,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .expect("watch id space exhausted");
        id
    }

    fn ensure_running(&mut self) -> notify::Result<()> {
        if matches!(self.engine, WatchEngine::Idle) {
            let watcher = make_watcher(&self.routing, &self.wake)?;
            self.engine = WatchEngine::Running {
                watcher,
                active: BTreeMap::new(),
            };
        }
        Ok(())
    }

    fn active_len(&self) -> usize {
        match &self.engine {
            WatchEngine::Running { active, .. } => active.len(),
            WatchEngine::Idle | WatchEngine::NeedsRebuild => 0,
        }
    }

    #[cfg(test)]
    fn active(&self) -> Option<&BTreeMap<AbsPath, RecursiveMode>> {
        match &self.engine {
            WatchEngine::Running { active, .. } => Some(active),
            WatchEngine::Idle | WatchEngine::NeedsRebuild => None,
        }
    }

    fn tree_count(&self) -> usize {
        locked(&self.routing).trees.len()
    }

    fn outside_count(&self) -> usize {
        locked(&self.routing).outsides.len()
    }

    fn rescan_all(&self) {
        if locked(&self.routing).rescan_all() {
            wake_up(&self.wake);
        }
    }

    fn shutdown(&mut self) {
        self.engine = WatchEngine::Idle;
        locked(&self.routing).clear();
    }

    fn take_heard(&self) -> Heard {
        let mut routing = locked(&self.routing);
        assert!(routing.target_count() <= WATCHES_MAX);
        let mut trees = HashSet::new();
        for (&id, target) in &mut routing.trees {
            if mem::take(&mut target.heard) {
                trees.insert(id);
            }
        }
        let mut outsides = HashSet::new();
        for (&id, target) in &mut routing.outsides {
            if mem::take(&mut target.heard) {
                outsides.insert(id);
            }
        }
        Heard { trees, outsides }
    }

    fn take_dirs(&self, watch: &TreeWatch) -> Dirs {
        let mut routing = locked(&self.routing);
        let target = routing
            .trees
            .get_mut(&watch.id())
            .expect("tree watch id must have a target");
        target.dirs.take()
    }

    fn went(&self, watch: &OutsideWatch) -> bool {
        let routing = locked(&self.routing);
        routing
            .outsides
            .get(&watch.id())
            .expect("outside watch id must have a target")
            .went
    }

    fn add_tree(&mut self, paths: BTreeMap<AbsPath, RecursiveMode>) -> notify::Result<TreeWatch> {
        assert!(paths.len() <= TREE_PATHS_MAX);
        self.ensure_running()?;
        let id = TreeId(self.next_id());
        let key = WatchKey::Tree(id);
        {
            let mut routing = locked(&self.routing);
            assert!(routing.target_count() < WATCHES_MAX);
            let replaced = routing.trees.insert(
                id,
                TreeTarget {
                    heard: false,
                    dirs: Dirs::None,
                },
            );
            assert!(replaced.is_none());
            for (path, &mode) in &paths {
                routing.add_path(key, path, mode);
            }
        }
        let touched: BTreeSet<AbsPath> = paths.keys().cloned().collect();
        let watch = TreeWatch::new(id, paths);
        let request = SyncRequest {
            caller: Some(key),
            touched: &touched,
            refresh: &BTreeSet::new(),
        };
        if let Err(error) = self.sync(request) {
            if self.remove_tree(watch).is_err() {
                self.rescan_all();
            }
            return Err(error);
        }
        Ok(watch)
    }

    fn add_outside(&mut self, ground: &Ground, refresh: bool) -> notify::Result<OutsideWatch> {
        self.ensure_running()?;
        let id = OutsideId(self.next_id());
        let key = WatchKey::Outside(id);
        let git = ground.real.join(".git");
        let mut paths = BTreeMap::from([(ground.real.clone(), RecursiveMode::NonRecursive)]);
        if ground.git {
            paths.insert(git.clone(), RecursiveMode::NonRecursive);
        }
        assert!(paths.len() <= 2);
        let refreshed = if refresh {
            paths.keys().cloned().collect()
        } else {
            BTreeSet::new()
        };
        {
            let mut routing = locked(&self.routing);
            assert!(routing.target_count() < WATCHES_MAX);
            let target = OutsideTarget {
                real: ground.real.clone(),
                git,
                heard: false,
                went: false,
            };
            let replaced = routing.outsides.insert(id, target);
            assert!(replaced.is_none());
            for (path, &mode) in &paths {
                routing.add_path(key, path, mode);
            }
        }
        let touched: BTreeSet<AbsPath> = paths.keys().cloned().collect();
        let watch = OutsideWatch::new(id, paths);
        let request = SyncRequest {
            caller: Some(key),
            touched: &touched,
            refresh: &refreshed,
        };
        if let Err(error) = self.sync(request) {
            let RemovedOutside { synced, .. } = self.remove_outside(watch);
            if synced.is_err() {
                self.rescan_all();
            }
            return Err(error);
        }
        Ok(watch)
    }

    fn update_tree(
        &mut self,
        mut watch: TreeWatch,
        new_paths: BTreeMap<AbsPath, RecursiveMode>,
        refresh: &BTreeSet<AbsPath>,
    ) -> notify::Result<TreeWatch> {
        assert!(new_paths.len() <= TREE_PATHS_MAX);
        let id = watch.id();
        let key = WatchKey::Tree(id);
        let old_paths = mem::replace(&mut watch.paths, new_paths);
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for (path, &old_mode) in &old_paths {
            if watch.paths.get(path) != Some(&old_mode) {
                removed.push((path.clone(), old_mode));
            }
        }
        for (path, &new_mode) in &watch.paths {
            if old_paths.get(path) != Some(&new_mode) {
                added.push((path.clone(), new_mode));
            }
        }
        if !removed.is_empty() || !added.is_empty() {
            let mut routing = locked(&self.routing);
            assert!(routing.trees.contains_key(&id));
            for (path, mode) in &removed {
                routing.remove_path(key, path, *mode);
            }
            for (path, mode) in &added {
                routing.add_path(key, path, *mode);
            }
        }
        let mut touched = BTreeSet::new();
        for (path, _) in removed {
            touched.insert(path);
        }
        for (path, _) in added {
            touched.insert(path);
        }
        for path in refresh {
            touched.insert(path.clone());
        }
        let request = SyncRequest {
            caller: Some(key),
            touched: &touched,
            refresh,
        };
        if let Err(error) = self.sync(request) {
            if self.remove_tree(watch).is_err() {
                self.rescan_all();
            }
            return Err(error);
        }
        Ok(watch)
    }

    fn remove_tree(&mut self, watch: TreeWatch) -> notify::Result<()> {
        let (id, paths) = watch.disarm();
        let key = WatchKey::Tree(id);
        {
            let mut routing = locked(&self.routing);
            for (path, &mode) in &paths {
                routing.remove_path(key, path, mode);
            }
            let removed = routing.trees.remove(&id);
            assert!(removed.is_some());
        }
        let touched: BTreeSet<AbsPath> = paths.into_keys().collect();
        self.sync(SyncRequest {
            caller: None,
            touched: &touched,
            refresh: &BTreeSet::new(),
        })
    }

    fn remove_outside(&mut self, watch: OutsideWatch) -> RemovedOutside {
        let (id, paths) = watch.disarm();
        let key = WatchKey::Outside(id);
        let went = {
            let mut routing = locked(&self.routing);
            for (path, &mode) in &paths {
                routing.remove_path(key, path, mode);
            }
            routing
                .outsides
                .remove(&id)
                .expect("outside watch id must have a target")
                .went
        };
        let touched: BTreeSet<AbsPath> = paths.into_keys().collect();
        let synced = self.sync(SyncRequest {
            caller: None,
            touched: &touched,
            refresh: &BTreeSet::new(),
        });
        RemovedOutside { went, synced }
    }

    fn rebuild(&mut self, caller: Option<WatchKey>) -> notify::Result<()> {
        let desired = {
            let routing = locked(&self.routing);
            if routing.is_empty() {
                self.engine = WatchEngine::Idle;
                return Ok(());
            }
            routing.desired_paths()
        };
        let mut watcher = make_watcher(&self.routing, &self.wake)?;
        let mut active = BTreeMap::new();
        let mut failed_paths = Vec::new();
        let mut reset_error = None;
        let committed = {
            let mut edits = watcher.paths_mut();
            for (path, mode) in desired {
                match add_one(&mut *edits, &mut active, &path, mode) {
                    AddOutcome::Watched | AddOutcome::Skipped => {}
                    AddOutcome::Failed {
                        error,
                        needs_reset: true,
                    } => {
                        reset_error = Some(error);
                        break;
                    }
                    AddOutcome::Failed {
                        error,
                        needs_reset: false,
                    } => {
                        failed_paths.push((path, mode, error));
                    }
                }
            }
            if reset_error.is_none() {
                edits.commit()
            } else {
                Ok(())
            }
        };
        if let Some(error) = reset_error.or_else(|| committed.err()) {
            self.engine = WatchEngine::NeedsRebuild;
            self.rescan_all();
            return Err(error);
        }
        self.engine = WatchEngine::Running { watcher, active };
        let mut caller_error = None;
        let mut rescanned = false;
        if !failed_paths.is_empty() {
            let mut routing = locked(&self.routing);
            for (path, mode, error) in failed_paths {
                let notified = match mode {
                    RecursiveMode::Recursive => routing.rescan_subtree(path.as_path()),
                    RecursiveMode::NonRecursive => routing.rescan_subscribers(path.as_path()),
                };
                if notified {
                    rescanned = true;
                }
                let blames_caller =
                    caller.is_none_or(|key| routing.watches_exact(key, path.as_path()));
                if blames_caller {
                    caller_error.get_or_insert(error);
                }
            }
        }
        if rescanned {
            wake_up(&self.wake);
        }
        if let Some(error) = caller_error {
            return Err(error);
        }
        Ok(())
    }

    fn sync(&mut self, request: SyncRequest<'_>) -> notify::Result<()> {
        if matches!(self.engine, WatchEngine::NeedsRebuild) {
            return self.rebuild(request.caller);
        }
        let mut to_remove = BTreeMap::new();
        let mut readd = BTreeMap::new();
        let mut fresh_add = BTreeMap::new();
        let mut wiped = BTreeSet::new();
        let active_bound = {
            let routing = locked(&self.routing);
            let indexed = routing.direct.len() + routing.recursive.len();
            assert!(self.active_len() <= indexed + request.touched.len());
            let active = match &self.engine {
                WatchEngine::Running { active, .. } => Some(active),
                WatchEngine::Idle | WatchEngine::NeedsRebuild => None,
            };
            let mut recursive_added = Vec::new();
            let mut uncover_roots = BTreeSet::new();
            for path in request.touched {
                let was = active.and_then(|active| active.get(path).copied());
                let want = routing.effective_mode(path.as_path());
                match (was, want) {
                    (Some(old_mode), None) => {
                        to_remove.insert(path.clone(), old_mode);
                    }
                    (Some(old_mode), Some(new_mode)) => {
                        if old_mode != new_mode || request.refresh.contains(path) {
                            to_remove.insert(path.clone(), old_mode);
                            let caller_owns = request
                                .caller
                                .is_none_or(|key| routing.watches_exact(key, path.as_path()));
                            readd.insert(
                                path.clone(),
                                Readd {
                                    mode: new_mode,
                                    caller_owns,
                                },
                            );
                            if new_mode == RecursiveMode::Recursive {
                                recursive_added.push(path.clone());
                            }
                        }
                    }
                    (None, Some(new_mode)) => {
                        fresh_add.insert(path.clone(), new_mode);
                        if new_mode == RecursiveMode::Recursive {
                            recursive_added.push(path.clone());
                        }
                    }
                    (None, None) => {}
                }
                if want != Some(RecursiveMode::Recursive)
                    && !routing.has_recursive_ancestor(path.as_path())
                {
                    uncover_roots.insert(path.clone());
                }
            }
            if let Some(active) = active {
                for root in &recursive_added {
                    for (descendant, &old_mode) in active
                        .range::<Path, _>((Bound::Excluded(root.as_path()), Bound::Unbounded))
                        .take_while(|(path, _)| path.starts_with(root.as_path()))
                    {
                        to_remove.insert(descendant.clone(), old_mode);
                        readd.remove(descendant);
                        fresh_add.remove(descendant);
                    }
                }
                for (root, &old_mode) in &to_remove {
                    if old_mode != RecursiveMode::Recursive {
                        continue;
                    }
                    uncover_roots.insert(root.clone());
                    for (descendant, _) in active
                        .range::<Path, _>((Bound::Excluded(root.as_path()), Bound::Unbounded))
                        .take_while(|(path, _)| path.starts_with(root.as_path()))
                    {
                        wiped.insert(descendant.clone());
                    }
                }
            }
            for root in &uncover_roots {
                let direct_desc = routing
                    .direct
                    .strict_descendants(root.as_path())
                    .map(|(path, _)| path);
                let recursive_desc = routing
                    .recursive
                    .strict_descendants(root.as_path())
                    .map(|(path, _)| path);
                for descendant in direct_desc.chain(recursive_desc) {
                    let Some(mode) = routing.effective_mode(descendant.as_path()) else {
                        continue;
                    };
                    let was = active.and_then(|active| active.get(descendant).copied());
                    if wiped.contains(descendant) || was != Some(mode) {
                        if let Some(old_mode) = was
                            && !wiped.contains(descendant)
                        {
                            to_remove.insert(descendant.clone(), old_mode);
                        }
                        fresh_add.remove(descendant);
                        let caller_owns = request
                            .caller
                            .is_none_or(|key| routing.watches_exact(key, descendant.as_path()));
                        readd.insert(descendant.clone(), Readd { mode, caller_owns });
                    }
                }
            }
            indexed
        };
        if to_remove.is_empty() && readd.is_empty() && fresh_add.is_empty() {
            assert!(self.active_len() <= active_bound);
            return Ok(());
        }
        self.ensure_running()?;
        let mut needs_reset = false;
        let mut failed = None;
        let mut readd_failed_paths = Vec::new();
        {
            let WatchEngine::Running { watcher, active } = &mut self.engine else {
                unreachable!("ensure_running guarantees WatchEngine::Running");
            };
            let mut edits = watcher.paths_mut();
            for path in to_remove.keys() {
                match remove_one(&mut *edits, path) {
                    Ok(()) => {
                        active.remove(path);
                    }
                    Err(error) => {
                        needs_reset = true;
                        failed.get_or_insert(error);
                    }
                }
            }
            for path in &wiped {
                active.remove(path);
            }
            if !needs_reset {
                for (path, &Readd { mode, caller_owns }) in &readd {
                    match add_one(&mut *edits, active, path, mode) {
                        AddOutcome::Watched | AddOutcome::Skipped => {}
                        AddOutcome::Failed {
                            error,
                            needs_reset: true,
                        } => {
                            needs_reset = true;
                            failed.get_or_insert(error);
                            break;
                        }
                        AddOutcome::Failed {
                            error,
                            needs_reset: false,
                        } => {
                            readd_failed_paths.push((path.clone(), mode));
                            if caller_owns {
                                failed.get_or_insert(error);
                            }
                        }
                    }
                }
                if !needs_reset && failed.is_none() {
                    for (path, &mode) in &fresh_add {
                        match add_one(&mut *edits, active, path, mode) {
                            AddOutcome::Watched | AddOutcome::Skipped => {}
                            AddOutcome::Failed {
                                error,
                                needs_reset: reset,
                            } => {
                                if reset {
                                    needs_reset = true;
                                }
                                failed = Some(error);
                                break;
                            }
                        }
                    }
                }
            }
            if !needs_reset && let Err(error) = edits.commit() {
                needs_reset = true;
                failed.get_or_insert(error);
            }
        }
        if needs_reset {
            if locked(&self.routing).is_empty() {
                self.engine = WatchEngine::Idle;
            } else {
                self.engine = WatchEngine::NeedsRebuild;
                self.rescan_all();
            }
        } else if !readd_failed_paths.is_empty() {
            let mut rescanned = false;
            {
                let mut routing = locked(&self.routing);
                for (path, mode) in &readd_failed_paths {
                    let notified = match mode {
                        RecursiveMode::Recursive => routing.rescan_subtree(path.as_path()),
                        RecursiveMode::NonRecursive => routing.rescan_subscribers(path.as_path()),
                    };
                    if notified {
                        rescanned = true;
                    }
                }
            }
            if rescanned {
                wake_up(&self.wake);
            }
        }
        assert!(self.active_len() <= active_bound);
        if let Some(error) = failed {
            return Err(error);
        }
        Ok(())
    }
}

fn tracked_match(path: &Path, tracked: &TrackedDirs) -> Option<AbsPath> {
    if let Some(dir) = tracked.get(path) {
        return Some(dir.clone());
    }
    if path.file_name() == Some(OsStr::new(".git"))
        && let Some(parent) = path.parent().and_then(|parent| tracked.get(parent))
    {
        return Some(parent.join(".git"));
    }
    None
}

fn is_tracked(path: &Path, tracked: Option<&TrackedDirs>) -> bool {
    tracked.is_some_and(|tracked| tracked_match(path, tracked).is_some())
}

struct Reconcile<'a, F> {
    place: &'a Place,
    old_tracked: Option<&'a TrackedDirs>,
    new_tracked: Option<&'a TrackedDirs>,
    old_paths: &'a BTreeMap<AbsPath, RecursiveMode>,
    git_paths: BTreeMap<AbsPath, RecursiveMode>,
    new_paths: BTreeMap<AbsPath, RecursiveMode>,
    aliases: WatchAliases,
    refresh: BTreeSet<AbsPath>,
    resolve: F,
    scan_unwatched_aliases: bool,
}

impl<F: Fn(&AbsPath) -> Option<AbsPath>> Reconcile<'_, F> {
    fn is_tracked_actual(&self, actual: &Path) -> bool {
        is_tracked(actual, self.new_tracked) || self.aliases.contains_actual(actual)
    }

    fn remove_tracked_dir(&mut self, dir: &AbsPath) {
        for candidate in [dir.clone(), dir.join(".git")] {
            let watched = self.aliases.remove(&candidate).unwrap_or(candidate);
            if !self.git_paths.contains_key(&watched) && !self.is_tracked_actual(watched.as_path())
            {
                self.new_paths.remove(&watched);
            }
        }
    }

    fn add_tracked_dir(&mut self, dir: &AbsPath) {
        for candidate in [dir.clone(), dir.join(".git")] {
            if let Some(actual) = (self.resolve)(&candidate) {
                self.aliases.set(candidate, actual.clone());
                self.new_paths
                    .entry(actual)
                    .or_insert(RecursiveMode::NonRecursive);
            }
        }
    }

    fn reconcile_tracked(&mut self) {
        match (self.old_tracked, self.new_tracked) {
            (Some(old), Some(now)) if old == now => return,
            (None, None) => return,
            (Some(old), Some(now)) => {
                for removed in old.difference(now) {
                    self.remove_tracked_dir(removed);
                }
                for added in now.difference(old) {
                    self.add_tracked_dir(added);
                }
            }
            (Some(old), None) => {
                for removed in old.iter() {
                    self.remove_tracked_dir(removed);
                }
                self.aliases.clear();
            }
            (None, Some(now)) => {
                for added in now.iter() {
                    self.add_tracked_dir(added);
                }
            }
        }
        assert!(self.new_paths.len() <= TREE_PATHS_MAX);
    }

    fn reconcile_git_folders(&mut self, affected: Option<&DirEvents>) {
        assert!(self.git_paths.len() <= GIT_FOLDERS_MAX);
        let stale_recursive: Vec<AbsPath> = self
            .new_paths
            .iter()
            .filter(|(path, mode)| {
                **mode == RecursiveMode::Recursive && !self.git_paths.contains_key(*path)
            })
            .map(|(path, _)| path.clone())
            .collect();
        for path in stale_recursive {
            if self.is_tracked_actual(path.as_path()) && is_real_dir(path.as_path()) {
                self.new_paths.insert(path, RecursiveMode::NonRecursive);
            } else {
                self.new_paths.remove(&path);
            }
        }
        let known = [
            self.place.own.clone(),
            self.place.common.clone(),
            self.place.common.join("reftable"),
            self.place.own.join("reftable"),
        ];
        for path in known {
            if !self.git_paths.contains_key(&path) && !self.is_tracked_actual(path.as_path()) {
                self.new_paths.remove(&path);
            }
        }
        let refs_link = self.place.common.join("refs");
        let refs_touched = affected
            .is_some_and(|dirs| dirs.iter().any(|dir| refs_link.starts_with(dir.as_path())));
        for (path, &mode) in &self.git_paths {
            let was_present = self.old_paths.contains_key(path);
            self.new_paths.insert(path.clone(), mode);
            if !was_present {
                continue;
            }
            let Some(affected) = affected else {
                continue;
            };
            let touched = if affected.iter().any(|dir| path.starts_with(dir.as_path())) {
                true
            } else {
                mode == RecursiveMode::Recursive && refs_touched
            };
            if touched {
                self.refresh.insert(path.clone());
            }
        }
        assert!(self.new_paths.len() <= TREE_PATHS_MAX);
    }

    fn reconcile_dirs(&mut self, affected: &DirEvents) {
        let mut roots: Vec<&NonEmptyPathBuf> = Vec::new();
        for affected_path in affected.iter() {
            if roots
                .last()
                .is_some_and(|root| affected_path.starts_with(root.as_path()))
            {
                continue;
            }
            roots.push(affected_path);
        }
        let mut logical_candidates = BTreeSet::new();
        let mut existing_candidates = BTreeSet::new();
        for root in &roots {
            let root_path = root.as_path();
            for (path, &mode) in self
                .new_paths
                .range::<Path, _>((Bound::Included(root_path), Bound::Unbounded))
                .take_while(|(path, _)| path.starts_with(root_path))
            {
                if self.git_paths.contains_key(path) {
                    continue;
                }
                assert_eq!(mode, RecursiveMode::NonRecursive);
                existing_candidates.insert(path.clone());
            }
            if let Some(tracked) = self.new_tracked {
                for dir in tracked.range_under(root_path) {
                    for candidate in [dir.clone(), dir.join(".git")] {
                        if !self.git_paths.contains_key(&candidate) {
                            logical_candidates.insert(candidate);
                        }
                    }
                }
                if let Some(matched) = tracked_match(root_path, tracked)
                    && !self.git_paths.contains_key(&matched)
                {
                    logical_candidates.insert(matched);
                }
                if !self.aliases.is_empty() {
                    let alias_matches: Vec<(AbsPath, AbsPath)> = self
                        .aliases
                        .iter()
                        .filter(|(_, actual)| {
                            actual.starts_with(root_path) || root_path.starts_with(actual.as_path())
                        })
                        .map(|(logical, actual)| (logical.clone(), actual.clone()))
                        .collect();
                    for (logical, actual) in alias_matches {
                        if actual.starts_with(root_path) {
                            logical_candidates.insert(logical.clone());
                            for dir in tracked.range_under(logical.as_path()) {
                                logical_candidates.insert(dir.clone());
                                logical_candidates.insert(dir.join(".git"));
                            }
                        } else if let Ok(rel) = root_path.strip_prefix(actual.as_path()) {
                            let mapped = logical.join(rel);
                            for dir in tracked.range_under(mapped.as_path()) {
                                logical_candidates.insert(dir.clone());
                                logical_candidates.insert(dir.join(".git"));
                            }
                            if let Some(matched) = tracked_match(mapped.as_path(), tracked) {
                                logical_candidates.insert(matched);
                            }
                        }
                    }
                }
            }
        }
        if self.scan_unwatched_aliases
            && let Some(tracked) = self.new_tracked
            && roots
                .iter()
                .any(|root| root.starts_with(self.place.top.as_path()))
        {
            for dir in tracked.iter() {
                for candidate in [dir.clone(), dir.join(".git")] {
                    if self.git_paths.contains_key(&candidate)
                        || logical_candidates.contains(&candidate)
                    {
                        continue;
                    }
                    let watched = self.aliases.watched_for(&candidate);
                    if self.new_paths.contains_key(watched) {
                        continue;
                    }
                    if let Some(actual) = (self.resolve)(&candidate)
                        && roots.iter().any(|root| actual.starts_with(root.as_path()))
                    {
                        logical_candidates.insert(candidate);
                    }
                }
            }
        }
        let mut resolved_actuals = BTreeSet::new();
        for logical in logical_candidates {
            let previous = self.aliases.get(&logical).cloned();
            if let Some(actual) = (self.resolve)(&logical) {
                self.aliases.set(logical, actual.clone());
                if let Some(old_actual) = previous
                    && old_actual != actual
                    && !self.git_paths.contains_key(&old_actual)
                    && !self.is_tracked_actual(old_actual.as_path())
                {
                    self.new_paths.remove(&old_actual);
                }
                if !self.git_paths.contains_key(&actual) {
                    if self.old_paths.contains_key(&actual) {
                        self.refresh.insert(actual.clone());
                    }
                    self.new_paths
                        .insert(actual.clone(), RecursiveMode::NonRecursive);
                    resolved_actuals.insert(actual);
                }
            } else {
                let watched = self.aliases.remove(&logical).unwrap_or(logical);
                if !self.git_paths.contains_key(&watched)
                    && !self.is_tracked_actual(watched.as_path())
                {
                    self.new_paths.remove(&watched);
                }
            }
        }
        for existing in existing_candidates {
            if resolved_actuals.contains(&existing) || !self.new_paths.contains_key(&existing) {
                continue;
            }
            if is_real_dir(existing.as_path()) {
                if self.old_paths.contains_key(&existing) {
                    self.refresh.insert(existing.clone());
                }
                self.new_paths.insert(existing, RecursiveMode::NonRecursive);
            } else {
                self.new_paths.remove(&existing);
            }
        }
        assert!(self.new_paths.len() <= TREE_PATHS_MAX);
    }
}

struct Reconciled {
    paths: BTreeMap<AbsPath, RecursiveMode>,
    aliases: WatchAliases,
    refresh: BTreeSet<AbsPath>,
}

fn reconcile_with(
    place: &Place,
    old_tracked: Option<&TrackedDirs>,
    old_paths: &BTreeMap<AbsPath, RecursiveMode>,
    old_aliases: &WatchAliases,
    new_tracked: Option<&TrackedDirs>,
    dirs: Dirs,
    resolve: impl Fn(&AbsPath) -> Option<AbsPath>,
    scan_unwatched_aliases: bool,
) -> Option<Reconciled> {
    assert!(old_paths.len() <= TREE_PATHS_MAX);
    let result = match dirs {
        Dirs::None => {
            if old_tracked == new_tracked {
                return None;
            }
            let mut state = Reconcile {
                place,
                old_tracked,
                new_tracked,
                old_paths,
                git_paths: git_folders(place),
                new_paths: old_paths.clone(),
                aliases: old_aliases.clone(),
                refresh: BTreeSet::new(),
                resolve,
                scan_unwatched_aliases,
            };
            state.reconcile_git_folders(None);
            state.reconcile_tracked();
            if state.new_paths == *old_paths && state.aliases == *old_aliases {
                None
            } else {
                Some(Reconciled {
                    paths: state.new_paths,
                    aliases: state.aliases,
                    refresh: state.refresh,
                })
            }
        }
        Dirs::All => {
            let (paths, aliases) = watched_with(place, new_tracked, resolve);
            let refresh = paths
                .keys()
                .filter(|path| old_paths.contains_key(*path))
                .cloned()
                .collect();
            Some(Reconciled {
                paths,
                aliases,
                refresh,
            })
        }
        Dirs::Under(affected) => {
            let mut state = Reconcile {
                place,
                old_tracked,
                new_tracked,
                old_paths,
                git_paths: git_folders(place),
                new_paths: old_paths.clone(),
                aliases: old_aliases.clone(),
                refresh: BTreeSet::new(),
                resolve,
                scan_unwatched_aliases,
            };
            state.reconcile_git_folders(Some(&affected));
            state.reconcile_tracked();
            state.reconcile_dirs(&affected);
            if state.new_paths == *old_paths
                && state.aliases == *old_aliases
                && state.refresh.is_empty()
            {
                None
            } else {
                Some(Reconciled {
                    paths: state.new_paths,
                    aliases: state.aliases,
                    refresh: state.refresh,
                })
            }
        }
    };
    if let Some(ref reconciled) = result {
        assert!(reconciled.paths.len() <= TREE_PATHS_MAX);
    }
    result
}

fn reconcile(
    place: &Place,
    old_tracked: Option<&TrackedDirs>,
    old_paths: &BTreeMap<AbsPath, RecursiveMode>,
    old_aliases: &WatchAliases,
    new_tracked: Option<&TrackedDirs>,
    dirs: Dirs,
) -> Option<Reconciled> {
    reconcile_with(
        place,
        old_tracked,
        old_paths,
        old_aliases,
        new_tracked,
        dirs,
        resolve_tracked_path,
        cfg!(target_os = "macos"),
    )
}

struct Tree {
    place: Place,
    index: Option<(u64, u64, SystemTime)>,
    watching: Watching,
}

#[derive(Clone, PartialEq, Eq)]
enum FoldersOnly {
    TooManyDirs,
    Unlisted(String),
}

impl FoldersOnly {
    fn trouble(&self, top: &AbsPath) -> String {
        match self {
            FoldersOnly::TooManyDirs => format!(
                "{} has more than {TREE_DIRS_MAX} directories that git tracks a path \
                 in, and only git's folders are watched",
                top.display(),
            ),
            FoldersOnly::Unlisted(why) => why.clone(),
        }
    }
}

enum Tracked {
    Listed(TrackedDirs),
    FoldersOnly(FoldersOnly),
}

impl Tracked {
    fn dirs(&self) -> Option<&TrackedDirs> {
        match self {
            Tracked::Listed(dirs) => Some(dirs),
            Tracked::FoldersOnly(_) => None,
        }
    }
}

enum Watching {
    Whole {
        id: TreeWatch,
        tracked: TrackedDirs,
        aliases: WatchAliases,
    },
    Folders {
        id: TreeWatch,
        why: FoldersOnly,
    },
    Sweep {
        why: String,
    },
}

impl Watching {
    fn id(&self) -> Option<&TreeWatch> {
        match self {
            Watching::Whole { id, .. } | Watching::Folders { id, .. } => Some(id),
            Watching::Sweep { .. } => None,
        }
    }

    fn into_id(self) -> Option<TreeWatch> {
        match self {
            Watching::Whole { id, .. } | Watching::Folders { id, .. } => Some(id),
            Watching::Sweep { .. } => None,
        }
    }

    fn trouble(&self, top: &AbsPath) -> Option<String> {
        match self {
            Watching::Whole { .. } => None,
            Watching::Folders { why, .. } => Some(why.trouble(top)),
            Watching::Sweep { why } => Some(why.clone()),
        }
    }
}

enum ReuseTrigger {
    DirsOnly(DirEvents),
    Relist(Dirs),
}

struct TreeReuse {
    watch: TreeWatch,
    previous: Tracked,
    aliases: WatchAliases,
    trigger: ReuseTrigger,
}

fn list_tracked(place: &Place) -> Tracked {
    let listed = match listed(&place.top) {
        Ok(listed) => listed,
        Err(why) => return Tracked::FoldersOnly(FoldersOnly::Unlisted(why)),
    };
    match tracked(&place.top, &listed) {
        Some(dirs) => Tracked::Listed(dirs),
        None => Tracked::FoldersOnly(FoldersOnly::TooManyDirs),
    }
}

fn note_watch_trouble(watches: &Watches, troubles: &mut Troubles, synced: notify::Result<()>) {
    if let Err(error) = synced {
        troubles.say(format!("git watch cleanup failed: {error}"));
        watches.rescan_all();
    }
}

impl Tree {
    fn watch(
        before: Option<Tree>,
        place: Place,
        watches: &mut Watches,
        troubles: &mut Troubles,
    ) -> (Tree, bool) {
        let index = stamp(place.own.join("index").as_path());
        let reuse = match before {
            Some(tree) => {
                let dirs = tree
                    .watching
                    .id()
                    .map_or(Dirs::None, |id| watches.take_dirs(id));
                let same_place = tree.place == place;
                let same_index = tree.index == index;
                let trigger = if same_place {
                    match (same_index, dirs) {
                        (true, Dirs::None) => return (tree, false),
                        (true, Dirs::Under(affected)) => Some(ReuseTrigger::DirsOnly(affected)),
                        (true, Dirs::All) => Some(ReuseTrigger::Relist(Dirs::All)),
                        (false, dirs) => Some(ReuseTrigger::Relist(dirs)),
                    }
                } else {
                    None
                };
                match (tree.watching, trigger) {
                    (
                        Watching::Whole {
                            id,
                            tracked,
                            aliases,
                        },
                        Some(trigger),
                    ) => Some(TreeReuse {
                        watch: id,
                        previous: Tracked::Listed(tracked),
                        aliases,
                        trigger,
                    }),
                    (Watching::Folders { id, why }, Some(trigger)) => Some(TreeReuse {
                        watch: id,
                        previous: Tracked::FoldersOnly(why),
                        aliases: WatchAliases::default(),
                        trigger,
                    }),
                    (Watching::Whole { id, .. } | Watching::Folders { id, .. }, None) => {
                        let synced = watches.remove_tree(id);
                        note_watch_trouble(watches, troubles, synced);
                        None
                    }
                    (Watching::Sweep { .. }, _) => None,
                }
            }
            None => None,
        };
        let tracked = match &reuse {
            Some(TreeReuse {
                previous: Tracked::Listed(old_tracked),
                trigger: ReuseTrigger::DirsOnly(_),
                ..
            }) => Tracked::Listed(old_tracked.clone()),
            Some(TreeReuse {
                previous: Tracked::FoldersOnly(FoldersOnly::TooManyDirs),
                trigger: ReuseTrigger::DirsOnly(_),
                ..
            }) => Tracked::FoldersOnly(FoldersOnly::TooManyDirs),
            _ => list_tracked(&place),
        };
        let (watch, aliases, anew) = if let Some(reuse) = reuse {
            let dirs = match reuse.trigger {
                ReuseTrigger::DirsOnly(affected) => Dirs::Under(affected),
                ReuseTrigger::Relist(dirs) => dirs,
            };
            let update = reconcile(
                &place,
                reuse.previous.dirs(),
                reuse.watch.paths(),
                &reuse.aliases,
                tracked.dirs(),
                dirs,
            );
            match update {
                Some(Reconciled {
                    paths,
                    aliases,
                    refresh,
                }) => {
                    let changed = paths != *reuse.watch.paths() || !refresh.is_empty();
                    let updated = if changed {
                        watches.update_tree(reuse.watch, paths, &refresh)
                    } else {
                        Ok(reuse.watch)
                    };
                    (updated, aliases, changed)
                }
                None => (Ok(reuse.watch), reuse.aliases, false),
            }
        } else {
            let (paths, aliases) = watched(&place, tracked.dirs());
            (watches.add_tree(paths), aliases, true)
        };
        let watching = match (watch, tracked) {
            (Ok(id), Tracked::Listed(tracked)) => Watching::Whole {
                id,
                tracked,
                aliases,
            },
            (Ok(id), Tracked::FoldersOnly(why)) => Watching::Folders { id, why },
            (Err(error), _) => Watching::Sweep {
                why: format!(
                    "{} is read on the sweep alone: {error}",
                    place.top.display()
                ),
            },
        };
        if let Some(trouble) = watching.trouble(&place.top) {
            troubles.say(trouble);
        }
        assert_eq!(
            watching.id().is_some(),
            !matches!(watching, Watching::Sweep { .. })
        );
        (
            Tree {
                place,
                index,
                watching,
            },
            anew,
        )
    }
}

struct Outside {
    watching: Beginning,
}

#[derive(PartialEq, Eq)]
struct Ground {
    real: AbsPath,
    git: bool,
}

impl Ground {
    fn of(dir: &Path) -> io::Result<Option<Ground>> {
        let real = match AbsPath::canonical(dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            real => real?,
        };
        let git = real.join(".git").is_dir();
        Ok(Some(Ground { real, git }))
    }
}

enum Beginning {
    Watched { id: OutsideWatch, ground: Ground },
    Gone,
    Sweep { ground: Option<Ground>, why: String },
}

impl Beginning {
    fn id(&self) -> Option<&OutsideWatch> {
        match self {
            Beginning::Watched { id, .. } => Some(id),
            Beginning::Gone | Beginning::Sweep { .. } => None,
        }
    }

    fn into_id(self) -> Option<OutsideWatch> {
        match self {
            Beginning::Watched { id, .. } => Some(id),
            Beginning::Gone | Beginning::Sweep { .. } => None,
        }
    }

    fn covers(&self, now: &io::Result<Option<Ground>>, watches: &Watches) -> bool {
        match (self, now) {
            (Beginning::Watched { id, ground }, Ok(Some(now))) => {
                if watches.went(id) {
                    false
                } else {
                    ground == now
                }
            }
            (
                Beginning::Sweep {
                    ground: Some(ground),
                    ..
                },
                Ok(Some(now)),
            ) => ground == now,
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
        dir: &Dir,
        before: Option<Outside>,
        watches: &mut Watches,
        troubles: &mut Troubles,
    ) -> (Outside, bool) {
        let now = Ground::of(Path::new(dir.as_str()));
        let went = match before {
            Some(outside) if outside.watching.covers(&now, watches) => return (outside, false),
            Some(outside) => match outside.watching.into_id() {
                Some(id) => {
                    let RemovedOutside { went, synced } = watches.remove_outside(id);
                    note_watch_trouble(watches, troubles, synced);
                    went
                }
                None => false,
            },
            None => false,
        };
        let sweep = |ground: Option<Ground>, error: &dyn fmt::Display| Beginning::Sweep {
            ground,
            why: format!("{} is read on the sweep alone: {error}", dir.as_str()),
        };
        let watching = match now {
            Ok(Some(ground)) => match watches.add_outside(&ground, went) {
                Ok(id) => Beginning::Watched { id, ground },
                Err(error) => sweep(Some(ground), &error),
            },
            Ok(None) => Beginning::Gone,
            Err(error) => sweep(None, &error),
        };
        if let Some(trouble) = watching.trouble() {
            troubles.say(trouble.clone());
        }
        let anew = matches!(watching, Beginning::Watched { .. });
        assert_eq!(watching.id().is_some(), anew);
        (Outside { watching }, anew)
    }
}

enum Placed {
    In(AbsPath),
    Outside(Outside),
    Unsaid,
}

impl Placed {
    fn top(&self) -> Option<&AbsPath> {
        match self {
            Placed::In(top) => Some(top),
            Placed::Outside(_) | Placed::Unsaid => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Heard,
    Moved,
    Swept,
}

impl Why {
    pub fn said(self, dir: &str) -> Option<String> {
        match self {
            Why::Heard => Some(format!("the watch on {dir} heard it change")),
            Why::Swept => Some(format!("the sweep found {dir} changed")),
            Why::Moved => None,
        }
    }
}

struct Asked {
    ended: Instant,
    took: Duration,
    next: Why,
    placed: Placed,
    trouble: Option<String>,
}

impl Asked {
    fn due(&self, sweep: Duration) -> Instant {
        let wait = match self.next {
            Why::Heard | Why::Moved => self.took,
            Why::Swept => self.took.max(sweep),
        };
        self.ended + wait
    }
}

fn prune_tree_if_unused(
    top: &AbsPath,
    asked: &BTreeMap<Dir, Asked>,
    trees: &mut HashMap<AbsPath, Tree>,
    watches: &mut Watches,
    troubles: &mut Troubles,
) {
    if asked.values().any(|was| was.placed.top() == Some(top)) {
        return;
    }
    if let Some(tree) = trees.remove(top)
        && let Some(id) = tree.watching.into_id()
    {
        let synced = watches.remove_tree(id);
        note_watch_trouble(watches, troubles, synced);
    }
}

fn prune_trees(
    asked: &BTreeMap<Dir, Asked>,
    trees: &mut HashMap<AbsPath, Tree>,
    watches: &mut Watches,
    troubles: &mut Troubles,
) {
    assert!(asked.len() <= FOLLOW_DIRS_MAX);
    assert!(trees.len() <= WATCHES_MAX);
    let live: HashSet<&AbsPath> = asked.values().filter_map(|was| was.placed.top()).collect();
    let stale: Vec<AbsPath> = trees
        .keys()
        .filter(|top| !live.contains(*top))
        .cloned()
        .collect();
    for top in stale {
        if let Some(tree) = trees.remove(&top)
            && let Some(id) = tree.watching.into_id()
        {
            let synced = watches.remove_tree(id);
            note_watch_trouble(watches, troubles, synced);
        }
    }
    assert!(trees.len() <= live.len());
}

fn assert_watches_consistent(
    asked: &BTreeMap<Dir, Asked>,
    trees: &HashMap<AbsPath, Tree>,
    watches: &Watches,
) {
    let live_tops: HashSet<&AbsPath> = asked.values().filter_map(|was| was.placed.top()).collect();
    assert_eq!(trees.len(), live_tops.len());
    for top in &live_tops {
        assert!(trees.contains_key(*top));
    }
    let live_trees = trees
        .values()
        .filter(|tree| tree.watching.id().is_some())
        .count();
    let live_outside = asked
        .values()
        .filter(|was| {
            matches!(&was.placed, Placed::Outside(outside) if outside.watching.id().is_some())
        })
        .count();
    assert!(live_trees + live_outside <= FOLLOW_DIRS_MAX);
    assert_eq!(watches.tree_count(), live_trees);
    assert_eq!(watches.outside_count(), live_outside);
}

fn ask(
    dir: &Dir,
    before: Option<Placed>,
    asked: &BTreeMap<Dir, Asked>,
    trees: &mut HashMap<AbsPath, Tree>,
    watches: &mut Watches,
    troubles: &mut Troubles,
) -> (Placed, Stood, bool) {
    let (placed, anew, common) = match place(dir.as_str()) {
        Ok(Some(place)) => {
            match before {
                Some(Placed::Outside(outside)) => {
                    if let Some(id) = outside.watching.into_id() {
                        let RemovedOutside { synced, .. } = watches.remove_outside(id);
                        note_watch_trouble(watches, troubles, synced);
                    }
                }
                Some(Placed::In(old_top)) if old_top != place.top => {
                    prune_tree_if_unused(&old_top, asked, trees, watches, troubles);
                }
                Some(Placed::In(_) | Placed::Unsaid) | None => {}
            }
            let (top, common) = (place.top.clone(), place.common.clone());
            let (tree, anew) = Tree::watch(trees.remove(&top), place, watches, troubles);
            trees.insert(top.clone(), tree);
            (Placed::In(top), anew, Some(common))
        }
        Ok(None) => {
            let before = match before {
                Some(Placed::Outside(outside)) => Some(outside),
                Some(Placed::In(old_top)) => {
                    prune_tree_if_unused(&old_top, asked, trees, watches, troubles);
                    None
                }
                Some(Placed::Unsaid) | None => None,
            };
            let (outside, anew) = Outside::watch(dir, before, watches, troubles);
            (Placed::Outside(outside), anew, None)
        }
        Err(Unanswered) => (before.unwrap_or(Placed::Unsaid), false, None),
    };
    assert!(trees.len() <= WATCHES_MAX);
    let said = status(dir, &placed, common.as_ref());
    let elsewhere = |now: Option<Place>| now.map(|place| place.top).as_ref() != placed.top();
    let moved = anew && place(dir.as_str()).is_ok_and(elsewhere);
    (placed, said, moved)
}

struct Follow {
    asked: BTreeMap<Dir, Asked>,
    trees: HashMap<AbsPath, Tree>,
    watches: Watches,
    troubles: Troubles,
}

impl Follow {
    fn new(wake: SyncSender<()>) -> Self {
        Follow {
            asked: BTreeMap::new(),
            trees: HashMap::new(),
            watches: Watches::new(wake),
            troubles: Troubles::default(),
        }
    }

    fn run(&mut self, core: &SyncSender<core::Command>, woken: &Receiver<()>, sweep: Duration) {
        'sweep: loop {
            let Some(dirs) = core::ask(core, core::Command::Dirs) else {
                return;
            };
            assert!(dirs.len() <= FOLLOW_DIRS_MAX);
            let stale: Vec<Dir> = self
                .asked
                .keys()
                .filter(|dir| !dirs.contains(dir))
                .cloned()
                .collect();
            for dir in stale {
                if let Some(was) = self.asked.remove(&dir)
                    && let Placed::Outside(outside) = was.placed
                    && let Some(id) = outside.watching.into_id()
                {
                    let RemovedOutside { synced, .. } = self.watches.remove_outside(id);
                    note_watch_trouble(&self.watches, &mut self.troubles, synced);
                }
            }
            prune_trees(
                &self.asked,
                &mut self.trees,
                &mut self.watches,
                &mut self.troubles,
            );
            assert_watches_consistent(&self.asked, &self.trees, &self.watches);
            let heard = self.watches.take_heard();
            if !heard.is_empty() {
                for was in self.asked.values_mut() {
                    let notified = match &was.placed {
                        Placed::In(top) => {
                            let tree = self
                                .trees
                                .get(top)
                                .expect("placed worktree must exist in trees");
                            tree.watching.id().is_some_and(|id| heard.contains_tree(id))
                        }
                        Placed::Outside(outside) => outside
                            .watching
                            .id()
                            .is_some_and(|id| heard.contains_outside(id)),
                        Placed::Unsaid => false,
                    };
                    if notified {
                        was.next = Why::Heard;
                    }
                }
            }
            let now = Instant::now();
            let due = |dir: &&Dir| self.asked.get(*dir).is_none_or(|was| was.due(sweep) <= now);
            let mut due: Vec<Dir> = dirs.iter().filter(due).cloned().collect();
            due.sort_by_key(|dir| self.asked.get(dir).map(|was| was.due(sweep)));
            for dir in due {
                let started = Instant::now();
                let was = self.asked.remove(&dir);
                let why = was.as_ref().map(|was| was.next);
                let before = was.map(|was| was.placed);
                let (placed, said, moved) = ask(
                    &dir,
                    before,
                    &self.asked,
                    &mut self.trees,
                    &mut self.watches,
                    &mut self.troubles,
                );
                let (took, ended) = (started.elapsed(), Instant::now());
                let trouble = match &said {
                    Ok((_, uncounted)) => uncounted.clone(),
                    Err(why) => Some(why.clone()),
                };
                if let Some(trouble) = &trouble {
                    self.troubles.say(trouble.clone());
                }
                let next = if moved { Why::Moved } else { Why::Swept };
                self.asked.insert(
                    dir.clone(),
                    Asked {
                        ended,
                        took,
                        next,
                        placed,
                        trouble,
                    },
                );
                assert_watches_consistent(&self.asked, &self.trees, &self.watches);
                if let Ok((status, _)) = said {
                    let stored = |answer| core::Command::GitStatus {
                        dir,
                        status,
                        why,
                        answer,
                    };
                    match core::ask(core, stored) {
                        Some(Some(line)) => log::say(&line),
                        Some(None) => {}
                        None => return,
                    }
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            assert_watches_consistent(&self.asked, &self.trees, &self.watches);
            let asking = self.asked.values().filter_map(|was| was.trouble.clone());
            let outside = self.asked.values().filter_map(|was| match &was.placed {
                Placed::Outside(outside) => outside.watching.trouble().cloned(),
                Placed::In(_) | Placed::Unsaid => None,
            });
            let watching = self
                .trees
                .values()
                .filter_map(|tree| tree.watching.trouble(&tree.place.top));
            for trouble in asking.chain(outside).chain(watching) {
                self.troubles.say(trouble);
            }
            self.troubles.done();
            let now = Instant::now();
            let next = self
                .asked
                .values()
                .map(|was| was.due(sweep).saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) => watch::settle(woken),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        self.watches.shutdown();
        for (_, tree) in self.trees.drain() {
            if let Some(watch) = tree.watching.into_id() {
                let _ = watch.disarm();
            }
        }
        for was in mem::take(&mut self.asked).into_values() {
            if let Placed::Outside(outside) = was.placed
                && let Some(watch) = outside.watching.into_id()
            {
                let _ = watch.disarm();
            }
        }
    }
}

pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new()
        .name("git".to_owned())
        .spawn(move || {
            let mut state = Follow::new(wake);
            state.run(&core, &woken, sweep);
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::ffi::OsStr;
    use std::fs::{self, File};
    use std::num::NonZeroU32;
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use lune_domain::projects::Dir;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind, RenameMode};
    use notify::{ErrorKind, Event, EventKind, PathsMut, RecursiveMode};

    use super::{
        AbsPath, AddOutcome, BARE_UNREAD, Changed, Common, DIR_EVENTS_MAX, DefaultBranch,
        DirEvents, Directory, Dirs, EVENT_PATHS_MAX, FoldersOnly, Git, Ground, LIMIT,
        NonEmptyPathBuf, ParsedEvent, Placed, RECURSIVE_ADD_RETRIES, REPOSITORY_VARS, Symbolic,
        TrackedDirs, Tree, TreeWatch, Troubles, WatchEngine, Watches, Watching, add_one, ask,
        assert_watches_consistent, branch, changed, config_count, handle_event, head_oid,
        is_ancestor, is_clean, locked, place, pointed, push_force_with_lease, push_if_ahead,
        rebase_in_progress, reconcile, reconcile_with, scratch, stamp, upstream_oid, watched,
        watched_with,
    };

    fn git(dir: &Path, args: &[&str]) -> bool {
        scratch(dir, args).is_ok()
    }

    fn clear_heard(watches: &Watches, woken: &mpsc::Receiver<()>) {
        while woken.try_recv().is_ok() {}
        let _ = watches.take_heard();
    }

    fn wait_heard_tree(
        what: &str,
        watches: &Watches,
        woken: &mpsc::Receiver<()>,
        tree: &TreeWatch,
    ) {
        if watches.take_heard().contains_tree(tree) {
            return;
        }
        let () = lune_waits::Patience::begun().heard_first(what, woken, |()| {
            watches.take_heard().contains_tree(tree).then_some(())
        });
    }

    fn wait_dir_tree(
        what: &str,
        watches: &Watches,
        woken: &mpsc::Receiver<()>,
        tree: &TreeWatch,
        dir: &AbsPath,
    ) {
        let target = NonEmptyPathBuf::new(dir.as_path().to_path_buf()).unwrap();
        let saw = || match watches.take_dirs(tree) {
            Dirs::All => true,
            Dirs::Under(paths) => paths.paths().contains(&target),
            Dirs::None => false,
        };
        if saw() {
            let _ = watches.take_heard();
            return;
        }
        let () = lune_waits::Patience::begun().heard_first(what, woken, |()| {
            let _ = watches.take_heard();
            saw().then_some(())
        });
    }

    fn write_until_heard_tree(
        what: &str,
        watches: &Watches,
        woken: &mpsc::Receiver<()>,
        tree: &TreeWatch,
        file: &AbsPath,
    ) {
        enum Probe {
            Writing { attempt: NonZeroU32, woke: bool },
            WaitingWake,
        }

        assert!(!what.is_empty(), "wait description must be non-empty");
        assert!(
            file.as_path().parent().is_some_and(Path::is_dir),
            "target parent directory must exist before probing: {}",
            file.display()
        );
        let write_probe = |attempt: NonZeroU32| {
            fs::write(file.as_path(), format!("{attempt}\n")).unwrap();
        };
        write_probe(NonZeroU32::MIN);
        let mut state = Probe::Writing {
            attempt: NonZeroU32::MIN,
            woke: false,
        };
        lune_waits::eventually(what, || match state {
            Probe::Writing { attempt, woke } => {
                let woke = woke || woken.try_recv().is_ok();
                if watches.take_heard().contains_tree(tree) {
                    if woke {
                        return Some(());
                    }
                    state = Probe::WaitingWake;
                    return None;
                }
                let next = attempt.checked_add(1).expect("probe write count fits u32");
                write_probe(next);
                state = Probe::Writing {
                    attempt: next,
                    woke,
                };
                None
            }
            Probe::WaitingWake => woken.try_recv().is_ok().then_some(()),
        });
        assert!(
            file.as_path().is_file(),
            "probed file must exist: {}",
            file.display()
        );
    }

    #[test]
    fn a_default_branch_is_a_named_branch_of_a_known_remote_or_of_its_own() {
        let remote = |branch: &str| DefaultBranch::Remote {
            remote: "origin",
            branch: branch.to_owned(),
        };
        assert_eq!(
            DefaultBranch::of("refs/remotes/origin/main"),
            Some(remote("main"))
        );
        assert_eq!(
            DefaultBranch::of("refs/remotes/origin/team/main"),
            Some(remote("team/main"))
        );
        let local = DefaultBranch::Local {
            branch: "trunk".to_owned(),
        };
        assert_eq!(DefaultBranch::of("refs/heads/trunk"), Some(local));
        for unnamed in [
            "refs/remotes/origin/",
            "refs/heads/",
            "refs/remotes/originals/main",
        ] {
            assert_eq!(DefaultBranch::of(unnamed), None, "{unnamed}");
        }
    }

    #[test]
    fn a_branch_reads_as_its_whole_name_beside_a_tag_of_that_name_and_unborn() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| git(dir.path(), args);
        let at = dir.path().to_str().unwrap();
        let named = |name: &str| Ok(Some(name.to_owned()));
        assert!(git(&["init", "-q", "-b", "x"]));
        assert_eq!(branch(at), named("x"));
        assert!(git(&["commit", "-q", "--allow-empty", "-m", "x"]));
        assert!(git(&["tag", "x"]));
        assert_eq!(branch(at), named("x"));
        assert!(git(&["switch", "-q", "-c", "team/x"]));
        assert_eq!(branch(at), named("team/x"));
        assert!(git(&["switch", "-q", "--detach"]));
        assert_eq!(branch(at), Ok(None));
    }

    #[test]
    fn a_git_that_fails_says_why_where_a_ref_pointed_nowhere_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().to_str().unwrap();
        let gone = dir.path().join("gone");
        let unmade = branch(gone.to_str().unwrap()).expect_err("no repository is there");
        assert!(unmade.contains("git symbolic-ref HEAD"), "{unmade}");
        assert!(git(dir.path(), &["init", "-q", "-b", "x"]));
        let limit = Duration::from_secs(60);
        let origin = Symbolic::RemoteHead("origin");
        let remote = || pointed(|command| Git::at(at, command), origin, limit);
        assert_eq!(remote(), Ok(None), "an origin never heard of");
        assert!(git(
            dir.path(),
            &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/heads/x"]
        ));
        assert_eq!(remote(), Ok(None), "it points outside the folder");
        let main = "refs/remotes/origin/main";
        assert!(git(
            dir.path(),
            &["symbolic-ref", "refs/remotes/origin/HEAD", main]
        ));
        assert_eq!(
            remote(),
            Ok(Some("main".to_owned())),
            "it points at origin's main"
        );
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
        let same = File::options()
            .write(true)
            .open(dir.path().join("same"))
            .unwrap();
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
        assert!(git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/trunk"
        ]));
        assert!(git(&["switch", "-q", "-c", "fold"]));
        commit("fold", "f\n");
        assert_eq!(changed(at, "fold"), Ok(Changed::new(1, 0)));
        assert_eq!(changed(at, "trunk"), Ok(None));
        assert!(git(&["update-ref", "refs/remotes/upstream/next", "HEAD"]));
        assert!(git(&[
            "symbolic-ref",
            "refs/remotes/upstream/HEAD",
            "refs/remotes/upstream/next"
        ]));
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
        let cloned = [
            "clone",
            "-q",
            "--filter=blob:none",
            "--no-checkout",
            &url,
            "clone",
        ];
        assert!(git(dir.path(), &cloned));
        assert!(git(
            &clone,
            &["checkout", "-q", "-b", "fold", "origin/fold"]
        ));

        let at = clone.to_str().unwrap();
        let said = changed(at, "fold");
        assert!(
            said.as_ref()
                .is_err_and(|why| why.starts_with("git diff-index said")),
            "{said:?}"
        );
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
            "fatal: cannot change to '/w': Stale file handle",
            "it did not answer in time",
        ] {
            assert_eq!(said(why), Common::Unsaid(why.to_owned()), "{why}");
        }
        assert_eq!(
            said("fatal: cannot change to '/w': No such file or directory"),
            Common::Gone
        );
        let dir = |path: &str| Directory::parse(path.to_owned()).unwrap();
        assert_eq!(
            Common::of(Ok(b"/w/.bare\n".to_vec())),
            Common::Of(dir("/w/.bare"))
        );
        assert!(matches!(
            Common::of(Ok(b".bare\n".to_vec())),
            Common::Unsaid(_)
        ));
        assert!(matches!(Common::of(Ok(Vec::new())), Common::Unsaid(_)));
    }

    #[test]
    fn a_bare_repository_git_will_not_open_unnamed_is_one_it_names_by_an_absolute_path() {
        let refused =
            |bare: &str| format!("{BARE_UNREAD}'{bare}' (safe.bareRepository is 'explicit')");
        let said = |why: &str| Common::of(Err(why.to_owned()));
        let bare = |path: &str| Common::BareUnread(Directory::parse(path.to_owned()).unwrap());
        assert_eq!(said(&refused("/w/o/n.git")), bare("/w/o/n.git"));
        assert_eq!(said(&refused("/w/it' (s).git")), bare("/w/it' (s).git"));
        let relative = refused("n.git");
        assert_eq!(
            said(&relative),
            Common::Unsaid(relative.clone()),
            "it would be read from /"
        );
        assert_eq!(
            said(BARE_UNREAD),
            Common::Unsaid(BARE_UNREAD.to_owned()),
            "it named none"
        );
    }

    #[test]
    fn each_variable_git_keeps_to_a_repository_is_dropped_or_kept_on_purpose() {
        const CONFIG_VARS: [&str; 2] = ["GIT_CONFIG_PARAMETERS", "GIT_CONFIG_COUNT"];
        let mut git = Git::at("/", "rev-parse");
        git.arg("--local-env-vars");
        let said = git.run(LIMIT).unwrap().answer().unwrap();
        let said = String::from_utf8(said).unwrap();
        for name in said.lines() {
            let known = REPOSITORY_VARS.contains(&name) || CONFIG_VARS.contains(&name);
            assert!(
                known,
                "git keeps {name} to a repository; luned neither drops nor keeps it"
            );
        }
    }

    #[test]
    fn config_given_to_git_follows_what_was_inherited_and_leaves_a_bogus_count_to_git() {
        let count = |inherited: &str| config_count(Some(OsStr::new(inherited)));
        assert_eq!(config_count(None), Some(0));
        assert_eq!(count(""), Some(0));
        assert_eq!(count("2"), Some(2));
        assert_eq!(count("abc"), None);
        assert_eq!(count("-1"), None);
    }

    #[test]
    fn config_luned_gives_git_wins_over_config_a_parent_git_gave_it() {
        if std::env::var_os("GIT_CONFIG_PARAMETERS").is_some() {
            let mut git = Git::at("/", "config");
            git.configured(&[("status.showUntrackedFiles", "normal")]);
            git.args(["--get", "status.showUntrackedFiles"]);
            assert_eq!(git.run(LIMIT).unwrap().answer().unwrap(), b"normal\n");
            return;
        }
        let name = "git::tests::config_luned_gives_git_wins_over_config_a_parent_git_gave_it";
        let ran = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name])
            .env("GIT_CONFIG_PARAMETERS", "'status.showuntrackedfiles'='no'")
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&ran.stdout);
        assert!(ran.status.success() && said.contains("1 passed"), "{said}");
    }

    #[test]
    fn an_index_written_anew_is_stamped_anew() {
        let dir = tempfile::tempdir().unwrap();
        let (index, lock) = (dir.path().join("index"), dir.path().join("index.lock"));
        fs::write(&index, "before").unwrap();
        let modified = fs::metadata(&index).unwrap().modified().unwrap();
        let before = stamp(&index);
        fs::write(&lock, "after!").unwrap();
        File::options()
            .write(true)
            .open(&lock)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        fs::rename(&lock, &index).unwrap();
        assert_ne!(stamp(&index), before);
    }

    #[test]
    fn a_branch_patch_includes_committed_uncommitted_and_untracked_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        scratch(path, &["init", "-b", "main"]).unwrap();
        fs::write(path.join("a.rs"), "fn a() {}\n").unwrap();
        scratch(path, &["add", "a.rs"]).unwrap();
        scratch(path, &["commit", "-m", "init"]).unwrap();
        scratch(path, &["checkout", "-b", "feat"]).unwrap();
        fs::write(path.join("a.rs"), "fn a() { let x = 1; }\n").unwrap();
        scratch(path, &["add", "a.rs"]).unwrap();
        scratch(path, &["commit", "-m", "change"]).unwrap();
        fs::write(path.join("a.rs"), "fn a() { let x = 2; }\n").unwrap();
        fs::write(path.join("b.rs"), "fn b() {}\n").unwrap();

        let dir = super::Dir::parse(path.to_str().unwrap()).unwrap();
        let out = super::patch(&dir).unwrap();
        assert_eq!(out.base().as_str(), "main");
        assert_eq!(out.head().as_str(), "feat");
        assert!(!out.truncated());
        let text = String::from_utf8(out.patch().to_vec()).unwrap();
        assert!(text.contains("-fn a() {}"));
        assert!(text.contains("+fn a() { let x = 2; }"));
        assert_eq!(out.untracked().len(), 1);
        assert_eq!(out.untracked()[0].path().as_str(), "b.rs");
        assert_eq!(out.untracked()[0].content(), "fn b() {}\n");
    }

    #[test]
    fn untracked_symlinks_are_skipped_and_oversized_files_mark_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        scratch(path, &["init", "-b", "main"]).unwrap();
        fs::write(path.join("a.rs"), "fn a() {}\n").unwrap();
        scratch(path, &["add", "a.rs"]).unwrap();
        scratch(path, &["commit", "-m", "init"]).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        fs::write(&secret, "do not read").unwrap();
        std::os::unix::fs::symlink(&secret, path.join("link.txt")).unwrap();
        fs::write(
            path.join("huge.txt"),
            "x".repeat(super::UNTRACKED_BYTES_MAX + 10),
        )
        .unwrap();
        fs::write(path.join("small.txt"), "hello\n").unwrap();

        let dir = super::Dir::parse(path.to_str().unwrap()).unwrap();
        let out = super::patch(&dir).unwrap();
        assert!(out.truncated());
        let paths: Vec<&str> = out
            .untracked()
            .iter()
            .map(|file| file.path().as_str())
            .collect();
        assert_eq!(paths, ["small.txt"]);

        std::os::unix::fs::symlink(outside.path(), path.join("escape")).unwrap();
        let root_fd = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let escaped = super::RelativePath::parse("escape/secret.txt".to_owned()).unwrap();
        assert!(super::open_untracked_at(&root_fd, &escaped).is_err());

        let fifo_path = path.join("pipe.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status()
            .unwrap();
        assert!(status.success());
        let fifo = super::RelativePath::parse("pipe.fifo".to_owned()).unwrap();
        if let Ok(fd) = super::open_untracked_at(&root_fd, &fifo) {
            assert!(!fs::File::from(fd).metadata().unwrap().file_type().is_file());
        }
    }

    #[test]
    fn a_commit_with_no_new_folder_keeps_the_tree_watch_and_a_new_folder_updates_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let src = root.join("src");
        fs::create_dir(src.as_path()).unwrap();
        assert!(git(root.as_path(), &["init", "-q", "-b", "main"]));
        fs::write(src.join("lib.rs").as_path(), "start\n").unwrap();
        assert!(git(root.as_path(), &["add", "src/lib.rs"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "start"]));

        let (wake, _woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let mut troubles = Troubles::default();
        let at = root.to_str().unwrap();
        let first = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(None, first, &mut watches, &mut troubles);
        assert!(anew);
        let Watching::Whole { id, .. } = &tree.watching else {
            panic!("expected whole tree watch");
        };
        let id = id.id();
        assert!(watches.active().unwrap().contains_key(src.as_path()));

        fs::write(src.join("lib.rs").as_path(), "edited\n").unwrap();
        assert!(git(root.as_path(), &["add", "src/lib.rs"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "edit"]));
        assert_ne!(stamp(tree.place.own.join("index").as_path()), tree.index);
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(!anew);
        assert_eq!(tree.watching.id().map(TreeWatch::id), Some(id));

        let extra = root.join("extra");
        fs::create_dir(extra.as_path()).unwrap();
        fs::write(extra.join("mod.rs").as_path(), "extra\n").unwrap();
        assert!(git(root.as_path(), &["add", "extra/mod.rs"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "extra"]));
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(anew);
        assert_eq!(tree.watching.id().map(TreeWatch::id), Some(id));
        assert!(watches.active().unwrap().contains_key(extra.as_path()));

        assert!(git(root.as_path(), &["rm", "-q", "-r", "extra"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "drop"]));
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(anew);
        assert_eq!(tree.watching.id().map(TreeWatch::id), Some(id));
        assert!(!watches.active().unwrap().contains_key(extra.as_path()));
        assert!(watches.active().unwrap().contains_key(src.as_path()));

        watches
            .remove_tree(tree.watching.into_id().unwrap())
            .unwrap();
        assert!(watches.active().unwrap().is_empty());
        let routing = locked(&watches.routing);
        assert!(routing.trees.is_empty());
        assert!(routing.outsides.is_empty());
        assert!(routing.direct.is_empty());
        assert!(routing.recursive.is_empty());
    }

    #[test]
    fn shared_watches_reference_count_modes_and_clean_up_on_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let shared = root.join("shared");
        let child = shared.join("child");
        let other = root.join("other");
        for path in [&child, &other] {
            fs::create_dir_all(path.as_path()).unwrap();
        }
        let (wake, _woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let first = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::NonRecursive),
                (child.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(shared.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert_eq!(
            watches.active().unwrap().get(child.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );

        let second = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (other.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(shared.as_path()),
            Some(&RecursiveMode::Recursive)
        );
        assert_eq!(
            watches.active().unwrap().get(other.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );

        watches.remove_tree(second).unwrap();
        assert_eq!(
            watches.active().unwrap().get(shared.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert_eq!(
            watches.active().unwrap().get(child.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert!(!watches.active().unwrap().contains_key(other.as_path()));

        #[cfg(target_os = "linux")]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt as _;

            let unread = root.join("unread");
            fs::create_dir(unread.as_path()).unwrap();
            fs::set_permissions(unread.as_path(), Permissions::from_mode(0o000)).unwrap();
            let failed = watches.add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (unread.clone(), RecursiveMode::NonRecursive),
            ]));
            fs::set_permissions(unread.as_path(), Permissions::from_mode(0o755)).unwrap();
            assert!(failed.is_err());
            assert_eq!(
                watches.active().unwrap().get(shared.as_path()),
                Some(&RecursiveMode::NonRecursive)
            );
            assert_eq!(
                watches.active().unwrap().get(child.as_path()),
                Some(&RecursiveMode::NonRecursive)
            );
            assert!(!watches.active().unwrap().contains_key(unread.as_path()));
            assert_eq!(watches.tree_count(), 1);
        }

        watches.remove_tree(first).unwrap();
        assert!(watches.active().unwrap().is_empty());
        let routing = locked(&watches.routing);
        assert!(routing.trees.is_empty());
        assert!(routing.outsides.is_empty());
        assert!(routing.direct.is_empty());
        assert!(routing.recursive.is_empty());
    }

    #[test]
    fn removing_a_recursive_watch_restores_a_shared_touched_descendant() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let shared = root.join("shared");
        let child = shared.join("child");
        fs::create_dir_all(child.as_path()).unwrap();
        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let survivor = watches
            .add_tree(BTreeMap::from([(
                child.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();
        let recursive = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (child.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(shared.as_path()),
            Some(&RecursiveMode::Recursive)
        );
        assert!(!watches.active().unwrap().contains_key(child.as_path()));

        watches.remove_tree(recursive).unwrap();
        assert!(!watches.active().unwrap().contains_key(shared.as_path()));
        assert_eq!(
            watches.active().unwrap().get(child.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        clear_heard(&watches, &woken);

        fs::write(child.join("probe").as_path(), "changed\n").unwrap();
        wait_heard_tree(
            "the restored descendant watch hears an edit",
            &watches,
            &woken,
            &survivor,
        );

        watches.remove_tree(survivor).unwrap();
        assert!(watches.active().unwrap().is_empty());
        assert_eq!(watches.tree_count(), 0);
    }

    #[test]
    fn dirs_record_ignores_duplicates_at_the_cap_and_escalates_on_overflow() {
        let mut dirs = Dirs::None;
        for index in 0..DIR_EVENTS_MAX {
            let path = NonEmptyPathBuf::new(PathBuf::from(format!("/repo/dir-{index}"))).unwrap();
            dirs.record(&path);
        }
        let dup = NonEmptyPathBuf::new(PathBuf::from("/repo/dir-0")).unwrap();
        dirs.record(&dup);
        let Dirs::Under(events) = &dirs else {
            panic!("duplicate path at the cap must not escalate");
        };
        assert_eq!(events.len(), DIR_EVENTS_MAX);

        let overflow = NonEmptyPathBuf::new(PathBuf::from("/repo/dir-overflow")).unwrap();
        dirs.record(&overflow);
        assert!(matches!(dirs, Dirs::All));
    }

    #[test]
    fn handle_event_records_exact_remove_any_and_caps_oversized_event_batches() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let src = root.join("src");
        fs::create_dir(src.as_path()).unwrap();
        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake.clone());
        let id = watches
            .add_tree(BTreeMap::from([
                (root.clone(), RecursiveMode::NonRecursive),
                (src.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();

        let self_deleted =
            Event::new(EventKind::Remove(RemoveKind::Any)).add_path(src.as_path().to_path_buf());
        handle_event(Ok(self_deleted), &watches.routing, &wake);
        assert!(woken.try_recv().is_ok());
        assert!(watches.take_heard().contains_tree(&id));
        let Dirs::Under(recorded) = watches.take_dirs(&id) else {
            panic!("exact Remove(Any) on a watched directory must be recorded");
        };
        let expected = BTreeSet::from([NonEmptyPathBuf::new(src.as_path().to_path_buf()).unwrap()]);
        assert_eq!(recorded.paths(), &expected);

        let mut oversized = Event::new(EventKind::Remove(RemoveKind::Any));
        for index in 0..=EVENT_PATHS_MAX {
            oversized =
                oversized.add_path(root.join(format!("path-{index}")).as_path().to_path_buf());
        }
        handle_event(Ok(oversized), &watches.routing, &wake);
        assert!(woken.try_recv().is_ok());
        assert!(matches!(watches.take_dirs(&id), Dirs::All));

        watches.remove_tree(id).unwrap();
    }

    #[test]
    fn reconcile_skips_no_op_tracked_diff_and_failed_update_leaves_siblings_watched() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let src = root.join("src");
        fs::create_dir(src.as_path()).unwrap();
        assert!(git(root.as_path(), &["init", "-q", "-b", "main"]));
        fs::write(src.join("lib.rs").as_path(), "start\n").unwrap();
        assert!(git(root.as_path(), &["add", "src/lib.rs"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "start"]));

        let placed = place(root.to_str().unwrap()).ok().flatten().unwrap();
        let old_tracked = TrackedDirs::from_paths(BTreeSet::from([root.clone(), src.clone()]));
        let ghost = root.join("ghost");
        let new_tracked =
            TrackedDirs::from_paths(BTreeSet::from([root.clone(), src.clone(), ghost]));
        let (old_paths, old_aliases) = watched(&placed, Some(&old_tracked));
        assert!(
            reconcile(
                &placed,
                Some(&old_tracked),
                &old_paths,
                &old_aliases,
                Some(&new_tracked),
                Dirs::None,
            )
            .is_none()
        );

        #[cfg(target_os = "linux")]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt as _;

            let sibling = root.join("sibling");
            let unread = root.join("unread");
            fs::create_dir(sibling.as_path()).unwrap();
            fs::create_dir(unread.as_path()).unwrap();
            let (wake, _woken) = mpsc::sync_channel(1);
            let mut watches = Watches::new(wake);
            let kept = watches
                .add_tree(BTreeMap::from([(
                    sibling.clone(),
                    RecursiveMode::NonRecursive,
                )]))
                .unwrap();
            let updated = watches
                .add_tree(BTreeMap::from([(src.clone(), RecursiveMode::NonRecursive)]))
                .unwrap();
            fs::set_permissions(unread.as_path(), Permissions::from_mode(0o000)).unwrap();
            let failed = watches.update_tree(
                updated,
                BTreeMap::from([
                    (src.clone(), RecursiveMode::NonRecursive),
                    (unread.clone(), RecursiveMode::NonRecursive),
                ]),
                &BTreeSet::new(),
            );
            fs::set_permissions(unread.as_path(), Permissions::from_mode(0o755)).unwrap();
            assert!(failed.is_err());
            assert_eq!(watches.tree_count(), 1);
            assert_eq!(
                watches.active().unwrap().get(sibling.as_path()),
                Some(&RecursiveMode::NonRecursive)
            );
            assert!(!watches.active().unwrap().contains_key(src.as_path()));
            assert!(!watches.active().unwrap().contains_key(unread.as_path()));
            watches.remove_tree(kept).unwrap();
            assert!(watches.active().unwrap().is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn readd_failure_on_shared_descendant_rescans_survivor_without_dropping_caller() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let shared = root.join("shared");
        let child = shared.join("child");
        let other = root.join("other");
        fs::create_dir_all(child.as_path()).unwrap();
        fs::create_dir(other.as_path()).unwrap();

        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let survivor = watches
            .add_tree(BTreeMap::from([(
                child.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();
        let caller = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (other.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        while woken.try_recv().is_ok() {}
        let _ = watches.take_heard();

        fs::set_permissions(child.as_path(), Permissions::from_mode(0o000)).unwrap();
        let caller = watches
            .update_tree(
                caller,
                BTreeMap::from([(other.clone(), RecursiveMode::NonRecursive)]),
                &BTreeSet::new(),
            )
            .unwrap();
        fs::set_permissions(child.as_path(), Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            watches.active().unwrap().get(other.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert!(!watches.active().unwrap().contains_key(shared.as_path()));
        assert!(!watches.active().unwrap().contains_key(child.as_path()));
        assert!(woken.try_recv().is_ok());
        assert!(watches.take_heard().contains_tree(&survivor));
        assert!(matches!(watches.take_dirs(&survivor), Dirs::All));

        let survivor = watches
            .update_tree(
                survivor,
                BTreeMap::from([(child.clone(), RecursiveMode::NonRecursive)]),
                &BTreeSet::from([child.clone()]),
            )
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(child.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        clear_heard(&watches, &woken);

        fs::write(child.join("probe").as_path(), "after\n").unwrap();
        wait_heard_tree(
            "rescanned survivor hears edits after recovery",
            &watches,
            &woken,
            &survivor,
        );

        watches.remove_tree(caller).unwrap();
        watches.remove_tree(survivor).unwrap();
        assert!(watches.active().unwrap().is_empty());
    }

    #[test]
    fn needs_rebuild_restores_all_surviving_watches_on_next_sync() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let first_dir = root.join("first");
        let second_dir = root.join("second");
        let third_dir = root.join("third");
        for path in [&first_dir, &second_dir, &third_dir] {
            fs::create_dir(path.as_path()).unwrap();
        }
        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let first = watches
            .add_tree(BTreeMap::from([(
                first_dir.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();
        let second = watches
            .add_tree(BTreeMap::from([(
                second_dir.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();

        watches.engine = WatchEngine::NeedsRebuild;
        let third = watches
            .add_tree(BTreeMap::from([(
                third_dir.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(first_dir.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert_eq!(
            watches.active().unwrap().get(second_dir.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        assert_eq!(
            watches.active().unwrap().get(third_dir.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        clear_heard(&watches, &woken);

        fs::write(first_dir.join("probe").as_path(), "a\n").unwrap();
        wait_heard_tree(
            "first survivor hears edit after rebuild",
            &watches,
            &woken,
            &first,
        );

        clear_heard(&watches, &woken);
        fs::write(second_dir.join("probe").as_path(), "b\n").unwrap();
        wait_heard_tree(
            "second survivor hears edit after rebuild",
            &watches,
            &woken,
            &second,
        );

        watches.remove_tree(third).unwrap();
        watches.remove_tree(second).unwrap();
        watches.remove_tree(first).unwrap();
        assert!(watches.active().unwrap().is_empty());
    }

    #[test]
    fn ask_prunes_abandoned_worktree_immediately_when_directory_leaves_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        assert!(git(root.as_path(), &["init", "-q", "-b", "main"]));
        let followed = Dir::parse(root.to_str().unwrap()).unwrap();

        let (wake, _woken) = mpsc::sync_channel(1);
        let mut asked = BTreeMap::new();
        let mut trees = HashMap::new();
        let mut watches = Watches::new(wake);
        let mut troubles = Troubles::default();

        let (placed, _, _) = ask(
            &followed,
            None,
            &asked,
            &mut trees,
            &mut watches,
            &mut troubles,
        );
        assert!(matches!(placed, Placed::In(_)));
        assert_eq!(trees.len(), 1);
        assert_eq!(watches.tree_count(), 1);
        assert_eq!(watches.outside_count(), 0);

        fs::remove_dir_all(root.join(".git").as_path()).unwrap();
        let (placed, _, _) = ask(
            &followed,
            Some(placed),
            &asked,
            &mut trees,
            &mut watches,
            &mut troubles,
        );
        assert!(matches!(placed, Placed::Outside(_)));
        assert!(trees.is_empty());
        assert_eq!(watches.tree_count(), 0);
        assert_eq!(watches.outside_count(), 1);

        let now = std::time::Instant::now();
        asked.insert(
            followed.clone(),
            super::Asked {
                ended: now,
                took: Duration::ZERO,
                next: super::Why::Swept,
                placed,
                trouble: None,
            },
        );
        assert_watches_consistent(&asked, &trees, &watches);

        if let Some(was) = asked.remove(&followed)
            && let Placed::Outside(outside) = was.placed
            && let Some(id) = outside.watching.into_id()
        {
            watches.remove_outside(id).synced.unwrap();
        }
        assert_watches_consistent(&asked, &trees, &watches);
    }

    #[test]
    fn parsed_event_classifies_boundary_events_and_filters_empty_paths() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        let file = dir.path().join("file");
        fs::create_dir(&sub).unwrap();
        fs::write(&file, "x").unwrap();

        assert!(matches!(
            ParsedEvent::of(Ok(Event::new(EventKind::Access(AccessKind::Any)))),
            ParsedEvent::Ignore
        ));
        assert!(matches!(
            ParsedEvent::of(Err(notify::Error::generic("boom"))),
            ParsedEvent::RescanAll
        ));
        assert!(matches!(
            ParsedEvent::of(Ok(Event::new(EventKind::Create(CreateKind::Folder)))),
            ParsedEvent::HearAll { dir_changed: true }
        ));
        assert!(matches!(
            ParsedEvent::of(Ok(Event::new(EventKind::Modify(ModifyKind::Name(
                RenameMode::Any
            )))
            .add_path(PathBuf::new()),)),
            ParsedEvent::Ignore
        ));

        let renamed = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Any)))
            .add_path(PathBuf::new())
            .add_path(sub.clone())
            .add_path(file.clone());
        let ParsedEvent::Paths {
            rename: true,
            paths,
            ..
        } = ParsedEvent::of(Ok(renamed))
        else {
            panic!("expected parsed rename paths");
        };
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].path.as_path(), sub.as_path());
        assert!(paths[0].renamed_dir);
        assert_eq!(paths[1].path.as_path(), file.as_path());
        assert!(!paths[1].renamed_dir);

        let top = AbsPath::from_absolute(PathBuf::from("/repo")).unwrap();
        assert!(FoldersOnly::TooManyDirs.trouble(&top).contains("more than"));
        assert_eq!(
            FoldersOnly::Unlisted("failed".to_owned()).trouble(&top),
            "failed"
        );
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "tree watch dropped without being removed from Watches")]
    fn dropping_armed_tree_watch_panics_in_debug_builds() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let (wake, _woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let leaked = watches
            .add_tree(BTreeMap::from([(root, RecursiveMode::NonRecursive)]))
            .unwrap();
        drop(leaked);
    }

    #[test]
    fn recursive_add_one_cleans_up_partial_watches_and_retries_child_missing() {
        struct FakePathsMut {
            outcomes: std::collections::VecDeque<notify::Result<()>>,
            remove_error: bool,
            removed: Vec<PathBuf>,
            add_calls: usize,
        }

        impl PathsMut for FakePathsMut {
            fn add(&mut self, _path: &Path, _mode: RecursiveMode) -> notify::Result<()> {
                self.add_calls += 1;
                self.outcomes.pop_front().unwrap_or(Ok(()))
            }

            fn remove(&mut self, path: &Path) -> notify::Result<()> {
                self.removed.push(path.to_path_buf());
                if self.remove_error {
                    Err(notify::Error::generic("remove failed"))
                } else {
                    Ok(())
                }
            }

            fn commit(self: Box<Self>) -> notify::Result<()> {
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let refs = root.join("refs");
        let missing_child = refs.join("heads/feat");
        fs::create_dir(refs.as_path()).unwrap();

        let child_missing = || {
            notify::Error::new(ErrorKind::PathNotFound)
                .add_path(missing_child.as_path().to_path_buf())
        };
        let mut edits = FakePathsMut {
            outcomes: std::collections::VecDeque::from([Err(child_missing()), Ok(())]),
            remove_error: false,
            removed: Vec::new(),
            add_calls: 0,
        };
        let mut active = BTreeMap::new();
        assert!(matches!(
            add_one(&mut edits, &mut active, &refs, RecursiveMode::Recursive),
            AddOutcome::Watched
        ));
        assert_eq!(edits.add_calls, 2);
        assert_eq!(edits.removed, vec![refs.as_path().to_path_buf()]);
        assert_eq!(active.get(&refs), Some(&RecursiveMode::Recursive));

        let mut exhausted = FakePathsMut {
            outcomes: (0..RECURSIVE_ADD_RETRIES)
                .map(|_| Err(child_missing()))
                .collect(),
            remove_error: false,
            removed: Vec::new(),
            add_calls: 0,
        };
        let mut active = BTreeMap::new();
        assert!(matches!(
            add_one(&mut exhausted, &mut active, &refs, RecursiveMode::Recursive),
            AddOutcome::Failed {
                needs_reset: false,
                ..
            }
        ));
        assert_eq!(exhausted.add_calls, RECURSIVE_ADD_RETRIES);
        assert_eq!(exhausted.removed.len(), RECURSIVE_ADD_RETRIES);
        assert!(active.is_empty());

        let mut max_watches = FakePathsMut {
            outcomes: std::collections::VecDeque::from([Err(notify::Error::new(
                ErrorKind::MaxFilesWatch,
            ))]),
            remove_error: false,
            removed: Vec::new(),
            add_calls: 0,
        };
        let mut active = BTreeMap::new();
        assert!(matches!(
            add_one(
                &mut max_watches,
                &mut active,
                &refs,
                RecursiveMode::Recursive
            ),
            AddOutcome::Failed {
                needs_reset: false,
                ..
            }
        ));
        assert_eq!(max_watches.removed, vec![refs.as_path().to_path_buf()]);

        let mut cleanup_fails = FakePathsMut {
            outcomes: std::collections::VecDeque::from([Err(notify::Error::new(
                ErrorKind::MaxFilesWatch,
            ))]),
            remove_error: true,
            removed: Vec::new(),
            add_calls: 0,
        };
        assert!(matches!(
            add_one(
                &mut cleanup_fails,
                &mut active,
                &refs,
                RecursiveMode::Recursive
            ),
            AddOutcome::Failed {
                needs_reset: true,
                ..
            }
        ));

        let active_child = refs.join("heads");
        let mut with_descendant = BTreeMap::from([(active_child, RecursiveMode::NonRecursive)]);
        let mut has_descendant_edits = FakePathsMut {
            outcomes: std::collections::VecDeque::from([Err(notify::Error::new(
                ErrorKind::MaxFilesWatch,
            ))]),
            remove_error: false,
            removed: Vec::new(),
            add_calls: 0,
        };
        assert!(matches!(
            add_one(
                &mut has_descendant_edits,
                &mut with_descendant,
                &refs,
                RecursiveMode::Recursive
            ),
            AddOutcome::Failed {
                needs_reset: true,
                ..
            }
        ));
        assert_eq!(has_descendant_edits.removed, [] as [PathBuf; 0]);

        let gone = root.join("gone");
        let mut root_missing = FakePathsMut {
            outcomes: std::collections::VecDeque::from([Err(notify::Error::new(
                ErrorKind::PathNotFound,
            )
            .add_path(gone.as_path().to_path_buf()))]),
            remove_error: false,
            removed: Vec::new(),
            add_calls: 0,
        };
        assert!(matches!(
            add_one(
                &mut root_missing,
                &mut active,
                &gone,
                RecursiveMode::Recursive
            ),
            AddOutcome::Skipped
        ));
    }

    #[test]
    fn non_recursive_child_under_recursive_watch_preserves_parent_recursive_watch() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let refs = root.join("refs");
        let heads = refs.join("heads");
        fs::create_dir_all(heads.as_path()).unwrap();

        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let tree = watches
            .add_tree(BTreeMap::from([(refs.clone(), RecursiveMode::Recursive)]))
            .unwrap();
        let ground = Ground {
            real: heads.clone(),
            git: false,
        };
        let outside = watches.add_outside(&ground, false).unwrap();
        assert_eq!(
            watches.active().unwrap().get(refs.as_path()),
            Some(&RecursiveMode::Recursive)
        );
        assert!(!watches.active().unwrap().contains_key(heads.as_path()));
        clear_heard(&watches, &woken);
        let _ = watches.take_dirs(&tree);

        let feat = heads.join("feat");
        fs::create_dir(feat.as_path()).unwrap();
        wait_dir_tree(
            "recursive parent hears nested dir while child is watched",
            &watches,
            &woken,
            &tree,
            &feat,
        );
        clear_heard(&watches, &woken);

        write_until_heard_tree(
            "recursive parent watches newly created nested dir",
            &watches,
            &woken,
            &tree,
            &feat.join("branch"),
        );

        watches.remove_outside(outside).synced.unwrap();
        assert_eq!(
            watches.active().unwrap().get(refs.as_path()),
            Some(&RecursiveMode::Recursive)
        );
        clear_heard(&watches, &woken);

        fs::write(heads.join("main").as_path(), "def\n").unwrap();
        wait_heard_tree(
            "recursive parent still hears heads after child removed",
            &watches,
            &woken,
            &tree,
        );

        let _ = watches.take_dirs(&tree);
        let other = heads.join("other");
        fs::create_dir(other.as_path()).unwrap();
        wait_dir_tree(
            "recursive parent hears new dir after child removed",
            &watches,
            &woken,
            &tree,
            &other,
        );
        clear_heard(&watches, &woken);

        write_until_heard_tree(
            "recursive parent hears write inside new nested dir",
            &watches,
            &woken,
            &tree,
            &other.join("tip"),
        );

        watches.remove_tree(tree).unwrap();
        assert!(watches.active().unwrap().is_empty());

        let first_outside = watches.add_outside(&ground, false).unwrap();
        assert_eq!(
            watches.active().unwrap().get(heads.as_path()),
            Some(&RecursiveMode::NonRecursive)
        );
        let second_tree = watches
            .add_tree(BTreeMap::from([(refs.clone(), RecursiveMode::Recursive)]))
            .unwrap();
        assert_eq!(
            watches.active().unwrap().get(refs.as_path()),
            Some(&RecursiveMode::Recursive)
        );
        assert!(!watches.active().unwrap().contains_key(heads.as_path()));
        watches.remove_outside(first_outside).synced.unwrap();
        clear_heard(&watches, &woken);

        fs::write(heads.join("after").as_path(), "xyz\n").unwrap();
        wait_heard_tree(
            "recursive parent survives pre-existing child removal",
            &watches,
            &woken,
            &second_tree,
        );

        watches.remove_tree(second_tree).unwrap();
        assert!(watches.active().unwrap().is_empty());
    }

    #[test]
    fn moving_parent_directory_rescans_nested_tree_and_outside_targets() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let outer = root.join("outer");
        let sub = outer.join("sub");
        let inner = sub.join("inner");
        let plain = sub.join("plain");
        fs::create_dir_all(inner.as_path()).unwrap();
        fs::create_dir_all(plain.as_path()).unwrap();

        let (wake, woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake.clone());
        let outer_tree = watches
            .add_tree(BTreeMap::from([
                (outer.clone(), RecursiveMode::NonRecursive),
                (sub.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        let inner_tree = watches
            .add_tree(BTreeMap::from([(
                inner.clone(),
                RecursiveMode::NonRecursive,
            )]))
            .unwrap();
        let plain_outside = watches
            .add_outside(
                &Ground {
                    real: plain.clone(),
                    git: false,
                },
                false,
            )
            .unwrap();
        while woken.try_recv().is_ok() {}
        let _ = watches.take_heard();

        let moved = Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
            .add_path(sub.as_path().to_path_buf());
        handle_event(Ok(moved), &watches.routing, &wake);
        assert!(woken.try_recv().is_ok());
        let heard = watches.take_heard();
        assert!(heard.contains_tree(&outer_tree));
        assert!(heard.contains_tree(&inner_tree));
        assert!(heard.contains_outside(&plain_outside));
        assert!(watches.went(&plain_outside));
        let Dirs::Under(recorded) = watches.take_dirs(&inner_tree) else {
            panic!("nested tree must record moved ancestor path");
        };
        let expected = BTreeSet::from([NonEmptyPathBuf::new(sub.as_path().to_path_buf()).unwrap()]);
        assert_eq!(recorded.paths(), &expected);

        let moved_out = root.join("moved-out");
        let replacement = root.join("replacement");
        fs::create_dir_all(replacement.join("inner").as_path()).unwrap();
        fs::create_dir_all(replacement.join("plain").as_path()).unwrap();
        clear_heard(&watches, &woken);
        let _ = watches.take_dirs(&outer_tree);
        let _ = watches.take_dirs(&inner_tree);

        fs::rename(sub.as_path(), moved_out.as_path()).unwrap();
        fs::rename(replacement.as_path(), sub.as_path()).unwrap();
        let mut heard_inner = watches.take_heard().contains_tree(&inner_tree);
        let mut recorded_dirs = !matches!(watches.take_dirs(&inner_tree), Dirs::None);
        if !(heard_inner && recorded_dirs) {
            let () = lune_waits::Patience::begun().heard_first(
                "swapping parent folder notifies nested watchers",
                &woken,
                |()| {
                    heard_inner |= watches.take_heard().contains_tree(&inner_tree);
                    recorded_dirs |= !matches!(watches.take_dirs(&inner_tree), Dirs::None);
                    (heard_inner && recorded_dirs).then_some(())
                },
            );
        }

        let inner_tree = watches
            .update_tree(
                inner_tree,
                BTreeMap::from([(inner.clone(), RecursiveMode::NonRecursive)]),
                &BTreeSet::from([inner.clone()]),
            )
            .unwrap();
        clear_heard(&watches, &woken);

        fs::write(inner.join("probe").as_path(), "after swap\n").unwrap();
        wait_heard_tree(
            "refreshed nested tree hears edits on new inode",
            &watches,
            &woken,
            &inner_tree,
        );

        watches.remove_outside(plain_outside).synced.unwrap();
        watches.remove_tree(inner_tree).unwrap();
        watches.remove_tree(outer_tree).unwrap();
        assert!(watches.active().unwrap().is_empty());
    }

    #[test]
    fn reconcile_with_canonical_aliases_handles_case_mismatched_tracked_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let actual_src = root.join("Src");
        let logical_src = root.join("src");
        fs::create_dir(actual_src.as_path()).unwrap();
        assert!(git(root.as_path(), &["init", "-q", "-b", "main"]));
        fs::write(actual_src.join("lib.rs").as_path(), "start\n").unwrap();
        assert!(git(root.as_path(), &["add", "Src/lib.rs"]));
        assert!(git(root.as_path(), &["commit", "-q", "-m", "start"]));

        let placed = place(root.to_str().unwrap()).ok().flatten().unwrap();
        let tracked_dirs =
            TrackedDirs::from_paths(BTreeSet::from([root.clone(), logical_src.clone()]));
        let resolve = |path: &AbsPath| {
            let mapped = if *path == logical_src {
                actual_src.clone()
            } else {
                path.clone()
            };
            super::is_real_dir(mapped.as_path()).then_some(mapped)
        };

        let (paths, aliases) = watched_with(&placed, Some(&tracked_dirs), resolve);
        assert!(paths.contains_key(&actual_src));
        assert!(!paths.contains_key(&logical_src));
        assert_eq!(aliases.get(&logical_src), Some(&actual_src));

        fs::remove_dir_all(actual_src.as_path()).unwrap();
        let affected =
            DirEvents::single(NonEmptyPathBuf::new(actual_src.as_path().to_path_buf()).unwrap());
        let after_delete = reconcile_with(
            &placed,
            Some(&tracked_dirs),
            &paths,
            &aliases,
            Some(&tracked_dirs),
            Dirs::Under(affected),
            resolve,
            true,
        )
        .unwrap();
        assert!(!after_delete.paths.contains_key(&actual_src));
        assert!(after_delete.aliases.is_empty());

        fs::create_dir(actual_src.as_path()).unwrap();
        let affected =
            DirEvents::single(NonEmptyPathBuf::new(actual_src.as_path().to_path_buf()).unwrap());
        let after_recreate = reconcile_with(
            &placed,
            Some(&tracked_dirs),
            &after_delete.paths,
            &after_delete.aliases,
            Some(&tracked_dirs),
            Dirs::Under(affected),
            resolve,
            true,
        )
        .unwrap();
        assert!(after_recreate.paths.contains_key(&actual_src));
        assert_eq!(after_recreate.aliases.get(&logical_src), Some(&actual_src));

        fs::remove_dir_all(actual_src.as_path()).unwrap();
        let root_only = TrackedDirs::from_paths(BTreeSet::from([root.clone()]));
        let after_drop = reconcile_with(
            &placed,
            Some(&tracked_dirs),
            &after_recreate.paths,
            &after_recreate.aliases,
            Some(&root_only),
            Dirs::None,
            resolve,
            true,
        )
        .unwrap();
        assert!(!after_drop.paths.contains_key(&actual_src));
        assert!(after_drop.aliases.is_empty());
    }

    #[test]
    fn watched_skips_symlinked_dot_git_and_reconcile_prunes_stale_recursive_first() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let root = AbsPath::canonical(dir.path()).unwrap();
        let repo = root.join("repo");
        let external_git = root.join("external.git");
        let old_refs = root.join("old-refs");
        let kept_dir = repo.join("kept");
        let dropped_dir = repo.join("dropped");
        let added_dir = repo.join("added");
        for path in [&repo, &old_refs, &kept_dir, &dropped_dir, &added_dir] {
            fs::create_dir_all(path.as_path()).unwrap();
        }
        assert!(git(repo.as_path(), &["init", "-q", "-b", "main"]));
        fs::rename(repo.join(".git").as_path(), external_git.as_path()).unwrap();
        symlink(external_git.as_path(), repo.join(".git").as_path()).unwrap();
        fs::remove_dir_all(external_git.join("refs").as_path()).unwrap();
        symlink(old_refs.as_path(), external_git.join("refs").as_path()).unwrap();

        let placed = place(repo.to_str().unwrap()).ok().flatten().unwrap();
        let old_tracked = TrackedDirs::from_paths(BTreeSet::from([
            repo.clone(),
            kept_dir.clone(),
            dropped_dir.clone(),
        ]));
        let (old_paths, old_aliases) = watched(&placed, Some(&old_tracked));
        assert!(old_paths.contains_key(&external_git));
        assert!(!old_paths.contains_key(repo.join(".git").as_path()));
        assert_eq!(old_paths.get(&old_refs), Some(&RecursiveMode::Recursive));

        fs::remove_file(external_git.join("refs").as_path()).unwrap();
        symlink(dropped_dir.as_path(), external_git.join("refs").as_path()).unwrap();
        let new_tracked = TrackedDirs::from_paths(BTreeSet::from([
            repo.clone(),
            kept_dir.clone(),
            added_dir.clone(),
        ]));
        let updated = reconcile(
            &placed,
            Some(&old_tracked),
            &old_paths,
            &old_aliases,
            Some(&new_tracked),
            Dirs::None,
        )
        .unwrap();
        assert!(!updated.paths.contains_key(&old_refs));
        assert_eq!(
            updated.paths.get(&dropped_dir),
            Some(&RecursiveMode::Recursive)
        );
        assert_eq!(
            updated.paths.get(&added_dir),
            Some(&RecursiveMode::NonRecursive)
        );
        assert_eq!(
            updated.paths.get(&kept_dir),
            Some(&RecursiveMode::NonRecursive)
        );
    }

    #[test]
    fn flow_git_helpers_check_cleanliness_ancestry_and_safe_pushes() {
        let scratch_dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(scratch_dir.path()).unwrap();
        let bare = root.join("origin.git");
        let work = root.join("work");
        fs::create_dir(&bare).unwrap();
        scratch(&bare, &["init", "--bare", "-q", "-b", "main"]).unwrap();
        scratch(
            &root,
            &[
                "clone",
                "-q",
                bare.to_str().unwrap(),
                work.to_str().unwrap(),
            ],
        )
        .unwrap();
        let work_str = work.to_str().unwrap();

        fs::write(work.join("README"), "v1\n").unwrap();
        assert!(!is_clean(work_str).unwrap());
        scratch(&work, &["add", "README"]).unwrap();
        scratch(&work, &["commit", "-q", "-m", "initial"]).unwrap();
        scratch(&work, &["push", "-q", "-u", "origin", "main"]).unwrap();
        assert!(is_clean(work_str).unwrap());
        assert!(!rebase_in_progress(work_str).unwrap());
        let base_oid = head_oid(work_str).unwrap();

        scratch(&work, &["switch", "-q", "-c", "feat"]).unwrap();
        fs::write(work.join("feat.txt"), "hello\n").unwrap();
        scratch(&work, &["add", "feat.txt"]).unwrap();
        scratch(&work, &["commit", "-q", "-m", "feat"]).unwrap();
        let feat_oid = head_oid(work_str).unwrap();
        assert!(is_ancestor(work_str, base_oid.as_str(), feat_oid.as_str()).unwrap());
        assert!(!is_ancestor(work_str, feat_oid.as_str(), base_oid.as_str()).unwrap());

        let pushed = push_if_ahead(work_str).unwrap();
        assert_eq!(pushed, feat_oid);
        assert_eq!(
            upstream_oid(work_str, "feat").unwrap().as_deref(),
            Some(feat_oid.as_str()),
            "a first push tracks the branch it made"
        );
        let pushed_again = push_if_ahead(work_str).unwrap();
        assert_eq!(pushed_again, feat_oid);

        fs::write(work.join("feat.txt"), "amended\n").unwrap();
        scratch(&work, &["add", "feat.txt"]).unwrap();
        scratch(&work, &["commit", "-q", "--amend", "-m", "feat amended"]).unwrap();
        let amended_oid = head_oid(work_str).unwrap();
        assert_ne!(amended_oid, feat_oid);
        assert!(push_force_with_lease(work_str, &feat_oid).is_err());
        push_force_with_lease(work_str, &amended_oid).unwrap();
        let remote_feat = String::from_utf8(
            scratch(
                &root,
                &[
                    "--git-dir",
                    bare.to_str().unwrap(),
                    "rev-parse",
                    "refs/heads/feat",
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(remote_feat.trim(), amended_oid.as_str());
    }

    #[test]
    fn a_push_reports_the_commit_it_pushed_when_the_branch_moves_during_it() {
        let scratch_dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(scratch_dir.path()).unwrap();
        let bare = root.join("origin.git");
        let work = root.join("work");
        fs::create_dir(&bare).unwrap();
        scratch(&bare, &["init", "--bare", "-q", "-b", "main"]).unwrap();
        let (bare_str, work_str) = (bare.to_str().unwrap(), work.to_str().unwrap());
        scratch(&root, &["clone", "-q", bare_str, work_str]).unwrap();
        scratch(&work, &["commit", "-q", "--allow-empty", "-m", "initial"]).unwrap();
        scratch(&work, &["switch", "-q", "-c", "feat"]).unwrap();
        let pushed_head = head_oid(work_str).unwrap();
        let hook = work.join(".git").join("hooks").join("pre-push");
        let staged = std::process::Command::new("sh")
            .args(["-c", "printf '#!/bin/sh\\ngit commit -q --allow-empty -m raced\\n' > \"$1\" && chmod +x \"$1\"", "sh"])
            .arg(&hook)
            .status()
            .unwrap();
        assert!(staged.success());

        let pushed = push_if_ahead(work_str).unwrap();
        assert_eq!(pushed, pushed_head);
        assert_ne!(
            head_oid(work_str).unwrap(),
            pushed_head,
            "the hook moved HEAD"
        );
        let remote = scratch(
            &root,
            &["--git-dir", bare_str, "rev-parse", "refs/heads/feat"],
        );
        let remote = String::from_utf8(remote.unwrap()).unwrap();
        assert_eq!(remote.trim(), pushed_head.as_str());
    }
}
