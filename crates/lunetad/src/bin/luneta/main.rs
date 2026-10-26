//! luneta: zellij, beside a sidebar of every session on this host and what
//! runs in each, and the CLI that asks lunetad, the daemon of this host.

mod commands;
mod host;
mod hosted;
mod icons;
mod keys;
mod sidebar;
mod watch;
mod zellij;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use lunetad::folder::{self, SOCKET};

use crate::commands::Command;
use crate::zellij::SessionName;

/// zellij, beside a sidebar of every session on this host and what the agent
/// in each pane is doing. With a command, ask lunetad, the daemon of this
/// host, instead.
#[derive(Parser)]
#[command(version, args_conflicts_with_subcommands = true)]
struct Cli {
    /// The session to attach to, made if none runs by that name. Without
    /// one, the session named for this directory, as the picker names it.
    session: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let socket = socket();
    let Some(command) = cli.command else {
        let session = match cli.session.as_deref().map(SessionName::parse).transpose() {
            Ok(session) => session,
            Err(refusal) => {
                eprintln!("luneta: no such session to make: {refusal}");
                return ExitCode::FAILURE;
            },
        };
        return host::run(session, socket);
    };
    match socket {
        Ok(socket) => commands::run(&command, &socket),
        Err(why) => {
            eprintln!("luneta: {why}");
            ExitCode::FAILURE
        },
    }
}

/// The socket the daemon listens on, or why luneta will not look for it there:
/// in a folder that is not this user's alone, whoever has it could be
/// listening in the daemon's place.
fn socket() -> Result<PathBuf, String> {
    let folder = folder::find()
        .ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in")?;
    folder::check(&folder).map_err(|refusal| refusal.to_string())?;
    Ok(folder.join(SOCKET))
}
