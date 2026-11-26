use std::fmt;
use std::num::NonZeroU32;

#[cfg(target_os = "linux")]
#[path = "process/linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "process/macos.rs"]
mod os;

pub use os::{children, owner_info, process, same_root};

pub const CHILDREN_MAX: usize = 256;

pub const ARGV_BYTES_MAX: usize = lune_protocol::program_resume::ARGV_BYTES_MAX;

pub const ANCESTOR_DEPTH_MAX: usize = 64;

#[must_use]
pub fn argv(pid: ProcessId) -> Option<Argv> {
    os::argv(pid).and_then(bounded)
}

fn bounded(args: Vec<String>) -> Option<Argv> {
    if args.is_empty() {
        return None;
    }
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
    assert_ne!(kept.len(), 0);
    assert!(kept.iter().map(|arg| arg.len() + 1).sum::<usize>() <= ARGV_BYTES_MAX);
    Some(Argv(kept))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Argv(Vec<String>);

impl Argv {
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessId(NonZeroU32);

impl ProcessId {
    #[must_use]
    pub const fn new(raw: u32) -> Option<ProcessId> {
        match NonZeroU32::new(raw) {
            Some(pid) => Some(ProcessId(pid)),
            None => None,
        }
    }

    #[must_use]
    pub fn of(pid: rustix::process::Pid) -> ProcessId {
        let raw = u32::try_from(pid.as_raw_nonzero().get()).expect("a Unix pid is positive");
        ProcessId(NonZeroU32::new(raw).expect("a Unix pid is non-zero"))
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[must_use]
pub fn process_argv(pid: u32) -> Option<lune_protocol::program_resume::ProcessArgv> {
    os::argv(ProcessId::new(pid)?).map(lune_protocol::program_resume::ProcessArgv::bounded)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerInfo {
    pub pid: u32,
    pub parent: u32,
    pub uid: u32,
    pub euid: u32,
    pub proc_start: String,
    pub name: String,
    pub exe: Option<String>,
    pub cwd: Option<String>,
    pub terminal_device: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Process {
    pub pid: ProcessId,
    pub parent: Option<ProcessId>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Group(NonZeroU32);

impl Group {
    #[must_use]
    pub const fn new(raw: u32) -> Option<Group> {
        match NonZeroU32::new(raw) {
            Some(group) => Some(Group(group)),
            None => None,
        }
    }

    #[must_use]
    pub fn of(pid: rustix::process::Pid) -> Group {
        Group(ProcessId::of(pid).0)
    }

    #[must_use]
    pub const fn of_process(pid: ProcessId) -> Group {
        Group(pid.0)
    }

    #[must_use]
    pub const fn leader(self) -> ProcessId {
        ProcessId(self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Debug;
    use std::io::{Read as _, Write as _};
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    use lune_waits::{eventually, eventually_showing};

    use super::*;

    fn pid(raw: u32) -> ProcessId {
        ProcessId::new(raw).expect("test pid is non-zero")
    }

    #[test]
    fn this_process_reads_as_itself() {
        let me = pid(std::process::id());
        let read = process(me).expect("this process reads");
        assert_eq!(read.pid, me);
        let parent = rustix::process::getppid().expect("this process has a parent");
        assert_eq!(read.parent, Some(ProcessId::of(parent)));
        let group = Group::of(rustix::process::getpgrp());
        assert_eq!(read.group, group);
        assert!(!read.stopped);
        let args: Vec<String> = std::env::args().collect();
        assert_eq!(argv(me).map(|it| it.as_slice().to_vec()), Some(args));
    }

    #[test]
    fn a_child_reads_with_its_arguments_and_stops() {
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", "b c"])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let child_pid = pid(child.id());
        let argv = sh_ran(child_pid, |argv| argv);
        assert_eq!(argv.as_slice(), ["sh", "-c", "read line", "", "b c"]);
        assert!(children(pid(std::process::id())).contains(&child_pid));
        let read = process(child_pid).expect("the child reads");
        assert_eq!(read.parent, Some(pid(std::process::id())));
        assert!(["sh", "bash"].contains(&read.name.as_str()), "{read:?}");
        let raw = rustix::process::Pid::from_raw(child_pid.get().cast_signed()).expect("a pid is not 0");
        rustix::process::kill_process(raw, rustix::process::Signal::STOP).expect("it stops");
        eventually("it reads as stopped", || process(child_pid).filter(|read| read.stopped));
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
        let read = argv(pid(child.id()));
        drop(child.stdin.take());
        child.wait().expect("it ends");
        assert_eq!(read.map(|it| it.as_slice().to_vec()), Some(vec![String::new()]));
    }

    #[test]
    fn arguments_are_cut_to_their_bound() {
        let long = "é".repeat(ARGV_BYTES_MAX);
        let args = bounded(vec!["abc".to_owned(), long.clone(), "d".to_owned()]).unwrap();
        let args = args.as_slice();
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], "abc");
        assert!(long.starts_with(&args[1]));
        assert_eq!(args[1].len(), ARGV_BYTES_MAX - 6);
        let short = vec!["a".to_owned(), String::new()];
        assert_eq!(bounded(short.clone()).unwrap().as_slice(), short.as_slice());
        assert_eq!(bounded(Vec::new()), None);
    }

    #[test]
    fn a_child_s_long_argument_reads_cut() {
        let long = "a".repeat(4 * ARGV_BYTES_MAX);
        let mut child = Command::new("sh")
            .args(["-c", "read line", "", &long])
            .stdin(Stdio::piped())
            .spawn()
            .expect("sh starts");
        let argv = sh_ran(pid(child.id()), |argv| argv.map(|args| args.as_slice().len()));
        let argv = argv.as_slice();
        assert_eq!(argv[..4], ["sh", "-c", "read line", ""]);
        assert_eq!(argv[4], long[..ARGV_BYTES_MAX - 18]);
        assert_eq!(argv.len(), 5);
        child.stdin.take().expect("its stdin").write_all(b"\n").expect("it reads a line");
        child.wait().expect("it ends");
    }

    #[test]
    fn a_process_gone_reads_as_none() {
        let mut child = Command::new("true").spawn().expect("true starts");
        let child_pid = pid(child.id());
        child.wait().expect("it ends");
        assert_eq!(process(child_pid), None);
        assert_eq!(argv(child_pid), None);
        assert_eq!(children(child_pid), Vec::<ProcessId>::new());
    }

    fn sh_ran<E: Debug>(pid: ProcessId, shown: impl Fn(Option<Argv>) -> E) -> Argv {
        eventually_showing("sh runs", || match argv(pid) {
            Some(argv) if argv.as_slice().first().is_some_and(|first| first == "sh") => Ok(argv),
            other => Err(shown(other)),
        })
    }
}
