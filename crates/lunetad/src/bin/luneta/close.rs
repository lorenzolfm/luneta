use std::fmt;

use luneta_protocol::unread::Unplaced;
use lunetad::api::agent::Status as Reported;
use lunetad::api::{Agent, Status};
use lunetad::mux::TerminalKey;
use lunetad::projects::{ProjectId, ViewId, WorktreeId};
use lunetad::seats::Unread;
use lunetad::SessionName;

use crate::sidebar::{Listed, Place, ViewKey};

const UNTOLD: &str = "luneta cannot tell which agents it ends";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    View(ViewKey),
    Worktree(WorktreeId),
    Project,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Deletes {
    View(ViewKey),
    Worktree { worktree: WorktreeId, views: Vec<u32>, seen: Vec<ViewId> },
    Project { project: ProjectId, worktrees: Vec<WorktreeId>, seen: Vec<ViewId> },
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
    unread: &'a [Unread],
    held: &'a [TerminalKey],
) -> impl Iterator<Item = (u32, Doing)> + 'a {
    let placed = unread.iter().filter_map(|file| file.terminal);
    let held = placed.filter(move |key| held.contains(key));
    held.map(|key| (key.id.get(), Doing::Unknown))
}

#[derive(Debug)]
pub struct Subject<'a> {
    pub session: &'a Listed,
    pub deletes: Deletes,
    pub agents: Result<Vec<(u32, Doing)>, Untold>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    View { session: SessionName, view: ViewKey },
    Worktree { session: SessionName, worktree: WorktreeId, views: Vec<u32>, seen: Vec<ViewId> },
    Project {
        session: SessionName,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
    },
}

impl Act {
    fn what(&self) -> String {
        match self {
            Act::View { .. } => "Close the view".to_owned(),
            Act::Worktree { .. } => "Close the worktree".to_owned(),
            Act::Project { session, .. } => format!("Delete project {session}"),
        }
    }

    fn always_asks(&self) -> bool {
        match self {
            Act::View { .. } | Act::Worktree { .. } => false,
            Act::Project { .. } => true,
        }
    }

    pub fn takes(&self, view: u32) -> bool {
        match self {
            Act::View { view: closed, .. } => view == closed.terminal_id(),
            Act::Worktree { views, .. } => views.contains(&view),
            Act::Project { .. } => true,
        }
    }

    pub fn closes(&self, place: &Place) -> bool {
        match (self, place) {
            (Act::View { view, .. }, Place::View(at)) => at.view == *view,
            (Act::Worktree { worktree, .. }, Place::Worktree(at)) => at.worktree == *worktree,
            (Act::Worktree { session, views, .. }, Place::View(at)) => {
                place.session() == session.as_str() && views.contains(&at.view.terminal_id())
            },
            (Act::Project { project, .. }, _) => place.listed().project() == *project,
            (Act::View { .. }, Place::Session(_) | Place::Worktree(_))
            | (Act::Worktree { .. }, Place::Session(_)) => false,
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
        Deletes::View(view) => Act::View { session, view: *view },
        Deletes::Worktree { worktree, views, seen } => {
            let (worktree, views, seen) = (*worktree, views.clone(), seen.clone());
            Act::Worktree { session, worktree, views, seen }
        },
        Deletes::Project { project, worktrees, seen } => {
            let (worktrees, seen) = (worktrees.clone(), seen.clone());
            Act::Project { session, project: *project, worktrees, seen }
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
            Ending::of(agents.iter().filter(|(view, _)| act.takes(*view)).map(|(_, doing)| *doing))
        });
        Plan { act, ending }
    }

    pub fn question(&self) -> Option<String> {
        let what = self.act.what();
        match self.ending {
            Ok(ending) if self.act.always_asks() => Some(format!("{what}? {}", ending.says())),
            ending => asked(&what, ending),
        }
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

    fn view() -> Deletes {
        Deletes::View(ViewKey::terminal(4))
    }

    #[test]
    fn a_view_closes_alone() {
        let luneta = luneta();
        let planned = plan(&subject(&luneta, view(), Vec::new())).act;
        let session = luneta.name().clone();
        assert_eq!(planned, Act::View { session, view: ViewKey::terminal(4) });
    }

    #[test]
    fn a_project_s_delete_kills_it_and_always_asks_first_by_the_project_s_name() {
        let project = Listed::new_project("luneta", 6);
        let worktrees = vec![WorktreeId::of(31).unwrap()];
        let seen = vec![ViewId::of(4).unwrap()];
        let project_id = ProjectId::of(6).unwrap();
        let deletes = || Deletes::Project {
            project: project_id,
            worktrees: worktrees.clone(),
            seen: seen.clone(),
        };
        let planned = |agents| plan(&subject(&project, deletes(), agents));
        let session = project.name().clone();
        let (worktrees, seen) = (worktrees.clone(), seen.clone());
        let act = Act::Project { session, project: project_id, worktrees, seen };
        let busy = planned(vec![(4, Doing::Busy)]);
        assert_eq!(busy.act, act, "an own project is deleted with what was seen of it");
        assert_eq!(busy.question().as_deref(), Some("Delete project luneta? 1 agent, 1 busy"));
        let idle = planned(vec![(4, Doing::Idle), (5, Doing::Shell)]).question();
        assert_eq!(idle.as_deref(), Some("Delete project luneta? 1 agent"));
        let unused = planned(Vec::new()).question();
        assert_eq!(unused.as_deref(), Some("Delete project luneta? no agent"));
    }

    #[test]
    fn a_worktree_s_delete_asks_first_only_for_an_agent_at_work_in_its_views() {
        let luneta = luneta();
        let worktree = WorktreeId::of(31).unwrap();
        let seen = vec![ViewId::of(4).unwrap(), ViewId::of(5).unwrap()];
        let deletes = || Deletes::Worktree { worktree, views: vec![4, 5], seen: seen.clone() };
        let planned = |agents| plan(&subject(&luneta, deletes(), agents));
        assert_eq!(planned(vec![(6, Doing::Busy), (4, Doing::Idle)]).question(), None);
        let asks = planned(vec![(5, Doing::Waiting)]).question();
        assert_eq!(asks.as_deref(), Some("Close the worktree? 1 agent, 1 waiting"));
        let session = luneta.name().clone();
        let act = Act::Worktree { session, worktree, views: vec![4, 5], seen: seen.clone() };
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
        assert_eq!(question(view()), None);
        let unknown = Deletes::View(ViewKey::terminal(7));
        assert_eq!(question(unknown), Some("Close the view? 1 agent, 1 unknown".to_owned()));
    }
}
