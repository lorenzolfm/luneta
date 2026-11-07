//! The worktrees of a repository whose pull requests are merged, and deleting
//! them once you say so, for the menu's `d m` and for `luneta prune`. It runs
//! where it is asked, on its own machine, or for a luneta on another through
//! that machine's daemon, and nothing here deletes anything on its own.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use luneta_protocol::github::State;

use crate::api::{self, On, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse};
use crate::git::{self, Common};
use crate::github::{self, Asker, Pulled};
use crate::program;

/// How long a luneta waits to hear what would go: gh asks across the network,
/// a worktree at a time.
pub const PLANNING: Duration = Duration::from_mins(5);

/// How long a luneta waits to hear how deleting went: each worktree can hold
/// many gigabytes of build output.
pub const DELETING: Duration = Duration::from_mins(30);

const ASKS_AT_ONCE: usize = 8;

const GIT_SAYS: Duration = Duration::from_mins(1);

const REMOVING: Duration = Duration::from_mins(10);

const NOT_AT_HEAD: &str = "its HEAD is not its pull request's head";
const MAIN: &str = "it is the repository's main worktree";
const LOCKED: &str = "it is locked";
const CHANGED: &str = "it has modified or untracked files";
const A_PANE: &str = "it has a pane";
const AN_AGENT: &str = "an agent works in it";
const RUNS_HERE: &str = "this runs from it";
const DEFAULT: &str = "it is the default branch";

/// What deleting the merged worktrees of a repository deletes, and what stays.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub repository: String,
    pub goes: Vec<Goes>,
    pub kept: Vec<Kept>,
    pub unasked: Vec<Unasked>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goes {
    pub worktree: String,
    pub branch: String,
    pub head: String,
    pub number: u32,
    pub pushed: Pushed,
    pub bytes: Option<u64>,
}

/// The branch on the worktree's push remote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pushed {
    /// That remote has no such branch, or the worktree's branch has no such
    /// remote.
    Nowhere,
    Goes {
        remote: String,
        branch: String,
    },
    Stays(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub worktree: String,
    pub number: u32,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unasked {
    pub worktree: String,
    pub why: String,
}

/// What one of a plan's worktrees came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub worktree: String,
    pub gone: bool,
    pub trouble: Option<String>,
}

/// What a plan does, whichever way it is listed and whatever a worktree takes
/// up, which goes on changing: two plans that say the same are one.
pub type Deletes<'a> = Vec<(&'a str, &'a str, &'a str, u32, Option<(&'a str, &'a str)>)>;

impl Plan {
    #[must_use]
    pub fn deletes(&self) -> Deletes<'_> {
        let mut deletes: Deletes<'_> = self.goes.iter().map(Goes::does).collect();
        deletes.sort_unstable();
        deletes
    }

    #[must_use]
    pub fn same_as(&self, other: &Plan) -> bool {
        self.deletes() == other.deletes()
    }

    /// The name the repository goes by: its directory's.
    #[must_use]
    pub fn repository_name(&self) -> &str {
        name_of(&self.repository)
    }
}

impl Goes {
    fn does(&self) -> (&str, &str, &str, u32, Option<(&str, &str)>) {
        let pushed = match &self.pushed {
            Pushed::Goes { remote, branch } => Some((remote.as_str(), branch.as_str())),
            Pushed::Nowhere | Pushed::Stays(_) => None,
        };
        (&self.worktree, &self.branch, &self.head, self.number, pushed)
    }
}

/// The last part of a path, as a worktree or a repository goes by it.
#[must_use]
pub fn name_of(path: &str) -> &str {
    Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path)
}

/// Where the panes and the agents of a machine work, as its daemon says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupied {
    panes: Vec<PathBuf>,
    agents: Vec<PathBuf>,
}

impl Occupied {
    /// Where `host` says they work, or why that is not all of them: a session
    /// whose panes it has yet to list, or a pane it has yet to hear where it
    /// works of, could be in any worktree.
    pub fn of(host: &api::Host) -> Result<Occupied, String> {
        let mut panes = Vec::new();
        for session in &host.sessions {
            let name = &session.name;
            let listed = session
                .panes
                .as_ref()
                .ok_or_else(|| format!("session {name} has yet to list its panes"))?;
            for pane in &listed.panes {
                let id = pane.id;
                let cwd = pane
                    .cwd
                    .as_ref()
                    .ok_or_else(|| format!("pane {name}:{id} has yet to say where it works"))?;
                panes.push(PathBuf::from(cwd));
            }
        }
        let agents = host.agents.iter().map(|agent| PathBuf::from(&agent.cwd)).collect();
        Ok(Occupied { panes, agents })
    }

