use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lune_domain::projects::Dir;
use lune_protocol::diff::{ReadDiff, RelativePath, UntrackedFile};
use lune_protocol::git::{self, Changed, GitStatus, Head};
use lune_protocol::said::Said;
pub use lune_protocol::PATCH_BYTES_MAX;
use lune_protocol::{UNTRACKED_BYTES_MAX, UNTRACKED_FILES_MAX, UNTRACKED_TOTAL_BYTES_MAX};
use notify::event::{CreateKind, ModifyKind, RemoveKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
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

fn read_untracked(
    dir: &str,
    git: &impl Fn(&'static str, &[&'static str], &[&str]) -> Result<Ran, String>,
) -> Result<(Vec<UntrackedFile>, bool), String> {
    let listed = git("ls-files", &["--others", "--exclude-standard", "-z"], &[])?
        .answer()
        .map_err(|why| format!("git ls-files said {why}"))?;
    let root = Path::new(dir);
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
        let full = root.join(rel_path.as_path());
        let fd = match rustix::fs::open(
            &full,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::LOOP | Errno::NOTDIR) => continue,
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
                    within.for_each(|was| was.next = Why::Heard);
                }
            }
            for was in asked.values_mut() {
                if let Placed::Outside(outside) = &was.placed
                    && outside.heard.swap(false, Ordering::SeqCst)
                {
                    was.next = Why::Heard;
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
                let (placed, said, moved) = ask(&dir, before, &mut trees, &wake, &mut troubles);
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

    use std::ffi::OsStr;

    use super::{
        BARE_UNREAD, Changed, Common, DefaultBranch, Directory, Git, LIMIT,
        REPOSITORY_VARS, Symbolic, branch, changed, config_count, pointed, scratch, stamp,
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
    }
}
