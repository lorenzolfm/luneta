use std::fmt;

use luneta_protocol::unread::Unplaced;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Status};
use lunetad::mux::OwnKey;
use lunetad::projects::{ProjectId, ViewId, WorktreeId};
use lunetad::seats::Where;
use lunetad::SessionName;

use crate::sidebar::{Listed, PaneKey};

const UNTOLD: &str = "luneta cannot tell which agents it ends";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Pane(PaneKey),
    Worktree(WorktreeId),
    Project,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Deletes {
    Pane(PaneKey),
    Worktree { worktree: WorktreeId, panes: Vec<u32>, seen: Vec<ViewId> },
    Project {
        key: OwnKey,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
    },
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

pub fn unread_in(
    unread_seats: &[(Where, u32)],
    session: Where,
) -> impl Iterator<Item = (u32, Doing)> + '_ {
    let in_session = unread_seats.iter().filter(move |(at, _)| *at == session);
    in_session.map(|(_, pane)| (*pane, Doing::Unknown))
}

#[derive(Debug)]
pub struct Subject<'a> {
    pub session: &'a Listed,
    pub deletes: Deletes,
    pub agents: Result<Vec<(u32, Doing)>, Untold>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Pane { session: SessionName, pane: PaneKey },
    Worktree { session: SessionName, worktree: WorktreeId, panes: Vec<u32>, seen: Vec<ViewId> },
    Project {
        session: SessionName,
        key: OwnKey,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
    },
}

impl Act {
    fn what(&self) -> String {
        match self {
            Act::Pane { .. } => "Close the view".to_owned(),
            Act::Worktree { .. } => "Close the worktree".to_owned(),
            Act::Project { session, .. } => format!("Delete project {session}"),
        }
    }

    pub fn takes(&self, pane: u32) -> bool {
        match self {
            Act::Pane { pane: closed, .. } => pane == closed.id(),
            Act::Worktree { panes, .. } => panes.contains(&pane),
            Act::Project { .. } => true,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub act: Act,
    ending: Result<Ending, Untold>,
}

pub fn plan(subject: &Subject) -> Plan {
    let session = subject.session.name().clone();
    let act = match &subject.deletes {
        Deletes::Pane(pane) => Act::Pane { session, pane: *pane },
        Deletes::Worktree { worktree, panes, seen } => {
            let (worktree, panes, seen) = (*worktree, panes.clone(), seen.clone());
            Act::Worktree { session, worktree, panes, seen }
        },
        Deletes::Project { key, project, worktrees, seen } => {
            let (worktrees, seen) = (worktrees.clone(), seen.clone());
            Act::Project { session, key: *key, project: *project, worktrees, seen }
        },
    };
    Plan::of(act, subject.agents.as_deref().map_err(|untold| *untold))
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
        asked(&self.act.what(), self.ending)
    }

}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ends {
    pub seen: Vec<ViewId>,
    ending: Result<Ending, Untold>,
}

impl Ends {
    pub fn of(seen: &[ViewId], doing: Result<&[Doing], Untold>) -> Ends {
        let mut seen = seen.to_vec();
        seen.sort_unstable();
        Ends { seen, ending: doing.map(|doing| Ending::of(doing.iter().copied())) }
    }

    pub fn question(&self, what: &str) -> Option<String> {
        asked(what, self.ending)
    }
}

fn asked(what: &str, ending: Result<Ending, Untold>) -> Option<String> {
    match ending {
        Ok(ending) if !ending.at_work() => None,
        Ok(ending) => Some(format!("{what}? {}", ending.says())),
        Err(untold) => Some(format!("{what}? {UNTOLD}: {untold}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luneta() -> Listed {
        Listed::new_project("luneta", 3)
    }

    fn subject(session: &Listed, deletes: Deletes, agents: Vec<(u32, Doing)>) -> Subject<'_> {
        Subject { session, deletes, agents: Ok(agents) }
    }

    fn pane() -> Deletes {
        Deletes::Pane(PaneKey::terminal(4))
    }

    #[test]
    fn a_pane_closes_alone() {
        let luneta = luneta();
        let planned = plan(&subject(&luneta, pane(), Vec::new())).act;
        let session = luneta.name().clone();
        assert_eq!(planned, Act::Pane { session, pane: PaneKey::terminal(4) });
    }

    #[test]
    fn a_project_s_delete_kills_it_and_asks_first_by_the_project_s_name() {
        let project = Listed::new_project("luneta", 6);
        let busy = vec![(4, Doing::Busy)];
        let key = project.own_key();
        let worktrees = vec![WorktreeId::of(31).unwrap()];
        let seen = vec![ViewId::of(4).unwrap()];
        let project_id = ProjectId::of(6).unwrap();
        let deletes = Deletes::Project {
            key,
            project: project_id,
            worktrees: worktrees.clone(),
            seen: seen.clone(),
        };
        let planned = plan(&subject(&project, deletes, busy));
        let session = project.name().clone();
        let act = Act::Project { session, key, project: project_id, worktrees, seen };
        assert_eq!(planned.act, act, "an own project is deleted with what was seen of it");
        let asks = Some("Delete project luneta? 1 agent, 1 busy".to_owned());
        assert_eq!(planned.question(), asks);
    }

    #[test]
    fn a_worktree_s_delete_asks_first_only_for_an_agent_at_work_in_its_panes() {
        let luneta = luneta();
        let worktree = WorktreeId::of(31).unwrap();
        let seen = vec![ViewId::of(4).unwrap(), ViewId::of(5).unwrap()];
        let deletes = || Deletes::Worktree { worktree, panes: vec![4, 5], seen: seen.clone() };
        let planned = |agents| plan(&subject(&luneta, deletes(), agents));
        assert_eq!(planned(vec![(6, Doing::Busy), (4, Doing::Idle)]).question(), None);
        let asks = planned(vec![(5, Doing::Waiting)]).question();
        assert_eq!(asks.as_deref(), Some("Close the worktree? 1 agent, 1 waiting"));
        let session = luneta.name().clone();
        let act = Act::Worktree { session, worktree, panes: vec![4, 5], seen: seen.clone() };
        assert_eq!(planned(Vec::new()).act, act);
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
        let question =
            |deletes: Deletes| plan(&subject(&luneta, deletes, agents.clone())).question();
        assert_eq!(question(pane()), None);
        let unknown = Deletes::Pane(PaneKey::terminal(7));
        assert_eq!(question(unknown), Some("Close the view? 1 agent, 1 unknown".to_owned()));
    }
}
