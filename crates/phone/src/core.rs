use std::future::Future;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::num::NonZeroU32;
use std::panic;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use lune_protocol::said::Said;
use lune_protocol::turn::Turn;
use lune_protocol::watch::{HostVersion, VersionedHost, WatchedAfter};
use tokio::net::UnixStream;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, timeout, timeout_at, Instant};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Response, Status, Streaming};
use tower::service_fn;

use crate::api::change_projects_request::{
    Change, CloseView, CloseWorktree, RenameProject, RenameTerminal, RenameWorktree,
    ReorderProjects, ReorderWorktrees,
};
use crate::api::daemon_client::DaemonClient;
use crate::api::{self, On};
use crate::model::{Model, TerminalKey};
use crate::terminal::{Appearance, TerminalView};
use crate::unanswered::{Refusals, Unanswered};
use crate::verbs::{
    Answer, Filed, Goes, Heard, IssueRepository, Landed, Listed, Marked, Order, Plan, Pruned,
    Rebased, Screen, Step, Waiting,
};
use crate::waits::{self, positive, NonZeroDuration};

type Daemon = DaemonClient<Channel>;

const CONNECT: Duration = Duration::from_secs(5);

const QUIET: Duration = Duration::from_secs(10);

const PONG: Duration = Duration::from_secs(10);

const WORKERS: usize = 2;

const STEPS_HELD: usize = 16;

pub const TACKLE: &str = "Tackle these";

#[uniffi::export(foreign)]
pub trait Observer: Send + Sync {
    fn changed(&self, model: Model);
    fn lost(&self, why: String);
}

struct Workers(Option<Runtime>);

impl Drop for Workers {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

pub type Backoff = fn(failed: NonZeroU32, chance: u64) -> NonZeroDuration;

#[derive(Clone)]
struct Wake(Arc<watch::Sender<()>>);

impl Wake {
    fn new() -> Wake {
        let (tell, _) = watch::channel(());
        Wake(Arc::new(tell))
    }

    fn signal(&self) {
        self.0.send_replace(());
    }

    fn subscribe(&self) -> watch::Receiver<()> {
        self.0.subscribe()
    }
}

fn reached(status: &Status) -> bool {
    matches!(
        status.code(),
        Code::InvalidArgument
            | Code::FailedPrecondition
            | Code::NotFound
            | Code::AlreadyExists
            | Code::Aborted
            | Code::OutOfRange
            | Code::PermissionDenied
            | Code::Unimplemented
            | Code::Unauthenticated
    )
}

#[derive(Clone)]
pub(crate) struct Rpc {
    daemon: Daemon,
    patience: NonZeroDuration,
    wake: Wake,
}

impl Rpc {
    fn new(daemon: Daemon, patience: NonZeroDuration) -> Rpc {
        Rpc { daemon, patience, wake: Wake::new() }
    }

    fn for_watch(&self) -> (Daemon, NonZeroDuration, watch::Receiver<()>) {
        (self.daemon.clone(), self.patience, self.wake.subscribe())
    }

    fn patience(&self) -> NonZeroDuration {
        self.patience
    }

    pub(crate) async fn unary<T, F>(
        &self,
        refusals: Refusals,
        ask: impl FnOnce(Daemon) -> F,
    ) -> Result<T, Unanswered>
    where
        F: Future<Output = Result<Response<T>, Status>>,
    {
        match timeout(self.patience.get(), ask(self.daemon.clone())).await {
            Ok(Ok(answer)) => {
                self.wake.signal();
                Ok(answer.into_inner())
            },
            Ok(Err(status)) => {
                if reached(&status) {
                    self.wake.signal();
                }
                Err(Unanswered::of(&status, refusals))
            },
            Err(_) => Err(Unanswered::late(self.patience.get())),
        }
    }

    async fn first<T, F>(
        &self,
        works: NonZeroDuration,
        refusals: Refusals,
        ask: impl FnOnce(Daemon) -> F,
    ) -> Result<Option<T>, Unanswered>
    where
        F: Future<Output = Result<Response<Streaming<T>>, Status>>,
    {
        let mut answering = self.unary(refusals, ask).await?;
        let within = self.patience.saturating_add(works);
        match timeout(within.get(), answering.message()).await {
            Ok(Ok(said)) => Ok(said),
            Ok(Err(status)) => Err(Unanswered::of(&status, refusals)),
            Err(_) => Err(Unanswered::late(within.get())),
        }
    }
}

#[derive(uniffi::Object)]
pub struct Core {
    rpc: Rpc,
    handle: Handle,
    backoff: Backoff,
    watching: Mutex<Option<JoinHandle<()>>>,
    _workers: Workers,
}

impl Drop for Core {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Rest {
    Fresh,
    Idle { session: String, read: Option<u64> },
}

impl Rest {
    fn after(session: Option<String>, read: Option<u64>) -> Rest {
        match session {
            Some(session) => Rest::Idle { session, read },
            None => Rest::Fresh,
        }
    }

