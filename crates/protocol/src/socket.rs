use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SocketId {
    device: u64,
    inode: u64,
}

impl SocketId {
    #[must_use]
    pub fn at(socket: &Path) -> Option<SocketId> {
        let found = fs::symlink_metadata(socket).ok()?;
        found.file_type().is_socket().then_some(SocketId { device: found.dev(), inode: found.ino() })
    }
}

#[derive(Clone, Debug)]
pub struct Bound {
    socket: PathBuf,
    id: SocketId,
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
        write!(f, ", so nothing reaches it again")
    }
}

pub fn bind(socket: &Path) -> io::Result<(UnixListener, Bound)> {
    assert!(socket.is_absolute(), "{} is looked at from any folder", socket.display());
    let listener = UnixListener::bind(socket)?;
    let bound = Bound::of(socket).map_err(|error| {
        let socket = socket.display();
        let left = format!("{socket} is left as it is, as it is not known to be ours: {error}");
        io::Error::new(error.kind(), left)
    })?;
    Ok((listener, bound))
}

impl Bound {
    fn of(socket: &Path) -> io::Result<Bound> {
        let found = fs::symlink_metadata(socket)?;
        if !found.file_type().is_socket() {
            return Err(io::Error::other("it is no socket just after it was bound"));
        }
        let id = SocketId { device: found.dev(), inode: found.ino() };
        Ok(Bound { socket: socket.to_owned(), id })
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    #[must_use]
    pub fn id(&self) -> SocketId {
        self.id
    }

    pub fn found(&self) -> io::Result<Found> {
        let lost = |lost| Ok(Found::Lost(Unreached { socket: self.socket.clone(), lost }));
        match fs::symlink_metadata(&self.socket) {
            Ok(found)
                if found.file_type().is_socket()
                    && SocketId { device: found.dev(), inode: found.ino() } == self.id =>
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
