use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;

use hyper_util::rt::TokioIo;
use lune_mux::api::{self, mux_client::MuxClient};
use lune_waits::{shell, Patience};
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Streaming;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

pub const LUNE_MUX: &str = env!("CARGO_BIN_EXE_lune-mux");

pub fn run<F: Future<Output = ()>>(test: F) {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(test);
}

pub struct Server {
    child: Child,
    runtime: TempDir,
}

impl Server {
    pub async fn start() -> Server {
        Server::started(Command::new(LUNE_MUX)).await
    }

    pub async fn start_with_files(files: u32) -> Server {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "ulimit -Sn \"$1\" && exec \"$0\" \"$2\"", LUNE_MUX]);
        command.arg(files.to_string());
        Server::started(command).await
    }

    async fn started(mut command: Command) -> Server {
        let runtime = tempfile::tempdir().unwrap();
        command
            .arg(runtime.path().join("lune"))
            .env("ZELLIJ", "0")
            .env("ZELLIJ_SESSION_NAME", "theirs")
            .env("ZELLIJ_PANE_ID", "7")
            .env_remove("LUNE_MUX_TERMINAL")
            .env_remove("LUNE_MUX_TERMINAL_MADE")
            .stdin(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (said, mut heard) = mpsc::unbounded_channel();
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                eprintln!("{line}");
                let _ = said.send(line);
            }
        });
        let listens = |line: Option<String>| {
            let line = line.expect("lune-mux says it listens before its stderr closes");
            line.starts_with("lune-mux: listening on ").then_some(())
        };
        let what = "lune-mux says it listens";
        Patience::begun().awaited_first(what, async || heard.recv().await, listens).await;
        Server { child, runtime }
    }

    #[cfg(target_os = "linux")]
    pub fn resident_kib(&self) -> u64 {
        let pid = self.child.id().to_string();
        let listed = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().unwrap();
        String::from_utf8(listed.stdout).unwrap().trim().parse().unwrap()
    }

    #[cfg(target_os = "macos")]
    pub fn resident_kib(&self) -> u64 {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let mut system = System::new();
        let pid = Pid::from_u32(self.child.id());
        let resident = ProcessRefreshKind::nothing().with_memory();
        system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, resident);
        let bytes = system.process(pid).expect("the mux server runs").memory();
        assert_eq!(bytes % 1024, 0, "resident memory is whole pages");
        bytes / 1024
    }

    pub fn socket(&self) -> PathBuf {
        self.runtime.path().join("lune/mux.sock")
    }

    pub async fn client(&self) -> MuxClient<Channel> {
        let socket = self.socket();
        let channel = Endpoint::from_static("http://lune-mux")
            .connect_with_connector(service_fn(move |_: Uri| {
                let socket = socket.clone();
                async move { UnixStream::connect(socket).await.map(TokioIo::new) }
            }))
            .await
            .unwrap();
        MuxClient::new(channel).max_decoding_message_size(lune_protocol::RPC_MESSAGE_BYTES_MAX)
    }

    pub async fn ended(&mut self) -> ExitStatus {
        lune_waits::ended_async("lune-mux ends", &mut self.child).await
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
        program: Some(api::Program { argv: shell::AT_A_PROMPT.map(str::to_owned).to_vec() }),
        ends: api::Ends::Closes.into(),
        resume_in_shell: None,
    }
}

pub fn script(script: &str, ends: api::Ends) -> api::Opens {
    let argv = ["/bin/sh", "-c", script].map(str::to_owned).to_vec();
    api::Opens {
        cwd: "/".to_owned(),
        program: Some(api::Program { argv }),
        ends: ends.into(),
        resume_in_shell: None,
    }
}

pub async fn new_terminal(
    mux: &mut MuxClient<Channel>,
    opens: api::Opens,
    tag: &[u8],
    size: api::Size,
) -> api::TerminalKey {
    let request =
        api::NewTerminalRequest { opens: Some(opens), tag: tag.to_vec(), size: Some(size) };
    mux.new_terminal(request).await.unwrap().into_inner().terminal.unwrap()
}

pub fn size(columns: u32, rows: u32) -> api::Size {
    api::Size { columns, rows }
}

