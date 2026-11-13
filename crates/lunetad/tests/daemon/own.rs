//! Own sessions, of luneta's own multiplexer, as the daemon makes them and
//! follows them: on the host with their panes and clients, through the same
//! questions as zellij's, under names the two share. Each test has the daemon
//! start the real `luneta-mux`, which the same build puts beside it.

use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;
use std::{fs, thread};

use hyper_util::rt::TokioIo;
use lunetad::api::close_request::Closes as Closed;
use lunetad::api::closes_response::Closes;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::open_request::{self, Opens};
use lunetad::api::{
    self, agent, close_request, kill_request, session as kind, CheckNameRequest, CloseRequest,
    ClosesRequest, DumpScreenRequest, GoRequest, KillRequest, NewOwnRequest, NewOwnResponse,
    OpenRequest, RenameSessionRequest, TurnIntoTabRequest,
};
use lunetad::mux::own::api::mux_client::MuxClient;
use lunetad::mux::own::api::{self as mux_api, pane::Program, ListRequest, Listing};
use lunetad::mux::own::server;
use tokio_stream::StreamExt as _;
use tonic::transport::{Channel, Endpoint, Uri};
use tonic::{Code, Status};

use crate::agents;
use crate::host::{answered, eventually, folder_in, start, Daemon, Host, Running, LUNETA, LUNETAD};
use crate::sessions::session;

/// What an own pane runs in these tests: a program that waits, and so holds
/// its tab open, and that the server hangs up as its session goes.
const WAITS: [&str; 2] = ["sleep", "600"];

/// The own sessions' server, beside the daemon, where the daemon starts it
/// from: built since each file of its crate and its protocol last changed,
/// which cargo builds it again for. `dev test` builds it, but a `cargo test -p
/// lunetad` builds the daemon alone, and would leave the tests a server built
/// before, or none. A file cargo does not build it of, in its folders, takes
/// it to be built again all the same, and a test says which.
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

/// Adds `path` to `files`, or each file in it and its folders, where it is a
/// folder.
fn files_in(path: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(path) else {
        files.push(path.to_owned());
        return;
    };
    for entry in entries {
        files_in(&entry.unwrap().path(), files);
    }
}

/// A host whose daemon runs, and the server of own sessions the daemon starts
/// for it, which goes with the test, whatever becomes of its sessions.
pub struct Owned {
    pub host: Host,
    daemon: Daemon,
    runtime: tokio::runtime::Runtime,
}

