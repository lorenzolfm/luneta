use std::collections::BTreeMap;

use lunetad::api::projects::Held;
use lunetad::api::session::Mux;
use lunetad::api::{self, agent, terminal, Host};
use lunetad::mux::own::{OwnId, OwnKey, OwnMade, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{ProjectId, TerminalId, WorkstreamId};

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

fn tab_id(tab: u32) -> u32 {
    tab.checked_add(1).expect("a project holds fewer than 2^32 workstreams")
}

impl Projected {
    #[must_use]
    pub fn of(host: &Host) -> Projected {
        let held = host.projects.as_ref().and_then(|projects| projects.held.as_ref());
        let Some(Held::Kept(kept)) = held else {
            return Projected::default();
        };
        Projected { projects: kept.projects.iter().filter_map(project).collect() }
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
        let project = self.project(pane.session)?;
        project.workstreams.iter().find_map(|workstream| {
            let held = workstream.terminals.iter().find(|(_, key)| {
                key.is_some_and(|key| key.id == pane.id)
            })?;
            Some((workstream.id, held.0))
        })
    }

    #[must_use]
    pub fn key_of(&self, pane: OwnPane) -> Option<TerminalKey> {
        let project = self.project(pane.session)?;
        let held = project.workstreams.iter().flat_map(|it| &it.terminals);
        held.filter_map(|(_, key)| *key).find(|key| key.id == pane.id)
    }

    #[must_use]
    pub fn pane_of(&self, terminal: TerminalId) -> Option<OwnPane> {
        self.projects.iter().find_map(|project| {
            let key = project.workstreams.iter().find_map(|workstream| {
                let held = workstream.terminals.iter().find(|(id, _)| *id == terminal)?;
                held.1
            })?;
            Some(OwnPane { session: project.key, id: key.id })
        })
    }

    #[must_use]
    pub fn tabs(&self, workstream: WorkstreamId) -> (Vec<Tab>, BTreeMap<TerminalId, String>) {
        let Some((project, shown)) = self.holding(workstream) else {
            return (Vec::new(), BTreeMap::new());
        };
        let tab = |it: &Workstream| Tab { workstream: it.id, name: it.name.clone() };
        (project.workstreams.iter().map(tab).collect(), shown.titles.clone())
    }

    pub fn adapt(&self, host: &mut Host) {
        let own = |session: &api::Session| matches!(session.mux, Some(Mux::Own(_)));
        host.sessions.retain(|session| !own(session));
        let held = host.projects.as_ref().and_then(|projects| projects.held.as_ref());
        if let Some(Held::Kept(kept)) = held {
            let sessions: Vec<api::Session> = kept.projects.iter().filter_map(session).collect();
            host.sessions.extend(sessions);
        }
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
}

fn found_key(terminal: &api::Terminal) -> Option<TerminalKey> {
    match &terminal.found {
        Some(terminal::Found::Key(key)) => TerminalKey::of(key).ok(),
        Some(terminal::Found::Lost(()) | terminal::Found::Looking(())) | None => None,
    }
}

fn title_of(terminal: &api::Terminal) -> Option<String> {
    let topic = terminal.titled.as_ref().and_then(|titled| titled.topic.clone());
    topic.or_else(|| terminal.command.clone())
}

fn project(project: &api::Project) -> Option<Project> {
    let id = ProjectId::of(project.id)?;
    let workstreams = project.workstreams.iter().zip(0..).filter_map(|(workstream, tab)| {
        let id = WorkstreamId::of(workstream.id)?;
        let mut terminals = Vec::new();
        let mut titles = BTreeMap::new();
        for terminal in &workstream.terminals {
            let Some(held) = TerminalId::of(terminal.id) else {
                continue;
            };
            terminals.push((held, found_key(terminal)));
            if let Some(title) = title_of(terminal) {
                titles.insert(held, title);
            }
        }
        let (name, folder) = (workstream.name.clone(), workstream.folder.clone());
        Some(Workstream { id, name, folder, tab, terminals, titles })
    });
    Some(Project {
        id,
        key: key_of(id),
        name: project.name.clone(),
        dir: project.dir.clone(),
        workstreams: workstreams.collect(),
    })
}

fn session(project: &api::Project) -> Option<api::Session> {
    let key = key_of(ProjectId::of(project.id)?);
    let mut panes = Vec::new();
    for (workstream, tab) in project.workstreams.iter().zip(0..) {
        for terminal in &workstream.terminals {
            let Some(key) = found_key(terminal) else {
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
            });
        }
    }
    Some(api::Session {
        name: project.name.clone(),
        started_at: None,
        panes: Some(api::Panes { panes }),
        clients: Some(api::Clients { clients: Vec::new() }),
        socket: 0,
        mux: Some(Mux::Own(key.into())),
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
        let projected = Projected::of(&host);
        let mut adapted = host.clone();
        projected.adapt(&mut adapted);
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
        let own = api::Own::from(OwnPane { session: project_key, id: NonZeroU32::new(5).unwrap() });
        assert_eq!(adapted.agents[0].pane, Some(agent::Pane::Own(own)));
        assert_eq!(adapted.agents[1].pane, host.agents[1].pane, "a terminal of no view stays");
    }

    #[test]
    fn a_pane_or_tab_of_a_project_row_leads_back_to_its_workstream_and_terminal() {
        let projected = Projected::of(&host());
        let project_key = key_of(ProjectId::of(7).unwrap());
        let pane = OwnPane { session: project_key, id: NonZeroU32::new(4).unwrap() };
        let terminal = TerminalId::of(21).unwrap();
        assert_eq!(projected.of_pane(pane), Some((workstream(11), terminal)));
        assert_eq!(projected.pane_of(terminal), Some(pane));
        assert_eq!(projected.pane_of(TerminalId::of(22).unwrap()), None, "a lost one is no pane");
        let (tabs, titles) = projected.tabs(workstream(11));
        let names: Vec<&str> = tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(titles.get(&terminal).map(String::as_str), Some("Fixing"));
        let lost = TerminalId::of(22).unwrap();
        assert_eq!(titles.get(&lost).map(String::as_str), Some("sleep 600"));
    }
}