pub async fn watched_until(
    watch: &mut Streaming<api::Watched>,
    what: &str,
    test: impl Fn(&api::Listing) -> bool,
) -> api::Listing {
    let patience = Patience::begun();
    let mut last = None;
    loop {
        let waited = fmt::from_fn(|f| write!(f, "the watch says {what}: last {last:#?}"));
        let next = patience.awaited(waited, watch.message()).await;
        let watched = next.unwrap().expect("the watch went on");
        let Some(api::watched::Is::Listing(listing)) = watched.is else {
            panic!("the watch ended before it said {what}: {:?}", watched.is);
        };
        if test(&listing) {
            return listing;
        }
        last = Some(listing);
    }
}

pub async fn watched_end(watch: &mut Streaming<api::Watched>) -> api::watched::ended::Why {
    let patience = Patience::begun();
    let ends = |next: Result<Option<api::Watched>, tonic::Status>| {
        let watched = next.unwrap().expect("the watch says why before it ends");
        match watched.is.expect("the watch says something") {
            api::watched::Is::Listing(_) => None,
            api::watched::Is::Ended(ended) => Some(ended),
        }
    };
    let what = "the watch says why it ended";
    let ended = patience.awaited_first(what, async || watch.message().await, ends).await;
    let after = patience.awaited("the watch ends", watch.message()).await;
    assert!(after.unwrap().is_none(), "the watch ends once it says why");
    ended.why.expect("the watch says why it ended")
}

pub async fn listed(mux: &mut MuxClient<Channel>) -> api::Listing {
    mux.list(api::ListRequest {}).await.unwrap().into_inner()
}

type Screens = BTreeMap<u32, (Vec<String>, api::Frame)>;

pub struct Viewer {
    pub refused: Vec<String>,
    pub closed: Vec<u32>,
    screens: Screens,
    to: mpsc::Sender<api::FromViewer>,
    from: Streaming<api::ToViewer>,
}

impl Viewer {
    pub async fn new(mux: &mut MuxClient<Channel>) -> Viewer {
        Viewer::hello(mux, api::Greeting::default()).await.unwrap()
    }

    pub async fn hello(
        mux: &mut MuxClient<Channel>,
        greeting: api::Greeting,
    ) -> Result<Viewer, tonic::Status> {
        let (to, said) = mpsc::channel(64);
        to.send(viewer_said(api::from_viewer::Event::Hello(greeting))).await.unwrap();
        let from = mux.show(ReceiverStream::new(said)).await?.into_inner();
        Ok(Viewer { refused: Vec::new(), closed: Vec::new(), screens: BTreeMap::new(), to, from })
    }

    pub async fn send(&self, said: api::from_viewer::Event) {
        self.to.send(viewer_said(said)).await.unwrap();
    }

    pub async fn show(&self, terminals: &[api::TerminalKey]) {
        let showing = api::Showing { terminals: terminals.to_vec() };
        self.send(api::from_viewer::Event::Showing(showing)).await;
    }

    pub async fn press_enter(&self, terminal: api::TerminalKey) {
        self.type_line(terminal, "").await;
    }

    async fn type_line(&self, terminal: api::TerminalKey, text: &str) {
        for key in line_keys(text) {
            let (terminal, event) = (Some(terminal), Some(api::typed::Event::Key(key)));
            self.send(api::from_viewer::Event::Typed(api::Typed { terminal, event })).await;
        }
    }

    pub async fn press_ctrl(&self, terminal: api::TerminalKey, character: char) {
        let key = api::Key {
            press: api::Press::Down.into(),
            modifiers: Some(api::Modifiers { ctrl: true, ..api::Modifiers::default() }),
            key: Some(api::key::Key::Character(character.into())),
            text: String::new(),
        };
        let (terminal, event) = (Some(terminal), Some(api::typed::Event::Key(key)));
        self.send(api::from_viewer::Event::Typed(api::Typed { terminal, event })).await;
    }

    pub async fn next(
        &mut self,
        patience: Patience,
        what: &str,
    ) -> Result<Option<api::to_viewer::Event>, tonic::Status> {
        let screens = &self.screens;
        let awaited_and_seen = fmt::from_fn(|f| write!(f, "{what}, but {}", seen(screens)));
        let next = patience.awaited(awaited_and_seen, self.from.message()).await;
        let Some(message) = next? else {
            return Ok(None);
        };
        let event = message.event.expect("a viewer hears an event");
        match &event {
            api::to_viewer::Event::Frame(frame) => applied(&mut self.screens, frame),
            api::to_viewer::Event::Refused(refused) => self.refused.push(refused.reason.clone()),
            api::to_viewer::Event::Closed(closed) => self.closed.push(closed.terminal.unwrap().id),
        }
        Ok(Some(event))
    }

