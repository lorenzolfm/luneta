//! What luneta asks of zellij: a client to run beside the sidebar, to take
//! that client to a pane, the clients of a session, and what the menu opens
//! and closes. All luneta knows of zellij's ways is here and in `own`, which
//! finds luneta's client among those, so that a multiplexer of luneta's own
//! can take zellij's place behind it.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::TermMode;
use lunetad::api::{refuge_response, Client};
pub use lunetad::moves::{Ended, Refuge};
use lunetad::cli::{
    answered, done, failure, laid, quiet, tabs, terminal_of, unrun, Laid, ANSWER, LEFT_OUT,
    PANE_VAR, SESSION_VAR,
};
pub use lunetad::closes::{
    closed, closes_now, closes_together, deleted, killed, Close, Closes, Closing,
};
use lunetad::closes::running_in;
use lunetad::opens::tab_opened;
pub use lunetad::names::{named, session_for};
use lunetad::names::kept;
pub use lunetad::opens::{opening, Open, Tab};
use lunetad::{clients, moves, program, Destination};
pub use lunetad::SessionName;

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::{Event, Nudged};
use crate::machines::Machine;
use crate::own::{Before, Look, Looked};

/// What names the pane you asked luneta's ask box from, in the variables of
/// the agent it runs, which runs in no pane: the pane luneta's zellij was on
/// as the turn started, as `<session>:<pane>`, by the name its session had
/// then, not the one it had as the pane opened.
pub const ASKED_VAR: &str = "LUNETA_PANE";

/// A pane of a session, as the CLI names it: `<session>:<pane>`, as `luneta
/// agents` writes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub session: String,
    pub pane: u32,
}

impl fmt::Display for Pane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.session, self.pane)
    }
}

/// A pane as it is spelled: its id after the last `:`, and its session's name
/// before it, which may hold a `:`, as zellij's may.
pub fn pane_address(spelled: &str) -> Result<Pane, String> {
    let pane = spelled.rsplit_once(':').and_then(|(session, pane)| {
        let pane = pane.parse().ok()?;
        (!session.is_empty()).then(|| Pane { session: session.to_owned(), pane })
    });
    pane.ok_or_else(|| format!("{spelled} is no pane: name one as <session>:<pane>"))
}

/// Where a command of luneta's runs, as its variables say.
#[derive(Debug, PartialEq, Eq)]
pub enum Seat {
    /// In a pane of zellij, by the name its session had as the pane opened,
    /// which a rename leaves behind.
    Opened { named: String, pane: u32 },
    /// In no pane, as the agent of luneta's ask box runs, by the pane you
    /// asked from, by the name its session had as you asked.
    Asked(Pane),
}

/// Where this runs, as zellij names it in the variables of what runs in its
/// panes, or luneta's ask box in those of its agent: none outside both. Or
/// why the box's does not read, or names a pane where zellij's name another:
/// nothing runs in one pane and asks from another.
pub fn here() -> Result<Option<Seat>, String> {
    let (asked, session, pane) = (var(ASKED_VAR)?, var(SESSION_VAR)?, var(PANE_VAR)?);
    seat_in(asked.as_deref(), session.as_deref(), pane.as_deref())
}

/// The variable `name` of this process: none where it is not set, or why it
/// does not read.
pub fn var(name: &str) -> Result<Option<String>, String> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => Err(format!("{name} does not read")),
    }
}

