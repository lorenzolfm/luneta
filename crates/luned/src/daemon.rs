use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind};
use std::mem;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use lune_domain::agent::Harness;
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{folder_has_one_worktree, Holders, Made, Opens, OwnedFolder};
use lune_domain::projects::{ProjectId, WorktreeId};
use lune_protocol::socket::{self, Bound, Found};
use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::runtime::Handle;
use tokio::sync::{oneshot, OwnedSemaphorePermit};
use tokio::task;
use tokio::time::timeout;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::Server;
use tonic::{Code, Request, Response, Status, Streaming};
use tower::layer::layer_fn;

use crate::api::change_projects_request::{self, AddWorktree};
use crate::api::change_projects_response;
use crate::api::make_worktree_response::Landed;
use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::keep_image_request::{Image, Part};
use crate::api::keep_checks_response::{self, Failed as FailedCheck};
use crate::api::list_repositories_response::Repository;
use crate::api::list_skills_response::Skill;
use crate::api::{
    self, AskRequest, AskResponse, ChangeProjectsRequest, ChangeProjectsResponse,
    FileIssueRequest, FileIssueResponse, InfoResponse, IssueRepositoriesRequest,
    IssueRepositoriesResponse, KeepChecksRequest, KeepChecksResponse, KeepImageRequest,
    KeepImageResponse, ListAgentsResponse, ListContentsRequest, ListContentsResponse,
    ListRepositoriesRequest, ListRepositoriesResponse, ListSkillsRequest, ListSkillsResponse,
    ListWorkRequest, ListWorkResponse, MakeWorktreeRequest, MakeWorktreeResponse, PasteRequest,
    PasteResponse, PrintedFile, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse,
    RebaseRequest, RebaseResponse, SayRequest, ShowRequest, ShowResponse, TranscriptEndsRequest,
    TranscriptEndsResponse, FramesRequest, AnswerRequest, WaitingRequest, WaitingResponse,
};
use crate::answers::{self, Answer, Runs, Unanswered};
use crate::mux::Failure;
use crate::panics::Hooked;
use crate::asks::{Asked, Step};
use crate::core::{self, After, Command, Full, Parked, Wakes};
use crate::directory::Directory;
use crate::projects::{Changed, Lock, Opened, Store};
use crate::layouts::{self, Root, Unrooted};
use crate::mux::own::api::{FromViewer, ToViewer};
use crate::mux::own::viewer;
use crate::prunes::{self, Occupied};
use crate::rebases;
use crate::folder::{self, Refusal, LOCK, SOCKET};
use crate::images::{self, Keeping, Size};
use crate::issues::{self, Filing, Remote, Unfiled};
use crate::links::{self, Branch, Of, Pasted, Unready};
use crate::sayings::ToSay;
use crate::remote::{Bridges, Bridging, Destination, Standing};
use crate::printed::Kept as KeptFile;
use crate::transcripts::{self, Reading, SessionId};
use crate::agent::Agent;
use crate::checks::{Failing, Log};
use crate::{
    changes, checks, claude, client, eza, fetches, git, github, keeps, log, owns, pi, printed,
    projects, release, remote, repositories, skills, tailnet, topics, watch, work,
};

pub const SWEEP: Duration = Duration::from_secs(2);

const GIT_STATUS_SWEEP: Duration = Duration::from_secs(5);

const FETCH_SWEEP: Duration = Duration::from_mins(5);

const PULL_REQUEST_SWEEP: Duration = Duration::from_mins(2);

const PENDING_CHECKS_SWEEP: Duration = Duration::from_secs(15);

const KEEPS_SWEEP: Duration = Duration::from_hours(1);

pub const WATCH: Duration = Duration::from_secs(4);

const _: () = assert!(WATCH.as_millis() < client::ANSWER.waits().as_millis());

#[must_use]
pub fn parked_watch(version: api::Version) -> String {
    let api::Version { pid, seq } = version;
    format!("a watch waits for a change past {pid}:{seq}")
}

async fn says_parked(told: Option<oneshot::Receiver<api::Version>>) {
    let Some(told) = told else {
        return;
    };
    if let Ok(Ok(version)) = timeout(WATCH, told).await {
        log::say(&parked_watch(version));
    }
}

