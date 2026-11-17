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
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use notify::RecursiveMode;
use tokio::net::UnixListener;
use tokio::runtime::Handle;
use tokio::sync::oneshot;
use tokio::task;
use tokio::time::timeout;
use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
use tokio_stream::{Stream, StreamExt as _};
use tonic::transport::{Channel, Server};
use tonic::{Request, Response, Status, Streaming};

use crate::api::close_request;
use crate::api::closes_response::{self, Tab as ClosedTab};
use crate::api::daemon_client::DaemonClient;
use crate::api::daemon_server::{Daemon, DaemonServer};
use crate::api::keep_image_request::{Image, Part};
use crate::api::keep_checks_response::{self, Failed as FailedCheck};
use crate::api::keep_printed_response::{self, File as PrintedFile, Top};
use crate::api::list_dirs_response::Dir;
use crate::api::list_exited_response::Exited;
use crate::api::list_skills_response::Skill;
use crate::api::open_request::{self, Opens};
use crate::api::other_tabs_response::Tab as OtherTab;
use crate::api::refuge_response::{self, Went};
use crate::api::{turn_into_panes_response, turn_into_tab_response};
use crate::api::{
    self, AskRequest, AskResponse, ChangeProjectsRequest, ChangeProjectsResponse,
    CheckNameRequest, CheckNameResponse, CloseRequest, ClosesRequest, ClosesResponse,
    DeleteExitedRequest, DumpScreenRequest, DumpScreenResponse, FileIssueRequest,
    FileIssueResponse, GoRequest, InfoResponse,
    IssueRepositoriesRequest, IssueRepositoriesResponse, KeepChecksRequest, KeepChecksResponse,
    KeepImageRequest, KeepImageResponse, KeepPrintedRequest, KeepPrintedResponse, KillRequest, ListAgentsResponse, ListClientsRequest,
    ListClientsResponse, ListContentsRequest, ListContentsResponse, ListDirsRequest,
    ListDirsResponse, ListExitedRequest, ListExitedResponse, ListSkillsRequest, ListSkillsResponse,
    NameSessionRequest, NameSessionResponse, OpenRequest,
    OpenResponse, OtherTabsRequest,
    OtherTabsResponse, PasteRequest, PasteResponse, PruneRequest, PruneResponse, PrunesRequest,
    PrunesResponse, RefugeRequest, RefugeResponse, RenameSessionRequest, SayRequest, ShowRequest,
    ShowResponse, StartRequest,
    TranscriptEndsRequest, TranscriptEndsResponse, TurnIntoPanesRequest, TurnIntoPanesResponse,
    TurnIntoTabRequest, TurnIntoTabResponse,
};
use crate::asks::{Asked, Step};
use crate::closes::{Close, Closes};
use crate::core::{self, Command, Wakes};
use crate::directory::Directory;
use crate::projects::{Change, Failed, Id, Store, WorkstreamId};
use crate::views;
use crate::prunes::{self, Occupied};
use crate::folder::{self, Refusal, LOCK, SOCKET};
use crate::images::{self, Keeping, Size};
use crate::issues::{self, Filing, Remote, Unfiled};
use crate::links::{Pasted, Unready};
use crate::names::Starts;
use crate::sayings::ToSay;
use crate::remote::{Bridges, Destination, Standing};
use crate::watch::Troubles;
use crate::mux::{self, Mux, SessionName, SocketFolder, Zellij};
use crate::opens::{Open, Tab};
use crate::printed::{Kept as KeptFile, Printed};
use crate::transcripts::Reading;
use crate::agent::Agent;
use crate::turns::{IntoPanes, IntoTab};
use crate::checks::{Failing, Log};
use crate::{
    checks, claude, client, closes, eza, fetches, git, github, keeps, log, moves, names, opens,
    owns, panes, pi, printed, projects, release, remote, screens, shells, skills, topics, turns,
    watch, zoxide,
    Harness,
};

const SWEEP: Duration = Duration::from_secs(2);

const TERMINAL_SWEEP: Duration = Duration::from_secs(1);

const WORK_TREE_SWEEP: Duration = Duration::from_secs(5);

const FETCH_SWEEP: Duration = Duration::from_mins(5);

