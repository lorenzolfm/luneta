mod actions;
mod agreed;
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
mod copied;
mod daemon;
mod desktop;
mod dictation;
mod due;
mod folds;
mod gruvbox;
mod home;
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
mod rebase;
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

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lunetad::{folder, release};

#[derive(Parser)]
#[command(
    name = "luneta",
    version = release::SAID.as_str(),
    about = "Sessions of luneta's own multiplexer, beside a sidebar of every session on this \
             host and what the agent in each view is doing. With a command, ask lunetad, the \
             daemon of this host, instead, or have it start or delete something"
)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[arg(
        help = "A project's name, to show it wherever luneta starts. In a project of a clone wt \
                made, a name no project has is a branch: luneta shows its worktree, made as \
                `n w` makes one if there is none. Outside every project, a project of that name \
                is made of this directory. Without a name, luneta shows the worktree of this \
                directory, or else the one you used last"
    )]
    name: Option<String>,
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
        return host::run(cli.name.as_deref(), folder);
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;
    use lunetad::asks::SKILL;

    use super::*;

    fn commands_named(skill: &str) -> Vec<String> {
        let mut named = Vec::new();
        let mut prose = String::new();
        let mut fence: Option<&str> = None;
        for line in skill.lines() {
            match (fence, line.trim_start().strip_prefix("```")) {
                (None, Some(info)) => fence = Some(info.trim()),
                (Some(_), Some(_)) => fence = None,
                (Some("sh"), None) => named.push(line.split(" # ").next().unwrap_or(line).to_owned()),
                (Some(_), None) => {},
                (None, None) => {
                    prose.push_str(line);
                    prose.push('\n');
                },
            }
        }
        assert!(fence.is_none(), "the skill leaves a code block open");
        named.extend(prose.split('`').skip(1).step_by(2).map(|span| span.replace('\n', " ")));
        let named = named.into_iter().map(|said| said.trim().to_owned());
        named.filter(|said| said.starts_with("luneta ")).collect()
    }

    fn unlisted(skill: &str) -> Vec<String> {
        let mut cli = Cli::command();
        cli.build();
        let mut unlisted = Vec::new();
        for said in commands_named(skill) {
            let mut words = said.split_whitespace().skip(1);
            let lowercase = |word: &&str| word.starts_with(|c: char| c.is_ascii_lowercase());
            let Some(verb) = words.next().filter(lowercase) else {
                continue;
            };
            let Some(command) = cli.find_subcommand(verb).filter(|command| !command.is_hide_set())
            else {
                unlisted.push(format!("luneta {verb}"));
                continue;
            };
            for flag in words.filter_map(|word| word.split_once("--").map(|(_, flag)| flag)) {
                let flag = flag.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').next();
                let Some(flag) = flag.filter(|flag| !flag.is_empty()) else {
                    continue;
                };
                let listed = command.get_arguments().any(|argument| {
                    argument.get_long() == Some(flag) && !argument.is_hide_set()
                });
                if !listed {
                    unlisted.push(format!("luneta {verb} --{flag}"));
                }
            }
        }
        unlisted
    }

    #[test]
    fn the_luneta_skill_names_only_commands_and_flags_luneta_help_lists() {
        let named = commands_named(SKILL);
        assert!(named.len() >= 8, "the skill names {} luneta commands: {named:?}", named.len());
        assert_eq!(unlisted(SKILL), Vec::<String>::new());
        let stale = "Run `luneta\nhere`, then:\n\n```sh\nluneta prune --beside x   # no such \
                     flag\nluneta terminate --here\n```\n\n```text\nluneta go a\n```\n";
        let said = ["luneta prune --beside", "luneta terminate --here", "luneta here"];
        assert_eq!(unlisted(stale), said, "a verb gone, a flag it lacks, and a hidden flag");
    }
}
