//! The daemon: one to a folder, and so one to a user on a host.
//!
//! A daemon first takes the folder. It makes it, or checks that the one there
//! is its user's alone, and then takes the lock in it. Only then does it clear
//! away a socket that a daemon before it left, which by then it knows is no
//! running daemon's. The kernel lets go of the lock as the process ends,
//! however it ends, so that a daemon that crashed never keeps the next one
//! from starting.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{Duration, SystemTime};

use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::time::timeout;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::{self, InfoRequest, InfoResponse, ListAgentsRequest, ListAgentsResponse};
use crate::core::{self, Command, Wakes};
use crate::folder::{self, Refusal, LOCK, SOCKET};
use crate::watch::Troubles;
use crate::{claude, client, git, panes, shells, watch, zellij};

/// How often a folder is read again with no word that it changed: for an agent
/// that crashed, which leaves its file as it was. zellij is asked for each
/// session's panes as often, as it says nothing as they change either.
const SWEEP: Duration = Duration::from_secs(2);

/// How often the shells' files are read again with no word that they changed,
/// and with them the terminals of the commands that run: a command comes to
/// wait for you to type without a word to any file.
const TERMINAL_SWEEP: Duration = Duration::from_secs(1);

/// How long git rests from a work tree, at the least, before it is asked
/// about it again. `git status` reads the whole tree, which takes a large one
/// a while, and git rests from one for as long as it took over it.
const WORK_TREE_SWEEP: Duration = Duration::from_secs(5);

/// How long `Watch` waits for the host to change before it answers with the
/// host as it was: within the time a client waits for an answer.
const WATCH: Duration = Duration::from_secs(4);

const _: () = assert!(WATCH.as_millis() < client::ANSWER.as_millis());

/// The most questions a client has open at once on one connection, which
/// HTTP/2 tells it: `luneta` asks one.
const QUESTIONS_MAX: u32 = 16;

/// A folder that this process's daemon holds, and the socket it listens on
/// there.
pub struct Claim {
    folder: PathBuf,
    socket: PathBuf,
    /// Held for as long as the process runs.
    _lock: File,
}

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
            Ok(()) => {},
            // No daemon before this one left a socket.
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => return Err(Refusal::Io(socket, error)),
        }
        Ok(Claim { folder: folder.to_owned(), socket, _lock: lock })
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

/// Makes `folder` for this user alone, unless it is there, and checks that it
/// is that, as the shell hook does before it writes in it. One just made is
/// checked too, as whatever made it has no say in what it turned out to be.
fn own(folder: &Path) -> Result<(), Refusal> {
    match DirBuilder::new().mode(0o700).create(folder) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(folder.to_owned(), error)),
    }
    folder::check(folder)
}

/// Listens on the claim's socket and answers there for as long as the
/// process runs. It says on stderr that it listens once it does, which is
/// how whoever started it can tell that it is ready: by then it has read
/// what runs, and answers with all of it.
pub async fn serve(claim: Claim) -> io::Result<()> {
    let listener = UnixListener::bind(&claim.socket)?;
    // One wake-up is all a thread that asks on a sweep needs, however many
    // changes made it.
    let (wake_panes, panes_woken) = mpsc::sync_channel(1);
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let core = core::start(Wakes { panes: wake_panes, git: wake_git })?;
    follow(&core, &claim.folder)?;
    panes::follow(core.clone(), panes_woken, SWEEP)?;
    git::follow(core.clone(), git_woken, WORK_TREE_SWEEP)?;
    eprintln!("lunetad: listening on {}", claim.socket.display());
    let host = Host { started: SystemTime::now(), core };
    // Bounds of the daemon's own, rather than tonic's, which are none.
    Server::builder()
        .max_concurrent_streams(QUESTIONS_MAX)
        .timeout(client::ANSWER)
        .add_service(DaemonServer::new(host))
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await
        .map_err(io::Error::other)
}

/// Has the core told what Claude Code's files, the shells' files in luneta's
/// `folder` and zellij's sockets say, as they change.
fn follow(core: &SyncSender<Command>, folder: &Path) -> io::Result<()> {
    match claude::folder() {
        Some(folder) => {
            let mut files = claude::SessionFiles::new(&folder);
            let dir = files.dir().to_owned();
            let core = core.clone();
            let read = move || files.scan();
            let tell = move |found| core.send(Command::Agents(found)).is_ok();
            let any = |_: &Path| true;
            watch::watch("claude", dir, RecursiveMode::NonRecursive, any, SWEEP, read, tell)?;
        },
        None => eprintln!("lunetad: neither CLAUDE_CONFIG_DIR nor HOME is set, to find agents in"),
    }
    let mut shells = shells::Shells::new(folder);
    let dir = shells.dir().to_owned();
    let read = move || shells.read();
    let tell = {
        let core = core.clone();
        move |found| core.send(Command::Shells(found)).is_ok()
    };
    let any = |_: &Path| true;
    watch::watch("shells", dir, RecursiveMode::Recursive, any, TERMINAL_SWEEP, read, tell)?;
    let folder = zellij::folder();
    let core = core.clone();
    let read = {
        let folder = folder.clone();
        let mut troubles = Troubles::default();
        move || zellij::scan(&folder, &mut troubles)
    };
    let tell = move |sockets| core.send(Command::Sockets(sockets)).is_ok();
    let wanted = zellij::of_sessions;
    watch::watch("zellij", folder, RecursiveMode::Recursive, wanted, SWEEP, read, tell)
}

/// What the daemon answers from.
struct Host {
    started: SystemTime,
    core: SyncSender<Command>,
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
        self.ask(Command::List).await.map(Response::new)
    }

    async fn watch(
        &self,
        request: Request<api::WatchRequest>,
    ) -> Result<Response<api::Host>, Status> {
        let after = request.into_inner().after;
        let watched = match timeout(WATCH, self.watched(after)).await {
            Ok(watched) => watched,
            // It waited out its time, with the host as it was.
            Err(_) => self.watched(None).await,
        };
        watched.map(Response::new)
    }
}

impl Host {
    /// Asks the core, and waits for its answer. A core with too many
    /// questions open has the client told that the daemon is busy, rather
    /// than wait.
    async fn ask<T>(
        &self,
        question: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, Status> {
        let (answer, answered) = oneshot::channel();
        let stopped = || Status::unavailable("the daemon's core has stopped");
        match self.core.try_send(question(answer)) {
            Ok(()) => {},
            Err(TrySendError::Full(_)) => {
                return Err(Status::resource_exhausted("the daemon has too many questions open"));
            },
            Err(TrySendError::Disconnected(_)) => return Err(stopped()),
        }
        answered.await.map_err(|_| stopped())
    }

    /// The host once it is no longer as `after` saw it.
    async fn watched(&self, after: Option<api::Version>) -> Result<api::Host, Status> {
        let host = self.ask(|answer| Command::Watch { after, answer }).await?;
        host.ok_or_else(|| Status::resource_exhausted("too many clients wait for the host"))
    }
}
