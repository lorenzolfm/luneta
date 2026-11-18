use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::num::NonZeroU64;
use std::path::{Component, Path};
use std::str::FromStr;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::github::{PullRequest, State, Verdict};

use crate::api;
use crate::core::{self, Command};
use crate::watch::Troubles;
use crate::SessionName;

mod file;
pub(crate) mod layout;
mod view;

pub use file::{Changed, Failed, Revision, Store};
pub use layout::{Size, FRAME_CELLS, PANES_MAX};
pub use view::{Line, Placed, Runs, Session, Terminal, View, LINE_BYTES_MAX, SESSION_BYTES_MAX};

pub const PROJECTS_MAX: usize = 64;

pub const WORKSTREAMS_MAX: usize = 512;

pub const TERMINALS_MAX: usize = 1024;

pub const NAME_CHARS_MAX: usize = 64;

pub const PATH_BYTES_MAX: usize = 4096;

pub const USED_MILLIS_MAX: u64 = u64::MAX / 2;

const READ_MILLIS_MAX: u64 = USED_MILLIS_MAX / 2;

const CLOCK_MILLIS_MAX: u64 = 253_402_300_799_999;

const _: () = assert!(CLOCK_MILLIS_MAX < READ_MILLIS_MAX);

const FRESH_TRIES: u32 = 8;

const UNNAMED: &str = "a path holds a part that is no name: empty, `.` or `..`";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(NonZeroU64);

impl Id {
    #[must_use]
    pub fn of(id: u64) -> Option<Id> {
        NonZeroU64::new(id).map(Id)
    }

