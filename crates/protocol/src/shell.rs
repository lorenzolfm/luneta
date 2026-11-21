use crate::mux::v1 as mux;
use crate::v1;
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

    #[must_use]
    pub fn of_mux(ending: &mux::ran::ended::Ending) -> Self {
        match ending {
            mux::ran::ended::Ending::Status(status) => Ending::Exited(*status),
            mux::ran::ended::Ending::Resumed(mux::ran::Resumed {}) => Ending::Resumed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    Working,
    Asking,
    Interactive,
}

impl Front {
    #[must_use]
    pub fn to_api(self) -> v1::Front {
        match self {
            Front::Working => v1::Front::Working,
            Front::Asking => v1::Front::Asking,
            Front::Interactive => v1::Front::Interactive,
        }
    }

    #[must_use]
    pub fn of_api(front: v1::Front) -> Option<Self> {
        match front {
            v1::Front::Working => Some(Front::Working),
            v1::Front::Asking => Some(Front::Asking),
            v1::Front::Interactive => Some(Front::Interactive),
            v1::Front::Unspecified => None,
        }
    }

    #[must_use]
    pub fn of_mux(front: mux::Front) -> Option<Self> {
        match front {
            mux::Front::Working => Some(Front::Working),
            mux::Front::Asking => Some(Front::Asking),
            mux::Front::Interactive => Some(Front::Interactive),
            mux::Front::Unspecified => None,
        }
    }
}