const PULL_REQUEST_SWEEP: Duration = Duration::from_mins(2);

const PENDING_CHECKS_SWEEP: Duration = Duration::from_secs(15);

const KEEPS_SWEEP: Duration = Duration::from_hours(1);

const WATCH: Duration = Duration::from_secs(4);

const _: () = assert!(WATCH.as_millis() < client::ANSWER.as_millis());

const QUESTIONS_MAX: u32 = 16;

const STEPS_WAITING: usize = 64;

const UNTALKED: &str = "the turn ended with no answer, as the daemon's runner of it failed";

const IN_FLIGHT: u32 = 128 * 1024;

const ALONE: Duration = Duration::from_secs(30);

const DRAIN: Duration = client::ANSWER;

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
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}

pub async fn serve(claim: Claim, unkept: Option<String>) -> io::Result<()> {
    let listener = UnixListener::bind(&claim.socket)?;
    let started = SystemTime::now();
    let (wake_panes, panes_woken) = mpsc::sync_channel(1);
    let (wake_git, git_woken) = mpsc::sync_channel(1);
    let (wake_github, github_woken) = mpsc::sync_channel(1);
    let github = wake_github.clone();
    let wakes = Wakes { panes: wake_panes.clone(), git: wake_git.clone(), github };
    let core = core::start(wakes)?;
    let store = Store::open(core.clone())?;
    if let Some(why) = store.unkept() {
        log::say(why);
    }
    let (wake_owns, owns_woken) = mpsc::sync_channel(1);
    let views = views::start(core.clone(), store, wake_owns.clone(), SWEEP)?;
    projects::follow(core.clone(), SWEEP)?;
    let (end, ending) = oneshot::channel();
    let sockets = Zellij::socket_folder();
    let claude_folder = claude::folder();
    let watchers = Arc::new(Watchers::default());
    let folders = (claude_folder.clone(), sockets.clone());
    let owns = Arc::new(AtomicBool::new(false));
    follow(&core, &claim.folder, folders, (watchers.clone(), owns.clone()), end)?;
    panes::follow(core.clone(), wake_panes, panes_woken, Zellij::cache_folder(), SWEEP)?;
    owns::follow(views.clone(), (wake_owns, owns_woken), owns, SWEEP)?;
    git::follow(core.clone(), wake_git, git_woken, WORK_TREE_SWEEP)?;
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
    let host = Host { started, sockets, claude_folder, core, watchers, bridges, views };
    let socket = claim.socket.clone();
    let (begun, beginning) = oneshot::channel();
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
    let served = tokio::spawn(
        Server::builder()
            .max_concurrent_streams(QUESTIONS_MAX)
            .initial_stream_window_size(IN_FLIGHT)
            .timeout(client::ANSWER)
            .add_service(DaemonServer::new(host))
            .serve_with_incoming_shutdown(UnixListenerStream::new(listener), ended),
    );
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
    (claude_folder, sockets): (Option<PathBuf>, SocketFolder),
    (watchers, owns): (Arc<Watchers>, Arc<AtomicBool>),
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
    watch::watch_ending("pi", dir, mode, any, SWEEP, read, pids, tell)?;
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
        move || sockets.scan(&mut troubles)
    };
    let mut alone = Alone::default();
    let mut end = Some(end);
    let tell = move |scan: mux::Scan| {
        let (clients, daemons) = (watchers.clients.looked(), watchers.daemons.looked());
        let (window, far_window) = (watchers.windows.looked(), watchers.far_windows.looked());
        let sessions = !scan.alone() || owns.load(Ordering::Relaxed);
        let standing = Standing::of(sessions || clients || window, window);
        watchers.standing.send_if_modified(|was| mem::replace(was, standing) != standing);
        let shown = window || far_window;
        watchers.shown.send_if_modified(|was| mem::replace(was, shown) != shown);
        if alone.saw(standing == Standing::Idle && !daemons, Instant::now()) >= ALONE
            && let Some(end) = end.take()
        {
            let secs = ALONE.as_secs();
            let none = format!(
                "zellij has had no session for {secs}s, nor luneta-mux, nor lunetad a watcher"
            );
            log::say(&format!("{none}, and so lunetad ends"));
            let _ = end.send(());
        }
        core.send(Command::Sockets { sockets: scan.sockets, unread: scan.unread }).is_ok()
    };
    let wanted = |path: &Path| Zellij::of_sessions(path);
    let sockets = sockets.path().to_owned();
    watch::watch("zellij", sockets, RecursiveMode::Recursive, wanted, SWEEP, read, tell)
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
    sockets: SocketFolder,
    claude_folder: Option<PathBuf>,
    core: SyncSender<Command>,
    watchers: Arc<Watchers>,
    bridges: Arc<Bridges>,
    views: SyncSender<views::Command>,
}