/// The seat `asked` names, as `ASKED_VAR` does, or else the one zellij's
/// `session` and `pane` do, or none.
fn seat_in(
    asked: Option<&str>,
    session: Option<&str>,
    pane: Option<&str>,
) -> Result<Option<Seat>, String> {
    match (asked, session.filter(|session| !session.is_empty())) {
        (Some(asked), Some(session)) => Err(format!(
            "{ASKED_VAR} names {asked}, a pane luneta's ask box asked from, and this runs in a \
             pane of {session}"
        )),
        (Some(asked), None) => {
            let asked = pane_address(asked).map_err(|why| format!("{ASKED_VAR}: {why}"))?;
            Ok(Some(Seat::Asked(asked)))
        },
        (None, Some(named)) => {
            let pane = pane.and_then(|pane| pane.parse().ok());
            Ok(pane.map(|pane| Seat::Opened { named: named.to_owned(), pane }))
        },
        (None, None) => Ok(None),
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
    (current == Some(session.as_str())).then(|| {
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
        Some(session) => client_with(&["attach", "--create", session.as_str()]),
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

/// The program luneta runs beside the sidebar for a client of `session` on
/// `destination`, which runs, and its arguments: zellij's client there,
/// through ssh, in a terminal ssh asks for there. It goes on the connection
/// luneta's daemon reaches that machine's on, as `lunetad::sharing` says,
/// where luneta's runtime folder is one ssh can name. ssh may ask for a
/// password or a host's key in luneta's terminal.
pub fn attach_away(destination: &Destination, session: &str) -> (String, Vec<String>) {
    away(destination, format!("zellij attach {}", quoted(session)))
}

/// The program luneta runs beside the sidebar for a client that makes
/// `session` on `destination`, in `cwd`, or where ssh starts it there, and
/// its arguments, as `attach_away` says: zellij's client makes a session in
/// the directory it starts in, and attaches to one that runs by that name,
/// or that ended, which it brings back, instead.
pub fn make_away(
    destination: &Destination,
    session: &str,
    cwd: Option<&str>,
) -> (String, Vec<String>) {
    let cd = cwd.map(|cwd| format!("cd {} && ", quoted(cwd))).unwrap_or_default();
    away(destination, format!("{cd}zellij attach --create {}", quoted(session)))
}

/// ssh, to run `command` on `destination`, in a terminal it asks for there,
/// on the connection luneta's daemon reaches that machine's on, as
/// `attach_away` says.
fn away(destination: &Destination, command: String) -> (String, Vec<String>) {
    // Nor does ssh say, at the info level it leaves out, that the connection
    // closed: the last line on its screen is what went wrong, if anything.
    let mut args = ["-t", "-o", "LogLevel=ERROR"].map(str::to_owned).to_vec();
    let sharing = lunetad::folder::find().map(|folder| lunetad::sharing(&folder, destination));
    // An option that is no UTF-8, as of a folder whose path is not, leaves
    // them all out.
    let sharing = sharing.and_then(|options| {
        options.into_iter().map(OsString::into_string).collect::<Result<Vec<_>, _>>().ok()
    });
    args.extend(sharing.into_iter().flatten());
    args.push(destination.to_string());
    // ssh hands the command to the shell there, which reads it as words.
    args.push(command);
    ("ssh".to_owned(), args)
}

/// `word` as a shell there reads it back, sh or fish: in single quotes, with
/// a quote or a backslash in it left out of them, after a backslash, which
/// both read alike there. fish would read either inside them as an escape.
fn quoted(word: &str) -> String {
    let mut quoted = String::from("'");
    for char in word.chars() {
        match char {
            '\'' | '\\' => {
                quoted.push('\'');
                quoted.push('\\');
                quoted.push(char);
                quoted.push('\'');
            },
            char => quoted.push(char),
        }
    }
    quoted.push('\'');
    quoted
}

/// Where luneta's zellij goes on once its client ended in one of `seats`,
/// the sessions it may have been in, as `lunetad::moves::refuge` finds it on
/// this machine: never the session luneta's terminal is a pane of, as
/// `refusal` says. None where the client's session runs still, as it
/// detached, or none answers.
pub fn refuge(seats: &[SessionName]) -> Option<Refuge> {
    let current = env::var(SESSION_VAR).ok();
    let folder = lunetad::zellij_folder();
    let ended = moves::refuge(&folder, seats, current.as_deref(), |session| {
        clients(session.as_str()).ok()
    });
    let Ended::Went(found) = ended else {
        return None;
    };
    found
}

/// This machine's newest session, as `lunetad::moves::newest` finds it,
/// where luneta's zellij goes on once its client ended on another machine:
/// never the session luneta's terminal is a pane of.
pub fn newest() -> Option<Refuge> {
    let current = env::var(SESSION_VAR).ok();
    moves::newest(&lunetad::zellij_folder(), current.as_deref(), |session| {
        clients(session.as_str()).ok()
    })
}

/// What became of the session luneta's zellij's client ended in, of
/// `seats`, on `destination`, as its daemon there finds it, and where it
/// goes on there; or why that daemon did not say, or said what luneta does
/// not read.
pub fn ended_away(destination: &Destination, seats: &[SessionName]) -> Result<Ended, String> {
    let found = Away::reach(destination, away::ANSWER)?.refuge(seats)?;
    match found.ended {
        Some(refuge_response::Ended::Kept(_)) => Ok(Ended::Kept),
        Some(refuge_response::Ended::Went(went)) => {
            let refuge = went.refuge.map(|refuge| -> Result<Refuge, String> {
                let session = SessionName::running(&refuge.session).map_err(|why| {
                    format!("{destination}: lunetad went on in \"{}\": {why}", refuge.session)
                })?;
                Ok(Refuge { session, before: refuge.before })
            });
            Ok(Ended::Went(refuge.transpose()?))
        },
        // A newer daemon's.
        None => Err(format!(
            "{destination}: its lunetad says what became of the session in words this luneta \
             cannot read, as a newer one does"
        )),
    }
}

/// What luneta types into zellij's client before it asks zellij to move it,
/// and ahead of that, as the keys or a press of the mouse come to luneta,
/// where the client typed nothing in its session yet: `ScrollLock` let go,
/// as the kitty keyboard protocol spells it, with no modifiers and the event
/// of a release, `1:3`. zellij moves the client of a session that last sent
/// it a key, and a client that has sent none since it came to the session,
/// as luneta's has not once the sidebar took it there, moves for no action.
/// The client counts a key let go as a key. No one binds `ScrollLock`, and
/// zellij writes nothing of it to a pane that does not read keys as kitty
/// spells them. One that does gets it as it came, a release it
/// never asked to hear of, and passes it over: nvim does, where the key
/// pressed would have it insert a character of the protocol's own in the
/// text. The client reads it as kitty spells it unless its config turns the
/// protocol off: then zellij takes it for no key, and hands the pane
/// nothing. So `nudge` gives it only to a client that asks its terminal for
/// kitty's keys, and zellij moves one that does not only once you type in
/// it. As you rename a tab or a pane, zellij takes a key it binds nothing to
/// for text, and writes the nudge's, `[57359;1:3u`, into the name: only a
/// client that typed in its session can be renaming there.
const NUDGE: &[u8] = b"\x1b[57359;1:3u";

/// `NUDGE`, where zellij's client reads it as a key: in `mode`, it asked its
/// terminal, luneta's, for keys as kitty spells them.
pub fn nudge(mode: TermMode) -> Option<&'static [u8]> {
    mode.contains(TermMode::DISAMBIGUATE_ESC_CODES).then_some(NUDGE)
}

/// How long an action waits, after the client typed `NUDGE` or another key,
/// for zellij to take it in, as it goes to zellij through its client while
/// the action goes straight to its server.
pub const NUDGED: Duration = Duration::from_millis(30);

/// How long luneta waits for its client to come to a session it sent it to
/// from another, after zellij took the action, before it takes the client
/// for one that stayed. It comes within milliseconds.
const ARRIVAL: Duration = Duration::from_secs(2);

/// How long luneta waits for its client to come to the session it attaches
/// to as it starts, which zellij can take a while to make.
pub const START: Duration = Duration::from_secs(10);

/// How often luneta asks zellij for the clients of a session its client is
/// on its way to.
const ARRIVING: Duration = Duration::from_millis(20);

/// Takes the client from the session it was `nudged` in to the pane `pane`
/// of `to`, or to the session `to` alone, on the pane it last had the focus
/// on, and then takes the look `then`, if any: of the session it was in, for
/// the pane, or of `to`, for the client's arrival there, after which it does
/// `after`. zellij takes the action once it has taken in the key that makes
/// the client the one it moves, as `nudged` says, on the machine it was
/// nudged on, whose session `to` is too. What zellij says of a failure comes
/// back as a notice.
pub fn go(
    nudged: &Nudged,
    to: &SessionName,
    pane: Option<u32>,
    then: Option<Look>,
    after: Option<After>,
    events: &Sender<Event>,
) {
    let at = nudged.session();
    if at == to && pane.is_none() {
        return;
    }
    let then = match (then, after) {
        (Some(look), after) if at != to => Some(Then::Arrive(look, after)),
        (then, None) => then.map(Then::Look),
        // Nothing would say when the client is there.
        (_, Some(after)) => {
            after.untold(events);
            return;
        },
    };
    let (on, at, to) = (nudged.on().clone(), at.clone(), to.clone());
    run(Action::Go { on, at, to, pane }, then, nudged.until(), events);
}

/// Takes the client from the session it was `nudged` in, as `go` says, on
/// the machine it was nudged on, to a session it makes, `to`, that starts in
/// `cwd`, or where the client started, and then takes the look `then`, if
/// any, for the client's arrival there, after which it opens the tab `tab`
/// there, if any, for the menu owed what it opens: a tab of this machine's.
/// A session that runs under that name is where it goes, and `cwd` goes
/// unused; one that ended, which zellij kept to bring back, comes back as it
/// was: `makes` says first whether zellij would make it, on this machine,
/// and the daemon of another named it for one it would.
///
/// Unless zellij's sockets here say one runs by that name, as they do not of
/// one the sidebar names, nor of another machine's, zellij makes it, and must
/// take the client there before it is asked anything, as `Before::Made`
/// says: the look comes back as the client's arrival, for luneta to ask for
/// once zellij titles its terminal in that session, with the tab, which
/// takes the place of zellij's there, as `Pending` says.
pub fn start(
    nudged: &Nudged,
    to: &SessionName,
    cwd: Option<&str>,
    then: Option<Look>,
    tab: Option<(Due<u32>, Tab)>,
    events: &Sender<Event>,
) -> Option<Arrival> {
    let on = nudged.on().clone();
    let runs = match on {
        Machine::Local => runs(to.as_str()),
        Machine::Remote(_) => None,
    };
    let (at, cwd) = (nudged.session().clone(), cwd.map(str::to_owned));
    let command = Action::Start { on: on.clone(), at, to: to.clone(), cwd };
    let when = nudged.until();
    let asked = SystemTime::now();
    match (then, tab) {
        (Some(look), tab) if runs != Some(true) => {
            let waits = match (on, tab) {
                (Machine::Local, Some((owed, tab))) => {
                    Waits::Made(Pending::new(owed, tab, to, asked, events))
                },
                (on, None) => Waits::After(on, None),
                (Machine::Remote(_), Some((owed, _))) => {
                    owed.pay(Err(TAB_HERE.to_owned()));
                    return None;
                },
            };
            run(command, Some(Then::Made(look.number)), when, events);
            Some(Arrival { look, before: Before::Made, waits })
        },
        (Some(look), tab) => {
            let after = tab.map(|(owed, tab)| After::tab(owed, to, tab));
            run(command, Some(Then::Arrive(look, after)), when, events);
            None
        },
        (None, None) => {
            run(command, None, when, events);
            None
        },
        // Nothing would say when the client is there.
        (None, Some((owed, tab))) => {
            After::tab(owed, to, tab).untold(events);
            None
        },
    }
}

/// Why a session luneta makes opens a tab on this machine alone: its CLI
/// opens the tab, as `Pending` says, while luneta's zellij is on it.
pub const TAB_HERE: &str =
    "luneta makes a session for a tab on this machine alone, while its zellij is on it";

/// What the CLI hears of a client it sent somewhere, where nothing typed in
/// the session it sent it from: zellij takes such an action as done, and
/// moves nobody.
const UNMOVED: &str = "zellij moved nobody: it moves the client that last typed in the session \
                       it is asked from, and none has typed there since it came";

/// Takes the client that last typed in the session `at` to the pane `pane`
/// of `to`, or to `to` alone, on the pane it last had the focus on, as the
/// CLI does. Waits for the client to be there, and says why it is not: as
/// `UNMOVED` says, or as zellij said.
pub fn taken(at: &str, to: &str, pane: Option<u32>) -> Result<(), String> {
    let mut command = Command::new("zellij");
    let pane_id = pane.map(|id| format!("terminal_{id}"));
    if at == to {
        let (Some(pane), Some(pane_id)) = (pane, pane_id) else {
            return Ok(());
        };
        command.args(["--session", at, "action", "focus-pane-id", &pane_id]);
        match done(command) {
            Err(said) if !said.contains("is already focused") => return Err(said),
            _ => {},
        }
        let on = |clients: Vec<Client>| clients.iter().any(|client| client.pane == Some(pane));
        return seen(at, |clients| on(clients).then_some(())).ok_or_else(|| UNMOVED.to_owned());
    }
    let before = clients(to)?;
    command.args(["--session", at, "action", "switch-session", to]);
    command.args(pane_id.iter().flat_map(|pane| ["--pane-id", pane.as_str()]));
    done(command)?;
    let new = |client: &Client| before.iter().all(|was| was.id != client.id);
    let came = seen(to, |clients| clients.into_iter().find(new)).ok_or_else(|| UNMOVED.to_owned())?;
    match pane {
        Some(pane) if came.pane != Some(pane) => {
            Err(format!("zellij took the client to {to}, but not to pane {pane}"))
        },
        _ => Ok(()),
    }
}

/// Makes the session `to`, starting in `cwd`, and takes the client that last
/// typed in the session `at` there. zellij's client makes it, so that it has
/// the client's environment, not the CLI's. zellij takes the client to a
/// session that runs by that name instead, and makes none, as `made_since`
/// tells.
///
/// A session made so is there once its socket is: no client but the one sent
/// would make it, and zellij's server, asked anything before its first client
/// is on, can fail.
pub fn made(at: &str, to: &SessionName, cwd: &str) -> Result<(), String> {
    let mut command = Command::new("zellij");
    command.args(["--session", at, "action", "switch-session", to.as_str(), "--cwd", cwd]);
    let asked = SystemTime::now();
    done(command)?;
    match made_since(to.as_str(), asked, START) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("{to} ran already, so zellij made none")),
        None => Err(UNMOVED.to_owned()),
    }
}

/// Whether the session named `session`, once one runs within `patience`,
/// started at `asked` or after, by its socket: one zellij made as luneta
/// asked, rather than one that ran already, under that name or another,
/// renamed to it since, as a rename keeps the socket's time. A socket's time
/// is of the kernel's coarse clock, a tick behind at most, and zellij takes
/// far longer than a tick to make a session. None where none runs.
fn made_since(session: &str, asked: SystemTime, patience: Duration) -> Option<bool> {
    started(session, patience).map(|started| started >= asked)
}

/// What `seen` makes of the clients of `session`, once it makes anything of
/// them, asking zellij every `ARRIVING` for as long as `ARRIVAL`: zellij
/// takes an action a moment after it answers it.
fn seen<T>(session: &str, seen: impl Fn(Vec<Client>) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + ARRIVAL;
    loop {
        if let Some(seen) = clients(session).ok().and_then(&seen) {
            return Some(seen);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(ARRIVING);
    }
}

/// When the session named `session` started, by its socket, once one runs
/// within `patience`. A folder it cannot read yet is one it has yet to see the
/// socket in.
fn started(session: &str, patience: Duration) -> Option<SystemTime> {
    let folder = lunetad::zellij_folder();
    let deadline = Instant::now() + patience;
    loop {
        let running = lunetad::zellij_sessions(&folder).unwrap_or_default();
        if let Some((_, started)) = running.into_iter().find(|(name, _)| name == session) {
            return Some(started);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(ARRIVING);
    }
}

/// What luneta has zellij do once its client has come to the session a go
/// takes it to.
pub enum After {
    /// Open this, as `open` does, for the menu owed what it opens: told once
    /// done, or else as it is dropped undone.
    Open(Due<u32>, Open),
    /// Kill the session the client left, which it was in, the one that
    /// started at `started`, and delete it.
    Kill { session: String, started: SystemTime },
}

impl After {
    /// Open `tab` in `session`, for the menu `owed` what it opens.
    fn tab(owed: Due<u32>, session: &SessionName, tab: Tab) -> Self {
        After::Open(owed, Open::Tab { session: session.clone(), tab })
    }

    /// Says that nothing would tell when the client is there, so that it is
    /// not done: to the menu owed what it opens, or as a notice.
    pub fn untold(self, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) => {
                owed.pay(Err("luneta cannot tell when zellij's client is there".to_owned()));
            },
            After::Kill { session, .. } => {
                let when = format!("when zellij's client has left \"{session}\", which stays");
                // luneta is on its way out.
                let _ = events.send(Event::Notice(format!("luneta cannot tell {when}")));
            },
        }
    }

    /// Does it, now that the client is there, by zellij `there`, on the
    /// machine the client came to.
    fn came(self, there: &mut There, events: &Sender<Event>) {
        match (self, there) {
            (After::Open(owed, open), there) => owed.pay(there.open(&open)),
            // A zellij of its own kills it: one run as the client's would
            // wait on the session it ends.
            (After::Kill { session, started }, There::Here) => {
                kill(&Machine::Local, &session, started, events);
            },
            // The daemon there kills it, which waits on no client.
            (After::Kill { session, started }, There::Away(away)) => {
                if let Err(why) = away.kill(&session, started) {
                    // luneta is on its way out.
                    let _ = events.send(Event::Notice(why));
                }
            },
        }
    }

    /// Says why it is not done: the client did not come.
    fn stayed(self, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) => {
                owed.pay(Err("zellij did not take its client there".to_owned()));
            },
            After::Kill { session, .. } => {
                // luneta is on its way out.
                let _ = events.send(Event::Notice(format!("\"{session}\" stays")));
            },
        }
    }
}

