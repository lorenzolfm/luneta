//! The daemon: one to a folder, and so one to a user on a host.
//!
//! A daemon first takes the folder. It makes it, or checks that the one there
//! is its user's alone, and then takes the lock in it. Only then does it clear
//! away a socket that a daemon before it left, which by then it knows is no
//! running daemon's. The kernel lets go of the lock as the process ends,
//! however it ends, so that a daemon that crashed never keeps the next one
//! from starting.
//!
//! A daemon lives as long as zellij does: once zellij has had no session for
//! `ALONE`, it ends. It takes its socket away first, while it still holds the
//! folder, so that a luneta that comes for it then finds none there, and
//! starts the next.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime};

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
use crate::{claude, client, git, github, panes, shells, watch, zellij};

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

/// How long gh rests from a branch before it is asked about its pull request
/// again. gh asks GitHub, and each ask counts against the hourly limit that
/// GitHub puts on the user, and that each gh of theirs shares.
const PULL_REQUEST_SWEEP: Duration = Duration::from_mins(1);

/// How long `Watch` waits for the host to change before it answers with the
/// host as it was: within the time a client waits for an answer.
const WATCH: Duration = Duration::from_secs(4);

const _: () = assert!(WATCH.as_millis() < client::ANSWER.as_millis());

/// The most questions a client has open at once on one connection, which
/// HTTP/2 tells it: `luneta` asks one.
const QUESTIONS_MAX: u32 = 16;

/// How long the daemon runs on once zellij has no session: long enough that
/// ending the last one and opening another does not end it.
const ALONE: Duration = Duration::from_secs(30);

/// How long the daemon waits, as it ends, for its clients to go: as long as
/// a question takes to answer, at the most, and so long enough for each one
/// asked already. A client that keeps its connection open for longer, as one
/// that never says anything, would keep it from ever ending, and holding its
/// folder, with no socket there to reach it by.
const DRAIN: Duration = client::ANSWER;

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
        folder::make(folder)?;
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

/// Listens on the claim's socket and answers there until zellij has had no
/// session for `ALONE`. It says on stderr that it listens once it does, which
/// is how whoever started it can tell that it is ready: by then it has read
/// what runs, and answers with all of it. As it ends, it answers what it was
/// asked already, and no more, and waits for its clients to go for `DRAIN`
/// at the most.
pub async fn serve(claim: Claim) -> io::Result<()> {
    let listener = UnixListener::bind(&claim.socket)?;
    // One wake-up is all a thread that asks on a sweep needs, however many
    // changes made it.
    let (wake_panes, panes_woken) = mpsc::sync_channel(1);
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let (wake_github, github_woken) = mpsc::sync_channel(1);
    let core = core::start(Wakes { panes: wake_panes, git: wake_git, github: wake_github })?;
    let (end, ending) = oneshot::channel();
    // Read once, so that the folder the daemon says it reads is the one it
    // does.
    let sockets = zellij::folder();
    follow(&core, &claim.folder, sockets.clone(), end)?;
    panes::follow(core.clone(), panes_woken, SWEEP)?;
    git::follow(core.clone(), git_woken, WORK_TREE_SWEEP)?;
    github::follow(core.clone(), github_woken, PULL_REQUEST_SWEEP)?;
    eprintln!("lunetad: listening on {}", claim.socket.display());
    let host = Host { started: SystemTime::now(), sockets, core };
    let socket = claim.socket.clone();
    let (begun, beginning) = oneshot::channel();
    // Once zellij has been alone long enough, or the thread that would say so
    // has ended, which leaves the daemon blind to it.
    let ended = async move {
        if ending.await.is_err() {
            eprintln!("lunetad: zellij's sessions are no longer followed, and so lunetad ends");
        }
        match fs::remove_file(&socket) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => eprintln!("lunetad: {}: {error}", socket.display()),
        }
        let _ = begun.send(());
    };
    // Bounds of the daemon's own, rather than tonic's, which are none.
    let served = tokio::spawn(
        Server::builder()
            .max_concurrent_streams(QUESTIONS_MAX)
            .timeout(client::ANSWER)
            .add_service(DaemonServer::new(host))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), ended),
    );
    // The daemon has begun to end, or the server has ended on its own, which
    // lets go of `begun`. Either way, the server ends within `DRAIN`, as it
    // waits for every connection to close.
    let _ = beginning.await;
    match timeout(DRAIN, served).await {
        Ok(Ok(served)) => served.map_err(io::Error::other),
        Ok(Err(error)) => Err(io::Error::other(error)),
        Err(_) => {
            let secs = DRAIN.as_secs();
            eprintln!("lunetad: clients still connected after {secs}s are left");
            Ok(())
        },
    }
}

/// Since when zellij has had no session, as the daemon has seen it.
#[derive(Default)]
struct Alone {
    since: Option<Instant>,
}

impl Alone {
    /// For how long zellij has had no session, as it is `alone` at `now`.
    fn saw(&mut self, alone: bool, now: Instant) -> Duration {
        if !alone {
            self.since = None;
            return Duration::ZERO;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since)
    }
}

/// Has the core told what Claude Code's files, the shells' files in luneta's
/// `folder` and zellij's sockets in `sockets` say, as they change, and says
/// on `end` once zellij has had no session for `ALONE`.
fn follow(
    core: &SyncSender<Command>,
    folder: &Path,
    sockets: PathBuf,
    end: oneshot::Sender<()>,
) -> io::Result<()> {
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
    let core = core.clone();
    let read = {
        let sockets = sockets.clone();
        let mut troubles = Troubles::default();
        move || zellij::scan(&sockets, &mut troubles)
    };
    let mut alone = Alone::default();
    let mut end = Some(end);
    let tell = move |scan: zellij::Scan| {
        if alone.saw(scan.alone(), Instant::now()) >= ALONE
            && let Some(end) = end.take()
        {
            let secs = ALONE.as_secs();
            eprintln!("lunetad: zellij has had no session for {secs}s, and so lunetad ends");
            // The server has ended already, if no one waits for the word.
            let _ = end.send(());
        }
        core.send(Command::Sockets { sockets: scan.sockets, unread: scan.unread }).is_ok()
    };
    let wanted = zellij::of_sessions;
    watch::watch("zellij", sockets, RecursiveMode::Recursive, wanted, SWEEP, read, tell)
}

/// What the daemon answers from.
struct Host {
    started: SystemTime,
    /// The folder it reads zellij's sessions in.
    sockets: PathBuf,
    core: SyncSender<Command>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<InfoRequest>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
            zellij_folder: Some(self.sockets.as_os_str().as_bytes().to_vec()),
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::Alone;

    /// zellij is alone from the first look that finds no session, and a
    /// session, however short its life, has it start over: ending the last
    /// one and opening another does not end the daemon.
    #[test]
    fn zellij_is_alone_from_the_first_look_that_finds_no_session() {
        let start = Instant::now();
        let at = |secs| start + Duration::from_secs(secs);
        let mut alone = Alone::default();
        assert_eq!(alone.saw(true, at(0)), Duration::ZERO);
        assert_eq!(alone.saw(true, at(20)), Duration::from_secs(20));
        assert_eq!(alone.saw(false, at(25)), Duration::ZERO);
        assert_eq!(alone.saw(true, at(40)), Duration::ZERO);
        assert_eq!(alone.saw(true, at(69)), Duration::from_secs(29));
        assert_eq!(alone.saw(true, at(70)), Duration::from_secs(30));
    }
}
