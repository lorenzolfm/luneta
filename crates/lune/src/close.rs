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
        seen_browsers: Vec<ViewId>,
    },
    Project {
        session: SessionName,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
        seen_browsers: Vec<ViewId>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalAct {
    Terminal(TerminalKey),
    Worktree {
        worktree: WorktreeId,
        views: Vec<TerminalKey>,
        seen: Vec<ViewId>,
        seen_browsers: Vec<ViewId>,
    },
    Project {
        session: SessionName,
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
        seen: Vec<ViewId>,
        seen_browsers: Vec<ViewId>,
    },
}

impl From<TerminalAct> for Act {
    fn from(act: TerminalAct) -> Self {
        match act {
            TerminalAct::Terminal(key) => Act::View(ViewKey::Terminal(key)),
            TerminalAct::Worktree {
                worktree,
                views,
                seen,
                seen_browsers,
            } => Act::Worktree {
                worktree,
                views,
                seen,
                seen_browsers,
            },
            TerminalAct::Project {
                session,
                project,
                worktrees,
                seen,
                seen_browsers,
            } => Act::Project {
                session,
                project,
                worktrees,
                seen,
                seen_browsers,
            },
        }
    }
}

impl TerminalAct {
    fn what(&self) -> String {
        match self {
            TerminalAct::Terminal(_) => "Close the view".to_owned(),
            TerminalAct::Worktree { .. } => "Close the worktree".to_owned(),
            TerminalAct::Project { session, .. } => format!("Close project {session}"),
        }
    }

    fn always_asks(&self) -> bool {
        match self {
            TerminalAct::Terminal(_) | TerminalAct::Worktree { .. } => false,
            TerminalAct::Project { .. } => true,
        }
    }

    pub fn takes(&self, terminal: TerminalKey) -> bool {
        match self {
            TerminalAct::Terminal(key) => terminal == *key,
            TerminalAct::Worktree { views, .. } => views.contains(&terminal),
            TerminalAct::Project { .. } => true,
        }
    }
}

impl Act {
    pub fn noun(&self) -> &'static str {
        match self {
            Act::View(_) => "view",
            Act::Worktree { .. } => "worktree",
            Act::Project { .. } => "project",
        }
    }

    pub fn closes(&self, place: &Place) -> bool {
        match (self, place) {
            (Act::View(view), Place::View(at)) => at.view() == *view,
            (Act::Worktree { worktree, .. }, Place::Worktree(at)) => at.worktree == *worktree,
            (Act::Worktree { worktree, .. }, Place::View(at)) => {
                at.worktree().worktree == *worktree
            }
            (Act::Project { project, .. }, _) => place.listed().project() == *project,
            (Act::View(_), Place::Session(_) | Place::Worktree(_))
            | (Act::Worktree { .. }, Place::Session(_)) => false,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Browser(ViewId),
    Terminals {
        act: TerminalAct,
        ending: Result<Ending, Untold>,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ending {
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
        let terminal_act = match act {
            Act::View(ViewKey::Browser(view)) => return Plan::Browser(view),
            Act::View(ViewKey::Terminal(key)) => TerminalAct::Terminal(key),
            Act::Worktree {
                worktree,
                views,
                seen,
                seen_browsers,
            } => TerminalAct::Worktree {
                worktree,
                views,
                seen,
                seen_browsers,
            },
            Act::Project {
                session,
                project,
                worktrees,
                seen,
                seen_browsers,
            } => TerminalAct::Project {
                session,
                project,
                worktrees,
                seen,
                seen_browsers,
            },
        };
        let ending = agents.map(|agents| {
            let taken = agents
                .iter()
                .filter(|(terminal, _)| terminal_act.takes(*terminal));
            Ending::of(taken.map(|(_, doing)| *doing))
        });
        Plan::Terminals {
            act: terminal_act,
            ending,
        }
    }

    #[must_use]
    pub fn into_act(self) -> Act {
        match self {
            Plan::Browser(view) => Act::View(ViewKey::Browser(view)),
            Plan::Terminals { act, .. } => act.into(),
        }
    }

    pub fn question(&self) -> Option<String> {
        let Plan::Terminals { act, ending } = self else {
            return None;
        };
        let what = act.what();
        match ending {
            Ok(ending) if act.always_asks() => Some(format!("{what}? {}", ending.says())),
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
        ViewKey::terminal(id).terminal_key().unwrap()
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
            seen_browsers: Vec::new(),
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
            seen_browsers: Vec::new(),
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
        let view = TerminalAct::Terminal(at(4));
        let worktree = WorktreeId::of(31).unwrap();
        let views = TerminalAct::Worktree {
            worktree,
            views: vec![at(4)],
            seen: Vec::new(),
            seen_browsers: Vec::new(),
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

    #[test]
    fn a_browser_view_close_never_asks_and_a_worktree_close_covers_its_browser_rows() {
        use crate::sidebar::{ProjectRow, ViewPlace, WorktreePlace};

        let browser = ViewKey::browser(30);
        let act = Act::View(browser);
        assert_eq!(planned(&act, &[(4, Doing::Busy)]).question(), None);
        assert_eq!(Plan::of(act.clone(), Err(Untold::Away)).question(), None);
        assert_eq!(
            Plan::of(act.clone(), Err(Untold::Away)).into_act(),
            act.clone()
        );

        let worktree = WorktreeId::of(31).unwrap();
        let other = WorktreeId::of(32).unwrap();
        let project = ProjectRow::new("lune", 6);
        let place_in = |worktree, view| {
            Place::View(ViewPlace::new(
                WorktreePlace {
                    project: project.clone(),
                    worktree,
                },
                view,
            ))
        };
        assert!(act.closes(&place_in(worktree, browser)));
        assert!(!act.closes(&place_in(worktree, ViewKey::browser(31))));
        assert!(!act.closes(&place_in(worktree, ViewKey::terminal(4))));

        let worktree_act = Act::Worktree {
            worktree,
            views: vec![at(4)],
            seen: vec![ViewId::of(4).unwrap()],
            seen_browsers: vec![ViewId::of(30).unwrap()],
        };
        assert!(worktree_act.closes(&place_in(worktree, browser)));
        assert!(!worktree_act.closes(&place_in(other, browser)));
    }
}
