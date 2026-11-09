//! The pull request of the branch checked out where each pane works, as
//! `gh pr view` finds it on GitHub.
//!
//! GitHub tells no one as a pull request opens, or is merged, so each branch
//! is asked about again once gh has rested from it for a sweep, and a branch
//! it has said nothing of yet at once. gh asks across the network, and each
//! ask counts against the hourly limit GitHub puts on the user, which every
//! other gh they run shares: panes on one branch of one repository are one
//! ask, however many directories of it they work in.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::panic::{self, AssertUnwindSafe};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use luneta_protocol::github::{PullRequest, State, Verdict};
use serde::Deserialize;

use crate::checks;
use crate::core;
use crate::git::{self, NamedHead};
use crate::links::{self, Remotes};
use crate::log;
use crate::program;
use crate::watch::Troubles;

/// How long gh has to say: it asks GitHub, across the network.
const LIMIT: Duration = Duration::from_secs(30);

/// How many branches gh is asked about at once, so that one it does not
/// answer for in time holds up none of the others.
const ASKS_AT_ONCE: usize = 4;

/// How gh starts what it says of a branch with no pull request, which is no
/// failure to the daemon: gh fails the same way at anything else.
const NONE: [&str; 2] = ["no pull requests found", "GraphQL: Could not resolve to a PullRequest"];

/// The pull request gh found of the branch checked out in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The branch, as it was checked out where gh was asked, when it answered.
    pub branch: String,
    pub pull_request: PullRequest,
}

/// What gh is asked of a pull request: `--json` takes them so, the checks of
/// its head with them, which cost no other ask.
const VIEWED: &str = concat!(
    "number,state,isDraft,url,headRefName,headRefOid,author,headRepository,headRepositoryOwner,",
    "statusCheckRollup",
);

/// A pull request as `gh pr view --json` writes what `VIEWED` asks of it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    number: NonZeroU32,
    state: Stands,
    /// Whether it is a draft, which a closed one can still be.
    is_draft: bool,
    /// Its page, of the repository gh found it in.
    url: String,
    head_ref_name: String,
    head_ref_oid: String,
    author: Option<Owner>,
    head_repository: Option<HeadRepository>,
    head_repository_owner: Option<Owner>,
    /// The checks of its head, as `checks` reads them.
    #[serde(default)]
    status_check_rollup: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct HeadRepository {
    name: String,
}

/// A pull request's state, as GitHub spells it.
#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Stands {
    Open,
    Closed,
    Merged,
}

/// A pull request, with what tells whose head it is, for `prunes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pulled {
    pub pull_request: PullRequest,
    pub head_name: String,
    pub head: String,
    pub author: Option<String>,
    pub head_repository: Option<RepositoryName>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryName {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepositoryName {
    #[must_use]
    pub fn is_at(&self, url: &str) -> bool {
        links::remote_is_of(url, (&self.host, &self.owner, &self.name))
    }
}

impl fmt::Display for RepositoryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.host, self.owner, self.name)
    }
}

fn host_of(url: &str) -> Option<&str> {
    url.split_once("://")?.1.split('/').next().filter(|host| !host.is_empty())
}

/// What gh wrote of a pull request, or why it does not read.
fn parse(said: &[u8]) -> Result<Pulled, String> {
    let viewed: Viewed = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let state = match (viewed.state, viewed.is_draft) {
        (Stands::Open, false) => State::Open,
        (Stands::Open, true) => State::Draft,
        (Stands::Merged, _) => State::Merged,
        (Stands::Closed, _) => State::Closed,
    };
    let host = host_of(&viewed.url).ok_or_else(|| format!("{} is no page", viewed.url))?;
    let host = host.to_owned();
    let owner = viewed.head_repository_owner.map(|owner| owner.login);
    let name = viewed.head_repository.map(|repository| repository.name);
    let head_repository = match (owner, name) {
        (Some(owner), Some(name)) if !owner.is_empty() && !name.is_empty() => {
            Some(RepositoryName { host, owner, name })
        },
        _ => None,
    };
    Ok(Pulled {
        pull_request: PullRequest {
            number: viewed.number,
            state,
            url: Some(viewed.url),
            checks: checks::rolled_up(viewed.status_check_rollup),
            stale: false,
        },
        head_name: viewed.head_ref_name,
        head: viewed.head_ref_oid,
        author: viewed.author.map(|author| author.login).filter(|login| !login.is_empty()),
        head_repository,
    })
}

