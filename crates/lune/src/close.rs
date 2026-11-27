use std::fmt;

use lune_domain::mux::TerminalKey;
use lune_domain::names::SessionName;
use lune_domain::projects::{ProjectId, ViewId, WorktreeId};
use lune_protocol::agent_status::AgentStatus;
use lune_protocol::unread::Unplaced;
use luned::api::Agent;
use luned::seats::Unread;

use crate::actions::Action;
use crate::sidebar::{Place, ViewKey};

const UNTOLD: &str = "lune cannot tell which agents it ends";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    View(ViewKey),
    Worktree(WorktreeId),
    Project,
}

impl Scope {
    #[must_use]
    pub fn action(self) -> Action {
        match self {
            Scope::View(_) => Action::CloseView,
            Scope::Worktree(_) => Action::CloseWorktree,
            Scope::Project => Action::CloseProject,
        }
    }
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
        match AgentStatus::of_api(agent) {
            AgentStatus::Working => Doing::Busy,
            AgentStatus::Idle
            | AgentStatus::Stopped
            | AgentStatus::Done { .. }
            | AgentStatus::Error { .. } => Doing::Idle,
            AgentStatus::Blocked { .. } => Doing::Waiting,
            AgentStatus::Shell => Doing::Shell,
            AgentStatus::Other(_) | AgentStatus::Unreported => Doing::Unknown,
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
            Untold::Away => f.write_str("luned does not answer"),
            Untold::Unplaced(unplaced) => unplaced.fmt(f),
        }
    }
}

pub fn unread_in<'a>(
    unread: &'a [Unread],
    held: &'a [TerminalKey],
) -> impl Iterator<Item = (TerminalKey, Doing)> + 'a {
    let placed = unread.iter().filter_map(|file| file.terminal);
    let held = placed.filter(move |key| held.contains(key));
    held.map(|key| (key, Doing::Unknown))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    View(ViewKey),
    Worktree {
        worktree: WorktreeId,
        views: Vec<TerminalKey>,
        seen: Vec<ViewId>,
    },
    Project {
        session: SessionName,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
    },
}

