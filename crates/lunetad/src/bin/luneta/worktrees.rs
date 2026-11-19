use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use lunetad::api::change_projects_request::{AddProject, AddWorktree, Change, CloseProject};
use lunetad::api::change_projects_request::{CloseView, CloseWorktree, Pull, Start};
use lunetad::api::change_projects_request::RenameProject;
use lunetad::api::change_projects_request::{ReorderProjects, ReorderWorktrees};
use lunetad::api::change_projects_request::{Bring, Join, RenameTerminal};
use lunetad::api::change_projects_response::Made;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::make_worktree_response::Landed as Answered;
use lunetad::api::projects::Held;
use lunetad::api::{ChangeProjectsRequest, MakeWorktreeRequest, On, Version, WatchRequest};
use lunetad::mux::own::TerminalKey;
use lunetad::projects::{self, Name, ProjectId, Size, ViewId, WorktreeId};
use lunetad::links::{self, Branch, Unready};
use lunetad::{Asked, Beside, Heard};
use tonic::transport::Channel;
use tonic::{Code, Status};

use lunetad::Directory;

use crate::daemon;
use crate::machines::Machine;
use crate::projected::{self, Project};
use crate::sidebar::Reorder;

const ASKING: Duration = Duration::from_secs(15);

type Daemon = DaemonClient<Channel>;

enum Unmade {
    Nests(String),
    Failed(String),
    InDoubt(String),
}

impl From<String> for Unmade {
    fn from(why: String) -> Unmade {
        Unmade::Failed(why)
    }
}

impl Unmade {
    fn of(status: &Status) -> Unmade {
        let why = status.message().to_owned();
        match status.code() {
            Code::AlreadyExists => Unmade::Nests(why),
            code if refused(code) => Unmade::Failed(why),
            _ => Unmade::InDoubt(why),
        }
    }

    fn said(self) -> String {
        match self {
            Unmade::Nests(why) | Unmade::Failed(why) | Unmade::InDoubt(why) => why,
        }
    }

    fn said_of(self, what: &str) -> String {
        match self {
            Unmade::Nests(why) | Unmade::Failed(why) => format!("no {what} made: {why}"),
            Unmade::InDoubt(why) => format!("{why}, so the {what} may be made"),
        }
    }
}

fn refused(code: Code) -> bool {
    let refusals = [
        Code::InvalidArgument,
        Code::FailedPrecondition,
        Code::Aborted,
        Code::NotFound,
        Code::Unimplemented,
    ];
    refusals.contains(&code)
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
    asked_or(path, ASKING, asking, E::from)
}

fn asked_or<T, E: From<String>>(
    path: &Path,
    within: Duration,
    asking: impl AsyncFnOnce(&mut Daemon) -> Result<T, E>,
    late: impl FnOnce(String) -> E,
) -> Result<T, E> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| E::from(format!("no runtime: {error}")))?;
    runtime.block_on(async {
        let mut daemon = daemon::current(path).await.map_err(E::from)?.daemon;
        answered_within(within, asking(&mut daemon), late).await
    })
}

async fn answered_within<T, E>(
    within: Duration,
    answer: impl Future<Output = Result<T, E>>,
    late: impl FnOnce(String) -> E,
) -> Result<T, E> {
    let answered = tokio::time::timeout(within, answer).await;
    answered.map_err(|_| late(format!("lunetad did not answer in {}s", within.as_secs())))?
}

pub fn kept(path: &Path) -> Result<Vec<Project>, String> {
    asked(path, async |daemon| {
        let request = WatchRequest { after: None, kept: None, shown: false };
        let host = daemon.watch(request).await.map_err(|status| status.message().to_owned())?;
        match host.into_inner().projects.and_then(|projects| projects.held) {
            Some(Held::Kept(kept)) => projected::listed(&kept.projects),
            Some(Held::Unkept(why)) => Err(format!("lunetad keeps no projects: {why}")),
            None => Err("lunetad is older than its projects".to_owned()),
        }
    })
}

fn changed_on(path: &Path, on: Option<On>, change: Change) -> Result<Option<Made>, Unmade> {
    let request = ChangeProjectsRequest { on, change: Some(change) };
    let asking = async |daemon: &mut Daemon| match daemon.change_projects(request).await {
        Ok(made) => Ok(made.into_inner().made),
        Err(status) => Err(Unmade::of(&status)),
    };
    asked_or(path, ASKING, asking, Unmade::InDoubt)
}

fn add_project(path: &Path, dir: &str, name: Option<&str>) -> Result<ProjectId, Unmade> {
    let (dir, name) = (dir.to_owned(), name.map(str::to_owned));
    let change = Change::AddProject(AddProject { dir, name });
    project_of(changed_on(path, None, change)?)
}

