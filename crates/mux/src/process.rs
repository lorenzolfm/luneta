//! What the kernel says of a process: its parent, its group, its name, where
//! it sleeps, whether it is stopped, its terminal, and its arguments. Read of
//! the processes of a pane, as the server looks at what runs in front of it.

#[cfg(target_os = "linux")]
#[path = "process/linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "process/macos.rs"]
mod os;

pub use os::{argv, children, process};

/// The most children of a process the server reads.
pub const CHILDREN_MAX: usize = 256;

/// A process, as the kernel says it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: u32,
    /// Its binary's name, as the kernel keeps it: cut to 15 bytes on Linux,
    /// 16 on macOS.
    pub name: String,
    /// Where it sleeps in the kernel, as Linux names it. Empty where it does
    /// not, and on macOS, which says it of no process.
    pub sleeps: String,
    pub stopped: bool,
    /// Its controlling terminal, if it has one.
    pub terminal: Option<Terminal>,
}

/// A process's controlling terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    /// Its device number.
    pub device: u64,
    /// The process group in front of it, if one is.
    pub front: Option<u32>,
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// This process reads as itself: its parent, its group, and its
    /// arguments, as std has them.
    #[test]
    fn this_process_reads_as_itself() {
        let pid = std::process::id();
        let read = process(pid).expect("this process reads");
        assert_eq!(read.pid, pid);
        let parent = rustix::process::getppid().expect("this process has a parent");
        assert_eq!(read.parent, parent.as_raw_nonzero().get().cast_unsigned());
        let group = rustix::process::getpgrp().as_raw_nonzero().get().cast_unsigned();
        assert_eq!(read.group, group);
        assert!(!read.stopped);
        let args: Vec<String> = std::env::args().collect();
        assert_eq!(argv(pid), Some(args));
    }

    /// A child's arguments read as it was given them, an empty one with
    /// them; it is among its parent's children; and stopped, it reads so. Its
    /// name is the binary's that runs, which for macOS's `sh` is bash.
    #[test]
    fn a_child_reads_with_its_arguments_and_stops() {
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", "b c"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let started =
            || argv(pid).is_some_and(|argv| argv.first().is_some_and(|first| first == "sh"));
        while !started() {
            assert!(Instant::now() < deadline, "sh never ran: {:?}", argv(pid));
            std::thread::sleep(Duration::from_millis(5));
        }
        let argv = argv(pid).expect("the child's arguments read");
        assert_eq!(argv, ["sh", "-c", "read line", "", "b c"]);
        assert!(children(std::process::id()).contains(&pid));
        let read = process(pid).expect("the child reads");
        assert_eq!(read.parent, std::process::id());
        assert!(["sh", "bash"].contains(&read.name.as_str()), "{read:?}");
        let raw = rustix::process::Pid::from_raw(pid.cast_signed()).expect("a pid is not 0");
        rustix::process::kill_process(raw, rustix::process::Signal::STOP).expect("it stops");
        while !process(pid).is_some_and(|read| read.stopped) {
            assert!(Instant::now() < deadline, "it never read as stopped");
            std::thread::sleep(Duration::from_millis(5));
        }
        rustix::process::kill_process(raw, rustix::process::Signal::CONT).expect("it goes on");
        child.stdin.take().expect("its stdin").write_all(b"\n").expect("it reads a line");
        child.wait().expect("it ends");
    }

    /// A process that is gone reads as none.
    #[test]
    fn a_process_gone_reads_as_none() {
        let mut child = Command::new("true").spawn().expect("true starts");
        let pid = child.id();
        child.wait().expect("it ends");
        assert_eq!(process(pid), None);
        assert_eq!(argv(pid), None);
        assert_eq!(children(pid), Vec::<u32>::new());
    }
}
