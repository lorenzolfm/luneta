use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind, Read as _};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;
use rustix::fs::{Mode, OFlags};
use rustix::termios::LocalModes;

use crate::api;
use crate::program;
use crate::watch::{self, Troubles};

const FOLDER: &str = "commands";

const FILE_BYTES_MAX: u64 = 64 * 1024;

const LIMIT: Duration = Duration::from_secs(2);

const READING: [&str; 3] = ["wait_woken", "n_tty_read", "ttyin"];

const TAKING_OVER: [&str; 22] = [
    "sh", "bash", "dash", "zsh", "fish", "nu", "ksh", "tcsh", "vi", "vim", "nvim", "nano", "emacs",
    "hx", "less", "more", "man", "top", "htop", "btop", "ssh", "mosh",
];

const RELAYS: [&str; 2] = ["sudo", "script"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shell {
    pub named: String,
    pub pane: u32,
    pub started: SystemTime,
    pub doing: Doing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Doing {
    Ready,
    Going { command: Option<Said>, since: SystemTime, front: Front },
    Ended { command: Option<Said>, at: SystemTime, ending: Option<Ending>, took: Duration },
    Stopped { command: Option<Said>, at: SystemTime },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    Working,
    Asking,
    Interactive,
}

impl Shell {
    #[must_use]
    pub fn says_as(&self, other: &Shell) -> bool {
        let Shell { named, pane, started, doing } = self;
        let same = *named == other.named && *pane == other.pane;
        same && *started == other.started
            && match (doing, &other.doing) {
                (
                    Doing::Going { command, since, front: _ },
                    Doing::Going { command: other_command, since: other_since, front: _ },
                ) => command == other_command && since == other_since,
                (doing, said) => doing == said,
            }
    }

}

impl Doing {
    #[must_use]
    pub fn to_api(&self) -> api::Shell {
        use api::shell::{Doing as Told, Ended, Going, Stopped};
        let doing = match self {
            Doing::Ready => Told::Ready(()),
            Doing::Going { command, since, front } => Told::Going(Going {
                command: command.clone().map(String::from),
                since: Some((*since).into()),
                front: front.to_api().into(),
            }),
            Doing::Ended { command, at, ending, took } => Told::Ended(Ended {
                command: command.clone().map(String::from),
                at: Some((*at).into()),
                ending: ending.map(Ending::to_api),
                took: Some((*took).try_into().expect("the milliseconds of a u64 fit a Duration")),
            }),
            Doing::Stopped { command, at } => Told::Stopped(Stopped {
                command: command.clone().map(String::from),
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

#[derive(Debug, PartialEq, Eq)]
enum Line {
    Going { command: Said, since: SystemTime, tty: PathBuf },
    Whole(Doing),
}

impl Line {
    fn read(line: &str) -> Option<Self> {
        if line == "ready" {
            return Some(Line::Whole(Doing::Ready));
        }
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
                let ending = match status {
                    "-" => None,
                    "fg" => Some(Ending::Resumed),
                    status => Some(Ending::Exited(status.parse().ok()?)),
                };
                let took = Duration::from_millis(took.parse().ok()?);
                let command = Some(command(rest)?);
                Some(Line::Whole(Doing::Ended { command, at: moment, ending, took }))
            },
            "stopped" => {
                Some(Line::Whole(Doing::Stopped { command: Some(command(rest)?), at: moment }))
            },
            _ => None,
        }
    }
}

fn shell_of(file: &str) -> Option<(u32, SystemTime)> {
    let (pane, started) = file.split_once('.')?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(pane) || !digits(started) {
        return None;
    }
    let started = UNIX_EPOCH.checked_add(Duration::from_secs(started.parse().ok()?))?;
    Some((pane.parse().ok()?, started))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Process {
    tty: String,
    stat: String,
    pid: u32,
    parent: u32,
    name: String,
    wchan: String,
}

const COLUMNS: [&str; 6] = ["tty=", "stat=", "pid=", "ppid=", "comm=", "wchan="];

impl Process {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Modes {
    lines: bool,
    echo: bool,
}

fn modes(tty: &Path) -> Option<Modes> {
    let flags = OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let terminal = rustix::fs::open(tty, flags, Mode::empty()).ok()?;
    let modes = rustix::termios::tcgetattr(&terminal).ok()?.local_modes;
    let (lines, echo) = (modes.contains(LocalModes::ICANON), modes.contains(LocalModes::ECHO));
    Some(Modes { lines, echo })
}

impl Front {
    fn of(modes: Option<Modes>, front: &[&Process]) -> Self {
        let shell = front.iter().any(|process| process.leads());
        match Front::by_modes(modes, front) {
            Front::Interactive if shell => Front::Asking,
            front => front,
        }
    }

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

pub struct Shells {
    dir: PathBuf,
    troubles: Troubles,
}

impl Shells {
    #[must_use]
    pub fn new(folder: &Path) -> Self {
        Shells { dir: folder.join(FOLDER), troubles: Troubles::default() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn read(&mut self) -> Vec<Shell> {
        self.read_by(modes)
    }

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
                    Doing::Going { command: Some(command), since, front }
                },
            };
            shells.push(Shell { named, pane, started, doing });
        }
        self.troubles.done();
        shells
    }

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
                    Err(error) if error.kind() == ErrorKind::NotFound => continue,
                    Err(error) => {
                        self.troubles.say(format!("{}: {error}", path.display()));
                        continue;
                    },
                };
                let Some(line) = Line::read(&text) else {
                    let why = "a line this lunetad cannot read, as an older or a newer hook's";
                    self.troubles.say(format!("{}: {why}", path.display()));
                    continue;
                };
                lines.push(((named.clone(), pane, started), line));
            }
        }
        lines
    }

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

fn listed(dir: &Path, troubles: &mut Troubles) -> Vec<PathBuf> {
    watch::listed(dir).unwrap_or_else(|error| {
        troubles.say(format!("{}: {error}", dir.display()));
        Vec::new()
    })
}

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
        assert_eq!(Line::read("ready"), Some(Line::Whole(Doing::Ready)));
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
                command: Some(said("cargo test")),
                at: at(1_760_900_042),
                ending: Some(Ending::Exited(101)),
                took: Duration::from_millis(41_873),
            }))
        );
        let untold = Line::read("ended 1760900050 - 7120 cargo test");
        let ended = |line: Option<Line>| match line {
            Some(Line::Whole(Doing::Ended { ending, .. })) => ending,
            line => panic!("{line:?}"),
        };
        assert_eq!(ended(untold), None);
        let fg = Line::read("ended 1760900050 fg 7120 cargo test");
        assert_eq!(ended(fg), Some(Ending::Resumed));
        assert_eq!(
            Line::read("stopped 1760900045 cargo test"),
            Some(Line::Whole(Doing::Stopped {
                command: Some(said("cargo test")),
                at: at(1_760_900_045),
            }))
        );
        for wrong in [
            "",
            "ready ",
            "ready 12",
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
        let ending = Some(Ending::Exited(0));
        let doing = Doing::Ended { command: Some(said("make")), at: at(102), ending, took };
        let shell = Shell { named: "work".to_owned(), pane: 1, started: at(100), doing };
        assert_eq!(read, [shell]);
    }

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