impl Owned {
    pub fn new() -> Self {
        built_server();
        let host = Host::new();
        let daemon = start(host.vars()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        Owned { host, daemon, runtime }
    }

    /// Asks the daemon, as a client does.
    pub fn ask<T, F>(&self, asking: impl FnOnce(DaemonClient<Channel>) -> F) -> Result<T, Status>
    where
        F: Future<Output = Result<tonic::Response<T>, Status>>,
    {
        let folder = folder_in(self.host.runtime.path()).unwrap();
        self.runtime.block_on(async {
            let daemon = lunetad::connect(&folder).await.unwrap();
            asking(daemon).await.map(tonic::Response::into_inner)
        })
    }

    /// Has the daemon make the own session `name`, in the home, that runs
    /// `WAITS`.
    pub fn make(&self, name: &str) -> Result<NewOwnResponse, Status> {
        let request = NewOwnRequest {
            on: None,
            name: name.to_owned(),
            cwd: self.home(),
            command: WAITS.map(str::to_owned).to_vec(),
        };
        self.ask(|mut daemon| async move { daemon.new_own(request).await })
    }

    pub fn home(&self) -> String {
        self.host.home.path().to_str().unwrap().to_owned()
    }

    /// The own session `name`, once the host has it.
    pub fn listed(&self, name: &str) -> api::Session {
        eventually(&format!("{name} is on the host"), || session(&self.host.host(), name).cloned())
    }

    /// Where the server listens.
    pub fn socket(&self) -> PathBuf {
        self.host.folder().join(server::SOCKET)
    }

    /// The sessions as their server lists them now.
    pub fn listing(&self) -> Listing {
        let socket = self.socket();
        self.runtime.block_on(async move {
            mux_client(socket).await.list(ListRequest {}).await.unwrap().into_inner()
        })
    }

    /// A client of the server, attached to the own session the daemon says it
    /// made.
    pub fn attach(&self, made: &NewOwnResponse) -> Attached {
        self.attach_saying(made, Vec::new())
    }

    /// A client of the server, attached to the own session the daemon says it
    /// made, that says `said` once there.
    pub fn attach_saying(&self, made: &NewOwnResponse, said: Vec<mux_api::ToServer>) -> Attached {
        let (session, made_at) = (made.session, made.made_at);
        let socket = self.socket();
        let (hang_up, to_go) = tokio::sync::oneshot::channel::<()>();
        let (came, arrival) = mpsc::channel();
        let attaching = thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build();
            runtime.unwrap().block_on(async move {
                let size = Some(mux_api::Size { columns: 80, rows: 24 });
                let is = Some(mux_api::place::Is::Session(session));
                let to = Some(mux_api::Place { is, made_at });
                let hello = mux_api::Hello { to, size, colours: None };
                let hello = Some(mux_api::to_server::Event::Hello(hello));
                let hello = mux_api::ToServer { event: hello };
                let said = std::iter::once(hello).chain(said);
                let said = tokio_stream::iter(said).chain(tokio_stream::pending());
                let mut heard = mux_client(socket).await.attach(said).await.unwrap().into_inner();
                let attached = heard.message().await.unwrap().and_then(|said| match said.event {
                    Some(mux_api::to_client::Event::Attached(attached)) => Some(attached.client),
                    _ => None,
                });
                came.send(attached).unwrap();
                tokio::select! {
                    _ = to_go => {},
                    () = async { while let Ok(Some(_)) = heard.message().await {} } => {},
                }
            });
        });
        let came = arrival.recv_timeout(Duration::from_secs(15));
        let Ok(Some(client)) = came else { panic!("the client came: {came:?}") };
        Attached { client, hang_up: Some(hang_up), attaching: Some(attaching) }
    }

    /// The pid of what the pane `pane` runs, as its server lists it.
    fn pid_of(&self, pane: u32) -> u32 {
        let listing = self.listing();
        let panes = listing.sessions.iter().flat_map(|session| &session.tabs);
        let listed = panes.flat_map(|tab| &tab.panes).find(|it| it.id == pane).unwrap();
        let Some(Program::Pid(pid)) = listed.program else {
            panic!("pane {pane} runs nothing: {listed:?}");
        };
        pid
    }
}

/// A client of the server, by the id it gave it, which hangs up as it drops.
pub struct Attached {
    pub client: u64,
    hang_up: Option<tokio::sync::oneshot::Sender<()>>,
    attaching: Option<thread::JoinHandle<()>>,
}

impl Drop for Attached {
    fn drop(&mut self) {
        if let Some(hang_up) = self.hang_up.take() {
            let _ = hang_up.send(());
        }
        if let Some(attaching) = self.attaching.take() {
            let _ = attaching.join();
        }
    }
}

/// A client of the server that listens on `socket`.
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

/// The own session `session` is, by its id and when its server made it: none
/// for zellij's.
pub fn own(session: &api::Session) -> Option<api::OwnSession> {
    match &session.mux {
        Some(kind::Mux::Own(own)) => {
            assert!(own.made_at.is_some(), "an own session was made");
            Some(*own)
        },
        Some(kind::Mux::Zellij(_)) | None => None,
    }
}