const REPOSITORY: &str = "parent,url,defaultBranchRef";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    default_branch_ref: BranchRef,
    parent: Option<Parent>,
    url: String,
}

#[derive(Deserialize)]
struct BranchRef {
    name: String,
}

#[derive(Deserialize)]
struct Parent {
    name: String,
    owner: Owner,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fork {
    parent: RepositoryName,
    default_branch: String,
}

impl Fork {
    fn parent_may_have(&self, head: &NamedHead) -> bool {
        match head {
            NamedHead::OnRemote => true,
            NamedHead::PullRef { remote_url } => self.parent.is_at(remote_url),
        }
    }
}

fn fork_of(said: &[u8]) -> Result<Option<Fork>, String> {
    let repository: Repository = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let Some(Parent { name, owner: Owner { login } }) = repository.parent else {
        return Ok(None);
    };
    if !links::named(&login) || !links::named(&name) {
        return Err(format!("{login}/{name} names no repository"));
    }
    let host = host_of(&repository.url).ok_or_else(|| format!("{} is no page", repository.url))?;
    Ok(Some(Fork {
        parent: RepositoryName { host: host.to_owned(), owner: login, name },
        default_branch: repository.default_branch_ref.name,
    }))
}

/// What gh said of the branch it was asked about.
enum Answer {
    /// The branch was still checked out as gh answered: the pull request gh
    /// found of it, or none.
    Stayed(Option<Pulled>),
    Partly {
        own: Option<Pulled>,
        why: String,
    },
    /// Another branch was checked out by the time gh answered, which git has
    /// yet to see: what gh said was of that one.
    Moved,
}

fn find(dir: &str, base: Option<&str>) -> Result<Option<Pulled>, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", "--json", VIEWED]).current_dir(dir));
    let asked = match base {
        Some(base) => {
            gh.env("GH_REPO", base);
            format!("gh pr view of {base} in {dir}")
        },
        None => format!("gh pr view in {dir}"),
    };
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    match ran.answer() {
        Ok(said) => parse(&said)
            .map(Some)
            .map_err(|why| format!("{asked} said what does not read: {why}")),
        Err(why) if NONE.iter().any(|none| why.starts_with(none)) => Ok(None),
        Err(why) => Err(format!("{asked}: {why}")),
    }
}

fn fork(dir: &str, repository: &str, forks: &Forks) -> Result<Option<Fork>, String> {
    let remotes = match links::remotes_gh_reads(dir) {
        Remotes::Listed(listed) => listed,
        Remotes::Unlisted => return Ok(None),
        Remotes::Unsaid(why) => return Err(format!("git did not say the remotes of {dir}: {why}")),
    };
    let lineage = forks.of(repository);
    let mut lineage = locked(&lineage);
    if let Some(known) = lineage.as_ref()
        && known.remotes == remotes
    {
        return Ok(known.fork.clone());
    }
    let mut gh = Command::new("gh");
    git::local(gh.args(["repo", "view", "--json", REPOSITORY]).current_dir(dir));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh repo view in {dir}: {why}"))?;
    let fork = fork_of(&said)
        .map_err(|why| format!("gh repo view in {dir} said what does not read: {why}"))?;
    *lineage = Some(Lineage { remotes, fork: fork.clone() });
    Ok(fork)
}

fn in_parent(
    dir: &str,
    repository: &str,
    branch: &str,
    forks: &Forks,
) -> Result<Option<Pulled>, String> {
    let Some(head) = git::named_head(dir, branch)? else {
        return Ok(None);
    };
    let Some(fork) = fork(dir, repository, forks)?.filter(|fork| fork.parent_may_have(&head)) else {
        return Ok(None);
    };
    let done = |found: &Pulled| matches!(found.pull_request.state, State::Merged | State::Closed);
    let stale = |found: &Pulled| done(found) && branch == fork.default_branch;
    Ok(find(dir, Some(&fork.parent.to_string()))?.filter(|found| !stale(found)))
}

