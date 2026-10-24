//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::SystemTime;

use tokio::sync::oneshot;

use crate::api;
use crate::claude::Scan;
use crate::git::WorkTree;
use crate::panes::{self, Pane};
use crate::zellij::{Sessions, Socket};

pub enum Command {
    /// What Claude Code's files say now.
    Agents(Scan),
    /// The zellij sessions that run now.
    Sockets(Vec<Socket>),
    /// What zellij listed of the panes of the session called `session`.
    Panes { session: String, panes: Vec<Pane> },
    /// What git said of the work tree in `dir`.
    WorkTree { dir: String, tree: WorkTree },
    /// Asks for the names of the sessions that run, for zellij to be asked
    /// for their panes.
    Sessions(oneshot::Sender<Vec<String>>),
    /// Asks for the directories the panes work in, for git to be asked about.
    Dirs(oneshot::Sender<BTreeSet<String>>),
    /// Asks for every agent, as the API gives it.
    List(oneshot::Sender<api::ListAgentsResponse>),
    /// Asks for the host once it is no longer as `after` saw it: at once if it
    /// is not, or `after` is none. `None` for an answer is too many asking.
    Watch { after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>> },
}

/// Who the core wakes, as the host changes in a way that has them ask
/// something at once rather than on their next sweep.
pub struct Wakes {
    /// The thread that asks zellij for panes, as a session turns up.
    pub panes: SyncSender<()>,
    /// The thread that asks git about work trees, as a pane works in a
    /// directory it has not asked about.
    pub git: SyncSender<()>,
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
    /// What zellij last listed of each session's panes, by the name the
    /// session has now.
    panes: BTreeMap<String, Vec<Pane>>,
    /// What git last said of each directory a pane works in.
    trees: BTreeMap<String, WorkTree>,
    /// The host as the API gives it, all but its version.
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
        panes: BTreeMap::new(),
        trees: BTreeMap::new(),
        host: api::Host::default(),
        seq: 0,
        waiting: Vec::with_capacity(WAITING_MAX),
    };
    known.host = known.built();
    for command in commands {
        match command {
            Command::Agents(now) => {
                // The adapter keys its files by the pid each is named for.
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                known.found = now;
            },
            Command::Sockets(now) => {
                if known.saw(now) {
                    wake(&wakes.panes);
                }
            },
            Command::Panes { session, panes } => {
                // One that went, or took another name, since zellij was asked.
                if !known.running().any(|name| name == session) {
                    continue;
                }
                if known.listed(session, panes) {
                    wake(&wakes.git);
                }
            },
            Command::WorkTree { dir, tree } => {
                // One that no pane works in since git was asked.
                if !known.dirs().contains(&dir) {
                    continue;
                }
                known.trees.insert(dir, tree);
            },
            Command::Sessions(answer) => {
                // One who stopped waiting has no use for the answer.
                let _ = answer.send(known.running().map(str::to_owned).collect());
                continue;
            },
            Command::Dirs(answer) => {
                let _ = answer.send(known.dirs());
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
    /// The names the sessions that run have now.
    fn running(&self) -> impl Iterator<Item = &str> {
        self.sessions.running().map(|(name, _)| name)
    }

    /// Takes in the sessions that run now. A rename keeps the session's panes,
    /// which zellij is asked for under its new name from now on, and a session
    /// that ended takes its panes with it. True when a session turned up.
    fn saw(&mut self, sockets: Vec<Socket>) -> bool {
        let before: BTreeSet<String> = self.running().map(str::to_owned).collect();
        let renamed = self.sessions.saw(sockets);
        let moved: Vec<(String, Vec<Pane>)> = renamed
            .into_iter()
            .filter_map(|(was, now)| Some((now, self.panes.remove(&was)?)))
            .collect();
        self.panes.extend(moved);
        let running: BTreeSet<String> = self.running().map(str::to_owned).collect();
        self.panes.retain(|session, _| running.contains(session));
        !running.is_subset(&before)
    }

    /// Takes in what zellij listed of the panes of `session`, which runs, and
    /// keeps what it said before where it says nothing now. True when a pane
    /// works in a directory that none did.
    fn listed(&mut self, session: String, mut panes: Vec<Pane>) -> bool {
        assert!(self.running().any(|name| name == session));
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
        self.panes.values().flatten().filter_map(|pane| pane.cwd.clone()).collect()
    }

    /// Lets go of what git said of a directory no pane works in now.
    fn forget_dirs(&mut self) {
        let dirs = self.dirs();
        self.trees.retain(|dir, _| dirs.contains(dir));
    }

    fn agents(&self) -> Vec<api::Agent> {
        self.found.agents.iter().map(|agent| agent.to_api(&self.sessions)).collect()
    }

    /// The host as the API gives it, all but its version. Each list is in an
    /// order of its own, so that a host that has not changed is built the
    /// same.
    fn built(&self) -> api::Host {
        // The panes of a session that does not run would be left out without
        // a word: `saw` lets them go as it ends, and `run` takes in none for it.
        for session in self.panes.keys() {
            assert!(self.running().any(|name| name == session));
        }
        // By name, and those that share one, as sessions of zellij versions
        // with contracts of their own can, by when each started.
        let mut running: Vec<(&str, SystemTime)> = self.sessions.running().collect();
        running.sort_unstable();
        let sessions = running
            .into_iter()
            .map(|(name, started)| api::Session {
                name: name.to_owned(),
                started_at: Some(started.into()),
                panes: self.panes.get(name).map(|panes| api::Panes {
                    panes: panes.iter().map(Pane::to_api).collect(),
                }),
            })
            .collect();
        let mut agents = self.agents();
        agents.sort_by_key(|agent| agent.pid);
        api::Host {
            version: None,
            sessions,
            work_trees: self.trees.iter().map(|(dir, tree)| tree.to_api(dir)).collect(),
            agents,
            unread: self.found.unread,
        }
    }

    fn version(&self) -> api::Version {
        api::Version { pid: std::process::id(), seq: self.seq }
    }

    /// The host as the API gives it.
    fn answer(&self) -> api::Host {
        api::Host { version: Some(self.version()), ..self.host.clone() }
    }

    /// Counts a change to the host, if there is one, and answers everyone who
    /// waits for one. What git said of a directory no pane works in now goes
    /// first.
    fn update(&mut self) {
        self.forget_dirs();
        let host = self.built();
        if host == self.host {
            return;
        }
        self.host = host;
        self.seq = self.seq.checked_add(1).expect("a host changes fewer than 2^64 times");
        let answer = self.answer();
        for waiting in self.waiting.drain(..) {
            // One who stopped waiting has no use for the answer.
            let _ = waiting.send(Some(answer.clone()));
        }
    }

    fn watch(&mut self, after: Option<api::Version>, answer: oneshot::Sender<Option<api::Host>>) {
        if after != Some(self.version()) {
            let _ = answer.send(Some(self.answer()));
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
