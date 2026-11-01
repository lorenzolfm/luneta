//! The daemon: one to a folder, and so one to a user on a host.
//!
//! A daemon first takes the folder. It makes it, or checks that the one there
//! is its user's alone, and then takes the lock in it. Only then does it clear
//! away a socket that a daemon before it left, which by then it knows is no
//! running daemon's. The kernel lets go of the lock as the process ends,
//! however it ends, so that a daemon that crashed never keeps the next one
//! from starting.
//!
//! A daemon lives as long as zellij has a session, or a client watches it:
//! once zellij has had no session, and no client has watched it, for `ALONE`,
//! it ends. It takes its socket away first, while it still holds the folder,
//! so that a luneta that comes for it then finds none there, and starts the
//! next.
//!
//! So a luneta that watches it keeps it, as it would not if every session
//! were on another machine. So does the daemon of another machine that
//! watches this one through `luneta bridge`, which would else reach a daemon
//! that ended every `ALONE`, and started again for it: but only while that
//! one is kept, as its `WatchRequest` says, as zellij there has a session, or
//! a client watches it. And a daemon that is not kept asks the machines it
//! watches nothing, which could start their daemons again through the
//! bridge. Two daemons that watched each other would else keep each other
//! running for good, or start each other again, with no one to watch either.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::mem;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task;
use tokio::time::timeout;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

use crate::api::daemon_client::DaemonClient;
use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::{
    self, GoRequest, GoResponse, InfoRequest, InfoResponse, ListAgentsRequest, ListAgentsResponse,
    ListClientsRequest, ListClientsResponse, RefugeRequest, RefugeResponse,
};
use crate::core::{self, Command, Wakes};
use crate::folder::{self, Refusal, LOCK, SOCKET};
use crate::remote::{Bridges, Destination};
use crate::watch::Troubles;
use crate::{
    claude, client, clients, git, github, log, metadata, moves, panes, release, remote, shells,
    watch, zellij,
};

/// How often a folder is read again with no word that it changed: for an agent
/// that crashed, which leaves its file as it was. zellij is asked about each
/// session's panes as often, for what changes in them with no word from zellij
/// or a shell's hook.
const SWEEP: Duration = Duration::from_secs(2);

/// How often the shells' files are read again with no word that they changed,
/// and with them the terminals of the commands that run: a command comes to
/// wait for you to type without a word to any file.
const TERMINAL_SWEEP: Duration = Duration::from_secs(1);

/// How long git rests from a work tree, at the least, before it is asked
/// about it again with no word that it changed: for a file in a directory new
/// to git, or a work tree across the network. `git status` reads the whole
/// tree, which takes a large one a while, and git rests from one for as long
/// as it took over it.
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

/// How long the daemon runs on once zellij has no session, and no client
/// watches it: long enough that ending the last one and opening another does
/// not end it.
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
/// session, and no client has watched it, for `ALONE`. It says on stderr that
/// it listens once it does, which is how whoever started it can tell that it
/// is ready: by then it has read what runs, and answers with all of it. Then
/// it says `unkept`, why what goes wrong is kept in no log, where it is not:
/// not before, so that the first thing one that does not start says is why,
/// which luneta reads back. As it ends, it answers what it was asked already,
/// and no more, and waits for its clients to go for `DRAIN` at the most.
pub async fn serve(claim: Claim, unkept: Option<String>) -> io::Result<()> {
    let listener = UnixListener::bind(&claim.socket)?;
    // One wake-up is all a thread that asks on a sweep needs, however many
    // changes made it.
    let (wake_panes, panes_woken) = mpsc::sync_channel(1);
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let (wake_github, github_woken) = mpsc::sync_channel(1);
    let wakes = Wakes { panes: wake_panes.clone(), git: wake_git.clone(), github: wake_github };
    let core = core::start(wakes)?;
    let (end, ending) = oneshot::channel();
    // Read once, so that the folders the daemon says it reads are the ones it
    // does.
    let sockets = zellij::folder();
    let claude_folder = claude::folder();
    let watchers = Arc::new(Watchers::default());
    let folders = (claude_folder.clone(), sockets.clone());
    follow(&core, &claim.folder, folders, watchers.clone(), end)?;
    panes::follow(core.clone(), wake_panes, panes_woken, metadata::folder(), SWEEP)?;
    git::follow(core.clone(), wake_git, git_woken, WORK_TREE_SWEEP)?;
    github::follow(core.clone(), github_woken, PULL_REQUEST_SWEEP)?;
    let bridges = Arc::new(Bridges::default());
    let folder = claim.folder.clone();
    remote::follow(core.clone(), watchers.kept.subscribe(), SWEEP, folder, Arc::clone(&bridges))?;
    log::say(&format!("listening on {}", claim.socket.display()));
    if let Some(why) = unkept {
        log::say(&format!("what goes wrong is kept in no log: {why}"));
    }
    let started = SystemTime::now();
    let host = Host { started, sockets, claude_folder, core, watchers, bridges };
    let socket = claim.socket.clone();
    let (begun, beginning) = oneshot::channel();
    // Once the daemon has been alone long enough, or the thread that would say
    // so has ended, which leaves it blind to zellij.
    let ended = async move {
        if ending.await.is_err() {
            log::say("zellij's sessions are no longer followed, and so lunetad ends");
        }
        match fs::remove_file(&socket) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => log::say(&format!("{}: {error}", socket.display())),
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
            log::say(&format!("clients still connected after {secs}s are left"));
            Ok(())
        },
    }
}

