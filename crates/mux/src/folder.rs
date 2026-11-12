//! Where the server listens: `luneta` in `$XDG_RUNTIME_DIR`, or in `$TMPDIR`
//! where that is not set, as on macOS, the folder `lunetad` and the shell hook
//! use too, by the same rules. The server keeps its own copy of them, as it
//! does not depend on `lunetad`.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

/// The socket the server listens on, in the folder.
pub const SOCKET: &str = "mux.sock";

/// The file the server holds a lock on while it runs, in the folder.
pub const LOCK: &str = "mux.lock";

/// How the server ends when another holds its folder: apart from the other
/// ways it fails, so that whoever started it reaches the one that runs.
pub const ANOTHER_RUNS: u8 = 3;

/// The folder, by this process's variables. A variable set to nothing is not
/// set.
#[must_use]
pub fn find() -> Option<PathBuf> {
    ["XDG_RUNTIME_DIR", "TMPDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|root| !root.is_empty())
        .map(|root| PathBuf::from(root).join("luneta"))
}

/// Why the server does not run in a folder.
#[derive(Debug)]
pub enum Refusal {
    /// It is a link, or not a folder, or another user's.
    NotYours(PathBuf),
    /// Other users may reach into it, and so reach the socket.
    Open(PathBuf),
    /// Another server holds it.
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
                write!(f, "another luneta-mux already runs in {}", folder.display())
            },
            Refusal::Io(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for Refusal {}

/// The folder, held for one server: its lock is taken until this drops, and
/// no socket of an earlier server is left in it.
pub struct Claim {
    socket: PathBuf,
    _lock: File,
}

impl Claim {
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// Makes the folder at `path` for this user alone, unless it is there, checks
/// that it is, and claims it for this server.
pub fn claim(path: &Path) -> Result<Claim, Refusal> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(path.to_owned(), error)),
    }
    check(path)?;
    let lock_path = path.join(LOCK);
    let lock =
        OpenOptions::new().create(true).truncate(false).write(true).mode(0o600).open(&lock_path);
    let lock = lock.map_err(|error| Refusal::Io(lock_path.clone(), error))?;
    match lock.try_lock() {
        Ok(()) => {},
        Err(TryLockError::WouldBlock) => return Err(Refusal::Running(path.to_owned())),
        Err(TryLockError::Error(error)) => return Err(Refusal::Io(lock_path, error)),
    }
    let socket = path.join(SOCKET);
    match fs::remove_file(&socket) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::NotFound => {},
        Err(error) => return Err(Refusal::Io(socket, error)),
    }
    Ok(Claim { socket, _lock: lock })
}

/// Checks that `folder` is this user's alone.
fn check(folder: &Path) -> Result<(), Refusal> {
    let found = fs::symlink_metadata(folder);
    let found = found.map_err(|error| Refusal::Io(folder.to_owned(), error))?;
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
