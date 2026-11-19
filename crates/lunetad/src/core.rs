use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::mem;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime};

use luneta_protocol::git::{Head, WorkTree};
use luneta_protocol::said::Said;
use tokio::sync::oneshot;

use crate::agent::Agent;
use crate::api;
use crate::claude::Scan;
use crate::github::Found;
use crate::mux::own::server::{self, Is};
use crate::panes::{self, Pane, Titled};
use crate::projects::layout::Leaf as _;
use crate::projects::{Dir, Name, Project, Projects, Revision, Seen, Stage, ViewId, Workstream};
use crate::projects::WorkstreamId;
use crate::remote::{Destination, Named, Reached};
use crate::sayings::{Sayings, ToSay};
use crate::shells::{Doing, Shell};
use crate::transcripts::SessionId;
use crate::mux::{Client, Id, Listed, SessionName, Sessions, Socket};
use crate::mux::TerminalKey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub id: Id,
    pub name: SessionName,
}

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

pub enum Command {
    Agents(Scan),
    PiAgents(Vec<Agent>),
    Sockets { sockets: Vec<Socket>, unread: u32 },
    Panes { asked: Asked, panes: Vec<Pane> },
    Clients { asked: Asked, clients: Listed },
    Terminals(BTreeMap<ViewId, Located>),
    WorkTree { dir: String, tree: WorkTree },
    PullRequests(BTreeMap<String, Found>),
    Shells(Vec<Shell>),
    Topics(HashMap<SessionId, Said>),
    Hosts(Named),
    Projects { projects: Result<Projects, String>, revision: Revision },
    Missing(BTreeSet<Dir>),
    Remote { destination: Destination, reached: Reached },
    Say { said: ToSay, at: SystemTime, answer: oneshot::Sender<Result<(), String>> },
    Sessions(oneshot::Sender<Vec<Addressed>>),
    Dirs(oneshot::Sender<BTreeSet<String>>),
    ProjectFolders(oneshot::Sender<BTreeSet<Dir>>),
    Clones(oneshot::Sender<BTreeMap<Dir, BTreeSet<WorkstreamId>>>),
    Plain(oneshot::Sender<BTreeSet<Dir>>),
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    Tracking(oneshot::Sender<BTreeMap<String, Branch>>),
    Claudes(oneshot::Sender<Vec<Agent>>),
    AllAgents(oneshot::Sender<Vec<Agent>>),
    List(oneshot::Sender<api::ListAgentsResponse>),
    Watch { after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>> },
}

pub struct Addressed {
    pub id: Id,
    pub name: SessionName,
    pub told: u64,
    pub listed: bool,
}

