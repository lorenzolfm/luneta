use std::cell::Cell;
use std::convert::Infallible;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hyper_util::rt::TokioIo;
use lune_waits::{Span, eventually, eventually_showing};
use luned::mux::own::api::mux_client::MuxClient;
use luned::mux::own::api::{self as mux_api, InfoRequest, InfoResponse, ListRequest, Listing};
use luned::mux::own::server;
use luned::{ALONE, PeerPid, ProcessExit, SWEEP};
use tempfile::TempDir;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::Status;
use tonic::codegen::{Body, BoxFuture, Context, Poll, Service, StdError, http};
use tonic::server::{Grpc, NamedService, UnaryService};
use tonic::service::Routes;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tonic_prost::ProstCodec;

use crate::demand::build_of;
use crate::host::{Daemon, Host, LUNED, Running, Slot, script, start, start_as};

const STAND_IN_MUX: &str = "LUNE_TEST_STAND_IN_MUX_FOLDER";

const STAND_IN_SAYS: &str = "LUNE_TEST_STAND_IN_MUX_SAYS";

const LOCK: &str = "mux.lock";

const WAITS: [&str; 2] = ["sleep", "600"];

pub fn built_server() -> PathBuf {
    let program = Path::new(LUNED).with_file_name("lune-mux");
    let shown = program.display();
    let rebuild = "dev test builds it, and so does cargo build -p lune-mux";
    let built = fs::metadata(&program).and_then(|it| it.modified());
    let built = built.unwrap_or_else(|error| panic!("{shown}: {error}: {rebuild}"));
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let mut sources = Vec::new();
    let built_from = [
        "crates/mux/src",
        "crates/mux/build.rs",
        "crates/protocol/src",
        "crates/protocol/build.rs",
        "proto/lune",
    ];
    for source in built_from {
        files_in(&root.join(source), &mut sources);
    }
    assert!(
        sources.len() > 2,
        "lune-mux's sources are not in {}",
        root.display()
    );
    for source in sources {
        let changed = fs::metadata(&source).and_then(|it| it.modified());
        let fresh = changed.is_ok_and(|changed| changed <= built);
        assert!(
            fresh,
            "{} changed since {shown} was built: {rebuild}",
            source.display()
        );
    }
    program
}

fn files_in(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        files.push(path.to_owned());
        return;
    };
    for entry in entries {
        files_in(&entry.unwrap().path(), files);
    }
}

pub struct Owned {
    daemon: Option<Daemon>,
    pub host: Host,
    runtime: tokio::runtime::Runtime,
    mux_stalled: Cell<bool>,
}

