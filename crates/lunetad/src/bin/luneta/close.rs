//! Deleting what the menu is for: its pane, or its session. What goes, and
//! which agents end with it.
//!
//! zellij closes a tab with its last pane that holds it open, and a session
//! with its last tab: luneta's client too, when that is the session luneta's
//! zellij is in. In a session no client is attached to, it leaves the tab
//! there with nothing in it instead, and a session with no pane. So a pane
//! that is the last to hold its tab open closes the tab, and the last to hold
//! a session's only tab open deletes the session. The session luneta's zellij
//! is in goes once its client is in another, which keeps the client on
//! screen throughout, or with luneta, where it is the only one.

use std::fmt;
use std::time::SystemTime;

use luneta_protocol::unread::Unplaced;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Status};

use crate::sidebar::{Listed, Place};
use crate::zellij::Closes;

/// What the menu says of deleting a session while luneta's zellij is on its
/// way to one: it may be the session luneta is in, or not.
const MOVING: &str = "menu: zellij has yet to say which session it is in; try again in a moment";

/// What a delete says where luneta cannot tell which agents it ends, before
/// why.
const UNTOLD: &str = "luneta cannot tell which agents it ends";

/// What a key of the menu's Delete group deletes: the terminal of this id,
/// or the session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Pane(u32),
    Session,
}

impl Scope {
    /// What the menu says the key does.
    pub fn does(self) -> &'static str {
        match self {
            Scope::Pane(_) => "close the pane",
            Scope::Session => "delete the session",
        }
    }
}

/// What a delete deletes, as it is planned: a pane, and what closing it
/// closes, as zellij listed the session's panes just before; or a session.
#[derive(Debug, PartialEq, Eq)]
pub enum Deletes {
    Pane { pane: u32, closes: Closes },
    Session,
}

/// Where luneta's zellij is, as deleting a session goes by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    /// In the session the delete is for.
    In,
    /// In another session.
    Elsewhere,
    /// On its way to the session luneta sent it to, or yet to say which it
    /// is in: it may be in either.
    Moving,
}

/// What an agent is doing, as far as deleting it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doing {
    Idle,
    Busy,
    Waiting,
    /// A word luneta does not know, or none: it may be at work.
    Unknown,
    /// A shell, which ends with nothing to lose.
    Shell,
}

impl From<&Agent> for Doing {
    /// What an agent is doing, as lunetad says it.
    fn from(agent: &Agent) -> Self {
        match &agent.status {
            Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
                Ok(Status::Busy) => Doing::Busy,
                Ok(Status::Idle) => Doing::Idle,
                Ok(Status::Waiting) => Doing::Waiting,
                Ok(Status::Shell) => Doing::Shell,
                // A daemon newer than this luneta's.
                Ok(Status::Unspecified) | Err(_) => Doing::Unknown,
            },
            Some(Reported::OtherStatus(_)) | None => Doing::Unknown,
        }
    }
}

/// Why luneta cannot tell which agents a delete ends. A question says it
/// alike each time it asks, so that `y` goes ahead with what it asked about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Untold {
    /// lunetad does not answer: what it said last of the agents may be out of
    /// date.
    Away,
    /// lunetad cannot read some of Claude Code's session files, nor tell
    /// which session the process of each runs in, so their agents may be in
    /// any pane of any session.
    Unplaced(Unplaced),
}

impl fmt::Display for Untold {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Untold::Away => f.write_str("lunetad does not answer"),
            Untold::Unplaced(unplaced) => unplaced.fmt(f),
        }
    }
}

/// What the agents of `session` among `unread` are doing, each by its pane:
/// those of the session files lunetad cannot read, by where their processes
/// run, as `unread_seats` gives them. What each is doing no one can tell, so
/// each may be at work.
pub fn unread_in<'a>(
    unread: &'a [(&str, u32)],
    session: &'a str,
) -> impl Iterator<Item = (u32, Doing)> + 'a {
    unread.iter().filter(move |(at, _)| *at == session).map(|(_, pane)| (*pane, Doing::Unknown))
}

