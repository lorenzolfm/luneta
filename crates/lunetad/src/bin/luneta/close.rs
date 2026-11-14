use std::fmt;

use luneta_protocol::unread::Unplaced;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Status};
use lunetad::closes::Whose;
use lunetad::mux::TabId;
use lunetad::seats::Where;

use crate::sidebar::{Listed, Place};
use crate::zellij::{Closes, SessionName};

const MOVING: &str = "menu: zellij has yet to say which session it is in; try again in a moment";

const UNTOLD: &str = "luneta cannot tell which agents it ends";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Pane(u32),
    Session,
}

impl Scope {
    pub fn does(self) -> &'static str {
        match self {
            Scope::Pane(_) => "close the pane",
            Scope::Session => "delete the session",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Deletes {
    Pane { pane: u32, closes: Closes },
    Session,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seat {
    In,
    Elsewhere,
    Moving,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Doing {
    Idle,
    Busy,
    Waiting,
    Unknown,
    Shell,
}

impl From<&Agent> for Doing {
    fn from(agent: &Agent) -> Self {
        match &agent.status {
            Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
                Ok(Status::Busy) => Doing::Busy,
                Ok(Status::Idle) => Doing::Idle,
                Ok(Status::Waiting) => Doing::Waiting,
                Ok(Status::Shell) => Doing::Shell,
                Ok(Status::Unspecified) | Err(_) => Doing::Unknown,
            },
            Some(Reported::OtherStatus(_)) | None => Doing::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Untold {
    Away,
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

pub fn unread_in<'a>(
    unread: &'a [(Where<'_>, u32)],
    session: Where<'a>,
) -> impl Iterator<Item = (u32, Doing)> + 'a {
    unread.iter().filter(move |(at, _)| *at == session).map(|(_, pane)| (*pane, Doing::Unknown))
}

#[derive(Debug)]
pub struct Subject<'a> {
    pub session: &'a Listed,
    pub seat: Seat,
    pub deletes: Deletes,
    pub agents: Result<Vec<(u32, Doing)>, Untold>,
    pub refuge: Option<Place>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Pane { session: SessionName, whose: Whose, pane: u32 },
    Tab { session: SessionName, whose: Whose, tab: TabId, panes: Vec<u32> },
    Kill { session: SessionName, whose: Whose },
    Leave { session: SessionName, whose: Whose, to: Place },
}

impl Act {
    fn what(&self) -> String {
        match self {
            Act::Pane { .. } => "Close the pane".to_owned(),
            Act::Tab { .. } => "Close its tab".to_owned(),
            Act::Kill { session, .. } | Act::Leave { session, .. } => {
                format!("Delete session {session}")
            },
        }
    }

    pub fn takes(&self, pane: u32) -> bool {
        match self {
            Act::Pane { pane: closed, .. } => pane == *closed,
            Act::Tab { panes, .. } => panes.contains(&pane),
            Act::Kill { .. } | Act::Leave { .. } => true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub act: Act,
    ending: Result<Ending, Untold>,
}

pub fn plan(subject: &Subject) -> Result<Plan, String> {
    let (session, whose) = (subject.session.name().clone(), subject.session.whose());
    let act = match &subject.deletes {
        Deletes::Pane { pane, closes: Closes::Pane } => {
            Act::Pane { session, whose, pane: *pane }
        },
        Deletes::Pane { closes: Closes::Tab { id, panes }, .. } => {
            Act::Tab { session, whose, tab: *id, panes: panes.clone() }
        },
        Deletes::Pane { closes: Closes::Session, .. } | Deletes::Session => match subject.seat {
            Seat::Elsewhere => Act::Kill { session, whose },
            Seat::In => match subject.refuge.clone() {
                Some(to) => Act::Leave { session, whose, to },
                None => Act::Kill { session, whose },
            },
            Seat::Moving => return Err(MOVING.to_owned()),
        },
    };
    Ok(Plan::of(act, subject.agents.as_deref().map_err(|untold| *untold)))
}

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

    fn at_work(self) -> bool {
        self.busy + self.waiting + self.unknown > 0
    }

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
    pub fn of(act: Act, agents: Result<&[(u32, Doing)], Untold>) -> Self {
        let ending = agents.map(|agents| {
            Ending::of(agents.iter().filter(|(pane, _)| act.takes(*pane)).map(|(_, doing)| *doing))
        });
        Plan { act, ending }
    }

    pub fn question(&self) -> Option<String> {
        match self.ending {
            Ok(ending) if !ending.at_work() => None,
            Ok(_) | Err(_) => Some(format!("{}? {}", self.act.what(), self.ends())),
        }
    }

    pub fn ends(&self) -> String {
        match self.ending {
            Ok(ending) => ending.says(),
            Err(untold) => format!("{UNTOLD}: {untold}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::*;

    fn elsewhere() -> Place {
        Place::Session(Listed::new("elo", SystemTime::UNIX_EPOCH + Duration::from_secs(9)))
    }

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

    fn pane(closes: Closes) -> Deletes {
        Deletes::Pane { pane: 4, closes }
    }

    #[test]
    fn a_pane_closes_alone_its_tab_or_its_session_as_zellij_would_close_them() {
        let tab = Closes::Tab { id: TabId(3), panes: vec![4, 5] };
        let luneta = luneta();
        let planned = |seat: Seat, deletes: Deletes| {
            plan(&subject(&luneta, seat, deletes, Vec::new())).map(|plan| plan.act)
        };
        let (session, whose) = (luneta.name().clone(), luneta.whose());
        assert_eq!(
            planned(Seat::In, pane(Closes::Pane)),
            Ok(Act::Pane { session: session.clone(), whose, pane: 4 })
        );
        assert_eq!(
            planned(Seat::In, pane(tab)),
            Ok(Act::Tab { session: session.clone(), whose, tab: TabId(3), panes: vec![4, 5] })
        );
        let kill = Act::Kill { session: session.clone(), whose };
        assert_eq!(planned(Seat::Elsewhere, pane(Closes::Session)), Ok(kill.clone()));
        let leave = Act::Leave { session, whose, to: elsewhere() };
        assert_eq!(planned(Seat::In, pane(Closes::Session)), Ok(leave.clone()));
        assert_eq!(planned(Seat::In, Deletes::Session), Ok(leave));
        assert_eq!(planned(Seat::Elsewhere, Deletes::Session), Ok(kill));
    }

    #[test]
    fn the_only_session_is_killed_with_luneta_in_it() {
        let luneta = luneta();
        for deletes in [Deletes::Session, pane(Closes::Session)] {
            let agents = vec![(4, Doing::Busy)];
            let alone = Subject { refuge: None, ..subject(&luneta, Seat::In, deletes, agents) };
            let Ok(planned) = plan(&alone) else {
                panic!("the only session is killed");
            };
            let whose = luneta.whose();
            assert_eq!(planned.act, Act::Kill { session: luneta.name().clone(), whose });
            let asks = Some("Delete session luneta? 1 agent, 1 busy".to_owned());
            assert_eq!(planned.question(), asks);
        }
    }

    #[test]
    fn no_session_goes_while_luneta_is_on_its_way_to_one() {
        let luneta = luneta();
        for deletes in [Deletes::Session, pane(Closes::Session)] {
            let planned = plan(&subject(&luneta, Seat::Moving, deletes, Vec::new()));
            assert_eq!(planned.map(|plan| plan.act), Err(MOVING.to_owned()));
        }
        let planned = plan(&subject(&luneta, Seat::Moving, pane(Closes::Pane), Vec::new()));
        let (session, whose) = (luneta.name().clone(), luneta.whose());
        assert_eq!(planned.map(|plan| plan.act), Ok(Act::Pane { session, whose, pane: 4 }));
    }

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
        let tab = Closes::Tab { id: TabId(1), panes: vec![4, 5, 8] };
        assert_eq!(question(pane(tab)), Some("Close its tab? 2 agents, 1 busy".to_owned()));
        assert_eq!(
            question(Deletes::Session),
            Some("Delete session luneta? 4 agents, 1 busy, 1 waiting, 1 unknown".to_owned())
        );
        let unknown = Deletes::Pane { pane: 7, closes: Closes::Pane };
        assert_eq!(question(unknown), Some("Close the pane? 1 agent, 1 unknown".to_owned()));
    }
}