impl Drop for Owned {
    /// Ends the server, which hangs up what runs in its panes, where the test
    /// left it running.
    fn drop(&mut self) {
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

/// The question that closes `closes` of the own session `own`, named `name`.
fn close(name: &str, own: api::OwnSession, closes: Closed) -> CloseRequest {
    let session = name.to_owned();
    let whose = Some(close_request::Whose::Own(own));
    CloseRequest { on: None, session, closes: Some(closes), whose }
}

/// The question that opens a tab in `name` that runs `WAITS`.
fn tab_in(name: &str, cwd: String) -> OpenRequest {
    let tab = open_request::Tab { name: Some("second".to_owned()), alone_since: None };
    let command = WAITS.map(str::to_owned).to_vec();
    OpenRequest { on: None, session: name.to_owned(), cwd, command, opens: Some(Opens::Tab(tab)) }
}

/// The daemon makes an own session, starting the server for it, and has it
/// on the host by its name and its id, with no socket and no time it started
/// at, which are zellij's: its pane, which works where it was made, and its
/// clients, none yet.
#[test]
fn an_own_session_the_daemon_makes_is_on_the_host_with_its_panes() {
    let owned = Owned::new();
    let made = owned.make("own").unwrap();

    let listed = eventually("own's pane says what it runs", || {
        let listed = owned.listed("own");
        let panes = listed.panes.as_ref()?;
        panes.panes.iter().any(|pane| pane.command.is_some()).then_some(listed)
    });
    let key = own(&listed).map(|own| (own.id, own.made_at));
    assert_eq!(key, Some((made.session, made.made_at)));
    assert!(made.made_at.is_some(), "the daemon says when it made the session");
    assert_eq!((listed.socket, listed.started_at), (0, None));
    let pane = api::Pane {
        id: made.pane,
        tab: 0,
        tab_name: "Tab #1".to_owned(),
        cwd: Some(owned.home()),
        command: Some(WAITS.join(" ")),
        titled: None,
        shell: None,
        tab_id: Some(made.tab),
        held: false,
    };
    assert_eq!(listed.panes, Some(api::Panes { panes: vec![pane] }));
    assert_eq!(listed.clients, Some(api::Clients { clients: Vec::new() }));
}

/// An own pane's shell needs no hook: the host has what it runs, and what the
/// marks it writes (OSC 133) say it ran, and how that ended.
#[test]
fn an_own_pane_s_shell_marks_say_what_it_ran_on_the_host() {
    let owned = Owned::new();
    let marks = "m() { printf '\\033]133;%s\\007' \"$1\"; }; \
        m A; m 'C;cmdline_url=make%20test'; m 'D;3'; m A; exec sleep 600";
    let command = ["sh", "-c", marks].map(str::to_owned).to_vec();
    let request =
        NewOwnRequest { on: None, name: "marks".to_owned(), cwd: owned.home(), command };
    owned.ask(|mut daemon| async move { daemon.new_own(request).await }).unwrap();
    let pane = eventually("the host says make test ended", || {
        let panes = owned.listed("marks").panes?.panes;
        let pane = panes.into_iter().next()?;
        let ended = matches!(
            pane.shell.as_ref()?.doing.as_ref()?,
            api::shell::Doing::Ended(ended) if ended.command.as_deref() == Some("make test")
        );
        (ended && pane.command.as_deref() == Some("sleep 600")).then_some(pane)
    });
    let Some(api::shell::Doing::Ended(ended)) = pane.shell.and_then(|shell| shell.doing) else {
        unreachable!("the host said it ended");
    };
    assert_eq!(ended.ending, Some(api::shell::ended::Ending::Status(3)));
    assert!(ended.at.is_some() && ended.took.is_some(), "{ended:?}");
}

/// A shell that marks a command without its line, as bash and zsh do with a
/// terminal's integration, has the host say that command ended, with no line.
#[test]
fn an_own_pane_s_shell_that_marks_no_line_says_its_command_ended() {
    let owned = Owned::new();
    let marks =
        "m() { printf '\\033]133;%s\\007' \"$1\"; }; m A; m C; m 'D;3'; m A; exec sleep 600";
    let command = ["sh", "-c", marks].map(str::to_owned).to_vec();
    let request =
        NewOwnRequest { on: None, name: "unsaid".to_owned(), cwd: owned.home(), command };
    owned.ask(|mut daemon| async move { daemon.new_own(request).await }).unwrap();
    let ended = eventually("the host says a command ended", || {
        let pane = owned.listed("unsaid").panes?.panes.into_iter().next()?;
        match pane.shell?.doing? {
            api::shell::Doing::Ended(ended) => Some(ended),
            _ => None,
        }
    });
    assert_eq!(ended.command, None);
    assert_eq!(ended.ending, Some(api::shell::ended::Ending::Status(3)));
}

/// A tab opens in an own session as in zellij's, at its end, and closes as
/// `Closes` says it does: a tab, by the own session's id. The last closes
/// the session, which `Closes` says too, and the host lets it go.
#[test]
fn an_own_session_s_tabs_open_and_close_as_zellij_s_do() {
    let owned = Owned::new();
    let made = owned.make("own").unwrap();
    owned.listed("own");

    let request = tab_in("own", owned.home());
    let opened = owned.ask(|mut daemon| async move { daemon.open(request).await }).unwrap();
    let tabs = eventually("the tab is on the host", || {
        let panes = session(&owned.host.host(), "own")?.panes.clone()?.panes;
        (panes.len() == 2).then_some(panes)
    });
    let second = (tabs[1].id, tabs[1].tab, tabs[1].tab_name.as_str());
    assert_eq!(second, (opened.terminal, 1, "second"));

    let closes = |pane| {
        let request = ClosesRequest { on: None, session: "own".to_owned(), pane };
        owned.ask(|mut daemon| async move { daemon.closes(request).await }).unwrap().closes
    };
    let tab_id = tabs[1].tab_id.unwrap();
    let tab = api::closes_response::Tab { id: tab_id, panes: vec![opened.terminal] };
    assert_eq!(closes(opened.terminal), Some(Closes::Tab(tab.clone())));
    let request = close("own", own(&owned.listed("own")).unwrap(), Closed::Tab(tab));
    owned.ask(|mut daemon| async move { daemon.close(request).await }).unwrap();
    eventually("the tab is gone", || {
        let panes = session(&owned.host.host(), "own")?.panes.clone()?.panes;
        (panes.len() == 1).then_some(())
    });

    assert_eq!(closes(made.pane), Some(Closes::Session(())));
}

/// A rename keeps an own session's id and when it was made, and a kill by its
/// name, its id and when it was made ends it: the host lets it go, and so does
/// its server, which held no other. A kill by the name it had stays, and so
/// does one of another time it was made, as the next server gives its ids
/// again.
#[test]
fn an_own_session_is_renamed_and_killed_by_its_name_id_and_making() {
    let owned = Owned::new();
    let made = owned.make("own").unwrap();
    let before = own(&owned.listed("own")).unwrap();
    assert_eq!(before.id, made.session);

    let session = "own".to_owned();
    let request = RenameSessionRequest { on: None, session, to: "renamed".to_owned() };
    owned.ask(|mut daemon| async move { daemon.rename_session(request).await }).unwrap();
    assert_eq!(own(&owned.listed("renamed")), Some(before));

    let kill = |session: &str, own: api::OwnSession| {
        let session = session.to_owned();
        let whose = Some(kill_request::Whose::Own(own));
        let request = KillRequest { on: None, session, whose };
        owned.ask(|mut daemon| async move { daemon.kill(request).await })
    };
    let stays = kill("own", before).unwrap_err();
    assert!(stays.message().contains("no own session that runs"), "{stays:?}");
    let mut other = before.made_at.unwrap();
    other.nanos = (other.nanos + 1) % 1_000_000_000;
    let other = api::OwnSession { made_at: Some(other), ..before };
    let stays = kill("renamed", other).unwrap_err();
    assert!(stays.message().contains("made since, which stays"), "{stays:?}");
    kill("renamed", before).unwrap();
    eventually("the session is gone", || owned.host.host().sessions.is_empty().then_some(()));
    eventually("the server has ended", || (!owned.socket().exists()).then_some(()));
}

/// The server listens where the daemon reaches it, and one started while
/// another holds the folder ends as the daemon waits on it to: the server
/// keeps its own copy of both, as it does not depend on the daemon.
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

/// A session made as the server ends, its last session killed, is made all
/// the same: the daemon waits for that server to go, and starts another. That
/// one counts its ids from 1 again, and the time it made it tells its session
/// from the first.
#[test]
fn a_session_made_as_the_server_ends_is_made_by_the_next() {
    let owned = Owned::new();
    let first = owned.make("first").unwrap();
    let was = own(&owned.listed("first")).unwrap();
    assert_eq!(was.id, first.session);
    let session = "first".to_owned();
    let whose = Some(kill_request::Whose::Own(was));
    let request = KillRequest { on: None, session, whose };
    owned.ask(|mut daemon| async move { daemon.kill(request).await }).unwrap();

    let next = owned.make("next").unwrap();
    let now = own(&owned.listed("next")).unwrap();
    assert_eq!(now.id, next.session);
    assert_eq!(now.id, first.session, "the next server counts from 1 again");
    assert_ne!(now.made_at, was.made_at);
}

/// zellij's sessions and the own ones share their names: neither makes, nor
/// renames to, a name a session of either has, and a name an own session has
/// is no free one.
#[test]
fn zellij_s_sessions_and_own_ones_share_their_names() {
    let owned = Owned::new();
    let _work = owned.host.serve("work");
    eventually("work runs", || session(&owned.host.host(), "work").map(drop));

    let refused = owned.make("work").unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert!(refused.message().contains("work runs already"), "{refused:?}");
    owned.make("own").unwrap();
    owned.listed("own");
    let refused = owned.make("own").unwrap_err();
    assert!(refused.message().contains("own session"), "{refused:?}");

    let request = CheckNameRequest { on: None, name: "own".to_owned() };
    let refused = owned.ask(|mut daemon| async move { daemon.check_name(request).await });
    assert!(refused.unwrap_err().message().contains("own session"));
    let rename = |session: &str, to: &str| {
        let (session, to) = (session.to_owned(), to.to_owned());
        let request = RenameSessionRequest { on: None, session, to };
        owned.ask(|mut daemon| async move { daemon.rename_session(request).await }).unwrap_err()
    };
    assert!(rename("work", "own").message().contains("as an own session"));
    assert!(rename("own", "work").message().contains("work runs already"));
    assert_eq!(owned.host.zellij.renamed(), [] as [String; 0]);
}

/// A zellij session made by a name an own session has, as zellij checks only
/// its own, takes the name from both: a question by it goes to neither.
#[test]
fn a_name_a_zellij_session_and_an_own_one_share_is_neither_s() {
    let owned = Owned::new();
    owned.make("work").unwrap();
    owned.listed("work");
    let _work = owned.host.serve("work");
    eventually("both are on the host", || {
        let host = owned.host.host();
        let named = host.sessions.iter().filter(|session| session.name == "work").count();
        (named == 2).then_some(())
    });

    let request = DumpScreenRequest { on: None, session: "work".to_owned(), pane: None };
    let refused = owned.ask(|mut daemon| async move { daemon.dump_screen(request).await });
    let message = refused.unwrap_err().message().to_owned();
    assert!(message.contains("two sessions are named work"), "{message}");
}

/// The server numbers panes across its sessions, so a pane of one own session
/// is none of another's, and its screen does not dump by the other's name.
#[test]
fn a_pane_of_another_own_session_does_not_dump_by_this_one_s_name() {
    let owned = Owned::new();
    owned.make("one").unwrap();
    let other = owned.make("other").unwrap();
    owned.listed("one");

    let session = "one".to_owned();
    let request = DumpScreenRequest { on: None, session, pane: Some(other.pane) };
    let refused = owned.ask(|mut daemon| async move { daemon.dump_screen(request).await });
    let message = refused.unwrap_err().message().to_owned();
    assert!(message.contains(&format!("has no pane {}", other.pane)), "{message}");
}

/// What an own session has no way to do yet is refused as such, and a
/// question that names it by the id 0, which no session has, or by no time it
/// was made, is no question.
#[test]
fn what_an_own_session_cannot_do_yet_is_refused_as_such() {
    let owned = Owned::new();
    let made = owned.make("own").unwrap();
    owned.listed("own");

    let beside = OpenRequest {
        on: None,
        session: "own".to_owned(),
        cwd: owned.home(),
        command: Vec::new(),
        opens: Some(Opens::Beside(made.pane)),
    };
    let refused = owned.ask(|mut daemon| async move { daemon.open(beside).await }).unwrap_err();
    let message = refused.message();
    assert!(message.contains("own sessions cannot open a pane beside another yet"), "{message}");

    let started_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
    let session = "own".to_owned();
    let request = TurnIntoTabRequest { on: None, session, pane: made.pane, started_at };
    let refused = owned.ask(|mut daemon| async move { daemon.turn_into_tab(request).await });
    assert!(refused.unwrap_err().message().contains("cannot turn a pane into a tab"));

    let listed = own(&owned.listed("own")).unwrap();
    let unread = [api::OwnSession { id: 0, ..listed }, api::OwnSession { made_at: None, ..listed }];
    for unread in unread {
        let session = "own".to_owned();
        let whose = Some(kill_request::Whose::Own(unread));
        let request = KillRequest { on: None, session, whose };
        let refused = owned.ask(|mut daemon| async move { daemon.kill(request).await });
        let refused = refused.unwrap_err();
        assert_eq!(refused.code(), Code::InvalidArgument, "{refused:?}");
    }
    assert_eq!(own(&owned.listed("own")).map(|own| own.id), Some(made.session));
}

/// A client in an own session goes to no name that no own session has, and
/// the refusal says why: zellij has it, which the client cannot cross to yet,
/// or no session has it.
#[test]
fn a_client_in_an_own_session_goes_to_no_other_kind_s_name_or_none() {
    let owned = Owned::new();
    owned.make("own").unwrap();
    owned.listed("own");
    let _work = owned.host.serve("work");
    eventually("work runs", || session(&owned.host.host(), "work").map(drop));

    let go = |to: &str| {
        let (at, to) = ("own".to_owned(), to.to_owned());
        let request = GoRequest { on: None, at, to, pane: None };
        owned.ask(|mut daemon| async move { daemon.go(request).await }).unwrap_err()
    };
    let across = go("work");
    assert_eq!(across.code(), Code::FailedPrecondition, "{across:?}");
    assert!(across.message().contains("cannot take a client between"), "{across:?}");
    let none = go("nowhere");
    assert_eq!(none.code(), Code::FailedPrecondition, "{none:?}");
    assert!(none.message().contains("no session is named nowhere"), "{none:?}");
}

/// The screen of an own pane dumps as a zellij pane's does, by the session's
/// name.
#[test]
fn an_own_pane_s_screen_dumps_by_its_session_s_name() {
    let owned = Owned::new();
    let request = NewOwnRequest {
        on: None,
        name: "own".to_owned(),
        cwd: owned.home(),
        command: ["sh", "-c", "echo own pane; exec sleep 600"].map(str::to_owned).to_vec(),
    };
    let made = owned.ask(|mut daemon| async move { daemon.new_own(request).await }).unwrap();

    let screen = eventually("the pane says its line", || {
        let request = DumpScreenRequest { on: None, session: "own".to_owned(), pane: None };
        let dumped = owned.ask(|mut daemon| async move { daemon.dump_screen(request).await });
        dumped.ok().filter(|dumped| dumped.screen.contains("own pane"))
    });
    assert_eq!(screen.pane, made.pane);
}

/// A socket no one but root may connect to, until it drops: then as open as
/// it was, even where a test fails first, for `Owned` to reach the server as
/// it drops, and end it.
struct Shut {
    path: PathBuf,
    was: fs::Permissions,
}

impl Shut {
    fn new(path: &Path) -> Shut {
        let was = fs::metadata(path).unwrap().permissions();
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
        Shut { path: path.to_owned(), was }
    }
}

impl Drop for Shut {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.was.clone());
    }
}

