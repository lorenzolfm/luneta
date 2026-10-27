//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Duration;

use luneta_protocol::git::{Head, WorkTree};
use tokio::sync::oneshot;

use crate::api;
use crate::claude::Scan;
use crate::clients::Client;
use crate::github::Found;
use crate::panes::{self, Pane};
use crate::shells::Shell;
use crate::zellij::{Id, Sessions, Socket};

pub enum Command {
    /// What Claude Code's files say now.
    Agents(Scan),
    /// The zellij sessions that run now, and how many of zellij's folders and
    /// sockets could not be read, any of which could be another's.
    Sockets { sockets: Vec<Socket>, unread: u32 },
    /// What zellij listed of the panes of `session`, asked for by `name`.
    Panes { session: Id, name: String, panes: Vec<Pane> },
    /// What zellij listed of the clients of `session`, asked for by `name`.
    Clients { session: Id, name: String, clients: Vec<Client> },
    /// What git said of the work tree in `dir`.
    WorkTree { dir: String, tree: WorkTree },
    /// The pull request gh found last of the branch in each directory, by
    /// the directory.
    PullRequests(BTreeMap<String, Found>),
    /// What the shells with luneta's hook say now.
    Shells(Vec<Shell>),
    /// Asks for the sessions zellij can be asked about, and the name to ask
    /// by, for their panes.
    Sessions(oneshot::Sender<Vec<(Id, String)>>),
    /// Asks for the directories the panes work in, for git to be asked about.
    Dirs(oneshot::Sender<BTreeSet<String>>),
    /// Asks for the branch checked out in each directory a pane works in, by
    /// the directory, where git has said one is, for gh to be asked about.
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    /// Asks for every agent, as the API gives it.
    List(oneshot::Sender<api::ListAgentsResponse>),
    /// Asks for the host once it is no longer as `after` saw it: at once if it
    /// is not, or `after` is none. `None` for an answer is too many asking.
    Watch { after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>> },
}

/// Who the core wakes, as the host changes in a way that has them ask
/// something at once rather than on their next sweep.
pub struct Wakes {
    /// The thread that asks zellij for panes, as it can be asked about a
    /// session it could not be before.
    pub panes: SyncSender<()>,
    /// The thread that asks git about work trees, as a pane works in a
    /// directory it has not asked about.
    pub git: SyncSender<()>,
    /// The thread that asks gh about pull requests, as a branch is checked
    /// out where a pane works that was not before.
    pub github: SyncSender<()>,
}

/// The most commands that wait for the core. A watcher has one waiting at a
/// time at most, as it waits for room; the rest are the API's, which tells a
/// client that the daemon is busy rather than wait.
const COMMANDS_MAX: usize = 64;

/// The most clients that wait at once for the host to change. A sidebar is
/// one, for each client of each zellij session.
const WAITING_MAX: usize = 256;

/// Asks the core, from a thread of its own, and waits for its answer: none
/// once the core has stopped.
pub fn ask<T>(
    core: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    core.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
}

/// Starts the core, which runs until no one is left to send it a command.
pub fn start(wakes: Wakes) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands, &wakes))?;
    Ok(send)
}

/// What the core knows.
struct Known {
    found: Scan,
    sessions: Sessions,
    /// How many of zellij's folders and sockets the last scan could not read.
    unread_sockets: u32,
    /// What zellij last listed of each session's panes.
    panes: HashMap<Id, Vec<Pane>>,
    /// What zellij last listed of each session's clients, by their ids.
    clients: HashMap<Id, Vec<Client>>,
    /// What git last said of each directory a pane works in.
    trees: BTreeMap<String, WorkTree>,
    /// What gh last found of the branch in each directory a pane works in.
    pull_requests: BTreeMap<String, Found>,
    /// What the shells with the hook last said, each in the session it
    /// started in.
    shells: Vec<Shell>,
    /// The host as the API gives it, its version with it.
    host: api::Host,
    /// How many times the host has changed.
    seq: u64,
    /// The clients that wait for the host to change.
    waiting: Vec<oneshot::Sender<Option<api::Host>>>,
}

