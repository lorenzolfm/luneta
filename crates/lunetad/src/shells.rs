//! The shells adapter: what the shell in each pane says through luneta's hook,
//! and what the terminal of a command it runs says the command does.
//!
//! The hook (the README's "Hook the shell") keeps a file for each shell,
//! `commands/<session>/<pane>.<started>` in luneta's folder, which is the
//! daemon's own, and moves into it, whole, a line of one of
//!
//! ```text
//! going <since> <tty> <command>
//! ended <at> <status> <took> <command>
//! stopped <at> <command>
//! ```
//!
//! with `<started>`, `<since>` and `<at>` in seconds since the epoch, `<took>`
//! in milliseconds, `<status>` `-` where the shell cannot say it, as fish
//! cannot after `fg`, and `stopped` for a job stopped with `Ctrl z`. The
//! session is named as it was when the shell started, which a rename leaves
//! behind, as it leaves an agent's.
//!
//! What a command going does, its line does not say: it works, or waits for
//! you to type, or has the terminal to itself, as an editor does. Its terminal
//! says. It takes keys a line at a time or a key at a time, with the echo on
//! or off, and `ps` says which processes are in front of it, and where each
//! sleeps in the kernel: one that reads the terminal sleeps in a read. `sudo`
//! and `script` run the command on a terminal of their own, and keep yours a
//! key at a time to pass the keys on: what runs is what that one says.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read as _};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::said::Said;
use rustix::fs::{Mode, OFlags};
use rustix::termios::LocalModes;

use crate::api;
use crate::program;
use crate::watch::{self, Troubles};

/// The folder the hook keeps its files in, in luneta's.
const FOLDER: &str = "commands";

/// The most of a file the adapter reads. The hook writes a line, with a
/// command on it that someone typed: a file past this is none of its.
const FILE_BYTES_MAX: u64 = 64 * 1024;

/// How long `ps` has to list the processes: it reads them at once.
const LIMIT: Duration = Duration::from_secs(2);

/// Where a process that reads its terminal sleeps in the kernel, as `ps`
/// names it: Linux's, and the BSDs'. A blocking read of a socket sleeps in
/// Linux's too, and reads as a prompt.
const READING: [&str; 3] = ["wait_woken", "n_tty_read", "ttyin"];

/// The programs that take a terminal over for as long as you use them, by
/// their binaries' names: shells, editors, pagers, monitors, and `ssh`.
const TAKING_OVER: [&str; 22] = [
    "sh", "bash", "dash", "zsh", "fish", "nu", "ksh", "tcsh", "vi", "vim", "nvim", "nano", "emacs",
    "hx", "less", "more", "man", "top", "htop", "btop", "ssh", "mosh",
];

/// The programs that run a command on a terminal of their own.
const RELAYS: [&str; 2] = ["sudo", "script"];

/// A shell, by the file the hook keeps for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    /// The name of the session it started in, as it was then.
    pub named: String,
    pub pane: u32,
    /// When it started, to the second.
    pub started: SystemTime,
    pub doing: Doing,
}

/// What a shell said last, each time to the second it said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    /// It runs `command`, since `since`, and the command in front of its
    /// terminal does as `front` says.
    Going { command: Said, since: SystemTime, front: Front },
    /// It ran `command`, for as long as `took`, and it ended at `at` with
    /// `status`, if the shell could say it.
    Ended { command: Said, at: SystemTime, status: Option<i32>, took: Duration },
    /// It stopped the job that runs `command`, which waits to be brought back.
    Stopped { command: Said, at: SystemTime },
}

/// What the command in front of a terminal does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    /// It works, and reads nothing from the terminal.
    Working,
    /// It waits for you to type: it reads the terminal a line at a time, or
    /// with the echo off, as a password prompt does.
    Asking,
    /// It takes the terminal over a key at a time, as an editor, a pager or
    /// `ssh` does, for as long as you use it.
    Interactive,
}

