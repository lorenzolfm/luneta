//! What luneta asks of zellij: a client to run beside the sidebar, to take
//! that client to a pane, the clients of a session, and what the menu opens
//! and closes, through `lunetad::mux::Mux`. What luneta knows of zellij's ways
//! that `Mux` does not say is here and in `own`, which finds luneta's client
//! among those.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::TermMode;
use lunetad::api::{refuge_response, Client};
pub use lunetad::moves::{Ended, Refuge};
use lunetad::mux::{self, ClientWords, Mux, PaneId, PANE_VAR, SESSION_VAR};
pub use lunetad::closes::{
    closed, closes_now, closes_together, deleted, killed, Close, Closes, Closing,
};
use lunetad::closes::running_in;
pub use lunetad::names::{session_for, Starts};
pub use lunetad::opens::{opening, Open, Opened, Tab};
use lunetad::asks::{self, Turn, User, ASKED_VAR, AWAY_VAR, BEFORE_VAR, TURN_VAR};
use lunetad::turns::{self, IntoPanes, IntoTab, OtherTab};
use lunetad::{moves, Destination, Directory, SocketId};
pub use lunetad::SessionName;

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::{Event, Nudged};
use crate::icons;
use crate::machines::Machine;
use crate::own::{Before, Look, Looked};
use crate::sidebar::PanePlace;

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
    /// In no pane, as the agent of luneta's ask box runs, in `turn`, for you
    /// where `user` says: in the pane you asked from, by the name its session
    /// had as you asked, or on another machine, where luneta's zellij was,
    /// or where luneta could not tell.
    Asked { turn: Turn, user: User },
}

impl Seat {
    /// The turn of luneta's ask box it is in: none outside the box.
    pub fn turn(&self) -> Option<Turn> {
        match self {
            Seat::Asked { turn, .. } => Some(*turn),
            Seat::Opened { .. } => None,
        }
    }
}