/// An agent that runs in an own pane is there, as the variables the server
/// gives what runs in its panes name it: by the session's id and the pane's.
/// `luneta agents` names it by its session's name, or by its ids where the
/// server cannot be reached, and says why, but only where the daemon can be.
#[test]
fn an_agent_in_an_own_pane_is_there_by_its_ids() {
    let owned = Owned::new();
    let made = owned.make("own").unwrap();
    owned.listed("own");
    let pid = owned.pid_of(made.pane);
    let sessions = owned.host.home.path().join(".claude/sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let file = agents::file_of_pid("in-own", pid, "busy");
    std::fs::write(sessions.join(format!("{pid}.json")), file.to_string()).unwrap();

    let agent = eventually("the agent is on the host", || {
        let host = owned.host.host();
        host.agents.into_iter().find(|agent| agent.pid == pid)
    });
    let session = own(&owned.listed("own"));
    let id = session.unwrap().id;
    assert_eq!(agent.pane, Some(agent::Pane::Own(api::Own { session, pane: made.pane })));
    assert_eq!(agent.zellij(), None);
    let listed = answered(owned.host.vars().command(LUNETA).arg("agents"));
    let listed = String::from_utf8(listed.stdout).unwrap();
    assert!(listed.starts_with(&format!("own:{}  busy", made.pane)), "{listed}");

    // A socket luneta may not connect to is a server it cannot reach: to all
    // but root, which connects whatever the socket's mode.
    let socket = owned.socket();
    let shut = Shut::new(&socket);
    let refused = UnixStream::connect(&socket).map(drop).map_err(|error| error.kind());
    assert_eq!(refused, Err(ErrorKind::PermissionDenied), "root reaches any socket");
    let unreached = answered(owned.host.vars().command(LUNETA).arg("agents"));
    let listed = String::from_utf8(unreached.stdout).unwrap();
    let why = String::from_utf8(unreached.stderr).unwrap();
    assert!(unreached.status.success(), "{why}");
    assert!(listed.starts_with(&format!("?:{} (own {id})  busy", made.pane)), "{listed}");
    assert!(why.contains("luneta-mux cannot list them"), "{why}");

    // Where the daemon cannot be reached either, its failure is all luneta
    // says: what luneta-mux could not do is said only beside the agents.
    let daemon = Shut::new(&owned.host.folder().join(lunetad::folder::SOCKET));
    let alone = answered(owned.host.vars().command(LUNETA).arg("agents"));
    drop((daemon, shut));
    let why = String::from_utf8(alone.stderr).unwrap();
    assert!(!alone.status.success(), "{why}");
    assert!(alone.stdout.is_empty(), "{why}");
    assert!(why.contains(lunetad::folder::SOCKET), "{why}");
    assert!(!why.contains("luneta-mux"), "{why}");
}

/// The daemon runs on while an own session does, however long zellij has had
/// no session and no client has watched it. The test waits out longer than
/// the daemon would run on alone.
pub fn the_daemon_runs_on_while_an_own_session_does() {
    let mut owned = Owned::new();
    owned.make("own").unwrap();
    owned.listed("own");
    assert_eq!(owned.daemon.ended_within(Duration::from_secs(36)), None);
}
