//! A server of a test's own, and clients of it.

use std::future::Future;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc as std_mpsc;
use std::thread;
use std::time::{Duration, Instant};

use hyper_util::rt::TokioIo;
use luneta_mux::api::{self, mux_client::MuxClient};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

pub const LUNETA_MUX: &str = env!("CARGO_BIN_EXE_luneta-mux");

/// How long a test waits for what it waits for.
pub const PATIENCE: Duration = Duration::from_secs(20);

/// Runs `test` to its end on a runtime of its own.
pub fn run<F: Future<Output = ()>>(test: F) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(test);
}

/// A server, started as one is in a pane of zellij's, with zellij's variables
/// set, in a runtime folder of its own.
pub struct Server {
    child: Child,
    runtime: TempDir,
}

impl Server {
    pub fn start() -> Server {
        Server::started(Command::new(LUNETA_MUX))
    }

    /// A server that starts with a limit of `files` open files, as a shell
    /// that set it starts it.
    pub fn start_with_files(files: u32) -> Server {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -Sn \"$1\" && exec \"$0\"", LUNETA_MUX]);
        command.arg(files.to_string());
        Server::started(command)
    }

    fn started(mut command: Command) -> Server {
        let runtime = tempfile::tempdir().unwrap();
        command
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("SHELL", "/bin/sh")
            .env("ZELLIJ", "0")
            .env("ZELLIJ_SESSION_NAME", "theirs")
            .env("ZELLIJ_PANE_ID", "7")
            .env_remove("LUNETA_MUX_SESSION")
            .env_remove("LUNETA_MUX_MADE")
            .env_remove("LUNETA_MUX_PANE")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (said, heard) = std_mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("{line}");
                let _ = said.send(line);
            }
        });
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = heard.recv_timeout(left).expect("luneta-mux said it listens");
            if line.starts_with("luneta-mux: listening on ") {
                break;
            }
        }
        Server { child, runtime }
    }

    /// The server's resident memory, in KiB, as `ps` says.
    pub fn resident_kib(&self) -> u64 {
        let pid = self.child.id().to_string();
        let listed = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().unwrap();
        String::from_utf8(listed.stdout).unwrap().trim().parse().unwrap()
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.path().join("luneta/mux.sock")
    }

    pub async fn client(&self) -> MuxClient<Channel> {
        let socket = self.socket();
        let channel = Endpoint::from_static("http://luneta-mux")
            .connect_with_connector(service_fn(move |_: Uri| {
                let socket = socket.clone();
                async move { UnixStream::connect(socket).await.map(TokioIo::new) }
            }))
            .await
            .unwrap();
        MuxClient::new(channel)
    }

    /// How the server ended, which it does within `PATIENCE`.
    pub async fn ended(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "luneta-mux did not end");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn shell() -> api::Opens {
    api::Opens {
        cwd: "/".to_owned(),
        runs: Some(api::opens::Runs::Shell(api::Shell {})),
        ends: api::Ends::Closes.into(),
    }
}

/// What runs `script` in `/bin/sh`, in `/`, its tab ending as `ends` says.
pub fn script(script: &str, ends: api::Ends) -> api::Opens {
    let argv = ["/bin/sh", "-c", script].map(str::to_owned).to_vec();
    api::Opens {
        cwd: "/".to_owned(),
        runs: Some(api::opens::Runs::Program(api::Program { argv })),
        ends: ends.into(),
    }
}

pub async fn new_session(
    mux: &mut MuxClient<Channel>,
    name: &str,
    opens: api::Opens,
) -> api::NewSessionResponse {
    let request = api::NewSessionRequest { name: name.to_owned(), opens: Some(opens) };
    mux.new_session(request).await.unwrap().into_inner()
}

pub async fn new_tab(
    mux: &mut MuxClient<Channel>,
    session: u64,
    opens: api::Opens,
) -> api::NewTabResponse {
    let request = api::NewTabRequest { session, name: None, opens: Some(opens) };
    mux.new_tab(request).await.unwrap().into_inner()
}

/// The session `made` says it made.
pub fn session(made: &api::NewSessionResponse) -> api::Place {
    api::Place { is: Some(api::place::Is::Session(made.session)), made_at: made.made_at }
}

/// The tab `id` of the session `made` says it made.
pub fn tab(id: u32, made: &api::NewSessionResponse) -> api::Place {
    api::Place { is: Some(api::place::Is::Tab(id)), made_at: made.made_at }
}

pub fn size(columns: u32, rows: u32) -> api::Size {
    api::Size { columns, rows }
}

/// The sessions a watch says, each time they change, until `test` holds of
/// them.
pub async fn watched_until(
    watch: &mut Streaming<api::Listing>,
    what: &str,
    test: impl Fn(&api::Listing) -> bool,
) -> api::Listing {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    let mut last = None;
    loop {
        let next = tokio::time::timeout_at(deadline, watch.message()).await;
        let Ok(next) = next else {
            panic!("the watch never said {what}: last {last:#?}");
        };
        let listing = next.unwrap().expect("the watch went on");
        if test(&listing) {
            return listing;
        }
        last = Some(listing);
    }
}

