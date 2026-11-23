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

pub use os::{environ, signalled, start, started, Domain};

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
    pub parent: Option<u32>,
    pub name: String,
    pub argv: Vec<OsString>,
    pub environ: Vec<OsString>,
}

impl Found {
    fn forked_from(&self, parent: &Found) -> bool {
        self.parent == Some(parent.id.pid)
            && self.argv == parent.argv
            && self.environ == parent.environ
    }
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

pub const NAME_BYTES_MAX: usize = 15;

#[must_use]
pub fn mine(names: &[&str]) -> Mine {
    let Mine { mut found, unreadable } = os::listed(names);
    found.retain(|it| os::named(it.id.pid).as_ref() == Some(&it.name));
    for it in &found {
        assert!(names.contains(&it.name.as_str()), "{} was not asked for", it.name);
    }
    Mine { found: unforked(found), unreadable }
}

fn unforked(found: Vec<Found>) -> Vec<Found> {
    let forked: Vec<bool> =
        found.iter().map(|it| found.iter().any(|parent| it.forked_from(parent))).collect();
    found.into_iter().zip(forked).filter(|(_, forked)| !forked).map(|(it, _)| it).collect()
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
    use std::io::{self, Read as _, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{self, Command, Stdio};
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

    #[expect(
        unsafe_code,
        reason = "the closure runs in the child between its fork and its exec, and only writes \
                  and reads a pipe, which a signal handler may do"
    )]
    #[test]
    fn a_process_forked_and_not_yet_given_its_own_program_is_not_found_as_its_parent() {
        const NAMED_ON_LINUX_AS: &str = "lune-forks";
        let me = process::id();
        let own = super::os::named(me).expect("this test's process has a name");
        let (mut forked_heard, mut forked_said) = io::pipe().unwrap();
        let (mut go_heard, mut go_said) = io::pipe().unwrap();
        let mut held = Command::new("true");
        unsafe {
            held.pre_exec(move || {
                forked_said.write_all(&[1])?;
                go_heard.read_exact(&mut [0; 1])
            });
        }
        let forks = thread::Builder::new().name(NAMED_ON_LINUX_AS.to_owned());
        let spawned = forks.spawn(move || held.spawn().unwrap().wait().unwrap()).unwrap();
        forked_heard.read_exact(&mut [0; 1]).expect("the child says it was forked");
        let mine = super::mine(&[own.as_str(), NAMED_ON_LINUX_AS]);
        go_said.write_all(&[1]).unwrap();
        assert!(spawned.join().unwrap().success());
        let pids: Vec<u32> = mine.found.iter().map(|it| it.id.pid).collect();
        assert!(pids.contains(&me), "{pids:?} lists this test's process");
        let children = mine.found.iter().filter(|it| it.parent == Some(me));
        let children: Vec<u32> = children.map(|it| it.id.pid).collect();
        assert_eq!(children, Vec::<u32>::new());
    }

    #[test]
    fn only_a_copy_of_a_listed_parent_is_left_out_and_a_child_of_its_own_program_is_kept() {
        let runtime = "XDG_RUNTIME_DIR=/run/user/1000";
        let found = |pid: u32, parent: Option<u32>, argv: &[&str], environ: &str| super::Found {
            id: super::Id { pid, start: super::Start::spelled("1".to_owned()).unwrap() },
            parent,
            name: "luned".to_owned(),
            argv: argv.iter().map(OsString::from).collect(),
            environ: vec![OsString::from(environ)],
        };
        let listed = vec![
            found(1, None, &["luned"], runtime),
            found(2, Some(1), &["luned"], runtime),
            found(3, Some(1), &["luned", "--detached"], runtime),
            found(4, Some(9), &["luned"], runtime),
            found(5, Some(1), &["luned"], "XDG_RUNTIME_DIR=/run/user/1001"),
            found(6, Some(2), &["luned"], runtime),
        ];
        let kept: Vec<u32> = super::unforked(listed).into_iter().map(|it| it.id.pid).collect();
        assert_eq!(kept, [1, 3, 4, 5]);
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