const QUESTIONS_MAX: u32 = 16;

pub const ANSWERING_MAX: Duration = Duration::from_mins(2);

const BRIDGED_ASKS_MIN: usize = 4;

const _: () = assert!(remote::HELD_MAX + BRIDGED_ASKS_MIN == QUESTIONS_MAX as usize);

const RUNS_IN_ON: &str = "runs_in names a terminal of this machine, and on names another";

const STEPS_WAITING: usize = 64;

const UNTALKED: &str = "the turn ended with no answer, as the daemon's runner of it failed";

const IN_FLIGHT: u32 = 128 * 1024;

pub const ALONE: Duration = Duration::from_secs(30);

const _: () =
    assert!(ALONE.as_secs() > SWEEP.as_secs(), "luned is alone over more than one look");

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
            Ok(()) => {},
            Err(TryLockError::WouldBlock) => return Err(Refusal::Running(folder.to_owned())),
            Err(TryLockError::Error(error)) => return Err(Refusal::Io(path, error)),
        }
        let socket = folder.join(SOCKET);
        match fs::remove_file(&socket) {
            Ok(()) => {},
            Err(error) if error.kind() == ErrorKind::NotFound => {},
            Err(error) => return Err(Refusal::Io(socket, error)),
        }
        Ok(Claim { folder: folder.to_owned(), socket, _lock: lock })
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
) -> io::Result<()> {
    let (listener, bound) = claim.bind()?;
    let tailnet = if projects.is_ok() {
        let home = tailnet::in_own_home();
        if let Err(why) = &home {
            log::say(&format!("{why}, so luned stays off the tailnet"));
        }
        home.is_ok()
    } else {
        false
    };
    let started = SystemTime::now();
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let (wake_github, github_woken) = mpsc::sync_channel(1);
    let github = wake_github.clone();
    let wakes = Wakes { git: wake_git.clone(), github };
    let core = core::start(wakes)?;
    let store = Store::open(projects, core.clone())?;
    if let Some(why) = store.unkept() {
        log::say(why);
    }
    let (wake_owns, owns_woken) = mpsc::sync_channel(1);
    let layouts = layouts::start(core.clone(), store, wake_owns.clone(), SWEEP)?;
    projects::follow(core.clone(), layouts.clone(), SWEEP)?;
    let (end, ending) = oneshot::channel();
    let claude_folder = claude::folder();
    let watchers = Arc::new(Watchers::default());
    let owns = Arc::new(AtomicBool::new(false));
    follow(&core, &claim.folder, claude_folder.clone())?;
    tokio::spawn(stand(watchers.clone(), owns.clone(), bound.clone(), end));
    owns::follow(layouts.clone(), (wake_owns, owns_woken), owns, SWEEP)?;
    git::follow(core.clone(), wake_git, git_woken, GIT_STATUS_SWEEP)?;
    fetches::follow(core.clone(), FETCH_SWEEP)?;
    let rests = github::Rests { pending: PENDING_CHECKS_SWEEP, settled: PULL_REQUEST_SWEEP };
    github::follow(core.clone(), wake_github, github_woken, rests)?;
    let bridges = Arc::new(Bridges::default());
    let folder = claim.folder.clone();
    let standing = watchers.standing.subscribe();
    remote::follow(core.clone(), standing, SWEEP, folder, Arc::clone(&bridges))?;
    if let Some(claude) = &claude_folder {
        let projects = claude.join("projects");
        topics::follow(core.clone(), watchers.shown.subscribe(), projects, started, SWEEP)?;
    }
    tokio::spawn(sweep_keeps(claim.folder.clone()));
    log::say(&format!("listening on {}", claim.socket.display()));
    if let Some(why) = unkept {
        log::say(&format!("what goes wrong is kept in no log: {why}"));
    }
    let listings = Arc::new(work::Listings::default());
    let host = Arc::new(Host { started, claude_folder, core, watchers, bridges, layouts, listings });
    let (begun, beginning) = oneshot::channel();
    let (closing, closed) = tokio::sync::watch::channel(false);
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
    let router = |server: Server, host| {
        server
            .max_concurrent_streams(QUESTIONS_MAX)
            .initial_stream_window_size(IN_FLIGHT)
            .layer(layer_fn(|routes| {
                client::Within::new(routes, ANSWERING_MAX, client::unanswered)
            }))
            .add_service(DaemonServer::from_arc(host))
    };
    let tailnet_served = if tailnet {
        let mut closed = closed;
        let closed = async move {
            let _ = closed.wait_for(|closed| *closed).await;
        };
        let incoming = tailnet::incoming(SWEEP);
        let served = router(tailnet::server(), Arc::clone(&host))
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
        router(Server::builder(), host)
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), ended),
    );
    let _ = beginning.await;
    let drained = async move {
        let tailnet_ended = match tailnet_served {
            Some(tailnet_served) => tailnet_served.await,
            None => Ok(()),
        };
        if let Err(error) = tailnet_ended {
            log::say(&format!("the tailnet's server did not end: {error}"));
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
        },
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
            watch::watch_ending("claude", dir, mode, any, SWEEP, read, claude::Scan::pids, tell)?;
        },
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
    let pids = |agents: &Vec<Agent>| agents.iter().map(|agent| agent.pid).collect();
    watch::watch_ending("pi", dir, mode, any, SWEEP, read, pids, tell)
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
            },
            Err(error) => {
                if !unlooked_said {
                    let socket = bound.socket().display();
                    log::say(&format!("{socket} is not looked at, so luned runs on: {error}"));
                }
                unlooked_said = true;
            },
        }
        let (clients, daemons) = (watchers.clients.looked(), watchers.daemons.looked());
        let (window, far_window) = (watchers.windows.looked(), watchers.far_windows.looked());
        let terminals = owns.load(Ordering::Relaxed);
        let standing = Standing::of(terminals || clients || window, window);
        watchers.standing.send_if_modified(|was| mem::replace(was, standing) != standing);
        let shown = window || far_window;
        watchers.shown.send_if_modified(|was| mem::replace(was, shown) != shown);
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
            Waiter::Client => (&self.clients, None, Some(Standing::Kept)),
            Waiter::Window => (&self.clients, Some(&self.windows), Some(Standing::Window)),
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
enum Waiter {
    Client,
    Window,
    Daemon(Standing),
}