/// What gh says of the pull request of `branch`, checked out in `dir`: the
/// one it finds, or none, or that `branch` is no longer checked out there.
/// When gh did not say, as it is not logged in, or did not answer in time,
/// what is to be said of it instead.
///
/// gh finds it as it does in a shell in `dir`, or else in the repository that
/// one forks, for a branch git names a head of: by the branch's config for
/// where it pushes the branch. It reads what is checked out as it starts,
/// where git can have last looked a sweep before, so what is checked out is
/// read again once gh has answered.
fn view(dir: &str, (repository, branch): &Branch, forks: &Forks) -> Result<Answer, String> {
    let own = find(dir, None)?;
    let settled = own.as_ref().is_some_and(|found| found.pull_request.state != State::Closed);
    let beyond = if settled { Ok(None) } else { in_parent(dir, repository, branch, forks) };
    if git::branch(dir).as_deref() != Some(branch.as_str()) {
        return Ok(Answer::Moved);
    }
    Ok(match beyond {
        Ok(beyond) => Answer::Stayed(beyond.or(own)),
        Err(why) => Answer::Partly { own, why },
    })
}

fn standing(was: Option<PullRequest>, own: Option<PullRequest>) -> Option<PullRequest> {
    match (was, own) {
        (Some(was), Some(own)) if was.url == own.url => Some(own),
        (was, own) => was.or(own),
    }
}

/// A branch of a repository, by the repository's common directory and the
/// branch's name.
type Branch = (String, String);

/// Asks gh for what `prunes` needs of it, which only a sweep you asked for
/// does: one `view` for each worktree, as `follow` asks of a branch.
#[derive(Default)]
pub struct Asker {
    forks: Forks,
}

impl Asker {
    pub fn pull_of(
        &mut self,
        dir: &str,
        repository: &str,
        branch: &str,
    ) -> Result<Option<Pulled>, String> {
        let asked = (repository.to_owned(), branch.to_owned());
        match view(dir, &asked, &self.forks)? {
            Answer::Stayed(found) => Ok(found),
            Answer::Partly { why, .. } => Err(why),
            Answer::Moved => Err(format!("{dir} moved off {branch} as gh answered")),
        }
    }

    pub fn open_on_parent(
        &mut self,
        dir: &str,
        repository: &str,
        branch: &str,
    ) -> Result<Option<u32>, String> {
        let found = in_parent(dir, repository, branch, &self.forks)?;
        let open = |found: &Pulled| matches!(found.pull_request.state, State::Open | State::Draft);
        Ok(found.filter(open).map(|found| found.pull_request.number.get()))
    }
}

pub fn default_branch(dir: &str) -> Result<String, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["repo", "view", "--json", REPOSITORY]).current_dir(dir));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh repo view in {dir}: {why}"))?;
    let repository: Repository = serde_json::from_slice(&said)
        .map_err(|why| format!("gh repo view in {dir} said what does not read: {why}"))?;
    Ok(repository.default_branch_ref.name)
}

#[derive(Deserialize)]
struct Numbered {
    number: u32,
    state: Stands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Based {
    Open(u32),
    Merged(u32),
    Neither,
}

/// What is based on `branch` of `repository`, of its newest 100.
pub fn based_on(dir: &str, repository: &RepositoryName, branch: &str) -> Result<Based, String> {
    let asked = format!("gh pr list of {repository} based on {branch}");
    let mut gh = Command::new("gh");
    gh.args(["pr", "list", "--base", branch, "--state", "all", "--limit", "100"]);
    gh.args(["--json", "number,state"]).current_dir(dir).env("GH_REPO", repository.to_string());
    git::local(&mut gh);
    let ran =
        program::run(&mut gh, LIMIT).map_err(|error| format!("{asked} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("{asked}: {why}"))?;
    let listed: Vec<Numbered> = serde_json::from_slice(&said)
        .map_err(|why| format!("{asked} said what does not read: {why}"))?;
    let open = listed.iter().find(|pull| matches!(pull.state, Stands::Open));
    let merged = listed.iter().find(|pull| matches!(pull.state, Stands::Merged));
    Ok(match (open, merged) {
        (Some(open), _) => Based::Open(open.number),
        (None, Some(merged)) => Based::Merged(merged.number),
        (None, None) => Based::Neither,
    })
}

/// Who gh is logged in as on `host`.
pub fn logged_in(host: &str) -> Result<String, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["api", "user", "--hostname", host, "--jq", ".login"]));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh api user could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh api user: {why}"))?;
    let login = String::from_utf8_lossy(&said).trim().to_owned();
    if login.is_empty() {
        return Err("gh api user named no one".to_owned());
    }
    Ok(login)
}

struct Lineage {
    remotes: String,
    fork: Option<Fork>,
}

