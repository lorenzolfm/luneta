use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io::{self, ErrorKind, IoSlice};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use hyper_util::rt::TokioIo;
use lune_protocol::socket::SocketId;
use rustix::process::Pid;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncReadExt as _, AsyncWrite, BufReader, ReadBuf};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, SemaphorePermit, TryAcquireError};
use tokio::task::JoinHandle;
use tokio::time::{timeout_at, Instant};
use tokio_stream::Stream;
use tonic::transport::{Channel, Endpoint, Error, Uri};
use tonic::{IntoStreamingRequest, Response, Status, Streaming};
use tower::{service_fn, BoxError, Service};

use crate::api;
use crate::api::daemon_client::DaemonClient;
use crate::daemon::{ANSWERING_MAX, QUESTIONS_MAX};
use crate::folder::Folder;
use crate::{release, remote, tasks};

const CONNECT: Duration = Duration::from_secs(1);

pub const ANSWER: Answer = Answer::of(Duration::from_secs(5));

pub const BRIDGED: &[u8] = b"luned\n";

pub const REACH: Duration = Duration::from_secs(10);

pub(crate) const ROUND_TRIP: RoundTrip = RoundTrip::of(Duration::from_secs(5));

const QUIET: Duration = Duration::from_secs(3);

const _: () = assert!(QUIET.as_millis() < ANSWER.waits().as_millis());

const KEPT_MAX: usize = 16;

const CONNECTING_MAX: usize = KEPT_MAX;

pub const HELD_MAX: usize = remote::HELD_MAX;

pub const ASKED_MAX: usize = QUESTIONS_MAX as usize - HELD_MAX;

pub const ASKED_WAITING_MAX: usize = 32;

const STREAM_WINDOW_BYTES: u32 = 64 * 1024;

const CONNECTION_WINDOW_BYTES: u32 = QUESTIONS_MAX * STREAM_WINDOW_BYTES;

const REACH_ATTEMPTS_MAX: usize = 2;

const _: () = assert!(KEPT_MAX > 0, "lune keeps at least one channel to luned");
const _: () = assert!(CONNECTING_MAX > 0, "lune connects to at least one luned at once");
const _: () = assert!(REACH_ATTEMPTS_MAX > 0, "reach tries at least once");
const _: () = assert!(HELD_MAX > 0, "lune holds at least one stream to luned");
const _: () = assert!(ASKED_MAX > 0, "lune keeps room for questions to luned");
const _: () = assert!(ASKED_WAITING_MAX > 0, "short bursts of questions wait their turn");
const _: () = assert!(
    HELD_MAX + ASKED_MAX <= QUESTIONS_MAX as usize,
    "the one connection to luned holds every stream and question lune puts on it"
);
const _: () = assert!(
    CONNECTION_WINDOW_BYTES as u64 <= i32::MAX as u64,
    "a window of every stream lune does not read fits in what HTTP/2 lets a connection hold"
);

struct Slots<const MAX: usize>(Semaphore);

impl<const MAX: usize> Slots<MAX> {
    const fn new() -> Self {
        const { assert!(MAX > 0, "slots hold at least one place") }
        Self(Semaphore::const_new(MAX))
    }

    fn available(&self) -> usize {
        self.0.available_permits()
    }

    fn try_acquire(&self) -> Option<SemaphorePermit<'_>> {
        assert!(!self.0.is_closed(), "slots are never closed");
        assert!(self.0.available_permits() <= MAX, "slots hold at most {MAX} places");
        match self.0.try_acquire() {
            Ok(permit) => {
                assert!(self.0.available_permits() < MAX, "an active slot occupies a place");
                Some(permit)
            },
            Err(TryAcquireError::NoPermits) => None,
            Err(TryAcquireError::Closed) => unreachable!("slots are never closed"),
        }
    }
}

struct OwnedSlots<const MAX: usize>(Arc<Semaphore>);

impl<const MAX: usize> OwnedSlots<MAX> {
    fn new() -> Self {
        const { assert!(MAX > 0, "slots hold at least one place") }
        let slots = Self(Arc::new(Semaphore::new(MAX)));
        assert_eq!(slots.0.available_permits(), MAX, "slots start full");
        slots
    }

    fn try_acquire(&self) -> Option<OwnedSemaphorePermit> {
        assert!(!self.0.is_closed(), "slots are never closed");
        assert!(self.0.available_permits() <= MAX, "slots hold at most {MAX} places");
        match Arc::clone(&self.0).try_acquire_owned() {
            Ok(permit) => {
                assert!(self.0.available_permits() < MAX, "an active slot occupies a place");
                Some(permit)
            },
            Err(TryAcquireError::NoPermits) => None,
            Err(TryAcquireError::Closed) => unreachable!("slots are never closed"),
        }
    }

    async fn acquire_until(&self, deadline: Instant) -> Option<OwnedSemaphorePermit> {
        assert!(!self.0.is_closed(), "slots are never closed");
        assert!(self.0.available_permits() <= MAX, "slots hold at most {MAX} places");
        let acquired = timeout_at(deadline, Arc::clone(&self.0).acquire_owned()).await.ok()?;
        let permit = acquired.expect("slots are never closed");
        assert!(self.0.available_permits() < MAX, "an active slot occupies a place");
        Some(permit)
    }
}

static KEPT: Mutex<BTreeMap<PathBuf, Kept>> = Mutex::new(BTreeMap::new());

static CONNECTING: LazyLock<OwnedSlots<CONNECTING_MAX>> = LazyLock::new(OwnedSlots::new);

#[derive(Clone, Copy, Debug)]
pub struct Answer(Duration);

impl Answer {
    #[must_use]
    pub const fn of(answer: Duration) -> Answer {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        assert!(
            answer.as_millis() < ANSWERING_MAX.as_millis(),
            "luned works at a question for longer than any of lune's askers waits for its answer"
        );
        Answer(answer)
    }

    #[must_use]
    pub const fn waits(self) -> Duration {
        self.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RoundTrip {
    pong: Duration,
    answer: Answer,
}

impl RoundTrip {
    #[must_use]
    pub const fn of(round_trip: Duration) -> RoundTrip {
        assert!(!round_trip.is_zero(), "a keepalive waits some time for its pong");
        let answer = ANSWER.0.checked_add(round_trip);
        let answer = answer.expect("an answer through a bridge waits a finite time");
        RoundTrip { pong: round_trip, answer: Answer::of(answer) }
    }
}

const SAID_MAX: u64 = 4096;

pub type Daemon = DaemonClient<Within<Channel>>;

#[derive(Debug, Default)]
struct GoneInner {
    marked: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

#[derive(Clone, Debug, Default)]
struct Gone(Arc<GoneInner>);

impl Gone {
    fn mark(&self) {
        if self.is() {
            return;
        }
        let waker = {
            let mut slot = self.0.waker.lock().unwrap_or_else(PoisonError::into_inner);
            self.0.marked.store(true, Ordering::Release);
            slot.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn is(&self) -> bool {
        self.0.marked.load(Ordering::Acquire)
    }

    fn register(&self, waker: &Waker) -> bool {
        let mut slot = self.0.waker.lock().unwrap_or_else(PoisonError::into_inner);
        if self.0.marked.load(Ordering::Acquire) {
            *slot = None;
            return false;
        }
        match slot.as_mut() {
            Some(existing) => existing.clone_from(waker),
            None => *slot = Some(waker.clone()),
        }
        true
    }
}

struct MarkOnDrop(Option<Gone>);

impl MarkOnDrop {
    fn arm(gone: Gone) -> MarkOnDrop {
        MarkOnDrop(Some(gone))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for MarkOnDrop {
    fn drop(&mut self) {
        if let Some(gone) = self.0.take() {
            gone.mark();
        }
    }
}

fn daemon(channel: Channel, gone: Option<Gone>, answer: Answer) -> Daemon {
    let within = match gone {
        Some(gone) => Within::watched(channel, answer.0, unanswered, gone),
        None => Within::new(channel, answer.0, unanswered),
    };
    DaemonClient::new(within).max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX)
}

#[cfg(test)]
pub(crate) fn unwatched(channel: Channel, answer: Answer) -> Daemon {
    daemon(channel, None, answer)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsent {
    HoldsFull,
    QueueFull,
    TimedOut(Duration),
    ChannelGone,
}

impl fmt::Display for Unsent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsent::HoldsFull => write!(f, "lune holds at most {HELD_MAX} streams to luned at once"),
            Unsent::QueueFull => write!(
                f,
                "lune has at most {ASKED_WAITING_MAX} questions waiting on luned at once"
            ),
            Unsent::TimedOut(wait) => f.write_str(&unanswered(*wait)),
            Unsent::ChannelGone => f.write_str("luned's channel went before the call was sent"),
        }
    }
}

impl std::error::Error for Unsent {}

#[derive(Debug)]
pub enum Unasked {
    Unsent(Unsent),
    Asked(Status),
}

impl Unasked {
    #[must_use]
    pub fn said(&self, on: Option<&remote::Destination>) -> String {
        match self {
            Unasked::Unsent(unsent) => unsent.to_string(),
            Unasked::Asked(status) => remote::said_by(on, status),
        }
    }
}

impl fmt::Display for Unasked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unasked::Unsent(unsent) => unsent.fmt(f),
            Unasked::Asked(status) => status.fmt(f),
        }
    }
}

impl std::error::Error for Unasked {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Unasked::Unsent(_) => None,
            Unasked::Asked(status) => status.source(),
        }
    }
}

#[derive(Debug)]
pub enum WatchError {
    Unasked(Unasked),
    Unversioned,
}

impl fmt::Display for WatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WatchError::Unasked(unasked) => unasked.fmt(f),
            WatchError::Unversioned => {
                f.write_str("an answer with no version, which this lune does not read")
            },
        }
    }
}

impl std::error::Error for WatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            WatchError::Unasked(unasked) => unasked.source(),
            WatchError::Unversioned => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalWatch {
    Client,
    Window,
}

impl LocalWatch {
    const fn shown(self) -> bool {
        match self {
            LocalWatch::Client => false,
            LocalWatch::Window => true,
        }
    }
}

