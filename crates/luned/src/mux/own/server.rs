use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use hyper_util::rt::TokioIo;
use lune_protocol::shell::Doing;
use rustix::process::{Pid, Signal};
use tokio::net::UnixStream;
use tokio::runtime::{self, Runtime};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Response, Status, Streaming};
use tower::service_fn;

use super::api::{self, mux_client::MuxClient};
use super::frames::colours;
use super::{in_no_terminal, TerminalKey};
use crate::changes::Unchanged;
use crate::client::Within;
use crate::mux::{Failure, Runs};
use crate::projects::{Line, Size, VIEWS_MAX};
use crate::{folder, log, release, Directory};

pub const SOCKET: &str = "mux.sock";

const PROGRAM: &str = "lune-mux";

pub const ANOTHER_RUNS: u8 = 3;

const CONNECT: Duration = Duration::from_millis(500);

const ANSWER: Duration = Duration::from_millis(1500);

const WAITS_PER_ANSWER_MAX: u128 = 2;

const _: () = assert!(
    WAITS_PER_ANSWER_MAX * (CONNECT.as_millis() + ANSWER.as_millis())
        < crate::client::ANSWER.waits().as_millis()
);

const MAKING: Duration = Duration::from_secs(10);

const STOPPING: Duration = Duration::from_secs(2);

const LOOK: Duration = Duration::from_millis(10);

pub const PRESSES_MAX: usize = crate::choices::CHOICES_MAX - 1;

const _: () = assert!(
    PRESSES_MAX + 1 == crate::choices::CHOICES_MAX,
    "an answer presses at once at most the moves from a menu's first choice to its last"
);

const _: () = assert!(PRESSES_MAX >= 2, "a text answer's paste and Enter go at once");

const AGAIN: Duration = Duration::from_millis(100);

static MATCHED: AtomicI32 = AtomicI32::new(0);

static STARTED: AtomicI32 = AtomicI32::new(0);

pub(super) struct Unmatched {
    pid: Option<Pid>,
    build: Option<release::Other>,
}

impl Unmatched {
    fn is_newer(&self) -> bool {
        self.build.as_ref().is_some_and(release::Other::is_newer)
    }

    fn left_be(&self) -> String {
        let end = |pid| format!("end it (`kill {pid}`)");
        let end = self.pid.map_or_else(|| "end it".to_owned(), end);
        format!(
            "{self}, so luned leaves it be: open a lune of its build, or {end} for this \
             luned to start its own, which ends what its terminals run"
        )
    }
}

impl fmt::Display for Unmatched {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pid = self.pid.map_or_default(|pid| format!(" (pid {pid})"));
        let own = release::SAID.as_str();
        match &self.build {
            Some(build) if build.is_newer() => {
                write!(f, "lune-mux {build}{pid} is newer than {own}")
            },
            Some(build) => write!(f, "lune-mux {build}{pid} is another build than {own}"),
            None => write!(f, "lune-mux{pid} says no build, as one older than {own} does"),
        }
    }
}

pub(super) enum Reached {
    Unreached,
    Matched(Matched),
    Unmatched(Unmatched),
}

pub(super) struct Matched(Channel);

impl Matched {
    pub(super) fn streams(self) -> Streams {
        Streams(MuxClient::new(self.0))
    }
}

pub(super) struct Streams(MuxClient<Channel>);

impl Streams {
    pub(super) async fn show(
        &mut self,
        words: impl tonic::IntoStreamingRequest<Message = api::FromViewer>,
    ) -> Result<Response<Streaming<api::ToViewer>>, Status> {
        self.0.show(words).await
    }

    async fn watch(&mut self) -> Result<Response<Streaming<api::Watched>>, Status> {
        self.0.watch(api::WatchRequest {}).await
    }
}

struct Server {
    runtime: Runtime,
    client: MuxClient<Within<Channel>>,
}

impl Server {
    fn matched(runtime: Runtime, Matched(channel): Matched) -> Server {
        Server { runtime, client: MuxClient::new(Within::new(channel, ANSWER, unanswered)) }
    }

