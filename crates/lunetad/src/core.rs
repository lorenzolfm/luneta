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
use crate::panes::{self, Pane};
use crate::remote::{Destination, Named, Reached};
use crate::sayings::{Sayings, ToSay};
use crate::shells::{Doing, Shell};
use crate::log;
use crate::transcripts::SessionId;
use crate::mux::{Client, Id, Listed, OwnId, OwnKey, OwnMade, SessionName, Sessions, Socket};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub id: Id,
    pub name: SessionName,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Own {
    pub id: OwnId,
    pub made: OwnMade,
    pub name: SessionName,
    pub panes: Vec<Pane>,
    pub clients: Listed,
    pub doings: BTreeMap<u32, Doing>,
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
    Owns(Vec<Own>),
    WorkTree { dir: String, tree: WorkTree },
    PullRequests(BTreeMap<String, Found>),
    Shells(Vec<Shell>),
    Topics(HashMap<SessionId, Said>),
    Hosts(Named),
    Remote { destination: Destination, reached: Reached },
    Say { said: ToSay, at: SystemTime, answer: oneshot::Sender<()> },
    Sessions(oneshot::Sender<Vec<Addressed>>),
    Dirs(oneshot::Sender<BTreeSet<String>>),
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    Tracking(oneshot::Sender<BTreeMap<String, Branch>>),
    Claudes(oneshot::Sender<Vec<Agent>>),
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
    owns: Vec<Own>,
    shared: BTreeSet<SessionName>,
    panes: HashMap<Id, Vec<Pane>>,
    clients: HashMap<Id, Listed>,
    trees: BTreeMap<String, WorkTree>,
    pull_requests: BTreeMap<String, Found>,
    shells: Vec<Shell>,
    topics: HashMap<SessionId, Said>,
    remotes: Vec<(Destination, Reached)>,
    unread_hosts: u32,
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
        owns: Vec::new(),
        shared: BTreeSet::new(),
        panes: HashMap::new(),
        clients: HashMap::new(),
        trees: BTreeMap::new(),
        pull_requests: BTreeMap::new(),
        shells: Vec::new(),
        topics: HashMap::new(),
        remotes: Vec::new(),
        unread_hosts: 0,
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
                known.say_shared();
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
            Command::Owns(now) => {
                if known.owned(now) {
                    wake(&wakes.git);
                }
                known.say_shared();
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
            Command::Remote { destination, reached } => {
                let Some((_, was)) = known.remotes.iter_mut().find(|(it, _)| *it == destination)
                else {
                    continue;
                };
                *was = reached;
            },
            Command::Say { said, at, answer } => {
                known.sayings.take(said, &known.sessions, at);
                let _ = answer.send(());
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

#[derive(Clone, Copy)]
enum Row<'a> {
    Zellij(Id, &'a SessionName),
    Own(&'a Own),
}

impl<'a> Row<'a> {
    fn name(self) -> &'a SessionName {
        match self {
            Row::Zellij(_, name) => name,
            Row::Own(own) => &own.name,
        }
    }

    fn ranked(self) -> (&'a SessionName, Option<OwnId>, Option<SystemTime>) {
        match self {
            Row::Zellij(id, name) => (name, None, Some(id.started())),
            Row::Own(own) => (&own.name, Some(own.id), None),
        }
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

    fn owned(&mut self, now: Vec<Own>) -> bool {
        let ids: HashSet<OwnId> = now.iter().map(|own| own.id).collect();
        assert_eq!(ids.len(), now.len(), "an own session's id comes twice");
        let names: HashSet<&SessionName> = now.iter().map(|own| &own.name).collect();
        assert_eq!(names.len(), now.len(), "an own session's name comes twice");
        for own in &now {
            let ids: HashSet<u32> = own.panes.iter().map(|pane| pane.id).collect();
            assert_eq!(ids.len(), own.panes.len(), "a pane's id comes twice");
        }
        let before = self.dirs();
        self.owns = now;
        !self.dirs().is_subset(&before)
    }

    fn say_shared(&mut self) {
        let zellij: BTreeSet<&SessionName> =
            self.sessions.running().map(|(_, name)| name).collect();
        let shared: BTreeSet<SessionName> = self
            .owns
            .iter()
            .filter(|own| zellij.contains(&own.name))
            .map(|own| own.name.clone())
            .collect();
        for name in shared.difference(&self.shared) {
            log::say(&format!("a zellij session and an own one are both named {name}"));
        }
        self.shared = shared;
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
        let owns = self.owns.iter().flat_map(|own| &own.panes);
        let cwds = self.panes.values().flatten().chain(owns).filter_map(|pane| pane.cwd.as_ref());
        cwds.map(|cwd| cwd.as_str().to_owned()).collect()
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
        }
    }

    fn built_sessions(&self) -> Vec<api::Session> {
        let zellij = self.sessions.running().map(|(id, name)| Row::Zellij(id, name));
        let mut running: Vec<Row> = zellij.chain(self.owns.iter().map(Row::Own)).collect();
        running.sort_unstable_by_key(|row| row.ranked());
        let shells = self.shells();
        let zellij_panes = |id: Id, panes: &[Pane]| {
            let doing = |pane: &Pane| shells.get(&(id, pane.id)).map(|shell| &shell.doing);
            api::Panes { panes: panes.iter().map(|pane| pane.to_api(doing(pane))).collect() }
        };
        let own_panes = |own: &Own| api::Panes {
            panes: own.panes.iter().map(|pane| pane.to_api(own.doings.get(&pane.id))).collect(),
        };
        let clients_of = |clients: &Listed| api::Clients {
            clients: clients.iter().copied().map(Client::to_api).collect(),
        };
        running
            .into_iter()
            .map(|row| {
                let name = row.name().to_string();
                match row {
                    Row::Zellij(id, _) => {
                        let started_at = Some(id.started().into());
                        let zellij = api::ZellijSession { started_at, socket: id.socket() };
                        api::Session {
                            name,
                            started_at: zellij.started_at,
                            socket: zellij.socket,
                            panes: self.panes.get(&id).map(|panes| zellij_panes(id, panes)),
                            clients: self.clients.get(&id).map(clients_of),
                            mux: Some(api::session::Mux::Zellij(zellij)),
                        }
                    },
                    Row::Own(own) => api::Session {
                        name,
                        started_at: None,
                        socket: 0,
                        panes: Some(own_panes(own)),
                        clients: Some(clients_of(&own.clients)),
                        mux: Some(api::session::Mux::Own(
                            OwnKey { id: own.id, made: own.made }.into(),
                        )),
                    },
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