impl Shell {
    #[must_use]
    pub fn to_api(&self) -> api::Shell {
        use api::shell::{Doing as Told, Ended, Going, Stopped};
        let doing = match &self.doing {
            Doing::Going { command, since, front } => Told::Going(Going {
                command: command.as_str().to_owned(),
                since: Some((*since).into()),
                front: front.to_api().into(),
            }),
            Doing::Ended { command, at, status, took } => Told::Ended(Ended {
                command: command.as_str().to_owned(),
                at: Some((*at).into()),
                status: *status,
                took: Some((*took).try_into().expect("the milliseconds of a u64 fit a Duration")),
            }),
            Doing::Stopped { command, at } => Told::Stopped(Stopped {
                command: command.as_str().to_owned(),
                at: Some((*at).into()),
            }),
        };
        api::Shell { doing: Some(doing) }
    }
}

impl Front {
    fn to_api(self) -> api::Front {
        match self {
            Front::Working => api::Front::Working,
            Front::Asking => api::Front::Asking,
            Front::Interactive => api::Front::Interactive,
        }
    }
}

/// A line as the hook writes it.
#[derive(Debug, PartialEq, Eq)]
enum Line {
    /// A command going on the terminal at `tty`, which says what it does.
    Going { command: Said, since: SystemTime, tty: PathBuf },
    /// A line that says all there is.
    Whole(Doing),
}

impl Line {
    fn read(line: &str) -> Option<Self> {
        let (word, rest) = line.split_once(' ')?;
        let (secs, rest) = rest.split_once(' ')?;
        let moment = UNIX_EPOCH.checked_add(Duration::from_secs(secs.parse().ok()?))?;
        let command = |command: &str| Said::new(command.to_owned());
        match word {
            "going" => {
                let (tty, rest) = rest.split_once(' ')?;
                let tty = PathBuf::from(tty);
                Some(Line::Going { command: command(rest)?, since: moment, tty })
            },
            "ended" => {
                let (status, rest) = rest.split_once(' ')?;
                let (took, rest) = rest.split_once(' ')?;
                let status = match status {
                    "-" => None,
                    status => Some(status.parse().ok()?),
                };
                let took = Duration::from_millis(took.parse().ok()?);
                let command = command(rest)?;
                Some(Line::Whole(Doing::Ended { command, at: moment, status, took }))
            },
            "stopped" => Some(Line::Whole(Doing::Stopped { command: command(rest)?, at: moment })),
            _ => None,
        }
    }
}

/// The shell a file is for, by its name: its pane and when it started, as
/// `<pane>.<started>`. A file the hook is still writing, `<pane>.<started>.new`,
/// is none.
fn shell_of(file: &str) -> Option<(u32, SystemTime)> {
    let (pane, started) = file.split_once('.')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(pane) || !digits(started) {
        return None;
    }
    let started = UNIX_EPOCH.checked_add(Duration::from_secs(started.parse().ok()?))?;
    Some((pane.parse().ok()?, started))
}

/// A process, as `ps` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    /// The terminal it runs on, as `ps` names it: `pts/3` for `/dev/pts/3`,
    /// and `?` for none.
    tty: String,
    /// Its state, with `+` for one in front of its terminal, and `s` for the
    /// leader of its session: the shell the terminal was opened for.
    stat: String,
    pid: u32,
    /// Its parent's id.
    parent: u32,
    /// Its binary's name.
    name: String,
    /// Where it sleeps in the kernel.
    wchan: String,
}

/// `ps`'s columns, where it sleeps the last: in any other place `ps` cuts it
/// to six characters, and `wait_woken` would read as `wait_w`.
const COLUMNS: [&str; 6] = ["tty=", "stat=", "pid=", "ppid=", "comm=", "wchan="];

impl Process {
    /// A line of what `ps` lists, in the order of `COLUMNS`. Its binary's name
    /// can hold spaces, and comes between the numbers and where it sleeps.
    fn read(line: &str) -> Option<Self> {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [tty, stat, pid, parent, ref comm @ .., wchan] = fields[..] else {
            return None;
        };
        if comm.is_empty() {
            return None;
        }
        let comm = comm.join(" ");
        let name = comm.rsplit('/').next().unwrap_or(&comm);
        Some(Process {
            tty: tty.to_owned(),
            stat: stat.to_owned(),
            pid: pid.parse().ok()?,
            parent: parent.parse().ok()?,
            name: name.strip_prefix('-').unwrap_or(name).to_owned(),
            wchan: wchan.to_owned(),
        })
    }