    fn why_in(&self, worktree: &Path) -> Option<&'static str> {
        let real = fs::canonicalize(worktree).ok();
        let holds = |dir: &PathBuf| {
            dir.starts_with(worktree) || real.as_ref().is_some_and(|real| dir.starts_with(real))
        };
        if self.panes.iter().any(holds) {
            return Some(A_PANE);
        }
        self.agents.iter().any(holds).then_some(AN_AGENT)
    }
}

/// A worktree as `git worktree list --porcelain -z` lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Listed {
    path: String,
    head: String,
    branch: Option<String>,
    /// The first it lists, which git will not remove.
    main: bool,
    locked: bool,
    prunable: bool,
}

#[derive(Clone, Copy)]
struct OnBranch<'a> {
    listed: &'a Listed,
    branch: &'a str,
}

impl<'a> OnBranch<'a> {
    fn of(listed: &'a Listed) -> Option<Self> {
        Some(OnBranch { listed, branch: listed.branch.as_deref()? })
    }
}

fn parse_listed(said: &[u8]) -> Vec<Listed> {
    let said = String::from_utf8_lossy(said);
    let mut listed: Vec<Listed> = Vec::new();
    for line in said.split('\0') {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        if word == "worktree" {
            let main = listed.is_empty();
            listed.push(Listed { path: rest.to_owned(), main, ..Listed::default() });
            continue;
        }
        let Some(one) = listed.last_mut() else {
            continue;
        };
        match word {
            "HEAD" => rest.clone_into(&mut one.head),
            "branch" => one.branch = rest.strip_prefix("refs/heads/").map(str::to_owned),
            "locked" => one.locked = true,
            "prunable" => one.prunable = true,
            _ => {},
        }
    }
    listed
}

/// git on the repository whose common directory is `common`, wherever it runs.
fn on_repository(common: &str) -> Command {
    let mut git = Command::new("git");
    git::local(&mut git).args(["-c", "safe.bareRepository=explicit", "-c", "core.fsmonitor=false"]);
    git.arg(format!("--git-dir={common}"));
    git
}

