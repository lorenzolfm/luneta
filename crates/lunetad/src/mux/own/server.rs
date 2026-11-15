use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::future::Future;
use std::io::{self, ErrorKind};
use std::num::NonZeroU32;
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
use super::{OwnClientId, OwnId, OwnKey, OwnMade, OwnPane};
use crate::mux::{in_no_pane, Client, Failure, Laid, Listed, PaneId, Runs, SessionName, TabId};
use crate::shells::{Doing, Front};
use crate::{folder, log, Directory};

pub const SOCKET: &str = "mux.sock";

const PROGRAM: &str = "luneta-mux";

pub const ANOTHER_RUNS: u8 = 3;

const CONNECT: Duration = Duration::from_secs(1);

const ANSWER: Duration = Duration::from_secs(5);

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

    fn listing(&self) -> Result<Vec<api::Session>, Failure> {
        let listing = self.ask(|mut client| async move { client.list(api::ListRequest {}).await });
        Ok(listing.map_err(|status| refused(&status))?.sessions)
    }

    fn session(&self, id: OwnId) -> Result<api::Session, Failure> {
        let listed = self.listing()?.into_iter().find(|session| session.id == id.get());
        listed.ok_or_else(|| gone(id))
    }

    fn session_keyed(&self, key: OwnKey) -> Result<api::Session, Failure> {
        of_key(self.session(key.id)?, key)
    }

    fn tab_of(&self, pane: OwnPane) -> Result<u32, Failure> {
        let tabs = self.session_keyed(pane.session)?.tabs;
        let tab = tabs.iter().find(|tab| tab.panes.iter().any(|it| it.id == pane.id.get()));
        let none = || Failure::Own(no_pane(pane.session.id, pane.id.get()));
        Ok(tab.ok_or_else(none)?.id)
    }
}

fn of_key(session: api::Session, key: OwnKey) -> Result<api::Session, Failure> {
    assert_eq!(session.id, key.id.get(), "of_key takes a session of the key's id");
    match made_of(&session, key.id) {
        Ok(made) if made == key.made => Ok(session),
        Ok(_) => Err(gone(key.id)),
        Err(why) => Err(Failure::Unread(why)),
    }
}

pub(super) fn runtime() -> Result<Runtime, Failure> {
    let runtime = runtime::Builder::new_current_thread().enable_all().build();
    runtime.map_err(|error| Failure::Own(format!("no runtime: {error}")))
}

pub(super) fn refused(status: &Status) -> Failure {
    let said = match status.message() {
        "" => status.code().description().to_owned(),
        said => said.to_owned(),
    };
    match status.code() {
        Code::AlreadyExists => Failure::Taken(said),
        _ => Failure::Own(said),
    }
}

fn gone(id: OwnId) -> Failure {
    Failure::Own(format!("own session {id} no longer runs"))
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
            command: pane.foreground.as_ref().and_then(|front| Said::new(front.argv.join(" "))),
        })
    });
    laid.collect()
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    pub id: OwnId,
    pub made: OwnMade,
    pub name: SessionName,
    pub laid: Vec<Laid>,
    pub clients: Listed,
    pub doings: BTreeMap<u32, Doing>,
}

impl Listing {
    #[must_use]
    pub fn key(&self) -> OwnKey {
        OwnKey { id: self.id, made: self.made }
    }
}

fn read(sessions: Vec<api::Session>) -> (Vec<Listing>, Vec<String>) {
    let (listed, mut unread, shells) = read_apart(sessions);
    unread.extend(shells);
    (listed, unread)
}

fn read_apart(sessions: Vec<api::Session>) -> (Vec<Listing>, Vec<String>, Vec<String>) {
    let mut listed: Vec<Listing> = Vec::with_capacity(sessions.len());
    let (mut unread, mut unsaid) = (Vec::new(), Vec::new());
    for session in sessions {
        match one(&session, &listed) {
            Ok((one, shells)) => {
                listed.push(one);
                unsaid.extend(shells);
            },
            Err(why) => unread.push(why),
        }
    }
    (listed, unread, unsaid)
}

fn one(session: &api::Session, before: &[Listing]) -> Result<(Listing, Vec<String>), String> {
    let id = OwnId::new(session.id).ok_or("a session of id 0")?;
    let name = SessionName::running(&session.name)
        .map_err(|why| format!("own session {id}'s name: {why}"))?;
    if before.iter().any(|listed| listed.id == id || listed.name == name) {
        return Err(format!("own session {id}, {name}, has the id or the name of another"));
    }
    let made = made_of(session, id)?;
    let clients = clients(session).map_err(|why| format!("own session {id}'s clients: {why}"))?;
    let (doings, unread) = doings(session, id);
    Ok((Listing { id, made, name, laid: laid(session), clients, doings }, unread))
}

