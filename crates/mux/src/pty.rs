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
use rustix::process::{Resource, Rlimit, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::{
    self, InputModes, LocalModes, OptionalActions, QueueSelector, SpecialCodeIndex, Winsize,
};

use crate::asked::{Opens, Size};
use crate::process::Group;

pub const TERMINAL_VAR: &str = "LUNE_MUX_TERMINAL";

pub const TERMINAL_MADE_VAR: &str = "LUNE_MUX_TERMINAL_MADE";

const OWN_VARS: [&str; 2] = [TERMINAL_VAR, TERMINAL_MADE_VAR];

pub const ZELLIJ_VARS: [&str; 3] = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];

const TERM: &str = "xterm-256color";

const OPEN_MAX: u64 = 10_240;

#[cfg(target_os = "linux")]
const KEY_TURNED_OFF: u8 = 0;

#[cfg(target_os = "macos")]
const KEY_TURNED_OFF: u8 = 0xff;

#[cfg(target_os = "linux")]
const LINE_FULL_BYTES: Option<u64> = Some(4093);

#[cfg(target_os = "macos")]
const LINE_FULL_BYTES: Option<u64> = None;

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

    pub fn signal_key(&self, byte: u8) -> io::Result<Option<SignalKey>> {
        if byte == KEY_TURNED_OFF {
            return Ok(None);
        }
        let modes = termios::tcgetattr(&self.far)?;
        if !modes.local_modes.contains(LocalModes::ISIG) {
            return Ok(None);
        }
        let flushes = !modes.local_modes.contains(LocalModes::NOFLSH);
        let signal = KeySignal::ALL.into_iter().find(|key| modes.special_codes[key.code()] == byte);
        Ok(signal.map(|signal| SignalKey { signal, flushes }))
    }

    pub fn line_full(&self) -> io::Result<bool> {
        let Some(full) = LINE_FULL_BYTES else {
            return Ok(false);
        };
        Ok(rustix::io::ioctl_fionread(&self.far)? >= full)
    }

    pub fn raise(&self, key: SignalKey) -> Raise {
        let unraised = |error: io::Error| Raise { queue: Queue::Kept, signalled: Err(error) };
        let group = match termios::tcgetpgrp(&self.near) {
            Ok(group) => group,
            Err(error) => return unraised(error.into()),
        };
        let queue = if key.flushes {
            if let Err(error) = self.flush() {
                return unraised(error);
            }
            Queue::Flushed
        } else {
            Queue::Kept
        };
        let signalled = rustix::process::kill_process_group(group, key.signal.signal());
        Raise { queue, signalled: signalled.map_err(io::Error::from) }
    }

    fn flush(&self) -> io::Result<()> {
        Ok(termios::tcflush(&self.far, QueueSelector::IOFlush)?)
    }

    pub fn modes(&self) -> io::Result<Modes> {
        Modes::of(&self.far)
    }

    pub fn device(&self) -> io::Result<u64> {
        Ok(device_of(&self.far)?)
    }

    pub fn spawned(&self, opens: &Opens, vars: &[(&str, String)]) -> io::Result<Led> {
        let mut command = Command::new(&opens.program);
        command.args(&opens.args);
        command.current_dir(&opens.cwd);
        command.stdin(self.far.try_clone()?).stdout(self.far.try_clone()?);
        command.stderr(self.far.try_clone()?);
        for name in ZELLIJ_VARS.iter().chain(&OWN_VARS) {
            command.env_remove(name);
        }
        command.env_remove("TERM_PROGRAM");
        command.env("TERM", TERM).env("COLORTERM", "truecolor");
        for (name, value) in vars {
            command.env(name, value);
        }
        leads(&mut command).spawn().map(Led)
    }
}

pub struct Led(Child);