    fn session(&self) -> Option<&str> {
        match self {
            Rest::Fresh => None,
            Rest::Idle { session, .. } => Some(session),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    Read,
    Refused,
    Unread,
}

impl Ending {
    fn of(unanswered: &Unanswered) -> Ending {
        match unanswered {
            Unanswered::Refused { .. } => Ending::Refused,
            Unanswered::InDoubt { .. } => Ending::Unread,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Talked {
    At(Rest),
    Running { turn: u64, was: Rest, session: Option<String>, heard: bool },
}

impl Talked {
    fn begin(&mut self) -> Result<(Turn, Option<String>), Unanswered> {
        let Talked::At(rest) = self else {
            let why = "a turn of this conversation still runs: read it to its end first";
            return Err(Unanswered::refused(why));
        };
        let was = rest.clone();
        let (resumed, before) = match &was {
            Rest::Fresh => (None, None),
            Rest::Idle { session, read } => (Some(session.clone()), *read),
        };
        let turn = Turn::after(before);
        *self = Talked::Running { turn: turn.key(), was, session: None, heard: false };
        Ok((turn, resumed))
    }

    fn heard(&mut self, of: u64, named: Option<String>) {
        if let Talked::Running { turn, session, heard, .. } = self
            && *turn == of
        {
            *heard = true;
            if named.is_some() {
                *session = named;
            }
        }
    }

    fn end(&mut self, of: u64, ending: Ending) {
        let Talked::Running { turn, was, session, heard } = self else {
            return;
        };
        if *turn != of {
            return;
        }
        let session = session.take().or_else(|| was.session().map(str::to_owned));
        let rest = match ending {
            Ending::Refused if !*heard => was.clone(),
            Ending::Read => Rest::after(session, Some(of)),
            Ending::Refused | Ending::Unread => Rest::after(session, None),
        };
        *self = Talked::At(rest);
    }

    fn session(&self) -> Option<&str> {
        match self {
            Talked::At(rest) => rest.session(),
            Talked::Running { was, session, .. } => session.as_deref().or_else(|| was.session()),
        }
    }
}

fn held<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn on(on: Option<String>) -> Option<On> {
    on.map(|destination| On { destination })
}

#[uniffi::export]
pub fn tailnet_name(named: &str) -> Result<String, Unanswered> {
    let host = named.trim();
    let odd = |char: char| char.is_whitespace() || "/@?#".contains(char);
    if host.is_empty() || host.contains(odd) {
        return Err(Unanswered::refused(format!("{host:?} names no machine")));
    }
    Ok(host.to_owned())
}

fn tailnet(named: &str) -> Result<Endpoint, Unanswered> {
    let host = tailnet_name(named)?;
    let named = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.clone()
    };
    let uri = format!("http://{named}:{}", lune_protocol::TAILNET_PORT);
    let endpoint = Endpoint::from_shared(uri).map_err(|error| Unanswered::refused(format!("{host:?}: {error}")))?;
    Ok(endpoint
        .connect_timeout(CONNECT)
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(PONG)
        .keep_alive_while_idle(false))
}

fn watch_failed(unanswered: &Unanswered) -> Said {
    Said::new(unanswered.why().to_owned()).expect("unanswered reasons from unary are non-empty")
}

fn unreadable(why: &str) -> Said {
    Said::new(format!("{why}, which the core does not read")).expect("loss literal is non-empty")
}

enum WatchCall<T> {
    Answered(T),
    Unimplemented,
    Failed(Said),
}

async fn watch_at<T>(
    deadline: Instant,
    within: NonZeroDuration,
    asked: impl Future<Output = Result<Response<T>, Status>>,
) -> WatchCall<T> {
    match timeout_at(deadline, asked).await {
        Ok(Ok(answer)) => WatchCall::Answered(answer.into_inner()),
        Ok(Err(status)) if status.code() == Code::Unimplemented => WatchCall::Unimplemented,
        Ok(Err(status)) => {
            WatchCall::Failed(watch_failed(&Unanswered::of(&status, Refusals::Others)))
        },
        Err(_) => WatchCall::Failed(watch_failed(&Unanswered::late(within.get()))),
    }
}

async fn watch_legacy_at(
    deadline: Instant,
    within: NonZeroDuration,
    daemon: &mut Daemon,
    after: Option<HostVersion>,
) -> Result<VersionedHost, Said> {
    let asked = api::WatchRequest {
        after: after.map(HostVersion::to_api),
        kept: None,
        shown: true,
    };
    let host = match timeout_at(deadline, daemon.watch(asked)).await {
        Ok(Ok(answer)) => answer.into_inner(),
        Ok(Err(status)) => return Err(watch_failed(&Unanswered::of(&status, Refusals::Others))),
        Err(_) => return Err(watch_failed(&Unanswered::late(within.get()))),
    };
    VersionedHost::of_api(Box::new(host)).map_err(unreadable)
}

impl Core {
    fn of(
        runtime: Runtime,
        patience: NonZeroDuration,
        backoff: Backoff,
        channel: impl FnOnce() -> Channel,
    ) -> Arc<Core> {
        let handle = runtime.handle().clone();
        let channel = {
            let _entered = runtime.enter();
            channel()
        };
        let daemon = DaemonClient::new(channel)
            .max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX);
        Arc::new(Core {
            rpc: Rpc::new(daemon, patience),
            handle,
            backoff,
            watching: Mutex::new(None),
            _workers: Workers(Some(runtime)),
        })
    }

    fn runtime() -> Result<Runtime, Unanswered> {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(WORKERS)
            .thread_name("lune-phone")
            .enable_all()
            .build()
            .map_err(|error| Unanswered::refused(format!("no runtime: {error}")))
    }

    pub fn over_socket(
        socket: PathBuf,
        patience: NonZeroDuration,
        backoff: Backoff,
    ) -> Result<Arc<Core>, Unanswered> {
        let connect = move || {
            Endpoint::from_static("http://luned").connect_with_connector_lazy(service_fn(
                move |_: Uri| {
                    let socket = socket.clone();
                    async move { UnixStream::connect(socket).await.map(TokioIo::new) }
                },
            ))
        };
        Ok(Core::of(Core::runtime()?, patience, backoff, connect))
    }

    async fn on_runtime<T: Send + 'static>(
        &self,
        work: impl Future<Output = Result<T, Unanswered>> + Send + 'static,
    ) -> Result<T, Unanswered> {
        match self.handle.spawn(work).await {
            Ok(done) => done,
            Err(ended) if ended.is_panic() => panic::resume_unwind(ended.into_panic()),
            Err(ended) => Err(Unanswered::InDoubt { why: format!("the core's call ended: {ended}") }),
        }
    }

    async fn changed(&self, on: Option<On>, change: Change) -> Result<(), Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::ChangeProjectsRequest { on, change: Some(change) };
        self.on_runtime(async move {
            rpc.unary(Refusals::Changes, |mut daemon| async move {
                daemon.change_projects(request).await
            })
            .await
            .map(drop)
        })
        .await
    }
}

#[uniffi::export]
impl Core {
    #[uniffi::constructor]
    pub fn new(host: &str) -> Result<Arc<Core>, Unanswered> {
        let endpoint = tailnet(host)?;
        Ok(Core::of(
            Core::runtime()?,
            positive::ANSWER_WAIT,
            waits::again,
            move || endpoint.connect_lazy(),
        ))
    }