fn says(mut git: Command, limit: Duration, asked: &str) -> Result<Vec<u8>, String> {
    let ran = program::run(&mut git, limit)
        .map_err(|error| format!("git {asked} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("git {asked}: {why}"))
}

fn says_alone(mut git: Command, asked: &str) -> Result<Vec<u8>, String> {
    let ran = program::run_alone(&mut git, GIT_SAYS)
        .map_err(|error| format!("git {asked} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("git {asked}: {why}"))
}

fn worktrees(common: &str) -> Result<Vec<Listed>, String> {
    let mut git = on_repository(common);
    git.args(["worktree", "list", "--porcelain", "-z"]);
    says(git, GIT_SAYS, "worktree list").map(|said| parse_listed(&said))
}

fn clean(worktree: &str) -> Result<bool, String> {
    let mut git = Command::new("git");
    git::local(&mut git).args(["-c", "safe.bareRepository=explicit", "-c", "core.fsmonitor=false"]);
    git.args(["-C", worktree, "--no-optional-locks", "status", "--porcelain"]);
    says(git, GIT_SAYS, "status").map(|said| said.is_empty())
}

fn common_of(dir: &str) -> Result<String, String> {
    match git::common(dir) {
        Common::Of(common) => Ok(common),
        Common::NoRepository => Err(format!("{dir} is in no repository")),
        Common::Unsaid(why) => Err(format!("git did not say what repository {dir} is in: {why}")),
    }
}

/// Why `listed` stays whatever gh says of it, as things stand now: asked again
/// of each worktree just before it goes.
fn stays(
    listed: &Listed,
    head: &str,
    occupied: &Occupied,
    here: Option<&Path>,
) -> Result<Option<&'static str>, String> {
    if listed.head != head {
        return Ok(Some(NOT_AT_HEAD));
    }
    if listed.main {
        return Ok(Some(MAIN));
    }
    if listed.locked {
        return Ok(Some(LOCKED));
    }
    if !clean(&listed.path)? {
        return Ok(Some(CHANGED));
    }
    let worktree = Path::new(&listed.path);
    if let Some(why) = occupied.why_in(worktree) {
        return Ok(Some(why));
    }
    let runs_here = here.is_some_and(|here| {
        here.starts_with(worktree)
            || fs::canonicalize(worktree).is_ok_and(|real| here.starts_with(real))
    });
    Ok(runs_here.then_some(RUNS_HERE))
}

/// Has `ask` answer of each of `items`, a few at a time, as each asks across
/// the network, with what `state` makes for each thread that asks.
fn at_once<T: Sync, S, R: Send>(
    items: &[T],
    state: impl Fn() -> S + Sync,
    ask: impl Fn(&mut S, &T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let answers = Mutex::new(Vec::with_capacity(items.len()));
    thread::scope(|scope| {
        for _ in 0..ASKS_AT_ONCE.min(items.len()) {
            scope.spawn(|| {
                let mut state = state();
                loop {
                    let at = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(at) else {
                        return;
                    };
                    let answer = ask(&mut state, item);
                    if let Ok(mut answers) = answers.lock() {
                        answers.push((at, answer));
                    }
                }
            });
        }
    });
    let mut answers = answers.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    answers.sort_by_key(|(at, _)| *at);
    answers.into_iter().map(|(_, answer)| answer).collect()
}

/// Where a push of `branch` goes, as the remote and its name there.
fn push_target(common: &str, branch: &str) -> Result<Option<(String, String)>, String> {
    let mut git = on_repository(common);
    let head = format!("refs/heads/{branch}");
    git.args(["for-each-ref", "--format=%(push:remotename)%00%(push)", &head]);
    let said = says(git, GIT_SAYS, "for-each-ref")?;
    let said = String::from_utf8_lossy(&said);
    let Some((remote, tracking)) = said.trim_end().split_once('\0') else {
        return Ok(None);
    };
    let Some(there) = tracking.strip_prefix(&format!("refs/remotes/{remote}/")) else {
        return Ok(None);
    };
    Ok(Some((remote.to_owned(), there.to_owned())))
}

fn remote_default(common: &str, remote: &str) -> Option<String> {
    let mut git = on_repository(common);
    git.args(["symbolic-ref", "--quiet", &format!("refs/remotes/{remote}/HEAD")]);
    let said = says(git, GIT_SAYS, "symbolic-ref").ok()?;
    let said = String::from_utf8_lossy(&said);
    said.trim_end().strip_prefix(&format!("refs/remotes/{remote}/")).map(str::to_owned)
}

/// The branches of `remote`, each by its name there, at their commits. It asks
/// with no terminal, as ssh may want a passphrase there.
fn remote_heads(common: &str, remote: &str) -> Result<HashMap<String, String>, String> {
    let mut git = on_repository(common);
    git.args(["ls-remote", "--heads", "--", remote]);
    let said = says_alone(git, "ls-remote")?;
    let said = String::from_utf8_lossy(&said);
    let heads = said.lines().filter_map(|line| {
        let (head, name) = line.split_once('\t')?;
        Some((name.strip_prefix("refs/heads/")?.to_owned(), head.to_owned()))
    });
    Ok(heads.collect())
}

/// What goes of a worktree's branch on its push remote.
struct Remotes<'a> {
    common: &'a str,
    heads: HashMap<String, Result<HashMap<String, String>, String>>,
    logins: HashMap<String, Result<String, String>>,
}

impl Remotes<'_> {
    fn pushed(&mut self, target: Option<&(String, String)>, pulled: &Pulled) -> Pushed {
        let Some((remote, branch)) = target else {
            return Pushed::Nowhere;
        };
        let common = self.common;
        let heads =
            self.heads.entry(remote.clone()).or_insert_with(|| remote_heads(common, remote));
        let there = match heads {
            Ok(heads) => heads.get(branch),
            Err(why) => return Pushed::Stays(format!("git did not reach {remote}: {why}")),
        };
        let Some(there) = there else {
            return Pushed::Nowhere;
        };
        if *there != pulled.head {
            return Pushed::Stays(format!("{remote} has it at another commit"));
        }
        if *branch != pulled.head_name {
            return Pushed::Stays("its pull request's head is another branch".to_owned());
        }
        let Some(host) = pulled.host.as_deref() else {
            return Pushed::Stays("its pull request's page names no host".to_owned());
        };
        let login = self.logins.entry(host.to_owned()).or_insert_with(|| github::logged_in(host));
        match login {
            Ok(login) if pulled.author.as_deref() == Some(login.as_str()) => {
                Pushed::Goes { remote: remote.clone(), branch: branch.clone() }
            },
            Ok(_) => Pushed::Stays("someone else wrote its pull request".to_owned()),
            Err(why) => Pushed::Stays(format!("gh did not say who you are: {why}")),
        }
    }
}

fn bytes_of(worktree: &str) -> Option<u64> {
    let mut du = Command::new("du");
    du.args(["-sk", "--", worktree]);
    let said = program::run(&mut du, GIT_SAYS).ok()?.answer().ok()?;
    let said = String::from_utf8_lossy(&said);
    let kibibytes: u64 = said.split_whitespace().next()?.parse().ok()?;
    kibibytes.checked_mul(1024)
}

/// What deleting the merged worktrees of the repository `dir` is in deletes,
/// where the panes and agents of this machine work as `occupied` says, and this
/// runs in `here`. Or why it cannot tell: nothing is deleted then.
pub fn plan(dir: &str, occupied: &Occupied, here: Option<&Path>) -> Result<Plan, String> {
    let common = common_of(dir)?;
    let repository = git::holder(&common)
        .ok_or_else(|| format!("no directory holds the repository {common}"))?;
    let listed = worktrees(&common)?;
    let askable: Vec<OnBranch<'_>> =
        listed.iter().filter(|it| !it.prunable).filter_map(OnBranch::of).collect();
    let answers = at_once(&askable, Asker::default, |asker, on: &OnBranch<'_>| {
        asker.pull_of(&on.listed.path, &common, on.branch)
    });
    let mut plan = Plan { repository, goes: Vec::new(), kept: Vec::new(), unasked: Vec::new() };
    let mut merged: Vec<(OnBranch<'_>, Pulled)> = Vec::new();
    for (on, answer) in askable.into_iter().zip(answers) {
        match answer {
            Ok(Some(pulled)) if pulled.pull_request.state == State::Merged => {
                merged.push((on, pulled));
            },
            Ok(_) => {},
            Err(why) => plan.unasked.push(Unasked { worktree: on.listed.path.clone(), why }),
        }
    }
    let going = sorted_out(&mut plan, merged, occupied, here, &common)?;
    let based = at_once(
        &going,
        || (),
        |(), (on, pulled, _): &Going<'_>| match &pulled.head_repository {
            Some(repository) => {
                github::open_based_on(&on.listed.path, repository, &pulled.head_name)
            },
            None => Ok(None),
        },
    );
    let mut remotes = Remotes { common: &common, heads: HashMap::new(), logins: HashMap::new() };
    let mut goes = Vec::new();
    for ((on, pulled, target), based) in going.into_iter().zip(based) {
        let number = pulled.pull_request.number.get();
        match based {
            Ok(None) => {
                let pushed = remotes.pushed(target.as_ref(), &pulled);
                goes.push(Goes {
                    worktree: on.listed.path.clone(),
                    branch: on.branch.to_owned(),
                    head: pulled.head,
                    number,
                    pushed,
                    bytes: None,
                });
            },
            Ok(Some(open)) => plan.kept.push(Kept {
                worktree: on.listed.path.clone(),
                number,
                why: format!("#{open} is based on it"),
            }),
            Err(why) => plan.unasked.push(Unasked { worktree: on.listed.path.clone(), why }),
        }
    }
    let bytes = at_once(&goes, || (), |(), goes: &Goes| bytes_of(&goes.worktree));
    for (goes, bytes) in goes.iter_mut().zip(bytes) {
        goes.bytes = bytes;
    }
    goes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.worktree.cmp(&b.worktree)));
    plan.goes = goes;
    Ok(plan)
}

