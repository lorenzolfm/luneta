use std::env;
use std::ffi::OsString;
use std::fmt;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::TermMode;
use luneta_protocol::said::Said;
use lunetad::api::{refuge_response, Client};
pub use lunetad::moves::{Ended, Refuge};
use lunetad::mux::own;
use lunetad::mux::{self, ClientWords, Mux, OwnPane, PaneId, PaneVars, Zellij, ZellijPane};
pub use lunetad::closes::{closes_now, closes_together, deleted, Close, Closes, Closing};
use lunetad::closes::{running_in, Whose};
pub use lunetad::names::{session_for, Starts};
pub use lunetad::opens::{opening, opening_of, Open, Opened, Tab};
use lunetad::asks::{
    self, Turn, User, ASKED_VAR, AWAY_VAR, BEFORE_VAR, OWN_VAR, OWN_VAR_BEFORE, TURN_VAR,
};
use lunetad::turns::{self, IntoPanes, IntoTab, OtherTab};
use lunetad::{moves, Destination, Directory};
pub use lunetad::{var, SessionName};

use crate::away::{self, Away};
use crate::due::Due;
use crate::host::{Event, Nudged};
use crate::icons;
use crate::machines::Machine;
use crate::own::{Before, Look, Looked};
use crate::sidebar::PanePlace;

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

