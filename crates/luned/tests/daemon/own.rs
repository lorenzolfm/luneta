use std::cell::Cell;
use std::convert::Infallible;
use std::env;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use hyper_util::rt::TokioIo;
use lune_waits::{Span, awaited, eventually, eventually_showing};
use luned::mux::own::api::mux_client::MuxClient;
use luned::mux::own::api::{self as mux_api, InfoRequest, InfoResponse, ListRequest, Listing};
use luned::mux::own::server;
use luned::{ALONE, PeerPid, ProcessExit, SWEEP};
#[cfg(target_os = "macos")]
use luned::{MacosActiveState, MacosProcessState, MacosStartTime, macos_process_state};
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

fn built_server() -> PathBuf {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MuxState {
    Active,
    Stalled,
}

pub struct Owned {
    daemon: Option<Daemon>,
    mux_child: Option<Running>,
    pub host: Host,
    runtime: tokio::runtime::Runtime,
    mux_state: Cell<MuxState>,
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
            mux_child: None,
            host,
            runtime,
            mux_state: Cell::new(MuxState::Active),
        }
    }

    pub fn stop_daemon(&mut self) {
        self.daemon = None;
    }

    pub fn daemon_ended(&mut self, what: &str) -> ExitStatus {
        let mut daemon = self.daemon.take().expect("a daemon was started");
        daemon.ended(what)
    }

    pub fn daemon_runs_through(&mut self, span: Span, what: &str) {
        self.daemon
            .as_mut()
            .expect("a daemon was started")
            .runs_through(span, what);
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

    pub fn spawn_mux(&mut self, configure: impl FnOnce(&mut Command)) {
        self.assert_mux_active("spawning lune-mux");
        assert!(
            self.mux_child.is_none(),
            "one spawned lune-mux runs at a time"
        );
        let program = built_server();
        let mut mux = self.host.vars().command(program.to_str().unwrap());
        mux.arg(self.host.folder()).stdin(Stdio::null());
        configure(&mut mux);
        let running = Running::spawn(&mut mux);
        let pid = MuxPid::from_u32(running.pid())
            .expect("spawned lune-mux pid is > 1 and not the test process");
        self.mux_child = Some(running);
        eventually("the server listens", || match self.mux_socket() {
            Ok(MuxSocket::Listening(listening)) if listening == pid => Some(()),
            Ok(MuxSocket::Absent | MuxSocket::Listening(_)) => None,
            Err(error) => panic!("lune-mux socket failed while waiting to listen: {error}"),
        });
    }

    pub fn mux_exit_status(&mut self) -> Option<ExitStatus> {
        self.mux_child
            .as_mut()
            .expect("a lune-mux was spawned")
            .exit_status()
    }

    pub fn mux_ended(&mut self, what: &str) -> ExitStatus {
        let mut mux = self.mux_child.take().expect("a lune-mux was spawned");
        mux.ended(what)
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

    fn assert_mux_active(&self, action: &str) {
        assert_eq!(
            self.mux_state.get(),
            MuxState::Active,
            "resume StalledMux before {action}"
        );
    }

    pub fn listing(&self) -> Listing {
        self.assert_mux_active("listing lune-mux terminals");
        let socket = self.socket();
        self.runtime.block_on(awaited("lune-mux lists", async move {
            mux_client(socket)
                .await
                .list(ListRequest {})
                .await
                .unwrap()
                .into_inner()
        }))
    }

    pub fn dumped(&self, terminal: mux_api::TerminalKey) -> String {
        self.assert_mux_active("dumping a lune-mux terminal");
        let socket = self.socket();
        let dumped = mux_api::Dumped::Text.into();
        let request = mux_api::DumpRequest {
            dumped,
            terminal: Some(terminal),
        };
        self.runtime.block_on(awaited("lune-mux dumps", async move {
            mux_client(socket)
                .await
                .dump(request)
                .await
                .unwrap()
                .into_inner()
                .text
        }))
    }

    pub fn new_terminal(&self, tag: &[u8]) -> mux_api::TerminalKey {
        self.new_terminal_running(tag, WAITS.map(str::to_owned).to_vec())
    }

    pub fn new_terminal_running(&self, tag: &[u8], argv: Vec<String>) -> mux_api::TerminalKey {
        self.assert_mux_active("opening a lune-mux terminal");
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
        self.runtime
            .block_on(awaited("lune-mux opens a terminal", async move {
                let made = mux_client(socket)
                    .await
                    .new_terminal(request)
                    .await
                    .unwrap();
                made.into_inner().terminal.unwrap()
            }))
    }

    pub fn close_terminal(&self, request: mux_api::CloseTerminalRequest) {
        self.assert_mux_active("closing a lune-mux terminal");
        let socket = self.socket();
        self.runtime
            .block_on(awaited("lune-mux closes a terminal", async move {
                mux_client(socket)
                    .await
                    .close_terminal(request)
                    .await
                    .unwrap();
            }));
    }

    pub fn kill_mux(&self) {
        self.assert_mux_active("killing lune-mux");
        let handle = MuxHandle::attach(self)
            .unwrap_or_else(|error| panic!("lune-mux attach failed before killing: {error}"));
        if let Some(handle) = handle {
            handle.kill_and_wait(&self.runtime);
        }
    }

    fn kill_mux_on_drop(&mut self) {
        self.mux_state.set(MuxState::Active);
        let handle = match MuxHandle::attach(self) {
            Ok(handle) => handle,
            Err(error) if std::thread::panicking() => {
                eprintln!("lune-mux attach failed during Owned::drop unwind: {error}");
                return;
            }
            Err(error) => panic!("lune-mux attach failed during Owned::drop: {error}"),
        };
        if let Some(handle) = handle {
            handle.kill_and_wait_for_drop(&self.runtime);
        }
    }

    pub fn stall_mux(&self) -> StalledMux<'_> {
        self.assert_mux_active("stalling lune-mux again");
        let handle = MuxHandle::attach(self)
            .unwrap_or_else(|error| panic!("lune-mux attach failed before SIGSTOP: {error}"))
            .expect("lune-mux listens");
        handle.signal_stop();
        let stalled = StalledMux::begin(self, handle);
        eventually_showing("lune-mux stops", || match mux_stopped(&stalled.handle) {
            MuxStopCheck::Stopped => Ok(()),
            MuxStopCheck::Pending(reason) => Err(reason),
            MuxStopCheck::Ended(ended) => {
                panic!(
                    "lune-mux (pid {}) ended before stopping: {ended}",
                    stalled.handle.pid
                );
            }
        });
        stalled
    }

    fn mux_socket(&self) -> Result<MuxSocket, MuxPeerError> {
        mux_socket_at(&self.runtime, self.socket())
    }

    pub fn stand_in_mux(&self, says: Option<&str>) -> Running {
        self.assert_mux_active("starting a stand-in lune-mux");
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
            match self.mux_socket() {
                Ok(MuxSocket::Listening(listening)) if listening == pid => Some(()),
                Ok(MuxSocket::Absent | MuxSocket::Listening(_)) => None,
                Err(error) => panic!("stand-in lune-mux socket failed: {error}"),
            }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MuxSocket {
    Absent,
    Listening(MuxPid),
}

#[derive(Debug)]
enum MuxPeerError {
    Connect(std::io::Error),
    PeerCred(std::io::Error),
    ForeignUid { expected: u32, actual: u32 },
    MissingPid,
    InvalidPid(i32),
}

impl fmt::Display for MuxPeerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MuxPeerError::Connect(error) => write!(f, "connect to mux.sock failed: {error}"),
            MuxPeerError::PeerCred(error) => write!(f, "peer_cred on mux.sock failed: {error}"),
            MuxPeerError::ForeignUid { expected, actual } => write!(
                f,
                "another user's process listens on mux.sock (expected uid {expected}, saw {actual})"
            ),
            MuxPeerError::MissingPid => f.write_str("kernel reported no peer pid on mux.sock"),
            MuxPeerError::InvalidPid(raw) => {
                write!(f, "mux.sock peer pid {raw} is <= 1 or the test process")
            }
        }
    }
}

