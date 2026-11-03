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
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task;
use tokio::time::timeout;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tokio_stream::Stream;
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status};

use crate::api::close_request;
use crate::api::closes_response::{self, Pane, Session as WholeSession, Tab as ClosedTab};
use crate::api::daemon_client::DaemonClient;
use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::keep_printed_response::{self, File as PrintedFile, Nothing, Top};
use crate::api::list_dirs_response::Dir;
use crate::api::list_exited_response::Exited;
use crate::api::list_skills_response::Skill;
use crate::api::open_request::{self, Opens};
use crate::api::refuge_response::{self, Kept, Went};
use crate::api::{
    self, AskRequest, AskResponse, CloseRequest, CloseResponse, ClosesRequest, ClosesResponse,
    DeleteExitedRequest, DeleteExitedResponse, DumpScreenRequest, DumpScreenResponse, GoRequest,
    GoResponse, InfoRequest, InfoResponse, KeepPrintedRequest, KeepPrintedResponse, KillRequest,
    KillResponse, ListAgentsRequest, ListAgentsResponse, ListClientsRequest, ListClientsResponse,
    ListContentsRequest, ListContentsResponse, ListDirsRequest, ListDirsResponse,
    ListExitedRequest, ListExitedResponse, ListSkillsRequest, ListSkillsResponse,
    NameSessionRequest, NameSessionResponse, OpenRequest, OpenResponse, PasteRequest,
    PasteResponse, RefugeRequest, RefugeResponse, RenameSessionRequest, RenameSessionResponse,
    StartRequest, StartResponse, TranscriptEndsRequest, TranscriptEndsResponse,
};
use crate::asks::{Asked, Step};
use crate::closes::{Close, Closes};
use crate::core::{self, Command, Wakes};
use crate::directory::Directory;
use crate::folder::{self, Refusal, LOCK, SOCKET};
use crate::links::{Pasted, Ready};
use crate::remote::{Bridges, Destination};
use crate::watch::Troubles;
use crate::zellij::SessionName;
use crate::opens::{Open, Tab};
use crate::printed::{Kept as KeptFile, Printed};
use crate::transcripts::Reading;
use crate::{
    claude, client, clients, closes, ended, eza, git, github, log, metadata, moves, names, opens,
    panes, printed, release, remote, renames, screens, shells, skills, watch, zellij, zoxide,
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

/// The most steps of a turn of the ask box's agent that wait to be handed
/// on: a turn takes a few dozen, and `claude` waits while the client takes
/// in this many.
const STEPS_WAITING: usize = 64;

/// What the daemon says of a turn of the ask box's agent that it ran, and
/// that ended without saying how.
const UNTALKED: &str = "the turn ended with no answer, as the daemon's runner of it failed";

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
        let (at, to) = (named("at", &at)?, named("to", &to)?);
        if let Some(mut daemon) = self.bridged(on)? {
            let (at, to) = (at.to_string(), to.to_string());
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
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
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
        let seats: Vec<SessionName> =
            seats.iter().map(|seat| named("seats", seat)).collect::<Result<_, _>>()?;
        if seats.is_empty() {
            return Err(Status::invalid_argument("seats: the client was in none"));
        }
        if let Some(mut daemon) = self.bridged(on)? {
            let seats = seats.iter().map(SessionName::to_string).collect();
            return daemon.refuge(RefugeRequest { on: None, seats }).await;
        }
        let sockets = self.sockets.clone();
        let found = task::spawn_blocking(move || {
            // No session here is one luneta's terminal is a pane of: that
            // luneta's client is on another machine.
            moves::refuge(&sockets, &seats, None, |session| clients_of(session).ok())
        });
        let refuge = |moves::Refuge { session, before }| api::Refuge {
            session: session.to_string(),
            before,
        };
        let ended = match found.await.map_err(|error| unfinished(&error))? {
            moves::Ended::Kept => refuge_response::Ended::Kept(Kept {}),
            moves::Ended::Went(found) => {
                refuge_response::Ended::Went(Went { refuge: found.map(refuge) })
            },
        };
        Ok(Response::new(RefugeResponse { ended: Some(ended) }))
    }

    async fn open(&self, request: Request<OpenRequest>) -> Result<Response<OpenResponse>, Status> {
        let (on, open) = asked_open(request.into_inner())?;
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.open(open.asked(None)).await;
        }
        let opened = task::spawn_blocking(move || opens::opening(&open)).await;
        let opened = opened.map_err(|error| unfinished(&error))?;
        let opens::Opened { terminal, notes } = opened.map_err(Status::failed_precondition)?;
        Ok(Response::new(OpenResponse { terminal, notes }))
    }

    async fn list_skills(
        &self,
        request: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        let ListSkillsRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_skills(ListSkillsRequest { on: None, dir: dir.into() }).await;
        }
        let found = task::spawn_blocking(move || skills::found_in(dir.as_str())).await;
        let found = found.map_err(|error| unfinished(&error))?;
        let skills = found.map_err(Status::failed_precondition)?;
        let skill = |skills::Skill { name, description }| {
            Skill { name: name.to_string(), description }
        };
        Ok(Response::new(ListSkillsResponse { skills: skills.into_iter().map(skill).collect() }))
    }

    async fn keep_printed(
        &self,
        request: Request<KeepPrintedRequest>,
    ) -> Result<Response<KeepPrintedResponse>, Status> {
        let KeepPrintedRequest { on, session, pane, typed } = request.into_inner();
        let session = named("session", &session)?;
        let typed = Said::new(typed).ok_or_else(|| Status::invalid_argument("typed: it is empty"))?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, typed) = (session.to_string(), typed.into());
            return daemon.keep_printed(KeepPrintedRequest { on: None, session, pane, typed }).await;
        }
        let kept = task::spawn_blocking(move || printed::kept_after(&session, pane, &typed)).await;
        let kept = kept.map_err(|error| unfinished(&error))?;
        let kept = kept.map_err(Status::failed_precondition)?;
        let file = |KeptFile { file, lines, left_out }| PrintedFile {
            path: file,
            lines: lines as u64,
            left_out: left_out as u64,
        };
        let printed = match kept {
            Printed::After(kept) => keep_printed_response::Printed::After(file(kept)),
            Printed::Nothing => keep_printed_response::Printed::Nothing(Nothing {}),
            Printed::Unsure(kept) => keep_printed_response::Printed::Unsure(file(kept)),
            Printed::Top(kept) => keep_printed_response::Printed::Top(Top { file: kept.map(file) }),
        };
        Ok(Response::new(KeepPrintedResponse { printed: Some(printed) }))
    }

    async fn name_session(
        &self,
        request: Request<NameSessionRequest>,
    ) -> Result<Response<NameSessionResponse>, Status> {
        let NameSessionRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.name_session(NameSessionRequest { on: None, dir: dir.into() }).await;
        }
        let named = task::spawn_blocking(move || names::free_for(dir.as_str())).await;
        let named = named.map_err(|error| unfinished(&error))?;
        let (name, since) = named.map_err(Status::failed_precondition)?;
        let named_at = Some(since.into());
        Ok(Response::new(NameSessionResponse { name: name.to_string(), named_at }))
    }

    async fn list_dirs(
        &self,
        request: Request<ListDirsRequest>,
    ) -> Result<Response<ListDirsResponse>, Status> {
        let ListDirsRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_dirs(ListDirsRequest { on: None }).await;
        }
        let visited = task::spawn_blocking(zoxide::visited_free).await;
        let visited = visited.map_err(|error| unfinished(&error))?;
        let visited = visited.map_err(Status::failed_precondition)?;
        let dir = |zoxide::Dir { path, name }| Dir { path: path.into(), name: name.to_string() };
        Ok(Response::new(ListDirsResponse { dirs: visited.into_iter().map(dir).collect() }))
    }

    async fn list_contents(
        &self,
        request: Request<ListContentsRequest>,
    ) -> Result<Response<ListContentsResponse>, Status> {
        let ListContentsRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_contents(ListContentsRequest { on: None, dir: dir.into() }).await;
        }
        let listed = task::spawn_blocking(move || eza::listed(&dir)).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let listed = listed.map_err(Status::failed_precondition)?;
        Ok(Response::new(ListContentsResponse { listed }))
    }

    async fn dump_screen(
        &self,
        request: Request<DumpScreenRequest>,
    ) -> Result<Response<DumpScreenResponse>, Status> {
        let DumpScreenRequest { on, session, pane } = request.into_inner();
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
            return daemon.dump_screen(DumpScreenRequest { on: None, session, pane }).await;
        }
        let dumped = task::spawn_blocking(move || screens::dumped(&session, pane)).await;
        let dumped = dumped.map_err(|error| unfinished(&error))?;
        let (pane, screen) = dumped.map_err(Status::failed_precondition)?;
        Ok(Response::new(DumpScreenResponse { pane, screen }))
    }

    async fn start(
        &self,
        request: Request<StartRequest>,
    ) -> Result<Response<StartResponse>, Status> {
        let StartRequest { on, at, to, cwd } = request.into_inner();
        let (at, to) = (named("at", &at)?, named("to", &to)?);
        let cwd = cwd.map(|cwd| directory("cwd", cwd)).transpose()?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (at, to, cwd) = (at.to_string(), to.to_string(), cwd.map(String::from));
            return daemon.start(StartRequest { on: None, at, to, cwd }).await;
        }
        let start = move || moves::started(&at, &to, cwd.as_ref().map(Directory::as_str));
        let started = task::spawn_blocking(start).await;
        let started = started.map_err(|error| unfinished(&error))?;
        started.map_err(Status::failed_precondition)?;
        Ok(Response::new(StartResponse {}))
    }

    async fn closes(
        &self,
        request: Request<ClosesRequest>,
    ) -> Result<Response<ClosesResponse>, Status> {
        let ClosesRequest { on, session, pane } = request.into_inner();
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
            return daemon.closes(ClosesRequest { on: None, session, pane }).await;
        }
        let found = task::spawn_blocking(move || closes::closes_now(&session, pane)).await;
        let found = found.map_err(|error| unfinished(&error))?;
        let closes = match found.map_err(Status::failed_precondition)? {
            Closes::Pane => closes_response::Closes::Pane(Pane {}),
            Closes::Tab { id, panes } => closes_response::Closes::Tab(ClosedTab { id, panes }),
            Closes::Session => closes_response::Closes::Session(WholeSession {}),
        };
        Ok(Response::new(ClosesResponse { closes: Some(closes) }))
    }

    async fn close(
        &self,
        request: Request<CloseRequest>,
    ) -> Result<Response<CloseResponse>, Status> {
        let CloseRequest { on, session, closes } = request.into_inner();
        let session = named("session", &session)?;
        let close = match closes {
            Some(close_request::Closes::Pane(pane)) => Close::Pane(pane),
            Some(close_request::Closes::Tab(ClosedTab { panes, .. })) if panes.is_empty() => {
                return Err(Status::invalid_argument("closes: a tab of no terminal"));
            },
            Some(close_request::Closes::Tab(ClosedTab { id, panes })) => Close::Tab { id, panes },
            None => return Err(Status::invalid_argument("closes: neither a pane nor a tab")),
        };
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.close(close.asked(None, &session)).await;
        }
        let closed = task::spawn_blocking(move || closes::closed(&session, &close)).await;
        let closed = closed.map_err(|error| unfinished(&error))?;
        closed.map_err(Status::failed_precondition)?;
        Ok(Response::new(CloseResponse {}))
    }

    async fn kill(&self, request: Request<KillRequest>) -> Result<Response<KillResponse>, Status> {
        let KillRequest { on, session, started_at } = request.into_inner();
        let session = named("session", &session)?;
        let started = started_at.and_then(|at| SystemTime::try_from(at).ok()).ok_or_else(|| {
            Status::invalid_argument("started_at: it says no time the session started at")
        })?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, started_at) = (session.to_string(), Some(started.into()));
            return daemon.kill(KillRequest { on: None, session, started_at }).await;
        }
        let killed = task::spawn_blocking(move || closes::killed(&session, started)).await;
        let killed = killed.map_err(|error| unfinished(&error))?;
        killed.map_err(Status::failed_precondition)?;
        Ok(Response::new(KillResponse {}))
    }

    async fn list_exited(
        &self,
        request: Request<ListExitedRequest>,
    ) -> Result<Response<ListExitedResponse>, Status> {
        let ListExitedRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_exited(ListExitedRequest { on: None }).await;
        }
        let listed = task::spawn_blocking(ended::exited).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let listed = listed.map_err(Status::failed_precondition)?;
        // An age past what a Duration holds, some 292 billion years, is one
        // zellij said of no session, as a line that does not read.
        let exited = |ended::Exited { name, ago }| {
            Some(Exited { name: name.to_string(), ago: Some(ago.try_into().ok()?) })
        };
        let exited = listed.into_iter().filter_map(exited).collect();
        Ok(Response::new(ListExitedResponse { exited }))
    }

    async fn delete_exited(
        &self,
        request: Request<DeleteExitedRequest>,
    ) -> Result<Response<DeleteExitedResponse>, Status> {
        let DeleteExitedRequest { on, session } = request.into_inner();
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
            return daemon.delete_exited(DeleteExitedRequest { on: None, session }).await;
        }
        let deleted = task::spawn_blocking(move || closes::deleted(&session)).await;
        let deleted = deleted.map_err(|error| unfinished(&error))?;
        deleted.map_err(Status::failed_precondition)?;
        Ok(Response::new(DeleteExitedResponse {}))
    }

    async fn rename_session(
        &self,
        request: Request<RenameSessionRequest>,
    ) -> Result<Response<RenameSessionResponse>, Status> {
        let RenameSessionRequest { on, session, to } = request.into_inner();
        let session = named("session", &session)?;
        let to = SessionName::parse(&to)
            .map_err(|why| Status::invalid_argument(format!("to: {why}")))?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, to) = (session.to_string(), to.to_string());
            return daemon.rename_session(RenameSessionRequest { on: None, session, to }).await;
        }
        let renamed = task::spawn_blocking(move || renames::renamed(&session, &to)).await;
        let renamed = renamed.map_err(|error| unfinished(&error))?;
        renamed.map_err(Status::failed_precondition)?;
        Ok(Response::new(RenameSessionResponse {}))
    }

    async fn transcript_ends(
        &self,
        request: Request<TranscriptEndsRequest>,
    ) -> Result<Response<TranscriptEndsResponse>, Status> {
        let TranscriptEndsRequest { on, ids, share } = request.into_inner();
        let reading = Reading::of(&ids, share).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.transcript_ends(reading.asked(None)).await;
        }
        // With no folder to look in, it finds none.
        let Some(projects) = self.claude_folder.as_ref().map(|it| it.join("projects")) else {
            return Ok(Response::new(TranscriptEndsResponse::default()));
        };
        let read = task::spawn_blocking(move || reading.ends(&projects)).await;
        let read = read.map_err(|error| unfinished(&error))?;
        let ends = read.into_iter().map(|(id, end)| (id.as_str().to_owned(), end)).collect();
        Ok(Response::new(TranscriptEndsResponse { ends }))
    }

    type AskStream = Pin<Box<dyn Stream<Item = Result<AskResponse, Status>> + Send>>;

    async fn ask(&self, request: Request<AskRequest>) -> Result<Response<Self::AskStream>, Status> {
        let (on, asked) = Asked::of(request.into_inner()).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let steps = daemon.ask(asked.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(steps)));
        }
        // The server times out a question as it waits for the answer's
        // start, which the stream is, and never as it waits for the rest.
        let steps = talked(move |wanted, hear| asked.talk(wanted, hear));
        Ok(Response::new(Box::pin(steps)))
    }

    type PasteStream = Pin<Box<dyn Stream<Item = Result<PasteResponse, Status>> + Send>>;

    async fn paste(
        &self,
        request: Request<PasteRequest>,
    ) -> Result<Response<Self::PasteStream>, Status> {
        let (on, pasted) = Pasted::of(request.into_inner()).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let answer = daemon.paste(pasted.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let answer = late(move || pasted.ready().map(Ready::answer));
        Ok(Response::new(Box::pin(answer)))
    }
}