fn project_of(made: Option<Made>) -> Result<ProjectId, Unmade> {
    match made {
        Some(Made::Project(made)) => {
            ProjectId::of(made).ok_or_else(|| Unmade::InDoubt(zero("project")))
        },
        made => Err(unread(made, "project")),
    }
}

fn zero(made: &str) -> String {
    format!("lunetad said it made {made} 0, and no id is 0")
}

fn unread(answer: Option<Made>, wanted: &str) -> Unmade {
    Unmade::InDoubt(format!("luneta can't read lunetad's answer {answer:?} as a {wanted}"))
}

fn landed(made: Option<Made>) -> Result<Landed, Unmade> {
    let id = |id| WorktreeId::of(id).ok_or_else(|| Unmade::InDoubt(zero("worktree")));
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
    name: Option<&str>,
    argv: Vec<String>,
) -> Result<Landed, String> {
    let (project, folder, name) = (project.get(), folder.to_owned(), name.map(str::to_owned));
    let change = Change::AddWorktree(AddWorktree { project, folder, name, argv });
    let added = changed_on(path, on(machine), change).and_then(landed);
    added.map_err(|unmade| unmade.said_of("worktree"))
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
    made.map_err(|unready| match unready {
        Unready::Nowhere(why) | Unready::Failed(why) => format!("no worktree made: {why}"),
        Unready::InDoubt(why) => why,
    })
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
    started.map_err(|unmade| unmade.said_of("worktree"))
}

pub fn closed(
    path: &Path,
    machine: &Machine,
    worktree: WorktreeId,
    seen: &[ViewId],
) -> Result<(), String> {
    changed_on(path, on(machine), close(worktree, seen)).map(drop).map_err(Unmade::said)
}

pub fn closed_view(path: &Path, machine: &Machine, terminal: TerminalKey) -> Result<(), String> {
    let (id, made_at) = (terminal.id.get(), Some(terminal.made.into()));
    let terminal = Some(lunetad::api::TerminalKey { id, made_at });
    let change = Change::CloseView(CloseView { terminal });
    changed_on(path, on(machine), change).map(drop).map_err(Unmade::said)
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
        if refused(status.code()) { Unmoved::Refused(why) } else { Unmoved::Unsure(why) }
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
    asked_or(path, ASKING, asking, Unmoved::Unsure)
}

fn holds(dir: &str, folder: &Path) -> bool {
    folder.starts_with(dir)
}

fn at_folder(path: &Path, project: &Project, folder: &str) -> Result<Landing, Tried> {
    assert!(holds(&project.dir, Path::new(folder)), "{folder} is in {}", project.dir);
    let started = in_kept(path, project.id, started_at(path, folder))?;
    Ok(started.map_or(Landing::Unopened(project.id), Landing::Worktree))
}