fn mux_socket_at(
    runtime: &tokio::runtime::Runtime,
    socket: PathBuf,
) -> Result<MuxSocket, MuxPeerError> {
    let own_uid = rustix::process::getuid().as_raw();
    let raw_pid = runtime.block_on(async move {
        let stream = match tokio::net::UnixStream::connect(socket).await {
            Ok(stream) => stream,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotFound | ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(MuxPeerError::Connect(error)),
        };
        let cred = match stream.peer_cred() {
            Ok(cred) => cred,
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::NotConnected | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(MuxPeerError::PeerCred(error)),
        };
        if cred.uid() != own_uid {
            return Err(MuxPeerError::ForeignUid {
                expected: own_uid,
                actual: cred.uid(),
            });
        }
        cred.pid().ok_or(MuxPeerError::MissingPid).map(Some)
    })?;
    let Some(raw_pid) = raw_pid else {
        return Ok(MuxSocket::Absent);
    };
    let pid = MuxPid::from_raw(raw_pid).ok_or(MuxPeerError::InvalidPid(raw_pid))?;
    Ok(MuxSocket::Listening(pid))
}

#[derive(Debug)]
enum MuxAttachError {
    Socket(MuxPeerError),
    Watch(MuxPid, std::io::Error),
    ExitCheck(MuxPid, std::io::Error),
    UnstablePid(usize),
}

