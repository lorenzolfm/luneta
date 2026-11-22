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
mod fuzzy;
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
    about = "The views of a worktree, terminals of luneta's own multiplexer, beside a sidebar \
             of the projects on this machine and the others it watches, and what the agent in \
             each view is doing. With a command, ask lunetad, the daemon of this machine, \
             instead, or have it start or delete something"
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
            long,
            help = "When no worktree has the branch, and a worktree of the repository on another \
                    branch, or on none, is where wt would make one, make none and print that \
                    worktree after `moved off: `"
        )]
        moved_off: bool,
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
        Command::Worktree { repo, moved_off, of } => return pull::make(&repo, &of, moved_off),
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
    use std::collections::BTreeSet;

    use clap::{ArgAction, CommandFactory as _};
    use lunetad::asks::SKILL;

    use super::*;
    use crate::actions::Action;
    use crate::menu::Origin;

    const BY_THE_USER: &str = "## What the user does in luneta\n";

    const KEY_WORDS: [&str; 9] =
        ["Ctrl", "Alt", "Shift", "Super", "Tab", "Enter", "Esc", "BS", "Space"];

    struct Read {
        prose: String,
        shown: Vec<String>,
    }

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

    fn flags_listed(command: &clap::Command) -> impl Iterator<Item = &str> {
        let builtin = |argument: &&clap::Arg| {
            let helps = matches!(argument.get_action(), ArgAction::Help | ArgAction::HelpShort);
            helps || matches!(argument.get_action(), ArgAction::HelpLong | ArgAction::Version)
        };
        let shown = command.get_arguments().filter(|argument| !argument.is_hide_set());
        shown.filter(move |argument| !builtin(argument)).filter_map(clap::Arg::get_long)
    }

    fn long(word: &str) -> Option<&str> {
        let (_, flag) = word.split_once("--")?;
        let flag = flag.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').next();
        flag.filter(|flag| !flag.is_empty())
    }

    fn spans(text: &str) -> impl Iterator<Item = String> + '_ {
        text.split('`').skip(1).step_by(2).map(|span| span.replace('\n', " "))
    }

    fn read(skill: &str) -> Read {
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
                    let (command, comment) = line.split_once(" # ").unwrap_or((line, ""));
                    shown.push(command.to_owned());
                    let paired = comment.matches('`').count().is_multiple_of(2);
                    assert!(paired, "the skill's comment {comment:?} has a backtick unpaired");
                    shown.extend(spans(comment));
                },
                (Some(_), None) => {},
                (None, None) => {
                    let tilde = line.trim_start().starts_with("~~~");
                    assert!(!tilde, "the skill opens a ~~~ block, not a ``` one");
                    let indented = line.starts_with("    ") || line.starts_with('\t');
                    assert!(!indented, "the skill indents {line:?} as code, in no ``` block");
                    prose.push_str(line);
                    prose.push('\n');
                },
            }
        }
        assert!(fence.is_none(), "the skill leaves a code block open");
        let ticks = prose.matches('`').count();
        let paired = ticks.is_multiple_of(2);
        assert!(paired, "the skill's prose has {ticks} backticks, one unpaired");
        shown.extend(spans(&prose));
        let shown = shown.into_iter().map(|said| said.trim().to_owned()).collect();
        Read { prose, shown }
    }

    fn shown(skill: &str) -> Vec<String> {
        read(skill).shown
    }

    fn commands_named(skill: &str) -> Vec<String> {
        let mut named = Vec::new();
        for said in shown(skill) {
            let words: Vec<&str> = said.split_whitespace().collect();
            let luneta = |(_, word): &(usize, &&str)| {
                word.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()) == "luneta"
            };
            let joins = |word: &&&str| matches!(**word, "&&" | "||" | "|" | ";");
            for (at, _) in words.iter().enumerate().filter(luneta) {
                let rest = words[at + 1..].iter().take_while(|word| !joins(word));
                let rest: Vec<&str> = rest.copied().collect();
                if !rest.is_empty() {
                    named.push(format!("luneta {}", rest.join(" ")));
                }
            }
        }
        named
    }

    fn verb_of(said: &str) -> Option<&str> {
        let verb = said.split_whitespace().nth(1)?;
        let verb = verb.trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
        verb.starts_with(|c: char| c.is_ascii_lowercase()).then_some(verb)
    }

    fn unlisted(skill: &str) -> Vec<String> {
        let cli = cli();
        let mut unlisted = Vec::new();
        for said in commands_named(skill) {
            let Some(verb) = verb_of(&said) else {
                continue;
            };
            let Some(command) = cli.find_subcommand(verb).filter(|command| !command.is_hide_set())
            else {
                unlisted.push(format!("luneta {verb}"));
                continue;
            };
            for flag in said.split_whitespace().skip(2).filter_map(long) {
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
        let cli = cli();
        let mut unnamed = Vec::new();
        for command in listed(&cli) {
            let verb = command.get_name();
            let with = named.iter().filter(|said| verb_of(said) == Some(verb));
            let with: Vec<&String> = with.collect();
            if with.is_empty() {
                unnamed.push(format!("luneta {verb}"));
                continue;
            }
            let flags = with.iter().flat_map(|said| said.split_whitespace().filter_map(long));
            let flags: Vec<&str> = flags.collect();
            let unflagged = flags_listed(command).filter(|flag| !flags.contains(flag));
            unnamed.extend(unflagged.map(|flag| format!("luneta {verb} --{flag}")));
        }
        unnamed
    }

    fn unlabelled(skill: &str) -> Vec<String> {
        let section = skill.split_once(BY_THE_USER).map(|(_, section)| section);
        let section = section.expect("the skill says what the user does in luneta");
        let section = section.split_once("\n## ").map_or(section, |(section, _)| section);
        let marks = section.matches('"').count();
        assert!(marks > 0, "the skill quotes no action of luneta's by name");
        let paired = marks.is_multiple_of(2);
        assert!(paired, "the skill's actions have {marks} quote marks, one unpaired");
        let listed = Action::ALL.into_iter().filter(|action| action.in_palette());
        let labels: Vec<&str> = listed.map(|action| action.spec().name).collect();
        let quoted = section.split('"').skip(1).step_by(2);
        let quoted = quoted.map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "));
        quoted.filter(|name| !labels.contains(&name.as_str())).collect()
    }

    fn slug(heading: &str) -> String {
        let kept = heading.trim().to_lowercase();
        let kept = kept.chars().filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'));
        kept.map(|c| if c == ' ' { '-' } else { c }).collect()
    }

    fn unanchored(skill: &str) -> Vec<String> {
        let prose = read(skill).prose;
        let headings = prose.lines().filter(|line| line.starts_with('#'));
        let headings = headings.map(|line| slug(line.trim_start_matches('#')));
        let headings: Vec<String> = headings.collect();
        let anchors = prose.split("](#").skip(1).filter_map(|rest| rest.split_once(')'));
        let anchors = anchors.map(|(anchor, _)| anchor.to_owned());
        anchors.filter(|anchor| !headings.contains(anchor)).collect()
    }

    fn keyed(said: &str) -> bool {
        let one = |word: &str| word.len() == 1 && word.chars().all(|c| c.is_ascii_alphanumeric());
        said.split(' ').all(|word| KEY_WORDS.contains(&word) || one(word))
    }

    fn unbound(skill: &str) -> Vec<String> {
        let origins = [Origin::Sidebar, Origin::Views];
        let spelled = |action| origins.map(|origin| menu::spelled(action, origin));
        let spelled = Action::ALL.into_iter().flat_map(spelled);
        let moves = menu::MOVES.iter().chain(&menu::MENU_MOVES).map(|(label, _)| label.to_string());
        let bound: Vec<String> = spelled.chain(moves).collect();
        shown(skill).into_iter().filter(|said| keyed(said) && !bound.contains(said)).collect()
    }

    fn exits(skill: &str) -> BTreeSet<u8> {
        let skill = skill.split_whitespace().collect::<Vec<_>>().join(" ");
        let after = ["exits ", "Exit "].into_iter().flat_map(|word| skill.split(word).skip(1));
        let digits = |rest: &str| rest.chars().take_while(char::is_ascii_digit).collect::<String>();
        let codes = after.map(digits);
        codes.filter_map(|code| code.parse().ok()).collect()
    }

    #[test]
    fn the_luneta_skill_names_each_command_luneta_help_lists_and_no_other() {
        assert_eq!(unnamed(SKILL), Vec::<String>::new());
        assert_eq!(unlisted(SKILL), Vec::<String>::new());
        let stale = "Run `luneta\nhere`, then:\n\n```sh\nluneta prune --beside x   # no such \
                     flag, nor `luneta go`\nluneta terminate --here\n```\n\n```text\nluneta go \
                     a\n```\n\nPass `--beside\n<dir>` or `--yes <plan>`, as `cd a && luneta \
                     sessions` or `$(luneta open)` would.\n";
        let said = [
            "luneta prune --beside",
            "luneta go",
            "luneta terminate --here",
            "luneta here",
            "luneta sessions",
            "luneta open",
            "--beside",
        ];
        let why = "a verb gone, in a comment or past the span's start, a flag it lacks, a hidden \
                   flag, and a flag no command lists";
        assert_eq!(unlisted(stale), said, "{why}");
        assert!(unnamed(stale).contains(&"luneta agents".to_owned()), "{:?}", unnamed(stale));
        let flags = unnamed("Run `luneta say <text>`.\n");
        let unflagged = ["luneta say --now".to_owned(), "luneta say --stop".to_owned()];
        assert!(unflagged.iter().all(|flag| flags.contains(flag)), "{flags:?}");
        assert!(!flags.iter().any(|flag| flag.ends_with("--help")), "{flags:?}");
    }

    #[test]
    #[should_panic(expected = "block, not sh or text")]
    fn a_code_block_neither_sh_nor_text_fails_the_luneta_skill() {
        shown("```bash\nluneta here\n```\n");
    }

    #[test]
    #[should_panic(expected = "a ~~~ block")]
    fn a_tilde_block_fails_the_luneta_skill() {
        shown("~~~sh\nluneta here\n~~~\n");
    }

    #[test]
    #[should_panic(expected = "as code, in no ``` block")]
    fn a_line_indented_as_code_fails_the_luneta_skill() {
        shown("Run:\n\n    luneta here\n");
    }

    #[test]
    fn the_luneta_skill_names_only_actions_luneta_lists() {
        assert_eq!(unlabelled(SKILL), Vec::<String>::new());
        let stale = "## What the user does in luneta\n\n- \"new view\" or \"split the\n  \
                     view\", or \"tackle\".\n\n## Next\n\n\"not an action\"\n";
        let why = "a name the menu lacks, across a line, and one only a pasted link offers";
        assert_eq!(unlabelled(stale), ["split the view", "tackle"], "{why}");
    }

    #[test]
    fn each_link_of_the_luneta_skill_lands_on_a_heading() {
        assert_eq!(unanchored(SKILL), Vec::<String>::new());
        let stale = "See [it](#reset-luneta) and [that](#merged-worktrees).\n\n## Merged \
                     worktrees\n\n```text\n## Reset luneta\n```\n";
        assert_eq!(unanchored(stale), ["reset-luneta"], "a heading in a block is no heading");
    }

    #[test]
    fn each_key_the_luneta_skill_names_is_a_key_of_luneta() {
        assert_eq!(unbound(SKILL), Vec::<String>::new());
        let stale = "Press `Alt Z`, `q q q`, `Alt a`, `Tab`, the sidebar's `n m`, or a pasted \
                     link's `w`, past `-` and `~`.\n";
        let why = "a chord and a path luneta binds to nothing";
        assert_eq!(unbound(stale), ["Alt Z", "q q q"], "{why}");
    }

    #[test]
    fn the_luneta_skill_quotes_what_luneta_says_and_how_it_exits() {
        let skill = SKILL.split_whitespace().collect::<Vec<_>>().join(" ");
        let says = [rebase::IN_DOUBT, prune::UNTOLD, start::ELSEWHERE];
        let unsaid: Vec<&str> = says.into_iter().filter(|said| !skill.contains(said)).collect();
        assert_eq!(unsaid, Vec::<&str>::new());
        let codes = [0, 1, agreed::PLANNED, start::UNFINISHED, daemon::NONE_RUNS];
        assert_eq!(exits(SKILL), BTreeSet::from(codes));
        let stale = "It exits\n7, and with `--yes` it exits 3. Exit 1 means it failed.\n";
        assert_eq!(exits(stale), BTreeSet::from([1, 3, 7]), "a code across a line");
    }
}
