//! Fetches the upstream branch of each branch checked out where a pane works,
//! so that how far behind it a branch is, as `git status` counts it, is of
//! what the remote has, and not of what the repository fetched last. A branch
//! is fetched once as a pane first works on it, and once a `sweep` after it
//! was fetched last, by the repository that holds it and its name: two panes
//! on one branch have one fetch. wt fetches it, as `wt follow` does, which
//! moves the remote-tracking ref only where the move loses no commit, so that
//! a rewrite the author pushed stays for `wt follow` to see. The fetch writes
//! refs, which the work tree's watch hears, and so git is asked again how the
//! work tree stands once the fetch moved them, and not before. A branch the
//! remote no longer has, as one of a pull request that merged, is not fetched
//! again while a pane works on it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use wt::cmd::follow::Upstream;

use crate::core::{self, Branch};
use crate::git;
use crate::links;
use crate::log;
use crate::program::{self, Ran};
use crate::watch::Troubles;

/// How long a fetch has: one across the network takes a while, and one that
/// takes more than a minute is stuck, as on a remote that does not answer.
const FETCHING: Duration = Duration::from_mins(1);

/// The most fetches at once, so that one remote that does not answer holds up
/// no branch of another.
const AT_ONCE: usize = 4;

/// How often the core is asked which branches panes work on, for a branch
/// new to them to be fetched soon.
const LOOK: Duration = Duration::from_secs(5);

/// What git takes as config in the fetch, whatever the repository's config
/// says: no hook and no `core.fsmonitor`, as a path in the work tree would be
/// the repository's to run; nothing written but refs, as no maintenance, no
/// commit graph and no submodule's fetch; and no prompt for a password.
const QUIET: [(&str, &str); 7] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
    ("fetch.writeCommitGraph", "false"),
    ("fetch.recurseSubmodules", "false"),
    ("credential.interactive", "false"),
];

/// When a branch was fetched last, and what wt found of its upstream then,
/// or why it did not fetch it.
struct Fetched {
    at: Instant,
    found: Result<Upstream, String>,
}

impl Fetched {
    /// When the branch is to be fetched again: a `sweep` after this, or never
    /// while a pane works on it, once its remote no longer has its upstream.
    fn due(&self, sweep: Duration) -> Option<Instant> {
        (self.found != Ok(Upstream::Gone)).then_some(self.at + sweep)
    }
}

/// Fetches the upstream branch of each branch checked out where a pane works,
/// with an upstream, in a thread of its own: as a pane first works on it, and
/// a `sweep` after it was fetched last, whether that went well or not, unless
/// its upstream is gone, which is said once. What went wrong is said once
/// while it keeps going wrong. The thread ends once the core has.
pub fn follow(core: SyncSender<core::Command>, sweep: Duration) -> io::Result<()> {
    thread::Builder::new().name("fetches".to_owned()).spawn(move || {
        let (done, finished) = mpsc::channel::<(Branch, Result<Upstream, String>)>();
        let mut fetched: HashMap<Branch, Fetched> = HashMap::new();
        let mut fetching: HashSet<Branch> = HashSet::new();
        let mut troubles = Troubles::default();
        loop {
            let Some(tracking) = core::ask(&core, core::Command::Tracking) else {
                return;
            };
            let mut wanted: BTreeMap<Branch, String> = BTreeMap::new();
            for (dir, branch) in tracking {
                wanted.entry(branch).or_insert(dir);
            }
            fetched.retain(|branch, _| wanted.contains_key(branch));
            let now = Instant::now();
            let last = |branch: &Branch| fetched.get(branch);
            let mut due: Vec<(&Branch, &String)> = wanted
                .iter()
                .filter(|(branch, _)| !fetching.contains(*branch))
                .filter(|(branch, _)| {
                    last(branch).is_none_or(|was| was.due(sweep).is_some_and(|due| due <= now))
                })
                .collect();
            // Never fetched before all others, then the one due longest.
            due.sort_by_key(|(branch, _)| last(branch).map(|was| was.at));
            let room = AT_ONCE.saturating_sub(fetching.len());
            let mut ended = Vec::new();
            for (branch, dir) in due.into_iter().take(room) {
                let name = branch.name.as_str();
                let about = format!("git did not fetch the upstream of {name} in {dir}");
                let (dir, done, of) = (dir.clone(), done.clone(), branch.clone());
                let fetch = move || {
                    let found = fetch(&dir, of.name.as_str());
                    let found = found.map_err(|why| format!("{about}: {why}"));
                    let _ = done.send((of, found));
                };
                match thread::Builder::new().name("fetch".to_owned()).spawn(fetch) {
                    Ok(_) => {
                        fetching.insert(branch.clone());
                    },
                    Err(error) => {
                        let trouble = format!("no thread to fetch {name} in: {error}");
                        ended.push((branch.clone(), Err(trouble)));
                    },
                }
            }
            let next = wanted
                .keys()
                .filter(|branch| !fetching.contains(*branch))
                .filter_map(|branch| last(branch)?.due(sweep))
                .map(|due| due.saturating_duration_since(now))
                .fold(LOOK, Duration::min);
            match finished.recv_timeout(next) {
                Ok(one) => ended.push(one),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {},
            }
            ended.extend(finished.try_iter());
            for (branch, found) in ended {
                fetching.remove(&branch);
                if found == Ok(Upstream::Gone) {
                    let (name, repository) = (&branch.name, &branch.repository);
                    log::say(&format!(
                        "the remote no longer has the upstream of {} in {}: it is not fetched \
                         again while a pane works on it",
                        name.as_str(),
                        repository.as_str()
                    ));
                }
                fetched.insert(branch, Fetched { at: Instant::now(), found });
            }
            // What went wrong with a branch that waits for its next fetch has
            // not gone right since, and is not said again.
            for trouble in fetched.values().filter_map(|was| was.found.clone().err()) {
                troubles.say(trouble);
            }
            troubles.done();
        }
    })?;
    Ok(())
}

/// Has luneta fetch the upstream branch of `branch`, of the repository `dir`
/// is in, as a program of its own, alone, as git may ask for a passphrase on
/// the terminal of its session, and with nothing of the work tree's run: what
/// wt found of it, or why it did not.
fn fetch(dir: &str, branch: &str) -> Result<Upstream, String> {
    let mut luneta = Command::new(links::luneta()?);
    git::local(git::configured(&mut luneta, &QUIET).args(["fetch-upstream", "--", dir, branch]))
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SSH_ASKPASS_REQUIRE", "never")
        .env("GIT_OPTIONAL_LOCKS", "0");
    let ran = program::run_alone(&mut luneta, FETCHING)
        .map_err(|error| format!("luneta fetch-upstream could not be run: {error}"))?;
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => {
            let printed = String::from_utf8_lossy(&stdout);
            let word = printed.trim_end();
            links::upstream_of_word(word)
                .ok_or_else(|| format!("luneta fetch-upstream said {word:?}, which is no answer"))
        },
        Ran::Ended { status, stderr, .. } => {
            let said = String::from_utf8_lossy(&stderr);
            Err(links::failure(&said).unwrap_or_else(|| format!("luneta fetch-upstream: {status}")))
        },
        Ran::Stuck => Err("it did not end in time".to_owned()),
    }
}
