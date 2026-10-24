//! How the work tree stands in each directory a pane works in, as `git status`
//! says it.
//!
//! git tells no one as a work tree changes, so each directory is asked about
//! again once a sweep has passed since it last was, one directory at a time,
//! and one that git has said nothing of yet first.

use std::collections::HashMap;
use std::io;
use std::process::Command;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use crate::api;
use crate::core;
use crate::program::{self, Ran};
use crate::watch::Troubles;

/// How long git has to say how a work tree stands. A large one takes it a
/// while; one that takes longer than this is stuck.
const LIMIT: Duration = Duration::from_secs(10);

const DETACHED: &str = "(detached)";

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

/// How a work tree stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkTree {
    /// The branch checked out, if one is.
    pub branch: Option<String>,
    pub changes: Changes,
    pub stashes: u32,
    /// How far the branch is ahead of its upstream and behind it, if it has
    /// one.
    pub upstream: Option<(u32, u32)>,
}

/// How many paths there are of each kind of change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub conflicted: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub modified: u32,
    pub staged: u32,
    pub untracked: u32,
}

impl WorkTree {
    #[must_use]
    pub fn to_api(&self, dir: &str) -> api::WorkTree {
        let changes = &self.changes;
        api::WorkTree {
            dir: dir.to_owned(),
            branch: self.branch.clone().inspect(|branch| assert!(!branch.is_empty())),
            changes: Some(api::Changes {
                conflicted: changes.conflicted,
                deleted: changes.deleted,
                renamed: changes.renamed,
                modified: changes.modified,
                staged: changes.staged,
                untracked: changes.untracked,
            }),
            stashes: self.stashes,
            upstream: self.upstream.map(|(ahead, behind)| api::Upstream { ahead, behind }),
        }
    }

    /// What `git status --porcelain=v2 --branch --show-stash` prints. A
    /// detached `HEAD` is no branch.
    fn parse(status: &str) -> Self {
        let mut tree = WorkTree::default();
        for line in status.lines() {
            if let Some(head) = line.strip_prefix("# branch.head ") {
                let named = Some(head.trim()).filter(|head| !head.is_empty() && *head != DETACHED);
                tree.branch = named.map(str::to_owned);
            } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
                tree.upstream = ahead_behind(ab);
            } else if let Some(stashes) = line.strip_prefix("# stash ") {
                tree.stashes = stashes.parse().unwrap_or(0);
            } else {
                tree.changes.add(line);
            }
        }
        tree
    }
}

/// `+<ahead> -<behind>`.
fn ahead_behind(ab: &str) -> Option<(u32, u32)> {
    let (ahead, behind) = ab.split_once(' ')?;
    Some((ahead.strip_prefix('+')?.parse().ok()?, behind.strip_prefix('-')?.parse().ok()?))
}

impl Changes {
    /// A line of `git status --porcelain=v2` about a path: `1` for a changed
    /// one, `2` for a renamed or copied one, `u` for a conflict, `?` for an
    /// untracked one. The first two say how it stands in the index and in the
    /// work tree, `.` for unchanged.
    fn add(&mut self, line: &str) {
        let mut fields = line.split(' ');
        match (fields.next(), fields.next()) {
            (Some("1"), Some(xy)) => self.add_change(xy),
            (Some("2"), Some(xy)) => {
                self.renamed += 1;
                self.add_change(xy);
            },
            (Some("u"), _) => self.conflicted += 1,
            (Some("?"), _) => self.untracked += 1,
            _ => {},
        }
    }

    fn add_change(&mut self, xy: &str) {
        let mut states = xy.chars();
        let (Some(index), Some(tree)) = (states.next(), states.next()) else {
            return;
        };
        self.staged += u32::from(matches!(index, 'M' | 'A' | 'T'));
        self.deleted += u32::from(index == 'D') + u32::from(tree == 'D');
        // `A` in the work tree is a path added with `--intent-to-add`.
        self.modified += u32::from(matches!(tree, 'M' | 'A'));
    }
}

/// What git says of the work tree in `dir`: how it stands, or none at all, as
/// in a directory in no repository, where git fails. Why git said nothing it
/// can be asked again for, `Err` says.
fn status(dir: &str) -> Result<WorkTree, String> {
    // `--no-optional-locks` keeps git from taking the index lock to refresh
    // the index, which a `git` of your own would trip over. `core.fsmonitor`
    // off keeps a repository's own config from naming a command for every
    // status to run, since git is asked about any directory a pane works in,
    // trusted or not; a filter driver it names can still run on a changed
    // file, as with any `git status` there.
    let mut git = Command::new("git");
    git.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C", dir])
        .args(["status", "--porcelain=v2", "--branch", "--show-stash"]);
    // These would have git read some other repository than the one at `dir`,
    // as a git hook that started the daemon would pass them on.
    for repository in REPOSITORY_VARS {
        git.env_remove(repository);
    }
    match program::run(&mut git, LIMIT) {
        Ok(Ran::Ended { status, stdout, .. }) if status.success() => {
            Ok(WorkTree::parse(&String::from_utf8_lossy(&stdout)))
        },
        // git ran there and failed: the directory is in no repository, or in
        // one git cannot read.
        Ok(Ran::Ended { .. }) => Ok(WorkTree::default()),
        Ok(Ran::Stuck) => Err(format!("git status in {dir} did not answer in time")),
        Err(error) => Err(format!("git could not be started: {error}")),
    }
}

/// Asks git how the work tree stands in each directory a pane works in, in a
/// thread of its own, and tells the core: about each once a `sweep` has
/// passed since it was last asked about, and about a directory it has said
/// nothing of yet as soon as it is `woken`, before those left. What git said
/// of a directory it could not answer for stands until it can. The thread
/// ends once the core has.
pub fn follow(
    core: SyncSender<core::Command>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("git".to_owned()).spawn(move || {
        let mut asked: HashMap<String, Instant> = HashMap::new();
        let mut troubles = Troubles::default();
        loop {
            let Some(dirs) = core::ask(&core, core::Command::Dirs) else {
                return;
            };
            asked.retain(|dir, _| dirs.contains(dir));
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|at| now - *at >= sweep);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            // Never asked about before all others, then the longest ago.
            due.sort_by_key(|dir| asked.get(dir).copied());
            let mut cut_short = false;
            for dir in due {
                asked.insert(dir.clone(), Instant::now());
                if let Ok(tree) = status(&dir).inspect_err(|why| troubles.say(why.clone()))
                    && core.send(core::Command::WorkTree { dir, tree }).is_err()
                {
                    return;
                }
                if woken.try_recv().is_ok() {
                    cut_short = true;
                    break;
                }
            }
            if cut_short {
                continue;
            }
            troubles.done();
            let now = Instant::now();
            let next = asked.values().map(|at| (*at + sweep).saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}
