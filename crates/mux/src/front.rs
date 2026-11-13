//! What the command in front of a pane's terminal does: it works, or waits
//! for you to type, or has the terminal to itself, as an editor does. The
//! terminal says how it takes keys, a line at a time or a key at a time, with
//! the echo on or off, and the kernel says where each process in front of it
//! sleeps: one that reads the terminal sleeps in a read. `sudo` and `script`
//! run the command on a terminal of their own, and keep the pane's a key at a
//! time to pass the keys on: what runs is what that one says.
//!
//! As `lunetad`'s `shells` says it of a zellij pane's command, from what `ps`
//! lists; here, from the processes the pane's program started.

use std::path::PathBuf;

use rustix::fs::{Mode, OFlags};

use crate::api;
use crate::process::{self, Process};
use crate::pty::{self, Modes, Pty};

/// Where a process that reads its terminal sleeps in the kernel, as Linux
/// names it. A blocking read of a socket sleeps in `wait_woken` too, and reads
/// as a prompt. macOS names where none sleeps.
const READING: [&str; 2] = ["wait_woken", "n_tty_read"];

/// The programs that take a terminal over for as long as you use them, by
/// their binaries' names: shells, editors, pagers, monitors, and `ssh`.
const TAKING_OVER: [&str; 22] = [
    "sh", "bash", "dash", "zsh", "fish", "nu", "ksh", "tcsh", "vi", "vim", "nvim", "nano", "emacs",
    "hx", "less", "more", "man", "top", "htop", "btop", "ssh", "mosh",
];

/// The programs that run a command on a terminal of their own.
const RELAYS: [&str; 2] = ["sudo", "script"];

/// The most processes a look reads.
const PROCESSES_MAX: usize = 512;

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

impl Front {
    #[must_use]
    pub fn said(self) -> api::Front {
        match self {
            Front::Working => api::Front::Working,
            Front::Asking => api::Front::Asking,
            Front::Interactive => api::Front::Interactive,
        }
    }

    /// What the command in front of `pty` does, the group `group` in front of
    /// it, of the processes `program`, the pane's, started.
    #[must_use]
    pub fn of_pane(pty: &Pty, program: u32, group: u32) -> Front {
        let modes = pty.modes().ok();
        let processes = tree(program, group);
        let front = in_front(&processes, group);
        let said = Front::of(modes, &front, group == program);
        if said != Front::Interactive {
            return said;
        }
        let Ok(ours) = pty.device() else {
            return said;
        };
        let relays = front.iter().filter(|process| RELAYS.contains(&process.name.as_str()));
        let children = relays.flat_map(|relay| {
            processes.iter().filter(move |child| child.parent == relay.pid)
        });
        let inner = children.filter_map(|child| Some((child, child.terminal?)));
        match inner.into_iter().find(|(_, terminal)| terminal.device != ours) {
            Some((child, terminal)) => {
                let Some(inner) = terminal.front else {
                    return Front::behind(modes_at(terminal.device), &[]);
                };
                let below = tree(child.pid, inner);
                Front::behind(modes_at(terminal.device), &in_front(&below, inner))
            },
            None => said,
        }
    }

    /// What the command in front of a terminal does, as its `modes` say, and
    /// the processes in `front` of it. The shell itself in front, as `shell`
    /// says, its terminal a key at a time, is its own `read` asking you
    /// something, as fish's line editor takes the keys.
    fn of(modes: Option<Modes>, front: &[&Process], shell: bool) -> Self {
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
        let reading = || front.iter().any(|process| READING.contains(&process.sleeps.as_str()));
        if !lines {
            Front::Interactive
        } else if !echo || reading() {
            Front::Asking
        } else {
            Front::Working
        }
    }

    /// What the command that `sudo` or `script` runs on a terminal of its own
    /// does, as that terminal's `modes` and `front` say. A terminal of another
    /// user's, as sudo's is root's, says no modes: there, a program in front
    /// that takes a terminal over has it, and anything else is at work, even
    /// as it asks.
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
}

