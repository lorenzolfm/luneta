use std::collections::HashSet;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use std::{env, fs};

use hyper_util::rt::TokioIo;
use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;
use tokio::net::UnixStream;
use tokio::runtime::{self, Runtime};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Response, Status, Streaming};
use tower::service_fn;

use super::api::{self, mux_client::MuxClient};
use super::frames::colours;
use super::TerminalKey;
use crate::mux::{in_no_pane, Failure, Runs};
use crate::projects::{Line, Size, TERMINALS_MAX};
use crate::shells::{Doing, Front};
use crate::{folder, log, Directory};

pub const SOCKET: &str = "mux.sock";

const PROGRAM: &str = "luneta-mux";

pub const ANOTHER_RUNS: u8 = 3;

const CONNECT: Duration = Duration::from_millis(500);

const ANSWER: Duration = Duration::from_millis(1500);

const WAITS_PER_ANSWER_MAX: u128 = 2;

const _: () = assert!(
    WAITS_PER_ANSWER_MAX * (CONNECT.as_millis() + ANSWER.as_millis())
        < crate::client::ANSWER.as_millis()
);

const MAKING: Duration = Duration::from_secs(10);

const LOOK: Duration = Duration::from_millis(10);

const AGAIN: Duration = Duration::from_millis(100);

struct Server {
    runtime: Runtime,
    client: MuxClient<Channel>,
}

impl Server {
    fn reached() -> Result<Option<Server>, Failure> {
        Ok(Server::reached_on(runtime()?, Some(ANSWER))?.ok())
    }

    fn reached_on(
        runtime: Runtime,
        answer: Option<Duration>,
    ) -> Result<Result<Server, Runtime>, Failure> {
        let socket = socket()?;
        match runtime.block_on(connect(&socket, answer)) {
            Ok(Some(client)) => Ok(Ok(Server { runtime, client })),
            Ok(None) => Ok(Err(runtime)),
            Err(error) => Err(Failure::Own(format!("{}: {error}", socket.display()))),
        }
    }

    fn into_runtime(self) -> Runtime {
        let Server { runtime, client } = self;
        drop(client);
        runtime
    }

    fn ask<T, F>(&self, asking: impl FnOnce(MuxClient<Channel>) -> F) -> Result<T, Status>
    where
        F: Future<Output = Result<Response<T>, Status>>,
    {
        self.runtime.block_on(asking(self.client.clone())).map(Response::into_inner)
    }

}

pub(super) fn runtime() -> Result<Runtime, Failure> {
    let runtime = runtime::Builder::new_current_thread().enable_all().build();
    runtime.map_err(|error| Failure::Own(format!("no runtime: {error}")))
}

pub(super) fn refused(status: &Status) -> Failure {
    match status.message() {
        "" => Failure::Own(status.code().description().to_owned()),
        said => Failure::Own(said.to_owned()),
    }
}

pub fn socket() -> Result<PathBuf, Failure> {
    let unset = || Failure::Own("neither XDG_RUNTIME_DIR nor TMPDIR is set".to_owned());
    Ok(folder::find().ok_or_else(unset)?.join(SOCKET))
}

pub(super) async fn connect(
    socket: &Path,
    answer: Option<Duration>,
) -> io::Result<Option<MuxClient<Channel>>> {
    let first = match UnixStream::connect(socket).await {
        Ok(stream) => mine(stream)?,
        Err(error)
            if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused) =>
        {
            return Ok(None);
        },
        Err(error) => return Err(error),
    };
    let first = Arc::new(Mutex::new(Some(first)));
    let mut endpoint = Endpoint::from_static("http://luneta-mux").connect_timeout(CONNECT);
    if let Some(answer) = answer {
        endpoint = endpoint.timeout(answer);
    }
    let channel = endpoint
        .connect_with_connector(service_fn(move |_: Uri| {
            let first = first.lock().unwrap_or_else(PoisonError::into_inner).take();
            async move {
                let gone = || io::Error::new(ErrorKind::NotConnected, "the server reached went");
                io::Result::Ok(TokioIo::new(first.ok_or_else(gone)?))
            }
        }))
        .await
        .map_err(io::Error::other)?;
    Ok(Some(MuxClient::new(channel)))
}

fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on luneta-mux's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}

fn opens(cwd: &Directory, runs: Runs<'_>) -> Result<api::Opens, Failure> {
    let program = |argv: Vec<String>| api::opens::Runs::Program(api::Program { argv });
    let (runs, ends) = match runs {
        Runs::Shell(shell) => {
            assert!(!shell.is_empty());
            let unread = || Failure::Own(format!("the shell {} is not UTF-8", shell.display()));
            (program(vec![shell.to_str().ok_or_else(unread)?.to_owned()]), api::Ends::Closes)
        },
        Runs::ZellijShell => (api::opens::Runs::Shell(api::Shell {}), api::Ends::Closes),
        Runs::Command { program: first, args } => {
            let argv = std::iter::once(first.to_owned()).chain(args.iter().cloned()).collect();
            (program(argv), api::Ends::Holds)
        },
    };
    Ok(api::Opens { cwd: cwd.as_str().to_owned(), runs: Some(runs), ends: ends.into() })
}

