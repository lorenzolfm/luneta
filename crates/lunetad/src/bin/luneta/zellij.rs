//! What luneta asks of zellij: a client to run beside the sidebar, to take
//! that client to a pane, the clients of a session, and what the menu opens
//! and closes. All luneta knows of zellij's ways is here and in `own`, which
//! finds luneta's client among those, so that a multiplexer of luneta's own
//! can take zellij's place behind it.

use std::collections::HashSet;
use std::env;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::Client;
use lunetad::{clients, program};
use serde::Deserialize;

use crate::host::Event;
use crate::own::{Look, Looked};

/// The longest name zellij makes a session of, in bytes.
const NAME_BYTES_MAX: usize = 60;

// `SessionName::parse` refuses a longer name in zellij's words, which count the
// bytes a name must be shorter than.
const _: () = assert!(NAME_BYTES_MAX + 1 == 61);

/// What names the session of a zellij pane, in the variables of what runs in
/// it.
const SESSION_VAR: &str = "ZELLIJ_SESSION_NAME";

/// What names the pane a zellij command runs in, in its variables, which
/// zellij opens a pane beside.
const PANE_VAR: &str = "ZELLIJ_PANE_ID";

/// Variables that zellij's client must not have. With `ZELLIJ` set, it takes
/// itself for one started in a pane of that session, and will not attach, as
/// when luneta itself runs in a pane: `refusal` makes the one check it skips
/// then. The window ids are of the terminal luneta's own is made by, and name
/// no window.
const LEFT_OUT: [&str; 5] = ["ZELLIJ", SESSION_VAR, PANE_VAR, "WINDOWID", "ALACRITTY_WINDOW_ID"];

