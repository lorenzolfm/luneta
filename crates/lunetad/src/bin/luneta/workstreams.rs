use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorkstream, Change, RenameProject};
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorkstreams, WorkstreamAt};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, On, WatchRequest};
use lunetad::projects::{ProjectId, WorkstreamId};
use tonic::transport::Channel;
use tonic::Code;

use lunetad::names::session_for;

use crate::daemon;
use crate::machines::Machine;
use crate::projected::{self, Project};
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

fn asked<T, E: From<String>>(
    path: &Path,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, E>,
) -> Result<T, E> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| E::from(format!("no runtime: {error}")))?;
    runtime.block_on(async {
        let mut daemon = daemon::current(path).await.map_err(E::from)?.daemon;
        let answered = tokio::time::timeout(ASKING, asking(&mut daemon)).await;
        answered.map_err(|_| E::from(format!("lunetad did not answer in {}s", ASKING.as_secs())))?
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

pub fn reordered(path: &Path, machine: &Machine, order: &Reorder) -> Result<(), String> {
    let on = match machine {
        Machine::Local => None,
        Machine::Remote(destination) => Some(On { destination: destination.to_string() }),
    };
    let change = match order {
        Reorder::Projects(projects) => {
            let projects = projects.iter().map(|it| it.get()).collect();
            Change::ReorderProjects(ReorderProjects { projects })
        },
        Reorder::Workstreams { project, workstreams } => {
            let workstreams = workstreams.iter().map(|it| it.get()).collect();
            Change::ReorderWorkstreams(ReorderWorkstreams { project: project.get(), workstreams })
        },
    };
    changed_on(path, on, change).map(drop).map_err(Unmade::said)
}

fn holds(dir: &str, folder: &Path) -> bool {
    folder.starts_with(dir)
}

fn holding(projects: &[Project], cwd: &Path) -> Option<ProjectId> {
    projects.iter().find(|project| holds(&project.dir, cwd)).map(|project| project.id)
}

fn first_of(path: &Path, project: &Project) -> Result<WorkstreamId, String> {
    match project.workstreams.first() {
        Some(first) => Ok(first.id),
        None => workstream_at(path, project.id, &project.dir),
    }
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<WorkstreamId, String>> {
    if let Some(name) = name {
        let named: Vec<&Project> = projects.iter().filter(|project| project.name == name).collect();
        match named[..] {
            [] => {},
            [one] => return Some(first_of(path, one)),
            [..] => {
                let dirs: Vec<&str> = named.iter().map(|project| project.dir.as_str()).collect();
                let dirs = dirs.join(", ");
                return Some(Err(format!(
                    "projects in {dirs} are all named {name}: start luneta in the one you mean"
                )));
            },
        }
    }
    let holding = projects.iter().find(|project| holds(&project.dir, cwd))?;
    if let Some(name) = name
        && name != holding.name
    {
        let (cwd, holding) = (cwd.display(), &holding.name);
        return Some(Err(format!(
            "{cwd} is in project {holding}, and no project is made inside another"
        )));
    }
    Some(workstream_at(path, holding.id, folder))
}

pub fn start(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    seen: impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<WorkstreamId, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    if let Some(chosen) = chosen(path, &kept(path)?, name, cwd, folder) {
        return chosen;
    }
    let unmade = if name.is_some() || session_for(cwd).is_some() {
        match add_project(path, folder, name) {
            Ok(made) => return workstream_at(path, made, folder),
            Err(Unmade::Failed(why)) => return Err(why),
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
        return Err(unmade);
    }
    let last = |project: &&Project| seen(project.id);
    first_of(path, projects.iter().max_by_key(last).ok_or(unmade)?)
}

pub fn here(path: &Path, cwd: &Path, name: Option<&str>) -> Result<WorkstreamId, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    if let Some(id) = holding(&kept(path)?, cwd) {
        return workstream_at(path, id, folder);
    }
    match add_project(path, folder, name) {
        Ok(made) => workstream_at(path, made, folder),
        Err(Unmade::Failed(why)) => Err(why),
        Err(Unmade::Nests(why)) => match holding(&kept(path)?, cwd) {
            Some(id) => workstream_at(path, id, folder),
            None => Err(why),
        },
    }
}

pub fn pulled(path: &Path, project: &str, cwd: &Path, name: &str) -> Result<WorkstreamId, String> {
    let projects = kept(path)?;
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    let id = match holding(&projects, cwd) {
        Some(id) => id,
        None => add_project(path, folder, Some(project)).map_err(Unmade::said)?,
    };
    add_workstream(path, id, folder, Some(name))
}

#[must_use]
pub fn real(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned())
}
