//! Where luneta keeps the files of what runs on a host: the folder `luneta`
//! in `$XDG_RUNTIME_DIR`, or in `$TMPDIR` where that is not set, as on macOS,
//! which has no `XDG_RUNTIME_DIR`. The shell hook keeps its commands there by
//! the same rule, and the daemon and its CLI find each other there.

use std::fmt;
use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The socket the daemon listens on, in the folder.
pub const SOCKET: &str = "lunetad.sock";

/// The file the daemon holds a lock on while it runs, in the folder.
pub const LOCK: &str = "lunetad.lock";

/// What a daemon that luneta started says, in the folder: luneta gives it
/// this file for its stderr.
pub const LOG: &str = "lunetad.log";

/// The folder, by this process's variables. A variable set to nothing is not
/// set, as the shell hook takes it.
#[must_use]
pub fn find() -> Option<PathBuf> {
    ["XDG_RUNTIME_DIR", "TMPDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|root| !root.is_empty())
        .map(|root| PathBuf::from(root).join("luneta"))
}

/// How lunetad ends when another daemon holds its folder: apart from the
/// other ways it fails, so that a luneta that started it waits for the one
/// that runs, which another luneta may have started in the same moment.
pub const ANOTHER_RUNS: u8 = 3;

/// Why a folder is not one to run a daemon in, or to reach one through.
#[derive(Debug)]
pub enum Refusal {
    /// It is a link, or not a folder, or another user's. In a `TMPDIR` of
    /// `/tmp`, anyone can make one before you do.
    NotYours(PathBuf),
    /// Other users may reach into it, and so reach the socket.
    Open(PathBuf),
    /// Another daemon holds it.
    Running(PathBuf),
    Io(PathBuf, io::Error),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotYours(folder) => {
                write!(f, "{} is not a folder of yours alone", folder.display())
            },
            Refusal::Open(folder) => write!(f, "{} is open to other users", folder.display()),
            Refusal::Running(folder) => {
                write!(f, "another lunetad already runs in {}", folder.display())
            },
            Refusal::Io(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for Refusal {}

/// A folder that is this user's alone, as `open` or `make` found it: the only
/// place a daemon is reached in, or looked for. A socket in a folder that is
/// not could be anyone's.
pub struct Folder(PathBuf);

impl Folder {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The socket the daemon listens on, in the folder.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.0.join(SOCKET)
    }
}

/// The folder at `path`, which must be this user's alone if it is there.
/// None is there before a daemon has made it.
pub fn open(path: &Path) -> Result<Option<Folder>, Refusal> {
    match check(path) {
        Ok(()) => Ok(Some(Folder(path.to_owned()))),
        Err(Refusal::Io(_, error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Makes the folder at `path` for this user alone, unless it is there, and
/// checks that it is that, as the shell hook does before it writes in it. One
/// just made is checked too, as whatever made it has no say in what it turned
/// out to be.
pub fn make(path: &Path) -> Result<Folder, Refusal> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(path.to_owned(), error)),
    }
    check(path)?;
    Ok(Folder(path.to_owned()))
}

/// Whether a daemon holds `folder`: one does from the moment it starts until
/// it has ended, however it ends. To tell, the lock is taken and let go at
/// once, and a daemon that starts in that moment takes the folder for
/// another's, and ends.
pub fn held(folder: &Folder) -> Result<bool, Refusal> {
    let path = folder.0.join(LOCK);
    let lock = match File::open(&path) {
        Ok(lock) => lock,
        // No daemon has run in the folder.
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Refusal::Io(path, error)),
    };
    match lock.try_lock() {
        // Let go of as `lock` closes.
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(Refusal::Io(path, error)),
    }
}

/// Checks that `folder` is this user's alone, as the daemon, its CLI and the
/// shell hook each do before they use it.
fn check(folder: &Path) -> Result<(), Refusal> {
    let found = fs::symlink_metadata(folder);
    let found = found.map_err(|error| Refusal::Io(folder.to_owned(), error))?;
    // A link is not taken for the folder it leads to, even one of yours.
    if !found.is_dir() {
        return Err(Refusal::NotYours(folder.to_owned()));
    }
    if found.uid() != rustix::process::getuid().as_raw() {
        return Err(Refusal::NotYours(folder.to_owned()));
    }
    if found.mode() & 0o077 != 0 {
        return Err(Refusal::Open(folder.to_owned()));
    }
    Ok(())
}
