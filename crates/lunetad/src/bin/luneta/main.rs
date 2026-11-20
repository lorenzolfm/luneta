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

    const BY_THE_USER: &str = "## What the user does in luneta\n";

    fn cli() -> clap::Command {
        let mut cli = Cli::command();
        cli.build();
        cli
    }

    fn listed(cli: &clap::Command) -> impl Iterator<Item = &clap::Command> {
        let visible = cli.get_subcommands().filter(|command| !command.is_hide_set());
        visible.filter(|command| command.get_name() != "help")
    }

    fn lists(command: &clap::Command, flag: &str) -> bool {
        let spelled = |argument: &clap::Arg| argument.get_long() == Some(flag);
        command.get_arguments().any(|argument| spelled(argument) && !argument.is_hide_set())
    }

    fn long(word: &str) -> Option<&str> {
        let (_, flag) = word.split_once("--")?;
        let flag = flag.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').next();
        flag.filter(|flag| !flag.is_empty())
    }

    fn shown(skill: &str) -> Vec<String> {
        let mut shown = Vec::new();
        let mut prose = String::new();
        let mut fence: Option<&str> = None;
        for line in skill.lines() {
            match (fence, line.trim_start().strip_prefix("```")) {
                (None, Some(info)) => {
                    let info = info.trim();
                    let known = matches!(info, "sh" | "text");
                    assert!(known, "the skill opens a {info:?} block, not sh or text");
                    fence = Some(info);
                },
                (Some(_), Some(_)) => fence = None,
                (Some("sh"), None) => {
                    let command = line.split_once(" # ").map_or(line, |(command, _)| command);
                    shown.push(command.to_owned());
                },
                (Some(_), None) => {},
                (None, None) => {
                    prose.push_str(line);
                    prose.push('\n');
                },
            }
        }
        assert!(fence.is_none(), "the skill leaves a code block open");
        let ticks = prose.matches('`').count();
        let paired = ticks.is_multiple_of(2);
        assert!(paired, "the skill's prose has {ticks} backticks, one unpaired");
        shown.extend(prose.split('`').skip(1).step_by(2).map(|span| span.replace('\n', " ")));
        shown.into_iter().map(|said| said.trim().to_owned()).collect()
    }

    fn commands_named(skill: &str) -> Vec<String> {
        shown(skill).into_iter().filter(|said| said.starts_with("luneta ")).collect()
    }

    fn unlisted(skill: &str) -> Vec<String> {
        let cli = cli();
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
            for flag in words.filter_map(long) {
                if !lists(command, flag) {
                    unlisted.push(format!("luneta {verb} --{flag}"));
                }
            }
        }
        for said in shown(skill).iter().filter(|said| said.starts_with("--")) {
            let Some(flag) = long(said) else {
                continue;
            };
            let mut commands = std::iter::once(&cli).chain(listed(&cli));
            if !commands.any(|command| lists(command, flag)) {
                unlisted.push(format!("--{flag}"));
            }
        }
        unlisted
    }

    fn unnamed(skill: &str) -> Vec<String> {
        let named = commands_named(skill);
        let verbs = named.iter().filter_map(|said| said.split_whitespace().nth(1));
        let verbs: Vec<&str> = verbs.collect();
        let cli = cli();
        let verbs_listed = listed(&cli).map(clap::Command::get_name);
        let unnamed = verbs_listed.filter(|verb| !verbs.contains(verb));
        unnamed.map(|verb| format!("luneta {verb}")).collect()
    }

    fn unlabelled(skill: &str) -> Vec<String> {
        let section = skill.split_once(BY_THE_USER).map(|(_, section)| section);
        let section = section.expect("the skill says what the user does in luneta");
        let section = section.split_once("\n## ").map_or(section, |(section, _)| section);
        let marks = section.matches('"').count();
        assert!(marks > 0, "the skill quotes no action of luneta's by name");
        let paired = marks.is_multiple_of(2);
        assert!(paired, "the skill's actions have {marks} quote marks, one unpaired");
        let labels = actions::Action::ALL.map(|action| action.spec().name);
        let quoted = section.split('"').skip(1).step_by(2);
        let quoted = quoted.map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "));
        quoted.filter(|name| !labels.contains(&name.as_str())).collect()
    }

    #[test]
    fn the_luneta_skill_names_each_command_luneta_help_lists_and_no_other() {
        assert_eq!(unnamed(SKILL), Vec::<String>::new());
        assert_eq!(unlisted(SKILL), Vec::<String>::new());
        let stale = "Run `luneta\nhere`, then:\n\n```sh\nluneta prune --beside x   # no such \
                     flag\nluneta terminate --here\n```\n\n```text\nluneta go a\n```\n\nPass \
                     `--beside\n<dir>` or `--yes <plan>`.\n";
        let said = ["luneta prune --beside", "luneta terminate --here", "luneta here", "--beside"];
        let why = "a verb gone, a flag it lacks, a hidden flag, and a flag no command lists";
        assert_eq!(unlisted(stale), said, "{why}");
        assert!(unnamed(stale).contains(&"luneta agents".to_owned()), "{:?}", unnamed(stale));
    }

    #[test]
    #[should_panic(expected = "block, not sh or text")]
    fn a_code_block_neither_sh_nor_text_fails_the_luneta_skill() {
        shown("```bash\nluneta here\n```\n");
    }

    #[test]
    fn the_luneta_skill_names_only_actions_luneta_lists() {
        assert_eq!(unlabelled(SKILL), Vec::<String>::new());
        let stale = "## What the user does in luneta\n\n- \"new view\" or \"split the\n  \
                     view\".\n\n## Next\n\n\"not an action\"\n";
        assert_eq!(unlabelled(stale), ["split the view"], "a name the menu lacks, across a line");
    }
}
