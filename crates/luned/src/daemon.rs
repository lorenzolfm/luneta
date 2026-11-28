use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Write as _};
use std::mem;
use std::num::NonZeroU32;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use lune_domain::agent::Harness;
use lune_domain::flows::FlowEntry;
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{Dir, ProjectId, WorktreeId};
use lune_domain::projects::{Holders, Made, Opens, OwnedFolder, folder_has_one_worktree};
use lune_protocol::said::Said;
use lune_protocol::socket::{self, Bound, Found};
use lune_protocol::watch::{HostVersion, LocalVersion};
use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::runtime::Handle;
use tokio::sync::{OwnedSemaphorePermit, oneshot};
use tokio::task;
use tokio::time::{timeout, timeout_at};
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Code, Request, Response, Status, Streaming};
use tower::layer::layer_fn;

use crate::answers::{self, Answer, Runs, Unanswered};
use crate::api::change_projects_request::{self, AddWorktree};
use crate::api::change_projects_response;
use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::keep_checks_response::{self, Failed as FailedCheck};
use crate::api::keep_image_request::{Image, Part};
use crate::api::list_repositories_response::Repository;
use crate::api::list_skills_response::Skill;
use crate::api::make_worktree_response::Landed;
use crate::api::{
    self, AnswerRequest, AskRequest, AskResponse, ChangeProjectsRequest, ChangeProjectsResponse,
    ControlFlowRequest, FileIssueRequest, FileIssueResponse, FlowStateResponse, FramesRequest,
    InfoResponse, IssueRepositoriesRequest, IssueRepositoriesResponse, KeepChecksRequest,
    KeepChecksResponse, KeepImageRequest, KeepImageResponse, KeepPrintedRequest,
    KeepPrintedResponse, ListAgentsResponse, ListContentsRequest, ListContentsResponse,
    ListFlowsRequest, ListFlowsResponse, ListRepositoriesRequest, ListRepositoriesResponse,
    ListResumeRequest, ListResumeResponse, ListSkillsRequest, ListSkillsResponse, ListWorkRequest,
    ListWorkResponse, MakeWorktreeRequest, MakeWorktreeResponse, PasteRequest, PasteResponse,
    PruneRequest, PruneResponse, PrunesRequest, PrunesResponse, ReadDiffRequest, ReadDiffResponse,
    RebaseRequest, RebaseResponse, ReloadResumeRequest, ReloadResumeResponse, SayRequest,
    ShowRequest, ShowResponse, StartFlowRequest, TranscriptEndsRequest, TranscriptEndsResponse,
    WaitingRequest, WaitingResponse,
};
use crate::asks::{Asked, Step};
use crate::checks::{Failing, Log};
use crate::core::{self, AllSnapshot, Command, LocalSnapshot, Wakes, WatchDecision};
use crate::directory::Directory;
use crate::folder::{self, LOCK, Refusal, SOCKET};
use crate::images::{self, Keeping, Size};
use crate::issues::{self, Filing, Remote, Unfiled};
use crate::layouts::{self, Root, Unrooted};
use crate::links::{self, Branch, Of, Pasted, Unready};
use crate::mux::Failure;
use crate::mux::own::api::{FromViewer, ToViewer};
use crate::mux::own::viewer;
use crate::panics::Hooked;
use crate::printed::Kept as KeptFile;
use crate::projects::{Changed, Lock, Opened, Store};
use crate::prunes::{self, Lunes, Occupied};
use crate::rebases;
use crate::remote::{Bridges, Bridging, Destination, Standing};
use crate::sayings::ToSay;
use crate::statuses::ReportedAgent;
use crate::transcripts::{self, Reading, SessionId};
use crate::{
    agy, changes, checks, claude, client, devices, eza, fetches, flows, git, github, keeps, log,
    owns, pi, printed, program, projects, release, remote, repositories, skills, state, tailnet,
    topics, watch, work,
};

mod over_tailnet;

use over_tailnet::OverTailnet;

pub const LISTENING: lune_protocol::ReadyPrefix =
    lune_protocol::ReadyPrefix::try_new("luned: listening on ")
        .expect("luned readiness prefix is valid");

pub const SWEEP: Duration = Duration::from_secs(2);

const GIT_STATUS_SWEEP: Duration = Duration::from_secs(5);

const FETCH_SWEEP: Duration = Duration::from_mins(5);

const PULL_REQUEST_SWEEP: Duration = Duration::from_mins(2);

const PENDING_CHECKS_SWEEP: Duration = Duration::from_secs(15);

const KEEPS_SWEEP: Duration = Duration::from_hours(1);

pub const WATCH: Duration = Duration::from_secs(4);

const _: () = assert!(WATCH.as_millis() < client::ANSWER.waits().as_millis());

#[must_use]
pub fn parked_watch(version: HostVersion) -> String {
    format!("a watch waits for a change past {version}")
}

pub(crate) const QUESTIONS_MAX: u32 = 16;

pub const ANSWERING_MAX: Duration = Duration::from_mins(2);

const BRIDGED_ASKS_MIN: usize = 4;

const _: () = assert!(remote::HELD_MAX + BRIDGED_ASKS_MIN == QUESTIONS_MAX as usize);

const RUNS_IN_ON: &str = "runs_in names a terminal of this machine, and on names another";

const STEPS_WAITING: usize = 64;

const UNTALKED: &str = "the turn ended with no answer, as the daemon's runner of it failed";

const IN_FLIGHT: u32 = 128 * 1024;

pub const ALONE: Duration = Duration::from_secs(30);

const _: () = assert!(
    ALONE.as_secs() > SWEEP.as_secs(),
    "luned is alone over more than one look"
);

const DRAIN: Duration = client::ANSWER.waits();

pub struct Claim {
    folder: PathBuf,
    socket: PathBuf,
    _lock: File,
}

impl Claim {
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
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(Refusal::Running(folder.to_owned())),
            Err(TryLockError::Error(error)) => return Err(Refusal::Io(path, error)),
        }
        let socket = folder.join(SOCKET);
        match fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(Refusal::Io(socket, error)),
        }
        Ok(Claim {
            folder: folder.to_owned(),
            socket,
            _lock: lock,
        })
    }

    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    fn bind(&self) -> io::Result<(UnixListener, Bound)> {
        let (listener, bound) = socket::bind(&self.socket)?;
        listener.set_nonblocking(true)?;
        Ok((UnixListener::from_std(listener)?, bound))
    }
}

pub async fn serve(
    _hooked: Hooked,
    claim: Claim,
    projects: Result<Lock, String>,
    unkept: Option<String>,
    detached: bool,
) -> io::Result<()> {
    let (listener, bound) = claim.bind()?;
    let devices_file = if projects.is_ok() {
        let devices_file = tailnet::in_own_home()
            .and_then(|()| state::path(devices::FILE).map_err(|unset| unset.to_string()));
        if let Err(why) = &devices_file {
            log::say(&format!("{why}, so luned stays off the tailnet"));
        }
        devices_file.ok()
    } else {
        None
    };
    let started = SystemTime::now();
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let (wake_github, github_woken) = mpsc::sync_channel(1);
    let (wake_projects, projects_woken) = mpsc::sync_channel(1);
    let github = wake_github.clone();
    let wakes = Wakes {
        git: wake_git.clone(),
        github,
        projects: wake_projects.clone(),
    };
    let core = core::start(wakes)?;
    let store = Store::open(projects, core.clone())?;
    if let Some(why) = store.unkept() {
        log::say(why);
    }
    let (wake_owns, owns_woken) = mpsc::sync_channel(1);
    let layouts = layouts::start(core.clone(), store, wake_owns.clone(), SWEEP)?;
    projects::follow(
        core.clone(),
        layouts.clone(),
        wake_projects,
        projects_woken,
        SWEEP,
    )?;
    let (end, ending) = oneshot::channel();
    let claude_folder = claude::folder();
    let agy_folder = agy::folder();
    let watchers = Arc::new(Watchers::default());
    let owns = Arc::new(AtomicBool::new(false));
    follow(
        &core,
        &claim.folder,
        claude_folder.clone(),
        agy_folder.clone(),
    )?;
    tokio::spawn(stand(watchers.clone(), owns.clone(), bound.clone(), end));
    owns::follow(
        core.clone(),
        layouts.clone(),
        (wake_owns, owns_woken),
        owns,
        SWEEP,
    )?;
    git::follow(core.clone(), wake_git, git_woken, GIT_STATUS_SWEEP)?;
    fetches::follow(core.clone(), FETCH_SWEEP)?;
    let rests = github::Rests {
        pending: PENDING_CHECKS_SWEEP,
        settled: PULL_REQUEST_SWEEP,
    };
    let wake_github_flows = wake_github.clone();
    github::follow(core.clone(), wake_github, github_woken, rests)?;
    let flows = flows::start(core.clone(), layouts.clone(), wake_github_flows)?;
    core.send(Command::AttachFlows(flows.clone()))
        .map_err(|_| io::Error::other("core stopped before attaching flows"))?;
    layouts
        .send(layouts::Command::AttachFlows(flows.clone()))
        .map_err(|_| io::Error::other("layouts stopped before attaching flows"))?;
    let bridges = Arc::new(Bridges::default());
    let folder = claim.folder.clone();
    let standing = watchers.standing.subscribe();
    remote::follow(core.clone(), standing, SWEEP, folder, Arc::clone(&bridges))?;
    if let Some(claude) = &claude_folder {
        let projects = claude.join("projects");
        topics::follow(
            core.clone(),
            watchers.shown.subscribe(),
            projects,
            started,
            SWEEP,
        )?;
    }
    tokio::spawn(sweep_keeps(claim.folder.clone()));
    let browser_bound = crate::terminal_socket::bind(&claim.folder);
    log::say(&format!("listening on {}", claim.socket.display()));
    if let Some(why) = unkept {
        log::say(&format!("what goes wrong is kept in no log: {why}"));
    }
    let listings = Arc::new(work::Listings::default());
    let host = Arc::new(Host {
        started,
        claude_folder,
        agy_folder,
        core,
        watchers,
        bridges,
        layouts,
        flows,
        listings,
    });
    let (begun, beginning) = oneshot::channel();
    let (closing, closed) = tokio::sync::watch::channel(false);
    let browser_socket = browser_bound.map(|bound| {
        tokio::spawn(crate::terminal_socket::serve(
            bound,
            host.layouts.clone(),
            closed.clone(),
        ))
    });
    let ended = async move {
        if ending.await.is_err() {
            log::say("luned no longer follows what keeps it, and so it ends");
        }
        if let Err(error) = bound.unbind() {
            log::say(&format!("{}: {error}", bound.socket().display()));
        }
        let _ = closing.send(true);
        let _ = begun.send(());
    };
    let router = |server: Server| {
        server
            .max_concurrent_streams(QUESTIONS_MAX)
            .initial_stream_window_size(IN_FLIGHT)
            .layer(layer_fn(|routes| {
                client::Within::new(routes, ANSWERING_MAX, client::unanswered)
            }))
    };
    let tailnet_served = if let Some(devices_file) = devices_file {
        let mut closed = closed;
        let closed = async move {
            let _ = closed.wait_for(|closed| *closed).await;
        };
        let incoming = tailnet::incoming(SWEEP, devices_file);
        let served = router(tailnet::server())
            .add_service(DaemonServer::new(OverTailnet(Arc::clone(&host))))
            .serve_with_incoming_shutdown(incoming, closed);
        Some(tokio::spawn(async move {
            if let Err(error) = served.await {
                log::say(&format!("the tailnet is no longer served: {error}"));
            }
        }))
    } else {
        None
    };
    let served = tokio::spawn(
        router(Server::builder())
            .add_service(DaemonServer::from_arc(host))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), ended),
    );
    if detached {
        announce_ready(&claim.socket);
    }
    let _ = beginning.await;
    let drained = async move {
        let tailnet_ended = match tailnet_served {
            Some(tailnet_served) => tailnet_served.await,
            None => Ok(()),
        };
        if let Err(error) = tailnet_ended {
            log::say(&format!("the tailnet's server did not end: {error}"));
        }
        if let Some(browser_socket) = browser_socket
            && let Err(error) = browser_socket.await
        {
            log::say(&format!("the terminal browser socket did not end: {error}"));
        }
        served.await
    };
    match timeout(DRAIN, drained).await {
        Ok(Ok(served)) => served.map_err(io::Error::other),
        Ok(Err(error)) => Err(io::Error::other(error)),
        Err(_) => {
            let secs = DRAIN.as_secs();
            log::say(&format!("clients still connected after {secs}s are left"));
            Ok(())
        }
    }
}