fn doings(session: &api::Session, id: OwnId) -> (BTreeMap<u32, Doing>, Vec<String>) {
    let mut doings = BTreeMap::new();
    let mut unread = Vec::new();
    for pane in session.tabs.iter().flat_map(|tab| &tab.panes) {
        let Some(ran) = &pane.ran else {
            continue;
        };
        let read = ran.is.as_ref().ok_or_else(|| "a word of a newer luneta-mux's".to_owned());
        match read.and_then(doing) {
            Ok(doing) => {
                doings.insert(pane.id, doing);
            },
            Err(why) => unread.push(format!("own session {id}'s pane {}'s shell: {why}", pane.id)),
        }
    }
    (doings, unread)
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

pub fn followed(
    mut told: impl FnMut(Vec<Listing>, Vec<String>) -> bool,
) -> Result<Option<()>, Failure> {
    let Ok(server) = Server::reached_on(runtime()?, None)? else {
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

fn made_of(session: &api::Session, id: OwnId) -> Result<OwnMade, String> {
    let made = session.made_at.ok_or_else(|| format!("own session {id} has no made_at"))?;
    OwnMade::read(made).map_err(|why| format!("own session {id}'s made_at: {why}"))
}

fn found(listing: &[api::Session], name: &SessionName) -> Result<Option<OwnKey>, Failure> {
    let mut named = listing.iter().filter(|session| session.name == name.as_str());
    let Some(session) = named.next() else {
        return Ok(None);
    };
    if named.next().is_some() {
        return Err(Failure::Unread(format!("luneta-mux lists two sessions named {name}")));
    }
    let zero = || Failure::Unread(format!("luneta-mux lists {name} with the id 0"));
    let id = OwnId::new(session.id).ok_or_else(zero)?;
    let made = made_of(session, id).map_err(Failure::Unread)?;
    Ok(Some(OwnKey { id, made }))
}

pub fn named(name: &SessionName) -> Result<Option<Own>, Failure> {
    let Some(server) = Server::reached()? else {
        return Ok(None);
    };
    let key = found(&server.listing()?, name)?;
    Ok(key.map(|key| Own { server, key }))
}

pub fn keyed(key: OwnKey) -> Result<Own, Failure> {
    let Some(server) = Server::reached()? else {
        return Err(gone(key.id));
    };
    let (listed, _) = read(server.listing()?);
    if !listed.iter().any(|own| own.key() == key) {
        return Err(gone(key.id));
    }
    Ok(Own { server, key })
}

pub fn names() -> Result<Vec<String>, Failure> {
    let Some(server) = Server::reached()? else {
        return Ok(Vec::new());
    };
    Ok(server.listing()?.into_iter().map(|session| session.name).collect())
}

pub fn sessions() -> Result<Vec<Listing>, Failure> {
    Ok(listed()?.0)
}

pub fn listed() -> Result<(Vec<Listing>, Vec<String>), Failure> {
    let Some(server) = Server::reached()? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let (listed, unread, _) = read_apart(server.listing()?);
    Ok((listed, unread))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Session(OwnKey),
    Pane(OwnPane),
}

pub fn pane_of(session: OwnKey, pane: u32) -> Result<OwnPane, Failure> {
    OwnPane::new(session, pane).ok_or_else(|| Failure::Own(no_pane(session.id, pane)))
}

fn no_pane(id: OwnId, pane: u32) -> String {
    format!("own session {id} has no pane {pane}")
}

pub struct Own {
    server: Server,
    key: OwnKey,
}

impl fmt::Debug for Own {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Own").field("key", &self.key).finish_non_exhaustive()
    }
}

impl Own {
    #[must_use]
    pub fn id(&self) -> OwnId {
        self.key.id
    }

    #[must_use]
    pub fn key(&self) -> OwnKey {
        self.key
    }

    pub fn named(&self, name: &SessionName) -> Result<Option<OwnKey>, Failure> {
        found(&self.server.listing()?, name)
    }

    pub fn laid(&self) -> Result<Vec<Laid>, Failure> {
        Ok(laid(&self.server.session(self.id())?))
    }

    pub fn clients(&self) -> Result<Listed, Failure> {
        clients(&self.server.session(self.id())?).map_err(Failure::Unread)
    }

    pub fn tab_opened(
        &self,
        cwd: &Directory,
        name: Option<&str>,
        runs: Runs<'_>,
    ) -> Result<TabId, Failure> {
        self.tab_opened_as(name, opens(cwd, runs)?)
    }

    pub fn tab_opened_once_heard(
        &self,
        name: Option<&str>,
        runs: &[&str],
    ) -> Result<TabId, Failure> {
        assert_ne!(runs.len(), 0);
        let session = self.server.session(self.id())?;
        let current = session.tabs.iter().find(|tab| tab.id == session.current_tab);
        let pane = current.and_then(|tab| tab.panes.first());
        let unknown = || Failure::Own(format!("own session {} lists no current tab", self.id()));
        let cwd = pane.ok_or_else(unknown)?.cwd.clone();
        let argv = runs.iter().map(|word| (*word).to_owned()).collect();
        let runs = api::opens::Runs::Program(api::Program { argv });
        let opens = api::Opens { cwd, runs: Some(runs), ends: api::Ends::Closes.into() };
        self.tab_opened_as(name, opens)
    }

    fn tab_opened_as(&self, name: Option<&str>, opens: api::Opens) -> Result<TabId, Failure> {
        let name = name.map(str::to_owned);
        let request = api::NewTabRequest { session: self.id().get(), name, opens: Some(opens) };
        let opened = self.server.ask(|mut client| async move { client.new_tab(request).await });
        Ok(TabId(opened.map_err(|status| refused(&status))?.tab))
    }

    pub fn tab_closed(&self, tab: TabId) -> Result<(), Failure> {
        let request = api::CloseTabRequest { tab: tab.0 };
        let closed = self.server.ask(|mut client| async move { client.close_tab(request).await });
        closed.map(drop).map_err(|status| refused(&status))
    }

    pub fn renamed(&self, to: &SessionName) -> Result<(), Failure> {
        let request = api::RenameRequest { session: self.id().get(), name: to.as_str().to_owned() };
        let renamed = self.server.ask(|mut client| async move { client.rename(request).await });
        renamed.map(drop).map_err(|status| refused(&status))
    }

    pub fn dumped(&self, pane: u32, dumped: api::Dumped) -> Result<String, Failure> {
        self.server.tab_of(pane_of(self.key, pane)?)?;
        let request = api::DumpRequest { pane, dumped: dumped.into() };
        let dump = self.server.ask(|mut client| async move { client.dump(request).await });
        Ok(dump.map_err(|status| refused(&status))?.text)
    }

    pub fn killed(&self) -> Result<(), Failure> {
        let request = api::KillRequest { session: self.id().get() };
        let killed = self.server.ask(|mut client| async move { client.kill(request).await });
        killed.map(drop).map_err(|status| refused(&status))
    }

    pub fn went(&self, mover: Mover, to: Place) -> Result<(), Failure> {
        let (is, session) = match to {
            Place::Session(session) => (api::place::Is::Session(session.id.get()), session),
            Place::Pane(pane) => (api::place::Is::Tab(self.server.tab_of(pane)?), pane.session),
        };
        let to = api::Place { is: Some(is), made_at: Some(session.made.into()) };
        let request = api::GoRequest {
            who: Some(match mover {
                Mover::LastTyped => api::go_request::Who::LastTypedIn(self.id().get()),
                Mover::Client(client) => api::go_request::Who::Client(client.get()),
            }),
            to: Some(to),
        };
        let went = self.server.ask(|mut client| async move { client.go(request).await });
        went.map(drop).map_err(|status| refused(&status))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mover {
    LastTyped,
    Client(OwnClientId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Made {
    pub pane: OwnPane,
    pub tab: NonZeroU32,
}

pub fn made(
    name: &SessionName,
    tab: Option<&str>,
    cwd: &Directory,
    runs: Runs<'_>,
) -> Result<Made, Failure> {
    let deadline = Instant::now() + MAKING;
    let request = api::NewSessionRequest {
        name: name.as_str().to_owned(),
        opens: Some(opens(cwd, runs)?),
        tab: tab.map(str::to_owned),
    };
    let mut runtime = runtime()?;
    loop {
        if Instant::now() >= deadline {
            return Err(Failure::Own("no server made the session in time".to_owned()));
        }
        let server = match Server::reached_on(runtime, Some(ANSWER))? {
            Ok(server) => server,
            Err(runtime) => started(runtime, deadline)?,
        };
        let request = request.clone();
        match server.ask(|mut client| async move { client.new_session(request).await }) {
            Ok(made) => {
                let session = OwnId::new(made.session);
                let unread = || Failure::Unread("luneta-mux made a session of id 0".to_owned());
                let id = session.ok_or_else(unread)?;
                let made_at = match made.made_at {
                    Some(at) => OwnMade::read(at)
                        .map_err(|why| format!("own session {id}'s made_at: {why}")),
                    None => made_of(&server.session(id)?, id),
                };
                let session = OwnKey { id, made: made_at.map_err(Failure::Unread)? };
                let zero = || Failure::Unread(format!("luneta-mux made {id} with the pane 0"));
                let pane = OwnPane::new(session, made.pane).ok_or_else(zero)?;
                let unnumbered = || Failure::Unread(format!("luneta-mux made {id} with the tab 0"));
                let tab = NonZeroU32::new(made.tab).ok_or_else(unnumbered)?;
                return Ok(Made { pane, tab });
            },
            Err(status) if status.code() == Code::Unavailable => {
                gone_by(&server, deadline)?;
                runtime = server.into_runtime();
            },
            Err(status) => return Err(refused(&status)),
        }
    }
}

fn gone_by(server: &Server, deadline: Instant) -> Result<(), Failure> {
    while server.listing().is_ok() {
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

    fn session(id: u64, name: &str, clients: &[u64]) -> api::Session {
        let clients = clients.iter().map(|id| api::Client { id: *id, size: None }).collect();
        let pane = api::Pane {
            id: 7,
            title: String::new(),
            cwd: "/w".to_owned(),
            program: Some(api::pane::Program::Pid(70)),
            clients,
            foreground: None,
            ran: None,
            name: None,
        };
        let tab = api::Tab { id: 4, name: "Tab #1".to_owned(), panes: vec![pane] };
        let made_at = Some(prost_types::Timestamp { seconds: 60, nanos: 0 });
        api::Session { id, name: name.to_owned(), tabs: vec![tab], current_tab: 4, made_at }
    }

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
            api::Session { made_at: None, ..session(7, "unmade", &[]) },
            twice,
            session(5, "fine", &[]),
        ];
        let (_, sessions, shells) = read_apart(listing.clone());
        assert_eq!((sessions.len(), shells.len()), (7, 0), "{sessions:?} {shells:?}");
        let (listed, unread) = read(listing);
        let read: Vec<(u64, &str)> =
            listed.iter().map(|listed| (listed.id.get(), listed.name.as_str())).collect();
        assert_eq!(read, [(1, "work"), (5, "fine")]);
        assert_eq!(unread.len(), 7, "{unread:?}");
        let work = &listed[0];
        assert_eq!(work.made, OwnMade::from_nanos(60_000_000_000));
        assert_eq!(work.clients.to_vec(), [Client { id: 2, pane: PaneId::Terminal(7) }]);
        assert_eq!(work.laid[0].tab_id, TabId(4));
        assert_eq!(work.laid[0].cwd.as_ref().map(Said::as_str), Some("/w"));
        assert!(!work.laid[0].is_held);
    }

    #[test]
    fn a_shell_listed_that_does_not_read_is_left_out_alone() {
        use api::ran::{Going, Is, Ready, Stopped};
        let since = Some(prost_types::Timestamp { seconds: 90, nanos: 0 });
        let working = api::Front::Working.into();
        let unsaid = Going { command: None, since, front: working };
        let shells = [
            Some(Is::Ready(Ready {})),
            None,
            Some(Is::Going(Going { front: api::Front::Unspecified.into(), ..unsaid.clone() })),
            Some(Is::Going(Going { command: Some(String::new()), ..unsaid.clone() })),
            Some(Is::Stopped(Stopped { command: Some("vim".to_owned()), at: None })),
            Some(Is::Going(unsaid)),
        ];
        let mut listed = session(1, "work", &[]);
        let pane = listed.tabs[0].panes[0].clone();
        listed.tabs[0].panes = (1..)
            .zip(shells)
            .map(|(id, is)| api::Pane { id, ran: Some(api::Ran { is }), ..pane.clone() })
            .collect();
        let (_, sessions, shells) = read_apart(vec![listed.clone()]);
        assert_eq!((sessions.len(), shells.len()), (0, 4), "{sessions:?} {shells:?}");
        let (listed, unread) = read(vec![listed]);
        assert_eq!(unread.len(), 4, "{unread:?}");
        let doings: Vec<(u32, &Doing)> =
            listed[0].doings.iter().map(|(pane, doing)| (*pane, doing)).collect();
        let going = Doing::Going {
            command: None,
            since: SystemTime::UNIX_EPOCH + Duration::from_secs(90),
            front: Front::Working,
        };
        assert_eq!(doings, [(1, &Doing::Ready), (6, &going)]);
    }

    #[test]
    fn a_session_is_the_one_its_key_says_or_gone() {
        let key = |seconds| OwnKey {
            id: OwnId::new(3).unwrap(),
            made: OwnMade::from_nanos(seconds * 1_000_000_000_u64),
        };
        let work = || session(3, "work", &[]);
        assert_eq!(of_key(work(), key(60)).map(|it| it.name).ok(), Some("work".to_owned()));
        let before = of_key(work(), key(59));
        assert!(matches!(&before, Err(Failure::Own(why)) if why.ends_with("no longer runs")));
        let unmade = of_key(api::Session { made_at: None, ..work() }, key(60));
        assert!(matches!(&unmade, Err(Failure::Unread(_))), "{unmade:?}");
    }
}