#[tonic::async_trait]
impl Daemon for Host {
    async fn info(&self, _: Request<()>) -> Result<Response<InfoResponse>, Status> {
        Ok(Response::new(InfoResponse {
            version: release::VERSION.to_owned(),
            pid: std::process::id(),
            started_at: Some(self.started.into()),
            zellij_folder: Some(self.sockets.path().as_os_str().as_bytes().to_vec()),
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
        let _waits = self.watchers.waits(Waiter::of(&request));
        let after = request.after;
        let watched = match timeout(WATCH, self.watched(after)).await {
            Ok(watched) => watched,
            Err(_) => self.watched(None).await,
        };
        watched.map(Response::new)
    }

    async fn go(&self, request: Request<GoRequest>) -> Result<Response<()>, Status> {
        let GoRequest { on, at, to, pane } = request.into_inner();
        let (at, to) = (named("at", &at)?, named("to", &to)?);
        if let Some(mut daemon) = self.bridged(on)? {
            let (at, to) = (at.to_string(), to.to_string());
            return daemon.go(GoRequest { on: None, at, to, pane }).await;
        }
        let went = task::spawn_blocking(move || Mux::Zellij(&at).went(&to, pane)).await;
        let went = went.map_err(|error| unfinished(&error))?;
        went.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
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
            moves::refuge(&sockets, &seats, moves::before)
        });
        let refuge = |moves::Refuge { session, before, .. }| api::Refuge {
            session: session.to_string(),
            before,
        };
        let ended = match found.await.map_err(|error| unfinished(&error))? {
            moves::Ended::Kept => refuge_response::Ended::Kept(()),
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
        let ListSkillsRequest { on, dir, harness } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let asked = ListSkillsRequest { on: None, dir: dir.into(), harness };
            return daemon.list_skills(asked).await;
        }
        let harness = Harness::said(harness)
            .map_err(|why| Status::invalid_argument(format!("harness: {why}")))?;
        let found = task::spawn_blocking(move || skills::found_in(dir.as_str(), harness)).await;
        let found = found.map_err(|error| unfinished(&error))?;
        let skills = found.map_err(Status::failed_precondition)?;
        let skill = |skills::Skill { name, description, runs }| Skill {
            name: name.to_string(),
            description,
            only_you: runs == skills::Runs::OnlyForYou,
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
            Printed::Nothing => keep_printed_response::Printed::Nothing(()),
            Printed::Unsure(kept) => keep_printed_response::Printed::Unsure(file(kept)),
            Printed::Top(kept) => keep_printed_response::Printed::Top(Top { file: kept.map(file) }),
        };
        Ok(Response::new(KeepPrintedResponse { printed: Some(printed) }))
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
        if let Some(mut daemon) = self.bridged(on)? {
            let answer = daemon.keep_checks(KeepChecksRequest { on: None, url }).await?;
            return Ok(Response::new(Box::pin(answer.into_inner())));
        }
        let answer = late(move || {
            let kept = checks::kept_failing(&page).map_err(Status::failed_precondition)?;
            Ok(kept_checks(kept))
        });
        Ok(Response::new(Box::pin(answer)))
    }

    async fn name_session(
        &self,
        request: Request<NameSessionRequest>,
    ) -> Result<Response<NameSessionResponse>, Status> {
        let NameSessionRequest { on, dir } = request.into_inner();
        let starts = match dir {
            Some(dir) => Starts::In(directory("dir", dir)?),
            None => Starts::Home,
        };
        if let Some(mut daemon) = self.bridged(on)? {
            let dir = starts.dir().map(str::to_owned);
            return daemon.name_session(NameSessionRequest { on: None, dir }).await;
        }
        let named = task::spawn_blocking(move || names::free_at(starts)).await;
        let named = named.map_err(|error| unfinished(&error))?;
        let (name, since, dir) = named.map_err(Status::failed_precondition)?;
        let (named_at, dir) = (Some(since.into()), Some(dir.into()));
        Ok(Response::new(NameSessionResponse { name: name.to_string(), named_at, dir }))
    }

    async fn check_name(
        &self,
        request: Request<CheckNameRequest>,
    ) -> Result<Response<CheckNameResponse>, Status> {
        let CheckNameRequest { on, name } = request.into_inner();
        let name = SessionName::parse(&name)
            .map_err(|why| Status::invalid_argument(format!("name: {why}")))?;
        if let Some(mut daemon) = self.bridged(on)? {
            let name = name.to_string();
            return daemon.check_name(CheckNameRequest { on: None, name }).await;
        }
        let checked = task::spawn_blocking(move || names::unused(&name)).await;
        let checked = checked.map_err(|error| unfinished(&error))?;
        let since = checked.map_err(|unusable| Status::failed_precondition(unusable.to_string()))?;
        Ok(Response::new(CheckNameResponse { named_at: Some(since.into()) }))
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

    async fn start(&self, request: Request<StartRequest>) -> Result<Response<()>, Status> {
        let StartRequest { on, at, to, cwd } = request.into_inner();
        let (at, to) = (named("at", &at)?, named("to", &to)?);
        let cwd = cwd.map(|cwd| directory("cwd", cwd)).transpose()?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (at, to, cwd) = (at.to_string(), to.to_string(), cwd.map(String::from));
            return daemon.start(StartRequest { on: None, at, to, cwd }).await;
        }
        let start = move || Mux::Zellij(&at).started(&to, cwd.as_ref());
        let started = task::spawn_blocking(start).await;
        let started = started.map_err(|error| unfinished(&error))?;
        started.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
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
            Closes::Pane => closes_response::Closes::Pane(()),
            Closes::Tab { id, panes } => {
                closes_response::Closes::Tab(ClosedTab { id: id.0, panes })
            },
            Closes::Session => closes_response::Closes::Session(()),
        };
        Ok(Response::new(ClosesResponse { closes: Some(closes) }))
    }