impl fmt::Display for MuxAttachError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MuxAttachError::Socket(error) => write!(f, "{error}"),
            MuxAttachError::Watch(pid, error) => {
                write!(f, "lune-mux (pid {pid}) exit watch failed: {error}")
            }
            MuxAttachError::ExitCheck(pid, error) => {
                write!(f, "lune-mux (pid {pid}) exit check failed: {error}")
            }
            MuxAttachError::UnstablePid(attempts) => {
                write!(f, "lune-mux pid changed across {attempts} attach attempts")
            }
        }
    }
}

struct MuxHandle {
    pid: MuxPid,
    exit: ProcessExit,
    #[cfg(target_os = "macos")]
    start_time: MacosStartTime,
}

impl MuxHandle {
    fn attach(owned: &Owned) -> Result<Option<Self>, MuxAttachError> {
        const ATTACH_RETRIES_MAX: usize = 8;
        const _: () = assert!(ATTACH_RETRIES_MAX > 0);

        for _ in 0..ATTACH_RETRIES_MAX {
            let MuxSocket::Listening(pid) = owned.mux_socket().map_err(MuxAttachError::Socket)?
            else {
                return Ok(None);
            };
            let Some(exit) = ProcessExit::watch(pid.as_pid())
                .map_err(|error| MuxAttachError::Watch(pid, error))?
            else {
                continue;
            };
            #[cfg(target_os = "macos")]
            let Some((start_time, _)) = macos_process_state(pid.get()) else {
                continue;
            };
            match owned.mux_socket().map_err(MuxAttachError::Socket)? {
                MuxSocket::Listening(confirmed) if confirmed == pid => {}
                MuxSocket::Absent => return Ok(None),
                MuxSocket::Listening(_) => continue,
            }
            if exit
                .has_exited()
                .map_err(|error| MuxAttachError::ExitCheck(pid, error))?
            {
                continue;
            }
            return Ok(Some(Self {
                pid,
                exit,
                #[cfg(target_os = "macos")]
                start_time,
            }));
        }
        Err(MuxAttachError::UnstablePid(ATTACH_RETRIES_MAX))
    }

    fn has_exited(&self) -> Result<bool, std::io::Error> {
        self.exit.has_exited()
    }

    #[cfg(target_os = "macos")]
    fn same_instance(&self) -> bool {
        !self.exit.has_exited().unwrap_or(true)
            && macos_process_state(self.pid.get())
                .is_some_and(|(start_time, _)| start_time == self.start_time)
    }