impl Owned {
    pub fn new() -> Self {
        built_server();
        let host = Host::new();
        let daemon = start(host.vars()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        Owned {
            daemon: Some(daemon),
            host,
            runtime,
            mux_stalled: Cell::new(false),
        }
    }

    pub fn stop_daemon(&mut self) {
        self.daemon = None;
    }

    pub fn daemon_ended(&mut self, what: &str) -> std::process::ExitStatus {
        let mut daemon = self.daemon.take().expect("a daemon was started");
        daemon.ended(what)
    }

    pub fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "one daemon runs at a time");
        self.daemon = Some(start(self.host.vars()).unwrap());
    }

    pub fn start_daemon_from(&mut self, luned: &Path) {
        assert!(self.daemon.is_none(), "one daemon runs at a time");
        let luned = self.host.vars().command(luned.to_str().unwrap());
        self.daemon = Some(start_as(luned).unwrap());
    }

    pub fn block_on<F: Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }

    pub fn home(&self) -> String {
        self.host.home.path().to_str().unwrap().to_owned()
    }

    pub fn socket(&self) -> PathBuf {
        self.host.folder().join(server::SOCKET)
    }

    pub fn listing(&self) -> Listing {
        let socket = self.socket();
        self.runtime.block_on(async move {
            mux_client(socket)
                .await
                .list(ListRequest {})
                .await
                .unwrap()
                .into_inner()
        })
    }

    pub fn dumped(&self, terminal: mux_api::TerminalKey) -> String {
        let socket = self.socket();
        let dumped = mux_api::Dumped::Text.into();
        let request = mux_api::DumpRequest {
            dumped,
            terminal: Some(terminal),
        };
        self.runtime.block_on(async move {
            mux_client(socket)
                .await
                .dump(request)
                .await
                .unwrap()
                .into_inner()
                .text
        })
    }

    pub fn new_terminal(&self, tag: &[u8]) -> mux_api::TerminalKey {
        self.new_terminal_running(tag, WAITS.map(str::to_owned).to_vec())
    }

    pub fn new_terminal_running(&self, tag: &[u8], argv: Vec<String>) -> mux_api::TerminalKey {
        let socket = self.socket();
        let program = Some(mux_api::Program { argv });
        let ends = mux_api::Ends::Holds.into();
        let opens = Some(mux_api::Opens {
            cwd: self.home(),
            program,
            ends,
            resume_in_shell: None,
        });
        let size = Some(mux_api::Size {
            columns: 80,
            rows: 24,
        });
        let request = mux_api::NewTerminalRequest {
            opens,
            tag: tag.to_vec(),
            size,
        };
        self.runtime.block_on(async move {
            let made = mux_client(socket)
                .await
                .new_terminal(request)
                .await
                .unwrap();
            made.into_inner().terminal.unwrap()
        })
    }

    pub fn close_terminal(&self, request: mux_api::CloseTerminalRequest) {
        let socket = self.socket();
        self.runtime.block_on(async move {
            mux_client(socket)
                .await
                .close_terminal(request)
                .await
                .unwrap();
        });
    }

    pub fn kill_mux(&self) {
        assert!(
            !self.mux_stalled.get(),
            "resume StalledMux before killing lune-mux"
        );
        let Some(pid) = self.mux_pid() else {
            return;
        };
        let Some(exit) = ProcessExit::watch(pid.as_pid())
            .unwrap_or_else(|error| panic!("lune-mux (pid {pid}) exit watch failed: {error}"))
        else {
            return;
        };
        match exit.signal(rustix::process::Signal::KILL) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => return,
            Err(error) => panic!("lune-mux (pid {pid}) did not take SIGKILL: {error}"),
        }
        self.runtime
            .block_on(lune_waits::awaited("lune-mux exits", exit.wait()))
            .unwrap_or_else(|error| panic!("lune-mux (pid {pid}) exit wait failed: {error}"));
    }

    pub fn stall_mux(&self) -> StalledMux<'_> {
        assert!(
            !self.mux_stalled.get(),
            "lune-mux is already stalled by an active StalledMux guard"
        );
        let pid = self.mux_pid().expect("lune-mux listens");
        let handle = MuxHandle::watch(pid)
            .unwrap_or_else(|| panic!("lune-mux (pid {pid}) exited before stopping"));
        let mut pending = PendingStall::begin(self, handle);
        pending.signal_stop();
        eventually_showing("lune-mux stops", || match mux_stopped(pending.handle()) {
            MuxStopCheck::Stopped => Ok(()),
            MuxStopCheck::Pending(reason) => Err(reason),
            MuxStopCheck::Ended(ended) => {
                pending.disarm();
                panic!("lune-mux (pid {pid}) ended before stopping: {ended}");
            }
        });
        pending.confirm()
    }

    fn mux_pid(&self) -> Option<MuxPid> {
        let socket = self.socket();
        let own_uid = rustix::process::getuid().as_raw();
        let listening = self.runtime.block_on(async move {
            let stream = tokio::net::UnixStream::connect(socket).await.ok()?;
            let cred = stream.peer_cred().ok()?;
            assert_eq!(
                cred.uid(),
                own_uid,
                "another user's process listens on mux.sock"
            );
            cred.pid()
        })?;
        Some(MuxPid::from_raw(listening).expect("lune-mux pid is > 1 and not the test process"))
    }

    pub fn stand_in_mux(&self, says: Option<&str>) -> Running {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["own::stand_in_mux", "--exact", "--ignored"]);
        command
            .env(STAND_IN_MUX, self.host.folder())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(says) = says {
            command.env(STAND_IN_SAYS, says);
        }
        let stand_in = Running::spawn(&mut command);
        let pid = MuxPid::from_u32(stand_in.pid())
            .expect("stand-in lune-mux pid is > 1 and not the test process");
        eventually("the stand-in lune-mux listens", || {
            (self.mux_pid() == Some(pid)).then_some(())
        });
        stand_in
    }
}

