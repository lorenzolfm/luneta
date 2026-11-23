use std::collections::VecDeque;
use std::convert::Infallible;
use std::future::Future;
use std::panic;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, timeout, timeout_at, Instant};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Response, Status, Streaming};
use tower::service_fn;

use crate::api::change_projects_request::{
    Change, CloseView, CloseWorktree, RenameProject, RenameTerminal, RenameWorktree,
    ReorderProjects, ReorderWorktrees,
};
use crate::api::daemon_client::DaemonClient;
use crate::api::{self, On};
use crate::model::{Model, TerminalKey};
use crate::unanswered::Unanswered;
use crate::verbs::{
    Answer, Filed, Goes, IssueRepository, Landed, Listed, Marked, Order, Plan, Pruned, Rebased,
    Step, Waiting,
};
use crate::waits;

type Daemon = DaemonClient<Channel>;

const CONNECT: Duration = Duration::from_secs(5);

const QUIET: Duration = Duration::from_secs(10);

const PONG: Duration = Duration::from_secs(10);

const WORKERS: usize = 2;

const STEPS_HELD: usize = 16;

const TURNS_HELD: usize = 64;

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

#[derive(uniffi::Object)]
pub struct Core {
    daemon: Daemon,
    handle: Handle,
    patience: Duration,
    watching: Mutex<Option<JoinHandle<Infallible>>>,
    turns: Arc<Mutex<Turns>>,
    _workers: Workers,
}

struct Turns(VecDeque<(String, u64)>);

impl Turns {
    fn new() -> Turns {
        Turns(VecDeque::with_capacity(TURNS_HELD))
    }

    fn before(&self, session: &str) -> Option<u64> {
        assert!(self.0.len() <= TURNS_HELD);
        self.0.iter().find(|(held, _)| held == session).map(|&(_, turn)| turn)
    }

    fn record(&mut self, session: String, turn: u64) {
        assert!(self.0.len() <= TURNS_HELD);
        self.0.retain(|(held, _)| *held != session);
        if self.0.len() == TURNS_HELD {
            self.0.pop_front();
        }
        assert!(self.0.len() < TURNS_HELD);
        self.0.push_back((session, turn));
    }
}

struct Wait {
    answer: Duration,
    works: Duration,
}

fn held<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn on(on: Option<String>) -> Option<On> {
    on.map(|destination| On { destination })
}

fn tailnet(host: &str) -> Result<Endpoint, Unanswered> {
    let host = host.trim();
    let odd = |char: char| char.is_whitespace() || "/@?#".contains(char);
    if host.is_empty() || host.contains(odd) {
        return Err(Unanswered::refused(format!("{host:?} names no machine")));
    }
    let named = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let uri = format!("http://{named}:{}", lune_protocol::TAILNET_PORT);
    let endpoint = Endpoint::from_shared(uri).map_err(|error| Unanswered::refused(format!("{host:?}: {error}")))?;
    Ok(endpoint
        .connect_timeout(CONNECT)
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(PONG)
        .keep_alive_while_idle(true))
}

async fn unary<T>(
    within: Duration,
    asked: impl Future<Output = Result<Response<T>, Status>>,
) -> Result<T, Unanswered> {
    match timeout(within, asked).await {
        Ok(Ok(answer)) => Ok(answer.into_inner()),
        Ok(Err(status)) => Err(Unanswered::of(&status)),
        Err(_) => Err(Unanswered::late(within)),
    }
}

async fn first<T>(
    wait: Wait,
    asked: impl Future<Output = Result<Response<Streaming<T>>, Status>>,
) -> Result<Option<T>, Unanswered> {
    let Wait { answer, works } = wait;
    let mut answering = unary(answer, asked).await?;
    let within = answer.saturating_add(works);
    match timeout(within, answering.message()).await {
        Ok(Ok(said)) => Ok(said),
        Ok(Err(status)) => Err(Unanswered::of(&status)),
        Err(_) => Err(Unanswered::late(within)),
    }
}