type Going<'a> = (OnBranch<'a>, Pulled, Option<(String, String)>);

/// What stays of `merged` as things stand here, into `plan`, and what goes on
/// to what the network says.
fn sorted_out<'a>(
    plan: &mut Plan,
    merged: Vec<(OnBranch<'a>, Pulled)>,
    occupied: &Occupied,
    here: Option<&Path>,
    common: &str,
) -> Result<Vec<Going<'a>>, String> {
    let mut default = None;
    let mut going = Vec::new();
    for (on, pulled) in merged {
        let (worktree, number) = (on.listed.path.clone(), pulled.pull_request.number.get());
        let why = match stays(on.listed, &pulled.head, occupied, here) {
            Ok(why) => why,
            Err(why) => {
                plan.unasked.push(Unasked { worktree, why });
                continue;
            },
        };
        if let Some(why) = why {
            plan.kept.push(Kept { worktree, number, why: why.to_owned() });
            continue;
        }
        let default = match &default {
            Some(default) => default,
            None => default.insert(github::default_branch(&on.listed.path)?),
        };
        let target = push_target(common, on.branch)?;
        let (remote, there) =
            target.as_ref().map(|(remote, there)| (remote.as_str(), there.as_str())).unzip();
        let far = remote.and_then(|remote| remote_default(common, remote));
        let is_default = |name: &str| name == default.as_str() || far.as_deref() == Some(name);
        if is_default(on.branch) || there.is_some_and(is_default) {
            plan.kept.push(Kept { worktree, number, why: DEFAULT.to_owned() });
            continue;
        }
        going.push((on, pulled, target));
    }
    Ok(going)
}

