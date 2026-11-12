//! A pane's pseudoterminal, and the program that runs on it.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::OnceLock;

use rustix::fs::{Mode, OFlags};
use rustix::process::{Resource, Rlimit};
use rustix::pty::OpenptFlags;
use rustix::termios::{self, InputModes, OptionalActions, Winsize};

use crate::asked::{Opens, Runs, Size};

/// What names the session of a pane, by its id, in the variables of what runs
/// in it: as `lunetad`'s `mux::own` reads it.
pub const SESSION_VAR: &str = "LUNETA_MUX_SESSION";

/// What says when the session of a pane was made, in nanoseconds since the
/// epoch, in the variables of what runs in it.
pub const MADE_VAR: &str = "LUNETA_MUX_MADE";

/// What names a pane, by its id, in the variables of what runs in it.
pub const PANE_VAR: &str = "LUNETA_MUX_PANE";

/// The variables zellij gives what runs in its panes, which a server started
/// in one of them would pass on to its own: `lunetad` takes them over this
/// server's, so a pane of its with them would be taken for zellij's.
pub const ZELLIJ_VARS: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

/// The terminal the server says it is, to what runs in its panes.
const TERM: &str = "xterm-256color";

/// The most files macOS lets a process say it may open (`OPEN_MAX`), where
/// it refuses a limit of no bound.
const OPEN_MAX: u64 = 10_240;

/// The limit of open files the server started with, where it raised it: what
/// the programs of its panes get back.
static FILES_FOUND: OnceLock<Rlimit> = OnceLock::new();

/// Raises the server's limit of open files as far as it may go, as each pane
/// holds four.
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

/// A pseudoterminal: the end the server reads and writes, and the end the
/// program has, which the server holds open too, so that what the program
/// writes as it ends is still there to read.
pub struct Pty {
    pub near: File,
    far: OwnedFd,
}

impl Pty {
    /// A pseudoterminal of `size`, its near end not blocking.
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

    /// Tells the program its terminal is now `size`.
    pub fn resize(&self, size: Size) -> io::Result<()> {
        let size = Winsize {
            ws_row: size.rows.get(),
            ws_col: size.columns.get(),
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        Ok(termios::tcsetwinsize(&self.near, size)?)
    }

    /// Starts what `opens` runs on the far end, in a session of its own whose
    /// terminal it is, with `vars` set among the server's own variables, and
    /// zellij's left out.
    pub fn spawned(&self, opens: &Opens, vars: &[(&str, String)]) -> io::Result<Child> {
        let mut command = match &opens.runs {
            Runs::Shell => {
                Command::new(std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into()))
            },
            Runs::Program { program, args } => {
                let mut command = Command::new(program);
                command.args(args);
                command
            },
        };
        command.current_dir(&opens.cwd);
        command.stdin(self.far.try_clone()?).stdout(self.far.try_clone()?);
        command.stderr(self.far.try_clone()?);
        for name in ZELLIJ_VARS {
            command.env_remove(name);
        }
        command.env("TERM", TERM).env("COLORTERM", "truecolor");
        for (name, value) in vars {
            command.env(name, value);
        }
        leads(&mut command).spawn()
    }
}

/// Has `command` lead a session of its own, whose terminal is its stdin,
/// with the limit of open files the server started with.
#[expect(unsafe_code, reason = "std has no safe way to start a command in a session of its own")]
fn leads(command: &mut Command) -> &mut Command {
    let files = FILES_FOUND.get().copied();
    // SAFETY: the closure runs in the child, between its fork and its exec,
    // where only what a signal handler may do is safe: `setsid`, the ioctl
    // and `setrlimit` are system calls alone, and their errors hold no
    // memory. Stdin is open there, as std set it up before the closure runs.
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
