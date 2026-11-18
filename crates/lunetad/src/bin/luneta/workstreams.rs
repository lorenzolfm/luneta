use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorkstream, Change, CloseProject};
use lunetad::api::change_projects_request::{CloseWorkstream, Pull, Start};
use lunetad::api::change_projects_request::RenameProject;
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorkstreams, WorkstreamAt};
use lunetad::api::change_projects_request::RenameTerminal;
use lunetad::api::change_projects_response::Made;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, On, Version, WatchRequest};
use lunetad::projects::{Name, ProjectId, TerminalId, WorkstreamId};
use tonic::transport::Channel;
use tonic::{Code, Status};

use lunetad::names::session_for;
use lunetad::Directory;

use crate::daemon;
use crate::machines::Machine;
use crate::projected::{self, Project, Workstream};
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

fn workstream_made(made: Option<Made>) -> Result<WorkstreamId, String> {
    match made {
        Some(Made::Workstream(made)) => WorkstreamId::of(made).ok_or_else(|| zero("workstream")),
        made => Err(format!("lunetad made no workstream but {made:?}")),
    }
}

pub fn add_workstream(
    path: &Path,
    machine: &Machine,
    project: ProjectId,
    folder: &str,
    name: Option<&str>,
) -> Result<WorkstreamId, String> {
    let (project, folder, name) = (project.get(), folder.to_owned(), name.map(str::to_owned));
    let change = Change::AddWorkstream(AddWorkstream { project, folder, name });
    workstream_made(changed_on(path, on(machine), change).map_err(Unmade::said)?)
}

fn workstream_at(path: &Path, project: ProjectId, folder: &str) -> Result<WorkstreamId, String> {
    let (project, folder) = (project.get(), folder.to_owned());
    let change = Change::WorkstreamAt(WorkstreamAt { project, folder });
    workstream_made(changed_on(path, None, change).map_err(Unmade::said)?)
}

pub fn closed(
    path: &Path,
    machine: &Machine,
    workstream: WorkstreamId,
    seen: &[TerminalId],
) -> Result<(), String> {
    changed_on(path, on(machine), close(workstream, seen)).map(drop).map_err(Unmade::said)
}

fn close(workstream: WorkstreamId, seen: &[TerminalId]) -> Change {
    let (workstream, seen) = (workstream.get(), seen.iter().map(|it| it.get()).collect());
    Change::CloseWorkstream(CloseWorkstream { workstream, seen })
}

