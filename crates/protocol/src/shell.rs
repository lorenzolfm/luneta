use crate::v1::shell::ended;

pub const SHELLS: [&str; 7] = ["bash", "dash", "elvish", "fish", "nu", "sh", "zsh"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Exited(i32),
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

    #[must_use]
    pub fn of_api(ending: &ended::Ending) -> Self {
        match ending {
            ended::Ending::Status(status) => Ending::Exited(*status),
            ended::Ending::Resumed(()) => Ending::Resumed,
        }
    }
}