/// Opens `open`, on the machine `on`, on a thread of its own, for the menu
/// numbered `menu`, with no client moved, and hands back the terminal it
/// opened, for luneta to take its client to, or why it opened none, as
/// `owed` does.
pub fn open(menu: u64, on: &Machine, open: Open, events: &Sender<Event>) {
    let (owed, on) = (owed(menu, events), on.clone());
    thread::spawn(move || owed.pay(There::on(&on).and_then(|mut there| there.open(&open))));
}

/// What the menu numbered `menu` is owed, as it waits on zellij to open what
/// a key asked for: the terminal it opened, or why none, as `Due` says.
pub fn owed(menu: u64, events: &Sender<Event>) -> Due<u32> {
    Due::new(events, move |opened| Event::Opened { menu, opened })
}

/// Opens `tab`, as `opening` does, in `to`, a session zellij made for it, in
/// place of the tabs zellij made the session with, which luneta closes, so
/// that the session holds the tab alone. They are those zellij lists before
/// the tab opens, and they hold what its layout laid out and no more, as far
/// as luneta can tell: nothing zellij has says so, but the session is new,
/// and the menu has the keys until the tab opens, so that only a tab another
/// client or an agent opened there by then goes with them. What zellij says
/// of a tab it did not close comes back as a notice.
fn alone(to: &SessionName, tab: &Tab, events: &Sender<Event>) -> Result<u32, String> {
    let made = tabs(&laid(to.as_str())?);
    let (opened, laid) = tab_opened(to, tab)?;
    let terminal = terminal_of(&laid, opened).ok_or("zellij opened a tab with no pane")?;
    for made in made {
        if let Err(why) = closed(to.as_str(), Close::Tab(made)) {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(why));
        }
    }
    Ok(terminal)
}

