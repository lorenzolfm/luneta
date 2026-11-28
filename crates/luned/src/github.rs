use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use lune_protocol::github::{PullPage, PullRequest, State, Verdict};
use serde::Deserialize;

use crate::checks;
use crate::core;
use crate::directory::Directory;
use crate::git::{self, NamedHead};
use crate::links::{self, Remotes};
use crate::log;
use crate::panics::{self, Panicked};
use crate::program;
use crate::watch::Troubles;

const LIMIT: Duration = Duration::from_secs(30);

const ASKS_AT_ONCE: usize = 4;

const NONE: [&str; 2] = [
    "no pull requests found",
    "GraphQL: Could not resolve to a PullRequest",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub branch: String,
    pub pull_request: PullRequest,
}

const VIEWED: &str = concat!(
    "state,isDraft,url,baseRefName,headRefName,headRefOid,author,headRepository,",
    "headRepositoryOwner,statusCheckRollup,mergeable",
);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    state: Stands,
    is_draft: bool,
    url: String,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
    author: Option<Owner>,
    head_repository: Option<HeadRepository>,
    head_repository_owner: Option<Owner>,
    #[serde(default)]
    status_check_rollup: Option<serde_json::Value>,
    #[serde(default)]
    mergeable: Option<Mergeability>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

impl Mergeability {
    pub(crate) fn is_conflicting(self) -> bool {
        match self {
            Mergeability::Conflicting => true,
            Mergeability::Mergeable | Mergeability::Unknown => false,
        }
    }
}

