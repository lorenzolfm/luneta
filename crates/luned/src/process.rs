use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::os::fd::OwnedFd;
use std::process::ChildStdout;

use lune_protocol::program_resume::{ProcessArgv, ProcessPlacement, is_shell_ancestor};
use rustix::io::Errno;
use rustix::process::Pid;
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;

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

#[cfg(target_os = "macos")]
pub use os::{ActiveState, ProcessState, ProcessStateError, StartTime, state};
pub use os::{
    Domain, cwd, environ, meta, open_files, parent, same_root, signalled, start, started,
};

pub const OPEN_FILES_MAX: usize = 4096;

const _: () = assert!(OPEN_FILES_MAX > 0);

fn open_files_over(limit: usize) -> Unread {
    Unread::Failed(format!("there are more than {limit}"))
}

pub const ANCESTOR_DEPTH_MAX: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessMeta {
    pub pid: u32,
    pub parent: u32,
    pub uid: u32,
    pub euid: u32,
    pub name: String,
    pub exe: Option<String>,
    pub argv: Option<ProcessArgv>,
}

#[must_use]
pub fn placement(pid: u32, pty_root_pid: u32) -> ProcessPlacement {
    assert!(pid > 0);
    assert!(pty_root_pid > 0);
    if pid == pty_root_pid {
        return ProcessPlacement::Root;
    }
    if !same_root(pid, pty_root_pid) {
        return ProcessPlacement::Wrapped;
    }
    let Some(first) = meta(pid) else {
        return ProcessPlacement::Wrapped;
    };
    let mut current = first.parent;
    let mut visited = vec![pid];
    for _ in 0..ANCESTOR_DEPTH_MAX {
        if current <= 1 || visited.contains(&current) {
            break;
        }
        visited.push(current);
        let Some(ancestor) = meta(current) else {
            return ProcessPlacement::Wrapped;
        };
        let is_root = ancestor.pid == pty_root_pid;
        if !is_shell_ancestor(
            &ancestor.name,
            ancestor.exe.as_deref(),
            ancestor.argv.as_ref().map(ProcessArgv::words),
            is_root,
        ) {
            return ProcessPlacement::Wrapped;
        }
        if is_root {
            return ProcessPlacement::ShellHosted;
        }
        current = ancestor.parent;
    }
    ProcessPlacement::Wrapped
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentPid(NonZeroU32);

impl AgentPid {
    #[must_use]
    pub const fn new(pid: u32) -> Option<Self> {
        if pid <= 1 {
            return None;
        }
        match NonZeroU32::new(pid) {
            Some(pid) => Some(Self(pid)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for AgentPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId {
    pid: AgentPid,
    start: Start,
}

impl AgentId {
    #[must_use]
    pub fn new(pid: AgentPid, start: Start) -> Self {
        Self { pid, start }
    }

    #[must_use]
    pub const fn pid(&self) -> AgentPid {
        self.pid
    }

    #[must_use]
    pub fn start(&self) -> &Start {
        &self.start
    }

    #[must_use]
    pub fn to_process_id(&self) -> Id {
        Id {
            pid: self.pid.get(),
            start: self.start.clone(),
        }
    }
}

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
        (!spelling.trim().is_empty()).then_some(Start(spelling))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found<P> {
    pub id: Id,
    pub program: P,
    pub argv: Vec<OsString>,
    pub environ: Vec<OsString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unread {
    Gone,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable<P> {
    pub id: Id,
    pub program: P,
    pub why: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mine<P> {
    pub found: Vec<Found<P>>,
    pub unreadable: Vec<Unreadable<P>>,
}

impl<P> Default for Mine<P> {
    fn default() -> Mine<P> {
        Mine {
            found: Vec::new(),
            unreadable: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unended {
    Outlived,
    Unsignalled(Errno),
    Unwatched(Errno),
}

pub const NAME_BYTES_MAX: usize = 15;

#[must_use]
pub fn mine<P: Copy + Eq>(programs: &[(P, &str)]) -> Mine<P> {
    let Mine {
        mut found,
        unreadable,
    } = os::listed(programs);
    found.retain(|it| {
        let named = os::named(it.id.pid);
        programs
            .iter()
            .any(|&(program, name)| program == it.program && named.as_deref() == Some(name))
    });
    Mine { found, unreadable }
}

fn program_named<P: Copy>(programs: &[(P, &str)], name: &str) -> Option<P> {
    programs
        .iter()
        .find(|&&(_, named)| named == name)
        .map(|&(program, _)| program)
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

pub struct Exit(os::Exit);

impl Exit {
    pub fn watch(pid: Pid) -> io::Result<Option<Exit>> {
        os::Exit::watch(pid).map(|exit| exit.map(Exit))
    }

    pub fn signal(&self, signal: rustix::process::Signal) -> Result<(), Errno> {
        self.0.signal(signal)
    }

    pub fn has_exited(&self) -> io::Result<bool> {
        self.0.has_exited()
    }

    pub async fn wait(self) -> io::Result<()> {
        self.0.wait().await
    }
}

pub use lune_protocol::{READY_BYTES_MAX, ReadyPrefix};

const READY_READS_MAX: usize = READY_BYTES_MAX * 2;

pub(super) const SPURIOUS_WAKES_MAX: usize = 1024;

const _: () = assert!(READY_READS_MAX >= READY_BYTES_MAX);
const _: () = assert!(SPURIOUS_WAKES_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyLine {
    Matched,
    Ended,
    Overflow,
}

#[expect(
    unsafe_code,
    reason = "AsyncFd::register_with_interest requires an open fd whose number stays fixed for \
              its lifetime; an OwnedFd owns its descriptor exclusively until AsyncFd drops it"
)]
fn readable_fd(fd: OwnedFd) -> io::Result<AsyncFd<OwnedFd>> {
    unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) }.map_err(io::Error::from)
}

pub async fn read_ready_line(stdout: ChildStdout, prefix: ReadyPrefix) -> io::Result<ReadyLine> {
    rustix::io::ioctl_fionbio(&stdout, true).map_err(io::Error::from)?;
    let ready = readable_fd(OwnedFd::from(stdout))?;
    let mut line = Vec::with_capacity(prefix.len());
    let mut chunk = [0_u8; 512];
    let mut bytes_total = 0_usize;
    for _ in 0..READY_READS_MAX {
        let mut guard = ready.readable().await?;
        let read = guard
            .try_io(|inner| rustix::io::read(inner.get_ref(), &mut chunk).map_err(io::Error::from));
        match read {
            Ok(Ok(0)) => return Ok(ReadyLine::Ended),
            Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
            Ok(Err(error)) => return Err(error),
            Ok(Ok(bytes_read)) => {
                assert!(
                    bytes_read <= chunk.len(),
                    "read returned more bytes than chunk capacity"
                );
                for &byte in &chunk[..bytes_read] {
                    bytes_total += 1;
                    if bytes_total > READY_BYTES_MAX {
                        return Ok(ReadyLine::Overflow);
                    }
                    if byte == b'\n' {
                        if line.as_slice() == prefix.as_bytes() {
                            return Ok(ReadyLine::Matched);
                        }
                        line.clear();
                    } else if line.len() < prefix.len() {
                        line.push(byte);
                    }
                }
            }
            Err(_would_block) => {}
        }
    }
    Err(io::Error::other(
        "stdout stayed unread across too many wakeups",
    ))
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
            assert!(
                before <= started,
                "dated {started:?}, started after {before:?}"
            );
            assert!(
                started <= after + Duration::from_secs(1),
                "dated {started:?}, by {after:?}"
            );
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
        given
            .env_clear()
            .env("PATH", &path)
            .env("LUNE_GIVEN", "a b=c");
        let mut child = given.spawn().unwrap();
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut [0; 1])
            .expect("it says it runs");
        let read = super::environ(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        let read = read.expect("this user's process has an environment that reads");
        assert!(read.len() >= 2, "{read:?}");
        let mut given = read[..2].to_vec();
        given.sort_unstable();
        let mut path_given = OsString::from("PATH=");
        path_given.push(path);
        assert_eq!(
            given,
            [OsString::from("LUNE_GIVEN=a b=c"), path_given],
            "{read:?}"
        );
    }

    #[test]
    fn a_process_of_mine_reads_its_empty_arguments_and_its_whole_environment() {
        let path = env::var_os("PATH").expect("the tests run with a PATH");
        let mut given = Command::new("sh");
        given.args(["-c", "echo; read -r line", "", ""]);
        given.stdin(Stdio::piped()).stdout(Stdio::piped());
        given
            .env_clear()
            .env("PATH", &path)
            .env("LUNE_GIVEN", "a b=c");
        let mut child = given.spawn().unwrap();
        child
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut [0; 1])
            .expect("it says it runs");
        let mine = super::mine(&[("sh", "sh"), ("bash", "bash")]);
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        let unreadable = mine.unreadable.iter().find(|it| it.id.pid == pid);
        assert_eq!(unreadable, None, "{:?}", mine.unreadable);
        let found = mine.found.into_iter().find(|it| it.id.pid == pid);
        let found = found.expect("this user's shell is found");
        assert_eq!(
            found.argv[1..],
            ["-c", "echo; read -r line", "", ""],
            "{found:?}"
        );
        let given = OsString::from("LUNE_GIVEN=a b=c");
        assert!(found.environ.contains(&given), "{found:?}");
        assert!(
            found
                .environ
                .iter()
                .any(|it| it.as_encoded_bytes().starts_with(b"PATH="))
        );
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
                forked_said.write_all(&process::id().to_ne_bytes())?;
                go_heard.read_exact(&mut [0; 1])
            });
        }
        let forks = thread::Builder::new().name(NAMED_ON_LINUX_AS.to_owned());
        let spawned = forks
            .spawn(move || held.spawn().unwrap().wait().unwrap())
            .unwrap();
        let mut forked = [0; 4];
        forked_heard
            .read_exact(&mut forked)
            .expect("the child says its pid once forked");
        let forked = u32::from_ne_bytes(forked);
        let names = [own.as_str(), NAMED_ON_LINUX_AS];
        let held_as = super::os::named(forked);
        let mine = super::mine(&names.map(|name| (name, name)));
        go_said.write_all(&[1]).unwrap();
        assert!(spawned.join().unwrap().success());
        let held_as = held_as.expect("the held child has a name");
        assert!(
            names.contains(&held_as.as_str()),
            "{held_as} is a name asked for"
        );
        let pids: Vec<u32> = mine.found.iter().map(|it| it.id.pid).collect();
        assert!(pids.contains(&me), "{pids:?} lists this test's process");
        assert!(
            !pids.contains(&forked),
            "{pids:?} lists the held child {forked}"
        );
    }

    #[test]
    fn where_a_process_runs_reads_while_it_runs_and_is_gone_once_it_ended() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let mut child = Command::new("sleep")
            .arg("60")
            .current_dir(&dir)
            .spawn()
            .unwrap();
        let pid = child.id();
        assert_eq!(super::cwd(pid), Ok(dir));
        child.kill().unwrap();
        hears_it_ended(pid, "ended, and not waited for");
        assert_eq!(
            super::cwd(pid),
            Err(super::Unread::Gone),
            "an ended process runs nowhere"
        );
        child.wait().unwrap();
        assert_eq!(super::cwd(pid), Err(super::Unread::Gone));
    }

    #[test]
    fn the_files_a_process_holds_open_read_while_it_runs_and_are_gone_once_it_ended() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let held = dir.join("held.lock");
        let file = std::fs::File::create(&held).unwrap();
        let most = super::OPEN_FILES_MAX;
        let mine = super::open_files(std::process::id(), most).unwrap();
        assert!(mine.contains(&held), "{mine:?} lists {held:?}");
        assert!(mine.len() <= most);
        let other = std::fs::File::open(&dir).unwrap();
        assert_eq!(
            super::open_files(std::process::id(), 1),
            Err(super::Unread::Failed("there are more than 1".to_owned())),
            "a list cut at its limit would leave out a file without saying so"
        );
        drop(other);
        drop(file);
        let after = super::open_files(std::process::id(), most).unwrap();
        assert!(!after.contains(&held), "{after:?} still lists {held:?}");

        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = child.id();
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(super::open_files(pid, most), Err(super::Unread::Gone));
    }

    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }

    #[test]
    fn an_exit_watch_wakes_as_its_process_ends_and_not_before() {
        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = rustix::process::Pid::from_child(&child);
        let exit = super::Exit::watch(pid)
            .unwrap()
            .expect("a running child is watched");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (done, heard) = mpsc::channel();
        let waiter = thread::spawn(move || {
            let ended = runtime.block_on(exit.wait());
            let _ = done.send(ended);
        });
        lune_waits::never("an exit watch wakes while its process runs", || {
            heard.try_recv().ok()
        });
        child.kill().unwrap();
        lune_waits::heard("an exit watch wakes when its process ends", &heard).unwrap();
        waiter.join().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn an_exit_watch_reports_has_exited_only_after_its_process_ends() {
        use std::sync::Barrier;

        let mut child = Command::new("sleep").arg("60").spawn().unwrap();
        let pid = rustix::process::Pid::from_child(&child);
        let exit = super::Exit::watch(pid)
            .unwrap()
            .expect("a running child is watched");
        assert!(!exit.has_exited().unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        lune_waits::eventually("has_exited becomes true once the process exits", || {
            exit.has_exited().unwrap().then_some(())
        });
        let barrier = Barrier::new(4);
        thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    barrier.wait();
                    assert!(exit.has_exited().unwrap());
                });
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(lune_waits::awaited(
                "wait completes after has_exited observed exit",
                exit.wait(),
            ))
            .unwrap();
    }

    #[test]
    fn concurrent_has_exited_calls_never_revert_to_false_after_exit() {
        use std::sync::Barrier;
        use std::sync::atomic::{AtomicBool, Ordering};

        const WORKERS: usize = 4;
        const ROUNDS: usize = 8;

        let patience = lune_waits::Patience::begun();
        for _ in 0..ROUNDS {
            let mut child = Command::new("sleep").arg("60").spawn().unwrap();
            let pid = rustix::process::Pid::from_child(&child);
            let exit = super::Exit::watch(pid)
                .unwrap()
                .expect("a running child is watched");
            child.kill().unwrap();
            child.wait().unwrap();
            let barrier = Barrier::new(WORKERS);
            let saw_exited = AtomicBool::new(false);
            thread::scope(|scope| {
                for _ in 0..WORKERS {
                    scope.spawn(|| {
                        barrier.wait();
                        lune_waits::eventually("concurrent has_exited observes exit", || {
                            let already = saw_exited.load(Ordering::Acquire);
                            let exited = exit.has_exited().unwrap();
                            assert!(
                                !already || exited,
                                "has_exited reverted to false after another call observed exit"
                            );
                            if exited {
                                saw_exited.store(true, Ordering::Release);
                            }
                            exited.then_some(())
                        });
                        assert!(exit.has_exited().unwrap());
                    });
                }
            });
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime
                .block_on(
                    patience.awaited("wait completes after concurrent has_exited", exit.wait()),
                )
                .unwrap();
        }
    }

    #[test]
    fn a_ready_line_wakes_on_its_prefix_or_eof() {
        let prefix = super::ReadyPrefix::try_new("luned: listening on ").unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut child = Command::new("sh")
            .args([
                "-c",
                "echo noise; echo 'luned: listening on /tmp/s'; exec sleep 60",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let heard = runtime.block_on(lune_waits::awaited(
            "the ready line arrives while the child runs on",
            super::read_ready_line(stdout, prefix),
        ));
        assert_eq!(heard.unwrap(), super::ReadyLine::Matched);
        assert!(
            child.try_wait().unwrap().is_none(),
            "the child keeps running after saying it listens"
        );
        child.kill().unwrap();
        child.wait().unwrap();

        let mut silent = Command::new("true").stdout(Stdio::piped()).spawn().unwrap();
        let stdout = silent.stdout.take().unwrap();
        let heard = runtime.block_on(lune_waits::awaited(
            "an exiting child closes its stdout",
            super::read_ready_line(stdout, prefix),
        ));
        assert_eq!(heard.unwrap(), super::ReadyLine::Ended);
        silent.wait().unwrap();

        let mut noisy = Command::new("head")
            .args(["-c", &(super::READY_BYTES_MAX + 1).to_string(), "/dev/zero"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = noisy.stdout.take().unwrap();
        let heard = runtime.block_on(lune_waits::awaited(
            "a noisy child hits the ready byte bound",
            super::read_ready_line(stdout, prefix),
        ));
        assert_eq!(heard.unwrap(), super::ReadyLine::Overflow);
        let _ = noisy.kill();
        let _ = noisy.wait();
    }
}
