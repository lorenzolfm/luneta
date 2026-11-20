use std::collections::{BTreeMap, BTreeSet};

use lunetad::api::projects::Held;
use lunetad::api::{self, remote, terminal, Host};
use lunetad::log;
use lunetad::mux::TerminalKey;
use lunetad::projects::{ProjectId, Used, ViewId, WorktreeId, VIEWS_MAX, WORKTREES_MAX};
use lunetad::Destination;

use crate::host::NOT_YET;
use crate::machines::{self, Machine};
use crate::sidebar::{Listed, ViewKey};
use crate::viewed::Tab;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub id: WorktreeId,
    pub name: String,
    pub folder: String,
    pub tab: u32,
    pub terminals: Vec<(ViewId, Option<TerminalKey>)>,
    pub titles: BTreeMap<ViewId, String>,
    pub used: Option<Used>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holds {
    Folder(Worktree),
    Clone(Vec<Worktree>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: String,
    pub dir: String,
    pub holds: Holds,
}

pub fn route(on: &Machine, view: ViewKey) -> Result<TerminalKey, String> {
    match (view, on) {
        (ViewKey::Terminal(_), Machine::Remote(_)) => Err(NOT_YET.to_owned()),
        (ViewKey::Terminal(key), Machine::Local) => Ok(key),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projected {
    here: Vec<Project>,
    far: BTreeMap<Destination, Vec<Project>>,
    read: bool,
}

impl Project {
    #[must_use]
    pub fn worktrees(&self) -> &[Worktree] {
        match &self.holds {
            Holds::Folder(own) => std::slice::from_ref(own),
            Holds::Clone(rows) => rows,
        }
    }

    pub fn uses(&self) -> impl Iterator<Item = (WorktreeId, Option<Used>)> + '_ {
        self.worktrees().iter().map(|it| (it.id, it.used))
    }

    #[must_use]
    pub fn terminal_key(&self, view: ViewId) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        let (_, key) = held.find(|(id, _)| *id == view)?;
        *key
    }

    fn first_keyed(&self) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        held.find_map(|(_, key)| *key)
    }

    #[must_use]
    pub fn view_of(&self, terminal: TerminalKey) -> Option<(WorktreeId, ViewId)> {
        self.worktrees().iter().find_map(|worktree| {
            let (view, _) = worktree.terminals.iter().find(|(_, key)| *key == Some(terminal))?;
            Some((worktree.id, *view))
        })
    }
}

impl Projected {
    pub fn adapted(host: &Host) -> Result<Projected, String> {
        let mut far_projects = BTreeMap::new();
        let named = machines::named_once(&host.remotes);
        for (remote, destination) in host.remotes.iter().zip(named) {
            if let Some(remote::Reached::Host(far)) = &remote.reached {
                let projects = listed(kept(far.projects.as_ref())).unwrap_or_else(|why| {
                    log::say(&format!("{}'s projects: {why}", remote.destination));
                    Vec::new()
                });
                if let Some(destination) = destination {
                    let held = far_projects.insert(destination, projects);
                    assert!(held.is_none(), "named_once names each machine once");
                }
            }
        }
        let here = listed(kept(host.projects.as_ref()))?;
        Ok(Projected { here, far: far_projects, read: true })
    }