/// Since when zellij has had no session, and no client has watched the host,
/// as the daemon has seen it.
#[derive(Default)]
struct Alone {
    since: Option<Instant>,
}

impl Alone {
    /// For how long the daemon has been alone, as it is `alone` at `now`.
    fn saw(&mut self, alone: bool, now: Instant) -> Duration {
        if !alone {
            self.since = None;
            return Duration::ZERO;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since)
    }
}

/// Has the core told what Claude Code's files in `claude_folder`, the shells'
/// files in luneta's `folder` and zellij's sockets in `sockets` say, as they
/// change, and says on `end` once zellij has had no session, and none of the
/// `watchers` has watched the host, for `ALONE`. Each look has them say
/// whether the daemon is kept.
fn follow(
    core: &SyncSender<Command>,
    folder: &Path,
    (claude_folder, sockets): (Option<PathBuf>, PathBuf),
    watchers: Arc<Watchers>,
    end: oneshot::Sender<()>,
) -> io::Result<()> {
    match claude_folder {
        Some(folder) => {
            let mut files = claude::SessionFiles::new(&folder);
            let dir = files.dir().to_owned();
            let core = core.clone();
            let read = move || files.scan();
            let tell = move |found| core.send(Command::Agents(found)).is_ok();
            let any = |_: &Path| true;
            watch::watch("claude", dir, RecursiveMode::NonRecursive, any, SWEEP, read, tell)?;
        },
        None => log::say("neither CLAUDE_CONFIG_DIR nor HOME is set, to find agents in"),
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
        // Looked at on each look, whether zellij has a session or not, so that
        // only a client that asked since the look before counts.
        let (clients, daemons) = (watchers.clients.looked(), watchers.daemons.looked());
        let kept = !scan.alone() || clients;
        watchers.kept.send_if_modified(|was| mem::replace(was, kept) != kept);
        if alone.saw(!kept && !daemons, Instant::now()) >= ALONE
            && let Some(end) = end.take()
        {
            let secs = ALONE.as_secs();
            let none = format!("zellij has had no session for {secs}s, nor lunetad a watcher");
            log::say(&format!("{none}, and so lunetad ends"));
            // The server has ended already, if no one waits for the word.
            let _ = end.send(());
        }
        core.send(Command::Sockets { sockets: scan.sockets, unread: scan.unread }).is_ok()
    };
    let wanted = zellij::of_sessions;
    watch::watch("zellij", sockets, RecursiveMode::Recursive, wanted, SWEEP, read, tell)
}

/// Who watches the host: clients, as luneta, and the daemons of other
/// machines that watch this one and run for reasons of their own, each as its
/// `WatchRequest` says. A daemon that does not is no one: two daemons that
/// watched each other would else keep each other running for good.
#[derive(Default)]
struct Watchers {
    clients: Watching,
    daemons: Watching,
    /// Whether the daemon is kept: runs for reasons of its own, as zellij has
    /// a session, or a client watches it, as it last looked, or a client has
    /// come since. Only a daemon that is kept runs ssh to the machines it
    /// watches, and keeps their daemons running.
    kept: tokio::sync::watch::Sender<bool>,
}

impl Watchers {
    /// Counts one that waits on `Watch`, until what this returns is dropped,
    /// as `kept`, of its `WatchRequest`, says who it is: a client, where it
    /// says nothing, and else the daemon of another machine, which is no one
    /// where it runs for no reason of its own.
    fn waits(&self, kept: Option<bool>) -> Option<Waits<'_>> {
        let watching = match kept {
            None => &self.clients,
            Some(true) => &self.daemons,
            Some(false) => return None,
        };
        watching.asked.store(true, Ordering::Relaxed);
        watching.waiting.fetch_add(1, Ordering::Relaxed);
        // A client keeps the daemon at once, so that it need not wait for
        // the next look for the machines the daemon watches to be reached.
        if kept.is_none() {
            self.kept.send_if_modified(|was| !mem::replace(was, true));
        }
        Some(Waits(watching))
    }
}