/// Where this runs, as zellij names it in the variables of what runs in its
/// panes, or luneta's ask box in those of its agent: none outside both. Or
/// why the box's do not read, as `asks::turn_in` says, or name a turn where
/// zellij's name a pane: nothing runs in a pane and asks from another.
pub fn here() -> Result<Option<Seat>, String> {
    let (key, before) = (var(TURN_VAR)?, var(BEFORE_VAR)?);
    let (asked, away) = (var(ASKED_VAR)?, var(AWAY_VAR)?);
    let turn = asks::turn_in(key.as_deref(), before.as_deref(), asked.as_deref(), away.as_deref());
    let (session, pane) = (var(SESSION_VAR)?, var(PANE_VAR)?);
    seat_in(turn?, session.as_deref(), pane.as_deref())
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

/// The seat of the turn of luneta's ask box `asked`, and where it says you
/// are, or else the one zellij's `session` and `pane` name, or none.
fn seat_in(
    asked: Option<(Turn, User)>,
    session: Option<&str>,
    pane: Option<&str>,
) -> Result<Option<Seat>, String> {
    match (asked, session.filter(|session| !session.is_empty())) {
        (Some((_, user)), Some(session)) => {
            let asked = match user {
                User::Pane { session: from, pane } => {
                    format!("{ASKED_VAR} names {from}:{pane}, a pane luneta's ask box asked from")
                },
                User::Away => {
                    format!("{AWAY_VAR} says luneta's ask box asked from another machine")
                },
                User::Untold => format!("{TURN_VAR} names a turn of luneta's ask box"),
            };
            Err(format!("{asked}, and this runs in a pane of {session}"))
        },
        (Some((turn, user)), None) => Ok(Some(Seat::Asked { turn, user })),
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
/// in a session of a name zellij makes up; or why it cannot attach.
pub fn client(session: Option<&SessionName>) -> Result<(String, Vec<String>), String> {
    Mux::Zellij.client_attaching(session).map(client_with)
}

/// The program luneta runs beside the sidebar for a client that makes
/// `session`, and its arguments: zellij's client, which makes a session in
/// the directory it starts in, and fails where one runs by that name, or
/// zellij keeps one that ended, to bring back, and says so: a name taken
/// since luneta found it unused, where `attach --create` would take the
/// client to that session, as though it had made it.
pub fn make(session: &SessionName) -> (String, Vec<String>) {
    client_with(Mux::Zellij.client_making(session))
}

/// The program luneta runs beside the sidebar in place of a client that
/// ended, and its arguments: zellij's client, attached to `session`, which
/// runs. Without `--create`, which would make one gone since anew, empty.
/// Or why it cannot attach.
pub fn rejoin(session: &SessionName) -> Result<(String, Vec<String>), String> {
    Mux::Zellij.client_rejoining(session).map(client_with)
}

/// The client `words` say. It runs through `env`, to leave out what they say
/// to, which luneta cannot take out of its own variables.
fn client_with(words: ClientWords) -> (String, Vec<String>) {
    let ClientWords { program, args, unset } = words;
    let mut through: Vec<String> =
        unset.iter().flat_map(|name| ["-u".to_owned(), (*name).to_owned()]).collect();
    through.push(program.to_owned());
    through.extend(args);
    ("env".to_owned(), through)
}

/// The program luneta runs beside the sidebar for a client of `session` on
/// `destination`, which runs, and its arguments: zellij's client there,
/// through ssh, in a terminal ssh asks for there. It goes on the connection
/// luneta's daemon reaches that machine's on, as `lunetad::sharing` says,
/// where luneta's runtime folder is one ssh can name. ssh may ask for a
/// password or a host's key in luneta's terminal. Or why it cannot attach.
pub fn attach_away(
    destination: &Destination,
    session: &SessionName,
) -> Result<(String, Vec<String>), String> {
    Ok(away(destination, spelled(&Mux::Zellij.client_rejoining(session)?)))
}

/// The program luneta runs beside the sidebar for a client that makes
/// `session` on `destination`, in `cwd`, or where ssh starts it there, and
/// its arguments, as `attach_away` says: zellij's client there, as `make`
/// says of this machine's, which fails where the name is taken.
pub fn make_away(
    destination: &Destination,
    session: &SessionName,
    cwd: Option<&Directory>,
) -> (String, Vec<String>) {
    let cd = cwd.map_or_default(|cwd| format!("cd {} && ", quoted(cwd.as_str())));
    away(destination, format!("{cd}{}", spelled(&Mux::Zellij.client_making(session))))
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

/// The client `words` say, as a shell there reads them back: each argument
/// as `quoted` says, and the program bare, as some shells take a quoted first
/// word for a string.
fn spelled(words: &ClientWords) -> String {
    let mut spelled = vec![words.program.to_owned()];
    spelled.extend(words.args.iter().map(|arg| quoted(arg)));
    spelled.join(" ")
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

/// What became of the session luneta's zellij's client ended in, of
/// `seats`, the sessions it may have been in, and where it goes on on this
/// machine, as `lunetad::moves::refuge` finds them here: never a session that
/// may hold luneta's terminal, as `clients_of_refuge` says.
pub fn ended(seats: &[SessionName]) -> Ended {
    let current = env::var(SESSION_VAR).ok();
    let folder = Mux::Zellij.socket_folder();
    moves::refuge(&folder, seats, current.as_deref(), clients_of_refuge)
}

/// The clients `session` has before luneta's zellij goes on in it, as
/// `lunetad::moves::before` says them: none of one that may hold luneta's
/// terminal, as
/// `may_hold_terminal` says, which `lunetad::moves` passes over as it does one
/// that does not answer. Its name in the pane's environment would not do, as a
/// rename leaves it behind.
fn clients_of_refuge(session: &SessionName) -> Option<Vec<Client>> {
    if may_hold_terminal(session.as_str()) {
        return None;
    }
    moves::before(session)
}

/// What sshd sets in a session it runs: in every one, and in one with a
/// terminal.
const SSH_VARS: [&str; 2] = ["SSH_CONNECTION", "SSH_TTY"];

/// Where luneta's own terminal may be, as luneta's environment says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Within {
    /// On this machine, in no pane of zellij's, nor through ssh: no session
    /// of another machine's holds it.
    Here,
    /// Maybe in a pane of another machine's zellij: luneta runs in an ssh
    /// session, or in a pane of zellij here, whose client may come through
    /// ssh in its turn. Which session holds it, luneta cannot tell from here.
    MaybeAway,
}

/// Where luneta's own terminal may be, as `Within` says.
pub fn within() -> Within {
    let set = |var: &&str| env::var_os(var).is_some();
    if SSH_VARS.iter().any(set) || ["ZELLIJ", SESSION_VAR].iter().any(set) {
        Within::MaybeAway
    } else {
        Within::Here
    }
}

/// Whether `session`, which runs here, may hold luneta's terminal in a pane: it
/// has the name the pane opened under, or zellij lists the pane there running
/// luneta, or what it has yet to say, or will not list its panes. The name
/// alone will not do: a rename leaves it behind in the pane, and another
/// session can take it. Nor can the list tell the pane of a script that runs
/// luneta, which it lists as that script.
pub fn may_hold_terminal(session: &str) -> bool {
    if env::var(SESSION_VAR).is_ok_and(|named| named == session) {
        return true;
    }
    let Some(pane) = env::var(PANE_VAR).ok().and_then(|pane| pane.parse::<u32>().ok()) else {
        return false;
    };
    let laid = |session: SessionName| Mux::Zellij.laid(&session).ok();
    let Some(laid) = SessionName::running(session).ok().and_then(laid) else {
        return true;
    };
    laid.iter().filter(|listed| !listed.is_plugin && listed.id == pane).any(|listed| {
        let runs = listed.pane_command.as_deref();
        runs.is_none_or(|command| icons::program(command) == Some("luneta"))
    })
}

/// The sessions that run on this machine, as zellij's sockets say, once none
/// is so new that its server may have yet to take its first client, which a
/// call of zellij's CLI would kill, as `Before::Made` says. Another luneta can
/// be making one as this one starts. None where not all of zellij's folder
/// reads.
pub fn settled() -> Option<Vec<(String, SocketId)>> {
    let folder = Mux::Zellij.socket_folder();
    loop {
        let running = Mux::Zellij.sockets(&folder)?;
        let started = running.iter().map(|(_, id)| id.started());
        match Mux::Zellij.newborn(started, SystemTime::now()) {
            Some(left) => thread::sleep(left),
            None => return Some(running),
        }
    }
}

/// The first of `sessions`, of this machine's, whose clients zellij lists, as
/// `lunetad::moves::answering` tries them: a socket can outlive its server.
pub fn answering(sessions: Vec<SessionName>) -> Option<Refuge> {
    moves::answering(sessions, moves::before)
}

/// This machine's newest session, as `lunetad::moves::newest` finds it,
/// where luneta's zellij goes on once its client ended on another machine:
/// never a session that may hold luneta's terminal, as `clients_of_refuge`
/// says.
pub fn newest() -> Option<Refuge> {
    let current = env::var(SESSION_VAR).ok();
    moves::newest(&Mux::Zellij.socket_folder(), current.as_deref(), clients_of_refuge)
}

/// The first of `sessions` of `destination` whose clients its daemon lists,
/// each within `limit`, as `lunetad::moves::answering` tries them, where
/// luneta's zellij goes on once the machine it was on has no session left; or
/// why luneta does not reach the daemon it watches, to ask. None where none
/// answers.
pub fn answering_away(
    destination: &Destination,
    sessions: Vec<SessionName>,
    limit: Duration,
) -> Result<Option<Refuge>, String> {
    let mut away = Away::reach(destination, limit)?;
    Ok(moves::answering(sessions, |session| away.clients(session.as_str()).ok()))
}

/// What became of the session luneta's zellij's client ended in, of
/// `seats`, on `destination`, as its daemon there finds it, and where it
/// goes on there; or why that daemon did not say, or said what luneta does
/// not read.
pub fn ended_away(destination: &Destination, seats: &[SessionName]) -> Result<Ended, String> {
    let found = Away::reach(destination, away::ANSWER)?.refuge(seats)?;
    match found.ended {
        Some(refuge_response::Ended::Kept(())) => Ok(Ended::Kept),
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

/// What luneta types into its client before it asks the multiplexer to move
/// it, as `Mux::nudge` says, where in `mode` the client asked its terminal,
/// luneta's, for keys as kitty spells them.
pub fn nudge(mode: TermMode) -> Option<&'static [u8]> {
    Mux::Zellij.nudge(mode.contains(TermMode::DISAMBIGUATE_ESC_CODES))
}

/// How long luneta waits for its client to come to a session it sent it to
/// from another, from zellij's first answer after it took the action, before it
/// takes the client for one that stayed. It comes within milliseconds.
const ARRIVAL: Duration = Duration::from_secs(2);

/// How long luneta waits for its client to come to the session it attaches
/// to as it starts, which zellij can take a while to make.
pub const START: Duration = Duration::from_secs(10);

/// How often luneta asks zellij for the clients of a session its client is
/// on its way to, at first.
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
/// this machine, which it was nudged on, to a session it makes, `to`, that
/// starts in `cwd`, or where the client started, and then takes the look
/// `then`, if any, for the client's arrival there, after which it opens the
/// tab of `alone` there, if any, as `Alone` says. A session that runs under
/// that name is where it goes, and `cwd` goes unused, and the tab opens
/// beside its own; one that ended, which zellij kept to bring back, comes
/// back as it was: `makes` says first whether zellij would make it.
///
/// Unless zellij's sockets say one runs by that name just before, as they do
/// not of one the sidebar names, zellij makes it, and must take the client
/// there before it is asked anything, as `Before::Made` says: the look
/// comes back as the client's arrival, for luneta to ask for once zellij
/// titles its terminal in that session, with the tab. Of another machine,
/// whose sockets luneta reads only through its daemon there, luneta makes a
/// session only by a client of its own, as `Action::Start` says.
pub fn start(
    nudged: &Nudged,
    to: &SessionName,
    cwd: Option<&Directory>,
    then: Option<Look>,
    alone: Option<Alone>,
    events: &Sender<Event>,
) -> Option<Arrival> {
    let runs = runs(to);
    let (at, cwd) = (nudged.session().clone(), cwd.cloned());
    let command = Action::Start { at, to: to.clone(), cwd };
    let when = nudged.until();
    match (then, alone) {
        (Some(look), alone) if runs != Some(true) => {
            let after = alone.map(Alone::after);
            run(command, Some(Then::Made(look.number)), when, events);
            Some(Arrival::new(Machine::Local, look, Before::Made, after))
        },
        (Some(look), alone) => {
            let beside = |Alone { owed, tab, session, .. }| After::tab(owed, &session, tab);
            run(command, Some(Then::Arrive(look, alone.map(beside))), when, events);
            None
        },
        (None, None) => {
            run(command, None, when, events);
            None
        },
        // Nothing would say when the client is there.
        (None, Some(alone)) => {
            alone.after().untold(events);
            None
        },
    }
}

/// The tab a session luneta makes for a menu opens with, for the menu owed
/// what it opens: in place of the tabs zellij makes the session with, as
/// `Open::Tab` says, where the session's machine made it since luneta found
/// its name unused there, by that machine's clock, as `Unused` says. Where
/// that machine's daemon is too old to say when, the tab opens beside them,
/// as that daemon would open it anyway: it knows no tab that opens alone.
pub struct Alone {
    owed: Due<u32>,
    tab: Tab,
    session: SessionName,
    since: Option<SystemTime>,
}

impl Alone {
    /// The tab `tab`, for the menu `owed` what it opens, in the session
    /// luneta makes by the name `made` found unused.
    pub fn new(owed: Due<u32>, tab: Tab, made: &Unused) -> Self {
        Alone { owed, tab, session: made.name.clone(), since: made.since }
    }

    /// What opens it once the client has come to the session made for it.
    pub fn after(self) -> After {
        let Alone { owed, tab, session, since } = self;
        After::Open(owed, Open::Tab { session, tab, alone_since: since })
    }
}

/// What the CLI hears of a client it sent somewhere, where nothing typed in
/// the session it sent it from: zellij takes such an action as done, and
/// moves nobody.
const UNMOVED: &str = "zellij moved nobody: it moves the client that last typed in the session \
                       it is asked from, and none has typed there since it came";

/// Takes the client that last typed in the session `at` to the pane `pane`
/// of `to`, or to `to` alone, on the pane it last had the focus on, as the
/// CLI does. Waits for the client to be there, and says why it is not: as
/// `UNMOVED` says, or as zellij said.
pub fn taken(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Result<(), String> {
    if at == to {
        return pane.map_or(Ok(()), |pane| focused(at, PaneId::Terminal(pane)));
    }
    let before = clients_on_panes(to)?;
    Mux::Zellij.went(at, to, pane)?;
    let new = |client: &mux::Client| before.iter().all(|was| was.id != client.id);
    let came = seen(to, |clients| clients.iter().copied().find(new));
    let came = came.ok_or_else(|| UNMOVED.to_owned())?;
    match pane {
        Some(pane) if came.pane != PaneId::Terminal(pane) => {
            Err(format!("zellij took the client to {to}, but not to pane {pane}"))
        },
        _ => Ok(()),
    }
}

/// Takes the client that last typed in `session` to its pane `pane`, a terminal's
/// or a plugin's. Waits for a client to be on it, and says why none is: as
/// `UNMOVED` says, or as zellij said. It is asked again where a pane closing kept
/// it from asking, as a turn's stand-ins do just before it takes a client back.
pub fn focused(session: &SessionName, pane: PaneId) -> Result<(), String> {
    Mux::Zellij.pane_focused(session, pane)?;
    let on = |clients: &[mux::Client]| clients.iter().any(|client| client.pane == pane);
    seen(session, |clients| on(clients).then_some(())).ok_or_else(|| UNMOVED.to_owned())
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
pub fn made(at: &SessionName, to: &SessionName, cwd: &Directory) -> Result<(), String> {
    let asked = SystemTime::now();
    Mux::Zellij.started(at, to, Some(cwd))?;
    match made_since(to, asked, START) {
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
fn made_since(session: &SessionName, asked: SystemTime, patience: Duration) -> Option<bool> {
    started(session, patience).map(|started| started >= asked)
}

/// What `seen` makes of the clients of `session`, once it makes anything of
/// them, asking zellij every `ARRIVING` for as long as `ARRIVAL`: zellij
/// takes an action a moment after it answers it.
fn seen<T>(session: &SessionName, seen: impl Fn(&[mux::Client]) -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + ARRIVAL;
    loop {
        if let Some(seen) = clients_on_panes(session).ok().and_then(|clients| seen(&clients)) {
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
fn started(session: &SessionName, patience: Duration) -> Option<SystemTime> {
    let folder = Mux::Zellij.socket_folder();
    let deadline = Instant::now() + patience;
    loop {
        let running = Mux::Zellij.running(&folder).unwrap_or_default();
        let found = running.into_iter().find(|(name, _)| name == session.as_str());
        if let Some((_, started)) = found {
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
    /// Turn the pane `place` into a tab, for the menu owed the pane, now that a
    /// client is there to make room for it: told once done, or else as it is
    /// dropped undone.
    IntoTab(Due<u32>, PanePlace),
    /// Kill the session the client left, which it was in, the one that
    /// started at `started`, and delete it.
    Kill { session: SessionName, started: SystemTime },
}

impl After {
    /// Open `tab` in `session`, for the menu `owed` what it opens.
    fn tab(owed: Due<u32>, session: &SessionName, tab: Tab) -> Self {
        After::Open(owed, Open::Tab { session: session.clone(), tab, alone_since: None })
    }

    /// Says that nothing would tell when the client is there, so that it is
    /// not done: to the menu owed what it opens, or as a notice.
    pub fn untold(self, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) | After::IntoTab(owed, ..) => {
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
            (After::Open(owed, open), there) => {
                owed.pay(there.open(&open).map(|opened| told(opened, events)));
            },
            (After::IntoTab(owed, place), there) => {
                let (session, started) = (place.session.name(), place.session.started());
                let came = there.turn_into_tab(session, started, place.pane);
                owed.pay(came.and_then(|came| match came {
                    IntoTab::Done => Ok(place.pane),
                    IntoTab::NoClient => Err(lunetad::opens::no_client(session)),
                }));
            },
            // A zellij of its own kills it: one run as the client's would
            // wait on the session it ends.
            (After::Kill { session, started }, There::Here) => {
                kill(&Machine::Local, session, started, events);
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
            After::Open(owed, _) | After::IntoTab(owed, ..) => {
                owed.pay(Err("zellij did not take its client there".to_owned()));
            },
            After::Kill { session, .. } => {
                // luneta is on its way out.
                let _ = events.send(Event::Notice(format!("\"{session}\" stays")));
            },
        }
    }

    /// Says that the client did not come, as it failed, saying `why`: to the
    /// menu owed what it opens, or as a notice that the session stays.
    fn failed(self, why: String, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) | After::IntoTab(owed, ..) => owed.pay(Err(why)),
            kill @ After::Kill { .. } => kill.stayed(events),
        }
    }
}

/// Opens `open`, on the machine `on`, on a thread of its own, for the menu
/// numbered `menu`, with no client moved, and hands back the terminal it
/// opened, for luneta to take its client to, or why it opened none, as
/// `owed` does.
pub fn open(menu: u64, on: &Machine, open: Open, events: &Sender<Event>) {
    let (owed, on, events) = (owed(menu, events), on.clone(), events.clone());
    thread::spawn(move || {
        let opened = There::on(&on).and_then(|mut there| there.open(&open));
        owed.pay(opened.map(|opened| told(opened, &events)));
    });
}

/// The terminal zellij `opened`, once what it left as it was is in the
/// notes.
fn told(opened: Opened, events: &Sender<Event>) -> u32 {
    let Opened { terminal, notes } = opened;
    for note in notes {
        // luneta is on its way out.
        let _ = events.send(Event::Notice(note));
    }
    terminal
}

/// What the menu numbered `menu` is owed, as it waits on zellij to open what
/// a key asked for: the terminal it opened, or why none, as `Due` says.
pub fn owed(menu: u64, events: &Sender<Event>) -> Due<u32> {
    Due::new(events, move |opened| Event::Opened { menu, opened })
}

/// Asks zellij on the machine `on`, on a thread of its own, which tabs of
/// `session` are not that of its terminal `pane`, for the menu numbered
/// `menu`, and hands back what it answers.
pub fn other_tabs(
    menu: u64,
    on: &Machine,
    session: SessionName,
    pane: u32,
    events: &Sender<Event>,
) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        let tabs = There::on(&on).and_then(|mut there| there.other_tabs(&session, pane));
        // luneta is on its way out.
        let _ = events.send(Event::Tabs { menu, tabs });
    });
}

/// Has zellij on the machine `on` turn the pane `place` as `turn` says, on a
/// thread of its own, for the menu numbered `menu`, and hands back what came of
/// it. What stays moves nothing, luneta's client neither.
pub fn turn(menu: u64, on: &Machine, place: PanePlace, turn: turns::Turn, events: &Sender<Event>) {
    match turn {
        turns::Turn::PaneIntoTab => pane_into_tab(menu, on, place, events),
        turns::Turn::TabIntoPanes { beside } => tab_into_panes(menu, on, &place, beside, events),
    }
}

/// Turns the pane `place` into a tab, and hands back its terminal, for luneta
/// to take its client to, or why it did not, as `owed` does. Where its session
/// has no client to make room for the tab, the menu is owed that yet, once
/// luneta's client has come, as `Event::NoClient` says.
fn pane_into_tab(menu: u64, on: &Machine, place: PanePlace, events: &Sender<Event>) {
    let (owed, on, events) = (owed(menu, events), on.clone(), events.clone());
    thread::spawn(move || {
        let (session, started, pane) =
            (place.session.name(), place.session.started(), place.pane);
        match There::on(&on).and_then(|mut there| there.turn_into_tab(session, started, pane)) {
            Ok(IntoTab::Done) => owed.pay(Ok(pane)),
            Ok(IntoTab::NoClient) => {
                // luneta is on its way out.
                let _ = events.send(Event::NoClient { menu, owed, on, place });
            },
            Err(why) => owed.pay(Err(why)),
        }
    });
}

/// Turns the tab of the pane `place` into panes of the tab of the terminal
/// `beside`, and hands back what came of it where any moved, or why none did,
/// with `Event::IntoPanes`.
fn tab_into_panes(menu: u64, on: &Machine, place: &PanePlace, beside: u32, events: &Sender<Event>) {
    let (session, started, pane) =
        (place.session.name().clone(), place.session.started(), place.pane);
    let owed = Due::new(events, move |came| Event::IntoPanes { menu, pane, came });
    let on = on.clone();
    thread::spawn(move || {
        let came = There::on(&on).and_then(|mut there| {
            there.turn_into_panes(&session, started, pane, beside)
        });
        owed.pay(came);
    });
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
pub fn close(
    on: &Machine,
    session: SessionName,
    started: SystemTime,
    close: Close,
    events: &Sender<Event>,
) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        let closed = There::on(&on).and_then(|mut there| there.close(&session, started, &close));
        if let Err(why) = closed {
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
    session: SessionName,
    started: SystemTime,
    events: &Sender<Event>,
) -> JoinHandle<()> {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        if let Err(why) = There::on(&on).and_then(|mut there| there.kill(&session, started)) {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(why));
        }
    })
}

/// The sessions that run, as `running_in` says of zellij's folder.
pub fn running() -> Result<Vec<(String, SystemTime)>, String> {
    running_in(&Mux::Zellij.socket_folder())
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

/// luneta's client on its way to the session of `look`, on the machine `on`,
/// which zellij lists once it is there: as a client of it that was not there
/// `before`. What a go does once it is there, if anything, as `After` says,
/// is done once it is there, or not, when it does not come.
pub struct Arrival {
    pub look: Look,
    before: Before,
    on: Machine,
    after: Option<After>,
}

impl Arrival {
    /// luneta's client on its way to the session of `look`, on `on`, with
    /// `after` waiting on it, if anything.
    pub fn new(on: Machine, look: Look, before: Before, after: Option<After>) -> Self {
        Arrival { look, before, on, after }
    }

    /// Whether the session's server may have yet to take its first client as
    /// luneta's comes, as `Before::fresh` says.
    pub fn fresh(&self) -> bool {
        self.before.fresh()
    }

    /// Says to what waits on it that the client did not come, as luneta
    /// stops waiting on it: zellij did not take it there.
    pub fn stayed(self, events: &Sender<Event>) {
        if let Some(after) = self.after {
            after.stayed(events);
        }
    }

    /// Says to what waits on it that the client did not come, as it failed,
    /// saying `why`.
    pub fn failed(self, why: String, events: &Sender<Event>) {
        if let Some(after) = self.after {
            after.failed(why, events);
        }
    }
}

/// Asks zellij for the clients of the session of `arrival`'s look, on a
/// thread of its own, until it lists one that was not there before, which is
/// luneta's own client as it comes, and hands back what it answers then, and
/// then does what waits on it. A client that comes to a session zellij makes
/// is to be in it first, as zellij says by drawing for it: asked sooner,
/// zellij can end that session, as `Before::Made` says. It stops once `alive`
/// has gone, as another client took the place of the one it waits for.
pub fn arrive(arrival: Arrival, alive: &Arc<()>, events: &Sender<Event>) {
    let (alive, events) = (Arc::downgrade(alive), events.clone());
    thread::spawn(move || {
        let Arrival { look, before, on, after } = arrival;
        let failure = match There::on(&on) {
            Ok(mut there) => {
                let list = || there.clients(&look.session);
                let replaced = || alive.strong_count() == 0;
                match arriving(list, replaced, &look, before, START, START, &events) {
                    Arrived::Came => {
                        if let Some(after) = after {
                            after.came(&mut there, &events);
                        }
                        return;
                    },
                    Arrived::Stayed => None,
                    Arrived::Untold(why) => Some(why),
                }
            },
            Err(why) => Some(why),
        };
        let _ = events.send(Event::Unfound(look.number));
        if let Some(why) = &failure {
            // luneta is on its way out.
            let _ = events.send(Event::Notice(why.clone()));
        }
        match (after, failure) {
            (Some(after), Some(why)) => after.failed(why, &events),
            (Some(after), None) => after.stayed(&events),
            (None, _) => {},
        }
    });
}

/// What luneta knows of the clients of `session` before its own comes there:
/// none of one that does not run, as zellij's sockets say, which zellij is to
/// make, and otherwise what zellij lists, if it does.
pub fn before(session: &SessionName) -> Before {
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
    /// Just before luneta read which names zellij on `on` has, by that
    /// machine's clock: a session of the name that started then or after is
    /// one zellij made since, as one renamed to it keeps the time it started
    /// at. None from the daemon of another machine older than
    /// `NameSessionResponse.named_at`, which says no time.
    since: Option<SystemTime>,
}

impl Unused {
    pub fn on(&self) -> &Machine {
        &self.on
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }
}

/// The name a session luneta makes on `destination` takes, that starts as
/// `starts` says, as its daemon there names it within `away::QUICK`, as
/// luneta's screen waits on it, and the directory it named it for; or why it
/// takes none.
fn name_away(destination: &Destination, starts: &Starts) -> Result<(Unused, Directory), String> {
    let named = Away::reach(destination, away::QUICK).and_then(|mut away| away.name(starts));
    let (name, since, dir) = named?;
    Ok((Unused { on: Machine::Remote(destination.clone()), name, since }, dir))
}

/// The name a session luneta makes on `on` takes, that starts as `starts`
/// says, as `lunetad::names::free_at` and `name_away` name it, and the
/// directory it is named for, which luneta makes it in. Or why it takes none.
pub fn unused_at(on: &Machine, starts: Starts) -> Result<(Unused, Directory), String> {
    match on {
        Machine::Local => {
            let (name, since, dir) = lunetad::names::free_at(starts)?;
            Ok((Unused { on: Machine::Local, name, since: Some(since) }, dir))
        },
        Machine::Remote(destination) => name_away(destination, &starts),
    }
}

/// What luneta knows of the clients of `session` on `destination`, which
/// runs, before its own comes there, as zellij there lists them within
/// `away::QUICK`, as luneta's screen waits on it: none, where it did not.
pub fn before_away(destination: &Destination, session: &SessionName) -> Before {
    let reached = Away::reach(destination, away::QUICK);
    let listed = reached.and_then(|mut away| away.clients(session.as_str()));
    listed.map_or(Before::Unknown, Before::Listed)
}

/// Whether a session runs under the name `session`, as zellij's sockets say
/// now: none where luneta cannot read all of zellij's folder, as the socket
/// could be in what it cannot.
fn runs(session: &SessionName) -> Option<bool> {
    let running = Mux::Zellij.running(&Mux::Zellij.socket_folder())?;
    Some(running.iter().any(|(name, _)| name == session.as_str()))
}

/// `to`, as a name zellij on this machine would make a session of now, or
/// why it would not: it would take the client to one that has the name
/// instead, one that runs or one that ended, which it brings back, the
/// directory it was to make it in unused. zellij makes a session only by
/// `switch-session`, which does either, so luneta asks just before.
pub fn makes(to: SessionName) -> Result<Unused, String> {
    let since = lunetad::names::unused(&to).map_err(|why| format!("no session made: {why}"))?;
    Ok(Unused { on: Machine::Local, name: to, since: Some(since) })
}

/// `to`, as a name zellij on `destination` would make a session of now, as
/// `makes` says of this machine's, as its daemon there says within
/// `away::QUICK`, as luneta's screen waits on it; or why zellij there would
/// make none.
pub fn makes_there(destination: &Destination, to: SessionName) -> Result<Unused, String> {
    let checked = Away::reach(destination, away::QUICK).and_then(|mut away| away.check(&to));
    let since = checked.map_err(|why| format!("no session made: {why}"))?;
    Ok(Unused { on: Machine::Remote(destination.clone()), name: to, since: Some(since) })
}

/// `to`, as a name zellij on `destination` would make a session of now, for
/// `cwd`, as `makes` says of this machine's: the name its daemon there gives
/// a session for `cwd` within `away::QUICK`, as luneta's screen waits on
/// it, which is `to` while no session there has it, nor one that ended, as
/// the daemon named it before. Or why zellij there would take the client to
/// one that has it.
pub fn makes_away(
    destination: &Destination,
    to: &SessionName,
    cwd: &Directory,
) -> Result<Unused, String> {
    let named = name_away(destination, &Starts::In(cwd.clone()));
    let (named, _) = named.map_err(|why| format!("no session made: {why}"))?;
    if named.name != *to {
        return Err(format!(
            "no session made: {to} is taken on {destination} now, which names one {} for {cwd}",
            named.name
        ));
    }
    Ok(named)
}

/// The clients of `session`, as zellij lists them now, by their ids, or why
/// zellij did not list them, as of a session that does not run, or in time:
/// zellij's CLI waits on every session's server, and one that hangs holds it
/// up for good.
pub fn clients(session: &SessionName) -> Result<Vec<Client>, String> {
    let clients = clients_on_panes(session)?;
    Ok(clients.iter().copied().map(mux::Client::to_api).collect())
}

/// The clients of `session`, as `clients` lists them, each by the pane it is on,
/// a plugin's too.
pub fn clients_on_panes(session: &SessionName) -> Result<mux::Listed, String> {
    Ok(Mux::Zellij.clients(session)?)
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
    /// A go, as `Mux::went` says, on `on`.
    Go { on: Machine, at: SessionName, to: SessionName, pane: Option<u32> },
    /// A start, as `Mux::started` says, on this machine. `switch-session`
    /// takes the client to whatever has the name by the time zellij takes
    /// it, which `start` looks for just before only here: on another
    /// machine, luneta makes a session by a client that makes it there, as
    /// `zellij --session` does, which zellij refuses a name taken by then.
    Start { at: SessionName, to: SessionName, cwd: Option<Directory> },
}

impl Action {
    /// The machine whose zellij takes it.
    fn machine(&self) -> Machine {
        match self {
            Action::Go { on, .. } => on.clone(),
            Action::Start { .. } => Machine::Local,
        }
    }
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
            There::Here => clients(&SessionName::running(session)?),
            There::Away(away) => away.clients(session),
        }
    }

    /// What luneta knows of the clients of `session` before its own comes
    /// there, as `before` says.
    fn before(&mut self, session: &str) -> Before {
        match self {
            There::Here => SessionName::running(session).map_or(Before::Unknown, |at| before(&at)),
            There::Away(away) => away.clients(session).map_or(Before::Unknown, Before::Listed),
        }
    }

    /// Has zellij open `open`, as `lunetad::opens::opening` does, and says
    /// which terminal it opened, and what it left as it was, or why it
    /// opened nothing.
    fn open(&mut self, open: &Open) -> Result<Opened, String> {
        match self {
            There::Here => opening(open),
            There::Away(away) => away.open(open),
        }
    }

    /// The tabs of `session` other than that of its terminal `pane`, as
    /// `lunetad::turns::other_tabs` says, or why it does not say.
    fn other_tabs(&mut self, session: &SessionName, pane: u32) -> Result<Vec<OtherTab>, String> {
        match self {
            There::Here => turns::other_tabs(session, pane),
            There::Away(away) => away.other_tabs(session, pane),
        }
    }

    /// Has zellij turn the terminal `pane` of `session` into a tab, as
    /// `lunetad::turns::into_tab` does, and says what came of it, or why it did
    /// not.
    fn turn_into_tab(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        pane: u32,
    ) -> Result<IntoTab, String> {
        match self {
            There::Here => turns::into_tab(session, started, pane),
            There::Away(away) => away.turn_into_tab(session, started, pane),
        }
    }

    /// Has zellij turn the panes of the tab of the terminal `pane` of `session`
    /// into panes of the tab of the terminal `beside`, as
    /// `lunetad::turns::into_panes` does, and says what came of it where any
    /// moved, or why none did.
    fn turn_into_panes(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        pane: u32,
        beside: u32,
    ) -> Result<IntoPanes, String> {
        match self {
            There::Here => turns::into_panes(session, started, pane, beside),
            There::Away(away) => away.turn_into_panes(session, started, pane, beside),
        }
    }

    /// What closing the terminal `pane` of `session` closes, as
    /// `lunetad::closes::closes_now` says, or why it closes nothing.
    fn closes(&mut self, session: &SessionName, pane: u32) -> Result<Closes, String> {
        match self {
            There::Here => closes_now(session, pane),
            There::Away(away) => away.closes(session, pane),
        }
    }

    /// Has zellij close `close` of `session`, or says why it did not.
    fn close(
        &mut self,
        session: &SessionName,
        started: SystemTime,
        close: &Close,
    ) -> Result<(), String> {
        match self {
            There::Here => closed(session, started, close),
            There::Away(away) => away.close(session, started, close),
        }
    }

    /// Has zellij kill `session`, the one that started at `started`, and
    /// delete it, as `lunetad::closes::killed` does, or says why it did not.
    fn kill(&mut self, session: &SessionName, started: SystemTime) -> Result<(), String> {
        match self {
            There::Here => killed(session, started),
            There::Away(away) => away.kill(session, started),
        }
    }

    /// Has zellij take `action`, which is of its machine, as `run` reaches
    /// it by `Action::machine`, or says why it did not. A pane already
    /// focused is where the action would take the client, and no failure.
    fn take(&mut self, action: Action) -> Result<(), String> {
        match action {
            Action::Go { at, to, pane, .. } => match self {
                There::Away(away) => away.go(&at, &to, pane),
                There::Here => Ok(Mux::Zellij.went(&at, &to, pane)?),
            },
            Action::Start { at, to, cwd } => Ok(Mux::Zellij.started(&at, &to, cwd.as_ref())?),
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
        let mut there = match There::on(&action.machine()) {
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
                if !act(&mut there, action, when, &events) {
                    Then::Arrive(look, after).undone(&events);
                    return;
                }
                let list = || there.clients(&look.session);
                match arriving(list, || false, &look, before, ARRIVAL, START, &events) {
                    Arrived::Came => {
                        if let Some(after) = after {
                            after.came(&mut there, &events);
                        }
                    },
                    Arrived::Stayed => {
                        let said = "zellij did not take its client there".to_owned();
                        let _ = events.send(Event::Notice(said));
                        Then::Arrive(look, after).undone(&events);
                    },
                    Arrived::Untold(why) => {
                        let said = format!("zellij did not say if it took its client there: {why}");
                        let _ = events.send(Event::Notice(said));
                        let _ = events.send(Event::Unfound(look.number));
                        if let Some(after) = after {
                            after.failed(why, &events);
                        }
                    },
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

#[derive(Debug, PartialEq, Eq)]
enum Arrived {
    Came,
    Stayed,
    Untold(String),
}

/// Has `list` ask zellij for the clients of the session `look` is of until it
/// lists one that was not there `before`, for `patience` from its first answer,
/// or `untold` while it does not answer, or until the client was `replaced`, and
/// hands back what it answers then. Of a session zellij makes, the first it lists
/// is luneta's; of one it did not list the clients of, any it lists is taken for
/// luneta's coming, though it may be another's.
fn arriving(
    mut list: impl FnMut() -> Result<Vec<Client>, String>,
    replaced: impl Fn() -> bool,
    look: &Look,
    before: Before,
    patience: Duration,
    untold: Duration,
    events: &Sender<Event>,
) -> Arrived {
    let started = Instant::now();
    let mut answered = None;
    loop {
        if replaced() {
            return Arrived::Stayed;
        }
        let was = before.clients();
        let is_new = |client: &Client| was.iter().all(|was| was.id != client.id);
        match list() {
            Ok(clients) if clients.iter().any(is_new) => {
                let (number, went, before) = (look.number, look.went, Some(before));
                // luneta is on its way out.
                let _ = events.send(Event::Looked(Looked { number, went, clients, before }));
                return Arrived::Came;
            },
            Ok(_) if answered.get_or_insert_with(Instant::now).elapsed() >= patience => {
                return Arrived::Stayed;
            },
            Err(why) if started.elapsed() >= untold => return Arrived::Untold(why),
            Ok(_) | Err(_) => thread::sleep(resting(started.elapsed())),
        }
    }
}

fn resting(waited: Duration) -> Duration {
    ARRIVING.max(waited / 10)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io;
    use std::process::Command;
    use std::sync::mpsc;

    use super::*;

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    const AGES: Duration = Duration::from_secs(600);

    fn look() -> Look {
        Look { session: "work".to_owned(), number: 7, went: None }
    }

    fn on_a_pane(id: u32) -> Client {
        Client { id, pane: Some(1) }
    }

    #[test]
    fn a_list_that_fails_uses_up_no_patience_for_a_client_on_its_way() {
        const PATIENCE: Duration = Duration::from_millis(100);
        let (events, heard) = mpsc::channel();
        let mut lists = vec![
            Ok(vec![on_a_pane(1), on_a_pane(2)]),
            Ok(vec![on_a_pane(1)]),
            Err("stuck".to_owned()),
        ];
        let list = || {
            let listed = lists.pop().unwrap();
            if listed.is_err() {
                thread::sleep(PATIENCE);
            }
            listed
        };
        let before = Before::Listed(vec![on_a_pane(1)]);
        let arrived = arriving(list, || false, &look(), before, PATIENCE, AGES, &events);
        assert_eq!(arrived, Arrived::Came);
        let Ok(Event::Looked(looked)) = heard.try_recv() else {
            panic!("zellij's answer was not handed back");
        };
        assert_eq!((looked.number, looked.clients), (7, vec![on_a_pane(1), on_a_pane(2)]));
    }

    #[test]
    fn a_list_with_no_new_client_for_the_patience_is_a_client_that_stayed() {
        let (events, heard) = mpsc::channel();
        let list = || Ok(vec![on_a_pane(1)]);
        let before = Before::Listed(vec![on_a_pane(1)]);
        let arrived = arriving(list, || false, &look(), before, Duration::ZERO, AGES, &events);
        assert_eq!(arrived, Arrived::Stayed);
        assert!(heard.try_recv().is_err());
    }

    #[test]
    fn no_list_for_as_long_as_luneta_waits_says_nothing_of_where_the_client_is() {
        let (events, heard) = mpsc::channel();
        let list = || Err("stuck".to_owned());
        let before = Before::Listed(Vec::new());
        let arrived = arriving(list, || false, &look(), before, AGES, Duration::ZERO, &events);
        assert_eq!(arrived, Arrived::Untold("stuck".to_owned()));
        assert!(heard.try_recv().is_err());
    }

    #[test]
    fn a_client_replaced_is_asked_for_no_more() {
        let (events, heard) = mpsc::channel();
        let asks = Cell::new(0);
        let list = || {
            asks.set(asks.get() + 1);
            Ok(Vec::new())
        };
        let replaced = || asks.get() == 2;
        let before = Before::Listed(Vec::new());
        let arrived = arriving(list, replaced, &look(), before, AGES, AGES, &events);
        assert_eq!(arrived, Arrived::Stayed);
        assert_eq!(asks.get(), 2);
        assert!(heard.try_recv().is_err());
    }

    #[test]
    fn a_client_that_never_comes_is_asked_for_less_and_less() {
        assert_eq!(resting(Duration::ZERO), ARRIVING);
        assert_eq!(resting(Duration::from_secs(5)), Duration::from_millis(500));
        let mut waited = Duration::ZERO;
        let mut asks = 0;
        while waited < START {
            waited += resting(waited);
            asks += 1;
        }
        assert!(asks <= 60, "it asks {asks} times");
    }

    /// A command runs in the pane zellij's variables name, by the name its
    /// session had as the pane opened, and the agent of luneta's ask box, in
    /// none, goes by the turn the box names, for you where it says. Where
    /// zellij names a pane too, it goes by neither: nothing runs in one pane
    /// and asks from another.
    #[test]
    fn a_command_runs_in_the_pane_zellij_names_or_goes_by_the_one_the_ask_box_does() {
        let opened = |named: &str, pane| Ok(Some(Seat::Opened { named: named.to_owned(), pane }));
        assert_eq!(seat_in(None, Some("work"), Some("2")), opened("work", 2));
        for (session, pane) in
            [(Some(""), Some("2")), (Some("work"), Some("terminal_2")), (Some("work"), None), (None, None)]
        {
            assert_eq!(seat_in(None, session, pane), Ok(None), "{session:?} {pane:?}");
        }
        let turn = Turn::new(255, Some(10)).unwrap();
        let work = User::Pane { session: SessionName::running("work").unwrap(), pane: 4 };
        for user in [work, User::Away, User::Untold] {
            let asked = Ok(Some(Seat::Asked { turn, user: user.clone() }));
            assert_eq!(seat_in(Some((turn, user.clone())), None, None), asked);
            assert_eq!(seat_in(Some((turn, user.clone())), Some(""), Some("2")), asked);
            let in_a_pane = seat_in(Some((turn, user.clone())), Some("play"), Some("2"));
            assert!(in_a_pane.is_err_and(|why| why.ends_with("this runs in a pane of play")));
        }
    }

    /// zellij's client reads the nudge as a key let go once it asked its
    /// terminal for kitty's keys. One its config keeps from asking reads it
    /// as no key, which luneta would take for one, and gets none.
    #[test]
    fn only_a_client_that_reads_kitty_s_keys_is_nudged() {
        assert!(nudge(TermMode::DISAMBIGUATE_ESC_CODES).is_some());
        assert_eq!(nudge(TermMode::empty()), None);
    }

    #[test]
    fn the_client_leaves_zellij_s_variables_out() {
        let Ok(session) = SessionName::parse("luneta-main") else {
            panic!("a name zellij takes");
        };
        let (program, args) = client(Some(&session)).unwrap();
        assert_eq!(program, "env");
        assert!(args.windows(2).any(|pair| pair == ["-u", "ZELLIJ"]));
        assert_eq!(args[args.len() - 4..], ["zellij", "attach", "--create", "luneta-main"]);
        let (program, args) = make(&session);
        assert_eq!(program, "env");
        assert!(args.windows(2).any(|pair| pair == ["-u", "ZELLIJ"]));
        assert_eq!(args[args.len() - 2..], ["zellij", "--session=luneta-main"]);
        let Ok(elo) = SessionName::parse("elo") else {
            panic!("a name zellij takes");
        };
        let (program, args) = rejoin(&elo).unwrap();
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

    /// Where nothing would tell luneta its client came, what a menu waited
    /// to open there is not opened, and the menu hears why, once: it closes,
    /// rather than wait for good.
    #[test]
    fn a_menu_hears_that_luneta_cannot_tell_when_to_open_what_it_waits_on() {
        let (events, heard) = std::sync::mpsc::channel();
        let Ok(to) = SessionName::parse("cdk") else {
            panic!("a name zellij takes");
        };
        let tab = Tab { cwd: directory("/w/cdk/feat"), name: None, command: Vec::new() };
        After::tab(owed(7, &events), &to, tab).untold(&events);
        let heard: Vec<Event> = heard.try_iter().collect();
        let [Event::Opened { menu: 7, opened: Err(why) }] = &heard[..] else {
            panic!("the menu alone hears, once");
        };
        assert_eq!(why, "luneta cannot tell when zellij's client is there");
    }

    /// A client that failed on its way to a session, as zellij refuses to make
    /// one of a name taken, has the menu waiting on it hear why, once.
    #[test]
    fn a_menu_hears_why_the_client_it_waits_on_failed() {
        let (events, heard) = std::sync::mpsc::channel();
        let Ok(to) = SessionName::parse("cdk") else {
            panic!("a name zellij takes");
        };
        let tab = Tab { cwd: directory("/w/cdk/feat"), name: None, command: Vec::new() };
        let after = After::tab(owed(7, &events), &to, tab);
        let look = Look { session: "cdk".to_owned(), number: 3, went: None };
        let arrival = Arrival::new(Machine::Local, look, Before::Made, Some(after));
        arrival.failed("Session with name \"cdk\" already exists".to_owned(), &events);
        let heard: Vec<Event> = heard.try_iter().collect();
        let [Event::Opened { menu: 7, opened: Err(why) }] = &heard[..] else {
            panic!("the menu alone hears, once");
        };
        assert_eq!(why, "Session with name \"cdk\" already exists");
    }
}