    fn in_front(&self) -> bool {
        self.stat.contains('+')
    }

    fn leads(&self) -> bool {
        self.stat.contains('s')
    }
}

/// How a terminal takes keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Modes {
    /// A line at a time, as the kernel edits it, or else a key at a time.
    lines: bool,
    echo: bool,
}

/// How the terminal at `tty` takes keys, unless it says none: it is no
/// terminal, or another user's, as the one `sudo` runs a command on is root's.
fn modes(tty: &Path) -> Option<Modes> {
    // Never as the daemon's own terminal, which a daemon that has none, as
    // one started by systemd, would take it for, and be hung up on as it
    // closes. Nor waiting for a line that is not there.
    let flags = OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let terminal = rustix::fs::open(tty, flags, Mode::empty()).ok()?;
    let modes = rustix::termios::tcgetattr(&terminal).ok()?.local_modes;
    let (lines, echo) = (modes.contains(LocalModes::ICANON), modes.contains(LocalModes::ECHO));
    Some(Modes { lines, echo })
}

impl Front {
    /// What the command in front of a terminal does, as its `modes` say, and
    /// the processes in front of it. The shell itself in front, its terminal a
    /// key at a time, is its own `read` asking you something, as fish's line
    /// editor takes the keys.
    fn of(modes: Option<Modes>, front: &[&Process]) -> Self {
        let shell = front.iter().any(|process| process.leads());
        match Front::by_modes(modes, front) {
            Front::Interactive if shell => Front::Asking,
            front => front,
        }
    }

    /// What the command in front does, as the terminal's `modes` say, and
    /// whether a process in `front` reads it. A terminal that says none
    /// takes lines, with the echo on, as one does until a program asks for
    /// another way.
    fn by_modes(modes: Option<Modes>, front: &[&Process]) -> Self {
        let Modes { lines, echo } = modes.unwrap_or(Modes { lines: true, echo: true });
        let reading = front.iter().any(|process| READING.contains(&process.wchan.as_str()));
        if !lines {
            Front::Interactive
        } else if !echo || reading {
            Front::Asking
        } else {
            Front::Working
        }
    }

    /// What the command that `sudo` or `script` runs on a terminal of its own
    /// does, as that terminal's `modes` and `front` say. A terminal of another
    /// user's, as sudo's is root's, says neither its modes nor where its
    /// processes sleep: there, a program in front that takes a terminal over
    /// has it, and anything else is at work, even as it asks.
    fn behind(modes: Option<Modes>, front: &[&Process]) -> Self {
        if modes.is_some() {
            return Front::by_modes(modes, front);
        }
        if front.iter().any(|process| TAKING_OVER.contains(&process.name.as_str())) {
            Front::Interactive
        } else {
            Front::Working
        }
    }

    /// What the command going on the terminal at `tty` does, as `processes`
    /// and the `modes` of the terminals say. Behind `sudo` or `script` in
    /// front, it is what the terminal of their child says.
    fn at(tty: &Path, processes: &[Process], modes: impl Fn(&Path) -> Option<Modes>) -> Self {
        let named = tty.strip_prefix("/dev").unwrap_or(tty).to_string_lossy();
        let in_front = |named: &str| -> Vec<&Process> {
            processes.iter().filter(|process| process.tty == named && process.in_front()).collect()
        };
        let front = in_front(&named);
        let said = Front::of(modes(tty), &front);
        if said != Front::Interactive {
            return said;
        }
        let relays = front.iter().filter(|process| RELAYS.contains(&process.name.as_str()));
        let children = relays
            .flat_map(|relay| processes.iter().filter(move |child| child.parent == relay.pid));
        let mut ttys = children.map(|child| child.tty.as_str());
        match ttys.find(|tty| !tty.starts_with('?') && *tty != named) {
            Some(inner) => Front::behind(modes(&Path::new("/dev").join(inner)), &in_front(inner)),
            None => said,
        }
    }
}