/// Whether some watch the host: one waits on `Watch` now, or one has asked
/// since the daemon last looked. A luneta asks again as soon as it is
/// answered, and so is always one or the other, on this machine or another,
/// and so is a daemon that watches this machine from another.
#[derive(Default)]
struct Watching {
    waiting: AtomicUsize,
    asked: AtomicBool,
}

impl Watching {
    /// Whether one watches the host. The next look counts only those that
    /// ask after this one.
    fn looked(&self) -> bool {
        let asked = self.asked.swap(false, Ordering::Relaxed);
        asked || self.waiting.load(Ordering::Relaxed) > 0
    }
}

/// One that waits on `Watch`.
struct Waits<'a>(&'a Watching);

impl Drop for Waits<'_> {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, Ordering::Relaxed);
    }
}

/// What the daemon answers from.
struct Host {
    started: SystemTime,
    /// The folder it reads zellij's sessions in.
    sockets: PathBuf,
    /// The folder it reads Claude Code's sessions in, if its environment
    /// named one.
    claude_folder: Option<PathBuf>,
    core: SyncSender<Command>,
    watchers: Arc<Watchers>,
    /// The daemons of the other machines it watches, while it reaches them.
    bridges: Arc<Bridges>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<InfoRequest>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: release::VERSION.to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
            zellij_folder: Some(self.sockets.as_os_str().as_bytes().to_vec()),
            claude_folder: self.claude_folder.as_ref().map(|it| it.as_os_str().as_bytes().to_vec()),
            built: release::BUILT,
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
        let request = request.into_inner();
        let _waits = self.watchers.waits(request.kept);
        let after = request.after;
        let watched = match timeout(WATCH, self.watched(after)).await {
            Ok(watched) => watched,
            // It waited out its time, with the host as it was.
            Err(_) => self.watched(None).await,
        };
        watched.map(Response::new)
    }

    async fn go(&self, request: Request<GoRequest>) -> Result<Response<GoResponse>, Status> {
        let GoRequest { on, at, to, pane } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.go(GoRequest { on: None, at, to, pane }).await;
        }
        let went = task::spawn_blocking(move || moves::went(&at, &to, pane)).await;
        let went = went.map_err(|error| unfinished(&error))?;
        went.map_err(Status::failed_precondition)?;
        Ok(Response::new(GoResponse {}))
    }

    async fn list_clients(
        &self,
        request: Request<ListClientsRequest>,
    ) -> Result<Response<ListClientsResponse>, Status> {
        let ListClientsRequest { on, session } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_clients(ListClientsRequest { on: None, session }).await;
        }
        let listed = task::spawn_blocking(move || clients_of(&session)).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let clients = listed.map_err(Status::failed_precondition)?;
        Ok(Response::new(ListClientsResponse { clients }))
    }

    async fn refuge(
        &self,
        request: Request<RefugeRequest>,
    ) -> Result<Response<RefugeResponse>, Status> {
        let RefugeRequest { on, seats } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.refuge(RefugeRequest { on: None, seats }).await;
        }
        let sockets = self.sockets.clone();
        let found = task::spawn_blocking(move || {
            let seats: Vec<&str> = seats.iter().map(String::as_str).collect();
            // No session here is one luneta's terminal is a pane of: that
            // luneta's client is on another machine.
            moves::refuge(&sockets, &seats, None, |session| clients_of(session).ok())
        });
        let refuge = |moves::Refuge { session, before }| api::Refuge { session, before };
        let answer = match found.await.map_err(|error| unfinished(&error))? {
            moves::Ended::Kept => RefugeResponse { went: false, refuge: None },
            moves::Ended::Went(found) => RefugeResponse { went: true, refuge: found.map(refuge) },
        };
        Ok(Response::new(answer))
    }
}

/// The clients of `session` here, as zellij lists them now, or why not.
fn clients_of(session: &str) -> Result<Vec<api::Client>, String> {
    let listed = clients::list(std::process::Command::new("zellij"), session)?;
    Ok(listed.into_iter().map(clients::Client::to_api).collect())
}

/// Why what the daemon asked of zellij did not answer: the thread it was asked
/// on panicked.
fn unfinished(error: &task::JoinError) -> Status {
    Status::internal(format!("zellij's answer was lost: {error}"))
}

impl Host {
    /// The daemon of the machine `on` names, where it names one, which the
    /// question is handed on to; none for this machine; or why not, as one
    /// this daemon does not reach now.
    fn bridged(&self, on: Option<api::On>) -> Result<Option<DaemonClient<Channel>>, Status> {
        let Some(api::On { destination }) = on else {
            return Ok(None);
        };
        let destination: Destination =
            destination.parse().map_err(|why: String| Status::invalid_argument(why))?;
        let daemon = self.bridges.to(&destination);
        let unreached = || Status::unavailable(format!("lunetad does not reach {destination} now"));
        daemon.map(Some).ok_or_else(unreached)
    }

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
