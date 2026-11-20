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
const EXEC_PATH_ALIGNMENT: usize = size_of::<u64>();

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
struct Laid {
    argv: Vec<OsString>,
    environ: Vec<OsString>,
}

#[cfg(any(target_os = "macos", test))]
fn laid_in_procargs(procargs: &[u8]) -> Result<Laid, &'static str> {
    let fewer = "KERN_PROCARGS2 holds fewer arguments than its argc";
    let (count, strings) = procargs
        .split_first_chunk::<{ size_of::<i32>() }>()
        .ok_or("KERN_PROCARGS2 holds no argc")?;
    let argument_count = usize::try_from(i32::from_ne_bytes(*count))
        .map_err(|_| "KERN_PROCARGS2 holds a negative argc")?;
    let path_end = strings
        .iter()
        .position(|byte| *byte == 0)
        .ok_or("KERN_PROCARGS2's exec path does not end")?;
    let arguments_start = (path_end + 1).next_multiple_of(EXEC_PATH_ALIGNMENT);
    let padding = strings
        .get(path_end..arguments_start)
        .ok_or("KERN_PROCARGS2 ends in its exec path's padding")?;
    if padding.iter().any(|byte| *byte != 0) {
        return Err("KERN_PROCARGS2 pads its exec path with more than zeros");
    }
    let mut rest = &strings[arguments_start..];
    if rest.len() < argument_count {
        return Err(fewer);
    }
    let mut argv = Vec::with_capacity(argument_count);
    for _ in 0..argument_count {
        let argument_end = rest.iter().position(|byte| *byte == 0).ok_or(fewer)?;
        argv.push(OsString::from_vec(rest[..argument_end].to_vec()));
        rest = &rest[argument_end + 1..];
    }
    let environ_start = rest.iter().position(|byte| *byte != 0).unwrap_or(rest.len());
    let mut environ = Vec::new();
    for entry in rest[environ_start..].split_inclusive(|byte| *byte == 0) {
        let entry = entry.strip_suffix(&[0]).ok_or("KERN_PROCARGS2's environment does not end")?;
        if entry.is_empty() {
            break;
        }
        environ.push(OsString::from_vec(entry.to_vec()));
    }
    Ok(Laid { argv, environ })
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
        given.arg0("").args(["-c", "echo; read -r line"]);
        given.stdin(Stdio::piped()).stdout(Stdio::piped());
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

    #[test]
    fn a_process_of_mine_reads_its_empty_arguments_and_its_whole_environment() {
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let mut given = Command::new("sh");
        given.args(["-c", "echo; read -r line", "", ""]);
        given.stdin(Stdio::piped()).stdout(Stdio::piped());
        given.env_clear().env("PATH", &path).env("LUNETA_GIVEN", "a b=c");
        let mut child = given.spawn().unwrap();
        child.stdout.as_mut().unwrap().read_exact(&mut [0; 1]).expect("it says it runs");
        let found = super::mine(&["sh", "bash"]).into_iter().find(|it| it.id.pid == child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        let found = found.expect("this user's shell is found");
        assert_eq!(found.argv[1..], ["-c", "echo; read -r line", "", ""], "{found:?}");
        let given = OsString::from("LUNETA_GIVEN=a b=c");
        assert!(found.environ.contains(&given), "{found:?}");
        assert!(found.environ.iter().any(|it| it.as_encoded_bytes().starts_with(b"PATH=")));
    }

    fn procargs(argument_count: i32, path: &str, strings: &[&str]) -> Vec<u8> {
        let mut procargs = argument_count.to_ne_bytes().to_vec();
        let mut padded = path.as_bytes().to_vec();
        padded.resize((path.len() + 1).next_multiple_of(8), 0);
        procargs.extend_from_slice(&padded);
        for string in strings {
            procargs.extend_from_slice(string.as_bytes());
            procargs.push(0);
        }
        procargs
    }

    #[test]
    fn an_environment_starts_after_as_many_arguments_as_argc() {
        let read = |argument_count, path, strings: &[&str]| {
            let read = super::laid_in_procargs(&procargs(argument_count, path, strings));
            read.map(|laid| {
                laid.environ.into_iter().map(|it| it.into_string().unwrap()).collect::<Vec<_>>()
            })
        };
        let given = |entries: &[&str]| Ok(entries.iter().map(|it| (*it).to_owned()).collect());
        let laid = ["sleep", "60", "A=b", "PATH=/bin", "", "", "ptr_munge="];
        assert_eq!(read(2, "/bin/sleep", &laid), given(&["A=b", "PATH=/bin"]));
        let empty_argument = ["echo", "", "x", "A=b", "", "main_stack="];
        assert_eq!(read(3, "/bin/echo", &empty_argument), given(&["A=b"]), "an empty argument");
        let empty_first = ["", "60", "A=b", "B=c", ""];
        assert_eq!(read(2, "/bin/sleep", &empty_first), given(&["A=b", "B=c"]), "an empty first");
        let unpadded = ["", "A=b", ""];
        assert_eq!(read(1, "/bin/sh", &unpadded), given(&["A=b"]), "a path its alignment fits");
        let titled = [&["claude"][..], &[""; 17], &["A=b", ""]].concat();
        let why = "a title leaves zeros where its arguments were";
        assert_eq!(read(3, "/usr/bin/node", &titled), given(&["A=b"]), "{why}");
        assert_eq!(read(0, "/bin/true", &["", ""]), given(&[]));

        let fewer = Err("KERN_PROCARGS2 holds fewer arguments than its argc");
        assert_eq!(read(5, "/bin/sleep", &["sleep", "60"]), fewer);
        assert_eq!(read(3, "/bin/sleep", &["sleep", "60"]), fewer, "its end ends no argument");
        assert_eq!(read(-1, "/bin/sleep", &[]), Err("KERN_PROCARGS2 holds a negative argc"));
        let raw = |argument_count: i32, strings: &[u8]| {
            super::laid_in_procargs(&[&argument_count.to_ne_bytes()[..], strings].concat())
                .map(|laid| laid.environ)
        };
        assert_eq!(super::laid_in_procargs(&[1, 0]), Err("KERN_PROCARGS2 holds no argc"));
        let unended = Err("KERN_PROCARGS2's exec path does not end");
        assert_eq!(raw(0, b"/bin/true"), unended);
        let unpadded = Err("KERN_PROCARGS2 ends in its exec path's padding");
        assert_eq!(raw(0, b"/bin/x\0"), unpadded);
        let misaligned = Err("KERN_PROCARGS2 pads its exec path with more than zeros");
        assert_eq!(raw(1, b"/bin/x\0y\0A=b\0"), misaligned);
        let cut = [&procargs(1, "/bin/sh", &["sh", "A=b"])[..], b"B="].concat();
        let cut_entry = Err("KERN_PROCARGS2's environment does not end");
        assert_eq!(super::laid_in_procargs(&cut), cut_entry);
    }

    #[test]
    fn arguments_read_as_argc_strings_after_the_exec_path_empty_ones_too() {
        let read = |argument_count, path, strings: &[&str]| {
            let laid = super::laid_in_procargs(&procargs(argument_count, path, strings)).unwrap();
            laid.argv.into_iter().map(|it| it.into_string().unwrap()).collect::<Vec<_>>()
        };
        let empty_argument = ["echo", "", "x", "A=b", "", "main_stack="];
        assert_eq!(read(3, "/bin/echo", &empty_argument), ["echo", "", "x"]);
        let empty_last = ["sh", "-c", "", "A=b", ""];
        assert_eq!(read(3, "/bin/sh", &empty_last), ["sh", "-c", ""]);
        let empty_first = ["", "60", "A=b", ""];
        assert_eq!(read(2, "/bin/sleep", &empty_first), ["", "60"]);
        let unpadded = ["", "A=b", ""];
        assert_eq!(read(1, "/bin/sh", &unpadded), [""]);
        assert_eq!(read(0, "/bin/true", &["", ""]), Vec::<String>::new());
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