impl Waiter {
    fn of(request: &api::WatchRequest) -> Self {
        match (request.kept, request.shown) {
            (None, false) => Waiter::Client,
            (None, true) => Waiter::Window,
            (Some(kept), shown) => Waiter::Daemon(Standing::of(kept, shown)),
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
    core: SyncSender<Command>,
    watchers: Arc<Watchers>,
    bridges: Arc<Bridges>,
    layouts: SyncSender<layouts::Command>,
    listings: Arc<work::Listings>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: release::VERSION.to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
            claude_folder: self.claude_folder.as_ref().map(|it| it.as_os_str().as_bytes().to_vec()),
            built: release::BUILT,
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
        let waiter = Waiter::of(&request);
        let _waits = self.watchers.waits(waiter);
        let Some(version) = request.after else {
            return self.host().await.map(Response::new);
        };
        let (parked, told) = match waiter {
            Waiter::Client => {
                let (tell, told) = oneshot::channel();
                (Parked::Aloud(tell), Some(told))
            },
            Waiter::Window | Waiter::Daemon(_) => (Parked::Quietly, None),
        };
        let watching = timeout(WATCH, self.watched(After { version, parked }));
        let (watched, ()) = tokio::join!(watching, says_parked(told));
        let watched = match watched {
            Ok(watched) => watched,
            Err(_) => self.host().await,
        };
        watched.map(Response::new)
    }

    async fn list_skills(
        &self,
        request: Request<ListSkillsRequest>,
    ) -> Result<Response<ListSkillsResponse>, Status> {
        let ListSkillsRequest { on, dir, harness } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = ListSkillsRequest { on: None, dir: dir.into(), harness };
            return daemon.list_skills(asked).await;
        }
        let harness = Harness::said(harness)
            .map_err(|why| Status::invalid_argument(format!("harness: {why}")))?;
        let found = task::spawn_blocking(move || skills::found_in(&dir, harness)).await;
        let found = found.map_err(|error| unfinished(&error))?;
        let skills = found.map_err(Status::failed_precondition)?;
        let skill = |skills::Skill { name, description, runs }| Skill {
            name: name.to_string(),
            description,
            only_you: runs == skills::Runs::OnlyForYou,
        };
        Ok(Response::new(ListSkillsResponse { skills: skills.into_iter().map(skill).collect() }))
    }