/// Deletes each of `goes`, of the repository `dir` is in, where it stands as
/// it did for the plan, whichever way the others go. `occupied` says where the
/// panes and the agents work just before each.
pub fn deleted(
    dir: &str,
    goes: &[Goes],
    occupied: impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Result<Vec<Outcome>, String> {
    let common = common_of(dir)?;
    Ok(goes
        .iter()
        .map(|goes| {
            let (gone, trouble) = match removed(&common, goes, &occupied, here) {
                Ok(()) => match branch_deleted(&common, goes) {
                    Ok(()) => (true, None),
                    Err(why) => (true, Some(why)),
                },
                Err(why) => (false, Some(why)),
            };
            Outcome { worktree: goes.worktree.clone(), gone, trouble }
        })
        .collect())
}

fn removed(
    common: &str,
    goes: &Goes,
    occupied: &impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Result<(), String> {
    let occupied = occupied().map_err(|why| format!("luneta cannot tell its panes: {why}"))?;
    let listed = worktrees(common)?;
    let listed = listed
        .iter()
        .find(|listed| listed.path == goes.worktree)
        .ok_or_else(|| "git lists no such worktree now".to_owned())?;
    if listed.branch.as_deref() != Some(goes.branch.as_str()) {
        return Err(format!("it is no longer on {}", goes.branch));
    }
    if let Some(why) = stays(listed, &goes.head, &occupied, here)? {
        return Err(why.to_owned());
    }
    let mut git = on_repository(common);
    git.args(["worktree", "remove", "--", &goes.worktree]);
    says(git, REMOVING, "worktree remove").map(|_| ())
}

/// Deletes the branch of a worktree that went, here and on its push remote.
fn branch_deleted(common: &str, goes: &Goes) -> Result<(), String> {
    let mut at = on_repository(common);
    at.args(["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", goes.branch)]);
    let at = says(at, GIT_SAYS, "rev-parse")?;
    if String::from_utf8_lossy(&at).trim_end() != goes.head {
        return Err(format!("its branch {} stays: it has moved", goes.branch));
    }
    let mut git = on_repository(common);
    git.args(["branch", "-D", "--", &goes.branch]);
    says(git, GIT_SAYS, "branch -D")
        .map_err(|why| format!("its branch {} stays: {why}", goes.branch))?;
    let Pushed::Goes { remote, branch } = &goes.pushed else {
        return Ok(());
    };
    let there = format!("refs/heads/{branch}");
    let mut git = on_repository(common);
    git.arg("push").arg(format!("--force-with-lease={there}:{}", goes.head));
    git.args(["--", remote, &format!(":{there}")]);
    says_alone(git, "push")
        .map(|_| ())
        .map_err(|why| format!("its branch on {remote} stays: {why}"))
}

impl From<&Goes> for api::prunes_response::Goes {
    fn from(goes: &Goes) -> Self {
        use api::prunes_response::goes::Pushed as Said;
        let pushed = match &goes.pushed {
            Pushed::Nowhere => None,
            Pushed::Goes { remote, branch } => {
                Some(Said::Remote(api::prunes_response::RemoteBranch {
                    remote: remote.clone(),
                    branch: branch.clone(),
                }))
            },
            Pushed::Stays(why) => Some(Said::Stays(why.clone())),
        };
        api::prunes_response::Goes {
            worktree: goes.worktree.clone(),
            branch: goes.branch.clone(),
            head: goes.head.clone(),
            number: goes.number,
            pushed,
            bytes: goes.bytes,
        }
    }
}

impl From<api::prunes_response::Goes> for Goes {
    fn from(goes: api::prunes_response::Goes) -> Self {
        use api::prunes_response::goes::Pushed as Said;
        let pushed = match goes.pushed {
            None => Pushed::Nowhere,
            Some(Said::Remote(api::prunes_response::RemoteBranch { remote, branch })) => {
                Pushed::Goes { remote, branch }
            },
            Some(Said::Stays(why)) => Pushed::Stays(why),
        };
        Goes {
            worktree: goes.worktree,
            branch: goes.branch,
            head: goes.head,
            number: goes.number,
            pushed,
            bytes: goes.bytes,
        }
    }
}

impl From<&Plan> for PrunesResponse {
    fn from(plan: &Plan) -> Self {
        let kept = plan.kept.iter().map(|kept| api::prunes_response::Kept {
            worktree: kept.worktree.clone(),
            number: kept.number,
            why: kept.why.clone(),
        });
        let unasked = plan.unasked.iter().map(|unasked| api::prunes_response::Unasked {
            worktree: unasked.worktree.clone(),
            why: unasked.why.clone(),
        });
        PrunesResponse {
            repository: plan.repository.clone(),
            goes: plan.goes.iter().map(Into::into).collect(),
            kept: kept.collect(),
            unasked: unasked.collect(),
        }
    }
}

impl From<PrunesResponse> for Plan {
    fn from(said: PrunesResponse) -> Self {
        let kept = said.kept.into_iter().map(|kept| Kept {
            worktree: kept.worktree,
            number: kept.number,
            why: kept.why,
        });
        let unasked = said
            .unasked
            .into_iter()
            .map(|unasked| Unasked { worktree: unasked.worktree, why: unasked.why });
        Plan {
            repository: said.repository,
            goes: said.goes.into_iter().map(Into::into).collect(),
            kept: kept.collect(),
            unasked: unasked.collect(),
        }
    }
}

impl From<&Outcome> for api::prune_response::Outcome {
    fn from(outcome: &Outcome) -> Self {
        api::prune_response::Outcome {
            worktree: outcome.worktree.clone(),
            gone: outcome.gone,
            trouble: outcome.trouble.clone(),
        }
    }
}

#[must_use]
pub fn outcomes_of(said: PruneResponse) -> Vec<Outcome> {
    let outcomes = said.outcomes.into_iter().map(|outcome| Outcome {
        worktree: outcome.worktree,
        gone: outcome.gone,
        trouble: outcome.trouble,
    });
    outcomes.collect()
}

/// The question that has the daemon of the machine `on` names say what would
/// go in the repository `dir` is in.
#[must_use]
pub fn asked(on: Option<On>, dir: &str) -> PrunesRequest {
    PrunesRequest { on, dir: dir.to_owned() }
}

/// The question that has that daemon delete `goes`.
#[must_use]
pub fn told(on: Option<On>, dir: &str, goes: &[Goes]) -> PruneRequest {
    PruneRequest { on, dir: dir.to_owned(), goes: goes.iter().map(Into::into).collect() }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use luneta_protocol::github::PullRequest;

    use super::*;

    fn goes(worktree: &str, pushed: Pushed, bytes: Option<u64>) -> Goes {
        let (branch, head) = ("fix".to_owned(), "abc".to_owned());
        Goes { worktree: worktree.to_owned(), branch, head, number: 1, pushed, bytes }
    }

    fn plan_of(goes: Vec<Goes>) -> Plan {
        Plan { repository: "/w/luneta".to_owned(), goes, kept: Vec::new(), unasked: Vec::new() }
    }

    /// What a plan does does not go by what a worktree takes up, nor by the
    /// order of the list, which goes by that. A branch on a remote that goes
    /// or stays is another plan, but why it stays is not.
    #[test]
    fn two_plans_are_the_same_by_what_they_delete() {
        let (a, b) =
            (|bytes| goes("/w/a", Pushed::Nowhere, bytes), goes("/w/b", Pushed::Nowhere, None));
        let plan = plan_of(vec![a(Some(9)), b.clone()]);
        assert!(plan.same_as(&plan_of(vec![b.clone(), a(Some(1))])));
        assert!(!plan.same_as(&plan_of(vec![a(Some(9))])));
        let remote = || Pushed::Goes { remote: "origin".to_owned(), branch: "fix".to_owned() };
        let pushed = plan_of(vec![goes("/w/a", remote(), None)]);
        let stays = |why: &str| plan_of(vec![goes("/w/a", Pushed::Stays(why.to_owned()), None)]);
        assert!(!pushed.same_as(&stays("moved")));
        assert!(stays("moved").same_as(&stays("no agent")));
        assert!(stays("moved").same_as(&plan_of(vec![goes("/w/a", Pushed::Nowhere, None)])));
        let moved = Goes { head: "def".to_owned(), ..goes("/w/a", Pushed::Nowhere, None) };
        assert!(!plan_of(vec![moved]).same_as(&stays("moved")));
    }

    /// The first it lists is the main one, a bare repository's too, which has
    /// no branch. A path that holds a space or a line is whole.
    #[test]
    fn git_s_list_of_worktrees_reads_as_it_writes_it() {
        let said = "worktree /w/.bare\0bare\0\0\
                    worktree /w/a b\0HEAD abc\0branch refs/heads/fix/a\0\0\
                    worktree /w/c\nd\0HEAD def\0detached\0\0\
                    worktree /w/e\0HEAD 012\0branch refs/heads/e\0locked why not\0\0\
                    worktree /w/f\0HEAD 345\0branch refs/heads/f\0prunable gone\0\0";
        let listed = parse_listed(said.as_bytes());
        let of = |path: &str| Listed { path: path.to_owned(), ..Listed::default() };
        let on = |branch: &str| Some(branch.to_owned());
        assert_eq!(
            listed,
            [
                Listed { main: true, ..of("/w/.bare") },
                Listed { head: "abc".to_owned(), branch: on("fix/a"), ..of("/w/a b") },
                Listed { head: "def".to_owned(), ..of("/w/c\nd") },
                Listed { head: "012".to_owned(), branch: on("e"), locked: true, ..of("/w/e") },
                Listed { head: "345".to_owned(), branch: on("f"), prunable: true, ..of("/w/f") },
            ]
        );
    }

    fn pane(id: u32, cwd: Option<&str>) -> api::Pane {
        api::Pane { id, cwd: cwd.map(str::to_owned), ..api::Pane::default() }
    }

    fn session(name: &str, panes: Option<Vec<api::Pane>>) -> api::Session {
        let panes = panes.map(|panes| api::Panes { panes });
        api::Session { name: name.to_owned(), panes, ..api::Session::default() }
    }

    /// A pane in a directory of a worktree is in it, and one in a directory
    /// beside it, whose name starts alike, is not. An agent counts too.
    #[test]
    fn a_pane_or_an_agent_in_a_worktree_or_under_it_keeps_it() {
        let host = api::Host {
            sessions: vec![session(
                "work",
                Some(vec![pane(1, Some("/w/a/src")), pane(2, Some("/w/bb"))]),
            )],
            agents: vec![api::Agent { cwd: "/w/c".to_owned(), ..api::Agent::default() }],
            ..api::Host::default()
        };
        let occupied = Occupied::of(&host).unwrap();
        assert_eq!(occupied.why_in(Path::new("/w/a")), Some(A_PANE));
        assert_eq!(occupied.why_in(Path::new("/w/b")), None);
        assert_eq!(occupied.why_in(Path::new("/w/c")), Some(AN_AGENT));
    }

    /// While the daemon has yet to say where any pane works, that pane could
    /// be in any worktree.
    #[test]
    fn a_pane_the_daemon_has_yet_to_place_tells_nothing() {
        let host = |session| api::Host { sessions: vec![session], ..api::Host::default() };
        let unlisted = Occupied::of(&host(session("work", None)));
        assert_eq!(unlisted, Err("session work has yet to list its panes".to_owned()));
        let unsaid = Occupied::of(&host(session("work", Some(vec![pane(4, None)]))));
        assert_eq!(unsaid, Err("pane work:4 has yet to say where it works".to_owned()));
    }

    fn pulled(author: Option<&str>, head: &str) -> Pulled {
        let number = NonZeroU32::new(1).unwrap();
        Pulled {
            pull_request: PullRequest { number, state: State::Merged, url: None },
            head_name: "origin-branch".to_owned(),
            head: head.to_owned(),
            author: author.map(str::to_owned),
            host: Some("github.com".to_owned()),
            head_repository: None,
        }
    }

    /// The branch on a remote goes only where the remote has it at the pull
    /// request's head and you wrote the pull request. gh is asked who you are
    /// only then.
    #[test]
    fn the_branch_on_a_remote_goes_only_at_that_head_and_if_it_is_yours() {
        let ok = |remote: &str, head: &str| {
            Ok(HashMap::from([(format!("{remote}-branch"), head.to_owned())]))
        };
        let mut remotes = Remotes {
            common: "/nowhere",
            heads: HashMap::from([
                ("origin".to_owned(), ok("origin", "abc")),
                ("gone".to_owned(), Err("no key".to_owned())),
            ]),
            logins: HashMap::from([("github.com".to_owned(), Ok("me".to_owned()))]),
        };
        let target = |remote: &str| Some((remote.to_owned(), format!("{remote}-branch")));
        let goes = Pushed::Goes { remote: "origin".to_owned(), branch: "origin-branch".to_owned() };
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(Some("me"), "abc")), goes);
        assert_eq!(remotes.pushed(None, &pulled(Some("me"), "abc")), Pushed::Nowhere);
        let elsewhere = Some(("origin".to_owned(), "other".to_owned()));
        assert_eq!(remotes.pushed(elsewhere.as_ref(), &pulled(Some("me"), "abc")), Pushed::Nowhere);
        assert_eq!(
            remotes.pushed(target("origin").as_ref(), &pulled(Some("me"), "def")),
            Pushed::Stays("origin has it at another commit".to_owned())
        );
        let elsewhere = Pulled { head_name: "fix".to_owned(), ..pulled(Some("me"), "abc") };
        assert_eq!(
            remotes.pushed(target("origin").as_ref(), &elsewhere),
            Pushed::Stays("its pull request's head is another branch".to_owned())
        );
        let theirs = Pushed::Stays("someone else wrote its pull request".to_owned());
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(Some("them"), "abc")), theirs);
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(None, "abc")), theirs);
        assert_eq!(
            remotes.pushed(target("gone").as_ref(), &pulled(Some("me"), "abc")),
            Pushed::Stays("git did not reach gone: no key".to_owned())
        );
    }

    /// Every answer comes back beside what it is of, however few threads ask.
    #[test]
    fn answers_asked_at_once_keep_their_order() {
        let items: Vec<usize> = (0..50).collect();
        let doubled = at_once(&items, || (), |(), item: &usize| item * 2);
        assert_eq!(doubled, items.iter().map(|item| item * 2).collect::<Vec<_>>());
        assert_eq!(at_once(&[] as &[usize], || (), |(), item: &usize| *item), [] as [usize; 0]);
    }

    #[test]
    fn a_plan_crosses_the_wire_whole() {
        let goes = vec![
            goes("/w/a", Pushed::Nowhere, None),
            goes("/w/b", Pushed::Goes { remote: "o".to_owned(), branch: "x".to_owned() }, Some(7)),
            goes("/w/c", Pushed::Stays("moved".to_owned()), Some(0)),
        ];
        let kept = vec![Kept { worktree: "/w/d".to_owned(), number: 2, why: "pane".to_owned() }];
        let unasked = vec![Unasked { worktree: "/w/e".to_owned(), why: "gh".to_owned() }];
        let plan = Plan { repository: "/w".to_owned(), goes, kept, unasked };
        assert_eq!(Plan::from(PrunesResponse::from(&plan)), plan);
        let outcome =
            Outcome { worktree: "/w/a".to_owned(), gone: true, trouble: Some("x".to_owned()) };
        let said = PruneResponse { outcomes: vec![(&outcome).into()] };
        assert_eq!(outcomes_of(said), [outcome]);
    }
}