fn run(commands: &Receiver<Command>, wakes: &Wakes) {
    let mut known = Known {
        found: Scan { agents: Vec::new(), unread: 0 },
        sessions: Sessions::default(),
        unread_sockets: 0,
        panes: HashMap::new(),
        clients: HashMap::new(),
        trees: BTreeMap::new(),
        pull_requests: BTreeMap::new(),
        shells: Vec::new(),
        host: api::Host::default(),
        seq: 0,
        waiting: Vec::with_capacity(WAITING_MAX),
    };
    known.host = api::Host { version: Some(known.version()), ..known.built() };
    for command in commands {
        match command {
            Command::Agents(now) => {
                // The adapter keys its files by the pid each is named for.
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                known.found = now;
            },
            Command::Sockets { sockets, unread } => {
                known.unread_sockets = unread;
                if known.saw(sockets) {
                    wake(&wakes.panes);
                }
            },
            Command::Panes { session, name, panes } => {
                // One that went, took another name, or now shares it, since
                // zellij was asked.
                if known.sessions.addressed(&name) != Some(session) {
                    continue;
                }
                if known.listed(session, panes) {
                    wake(&wakes.git);
                }
            },
            Command::Clients { session, name, clients } => {
                if known.sessions.addressed(&name) != Some(session) {
                    continue;
                }
                // `clients::parse` lists each once, by its id.
                assert!(clients.windows(2).all(|pair| pair[0].id < pair[1].id));
                known.clients.insert(session, clients);
            },
            Command::WorkTree { dir, tree } => {
                // One that no pane works in since git was asked.
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
            Command::Shells(now) => known.shells = now,
            Command::Sessions(answer) => {
                // One who stopped waiting has no use for the answer.
                let _ = answer.send(known.addressed().into_iter().collect());
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
            Command::List(answer) => {
                let agents = known.agents();
                let _ = answer.send(api::ListAgentsResponse { agents, unread: known.found.unread });
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

/// Wakes a thread that asks something on a sweep. One that is woken already
/// has nothing to gain from a second word, and one that has ended hears none.
fn wake(thread: &SyncSender<()>) {
    match thread.try_send(()) {
        Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {},
    }
}

impl Known {
    /// The sessions zellij can be asked about by name: each that runs under a
    /// name no other runs under, and that name.
    fn addressed(&self) -> HashSet<(Id, String)> {
        let sessions = &self.sessions;
        let alone = |&(id, name): &(Id, &str)| sessions.addressed(name) == Some(id);
        sessions.running().filter(alone).map(|(id, name)| (id, name.to_owned())).collect()
    }

    /// Takes in the sessions that run now. A session that ended takes its
    /// panes and clients with it, and so does one that now shares its name:
    /// zellij is not asked about it while it does, and what it listed would
    /// only go stale. One renamed to a name of its own keeps them. True when
    /// zellij can be asked about a session it could not be before: one that
    /// started, or took a name, or has one another session no longer shares.
    fn saw(&mut self, sockets: Vec<Socket>) -> bool {
        let before = self.addressed();
        self.sessions.saw(sockets);
        let now = self.addressed();
        let addressed = |session: &Id| now.iter().any(|(id, _)| id == session);
        self.panes.retain(|session, _| addressed(session));
        self.clients.retain(|session, _| addressed(session));
        !now.is_subset(&before)
    }

    /// Takes in what zellij listed of the panes of `session`, which runs, and
    /// keeps what it said before where it says nothing now. True when a pane
    /// works in a directory that none did.
    fn listed(&mut self, session: Id, mut panes: Vec<Pane>) -> bool {
        assert!(self.sessions.name_of(session).is_some());
        // `keep` finds the pane each was by its id: zellij gives each terminal
        // of a session one of its own, which `panes::parse` checks.
        let ids: HashSet<u32> = panes.iter().map(|pane| pane.id).collect();
        assert_eq!(ids.len(), panes.len(), "a pane's id comes twice");
        if let Some(listed) = self.panes.get(&session) {
            panes::keep(&mut panes, listed);
        }
        let before = self.dirs();
        self.panes.insert(session, panes);
        !self.dirs().is_subset(&before)
    }

    /// The directories the panes work in.
    fn dirs(&self) -> BTreeSet<String> {
        let cwds = self.panes.values().flatten().filter_map(|pane| pane.cwd.as_ref());
        cwds.map(|cwd| cwd.as_str().to_owned()).collect()
    }

    /// Lets go of what git and gh said of a directory no pane works in now.
    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.trees.retain(|dir, _| dirs.contains(dir));
        self.pull_requests.retain(|dir, _| dirs.contains(dir));
    }

    /// The branch checked out in `dir`, where git has said one is.
    fn branch(&self, dir: &str) -> Option<&str> {
        match self.trees.get(dir)? {
            WorkTree::Checkout { head: Head::Branch { name, .. }, .. } => Some(name.as_str()),
            WorkTree::Checkout { head: Head::Detached, .. } | WorkTree::Outside => None,
        }
    }

    /// The branch checked out in each directory, where git has said one is.
    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &String| Some((dir.clone(), self.branch(dir)?.to_owned()));
        self.trees.keys().filter_map(branch).collect()
    }

    fn agents(&self) -> Vec<api::Agent> {
        self.found.agents.iter().map(|agent| agent.to_api(&self.sessions)).collect()
    }

    /// The shell of each pane, by its session and its id: of the shells in
    /// it, the one that started last, as a shell started in another runs in
    /// front of it. A shell is in the session its session's name leads to, as
    /// an agent is, or, of more than one, in the one that has that name now;
    /// and one that started before a session took that name is none of that
    /// session's.
    fn shells(&self) -> HashMap<(Id, u32), &Shell> {
        let mut shells: HashMap<(Id, u32), &Shell> = HashMap::new();
        for shell in &self.shells {
            // The hook counts whole seconds: the shell started by the end of
            // the one it says.
            let by = shell.started.checked_add(Duration::from_secs(1));
            let Some(session) = by.and_then(|by| self.sessions.housing(&shell.named, by)) else {
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

    /// The host as the API gives it, all but its version, which `update`
    /// counts. Each list is in an order of its own, so that a host that has
    /// not changed is built the same.
    fn built(&self) -> api::Host {
        // The panes and clients of a session zellij cannot be asked about
        // would be left out, or go stale, without a word: `saw` lets them go
        // as it ends or comes to share its name, and `run` takes in none for
        // it.
        let sessions = &self.sessions;
        for session in self.panes.keys().chain(self.clients.keys()) {
            let name = sessions.name_of(*session).expect("a session zellij listed runs");
            assert_eq!(sessions.addressed(name), Some(*session));
        }
        // By name, and those that share one, as sessions of zellij versions
        // with contracts of their own can, by when each started.
        let mut running: Vec<(Id, &str)> = self.sessions.running().collect();
        running.sort_unstable_by_key(|&(id, name)| (name, id.started()));
        let shells = self.shells();
        let sessions = running
            .into_iter()
            .map(|(id, name)| api::Session {
                name: name.to_owned(),
                started_at: Some(id.started().into()),
                socket: id.socket(),
                panes: self.panes.get(&id).map(|panes| api::Panes {
                    panes: panes
                        .iter()
                        .map(|pane| pane.to_api(shells.get(&(id, pane.id)).copied()))
                        .collect(),
                }),
                clients: self.clients.get(&id).map(|clients| api::Clients {
                    clients: clients.iter().copied().map(Client::to_api).collect(),
                }),
            })
            .collect();
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        // What gh found of a branch that is no longer checked out where it
        // was asked about is not that directory's, until gh has said of the
        // one that is.
        let pull_requests = self
            .pull_requests
            .iter()
            .filter(|(dir, found)| self.branch(dir) == Some(&found.branch))
            .map(|(dir, found)| found.pull_request.to_api(dir))
            .collect();
        api::Host {
            version: None,
            sessions,
            work_trees: self.trees.iter().map(|(dir, tree)| tree.to_api(dir)).collect(),
            agents,
            unread: self.found.unread,
            pull_requests,
            unread_sockets: self.unread_sockets,
        }
    }

    fn version(&self) -> api::Version {
        api::Version { pid: std::process::id(), seq: self.seq }
    }

    /// Counts a change to the host, if there is one, and answers everyone who
    /// waits for one. What git said of a directory no pane works in now goes
    /// first.
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
            // One who stopped waiting has no use for the answer.
            let _ = waiting.send(Some(self.host.clone()));
        }
    }

    fn watch(&mut self, after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>>) {
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
