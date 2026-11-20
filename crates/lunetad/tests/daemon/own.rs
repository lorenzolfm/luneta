use std::convert::Infallible;
use std::env;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hyper_util::rt::TokioIo;
use luneta_waits::{eventually, Span};
use lunetad::mux::own::api::mux_client::MuxClient;
use lunetad::mux::own::api::{self as mux_api, InfoRequest, InfoResponse, ListRequest, Listing};
use lunetad::mux::own::server;
use lunetad::{ALONE, SWEEP};
use tempfile::TempDir;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::codegen::{http, Body, BoxFuture, Context, Poll, Service, StdError};
use tonic::server::{Grpc, NamedService, UnaryService};
use tonic::service::Routes;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tonic::Status;
use tonic_prost::ProstCodec;

use crate::demand::build_of;
use crate::host::{script, start, start_as, Daemon, Host, Running, LUNETAD};

const STAND_IN_MUX: &str = "LUNETA_TEST_STAND_IN_MUX_FOLDER";

const STAND_IN_SAYS: &str = "LUNETA_TEST_STAND_IN_MUX_SAYS";

const LOCK: &str = "mux.lock";

const WAITS: [&str; 2] = ["sleep", "600"];

pub fn built_server() -> PathBuf {
    let program = Path::new(LUNETAD).with_file_name("luneta-mux");
    let shown = program.display();
    let rebuild = "dev test builds it, and so does cargo build -p luneta-mux";
    let built = fs::metadata(&program).and_then(|it| it.modified());
    let built = built.unwrap_or_else(|error| panic!("{shown}: {error}: {rebuild}"));
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let mut sources = Vec::new();
    let built_from = [
        "crates/mux/src",
        "crates/mux/build.rs",
        "crates/protocol/src",
        "crates/protocol/build.rs",
        "proto/luneta",
    ];
    for source in built_from {
        files_in(&root.join(source), &mut sources);
    }
    assert!(sources.len() > 2, "luneta-mux's sources are not in {}", root.display());
    for source in sources {
        let changed = fs::metadata(&source).and_then(|it| it.modified());
        let fresh = changed.is_ok_and(|changed| changed <= built);
        assert!(fresh, "{} changed since {shown} was built: {rebuild}", source.display());
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
    pub host: Host,
    daemon: Option<Daemon>,
    runtime: tokio::runtime::Runtime,
}

impl Owned {
    pub fn new() -> Self {
        built_server();
        let host = Host::new();
        let daemon = start(host.vars()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Owned { host, daemon: Some(daemon), runtime }
    }

    pub fn stop_daemon(&mut self) {
        self.daemon = None;
    }

    pub fn daemon_ended(&mut self, what: &str) -> std::process::ExitStatus {
        self.daemon.as_mut().expect("a daemon was started").ended(what)
    }

    pub fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "one daemon runs at a time");
        self.daemon = Some(start(self.host.vars()).unwrap());
    }

    pub fn start_daemon_from(&mut self, lunetad: &Path) {
        assert!(self.daemon.is_none(), "one daemon runs at a time");
        let lunetad = self.host.vars().command(lunetad.to_str().unwrap());
        self.daemon = Some(start_as(lunetad).unwrap());
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
            mux_client(socket).await.list(ListRequest {}).await.unwrap().into_inner()
        })
    }

    pub fn dumped(&self, terminal: mux_api::TerminalKey) -> String {
        let socket = self.socket();
        let dumped = mux_api::Dumped::Text.into();
        let request = mux_api::DumpRequest { dumped, terminal: Some(terminal) };
        self.runtime.block_on(async move {
            mux_client(socket).await.dump(request).await.unwrap().into_inner().text
        })
    }

    pub fn new_terminal(&self, tag: &[u8]) -> mux_api::TerminalKey {
        let socket = self.socket();
        let runs = Some(mux_api::opens::Runs::Program(mux_api::Program {
            argv: WAITS.map(str::to_owned).to_vec(),
        }));
        let ends = mux_api::Ends::Holds.into();
        let opens = Some(mux_api::Opens { cwd: self.home(), runs, ends });
        let size = Some(mux_api::Size { columns: 80, rows: 24 });
        let request = mux_api::NewTerminalRequest { opens, tag: tag.to_vec(), size };
        self.runtime.block_on(async move {
            let made = mux_client(socket).await.new_terminal(request).await.unwrap();
            made.into_inner().terminal.unwrap()
        })
    }

    pub fn close_terminal(&self, request: mux_api::CloseTerminalRequest) {
        let socket = self.socket();
        self.runtime.block_on(async move {
            mux_client(socket).await.close_terminal(request).await.unwrap();
        });
    }

    pub fn kill_mux(&self) {
        if let Some(pid) = self.mux_pid() {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }

    pub fn stall_mux(&self) -> StalledMux {
        let pid = self.mux_pid().expect("luneta-mux listens");
        rustix::process::kill_process(pid, rustix::process::Signal::STOP).unwrap();
        StalledMux(pid)
    }

    fn mux_pid(&self) -> Option<rustix::process::Pid> {
        let socket = self.socket();
        let listening = self.runtime.block_on(async move {
            let stream = tokio::net::UnixStream::connect(socket).await.ok()?;
            stream.peer_cred().ok()?.pid()
        });
        listening.and_then(rustix::process::Pid::from_raw)
    }

    pub fn stand_in_mux(&self, says: Option<&str>) -> Running {
        let mut command = Command::new(env::current_exe().unwrap());
        command.args(["own::stand_in_mux", "--exact", "--ignored"]);
        command.env(STAND_IN_MUX, self.host.folder()).stdout(Stdio::null()).stderr(Stdio::null());
        if let Some(says) = says {
            command.env(STAND_IN_SAYS, says);
        }
        let stand_in = Running::spawn(&mut command);
        let pid = rustix::process::Pid::from_raw(i32::try_from(stand_in.pid()).unwrap());
        eventually("the stand-in luneta-mux listens", || (self.mux_pid() == pid).then_some(()));
        stand_in
    }
}