/// What gh said each repository is a fork of, by its common directory, with
/// the remotes it said it for. An ask holds a repository's lock as gh is asked
/// about it, so that asks of one repository at once ask gh once.
#[derive(Default)]
struct Forks(Mutex<HashMap<String, Arc<Mutex<Option<Lineage>>>>>);

impl Forks {
    fn of(&self, repository: &str) -> Arc<Mutex<Option<Lineage>>> {
        Arc::clone(locked(&self.0).entry(repository.to_owned()).or_default())
    }

    fn retain(&self, keep: impl Fn(&str) -> bool) {
        locked(&self.0).retain(|repository, _| keep(repository));
    }
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long gh rests from a branch before it is asked about it again.
#[derive(Clone, Copy, Debug)]
pub struct Rests {
    /// From one whose pull request has checks yet to end: they are what
    /// change, and soon.
    pub pending: Duration,
    /// From any other.
    pub settled: Duration,
}

/// What gh said of a branch the last time it answered: when, and the pull
/// request it found, if it found one.
struct Said {
    at: Instant,
    found: Option<PullRequest>,
}

/// Where gh stands with a branch it has been asked about.
struct Asked {
    /// When it is to be asked about again.
    next: Instant,
    /// What it said the last time it answered, if it has.
    said: Option<Said>,
    /// How many times it failed to answer since.
    failed: u32,
    /// What went wrong the last time it was asked about, if anything did.
    trouble: Option<String>,
}

impl Asked {
    /// The pull request gh found last, stale where it failed since.
    fn found(&self) -> Option<PullRequest> {
        let found = self.said.as_ref()?.found.clone()?;
        Some(PullRequest { stale: self.failed > 0, ..found })
    }
}

/// Where gh is with a branch: resting from it, or asking about it, from
/// where it stood with it before, if it had been asked about it.
enum Ask {
    Resting(Asked),
    Asking(Option<Asked>),
}

impl Ask {
    fn asked(&self) -> Option<&Asked> {
        match self {
            Ask::Resting(asked) => Some(asked),
            Ask::Asking(was) => was.as_ref(),
        }
    }

    fn into_asked(self) -> Option<Asked> {
        match self {
            Ask::Resting(asked) => Some(asked),
            Ask::Asking(was) => was,
        }
    }

