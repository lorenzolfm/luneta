//! How a command a shell ran ended, as the shell hook says it to the daemon,
//! and the daemon to its clients.

use crate::v1::shell::ended;

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
            Ending::Resumed => ended::Ending::Resumed(ended::Resumed {}),
        }
    }

    /// What `to_api` wrote out, read back.
    #[must_use]
    pub fn of_api(ending: &ended::Ending) -> Self {
        match ending {
            ended::Ending::Status(status) => Ending::Exited(*status),
            ended::Ending::Resumed(_) => Ending::Resumed,
        }
    }
}
