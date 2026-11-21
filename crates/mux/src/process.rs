#[cfg(target_os = "linux")]
#[path = "process/linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "process/macos.rs"]
mod os;

pub use os::{children, process};

pub const CHILDREN_MAX: usize = 256;

pub const ARGV_BYTES_MAX: usize = 16 * 1024;

pub fn argv(pid: u32) -> Option<Vec<String>> {
    os::argv(pid).map(bounded)
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub group: Group,
    pub name: String,
    pub sleeps: String,
    pub stopped: bool,
    pub terminal: Option<Terminal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub device: u64,
    pub front: Option<Group>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Group(pub u32);

impl Group {
    #[must_use]
    pub fn leader(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use luneta_waits::{eventually, eventually_showing};

    use super::*;

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

    #[test]
    fn a_child_reads_with_its_arguments_and_stops() {
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", "b c"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let pid = child.id();
        let argv = sh_ran(pid, |argv| argv);
        assert_eq!(argv, ["sh", "-c", "read line", "", "b c"]);
        assert!(children(std::process::id()).contains(&pid));
        let read = process(pid).expect("the child reads");
        assert_eq!(read.parent, std::process::id());
        assert!(["sh", "bash"].contains(&read.name.as_str()), "{read:?}");
        let raw = rustix::process::Pid::from_raw(pid.cast_signed()).expect("a pid is not 0");
        rustix::process::kill_process(raw, rustix::process::Signal::STOP).expect("it stops");
        eventually("it reads as stopped", || process(pid).filter(|read| read.stopped));
        rustix::process::kill_process(raw, rustix::process::Signal::CONT).expect("it goes on");
        child.stdin.take().expect("its stdin").write_all(b"\n").expect("it reads a line");
        child.wait().expect("it ends");
    }

    #[test]
    fn a_lone_empty_argument_reads_as_one() {
        let mut child = Command::new("sh")
            .arg0("")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let script = b"echo x\n";
        child.stdin.as_mut().expect("its stdin").write_all(script).expect("it reads its script");
        child.stdout.as_mut().expect("its stdout").read_exact(&mut [0; 2]).expect("it runs");
        let read = argv(child.id());
        drop(child.stdin.take());
        child.wait().expect("it ends");
        assert_eq!(read, Some(vec![String::new()]));
    }

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

    #[test]
    fn a_child_s_long_argument_reads_cut() {
        let long = "a".repeat(4 * ARGV_BYTES_MAX);
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", &long])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let argv = sh_ran(child.id(), |argv| argv.map(|args| args.len()));
        assert_eq!(argv[..4], ["sh", "-c", "read line", ""]);
        assert_eq!(argv[4], long[..ARGV_BYTES_MAX - 18]);
        assert_eq!(argv.len(), 5);
        child.stdin.take().expect("its stdin").write_all(b"\n").expect("it reads a line");
        child.wait().expect("it ends");
    }

    #[test]
    fn a_process_gone_reads_as_none() {
        let mut child = Command::new("true").spawn().expect("true starts");
        let pid = child.id();
        child.wait().expect("it ends");
        assert_eq!(process(pid), None);
        assert_eq!(argv(pid), None);
        assert_eq!(children(pid), Vec::<u32>::new());
    }

    fn sh_ran<E: Debug>(pid: u32, shown: impl Fn(Option<Vec<String>>) -> E) -> Vec<String> {
        eventually_showing("sh runs", || match argv(pid) {
            Some(argv) if argv.first().is_some_and(|first| first == "sh") => Ok(argv),
            other => Err(shown(other)),
        })
    }
}