pub fn beside_a_stand_in_mux(says: &str) -> TempDir {
    let beside = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fs::hard_link(LUNED, beside.path().join("luned")).unwrap();
    let test = env::current_exe().unwrap();
    let test = test.display();
    let runs = format!(
        "{STAND_IN_MUX}=\"$1\" {STAND_IN_SAYS}='{says}' exec '{test}' own::stand_in_mux --exact \
         --ignored\n"
    );
    script(&beside.path().join("lune-mux"), &runs);
    beside
}

#[test]
#[ignore = "the stand-in for a lune-mux of another build, which a test runs as a process"]
fn stand_in_mux() {
    let Some(folder) = env::var_os(STAND_IN_MUX).map(PathBuf::from) else {
        return;
    };
    let says = env::var(STAND_IN_SAYS).ok().map(build_of);
    let says = says.map(|(version, built)| InfoResponse { version, built });
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(folder.join(LOCK));
    let lock = lock.unwrap();
    lock.lock().unwrap();
    let socket = folder.join(server::SOCKET);
    let _ = fs::remove_file(&socket);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        Server::builder()
            .add_routes(Routes::new(StandIn(says)))
            .serve_with_incoming(UnixListenerStream::new(listener))
            .await
            .unwrap();
    });
}

#[derive(Clone)]
struct StandIn(Option<InfoResponse>);

impl NamedService for StandIn {
    const NAME: &'static str = "lune.mux.v1.Mux";
}

impl<B> Service<http::Request<B>> for StandIn
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future = BoxFuture<Self::Response, Self::Error>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let info = request.uri().path() == "/lune.mux.v1.Mux/Info";
        let says = self.0.clone().filter(|_| info);
        Box::pin(async move {
            let Some(says) = says else {
                return Ok(Status::unimplemented("a stand-in lune-mux").into_http());
            };
            Ok(Grpc::new(ProstCodec::default())
                .unary(Says(says), request)
                .await)
        })
    }
}

struct Says(InfoResponse);

impl UnaryService<InfoRequest> for Says {
    type Response = InfoResponse;
    type Future = BoxFuture<tonic::Response<InfoResponse>, Status>;

    fn call(&mut self, _: tonic::Request<InfoRequest>) -> Self::Future {
        let says = self.0.clone();
        Box::pin(async move { Ok(tonic::Response::new(says)) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MuxPid(PeerPid);

impl MuxPid {
    fn from_raw(raw: i32) -> Option<Self> {
        let peer = PeerPid::from_raw(raw)?;
        (peer.as_pid() != rustix::process::getpid()).then_some(Self(peer))
    }

    fn from_u32(raw: u32) -> Option<Self> {
        let peer = PeerPid::new(raw)?;
        (peer.as_pid() != rustix::process::getpid()).then_some(Self(peer))
    }

    fn get(self) -> u32 {
        self.0.get()
    }

    fn as_pid(self) -> rustix::process::Pid {
        self.0.as_pid()
    }
}

impl fmt::Display for MuxPid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

struct MuxHandle {
    pid: MuxPid,
    exit: ProcessExit,
    #[cfg(target_os = "macos")]
    start_time: u64,
}

impl MuxHandle {
    fn watch(pid: MuxPid) -> Option<Self> {
        let exit = ProcessExit::watch(pid.as_pid())
            .unwrap_or_else(|error| panic!("lune-mux (pid {pid}) exit watch failed: {error}"))?;
        #[cfg(target_os = "macos")]
        let (start_time, _) = macos_process_info(pid)?;
        Some(Self {
            pid,
            exit,
            #[cfg(target_os = "macos")]
            start_time,
        })
    }

    fn resume(&self) {
        #[cfg(target_os = "macos")]
        if macos_process_info(self.pid).is_none_or(|(start_time, _)| start_time != self.start_time)
        {
            return;
        }
        match self.exit.signal(rustix::process::Signal::CONT) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(error) if std::thread::panicking() => {
                eprintln!(
                    "lune-mux (pid {}) did not take SIGCONT while unwinding: {error}",
                    self.pid
                );
            }
            Err(error) => panic!("lune-mux (pid {}) did not take SIGCONT: {error}", self.pid),
        }
    }
}

struct PendingStall<'a> {
    owned: &'a Owned,
    handle: Option<MuxHandle>,
}

impl<'a> PendingStall<'a> {
    fn begin(owned: &'a Owned, handle: MuxHandle) -> Self {
        owned.mux_stalled.set(true);
        Self {
            owned,
            handle: Some(handle),
        }
    }

    fn handle(&self) -> &MuxHandle {
        self.handle
            .as_ref()
            .expect("pending stall holds its handle")
    }

    fn signal_stop(&mut self) {
        let handle = self.handle();
        let pid = handle.pid;
        match handle.exit.signal(rustix::process::Signal::STOP) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => {
                self.disarm();
                panic!("lune-mux (pid {pid}) exited before SIGSTOP");
            }
            Err(error) => {
                self.disarm();
                panic!("lune-mux (pid {pid}) did not take SIGSTOP: {error}");
            }
        }
    }

    fn disarm(&mut self) {
        self.handle = None;
        self.owned.mux_stalled.set(false);
    }

    fn confirm(mut self) -> StalledMux<'a> {
        let handle = self.handle.take().expect("pending stall holds its handle");
        StalledMux {
            owned: self.owned,
            handle,
        }
    }
}

