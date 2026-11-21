use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, SystemTime};

use luneta_protocol::mux::v1 as mux;
use luneta_protocol::said::Said;
use luneta_protocol::shell::{Ending, Front};

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

    pub fn of_api(shell: &api::Shell) -> Result<Doing, String> {
        use api::shell::Doing as Told;
        let doing = match shell.doing.as_ref().ok_or("no doing")? {
            Told::Ready(()) => Doing::Ready,
            Told::Going(going) => Doing::Going {
                command: said(going.command.as_ref())?,
                since: time(going.since, "since")?,
                front: Front::of_api(going.front()).ok_or(NEWER_FRONT)?,
            },
            Told::Ended(ended) => Doing::Ended {
                command: said(ended.command.as_ref())?,
                at: time(ended.at, "at")?,
                ending: ended.ending.as_ref().map(Ending::of_api),
                took: took(ended.took)?,
            },
            Told::Stopped(stopped) => Doing::Stopped {
                command: said(stopped.command.as_ref())?,
                at: time(stopped.at, "at")?,
            },
        };
        Ok(doing)
    }

    pub fn of_mux(is: &mux::ran::Is) -> Result<Doing, String> {
        use mux::ran::Is;
        let doing = match is {
            Is::Ready(mux::ran::Ready {}) => Doing::Ready,
            Is::Going(going) => Doing::Going {
                command: said(going.command.as_ref())?,
                since: time(going.since, "since")?,
                front: Front::of_mux(going.front()).ok_or(NEWER_FRONT)?,
            },
            Is::Ended(ended) => Doing::Ended {
                command: said(ended.command.as_ref())?,
                at: time(ended.at, "at")?,
                ending: ended.ending.as_ref().map(Ending::of_mux),
                took: took(ended.took)?,
            },
            Is::Stopped(stopped) => Doing::Stopped {
                command: said(stopped.command.as_ref())?,
                at: time(stopped.at, "at")?,
            },
        };
        Ok(doing)
    }
}

const NEWER_FRONT: &str = "what its command does, in a word of a newer build's";

fn said(command: Option<&String>) -> Result<Option<Said>, String> {
    match command {
        None => Ok(None),
        Some(command) => Said::new(command.clone()).map(Some).ok_or("an empty command line".into()),
    }
}

fn time(at: Option<prost_types::Timestamp>, what: &str) -> Result<SystemTime, String> {
    let at = at.ok_or_else(|| format!("no {what}"))?;
    SystemTime::try_from(at).map_err(|why| format!("its {what}: {why}"))
}

