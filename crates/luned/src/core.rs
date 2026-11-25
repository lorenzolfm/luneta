use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::mem;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::SystemTime;

use lune_domain::agent::{Harness, Rung};
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{Dir, Holds, Name, Project, Projects, Seen, Stage, Subfolder};
use lune_domain::projects::{Listed, ViewId, Worktree, WorktreeId};
use lune_protocol::git::{GitStatus, Head};
use lune_protocol::github::PullRequest;
use lune_protocol::said::Said;
use lune_protocol::shell::Doing;
use tokio::sync::oneshot;

use crate::agent::{Agent, SeenAgent, Unread};
use crate::api;
use crate::claude::Scan;
use crate::git;
use crate::github::Found;
use crate::mux::own::server::{self, Is, ProgramStatus, ProgramStatusKind, ProgramStatusState};
use crate::titles::{self, Titled};
use crate::projects::Revision;
use crate::remote::{Destination, Line, Named, Reached};
use crate::sayings::{Sayings, ToSay};
use crate::statuses::{BlockedKind, Report, ReportedAgent, State, Statuses, SubagentReport};
use crate::transcripts::SessionId;
use crate::watch::Troubles;
use crate::{process, Directory};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shown {
    pub key: TerminalKey,
    pub listed: Option<Box<server::Terminal>>,
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

pub enum Command {
    Agents(Scan),
    PiAgents(Vec<ReportedAgent>),
    AgyAgents(Vec<ReportedAgent>),
    MuxTerminals(Vec<server::Terminal>),
    Terminals(BTreeMap<ViewId, Located>),
    GitStatus {
        dir: String,
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
    Dirs(oneshot::Sender<BTreeSet<String>>),
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
    id: process::Id,
    started_at: Option<SystemTime>,
    cwd: Option<Directory>,
}

struct Known {
    found: Scan,
    pi: Vec<ReportedAgent>,
    agy: Vec<ReportedAgent>,
    mux_terminals: BTreeMap<TerminalKey, server::Terminal>,
    osc_agents: Vec<SeenAgent>,
    osc_processes: HashMap<(TerminalKey, u32), CachedProcess>,
    osc_troubles: Troubles,
    agent_statuses: Statuses,
    terminals: BTreeMap<ViewId, Located>,
    statuses: BTreeMap<String, GitStatus>,
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
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.seen.id.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                let reports = now.agents.iter().map(|agent| agent.report.clone()).collect();
                known.found = now;
                known.agent_statuses.replace_rung(Rung::Feed, reports);
                known.reconcile_osc_statuses();
            },
            Command::PiAgents(now) => {
                let pids: HashSet<u32> = now.iter().map(|agent| agent.seen.id.pid).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                let reports = now.iter().map(|agent| agent.report.clone()).collect();
                known.pi = now;
                known.agent_statuses.replace_rung(Rung::Extension, reports);
                known.reconcile_osc_statuses();
            },
            Command::AgyAgents(now) => {
                let pids: HashSet<u32> = now.iter().map(|agent| agent.seen.id.pid).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                let reports = now.iter().map(|agent| agent.report.clone()).collect();
                known.agy = now;
                known.agent_statuses.replace_rung(Rung::Hook, reports);
                known.reconcile_osc_statuses();
            },
            Command::MuxTerminals(now) => {
                let count = now.len();
                known.mux_terminals =
                    now.into_iter().map(|terminal| (terminal.key, terminal)).collect();
                assert_eq!(known.mux_terminals.len(), count, "a mux terminal's key comes twice");
                known.reconcile_osc_statuses();
            },
            Command::Terminals(now) => {
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
                let _ = answer.send(why.filter(|_| changed).and_then(|why| why.said(&dir)));
                if known.branch(&dir).is_some_and(|now| before.as_deref() != Some(now)) {
                    wake(&wakes.github);
                }
            },
            Command::PullRequests(now) => known.pull_requests = now,
            Command::Topics(now) => known.topics = now,
            Command::Hosts(now) => known.named(now),
            Command::Projects { projects, revision } => {
                let before = known.dirs();
                known.projects = projects;
                known.revision = revision;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
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

    fn dirs(&self) -> BTreeSet<String> {
        let listed = self.terminals.values().filter_map(|found| match found {
            Located::Shown(Shown { listed: Some(listed), .. }) => Dir::parse(&listed.cwd).ok(),
            Located::Shown(Shown { listed: None, .. }) | Located::Lost | Located::Looking => None,
        });
        let cwds = listed.map(|cwd| cwd.as_str().to_owned());
        let present = |folder: &Dir| !self.missing.contains(folder);
        let folders = self.projects.iter().flat_map(Projects::worktree_folders).filter(present);
        cwds.chain(folders.map(|folder| folder.as_str().to_owned())).collect()
    }

    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.statuses.retain(|dir, _| dirs.contains(dir));
        self.pull_requests.retain(|dir, _| dirs.contains(dir));
    }

    fn branch(&self, dir: &str) -> Option<&str> {
        match self.statuses.get(dir)? {
            GitStatus::Checkout { head: Head::Branch { name, .. }, .. } => Some(name.as_str()),
            GitStatus::Checkout { head: Head::Detached, .. }
            | GitStatus::NoCheckout { .. }
            | GitStatus::Outside => None,
        }
    }

    fn pull_request(&self, dir: &str) -> Option<&PullRequest> {
        let found = self.pull_requests.get(dir)?;
        let on_its_branch = self.branch(dir) == Some(found.branch.as_str());
        on_its_branch.then_some(&found.pull_request)
    }

    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &String| Some((dir.clone(), self.branch(dir)?.to_owned()));
        self.statuses.keys().filter_map(branch).collect()
    }

    fn tracking(&self) -> BTreeMap<String, Branch> {
        let tracking = |(dir, status): (&String, &GitStatus)| {
            let GitStatus::Checkout {
                repository: Some(repository),
                head: Head::Branch { name, upstream: Some(_), .. },
                ..
            } = status
            else {
                return None;
            };
            Some((dir.clone(), Branch { repository: repository.clone(), name: name.clone() }))
        };
        self.statuses.iter().filter_map(tracking).collect()
    }

    fn reconcile_osc_statuses(&mut self) {
        let mut by_terminal: HashMap<(TerminalKey, Harness), process::Id> = HashMap::new();
        let mut known_pids: HashSet<u32> = HashSet::new();
        for reported in self.found.agents.iter().chain(&self.pi).chain(&self.agy) {
            known_pids.insert(reported.seen.id.pid);
            if let Some(terminal) = reported.seen.terminal {
                by_terminal
                    .entry((terminal, reported.seen.harness))
                    .or_insert_with(|| reported.seen.id.clone());
            }
        }
        let mut osc_reports = Vec::new();
        let mut osc_agents = Vec::new();
        let mut active_processes = HashSet::new();
        for terminal in self.mux_terminals.values() {
            assert!(terminal.statuses.len() <= lune_protocol::PROGRAM_STATUS_RECORDS_MAX);
            assert!(
                terminal.statuses.iter().filter(|status| status.id.is_empty()).count() <= 1,
                "a terminal reports at most one root program status"
            );
            let Is::Running { root, front, doing } = &terminal.is else {
                continue;
            };
            let Some(root_record) = terminal.statuses.iter().find(|status| status.id.is_empty())
            else {
                continue;
            };
            let Some(harness) = Harness::from_osc_7501_app(&root_record.app) else {
                continue;
            };
            let subagents: Vec<SubagentReport> = terminal
                .statuses
                .iter()
                .filter(|status| !status.id.is_empty() && status.app == root_record.app)
                .map(subagent_report)
                .collect();
            if let Some(matched) = by_terminal.get(&(terminal.key, harness)) {
                let in_foreground = front
                    .as_ref()
                    .is_none_or(|front| front.pid != *root || matched.pid == *root);
                if in_foreground {
                    osc_reports.push(osc_report(matched.clone(), root_record, subagents));
                }
                continue;
            }
            if matches!(doing, Some(Doing::Ready | Doing::Ended { .. })) {
                continue;
            }
            let pid = front.as_ref().map_or(*root, |front| front.pid);
            if pid <= 1 {
                continue;
            }
            if !known_pids.insert(pid) {
                continue;
            }
            let Some((id, started_at, cwd)) = resolve_osc_process(
                &mut self.osc_processes,
                &mut self.osc_troubles,
                terminal,
                pid,
            ) else {
                continue;
            };
            active_processes.insert((terminal.key, pid));
            let entrypoint =
                harness.to_api().prompt_word().and_then(|word| Said::new(word.to_owned()));
            osc_agents.push(SeenAgent {
                id: id.clone(),
                harness,
                session_id: None,
                cwd,
                started_at,
                name: None,
                entrypoint,
                terminal: Some(terminal.key),
            });
            osc_reports.push(osc_report(id, root_record, subagents));
        }
        self.osc_processes.retain(|key, _| active_processes.contains(key));
        self.osc_troubles.done();
        assert!(osc_agents.len() <= self.mux_terminals.len());
        assert!(osc_reports.len() <= self.mux_terminals.len());
        assert!(self.osc_processes.len() <= self.mux_terminals.len());
        self.osc_agents = osc_agents;
        self.agent_statuses.replace_rung(Rung::Osc7501, osc_reports);
        let live: HashSet<process::Id> = self
            .found
            .agents
            .iter()
            .chain(&self.pi)
            .chain(&self.agy)
            .map(|reported| reported.seen.id.clone())
            .chain(self.osc_agents.iter().map(|seen| seen.id.clone()))
            .collect();
        self.agent_statuses.retain_live(&live);
    }

    fn all_agents(&self) -> Vec<Agent> {
        self.found
            .agents
            .iter()
            .chain(&self.pi)
            .chain(&self.agy)
            .map(|reported| &reported.seen)
            .chain(&self.osc_agents)
            .map(|seen| self.agent_statuses.apply(seen))
            .collect()
    }

    fn agents(&self) -> Vec<api::Agent> {
        self.all_agents()
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
            .collect()
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
            .filter_map(|dir| Some(self.pull_request(dir)?.to_api(dir)))
            .collect();
        let remotes = self.remotes.iter().map(Remote::to_api).collect();
        api::Host {
            version: None,
            git_statuses: self.statuses.iter().map(|(dir, status)| status.to_api(dir)).collect(),
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
                let given = self.branch(dir.as_str()).unwrap_or_else(|| dir.last());
                api::project::Holds::Folder(worktree(dir.clone(), given, own))
            },
            Holds::Clone(rows) => {
                let listing = Some(self.listing(project.dir(), rows));
                let rows = rows.iter().map(|(subfolder, it)| {
                    let folder = project.dir().at(subfolder);
                    let given = match self.branch(folder.as_str()) {
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
        use api::terminal::Found as Told;
        let built = worktree.layout().terminals().map(|terminal| {
            let id = terminal.id().view();
            let found = self.terminals.get(&id).unwrap_or(&Located::Looking);
            let (told, listed) = match found {
                Located::Shown(Shown { key, listed }) => {
                    (Told::Key((*key).into()), listed.as_deref())
                },
                Located::Lost => (Told::Lost(()), None),
                Located::Looking => (Told::Looking(()), None),
            };
            let cwd = listed.and_then(|listed| Dir::parse(&listed.cwd).ok());
            let folder = cwd.as_ref().unwrap_or_else(|| terminal.folder());
            let (command, doing, held) = match listed.map(|listed| &listed.is) {
                Some(Is::Running { front, doing, .. }) => {
                    let command = front.as_ref().and_then(|front| Said::new(front.argv.join(" ")));
                    (command, doing.as_ref(), false)
                },
                Some(Is::Ended) => (None, None, true),
                None => (None, None, false),
            };
            let titled = listed.and_then(|listed| titles::titled(&listed.title));
            api::Terminal {
                id: id.get(),
                found: Some(told),
                folder: folder.as_str().to_owned(),
                command: command.map(String::from),
                titled: titled.as_ref().map(Titled::to_api),
                shell: doing.map(Doing::to_api),
                held,
                name: terminal.name().map(|name| name.as_str().to_owned()),
            }
        });
        built.collect()
    }

    fn seen(&self, folder: &Dir) -> Seen {
        if self.missing.contains(folder) {
            return Seen::Missing;
        }
        if !self.statuses.contains_key(folder.as_str()) {
            return Seen::Unread;
        }
        let dir = folder.as_str();
        Seen::Read(Stage::of(self.branch(dir), self.pull_request(dir)))
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

fn osc_state(state: ProgramStatusState) -> State {
    match state {
        ProgramStatusState::Idle => State::Idle,
        ProgramStatusState::Working => State::Working,
        ProgramStatusState::Done => State::Done,
        ProgramStatusState::Blocked => State::Blocked,
        ProgramStatusState::Error => State::Error,
    }
}

fn osc_kind(kind: ProgramStatusKind) -> Option<BlockedKind> {
    match kind {
        ProgramStatusKind::None => None,
        ProgramStatusKind::Permission => Some(BlockedKind::Permission),
        ProgramStatusKind::Question => Some(BlockedKind::Question),
        ProgramStatusKind::Auth => Some(BlockedKind::Auth),
    }
}

fn osc_report(
    agent: process::Id,
    status: &ProgramStatus,
    subagents: Vec<SubagentReport>,
) -> Report {
    assert!(agent.pid > 1);
    let state = osc_state(status.state);
    let kind = if matches!(state, State::Blocked) { osc_kind(status.kind) } else { None };
    Report {
        agent,
        rung: Rung::Osc7501,
        state,
        kind,
        message: Said::new(status.message.trim().to_owned()),
        at: status.at,
        subagents,
    }
}

fn subagent_report(status: &ProgramStatus) -> SubagentReport {
    assert_ne!(status.id, "");
    let state = osc_state(status.state);
    let kind = if matches!(state, State::Blocked) { osc_kind(status.kind) } else { None };
    SubagentReport {
        id: status.id.clone(),
        state,
        kind,
        title: Said::new(status.title.trim().to_owned()),
        message: Said::new(status.message.trim().to_owned()),
        progress: status.progress,
        at: status.at,
    }
}

fn resolve_osc_process(
    cache: &mut HashMap<(TerminalKey, u32), CachedProcess>,
    troubles: &mut Troubles,
    terminal: &server::Terminal,
    pid: u32,
) -> Option<(process::Id, Option<SystemTime>, Directory)> {
    assert!(pid > 1);
    let parsed_cwd = Directory::parse(terminal.cwd.clone()).ok();
    if let Some(cached) = cache.get(&(terminal.key, pid)) {
        let cwd = parsed_cwd.or_else(|| cached.cwd.clone())?;
        return Some((cached.id.clone(), cached.started_at, cwd));
    }
    let start = process::start(pid)?;
    let id = process::Id { pid, start };
    let started_at = process::started(pid);
    let process_cwd = if parsed_cwd.is_some() {
        None
    } else {
        process_cwd_or_trouble(pid, troubles)
    };
    let cwd = parsed_cwd.or_else(|| process_cwd.clone())?;
    cache.insert(
        (terminal.key, pid),
        CachedProcess { id: id.clone(), started_at, cwd: process_cwd },
    );
    Some((id, started_at, cwd))
}

fn process_cwd_or_trouble(pid: u32, troubles: &mut Troubles) -> Option<Directory> {
    assert!(pid > 1);
    match process::cwd(pid) {
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

    use lune_domain::mux::OwnMade;

    use super::*;

    fn key(id: u32) -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(id).unwrap(), made: OwnMade::from_nanos(10) }
    }

    #[test]
    fn osc_7501_overrides_lower_rung_and_yields_when_agent_leaves_foreground() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake() }).unwrap();
        let pid = std::process::id();
        let start_id = process::start(pid).unwrap();
        let id = process::Id { pid, start: start_id };
        let terminal = key(7);
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);

        let seen = SeenAgent {
            id: id.clone(),
            harness: Harness::ClaudeCode,
            session_id: Said::new("s1".to_owned()),
            cwd: Directory::parse("/w".to_owned()).unwrap(),
            started_at: Some(at(1)),
            name: None,
            entrypoint: Said::new("cli".to_owned()),
            terminal: Some(terminal),
        };
        let feed = Report {
            agent: id,
            rung: Rung::Feed,
            state: State::Working,
            kind: None,
            message: None,
            at: Some(at(10)),
            subagents: Vec::new(),
        };
        core.send(Command::Agents(Scan {
            agents: vec![ReportedAgent { seen, report: feed }],
            unread: Vec::new(),
        }))
        .unwrap();

        let listed = || ask(&core, Command::List).unwrap().agents;
        let initial = listed();
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].status, Some(api::agent::Status::KnownStatus(api::Status::Busy.into())));
        assert_eq!(initial[0].status_rung, Some(api::StatusRung::Feed.into()));

        let root_status = ProgramStatus {
            id: String::new(),
            state: ProgramStatusState::Blocked,
            kind: ProgramStatusKind::Permission,
            progress: None,
            app: "claude-code".to_owned(),
            title: String::new(),
            message: "approve touch".to_owned(),
            at: Some(at(20)),
        };
        let child_status = ProgramStatus {
            id: "sub-1".to_owned(),
            state: ProgramStatusState::Working,
            kind: ProgramStatusKind::None,
            progress: Some(50),
            app: "claude-code".to_owned(),
            title: "Explore".to_owned(),
            message: String::new(),
            at: Some(at(21)),
        };
        let mux_terminal = |front_pid| server::Terminal {
            key: terminal,
            tag: Vec::new(),
            size: None,
            cwd: "/w".to_owned(),
            title: String::new(),
            is: Is::Running {
                root: 100,
                front: Some(server::Foreground { pid: front_pid, argv: vec!["claude".to_owned()] }),
                doing: None,
            },
            statuses: vec![root_status.clone(), child_status.clone()],
        };

        core.send(Command::MuxTerminals(vec![mux_terminal(pid)])).unwrap();
        let after = listed();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].status, Some(api::agent::Status::KnownStatus(api::Status::Waiting.into())));
        assert_eq!(after[0].status_rung, Some(api::StatusRung::Osc7501.into()));
        assert_eq!(after[0].blocked_kind, Some(api::BlockedKind::Permission.into()));
        assert_eq!(after[0].status_message.as_deref(), Some("approve touch"));
        assert_eq!(after[0].subagents.len(), 1);
        assert_eq!(after[0].subagents[0].id, "sub-1");
        assert_eq!(after[0].subagents[0].title.as_deref(), Some("Explore"));
        assert_eq!(after[0].subagents[0].progress, Some(50));

        core.send(Command::MuxTerminals(vec![mux_terminal(100)])).unwrap();
        let fallback = listed();
        assert_eq!(fallback.len(), 1);
        assert_eq!(
            fallback[0].status,
            Some(api::agent::Status::KnownStatus(api::Status::Busy.into()))
        );
        assert_eq!(fallback[0].status_rung, Some(api::StatusRung::Feed.into()));
        assert_eq!(fallback[0].subagents, []);
    }

    #[test]
    fn a_line_naming_this_machine_takes_no_word_of_a_watch_until_it_names_another() {
        let wake = || mpsc::sync_channel(1).0;
        let core = start(Wakes { git: wake(), github: wake() }).unwrap();
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
}