    type KeepChecksStream =
        Pin<Box<dyn Stream<Item = Result<KeepChecksResponse, Status>> + Send>>;

    async fn keep_checks(
        &self,
        request: Request<KeepChecksRequest>,
    ) -> Result<Response<Self::KeepChecksStream>, Status> {
        let KeepChecksRequest { on, url } = request.into_inner();
        let page = checks::PullPage::parse(&url)
            .map_err(|why| Status::invalid_argument(format!("url: {why}")))?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let answer = daemon.keep_checks(KeepChecksRequest { on: None, url }).await?;
            return Ok(Response::new(Box::pin(answer.into_inner())));
        }
        let answer = late(move || {
            let kept = checks::kept_failing(&page).map_err(Status::failed_precondition)?;
            Ok(kept_checks(kept))
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn list_repositories(
        &self,
        request: Request<ListRepositoriesRequest>,
    ) -> Result<Response<ListRepositoriesResponse>, Status> {
        let ListRepositoriesRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.list_repositories(ListRepositoriesRequest { on: None }).await;
        }
        let found = task::spawn_blocking(repositories::found).await;
        let repositories::Found { repositories, owned, unread } =
            found.map_err(|error| unfinished(&error))?;
        let repository =
            |it: repositories::Repository| Repository { path: it.path().as_str().to_owned() };
        let repositories = repositories.into_iter().map(repository).collect();
        let owned = owned.iter().flat_map(OwnedFolder::folders).map(|it| it.as_str().to_owned());
        let owned = owned.collect();
        Ok(Response::new(ListRepositoriesResponse { repositories, unread, owned }))
    }

    async fn list_contents(
        &self,
        request: Request<ListContentsRequest>,
    ) -> Result<Response<ListContentsResponse>, Status> {
        let ListContentsRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.list_contents(ListContentsRequest { on: None, dir: dir.into() }).await;
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
        waiting.map(Response::new).map_err(|failure| unanswered(&Unanswered::Unreached(failure)))
    }

    async fn answer(&self, request: Request<AnswerRequest>) -> Result<Response<()>, Status> {
        let AnswerRequest { on, terminal, shown, answer } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on).await? {
            return daemon.answer(AnswerRequest { on: None, terminal, shown, answer }).await;
        }
        let (key, asked) = terminal_named(terminal)?;
        let answer = Answer::of(shown, answer).map_err(Status::invalid_argument)?;
        let runs = self.runs_in(&asked).await;
        let answered = task::spawn_blocking(move || answers::answer(key, &runs, answer));
        let answered = answered.await.map_err(|error| unfinished(&error))?;
        answered.map(Response::new).map_err(|refused| unanswered(&refused))
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
        let holders = holders(&self.host().await?)?;
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
        let host = self.host().await?;
        let occupied = Occupied::of(&host).map_err(Status::failed_precondition)?;
        let here = env::current_dir().ok();
        let answer = late(move || {
            let planned = prunes::plan(&dir, &occupied, here.as_deref());
            let said = planned.map(|planned| PrunesResponse::from(planned.plan()));
            said.map_err(Status::failed_precondition)
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
            let told = PruneRequest { on: None, dir: dir.into(), agreed };
            let answer = daemon.prune(told).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let agreed: BTreeSet<prunes::Deleting> = agreed.into_iter().map(Into::into).collect();
        let core = self.core.clone();
        let here = env::current_dir().ok();
        let answer = late(move || {
            let occupied = || {
                let host = core::ask(&core, Command::Host).ok_or("luned's core did not say")?;
                Occupied::of(&host)
            };
            let pruned = prunes::deleted_as_agreed(&dir, &agreed, occupied, here.as_deref())
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
        if let Some(mut daemon) = self.bridged(on).await? {
            let asked = rebases::asked(None, &dir, branch.as_deref());
            let answer = daemon.rebase(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let answer = late(move || {
            let rebased = rebases::rebase(&dir, branch.as_deref());
            let rebased = rebased.map_err(rebases::Unrebased::status)?;
            Ok(rebased.to_api())
        });
        Ok(Response::new(Box::pin(answer)))
    }

    type FileIssueStream =
        Pin<Box<dyn Stream<Item = Result<FileIssueResponse, Status>> + Send>>;

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
            let request = IssueRepositoriesRequest { on: None, dir: dir.to_string() };
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
        let Some(ShowRequest { event: Some(Event::Hello(hello)) }) = heard.message().await? else {
            return Err(Status::invalid_argument("the first word is no hello"));
        };
        let Hello { worktree, size, runs_in, again, on } = hello;
        if on.is_some() && runs_in.is_some() {
            return Err(Status::invalid_argument(RUNS_IN_ON));
        }
        if let Some((mut daemon, permit)) = self.bridged_held(on).await? {
            let hello = Hello { worktree, size, runs_in: None, again, on: None };
            let first = ShowRequest { event: Some(Event::Hello(hello)) };
            let rest = heard.map_while(Result::ok);
            let answer = daemon.show(tokio_stream::once(first).chain(rest)).await?;
            let held = Held { stream: answer.into_inner(), _permit: permit };
            return Ok(Response::new(Box::pin(held)));
        }
        let worktree = layouts::id("worktree", worktree).map(WorktreeId::new);
        let worktree = worktree.map_err(Status::invalid_argument)?;
        let size = layouts::size_of(size).map_err(Status::invalid_argument)?;
        let runs_in = runs_in.as_ref().map(TerminalKey::of).transpose();
        let runs_in = runs_in.map_err(|why| Status::invalid_argument(format!("runs_in: {why}")))?;
        let (out, answers) = tokio::sync::mpsc::channel(layouts::ANSWERS_MAX);
        let layouts = self.layouts.clone();
        let came = task::spawn_blocking(move || {
            let came =
                |answer| layouts::Command::Came { worktree, size, runs_in, again, out, answer };
            layouts::ask(&layouts, came)
        });
        let came = came.await.map_err(|error| unfinished(&error))?;
        let client = came.ok_or_else(|| Status::unavailable("the layouts have stopped"))??;
        let layouts = self.layouts.clone();
        tokio::spawn(async move {
            loop {
                let asked = match heard.message().await {
                    Ok(Some(ShowRequest { event: Some(event) })) => layouts::Asked::of_api(event),
                    Ok(Some(ShowRequest { event: None })) => Err("a word says nothing".to_owned()),
                    Ok(None) | Err(_) => break,
                };
                let layouts = layouts.clone();
                let told = task::spawn_blocking(move || {
                    layouts.send(layouts::Command::Asked { client, asked }).is_ok()
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
        use crate::api::frames_request::{Event, Hello};
        use crate::mux::own::api::from_viewer;
        let mut heard = request.into_inner();
        let Some(FramesRequest { event: Some(Event::Hello(Hello { on, greeting })) }) =
            heard.message().await?
        else {
            return Err(Status::invalid_argument("the first word is no hello"));
        };
        let greeting = greeting.ok_or_else(|| Status::invalid_argument("the hello greets nobody"))?;
        if on.is_some() && greeting.runs_in.is_some() {
            return Err(Status::invalid_argument(RUNS_IN_ON));
        }
        let (refuse, refused) = oneshot::channel();
        let mut refuse = Some(refuse);
        let said = heard.map_while(move |word| {
            let refusal = match word {
                Ok(FramesRequest { event: Some(Event::Said(said)) }) => return Some(said),
                Ok(FramesRequest { event: Some(Event::Hello(_)) }) => "a hello comes only first",
                Ok(FramesRequest { event: None }) => "a word of the frames says nothing",
                Err(_) => return None,
            };
            let refuse = refuse.take().expect("the words for frames end at the first refused");
            drop(refuse.send(Status::invalid_argument(refusal)));
            None
        });
        if let Some((mut daemon, permit)) = self.bridged_held(on).await? {
            let hello = Hello { on: None, greeting: Some(greeting) };
            let first = FramesRequest { event: Some(Event::Hello(hello)) };
            let rest = said.map(|said| FramesRequest { event: Some(Event::Said(said)) });
            let answer = daemon.frames(tokio_stream::once(first).chain(rest)).await?;
            let stream = ended_by(answer.into_inner(), refused);
            return Ok(Response::new(Box::pin(Held { stream, _permit: permit })));
        }
        let first = FromViewer { event: Some(from_viewer::Event::Hello(greeting)) };
        let answer = viewer::relayed(tokio_stream::once(first).chain(said)).await?;
        Ok(Response::new(ended_by(answer, refused)))
    }

    async fn change_projects(
        &self,
        request: Request<ChangeProjectsRequest>,
    ) -> Result<Response<ChangeProjectsResponse>, Status> {
        let ChangeProjectsRequest { on, change } = request.into_inner();
        let unsent = |unreached: Status| changes::unsent(&unreached);
        if let Some(mut daemon) = self.bridged(on).await.map_err(unsent)? {
            return daemon.change_projects(ChangeProjectsRequest { on: None, change }).await;
        }
        let change = change.ok_or_else(|| Status::invalid_argument("it says nothing to change"))?;
        let layouts = self.layouts.clone();
        let changed = task::spawn_blocking(move || change_answered(&layouts, change));
        Ok(Response::new(changed.await.map_err(|error| unfinished(&error))??))
    }

    type MakeWorktreeStream =
        Pin<Box<dyn Stream<Item = Result<MakeWorktreeResponse, Status>> + Send>>;

    async fn make_worktree(
        &self,
        request: Request<MakeWorktreeRequest>,
    ) -> Result<Response<Self::MakeWorktreeStream>, Status> {
        let MakeWorktreeRequest { on, project, branch, opens } = request.into_inner();
        let unnamed = || Status::invalid_argument("project: no id is 0");
        let project = ProjectId::of(project).ok_or_else(unnamed)?;
        let branch = asked_branch(&branch)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let (project, branch) = (project.get(), branch.as_str().to_owned());
            let asked = MakeWorktreeRequest { on: None, project, branch, opens };
            let answer = daemon.make_worktree(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let unopenable = |why: String| Status::invalid_argument(format!("opens: {why}"));
        let agent = |opens: &api::Opens| matches!(opens.runs, Some(api::opens::Runs::Agent(_)));
        let agent_asked = opens.as_ref().is_some_and(agent);
        let opens = match opens {
            None => Opens::Shell,
            opens @ Some(_) => Opens::of_api(opens).map_err(unopenable)?,
        };
        let layouts = self.layouts.clone();
        let answer = late(move || {
            let root = match rooted(&layouts, project)? {
                Root::Clone(root) => root,
                Root::Folder { name, .. } => {
                    let one = folder_has_one_worktree(name.as_str());
                    return Err(Status::failed_precondition(one));
                },
            };
            let of = Of::Branch(branch);
            let folder = links::worktree(root.as_str(), &of).map_err(branch_unready)?;
            let unopened = |status: Status| {
                let why = status.message();
                Status::aborted(format!("{folder} is {of}'s worktree, and it is not opened: {why}"))
            };
            let (project, opens) = (project.get(), Some(opens.to_api()));
            let add = AddWorktree { project, folder: folder.to_string(), name: None, opens };
            let add = change_projects_request::Change::AddWorktree(add);
            let Changed { made, .. } = change_made(&layouts, add).map_err(unopened)?;
            let landed = match made {
                Some(Made::Worktree(made)) if agent_asked => Landed::Started(made.get()),
                Some(Made::Worktree(made)) => Landed::Made(made.get()),
                Some(Made::Found(found)) => Landed::Found(found.get()),
                made => unreachable!("an added worktree is made or found, not {made:?}"),
            };
            Ok(MakeWorktreeResponse { landed: Some(landed) })
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
            let asked = ListWorkRequest { on: None, project: project.get() };
            let answer = daemon.list_work(asked).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let (layouts, listings) = (self.layouts.clone(), Arc::clone(&self.listings));
        let answer = late(move || {
            let root = rooted(&layouts, project)?;
            let listed = listings.list(root.dir()).map_err(Status::failed_precondition)?;
            Ok(listed.answer())
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn keep_image(
        &self,
        request: Request<Streaming<KeepImageRequest>>,
    ) -> Result<Response<Self::KeepImageStream>, Status> {
        let mut parts = request.into_inner();
        let Some(KeepImageRequest { part: Some(Part::Image(Image { on, size })) }) =
            parts.message().await?
        else {
            return Err(Status::invalid_argument("the first part does not say what the image is"));
        };
        let size = Size::new(size).map_err(Status::invalid_argument)?;
        if let Some(mut daemon) = self.bridged(on).await? {
            let image = Image { on: None, size: size.get() };
            let image = KeepImageRequest { part: Some(Part::Image(image)) };
            let rest = parts.map_while(Result::ok);
            let answer = daemon.keep_image(tokio_stream::once(image).chain(rest)).await?;
            return Ok(Response::new(Box::pin(answer.into_inner())));
        }
        let handle = Handle::current();
        let answer = late(move || {
            let kept = kept_image(&handle, parts, size);
            kept.map(|path| KeepImageResponse { path }).map_err(Status::failed_precondition)
        });
        Ok(Response::new(Box::pin(answer)))
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
        let next = handle.block_on(async {
            tokio::time::timeout_at(deadline, parts.message()).await
        });
        let next = next.map_err(|_| format!("the image had not all come after {minutes} minutes"))?;
        let next = next.map_err(|status| format!("the image did not all come: {}", status.message()))?;
        match next {
            None => return keeping.kept(),
            Some(KeepImageRequest { part: Some(Part::Bytes(bytes)) }) => {
                keeping = keeping.write(&bytes)?;
            },
            Some(_) => return Err("a part after the first is not the image's bytes".to_owned()),
        }
    }
}

fn kept_checks(Failing { head, failed, pending, passed }: Failing) -> KeepChecksResponse {
    let file = |KeptFile { file, lines, left_out }| PrintedFile {
        path: file,
        lines: lines as u64,
        left_out: left_out as u64,
    };
    let failed = failed.into_iter().map(|checks::Failed { name, link, log }| FailedCheck {
        name,
        link: link.unwrap_or_default(),
        log: Some(match log {
            Log::Kept(kept) => keep_checks_response::failed::Log::Kept(file(kept)),
            Log::Unkept(why) => keep_checks_response::failed::Log::Unkept(why),
        }),
    });
    KeepChecksResponse { head, failed: failed.collect(), pending, passed }
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

fn holders(host: &api::Host) -> Result<Holders, Status> {
    let held = host.projects.as_ref().and_then(|projects| projects.held.as_ref());
    match held.expect("luned's own host says whether it keeps its projects") {
        api::projects::Held::Kept(kept) => {
            Holders::of_api(&kept.projects).map_err(Status::internal)
        },
        api::projects::Held::Unkept(why) => {
            let unkept = format!("nothing made: luned keeps no projects: {why}");
            Err(Status::failed_precondition(unkept))
        },
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
    T: FnOnce(&dyn Fn() -> bool, &dyn Fn(Step)) -> Option<Result<(), String>> + Send + 'static,
{
    let (heard, steps) = tokio::sync::mpsc::channel(STEPS_WAITING);
    let failed = heard.clone();
    let talking = task::spawn_blocking(move || {
        let wanted = || !heard.is_closed();
        let hear = |step: Step| {
            let _ = heard.blocking_send(Ok(step.answer()));
        };
        if let Some(Err(why)) = talk(&wanted, &hear) {
            let _ = heard.blocking_send(Err(Status::failed_precondition(why)));
        }
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
    let asked = layouts::ask(layouts, |answer| layouts::Command::Change { change, answer });
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
    Ok(ChangeProjectsResponse { made, kept: Some(kept.to_api()), opened })
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
        },
        Unanswered::Unreached(Failure::Called(_) | Failure::Own(_)) => Status::unavailable(said),
        Unanswered::Unreached(Failure::Unread(_)) => Status::internal(said),
    }
}

impl Host {
    async fn runs_in(&self, terminal: &api::TerminalKey) -> Runs {
        let id = terminal.id;
        let ListAgentsResponse { agents, .. } = match self.ask(Command::List).await {
            Ok(listed) => listed,
            Err(status) => {
                let why = status.message();
                return Runs::Unread(format!("luned could not list terminal {id}'s agent: {why}"));
            },
        };
        let agent = agents.into_iter().find(|agent| agent.terminal.as_ref() == Some(terminal));
        let Some(agent) = agent else {
            return Runs::Unread(format!("terminal {id} runs no agent luned knows"));
        };
        match agent.runs_in() {
            api::Harness::ClaudeCode => {},
            api::Harness::Pi => {
                let why = format!("terminal {id} runs Pi, whose menus luned does not read");
                return Runs::Unread(why);
            },
            api::Harness::Unspecified => {
                return Runs::Unread(format!("terminal {id} runs a harness luned does not know"));
            },
        }
        let projects = self.claude_folder.as_ref().map(|folder| folder.join("projects"));
        let Some((projects, session)) = projects.zip(SessionId::parse(&agent.session_id)) else {
            return Runs::Claude { transcript: None };
        };
        let found = task::spawn_blocking(move || transcripts::find(&projects, &session)).await;
        let transcript = found.unwrap_or_else(|error| {
            log::say(&format!("terminal {id}: no transcript is looked for: {error}"));
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
            let held = format!("{} shows and frames open to {destination}", remote::HELD_MAX);
            Status::resource_exhausted(format!("luned's bridge holds {held}, its most"))
        };
        Ok(Some((daemon, held.try_acquire_owned().map_err(full)?)))
    }

    async fn bridge(
        &self,
        on: Option<api::On>,
    ) -> Result<Option<(Destination, Bridging)>, Status> {
        let Some(api::On { destination }) = on else {
            return Ok(None);
        };
        let destination: Destination =
            destination.parse().map_err(|why: String| Status::invalid_argument(why))?;
        let Some(bridging) = self.bridges.to(&destination).await else {
            return Err(Status::unavailable(format!("luned does not reach {destination} now")));
        };
        Ok(Some((destination, bridging)))
    }

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

    async fn host(&self) -> Result<api::Host, Status> {
        self.ask(Command::Host).await
    }

    async fn watched(&self, after: After) -> Result<api::Host, Status> {
        let host = self.ask(|answer| Command::Watch { after, answer }).await?;
        host.map_err(|Full| Status::resource_exhausted("too many clients wait for the host"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tokio_stream::StreamExt as _;
    use tonic::Code;

    use super::{talked, Alone, Waiter, Watchers, UNTALKED};
    use crate::api::WatchRequest;
    use crate::asks::Step;
    use crate::remote::Standing;

    #[test]
    fn a_watch_request_says_who_waits() {
        let asked = |kept, shown| Waiter::of(&WatchRequest { after: None, kept, shown });
        assert_eq!(asked(None, false), Waiter::Client);
        assert_eq!(asked(None, true), Waiter::Window);
        assert_eq!(asked(Some(false), false), Waiter::Daemon(Standing::Idle));
        assert_eq!(asked(Some(false), true), Waiter::Daemon(Standing::Idle));
        assert_eq!(asked(Some(true), false), Waiter::Daemon(Standing::Kept));
        assert_eq!(asked(Some(true), true), Waiter::Daemon(Standing::Window));
        for standing in [Standing::Idle, Standing::Kept, Standing::Window] {
            assert_eq!(Waiter::of(&standing.question(None)), Waiter::Daemon(standing));
        }
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
        let _client = watchers.waits(Waiter::Client);
        assert_eq!((standing(), shown()), (Standing::Kept, false));
        let _window = watchers.waits(Waiter::Window);
        assert_eq!((standing(), shown()), (Standing::Window, true));
        let _another = watchers.waits(Waiter::Client);
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
