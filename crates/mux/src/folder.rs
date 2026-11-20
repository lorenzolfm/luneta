use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{
    DirBuilderExt as _, FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _,
};
use std::path::{Path, PathBuf};

pub const SOCKET: &str = "mux.sock";

pub const LOCK: &str = "mux.lock";

pub const ANOTHER_RUNS: u8 = 3;

#[derive(Debug)]
pub enum Refusal {
    NotYours(PathBuf),
    Open(PathBuf),
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

pub struct Claim {
    socket: PathBuf,
    _lock: File,
}

impl Claim {
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn bound(&self) -> io::Result<Bound> {
        Bound::of(&self.socket).map_err(|error| {
            let socket = self.socket.display();
            let left = format!("{socket} is left as it is, as it is not known to be ours: {error}");
            io::Error::new(error.kind(), left)
        })
    }
}

#[derive(Clone, Debug)]
pub struct Bound {
    socket: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    Ours,
    Lost(Unreached),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreached {
    socket: PathBuf,
    lost: Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lost {
    Gone,
    Replaced,
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let socket = self.socket.display();
        match self.lost {
            Lost::Gone => write!(f, "{socket} is gone")?,
            Lost::Replaced => write!(f, "{socket} is another file now")?,
        }
        write!(f, ", so nothing reaches luneta-mux again")
    }
}

impl Bound {
    fn of(socket: &Path) -> io::Result<Bound> {
        assert!(socket.is_absolute(), "{} is looked at from any folder", socket.display());
        let found = fs::symlink_metadata(socket)?;
        if !found.file_type().is_socket() {
            return Err(io::Error::other("it is no socket just after it was bound"));
        }
        Ok(Bound { socket: socket.to_owned(), device: found.dev(), inode: found.ino() })
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    pub fn found(&self) -> io::Result<Found> {
        let lost = |lost| Ok(Found::Lost(Unreached { socket: self.socket.clone(), lost }));
        match fs::symlink_metadata(&self.socket) {
            Ok(found)
                if found.file_type().is_socket()
                    && (found.dev(), found.ino()) == (self.device, self.inode) =>
            {
                Ok(Found::Ours)
            },
            Ok(_) => lost(Lost::Replaced),
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                lost(Lost::Gone)
            },
            Err(error) => Err(error),
        }
    }

    pub fn unbind(&self) -> io::Result<()> {
        let Found::Ours = self.found()? else {
            return Ok(());
        };
        match fs::remove_file(&self.socket) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

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