pub fn pane_address(spelled: &str) -> Result<Pane, String> {
    let pane = spelled.rsplit_once(':').and_then(|(session, pane)| {
        let pane = pane.parse().ok()?;
        (!session.is_empty()).then(|| Pane { session: session.to_owned(), pane })
    });
    pane.ok_or_else(|| format!("{spelled} is no pane: name one as <session>:<pane>"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seat {
    Opened(ZellijPane),
    Own(OwnPane),
    Asked { turn: Turn, user: User },
}

impl Seat {
    pub fn turn(&self) -> Option<Turn> {
        match self {
            Seat::Asked { turn, .. } => Some(*turn),
            Seat::Opened(_) | Seat::Own(_) => None,
        }
    }
}

pub fn here() -> Result<Option<Seat>, String> {
    let (key, before) = (var(TURN_VAR)?, var(BEFORE_VAR)?);
    let (asked, own, away) = (var(ASKED_VAR)?, var(OWN_VAR)?, var(AWAY_VAR)?);
    let own_before = var(OWN_VAR_BEFORE)?;
    let said = asks::Where {
        asked: asked.as_deref(),
        own: own.as_deref(),
        away: away.as_deref(),
        own_before: own_before.as_deref(),
    };
    let turn = asks::turn_in(key.as_deref(), before.as_deref(), said);
    seat_in(turn?, Zellij::here()?, own::here()?)
}

fn seat_in(
    asked: Option<(Turn, User)>,
    here: PaneVars,
    own: Option<OwnPane>,
) -> Result<Option<Seat>, String> {
    match (asked, here.named(), own) {
        (Some((_, user)), Some(session), _) => {
            Err(asked_in_pane(&user, &format!("a pane of {session}")))
        },
        (Some((_, user)), None, Some(own)) => {
            let (pane, session) = (own.id, own.session.id);
            Err(asked_in_pane(&user, &format!("pane {pane} of own session {session}")))
        },
        (Some((turn, user)), None, None) => Ok(Some(Seat::Asked { turn, user })),
        (None, Some(_), _) => Ok(here.pane_named().map(Seat::Opened)),
        (None, None, own) => Ok(own.map(Seat::Own)),
    }
}

fn asked_in_pane(user: &User, in_pane: &str) -> String {
    let asked = match user {
        User::Pane { session: from, pane } => {
            format!("{ASKED_VAR} names {from}:{pane}, a pane luneta's ask box asked from")
        },
        User::Own { pane, named, .. } => {
            format!("{OWN_VAR} names {named}:{}, a pane luneta's ask box asked from", pane.id)
        },
        User::Away => format!("{AWAY_VAR} says luneta's ask box asked from another machine"),
        User::Untold => format!("{TURN_VAR} names a turn of luneta's ask box"),
    };
    format!("{asked}, and this runs in {in_pane}")
}

pub fn make(session: &SessionName) -> (String, Vec<String>) {
    client_with(Zellij::client_making(session))
}

pub fn rejoin(session: &SessionName) -> Result<(String, Vec<String>), String> {
    Zellij::client_rejoining(session).map(client_with)
}

fn client_with(words: ClientWords) -> (String, Vec<String>) {
    let ClientWords { program, args, unset, .. } = words;
    let mut through: Vec<String> =
        unset.iter().flat_map(|name| ["-u".to_owned(), (*name).to_owned()]).collect();
    through.push(program.to_owned());
    through.extend(args);
    ("env".to_owned(), through)
}

pub fn attach_away(
    destination: &Destination,
    session: &SessionName,
) -> Result<(String, Vec<String>), String> {
    Ok(away(destination, spelled(&Zellij::client_rejoining(session)?)))
}

pub fn make_away(
    destination: &Destination,
    session: &SessionName,
    cwd: Option<&Directory>,
) -> (String, Vec<String>) {
    let cd = cwd.map_or_default(|cwd| format!("cd {} && ", quoted(cwd.as_str())));
    away(destination, format!("{cd}{}", spelled(&Zellij::client_making(session))))
}

fn away(destination: &Destination, command: String) -> (String, Vec<String>) {
    let mut args = ["-t", "-o", "LogLevel=ERROR"].map(str::to_owned).to_vec();
    let sharing = lunetad::folder::find().map(|folder| lunetad::sharing(&folder, destination));
    let sharing = sharing.and_then(|options| {
        options.into_iter().map(OsString::into_string).collect::<Result<Vec<_>, _>>().ok()
    });
    args.extend(sharing.into_iter().flatten());
    args.push(destination.to_string());
    args.push(command);
    ("ssh".to_owned(), args)
}

fn spelled(words: &ClientWords) -> String {
    let mut spelled = vec![words.program.to_owned()];
    spelled.extend(words.args.iter().map(|arg| quoted(arg)));
    spelled.join(" ")
}

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

pub fn ended(seats: &[SessionName]) -> Ended {
    let folder = Zellij::socket_folder();
    moves::refuge(&folder, seats, clients_of_refuge)
}

fn clients_of_refuge(session: &SessionName) -> Option<Vec<Client>> {
    if may_hold_terminal(session) {
        return None;
    }
    moves::before(session)
}

const SSH_VARS: [&str; 2] = ["SSH_CONNECTION", "SSH_TTY"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Within {
    Here,
    MaybeAway,
}

pub fn within() -> Within {
    let set = |var: &&str| env::var_os(var).is_some();
    if SSH_VARS.iter().any(set) || Zellij::may_be_in_a_pane() {
        Within::MaybeAway
    } else {
        Within::Here
    }
}

pub fn may_hold_terminal(session: &SessionName) -> bool {
    let Ok(here) = Zellij::here() else {
        return true;
    };
    if here.named() == Some(session.as_str()) {
        return true;
    }
    let Some(pane) = here.pane() else {
        return false;
    };
    let Ok(laid) = Mux::Zellij(session).laid() else {
        return true;
    };
    laid.iter().filter(|listed| listed.pane == PaneId::Terminal(pane)).any(|listed| {
        let runs = listed.command.as_ref().map(Said::as_str);
        runs.is_none_or(|command| icons::program(command) == Some("luneta"))
    })
}

pub fn newest() -> Option<Refuge> {
    moves::newest(&Zellij::socket_folder(), clients_of_refuge)
}

pub fn answering_away(
    destination: &Destination,
    sessions: Vec<SessionName>,
    limit: Duration,
) -> Result<Option<Refuge>, String> {
    let mut away = Away::reach(destination, limit)?;
    Ok(moves::answering(sessions, |session| away.clients(session.as_str()).ok()))
}

pub fn ended_away(destination: &Destination, seats: &[SessionName]) -> Result<Ended, String> {
    let found = Away::reach(destination, away::ANSWER)?.refuge(seats)?;
    match found.ended {
        Some(refuge_response::Ended::Kept(())) => Ok(Ended::Kept),
        Some(refuge_response::Ended::Went(went)) => {
            let refuge = went.refuge.map(|refuge| -> Result<Refuge, String> {
                let session = SessionName::running(&refuge.session).map_err(|why| {
                    format!("{destination}: lunetad went on in \"{}\": {why}", refuge.session)
                })?;
                Ok(Refuge { session, before: refuge.before, started: None })
            });
            Ok(Ended::Went(refuge.transpose()?))
        },
        None => Err(format!(
            "{destination}: its lunetad says what became of the session in words this luneta \
             cannot read, as a newer one does"
        )),
    }
}

pub fn nudge(mode: TermMode) -> Option<&'static [u8]> {
    Zellij::nudge(mode.contains(TermMode::DISAMBIGUATE_ESC_CODES))
}

const ARRIVAL: Duration = Duration::from_secs(2);

pub const START: Duration = Duration::from_secs(10);

const ARRIVING: Duration = Duration::from_millis(20);

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
        (_, Some(after)) => {
            after.untold(events);
            return;
        },
    };
    let (on, at, to) = (nudged.on().clone(), at.clone(), to.clone());
    run(Action::Go { on, at, to, pane }, then, nudged.until(), events);
}

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
        (None, Some(alone)) => {
            alone.after().untold(events);
            None
        },
    }
}