impl Core {
    fn of(runtime: Runtime, patience: Duration, channel: impl FnOnce() -> Channel) -> Arc<Core> {
        let handle = runtime.handle().clone();
        let channel = {
            let _entered = runtime.enter();
            channel()
        };
        Arc::new(Core {
            daemon: DaemonClient::new(channel),
            handle,
            patience,
            watching: Mutex::new(None),
            turns: Arc::new(Mutex::new(Turns::new())),
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

    pub fn over_socket(socket: PathBuf, patience: Duration) -> Result<Arc<Core>, Unanswered> {
        let connect = move || {
            Endpoint::from_static("http://luned").connect_with_connector_lazy(service_fn(
                move |_: Uri| {
                    let socket = socket.clone();
                    async move { UnixStream::connect(socket).await.map(TokioIo::new) }
                },
            ))
        };
        Ok(Core::of(Core::runtime()?, patience, connect))
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

    async fn changed(&self, on: Option<String>, change: Change) -> Result<(), Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::ChangeProjectsRequest { on: self::on(on), change: Some(change) };
        self.on_runtime(async move { unary(patience, daemon.change_projects(request)).await.map(drop) })
            .await
    }

    fn turn_after(&self, resumed: Option<&str>) -> (u64, Option<u64>) {
        let before = resumed.and_then(|session| held(&self.turns).before(session));
        let key = RandomState::new().build_hasher().finish();
        let key = if before == Some(key) { !key } else { key };
        assert_ne!(Some(key), before);
        (key, before)
    }
}

#[uniffi::export]
impl Core {
    #[uniffi::constructor]
    pub fn new(host: &str) -> Result<Arc<Core>, Unanswered> {
        let endpoint = tailnet(host)?;
        Ok(Core::of(Core::runtime()?, waits::ANSWER, move || endpoint.connect_lazy()))
    }

    pub fn watch(&self, observer: Arc<dyn Observer>) {
        let mut watching = held(&self.watching);
        if let Some(watched) = watching.take() {
            watched.abort();
        }
        let watched = watched(self.daemon.clone(), self.patience, observer);
        *watching = Some(self.handle.spawn(watched));
    }

    pub fn stop(&self) {
        if let Some(watched) = held(&self.watching).take() {
            watched.abort();
        }
    }

    pub fn ask(
        &self,
        on: Option<String>,
        cwd: Option<String>,
        words: String,
        resumed: Option<String>,
        marked: Marked,
    ) -> Arc<Asking> {
        let (turn, before) = self.turn_after(resumed.as_deref());
        let request = api::AskRequest {
            on: self::on(on),
            cwd,
            words,
            resumed,
            highlighted: String::new(),
            turn,
            before,
            marked_issues: marked.issues,
            marked_pulls: marked.pulls,
        };
        let (tell, told) = mpsc::channel(STEPS_HELD);
        let asked = Talk {
            daemon: self.daemon.clone(),
            patience: self.patience,
            turns: self.turns.clone(),
            turn,
            tell,
        };
        self.handle.spawn(asked.steps(request));
        Arc::new(Asking { told: tokio::sync::Mutex::new(told) })
    }

    pub fn tackle(&self, on: Option<String>, dir: String, marked: Marked) -> Arc<Asking> {
        self.ask(on, Some(dir), TACKLE.to_owned(), None, marked)
    }

    pub async fn waiting(
        &self,
        on: Option<String>,
        terminal: TerminalKey,
    ) -> Result<Waiting, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::WaitingRequest { on: self::on(on), terminal: Some(terminal.to_api()) };
        self.on_runtime(async move {
            Waiting::of(unary(patience, daemon.waiting(request)).await?)
        })
        .await
    }

    pub async fn answer(
        &self,
        on: Option<String>,
        terminal: TerminalKey,
        shown: Option<u64>,
        answer: Answer,
    ) -> Result<(), Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::AnswerRequest {
            on: self::on(on),
            terminal: Some(terminal.to_api()),
            shown,
            answer: Some(answer.into_api()),
        };
        self.on_runtime(async move { unary(patience, daemon.answer(request)).await.map(drop) }).await
    }

    pub async fn new_agent(
        &self,
        on: Option<String>,
        project: u64,
        branch: String,
        words: String,
    ) -> Result<Landed, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let prompted = api::Prompted { harness: api::Harness::ClaudeCode.into(), words };
        let opens = Some(api::Opens { runs: Some(api::opens::Runs::Agent(prompted)) });
        let request = api::MakeWorktreeRequest { on: self::on(on), project, branch, opens };
        let wait = Wait { answer: patience, works: waits::MAKING };
        self.on_runtime(async move {
            Landed::of(first(wait, daemon.make_worktree(request)).await?)
        })
        .await
    }

