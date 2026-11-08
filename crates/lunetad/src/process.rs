//! What the daemon reads of another process: when it started, spelled as Claude
//! Code records it and as a time, the environment it started with, and the
//! command line it runs; and when it ends, which it hears. Each system has its
//! own way to say them, and a backend of its own.

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

pub use os::{args, environ, start, started, Domain};

/// Waits for the process `pid` to end, or says why it cannot tell. One that has
/// ended already has at once, whether or not its parent has waited for it. A
/// pid the system gives no process says so, as nothing by it has ended.
pub fn ends(pid: u32) -> io::Result<()> {
    match i32::try_from(pid).ok().and_then(Pid::from_raw) {
        Some(pid) => os::ends(pid),
        None => Err(Errno::INVAL.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, SystemTime};

    /// A process is dated no earlier than it started, wherever in a clock
    /// tick or a second it starts, so that the session it started in is not
    /// ruled out for being made a moment before; and less than a second
    /// later.
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

    /// A process's command line reads as it was started, an argument to a
    /// string, an empty one and one with a space among them; and one that
    /// ended has none. The shell runs on past its sleep, so it stays a shell.
    /// It is read once the shell has said it runs: the kernel lets its parent
    /// go on from `exec` before it has laid out the arguments to read.
    #[test]
    fn a_process_s_command_line_reads_as_it_was_started() {
        let script = "echo; sleep 60; :";
        let mut shell = Command::new("sh");
        shell.args(["-c", script, "a b", ""]).stdout(Stdio::piped());
        let mut child = shell.spawn().unwrap();
        let mut said = [0];
        child.stdout.take().unwrap().read_exact(&mut said).unwrap();
        let args = super::args(child.id()).expect("a process that runs has its arguments");
        let args: Vec<&str> = args.iter().map(|arg| arg.to_str().unwrap()).collect();
        assert_eq!(args[1..], ["-c", script, "a b", ""], "{args:?}");
        assert!(args[0].ends_with("sh"), "{args:?}");
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(super::args(child.id()), None);
    }

    /// Whether `ends` says the process `pid` has ended within `limit`, as it
    /// waits in a thread of its own.
    fn heard_within(pid: u32, limit: Duration) -> bool {
        let (ended, heard) = mpsc::channel();
        thread::spawn(move || ended.send(super::ends(pid)));
        heard.recv_timeout(limit).map(|said| said.unwrap()).is_ok()
    }

    /// A process that runs is not heard to end, and one that is killed is, at
    /// once: while its parent has yet to wait for it, which leaves it dated by
    /// nothing, as it is no longer running, and once its parent has, which
    /// leaves no process by its pid to ask about.
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

    /// A pid the system gives no process is not said to have ended.
    #[test]
    fn no_end_is_heard_of_a_pid_no_process_can_have() {
        assert!(super::ends(0).is_err());
        assert!(super::ends(u32::MAX).is_err());
    }
}
