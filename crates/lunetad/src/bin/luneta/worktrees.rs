use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorktree, Change, CloseProject};
use lunetad::api::change_projects_request::{CloseWorktree, Pull, Start};
use lunetad::api::change_projects_request::RenameProject;
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorktrees};
use lunetad::api::change_projects_request::{Bring, Join, RenameTerminal};
use lunetad::api::change_projects_response::Made;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, On, Version, WatchRequest};
use lunetad::mux::own::TerminalKey;
use lunetad::projects::{Name, ProjectId, Size, ViewId, WorktreeId};
use lunetad::{Asked, Beside, Heard};
use tonic::transport::Channel;
use tonic::{Code, Status};

use lunetad::names::session_for;
use lunetad::Directory;

use crate::daemon;
use crate::machines::Machine;
use crate::projected::{self, Project, Worktree};
use crate::sidebar::Reorder;

const ASKING: Duration = Duration::from_secs(15);

type Daemon = DaemonClient<Channel>;

enum Unmade {
    Nests(String),
    Failed(String),
}

impl From<String> for Unmade {
    fn from(why: String) -> Unmade {
        Unmade::Failed(why)
    }
}

impl Unmade {
    fn said(self) -> String {
        match self {
            Unmade::Nests(why) | Unmade::Failed(why) => why,
        }
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
    asked_or(path, asking, E::from)
}

fn asked_or<T, E: From<String>>(
    path: &Path,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, E>,
    late: impl FnOnce(String) -> E,
) -> Result<T, E> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| E::from(format!("no runtime: {error}")))?;
    runtime.block_on(async {
        let mut daemon = daemon::current(path).await.map_err(E::from)?.daemon;
        let answered = tokio::time::timeout(ASKING, asking(&mut daemon)).await;
        answered.map_err(|_| late(format!("lunetad did not answer in {}s", ASKING.as_secs())))?
    })
}

pub fn kept(path: &Path) -> Result<Vec<Project>, String> {
    asked(path, async |daemon| {
        let request = WatchRequest { after: None, kept: None, shown: false };
        let host = daemon.watch(request).await.map_err(|status| status.message().to_owned())?;
        match host.into_inner().projects.and_then(|projects| projects.held) {
            Some(Held::Kept(kept)) => kept.projects.iter().map(projected::project).collect(),
            Some(Held::Unkept(why)) => Err(format!("lunetad keeps no projects: {why}")),
            None => Err("lunetad is older than its projects".to_owned()),
        }
    })
}

fn changed_on(path: &Path, on: Option<On>, change: Change) -> Result<Option<Made>, Unmade> {
    asked(path, async |daemon| {
        let request = ChangeProjectsRequest { on, change: Some(change) };
        match daemon.change_projects(request).await {
            Ok(made) => Ok(made.into_inner().made),
            Err(status) => {
                let why = status.message().to_owned();
                Err(match status.code() {
                    Code::AlreadyExists => Unmade::Nests(why),
                    _ => Unmade::Failed(why),
                })
            },
        }
    })
}

fn add_project(path: &Path, dir: &str, name: Option<&str>) -> Result<ProjectId, Unmade> {
    let (dir, name) = (dir.to_owned(), name.map(str::to_owned));
    let change = Change::AddProject(AddProject { dir, name });
    match changed_on(path, None, change)? {
        Some(Made::Project(made)) => ProjectId::of(made).ok_or_else(|| zero("project").into()),
        made => Err(Unmade::Failed(format!("lunetad made no project but {made:?}"))),
    }
}

fn zero(made: &str) -> String {
    format!("lunetad said it made {made} 0, and no id is 0")
}

fn worktree_made(made: Option<Made>) -> Result<WorktreeId, String> {
    match made {
        Some(Made::Worktree(made)) => WorktreeId::of(made).ok_or_else(|| zero("worktree")),
        made => Err(format!("lunetad made no worktree but {made:?}")),
    }
}

