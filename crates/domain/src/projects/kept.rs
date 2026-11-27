use std::collections::BTreeSet;
use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::layout::{Leaf as _, NODES_MAX, Preordered, Towards};
use super::{Arranged, Browser, BrowserUrl, Focus, Layout, Shape, Worktree, WorktreeId};
use super::{Below, Dir, Holds, Id, Line, Listed, Name, Project, ProjectId, ProjectName, Projects};
use super::{
    LINE_BYTES_MAX, NAME_CHARS_MAX, PATH_BYTES_MAX, PROJECTS_MAX, SESSION_BYTES_MAX, URL_BYTES_MAX,
};
use super::{Runs, Session, Spot, Subfolder, Terminal, TerminalViewId, Used, View, ViewId};
use super::{VIEWS_MAX, WORKTREES_MAX};
use crate::agent::Harness;

const VERSION: u32 = 13;

const WITHOUT_LAYOUTS: u32 = 1;

const WITHOUT_UNRUN: u32 = 2;

const WITHOUT_NAMES_OR_USE: u32 = 3;

const UNROOTED: u32 = 5;

const LAID_AS_VIEW: u32 = 6;

const WITHOUT_WELCOMES: u32 = 7;

const ROWS_UNOPENED: u32 = 8;

const WITHOUT_LAST_FOCUS: u32 = 9;

const WITHOUT_REVIEWS: u32 = 11;

const WITHOUT_BROWSERS: u32 = 12;

const CHAR_BYTES_MAX: usize = 4;

const ESCAPED_BYTES_PER_BYTE: usize = 2;

const NAME_BYTES_MAX: usize = CHAR_BYTES_MAX * NAME_CHARS_MAX;

const ENTRY_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE * (PATH_BYTES_MAX + NAME_BYTES_MAX) + 256;

const TERMINAL_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE
    * (PATH_BYTES_MAX + LINE_BYTES_MAX + SESSION_BYTES_MAX + NAME_BYTES_MAX)
    + 256;

const BROWSER_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE * URL_BYTES_MAX + 256;

const SPLIT_BYTES_MAX: usize = 128;

pub const FILE_BYTES_MAX: usize = 32 * 1024 * 1024;

const _: () = assert!(BROWSER_BYTES_MAX <= TERMINAL_BYTES_MAX);

const _: () = assert!(
    (PROJECTS_MAX + WORKTREES_MAX) * ENTRY_BYTES_MAX
        + VIEWS_MAX * TERMINAL_BYTES_MAX
        + WORKTREES_MAX * NODES_MAX * SPLIT_BYTES_MAX
        <= FILE_BYTES_MAX
);

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept<P> {
    version: u32,
    projects: Vec<P>,
}

type Laid = KeptProject<KeptFolderWorktree<KeptLayout>, KeptCloneWorktree<KeptLayout>>;

type LaidBefore =
    KeptProject<KeptFolderWorktree<KeptOlderLayout>, KeptCloneWorktree<KeptOlderLayout>>;

type Viewed = KeptProject<ViewedFolderWorktree, ViewedCloneWorktree>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptProject<F, C> {
    id: String,
    name: String,
    dir: String,
    holds: KeptHolds<F, C>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptHolds<F, C> {
    Folder(F),
    Clone(Vec<C>),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "L: Deserialize<'de>"))]