fn announce_ready(socket: &Path) {
    assert!(socket.is_absolute(), "announced socket path is absolute");
    {
        let mut out = io::stdout().lock();
        let written = writeln!(out, "{LISTENING}{}", socket.display()).and_then(|()| out.flush());
        if let Err(error) = written
            && error.kind() != ErrorKind::BrokenPipe
        {
            log::say(&format!("stdout did not take its listening line: {error}"));
        }
    }
    match OpenOptions::new().write(true).open("/dev/null") {
        Ok(null) => {
            if let Err(error) = rustix::stdio::dup2_stdout(&null) {
                log::say(&format!("stdout did not turn to /dev/null: {error}"));
            }
        }
        Err(error) => log::say(&format!("/dev/null did not open for stdout: {error}")),
    }
}

async fn sweep_keeps(folder: PathBuf) {
    let mut sweeps = tokio::time::interval(KEEPS_SWEEP);
    loop {
        sweeps.tick().await;
        let folder = folder.clone();
        let swept = task::spawn_blocking(move || {
            let now = SystemTime::now();
            keeps::sweep_kind(&folder, images::FOLDER, now);
            keeps::sweep_kind(&folder, printed::FOLDER, now);
            keeps::sweep_kind(&folder, checks::FOLDER, now);
        });
        let _ = swept.await;
    }
}

#[derive(Default)]
struct Alone {
    since: Option<Instant>,
}

impl Alone {
    fn saw(&mut self, alone: bool, now: Instant) -> Duration {
        if !alone {
            self.since = None;
            return Duration::ZERO;
        }
        let since = *self.since.get_or_insert(now);
        now.saturating_duration_since(since)
    }
}

fn follow(
    core: &SyncSender<Command>,
    folder: &Path,
    claude_folder: Option<PathBuf>,
    agy_folder: Option<PathBuf>,
) -> io::Result<()> {
    match claude_folder {
        Some(folder) => {
            let mut files = claude::SessionFiles::new(&folder);
            let dir = files.dir().to_owned();
            let core = core.clone();
            let read = move || files.scan();
            let tell = move |found| core.send(Command::Agents(found)).is_ok();
            let any = |_: &Path| true;
            let mode = RecursiveMode::NonRecursive;
            watch::watch_ending(
                "claude",
                dir,
                mode,
                any,
                SWEEP,
                read,
                claude::Scan::pids,
                tell,
            )?;
        }
        None => log::say("neither CLAUDE_CONFIG_DIR nor HOME is set, to find agents in"),
    }
    let mut files = pi::AgentFiles::new(folder, pi::folder());
    let dir = files.dir().to_owned();
    let read = move || files.scan();
    let tell = {
        let core = core.clone();
        move |found| core.send(Command::PiAgents(found)).is_ok()
    };
    let any = |_: &Path| true;
    let mode = RecursiveMode::NonRecursive;
    let pids = |agents: &Vec<ReportedAgent>| {
        agents
            .iter()
            .map(|agent| agent.seen().id.pid().get())
            .collect()
    };
    watch::watch_ending("pi", dir, mode, any, SWEEP, read, pids, tell)?;
    let mut files = agy::AgentFiles::new(folder, agy_folder);
    let dir = files.dir().to_owned();
    let read = move || files.scan();
    let tell = {
        let core = core.clone();
        move |found| core.send(Command::AgyAgents(found)).is_ok()
    };
    watch::watch_ending("agy", dir, mode, any, SWEEP, read, pids, tell)
}

async fn stand(
    watchers: Arc<Watchers>,
    owns: Arc<AtomicBool>,
    bound: Bound,
    end: oneshot::Sender<()>,
) {
    let mut sweeps = tokio::time::interval(SWEEP);
    let mut alone = Alone::default();
    let mut unlooked_said = false;
    loop {
        sweeps.tick().await;
        match bound.found() {
            Ok(Found::Ours) => unlooked_said = false,
            Ok(Found::Lost(unreached)) => {
                log::say(&format!("{unreached}: luned ends"));
                let _ = end.send(());
                return;
            }
            Err(error) => {
                if !unlooked_said {
                    let socket = bound.socket().display();
                    log::say(&format!(
                        "{socket} is not looked at, so luned runs on: {error}"
                    ));
                }
                unlooked_said = true;
            }
        }
        let (clients, daemons) = (watchers.clients.looked(), watchers.daemons.looked());
        let (window, far_window) = (watchers.windows.looked(), watchers.far_windows.looked());
        let terminals = owns.load(Ordering::Relaxed);
        let standing = Standing::of(terminals || clients || window, window);
        watchers
            .standing
            .send_if_modified(|was| mem::replace(was, standing) != standing);
        let shown = window || far_window;
        watchers
            .shown
            .send_if_modified(|was| mem::replace(was, shown) != shown);
        if alone.saw(standing == Standing::Idle && !daemons, Instant::now()) >= ALONE {
            let secs = ALONE.as_secs();
            let none = format!("lune-mux has had no terminal for {secs}s, nor luned a watcher");
            log::say(&format!("{none}, and so luned ends"));
            let _ = end.send(());
            return;
        }
    }
}

#[derive(Default)]
struct Watchers {
    clients: Watching,
    daemons: Watching,
    standing: tokio::sync::watch::Sender<Standing>,
    windows: Watching,
    far_windows: Watching,
    shown: tokio::sync::watch::Sender<bool>,
}

