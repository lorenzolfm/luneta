use std::env;
use std::ffi::OsString;
use std::io;

use rustix::io::Errno;
use rustix::process::Pid;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as os;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as os;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("luned reads processes on Linux and macOS, and on nothing else yet");

pub use os::{environ, mine, signalled, start, started, Domain};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id {
    pub pid: u32,
    pub start: Start,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Start(String);

impl Start {
    #[must_use]
    pub fn spelled(spelling: String) -> Option<Start> {
        (!spelling.is_empty()).then_some(Start(spelling))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: Id,
    pub name: String,
    pub argv: Vec<OsString>,
    pub environ: Vec<OsString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    Gone,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    pub id: Id,
    pub name: String,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mine {
    pub found: Vec<Found>,
    pub unreadable: Vec<Unreadable>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unended {
    Outlived,
    Unsignalled(Errno),
    Unwatched(Errno),
}

pub fn var(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} does not read")),
    }
}

pub fn ends(pid: u32) -> io::Result<()> {
    match i32::try_from(pid).ok().and_then(Pid::from_raw) {
        Some(pid) => os::ends(pid),
        None => Err(Errno::INVAL.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::ffi::OsString;
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    #[test]
    fn a_process_is_dated_no_earlier_than_it_started() {
        for _ in 0..10 {
            let before = SystemTime::now();
            let mut child = Command::new("sleep").arg("60").spawn().unwrap();
            let started = super::started(child.id());
            let after = SystemTime::now();
            child.kill().unwrap();
            child.wait().unwrap();
            let started = started.expect("a process that runs is dated");
            assert!(before <= started, "dated {started:?}, started after {before:?}");
            assert!(started <= after + Duration::from_secs(1), "dated {started:?}, by {after:?}");
        }
    }

    fn hears_it_ended(pid: u32, what: &str) {
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || ended.send(super::ends(pid)));
        lune_waits::heard(what, &heard).unwrap();
    }

    #[test]
    fn the_end_of_a_process_is_heard_as_it_comes() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || ended.send(super::ends(pid)));
        lune_waits::never("its end is heard while it runs", || heard.try_recv().ok());
        assert!(super::start(pid).is_some());

        child.kill().unwrap();
        lune_waits::heard("it was killed", &heard).unwrap();
        assert_eq!(super::start(pid), None);
        hears_it_ended(pid, "ended, and not waited for");

        child.wait().unwrap();
        assert_eq!(super::start(pid), None);
        hears_it_ended(pid, "ended, and waited for");
    }

    #[test]
    fn an_environment_reads_as_it_was_given() {
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let mut given = Command::new("sh");
        given.arg0("").args(["-c", "echo; read -r line"]);
        given.stdin(Stdio::piped()).stdout(Stdio::piped());
        given.env_clear().env("PATH", &path).env("LUNE_GIVEN", "a b=c");
        let mut child = given.spawn().unwrap();
        child.stdout.as_mut().unwrap().read_exact(&mut [0; 1]).expect("it says it runs");
        let read = super::environ(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        let read = read.expect("this user's process has an environment that reads");
        assert!(read.len() >= 2, "{read:?}");
        let mut given = read[..2].to_vec();
        given.sort_unstable();
        let mut path_given = OsString::from("PATH=");
        path_given.push(path);
        assert_eq!(given, [OsString::from("LUNE_GIVEN=a b=c"), path_given], "{read:?}");
    }

    #[test]
    fn a_process_of_mine_reads_its_empty_arguments_and_its_whole_environment() {
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let mut given = Command::new("sh");
        given.args(["-c", "echo; read -r line", "", ""]);
        given.stdin(Stdio::piped()).stdout(Stdio::piped());
        given.env_clear().env("PATH", &path).env("LUNE_GIVEN", "a b=c");
        let mut child = given.spawn().unwrap();
        child.stdout.as_mut().unwrap().read_exact(&mut [0; 1]).expect("it says it runs");
        let mine = super::mine(&["sh", "bash"]);
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        let unreadable = mine.unreadable.iter().find(|it| it.id.pid == pid);
        assert_eq!(unreadable, None, "{:?}", mine.unreadable);
        let found = mine.found.into_iter().find(|it| it.id.pid == pid);
        let found = found.expect("this user's shell is found");
        assert_eq!(found.argv[1..], ["-c", "echo; read -r line", "", ""], "{found:?}");
        let given = OsString::from("LUNE_GIVEN=a b=c");
        assert!(found.environ.contains(&given), "{found:?}");
        assert!(found.environ.iter().any(|it| it.as_encoded_bytes().starts_with(b"PATH=")));
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