pub struct Alone {
    owed: Due<u32>,
    tab: Tab,
    session: SessionName,
    since: Option<SystemTime>,
}

impl Alone {
    pub fn new(owed: Due<u32>, tab: Tab, made: &Unused) -> Self {
        Alone { owed, tab, session: made.name.clone(), since: made.since }
    }

    pub fn after(self) -> After {
        let Alone { owed, tab, session, since } = self;
        After::Open(owed, Open::Tab { session, tab, alone_since: since })
    }
}

const UNMOVED: &str = "zellij moved nobody: it moves the client that last typed in the session \
                       it is asked from, and none has typed there since it came";

pub fn taken(at: &SessionName, to: &SessionName, pane: Option<u32>) -> Result<(), String> {
    if at == to {
        return pane.map_or(Ok(()), |pane| focused(at, PaneId::Terminal(pane)));
    }
    let before = clients_on_panes(to)?;
    Mux::Zellij(at).went(to, pane)?;
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

pub fn focused(session: &SessionName, pane: PaneId) -> Result<(), String> {
    Mux::Zellij(session).pane_focused(pane)?;
    let on = |clients: &[mux::Client]| clients.iter().any(|client| client.pane == pane);
    seen(session, |clients| on(clients).then_some(())).ok_or_else(|| UNMOVED.to_owned())
}

pub fn made(at: &SessionName, to: &SessionName, cwd: &Directory) -> Result<(), String> {
    let asked = SystemTime::now();
    Mux::Zellij(at).started(to, Some(cwd))?;
    match made_since(to, asked, START) {
        Some(true) => Ok(()),
        Some(false) => Err(format!("{to} ran already, so zellij made none")),
        None => Err(UNMOVED.to_owned()),
    }
}

fn made_since(session: &SessionName, asked: SystemTime, patience: Duration) -> Option<bool> {
    started(session, patience).map(|started| started >= asked)
}

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

fn started(session: &SessionName, patience: Duration) -> Option<SystemTime> {
    let folder = Zellij::socket_folder();
    let deadline = Instant::now() + patience;
    loop {
        let running = folder.running().unwrap_or_default();
        let found = running.into_iter().find(|(name, _)| name == session);
        if let Some((_, started)) = found {
            return Some(started);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(ARRIVING);
    }
}

pub enum After {
    Open(Due<u32>, Open),
    IntoTab(Due<u32>, Turned),
    Kill { session: SessionName, whose: Whose },
}

impl After {
    fn tab(owed: Due<u32>, session: &SessionName, tab: Tab) -> Self {
        After::Open(owed, Open::Tab { session: session.clone(), tab, alone_since: None })
    }

    pub fn untold(self, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) | After::IntoTab(owed, ..) => {
                owed.pay(Err("luneta cannot tell when zellij's client is there".to_owned()));
            },
            After::Kill { session, .. } => {
                let when = format!("when zellij's client has left \"{session}\", which stays");
                let _ = events.send(Event::Notice(format!("luneta cannot tell {when}")));
            },
        }
    }

    fn came(self, there: &mut There, events: &Sender<Event>) {
        match (self, there) {
            (After::Open(owed, open), there) => {
                owed.pay(there.open(&open).map(|opened| told(opened, events)));
            },
            (After::IntoTab(owed, turned), there) => {
                let (session, pane) = (turned.place.session.name(), turned.place.pane);
                let came = there.turn_into_tab(session, turned.started, pane);
                owed.pay(came.and_then(|came| match came {
                    IntoTab::Done => Ok(pane),
                    IntoTab::NoClient => Err(lunetad::opens::no_client(session)),
                }));
            },
            (After::Kill { session, whose }, There::Here) => {
                kill(&Machine::Local, session, whose, events);
            },
            (After::Kill { session, whose }, There::Away(away)) => {
                if let Err(why) = away.kill(&session, whose) {
                    let _ = events.send(Event::Notice(why));
                }
            },
        }
    }

    fn stayed(self, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) | After::IntoTab(owed, ..) => {
                owed.pay(Err("zellij did not take its client there".to_owned()));
            },
            After::Kill { session, .. } => {
                let _ = events.send(Event::Notice(format!("\"{session}\" stays")));
            },
        }
    }

    fn failed(self, why: String, events: &Sender<Event>) {
        match self {
            After::Open(owed, _) | After::IntoTab(owed, ..) => owed.pay(Err(why)),
            kill @ After::Kill { .. } => kill.stayed(events),
        }
    }
}