/// A name zellij can make a session of. `parse` is the one way to make one, so
/// a name that reaches zellij as one has been held to what zellij takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        if name.len() > NAME_BYTES_MAX {
            return Err("name must be shorter than 61 bytes");
        }
        Ok(SessionName(name.to_owned()))
    }

    /// The name with `-n` after it, cut between characters where it must be
    /// for the two to fit. What is left of a name holds no `/`, and the
    /// postfix is neither whitespace nor a dot, so the two are a name too.
    pub fn with_postfix(&self, n: usize) -> Self {
        let postfix = format!("-{n}");
        let mut end = NAME_BYTES_MAX.saturating_sub(postfix.len()).min(self.0.len());
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        SessionName(format!("{}{postfix}", &self.0[..end]))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why zellij's client must not attach to `session` from the terminal luneta
/// runs in, where it must not: the terminal is a pane of that session, which
/// the client would show inside itself, with the focus on it, so that a key
/// typed there would come round to it again for good. zellij refuses this by
/// `SESSION_VAR`, which the client runs without. A pane of a session renamed
/// since it opened holds the name it had then, and zellij's check goes by it.
pub fn refusal(session: &SessionName) -> Option<String> {
    refused(session, env::var(SESSION_VAR).ok().as_deref())
}

/// `refusal`, for a terminal in the session `current`, if it is a pane's.
fn refused(session: &SessionName, current: Option<&str>) -> Option<String> {
    (current == Some(session.0.as_str())).then(|| {
        format!(
            "this terminal is a pane of \"{session}\", which zellij would show inside itself: \
             name another session, or run luneta outside zellij"
        )
    })
}

/// The program luneta runs beside the sidebar, and its arguments: zellij's
/// client, attached to `session` and making it if none runs, or without one
/// in a session of a name zellij makes up.
pub fn client(session: Option<&SessionName>) -> (String, Vec<String>) {
    match session {
        Some(session) => client_with(&["attach", "--create", &session.0]),
        None => client_with(&[]),
    }
}

/// The program luneta runs beside the sidebar in place of a client that
/// ended, and its arguments: zellij's client, attached to `session`, which
/// runs. Without `--create`, which would make one gone since anew, empty.
pub fn rejoin(session: &str) -> (String, Vec<String>) {
    client_with(&["attach", session])
}

/// zellij's client, with `args`. It runs through `env`, to leave out what
/// `LEFT_OUT` names, which luneta cannot take out of its own variables.
fn client_with(args: &[&str]) -> (String, Vec<String>) {
    let mut through: Vec<String> =
        LEFT_OUT.iter().flat_map(|name| ["-u".to_owned(), (*name).to_owned()]).collect();
    through.push("zellij".to_owned());
    through.extend(args.iter().map(|arg| (*arg).to_owned()));
    ("env".to_owned(), through)
}

/// How long luneta looks for the session its client was in to be gone, once
/// the client ended. zellij takes the session's socket away a moment before
/// its clients end, measured at under a millisecond, and a client that
/// detached leaves it there: that one costs luneta this long as it ends.
const ENDING: Duration = Duration::from_millis(100);

/// How often luneta looks meanwhile.
const ENDING_LOOK: Duration = Duration::from_millis(10);

/// How long luneta goes on trying one session after another for one that
/// answers, once its client's is gone. A session whose server is gone
/// answers at once that it does not run, and a server that hangs holds up
/// every zellij command for as long as `clients` waits, which is longer: it
/// costs luneta one try.
const SEEKING: Duration = Duration::from_secs(1);

/// A session luneta's zellij can go on in: one that runs, as it answered for
/// its clients, which are those it has before luneta's comes.
pub struct Refuge {
    pub session: String,
    pub before: Vec<Client>,
}

/// Where luneta's zellij goes on once its client ended in `left`: the newest
/// session that answers for its clients, which the picker lists first, where
/// `left` went with the client, as it does once zellij quits, its last pane
/// exits, or it is killed. None where `left` runs still, as the client
/// detached; where no other session answers; and where luneta cannot read
/// all of zellij's folder, as either could be in what it cannot.
///
/// A socket can outlive its server, as one that crashed, and a session can
/// end as luneta looks. `attach` brings back a session that ended, where
/// zellij kept it to resurrect, and otherwise zellij's client fails: only a
/// session that answers is one to go on in.
pub fn refuge(left: &str) -> Option<Refuge> {
    let folder = lunetad::zellij_folder();
    let deadline = Instant::now() + ENDING;
    loop {
        let running = lunetad::zellij_sessions(&folder)?;
        if running.iter().all(|(name, _)| name != left) {
            let seeking = Instant::now() + SEEKING;
            let sessions = newest_first(running).into_iter();
            return sessions.take_while(|_| Instant::now() < seeking).find_map(|session| {
                let before = clients(&session).ok()?;
                Some(Refuge { session, before })
            });
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(ENDING_LOOK);
    }
}

/// The names of `running`, newest first, by when each started and then by
/// name, as the picker lists them.
fn newest_first(mut running: Vec<(String, SystemTime)>) -> Vec<String> {
    running.sort_by(|(a, a_at), (b, b_at)| (b_at, b).cmp(&(a_at, a)));
    running.into_iter().map(|(name, _)| name).collect()
}

/// The session a directory is for, as the picker's directories name it: for
/// the directory itself, or for its repository too when it is a worktree of
/// one, as `luneta-main`. None for a directory no session can be named for,
/// as `/`.
pub fn session_for(dir: &Path) -> Option<SessionName> {
    let path = dir.to_str()?;
    common_dir(dir)
        .and_then(|common| worktree_name(path, common.to_str()?))
        .or_else(|| SessionName::parse(path.split('/').rfind(|part| !part.is_empty())?).ok())
}

/// The repository a linked worktree is of: its git directory, which every
/// worktree of it shares. A worktree's `.git` is a file that names its own git
/// directory, and that names the shared one in `commondir`. None for a
/// directory that is no linked worktree, as a repository's main one is not.
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = fs::read_to_string(dir.join(".git")).ok()?;
    let own = dir.join(dot_git.lines().next()?.strip_prefix("gitdir: ")?);
    let common = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(common.lines().next()?)).ok()
}

/// A worktree is named for its repository too, `luneta-main`, where `main`
/// alone would name one of every repository, wherever the worktree lives. The
/// repository is its git directory, less a bare repository's `.git`, or, when
/// that directory is hidden, as a `.git` or a `.bare`, the directory that
/// holds it. A worktree whose own name starts with its repository's, as
/// `app-hotfix` beside `app`, keeps it, and so does one whose name would come
/// out too long for zellij to make a session of.
fn worktree_name(path: &str, common: &str) -> Option<SessionName> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    let mut parts = common.split('/').filter(|part| !part.is_empty());
    let mut repository = parts.next_back()?;
    if repository.starts_with('.') {
        repository = parts.next_back()?;
    }
    let repository =
        repository.strip_suffix(".git").filter(|bare| !bare.is_empty()).unwrap_or(repository);
    if base.strip_prefix(repository).is_some_and(|rest| !rest.starts_with(char::is_alphanumeric)) {
        return None;
    }
    SessionName::parse(&format!("{repository}-{base}")).ok()
}

