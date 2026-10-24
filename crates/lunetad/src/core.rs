//! The core: the one thread that holds what the daemon knows, and the only one
//! that changes it. The others hand it a command and, if they asked something,
//! wait for its answer. So nothing changes what another is reading, and the
//! commands take effect in the order they came in.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use tokio::sync::oneshot;

use crate::agent::Agent;
use crate::api;
use crate::zellij::{Sessions, Socket};

pub enum Command {
    /// What Claude Code's files say now.
    Agents(Vec<Agent>),
    /// The zellij sessions that run now.
    Sockets(Vec<Socket>),
    /// Asks for every agent, as the API gives it.
    List(oneshot::Sender<Vec<api::Agent>>),
}

/// Starts the core, which runs until no one is left to send it a command.
pub fn start() -> io::Result<Sender<Command>> {
    let (send, commands) = mpsc::channel();
    thread::Builder::new().name("core".to_owned()).spawn(move || run(&commands))?;
    Ok(send)
}

fn run(commands: &Receiver<Command>) {
    let mut agents = Vec::new();
    let mut sessions = Sessions::default();
    for command in commands {
        match command {
            Command::Agents(now) => agents = now,
            Command::Sockets(now) => sessions.saw(now),
            Command::List(answer) => {
                // One who stopped waiting has no use for the answer.
                let _ = answer.send(agents.iter().map(|agent| agent.to_api(&sessions)).collect());
            },
        }
    }
}