/// The files the hook keeps, read again on every read.
pub struct Shells {
    dir: PathBuf,
    /// What went wrong in reading them, and in asking `ps`.
    troubles: Troubles,
}

impl Shells {
    /// The files the hook keeps in luneta's `folder`.
    #[must_use]
    pub fn new(folder: &Path) -> Self {
        Shells { dir: folder.join(FOLDER), troubles: Troubles::default() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// What each shell says now, and for each command going, what its
    /// terminal says it does. One going on a terminal that is gone is no
    /// pane's: its shell went without a word.
    pub fn read(&mut self) -> Vec<Shell> {
        self.read_by(modes)
    }

    /// What `read` says, with how each terminal takes keys as `modes` says.
    fn read_by(&mut self, modes: impl Fn(&Path) -> Option<Modes>) -> Vec<Shell> {
        let mut lines = self.lines();
        let going = lines.iter().any(|(_, line)| matches!(line, Line::Going { .. }));
        let processes = if going { self.processes() } else { Vec::new() };
        let mut fronts: HashMap<PathBuf, Front> = HashMap::new();
        let mut front = |tty: &Path| {
            if !fs::metadata(tty).is_ok_and(|found| found.file_type().is_char_device()) {
                return None;
            }
            let front = fronts.entry(tty.to_owned());
            Some(*front.or_insert_with_key(|tty| Front::at(tty, &processes, &modes)))
        };
        if going {
            // The terminals, then the files again. A command that ended
            // after its file was read left its shell at its prompt, with
            // the terminal's modes the shell's own, which read as asking:
            // what the file says once the terminal has been read stands.
            for (_, line) in &lines {
                if let Line::Going { tty, .. } = line {
                    front(tty);
                }
            }
            lines = self.lines();
        }
        let mut shells = Vec::with_capacity(lines.len());
        for ((named, pane, started), line) in lines {
            let doing = match line {
                Line::Whole(doing) => doing,
                Line::Going { command, since, tty } => {
                    let Some(front) = front(&tty) else {
                        continue;
                    };
                    Doing::Going { command, since, front }
                },
            };
            shells.push(Shell { named, pane, started, doing });
        }
        self.troubles.done();
        shells
    }

    /// The line in each file, and the shell it is for: the session it
    /// started in, its pane, and when it started.
    fn lines(&mut self) -> Vec<((String, u32, SystemTime), Line)> {
        let mut lines = Vec::new();
        for session in listed(&self.dir, &mut self.troubles) {
            let Some(named) = session.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let named = named.to_owned();
            if !fs::metadata(&session).is_ok_and(|found| found.is_dir()) {
                continue;
            }
            for path in listed(&session, &mut self.troubles) {
                let Some((pane, started)) =
                    path.file_name().and_then(|name| name.to_str()).and_then(shell_of)
                else {
                    continue;
                };
                let text = match first_line(&path) {
                    Ok(text) => text,
                    // A file gone since the folder was listed is a shell that
                    // exited.
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => {
                        self.troubles.say(format!("{}: {error}", path.display()));
                        continue;
                    },
                };
                if let Some(line) = Line::read(&text) {
                    lines.push(((named.clone(), pane, started), line));
                }
            }
        }
        lines
    }

    /// The processes on the host, or none when `ps` does not list them.
    fn processes(&mut self) -> Vec<Process> {
        let mut ps = Command::new("ps");
        ps.arg("-A");
        for column in COLUMNS {
            ps.args(["-o", column]);
        }
        let said = program::run(&mut ps, LIMIT)
            .map_err(|error| format!("ps could not be run: {error}"))
            .and_then(|ran| ran.answer().map_err(|why| format!("ps: {why}")));
        match said {
            Ok(said) => String::from_utf8_lossy(&said).lines().filter_map(Process::read).collect(),
            Err(why) => {
                self.troubles.say(why);
                Vec::new()
            },
        }
    }
}

/// The paths in `dir`, or none when it cannot be listed, which `troubles`
/// says.
fn listed(dir: &Path, troubles: &mut Troubles) -> Vec<PathBuf> {
    watch::listed(dir).unwrap_or_else(|error| {
        troubles.say(format!("{}: {error}", dir.display()));
        Vec::new()
    })
}

/// The first line of the file at `path`, if it holds no more than the hook
/// writes.
fn first_line(path: &Path) -> io::Result<String> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(ErrorKind::InvalidData, "it is no file"));
    }
    let mut text = Vec::new();
    fs::File::open(path)?.take(FILE_BYTES_MAX + 1).read_to_end(&mut text)?;
    let bytes = u64::try_from(text.len()).expect("a length under a u64 limit fits a u64");
    if bytes > FILE_BYTES_MAX {
        let why = format!("it holds more than {FILE_BYTES_MAX} bytes");
        return Err(io::Error::new(ErrorKind::FileTooLarge, why));
    }
    let line = text.split(|byte| *byte == b'\n').next().unwrap_or_default();
    Ok(String::from_utf8_lossy(line).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn said(text: &str) -> Said {
        Said::new(text.to_owned()).unwrap()
    }

    #[test]
    fn a_line_reads_as_the_hook_writes_it() {
        assert_eq!(
            Line::read("going 1760900000 /dev/pts/3 cargo build --release"),
            Some(Line::Going {
                command: said("cargo build --release"),
                since: at(1_760_900_000),
                tty: PathBuf::from("/dev/pts/3"),
            })
        );
        assert_eq!(
            Line::read("ended 1760900042 101 41873 cargo test"),
            Some(Line::Whole(Doing::Ended {
                command: said("cargo test"),
                at: at(1_760_900_042),
                status: Some(101),
                took: Duration::from_millis(41_873),
            }))
        );
        let fg = Line::read("ended 1760900050 - 7120 cargo test");
        assert!(matches!(fg, Some(Line::Whole(Doing::Ended { status: None, .. }))), "{fg:?}");
        assert_eq!(
            Line::read("stopped 1760900045 cargo test"),
            Some(Line::Whole(Doing::Stopped { command: said("cargo test"), at: at(1_760_900_045) }))
        );
        for wrong in [
            "",
            "going",
            "going 12 /dev/pts/3",
            "going 12 /dev/pts/3 ",
            "went 12 /dev/pts/3 make",
            "ended 12 x 5 make",
            "ended 12 0 5",
            "stopped 12",
            "stopped x make",
        ] {
            assert_eq!(Line::read(wrong), None, "{wrong:?}");
        }
    }

    fn process(tty: &str, stat: &str, pid: u32, name: &str, wchan: &str) -> Process {
        let (tty, stat, name, wchan) = (tty.to_owned(), stat.to_owned(), name.to_owned(), wchan);
        Process { tty, stat, pid, parent: 1, name, wchan: wchan.to_owned() }
    }

    /// A terminal that takes keys one at a time is a program's own, or the
    /// shell's, which reads one line in its own editor. One with its echo off,
    /// or a process in front that reads it, waits for you to type. Else the
    /// command is at work, as it is on a terminal that says nothing of how it
    /// takes keys.
    #[test]
    fn a_terminal_says_whether_its_command_works_asks_or_is_interactive() {
        let cooked = Some(Modes { lines: true, echo: true });
        let raw = Some(Modes { lines: false, echo: false });
        let password = Some(Modes { lines: true, echo: false });
        let ps = |stat, wchan| process("pts/3", stat, 30, "x", wchan);
        for (modes, front, said) in [
            (cooked, vec![ps("S+", "hrtimer_nanosleep")], Front::Working),
            (None, vec![ps("S+", "hrtimer_nanosleep")], Front::Working),
            (cooked, vec![ps("S+", "do_wait"), ps("S+", "wait_woken")], Front::Asking),
            (cooked, vec![ps("S+", "ttyin")], Front::Asking),
            (password, vec![ps("Ss+", "0")], Front::Asking),
            (raw, vec![ps("S+", "wait_woken")], Front::Interactive),
            (raw, vec![ps("Sl+", "do_epoll_wait")], Front::Interactive),
            (raw, vec![ps("Ss+", "poll_schedule_timeout.constprop.0")], Front::Asking),
        ] {
            let front: Vec<&Process> = front.iter().collect();
            assert_eq!(Front::of(modes, &front), said, "{modes:?} {front:?}");
        }
    }

    /// Behind `sudo` or `script`, which keep your terminal a key at a time to
    /// pass the keys on, what runs is what the terminal they run it on says:
    /// by its modes and its processes where it is yours, as `script`'s is,
    /// with no shell's `read` in it, and where it is root's, as sudo's is, by
    /// the name of the program in front of it. Only the processes in front
    /// count.
    #[test]
    fn behind_sudo_or_script_what_runs_is_what_their_terminal_says() {
        let raw = Some(Modes { lines: false, echo: false });
        let cooked = Some(Modes { lines: true, echo: true });
        for (relay, inner, front, said) in [
            ("sudo", None, ("S+", "nixos-rebuild", "-"), Front::Working),
            ("sudo", None, ("S+", "bash", "-"), Front::Interactive),
            ("sudo", None, ("S+", "nvim", "-"), Front::Interactive),
            ("sudo", None, ("S", "nvim", "-"), Front::Working),
            ("script", cooked, ("S+", "sleep", "hrtimer_nanosleep"), Front::Working),
            ("script", cooked, ("S+", "cat", "wait_woken"), Front::Asking),
            ("script", raw, ("Ss+", "bash", "do_select"), Front::Interactive),
        ] {
            let (stat, name, wchan) = front;
            let processes = [
                process("pts/3", "Ss", 100, "fish", "do_wait"),
                Process { parent: 100, ..process("pts/3", "S+", 200, relay, "poll_schedule") },
                // Its child on your terminal, as well, is no terminal of its own.
                Process { parent: 200, ..process("pts/3", "S", 201, relay, "do_wait") },
                Process { parent: 200, ..process("pts/10", stat, 700, name, wchan) },
            ];
            let modes = |tty: &Path| match tty.to_str() {
                Some("/dev/pts/3") => raw,
                Some("/dev/pts/10") => inner,
                _ => None,
            };
            let front = Front::at(Path::new("/dev/pts/3"), &processes, modes);
            assert_eq!(front, said, "{relay} {name} {stat}");
        }
    }

    /// A command that ends as its terminal is read leaves its shell at its
    /// prompt, with the terminal's modes the shell's own, which read as
    /// asking. Its file, read once the terminal has been, says it ended.
    #[test]
    fn a_command_that_ends_as_its_terminal_is_read_has_ended() {
        let folder = tempfile::tempdir().unwrap();
        let dir = folder.path().join(FOLDER).join("work");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("1.100");
        fs::write(&file, "going 100 /dev/null make\n").unwrap();
        let ending = |_: &Path| {
            fs::write(&file, "ended 102 0 1500 make\n").unwrap();
            Some(Modes { lines: true, echo: false })
        };
        let read = Shells::new(folder.path()).read_by(ending);
        let took = Duration::from_millis(1500);
        let doing = Doing::Ended { command: said("make"), at: at(102), status: Some(0), took };
        let shell = Shell { named: "work".to_owned(), pane: 1, started: at(100), doing };
        assert_eq!(read, [shell]);
    }

    /// `ps` cuts where a process sleeps to six characters unless it comes
    /// last, and then `wait_woken` would never read as a read of the
    /// terminal. What it says is what the kernel says, in full.
    #[cfg(target_os = "linux")]
    #[test]
    fn ps_says_where_a_process_sleeps_in_full() {
        let mut sleep = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        let pid = sleep.id();
        let wchan = format!("/proc/{pid}/wchan");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let kernel = loop {
            let kernel = fs::read_to_string(&wchan).unwrap();
            if kernel.len() > 6 || std::time::Instant::now() > deadline {
                break kernel;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut shells = Shells::new(Path::new("/nonexistent"));
        let processes = shells.processes();
        sleep.kill().unwrap();
        sleep.wait().unwrap();
        let listed = processes.iter().find(|process| process.pid == pid);
        let listed = listed.unwrap_or_else(|| panic!("ps did not list {pid}"));
        assert!(kernel.len() > 6, "the kernel says {kernel:?}");
        assert_eq!((listed.name.as_str(), listed.wchan.as_str()), ("sleep", kernel.as_str()));
        assert_eq!(listed.parent, std::process::id());
    }
}