    fn signal_stop(&self) {
        #[cfg(target_os = "macos")]
        assert!(
            self.same_instance(),
            "lune-mux (pid {}) exited before SIGSTOP",
            self.pid
        );
        match self.exit.signal(rustix::process::Signal::STOP) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => {
                panic!("lune-mux (pid {}) exited before SIGSTOP", self.pid);
            }
            Err(error) => {
                panic!("lune-mux (pid {}) did not take SIGSTOP: {error}", self.pid);
            }
        }
    }

    fn resume(&self) {
        match self.exit.has_exited() {
            Ok(true) => return,
            Ok(false) => {}
            Err(error) if std::thread::panicking() => {
                eprintln!(
                    "lune-mux (pid {}) exit check failed before SIGCONT while unwinding: {error}",
                    self.pid
                );
                return;
            }
            Err(error) => panic!(
                "lune-mux (pid {}) exit check failed before SIGCONT: {error}",
                self.pid
            ),
        }
        #[cfg(target_os = "macos")]
        if !self.same_instance() {
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

    fn kill_and_wait(self, runtime: &tokio::runtime::Runtime) {
        let pid = self.pid;
        #[cfg(target_os = "macos")]
        if !self.same_instance() {
            return;
        }
        match self.exit.signal(rustix::process::Signal::KILL) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => return,
            Err(error) => panic!("lune-mux (pid {pid}) did not take SIGKILL: {error}"),
        }
        runtime
            .block_on(awaited("lune-mux exits", self.exit.wait()))
            .unwrap_or_else(|error| panic!("lune-mux (pid {pid}) exit wait failed: {error}"));
    }

    fn kill_and_wait_for_drop(self, runtime: &tokio::runtime::Runtime) {
        if !std::thread::panicking() {
            self.kill_and_wait(runtime);
            return;
        }
        let pid = self.pid;
        #[cfg(target_os = "macos")]
        if !self.same_instance() {
            return;
        }
        match self.exit.signal(rustix::process::Signal::KILL) {
            Ok(()) => {}
            Err(rustix::io::Errno::SRCH) => return,
            Err(error) => {
                eprintln!("lune-mux (pid {pid}) did not take SIGKILL while unwinding: {error}");
                return;
            }
        }
        match runtime.block_on(lune_waits::try_awaited(self.exit.wait())) {
            Some(Ok(())) => {}
            Some(Err(error)) => {
                eprintln!("lune-mux (pid {pid}) exit wait failed while unwinding: {error}");
            }
            None => {
                eprintln!("lune-mux (pid {pid}) exit wait timed out while unwinding");
            }
        }
    }
}

#[must_use]
pub struct StalledMux<'a> {
    owned: &'a Owned,
    handle: MuxHandle,
}

impl<'a> StalledMux<'a> {
    fn begin(owned: &'a Owned, handle: MuxHandle) -> Self {
        owned.mux_state.set(MuxState::Stalled);
        Self { owned, handle }
    }
}

impl Drop for StalledMux<'_> {
    fn drop(&mut self) {
        self.handle.resume();
        self.owned.mux_state.set(MuxState::Active);
    }
}

