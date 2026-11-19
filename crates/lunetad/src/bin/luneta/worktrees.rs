use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorktree, Change, CloseProject};
use lunetad::api::change_projects_request::{CloseView, CloseWorktree, Pull, Start};
use lunetad::api::change_projects_request::RenameProject;
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorktrees};
use lunetad::api::change_projects_request::{Bring, Join, RenameTerminal};
use lunetad::api::change_projects_response::Made;
use lunetad::api::ChangeProjectsResponse;
use lunetad::api::make_worktree_response::Landed as Answered;
use lunetad::api::projects::Held;
use lunetad::api::{self, ChangeProjectsRequest, ListWorkRequest, MakeWorktreeRequest, On};
use lunetad::api::{Version, WatchRequest};
use lunetad::changes::{Unadded, Unchanged};
use lunetad::mux::own::TerminalKey;
use lunetad::projects::{self, Name, Plain, ProjectId, Size, ViewId, WorktreeId};
use lunetad::links::{self, Branch, Unready};
use lunetad::work::{self, Listed};
use lunetad::{Asked, Beside, Daemon, Heard};
use tonic::{Code, Status};

use lunetad::Directory;

use crate::daemon;
use crate::machines::Machine;
use crate::projected::{self, Holds, Project};
use crate::sidebar::Reorder;

const ASKING: Duration = Duration::from_secs(15);

const WORK_PATIENCE: Duration = work::ANSWERED_MAX.saturating_add(ASKING);

fn said_unmade(unchanged: Unchanged, what: &str) -> String {
    unchanged.said(&format!("no {what} made"), &format!("the {what} may be made"))
}

fn said_unopened(unchanged: Unchanged, folder: &Directory, tab: &str) -> String {
    match unchanged {
        Unchanged::Refused(why) => unopened(folder, tab, &why),
        Unchanged::InDoubt(why) => format!("{why}, so {folder}, {tab}'s worktree, may be opened"),
    }
}

enum Tried {
    Went(String),
    Failed(String),
}

impl From<String> for Tried {
    fn from(why: String) -> Tried {
        Tried::Failed(why)
    }
}

impl Tried {
    fn said(self) -> String {
        match self {
            Tried::Went(why) | Tried::Failed(why) => why,
        }
    }
}

fn twice<T>(mut tried: impl FnMut() -> Result<T, Tried>) -> Result<T, String> {
    match tried() {
        Err(Tried::Went(_)) => tried().map_err(Tried::said),
        done => done.map_err(Tried::said),
    }
}

fn in_kept<T>(path: &Path, project: ProjectId, made: Result<T, String>) -> Result<T, Tried> {
    let why = match made {
        Ok(made) => return Ok(made),
        Err(why) => why,
    };
    let went = kept(path)?.iter().all(|it| it.id != project);
    Err(if went { Tried::Went(why) } else { Tried::Failed(why) })
}

fn asked<T, E: From<String>>(
    path: &Path,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, E>,
) -> Result<T, E> {
    asked_or(path, ASKING, asking, E::from).unwrap_or_else(|unasked| Err(E::from(unasked)))
}

fn asked_or<T, E>(
    path: &Path,
    within: Duration,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, E>,
    late: impl FnOnce(String) -> E,
) -> Result<Result<T, E>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))?;
    runtime.block_on(async {
        let mut daemon = daemon::current(path).await?.daemon;
        Ok(answered_within(within, asking(&mut daemon), late).await)
    })
}

async fn answered_within<T, E>(
    within: Duration,
    answer: impl Future<Output = Result<T, E>>,
    late: impl FnOnce(String) -> E,
) -> Result<T, E> {
    let answered = tokio::time::timeout(within, answer).await;
    answered.map_err(|_| late(lunetad::unanswered(within)))?
}

pub fn kept(path: &Path) -> Result<Vec<Project>, String> {
    projected::listed(&kept_as_said(path)?)
}

pub fn plain(path: &Path) -> Result<Vec<Plain>, String> {
    Plain::of_api(&kept_as_said(path)?)
}