    fn reached() -> Result<Option<Server>, Failure> {
        match Server::reached_on(runtime()?)? {
            (runtime, Reached::Matched(matched)) => Ok(Some(Server::matched(runtime, matched))),
            (_, Reached::Unreached) => Ok(None),
            (_, Reached::Unmatched(unmatched)) => Err(Failure::Own(unmatched.to_string())),
        }
    }

    fn reached_on(runtime: Runtime) -> Result<(Runtime, Reached), Failure> {
        let socket = socket()?;
        match runtime.block_on(connect(&socket)) {
            Ok(reached) => Ok((runtime, reached)),
            Err(error) => Err(Failure::Own(unreached_at(&socket, &error))),
        }
    }

    fn matched_on(runtime: Runtime) -> Result<Result<Server, Runtime>, Failure> {
        match Server::reached_on(runtime)? {
            (runtime, Reached::Matched(matched)) => Ok(Ok(Server::matched(runtime, matched))),
            (runtime, Reached::Unreached) => Ok(Err(runtime)),
            (runtime, Reached::Unmatched(unmatched)) => {
                stop(&runtime, &unmatched)?;
                Ok(Err(runtime))
            },
        }
    }

    fn into_runtime(self) -> Runtime {
        let Server { runtime, client } = self;
        drop(client);
        runtime
    }

    fn ask<T, F>(&self, asking: impl FnOnce(MuxClient<Within<Channel>>) -> F) -> Result<T, Status>
    where
        F: Future<Output = Result<Response<T>, Status>>,
    {
        self.runtime.block_on(asking(self.client.clone())).map(Response::into_inner)
    }

}

fn stop(runtime: &Runtime, unmatched: &Unmatched) -> Result<(), Failure> {
    let unstopped = |why: String| Failure::Own(format!("{unmatched}, and is not stopped: {why}"));
    if let Some(pid) = unmatched.pid
        && Pid::from_raw(STARTED.load(Ordering::Relaxed)) == Some(pid)
    {
        let from = program();
        let from = from.display();
        return Err(Failure::Own(format!("{unmatched}, though luned started it from {from}")));
    }
    if unmatched.is_newer() {
        return Err(Failure::Own(unmatched.left_be()));
    }
    let pid = unmatched.pid.ok_or_else(|| unstopped("the kernel names no pid for it".to_owned()))?;
    if pid == rustix::process::getpid() {
        return Err(unstopped("its socket leads back to luned itself".to_owned()));
    }
    match rustix::process::kill_process(pid, Signal::TERM) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => {},
        Err(error) => return Err(unstopped(error.to_string())),
    }
    log::say(&format!("{unmatched}, and is stopped"));
    let socket = socket()?;
    let stopped = Instant::now();
    let deadline = stopped + STOPPING;
    let mut answered = "it neither answers nor goes".to_owned();
    loop {
        let asked = runtime.block_on(async {
            tokio::time::timeout_at(deadline.into(), connect(&socket)).await
        });
        match asked {
            Ok(Ok(Reached::Unmatched(still))) if still.pid == unmatched.pid => {
                "it still answers".clone_into(&mut answered);
            },
            Ok(Ok(Reached::Unreached | Reached::Matched(_) | Reached::Unmatched(_))) => {
                return Ok(());
            },
            Ok(Err(error)) => answered = format!("its socket says {error}"),
            Err(_) => {},
        }
        if Instant::now() >= deadline {
            let waited = stopped.elapsed().as_millis();
            let runs = format!("{unmatched}, and {waited}ms after it was stopped {answered}");
            return Err(Failure::Own(runs));
        }
        thread::sleep(LOOK);
    }
}

pub(super) fn runtime() -> Result<Runtime, Failure> {
    let runtime = runtime::Builder::new_current_thread().enable_all().build();
    runtime.map_err(|error| Failure::Own(format!("no runtime to reach lune-mux: {error}")))
}

fn unanswered(within: Duration) -> String {
    format!("it did not answer in {}s", within.as_secs_f64())
}

pub(super) fn refused(status: &Status) -> Failure {
    Failure::Called(Reply::of(status))
}

pub(super) fn unreached_at(socket: &Path, error: &io::Error) -> String {
    assert!(socket.is_absolute(), "lune-mux's socket is found in an absolute folder");
    format!("lune-mux at {}: {error}", socket.display())
}