pub fn beside_a_stand_in_mux(says: &str) -> TempDir {
    let beside = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fs::hard_link(LUNETAD, beside.path().join("lunetad")).unwrap();
    let test = env::current_exe().unwrap();
    let test = test.display();
    let runs = format!(
        "{STAND_IN_MUX}=\"$1\" {STAND_IN_SAYS}='{says}' exec '{test}' own::stand_in_mux --exact \
         --ignored\n"
    );
    script(&beside.path().join("luneta-mux"), &runs);
    beside
}

#[test]
#[ignore = "the stand-in for a luneta-mux of another build, which a test runs as a process"]
fn stand_in_mux() {
    let Some(folder) = env::var_os(STAND_IN_MUX).map(PathBuf::from) else {
        return;
    };
    let says = env::var(STAND_IN_SAYS).ok().map(build_of);
    let says = says.map(|(version, built)| InfoResponse { version, built });
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(folder.join(LOCK));
    let lock = lock.unwrap();
    lock.lock().unwrap();
    let socket = folder.join(server::SOCKET);
    let _ = fs::remove_file(&socket);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
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
    const NAME: &'static str = "luneta.mux.v1.Mux";
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
        let info = request.uri().path() == "/luneta.mux.v1.Mux/Info";
        let says = self.0.clone().filter(|_| info);
        Box::pin(async move {
            let Some(says) = says else {
                return Ok(Status::unimplemented("a stand-in luneta-mux").into_http());
            };
            Ok(Grpc::new(ProstCodec::default()).unary(Says(says), request).await)
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

pub struct StalledMux(rustix::process::Pid);

impl Drop for StalledMux {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process(self.0, rustix::process::Signal::CONT);
    }
}

async fn mux_client(socket: PathBuf) -> MuxClient<Channel> {
    let channel = Endpoint::from_static("http://luneta-mux")
        .connect_with_connector(tower::service_fn(move |_: Uri| {
            let socket = socket.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(socket).await?;
                std::io::Result::Ok(TokioIo::new(stream))
            }
        }))
        .await
        .unwrap();
    MuxClient::new(channel)
}

impl Drop for Owned {
    fn drop(&mut self) {
        self.kill_mux();
    }
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
    let second = second.stdin(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert_eq!(second.code(), Some(i32::from(server::ANOTHER_RUNS)));
}

pub fn the_daemon_runs_on_while_a_terminal_does() {
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