pub fn open(menu: u64, on: &Machine, open: Open, events: &Sender<Event>) {
    let (owed, on, events) = (owed(menu, events), on.clone(), events.clone());
    thread::spawn(move || {
        let opened = There::on(&on).and_then(|mut there| there.open(&open));
        owed.pay(opened.map(|opened| told(opened, &events)));
    });
}

fn told(opened: Opened, events: &Sender<Event>) -> u32 {
    let Opened { terminal, notes } = opened;
    for note in notes {
        let _ = events.send(Event::Notice(note));
    }
    terminal
}

pub fn owed(menu: u64, events: &Sender<Event>) -> Due<u32> {
    Due::new(events, move |opened| Event::Opened { menu, opened })
}

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
        let _ = events.send(Event::Tabs { menu, tabs });
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Turned {
    place: PanePlace,
    started: SystemTime,
}

impl Turned {
    pub fn of(place: PanePlace) -> Option<Self> {
        match place.session.whose() {
            Whose::Zellij(started) => Some(Turned { place, started }),
            Whose::Own(_) => None,
        }
    }

    pub fn place(&self) -> &PanePlace {
        &self.place
    }
}

pub const OWN_TURNS: &str = "luneta's own sessions cannot turn a pane yet";

pub fn turn(menu: u64, on: &Machine, turned: Turned, turn: turns::Turn, events: &Sender<Event>) {
    match turn {
        turns::Turn::PaneIntoTab => pane_into_tab(menu, on, turned, events),
        turns::Turn::TabIntoPanes { beside } => tab_into_panes(menu, on, &turned, beside, events),
    }
}

