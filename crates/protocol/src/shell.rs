use std::time::{Duration, SystemTime};

use crate::mux::v1 as mux;
use crate::said::Said;
use crate::v1;
use crate::v1::shell::ended;

pub const SHELLS: [&str; 7] = ["bash", "dash", "elvish", "fish", "nu", "sh", "zsh"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    Ready,
    Going { command: Option<Said>, since: SystemTime, front: Front },
    Ended { command: Option<Said>, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: Option<Said>, at: SystemTime },
}

impl Doing {
    #[must_use]
    pub fn to_api(&self) -> v1::Shell {
        use v1::shell::{Doing as Told, Ended, Going, Stopped};
        let doing = match self {
            Doing::Ready => Told::Ready(()),
            Doing::Going { command, since, front } => Told::Going(Going {
                command: command_line(command.as_ref()),
                since: Some((*since).into()),
                front: front.to_api().into(),
            }),
            Doing::Ended { command, at, ending, took } => Told::Ended(Ended {
                command: command_line(command.as_ref()),
                at: Some((*at).into()),
                ending: ending.map(Ending::to_api),
                took: Some(told_took(*took)),
            }),
            Doing::Stopped { command, at } => Told::Stopped(Stopped {
                command: command_line(command.as_ref()),
                at: Some((*at).into()),
            }),
        };
        v1::Shell { doing: Some(doing) }
    }

    pub fn of_api(shell: &v1::Shell) -> Result<Doing, String> {
        use v1::shell::Doing as Told;
        let doing = match shell.doing.as_ref().ok_or("no doing")? {
            Told::Ready(()) => Doing::Ready,
            Told::Going(going) => Doing::Going {
                command: command_of(going.command.as_ref())?,
                since: time(going.since, "since")?,
                front: Front::of_api(going.front()).ok_or(NEWER_FRONT)?,
            },
            Told::Ended(ended) => Doing::Ended {
                command: command_of(ended.command.as_ref())?,
                at: time(ended.at, "at")?,
                ending: ended.ending.as_ref().map(Ending::of_api),
                took: took(ended.took)?,
            },
            Told::Stopped(stopped) => Doing::Stopped {
                command: command_of(stopped.command.as_ref())?,
                at: time(stopped.at, "at")?,
            },
        };
        Ok(doing)
    }

    #[must_use]
    pub fn to_mux(&self) -> mux::Ran {
        use mux::ran::{Ended, Going, Is, Ready, Stopped};
        let is = match self {
            Doing::Ready => Is::Ready(Ready {}),
            Doing::Going { command, since, front } => Is::Going(Going {
                command: command_line(command.as_ref()),
                since: Some((*since).into()),
                front: front.to_mux().into(),
            }),
            Doing::Ended { command, at, ending, took } => Is::Ended(Ended {
                command: command_line(command.as_ref()),
                at: Some((*at).into()),
                ending: ending.map(Ending::to_mux),
                took: Some(told_took(*took)),
            }),
            Doing::Stopped { command, at } => Is::Stopped(Stopped {
                command: command_line(command.as_ref()),
                at: Some((*at).into()),
            }),
        };
        mux::Ran { is: Some(is) }
    }

    pub fn of_mux(ran: &mux::Ran) -> Result<Doing, String> {
        use mux::ran::Is;
        let doing = match ran.is.as_ref().ok_or("no doing")? {
            Is::Ready(mux::ran::Ready {}) => Doing::Ready,
            Is::Going(going) => Doing::Going {
                command: command_of(going.command.as_ref())?,
                since: time(going.since, "since")?,
                front: Front::of_mux(going.front()).ok_or(NEWER_FRONT)?,
            },
            Is::Ended(ended) => Doing::Ended {
                command: command_of(ended.command.as_ref())?,
                at: time(ended.at, "at")?,
                ending: ended.ending.as_ref().map(Ending::of_mux),
                took: took(ended.took)?,
            },
            Is::Stopped(stopped) => Doing::Stopped {
                command: command_of(stopped.command.as_ref())?,
                at: time(stopped.at, "at")?,
            },
        };
        Ok(doing)
    }
}

const NEWER_FRONT: &str = "what its command does, in a word of a newer build's";

