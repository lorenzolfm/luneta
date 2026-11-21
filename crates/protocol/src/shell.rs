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
    pub fn to_mux(self) -> mux::ran::ended::Ending {
        match self {
            Ending::Exited(status) => mux::ran::ended::Ending::Status(status),
            Ending::Resumed => mux::ran::ended::Ending::Resumed(mux::ran::Resumed {}),
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
    pub fn to_mux(self) -> mux::Front {
        match self {
            Front::Working => mux::Front::Working,
            Front::Asking => mux::Front::Asking,
            Front::Interactive => mux::Front::Interactive,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_front_told_is_read_back_as_it_was() {
        for front in [Front::Working, Front::Asking, Front::Interactive] {
            assert_eq!(Front::of_api(front.to_api()), Some(front));
            assert_eq!(Front::of_mux(front.to_mux()), Some(front));
        }
        assert_eq!(Front::of_api(v1::Front::Unspecified), None);
        assert_eq!(Front::of_mux(mux::Front::Unspecified), None);
    }

    #[test]
    fn an_ending_told_is_read_back_as_it_was() {
        for ending in [Ending::Exited(0), Ending::Exited(-1), Ending::Exited(255), Ending::Resumed] {
            assert_eq!(Ending::of_api(&ending.to_api()), ending);
            assert_eq!(Ending::of_mux(&ending.to_mux()), ending);
        }
    }
}