    pub async fn until(&mut self, what: &str, test: impl Fn(&Viewer) -> bool) {
        self.until_found(what, |viewer| test(viewer).then_some(())).await;
    }

    async fn until_found<T>(&mut self, what: &str, find: impl Fn(&Viewer) -> Option<T>) -> T {
        let patience = Patience::begun();
        loop {
            if let Some(found) = find(self) {
                return found;
            }
            let next = self.next(patience, what).await.unwrap();
            assert!(next.is_some(), "the stream ended before {what}");
        }
    }

    pub async fn until_row(&mut self, terminal: u32, text: &str) {
        self.until(&format!("a row of terminal {terminal} reads {text:?}"), |viewer| {
            viewer.rows_of(terminal).iter().any(|row| row == text)
        })
        .await;
    }

    pub async fn until_prompted(&mut self, terminal: u32) -> shell::Prompted {
        let what = format!("terminal {terminal}'s shell prompts");
        self.until_found(&what, |viewer| shell::prompted(&viewer.screen_of(terminal))).await
    }

    pub async fn type_at(&self, terminal: api::TerminalKey, prompted: &shell::Prompted, line: &str) -> shell::Typed {
        let typed = prompted.typed(line);
        self.type_line(terminal, typed.line()).await;
        typed
    }

    pub async fn until_said(&mut self, terminal: u32, typed: &shell::Typed) -> Vec<String> {
        let what = format!("terminal {terminal} says what {:?} does, then prompts", typed.line());
        self.until_found(&what, |viewer| typed.said(&viewer.screen_of(terminal)).map(<[String]>::to_vec)).await
    }

    fn screen_of(&self, terminal: u32) -> shell::Screen {
        let rows = self.rows_of(terminal).to_vec();
        let cursor = self.frame_of(terminal).and_then(|frame| frame.cursor.as_ref()).map(|cursor| shell::Cursor {
            row: usize::try_from(cursor.row).unwrap(),
            column: usize::try_from(cursor.column).unwrap(),
        });
        shell::Screen::new(rows, cursor).unwrap_or_else(|why| panic!("terminal {terminal}'s screen is broken: {why}"))
    }

    pub fn rows_of(&self, terminal: u32) -> &[String] {
        self.screens.get(&terminal).map_or(&[], |(rows, _)| rows)
    }

    pub fn frame_of(&self, terminal: u32) -> Option<&api::Frame> {
        self.screens.get(&terminal).map(|(_, frame)| frame)
    }
}

fn seen(screens: &Screens) -> impl fmt::Display {
    fmt::from_fn(move |f| {
        if screens.is_empty() {
            return write!(f, "no terminal was shown");
        }
        let mut separator = "";
        for (terminal, (rows, _)) in screens {
            write!(f, "{separator}terminal {terminal} shows:")?;
            for row in rows.iter().filter(|row| !row.trim().is_empty()) {
                write!(f, "\n  {row:?}")?;
            }
            separator = "\n";
        }
        Ok(())
    })
}

fn applied(screens: &mut Screens, frame: &api::Frame) {
    let terminal = frame.terminal.unwrap().id;
    match frame.rows.as_ref().unwrap() {
        api::frame::Rows::Whole(whole) => {
            let rows = whole.lines.iter().map(line_text).collect();
            screens.insert(terminal, (rows, frame.clone()));
        },
        api::frame::Rows::Changed(changed) => {
            let Some((rows, last)) = screens.get_mut(&terminal) else {
                return;
            };
            for row in &changed.rows {
                let index = usize::try_from(row.index).unwrap();
                rows[index] = line_text(row.line.as_ref().unwrap());
            }
            *last = frame.clone();
        },
    }
}

fn line_keys(text: &str) -> Vec<api::Key> {
    let mut keys: Vec<api::Key> = text
        .chars()
        .map(|character| api::Key {
            press: api::Press::Down.into(),
            modifiers: None,
            key: Some(api::key::Key::Character(character.into())),
            text: character.to_string(),
        })
        .collect();
    keys.push(api::Key {
        press: api::Press::Down.into(),
        modifiers: None,
        key: Some(api::key::Key::Named(api::Named::Enter.into())),
        text: String::new(),
    });
    keys
}

fn viewer_said(said: api::from_viewer::Event) -> api::FromViewer {
    api::FromViewer { event: Some(said) }
}

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

pub fn gone(path: &Path) -> bool {
    !path.exists()
}
