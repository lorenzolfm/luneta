use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::mem;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime};

use lune_domain::agent::{Harness, OscReads, Rung};
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{
    Dir, Holds, Listed, Name, Project, Projects, Seen, Stage, Subfolder, ViewId, Worktree,
    WorktreeId, VIEWS_MAX, WORKTREES_MAX,
};
use lune_protocol::git::{GitStatus, Head};
use lune_protocol::github::PullRequest;
use lune_protocol::program_status::bound_fair;
use lune_protocol::said::Said;
use lune_protocol::shell::Doing;
use lune_protocol::HOST_SUBAGENTS_BYTES_MAX;
use prost::Message as _;
use tokio::sync::oneshot;

use crate::agent::{Agent, SeenAgent, Unread};
use crate::api;
use crate::claude::{self, Scan};
use crate::git;
use crate::github::Found;
use crate::mux::own::server::{self, Is};
use crate::{agy, osc_7501, pi};
use crate::process::{self, AgentId, AgentPid};
use crate::titles::{self, Titled};
use crate::projects::Revision;
use crate::remote::{Destination, Line, Named, Reached};
use crate::sayings::{Sayings, ToSay};
use crate::statuses::{ActiveStatus, Proof, Report, ReportedAgent, Statuses, Subagent};
use crate::transcripts::SessionId;
use crate::watch::Troubles;
use crate::Directory;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shown {
    Unlisted(TerminalKey),
    Listed {
        key: TerminalKey,
        cwd: Option<Dir>,
        titled: Option<Titled>,
        program: Program,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    Running { command: Option<Said>, doing: Option<Doing> },
    Ended,
}

impl Shown {
    #[must_use]
    pub fn unlisted(key: TerminalKey) -> Shown {
        Shown::Unlisted(key)
    }

    #[must_use]
    pub fn listed(key: TerminalKey, listed: &server::Terminal) -> Shown {
        let cwd = listed.cwd.as_ref().and_then(|cwd| Dir::parse(cwd.as_str()).ok());
        let titled = titles::titled(&listed.title);
        let program = match &listed.is {
            Is::Running { front, doing, .. } => Program::Running {
                command: front.as_ref().and_then(|front| Said::new(front.argv.join(" "))),
                doing: doing.clone(),
            },
            Is::Ended => Program::Ended,
        };
        Shown::Listed { key, cwd, titled, program }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MuxTerminal {
    cwd: Option<Directory>,
    root: AgentPid,
    front: Option<server::Foreground>,
    shell_idle: bool,
    reads: OscReads,
    status: ActiveStatus,
    at: Option<SystemTime>,
    subagents: Vec<Subagent>,
}

impl MuxTerminal {
    #[must_use]
    pub fn of(terminal: &server::Terminal) -> Option<MuxTerminal> {
        assert!(terminal.statuses.children.len() <= lune_protocol::PROGRAM_STATUS_RECORDS_MAX);
        let Is::Running { root, front, doing, .. } = &terminal.is else {
            return None;
        };
        let root_record = terminal.statuses.root.as_ref()?;
        let root_app = root_record.app.as_ref()?;
        let reads = Harness::from_osc_7501_app(root_app.as_str())?;
        let subagents: Vec<Subagent> = terminal
            .statuses
            .children
            .iter()
            .filter(|child| child.status.app.as_ref() == Some(root_app))
            .map(osc_7501::subagent)
            .collect();
        assert!(subagents.len() <= lune_protocol::PROGRAM_STATUS_RECORDS_MAX);
        let shell_idle =
            matches!(doing, Some(Doing::Ready | Doing::Ended { .. } | Doing::Stopped { .. }));
        Some(MuxTerminal {
            cwd: terminal.cwd.clone(),
            root: *root,
            front: front.clone(),
            shell_idle,
            reads,
            status: osc_7501::phase(&root_record.phase),
            at: root_record.at,
            subagents,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Located {
    Shown(Shown),
    Lost,
    Looking,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Branch {
    pub repository: Said,
    pub name: Said,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listing {
    Listed(Listed),
    Unlisted(String),
}

pub const FOLLOW_DIRS_MAX: usize = WORKTREES_MAX + VIEWS_MAX;

const _: () = assert!(FOLLOW_DIRS_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowDirs(BTreeSet<Dir>);

impl FollowDirs {
    fn new(dirs: BTreeSet<Dir>) -> Self {
        assert!(dirs.len() <= FOLLOW_DIRS_MAX);
        FollowDirs(dirs)
    }

    #[must_use]
    pub fn contains(&self, dir: &Dir) -> bool {
        self.0.contains(dir)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Dir> {
        self.0.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    fn is_subset(&self, other: &FollowDirs) -> bool {
        self.0.is_subset(&other.0)
    }
}

pub enum Command {
    Agents(Scan),
    PiAgents(Vec<ReportedAgent>),
    AgyAgents(Vec<ReportedAgent>),
    MuxTerminals(BTreeMap<TerminalKey, MuxTerminal>),
    Terminals(BTreeMap<ViewId, Located>),
    GitStatus {
        dir: Dir,
        status: GitStatus,
        why: Option<git::Why>,
        answer: oneshot::Sender<Option<String>>,
    },
    PullRequests(BTreeMap<String, Found>),
    Topics(HashMap<SessionId, Said>),
    Hosts(Named),
    Projects { projects: Result<Projects, String>, revision: Revision },
    Missing(BTreeSet<Dir>),
    Listed(BTreeMap<Dir, Listing>),
    Remote { destination: Destination, reached: Reached },
    Say { said: ToSay, at: SystemTime, answer: oneshot::Sender<()> },
    Dirs(oneshot::Sender<FollowDirs>),
    ProjectFolders(oneshot::Sender<BTreeSet<Dir>>),
    Clones(oneshot::Sender<BTreeMap<Dir, BTreeSet<WorktreeId>>>),
    Plain(oneshot::Sender<BTreeSet<Dir>>),
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    Tracking(oneshot::Sender<BTreeMap<String, Branch>>),
    Claudes(oneshot::Sender<Vec<Agent>>),
    AllAgents(oneshot::Sender<Vec<Agent>>),
    List(oneshot::Sender<api::ListAgentsResponse>),
    Host(oneshot::Sender<api::Host>),
    Watch { after: After, answer: oneshot::Sender<Result<api::Host, Full>> },
}

#[derive(Debug)]
pub struct Full;

#[derive(Debug)]
pub struct After {
    pub version: api::Version,
    pub parked: Parked,
}

#[derive(Debug)]
pub enum Parked {
    Quietly,
    Aloud(oneshot::Sender<api::Version>),
}

pub struct Wakes {
    pub git: SyncSender<()>,
    pub github: SyncSender<()>,
    pub projects: SyncSender<()>,
}

const COMMANDS_MAX: usize = 64;

const WAITING_MAX: usize = 256;

pub fn ask<T>(
    core: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    core.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
}

pub fn start(wakes: Wakes) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands, &wakes))?;
    Ok(send)
}

enum Remote {
    Far(Destination, Reached),
    Here(Destination),
}

impl Remote {
    fn to_api(&self) -> api::Remote {
        match self {
            Remote::Far(destination, reached) => reached.to_api(destination),
            Remote::Here(destination) => api::Remote {
                destination: destination.to_string(),
                reached: Some(api::remote::Reached::Here(())),
            },
        }
    }
}

struct CachedProcess {
    id: AgentId,
    started_at: Option<SystemTime>,
    cwd: Option<Directory>,
}

struct Known {
    found: Scan,
    pi: Vec<ReportedAgent>,
    agy: Vec<ReportedAgent>,
    contested: HashMap<AgentId, Contest>,
    mux_terminals: BTreeMap<TerminalKey, MuxTerminal>,
    osc_agents: Vec<SeenAgent>,
    osc_processes: HashMap<(TerminalKey, AgentPid), CachedProcess>,
    osc_troubles: Troubles,
    agent_statuses: Statuses,
    terminals: BTreeMap<ViewId, Located>,
    statuses: BTreeMap<Dir, GitStatus>,
    pull_requests: BTreeMap<String, Found>,
    topics: HashMap<SessionId, Said>,
    remotes: Vec<Remote>,
    unread_hosts: u32,
    projects: Result<Projects, String>,
    revision: Revision,
    missing: BTreeSet<Dir>,
    listed: BTreeMap<Dir, Listing>,
    host: api::Host,
    seq: u64,
    waiting: Vec<oneshot::Sender<Result<api::Host, Full>>>,
    sayings: Sayings,
}

fn run(commands: &Receiver<Command>, wakes: &Wakes) {
    let mut known = Known {
        found: Scan { agents: Vec::new(), unread: Vec::new() },
        pi: Vec::new(),
        agy: Vec::new(),
        contested: HashMap::new(),
        mux_terminals: BTreeMap::new(),
        osc_agents: Vec::new(),
        osc_processes: HashMap::new(),
        osc_troubles: Troubles::default(),
        agent_statuses: Statuses::default(),
        terminals: BTreeMap::new(),
        statuses: BTreeMap::new(),
        pull_requests: BTreeMap::new(),
        topics: HashMap::new(),
        remotes: Vec::new(),
        unread_hosts: 0,
        projects: Err("luned has read no projects yet".to_owned()),
        revision: Revision::default(),
        missing: BTreeSet::new(),
        listed: BTreeMap::new(),
        host: api::Host::default(),
        seq: 0,
        waiting: Vec::with_capacity(WAITING_MAX),
        sayings: Sayings::default(),
    };
    known.host = api::Host { version: Some(known.version()), ..known.built() };
    for command in commands {
        match command {
            Command::Agents(now) => {
                let pids: HashSet<AgentPid> =
                    now.agents.iter().map(|agent| agent.seen().id.pid()).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                assert!(now.agents.iter().all(|agent| agent.report().reads() == claude::READS));
                known.found = now;
                known.settle_readers();
            },
            Command::PiAgents(now) => {
                let pids: HashSet<AgentPid> =
                    now.iter().map(|agent| agent.seen().id.pid()).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                assert!(now.iter().all(|agent| agent.report().reads() == pi::READS));
                known.pi = now;
                known.settle_readers();
            },
            Command::AgyAgents(now) => {
                let pids: HashSet<AgentPid> =
                    now.iter().map(|agent| agent.seen().id.pid()).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                assert!(now.iter().all(|agent| agent.report().reads() == agy::READS));
                known.agy = now;
                known.settle_readers();
            },
            Command::MuxTerminals(now) if now == known.mux_terminals => continue,
            Command::MuxTerminals(now) => {
                known.mux_terminals = now;
                known.reconcile_osc_statuses();
            },
            Command::Terminals(now) if now == known.terminals => continue,
            Command::Terminals(now) => {
                assert!(now.len() <= VIEWS_MAX);
                let before = known.dirs();
                known.terminals = now;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::GitStatus { dir, status, why, answer } => {
                if !known.dirs().contains(&dir) {
                    let _ = answer.send(None);
                    continue;
                }
                let before = known.branch(&dir).map(str::to_owned);
                let changed = known.statuses.get(&dir).is_some_and(|was| *was != status);
                known.statuses.insert(dir.clone(), status);
                let _ = answer.send(why.filter(|_| changed).and_then(|why| why.said(dir.as_str())));
                if known.branch(&dir).is_some_and(|now| before.as_deref() != Some(now)) {
                    wake(&wakes.github);
                }
            },
            Command::PullRequests(now) => known.pull_requests = now,
            Command::Topics(now) => known.topics = now,
            Command::Hosts(now) => known.named(now),
            Command::Projects { projects, revision } => {
                let before = known.dirs();
                let before_projects = known.watched_projects();
                known.projects = projects;
                known.revision = revision;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
                if known.watched_projects() != before_projects {
                    wake(&wakes.projects);
                }
            },
            Command::Missing(now) if now == known.missing => continue,
            Command::Missing(now) => {
                let before = known.dirs();
                known.missing = now;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::Listed(now) if now == known.listed => continue,
            Command::Listed(now) => known.listed = now,
            Command::Remote { destination, reached } => {
                let far = known.remotes.iter_mut().find_map(|remote| match remote {
                    Remote::Far(far, was) if *far == destination => Some(was),
                    Remote::Far(..) | Remote::Here(_) => None,
                });
                let Some(was) = far else {
                    continue;
                };
                *was = reached;
            },
            Command::Say { said, at, answer } => {
                known.sayings.take(said, at);
                let _ = answer.send(());
            },
            Command::Dirs(answer) => {
                let _ = answer.send(known.dirs());
                continue;
            },
            Command::ProjectFolders(answer) => {
                let _ = answer.send(known.projects.iter().flat_map(Projects::folders).collect());
                continue;
            },
            Command::Plain(answer) => {
                let _ = answer.send(known.projects.iter().flat_map(Projects::plain).collect());
                continue;
            },
            Command::Clones(answer) => {
                let clones = known.projects.iter().flat_map(Projects::clones);
                let _ = answer.send(clones.collect());
                continue;
            },
            Command::Branches(answer) => {
                let _ = answer.send(known.branches());
                continue;
            },
            Command::Tracking(answer) => {
                let _ = answer.send(known.tracking());
                continue;
            },
            Command::Claudes(answer) => {
                let claudes = known
                    .all_agents()
                    .into_iter()
                    .filter(|agent| agent.harness == Harness::ClaudeCode)
                    .collect();
                let _ = answer.send(claudes);
                continue;
            },
            Command::AllAgents(answer) => {
                let _ = answer.send(known.all_agents());
                continue;
            },
            Command::List(answer) => {
                let (unread, unread_files) = known.unread();
                let agents = known.agents();
                let _ = answer.send(api::ListAgentsResponse { agents, unread, unread_files });
                continue;
            },
            Command::Host(answer) => {
                let _ = answer.send(known.now());
                continue;
            },
            Command::Watch { after, answer } => {
                known.watch(after, answer);
                continue;
            },
        }
        known.update();
    }
}

fn wake(thread: &SyncSender<()>) {
    match thread.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

impl Known {
    fn named(&mut self, now: Named) {
        let unique: HashSet<&Destination> = now.lines.iter().map(Line::destination).collect();
        assert_eq!(unique.len(), now.lines.len(), "a machine is named twice");
        self.unread_hosts = now.unread;
        let before = mem::take(&mut self.remotes).into_iter().filter_map(|remote| match remote {
            Remote::Far(destination, reached) => Some((destination, reached)),
            Remote::Here(_) => None,
        });
        let mut before: HashMap<Destination, Reached> = before.collect();
        for line in now.lines {
            let remote = match line {
                Line::Far(destination) => {
                    let reached = before.remove(&destination).unwrap_or(Reached::Reaching);
                    Remote::Far(destination, reached)
                },
                Line::Here(destination) => Remote::Here(destination),
            };
            self.remotes.push(remote);
        }
    }

    fn watched_projects(&self) -> (BTreeSet<Dir>, BTreeSet<Dir>) {
        let clones =
            self.projects.iter().flat_map(Projects::clones).map(|(root, _)| root).collect();
        let plain = self.projects.iter().flat_map(Projects::plain).collect();
        (clones, plain)
    }

    fn dirs(&self) -> FollowDirs {
        let listed = self.terminals.values().filter_map(|found| match found {
            Located::Shown(Shown::Listed { cwd: Some(cwd), .. }) => Some(cwd.clone()),
            Located::Shown(Shown::Listed { cwd: None, .. } | Shown::Unlisted(_))
            | Located::Lost
            | Located::Looking => None,
        });
        let present = |folder: &Dir| !self.missing.contains(folder);
        let folders = self.projects.iter().flat_map(Projects::worktree_folders).filter(present);
        FollowDirs::new(listed.chain(folders).collect())
    }

    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.statuses.retain(|dir, _| dirs.contains(dir));
        self.pull_requests.retain(|dir, _| Dir::parse(dir).is_ok_and(|dir| dirs.contains(&dir)));
    }

    fn branch(&self, dir: &Dir) -> Option<&str> {
        match self.statuses.get(dir)? {
            GitStatus::Checkout { head: Head::Branch { name, .. }, .. } => Some(name.as_str()),
            GitStatus::Checkout { head: Head::Detached, .. }
            | GitStatus::NoCheckout { .. }
            | GitStatus::Outside => None,
        }
    }

    fn pull_request(&self, dir: &Dir) -> Option<&PullRequest> {
        let found = self.pull_requests.get(dir.as_str())?;
        let on_its_branch = self.branch(dir) == Some(found.branch.as_str());
        on_its_branch.then_some(&found.pull_request)
    }

    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &Dir| Some((dir.as_str().to_owned(), self.branch(dir)?.to_owned()));
        self.statuses.keys().filter_map(branch).collect()
    }

    fn tracking(&self) -> BTreeMap<String, Branch> {
        let tracking = |(dir, status): (&Dir, &GitStatus)| {
            let GitStatus::Checkout {
                repository: Some(repository),
                head: Head::Branch { name, upstream: Some(_), .. },
                ..
            } = status
            else {
                return None;
            };
            Some((
                dir.as_str().to_owned(),
                Branch { repository: repository.clone(), name: name.clone() },
            ))
        };
        self.statuses.iter().filter_map(tracking).collect()
    }

    fn readers(&self) -> impl Iterator<Item = &ReportedAgent> {
        standing(&self.found.agents, &self.pi, &self.agy, &self.contested)
    }

    fn settle_readers(&mut self) {
        self.contested = contested(self.found.agents.iter().chain(&self.pi).chain(&self.agy));
        for (agent, contest) in &self.contested {
            for harness in Harness::ALL {
                if !contest.won_by(harness) {
                    self.agent_statuses.forget(agent, harness);
                }
            }
        }
        for reads in [claude::READS, pi::READS, agy::READS] {
            let reports: Vec<Report> = self
                .readers()
                .map(ReportedAgent::report)
                .filter(|report| report.reads() == reads)
                .cloned()
                .collect();
            self.agent_statuses.replace_rung(reads.rung(), reports);
        }
        self.reconcile_osc_statuses();
    }

    fn reconcile_osc_statuses(&mut self) {
        let mut by_terminal: HashMap<(TerminalKey, Harness), Vec<&SeenAgent>> = HashMap::new();
        let mut known_pids: HashSet<AgentPid> = HashSet::new();
        for reported in standing(&self.found.agents, &self.pi, &self.agy, &self.contested) {
            let seen = reported.seen();
            known_pids.insert(seen.id.pid());
            if let Some(terminal) = seen.terminal {
                by_terminal.entry((terminal, seen.harness)).or_default().push(seen);
            }
        }
        let mut osc_reports = Vec::new();
        let mut osc_agents = Vec::new();
        let mut active_processes = HashSet::new();
        for (&key, terminal) in &self.mux_terminals {
            assert!(terminal.subagents.len() <= lune_protocol::PROGRAM_STATUS_RECORDS_MAX);
            if let Some(candidates) = by_terminal.get(&(key, terminal.reads.harness())) {
                let matched = terminal
                    .front
                    .as_ref()
                    .and_then(|front| {
                        candidates.iter().copied().find(|seen| seen.id.pid() == front.pid)
                    })
                    .or_else(|| {
                        candidates
                            .iter()
                            .copied()
                            .min_by_key(|seen| (!seen.at_a_prompt(), seen.started_at, seen.id.pid()))
                    })
                    .expect("terminal candidates entry is non-empty");
                let in_foreground = match &terminal.front {
                    None => true,
                    Some(front) if front.pid == matched.id.pid() => true,
                    Some(front) if front.pid == terminal.root => false,
                    Some(front) => !known_pids.contains(&front.pid),
                };
                if in_foreground {
                    osc_reports.push(osc_report(matched.id.clone(), terminal));
                }
                continue;
            }
            if terminal.shell_idle {
                continue;
            }
            let Some(front) = &terminal.front else {
                continue;
            };
            if !argv_matches_harness(&front.argv, terminal.reads.harness()) {
                continue;
            }
            let pid = front.pid;
            if known_pids.contains(&pid) {
                continue;
            }
            let Some((id, started_at, cwd)) = resolve_osc_process(
                &mut self.osc_processes,
                &mut self.osc_troubles,
                key,
                terminal,
                pid,
            ) else {
                continue;
            };
            if let (Some(recorded_at), Some(started_at)) = (terminal.at, started_at)
                && !osc_dated_since(recorded_at, started_at)
            {
                continue;
            }
            known_pids.insert(pid);
            active_processes.insert((key, pid));
            let entrypoint = terminal
                .reads
                .harness()
                .to_api()
                .prompt_word()
                .and_then(|word| Said::new(word.to_owned()));
            osc_agents.push(SeenAgent {
                id: id.clone(),
                harness: terminal.reads.harness(),
                session_id: None,
                cwd,
                started_at,
                name: None,
                entrypoint,
                terminal: Some(key),
            });
            osc_reports.push(osc_report(id, terminal));
        }
        self.osc_processes.retain(|key, _| active_processes.contains(key));
        self.osc_troubles.done();
        assert!(osc_agents.len() <= self.mux_terminals.len());
        assert!(osc_reports.len() <= self.mux_terminals.len());
        assert!(self.osc_processes.len() <= self.mux_terminals.len());
        self.osc_agents = osc_agents;
        self.agent_statuses.replace_rung(Rung::Osc7501, osc_reports);
        let live: HashSet<AgentId> = self
            .readers()
            .map(|reported| reported.seen().id.clone())
            .chain(self.osc_agents.iter().map(|seen| seen.id.clone()))
            .collect();
        self.agent_statuses.retain_live(&live);
    }

    fn all_agents(&self) -> Vec<Agent> {
        self.readers()
            .map(ReportedAgent::seen)
            .chain(&self.osc_agents)
            .map(|seen| self.agent_statuses.apply(seen))
            .collect()
    }

    fn agents(&self) -> Vec<api::Agent> {
        let mut agents: Vec<api::Agent> = self
            .all_agents()
            .into_iter()
            .map(|agent| {
                let topic = (agent.harness == Harness::ClaudeCode)
                    .then(|| {
                        agent
                            .session_id
                            .as_ref()
                            .and_then(|id| self.topics.get(id.as_str()))
                            .cloned()
                    })
                    .flatten();
                agent.to_api(topic)
            })
            .collect();
        bound_fair(
            &mut agents,
            |agent| Some(&mut agent.subagents),
            |subagent| subagent.encoded_len().saturating_add(16),
            HOST_SUBAGENTS_BYTES_MAX,
        );
        agents
    }

    fn unread(&self) -> (u32, Vec<api::UnreadFile>) {
        let mut files: Vec<api::UnreadFile> =
            self.found.unread.iter().map(Unread::to_api).collect();
        files.sort_by_key(|file| file.pid);
        let count =
            u32::try_from(files.len()).expect("a folder listed in memory has under 2^32 files");
        (count, files)
    }

    fn built(&self) -> api::Host {
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        let (unread, unread_files) = self.unread();
        let pull_requests = self
            .pull_requests
            .keys()
            .filter_map(|dir| {
                let parsed = Dir::parse(dir).ok()?;
                Some(self.pull_request(&parsed)?.to_api(dir))
            })
            .collect();
        let remotes = self.remotes.iter().map(Remote::to_api).collect();
        api::Host {
            version: None,
            git_statuses: self
                .statuses
                .iter()
                .map(|(dir, status)| status.to_api(dir.as_str()))
                .collect(),
            agents,
            unread,
            pull_requests,
            unread_files,
            remotes,
            unread_hosts: Some(self.unread_hosts),
            sayings: self.sayings.fresh(SystemTime::now()),
            projects: Some(self.built_projects()),
        }
    }

    fn built_projects(&self) -> api::Projects {
        let projects = match &self.projects {
            Ok(projects) => projects,
            Err(why) => {
                let unkept = api::projects::Held::Unkept(why.clone());
                return api::Projects { held: Some(unkept) };
            },
        };
        let worktree = |folder: Dir, given: &str, worktree: &Worktree| {
            let renamed = worktree.renamed().map(Name::as_str);
            api::Worktree {
                id: worktree.id().get(),
                name: renamed.unwrap_or(given).to_owned(),
                renamed: renamed.is_some(),
                seen: Some(self.seen(&folder).to_api()),
                folder: folder.as_str().to_owned(),
                terminals: self.built_terminals(worktree),
                used_at: worktree.used().map(|used| used.time().into()),
                arranged: worktree.layout().arranged().map(|it| it.to_api().into()),
            }
        };
        let holds = |project: &Project| match project.holds() {
            Holds::Folder(own) => {
                let dir = project.dir();
                let given = self.branch(dir).unwrap_or_else(|| dir.last());
                api::project::Holds::Folder(worktree(dir.clone(), given, own))
            },
            Holds::Clone(rows) => {
                let listing = Some(self.listing(project.dir(), rows));
                let rows = rows.iter().map(|(subfolder, it)| {
                    let folder = project.dir().at(subfolder);
                    let given = match self.branch(&folder) {
                        Some(branch) if subfolder.named_for(branch) => branch,
                        Some(_) | None => subfolder.as_str(),
                    };
                    worktree(folder, given, it)
                });
                let clone = api::project::Clone { worktrees: rows.collect(), listing };
                api::project::Holds::Clone(clone)
            },
        };
        let project = |project: &Project| api::Project {
            id: project.id().get(),
            name: project.name().as_str().to_owned(),
            dir: project.dir().as_str().to_owned(),
            missing: self.missing.contains(project.dir()),
            holds: Some(holds(project)),
        };
        let projects = projects.all().iter().map(project).collect();
        let kept = api::projects::Kept { projects, version: Some(self.revision.to_api()) };
        api::Projects { held: Some(api::projects::Held::Kept(kept)) }
    }

    fn listing(&self, dir: &Dir, rows: &[(Subfolder, Worktree)]) -> api::project::clone::Listing {
        use api::project::clone::Listing as Said;
        let listed = match self.listed.get(dir) {
            None => return Said::Unlisted("not listed yet".to_owned()),
            Some(Listing::Unlisted(why)) => return Said::Unlisted(why.clone()),
            Some(Listing::Listed(listed)) => listed,
        };
        let opened: BTreeSet<&Subfolder> = rows.iter().map(|(subfolder, _)| subfolder).collect();
        let unopened = listed.subfolders().iter().filter(|it| !opened.contains(it));
        let subfolders: Vec<String> = unopened.map(|it| it.as_str().to_owned()).collect();
        Said::Unopened(api::project::Unopened { subfolders })
    }

    fn built_terminals(&self, worktree: &Worktree) -> Vec<api::Terminal> {
        use api::terminal::{Ended, Found as Told, Running, State};
        let built = worktree.layout().terminals().map(|terminal| {
            let id = terminal.id().view();
            let found = self.terminals.get(&id).unwrap_or(&Located::Looking);
            let (told, state, folder, command, titled, shell, held) = match found {
                Located::Shown(Shown::Listed { key, cwd, titled, program }) => {
                    let api_key: api::TerminalKey = (*key).into();
                    let folder = cwd.as_ref().unwrap_or_else(|| terminal.folder());
                    let titled = titled.as_ref().map(Titled::to_api);
                    match program {
                        Program::Running { command, doing } => {
                            let command = command.as_ref().map(|it| it.as_str().to_owned());
                            let shell = doing.as_ref().map(Doing::to_api);
                            let state = State::Running(Running {
                                key: Some(api_key),
                                command: command.clone(),
                                titled: titled.clone(),
                                shell: shell.clone(),
                            });
                            (Told::Key(api_key), state, folder, command, titled, shell, false)
                        },
                        Program::Ended => {
                            let state =
                                State::Ended(Ended { key: Some(api_key), titled: titled.clone() });
                            (Told::Key(api_key), state, folder, None, titled, None, true)
                        },
                    }
                },
                Located::Shown(Shown::Unlisted(key)) => {
                    let api_key: api::TerminalKey = (*key).into();
                    let state = State::Unlisted(api_key);
                    (Told::Key(api_key), state, terminal.folder(), None, None, None, false)
                },
                Located::Lost => (
                    Told::Lost(()),
                    State::StateLost(()),
                    terminal.folder(),
                    None,
                    None,
                    None,
                    false,
                ),
                Located::Looking => (
                    Told::Looking(()),
                    State::StateLooking(()),
                    terminal.folder(),
                    None,
                    None,
                    None,
                    false,
                ),
            };
            api::Terminal {
                id: id.get(),
                found: Some(told),
                folder: folder.as_str().to_owned(),
                command,
                titled,
                shell,
                held,
                name: terminal.name().map(|name| name.as_str().to_owned()),
                state: Some(state),
            }
        });
        built.collect()
    }

    fn seen(&self, folder: &Dir) -> Seen {
        if self.missing.contains(folder) {
            return Seen::Missing;
        }
        if !self.statuses.contains_key(folder) {
            return Seen::Unread;
        }
        Seen::Read(Stage::of(self.branch(folder), self.pull_request(folder)))
    }

    fn version(&self) -> api::Version {
        api::Version { pid: std::process::id(), seq: self.seq }
    }

    fn update(&mut self) {
        self.forget_dirs();
        let mut host = self.built();
        host.version = self.host.version;
        if host == self.host {
            return;
        }
        self.seq = self.seq.checked_add(1).expect("a host changes fewer than 2^64 times");
        host.version = Some(self.version());
        self.host = host;
        for waiting in self.waiting.drain(..) {
            let _ = waiting.send(Ok(self.host.clone()));
        }
    }

    fn now(&mut self) -> api::Host {
        self.update();
        assert_eq!(self.host.version, Some(self.version()));
        self.host.clone()
    }

    fn watch(&mut self, after: After, answer: oneshot::Sender<Result<api::Host, Full>>) {
        self.update();
        let version = self.version();
        assert_eq!(self.host.version, Some(version));
        let After { version: asked, parked } = after;
        if asked != version {
            let _ = answer.send(Ok(self.host.clone()));
            return;
        }
        self.waiting.retain(|waiting| !waiting.is_closed());
        assert!(self.waiting.len() <= WAITING_MAX);
        if answer.is_closed() {
            return;
        }
        if self.waiting.len() == WAITING_MAX {
            let _ = answer.send(Err(Full));
            return;
        }
        self.waiting.push(answer);
        assert!(self.waiting.len() <= WAITING_MAX);
        match parked {
            Parked::Quietly => {},
            Parked::Aloud(tell) => {
                let _ = tell.send(version);
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Contest {
    Won(Harness),
    Unproven,
}

impl Contest {
    fn won_by(self, harness: Harness) -> bool {
        match self {
            Contest::Won(winner) => winner == harness,
            Contest::Unproven => false,
        }
    }
}

fn standing<'a>(
    claude: &'a [ReportedAgent],
    pi: &'a [ReportedAgent],
    agy: &'a [ReportedAgent],
    contested: &'a HashMap<AgentId, Contest>,
) -> impl Iterator<Item = &'a ReportedAgent> {
    let stands = |reported: &&ReportedAgent| {
        let seen = reported.seen();
        contested.get(&seen.id).is_none_or(|contest| contest.won_by(seen.harness))
    };
    claude.iter().chain(pi).chain(agy).filter(stands)
}

fn contested<'a>(reported: impl Iterator<Item = &'a ReportedAgent>) -> HashMap<AgentId, Contest> {
    let mut claims: HashMap<&AgentId, Vec<(Harness, Proof)>> = HashMap::new();
    for reported in reported {
        let seen = reported.seen();
        claims.entry(&seen.id).or_default().push((seen.harness, reported.proof()));
    }
    let mut contested = HashMap::new();
    for (agent, claims) in claims {
        assert!(claims.len() <= Harness::ALL.len(), "each harness's reader claims an agent once");
        if claims.len() == 1 {
            continue;
        }
        let harnesses: HashSet<Harness> = claims.iter().map(|(harness, _)| *harness).collect();
        assert_eq!(harnesses.len(), claims.len(), "each harness's reader claims an agent once");
        contested.insert(agent.clone(), contest_of(&claims));
    }
    contested
}

fn contest_of(claims: &[(Harness, Proof)]) -> Contest {
    let mut started: Vec<(SystemTime, Harness)> = claims
        .iter()
        .filter_map(|(harness, proof)| match proof {
            Proof::Started { written } => Some((*written, *harness)),
            Proof::Dated => None,
        })
        .collect();
    started.sort_unstable_by_key(|(written, _)| std::cmp::Reverse(*written));
    match started.as_slice() {
        [(_, harness)] => Contest::Won(*harness),
        [(last, harness), (before, _), ..] if last > before => Contest::Won(*harness),
        [] | [_, _, ..] => Contest::Unproven,
    }
}

fn osc_report(agent: AgentId, terminal: &MuxTerminal) -> Report {
    let (status, subagents) = (terminal.status.clone(), terminal.subagents.clone());
    Report::osc_7501(terminal.reads, agent, status, terminal.at, subagents)
}

const OSC_DATED_LATE: Duration = Duration::from_secs(1);

const RUNNERS: [&str; 12] = [
    "sh", "bash", "dash", "zsh", "fish", "env", "node", "bun", "deno", "npx", "bunx", "tsx",
];

fn osc_dated_since(recorded_at: SystemTime, started_at: SystemTime) -> bool {
    recorded_at.checked_add(OSC_DATED_LATE).is_none_or(|late| started_at <= late)
}

fn argv_matches_harness(argv: &[String], harness: Harness) -> bool {
    let mut rest = argv;
    loop {
        let Some((first, tail)) = rest.split_first() else {
            return false;
        };
        if arg_matches_harness(first, harness) {
            return true;
        }
        let Some(runner) = basename_lower(first) else {
            return false;
        };
        if !RUNNERS.contains(&runner.as_str()) {
            return false;
        }
        let Some(next) = tail.iter().position(|word| {
            let word = word.as_str();
            !(word.is_empty()
                || word.starts_with('-')
                || (runner == "env" && word.contains('=')))
        }) else {
            return false;
        };
        rest = &tail[next..];
    }
}

fn basename_lower(arg: &str) -> Option<String> {
    let base = arg.rsplit('/').next()?;
    let trimmed = base.trim_start_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn arg_matches_harness(arg: &str, harness: Harness) -> bool {
    let names: &[&str] = match harness {
        Harness::ClaudeCode => &["claude", "claude-code"],
        Harness::Pi => &["pi", "pi-coding-agent"],
        Harness::Antigravity => &["agy", "antigravity", "antigravity-cli"],
    };
    arg.split('/').any(|segment| {
        let segment = segment.trim_start_matches('-').trim_start_matches('.');
        let lower = segment.to_ascii_lowercase();
        names.iter().any(|name| {
            lower == *name
                || lower
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('.'))
        })
    })
}

fn resolve_osc_process(
    cache: &mut HashMap<(TerminalKey, AgentPid), CachedProcess>,
    troubles: &mut Troubles,
    key: TerminalKey,
    terminal: &MuxTerminal,
    pid: AgentPid,
) -> Option<(AgentId, Option<SystemTime>, Directory)> {
    let parsed_cwd = terminal.cwd.clone();
    if let Some(cached) = cache.get(&(key, pid)) {
        let cwd = parsed_cwd.or_else(|| cached.cwd.clone())?;
        return Some((cached.id.clone(), cached.started_at, cwd));
    }
    let start = process::start(pid.get())?;
    let id = AgentId::new(pid, start);
    let started_at = process::started(pid.get());
    let process_cwd = if parsed_cwd.is_some() {
        None
    } else {
        process_cwd_or_trouble(pid, troubles)
    };
    let cwd = parsed_cwd.or_else(|| process_cwd.clone())?;
    cache.insert((key, pid), CachedProcess { id: id.clone(), started_at, cwd: process_cwd });
    Some((id, started_at, cwd))
}

fn process_cwd_or_trouble(pid: AgentPid, troubles: &mut Troubles) -> Option<Directory> {
    match process::cwd(pid.get()) {
        Ok(path) => Directory::parse(path.to_str()?.to_owned()).ok(),
        Err(process::Unread::Gone) => None,
        Err(process::Unread::Failed(why)) => {
            troubles.say(format!("agent {pid}'s cwd is unread: {why}"));
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, UNIX_EPOCH};

    use lune_domain::agent::Declared;
    use lune_domain::mux::OwnMade;
    use lune_domain::projects::{Change, Id, Made, Opens, Spot, Subfolder};
    use lune_protocol::agent_status::{AgentStatus, BlockedReason};
    use lune_protocol::program_status::{
        AppName, ChildId, ChildProgramStatus, Message, OscBlockedKind, ProgramPhase,
        ProgramStatus, ProgramStatuses, Progress, Title,
    };

    use super::*;
    use crate::agent::Sighting;
    use crate::statuses::{ActiveStatus, RungUpdate};

    fn key(id: u32) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(10) }
    }

    fn mux_terminals(
        terminals: impl IntoIterator<Item = (TerminalKey, server::Terminal)>,
    ) -> BTreeMap<TerminalKey, MuxTerminal> {
        terminals
            .into_iter()
            .filter_map(|(key, terminal)| Some((key, MuxTerminal::of(&terminal)?)))
            .collect()
    }

    fn dir(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    #[test]
    fn osc_7501_overrides_lower_rung_and_yields_when_agent_leaves_foreground() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let pid = AgentPid::new(std::process::id()).unwrap();
        let start_id = process::start(pid.get()).unwrap();
        let id = AgentId::new(pid, start_id);
        let terminal = key(7);
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);

        let sighting = Sighting {
            session_id: Said::new("s1".to_owned()),
            cwd: Directory::parse("/w".to_owned()).unwrap(),
            started_at: Some(at(1)),
            name: None,
            entrypoint: Said::new("cli".to_owned()),
            terminal: Some(terminal),
        };
        let feed = RungUpdate::Active { status: ActiveStatus::Working, at: Some(at(10)) };
        let proof = Proof::Started { written: at(10) };
        let reported = ReportedAgent::new(claude::READS, id, sighting, feed, proof);
        core.send(Command::Agents(Scan {
            agents: vec![reported],
            unread: Vec::new(),
        }))
        .unwrap();

        let listed = || ask(&core, Command::List).unwrap().agents;
        let initial = listed();
        assert_eq!(initial.len(), 1);
        assert_eq!(AgentStatus::of_api(&initial[0]), AgentStatus::Working);
        assert_eq!(initial[0].status_rung, Some(api::StatusRung::Feed.into()));

        let app = AppName::parse("claude-code").unwrap();
        let root_status = ProgramStatus {
            phase: ProgramPhase::Blocked {
                kind: Some(OscBlockedKind::Permission),
                message: Some(Message::parse("message", "approve touch").unwrap()),
            },
            app: Some(app.clone()),
            title: None,
            at: Some(at(20)),
        };
        let child_status = ChildProgramStatus {
            id: ChildId::parse("sub-1").unwrap(),
            status: ProgramStatus {
                phase: ProgramPhase::Working { progress: Progress::new(50) },
                app: Some(app.clone()),
                title: Some(Title::parse("title", "Explore").unwrap()),
                at: Some(at(21)),
            },
        };
        let mux_terminal = |front_pid: AgentPid, statuses: ProgramStatuses| {
            (
                terminal,
                server::Terminal {
                    tag: Vec::new(),
                    size: None,
                    cwd: Some(dir("/w")),
                    title: String::new(),
                    tty: None,
                    is: Is::Running {
                        root: AgentPid::new(100).unwrap(),
                        front: Some(server::Foreground {
                            pid: front_pid,
                            argv: vec!["claude".to_owned()],
                        }),
                        doing: None,
                        resumes: None,
                    },
                    statuses,
                },
            )
        };

        let statuses =
            ProgramStatuses { root: Some(root_status), children: vec![child_status] };
        core.send(Command::MuxTerminals(mux_terminals([mux_terminal(pid, statuses.clone())])))
            .unwrap();
        let after = listed();
        assert_eq!(after.len(), 1);
        assert_eq!(
            AgentStatus::of_api(&after[0]),
            AgentStatus::Blocked {
                reason: BlockedReason::Permission,
                message: Message::parse("message", "approve touch").ok(),
            }
        );
        assert_eq!(after[0].status_rung, Some(api::StatusRung::Osc7501.into()));
        assert_eq!(after[0].subagents.len(), 1);
        assert_eq!(after[0].subagents[0].id, "sub-1");
        assert_eq!(after[0].subagents[0].title.as_deref(), Some("Explore"));
        assert_eq!(
            after[0].subagents[0].phase,
            Some(api::subagent::Phase::Working(api::WorkingStatus { progress: Some(50) }))
        );

        let stopped = ProgramStatuses {
            root: Some(ProgramStatus {
                phase: ProgramPhase::Idle,
                app: Some(app),
                title: None,
                at: Some(at(22)),
            }),
            children: Vec::new(),
        };
        core.send(Command::MuxTerminals(mux_terminals([mux_terminal(pid, stopped)]))).unwrap();
        let interrupted = listed();
        assert_eq!(AgentStatus::of_api(&interrupted[0]), AgentStatus::Stopped);
        assert!(!interrupted[0].is_finished_turn());

        core.send(Command::MuxTerminals(mux_terminals([mux_terminal(
            AgentPid::new(100).unwrap(),
            statuses,
        )])))
        .unwrap();
        let fallback = listed();
        assert_eq!(fallback.len(), 1);
        assert_eq!(AgentStatus::of_api(&fallback[0]), AgentStatus::Working);
        assert_eq!(fallback[0].status_rung, Some(api::StatusRung::Feed.into()));
        assert_eq!(fallback[0].subagents, []);
    }

    #[test]
    fn a_line_naming_this_machine_takes_no_word_of_a_watch_until_it_names_another() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let machine = |name: &str| name.parse::<Destination>().unwrap();
        let hosts = |lines| Command::Hosts(Named { lines, unread: 0 });
        let heard = |name: &str| Command::Remote {
            destination: machine(name),
            reached: Reached::Host(api::remote::Host::default()),
        };
        let told = || {
            let remotes = ask(&core, Command::Host).unwrap().remotes.into_iter();
            remotes.map(|remote| (remote.destination, remote.reached.unwrap())).collect::<Vec<_>>()
        };
        let said = |name: &str, reached| (name.to_owned(), reached);
        let host = || api::remote::Reached::Host(api::remote::Host::default());
        let (here, reaching) = (api::remote::Reached::Here(()), api::remote::Reached::Reaching(()));

        core.send(hosts(vec![Line::Here(machine("nixos")), Line::Far(machine("vps"))])).unwrap();
        assert_eq!(told(), [said("nixos", here.clone()), said("vps", reaching.clone())]);
        core.send(heard("nixos")).unwrap();
        core.send(heard("vps")).unwrap();
        assert_eq!(told(), [said("nixos", here), said("vps", host())]);

        core.send(hosts(vec![Line::Far(machine("nixos")), Line::Far(machine("vps"))])).unwrap();
        assert_eq!(told(), [said("nixos", reaching), said("vps", host())]);
    }

    #[test]
    fn osc_7501_rejects_unrelated_foreground_stopped_and_stale_unmatched_records() {
        let words = |items: &[&str]| items.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(argv_matches_harness(&words(&["claude"]), Harness::ClaudeCode));
        assert!(argv_matches_harness(
            &words(&["/usr/bin/env", "FOO=1", "node", "/opt/claude-code/cli.js"]),
            Harness::ClaudeCode,
        ));
        assert!(argv_matches_harness(&words(&["/bin/sh", "/tmp/claude.sh"]), Harness::ClaudeCode));
        assert!(argv_matches_harness(&words(&["pi", "--continue"]), Harness::Pi));
        assert!(argv_matches_harness(&words(&["agy"]), Harness::Antigravity));
        assert!(!argv_matches_harness(&words(&["vim", "README.md"]), Harness::ClaudeCode));
        assert!(!argv_matches_harness(&words(&["cat", "claude.log"]), Harness::ClaudeCode));
        assert!(!argv_matches_harness(&words(&["zsh"]), Harness::ClaudeCode));
        assert!(!argv_matches_harness(&words(&["/bin/sh", "-c", "echo hi"]), Harness::ClaudeCode));
        assert!(!argv_matches_harness(&[], Harness::ClaudeCode));

        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert!(osc_dated_since(at(10), at(10)));
        assert!(osc_dated_since(at(10), at(11)));
        assert!(!osc_dated_since(at(10), at(12)));

        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let pid = AgentPid::new(std::process::id()).unwrap();
        let root_pid = AgentPid::new(if pid.get() == 100 { 101 } else { 100 }).unwrap();
        let app = AppName::parse("claude-code").unwrap();
        let make_terminal = |argv: &[&str], doing: Option<Doing>, recorded_at: SystemTime| {
            (
                key(9),
                server::Terminal {
                    tag: Vec::new(),
                    size: None,
                    cwd: Some(dir("/w")),
                    title: String::new(),
                    tty: None,
                    is: Is::Running {
                        root: root_pid,
                        front: Some(server::Foreground { pid, argv: words(argv) }),
                        doing,
                        resumes: None,
                    },
                    statuses: ProgramStatuses {
                        root: Some(ProgramStatus {
                            phase: ProgramPhase::Working { progress: None },
                            app: Some(app.clone()),
                            title: None,
                            at: Some(recorded_at),
                        }),
                        children: Vec::new(),
                    },
                },
            )
        };
        let listed = || ask(&core, Command::List).unwrap().agents;

        core.send(Command::MuxTerminals(mux_terminals([make_terminal(
            &["vim", "notes.txt"],
            None,
            SystemTime::now(),
        )])))
        .unwrap();
        assert_eq!(listed(), []);

        core.send(Command::MuxTerminals(mux_terminals([make_terminal(
            &["claude"],
            Some(Doing::Stopped { command: None, at: SystemTime::now() }),
            SystemTime::now(),
        )])))
        .unwrap();
        assert_eq!(listed(), []);

        core.send(Command::MuxTerminals(mux_terminals([make_terminal(&["claude"], None, at(1))])))
            .unwrap();
        assert_eq!(listed(), []);

        core.send(Command::MuxTerminals(mux_terminals([make_terminal(
            &["claude"],
            None,
            SystemTime::now(),
        )])))
        .unwrap();
        let active = listed();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].status_rung, Some(api::StatusRung::Osc7501.into()));
    }

    #[test]
    fn a_process_that_execs_into_another_harness_is_that_harness() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let pid = AgentPid::new(std::process::id()).unwrap();
        let root_pid = AgentPid::new(if pid.get() == 100 { 101 } else { 100 }).unwrap();
        let running = |program: &str, app: &str| {
            let terminal = server::Terminal {
                tag: Vec::new(),
                size: None,
                cwd: Some(Directory::parse("/w".to_owned()).unwrap()),
                title: String::new(),
                tty: None,
                is: Is::Running {
                    root: root_pid,
                    front: Some(server::Foreground { pid, argv: vec![program.to_owned()] }),
                    doing: None,
                    resumes: None,
                },
                statuses: ProgramStatuses {
                    root: Some(ProgramStatus {
                        phase: ProgramPhase::Working { progress: None },
                        app: Some(AppName::parse(app).unwrap()),
                        title: None,
                        at: Some(SystemTime::now()),
                    }),
                    children: Vec::new(),
                },
            };
            Command::MuxTerminals(mux_terminals([(key(11), terminal)]))
        };
        let listed = || ask(&core, Command::List).unwrap().agents;

        core.send(running("pi", "pi")).unwrap();
        let before = listed();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].harness, Some(api::Harness::Pi.into()));

        core.send(running("claude", "claude-code")).unwrap();
        let after = listed();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].harness, Some(api::Harness::ClaudeCode.into()));
        assert_eq!(after[0].status_rung, Some(api::StatusRung::Osc7501.into()));
    }

    #[test]
    fn readers_that_claim_one_process_settle_on_the_claim_its_start_proves() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let start_id = process::start(std::process::id()).unwrap();
        let id = AgentId::new(AgentPid::new(301).unwrap(), start_id);
        let claim = |reads: Declared, proof: Proof| {
            let sighting = Sighting {
                session_id: Said::new("s-301".to_owned()),
                cwd: Directory::parse("/w".to_owned()).unwrap(),
                started_at: None,
                name: None,
                entrypoint: Said::new("cli".to_owned()),
                terminal: None,
            };
            let working = RungUpdate::Active { status: ActiveStatus::Working, at: None };
            ReportedAgent::new(reads, id.clone(), sighting, working, proof)
        };
        let listed = || ask(&core, Command::List).unwrap().agents;
        let claude_said = |agents| Command::Agents(Scan { agents, unread: Vec::new() });

        core.send(Command::AgyAgents(vec![claim(agy::READS, Proof::Dated)])).unwrap();
        assert_eq!(listed().len(), 1);
        core.send(Command::PiAgents(vec![claim(pi::READS, Proof::Dated)])).unwrap();
        assert_eq!(listed().len(), 0, "two dated claims prove neither");

        let written = |secs| Proof::Started { written: UNIX_EPOCH + Duration::from_secs(secs) };
        core.send(claude_said(vec![claim(claude::READS, written(5))])).unwrap();
        let settled = listed();
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].harness, Some(api::Harness::ClaudeCode.into()));
        assert_eq!(settled[0].status_rung, Some(api::StatusRung::Feed.into()));

        core.send(Command::PiAgents(vec![claim(pi::READS, written(7))])).unwrap();
        let execed = listed();
        assert_eq!(execed.len(), 1, "of two started claims, the one written last stands");
        assert_eq!(execed[0].harness, Some(api::Harness::Pi.into()));
        assert_eq!(execed[0].status_rung, Some(api::StatusRung::Extension.into()));

        core.send(Command::PiAgents(vec![claim(pi::READS, written(5))])).unwrap();
        assert_eq!(listed().len(), 0, "two started claims written at once prove neither");

        core.send(Command::PiAgents(vec![claim(pi::READS, Proof::Dated)])).unwrap();
        assert_eq!(listed()[0].harness, Some(api::Harness::ClaudeCode.into()));

        core.send(claude_said(Vec::new())).unwrap();
        assert_eq!(listed().len(), 0, "two dated claims still prove neither");
        core.send(Command::PiAgents(Vec::new())).unwrap();
        let left = listed();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].harness, Some(api::Harness::Antigravity.into()));
        assert_eq!(left[0].status_rung, Some(api::StatusRung::Hook.into()));
    }

    #[test]
    fn osc_7501_attributes_deterministically_when_multiple_agents_share_a_terminal() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        let terminal = key(11);
        let start_id = process::start(std::process::id()).unwrap();

        let sdk_pid = AgentPid::new(201).unwrap();
        let cli_pid = AgentPid::new(202).unwrap();
        let pi_pid = AgentPid::new(203).unwrap();
        let sdk_agent = AgentId::new(sdk_pid, start_id.clone());
        let cli_agent = AgentId::new(cli_pid, start_id.clone());
        let pi_agent = AgentId::new(pi_pid, start_id);

        let reported = |id: &AgentId, harness: Harness, entrypoint: &str, started: u64| {
            let reads = match harness {
                Harness::ClaudeCode => claude::READS,
                Harness::Pi => pi::READS,
                Harness::Antigravity => agy::READS,
            };
            let sighting = Sighting {
                session_id: Said::new(format!("s-{}", id.pid())),
                cwd: Directory::parse("/w".to_owned()).unwrap(),
                started_at: Some(at(started)),
                name: None,
                entrypoint: Said::new(entrypoint.to_owned()),
                terminal: Some(terminal),
            };
            let working = RungUpdate::Active { status: ActiveStatus::Working, at: Some(at(10)) };
            let proof = Proof::Started { written: at(started) };
            ReportedAgent::new(reads, id.clone(), sighting, working, proof)
        };

        core.send(Command::Agents(Scan {
            agents: vec![
                reported(&sdk_agent, Harness::ClaudeCode, "sdk-cli", 1),
                reported(&cli_agent, Harness::ClaudeCode, "cli", 2),
            ],
            unread: Vec::new(),
        }))
        .unwrap();
        core.send(Command::PiAgents(vec![reported(&pi_agent, Harness::Pi, "pi", 3)])).unwrap();

        let osc_terminal = |front: Option<AgentPid>| {
            (
                terminal,
                server::Terminal {
                    tag: Vec::new(),
                    size: None,
                    cwd: Some(dir("/w")),
                    title: String::new(),
                    tty: None,
                    is: Is::Running {
                        root: AgentPid::new(100).unwrap(),
                        front: front
                            .map(|pid| server::Foreground { pid, argv: vec!["claude".to_owned()] }),
                        doing: None,
                        resumes: None,
                    },
                    statuses: ProgramStatuses {
                        root: Some(ProgramStatus {
                            phase: ProgramPhase::Blocked {
                                kind: Some(OscBlockedKind::Permission),
                                message: None,
                            },
                            app: Some(AppName::parse("claude-code").unwrap()),
                            title: None,
                            at: Some(at(20)),
                        }),
                        children: Vec::new(),
                    },
                },
            )
        };

        let osc_pids = || {
            ask(&core, Command::List)
                .unwrap()
                .agents
                .into_iter()
                .filter(|agent| agent.status_rung == Some(api::StatusRung::Osc7501.into()))
                .map(|agent| agent.pid)
                .collect::<Vec<_>>()
        };

        core.send(Command::MuxTerminals(mux_terminals([osc_terminal(None)]))).unwrap();
        assert_eq!(osc_pids(), [cli_pid.get()]);

        core.send(Command::MuxTerminals(mux_terminals([osc_terminal(Some(sdk_pid))]))).unwrap();
        assert_eq!(osc_pids(), [sdk_pid.get()]);

        core.send(Command::MuxTerminals(mux_terminals([osc_terminal(Some(pi_pid))]))).unwrap();
        assert_eq!(osc_pids(), [] as [u32; 0]);
    }

    #[test]
    fn known_agents_bounds_total_subagent_bytes() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let at = UNIX_EPOCH + Duration::from_secs(10);
        let app = AppName::parse("claude-code").unwrap();
        let title = Title::parse("title", &"t".repeat(lune_protocol::program_status::TITLE_BYTES_MAX)).unwrap();
        let message = Message::parse("message", &"m".repeat(lune_protocol::program_status::MESSAGE_BYTES_MAX)).unwrap();

        let agent_count = 20u32;
        let start_id = process::start(std::process::id()).unwrap();
        let mut agents = Vec::new();
        let mut terminals = Vec::new();
        for index in 1..=agent_count {
            let pid = AgentPid::new(1000 + index).unwrap();
            let id = AgentId::new(pid, start_id.clone());
            let term = key(index);
            let sighting = Sighting {
                session_id: Said::new(format!("s-{index}")),
                cwd: Directory::parse("/w".to_owned()).unwrap(),
                started_at: Some(at),
                name: None,
                entrypoint: Said::new("cli".to_owned()),
                terminal: Some(term),
            };
            let working = RungUpdate::Active { status: ActiveStatus::Working, at: Some(at) };
            let proof = Proof::Started { written: at };
            agents.push(ReportedAgent::new(claude::READS, id, sighting, working, proof));
            let children = (0..lune_protocol::PROGRAM_STATUS_RECORDS_MAX)
                .map(|child| ChildProgramStatus {
                    id: ChildId::parse(&format!(
                        "{}-{child:03}",
                        "s".repeat(lune_protocol::program_status::ID_BYTES_MAX - 4)
                    ))
                    .unwrap(),
                    status: ProgramStatus {
                        phase: ProgramPhase::Done { message: Some(message.clone()) },
                        app: Some(app.clone()),
                        title: Some(title.clone()),
                        at: Some(at),
                    },
                })
                .collect();
            terminals.push((
                term,
                server::Terminal {
                    tag: Vec::new(),
                    size: None,
                    cwd: Some(dir("/w")),
                    title: String::new(),
                    tty: None,
                    is: Is::Running {
                        root: AgentPid::new(100).unwrap(),
                        front: Some(server::Foreground { pid, argv: vec!["claude".to_owned()] }),
                        doing: None,
                        resumes: None,
                    },
                    statuses: ProgramStatuses {
                        root: Some(ProgramStatus {
                            phase: ProgramPhase::Working { progress: None },
                            app: Some(app.clone()),
                            title: None,
                            at: Some(at),
                        }),
                        children,
                    },
                },
            ));
        }
        core.send(Command::Agents(Scan { agents, unread: Vec::new() })).unwrap();
        core.send(Command::MuxTerminals(mux_terminals(terminals))).unwrap();

        let listed = ask(&core, Command::List).unwrap().agents;
        assert_eq!(listed.len(), usize::try_from(agent_count).unwrap());
        let total_bytes: usize = listed
            .iter()
            .flat_map(|agent| &agent.subagents)
            .map(prost::Message::encoded_len)
            .sum();
        assert!(total_bytes <= lune_protocol::HOST_SUBAGENTS_BYTES_MAX);
        assert!(listed.iter().all(|agent| !agent.subagents.is_empty()));
    }

    #[test]
    fn dirs_and_git_status_preserve_validated_dir_keys() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let make_terminal = |id: u32, cwd: Option<Directory>| {
            let terminal = server::Terminal {
                tag: Vec::new(),
                size: None,
                cwd,
                title: String::new(),
                tty: None,
                is: Is::Ended,
                statuses: ProgramStatuses::default(),
            };
            (
                ViewId::of(u64::from(id)).unwrap(),
                Located::Shown(Shown::listed(key(id), &terminal)),
            )
        };
        let terminals = BTreeMap::from([
            make_terminal(1, Some(dir("/w/repo"))),
            make_terminal(2, None),
            make_terminal(3, Some(dir("/"))),
            (ViewId::of(4).unwrap(), Located::Shown(Shown::unlisted(key(4)))),
        ]);
        core.send(Command::Terminals(terminals)).unwrap();

        let expected = Dir::parse("/w/repo").unwrap();
        let dirs = ask(&core, Command::Dirs).unwrap();
        assert_eq!(dirs.len(), 1);
        assert!(dirs.contains(&expected));

        let stored = ask(&core, |answer| Command::GitStatus {
            dir: expected.clone(),
            status: GitStatus::Outside,
            why: Some(git::Why::Swept),
            answer,
        })
        .unwrap();
        assert_eq!(stored, None);

        let host = ask(&core, Command::Host).unwrap();
        assert_eq!(host.git_statuses.len(), 1);
        assert_eq!(host.git_statuses[0].dir, expected.as_str());
    }

    #[test]
    fn shown_terminal_renders_what_the_core_displays_and_ignores_spinner_frames() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake(), projects: wake() }).unwrap();
        let mut next = 0u64;
        let mut fresh = || {
            next += 1;
            Ok(Id::of(next).unwrap())
        };
        let root = Dir::parse("/w/lune").unwrap();
        let main = Subfolder::parse("main").unwrap();
        let listed = Listed::of(BTreeSet::from([main.clone()])).unwrap();
        let (projects, made) = Projects::default()
            .changed(
                Change::AddProject {
                    spot: Spot::in_clone(root.clone(), listed.clone(), None),
                    name: None,
                },
                None,
                &mut fresh,
            )
            .unwrap();
        let Some(Made::Project(project)) = made else { panic!("project made") };
        let (projects, _) = projects
            .changed(
                Change::AddWorktree {
                    project,
                    spot: Spot::in_clone(root, listed, Some(main)),
                    name: None,
                    opens: Opens::Shell,
                },
                None,
                &mut fresh,
            )
            .unwrap();
        let view = projects.terminals().next().unwrap().1.id().view();
        core.send(Command::Projects { projects: Ok(projects), revision: Revision::default() })
            .unwrap();

        let term_key = key(5);
        let frame = |title: &str, cwd: &str, is: Is| server::Terminal {
            tag: b"t".to_vec(),
            size: None,
            cwd: Some(dir(cwd)),
            title: title.to_owned(),
            tty: None,
            is,
            statuses: ProgramStatuses::default(),
        };
        let running = Is::Running {
            root: AgentPid::new(10).unwrap(),
            front: Some(server::Foreground {
                pid: AgentPid::new(20).unwrap(),
                argv: vec!["claude".to_owned(), "--resume".to_owned()],
            }),
            doing: Some(Doing::Ready),
            resumes: None,
        };
        let unlisted = Shown::unlisted(term_key);
        let first = Shown::listed(
            term_key,
            &frame("⠋ Fixing the tests", "/w/lune/main/src", running.clone()),
        );
        let second =
            Shown::listed(term_key, &frame("⠙ Fixing the tests", "/w/lune/main/src", running));
        assert_eq!(first, second);
        assert_ne!(unlisted, first);

        core.send(Command::Terminals(BTreeMap::from([(view, Located::Shown(unlisted))]))).unwrap();
        let host = ask(&core, Command::Host).unwrap();
        let Some(api::projects::Held::Kept(kept)) = host.projects.and_then(|it| it.held) else {
            panic!("projects kept");
        };
        let terminal = &kept.projects[0].worktrees().unwrap()[0].terminals[0];
        assert_eq!(terminal.state, Some(api::terminal::State::Unlisted(term_key.into())));
        assert_eq!(terminal.command, None);
        assert_eq!(terminal.shell, None);
        assert!(!terminal.held);

        core.send(Command::Terminals(BTreeMap::from([(view, Located::Shown(first))]))).unwrap();
        let host = ask(&core, Command::Host).unwrap();
        let Some(api::projects::Held::Kept(kept)) = host.projects.and_then(|it| it.held) else {
            panic!("projects kept");
        };
        let terminal = &kept.projects[0].worktrees().unwrap()[0].terminals[0];
        assert_eq!(terminal.folder, "/w/lune/main/src");
        assert_eq!(terminal.command.as_deref(), Some("claude --resume"));
        assert_eq!(
            terminal.titled.as_ref().and_then(|it| it.topic.as_deref()),
            Some("Fixing the tests")
        );
        assert_eq!(terminal.shell, Some(Doing::Ready.to_api()));
        assert!(!terminal.held);
        assert!(matches!(
            &terminal.state,
            Some(api::terminal::State::Running(api::terminal::Running {
                key: Some(k),
                command: Some(cmd),
                titled: Some(titled),
                shell: Some(shell),
            })) if *k == term_key.into()
                && cmd == "claude --resume"
                && titled.topic.as_deref() == Some("Fixing the tests")
                && *shell == Doing::Ready.to_api()
        ));
        assert!(ask(&core, Command::Dirs).unwrap().contains(&Dir::parse("/w/lune/main/src").unwrap()));

        let ended =
            Shown::listed(term_key, &frame("✳ Fixing the tests", "/w/lune/main/src", Is::Ended));
        core.send(Command::Terminals(BTreeMap::from([(view, Located::Shown(ended))]))).unwrap();
        let host = ask(&core, Command::Host).unwrap();
        let Some(api::projects::Held::Kept(kept)) = host.projects.and_then(|it| it.held) else {
            panic!("projects kept");
        };
        let terminal = &kept.projects[0].worktrees().unwrap()[0].terminals[0];
        assert_eq!(terminal.command, None);
        assert_eq!(terminal.shell, None);
        assert!(terminal.held);
        assert_eq!(
            terminal.titled.as_ref().and_then(|it| it.topic.as_deref()),
            Some("Fixing the tests")
        );
        assert!(matches!(
            &terminal.state,
            Some(api::terminal::State::Ended(api::terminal::Ended {
                key: Some(k),
                titled: Some(titled),
            })) if *k == term_key.into() && titled.topic.as_deref() == Some("Fixing the tests")
        ));
    }
}
