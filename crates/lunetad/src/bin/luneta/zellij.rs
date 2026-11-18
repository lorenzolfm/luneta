use std::fmt;
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use lunetad::mux::{self, Mux, PaneId, PaneVars, Zellij, ZellijPane};
pub use lunetad::closes::{closes_now, closes_together, Close, Closes, Closing};
use lunetad::closes::{self, running_in};
pub use lunetad::names::{session_for, Starts};
pub use lunetad::opens::{opening, Open, Opened, Tab};
use lunetad::asks::{self, Turn, User, ASKED_VAR, AWAY_VAR, BEFORE_VAR, TURN_VAR};
use lunetad::turns::{self, IntoPanes, IntoTab, OtherTab};
use lunetad::Directory;
pub use lunetad::{var, SessionName};

use crate::away::{self, Away};
use crate::close::Whose;
use crate::due::Due;
use crate::host::Event;
use crate::machines::Machine;
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
    Asked { turn: Turn, user: User },
}

impl Seat {
    pub fn turn(&self) -> Option<Turn> {
        match self {
            Seat::Asked { turn, .. } => Some(*turn),
            Seat::Opened(_) => None,
        }
    }
}

pub fn here() -> Result<Option<Seat>, String> {
    let (key, before) = (var(TURN_VAR)?, var(BEFORE_VAR)?);
    let (asked, away) = (var(ASKED_VAR)?, var(AWAY_VAR)?);
    let said = asks::Where { asked: asked.as_deref(), away: away.as_deref() };
    let turn = asks::turn_in(key.as_deref(), before.as_deref(), said);
    seat_in(turn?, Zellij::here()?)
}

fn seat_in(asked: Option<(Turn, User)>, here: PaneVars) -> Result<Option<Seat>, String> {
    match (asked, here.named()) {
        (Some((_, user)), Some(session)) => {
            Err(asked_in_pane(&user, &format!("a pane of {session}")))
        },
        (Some((turn, user)), None) => Ok(Some(Seat::Asked { turn, user })),
        (None, Some(_)) => Ok(here.pane_named().map(Seat::Opened)),
        (None, None) => Ok(None),
    }
}

fn asked_in_pane(user: &User, in_pane: &str) -> String {
    let asked = match user {
        User::Pane { session: from, pane } => {
            format!("{ASKED_VAR} names {from}:{pane}, a pane luneta's ask box asked from")
        },
        User::Away => format!("{AWAY_VAR} says luneta's ask box asked from another machine"),
        User::Untold => format!("{TURN_VAR} names a turn of luneta's ask box"),
    };
    format!("{asked}, and this runs in {in_pane}")
}

const ARRIVAL: Duration = Duration::from_secs(2);

pub const START: Duration = Duration::from_secs(10);

const ARRIVING: Duration = Duration::from_millis(20);

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

pub const NOT_THERE_YET: &str = "luneta cannot tell when its client is there";

pub fn stays(session: &SessionName, events: &Sender<Event>) {
    let when = format!("when its client has left \"{session}\", which stays");
    let _ = events.send(Event::Notice(format!("luneta cannot tell {when}")));
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

pub fn other_tabs(menu: u64, on: &Machine, turned: &Turned, events: &Sender<Event>) {
    let (on, events) = (on.clone(), events.clone());
    let (session, pane) = (turned.place.session.name().clone(), turned.place.pane.id());
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
        let (session, pane) = (turned.place.session.name(), turned.place.pane.id());
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
    let (session, pane) = (turned.place.session.name().clone(), turned.place.pane.id());
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

pub fn clients_on_panes(session: &SessionName) -> Result<mux::Listed, String> {
    Ok(Mux::Zellij(session).clients()?)
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
        let started = whose.started(session)?;
        match self {
            There::Here => closes::closed(session, started, close),
            There::Away(away) => away.close(session, started, close),
        }
    }

    fn kill(&mut self, session: &SessionName, whose: Whose) -> Result<(), String> {
        let started = whose.started(session)?;
        match self {
            There::Here => closes::killed(session, started),
            There::Away(away) => away.kill(session, started),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_runs_in_the_pane_zellij_names_or_goes_by_the_one_the_ask_box_does() {
        let here = |named: Option<&str>, pane| PaneVars::spelled(named.map(str::to_owned), pane);
        let opened = Seat::Opened(ZellijPane { session: "work".to_owned(), pane: 2 });
        assert_eq!(seat_in(None, here(Some("work"), Some("2"))), Ok(Some(opened)));
        let unnamed = [
            (Some(""), Some("2")),
            (Some("work"), Some("terminal_2")),
            (Some("work"), None),
            (None, Some("2")),
            (None, None),
        ];
        for (session, pane) in unnamed {
            assert_eq!(seat_in(None, here(session, pane)), Ok(None), "{session:?} {pane:?}");
        }
        let turn = Turn::new(255, Some(10)).unwrap();
        let work = User::Pane { session: SessionName::running("work").unwrap(), pane: 4 };
        for user in [work, User::Away, User::Untold] {
            let asked = Ok(Some(Seat::Asked { turn, user: user.clone() }));
            assert_eq!(seat_in(Some((turn, user.clone())), here(None, None)), asked);
            assert_eq!(seat_in(Some((turn, user.clone())), here(Some(""), Some("2"))), asked);
            let in_a_pane = seat_in(Some((turn, user.clone())), here(Some("play"), None));
            assert!(in_a_pane.is_err_and(|why| why.ends_with("this runs in a pane of play")));
        }
    }
}
