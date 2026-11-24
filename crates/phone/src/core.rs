use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::future::Future;
use std::panic;
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
use lune_protocol::turn::Turn;
use tonic::{Response, Status, Streaming};
use tower::service_fn;

use crate::api::change_projects_request::{
    Change, CloseView, CloseWorktree, RenameProject, RenameTerminal, RenameWorktree,
    ReorderProjects, ReorderWorktrees,
};
use crate::api::daemon_client::DaemonClient;
use crate::api::{self, On};
use crate::model::{Model, TerminalKey};
use crate::unanswered::{Refusals, Unanswered};
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

struct Turns {
    read: VecDeque<(String, u64)>,
    running: HashMap<String, u64>,
}

impl Turns {
    fn new() -> Turns {
        Turns { read: VecDeque::with_capacity(TURNS_HELD), running: HashMap::new() }
    }

    fn begin(&mut self, resumed: Option<&str>) -> Result<Turn, Unanswered> {
        assert!(self.read.len() <= TURNS_HELD);
        let Some(session) = resumed else {
            return Ok(Turn::after(None));
        };
        if self.running.contains_key(session) {
            let why = format!("a turn of session {session} still runs: read it to its end first");
            return Err(Unanswered::refused(why));
        }
        let read = self.read.iter().position(|(held, _)| held == session);
        let before = read.and_then(|at| self.read.remove(at)).map(|(_, turn)| turn);
        let turn = Turn::after(before);
        assert!(!self.running.values().any(|&running| running == turn.key()));
        self.running.insert(session.to_owned(), turn.key());
        Ok(turn)
    }

    fn run(&mut self, session: &str, turn: u64) {
        self.running.entry(session.to_owned()).or_insert(turn);
    }

    fn ended(&mut self, session: Option<String>, turn: u64) {
        if let Some(session) = session
            && self.running.get(&session) == Some(&turn)
        {
            self.record(session, turn);
        }
        self.release(turn);
    }

    fn release(&mut self, turn: u64) {
        self.running.retain(|_, &mut running| running != turn);
    }

    fn record(&mut self, session: String, turn: u64) {
        assert!(self.read.len() <= TURNS_HELD);
        self.read.retain(|(held, _)| *held != session);
        if self.read.len() == TURNS_HELD {
            self.read.pop_front();
        }
        assert!(self.read.len() < TURNS_HELD);
        self.read.push_back((session, turn));
    }
}

struct Wait {
    answer: Duration,
    works: Duration,
    refusals: Refusals,
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
    refusals: Refusals,
    asked: impl Future<Output = Result<Response<T>, Status>>,
) -> Result<T, Unanswered> {
    match timeout(within, asked).await {
        Ok(Ok(answer)) => Ok(answer.into_inner()),
        Ok(Err(status)) => Err(Unanswered::of(&status, refusals)),
        Err(_) => Err(Unanswered::late(within)),
    }
}