/// Asks zellij on the machine `on`, on a thread of its own, what closing the
/// terminal `pane` of `session` closes, for the menu numbered `menu`, and
/// hands back what it answers.
pub fn closes(menu: u64, on: &Machine, session: SessionName, pane: u32, events: &Sender<Event>) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        let closes = There::on(&on).and_then(|mut there| there.closes(&session, pane));
        // luneta is on its way out.
        let _ = events.send(Event::Closes { menu, closes });
    });
}

/// Has zellij on the machine `on` close `close` of `session`, on a thread of
/// its own, at once: it moves no client. What zellij says of a failure comes
/// back as a notice.
pub fn close(on: &Machine, session: &str, close: Close, events: &Sender<Event>) {
    let (on, session, events) = (on.clone(), session.to_owned(), events.clone());
    thread::spawn(move || {
        if let Err(why) = There::on(&on).and_then(|mut there| there.close(&session, close)) {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(why));
        }
    });
}

/// Has zellij on the machine `on` kill `session`, the one that started at
/// `started`, and delete it, on a thread of its own, as `killed` does: the
/// thread, for luneta to wait for as it ends.
pub fn kill(
    on: &Machine,
    session: &str,
    started: SystemTime,
    events: &Sender<Event>,
) -> JoinHandle<()> {
    let (on, session, events) = (on.clone(), session.to_owned(), events.clone());
    thread::spawn(move || {
        if let Err(why) = There::on(&on).and_then(|mut there| there.kill(&session, started)) {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(why));
        }
    })
}

/// The sessions that run, as `running_in` says of zellij's folder.
pub fn running() -> Result<Vec<(String, SystemTime)>, String> {
    running_in(&lunetad::zellij_folder())
}

/// What `list-sessions` says where no session runs, nor ended to be brought
/// back, as it fails.
const NO_SESSIONS: &str = "No active zellij sessions found.";

/// What follows the name of a session `list-sessions` lists that ended, and
/// that zellij keeps to bring back.
const EXITED: &str = "(EXITED - attach to resurrect)";

/// A session that ended, which zellij keeps to bring back: `switch-session`
/// brings it back, and `delete-session` lets it go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exited {
    pub name: SessionName,
    /// How long before zellij listed it it was made, as zellij says: to the
    /// second.
    pub ago: Duration,
}

/// The sessions zellij keeps to bring back, as `list-sessions` lists them
/// now, or why it did not list them, as in time: zellij's CLI waits on every
/// session's server, and one that hangs would hold it up for good.
pub fn exited() -> Result<Vec<Exited>, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    command.args(["list-sessions", "--no-formatting"]);
    let ran = program::run(&mut command, ANSWER).map_err(|error| unrun(&error))?;
    if let program::Ran::Ended { stderr, .. } = &ran
        && String::from_utf8_lossy(stderr).contains(NO_SESSIONS)
    {
        return Ok(Vec::new());
    }
    let listed = ran.answer().map_err(|why| format!("zellij: {why}"))?;
    Ok(exited_in(&String::from_utf8_lossy(&listed)))
}

/// The sessions that ended among those `listed` lists, a line each, as
/// `list-sessions --no-formatting` lists them: `<name> [Created 1day 2h 3m
/// 4s ago] (EXITED - attach to resurrect)`. A line that does not read so,
/// or of a name zellij would not take, is left out.
fn exited_in(listed: &str) -> Vec<Exited> {
    let exited = |line: &str| {
        let line = line.trim_end().strip_suffix(EXITED)?.trim_end();
        let (name, created) = line.rsplit_once(" [Created ")?;
        let ago = created.strip_suffix(" ago]").and_then(spelled_ago)?;
        Some(Exited { name: SessionName::running(name).ok()?, ago })
    };
    listed.lines().filter_map(exited).collect()
}

/// A time as zellij spells one, `1day 2h 3m 4s`, in whole seconds: what it
/// says under a second goes, and a unit it has no word for, nothing.
fn spelled_ago(spelled: &str) -> Option<Duration> {
    let mut secs: u64 = 0;
    for word in spelled.split_whitespace() {
        let digits = word.find(|c: char| !c.is_ascii_digit())?;
        let (count, unit) = word.split_at(digits);
        let each = match unit {
            "years" | "year" => 31_557_600,
            "months" | "month" => 2_630_016,
            "weeks" | "week" | "w" => 604_800,
            "days" | "day" | "d" => 86_400,
            "h" => 3600,
            "m" => 60,
            "s" => 1,
            "ms" | "us" | "ns" => 0,
            _ => return None,
        };
        secs = secs.checked_add(count.parse::<u64>().ok()?.checked_mul(each)?)?;
    }
    Some(Duration::from_secs(secs))
}

/// Renames `session` to `to`, or says why zellij did not. A session with no
/// client takes the name as well as one with.
pub fn renamed(session: &SessionName, to: &SessionName) -> Result<(), String> {
    relisted_in(&lunetad::zellij_folder(), session)?;
    let mut command = Command::new("zellij");
    command.args(["--session", session.as_str(), "action", "rename-session", to.as_str()]);
    done(command)
}

/// What zellij's web client says to its server to have it read again which
/// sessions run and which ended, framed as zellij frames a message: four
/// bytes of length, low first, then a `ClientToServerMsg` of zellij-utils'
/// `client_to_server.proto` that holds an empty `RequestSessionList`, its
/// field 25.
const RELIST: [u8; 7] = [3, 0, 0, 0, 0xca, 0x01, 0];

/// What zellij's CLI says to the server of each session to learn that it
/// runs, so framed: a `ClientToServerMsg` that holds an empty `ConnStatus`,
/// its field 13.
const CONN_STATUS: [u8; 6] = [2, 0, 0, 0, 0x6a, 0];

/// What the server answers `CONN_STATUS` with, to the client that sent it
/// alone, without the length before it: a `ServerToClientMsg` that holds an
/// empty `Connected`, its field 4.
const CONNECTED: [u8; 2] = [0x22, 0];