fn doing(is: &api::ran::Is) -> Result<Doing, String> {
    use api::ran::{ended, Is};
    let time = |at: Option<prost_types::Timestamp>, what: &str| {
        let at = at.ok_or_else(|| format!("no {what}"))?;
        SystemTime::try_from(at).map_err(|why| format!("its {what}: {why}"))
    };
    let said = |command: &Option<String>| match command {
        None => Ok(None),
        Some(command) => Said::new(command.clone()).map(Some).ok_or("an empty command line"),
    };
    let doing = match is {
        Is::Ready(_) => Doing::Ready,
        Is::Going(going) => {
            let front = match going.front() {
                api::Front::Working => Front::Working,
                api::Front::Asking => Front::Asking,
                api::Front::Interactive => Front::Interactive,
                api::Front::Unspecified => {
                    return Err("what its command does, in a word of a newer luneta-mux's".into());
                },
            };
            let since = time(going.since, "since")?;
            Doing::Going { command: said(&going.command)?, since, front }
        },
        Is::Ended(ended) => {
            let at = time(ended.at, "at")?;
            let ending = ended.ending.as_ref().map(|ending| match ending {
                ended::Ending::Status(status) => Ending::Exited(*status),
                ended::Ending::Resumed(_) => Ending::Resumed,
            });
            let took = ended.took.ok_or("no took")?;
            let took = Duration::try_from(took).map_err(|why| format!("its took: {why}"))?;
            Doing::Ended { command: said(&ended.command)?, at, ending, took }
        },
        Is::Stopped(stopped) => {
            let at = time(stopped.at, "at")?;
            Doing::Stopped { command: said(&stopped.command)?, at }
        },
    };
    Ok(doing)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Followed {
    Unreached { at: Instant },
    Emptied { since: Instant },
    LetGo,
}

pub fn followed(
    mut told: impl FnMut(Vec<String>, TerminalsListed) -> bool,
) -> Result<Followed, Failure> {
    let at = Instant::now();
    let Ok(server) = Server::reached_on(runtime()?, None)? else {
        return Ok(Followed::Unreached { at });
    };
    let since = Instant::now();
    let watched = server.ask(|mut client| async move { client.watch(api::WatchRequest {}).await });
    let mut stream: Streaming<api::Listing> = watched.map_err(|status| refused(&status))?;
    loop {
        let heard = server.runtime.block_on(stream.message());
        let Some(listing) = heard.map_err(|status| refused(&status))? else {
            return Ok(Followed::Emptied { since });
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
    Running { root: u32, front: Option<Foreground>, word: Option<Word>, doing: Option<Doing> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Foreground {
    pub pid: u32,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Word {
    Prompts,
    Going(Option<String>),
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
    use api::ran::Is as Said;
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
            let said = runs.ran.and_then(|ran| ran.is);
            let doing = match said.as_ref().map(doing).transpose() {
                Ok(doing) => doing,
                Err(why) => {
                    unsaid = Some(why);
                    None
                },
            };
            let word = said.map(|is| match is {
                Said::Ready(_) | Said::Ended(_) | Said::Stopped(_) => Word::Prompts,
                Said::Going(going) => Word::Going(going.command),
            });
            Is::Running { root: runs.pid, front, word, doing }
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
        let opens = opens(cwd, runs)?;
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
        assert!(pastes.len() <= TERMINALS_MAX, "no more pastes than terminals");
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
        let drained = server.runtime.block_on(async move {
            let shown = async move {
                let mut heard = client.show(tokio_stream::iter(words)).await?.into_inner();
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
            tokio::time::timeout(ANSWER, shown).await
        });
        match drained {
            Ok(Ok(reasons)) if reasons.is_empty() => Ok(()),
            Ok(Ok(reasons)) => Err(Failure::Own(reasons.join("; "))),
            Ok(Err(status)) => Err(refused(&status)),
            Err(_) => Err(Failure::Own("luneta-mux kept showing the pastes too long".to_owned())),
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

    pub fn closed(&self, key: TerminalKey) -> Result<(), Failure> {
        let request = api::CloseTerminalRequest { terminal: Some(key.said()) };
        match reached()?.ask(|mut client| async move { client.close_terminal(request).await }) {
            Err(status) if status.code() == Code::NotFound => Ok(()),
            Err(status) if matches!(status.code(), Code::Cancelled | Code::DeadlineExceeded) => {
                let unanswered = format!("luneta-mux did not answer in time ({})", status.message());
                let id = key.id;
                Err(Failure::Own(format!("{unanswered}, and terminal {id} may close still")))
            },
            asked => asked.map(drop).map_err(|status| refused(&status)),
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
    Server::reached()?.ok_or_else(|| Failure::Own("luneta-mux runs no more".to_owned()))
}

fn on_started<T>(
    mut ask: impl FnMut(&Server) -> Result<T, Status>,
) -> Result<(Server, T), Failure> {
    let deadline = Instant::now() + MAKING;
    let mut runtime = runtime()?;
    loop {
        if Instant::now() >= deadline {
            return Err(Failure::Own("no server answered in time".to_owned()));
        }
        let server = match Server::reached_on(runtime, Some(ANSWER))? {
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
            return Err(Failure::Own("the server that ends did not go in time".to_owned()));
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
            runtime = match Server::reached_on(runtime, Some(ANSWER))? {
                Ok(server) => return Ok(server),
                Err(runtime) => runtime,
            };
            thread::sleep(LOOK);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Own("another held its folder for too long".to_owned()));
        }
    }
}

fn listened(
    started: &mut Started,
    mut runtime: Runtime,
    deadline: Instant,
) -> Result<Result<Server, Runtime>, Failure> {
    loop {
        runtime = match Server::reached_on(runtime, Some(ANSWER))? {
            Ok(server) => return Ok(Ok(server)),
            Err(runtime) => runtime,
        };
        let ended = started.child.try_wait().map_err(|error| Failure::Own(error.to_string()))?;
        match ended {
            None => {},
            Some(status) if status.code() == Some(i32::from(ANOTHER_RUNS)) => {
                return Ok(Err(runtime));
            },
            Some(status) => {
                return Err(Failure::Own(format!("it ended as it started, with {status}")));
            },
        }
        if Instant::now() >= deadline {
            return Err(Failure::Own("it did not listen in time".to_owned()));
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
        let (reaper, handed) = mpsc::sync_channel::<Child>(1);
        let reaping = move || {
            if let Ok(mut child) = handed.recv()
                && let Err(error) = child.wait()
            {
                log::say(&format!("luneta-mux was not reaped: {error}"));
            }
        };
        let thread = thread::Builder::new().name("luneta-mux".to_owned()).spawn(reaping);
        thread.map_err(|error| Failure::Own(format!("no thread to reap it: {error}")))?;
        let program = program();
        let mut command = Command::new(&program);
        in_no_pane(&mut command);
        command.current_dir("/").stdin(Stdio::null()).stdout(Stdio::null());
        let spawned = command.spawn();
        let unstarted = |error| Failure::Own(format!("{}: {error}", program.display()));
        Ok(Started { child: spawned.map_err(unstarted)?, reaper })
    }

    fn reaped(self) {
        self.reaper.send(self.child).expect("the thread waits for the server to reap");
    }

    fn given_up(mut self, why: Failure) -> Failure {
        if let Err(error) = self.child.kill() {
            log::say(&format!("luneta-mux, given up on as {why}, was not killed: {error}"));
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
    use super::*;

    #[test]
    fn a_shell_listed_that_does_not_read_is_left_out_alone() {
        use api::ran::{Going, Is as Said, Ready, Stopped};
        let since = Some(prost_types::Timestamp { seconds: 90, nanos: 0 });
        let working = api::Front::Working.into();
        let unsaid = Going { command: None, since, front: working };
        let shells = [
            Some(Said::Ready(Ready {})),
            None,
            Some(Said::Going(Going { front: api::Front::Unspecified.into(), ..unsaid.clone() })),
            Some(Said::Going(Going { command: Some(String::new()), ..unsaid.clone() })),
            Some(Said::Stopped(Stopped { command: Some("vim".to_owned()), at: None })),
            Some(Said::Going(unsaid)),
        ];
        let made_at = Some(prost_types::Timestamp { seconds: 60, nanos: 0 });
        let listed = (1..).zip(shells).map(|(id, is)| api::Terminal {
            key: Some(api::TerminalKey { id, made_at }),
            program: Some(api::terminal::Program::Runs(api::Running {
                pid: 70,
                foreground: None,
                ran: is.map(|is| api::Ran { is: Some(is) }),
            })),
            ..api::Terminal::default()
        });
        let (terminals, unread, unsaid) = super::terminals(listed.collect());
        assert_eq!((terminals.len(), unread.len(), unsaid.len()), (6, 0, 3), "{unsaid:?}");
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
        let unread = [(3, None), (4, None), (5, None)];
        let read = [(1, Some(&Doing::Ready)), (2, None)];
        assert_eq!(doings, [&read[..], &unread[..], &[(6, Some(&going))]].concat());
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
