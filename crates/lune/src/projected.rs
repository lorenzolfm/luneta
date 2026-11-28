use std::collections::BTreeMap;

use lune_domain::mux::TerminalKey;
use lune_domain::projects::{ProjectId, ViewId, WorktreeId};
use lune_domain::rows::{self, Project, Worktree};

use luned::Destination;
use luned::api::projects::Held;
use luned::api::{self, Host, remote};
use luned::log;

use crate::host::NOT_YET;
use crate::machines::{self, Machine};
use crate::sidebar::Listed;
use crate::viewed::{Tab, WorktreeTabs};

pub fn route_terminal(on: &Machine, key: TerminalKey) -> Result<TerminalKey, String> {
    match on {
        Machine::Remote(_) => Err(NOT_YET.to_owned()),
        Machine::Local => Ok(key),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Holding {
    #[default]
    Unread,
    Read(Vec<Project>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projected {
    here: Holding,
    far: BTreeMap<Destination, Holding>,
}

impl Projected {
    pub fn adapted(host: &Host) -> Result<Projected, String> {
        let mut far_projects = BTreeMap::new();
        let named = machines::named_once(&host.remotes);
        for (remote, destination) in host.remotes.iter().zip(named) {
            if let Some(remote::Reached::Host(far)) = &remote.reached {
                let holding = held(far.projects.as_ref()).unwrap_or_else(|why| {
                    log::say(&format!("{}'s projects: {why}", remote.destination));
                    Holding::Unread
                });
                if let Some(destination) = destination {
                    let held = far_projects.insert(destination, holding);
                    assert!(held.is_none(), "named_once names each machine once");
                }
            }
        }
        let here = held(host.projects.as_ref())?;
        Ok(Projected {
            here,
            far: far_projects,
        })
    }

    fn held_on(&self, on: &Machine) -> Option<&Holding> {
        match on {
            Machine::Local => Some(&self.here),
            Machine::Remote(destination) => self.far.get(destination),
        }
    }

    #[must_use]
    pub fn lacks(&self, on: &Machine, id: ProjectId) -> bool {
        let holding = self.held_on(on);
        matches!(holding, Some(Holding::Read(projects)) if projects.iter().all(|it| it.id != id))
    }

    #[must_use]
    pub fn reaches(&self, on: &Machine) -> bool {
        match on {
            Machine::Local => true,
            Machine::Remote(destination) => self.far.contains_key(destination),
        }
    }

    #[must_use]
    pub fn projects(&self, on: &Machine) -> &[Project] {
        match self.held_on(on) {
            Some(Holding::Read(projects)) => projects,
            Some(Holding::Unread) | None => &[],
        }
    }

    #[must_use]
    pub fn project(&self, on: &Machine, id: ProjectId) -> Option<&Project> {
        self.projects(on).iter().find(|project| project.id == id)
    }

    #[must_use]
    pub fn holding(&self, on: &Machine, worktree: WorktreeId) -> Option<(&Project, &Worktree)> {
        self.projects(on).iter().find_map(|project| {
            let found = project.worktrees().iter().find(|it| it.id == worktree)?;
            Some((project, found))
        })
    }

    #[must_use]
    pub fn tabs_beside(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> (Option<WorktreeId>, Option<WorktreeId>) {
        let held = self.projects(on).iter().find_map(|project| {
            Some((
                project,
                project
                    .worktrees()
                    .iter()
                    .position(|it| it.id == worktree)?,
            ))
        });
        let Some((project, at)) = held else {
            return (None, None);
        };
        let worktrees = project.worktrees();
        let before = at.checked_sub(1).and_then(|it| worktrees.get(it));
        let after = worktrees.get(at + 1);
        (before.map(|it| it.id), after.map(|it| it.id))
    }

    pub fn whole(&self, on: &Machine, session: &Listed) -> Result<TerminalKey, String> {
        if let Machine::Remote(_) = on {
            return Err(NOT_YET.to_owned());
        }
        let project = self
            .project(on, session.project())
            .ok_or("luned keeps no such project")?;
        let unshown = || format!("{} has no terminal to show", project.name);
        project.first_keyed().ok_or_else(unshown)
    }

    #[must_use]
    pub fn tabs(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> (Option<WorktreeTabs>, BTreeMap<ViewId, String>) {
        let Some((project, shown)) = self.holding(on, worktree) else {
            return (None, BTreeMap::new());
        };
        let tab = |it: &Worktree| Tab {
            worktree: it.id,
            name: it.name.clone(),
        };
        let tabs: Vec<Tab> = project.worktrees().iter().map(tab).collect();
        assert!(
            tabs.iter().any(|tab| tab.worktree == worktree),
            "the tabs hold the worktree held"
        );
        (
            Some(WorktreeTabs {
                project: project.id,
                tabs,
            }),
            shown.titles(),
        )
    }
}

fn held(projects: Option<&api::Projects>) -> Result<Holding, String> {
    match projects.and_then(|projects| projects.held.as_ref()) {
        Some(Held::Kept(kept)) => rows::listed(&kept.projects).map(Holding::Read),
        Some(Held::Unkept(_)) | None => Ok(Holding::Unread),
    }
}

#[cfg(test)]
mod tests {
    use luned::api::projects::Kept;
    use luned::api::terminal;

    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey {
            id,
            made_at: Some(prost_types::Timestamp::default()),
        }
    }

    fn keyed(id: u32) -> TerminalKey {
        TerminalKey::of(&key(id)).unwrap()
    }

    fn seven() -> ProjectId {
        ProjectId::of(7).unwrap()
    }

    fn terminal(id: u64, found: terminal::Found, topic: Option<&str>) -> api::Terminal {
        let keyed = matches!(found, terminal::Found::Key(_));
        let titled = topic.map(|topic| api::Titled {
            topic: Some(topic.to_owned()),
        });
        api::Terminal {
            id,
            found: Some(found),
            folder: "/w/a".to_owned(),
            command: keyed.then(|| "sleep 600".to_owned()),
            titled,
            ..api::Terminal::default()
        }
    }

    fn row(id: u64, name: &str) -> api::Worktree {
        api::Worktree {
            id,
            name: name.to_owned(),
            folder: format!("/w/{name}"),
            seen: Some(api::worktree::Seen::Unread(())),
            ..api::Worktree::default()
        }
    }

    fn host() -> Host {
        let first = api::Worktree {
            terminals: vec![
                terminal(21, terminal::Found::Key(key(4)), Some("Fixing")),
                terminal(22, terminal::Found::Lost(()), None),
            ],
            ..row(11, "main")
        };
        let second = api::Worktree {
            terminals: vec![terminal(23, terminal::Found::Key(key(5)), None)],
            ..row(12, "docs")
        };
        let worktrees = vec![first, second];
        let project = api::Project {
            id: 7,
            name: "bitwarden backup".to_owned(),
            dir: "/w".to_owned(),
            holds: Some(api::project::Holds::Clone(api::project::Clone {
                worktrees,
                listing: None,
            })),
            ..api::Project::default()
        };
        let kept = Held::Kept(Kept {
            projects: vec![project],
            version: None,
        });
        let projects = Some(api::Projects { held: Some(kept) });
        api::Host {
            projects,
            ..Host::default()
        }
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    #[test]
    fn a_view_of_a_project_on_any_machine_leads_back_to_its_worktree() {
        let mut host = host();
        let far = api::remote::Host {
            projects: host.projects.clone(),
            ..api::remote::Host::default()
        };
        let reached = Some(remote::Reached::Host(far));
        host.remotes = vec![api::Remote {
            destination: "nixos".to_owned(),
            reached,
        }];
        let projected = Projected::adapted(&host).unwrap();
        let (here, nixos) = (Machine::Local, Machine::Remote("nixos".parse().unwrap()));
        let five = |on| projected.project(on, seven())?.view_of(keyed(5));
        let held = Some((worktree(12), ViewId::of(23).unwrap()));
        assert_eq!((five(&here), five(&nixos)), (held, held));
        let names = |on| -> Vec<&str> {
            projected
                .projects(on)
                .iter()
                .map(|it| it.name.as_str())
                .collect()
        };
        let both = (vec!["bitwarden backup"], vec!["bitwarden backup"]);
        assert_eq!((names(&here), names(&nixos)), both);
        let gone = Machine::Remote("gone".parse().unwrap());
        assert!(
            projected.projects(&gone).is_empty(),
            "a machine not reached holds no project"
        );
        assert!(projected.reaches(&here), "this machine is reached");
        assert!(projected.reaches(&nixos), "nixos is reached");
        assert!(!projected.reaches(&gone), "gone is not reached");
    }

    #[test]
    fn a_project_row_s_tabs_are_its_worktrees_titled_by_their_terminals() {
        let projected = Projected::adapted(&host()).unwrap();
        let terminal = ViewId::of(21).unwrap();
        assert!(projected.project(&Machine::Local, seven()).is_some());
        assert_eq!(
            projected.project(&Machine::Local, ProjectId::of(8).unwrap()),
            None
        );
        let (bar, titles) = projected.tabs(&Machine::Local, worktree(11));
        let bar = bar.unwrap();
        let names: Vec<&str> = bar.tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(bar.project, seven(), "the tabs are the project's");
        assert_eq!(titles.get(&terminal).map(String::as_str), Some("Fixing"));
        let lost = ViewId::of(22).unwrap();
        assert_eq!(titles.get(&lost), None);
    }

    #[test]
    fn the_tabs_beside_a_worktree_are_the_ones_before_and_after_it_and_none_past_the_ends() {
        let projected = Projected::adapted(&host()).unwrap();
        let (main, docs) = (worktree(11), worktree(12));
        let here = Machine::Local;
        assert_eq!(projected.tabs_beside(&here, main), (None, Some(docs)));
        assert_eq!(projected.tabs_beside(&here, docs), (Some(main), None));
        assert_eq!(projected.tabs_beside(&here, worktree(99)), (None, None));
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(
            projected.tabs_beside(&far, main),
            (None, None),
            "far keeps no project"
        );
    }

    fn kept(host: &mut Host) -> &mut api::Project {
        let held = host
            .projects
            .as_mut()
            .and_then(|projects| projects.held.as_mut());
        let Some(Held::Kept(kept)) = held else {
            panic!("the host keeps projects");
        };
        &mut kept.projects[0]
    }

    fn rows(project: &mut api::Project) -> &mut Vec<api::Worktree> {
        let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
            panic!("the project is a clone");
        };
        &mut clone.worktrees
    }

    #[test]
    fn a_view_routes_by_its_key_and_a_whole_project_row_to_its_first_keyed_terminal() {
        let projected = Projected::adapted(&host()).unwrap();
        let (here, project) = (Machine::Local, Listed::new_project("bitwarden backup", 7));
        assert_eq!(route_terminal(&here, keyed(9)), Ok(keyed(9)));
        assert_eq!(
            projected.whole(&here, &project),
            Ok(keyed(4)),
            "the first one keyed"
        );
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(route_terminal(&far, keyed(9)), Err(NOT_YET.to_owned()));
        assert_eq!(projected.whole(&far, &project), Err(NOT_YET.to_owned()));
        let gone = Listed::new_project("gone", 8);
        let unkept = Err("luned keeps no such project".to_owned());
        assert_eq!(projected.whole(&here, &gone), unkept);

        let mut looking = host();
        for worktree in rows(kept(&mut looking)) {
            for terminal in &mut worktree.terminals {
                terminal.found = Some(terminal::Found::Looking(()));
                terminal.command = None;
                terminal.titled = None;
                terminal.shell = None;
                terminal.held = false;
                terminal.state = None;
            }
        }
        let looking = Projected::adapted(&looking).unwrap();
        let unshown = Err("bitwarden backup has no terminal to show".to_owned());
        assert_eq!(looking.whole(&here, &project), unshown);
    }

    #[test]
    fn a_machine_that_lists_a_project_lune_cannot_read_is_reached_with_none_of_them() {
        let mut far = host();
        let readable = kept(&mut far).clone();
        let unread = api::Project {
            id: 0,
            ..readable.clone()
        };
        let held = Held::Kept(Kept {
            projects: vec![readable, unread],
            version: None,
        });
        let projects = Some(api::Projects { held: Some(held) });
        let far = api::remote::Host {
            projects,
            ..Default::default()
        };
        let reached = Some(remote::Reached::Host(far));
        let remotes = vec![api::Remote {
            destination: "nixos".to_owned(),
            reached,
        }];
        let projected = Projected::adapted(&Host {
            remotes,
            ..Host::default()
        })
        .unwrap();
        let nixos = Machine::Remote("nixos".parse().unwrap());
        assert!(
            projected.reaches(&nixos),
            "a machine lune cannot read is reached still"
        );
        assert!(
            projected.projects(&nixos).is_empty(),
            "with none of its projects, not some"
        );
        assert!(
            !projected.lacks(&nixos, seven()),
            "and none it is known to lack"
        );
    }

    #[test]
    fn a_machine_lacks_a_project_only_once_its_projects_are_read_and_hold_it_not() {
        let (here, eight) = (Machine::Local, ProjectId::of(8).unwrap());
        let read = Projected::adapted(&host()).unwrap();
        assert!(!read.lacks(&here, seven()), "it holds 7");
        assert!(read.lacks(&here, eight), "and lacks 8");
        let unkept = Held::Unkept("it changed since luned read it".to_owned());
        let unkept = Host {
            projects: Some(api::Projects { held: Some(unkept) }),
            ..Host::default()
        };
        let unheld = [Projected::default(), Projected::adapted(&unkept).unwrap()];
        let unsaid = Projected::adapted(&Host::default()).unwrap();
        for unread in unheld.iter().chain([&unsaid]) {
            assert!(unread.projects(&here).is_empty(), "{unread:?}");
            assert!(
                !unread.lacks(&here, eight),
                "nothing is lacked where nothing was read: {unread:?}"
            );
        }
        let far = Machine::Remote("nixos".parse().unwrap());
        assert!(!read.lacks(&far, eight), "nor on a machine not reached");
    }
}