/// Has the server of `session`, where its socket is in `folder`, zellij's,
/// read again which sessions run and which ended, or says why it did not.
/// zellij 0.45.1 refuses to rename a session to a name among those it read
/// last, and reads them only as a web client or a plugin asks, so a session
/// renamed could not take its old name back: zellij still had it as its own.
/// The server takes a client's messages in turn, and answers `CONN_STATUS`,
/// sent after `RELIST`, to that client alone, so once it has read them and
/// handed them on, ahead of a rename asked after. What it sends the client
/// before that, as it sends some to every client it has, luneta reads past.
/// It skips `RELIST` where it cannot read it, though, and reads none before
/// its first client: then a name it had stays taken, and its refusal says
/// so, as it did before luneta asked this. Where no session of the contract
/// `RELIST` is of has the name, the rename is left to say why, as zellij's
/// CLI speaks its own.
fn relisted_in(folder: &Path, session: &SessionName) -> Result<(), String> {
    let socket = folder.join(lunetad::ZELLIJ_CONTRACT).join(session.as_str());
    let failed = |error: io::Error| format!("zellij did not list its sessions again: {error}");
    let mut stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        // One of a zellij of another contract, or one that ended.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed(error)),
    };
    // macOS sets no option of a socket its peer closed, as the server closes
    // this one once it answered, though what it sent is still there to read.
    stream.set_read_timeout(Some(ANSWER)).map_err(failed)?;
    stream.write_all(&RELIST).map_err(failed)?;
    stream.write_all(&CONN_STATUS).map_err(failed)?;
    connected(&mut stream, Instant::now() + ANSWER).map_err(failed)
}

/// Reads the messages of the server on `stream`, each its length and then
/// itself, as zellij frames them, until it says `CONNECTED`, starting no
/// read past `deadline`; or says why it did not. Each read waits as long as
/// the stream's timeout at most.
fn connected(stream: &mut UnixStream, deadline: Instant) -> io::Result<()> {
    let mut part = [0; 4096];
    loop {
        let mut length = [0; 4];
        filled(stream, &mut length, deadline)?;
        let length = u32::from_le_bytes(length) as usize;
        let mut left = length;
        while left > 0 {
            let read = left.min(part.len());
            filled(stream, &mut part[..read], deadline)?;
            left -= read;
        }
        if length == CONNECTED.len() && part[..length] == CONNECTED {
            return Ok(());
        }
    }
}

/// Fills `buffer` with what comes next on `stream`, starting no read past
/// `deadline`, or says why it did not.
fn filled(stream: &mut UnixStream, buffer: &mut [u8], deadline: Instant) -> io::Result<()> {
    let late = || io::Error::new(io::ErrorKind::TimedOut, "it did not answer in time");
    let mut at = 0;
    while at < buffer.len() {
        if Instant::now() >= deadline {
            return Err(late());
        }
        match stream.read(&mut buffer[at..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "it hung up")),
            Ok(read) => at += read,
            // A signal luneta took on this thread: a read with a timeout is
            // not taken up again after one.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Err(late()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// The terminal of `session` a client coming there would be on, as zellij
/// lists its panes now: in its first tab, the one focused there, or else the
/// first, a tiled one before one floating. zellij says which tab a session
/// is on only while a client is on it.
pub fn focused(session: &str) -> Result<u32, String> {
    let laid = laid(session)?;
    shown(&laid).ok_or_else(|| format!("{session} has no terminal to show"))
}

/// The terminal among `laid`, the panes of a session, that a client coming
/// there would be on, as `focused` says.
fn shown(laid: &[Laid]) -> Option<u32> {
    let terminal = |laid: &&Laid| !laid.is_plugin && laid.is_selectable && !laid.is_suppressed;
    let rank = |laid: &&Laid| (laid.tab_position, laid.is_floating, !laid.is_focused);
    laid.iter().filter(terminal).min_by_key(rank).map(|laid| laid.id)
}

/// What the terminal `pane` of `session` shows, as zellij dumps it: its
/// lines, in the colours it shows them in.
pub fn dumped(session: &str, pane: u32) -> Result<String, String> {
    let mut command = Command::new("zellij");
    quiet(&mut command);
    let pane = format!("terminal_{pane}");
    command.args(["--session", session, "action", "dump-screen", "--pane-id", &pane, "--ansi"]);
    answered(&mut command)
}

/// Asks zellij on `on`, the machine luneta's client runs on, for the
/// clients of the session `look` is of, on a thread of its own, and hands
/// back what it answers.
pub fn look(on: &Machine, look: Look, events: &Sender<Event>) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || match There::on(&on) {
        Ok(mut there) => looking(&mut there, &look, &events),
        // luneta is on its way out.
        Err(why) => drop(events.send(Event::Notice(why))),
    });
}

/// luneta's client on its way to the session of `look`, on the machine
/// `waits` is of, which zellij lists once it is there: as a client of it that
/// was not there `before`. What waits on it is done once it is there, or not,
/// when it does not come.
pub struct Arrival {
    pub look: Look,
    before: Before,
    waits: Waits,
}

/// The machine luneta's client comes to, and what waits on its arrival
/// there.
enum Waits {
    /// On that machine: what a go does once the client is there, if
    /// anything, as `After` says.
    After(Machine, Option<After>),
    /// On this machine: a tab of a session zellij makes for the client, as
    /// `start` says, which this machine's CLI opens, and whose making its
    /// sockets tell.
    Made(Pending),
}

impl Arrival {
    /// luneta's client on its way to the session of `look`, on `on`, with
    /// `after` waiting on it, if anything.
    pub fn new(on: Machine, look: Look, before: Before, after: Option<After>) -> Self {
        Arrival { look, before, waits: Waits::After(on, after) }
    }

    /// Whether the session's server may have yet to take its first client as
    /// luneta's comes, as `Before::fresh` says.
    pub fn fresh(&self) -> bool {
        self.before.fresh()
    }
}

/// The tab that waits on luneta's client's arrival in `to`, a session zellij
/// makes for it, for the menu owed what it opens. It opens once: as the
/// client comes, or else as luneta stops waiting on it, however it stops, as
/// it drops it, so that the menu hears either way.
pub struct Pending {
    tab: Option<(Due<u32>, Tab)>,
    to: SessionName,
    /// When luneta asked zellij to make `to`: as zellij's sockets did not say
    /// one ran by that name, which one that did by then would have.
    asked: SystemTime,
    events: Sender<Event>,
}

impl Pending {
    fn new(
        owed: Due<u32>,
        tab: Tab,
        to: &SessionName,
        asked: SystemTime,
        events: &Sender<Event>,
    ) -> Self {
        Pending { tab: Some((owed, tab)), to: to.clone(), asked, events: events.clone() }
    }

    /// Opens the tab, now that the client is there, in place of the tabs
    /// zellij made the session with, as `alone` says, where zellij made it;
    /// beside them where it did not, as `made` says; or says why not when it
    /// did not come.
    fn done(mut self, came: bool) {
        let Some((owed, tab)) = self.tab.take() else {
            return;
        };
        match came.then(|| self.made()) {
            Some(Ok(())) => owed.pay(alone(&self.to, &tab, &self.events)),
            Some(Err(why)) => {
                let _ = self.events.send(Event::Notice(why));
                After::tab(owed, &self.to, tab).came(&mut There::Here, &self.events);
            },
            None => After::tab(owed, &self.to, tab).stayed(&self.events),
        }
    }