/// What luneta types into zellij's client before it asks zellij to move it:
/// `ScrollLock` let go, as the kitty keyboard protocol spells it, with no
/// modifiers and the event of a release, `1:3`. zellij moves the client of a
/// session that last sent it a key, and a client that has sent none since it
/// came to the session, as luneta's has not once the sidebar took it there,
/// moves for no action. The client counts a key let go as a key. No one binds
/// `ScrollLock`, and zellij writes nothing of it to a pane that does not read
/// keys as kitty spells them. One that does gets it as it came, a release it
/// never asked to hear of, and passes it over: nvim does, where the key
/// pressed would have it insert a character of the protocol's own in the
/// text. The client reads it as kitty spells it unless its config turns the
/// protocol off, and then zellij moves the client only once you type in it.
pub const NUDGE: &[u8] = b"\x1b[57359;1:3u";

/// How long an action waits for zellij to take in `NUDGE`, which goes to it
/// through its client while the action goes straight to its server.
const NUDGED: Duration = Duration::from_millis(30);

/// How long luneta waits for its client to come to a session it sent it to
/// from another, after zellij took the action, before it takes the client
/// for one that stayed. It comes within milliseconds.
const ARRIVAL: Duration = Duration::from_secs(2);

/// How long luneta waits for its client to come to the session it attaches
/// to as it starts, which zellij can take a while to make.
const START: Duration = Duration::from_secs(10);

/// How often luneta asks zellij for the clients of a session its client is
/// on its way to.
const ARRIVING: Duration = Duration::from_millis(20);

/// Takes the client from the session `at` to the pane `pane` of `to`, or to
/// the session `to` alone, on the pane it last had the focus on, and then
/// takes the look `then`, if any: of `at`, for the pane, or of `to`, for the
/// client's arrival there, after which it does `after`. The client must have
/// been sent `NUDGE` first. What zellij says of a failure comes back as a
/// notice.
pub fn go(
    at: &str,
    to: &str,
    pane: Option<u32>,
    then: Option<Look>,
    after: Option<After>,
    events: &Sender<Event>,
) {
    let mut command = Command::new("zellij");
    let pane = pane.map(|id| format!("terminal_{id}"));
    match pane {
        Some(pane) if at == to => {
            command.args(["--session", at, "action", "focus-pane-id", &pane]);
        },
        None if at == to => return,
        pane => {
            command.args(["--session", at, "action", "switch-session", to]);
            command.args(pane.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
        },
    }
    let then = match (then, after) {
        (Some(look), after) if at != to => Some(Then::Arrive(look, after)),
        (then, None) => then.map(Then::Look),
        // Nothing would say when the client is there.
        (_, Some(after)) => {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(format!("luneta cannot tell {}", after.when())));
            return;
        },
    };
    run(command, then, events);
}

/// Takes the client from the session `at` to a session it makes, `to`, that
/// starts in `cwd`, and then takes the look `then`, if any, for the client's
/// arrival there. A session that runs under that name is where it goes, and
/// `cwd` goes unused. The client must have been sent `NUDGE` first.
pub fn start(at: &str, to: &SessionName, cwd: &str, then: Option<Look>, events: &Sender<Event>) {
    let mut command = Command::new("zellij");
    command.args(["--session", at, "action", "switch-session", &to.0, "--cwd", cwd]);
    run(command, then.map(|look| Then::Arrive(look, None)), events);
}

/// What luneta has zellij do once its client has come to the session a go
/// takes it to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum After {
    /// Open this, for the menu numbered so, as `open` does.
    Open(u64, Open),
    /// Kill the session the client left, which it was in.
    Kill(String),
}

impl After {
    /// What waits on the client's arrival, as a sentence about it ends.
    fn when(&self) -> String {
        match self {
            After::Open(..) => "when zellij's client is there, and opens nothing".to_owned(),
            After::Kill(session) => {
                format!("when zellij's client has left \"{session}\", which stays")
            },
        }
    }

