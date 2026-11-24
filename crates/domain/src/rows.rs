use std::collections::{BTreeMap, BTreeSet};

use lune_protocol::shell::Doing;
use lune_protocol::v1::{self as api, terminal};

use crate::mux::TerminalKey;
use crate::projects::{Arranged, Dir, ProjectId, ProjectName, Seen, Subfolder, Used, ViewId};
use crate::projects::{WorktreeId, CLONE_WORKTREES_MAX, VIEWS_MAX, WORKTREES_MAX};

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
    pub cwd: Dir,
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

impl Terminal {
    fn title(&self) -> Option<String> {
        let topic = self.titled.as_ref().and_then(|titled| titled.topic.clone());
        topic.or_else(|| self.command.clone())
    }
}

impl Worktree {
    #[must_use]
    pub fn titles(&self) -> BTreeMap<ViewId, String> {
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

    #[must_use]
    pub fn first_keyed(&self) -> Option<TerminalKey> {
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
        let cwd = Dir::parse(&terminal.folder);
        let cwd = cwd.map_err(|why| format!("terminal {id}'s folder: {why}"))?;
        terminals.push(Terminal {
            id,
            key,
            cwd,
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
    let seen = seen.ok_or_else(|| format!("worktree {named} says no stage lune knows"))?;
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
        None => return Ok(Unopened::Unlisted("luned says nothing of them".to_owned())),
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
    use super::*;

    fn key(id: u32) -> api::TerminalKey {
        api::TerminalKey { id, made_at: Some(prost_types::Timestamp::default()) }
    }

    fn keyed(id: u32) -> TerminalKey {
        TerminalKey::of(&key(id)).unwrap()
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

    fn kept() -> api::Project {
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
        api::Project {
            id: 7,
            name: "bitwarden backup".to_owned(),
            dir: "/w".to_owned(),
            holds: Some(api::project::Holds::Clone(api::project::Clone {
                worktrees: vec![first, second],
                listing: None,
            })),
            ..api::Project::default()
        }
    }

    fn read(kept: &api::Project) -> Result<Project, String> {
        let [project] = listed(std::slice::from_ref(kept))?.try_into().unwrap();
        Ok(project)
    }

    fn rows(project: &mut api::Project) -> &mut Vec<api::Worktree> {
        let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
            panic!("the project is a clone");
        };
        &mut clone.worktrees
    }

    fn worktree(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    #[test]
    fn a_view_of_a_project_row_leads_back_to_its_worktree_and_terminal() {
        let project = read(&kept()).unwrap();
        let terminal = ViewId::of(21).unwrap();
        assert_eq!(project.view_of(keyed(4)), Some((worktree(11), terminal)));
        assert_eq!(project.terminal_key(terminal), Some(keyed(4)));
        assert_eq!(project.terminal_key(ViewId::of(22).unwrap()), None, "a lost one is no view");
        assert_eq!(project.first_keyed(), Some(keyed(4)));
    }

    #[test]
    fn a_terminal_made_again_under_an_id_in_use_leads_to_its_own_view() {
        let mut kept = kept();
        let made_at = Some(prost_types::Timestamp { seconds: 1, nanos: 0 });
        let remade = api::TerminalKey { id: 5, made_at };
        rows(&mut kept)[1].terminals.push(terminal(25, terminal::Found::Key(remade), None));
        let project = read(&kept).unwrap();
        let view = |id| Some((worktree(12), ViewId::of(id).unwrap()));
        assert_eq!(project.view_of(keyed(5)), view(23));
        assert_eq!(project.view_of(TerminalKey::of(&remade).unwrap()), view(25));
    }

    #[test]
    fn projects_that_list_one_terminal_in_two_views_are_refused() {
        let mut kept = kept();
        rows(&mut kept)[1].terminals.push(terminal(24, terminal::Found::Key(key(4)), None));
        assert_eq!(read(&kept), Err("two views hold terminal 4".to_owned()));
    }

    #[test]
    fn a_project_worktree_or_terminal_of_id_0_or_a_key_unread_is_refused() {
        let zeroed: [fn(&mut api::Project); 3] = [
            |project| project.id = 0,
            |project| rows(project)[1].id = 0,
            |project| rows(project)[0].terminals[1].id = 0,
        ];
        for zero in zeroed {
            let mut kept = kept();
            zero(&mut kept);
            let refused = read(&kept).unwrap_err();
            assert!(refused.contains("has id 0"), "{refused}");
        }
        let mut kept = kept();
        let unmade = api::TerminalKey { id: 4, made_at: None };
        rows(&mut kept)[0].terminals[0].found = Some(terminal::Found::Key(unmade));
        let refused = read(&kept).unwrap_err();
        assert!(refused.starts_with("terminal "), "{refused}");
    }

    #[test]
    fn projects_that_list_more_worktrees_than_luned_keeps_are_refused() {
        let listing = |count: usize| {
            let mut kept = kept();
            let each = |id: usize| row(u64::try_from(id).unwrap(), &format!("t{id}"));
            *rows(&mut kept) = (1..=count).map(each).collect();
            read(&kept)
        };
        assert_eq!(listing(WORKTREES_MAX).unwrap().worktrees().len(), WORKTREES_MAX);
        let refused = listing(WORKTREES_MAX + 1).unwrap_err();
        assert!(refused.contains(&format!("over the {WORKTREES_MAX} kept")), "{refused}");
    }

    #[test]
    fn projects_that_list_more_views_than_luned_keeps_are_refused() {
        let listing = |count: u64| {
            let mut kept = kept();
            let lost = |id| terminal(id, terminal::Found::Lost(()), None);
            rows(&mut kept)[0].terminals = (1..=count).map(lost).collect();
            read(&kept)
        };
        let most = u64::try_from(VIEWS_MAX).unwrap() - 1;
        assert!(listing(most).is_ok(), "{VIEWS_MAX} views, with the second worktree's one");
        let over = format!("they hold {} views, over the {VIEWS_MAX} kept", VIEWS_MAX + 1);
        assert_eq!(listing(most + 1), Err(over));
    }

    #[test]
    fn a_project_whose_folder_is_no_absolute_path_is_refused() {
        let mut unrooted = kept();
        unrooted.dir = "w".to_owned();
        let refused = read(&unrooted).unwrap_err();
        assert_eq!(refused, "project bitwarden backup's folder: \"w\" is no absolute path");
        assert_eq!(read(&kept()).unwrap().dir.as_str(), "/w");
    }

    #[test]
    fn a_project_holds_its_folder_or_its_clone_s_worktrees_and_says_which() {
        let mut folder = kept();
        let own = rows(&mut folder).remove(0);
        folder.holds = Some(api::project::Holds::Folder(own));
        let project = read(&folder).unwrap();
        let Holds::Folder(own) = &project.holds else {
            panic!("a folder holds its own worktree: {:?}", project.holds);
        };
        assert_eq!((own.id, project.worktrees().len()), (worktree(11), 1));
        let clone = read(&kept()).unwrap();
        let ids: Vec<WorktreeId> = clone.worktrees().iter().map(|it| it.id).collect();
        assert_eq!(ids, [worktree(11), worktree(12)]);
        folder.holds = None;
        assert_eq!(read(&folder), Err("project bitwarden backup says not what it holds".to_owned()));
    }

    #[test]
    fn a_worktree_said_with_no_stage_folder_or_arrangement_lune_reads_is_refused() {
        type Broke = fn(&mut api::Worktree);
        let unknown = "worktree docs says no stage lune knows";
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
            let mut kept = kept();
            broke(&mut rows(&mut kept)[1]);
            assert_eq!(read(&kept), Err(why.to_owned()));
        }
    }

    #[test]
    fn a_terminal_whose_folder_is_no_folder_luned_keeps_is_refused() {
        let unnamed = "a path holds a part that is no name: empty, `.` or `..`";
        let unkept = [
            ("", "\"\" is no absolute path".to_owned()),
            ("w/a", "\"w/a\" is no absolute path".to_owned()),
            ("/w/../a", format!("\"/w/../a\": {unnamed}")),
        ];
        for (folder, why) in unkept {
            let mut kept = kept();
            rows(&mut kept)[0].terminals[1].folder = folder.to_owned();
            let refused = format!("terminal 0000000000000016's folder: {why}");
            assert_eq!(read(&kept), Err(refused), "{folder:?}");
        }
    }

    #[test]
    fn a_clone_lists_the_worktrees_not_open_under_its_folder_or_is_refused() {
        use api::project::clone::Listing;
        let listing = |subfolders: Vec<String>| {
            let mut kept = kept();
            let Some(api::project::Holds::Clone(clone)) = &mut kept.holds else {
                panic!("the project is a clone");
            };
            clone.listing = Some(Listing::Unopened(api::project::Unopened { subfolders }));
            read(&kept).map(|it| it.holds)
        };
        let unopened = |holds: Holds| {
            let Holds::Clone { unopened, .. } = holds else {
                panic!("a clone: {holds:?}");
            };
            unopened
        };
        let spike = listing(vec!["spike".to_owned()]).map(unopened);
        assert_eq!(spike, Ok(Unopened::Listed(vec![Dir::parse("/w/spike").unwrap()])));
        let unsaid = unopened(read(&kept()).unwrap().holds);
        assert_eq!(unsaid, Unopened::Unlisted("luned says nothing of them".to_owned()));
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