    /// Whether zellij made the session as luneta asked, by its socket, as
    /// `made_since` tells, or why luneta takes it for one that ran already,
    /// which keeps its tabs: zellij took the client to a session renamed to
    /// that name after luneta looked.
    fn made(&self) -> Result<(), String> {
        let to = &self.to;
        match made_since(to.as_str(), self.asked, Duration::ZERO) {
            Some(true) => Ok(()),
            Some(false) => Err(format!("{to} ran already, and keeps its tabs")),
            None => {
                Err(format!("luneta cannot tell whether zellij made {to}, which keeps its tabs"))
            },
        }
    }
}

impl Drop for Pending {
    /// luneta stopped waiting on the client, and did not hear it come.
    fn drop(&mut self) {
        if let Some((owed, tab)) = self.tab.take() {
            After::tab(owed, &self.to, tab).stayed(&self.events);
        }
    }
}

/// Asks zellij for the clients of the session of `arrival`'s look, on a
/// thread of its own, until it lists one that was not there before, which is
/// luneta's own client as it comes, and hands back what it answers then, and
/// then does what waits on it. A client that comes to a session zellij makes
/// is to be in it first, as zellij says by drawing for it: asked sooner,
/// zellij can end that session, as `Before::Made` says.
pub fn arrive(arrival: Arrival, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let Arrival { look, before, waits } = arrival;
        let on = match &waits {
            Waits::After(on, _) => on.clone(),
            Waits::Made(_) => Machine::Local,
        };
        let mut there = There::on(&on)
            .inspect_err(|why| {
                // luneta is on its way out.
                let _ = events.send(Event::Notice(why.clone()));
            })
            .ok();
        let came = there.as_mut().is_some_and(|there| {
            arriving(there, &look, before, START, &events)
        });
        match (waits, there.filter(|_| came)) {
            (Waits::Made(pending), _) => pending.done(came),
            (Waits::After(_, Some(after)), Some(mut there)) => after.came(&mut there, &events),
            (Waits::After(_, Some(after)), None) => after.stayed(&events),
            (Waits::After(_, None), _) => {},
        }
    });
}

/// What luneta knows of the clients of `session` before its own comes there:
/// none of one that does not run, as zellij's sockets say, which zellij is to
/// make, and otherwise what zellij lists, if it does.
pub fn before(session: &str) -> Before {
    if runs(session) == Some(false) {
        return Before::Made;
    }
    clients(session).map_or(Before::Unknown, Before::Listed)
}

/// A name zellij on `on` would make a session of, rather than take a client
/// to one that has it: no session that runs there has it, nor one that
/// ended, which zellij keeps to bring back, as zellij there said just now.
pub struct Unused {
    on: Machine,
    name: SessionName,
}

impl Unused {
    pub fn on(&self) -> &Machine {
        &self.on
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }
}

/// The name a session luneta makes for `dir` on this machine takes, as
/// `lunetad::names::free_for` names it, or why it takes none.
pub fn unused_for(dir: &str) -> Result<Unused, String> {
    lunetad::names::free_for(dir).map(|name| Unused { on: Machine::Local, name })
}

/// The name a session luneta makes for `dir` on `destination` takes, as its
/// daemon there names it within `away::QUICK`, as luneta's screen waits on
/// it, or why it takes none.
pub fn name_away(destination: &Destination, dir: &str) -> Result<Unused, String> {
    let named = Away::reach(destination, away::QUICK).and_then(|mut away| away.name(dir));
    named.map(|name| Unused { on: Machine::Remote(destination.clone()), name })
}

/// What luneta knows of the clients of `session` on `destination`, which
/// runs, before its own comes there, as zellij there lists them within
/// `away::QUICK`, as luneta's screen waits on it: none, where it did not.
pub fn before_away(destination: &Destination, session: &str) -> Before {
    let listed = Away::reach(destination, away::QUICK).and_then(|mut away| away.clients(session));
    listed.map_or(Before::Unknown, Before::Listed)
}

/// Whether a session runs under the name `session`, as zellij's sockets say
/// now: none where luneta cannot read all of zellij's folder, as the socket
/// could be in what it cannot.
fn runs(session: &str) -> Option<bool> {
    let running = lunetad::zellij_sessions(&lunetad::zellij_folder())?;
    Some(running.iter().any(|(name, _)| name == session))
}

/// `to`, as a name zellij on this machine would make a session of now, or
/// why it would not: it would take the client to one that has the name
/// instead, one that runs or one that ended, which it brings back, the
/// directory it was to make it in unused. zellij makes a session only by
/// `switch-session`, which does either, so luneta asks just before.
pub fn makes(to: SessionName) -> Result<Unused, String> {
    let cannot = || format!("no session made: luneta cannot tell whether one is called {to}");
    if runs(to.as_str()).ok_or_else(cannot)? {
        return Err(format!("no session made: {to} runs already"));
    }
    if kept().ok_or_else(cannot)?.iter().any(|kept| kept == to.as_str()) {
        return Err(format!("no session made: zellij keeps {to}, which ended, to bring back"));
    }
    Ok(Unused { on: Machine::Local, name: to })
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
    /// The client's arrival in a session zellij makes for it, from another,
    /// which luneta asks for itself once zellij is in it: only the look
    /// numbered so, to hear that the client stayed where zellij did not take
    /// it there.
    Made(u64),
}

impl Then {
    /// zellij could not be asked to take the action at all: the client
    /// stays where it is, and what waits on its going hears so.
    fn undone(self, events: &Sender<Event>) {
        match self {
            Then::Look(_) => {},
            // luneta is on its way out.
            Then::Arrive(look, after) => {
                let _ = events.send(Event::Stayed(look.number));
                if let Some(after) = after {
                    after.stayed(events);
                }
            },
            Then::Made(look) => drop(events.send(Event::Stayed(look))),
        }
    }
}

/// An action luneta has zellij take.
enum Action {
    /// A go, as `moves::go` says, on `on`.
    Go { on: Machine, at: SessionName, to: SessionName, pane: Option<u32> },
    /// A start, as `moves::start` says, on `on`.
    Start { on: Machine, at: SessionName, to: SessionName, cwd: Option<String> },
}

/// zellij, on the machine luneta's client runs on: this one's, by its CLI,
/// or another's, by the daemon there, through this one's.
enum There {
    Here,
    Away(Box<Away>),
}

impl There {
    /// zellij on `on`, or why luneta does not reach it.
    fn on(on: &Machine) -> Result<There, String> {
        match on {
            Machine::Local => Ok(There::Here),
            Machine::Remote(destination) => {
                Away::reach(destination, away::ANSWER).map(|away| There::Away(Box::new(away)))
            },
        }
    }

    /// The clients of `session`, as zellij lists them now, or why not.
    fn clients(&mut self, session: &str) -> Result<Vec<Client>, String> {
        match self {
            There::Here => clients(session),
            There::Away(away) => away.clients(session),
        }
    }

    /// What luneta knows of the clients of `session` before its own comes
    /// there, as `before` says.
    fn before(&mut self, session: &str) -> Before {
        match self {
            There::Here => before(session),
            There::Away(away) => away.clients(session).map_or(Before::Unknown, Before::Listed),
        }
    }

    /// Has zellij open `open`, as `lunetad::opens::opening` does, and says
    /// which terminal it opened, or why it did not.
    fn open(&mut self, open: &Open) -> Result<u32, String> {
        match self {
            There::Here => opening(open),
            There::Away(away) => away.open(open),
        }
    }

    /// What closing the terminal `pane` of `session` closes, as
    /// `lunetad::closes::closes_now` says, or why it closes nothing.
    fn closes(&mut self, session: &SessionName, pane: u32) -> Result<Closes, String> {
        match self {
            There::Here => closes_now(session.as_str(), pane),
            There::Away(away) => away.closes(session, pane),
        }
    }