    #[must_use]
    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub fn random() -> io::Result<Id> {
        let mut urandom = File::open("/dev/urandom")?;
        for _ in 0..FRESH_TRIES {
            let mut bytes = [0; 8];
            urandom.read_exact(&mut bytes)?;
            if let Some(id) = Id::of(u64::from_ne_bytes(bytes)) {
                return Ok(id);
            }
        }
        Err(io::Error::other(format!("/dev/urandom gave {FRESH_TRIES} zeros in a row")))
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

impl FromStr for Id {
    type Err = String;

    fn from_str(text: &str) -> Result<Id, String> {
        let digit = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        let hex = text.len() == 16 && text.bytes().all(digit);
        let parsed = || u64::from_str_radix(text, 16).expect("16 hex digits fit a u64");
        let id = hex.then(parsed).and_then(Id::of);
        id.ok_or_else(|| format!("{text:?} is no id: an id is 16 hex digits, not all 0"))
    }
}

macro_rules! id_of {
    ($kind:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $kind(Id);

        impl $kind {
            #[must_use]
            pub fn new(id: Id) -> $kind {
                $kind(id)
            }

            #[must_use]
            pub fn of(id: u64) -> Option<$kind> {
                Id::of(id).map($kind)
            }

            #[must_use]
            pub fn id(self) -> Id {
                self.0
            }

            #[must_use]
            pub fn get(self) -> u64 {
                self.0.get()
            }
        }

        impl fmt::Display for $kind {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $kind {
            type Err = String;

            fn from_str(text: &str) -> Result<$kind, String> {
                text.parse().map($kind)
            }
        }
    };
}

id_of!(ProjectId);
id_of!(WorkstreamId);
id_of!(TerminalId);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name(String);

impl Name {
    pub fn parse(text: &str) -> Result<Name, String> {
        if text.is_empty() {
            return Err("a name is empty".to_owned());
        }
        if text.trim() != text {
            return Err(format!("the name {text:?} begins or ends with a space"));
        }
        if text.chars().count() > NAME_CHARS_MAX {
            return Err(format!("the name {text:?} is longer than {NAME_CHARS_MAX} characters"));
        }
        if text.chars().any(char::is_control) {
            return Err(format!("the name {text:?} holds a control character"));
        }
        Ok(Name(text.to_owned()))
    }

    pub fn of_project(text: &str) -> Result<Name, String> {
        let name = Name::parse(text)?;
        SessionName::running(text).map_err(|why| format!("{text:?} names no project: {why}"))?;
        Ok(name)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Used(u64);

impl Used {
    #[must_use]
    pub fn at(time: SystemTime) -> Used {
        let since = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
        let millis = u64::try_from(since.as_millis()).unwrap_or(u64::MAX);
        Used::of_millis(millis.min(CLOCK_MILLIS_MAX))
    }

    pub fn read(millis: u64) -> Result<Used, String> {
        if millis <= READ_MILLIS_MAX {
            Ok(Used(millis))
        } else {
            Err(format!("a use at {millis} ms is past {READ_MILLIS_MAX} ms"))
        }
    }

    #[must_use]
    fn of_millis(millis: u64) -> Used {
        assert!(millis <= USED_MILLIS_MAX, "a use at {millis} ms is past {USED_MILLIS_MAX} ms");
        Used(millis)
    }

    #[must_use]
    pub fn millis(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn time(self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.0)
    }

    fn next_millisecond(self) -> Option<Used> {
        (self.0 < USED_MILLIS_MAX).then(|| Used(self.0 + 1))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dir(String);

impl Dir {
    pub fn parse(text: &str) -> Result<Dir, String> {
        if text.len() > PATH_BYTES_MAX {
            return Err(format!("a path is longer than {PATH_BYTES_MAX} bytes"));
        }
        let Some(rest) = text.strip_prefix('/') else {
            return Err(format!("{text:?} is no absolute path"));
        };
        if rest.is_empty() {
            return Err("/ holds every folder, and so is no project's".to_owned());
        }
        Below::parse(rest).map_err(|why| format!("{text:?}: {why}"))?;
        Ok(Dir(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn last(&self) -> &str {
        self.0.rsplit('/').next().expect("rsplit yields at least once")
    }

    #[must_use]
    pub fn below(&self, folder: &Dir) -> Option<Below> {
        if folder == self {
            return Some(Below::OWN);
        }
        let rest = folder.0.strip_prefix(&self.0)?.strip_prefix('/')?;
        assert!(!rest.is_empty(), "a dir ends in no /, and so {folder:?} names a part below it");
        let below = Below(rest.to_owned());
        assert_eq!(&self.join(&below), folder);
        Some(below)
    }

    #[must_use]
    pub fn join(&self, below: &Below) -> Dir {
        if below.is_own() {
            return self.clone();
        }
        Dir(format!("{}/{}", self.0, below.0))
    }

    #[must_use]
    pub fn below_home(path: &str) -> Option<&str> {
        path.strip_prefix('~').filter(|below| below.is_empty() || below.starts_with('/'))
    }

    pub fn found(path: &str) -> Result<Dir, String> {
        Dir::found_in(path, std::env::var("HOME").ok().as_deref())
    }

    fn found_in(path: &str, home: Option<&str>) -> Result<Dir, String> {
        let home = || {
            let unset = format!("{path}: HOME is not set to an absolute path");
            home.filter(|home| home.starts_with('/')).map(str::to_owned).ok_or(unset)
        };
        let path = match Dir::below_home(path) {
            Some(below) => format!("{}{below}", home()?),
            None => path.to_owned(),
        };
        let path = path.as_str();
        if !path.starts_with('/') {
            return Err(format!("{path:?} is no absolute path"));
        }
        let found = fs::canonicalize(path).map_err(|error| format!("{path}: {error}"))?;
        if !found.is_dir() {
            return Err(format!("{path} is no folder"));
        }
        let text = found.to_str().ok_or_else(|| format!("{} is not UTF-8", found.display()))?;
        Dir::parse(text)
    }

    fn nests(&self, other: &Dir) -> bool {
        self.below(other).is_some() || other.below(self).is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Below(String);

impl Below {
    pub const OWN: Below = Below(String::new());

    pub fn parse(text: &str) -> Result<Below, String> {
        if text.is_empty() {
            return Ok(Below::OWN);
        }
        if text.len() > PATH_BYTES_MAX {
            return Err(format!("a path is longer than {PATH_BYTES_MAX} bytes"));
        }
        if text.chars().any(char::is_control) {
            return Err("a path holds a control character".to_owned());
        }
        let mut rebuilt = String::with_capacity(text.len());
        for component in Path::new(text).components() {
            let Component::Normal(part) = component else {
                return Err(UNNAMED.to_owned());
            };
            if !rebuilt.is_empty() {
                rebuilt.push('/');
            }
            rebuilt.push_str(part.to_str().expect("a part of a str is a str"));
        }
        if rebuilt != text {
            return Err(UNNAMED.to_owned());
        }
        Ok(Below(rebuilt))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_own(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    id: ProjectId,
    name: Name,
    dir: Dir,
    workstreams: Vec<Workstream>,
}

impl Project {
    #[must_use]
    pub fn id(&self) -> ProjectId {
        self.id
    }

    #[must_use]
    pub fn name(&self) -> &Name {
        &self.name
    }

    #[must_use]
    pub fn dir(&self) -> &Dir {
        &self.dir
    }

    #[must_use]
    pub fn workstreams(&self) -> &[Workstream] {
        &self.workstreams
    }

    #[must_use]
    pub fn folder(&self, workstream: &Workstream) -> Dir {
        self.dir.join(&workstream.below)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workstream {
    id: WorkstreamId,
    renamed: Option<Name>,
    below: Below,
    view: View,
    used: Option<Used>,
}

impl Workstream {
    #[must_use]
    pub fn id(&self) -> WorkstreamId {
        self.id
    }

    #[must_use]
    pub fn renamed(&self) -> Option<&Name> {
        self.renamed.as_ref()
    }

    #[must_use]
    pub fn below(&self) -> &Below {
        &self.below
    }

    #[must_use]
    pub fn view(&self) -> &View {
        &self.view
    }

    #[must_use]
    pub fn used(&self) -> Option<Used> {
        self.used
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projects {
    projects: Vec<Project>,
}

#[derive(Debug)]
pub enum Change {
    AddProject { dir: Dir, name: Option<Name> },
    AddWorkstream { project: ProjectId, folder: Dir, name: Option<Name> },
    WorkstreamAt { project: ProjectId, folder: Dir },
    RenameProject { project: ProjectId, to: Name },
    RenameWorkstream { workstream: WorkstreamId, to: Option<Name> },
    ReorderProjects { seen: Vec<ProjectId>, projects: Vec<ProjectId> },
    ReorderWorkstreams {
        project: ProjectId,
        seen: Vec<WorkstreamId>,
        workstreams: Vec<WorkstreamId>,
    },
    ForgetProject { project: ProjectId },
    ForgetWorkstream { workstream: WorkstreamId },
    CloseWorkstream { workstream: WorkstreamId },
    Went { workstream: WorkstreamId, at: Used },
    RenameTerminal { terminal: TerminalId, to: Option<Name> },
    Open { workstream: WorkstreamId, terminal: Terminal, placed: Placed, size: Size },
    Close { terminal: TerminalId },
    Unopen { terminal: TerminalId },
    Resize {
        terminal: TerminalId,
        sides: layout::Sides,
        growth: layout::Growth,
        size: Size,
    },
    Swap { one: TerminalId, other: TerminalId },
    Fill { workstream: WorkstreamId, full: Option<TerminalId> },
    Ran(Vec<Ran>),
}

#[derive(Debug)]
pub struct Ran {
    pub terminal: TerminalId,
    pub folder: Dir,
    pub runs: Runs,
}

#[derive(Debug)]
pub enum Ask {
    Change(Change),
    CloseWorkstream { workstream: WorkstreamId, seen: BTreeSet<TerminalId> },
}

impl Ask {
    pub fn of_api(change: api::change_projects_request::Change) -> Result<Ask, String> {
        use api::change_projects_request::{self as asked, Change as Asked};
        let field = |field: &'static str| move |why: String| format!("{field}: {why}");
        let id = |field: &'static str, id: u64| {
            Id::of(id).ok_or_else(|| format!("{field}: no id is 0"))
        };
        let name = |name: Option<String>| name.as_deref().map(Name::parse).transpose();
        let change = match change {
            Asked::AddProject(asked::AddProject { dir, name: named }) => Change::AddProject {
                dir: Dir::found(&dir).map_err(field("dir"))?,
                name: named.as_deref().map(Name::of_project).transpose().map_err(field("name"))?,
            },
            Asked::AddWorkstream(asked::AddWorkstream { project, folder, name: named }) => {
                Change::AddWorkstream {
                    project: id("project", project).map(ProjectId)?,
                    folder: Dir::found(&folder).map_err(field("folder"))?,
                    name: name(named).map_err(field("name"))?,
                }
            },
            Asked::WorkstreamAt(asked::WorkstreamAt { project, folder }) => Change::WorkstreamAt {
                project: id("project", project).map(ProjectId)?,
                folder: Dir::found(&folder).map_err(field("folder"))?,
            },
            Asked::RenameProject(asked::RenameProject { project, to }) => Change::RenameProject {
                project: id("project", project).map(ProjectId)?,
                to: Name::of_project(&to).map_err(field("to"))?,
            },
            Asked::RenameWorkstream(asked::RenameWorkstream { workstream, to }) => {
                Change::RenameWorkstream {
                    workstream: id("workstream", workstream).map(WorkstreamId)?,
                    to: name(to).map_err(field("to"))?,
                }
            },
            Asked::ReorderProjects(asked::ReorderProjects { seen, projects }) => {
                let ids = |field, ids: Vec<u64>| -> Result<Vec<ProjectId>, String> {
                    ids.into_iter().map(|it| id(field, it).map(ProjectId)).collect()
                };
                let (seen, projects) = (ids("seen", seen)?, ids("projects", projects)?);
                Change::ReorderProjects { seen, projects }
            },
            Asked::ReorderWorkstreams(asked::ReorderWorkstreams { project, seen, workstreams }) => {
                let project = id("project", project).map(ProjectId)?;
                let ids = |field, ids: Vec<u64>| -> Result<Vec<WorkstreamId>, String> {
                    ids.into_iter().map(|it| id(field, it).map(WorkstreamId)).collect()
                };
                let (seen, workstreams) = (ids("seen", seen)?, ids("workstreams", workstreams)?);
                Change::ReorderWorkstreams { project, seen, workstreams }
            },
            Asked::ForgetProject(asked::ForgetProject { project }) => {
                Change::ForgetProject { project: id("project", project).map(ProjectId)? }
            },
            Asked::ForgetWorkstream(asked::ForgetWorkstream { workstream }) => {
                let workstream = id("workstream", workstream).map(WorkstreamId)?;
                Change::ForgetWorkstream { workstream }
            },
            Asked::CloseWorkstream(asked::CloseWorkstream { workstream, seen }) => {
                let workstream = id("workstream", workstream).map(WorkstreamId)?;
                let seen = seen.into_iter().map(|it| id("seen", it).map(TerminalId));
                let seen = seen.collect::<Result<BTreeSet<TerminalId>, String>>()?;
                return Ok(Ask::CloseWorkstream { workstream, seen });
            },
            Asked::RenameTerminal(asked::RenameTerminal { terminal, to }) => {
                Change::RenameTerminal {
                    terminal: id("terminal", terminal).map(TerminalId)?,
                    to: name(to).map_err(field("to"))?,
                }
            },
        };
        Ok(Ask::Change(change))
    }
}

impl Change {
    #[must_use]
    pub fn kept_at_once(&self) -> bool {
        match self {
            Change::AddProject { .. }
            | Change::AddWorkstream { .. }
            | Change::WorkstreamAt { .. }
            | Change::RenameProject { .. }
            | Change::RenameWorkstream { .. }
            | Change::ReorderProjects { .. }
            | Change::ReorderWorkstreams { .. }
            | Change::ForgetProject { .. }
            | Change::ForgetWorkstream { .. }
            | Change::CloseWorkstream { .. }
            | Change::RenameTerminal { .. }
            | Change::Open { .. }
            | Change::Close { .. }
            | Change::Unopen { .. } => true,
            Change::Went { .. }
            | Change::Resize { .. }
            | Change::Swap { .. }
            | Change::Fill { .. }
            | Change::Ran(_) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    NoBranch,
    Working,
    PullRequestOpen,
    ChecksFailing,
    Merged,
}

impl Stage {
    #[must_use]
    pub fn of(branch: Option<&str>, pull_request: Option<&PullRequest>) -> Stage {
        if branch.is_none() {
            return Stage::NoBranch;
        }
        let Some(pull_request) = pull_request else {
            return Stage::Working;
        };
        match (pull_request.state, pull_request.verdict()) {
            (State::Merged, _) => Stage::Merged,
            (State::Closed, _) => Stage::Working,
            (State::Open | State::Draft, Some(Verdict::Failing)) => Stage::ChecksFailing,
            (State::Open | State::Draft, Some(Verdict::Pending | Verdict::Passing) | None) => {
                Stage::PullRequestOpen
            },
        }
    }

    #[must_use]
    pub fn to_api(self) -> api::Stage {
        match self {
            Stage::NoBranch => api::Stage::NoBranch,
            Stage::Working => api::Stage::Working,
            Stage::PullRequestOpen => api::Stage::PullRequestOpen,
            Stage::ChecksFailing => api::Stage::ChecksFailing,
            Stage::Merged => api::Stage::Merged,
        }
    }

    #[must_use]
    pub fn of_api(stage: api::Stage) -> Option<Stage> {
        match stage {
            api::Stage::Unspecified => None,
            api::Stage::NoBranch => Some(Stage::NoBranch),
            api::Stage::Working => Some(Stage::Working),
            api::Stage::PullRequestOpen => Some(Stage::PullRequestOpen),
            api::Stage::ChecksFailing => Some(Stage::ChecksFailing),
            api::Stage::Merged => Some(Stage::Merged),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Missing,
    Unread,
    Read(Stage),
}

impl Seen {
    #[must_use]
    pub fn to_api(self) -> api::workstream::Seen {
        match self {
            Seen::Missing => api::workstream::Seen::Missing(()),
            Seen::Unread => api::workstream::Seen::Unread(()),
            Seen::Read(stage) => api::workstream::Seen::Stage(stage.to_api().into()),
        }
    }

    #[must_use]
    pub fn of_api(seen: api::workstream::Seen) -> Option<Seen> {
        match seen {
            api::workstream::Seen::Missing(()) => Some(Seen::Missing),
            api::workstream::Seen::Unread(()) => Some(Seen::Unread),
            api::workstream::Seen::Stage(stage) => {
                let stage = api::Stage::try_from(stage).ok()?;
                Some(Seen::Read(Stage::of_api(stage)?))
            },
        }
    }
}

impl Projects {
    pub fn checked(projects: Vec<Project>) -> Result<Projects, String> {
        if projects.len() > PROJECTS_MAX {
            return Err(format!("it holds over {PROJECTS_MAX} projects"));
        }
        let projects = Projects { projects };
        if projects.workstream_count() > WORKSTREAMS_MAX {
            return Err(format!("it holds over {WORKSTREAMS_MAX} workstreams"));
        }
        if let Some(long) = projects.workstream_folders().find(|it| it.0.len() > PATH_BYTES_MAX) {
            return Err(format!("{}: a path is longer than {PATH_BYTES_MAX} bytes", long.0));
        }
        if projects.terminal_count() > TERMINALS_MAX {
            return Err(format!("it holds over {TERMINALS_MAX} terminals"));
        }
        let Projects { projects } = projects;
        let mut ids = BTreeSet::new();
        for (at, project) in projects.iter().enumerate() {
            let workstreams = project.workstreams.iter().map(|workstream| workstream.id.0);
            let terminals = project.workstreams.iter().flat_map(|workstream| {
                workstream.view.terminals().map(|terminal| layout::Leaf::id(terminal).0)
            });
            for id in std::iter::once(project.id.0).chain(workstreams).chain(terminals) {
                if !ids.insert(id) {
                    return Err(format!("the id {id} comes twice"));
                }
            }
            if let Some(other) = projects[..at].iter().find(|other| other.dir.nests(&project.dir)) {
                return Err(nested(&project.dir, other));
            }
        }
        Ok(Projects { projects })
    }

    #[must_use]
    pub fn all(&self) -> &[Project] {
        &self.projects
    }

    pub fn folders(&self) -> impl Iterator<Item = Dir> + '_ {
        self.projects.iter().map(|project| project.dir.clone()).chain(self.workstream_folders())
    }

    pub fn workstream_folders(&self) -> impl Iterator<Item = Dir> + '_ {
        self.projects.iter().flat_map(|project| {
            project.workstreams.iter().map(|workstream| project.folder(workstream))
        })
    }

    pub fn changed(
        &self,
        change: Change,
        draw: impl FnMut() -> io::Result<Id>,
    ) -> Result<(Projects, Option<Id>), Refused> {
        if let Change::AddProject { dir, .. } = &change
            && let Some(other) = self.projects.iter().find(|other| other.dir.nests(dir))
        {
            return Err(Refused::Nests(nested(dir, other)));
        }
        if let Some(why) = self.stale(&change) {
            return Err(Refused::Stale(why));
        }
        self.changed_by(change, draw).map_err(Refused::Said)
    }

    fn stale(&self, change: &Change) -> Option<String> {
        match change {
            Change::ReorderProjects { seen, .. } => {
                stale(self.projects.iter().map(Project::id), seen, "projects")
            },
            Change::ReorderWorkstreams { project, seen, .. } => {
                let project = self.projects.iter().find(|it| it.id == *project)?;
                stale(project.workstreams.iter().map(Workstream::id), seen, "workstreams")
            },
            _ => None,
        }
    }

    fn changed_by(
        &self,
        change: Change,
        mut draw: impl FnMut() -> io::Result<Id>,
    ) -> Result<(Projects, Option<Id>), String> {
        let mut changed = self.clone();
        let made = match change {
            Change::AddProject { dir, name } => {
                let name = match name {
                    Some(name) => name,
                    None => Name::parse(dir.last()).map_err(|why| format!("name it: {why}"))?,
                };
                if self.projects.len() >= PROJECTS_MAX {
                    return Err(format!("there are {PROJECTS_MAX} projects, as many as are kept"));
                }
                let id = self.fresh(&mut draw)?;
                let project = Project { id: ProjectId(id), name, dir, workstreams: Vec::new() };
                changed.projects.push(project);
                Some(id)
            },
            Change::AddWorkstream { project, folder, name } => {
                if self.workstream_count() >= WORKSTREAMS_MAX {
                    let many = format!("there are {WORKSTREAMS_MAX} workstreams");
                    return Err(format!("{many}, as many as are kept"));
                }
                let project = changed.project(project)?;
                let below = project.dir.below(&folder).ok_or_else(|| {
                    format!("{} is not in {}, the project's folder", folder.0, project.dir.0)
                })?;
                let id = self.fresh(&mut draw)?;
                let (view, used) = (View::default(), None);
                let made = WorkstreamId(id);
                let workstream = Workstream { id: made, renamed: name, below, view, used };
                project.workstreams.push(workstream);
                Some(id)
            },
            Change::WorkstreamAt { project, folder } => {
                let held = changed.project(project)?;
                let at = held.workstreams.iter().find(|it| held.folder(it) == folder);
                let Some(found) = at else {
                    let add = Change::AddWorkstream { project, folder, name: None };
                    return self.changed_by(add, draw);
                };
                Some(found.id.0)
            },
            Change::RenameProject { project, to } => {
                changed.project(project)?.name = to;
                None
            },
            Change::RenameWorkstream { workstream, to } => {
                let (project, at) = changed.workstream(workstream)?;
                project.workstreams[at].renamed = to;
                None
            },
            Change::ReorderProjects { projects, .. } => {
                reordered(&mut changed.projects, |it| it.id, &projects, "projects")?;
                None
            },
            Change::ReorderWorkstreams { project, workstreams, .. } => {
                let held = &mut changed.project(project)?.workstreams;
                reordered(held, |it| it.id, &workstreams, "workstreams")?;
                None
            },
            Change::ForgetProject { project } => {
                let held = changed.project(project)?.workstreams.len();
                if held > 0 {
                    return Err(format!("project {project} holds {held} workstreams still"));
                }
                changed.projects.retain(|it| it.id != project);
                None
            },
            Change::ForgetWorkstream { workstream } => {
                let (project, at) = changed.workstream(workstream)?;
                let held = project.workstreams[at].view.len();
                if held > 0 {
                    return Err(format!("workstream {workstream} holds {held} terminals still"));
                }
                project.workstreams.remove(at);
                None
            },
            Change::CloseWorkstream { workstream } => {
                let (project, at) = changed.workstream(workstream)?;
                let closed = project.workstreams.remove(at);
                for terminal in closed.view.terminals() {
                    let id = layout::Leaf::id(terminal);
                    assert!(!changed.holds(id.0), "terminal {id} goes with its workstream");
                }
                None
            },
            Change::Went { workstream, at } => {
                let others = self.workstreams().filter(|(_, it)| it.id != workstream);
                let newest = others.filter_map(|(_, it)| it.used).max();
                let (project, place) = changed.workstream(workstream)?;
                let went = &mut project.workstreams[place];
                let last = went.used.is_some_and(|used| newest.is_none_or(|newest| used > newest));
                if !last {
                    let past = || format!("no workstream is used past {USED_MILLIS_MAX} ms");
                    let at = match newest {
                        None => at,
                        Some(newest) => at.max(newest.next_millisecond().ok_or_else(past)?),
                    };
                    assert!(went.used.is_none_or(|used| used < at), "a stamp is never set back");
                    went.used = Some(at);
                }
                let used = went.used.expect("a workstream gone to is used");
                if let Some(newest) = newest {
                    assert!(used > newest, "workstream {workstream}, gone to last, is used last");
                }
                None
            },
            Change::RenameTerminal { terminal, to } => {
                let view = changed.view_of(terminal)?;
                view.terminal_mut(terminal).expect("its view holds it").renamed(to);
                None
            },
            Change::Open { workstream, terminal, placed, size } => {
                let id = layout::Leaf::id(&terminal);
                if self.holds(id.0) {
                    return Err(format!("the id {id} is taken"));
                }
                if self.terminal_count() >= TERMINALS_MAX {
                    let many = format!("there are {TERMINALS_MAX} terminals");
                    return Err(format!("{many}, as many as are kept"));
                }
                let (project, at) = changed.workstream(workstream)?;
                project.workstreams[at].view.opened(terminal, placed, size)?;
                None
            },
            Change::Close { terminal } => {
                let held_in = self.holding(terminal).map(|(workstream, _)| workstream.id());
                let workstream = held_in.ok_or_else(|| unknown("terminal", terminal))?;
                let (project, at) = changed.workstream(workstream)?;
                if project.workstreams[at].view.closed(terminal).is_none() {
                    project.workstreams.remove(at);
                    assert!(changed.find(workstream).is_none(), "{workstream} goes with its last");
                }
                assert!(!changed.holds(terminal.0), "terminal {terminal} left its view");
                None
            },
            Change::Unopen { terminal } => {
                let held_in = self.holding(terminal).map(|(workstream, _)| workstream.id());
                let workstream = held_in.ok_or_else(|| unknown("terminal", terminal))?;
                changed.view_of(terminal)?.closed(terminal);
                assert!(changed.find(workstream).is_some(), "{workstream} stays, if empty");
                assert!(!changed.holds(terminal.0), "terminal {terminal} left its view");
                None
            },
            Change::Resize { terminal, sides, growth, size } => {
                changed.view_of(terminal)?.resized(terminal, sides, growth, size)?;
                None
            },
            Change::Swap { one, other } => {
                let view = changed.view_of(one)?;
                if !view.holds(other) {
                    return Err(format!("terminals {one} and {other} are in two views"));
                }
                view.swapped(one, other)?;
                None
            },
            Change::Fill { workstream, full } => {
                let (project, at) = changed.workstream(workstream)?;
                project.workstreams[at].view.filled(full)?;
                None
            },
            Change::Ran(ran) => {
                for Ran { terminal, folder, runs } in ran {
                    let view = changed.view_of(terminal)?;
                    view.terminal_mut(terminal).expect("its view holds it").ran(folder, runs);
                }
                None
            },
        };
        assert_eq!(Projects::checked(changed.projects.clone()).as_ref(), Ok(&changed));
        Ok((changed, made))
    }

    pub fn fresh(&self, draw: &mut impl FnMut() -> io::Result<Id>) -> Result<Id, String> {
        for _ in 0..FRESH_TRIES {
            let id = draw().map_err(|error| format!("no id is drawn: {error}"))?;
            if !self.holds(id) {
                return Ok(id);
            }
        }
        Err(format!("{FRESH_TRIES} ids drawn were all taken"))
    }

    fn workstream_count(&self) -> usize {
        self.projects.iter().map(|project| project.workstreams.len()).sum()
    }

    fn terminal_count(&self) -> usize {
        self.workstreams().map(|(_, workstream)| workstream.view.len()).sum()
    }

    pub fn workstreams(&self) -> impl Iterator<Item = (&Project, &Workstream)> {
        self.projects
            .iter()
            .flat_map(|project| project.workstreams.iter().map(move |it| (project, it)))
    }

    #[must_use]
    pub fn find(&self, workstream: WorkstreamId) -> Option<(&Project, &Workstream)> {
        self.workstreams().find(|(_, it)| it.id == workstream)
    }

    #[must_use]
    pub fn holding(&self, terminal: TerminalId) -> Option<(&Workstream, &Terminal)> {
        self.workstreams().find_map(|(_, workstream)| {
            let tree = workstream.view.tree()?;
            Some((workstream, tree.get(terminal)?))
        })
    }

    pub fn terminals(&self) -> impl Iterator<Item = (&Workstream, &Terminal)> {
        self.workstreams()
            .flat_map(|(_, workstream)| workstream.view.terminals().map(move |it| (workstream, it)))
    }

    #[must_use]
    pub fn holds(&self, id: Id) -> bool {
        self.projects.iter().any(|project| {
            project.id.0 == id
                || project.workstreams.iter().any(|workstream| {
                    workstream.id.0 == id || workstream.view.holds(TerminalId(id))
                })
        })
    }

    fn view_of(&mut self, terminal: TerminalId) -> Result<&mut View, String> {
        self.projects
            .iter_mut()
            .flat_map(|project| project.workstreams.iter_mut())
            .map(|workstream| &mut workstream.view)
            .find(|view| view.holds(terminal))
            .ok_or_else(|| unknown("terminal", terminal))
    }

    fn project(&mut self, id: ProjectId) -> Result<&mut Project, String> {
        let project = self.projects.iter_mut().find(|project| project.id == id);
        project.ok_or_else(|| unknown("project", id))
    }

    fn workstream(&mut self, id: WorkstreamId) -> Result<(&mut Project, usize), String> {
        self.projects
            .iter_mut()
            .find_map(|project| {
                let at = project.workstreams.iter().position(|it| it.id == id)?;
                Some((project, at))
            })
            .ok_or_else(|| unknown("workstream", id))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    Nests(String),
    Stale(String),
    Said(String),
}

impl fmt::Display for Refused {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Nests(why) | Refused::Stale(why) | Refused::Said(why) => {
                formatter.write_str(why)
            },
        }
    }
}

fn nested(dir: &Dir, other: &Project) -> String {
    let (other_dir, name) = (&other.dir.0, &other.name.0);
    if *dir == other.dir {
        return format!("{} is project {name}'s folder already", dir.0);
    }
    if other.dir.below(dir).is_some() {
        return format!("{} is inside {other_dir}, project {name}'s folder", dir.0);
    }
    format!("{} holds {other_dir}, project {name}'s folder", dir.0)
}

fn unknown(what: &str, id: impl fmt::Display) -> String {
    format!("no {what} has the id {id}")
}

fn stale<I: Eq + Copy>(kept: impl Iterator<Item = I>, seen: &[I], kind: &str) -> Option<String> {
    let fresh = kept.eq(seen.iter().copied());
    (!fresh).then(|| format!("the {kind} kept are not in the order seen: they changed since"))
}

fn reordered<T, I: Ord + Copy>(
    items: &mut [T],
    id: impl Fn(&T) -> I,
    wanted: &[I],
    kind: &str,
) -> Result<(), String> {
    let mut kept: Vec<I> = items.iter().map(&id).collect();
    let mut asked = wanted.to_vec();
    kept.sort_unstable();
    asked.sort_unstable();
    assert!(kept.windows(2).all(|pair| pair[0] < pair[1]), "the {kind} kept have no id twice");
    if kept != asked {
        return Err(format!("the {kind} asked for are not the {kind} kept"));
    }
    items.sort_by_key(|it| wanted.iter().position(|wanted| *wanted == id(it)));
    assert!(items.iter().map(&id).eq(wanted.iter().copied()), "the {kind} are as asked");
    Ok(())
}

pub fn follow(core: SyncSender<Command>, sweep: Duration) -> io::Result<()> {
    thread::Builder::new().name("folders".to_owned()).spawn(move || {
        let mut troubles = Troubles::default();
        loop {
            let Some(folders) = core::ask(&core, Command::ProjectFolders) else {
                return;
            };
            let mut missing = BTreeSet::new();
            for folder in folders {
                match gone(&folder) {
                    Ok(true) => {
                        missing.insert(folder);
                    },
                    Ok(false) => {},
                    Err(error) => {
                        let folder = folder.as_str();
                        let why = format!("{folder}: {error}, and so it is not said to be missing");
                        troubles.say(why);
                    },
                }
            }
            troubles.done();
            if core.send(Command::Missing(missing)).is_err() {
                return;
            }
            thread::sleep(sweep);
        }
    })?;
    Ok(())
}

fn gone(folder: &Dir) -> io::Result<bool> {
    match fs::metadata(folder.as_str()) {
        Ok(metadata) => Ok(!metadata.is_dir()),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            Ok(true)
        },
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::os::unix::fs::PermissionsExt as _;

    use luneta_protocol::github::Checks;

    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn counting() -> impl FnMut() -> io::Result<Id> {
        let mut next = 0;
        move || {
            next += 1;
            Ok(Id::of(next).unwrap())
        }
    }

    fn refused(projects: &Projects, change: Change) -> Result<(Projects, Option<Id>), Refused> {
        let mut drawn = counting();
        let taken = |id: &Id| projects.holds(*id);
        projects.changed(change, || loop {
            let id = drawn()?;
            if !taken(&id) {
                return Ok(id);
            }
        })
    }

    fn change(projects: &Projects, change: Change) -> Result<(Projects, Option<Id>), String> {
        refused(projects, change).map_err(|refused| refused.to_string())
    }

    fn add(projects: &mut Projects, change_of: Change) -> Id {
        let (changed, made) = change(projects, change_of).unwrap();
        *projects = changed;
        made.unwrap()
    }

    fn project(projects: &mut Projects, at: &str) -> ProjectId {
        ProjectId(add(projects, Change::AddProject { dir: dir(at), name: None }))
    }

    fn workstream(projects: &mut Projects, project: ProjectId, at: &str) -> WorkstreamId {
        WorkstreamId(add(projects, Change::AddWorkstream { project, folder: dir(at), name: None }))
    }

    #[test]
    fn a_name_is_said_text_of_bounded_length() {
        assert_eq!(Name::parse("bitwarden backup").unwrap().as_str(), "bitwarden backup");
        assert!(Name::parse(&"a".repeat(NAME_CHARS_MAX)).is_ok());
        for refused in ["", " a", "a ", "a\nb", &"a".repeat(NAME_CHARS_MAX + 1)] {
            assert!(Name::parse(refused).is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_project_s_name_is_one_a_session_may_have() {
        assert_eq!(Name::of_project("bitwarden backup"), Name::parse("bitwarden backup"));
        assert!(Name::of_project("a\nb").is_err());
        for listed_by_none in ["api/v2", ".", ".."] {
            assert!(Name::parse(listed_by_none).is_ok(), "{listed_by_none:?}");
            let refused = Name::of_project(listed_by_none).unwrap_err();
            assert!(refused.contains("names no project"), "{refused}");
        }
    }

    #[test]
    fn a_dir_is_an_absolute_path_made_only_of_names() {
        assert_eq!(dir("/home/me/luneta").last(), "luneta");
        for refused in ["/", "home", "/a/", "/a//b", "/a/./b", "/a/../b", "/a\tb"] {
            assert!(Dir::parse(refused).is_err(), "{refused:?}");
        }
        assert_eq!(Below::parse(""), Ok(Below::OWN));
        assert!(Below::parse("/a").is_err());
    }

    #[test]
    fn a_folder_asked_for_is_found_where_it_really_is() {
        let scratch = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(scratch.path()).unwrap().join("real");
        fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, real.with_file_name("link")).unwrap();
        let link = real.with_file_name("link");
        assert_eq!(Dir::found(link.to_str().unwrap()).unwrap().as_str(), real.to_str().unwrap());
        fs::write(real.join("file"), "").unwrap();
        let file = real.join("file");
        assert!(Dir::found(file.to_str().unwrap()).unwrap_err().ends_with("is no folder"));
        assert!(Dir::found("real").unwrap_err().contains("no absolute path"));
        assert!(Dir::found("~me/real").unwrap_err().contains("no absolute path"));
        let home = real.to_str();
        for at in ["~", "~/"] {
            assert_eq!(Dir::found_in(at, home).unwrap().as_str(), real.to_str().unwrap());
        }
        let below = Dir::found_in("~/file", home).unwrap_err();
        assert!(below.ends_with("is no folder"), "{below}");
        let unset = Dir::found_in("~", None).unwrap_err();
        assert!(unset.contains("HOME is not set"), "{unset}");
        let nowhere = real.join("nowhere");
        assert!(Dir::found(nowhere.to_str().unwrap()).unwrap_err().contains("No such file"));
    }

    #[test]
    fn a_folder_is_below_a_dir_only_by_whole_names() {
        let luneta = dir("/w/luneta");
        assert_eq!(luneta.below(&dir("/w/luneta")), Some(Below::OWN));
        assert_eq!(luneta.join(&Below::OWN), luneta);
        assert_eq!(luneta.below(&dir("/w/luneta/issue-1/src")).unwrap().as_str(), "issue-1/src");
        assert_eq!(luneta.below(&dir("/w/luneta-old")), None);
        assert_eq!(luneta.below(&dir("/w")), None);
        assert_eq!(luneta.join(&luneta.below(&dir("/w/luneta/a")).unwrap()), dir("/w/luneta/a"));
    }

    #[test]
    fn an_id_reads_back_as_it_is_written() {
        let id = Id::random().unwrap();
        assert_eq!(id.to_string().parse::<Id>(), Ok(id));
        assert_eq!(Id::of(0x1f).unwrap().to_string(), "000000000000001f");
        for refused in ["0000000000000000", "1f", "000000000000001F", "+00000000000001f"] {
            assert!(refused.parse::<Id>().is_err(), "{refused:?}");
        }
    }

    #[test]
    fn a_project_is_named_after_its_folder_until_named_otherwise() {
        let mut projects = Projects::default();
        let vault = project(&mut projects, "/home/me/Documents/Vault");
        assert_eq!(projects.all()[0].name().as_str(), "Vault");
        let to = Name::parse("bitwarden backup").unwrap();
        (projects, _) = change(&projects, Change::RenameProject { project: vault, to }).unwrap();
        assert_eq!(projects.all()[0].name().as_str(), "bitwarden backup");
        assert_eq!(projects.all()[0].dir().as_str(), "/home/me/Documents/Vault");
    }

    #[test]
    fn projects_never_nest() {
        let mut projects = Projects::default();
        project(&mut projects, "/w/luneta");
        for (at, why) in [
            ("/w/luneta", "is project luneta's folder already"),
            ("/w/luneta/main", "is inside /w/luneta"),
            ("/w", "holds /w/luneta"),
        ] {
            let refused = refused(&projects, Change::AddProject { dir: dir(at), name: None });
            let Err(Refused::Nests(said)) = refused else {
                panic!("{at} is refused as nested: {refused:?}");
            };
            assert!(said.contains(why), "{at}: {said}");
        }
        project(&mut projects, "/w/luneta-old");
        assert_eq!(projects.all().len(), 2);
    }

    #[test]
    fn a_workstream_is_below_its_project_s_folder_and_may_share_one() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let again = workstream(&mut projects, luneta, "/w/luneta/main");
        let deep = workstream(&mut projects, luneta, "/w/luneta/main/src");
        assert_eq!(projects.all()[0].workstreams().len(), 3);
        assert!(main != again && again != deep);
        let folders: Vec<Dir> = projects.folders().collect();
        let want = ["/w/luneta", "/w/luneta/main", "/w/luneta/main", "/w/luneta/main/src"].map(dir);
        assert_eq!(folders, want);
        let outside = Change::AddWorkstream { project: luneta, folder: dir("/w/elo"), name: None };
        assert!(change(&projects, outside).unwrap_err().contains("is not in /w/luneta"));
    }

    #[test]
    fn a_workstream_at_a_folder_is_the_first_there_or_else_one_made_there() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        workstream(&mut projects, luneta, "/w/luneta/main");
        let at = |folder: &str| Change::WorkstreamAt { project: luneta, folder: dir(folder) };
        let (same, found) = change(&projects, at("/w/luneta/main")).unwrap();
        assert_eq!((same == projects, found), (true, Some(main.0)));
        let made = WorkstreamId(add(&mut projects, at("/w/luneta/feat")));
        assert_eq!(projects.all()[0].workstreams().len(), 3);
        let (_, again) = change(&projects, at("/w/luneta/feat")).unwrap();
        assert_eq!(again, Some(made.0));
        assert!(change(&projects, at("/w/elo")).unwrap_err().contains("is not in /w/luneta"));
    }

    #[test]
    fn a_workstream_asked_for_in_its_project_s_own_folder_is_made_there() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        assert_eq!(projects.all()[0].workstreams(), []);
        let own = workstream(&mut projects, luneta, "/w/luneta");
        let held = &projects.all()[0];
        assert!(held.workstreams()[0].below().is_own());
        assert_eq!(held.folder(&held.workstreams()[0]), dir("/w/luneta"));
        let at = Change::WorkstreamAt { project: luneta, folder: dir("/w/luneta") };
        let (same, found) = change(&projects, at).unwrap();
        assert_eq!((same == projects, found), (true, Some(own.0)));
    }

    #[test]
    fn a_rename_of_a_workstream_is_kept_until_taken_back() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let to = Some(Name::parse("release").unwrap());
        let rename = Change::RenameWorkstream { workstream: main, to };
        (projects, _) = change(&projects, rename).unwrap();
        let renamed = projects.all()[0].workstreams()[0].renamed();
        assert_eq!(renamed.map(Name::as_str), Some("release"));
        let back = Change::RenameWorkstream { workstream: main, to: None };
        (projects, _) = change(&projects, back).unwrap();
        assert_eq!(projects.all()[0].workstreams()[0].renamed(), None);
    }

    #[test]
    fn a_reorder_takes_the_whole_order_and_only_on_the_order_kept() {
        let mut projects = Projects::default();
        let a = project(&mut projects, "/a");
        let b = project(&mut projects, "/b");
        let c = project(&mut projects, "/c");
        let order = |projects: &Projects| {
            projects.all().iter().map(Project::id).collect::<Vec<_>>()
        };
        let reorder = Change::ReorderProjects { seen: vec![a, b, c], projects: vec![b, c, a] };
        (projects, _) = change(&projects, reorder).unwrap();
        assert_eq!(order(&projects), [b, c, a]);
        let is_stale = |it| matches!(it, Err(Refused::Stale(why)) if why.contains("since"));
        for seen in [vec![a, b, c], vec![b, c], vec![b, c, a, a], vec![]] {
            let stale = Change::ReorderProjects { seen, projects: vec![c, a, b] };
            assert!(is_stale(refused(&projects, stale)));
        }
        for wanted in [vec![a, b], vec![a, b, b], vec![c, b, a, a]] {
            let unlike = Change::ReorderProjects { seen: vec![b, c, a], projects: wanted };
            let refused = change(&projects, unlike).unwrap_err();
            assert!(refused.contains("asked for are not the projects kept"), "{refused}");
        }
        assert_eq!(order(&projects), [b, c, a]);

        let one = workstream(&mut projects, a, "/a/one");
        let two = workstream(&mut projects, a, "/a/two");
        let swap = |project, seen| Change::ReorderWorkstreams {
            project,
            seen,
            workstreams: vec![two, one],
        };
        (projects, _) = change(&projects, swap(a, vec![one, two])).unwrap();
        let ids: Vec<_> = projects.all()[2].workstreams().iter().map(Workstream::id).collect();
        assert_eq!(ids, [two, one]);
        assert!(is_stale(refused(&projects, swap(a, vec![one, two]))));
        assert!(is_stale(refused(&projects, swap(b, vec![one, two]))));
        let unknown = ProjectId::of(99).unwrap();
        let nowhere = refused(&projects, swap(unknown, vec![one, two]));
        assert!(matches!(nowhere, Err(Refused::Said(why)) if why.contains("no project")));
        let elsewhere = change(&projects, swap(b, vec![])).unwrap_err();
        assert!(elsewhere.contains("asked for are not the workstreams kept"), "{elsewhere}");
    }

    #[test]
    fn a_project_is_forgotten_only_once_it_holds_no_workstream() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let refused = change(&projects, Change::ForgetProject { project: luneta });
        assert!(refused.unwrap_err().contains("holds 1 workstreams still"));
        (projects, _) = change(&projects, Change::ForgetWorkstream { workstream: main }).unwrap();
        (projects, _) = change(&projects, Change::ForgetProject { project: luneta }).unwrap();
        assert_eq!(projects, Projects::default());
        let gone = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(gone.unwrap_err().contains("no workstream has the id"));
    }

    #[test]
    fn a_drawn_id_already_taken_is_drawn_again() {
        let mut projects = Projects::default();
        let first = project(&mut projects, "/a");
        let mut draws = [first.0, first.0, Id::of(7).unwrap()].into_iter();
        let change = Change::AddProject { dir: dir("/b"), name: None };
        let (_, made) = projects.changed(change, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(made, Id::of(7));
        let always = Change::AddProject { dir: dir("/b"), name: None };
        let refused = projects.changed(always, || Ok(first.0));
        assert!(refused.unwrap_err().to_string().contains("were all taken"));
    }

    #[test]
    fn the_projects_and_workstreams_kept_are_bounded() {
        let mut projects = Projects::default();
        for at in 0..PROJECTS_MAX {
            project(&mut projects, &format!("/p{at}"));
        }
        let over = refused(&projects, Change::AddProject { dir: dir("/over"), name: None });
        let Err(Refused::Said(said)) = over else {
            panic!("a project over the bound is no nested one: {over:?}");
        };
        assert!(said.contains("as many as are kept"));
        let first = projects.all()[0].id();
        for _ in 0..WORKSTREAMS_MAX {
            workstream(&mut projects, first, "/p0/w");
        }
        let over = Change::AddWorkstream { project: first, folder: dir("/p0/w"), name: None };
        assert!(change(&projects, over).unwrap_err().contains("as many as are kept"));
    }

    #[test]
    fn a_folder_is_gone_only_when_nothing_or_no_folder_is_there() {
        let scratch = tempfile::tempdir().unwrap();
        let at = |path: &Path| dir(path.to_str().unwrap());
        let real = fs::canonicalize(scratch.path()).unwrap();
        let (file, locked) = (real.join("file"), real.join("locked"));
        fs::write(&file, "").unwrap();
        fs::create_dir_all(locked.join("inner")).unwrap();
        assert!(!gone(&at(&real)).unwrap());
        assert!(gone(&at(&file)).unwrap());
        assert!(gone(&at(&real.join("nowhere"))).unwrap());
        assert!(gone(&at(&file.join("below"))).unwrap());
        let permissions = |mode| fs::Permissions::from_mode(mode);
        fs::set_permissions(&locked, permissions(0o000)).unwrap();
        let unread = gone(&at(&locked.join("inner")));
        fs::set_permissions(&locked, permissions(0o755)).unwrap();
        assert_eq!(unread.unwrap_err().kind(), ErrorKind::PermissionDenied);
    }

    fn opened(
        projects: &mut Projects,
        workstream: WorkstreamId,
        id: u64,
        placed: Placed,
    ) -> TerminalId {
        let terminal = TerminalId::of(id).unwrap();
        let made = Terminal::new(terminal, dir("/w/luneta"), Runs::Shell);
        let size = Size::of(80, 24).unwrap();
        let open = Change::Open { workstream, terminal: made, placed, size };
        (*projects, _) = change(projects, open).unwrap();
        terminal
    }

    fn view(projects: &Projects, workstream: WorkstreamId) -> &View {
        projects.find(workstream).unwrap().1.view()
    }

    #[test]
    fn a_view_opens_closes_and_fills_itself_with_one_of_its_terminals() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        assert!(view(&projects, main).is_empty());
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, main, 101, right);
        assert_eq!(view(&projects, main).tree().unwrap().panes(), [one, two]);
        assert_eq!(projects.holding(two).map(|(it, _)| it.id()), Some(main));

        (projects, _) = change(&projects, Change::Swap { one, other: two }).unwrap();
        assert_eq!(view(&projects, main).tree().unwrap().panes(), [two, one]);
        let fill = |full| Change::Fill { workstream: main, full };
        (projects, _) = change(&projects, fill(Some(two))).unwrap();
        assert_eq!(view(&projects, main).full(), Some(two));
        let swap = change(&projects, Change::Swap { one, other: two });
        assert!(swap.unwrap_err().contains("moves none"), "a full view moves no terminal");
        let (sides, grow) = (layout::Sides::One(layout::Direction::Left), layout::Growth::Grow);
        let size = Size::of(80, 24).unwrap();
        let resize = Change::Resize { terminal: one, sides, growth: grow, size };
        assert!(change(&projects, resize).unwrap_err().contains("resized by none"));
        let gone = TerminalId::of(7).unwrap();
        assert!(change(&projects, fill(Some(gone))).unwrap_err().contains("holds no terminal"));

        (projects, _) = change(&projects, Change::Close { terminal: two }).unwrap();
        assert_eq!(view(&projects, main).full(), None, "a view of one fills itself with none");
        assert!(change(&projects, fill(Some(one))).unwrap_err().contains("whole already"));
        let refused = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(refused.unwrap_err().contains("holds 1 terminals still"));
        (projects, _) = change(&projects, Change::Close { terminal: one }).unwrap();
        assert_eq!(projects.all()[0].workstreams(), [], "a workstream goes with its last terminal");
    }

    #[test]
    fn a_view_s_last_terminal_closes_with_its_workstream_and_no_other_does() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let kept = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, main, 101, right);
        (projects, _) = change(&projects, Change::Close { terminal: one }).unwrap();
        let held_in = projects.holding(two).map(|(it, _)| it.id());
        assert_eq!(held_in, Some(main), "a terminal not its view's last leaves the workstream");
        (projects, _) = change(&projects, Change::Close { terminal: two }).unwrap();
        let left = projects.all()[0].workstreams().iter().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [kept]);
        assert_eq!(projects.terminals().count(), 0);
        let gone = change(&projects, Change::Close { terminal: two });
        assert!(gone.unwrap_err().contains("no terminal has the id"));
    }

    #[test]
    fn an_open_taken_back_leaves_its_workstream_even_empty() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        (projects, _) = change(&projects, Change::Unopen { terminal: one }).unwrap();
        assert!(view(&projects, main).is_empty());
        let left = projects.all()[0].workstreams().iter().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [main], "a workstream an open failed in stays");
        let gone = change(&projects, Change::Unopen { terminal: one });
        assert!(gone.unwrap_err().contains("no terminal has the id"));
    }

    #[test]
    fn a_workstream_closes_with_its_terminals_where_a_forget_is_refused() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let kept = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        opened(&mut projects, main, 101, right);
        let forget = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(forget.unwrap_err().contains("holds 2 terminals still"));
        (projects, _) = change(&projects, Change::CloseWorkstream { workstream: main }).unwrap();
        let left = projects.all()[0].workstreams().iter().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [kept]);
        assert_eq!(projects.terminals().count(), 0);
        let gone = change(&projects, Change::CloseWorkstream { workstream: main });
        assert!(gone.unwrap_err().contains("no workstream has the id"));
    }

    #[test]
    fn the_workstream_gone_to_last_is_the_one_used_last_whatever_the_clock_says() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let one = workstream(&mut projects, luneta, "/w/luneta/one");
        let two = workstream(&mut projects, luneta, "/w/luneta/two");
        let used = |projects: &Projects, workstream| projects.find(workstream).unwrap().1.used();
        assert_eq!(used(&projects, one), None, "a workstream never gone to is used never");
        let went = |workstream, millis| Change::Went { workstream, at: Used::of_millis(millis) };
        (projects, _) = change(&projects, went(one, 5_000)).unwrap();
        assert_eq!(used(&projects, one), Some(Used::of_millis(5_000)));
        (projects, _) = change(&projects, went(two, 4_000)).unwrap();
        assert_eq!(used(&projects, two), Some(Used::of_millis(5_001)), "a clock set back");
        (projects, _) = change(&projects, went(one, 9_000)).unwrap();
        assert_eq!(used(&projects, one), Some(Used::of_millis(9_000)));
        let (same, _) = change(&projects, went(one, 8_000)).unwrap();
        assert_eq!(same, projects, "a go to the workstream used last changes nothing");
        let (same, _) = change(&projects, went(one, 10_000)).unwrap();
        assert_eq!(same, projects, "however late the clock says it is");
        let unknown = went(WorkstreamId::of(7).unwrap(), 1);
        assert!(change(&projects, unknown).unwrap_err().contains("no workstream has the id"));
        let at = SystemTime::UNIX_EPOCH + Duration::from_millis(1_234);
        assert_eq!((Used::at(at).millis(), Used::of_millis(1_234).time()), (1_234, at));
        let far = SystemTime::UNIX_EPOCH + Duration::from_millis(u64::MAX);
        assert_eq!(Used::at(far).millis(), CLOCK_MILLIS_MAX, "a clock past 9999 is held there");
        assert_eq!(Used::read(READ_MILLIS_MAX), Ok(Used::of_millis(READ_MILLIS_MAX)));
        let past = Used::read(READ_MILLIS_MAX + 1).unwrap_err();
        assert!(past.contains("is past"), "{past}");
        (projects, _) = change(&projects, went(two, 1)).unwrap();
        (projects, _) = change(&projects, went(one, USED_MILLIS_MAX)).unwrap();
        let refused = change(&projects, went(two, 1)).unwrap_err();
        assert!(refused.contains("no workstream is used past"), "{refused}");
        let (same, _) = change(&projects, went(one, 1)).unwrap();
        assert_eq!(same, projects, "the workstream used last is gone to still");
    }

    #[test]
    fn a_terminal_keeps_the_name_it_is_given_until_taken_back() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let name = |projects: &Projects| projects.holding(one).unwrap().1.name().cloned();
        assert_eq!(name(&projects), None);
        let to = Some(Name::parse("tests").unwrap());
        (projects, _) = change(&projects, Change::RenameTerminal { terminal: one, to }).unwrap();
        assert_eq!(name(&projects).as_ref().map(Name::as_str), Some("tests"));
        let ran = Ran { terminal: one, folder: dir("/w/luneta/src"), runs: Runs::Shell };
        (projects, _) = change(&projects, Change::Ran(vec![ran])).unwrap();
        assert!(name(&projects).is_some(), "a terminal keeps its name whatever it runs");
        let back = Change::RenameTerminal { terminal: one, to: None };
        (projects, _) = change(&projects, back).unwrap();
        assert_eq!(name(&projects), None);
        let other = TerminalId::of(7).unwrap();
        let unknown = Change::RenameTerminal { terminal: other, to: None };
        assert!(change(&projects, unknown).unwrap_err().contains("no terminal has the id"));
    }

    #[test]
    fn a_terminal_is_placed_only_where_its_view_has_it_and_its_id_is_free() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let size = Size::of(80, 24).unwrap();
        let terminal = |id| {
            Terminal::new(TerminalId::of(id).unwrap(), dir("/w/luneta"), Runs::Shell)
        };
        let open = |id, placed| {
            Change::Open { workstream: main, terminal: terminal(id), placed, size }
        };
        let at = TerminalId::of(9).unwrap();
        let nowhere = Placed::Beside { at, towards: layout::Towards::Down };
        assert!(change(&projects, open(100, nowhere)).unwrap_err().contains("view is empty"));
        let one = opened(&mut projects, main, 100, Placed::Alone);
        assert!(change(&projects, open(101, Placed::Alone)).unwrap_err().contains("already"));
        assert!(change(&projects, open(100, nowhere)).unwrap_err().contains("is taken"));
        assert!(change(&projects, open(main.get(), nowhere)).unwrap_err().contains("is taken"));
        let tiny = Size::of(5, 3).unwrap();
        let beside = Placed::Beside { at: one, towards: layout::Towards::Right };
        let (terminal, placed) = (terminal(102), beside);
        let cramped = Change::Open { workstream: main, terminal, placed, size: tiny };
        assert!(change(&projects, cramped).unwrap_err().contains("no room"));
        let drawn = projects.fresh(&mut || Ok(one.0));
        assert!(drawn.unwrap_err().contains("were all taken"), "a terminal's id is drawn once");
    }

    fn pull_request(state: State, failed: u32) -> PullRequest {
        let checks = Some(Checks { pending: 0, failed, passed: 1 });
        let number = NonZeroU32::new(1).unwrap();
        PullRequest { number, state, url: None, checks, stale: false }
    }

    #[test]
    fn what_is_seen_of_a_workstream_comes_back_from_the_api_as_it_went() {
        let stages = [
            Stage::NoBranch,
            Stage::Working,
            Stage::PullRequestOpen,
            Stage::ChecksFailing,
            Stage::Merged,
        ];
        let read = stages.map(Seen::Read);
        for seen in [Seen::Missing, Seen::Unread].into_iter().chain(read) {
            assert_eq!(Seen::of_api(seen.to_api()), Some(seen));
        }
        let unspecified = api::workstream::Seen::Stage(api::Stage::Unspecified.into());
        assert_eq!(Seen::of_api(unspecified), None);
        assert_eq!(Seen::of_api(api::workstream::Seen::Stage(i32::MAX)), None);
    }

    #[test]
    fn the_stage_is_read_off_the_branch_and_its_pull_request() {
        let branch = Some("issue-1");
        let stage = |state, failed| Stage::of(branch, Some(&pull_request(state, failed)));
        assert_eq!(Stage::of(None, None), Stage::NoBranch);
        assert_eq!(Stage::of(None, Some(&pull_request(State::Merged, 0))), Stage::NoBranch);
        assert_eq!(Stage::of(branch, None), Stage::Working);
        assert_eq!(stage(State::Open, 0), Stage::PullRequestOpen);
        assert_eq!(stage(State::Draft, 0), Stage::PullRequestOpen);
        assert_eq!(stage(State::Open, 1), Stage::ChecksFailing);
        assert_eq!(stage(State::Closed, 1), Stage::Working);
        assert_eq!(stage(State::Merged, 1), Stage::Merged);
    }
}