impl Drop for PendingStall<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.resume();
            self.owned.mux_stalled.set(false);
        }
    }
}

#[must_use]
pub struct StalledMux<'a> {
    owned: &'a Owned,
    handle: MuxHandle,
}

impl Drop for StalledMux<'_> {
    fn drop(&mut self) {
        self.handle.resume();
        self.owned.mux_stalled.set(false);
    }
}

enum MuxStopCheck {
    Stopped,
    Pending(StopPending),
    Ended(StopEnded),
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Stopped,
    TracingStopped,
    Zombie,
    Dead,
    Running,
    Sleeping,
    DiskSleep,
    Idle,
    Waking,
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
enum TaskStatError {
    MissingCommOpen,
    MissingCommClose,
    InvertedComm,
    InvalidTid,
    MismatchedTid {
        expected: std::num::NonZeroU32,
        actual: u32,
    },
    InvalidAfterCommUtf8,
    MissingState,
    UnknownState(String),
}

#[cfg(target_os = "linux")]
enum StopPending {
    ThreadVanished(std::io::ErrorKind),
    ThreadExited(std::num::NonZeroU32),
    ThreadEnding(std::num::NonZeroU32, TaskState),
    ThreadActive(std::num::NonZeroU32, TaskState),
    NoThreads,
}

#[cfg(target_os = "linux")]
impl fmt::Debug for StopPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopPending::ThreadVanished(kind) => {
                write!(f, "lune-mux thread vanished during scan ({kind:?})")
            }
            StopPending::ThreadExited(tid) => write!(f, "lune-mux thread {tid} exited mid-scan"),
            StopPending::ThreadEnding(tid, state) => {
                write!(f, "lune-mux thread {tid} is ending in state {state:?}")
            }
            StopPending::ThreadActive(tid, state) => {
                write!(f, "lune-mux thread {tid} is in state {state:?}")
            }
            StopPending::NoThreads => f.write_str("/proc task directory held no threads"),
        }
    }
}

#[cfg(target_os = "linux")]
enum StopEnded {
    Exited,
    MainThread(TaskState),
}

#[cfg(target_os = "linux")]
impl fmt::Display for StopEnded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopEnded::Exited => f.write_str("exited"),
            StopEnded::MainThread(state) => write!(f, "main thread in state {state:?}"),
        }
    }
}

