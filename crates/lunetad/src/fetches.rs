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

const FETCHING: Duration = Duration::from_mins(1);

const AT_ONCE: usize = 4;

const LOOK: Duration = Duration::from_secs(5);

const QUIET: [(&str, &str); 7] = [
    ("core.hooksPath", "/dev/null"),
    ("core.fsmonitor", "false"),
    ("gc.auto", "0"),
    ("maintenance.auto", "false"),
    ("fetch.writeCommitGraph", "false"),
    ("fetch.recurseSubmodules", "false"),
    ("credential.interactive", "false"),
];

struct Fetched {
    at: Instant,
    found: Result<Upstream, String>,
}

impl Fetched {
    fn due(&self, sweep: Duration) -> Option<Instant> {
        (self.found != Ok(Upstream::Gone)).then_some(self.at + sweep)
    }
}

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
            let next = if fetching.len() >= AT_ONCE {
                LOOK
            } else {
                wanted
                    .keys()
                    .filter(|branch| !fetching.contains(*branch))
                    .filter_map(|branch| last(branch)?.due(sweep))
                    .map(|due| due.saturating_duration_since(now))
                    .fold(LOOK, Duration::min)
            };
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
            for trouble in fetched.values().filter_map(|was| was.found.clone().err()) {
                troubles.say(trouble);
            }
            troubles.done();
        }
    })?;
    Ok(())
}

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