fn kept_as_said(path: &Path) -> Result<Vec<api::Project>, String> {
    asked(path, async |daemon| {
        let request = WatchRequest { after: None, kept: None, shown: false };
        let host = daemon.watch(request).await.map_err(|status| status.message().to_owned())?;
        match host.into_inner().projects.and_then(|projects| projects.held) {
            Some(Held::Kept(kept)) => Ok(kept.projects),
            Some(Held::Unkept(why)) => Err(format!("lunetad keeps no projects: {why}")),
            None => Err("lunetad is older than its projects".to_owned()),
        }
    })
}

fn change_answered<E: From<Unchanged>>(
    path: &Path,
    on: Option<On>,
    change: Change,
    read: fn(&Status) -> E,
) -> Result<ChangeProjectsResponse, E> {
    let request = ChangeProjectsRequest { on, change: Some(change) };
    let asking = async |daemon: &mut Daemon| match daemon.change_projects(request).await {
        Ok(answered) => Ok(answered.into_inner()),
        Err(status) => Err(read(&status)),
    };
    let late = |late| E::from(Unchanged::InDoubt(late));
    let answered = asked_or(path, ASKING, asking, late);
    answered.unwrap_or_else(|unasked| Err(E::from(Unchanged::Refused(unasked))))
}

fn changed_on(path: &Path, on: Option<On>, change: Change) -> Result<Option<Made>, Unchanged> {
    change_answered(path, on, change, Unchanged::of).map(|answered| answered.made)
}

fn add_project(path: &Path, dir: &str, name: Option<&str>) -> Result<ProjectId, Unadded> {
    let (dir, name) = (dir.to_owned(), name.map(str::to_owned));
    let change = Change::AddProject(AddProject { dir, name });
    let answered = change_answered(path, None, change, Unadded::of)?;
    project_of(answered.made).map_err(Unadded::from)
}

fn project_of(made: Option<Made>) -> Result<ProjectId, Unchanged> {
    match made {
        Some(Made::Project(made)) => {
            ProjectId::of(made).ok_or_else(|| Unchanged::InDoubt(zero("project")))
        },
        made => Err(unread(made, "project")),
    }
}

fn zero(made: &str) -> String {
    format!("lunetad said it made {made} 0, and no id is 0")
}

fn unread(answer: Option<Made>, wanted: &str) -> Unchanged {
    Unchanged::InDoubt(format!("luneta can't read lunetad's answer {answer:?} as a {wanted}"))
}

fn landed(made: Option<Made>) -> Result<Landed, Unchanged> {
    let id = |id| WorktreeId::of(id).ok_or_else(|| Unchanged::InDoubt(zero("worktree")));
    match made {
        Some(Made::Worktree(made)) => id(made).map(Landed::Made),
        Some(Made::Found(found)) => id(found).map(Landed::Found),
        made => Err(unread(made, "worktree")),
    }
}

pub fn add_worktree(
    path: &Path,
    machine: &Machine,
    (project, folder): (ProjectId, &str),
) -> Result<Landed, String> {
    let (project, folder) = (project.get(), folder.to_owned());
    let change = Change::AddWorktree(AddWorktree { project, folder, name: None, argv: Vec::new() });
    let added = changed_on(path, on(machine), change).and_then(landed);
    added.map_err(|unchanged| said_unmade(unchanged, "worktree"))
}