pub fn closed_project(
    path: &Path,
    machine: &Machine,
    project: ProjectId,
    workstreams: &[WorkstreamId],
    seen: &[TerminalId],
) -> Result<(), String> {
    let project = project.get();
    let workstreams = workstreams.iter().map(|it| it.get()).collect();
    let seen = seen.iter().map(|it| it.get()).collect();
    let change = Change::CloseProject(CloseProject { project, workstreams, seen });
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
        Reorder::Workstreams { project, seen, wanted } => {
            let ids = |ids: &[WorkstreamId]| ids.iter().map(|it| it.get()).collect();
            let (project, workstreams, seen) = (project.get(), ids(wanted), ids(seen));
            Change::ReorderWorkstreams(ReorderWorkstreams { project, workstreams, seen })
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

fn last_used(project: &Project) -> Option<WorkstreamId> {
    let mut last: Option<&Workstream> = None;
    for workstream in &project.workstreams {
        if last.is_none_or(|last| workstream.used > last.used) {
            last = Some(workstream);
        }
    }
    assert_eq!(last.is_some(), !project.workstreams.is_empty(), "a project's workstream is last");
    last.map(|it| it.id)
}

fn at_folder(path: &Path, project: &Project, folder: &str) -> Result<Option<WorkstreamId>, Tried> {
    assert!(holds(&project.dir, Path::new(folder)), "{folder} is in {}", project.dir);
    if project.dir == folder {
        return Ok(last_used(project));
    }
    in_kept(path, project.id, workstream_at(path, project.id, folder)).map(Some)
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<Option<WorkstreamId>, Tried>> {
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
) -> Result<Option<WorkstreamId>, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    twice(|| started(path, name, cwd, folder, &seen))
}

fn started(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
    seen: &impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<WorkstreamId>, Tried> {
    if let Some(chosen) = chosen(path, &kept(path)?, name, cwd, folder) {
        return chosen;
    }
    let unmade = if name.is_some() || session_for(cwd).is_some() {
        match add_project(path, folder, name) {
            Ok(_) => return Ok(None),
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
) -> Result<Option<WorkstreamId>, String> {
    let (folder, name) = (cwd.to_string(), name.map(str::to_owned));
    match changed_on(path, on(machine), Change::Start(Start { folder, name })) {
        Ok(Some(Made::Project(_))) => Ok(None),
        Ok(made) => workstream_made(made).map(Some),
        Err(unmade) => Err(unmade.said()),
    }
}

pub fn pulled(
    path: &Path,
    machine: &Machine,
    project: &str,
    (holder, cwd): (&Directory, &Directory),
    name: &str,
) -> Result<WorkstreamId, String> {
    let (holder, folder) = (holder.to_string(), cwd.to_string());
    let (project, name) = (project.to_owned(), name.to_owned());
    let change = Change::Pull(Pull { holder, folder, project, name });
    workstream_made(changed_on(path, on(machine), change).map_err(Unmade::said)?)
}

pub fn real(cwd: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(cwd).map_err(|error| format!("{}: {error}", cwd.display()))
}

fn made_there(machine: &Machine, change: Change) -> Result<Option<Made>, String> {
    let path = lunetad::folder::find().ok_or("lunetad has made no folder to keep projects in")?;
    changed_on(&path, on(machine), change).map_err(Unmade::said)
}

fn changed_there(machine: &Machine, change: Change) -> Result<(), String> {
    made_there(machine, change).map(drop)
}

pub fn closed_there(
    machine: &Machine,
    workstream: WorkstreamId,
    seen: &[TerminalId],
) -> Result<(), String> {
    changed_there(machine, close(workstream, seen))
}

pub fn renamed_project(machine: &Machine, project: ProjectId, to: &Name) -> Result<(), String> {
    let (project, to) = (project.get(), spelled(to));
    changed_there(machine, Change::RenameProject(RenameProject { project, to }))
}

pub fn renamed_workstream(
    machine: &Machine,
    workstream: WorkstreamId,
    to: Option<&Name>,
) -> Result<(), String> {
    let (workstream, to) = (workstream.get(), to.map(spelled));
    let renames = lunetad::api::change_projects_request::RenameWorkstream { workstream, to };
    changed_there(machine, Change::RenameWorkstream(renames))
}

pub fn renamed_terminal(
    machine: &Machine,
    terminal: TerminalId,
    to: Option<&Name>,
) -> Result<(), String> {
    let (terminal, to) = (terminal.get(), to.map(spelled));
    changed_there(machine, Change::RenameTerminal(RenameTerminal { terminal, to }))
}

pub fn project_made(
    machine: &Machine,
    folder: &str,
    name: Option<&Name>,
) -> Result<WorkstreamId, String> {
    let (dir, name) = (folder.to_owned(), name.map(spelled));
    let unmade = |why: String| format!("no project made: {why}");
    let made = made_there(machine, Change::AddProject(AddProject { dir, name })).map_err(unmade)?;
    let project = match made {
        Some(Made::Project(project)) => project,
        made => return Err(unmade(format!("lunetad made no project but {made:?}"))),
    };
    let empty = |why: String| format!("{folder} is a project now, with no workstream: {why}");
    let at = WorkstreamAt { project, folder: folder.to_owned() };
    workstream_made(made_there(machine, Change::WorkstreamAt(at)).map_err(empty)?).map_err(empty)
}

fn spelled(name: &Name) -> String {
    name.as_str().to_owned()
}
