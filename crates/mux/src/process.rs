//! What the kernel says of a process: its parent, its group, its name, where
//! it sleeps, whether it is stopped, its terminal, and its arguments. Read of
//! the processes of a pane, as the server looks at what runs in front of it.

#[cfg(target_os = "linux")]
#[path = "process/linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "process/macos.rs"]
mod os;

pub use os::{children, process};

/// The most children of a process the server reads.
pub const CHILDREN_MAX: usize = 256;

/// The most bytes of a process's arguments the server keeps, a NUL after
/// each among them: what runs in front names a pane's command, which needs
/// no more, and each listing holds it.
pub const ARGV_BYTES_MAX: usize = 16 * 1024;

/// The arguments of `pid`, as `os::argv` reads them, cut to
/// `ARGV_BYTES_MAX`: none where it is gone.
pub fn argv(pid: u32) -> Option<Vec<String>> {
    os::argv(pid).map(bounded)
}

/// `args` up to `ARGV_BYTES_MAX` bytes, a NUL after each counted: the one
/// that runs past them is cut at a character's boundary, and those after it
/// are left out.
fn bounded(args: Vec<String>) -> Vec<String> {
    let mut room = ARGV_BYTES_MAX;
    let mut kept = Vec::new();
    for mut arg in args {
        if room == 0 {
            break;
        }
        if arg.len() < room {
            room -= arg.len() + 1;
            kept.push(arg);
            continue;
        }
        let mut cut = room - 1;
        while !arg.is_char_boundary(cut) {
            cut -= 1;
        }
        arg.truncate(cut);
        kept.push(arg);
        break;
    }
    assert!(kept.iter().map(|arg| arg.len() + 1).sum::<usize>() <= ARGV_BYTES_MAX);
    kept
}

/// A process, as the kernel says it now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: Group,
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
    pub front: Option<Group>,
}

/// A process group, by the pid of its leader, which names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group(pub u32);

impl Group {
    /// The pid of its leader, which may be gone while the group is not.
    #[must_use]
    pub fn leader(self) -> u32 {
        self.0
    }
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
        assert_eq!(read.group, Group(group));
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

    /// Arguments past `ARGV_BYTES_MAX` are cut there, the one that runs past
    /// it at a character's boundary.
    #[test]
    fn arguments_are_cut_to_their_bound() {
        let long = "é".repeat(ARGV_BYTES_MAX);
        let args = bounded(vec!["abc".to_owned(), long.clone(), "d".to_owned()]);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "abc");
        assert!(long.starts_with(&args[1]));
        assert_eq!(args[1].len(), ARGV_BYTES_MAX - 6);
        let short = vec!["a".to_owned(), String::new()];
        assert_eq!(bounded(short.clone()), short);
    }

    /// A child given an argument longer than the bound reads cut to it.
    #[test]
    fn a_child_s_long_argument_reads_cut() {
        let long = "a".repeat(4 * ARGV_BYTES_MAX);
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", &long])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(5);
        let started =
            || argv(pid).is_some_and(|argv| argv.first().is_some_and(|first| first == "sh"));
        while !started() {
            assert!(Instant::now() < deadline, "sh never ran: {:?}", argv(pid).map(|args| args.len()));
            std::thread::sleep(Duration::from_millis(5));
        }
        let argv = argv(pid).expect("the child's arguments read");
        assert_eq!(argv[..4], ["sh", "-c", "read line", ""]);
        assert_eq!(argv[4], long[..ARGV_BYTES_MAX - 18]);
        assert_eq!(argv.len(), 5);
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