impl Watchers {
    fn waits(&self, waiter: Waiter) -> Option<(Waits<'_>, Option<Waits<'_>>)> {
        let (watching, showing, standing) = match waiter {
            Waiter::Daemon(Standing::Idle) => return None,
            Waiter::Daemon(Standing::Kept) => (&self.daemons, None, None),
            Waiter::Daemon(Standing::Window) => (&self.daemons, Some(&self.far_windows), None),
            Waiter::Client(ClientWaiter::Client) => (&self.clients, None, Some(Standing::Kept)),
            Waiter::Client(ClientWaiter::Window) => {
                (&self.clients, Some(&self.windows), Some(Standing::Window))
            }
        };
        let waits = (Waits::on(watching), showing.map(Waits::on));
        if let Some(standing) = standing {
            self.standing.send_if_modified(|was| {
                let before = mem::replace(was, (*was).max(standing));
                before != *was
            });
        }
        if showing.is_some() {
            self.shown.send_if_modified(|was| !mem::replace(was, true));
        }
        Some(waits)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClientWaiter {
    Client,
    Window,
}

impl ClientWaiter {
    fn parse(said: i32) -> Result<Self, Status> {
        match api::ClientWaiter::try_from(said) {
            Ok(api::ClientWaiter::Client) => Ok(ClientWaiter::Client),
            Ok(api::ClientWaiter::Window) => Ok(ClientWaiter::Window),
            Ok(api::ClientWaiter::Unspecified) | Err(_) => {
                Err(Status::invalid_argument("waiter: unknown watch waiter"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiter {
    Client(ClientWaiter),
    Daemon(Standing),
}

impl Waiter {
    fn parse_legacy(request: &api::WatchRequest) -> Result<Self, Status> {
        match (request.kept, request.shown) {
            (None, false) => Ok(Waiter::Client(ClientWaiter::Client)),
            (None, true) => Ok(Waiter::Client(ClientWaiter::Window)),
            (Some(false), false) => Ok(Waiter::Daemon(Standing::Idle)),
            (Some(false), true) => Err(Status::invalid_argument(
                "a daemon cannot be both idle and showing a window",
            )),
            (Some(true), false) => Ok(Waiter::Daemon(Standing::Kept)),
            (Some(true), true) => Ok(Waiter::Daemon(Standing::Window)),
        }
    }
}

#[derive(Default)]
struct Watching {
    waiting: AtomicUsize,
    asked: AtomicBool,
}

impl Watching {
    fn looked(&self) -> bool {
        let asked = self.asked.swap(false, Ordering::Relaxed);
        asked || self.waiting.load(Ordering::Relaxed) > 0
    }
}

struct Waits<'a>(&'a Watching);

impl<'a> Waits<'a> {
    fn on(watching: &'a Watching) -> Self {
        watching.asked.store(true, Ordering::Relaxed);
        watching.waiting.fetch_add(1, Ordering::Relaxed);
        Waits(watching)
    }
}

impl Drop for Waits<'_> {
    fn drop(&mut self) {
        self.0.waiting.fetch_sub(1, Ordering::Relaxed);
    }
}

struct Host {
    started: SystemTime,
    claude_folder: Option<PathBuf>,
    agy_folder: Option<PathBuf>,
    core: SyncSender<Command>,
    watchers: Arc<Watchers>,
    bridges: Arc<Bridges>,
    layouts: SyncSender<layouts::Command>,
    flows: SyncSender<flows::Command>,
    listings: Arc<work::Listings>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: release::VERSION.to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
            claude_folder: self
                .claude_folder
                .as_ref()
                .map(|it| it.as_os_str().as_bytes().to_vec()),
            built: release::BUILT,
            agy_folder: self
                .agy_folder
                .as_ref()
                .map(|it| it.as_os_str().as_bytes().to_vec()),
        }))
    }

    async fn list_agents(&self, _: Request<()>) -> Result<Response<ListAgentsResponse>, Status> {
        self.ask(Command::List).await.map(Response::new)
    }

    async fn watch(
        &self,
        request: Request<api::WatchRequest>,
    ) -> Result<Response<api::Host>, Status> {
        let request = request.into_inner();
        let waiter = Waiter::parse_legacy(&request)?;
        let host = match waiter {
            Waiter::Client(waiter) => {
                let after = request
                    .after
                    .map(HostVersion::of_api)
                    .transpose()
                    .map_err(|why| Status::invalid_argument(format!("after: {why}")))?;
                self.watch_all(waiter, after)
                    .await?
                    .into_snapshot()
                    .to_api()
            }
            Waiter::Daemon(standing) => {
                let after = request
                    .after
                    .map(LocalVersion::of_legacy_api)
                    .transpose()
                    .map_err(|why| Status::invalid_argument(format!("after: {why}")))?;
                self.watch_local(standing, after)
                    .await?
                    .into_snapshot()
                    .to_legacy_api()
            }
        };
        Ok(Response::new(host))
    }

    async fn watch_changes(
        &self,
        request: Request<api::WatchChangesRequest>,
    ) -> Result<Response<api::WatchResponse>, Status> {
        let request = request.into_inner();
        let waiter = ClientWaiter::parse(request.waiter)?;
        let after = request
            .after
            .map(HostVersion::of_api)
            .transpose()
            .map_err(|why| Status::invalid_argument(format!("after: {why}")))?;
        let response = match self.watch_all(waiter, after).await? {
            WatchAnswer::Changed(snapshot) => {
                api::WatchResponse::host(snapshot.version(), snapshot.to_api())
            }
            WatchAnswer::Unchanged(snapshot) => api::WatchResponse::unchanged(snapshot.version()),
        };
        Ok(Response::new(response))
    }

    async fn watch_local_changes(
        &self,
        request: Request<api::WatchLocalChangesRequest>,
    ) -> Result<Response<api::WatchLocalResponse>, Status> {
        let request = request.into_inner();
        let standing = Standing::parse_api(request.standing)
            .map_err(|why| Status::invalid_argument(format!("standing: {why}")))?;
        let after = request
            .after
            .map(LocalVersion::of_api)
            .transpose()
            .map_err(|why| Status::invalid_argument(format!("after: {why}")))?;
        let response = match self.watch_local(standing, after).await? {
            WatchAnswer::Changed(snapshot) => {
                api::WatchLocalResponse::host(snapshot.version(), snapshot.to_remote_api())
            }
            WatchAnswer::Unchanged(snapshot) => {
                api::WatchLocalResponse::unchanged(snapshot.version())
            }
        };
        Ok(Response::new(response))
    }

    async fn list_skills(
        &self,
        request: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        let ListSkillsRequest { on, dir, harness } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = ListSkillsRequest {
                on: None,
                dir: dir.into(),
                harness,
            };
            return daemon.list_skills(asked).await;
        }
        let harness = Harness::said(harness)
            .map_err(|why| Status::invalid_argument(format!("harness: {why}")))?;
        let found = task::spawn_blocking(move || skills::found_in(&dir, harness)).await;
        let found = found.map_err(|error| unfinished(&error))?;
        let skills = found.map_err(Status::failed_precondition)?;
        let skill = |skills::Skill {
                         name,
                         description,
                         runs,
                     }| Skill {
            name: name.to_string(),
            description,
            only_you: runs == skills::Runs::OnlyForYou,
        };
        Ok(Response::new(ListSkillsResponse {
            skills: skills.into_iter().map(skill).collect(),
        }))
    }

    type KeepChecksStream = Pin<Box<dyn Stream<Item = Result<KeepChecksResponse, Status>> + Send>>;

    async fn keep_checks(
        &self,
        request: Request<KeepChecksRequest>,
    ) -> Result<Response<Self::KeepChecksStream>, Status> {
        let KeepChecksRequest { on, url } = request.into_inner();
        let page = checks::PullPage::parse(&url)
            .map_err(|why| Status::invalid_argument(format!("url: {why}")))?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let answer = daemon
                .keep_checks(KeepChecksRequest { on: None, url })
                .await?;
            return Ok(Response::new(Box::pin(answer.into_inner())));
        }
        let answer = late(move || {
            let kept = checks::kept_failing(&page).map_err(Status::failed_precondition)?;
            Ok(kept_checks(kept))
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn keep_printed(
        &self,
        request: Request<KeepPrintedRequest>,
    ) -> Result<Response<KeepPrintedResponse>, Status> {
        let KeepPrintedRequest {
            on,
            terminal,
            typed,
        } = request.into_inner();
        let (terminal, _) = terminal_named(terminal)?;
        let typed = Said::new(typed).ok_or_else(|| Status::invalid_argument("typed: nothing"))?;
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .keep_printed(printed::asked(None, terminal, &typed))
                .await;
        }
        let kept = task::spawn_blocking(move || printed::kept_in(terminal, &typed)).await;
        let kept = kept.map_err(|error| unfinished(&error))?;
        Ok(Response::new(
            kept.map_err(Status::failed_precondition)?.to_api(),
        ))
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        let ListRepositoriesRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .list_repositories(ListRepositoriesRequest { on: None })
                .await;
        }
        let found = task::spawn_blocking(repositories::found).await;
        let repositories::Found {
            repositories,
            owned,
            unread,
        } = found.map_err(|error| unfinished(&error))?;
        let repository = |it: repositories::Repository| Repository {
            path: it.path().as_str().to_owned(),
        };
        let repositories = repositories.into_iter().map(repository).collect();
        let owned = owned
            .iter()
            .flat_map(OwnedFolder::folders)
            .map(|it| it.as_str().to_owned());
        let owned = owned.collect();
        Ok(Response::new(ListRepositoriesResponse {
            repositories,
            unread,
            owned,
        }))
    }

    async fn list_contents(
        &self,
        request: Request<ListContentsRequest>,
    ) -> Result<Response<ListContentsResponse>, Status> {
        let ListContentsRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .list_contents(ListContentsRequest {
                    on: None,
                    dir: dir.into(),
                })
                .await;
        }
        let listed = task::spawn_blocking(move || eza::listed(&dir)).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let listed = listed.map_err(Status::failed_precondition)?;
        Ok(Response::new(ListContentsResponse { listed }))
    }