enum MuxStopCheck {
    Stopped,
    Pending(StopPending),
    Ended(StopEnded),
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActiveTaskState {
    Running,
    Sleeping,
    DiskSleep,
    Idle,
    Waking,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EndingTaskState {
    Zombie,
    Dead,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskState {
    Stopped,
    TracingStopped,
    Ending(EndingTaskState),
    Active(ActiveTaskState),
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GoneReason {
    NotFound,
    NoSuchProcess,
}

#[cfg(target_os = "linux")]
fn gone_reason(error: &std::io::Error) -> Option<GoneReason> {
    if error.kind() == ErrorKind::NotFound {
        Some(GoneReason::NotFound)
    } else if rustix::io::Errno::from_io_error(error) == Some(rustix::io::Errno::SRCH) {
        Some(GoneReason::NoSuchProcess)
    } else {
        None
    }
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
        actual: std::num::NonZeroU32,
    },
    InvalidAfterCommUtf8,
    MissingState,
    UnknownState(String),
}

#[cfg(target_os = "linux")]
enum StopPending {
    ThreadVanished(GoneReason),
    ThreadExited(std::num::NonZeroU32),
    ThreadEnding(std::num::NonZeroU32, EndingTaskState),
    ThreadActive(std::num::NonZeroU32, ActiveTaskState),
    NoThreads,
}

#[cfg(target_os = "linux")]
impl fmt::Debug for StopPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopPending::ThreadVanished(reason) => {
                write!(f, "lune-mux thread vanished during scan ({reason:?})")
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
    MainThread(EndingTaskState),
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
    use std::num::NonZeroU32;

    const TASKS_MAX: usize = 256;
    const _: () = assert!(TASKS_MAX > 0);

    if handle
        .has_exited()
        .unwrap_or_else(|error| panic!("lune-mux (pid {}) exit check failed: {error}", handle.pid))
    {
        return MuxStopCheck::Ended(StopEnded::Exited);
    }
    let raw = handle.pid.get();
    let tasks = match fs::read_dir(format!("/proc/{raw}/task")) {
        Ok(tasks) => tasks,
        Err(error) if gone_reason(&error).is_some() => {
            return MuxStopCheck::Ended(StopEnded::Exited);
        }
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
            Err(error) => match gone_reason(&error) {
                Some(reason) => return MuxStopCheck::Pending(StopPending::ThreadVanished(reason)),
                None => panic!("/proc/{raw}/task entry did not read: {error}"),
            },
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
            Err(error) if gone_reason(&error).is_some() => {
                return MuxStopCheck::Pending(StopPending::ThreadExited(tid));
            }
            Err(error) => panic!("/proc/{raw}/task/{tid}/stat did not read: {error}"),
        };
        let state = parse_task_stat(&stat, tid).unwrap_or_else(|error| {
            panic!("/proc/{raw}/task/{tid}/stat failed to parse: {error:?}")
        });
        match state {
            TaskState::Stopped | TaskState::TracingStopped => {}
            TaskState::Ending(ending) => {
                if tid.get() == raw {
                    return MuxStopCheck::Ended(StopEnded::MainThread(ending));
                }
                return MuxStopCheck::Pending(StopPending::ThreadEnding(tid, ending));
            }
            TaskState::Active(active) => {
                return MuxStopCheck::Pending(StopPending::ThreadActive(tid, active));
            }
        }
        tasks_count += 1;
    }
    if tasks_count == 0 {
        return MuxStopCheck::Pending(StopPending::NoThreads);
    }
    if handle
        .has_exited()
        .unwrap_or_else(|error| panic!("lune-mux (pid {}) exit check failed: {error}", handle.pid))
    {
        return MuxStopCheck::Ended(StopEnded::Exited);
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
    let parsed_tid: std::num::NonZeroU32 = std::str::from_utf8(&stat[..comm_start_index])
        .map_err(|_| TaskStatError::InvalidTid)?
        .trim()
        .parse()
        .map_err(|_| TaskStatError::InvalidTid)?;
    if parsed_tid != expected_tid {
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
        "Z" => Ok(TaskState::Ending(EndingTaskState::Zombie)),
        "X" | "x" => Ok(TaskState::Ending(EndingTaskState::Dead)),
        "R" => Ok(TaskState::Active(ActiveTaskState::Running)),
        "S" => Ok(TaskState::Active(ActiveTaskState::Sleeping)),
        "D" => Ok(TaskState::Active(ActiveTaskState::DiskSleep)),
        "I" => Ok(TaskState::Active(ActiveTaskState::Idle)),
        "W" | "K" | "P" => Ok(TaskState::Active(ActiveTaskState::Waking)),
        other => Err(TaskStatError::UnknownState(other.to_owned())),
    }
}

#[cfg(target_os = "macos")]
enum StopPending {
    State(MacosActiveState),
}

#[cfg(target_os = "macos")]
impl fmt::Debug for StopPending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let StopPending::State(state) = self;
        write!(f, "lune-mux is in state {state:?}")
    }
}

#[cfg(target_os = "macos")]
enum StopEnded {
    Exited,
    PidReused {
        expected_start: MacosStartTime,
        actual_start: MacosStartTime,
    },
    Zombie,
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
            StopEnded::Zombie => f.write_str("state Zombie"),
        }
    }
}