pub struct Wakes {
    pub panes: SyncSender<()>,
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

struct Known {
    found: Scan,
    pi: Vec<Agent>,
    sessions: Sessions,
    unread_sockets: u32,
    terminals: BTreeMap<ViewId, Located>,
    panes: HashMap<Id, Vec<Pane>>,
    clients: HashMap<Id, Listed>,
    trees: BTreeMap<String, WorkTree>,
    pull_requests: BTreeMap<String, Found>,
    shells: Vec<Shell>,
    topics: HashMap<SessionId, Said>,
    remotes: Vec<(Destination, Reached)>,
    unread_hosts: u32,
    projects: Result<Projects, String>,
    revision: Revision,
    missing: BTreeSet<Dir>,
    words: u64,
    told: HashMap<Id, u64>,
    host: api::Host,
    seq: u64,
    waiting: Vec<oneshot::Sender<Option<api::Host>>>,
    sayings: Sayings,
}

fn run(commands: &Receiver<Command>, wakes: &Wakes) {
    let mut known = Known {
        found: Scan { agents: Vec::new(), unread: Vec::new() },
        pi: Vec::new(),
        sessions: Sessions::default(),
        unread_sockets: 0,
        terminals: BTreeMap::new(),
        panes: HashMap::new(),
        clients: HashMap::new(),
        trees: BTreeMap::new(),
        pull_requests: BTreeMap::new(),
        shells: Vec::new(),
        topics: HashMap::new(),
        remotes: Vec::new(),
        unread_hosts: 0,
        projects: Err("lunetad has read no projects yet".to_owned()),
        revision: Revision::default(),
        missing: BTreeSet::new(),
        words: 0,
        told: HashMap::new(),
        host: api::Host::default(),
        seq: 0,
        waiting: Vec::with_capacity(WAITING_MAX),
        sayings: Sayings::default(),
    };
    known.host = api::Host { version: Some(known.version()), ..known.built() };
    for command in commands {
        match command {
            Command::Agents(now) => {
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                known.found = now;
            },
            Command::PiAgents(now) => {
                let pids: HashSet<u32> = now.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.len(), "an agent's pid comes twice");
                known.pi = now;
            },
            Command::Sockets { sockets, unread } => {
                known.unread_sockets = unread;
                if known.saw(sockets) {
                    wake(&wakes.panes);
                }
            },
            Command::Panes { asked, panes } => {
                if !known.answers(&asked) {
                    continue;
                }
                if known.listed(asked.id, panes) {
                    wake(&wakes.git);
                }
            },
            Command::Clients { asked, clients } => {
                if !known.answers(&asked) {
                    continue;
                }
                known.clients.insert(asked.id, clients);
            },
            Command::Terminals(now) => {
                let before = known.dirs();
                known.terminals = now;
                if !known.dirs().is_subset(&before) {
                    wake(&wakes.git);
                }
            },
            Command::WorkTree { dir, tree } => {
                if !known.dirs().contains(&dir) {
                    continue;
                }
                let before = known.branch(&dir).map(str::to_owned);
                known.trees.insert(dir.clone(), tree);
                if known.branch(&dir).is_some_and(|now| before.as_deref() != Some(now)) {
                    wake(&wakes.github);
                }
            },
            Command::PullRequests(now) => known.pull_requests = now,
            Command::Shells(now) => {
                if known.heard(now) {
                    wake(&wakes.panes);
                }
            },
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
            Command::Remote { destination, reached } => {
                let Some((_, was)) = known.remotes.iter_mut().find(|(it, _)| *it == destination)
                else {
                    continue;
                };
                *was = reached;
            },
            Command::Say { said, at, answer } => {
                let taken = known.sayings.take(said, &known.sessions, at);
                let _ = answer.send(taken);
            },
            Command::Sessions(answer) => {
                let sessions = known.addressed().into_iter().map(|(id, name)| {
                    let told = known.told.get(&id).expect("a word is said of each as it comes");
                    Addressed { id, name, told: *told, listed: known.panes.contains_key(&id) }
                });
                let _ = answer.send(sessions.collect());
                continue;
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
                let _ = answer.send(known.found.agents.clone());
                continue;
            },
            Command::AllAgents(answer) => {
                let _ = answer.send(known.found.agents.iter().chain(&known.pi).cloned().collect());
                continue;
            },
            Command::List(answer) => {
                let (unread, unread_files) = known.unread();
                let agents = known.agents();
                let _ = answer.send(api::ListAgentsResponse { agents, unread, unread_files });
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
    fn answers(&self, asked: &Asked) -> bool {
        self.sessions.addressed(&asked.name) == Some(asked.id)
    }

    fn addressed(&self) -> HashSet<(Id, SessionName)> {
        let sessions = &self.sessions;
        let alone = |&(id, name): &(Id, &SessionName)| sessions.addressed(name) == Some(id);
        sessions.running().filter(alone).map(|(id, name)| (id, name.clone())).collect()
    }

    fn saw(&mut self, sockets: Vec<Socket>) -> bool {
        let before = self.addressed();
        self.sessions.saw(sockets);
        let now = self.addressed();
        let addressed = |session: &Id| now.iter().any(|(id, _)| id == session);
        self.panes.retain(|session, _| addressed(session));
        self.clients.retain(|session, _| addressed(session));
        self.told.retain(|session, _| addressed(session));
        let came: Vec<Id> = now.difference(&before).map(|(id, _)| *id).collect();
        for session in &came {
            self.say(*session);
        }
        !came.is_empty()
    }

    fn heard(&mut self, now: Vec<Shell>) -> bool {
        let unsaid = |shell: &&Shell, among: &[Shell]| !among.iter().any(|was| was.says_as(shell));
        let new = now.iter().filter(|shell| unsaid(shell, &self.shells));
        let gone = self.shells.iter().filter(|shell| unsaid(shell, &now));
        let can_ask = |session: &Id| self.told.contains_key(session);
        let told: HashSet<Id> =
            new.chain(gone).filter_map(|shell| self.house(shell)).filter(can_ask).collect();
        for session in &told {
            self.say(*session);
        }
        self.shells = now;
        !told.is_empty()
    }

    fn named(&mut self, now: Named) {
        let unique: HashSet<&Destination> = now.machines.iter().collect();
        assert_eq!(unique.len(), now.machines.len(), "a machine is named twice");
        self.unread_hosts = now.unread;
        let mut before = mem::take(&mut self.remotes);
        for destination in now.machines {
            let reached = match before.iter().position(|(was, _)| *was == destination) {
                Some(at) => before.swap_remove(at).1,
                None => Reached::Reaching,
            };
            self.remotes.push((destination, reached));
        }
    }

    fn say(&mut self, session: Id) {
        self.words = self.words.checked_add(1).expect("fewer than 2^64 words are said");
        self.told.insert(session, self.words);
    }

    fn house(&self, shell: &Shell) -> Option<Id> {
        let by = shell.started.checked_add(Duration::from_secs(1))?;
        self.sessions.housing(&shell.named, by)
    }

    fn listed(&mut self, session: Id, mut panes: Vec<Pane>) -> bool {
        assert!(self.sessions.name_of(session).is_some());
        let ids: HashSet<u32> = panes.iter().map(|pane| pane.id).collect();
        assert_eq!(ids.len(), panes.len(), "a pane's id comes twice");
        if let Some(listed) = self.panes.get(&session) {
            panes::keep(&mut panes, listed);
        }
        let before = self.dirs();
        self.panes.insert(session, panes);
        !self.dirs().is_subset(&before)
    }

    fn dirs(&self) -> BTreeSet<String> {
        let cwds = self.panes.values().flatten().filter_map(|pane| pane.cwd.as_ref());
        let cwds = cwds.map(|cwd| cwd.as_str().to_owned());
        let listed = self.terminals.values().filter_map(|found| match found {
            Located::Shown(Shown { listed: Some(listed), .. }) => Dir::parse(&listed.cwd).ok(),
            Located::Shown(Shown { listed: None, .. }) | Located::Lost | Located::Looking => None,
        });
        let cwds = cwds.chain(listed.map(|cwd| cwd.as_str().to_owned()));
        let present = |folder: &Dir| !self.missing.contains(folder);
        let folders = self.projects.iter().flat_map(Projects::workstream_folders).filter(present);
        cwds.chain(folders.map(|folder| folder.as_str().to_owned())).collect()
    }

    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.trees.retain(|dir, _| dirs.contains(dir));
        self.pull_requests.retain(|dir, _| dirs.contains(dir));
    }

    fn branch(&self, dir: &str) -> Option<&str> {
        match self.trees.get(dir)? {
            WorkTree::Checkout { head: Head::Branch { name, .. }, .. } => Some(name.as_str()),
            WorkTree::Checkout { head: Head::Detached, .. }
            | WorkTree::NoCheckout { .. }
            | WorkTree::Outside => None,
        }
    }

    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &String| Some((dir.clone(), self.branch(dir)?.to_owned()));
        self.trees.keys().filter_map(branch).collect()
    }

    fn tracking(&self) -> BTreeMap<String, Branch> {
        let tracking = |(dir, tree): (&String, &WorkTree)| {
            let WorkTree::Checkout {
                repository: Some(repository),
                head: Head::Branch { name, upstream: Some(_), .. },
                ..
            } = tree
            else {
                return None;
            };
            Some((dir.clone(), Branch { repository: repository.clone(), name: name.clone() }))
        };
        self.trees.iter().filter_map(tracking).collect()
    }

    fn agents(&self) -> Vec<api::Agent> {
        let claude = self.found.agents.iter().map(|agent| {
            let topic = self.topics.get(agent.session_id.as_str()).cloned();
            agent.to_api(&self.sessions, topic)
        });
        claude.chain(self.pi.iter().map(|agent| agent.to_api(&self.sessions, None))).collect()
    }

    fn unread(&self) -> (u32, Vec<api::UnreadFile>) {
        let mut files: Vec<api::UnreadFile> =
            self.found.unread.iter().map(|file| file.to_api(&self.sessions)).collect();
        files.sort_by_key(|file| file.pid);
        let count =
            u32::try_from(files.len()).expect("a folder listed in memory has under 2^32 files");
        (count, files)
    }

    fn shells(&self) -> HashMap<(Id, u32), &Shell> {
        let mut shells: HashMap<(Id, u32), &Shell> = HashMap::new();
        for shell in &self.shells {
            let Some(session) = self.house(shell) else {
                continue;
            };
            shells
                .entry((session, shell.pane))
                .and_modify(|was| {
                    if was.started < shell.started {
                        *was = shell;
                    }
                })
                .or_insert(shell);
        }
        shells
    }

    fn assert_listed(&self) {
        for id in self.panes.keys().chain(self.clients.keys()) {
            let name = self.sessions.name_of(*id).expect("a session zellij listed runs");
            assert_eq!(self.sessions.addressed(name), Some(*id));
        }
        let addressed = self.addressed();
        assert_eq!(self.told.len(), addressed.len());
        assert!(addressed.iter().all(|(id, _)| self.told.contains_key(id)));
    }

    fn built(&self) -> api::Host {
        self.assert_listed();
        let sessions = self.built_sessions();
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        let (unread, unread_files) = self.unread();
        let pull_requests = self
            .pull_requests
            .iter()
            .filter(|(dir, found)| self.branch(dir) == Some(&found.branch))
            .map(|(dir, found)| found.pull_request.to_api(dir))
            .collect();
        let remotes =
            self.remotes.iter().map(|(destination, reached)| reached.to_api(destination)).collect();
        api::Host {
            version: None,
            sessions,
            work_trees: self.trees.iter().map(|(dir, tree)| tree.to_api(dir)).collect(),
            agents,
            unread,
            pull_requests,
            unread_sockets: Some(self.unread_sockets),
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
        let workstreams = |project: &Project| {
            let built = project.placed().map(|(folder, workstream)| {
                let branch = self.branch(folder.as_str());
                let renamed = workstream.renamed().map(Name::as_str);
                api::Workstream {
                    id: workstream.id().get(),
                    name: renamed.or(branch).unwrap_or_else(|| folder.last()).to_owned(),
                    renamed: renamed.is_some(),
                    seen: Some(self.seen(&folder).to_api()),
                    folder: folder.as_str().to_owned(),
                    terminals: self.built_terminals(workstream),
                    used_at: workstream.used().map(|used| used.time().into()),
                }
            });
            built.collect()
        };
        let project = |project: &Project| api::Project {
            id: project.id().get(),
            name: project.name().as_str().to_owned(),
            dir: project.dir().as_str().to_owned(),
            missing: self.missing.contains(project.dir()),
            workstreams: workstreams(project),
        };
        let projects = projects.all().iter().map(project).collect();
        let kept = api::projects::Kept { projects, version: Some(self.revision.to_api()) };
        api::Projects { held: Some(api::projects::Held::Kept(kept)) }
    }

    fn built_terminals(&self, workstream: &Workstream) -> Vec<api::Terminal> {
        use api::terminal::Found as Told;
        let built = workstream.layout().terminals().map(|terminal| {
            let id = terminal.id();
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
            let titled = listed.and_then(|listed| panes::titled(&listed.title));
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
        if !self.trees.contains_key(folder.as_str()) {
            return Seen::Unread;
        }
        let branch = self.branch(folder.as_str());
        let found = self.pull_requests.get(folder.as_str());
        let found = found.filter(|found| branch == Some(found.branch.as_str()));
        Seen::Read(Stage::of(branch, found.map(|found| &found.pull_request)))
    }

    fn built_sessions(&self) -> Vec<api::Session> {
        let mut running: Vec<(Id, &SessionName)> = self.sessions.running().collect();
        running.sort_unstable_by_key(|(id, name)| (*name, id.started()));
        let shells = self.shells();
        let zellij_panes = |id: Id, panes: &[Pane]| {
            let doing = |pane: &Pane| shells.get(&(id, pane.id)).map(|shell| &shell.doing);
            api::Panes { panes: panes.iter().map(|pane| pane.to_api(doing(pane))).collect() }
        };
        let clients_of = |clients: &Listed| api::Clients {
            clients: clients.iter().copied().map(Client::to_api).collect(),
        };
        running
            .into_iter()
            .map(|(id, name)| {
                let started_at = Some(id.started().into());
                let zellij = api::ZellijSession { started_at, socket: id.socket() };
                api::Session {
                    name: name.to_string(),
                    started_at: zellij.started_at,
                    socket: zellij.socket,
                    panes: self.panes.get(&id).map(|panes| zellij_panes(id, panes)),
                    clients: self.clients.get(&id).map(clients_of),
                    mux: Some(api::session::Mux::Zellij(zellij)),
                }
            })
            .collect()
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
            let _ = waiting.send(Some(self.host.clone()));
        }
    }

    fn watch(&mut self, after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>>) {
        self.update();
        assert_eq!(self.host.version, Some(self.version()));
        if after != Some(self.version()) {
            let _ = answer.send(Some(self.host.clone()));
            return;
        }
        self.waiting.retain(|waiting| !waiting.is_closed());
        assert!(self.waiting.len() <= WAITING_MAX);
        if self.waiting.len() < WAITING_MAX {
            self.waiting.push(answer);
        } else {
            let _ = answer.send(None);
        }
    }
}