    pub async fn work(&self, on: Option<String>, project: u64) -> Result<Listed, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::ListWorkRequest { on: self::on(on), project };
        let wait = Wait { answer: patience, works: waits::LISTING };
        self.on_runtime(async move {
            Listed::of(first(wait, daemon.list_work(request)).await?)
        })
        .await
    }

    pub async fn close_view(
        &self,
        on: Option<String>,
        terminal: TerminalKey,
    ) -> Result<(), Unanswered> {
        let change = Change::CloseView(CloseView { terminal: Some(terminal.to_api()) });
        self.changed(on, change).await
    }

    pub async fn close_worktree(
        &self,
        on: Option<String>,
        worktree: u64,
        seen: Vec<u64>,
    ) -> Result<(), Unanswered> {
        self.changed(on, Change::CloseWorktree(CloseWorktree { worktree, seen })).await
    }

    pub async fn rename_project(
        &self,
        on: Option<String>,
        project: u64,
        to: String,
    ) -> Result<(), Unanswered> {
        self.changed(on, Change::RenameProject(RenameProject { project, to })).await
    }

    pub async fn rename_worktree(
        &self,
        on: Option<String>,
        worktree: u64,
        to: Option<String>,
    ) -> Result<(), Unanswered> {
        self.changed(on, Change::RenameWorktree(RenameWorktree { worktree, to })).await
    }

    pub async fn rename_view(
        &self,
        on: Option<String>,
        view: u64,
        to: Option<String>,
    ) -> Result<(), Unanswered> {
        let change = Change::RenameTerminal(RenameTerminal { terminal: view, to });
        self.changed(on, change).await
    }

    pub async fn reorder_projects(&self, on: Option<String>, order: Order) -> Result<(), Unanswered> {
        let Order { seen, wanted } = order;
        let change = Change::ReorderProjects(ReorderProjects { projects: wanted, seen });
        self.changed(on, change).await
    }

    pub async fn reorder_worktrees(
        &self,
        on: Option<String>,
        project: u64,
        order: Order,
    ) -> Result<(), Unanswered> {
        let Order { seen, wanted } = order;
        let reorder = ReorderWorktrees { project, worktrees: wanted, seen };
        self.changed(on, Change::ReorderWorktrees(reorder)).await
    }

    pub async fn prunes(&self, on: Option<String>, dir: String) -> Result<Plan, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::PrunesRequest { on: self::on(on), dir };
        let wait = Wait { answer: patience, works: waits::PLANNING };
        self.on_runtime(async move {
            let said = first(wait, daemon.prunes(request)).await?;
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
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let agreed = agreed.into_iter().map(Goes::deleting).collect();
        let request = api::PruneRequest { on: self::on(on), dir, agreed };
        let wait = Wait { answer: patience, works: waits::DELETING };
        self.on_runtime(async move {
            Pruned::of(first(wait, daemon.prune(request)).await?)
        })
        .await
    }

    pub async fn rebase(
        &self,
        on: Option<String>,
        dir: String,
        branch: Option<String>,
    ) -> Result<Rebased, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::RebaseRequest { on: self::on(on), dir, branch };
        let wait = Wait { answer: patience, works: waits::REBASING };
        self.on_runtime(async move {
            Rebased::of(first(wait, daemon.rebase(request)).await?)
        })
        .await
    }

    pub async fn issue_repositories(
        &self,
        on: Option<String>,
        dir: String,
    ) -> Result<Vec<IssueRepository>, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::IssueRepositoriesRequest { on: self::on(on), dir };
        self.on_runtime(async move {
            let listed = unary(patience, daemon.issue_repositories(request)).await?;
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
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::FileIssueRequest { on: self::on(on), dir, words, repository };
        let wait = Wait { answer: patience, works: waits::FILING };
        self.on_runtime(async move {
            Filed::of(first(wait, daemon.file_issue(request)).await?)
        })
        .await
    }
}

