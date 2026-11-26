use std::path::PathBuf;

use rustix::fs::{Mode, OFlags};

use lune_protocol::shell::Front;

use crate::process::{self, Group, Process};
use crate::pty::{self, Modes};

const READING: [&str; 2] = ["wait_woken", "n_tty_read"];

const TAKING_OVER: [&str; 22] = [
    "sh", "bash", "dash", "zsh", "fish", "nu", "ksh", "tcsh", "vi", "vim", "nvim", "nano", "emacs",
    "hx", "less", "more", "man", "top", "htop", "btop", "ssh", "mosh",
];

const RELAYS: [&str; 2] = ["sudo", "script"];

const PROCESSES_MAX: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub modes: Option<Modes>,
    pub device: u64,
}

#[must_use]
pub fn of_terminal(snapshot: Snapshot, program: u32, group: Group) -> Front {
    assert!(snapshot.device != 0);
    assert!(program > 0);
    assert!(group.leader() > 0);
    let processes = tree(program);
    let front = in_front(&processes, group);
    let said = of(snapshot.modes, &front, group.leader() == program);
    if said != Front::Interactive {
        return said;
    }
    let relays = front.iter().filter(|process| RELAYS.contains(&process.name.as_str()));
    let children = relays.flat_map(|relay| {
        processes.iter().filter(move |child| child.parent == relay.pid)
    });
    let inner = children.filter_map(|child| Some((child, child.terminal?)));
    match inner.into_iter().find(|(_, terminal)| terminal.device != snapshot.device) {
        Some((child, terminal)) => {
            let Some(inner) = terminal.front else {
                return behind(modes_at(terminal.device), &[]);
            };
            let below = tree(child.pid);
            behind(modes_at(terminal.device), &in_front(&below, inner))
        },
        None => said,
    }
}

fn of(modes: Option<Modes>, front: &[&Process], shell: bool) -> Front {
    match by_modes(modes, front) {
        Front::Interactive if shell => Front::Asking,
        front => front,
    }
}

fn by_modes(modes: Option<Modes>, front: &[&Process]) -> Front {
    let Modes { lines, echo } = modes.unwrap_or(Modes { lines: true, echo: true });
    let reading = || front.iter().any(|process| READING.contains(&process.sleeps.as_str()));
    if !lines {
        Front::Interactive
    } else if !echo || reading() {
        Front::Asking
    } else {
        Front::Working
    }
}

fn behind(modes: Option<Modes>, front: &[&Process]) -> Front {
    if modes.is_some() {
        return by_modes(modes, front);
    }
    if front.iter().any(|process| TAKING_OVER.contains(&process.name.as_str())) {
        Front::Interactive
    } else {
        Front::Working
    }
}

fn tree(root: u32) -> Vec<Process> {
    let Some(first) = process::process(root) else {
        return Vec::new();
    };
    let mut processes = vec![first];
    let mut next = 0;
    while next < processes.len() {
        let pid = processes[next].pid;
        next += 1;
        for child in process::children(pid) {
            if processes.len() == PROCESSES_MAX {
                return processes;
            }
            if let Some(child) = process::process(child).filter(|child| child.parent == pid) {
                processes.push(child);
            }
        }
    }
    assert!(processes.len() <= PROCESSES_MAX);
    processes
}

fn in_front(processes: &[Process], group: Group) -> Vec<&Process> {
    processes.iter().filter(|process| process.group == group).collect()
}

fn modes_at(device: u64) -> Option<Modes> {
    let path = path_of(device);
    let flags = OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let terminal = rustix::fs::open(&path, flags, Mode::empty()).ok()?;
    if pty::device_of(&terminal).ok()? != device {
        return None;
    }
    Modes::of(&terminal).ok()
}

#[cfg(target_os = "linux")]
fn path_of(device: u64) -> PathBuf {
    PathBuf::from(format!("/dev/pts/{}", rustix::fs::minor(device)))
}

#[cfg(target_os = "macos")]
fn path_of(device: u64) -> PathBuf {
    let device = u32::try_from(device).expect("macOS device numbers are 32 bits");
    let device = rustix::fs::Dev::from_ne_bytes(device.to_ne_bytes());
    PathBuf::from(format!("/dev/ttys{:03}", rustix::fs::minor(device)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str, sleeps: &str) -> Process {
        Process {
            pid,
            parent: 1,
            group: Group(pid),
            name: name.to_owned(),
            sleeps: sleeps.to_owned(),
            stopped: false,
            terminal: None,
        }
    }

    const LINES: Option<Modes> = Some(Modes { lines: true, echo: true });
    const KEYS: Option<Modes> = Some(Modes { lines: false, echo: false });
    const HIDDEN: Option<Modes> = Some(Modes { lines: true, echo: false });

    #[test]
    fn the_modes_and_a_read_say_what_the_command_does() {
        let sleeping = process(2, "sleep", "hrtimer_nanosleep");
        let reading = process(3, "python3", "n_tty_read");
        let socket = process(4, "curl", "wait_woken");
        assert_eq!(of(KEYS, &[&sleeping], false), Front::Interactive);
        assert_eq!(of(HIDDEN, &[&sleeping], false), Front::Asking);
        assert_eq!(of(LINES, &[&sleeping], false), Front::Working);
        assert_eq!(of(LINES, &[&sleeping, &reading], false), Front::Asking);
        assert_eq!(of(LINES, &[&socket], false), Front::Asking);
        assert_eq!(of(None, &[&sleeping], false), Front::Working);
    }

    #[test]
    fn the_shell_in_front_a_key_at_a_time_asks() {
        let fish = process(2, "fish", "");
        assert_eq!(of(KEYS, &[&fish], true), Front::Asking);
        assert_eq!(of(LINES, &[&fish], true), Front::Working);
    }

    #[test]
    fn behind_a_relay_a_root_terminal_goes_by_the_program_s_name() {
        let vim = process(2, "vim", "");
        let make = process(3, "make", "");
        assert_eq!(behind(None, &[&vim]), Front::Interactive);
        assert_eq!(behind(None, &[&make]), Front::Working);
        assert_eq!(behind(HIDDEN, &[&make]), Front::Asking);
        assert_eq!(behind(KEYS, &[&make]), Front::Interactive);
    }
}
