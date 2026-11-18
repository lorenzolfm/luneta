use std::ffi::OsString;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

pub const SOCKET: &str = "mux.sock";

pub const LOCK: &str = "mux.lock";

pub const ANOTHER_RUNS: u8 = 3;

#[must_use]
pub fn find() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|root| !root.is_empty());
    let root = set("XDG_RUNTIME_DIR").or_else(user_temp).or_else(|| set("TMPDIR"))?;
    Some(PathBuf::from(root).join("luneta"))
}

#[cfg(target_os = "macos")]
fn user_temp() -> Option<OsString> {
    use std::os::unix::ffi::OsStrExt as _;

    let mut buffer = [0_u8; 1024];
    #[expect(
        unsafe_code,
        reason = "confstr is C, which libc only declares. It writes no more than the \
                  `buffer.len()` bytes it is given"
    )]
    let needed = unsafe {
        libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), buffer.len())
    };
    if needed == 0 || needed > buffer.len() {
        return None;
    }
    let root = &buffer[..needed - 1];
    (!root.is_empty()).then(|| std::ffi::OsStr::from_bytes(root).to_owned())
}

#[cfg(not(target_os = "macos"))]
fn user_temp() -> Option<OsString> {
    None
}

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
