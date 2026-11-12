//! `luneta-mux`, the server of every own session of the user, as luneta and
//! its daemon reach it: on its socket in luneta's runtime folder, and started
//! there, as a session is made, where none listens.
//!
//! A server ends with its last session, and 30 s after it starts without one.
//! So it is started only to make a session, which is made at once. One that
//! is ending says so, and one that holds the folder still keeps the next from
//! starting: either way, luneta waits for it to go, and starts another.

use std::collections::HashSet;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use hyper_util::rt::TokioIo;
use luneta_protocol::said::Said;
use tokio::net::UnixStream;
use tokio::runtime::{self, Runtime};
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Response, Status, Streaming};
use tower::service_fn;

use super::api::{self, mux_client::MuxClient};
use super::OwnId;
use crate::folder;
use crate::mux::{in_no_pane, Client, Failure, Laid, Listed, PaneId, Runs, SessionName, TabId};
use crate::Directory;

/// The socket the server listens on, in luneta's runtime folder.
pub const SOCKET: &str = "mux.sock";

/// The server's program, as the same build installs it beside luneta's.
const PROGRAM: &str = "luneta-mux";

/// How the server ends where another holds its folder.
const ANOTHER_RUNS: i32 = 3;

/// How long a client waits for the socket to take it: a server on the same
/// machine takes it at once, or not at all.
const CONNECT: Duration = Duration::from_secs(1);

/// How long a client waits for an answer, as it waits for the daemon's.
const ANSWER: Duration = Duration::from_secs(5);

/// How long a session waits to be made, from the first word to the server:
/// for one that ends to go, and another to start and listen.
const MAKING: Duration = Duration::from_secs(10);

/// How often luneta looks meanwhile, for the socket to listen, or the server
/// it started to end.
const LOOK: Duration = Duration::from_millis(10);

/// How long luneta waits for a server that holds the folder to listen, or to
/// go, before it starts another: one that ends waits up to 5 s for its
/// clients first, and a start each `LOOK` meanwhile would be hundreds.
const AGAIN: Duration = Duration::from_millis(100);

/// The server that listens, reached.
struct Server {
    runtime: Runtime,
    client: MuxClient<Channel>,
}

impl Server {
    /// The server that listens in luneta's runtime folder, or none where none
    /// does.
    fn reached() -> Result<Option<Server>, Failure> {
        Server::reached_with(Some(ANSWER))
    }

    /// `reached`, with questions that wait `answer` at most, or as long as
    /// the server takes.
    fn reached_with(answer: Option<Duration>) -> Result<Option<Server>, Failure> {
        let socket = socket()?;
        let runtime = runtime::Builder::new_current_thread().enable_all().build();
        let runtime = runtime.map_err(|error| Failure::Own(format!("no runtime: {error}")))?;
        match runtime.block_on(connect(socket.clone(), answer)) {
            Ok(Some(client)) => Ok(Some(Server { runtime, client })),
            Ok(None) => Ok(None),
            Err(error) => Err(Failure::Own(format!("{}: {error}", socket.display()))),
        }
    }

    /// What the server answers `asking`, or why it did not.
    fn ask<T, F>(&self, asking: impl FnOnce(MuxClient<Channel>) -> F) -> Result<T, Status>
    where
        F: Future<Output = Result<Response<T>, Status>>,
    {
        self.runtime.block_on(asking(self.client.clone())).map(Response::into_inner)
    }

    /// The sessions as the server lists them now.
    fn listing(&self) -> Result<Vec<api::Session>, Failure> {
        let listing = self.ask(|mut client| async move { client.list(api::ListRequest {}).await });
        Ok(listing.map_err(|status| refused(&status))?.sessions)
    }

    /// The session `id`, as the server lists it now, or why not.
    fn session(&self, id: OwnId) -> Result<api::Session, Failure> {
        let listed = self.listing()?.into_iter().find(|session| session.id == id.get());
        listed.ok_or_else(|| gone(id))
    }
}

/// Why luneta-mux did not answer, as it said it, or the code it answered with
/// where it said nothing.
fn refused(status: &Status) -> Failure {
    match status.message() {
        "" => Failure::Own(status.code().description().to_owned()),
        said => Failure::Own(said.to_owned()),
    }
}

/// Why the own session `id` has nothing to do: it ended.
fn gone(id: OwnId) -> Failure {
    Failure::Own(format!("own session {id} no longer runs"))
}

