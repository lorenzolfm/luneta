use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::OnceLock;

use rustix::fs::{Mode, OFlags};
use rustix::process::{Resource, Rlimit};
use rustix::pty::OpenptFlags;
use rustix::termios::{self, InputModes, LocalModes, OptionalActions, Winsize};

use crate::asked::{Opens, Size};
use crate::process::Group;

pub const TERMINAL_VAR: &str = "LUNE_MUX_TERMINAL";

pub const TERMINAL_MADE_VAR: &str = "LUNE_MUX_TERMINAL_MADE";

const OWN_VARS: [&str; 2] = [TERMINAL_VAR, TERMINAL_MADE_VAR];

pub const ZELLIJ_VARS: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

const TERM: &str = "xterm-256color";

const OPEN_MAX: u64 = 10_240;

static FILES_FOUND: OnceLock<Rlimit> = OnceLock::new();

pub fn raise_files_limit() -> io::Result<()> {
    let found = rustix::process::getrlimit(Resource::Nofile);
    let most = if cfg!(target_os = "macos") {
        Some(found.maximum.map_or(OPEN_MAX, |maximum| maximum.min(OPEN_MAX)))
    } else {
        found.maximum
    };
    let higher = match (found.current, most) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(current), Some(most)) => most > current,
    };
    if !higher {
        return Ok(());
    }
    rustix::process::setrlimit(Resource::Nofile, Rlimit { current: most, maximum: found.maximum })?;
    assert!(FILES_FOUND.set(found).is_ok(), "the limit of open files was raised twice");
    Ok(())
}

pub struct Pty {
    pub near: File,
    far: OwnedFd,
}

impl Pty {
    pub fn opened(size: Size) -> io::Result<Pty> {
        let near = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
        rustix::pty::grantpt(&near)?;
        rustix::pty::unlockpt(&near)?;
        let path = rustix::pty::ptsname(&near, Vec::new())?;
        let path = Path::new(OsStr::from_bytes(path.as_bytes()));
        let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
        let far = rustix::fs::open(path, flags, Mode::empty())?;
        rustix::io::fcntl_setfd(&near, rustix::io::FdFlags::CLOEXEC)?;
        let mut modes = termios::tcgetattr(&far)?;
        modes.input_modes.set(InputModes::IUTF8, true);
        termios::tcsetattr(&far, OptionalActions::Now, &modes)?;
        let pty = Pty { near: File::from(near), far };
        pty.resize(size)?;
        let flags = rustix::fs::fcntl_getfl(&pty.near)?;
        rustix::fs::fcntl_setfl(&pty.near, flags | OFlags::NONBLOCK)?;
        Ok(pty)
    }

    pub fn resize(&self, size: Size) -> io::Result<()> {
        let size = Winsize {
            ws_row: size.rows.get(),
            ws_col: size.columns.get(),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        Ok(termios::tcsetwinsize(&self.near, size)?)
    }

    pub fn front_group(&self) -> Option<Group> {
        let group = termios::tcgetpgrp(&self.near).ok()?;
        u32::try_from(group.as_raw_nonzero().get()).ok().map(Group)
    }

    pub fn modes(&self) -> io::Result<Modes> {
        Modes::of(&self.far)
    }

    pub fn device(&self) -> io::Result<u64> {
        Ok(device_of(&self.far)?)
    }

    pub fn spawned(&self, opens: &Opens, vars: &[(&str, String)]) -> io::Result<Child> {
        let mut command = Command::new(&opens.program);
        command.args(&opens.args);
        command.current_dir(&opens.cwd);
        command.stdin(self.far.try_clone()?).stdout(self.far.try_clone()?);
        command.stderr(self.far.try_clone()?);
        for name in ZELLIJ_VARS.iter().chain(&OWN_VARS) {
            command.env_remove(name);
        }
        command.env("TERM", TERM).env("COLORTERM", "truecolor");
        for (name, value) in vars {
            command.env(name, value);
        }
        leads(&mut command).spawn()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modes {
    pub lines: bool,
    pub echo: bool,
}

impl Modes {
    pub fn of(terminal: impl AsFd) -> io::Result<Modes> {
        let modes = termios::tcgetattr(terminal)?.local_modes;
        let (lines, echo) = (modes.contains(LocalModes::ICANON), modes.contains(LocalModes::ECHO));
        Ok(Modes { lines, echo })
    }
}

#[cfg(target_os = "linux")]
pub fn device_of(terminal: impl AsFd) -> rustix::io::Result<u64> {
    Ok(rustix::fs::fstat(terminal)?.st_rdev)
}

#[cfg(target_os = "macos")]
pub fn device_of(terminal: impl AsFd) -> rustix::io::Result<u64> {
    Ok(u64::from(rustix::fs::fstat(terminal)?.st_rdev.cast_unsigned()))
}

#[expect(
    unsafe_code,
    reason = "std has no safe way to start a command in a session of its own. The closure runs \
              in the child, between its fork and its exec, where only what a signal handler may \
              do is safe: `setsid`, the ioctl and `setrlimit` are system calls alone, and their \
              errors hold no memory. Stdin is open there, as std set it up before the closure"
)]
fn leads(command: &mut Command) -> &mut Command {
    let files = FILES_FOUND.get().copied();
    unsafe {
        command.pre_exec(move || {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            if let Some(files) = files {
                rustix::process::setrlimit(Resource::Nofile, files)?;
            }
            Ok(())
        });
    }
    command
}