/// What a delete is for, and what it takes along, as the sidebar knows it.
#[derive(Debug)]
pub struct Subject<'a> {
    pub session: &'a Listed,
    /// Where luneta's zellij is.
    pub seat: Seat,
    pub deletes: Deletes,
    /// The agents of the session, each by its pane, or why luneta cannot
    /// tell them.
    pub agents: Result<Vec<(u32, Doing)>, Untold>,
    /// Where luneta's zellij goes before the session it is in is deleted: the
    /// newest of the others, which the picker lists first. None for the only
    /// session, which luneta ends with.
    pub refuge: Option<Place>,
}

/// What a delete key asks zellij for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Close a pane of a session.
    Pane { session: String, pane: u32 },
    /// Close a tab of a session, by its id, and the terminals in it, the
    /// floating ones too.
    Tab { session: String, tab: u32, panes: Vec<u32> },
    /// Kill the session that started at `started`, and delete it, so that
    /// zellij keeps nothing to bring it back with. A key plans it for a
    /// session luneta's zellij is not in, or for the only one, which luneta's
    /// zellij ends with, and luneta with it; `luneta close` for any, as a
    /// client that ends with its session goes on in another.
    Kill { session: String, started: SystemTime },
    /// Take luneta's zellij to `to`, and kill and delete `session`, which it
    /// is in, the one that started at `started`, once it has left.
    Leave { session: String, started: SystemTime, to: Place },
}

impl Act {
    /// What it closes, as a question begins.
    fn what(&self) -> String {
        match self {
            Act::Pane { .. } => "Close the pane".to_owned(),
            Act::Tab { .. } => "Close its tab".to_owned(),
            Act::Kill { session, .. } | Act::Leave { session, .. } => {
                format!("Delete session {session}")
            },
        }
    }

    /// Whether the terminal `pane` of its session goes with it.
    pub fn takes(&self, pane: u32) -> bool {
        match self {
            Act::Pane { pane: closed, .. } => pane == *closed,
            Act::Tab { panes, .. } => panes.contains(&pane),
            Act::Kill { .. } | Act::Leave { .. } => true,
        }
    }
}

/// What a delete key does, and the agents it would end, while luneta can tell
/// them. Two are the same when both do the same and end the same agents,
/// doing the same.
#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub act: Act,
    ending: Result<Ending, Untold>,
}

/// What deleting what `subject` deletes does, or why it does nothing. The
/// pane closes, or its tab once nothing else holds the tab open. The last
/// pane that holds a session's only tab open ends the session with it, and
/// deletes the session: the session luneta's zellij is in goes once the
/// client is in another, as it would for a key that deletes it, or with
/// luneta, where it is the only one; and none goes while luneta cannot tell
/// which session that is.
pub fn plan(subject: &Subject) -> Result<Plan, String> {
    let (session, started) = (subject.session.name().to_owned(), subject.session.started());
    let act = match &subject.deletes {
        Deletes::Pane { pane, closes: Closes::Pane } => Act::Pane { session, pane: *pane },
        Deletes::Pane { closes: Closes::Tab { id, panes }, .. } => {
            Act::Tab { session, tab: *id, panes: panes.clone() }
        },
        Deletes::Pane { closes: Closes::Session, .. } | Deletes::Session => match subject.seat {
            Seat::Elsewhere => Act::Kill { session, started },
            Seat::In => match subject.refuge.clone() {
                Some(to) => Act::Leave { session, started, to },
                None => Act::Kill { session, started },
            },
            Seat::Moving => return Err(MOVING.to_owned()),
        },
    };
    Ok(Plan::of(act, subject.agents.as_deref().map_err(|untold| *untold)))
}