#[derive(Debug)]
pub struct Reply {
    code: Code,
    said: String,
}

impl Reply {
    fn of(status: &Status) -> Reply {
        let said = match status.message() {
            "" => status.code().description().to_owned(),
            said => said.to_owned(),
        };
        Reply { code: status.code(), said }
    }

    #[must_use]
    pub fn code(&self) -> Code {
        self.code
    }
}

impl fmt::Display for Reply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.said)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown(u64);

impl Shown {
    #[must_use]
    pub fn of(shown: u64) -> Shown {
        Shown(shown)
    }

    #[must_use]
    pub fn said(self) -> u64 {
        self.0
    }
}

#[derive(Debug)]
pub struct Looked {
    pub text: String,
    pub shown: Shown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressed {
    Done,
    Changed,
}

pub fn socket() -> Result<PathBuf, Failure> {
    Ok(listened_in()?.join(SOCKET))
}

fn listened_in() -> Result<PathBuf, Failure> {
    folder::find().map_err(|unfound| Failure::Own(unfound.to_string()))
}

pub(super) async fn connect(socket: &Path) -> io::Result<Reached> {
    let (first, pid) = match UnixStream::connect(socket).await {
        Ok(stream) => mine(stream)?,
        Err(error)
            if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) =>
        {
            return Ok(Reached::Unreached);
        },
        Err(error) => return Err(error),
    };
    let first = Arc::new(Mutex::new(Some(first)));
    let channel = Endpoint::from_static("http://lune-mux")
        .connect_timeout(CONNECT)
        .connect_with_connector(service_fn(move |_: Uri| {
            let first = first.lock().unwrap_or_else(PoisonError::into_inner).take();
            async move {
                let gone = || io::Error::new(ErrorKind::NotConnected, "the server reached went");
                io::Result::Ok(TokioIo::new(first.ok_or_else(gone)?))
            }
        }))
        .await
        .map_err(io::Error::other)?;
    if pid.is_some() && Pid::from_raw(MATCHED.load(Ordering::Relaxed)) == pid {
        return Ok(Reached::Matched(Matched(channel)));
    }
    let mut client = MuxClient::new(channel.clone());
    let asked = tokio::time::timeout(ANSWER, client.info(api::InfoRequest {})).await;
    let late = || {
        let late = format!("it did not say its build in {}s", ANSWER.as_secs_f64());
        io::Error::new(ErrorKind::TimedOut, late)
    };
    let build = match asked.map_err(|_| late())? {
        Ok(info) => {
            let api::InfoResponse { version, built } = info.into_inner();
            match release::of(&version, built) {
                release::Of::Own => {
                    if let Some(pid) = pid {
                        MATCHED.store(pid.as_raw_pid(), Ordering::Relaxed);
                    }
                    return Ok(Reached::Matched(Matched(channel)));
                },
                release::Of::Other(other) => Some(other),
            }
        },
        Err(status) if status.code() == Code::Unimplemented => None,
        Err(status) => return Err(io::Error::other(Reply::of(&status).to_string())),
    };
    Ok(Reached::Unmatched(Unmatched { pid, build }))
}

fn mine(stream: UnixStream) -> io::Result<(UnixStream, Option<Pid>)> {
    let peer = stream.peer_cred()?;
    if peer.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on that socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok((stream, peer.pid().and_then(Pid::from_raw)))
}

