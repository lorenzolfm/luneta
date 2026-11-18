use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorkstream, Change, CloseProject};
use lunetad::api::change_projects_request::CloseWorkstream;
use lunetad::api::change_projects_request::RenameProject;
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorkstreams, WorkstreamAt};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, On, Version, WatchRequest};
use lunetad::projects::{ProjectId, WorkstreamId};
use tonic::transport::Channel;
use tonic::{Code, Status};

use lunetad::names::session_for;

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

fn changed(path: &Path, change: Change) -> Result<Option<u64>, Unmade> {
    changed_on(path, None, change)
}

fn changed_on(path: &Path, on: Option<On>, change: Change) -> Result<Option<u64>, Unmade> {
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
    let made = changed(path, change)?.and_then(ProjectId::of);
    made.ok_or_else(|| Unmade::Failed("lunetad made no project".to_owned()))
}

pub fn add_workstream(
    path: &Path,
    project: ProjectId,
    folder: &str,
    name: Option<&str>,
) -> Result<WorkstreamId, String> {
    let (project, folder, name) = (project.get(), folder.to_owned(), name.map(str::to_owned));
    let change = Change::AddWorkstream(AddWorkstream { project, folder, name });
    let made = changed(path, change).map_err(Unmade::said)?.and_then(WorkstreamId::of);
    made.ok_or_else(|| "lunetad made no workstream".to_owned())
}

fn workstream_at(path: &Path, project: ProjectId, folder: &str) -> Result<WorkstreamId, String> {
    let (project, folder) = (project.get(), folder.to_owned());
    let change = Change::WorkstreamAt(WorkstreamAt { project, folder });
    let found = changed(path, change).map_err(Unmade::said)?.and_then(WorkstreamId::of);
    found.ok_or_else(|| "lunetad found and made no workstream".to_owned())
}

pub fn renamed(project: ProjectId, to: &str) -> Result<(), String> {
    let path = lunetad::folder::find().ok_or("lunetad has made no folder to keep projects in")?;
    let (project, to) = (project.get(), to.to_owned());
    let change = Change::RenameProject(RenameProject { project, to });
    changed(&path, change).map(drop).map_err(Unmade::said)
}

pub fn closed(path: &Path, machine: &Machine, workstream: WorkstreamId) -> Result<(), String> {
    let change = Change::CloseWorkstream(CloseWorkstream { workstream: workstream.get() });
    changed_on(path, on(machine), change).map(drop).map_err(Unmade::said)
}

pub fn closed_project(path: &Path, machine: &Machine, project: ProjectId) -> Result<(), String> {
    let change = Change::CloseProject(CloseProject { project: project.get() });
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

fn holding(projects: &[Project], cwd: &Path) -> Option<Project> {
    projects.iter().find(|project| holds(&project.dir, cwd)).cloned()
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

pub fn here(path: &Path, cwd: &Path, name: Option<&str>) -> Result<Option<WorkstreamId>, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    twice(|| {
        if let Some(project) = holding(&kept(path)?, cwd) {
            return at_folder(path, &project, folder);
        }
        match add_project(path, folder, name) {
            Ok(_) => Ok(None),
            Err(Unmade::Failed(why)) => Err(Tried::Failed(why)),
            Err(Unmade::Nests(why)) => match holding(&kept(path)?, cwd) {
                Some(project) => at_folder(path, &project, folder),
                None => Err(Tried::Failed(why)),
            },
        }
    })
}

fn utf8(dir: &Path) -> Result<&str, String> {
    dir.to_str().ok_or_else(|| format!("{} is not UTF-8", dir.display()))
}

pub fn pulled(
    path: &Path,
    project: &str,
    (holder, cwd): (&Path, &Path),
    name: &str,
) -> Result<WorkstreamId, String> {
    let (dir, folder) = (utf8(holder)?, utf8(cwd)?);
    twice(|| {
        let id = match holding(&kept(path)?, cwd) {
            Some(project) => project.id,
            None => add_project(path, dir, Some(project)).map_err(Unmade::said)?,
        };
        in_kept(path, id, add_workstream(path, id, folder, Some(name)))
    })
}

#[must_use]
pub fn real(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned())
}