/// The server that runs, or why not: where none runs, the own session `id`
/// ended with the last one.
fn running(id: OwnId) -> Result<Server, Failure> {
    Server::reached()?.ok_or_else(|| gone(id))
}

/// Where the server listens, by this process's variables.
pub fn socket() -> Result<PathBuf, Failure> {
    let unset = || Failure::Own("neither XDG_RUNTIME_DIR nor TMPDIR is set".to_owned());
    Ok(folder::find().ok_or_else(unset)?.join(SOCKET))
}

/// A client of the server that listens on `socket`, whose questions wait
/// `answer` at most, or none where none listens there: no socket, or one a
/// server left as it ended without taking it away.
async fn connect(
    socket: PathBuf,
    answer: Option<Duration>,
) -> io::Result<Option<MuxClient<Channel>>> {
    let first = match UnixStream::connect(&socket).await {
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
            let socket = socket.clone();
            async move {
                let stream = match first {
                    Some(stream) => stream,
                    None => mine(UnixStream::connect(socket).await?)?,
                };
                io::Result::Ok(TokioIo::new(stream))
            }
        }))
        .await
        .map_err(io::Error::other)?;
    Ok(Some(MuxClient::new(channel)))
}

/// `stream`, if this user's process is at its other end, as the kernel says.
fn mine(stream: UnixStream) -> io::Result<UnixStream> {
    if stream.peer_cred()?.uid() != rustix::process::getuid().as_raw() {
        let refusal = "another user's process listens on luneta-mux's socket";
        return Err(io::Error::new(ErrorKind::PermissionDenied, refusal));
    }
    Ok(stream)
}