pub fn branched(
    path: &Path,
    machine: &Machine,
    project: ProjectId,
    branch: &Branch,
) -> Result<Landed, String> {
    let (on, project, branch) = (on(machine), project.get(), branch.as_str().to_owned());
    let request = MakeWorktreeRequest { on, project, branch };
    let asking = async |daemon: &mut Daemon| {
        let unready = |status: Status| unbranched(&status);
        let mut answer = daemon.make_worktree(request).await.map_err(unready)?.into_inner();
        let answered = answer.message().await.map_err(unready)?;
        let unsaid = || Unready::InDoubt("lunetad ended its answer with no worktree".to_owned());
        let id = |id| WorktreeId::of(id).ok_or_else(|| Unready::InDoubt(zero("worktree")));
        match answered.and_then(|answered| answered.landed).ok_or_else(unsaid)? {
            Answered::Made(made) => id(made).map(Landed::Made),
            Answered::Found(found) => id(found).map(Landed::Found),
        }
    };
    let late = |late: String| Unready::InDoubt(format!("{late}, so the worktree may be made"));
    let made = asked_or(path, links::WORKTREE_PATIENCE, asking, late);
    let made = made.unwrap_or_else(|unasked| Err(Unready::Failed(unasked)));
    made.map_err(|unready| match unready {
        Unready::Nowhere(why) | Unready::Failed(why) => format!("no worktree made: {why}"),
        Unready::InDoubt(why) => why,
    })
}

pub fn work(path: &Path, machine: &Machine, project: ProjectId) -> Result<Listed, String> {
    let request = ListWorkRequest { on: on(machine), project: project.get() };
    let asking = async |daemon: &mut Daemon| {
        let said = |status: Status| status.message().to_owned();
        let mut answer = daemon.list_work(request).await.map_err(said)?.into_inner();
        let listed = answer.message().await.map_err(said)?;
        let listed = listed.ok_or("lunetad ended its answer with no list")?;
        Listed::of(listed)
    };
    asked_or(path, WORK_PATIENCE, asking, |late| late).unwrap_or_else(Err)
}

fn unbranched(status: &Status) -> Unready {
    let why = status.message().to_owned();
    match status.code() {
        Code::NotFound => Unready::Nowhere(why),
        Code::InvalidArgument | Code::FailedPrecondition | Code::Unimplemented => {
            Unready::Failed(why)
        },
        _ => Unready::InDoubt(why),
    }
}

fn started_at(path: &Path, folder: &str) -> Result<Option<Landed>, String> {
    let start = Start { folder: folder.to_owned(), name: None };
    let started = changed_on(path, None, Change::Start(start)).and_then(|made| match made {
        Some(Made::Project(_)) => Ok(None),
        made => landed(made).map(Some),
    });
    started.map_err(|unchanged| said_unmade(unchanged, "worktree"))
}

pub fn closed(
    path: &Path,
    machine: &Machine,
    worktree: WorktreeId,
    seen: &[ViewId],
) -> Result<(), Unchanged> {
    changed_on(path, on(machine), close(worktree, seen)).map(drop)
}

pub fn closed_view(
    path: &Path,
    machine: &Machine,
    terminal: TerminalKey,
) -> Result<(), Unchanged> {
    let (id, made_at) = (terminal.id.get(), Some(terminal.made.into()));
    let terminal = Some(lunetad::api::TerminalKey { id, made_at });
    let change = Change::CloseView(CloseView { terminal });
    changed_on(path, on(machine), change).map(drop)
}

fn close(worktree: WorktreeId, seen: &[ViewId]) -> Change {
    let (worktree, seen) = (worktree.get(), seen.iter().map(|it| it.get()).collect());
    Change::CloseWorktree(CloseWorktree { worktree, seen })
}

pub fn closed_project(
    path: &Path,
    machine: &Machine,
    project: ProjectId,
    worktrees: &[WorktreeId],
    seen: &[ViewId],
) -> Result<(), Unchanged> {
    let project = project.get();
    let worktrees = worktrees.iter().map(|it| it.get()).collect();
    let seen = seen.iter().map(|it| it.get()).collect();
    let change = Change::CloseProject(CloseProject { project, worktrees, seen });
    changed_on(path, on(machine), change).map(drop)
}

fn on(machine: &Machine) -> Option<On> {
    match machine {
        Machine::Local => None,
        Machine::Remote(destination) => Some(On { destination: destination.to_string() }),
    }
}