impl Led {
    pub fn child(self) -> Child {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SignalKey {
    pub signal: KeySignal,
    flushes: bool,
}

#[must_use]
#[derive(Debug)]
pub struct Raise {
    pub queue: Queue,
    pub signalled: io::Result<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Queue {
    Flushed,
    Kept,
}

impl Queue {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Queue::Flushed => "flushed",
            Queue::Kept => "kept",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySignal {
    Interrupt,
    Quit,
    Suspend,
}

impl KeySignal {
    const ALL: [KeySignal; 3] = [KeySignal::Interrupt, KeySignal::Quit, KeySignal::Suspend];

    fn code(self) -> SpecialCodeIndex {
        match self {
            KeySignal::Interrupt => SpecialCodeIndex::VINTR,
            KeySignal::Quit => SpecialCodeIndex::VQUIT,
            KeySignal::Suspend => SpecialCodeIndex::VSUSP,
        }
    }

    fn signal(self) -> Signal {
        match self {
            KeySignal::Interrupt => Signal::INT,
            KeySignal::Quit => Signal::QUIT,
            KeySignal::Suspend => Signal::TSTP,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            KeySignal::Interrupt => "SIGINT",
            KeySignal::Quit => "SIGQUIT",
            KeySignal::Suspend => "SIGTSTP",
        }
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

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::num::NonZeroU16;

    use super::*;
    use crate::asked::Ends;

    fn opened() -> Pty {
        let columns = NonZeroU16::new(80).unwrap();
        let rows = NonZeroU16::new(24).unwrap();
        let pty = Pty::opened(Size { columns, rows }).unwrap();
        changed(&pty, |modes| {
            modes.special_codes[SpecialCodeIndex::VINTR] = 0x03;
            modes.special_codes[SpecialCodeIndex::VQUIT] = 0x1c;
            modes.special_codes[SpecialCodeIndex::VSUSP] = 0x1a;
            modes.local_modes.set(LocalModes::ISIG, true);
            modes.local_modes.set(LocalModes::NOFLSH, false);
        });
        pty
    }

    fn changed(pty: &Pty, change: impl FnOnce(&mut termios::Termios)) {
        let mut modes = termios::tcgetattr(&pty.far).unwrap();
        change(&mut modes);
        termios::tcsetattr(&pty.far, OptionalActions::Now, &modes).unwrap();
    }

    fn key(signal: KeySignal, flushes: bool) -> SignalKey {
        SignalKey { signal, flushes }
    }

    #[cfg(target_os = "linux")]
    fn typed_far(pty: &Pty, bytes: usize) {
        let mut near = &pty.near;
        let mut left = bytes;
        while left > 0 {
            left -= near.write(&vec![b'a'; left]).unwrap();
        }
    }

    #[test]
    fn a_signal_key_is_one_byte_the_terminal_signals_on() {
        let pty = opened();
        assert_eq!(pty.signal_key(0x03).unwrap(), Some(key(KeySignal::Interrupt, true)));
        assert_eq!(pty.signal_key(0x1c).unwrap(), Some(key(KeySignal::Quit, true)));
        assert_eq!(pty.signal_key(0x1a).unwrap(), Some(key(KeySignal::Suspend, true)));
        assert_eq!(pty.signal_key(b'c').unwrap(), None);
    }

    #[test]
    fn a_signal_key_under_noflsh_keeps_what_waits() {
        let pty = opened();
        changed(&pty, |modes| modes.local_modes.set(LocalModes::NOFLSH, true));
        assert_eq!(pty.signal_key(0x03).unwrap(), Some(key(KeySignal::Interrupt, false)));
    }

    #[test]
    fn no_key_signals_with_isig_off() {
        let pty = opened();
        changed(&pty, |modes| modes.local_modes.set(LocalModes::ISIG, false));
        assert_eq!(pty.signal_key(0x03).unwrap(), None);
        assert_eq!(pty.signal_key(0x1c).unwrap(), None);
    }

    #[test]
    fn a_key_turned_off_is_a_byte_like_any_other() {
        let pty = opened();
        changed(&pty, |modes| modes.special_codes[SpecialCodeIndex::VINTR] = KEY_TURNED_OFF);
        assert_eq!(pty.signal_key(KEY_TURNED_OFF).unwrap(), None);
        assert_eq!(pty.signal_key(0x03).unwrap(), None);
        assert_eq!(pty.signal_key(0x1c).unwrap(), Some(key(KeySignal::Quit, true)));
    }

    #[test]
    fn a_signal_raised_on_a_terminal_with_no_front_group_fails_and_keeps_what_waits() {
        let pty = opened();
        (&pty.near).write_all(b"waits\n").unwrap();
        lune_waits::eventually("the kernel takes what waits", || {
            (rustix::io::ioctl_fionread(&pty.far).unwrap() == 6).then_some(())
        });
        for flushes in [true, false] {
            let raise = pty.raise(key(KeySignal::Interrupt, flushes));
            assert_eq!(raise.queue, Queue::Kept);
            assert!(raise.signalled.is_err());
        }
        assert_eq!(rustix::io::ioctl_fionread(&pty.far).unwrap(), 6);
    }

    #[test]
    fn a_flushing_raise_leaves_the_programs_output_as_the_kernels_own_key_does() {
        let by_the_kernel = output_left_after(|pty| (&pty.near).write_all(&[0x03]).unwrap());
        let by_a_raise = output_left_after(|pty| {
            let key = pty.signal_key(0x03).unwrap().unwrap();
            let raise = pty.raise(key);
            assert_eq!(raise.queue, Queue::Flushed);
            raise.signalled.unwrap();
        });
        assert_eq!(by_a_raise, by_the_kernel);
    }

    fn output_left_after(interrupted: impl FnOnce(&Pty)) -> usize {
        let folder = tempfile::tempdir().unwrap();
        let written = folder.path().join("written");
        let pty = opened();
        changed(&pty, |modes| modes.local_modes.set(LocalModes::ECHO, false));
        let script =
            "trap 'printf T; sleep 1000' INT; printf %0500d 0; : > \"$0\"; sleep 1000 & wait";
        let opens = Opens {
            cwd: crate::asked::AbsoluteCwd::of_path("/".to_owned()).unwrap(),
            program: "/bin/sh".to_owned(),
            args: vec!["-c".to_owned(), script.to_owned(), written.to_str().unwrap().to_owned()],
            ends: Ends::Closes,
        };
        let mut child = pty.spawned(&opens, &[]).unwrap().child();
        let output_written = || written.exists().then_some(());
        lune_waits::eventually("the program's output is written", output_written);
        #[cfg(target_os = "linux")]
        lune_waits::eventually("the program's output reaches the near side", || {
            (rustix::io::ioctl_fionread(&pty.near).unwrap() == 500).then_some(())
        });
        interrupted(&pty);
        let mut left = Vec::new();
        lune_waits::eventually("the program's trap runs", || {
            let mut read = [0; 1024];
            if let Ok(count) = (&pty.near).read(&mut read) {
                left.extend_from_slice(&read[..count]);
            }
            left.ends_with(b"T").then_some(())
        });
        let group = rustix::process::Pid::from_child(&child);
        rustix::process::kill_process_group(group, Signal::KILL).unwrap();
        child.wait().unwrap();
        left.len() - 1
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_line_of_input_the_kernel_no_longer_takes_is_full() {
        let pty = opened();
        changed(&pty, |modes| {
            modes.local_modes.set(LocalModes::ICANON, false);
            modes.local_modes.set(LocalModes::ECHO, false);
        });
        typed_far(&pty, 4000);
        lune_waits::eventually("the kernel takes 4000 bytes", || {
            (rustix::io::ioctl_fionread(&pty.far).unwrap() == 4000).then_some(())
        });
        assert!(!pty.line_full().unwrap());
        typed_far(&pty, 1000);
        lune_waits::eventually("the kernel's line fills", || pty.line_full().unwrap().then_some(()));
        pty.flush().unwrap();
        assert!(!pty.line_full().unwrap());
    }
}