impl Act {
    pub fn noun(&self) -> &'static str {
        match self {
            Act::View(_) => "view",
            Act::Worktree { .. } => "worktree",
            Act::Project { .. } => "project",
        }
    }

    fn what(&self) -> String {
        match self {
            Act::View(_) => "Close the view".to_owned(),
            Act::Worktree { .. } => "Close the worktree".to_owned(),
            Act::Project { session, .. } => format!("Close project {session}"),
        }
    }

    fn always_asks(&self) -> bool {
        match self {
            Act::View(_) | Act::Worktree { .. } => false,
            Act::Project { .. } => true,
        }
    }

    pub fn takes(&self, terminal: TerminalKey) -> bool {
        match self {
            Act::View(view) => terminal == view.key(),
            Act::Worktree { views, .. } => views.contains(&terminal),
            Act::Project { .. } => true,
        }
    }

    pub fn closes(&self, place: &Place) -> bool {
        match (self, place) {
            (Act::View(view), Place::View(at)) => at.view() == *view,
            (Act::Worktree { worktree, .. }, Place::Worktree(at)) => at.worktree == *worktree,
            (Act::Worktree { views, .. }, Place::View(at)) => views.contains(&at.view().key()),
            (Act::Project { project, .. }, _) => place.listed().project() == *project,
            (Act::View(_), Place::Session(_) | Place::Worktree(_))
            | (Act::Worktree { .. }, Place::Session(_)) => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    pub act: Act,
    ending: Result<Ending, Untold>,
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
                Doing::Idle => {}
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
    pub fn of(act: Act, agents: Result<&[(TerminalKey, Doing)], Untold>) -> Self {
        let ending = agents.map(|agents| {
            let taken = agents.iter().filter(|(terminal, _)| act.takes(*terminal));
            Ending::of(taken.map(|(_, doing)| *doing))
        });
        Plan { act, ending }
    }

    pub fn question(&self) -> Option<String> {
        let what = self.act.what();
        match self.ending {
            Ok(ending) if self.act.always_asks() => Some(format!("{what}? {}", ending.says())),
            Ok(ending) if !ending.at_work() => None,
            Ok(ending) => Some(format!("{what}? {}", ending.says())),
            Err(untold) => Some(format!("{what}? {UNTOLD}: {untold}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(id: u32) -> TerminalKey {
        ViewKey::terminal(id).key()
    }

    fn planned(act: &Act, agents: &[(u32, Doing)]) -> Plan {
        let agents: Vec<_> = agents.iter().map(|(id, doing)| (at(*id), *doing)).collect();
        Plan::of(act.clone(), Ok(&agents))
    }

    #[test]
    fn a_scope_closes_by_its_own_action() {
        assert_eq!(
            Scope::View(ViewKey::terminal(4)).action(),
            Action::CloseView
        );
        let worktree = WorktreeId::of(31).unwrap();
        assert_eq!(Scope::Worktree(worktree).action(), Action::CloseWorktree);
        assert_eq!(Scope::Project.action(), Action::CloseProject);
    }

    #[test]
    fn a_project_s_close_kills_it_and_always_asks_first_by_the_project_s_name() {
        let session = SessionName::parse("lune").unwrap();
        let worktrees = vec![WorktreeId::of(31).unwrap()];
        let seen = vec![ViewId::of(4).unwrap()];
        let project = ProjectId::of(6).unwrap();
        let act = Act::Project {
            session,
            project,
            worktrees,
            seen,
        };
        let busy = planned(&act, &[(4, Doing::Busy)]).question();
        assert_eq!(busy.as_deref(), Some("Close project lune? 1 agent, 1 busy"));
        let idle = planned(&act, &[(4, Doing::Idle), (5, Doing::Shell)]).question();
        assert_eq!(idle.as_deref(), Some("Close project lune? 1 agent"));
        let unused = planned(&act, &[]).question();
        assert_eq!(unused.as_deref(), Some("Close project lune? no agent"));
    }

    #[test]
    fn a_worktree_s_close_asks_first_only_for_an_agent_at_work_in_its_views() {
        let worktree = WorktreeId::of(31).unwrap();
        let seen = vec![ViewId::of(4).unwrap(), ViewId::of(5).unwrap()];
        let act = Act::Worktree {
            worktree,
            views: vec![at(4), at(5)],
            seen,
        };
        assert_eq!(
            planned(&act, &[(6, Doing::Busy), (4, Doing::Idle)]).question(),
            None
        );
        let asks = planned(&act, &[(5, Doing::Waiting)]).question();
        assert_eq!(
            asks.as_deref(),
            Some("Close the worktree? 1 agent, 1 waiting")
        );
    }

    #[test]
    fn a_close_takes_a_terminal_only_by_its_whole_key() {
        let made = luned::mux::own::OwnMade::from_nanos(1);
        let again = TerminalKey { made, ..at(4) };
        let view = Act::View(ViewKey::terminal(4));
        let worktree = WorktreeId::of(31).unwrap();
        let views = Act::Worktree {
            worktree,
            views: vec![at(4)],
            seen: Vec::new(),
        };
        assert!(view.takes(at(4)) && views.takes(at(4)));
        assert!(
            !view.takes(again) && !views.takes(again),
            "an id made again is another terminal"
        );
    }

    #[test]
    fn a_close_asks_first_only_where_an_agent_at_work_would_end() {
        let agents = [
            (4, Doing::Idle),
            (5, Doing::Busy),
            (6, Doing::Waiting),
            (7, Doing::Unknown),
            (8, Doing::Shell),
        ];
        let question = |view| planned(&Act::View(ViewKey::terminal(view)), &agents).question();
        assert_eq!(question(4), None);
        assert_eq!(
            question(7),
            Some("Close the view? 1 agent, 1 unknown".to_owned())
        );
        let untold = Plan::of(Act::View(ViewKey::terminal(4)), Err(Untold::Away)).question();
        let says = "Close the view? lune cannot tell which agents it ends: luned does not";
        assert!(untold.is_some_and(|asks| asks.starts_with(says)));
    }
}
