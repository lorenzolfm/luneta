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

use luneta_protocol::git::{self, WorkTree};

use crate::core;
use crate::program::{self, Ran};
use crate::watch::Troubles;

/// How long git has to say how a work tree stands. A large one takes it a
/// while, and one across the network longer; one that takes more than a
/// minute is stuck.
const LIMIT: Duration = Duration::from_mins(1);

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