    /// Does it, now that the client is there, or says why not when it did not
    /// come.
    fn done(self, came: bool, events: &Sender<Event>) {
        match (self, came) {
            (After::Open(menu, open), came) => {
                let opened = if came {
                    opening(&open)
                } else {
                    Err("zellij did not take its client there".to_owned())
                };
                // luneta is on its way out.
                let _ = events.send(Event::Opened { menu, opened });
            },
            // A zellij of its own kills it: one run as the client's would
            // wait on the session it ends.
            (After::Kill(session), true) => kill(&session, events),
            (After::Kill(session), false) => {
                let _ = events.send(Event::Notice(format!("\"{session}\" stays")));
            },
        }
    }
}

/// What a key of the menu opens in a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Open {
    /// `command`, or your shell where there is none, in a pane of its own
    /// beside the pane `beside` of `session`, in its tab, and in `cwd`.
    Pane { session: String, beside: u32, cwd: String, command: Vec<String> },
    /// A tab of `session`, in `cwd`. zellij makes a tab as big as a client of
    /// its session, and one it opens in a session with none has no room for
    /// its panes: in another session than luneta's client's, it opens once
    /// that client has come.
    Tab { session: String, cwd: String },
}

/// Opens `open`, on a thread of its own, for the menu numbered `menu`, with
/// no client moved, and hands back the terminal it opened, for luneta to take
/// its client to, or why it opened none.
pub fn open(menu: u64, open: Open, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let opened = opening(&open);
        // luneta is on its way out.
        let _ = events.send(Event::Opened { menu, opened });
    });
}

/// How long zellij has to open a pane or a tab, or to list a session's
/// panes: a server that runs answers at once.
const ANSWER: Duration = Duration::from_secs(2);

/// Opens `open`, and says which terminal it opened. zellij opens a pane
/// `--no-focus` beside the one the command runs in, which it reads in
/// `PANE_VAR`, whatever tab that is in and whether a client is on it, and
/// says it as `terminal_<id>`. It says a tab's id, and the tab's terminal is
/// the one it lists in that tab, but for the placeholder of the plugin's dock,
/// which a layout of the dock's opens first.
fn opening(open: &Open) -> Result<u32, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    match open {
        Open::Pane { session, beside, cwd, command: runs } => {
            command.env(PANE_VAR, beside.to_string());
            command.args(["--session", session, "action", "new-pane", "--no-focus", "--cwd", cwd]);
            if !runs.is_empty() {
                command.arg("--").args(runs);
            }
            let said = answered(&mut command)?;
            let pane = said.trim().strip_prefix("terminal_").and_then(|id| id.parse().ok());
            pane.ok_or_else(|| format!("zellij opened no pane it named: {:?}", said.trim()))
        },
        Open::Tab { session, cwd } => {
            command.args(["--session", session, "action", "new-tab", "--no-focus", "--cwd", cwd]);
            let said = answered(&mut command)?;
            let Ok(tab) = said.trim().parse::<u32>() else {
                return Err(format!("zellij opened no tab it named: {:?}", said.trim()));
            };
            let laid = laid(session)?;
            terminal_of(&laid, tab).ok_or_else(|| "zellij opened a tab with no pane".to_owned())
        },
    }
}

/// What `command`, a zellij of `quiet`'s, said on its stdout as it did what
/// it was asked, or why it did not.
fn answered(command: &mut Command) -> Result<String, String> {
    let ran = program::run(command, ANSWER).map_err(|error| unrun(&error))?;
    let said = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    String::from_utf8(said).map_err(|_| "zellij said what is not UTF-8".to_owned())
}

/// What closing a terminal of a session closes with it, as zellij closes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Closes {
    /// The pane alone: something else holds its tab open.
    Pane,
    /// Its tab, by its id, and the terminals in it, the pane's own among
    /// them, and the floating ones: nothing else holds the tab open.
    Tab { id: u32, panes: Vec<u32> },
    /// Its session: nothing else holds the tab open, and the session has no
    /// other tab.
    Session,
}

/// What a placeholder of the plugin's dock runs: the column each tab keeps
/// for the plugin's sidebar, beside luneta's or in its place.
const PLACEHOLDER: [&str; 3] = ["tail", "-f", "/dev/null"];

