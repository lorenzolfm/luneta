use std::collections::BTreeMap;
use std::num::NonZeroU32;

use lunetad::api::projects::Held;
use lunetad::api::session::Mux;
use lunetad::api::{self, agent, terminal, Host};
use lunetad::mux::own::{OwnId, OwnKey, OwnMade, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{ProjectId, TerminalId, WorkstreamId};
use lunetad::SessionName;

use crate::close::Whose;
use crate::host::NOT_YET;
use crate::machines::Machine;
use crate::sidebar::{Listed, PaneKey};
use crate::viewed::Tab;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workstream {
    pub id: WorkstreamId,
    pub name: String,
    pub folder: String,
    pub tab: u32,
    pub terminals: Vec<(TerminalId, Option<TerminalKey>)>,
    pub titles: BTreeMap<TerminalId, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub key: OwnKey,
    pub name: String,
    pub dir: String,
    pub workstreams: Vec<Workstream>,
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
    projects: Vec<Project>,
}

#[must_use]
pub fn own_id(project: ProjectId) -> OwnId {
    OwnId::new(project.get()).expect("a project's id is not 0")
}

#[must_use]
pub fn key_of(project: ProjectId) -> OwnKey {
    OwnKey { id: own_id(project), made: OwnMade::from_nanos(0) }
}

#[must_use]
pub fn project_of(key: OwnKey) -> Option<ProjectId> {
    let project = ProjectId::of(key.id.get())?;
    (key_of(project) == key).then_some(project)
}

fn tab_id(tab: u32) -> u32 {
    tab.checked_add(1).expect("a project holds fewer than 2^32 workstreams")
}

impl Project {
    #[must_use]
    pub fn pane_of(&self, terminal: TerminalId) -> Option<OwnPane> {
        let mut held = self.workstreams.iter().flat_map(|it| &it.terminals);
        let (_, key) = held.find(|(id, _)| *id == terminal)?;
        Some(OwnPane { session: self.key, id: key.as_ref()?.id })
    }

    fn first_keyed(&self) -> Option<TerminalKey> {
        let mut held = self.workstreams.iter().flat_map(|it| &it.terminals);
        held.find_map(|(_, key)| *key)
    }

    fn holding(&self, pane: NonZeroU32) -> Option<(WorkstreamId, TerminalId, TerminalKey)> {
        let held = self.workstreams.iter().flat_map(|workstream| {
            let keyed = workstream.terminals.iter().filter_map(|(id, key)| Some((*id, (*key)?)));
            keyed.map(|(id, key)| (workstream.id, id, key))
        });
        let mut on_pane = held.filter(|(_, _, key)| key.id == pane);
        let first = on_pane.next()?;
        on_pane.next().is_none().then_some(first)
    }
}

impl Projected {
    pub fn adapted(host: &mut Host) -> Result<Projected, String> {
        let own = |session: &api::Session| matches!(session.mux, Some(Mux::Own(_)));
        host.sessions.retain(|session| !own(session));
        let held = host.projects.as_ref().and_then(|projects| projects.held.as_ref());
        let Some(Held::Kept(kept)) = held else {
            return Ok(Projected::default());
        };
        let projects = kept.projects.iter().map(project).collect::<Result<Vec<Project>, _>>()?;
        let said = kept.projects.iter().zip(&projects);
        let sessions: Vec<api::Session> = said.map(|(said, project)| session(said, project)).collect();
        host.sessions.extend(sessions);
        let projected = Projected { projects };
        projected.seat(host);
        Ok(projected)
    }

    fn seat(&self, host: &mut Host) {
        let seat = |key: &api::TerminalKey| {
            let key = TerminalKey::of(key).ok()?;
            self.projects.iter().find_map(|project| {
                let holds = project.workstreams.iter().flat_map(|it| &it.terminals);
                let held = holds.filter_map(|(_, held)| *held).any(|held| held == key);
                held.then(|| api::Own::from(OwnPane { session: project.key, id: key.id }))
            })
        };
        for agent in &mut host.agents {
            if let Some(agent::Pane::Terminal(key)) = &agent.pane
                && let Some(own) = seat(key)
            {
                agent.pane = Some(agent::Pane::Own(own));
            }
        }
        for file in &mut host.unread_files {
            if let Some(key) = &file.terminal
                && let Some(own) = seat(key)
            {
                file.terminal = None;
                file.own = Some(own);
            }
        }
    }

    #[must_use]
    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    #[must_use]
    pub fn project(&self, key: OwnKey) -> Option<&Project> {
        self.projects.iter().find(|project| project.key == key)
    }

    #[must_use]
    pub fn holding(&self, workstream: WorkstreamId) -> Option<(&Project, &Workstream)> {
        self.projects.iter().find_map(|project| {
            let found = project.workstreams.iter().find(|it| it.id == workstream)?;
            Some((project, found))
        })
    }

    #[must_use]
    pub fn of_pane(&self, pane: OwnPane) -> Option<(WorkstreamId, TerminalId)> {
        let (workstream, terminal, _) = self.project(pane.session)?.holding(pane.id)?;
        Some((workstream, terminal))
    }

    pub fn whole(&self, on: &Machine, session: &Listed) -> Result<Route<Option<u32>>, String> {
        let Whose::Own(key) = session.whose() else {
            let (on, session) = (on.clone(), session.name().clone());
            return Ok(Route::Zellij { on, session, pane: None });
        };
        if let Machine::Remote(_) = on {
            return Err(NOT_YET.to_owned());
        }
        let project = self.project(key).ok_or("lunetad keeps no such project")?;
        let unshown = || format!("{} has no terminal to show", project.name);
        project.first_keyed().map(Route::Terminal).ok_or_else(unshown)
    }

    #[must_use]
    pub fn tabs(&self, workstream: WorkstreamId) -> (Vec<Tab>, BTreeMap<TerminalId, String>) {
        let Some((project, shown)) = self.holding(workstream) else {
            return (Vec::new(), BTreeMap::new());
        };
        let tab = |it: &Workstream| Tab { workstream: it.id, name: it.name.clone() };
        (project.workstreams.iter().map(tab).collect(), shown.titles.clone())
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
    let workstream = |(workstream, tab): (&api::Workstream, u32)| {
        let (named, of) = (&workstream.name, &project.name);
        let id = WorkstreamId::of(workstream.id);
        let id = id.ok_or_else(|| format!("workstream {named} of project {of} has id 0"))?;
        let mut terminals = Vec::new();
        let mut titles = BTreeMap::new();
        for terminal in &workstream.terminals {
            let held = TerminalId::of(terminal.id);
            let held = held.ok_or_else(|| format!("a terminal of workstream {named} has id 0"))?;
            let key = found_key(terminal).map_err(|why| format!("terminal {held}: {why}"))?;
            terminals.push((held, key));
            if let Some(title) = title_of(terminal) {
                titles.insert(held, title);
            }
        }
        let (name, folder) = (workstream.name.clone(), workstream.folder.clone());
        Ok(Workstream { id, name, folder, tab, terminals, titles })
    };
    let workstreams = project.workstreams.iter().zip(0..).map(workstream);
    Ok(Project {
        id,
        key: key_of(id),
        name: project.name.clone(),
        dir: project.dir.clone(),
        workstreams: workstreams.collect::<Result<_, String>>()?,
    })
}

fn session(said: &api::Project, project: &Project) -> api::Session {
    let mut panes = Vec::new();
    for (workstream, parsed) in said.workstreams.iter().zip(&project.workstreams) {
        let tab = parsed.tab;
        for (terminal, (_, key)) in workstream.terminals.iter().zip(&parsed.terminals) {
            let Some(key) = key else {
                continue;
            };
            panes.push(api::Pane {
                id: key.id.get(),
                tab,
                tab_name: workstream.name.clone(),
                cwd: Some(terminal.folder.clone()),
                command: terminal.command.clone(),
                titled: terminal.titled.clone(),
                shell: terminal.shell.clone(),
                tab_id: Some(tab_id(tab)),
                held: terminal.held,
                key: Some((*key).into()),
            });
        }
    }
    api::Session {
        name: said.name.clone(),
        started_at: None,
        panes: Some(api::Panes { panes }),
        clients: Some(api::Clients { clients: Vec::new() }),
        socket: 0,
        mux: Some(Mux::Own(project.key.into())),
    }
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
        let first = api::Workstream {
            id: 11,
            name: "main".to_owned(),
            folder: "/w/a".to_owned(),
            terminals: vec![
                terminal(21, terminal::Found::Key(key(4)), Some("Fixing")),
                terminal(22, terminal::Found::Lost(()), None),
            ],
            ..api::Workstream::default()
        };
        let second = api::Workstream {
            id: 12,
            name: "docs".to_owned(),
            terminals: vec![terminal(23, terminal::Found::Key(key(5)), None)],
            ..api::Workstream::default()
        };
        let project = api::Project {
            id: 7,
            name: "bitwarden backup".to_owned(),
            dir: "/w".to_owned(),
            workstreams: vec![first, second],
            ..api::Project::default()
        };
        let kept = Held::Kept(Kept { projects: vec![project] });
        let projects = Some(api::Projects { held: Some(kept) });
        let own = api::Session {
            name: "old".to_owned(),
            mux: Some(Mux::Own(api::OwnSession { id: 3, made_at: None })),
            ..api::Session::default()
        };
        let in_terminal = |pid, id| api::Agent {
            pid,
            pane: Some(agent::Pane::Terminal(key(id))),
            ..api::Agent::default()
        };
        let (agent, stray) = (in_terminal(40, 5), in_terminal(41, 9));
        api::Host { projects, sessions: vec![own], agents: vec![agent, stray], ..Host::default() }
    }

    fn workstream(id: u64) -> WorkstreamId {
        WorkstreamId::of(id).unwrap()
    }

    #[test]
    fn a_project_is_a_session_whose_tabs_are_its_workstreams_and_panes_its_terminals() {
        let host = host();
        let mut adapted = host.clone();
        Projected::adapted(&mut adapted).unwrap();
        assert_eq!(adapted.sessions.len(), 1, "own sessions give way to projects");
        let session = &adapted.sessions[0];
        assert_eq!(session.name, "bitwarden backup");
        let project_key = key_of(ProjectId::of(7).unwrap());
        assert_eq!(session.mux, Some(Mux::Own(project_key.into())));
        let panes = &session.panes.as_ref().unwrap().panes;
        let shown: Vec<(u32, u32, &str, Option<u32>)> = panes
            .iter()
            .map(|pane| (pane.id, pane.tab, pane.tab_name.as_str(), pane.tab_id))
            .collect();
        assert_eq!(shown, [(4, 0, "main", Some(1)), (5, 1, "docs", Some(2))]);
        let keys: Vec<Option<api::TerminalKey>> = panes.iter().map(|pane| pane.key).collect();
        assert_eq!(keys, [Some(key(4)), Some(key(5))], "each pane says its terminal's whole key");
        let own = api::Own::from(OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() });
        assert_eq!(adapted.agents[0].pane, Some(agent::Pane::Own(own)));
        assert_eq!(adapted.agents[1].pane, host.agents[1].pane, "a terminal of no view stays");
    }

    #[test]
    fn a_pane_or_tab_of_a_project_row_leads_back_to_its_workstream_and_terminal() {
        let projected = Projected::adapted(&mut host()).unwrap();
        let project_key = key_of(ProjectId::of(7).unwrap());
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(4).unwrap() };
        let terminal = TerminalId::of(21).unwrap();
        assert_eq!(projected.of_pane(pane), Some((workstream(11), terminal)));
        let project = &projected.projects()[0];
        assert_eq!(project.pane_of(terminal), Some(pane));
        assert_eq!(project.pane_of(TerminalId::of(22).unwrap()), None, "a lost one is no pane");
        let (tabs, titles) = projected.tabs(workstream(11));
        let names: Vec<&str> = tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(titles.get(&terminal).map(String::as_str), Some("Fixing"));
        let lost = TerminalId::of(22).unwrap();
        assert_eq!(titles.get(&lost).map(String::as_str), Some("sleep 600"));
    }

    fn kept(host: &mut Host) -> &mut api::Project {
        let held = host.projects.as_mut().and_then(|projects| projects.held.as_mut());
        let Some(Held::Kept(kept)) = held else {
            panic!("the host keeps projects");
        };
        &mut kept.projects[0]
    }

    #[test]
    fn a_project_row_s_key_leads_back_to_its_project_and_an_own_session_s_to_none() {
        let project = ProjectId::of(7).unwrap();
        assert_eq!(project_of(key_of(project)), Some(project));
        let own = OwnKey { id: own_id(project), made: OwnMade::from_nanos(1) };
        assert_eq!(project_of(own), None);
    }

    #[test]
    fn a_pane_two_terminals_claim_leads_to_neither() {
        let mut host = host();
        let made_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
        let remade = api::TerminalKey { id: 4, made_at };
        let again = terminal(24, terminal::Found::Key(remade), None);
        kept(&mut host).workstreams[1].terminals.push(again);
        let projected = Projected::adapted(&mut host).unwrap();
        let project_key = key_of(ProjectId::of(7).unwrap());
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(4).unwrap() };
        assert_eq!(projected.of_pane(pane), None);
        let five = OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() };
        assert_eq!(projected.of_pane(five), Some((workstream(12), TerminalId::of(23).unwrap())));
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
        for workstream in &mut kept(&mut looking).workstreams {
            for terminal in &mut workstream.terminals {
                terminal.found = Some(terminal::Found::Looking(()));
            }
        }
        let looking = Projected::adapted(&mut looking).unwrap();
        let unshown = Err("bitwarden backup has no terminal to show".to_owned());
        assert_eq!(looking.whole(&here, &project), unshown);
    }

    #[test]
    fn a_project_workstream_or_terminal_of_id_0_or_a_key_unread_is_refused_and_own_sessions_go() {
        let zeroed: [fn(&mut api::Project); 3] = [
            |project| project.id = 0,
            |project| project.workstreams[1].id = 0,
            |project| project.workstreams[0].terminals[1].id = 0,
        ];
        for zero in zeroed {
            let mut host = host();
            zero(kept(&mut host));
            let refused = Projected::adapted(&mut host).unwrap_err();
            assert!(refused.contains("has id 0"), "{refused}");
            assert!(host.sessions.is_empty(), "{:?}", host.sessions);
        }
        let mut host = host();
        let unmade = api::TerminalKey { id: 4, made_at: None };
        kept(&mut host).workstreams[0].terminals[0].found = Some(terminal::Found::Key(unmade));
        let refused = Projected::adapted(&mut host).unwrap_err();
        assert!(refused.starts_with("terminal "), "{refused}");
    }
}
