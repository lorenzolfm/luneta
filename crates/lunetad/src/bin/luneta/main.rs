//! luneta: zellij, beside a sidebar of every session on this host and what
//! runs in each, and the CLI that asks lunetad, the daemon of this host, and
//! has zellij open, move and close what the sidebar's menu does, for the
//! agent of its ask box among others.

mod answers;
mod ask;
mod brief;
mod close;
mod commands;
mod daemon;
mod dirs;
mod due;
mod drive;
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
mod order;
mod own;
mod picker;
mod printed;
mod pull;
mod sidebar;
mod skills;
mod spots;
mod state;
mod watch;
mod width;
mod zellij;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lunetad::{folder, release};

use crate::zellij::SessionName;

/// zellij, beside a sidebar of every session on this host and what the agent
/// in each pane is doing. With a command, ask lunetad, the daemon of this
/// host, instead, or have zellij open, move or close something.
#[derive(Parser)]
// Named, or clap would name it for its package, and `--version` would say
// `lunetad`.
#[command(name = "luneta", version = release::SAID.as_str())]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    /// The session to attach to, made if none runs by that name. Without
    /// one, the session named for this directory, as the picker names it.
    session: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

/// What luneta does with a command, in the place of zellij: what the CLI asks
/// lunetad, or has zellij do, or a worktree, which needs neither.
#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Asked(commands::Command),
    /// Make a worktree of a branch, or of a pull request by its link, in a
    /// repository `wt clone` made, as `wt add` does, and print where it is, or
    /// where one of the branch is already. luneta makes the worktree of a
    /// link pasted in its sidebar so, with wt's code, and no `wt` on the
    /// `PATH`. Why it made none comes last on stderr, after `luneta: `.
    #[command(hide = true)]
    Worktree {
        /// A directory of the repository.
        #[arg(long, value_name = "DIR")]
        repo: String,
        /// The branch, or the link of a pull request on GitHub, with or
        /// without its `https://`.
        #[arg(value_parser = pull::Of::parse, value_name = "BRANCH")]
        of: pull::Of,
    },
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
    let command = match command {
        // It asks no lunetad, and so needs no folder to find one in.
        Command::Worktree { repo, of } => return pull::make(&repo, &of),
        Command::Asked(command) => command,
    };
    match folder {
        Ok(folder) => commands::run(&command, &folder),
        Err(why) => {
            eprintln!("luneta: {why}");
            ExitCode::FAILURE
        },
    }
}