    #[must_use]
    pub fn read(&self) -> bool {
        self.read
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
    pub fn project(&self, on: &Machine, id: ProjectId) -> Option<&Project> {
        self.projects(on).iter().find(|project| project.id == id)
    }

    #[must_use]
    pub fn holding(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> Option<(&Project, &Worktree)> {
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
            Some((project, project.worktrees().iter().position(|it| it.id == worktree)?))
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
        let project = self.project(on, session.project()).ok_or("lunetad keeps no such project")?;
        let unshown = || format!("{} has no terminal to show", project.name);
        project.first_keyed().ok_or_else(unshown)
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
        (project.worktrees().iter().map(tab).collect(), shown.titles.clone())
    }
}

pub fn listed(kept: &[api::Project]) -> Result<Vec<Project>, String> {
    let projects = kept.iter().map(project).collect::<Result<Vec<Project>, String>>()?;
    let worktrees: Vec<&Worktree> = projects.iter().flat_map(Project::worktrees).collect();
    let count = worktrees.len();
    if count > WORKTREES_MAX {
        return Err(format!("they hold {count} worktrees, over the {WORKTREES_MAX} kept"));
    }
    let views: usize = worktrees.iter().map(|it| it.terminals.len()).sum();
    if views > VIEWS_MAX {
        return Err(format!("they hold {views} views, over the {VIEWS_MAX} kept"));
    }
    let mut keyed = BTreeSet::new();
    for key in worktrees.iter().flat_map(|it| &it.terminals).filter_map(|(_, key)| *key) {
        if !keyed.insert(key) {
            return Err(format!("two views hold terminal {}", key.id));
        }
    }
    assert!(keyed.len() <= views, "each view holds at most one terminal");
    Ok(projects)
}

fn kept(projects: Option<&api::Projects>) -> &[api::Project] {
    match projects.and_then(|projects| projects.held.as_ref()) {
        Some(Held::Kept(kept)) => &kept.projects,
        Some(Held::Unkept(_)) | None => &[],
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

fn project(project: &api::Project) -> Result<Project, String> {
    let named = &project.name;
    let id = ProjectId::of(project.id).ok_or_else(|| format!("project {named} has id 0"))?;
    let worktree = |(worktree, tab): (&api::Worktree, u32)| -> Result<Worktree, String> {
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
        let used = worktree.used_at.map(Used::of_api).transpose();
        let used = used.map_err(|why| format!("worktree {named}'s last use: {why}"))?;
        Ok(Worktree { id, name, folder, tab, terminals, titles, used })
    };
    let holds = match project.holds.as_ref() {
        Some(api::project::Holds::Folder(own)) => Holds::Folder(worktree((own, 0))?),
        Some(api::project::Holds::Clone(clone)) => {
            let rows = clone.worktrees.iter().zip(0..).map(worktree);
            Holds::Clone(rows.collect::<Result<_, String>>()?)
        },
        None => return Err(format!("project {named} says not what it holds")),
    };
    Ok(Project { id, name: project.name.clone(), dir: project.dir.clone(), holds })
}

#[cfg(test)]
mod tests {
    use lunetad::api::projects::Kept;

    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey { id, made_at: Some(prost_types::Timestamp::default()) }
    }

    fn keyed(id: u32) -> TerminalKey {
        TerminalKey::of(&key(id)).unwrap()
    }

    fn seven() -> ProjectId {
        ProjectId::of(7).unwrap()
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
        let kept = Held::Kept(Kept { projects: vec![project], version: None });
        let projects = Some(api::Projects { held: Some(kept) });
        api::Host { projects, ..Host::default() }
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    #[test]
    fn a_view_of_a_project_on_any_machine_leads_back_to_its_worktree() {
        let mut host = host();
        let far =
            api::remote::Host { projects: host.projects.clone(), ..api::remote::Host::default() };
        let reached = Some(remote::Reached::Host(far));
        host.remotes = vec![api::Remote { destination: "nixos".to_owned(), reached }];
        let projected = Projected::adapted(&host).unwrap();
        let (here, nixos) = (Machine::Local, Machine::Remote("nixos".parse().unwrap()));
        let five = |on| projected.project(on, seven())?.view_of(keyed(5));
        let held = Some((worktree(12), ViewId::of(23).unwrap()));
        assert_eq!((five(&here), five(&nixos)), (held, held));
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
    fn a_view_or_tab_of_a_project_row_leads_back_to_its_worktree_and_terminal() {
        let projected = Projected::adapted(&host()).unwrap();
        let terminal = ViewId::of(21).unwrap();
        let project = projected.project(&Machine::Local, seven()).unwrap();
        assert_eq!(project.view_of(keyed(4)), Some((worktree(11), terminal)));
        assert_eq!(project.terminal_key(terminal), Some(keyed(4)));
        assert_eq!(project.terminal_key(ViewId::of(22).unwrap()), None, "a lost one is no view");
        assert_eq!(projected.project(&Machine::Local, ProjectId::of(8).unwrap()), None);
        let (tabs, titles) = projected.tabs(&Machine::Local, worktree(11));
        let names: Vec<&str> = tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(titles.get(&terminal).map(String::as_str), Some("Fixing"));
        let lost = ViewId::of(22).unwrap();
        assert_eq!(titles.get(&lost).map(String::as_str), Some("sleep 600"));
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
        assert_eq!(projected.tabs_beside(&far, main), (None, None), "far keeps no project");
    }

    fn kept(host: &mut Host) -> &mut api::Project {
        let held = host.projects.as_mut().and_then(|projects| projects.held.as_mut());
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
    fn a_terminal_made_again_under_an_id_in_use_leads_to_its_own_view() {
        let mut host = host();
        let made_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
        let remade = api::TerminalKey { id: 5, made_at };
        rows(kept(&mut host))[1].terminals.push(terminal(25, terminal::Found::Key(remade), None));
        let projected = Projected::adapted(&host).unwrap();
        let project = projected.project(&Machine::Local, seven()).unwrap();
        let view = |id| Some((worktree(12), ViewId::of(id).unwrap()));
        assert_eq!(project.view_of(keyed(5)), view(23));
        assert_eq!(project.view_of(TerminalKey::of(&remade).unwrap()), view(25));
    }

    #[test]
    fn a_machine_that_lists_one_terminal_in_two_views_is_refused_its_projects() {
        let mut host = host();
        rows(kept(&mut host))[1].terminals.push(terminal(24, terminal::Found::Key(key(4)), None));
        assert_eq!(Projected::adapted(&host), Err("two views hold terminal 4".to_owned()));
    }

    #[test]
    fn a_view_routes_by_its_key_and_a_whole_project_row_to_its_first_keyed_terminal() {
        let projected = Projected::adapted(&host()).unwrap();
        let (here, project) = (Machine::Local, Listed::new_project("bitwarden backup", 7));
        let unlisted = ViewKey::Terminal(keyed(9));
        assert_eq!(route(&here, unlisted), Ok(keyed(9)));
        assert_eq!(projected.whole(&here, &project), Ok(keyed(4)), "the first one keyed");
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(route(&far, unlisted), Err(NOT_YET.to_owned()));
        assert_eq!(projected.whole(&far, &project), Err(NOT_YET.to_owned()));
        let gone = Listed::new_project("gone", 8);
        let unkept = Err("lunetad keeps no such project".to_owned());
        assert_eq!(projected.whole(&here, &gone), unkept);

        let mut looking = host();
        for worktree in rows(kept(&mut looking)) {
            for terminal in &mut worktree.terminals {
                terminal.found = Some(terminal::Found::Looking(()));
            }
        }
        let looking = Projected::adapted(&looking).unwrap();
        let unshown = Err("bitwarden backup has no terminal to show".to_owned());
        assert_eq!(looking.whole(&here, &project), unshown);
    }

    #[test]
    fn a_project_worktree_or_terminal_of_id_0_or_a_key_unread_is_refused() {
        let zeroed: [fn(&mut api::Project); 3] = [
            |project| project.id = 0,
            |project| rows(project)[1].id = 0,
            |project| rows(project)[0].terminals[1].id = 0,
        ];
        for zero in zeroed {
            let mut host = host();
            zero(kept(&mut host));
            let refused = Projected::adapted(&host).unwrap_err();
            assert!(refused.contains("has id 0"), "{refused}");
        }
        let mut host = host();
        let unmade = api::TerminalKey { id: 4, made_at: None };
        rows(kept(&mut host))[0].terminals[0].found = Some(terminal::Found::Key(unmade));
        let refused = Projected::adapted(&host).unwrap_err();
        assert!(refused.starts_with("terminal "), "{refused}");
    }

    #[test]
    fn a_machine_that_lists_more_worktrees_than_lunetad_keeps_is_refused_its_projects() {
        let listing = |count: usize| {
            let mut host = host();
            let row = |id| api::Worktree { id: u64::try_from(id).unwrap(), ..Default::default() };
            *rows(kept(&mut host)) = (1..=count).map(row).collect();
            host.projects
        };
        let most = Host { projects: listing(WORKTREES_MAX), ..Host::default() };
        let most = Projected::adapted(&most).unwrap();
        assert_eq!(most.projects(&Machine::Local)[0].worktrees().len(), WORKTREES_MAX);
        let over = Host { projects: listing(WORKTREES_MAX + 1), ..Host::default() };
        let refused = Projected::adapted(&over).unwrap_err();
        assert!(refused.contains(&format!("over the {WORKTREES_MAX} kept")), "{refused}");
        let far = api::remote::Host { projects: listing(WORKTREES_MAX + 1), ..Default::default() };
        let reached = Some(remote::Reached::Host(far));
        let remotes = vec![api::Remote { destination: "nixos".to_owned(), reached }];
        let projected = Projected::adapted(&Host { remotes, ..Host::default() }).unwrap();
        let nixos = Machine::Remote("nixos".parse().unwrap());
        assert!(projected.reaches(&nixos), "a machine that lists too many is reached still");
        assert!(projected.projects(&nixos).is_empty(), "with no project to go to");
    }

    #[test]
    fn a_machine_that_lists_more_views_than_lunetad_keeps_is_refused_its_projects() {
        let listing = |count: u64| {
            let mut host = host();
            let lost = |id| terminal(id, terminal::Found::Lost(()), None);
            rows(kept(&mut host))[0].terminals = (1..=count).map(lost).collect();
            Projected::adapted(&host)
        };
        let most = u64::try_from(VIEWS_MAX).unwrap() - 1;
        assert!(listing(most).is_ok(), "{VIEWS_MAX} views, with the second worktree's one");
        let over = format!("they hold {} views, over the {VIEWS_MAX} kept", VIEWS_MAX + 1);
        assert_eq!(listing(most + 1), Err(over));
    }

    #[test]
    fn a_machine_that_lists_a_project_luneta_cannot_read_is_reached_with_none_of_them() {
        let mut far = host();
        let readable = kept(&mut far).clone();
        let unread = api::Project { id: 0, ..readable.clone() };
        let held = Held::Kept(Kept { projects: vec![readable, unread], version: None });
        let projects = Some(api::Projects { held: Some(held) });
        let far = api::remote::Host { projects, ..Default::default() };
        let reached = Some(remote::Reached::Host(far));
        let remotes = vec![api::Remote { destination: "nixos".to_owned(), reached }];
        let projected = Projected::adapted(&Host { remotes, ..Host::default() }).unwrap();
        let nixos = Machine::Remote("nixos".parse().unwrap());
        assert!(projected.reaches(&nixos), "a machine luneta cannot read is reached still");
        assert!(projected.projects(&nixos).is_empty(), "with none of its projects, not some");
    }

    #[test]
    fn a_project_holds_its_folder_or_its_clone_s_worktrees_and_says_which() {
        let mut folder = host();
        let own = rows(kept(&mut folder)).remove(0);
        kept(&mut folder).holds = Some(api::project::Holds::Folder(own));
        let projected = Projected::adapted(&folder).unwrap();
        let project = &projected.projects(&Machine::Local)[0];
        let Holds::Folder(own) = &project.holds else {
            panic!("a folder holds its own worktree: {:?}", project.holds);
        };
        assert_eq!((own.id, project.worktrees().len()), (worktree(11), 1));
        let clone = Projected::adapted(&host()).unwrap();
        let ids: Vec<WorktreeId> =
            clone.projects(&Machine::Local)[0].worktrees().iter().map(|it| it.id).collect();
        assert_eq!(ids, [worktree(11), worktree(12)]);
        kept(&mut folder).holds = None;
        let refused = Projected::adapted(&folder).unwrap_err();
        assert_eq!(refused, "project bitwarden backup says not what it holds");
    }
}
