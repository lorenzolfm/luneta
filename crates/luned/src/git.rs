use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::mem;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lune_domain::projects::Dir;
use lune_protocol::diff::{ReadDiff, RelativePath, UntrackedFile};
use lune_protocol::git::{self, Changed, GitStatus, Head};
use lune_protocol::said::Said;
pub use lune_protocol::PATCH_BYTES_MAX;
use lune_protocol::{UNTRACKED_BYTES_MAX, UNTRACKED_FILES_MAX, UNTRACKED_TOTAL_BYTES_MAX};
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, PathsMut, RecommendedWatcher, RecursiveMode, Watcher as _};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

use crate::core;
use crate::directory::Directory;
use crate::log;
use crate::program::{self, Ran};
use crate::watch::{self, Troubles};

const LIMIT: Duration = Duration::from_mins(1);

const TREE_DIRS_MAX: usize = 4096;

const UNTRACKED_PATHS_INSPECTED_MAX: usize = 1024;

const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

const _: () = assert!(program::SAID_MAX <= u32::MAX as u64);
const _: () = assert!(UNTRACKED_PATHS_INSPECTED_MAX >= UNTRACKED_FILES_MAX);
const _: () = assert!(PATCH_BYTES_MAX + UNTRACKED_TOTAL_BYTES_MAX < lune_protocol::RPC_MESSAGE_BYTES_MAX);

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

const HARDENED: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "safe.bareRepository=explicit"];

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
        Git { command: git, words: vec![command.into()] }
    }

    #[must_use]
    pub fn of(common: &Directory, command: &'static str) -> Git {
        let mut git = hardened();
        let mut git_dir = OsString::from("--git-dir=");
        git_dir.push(common.as_str());
        git.arg(git_dir).current_dir("/");
        Git { command: git, words: vec![command.into()] }
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
        self.words.push(format!("--force-with-lease=refs/heads/{branch}:{at}").into());
        self
    }

    pub fn editing_nothing(&mut self) -> &mut Git {
        self.command.env("GIT_EDITOR", ":").env("GIT_SEQUENCE_EDITOR", ":");
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
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
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

fn status(dir: &str, placed: &Placed, common: Option<&Path>) -> Stood {
    let mut git = Git::at(dir, "status");
    git.args(git::STATUS);
    let ran = git.run(LIMIT)
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
            .map(|mut status| {
                let uncounted = match &mut status {
                    GitStatus::Checkout { head: Head::Branch { name, changed, .. }, .. } => {
                        counted(dir, name.as_str(), changed)
                    },
                    _ => None,
                };
                (status, uncounted)
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
        Common::Of(common) => holder_said(common.as_str()).map(Some),
        Common::NoRepository | Common::Gone => Ok(None),
        Common::BareUnread(bare) => Err(format!("{dir} is in {}", Common::bare_unread(&bare))),
        Common::Unsaid(why) => Err(format!("git did not say what repository {dir} is in: {why}")),
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
    let ran = git.named([&name]).run(limit)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    let said = match ran {
        Ran::Ended { status, .. } if status.code() == Some(1) => return Ok(None),
        ran => ran.answer().map_err(|why| format!("{asked}: {why}"))?,
    };
    let said = one_line(said).ok_or_else(|| format!("{asked} said no one line"))?;
    let pointed = said.strip_prefix(&symbolic.folder()).filter(|pointed| !pointed.is_empty());
    Ok(pointed.map(str::to_owned))
}

pub fn changed(dir: &str, branch: &str) -> Result<Option<Changed>, String> {
    let git = |command: &'static str, args: &[&'static str], names: &[&str]| {
        let mut git = Git::at(dir, command);
        git.args(args.iter().copied()).fetching_nothing_lazily();
        git.named(names).run(LIMIT).map_err(|error| format!("git could not be run: {error}"))
    };
    let tip = format!("refs/heads/{branch}");
    let bases = defaults();
    let mut listing = vec![tip.as_str()];
    listing.extend(bases.iter().map(String::as_str));
    let listed = git("for-each-ref", &[LISTING], &listing)?;
    let listed = listed.answer().map_err(|why| format!("git for-each-ref said {why}"))?;
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
        ran => ran.answer().map_err(|why| format!("git merge-base said {why}"))?,
    };
    let fork = one_line(fork).ok_or("git merge-base named no commit")?;
    let said = git("diff-index", &["-M", "--numstat", "--no-textconv"], &[&fork])?
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
        git.named(names).run(LIMIT).map_err(|error| format!("git could not be run: {error}"))
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
    let mut raw = ran.answer().map_err(|why| format!("git diff-index said {why}"))?;
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
        let listed = listed.answer().map_err(|why| format!("git for-each-ref said {why}"))?;
        let listed = String::from_utf8_lossy(&listed);
        let listed = refs(&listed);
        if let Some(&(tip_oid, _)) = listed.get(tip.as_str())
            && let Some((base_oid, target)) = first_default(&bases, &listed)
            && let Some(default) = DefaultBranch::of(target)
            && default.branch() != branch
        {
            match git("merge-base", &[], &[tip_oid, base_oid])? {
                Ran::Ended { status, .. } if status.code() == Some(1) => {},
                ran => {
                    let fork = ran.answer().map_err(|why| format!("git merge-base said {why}"))?;
                    if let Some(fork) = one_line(fork)
                        && let Some(label) = Said::new(default.to_string())
                    {
                        return Ok((fork, label));
                    }
                },
            }
        }
    }
    match git("rev-parse", &["--verify", "--quiet"], &["HEAD"])? {
        Ran::Ended { status, stdout, .. } if status.success() => {
            if let Some(head_oid) = one_line(stdout) {
                return Ok((head_oid, Said::new("HEAD".to_owned()).expect("HEAD is non-empty")));
            }
        },
        Ran::Ended { .. } => {},
        Ran::Stuck => return Err(format!("git rev-parse in {dir} did not answer in time")),
    }
    Ok((EMPTY_TREE.to_owned(), Said::new("empty".to_owned()).expect("empty is non-empty")))
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
            },
            Err(_) => {
                truncated = true;
                continue;
            },
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
    assert!(inspected <= UNTRACKED_PATHS_INSPECTED_MAX, "inspected untracked paths stay bounded");
    Ok((files, truncated))
}

const DEFAULT_REMOTES: [&str; 3] = ["upstream", "github", "origin"];