/// The agents a delete would end, by what they are doing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Ending {
    agents: usize,
    busy: usize,
    waiting: usize,
    unknown: usize,
}

impl Ending {
    fn of(doing: impl Iterator<Item = Doing>) -> Self {
        let mut ending = Ending::default();
        for doing in doing {
            match doing {
                Doing::Shell => continue,
                Doing::Busy => ending.busy += 1,
                Doing::Waiting => ending.waiting += 1,
                Doing::Unknown => ending.unknown += 1,
                Doing::Idle => {},
            }
            ending.agents += 1;
        }
        ending
    }

    /// Whether an agent that would end may be in the middle of something:
    /// busy, waiting on you, or doing what luneta cannot tell.
    fn at_work(self) -> bool {
        self.busy + self.waiting + self.unknown > 0
    }

    /// As `2 agents, 1 busy`.
    fn says(self) -> String {
        let mut said = vec![match self.agents {
            0 => "no agent".to_owned(),
            1 => "1 agent".to_owned(),
            agents => format!("{agents} agents"),
        }];
        if self.busy > 0 {
            said.push(format!("{} busy", self.busy));
        }
        if self.waiting > 0 {
            said.push(format!("{} waiting", self.waiting));
        }
        if self.unknown > 0 {
            said.push(format!("{} unknown", self.unknown));
        }
        said.join(", ")
    }
}

impl Plan {
    /// What doing `act` does, and the agents it ends among `agents`, those of
    /// its session by their panes, or why luneta cannot tell them.
    pub fn of(act: Act, agents: Result<&[(u32, Doing)], Untold>) -> Self {
        let ending = agents.map(|agents| {
            Ending::of(agents.iter().filter(|(pane, _)| act.takes(*pane)).map(|(_, doing)| *doing))
        });
        Plan { act, ending }
    }

    /// What to ask before going ahead, as `Delete session elo? 2 agents, 1
    /// busy`. Nothing when luneta knows that no agent busy or waiting on you
    /// would end: an idle agent or a shell goes without asking, and one whose
    /// status luneta cannot read asks, as does any delete while luneta cannot
    /// tell the agents, saying why.
    pub fn question(&self) -> Option<String> {
        match self.ending {
            Ok(ending) if !ending.at_work() => None,
            Ok(_) | Err(_) => Some(format!("{}? {}", self.act.what(), self.ends())),
        }
    }