fn took(took: Option<prost_types::Duration>) -> Result<Duration, String> {
    let took = took.ok_or("no took")?;
    Duration::try_from(took).map_err(|why| format!("its took: {why}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell(String);

const LOGIN_REFUSERS: [&str; 2] = ["nologin", "false"];

impl Shell {
    pub(crate) fn of(path: OsString) -> Option<Shell> {
        let path = path.into_string().ok()?;
        let named = Path::new(&path).file_name()?.to_str()?;
        let opens = !LOGIN_REFUSERS.contains(&named) && Path::new(&path).is_file();
        opens.then_some(Shell(path))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn name(&self) -> &str {
        let named = Path::new(&self.0).file_name().and_then(|name| name.to_str());
        named.expect("a shell's path ends in its name, in UTF-8")
    }
}

#[must_use]
pub fn your_shell() -> Shell {
    shell(login_shell(), env::var_os(SHELL_VAR))
}

fn shell(login: Option<OsString>, named: Option<OsString>) -> Shell {
    let opening = |shell: Option<OsString>| shell.and_then(Shell::of);
    opening(login).or_else(|| opening(named)).unwrap_or_else(|| Shell(NO_SHELL.to_owned()))
}

fn login_shell() -> Option<OsString> {
    let you = uzers::get_user_by_uid(rustix::process::getuid().as_raw())?;
    Some(uzers::os::unix::UserExt::shell(&you).as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn made(folder: &Path, name: &str) -> OsString {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        path.into_os_string()
    }

    fn chosen(login: Option<&OsString>, named: Option<&OsString>) -> String {
        shell(login.cloned(), named.cloned()).as_str().to_owned()
    }

    fn text(path: &OsString) -> &str {
        path.to_str().unwrap()
    }

    #[test]
    fn your_shell_is_the_one_the_user_database_names() {
        let folder = tempfile::tempdir().unwrap();
        let [fish, bash] = ["fish", "bash"].map(|name| made(folder.path(), name));
        let empty = OsString::new();
        assert_eq!(chosen(Some(&fish), Some(&bash)), text(&fish));
        assert_eq!(chosen(None, Some(&bash)), text(&bash));
        assert_eq!(chosen(Some(&empty), Some(&bash)), text(&bash));
        assert_eq!(chosen(None, Some(&empty)), "/bin/sh");
        assert_eq!(chosen(None, None), "/bin/sh");
        assert_eq!(shell(Some(fish), None).name(), "fish");
    }

    #[test]
    fn a_shell_that_is_no_file_is_no_shell() {
        let folder = tempfile::tempdir().unwrap();
        let bash = made(folder.path(), "bash");
        let gone = folder.path().join("fish").into_os_string();
        let folder = folder.path().as_os_str().to_owned();
        assert_eq!(chosen(Some(&gone), Some(&bash)), text(&bash));
        assert_eq!(chosen(Some(&folder), Some(&bash)), text(&bash));
        assert_eq!(chosen(Some(&bash), Some(&gone)), text(&bash));
        assert_eq!(chosen(None, Some(&gone)), "/bin/sh");
    }

    #[test]
    fn a_login_shell_that_refuses_logins_is_no_shell() {
        let folder = tempfile::tempdir().unwrap();
        let bash = made(folder.path(), "bash");
        for refusing in ["sbin/nologin", "usr/sbin/nologin", "usr/bin/false", "bin/false"] {
            let refusing = made(folder.path(), refusing);
            assert_eq!(chosen(Some(&refusing), Some(&bash)), text(&bash), "{refusing:?}");
            assert_eq!(chosen(Some(&refusing), None), "/bin/sh", "{refusing:?}");
            assert_eq!(chosen(None, Some(&refusing)), "/bin/sh", "{refusing:?}");
            assert_eq!(chosen(Some(&refusing), Some(&refusing)), "/bin/sh", "{refusing:?}");
        }
        let fish = made(folder.path(), "nologin-not/fish");
        assert_eq!(chosen(Some(&fish), Some(&bash)), text(&fish));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_shell_whose_path_is_not_utf_8_is_no_shell() {
        use std::os::unix::ffi::OsStringExt;
        let folder = tempfile::tempdir().unwrap();
        let bash = made(folder.path(), "bash");
        let mut unread = folder.path().as_os_str().to_owned().into_vec();
        unread.extend(b"/fi\xffsh");
        let unread = OsString::from_vec(unread);
        fs::write(&unread, "").unwrap();
        assert_eq!(chosen(Some(&unread), Some(&bash)), text(&bash));
        assert_eq!(chosen(None, Some(&unread)), "/bin/sh");
    }

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
        }
    }

    #[test]
    fn a_doing_told_with_a_hole_is_refused() {
        use api::shell::{Doing as Told, Ended, Going};
        let since = Some(at(1).into());
        let (command, front) = (Some("make".to_owned()), api::Front::Working.into());
        let going = Going { command, since, front };
        let took = Some(prost_types::Duration::default());
        let ended = Ended { command: None, at: since, ending: None, took };
        let holed = [
            None,
            Some(Told::Going(Going { front: api::Front::Unspecified.into(), ..going.clone() })),
            Some(Told::Going(Going { command: Some(String::new()), ..going.clone() })),
            Some(Told::Going(Going { since: None, ..going })),
            Some(Told::Ended(Ended { at: None, ..ended.clone() })),
            Some(Told::Ended(Ended { took: None, ..ended })),
        ];
        for doing in holed {
            let shell = api::Shell { doing };
            assert!(Doing::of_api(&shell).is_err(), "{shell:?}");
        }
    }
}