    async fn waiting(
        &self,
        request: Request<WaitingRequest>,
    ) -> Result<Response<WaitingResponse>, Status> {
        let WaitingRequest { on, terminal } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.waiting(WaitingRequest { on: None, terminal }).await;
        }
        let (key, asked) = terminal_named(terminal)?;
        let runs = self.runs_in(&asked).await;
        let waiting = task::spawn_blocking(move || answers::waiting(key, &runs));
        let waiting = waiting.await.map_err(|error| unfinished(&error))?;
        waiting
            .map(Response::new)
            .map_err(|failure| unanswered(&Unanswered::Unreached(failure)))
    }

    async fn answer(&self, request: Request<AnswerRequest>) -> Result<Response<()>, Status> {
        let AnswerRequest {
            on,
            terminal,
            shown,
            answer,
        } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .answer(AnswerRequest {
                    on: None,
                    terminal,
                    shown,
                    answer,
                })
                .await;
        }
        let (key, asked) = terminal_named(terminal)?;
        let answer = Answer::of(shown, answer).map_err(Status::invalid_argument)?;
        let runs = self.runs_in(&asked).await;
        let answered = task::spawn_blocking(move || answers::answer(key, &runs, answer));
        let answered = answered.await.map_err(|error| unfinished(&error))?;
        answered
            .map(Response::new)
            .map_err(|refused| unanswered(&refused))
    }

    async fn transcript_ends(
        &self,
        request: Request<TranscriptEndsRequest>,
    ) -> Result<Response<TranscriptEndsResponse>, Status> {
        let TranscriptEndsRequest { on, ids, share } = request.into_inner();
        let reading = Reading::of(&ids, share).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.transcript_ends(reading.asked(None)).await;
        }
        let folders = transcripts::Folders {
            claude: self.claude_folder.clone(),
            agy: self.agy_folder.clone(),
        };
        if folders.claude.is_none() && folders.agy.is_none() {
            return Ok(Response::new(TranscriptEndsResponse::default()));
        }
        let read = task::spawn_blocking(move || reading.ends(&folders)).await;
        let read = read.map_err(|error| unfinished(&error))?;
        let mut ends = HashMap::with_capacity(read.len());
        let mut typed_ends = HashMap::with_capacity(read.len());
        for (id, end) in read {
            let key = id.as_str().to_owned();
            typed_ends.insert(key.clone(), end.to_api());
            ends.insert(key, end.lines().to_owned());
        }
        Ok(Response::new(TranscriptEndsResponse { ends, typed_ends }))
    }

    type AskStream = Pin<Box<dyn Stream<Item = Result<AskResponse, Status>> + Send>>;

    async fn ask(&self, request: Request<AskRequest>) -> Result<Response<Self::AskStream>, Status> {
        let (on, asked) = Asked::of(request.into_inner()).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let steps = daemon.ask(asked.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(steps)));
        }
        let steps = talked(move |wanted, hear| asked.talk(wanted, hear));
        Ok(Response::new(Box::pin(steps)))
    }

    type PasteStream = Pin<Box<dyn Stream<Item = Result<PasteResponse, Status>> + Send>>;

    async fn say(&self, request: Request<SayRequest>) -> Result<Response<()>, Status> {
        let say = request.into_inner().say.and_then(|say| say.does);
        let say = say.ok_or_else(|| Status::invalid_argument("it says nothing to do"))?;
        let said = ToSay::parse(say).map_err(Status::invalid_argument)?;
        let at = SystemTime::now();
        self.ask(|answer| Command::Say { said, at, answer }).await?;
        Ok(Response::new(()))
    }

    async fn paste(
        &self,
        request: Request<PasteRequest>,
    ) -> Result<Response<Self::PasteStream>, Status> {
        let (on, pasted) = Pasted::of(request.into_inner()).map_err(Status::invalid_argument)?;
        let unasked = |why: Status| Status::failed_precondition(why.message());
        if let Some(mut daemon) = self.bridged(on).await.map_err(unasked)? {
            let answer = daemon.paste(pasted.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let holders = holders(&self.host_local().await?.into_local().projects)?;
        let answer = late(move || pasted.answer(&holders).map_err(unready));
        Ok(Response::new(Box::pin(answer)))
    }

    type PrunesStream = Pin<Box<dyn Stream<Item = Result<PrunesResponse, Status>> + Send>>;

    async fn prunes(
        &self,
        request: Request<PrunesRequest>,
    ) -> Result<Response<Self::PrunesStream>, Status> {
        let PrunesRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let answer = daemon.prunes(prunes::asked(None, &dir)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let local = self.host_local().await?.into_local();
        let answer = late(move || {
            let occupied = || Occupied::of(&local, Lunes::running()?);
            let planned = prunes::plan(&dir, occupied).map_err(Status::failed_precondition)?;
            Ok(PrunesResponse::from(planned.plan()))
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type PruneStream = Pin<Box<dyn Stream<Item = Result<PruneResponse, Status>> + Send>>;

    async fn prune(
        &self,
        request: Request<PruneRequest>,
    ) -> Result<Response<Self::PruneStream>, Status> {
        let PruneRequest { on, dir, agreed } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let told = PruneRequest {
                on: None,
                dir: dir.into(),
                agreed,
            };
            let answer = daemon.prune(told).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let agreed: BTreeSet<prunes::Deleting> = agreed.into_iter().map(Into::into).collect();
        let core = self.core.clone();
        let answer = late(move || {
            let occupied = || {
                let local = core::ask(&core, Command::HostLocal)
                    .ok_or("luned's core did not say")?
                    .into_local();
                Occupied::of(&local, Lunes::running()?)
            };
            let pruned = prunes::deleted_as_agreed(&dir, &agreed, occupied)
                .map_err(Status::failed_precondition)?;
            Ok(PruneResponse::from(&pruned))
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type RebaseStream = Pin<Box<dyn Stream<Item = Result<RebaseResponse, Status>> + Send>>;

    async fn rebase(
        &self,
        request: Request<RebaseRequest>,
    ) -> Result<Response<Self::RebaseStream>, Status> {
        let RebaseRequest { on, dir, branch } = request.into_inner();
        let dir = directory("dir", dir)?;
        let unnamed = || Status::invalid_argument("branch: it names no branch");
        let branch = branch
            .map(|branch| Said::new(branch).ok_or_else(unnamed))
            .transpose()?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = rebases::asked(None, &dir, branch.as_ref());
            let answer = daemon.rebase(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let answer = late(move || {
            let rebased = rebases::rebase(&dir, branch.as_ref());
            let rebased = rebased.map_err(rebases::Unrebased::status)?;
            Ok(rebased.to_api())
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type FileIssueStream = Pin<Box<dyn Stream<Item = Result<FileIssueResponse, Status>> + Send>>;

    async fn file_issue(
        &self,
        request: Request<FileIssueRequest>,
    ) -> Result<Response<Self::FileIssueStream>, Status> {
        let (on, filing) = Filing::of(request.into_inner()).map_err(Status::invalid_argument)?;
        let unreached = |status: Status| Unfiled::Refused(status.message().to_owned()).status();
        if let Some(mut daemon) = self.bridged(on).await.map_err(unreached)? {
            let answer = daemon.file_issue(filing.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let answer = late(move || Ok(filing.file().map_err(Unfiled::status)?.answer()));
        Ok(Response::new(Box::pin(answer)))
    }

    async fn issue_repositories(
        &self,
        request: Request<IssueRepositoriesRequest>,
    ) -> Result<Response<IssueRepositoriesResponse>, Status> {
        let IssueRepositoriesRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let request = IssueRepositoriesRequest {
                on: None,
                dir: dir.to_string(),
            };
            return daemon.issue_repositories(request).await;
        }
        let listed = task::spawn_blocking(move || issues::remotes(&dir)).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let remotes = listed.map_err(Status::failed_precondition)?;
        let repositories = remotes.into_iter().map(Remote::answer).collect();
        Ok(Response::new(IssueRepositoriesResponse { repositories }))
    }

    type KeepImageStream = Pin<Box<dyn Stream<Item = Result<KeepImageResponse, Status>> + Send>>;

    type ShowStream = Pin<Box<dyn Stream<Item = Result<ShowResponse, Status>> + Send>>;

    async fn show(
        &self,
        request: Request<Streaming<ShowRequest>>,
    ) -> Result<Response<Self::ShowStream>, Status> {
        use crate::api::show_request::{Event, Hello};
        let mut heard = request.into_inner();
        let Some(ShowRequest {
            event: Some(Event::Hello(hello)),
            asked,
        }) = heard.message().await?
        else {
            return Err(Status::invalid_argument("the first word is no hello"));
        };
        let Hello {
            worktree,
            size,
            runs_in,
            again,
            on,
        } = hello;
        if on.is_some() && runs_in.is_some() {
            return Err(Status::invalid_argument(RUNS_IN_ON));
        }
        if let Some((mut daemon, permit)) = self.bridged_held(on).await? {
            let hello = Hello {
                worktree,
                size,
                runs_in: None,
                again,
                on: None,
            };
            let first = ShowRequest {
                event: Some(Event::Hello(hello)),
                asked,
            };
            let rest = heard.map_while(Result::ok);
            let answer = daemon.show(tokio_stream::once(first).chain(rest)).await?;
            let held = Held {
                stream: answer.into_inner(),
                _permit: permit,
            };
            return Ok(Response::new(Box::pin(held)));
        }
        let asked_number = crate::AskNumber::of(asked);
        let worktree = layouts::id("worktree", worktree).map(WorktreeId::new);
        let worktree = worktree.map_err(Status::invalid_argument)?;
        let size = layouts::size_of(size).map_err(Status::invalid_argument)?;
        let runs_in = runs_in.as_ref().map(TerminalKey::of).transpose();
        let runs_in = runs_in.map_err(|why| Status::invalid_argument(format!("runs_in: {why}")))?;
        let (out, answers) = tokio::sync::mpsc::channel(layouts::ANSWERS_MAX);
        let layouts = self.layouts.clone();
        let came = task::spawn_blocking(move || {
            let came = |answer| layouts::Command::Came {
                worktree,
                size,
                runs_in,
                again,
                asked_number,
                out,
                answer,
            };
            layouts::ask(&layouts, came)
        });
        let came = came.await.map_err(|error| unfinished(&error))?;
        let client = came.ok_or_else(|| Status::unavailable("the layouts have stopped"))??;
        let layouts = self.layouts.clone();
        tokio::spawn(async move {
            loop {
                let (asked_number, asked) = match heard.message().await {
                    Ok(Some(ShowRequest {
                        event: Some(event),
                        asked,
                    })) => (crate::AskNumber::of(asked), layouts::Asked::of_api(event)),
                    Ok(Some(ShowRequest { event: None, asked })) => (
                        crate::AskNumber::of(asked),
                        Err("a word says nothing".to_owned()),
                    ),
                    Ok(None) | Err(_) => break,
                };
                let layouts = layouts.clone();
                let told = task::spawn_blocking(move || {
                    layouts
                        .send(layouts::Command::Asked {
                            client,
                            asked_number,
                            asked,
                        })
                        .is_ok()
                });
                if !matches!(told.await, Ok(true)) {
                    return;
                }
            }
            let left = move || layouts.send(layouts::Command::Left(client));
            let _ = task::spawn_blocking(left).await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(answers))))
    }

    type FramesStream = Pin<Box<dyn Stream<Item = Result<ToViewer, Status>> + Send>>;

    async fn frames(
        &self,
        request: Request<Streaming<FramesRequest>>,
    ) -> Result<Response<Self::FramesStream>, Status> {
        self.frames_served(request, |_| Ok(())).await
    }

    async fn change_projects(
        &self,
        request: Request<ChangeProjectsRequest>,
    ) -> Result<Response<ChangeProjectsResponse>, Status> {
        let ChangeProjectsRequest { on, change } = request.into_inner();
        let unsent = |unreached: Status| changes::unsent(&unreached);
        if let Some(mut daemon) = self.bridged(on).await.map_err(unsent)? {
            return daemon
                .change_projects(ChangeProjectsRequest { on: None, change })
                .await;
        }
        let change = change.ok_or_else(|| Status::invalid_argument("it says nothing to change"))?;
        let layouts = self.layouts.clone();
        let changed = task::spawn_blocking(move || change_answered(&layouts, change));
        Ok(Response::new(
            changed.await.map_err(|error| unfinished(&error))??,
        ))
    }

    type MakeWorktreeStream =
        Pin<Box<dyn Stream<Item = Result<MakeWorktreeResponse, Status>> + Send>>;

    async fn make_worktree(
        &self,
        request: Request<MakeWorktreeRequest>,
    ) -> Result<Response<Self::MakeWorktreeStream>, Status> {
        let MakeWorktreeRequest {
            on,
            project,
            branch,
            opens,
        } = request.into_inner();
        let unnamed = || Status::invalid_argument("project: no id is 0");
        let project = ProjectId::of(project).ok_or_else(unnamed)?;
        let branch = asked_branch(&branch)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let (project, branch) = (project.get(), branch.as_str().to_owned());
            let asked = MakeWorktreeRequest {
                on: None,
                project,
                branch,
                opens,
            };
            let answer = daemon.make_worktree(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let unopenable = |why: String| Status::invalid_argument(format!("opens: {why}"));
        let opens = match opens {
            None => Opens::Shell,
            opens @ Some(_) => Opens::of_api(opens).map_err(unopenable)?,
        };
        let agent_asked = matches!(opens, Opens::Agent(_));
        let layouts = self.layouts.clone();
        let answer = late(move || {
            let root = match rooted(&layouts, project)? {
                Root::Clone(root) => root,
                Root::Folder { name, .. } => {
                    let one = folder_has_one_worktree(name.as_str());
                    return Err(Status::failed_precondition(one));
                }
            };
            let of = Of::Branch(branch);
            let folder = links::worktree(root.as_str(), &of).map_err(branch_unready)?;
            let unopened = |status: Status| {
                let why = status.message();
                Status::aborted(format!(
                    "{folder} is {of}'s worktree, and it is not opened: {why}"
                ))
            };
            let (project, opens) = (project.get(), Some(opens.to_api()));
            let add = AddWorktree {
                project,
                folder: folder.to_string(),
                name: None,
                opens,
            };
            let add = change_projects_request::Change::AddWorktree(add);
            let Changed { made, .. } = change_made(&layouts, add).map_err(unopened)?;
            let landed = match made {
                Some(Made::Worktree(made)) if agent_asked => Landed::Started(made.get()),
                Some(Made::Worktree(made)) => Landed::Made(made.get()),
                Some(Made::Found(found)) => Landed::Found(found.get()),
                made => unreachable!("an added worktree is made or found, not {made:?}"),
            };
            Ok(MakeWorktreeResponse {
                landed: Some(landed),
            })
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type ListWorkStream = Pin<Box<dyn Stream<Item = Result<ListWorkResponse, Status>> + Send>>;

    async fn list_work(
        &self,
        request: Request<ListWorkRequest>,
    ) -> Result<Response<Self::ListWorkStream>, Status> {
        let ListWorkRequest { on, project } = request.into_inner();
        let unnamed = || Status::invalid_argument("project: no id is 0");
        let project = ProjectId::of(project).ok_or_else(unnamed)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = ListWorkRequest {
                on: None,
                project: project.get(),
            };
            let answer = daemon.list_work(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let (layouts, listings) = (self.layouts.clone(), Arc::clone(&self.listings));
        let answer = late(move || {
            let root = rooted(&layouts, project)?;
            let listed = listings
                .list(root.dir())
                .map_err(Status::failed_precondition)?;
            Ok(listed.answer())
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type ReadDiffStream = Pin<Box<dyn Stream<Item = Result<ReadDiffResponse, Status>> + Send>>;

    async fn read_diff(
        &self,
        request: Request<ReadDiffRequest>,
    ) -> Result<Response<Self::ReadDiffStream>, Status> {
        let ReadDiffRequest { on, worktree } = request.into_inner();
        let unnamed = || Status::invalid_argument("worktree: no id is 0");
        let worktree = WorktreeId::of(worktree).ok_or_else(unnamed)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = ReadDiffRequest {
                on: None,
                worktree: worktree.get(),
            };
            let answer = daemon.read_diff(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let layouts = self.layouts.clone();
        let answer = late(move || {
            let dir = worktree_folder(&layouts, worktree)?;
            let out = git::patch(&dir).map_err(Status::failed_precondition)?;
            Ok(out.into_api())
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn keep_image(
        &self,
        request: Request<Streaming<KeepImageRequest>>,
    ) -> Result<Response<Self::KeepImageStream>, Status> {
        let mut parts = request.into_inner();
        let Some(KeepImageRequest {
            part: Some(Part::Image(Image { on, size })),
        }) = parts.message().await?
        else {
            return Err(Status::invalid_argument(
                "the first part does not say what the image is",
            ));
        };
        let size = Size::new(size).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let image = Image {
                on: None,
                size: size.get(),
            };
            let image = KeepImageRequest {
                part: Some(Part::Image(image)),
            };
            let rest = parts.map_while(Result::ok);
            let answer = daemon
                .keep_image(tokio_stream::once(image).chain(rest))
                .await?;
            return Ok(Response::new(Box::pin(answer.into_inner())));
        }
        let handle = Handle::current();
        let answer = late(move || {
            let kept = kept_image(&handle, parts, size);
            kept.map(|path| KeepImageResponse { path })
                .map_err(Status::failed_precondition)
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn list_resume(
        &self,
        request: Request<ListResumeRequest>,
    ) -> Result<Response<ListResumeResponse>, Status> {
        let ListResumeRequest { on, terminal_id } = request.into_inner();
        let terminal_id = terminal_id
            .map(|id| {
                NonZeroU32::new(id)
                    .ok_or_else(|| Status::invalid_argument("terminal_id: no id is 0"))
            })
            .transpose()?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let terminal_id = terminal_id.map(NonZeroU32::get);
            return daemon
                .list_resume(ListResumeRequest {
                    on: None,
                    terminal_id,
                })
                .await;
        }
        let layouts = self.layouts.clone();
        let listed = task::spawn_blocking(move || {
            layouts::ask(&layouts, |answer| layouts::Command::ListResume {
                terminal_id,
                answer,
            })
        })
        .await
        .map_err(|error| unfinished(&error))?;
        let response = listed.ok_or_else(|| Status::unavailable("the layouts have stopped"))?;
        Ok(Response::new(response))
    }

    async fn reload_resume(
        &self,
        request: Request<ReloadResumeRequest>,
    ) -> Result<Response<ReloadResumeResponse>, Status> {
        let ReloadResumeRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.reload_resume(ReloadResumeRequest { on: None }).await;
        }
        let layouts = self.layouts.clone();
        let reloaded = task::spawn_blocking(move || {
            layouts::ask(&layouts, |answer| layouts::Command::ReloadResume { answer })
        })
        .await
        .map_err(|error| unfinished(&error))?;
        let response = reloaded.ok_or_else(|| Status::unavailable("the layouts have stopped"))?;
        Ok(Response::new(response))
    }

    async fn start_flow(
        &self,
        request: Request<StartFlowRequest>,
    ) -> Result<Response<FlowStateResponse>, Status> {
        let StartFlowRequest {
            on,
            worktree,
            flow_name,
            mode,
            harness,
            entry,
        } = request.into_inner();
        let worktree_id = layouts::id("worktree", worktree)
            .map(WorktreeId::new)
            .map_err(Status::invalid_argument)?;
        let parsed_mode =
            lune_domain::flows::FlowMode::of_api(mode).map_err(Status::invalid_argument)?;
        let parsed_harness = match api::Harness::try_from(harness) {
            Ok(api::Harness::ClaudeCode | api::Harness::Unspecified) => Harness::ClaudeCode,
            Ok(api::Harness::Pi) => Harness::Pi,
            Ok(api::Harness::Antigravity) => Harness::Antigravity,
            Err(_) => return Err(Status::invalid_argument("harness: unknown harness")),
        };
        let parsed_entry = match &entry {
            Some(api::start_flow_request::Entry::ImplementPrompt(prompt)) => FlowEntry::Implement {
                prompt: prompt.clone(),
            },
            Some(api::start_flow_request::Entry::PullRequest(())) => FlowEntry::PullRequest,
            None => {
                return Err(Status::invalid_argument(
                    "entry: say whether the flow implements a prompt or starts from the pull request",
                ));
            }
        };
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .start_flow(StartFlowRequest {
                    on: None,
                    worktree,
                    flow_name,
                    mode,
                    harness,
                    entry,
                })
                .await;
        }
        let layouts = self.layouts.clone();
        let flows = self.flows.clone();
        let started = task::spawn_blocking(move || {
            let dir = worktree_folder(&layouts, worktree_id)?;
            flows::ops::check_start(dir.as_str(), &parsed_entry)
                .map_err(Status::failed_precondition)?;
            let result = flows::ask(&flows, |answer| flows::Command::Start {
                worktree: worktree_id,
                dir: dir.as_str().to_owned(),
                flow_name,
                mode: parsed_mode,
                harness: parsed_harness,
                entry: parsed_entry,
                answer,
            })
            .map_err(Status::unavailable)?;
            result.map_err(Status::failed_precondition)
        })
        .await
        .map_err(|error| unfinished(&error))??;
        Ok(Response::new(FlowStateResponse {
            flow: Some(started),
        }))
    }

    async fn control_flow(
        &self,
        request: Request<ControlFlowRequest>,
    ) -> Result<Response<FlowStateResponse>, Status> {
        let ControlFlowRequest {
            on,
            worktree,
            control,
        } = request.into_inner();
        let worktree_id = layouts::id("worktree", worktree)
            .map(WorktreeId::new)
            .map_err(Status::invalid_argument)?;
        let parsed_control =
            lune_domain::flows::FlowControl::of_api(control).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon
                .control_flow(ControlFlowRequest {
                    on: None,
                    worktree,
                    control,
                })
                .await;
        }
        let layouts = self.layouts.clone();
        let flows = self.flows.clone();
        let updated = task::spawn_blocking(move || {
            let dir = worktree_folder(&layouts, worktree_id)
                .ok()
                .map(|dir| dir.as_str().to_owned());
            let result = flows::ask(&flows, |answer| flows::Command::Control {
                worktree: worktree_id,
                dir,
                control: parsed_control,
                answer,
            })
            .map_err(Status::unavailable)?;
            result.map_err(Status::failed_precondition)
        })
        .await
        .map_err(|error| unfinished(&error))??;
        Ok(Response::new(FlowStateResponse { flow: updated }))
    }

    async fn list_flows(
        &self,
        request: Request<ListFlowsRequest>,
    ) -> Result<Response<ListFlowsResponse>, Status> {
        let ListFlowsRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.list_flows(ListFlowsRequest { on: None }).await;
        }
        let flows = self.flows.clone();
        let listed = task::spawn_blocking(move || {
            flows::ask(&flows, |answer| flows::Command::List { answer })
        })
        .await
        .map_err(|error| unfinished(&error))?;
        let response = listed.map_err(Status::unavailable)?;
        Ok(Response::new(response))
    }
}

fn kept_image(
    handle: &Handle,
    mut parts: Streaming<KeepImageRequest>,
    size: Size,
) -> Result<String, String> {
    let runtime = folder::find().map_err(|unfound| format!("{unfound}, to keep it in"))?;
    let mut keeping = Keeping::new(&runtime, size, SystemTime::now())?;
    let deadline = tokio::time::Instant::now() + images::PATIENCE;
    let minutes = images::PATIENCE.as_secs() / 60;
    loop {
        let next =
            handle.block_on(async { tokio::time::timeout_at(deadline, parts.message()).await });
        let next =
            next.map_err(|_| format!("the image had not all come after {minutes} minutes"))?;
        let next =
            next.map_err(|status| format!("the image did not all come: {}", status.message()))?;
        match next {
            None => return keeping.kept(),
            Some(KeepImageRequest {
                part: Some(Part::Bytes(bytes)),
            }) => {
                keeping = keeping.write(&bytes)?;
            }
            Some(_) => return Err("a part after the first is not the image's bytes".to_owned()),
        }
    }
}

fn kept_checks(
    Failing {
        head,
        failed,
        pending,
        passed,
    }: Failing,
) -> KeepChecksResponse {
    let file = |kept: KeptFile| kept.to_api();
    let failed = failed
        .into_iter()
        .map(|checks::Failed { name, link, log }| FailedCheck {
            name,
            link: link.unwrap_or_default(),
            log: Some(match log {
                Log::Kept(kept) => keep_checks_response::failed::Log::Kept(file(kept)),
                Log::Unkept(why) => keep_checks_response::failed::Log::Unkept(why),
            }),
        });
    KeepChecksResponse {
        head,
        failed: failed.collect(),
        pending,
        passed,
    }
}

struct Held<S> {
    stream: S,
    _permit: OwnedSemaphorePermit,
}

impl<S: Stream + Unpin> Stream for Held<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<S::Item>> {
        Pin::new(&mut self.stream).poll_next(context)
    }
}

fn ended_by(
    answer: impl Stream<Item = Result<ToViewer, Status>> + Send + 'static,
    mut refused: oneshot::Receiver<Status>,
) -> Pin<Box<dyn Stream<Item = Result<ToViewer, Status>> + Send>> {
    let refusal = tokio_stream::once(()).filter_map(move |()| refused.try_recv().ok().map(Err));
    Box::pin(answer.chain(refusal))
}

fn late<T, J>(job: J) -> ReceiverStream<Result<T, Status>>
where
    T: Send + 'static,
    J: FnOnce() -> Result<T, Status> + Send + 'static,
{
    let (answer, answered) = tokio::sync::mpsc::channel(1);
    task::spawn(async move {
        let done = match task::spawn_blocking(job).await {
            Ok(done) => done,
            Err(error) => Err(unfinished(&error)),
        };
        let _ = answer.send(done).await;
    });
    ReceiverStream::new(answered)
}

fn holders(projects: &api::projects::Held) -> Result<Holders, Status> {
    match projects {
        api::projects::Held::Kept(kept) => {
            Holders::of_api(&kept.projects).map_err(Status::internal)
        }
        api::projects::Held::Unkept(why) => {
            let unkept = format!("nothing made: luned keeps no projects: {why}");
            Err(Status::failed_precondition(unkept))
        }
    }
}

fn unready(why: Unready) -> Status {
    match why {
        Unready::Nowhere(why) => Status::not_found(why),
        Unready::Failed(why) => Status::failed_precondition(why),
        Unready::InDoubt(why) => Status::aborted(why),
    }
}

fn talked<T>(talk: T) -> ReceiverStream<Result<AskResponse, Status>>
where
    T: FnOnce(&program::Wanted, &dyn Fn(Step)) -> Option<Result<(), String>> + Send + 'static,
{
    let (heard, steps) = tokio::sync::mpsc::channel(STEPS_WAITING);
    let (cancel, wanted) = program::cancel();
    let (done, mut finished) = oneshot::channel::<()>();
    let closed = heard.clone();
    task::spawn(async move {
        let mut closing = std::pin::pin!(closed.closed());
        std::future::poll_fn(|cx| {
            if closing.as_mut().poll(cx).is_ready() {
                return Poll::Ready(());
            }
            if Pin::new(&mut finished).poll(cx).is_ready() {
                return Poll::Ready(());
            }
            Poll::Pending
        })
        .await;
        cancel.give_up();
    });
    let failed = heard.clone();
    let talking = task::spawn_blocking(move || {
        let hear = |step: Step| {
            let _ = heard.blocking_send(Ok(step.answer()));
        };
        if let Some(Err(why)) = talk(&wanted, &hear) {
            let _ = heard.blocking_send(Err(Status::failed_precondition(why)));
        }
        let _ = done.send(());
    });
    task::spawn(async move {
        if talking.await.is_err() {
            let _ = failed.send(Err(Status::internal(UNTALKED))).await;
        }
    });
    ReceiverStream::new(steps)
}

fn directory(field: &str, dir: String) -> Result<Directory, Status> {
    Directory::parse(dir).map_err(|why| Status::invalid_argument(format!("{field}: {why}")))
}

fn asked_branch(branch: &str) -> Result<Branch, Status> {
    match Of::parse(branch).map_err(Status::invalid_argument)? {
        Of::Branch(branch) => Ok(branch),
        Of::Pull(link) => Err(Status::invalid_argument(format!(
            "{link} is a pull request: paste its link to make its worktree"
        ))),
    }
}

fn rooted(layouts: &SyncSender<layouts::Command>, project: ProjectId) -> Result<Root, Status> {
    let asked = layouts::ask(layouts, |answer| layouts::Command::Root { project, answer });
    let root = asked.ok_or_else(|| Status::unavailable("the layouts have stopped"))?;
    root.map_err(|not| match not {
        Unrooted::Unkept(why) => Status::failed_precondition(why),
        Unrooted::Unknown => Status::not_found(format!("no project has the id {project}")),
    })
}

fn worktree_folder(
    layouts: &SyncSender<layouts::Command>,
    worktree: WorktreeId,
) -> Result<Dir, Status> {
    let asked = layouts::ask(layouts, |answer| layouts::Command::WorktreeFolder {
        worktree,
        answer,
    });
    let dir = asked.ok_or_else(|| Status::unavailable("the layouts have stopped"))?;
    dir.map_err(|not| match not {
        Unrooted::Unkept(why) => Status::failed_precondition(why),
        Unrooted::Unknown => Status::not_found(format!("no worktree has the id {worktree}")),
    })
}

fn branch_unready(why: Unready) -> Status {
    match why {
        Unready::InDoubt(why) => Status::aborted(format!("{why}, so the worktree may be made")),
        why @ (Unready::Nowhere(_) | Unready::Failed(_)) => unready(why),
    }
}

fn change_made(
    layouts: &SyncSender<layouts::Command>,
    change: change_projects_request::Change,
) -> Result<Changed, Status> {
    let change = projects::asked(change).map_err(Status::invalid_argument)?;
    let asked = layouts::ask(layouts, |answer| layouts::Command::Change {
        change,
        answer,
    });
    let changed = asked.ok_or_else(|| Status::unavailable("the layouts have stopped"))?;
    changed.map_err(changes::status)
}

fn change_answered(
    layouts: &SyncSender<layouts::Command>,
    change: change_projects_request::Change,
) -> Result<ChangeProjectsResponse, Status> {
    let Changed { made, opened, kept } = change_made(layouts, change)?;
    let made = made.map(|made| match made {
        Made::Project(project) => change_projects_response::Made::Project(project.get()),
        Made::Worktree(worktree) => change_projects_response::Made::Worktree(worktree.get()),
        Made::Found(worktree) => change_projects_response::Made::Found(worktree.get()),
        Made::Taken(worktree) => change_projects_response::Made::Taken(worktree.get()),
        Made::Untaken => change_projects_response::Made::Untaken(()),
    });
    let opened = opened.map(Opened::to_api);
    Ok(ChangeProjectsResponse {
        made,
        kept: Some(kept.to_api()),
        opened,
    })
}

fn unfinished(error: &task::JoinError) -> Status {
    Status::internal(format!("luned lost its answer: {error}"))
}

fn terminal_named(
    terminal: Option<api::TerminalKey>,
) -> Result<(TerminalKey, api::TerminalKey), Status> {
    let asked = terminal.ok_or_else(|| Status::invalid_argument("it names no terminal"))?;
    let key = TerminalKey::of(&asked);
    let key = key.map_err(|why| Status::invalid_argument(format!("terminal: {why}")))?;
    Ok((key, asked))
}

fn unanswered(unanswered: &Unanswered) -> Status {
    let said = unanswered.to_string();
    match unanswered {
        Unanswered::Changed => Status::aborted(said),
        Unanswered::Refused(_) => Status::failed_precondition(said),
        Unanswered::Unreached(Failure::Called(reply)) if reply.code() == Code::NotFound => {
            Status::not_found(said)
        }
        Unanswered::Unreached(Failure::Called(_) | Failure::Own(_)) => Status::unavailable(said),
        Unanswered::Unreached(Failure::Unread(_)) => Status::internal(said),
    }
}

impl Host {
    async fn frames_served(
        &self,
        request: Request<Streaming<FramesRequest>>,
        served: fn(&FromViewer) -> Result<(), Status>,
    ) -> Result<Response<<Host as Daemon>::FramesStream>, Status> {
        use crate::api::frames_request::{Event, Hello};
        use crate::mux::own::api::from_viewer;
        let mut heard = request.into_inner();
        let Some(FramesRequest {
            event: Some(Event::Hello(Hello { on, greeting })),
        }) = heard.message().await?
        else {
            return Err(Status::invalid_argument("the first word is no hello"));
        };
        let greeting =
            greeting.ok_or_else(|| Status::invalid_argument("the hello greets nobody"))?;
        if on.is_some() && greeting.runs_in.is_some() {
            return Err(Status::invalid_argument(RUNS_IN_ON));
        }
        let (refuse, refused) = oneshot::channel();
        let mut refuse = Some(refuse);
        let said = heard.map_while(move |word| {
            let unread = |why: &str| Status::invalid_argument(why);
            let refusal = match word {
                Ok(FramesRequest {
                    event: Some(Event::Said(said)),
                }) => match served(&said) {
                    Ok(()) => return Some(said),
                    Err(unserved) => unserved,
                },
                Ok(FramesRequest {
                    event: Some(Event::Hello(_)),
                }) => unread("a hello comes only first"),
                Ok(FramesRequest { event: None }) => unread("a word of the frames says nothing"),
                Err(_) => return None,
            };
            let refuse = refuse
                .take()
                .expect("the words for frames end at the first refused");
            drop(refuse.send(refusal));
            None
        });
        if let Some((mut daemon, permit)) = self.bridged_held(on).await? {
            let hello = Hello {
                on: None,
                greeting: Some(greeting),
            };
            let first = FramesRequest {
                event: Some(Event::Hello(hello)),
            };
            let rest = said.map(|said| FramesRequest {
                event: Some(Event::Said(said)),
            });
            let answer = daemon.frames(tokio_stream::once(first).chain(rest)).await?;
            let stream = ended_by(answer.into_inner(), refused);
            return Ok(Response::new(Box::pin(Held {
                stream,
                _permit: permit,
            })));
        }
        let first = FromViewer {
            event: Some(from_viewer::Event::Hello(greeting)),
        };
        let answer = viewer::relayed(tokio_stream::once(first).chain(said)).await?;
        Ok(Response::new(ended_by(answer, refused)))
    }

    async fn runs_in(&self, terminal: &api::TerminalKey) -> Runs {
        let id = terminal.id;
        let ListAgentsResponse { agents, .. } = match self.ask(Command::List).await {
            Ok(listed) => listed,
            Err(status) => {
                let why = status.message();
                return Runs::Unread(format!("luned could not list terminal {id}'s agent: {why}"));
            }
        };
        let agent = agents
            .into_iter()
            .find(|agent| agent.terminal.as_ref() == Some(terminal));
        let Some(agent) = agent else {
            return Runs::Unread(format!("terminal {id} runs no agent luned knows"));
        };
        match agent.runs_in() {
            api::Harness::ClaudeCode => {}
            api::Harness::Pi => {
                let why = format!("terminal {id} runs Pi, whose menus luned does not read");
                return Runs::Unread(why);
            }
            api::Harness::Antigravity => {
                let why =
                    format!("terminal {id} runs Antigravity, whose menus luned does not read");
                return Runs::Unread(why);
            }
            api::Harness::Unspecified => {
                return Runs::Unread(format!("terminal {id} runs a harness luned does not know"));
            }
        }
        let projects = self
            .claude_folder
            .as_ref()
            .map(|folder| folder.join("projects"));
        let session = agent.session_id.as_deref().and_then(SessionId::parse);
        let Some((projects, session)) = projects.zip(session) else {
            return Runs::Claude { transcript: None };
        };
        let found = task::spawn_blocking(move || transcripts::find(&projects, &session)).await;
        let transcript = found.unwrap_or_else(|error| {
            log::say(&format!(
                "terminal {id}: no transcript is looked for: {error}"
            ));
            None
        });
        Runs::Claude { transcript }
    }

    async fn bridged(&self, on: Option<api::On>) -> Result<Option<client::Daemon>, Status> {
        Ok(self.bridge(on).await?.map(|(_, bridging)| bridging.daemon))
    }

    async fn bridged_held(
        &self,
        on: Option<api::On>,
    ) -> Result<Option<(client::Daemon, OwnedSemaphorePermit)>, Status> {
        let Some((destination, Bridging { daemon, held })) = self.bridge(on).await? else {
            return Ok(None);
        };
        let full = |_| {
            let held = format!(
                "{} shows and frames open to {destination}",
                remote::HELD_MAX
            );
            Status::resource_exhausted(format!("luned's bridge holds {held}, its most"))
        };
        Ok(Some((daemon, held.try_acquire_owned().map_err(full)?)))
    }

    async fn bridge(&self, on: Option<api::On>) -> Result<Option<(Destination, Bridging)>, Status> {
        let Some(api::On { destination }) = on else {
            return Ok(None);
        };
        let destination: Destination = destination
            .parse()
            .map_err(|why: String| Status::invalid_argument(why))?;
        let Some(bridging) = self.bridges.to(&destination).await else {
            return Err(Status::unavailable(format!(
                "luned does not reach {destination} now"
            )));
        };
        Ok(Some((destination, bridging)))
    }

    fn send_question<T>(
        &self,
        question: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<oneshot::Receiver<T>, Status> {
        let (answer, answered) = oneshot::channel();
        match self.core.try_send(question(answer)) {
            Ok(()) => Ok(answered),
            Err(TrySendError::Full(_)) => Err(Status::resource_exhausted(
                "the daemon has too many questions open",
            )),
            Err(TrySendError::Disconnected(_)) => Err(stopped()),
        }
    }

    async fn ask<T>(
        &self,
        question: impl FnOnce(oneshot::Sender<T>) -> Command,
    ) -> Result<T, Status> {
        let answered = self.send_question(question)?;
        answered.await.map_err(|_| stopped())
    }

    async fn host_all(&self) -> Result<AllSnapshot, Status> {
        self.ask(Command::HostAll).await
    }

    async fn host_local(&self) -> Result<LocalSnapshot, Status> {
        self.ask(Command::HostLocal).await
    }

    async fn watch_all(
        &self,
        waiter: ClientWaiter,
        after: Option<HostVersion>,
    ) -> Result<WatchAnswer<AllSnapshot>, Status> {
        let _waits = self.watchers.waits(Waiter::Client(waiter));
        let Some(version) = after else {
            return Ok(WatchAnswer::Changed(self.host_all().await?));
        };
        let decided = self.send_question(|decision| Command::WatchAll {
            after: version,
            decision,
        })?;
        await_decision(decided, || {
            if waiter == ClientWaiter::Client {
                log::say(&parked_watch(version));
            }
        })
        .await
    }

    async fn watch_local(
        &self,
        standing: Standing,
        after: Option<LocalVersion>,
    ) -> Result<WatchAnswer<LocalSnapshot>, Status> {
        let _waits = self.watchers.waits(Waiter::Daemon(standing));
        let Some(version) = after else {
            return Ok(WatchAnswer::Changed(self.host_local().await?));
        };
        let decided = self.send_question(|decision| Command::WatchLocal {
            after: version,
            decision,
        })?;
        await_decision(decided, || {}).await
    }
}

async fn await_decision<S>(
    decided: oneshot::Receiver<WatchDecision<S>>,
    on_parked: impl FnOnce(),
) -> Result<WatchAnswer<S>, Status> {
    let deadline = tokio::time::Instant::now() + WATCH;
    match timeout_at(deadline, decided).await {
        Ok(Ok(WatchDecision::Now(snapshot))) => Ok(WatchAnswer::Changed(snapshot)),
        Ok(Ok(WatchDecision::Full)) => Err(Status::resource_exhausted(
            "too many clients wait for the host",
        )),
        Ok(Ok(WatchDecision::Parked { at, woken })) => {
            on_parked();
            match timeout_at(deadline, woken).await {
                Ok(Ok(snapshot)) => Ok(WatchAnswer::Changed(snapshot)),
                Ok(Err(_)) => Err(stopped()),
                Err(_) => Ok(WatchAnswer::Unchanged(at)),
            }
        }
        Ok(Err(_)) => Err(stopped()),
        Err(_) => Err(Status::deadline_exceeded(client::unanswered(WATCH))),
    }
}

enum WatchAnswer<S> {
    Changed(S),
    Unchanged(S),
}

impl<S> WatchAnswer<S> {
    fn into_snapshot(self) -> S {
        match self {
            WatchAnswer::Changed(snapshot) | WatchAnswer::Unchanged(snapshot) => snapshot,
        }
    }
}

fn stopped() -> Status {
    Status::unavailable("the daemon's core has stopped")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant, SystemTime};

    use tokio_stream::StreamExt as _;
    use tonic::{Code, Request};

    use super::{Alone, ClientWaiter, Host, UNTALKED, Waiter, Watchers, talked};
    use crate::api::daemon_server::Daemon as _;
    use crate::api::watch_local_response::Answer as LocalAnswer;
    use crate::api::watch_response::Answer;
    use crate::api::{
        self, LocalVersion, Version, WatchChangesRequest, WatchLocalChangesRequest, WatchRequest,
    };
    use crate::asks::Step;
    use crate::core::{self, Wakes};
    use crate::remote::Standing;

    #[test]
    fn a_watch_request_says_who_waits() {
        let asked = |kept, shown| {
            Waiter::parse_legacy(&WatchRequest {
                after: None,
                kept,
                shown,
            })
        };
        assert_eq!(
            asked(None, false).unwrap(),
            Waiter::Client(ClientWaiter::Client)
        );
        assert_eq!(
            asked(None, true).unwrap(),
            Waiter::Client(ClientWaiter::Window)
        );
        assert_eq!(
            asked(Some(false), false).unwrap(),
            Waiter::Daemon(Standing::Idle)
        );
        assert_eq!(
            asked(Some(false), true).unwrap_err().code(),
            Code::InvalidArgument
        );
        assert_eq!(
            asked(Some(true), false).unwrap(),
            Waiter::Daemon(Standing::Kept)
        );
        assert_eq!(
            asked(Some(true), true).unwrap(),
            Waiter::Daemon(Standing::Window)
        );
        assert_eq!(
            ClientWaiter::parse(api::ClientWaiter::Client.into()).unwrap(),
            ClientWaiter::Client
        );
        assert_eq!(
            ClientWaiter::parse(api::ClientWaiter::Window.into()).unwrap(),
            ClientWaiter::Window
        );
        assert_eq!(
            ClientWaiter::parse(api::ClientWaiter::Unspecified.into())
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        for standing in [Standing::Idle, Standing::Kept, Standing::Window] {
            let asked = standing.question(None);
            assert_eq!(Standing::parse_api(asked.standing), Ok(standing));
        }
        assert!(Standing::parse_api(api::DaemonStanding::Unspecified.into()).is_err());
    }

    #[test]
    fn only_a_window_here_is_said_to_other_machines() {
        let watchers = Watchers::default();
        let standing = || *watchers.standing.borrow();
        let shown = || *watchers.shown.borrow();

        assert!(watchers.waits(Waiter::Daemon(Standing::Idle)).is_none());
        let _kept = watchers.waits(Waiter::Daemon(Standing::Kept));
        assert_eq!((standing(), shown()), (Standing::Idle, false));
        let _far = watchers.waits(Waiter::Daemon(Standing::Window));
        assert_eq!((standing(), shown()), (Standing::Idle, true));

        let watchers = Watchers::default();
        let standing = || *watchers.standing.borrow();
        let shown = || *watchers.shown.borrow();
        let _client = watchers.waits(Waiter::Client(ClientWaiter::Client));
        assert_eq!((standing(), shown()), (Standing::Kept, false));
        let _window = watchers.waits(Waiter::Client(ClientWaiter::Window));
        assert_eq!((standing(), shown()), (Standing::Window, true));
        let _another = watchers.waits(Waiter::Client(ClientWaiter::Client));
        assert_eq!(standing(), Standing::Window);
    }

    #[test]
    fn luned_is_alone_from_the_first_look_that_finds_nothing_keeping_it() {
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

    #[test]
    fn a_turn_whose_runner_failed_does_not_end_well() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
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
        assert_eq!(
            (failed.code(), failed.message()),
            (Code::Internal, UNTALKED)
        );
    }

    #[test]
    fn a_watch_timeout_answers_unchanged_only_after_the_core_parked_it() {
        let host_with = |core| {
            let (layouts, _laid) = mpsc::sync_channel(1);
            let (flows, _flows) = mpsc::sync_channel(1);
            Host {
                started: SystemTime::now(),
                claude_folder: None,
                agy_folder: None,
                core,
                watchers: Arc::default(),
                bridges: Arc::default(),
                layouts,
                flows,
                listings: Arc::default(),
            }
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let wake = || mpsc::sync_channel(1).0;
            let host = host_with(
                core::start(Wakes {
                    git: wake(),
                    github: wake(),
                    projects: wake(),
                })
                .unwrap(),
            );
            let first = host
                .watch_changes(Request::new(WatchChangesRequest {
                    after: None,
                    waiter: api::ClientWaiter::Window.into(),
                }))
                .await
                .unwrap()
                .into_inner();
            let version = first.read_host().unwrap().version();
            let first_local = host
                .watch_local_changes(Request::new(WatchLocalChangesRequest {
                    after: None,
                    standing: api::DaemonStanding::Kept.into(),
                }))
                .await
                .unwrap()
                .into_inner();
            let local_version = first_local.read_host().unwrap().version();

            let zero_all = host
                .watch_changes(Request::new(WatchChangesRequest {
                    after: Some(Version { pid: 0, seq: 0 }),
                    waiter: api::ClientWaiter::Window.into(),
                }))
                .await
                .unwrap_err();
            assert_eq!(zero_all.code(), Code::InvalidArgument);
            let zero_local = host
                .watch_local_changes(Request::new(WatchLocalChangesRequest {
                    after: Some(LocalVersion { pid: 0, seq: 0 }),
                    standing: api::DaemonStanding::Kept.into(),
                }))
                .await
                .unwrap_err();
            assert_eq!(zero_local.code(), Code::InvalidArgument);
            let zero_legacy = host
                .watch(Request::new(WatchRequest {
                    after: Some(Version { pid: 0, seq: 0 }),
                    kept: None,
                    shown: true,
                }))
                .await
                .unwrap_err();
            assert_eq!(zero_legacy.code(), Code::InvalidArgument);
            let unspecified_waiter = host
                .watch_changes(Request::new(WatchChangesRequest {
                    after: None,
                    waiter: api::ClientWaiter::Unspecified.into(),
                }))
                .await
                .unwrap_err();
            assert_eq!(unspecified_waiter.code(), Code::InvalidArgument);
            let unspecified_standing = host
                .watch_local_changes(Request::new(WatchLocalChangesRequest {
                    after: None,
                    standing: api::DaemonStanding::Unspecified.into(),
                }))
                .await
                .unwrap_err();
            assert_eq!(unspecified_standing.code(), Code::InvalidArgument);

            let (wedged_tx, _wedged_rx) = mpsc::sync_channel(3);
            let wedged = host_with(wedged_tx);
            let asked = Version { pid: 1, seq: 0 };
            let asked_local = LocalVersion { pid: 1, seq: 0 };

            let (
                timed_out,
                stalled,
                local_timed_out,
                local_stalled,
                legacy_timed_out,
                legacy_stalled,
            ) = tokio::join!(
                host.watch_changes(Request::new(WatchChangesRequest {
                    after: Some(version.to_api()),
                    waiter: api::ClientWaiter::Window.into(),
                })),
                wedged.watch_changes(Request::new(WatchChangesRequest {
                    after: Some(asked),
                    waiter: api::ClientWaiter::Window.into(),
                })),
                host.watch_local_changes(Request::new(WatchLocalChangesRequest {
                    after: Some(local_version.to_api()),
                    standing: api::DaemonStanding::Kept.into(),
                })),
                wedged.watch_local_changes(Request::new(WatchLocalChangesRequest {
                    after: Some(asked_local),
                    standing: api::DaemonStanding::Kept.into(),
                })),
                host.watch(Request::new(WatchRequest {
                    after: Some(version.to_api()),
                    kept: None,
                    shown: true,
                })),
                wedged.watch(Request::new(WatchRequest {
                    after: Some(asked),
                    kept: None,
                    shown: true,
                })),
            );
            assert_eq!(
                timed_out.unwrap().into_inner().answer,
                Some(Answer::Unchanged(version.to_api()))
            );
            assert_eq!(stalled.unwrap_err().code(), Code::DeadlineExceeded);
            assert_eq!(
                local_timed_out.unwrap().into_inner().answer,
                Some(LocalAnswer::Unchanged(local_version.to_api()))
            );
            assert_eq!(local_stalled.unwrap_err().code(), Code::DeadlineExceeded);
            assert_eq!(
                legacy_timed_out.unwrap().into_inner().version,
                Some(version.to_api())
            );
            assert_eq!(legacy_stalled.unwrap_err().code(), Code::DeadlineExceeded);
        });
    }
}