pub fn reordered(
    path: &Path,
    machine: &Machine,
    order: &Reorder,
) -> Result<Option<Version>, Unchanged> {
    let change = match order {
        Reorder::Projects { seen, wanted } => {
            let ids = |ids: &[ProjectId]| ids.iter().map(|it| it.get()).collect();
            Change::ReorderProjects(ReorderProjects { projects: ids(wanted), seen: ids(seen) })
        },
        Reorder::Worktrees { project, seen, wanted } => {
            let ids = |ids: &[WorktreeId]| ids.iter().map(|it| it.get()).collect();
            let (project, worktrees, seen) = (project.get(), ids(wanted), ids(seen));
            Change::ReorderWorktrees(ReorderWorktrees { project, worktrees, seen })
        },
    };
    change_answered(path, on(machine), change, Unchanged::of).map(|answered| answered.kept)
}

fn holds(dir: &str, folder: &Path) -> bool {
    folder.starts_with(dir)
}

fn at_folder(path: &Path, project: &Project, folder: &str) -> Result<Landing, Tried> {
    let dir = project.dir.as_str();
    assert!(holds(dir, Path::new(folder)), "{folder} is in {dir}");
    let started = in_kept(path, project.id, started_at(path, folder))?;
    Ok(started.map_or(Landing::Unopened(project.id), Landing::Worktree))
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<Arrival, Tried>> {
    if let Some(name) = name {
        let named: Vec<&Project> = projects.iter().filter(|project| project.name == name).collect();
        match named[..] {
            [] => {},
            [one] => return Some(Ok(Arrival::Home(one.id))),
            [..] => {
                let dirs: Vec<&str> = named.iter().map(|project| project.dir.as_str()).collect();
                let dirs = dirs.join(", ");
                return Some(Err(Tried::Failed(format!(
                    "projects in {dirs} are all named {name}: start luneta in the one you mean"
                ))));
            },
        }
    }
    let holding = projects.iter().find(|project| holds(project.dir.as_str(), cwd))?;
    let landed = match name {
        Some(name) => branched_in(path, holding, name),
        None => at_folder(path, holding, folder),
    };
    Some(landed.map(Arrival::of))
}

fn branched_in(path: &Path, holding: &Project, name: &str) -> Result<Landing, Tried> {
    assert_ne!(holding.name, name, "a name a project has goes to that project");
    match holding.holds {
        Holds::Clone(_) => {},
        Holds::Folder(_) => {
            let one = projects::folder_has_one_worktree(&holding.name);
            return Err(Tried::Failed(format!("{name} names no project, and {one}")));
        },
    }
    let branch = Branch::parse(name).map_err(|why| format!("no worktree made: {why}"))?;
    let made = branched(path, &Machine::Local, holding.id, &branch);
    made.map(Landing::Worktree).map_err(Tried::Failed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    Worktree(WorktreeId),
    Home(ProjectId),
}

impl Arrival {
    fn of(landing: Landing) -> Arrival {
        match landing {
            Landing::Worktree(landed) => Arrival::Worktree(landed.worktree()),
            Landing::Unopened(project) => Arrival::Home(project),
        }
    }
}

pub fn start(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    seen: impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<Arrival>, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    let name = name.map(str::trim);
    twice(|| started(path, name, cwd, folder, &seen))
}

fn started(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
    seen: &impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<Arrival>, Tried> {
    let projects = kept(path)?;
    if let Some(chosen) = chosen(path, &projects, name, cwd, folder) {
        return chosen.map(Some);
    }
    let Some(name) = name else {
        let last = |project: &&Project| seen(project.id);
        let held = projects.iter().filter(|project| !project.worktrees().is_empty());
        let found = held.max_by_key(last).and_then(|it| projects::last_used(it.uses()));
        return Ok(found.map(Arrival::Worktree));
    };
    let unmade = match add_project(path, folder, Some(name)) {
        Ok(_) => None,
        Err(Unadded::Unchanged(refused @ Unchanged::Refused(_))) => {
            return Err(Tried::Failed(said_unmade(refused, "project")));
        },
        Err(Unadded::Nests(why)) => Some(said_unmade(Unchanged::Refused(why), "project")),
        Err(Unadded::Unchanged(in_doubt @ Unchanged::InDoubt(_))) => {
            Some(said_unmade(in_doubt, "project"))
        },
    };
    let unlisted = |unlisted: String| Tried::Failed(unmade.clone().unwrap_or(unlisted));
    let projects = kept(path).map_err(unlisted)?;
    let unkept = || format!("lunetad made the project of {}, and keeps it nowhere", cwd.display());
    let unfound = unmade.unwrap_or_else(unkept);
    let chosen = chosen(path, &projects, Some(name), cwd, folder);
    chosen.unwrap_or(Err(Tried::Failed(unfound))).map(Some)
}

pub fn made_at(
    path: &Path,
    machine: &Machine,
    cwd: &Directory,
    name: Option<&str>,
) -> Result<Landing, String> {
    let (folder, name) = (cwd.to_string(), name.map(str::to_owned));
    let start = Change::Start(Start { folder, name });
    let made = changed_on(path, on(machine), start).and_then(|made| match made {
        Some(Made::Project(_)) => project_of(made).map(Landing::Unopened),
        made => landed(made).map(Landing::Worktree),
    });
    made.map_err(|unchanged| said_unmade(unchanged, "worktree"))
}

pub fn pulled(
    path: &Path,
    machine: &Machine,
    cwd: &Directory,
    name: &str,
    argv: Vec<String>,
) -> Result<Landed, String> {
    let change = Change::Pull(Pull { folder: cwd.to_string(), name: name.to_owned(), argv });
    let pulled = changed_on(path, on(machine), change).and_then(landed);
    pulled.map_err(|unchanged| said_unopened(unchanged, cwd, name))
}

pub fn unopened(folder: &Directory, tab: &str, why: &str) -> String {
    format!("{folder} is {tab}'s worktree, and it is not opened: {why}")
}

pub fn real(cwd: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(cwd).map_err(|error| format!("{}: {error}", cwd.display()))
}

fn made_there(machine: &Machine, change: Change) -> Result<Option<Made>, Unchanged> {
    let unfound = |unfound| Unchanged::Refused(format!("{unfound}, to find lunetad in"));
    let path = lunetad::folder::find().map_err(unfound)?;
    changed_on(&path, on(machine), change)
}

fn changed_there(machine: &Machine, change: Change) -> Result<(), Unchanged> {
    made_there(machine, change).map(drop)
}

pub fn closed_there(
    machine: &Machine,
    worktree: WorktreeId,
    seen: &[ViewId],
) -> Result<(), Unchanged> {
    changed_there(machine, close(worktree, seen))
}

pub fn renamed_project(
    machine: &Machine,
    project: ProjectId,
    to: &Name,
) -> Result<(), Unchanged> {
    let (project, to) = (project.get(), spelled(to));
    changed_there(machine, Change::RenameProject(RenameProject { project, to }))
}

pub fn renamed_worktree(
    machine: &Machine,
    worktree: WorktreeId,
    to: Option<&Name>,
) -> Result<(), Unchanged> {
    let (worktree, to) = (worktree.get(), to.map(spelled));
    let renames = lunetad::api::change_projects_request::RenameWorktree { worktree, to };
    changed_there(machine, Change::RenameWorktree(renames))
}

pub fn renamed_terminal(
    machine: &Machine,
    terminal: ViewId,
    to: Option<&Name>,
) -> Result<(), Unchanged> {
    let (terminal, to) = (terminal.get(), to.map(spelled));
    changed_there(machine, Change::RenameTerminal(RenameTerminal { terminal, to }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landing {
    Worktree(Landed),
    Unopened(ProjectId),
}

pub fn project_made(
    machine: &Machine,
    folder: &str,
    name: Option<&Name>,
) -> Result<ProjectId, String> {
    let (dir, name) = (folder.to_owned(), name.map(spelled));
    let made = made_there(machine, Change::AddProject(AddProject { dir, name }));
    let project = made.and_then(project_of).map_err(|unchanged| said_unmade(unchanged, "project"))?;
    let start = Start { folder: folder.to_owned(), name: None };
    let started = made_there(machine, Change::Start(start)).and_then(|made| match made {
        Some(Made::Project(root)) if root == project.get() => Ok(project),
        made => landed(made).map(|_| project),
    });
    let unstarted = |unchanged: Unchanged| {
        format!("{folder} is project {project}: {}", said_unmade(unchanged, "worktree"))
    };
    started.map_err(unstarted)
}

fn spelled(name: &Name) -> String {
    name.as_str().to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Landed {
    Made(WorktreeId),
    Found(WorktreeId),
}

impl Landed {
    pub fn worktree(self) -> WorktreeId {
        match self {
            Landed::Found(worktree) | Landed::Made(worktree) => worktree,
        }
    }
}

pub fn worked_in(path: &Path, folder: &Path, argv: Vec<String>) -> Result<Landed, String> {
    let folder = folder.to_str().ok_or_else(|| format!("{} is not UTF-8", folder.display()))?;
    joined(path, folder, argv).map_err(|unchanged| match unchanged {
        Unchanged::Refused(why) => why,
        Unchanged::InDoubt(why) => format!("{why}, so lunetad may keep {folder} as a worktree"),
    })
}

pub fn joined_as(
    path: &Path,
    folder: &Directory,
    tab: &str,
    argv: Vec<String>,
) -> Result<Landed, String> {
    joined(path, folder.as_str(), argv).map_err(|unchanged| said_unopened(unchanged, folder, tab))
}

fn joined(path: &Path, folder: &str, argv: Vec<String>) -> Result<Landed, Unchanged> {
    let change = Change::Join(Join { folder: folder.to_owned(), argv });
    changed_on(path, None, change).and_then(landed)
}

pub fn dirs(path: &Path) -> Result<Vec<String>, String> {
    Ok(kept(path)?.into_iter().map(|project| project.dir.as_str().to_owned()).collect())
}

pub fn folder_of(path: &Path, worktree: WorktreeId) -> Result<PathBuf, String> {
    let projects = kept(path)?;
    let mut worktrees = projects.iter().flat_map(Project::worktrees);
    let found = worktrees.find(|it| it.id == worktree);
    let found = found.ok_or_else(|| format!("lunetad keeps no worktree {worktree}"))?;
    Ok(PathBuf::from(&found.folder))
}

pub enum Went {
    Taken,
    Elsewhere,
}

pub fn bring(path: &Path, worktree: WorktreeId, from: TerminalKey) -> Result<Went, Unchanged> {
    let from = lunetad::api::TerminalKey { id: from.id.get(), made_at: Some(from.made.into()) };
    let change = Change::Bring(Bring { worktree: worktree.get(), from: Some(from) });
    changed_on(path, None, change).and_then(|made| match made {
        Some(Made::Taken(taken)) if taken == worktree.get() => Ok(Went::Taken),
        Some(Made::Untaken(())) => Ok(Went::Elsewhere),
        made => Err(unread(made, "taken worktree")),
    })
}

const OPENING: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Opening {
    IfEmpty,
    Beside,
}

enum Asking {
    Unsent(Asked),
    Sent { before: BTreeSet<ViewId> },
}

pub fn opened(
    path: &Path,
    worktree: WorktreeId,
    argv: Vec<String>,
    opening: Opening,
) -> Result<(), String> {
    let folder = lunetad::folder::open(path).map_err(|refusal| refusal.to_string())?;
    let folder = folder.ok_or("lunetad has made no folder yet")?;
    let size = Size::of(80, 24).expect("80 by 24 cells is a size");
    let (heard, hearing) = mpsc::channel();
    let at = lunetad::Where::Here { runs_in: None };
    let unvisited = true;
    let show = lunetad::show(folder, at, worktree, size, unvisited, move |it| {
        drop(heard.send(it));
    });
    let beside = match opening {
        Opening::IfEmpty => Beside::Alone,
        Opening::Beside => Beside::Focused,
    };
    let mut asking = Asking::Unsent(Asked::Open { beside, argv });
    let until = Instant::now() + OPENING;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let late = |_| format!("lunetad opened no terminal in {}s", OPENING.as_secs());
        match hearing.recv_timeout(left).map_err(late)? {
            Heard::Laid(laid) if laid.worktree() != worktree => {
                let other = laid.worktree();
                return Err(format!("lunetad laid out worktree {other}, not {worktree}"));
            },
            Heard::Laid(laid) => {
                let views: BTreeSet<ViewId> = laid.views().map(|view| view.id).collect();
                asking = match (asking, opening) {
                    (Asking::Unsent(_), Opening::IfEmpty) if !laid.is_empty() => return Ok(()),
                    (Asking::Unsent(open), Opening::IfEmpty | Opening::Beside) => {
                        show.ask(open);
                        Asking::Sent { before: views }
                    },
                    (Asking::Sent { before }, _) if views.is_subset(&before) => {
                        Asking::Sent { before }
                    },
                    (Asking::Sent { .. }, _) => return Ok(()),
                };
            },
            Heard::Edged { .. } | Heard::Sent(_) => {},
            Heard::Left(gone) => return Err(format!("worktree {gone} is gone")),
            Heard::Refused(why) => return Err(format!("lunetad refused: {why}")),
            Heard::Forgotten(why) | Heard::Lost(why) => return Err(why),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(status: &Status) -> String {
        said_unmade(Unchanged::of(status), "project")
    }

    #[test]
    fn only_a_refusal_from_lunetad_says_nothing_was_made() {
        let refused = Status::failed_precondition("it said no");
        assert_eq!(said(&refused), "no project made: it said no");
        let nests = Status::already_exists("/a/b is inside /a");
        assert_eq!(said(&nests), "no project made: /a/b is inside /a");
        let doubt = Status::unavailable("it dropped");
        assert_eq!(said(&doubt), "it dropped, so the project may be made");
        let timed_out = Status::deadline_exceeded("lunetad did not answer in 5s");
        let late = "lunetad did not answer in 5s, so the project may be made";
        assert_eq!(said(&timed_out), late);

        let folder = Directory::parse("/w/cdk/issue-5".to_owned()).unwrap();
        let unopened = |status: &Status| said_unopened(Unchanged::of(status), &folder, "#5");
        let left = "/w/cdk/issue-5 is #5's worktree, and it is not opened: it said no";
        assert_eq!(unopened(&refused), left, "a pull refused leaves the worktree made");
        let maybe = "it dropped, so /w/cdk/issue-5, #5's worktree, may be opened";
        assert_eq!(unopened(&doubt), maybe, "a pull answered late may have opened it");
    }

    #[test]
    fn a_late_answer_says_the_change_may_be_made() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let never = std::future::pending::<Result<Option<Made>, Unchanged>>();
        let late = runtime.block_on(answered_within(Duration::ZERO, never, Unchanged::InDoubt));
        let said = late.map_err(|unchanged| said_unmade(unchanged, "worktree"));
        let late = "lunetad did not answer in 0s, so the worktree may be made";
        assert_eq!(said, Err(late.to_owned()));
    }

    #[test]
    fn an_answer_that_names_no_such_thing_says_it_cannot_be_read() {
        let said = |unchanged| said_unmade(unchanged, "worktree");
        assert_eq!(
            landed(None).map_err(said),
            Err("luneta can't read lunetad's answer None as a worktree, so the worktree may be \
                 made"
                .to_owned())
        );
        assert_eq!(
            landed(Some(Made::Worktree(0))).map_err(said),
            Err("lunetad said it made worktree 0, and no id is 0, so the worktree may be made"
                .to_owned())
        );
        let said = |unchanged| said_unmade(unchanged, "project");
        let project = project_of(Some(Made::Taken(4))).map_err(said);
        let unread = "luneta can't read lunetad's answer Some(Taken(4)) as a project";
        assert_eq!(project, Err(format!("{unread}, so the project may be made")));
        assert!(matches!(project_of(Some(Made::Project(4))), Ok(id) if id.get() == 4));
        assert!(matches!(landed(Some(Made::Found(5))), Ok(Landed::Found(id)) if id.get() == 5));
    }
}
