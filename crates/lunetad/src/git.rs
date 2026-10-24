//! How the work tree stands in each directory a pane works in, as `git status`
//! says it.
//!
//! git tells no one as a work tree changes, so each directory is asked about
//! again once git has rested from it for a sweep, or for as long as it took
//! over it if that is longer, one directory at a time, and one that git has
//! said nothing of yet first.

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
/// while, and one across the network longer; one that takes more than a
/// minute is stuck.
const LIMIT: Duration = Duration::from_mins(1);

const DETACHED: &str = "(detached)";

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
            branch: self.branch.clone().inspect(|branch| {
                assert!(!branch.is_empty());
                assert_ne!(branch, DETACHED);
            }),
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

    /// What `git status --porcelain=v2 --branch --show-stash` prints, or the
    /// first line that does not read. A detached `HEAD` is no branch.
    fn parse(status: &str) -> Result<Self, String> {
        let mut tree = WorkTree::default();
        for line in status.lines() {
            let unread = || format!("a line that does not read: {line}");
            if let Some(head) = line.strip_prefix("# branch.head ") {
                let named = Some(head.trim()).filter(|head| !head.is_empty() && *head != DETACHED);
                tree.branch = named.map(str::to_owned);
            } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
                tree.upstream = Some(ahead_behind(ab).ok_or_else(unread)?);
            } else if let Some(stashes) = line.strip_prefix("# stash ") {
                tree.stashes = stashes.parse().map_err(|_| unread())?;
            } else if line.starts_with("# ") {
                // `branch.oid` and `branch.upstream`, and any header a git to
                // come adds, which its manual asks to pass over.
            } else if !tree.changes.add(line) {
                return Err(unread());
            }
        }
        Ok(tree)
    }
}

/// `+<ahead> -<behind>`.
fn ahead_behind(ab: &str) -> Option<(u32, u32)> {
    let (ahead, behind) = ab.split_once(' ')?;
    Some((ahead.strip_prefix('+')?.parse().ok()?, behind.strip_prefix('-')?.parse().ok()?))
}

/// `XY`: how a path stands in the index, and in the work tree.
fn states(xy: &str) -> Option<(char, char)> {
    let mut states = xy.chars();
    let (Some(index), Some(tree), None) = (states.next(), states.next(), states.next()) else {
        return None;
    };
    Some((index, tree))
}

impl Changes {
    /// Counts a line of `git status --porcelain=v2` about a path: `1` for a
    /// changed one, `2` for a renamed or copied one, `u` for a conflict, `?`
    /// for an untracked one. The first three go on to say how it stands, `.`
    /// for unchanged. False for a line that is none of these.
    fn add(&mut self, line: &str) -> bool {
        let mut fields = line.split(' ');
        match (fields.next(), fields.next().and_then(states)) {
            (Some("1"), Some(states)) => self.add_change(states),
            (Some("2"), Some(states)) => {
                self.renamed += 1;
                self.add_change(states);
            },
            (Some("u"), Some(_)) => self.conflicted += 1,
            (Some("?"), _) => self.untracked += 1,
            _ => return false,
        }
        true
    }

    fn add_change(&mut self, (index, tree): (char, char)) {
        self.staged += u32::from(matches!(index, 'M' | 'A' | 'T'));
        self.deleted += u32::from(index == 'D') + u32::from(tree == 'D');
        // `A` in the work tree is a path added with `--intent-to-add`.
        self.modified += u32::from(matches!(tree, 'M' | 'A'));
    }
}

/// What git says of the work tree in `dir`: how it stands, or none at all, as
/// in a directory in no repository, where git fails. When git did not say, as
/// it did not answer in time, what is to be said of it instead.
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
    let ran = program::run(&mut git, LIMIT)
        .map_err(|error| format!("git status in {dir} could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            WorkTree::parse(&String::from_utf8_lossy(&stdout))
                .map_err(|why| format!("git status in {dir} said {why}"))
        },
        // git ran there and failed: the directory is in no repository, or in
        // one git cannot read.
        Ran::Ended { .. } => Ok(WorkTree::default()),
        Ran::Stuck => Err(format!("git status in {dir} did not answer in time")),
    }
}

/// Where git stands with a directory it has been asked about.
struct Asked {
    /// When it is to be asked about again.
    next: Instant,
    /// What went wrong the last time it was asked about, if anything did.
    trouble: Option<String>,
}

/// Asks git how the work tree stands in each directory a pane works in, in a
/// thread of its own, and tells the core: about each once git has rested from
/// it for a `sweep`, or for as long as it took over it if that is longer, and
/// about a directory it has said nothing of yet as soon as it is `woken`,
/// before those left. What git said of a directory it could not answer for
/// stands until it can. The thread ends once the core has.
pub fn follow(
    core: SyncSender<core::Command>,
    woken: Receiver<()>,
    sweep: Duration,
) -> io::Result<()> {
    thread::Builder::new().name("git".to_owned()).spawn(move || {
        let mut asked: HashMap<String, Asked> = HashMap::new();
        let mut troubles = Troubles::default();
        'sweep: loop {
            let Some(dirs) = core::ask(&core, core::Command::Dirs) else {
                return;
            };
            asked.retain(|dir, _| dirs.contains(dir));
            let now = Instant::now();
            let due = |dir: &&String| asked.get(*dir).is_none_or(|was| was.next <= now);
            let mut due: Vec<String> = dirs.iter().filter(due).cloned().collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|dir| asked.get(dir).map(|was| was.next));
            for dir in due {
                let started = Instant::now();
                let said = status(&dir);
                // So that a slow work tree takes up half of git's time at the
                // most, and the rest goes to the others.
                let next = Instant::now() + started.elapsed().max(sweep);
                asked.insert(dir.clone(), Asked { next, trouble: said.as_ref().err().cloned() });
                match said {
                    Ok(tree) => {
                        if core.send(core::Command::WorkTree { dir, tree }).is_err() {
                            return;
                        }
                    },
                    Err(trouble) => troubles.say(trouble),
                }
                if woken.try_recv().is_ok() {
                    continue 'sweep;
                }
            }
            // What went wrong with one that git rests from has not gone right
            // since, and is not said again.
            for trouble in asked.values().filter_map(|was| was.trouble.clone()) {
                troubles.say(trouble);
            }
            troubles.done();
            let now = Instant::now();
            let next = asked.values().map(|was| was.next.saturating_duration_since(now));
            match woken.recv_timeout(next.min().unwrap_or(sweep)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::WorkTree;

    /// What git says that does not read is no work tree, rather than one
    /// counted short: no git the tests can run says it, so it is tested here.
    /// A header that git adds for an option of its own is passed over, as its
    /// manual asks.
    #[test]
    fn a_status_that_does_not_read_is_no_work_tree() {
        let head = "# branch.oid 0123abc\n# branch.head main\n";
        let unread = [
            "# branch.ab +1",
            "# branch.ab 1 -2",
            "# stash many",
            "1 M",
            "1 .MM path",
            "u",
            "! ignored",
            "",
        ];
        for line in unread {
            assert!(WorkTree::parse(&format!("{head}{line}\n")).is_err(), "{line:?}");
        }
        let tree = WorkTree::parse(&format!("{head}# branch.to-come 1\n? new\n")).unwrap();
        assert_eq!(tree.branch.as_deref(), Some("main"));
        assert_eq!(tree.changes.untracked, 1);
    }
}