#[cfg(target_os = "macos")]
fn mux_stopped(handle: &MuxHandle) -> MuxStopCheck {
    if handle
        .has_exited()
        .unwrap_or_else(|error| panic!("lune-mux (pid {}) exit check failed: {error}", handle.pid))
    {
        return MuxStopCheck::Ended(StopEnded::Exited);
    }
    let Some((start_time, state)) = macos_process_state(handle.pid.get()) else {
        return MuxStopCheck::Ended(StopEnded::Exited);
    };
    if start_time != handle.start_time {
        return MuxStopCheck::Ended(StopEnded::PidReused {
            expected_start: handle.start_time,
            actual_start: start_time,
        });
    }
    match state {
        MacosProcessState::Stopped => {
            if handle.has_exited().unwrap_or_else(|error| {
                panic!("lune-mux (pid {}) exit check failed: {error}", handle.pid)
            }) {
                return MuxStopCheck::Ended(StopEnded::Exited);
            }
            MuxStopCheck::Stopped
        }
        MacosProcessState::Zombie => MuxStopCheck::Ended(StopEnded::Zombie),
        MacosProcessState::Active(active) => MuxStopCheck::Pending(StopPending::State(active)),
        MacosProcessState::Unknown(raw) => panic!(
            "lune-mux (pid {}) reported unexpected macOS process status {raw}",
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
        self.mux_child = None;
        self.kill_mux_on_drop();
    }
}

#[test]
fn osc_7501_reports_flow_from_lune_mux_to_luned_and_outrank_session_files() {
    use lune_protocol::agent_status::{AgentStatus, BlockedReason};
    use lune_protocol::program_status::Message;
    use luned::api::{self, Harness, StatusRung};

    let mut owned = Owned::new();
    owned.spawn_mux(|mux| {
        mux.env("TERM_PROGRAM", "ghostty");
    });

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
    owned.spawn_mux(|_| {});
    owned.new_terminal(b"kept");
    let kept = const { Span::of(ALONE.saturating_add(SWEEP.saturating_mul(3))) };
    owned.daemon_runs_through(kept, "the daemon ends while a terminal runs");
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

#[test]
fn mux_socket_at_distinguishes_absent_self_and_connect_errors() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    assert_eq!(
        mux_socket_at(&runtime, dir.path().join("missing.sock")).unwrap(),
        MuxSocket::Absent
    );

    let regular = dir.path().join("file");
    fs::write(&regular, b"").unwrap();
    let connect_error = mux_socket_at(&runtime, regular.join("mux.sock")).unwrap_err();
    assert!(
        matches!(connect_error, MuxPeerError::Connect(_)),
        "expected Connect error, got {connect_error:?}"
    );

    let self_socket = dir.path().join("self.sock");
    let _listener =
        runtime.block_on(async { tokio::net::UnixListener::bind(&self_socket).unwrap() });
    let self_error = mux_socket_at(&runtime, self_socket).unwrap_err();
    assert!(
        matches!(
            self_error,
            MuxPeerError::InvalidPid(pid) if pid == rustix::process::getpid().as_raw_pid()
        ),
        "expected InvalidPid(self), got {self_error:?}"
    );
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
        Ok(TaskState::Active(ActiveTaskState::Sleeping))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) R 1 42 42", tid(43)),
        Ok(TaskState::Active(ActiveTaskState::Running))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) D 1 42 42", tid(43)),
        Ok(TaskState::Active(ActiveTaskState::DiskSleep))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) I 1 42 42", tid(43)),
        Ok(TaskState::Active(ActiveTaskState::Idle))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) Z 1 42 42", tid(43)),
        Ok(TaskState::Ending(EndingTaskState::Zombie))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) X 1 42 42", tid(43)),
        Ok(TaskState::Ending(EndingTaskState::Dead))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) x 1 42 42", tid(43)),
        Ok(TaskState::Ending(EndingTaskState::Dead))
    );
    assert_eq!(
        parse_task_stat(b"43 (tokio-rt) W 1 42 42", tid(43)),
        Ok(TaskState::Active(ActiveTaskState::Waking))
    );
    assert_eq!(
        parse_task_stat(b"99 (lune-mux) T 1", tid(43)),
        Err(TaskStatError::MismatchedTid {
            expected: tid(43),
            actual: tid(99),
        })
    );
    assert_eq!(
        parse_task_stat(b"0 (lune-mux) T 1", tid(43)),
        Err(TaskStatError::InvalidTid)
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