async fn first<T>(
    wait: Wait,
    asked: impl Future<Output = Result<Response<Streaming<T>>, Status>>,
) -> Result<Option<T>, Unanswered> {
    let Wait { answer, works, refusals } = wait;
    let mut answering = unary(answer, refusals, asked).await?;
    let within = answer.saturating_add(works);
    match timeout(within, answering.message()).await {
        Ok(Ok(said)) => Ok(said),
        Ok(Err(status)) => Err(Unanswered::of(&status, refusals)),
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
        self.on_runtime(async move {
            unary(patience, Refusals::Changes, daemon.change_projects(request)).await.map(drop)
        })
            .await
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
    ) -> Result<Arc<Asking>, Unanswered> {
        let turn = held(&self.turns).begin(resumed.as_deref())?;
        let request = api::AskRequest {
            on: self::on(on),
            cwd,
            words,
            resumed,
            highlighted: String::new(),
            turn: turn.key(),
            before: turn.before(),
            marked_issues: marked.issues,
            marked_pulls: marked.pulls,
        };
        let (tell, told) = mpsc::channel(STEPS_HELD);
        let asked = Talk { daemon: self.daemon.clone(), patience: self.patience, tell };
        self.handle.spawn(asked.steps(request));
        let listening = Listening { told, session: None, over: false };
        let (listening, turns) = (tokio::sync::Mutex::new(listening), self.turns.clone());
        Ok(Arc::new(Asking { listening, turns, turn: turn.key() }))
    }

    pub fn tackle(
        &self,
        on: Option<String>,
        dir: String,
        marked: Marked,
    ) -> Result<Arc<Asking>, Unanswered> {
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
            Waiting::of(unary(patience, Refusals::Others, daemon.waiting(request)).await?)
        })
        .await
    }

    pub async fn answer(
        &self,
        on: Option<String>,
        terminal: TerminalKey,
        shown: u64,
        answer: Answer,
    ) -> Result<(), Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::AnswerRequest {
            on: self::on(on),
            terminal: Some(terminal.to_api()),
            shown: Some(shown),
            answer: Some(answer.into_api()),
        };
        self.on_runtime(async move {
            unary(patience, Refusals::Changes, daemon.answer(request)).await.map(drop)
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
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let prompted = api::Prompted { harness: api::Harness::ClaudeCode.into(), words };
        let opens = Some(api::Opens { runs: Some(api::opens::Runs::Agent(prompted)) });
        let request = api::MakeWorktreeRequest { on: self::on(on), project, branch, opens };
        let wait = Wait { answer: patience, works: waits::MAKING, refusals: Refusals::Others };
        self.on_runtime(async move {
            Landed::of(first(wait, daemon.make_worktree(request)).await?)
        })
        .await
    }

    pub async fn work(&self, on: Option<String>, project: u64) -> Result<Listed, Unanswered> {
        let (mut daemon, patience) = (self.daemon.clone(), self.patience);
        let request = api::ListWorkRequest { on: self::on(on), project };
        let wait = Wait { answer: patience, works: waits::LISTING, refusals: Refusals::Others };
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
        let wait = Wait { answer: patience, works: waits::PLANNING, refusals: Refusals::Others };
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
        let wait = Wait { answer: patience, works: waits::DELETING, refusals: Refusals::Others };
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
        let wait = Wait { answer: patience, works: waits::REBASING, refusals: Refusals::Others };
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
            let listing = daemon.issue_repositories(request);
            let listed = unary(patience, Refusals::Others, listing).await?;
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
        let wait = Wait { answer: patience, works: waits::FILING, refusals: Refusals::Filings };
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
        let why = match unary(patience, Refusals::Others, daemon.watch(asked)).await {
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

enum Told {
    Step(Step),
    Ended,
    Failed(Unanswered),
}

struct Talk {
    daemon: Daemon,
    patience: Duration,
    tell: mpsc::Sender<Told>,
}

impl Talk {
    async fn steps(mut self, request: api::AskRequest) {
        let within = self.patience.saturating_add(waits::TURN);
        let deadline = Instant::now() + within;
        let asked = tokio::select! {
            () = self.tell.closed() => return,
            asked = unary(self.patience, Refusals::Others, self.daemon.ask(request)) => asked,
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
                Ok(Ok(Some(said))) => Step::of(said),
                Ok(Ok(None)) => return self.last(Told::Ended).await,
                Ok(Err(status)) => Err(Unanswered::of(&status, Refusals::Others)),
                Err(_) => Err(Unanswered::late(within)),
            };
            let step = match said {
                Ok(step) => step,
                Err(unanswered) => return self.last(Told::Failed(unanswered)).await,
            };
            if self.tell.send(Told::Step(step)).await.is_err() {
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
    session: Option<String>,
    over: bool,
}

#[derive(uniffi::Object)]
pub struct Asking {
    listening: tokio::sync::Mutex<Listening>,
    turns: Arc<Mutex<Turns>>,
    turn: u64,
}

#[uniffi::export]
impl Asking {
    pub async fn next(&self) -> Result<Option<Step>, Unanswered> {
        let mut listening = self.listening.lock().await;
        if listening.over {
            return Ok(None);
        }
        let told = listening.told.recv().await;
        if !matches!(told, Some(Told::Step(_))) {
            listening.over = true;
        }
        match told {
            Some(Told::Step(step)) => {
                if let Step::Session { id } = &step {
                    held(&self.turns).run(id, self.turn);
                    listening.session = Some(id.clone());
                }
                Ok(Some(step))
            },
            Some(Told::Ended) => {
                held(&self.turns).ended(listening.session.take(), self.turn);
                Ok(None)
            },
            Some(Told::Failed(unanswered)) => {
                held(&self.turns).release(self.turn);
                Err(unanswered)
            },
            None => {
                held(&self.turns).release(self.turn);
                Err(Unanswered::InDoubt { why: "the core's talk ended before the turn did".to_owned() })
            },
        }
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        held(&self.turns).release(self.turn);
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

    fn before(turns: &mut Turns, session: &str) -> Option<u64> {
        let turn = turns.begin(Some(session)).unwrap();
        turns.release(turn.key());
        turn.before()
    }

    #[test]
    fn the_turns_held_forget_the_session_heard_of_longest_ago() {
        let mut turns = Turns::new();
        for session in 0..TURNS_HELD as u64 {
            turns.record(format!("s{session}"), session);
        }
        turns.record("s0".to_owned(), 100);
        turns.record("new".to_owned(), 200);
        assert_eq!(turns.read.len(), TURNS_HELD);
        assert_eq!(before(&mut turns, "s1"), None);
        assert_eq!(before(&mut turns, "s0"), Some(100));
        assert_eq!(before(&mut turns, "new"), Some(200));
    }

    #[test]
    fn a_turn_is_the_one_before_only_once_it_was_read_to_its_end() {
        let mut turns = Turns::new();
        let first = turns.begin(None).unwrap();
        turns.run("7c22", first.key());
        turns.ended(Some("7c22".to_owned()), first.key());
        let read = turns.begin(Some("7c22")).unwrap();
        assert_eq!(read.before(), Some(first.key()));
        turns.release(read.key());
        let unread = turns.begin(Some("7c22")).unwrap();
        assert_eq!(unread.before(), None, "the turn left unread is none before, nor the one before it");
    }

    #[test]
    fn a_session_takes_one_turn_at_a_time() {
        let mut turns = Turns::new();
        let running = turns.begin(Some("7c22")).unwrap();
        assert!(matches!(turns.begin(Some("7c22")), Err(Unanswered::Refused { .. })));
        let other = turns.begin(Some("9f00")).unwrap();
        turns.ended(Some("7c22".to_owned()), other.key());
        assert_eq!(before(&mut turns, "9f00"), None, "a turn ended in one session is read in no other");
        turns.release(running.key());
        assert!(turns.begin(Some("7c22")).is_ok());
    }
}
