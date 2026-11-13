//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

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

/// A zellij session zellij was asked about, as it was asked: by the name it
/// had then, which a rename moves to another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub id: Id,
    pub name: SessionName,
}

/// An own session, as its server lists it: whole, as it lists every session
/// each time one changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Own {
    pub id: OwnId,
    /// When its server made it, which tells it from the sessions of another
    /// server with its id.
    pub made: OwnMade,
    pub name: SessionName,
    pub panes: Vec<Pane>,
    pub clients: Listed,
    /// What the shell of each pane said, by the pane's id, as its server
    /// says it.
    pub doings: BTreeMap<u32, Doing>,
}

/// A branch, by the directory that holds its repository, and its name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Branch {
    pub repository: Said,
    pub name: Said,
}

pub enum Command {
    /// What Claude Code's files say now.
    Agents(Scan),
    /// What the files of luneta's extension for pi say now.
    PiAgents(Vec<Agent>),
    /// The zellij sessions that run now, and how many of zellij's folders and
    /// sockets could not be read, any of which could be another's.
    Sockets { sockets: Vec<Socket>, unread: u32 },
    /// What zellij listed of the panes of the session it was `asked` about.
    Panes { asked: Asked, panes: Vec<Pane> },
    /// What zellij listed of the clients of the session it was `asked` about.
    Clients { asked: Asked, clients: Listed },
    /// The own sessions that run now, as their server lists them, in the order
    /// it made them: none where none runs.
    Owns(Vec<Own>),
    /// What git said of the work tree in `dir`.
    WorkTree { dir: String, tree: WorkTree },
    /// The pull request gh found last of the branch in each directory, by
    /// the directory.
    PullRequests(BTreeMap<String, Found>),
    /// What the shells with luneta's hook say now.
    Shells(Vec<Shell>),
    /// The topic of each agent that has one, by its session's id.
    Topics(HashMap<SessionId, Said>),
    /// What the hosts file names now.
    Hosts(Named),
    /// What the watch of `destination` has of it now.
    Remote { destination: Destination, reached: Reached },
    /// Takes what `Say` was asked, at `at`.
    Say { said: ToSay, at: SystemTime, answer: oneshot::Sender<()> },
    /// Asks for the sessions zellij can be asked about, for their panes.
    Sessions(oneshot::Sender<Vec<Addressed>>),
    /// Asks for the directories the panes work in, for git to be asked about.
    Dirs(oneshot::Sender<BTreeSet<String>>),
    /// Asks for the branch checked out in each directory a pane works in, by
    /// the directory, where git has said one is, for gh to be asked about.
    Branches(oneshot::Sender<BTreeMap<String, String>>),
    /// Asks for the branch checked out in each directory a pane works in that
    /// has an upstream, by the directory, where git has said one is, for its
    /// upstream to be fetched.
    Tracking(oneshot::Sender<BTreeMap<String, Branch>>),
    /// Asks for Claude Code's agents, for their topics.
    Claudes(oneshot::Sender<Vec<Agent>>),
    /// Asks for every agent, as the API gives it.
    List(oneshot::Sender<api::ListAgentsResponse>),
    /// Asks for the host once it is no longer as `after` saw it: at once if it
    /// is not, or `after` is none. `None` for an answer is too many asking.
    Watch { after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>> },
}

/// A session zellij can be asked about, for its panes.
pub struct Addressed {
    pub id: Id,
    /// The name to ask by.
    pub name: SessionName,
    /// The last word said of it, as the core counts the words said of every
    /// session: one as zellij could first be asked about it by its name, and
    /// one each time a shell in it runs a command, which can change where its
    /// pane works and what runs in it.
    pub told: u64,
    /// Whether the host has its panes, as zellij last listed them.
    pub listed: bool,
}