pub struct WatchedHost {
    pub version: api::Version,
    pub host: api::Host,
}

struct Budget {
    held: OwnedSlots<HELD_MAX>,
    asked: OwnedSlots<ASKED_MAX>,
    waiting: Slots<ASKED_WAITING_MAX>,
}

impl Budget {
    fn new() -> Budget {
        let budget = Budget {
            held: OwnedSlots::new(),
            asked: OwnedSlots::new(),
            waiting: Slots::new(),
        };
        assert_eq!(
            budget.waiting.available(),
            ASKED_WAITING_MAX,
            "question wait queue starts empty"
        );
        budget
    }

    fn hold(&self) -> Result<OwnedSemaphorePermit, Unsent> {
        self.held.try_acquire().ok_or(Unsent::HoldsFull)
    }

    async fn ask(&self, deadline: Instant, answer: Answer) -> Result<OwnedSemaphorePermit, Unsent> {
        let wait = answer.waits();
        assert!(!wait.is_zero(), "a question waits some time for its answer");
        if let Some(permit) = self.asked.try_acquire() {
            return Ok(permit);
        }
        let _waiting = self.waiting.try_acquire().ok_or(Unsent::QueueFull)?;
        self.asked.acquire_until(deadline).await.ok_or(Unsent::TimedOut(wait))
    }
}

pub struct Held<S> {
    heard: S,
    _permit: OwnedSemaphorePermit,
}

impl<T> Held<Streaming<T>> {
    pub async fn message(&mut self) -> Result<Option<T>, Status> {
        self.heard.message().await
    }
}

impl<S: Stream + Unpin> Stream for Held<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.heard).poll_next(context)
    }
}

pub struct Watching {
    daemon: Daemon,
    gone: Gone,
    answer: Answer,
    shown: bool,
    after: Option<api::Version>,
    _permit: OwnedSemaphorePermit,
}

impl Watching {
    pub async fn next(&mut self) -> Result<WatchedHost, WatchError> {
        let request = api::WatchRequest { after: self.after, kept: None, shown: self.shown };
        let host = self.call(request).await.map_err(WatchError::Unasked)?;
        let version = host.version.ok_or(WatchError::Unversioned)?;
        self.after = Some(version);
        Ok(WatchedHost { version, host })
    }

    pub async fn once(mut self, after: api::Version) -> Result<api::Host, Unasked> {
        self.call(api::WatchRequest { after: Some(after), kept: None, shown: self.shown }).await
    }

    async fn call(&mut self, request: api::WatchRequest) -> Result<api::Host, Unasked> {
        let wait = self.answer.waits();
        assert!(!wait.is_zero(), "a watch waits some time for its answer");
        if self.gone.is() {
            return Err(Unasked::Unsent(Unsent::ChannelGone));
        }
        match tokio::time::timeout(wait, self.daemon.watch(request)).await {
            Ok(watched) => watched.map(Response::into_inner).map_err(Unasked::Asked),
            Err(_) => Err(Unasked::Asked(Status::deadline_exceeded(unanswered(wait)))),
        }
    }
}

#[derive(Clone)]
pub struct Shared {
    channel: Channel,
    gone: Gone,
    budget: Arc<Budget>,
}

impl Shared {
    fn new(channel: Channel, gone: Gone) -> Shared {
        Shared { channel, gone, budget: Arc::new(Budget::new()) }
    }

    fn daemon(&self, answer: Answer) -> Daemon {
        daemon(self.channel.clone(), Some(self.gone.clone()), answer)
    }

    async fn ask_unary<T, F>(
        &self,
        answer: Answer,
        asking: impl FnOnce(Daemon) -> F,
    ) -> Result<T, Unasked>
    where
        T: prost::Message,
        F: Future<Output = Result<Response<T>, Status>>,
    {
        let wait = answer.waits();
        assert!(!wait.is_zero(), "a question waits some time for its answer");
        self.ask_until(Instant::now() + wait, answer, asking).await
    }

    async fn ask_until<T, F>(
        &self,
        deadline: Instant,
        answer: Answer,
        asking: impl FnOnce(Daemon) -> F,
    ) -> Result<T, Unasked>
    where
        T: prost::Message,
        F: Future<Output = Result<Response<T>, Status>>,
    {
        let wait = answer.waits();
        assert!(!wait.is_zero(), "a question waits some time for its answer");
        if self.gone.is() {
            return Err(Unasked::Unsent(Unsent::ChannelGone));
        }
        let _permit = self.budget.ask(deadline, answer).await.map_err(Unasked::Unsent)?;
        if self.gone.is() {
            return Err(Unasked::Unsent(Unsent::ChannelGone));
        }
        if Instant::now() >= deadline {
            return Err(Unasked::Unsent(Unsent::TimedOut(wait)));
        }
        let daemon = self.daemon(answer);
        match timeout_at(deadline, asking(daemon)).await {
            Ok(answered) => answered.map(Response::into_inner).map_err(Unasked::Asked),
            Err(_) => Err(Unasked::Asked(Status::deadline_exceeded(unanswered(wait)))),
        }
    }

    async fn hold_stream<T, F>(
        &self,
        answer: Answer,
        asking: impl FnOnce(Daemon) -> F,
    ) -> Result<Held<Streaming<T>>, Unasked>
    where
        F: Future<Output = Result<Response<Streaming<T>>, Status>>,
    {
        let wait = answer.waits();
        assert!(!wait.is_zero(), "a held stream waits some time for its answer");
        if self.gone.is() {
            return Err(Unasked::Unsent(Unsent::ChannelGone));
        }
        let permit = self.budget.hold().map_err(Unasked::Unsent)?;
        let daemon = self.daemon(answer);
        let heard = match tokio::time::timeout(wait, asking(daemon)).await {
            Ok(heard) => heard.map_err(Unasked::Asked)?.into_inner(),
            Err(_) => return Err(Unasked::Asked(Status::deadline_exceeded(unanswered(wait)))),
        };
        Ok(Held { heard, _permit: permit })
    }

    pub fn watching(&self, answer: Answer, kind: LocalWatch) -> Result<Watching, Unsent> {
        if self.gone.is() {
            return Err(Unsent::ChannelGone);
        }
        let permit = self.budget.hold()?;
        Ok(Watching {
            daemon: self.daemon(answer),
            gone: self.gone.clone(),
            answer,
            shown: kind.shown(),
            after: None,
            _permit: permit,
        })
    }

    pub async fn snapshot(&self, answer: Answer, shown: bool) -> Result<api::Host, Unasked> {
        let request = api::WatchRequest { after: None, kept: None, shown };
        self.ask_unary(answer, |mut daemon| async move { daemon.watch(request).await }).await
    }

    pub async fn list_agents(&self, answer: Answer) -> Result<api::ListAgentsResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.list_agents(()).await }).await
    }

    pub async fn say(&self, answer: Answer, request: api::SayRequest) -> Result<(), Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.say(request).await }).await
    }

    pub async fn change_projects(
        &self,
        answer: Answer,
        request: api::ChangeProjectsRequest,
    ) -> Result<api::ChangeProjectsResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.change_projects(request).await })
            .await
    }

    pub async fn list_repositories(
        &self,
        answer: Answer,
        request: api::ListRepositoriesRequest,
    ) -> Result<api::ListRepositoriesResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.list_repositories(request).await })
            .await
    }

    pub async fn list_contents(
        &self,
        answer: Answer,
        request: api::ListContentsRequest,
    ) -> Result<api::ListContentsResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.list_contents(request).await })
            .await
    }

    pub async fn list_skills(
        &self,
        answer: Answer,
        request: api::ListSkillsRequest,
    ) -> Result<api::ListSkillsResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.list_skills(request).await }).await
    }

    pub async fn transcript_ends(
        &self,
        answer: Answer,
        request: api::TranscriptEndsRequest,
    ) -> Result<api::TranscriptEndsResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.transcript_ends(request).await })
            .await
    }

    pub async fn keep_printed(
        &self,
        answer: Answer,
        request: api::KeepPrintedRequest,
    ) -> Result<api::KeepPrintedResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.keep_printed(request).await })
            .await
    }

    pub async fn issue_repositories(
        &self,
        answer: Answer,
        request: api::IssueRepositoriesRequest,
    ) -> Result<api::IssueRepositoriesResponse, Unasked> {
        self.ask_unary(answer, |mut daemon| async move { daemon.issue_repositories(request).await })
            .await
    }

    pub async fn keep_checks(
        &self,
        answer: Answer,
        request: api::KeepChecksRequest,
    ) -> Result<Held<Streaming<api::KeepChecksResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.keep_checks(request).await })
            .await
    }

    pub async fn ask(
        &self,
        answer: Answer,
        request: api::AskRequest,
    ) -> Result<Held<Streaming<api::AskResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.ask(request).await }).await
    }

    pub async fn paste(
        &self,
        answer: Answer,
        request: api::PasteRequest,
    ) -> Result<Held<Streaming<api::PasteResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.paste(request).await }).await
    }

    pub async fn file_issue(
        &self,
        answer: Answer,
        request: api::FileIssueRequest,
    ) -> Result<Held<Streaming<api::FileIssueResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.file_issue(request).await }).await
    }

    pub async fn prunes(
        &self,
        answer: Answer,
        request: api::PrunesRequest,
    ) -> Result<Held<Streaming<api::PrunesResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.prunes(request).await }).await
    }

    pub async fn prune(
        &self,
        answer: Answer,
        request: api::PruneRequest,
    ) -> Result<Held<Streaming<api::PruneResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.prune(request).await }).await
    }

    pub async fn rebase(
        &self,
        answer: Answer,
        request: api::RebaseRequest,
    ) -> Result<Held<Streaming<api::RebaseResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.rebase(request).await }).await
    }

    pub async fn make_worktree(
        &self,
        answer: Answer,
        request: api::MakeWorktreeRequest,
    ) -> Result<Held<Streaming<api::MakeWorktreeResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.make_worktree(request).await })
            .await
    }

    pub async fn list_work(
        &self,
        answer: Answer,
        request: api::ListWorkRequest,
    ) -> Result<Held<Streaming<api::ListWorkResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.list_work(request).await }).await
    }

    pub async fn read_diff(
        &self,
        answer: Answer,
        request: api::ReadDiffRequest,
    ) -> Result<Held<Streaming<api::ReadDiffResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.read_diff(request).await }).await
    }

    pub async fn keep_image(
        &self,
        answer: Answer,
        request: impl IntoStreamingRequest<Message = api::KeepImageRequest>,
    ) -> Result<Held<Streaming<api::KeepImageResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.keep_image(request).await }).await
    }

    pub async fn show(
        &self,
        answer: Answer,
        words: impl IntoStreamingRequest<Message = api::ShowRequest>,
    ) -> Result<Held<Streaming<api::ShowResponse>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.show(words).await }).await
    }

    pub async fn frames(
        &self,
        answer: Answer,
        words: impl IntoStreamingRequest<Message = api::FramesRequest>,
    ) -> Result<Held<Streaming<crate::mux::own::api::ToViewer>>, Unasked> {
        self.hold_stream(answer, |mut daemon| async move { daemon.frames(words).await }).await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PeerPid(Pid);

impl PeerPid {
    #[must_use]
    pub fn new(pid: u32) -> Option<PeerPid> {
        if pid <= 1 {
            return None;
        }
        i32::try_from(pid).ok().and_then(Pid::from_raw).map(PeerPid)
    }

    #[must_use]
    pub fn from_raw(pid: i32) -> Option<PeerPid> {
        if pid <= 1 {
            return None;
        }
        Pid::from_raw(pid).map(PeerPid)
    }

    #[must_use]
    pub fn get(self) -> u32 {
        u32::try_from(self.0.as_raw_pid()).expect("a Unix peer pid is positive")
    }

    #[must_use]
    pub const fn as_pid(self) -> Pid {
        self.0
    }
}

impl fmt::Display for PeerPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmatched {
    other: release::Other,
    pid: Option<PeerPid>,
}

impl Unmatched {
    #[must_use]
    pub fn of(other: release::Other, pid: Option<PeerPid>) -> Unmatched {
        Unmatched { other, pid }
    }

    #[must_use]
    pub fn other(&self) -> &release::Other {
        &self.other
    }

    #[must_use]
    pub fn pid(&self) -> Option<PeerPid> {
        self.pid
    }
}

impl fmt::Display for Unmatched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pid = self.pid.map_or_default(|pid| format!(" (pid {pid})"));
        let (daemon, own) = (format!("luned {}{pid}", self.other), release::SAID.as_str());
        if self.other.is_newer() {
            write!(f, "{daemon} is newer than this lune, {own}: run a lune of its build")
        } else {
            write!(
                f,
                "{daemon} is another build than this lune, {own}: open lune to have it replaced"
            )
        }
    }
}