    /// Has zellij close `close` of `session`, or says why it did not.
    fn close(&mut self, session: &str, close: Close) -> Result<(), String> {
        match self {
            There::Here => closed(session, close),
            There::Away(away) => away.close(session, close),
        }
    }

    /// Has zellij kill `session`, the one that started at `started`, and
    /// delete it, as `lunetad::closes::killed` does, or says why it did not.
    fn kill(&mut self, session: &str, started: SystemTime) -> Result<(), String> {
        match self {
            There::Here => killed(session, started),
            There::Away(away) => away.kill(session, started),
        }
    }

    /// Has zellij take `action`, which is of its machine, or says why it did
    /// not. A pane already focused is where the action would take the
    /// client, and no failure.
    fn take(&mut self, action: Action) -> Result<(), String> {
        let command = match (action, self) {
            (Action::Go { at, to, pane, .. }, There::Away(away)) => return away.go(&at, &to, pane),
            (Action::Go { at, to, pane, .. }, There::Here) => {
                match moves::go(Command::new("zellij"), &at, &to, pane) {
                    Some(command) => command,
                    None => return Ok(()),
                }
            },
            (Action::Start { at, to, cwd, .. }, There::Away(away)) => {
                return away.start(&at, &to, cwd.as_deref());
            },
            (Action::Start { at, to, cwd, .. }, There::Here) => {
                moves::start(Command::new("zellij"), &at, &to, cwd.as_deref())
            },
        };
        let mut command = command;
        quiet(&mut command);
        match command.output() {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let said = failure(&output);
                if said.contains(moves::FOCUSED) { Ok(()) } else { Err(said) }
            },
            Err(error) => Err(unrun(&error)),
        }
    }
}

/// Has zellij take `action` at `when`, on the machine it is of, on a thread
/// of its own, so that the sidebar goes on drawing while zellij answers, says
/// why when it fails, and then does as `then` says. A client on its way to
/// another session comes there once zellij lists one more client of it than
/// before the action, and then what waits on it is done.
fn run(action: Action, then: Option<Then>, when: Instant, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let (Action::Go { on, .. } | Action::Start { on, .. }) = &action;
        let mut there = match There::on(on) {
            Ok(there) => there,
            Err(why) => {
                // luneta is on its way out.
                let _ = events.send(Event::Notice(why));
                if let Some(then) = then {
                    then.undone(&events);
                }
                return;
            },
        };
        match then {
            None => {
                act(&mut there, action, when, &events);
            },
            Some(Then::Look(look)) => {
                let went = if act(&mut there, action, when, &events) { look.went } else { None };
                looking(&mut there, &Look { went, ..look }, &events);
            },
            Some(Then::Arrive(look, after)) => {
                let before = there.before(&look.session);
                let acted = act(&mut there, action, when, &events);
                let came = acted && arriving(&mut there, &look, before, ARRIVAL, &events);
                if acted && !came {
                    let said = "zellij did not take its client there".to_owned();
                    let _ = events.send(Event::Notice(said));
                }
                if !came {
                    let _ = events.send(Event::Stayed(look.number));
                }
                match after {
                    Some(after) if came => after.came(&mut there, &events),
                    Some(after) => after.stayed(&events),
                    None => {},
                }
            },
            Some(Then::Made(look)) => {
                if !act(&mut there, action, when, &events) {
                    // luneta is on its way out.
                    let _ = events.send(Event::Stayed(look));
                }
            },
        }
    });
}

/// Has zellij `there` take `action` at `when`, and says why when it fails.
/// Whether it took it.
fn act(there: &mut There, action: Action, when: Instant, events: &Sender<Event>) -> bool {
    thread::sleep(when.saturating_duration_since(Instant::now()));
    let failure = there.take(action).err();
    if let Some(said) = &failure {
        // luneta is on its way out.
        let _ = events.send(Event::Notice(said.clone()));
    }
    failure.is_none()
}

/// Asks zellij `there` for the clients of the session `look` is of, and
/// hands back what it answers, or why it did not.
fn looking(there: &mut There, look: &Look, events: &Sender<Event>) {
    // luneta is on its way out.
    let _ = events.send(match there.clients(&look.session) {
        Ok(clients) => {
            Event::Looked(Looked { number: look.number, went: look.went, clients, before: None })
        },
        Err(why) => Event::Notice(why),
    });
}

