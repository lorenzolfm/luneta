use std::fmt;
use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

pub const SOCKET: &str = "lunetad.sock";

pub const LOCK: &str = "lunetad.lock";

pub const LOG: &str = "lunetad.log";

#[must_use]
pub fn find() -> Option<PathBuf> {
    ["XDG_RUNTIME_DIR", "TMPDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|root| !root.is_empty())
        .map(|root| PathBuf::from(root).join("luneta"))
}

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
                write!(f, "another lunetad already runs in {}", folder.display())
            },
            Refusal::Io(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for Refusal {}

#[derive(Clone, Debug)]
pub struct Folder(PathBuf);

impl Folder {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.0.join(SOCKET)
    }
}

pub fn open(path: &Path) -> Result<Option<Folder>, Refusal> {
    match check(path) {
        Ok(()) => Ok(Some(Folder(path.to_owned()))),
        Err(Refusal::Io(_, error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

pub fn make(path: &Path) -> Result<Folder, Refusal> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(path.to_owned(), error)),
    }
    check(path)?;
    Ok(Folder(path.to_owned()))
}

pub fn held(folder: &Folder) -> Result<bool, Refusal> {
    let path = folder.0.join(LOCK);
    let lock = match File::open(&path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Refusal::Io(path, error)),
    };
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(Refusal::Io(path, error)),
    }
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
