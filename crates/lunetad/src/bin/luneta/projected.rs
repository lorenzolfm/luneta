use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::time::SystemTime;

use lunetad::api::projects::Held;
use lunetad::api::{self, agent, remote, terminal, Host};
use lunetad::mux::own::{OwnId, OwnKey, OwnMade, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{ProjectId, ViewId, WorktreeId};
use lunetad::{Destination, SessionName};

use crate::close::Whose;
use crate::host::NOT_YET;
use crate::machines::{self, Machine};
use crate::sidebar::{Listed, PaneKey};
use crate::viewed::Tab;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub id: WorktreeId,
    pub name: String,
    pub folder: String,
    pub tab: u32,
    pub terminals: Vec<(ViewId, Option<TerminalKey>)>,
    pub titles: BTreeMap<ViewId, String>,
    pub used: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub key: OwnKey,
    pub name: String,
    pub dir: String,
    pub worktrees: Vec<Worktree>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route<P> {
    Zellij { on: Machine, session: SessionName, pane: P },
    Terminal(TerminalKey),
}

impl<P> Route<P> {
    pub fn map<Q>(self, pane: impl FnOnce(P) -> Q) -> Route<Q> {
        match self {
            Route::Zellij { on, session, pane: at } => {
                Route::Zellij { on, session, pane: pane(at) }
            },
            Route::Terminal(key) => Route::Terminal(key),
        }
    }
}

pub fn route(on: &Machine, session: &Listed, pane: PaneKey) -> Result<Route<u32>, String> {
    match (pane, on) {
        (PaneKey::Zellij(pane), on) => {
            let (on, session) = (on.clone(), session.name().clone());
            Ok(Route::Zellij { on, session, pane })
        },
        (PaneKey::Terminal(_), Machine::Remote(_)) => Err(NOT_YET.to_owned()),
        (PaneKey::Terminal(key), Machine::Local) => Ok(Route::Terminal(key)),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projected {
    here: Vec<Project>,
    far: BTreeMap<Destination, Vec<Project>>,
}

#[must_use]
pub fn own_id(project: ProjectId) -> OwnId {
    OwnId::new(project.get()).expect("a project's id is not 0")
}

#[must_use]
pub fn key_of(project: ProjectId) -> OwnKey {
    OwnKey { id: own_id(project), made: OwnMade::from_nanos(0) }
}

impl Project {
    #[must_use]
    pub fn pane_of(&self, terminal: ViewId) -> Option<OwnPane> {
        let mut held = self.worktrees.iter().flat_map(|it| &it.terminals);
        let (_, key) = held.find(|(id, _)| *id == terminal)?;
        Some(OwnPane { session: self.key, id: key.as_ref()?.id })
    }

    fn first_keyed(&self) -> Option<TerminalKey> {
        let mut held = self.worktrees.iter().flat_map(|it| &it.terminals);
        held.find_map(|(_, key)| *key)
    }

    fn holding(&self, pane: NonZeroU32) -> Option<(WorktreeId, ViewId, TerminalKey)> {
        let held = self.worktrees.iter().flat_map(|worktree| {
            let keyed = worktree.terminals.iter().filter_map(|(id, key)| Some((*id, (*key)?)));
            keyed.map(|(id, key)| (worktree.id, id, key))
        });
        let mut on_pane = held.filter(|(_, _, key)| key.id == pane);
        let first = on_pane.next()?;
        on_pane.next().is_none().then_some(first)
    }
}

impl Projected {
    pub fn adapted(host: &mut Host) -> Result<Projected, String> {
        host.sessions.clear();
        let mut far_projects = BTreeMap::new();
        let named = machines::named_once(&host.remotes);
        for (remote, destination) in host.remotes.iter_mut().zip(named) {
            if let Some(remote::Reached::Host(far)) = &mut remote.reached {
                far.sessions.clear();
                let projects = kept(far.projects.as_ref());
                let projects: Vec<Project> =
                    projects.iter().filter_map(|it| project(it).ok()).collect();
                seat(&projects, &mut far.agents, &mut far.unread_files);
                if let Some(destination) = destination {
                    let held = far_projects.insert(destination, projects);
                    assert!(held.is_none(), "named_once names each machine once");
                }
            }
        }
        let here = kept(host.projects.as_ref());
        let here = here.iter().map(project).collect::<Result<Vec<Project>, _>>()?;
        seat(&here, &mut host.agents, &mut host.unread_files);
        Ok(Projected { here, far: far_projects })
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
        match on {
            Machine::Local => &self.here,
            Machine::Remote(destination) => self.far.get(destination).map_or(&[], Vec::as_slice),
        }
    }

    #[must_use]
    pub fn project(&self, on: &Machine, key: OwnKey) -> Option<&Project> {
        self.projects(on).iter().find(|project| project.key == key)
    }

    #[must_use]
    pub fn holding(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> Option<(&Project, &Worktree)> {
        self.projects(on).iter().find_map(|project| {
            let found = project.worktrees.iter().find(|it| it.id == worktree)?;
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
            Some((project, project.worktrees.iter().position(|it| it.id == worktree)?))
        });
        let Some((project, at)) = held else {
            return (None, None);
        };
        let before = at.checked_sub(1).and_then(|it| project.worktrees.get(it));
        let after = project.worktrees.get(at + 1);
        (before.map(|it| it.id), after.map(|it| it.id))
    }

    #[must_use]
    pub fn of_pane(&self, on: &Machine, pane: OwnPane) -> Option<(WorktreeId, ViewId)> {
        let (worktree, terminal, _) = self.project(on, pane.session)?.holding(pane.id)?;
        Some((worktree, terminal))
    }

    pub fn whole(&self, on: &Machine, session: &Listed) -> Result<Route<Option<u32>>, String> {
        let Whose::Own(key) = session.whose() else {
            let (on, session) = (on.clone(), session.name().clone());
            return Ok(Route::Zellij { on, session, pane: None });
        };
        if let Machine::Remote(_) = on {
            return Err(NOT_YET.to_owned());
        }
        let project = self.project(on, key).ok_or("lunetad keeps no such project")?;
        let unshown = || format!("{} has no terminal to show", project.name);
        project.first_keyed().map(Route::Terminal).ok_or_else(unshown)
    }

    #[must_use]
    pub fn tabs(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> (Vec<Tab>, BTreeMap<ViewId, String>) {
        let Some((project, shown)) = self.holding(on, worktree) else {
            return (Vec::new(), BTreeMap::new());
        };
        let tab = |it: &Worktree| Tab { worktree: it.id, name: it.name.clone() };
        (project.worktrees.iter().map(tab).collect(), shown.titles.clone())
    }
}

fn kept(projects: Option<&api::Projects>) -> &[api::Project] {
    match projects.and_then(|projects| projects.held.as_ref()) {
        Some(Held::Kept(kept)) => &kept.projects,
        Some(Held::Unkept(_)) | None => &[],
    }
}

fn seat(projects: &[Project], agents: &mut [api::Agent], unread_files: &mut [api::UnreadFile]) {
    let seat = |key: &api::TerminalKey| {
        let key = TerminalKey::of(key).ok()?;
        projects.iter().find_map(|project| {
            let holds = project.worktrees.iter().flat_map(|it| &it.terminals);
            let held = holds.filter_map(|(_, held)| *held).any(|held| held == key);
            held.then(|| api::Own::from(OwnPane { session: project.key, id: key.id }))
        })
    };
    for agent in agents {
        if let Some(agent::Pane::Terminal(key)) = &agent.pane
            && let Some(own) = seat(key)
        {
            agent.pane = Some(agent::Pane::Own(own));
        }
    }
    for file in unread_files {
        if let Some(key) = &file.terminal
            && let Some(own) = seat(key)
        {
            file.terminal = None;
            file.own = Some(own);
        }
    }
}

fn found_key(terminal: &api::Terminal) -> Result<Option<TerminalKey>, String> {
    match &terminal.found {
        Some(terminal::Found::Key(key)) => TerminalKey::of(key).map(Some),
        Some(terminal::Found::Lost(()) | terminal::Found::Looking(())) | None => Ok(None),
    }
}

fn title_of(terminal: &api::Terminal) -> Option<String> {
    let topic = terminal.titled.as_ref().and_then(|titled| titled.topic.clone());
    topic.or_else(|| terminal.command.clone())
}

pub fn project(project: &api::Project) -> Result<Project, String> {
    let named = &project.name;
    let id = ProjectId::of(project.id).ok_or_else(|| format!("project {named} has id 0"))?;
    let worktree = |(worktree, tab): (&api::Worktree, u32)| {
        let (named, of) = (&worktree.name, &project.name);
        let id = WorktreeId::of(worktree.id);
        let id = id.ok_or_else(|| format!("worktree {named} of project {of} has id 0"))?;
        let mut terminals = Vec::new();
        let mut titles = BTreeMap::new();
        for terminal in &worktree.terminals {
            let held = ViewId::of(terminal.id);
            let held = held.ok_or_else(|| format!("a terminal of worktree {named} has id 0"))?;
            let key = found_key(terminal).map_err(|why| format!("terminal {held}: {why}"))?;
            terminals.push((held, key));
            if let Some(title) = title_of(terminal) {
                titles.insert(held, title);
            }
        }
        let (name, folder) = (worktree.name.clone(), worktree.folder.clone());
        let used = worktree.used_at.map(SystemTime::try_from).transpose();
        let used = used.map_err(|why| format!("worktree {named}'s last use: {why}"))?;
        Ok(Worktree { id, name, folder, tab, terminals, titles, used })
    };
    let worktrees = project.worktrees.iter().zip(0..).map(worktree);
    Ok(Project {
        id,
        key: key_of(id),
        name: project.name.clone(),
        dir: project.dir.clone(),
        worktrees: worktrees.collect::<Result<_, String>>()?,
    })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use lunetad::api::projects::Kept;

    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey { id, made_at: Some(prost_types::Timestamp::default()) }
    }

    fn terminal(id: u64, found: terminal::Found, topic: Option<&str>) -> api::Terminal {
        let titled = topic.map(|topic| api::Titled { topic: Some(topic.to_owned()) });
        api::Terminal {
            id,
            found: Some(found),
            folder: "/w/a".to_owned(),
            command: Some("sleep 600".to_owned()),
            titled,
            ..api::Terminal::default()
        }
    }

    fn host() -> Host {
        let first = api::Worktree {
            id: 11,
            name: "main".to_owned(),
            folder: "/w/a".to_owned(),
            terminals: vec![
                terminal(21, terminal::Found::Key(key(4)), Some("Fixing")),
                terminal(22, terminal::Found::Lost(()), None),
            ],
            ..api::Worktree::default()
        };
        let second = api::Worktree {
            id: 12,
            name: "docs".to_owned(),
            terminals: vec![terminal(23, terminal::Found::Key(key(5)), None)],
            ..api::Worktree::default()
        };
        let project = api::Project {
            id: 7,
            name: "bitwarden backup".to_owned(),
            dir: "/w".to_owned(),
            worktrees: vec![first, second],
            ..api::Project::default()
        };
        let kept = Held::Kept(Kept { projects: vec![project], version: None });
        let projects = Some(api::Projects { held: Some(kept) });
        let own = api::Session { name: "old".to_owned(), ..api::Session::default() };
        let in_terminal = |pid, id| api::Agent {
            pid,
            pane: Some(agent::Pane::Terminal(key(id))),
            ..api::Agent::default()
        };
        let (agent, stray) = (in_terminal(40, 5), in_terminal(41, 9));
        api::Host { projects, sessions: vec![own], agents: vec![agent, stray], ..Host::default() }
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    #[test]
    fn sessions_go_and_an_agent_in_a_terminal_of_a_project_on_any_machine_sits_on_its_pane() {
        let mut host = host();
        let far = api::remote::Host {
            projects: host.projects.clone(),
            sessions: host.sessions.clone(),
            agents: host.agents.clone(),
            ..api::remote::Host::default()
        };
        let reached = Some(remote::Reached::Host(far));
        host.remotes = vec![api::Remote { destination: "nixos".to_owned(), reached }];
        let mut adapted = host.clone();
        let projected = Projected::adapted(&mut adapted).unwrap();
        assert!(adapted.sessions.is_empty(), "{:?}", adapted.sessions);
        let project_key = key_of(ProjectId::of(7).unwrap());
        let own = api::Own::from(OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() });
        assert_eq!(adapted.agents[0].pane, Some(agent::Pane::Own(own)));
        assert_eq!(adapted.agents[1].pane, host.agents[1].pane, "a terminal of no layout stays");
        let Some(remote::Reached::Host(far)) = &adapted.remotes[0].reached else {
            panic!("the remote is reached");
        };
        assert!(far.sessions.is_empty(), "{:?}", far.sessions);
        assert_eq!(far.agents[0].pane, Some(agent::Pane::Own(own)));
        assert_eq!(far.agents[1].pane, host.agents[1].pane);
        let (here, nixos) = (Machine::Local, Machine::Remote("nixos".parse().unwrap()));
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() };
        let held = Some((worktree(12), ViewId::of(23).unwrap()));
        assert_eq!((projected.of_pane(&here, pane), projected.of_pane(&nixos, pane)), (held, held));
        let names = |on| -> Vec<&str> {
            projected.projects(on).iter().map(|it| it.name.as_str()).collect()
        };
        let both = (vec!["bitwarden backup"], vec!["bitwarden backup"]);
        assert_eq!((names(&here), names(&nixos)), both);
        let gone = Machine::Remote("gone".parse().unwrap());
        assert!(projected.projects(&gone).is_empty(), "a machine not reached holds no project");
        assert!(projected.reaches(&here), "this machine is reached");
        assert!(projected.reaches(&nixos), "nixos is reached");
        assert!(!projected.reaches(&gone), "gone is not reached");
    }

    #[test]
    fn a_pane_or_tab_of_a_project_row_leads_back_to_its_worktree_and_terminal() {
        let projected = Projected::adapted(&mut host()).unwrap();
        let project_key = key_of(ProjectId::of(7).unwrap());
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(4).unwrap() };
        let terminal = ViewId::of(21).unwrap();
        assert_eq!(projected.of_pane(&Machine::Local, pane), Some((worktree(11), terminal)));
        let project = &projected.projects(&Machine::Local)[0];
        assert_eq!(project.pane_of(terminal), Some(pane));
        assert_eq!(project.pane_of(ViewId::of(22).unwrap()), None, "a lost one is no pane");
        let (tabs, titles) = projected.tabs(&Machine::Local, worktree(11));
        let names: Vec<&str> = tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(titles.get(&terminal).map(String::as_str), Some("Fixing"));
        let lost = ViewId::of(22).unwrap();
        assert_eq!(titles.get(&lost).map(String::as_str), Some("sleep 600"));
    }

    #[test]
    fn the_tabs_beside_a_worktree_are_the_ones_before_and_after_it_and_none_past_the_ends() {
        let projected = Projected::adapted(&mut host()).unwrap();
        let (main, docs) = (worktree(11), worktree(12));
        let here = Machine::Local;
        assert_eq!(projected.tabs_beside(&here, main), (None, Some(docs)));
        assert_eq!(projected.tabs_beside(&here, docs), (Some(main), None));
        assert_eq!(projected.tabs_beside(&here, worktree(99)), (None, None));
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(projected.tabs_beside(&far, main), (None, None), "far keeps no project");
    }

    fn kept(host: &mut Host) -> &mut api::Project {
        let held = host.projects.as_mut().and_then(|projects| projects.held.as_mut());
        let Some(Held::Kept(kept)) = held else {
            panic!("the host keeps projects");
        };
        &mut kept.projects[0]
    }

    #[test]
    fn a_pane_two_terminals_claim_leads_to_neither() {
        let mut host = host();
        let made_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
        let remade = api::TerminalKey { id: 4, made_at };
        let again = terminal(24, terminal::Found::Key(remade), None);
        kept(&mut host).worktrees[1].terminals.push(again);
        let projected = Projected::adapted(&mut host).unwrap();
        let project_key = key_of(ProjectId::of(7).unwrap());
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(4).unwrap() };
        assert_eq!(projected.of_pane(&Machine::Local, pane), None);
        let five = OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() };
        let held = Some((worktree(12), ViewId::of(23).unwrap()));
        assert_eq!(projected.of_pane(&Machine::Local, five), held);
    }

    #[test]
    fn a_pane_routes_by_its_key_and_a_whole_project_row_to_its_first_keyed_terminal() {
        let projected = Projected::adapted(&mut host()).unwrap();
        let (here, project) = (Machine::Local, Listed::new_project("bitwarden backup", 7));
        let made = OwnMade::read(prost_types::Timestamp::default()).unwrap();
        let keyed = |id| TerminalKey { id: NonZeroU32::new(id).unwrap(), made };
        let unlisted = PaneKey::Terminal(keyed(9));
        assert_eq!(route(&here, &project, unlisted), Ok(Route::Terminal(keyed(9))));
        let first = Ok(Route::Terminal(keyed(4)));
        assert_eq!(projected.whole(&here, &project), first, "the first one keyed");
        let zellij = Listed::new("s", std::time::SystemTime::UNIX_EPOCH);
        let session = zellij.name().clone();
        let by_name = Route::Zellij { on: here.clone(), session: session.clone(), pane: 9 };
        assert_eq!(route(&here, &zellij, PaneKey::Zellij(9)), Ok(by_name));
        let far = Machine::Remote("far".parse().unwrap());
        let whole = Route::Zellij { on: far.clone(), session, pane: None };
        assert_eq!(projected.whole(&far, &zellij), Ok(whole));
        assert_eq!(route(&far, &project, unlisted), Err(NOT_YET.to_owned()));
        assert_eq!(projected.whole(&far, &project), Err(NOT_YET.to_owned()));
        let gone = Listed::new_project("gone", 8);
        let unkept = Err("lunetad keeps no such project".to_owned());
        assert_eq!(projected.whole(&here, &gone), unkept);

        let mut looking = host();
        for worktree in &mut kept(&mut looking).worktrees {
            for terminal in &mut worktree.terminals {
                terminal.found = Some(terminal::Found::Looking(()));
            }
        }
        let looking = Projected::adapted(&mut looking).unwrap();
        let unshown = Err("bitwarden backup has no terminal to show".to_owned());
        assert_eq!(looking.whole(&here, &project), unshown);
    }

    #[test]
    fn a_project_worktree_or_terminal_of_id_0_or_a_key_unread_is_refused() {
        let zeroed: [fn(&mut api::Project); 3] = [
            |project| project.id = 0,
            |project| project.worktrees[1].id = 0,
            |project| project.worktrees[0].terminals[1].id = 0,
        ];
        for zero in zeroed {
            let mut host = host();
            zero(kept(&mut host));
            let refused = Projected::adapted(&mut host).unwrap_err();
            assert!(refused.contains("has id 0"), "{refused}");
        }
        let mut host = host();
        let unmade = api::TerminalKey { id: 4, made_at: None };
        kept(&mut host).worktrees[0].terminals[0].found = Some(terminal::Found::Key(unmade));
        let refused = Projected::adapted(&mut host).unwrap_err();
        assert!(refused.starts_with("terminal "), "{refused}");
    }
}
