//! luneta: zellij, beside a sidebar of every session on this host and what
//! runs in each, and the CLI that asks lunetad, the daemon of this host.

mod answers;
mod close;
mod commands;
mod daemon;
mod folds;
mod gruvbox;
mod host;
mod hosted;
mod icons;
mod input;
mod keys;
mod ledger;
mod menu;
mod news;
mod own;
mod sidebar;
mod skills;
mod state;
mod watch;
mod zellij;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use lunetad::folder;

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
    let folder: Result<PathBuf, String> = folder::find()
        .ok_or_else(|| "neither XDG_RUNTIME_DIR nor TMPDIR is set, to find lunetad in".to_owned());
    let Some(command) = cli.command else {
        let session = match cli.session.as_deref().map(SessionName::parse).transpose() {
            Ok(session) => session,
            Err(refusal) => {
                eprintln!("luneta: no such session to make: {refusal}");
                return ExitCode::FAILURE;
            },
        };
        return host::run(session, folder);
    };
    match folder {
        Ok(folder) => commands::run(&command, &folder),
        Err(why) => {
            eprintln!("luneta: {why}");
            ExitCode::FAILURE
        },
    }
}