/// Who the core wakes, as the host changes in a way that has them ask
/// something at once rather than on their next sweep.
pub struct Wakes {
    /// The thread that asks zellij for panes, as it can be asked about a
    /// session it could not be before, or a shell in one says a word.
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
    /// The agents pi's adapter found.
    pi: Vec<Agent>,
    sessions: Sessions,
    /// How many of zellij's folders and sockets the last scan could not read.
    unread_sockets: u32,
    /// The own sessions that run, whole, in the order their server made them.
    owns: Vec<Own>,
    /// The names a zellij session and an own one both have, as was last said.
    shared: BTreeSet<SessionName>,
    /// What zellij last listed of each of its sessions' panes.
    panes: HashMap<Id, Vec<Pane>>,
    /// What zellij last listed of each of its sessions' clients, by their ids.
    clients: HashMap<Id, Listed>,
    /// What git last said of each directory a pane works in.
    trees: BTreeMap<String, WorkTree>,
    /// What gh last found of the branch in each directory a pane works in.
    pull_requests: BTreeMap<String, Found>,
    /// What the shells with the hook last said, each in the session it
    /// started in.
    shells: Vec<Shell>,
    /// The topic the daemon keeps of each agent that has one, by its session's
    /// id.
    topics: HashMap<SessionId, Said>,
    /// What the daemon has of each other machine the hosts file names, in its
    /// order.
    remotes: Vec<(Destination, Reached)>,
    /// How much of the hosts file named no machine as it was last read.
    unread_hosts: u32,
    /// How many words have been said of the sessions: one as zellij could
    /// first be asked about each by its name, and one each time a shell in one
    /// says a new line, or goes.
    words: u64,
    /// The last word said of each session that zellij can be asked about.
    told: HashMap<Id, u64>,
    /// The host as the API gives it, its version with it.
    host: api::Host,
    /// How many times the host has changed.
    seq: u64,
    /// The clients that wait for the host to change.
    waiting: Vec<oneshot::Sender<Option<api::Host>>>,
    /// What `Say` last took.
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
                // The adapter keys its files by the pid each is named for.
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                known.found = now;
            },
            Command::PiAgents(now) => {
                // The adapter keys its files by the pid each is named for.
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
                // One that went, took another name, or now shares it, since
                // its multiplexer was asked.
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
            Command::Shells(now) => {
                if known.heard(now) {
                    wake(&wakes.panes);
                }
            },
            Command::Topics(now) => known.topics = now,
            Command::Hosts(now) => known.named(now),
            Command::Remote { destination, reached } => {
                // One the file no longer names, since its watch heard it.
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
                // One who stopped waiting has no use for the answer.
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

/// A session as `built` lists it, of either multiplexer.
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

    /// Where it goes among the sessions of the host: by its name, zellij's
    /// first where one of each shares it, and zellij's that share one by when
    /// each started.
    fn ranked(self) -> (&'a SessionName, Option<OwnId>, Option<SystemTime>) {
        match self {
            Row::Zellij(id, name) => (name, None, Some(id.started())),
            Row::Own(own) => (&own.name, Some(own.id), None),
        }
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
    /// Whether the session `asked` is still the one it was asked as.
    fn answers(&self, asked: &Asked) -> bool {
        self.sessions.addressed(&asked.name) == Some(asked.id)
    }

    /// The sessions zellij can be asked about by name: each that runs under a
    /// name no other runs under, and that name.
    fn addressed(&self) -> HashSet<(Id, SessionName)> {
        let sessions = &self.sessions;
        let alone = |&(id, name): &(Id, &SessionName)| sessions.addressed(name) == Some(id);
        sessions.running().filter(alone).map(|(id, name)| (id, name.clone())).collect()
    }

    /// Takes in the sessions that run now. A session that ended takes its
    /// panes and clients with it, and so does one that now shares its name:
    /// zellij is not asked about it while it does, and what it listed would
    /// only go stale. One renamed to a name of its own keeps them. True when
    /// zellij can be asked about a session it could not be before: one that
    /// started, or took a name, or has one another session no longer shares.
    /// A word is said of each, so that the panes thread asks about it however
    /// lately it did under another name, or before it came to share one.
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

    /// Takes in the own sessions that run now, as their server lists them,
    /// with their panes and clients. A session it no longer lists takes them
    /// with it: it ended, or its server did. True when a pane works in a
    /// directory that none did.
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

    /// Says each name that a zellij session and an own one have both come to
    /// have, once: a question by that name goes to neither, as `Called::named`
    /// refuses it, and zellij makes sessions by names it alone checks.
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

    /// Takes in what the shells with luneta's hook say now, each in a zellij
    /// session: an own session's server says what its shells say. A shell that
    /// says what it did not, or has gone, says a word in the session it is in,
    /// if zellij can be asked about that one. True when one did.
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

    /// Takes in what the hosts file names now. A machine it named before
    /// keeps what the daemon has of it, and one new to it is one the daemon
    /// is reaching.
    fn named(&mut self, now: Named) {
        // `remote::named` names each once.
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

    /// Says a word of `session`, which zellij can be asked about.
    fn say(&mut self, session: Id) {
        self.words = self.words.checked_add(1).expect("fewer than 2^64 words are said");
        self.told.insert(session, self.words);
    }

    /// The session `shell` is in, as `Sessions::housing` has it. The hook
    /// counts whole seconds: the shell started by the end of the one it says.
    fn house(&self, shell: &Shell) -> Option<Id> {
        let by = shell.started.checked_add(Duration::from_secs(1))?;
        self.sessions.housing(&shell.named, by)
    }

    /// Takes in what zellij listed of the panes of `session`, which runs, and
    /// keeps what it said before where it says nothing now. True when a pane
    /// works in a directory that none did.
    fn listed(&mut self, session: Id, mut panes: Vec<Pane>) -> bool {
        assert!(self.sessions.name_of(session).is_some());
        // `keep` finds the pane each was by its id: zellij gives each terminal
        // of a session one of its own, which `panes::terminals` checks.
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
        let owns = self.owns.iter().flat_map(|own| &own.panes);
        let cwds = self.panes.values().flatten().chain(owns).filter_map(|pane| pane.cwd.as_ref());
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
            WorkTree::Checkout { head: Head::Detached, .. }
            | WorkTree::NoCheckout { .. }
            | WorkTree::Outside => None,
        }
    }

    /// The branch checked out in each directory, where git has said one is.
    fn branches(&self) -> BTreeMap<String, String> {
        let branch = |dir: &String| Some((dir.clone(), self.branch(dir)?.to_owned()));
        self.trees.keys().filter_map(branch).collect()
    }

    /// The branch checked out in each directory, as `Tracking` asks for it.
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

    /// Each agent, of whichever harness.
    fn agents(&self) -> Vec<api::Agent> {
        let claude = self.found.agents.iter().map(|agent| {
            let topic = self.topics.get(agent.session_id.as_str()).cloned();
            agent.to_api(&self.sessions, topic)
        });
        claude.chain(self.pi.iter().map(|agent| agent.to_api(&self.sessions, None))).collect()
    }

    /// How many session files do not read, and each, as the API gives them:
    /// as many of one as of the other, in the order of their pids.
    fn unread(&self) -> (u32, Vec<api::UnreadFile>) {
        let mut files: Vec<api::UnreadFile> =
            self.found.unread.iter().map(|file| file.to_api(&self.sessions)).collect();
        files.sort_by_key(|file| file.pid);
        let count =
            u32::try_from(files.len()).expect("a folder listed in memory has under 2^32 files");
        (count, files)
    }

    /// The shell with luneta's hook of each pane of zellij's, by its session
    /// and its id: of the shells in it, the one that started last, as a shell
    /// started in another runs in front of it. A shell is in the session its
    /// session's name leads to, as an agent is, or, of more than one, in the
    /// one that has that name now; and one that started before a session took
    /// that name is none of that session's.
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

    /// Asserts that what the core keeps of zellij's sessions holds together:
    /// the panes and clients of a session zellij cannot be asked about would
    /// be left out, or go stale, without a word. `saw` lets them go as it ends
    /// or comes to share its name, and `run` takes in none for it. `saw` says
    /// a word of each as zellij can be asked about it, and lets the word go as
    /// it no longer can be. An own session needs none, as its server says each
    /// change.
    fn assert_listed(&self) {
        for id in self.panes.keys().chain(self.clients.keys()) {
            let name = self.sessions.name_of(*id).expect("a session zellij listed runs");
            assert_eq!(self.sessions.addressed(name), Some(*id));
        }
        let addressed = self.addressed();
        assert_eq!(self.told.len(), addressed.len());
        assert!(addressed.iter().all(|(id, _)| self.told.contains_key(id)));
    }

    /// The host as the API gives it, all but its version, which `update`
    /// counts. Each list is in an order of its own, so that a host that has
    /// not changed is built the same.
    fn built(&self) -> api::Host {
        self.assert_listed();
        let sessions = self.built_sessions();
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        let (unread, unread_files) = self.unread();
        // What gh found of a branch that is no longer checked out where it
        // was asked about is not that directory's, until gh has said of the
        // one that is.
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

    /// The sessions as `built` gives them, in the order they rank in.
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
        // What was said too long ago goes before anyone hears it, as a daemon
        // that slept has not swept since.
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