fn chosen(
    path: &Path,
    projects: &[Project],
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
) -> Option<Result<Landing, Tried>> {
    if let Some(name) = name {
        let named: Vec<&Project> = projects.iter().filter(|project| project.name == name).collect();
        match named[..] {
            [] => {},
            [one] => {
                let last = projects::last_used(one.uses()).map(Landed::Found);
                let landing = last.map_or(Landing::Unopened(one.id), Landing::Worktree);
                return Some(Ok(landing));
            },
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
) -> Result<Option<Landing>, String> {
    let folder = cwd.to_str().ok_or_else(|| format!("{} is not UTF-8", cwd.display()))?;
    twice(|| started(path, name, cwd, folder, &seen))
}

fn started(
    path: &Path,
    name: Option<&str>,
    cwd: &Path,
    folder: &str,
    seen: &impl Fn(ProjectId) -> Option<SystemTime>,
) -> Result<Option<Landing>, Tried> {
    let projects = kept(path)?;
    if let Some(chosen) = chosen(path, &projects, name, cwd, folder) {
        return chosen.map(Some);
    }
    let Some(name) = name else {
        let last = |project: &&Project| seen(project.id);
        let held = projects.iter().filter(|project| !project.worktrees().is_empty());
        let found = held.max_by_key(last).and_then(|it| projects::last_used(it.uses()));
        let found = found.map(Landed::Found);
        return Ok(found.map(Landing::Worktree));
    };
    let unmade = match add_project(path, folder, Some(name)) {
        Ok(_) => None,
        Err(unmade @ Unmade::Failed(_)) => return Err(Tried::Failed(unmade.said_of("project"))),
        Err(unmade @ (Unmade::Nests(_) | Unmade::InDoubt(_))) => Some(unmade.said_of("project")),
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
    made.map_err(|unmade| unmade.said_of("worktree"))
}

pub fn pulled(
    path: &Path,
    machine: &Machine,
    project: &str,
    (holder, cwd): (&Directory, &Directory),
    name: &str,
    argv: Vec<String>,
) -> Result<Landed, String> {
    let (holder, folder) = (holder.to_string(), cwd.to_string());
    let (project, name) = (project.to_owned(), name.to_owned());
    let change = Change::Pull(Pull { holder, folder, project, name, argv });
    let pulled = changed_on(path, on(machine), change).and_then(landed);
    pulled.map_err(|unmade| unmade.said_of("session"))
}

pub fn real(cwd: &Path) -> Result<PathBuf, String> {
    std::fs::canonicalize(cwd).map_err(|error| format!("{}: {error}", cwd.display()))
}

fn made_there(machine: &Machine, change: Change) -> Result<Option<Made>, Unmade> {
    let unfound = |unfound| Unmade::Failed(format!("{unfound}, to find lunetad in"));
    let path = lunetad::folder::find().map_err(unfound)?;
    changed_on(&path, on(machine), change)
}

fn changed_there(machine: &Machine, change: Change) -> Result<(), String> {
    made_there(machine, change).map(drop).map_err(Unmade::said)
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

pub enum Landing {
    Worktree(Landed),
    Unopened(ProjectId),
}

pub fn project_made(
    machine: &Machine,
    folder: &str,
    name: Option<&Name>,
) -> Result<Landing, String> {
    let (dir, name) = (folder.to_owned(), name.map(spelled));
    let made = made_there(machine, Change::AddProject(AddProject { dir, name }));
    let project = made.and_then(project_of).map_err(|unmade| unmade.said_of("project"))?;
    let start = Start { folder: folder.to_owned(), name: None };
    let started = made_there(machine, Change::Start(start)).and_then(|made| match made {
        Some(Made::Project(root)) if root == project.get() => Ok(Landing::Unopened(project)),
        made => landed(made).map(Landing::Worktree),
    });
    let unstarted = |unmade: Unmade| {
        format!("{folder} is project {project}: {}", unmade.said_of("worktree"))
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
    let change = Change::Join(Join { folder: folder.to_owned(), argv });
    let joined = changed_on(path, None, change).and_then(landed);
    joined.map_err(|unmade| match unmade {
        Unmade::Nests(why) | Unmade::Failed(why) => why,
        Unmade::InDoubt(why) => format!("{why}, so lunetad may keep {folder} as a worktree"),
    })
}

pub fn dirs(path: &Path) -> Result<Vec<String>, String> {
    Ok(kept(path)?.into_iter().map(|project| project.dir).collect())
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
        Unmade::of(status).said_of("project")
    }

    #[test]
    fn only_a_refusal_from_lunetad_says_nothing_was_made() {
        let refusals = [
            Status::invalid_argument("it said no"),
            Status::failed_precondition("it said no"),
            Status::aborted("it said no"),
            Status::not_found("it said no"),
            Status::unimplemented("it said no"),
        ];
        for status in &refusals {
            assert_eq!(said(status), "no project made: it said no", "{status:?}");
        }
        let nests = Status::already_exists("/a/b is inside /a");
        assert_eq!(said(&nests), "no project made: /a/b is inside /a");
        let doubts = [
            Status::unavailable("it dropped"),
            Status::cancelled("it dropped"),
            Status::deadline_exceeded("it dropped"),
            Status::unknown("it dropped"),
            Status::internal("it dropped"),
            Status::resource_exhausted("it dropped"),
        ];
        for status in &doubts {
            assert_eq!(said(status), "it dropped, so the project may be made", "{status:?}");
        }
        let timed_out = Status::cancelled("Timeout expired");
        assert_eq!(said(&timed_out), "Timeout expired, so the project may be made");
        for status in &refusals {
            assert!(matches!(Unmoved::of(status), Unmoved::Refused(_)), "{status:?}");
        }
        for status in &doubts {
            assert!(matches!(Unmoved::of(status), Unmoved::Unsure(_)), "{status:?}");
        }
    }

    #[test]
    fn a_late_answer_says_the_change_may_be_made() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let never = std::future::pending::<Result<Option<Made>, Unmade>>();
        let late = runtime.block_on(answered_within(Duration::ZERO, never, Unmade::InDoubt));
        let said = late.map_err(|unmade| unmade.said_of("worktree"));
        let late = "lunetad did not answer in 0s, so the worktree may be made";
        assert_eq!(said, Err(late.to_owned()));
    }

    #[test]
    fn an_answer_that_names_no_such_thing_says_it_cannot_be_read() {
        let said = |unmade: Unmade| unmade.said_of("worktree");
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
        let said = |unmade: Unmade| unmade.said_of("project");
        let project = project_of(Some(Made::Taken(4))).map_err(said);
        let unread = "luneta can't read lunetad's answer Some(Taken(4)) as a project";
        assert_eq!(project, Err(format!("{unread}, so the project may be made")));
        assert!(matches!(project_of(Some(Made::Project(4))), Ok(id) if id.get() == 4));
        assert!(matches!(landed(Some(Made::Found(5))), Ok(Landed::Found(id)) if id.get() == 5));
    }
}