const LISTING: &str = "--format=%(refname) %(objectname) %(symref)";

fn defaults() -> Vec<String> {
    let mut named = DEFAULT_REMOTES.map(|remote| format!("refs/remotes/{remote}/HEAD")).to_vec();
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
            Some((fields.next()?, (fields.next()?, fields.next().unwrap_or(""))))
        })
        .collect()
}

fn first_default<'a>(
    named: &'a [String],
    listed: &HashMap<&'a str, (&'a str, &'a str)>,
) -> Option<(&'a str, &'a str)> {
    named.iter().find_map(|name| {
        let &(base, symref) = listed.get(name.as_str())?;
        Some((base, if symref.is_empty() { name.as_str() } else { symref }))
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefaultBranch {
    Remote { remote: &'static str, branch: String },
    Local { branch: String },
}

impl DefaultBranch {
    fn of(target: &str) -> Option<DefaultBranch> {
        let named = |branch: &&str| !branch.is_empty();
        if let Some(branch) = target.strip_prefix("refs/heads/").filter(named) {
            return Some(DefaultBranch::Local { branch: branch.to_owned() });
        }
        let remote = target.strip_prefix("refs/remotes/")?;
        DEFAULT_REMOTES.into_iter().find_map(|name| {
            let branch = remote.strip_prefix(name)?.strip_prefix('/').filter(named)?;
            Some(DefaultBranch::Remote { remote: name, branch: branch.to_owned() })
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
    let push = says(dir, "for-each-ref", &["--format=%(push:remotename)"], &[&head])?;
    if push.is_empty() {
        return Ok(None);
    }
    let config = |name: &str| {
        says(dir, "config", &["--default", "", "--get"], &[format!("branch.{branch}.{name}")])
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
    let asked = [command].iter().chain(args).copied().collect::<Vec<_>>().join(" ");
    let mut git = Git::at(dir, command);
    git.args(args.iter().copied());
    let ran = git.named(names).run(limit)
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
        let real = |path: &[u8]| fs::canonicalize(OsStr::from_bytes(path)).ok();
        Some(Place { top: real(top)?, own: real(own)?, common: real(common)? })
    }
}

fn listed(top: &Path) -> Result<Vec<u8>, String> {
    let mut git = Git::at(top, "ls-files");
    git.arg("-z");
    let ran = git.run(LIMIT).map_err(|error| {
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

const DIRS_EVENT_MAX: usize = 256;

static NO_PATHS: BTreeMap<PathBuf, RecursiveMode> = BTreeMap::new();

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn git_folders(place: &Place) -> BTreeMap<PathBuf, RecursiveMode> {
    use RecursiveMode::{NonRecursive, Recursive};
    let folders = [&place.own, &place.common].map(|folder| (folder.clone(), NonRecursive));
    let refs = place.common.join("refs");
    let refs = [
        (fs::canonicalize(&refs).unwrap_or(refs), Recursive),
        (place.common.join("reftable"), NonRecursive),
        (place.own.join("reftable"), NonRecursive),
    ];
    folders.into_iter().chain(refs).filter(|(path, _)| path.is_dir()).collect()
}

fn watched(place: &Place, tracked: Option<&BTreeSet<PathBuf>>) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut paths = BTreeMap::new();
    for dir in tracked.into_iter().flatten() {
        for candidate in [dir.clone(), dir.join(".git")] {
            if candidate.is_dir() {
                paths.insert(candidate, RecursiveMode::NonRecursive);
            }
        }
    }
    paths.extend(git_folders(place));
    paths
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
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct WatchId(u64);

enum Dirs {
    None,
    Under(BTreeSet<PathBuf>),
    All,
}

impl Dirs {
    fn record(&mut self, path: PathBuf) {
        match self {
            Dirs::All => {},
            Dirs::None => *self = Dirs::Under(BTreeSet::from([path])),
            Dirs::Under(paths) => {
                if paths.len() >= DIRS_EVENT_MAX {
                    *self = Dirs::All;
                } else {
                    paths.insert(path);
                }
            },
        }
    }

    fn all(&mut self) {
        *self = Dirs::All;
    }

    fn take(&mut self) -> Dirs {
        mem::replace(self, Dirs::None)
    }
}

enum Target {
    Tree { heard: bool, dirs: Dirs },
    Outside { real: PathBuf, git: PathBuf, heard: bool, went: bool },
}

#[derive(Default)]
struct Routing {
    targets: HashMap<WatchId, Target>,
    direct: HashMap<PathBuf, BTreeSet<WatchId>>,
    recursive: HashMap<PathBuf, BTreeSet<WatchId>>,
}

impl Routing {
    fn add_path(&mut self, id: WatchId, path: &Path, mode: RecursiveMode) {
        let index = match mode {
            RecursiveMode::NonRecursive => &mut self.direct,
            RecursiveMode::Recursive => &mut self.recursive,
        };
        index.entry(path.to_path_buf()).or_default().insert(id);
    }

    fn remove_path(&mut self, id: WatchId, path: &Path, mode: RecursiveMode) {
        let index = match mode {
            RecursiveMode::NonRecursive => &mut self.direct,
            RecursiveMode::Recursive => &mut self.recursive,
        };
        if let Some(ids) = index.get_mut(path) {
            ids.remove(&id);
            if ids.is_empty() {
                index.remove(path);
            }
        }
    }

    fn watches_exact(&self, id: WatchId, path: &Path) -> bool {
        self.direct.get(path).is_some_and(|ids| ids.contains(&id))
            || self.recursive.get(path).is_some_and(|ids| ids.contains(&id))
    }

    fn collect_matches(&self, path: &Path, out: &mut Vec<WatchId>) {
        out.clear();
        if let Some(ids) = self.direct.get(path) {
            out.extend(ids.iter().copied());
        }
        if let Some(parent) = path.parent()
            && let Some(ids) = self.direct.get(parent)
        {
            out.extend(ids.iter().copied());
        }
        if !self.recursive.is_empty() {
            for ancestor in path.ancestors() {
                if let Some(ids) = self.recursive.get(ancestor) {
                    out.extend(ids.iter().copied());
                }
            }
        }
    }

    fn rescan_all(&mut self) -> bool {
        for target in self.targets.values_mut() {
            match target {
                Target::Tree { heard, dirs } => {
                    *heard = true;
                    dirs.all();
                },
                Target::Outside { heard, went, .. } => {
                    *heard = true;
                    *went = true;
                },
            }
        }
        !self.targets.is_empty()
    }

    fn hear_all(&mut self, dir_changed: bool) -> bool {
        for target in self.targets.values_mut() {
            match target {
                Target::Tree { heard, dirs } => {
                    *heard = true;
                    if dir_changed {
                        dirs.all();
                    }
                },
                Target::Outside { heard, .. } => *heard = true,
            }
        }
        !self.targets.is_empty()
    }
}

fn handle_event(event: notify::Result<Event>, routing: &Mutex<Routing>, wake: &SyncSender<()>) {
    let Ok(event) = event else {
        if locked(routing).rescan_all() {
            wake_up(wake);
        }
        return;
    };
    if event.need_rescan() {
        if locked(routing).rescan_all() {
            wake_up(wake);
        }
        return;
    }
    if event.kind.is_access() {
        return;
    }
    if event.paths.is_empty() {
        let dir_changed = matches!(
            event.kind,
            EventKind::Create(CreateKind::Folder)
                | EventKind::Remove(RemoveKind::Folder)
                | EventKind::Modify(ModifyKind::Name(_))
        );
        if locked(routing).hear_all(dir_changed) {
            wake_up(wake);
        }
        return;
    }
    let folder_event = matches!(
        event.kind,
        EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder)
    );
    let rename = matches!(event.kind, EventKind::Modify(ModifyKind::Name(_)));
    let outside_left =
        matches!(event.kind, EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_)));
    let renamed_dirs: Vec<bool> =
        if rename { event.paths.iter().map(|path| path.is_dir()).collect() } else { Vec::new() };
    let mut notified = false;
    let mut matched = Vec::new();
    {
        let mut routing = locked(routing);
        for (index, path) in event.paths.iter().enumerate() {
            routing.collect_matches(path, &mut matched);
            let renamed_dir = rename && renamed_dirs[index];
            for &id in &matched {
                let exact = rename && !renamed_dir && routing.watches_exact(id, path);
                let Some(target) = routing.targets.get_mut(&id) else {
                    continue;
                };
                match target {
                    Target::Tree { heard, dirs } => {
                        *heard = true;
                        if folder_event || renamed_dir || exact {
                            dirs.record(path.clone());
                        }
                        notified = true;
                    },
                    Target::Outside { real, git, heard, went } => {
                        if outside_left && path == real {
                            *went = true;
                            *heard = true;
                            notified = true;
                        } else if path == git || path.parent() == Some(git.as_path()) {
                            *heard = true;
                            notified = true;
                        }
                    },
                }
            }
        }
    }
    if notified {
        wake_up(wake);
    }
}

#[derive(Default)]
struct Counts {
    non_recursive: usize,
    recursive: usize,
}

impl Counts {
    fn mode(&self) -> Option<RecursiveMode> {
        if self.recursive > 0 {
            Some(RecursiveMode::Recursive)
        } else if self.non_recursive > 0 {
            Some(RecursiveMode::NonRecursive)
        } else {
            None
        }
    }

    fn add(&mut self, mode: RecursiveMode) {
        match mode {
            RecursiveMode::NonRecursive => self.non_recursive += 1,
            RecursiveMode::Recursive => self.recursive += 1,
        }
    }

    fn remove(&mut self, mode: RecursiveMode) {
        match mode {
            RecursiveMode::NonRecursive => self.non_recursive -= 1,
            RecursiveMode::Recursive => self.recursive -= 1,
        }
    }

    fn is_empty(&self) -> bool {
        self.non_recursive == 0 && self.recursive == 0
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

fn add_one(
    edits: &mut dyn PathsMut,
    active: &mut BTreeMap<PathBuf, RecursiveMode>,
    path: &Path,
    mode: RecursiveMode,
) -> notify::Result<()> {
    match edits.add(path, mode) {
        Ok(()) => {
            active.insert(path.to_path_buf(), mode);
            Ok(())
        },
        Err(error) if watch::missing(&error) => Ok(()),
        Err(error) if path.ends_with(".git") && denied(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

struct Watches {
    watcher: Option<RecommendedWatcher>,
    routing: Arc<Mutex<Routing>>,
    wake: SyncSender<()>,
    next: u64,
    watched: HashMap<WatchId, BTreeMap<PathBuf, RecursiveMode>>,
    counts: HashMap<PathBuf, Counts>,
    active: BTreeMap<PathBuf, RecursiveMode>,
}

impl Watches {
    fn new(wake: SyncSender<()>) -> Self {
        Watches {
            watcher: None,
            routing: Arc::new(Mutex::new(Routing::default())),
            wake,
            next: 0,
            watched: HashMap::new(),
            counts: HashMap::new(),
            active: BTreeMap::new(),
        }
    }

    fn next_id(&mut self) -> WatchId {
        let id = WatchId(self.next);
        self.next += 1;
        id
    }

    fn ensure_watcher(&mut self) -> notify::Result<()> {
        if self.watcher.is_none() {
            self.watcher = Some(make_watcher(&self.routing, &self.wake)?);
        }
        Ok(())
    }

    fn paths(&self, id: WatchId) -> &BTreeMap<PathBuf, RecursiveMode> {
        self.watched.get(&id).unwrap_or(&NO_PATHS)
    }

    fn take_heard(&self) -> HashSet<WatchId> {
        let mut routing = locked(&self.routing);
        let mut heard_ids = HashSet::new();
        for (&id, target) in &mut routing.targets {
            let heard = match target {
                Target::Tree { heard, .. } | Target::Outside { heard, .. } => heard,
            };
            if mem::take(heard) {
                heard_ids.insert(id);
            }
        }
        heard_ids
    }

    fn take_dirs(&self, id: WatchId) -> Dirs {
        let mut routing = locked(&self.routing);
        match routing.targets.get_mut(&id) {
            Some(Target::Tree { dirs, .. }) => dirs.take(),
            Some(Target::Outside { .. }) | None => Dirs::None,
        }
    }

    fn went(&self, id: WatchId) -> bool {
        let routing = locked(&self.routing);
        match routing.targets.get(&id) {
            Some(Target::Outside { went, .. }) => *went,
            Some(Target::Tree { .. }) | None => false,
        }
    }

    fn increment(&mut self, path: &Path, mode: RecursiveMode) {
        self.counts.entry(path.to_path_buf()).or_default().add(mode);
    }

    fn decrement(&mut self, path: &Path, mode: RecursiveMode) {
        if let Some(count) = self.counts.get_mut(path) {
            count.remove(mode);
            if count.is_empty() {
                self.counts.remove(path);
            }
        }
    }

    fn add_tree(&mut self, paths: BTreeMap<PathBuf, RecursiveMode>) -> notify::Result<WatchId> {
        let id = self.next_id();
        let target = Target::Tree { heard: false, dirs: Dirs::None };
        self.insert(id, target, paths, &BTreeSet::new())
    }

    fn add_outside(&mut self, ground: &Ground, refresh: bool) -> notify::Result<WatchId> {
        let id = self.next_id();
        let git = ground.real.join(".git");
        let mut paths = BTreeMap::from([(ground.real.clone(), RecursiveMode::NonRecursive)]);
        if ground.git {
            paths.insert(git.clone(), RecursiveMode::NonRecursive);
        }
        let refreshed = if refresh { paths.keys().cloned().collect() } else { BTreeSet::new() };
        let target = Target::Outside { real: ground.real.clone(), git, heard: false, went: false };
        self.insert(id, target, paths, &refreshed)
    }

    fn insert(
        &mut self,
        id: WatchId,
        target: Target,
        paths: BTreeMap<PathBuf, RecursiveMode>,
        refresh: &BTreeSet<PathBuf>,
    ) -> notify::Result<WatchId> {
        self.ensure_watcher()?;
        {
            let mut routing = locked(&self.routing);
            for (path, &mode) in &paths {
                routing.add_path(id, path, mode);
            }
            routing.targets.insert(id, target);
        }
        let mut touched = BTreeSet::new();
        for (path, &mode) in &paths {
            self.increment(path, mode);
            touched.insert(path.clone());
        }
        self.watched.insert(id, paths);
        if let Err(error) = self.sync(&touched, refresh) {
            self.remove(id);
            return Err(error);
        }
        Ok(id)
    }

    fn update_tree(
        &mut self,
        id: WatchId,
        new_paths: BTreeMap<PathBuf, RecursiveMode>,
        refresh: &BTreeSet<PathBuf>,
    ) -> notify::Result<()> {
        let Some(old_paths) = self.watched.insert(id, new_paths) else {
            return Ok(());
        };
        let Some(new_paths) = self.watched.get(&id) else {
            return Ok(());
        };
        let mut removed = Vec::new();
        let mut added = Vec::new();
        for (path, &old_mode) in &old_paths {
            if new_paths.get(path) != Some(&old_mode) {
                removed.push((path.clone(), old_mode));
            }
        }
        for (path, &new_mode) in new_paths {
            if old_paths.get(path) != Some(&new_mode) {
                added.push((path.clone(), new_mode));
            }
        }
        if !removed.is_empty() || !added.is_empty() {
            let mut routing = locked(&self.routing);
            for (path, mode) in &removed {
                routing.remove_path(id, path, *mode);
            }
            for (path, mode) in &added {
                routing.add_path(id, path, *mode);
            }
        }
        let mut touched = BTreeSet::new();
        for (path, mode) in removed {
            self.decrement(&path, mode);
            touched.insert(path);
        }
        for (path, mode) in added {
            self.increment(&path, mode);
            touched.insert(path);
        }
        for path in refresh {
            touched.insert(path.clone());
        }
        if let Err(error) = self.sync(&touched, refresh) {
            self.remove(id);
            return Err(error);
        }
        Ok(())
    }

    fn remove(&mut self, id: WatchId) -> bool {
        let went = {
            let mut routing = locked(&self.routing);
            if let Some(paths) = self.watched.get(&id) {
                for (path, &mode) in paths {
                    routing.remove_path(id, path, mode);
                }
            }
            match routing.targets.remove(&id) {
                Some(Target::Outside { went, .. }) => went,
                Some(Target::Tree { .. }) | None => false,
            }
        };
        let Some(paths) = self.watched.remove(&id) else {
            return went;
        };
        let mut touched = BTreeSet::new();
        for (path, mode) in paths {
            self.decrement(&path, mode);
            touched.insert(path);
        }
        let _ = self.sync(&touched, &BTreeSet::new());
        went
    }

    fn remove_tree(&mut self, tree: &Tree) {
        if let Some(id) = tree.watching.id() {
            self.remove(id);
        }
    }

    fn remove_outside(&mut self, outside: &Outside) -> bool {
        outside.watching.id().is_some_and(|id| self.remove(id))
    }

    fn sync(
        &mut self,
        touched: &BTreeSet<PathBuf>,
        refresh: &BTreeSet<PathBuf>,
    ) -> notify::Result<()> {
        let mut pure_remove = Vec::new();
        let mut replace = Vec::new();
        let mut pure_add = Vec::new();
        for path in touched {
            let was = self.active.get(path).copied();
            let want = self.counts.get(path).and_then(Counts::mode);
            match (was, want) {
                (Some(old_mode), None) => pure_remove.push((path.clone(), old_mode)),
                (Some(old_mode), Some(new_mode))
                    if old_mode != new_mode || refresh.contains(path) =>
                {
                    replace.push((path.clone(), old_mode, new_mode));
                },
                (None, Some(new_mode)) => pure_add.push((path.clone(), new_mode)),
                (None, None) | (Some(_), Some(_)) => {},
            }
        }
        if pure_remove.is_empty() && replace.is_empty() && pure_add.is_empty() {
            return Ok(());
        }
        let mut restore = BTreeMap::new();
        let removed_recursive = pure_remove
            .iter()
            .filter(|(_, mode)| *mode == RecursiveMode::Recursive)
            .map(|(path, _)| path)
            .chain(
                replace
                    .iter()
                    .filter(|(_, old_mode, _)| *old_mode == RecursiveMode::Recursive)
                    .map(|(path, _, _)| path),
            );
        for root in removed_recursive {
            for (descendant, _) in self
                .active
                .range::<PathBuf, _>(root..)
                .take_while(|(path, _)| path.starts_with(root))
                .filter(|(path, _)| *path != root)
            {
                if touched.contains(descendant) {
                    continue;
                }
                if let Some(mode) = self.counts.get(descendant).and_then(Counts::mode) {
                    restore.insert(descendant.clone(), mode);
                }
            }
        }
        self.ensure_watcher()?;
        let Some(watcher) = &mut self.watcher else {
            return Ok(());
        };
        let mut edits = watcher.paths_mut();
        for (path, _) in &pure_remove {
            let _ = edits.remove(path);
            self.active.remove(path);
        }
        let mut failed = None;
        for (path, _, new_mode) in &replace {
            let _ = edits.remove(path);
            self.active.remove(path);
            if let Err(error) = add_one(&mut *edits, &mut self.active, path, *new_mode) {
                failed = Some(error);
                break;
            }
        }
        for (path, mode) in &restore {
            self.active.remove(path);
            if let Err(error) = add_one(&mut *edits, &mut self.active, path, *mode) {
                failed.get_or_insert(error);
            }
        }
        if failed.is_none() {
            for (path, mode) in &pure_add {
                if let Err(error) = add_one(&mut *edits, &mut self.active, path, *mode) {
                    failed = Some(error);
                    break;
                }
            }
        }
        let committed = edits.commit();
        if let Some(error) = failed {
            return Err(error);
        }
        committed
    }
}

fn is_tracked(path: &Path, tracked: Option<&BTreeSet<PathBuf>>) -> bool {
    let Some(tracked) = tracked else {
        return false;
    };
    if tracked.contains(path) {
        return true;
    }
    path.file_name() == Some(OsStr::new(".git"))
        && path.parent().is_some_and(|parent| tracked.contains(parent))
}

fn apply_tracked_diff(
    new_paths: &mut BTreeMap<PathBuf, RecursiveMode>,
    meta: &BTreeMap<PathBuf, RecursiveMode>,
    old_tracked: Option<&BTreeSet<PathBuf>>,
    new_tracked: Option<&BTreeSet<PathBuf>>,
) {
    if old_tracked == new_tracked {
        return;
    }
    for removed in old_tracked
        .into_iter()
        .flatten()
        .filter(|dir| new_tracked.is_none_or(|now| !now.contains(*dir)))
    {
        for candidate in [removed.clone(), removed.join(".git")] {
            if !meta.contains_key(&candidate) && !is_tracked(&candidate, new_tracked) {
                new_paths.remove(&candidate);
            }
        }
    }
    for added in new_tracked
        .into_iter()
        .flatten()
        .filter(|dir| old_tracked.is_none_or(|was| !was.contains(*dir)))
    {
        for candidate in [added.clone(), added.join(".git")] {
            if !new_paths.contains_key(&candidate) && candidate.is_dir() {
                new_paths.insert(candidate, RecursiveMode::NonRecursive);
            }
        }
    }
}

fn apply_meta_refresh(
    place: &Place,
    old_paths: &BTreeMap<PathBuf, RecursiveMode>,
    meta: &BTreeMap<PathBuf, RecursiveMode>,
    new_tracked: Option<&BTreeSet<PathBuf>>,
    affected: &BTreeSet<PathBuf>,
    new_paths: &mut BTreeMap<PathBuf, RecursiveMode>,
    refresh: &mut BTreeSet<PathBuf>,
) {
    let stale_recursive: Vec<PathBuf> = new_paths
        .iter()
        .filter(|(path, mode)| **mode == RecursiveMode::Recursive && !meta.contains_key(*path))
        .map(|(path, _)| path.clone())
        .collect();
    for path in stale_recursive {
        if is_tracked(&path, new_tracked) && path.is_dir() {
            new_paths.insert(path, RecursiveMode::NonRecursive);
        } else {
            new_paths.remove(&path);
        }
    }
    let known = [
        place.own.clone(),
        place.common.clone(),
        place.common.join("reftable"),
        place.own.join("reftable"),
    ];
    for path in known {
        if !meta.contains_key(&path) && !is_tracked(&path, new_tracked) {
            new_paths.remove(&path);
        }
    }
    let refs_link = place.common.join("refs");
    let refs_touched = affected.iter().any(|dir| refs_link.starts_with(dir));
    for (path, &mode) in meta {
        let was_present = old_paths.contains_key(path);
        new_paths.insert(path.clone(), mode);
        let touched = affected.iter().any(|dir| path.starts_with(dir))
            || (mode == RecursiveMode::Recursive && refs_touched);
        if was_present && touched {
            refresh.insert(path.clone());
        }
    }
}

fn apply_dir_refresh(
    old_paths: &BTreeMap<PathBuf, RecursiveMode>,
    meta: &BTreeMap<PathBuf, RecursiveMode>,
    new_tracked: Option<&BTreeSet<PathBuf>>,
    affected: &BTreeSet<PathBuf>,
    new_paths: &mut BTreeMap<PathBuf, RecursiveMode>,
    refresh: &mut BTreeSet<PathBuf>,
) {
    for changed in affected {
        let under: Vec<PathBuf> = new_paths
            .range::<PathBuf, _>(changed..)
            .take_while(|(path, _)| path.starts_with(changed))
            .map(|(path, _)| path.clone())
            .collect();
        for path in under {
            if meta.contains_key(&path) {
                continue;
            }
            if path.is_dir() {
                if old_paths.contains_key(&path) {
                    refresh.insert(path);
                }
            } else {
                new_paths.remove(&path);
            }
        }
        let Some(tracked) = new_tracked else {
            continue;
        };
        for dir in tracked.range::<PathBuf, _>(changed..).take_while(|dir| dir.starts_with(changed))
        {
            for candidate in [dir.clone(), dir.join(".git")] {
                if meta.contains_key(&candidate) {
                    continue;
                }
                if candidate.is_dir() {
                    if old_paths.contains_key(&candidate) {
                        refresh.insert(candidate.clone());
                    }
                    new_paths.insert(candidate, RecursiveMode::NonRecursive);
                } else {
                    new_paths.remove(&candidate);
                }
            }
        }
        if changed.file_name() == Some(OsStr::new(".git"))
            && let Some(parent) = changed.parent()
            && tracked.contains(parent)
            && !meta.contains_key(changed)
        {
            if changed.is_dir() {
                if old_paths.contains_key(changed) {
                    refresh.insert(changed.clone());
                }
                new_paths.insert(changed.clone(), RecursiveMode::NonRecursive);
            } else {
                new_paths.remove(changed);
            }
        }
    }
}

fn reconcile(
    place: &Place,
    old_tracked: Option<&BTreeSet<PathBuf>>,
    old_paths: &BTreeMap<PathBuf, RecursiveMode>,
    new_tracked: Option<&BTreeSet<PathBuf>>,
    dirs: Dirs,
) -> Option<(BTreeMap<PathBuf, RecursiveMode>, BTreeSet<PathBuf>)> {
    match dirs {
        Dirs::None => {
            if old_tracked == new_tracked {
                return None;
            }
            let meta = git_folders(place);
            let mut new_paths = old_paths.clone();
            apply_tracked_diff(&mut new_paths, &meta, old_tracked, new_tracked);
            new_paths.extend(meta);
            Some((new_paths, BTreeSet::new()))
        },
        Dirs::All => {
            let new_paths = watched(place, new_tracked);
            let refresh =
                new_paths.keys().filter(|path| old_paths.contains_key(*path)).cloned().collect();
            Some((new_paths, refresh))
        },
        Dirs::Under(affected) => {
            let meta = git_folders(place);
            let mut new_paths = old_paths.clone();
            let mut refresh = BTreeSet::new();
            apply_tracked_diff(&mut new_paths, &meta, old_tracked, new_tracked);
            apply_meta_refresh(
                place,
                old_paths,
                &meta,
                new_tracked,
                &affected,
                &mut new_paths,
                &mut refresh,
            );
            apply_dir_refresh(
                old_paths,
                &meta,
                new_tracked,
                &affected,
                &mut new_paths,
                &mut refresh,
            );
            if new_paths == *old_paths && refresh.is_empty() {
                None
            } else {
                Some((new_paths, refresh))
            }
        },
    }
}

struct Tree {
    place: Place,
    index: Option<(u64, u64, SystemTime)>,
    watching: Watching,
}

enum Watching {
    Whole { id: WatchId, tracked: BTreeSet<PathBuf> },
    Folders { id: WatchId, why: String },
    Sweep { why: String },
}

impl Watching {
    fn id(&self) -> Option<WatchId> {
        match self {
            Watching::Whole { id, .. } | Watching::Folders { id, .. } => Some(*id),
            Watching::Sweep { .. } => None,
        }
    }

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
        watches: &mut Watches,
        troubles: &mut Troubles,
    ) -> (Tree, bool) {
        let index = stamp(&place.own.join("index"));
        let reusing = match before {
            Some(tree) => {
                let dirs = tree.watching.id().map_or(Dirs::None, |id| watches.take_dirs(id));
                if matches!(dirs, Dirs::None) && tree.place == place && tree.index == index {
                    return (tree, false);
                }
                let index_unchanged = tree.index == index;
                match tree.watching {
                    Watching::Whole { id, tracked } if tree.place == place => {
                        Some((id, Some(tracked), dirs, index_unchanged))
                    },
                    Watching::Folders { id, .. } if tree.place == place => {
                        Some((id, None, dirs, index_unchanged))
                    },
                    Watching::Whole { id, .. } | Watching::Folders { id, .. } => {
                        watches.remove(id);
                        None
                    },
                    Watching::Sweep { .. } => None,
                }
            },
            None => None,
        };
        let top = place.top.display();
        let tracked = match &reusing {
            Some((_, Some(old_tracked), Dirs::Under(_), true)) => Ok(old_tracked.clone()),
            _ => listed(&place.top).and_then(|listed| {
                let many = || {
                    format!(
                        "{top} has more than {TREE_DIRS_MAX} directories that git tracks a path \
                         in, and only git's folders are watched"
                    )
                };
                tracked(&place.top, &listed).ok_or_else(many)
            }),
        };
        if let Some((id, old_tracked, dirs, _)) = reusing {
            let update = reconcile(
                &place,
                old_tracked.as_ref(),
                watches.paths(id),
                tracked.as_ref().ok(),
                dirs,
            );
            let (updated, anew) = match update {
                Some((new_paths, refresh)) => {
                    (watches.update_tree(id, new_paths, &refresh), true)
                },
                None => (Ok(()), false),
            };
            let watching = match (updated, tracked) {
                (Ok(()), Ok(tracked)) => Watching::Whole { id, tracked },
                (Ok(()), Err(why)) => Watching::Folders { id, why },
                (Err(error), _) => {
                    Watching::Sweep { why: format!("{top} is read on the sweep alone: {error}") }
                },
            };
            if let Some(trouble) = watching.trouble() {
                troubles.say(trouble.clone());
            }
            return (Tree { place, index, watching }, anew);
        }
        let paths = watched(&place, tracked.as_ref().ok());
        let watching = match (watches.add_tree(paths), tracked) {
            (Ok(id), Ok(tracked)) => Watching::Whole { id, tracked },
            (Ok(id), Err(why)) => Watching::Folders { id, why },
            (Err(error), _) => {
                Watching::Sweep { why: format!("{top} is read on the sweep alone: {error}") }
            },
        };
        if let Some(trouble) = watching.trouble() {
            troubles.say(trouble.clone());
        }
        (Tree { place, index, watching }, true)
    }
}

struct Outside {
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
    Watched { id: WatchId, ground: Ground },
    Gone,
    Sweep { ground: Option<Ground>, why: String },
}

impl Beginning {
    fn id(&self) -> Option<WatchId> {
        match self {
            Beginning::Watched { id, .. } => Some(*id),
            Beginning::Gone | Beginning::Sweep { .. } => None,
        }
    }

    fn covers(&self, now: &io::Result<Option<Ground>>, watches: &Watches) -> bool {
        match (self, now) {
            (Beginning::Watched { id, ground }, Ok(Some(now))) => {
                !watches.went(*id) && ground == now
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
        watches: &mut Watches,
        troubles: &mut Troubles,
    ) -> (Outside, bool) {
        let now = Ground::of(dir);
        let went = match before {
            Some(outside) if outside.watching.covers(&now, watches) => return (outside, false),
            Some(outside) => watches.remove_outside(&outside),
            None => false,
        };
        let sweep = |ground: Option<Ground>, error: &dyn fmt::Display| Beginning::Sweep {
            ground,
            why: format!("{} is read on the sweep alone: {error}", dir.display()),
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
        (Outside { watching }, anew)
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

    fn within(&self, top: &Path) -> bool {
        self.placed.top() == Some(top)
    }
}

fn prune_trees(
    asked: &HashMap<String, Asked>,
    trees: &mut HashMap<PathBuf, Tree>,
    watches: &mut Watches,
) {
    trees.retain(|top, tree| {
        let keep = asked.values().any(|was| was.within(top));
        if !keep {
            watches.remove_tree(tree);
        }
        keep
    });
}

fn ask(
    dir: &str,
    before: Option<Placed>,
    trees: &mut HashMap<PathBuf, Tree>,
    watches: &mut Watches,
    troubles: &mut Troubles,
) -> (Placed, Stood, bool) {
    let (placed, anew, common) = match place(dir) {
        Ok(Some(place)) => {
            if let Some(Placed::Outside(outside)) = before {
                watches.remove_outside(&outside);
            }
            let (top, common) = (place.top.clone(), place.common.clone());
            let (tree, anew) = Tree::watch(trees.remove(&top), place, watches, troubles);
            trees.insert(top.clone(), tree);
            (Placed::In(top), anew, Some(common))
        },
        Ok(None) => {
            let before = match before {
                Some(Placed::Outside(outside)) => Some(outside),
                Some(Placed::In(_) | Placed::Unsaid) | None => None,
            };
            let (outside, anew) = Outside::watch(Path::new(dir), before, watches, troubles);
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
        let mut watches = Watches::new(wake);
        let mut troubles = Troubles::default();
        'sweep: loop {
            let Some(dirs) = core::ask(&core, core::Command::Dirs) else {
                return;
            };
            asked.retain(|dir, was| {
                let keep = dirs.contains(dir);
                if !keep && let Placed::Outside(outside) = &was.placed {
                    watches.remove_outside(outside);
                }
                keep
            });
            prune_trees(&asked, &mut trees, &mut watches);
            let heard = watches.take_heard();
            if !heard.is_empty() {
                for (top, tree) in &trees {
                    if tree.watching.id().is_some_and(|id| heard.contains(&id)) {
                        let within = asked.values_mut().filter(|was| was.within(top));
                        within.for_each(|was| was.next = Why::Heard);
                    }
                }
                for was in asked.values_mut() {
                    if let Placed::Outside(outside) = &was.placed
                        && outside.watching.id().is_some_and(|id| heard.contains(&id))
                    {
                        was.next = Why::Heard;
                    }
                }
            }
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|was| was.due(sweep) <= now);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            due.sort_by_key(|dir| asked.get(dir).map(|was| was.due(sweep)));
            for dir in due {
                let started = Instant::now();
                let was = asked.remove(&dir);
                let why = was.as_ref().map(|was| was.next);
                let before = was.map(|was| was.placed);
                let (placed, said, moved) =
                    ask(&dir, before, &mut trees, &mut watches, &mut troubles);
                let (took, ended) = (started.elapsed(), Instant::now());
                let trouble = match &said {
                    Ok((_, uncounted)) => uncounted.clone(),
                    Err(why) => Some(why.clone()),
                };
                if let Some(trouble) = &trouble {
                    troubles.say(trouble.clone());
                }
                let next = if moved { Why::Moved } else { Why::Swept };
                asked.insert(dir.clone(), Asked { ended, took, next, placed, trouble });
                if let Ok((status, _)) = said {
                    let stored = |answer| core::Command::GitStatus { dir, status, why, answer };
                    match core::ask(&core, stored) {
                        Some(Some(line)) => log::say(&line),
                        Some(None) => {},
                        None => return,
                    }
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            prune_trees(&asked, &mut trees, &mut watches);
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
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::fs::{self, File};
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::{Duration, SystemTime};

    use notify::RecursiveMode;

    use super::{
        BARE_UNREAD, Changed, Common, DefaultBranch, Directory, Git, LIMIT, REPOSITORY_VARS,
        Symbolic, Tree, Troubles, Watches, Watching, branch, changed, config_count, locked, place,
        pointed, scratch, stamp,
    };

    fn git(dir: &Path, args: &[&str]) -> bool {
        scratch(dir, args).is_ok()
    }

    #[test]
    fn a_default_branch_is_a_named_branch_of_a_known_remote_or_of_its_own() {
        let remote = |branch: &str| {
            DefaultBranch::Remote { remote: "origin", branch: branch.to_owned() }
        };
        assert_eq!(DefaultBranch::of("refs/remotes/origin/main"), Some(remote("main")));
        assert_eq!(DefaultBranch::of("refs/remotes/origin/team/main"), Some(remote("team/main")));
        let local = DefaultBranch::Local { branch: "trunk".to_owned() };
        assert_eq!(DefaultBranch::of("refs/heads/trunk"), Some(local));
        for unnamed in ["refs/remotes/origin/", "refs/heads/", "refs/remotes/originals/main"] {
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
        assert!(git(dir.path(), &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/heads/x"]));
        assert_eq!(remote(), Ok(None), "it points outside the folder");
        let main = "refs/remotes/origin/main";
        assert!(git(dir.path(), &["symbolic-ref", "refs/remotes/origin/HEAD", main]));
        assert_eq!(remote(), Ok(Some("main".to_owned())), "it points at origin's main");
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
            "fatal: cannot change to '/w': Stale file handle",
            "it did not answer in time",
        ] {
            assert_eq!(said(why), Common::Unsaid(why.to_owned()), "{why}");
        }
        assert_eq!(said("fatal: cannot change to '/w': No such file or directory"), Common::Gone);
        let dir = |path: &str| Directory::parse(path.to_owned()).unwrap();
        assert_eq!(Common::of(Ok(b"/w/.bare\n".to_vec())), Common::Of(dir("/w/.bare")));
        assert!(matches!(Common::of(Ok(b".bare\n".to_vec())), Common::Unsaid(_)));
        assert!(matches!(Common::of(Ok(Vec::new())), Common::Unsaid(_)));
    }

    #[test]
    fn a_bare_repository_git_will_not_open_unnamed_is_one_it_names_by_an_absolute_path() {
        let refused = |bare: &str| {
            format!("{BARE_UNREAD}'{bare}' (safe.bareRepository is 'explicit')")
        };
        let said = |why: &str| Common::of(Err(why.to_owned()));
        let bare = |path: &str| Common::BareUnread(Directory::parse(path.to_owned()).unwrap());
        assert_eq!(said(&refused("/w/o/n.git")), bare("/w/o/n.git"));
        assert_eq!(said(&refused("/w/it' (s).git")), bare("/w/it' (s).git"));
        let relative = refused("n.git");
        assert_eq!(said(&relative), Common::Unsaid(relative.clone()), "it would be read from /");
        assert_eq!(said(BARE_UNREAD), Common::Unsaid(BARE_UNREAD.to_owned()), "it named none");
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
            assert!(known, "git keeps {name} to a repository; luned neither drops nor keeps it");
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
        File::options().write(true).open(&lock).unwrap().set_modified(modified).unwrap();
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
        fs::write(path.join("huge.txt"), "x".repeat(super::UNTRACKED_BYTES_MAX + 10)).unwrap();
        fs::write(path.join("small.txt"), "hello\n").unwrap();

        let dir = super::Dir::parse(path.to_str().unwrap()).unwrap();
        let out = super::patch(&dir).unwrap();
        assert!(out.truncated());
        let paths: Vec<&str> = out.untracked().iter().map(|file| file.path().as_str()).collect();
        assert_eq!(paths, ["small.txt"]);

        std::os::unix::fs::symlink(outside.path(), path.join("escape")).unwrap();
        let root_fd = rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let escaped = super::RelativePath::parse("escape/secret.txt".to_owned()).unwrap();
        assert!(super::open_untracked_at(&root_fd, &escaped).is_err());

        let fifo_path = path.join("pipe.fifo");
        let status = std::process::Command::new("mkfifo").arg(&fifo_path).status().unwrap();
        assert!(status.success());
        let fifo = super::RelativePath::parse("pipe.fifo".to_owned()).unwrap();
        if let Ok(fd) = super::open_untracked_at(&root_fd, &fifo) {
            assert!(!fs::File::from(fd).metadata().unwrap().file_type().is_file());
        }
    }

    #[test]
    fn a_commit_with_no_new_folder_keeps_the_tree_watch_and_a_new_folder_updates_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let src = root.join("src");
        fs::create_dir(&src).unwrap();
        assert!(git(&root, &["init", "-q", "-b", "main"]));
        fs::write(src.join("lib.rs"), "start\n").unwrap();
        assert!(git(&root, &["add", "src/lib.rs"]));
        assert!(git(&root, &["commit", "-q", "-m", "start"]));

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
        let id = *id;
        assert!(watches.active.contains_key(&src));

        fs::write(src.join("lib.rs"), "edited\n").unwrap();
        assert!(git(&root, &["add", "src/lib.rs"]));
        assert!(git(&root, &["commit", "-q", "-m", "edit"]));
        assert_ne!(stamp(&tree.place.own.join("index")), tree.index);
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(!anew);
        assert_eq!(tree.watching.id(), Some(id));

        let extra = root.join("extra");
        fs::create_dir(&extra).unwrap();
        fs::write(extra.join("mod.rs"), "extra\n").unwrap();
        assert!(git(&root, &["add", "extra/mod.rs"]));
        assert!(git(&root, &["commit", "-q", "-m", "extra"]));
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(anew);
        assert_eq!(tree.watching.id(), Some(id));
        assert!(watches.active.contains_key(&extra));

        assert!(git(&root, &["rm", "-q", "-r", "extra"]));
        assert!(git(&root, &["commit", "-q", "-m", "drop"]));
        let next = place(at).ok().flatten().unwrap();
        let (tree, anew) = Tree::watch(Some(tree), next, &mut watches, &mut troubles);
        assert!(anew);
        assert_eq!(tree.watching.id(), Some(id));
        assert!(!watches.active.contains_key(&extra));
        assert!(watches.active.contains_key(&src));

        watches.remove_tree(&tree);
        assert!(watches.active.is_empty());
        assert!(watches.counts.is_empty());
        assert!(watches.watched.is_empty());
        let routing = locked(&watches.routing);
        assert!(routing.targets.is_empty());
        assert!(routing.direct.is_empty());
        assert!(routing.recursive.is_empty());
    }

    #[test]
    fn shared_watches_reference_count_modes_and_clean_up_on_remove() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let shared = root.join("shared");
        let child = shared.join("child");
        let other = root.join("other");
        for path in [&child, &other] {
            fs::create_dir_all(path).unwrap();
        }
        let (wake, _woken) = mpsc::sync_channel(1);
        let mut watches = Watches::new(wake);
        let first = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::NonRecursive),
                (child.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        assert_eq!(watches.active.get(&shared), Some(&RecursiveMode::NonRecursive));
        assert_eq!(watches.active.get(&child), Some(&RecursiveMode::NonRecursive));

        let second = watches
            .add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (other.clone(), RecursiveMode::NonRecursive),
            ]))
            .unwrap();
        assert_eq!(watches.active.get(&shared), Some(&RecursiveMode::Recursive));
        assert_eq!(watches.active.get(&other), Some(&RecursiveMode::NonRecursive));

        watches.remove(second);
        assert_eq!(watches.active.get(&shared), Some(&RecursiveMode::NonRecursive));
        assert_eq!(watches.active.get(&child), Some(&RecursiveMode::NonRecursive));
        assert!(!watches.active.contains_key(&other));

        #[cfg(target_os = "linux")]
        {
            use std::fs::Permissions;
            use std::os::unix::fs::PermissionsExt as _;

            let unread = root.join("unread");
            fs::create_dir(&unread).unwrap();
            fs::set_permissions(&unread, Permissions::from_mode(0o000)).unwrap();
            let failed = watches.add_tree(BTreeMap::from([
                (shared.clone(), RecursiveMode::Recursive),
                (unread.clone(), RecursiveMode::NonRecursive),
            ]));
            fs::set_permissions(&unread, Permissions::from_mode(0o755)).unwrap();
            assert!(failed.is_err());
            assert_eq!(watches.active.get(&shared), Some(&RecursiveMode::NonRecursive));
            assert_eq!(watches.active.get(&child), Some(&RecursiveMode::NonRecursive));
            assert!(!watches.active.contains_key(&unread));
            assert_eq!(watches.watched.len(), 1);
        }

        watches.remove(first);
        assert!(watches.active.is_empty());
        assert!(watches.counts.is_empty());
        assert!(watches.watched.is_empty());
        let routing = locked(&watches.routing);
        assert!(routing.targets.is_empty());
        assert!(routing.direct.is_empty());
        assert!(routing.recursive.is_empty());
    }
}