fn opens(cwd: &Directory, runs: Runs<'_>) -> api::Opens {
    let (argv, ends) = match runs {
        Runs::Shell(shell) => (vec![shell.as_str().to_owned()], api::Ends::Closes),
        Runs::Command { program, args } => {
            let argv = std::iter::once(program.to_owned()).chain(args.iter().cloned()).collect();
            (argv, api::Ends::Holds)
        },
    };
    let program = Some(api::Program { argv });
    api::Opens { cwd: cwd.as_str().to_owned(), program, ends: ends.into() }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Followed {
    Unreached { at: Instant },
    Unmatched,
    Newer(String),
    Emptied { since: Instant },
    Lost(String),
    LetGo,
}

enum Heard {
    Listing(api::Listing),
    Ended(Followed),
}

fn heard(watched: Option<api::Watched>, since: Instant) -> Result<Heard, Failure> {
    use api::watched::ended::Why;
    let Some(watched) = watched else {
        return Err(Failure::Own("lune-mux's watch ended with no word why".to_owned()));
    };
    let ended = match watched.is {
        Some(api::watched::Is::Listing(listing)) => return Ok(Heard::Listing(listing)),
        Some(api::watched::Is::Ended(ended)) => ended,
        None => return Err(Failure::Unread("lune-mux's watch said nothing".to_owned())),
    };
    let followed = match ended.why {
        Some(Why::Emptied(api::watched::Emptied {})) => Followed::Emptied { since },
        Some(Why::Unreached(api::watched::Unreached { why })) => Followed::Lost(why),
        None => {
            let unread = "lune-mux's watch ended, and why is unread".to_owned();
            return Err(Failure::Unread(unread));
        },
    };
    Ok(Heard::Ended(followed))
}

pub fn followed(
    mut told: impl FnMut(Vec<String>, TerminalsListed) -> bool,
) -> Result<Followed, Failure> {
    let at = Instant::now();
    let (runtime, mut streams) = match Server::reached_on(runtime()?)? {
        (runtime, Reached::Matched(matched)) => (runtime, matched.streams()),
        (_, Reached::Unreached) => return Ok(Followed::Unreached { at }),
        (_, Reached::Unmatched(unmatched)) if unmatched.is_newer() => {
            return Ok(Followed::Newer(unmatched.left_be()));
        },
        (runtime, Reached::Unmatched(unmatched)) => {
            stop(&runtime, &unmatched)?;
            return Ok(Followed::Unmatched);
        },
    };
    let since = Instant::now();
    let watched = runtime.block_on(streams.watch());
    let mut stream = watched.map_err(|status| refused(&status))?.into_inner();
    loop {
        let watched = runtime.block_on(stream.message());
        let listing = match heard(watched.map_err(|status| refused(&status))?, since)? {
            Heard::Listing(listing) => listing,
            Heard::Ended(followed) => return Ok(followed),
        };
        let (terminals, mut unread, unsaid) = terminals(listing.terminals);
        let whole = unread.is_empty();
        unread.extend(unsaid);
        if !told(unread, TerminalsListed { terminals, whole, since }) {
            return Ok(Followed::LetGo);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalsListed {
    pub terminals: Vec<Terminal>,
    pub whole: bool,
    pub since: Instant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub key: TerminalKey,
    pub tag: Vec<u8>,
    pub size: Option<Size>,
    pub cwd: String,
    pub title: String,
    pub is: Is,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Is {
    Ended,
    Running { root: u32, front: Option<Foreground>, doing: Option<Doing> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foreground {
    pub pid: u32,
    pub argv: Vec<String>,
}

fn terminals(listed: Vec<api::Terminal>) -> (Vec<Terminal>, Vec<String>, Vec<String>) {
    let count = listed.len();
    let mut terminals: Vec<Terminal> = Vec::with_capacity(listed.len());
    let mut ids = HashSet::with_capacity(listed.len());
    let (mut unread, mut unsaid) = (Vec::new(), Vec::new());
    for terminal in listed {
        match terminal_of(terminal) {
            Ok((read, _)) if !ids.insert(read.key.id) => {
                unread.push(format!("terminal {} is listed twice", read.key.id));
            },
            Ok((read, shell)) => {
                unsaid.extend(shell.map(|why| format!("terminal {}'s shell: {why}", read.key.id)));
                terminals.push(read);
            },
            Err(why) => unread.push(why),
        }
    }
    assert_eq!(terminals.len() + unread.len(), count, "each terminal listed is read or unread");
    (terminals, unread, unsaid)
}

fn terminal_of(terminal: api::Terminal) -> Result<(Terminal, Option<String>), String> {
    let key = TerminalKey::read(terminal.key)?;
    let size = terminal.size.map(|size| Size::of(size.columns, size.rows));
    let size = size.transpose().map_err(|why| format!("terminal {}'s size: {why}", key.id))?;
    let mut unsaid = None;
    let is = match terminal.program {
        None => return Err(format!("terminal {} runs nothing it says", key.id)),
        Some(api::terminal::Program::Ended(_)) => Is::Ended,
        Some(api::terminal::Program::Runs(runs)) => {
            let front = runs.foreground;
            let front = front.map(|front| Foreground { pid: front.pid, argv: front.argv });
            let doing = match runs.ran.as_ref().map(Doing::of_mux).transpose() {
                Ok(doing) => doing,
                Err(why) => {
                    unsaid = Some(why);
                    None
                },
            };
            Is::Running { root: runs.pid, front, doing }
        },
    };
    let (tag, cwd, title) = (terminal.tag, terminal.cwd, terminal.title);
    Ok((Terminal { key, tag, size, cwd, title, is }, unsaid))
}

pub struct Terminals;

impl Terminals {
    pub fn made(
        &self,
        cwd: &Directory,
        runs: Runs<'_>,
        tag: Vec<u8>,
        size: Size,
    ) -> Result<TerminalKey, Failure> {
        let opens = opens(cwd, runs);
        let (columns, rows) = size.get();
        let size = Some(api::Size { columns: columns.into(), rows: rows.into() });
        let request = api::NewTerminalRequest { opens: Some(opens), tag, size };
        let (_, made) = on_started(|server| {
            let request = request.clone();
            server.ask(|mut client| async move { client.new_terminal(request).await })
        })?;
        TerminalKey::read(made.terminal).map_err(Failure::Unread)
    }

    pub fn tagged(&self, tag: &[u8]) -> Result<Option<TerminalKey>, Failure> {
        let found = listed()?.into_iter().find(|terminal| terminal.tag == tag);
        let key = found.map(|terminal| TerminalKey::read(terminal.key));
        key.transpose().map_err(Failure::Unread)
    }

    pub fn tags(&self) -> Result<Vec<(Vec<u8>, TerminalKey)>, Failure> {
        let tags = listed()?.into_iter().map(|terminal| {
            let key = TerminalKey::read(terminal.key).map_err(Failure::Unread)?;
            Ok((terminal.tag, key))
        });
        tags.collect()
    }

    pub fn typed(&self, pastes: &[(TerminalKey, &Line)]) -> Result<(), Failure> {
        use api::from_viewer::Event;
        assert!(!pastes.is_empty(), "a paste is asked for");
        assert!(pastes.len() <= VIEWS_MAX, "no more pastes than terminals");
        let said = |event| api::FromViewer { event: Some(event) };
        let hello = api::Greeting { colours: Some(colours(None, None)), runs_in: None };
        let terminals = pastes.iter().map(|(key, _)| key.said()).collect();
        let mut words = vec![
            said(Event::Hello(hello)),
            said(Event::Showing(api::Showing { terminals })),
        ];
        for (key, line) in pastes {
            assert!(!line.as_str().contains('\n'), "a pasted line holds no newline to run it");
            let paste = api::typed::Event::Paste(api::Paste { text: line.as_str().to_owned() });
            let typed = api::Typed { terminal: Some(key.said()), event: Some(paste) };
            words.push(said(Event::Typed(typed)));
        }
        assert_eq!(words.len(), pastes.len() + 2, "a hello, a showing and one word per paste");
        let refusals_max = 2 * pastes.len();
        let server = reached()?;
        let mut client = server.client.clone();
        let reasons = server.runtime.block_on(async move {
            let deadline = tokio::time::Instant::now() + ANSWER;
            let shown = client.show(tokio_stream::iter(words)).await;
            let mut heard = shown.map_err(|status| refused(&status))?.into_inner();
            let drained = async move {
                let mut reasons = Vec::new();
                while let Some(word) = heard.message().await? {
                    if let Some(api::to_viewer::Event::Refused(refusal)) = word.event {
                        reasons.push(refusal.reason);
                    }
                    if reasons.len() >= refusals_max {
                        break;
                    }
                }
                Ok::<_, Status>(reasons)
            };
            let kept = || Failure::Own("lune-mux kept showing the pastes too long".to_owned());
            let reasons = tokio::time::timeout_at(deadline, drained).await.map_err(|_| kept())?;
            reasons.map_err(|status| refused(&status))
        })?;
        assert!(reasons.len() <= refusals_max, "the drain stops at its most refusals");
        if reasons.is_empty() {
            Ok(())
        } else {
            Err(Failure::Called(Reply { code: Code::Unknown, said: reasons.join("; ") }))
        }
    }

    pub fn sized(&self, key: TerminalKey, size: Size) -> Result<(), Failure> {
        let (columns, rows) = size.get();
        let size = Some(api::Size { columns: columns.into(), rows: rows.into() });
        let request = api::SizeTerminalRequest { terminal: Some(key.said()), size };
        let asked = reached()?.ask(|mut client| async move { client.size_terminal(request).await });
        asked.map(drop).map_err(|status| refused(&status))
    }

    pub fn dumped(&self, key: TerminalKey, dumped: api::Dumped) -> Result<String, Failure> {
        let request = api::DumpRequest { dumped: dumped.into(), terminal: Some(key.said()) };
        let asked = reached()?.ask(|mut client| async move { client.dump(request).await });
        Ok(asked.map_err(|status| refused(&status))?.text)
    }

    pub fn looked(&self, key: TerminalKey) -> Result<Looked, Failure> {
        let request = api::LookRequest { terminal: Some(key.said()) };
        let asked = reached()?.ask(|mut client| async move { client.look(request).await });
        let api::Looked { text, shown } = asked.map_err(|status| refused(&status))?;
        Ok(Looked { text, shown: Shown(shown) })
    }

    pub fn pressed(
        &self,
        key: TerminalKey,
        shown: Shown,
        pressed: Vec<api::pressed::Event>,
    ) -> Result<Pressed, Failure> {
        assert!(!pressed.is_empty(), "something is pressed");
        assert!(pressed.len() <= PRESSES_MAX, "no more presses than an answer takes");
        let pressed = pressed.into_iter().map(|event| api::Pressed { event: Some(event) });
        let request = api::PressRequest {
            terminal: Some(key.said()),
            shown: Some(shown.0),
            pressed: pressed.collect(),
        };
        match reached()?.ask(|mut client| async move { client.press(request).await }) {
            Ok(api::PressResponse {}) => Ok(Pressed::Done),
            Err(status) if status.code() == Code::Aborted => Ok(Pressed::Changed),
            Err(status) => Err(refused(&status)),
        }
    }

    pub fn closed(&self, key: TerminalKey) -> Result<(), Failure> {
        let request = api::CloseTerminalRequest { terminal: Some(key.said()) };
        match reached()?.ask(|mut client| async move { client.close_terminal(request).await }) {
            Ok(_) => Ok(()),
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) => {
                let said = Reply::of(&status);
                match Unchanged::of(&status) {
                    Unchanged::Refused(_) => Err(Failure::Called(said)),
                    Unchanged::InDoubt(_) => {
                        let id = key.id;
                        let called = Failure::Called(said);
                        Err(Failure::Own(format!("{called}, and terminal {id} may close still")))
                    },
                }
            },
        }
    }
}

fn listed() -> Result<Vec<api::Terminal>, Failure> {
    let Some(server) = Server::reached()? else {
        return Ok(Vec::new());
    };
    let listed = server.ask(|mut client| async move { client.list(api::ListRequest {}).await });
    Ok(listed.map_err(|status| refused(&status))?.terminals)
}

fn reached() -> Result<Server, Failure> {
    Server::reached()?.ok_or_else(|| Failure::Own("lune-mux runs no more".to_owned()))
}

fn on_started<T>(
    mut ask: impl FnMut(&Server) -> Result<T, Status>,
) -> Result<(Server, T), Failure> {
    let deadline = Instant::now() + MAKING;
    let mut runtime = runtime()?;
    loop {
        if Instant::now() >= deadline {
            return Err(Failure::Own("no lune-mux answered in time".to_owned()));
        }
        let server = match Server::matched_on(runtime)? {
            Ok(server) => server,
            Err(runtime) => started(runtime, deadline)?,
        };
        match ask(&server) {
            Ok(answer) => return Ok((server, answer)),
            Err(status) if status.code() == Code::Unavailable => {
                gone_by(&server, deadline)?;
                runtime = server.into_runtime();
            },
            Err(status) => return Err(refused(&status)),
        }
    }
}

fn gone_by(server: &Server, deadline: Instant) -> Result<(), Failure> {
    while server.ask(|mut client| async move { client.list(api::ListRequest {}).await }).is_ok() {
        if Instant::now() >= deadline {
            return Err(Failure::Own("the lune-mux that ends did not go in time".to_owned()));
        }
        thread::sleep(LOOK);
    }
    Ok(())
}

fn started(mut runtime: Runtime, deadline: Instant) -> Result<Server, Failure> {
    loop {
        let mut started = Started::spawned()?;
        runtime = match listened(&mut started, runtime, deadline) {
            Ok(Ok(server)) => {
                started.reaped();
                return Ok(server);
            },
            Ok(Err(runtime)) => {
                started.reaped();
                runtime
            },
            Err(why) => return Err(started.given_up(why)),
        };
        let again = Instant::now() + AGAIN;
        while Instant::now() < again {
            runtime = match Server::matched_on(runtime)? {
                Ok(server) => return Ok(server),
                Err(runtime) => runtime,
            };
            thread::sleep(LOOK);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Own("another lune-mux held its folder too long".to_owned()));
        }
    }
}

fn listened(
    started: &mut Started,
    mut runtime: Runtime,
    deadline: Instant,
) -> Result<Result<Server, Runtime>, Failure> {
    loop {
        runtime = match Server::reached_on(runtime)? {
            (runtime, Reached::Matched(matched)) => {
                return Ok(Ok(Server::matched(runtime, matched)));
            },
            (runtime, Reached::Unreached) => runtime,
            (runtime, Reached::Unmatched(unmatched)) => {
                stop(&runtime, &unmatched)?;
                runtime
            },
        };
        let unread = |error| Failure::Own(format!("the lune-mux started is unread: {error}"));
        let ended = started.child.try_wait().map_err(unread)?;
        match ended {
            None => {},
            Some(status) if status.code() == Some(i32::from(ANOTHER_RUNS)) => {
                return Ok(Err(runtime));
            },
            Some(status) => {
                return Err(Failure::Own(format!("lune-mux ended as it started, with {status}")));
            },
        }
        if Instant::now() >= deadline {
            return Err(Failure::Own("lune-mux did not listen in time".to_owned()));
        }
        thread::sleep(LOOK);
    }
}

struct Started {
    child: Child,
    reaper: SyncSender<Child>,
}

impl Started {
    fn spawned() -> Result<Started, Failure> {
        let folder = listened_in()?;
        let (reaper, handed) = mpsc::sync_channel::<Child>(1);
        let reaping = move || {
            if let Ok(mut child) = handed.recv()
                && let Err(error) = child.wait()
            {
                log::say(&format!("lune-mux was not reaped: {error}"));
            }
        };
        let thread = thread::Builder::new().name("lune-mux".to_owned()).spawn(reaping);
        thread.map_err(|error| Failure::Own(format!("no thread to reap lune-mux: {error}")))?;
        let program = program();
        let mut command = Command::new(&program);
        command.arg(&folder);
        in_no_terminal(&mut command);
        command.current_dir("/").stdin(Stdio::null()).stdout(Stdio::null());
        let spawned = command.spawn();
        let unstarted = |error| Failure::Own(format!("{}: {error}", program.display()));
        let child = spawned.map_err(unstarted)?;
        STARTED.store(Pid::from_child(&child).as_raw_pid(), Ordering::Relaxed);
        Ok(Started { child, reaper })
    }

    fn reaped(self) {
        self.reaper.send(self.child).expect("the thread waits for the server to reap");
    }

    fn given_up(mut self, why: Failure) -> Failure {
        if let Err(error) = self.child.kill() {
            log::say(&format!("luned gave up, as {why}, and did not kill it: {error}"));
        }
        self.reaped();
        why
    }
}

fn program() -> PathBuf {
    let beside = env::current_exe()
        .and_then(fs::canonicalize)
        .ok()
        .and_then(|this| Some(this.parent()?.join(PROGRAM)))
        .filter(|program| Path::is_file(program));
    beside.unwrap_or_else(|| PathBuf::from(PROGRAM))
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use lune_protocol::shell::Front;

    use super::*;

    #[test]
    fn only_a_watch_that_says_it_emptied_is_heard_as_emptied() {
        use api::watched::{Emptied, Ended, Is, Unreached, ended::Why};
        let since = Instant::now();
        let ended = |why| Some(api::Watched { is: Some(Is::Ended(Ended { why })) });
        let emptied = heard(ended(Some(Why::Emptied(Emptied {}))), since);
        assert!(matches!(emptied, Ok(Heard::Ended(Followed::Emptied { since: at })) if at == since));
        let why = "/run/lune/mux.sock is gone".to_owned();
        let lost = heard(ended(Some(Why::Unreached(Unreached { why: why.clone() }))), since);
        assert!(matches!(lost, Ok(Heard::Ended(Followed::Lost(said))) if said == why));
        assert!(matches!(heard(None, since), Err(Failure::Own(_))), "a watch that just ends");
        assert!(matches!(heard(ended(None), since), Err(Failure::Unread(_))));
        let silent = heard(Some(api::Watched { is: None }), since);
        assert!(matches!(silent, Err(Failure::Unread(_))));
    }

    #[test]
    fn a_shell_listed_that_does_not_read_is_left_out_alone() {
        use api::ran::{Going, Is as Said, Ready, Stopped};
        let since = Some(prost_types::Timestamp { seconds: 90, nanos: 0 });
        let working = api::Front::Working.into();
        let unsaid = Going { command: None, since, front: working };
        let said = |is| Some(api::Ran { is: Some(is) });
        let shells = [
            said(Said::Ready(Ready {})),
            None,
            said(Said::Going(Going { front: api::Front::Unspecified.into(), ..unsaid.clone() })),
            said(Said::Going(Going { command: Some(String::new()), ..unsaid.clone() })),
            said(Said::Stopped(Stopped { command: Some("vim".to_owned()), at: None })),
            Some(api::Ran { is: None }),
            said(Said::Going(unsaid)),
        ];
        let made_at = Some(prost_types::Timestamp { seconds: 60, nanos: 0 });
        let listed = (1..).zip(shells).map(|(id, ran)| api::Terminal {
            key: Some(api::TerminalKey { id, made_at }),
            program: Some(api::terminal::Program::Runs(api::Running {
                pid: 70,
                foreground: None,
                ran,
            })),
            ..api::Terminal::default()
        });
        let (terminals, unread, unsaid) = super::terminals(listed.collect());
        assert_eq!((terminals.len(), unread.len(), unsaid.len()), (7, 0, 4), "{unsaid:?}");
        let doings: Vec<(u32, Option<&Doing>)> = terminals
            .iter()
            .map(|terminal| match &terminal.is {
                Is::Running { doing, .. } => (terminal.key.id.get(), doing.as_ref()),
                Is::Ended => panic!("terminal {} runs", terminal.key.id),
            })
            .collect();
        let going = Doing::Going {
            command: None,
            since: SystemTime::UNIX_EPOCH + Duration::from_secs(90),
            front: Front::Working,
        };
        let unread = [(3, None), (4, None), (5, None), (6, None)];
        let read = [(1, Some(&Doing::Ready)), (2, None)];
        assert_eq!(doings, [&read[..], &unread[..], &[(7, Some(&going))]].concat());
    }

    #[test]
    fn a_terminal_listed_twice_or_with_no_key_is_left_out() {
        let made_at = Some(prost_types::Timestamp { seconds: 60, nanos: 0 });
        let ended = Some(api::terminal::Program::Ended(api::Exit::default()));
        let terminal = |id| api::Terminal {
            key: Some(api::TerminalKey { id, made_at }),
            program: ended.clone(),
            ..api::Terminal::default()
        };
        let listed = vec![
            terminal(1),
            terminal(1),
            api::Terminal { key: None, ..terminal(2) },
            api::Terminal { program: None, ..terminal(3) },
            terminal(4),
        ];
        let (terminals, unread, unsaid) = super::terminals(listed);
        let ids: Vec<u32> = terminals.iter().map(|terminal| terminal.key.id.get()).collect();
        assert_eq!(ids, [1, 4]);
        assert_eq!((unread.len(), unsaid.len()), (3, 0), "{unread:?}");
    }
}