/// What a session's tab runs, as `Runs` says, and where.
fn opens(cwd: &Directory, runs: Runs<'_>) -> Result<api::Opens, Failure> {
    let program = |argv: Vec<String>| api::opens::Runs::Program(api::Program { argv });
    let (runs, ends) = match runs {
        Runs::Shell(shell) => {
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

/// The panes of `session`, as luneta lays a session's panes out: each tab
/// holds one, focused, tiled and on the screen, and there are no plugins.
fn laid(session: &api::Session) -> Vec<Laid> {
    let tabs = session.tabs.iter().enumerate();
    let laid = tabs.flat_map(|(position, tab)| {
        let position = u32::try_from(position).expect("a message holds fewer than 2^32 tabs");
        tab.panes.iter().map(move |pane| Laid {
            pane: PaneId::Terminal(pane.id),
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: true,
            is_fullscreen: false,
            is_held: matches!(pane.program, Some(api::pane::Program::Ended(_))),
            title: pane.title.clone(),
            tab_id: TabId(tab.id),
            tab_position: position,
            tab_name: tab.name.clone(),
            cwd: Said::new(pane.cwd.clone()),
            command: None,
        })
    });
    laid.collect()
}

/// The clients of `session`, each on its pane, or why they do not read: a
/// client's id past what luneta's clients are numbered by, or on two panes.
fn clients(session: &api::Session) -> Result<Listed, String> {
    let panes = session.tabs.iter().flat_map(|tab| &tab.panes);
    let on = |pane: &api::Pane| {
        let client = move |client: &api::Client| {
            let past = |_| format!("client {} is past 2^32", client.id);
            let id = u32::try_from(client.id).map_err(past)?;
            Ok(Client { id, pane: PaneId::Terminal(pane.id) })
        };
        pane.clients.iter().map(client).collect::<Vec<Result<Client, String>>>()
    };
    let clients: Vec<Client> = panes.flat_map(on).collect::<Result<_, String>>()?;
    let mut ids = HashSet::new();
    if let Some(twice) = clients.iter().find(|client| !ids.insert(client.id)) {
        return Err(format!("client {} comes twice", twice.id));
    }
    Ok(Listed::of(clients))
}

/// An own session, as its server lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub id: OwnId,
    pub name: SessionName,
    /// Its panes, as luneta lays a session's out.
    pub laid: Vec<Laid>,
    pub clients: Listed,
}

/// The own sessions among `sessions`, as the server lists them, and why each
/// that it leaves out does not read: one whose id or name another before it
/// has is left out too, as the server gives each its own.
fn read(sessions: Vec<api::Session>) -> (Vec<Listing>, Vec<String>) {
    let mut listed: Vec<Listing> = Vec::with_capacity(sessions.len());
    let mut unread = Vec::new();
    for session in sessions {
        match one(&session, &listed) {
            Ok(one) => listed.push(one),
            Err(why) => unread.push(why),
        }
    }
    (listed, unread)
}

/// `session`, as the server lists it after `before`, or why it does not read.
fn one(session: &api::Session, before: &[Listing]) -> Result<Listing, String> {
    let id = OwnId::new(session.id).ok_or("a session of id 0")?;
    let name = SessionName::running(&session.name)
        .map_err(|why| format!("own session {id}'s name: {why}"))?;
    if before.iter().any(|listed| listed.id == id || listed.name == name) {
        return Err(format!("own session {id}, {name}, has the id or the name of another"));
    }
    let clients = clients(session).map_err(|why| format!("own session {id}'s clients: {why}"))?;
    Ok(Listing { id, name, laid: laid(session), clients })
}

/// Follows the own sessions: has `told` hear them as the server lists them
/// now, and each time they change, with why each it left out does not read,
/// until the server ends, or `told` says to stop, which it says as `false`.
/// None where no server listens.
pub fn followed(
    mut told: impl FnMut(Vec<Listing>, Vec<String>) -> bool,
) -> Result<Option<()>, Failure> {
    let Some(server) = Server::reached_with(None)? else {
        return Ok(None);
    };
    let watched = server.ask(|mut client| async move { client.watch(api::WatchRequest {}).await });
    let mut stream: Streaming<api::Listing> = watched.map_err(|status| refused(&status))?;
    loop {
        let heard = server.runtime.block_on(stream.message());
        let Some(listing) = heard.map_err(|status| refused(&status))? else {
            return Ok(Some(()));
        };
        let (listed, unread) = read(listing.sessions);
        if !told(listed, unread) {
            return Ok(Some(()));
        }
    }
}

/// The own session named `name`, where one runs by it.
pub fn named(name: &SessionName) -> Result<Option<OwnId>, Failure> {
    let Some(server) = Server::reached()? else {
        return Ok(None);
    };
    let listing = server.listing()?;
    let listed = listing.iter().find(|session| session.name == name.as_str());
    Ok(listed.and_then(|session| OwnId::new(session.id)))
}

/// The names of the own sessions that run: none where no server does.
pub fn names() -> Result<Vec<String>, Failure> {
    let Some(server) = Server::reached()? else {
        return Ok(Vec::new());
    };
    Ok(server.listing()?.into_iter().map(|session| session.name).collect())
}

/// The panes of the session `id`, as luneta lays them out.
pub fn laid_of(id: OwnId) -> Result<Vec<Laid>, Failure> {
    Ok(laid(&running(id)?.session(id)?))
}

/// The clients of the session `id`.
pub fn clients_of(id: OwnId) -> Result<Listed, Failure> {
    clients(&running(id)?.session(id)?).map_err(Failure::Unread)
}

/// Opens a tab at the end of the session `id`, in `cwd`, named `name` or as
/// the server numbers it, that runs `runs`, and says its id.
pub fn tab_opened(
    id: OwnId,
    cwd: &Directory,
    name: Option<&str>,
    runs: Runs<'_>,
) -> Result<TabId, Failure> {
    let request = api::NewTabRequest {
        session: id.get(),
        name: name.map(str::to_owned),
        opens: Some(opens(cwd, runs)?),
    };
    let opened = running(id)?.ask(|mut client| async move { client.new_tab(request).await });
    Ok(TabId(opened.map_err(|status| refused(&status))?.tab))
}

/// Opens a tab at the end of the session `id`, where its current tab works,
/// named `name` or as the server numbers it, that runs `runs` and closes as
/// it ends, and says its id.
pub fn tab_opened_once_heard(
    id: OwnId,
    name: Option<&str>,
    runs: &[&str],
) -> Result<TabId, Failure> {
    let server = running(id)?;
    let session = server.session(id)?;
    let current = session.tabs.iter().find(|tab| tab.id == session.current_tab);
    let pane = current.and_then(|tab| tab.panes.first());
    let unknown = || Failure::Own(format!("own session {id} lists no current tab"));
    let cwd = pane.ok_or_else(unknown)?.cwd.clone();
    let argv = runs.iter().map(|word| (*word).to_owned()).collect();
    let runs = api::opens::Runs::Program(api::Program { argv });
    let opens = api::Opens { cwd, runs: Some(runs), ends: api::Ends::Closes.into() };
    let request =
        api::NewTabRequest { session: id.get(), name: name.map(str::to_owned), opens: Some(opens) };
    let opened = server.ask(|mut client| async move { client.new_tab(request).await });
    Ok(TabId(opened.map_err(|status| refused(&status))?.tab))
}

/// Closes the tab `tab` of the session `id`, which hangs up what runs in it.
pub fn tab_closed(id: OwnId, tab: TabId) -> Result<(), Failure> {
    let request = api::CloseTabRequest { tab: tab.0 };
    let closed = running(id)?.ask(|mut client| async move { client.close_tab(request).await });
    closed.map(drop).map_err(|status| refused(&status))
}

/// Renames the session `id` to `to`, which the server holds unique among its
/// sessions alone.
pub fn renamed(id: OwnId, to: &SessionName) -> Result<(), Failure> {
    let request = api::RenameRequest { session: id.get(), name: to.as_str().to_owned() };
    let renamed = running(id)?.ask(|mut client| async move { client.rename(request).await });
    renamed.map(drop).map_err(|status| refused(&status))
}

/// What the pane `pane` of the session `id` shows, or all it was written, as
/// `dumped` says.
pub fn dumped(id: OwnId, pane: u32, dumped: api::Dumped) -> Result<String, Failure> {
    let request = api::DumpRequest { pane, dumped: dumped.into() };
    let dump = running(id)?.ask(|mut client| async move { client.dump(request).await });
    Ok(dump.map_err(|status| refused(&status))?.text)
}

/// Ends the session `id`, which hangs up what runs in each of its tabs.
pub fn killed(id: OwnId) -> Result<(), Failure> {
    let request = api::KillRequest { session: id.get() };
    let killed = running(id)?.ask(|mut client| async move { client.kill(request).await });
    killed.map(drop).map_err(|status| refused(&status))
}

/// Where a client goes among the own sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A session, on the tab a client coming there goes to.
    Session(OwnId),
    /// The tab of a session that holds a pane.
    Pane { session: OwnId, pane: u32 },
}

/// Takes the client that last typed in the session `at` to `to`.
pub fn went(at: OwnId, to: Place) -> Result<(), Failure> {
    let server = running(at)?;
    let is = match to {
        Place::Session(session) => api::place::Is::Session(session.get()),
        Place::Pane { session, pane } => {
            let tabs = server.session(session)?.tabs;
            let tab = tabs.iter().find(|tab| tab.panes.iter().any(|it| it.id == pane));
            let gone = || Failure::Own(format!("own session {session} has no pane {pane}"));
            api::place::Is::Tab(tab.ok_or_else(gone)?.id)
        },
    };
    let request = api::GoRequest {
        who: Some(api::go_request::Who::LastTypedIn(at.get())),
        to: Some(api::Place { is: Some(is) }),
    };
    let went = server.ask(|mut client| async move { client.go(request).await });
    went.map(drop).map_err(|status| refused(&status))
}

/// An own session made: its id, and its first tab's and pane's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Made {
    pub session: OwnId,
    pub tab: TabId,
    pub pane: u32,
}

