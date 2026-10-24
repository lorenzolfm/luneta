//! Where luneta keeps the files of what runs on a host: the folder `luneta`
//! in `$XDG_RUNTIME_DIR`, or in `$TMPDIR` where that is not set, as on macOS,
//! which has no `XDG_RUNTIME_DIR`. The shell hook keeps its commands there by
//! the same rule, and the daemon and its CLI find each other there.

use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The socket the daemon listens on, in the folder.
pub const SOCKET: &str = "lunetad.sock";

/// The file the daemon holds a lock on while it runs, in the folder.
pub const LOCK: &str = "lunetad.lock";

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

/// Checks that `folder` is this user's alone, as the daemon, its CLI and the
/// shell hook each do before they use it: a socket in a folder that is not
/// could be anyone's.
pub fn check(folder: &Path) -> Result<(), Refusal> {
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