/// What `job` answers, from a thread of its own, as a stream of that answer
/// alone: one that comes as late as the job takes, as the server times out
/// a question as it waits for the answer's start, which the stream is. The
/// stream ends with why there is no answer where the job says so, or where
/// its thread panicked. A question that hangs up first does not stop it.
fn late<T, J>(job: J) -> ReceiverStream<Result<T, Status>>
where
    T: Send + 'static,
    J: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (answer, answered) = tokio::sync::mpsc::channel(1);
    task::spawn(async move {
        let done = match task::spawn_blocking(job).await {
            Ok(done) => done.map_err(Status::failed_precondition),
            Err(error) => Err(unfinished(&error)),
        };
        // The question hung up.
        let _ = answer.send(done).await;
    });
    ReceiverStream::new(answered)
}

/// What `talk` hears of a turn of the ask box's agent, a step at a time, as
/// the daemon hands it on, from a thread of its own, as a turn takes as long
/// as `claude` does. The stream ends as the turn does: well, or with why
/// not, or with the daemon saying that its runner of the turn failed. A
/// question that hangs up gives up the turn, as it is no longer `wanted`.
fn talked<T>(talk: T) -> ReceiverStream<Result<AskResponse, Status>>
where
    T: FnOnce(&dyn Fn() -> bool, &dyn Fn(Step)) -> Option<Result<(), String>> + Send + 'static,
{
    let (heard, steps) = tokio::sync::mpsc::channel(STEPS_WAITING);
    let failed = heard.clone();
    let talking = task::spawn_blocking(move || {
        // The question hung up, which gives the turn up.
        let wanted = || !heard.is_closed();
        let hear = |step: Step| {
            let _ = heard.blocking_send(Ok(step.answer()));
        };
        if let Some(Err(why)) = talk(&wanted, &hear) {
            let _ = heard.blocking_send(Err(Status::failed_precondition(why)));
        }
    });
    // A turn that panicked dropped what it heard with, which alone would end
    // the stream as a turn that ended well.
    task::spawn(async move {
        if talking.await.is_err() {
            let _ = failed.send(Err(Status::internal(UNTALKED))).await;
        }
    });
    ReceiverStream::new(steps)
}