#[derive(Deserialize)]
struct HeadRepository {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Stands {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pulled {
    pub pull_request: PullRequest,
    pub base_name: String,
    pub head_name: String,
    pub head: String,
    pub author: Option<String>,
    pub head_repository: Option<RepositoryName>,
    pub mergeability_unknown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryName {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepositoryName {
    #[must_use]
    pub fn of_page(page: &PullPage) -> RepositoryName {
        let mut parts = page.repository().split('/').map(str::to_owned);
        let mut part = || {
            parts
                .next()
                .expect("a pull request's page is of host/owner/name")
        };
        RepositoryName {
            host: part(),
            owner: part(),
            name: part(),
        }
    }

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
    url.split_once("://")?
        .1
        .split('/')
        .next()
        .filter(|host| !host.is_empty())
}

fn parse(said: &[u8]) -> Result<Pulled, String> {
    let viewed: Viewed = serde_json::from_slice(said).map_err(|error| error.to_string())?;
    let state = match (viewed.state, viewed.is_draft) {
        (Stands::Open, false) => State::Open,
        (Stands::Open, true) => State::Draft,
        (Stands::Merged, _) => State::Merged,
        (Stands::Closed, _) => State::Closed,
    };
    let page = PullPage::parse(&viewed.url)?;
    let host = page.host().to_owned();
    let owner = viewed.head_repository_owner.map(|owner| owner.login);
    let name = viewed.head_repository.map(|repository| repository.name);
    let head_repository = match (owner, name) {
        (Some(owner), Some(name)) if !owner.is_empty() && !name.is_empty() => {
            Some(RepositoryName { host, owner, name })
        }
        _ => None,
    };
    let (conflicting, mergeability_unknown) = match (state, viewed.mergeable) {
        (State::Open | State::Draft, Some(Mergeability::Conflicting)) => (true, false),
        (State::Open | State::Draft, Some(Mergeability::Unknown)) => (false, true),
        (State::Open | State::Draft, Some(Mergeability::Mergeable) | None)
        | (State::Merged | State::Closed, _) => (false, false),
    };
    if matches!(state, State::Merged | State::Closed) {
        assert!(!conflicting);
        assert!(!mergeability_unknown);
    }
    if conflicting {
        assert!(!mergeability_unknown);
    }
    Ok(Pulled {
        pull_request: PullRequest {
            page,
            state,
            checks: checks::rolled_up(viewed.status_check_rollup),
            stale: false,
            conflicting,
        },
        base_name: viewed.base_ref_name,
        head_name: viewed.head_ref_name,
        head: viewed.head_ref_oid,
        author: viewed
            .author
            .map(|author| author.login)
            .filter(|login| !login.is_empty()),
        head_repository,
        mergeability_unknown,
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
    let Some(Parent {
        name,
        owner: Owner { login },
    }) = repository.parent
    else {
        return Ok(None);
    };
    if !links::named(&login) || !links::named(&name) {
        return Err(format!("{login}/{name} names no repository"));
    }
    let host = host_of(&repository.url).ok_or_else(|| format!("{} is no page", repository.url))?;
    Ok(Some(Fork {
        parent: RepositoryName {
            host: host.to_owned(),
            owner: login,
            name,
        },
        default_branch: repository.default_branch_ref.name,
    }))
}

enum Answer {
    Stayed(Option<Pulled>),
    Partly { own: Option<Pulled>, why: String },
    Moved,
}

fn find(dir: &str, base: Option<&str>) -> Result<Option<Pulled>, String> {
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", "--json", VIEWED]).current_dir(dir));
    let asked = match base {
        Some(base) => {
            gh.env("GH_REPO", base);
            format!("gh pr view of {base} in {dir}")
        }
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

fn fork(dir: &str, repository: &Directory, forks: &Forks) -> Result<Option<Fork>, String> {
    let remotes = match links::remotes_gh_reads(dir) {
        Remotes::Listed(listed) | Remotes::InBare(listed) => listed,
        Remotes::Unlisted => return Ok(None),
        Remotes::Unsaid(why) => return Err(format!("git did not say the remotes of {dir}: {why}")),
    };
    let lineage = forks.of(repository);
    let mut lineage = unpoisoned(&lineage);
    if let Some(known) = lineage.as_ref()
        && known.remotes == remotes
    {
        return Ok(known.fork.clone());
    }
    let mut gh = Command::new("gh");
    git::local(
        gh.args(["repo", "view", "--json", REPOSITORY])
            .current_dir(dir),
    );
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran
        .answer()
        .map_err(|why| format!("gh repo view in {dir}: {why}"))?;
    let fork = fork_of(&said)
        .map_err(|why| format!("gh repo view in {dir} said what does not read: {why}"))?;
    *lineage = Some(Lineage {
        remotes,
        fork: fork.clone(),
    });
    Ok(fork)
}

fn in_parent(
    dir: &str,
    repository: &Directory,
    branch: &str,
    forks: &Forks,
) -> Result<Option<Pulled>, String> {
    let Some(head) = git::named_head(dir, branch)? else {
        return Ok(None);
    };
    let Some(fork) = fork(dir, repository, forks)?.filter(|fork| fork.parent_may_have(&head))
    else {
        return Ok(None);
    };
    let done = |found: &Pulled| matches!(found.pull_request.state, State::Merged | State::Closed);
    let stale = |found: &Pulled| done(found) && branch == fork.default_branch;
    Ok(find(dir, Some(&fork.parent.to_string()))?.filter(|found| !stale(found)))
}

fn view(dir: &str, (repository, branch): &Branch, forks: &Forks) -> Result<Answer, String> {
    let own = find(dir, None)?;
    let settled = own
        .as_ref()
        .is_some_and(|found| found.pull_request.state != State::Closed);
    let beyond = if settled {
        Ok(None)
    } else {
        in_parent(dir, repository, branch, forks)
    };
    if git::branch(dir)?.as_deref() != Some(branch.as_str()) {
        return Ok(Answer::Moved);
    }
    Ok(match beyond {
        Ok(beyond) => Answer::Stayed(beyond.or(own)),
        Err(why) => Answer::Partly { own, why },
    })
}

fn standing(was: Option<PullRequest>, own: Option<PullRequest>) -> Option<PullRequest> {
    match (was, own) {
        (Some(was), Some(own)) if was.page == own.page => Some(own),
        (was, own) => was.or(own),
    }
}

type Branch = (Directory, String);

#[derive(Default)]
pub struct Asker {
    forks: Forks,
}

impl Asker {
    pub fn pull_of(
        &mut self,
        dir: &str,
        repository: &Directory,
        branch: &str,
    ) -> Result<Option<Pulled>, String> {
        let asked = (repository.clone(), branch.to_owned());
        match view(dir, &asked, &self.forks)? {
            Answer::Stayed(found) => Ok(found),
            Answer::Partly { why, .. } => Err(why),
            Answer::Moved => Err(format!("{dir} moved off {branch} as gh answered")),
        }
    }

    pub fn open_on_parent(
        &mut self,
        dir: &str,
        repository: &Directory,
        branch: &str,
    ) -> Result<Option<u32>, String> {
        let found = in_parent(dir, repository, branch, &self.forks)?;
        let open = |found: &Pulled| matches!(found.pull_request.state, State::Open | State::Draft);
        Ok(found
            .filter(open)
            .map(|found| found.pull_request.number().get()))
    }
}

pub fn default_branch(dir: &str) -> Result<String, String> {
    let mut gh = Command::new("gh");
    git::local(
        gh.args(["repo", "view", "--json", REPOSITORY])
            .current_dir(dir),
    );
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh repo view in {dir} could not be run: {error}"))?;
    let said = ran
        .answer()
        .map_err(|why| format!("gh repo view in {dir}: {why}"))?;
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

pub fn based_on(dir: &str, repository: &RepositoryName, branch: &str) -> Result<Based, String> {
    let asked = format!("gh pr list of {repository} based on {branch}");
    let mut gh = Command::new("gh");
    gh.args([
        "pr", "list", "--base", branch, "--state", "all", "--limit", "100",
    ]);
    gh.args(["--json", "number,state"])
        .current_dir(dir)
        .env("GH_REPO", repository.to_string());
    git::local(&mut gh);
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("{asked}: {why}"))?;
    let listed: Vec<Numbered> = serde_json::from_slice(&said)
        .map_err(|why| format!("{asked} said what does not read: {why}"))?;
    let open = listed
        .iter()
        .find(|pull| matches!(pull.state, Stands::Open));
    let merged = listed
        .iter()
        .find(|pull| matches!(pull.state, Stands::Merged));
    Ok(match (open, merged) {
        (Some(open), _) => Based::Open(open.number),
        (None, Some(merged)) => Based::Merged(merged.number),
        (None, None) => Based::Neither,
    })
}

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

#[derive(Default)]
struct Forks(Mutex<HashMap<Directory, Arc<Mutex<Option<Lineage>>>>>);

impl Forks {
    fn of(&self, repository: &Directory) -> Arc<Mutex<Option<Lineage>>> {
        Arc::clone(locked(&self.0).entry(repository.clone()).or_default())
    }

    fn retain(&self, keep: impl Fn(&Directory) -> bool) {
        locked(&self.0).retain(|repository, _| keep(repository));
    }
}

fn unpoisoned(lineage: &Mutex<Option<Lineage>>) -> MutexGuard<'_, Option<Lineage>> {
    lineage.lock().unwrap_or_else(|poisoned| {
        let mut unknown = poisoned.into_inner();
        *unknown = None;
        lineage.clear_poison();
        unknown
    })
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug)]
pub struct Rests {
    pub pending: Duration,
    pub settled: Duration,
}

#[derive(PartialEq, Eq)]
struct Reported {
    pull_request: PullRequest,
    mergeability_unknown: bool,
}

impl Reported {
    fn is_pending(&self) -> bool {
        self.pull_request.verdict() == Some(Verdict::Pending) || self.mergeability_unknown
    }
}

struct Said {
    at: Instant,
    found: Option<Reported>,
}

struct Asked {
    next: Instant,
    rest: Duration,
    said: Option<Said>,
    failed: u32,
    trouble: Option<String>,
}

impl Asked {
    fn found(&self) -> Option<PullRequest> {
        let found = self.said.as_ref()?.found.as_ref()?.pull_request.clone();
        Some(PullRequest {
            stale: self.failed > 0,
            ..found
        })
    }
}

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

    fn next(&self) -> Option<Instant> {
        match self {
            Ask::Resting(asked) => Some(asked.next),
            Ask::Asking(_) => None,
        }
    }
}

struct Answered {
    branch: Branch,
    said: Result<Answer, String>,
}

fn answered(was: Option<Asked>, said: Result<Answer, String>, rests: Rests) -> Asked {
    assert!(
        rests.pending <= rests.settled,
        "pending rest cannot exceed settled rest"
    );
    let now = Instant::now();
    let (before, failed, rested) = match was {
        Some(was) => (was.said, was.failed, Some(was.rest)),
        None => (None, 0, None),
    };
    let of = |found: Option<Pulled>| {
        found.map(|found| {
            if found.mergeability_unknown {
                assert!(!found.pull_request.conflicting);
                assert!(matches!(
                    found.pull_request.state,
                    State::Open | State::Draft
                ));
            }
            if matches!(found.pull_request.state, State::Merged | State::Closed) {
                assert!(!found.pull_request.conflicting);
            }
            Reported {
                pull_request: found.pull_request,
                mergeability_unknown: found.mergeability_unknown,
            }
        })
    };
    let fresh = |found: Option<Reported>, trouble| {
        let now_pull = found.as_ref().map(|it| &it.pull_request);
        let change = |was| checks_changed(was, now_pull, failed, now);
        if let Some(change) = before.as_ref().and_then(change) {
            log::say(&change);
        }
        let new = before.as_ref().is_none_or(|was| was.found != found);
        (Some(Said { at: now, found }), trouble, 0, new)
    };
    let (said, trouble, failed, new) = match said {
        Ok(Answer::Stayed(found)) => fresh(of(found), None),
        Ok(Answer::Moved) => (before, None, failed, false),
        Ok(Answer::Partly { own, why }) => {
            let own = of(own);
            let was = before
                .as_ref()
                .and_then(|was| was.found.as_ref().map(|it| it.pull_request.clone()));
            let own_pull = own.as_ref().map(|it| it.pull_request.clone());
            if standing(was, own_pull.clone()) == own_pull {
                fresh(own, Some(why))
            } else {
                (before, Some(why), failed.saturating_add(1), false)
            }
        }
        Err(why) => (before, Some(why), failed.saturating_add(1), false),
    };
    let pending = said
        .as_ref()
        .and_then(|said| said.found.as_ref())
        .is_some_and(Reported::is_pending);
    let rest = match rested {
        _ if !pending => rests.settled,
        Some(rested) if !new => rested
            .saturating_mul(2)
            .max(rests.pending)
            .min(rests.settled),
        _ => rests.pending,
    };
    assert!(rest <= rests.settled);
    Asked {
        next: now + rest,
        rest,
        said,
        failed,
        trouble,
    }
}

fn checks_changed(
    was: &Said,
    now: Option<&PullRequest>,
    failed: u32,
    at: Instant,
) -> Option<String> {
    let (said, was, now) = (was.at, was.found.as_ref().map(|it| &it.pull_request)?, now?);
    if was.page != now.page {
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
    let named = now.page.url();
    let said = at.duration_since(said).as_secs();
    let failed = match failed {
        0 => String::new(),
        1 => ", failing once in between".to_owned(),
        failed => format!(", failing {failed} times in between"),
    };
    let (before, after) = (word(before), word(after));
    let said = format!(", {said}s after gh last said them");
    Some(format!(
        "the checks of {named} went from {before} to {after}{said}{failed}"
    ))
}

pub fn follow(
    core: SyncSender<core::Command>,
    wake: SyncSender<()>,
    woken: Receiver<()>,
    rests: Rests,
) -> io::Result<()> {
    thread::Builder::new()
        .name("gh".to_owned())
        .spawn(move || {
            let mut asked: HashMap<Branch, Ask> = HashMap::new();
            let forks = Arc::new(Forks::default());
            let (answer, answers) = mpsc::channel();
            let mut troubles = Troubles::default();
            let mut seen = None;
            let mut on = BTreeMap::new();
            loop {
                let Some(branches) = core::ask(&core, core::Command::Branches) else {
                    return;
                };
                while let Ok(Answered { branch, said }) = answers.try_recv() {
                    let was = asked.remove(&branch).and_then(Ask::into_asked);
                    asked.insert(branch, Ask::Resting(answered(was, said, rests)));
                }
                let now = Instant::now();
                let due = |asked: &HashMap<Branch, Ask>, branch: &Branch| {
                    asked
                        .get(branch)
                        .is_none_or(|ask| ask.next().is_some_and(|next| next <= now))
                };
                if seen.as_ref() != Some(&branches) || on.keys().any(|branch| due(&asked, branch)) {
                    on = located(&branches);
                    seen = Some(branches);
                }
                let asking = |ask: &Ask| matches!(ask, Ask::Asking(_));
                asked.retain(|branch, ask| on.contains_key(branch) || asking(ask));
                forks.retain(|repository| on.keys().any(|(of, _)| of == repository));
                if !tell(&core, &on, &asked) {
                    return;
                }
                let mut due: Vec<&Branch> =
                    on.keys().filter(|branch| due(&asked, branch)).collect();
                due.sort_by_key(|branch| asked.get(*branch).and_then(Ask::next));
                let free = |asked: &HashMap<Branch, Ask>| {
                    ASKS_AT_ONCE.saturating_sub(asked.values().filter(|ask| asking(ask)).count())
                };
                for branch in due.into_iter().take(free(&asked)) {
                    let dir = on[branch][0].clone();
                    let (of, forks) = (branch.clone(), Arc::clone(&forks));
                    let (answer, wake) = (answer.clone(), wake.clone());
                    let asks = thread::Builder::new()
                        .name("gh ask".to_owned())
                        .spawn(move || {
                            let said = panics::contained(|| view(&dir, &of, &forks))
                                .unwrap_or_else(|Panicked| {
                                    Err(format!("gh's ask about {} panicked", of.1))
                                });
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
                        }
                    };
                    asked.insert(branch.clone(), ask);
                }
                for trouble in asked
                    .values()
                    .filter_map(|ask| ask.asked()?.trouble.clone())
                {
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
                    Ok(()) | Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        })?;
    Ok(())
}

fn located(branches: &BTreeMap<String, String>) -> BTreeMap<Branch, Vec<String>> {
    let mut on: BTreeMap<Branch, Vec<String>> = BTreeMap::new();
    for (dir, branch) in branches {
        if let git::Common::Of(repository) = git::common(dir) {
            on.entry((repository, branch.clone()))
                .or_default()
                .push(dir.clone());
        }
    }
    on
}

fn tell(
    core: &SyncSender<core::Command>,
    on: &BTreeMap<Branch, Vec<String>>,
    asked: &HashMap<Branch, Ask>,
) -> bool {
    let mut found = BTreeMap::new();
    for (branch, dirs) in on {
        let Some(pull_request) = asked
            .get(branch)
            .and_then(Ask::asked)
            .and_then(Asked::found)
        else {
            continue;
        };
        let of = Found {
            branch: branch.1.clone(),
            pull_request,
        };
        found.extend(dirs.iter().map(|dir| (dir.clone(), of.clone())));
    }
    core.send(core::Command::PullRequests(found)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::panic;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use lune_protocol::github::{Checks, PullPage, PullRequest, State};

    use super::{
        Answer, Asked, Lineage, Pulled, Reported, RepositoryName, Rests, Said, answered,
        checks_changed, fork_of, parse, standing, unpoisoned,
    };

    #[test]
    fn a_lineage_whose_ask_panicked_halfway_is_known_no_more() {
        let lineage = Mutex::new(None);
        let torn = panic::catch_unwind(|| {
            let mut known = unpoisoned(&lineage);
            *known = Some(Lineage {
                remotes: "origin".to_owned(),
                fork: None,
            });
            panic!("gh's answer about the fork does not read");
        });
        assert!(torn.is_err(), "the ask panicked");
        assert!(lineage.is_poisoned(), "and left its lineage poisoned");
        assert!(
            unpoisoned(&lineage).is_none(),
            "what it left is not read as known"
        );
        assert!(!lineage.is_poisoned(), "and the next ask finds it whole");
    }

    fn pull(repository: &str, number: u32, state: State, checks: Option<Checks>) -> PullRequest {
        let page = PullPage::parse(&format!("https://github.com/{repository}/pull/{number}"));
        PullRequest {
            page: page.unwrap(),
            state,
            checks,
            stale: false,
            conflicting: false,
        }
    }

    fn pulled(pull_request: PullRequest) -> Pulled {
        Pulled {
            pull_request,
            base_name: "main".to_owned(),
            head_name: "fix".to_owned(),
            head: "abc".to_owned(),
            author: None,
            head_repository: None,
            mergeability_unknown: false,
        }
    }

    #[test]
    fn what_gh_failed_to_say_again_stands_as_stale() {
        let rests = Rests {
            pending: Duration::from_secs(1),
            settled: Duration::from_hours(1),
        };
        let pending = Some(Checks {
            pending: 1,
            failed: 0,
            passed: 0,
        });
        let parent_s = pull("upstream/repo", 5, State::Open, pending);
        let at = Instant::now();
        let was = |found: &PullRequest| Asked {
            next: at,
            rest: rests.pending,
            said: Some(Said {
                at,
                found: Some(Reported {
                    pull_request: found.clone(),
                    mergeability_unknown: false,
                }),
            }),
            failed: 1,
            trouble: None,
        };
        let partly = |own| {
            Ok(Answer::Partly {
                own,
                why: "the parent did not say".to_owned(),
            })
        };
        let said = |asked: &Asked| asked.said.as_ref().map(|said| said.at);

        let asked = answered(Some(was(&parent_s)), partly(None), rests);
        let stale = PullRequest {
            stale: true,
            ..parent_s.clone()
        };
        assert_eq!(
            (asked.found(), said(&asked), asked.failed),
            (Some(stale), Some(at), 2)
        );
        assert_eq!(asked.trouble.as_deref(), Some("the parent did not say"));

        let closed = pull("owner/repo", 4, State::Closed, None);
        let fork_s = pull("owner/repo", 4, State::Open, pending);
        let asked = answered(
            Some(was(&fork_s)),
            partly(Some(pulled(closed.clone()))),
            rests,
        );
        assert_eq!((asked.found(), asked.failed), (Some(closed), 0));
        assert!(said(&asked).is_some_and(|now| now > at));
        assert_eq!(asked.rest, rests.settled);

        let asked = answered(Some(was(&fork_s)), Ok(Answer::Moved), rests);
        let stale = PullRequest {
            stale: true,
            ..fork_s
        };
        assert_eq!(
            (asked.found(), asked.trouble, asked.failed),
            (Some(stale), None, 1)
        );
    }

    #[test]
    fn checks_that_stay_pending_are_asked_about_less_and_less_often() {
        let rests = Rests {
            pending: Duration::from_secs(15),
            settled: Duration::from_mins(2),
        };
        let checks = |pending, passed| {
            Some(Checks {
                pending,
                failed: 0,
                passed,
            })
        };
        let stayed = |checks| {
            let pull_request = pull("owner/repo", 7, State::Open, checks);
            Ok(Answer::Stayed(Some(pulled(pull_request))))
        };
        let failed = || Err("gh failed".to_owned());
        let mut asked = answered(None, stayed(checks(2, 0)), rests);
        let mut rested = vec![asked.rest];
        let said = [
            stayed(checks(2, 0)),
            failed(),
            stayed(checks(2, 0)),
            stayed(checks(2, 0)),
            stayed(checks(1, 1)),
            failed(),
            stayed(checks(0, 2)),
        ];
        for said in said {
            asked = answered(Some(asked), said, rests);
            rested.push(asked.rest);
        }
        let secs = Duration::from_secs;
        let rests = [15, 30, 60, 120, 120, 15, 30, 120].map(secs);
        assert_eq!(rested, rests);
    }

    #[test]
    fn a_change_in_the_checks_says_how_long_gh_took_to_see_it() {
        let checks = |pending, failed, passed| {
            Some(Checks {
                pending,
                failed,
                passed,
            })
        };
        let open = |checks| pull("owner/repo", 7, State::Open, checks);
        let at = Instant::now();
        let said = |found: Option<PullRequest>| Said {
            at: at.checked_sub(Duration::from_secs(62)).unwrap(),
            found: found.map(|pull_request| Reported {
                pull_request,
                mergeability_unknown: false,
            }),
        };
        let changed = |was: &PullRequest, now: &PullRequest, failed| {
            checks_changed(&said(Some(was.clone())), Some(now), failed, at)
        };
        let of = "the checks of https://github.com/owner/repo/pull/7 went from";
        assert_eq!(
            changed(&open(checks(1, 0, 2)), &open(checks(0, 0, 3)), 0),
            Some(format!(
                "{of} pending to passing, 62s after gh last said them"
            ))
        );
        assert_eq!(
            changed(&open(checks(0, 0, 0)), &open(checks(0, 1, 0)), 3),
            Some(format!(
                "{of} no checks to failing, 62s after gh last said them, failing 3 times in between"
            ))
        );
        assert_eq!(
            changed(&open(checks(2, 0, 1)), &open(checks(1, 0, 2)), 0),
            None
        );
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
        assert_eq!(
            standing(Some(fork_s(State::Open)), Some(closed())),
            Some(closed())
        );
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
        assert_eq!(
            parent_of(&forked("https://github.com/lorenzolfm/cdk")),
            on("github.com")
        );
        assert_eq!(
            parent_of(&forked("https://git.example.com/lorenzolfm/cdk")),
            on("git.example.com")
        );
        let default_branch = read(&forked("https://github.com/lorenzolfm/cdk"))
            .map(|fork| fork.map(|fork| fork.default_branch));
        assert_eq!(default_branch, Ok(Some("trunk".to_owned())));
        let none =
            format!(r#"{{{default},"parent":null,"url":"https://github.com/lorenzolfm/lune"}}"#);
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

    #[test]
    fn a_pull_request_says_whose_head_it_is() {
        let viewed = |author: &str, repository: &str, owner: &str| {
            let said = format!(
                r#"{{"isDraft":false,"state":"MERGED","baseRefName":"main","headRefName":"fix",
                "headRefOid":"abc","author":{author},"headRepository":{repository},
                "headRepositoryOwner":{owner},"url":"https://git.example.com/up/repo/pull/1"}}"#
            );
            parse(said.as_bytes()).unwrap()
        };
        let me = r#"{"id":"U","is_bot":false,"login":"me","name":"Me"}"#;
        let repository = r#"{"id":"R","name":"repo"}"#;
        let Pulled {
            head_name,
            head,
            author,
            head_repository,
            ..
        } = viewed(me, repository, me);
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

    #[test]
    fn a_pull_request_reads_as_gh_writes_it() {
        let read = |said: &str| parse(said.as_bytes()).map(|pulled| pulled.pull_request);
        let state = |said: &str| read(said).map(|pull_request| pull_request.state);
        let url = "https://github.com/owner/repo/pull/126";
        let head = r#""baseRefName":"main","headRefName":"fold","headRefOid":"abc""#;
        let open = format!(r#""isDraft":false,"state":"OPEN",{head}"#);
        assert_eq!(
            read(&format!(r#"{{{open},"url":"{url}"}}"#)),
            Ok(PullRequest {
                page: PullPage::parse(url).unwrap(),
                state: State::Open,
                checks: Some(Checks::default()),
                stale: false,
                conflicting: false,
            })
        );
        let page = format!(r#"{head},"url":"https://github.com/owner/repo/pull/1""#);
        let viewed = |fields: &str| format!("{{{fields},{page}}}");
        let draft = viewed(r#""isDraft":true,"state":"OPEN""#);
        assert_eq!(state(&draft), Ok(State::Draft));
        let merged = viewed(r#""isDraft":true,"state":"MERGED""#);
        assert_eq!(state(&merged), Ok(State::Merged));
        let closed = viewed(r#""isDraft":true,"state":"CLOSED""#);
        assert_eq!(state(&closed), Ok(State::Closed));

        let mergeable = |fields: &str| {
            let parsed = parse(viewed(fields).as_bytes()).unwrap();
            (
                parsed.pull_request.has_conflicts(),
                parsed.mergeability_unknown,
            )
        };
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"OPEN","mergeable":"CONFLICTING""#),
            (true, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":true,"state":"OPEN","mergeable":"CONFLICTING""#),
            (true, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"OPEN","mergeable":"MERGEABLE""#),
            (false, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"OPEN","mergeable":"UNKNOWN""#),
            (false, true)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"OPEN","mergeable":null"#),
            (false, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"MERGED","mergeable":"CONFLICTING""#),
            (false, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"MERGED","mergeable":"UNKNOWN""#),
            (false, false)
        );
        assert_eq!(
            mergeable(r#""isDraft":false,"state":"CLOSED","mergeable":"CONFLICTING""#),
            (false, false)
        );
        let unread = [
            viewed(r#""isDraft":false,"state":"QUEUED""#),
            viewed(r#""isDraft":false,"state":"OPEN","mergeable":"DIRTY""#),
            viewed(r#""isDraft":false,"state":"OPEN","mergeable":"conflicting""#),
            viewed(r#""state":"OPEN""#),
            format!(r#"{{"isDraft":false,"state":"OPEN",{head}}}"#),
            format!(r#"{{"isDraft":false,"state":"OPEN",{head},"url":"nowhere"}}"#),
            format!(r#"{{{open},"url":"https://github.com/owner/repo/issues/1"}}"#),
            format!(r#"{{{open},"url":"https://github.com/owner/repo/pull/0"}}"#),
            r#"{"isDraft":false,"state":"OPEN","url":"https://x/1"}"#.to_owned(),
            String::new(),
        ];
        for said in unread {
            assert!(read(&said).is_err(), "{said:?}");
        }
    }

    #[test]
    fn unknown_mergeability_is_asked_about_again_soon_and_backs_off() {
        let rests = Rests {
            pending: Duration::from_secs(15),
            settled: Duration::from_mins(2),
        };
        let stayed = |conflicting, mergeability_unknown| {
            let pull_request = PullRequest {
                conflicting,
                ..pull(
                    "owner/repo",
                    7,
                    State::Open,
                    Some(Checks {
                        pending: 0,
                        failed: 0,
                        passed: 1,
                    }),
                )
            };
            Ok(Answer::Stayed(Some(Pulled {
                mergeability_unknown,
                ..pulled(pull_request)
            })))
        };
        let mut asked = answered(None, stayed(false, false), rests);
        let mut rested = vec![asked.rest];
        for said in [
            stayed(false, true),
            stayed(false, true),
            stayed(false, true),
            stayed(true, false),
        ] {
            asked = answered(Some(asked), said, rests);
            rested.push(asked.rest);
        }
        let secs = Duration::from_secs;
        assert_eq!(rested, [120, 15, 30, 60, 120].map(secs));
        assert!(asked.found().is_some_and(|found| found.has_conflicts()));
    }
}
