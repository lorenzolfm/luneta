//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

use std::collections::HashSet;
use std::io;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

use tokio::sync::oneshot;

use crate::api;
use crate::claude::Scan;
use crate::zellij::{Sessions, Socket};

pub enum Command {
    /// What Claude Code's files say now.
    Agents(Scan),
    /// The zellij sessions that run now.
    Sockets(Vec<Socket>),
    /// Asks for every agent, as the API gives it.
    List(oneshot::Sender<api::ListAgentsResponse>),
}

/// The most commands that wait for the core. A watcher has one waiting at a
/// time at most, as it waits for room; the rest are the API's, which tells a
/// client that the daemon is busy rather than wait.
const COMMANDS_MAX: usize = 64;

/// Starts the core, which runs until no one is left to send it a command.
pub fn start() -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands))?;
    Ok(send)
}

fn run(commands: &Receiver<Command>) {
    let mut found = Scan { agents: Vec::new(), unread: 0 };
    let mut sessions = Sessions::default();
    for command in commands {
        match command {
            Command::Agents(now) => {
                // The adapter keys its files by the pid each is named for.
                let pids: HashSet<u32> = now.agents.iter().map(|agent| agent.pid).collect();
                assert_eq!(pids.len(), now.agents.len(), "an agent's pid comes twice");
                found = now;
            },
            Command::Sockets(now) => sessions.saw(now),
            Command::List(answer) => {
                let agents = found.agents.iter().map(|agent| agent.to_api(&sessions)).collect();
                // One who stopped waiting has no use for the answer.
                let _ = answer.send(api::ListAgentsResponse { agents, unread: found.unread });
            },
        }
    }
}
