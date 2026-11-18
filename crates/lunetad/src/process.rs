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
compile_error!("lunetad reads processes on Linux and macOS, and on nothing else yet");

pub use os::{environ, mine, signalled, start, started, Domain};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id {
    pub pid: u32,
    pub start: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub id: Id,
    pub name: String,
    pub argv: Vec<String>,
    pub environ: Vec<OsString>,
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
    use std::process::Command;
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

    fn heard_within(pid: u32, limit: Duration) -> bool {
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || ended.send(super::ends(pid)));
        heard.recv_timeout(limit).map(|said| said.unwrap()).is_ok()
    }

    #[test]
    fn the_end_of_a_process_is_heard_as_it_comes() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || ended.send(super::ends(pid)));
        assert!(heard.recv_timeout(Duration::from_millis(100)).is_err(), "it still runs");
        assert!(super::start(pid).is_some());

        child.kill().unwrap();
        heard.recv_timeout(Duration::from_secs(5)).expect("it was killed").unwrap();
        assert_eq!(super::start(pid), None);
        assert!(heard_within(pid, Duration::from_secs(5)), "ended, and not waited for");

        child.wait().unwrap();
        assert_eq!(super::start(pid), None);
        assert!(heard_within(pid, Duration::from_secs(5)), "ended, and waited for");
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