    pub fn watch(&self, observer: Arc<dyn Observer>) {
        let mut watching = held(&self.watching);
        if let Some(watched) = watching.take() {
            watched.abort();
        }
        let (daemon, patience, woken) = self.rpc.for_watch();
        let watched = watched(daemon, patience, observer, woken, self.backoff);
        *watching = Some(self.handle.spawn(watched));
    }

    pub fn stop(&self) {
        if let Some(watched) = held(&self.watching).take() {
            watched.abort();
        }
    }

    #[must_use]
    pub fn converse(self: Arc<Self>, on: Option<String>, cwd: Option<String>) -> Arc<Conversation> {
        let talked = Mutex::new(Talked::At(Rest::Fresh));
        Arc::new(Conversation { core: self, on, cwd, talked })
    }

    #[must_use]
    pub fn terminal(&self, terminal: &TerminalKey, appearance: Appearance) -> Arc<TerminalView> {
        TerminalView::open(self.rpc.clone(), &self.handle, terminal, appearance)
    }

    pub async fn waiting(&self, terminal: Arc<TerminalKey>) -> Result<Waiting, Unanswered> {
        let rpc = self.rpc.clone();
        let (on, key) = terminal.to_api();
        let request = api::WaitingRequest { on, terminal: Some(key) };
        self.on_runtime(async move {
            let said = rpc
                .unary(Refusals::Others, |mut daemon| async move { daemon.waiting(request).await })
                .await?;
            Waiting::of(terminal, said)
        })
        .await
    }

    pub async fn answer(&self, screen: Screen, answer: Answer) -> Result<(), Unanswered> {
        let rpc = self.rpc.clone();
        let Screen { terminal, shown } = screen;
        let (on, key) = terminal.to_api();
        let request = api::AnswerRequest {
            on,
            terminal: Some(key),
            shown: Some(shown),
            answer: Some(answer.into_api()),
        };
        self.on_runtime(async move {
            rpc.unary(Refusals::Changes, |mut daemon| async move { daemon.answer(request).await })
                .await
                .map(drop)
        })
        .await
    }

    pub async fn new_agent(
        &self,
        on: Option<String>,
        project: u64,
        branch: String,
        words: String,
    ) -> Result<Landed, Unanswered> {
        let rpc = self.rpc.clone();
        let prompted = api::Prompted { harness: api::Harness::ClaudeCode.into(), words };
        let opens = Some(api::Opens { runs: Some(api::opens::Runs::Agent(prompted)) });
        let request = api::MakeWorktreeRequest { on: self::on(on), project, branch, opens };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::MAKING_WAIT, Refusals::Others, |mut daemon| async move {
                    daemon.make_worktree(request).await
                })
                .await?;
            Landed::of(said)
        })
        .await
    }

