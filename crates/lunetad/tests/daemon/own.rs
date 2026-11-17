use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use hyper_util::rt::TokioIo;
use lunetad::mux::own::api::mux_client::MuxClient;
use lunetad::mux::own::api::{self as mux_api, ListRequest, Listing};
use lunetad::mux::own::server;
use tonic::transport::{Channel, Endpoint, Uri};

use crate::host::{eventually, start, Daemon, Host, Running, LUNETAD};

const WAITS: [&str; 2] = ["sleep", "600"];

pub fn built_server() -> PathBuf {
    let program = Path::new(LUNETAD).with_file_name("luneta-mux");
    let shown = program.display();
    let rebuild = "dev test builds it, and so does cargo build -p luneta-mux";
    let built = fs::metadata(&program).and_then(|it| it.modified());
    let built = built.unwrap_or_else(|error| panic!("{shown}: {error}: {rebuild}"));
    let root = fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
    let mut sources = Vec::new();
    for source in ["crates/mux/src", "crates/mux/build.rs", "proto/luneta/mux"] {
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

    pub fn start_daemon(&mut self) {
        assert!(self.daemon.is_none(), "one daemon runs at a time");
        self.daemon = Some(start(self.host.vars()).unwrap());
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
        let socket = self.socket();
        let listening = self.runtime.block_on(async move {
            let stream = tokio::net::UnixStream::connect(socket).await.ok()?;
            stream.peer_cred().ok()?.pid()
        });
        if let Some(pid) = listening.and_then(rustix::process::Pid::from_raw) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
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
    let _first = Running::spawn(first.stdin(Stdio::null()));
    let socket = host.folder().join(server::SOCKET);
    eventually("the first server listens", || socket.exists().then_some(()));
    let mut second = host.vars().command(program);
    let second = second.stdin(Stdio::null()).stderr(Stdio::null()).status().unwrap();
    assert_eq!(second.code(), Some(i32::from(server::ANOTHER_RUNS)));
}

pub fn the_daemon_runs_on_while_a_terminal_does() {
    let mut owned = Owned::new();
    let program = built_server();
    let mut mux = owned.host.vars().command(program.to_str().unwrap());
    let _mux = Running::spawn(mux.stdin(Stdio::null()));
    let socket = owned.socket();
    eventually("the server listens", || socket.exists().then_some(()));
    owned.new_terminal(b"kept");
    let daemon = owned.daemon.as_mut().unwrap();
    assert_eq!(daemon.ended_within(Duration::from_secs(36)), None);
}
