use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorkstream, Change, RenameProject};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, Project, WatchRequest};
use lunetad::projects::{ProjectId, WorkstreamId};
use tonic::transport::Channel;

use lunetad::names::session_for;

use crate::daemon;

const ASKING: Duration = Duration::from_secs(15);

type Daemon = DaemonClient<Channel>;

fn asked<T>(
    path: &Path,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, String>,
) -> Result<T, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("no runtime: {error}"))?;
    runtime.block_on(async {
        let mut daemon = daemon::current(path).await?.daemon;
        tokio::time::timeout(ASKING, asking(&mut daemon)).await.map_err(|_| {
            format!("lunetad did not answer in {}s", ASKING.as_secs())
        })?
    })
}

pub fn kept(path: &Path) -> Result<Vec<Project>, String> {
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

fn changed(path: &Path, change: Change) -> Result<Option<u64>, String> {
    asked(path, async |daemon| {
        let request = ChangeProjectsRequest { on: None, change: Some(change) };
        let made = daemon.change_projects(request).await;
        Ok(made.map_err(|status| status.message().to_owned())?.into_inner().made)
    })
}

pub fn add_project(path: &Path, dir: &str, name: Option<&str>) -> Result<ProjectId, String> {
    let (dir, name) = (dir.to_owned(), name.map(str::to_owned));
    let change = Change::AddProject(AddProject { dir, name });
    let made = changed(path, change)?.and_then(ProjectId::of);
    made.ok_or_else(|| "lunetad made no project".to_owned())
}

pub fn add_workstream(
    path: &Path,
    project: ProjectId,
    folder: &str,
    name: Option<&str>,
) -> Result<WorkstreamId, String> {
    let (project, folder, name) = (project.get(), folder.to_owned(), name.map(str::to_owned));
    let change = Change::AddWorkstream(AddWorkstream { project, folder, name });
    let made = changed(path, change)?.and_then(WorkstreamId::of);
    made.ok_or_else(|| "lunetad made no workstream".to_owned())
}

pub fn renamed(from: &str, to: &str) -> Option<Result<(), String>> {
    let path = lunetad::folder::find()?;
    let projects = kept(&path).ok()?;
    let named = projects.iter().find(|project| project.name == from)?;
    let project = named.id;
    let change = Change::RenameProject(RenameProject { project, to: to.to_owned() });
    Some(changed(&path, change).map(drop))
}

fn holds(dir: &str, folder: &Path) -> bool {
    folder.starts_with(dir)
}

fn first_of(path: &Path, project: &Project) -> Result<WorkstreamId, String> {
    match project.workstreams.first().and_then(|it| WorkstreamId::of(it.id)) {
        Some(first) => Ok(first),
        None => add_workstream(path, project_id(project)?, &project.dir, None),
    }
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<WorkstreamId, String>> {
    if let Some(name) = name
        && let Some(named) = projects.iter().find(|project| project.name == name)
    {
        return Some(first_of(path, named));
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
    let here = holding.workstreams.iter().find(|it| Path::new(&it.folder) == cwd);
    Some(match here.and_then(|it| WorkstreamId::of(it.id)) {
        Some(here) => Ok(here),
        None => project_id(holding).and_then(|id| add_workstream(path, id, folder, None)),
    })
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
    let made = if name.is_some() || session_for(cwd).is_some() {
        add_project(path, folder, name)
    } else {
        Err(format!("{} names no project: name one", cwd.display()))
    };
    let unmade = match made {
        Ok(made) => return add_workstream(path, made, folder, None),
        Err(unmade) => unmade,
    };
    let projects = kept(path)?;
    if let Some(chosen) = chosen(path, &projects, name, cwd, folder) {
        return chosen;
    }
    if name.is_some() {
        return Err(unmade);
    }
    let last = |project: &&Project| ProjectId::of(project.id).and_then(&seen);
    first_of(path, projects.iter().max_by_key(last).ok_or(unmade)?)
}

pub fn pulled(path: &Path, project: &str, cwd: &Path, name: &str) -> Result<WorkstreamId, String> {
    let projects = kept(path)?;
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    let holding = projects.iter().find(|it| holds(&it.dir, cwd));
    let id = match holding {
        Some(holding) => project_id(holding)?,
        None => add_project(path, folder, Some(project))?,
    };
    add_workstream(path, id, folder, Some(name))
}

fn project_id(project: &Project) -> Result<ProjectId, String> {
    ProjectId::of(project.id).ok_or_else(|| "lunetad keeps a project of id 0".to_owned())
}

#[must_use]
pub fn real(cwd: &Path) -> PathBuf {
    std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned())
}