    /// The agents it ends, as `2 agents, 1 busy`, or that luneta cannot tell
    /// them, and why, whether or not the menu would ask: for the CLI too,
    /// which says what a close ends before it closes anything.
    pub fn ends(&self) -> String {
        match self.ending {
            Ok(ending) => ending.says(),
            Err(untold) => format!("{UNTOLD}: {untold}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn elsewhere() -> Place {
        Place::Session(Listed::new("elo", SystemTime::UNIX_EPOCH + Duration::from_secs(9)))
    }

    /// The session the tests delete.
    fn luneta() -> Listed {
        Listed::new("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(3))
    }

    fn subject(
        session: &Listed,
        seat: Seat,
        deletes: Deletes,
        agents: Vec<(u32, Doing)>,
    ) -> Subject<'_> {
        let agents = Ok(agents);
        Subject { session, seat, deletes, agents, refuge: Some(elsewhere()) }
    }

    /// Pane 4, which closes what zellij says closing it closes.
    fn pane(closes: Closes) -> Deletes {
        Deletes::Pane { pane: 4, closes }
    }

    #[test]
    fn a_pane_closes_alone_its_tab_or_its_session_as_zellij_would_close_them() {
        let tab = Closes::Tab { id: 3, panes: vec![4, 5] };
        let luneta = luneta();
        let planned = |seat: Seat, deletes: Deletes| {
            plan(&subject(&luneta, seat, deletes, Vec::new())).map(|plan| plan.act)
        };
        let (session, started) = ("luneta".to_owned(), luneta.started());
        assert_eq!(
            planned(Seat::In, pane(Closes::Pane)),
            Ok(Act::Pane { session: session.clone(), pane: 4 })
        );
        assert_eq!(
            planned(Seat::In, pane(tab)),
            Ok(Act::Tab { session: session.clone(), tab: 3, panes: vec![4, 5] })
        );
        let kill = Act::Kill { session: session.clone(), started };
        assert_eq!(planned(Seat::Elsewhere, pane(Closes::Session)), Ok(kill.clone()));
        let leave = Act::Leave { session, started, to: elsewhere() };
        assert_eq!(planned(Seat::In, pane(Closes::Session)), Ok(leave.clone()));
        assert_eq!(planned(Seat::In, Deletes::Session), Ok(leave));
        assert_eq!(planned(Seat::Elsewhere, Deletes::Session), Ok(kill));
    }

    /// The only session has none to go to first: it is killed with luneta's
    /// zellij in it, and luneta ends with it. It asks first as any session
    /// does, by the agents it ends.
    #[test]
    fn the_only_session_is_killed_with_luneta_in_it() {
        let luneta = luneta();
        for deletes in [Deletes::Session, pane(Closes::Session)] {
            let agents = vec![(4, Doing::Busy)];
            let alone = Subject { refuge: None, ..subject(&luneta, Seat::In, deletes, agents) };
            let Ok(planned) = plan(&alone) else {
                panic!("the only session is killed");
            };
            let started = luneta.started();
            assert_eq!(planned.act, Act::Kill { session: "luneta".to_owned(), started });
            let asks = Some("Delete session luneta? 1 agent, 1 busy".to_owned());
            assert_eq!(planned.question(), asks);
        }
    }

    /// While luneta's zellij is on its way to a session, either session may
    /// be the one it is in: none goes, as luneta would have to leave it
    /// first, but a pane that leaves its session open closes.
    #[test]
    fn no_session_goes_while_luneta_is_on_its_way_to_one() {
        let luneta = luneta();
        for deletes in [Deletes::Session, pane(Closes::Session)] {
            let planned = plan(&subject(&luneta, Seat::Moving, deletes, Vec::new()));
            assert_eq!(planned.map(|plan| plan.act), Err(MOVING.to_owned()));
        }
        let planned = plan(&subject(&luneta, Seat::Moving, pane(Closes::Pane), Vec::new()));
        let session = "luneta".to_owned();
        assert_eq!(planned.map(|plan| plan.act), Ok(Act::Pane { session, pane: 4 }));
    }

    /// An idle agent or a shell goes without asking. A busy agent, one that
    /// waits on you, and one luneta cannot tell asks, and only those the
    /// delete ends count.
    #[test]
    fn a_delete_asks_first_only_where_an_agent_at_work_would_end() {
        let agents = vec![
            (4, Doing::Idle),
            (5, Doing::Busy),
            (6, Doing::Waiting),
            (7, Doing::Unknown),
            (8, Doing::Shell),
        ];
        let luneta = luneta();
        let question = |deletes: Deletes| {
            let planned = plan(&subject(&luneta, Seat::Elsewhere, deletes, agents.clone()));
            planned.ok().and_then(|plan| plan.question())
        };
        assert_eq!(question(pane(Closes::Pane)), None);
        let tab = Closes::Tab { id: 1, panes: vec![4, 5, 8] };
        assert_eq!(question(pane(tab)), Some("Close its tab? 2 agents, 1 busy".to_owned()));
        assert_eq!(
            question(Deletes::Session),
            Some("Delete session luneta? 4 agents, 1 busy, 1 waiting, 1 unknown".to_owned())
        );
        let unknown = Deletes::Pane { pane: 7, closes: Closes::Pane };
        assert_eq!(question(unknown), Some("Close the pane? 1 agent, 1 unknown".to_owned()));
    }
}
