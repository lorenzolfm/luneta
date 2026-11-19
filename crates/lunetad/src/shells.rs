use std::env;
use std::ffi::OsString;
use std::time::{Duration, SystemTime};

use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;

use crate::api;

const SHELL_VAR: &str = "SHELL";

const NO_SHELL: &str = "/bin/sh";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    Ready,
    Going { command: Option<Said>, since: SystemTime, front: Front },
    Ended { command: Option<Said>, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: Option<Said>, at: SystemTime },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    Working,
    Asking,
    Interactive,
}

impl Doing {
    #[must_use]
    pub fn to_api(&self) -> api::Shell {
        use api::shell::{Doing as Told, Ended, Going, Stopped};
        let doing = match self {
            Doing::Ready => Told::Ready(()),
            Doing::Going { command, since, front } => Told::Going(Going {
                command: command.clone().map(String::from),
                since: Some((*since).into()),
                front: front.to_api().into(),
            }),
            Doing::Ended { command, at, ending, took } => Told::Ended(Ended {
                command: command.clone().map(String::from),
                at: Some((*at).into()),
                ending: ending.map(Ending::to_api),
                took: Some((*took).try_into().expect("the milliseconds of a u64 fit a Duration")),
            }),
            Doing::Stopped { command, at } => Told::Stopped(Stopped {
                command: command.clone().map(String::from),
                at: Some((*at).into()),
            }),
        };
        api::Shell { doing: Some(doing) }
    }
}

impl Front {
    fn to_api(self) -> api::Front {
        match self {
            Front::Working => api::Front::Working,
            Front::Asking => api::Front::Asking,
            Front::Interactive => api::Front::Interactive,
        }
    }
}

#[must_use]
pub fn your_shell() -> OsString {
    shell(login_shell(), env::var_os(SHELL_VAR))
}

fn shell(login: Option<OsString>, named: Option<OsString>) -> OsString {
    let some = |shell: &OsString| !shell.is_empty();
    login.filter(some).or_else(|| named.filter(some)).unwrap_or_else(|| NO_SHELL.into())
}

fn login_shell() -> Option<OsString> {
    let you = uzers::get_user_by_uid(rustix::process::getuid().as_raw())?;
    let shell = uzers::os::unix::UserExt::shell(&you);
    shell.is_file().then(|| shell.as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn your_shell_is_the_one_the_user_database_names() {
        let (fish, bash) = (|| Some("/bin/fish".into()), || Some("/bin/bash".into()));
        assert_eq!(shell(fish(), bash()), "/bin/fish");
        assert_eq!(shell(None, bash()), "/bin/bash");
        assert_eq!(shell(Some(OsString::new()), bash()), "/bin/bash");
        assert_eq!(shell(None, Some(OsString::new())), "/bin/sh");
        assert_eq!(shell(None, None), "/bin/sh");
    }
}