#[cfg(target_os = "linux")]
fn mux_stopped(handle: &MuxHandle) -> MuxStopCheck {
    use std::io::ErrorKind;
    use std::num::NonZeroU32;

    const TASKS_MAX: usize = 256;
    const _: () = assert!(TASKS_MAX > 0);

    let gone = |error: &std::io::Error| {
        error.kind() == ErrorKind::NotFound
            || rustix::io::Errno::from_io_error(error) == Some(rustix::io::Errno::SRCH)
    };
    let raw = handle.pid.get();
    let tasks = match fs::read_dir(format!("/proc/{raw}/task")) {
        Ok(tasks) => tasks,
        Err(error) if gone(&error) => return MuxStopCheck::Ended(StopEnded::Exited),
        Err(error) => panic!("/proc/{raw}/task did not read: {error}"),
    };
    let mut tasks_count = 0;
    for entry in tasks {
        assert!(
            tasks_count < TASKS_MAX,
            "lune-mux runs at most {TASKS_MAX} threads"
        );
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if gone(&error) => {
                return MuxStopCheck::Pending(StopPending::ThreadVanished(error.kind()));
            }
            Err(error) => panic!("/proc/{raw}/task entry did not read: {error}"),
        };
        let tid_name = entry.file_name();
        let tid: NonZeroU32 = tid_name
            .to_str()
            .and_then(|name| name.parse().ok())
            .unwrap_or_else(|| {
                panic!("/proc/{raw}/task entry {tid_name:?} is not a positive numeric tid")
            });
        let stat = match fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if gone(&error) => {
                return MuxStopCheck::Pending(StopPending::ThreadExited(tid));
            }
            Err(error) => panic!("/proc/{raw}/task/{tid}/stat did not read: {error}"),
        };
        let state = parse_task_stat(&stat, tid).unwrap_or_else(|error| {
            panic!("/proc/{raw}/task/{tid}/stat failed to parse: {error:?}")
        });
        match state {
            TaskState::Stopped | TaskState::TracingStopped => {}
            TaskState::Zombie | TaskState::Dead => {
                if tid.get() == raw {
                    return MuxStopCheck::Ended(StopEnded::MainThread(state));
                }
                return MuxStopCheck::Pending(StopPending::ThreadEnding(tid, state));
            }
            TaskState::Running
            | TaskState::Sleeping
            | TaskState::DiskSleep
            | TaskState::Idle
            | TaskState::Waking => {
                return MuxStopCheck::Pending(StopPending::ThreadActive(tid, state));
            }
        }
        tasks_count += 1;
    }
    if tasks_count == 0 {
        return MuxStopCheck::Pending(StopPending::NoThreads);
    }
    MuxStopCheck::Stopped
}

#[cfg(target_os = "linux")]
fn parse_task_stat(
    stat: &[u8],
    expected_tid: std::num::NonZeroU32,
) -> Result<TaskState, TaskStatError> {
    let comm_start_index = stat
        .iter()
        .position(|byte| *byte == b'(')
        .ok_or(TaskStatError::MissingCommOpen)?;
    let comm_end_index = stat
        .iter()
        .rposition(|byte| *byte == b')')
        .ok_or(TaskStatError::MissingCommClose)?;
    if comm_start_index >= comm_end_index {
        return Err(TaskStatError::InvertedComm);
    }
    let parsed_tid: u32 = std::str::from_utf8(&stat[..comm_start_index])
        .map_err(|_| TaskStatError::InvalidTid)?
        .trim()
        .parse()
        .map_err(|_| TaskStatError::InvalidTid)?;
    if parsed_tid != expected_tid.get() {
        return Err(TaskStatError::MismatchedTid {
            expected: expected_tid,
            actual: parsed_tid,
        });
    }
    let after_comm = stat
        .get(comm_end_index + 1..)
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .ok_or(TaskStatError::InvalidAfterCommUtf8)?;
    let state = after_comm
        .split_ascii_whitespace()
        .next()
        .ok_or(TaskStatError::MissingState)?;
    match state {
        "T" => Ok(TaskState::Stopped),
        "t" => Ok(TaskState::TracingStopped),
        "Z" => Ok(TaskState::Zombie),
        "X" | "x" => Ok(TaskState::Dead),
        "R" => Ok(TaskState::Running),
        "S" => Ok(TaskState::Sleeping),
        "D" => Ok(TaskState::DiskSleep),
        "I" => Ok(TaskState::Idle),
        "W" | "K" | "P" => Ok(TaskState::Waking),
        other => Err(TaskStatError::UnknownState(other.to_owned())),
    }
}