fn command_line(command: Option<&Said>) -> Option<String> {
    command.map(|command| command.as_str().to_owned())
}

fn command_of(command: Option<&String>) -> Result<Option<Said>, String> {
    match command {
        None => Ok(None),
        Some(command) => Said::new(command.clone()).map(Some).ok_or("an empty command line".into()),
    }
}

fn time(at: Option<prost_types::Timestamp>, what: &str) -> Result<SystemTime, String> {
    let at = at.ok_or_else(|| format!("no {what}"))?;
    SystemTime::try_from(at).map_err(|why| format!("its {what}: {why}"))
}

fn told_took(took: Duration) -> prost_types::Duration {
    took.try_into().expect("a command ran for under 2^63 seconds")
}

fn took(took: Option<prost_types::Duration>) -> Result<Duration, String> {
    let took = took.ok_or("no took")?;
    Duration::try_from(took).map_err(|why| format!("its took: {why}"))
}

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

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    #[test]
    fn a_doing_told_is_read_back_as_it_was() {
        let make = || Said::new("make".to_owned());
        let doings = [
            Doing::Ready,
            Doing::Going { command: make(), since: at(1), front: Front::Asking },
            Doing::Going { command: None, since: at(1), front: Front::Interactive },
            Doing::Ended {
                command: make(),
                at: at(2),
                ending: Some(Ending::Exited(2)),
                took: Duration::from_millis(1500),
            },
            Doing::Ended {
                command: None,
                at: at(2),
                ending: Some(Ending::Resumed),
                took: Duration::ZERO,
            },
            Doing::Ended { command: make(), at: at(2), ending: None, took: Duration::ZERO },
            Doing::Stopped { command: make(), at: at(3) },
        ];
        for doing in doings {
            assert_eq!(Doing::of_api(&doing.to_api()), Ok(doing.clone()), "{doing:?}");
            assert_eq!(Doing::of_mux(&doing.to_mux()), Ok(doing.clone()), "{doing:?}");
        }
    }

    #[test]
    fn a_doing_told_with_a_hole_is_refused() {
        use v1::shell::{Doing as Told, Ended, Going};
        let since = Some(at(1).into());
        let (command, front) = (Some("make".to_owned()), v1::Front::Working.into());
        let going = Going { command, since, front };
        let took = Some(prost_types::Duration::default());
        let ended = Ended { command: None, at: since, ending: None, took };
        let holed = [
            None,
            Some(Told::Going(Going { front: v1::Front::Unspecified.into(), ..going.clone() })),
            Some(Told::Going(Going { command: Some(String::new()), ..going.clone() })),
            Some(Told::Going(Going { since: None, ..going })),
            Some(Told::Ended(Ended { at: None, ..ended.clone() })),
            Some(Told::Ended(Ended { took: None, ..ended })),
        ];
        for doing in holed {
            let shell = v1::Shell { doing };
            assert!(Doing::of_api(&shell).is_err(), "{shell:?}");
        }
    }

    #[test]
    fn a_doing_the_mux_tells_with_a_hole_is_refused() {
        use mux::ran::{Ended, Going, Is, Stopped};
        let since = Some(at(1).into());
        let (command, front) = (Some("make".to_owned()), mux::Front::Working.into());
        let going = Going { command, since, front };
        let took = Some(prost_types::Duration::default());
        let ended = Ended { command: None, at: since, ending: None, took };
        let negative = Some(prost_types::Duration { seconds: -1, nanos: 0 });
        let holed = [
            None,
            Some(Is::Going(Going { front: mux::Front::Unspecified.into(), ..going.clone() })),
            Some(Is::Going(Going { command: Some(String::new()), ..going.clone() })),
            Some(Is::Going(Going { since: None, ..going })),
            Some(Is::Ended(Ended { at: None, ..ended.clone() })),
            Some(Is::Ended(Ended { took: negative, ..ended.clone() })),
            Some(Is::Ended(Ended { took: None, ..ended })),
            Some(Is::Stopped(Stopped { command: None, at: None })),
        ];
        for is in holed {
            let ran = mux::Ran { is };
            assert!(Doing::of_mux(&ran).is_err(), "{ran:?}");
        }
    }

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