struct KeptFolderWorktree<L> {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<L>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_millis: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "L: Deserialize<'de>"))]
struct KeptCloneWorktree<L> {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(rename = "worktree")]
    subfolder: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<L>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_millis: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewedFolderWorktree {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    view: Option<KeptOlderLayout>,
    #[serde(default)]
    used_millis: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ViewedCloneWorktree {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(rename = "worktree")]
    subfolder: String,
    #[serde(default)]
    view: Option<KeptOlderLayout>,
    #[serde(default)]
    used_millis: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptUnrooted {
    version: u32,
    projects: Vec<UnrootedProject>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnrootedProject {
    id: String,
    name: String,
    dir: String,
    #[serde(default, rename = "workstreams")]
    worktrees: Vec<UnrootedWorktree>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnrootedWorktree {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    below: String,
    #[serde(default)]
    view: Option<KeptOlderLayout>,
    #[serde(default)]
    used_millis: Option<u64>,
}

struct Counted<'a> {
    tree: Option<&'a [KeptNode]>,
    last_focus: bool,
    used_millis: Option<u64>,
}

trait KeptLaid: Sized {
    fn tree(&self) -> &[KeptNode];

    fn focuses_last(&self) -> bool;

    fn read(self) -> Result<Layout, String>;
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptLayout {
    shown: KeptShown,
    tree: Vec<KeptNode>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum KeptShown {
    Tabs {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last: Option<String>,
    },
    Panes {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        focus: Option<KeptFocus>,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptFocus {
    Last(String),
    Full(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptOlderLayout {
    #[serde(default)]
    full: Option<String>,
    #[serde(default)]
    last_focus: Option<String>,
    tree: Vec<KeptNode>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum KeptNode {
    Split(KeptSplit),
    Terminal(KeptTerminal),
    Welcome(KeptWelcome),
    Browser(KeptBrowser),
    Review(KeptReview),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptWelcome {
    id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptBrowser {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptReview {
    id: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptSplit {
    towards: KeptTowards,
    share: u32,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptTowards {
    Right,
    Down,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptTerminal {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    folder: String,
    runs: KeptRuns,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum KeptRuns {
    Shell,
    Command(String),
    Unrun(String),
    Agent(KeptAgent),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptAgent {
    harness: KeptHarness,
    session: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptHarness {
    ClaudeCode,
    Pi,
    Antigravity,
}

pub struct Parsed {
    pub projects: Projects,
    pub migrated: bool,
}

pub fn parse(
    text: &str,
    root: &mut impl FnMut(&Dir) -> Result<Option<Spot>, String>,
    draw: &mut impl FnMut() -> io::Result<Id>,
) -> Result<Parsed, String> {
    let Versioned { version } = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if !(WITHOUT_LAYOUTS..=VERSION).contains(&version) {
        let read = format!("this luned reads only {WITHOUT_LAYOUTS} to {VERSION}");
        return Err(format!("it is of version {version}, and {read}"));
    }
    if version > UNROOTED {
        return match version {
            WITHOUT_REVIEWS..=VERSION => read_laid(version, parsed::<Laid>(text, version)?),
            LAID_AS_VIEW => {
                let viewed = parsed::<Viewed>(text, version)?;
                read_laid(version, viewed.into_iter().map(Viewed::laid).collect())
            }
            _ => read_laid(version, parsed::<LaidBefore>(text, version)?),
        };
    }
    let kept: KeptUnrooted = serde_json::from_str(text).map_err(|error| error.to_string())?;
    assert_eq!(kept.version, version, "a file says one version");
    let worktrees = || kept.projects.iter().flat_map(|project| &project.worktrees);
    bounded(version, kept.projects.len(), || {
        worktrees().map(UnrootedWorktree::counted)
    })?;
    let unrooted = kept.projects.into_iter().map(UnrootedProject::read);
    let unrooted: Vec<Unrooted> = unrooted.collect::<Result<_, _>>()?;
    let projects = root_projects(unrooted, root, draw)?;
    Ok(Parsed {
        projects: Projects::checked(projects)?,
        migrated: true,
    })
}

fn parsed<P: DeserializeOwned>(text: &str, version: u32) -> Result<Vec<P>, String> {
    let kept: Kept<P> = serde_json::from_str(text).map_err(|error| error.to_string())?;
    assert_eq!(kept.version, version, "a file says one version");
    Ok(kept.projects)
}

fn read_laid<L: KeptLaid>(
    version: u32,
    mut kept: Vec<KeptProject<KeptFolderWorktree<L>, KeptCloneWorktree<L>>>,
) -> Result<Parsed, String> {
    bounded(version, kept.len(), || {
        kept.iter().flat_map(KeptProject::counted)
    })?;
    let unopened = if version <= ROWS_UNOPENED {
        unopened_forgotten(&mut kept)
    } else {
        0
    };
    let projects = kept.into_iter().map(KeptProject::read);
    let projects = projects.collect::<Result<_, String>>()?;
    Ok(Parsed {
        projects: Projects::checked(projects)?,
        migrated: unopened > 0,
    })
}

fn unopened_forgotten<L>(
    projects: &mut [KeptProject<KeptFolderWorktree<L>, KeptCloneWorktree<L>>],
) -> usize {
    let mut forgotten = 0;
    for project in projects {
        let KeptHolds::Clone(rows) = &mut project.holds else {
            continue;
        };
        let before = rows.len();
        rows.retain(|it| it.layout.is_some());
        forgotten += before - rows.len();
    }
    forgotten
}

fn bounded<'a, I: Iterator<Item = Counted<'a>>>(
    version: u32,
    projects: usize,
    worktrees: impl Fn() -> I,
) -> Result<(), String> {
    let laid = || worktrees().filter_map(|it| it.tree);
    let nodes = || laid().flatten();
    if projects > PROJECTS_MAX {
        return Err(format!("it holds over {PROJECTS_MAX} projects"));
    }
    if worktrees().count() > WORKTREES_MAX {
        return Err(format!("it holds over {WORKTREES_MAX} worktrees"));
    }
    if nodes()
        .filter(|node| !matches!(node, KeptNode::Split(_)))
        .count()
        > VIEWS_MAX
    {
        return Err(format!("it holds over {VIEWS_MAX} views"));
    }
    if version == WITHOUT_LAYOUTS && laid().next().is_some() {
        return Err(format!(
            "it is of version {WITHOUT_LAYOUTS}, which holds no layouts"
        ));
    }
    let unrun = |node: &KeptNode| {
        matches!(
            node,
            KeptNode::Terminal(KeptTerminal {
                runs: KeptRuns::Unrun(_),
                ..
            })
        )
    };
    if version <= WITHOUT_LAST_FOCUS && worktrees().any(|it| it.last_focus) {
        return Err(format!(
            "it is of version {version}, which keeps no last focus"
        ));
    }
    if version <= WITHOUT_WELCOMES && nodes().any(|it| matches!(it, KeptNode::Welcome(_))) {
        return Err(format!(
            "it is of version {version}, which holds no welcome"
        ));
    }
    if version <= WITHOUT_BROWSERS && nodes().any(|it| matches!(it, KeptNode::Browser(_))) {
        return Err(format!(
            "it is of version {version}, which holds no browser"
        ));
    }
    if version <= WITHOUT_REVIEWS && nodes().any(|it| matches!(it, KeptNode::Review(_))) {
        return Err(format!("it is of version {version}, which holds no review"));
    }
    if version <= WITHOUT_UNRUN && nodes().any(unrun) {
        return Err(format!(
            "it is of version {version}, which holds no line left unrun"
        ));
    }
    let named =
        |node: &KeptNode| matches!(node, KeptNode::Terminal(KeptTerminal { name: Some(_), .. }));
    if version <= WITHOUT_NAMES_OR_USE {
        if nodes().any(named) {
            return Err(format!(
                "it is of version {version}, which holds no terminal's name"
            ));
        }
        if worktrees().any(|it| it.used_millis.is_some()) {
            return Err(format!(
                "it is of version {version}, which holds no worktree's use"
            ));
        }
    }
    Ok(())
}

struct Unrooted {
    id: ProjectId,
    name: ProjectName,
    dir: Dir,
    worktrees: Vec<(Below, Worktree)>,
}

struct Gathered {
    id: ProjectId,
    name: ProjectName,
    dir: Dir,
    holds: Gathering,
}

enum Gathering {
    Folder(Vec<Worktree>),
    Clone {
        own: Vec<Worktree>,
        rows: Vec<(Subfolder, Worktree)>,
        subfolders: BTreeSet<Subfolder>,
    },
}

fn root_projects(
    unrooted: Vec<Unrooted>,
    root: &mut impl FnMut(&Dir) -> Result<Option<Spot>, String>,
    draw: &mut impl FnMut() -> io::Result<Id>,
) -> Result<Vec<Project>, String> {
    let view_count = |worktrees: &mut dyn Iterator<Item = &Worktree>| -> usize {
        worktrees.map(|it| it.layout.len()).sum()
    };
    let views: usize = unrooted
        .iter()
        .map(|it| view_count(&mut it.worktrees.iter().map(|(_, it)| it)))
        .sum();
    let mut taken: BTreeSet<Id> = BTreeSet::new();
    for project in &unrooted {
        let worktrees = project.worktrees.iter().map(|(_, it)| it);
        let layouts = worktrees.clone().map(|it| &it.layout);
        let views = layouts.flat_map(|layout| layout.views().map(|it| it.id().id()));
        let ids = std::iter::once(project.id.id()).chain(worktrees.map(|it| it.id.id()));
        for id in ids.chain(views) {
            if !taken.insert(id) {
                return Err(format!("the id {id} comes twice"));
            }
        }
    }
    let mut fresh = || -> Result<WorktreeId, String> {
        for _ in 0..super::FRESH_TRIES {
            let id = draw().map_err(|error| format!("no id is drawn: {error}"))?;
            if taken.insert(id) {
                return Ok(WorktreeId::new(id));
            }
        }
        Err(format!("{} ids drawn were all taken", super::FRESH_TRIES))
    };
    let mut gathered: Vec<Gathered> = Vec::with_capacity(unrooted.len());
    for project in unrooted {
        let Gathered {
            id,
            name,
            dir,
            holds,
        } = root_project(project, root)?;
        match holds {
            Gathering::Folder(more) => {
                let held = gathered.iter_mut().find_map(|it| match &mut it.holds {
                    Gathering::Folder(all) if it.dir == dir => Some(all),
                    Gathering::Folder(_) | Gathering::Clone { .. } => None,
                });
                match held {
                    Some(all) => all.extend(more),
                    None => gathered.push(Gathered {
                        id,
                        name,
                        dir,
                        holds: Gathering::Folder(more),
                    }),
                }
            }
            Gathering::Clone {
                own: more_own,
                rows: more_rows,
                subfolders,
            } => {
                let held = gathered.iter_mut().find_map(|it| match &mut it.holds {
                    Gathering::Clone { own, rows, .. } if it.dir == dir => Some((own, rows)),
                    Gathering::Folder(_) | Gathering::Clone { .. } => None,
                });
                if let Some((own, rows)) = held {
                    own.extend(more_own);
                    rows.extend(more_rows);
                } else {
                    let holds = Gathering::Clone {
                        own: more_own,
                        rows: more_rows,
                        subfolders,
                    };
                    gathered.push(Gathered {
                        id,
                        name,
                        dir,
                        holds,
                    });
                }
            }
        }
    }
    let shaped = gathered.into_iter().map(|it| shape(it, &mut fresh));
    let projects: Vec<Project> = shaped.collect::<Result<_, _>>()?;
    let rooted: usize = projects
        .iter()
        .map(|it| view_count(&mut it.worktrees()))
        .sum();
    assert_eq!(rooted, views, "rooting keeps every view");
    Ok(projects)
}

fn root_project(
    project: Unrooted,
    root: &mut impl FnMut(&Dir) -> Result<Option<Spot>, String>,
) -> Result<Gathered, String> {
    let named = |at: &Dir| {
        if project.name.as_str() == project.dir.last() {
            ProjectName::of_folder(at).unwrap_or_else(|_| project.name.clone())
        } else {
            project.name.clone()
        }
    };
    let unread = |why: String| {
        let (name, dir) = (project.name.as_str(), project.dir.as_str());
        let unsaid = "git did not say whose clone it is, so none is rooted";
        format!("project {name} in {dir}: {unsaid}: {why}")
    };
    let Some(spot) = root(&project.dir).map_err(unread)? else {
        let Unrooted {
            id,
            name,
            dir,
            worktrees,
        } = project;
        let all = worktrees.into_iter().map(|(_, it)| it).collect();
        return Ok(Gathered {
            id,
            name,
            dir,
            holds: Gathering::Folder(all),
        });
    };
    let name = named(spot.dir());
    let Unrooted {
        id, dir, worktrees, ..
    } = project;
    let Some(subfolders) = spot.listed().map(Listed::subfolders) else {
        let all = worktrees.into_iter().map(|(_, it)| it).collect();
        let dir = spot.dir().clone();
        return Ok(Gathered {
            id,
            name,
            dir,
            holds: Gathering::Folder(all),
        });
    };
    let root = spot.dir().clone();
    let (mut own, mut rows) = (Vec::new(), Vec::new());
    for (below, worktree) in worktrees {
        match subfolder_holding(&root, subfolders, &dir.join(&below)) {
            Some(subfolder) => rows.push((subfolder, worktree)),
            None => own.push(worktree),
        }
    }
    let holds = Gathering::Clone {
        own,
        rows,
        subfolders: subfolders.clone(),
    };
    Ok(Gathered {
        id,
        name,
        dir: root,
        holds,
    })
}

fn subfolder_holding(
    root: &Dir,
    subfolders: &BTreeSet<Subfolder>,
    folder: &Dir,
) -> Option<Subfolder> {
    let holding = subfolders
        .iter()
        .filter(|it| root.at(it).below(folder).is_some());
    let listed = holding.max_by_key(|it| it.as_str().len()).cloned();
    listed.or_else(|| {
        let below = root.below(folder)?;
        let first = below
            .as_str()
            .split('/')
            .next()
            .expect("a split yields at least once");
        Subfolder::parse(first).ok()
    })
}

fn shape(
    gathered: Gathered,
    fresh: &mut impl FnMut() -> Result<WorktreeId, String>,
) -> Result<Project, String> {
    let Gathered {
        id,
        name,
        dir,
        holds,
    } = gathered;
    let holds = match holds {
        Gathering::Folder(all) => Holds::Folder(one_of(all, fresh)?),
        Gathering::Clone {
            own,
            rows,
            subfolders,
        } => shape_clone(own, rows, &subfolders, fresh)?,
    };
    Ok(Project {
        id,
        name,
        dir,
        holds,
    })
}

fn shape_clone(
    own: Vec<Worktree>,
    rows: Vec<(Subfolder, Worktree)>,
    subfolders: &BTreeSet<Subfolder>,
    fresh: &mut impl FnMut() -> Result<WorktreeId, String>,
) -> Result<Holds, String> {
    let mut merged: Vec<(Subfolder, Worktree)> = Vec::with_capacity(rows.len());
    for (subfolder, worktree) in rows {
        let held = merged.iter_mut().find(|(it, _)| *it == subfolder);
        match held {
            Some((_, held)) => join(held, worktree)?,
            None => merged.push((subfolder, worktree)),
        }
    }
    let own_views = own.iter().any(|it| !it.layout.is_empty());
    if own_views && merged.is_empty() {
        let Some(first) = subfolders.first() else {
            return one_of(own, fresh).map(Holds::Folder);
        };
        merged.push((first.clone(), Worktree::new(fresh()?, None)));
    }
    let used_last =
        |(at, (_, it)): &(usize, &(Subfolder, Worktree))| (it.used, std::cmp::Reverse(*at));
    let last = merged
        .iter()
        .enumerate()
        .max_by_key(used_last)
        .map(|(at, _)| at);
    if let Some(last) = last.filter(|_| own_views) {
        let (_, into) = &mut merged[last];
        for worktree in own {
            into.layout = std::mem::take(&mut into.layout).joined(worktree.layout)?;
        }
    }
    merged.retain(|(_, it)| !it.layout.is_empty());
    Ok(Holds::Clone(merged))
}

fn one_of(
    all: Vec<Worktree>,
    fresh: &mut impl FnMut() -> Result<WorktreeId, String>,
) -> Result<Worktree, String> {
    let mut all = all.into_iter();
    let Some(mut one) = all.next() else {
        return Ok(Worktree::new(fresh()?, None));
    };
    for other in all {
        join(&mut one, other)?;
    }
    Ok(one)
}

fn join(held: &mut Worktree, other: Worktree) -> Result<(), String> {
    let views = held.layout.len() + other.layout.len();
    let layout = std::mem::take(&mut held.layout).joined(other.layout)?;
    if other.used > held.used {
        *held = Worktree { layout, ..other };
    } else {
        held.layout = layout;
    }
    assert_eq!(held.layout.len(), views, "worktrees joined keep every view");
    Ok(())
}

impl<L: KeptLaid> KeptProject<KeptFolderWorktree<L>, KeptCloneWorktree<L>> {
    fn read(self) -> Result<Project, String> {
        let (id, name) = (self.id.parse()?, ProjectName::parse(&self.name)?);
        let dir = Dir::parse(&self.dir)?;
        let holds = match self.holds {
            KeptHolds::Folder(own) => Holds::Folder(own.read()?),
            KeptHolds::Clone(rows) => {
                let rows = rows.into_iter().map(KeptCloneWorktree::read);
                Holds::Clone(rows.collect::<Result<_, _>>()?)
            }
        };
        Ok(Project {
            id,
            name,
            dir,
            holds,
        })
    }

    fn counted(&self) -> impl Iterator<Item = Counted<'_>> {
        let (own, rows): (_, &[KeptCloneWorktree<L>]) = match &self.holds {
            KeptHolds::Folder(own) => (Some(own.counted()), &[]),
            KeptHolds::Clone(rows) => (None, rows),
        };
        own.into_iter()
            .chain(rows.iter().map(KeptCloneWorktree::counted))
    }
}

impl Laid {
    fn of(project: &Project) -> Laid {
        let holds = match &project.holds {
            Holds::Folder(own) => KeptHolds::Folder(KeptFolderWorktree::of(own)),
            Holds::Clone(rows) => {
                let rows = rows
                    .iter()
                    .map(|(subfolder, it)| KeptCloneWorktree::of(subfolder, it));
                KeptHolds::Clone(rows.collect())
            }
        };
        KeptProject {
            id: project.id.to_string(),
            name: project.name.as_str().to_owned(),
            dir: project.dir.0.clone(),
            holds,
        }
    }
}

impl Viewed {
    fn laid(self) -> LaidBefore {
        let KeptProject {
            id,
            name,
            dir,
            holds,
        } = self;
        let holds = match holds {
            KeptHolds::Folder(own) => KeptHolds::Folder(own.laid()),
            KeptHolds::Clone(rows) => {
                KeptHolds::Clone(rows.into_iter().map(ViewedCloneWorktree::laid).collect())
            }
        };
        KeptProject {
            id,
            name,
            dir,
            holds,
        }
    }
}

impl ViewedFolderWorktree {
    fn laid(self) -> KeptFolderWorktree<KeptOlderLayout> {
        let ViewedFolderWorktree {
            id,
            name,
            view,
            used_millis,
        } = self;
        KeptFolderWorktree {
            id,
            name,
            layout: view,
            used_millis,
        }
    }
}

impl ViewedCloneWorktree {
    fn laid(self) -> KeptCloneWorktree<KeptOlderLayout> {
        let ViewedCloneWorktree {
            id,
            name,
            subfolder,
            view,
            used_millis,
        } = self;
        KeptCloneWorktree {
            id,
            name,
            subfolder,
            layout: view,
            used_millis,
        }
    }
}

fn read_worktree<L: KeptLaid>(
    id: &str,
    name: Option<&str>,
    layout: Option<L>,
    used_millis: Option<u64>,
) -> Result<Worktree, String> {
    let id: WorktreeId = id.parse()?;
    let layout = match layout {
        None => Layout::default(),
        Some(layout) => layout
            .read()
            .map_err(|why| format!("worktree {id}'s layout: {why}"))?,
    };
    Ok(Worktree {
        id,
        renamed: name.map(Name::parse).transpose()?,
        layout,
        used: used_millis
            .map(Used::read)
            .transpose()
            .map_err(|why| format!("worktree {id}: {why}"))?,
    })
}

fn counted<L: KeptLaid>(layout: Option<&L>, used_millis: Option<u64>) -> Counted<'_> {
    let tree = layout.map(KeptLaid::tree);
    Counted {
        tree,
        last_focus: layout.is_some_and(KeptLaid::focuses_last),
        used_millis,
    }
}

impl<L: KeptLaid> KeptFolderWorktree<L> {
    fn read(self) -> Result<Worktree, String> {
        read_worktree(
            &self.id,
            self.name.as_deref(),
            self.layout,
            self.used_millis,
        )
    }

    fn counted(&self) -> Counted<'_> {
        counted(self.layout.as_ref(), self.used_millis)
    }
}

impl KeptFolderWorktree<KeptLayout> {
    fn of(worktree: &Worktree) -> KeptFolderWorktree<KeptLayout> {
        KeptFolderWorktree {
            id: worktree.id.to_string(),
            name: worktree.renamed.as_ref().map(|name| name.0.clone()),
            layout: KeptLayout::of(&worktree.layout),
            used_millis: worktree.used.map(Used::millis),
        }
    }
}

impl<L: KeptLaid> KeptCloneWorktree<L> {
    fn read(self) -> Result<(Subfolder, Worktree), String> {
        let (name, used_millis) = (self.name.as_deref(), self.used_millis);
        let worktree = read_worktree(&self.id, name, self.layout, used_millis)?;
        let at = |why: String| format!("worktree {}: {why}", worktree.id);
        Ok((Subfolder::parse(&self.subfolder).map_err(at)?, worktree))
    }

    fn counted(&self) -> Counted<'_> {
        counted(self.layout.as_ref(), self.used_millis)
    }
}

impl KeptCloneWorktree<KeptLayout> {
    fn of(subfolder: &Subfolder, worktree: &Worktree) -> KeptCloneWorktree<KeptLayout> {
        let KeptFolderWorktree {
            id,
            name,
            layout,
            used_millis,
        } = KeptFolderWorktree::of(worktree);
        let subfolder = subfolder.as_str().to_owned();
        KeptCloneWorktree {
            id,
            name,
            subfolder,
            layout,
            used_millis,
        }
    }
}

impl UnrootedProject {
    fn read(self) -> Result<Unrooted, String> {
        let (id, name) = (self.id.parse()?, ProjectName::parse(&self.name)?);
        let dir = Dir::parse(&self.dir)?;
        let worktrees = self.worktrees.into_iter().map(UnrootedWorktree::read);
        Ok(Unrooted {
            id,
            name,
            dir,
            worktrees: worktrees.collect::<Result<_, _>>()?,
        })
    }
}

impl UnrootedWorktree {
    fn read(self) -> Result<(Below, Worktree), String> {
        let worktree = read_worktree(&self.id, self.name.as_deref(), self.view, self.used_millis)?;
        Ok((Below::parse(&self.below)?, worktree))
    }

    fn counted(&self) -> Counted<'_> {
        counted(self.view.as_ref(), self.used_millis)
    }
}

fn read_tree(tree: Vec<KeptNode>) -> Result<Vec<Preordered<View>>, String> {
    let tree = tree.into_iter().map(|node| match node {
        KeptNode::Split(KeptSplit { towards, share }) => Ok(Preordered::Split {
            towards: towards.read(),
            share,
        }),
        KeptNode::Terminal(terminal) => terminal
            .read()
            .map(|terminal| Preordered::View(View::Terminal(terminal))),
        KeptNode::Welcome(KeptWelcome { id }) => {
            id.parse().map(|id| Preordered::View(View::Welcome(id)))
        }
        KeptNode::Browser(KeptBrowser { id, url }) => {
            let id = id.parse()?;
            let why = |why: String| format!("browser {id}: {why}");
            let url = url
                .as_deref()
                .map(BrowserUrl::parse)
                .transpose()
                .map_err(why)?;
            Ok(Preordered::View(View::Browser(Browser::new(id, url))))
        }
        KeptNode::Review(KeptReview { id }) => {
            id.parse().map(|id| Preordered::View(View::Review(id)))
        }
    });
    tree.collect()
}

impl KeptLaid for KeptLayout {
    fn tree(&self) -> &[KeptNode] {
        &self.tree
    }

    fn focuses_last(&self) -> bool {
        match &self.shown {
            KeptShown::Tabs { last } => last.is_some(),
            KeptShown::Panes { focus } => matches!(focus, Some(KeptFocus::Last(_))),
        }
    }

    fn read(self) -> Result<Layout, String> {
        Layout::read(read_tree(self.tree)?, self.shown.read()?)
    }
}

impl KeptLayout {
    fn of(layout: &Layout) -> Option<KeptLayout> {
        let tree = layout
            .tree()?
            .preorder()
            .into_iter()
            .map(|node| match node {
                Preordered::Split { towards, share } => KeptNode::Split(KeptSplit {
                    towards: KeptTowards::of(towards),
                    share,
                }),
                Preordered::View(View::Terminal(terminal)) => {
                    KeptNode::Terminal(KeptTerminal::of(terminal))
                }
                Preordered::View(View::Welcome(id)) => {
                    KeptNode::Welcome(KeptWelcome { id: id.to_string() })
                }
                Preordered::View(View::Browser(browser)) => KeptNode::Browser(KeptBrowser {
                    id: browser.id().to_string(),
                    url: browser.url().map(|url| url.as_str().to_owned()),
                }),
                Preordered::View(View::Review(id)) => {
                    KeptNode::Review(KeptReview { id: id.to_string() })
                }
            });
        let shown = KeptShown::of(layout.shape()?);
        Some(KeptLayout {
            shown,
            tree: tree.collect(),
        })
    }
}

impl KeptShown {
    fn read(self) -> Result<Shape, String> {
        let view = |id: String| id.parse::<ViewId>();
        Ok(match self {
            KeptShown::Tabs { last } => Shape::Tabs {
                last: last.map(view).transpose()?,
            },
            KeptShown::Panes { focus: None } => Shape::Panes { focus: None },
            KeptShown::Panes {
                focus: Some(KeptFocus::Last(last)),
            } => Shape::Panes {
                focus: Some(Focus::Last(view(last)?)),
            },
            KeptShown::Panes {
                focus: Some(KeptFocus::Full(full)),
            } => Shape::Panes {
                focus: Some(Focus::Full(view(full)?)),
            },
        })
    }

    fn of(shape: Shape) -> KeptShown {
        let id = |view: ViewId| view.to_string();
        match shape {
            Shape::Tabs { last } => KeptShown::Tabs { last: last.map(id) },
            Shape::Panes { focus: None } => KeptShown::Panes { focus: None },
            Shape::Panes {
                focus: Some(Focus::Last(last)),
            } => KeptShown::Panes {
                focus: Some(KeptFocus::Last(id(last))),
            },
            Shape::Panes {
                focus: Some(Focus::Full(full)),
            } => KeptShown::Panes {
                focus: Some(KeptFocus::Full(id(full))),
            },
        }
    }
}

impl KeptLaid for KeptOlderLayout {
    fn tree(&self) -> &[KeptNode] {
        &self.tree
    }

    fn focuses_last(&self) -> bool {
        self.last_focus.is_some()
    }

    fn read(self) -> Result<Layout, String> {
        let full: Option<ViewId> = self.full.as_deref().map(str::parse).transpose()?;
        let last_focus: Option<ViewId> = self.last_focus.as_deref().map(str::parse).transpose()?;
        let tree = read_tree(self.tree)?;
        let full = match (full, last_focus) {
            (None, last) => return Layout::read(tree, Shape::Tabs { last }),
            (Some(full), None) => full,
            (Some(full), Some(last_focus)) => {
                let one = format!("the layout is filled by view {full}");
                return Err(format!(
                    "{one}, and so is focused on no other view {last_focus}"
                ));
            }
        };
        let mut layout = Layout::read(
            tree,
            Shape::Panes {
                focus: Some(Focus::Full(full)),
            },
        )?;
        layout.arranged_as(Arranged::Tabs)?;
        let shape = Some(Shape::Tabs { last: Some(full) });
        assert_eq!(
            layout.shape(),
            shape,
            "an older file's full view is the tab focused"
        );
        Ok(layout)
    }
}

impl KeptTowards {
    fn read(self) -> Towards {
        match self {
            KeptTowards::Right => Towards::Right,
            KeptTowards::Down => Towards::Down,
        }
    }

    fn of(towards: Towards) -> KeptTowards {
        match towards {
            Towards::Right => KeptTowards::Right,
            Towards::Down => KeptTowards::Down,
        }
    }
}

impl KeptTerminal {
    fn read(self) -> Result<Terminal, String> {
        let id = TerminalViewId::new(self.id.parse()?);
        let why = |why: String| format!("terminal {id}: {why}");
        let folder = Dir::parse(&self.folder).map_err(why)?;
        let name = self
            .name
            .as_deref()
            .map(Name::parse)
            .transpose()
            .map_err(why)?;
        let runs = match self.runs {
            KeptRuns::Shell => Runs::Shell,
            KeptRuns::Command(line) => Runs::Command(Line::parse(&line).map_err(why)?),
            KeptRuns::Unrun(line) => Runs::Unrun(Line::parse(&line).map_err(why)?),
            KeptRuns::Agent(KeptAgent { harness, session }) => Runs::Agent {
                harness: harness.read(),
                session: Session::parse(&session).map_err(why)?,
            },
        };
        Ok(Terminal::new(id, folder, runs).named(name))
    }

    fn of(terminal: &Terminal) -> KeptTerminal {
        let runs = match terminal.runs() {
            Runs::Shell => KeptRuns::Shell,
            Runs::Command(line) => KeptRuns::Command(line.as_str().to_owned()),
            Runs::Unrun(line) => KeptRuns::Unrun(line.as_str().to_owned()),
            Runs::Agent { harness, session } => KeptRuns::Agent(KeptAgent {
                harness: KeptHarness::of(*harness),
                session: session.as_str().to_owned(),
            }),
        };
        KeptTerminal {
            id: terminal.id().to_string(),
            name: terminal.name().map(|name| name.as_str().to_owned()),
            folder: terminal.folder().as_str().to_owned(),
            runs,
        }
    }
}

impl KeptHarness {
    fn read(self) -> Harness {
        match self {
            KeptHarness::ClaudeCode => Harness::ClaudeCode,
            KeptHarness::Pi => Harness::Pi,
            KeptHarness::Antigravity => Harness::Antigravity,
        }
    }

    fn of(harness: Harness) -> KeptHarness {
        match harness {
            Harness::ClaudeCode => KeptHarness::ClaudeCode,
            Harness::Pi => KeptHarness::Pi,
            Harness::Antigravity => KeptHarness::Antigravity,
        }
    }
}

pub fn written(projects: &Projects) -> String {
    let projects = projects.projects.iter().map(Laid::of).collect();
    let kept = Kept {
        version: VERSION,
        projects,
    };
    let mut text = serde_json::to_string_pretty(&kept).expect("strings and numbers serialize");
    text.push('\n');
    assert!(
        text.len() <= FILE_BYTES_MAX,
        "what is kept is bounded to what is read back"
    );
    text
}

#[cfg(test)]
mod tests {
    use super::super::{Change, Opens, Placed, Relaid};
    use super::*;

    fn add_in(project: ProjectId, spot: Spot) -> Change {
        Change::AddWorktree {
            project,
            spot,
            name: None,
            opens: Opens::Shell,
        }
    }

    fn add_named(project: ProjectId, spot: Spot, name: Option<Name>) -> Change {
        Change::AddWorktree {
            project,
            spot,
            name,
            opens: Opens::Shell,
        }
    }

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn alone(at: &str) -> Spot {
        Spot::in_folder(dir(at), None)
    }

    fn tree(at: &str) -> Subfolder {
        Subfolder::parse(at).unwrap()
    }

    fn listing(at: &[&str]) -> Listed {
        Listed::of(at.iter().copied().map(tree).collect()).unwrap()
    }

    fn clone_of(root: &str, subfolders: &[&str]) -> Spot {
        Spot::in_clone(dir(root), listing(subfolders), None)
    }

    fn spot_at(root: &str, at: &str) -> Spot {
        Spot::in_clone(dir(root), listing(&[at]), Some(tree(at)))
    }

    fn is_clone(project: &Project) -> bool {
        matches!(project.holds(), Holds::Clone(_))
    }

    fn rows(project: &Project) -> &[(Subfolder, Worktree)] {
        let Holds::Clone(rows) = project.holds() else {
            panic!("{project:?} is a clone")
        };
        rows
    }

    fn row<'a>(project: &'a Project, at: &str) -> &'a Worktree {
        let found = rows(project).iter().find(|(it, _)| *it == tree(at));
        &found
            .unwrap_or_else(|| panic!("{at} is a worktree of {project:?}"))
            .1
    }

    fn counting(from: u64) -> impl FnMut() -> io::Result<Id> {
        let mut next = from;
        move || {
            next += 1;
            Ok(Id::of(next).unwrap())
        }
    }

    fn rooted_as(
        text: &str,
        mut root: impl FnMut(&Dir) -> Result<Option<Spot>, String>,
    ) -> Result<Parsed, String> {
        super::parse(text, &mut root, &mut counting(100))
    }

    fn parse(text: &str) -> Result<Projects, String> {
        let alone = |dir: &Dir| Ok(Some(Spot::in_folder(dir.clone(), None)));
        rooted_as(text, alone).map(|read| read.projects)
    }

    #[test]
    fn the_file_says_its_version_and_only_what_differs_from_the_defaults() {
        let mut projects = Projects::default();
        let ids = [1, 2, 3, 4, 5, 6, 7].map(|id| Id::of(id).unwrap());
        let mut drawn = ids.into_iter();
        let mut draw = || Ok(drawn.next().unwrap());
        let change = Change::AddProject {
            spot: clone_of("/w/lune", &["top"]),
            name: None,
        };
        (projects, _) = projects.changed(change, None, &mut draw).unwrap();
        let project = ProjectId::new(ids[0]);
        let top = add_in(project, spot_at("/w/lune", "top"));
        (projects, _) = projects.changed(top, None, &mut draw).unwrap();
        let named = Some(Name::parse("release").unwrap());
        let main = add_named(project, spot_at("/w/lune", "main"), named);
        (projects, _) = projects.changed(main, None, &mut draw).unwrap();
        let change = Change::AddProject {
            spot: alone("/home/me/Vault"),
            name: None,
        };
        (projects, _) = projects.changed(change, None, &mut draw).unwrap();
        let want = r#"{
  "version": 13,
  "projects": [
    {
      "id": "0000000000000001",
      "name": "lune",
      "dir": "/w/lune",
      "holds": {
        "clone": [
          {
            "id": "0000000000000002",
            "worktree": "top",
            "layout": {
              "shown": {
                "tabs": {}
              },
              "tree": [
                {
                  "terminal": {
                    "id": "0000000000000003",
                    "folder": "/w/lune/top",
                    "runs": "shell"
                  }
                }
              ]
            }
          },
          {
            "id": "0000000000000004",
            "name": "release",
            "worktree": "main",
            "layout": {
              "shown": {
                "tabs": {}
              },
              "tree": [
                {
                  "terminal": {
                    "id": "0000000000000005",
                    "folder": "/w/lune/main",
                    "runs": "shell"
                  }
                }
              ]
            }
          }
        ]
      }
    },
    {
      "id": "0000000000000006",
      "name": "Vault",
      "dir": "/home/me/Vault",
      "holds": {
        "folder": {
          "id": "0000000000000007"
        }
      }
    }
  ]
}
"#;
        assert_eq!(written(&projects), want);
        assert_eq!(parse(want), Ok(projects.clone()));
        assert_eq!(
            parse(&want.replace(r#""version": 13"#, r#""version": 12"#)),
            Ok(projects.clone())
        );
        assert_eq!(
            parse(&want.replace(r#""version": 13"#, r#""version": 11"#)),
            Ok(projects.clone())
        );
        let tabbed = "\n              \"shown\": {\n                \"tabs\": {}\n              },";
        let older = want.replace(tabbed, "");
        assert!(parse(&older).unwrap_err().contains("missing field `shown`"));
        for before in [7, 8, 9, 10] {
            let before = older.replace(r#""version": 13"#, &format!(r#""version": {before}"#));
            assert_eq!(parse(&before), Ok(projects.clone()));
        }
        let shown_before = want.replace(r#""version": 13"#, r#""version": 10"#);
        assert!(
            parse(&shown_before)
                .unwrap_err()
                .contains("unknown field `shown`")
        );
        let viewed = older.replace(r#""version": 13"#, r#""version": 6"#);
        assert_eq!(
            parse(&viewed.replace(r#""layout": {"#, r#""view": {"#)),
            Ok(projects.clone())
        );
        assert!(
            parse(&viewed)
                .unwrap_err()
                .contains("unknown field `layout`")
        );
        let before = want.replace(r#""version": 13"#, r#""version": 5"#);
        assert!(
            parse(&before)
                .unwrap_err()
                .contains("unknown field `holds`")
        );

        let unopened = r#""clone": [
          {
            "id": "0000000000000008",
            "worktree": "old"
          },"#;
        let with_unopened = older.replace(r#""clone": ["#, unopened);
        let alone = |dir: &Dir| Ok(Some(Spot::in_folder(dir.clone(), None)));
        for before in [7, 8] {
            let version = format!(r#""version": {before}"#);
            let before = with_unopened.replace(r#""version": 13"#, &version);
            let read = rooted_as(&before, alone).unwrap();
            assert_eq!(
                read.projects, projects,
                "a worktree an old file kept with no view goes"
            );
            assert!(read.migrated, "and the file is written again at once");
        }
        let refused = parse(&want.replace(r#""clone": ["#, unopened)).unwrap_err();
        assert!(
            refused.contains("holds no view, as no clone's may"),
            "{refused}"
        );
    }

    #[test]
    fn a_layout_is_kept_as_its_terminals_in_preorder_with_what_each_runs() {
        let mut projects = Projects::default();
        let ids = [1, 2, 3].map(|id| Id::of(id).unwrap());
        let mut drawn = ids.into_iter();
        let mut draw = || Ok(drawn.next().unwrap());
        let change = Change::AddProject {
            spot: clone_of("/w/lune", &["top"]),
            name: None,
        };
        (projects, _) = projects.changed(change, None, &mut draw).unwrap();
        let top = add_in(ProjectId::new(ids[0]), spot_at("/w/lune", "top"));
        (projects, _) = projects.changed(top, None, &mut draw).unwrap();
        let worktree = WorktreeId::new(ids[1]);
        let size = super::super::Size::of(80, 24).unwrap();
        let [shell, vim, agent] = [3, 5, 6].map(|id| ViewId::of(id).unwrap());
        let runs = [
            (
                vim,
                Runs::Command(Line::parse("vim notes").unwrap()),
                Placed::Beside {
                    at: shell,
                    towards: Towards::Right,
                },
            ),
            (
                agent,
                Runs::Agent {
                    harness: Harness::ClaudeCode,
                    session: Session::parse("7c22-ab").unwrap(),
                },
                Placed::Beside {
                    at: vim,
                    towards: Towards::Down,
                },
            ),
        ];
        for (id, runs, placed) in runs {
            let terminal = Terminal::new(TerminalViewId::new(id), dir("/w/lune/top"), runs);
            let open = Change::Open {
                worktree,
                view: View::Terminal(terminal),
                placed,
                size,
            };
            (projects, _) = projects.changed(open, None, &mut draw).unwrap();
        }
        projects = projects.relaid(Relaid::Focus { view: shell }).unwrap();
        let panes = Change::Arrange {
            worktree,
            arranged: Arranged::Panes,
        };
        (projects, _) = projects.changed(panes, None, &mut draw).unwrap();
        projects = projects
            .relaid(Relaid::Fill {
                worktree,
                full: Some(vim),
            })
            .unwrap();
        let want = r#"{
  "version": 13,
  "projects": [
    {
      "id": "0000000000000001",
      "name": "lune",
      "dir": "/w/lune",
      "holds": {
        "clone": [
          {
            "id": "0000000000000002",
            "worktree": "top",
            "layout": {
              "shown": {
                "panes": {
                  "focus": {
                    "full": "0000000000000005"
                  }
                }
              },
              "tree": [
                {
                  "split": {
                    "towards": "right",
                    "share": 500000
                  }
                },
                {
                  "terminal": {
                    "id": "0000000000000003",
                    "folder": "/w/lune/top",
                    "runs": "shell"
                  }
                },
                {
                  "split": {
                    "towards": "down",
                    "share": 500000
                  }
                },
                {
                  "terminal": {
                    "id": "0000000000000005",
                    "folder": "/w/lune/top",
                    "runs": {
                      "command": "vim notes"
                    }
                  }
                },
                {
                  "terminal": {
                    "id": "0000000000000006",
                    "folder": "/w/lune/top",
                    "runs": {
                      "agent": {
                        "harness": "claude-code",
                        "session": "7c22-ab"
                      }
                    }
                  }
                }
              ]
            }
          }
        ]
      }
    }
  ]
}
"#;
        assert_eq!(written(&projects), want);
        assert_eq!(parse(want), Ok(projects.clone()));
        assert_eq!(
            parse(&want.replace(r#""version": 13"#, r#""version": 12"#)),
            Ok(projects.clone())
        );
        assert_eq!(
            parse(&want.replace(r#""version": 13"#, r#""version": 11"#)),
            Ok(projects.clone())
        );
        let (_, top) = projects.find(worktree).unwrap();
        assert_eq!(
            top.layout().last_focus(),
            Some(vim),
            "a full view is the last focus"
        );
        let tabs = Change::Arrange {
            worktree,
            arranged: Arranged::Tabs,
        };
        let as_tabs = projects
            .relaid(Relaid::Fill {
                worktree,
                full: None,
            })
            .unwrap();
        let (as_tabs, _) = as_tabs.changed(tabs, None, &mut draw).unwrap();
        let (_, top) = as_tabs.find(worktree).unwrap();
        assert_eq!(
            top.layout().last_focus(),
            Some(vim),
            "the view filled is the tab focused"
        );
        let vim_full = r#""full": "0000000000000005""#;
        let shown = [
            r#""shown": {"#,
            r#"                "panes": {"#,
            r#"                  "focus": {"#,
            r#"                    "full": "0000000000000005""#,
            "                  }",
            "                }",
            "              },",
        ]
        .join("\n");
        let older = want.replace(&shown, &format!("{vim_full},"));
        for before in [7, 8, 9, 10] {
            let version = format!(r#""version": {before}"#);
            let older = older.replace(r#""version": 13"#, &version);
            assert_eq!(
                parse(&older),
                Ok(as_tabs.clone()),
                "an older file shows tabs"
            );
            let refused = parse(&want.replace(r#""version": 13"#, &version)).unwrap_err();
            assert!(
                refused.contains("unknown field `shown`"),
                "an older file shows no panes"
            );
        }
        assert!(parse(&older).unwrap_err().contains("unknown field `full`"));
        let shell_last = r#""last": "0000000000000003""#;
        let tiled = projects.relaid(Relaid::Focus { view: shell }).unwrap();
        let on_shell = want.replace(vim_full, shell_last);
        assert_eq!(
            written(&tiled),
            on_shell,
            "a focus on another view tiles the layout"
        );
        assert_eq!(parse(&on_shell), Ok(tiled));
        let astray = r#""last": "0000000000000009""#;
        let refused = parse(&want.replace(vim_full, astray)).unwrap_err();
        assert!(
            refused.contains("holds no view 0000000000000009 to focus"),
            "{refused}"
        );
        let both = format!("{vim_full},\n                    {shell_last}");
        let both = want.replace(vim_full, &both);
        assert!(parse(&both).is_err(), "a focus is one view, full or last");
        let older_focus = r#""last_focus": "0000000000000003""#;
        let older_both = format!("{vim_full},\n              {older_focus}");
        let older_both = older.replace(vim_full, &older_both);
        let older_both = older_both.replace(r#""version": 13"#, r#""version": 10"#);
        let refused = parse(&older_both).unwrap_err();
        assert!(
            refused.contains("and so is focused on no other view"),
            "{refused}"
        );
        let older_on_shell = older.replace(vim_full, older_focus);
        for before in [7, 8, 9] {
            let version = format!(r#""version": {before}"#);
            let refused = format!("it is of version {before}, which keeps no last focus");
            assert_eq!(
                parse(&older_on_shell.replace(r#""version": 13"#, &version)),
                Err(refused)
            );
        }
        let viewed = older.replace(r#""version": 13"#, r#""version": 6"#);
        assert_eq!(
            parse(&viewed.replace(r#""layout": {"#, r#""view": {"#)),
            Ok(as_tabs)
        );
        assert!(
            parse(&viewed)
                .unwrap_err()
                .contains("unknown field `layout`")
        );
        let relaid = want.replace(r#""layout": {"#, r#""view": {"#);
        assert!(parse(&relaid).unwrap_err().contains("unknown field `view`"));
        let before = r#"{"version": 1, "projects": [{"id": "0000000000000001", "name": "lune",
            "dir": "/w/lune", "workstreams": [{"id": "0000000000000002", "below": "top",
            "view": {"tree": [{"terminal": {"id": "0000000000000004", "folder": "/w/lune/top",
            "runs": "shell"}}]}}]}]}"#;
        assert_eq!(
            parse(before),
            Err("it is of version 1, which holds no layouts".to_owned())
        );
    }

    #[test]
    fn a_welcome_view_is_kept_by_its_id_alone_and_only_from_version_8() {
        let text = |version: u32| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "holds": {{"clone": [{{"id": "0000000000000002", "worktree": "b",
                "layout": {{"tree": [{{"split": {{"towards": "right", "share": 500000}}}},
                {{"terminal": {{"id": "0000000000000003", "folder": "/a/b", "runs": "shell"}}}},
                {{"welcome": {{"id": "0000000000000004"}}}}]}}}}]}}}}]}}"#
            )
        };
        let projects = parse(&text(8)).unwrap();
        let (_, worktree) = projects.find(WorktreeId::of(2).unwrap()).unwrap();
        let views: Vec<&View> = worktree.layout().views().collect();
        assert!(matches!(views[..], [View::Terminal(_), View::Welcome(id)] if id.get() == 4));
        assert_eq!(parse(&written(&projects)), Ok(projects.clone()));
        assert!(written(&projects).contains(r#""welcome": {"#));
        let refused = "it is of version 7, which holds no welcome";
        assert_eq!(parse(&text(7)), Err(refused.to_owned()));
    }

    #[test]
    fn a_review_view_is_kept_by_its_id_alone_and_only_from_version_12() {
        let text = |version: u32| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "holds": {{"clone": [{{"id": "0000000000000002", "worktree": "b",
                "layout": {{"shown": {{"tabs": {{}}}}, "tree": [
                {{"split": {{"towards": "right", "share": 500000}}}},
                {{"terminal": {{"id": "0000000000000003", "folder": "/a/b", "runs": "shell"}}}},
                {{"review": {{"id": "0000000000000004"}}}}]}}}}]}}}}]}}"#
            )
        };
        let projects = parse(&text(12)).unwrap();
        let (_, worktree) = projects.find(WorktreeId::of(2).unwrap()).unwrap();
        let views: Vec<&View> = worktree.layout().views().collect();
        assert!(matches!(views[..], [View::Terminal(_), View::Review(id)] if id.get() == 4));
        assert_eq!(parse(&written(&projects)), Ok(projects.clone()));
        assert!(written(&projects).contains(r#""review": {"#));
        let refused = "it is of version 11, which holds no review";
        assert_eq!(parse(&text(11)), Err(refused.to_owned()));
    }

    #[test]
    fn a_browser_view_is_kept_with_its_url_and_only_from_version_13() {
        let text = |version: u32| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "holds": {{"clone": [{{"id": "0000000000000002", "worktree": "b",
                "layout": {{"shown": {{"tabs": {{}}}}, "tree": [
                {{"split": {{"towards": "right", "share": 500000}}}},
                {{"browser": {{"id": "0000000000000003", "url": "https://example.com/pr/1"}}}},
                {{"browser": {{"id": "0000000000000004"}}}}]}}}}]}}}}]}}"#
            )
        };
        let projects = parse(&text(13)).unwrap();
        let (_, worktree) = projects.find(WorktreeId::of(2).unwrap()).unwrap();
        let views: Vec<&View> = worktree.layout().views().collect();
        let expected_url = BrowserUrl::parse("https://example.com/pr/1").unwrap();
        assert_eq!(
            views,
            [
                &View::Browser(Browser::new(ViewId::of(3).unwrap(), Some(expected_url))),
                &View::Browser(Browser::new(ViewId::of(4).unwrap(), None)),
            ]
        );
        let serialized = written(&projects);
        assert_eq!(parse(&serialized), Ok(projects.clone()));
        assert!(serialized.contains(r#""browser": {"#));
        assert!(serialized.contains(r#""url": "https://example.com/pr/1""#));
        let refused = "it is of version 12, which holds no browser";
        assert_eq!(parse(&text(12)), Err(refused.to_owned()));
        let about_blank = text(13).replace("https://example.com/pr/1", "about:blank");
        assert!(
            parse(&about_blank).is_err(),
            "about:blank is not stored; Start Page is omitted"
        );
    }

    #[test]
    fn an_old_file_s_folder_holds_its_worktrees_as_one_in_its_own_folder() {
        let text = r#"{"version": 4, "projects": [{"id": "0000000000000001", "name": "a",
            "dir": "/a", "workstreams": [{"id": "0000000000000002"},
            {"id": "0000000000000003", "below": "b", "used_millis": 7}]}, {"id": "0000000000000004",
            "name": "c", "dir": "/c"}]}"#;
        let alone = |dir: &Dir| Ok(Some(Spot::in_folder(dir.clone(), None)));
        let read = rooted_as(text, alone).unwrap();
        assert!(
            read.migrated,
            "an old file is rooted, and so written again at once"
        );
        let kept = read.projects;
        let [a, c] = kept.all() else {
            panic!("both projects are kept")
        };
        assert_eq!((is_clone(a), a.dir()), (false, &dir("/a")));
        let ids: Vec<u64> = a.worktrees().map(|it| it.id().get()).collect();
        assert_eq!(ids, [3], "the one used last stands for both");
        assert_eq!(c.worktrees().count(), 1, "a folder with none is given one");
        assert_eq!(parse(&written(&kept)), Ok(kept));
    }

    #[test]
    fn an_old_file_s_projects_of_one_clone_are_one_at_its_root_holding_every_worktree() {
        let terminal = |id: u64, folder: &str| {
            let at = format!(r#""id": "{id:016x}", "folder": "{folder}""#);
            format!(r#"{{"terminal": {{{at}, "runs": "shell"}}}}"#)
        };
        let view = |id: u64, at: &str| format!(r#""view": {{"tree": [{}]}}"#, terminal(id, at));
        let text = format!(
            r#"{{"version": 5, "projects": [
            {{"id": "0000000000000001", "name": "main", "dir": "/w/lune/main", "workstreams": [
              {{"id": "0000000000000002", {}, "used_millis": 5}},
              {{"id": "0000000000000003", "name": "kept", {}, "used_millis": 9}}]}},
            {{"id": "0000000000000004", "name": "feat", "dir": "/w/lune/feat", "workstreams": [
              {{"id": "0000000000000005", "below": "crates", {}}}]}},
            {{"id": "0000000000000006", "name": "Vault", "dir": "/home/me/Vault", "workstreams": [
              {{"id": "0000000000000007"}}, {{"id": "0000000000000008", "below": "notes"}}]}},
            {{"id": "0000000000000009", "name": "x", "dir": "/w/lune/team", "workstreams": [
              {{"id": "000000000000000a", "below": "x/src", {}}}]}}]}}"#,
            view(16, "/w/lune/main"),
            view(17, "/w/lune/main/src"),
            view(18, "/w/lune/feat/crates"),
            view(19, "/w/lune/team/x/src"),
        );
        let clone = clone_of("/w/lune", &["feat", "fix", "main", "team/x"]);
        let root = |at: &Dir| {
            let held = at.as_str().starts_with("/w/lune");
            if held {
                Ok(Some(clone.clone()))
            } else {
                Ok(None)
            }
        };
        let read = rooted_as(&text, root).unwrap();
        assert!(read.migrated);
        let [lune, vault] = read.projects.all() else {
            panic!("one clone, one folder")
        };
        let held = (
            lune.id().get(),
            lune.name().as_str(),
            lune.dir(),
            is_clone(lune),
        );
        let named = "the first keeps its id, and its folder's name becomes its clone's";
        assert_eq!(held, (1, "lune", &dir("/w/lune"), true), "{named}");
        let (first, main) = &rows(lune)[0];
        assert_eq!(
            (main.id().get(), first.as_str(), main.layout().len()),
            (3, "main", 2)
        );
        let renamed = main.renamed().map(Name::as_str);
        assert_eq!(renamed, Some("kept"), "the one used last stands for both");
        let feat = row(lune, "feat");
        assert_eq!(
            (feat.id().get(), feat.layout().len()),
            (5, 1),
            "a folder below a worktree is it"
        );
        let nested = row(lune, "team/x");
        let held = (nested.id().get(), nested.layout().len());
        assert_eq!(
            held,
            (10, 1),
            "a folder in a nested worktree is that worktree's"
        );
        let unopened = rows(lune).iter().find(|(it, _)| *it == tree("fix"));
        assert!(
            unopened.is_none(),
            "a worktree listed with no view in it is opened by hand"
        );
        assert_eq!(rows(lune).len(), 3);
        let held = (is_clone(vault), vault.worktrees().count());
        assert_eq!(held, (false, 1), "a folder gone stays");
        let again = parse(&written(&read.projects)).unwrap();
        assert_eq!(again, read.projects);
    }

    #[test]
    fn an_old_file_is_rooted_only_where_git_says_whose_clone_each_project_is() {
        let text = r#"{"version": 5, "projects": [{"id": "0000000000000001", "name": "a",
            "dir": "/a"}]}"#;
        let stuck = |_: &Dir| Err("git did not answer in 2000ms".to_owned());
        let Err(refused) = rooted_as(text, stuck) else {
            panic!("a file unrooted is refused")
        };
        let unsaid = "project a in /a: git did not say whose clone it is, so none is rooted";
        assert!(refused.starts_with(unsaid), "{refused}");
        assert!(
            refused.ends_with("git did not answer in 2000ms"),
            "{refused}"
        );

        let text = r#"{"version": 5, "projects": [{"id": "0000000000000001", "name": "bare",
            "dir": "/w/bare", "workstreams": [{"id": "0000000000000002"}]}]}"#;
        let read = rooted_as(text, |_: &Dir| Ok(Some(clone_of("/w/bare", &[])))).unwrap();
        let [bare] = read.projects.all() else {
            panic!("the clone is kept")
        };
        let held = (is_clone(bare), bare.worktrees().count());
        assert_eq!(
            held,
            (true, 0),
            "a clone with no worktree and no terminal stays one"
        );
    }

    #[test]
    fn an_old_file_over_the_limits_is_refused_before_git_is_asked_of_it() {
        let project =
            |at: usize| format!(r#"{{"id": "{at:016x}", "name": "p{at}", "dir": "/p{at}"}}"#);
        let many: Vec<String> = (1..=PROJECTS_MAX + 1).map(project).collect();
        let text = format!(r#"{{"version": 5, "projects": [{}]}}"#, many.join(", "));
        let mut asked = 0;
        let counted = |_: &Dir| {
            asked += 1;
            Ok(None)
        };
        let Err(refused) = rooted_as(&text, counted) else {
            panic!("a file over is refused")
        };
        assert_eq!(refused, format!("it holds over {PROJECTS_MAX} projects"));
        assert_eq!(asked, 0, "no git is asked of a file refused");
    }

    #[test]
    fn a_welcome_view_counts_toward_the_views_a_file_may_hold() {
        let welcome = |at: usize| format!(r#"{{"welcome": {{"id": "{:016x}"}}}}"#, at + 3);
        let many: Vec<String> = (0..=VIEWS_MAX).map(welcome).collect();
        let text = format!(
            r#"{{"version": 8, "projects": [{{"id": "0000000000000001", "name": "a", "dir": "/a",
            "holds": {{"clone": [{{"id": "0000000000000002", "worktree": "b",
            "layout": {{"tree": [{}]}}}}]}}}}]}}"#,
            many.join(", ")
        );
        assert_eq!(
            parse(&text),
            Err(format!("it holds over {VIEWS_MAX} views"))
        );
    }

    #[test]
    fn a_line_left_unrun_is_kept_apart_from_a_command_and_only_from_version_3() {
        let text = |version: u32, runs: &str| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "workstreams": [{{"id": "0000000000000002", "below": "b",
                "view": {{"tree": [
                {{"terminal": {{"id": "0000000000000003", "folder": "/a", "runs": {runs}}}}}]}}}}]}}]}}"#
            )
        };
        let unrun = parse(&text(3, r#"{"unrun": "make test"}"#)).unwrap();
        let (_, held) = unrun.holding_terminal(ViewId::of(3).unwrap()).unwrap();
        assert_eq!(held.runs(), &Runs::Unrun(Line::parse("make test").unwrap()));
        assert!(
            written(&unrun).contains(r#""unrun": "make test""#),
            "{}",
            written(&unrun)
        );
        assert_eq!(parse(&written(&unrun)), Ok(unrun.clone()));
        let command = parse(&text(2, r#"{"command": "make test"}"#)).unwrap();
        assert_ne!(command, unrun, "a command run is not a line left unrun");
        let refused = parse(&text(2, r#"{"unrun": "make test"}"#));
        assert_eq!(
            refused,
            Err("it is of version 2, which holds no line left unrun".to_owned())
        );
    }

    #[test]
    fn a_terminal_s_name_and_a_worktree_s_use_are_kept_only_from_version_4() {
        let text = |version: u32, name: &str, used: &str| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "workstreams": [{{"id": "0000000000000002", "below": "b",
                "view": {{"tree": [
                {{"terminal": {{"id": "0000000000000003", {name} "folder": "/a", "runs": "shell"}}}}
                ]}} {used}}}]}}]}}"#
            )
        };
        let (name, used) = (r#""name": "tests","#, r#", "used_millis": 1763460000123"#);
        let kept = parse(&text(4, name, used)).unwrap();
        let (worktree, held) = kept.holding_terminal(ViewId::of(3).unwrap()).unwrap();
        assert_eq!(held.name().map(Name::as_str), Some("tests"));
        assert_eq!(worktree.used(), Some(Used::of_millis(1_763_460_000_123)));
        let again = written(&kept);
        assert!(again.contains(r#""name": "tests""#));
        assert!(again.contains(r#""used_millis": 1763460000123"#));
        assert_eq!(parse(&again), Ok(kept));
        let refusals = [(name, "", "terminal's name"), ("", used, "worktree's use")];
        for (name, used, without) in refusals {
            let refused = parse(&text(3, name, used));
            assert_eq!(
                refused,
                Err(format!("it is of version 3, which holds no {without}"))
            );
        }
        assert!(
            parse(&text(4, r#""name": "","#, ""))
                .unwrap_err()
                .contains("a name is empty")
        );
        let past = format!(r#", "used_millis": {}"#, u64::MAX);
        let refused = parse(&text(4, "", &past)).unwrap_err();
        assert!(
            refused.contains("worktree 0000000000000002: a use at"),
            "{refused}"
        );
    }
}
