use std::collections::{BTreeMap, BTreeSet};

use luneta_protocol::shell::Doing;

use lunetad::api::projects::Held;
use lunetad::api::{self, remote, terminal, Host};
use lunetad::log;
use lunetad::mux::TerminalKey;
use lunetad::projects::{Arranged, Dir, ProjectId, ProjectName, Seen, Subfolder, Used, ViewId};
use lunetad::projects::{WorktreeId, CLONE_WORKTREES_MAX, VIEWS_MAX, WORKTREES_MAX};
use lunetad::Destination;

use crate::host::NOT_YET;
use crate::machines::{self, Machine};
use crate::sidebar::{Listed, ViewKey};
use crate::viewed::{Tab, WorktreeTabs};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub id: WorktreeId,
    pub name: String,
    pub folder: Dir,
    pub tab: u32,
    pub terminals: Vec<Terminal>,
    pub used: Option<Used>,
    pub seen: Seen,
    pub arranged: Option<Arranged>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terminal {
    pub id: ViewId,
    pub key: Option<TerminalKey>,
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub titled: Option<api::Titled>,
    pub told: Told,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Told {
    Unhooked,
    Unread,
    Doing(Doing),
}

impl Told {
    pub fn of(shell: Option<&api::Shell>) -> Told {
        match shell.map(Doing::of_api) {
            None => Told::Unhooked,
            Some(Err(_)) => Told::Unread,
            Some(Ok(doing)) => Told::Doing(doing),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holds {
    Folder(Worktree),
    Clone { worktrees: Vec<Worktree>, unopened: Unopened },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unopened {
    Listed(Vec<Dir>),
    Unlisted(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
    pub dir: Dir,
    pub holds: Holds,
}

pub fn route(on: &Machine, view: ViewKey) -> Result<TerminalKey, String> {
    match (view, on) {
        (ViewKey::Terminal(_), Machine::Remote(_)) => Err(NOT_YET.to_owned()),
        (ViewKey::Terminal(key), Machine::Local) => Ok(key),
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

impl Terminal {
    fn title(&self) -> Option<String> {
        let topic = self.titled.as_ref().and_then(|titled| titled.topic.clone());
        topic.or_else(|| self.command.clone())
    }
}

impl Worktree {
    fn titles(&self) -> BTreeMap<ViewId, String> {
        let titled = self.terminals.iter();
        titled.filter_map(|it| Some((it.id, it.title()?))).collect()
    }
}

impl Project {
    #[must_use]
    pub fn worktrees(&self) -> &[Worktree] {
        match &self.holds {
            Holds::Folder(own) => std::slice::from_ref(own),
            Holds::Clone { worktrees, .. } => worktrees,
        }
    }

    pub fn uses(&self) -> impl Iterator<Item = (WorktreeId, Option<Used>)> + '_ {
        self.worktrees().iter().map(|it| (it.id, it.used))
    }

    #[must_use]
    pub fn terminal_key(&self, view: ViewId) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        held.find(|it| it.id == view)?.key
    }

    fn first_keyed(&self) -> Option<TerminalKey> {
        let mut held = self.worktrees().iter().flat_map(|it| &it.terminals);
        held.find_map(|it| it.key)
    }

    #[must_use]
    pub fn view_of(&self, terminal: TerminalKey) -> Option<(WorktreeId, ViewId)> {
        self.worktrees().iter().find_map(|worktree| {
            let held = worktree.terminals.iter().find(|it| it.key == Some(terminal))?;
            Some((worktree.id, held.id))
        })
    }
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
        Ok(Projected { here, far: far_projects })
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
    ) -> (Option<WorktreeTabs>, BTreeMap<ViewId, String>) {
        let Some((project, shown)) = self.holding(on, worktree) else {
            return (None, BTreeMap::new());
        };
        let tab = |it: &Worktree| Tab { worktree: it.id, name: it.name.clone() };
        let tabs: Vec<Tab> = project.worktrees().iter().map(tab).collect();
        assert!(tabs.iter().any(|tab| tab.worktree == worktree), "the tabs hold the worktree held");
        (Some(WorktreeTabs { project: project.id, tabs }), shown.titles())
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
    for key in worktrees.iter().flat_map(|it| &it.terminals).filter_map(|it| it.key) {
        if !keyed.insert(key) {
            return Err(format!("two views hold terminal {}", key.id));
        }
    }
    assert!(keyed.len() <= views, "each view holds at most one terminal");
    Ok(projects)
}

fn held(projects: Option<&api::Projects>) -> Result<Holding, String> {
    match projects.and_then(|projects| projects.held.as_ref()) {
        Some(Held::Kept(kept)) => listed(&kept.projects).map(Holding::Read),
        Some(Held::Unkept(_)) | None => Ok(Holding::Unread),
    }
}

fn found_key(terminal: &api::Terminal) -> Result<Option<TerminalKey>, String> {
    match &terminal.found {
        Some(terminal::Found::Key(key)) => TerminalKey::of(key).map(Some),
        Some(terminal::Found::Lost(()) | terminal::Found::Looking(())) | None => Ok(None),
    }
}

fn worktree(worktree: &api::Worktree, tab: u32, of: &str) -> Result<Worktree, String> {
    let named = &worktree.name;
    let id = WorktreeId::of(worktree.id);
    let id = id.ok_or_else(|| format!("worktree {named} of project {of} has id 0"))?;
    let mut terminals = Vec::with_capacity(worktree.terminals.len());
    for terminal in &worktree.terminals {
        let held = ViewId::of(terminal.id);
        let id = held.ok_or_else(|| format!("a terminal of worktree {named} has id 0"))?;
        let key = found_key(terminal).map_err(|why| format!("terminal {id}: {why}"))?;
        terminals.push(Terminal {
            id,
            key,
            cwd: Some(terminal.folder.clone()).filter(|folder| !folder.is_empty()),
            command: terminal.command.clone(),
            titled: terminal.titled.clone(),
            told: Told::of(terminal.shell.as_ref()),
        });
    }
    assert_eq!(terminals.len(), worktree.terminals.len(), "every terminal said is read");
    let folder = Dir::parse(&worktree.folder);
    let folder = folder.map_err(|why| format!("worktree {named}'s folder: {why}"))?;
    let used = worktree.used_at.map(Used::of_api).transpose();
    let used = used.map_err(|why| format!("worktree {named}'s last use: {why}"))?;
    let seen = worktree.seen.and_then(Seen::of_api);
    let seen = seen.ok_or_else(|| format!("worktree {named} says no stage luneta knows"))?;
    let arranged = worktree.arranged.map(Arranged::said).transpose();
    let arranged = arranged.map_err(|why| format!("worktree {named}'s arrangement: {why}"))?;
    let name = named.clone();
    Ok(Worktree { id, name, folder, tab, terminals, used, seen, arranged })
}

fn unopened(
    clone: &api::project::Clone,
    dir: &Dir,
    open: &[Worktree],
) -> Result<Unopened, String> {
    use api::project::clone::Listing;
    let listed = match &clone.listing {
        Some(Listing::Unopened(listed)) => &listed.subfolders,
        Some(Listing::Unlisted(why)) => return Ok(Unopened::Unlisted(why.clone())),
        None => return Ok(Unopened::Unlisted("lunetad says nothing of them".to_owned())),
    };
    let (count, max) = (listed.len(), CLONE_WORKTREES_MAX);
    if count > max {
        return Err(format!("it lists {count} worktrees not open, over the {max} a clone holds"));
    }
    let open: BTreeSet<&Dir> = open.iter().map(|it| &it.folder).collect();
    let mut unopened = Vec::new();
    for said in listed {
        let subfolder = Subfolder::parse(said).map_err(|why| format!("{said}: {why}"))?;
        let folder = dir.at(&subfolder);
        if open.contains(&folder) {
            return Err(format!("{said} is said not open, and a worktree is open there"));
        }
        unopened.push(folder);
    }
    assert_eq!(unopened.len(), count, "each subfolder said is a folder not open");
    Ok(Unopened::Listed(unopened))
}

fn project(project: &api::Project) -> Result<Project, String> {
    let named = &project.name;
    let id = ProjectId::of(project.id).ok_or_else(|| format!("project {named} has id 0"))?;
    let dir = Dir::parse(&project.dir).map_err(|why| format!("project {named}'s folder: {why}"))?;
    let holds = match project.holds.as_ref() {
        Some(api::project::Holds::Folder(own)) => Holds::Folder(worktree(own, 0, named)?),
        Some(api::project::Holds::Clone(clone)) => {
            let rows = clone.worktrees.iter().zip(0..).map(|(it, tab)| worktree(it, tab, named));
            let worktrees = rows.collect::<Result<Vec<_>, String>>()?;
            let unopened = unopened(clone, &dir, &worktrees);
            let unopened = unopened.map_err(|why| format!("project {named}'s clone: {why}"))?;
            Holds::Clone { worktrees, unopened }
        },
        None => return Err(format!("project {named} says not what it holds")),
    };
    let name = ProjectName::parse(&project.name)?;
    Ok(Project { id, name, dir, holds })
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
        let (bar, titles) = projected.tabs(&Machine::Local, worktree(11));
        let bar = bar.unwrap();
        let names: Vec<&str> = bar.tabs.iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(names, ["main", "docs"]);
        assert_eq!(bar.project, seven(), "the tabs are the project's");
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
            let each = |id: usize| row(u64::try_from(id).unwrap(), &format!("t{id}"));
            *rows(kept(&mut host)) = (1..=count).map(each).collect();
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
        assert!(!projected.lacks(&nixos, seven()), "and none it is known to lack");
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
        assert!(!projected.lacks(&nixos, seven()), "and none it is known to lack");
    }

    #[test]
    fn a_machine_lacks_a_project_only_once_its_projects_are_read_and_hold_it_not() {
        let (here, eight) = (Machine::Local, ProjectId::of(8).unwrap());
        let read = Projected::adapted(&host()).unwrap();
        assert!(!read.lacks(&here, seven()), "it holds 7");
        assert!(read.lacks(&here, eight), "and lacks 8");
        let unkept = Held::Unkept("it changed since lunetad read it".to_owned());
        let unkept = Host { projects: Some(api::Projects { held: Some(unkept) }), ..Host::default() };
        let unheld = [Projected::default(), Projected::adapted(&unkept).unwrap()];
        let unsaid = Projected::adapted(&Host::default()).unwrap();
        for unread in unheld.iter().chain([&unsaid]) {
            assert!(unread.projects(&here).is_empty(), "{unread:?}");
            assert!(!unread.lacks(&here, eight), "nothing is lacked where nothing was read: {unread:?}");
        }
        let far = Machine::Remote("nixos".parse().unwrap());
        assert!(!read.lacks(&far, eight), "nor on a machine not reached");
    }

    #[test]
    fn a_project_whose_folder_is_no_absolute_path_is_refused() {
        let mut unrooted = host();
        kept(&mut unrooted).dir = "w".to_owned();
        let refused = Projected::adapted(&unrooted).unwrap_err();
        assert_eq!(refused, "project bitwarden backup's folder: \"w\" is no absolute path");
        let projected = Projected::adapted(&host()).unwrap();
        let dir = &projected.project(&Machine::Local, seven()).unwrap().dir;
        assert_eq!(dir.as_str(), "/w");
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

    #[test]
    fn a_worktree_said_with_no_stage_folder_or_arrangement_luneta_reads_is_refused() {
        type Broke = fn(&mut api::Worktree);
        let unknown = "worktree docs says no stage luneta knows";
        let broken: [(Broke, &str); 4] = [
            (|it| it.seen = None, unknown),
            (|it| it.seen = Some(api::worktree::Seen::Stage(99)), unknown),
            (
                |it| it.folder = "w/docs".to_owned(),
                "worktree docs's folder: \"w/docs\" is no absolute path",
            ),
            (
                |it| it.arranged = Some(99),
                "worktree docs's arrangement: it says neither tabs nor panes",
            ),
        ];
        for (broke, why) in broken {
            let mut host = host();
            broke(&mut rows(kept(&mut host))[1]);
            assert_eq!(Projected::adapted(&host), Err(why.to_owned()));
        }
    }

    #[test]
    fn a_clone_lists_the_worktrees_not_open_under_its_folder_or_is_refused() {
        use api::project::clone::Listing;
        let listing = |subfolders: Vec<String>| {
            let mut host = host();
            let Some(api::project::Holds::Clone(clone)) = &mut kept(&mut host).holds else {
                panic!("the project is a clone");
            };
            clone.listing = Some(Listing::Unopened(api::project::Unopened { subfolders }));
            Projected::adapted(&host).map(|it| it.projects(&Machine::Local)[0].holds.clone())
        };
        let unopened = |holds: Holds| {
            let Holds::Clone { unopened, .. } = holds else {
                panic!("a clone: {holds:?}");
            };
            unopened
        };
        let spike = listing(vec!["spike".to_owned()]).map(unopened);
        assert_eq!(spike, Ok(Unopened::Listed(vec![Dir::parse("/w/spike").unwrap()])));
        let unsaid = Projected::adapted(&host()).unwrap();
        let unsaid = unopened(unsaid.projects(&Machine::Local)[0].holds.clone());
        assert_eq!(unsaid, Unopened::Unlisted("lunetad says nothing of them".to_owned()));
        let clone = "project bitwarden backup's clone";
        let open = format!("{clone}: docs is said not open, and a worktree is open there");
        assert_eq!(listing(vec!["docs".to_owned()]), Err(open));
        let root = format!("{clone}: : a worktree is below its clone's root, and never the root");
        assert_eq!(listing(vec![String::new()]), Err(root));
        let max = CLONE_WORKTREES_MAX;
        let many = |count: usize| (0..count).map(|it| format!("s{it}")).collect();
        assert!(listing(many(max)).is_ok(), "a clone lists {max} not open");
        let over = max + 1;
        let over = format!("it lists {over} worktrees not open, over the {max} a clone holds");
        let over = format!("{clone}: {over}");
        assert_eq!(listing(many(max + 1)), Err(over));
    }
}