    /// When it is to be asked about again: none while it is.
    fn next(&self) -> Option<Instant> {
        match self {
            Ask::Resting(asked) => Some(asked.next),
            Ask::Asking(_) => None,
        }
    }
}

/// What gh said of a branch, from the thread that asked it.
struct Answered {
    branch: Branch,
    said: Result<Answer, String>,
}

/// Where gh stands with a branch once it said `said` of it, from where it
/// stood with it before, if it had been asked about it: what it found last
/// stands, as stale, where it failed. One whose checks are pending is asked
/// about again the soonest.
fn answered(was: Option<Asked>, said: Result<Answer, String>, rests: Rests) -> Asked {
    let now = Instant::now();
    let (before, failed) = was.map_or((None, 0), |was| (was.said, was.failed));
    let of = |found: Option<Pulled>| found.map(|found| found.pull_request);
    let fresh = |found: Option<PullRequest>, trouble| {
        let change = |was| checks_changed(was, found.as_ref(), failed, now);
        if let Some(change) = before.as_ref().and_then(change) {
            log::say(&change);
        }
        (Some(Said { at: now, found }), trouble, 0)
    };
    let (said, trouble, failed) = match said {
        Ok(Answer::Stayed(found)) => fresh(of(found), None),
        // The core wakes the thread once git sees the checkout.
        Ok(Answer::Moved) => (before, None, failed),
        Ok(Answer::Partly { own, why }) => {
            let own = of(own);
            let was = before.as_ref().and_then(|was| was.found.clone());
            if standing(was, own.clone()) == own {
                fresh(own, Some(why))
            } else {
                (before, Some(why), failed.saturating_add(1))
            }
        },
        Err(why) => (before, Some(why), failed.saturating_add(1)),
    };
    let found = said.as_ref().and_then(|said| said.found.as_ref());
    let pending = found.is_some_and(|found| found.verdict() == Some(Verdict::Pending));
    let rest = if pending { rests.pending } else { rests.settled };
    Asked { next: now + rest, said, failed, trouble }
}

/// What to say where the checks of a pull request come to otherwise than
/// they did the last time gh said them, `was`: with how long it had been, and
/// how many times gh failed to say them in between, which tells a slow sweep
/// from a failed one.
fn checks_changed(
    was: &Said,
    now: Option<&PullRequest>,
    failed: u32,
    at: Instant,
) -> Option<String> {
    let (said, was, now) = (was.at, was.found.as_ref()?, now?);
    if (was.number, &was.url) != (now.number, &now.url) {
        return None;
    }
    let verdict = |pull_request: &PullRequest| pull_request.checks.and_then(|it| it.verdict());
    let (before, after) = (verdict(was), verdict(now));
    if before == after {
        return None;
    }
    let word = |verdict| match verdict {
        Some(Verdict::Failing) => "failing",
        Some(Verdict::Pending) => "pending",
        Some(Verdict::Passing) => "passing",
        None => "no checks",
    };
    let named = now.url.clone().unwrap_or_else(|| format!("#{}", now.number));
    let said = at.duration_since(said).as_secs();
    let failed = match failed {
        0 => String::new(),
        1 => ", failing once in between".to_owned(),
        failed => format!(", failing {failed} times in between"),
    };
    let (before, after) = (word(before), word(after));
    let said = format!(", {said}s after gh last said them");
    Some(format!("the checks of {named} went from {before} to {after}{said}{failed}"))
}

/// Asks gh for the pull request of the branch checked out in each directory
/// a pane works in, a few at a time, each in a thread of its own, and tells
/// the core: about each branch once gh has rested from it, by `rests`, and
/// about one it has said nothing of yet as soon as it is `woken`, before
/// those left. What gh said of a branch it could not answer for stands, as
/// stale, until it can. A thread that asked wakes it with `wake` once it has
/// an answer. It ends once the core has.
pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    rests: Rests,
) -> io::Result<()> {
    thread::Builder::new().name("gh".to_owned()).spawn(move || {
        let mut asked: HashMap<Branch, Ask> = HashMap::new();
        let forks = Arc::new(Forks::default());
        let (answer, answers) = mpsc::channel();
        let mut troubles = Troubles::default();
        loop {
            let Some(branches) = core::ask(&core, core::Command::Branches) else {
                return;
            };
            // The directories each branch is checked out in. git says the
            // repository of each afresh: a directory a pane stays in can be
            // cloned over, with a branch of the same name checked out.
            let mut on: BTreeMap<Branch, Vec<String>> = BTreeMap::new();
            for (dir, branch) in branches {
                if let git::Common::Of(repository) = git::common(&dir) {
                    on.entry((repository, branch)).or_default().push(dir);
                }
            }
            while let Ok(Answered { branch, said }) = answers.try_recv() {
                let was = asked.remove(&branch).and_then(Ask::into_asked);
                asked.insert(branch, Ask::Resting(answered(was, said, rests)));
            }
            let asking = |ask: &Ask| matches!(ask, Ask::Asking(_));
            asked.retain(|branch, ask| on.contains_key(branch) || asking(ask));
            forks.retain(|repository| on.keys().any(|(of, _)| of == repository));
            // A directory that came to a branch gh has said of already.
            if !tell(&core, &on, &asked) {
                return;
            }
            let now = Instant::now();
            let due = |branch: &&Branch| {
                asked.get(*branch).is_none_or(|ask| ask.next().is_some_and(|next| next <= now))
            };
            let mut due: Vec<&Branch> = on.keys().filter(due).collect();
            // Never asked about before all others, then the one due longest.
            due.sort_by_key(|branch| asked.get(*branch).and_then(Ask::next));
            let free = |asked: &HashMap<Branch, Ask>| {
                ASKS_AT_ONCE.saturating_sub(asked.values().filter(|ask| asking(ask)).count())
            };
            for branch in due.into_iter().take(free(&asked)) {
                let dir = on[branch][0].clone();
                let (of, forks) = (branch.clone(), Arc::clone(&forks));
                let (answer, wake) = (answer.clone(), wake.clone());
                let asks = thread::Builder::new().name("gh ask".to_owned()).spawn(move || {
                    let said = panic::catch_unwind(AssertUnwindSafe(|| view(&dir, &of, &forks)))
                        .unwrap_or_else(|_| Err(format!("gh's ask about {} panicked", of.1)));
                    if answer.send(Answered { branch: of, said }).is_ok() {
                        let _ = wake.try_send(());
                    }
                });
                let was = asked.remove(branch).and_then(Ask::into_asked);
                let ask = match asks {
                    Ok(_) => Ask::Asking(was),
                    Err(error) => {
                        let why = format!("gh could not be asked about {}: {error}", branch.1);
                        Ask::Resting(answered(was, Err(why), rests))
                    },
                };
                asked.insert(branch.clone(), ask);
            }
            // What went wrong with one that gh rests from has not gone right
            // since, and is not said again.
            for trouble in asked.values().filter_map(|ask| ask.asked()?.trouble.clone()) {
                troubles.say(trouble);
            }
            troubles.done();
            let now = Instant::now();
            let next = if free(&asked) > 0 {
                let next = asked.values().filter_map(Ask::next);
                next.map(|next| next.saturating_duration_since(now)).min()
            } else {
                None
            };
            match woken.recv_timeout(next.unwrap_or(rests.settled)) {
                Ok(()) | Err(RecvTimeoutError::Timeout) => {},
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    })?;
    Ok(())
}

/// Tells the core the pull request gh found last for the branch in each
/// directory `on` has it checked out in. False once the core has stopped.
fn tell(
    core: &SyncSender<core::Command>,
    on: &BTreeMap<Branch, Vec<String>>,
    asked: &HashMap<Branch, Ask>,
) -> bool {
    let mut found = BTreeMap::new();
    for (branch, dirs) in on {
        let Some(pull_request) = asked.get(branch).and_then(Ask::asked).and_then(Asked::found)
        else {
            continue;
        };
        let of = Found { branch: branch.1.clone(), pull_request };
        found.extend(dirs.iter().map(|dir| (dir.clone(), of.clone())));
    }
    core.send(core::Command::PullRequests(found)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, Instant};

    use luneta_protocol::github::{Checks, PullRequest, State};

    use super::{
        answered, checks_changed, fork_of, parse, standing, Answer, Asked, Pulled, RepositoryName,
        Rests, Said,
    };

    fn pull(repository: &str, number: u32, state: State, checks: Option<Checks>) -> PullRequest {
        PullRequest {
            number: NonZeroU32::new(number).unwrap(),
            state,
            url: Some(format!("https://github.com/{repository}/pull/{number}")),
            checks,
            stale: false,
        }
    }

    /// Where gh fails to say all of a branch, what it said of it before
    /// stands as stale, and counts as a failure: unless what it said of the
    /// fork's stands, which is fresh. A branch whose checks are pending is
    /// asked about again sooner, stale or not, and one that moved off its
    /// branch stays as stale as it was.
    #[test]
    fn what_gh_failed_to_say_again_stands_as_stale() {
        let rests = Rests { pending: Duration::from_secs(1), settled: Duration::from_hours(1) };
        let pending = Some(Checks { pending: 1, failed: 0, passed: 0 });
        let parent_s = pull("upstream/repo", 5, State::Open, pending);
        let at = Instant::now();
        let was = |found: &PullRequest| Asked {
            next: at,
            said: Some(Said { at, found: Some(found.clone()) }),
            failed: 1,
            trouble: None,
        };
        let pulled = |pull_request| Pulled {
            pull_request,
            head_name: "fix".to_owned(),
            head: "abc".to_owned(),
            author: None,
            head_repository: None,
        };
        let partly = |own| Ok(Answer::Partly { own, why: "the parent did not say".to_owned() });
        let said = |asked: &Asked| asked.said.as_ref().map(|said| said.at);

        let asked = answered(Some(was(&parent_s)), partly(None), rests);
        let stale = PullRequest { stale: true, ..parent_s.clone() };
        assert_eq!((asked.found(), said(&asked), asked.failed), (Some(stale), Some(at), 2));
        assert_eq!(asked.trouble.as_deref(), Some("the parent did not say"));
        assert!(asked.next < Instant::now() + Duration::from_secs(2));

        let closed = pull("owner/repo", 4, State::Closed, None);
        let fork_s = pull("owner/repo", 4, State::Open, pending);
        let asked = answered(Some(was(&fork_s)), partly(Some(pulled(closed.clone()))), rests);
        assert_eq!((asked.found(), asked.failed), (Some(closed), 0));
        assert!(said(&asked).is_some_and(|now| now > at));
        assert!(asked.next > Instant::now() + Duration::from_mins(59));

        let asked = answered(Some(was(&fork_s)), Ok(Answer::Moved), rests);
        let stale = PullRequest { stale: true, ..fork_s };
        assert_eq!((asked.found(), asked.trouble, asked.failed), (Some(stale), None, 1));
    }

    /// What the daemon says as a pull request's checks come to otherwise:
    /// nothing where they come to the same, or it is another pull request, or
    /// gh found none before.
    #[test]
    fn a_change_in_the_checks_says_how_long_gh_took_to_see_it() {
        let checks = |pending, failed, passed| Some(Checks { pending, failed, passed });
        let open = |checks| pull("owner/repo", 7, State::Open, checks);
        let at = Instant::now();
        let said = |found| Said { at: at.checked_sub(Duration::from_secs(62)).unwrap(), found };
        let changed = |was: &PullRequest, now: &PullRequest, failed| {
            checks_changed(&said(Some(was.clone())), Some(now), failed, at)
        };
        let of = "the checks of https://github.com/owner/repo/pull/7 went from";
        assert_eq!(
            changed(&open(checks(1, 0, 2)), &open(checks(0, 0, 3)), 0),
            Some(format!("{of} pending to passing, 62s after gh last said them"))
        );
        assert_eq!(
            changed(&open(checks(0, 0, 0)), &open(checks(0, 1, 0)), 3),
            Some(format!(
                "{of} no checks to failing, 62s after gh last said them, failing 3 times in between"
            ))
        );
        assert_eq!(changed(&open(checks(2, 0, 1)), &open(checks(1, 0, 2)), 0), None);
        let other = pull("owner/repo", 8, State::Open, checks(0, 0, 3));
        assert_eq!(changed(&open(checks(1, 0, 0)), &other, 0), None);
        assert_eq!(checks_changed(&said(None), Some(&other), 0, at), None);
    }

    #[test]
    fn a_parent_that_does_not_say_leaves_its_own_and_takes_news_of_the_fork_s() {
        let pull = |repository: &str, number, state| pull(repository, number, state, None);
        let fork_s = |state| pull("owner/repo", 5, state);
        let parent_s = || pull("upstream/repo", 5, State::Open);
        let closed = || fork_s(State::Closed);
        assert_eq!(standing(Some(fork_s(State::Open)), Some(closed())), Some(closed()));
        assert_eq!(standing(Some(parent_s()), None), Some(parent_s()));
        assert_eq!(standing(Some(parent_s()), Some(closed())), Some(parent_s()));
        assert_eq!(standing(None, Some(closed())), Some(closed()));
        assert_eq!(standing(None, None), None);
    }

    #[test]
    fn a_fork_s_parent_reads_as_gh_writes_it_on_the_fork_s_host() {
        let parent = r#"{"id":"R","name":"cdk","owner":{"id":"O","login":"cashubtc"}}"#;
        let default = r#""defaultBranchRef":{"name":"trunk"}"#;
        let forked = |url: &str| format!(r#"{{{default},"parent":{parent},"url":"{url}"}}"#);
        let read = |said: &str| fork_of(said.as_bytes());
        let parent_of =
            |said: &str| read(said).map(|fork| fork.map(|fork| fork.parent.to_string()));
        let on = |host: &str| Ok(Some(format!("{host}/cashubtc/cdk")));
        assert_eq!(parent_of(&forked("https://github.com/lorenzolfm/cdk")), on("github.com"));
        assert_eq!(
            parent_of(&forked("https://git.example.com/lorenzolfm/cdk")),
            on("git.example.com")
        );
        let default_branch = read(&forked("https://github.com/lorenzolfm/cdk"))
            .map(|fork| fork.map(|fork| fork.default_branch));
        assert_eq!(default_branch, Ok(Some("trunk".to_owned())));
        let none = format!(r#"{{{default},"parent":null,"url":"https://github.com/lorenzolfm/luneta"}}"#);
        assert_eq!(read(&none), Ok(None));
        let unnamed = |name: &str, login: &str| {
            let parent = format!(r#"{{"name":"{name}","owner":{{"login":"{login}"}}}}"#);
            format!(r#"{{{default},"parent":{parent},"url":"https://github.com/lorenzolfm/cdk"}}"#)
        };
        let unread = [
            forked("nowhere"),
            forked("no/where/at"),
            forked("https:///lorenzolfm/cdk"),
            unnamed("", "cashubtc"),
            unnamed("cdk", ""),
            unnamed("cdk", "cashu/btc"),
            format!(r#"{{"parent":{parent},"url":"https://github.com/lorenzolfm/cdk"}}"#),
            String::new(),
        ];
        for unread in unread {
            assert!(read(&unread).is_err(), "{unread:?}");
        }
    }

    /// The head goes by the repository it is on, on the host of the pull
    /// request's page: a fork's, deleted since, has none, and a ghost no author.
    #[test]
    fn a_pull_request_says_whose_head_it_is() {
        let viewed = |author: &str, repository: &str, owner: &str| {
            let said = format!(
                r#"{{"isDraft":false,"number":1,"state":"MERGED","headRefName":"fix",
                "headRefOid":"abc","author":{author},"headRepository":{repository},
                "headRepositoryOwner":{owner},"url":"https://git.example.com/up/repo/pull/1"}}"#
            );
            parse(said.as_bytes()).unwrap()
        };
        let me = r#"{"id":"U","is_bot":false,"login":"me","name":"Me"}"#;
        let repository = r#"{"id":"R","name":"repo"}"#;
        let Pulled { head_name, head, author, head_repository, .. } = viewed(me, repository, me);
        assert_eq!((head_name.as_str(), head.as_str()), ("fix", "abc"));
        assert_eq!(author.as_deref(), Some("me"));
        let name = |host: &str, owner: &str, name: &str| RepositoryName {
            host: host.to_owned(),
            owner: owner.to_owned(),
            name: name.to_owned(),
        };
        assert_eq!(head_repository, Some(name("git.example.com", "me", "repo")));
        let gone = viewed(r#"{"login":""}"#, "null", "null");
        assert_eq!((gone.author, gone.head_repository), (None, None));
        let unnamed = viewed(me, r#"{"id":"","name":""}"#, r#"{"id":"","login":""}"#);
        assert_eq!(unnamed.head_repository, None);
    }

    /// A draft is open: one closed or merged is closed or merged, though
    /// GitHub still says it is a draft. What does not read, as a state GitHub
    /// has not spelled before, or a number it does not count, or one with no
    /// page, is no pull request.
    #[test]
    fn a_pull_request_reads_as_gh_writes_it() {
        let read = |said: &str| parse(said.as_bytes()).map(|pulled| pulled.pull_request);
        let state = |said: &str| read(said).map(|pull_request| pull_request.state);
        let number = NonZeroU32::new(126).unwrap();
        let url = "https://github.com/owner/repo/pull/126";
        let head = r#""headRefName":"fold","headRefOid":"abc""#;
        let open = format!(r#""isDraft":false,"number":126,"state":"OPEN",{head}"#);
        assert_eq!(
            read(&format!(r#"{{{open},"url":"{url}"}}"#)),
            Ok(PullRequest {
                number,
                state: State::Open,
                url: Some(url.to_owned()),
                checks: Some(Checks::default()),
                stale: false,
            })
        );
        let page = format!(r#"{head},"url":"https://github.com/owner/repo/pull/1""#);
        let viewed = |fields: &str| format!("{{{fields},{page}}}");
        let draft = viewed(r#""isDraft":true,"number":1,"state":"OPEN""#);
        assert_eq!(state(&draft), Ok(State::Draft));
        let merged = viewed(r#""isDraft":true,"number":1,"state":"MERGED""#);
        assert_eq!(state(&merged), Ok(State::Merged));
        let closed = viewed(r#""isDraft":true,"number":1,"state":"CLOSED""#);
        assert_eq!(state(&closed), Ok(State::Closed));
        let unread = [
            viewed(r#""isDraft":false,"number":1,"state":"QUEUED""#),
            viewed(r#""number":1,"state":"OPEN""#),
            viewed(r#""isDraft":false,"number":-1,"state":"OPEN""#),
            viewed(r#""isDraft":false,"number":0,"state":"OPEN""#),
            format!(r#"{{"isDraft":false,"number":1,"state":"OPEN",{head}}}"#),
            format!(r#"{{"isDraft":false,"number":1,"state":"OPEN",{head},"url":"nowhere"}}"#),
            r#"{"isDraft":false,"number":1,"state":"OPEN","url":"https://x/1"}"#.to_owned(),
            String::new(),
        ];
        for said in unread {
            assert!(read(&said).is_err(), "{said:?}");
        }
    }
}