/// Makes an own session named `name` of one tab, in `cwd`, that runs `runs`,
/// and starts the server for it where none listens; or says why not. A
/// server that ends as it is asked is waited for to go, and another started.
pub fn made(name: &SessionName, cwd: &Directory, runs: Runs<'_>) -> Result<Made, Failure> {
    let deadline = Instant::now() + MAKING;
    let request = api::NewSessionRequest {
        name: name.as_str().to_owned(),
        opens: Some(opens(cwd, runs)?),
    };
    loop {
        let server = match Server::reached()? {
            Some(server) => server,
            None => started(deadline)?,
        };
        let request = request.clone();
        match server.ask(|mut client| async move { client.new_session(request).await }) {
            Ok(made) => {
                let session = OwnId::new(made.session);
                let unread = || Failure::Unread("luneta-mux made a session of id 0".to_owned());
                let session = session.ok_or_else(unread)?;
                return Ok(Made { session, tab: TabId(made.tab), pane: made.pane });
            },
            Err(status) if status.code() == Code::Unavailable => gone_by(&server, deadline)?,
            Err(status) => return Err(refused(&status)),
        }
    }
}

/// Waits for `server`, which ends, to go, until `deadline`.
fn gone_by(server: &Server, deadline: Instant) -> Result<(), Failure> {
    while server.listing().is_ok() {
        if Instant::now() >= deadline {
            return Err(Failure::Own("the server that ends did not go in time".to_owned()));
        }
        thread::sleep(LOOK);
    }
    Ok(())
}

