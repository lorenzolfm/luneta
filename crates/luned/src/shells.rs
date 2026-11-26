use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use crate::log;

const SHELL_VAR: &str = "SHELL";

const NO_SHELL: &str = "/bin/sh";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell(String);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Unopened {
    Unread,
    Relative,
    Unnamed,
    RefusesLogins,
    NoFile,
    Unrunnable,
}

impl fmt::Display for Unopened {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Unopened::Unread => "is not UTF-8",
            Unopened::Relative => "is not an absolute path",
            Unopened::Unnamed => "names no file",
            Unopened::RefusesLogins => "refuses logins",
            Unopened::NoFile => "is no file",
            Unopened::Unrunnable => "may not be run",
        })
    }
}

const LOGIN_REFUSERS: [&str; 2] = ["nologin", "false"];

impl Shell {
    pub(crate) fn of(path: OsString) -> Result<Shell, Unopened> {
        let path = path.into_string().map_err(|_| Unopened::Unread)?;
        if !Path::new(&path).is_absolute() {
            return Err(Unopened::Relative);
        }
        let named = Path::new(&path).file_name().ok_or(Unopened::Unnamed)?;
        let named = named.to_str().expect("a file name in a UTF-8 path is UTF-8");
        if LOGIN_REFUSERS.contains(&named) {
            return Err(Unopened::RefusesLogins);
        }
        if !Path::new(&path).is_file() {
            return Err(Unopened::NoFile);
        }
        match rustix::fs::access(path.as_str(), rustix::fs::Access::EXEC_OK) {
            Ok(()) => Ok(Shell(path)),
            Err(_) => Err(Unopened::Unrunnable),
        }
    }

    fn none() -> Shell {
        Shell(NO_SHELL.to_owned())
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

    #[must_use]
    pub fn resume_kind(&self) -> Option<lune_protocol::program_resume::ShellKind> {
        lune_protocol::program_resume::ShellKind::of_path(&self.0)
    }
}

struct Named {
    login: Option<OsString>,
    variable: Option<OsString>,
}

struct Chosen {
    shell: Shell,
    passed: Vec<String>,
}

#[must_use]
pub fn your_shell() -> Shell {
    let chosen = shell(Named { login: login_shell(), variable: env::var_os(SHELL_VAR) });
    for passed in &chosen.passed {
        log::say(&format!("{passed}, so terminals open {}", chosen.shell.as_str()));
    }
    chosen.shell
}

fn shell(named: Named) -> Chosen {
    let sources = [("the user database's shell", named.login), ("$SHELL", named.variable)];
    let sources_count = sources.len();
    let mut passed = Vec::with_capacity(sources_count);
    for (source, path) in sources {
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            continue;
        };
        let shown = path.to_string_lossy().into_owned();
        match Shell::of(path) {
            Ok(shell) => return Chosen { shell, passed },
            Err(why) => passed.push(format!("{source} {shown} {why}")),
        }
    }
    assert!(passed.len() <= sources_count);
    Chosen { shell: Shell::none(), passed }
}

fn login_shell() -> Option<OsString> {
    let you = uzers::get_user_by_uid(rustix::process::getuid().as_raw())?;
    Some(uzers::os::unix::UserExt::shell(&you).as_os_str().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn made(folder: &Path, name: &str) -> OsString {
        let path = folder.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path.into_os_string()
    }

    fn named(login: Option<&OsString>, variable: Option<&OsString>) -> Named {
        Named { login: login.cloned(), variable: variable.cloned() }
    }

    fn chosen(login: Option<&OsString>, variable: Option<&OsString>) -> String {
        shell(named(login, variable)).shell.as_str().to_owned()
    }

    fn passed(login: Option<&OsString>, variable: Option<&OsString>) -> Vec<String> {
        shell(named(login, variable)).passed
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
        assert_eq!(shell(named(Some(&fish), None)).shell.name(), "fish");
        assert_eq!(shell(named(None, None)).shell.name(), "sh");
    }

    #[test]
    fn a_shell_passed_over_says_why() {
        let folder = tempfile::tempdir().unwrap();
        let bash = made(folder.path(), "bash");
        let nologin = made(folder.path(), "sbin/nologin");
        let gone = folder.path().join("fish").into_os_string();
        let refuses = format!("the user database's shell {} refuses logins", text(&nologin));
        assert_eq!(passed(Some(&nologin), Some(&bash)), [refuses]);
        let no_file = format!("the user database's shell {} is no file", text(&gone));
        let variable = format!("$SHELL {} refuses logins", text(&nologin));
        assert_eq!(passed(Some(&gone), Some(&nologin)), [no_file, variable]);
        assert_eq!(passed(Some(&bash), Some(&gone)), Vec::<String>::new());
        assert_eq!(passed(Some(&OsString::new()), None), Vec::<String>::new());
        assert_eq!(passed(None, None), Vec::<String>::new());
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
        assert_eq!(Shell::of(gone), Err(Unopened::NoFile));
        assert_eq!(Shell::of("/".into()), Err(Unopened::Unnamed));
    }

    #[test]
    fn a_shell_named_from_here_or_that_may_not_run_is_no_shell() {
        let folder = tempfile::tempdir().unwrap();
        let bash = made(folder.path(), "bash");
        let unrunnable = made(folder.path(), "fish");
        fs::set_permissions(&unrunnable, fs::Permissions::from_mode(0o644)).unwrap();
        for relative in ["fish", "bin/fish", "./fish"] {
            let relative = OsString::from(relative);
            assert_eq!(Shell::of(relative.clone()), Err(Unopened::Relative), "{relative:?}");
            assert_eq!(chosen(Some(&relative), Some(&bash)), text(&bash), "{relative:?}");
        }
        assert_eq!(Shell::of(unrunnable.clone()), Err(Unopened::Unrunnable));
        assert_eq!(chosen(Some(&unrunnable), Some(&bash)), text(&bash));
        let said = format!("$SHELL {} may not be run", text(&unrunnable));
        assert_eq!(passed(None, Some(&unrunnable)), [said]);
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
        assert_eq!(Shell::of(unread), Err(Unopened::Unread));
    }
}