/// The title the plugin's sidebar gives its pane where it stands in that
/// column.
const SIDEBAR_TITLE: &str = "luneta";

/// A pane as `zellij action list-panes --json` lists it, as much of it as
/// says what it holds open.
#[derive(Clone, Deserialize)]
#[expect(clippy::struct_excessive_bools, reason = "zellij lists each as a bool of its own")]
struct Laid {
    id: u32,
    is_plugin: bool,
    is_selectable: bool,
    is_suppressed: bool,
    is_floating: bool,
    tab_id: u32,
    title: String,
    /// What a terminal opened to run, as one line; none for a shell.
    #[serde(default)]
    terminal_command: Option<String>,
}

impl Laid {
    /// Whether it keeps its tab open: a tiled pane you can go to, a plugin's
    /// as well, or a terminal another took the place of, which zellij brings
    /// back once that one closes. A floating pane goes with the tab, and so
    /// does a bar, which you cannot go to. So does the column of the plugin's
    /// dock, which closes a tab with the last pane beside it, as zellij would
    /// close the tab without one.
    fn holds_tab(&self) -> bool {
        if self.docked() {
            return false;
        }
        let tiled = self.is_selectable && !self.is_suppressed && !self.is_floating;
        tiled || (self.is_suppressed && !self.is_plugin)
    }

    /// Whether it stands in the column of the plugin's dock: a placeholder,
    /// or the plugin's sidebar.
    fn docked(&self) -> bool {
        if self.is_plugin {
            return self.title == SIDEBAR_TITLE && !self.is_floating;
        }
        let placeholder = |command: &str| command.split(' ').eq(PLACEHOLDER);
        self.terminal_command.as_deref().is_some_and(placeholder)
    }
}

/// The terminal of the tab whose id is `tab` among `laid`, the first that is
/// not a placeholder of the plugin's dock: where luneta goes in a tab it
/// opened.
fn terminal_of(laid: &[Laid], tab: u32) -> Option<u32> {
    let terminal = |pane: &&Laid| !pane.is_plugin && !pane.docked() && pane.tab_id == tab;
    laid.iter().find(terminal).map(|pane| pane.id)
}

/// Asks zellij, on a thread of its own, what closing the terminal `pane` of
/// `session` closes, for the menu numbered `menu`, and hands back what it
/// answers.
pub fn closes(menu: u64, session: &str, pane: u32, events: &Sender<Event>) {
    let (session, events) = (session.to_owned(), events.clone());
    thread::spawn(move || {
        let closes = laid(&session).and_then(|laid| closing(&laid, pane));
        // luneta is on its way out.
        let _ = events.send(Event::Closes { menu, closes });
    });
}

/// The panes of `session` as zellij lists them now, the plugins' you can go
/// to too: it leaves out the bars.
fn laid(session: &str) -> Result<Vec<Laid>, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["--session", session, "action", "list-panes", "--json"]);
    let said = answered(&mut command)?;
    serde_json::from_str(&said)
        .map_err(|error| format!("zellij listed panes that do not read: {error}"))
}

/// What closing the terminal `pane` closes among `laid`, the panes of its
/// session, or why it closes nothing: it is gone.
fn closing(laid: &[Laid], pane: u32) -> Result<Closes, String> {
    let terminal = |laid: &&Laid| !laid.is_plugin && laid.id == pane;
    let Some(tab) = laid.iter().find(terminal).map(|laid| laid.tab_id) else {
        return Err(format!("pane {pane} is gone"));
    };
    let in_tab = || laid.iter().filter(|laid| laid.tab_id == tab);
    if in_tab().any(|other| (other.is_plugin || other.id != pane) && other.holds_tab()) {
        return Ok(Closes::Pane);
    }
    let tabs: HashSet<u32> = laid.iter().map(|laid| laid.tab_id).collect();
    if tabs.len() == 1 {
        return Ok(Closes::Session);
    }
    let panes = in_tab().filter(|laid| !laid.is_plugin).map(|laid| laid.id).collect();
    Ok(Closes::Tab { id: tab, panes })
}

/// Closes the terminal `pane` of `session`, on a thread of its own.
pub fn close_pane(session: &str, pane: u32, events: &Sender<Event>) {
    let mut command = Command::new("zellij");
    let pane = format!("terminal_{pane}");
    command.args(["--session", session, "action", "close-pane", "--pane-id", &pane]);
    run(command, None, events);
}