#[cfg(target_os = "macos")]
enum StopPending {
    State(sysinfo::ProcessStatus),
}

#[cfg(target_os = "macos")]
impl fmt::Debug for StopPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let StopPending::State(status) = self;
        write!(f, "lune-mux is in state {status:?}")
    }
}

#[cfg(target_os = "macos")]
enum StopEnded {
    Exited,
    PidReused {
        expected_start: u64,
        actual_start: u64,
    },
    State(sysinfo::ProcessStatus),
}

#[cfg(target_os = "macos")]
impl fmt::Display for StopEnded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopEnded::Exited => f.write_str("exited"),
            StopEnded::PidReused {
                expected_start,
                actual_start,
            } => write!(
                f,
                "pid reused (expected start {expected_start}, saw {actual_start})"
            ),
            StopEnded::State(status) => write!(f, "state {status:?}"),
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_process_info(pid: MuxPid) -> Option<(u64, sysinfo::ProcessStatus)> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let mut system = System::new();
    let target_pid = sysinfo::Pid::from_u32(pid.get());
    let refresh = ProcessRefreshKind::nothing();
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[target_pid]), true, refresh);
    let process = system.process(target_pid)?;
    assert_eq!(
        process.pid(),
        target_pid,
        "sysinfo returned another process"
    );
    Some((process.start_time(), process.status()))
}

#[cfg(target_os = "macos")]
fn mux_stopped(handle: &MuxHandle) -> MuxStopCheck {
    use sysinfo::ProcessStatus;

    let Some((start_time, status)) = macos_process_info(handle.pid) else {
        return MuxStopCheck::Ended(StopEnded::Exited);
    };
    if start_time != handle.start_time {
        return MuxStopCheck::Ended(StopEnded::PidReused {
            expected_start: handle.start_time,
            actual_start: start_time,
        });
    }
    match status {
        ProcessStatus::Stop => MuxStopCheck::Stopped,
        ProcessStatus::Zombie | ProcessStatus::Dead => {
            MuxStopCheck::Ended(StopEnded::State(status))
        }
        ProcessStatus::Idle | ProcessStatus::Run | ProcessStatus::Sleep => {
            MuxStopCheck::Pending(StopPending::State(status))
        }
        other => panic!(
            "lune-mux (pid {}) reported unexpected macOS process status {other:?}",
            handle.pid
        ),
    }
}

async fn mux_client(socket: PathBuf) -> MuxClient<Channel> {
    let channel = Endpoint::from_static("http://lune-mux")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket).await?;
                std::io::Result::Ok(TokioIo::new(stream))
            }
        }))
        .await
        .unwrap();
    MuxClient::new(channel).max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX)
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.stop_daemon();
        self.kill_mux();
    }
}