fn pane_into_tab(menu: u64, on: &Machine, turned: Turned, events: &Sender<Event>) {
    let (owed, on, events) = (owed(menu, events), on.clone(), events.clone());
    thread::spawn(move || {
        let (session, pane) = (turned.place.session.name(), turned.place.pane);
        let started = turned.started;
        match There::on(&on).and_then(|mut there| there.turn_into_tab(session, started, pane)) {
            Ok(IntoTab::Done) => owed.pay(Ok(pane)),
            Ok(IntoTab::NoClient) => {
                let _ = events.send(Event::NoClient { menu, owed, on, turned });
            },
            Err(why) => owed.pay(Err(why)),
        }
    });
}

fn tab_into_panes(menu: u64, on: &Machine, turned: &Turned, beside: u32, events: &Sender<Event>) {
    let (session, pane) = (turned.place.session.name().clone(), turned.place.pane);
    let owed = Due::new(events, move |came| Event::IntoPanes { menu, pane, came });
    let (on, started) = (on.clone(), turned.started);
    thread::spawn(move || {
        let came = There::on(&on)
            .and_then(|mut there| there.turn_into_panes(&session, started, pane, beside));
        owed.pay(came);
    });
}

pub fn closes(menu: u64, on: &Machine, session: SessionName, pane: u32, events: &Sender<Event>) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        let closes = There::on(&on).and_then(|mut there| there.closes(&session, pane));
        let _ = events.send(Event::Closes { menu, closes });
    });
}

pub fn close(
    on: &Machine,
    session: SessionName,
    whose: Whose,
    close: Close,
    events: &Sender<Event>,
) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        let closed = There::on(&on).and_then(|mut there| there.close(&session, whose, &close));
        if let Err(why) = closed {
            let _ = events.send(Event::Notice(why));
        }
    });
}

pub fn kill(
    on: &Machine,
    session: SessionName,
    whose: Whose,
    events: &Sender<Event>,
) -> JoinHandle<()> {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || {
        if let Err(why) = There::on(&on).and_then(|mut there| there.kill(&session, whose)) {
            let _ = events.send(Event::Notice(why));
        }
    })
}

pub fn running() -> Result<Vec<(SessionName, SystemTime)>, String> {
    running_in(&Zellij::socket_folder())
}

pub fn came_here(after: After, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || after.came(&mut There::Here, &events));
}

pub fn look(on: &Machine, look: Look, events: &Sender<Event>) {
    let (on, events) = (on.clone(), events.clone());
    thread::spawn(move || match There::on(&on) {
        Ok(mut there) => looking(&mut there, &look, &events),
        Err(why) => drop(events.send(Event::Notice(why))),
    });
}

pub struct Arrival {
    pub look: Look,
    before: Before,
    on: Machine,
    after: Option<After>,
}

impl Arrival {
    pub fn new(on: Machine, look: Look, before: Before, after: Option<After>) -> Self {
        Arrival { look, before, on, after }
    }

    pub fn fresh(&self) -> bool {
        self.before.fresh()
    }

    pub fn stayed(self, events: &Sender<Event>) {
        if let Some(after) = self.after {
            after.stayed(events);
        }
    }

    pub fn failed(self, why: String, events: &Sender<Event>) {
        if let Some(after) = self.after {
            after.failed(why, events);
        }
    }
}

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
            let _ = events.send(Event::Notice(why.clone()));
        }
        match (after, failure) {
            (Some(after), Some(why)) => after.failed(why, &events),
            (Some(after), None) => after.stayed(&events),
            (None, _) => {},
        }
    });
}

pub fn before(session: &SessionName) -> Before {
    if runs(session) == Some(false) {
        return Before::Made;
    }
    clients(session).map_or(Before::Unknown, Before::Listed)
}