/// Closes the tab of `session` whose id is `tab`, its panes and all, on a
/// thread of its own.
pub fn close_tab(session: &str, tab: u32, events: &Sender<Event>) {
    let mut command = Command::new("zellij");
    command.args(["--session", session, "action", "close-tab-by-id", &tab.to_string()]);
    run(command, None, events);
}

/// Kills `session`, which luneta's client is not in, on a thread of its own.
pub fn kill(session: &str, events: &Sender<Event>) {
    let mut command = Command::new("zellij");
    command.args(["kill-session", session]);
    run(command, None, events);
}

/// Asks zellij for the clients of the session `look` is of, on a thread of
/// its own, and hands back what it answers.
pub fn look(look: Look, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || looking(&look, &events));
}

/// Asks zellij for the clients of the session `look` is of, on a thread of
/// its own, until it lists one that `before` does not, which is luneta's own
/// client as it attaches, and hands back what it answers then.
pub fn arrive(look: Look, before: Vec<Client>, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || arriving(&look, before, START, &events));
}

/// The clients of `session`, as zellij lists them now, by their ids, or why
/// zellij did not list them, as of a session that does not run, or in time:
/// zellij's CLI waits on every session's server, and one that hangs holds it
/// up for good.
pub fn clients(session: &str) -> Result<Vec<Client>, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    let clients = clients::list(command, session)?;
    Ok(clients.into_iter().map(clients::Client::to_api).collect())
}

/// What luneta asks of zellij once an action has run.
enum Then {
    /// The clients of the session the client is in, for the pane the action
    /// took it to.
    Look(Look),
    /// The client's arrival in the session of the look, from another, and
    /// what to do once it is there.
    Arrive(Look, Option<After>),
}

/// Runs a zellij action on a thread of its own, so that the sidebar goes on
/// drawing while zellij answers, says why when it fails, and then does as
/// `then` says. A client on its way to another session comes there once
/// zellij lists one more client of it than before the action, and then what
/// waits on it is done. A pane already focused is where the action would
/// take the client, and no failure.
fn run(mut command: Command, then: Option<Then>, events: &Sender<Event>) {
    quiet(&mut command);
    let events = events.clone();
    thread::spawn(move || {
        let before = match &then {
            Some(Then::Arrive(look, _)) => clients(&look.session).unwrap_or_default(),
            _ => Vec::new(),
        };
        thread::sleep(NUDGED);
        let failure = match command.output() {
            Ok(output) if output.status.success() => None,
            Ok(output) => Some(failure(&output)).filter(|said| !said.contains("is already focused")),
            Err(error) => Some(unrun(&error)),
        };
        if let Some(said) = &failure {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(said.clone()));
        }
        match then {
            None => {},
            Some(Then::Look(look)) => {
                let went = if failure.is_none() { look.went } else { None };
                looking(&Look { went, ..look }, &events);
            },
            Some(Then::Arrive(look, after)) => {
                let came = failure.is_none() && arriving(&look, before, ARRIVAL, &events);
                if failure.is_none() && !came {
                    let said = "zellij did not take its client there".to_owned();
                    let _ = events.send(Event::Notice(said));
                }
                if !came {
                    let _ = events.send(Event::Stayed(look.number));
                }
                if let Some(after) = after {
                    after.done(came, &events);
                }
            },
        }
    });
}

/// Asks zellij for the clients of the session `look` is of, and hands back
/// what it answers, or why it did not.
fn looking(look: &Look, events: &Sender<Event>) {
    // luneta is on its way out.
    let _ = events.send(match clients(&look.session) {
        Ok(clients) => {
            Event::Looked(Looked { number: look.number, went: look.went, clients, before: None })
        },
        Err(why) => Event::Notice(why),
    });
}