/// Starts the server, and reaches it once it listens, until `deadline`: or the
/// one that holds the folder, where that one listens by then. One that
/// held it and went meanwhile leaves the folder to the next, which this
/// starts.
fn started(deadline: Instant) -> Result<Server, Failure> {
    loop {
        let mut child = spawned()?;
        loop {
            if let Some(server) = Server::reached()? {
                reap(child);
                return Ok(server);
            }
            let ended = child.try_wait().map_err(|error| Failure::Own(error.to_string()))?;
            match ended {
                None => {},
                Some(status) if status.code() == Some(ANOTHER_RUNS) => break,
                Some(status) => {
                    return Err(Failure::Own(format!("it ended as it started, with {status}")));
                },
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                reap(child);
                return Err(Failure::Own("it did not listen in time".to_owned()));
            }
            thread::sleep(LOOK);
        }
        let again = Instant::now() + AGAIN;
        while Instant::now() < again {
            if let Some(server) = Server::reached()? {
                return Ok(server);
            }
            thread::sleep(LOOK);
        }
        if Instant::now() >= deadline {
            return Err(Failure::Own("another held its folder for too long".to_owned()));
        }
    }
}

/// The server, spawned: in no pane, with nothing on its stdin and stdout, and
/// on the stderr luneta or the daemon has, which the daemon's is its log.
fn spawned() -> Result<Child, Failure> {
    let program = program();
    let mut command = Command::new(&program);
    in_no_pane(&mut command);
    command.current_dir("/").stdin(Stdio::null()).stdout(Stdio::null());
    command.spawn().map_err(|error| Failure::Own(format!("{}: {error}", program.display())))
}

/// Waits for `child` to end, in a thread of its own, so that it leaves no
/// zombie: a server runs as long as its sessions.
fn reap(mut child: Child) {
    let reaping = thread::Builder::new().name("luneta-mux".to_owned()).spawn(move || child.wait());
    drop(reaping);
}

/// The server that luneta starts: the one beside this process's file, which
/// the same build made, or else the first on the `PATH`.
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

    fn session(id: u64, name: &str, clients: &[u64]) -> api::Session {
        let clients = clients.iter().map(|id| api::Client { id: *id, size: None }).collect();
        let pane = api::Pane {
            id: 7,
            title: String::new(),
            cwd: "/w".to_owned(),
            program: Some(api::pane::Program::Pid(70)),
            clients,
        };
        let tab = api::Tab { id: 4, name: "Tab #1".to_owned(), panes: vec![pane] };
        api::Session { id, name: name.to_owned(), tabs: vec![tab], current_tab: 4 }
    }

    /// What the server lists that does not read is left out, a session at a
    /// time, with why: an id of 0, a name no session takes, an id or a name
    /// another has, and a client numbered past luneta's clients or on two
    /// panes. The rest reads.
    #[test]
    fn a_session_listed_that_does_not_read_is_left_out_alone() {
        let mut twice = session(6, "twice", &[1]);
        let other = api::Pane { id: 8, ..twice.tabs[0].panes[0].clone() };
        twice.tabs[0].panes.push(other);
        let listing = vec![
            session(1, "work", &[2]),
            session(0, "zero", &[]),
            session(2, "a/b", &[]),
            session(1, "again", &[]),
            session(3, "work", &[]),
            session(4, "past", &[u64::from(u32::MAX) + 1]),
            twice,
            session(5, "fine", &[]),
        ];
        let (listed, unread) = read(listing);
        let read: Vec<(u64, &str)> =
            listed.iter().map(|listed| (listed.id.get(), listed.name.as_str())).collect();
        assert_eq!(read, [(1, "work"), (5, "fine")]);
        assert_eq!(unread.len(), 6, "{unread:?}");
        let work = &listed[0];
        assert_eq!(work.clients.to_vec(), [Client { id: 2, pane: PaneId::Terminal(7) }]);
        assert_eq!(work.laid[0].tab_id, TabId(4));
        assert_eq!(work.laid[0].cwd.as_ref().map(Said::as_str), Some("/w"));
        assert!(!work.laid[0].is_held);
    }
}
