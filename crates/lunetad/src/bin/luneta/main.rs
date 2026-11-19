mod actions;
mod agents;
mod alerts;
mod aloud;
mod answers;
mod ask;
mod attached;
mod away;
mod bridge;
mod brief;
mod browser;
mod checks;
mod clipboard;
mod close;
mod commands;
mod daemon;
mod desktop;
mod dictation;
mod due;
mod drive;
mod folds;
mod gruvbox;
mod host;
mod hush;
mod icons;
mod images;
mod input;
mod issue;
mod ledger;
mod live;
mod machines;
mod menu;
mod mic;
mod models;
mod named;
mod news;
mod peers;
mod picker;
mod printed;
mod projected;
mod prune;
mod pull;
mod screen;
mod sidebar;
mod skills;
mod speaker;
mod speech;
mod spots;
mod start;
mod state;
mod sweep;
mod terminate;
mod viewed;
mod voice;
mod watch;
mod width;
mod worktrees;
mod zellij;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lunetad::{folder, release};

use crate::zellij::SessionName;

#[derive(Parser)]
#[command(
    name = "luneta",
    version = release::SAID.as_str(),
    about = "Sessions of luneta's own multiplexer, beside a sidebar of every session on this \
             host and what the agent in each pane is doing. With a command, ask lunetad, the \
             daemon of this host, instead, or have it open, move or close something"
)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[arg(
        help = "The session to attach to, made in this directory if none runs by that name. \
                Without one, luneta joins a session that runs: the one named for this directory, \
                as the picker names it, or else the one you looked at last, or else the newest. \
                It makes the one named for this directory only where none runs. A name a \
                session of zellij's has is refused"
    )]
    session: Option<String>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Asked(commands::Command),
    #[command(
        hide = true,
        about = "Make a worktree of a branch, or of a pull request by its link, in a repository \
                 `wt clone` made, as `wt add` does, and print where it is, or where one of the \
                 branch is already. luneta makes the worktree of a link pasted in its sidebar so, \
                 with wt's code, and no `wt` on the `PATH`. Why it made none comes last on \
                 stderr, after `luneta: `"
    )]
    Worktree {
        #[arg(long, value_name = "DIR", help = "A directory of the repository")]
        repo: String,
        #[arg(
            value_parser = pull::Of::parse,
            value_name = "BRANCH",
            help = "The branch, or the link of a pull request on GitHub, with or without its \
                    `https://`"
        )]
        of: pull::Of,
    },
    #[command(
        hide = true,
        about = "Bring a worktree to its upstream branch, as `wt follow` does. Why it did not \
                 comes last on stderr, after `luneta: `"
    )]
    Follow {
        #[arg(value_name = "DIR")]
        dir: String,
    },
    #[command(
        hide = true,
        about = "Fetch the upstream branch of a branch of the repository a directory is in, as \
                 `wt follow` fetches it, and move no branch. lunetad has it fetched so, for the \
                 count of how far behind it a branch is. What wt found of it is one word on \
                 stdout; why it did not fetch it comes last on stderr, after `luneta: `"
    )]
    FetchUpstream {
        #[arg(value_name = "DIR")]
        dir: String,
        #[arg(value_name = "BRANCH")]
        branch: String,
    },
    #[command(
        hide = true,
        about = "Tell the luneta listening on a socket that its notification of a group was \
                 clicked, as a click on one runs on macOS"
    )]
    Clicked {
        #[arg(long, value_name = "PATH")]
        socket: PathBuf,
        group: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let folder: Result<PathBuf, String> = folder::find().map_err(|unfound| format!("{unfound}, to find lunetad in"));
    let Some(command) = cli.command else {
        let session = match cli.session.as_deref().map(SessionName::parse).transpose() {
            Ok(session) => session,
            Err(refusal) => {
                eprintln!("luneta: no such session to make: {refusal}");
                return ExitCode::FAILURE;
            },
        };
        return host::run(session.as_ref(), folder);
    };
    let command = match command {
        Command::Worktree { repo, of } => return pull::make(&repo, &of),
        Command::Follow { dir } => return pull::follow(&dir),
        Command::FetchUpstream { dir, branch } => return pull::fetch_upstream(&dir, &branch),
        Command::Clicked { socket, group } => {
            return match peers::tell(&socket, &group) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("luneta: {}: {error}", socket.display());
                    ExitCode::FAILURE
                },
            };
        },
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