/// Asks zellij for the clients of the session `look` is of until it lists
/// one that `before` does not, for as long as `patience`, and hands back what
/// it answers then. Whether it did.
fn arriving(look: &Look, before: Vec<Client>, patience: Duration, events: &Sender<Event>) -> bool {
    let deadline = Instant::now() + patience;
    loop {
        let came = clients(&look.session).ok().filter(|clients| {
            clients.iter().any(|client| before.iter().all(|was| was.id != client.id))
        });
        if let Some(clients) = came {
            let (number, went) = (look.number, look.went);
            // luneta is on its way out.
            let _ = events.send(Event::Looked(Looked { number, went, clients, before: Some(before) }));
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(ARRIVING);
    }
}

/// Has `command` run without what `LEFT_OUT` names, and with nothing to read.
fn quiet(command: &mut Command) {
    for name in LEFT_OUT {
        command.env_remove(name);
    }
    command.stdin(Stdio::null());
}

/// What zellij said as it failed: the last line it wrote on stderr, and how
/// it ended.
fn failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let line = stderr.lines().rfind(|line| !line.trim().is_empty()).unwrap_or("");
    format!("zellij: {} ({})", line.trim(), output.status)
}

/// Why zellij could not be run.
fn unrun(error: &io::Error) -> String {
    if error.kind() == io::ErrorKind::NotFound {
        return "zellij is not on the PATH".to_owned();
    }
    format!("zellij: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_directory_itself() {
        let named = |dir: &str| session_for(Path::new(dir)).map(|name| name.0);
        assert_eq!(named("/nowhere/at/all/luneta"), Some("luneta".to_owned()));
        assert_eq!(named("/nowhere/at/all/luneta/"), Some("luneta".to_owned()));
        assert_eq!(named("/"), None);
    }

    #[test]
    fn a_worktree_is_named_for_its_repository_too() {
        let named = |path: &str, common: &str| worktree_name(path, common).map(|name| name.0);
        assert_eq!(named("/w/luneta/main", "/w/luneta/.bare"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/trees/main", "/w/luneta/.git"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/infra.git/master", "/w/infra.git"), Some("infra-master".to_owned()));
        assert_eq!(named("/w/app-hotfix", "/w/app/.git"), None);
        assert_eq!(named("/w/application", "/w/app/.git"), Some("app-application".to_owned()));
        let long = "x".repeat(NAME_BYTES_MAX);
        assert_eq!(named(&format!("/w/{long}"), "/w/repo/.git"), None);
    }

    /// A worktree made by git is found one, by the files git leaves in it.
    #[test]
    fn a_linked_worktree_is_found_by_its_git_files() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let common = root.join("luneta").join(".bare");
        let own = common.join("worktrees").join("main");
        let tree = root.join("luneta").join("main");
        for dir in [&own, &tree] {
            assert!(fs::create_dir_all(dir).is_ok());
        }
        let wrote = [
            fs::write(tree.join(".git"), format!("gitdir: {}\n", own.display())),
            fs::write(own.join("commondir"), "../..\n"),
        ];
        assert!(wrote.iter().all(Result::is_ok));
        assert_eq!(session_for(&tree).map(|name| name.0), Some("luneta-main".to_owned()));
        let repository = root.join("luneta");
        assert_eq!(session_for(&repository).map(|name| name.0), Some("luneta".to_owned()));
    }

    #[test]
    fn the_client_leaves_zellij_s_variables_out() {
        let Ok(session) = SessionName::parse("luneta-main") else {
            panic!("a name zellij takes");
        };
        let (program, args) = client(Some(&session));
        assert_eq!(program, "env");
        assert!(args.windows(2).any(|pair| pair == ["-u", "ZELLIJ"]));
        assert_eq!(args[args.len() - 4..], ["zellij", "attach", "--create", "luneta-main"]);
        let (program, args) = rejoin("elo");
        assert_eq!(program, "env");
        assert!(args.windows(2).any(|pair| pair == ["-u", "ZELLIJ"]));
        assert_eq!(args[args.len() - 3..], ["zellij", "attach", "elo"]);
    }

    /// luneta tries the sessions for one to go on in as the picker lists
    /// them: the newest first, and of two that started at once, the one whose
    /// name comes last, as the sidebar has them.
    #[test]
    fn the_refuge_is_sought_in_the_order_the_picker_lists_sessions() {
        let at = |seconds| SystemTime::UNIX_EPOCH + Duration::from_secs(seconds);
        let running = [("b", at(2)), ("c", at(1)), ("a", at(2))];
        let running = running.map(|(name, started)| (name.to_owned(), started)).to_vec();
        assert_eq!(newest_first(running), ["b", "a", "c"]);
        assert!(newest_first(Vec::new()).is_empty());
    }

    #[test]
    fn a_postfix_makes_room_for_itself_between_characters() {
        let Ok(name) = SessionName::parse("luneta-main") else {
            panic!("a name zellij takes");
        };
        assert_eq!(name.with_postfix(2).0, "luneta-main-2");
        let Ok(long) = SessionName::parse(&format!("{}é", "x".repeat(NAME_BYTES_MAX - 3))) else {
            panic!("a name zellij takes");
        };
        let postfixed = long.with_postfix(12);
        assert_eq!(postfixed.0, format!("{}-12", "x".repeat(NAME_BYTES_MAX - 3)));
        assert!(SessionName::parse(&postfixed.0).is_ok());
    }

    fn laid(id: u32, tab_id: u32) -> Laid {
        Laid {
            id,
            is_plugin: false,
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            tab_id,
            title: String::new(),
            terminal_command: None,
        }
    }

    /// The column the plugin's dock keeps in a tab, as a layout of the dock's
    /// gives it.
    fn placeholder(id: u32, tab_id: u32) -> Laid {
        Laid { terminal_command: Some("tail -f /dev/null".to_owned()), ..laid(id, tab_id) }
    }

    /// zellij closes a tab with its last pane that holds it open, its
    /// floating panes and all, and a session with its last tab. A bar holds
    /// none open, a plugin's pane you can go to does, and so does a terminal
    /// another took the place of, which comes back. The column of the plugin's
    /// dock holds none open: its placeholder, shown or hidden, nor the
    /// sidebar that stands in it.
    #[test]
    fn the_last_pane_that_holds_its_tab_open_closes_the_tab_or_the_session() {
        let floating = Laid { is_floating: true, ..laid(5, 0) };
        let bar = Laid { is_plugin: true, is_selectable: false, ..laid(1, 0) };
        let plugin = Laid { is_plugin: true, ..laid(4, 0) };
        let suspended = Laid { is_suppressed: true, ..laid(7, 0) };
        let sidebar = Laid { is_plugin: true, title: SIDEBAR_TITLE.to_owned(), ..laid(3, 0) };
        let hidden = Laid { is_suppressed: true, ..placeholder(2, 0) };
        let other_tab = laid(9, 1);
        let tab = |panes: Vec<u32>| Ok(Closes::Tab { id: 0, panes });
        for (panes, id, closes) in [
            (vec![laid(4, 0), bar.clone(), other_tab.clone()], 4, tab(vec![4])),
            (vec![laid(4, 0), floating.clone(), other_tab.clone()], 4, tab(vec![4, 5])),
            (vec![laid(4, 0), laid(6, 0), other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![laid(4, 0), floating.clone(), other_tab.clone()], 5, Ok(Closes::Pane)),
            (vec![laid(4, 0), plugin, other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![suspended, laid(4, 0), other_tab.clone()], 4, Ok(Closes::Pane)),
            (vec![placeholder(2, 0), laid(4, 0), other_tab.clone()], 4, tab(vec![2, 4])),
            (vec![sidebar, hidden, laid(4, 0), bar.clone()], 4, Ok(Closes::Session)),
            (vec![laid(4, 0), floating, bar], 4, Ok(Closes::Session)),
            (vec![other_tab], 4, Err("pane 4 is gone".to_owned())),
        ] {
            assert_eq!(closing(&panes, id), closes, "pane {id}");
        }
    }

    /// luneta goes to the terminal of a tab it opened, of that tab, and not to
    /// the placeholder a layout of the plugin's dock opens in it first.
    #[test]
    fn a_tab_opened_is_gone_to_on_its_terminal_past_the_dock_s_placeholder() {
        let panes = [laid(1, 0), placeholder(2, 1), laid(3, 1)];
        assert_eq!(terminal_of(&panes, 1), Some(3));
    }

    /// The client leaves out the variable zellij refuses by to attach a pane's
    /// terminal to its own session, and luneta refuses in its place.
    #[test]
    fn a_pane_does_not_attach_to_its_own_session() {
        let Ok(session) = SessionName::parse("outer") else {
            panic!("a name zellij takes");
        };
        assert!(refused(&session, Some("outer")).is_some());
        assert_eq!(refused(&session, Some("other")), None);
        assert_eq!(refused(&session, None), None);
    }
}
