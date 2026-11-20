use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use luneta_protocol::github::State;

use crate::api::projects::Held;
use crate::api::terminal::Found;
use crate::api::{self, On, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse};
use crate::directory::Directory;
use crate::git::{self, Common};
use crate::github::{self, Asker, Based, Pulled};
use crate::mux::TerminalKey;
use crate::program;
use crate::seats;

pub const PLANNING: Duration = Duration::from_mins(5);

pub const DELETING: Duration = Duration::from_mins(30);

const ASKS_AT_ONCE: usize = 8;

const GIT_SAYS: Duration = Duration::from_mins(1);

const REMOVING: Duration = Duration::from_mins(10);

const NOT_AT_HEAD: &str = "its HEAD is not its pull request's head";
const MAIN: &str = "it is the repository's main worktree";
const LOCKED: &str = "it is locked";
const HOLDS_ANOTHER: &str = "another worktree is in it";
const CHANGED: &str = "it has modified or untracked files";
const A_VIEW: &str = "it has a view";
const AN_AGENT: &str = "an agent works in it";
const RUNS_HERE: &str = "this runs from it";
const DEFAULT: &str = "it is the default branch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub repository: String,
    pub goes: Vec<Goes>,
    pub kept: Vec<Kept>,
    pub unasked: Vec<Unasked>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Goes {
    pub folder: String,
    pub branch: String,
    pub head: String,
    pub number: u32,
    pub pushed: Pushed,
    pub bytes: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pushed {
    Nowhere,
    Goes(RemoteBranch),
    Stays(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kept {
    pub folder: String,
    pub number: u32,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unasked {
    pub folder: String,
    pub why: String,
}

#[derive(Debug)]
pub struct Planned {
    plan: Plan,
    common: String,
}

impl Planned {
    #[must_use]
    pub fn plan(&self) -> &Plan {
        &self.plan
    }

    #[must_use]
    pub fn into_plan(self) -> Plan {
        self.plan
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub folder: String,
    pub came: Came,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Came {
    Gone,
    WentBut(String),
    Stays(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pruned {
    Changed(Plan),
    Went(Vec<Outcome>),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Deleting {
    pub folder: String,
    pub branch: String,
    pub head: String,
    pub number: u32,
    pub remote: Option<RemoteBranch>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
}

impl Plan {
    #[must_use]
    pub fn deletes(&self) -> BTreeSet<Deleting> {
        self.goes.iter().map(Goes::does).collect()
    }

    #[must_use]
    pub fn repository_name(&self) -> &str {
        name_of(&self.repository)
    }
}

impl Goes {
    fn does(&self) -> Deleting {
        let remote = match &self.pushed {
            Pushed::Goes(far) => Some(far.clone()),
            Pushed::Nowhere | Pushed::Stays(_) => None,
        };
        Deleting {
            folder: self.folder.clone(),
            branch: self.branch.clone(),
            head: self.head.clone(),
            number: self.number,
            remote,
        }
    }
}

#[must_use]
pub fn name_of(path: &str) -> &str {
    Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occupied {
    views: Vec<PathBuf>,
    agents: Vec<PathBuf>,
}

impl Occupied {
    pub fn of(host: &api::Host) -> Result<Occupied, String> {
        let unread = seats::unread(host.unread, &host.unread_files);
        let unread = unread.map_err(|unplaced| unplaced.to_string())?;
        let terminals = terminals(host)?;
        for seats::Unread { pid, terminal } in unread {
            match terminal {
                None => {
                    return Err(format!(
                        "lunetad cannot read the session file of agent {pid}, which runs in no \
                         terminal"
                    ));
                },
                Some(key) if !terminals.iter().any(|(it, _)| *it == Some(key)) => {
                    return Err(format!(
                        "lunetad cannot read the session file of agent {pid}, which runs in a \
                         terminal no layout holds"
                    ));
                },
                Some(_) => {},
            }
        }
        let views = terminals.into_iter().map(|(_, folder)| folder).collect();
        let agents = host.agents.iter().map(|agent| PathBuf::from(&agent.cwd)).collect();
        Ok(Occupied { views, agents })
    }

    fn why_in(&self, folder: &Path) -> Option<&'static str> {
        let real = fs::canonicalize(folder).ok();
        let holds = |dir: &PathBuf| {
            dir.starts_with(folder) || real.as_ref().is_some_and(|real| dir.starts_with(real))
        };
        if self.views.iter().any(holds) {
            return Some(A_VIEW);
        }
        self.agents.iter().any(holds).then_some(AN_AGENT)
    }
}

fn terminals(host: &api::Host) -> Result<Vec<(Option<TerminalKey>, PathBuf)>, String> {
    let kept = match host.projects.as_ref().and_then(|projects| projects.held.as_ref()) {
        None => return Ok(Vec::new()),
        Some(Held::Unkept(why)) => {
            return Err(format!("lunetad keeps no layouts, and may leave out a terminal: {why}"));
        },
        Some(Held::Kept(kept)) => kept,
    };
    let mut worktrees = Vec::new();
    for project in &kept.projects {
        let unsaid = || format!("lunetad says not what project {} holds", project.name);
        worktrees.extend(project.worktrees().ok_or_else(unsaid)?);
    }
    let terminal = |terminal: &api::Terminal| {
        let key = match &terminal.found {
            Some(Found::Key(key)) => TerminalKey::of(key).ok(),
            Some(Found::Lost(()) | Found::Looking(())) | None => None,
        };
        (key, PathBuf::from(&terminal.folder))
    };
    Ok(worktrees.into_iter().flat_map(|worktree| &worktree.terminals).map(terminal).collect())
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Listed {
    path: String,
    head: String,
    branch: Option<String>,
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

fn on_repository(common: &str) -> Command {
    let mut git = Command::new("git");
    git::local(&mut git).args(["-c", "safe.bareRepository=explicit", "-c", "core.fsmonitor=false"]);
    git.arg(format!("--git-dir={common}")).current_dir("/");
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

fn clean(folder: &str) -> Result<bool, String> {
    let mut git = Command::new("git");
    git::local(&mut git).args(["-c", "safe.bareRepository=explicit", "-c", "core.fsmonitor=false"]);
    git.args(["-C", folder, "--no-optional-locks", "status", "--porcelain"]);
    git.arg("--untracked-files=normal");
    says(git, GIT_SAYS, "status").map(|said| said.is_empty())
}

fn common_of(dir: &Directory) -> Result<String, String> {
    match git::common(dir.as_str()) {
        Common::Of(common) => Ok(common),
        Common::NoRepository => Err(format!("{dir} is in no repository")),
        Common::Unsaid(why) => Err(format!("git did not say what repository {dir} is in: {why}")),
    }
}

fn stays(
    listed: &Listed,
    all: &[Listed],
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
    let folder = Path::new(&listed.path);
    let in_it = |other: &Listed| {
        !other.prunable && other.path != listed.path && Path::new(&other.path).starts_with(folder)
    };
    if all.iter().any(in_it) {
        return Ok(Some(HOLDS_ANOTHER));
    }
    if !clean(&listed.path)? {
        return Ok(Some(CHANGED));
    }
    if let Some(why) = occupied.why_in(folder) {
        return Ok(Some(why));
    }
    let runs_here = here.is_some_and(|here| {
        here.starts_with(folder)
            || fs::canonicalize(folder).is_ok_and(|real| here.starts_with(real))
    });
    Ok(runs_here.then_some(RUNS_HERE))
}

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

fn push_target(common: &str, branch: &str) -> Result<Option<RemoteBranch>, String> {
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
    Ok(Some(RemoteBranch { remote: remote.to_owned(), branch: there.to_owned() }))
}

fn default_heard(common: &str, remote: &str) -> Option<String> {
    let head = format!("refs/remotes/{remote}/HEAD");
    git::pointed(on_repository(common), &head, &format!("refs/remotes/{remote}/"), GIT_SAYS)
}

fn remote_default(common: &str, remote: &str) -> Result<Option<String>, String> {
    if let Some(default) = default_heard(common, remote) {
        return Ok(Some(default));
    }
    let mut git = on_repository(common);
    git.args(["ls-remote", "--symref", "--", remote, "HEAD"]);
    let said = says_alone(git, "ls-remote")?;
    let said = String::from_utf8_lossy(&said);
    let named = said.lines().find_map(|line| {
        line.strip_prefix("ref: refs/heads/")?.strip_suffix("\tHEAD")
    });
    Ok(named.map(str::to_owned))
}

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

fn push_urls(common: &str, remote: &str) -> Result<Vec<String>, String> {
    let mut git = on_repository(common);
    git.args(["config", "--get-regexp", r"^remote\..*\.(pushurl|url)$"]);
    let said = says(git, GIT_SAYS, "config")?;
    let said = String::from_utf8_lossy(&said);
    let of = |key: &str| -> Vec<String> {
        let key = format!("remote.{remote}.{key} ");
        said.lines().filter_map(|line| line.strip_prefix(&key)).map(str::to_owned).collect()
    };
    let pushes = of("pushurl");
    let urls = if pushes.is_empty() { of("url") } else { pushes };
    if urls.is_empty() {
        return Err(format!("{remote} has no url"));
    }
    Ok(urls)
}

struct Remotes<'a> {
    common: &'a str,
    heads: HashMap<String, Result<HashMap<String, String>, String>>,
    urls: HashMap<String, Result<Vec<String>, String>>,
    logins: HashMap<String, Result<String, String>>,
}

impl Remotes<'_> {
    fn pushed(&mut self, target: Option<&RemoteBranch>, pulled: &Pulled) -> Pushed {
        let Some(RemoteBranch { remote, branch }) = target else {
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
        let Some(repository) = &pulled.head_repository else {
            return Pushed::Stays("its pull request's head repository has gone".to_owned());
        };
        let urls = self.urls.entry(remote.clone()).or_insert_with(|| push_urls(common, remote));
        match urls {
            Ok(urls) if urls.iter().all(|url| repository.is_at(url)) => {},
            Ok(_) => {
                return Pushed::Stays(format!("{remote} is not where its pull request's head is"));
            },
            Err(why) => return Pushed::Stays(format!("git did not say where {remote} is: {why}")),
        }
        let host = &repository.host;
        let login = self.logins.entry(host.clone()).or_insert_with(|| github::logged_in(host));
        match login {
            Ok(login) if pulled.author.as_deref() == Some(login.as_str()) => {
                Pushed::Goes(RemoteBranch { remote: remote.clone(), branch: branch.clone() })
            },
            Ok(_) => Pushed::Stays("someone else wrote its pull request".to_owned()),
            Err(why) => Pushed::Stays(format!("gh did not say who you are: {why}")),
        }
    }
}

fn bytes_of(folder: &str) -> Option<u64> {
    let mut du = Command::new("du");
    du.args(["-sk", "--", folder]);
    let said = program::run(&mut du, GIT_SAYS).ok()?.answer().ok()?;
    let said = String::from_utf8_lossy(&said);
    let kibibytes: u64 = said.split_whitespace().next()?.parse().ok()?;
    kibibytes.checked_mul(1024)
}

pub fn plan(dir: &Directory, occupied: &Occupied, here: Option<&Path>) -> Result<Planned, String> {
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
            Err(why) => plan.unasked.push(Unasked { folder: on.listed.path.clone(), why }),
        }
    }
    let going = sorted_out(&mut plan, merged, &listed, occupied, here, &common)?;
    let others = at_once(&going, Asker::default, |asker, (on, pulled, _): &Going<'_>| {
        others_of(asker, &common, on, pulled)
    });
    let mut remotes = Remotes {
        common: &common,
        heads: HashMap::new(),
        urls: HashMap::new(),
        logins: HashMap::new(),
    };
    let mut goes = Vec::new();
    for ((on, pulled, target), others) in going.into_iter().zip(others) {
        let (folder, number) = (on.listed.path.clone(), pulled.pull_request.number.get());
        let merged_into = match others {
            Ok(Others::Leave) => None,
            Ok(Others::MergedInto(merged)) => Some(merged),
            Ok(Others::Need(why)) => {
                plan.kept.push(Kept { folder, number, why });
                continue;
            },
            Err(why) => {
                plan.unasked.push(Unasked { folder, why });
                continue;
            },
        };
        let pushed = match (remotes.pushed(target.as_ref(), &pulled), merged_into) {
            (Pushed::Goes(_), Some(merged)) => {
                Pushed::Stays(format!("#{merged} was merged into it"))
            },
            (pushed, _) => pushed,
        };
        let branch = on.branch.to_owned();
        goes.push(Goes { folder, branch, head: pulled.head, number, pushed, bytes: None });
    }
    let bytes = at_once(&goes, || (), |(), goes: &Goes| bytes_of(&goes.folder));
    for (goes, bytes) in goes.iter_mut().zip(bytes) {
        goes.bytes = bytes;
    }
    goes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.folder.cmp(&b.folder)));
    plan.goes = goes;
    Ok(Planned { plan, common })
}

type Going<'a> = (OnBranch<'a>, Pulled, Option<RemoteBranch>);

enum Others {
    Need(String),
    MergedInto(u32),
    Leave,
}

fn others_of(
    asker: &mut Asker,
    common: &str,
    on: &OnBranch<'_>,
    pulled: &Pulled,
) -> Result<Others, String> {
    let dir = &on.listed.path;
    if let Some(open) = asker.open_on_parent(dir, common, on.branch)? {
        return Ok(Others::Need(format!("#{open} is open on its parent")));
    }
    let Some(repository) = &pulled.head_repository else {
        return Ok(Others::Leave);
    };
    Ok(match github::based_on(dir, repository, &pulled.head_name)? {
        Based::Open(open) => Others::Need(format!("#{open} is based on it")),
        Based::Merged(merged) => Others::MergedInto(merged),
        Based::Neither => Others::Leave,
    })
}

fn sorted_out<'a>(
    plan: &mut Plan,
    merged: Vec<(OnBranch<'a>, Pulled)>,
    all: &[Listed],
    occupied: &Occupied,
    here: Option<&Path>,
    common: &str,
) -> Result<Vec<Going<'a>>, String> {
    let mut default = None;
    let mut defaults: HashMap<String, Result<Option<String>, String>> = HashMap::new();
    let mut going = Vec::new();
    for (on, pulled) in merged {
        let (folder, number) = (on.listed.path.clone(), pulled.pull_request.number.get());
        let why = match stays(on.listed, all, &pulled.head, occupied, here) {
            Ok(why) => why,
            Err(why) => {
                plan.unasked.push(Unasked { folder, why });
                continue;
            },
        };
        if let Some(why) = why {
            plan.kept.push(Kept { folder, number, why: why.to_owned() });
            continue;
        }
        let default = match &default {
            Some(default) => default,
            None => default.insert(github::default_branch(&on.listed.path)?),
        };
        let target = push_target(common, on.branch)?;
        let far = target.as_ref().map(|target| {
            let remote = &target.remote;
            defaults.entry(remote.clone()).or_insert_with(|| remote_default(common, remote)).clone()
        });
        let far = match far.transpose() {
            Ok(far) => far.flatten(),
            Err(why) => {
                plan.unasked.push(Unasked { folder, why });
                continue;
            },
        };
        let is_default = |name: &str| name == default.as_str() || far.as_deref() == Some(name);
        let there = target.as_ref().map(|target| target.branch.as_str());
        if is_default(on.branch) || there.is_some_and(is_default) {
            plan.kept.push(Kept { folder, number, why: DEFAULT.to_owned() });
            continue;
        }
        going.push((on, pulled, target));
    }
    Ok(going)
}

pub fn deleted(
    planned: Planned,
    occupied: impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Vec<Outcome> {
    let Planned { plan, common } = planned;
    let outcome = |goes: Goes| {
        let came = match removed(&common, &goes, &occupied, here) {
            Ok(()) => match branch_deleted(&common, &goes) {
                Ok(()) => Came::Gone,
                Err(why) => Came::WentBut(why),
            },
            Err(why) => Came::Stays(why),
        };
        Outcome { folder: goes.folder, came }
    };
    plan.goes.into_iter().map(outcome).collect()
}

pub fn deleted_as_agreed(
    dir: &Directory,
    agreed: &BTreeSet<Deleting>,
    occupied: impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Result<Pruned, String> {
    let now = occupied().map_err(|why| format!("luneta cannot tell its views: {why}"))?;
    let planned = plan(dir, &now, here)?;
    if planned.plan.deletes() != *agreed {
        return Ok(Pruned::Changed(planned.plan));
    }
    Ok(Pruned::Went(deleted(planned, occupied, here)))
}

fn removed(
    common: &str,
    goes: &Goes,
    occupied: &impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Result<(), String> {
    stands_as_planned(common, goes, occupied, here)?;
    let Pushed::Goes(far) = &goes.pushed else {
        return worktree_removed(common, &goes.folder);
    };
    far_branch_deleted(common, &goes.head, far)?;
    stands_as_planned(common, goes, occupied, here)
        .and_then(|()| worktree_removed(common, &goes.folder))
        .map_err(|why| format!("{why}; its branch on {} has gone", far.remote))
}

fn stands_as_planned(
    common: &str,
    goes: &Goes,
    occupied: &impl Fn() -> Result<Occupied, String>,
    here: Option<&Path>,
) -> Result<(), String> {
    let occupied = occupied().map_err(|why| format!("luneta cannot tell its views: {why}"))?;
    let all = worktrees(common)?;
    let listed = all
        .iter()
        .find(|listed| listed.path == goes.folder)
        .ok_or_else(|| "git lists no such worktree now".to_owned())?;
    if listed.branch.as_deref() != Some(goes.branch.as_str()) {
        return Err(format!("it is no longer on {}", goes.branch));
    }
    match stays(listed, &all, &goes.head, &occupied, here)? {
        Some(why) => Err(why.to_owned()),
        None => Ok(()),
    }
}

fn worktree_removed(common: &str, folder: &str) -> Result<(), String> {
    let mut git = on_repository(common);
    git.args(["-c", "status.showUntrackedFiles=normal", "worktree", "remove", "--", folder]);
    says(git, REMOVING, "worktree remove").map(|_| ())
}

fn far_branch_deleted(common: &str, head: &str, far: &RemoteBranch) -> Result<(), String> {
    let RemoteBranch { remote, branch } = far;
    let there = format!("refs/heads/{branch}");
    let mut git = on_repository(common);
    git.arg("push").arg(format!("--force-with-lease={there}:{head}"));
    git.args(["--", remote, &format!(":{there}")]);
    says_alone(git, "push")
        .map(|_| ())
        .map_err(|why| format!("its branch on {remote} stays: {why}"))
}

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
        .map(|_| ())
        .map_err(|why| format!("its branch {} stays: {why}", goes.branch))
}

impl From<&RemoteBranch> for api::prunes_response::RemoteBranch {
    fn from(far: &RemoteBranch) -> Self {
        let (remote, branch) = (far.remote.clone(), far.branch.clone());
        api::prunes_response::RemoteBranch { remote, branch }
    }
}

impl From<api::prunes_response::RemoteBranch> for RemoteBranch {
    fn from(far: api::prunes_response::RemoteBranch) -> Self {
        RemoteBranch { remote: far.remote, branch: far.branch }
    }
}

impl From<&Goes> for api::prunes_response::Goes {
    fn from(goes: &Goes) -> Self {
        use api::prunes_response::goes::Pushed as Said;
        let pushed = match &goes.pushed {
            Pushed::Nowhere => None,
            Pushed::Goes(far) => Some(Said::Remote(far.into())),
            Pushed::Stays(why) => Some(Said::Stays(why.clone())),
        };
        api::prunes_response::Goes {
            folder: goes.folder.clone(),
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
            Some(Said::Remote(far)) => Pushed::Goes(far.into()),
            Some(Said::Stays(why)) => Pushed::Stays(why),
        };
        Goes {
            folder: goes.folder,
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
            folder: kept.folder.clone(),
            number: kept.number,
            why: kept.why.clone(),
        });
        let unasked = plan.unasked.iter().map(|unasked| api::prunes_response::Unasked {
            folder: unasked.folder.clone(),
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
            folder: kept.folder,
            number: kept.number,
            why: kept.why,
        });
        let unasked = said
            .unasked
            .into_iter()
            .map(|unasked| Unasked { folder: unasked.folder, why: unasked.why });
        Plan {
            repository: said.repository,
            goes: said.goes.into_iter().map(Into::into).collect(),
            kept: kept.collect(),
            unasked: unasked.collect(),
        }
    }
}

impl From<&Deleting> for api::prune_request::Deleting {
    fn from(deleting: &Deleting) -> Self {
        api::prune_request::Deleting {
            folder: deleting.folder.clone(),
            branch: deleting.branch.clone(),
            head: deleting.head.clone(),
            number: deleting.number,
            remote: deleting.remote.as_ref().map(Into::into),
        }
    }
}

impl From<api::prune_request::Deleting> for Deleting {
    fn from(said: api::prune_request::Deleting) -> Self {
        Deleting {
            folder: said.folder,
            branch: said.branch,
            head: said.head,
            number: said.number,
            remote: said.remote.map(Into::into),
        }
    }
}

impl From<&Pruned> for PruneResponse {
    fn from(pruned: &Pruned) -> Self {
        use api::prune_response::outcome::Came as SaidCame;
        use api::prune_response::{Answer, Outcomes};
        let said = |outcome: &Outcome| api::prune_response::Outcome {
            folder: outcome.folder.clone(),
            came: Some(match &outcome.came {
                Came::Gone => SaidCame::Gone(()),
                Came::WentBut(why) => SaidCame::WentBut(why.clone()),
                Came::Stays(why) => SaidCame::Stays(why.clone()),
            }),
        };
        let answer = match pruned {
            Pruned::Changed(plan) => Answer::Changed(plan.into()),
            Pruned::Went(outcomes) => {
                Answer::Deleted(Outcomes { outcomes: outcomes.iter().map(said).collect() })
            },
        };
        PruneResponse { answer: Some(answer) }
    }
}

pub fn pruned_of(said: PruneResponse) -> Result<Pruned, String> {
    use api::prune_response::Answer;
    match said.answer {
        Some(Answer::Changed(plan)) => Ok(Pruned::Changed(plan.into())),
        Some(Answer::Deleted(deleted)) => {
            let outcomes = deleted.outcomes.into_iter().map(outcome_of);
            outcomes.collect::<Result<_, _>>().map(Pruned::Went)
        },
        None => Err("lunetad's answer says nothing of how it went".to_owned()),
    }
}

fn outcome_of(said: api::prune_response::Outcome) -> Result<Outcome, String> {
    use api::prune_response::outcome::Came as SaidCame;
    let folder = said.folder;
    let came = match said.came {
        Some(SaidCame::Gone(())) => Came::Gone,
        Some(SaidCame::WentBut(why)) => Came::WentBut(why),
        Some(SaidCame::Stays(why)) => Came::Stays(why),
        None => return Err(format!("lunetad's answer says nothing of how {folder} went")),
    };
    Ok(Outcome { folder, came })
}

#[must_use]
pub fn asked(on: Option<On>, dir: &Directory) -> PrunesRequest {
    PrunesRequest { on, dir: dir.as_str().to_owned() }
}

#[must_use]
pub fn told(on: Option<On>, dir: &Directory, agreed: &BTreeSet<Deleting>) -> PruneRequest {
    PruneRequest {
        on,
        dir: dir.as_str().to_owned(),
        agreed: agreed.iter().map(Into::into).collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use luneta_protocol::github::PullRequest;

    use crate::github::RepositoryName;

    use super::*;

    fn goes(folder: &str, pushed: Pushed, bytes: Option<u64>) -> Goes {
        let (branch, head) = ("fix".to_owned(), "abc".to_owned());
        Goes { folder: folder.to_owned(), branch, head, number: 1, pushed, bytes }
    }

    fn plan_of(goes: Vec<Goes>) -> Plan {
        Plan { repository: "/w/luneta".to_owned(), goes, kept: Vec::new(), unasked: Vec::new() }
    }

    fn on_origin(branch: &str) -> RemoteBranch {
        RemoteBranch { remote: "origin".to_owned(), branch: branch.to_owned() }
    }

    #[test]
    fn two_plans_are_the_same_by_what_they_delete() {
        let (a, b) =
            (|bytes| goes("/w/a", Pushed::Nowhere, bytes), goes("/w/b", Pushed::Nowhere, None));
        let plan = plan_of(vec![a(Some(9)), b.clone()]);
        assert_eq!(plan.deletes(), plan_of(vec![b.clone(), a(Some(1))]).deletes());
        assert_ne!(plan.deletes(), plan_of(vec![a(Some(9))]).deletes());
        let remote = || Pushed::Goes(on_origin("fix"));
        let pushed = plan_of(vec![goes("/w/a", remote(), None)]);
        let stays = |why: &str| plan_of(vec![goes("/w/a", Pushed::Stays(why.to_owned()), None)]);
        assert_ne!(pushed.deletes(), stays("moved").deletes());
        assert_eq!(stays("moved").deletes(), stays("no agent").deletes());
        let nowhere = plan_of(vec![goes("/w/a", Pushed::Nowhere, None)]);
        assert_eq!(stays("moved").deletes(), nowhere.deletes());
        let moved = Goes { head: "def".to_owned(), ..goes("/w/a", Pushed::Nowhere, None) };
        assert_ne!(plan_of(vec![moved]).deletes(), stays("moved").deletes());
    }

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

    fn viewed(held: api::projects::Held) -> api::Host {
        let projects = Some(api::Projects { held: Some(held) });
        api::Host { projects, ..api::Host::default() }
    }

    #[test]
    fn an_agent_in_a_worktree_or_under_it_keeps_it() {
        let agents = vec![api::Agent { cwd: "/w/c/src".to_owned(), ..api::Agent::default() }];
        let host = api::Host { agents, ..api::Host::default() };
        let occupied = Occupied::of(&host).unwrap();
        assert_eq!(occupied.why_in(Path::new("/w/c")), Some(AN_AGENT));
        assert_eq!(occupied.why_in(Path::new("/w/cc")), None);
    }

    fn kept(terminals: Vec<api::Terminal>) -> api::projects::Held {
        let worktree = api::Worktree { terminals, ..api::Worktree::default() };
        let holds = Some(api::project::Holds::Folder(worktree));
        let project = api::Project { name: "luneta".to_owned(), holds, ..api::Project::default() };
        api::projects::Held::Kept(api::projects::Kept { projects: vec![project], version: None })
    }

    fn in_layout(folder: &str, found: api::terminal::Found) -> api::Terminal {
        api::Terminal { folder: folder.to_owned(), found: Some(found), ..api::Terminal::default() }
    }

    fn terminal_key(id: u32) -> api::TerminalKey {
        api::TerminalKey { id, made_at: Some(prost_types::Timestamp::default()) }
    }

    #[test]
    fn a_terminal_of_a_layout_keeps_its_folder_found_or_lost() {
        use api::terminal::Found;
        let host = viewed(kept(vec![
            in_layout("/w/a/src", Found::Key(terminal_key(4))),
            in_layout("/w/b", Found::Lost(())),
            in_layout("/w/c", Found::Looking(())),
        ]));
        let occupied = Occupied::of(&host).unwrap();
        for held in ["/w/a", "/w/b", "/w/c"] {
            assert_eq!(occupied.why_in(Path::new(held)), Some(A_VIEW), "{held}");
        }
        assert_eq!(occupied.why_in(Path::new("/w/d")), None);
        let unkept = viewed(api::projects::Held::Unkept("a newer file".to_owned()));
        let why = "lunetad keeps no layouts, and may leave out a terminal: a newer file";
        assert_eq!(Occupied::of(&unkept), Err(why.to_owned()));
        let mut unsaid = kept(Vec::new());
        let api::projects::Held::Kept(held) = &mut unsaid else {
            panic!("the projects are kept");
        };
        held.projects[0].holds = None;
        let why = "lunetad says not what project luneta holds";
        assert_eq!(Occupied::of(&viewed(unsaid)), Err(why.to_owned()));
    }

    #[test]
    fn the_luneta_skill_quotes_why_a_worktree_with_a_view_stays() {
        let skill = crate::asks::SKILL.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(skill.contains(A_VIEW), "the skill quotes {A_VIEW:?}");
    }

    #[test]
    fn a_file_unread_in_a_terminal_no_layout_holds_could_be_anywhere() {
        use api::terminal::Found;
        let terminal = Some(terminal_key(4));
        let file = api::UnreadFile { pid: 9, terminal };
        let host = |terminals| api::Host {
            unread: 1,
            unread_files: vec![file],
            ..viewed(kept(terminals))
        };
        assert!(Occupied::of(&host(vec![in_layout("/w/a", Found::Key(terminal_key(4)))])).is_ok());
        let unheld = "lunetad cannot read the session file of agent 9, which runs in a terminal no \
                      layout holds";
        for terminals in [vec![], vec![in_layout("/w/a", Found::Key(terminal_key(5)))]] {
            assert_eq!(Occupied::of(&host(terminals)), Err(unheld.to_owned()));
        }
    }

    #[test]
    fn what_the_daemon_could_not_read_could_be_in_any_worktree() {
        assert!(Occupied::of(&api::Host::default()).is_ok());
        let files =
            |unread, unread_files| api::Host { unread, unread_files, ..api::Host::default() };
        let file = || api::UnreadFile { pid: 9, terminal: None };
        let unplaced = "lunetad cannot read 1 of Claude Code's session files, nor tell where its \
                        agent runs";
        assert_eq!(Occupied::of(&files(1, vec![])), Err(unplaced.to_owned()));
        let zero = api::TerminalKey { id: 0, made_at: Some(prost_types::Timestamp::default()) };
        let unkeyed = api::UnreadFile { terminal: Some(zero), ..file() };
        assert_eq!(Occupied::of(&files(1, vec![unkeyed])), Err(unplaced.to_owned()));
        let unseated = "lunetad cannot read the session file of agent 9, which runs in no terminal";
        assert_eq!(Occupied::of(&files(1, vec![file()])), Err(unseated.to_owned()));
    }

    fn pulled(author: Option<&str>, head: &str) -> Pulled {
        let number = NonZeroU32::new(1).unwrap();
        Pulled {
            pull_request: PullRequest {
                number,
                state: State::Merged,
                url: None,
                checks: None,
                stale: false,
            },
            head_name: "origin-branch".to_owned(),
            head: head.to_owned(),
            author: author.map(str::to_owned),
            head_repository: Some(RepositoryName {
                host: "github.com".to_owned(),
                owner: "o".to_owned(),
                name: "n".to_owned(),
            }),
        }
    }

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
                ("unsaid".to_owned(), ok("unsaid", "abc")),
                ("fork".to_owned(), ok("fork", "abc")),
                ("mirrored".to_owned(), ok("mirrored", "abc")),
            ]),
            urls: HashMap::from([
                ("origin".to_owned(), Ok(vec!["git@github.com:o/n.git".to_owned()])),
                ("unsaid".to_owned(), Err("no key".to_owned())),
                ("fork".to_owned(), Ok(vec!["https://github.com/me/n".to_owned()])),
                (
                    "mirrored".to_owned(),
                    Ok(vec!["git@github.com:o/n.git".to_owned(), "git@github.com:me/n".to_owned()]),
                ),
            ]),
            logins: HashMap::from([("github.com".to_owned(), Ok("me".to_owned()))]),
        };
        let target = |remote: &str| {
            Some(RemoteBranch { remote: remote.to_owned(), branch: format!("{remote}-branch") })
        };
        let goes = Pushed::Goes(on_origin("origin-branch"));
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(Some("me"), "abc")), goes);
        assert_eq!(remotes.pushed(None, &pulled(Some("me"), "abc")), Pushed::Nowhere);
        let elsewhere = Some(on_origin("other"));
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
        let of = |remote: &str| {
            let head_name = format!("{remote}-branch");
            Pulled { head_name, ..pulled(Some("me"), "abc") }
        };
        for remote in ["fork", "mirrored"] {
            assert_eq!(
                remotes.pushed(target(remote).as_ref(), &of(remote)),
                Pushed::Stays(format!("{remote} is not where its pull request's head is"))
            );
        }
        assert_eq!(
            remotes.pushed(target("unsaid").as_ref(), &of("unsaid")),
            Pushed::Stays("git did not say where unsaid is: no key".to_owned())
        );
        let gone = Pulled { head_repository: None, ..pulled(Some("me"), "abc") };
        assert_eq!(
            remotes.pushed(target("origin").as_ref(), &gone),
            Pushed::Stays("its pull request's head repository has gone".to_owned())
        );
        let theirs = Pushed::Stays("someone else wrote its pull request".to_owned());
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(Some("them"), "abc")), theirs);
        assert_eq!(remotes.pushed(target("origin").as_ref(), &pulled(None, "abc")), theirs);
        assert_eq!(
            remotes.pushed(target("gone").as_ref(), &pulled(Some("me"), "abc")),
            Pushed::Stays("git did not reach gone: no key".to_owned())
        );
    }

    #[test]
    fn a_remote_pushes_to_each_url_its_config_has() {
        let scratch = tempfile::tempdir().unwrap();
        let common = scratch.path().join("r.git");
        let common = common.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut git = on_repository(common);
            assert!(git.args(args).status().unwrap().success(), "{args:?}");
        };
        let mut init = Command::new("git");
        assert!(init.args(["init", "-q", "--bare", common]).status().unwrap().success());
        git(&["config", "remote.fetched.url", "https://x/o/f"]);
        git(&["config", "remote.fetched-too.url", "https://x/o/too"]);
        git(&["config", "remote.pushed.url", "https://x/o/u"]);
        git(&["config", "--add", "remote.pushed.pushurl", "https://x/o/a"]);
        git(&["config", "--add", "remote.pushed.pushurl", "https://x/o/b"]);
        let said = |remote: &str| push_urls(common, remote);
        assert_eq!(said("fetched"), Ok(vec!["https://x/o/f".to_owned()]));
        assert_eq!(said("pushed"), Ok(vec!["https://x/o/a".to_owned(), "https://x/o/b".to_owned()]));
        assert_eq!(said("none"), Err("none has no url".to_owned()));
    }

    fn ran(dir: &Path, args: &[&str]) {
        let mut git = Command::new("git");
        git::local(&mut git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(dir)
            .args(args);
        assert!(git.status().unwrap().success(), "{args:?}");
    }

    #[test]
    fn git_refuses_to_remove_an_untracked_file_whatever_it_is_set_to_show() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = scratch.path().canonicalize().unwrap();
        let (repository, folder) = (scratch.join("r"), scratch.join("w"));
        ran(&scratch, &["init", "-q", "-b", "main", repository.to_str().unwrap()]);
        ran(&repository, &["commit", "-q", "--allow-empty", "-m", "one"]);
        ran(&repository, &["config", "status.showUntrackedFiles", "no"]);
        let at = folder.to_str().unwrap();
        ran(&repository, &["worktree", "add", "-q", "-b", "w", at]);
        fs::write(folder.join("new"), "not yet added").unwrap();
        let common = repository.join(".git");
        let common = common.to_str().unwrap();
        assert!(worktree_removed(common, at).is_err());
        assert!(folder.join("new").exists());
        fs::remove_file(folder.join("new")).unwrap();
        assert_eq!(worktree_removed(common, at), Ok(()));
        assert!(!folder.exists());
    }

    #[test]
    fn a_view_that_comes_as_its_far_branch_goes_keeps_the_worktree() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = scratch.path().canonicalize().unwrap();
        let (up, repository, folder) = (scratch.join("up"), scratch.join("r"), scratch.join("w"));
        ran(&scratch, &["init", "-q", "--bare", "-b", "main", up.to_str().unwrap()]);
        ran(&scratch, &["init", "-q", "-b", "main", repository.to_str().unwrap()]);
        ran(&repository, &["commit", "-q", "--allow-empty", "-m", "one"]);
        ran(&repository, &["remote", "add", "origin", up.to_str().unwrap()]);
        let at = folder.to_str().unwrap();
        ran(&repository, &["worktree", "add", "-q", "-b", "fix", at]);
        ran(&repository, &["push", "-q", "origin", "fix"]);
        let common = repository.join(".git");
        let common = common.to_str().unwrap();
        let listed = worktrees(common).unwrap();
        let head = listed.iter().find(|listed| listed.path == at).unwrap().head.clone();

        let occupied = || {
            let views = if remote_heads(common, "origin")?.contains_key("fix") {
                Vec::new()
            } else {
                vec![folder.clone()]
            };
            Ok(Occupied { views, agents: Vec::new() })
        };
        let goes = Goes {
            head,
            ..goes(at, Pushed::Goes(on_origin("fix")), None)
        };
        let planned = Planned { plan: plan_of(vec![goes]), common: common.to_owned() };
        let outcomes = deleted(planned, occupied, None);
        let why = "it has a view; its branch on origin has gone";
        assert_eq!(outcomes[0].came, Came::Stays(why.to_owned()));
        assert!(folder.join(".git").is_file());
        let heads = remote_heads(common, "origin").unwrap();
        assert!(!heads.contains_key("fix"));
    }

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
            goes("/w/b", Pushed::Goes(on_origin("x")), Some(7)),
            goes("/w/c", Pushed::Stays("moved".to_owned()), Some(0)),
        ];
        let kept = vec![Kept { folder: "/w/d".to_owned(), number: 2, why: "view".to_owned() }];
        let unasked = vec![Unasked { folder: "/w/e".to_owned(), why: "gh".to_owned() }];
        let plan = Plan { repository: "/w".to_owned(), goes, kept, unasked };
        assert_eq!(Plan::from(PrunesResponse::from(&plan)), plan);
        let agreed = plan.deletes();
        let dir = Directory::parse("/w".to_owned()).unwrap();
        let said = told(None, &dir, &agreed).agreed.into_iter().map(Into::into);
        assert_eq!(said.collect::<BTreeSet<Deleting>>(), agreed);
        let outcome = |folder: &str, came| Outcome { folder: folder.to_owned(), came };
        let went = Pruned::Went(vec![
            outcome("/w/a", Came::Gone),
            outcome("/w/b", Came::WentBut("x".to_owned())),
            outcome("/w/c", Came::Stays("y".to_owned())),
        ]);
        assert_eq!(pruned_of(PruneResponse::from(&went)).as_ref(), Ok(&went));
        let changed = Pruned::Changed(plan);
        assert_eq!(pruned_of(PruneResponse::from(&changed)).as_ref(), Ok(&changed));
    }

    #[test]
    fn an_answer_that_says_nothing_of_how_it_went_does_not_read() {
        use api::prune_response::{Answer, Outcome, Outcomes};
        assert!(pruned_of(PruneResponse { answer: None }).is_err());
        let outcomes = vec![Outcome { folder: "/w/a".to_owned(), came: None }];
        let answer = Some(Answer::Deleted(Outcomes { outcomes }));
        assert_eq!(
            pruned_of(PruneResponse { answer }),
            Err("lunetad's answer says nothing of how /w/a went".to_owned())
        );
    }
}