#[test]
fn osc_7501_reports_flow_from_lune_mux_to_luned_and_outrank_session_files() {
    use lune_protocol::agent_status::{AgentStatus, BlockedReason};
    use lune_protocol::program_status::Message;
    use luned::api::{self, Harness, StatusRung};

    let owned = Owned::new();
    let program = built_server();
    let mut mux = owned.host.vars().command(program.to_str().unwrap());
    mux.arg(owned.host.folder()).env("TERM_PROGRAM", "ghostty");
    let _mux = Running::spawn(mux.stdin(Stdio::null()));
    let socket = owned.socket();
    eventually("the server listens", || socket.exists().then_some(()));

    let dir = tempfile::tempdir().unwrap();
    let step1 = dir.path().join("step1");
    let step2 = dir.path().join("step2");
    let step3 = dir.path().join("step3");
    let step4 = dir.path().join("step4");
    let term_program = dir.path().join("term_program");
    let script_path = dir.path().join("claude.sh");
    let body = format!(
        "printf '%s' \"${{TERM_PROGRAM-unset}}\" > '{}'\n\
         printf '\\033]7501;state=working:app=claude-code\\007'\n\
         while [ ! -f '{}' ]; do :; done\n\
         printf '\\033]7501;state=blocked:kind=permission:app=claude-code:msg=YXBwcm92ZSB0b3VjaA==\\007'\n\
         printf '\\033]7501;state=working:id=sub-1:title=RXhwbG9yZQ==:progress=50\\007'\n\
         while [ ! -f '{}' ]; do :; done\n\
         printf '\\033]7501;state=done:app=claude-code:msg=QWxsIGRvbmU=\\007'\n\
         while [ ! -f '{}' ]; do :; done\n\
         printf '\\033]7501;state=clear\\007'\n\
         while [ ! -f '{}' ]; do :; done\n",
        term_program.display(),
        step1.display(),
        step2.display(),
        step3.display(),
        step4.display(),
    );
    script(&script_path, &body);
    let _key = owned.new_terminal_running(b"osc", vec![script_path.to_str().unwrap().to_owned()]);

    eventually("TERM_PROGRAM is unset inside lune-mux", || {
        (fs::read_to_string(&term_program).ok().as_deref() == Some("unset")).then_some(())
    });

    let working = eventually("luned sees the OSC 7501 working agent", || {
        owned.host.host().agents.into_iter().find(|agent| {
            agent.runs_in() == Harness::ClaudeCode
                && AgentStatus::of_api(agent) == AgentStatus::Working
                && agent.status_rung == Some(StatusRung::Osc7501.into())
        })
    });
    let pid = working.pid;

    let sessions = owned.host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = crate::agents::file_of_pid("osc-session", pid, "busy");
    fs::write(sessions.join(format!("{pid}.json")), file.to_string()).unwrap();
    fs::write(&step1, "").unwrap();

    let blocked = eventually(
        "rung 1 blocked overrides rung 2 busy and carries subagents",
        || {
            owned.host.host().agents.into_iter().find(|agent| {
                agent.pid == pid
                    && agent.session_id.as_deref() == Some("osc-session")
                    && matches!(AgentStatus::of_api(agent), AgentStatus::Blocked { .. })
                    && agent.status_rung == Some(StatusRung::Osc7501.into())
            })
        },
    );
    assert_eq!(
        AgentStatus::of_api(&blocked),
        AgentStatus::Blocked {
            reason: BlockedReason::Permission,
            message: Message::parse("message", "approve touch").ok(),
        }
    );
    assert_eq!(blocked.subagents.len(), 1);
    assert_eq!(blocked.subagents[0].id, "sub-1");
    assert_eq!(blocked.subagents[0].title.as_deref(), Some("Explore"));
    assert_eq!(
        blocked.subagents[0].phase,
        Some(api::subagent::Phase::Working(api::WorkingStatus {
            progress: Some(50)
        }))
    );

    fs::write(&step2, "").unwrap();
    let done = eventually("rung 1 done with summary arrives", || {
        owned.host.host().agents.into_iter().find(|agent| {
            agent.pid == pid
                && matches!(AgentStatus::of_api(agent), AgentStatus::Done { .. })
                && agent.status_rung == Some(StatusRung::Osc7501.into())
        })
    });
    assert_eq!(
        AgentStatus::of_api(&done),
        AgentStatus::Done {
            message: Message::parse("message", "All done").ok()
        }
    );

    fs::write(&step3, "").unwrap();
    let fallback = eventually("clearing rung 1 falls back to rung 2 busy", || {
        owned.host.host().agents.into_iter().find(|agent| {
            agent.pid == pid
                && AgentStatus::of_api(agent) == AgentStatus::Working
                && agent.status_rung == Some(StatusRung::Feed.into())
        })
    });
    assert_eq!(fallback.subagents, []);
    fs::write(&step4, "").unwrap();
}

#[test]
fn the_server_listens_and_gives_way_as_the_daemon_has_it() {
    let host = Host::new();
    let program = built_server();
    let program = program.to_str().unwrap();
    let mut first = host.vars().command(program);
    first.arg(host.folder());
    let _first = Running::spawn(first.stdin(Stdio::null()));
    let socket = host.folder().join(server::SOCKET);
    eventually("the first server listens", || socket.exists().then_some(()));
    let mut second = host.vars().command(program);
    second.arg(host.folder());
    let second = second
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(second.code(), Some(i32::from(server::ANOTHER_RUNS)));
}