pub fn add_worktree(
    path: &Path,
    machine: &Machine,
    project: ProjectId,
    folder: &str,
    name: Option<&str>,
) -> Result<WorktreeId, String> {
    let (project, folder, name) = (project.get(), folder.to_owned(), name.map(str::to_owned));
    let change = Change::AddWorktree(AddWorktree { project, folder, name });
    worktree_made(changed_on(path, on(machine), change).map_err(Unmade::said)?)
}

fn started_at(path: &Path, folder: &str) -> Result<Option<WorktreeId>, String> {
    let start = Start { folder: folder.to_owned(), name: None };
    match changed_on(path, None, Change::Start(start)).map_err(Unmade::said)? {
        Some(Made::Project(_)) => Ok(None),
        made => worktree_made(made).map(Some),
    }
}

pub fn closed(
    path: &Path,
    machine: &Machine,
    worktree: WorktreeId,
    seen: &[ViewId],
) -> Result<(), String> {
    changed_on(path, on(machine), close(worktree, seen)).map(drop).map_err(Unmade::said)
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
) -> Result<(), String> {
    let project = project.get();
    let worktrees = worktrees.iter().map(|it| it.get()).collect();
    let seen = seen.iter().map(|it| it.get()).collect();
    let change = Change::CloseProject(CloseProject { project, worktrees, seen });
    changed_on(path, on(machine), change).map(drop).map_err(Unmade::said)
}