/// Asks zellij `there` for the clients of the session `look` is of until it
/// lists one that was not there `before`, for as long as `patience`, and
/// hands back what it answers then. Whether it did. Of a session zellij
/// makes, the first it lists is luneta's; of one it did not list the clients
/// of, any it lists is taken for luneta's coming, though it may be another's.
fn arriving(
    there: &mut There,
    look: &Look,
    before: Before,
    patience: Duration,
    events: &Sender<Event>,
) -> bool {
    let deadline = Instant::now() + patience;
    loop {
        let came = there.clients(&look.session).ok().filter(|clients| {
            let was = before.clients();
            clients.iter().any(|client| was.iter().all(|was| was.id != client.id))
        });
        if let Some(clients) = came {
            let (number, went, before) = (look.number, look.went, Some(before));
            // luneta is on its way out.
            let _ = events.send(Event::Looked(Looked { number, went, clients, before }));
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(ARRIVING);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A command runs in the pane zellij's variables name, by the name its
    /// session had as the pane opened, and the agent of luneta's ask box, in
    /// none, goes by the pane the box names. One the box misspells is no
    /// pane, nor is one it names where zellij's name another.
    #[test]
    fn a_command_runs_in_the_pane_zellij_names_or_goes_by_the_one_the_ask_box_does() {
        let opened = |named: &str, pane| Ok(Some(Seat::Opened { named: named.to_owned(), pane }));
        let asked = |session: &str, pane| {
            Ok(Some(Seat::Asked(Pane { session: session.to_owned(), pane })))
        };
        assert_eq!(seat_in(None, Some("work"), Some("2")), opened("work", 2));
        assert_eq!(seat_in(Some("a:b:4"), None, None), asked("a:b", 4));
        assert_eq!(seat_in(Some("work:4"), Some(""), Some("2")), asked("work", 4));
        for (session, pane) in
            [(Some(""), Some("2")), (Some("work"), Some("terminal_2")), (Some("work"), None), (None, None)]
        {
            assert_eq!(seat_in(None, session, pane), Ok(None), "{session:?} {pane:?}");
        }
        for asked in ["", "work", ":4", "work:x"] {
            assert!(seat_in(Some(asked), None, None).is_err(), "{asked}");
        }
        assert!(seat_in(Some("work:4"), Some("work"), Some("2")).is_err());
    }

    /// zellij's client reads `NUDGE` as a key let go once it asked its
    /// terminal for kitty's keys. One its config keeps from asking reads it
    /// as no key, which luneta would take for one, and gets none.
    #[test]
    fn only_a_client_that_reads_kitty_s_keys_is_nudged() {
        assert_eq!(nudge(TermMode::DISAMBIGUATE_ESC_CODES), Some(NUDGE));
        assert_eq!(nudge(TermMode::empty()), None);
    }

    /// The sessions that ended are read from what `list-sessions` lists,
    /// with how long ago each was made, to the second; the rest are left
    /// out: those that run, yours, and a line that does not read.
    #[test]
    fn the_sessions_that_ended_are_read_from_what_zellij_lists() {
        let listed = "bipa [Created 1day 5h 24m 9s ago] \n\
                      luneta [Created 1day 5h 20m 52s ago] (current)\n\
                      nixos [Created 1day 11m 50s ago] (EXITED - attach to resurrect)\n\
                      a b [Created 3s ago] (EXITED - attach to resurrect)\n\
                      odd [Created 2fortnights ago] (EXITED - attach to resurrect)\n\
                      (EXITED - attach to resurrect)\n";
        let exited = exited_in(listed);
        let read: Vec<(&str, u64)> =
            exited.iter().map(|exited| (exited.name.as_str(), exited.ago.as_secs())).collect();
        assert_eq!(read, [("nixos", 86_400 + 11 * 60 + 50), ("a b", 3)]);
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

    /// A session's name reaches zellij on another machine as it is, through
    /// the shell there that reads the command ssh hands it, whatever is in
    /// it: sh, or fish, where the tests run with one. fish runs without your
    /// config, which could print as it starts.
    #[test]
    fn a_session_s_name_reads_back_in_the_shell_there() {
        let names = ["work", "odd | name", "it's", r"back\slash", "$HOME *", "a\nb", r"\'"];
        for (shell, flags) in [("sh", &[][..]), ("fish", &["--no-config"][..])] {
            for name in names {
                let printed = format!("printf %s {}", quoted(name));
                let said = match Command::new(shell).args(flags).args(["-c", &printed]).output() {
                    Ok(said) => said,
                    Err(error) if shell == "fish" && error.kind() == io::ErrorKind::NotFound => {
                        break;
                    },
                    Err(error) => panic!("{shell}: {error}"),
                };
                assert_eq!(String::from_utf8(said.stdout).unwrap(), name, "{shell}");
            }
        }
    }

    fn laid(id: u32, tab_id: u32) -> Laid {
        Laid {
            id,
            is_plugin: false,
            is_selectable: true,
            is_suppressed: false,
            is_floating: false,
            is_focused: false,
            tab_id,
            tab_position: tab_id,
        }
    }

    /// A session with no client shows the terminal focused in its first tab,
    /// a tiled one over one floating, or else the first there: no other tab
    /// is the one zellij says it is on.
    #[test]
    fn a_session_with_no_client_shows_the_pane_focused_in_its_first_tab() {
        let focused = |laid: Laid| Laid { is_focused: true, ..laid };
        let floating = |laid: Laid| Laid { is_floating: true, ..laid };
        let plugin = |laid: Laid| Laid { is_plugin: true, ..laid };
        let tabs = [laid(1, 1), focused(laid(2, 1)), focused(laid(3, 0)), laid(4, 0)];
        assert_eq!(shown(&tabs), Some(3));
        let first = [focused(floating(laid(5, 0))), laid(6, 0), laid(7, 0)];
        assert_eq!(shown(&first), Some(6));
        assert_eq!(shown(&[plugin(laid(8, 0))]), None);
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

    /// Where no socket of zellij 0.45's contract has the name of the session
    /// a rename is of, the rename is left to say why: the session is of a
    /// zellij of another contract, or it ended. Where one has, and no server
    /// listens on it, luneta says so, and renames nothing.
    #[test]
    fn a_rename_lists_the_sessions_again_only_where_a_socket_of_the_contract_has_its_name() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let Ok(session) = SessionName::running("work") else {
            panic!("a name zellij takes");
        };
        assert_eq!(relisted_in(scratch.path(), &session), Ok(()));
        let contract = scratch.path().join(lunetad::ZELLIJ_CONTRACT);
        assert!(fs::create_dir(&contract).is_ok());
        assert!(std::os::unix::net::UnixListener::bind(contract.join("work")).is_ok());
        let refused = relisted_in(scratch.path(), &session);
        assert!(refused.as_ref().is_err_and(|why| why.contains("did not list")), "{refused:?}");
    }

    /// The rename goes on once the server answers the question zellij's CLI
    /// asks it, which it answers once it read again which sessions run and
    /// which ended, and to luneta alone: what it says before, to luneta among
    /// its other clients, luneta reads past. A server that hangs up first
    /// did not say.
    #[test]
    fn a_rename_goes_on_once_the_server_answers_past_what_else_it_says() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let Ok(session) = SessionName::running("work") else {
            panic!("a name zellij takes");
        };
        let contract = scratch.path().join(lunetad::ZELLIJ_CONTRACT);
        assert!(fs::create_dir(&contract).is_ok());
        let Ok(listener) = std::os::unix::net::UnixListener::bind(contract.join("work")) else {
            panic!("no socket");
        };
        let framed = |message: &[u8]| [&(message.len() as u32).to_le_bytes()[..], message].concat();
        // What the server answers each message with, and a screen to draw.
        let (unblocked, render) = (framed(&[0x12, 0]), framed(&[0; 10_000]));
        let answers = [vec![unblocked.clone(), render, framed(&CONNECTED)], vec![unblocked]];
        let server = thread::spawn(move || {
            for answer in answers {
                let Ok((mut client, _)) = listener.accept() else {
                    panic!("no client came");
                };
                let mut asked = [0; RELIST.len() + CONN_STATUS.len()];
                assert!(client.read_exact(&mut asked).is_ok());
                for message in answer {
                    assert!(client.write_all(&message).is_ok());
                }
            }
        });
        assert_eq!(relisted_in(scratch.path(), &session), Ok(()));
        let hung_up = relisted_in(scratch.path(), &session);
        assert!(hung_up.as_ref().is_err_and(|why| why.contains("it hung up")), "{hung_up:?}");
        assert!(server.join().is_ok());
    }

    /// What waits on luneta's client's arrival in a session zellij makes
    /// hears that it did not come where luneta drops it unheard, as where
    /// zellij did not take the client there: the menu waiting on it closes,
    /// and says so. Told once, it is told no more.
    #[test]
    fn what_waits_on_an_arrival_dropped_unheard_hears_that_it_did_not_come() {
        let (events, heard) = std::sync::mpsc::channel();
        let Ok(to) = SessionName::parse("cdk") else {
            panic!("a name zellij takes");
        };
        let tab =
            Tab { cwd: "/w/cdk/feat".to_owned(), name: Some("#5".to_owned()), command: Vec::new() };
        let pending =
            || Pending::new(owed(7, &events), tab.clone(), &to, SystemTime::now(), &events);
        drop(pending());
        pending().done(false);
        let heard: Vec<Event> = heard.try_iter().collect();
        assert_eq!(heard.len(), 2);
        for event in heard {
            assert!(matches!(event, Event::Opened { menu: 7, opened: Err(_) }));
        }
    }

    /// Where nothing would tell luneta its client came, what a menu waited
    /// to open there is not opened, and the menu hears why, once: it closes,
    /// rather than wait for good.
    #[test]
    fn a_menu_hears_that_luneta_cannot_tell_when_to_open_what_it_waits_on() {
        let (events, heard) = std::sync::mpsc::channel();
        let Ok(to) = SessionName::parse("cdk") else {
            panic!("a name zellij takes");
        };
        let tab = Tab { cwd: "/w/cdk/feat".to_owned(), name: None, command: Vec::new() };
        After::tab(owed(7, &events), &to, tab).untold(&events);
        let heard: Vec<Event> = heard.try_iter().collect();
        let [Event::Opened { menu: 7, opened: Err(why) }] = &heard[..] else {
            panic!("the menu alone hears, once");
        };
        assert_eq!(why, "luneta cannot tell when zellij's client is there");
    }
}