/// The clients of `session` here, as zellij lists them now, or why not.
fn clients_of(session: &SessionName) -> Result<Vec<api::Client>, String> {
    let listed = clients::list(std::process::Command::new("zellij"), session.as_str())?;
    Ok(listed.into_iter().map(clients::Client::to_api).collect())
}

/// `name`, of the question's `field`, as a session's name zellij takes, or
/// why the question is invalid: it is not one.
fn named(field: &str, name: &str) -> Result<SessionName, Status> {
    SessionName::running(name).map_err(|why| Status::invalid_argument(format!("{field}: {why}")))
}

/// `dir`, of the question's `field`, as a directory by its absolute path, or
/// why the question is invalid: one by a path relative to the daemon's own
/// directory, or by none, would be read as that one, which is no one's.
fn directory(field: &str, dir: String) -> Result<Directory, Status> {
    Directory::parse(dir).map_err(|why| Status::invalid_argument(format!("{field}: {why}")))
}

/// What `request` has zellij open, on the machine it names, or why the
/// question is invalid: it names a session zellij would not take, or a
/// directory by no absolute path, or it opens neither a pane nor a tab, or a
/// tab by an empty name, where an absent one has zellij number it, or alone
/// since no time there is.
fn asked_open(request: OpenRequest) -> Result<(Option<api::On>, Open), Status> {
    let OpenRequest { on, session, cwd, command, opens } = request;
    let (session, cwd) = (named("session", &session)?, String::from(directory("cwd", cwd)?));
    let open = match opens {
        None => return Err(Status::invalid_argument("opens: neither a pane nor a tab")),
        Some(Opens::Beside(beside)) => Open::Pane { session, beside, cwd, command },
        Some(Opens::Tab(open_request::Tab { name: Some(name), .. })) if name.is_empty() => {
            let why = "opens: a tab's name is empty, where an absent one has zellij number it";
            return Err(Status::invalid_argument(why));
        },
        Some(Opens::Tab(open_request::Tab { name, alone_since })) => {
            let alone_since = alone_since.map(SystemTime::try_from).transpose().map_err(|_| {
                Status::invalid_argument("opens: alone_since says no time a session started at")
            })?;
            Open::Tab { session, tab: Tab { cwd, name, command }, alone_since }
        },
    };
    Ok((on, open))
}

/// Why what the daemon asked of zellij, or of the finder of skills, or what
/// it read of Claude Code's transcripts, or where a pasted link's key goes,
/// did not answer: the thread it was asked on panicked.
fn unfinished(error: &task::JoinError) -> Status {
    Status::internal(format!("lunetad lost its answer: {error}"))
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

    use tokio_stream::StreamExt as _;
    use tonic::Code;

    use super::{talked, Alone, UNTALKED};
    use crate::asks::Step;

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

    /// A turn whose runner failed ends after the steps it heard, with the
    /// daemon saying so: not as a turn that ended well.
    #[test]
    fn a_turn_whose_runner_failed_does_not_end_well() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let ended: Vec<_> = runtime.block_on(async {
            let steps = talked(|_, hear| {
                hear(Step::Said("Looking.".to_owned()));
                panic!("the runner of the turn fails");
            });
            steps.collect().await
        });
        let [Ok(heard), Err(failed)] = ended.as_slice() else {
            panic!("{ended:?}");
        };
        assert_eq!(*heard, Step::Said("Looking.".to_owned()).answer());
        assert_eq!((failed.code(), failed.message()), (Code::Internal, UNTALKED));
    }
}