fn on(machine: &Machine) -> Option<On> {
    match machine {
        Machine::Local => None,
        Machine::Remote(destination) => Some(On { destination: destination.to_string() }),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unmoved {
    Refused(String),
    Unsure(String),
}

impl From<String> for Unmoved {
    fn from(why: String) -> Unmoved {
        Unmoved::Refused(why)
    }
}

impl Unmoved {
    fn of(status: &Status) -> Unmoved {
        let why = status.message().to_owned();
        let refused = [Code::Aborted, Code::InvalidArgument, Code::FailedPrecondition];
        if refused.contains(&status.code()) { Unmoved::Refused(why) } else { Unmoved::Unsure(why) }
    }
}

pub fn reordered(path: &Path, machine: &Machine, order: &Reorder) -> Result<Option<Version>, Unmoved> {
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
    let request = ChangeProjectsRequest { on: on(machine), change: Some(change) };
    let asking = async |daemon: &mut Daemon| match daemon.change_projects(request).await {
        Ok(answered) => Ok(answered.into_inner().kept),
        Err(status) => Err(Unmoved::of(&status)),
    };
    asked_or(path, asking, Unmoved::Unsure)
}

fn holds(dir: &str, folder: &Path) -> bool {
    folder.starts_with(dir)
}

fn last_used(project: &Project) -> Option<WorktreeId> {
    let mut last: Option<&Worktree> = None;
    for worktree in &project.worktrees {
        if last.is_none_or(|last| worktree.used > last.used) {
            last = Some(worktree);
        }
    }
    assert_eq!(last.is_some(), !project.worktrees.is_empty(), "a project's worktree is last");
    last.map(|it| it.id)
}

fn at_folder(path: &Path, project: &Project, folder: &str) -> Result<Option<WorktreeId>, Tried> {
    assert!(holds(&project.dir, Path::new(folder)), "{folder} is in {}", project.dir);
    in_kept(path, project.id, started_at(path, folder))
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<Option<WorktreeId>, Tried>> {
    if let Some(name) = name {
        let named: Vec<&Project> = projects.iter().filter(|project| project.name == name).collect();
        match named[..] {
            [] => {},
            [one] => return Some(Ok(last_used(one))),
            [..] => {
                let dirs: Vec<&str> = named.iter().map(|project| project.dir.as_str()).collect();
                let dirs = dirs.join(", ");
                return Some(Err(Tried::Failed(format!(
                    "projects in {dirs} are all named {name}: start luneta in the one you mean"
                ))));
            },
        }
    }
    let holding = projects.iter().find(|project| holds(&project.dir, cwd))?;
    if let Some(name) = name
        && name != holding.name
    {
        let (cwd, holding) = (cwd.display(), &holding.name);
        return Some(Err(Tried::Failed(format!(
            "{cwd} is in project {holding}, and no project is made inside another"
        ))));
    }
    Some(at_folder(path, holding, folder))
}

pub fn start(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    seen: impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<WorktreeId>, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    twice(|| started(path, name, cwd, folder, &seen))
}

fn started(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
    seen: &impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<WorktreeId>, Tried> {
    if let Some(chosen) = chosen(path, &kept(path)?, name, cwd, folder) {
        return chosen;
    }
    let unmade = if name.is_some() || session_for(cwd).is_some() {
        match add_project(path, folder, name) {
            Ok(_) => format!("lunetad made the project of {}, with no worktree", cwd.display()),
            Err(Unmade::Failed(why)) => return Err(Tried::Failed(why)),
            Err(Unmade::Nests(why)) => why,
        }
    } else {
        format!("{} names no project: name one", cwd.display())
    };
    let projects = kept(path)?;
    if let Some(chosen) = chosen(path, &projects, name, cwd, folder) {
        return chosen;
    }
    if name.is_some() {
        return Err(Tried::Failed(unmade));
    }
    let last = |project: &&Project| seen(project.id);
    Ok(last_used(projects.iter().max_by_key(last).ok_or(unmade)?))
}

pub fn made_at(
    path: &Path,
    machine: &Machine,
    cwd: &Directory,
    name: Option<&str>,
) -> Result<Option<WorktreeId>, String> {
    let (folder, name) = (cwd.to_string(), name.map(str::to_owned));
    match changed_on(path, on(machine), Change::Start(Start { folder, name })) {
        Ok(Some(Made::Project(_))) => Ok(None),
        Ok(made) => worktree_made(made).map(Some),
        Err(unmade) => Err(unmade.said()),
    }
}

pub fn pulled(
    path: &Path,
    machine: &Machine,
    project: &str,
    (holder, cwd): (&Directory, &Directory),
    name: &str,
) -> Result<WorktreeId, String> {
    let (holder, folder) = (holder.to_string(), cwd.to_string());
    let (project, name) = (project.to_owned(), name.to_owned());
    let change = Change::Pull(Pull { holder, folder, project, name });
    worktree_made(changed_on(path, on(machine), change).map_err(Unmade::said)?)
}

pub fn real(cwd: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(cwd).map_err(|error| format!("{}: {error}", cwd.display()))
}

fn made_there(machine: &Machine, change: Change) -> Result<Option<Made>, String> {
    let path = lunetad::folder::find().map_err(|unfound| format!("{unfound}, to find lunetad in"))?;
    changed_on(&path, on(machine), change).map_err(Unmade::said)
}

fn changed_there(machine: &Machine, change: Change) -> Result<(), String> {
    made_there(machine, change).map(drop)
}

pub fn closed_there(
    machine: &Machine,
    worktree: WorktreeId,
    seen: &[ViewId],
) -> Result<(), String> {
    changed_there(machine, close(worktree, seen))
}

pub fn renamed_project(machine: &Machine, project: ProjectId, to: &Name) -> Result<(), String> {
    let (project, to) = (project.get(), spelled(to));
    changed_there(machine, Change::RenameProject(RenameProject { project, to }))
}

pub fn renamed_worktree(
    machine: &Machine,
    worktree: WorktreeId,
    to: Option<&Name>,
) -> Result<(), String> {
    let (worktree, to) = (worktree.get(), to.map(spelled));
    let renames = lunetad::api::change_projects_request::RenameWorktree { worktree, to };
    changed_there(machine, Change::RenameWorktree(renames))
}

pub fn renamed_terminal(
    machine: &Machine,
    terminal: ViewId,
    to: Option<&Name>,
) -> Result<(), String> {
    let (terminal, to) = (terminal.get(), to.map(spelled));
    changed_there(machine, Change::RenameTerminal(RenameTerminal { terminal, to }))
}

pub fn project_made(
    machine: &Machine,
    folder: &str,
    name: Option<&Name>,
) -> Result<WorktreeId, String> {
    let (dir, name) = (folder.to_owned(), name.map(spelled));
    let unmade = |why: String| format!("no project made: {why}");
    let made = made_there(machine, Change::AddProject(AddProject { dir, name })).map_err(unmade)?;
    let project = match made {
        Some(Made::Project(project)) => project,
        made => return Err(unmade(format!("lunetad made no project but {made:?}"))),
    };
    let empty = |why: String| format!("{folder} is project {project}, with no worktree: {why}");
    let start = Start { folder: folder.to_owned(), name: None };
    worktree_made(made_there(machine, Change::Start(start)).map_err(empty)?).map_err(empty)
}

fn spelled(name: &Name) -> String {
    name.as_str().to_owned()
}

pub enum Joined {
    Found(WorktreeId),
    Made(WorktreeId),
}

impl Joined {
    pub fn worktree(&self) -> WorktreeId {
        match self {
            Joined::Found(worktree) | Joined::Made(worktree) => *worktree,
        }
    }
}

pub fn worked_in(path: &Path, folder: &Path) -> Result<Joined, String> {
    let folder = folder.to_str().ok_or_else(|| format!("{} is not UTF-8", folder.display()))?;
    let change = Change::Join(Join { folder: folder.to_owned() });
    let id = |id| WorktreeId::of(id).ok_or_else(|| zero("worktree"));
    match changed_on(path, None, change).map_err(Unmade::said)? {
        Some(Made::Found(found)) => id(found).map(Joined::Found),
        Some(Made::Worktree(made)) => id(made).map(Joined::Made),
        made => Err(format!("lunetad joined no worktree but {made:?}")),
    }
}

pub fn dirs(path: &Path) -> Result<Vec<String>, String> {
    Ok(kept(path)?.into_iter().map(|project| project.dir).collect())
}

pub fn folder_of(path: &Path, worktree: WorktreeId) -> Result<PathBuf, String> {
    let projects = kept(path)?;
    let mut worktrees = projects.into_iter().flat_map(|project| project.worktrees);
    let found = worktrees.find(|it| it.id == worktree);
    let found = found.ok_or_else(|| format!("lunetad keeps no worktree {worktree}"))?;
    Ok(PathBuf::from(found.folder))
}

pub enum Went {
    Taken,
    Elsewhere,
}

pub fn bring(path: &Path, worktree: WorktreeId, from: TerminalKey) -> Result<Went, String> {
    let from = lunetad::api::TerminalKey { id: from.id.get(), made_at: Some(from.made.into()) };
    let change = Change::Bring(Bring { worktree: worktree.get(), from: Some(from) });
    match changed_on(path, None, change).map_err(Unmade::said)? {
        Some(Made::Taken(taken)) if taken == worktree.get() => Ok(Went::Taken),
        None => Ok(Went::Elsewhere),
        made => Err(format!("lunetad took luneta nowhere but {made:?}")),
    }
}

const OPENING: Duration = Duration::from_secs(15);

pub fn opened(path: &Path, worktree: WorktreeId, argv: Vec<String>) -> Result<(), String> {
    let folder = lunetad::folder::open(path).map_err(|refusal| refusal.to_string())?;
    let folder = folder.ok_or("lunetad has made no folder yet")?;
    let size = Size::of(80, 24).expect("80 by 24 cells is a size");
    let (heard, hearing) = mpsc::channel();
    let at = lunetad::Where::Here { runs_in: None };
    let unvisited = true;
    let show = lunetad::show(folder, at, worktree, size, unvisited, move |it| {
        drop(heard.send(it));
    });
    let mut open = Some(Asked::Open { beside: Beside::Alone, argv });
    let until = Instant::now() + OPENING;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let late = |_| format!("lunetad opened no terminal in {}s", OPENING.as_secs());
        match hearing.recv_timeout(left).map_err(late)? {
            Heard::Laid(laid) if laid.worktree() != worktree => {
                let other = laid.worktree();
                return Err(format!("lunetad laid out worktree {other}, not {worktree}"));
            },
            Heard::Laid(laid) if laid.is_empty() => {
                if let Some(open) = open.take() {
                    show.ask(open);
                }
            },
            Heard::Laid(_) => return Ok(()),
            Heard::Edged { .. } | Heard::Sent(_) => {},
            Heard::Refused(why) => return Err(format!("lunetad refused: {why}")),
            Heard::Forgotten(why) | Heard::Lost(why) => return Err(why),
        }
    }
}