pub enum Reached {
    Matched(Shared),
    Unmatched(Unmatched),
}

#[derive(Debug)]
pub enum Unreached {
    Busy,
    Changed,
    Connect(Error),
    Asked(Status),
}

impl fmt::Display for Unreached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unreached::Busy => write!(f, "lune connects to at most {CONNECTING_MAX} luneds at once"),
            Unreached::Changed => f.write_str("luned's socket changed while lune reached it"),
            Unreached::Connect(error) => write!(f, "{error}"),
            Unreached::Asked(status) => match status.message() {
                "" => write!(f, "luned: {}", status.code().description()),
                said => f.write_str(said),
            },
        }
    }
}

impl std::error::Error for Unreached {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Unreached::Busy | Unreached::Changed => None,
            Unreached::Connect(error) => Some(error),
            Unreached::Asked(status) => status.source(),
        }
    }
}

struct Kept {
    shared: Shared,
    socket_id: SocketId,
}

fn lock_kept() -> MutexGuard<'static, BTreeMap<PathBuf, Kept>> {
    KEPT.lock().expect("lune's kept channel map is never poisoned")
}

#[must_use]
pub fn kept(folder: &Folder) -> Option<Shared> {
    kept_at(&folder.socket())
}

fn kept_at(socket: &Path) -> Option<Shared> {
    let mut kept = lock_kept();
    assert!(kept.len() <= KEPT_MAX, "lune keeps at most {KEPT_MAX} channels to luned");
    kept.retain(|_, entry| !entry.shared.gone.is());
    let found = if let Some(entry) = kept.get(socket) {
        if SocketId::at(socket) == Some(entry.socket_id) && !entry.shared.gone.is() {
            Some(entry.shared.clone())
        } else {
            entry.shared.gone.mark();
            let removed = kept.remove(socket);
            assert!(removed.is_some(), "the evicted channel was in the cache");
            assert!(!kept.contains_key(socket), "an evicted channel is no longer kept");
            None
        }
    } else {
        None
    };
    assert!(kept.len() <= KEPT_MAX, "lune keeps at most {KEPT_MAX} channels to luned");
    found
}

fn keep(socket: &Path, fresh: Kept) -> Option<Shared> {
    let mut kept = lock_kept();
    assert!(kept.len() <= KEPT_MAX, "lune keeps at most {KEPT_MAX} channels to luned");
    kept.retain(|_, entry| !entry.shared.gone.is());
    let current = SocketId::at(socket);
    if fresh.shared.gone.is() || current != Some(fresh.socket_id) {
        fresh.shared.gone.mark();
        if let Some(id) = current
            && let Some(existing) = kept.get(socket)
        {
            if existing.socket_id == id && !existing.shared.gone.is() {
                return Some(existing.shared.clone());
            }
            existing.shared.gone.mark();
            let removed = kept.remove(socket);
            assert!(removed.is_some(), "the evicted channel was in the cache");
        }
        return None;
    }
    if let Some(existing) = kept.get(socket) {
        if existing.socket_id == fresh.socket_id && !existing.shared.gone.is() {
            fresh.shared.gone.mark();
            return Some(existing.shared.clone());
        }
        existing.shared.gone.mark();
    } else if kept.len() == KEPT_MAX {
        let evicted = kept.pop_first();
        assert!(evicted.is_some(), "a full channel cache drops one entry to make room");
    }
    let shared = fresh.shared.clone();
    kept.insert(socket.to_owned(), fresh);
    assert!(kept.contains_key(socket), "a newly kept channel is in the cache");
    assert!(kept.len() <= KEPT_MAX, "lune keeps at most {KEPT_MAX} channels to luned");
    Some(shared)
}

struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Opened {
    Connected { channel: Channel, gone: Gone, id: SocketId },
    Changed,
}

enum Connected {
    Matched { channel: Channel, gone: Gone, id: SocketId },
    Unmatched(Unmatched),
    Changed,
}

pub async fn reach(folder: &Folder) -> Result<Reached, Unreached> {
    let socket = folder.socket();
    if let Some(shared) = kept_at(&socket) {
        return Ok(Reached::Matched(shared));
    }
    let mut connecting = CONNECTING.try_acquire().ok_or(Unreached::Busy)?;
    for _ in 0..REACH_ATTEMPTS_MAX {
        if let Some(shared) = kept_at(&socket) {
            return Ok(Reached::Matched(shared));
        }
        let at = socket.clone();
        let mut connected = AbortOnDrop(tasks::shared().spawn(async move {
            let held = connecting;
            let answered = connect_matched(at, ANSWER).await;
            (held, answered)
        }));
        let (returned, answered) = (&mut connected.0).await.unwrap_or_else(|ended| {
            assert!(ended.is_panic(), "a connect task spawned by reach is never cancelled");
            std::panic::resume_unwind(ended.into_panic())
        });
        connecting = returned;
        match answered? {
            Connected::Matched { channel, gone, id } => {
                let kept = Kept { shared: Shared::new(channel, gone), socket_id: id };
                if let Some(shared) = keep(&socket, kept) {
                    return Ok(Reached::Matched(shared));
                }
            },
            Connected::Unmatched(unmatched) => return Ok(Reached::Unmatched(unmatched)),
            Connected::Changed => {},
        }
    }
    Err(Unreached::Changed)
}

async fn connect_matched(socket: PathBuf, verify: Answer) -> Result<Connected, Unreached> {
    let (channel, gone, id) = match open(socket).await.map_err(Unreached::Connect)? {
        Opened::Connected { channel, gone, id } => (channel, gone, id),
        Opened::Changed => return Ok(Connected::Changed),
    };
    let mut guard = MarkOnDrop::arm(gone.clone());
    let wait = verify.waits();
    assert!(!wait.is_zero(), "build verification waits some time for its answer");
    let mut verified = daemon(channel.clone(), Some(gone.clone()), verify);
    let Ok(answered) = tokio::time::timeout(wait, verified.info(())).await else {
        return Err(Unreached::Asked(Status::deadline_exceeded(unanswered(wait))));
    };
    let info = answered.map_err(Unreached::Asked)?.into_inner();
    match release::of(&info.version, info.built) {
        release::Of::Own => {
            guard.disarm();
            Ok(Connected::Matched { channel, gone, id })
        },
        release::Of::Other(other) => {
            Ok(Connected::Unmatched(Unmatched { other, pid: PeerPid::new(info.pid) }))
        },
    }
}

fn endpoint() -> Endpoint {
    Endpoint::from_static("http://luned")
        .connect_timeout(CONNECT)
        .http2_adaptive_window(false)
        .initial_stream_window_size(STREAM_WINDOW_BYTES)
        .initial_connection_window_size(CONNECTION_WINDOW_BYTES)
}

async fn open(socket: PathBuf) -> Result<Opened, Error> {
    let gone = Gone::default();
    let mut guard = MarkOnDrop::arm(gone.clone());
    let tracked = gone.clone();
    let verified = Arc::new(OnceLock::new());
    let recorded = Arc::clone(&verified);
    let changed = Arc::new(AtomicBool::new(false));
    let swapped = Arc::clone(&changed);
    let mut at = Some(socket);
    let connected = endpoint()
        .connect_with_connector(service_fn(move |_: Uri| {
            let at = at.take();
            let gone = tracked.clone();
            let recorded = Arc::clone(&recorded);
            let swapped = Arc::clone(&swapped);
            async move {
                let at = at.ok_or_else(went)?;
                let before = SocketId::at(&at);
                let stream = mine(UnixStream::connect(&at).await?)?;
                let after = SocketId::at(&at);
                let Some(id) = before.filter(|before| Some(*before) == after) else {
                    swapped.store(true, Ordering::Relaxed);
                    return Err(went());
                };
                let _ = recorded.set(id);
                io::Result::Ok(TokioIo::new(Tracked { stream, gone }))
            }
        }))
        .await;
    match connected {
        Ok(channel) => {
            let id = *verified.get().expect("connect_with_connector ran the connector once");
            guard.disarm();
            Ok(Opened::Connected { channel, gone, id })
        },
        Err(_) if changed.load(Ordering::Relaxed) => Ok(Opened::Changed),
        Err(error) => Err(error),
    }
}