/// `root` and the processes it started, to the depth of `group`'s: past its
/// children, only those of `group` are followed, as a job the shell put in
/// the background, and what that started, are none of the one in front. Up
/// to `PROCESSES_MAX`.
fn tree(root: u32, group: u32) -> Vec<Process> {
    let Some(first) = process::process(root) else {
        return Vec::new();
    };
    let mut processes = vec![first];
    let mut next = 0;
    while next < processes.len() {
        let parent = &processes[next];
        let follow = next == 0 || parent.group == group;
        let pid = parent.pid;
        next += 1;
        if !follow {
            continue;
        }
        for child in process::children(pid) {
            if processes.len() == PROCESSES_MAX {
                return processes;
            }
            if let Some(child) = process::process(child).filter(|child| child.parent == pid) {
                processes.push(child);
            }
        }
    }
    processes
}

/// The processes of `group` among `processes`.
fn in_front(processes: &[Process], group: u32) -> Vec<&Process> {
    processes.iter().filter(|process| process.group == group).collect()
}

/// How the terminal of device `device` takes keys, where it is a terminal of
/// this user's that can be opened by its number: never as the server's own
/// terminal, nor waiting for one.
fn modes_at(device: u64) -> Option<Modes> {
    let path = path_of(device);
    let flags = OFlags::RDONLY | OFlags::NOCTTY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let terminal = rustix::fs::open(&path, flags, Mode::empty()).ok()?;
    if pty::device_of(&terminal).ok()? != device {
        return None;
    }
    Modes::of(&terminal).ok()
}

/// Where a pseudoterminal of device `device` would be: Linux numbers its
/// pseudoterminals by their minor numbers.
#[cfg(target_os = "linux")]
fn path_of(device: u64) -> PathBuf {
    PathBuf::from(format!("/dev/pts/{}", rustix::fs::minor(device)))
}

/// Where a pseudoterminal of device `device` would be: macOS numbers its
/// pseudoterminals by their minor numbers, in three digits at least. Its
/// device numbers are 32 bits, as `device_of` gives them.
#[cfg(target_os = "macos")]
fn path_of(device: u64) -> PathBuf {
    let device = rustix::fs::Dev::from_ne_bytes((device as u32).to_ne_bytes());
    PathBuf::from(format!("/dev/ttys{:03}", rustix::fs::minor(device)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str, sleeps: &str) -> Process {
        Process {
            pid,
            parent: 1,
            group: pid,
            name: name.to_owned(),
            sleeps: sleeps.to_owned(),
            stopped: false,
            terminal: None,
        }
    }

    const LINES: Option<Modes> = Some(Modes { lines: true, echo: true });
    const KEYS: Option<Modes> = Some(Modes { lines: false, echo: false });
    const HIDDEN: Option<Modes> = Some(Modes { lines: true, echo: false });

    /// A terminal a key at a time is taken over, one with the echo off asks,
    /// and one a line at a time with the echo on asks where a process in
    /// front reads it, and else works.
    #[test]
    fn the_modes_and_a_read_say_what_the_command_does() {
        let sleeping = process(2, "sleep", "hrtimer_nanosleep");
        let reading = process(3, "python3", "n_tty_read");
        let socket = process(4, "curl", "wait_woken");
        assert_eq!(Front::of(KEYS, &[&sleeping], false), Front::Interactive);
        assert_eq!(Front::of(HIDDEN, &[&sleeping], false), Front::Asking);
        assert_eq!(Front::of(LINES, &[&sleeping], false), Front::Working);
        assert_eq!(Front::of(LINES, &[&sleeping, &reading], false), Front::Asking);
        assert_eq!(Front::of(LINES, &[&socket], false), Front::Asking);
        assert_eq!(Front::of(None, &[&sleeping], false), Front::Working);
    }

    /// The shell in front a key at a time is its own `read` asking.
    #[test]
    fn the_shell_in_front_a_key_at_a_time_asks() {
        let fish = process(2, "fish", "");
        assert_eq!(Front::of(KEYS, &[&fish], true), Front::Asking);
        assert_eq!(Front::of(LINES, &[&fish], true), Front::Working);
    }

    /// Behind a relay, a terminal that says its modes says as any does, and
    /// one that says none has a program that takes a terminal over in front,
    /// or else works.
    #[test]
    fn behind_a_relay_a_root_terminal_goes_by_the_program_s_name() {
        let vim = process(2, "vim", "");
        let make = process(3, "make", "");
        assert_eq!(Front::behind(None, &[&vim]), Front::Interactive);
        assert_eq!(Front::behind(None, &[&make]), Front::Working);
        assert_eq!(Front::behind(HIDDEN, &[&make]), Front::Asking);
        assert_eq!(Front::behind(KEYS, &[&make]), Front::Interactive);
    }
}
