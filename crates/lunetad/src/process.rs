use std::env;
use std::ffi::OsString;
use std::io;
#[cfg(any(target_os = "macos", test))]
use std::os::unix::ffi::OsStringExt as _;

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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

#[cfg(any(target_os = "macos", test))]
fn environ_in_procargs(procargs: &[u8]) -> Option<Vec<OsString>> {
    let (count, strings) = procargs.split_first_chunk::<{ size_of::<i32>() }>()?;
    let argument_count = usize::try_from(i32::from_ne_bytes(*count)).ok()?;
    let path_end = strings.iter().position(|byte| *byte == 0).unwrap_or(strings.len());
    let padded = &strings[path_end..];
    let arguments_start = padded.iter().position(|byte| *byte != 0).unwrap_or(padded.len());
    let mut each = padded[arguments_start..].split(|byte| *byte == 0);
    for _ in 0..argument_count {
        each.next()?;
    }
    let entries = each.skip_while(|entry| entry.is_empty()).take_while(|entry| !entry.is_empty());
    Some(entries.map(|entry| OsString::from_vec(entry.to_vec())).collect())
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
    fn an_environment_reads_as_it_was_given() {
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let mut given = Command::new("sh");
        given.args(["-c", "echo; read -r line"]).stdin(Stdio::piped()).stdout(Stdio::piped());
        given.env_clear().env("PATH", &path).env("LUNETA_GIVEN", "a b=c");
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
        assert_eq!(given, [OsString::from("LUNETA_GIVEN=a b=c"), path_given], "{read:?}");
    }

    fn procargs(argument_count: i32, strings: &[&str]) -> Vec<u8> {
        let mut procargs = argument_count.to_ne_bytes().to_vec();
        for string in strings {
            procargs.extend_from_slice(string.as_bytes());
            procargs.push(0);
        }
        procargs
    }

    #[test]
    fn an_environment_starts_after_as_many_arguments_as_argc() {
        let read = |argument_count, strings: &[&str]| {
            let read = super::environ_in_procargs(&procargs(argument_count, strings));
            read.map(|entries| entries.into_iter().map(|it| it.into_string().unwrap()).collect())
        };
        let laid = ["/bin/sleep", "", "", "sleep", "60", "A=b", "PATH=/bin", "", "", "ptr_munge="];
        assert_eq!(read(2, &laid), Some(vec!["A=b".to_owned(), "PATH=/bin".to_owned()]));
        let empty_argument = ["/bin/echo", "", "echo", "", "x", "A=b", "", "main_stack="];
        let as_given = Some(vec!["A=b".to_owned()]);
        assert_eq!(read(3, &empty_argument), as_given, "an empty argument is one");
        let titled = [&["/usr/bin/node", "", "claude"][..], &[""; 17], &["A=b", ""]].concat();
        assert_eq!(read(3, &titled), as_given, "a title leaves zeros where its arguments were");
        assert_eq!(read(0, &["/bin/true", "", "", ""]), Some(Vec::new()));
        assert_eq!(read(5, &["/bin/sleep", "", "sleep", "60"]), None, "fewer than its argc");
        assert_eq!(read(-1, &["/bin/sleep"]), None);
        assert_eq!(super::environ_in_procargs(&[1, 0]), None, "no argc");
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