/// The sessions as they stand.
pub async fn listed(mux: &mut MuxClient<Channel>) -> api::Listing {
    mux.list(api::ListRequest {}).await.unwrap().into_inner()
}

/// The tabs of the first session `listing` has, none where it has none.
pub fn tabs(listing: &api::Listing) -> &[api::Tab] {
    listing.sessions.first().map_or(&[], |session| &session.tabs)
}

/// How the program of `pane` ended, none while it runs.
pub fn ended(pane: &api::Pane) -> Option<api::Exit> {
    match &pane.program {
        Some(api::pane::Program::Ended(exit)) => Some(*exit),
        Some(api::pane::Program::Pid(_)) | None => None,
    }
}

/// A client attached to a pane, and the text of the rows it was sent.
pub struct Attached {
    pub client: u64,
    pub went: api::Went,
    pub rows: Vec<String>,
    pub last: Option<api::Frame>,
    to: mpsc::Sender<api::ToServer>,
    from: Streaming<api::ToClient>,
}

impl Attached {
    pub async fn new(mux: &mut MuxClient<Channel>, to: api::Place, size: api::Size) -> Attached {
        Attached::hello(mux, api::Hello { to: Some(to), size: Some(size), colours: None })
            .await
            .unwrap()
    }

    pub async fn hello(
        mux: &mut MuxClient<Channel>,
        hello: api::Hello,
    ) -> Result<Attached, tonic::Status> {
        let (to, said) = mpsc::channel(64);
        to.send(event(api::to_server::Event::Hello(hello))).await.unwrap();
        let mut from = mux.attach(ReceiverStream::new(said)).await?.into_inner();
        let Some(api::to_client::Event::Attached(api::Attached { client })) =
            from.message().await?.and_then(|message| message.event)
        else {
            panic!("the first word was not attached");
        };
        let mut attached =
            Attached { client, went: api::Went::default(), rows: Vec::new(), last: None, to, from };
        attached.went = attached.next_went().await;
        Ok(attached)
    }

    pub async fn send(&self, said: api::to_server::Event) {
        self.to.send(event(said)).await.unwrap();
    }

    /// Types `text` as keys, a key a character, and Enter after.
    pub async fn type_line(&self, text: &str) {
        for character in text.chars() {
            let key = api::Key {
                press: api::Press::Down.into(),
                modifiers: None,
                key: Some(api::key::Key::Character(character.into())),
                text: character.to_string(),
            };
            self.send(api::to_server::Event::Key(key)).await;
        }
        let enter = api::Key {
            press: api::Press::Down.into(),
            modifiers: None,
            key: Some(api::key::Key::Named(api::Named::Enter.into())),
            text: String::new(),
        };
        self.send(api::to_server::Event::Key(enter)).await;
    }

    /// The next message, or none where the stream ended.
    pub async fn next(&mut self) -> Option<api::to_client::Event> {
        let next = tokio::time::timeout(PATIENCE, self.from.message()).await;
        let message = next.expect("the server said something in time").ok()??;
        let event = message.event?;
        if let api::to_client::Event::Frame(frame) = &event
            && frame.pane == self.went.pane
        {
            self.apply(frame);
        }
        if let api::to_client::Event::Went(went) = &event {
            self.went = *went;
            self.rows.clear();
        }
        Some(event)
    }

    async fn next_went(&mut self) -> api::Went {
        loop {
            if let Some(api::to_client::Event::Went(went)) = self.next().await {
                return went;
            }
        }
    }

    /// The rows until `test` holds of them.
    pub async fn until(&mut self, what: &str, test: impl Fn(&Attached) -> bool) {
        while !test(self) {
            assert!(self.next().await.is_some(), "the stream ended before {what}: {}", self.text());
        }
    }

    /// Waits until a row reads `text`, trimmed.
    pub async fn until_row(&mut self, text: &str) {
        self.until(&format!("a row reads {text:?}"), |attached| {
            attached.rows.iter().any(|row| row == text)
        })
        .await;
    }

    pub fn text(&self) -> String {
        self.rows.join("\n")
    }

    fn apply(&mut self, frame: &api::Frame) {
        match frame.rows.as_ref().unwrap() {
            api::frame::Rows::Whole(whole) => {
                self.rows = whole.lines.iter().map(line_text).collect();
            },
            api::frame::Rows::Changed(changed) => {
                for row in &changed.rows {
                    let index = usize::try_from(row.index).unwrap();
                    self.rows[index] = line_text(row.line.as_ref().unwrap());
                }
            },
        }
        self.last = Some(frame.clone());
    }
}

fn event(said: api::to_server::Event) -> api::ToServer {
    api::ToServer { event: Some(said) }
}

/// A line's text, with no spaces at its end.
fn line_text(line: &api::Line) -> String {
    let mut text = String::new();
    for glyph in &line.glyphs {
        match glyph.shows.as_ref().unwrap() {
            api::glyph::Shows::Blank(_) => text.push(' '),
            api::glyph::Shows::Narrow(narrow) => text.push_str(&narrow.grapheme),
            api::glyph::Shows::Wide(wide) => text.push_str(&wide.grapheme),
        }
    }
    text.trim_end().to_owned()
}

/// Whether nothing is at `path`.
pub fn gone(path: &Path) -> bool {
    !path.exists()
}
