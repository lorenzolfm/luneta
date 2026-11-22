use std::ffi::OsString;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

use crate::{process, shells};

pub mod own;

pub use own::{OwnMade, TerminalKey};

#[derive(Debug)]
pub enum Failure {
    Own(String),
    Called(own::server::Reply),
    Unread(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Own(why) | Failure::Unread(why) => f.write_str(why),
            Failure::Called(said) => write!(f, "luneta-mux: {said}"),
        }
    }
}

impl From<Failure> for String {
    fn from(failure: Failure) -> Self {
        failure.to_string()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runs<'a> {
    Shell(&'a shells::Shell),
    Command { program: &'a str, args: &'a [String] },
}

pub fn terminal_of(id: &process::Id) -> Result<Option<TerminalKey>, String> {
    let environ = process::environ(id.pid);
    if process::start(id.pid).as_ref() == Some(&id.start) {
        match environ {
            Ok(environ) => own::terminal_in(&environ),
            Err(process::Unread::Gone) => Ok(None),
            Err(process::Unread::Failed(why)) => {
                Err(format!("its environment does not read: {why}"))
            },
        }
    } else {
        Ok(None)
    }
}

fn value_in<'a>(environ: &'a [OsString], name: &str) -> Option<&'a [u8]> {
    environ.iter().find_map(|entry| {
        entry.as_bytes().strip_prefix(name.as_bytes())?.strip_prefix(b"=")
    })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::env;
    #[cfg(target_os = "linux")]
    use std::io::Read as _;
    use std::num::NonZeroU32;
    use std::process::Command;
    #[cfg(target_os = "linux")]
    use std::process::Stdio;

    use super::*;

    fn environ(entries: &[&str]) -> Vec<OsString> {
        entries.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_terminal_is_named_by_its_variables() {
        let terminal = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(own::terminal_in(&environ(&terminal)), Ok(Some(key)));
        let old = ["LUNETA_MUX_SESSION=3", "LUNETA_MUX_MADE=60", "LUNETA_MUX_PANE=12"];
        let old = own::terminal_in(&environ(&old));
        assert_eq!(old, Ok(None), "an old own session names no terminal");
    }

    #[test]
    fn a_variable_set_twice_reads_as_its_first() {
        let twice = ["LUNETA_MUX_TERMINAL=7", "LUNETA_MUX_TERMINAL_MADE=60"];
        let twice = [&twice[..], &["LUNETA_MUX_TERMINAL=8"]].concat();
        let key = TerminalKey { id: NonZeroU32::new(7).unwrap(), made: OwnMade::from_nanos(60) };
        assert_eq!(own::terminal_in(&environ(&twice)), Ok(Some(key)));
    }

    #[test]
    fn a_process_that_ended_runs_in_no_terminal() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let start = process::start(pid).expect("a process that runs has a start");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(terminal_of(&process::Id { pid, start }), Ok(None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_process_that_runs_and_whose_environment_does_not_read_is_a_trouble() {
        let scratch = tempfile::tempdir().unwrap();
        let sh = scratch.path().join("sh");
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let found = env::split_paths(&path).map(|dir| dir.join("sh")).find(|it| it.is_file());
        let found = found.expect("sh is on the PATH");
        let installed = Command::new("install").arg("-m111").arg(found).arg(&sh).status();
        assert!(installed.unwrap().success());
        let mut waits = Command::new(&sh);
        waits.args(["-c", "echo; read -r line"]).stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = waits.spawn().unwrap();
        child.stdout.as_mut().unwrap().read_exact(&mut [0; 1]).expect("it says it runs");
        let pid = child.id();
        let start = process::start(pid).expect("a process that runs has a start");
        let unread = process::Id { pid, start };
        let read = terminal_of(&unread);
        child.kill().unwrap();
        child.wait().unwrap();
        let why = read.expect_err("a program this user may run but not read hides its environment");
        assert!(why.starts_with("its environment does not read: "), "{why}");
        assert_eq!(terminal_of(&unread), Ok(None), "it ended");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_that_runs_and_whose_environment_does_not_read_is_a_trouble() {
        let start = process::start(1).expect("launchd runs");
        let launchd = process::Id { pid: 1, start };
        let why = terminal_of(&launchd).expect_err("root's environment reads only to root");
        assert!(why.starts_with("its environment does not read: "), "{why}");
    }
}