    pub async fn work(&self, on: Option<String>, project: u64) -> Result<Listed, Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::ListWorkRequest { on: self::on(on), project };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::LISTING_WAIT, Refusals::Others, |mut daemon| async move {
                    daemon.list_work(request).await
                })
                .await?;
            Listed::of(said)
        })
        .await
    }

    pub async fn close_view(&self, terminal: Arc<TerminalKey>) -> Result<(), Unanswered> {
        let (on, key) = terminal.to_api();
        let change = Change::CloseView(CloseView { terminal: Some(key) });
        self.changed(on, change).await
    }

    pub async fn close_worktree(
        &self,
        on: Option<String>,
        worktree: u64,
        seen: Vec<u64>,
    ) -> Result<(), Unanswered> {
        self.changed(self::on(on), Change::CloseWorktree(CloseWorktree { worktree, seen })).await
    }

    pub async fn rename_project(
        &self,
        on: Option<String>,
        project: u64,
        to: String,
    ) -> Result<(), Unanswered> {
        self.changed(self::on(on), Change::RenameProject(RenameProject { project, to })).await
    }

    pub async fn rename_worktree(
        &self,
        on: Option<String>,
        worktree: u64,
        to: Option<String>,
    ) -> Result<(), Unanswered> {
        self.changed(self::on(on), Change::RenameWorktree(RenameWorktree { worktree, to })).await
    }

    pub async fn rename_view(
        &self,
        on: Option<String>,
        view: u64,
        to: Option<String>,
    ) -> Result<(), Unanswered> {
        let change = Change::RenameTerminal(RenameTerminal { terminal: view, to });
        self.changed(self::on(on), change).await
    }

    pub async fn reorder_projects(&self, on: Option<String>, order: Order) -> Result<(), Unanswered> {
        let Order { seen, wanted } = order;
        let change = Change::ReorderProjects(ReorderProjects { projects: wanted, seen });
        self.changed(self::on(on), change).await
    }

    pub async fn reorder_worktrees(
        &self,
        on: Option<String>,
        project: u64,
        order: Order,
    ) -> Result<(), Unanswered> {
        let Order { seen, wanted } = order;
        let reorder = ReorderWorktrees { project, worktrees: wanted, seen };
        self.changed(self::on(on), Change::ReorderWorktrees(reorder)).await
    }

    pub async fn prunes(&self, on: Option<String>, dir: String) -> Result<Plan, Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::PrunesRequest { on: self::on(on), dir };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::PLANNING_WAIT, Refusals::Others, |mut daemon| async move {
                    daemon.prunes(request).await
                })
                .await?;
            said.map(Plan::of).ok_or_else(|| Unanswered::unread("it ended with no plan"))
        })
        .await
    }

    pub async fn prune(
        &self,
        on: Option<String>,
        dir: String,
        agreed: Vec<Goes>,
    ) -> Result<Pruned, Unanswered> {
        let rpc = self.rpc.clone();
        let agreed = agreed.into_iter().map(Goes::deleting).collect();
        let request = api::PruneRequest { on: self::on(on), dir, agreed };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::DELETING_WAIT, Refusals::Others, |mut daemon| async move {
                    daemon.prune(request).await
                })
                .await?;
            Pruned::of(said)
        })
        .await
    }

    pub async fn rebase(
        &self,
        on: Option<String>,
        dir: String,
        branch: Option<String>,
    ) -> Result<Rebased, Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::RebaseRequest { on: self::on(on), dir, branch };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::REBASING_WAIT, Refusals::Others, |mut daemon| async move {
                    daemon.rebase(request).await
                })
                .await?;
            Rebased::of(said)
        })
        .await
    }

    pub async fn issue_repositories(
        &self,
        on: Option<String>,
        dir: String,
    ) -> Result<Vec<IssueRepository>, Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::IssueRepositoriesRequest { on: self::on(on), dir };
        self.on_runtime(async move {
            let listed = rpc
                .unary(Refusals::Others, |mut daemon| async move {
                    daemon.issue_repositories(request).await
                })
                .await?;
            let repository = |it: api::IssueRepository| IssueRepository {
                remote: it.remote,
                repository: it.repository,
            };
            Ok(listed.repositories.into_iter().map(repository).collect())
        })
        .await
    }

    pub async fn file_issue(
        &self,
        on: Option<String>,
        dir: String,
        repository: String,
        words: String,
    ) -> Result<Filed, Unanswered> {
        let rpc = self.rpc.clone();
        let request = api::FileIssueRequest { on: self::on(on), dir, words, repository };
        self.on_runtime(async move {
            let said = rpc
                .first(positive::FILING_WAIT, Refusals::Filings, |mut daemon| async move {
                    daemon.file_issue(request).await
                })
                .await?;
            Filed::of(said)
        })
        .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchMode {
    Changes,
    Legacy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Poll {
    Immediate,
    Long { after: HostVersion, mode: WatchMode },
}

enum PollOutcome {
    Host { host: VersionedHost, mode: WatchMode },
    Unchanged { mode: WatchMode },
}

#[derive(Debug, PartialEq, Eq)]
struct BackedOff {
    why: Said,
    failed: NonZeroU32,
    tell: bool,
}

impl BackedOff {
    fn failed(&self) -> NonZeroU32 {
        self.failed
    }

    fn tell(&self) -> Option<&str> {
        self.tell.then(|| self.why.as_str())
    }

    fn wake(self) -> Watch {
        Watch::Woken { why: self.why }
    }

    fn retry(self) -> Watch {
        Watch::Lost { why: self.why, failed: self.failed }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Watch {
    Fresh,
    Heard { after: HostVersion, mode: WatchMode, shown: Model },
    Lost { why: Said, failed: NonZeroU32 },
    Woken { why: Said },
}

impl Watch {
    fn poll(&self) -> Poll {
        match self {
            Watch::Heard { after, mode, .. } => Poll::Long { after: *after, mode: *mode },
            Watch::Fresh | Watch::Lost { .. } | Watch::Woken { .. } => Poll::Immediate,
        }
    }

    fn heard(self, after: HostVersion, mode: WatchMode, model: Model) -> (Watch, Option<Model>) {
        let changed = match self {
            Watch::Heard { shown, .. } => shown != model,
            Watch::Fresh | Watch::Lost { .. } | Watch::Woken { .. } => true,
        };
        let shown = changed.then(|| model.clone());
        (Watch::Heard { after, mode, shown: model }, shown)
    }

    fn unchanged(self, mode: WatchMode) -> Watch {
        match self {
            Watch::Heard { after, shown, .. } => Watch::Heard { after, mode, shown },
            Watch::Fresh | Watch::Lost { .. } | Watch::Woken { .. } => {
                unreachable!("an unchanged watch follows a long poll")
            },
        }
    }

    fn lost(self, why: Said) -> BackedOff {
        let (tell, failed) = match self {
            Watch::Lost { why: missed, failed } => (missed != why, failed.saturating_add(1)),
            Watch::Woken { why: missed } => (missed != why, NonZeroU32::MIN),
            Watch::Fresh | Watch::Heard { .. } => (true, NonZeroU32::MIN),
        };
        BackedOff { why, failed, tell }
    }
}

async fn poll_watch(
    daemon: &mut Daemon,
    deadline: Instant,
    patience: NonZeroDuration,
    poll: Poll,
) -> Result<PollOutcome, Said> {
    match poll {
        Poll::Immediate => {
            let asked = api::WatchChangesRequest {
                after: None,
                waiter: api::ClientWaiter::Window.into(),
            };
            match watch_at(deadline, patience, daemon.watch_changes(asked)).await {
                WatchCall::Answered(response) => {
                    let host = response.read_host().map_err(unreadable)?;
                    Ok(PollOutcome::Host { host, mode: WatchMode::Changes })
                },
                WatchCall::Unimplemented => {
                    let host = watch_legacy_at(deadline, patience, daemon, None).await?;
                    Ok(PollOutcome::Host { host, mode: WatchMode::Legacy })
                },
                WatchCall::Failed(why) => Err(why),
            }
        },
        Poll::Long { after, mode: WatchMode::Changes } => {
            let asked = api::WatchChangesRequest {
                after: Some(after.to_api()),
                waiter: api::ClientWaiter::Window.into(),
            };
            match watch_at(deadline, patience, daemon.watch_changes(asked)).await {
                WatchCall::Answered(response) => match response.read_after(after).map_err(unreadable)? {
                    WatchedAfter::Host(host) => {
                        Ok(PollOutcome::Host { host, mode: WatchMode::Changes })
                    },
                    WatchedAfter::Unchanged => Ok(PollOutcome::Unchanged { mode: WatchMode::Changes }),
                },
                WatchCall::Unimplemented => {
                    let host = watch_legacy_at(deadline, patience, daemon, Some(after)).await?;
                    if host.version() == after {
                        Ok(PollOutcome::Unchanged { mode: WatchMode::Legacy })
                    } else {
                        Ok(PollOutcome::Host { host, mode: WatchMode::Legacy })
                    }
                },
                WatchCall::Failed(why) => Err(why),
            }
        },
        Poll::Long { after, mode: WatchMode::Legacy } => {
            let host = watch_legacy_at(deadline, patience, daemon, Some(after)).await?;
            if host.version() == after {
                Ok(PollOutcome::Unchanged { mode: WatchMode::Legacy })
            } else {
                Ok(PollOutcome::Host { host, mode: WatchMode::Legacy })
            }
        },
    }
}

async fn watched(
    mut daemon: Daemon,
    patience: NonZeroDuration,
    observer: Arc<dyn Observer>,
    mut woken: watch::Receiver<()>,
    backoff: Backoff,
) {
    let mut watch = Watch::Fresh;
    loop {
        let asked_at = Instant::now();
        let deadline = asked_at + patience.get();
        let poll = watch.poll();
        if matches!(poll, Poll::Immediate) {
            woken.mark_unchanged();
        }
        let why = match poll_watch(&mut daemon, deadline, patience, poll).await {
            Ok(PollOutcome::Host { host, mode }) => {
                let (next, model) = watch.heard(host.version(), mode, Model::of(host.host()));
                watch = next;
                if let Some(model) = model {
                    observer.changed(model);
                }
                sleep_until(asked_at + waits::PACE).await;
                continue;
            },
            Ok(PollOutcome::Unchanged { mode }) => {
                watch = watch.unchanged(mode);
                sleep_until(asked_at + waits::PACE).await;
                continue;
            },
            Err(why) => why,
        };
        if matches!(poll, Poll::Long { .. }) {
            woken.mark_unchanged();
        }
        let backed_off = watch.lost(why);
        if let Some(why) = backed_off.tell() {
            observer.lost(why.to_owned());
        }
        let wait = backoff(backed_off.failed(), chance());
        let woke = tokio::select! {
            biased;
            heard = woken.changed() => match heard {
                Ok(()) => true,
                Err(_) => return,
            },
            () = tokio::time::sleep(wait.get()) => false,
        };
        watch = if woke { backed_off.wake() } else { backed_off.retry() };
    }
}

fn chance() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[derive(uniffi::Object)]
pub struct Conversation {
    core: Arc<Core>,
    on: Option<String>,
    cwd: Option<String>,
    talked: Mutex<Talked>,
}

impl Conversation {
    fn asked(self: Arc<Self>, words: String, marked: Marked) -> Result<Arc<Asking>, Unanswered> {
        let (turn, resumed) = held(&self.talked).begin()?;
        let request = api::AskRequest {
            on: on(self.on.clone()),
            cwd: self.cwd.clone(),
            words,
            resumed,
            highlighted: String::new(),
            turn: turn.key(),
            before: turn.before(),
            marked_issues: marked.issues,
            marked_pulls: marked.pulls,
            review_only: false,
        };
        let (tell, told) = mpsc::channel(STEPS_HELD);
        let talk = Talk { rpc: self.core.rpc.clone(), tell };
        self.core.handle.spawn(talk.steps(request));
        let listening = tokio::sync::Mutex::new(Listening { told, over: false });
        Ok(Arc::new(Asking { listening, conversation: self, turn: turn.key() }))
    }
}

#[uniffi::export]
impl Conversation {
    pub fn ask(self: Arc<Self>, words: String) -> Result<Arc<Asking>, Unanswered> {
        self.asked(words, Marked::none())
    }

    pub fn tackle(self: Arc<Self>, marked: Marked) -> Result<Arc<Asking>, Unanswered> {
        if marked.is_empty() {
            return Err(Unanswered::refused("nothing is marked to tackle"));
        }
        self.asked(TACKLE.to_owned(), marked)
    }

    #[must_use]
    pub fn resumes(&self) -> bool {
        held(&self.talked).session().is_some()
    }
}

enum Told {
    Session(String),
    Step(Step),
    Ended,
    Failed(Unanswered),
}

struct Talk {
    rpc: Rpc,
    tell: mpsc::Sender<Told>,
}

impl Talk {
    async fn steps(self, request: api::AskRequest) {
        let within = self.rpc.patience().saturating_add(positive::TURN_WAIT);
        let deadline = Instant::now() + within.get();
        let asked = tokio::select! {
            () = self.tell.closed() => return,
            asked = self.rpc.unary(Refusals::Others, |mut daemon| async move {
                daemon.ask(request).await
            }) => asked,
        };
        let mut steps = match asked {
            Ok(steps) => steps,
            Err(unanswered) => return self.last(Told::Failed(unanswered)).await,
        };
        loop {
            let said = tokio::select! {
                () = self.tell.closed() => return,
                said = timeout_at(deadline, steps.message()) => said,
            };
            let said = match said {
                Ok(Ok(Some(said))) => Heard::of(said),
                Ok(Ok(None)) => return self.last(Told::Ended).await,
                Ok(Err(status)) => Err(Unanswered::of(&status, Refusals::Others)),
                Err(_) => Err(Unanswered::late(within.get())),
            };
            let told = match said {
                Ok(Heard::Session(session)) => Told::Session(session),
                Ok(Heard::Step(step)) => Told::Step(step),
                Err(unanswered) => return self.last(Told::Failed(unanswered)).await,
            };
            if self.tell.send(told).await.is_err() {
                return;
            }
        }
    }

    async fn last(self, told: Told) {
        let told = self.tell.send(told).await;
        assert!(told.is_ok() || self.tell.is_closed());
    }
}

struct Listening {
    told: mpsc::Receiver<Told>,
    over: bool,
}

#[derive(uniffi::Object)]
pub struct Asking {
    listening: tokio::sync::Mutex<Listening>,
    conversation: Arc<Conversation>,
    turn: u64,
}

impl Asking {
    fn talked(&self) -> MutexGuard<'_, Talked> {
        held(&self.conversation.talked)
    }
}

#[uniffi::export]
impl Asking {
    pub async fn next(&self) -> Result<Option<Step>, Unanswered> {
        let mut listening = self.listening.lock().await;
        loop {
            if listening.over {
                return Ok(None);
            }
            let ended = match listening.told.recv().await {
                Some(Told::Session(session)) => {
                    self.talked().heard(self.turn, Some(session));
                    continue;
                },
                Some(Told::Step(step)) => {
                    self.talked().heard(self.turn, None);
                    return Ok(Some(step));
                },
                Some(Told::Ended) => Ok(None),
                Some(Told::Failed(unanswered)) => Err(unanswered),
                None => {
                    let why = "the core's talk ended before the turn did".to_owned();
                    Err(Unanswered::InDoubt { why })
                },
            };
            listening.over = true;
            let ending = ended.as_ref().map_or_else(Ending::of, |_| Ending::Read);
            self.talked().end(self.turn, ending);
            return ended;
        }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        self.talked().end(self.turn, Ending::Unread);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_is_reached_on_lune_s_port_of_the_tailnet() {
        let uri = |host: &str| tailnet(host).map(|endpoint| endpoint.uri().to_string());
        assert_eq!(uri(" desktop ").unwrap(), "http://desktop:7473/");
        assert_eq!(uri("100.101.2.3").unwrap(), "http://100.101.2.3:7473/");
        assert_eq!(uri("fd7a:115c:a1e0::1").unwrap(), "http://[fd7a:115c:a1e0::1]:7473/");
        for odd in ["", "me@desktop", "desktop/x", "two words"] {
            assert!(matches!(tailnet(odd), Err(Unanswered::Refused { .. })), "{odd:?}");
        }
        assert_eq!(tailnet_name(" desktop\n"), Ok("desktop".to_owned()), "the name the core reaches");
    }

    fn idle(session: &str, read: Option<u64>) -> Talked {
        Talked::At(Rest::Idle { session: session.to_owned(), read })
    }

    #[test]
    fn a_turn_is_the_one_before_only_once_it_was_read_to_its_end() {
        let mut talked = Talked::At(Rest::Fresh);
        let (first, resumed) = talked.begin().unwrap();
        assert_eq!((first.before(), resumed), (None, None));
        talked.heard(first.key(), Some("7c22".to_owned()));
        talked.end(first.key(), Ending::Read);
        assert_eq!(talked, idle("7c22", Some(first.key())));
        let (read, resumed) = talked.begin().unwrap();
        assert_eq!((read.before(), resumed.as_deref()), (Some(first.key()), Some("7c22")));
        talked.end(read.key(), Ending::Unread);
        let (unread, _) = talked.begin().unwrap();
        assert_eq!(unread.before(), None, "the turn left unread is none before, nor the one before it");
    }

    #[test]
    fn a_conversation_takes_one_turn_at_a_time_and_an_old_turn_ends_none_after_it() {
        let mut talked = Talked::At(Rest::Fresh);
        let (first, _) = talked.begin().unwrap();
        assert!(matches!(talked.begin(), Err(Unanswered::Refused { .. })));
        talked.heard(first.key(), Some("7c22".to_owned()));
        talked.end(first.key(), Ending::Unread);
        let (second, resumed) = talked.begin().unwrap();
        assert_eq!(resumed.as_deref(), Some("7c22"), "a turn left unread still names its session");
        talked.end(first.key(), Ending::Read);
        assert!(matches!(talked, Talked::Running { turn, .. } if turn == second.key()));
        assert_eq!(talked.session(), Some("7c22"));
    }

    #[test]
    fn a_turn_refused_before_luned_said_anything_leaves_the_conversation_as_it_was() {
        let mut talked = idle("7c22", Some(5));
        let (refused, _) = talked.begin().unwrap();
        talked.end(refused.key(), Ending::Refused);
        assert_eq!(talked, idle("7c22", Some(5)));
        let (heard, _) = talked.begin().unwrap();
        assert_eq!(heard.before(), Some(5));
        talked.heard(heard.key(), Some("7c22".to_owned()));
        talked.end(heard.key(), Ending::Refused);
        assert_eq!(talked, idle("7c22", None), "a turn luned began was a turn, refused or not");
        let mut fresh = Talked::At(Rest::Fresh);
        let (first, _) = fresh.begin().unwrap();
        fresh.end(first.key(), Ending::Read);
        assert_eq!(fresh, Talked::At(Rest::Fresh), "a turn that named no session has none to resume");
    }

    #[test]
    fn a_conversation_tackles_nothing_when_nothing_is_marked() {
        let conversation = Core::new("desktop").unwrap().converse(None, None);
        let nothing = conversation.clone().tackle(Marked::none());
        assert!(matches!(nothing, Err(Unanswered::Refused { .. })), "{:?}", nothing.err());
        assert_eq!(*held(&conversation.talked), Talked::At(Rest::Fresh), "and starts no turn");
    }

    fn said(why: &str) -> Said {
        Said::new(why.to_owned()).unwrap()
    }

    #[test]
    fn a_woken_watch_starts_its_backoff_over_without_repeating_the_same_loss() {
        let version = |seq| HostVersion::new(NonZeroU32::new(1).unwrap(), seq);
        let model = Model { here: crate::model::Held::Untold, far: Vec::new(), inbox: Vec::new() };
        let watch = Watch::Fresh;
        assert_eq!(watch.poll(), Poll::Immediate);
        let (watch, shown) = watch.heard(version(1), WatchMode::Changes, model.clone());
        assert_eq!(shown, Some(model.clone()));
        assert_eq!(watch.poll(), Poll::Long { after: version(1), mode: WatchMode::Changes });
        let watch = watch.unchanged(WatchMode::Legacy);
        assert_eq!(watch.poll(), Poll::Long { after: version(1), mode: WatchMode::Legacy });
        let (watch, shown) = watch.heard(version(2), WatchMode::Legacy, model.clone());
        assert_eq!(shown, None, "an unchanged model is not told twice");
        assert_eq!(watch.poll(), Poll::Long { after: version(2), mode: WatchMode::Legacy });

        let first = watch.lost(said("down"));
        assert_eq!((first.tell(), first.failed().get()), (Some("down"), 1));
        let watch = first.retry();
        assert_eq!(watch.poll(), Poll::Immediate);
        let second = watch.lost(said("down"));
        assert_eq!((second.tell(), second.failed().get()), (None, 2));
        let third = second.retry().lost(said("gone"));
        assert_eq!((third.tell(), third.failed().get()), (Some("gone"), 3));

        let watch = third.wake();
        assert_eq!(watch.poll(), Poll::Immediate);
        let reset = watch.lost(said("gone"));
        assert_eq!((reset.tell(), reset.failed().get()), (None, 1), "same loss is not repeated, backoff starts over");
        let other = reset.wake().lost(said("other"));
        assert_eq!((other.tell(), other.failed().get()), (Some("other"), 1));
        let (watch, shown) = other.wake().heard(version(3), WatchMode::Changes, model.clone());
        assert_eq!(
            shown,
            Some(model),
            "even the same model as before brings Tree and Inbox back",
        );
        assert_eq!(watch.poll(), Poll::Long { after: version(3), mode: WatchMode::Changes });
    }

    #[test]
    fn only_an_answer_from_luned_counts_as_having_reached_it() {
        const REACHED: &[Code] = &[
            Code::InvalidArgument,
            Code::FailedPrecondition,
            Code::NotFound,
            Code::AlreadyExists,
            Code::Aborted,
            Code::OutOfRange,
            Code::PermissionDenied,
            Code::Unimplemented,
            Code::Unauthenticated,
        ];
        for code in (0..=16).map(Code::from) {
            assert_eq!(reached(&Status::new(code, "said")), REACHED.contains(&code), "{code:?}");
        }

        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let channel = Endpoint::from_static("http://luned").connect_lazy();
            let rpc = Rpc::new(
                DaemonClient::new(channel),
                NonZeroDuration::new(Duration::from_millis(50)).unwrap(),
            );
            let (_, _, mut woken) = rpc.for_watch();
            assert!(!woken.has_changed().unwrap());

            let unavailable = rpc
                .unary::<(), _>(Refusals::Others, |_| async { Err(Status::unavailable("down")) })
                .await;
            assert!(matches!(unavailable, Err(Unanswered::InDoubt { .. })));
            assert!(!woken.has_changed().unwrap(), "transport failures do not wake");

            let late = rpc
                .unary::<(), _>(Refusals::Others, |_| std::future::pending())
                .await;
            assert!(matches!(late, Err(Unanswered::InDoubt { .. })));
            assert!(!woken.has_changed().unwrap(), "timeouts do not wake");

            let answered = rpc.unary(Refusals::Others, |_| async { Ok(Response::new(())) }).await;
            assert_eq!(answered, Ok(()));
            assert!(woken.has_changed().unwrap(), "ok answers wake");
            woken.mark_unchanged();

            let far_gone = rpc
                .unary::<(), _>(Refusals::Filings, |_| async { Err(Status::not_found("far went")) })
                .await;
            assert!(
                matches!(far_gone, Err(Unanswered::InDoubt { .. })),
                "NotFound under Filings is InDoubt to the caller",
            );
            assert!(
                woken.has_changed().unwrap(),
                "a gRPC NotFound from local luned still proves luned was reached and wakes the watch",
            );
        });
    }
}