struct Tracked {
    stream: UnixStream,
    gone: Gone,
}

impl Tracked {
    fn marked<T>(&self, context: &Context<'_>, polled: Poll<io::Result<T>>) -> Poll<io::Result<T>> {
        match &polled {
            Poll::Ready(Err(_)) => self.gone.mark(),
            Poll::Pending if !self.gone.register(context.waker()) => {
                return Poll::Ready(Err(went()));
            },
            Poll::Ready(Ok(_)) | Poll::Pending => {},
        }
        polled
    }
}

impl AsyncRead for Tracked {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.gone.is() {
            return Poll::Ready(Err(went()));
        }
        let (before, room) = (buffer.filled().len(), buffer.remaining());
        let polled = Pin::new(&mut self.stream).poll_read(context, buffer);
        match &polled {
            Poll::Ready(Ok(())) => {
                assert!(
                    buffer.filled().len() >= before,
                    "AsyncRead never shrinks the filled region of its buffer"
                );
                if room > 0 && buffer.filled().len() == before {
                    self.gone.mark();
                }
            },
            Poll::Ready(Err(_)) => self.gone.mark(),
            Poll::Pending if !self.gone.register(context.waker()) => {
                return Poll::Ready(Err(went()));
            },
            Poll::Pending => {},
        }
        polled
    }
}

impl AsyncWrite for Tracked {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.gone.is() {
            return Poll::Ready(Err(went()));
        }
        let polled = Pin::new(&mut self.stream).poll_write(context, buffer);
        self.marked(context, polled)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffers: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.gone.is() {
            return Poll::Ready(Err(went()));
        }
        let polled = Pin::new(&mut self.stream).poll_write_vectored(context, buffers);
        self.marked(context, polled)
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.gone.is() {
            return Poll::Ready(Err(went()));
        }
        let polled = Pin::new(&mut self.stream).poll_flush(context);
        self.marked(context, polled)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.gone.mark();
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

impl Drop for Tracked {
    fn drop(&mut self) {
        self.gone.mark();
    }
}

#[derive(Clone, Debug)]
pub struct Within<S> {
    inner: S,
    answer: Duration,
    unanswered: fn(Duration) -> String,
    gone: Option<Gone>,
}

impl<S> Within<S> {
    pub(crate) fn new(inner: S, answer: Duration, unanswered: fn(Duration) -> String) -> Self {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        Within { inner, answer, unanswered, gone: None }
    }

    fn watched(
        inner: S,
        answer: Duration,
        unanswered: fn(Duration) -> String,
        gone: Gone,
    ) -> Self {
        assert!(!answer.is_zero(), "a call waits some time for its answer");
        Within { inner, answer, unanswered, gone: Some(gone) }
    }
}

impl<S: Service<R>, R> Service<R> for Within<S>
where
    S::Error: Into<BoxError>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        let polled = self.inner.poll_ready(context);
        if let Poll::Ready(Err(_)) = &polled
            && let Some(gone) = &self.gone
        {
            gone.mark();
        }
        polled.map_err(Into::into)
    }

    fn call(&mut self, request: R) -> Self::Future {
        let (answer, unanswered, gone) = (self.answer, self.unanswered, self.gone.clone());
        let asked = self.inner.call(request);
        Box::pin(async move {
            match tokio::time::timeout(answer, asked).await {
                Ok(Ok(heard)) => Ok(heard),
                Ok(Err(error)) => {
                    if let Some(gone) = gone {
                        gone.mark();
                    }
                    Err(error.into())
                },
                Err(_) => Err(Status::deadline_exceeded(unanswered(answer)).into()),
            }
        })
    }
}

#[must_use]
pub fn unanswered(within: Duration) -> String {
    format!("luned did not answer in {}s", within.as_secs_f64())
}

pub async fn info_of_any_build(
    folder: &Folder,
    answer: Answer,
) -> Result<api::InfoResponse, Unreached> {
    let wait = answer.waits();
    assert!(!wait.is_zero(), "an info call waits some time for its answer");
    let mut daemon = connect(folder, answer).await.map_err(Unreached::Connect)?;
    let Ok(answered) = tokio::time::timeout(wait, daemon.info(())).await else {
        return Err(Unreached::Asked(Status::deadline_exceeded(unanswered(wait))));
    };
    answered.map(Response::into_inner).map_err(Unreached::Asked)
}

pub async fn connect(folder: &Folder, answer: Answer) -> Result<Daemon, Error> {
    let mut socket = Some(folder.socket());
    let channel = endpoint()
        .connect_with_connector(service_fn(move |_: Uri| {
            let socket = socket.take();
            async move {
                let socket = socket.ok_or_else(went)?;
                mine(UnixStream::connect(socket).await?).map(TokioIo::new)
            }
        }))
        .await?;
    Ok(daemon(channel, None, answer))
}

pub async fn through(
    command: std::process::Command,
    round_trip: RoundTrip,
) -> Result<Daemon, Error> {
    let mut command = Some(command);
    let channel = Endpoint::from_static("http://luned")
        .http2_keep_alive_interval(QUIET)
        .keep_alive_timeout(round_trip.pong)
        .keep_alive_while_idle(true)
        .connect_with_connector(service_fn(move |_: Uri| {
            let command = command.take();
            async move {
                let command = command.ok_or_else(went)?;
                Piped::bridged(command).await.map(TokioIo::new)
            }
        }))
        .await?;
    Ok(daemon(channel, None, round_trip.answer))
}

fn went() -> io::Error {
    io::Error::new(ErrorKind::NotConnected, "the luned reached went")
}

#[derive(Debug)]
pub enum Unbridged {
    Io(io::Error),
    Ended(ExitStatus),
    Said(String),
    Silent,
}

impl fmt::Display for Unbridged {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unbridged::Io(error) => write!(f, "{error}"),
            Unbridged::Ended(ended) => write!(f, "it ended before it reached luned ({ended})"),
            Unbridged::Said(said) => write!(f, "it said {said:?} before it reached luned"),
            Unbridged::Silent => {
                write!(f, "it neither reached luned nor ended in {}s", REACH.as_secs())
            },
        }
    }
}

impl std::error::Error for Unbridged {}

impl From<io::Error> for Unbridged {
    fn from(error: io::Error) -> Self {
        Unbridged::Io(error)
    }
}

pub async fn stream(folder: &Folder) -> io::Result<UnixStream> {
    mine(UnixStream::connect(folder.socket()).await?)
}

struct Piped {
    stdout: BufReader<ChildStdout>,
    stdin: ChildStdin,
    _child: Child,
}

impl Piped {
    async fn bridged(command: std::process::Command) -> Result<Self, Unbridged> {
        let deadline = Instant::now() + REACH;
        let mut command = tokio::process::Command::from(command);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        let mut child = command.kill_on_drop(true).spawn()?;
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            return Err(io::Error::other("a command spawned piped has no pipes").into());
        };
        let mut stdout = BufReader::new(stdout);
        let mut said = Vec::new();
        let mut line = (&mut stdout).take(SAID_MAX);
        let heard = line.read_until(b'\n', &mut said);
        if let Ok(heard) = timeout_at(deadline, heard).await {
            heard?;
        }
        match said.as_slice() {
            BRIDGED => Ok(Piped { stdout, stdin, _child: child }),
            [] => match timeout_at(deadline, child.wait()).await {
                Ok(ended) => Err(Unbridged::Ended(ended?)),
                Err(_) => Err(Unbridged::Silent),
            },
            said => Err(Unbridged::Said(String::from_utf8_lossy(said).into_owned())),
        }
    }
}

impl AsyncRead for Piped {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdout).poll_read(context, buffer)
    }
}

impl AsyncWrite for Piped {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stdin).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stdin).poll_shutdown(context)
    }
}

pub async fn listening(folder: &Folder) -> io::Result<Option<PeerPid>> {
    let stream = mine(UnixStream::connect(folder.socket()).await?)?;
    Ok(stream.peer_cred()?.pid().and_then(PeerPid::from_raw))
}

fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on luned's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::future::pending;
    use std::io::ErrorKind;
    use std::pin::Pin;
    use std::process::Command;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, PoisonError};
    use std::time::Duration;

    use lune_protocol::socket::SocketId;
    use lune_waits::{eventually_async, Began, Patience};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::{mpsc, oneshot, Notify};
    use tokio::time::Instant;
    use tokio_stream::wrappers::{ReceiverStream, UnixListenerStream};
    use tokio_stream::{Stream, StreamExt as _};
    use tonic::body::Body;
    use tonic::codegen::http::{Request, Response};
    use tonic::transport::{Channel, Server};
    use tonic::{Code, Request as GrpcRequest, Response as GrpcResponse, Status, Streaming};
    use tower::{service_fn, BoxError, ServiceExt as _};

    use super::{
        connect_matched, keep, kept, kept_at, lock_kept, reach, through, unanswered, Answer, Gone,
        Kept, LocalWatch, PeerPid, Reached, RoundTrip, Shared, Tracked, Unasked, Unbridged,
        Unmatched, Unreached, Unsent, WatchError, Within, ANSWER, ASKED_MAX, ASKED_WAITING_MAX,
        CONNECTING, CONNECTING_MAX, HELD_MAX, KEPT_MAX, QUESTIONS_MAX, STREAM_WINDOW_BYTES,
    };
    use crate::changes::Unchanged;
    use crate::remote::Destination;
    use crate::api::daemon_client::DaemonClient;
    use crate::api::daemon_server::{Daemon as DaemonService, DaemonServer};
    use crate::api::{
        self, AnswerRequest, AskRequest, AskResponse, ChangeProjectsRequest,
        ChangeProjectsResponse, FileIssueRequest, FileIssueResponse, FramesRequest, InfoResponse,
        IssueRepositoriesRequest, IssueRepositoriesResponse, KeepChecksRequest,
        KeepChecksResponse, KeepImageRequest, KeepImageResponse, KeepPrintedRequest,
        KeepPrintedResponse, ListAgentsResponse, ListContentsRequest, ListContentsResponse,
        ListRepositoriesRequest, ListRepositoriesResponse, ListSkillsRequest, ListSkillsResponse,
        ListWorkRequest, ListWorkResponse, MakeWorktreeRequest, MakeWorktreeResponse,
        PasteRequest, PasteResponse, PruneRequest, PruneResponse, PrunesRequest, PrunesResponse,
        ReadDiffRequest, ReadDiffResponse, RebaseRequest, RebaseResponse, SayRequest, ShowRequest,
        ShowResponse, TranscriptEndsRequest, TranscriptEndsResponse, WaitingRequest,
        WaitingResponse, WatchRequest,
    };
    use crate::folder::{self, Folder};
    use crate::mux::own::api::ToViewer;
    use crate::{release, tasks};

    #[derive(Clone)]
    enum AgentsAnswer {
        Ok,
        Unavailable(String),
        Park { arrived: Arc<AtomicUsize>, release: Arc<Notify> },
    }

    #[derive(Clone)]
    struct Stub {
        version: String,
        built: Option<u64>,
        pid: u32,
        infos: Arc<AtomicUsize>,
        agents: Arc<Mutex<AgentsAnswer>>,
        flooded: Arc<AtomicUsize>,
        contents: String,
        watch_seq: Arc<AtomicU64>,
        watch_versioned: Arc<AtomicBool>,
        watch_afters: Arc<Mutex<Vec<Option<api::Version>>>>,
    }

    impl Stub {
        fn own() -> Stub {
            Stub {
                version: release::VERSION.to_owned(),
                built: release::BUILT,
                pid: std::process::id(),
                infos: Arc::new(AtomicUsize::new(0)),
                agents: Arc::new(Mutex::new(AgentsAnswer::Ok)),
                flooded: Arc::new(AtomicUsize::new(0)),
                contents: "x".repeat(32 * 1024),
                watch_seq: Arc::new(AtomicU64::new(0)),
                watch_versioned: Arc::new(AtomicBool::new(true)),
                watch_afters: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn serve(&self, folder: &Folder) -> Served {
            let tasks = tasks::shared();
            let socket = folder.socket();
            let _ = std::fs::remove_file(&socket);
            let std_listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
            std_listener.set_nonblocking(true).unwrap();
            let accepted = Arc::new(AtomicUsize::new(0));
            let counted = Arc::clone(&accepted);
            let (stop, stopping) = oneshot::channel::<()>();
            let stub = self.clone();
            let handle = tasks.spawn(async move {
                let listener = UnixListener::from_std(std_listener).unwrap();
                let incoming = UnixListenerStream::new(listener).map(move |stream| {
                    if stream.is_ok() {
                        counted.fetch_add(1, Ordering::Relaxed);
                    }
                    stream
                });
                let shutdown = async move {
                    let _ = stopping.await;
                };
                let _ = Server::builder()
                    .max_concurrent_streams(QUESTIONS_MAX)
                    .add_service(DaemonServer::new(stub))
                    .serve_with_incoming_shutdown(incoming, shutdown)
                    .await;
            });
            Served { accepted, stop: Some(stop), handle }
        }
    }

    struct Served {
        accepted: Arc<AtomicUsize>,
        stop: Option<oneshot::Sender<()>>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Served {
        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::Relaxed)
        }

        async fn stop(mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            let _ = (&mut self.handle).await;
        }
    }

    impl Drop for Served {
        fn drop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            self.handle.abort();
        }
    }

    fn unserved() -> Status {
        Status::unimplemented("unneeded in this test")
    }

    #[tonic::async_trait]
    impl DaemonService for Stub {
        type AskStream = Pin<Box<dyn Stream<Item = Result<AskResponse, Status>> + Send>>;
        type FileIssueStream = Pin<Box<dyn Stream<Item = Result<FileIssueResponse, Status>> + Send>>;
        type FramesStream = Pin<Box<dyn Stream<Item = Result<ToViewer, Status>> + Send>>;
        type KeepChecksStream =
            Pin<Box<dyn Stream<Item = Result<KeepChecksResponse, Status>> + Send>>;
        type KeepImageStream = Pin<Box<dyn Stream<Item = Result<KeepImageResponse, Status>> + Send>>;
        type ListWorkStream = Pin<Box<dyn Stream<Item = Result<ListWorkResponse, Status>> + Send>>;
        type MakeWorktreeStream =
            Pin<Box<dyn Stream<Item = Result<MakeWorktreeResponse, Status>> + Send>>;
        type PasteStream = Pin<Box<dyn Stream<Item = Result<PasteResponse, Status>> + Send>>;
        type PruneStream = Pin<Box<dyn Stream<Item = Result<PruneResponse, Status>> + Send>>;
        type PrunesStream = Pin<Box<dyn Stream<Item = Result<PrunesResponse, Status>> + Send>>;
        type ReadDiffStream = Pin<Box<dyn Stream<Item = Result<ReadDiffResponse, Status>> + Send>>;
        type RebaseStream = Pin<Box<dyn Stream<Item = Result<RebaseResponse, Status>> + Send>>;
        type ShowStream = Pin<Box<dyn Stream<Item = Result<ShowResponse, Status>> + Send>>;

        async fn info(&self, _: GrpcRequest<()>) -> Result<GrpcResponse<InfoResponse>, Status> {
            self.infos.fetch_add(1, Ordering::Relaxed);
            Ok(GrpcResponse::new(InfoResponse {
                version: self.version.clone(),
                pid: self.pid,
                started_at: None,
                claude_folder: None,
                built: self.built,
                agy_folder: None,
            }))
        }

        async fn list_agents(
            &self,
            _: GrpcRequest<()>,
        ) -> Result<GrpcResponse<ListAgentsResponse>, Status> {
            let answer = self.agents.lock().unwrap_or_else(PoisonError::into_inner).clone();
            match answer {
                AgentsAnswer::Ok => Ok(GrpcResponse::new(ListAgentsResponse::default())),
                AgentsAnswer::Unavailable(why) => Err(Status::unavailable(why)),
                AgentsAnswer::Park { arrived, release } => {
                    let notified = release.notified();
                    arrived.fetch_add(1, Ordering::Relaxed);
                    notified.await;
                    Ok(GrpcResponse::new(ListAgentsResponse::default()))
                },
            }
        }

        async fn watch(
            &self,
            request: GrpcRequest<WatchRequest>,
        ) -> Result<GrpcResponse<api::Host>, Status> {
            self.watch_afters
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request.into_inner().after);
            let version = self.watch_versioned.load(Ordering::Relaxed).then(|| {
                let seq = self.watch_seq.fetch_add(1, Ordering::Relaxed) + 1;
                api::Version { pid: self.pid, seq }
            });
            Ok(GrpcResponse::new(api::Host { version, ..api::Host::default() }))
        }

        async fn list_contents(
            &self,
            _: GrpcRequest<ListContentsRequest>,
        ) -> Result<GrpcResponse<ListContentsResponse>, Status> {
            Ok(GrpcResponse::new(ListContentsResponse { listed: self.contents.clone() }))
        }

        async fn ask(
            &self,
            request: GrpcRequest<AskRequest>,
        ) -> Result<GrpcResponse<Self::AskStream>, Status> {
            let words = request.into_inner().words;
            if words == "flood" {
                let (sent, heard) = mpsc::channel(1);
                let flooded = Arc::clone(&self.flooded);
                tokio::spawn(async move {
                    let chunk = "x".repeat(16 * 1024);
                    for _ in 0..256 {
                        let step = Some(api::ask_response::Step::Said(chunk.clone()));
                        if sent.send(Ok(AskResponse { step })).await.is_err() {
                            return;
                        }
                        flooded.fetch_add(chunk.len(), Ordering::Relaxed);
                    }
                });
                return Ok(GrpcResponse::new(Box::pin(ReceiverStream::new(heard))));
            }
            let (sent, heard) = mpsc::channel(1);
            let step = Some(api::ask_response::Step::Said("held".to_owned()));
            let _ = sent.send(Ok(AskResponse { step })).await;
            let hold = ReceiverStream::new(heard).chain(tokio_stream::pending());
            Ok(GrpcResponse::new(Box::pin(hold)))
        }

        async fn list_skills(
            &self,
            _: GrpcRequest<ListSkillsRequest>,
        ) -> Result<GrpcResponse<ListSkillsResponse>, Status> {
            Err(unserved())
        }

        async fn keep_checks(
            &self,
            _: GrpcRequest<KeepChecksRequest>,
        ) -> Result<GrpcResponse<Self::KeepChecksStream>, Status> {
            Err(unserved())
        }

        async fn keep_printed(
            &self,
            _: GrpcRequest<KeepPrintedRequest>,
        ) -> Result<GrpcResponse<KeepPrintedResponse>, Status> {
            Err(unserved())
        }

        async fn list_repositories(
            &self,
            _: GrpcRequest<ListRepositoriesRequest>,
        ) -> Result<GrpcResponse<ListRepositoriesResponse>, Status> {
            Err(unserved())
        }

        async fn waiting(
            &self,
            _: GrpcRequest<WaitingRequest>,
        ) -> Result<GrpcResponse<WaitingResponse>, Status> {
            Err(unserved())
        }

        async fn answer(&self, _: GrpcRequest<AnswerRequest>) -> Result<GrpcResponse<()>, Status> {
            Err(unserved())
        }

        async fn transcript_ends(
            &self,
            _: GrpcRequest<TranscriptEndsRequest>,
        ) -> Result<GrpcResponse<TranscriptEndsResponse>, Status> {
            Err(unserved())
        }

        async fn say(&self, _: GrpcRequest<SayRequest>) -> Result<GrpcResponse<()>, Status> {
            Err(unserved())
        }

        async fn paste(
            &self,
            _: GrpcRequest<PasteRequest>,
        ) -> Result<GrpcResponse<Self::PasteStream>, Status> {
            Err(unserved())
        }

        async fn prunes(
            &self,
            _: GrpcRequest<PrunesRequest>,
        ) -> Result<GrpcResponse<Self::PrunesStream>, Status> {
            Err(unserved())
        }

        async fn prune(
            &self,
            _: GrpcRequest<PruneRequest>,
        ) -> Result<GrpcResponse<Self::PruneStream>, Status> {
            Err(unserved())
        }

        async fn rebase(
            &self,
            _: GrpcRequest<RebaseRequest>,
        ) -> Result<GrpcResponse<Self::RebaseStream>, Status> {
            Err(unserved())
        }

        async fn file_issue(
            &self,
            _: GrpcRequest<FileIssueRequest>,
        ) -> Result<GrpcResponse<Self::FileIssueStream>, Status> {
            Err(unserved())
        }

        async fn issue_repositories(
            &self,
            _: GrpcRequest<IssueRepositoriesRequest>,
        ) -> Result<GrpcResponse<IssueRepositoriesResponse>, Status> {
            Err(unserved())
        }

        async fn keep_image(
            &self,
            _: GrpcRequest<Streaming<KeepImageRequest>>,
        ) -> Result<GrpcResponse<Self::KeepImageStream>, Status> {
            Err(unserved())
        }

        async fn show(
            &self,
            _: GrpcRequest<Streaming<ShowRequest>>,
        ) -> Result<GrpcResponse<Self::ShowStream>, Status> {
            Err(unserved())
        }

        async fn frames(
            &self,
            _: GrpcRequest<Streaming<FramesRequest>>,
        ) -> Result<GrpcResponse<Self::FramesStream>, Status> {
            Err(unserved())
        }

        async fn change_projects(
            &self,
            _: GrpcRequest<ChangeProjectsRequest>,
        ) -> Result<GrpcResponse<ChangeProjectsResponse>, Status> {
            Err(unserved())
        }

        async fn make_worktree(
            &self,
            _: GrpcRequest<MakeWorktreeRequest>,
        ) -> Result<GrpcResponse<Self::MakeWorktreeStream>, Status> {
            Err(unserved())
        }

        async fn list_work(
            &self,
            _: GrpcRequest<ListWorkRequest>,
        ) -> Result<GrpcResponse<Self::ListWorkStream>, Status> {
            Err(unserved())
        }

        async fn read_diff(
            &self,
            _: GrpcRequest<ReadDiffRequest>,
        ) -> Result<GrpcResponse<Self::ReadDiffStream>, Status> {
            Err(unserved())
        }
    }

    fn temp_folder() -> (tempfile::TempDir, Folder) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lune");
        let folder = folder::make(&path).unwrap();
        (dir, folder)
    }

    #[test]
    fn lune_keeps_one_connection_to_luned_and_checks_its_build_once_until_it_goes() {
        let (_dir, folder) = temp_folder();
        let stub = Stub::own();
        let served = stub.serve(&folder);

        let transient = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let Reached::Matched(first) = transient.block_on(reach(&folder)).unwrap() else {
            panic!("the own build did not match");
        };
        drop(transient);

        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(second) = reach(&folder).await.unwrap() else {
                panic!("the cached channel did not match");
            };
            second.list_agents(ANSWER).await.unwrap();
            assert_eq!(served.accepted(), 1, "a kept channel opens no second connection");
            assert_eq!(stub.infos.load(Ordering::Relaxed), 1, "build is checked once per channel");

            *stub.agents.lock().unwrap() =
                AgentsAnswer::Unavailable("luned does not reach far now".to_owned());
            let far = second.list_agents(ANSWER).await;
            let Err(Unasked::Asked(status)) = far else {
                panic!("a server Unavailable did not arrive as Unasked::Asked");
            };
            assert_eq!(status.code(), Code::Unavailable);
            assert!(
                !first.gone.is(),
                "a server-returned Unavailable leaves the local channel kept"
            );
            *stub.agents.lock().unwrap() = AgentsAnswer::Ok;
            let Reached::Matched(third) = reach(&folder).await.unwrap() else {
                panic!("the kept channel vanished after a server-returned Unavailable");
            };
            third.list_agents(ANSWER).await.unwrap();
            assert_eq!(served.accepted(), 1, "the same connection serves calls after Unavailable");
            assert_eq!(stub.infos.load(Ordering::Relaxed), 1);
        });

        tasks.block_on(async {
            served.stop().await;
            eventually_async("lune sees its closed connection go", async || {
                first.gone.is().then_some(())
            })
            .await;
            assert!(kept(&folder).is_none(), "a closed connection evicts its kept channel");
            let gone = first.list_agents(ANSWER).await.unwrap_err();
            assert!(
                matches!(gone, Unasked::Unsent(Unsent::ChannelGone)),
                "a gone channel refuses before sending: {gone:?}"
            );
        });

        let fresh = stub.serve(&folder);
        tasks.block_on(async {
            let Reached::Matched(again) = reach(&folder).await.unwrap() else {
                panic!("a fresh luned did not match");
            };
            again.list_agents(ANSWER).await.unwrap();
            assert_eq!(fresh.accepted(), 1);
            assert_eq!(stub.infos.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    fn a_replaced_socket_rechecks_luned_s_build_and_never_caches_another_build() {
        let (_dir, folder) = temp_folder();
        let own = Stub::own();
        let _old = own.serve(&folder);

        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(first) = reach(&folder).await.unwrap() else {
                panic!("the own build did not match");
            };
            let old_id = SocketId::at(&folder.socket()).unwrap();
            first.list_agents(ANSWER).await.unwrap();
            assert!(kept(&folder).is_some());

            let next_own = Stub::own();
            let next_served = next_own.serve(&folder);
            let Reached::Matched(second) = reach(&folder).await.unwrap() else {
                panic!("a replaced own build did not match");
            };
            let stale_gone = Gone::default();
            let stale = Kept {
                shared: super::Shared::new(first.channel.clone(), stale_gone.clone()),
                socket_id: old_id,
            };
            let returned = keep(&folder.socket(), stale)
                .expect("a stale keep returns the already-kept live channel for the new socket");
            assert!(stale_gone.is(), "a stale channel arriving after restart is marked gone");
            assert!(!returned.gone.is(), "keep never returns a gone channel");
            assert!(!second.gone.is(), "a stale keep leaves the live channel untouched");
            let Reached::Matched(still) = reach(&folder).await.unwrap() else {
                panic!("the live channel did not stay kept after a stale keep");
            };
            still.list_agents(ANSWER).await.unwrap();
            assert_eq!(next_served.accepted(), 1);

            let mut newer = Stub::own();
            newer.version = "99.0.0".to_owned();
            newer.built = None;
            newer.pid = 4242;
            let replaced = newer.serve(&folder);

            let Reached::Unmatched(unmatched) = reach(&folder).await.unwrap() else {
                panic!("a replaced luned of a newer build was not refused");
            };
            assert!(unmatched.other().is_newer());
            assert_eq!(unmatched.pid(), PeerPid::new(4242));
            assert!(second.gone.is(), "replacing the socket marks the old channel gone");
            assert!(kept(&folder).is_none(), "another build is never kept in the cache");
            let stale_no_live = Kept {
                shared: super::Shared::new(first.channel.clone(), Gone::default()),
                socket_id: old_id,
            };
            assert!(
                keep(&folder.socket(), stale_no_live).is_none(),
                "a stale keep with no live cached entry returns None rather than a gone channel"
            );

            let Reached::Unmatched(_) = reach(&folder).await.unwrap() else {
                panic!("a second reach to another build was not refused");
            };
            assert_eq!(replaced.accepted(), 2, "unmatched builds connect afresh each time");
            assert_eq!(newer.infos.load(Ordering::Relaxed), 2);
        });
    }

    #[test]
    fn a_connection_hyper_lets_go_of_or_errors_on_marks_its_channel_gone() {
        let tasks = tasks::shared();
        let _entered = tasks.enter();
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        ours.set_nonblocking(true).unwrap();
        let gone = Gone::default();
        let stream = UnixStream::from_std(ours).unwrap();
        let tracked = Tracked { stream, gone: gone.clone() };
        assert!(!gone.is(), "a connection held is not gone");
        drop(tracked);
        assert!(gone.is(), "a connection let go of without a word is gone");

        tasks.block_on(async {
            const IMPATIENT: Answer = Answer::of(Duration::from_millis(50));
            let gone = Gone::default();
            let ok = service_fn(|()| async { Ok::<_, &'static str>(()) });
            let mut watched = Within::watched(ok, ANSWER.waits(), unanswered, gone.clone());
            watched.ready().await.unwrap();
            watched.oneshot(()).await.unwrap();
            assert!(!gone.is(), "an Ok transport response leaves the channel alive");

            let silent = service_fn(|()| pending::<Result<(), &'static str>>());
            let watched = Within::watched(silent, IMPATIENT.waits(), unanswered, gone.clone());
            let late = watched.oneshot(()).await.unwrap_err();
            assert_eq!(late.downcast_ref::<Status>().unwrap().code(), Code::DeadlineExceeded);
            assert!(!gone.is(), "a call deadline leaves the channel alive");

            let broken = service_fn(|()| async { Err::<(), _>("goaway") });
            let watched = Within::watched(broken, ANSWER.waits(), unanswered, gone.clone());
            assert!(watched.oneshot(()).await.is_err());
            assert!(gone.is(), "a transport error marks the channel gone");
        });
    }

    #[test]
    fn lune_bounds_held_streams_and_keeps_room_for_questions_on_one_channel() {
        let (_dir, folder) = temp_folder();
        let stub = Stub::own();
        let served = stub.serve(&folder);

        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(shared) = reach(&folder).await.unwrap() else {
                panic!("the own build did not match");
            };
            let mut watching = shared.watching(ANSWER, LocalWatch::Client).unwrap();
            let first = watching.next().await.unwrap();
            let second = watching.next().await.unwrap();
            assert_eq!(
                *stub.watch_afters.lock().unwrap(),
                vec![None, Some(first.version)],
                "Watching::next parks past the last seen version on every call after the first"
            );
            assert!(second.version.seq > first.version.seq);

            let mut held = Vec::new();
            for _ in 0..(HELD_MAX - 1) {
                let mut stream = shared
                    .ask(ANSWER, AskRequest::default())
                    .await
                    .expect("a stream within HELD_MAX opens");
                assert!(stream.message().await.unwrap().is_some());
                held.push(stream);
            }
            let refused = shared
                .ask(ANSWER, AskRequest::default())
                .await
                .err()
                .expect("a stream past HELD_MAX is refused");
            assert!(matches!(refused, Unasked::Unsent(Unsent::HoldsFull)), "{refused:?}");
            assert!(matches!(Unchanged::of_asked(&refused), Unchanged::Refused(_)));
            let nixos: Destination = "nixos".parse().unwrap();
            assert_eq!(
                refused.said(Some(&nixos)),
                format!("lune holds at most {HELD_MAX} streams to luned at once"),
                "a local budget refusal is never attributed to a remote machine"
            );
            let refused_watch =
                shared.watching(ANSWER, LocalWatch::Client).err().expect("watch shares HELD_MAX");
            assert_eq!(refused_watch, Unsent::HoldsFull);
            assert!(!shared.gone.is(), "refusing past HELD_MAX leaves the channel kept");

            shared.list_agents(ANSWER).await.unwrap();
            assert_eq!(served.accepted(), 1, "questions share the same connection while HELD is full");

            drop(held.pop());
            let mut freed = shared
                .ask(ANSWER, AskRequest::default())
                .await
                .expect("dropping a held stream makes room for another");
            assert!(freed.message().await.unwrap().is_some());
            drop(watching);
            drop(held);
            drop(freed);

            stub.watch_versioned.store(false, Ordering::Relaxed);
            let mut unversioned = shared.watching(ANSWER, LocalWatch::Window).unwrap();
            assert!(
                matches!(unversioned.next().await, Err(WatchError::Unversioned)),
                "Watching::next rejects an answer with no version"
            );
        });
    }

    #[test]
    fn short_question_bursts_wait_their_turn_up_to_the_bound_and_timeout_cleanly() {
        const IMPATIENT: Answer = Answer::of(Duration::from_millis(50));
        let (_dir, folder) = temp_folder();
        let stub = Stub::own();
        let arrived = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());
        *stub.agents.lock().unwrap() =
            AgentsAnswer::Park { arrived: Arc::clone(&arrived), release: Arc::clone(&release) };
        let served = stub.serve(&folder);

        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(shared) = reach(&folder).await.unwrap() else {
                panic!("the own build did not match");
            };
            let mut active = Vec::new();
            for _ in 0..ASKED_MAX {
                let client = shared.clone();
                active.push(tasks.spawn(async move { client.list_agents(ANSWER).await }));
            }
            eventually_async("all ASKED_MAX questions reach the server", async || {
                (arrived.load(Ordering::Relaxed) == ASKED_MAX).then_some(())
            })
            .await;

            let mut waiting = Vec::new();
            for _ in 0..ASKED_WAITING_MAX {
                let client = shared.clone();
                waiting.push(tasks.spawn(async move { client.list_agents(ANSWER).await }));
            }
            eventually_async("all ASKED_WAITING_MAX waiters queue up", async || {
                (shared.budget.waiting.available() == 0).then_some(())
            })
            .await;

            let overflow = shared
                .list_agents(ANSWER)
                .await
                .expect_err("a question past ASKED_WAITING_MAX is refused");
            assert!(matches!(overflow, Unasked::Unsent(Unsent::QueueFull)), "{overflow:?}");
            assert!(matches!(Unchanged::of_asked(&overflow), Unchanged::Refused(_)));
            assert!(overflow.to_string().contains(&format!("at most {ASKED_WAITING_MAX} questions")));
            assert!(!shared.gone.is(), "queue overflow leaves the channel kept");

            *stub.agents.lock().unwrap() = AgentsAnswer::Ok;
            release.notify_waiters();
            let patience = Patience::begun();
            for handle in active.into_iter().chain(waiting) {
                let answered = patience.awaited("a queued question completes", handle).await;
                answered.unwrap().unwrap();
            }
            assert_eq!(served.accepted(), 1, "the whole burst rides one connection");

            let stuck_arrived = Arc::new(AtomicUsize::new(0));
            let stuck_release = Arc::new(Notify::new());
            *stub.agents.lock().unwrap() = AgentsAnswer::Park {
                arrived: Arc::clone(&stuck_arrived),
                release: Arc::clone(&stuck_release),
            };
            let mut stuck = Vec::new();
            for _ in 0..ASKED_MAX {
                let client = shared.clone();
                stuck.push(tasks.spawn(async move { client.list_agents(ANSWER).await }));
            }
            eventually_async("stuck questions fill ASKED_MAX", async || {
                (stuck_arrived.load(Ordering::Relaxed) == ASKED_MAX).then_some(())
            })
            .await;

            let began = Began::now();
            let late = shared
                .list_agents(IMPATIENT)
                .await
                .expect_err("a queued question times out when ASKED_MAX stay stuck");
            began.lasted().at_least(IMPATIENT.waits(), "a waiting question");
            assert!(
                matches!(late, Unasked::Unsent(Unsent::TimedOut(wait)) if wait == IMPATIENT.waits()),
                "{late:?}"
            );
            assert!(matches!(Unchanged::of_asked(&late), Unchanged::Refused(_)));
            assert_eq!(late.to_string(), "luned did not answer in 0.05s");
            stuck_release.notify_waiters();
            let patience = Patience::begun();
            for handle in stuck {
                let _ = patience.awaited("stuck question released", handle).await;
            }
        });
    }

    #[test]
    fn a_question_is_answered_while_unread_streams_fill_their_windows_on_the_same_channel() {
        const FLOODERS: usize = 3;
        let (_dir, folder) = temp_folder();
        let stub = Stub::own();
        let served = stub.serve(&folder);

        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(shared) = reach(&folder).await.unwrap() else {
                panic!("the own build did not match");
            };
            let mut flooders = Vec::new();
            for _ in 0..FLOODERS {
                let request = AskRequest { words: "flood".to_owned(), ..AskRequest::default() };
                let stream = shared.ask(ANSWER, request).await.unwrap();
                flooders.push(stream);
            }
            let filled = FLOODERS * (STREAM_WINDOW_BYTES as usize);
            eventually_async("unread streams fill their stream windows", async || {
                (stub.flooded.load(Ordering::Relaxed) >= filled).then_some(())
            })
            .await;

            let listed = shared
                .list_contents(ANSWER, ListContentsRequest::default())
                .await
                .expect("a question gets its answer while unread streams fill their windows");
            assert_eq!(listed.listed.len(), stub.contents.len());
            assert_eq!(served.accepted(), 1);
            drop(flooders);
        });
    }

    #[test]
    fn an_expired_deadline_refuses_an_ask_without_sending_it() {
        let tasks = tasks::shared();
        tasks.block_on(async {
            let channel = Channel::from_static("http://[::1]:1").connect_lazy();
            let shared = Shared::new(channel, Gone::default());
            let sent = AtomicBool::new(false);
            let late = shared
                .ask_until(Instant::now(), ANSWER, |mut daemon| {
                    sent.store(true, Ordering::Relaxed);
                    async move { daemon.list_agents(()).await }
                })
                .await
                .expect_err("an expired deadline refuses before sending");
            assert!(!sent.load(Ordering::Relaxed), "an expired ask never sends its RPC");
            assert!(
                matches!(late, Unasked::Unsent(Unsent::TimedOut(wait)) if wait == ANSWER.waits()),
                "{late:?}"
            );
            assert!(matches!(Unchanged::of_asked(&late), Unchanged::Refused(_)));
        });
    }

    #[test]
    fn build_verification_bounds_a_peer_that_accepts_but_never_becomes_ready() {
        const IMPATIENT: Answer = Answer::of(Duration::from_millis(50));
        const ZERO_STREAMS_SETTINGS: [u8; 15] = [0, 0, 6, 4, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0];
        let (_dir, folder) = temp_folder();
        let socket = folder.socket();
        let std_listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let tasks = tasks::shared();
        let peer = tasks.spawn(async move {
            let listener = UnixListener::from_std(std_listener).unwrap();
            let (mut accepted, _) = listener.accept().await.unwrap();
            accepted.write_all(&ZERO_STREAMS_SETTINGS).await.unwrap();
            let mut buf = [0_u8; 64];
            while matches!(accepted.read(&mut buf).await, Ok(read) if read > 0) {}
        });
        tasks.block_on(async {
            let began = Began::now();
            let result = connect_matched(socket, IMPATIENT).await;
            let Err(Unreached::Asked(late)) = result else {
                panic!("a peer with zero concurrent streams did not time out in poll_ready");
            };
            began.lasted().at_least(IMPATIENT.waits(), "build verification wait");
            assert_eq!(late.code(), Code::DeadlineExceeded, "{late:?}");
            assert_eq!(late.message(), "luned did not answer in 0.05s");
            Patience::begun()
                .awaited("timed-out build verification closes its socket", peer)
                .await
                .unwrap();
        });
    }

    #[test]
    fn kept_channels_evict_the_first_socket_at_kept_max_and_connects_stop_at_connecting_max() {
        let tasks = tasks::shared();
        let _entered = tasks.enter();
        let dir = tempfile::tempdir().unwrap();
        let mut listeners = Vec::new();
        let mut paths = Vec::new();
        let mut created = Vec::new();
        for index in 0..=KEPT_MAX {
            let path = dir.path().join(format!("s{index:02}.sock"));
            listeners.push(std::os::unix::net::UnixListener::bind(&path).unwrap());
            let id = SocketId::at(&path).unwrap();
            let channel = Channel::from_static("http://[::1]:1").connect_lazy();
            let shared = Shared::new(channel, Gone::default());
            let stored = keep(&path, Kept { shared, socket_id: id }).unwrap();
            created.push(stored);
            paths.push(path);
        }
        assert!(kept_at(&paths[0]).is_none(), "the first entry is evicted past KEPT_MAX");
        assert!(kept_at(&paths[KEPT_MAX]).is_some(), "the newest entry stays kept");
        assert!(lock_kept().len() <= KEPT_MAX);
        for shared in &created {
            shared.gone.mark();
        }
        assert!(kept_at(&paths[KEPT_MAX]).is_none());
        drop(listeners);

        let (_folder_dir, folder) = temp_folder();
        let mut held = Vec::with_capacity(CONNECTING_MAX);
        for _ in 0..CONNECTING_MAX {
            held.push(CONNECTING.try_acquire().unwrap());
        }
        let refused = tasks.block_on(reach(&folder));
        assert!(
            matches!(refused, Err(Unreached::Busy)),
            "a connect past CONNECTING_MAX is refused as Unreached::Busy"
        );
        assert!(Unreached::Busy.to_string().contains(&format!("at most {CONNECTING_MAX}")));
        drop(held);
    }

    #[test]
    fn a_symlink_alias_to_the_same_folder_reuses_one_kept_channel_and_peer_pid_rejects_invalid_ids() {
        let (dir, folder) = temp_folder();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(folder.path(), &alias).unwrap();
        let via_parent = dir.path().join("parent");
        std::os::unix::fs::symlink(dir.path(), &via_parent).unwrap();
        let aliased = folder::open(&via_parent.join("lune")).unwrap().unwrap();
        assert_eq!(folder.path(), aliased.path(), "Folder canonicalizes parent path aliases");
        assert!(folder::open(&alias).is_err(), "a leaf symlink is still refused as NotYours");

        let stub = Stub::own();
        let served = stub.serve(&folder);
        let tasks = tasks::shared();
        tasks.block_on(async {
            let Reached::Matched(first) = reach(&folder).await.unwrap() else {
                panic!("the own build did not match");
            };
            let Reached::Matched(second) = reach(&aliased).await.unwrap() else {
                panic!("the aliased folder did not match");
            };
            first.list_agents(ANSWER).await.unwrap();
            second.list_agents(ANSWER).await.unwrap();
            assert_eq!(served.accepted(), 1, "aliased folder paths share one kept channel");
            assert_eq!(stub.infos.load(Ordering::Relaxed), 1);
        });

        assert_eq!(PeerPid::new(0), None);
        assert_eq!(PeerPid::new(1), None);
        assert_eq!(PeerPid::new(u32::MAX), None);
        assert_eq!(PeerPid::from_raw(0), None);
        assert_eq!(PeerPid::from_raw(1), None);
        assert_eq!(PeerPid::from_raw(-1), None);
        let valid = PeerPid::new(4242).unwrap();
        assert_eq!(valid.get(), 4242);

        let release::Of::Other(newer) = release::of("99.0.0", None) else {
            panic!("99.0.0 is another build");
        };
        let without_pid = Unmatched::of(newer, None).to_string();
        assert!(!without_pid.contains("pid"), "an absent peer pid is omitted: {without_pid}");
    }

    #[test]
    fn a_call_past_its_answer_says_luned_did_not_answer_in_time() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let silent = service_fn(|_: Request<Body>| pending::<Result<Response<Body>, BoxError>>());
        let mut daemon = DaemonClient::new(Within::new(silent, Duration::from_secs(1), unanswered));
        let late = runtime.block_on(daemon.info(())).unwrap_err();
        let said = (late.code(), late.message());
        assert_eq!(said, (Code::DeadlineExceeded, "luned did not answer in 1s"));
    }

    #[test]
    fn a_command_that_stalls_ends_its_call_on_the_keepalive_past_its_round_trip_and_runs_once() {
        const STALLS: RoundTrip = RoundTrip::of(Duration::from_secs(1));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut stalls = Command::new("sh");
        stalls.args(["-c", "echo luned; exec sleep 60"]);
        runtime.block_on(async {
            let mut stalled = through(stalls, STALLS).await.unwrap();
            let began = Began::now();
            let ended = stalled.info(()).await.expect_err("a stalled command answered");
            began.lasted().at_least(STALLS.pong, "a stalled call");
            assert_eq!(ended.code(), Code::Unavailable, "{ended:?}");
            let keepalive = sources(&ended).any(|error| error.to_string() == "keep-alive timed out");
            assert!(keepalive, "{ended:?}");
            let again = stalled.info(()).await.expect_err("it ran a command again");
            let went = sources(&again).filter_map(|error| error.downcast_ref::<Unbridged>()).any(|why| {
                matches!(why, Unbridged::Io(error) if error.kind() == ErrorKind::NotConnected)
            });
            assert!(went, "{again:?}");
        });
    }

    fn sources<'a>(
        error: &'a (dyn Error + 'static),
    ) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
        std::iter::successors(Some(error), |&error| error.source())
    }

    #[test]
    fn a_channel_going_during_cache_lookup_or_insert_never_panics_or_poisons_kept() {
        const ROUNDS: usize = 512;
        let tasks = tasks::shared();
        let _entered = tasks.enter();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("racy.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let id = SocketId::at(&path).unwrap();
        let (to_marker, from_main) = std::sync::mpsc::channel::<(Gone, usize)>();
        let barrier = Arc::new(Barrier::new(2));
        let marker_barrier = Arc::clone(&barrier);
        let marker = std::thread::spawn(move || {
            while let Ok((gone, spins)) = from_main.recv() {
                marker_barrier.wait();
                for _ in 0..spins {
                    std::hint::spin_loop();
                }
                gone.mark();
                marker_barrier.wait();
            }
        });
        for round in 0..ROUNDS {
            let channel = Channel::from_static("http://[::1]:1").connect_lazy();
            let gone = Gone::default();
            let shared = Shared::new(channel, gone.clone());
            let spins = (round % 128) * 16;
            match round % 3 {
                0 => {
                    let _ = keep(&path, Kept { shared, socket_id: id });
                    to_marker.send((gone, spins)).unwrap();
                    barrier.wait();
                    let _ = kept_at(&path);
                    barrier.wait();
                },
                1 => {
                    to_marker.send((gone, spins)).unwrap();
                    barrier.wait();
                    let _ = keep(&path, Kept { shared, socket_id: id });
                    barrier.wait();
                },
                _ => {
                    let _ = keep(&path, Kept { shared, socket_id: id });
                    let second = Channel::from_static("http://[::1]:1").connect_lazy();
                    let fresh_gone = Gone::default();
                    let fresh = Shared::new(second, fresh_gone.clone());
                    to_marker.send((gone, spins)).unwrap();
                    barrier.wait();
                    let _ = keep(&path, Kept { shared: fresh, socket_id: id });
                    barrier.wait();
                    fresh_gone.mark();
                },
            }
        }
        drop(to_marker);
        marker.join().unwrap();
        assert!(kept_at(&path).is_none(), "every marked channel is evicted without poisoning KEPT");
    }

    #[test]
    fn cancelling_reach_aborts_its_connect_task_and_bounds_live_connects() {
        const ZERO_STREAMS_SETTINGS: [u8; 15] = [0, 0, 6, 4, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0];
        let (_dir, folder) = temp_folder();
        let socket = folder.socket();
        let std_listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (accepted_tx, mut accepted_rx) = mpsc::unbounded_channel::<()>();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let tasks = tasks::shared();
        let peer_live = Arc::clone(&live);
        let peer_peak = Arc::clone(&peak);
        let peer = tasks.spawn(async move {
            let listener = UnixListener::from_std(std_listener).unwrap();
            let mut readers = Vec::new();
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    incoming = listener.accept() => {
                        let Ok((mut stream, _)) = incoming else {
                            break;
                        };
                        let now = peer_live.fetch_add(1, Ordering::Relaxed) + 1;
                        peer_peak.fetch_max(now, Ordering::Relaxed);
                        let reader_live = Arc::clone(&peer_live);
                        let accepted = accepted_tx.clone();
                        readers.push(tokio::spawn(async move {
                            let _ = stream.write_all(&ZERO_STREAMS_SETTINGS).await;
                            let _ = accepted.send(());
                            let mut buf = [0_u8; 64];
                            while matches!(stream.read(&mut buf).await, Ok(read) if read > 0) {}
                            reader_live.fetch_sub(1, Ordering::Relaxed);
                        }));
                    },
                }
            }
            for reader in readers {
                reader.abort();
            }
        });
        tasks.block_on(async {
            let mut held = Vec::with_capacity(CONNECTING_MAX - 1);
            for _ in 0..(CONNECTING_MAX - 1) {
                held.push(CONNECTING.try_acquire().unwrap());
            }
            let patience = Patience::begun();
            for _ in 0..=CONNECTING_MAX {
                eventually_async("a cancelled reach frees its CONNECTING slot", async || {
                    tokio::select! {
                        result = reach(&folder) => match result {
                            Err(Unreached::Busy) => None,
                            Err(error) => panic!("a stalled peer failed: {error:?}"),
                            Ok(_) => panic!("a stalled peer answered"),
                        },
                        ready = accepted_rx.recv() => {
                            assert!(ready.is_some());
                            Some(())
                        },
                    }
                })
                .await;
            }
            assert!(
                peak.load(Ordering::Relaxed) <= CONNECTING_MAX,
                "cancelled reaches opened {} simultaneous sockets past CONNECTING_MAX ({CONNECTING_MAX})",
                peak.load(Ordering::Relaxed)
            );
            eventually_async("cancelled reaches close their sockets", async || {
                (live.load(Ordering::Relaxed) == 0).then_some(())
            })
            .await;
            let last = eventually_async("the last CONNECTING slot returns", async || {
                CONNECTING.try_acquire()
            })
            .await;
            drop(last);
            drop(held);
            let _ = stop_tx.send(());
            patience.awaited("stalled peer task ends", peer).await.unwrap();
        });
    }

    #[test]
    fn a_bound_in_part_of_a_second_is_said_exactly() {
        assert_eq!(unanswered(Duration::from_millis(1500)), "luned did not answer in 1.5s");
        assert_eq!(unanswered(Duration::from_millis(250)), "luned did not answer in 0.25s");
    }
}