async fn watched(mut daemon: Daemon, patience: Duration, observer: Arc<dyn Observer>) -> Infallible {
    let (mut after, mut shown, mut missed) = (None, None, None);
    loop {
        let asked_at = Instant::now();
        let asked = api::WatchRequest { after, kept: None, shown: true };
        let why = match unary(patience, daemon.watch(asked)).await {
            Ok(host) => match host.version {
                Some(version) => {
                    after = Some(version);
                    missed = None;
                    let model = Model::of(&host);
                    if shown.as_ref() != Some(&model) {
                        observer.changed(model.clone());
                        shown = Some(model);
                    }
                    sleep_until(asked_at + waits::PACE).await;
                    continue;
                },
                None => "luned answered with no version, which the core does not read".to_owned(),
            },
            Err(unanswered) => unanswered.why().to_owned(),
        };
        (after, shown) = (None, None);
        if missed.as_ref() != Some(&why) {
            observer.lost(why.clone());
            missed = Some(why);
        }
        tokio::time::sleep(waits::AGAIN).await;
    }
}

struct Talk {
    daemon: Daemon,
    patience: Duration,
    turns: Arc<Mutex<Turns>>,
    turn: u64,
    tell: mpsc::Sender<Result<Step, Unanswered>>,
}

impl Talk {
    async fn steps(mut self, request: api::AskRequest) {
        let within = self.patience.saturating_add(waits::TURN);
        let deadline = Instant::now() + within;
        let mut steps = match unary(self.patience, self.daemon.ask(request)).await {
            Ok(steps) => steps,
            Err(unanswered) => return self.ended(unanswered).await,
        };
        loop {
            let said = match timeout_at(deadline, steps.message()).await {
                Ok(Ok(Some(said))) => Step::of(said),
                Ok(Ok(None)) => return,
                Ok(Err(status)) => Err(Unanswered::of(&status)),
                Err(_) => Err(Unanswered::late(within)),
            };
            let step = match said {
                Ok(step) => step,
                Err(unanswered) => return self.ended(unanswered).await,
            };
            if let Step::Session { id } = &step {
                held(&self.turns).record(id.clone(), self.turn);
            }
            if self.tell.send(Ok(step)).await.is_err() {
                return;
            }
        }
    }

    async fn ended(self, unanswered: Unanswered) {
        let told = self.tell.send(Err(unanswered)).await;
        assert!(told.is_ok() || self.tell.is_closed());
    }
}

#[derive(uniffi::Object)]
pub struct Asking {
    told: tokio::sync::Mutex<mpsc::Receiver<Result<Step, Unanswered>>>,
}

#[uniffi::export]
impl Asking {
    pub async fn next(&self) -> Result<Option<Step>, Unanswered> {
        self.told.lock().await.recv().await.transpose()
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
    }

    #[test]
    fn the_turns_held_forget_the_session_heard_of_longest_ago() {
        let mut turns = Turns::new();
        for session in 0..TURNS_HELD as u64 {
            turns.record(format!("s{session}"), session);
        }
        turns.record("s0".to_owned(), 100);
        turns.record("new".to_owned(), 200);
        assert_eq!(turns.0.len(), TURNS_HELD);
        assert_eq!(turns.before("s1"), None);
        assert_eq!(turns.before("s0"), Some(100));
        assert_eq!(turns.before("new"), Some(200));
    }
}