pub fn the_daemon_runs_on_while_a_terminal_does(_: Slot) {
    let mut owned = Owned::new();
    let program = built_server();
    let mut mux = owned.host.vars().command(program.to_str().unwrap());
    mux.arg(owned.host.folder());
    let _mux = Running::spawn(mux.stdin(Stdio::null()));
    let socket = owned.socket();
    eventually("the server listens", || socket.exists().then_some(()));
    owned.new_terminal(b"kept");
    let daemon = owned.daemon.as_mut().unwrap();
    let kept = const { Span::of(ALONE.saturating_add(SWEEP.saturating_mul(3))) };
    daemon.runs_through(kept, "the daemon ends while a terminal runs");
}

#[test]
fn mux_pid_rejects_init_non_positive_and_self() {
    let self_raw = rustix::process::getpid().as_raw_pid();
    let self_u32 = std::process::id();
    assert_eq!(MuxPid::from_raw(-1), None);
    assert_eq!(MuxPid::from_raw(0), None);
    assert_eq!(MuxPid::from_raw(1), None);
    assert_eq!(MuxPid::from_raw(self_raw), None);
    assert_eq!(MuxPid::from_u32(0), None);
    assert_eq!(MuxPid::from_u32(1), None);
    assert_eq!(MuxPid::from_u32(self_u32), None);

    let other_u32 = if self_u32 == 2 { 3 } else { 2 };
    let other_raw = i32::try_from(other_u32).expect("small test pid fits i32");
    let pid = MuxPid::from_raw(other_raw).expect("pid > 1 and != self parses");
    assert_eq!(pid.get(), other_u32);
    assert_eq!(pid.as_pid().as_raw_pid(), other_raw);
    assert_eq!(MuxPid::from_u32(other_u32), Some(pid));
}

#[cfg(target_os = "linux")]
#[test]
fn parse_task_stat_reads_closed_states_and_rejects_mismatched_or_malformed_stat() {
    use std::num::NonZeroU32;

    let tid = |raw| NonZeroU32::new(raw).expect("test tid is positive");

    assert_eq!(
        parse_task_stat(b"42 (lune (mux) 1) T 1 42 42", tid(42)),
        Ok(TaskState::Stopped)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) t 1 42 42", tid(43)),
        Ok(TaskState::TracingStopped)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) S 1 42 42", tid(43)),
        Ok(TaskState::Sleeping)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) R 1 42 42", tid(43)),
        Ok(TaskState::Running)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) D 1 42 42", tid(43)),
        Ok(TaskState::DiskSleep)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) I 1 42 42", tid(43)),
        Ok(TaskState::Idle)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) Z 1 42 42", tid(43)),
        Ok(TaskState::Zombie)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) X 1 42 42", tid(43)),
        Ok(TaskState::Dead)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) x 1 42 42", tid(43)),
        Ok(TaskState::Dead)
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) W 1 42 42", tid(43)),
        Ok(TaskState::Waking)
    );
    assert_eq!(
        parse_task_stat(b"99 (lune-mux) T 1", tid(43)),
        Err(TaskStatError::MismatchedTid {
            expected: tid(43),
            actual: 99,
        })
    );
    assert_eq!(
        parse_task_stat(b"43 lune-mux T 1", tid(43)),
        Err(TaskStatError::MissingCommOpen)
    );
    assert_eq!(
        parse_task_stat(b"43 (lune-mux T 1", tid(43)),
        Err(TaskStatError::MissingCommClose)
    );
    assert_eq!(
        parse_task_stat(b"43 )lune-mux( T 1", tid(43)),
        Err(TaskStatError::InvertedComm)
    );
    assert_eq!(
        parse_task_stat(b"abc (lune-mux) T 1", tid(43)),
        Err(TaskStatError::InvalidTid)
    );
    assert_eq!(
        parse_task_stat(b"43 (lune-mux) \xff", tid(43)),
        Err(TaskStatError::InvalidAfterCommUtf8)
    );
    assert_eq!(
        parse_task_stat(b"43 (lune-mux) ", tid(43)),
        Err(TaskStatError::MissingState)
    );
    assert_eq!(
        parse_task_stat(b"43 (lune-mux) 1 42 42", tid(43)),
        Err(TaskStatError::UnknownState("1".to_owned()))
    );
}
