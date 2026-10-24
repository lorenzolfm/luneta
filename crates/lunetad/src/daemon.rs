//! The daemon: one to a folder, and so one to a user on a host.
//!
//! A daemon first takes the folder. It makes it, or checks that the one there
//! is its user's alone, and then takes the lock in it. Only then does it clear
//! away a socket that a daemon before it left, which by then it knows is no
//! running daemon's. The kernel lets go of the lock as the process ends,
//! however it ends, so that a daemon that crashed never keeps the next one
//! from starting.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::{InfoRequest, InfoResponse, ListAgentsRequest, ListAgentsResponse};
use crate::core::{self, Command};
use crate::folder::{LOCK, SOCKET};
use crate::{claude, watch, zellij};

/// How often a folder is read again with no word that it changed: for an agent
/// that crashed, which leaves its file as it was.
const SWEEP: Duration = Duration::from_secs(2);

/// A folder that this process's daemon holds, and the socket it listens on
/// there.
pub struct Claim {
    socket: PathBuf,
    /// Held for as long as the process runs.
    _lock: File,
}

/// Why a daemon does not run in a folder.
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

impl Claim {
    /// Takes `folder` for this process's daemon, and makes it if it is not
    /// there.
    pub fn take(folder: &Path) -> Result<Claim, Refusal> {
        own(folder)?;
        let path = folder.join(LOCK);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|error| Refusal::Io(path.clone(), error))?;
        match lock.try_lock() {
            Ok(()) => {},
            Err(TryLockError::WouldBlock) => return Err(Refusal::Running(folder.to_owned())),
            Err(TryLockError::Error(error)) => return Err(Refusal::Io(path, error)),
        }
        let socket = folder.join(SOCKET);
        match fs::remove_file(&socket) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(Refusal::Io(socket, error));
            },
            _ => {},
        }
        Ok(Claim { socket, _lock: lock })
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// Makes `folder` for this user alone, or checks that the one there is that,
/// as the shell hook does before it writes in it.
fn own(folder: &Path) -> Result<(), Refusal> {
    match DirBuilder::new().mode(0o700).create(folder) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(folder.to_owned(), error)),
    }
    let found = fs::symlink_metadata(folder).map_err(|error| Refusal::Io(folder.to_owned(), error))?;
    if !found.is_dir() || found.uid() != rustix::process::getuid().as_raw() {
        return Err(Refusal::NotYours(folder.to_owned()));
    }
    if found.mode() & 0o077 != 0 {
        return Err(Refusal::Open(folder.to_owned()));
    }
    Ok(())
}

/// Listens on the claim's socket and answers there for as long as the
/// process runs. It says on stderr that it listens once it does, which is
/// how whoever started it can tell that it is ready: by then it has read
/// what runs, and answers with all of it.
pub async fn serve(claim: Claim) -> io::Result<()> {
    let listener = UnixListener::bind(&claim.socket)?;
    let core = core::start()?;
    follow(&core)?;
    eprintln!("lunetad: listening on {}", claim.socket.display());
    let host = Host { started: SystemTime::now(), core };
    Server::builder()
        .add_service(DaemonServer::new(host))
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await
        .map_err(io::Error::other)
}

/// Has the core told what Claude Code's files and zellij's sockets say, as
/// they change.
fn follow(core: &Sender<Command>) -> io::Result<()> {
    match claude::folder() {
        Some(folder) => {
            let mut sessions = claude::Sessions::new(&folder);
            let dir = sessions.dir().to_owned();
            let core = core.clone();
            let read = move || sessions.scan();
            let tell = move |agents| core.send(Command::Agents(agents)).is_ok();
            watch::watch("claude", dir, RecursiveMode::NonRecursive, SWEEP, read, tell)?;
        },
        None => eprintln!("lunetad: neither CLAUDE_CONFIG_DIR nor HOME is set, to find agents in"),
    }
    let folder = zellij::folder();
    let core = core.clone();
    let read = {
        let folder = folder.clone();
        move || zellij::scan(&folder)
    };
    let tell = move |sockets| core.send(Command::Sockets(sockets)).is_ok();
    watch::watch("zellij", folder, RecursiveMode::Recursive, SWEEP, read, tell)
}

/// What the daemon answers from.
struct Host {
    started: SystemTime,
    core: Sender<Command>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<InfoRequest>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
        }))
    }

    async fn list_agents(
        &self,
        _: Request<ListAgentsRequest>,
    ) -> Result<Response<ListAgentsResponse>, Status> {
        let (answer, answered) = oneshot::channel();
        let stopped = || Status::unavailable("the daemon's core has stopped");
        self.core.send(Command::List(answer)).map_err(|_| stopped())?;
        let agents = answered.await.map_err(|_| stopped())?;
        Ok(Response::new(ListAgentsResponse { agents }))
    }
}