pub struct Unused {
    on: Machine,
    name: SessionName,
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

fn name_away(destination: &Destination, starts: &Starts) -> Result<(Unused, Directory), String> {
    let named = Away::reach(destination, away::QUICK).and_then(|mut away| away.name(starts));
    let (name, since, dir) = named?;
    Ok((Unused { on: Machine::Remote(destination.clone()), name, since }, dir))
}

pub fn unused_at(on: &Machine, starts: Starts) -> Result<(Unused, Directory), String> {
    match on {
        Machine::Local => {
            let (name, since, dir) = lunetad::names::free_at(starts)?;
            Ok((Unused { on: Machine::Local, name, since: Some(since) }, dir))
        },
        Machine::Remote(destination) => name_away(destination, &starts),
    }
}

pub fn before_away(destination: &Destination, session: &SessionName) -> Before {
    let reached = Away::reach(destination, away::QUICK);
    let listed = reached.and_then(|mut away| away.clients(session.as_str()));
    listed.map_or(Before::Unknown, Before::Listed)
}

fn runs(session: &SessionName) -> Option<bool> {
    let running = Zellij::socket_folder().running()?;
    Some(running.iter().any(|(name, _)| name == session))
}

pub fn makes(to: SessionName) -> Result<Unused, String> {
    let since = lunetad::names::unused(&to).map_err(|why| format!("no session made: {why}"))?;
    Ok(Unused { on: Machine::Local, name: to, since: Some(since) })
}

pub fn makes_there(destination: &Destination, to: SessionName) -> Result<Unused, String> {
    let checked = Away::reach(destination, away::QUICK).and_then(|mut away| away.check(&to));
    let since = checked.map_err(|why| format!("no session made: {why}"))?;
    Ok(Unused { on: Machine::Remote(destination.clone()), name: to, since: Some(since) })
}

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

pub fn clients(session: &SessionName) -> Result<Vec<Client>, String> {
    let clients = clients_on_panes(session)?;
    Ok(clients.iter().copied().map(mux::Client::to_api).collect())
}

pub fn clients_on_panes(session: &SessionName) -> Result<mux::Listed, String> {
    Ok(Mux::Zellij(session).clients()?)
}

enum Then {
    Look(Look),
    Arrive(Look, Option<After>),
    Made(u64),
}

impl Then {
    fn undone(self, events: &Sender<Event>) {
        match self {
            Then::Look(_) => {},
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

enum Action {
    Go { on: Machine, at: SessionName, to: SessionName, pane: Option<u32> },
    Start { at: SessionName, to: SessionName, cwd: Option<Directory> },
}

impl Action {
    fn machine(&self) -> Machine {
        match self {
            Action::Go { on, .. } => on.clone(),
            Action::Start { .. } => Machine::Local,
        }
    }
}

enum There {
    Here,
    Away(Box<Away>),
}

impl There {
    fn on(on: &Machine) -> Result<There, String> {
        match on {
            Machine::Local => Ok(There::Here),
            Machine::Remote(destination) => {
                Away::reach(destination, away::ANSWER).map(|away| There::Away(Box::new(away)))
            },
        }
    }

    fn clients(&mut self, session: &str) -> Result<Vec<Client>, String> {
        match self {
            There::Here => clients(&SessionName::running(session)?),
            There::Away(away) => away.clients(session),
        }
    }

    fn before(&mut self, session: &str) -> Before {
        match self {
            There::Here => SessionName::running(session).map_or(Before::Unknown, |at| before(&at)),
            There::Away(away) => away.clients(session).map_or(Before::Unknown, Before::Listed),
        }
    }

    fn open(&mut self, open: &Open) -> Result<Opened, String> {
        match self {
            There::Here => opening(open),
            There::Away(away) => away.open(open),
        }
    }

    fn other_tabs(&mut self, session: &SessionName, pane: u32) -> Result<Vec<OtherTab>, String> {
        match self {
            There::Here => turns::other_tabs(session, pane),
            There::Away(away) => away.other_tabs(session, pane),
        }
    }

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

    fn closes(&mut self, session: &SessionName, pane: u32) -> Result<Closes, String> {
        match self {
            There::Here => closes_now(session, pane),
            There::Away(away) => away.closes(session, pane),
        }
    }

    fn close(&mut self, session: &SessionName, whose: Whose, close: &Close) -> Result<(), String> {
        match self {
            There::Here => whose.closed(session, close),
            There::Away(away) => away.close(session, whose, close),
        }
    }

    fn kill(&mut self, session: &SessionName, whose: Whose) -> Result<(), String> {
        match self {
            There::Here => whose.killed(session),
            There::Away(away) => away.kill(session, whose),
        }
    }

    fn take(&mut self, action: Action) -> Result<(), String> {
        match action {
            Action::Go { at, to, pane, .. } => match self {
                There::Away(away) => away.go(&at, &to, pane),
                There::Here => Ok(Mux::Zellij(&at).went(&to, pane)?),
            },
            Action::Start { at, to, cwd } => Ok(Mux::Zellij(&at).started(&to, cwd.as_ref())?),
        }
    }
}

fn run(action: Action, then: Option<Then>, when: Instant, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let mut there = match There::on(&action.machine()) {
            Ok(there) => there,
            Err(why) => {
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
                    let _ = events.send(Event::Stayed(look));
                }
            },
        }
    });
}

fn act(there: &mut There, action: Action, when: Instant, events: &Sender<Event>) -> bool {
    thread::sleep(when.saturating_duration_since(Instant::now()));
    let failure = there.take(action).err();
    if let Some(said) = &failure {
        let _ = events.send(Event::Notice(said.clone()));
    }
    failure.is_none()
}

fn looking(there: &mut There, look: &Look, events: &Sender<Event>) {
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

    #[test]
    fn a_command_runs_in_the_pane_zellij_names_or_goes_by_the_one_the_ask_box_does() {
        let here = |named: Option<&str>, pane| PaneVars::spelled(named.map(str::to_owned), pane);
        let opened = Seat::Opened(ZellijPane { session: "work".to_owned(), pane: 2 });
        let made = mux::OwnMade::from_nanos(0);
        let key = mux::OwnKey { id: mux::OwnId::new(3).unwrap(), made };
        let own = OwnPane::new(key, 12);
        assert_eq!(seat_in(None, here(Some("work"), Some("2")), None), Ok(Some(opened.clone())));
        assert_eq!(seat_in(None, here(Some("work"), Some("2")), own), Ok(Some(opened)));
        let in_own = Seat::Own(own.unwrap());
        assert_eq!(seat_in(None, here(None, None), own), Ok(Some(in_own)));
        assert_eq!(seat_in(None, here(Some("work"), None), own), Ok(None));
        let unnamed = [
            (Some(""), Some("2")),
            (Some("work"), Some("terminal_2")),
            (Some("work"), None),
            (None, Some("2")),
            (None, None),
        ];
        for (session, pane) in unnamed {
            assert_eq!(seat_in(None, here(session, pane), None), Ok(None), "{session:?} {pane:?}");
        }
        let turn = Turn::new(255, Some(10)).unwrap();
        let work = User::Pane { session: SessionName::running("work").unwrap(), pane: 4 };
        let named = SessionName::running("mine").unwrap();
        let client = own::OwnClientId::new(8).unwrap();
        let mine = User::Own { pane: OwnPane::new(key, 5).unwrap(), named, client };
        for user in [work, mine, User::Away, User::Untold] {
            let asked = Ok(Some(Seat::Asked { turn, user: user.clone() }));
            assert_eq!(seat_in(Some((turn, user.clone())), here(None, None), None), asked);
            assert_eq!(seat_in(Some((turn, user.clone())), here(Some(""), Some("2")), None), asked);
            let in_a_pane = seat_in(Some((turn, user.clone())), here(Some("play"), None), None);
            assert!(in_a_pane.is_err_and(|why| why.ends_with("this runs in a pane of play")));
            let in_own = seat_in(Some((turn, user.clone())), here(None, None), own);
            let says = "this runs in pane 12 of own session 3";
            assert!(in_own.is_err_and(|why| why.ends_with(says)));
        }
    }

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
