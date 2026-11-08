//! How a command a shell ran ended, as the shell hook says it to the daemon,
//! and the daemon to its clients; and which command lines are shells at their
//! prompts.

use crate::v1::shell::ended;

/// The shells, by their binaries' names, whose name says only that a pane is
/// at its prompt.
pub const SHELLS: [&str; 7] = ["bash", "dash", "elvish", "fish", "nu", "sh", "zsh"];

/// Whether the command line `command`, as zellij spells one, is a shell at its
/// prompt, as far as its words tell: its binary is named as one, a login
/// shell's leading `-` and all, and nothing but options follows it. A word
/// that is none is a script it runs, or the command `-c` gave it: zellij
/// spells the command in front, and a shell that runs one is.
#[must_use]
pub fn prompts(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return false;
    };
    let program = program.rsplit('/').next().unwrap_or(program);
    SHELLS.contains(&program.strip_prefix('-').unwrap_or(program))
        && words.all(|word| word.starts_with('-'))
}

/// How a command ended, where its shell can say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// With this exit status.
    Exited(i32),
    /// Once `fg` brought it back, with an exit status its shell cannot say,
    /// as fish cannot after `fg`. The line typed was `fg`, not the command,
    /// and what it printed before it stopped came before that line.
    Resumed,
}

impl Ending {
    #[must_use]
    pub fn to_api(self) -> ended::Ending {
        match self {
            Ending::Exited(status) => ended::Ending::Status(status),
            Ending::Resumed => ended::Ending::Resumed(()),
        }
    }

    /// What `to_api` wrote out, read back.
    #[must_use]
    pub fn of_api(ending: &ended::Ending) -> Self {
        match ending {
            ended::Ending::Status(status) => Ending::Exited(*status),
            ended::Ending::Resumed(()) => Ending::Resumed,
        }
    }
}
