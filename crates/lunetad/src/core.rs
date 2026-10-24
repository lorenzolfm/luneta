//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
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

/// Starts the core, which runs until no one is left to send it a command.
pub fn start() -> io::Result<Sender<Command>> {
    let (send, commands) = mpsc::channel();
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands))?;
    Ok(send)
}

fn run(commands: &Receiver<Command>) {
    let mut found = Scan { agents: Vec::new(), unread: 0 };
    let mut sessions = Sessions::default();
    for command in commands {
        match command {
            Command::Agents(now) => found = now,
            Command::Sockets(now) => sessions.saw(now),
            Command::List(answer) => {
                let agents = found.agents.iter().map(|agent| agent.to_api(&sessions)).collect();
                // Past four billion files, the count says as many as it can.
                let unread = u32::try_from(found.unread).unwrap_or(u32::MAX);
                // One who stopped waiting has no use for the answer.
                let _ = answer.send(api::ListAgentsResponse { agents, unread });
            },
        }
    }
}