    async fn close(&self, request: Request<CloseRequest>) -> Result<Response<()>, Status> {
        let CloseRequest { on, session, closes, started_at } = request.into_inner();
        let session = named("session", &session)?;
        let started = started_of(started_at)?;
        let close = match closes {
            Some(close_request::Closes::Pane(pane)) => Close::Pane(pane),
            Some(close_request::Closes::Tab(ClosedTab { panes, .. })) if panes.is_empty() => {
                return Err(Status::invalid_argument("closes: a tab of no terminal"));
            },
            Some(close_request::Closes::Tab(ClosedTab { id, panes })) => {
                Close::Tab { id: mux::TabId(id), panes }
            },
            None => return Err(Status::invalid_argument("closes: neither a pane nor a tab")),
        };
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.close(close.asked(None, &session, started)).await;
        }
        let closed = task::spawn_blocking(move || closes::closed(&session, started, &close));
        let closed = closed.await;
        let closed = closed.map_err(|error| unfinished(&error))?;
        closed.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
    }

    async fn other_tabs(
        &self,
        request: Request<OtherTabsRequest>,
    ) -> Result<Response<OtherTabsResponse>, Status> {
        let OtherTabsRequest { on, session, pane } = request.into_inner();
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
            return daemon.other_tabs(OtherTabsRequest { on: None, session, pane }).await;
        }
        let tabs = task::spawn_blocking(move || turns::other_tabs(&session, pane)).await;
        let tabs = tabs.map_err(|error| unfinished(&error))?;
        let tabs = tabs.map_err(Status::failed_precondition)?;
        let tab = |turns::OtherTab { pane, name }| OtherTab { pane, name };
        Ok(Response::new(OtherTabsResponse { tabs: tabs.into_iter().map(tab).collect() }))
    }

    async fn turn_into_tab(
        &self,
        request: Request<TurnIntoTabRequest>,
    ) -> Result<Response<TurnIntoTabResponse>, Status> {
        let TurnIntoTabRequest { on, session, pane, started_at } = request.into_inner();
        let session = named("session", &session)?;
        let started = started_of(started_at)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, started_at) = (session.to_string(), Some(started.into()));
            let asked = TurnIntoTabRequest { on: None, session, pane, started_at };
            return daemon.turn_into_tab(asked).await;
        }
        let came = task::spawn_blocking(move || turns::into_tab(&session, started, pane)).await;
        let came = came.map_err(|error| unfinished(&error))?;
        let came = match came.map_err(Status::failed_precondition)? {
            IntoTab::Done => turn_into_tab_response::Came::Done(()),
            IntoTab::NoClient => turn_into_tab_response::Came::NoClient(()),
        };
        Ok(Response::new(TurnIntoTabResponse { came: Some(came) }))
    }

    async fn turn_into_panes(
        &self,
        request: Request<TurnIntoPanesRequest>,
    ) -> Result<Response<TurnIntoPanesResponse>, Status> {
        let TurnIntoPanesRequest { on, session, pane, beside, started_at } = request.into_inner();
        let session = named("session", &session)?;
        let started = started_of(started_at)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, started_at) = (session.to_string(), Some(started.into()));
            let asked = TurnIntoPanesRequest { on: None, session, pane, beside, started_at };
            return daemon.turn_into_panes(asked).await;
        }
        let came =
            task::spawn_blocking(move || turns::into_panes(&session, started, pane, beside)).await;
        let came = came.map_err(|error| unfinished(&error))?;
        let came = match came.map_err(Status::failed_precondition)? {
            IntoPanes::Done => turn_into_panes_response::Came::Done(()),
            IntoPanes::Partly { moved, of, why } => {
                let partly = turn_into_panes_response::Partly { moved, of, why };
                turn_into_panes_response::Came::Partly(partly)
            },
        };
        Ok(Response::new(TurnIntoPanesResponse { came: Some(came) }))
    }

    async fn kill(&self, request: Request<KillRequest>) -> Result<Response<()>, Status> {
        let KillRequest { on, session, started_at } = request.into_inner();
        let session = named("session", &session)?;
        let started = started_of(started_at)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, started_at) = (session.to_string(), Some(started.into()));
            return daemon.kill(KillRequest { on: None, session, started_at }).await;
        }
        let killed = task::spawn_blocking(move || closes::killed(&session, started));
        let killed = killed.await;
        let killed = killed.map_err(|error| unfinished(&error))?;
        killed.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
    }

    async fn list_exited(
        &self,
        request: Request<ListExitedRequest>,
    ) -> Result<Response<ListExitedResponse>, Status> {
        let ListExitedRequest { on } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.list_exited(ListExitedRequest { on: None }).await;
        }
        let listed = task::spawn_blocking(Zellij::exited).await;
        let listed = listed.map_err(|error| unfinished(&error))?;
        let listed = listed.map_err(Status::failed_precondition)?;
        let exited = |mux::Exited { name, ago }| {
            Some(Exited { name: name.to_string(), ago: Some(ago.try_into().ok()?) })
        };
        let read = listed.len();
        let exited: Vec<Exited> = listed.into_iter().filter_map(exited).collect();
        let left_out = read - exited.len();
        if left_out > 0 {
            log::say(&format!("left out {left_out} kept sessions of an age no Duration holds"));
        }
        Ok(Response::new(ListExitedResponse { exited }))
    }

    async fn delete_exited(
        &self,
        request: Request<DeleteExitedRequest>,
    ) -> Result<Response<()>, Status> {
        let DeleteExitedRequest { on, session } = request.into_inner();
        let session = named("session", &session)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let session = session.to_string();
            return daemon.delete_exited(DeleteExitedRequest { on: None, session }).await;
        }
        let deleted = task::spawn_blocking(move || closes::deleted(&session)).await;
        let deleted = deleted.map_err(|error| unfinished(&error))?;
        deleted.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
    }

    async fn rename_session(
        &self,
        request: Request<RenameSessionRequest>,
    ) -> Result<Response<()>, Status> {
        let RenameSessionRequest { on, session, to } = request.into_inner();
        let session = named("session", &session)?;
        let to = SessionName::parse(&to)
            .map_err(|why| Status::invalid_argument(format!("to: {why}")))?;
        if let Some(mut daemon) = self.bridged(on)? {
            let (session, to) = (session.to_string(), to.to_string());
            return daemon.rename_session(RenameSessionRequest { on: None, session, to }).await;
        }
        let renamed = move || Mux::Zellij(&session).renamed(&to);
        let renamed = task::spawn_blocking(renamed).await;
        let renamed = renamed.map_err(|error| unfinished(&error))?;
        renamed.map_err(Status::failed_precondition)?;
        Ok(Response::new(()))
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
        let steps = talked(move |wanted, hear| asked.talk(wanted, hear));
        Ok(Response::new(Box::pin(steps)))
    }

    type PasteStream = Pin<Box<dyn Stream<Item = Result<PasteResponse, Status>> + Send>>;

    async fn say(&self, request: Request<SayRequest>) -> Result<Response<()>, Status> {
        let say = request.into_inner().say.and_then(|say| say.does);
        let say = say.ok_or_else(|| Status::invalid_argument("it says nothing to do"))?;
        let said = ToSay::parse(say).map_err(Status::invalid_argument)?;
        let at = SystemTime::now();
        let taken = self.ask(|answer| Command::Say { said, at, answer }).await?;
        taken.map_err(Status::invalid_argument)?;
        Ok(Response::new(()))
    }

    async fn paste(
        &self,
        request: Request<PasteRequest>,
    ) -> Result<Response<Self::PasteStream>, Status> {
        let (on, pasted) = Pasted::of(request.into_inner()).map_err(Status::invalid_argument)?;
        let unasked = |why: Status| Status::failed_precondition(why.message());
        if let Some(mut daemon) = self.bridged(on).map_err(unasked)? {
            let answer = daemon.paste(pasted.asked(None)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let answer = late(move || pasted.answer().map_err(unready));
        Ok(Response::new(Box::pin(answer)))
    }

    type PrunesStream = Pin<Box<dyn Stream<Item = Result<PrunesResponse, Status>> + Send>>;

    async fn prunes(
        &self,
        request: Request<PrunesRequest>,
    ) -> Result<Response<Self::PrunesStream>, Status> {
        let PrunesRequest { on, dir } = request.into_inner();
        let dir = directory("dir", dir)?;
        if let Some(mut daemon) = self.bridged(on)? {
            let answer = daemon.prunes(prunes::asked(None, &dir)).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let host = self.watched(None).await?;
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
        if let Some(mut daemon) = self.bridged(on)? {
            let told = PruneRequest { on: None, dir: dir.into(), agreed };
            let answer = daemon.prune(told).await?.into_inner();
            return Ok(Response::new(Box::pin(answer)));
        }
        let agreed: BTreeSet<prunes::Deleting> = agreed.into_iter().map(Into::into).collect();
        let core = self.core.clone();
        let here = env::current_dir().ok();
        let answer = late(move || {
            let occupied = || {
                let host = core::ask(&core, |answer| Command::Watch { after: None, answer });
                let host = host.flatten().ok_or("lunetad's core did not say")?;
                Occupied::of(&host)
            };
            let pruned = prunes::deleted_as_agreed(&dir, &agreed, occupied, here.as_deref())
                .map_err(Status::failed_precondition)?;
            Ok(PruneResponse::from(&pruned))
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
        if let Some(mut daemon) = self.bridged(on).map_err(unreached)? {
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
        if let Some(mut daemon) = self.bridged(on)? {
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
        use crate::mux::own::TerminalKey;
        let mut heard = request.into_inner();
        let Some(ShowRequest { event: Some(Event::Hello(Hello { workstream, size, runs_in })) }) =
            heard.message().await?
        else {
            return Err(Status::invalid_argument("the first word is no hello"));
        };
        let workstream = views::id("workstream", workstream).map(WorkstreamId::new);
        let workstream = workstream.map_err(Status::invalid_argument)?;
        let size = views::size_of(size).map_err(Status::invalid_argument)?;
        let runs_in = runs_in.as_ref().map(TerminalKey::of).transpose();
        let runs_in = runs_in.map_err(|why| Status::invalid_argument(format!("runs_in: {why}")))?;
        let (out, answers) = tokio::sync::mpsc::channel(views::ANSWERS_MAX);
        let views = self.views.clone();
        let came = task::spawn_blocking(move || {
            let came = |answer| views::Command::Came { workstream, size, runs_in, out, answer };
            views::ask(&views, came)
        });
        let came = came.await.map_err(|error| unfinished(&error))?;
        let client = came.ok_or_else(|| Status::unavailable("the views have stopped"))??;
        let views = self.views.clone();
        tokio::spawn(async move {
            loop {
                let asked = match heard.message().await {
                    Ok(Some(ShowRequest { event: Some(event) })) => views::Asked::of_api(event),
                    Ok(Some(ShowRequest { event: None })) => Err("a word says nothing".to_owned()),
                    Ok(None) | Err(_) => break,
                };
                let views = views.clone();
                let told = task::spawn_blocking(move || {
                    views.send(views::Command::Asked { client, asked }).is_ok()
                });
                if !matches!(told.await, Ok(true)) {
                    return;
                }
            }
            let _ = task::spawn_blocking(move || views.send(views::Command::Left(client))).await;
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(answers))))
    }

    async fn change_projects(
        &self,
        request: Request<ChangeProjectsRequest>,
    ) -> Result<Response<ChangeProjectsResponse>, Status> {
        let ChangeProjectsRequest { on, change } = request.into_inner();
        if let Some(mut daemon) = self.bridged(on)? {
            return daemon.change_projects(ChangeProjectsRequest { on: None, change }).await;
        }
        let change = change.ok_or_else(|| Status::invalid_argument("it says nothing to change"))?;
        let views = self.views.clone();
        let changed = task::spawn_blocking(move || -> Result<Option<Id>, Status> {
            let change = Change::of_api(change).map_err(Status::invalid_argument)?;
            let stopped = "it is kept, but the daemon's core has stopped";
            let asked = views::ask(&views, |answer| views::Command::Change { change, answer });
            let changed = asked.ok_or_else(|| Status::unavailable("the views have stopped"))?;
            changed.map_err(|failed| match failed {
                Failed::NotKept(why) => Status::failed_precondition(why),
                Failed::Nests(why) => Status::already_exists(why),
                Failed::Refused(why) => Status::invalid_argument(why),
                Failed::Unwritten(why) => Status::internal(why),
                Failed::KeptUntold => Status::unavailable(stopped),
            })
        });
        let made = changed.await.map_err(|error| unfinished(&error))??;
        Ok(Response::new(ChangeProjectsResponse { made: made.map(Id::get) }))
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
        if let Some(mut daemon) = self.bridged(on)? {
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
    let runtime = folder::find().ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to keep it in")?;
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

fn clients_of(session: &SessionName) -> Result<Vec<api::Client>, String> {
    let listed = Mux::Zellij(session).clients()?;
    Ok(listed.iter().copied().map(mux::Client::to_api).collect())
}

fn started_of<At: TryInto<SystemTime>>(started_at: Option<At>) -> Result<SystemTime, Status> {
    started_at.and_then(|at| at.try_into().ok()).ok_or_else(|| {
        Status::invalid_argument("started_at: it says no time the session started at")
    })
}

fn named(field: &str, name: &str) -> Result<SessionName, Status> {
    SessionName::running(name).map_err(|why| Status::invalid_argument(format!("{field}: {why}")))
}

fn directory(field: &str, dir: String) -> Result<Directory, Status> {
    Directory::parse(dir).map_err(|why| Status::invalid_argument(format!("{field}: {why}")))
}

fn asked_open(request: OpenRequest) -> Result<(Option<api::On>, Open), Status> {
    let OpenRequest { on, session, cwd, command, opens } = request;
    let (session, cwd) = (named("session", &session)?, directory("cwd", cwd)?);
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

fn unfinished(error: &task::JoinError) -> Status {
    Status::internal(format!("lunetad lost its answer: {error}"))
}

impl Host {
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
