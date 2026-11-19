use std::cmp::Reverse;
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
use crate::mux::own::TerminalKey;
use crate::watch::Troubles;
use crate::{layouts, SessionName};

mod file;
pub(crate) mod layout;
mod spot;
mod view;

pub use file::{forget, holder, kept, take, Changed, Elsewhere, Failed, Forgotten, Lock, Revision};
pub use file::{Store, Unheld};
pub use layout::{Size, FRAME_CELLS, PANES_MAX};
pub use spot::{Spot, LOOK as SPOT_LOOK, WORKTREES_MAX};
pub use view::{Layout, Line, Placed, Runs, Session, Terminal, TerminalViewId, View};
pub use view::{LINE_BYTES_MAX, SESSION_BYTES_MAX};

pub const PROJECTS_MAX: usize = 64;

pub const WORKSTREAMS_MAX: usize = 512;

pub const VIEWS_MAX: usize = 1024;

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
id_of!(ViewId);

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
    pub fn worktree(&self, folder: &Dir) -> Option<Worktree> {
        self.below(folder).and_then(Worktree::of)
    }

    #[must_use]
    pub fn at(&self, worktree: &Worktree) -> Dir {
        self.join(&worktree.0)
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Worktree(Below);

impl Worktree {
    pub fn parse(text: &str) -> Result<Worktree, String> {
        let rooted = || "a worktree is below its clone's root, and never the root".to_owned();
        Worktree::of(Below::parse(text)?).ok_or_else(rooted)
    }

    #[must_use]
    pub fn of(below: Below) -> Option<Worktree> {
        (!below.is_own()).then_some(Worktree(below))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Holds {
    Folder(Workstream),
    Clone(Vec<(Worktree, Workstream)>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Within {
    Workstream(WorkstreamId),
    Unmade(Worktree),
    Project,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    id: ProjectId,
    name: Name,
    dir: Dir,
    holds: Holds,
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
    pub fn holds(&self) -> &Holds {
        &self.holds
    }

    pub fn workstreams(&self) -> impl Iterator<Item = &Workstream> {
        let (own, rows): (_, &[(Worktree, Workstream)]) = match &self.holds {
            Holds::Folder(own) => (Some(own), &[]),
            Holds::Clone(rows) => (None, rows),
        };
        own.into_iter().chain(rows.iter().map(|(_, it)| it))
    }

    pub fn placed(&self) -> impl Iterator<Item = (Dir, &Workstream)> {
        let (own, rows): (_, &[(Worktree, Workstream)]) = match &self.holds {
            Holds::Folder(own) => (Some((self.dir.clone(), own)), &[]),
            Holds::Clone(rows) => (None, rows),
        };
        own.into_iter().chain(rows.iter().map(|(worktree, it)| (self.dir.at(worktree), it)))
    }

    #[must_use]
    pub fn folder(&self, workstream: WorkstreamId) -> Option<Dir> {
        self.placed().find(|(_, it)| it.id == workstream).map(|(folder, _)| folder)
    }

    fn workstreams_mut(&mut self) -> impl Iterator<Item = &mut Workstream> {
        let (own, rows): (_, &mut [(Worktree, Workstream)]) = match &mut self.holds {
            Holds::Folder(own) => (Some(own), Default::default()),
            Holds::Clone(rows) => (None, rows),
        };
        own.into_iter().chain(rows.iter_mut().map(|(_, it)| it))
    }

    fn last_used(&self) -> Option<WorkstreamId> {
        let mut last: Option<&Workstream> = None;
        for workstream in self.workstreams() {
            if last.is_none_or(|last| workstream.used > last.used) {
                last = Some(workstream);
            }
        }
        let held = self.workstreams().next().is_some();
        assert_eq!(last.is_some(), held, "a project's workstream is last");
        last.map(Workstream::id)
    }

    fn within(&self, spot: &Spot) -> Option<Within> {
        self.dir.below(&spot.place())?;
        let rows = match &self.holds {
            Holds::Folder(own) => return Some(Within::Workstream(own.id)),
            Holds::Clone(rows) => rows,
        };
        let worktree = spot.worktree().filter(|_| *spot.dir() == self.dir);
        let Some(worktree) = worktree else {
            return Some(Within::Project);
        };
        let row = rows.iter().find(|(it, _)| it == worktree).map(|(_, it)| it.id);
        Some(row.map_or_else(|| Within::Unmade(worktree.clone()), Within::Workstream))
    }

    fn unplaced(&self, spot: &Spot) -> String {
        let (dir, place) = (&self.dir.0, spot.place());
        match self.holds {
            Holds::Folder(_) => format!("{} is not in {dir}, the project's folder", place.0),
            Holds::Clone(_) => format!("{} is in no worktree of {dir}, the project's clone", place.0),
        }
    }

    fn view_count(&self) -> usize {
        self.workstreams().map(|it| it.layout.len()).sum()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Workstream {
    id: WorkstreamId,
    renamed: Option<Name>,
    layout: Layout,
    used: Option<Used>,
}

impl Workstream {
    fn new(id: WorkstreamId, renamed: Option<Name>) -> Workstream {
        Workstream { id, renamed, layout: Layout::default(), used: None }
    }

    #[must_use]
    pub fn id(&self) -> WorkstreamId {
        self.id
    }

    #[must_use]
    pub fn renamed(&self) -> Option<&Name> {
        self.renamed.as_ref()
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
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
    AddProject { spot: Spot, name: Option<Name> },
    AddWorkstream { project: ProjectId, spot: Spot, name: Option<Name> },
    Start { spot: Spot, name: Option<Name> },
    Pull { spot: Spot, project: Name, name: Name },
    Join { spot: Spot },
    Listed { root: Dir, worktrees: BTreeSet<Worktree>, seen: BTreeSet<WorkstreamId> },
    Rooted { root: Dir, worktrees: BTreeSet<Worktree> },
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
    Went { workstream: WorkstreamId, at: Used },
    RenameTerminal { terminal: ViewId, to: Option<Name> },
    Open { workstream: WorkstreamId, view: View, placed: Placed, size: Size },
    Close { view: ViewId },
    Unopen { terminal: TerminalViewId },
    Move { terminal: TerminalViewId, to: WorkstreamId, size: Size },
    Resize {
        view: ViewId,
        sides: layout::Sides,
        growth: layout::Growth,
        size: Size,
    },
    Swap { one: ViewId, other: ViewId },
    Fill { workstream: WorkstreamId, full: Option<ViewId> },
    Ran(Vec<Ran>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Made {
    Project(ProjectId),
    Workstream(WorkstreamId),
    Found(WorkstreamId),
    Taken(WorkstreamId),
}

#[derive(Debug)]
pub struct Ran {
    pub terminal: TerminalViewId,
    pub folder: Dir,
    pub runs: Runs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closing {
    Workstream(WorkstreamId),
    Project(ProjectId),
}

#[derive(Debug)]
pub enum Ask {
    Change(Change),
    CloseWorkstream { workstream: WorkstreamId, seen: BTreeSet<ViewId> },
    CloseProject {
        project: ProjectId,
        workstreams: BTreeSet<WorkstreamId>,
        seen: BTreeSet<ViewId>,
    },
    Bring { workstream: WorkstreamId, from: TerminalKey },
}

impl Ask {
    pub fn of_api(change: api::change_projects_request::Change) -> Result<Ask, String> {
        use api::change_projects_request::{self as asked, Change as Asked};
        let field = |field: &'static str| move |why: String| format!("{field}: {why}");
        let id = |field: &'static str, id: u64| {
            Id::of(id).ok_or_else(|| format!("{field}: no id is 0"))
        };
        let name = |name: Option<String>| name.as_deref().map(Name::parse).transpose();
        let spot = |folder: &str| Dir::found(folder).and_then(|it| Spot::of(&it, SPOT_LOOK));
        let change = match change {
            Asked::AddProject(asked::AddProject { dir, name: named }) => Change::AddProject {
                spot: spot(&dir).map_err(field("dir"))?,
                name: named.as_deref().map(Name::of_project).transpose().map_err(field("name"))?,
            },
            Asked::AddWorkstream(asked::AddWorkstream { project, folder, name: named }) => {
                Change::AddWorkstream {
                    project: id("project", project).map(ProjectId)?,
                    spot: spot(&folder).map_err(field("folder"))?,
                    name: name(named).map_err(field("name"))?,
                }
            },
            Asked::WorkstreamAt(asked::WorkstreamAt { project, folder }) => Change::AddWorkstream {
                project: id("project", project).map(ProjectId)?,
                spot: spot(&folder).map_err(field("folder"))?,
                name: None,
            },
            Asked::Start(asked::Start { folder, name: named }) => Change::Start {
                spot: spot(&folder).map_err(field("folder"))?,
                name: named.as_deref().map(Name::of_project).transpose().map_err(field("name"))?,
            },
            Asked::Pull(asked::Pull { holder: _, folder, project, name }) => Change::Pull {
                spot: spot(&folder).map_err(field("folder"))?,
                project: Name::of_project(&project).map_err(field("project"))?,
                name: Name::parse(&name).map_err(field("name"))?,
            },
            Asked::Join(asked::Join { folder }) => {
                Change::Join { spot: spot(&folder).map_err(field("folder"))? }
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
                let seen = seen.into_iter().map(|it| id("seen", it).map(ViewId));
                let seen = seen.collect::<Result<BTreeSet<ViewId>, String>>()?;
                return Ok(Ask::CloseWorkstream { workstream, seen });
            },
            Asked::CloseProject(asked::CloseProject { project, workstreams, seen }) => {
                let project = id("project", project).map(ProjectId)?;
                let held = workstreams.into_iter().map(|it| id("workstream", it).map(WorkstreamId));
                let workstreams = held.collect::<Result<BTreeSet<WorkstreamId>, String>>()?;
                let seen = seen.into_iter().map(|it| id("seen", it).map(ViewId));
                let seen = seen.collect::<Result<BTreeSet<ViewId>, String>>()?;
                return Ok(Ask::CloseProject { project, workstreams, seen });
            },
            Asked::Bring(asked::Bring { workstream, from }) => {
                let workstream = id("workstream", workstream).map(WorkstreamId)?;
                let from = from.ok_or("from: it names no terminal")?;
                let from = TerminalKey::of(&from).map_err(field("from"))?;
                return Ok(Ask::Bring { workstream, from });
            },
            Asked::RenameTerminal(asked::RenameTerminal { terminal, to }) => {
                Change::RenameTerminal {
                    terminal: id("terminal", terminal).map(ViewId)?,
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
            | Change::Start { .. }
            | Change::Pull { .. }
            | Change::Join { .. }
            | Change::Listed { .. }
            | Change::Rooted { .. }
            | Change::RenameProject { .. }
            | Change::RenameWorkstream { .. }
            | Change::ReorderProjects { .. }
            | Change::ReorderWorkstreams { .. }
            | Change::ForgetProject { .. }
            | Change::ForgetWorkstream { .. }
            | Change::RenameTerminal { .. }
            | Change::Open { .. }
            | Change::Close { .. }
            | Change::Unopen { .. }
            | Change::Move { .. } => true,
            Change::Went { .. }
            | Change::Resize { .. }
            | Change::Swap { .. }
            | Change::Fill { .. }
            | Change::Ran(_) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Home {
    Stays,
    In(WorkstreamId),
    Made { project: ProjectId, spot: Spot },
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
        if projects.view_count() > VIEWS_MAX {
            return Err(format!("it holds over {VIEWS_MAX} views"));
        }
        let Projects { projects } = projects;
        let mut ids = BTreeSet::new();
        for (at, project) in projects.iter().enumerate() {
            let workstreams = project.workstreams().map(|workstream| workstream.id.0);
            let terminals = project.workstreams().flat_map(|workstream| {
                workstream.layout.views().map(|view| layout::Leaf::id(view).0)
            });
            for id in std::iter::once(project.id.0).chain(workstreams).chain(terminals) {
                if !ids.insert(id) {
                    return Err(format!("the id {id} comes twice"));
                }
            }
            if let Some(other) = projects[..at].iter().find(|other| other.dir.nests(&project.dir)) {
                return Err(nested(&project.dir, other));
            }
            shaped(project)?;
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

    pub fn clones(&self) -> impl Iterator<Item = (Dir, BTreeSet<WorkstreamId>)> + '_ {
        let clones = self.projects.iter().filter(|project| matches!(project.holds, Holds::Clone(_)));
        clones.map(|project| {
            let seen = project.workstreams().map(Workstream::id).collect();
            (project.dir.clone(), seen)
        })
    }

    pub fn plain(&self) -> impl Iterator<Item = Dir> + '_ {
        let plain = self.projects.iter().filter(|project| matches!(project.holds, Holds::Folder(_)));
        plain.map(|project| project.dir.clone())
    }

    pub fn workstream_folders(&self) -> impl Iterator<Item = Dir> + '_ {
        self.projects.iter().flat_map(|project| project.placed().map(|(folder, _)| folder))
    }

    pub fn changed(
        &self,
        change: Change,
        draw: impl FnMut() -> io::Result<Id>,
    ) -> Result<(Projects, Option<Made>), Refused> {
        if let Some(dir) = self.added_at(&change)
            && let Some(other) = self.projects.iter().find(|other| other.dir.nests(dir))
        {
            return Err(Refused::Nests(nested(dir, other)));
        }
        if let Some(why) = self.stale(&change) {
            return Err(Refused::Stale(why));
        }
        self.changed_by(change, draw).map_err(Refused::Said)
    }

    fn added_at<'a>(&self, change: &'a Change) -> Option<&'a Dir> {
        match change {
            Change::AddProject { spot, .. } => Some(spot.dir()),
            Change::Start { spot, .. } | Change::Pull { spot, .. } | Change::Join { spot } => {
                self.inside(spot).is_none().then_some(spot.dir())
            },
            _ => None,
        }
    }

    fn inside(&self, spot: &Spot) -> Option<(ProjectId, Within)> {
        self.projects.iter().find_map(|project| Some((project.id, project.within(spot)?)))
    }

    fn stale(&self, change: &Change) -> Option<String> {
        match change {
            Change::ReorderProjects { seen, .. } => {
                stale(self.projects.iter().map(Project::id), seen, "projects")
            },
            Change::ReorderWorkstreams { project, seen, .. } => {
                let project = self.projects.iter().find(|it| it.id == *project)?;
                stale(project.workstreams().map(Workstream::id), seen, "workstreams")
            },
            _ => None,
        }
    }

    fn changed_by(
        &self,
        change: Change,
        mut draw: impl FnMut() -> io::Result<Id>,
    ) -> Result<(Projects, Option<Made>), String> {
        let mut changed = self.clone();
        let made = match change {
            Change::AddProject { spot, name } => {
                Some(Made::Project(changed.with_project(&spot, name, &mut draw)?))
            },
            Change::AddWorkstream { project, spot, name } => {
                let held = self.project_of(project).ok_or_else(|| unknown("project", project))?;
                let made = match held.within(&spot) {
                    Some(within) => changed.workstream_in(project, within, name, &mut draw)?,
                    None => None,
                };
                Some(Made::Workstream(made.ok_or_else(|| held.unplaced(&spot))?))
            },
            Change::Start { spot, name } => {
                let (project, within) = match self.inside(&spot) {
                    Some(found) => found,
                    None => changed.made_inside(&spot, name, &mut draw)?,
                };
                let made = changed.workstream_in(project, within, None, &mut draw)?;
                let last = || changed.project_of(project).and_then(Project::last_used);
                Some(made.or_else(last).map_or(Made::Project(project), Made::Workstream))
            },
            Change::Pull { spot, project, name } => {
                let (project, within) = match self.inside(&spot) {
                    Some(found) => found,
                    None => changed.made_inside(&spot, Some(project), &mut draw)?,
                };
                let made = changed.workstream_in(project, within, Some(name), &mut draw)?;
                let held = changed.project_of(project).ok_or_else(|| unknown("project", project))?;
                Some(Made::Workstream(made.ok_or_else(|| held.unplaced(&spot))?))
            },
            Change::Join { spot } => {
                let (project, within) = match self.inside(&spot) {
                    Some(found) => found,
                    None => changed.made_inside(&spot, None, &mut draw)?,
                };
                let joined = changed.workstream_in(project, within, None, &mut draw)?;
                let held = changed.project_of(project).ok_or_else(|| unknown("project", project))?;
                let joined = joined.ok_or_else(|| held.unplaced(&spot))?;
                let found = self.find(joined).is_some();
                Some(if found { Made::Found(joined) } else { Made::Workstream(joined) })
            },
            Change::Listed { root, worktrees, seen } => {
                changed.worktrees_listed(&root, &worktrees, &seen, &mut draw)?;
                None
            },
            Change::Rooted { root, worktrees } => {
                changed.rooted(&root, &worktrees, &mut draw)?;
                None
            },
            Change::RenameProject { project, to } => {
                changed.project(project)?.name = to;
                None
            },
            Change::RenameWorkstream { workstream, to } => {
                changed.workstream_mut(workstream)?.renamed = to;
                None
            },
            Change::ReorderProjects { projects, .. } => {
                reordered(&mut changed.projects, |it| it.id, &projects, "projects")?;
                None
            },
            Change::ReorderWorkstreams { project, workstreams, .. } => {
                let asked = (&workstreams, "workstreams");
                match &mut changed.project(project)?.holds {
                    Holds::Folder(own) => {
                        reordered(std::slice::from_mut(own), |it| it.id, asked.0, asked.1)?;
                    },
                    Holds::Clone(rows) => reordered(rows, |(_, it)| it.id, asked.0, asked.1)?,
                }
                None
            },
            Change::ForgetProject { project } => {
                let views = changed.project(project)?.view_count();
                if views > 0 {
                    return Err(format!("project {project} holds {views} views still"));
                }
                changed.projects.retain(|it| it.id != project);
                None
            },
            Change::ForgetWorkstream { workstream } => {
                let found = self.find(workstream);
                let (project, held) = found.ok_or_else(|| unknown("workstream", workstream))?;
                if let Holds::Folder(_) = project.holds {
                    let name = &project.name.0;
                    return Err(format!("workstream {workstream} is project {name}'s own folder"));
                }
                let held = held.layout.len();
                if held > 0 {
                    return Err(format!("workstream {workstream} holds {held} views still"));
                }
                changed.without(workstream)?;
                None
            },
            Change::Went { workstream, at } => {
                let others = self.workstreams().filter(|(_, it)| it.id != workstream);
                let newest = others.filter_map(|(_, it)| it.used).max();
                let went = changed.workstream_mut(workstream)?;
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
            Change::RenameTerminal { terminal: view, to } => {
                let layout = changed.layout_of(view)?;
                let held = self.holding_terminal(view).map(|(_, it)| it.id());
                let terminal = held.ok_or_else(|| format!("view {view} runs no terminal"))?;
                layout.terminal_mut(terminal).renamed(to);
                None
            },
            Change::Open { workstream, view, placed, size } => {
                let id = layout::Leaf::id(&view);
                if self.holds(id.0) {
                    return Err(format!("the id {id} is taken"));
                }
                if self.view_count() >= VIEWS_MAX {
                    let many = format!("there are {VIEWS_MAX} views");
                    return Err(format!("{many}, as many as are kept"));
                }
                changed.workstream_mut(workstream)?.layout.opened(view, placed, size)?;
                None
            },
            Change::Close { view } => {
                self.close_view(&mut changed, view)?;
                None
            },
            Change::Unopen { terminal } => {
                self.close_view(&mut changed, terminal.view())?;
                None
            },
            Change::Move { terminal, to, size } => {
                let held_in = self.holding_terminal(terminal.view()).map(|(it, _)| it.id());
                let from = held_in.ok_or_else(|| unknown("terminal", terminal))?;
                if from == to {
                    return Err(format!("terminal {terminal} is in workstream {to} already"));
                }
                let (_, target) = self.find(to).ok_or_else(|| unknown("workstream", to))?;
                let placed = match target.layout.tree() {
                    None => Placed::Alone,
                    Some(tree) => {
                        let room = tree.room_beside_largest(terminal.view(), size);
                        let roomless = || format!("workstream {to} has no room for {terminal}");
                        let (at, towards, _) = room.ok_or_else(roomless)?;
                        Placed::Beside { at, towards }
                    },
                };
                let layout = changed.layout_of(terminal.view())?;
                let moved = layout.terminal_mut(terminal).clone();
                layout.closed(terminal.view());
                assert!(changed.find(from).is_some(), "{from} stays, if empty");
                changed.workstream_mut(to)?.layout.opened(View::Terminal(moved), placed, size)?;
                let held_in = changed.holding_terminal(terminal.view()).map(|(it, _)| it.id());
                assert_eq!(held_in, Some(to), "terminal {terminal} moved to workstream {to}");
                None
            },
            Change::Resize { view, sides, growth, size } => {
                changed.layout_of(view)?.resized(view, sides, growth, size)?;
                None
            },
            Change::Swap { one, other } => {
                let layout = changed.layout_of(one)?;
                if !layout.holds(other) {
                    return Err(format!("terminals {one} and {other} are in two layouts"));
                }
                layout.swapped(one, other)?;
                None
            },
            Change::Fill { workstream, full } => {
                changed.workstream_mut(workstream)?.layout.filled(full)?;
                None
            },
            Change::Ran(ran) => {
                for Ran { terminal, folder, runs } in ran {
                    changed.layout_of(terminal.view())?.terminal_mut(terminal).ran(folder, runs);
                }
                None
            },
        };
        Projects::kept_as(&changed);
        Ok((changed, made))
    }

    pub fn closed(&self, closing: Closing) -> Result<Projects, String> {
        let mut changed = self.clone();
        match closing {
            Closing::Workstream(workstream) => {
                let closed = std::mem::take(&mut changed.workstream_mut(workstream)?.layout);
                for view in closed.views() {
                    let id = layout::Leaf::id(view);
                    assert!(!changed.holds(id.0), "view {id} goes with its workstream's layout");
                }
                assert!(changed.find(workstream).is_some(), "workstream {workstream} stays, empty");
            },
            Closing::Project(project) => {
                let at = changed.projects.iter().position(|it| it.id == project);
                let at = at.ok_or_else(|| unknown("project", project))?;
                let closed = changed.projects.remove(at);
                let count = closed.workstreams().count();
                assert!(count <= WORKSTREAMS_MAX, "{WORKSTREAMS_MAX} at most");
                assert!(changed.project_of(project).is_none(), "project {project} is closed");
                for workstream in closed.workstreams() {
                    let id = workstream.id;
                    assert!(changed.find(id).is_none(), "workstream {id} goes with its project");
                    for view in workstream.layout.views() {
                        let id = layout::Leaf::id(view);
                        assert!(!changed.holds(id.0), "view {id} goes with its project");
                    }
                }
            },
        }
        Projects::kept_as(&changed);
        Ok(changed)
    }

    fn with_project(
        &mut self,
        spot: &Spot,
        name: Option<Name>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<ProjectId, String> {
        let dir = spot.dir().clone();
        let name = match name {
            Some(name) => name,
            None => Name::parse(dir.last()).map_err(|why| format!("name it: {why}"))?,
        };
        if self.projects.len() >= PROJECTS_MAX {
            return Err(format!("there are {PROJECTS_MAX} projects, as many as are kept"));
        }
        let id = ProjectId(self.fresh(draw)?);
        self.projects.push(Project { id, name, dir, holds: Holds::Clone(Vec::new()) });
        if let Some(worktrees) = spot.worktrees() {
            for worktree in worktrees {
                self.worktree_made(id, worktree, None, draw)?;
            }
        } else {
            let own = Workstream::new(self.drawn(draw)?, None);
            self.project(id)?.holds = Holds::Folder(own);
        }
        Ok(id)
    }

    fn made_inside(
        &mut self,
        spot: &Spot,
        name: Option<Name>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<(ProjectId, Within), String> {
        let made = self.with_project(spot, name, draw)?;
        let (place, dir) = (spot.place(), spot.dir());
        let outside = || format!("project {made}, made in {}, holds no {}", dir.0, place.0);
        self.inside(spot).ok_or_else(outside)
    }

    fn workstream_in(
        &mut self,
        project: ProjectId,
        within: Within,
        name: Option<Name>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<Option<WorkstreamId>, String> {
        match within {
            Within::Workstream(found) => {
                self.named(found, name)?;
                Ok(Some(found))
            },
            Within::Unmade(worktree) => self.worktree_made(project, &worktree, name, draw).map(Some),
            Within::Project => Ok(None),
        }
    }

    fn worktree_made(
        &mut self,
        project: ProjectId,
        worktree: &Worktree,
        name: Option<Name>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<WorkstreamId, String> {
        let found = self.rows(project)?.iter().find(|(it, _)| it == worktree).map(|(_, it)| it.id);
        if let Some(found) = found {
            self.named(found, name)?;
            return Ok(found);
        }
        let made = self.drawn(draw)?;
        self.rows(project)?.push((worktree.clone(), Workstream::new(made, name)));
        let count = self.workstream_count();
        assert!(count <= WORKSTREAMS_MAX, "{count} workstreams are kept, over {WORKSTREAMS_MAX}");
        Ok(made)
    }

    fn named(&mut self, workstream: WorkstreamId, name: Option<Name>) -> Result<(), String> {
        let row = self.workstream_mut(workstream)?;
        if row.renamed.is_none() {
            row.renamed = name;
        }
        Ok(())
    }

    fn drawn(&self, draw: &mut impl FnMut() -> io::Result<Id>) -> Result<WorkstreamId, String> {
        if self.workstream_count() >= WORKSTREAMS_MAX {
            let many = format!("there are {WORKSTREAMS_MAX} workstreams");
            return Err(format!("{many}, as many as are kept"));
        }
        self.fresh(draw).map(WorkstreamId)
    }

    fn rows(&mut self, project: ProjectId) -> Result<&mut Vec<(Worktree, Workstream)>, String> {
        let held = self.project(project)?;
        match &mut held.holds {
            Holds::Clone(rows) => Ok(rows),
            Holds::Folder(_) => {
                let (name, dir) = (&held.name.0, &held.dir.0);
                Err(format!("project {name} in {dir} is a folder, which holds no worktree"))
            },
        }
    }

    fn worktrees_listed(
        &mut self,
        root: &Dir,
        worktrees: &BTreeSet<Worktree>,
        seen: &BTreeSet<WorkstreamId>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<(), String> {
        let clone = |it: &&Project| it.dir == *root && matches!(it.holds, Holds::Clone(_));
        let Some(project) = self.projects.iter().find(clone).map(Project::id) else {
            return Ok(());
        };
        let rows = self.rows(project)?;
        let held = |workstream: &Workstream| workstream.layout.terminals().count();
        let terminals: usize = rows.iter().map(|(_, it)| held(it)).sum();
        rows.retain(|(worktree, it)| {
            !seen.contains(&it.id) || worktrees.contains(worktree) || held(it) > 0
        });
        let kept: usize = rows.iter().map(|(_, it)| held(it)).sum();
        assert_eq!(kept, terminals, "a listing of worktrees drops no terminal");
        for worktree in worktrees {
            self.worktree_made(project, worktree, None, draw)?;
        }
        Ok(())
    }

    fn rooted(
        &mut self,
        root: &Dir,
        worktrees: &BTreeSet<Worktree>,
        draw: &mut impl FnMut() -> io::Result<Id>,
    ) -> Result<(), String> {
        let Some(project) = self.projects.iter_mut().find(|it| it.dir == *root) else {
            return Ok(());
        };
        let Holds::Folder(own) = &project.holds else {
            return Ok(());
        };
        let holding = |worktree: &Worktree| {
            let folder = root.at(worktree);
            own.layout.terminals().filter(|it| folder.below(it.folder()).is_some()).count()
        };
        let most = worktrees.iter().enumerate().max_by_key(|(at, it)| (holding(it), Reverse(*at)));
        let Some((_, home)) = most else {
            return Ok(());
        };
        let views = project.view_count();
        project.holds = Holds::Clone(vec![(home.clone(), own.clone())]);
        assert_eq!(project.view_count(), views, "a folder rooted keeps every view");
        let id = project.id;
        for worktree in worktrees {
            self.worktree_made(id, worktree, None, draw)?;
        }
        Ok(())
    }

    fn kept_as(changed: &Projects) {
        assert_eq!(Projects::checked(changed.projects.clone()).as_ref(), Ok(changed));
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
        self.projects.iter().map(|project| project.workstreams().count()).sum()
    }

    fn view_count(&self) -> usize {
        self.workstreams().map(|(_, workstream)| workstream.layout.len()).sum()
    }

    pub fn workstreams(&self) -> impl Iterator<Item = (&Project, &Workstream)> {
        self.projects.iter().flat_map(|project| project.workstreams().map(move |it| (project, it)))
    }

    #[must_use]
    pub fn project_of(&self, project: ProjectId) -> Option<&Project> {
        self.projects.iter().find(|it| it.id == project)
    }

    #[must_use]
    pub fn find(&self, workstream: WorkstreamId) -> Option<(&Project, &Workstream)> {
        self.workstreams().find(|(_, it)| it.id == workstream)
    }

    #[must_use]
    pub fn home(&self, terminal: ViewId, spot: &Spot) -> Home {
        let Some((from, _)) = self.holding_terminal(terminal) else {
            return Home::Stays;
        };
        match self.inside(spot) {
            Some((_, Within::Workstream(home))) if home == from.id => Home::Stays,
            Some((_, Within::Workstream(home))) => Home::In(home),
            Some((project, Within::Unmade(_))) => Home::Made { project, spot: spot.clone() },
            Some((_, Within::Project)) | None => Home::Stays,
        }
    }

    #[must_use]
    pub fn holding_view(&self, view: ViewId) -> Option<(&Workstream, &View)> {
        self.workstreams().find_map(|(_, workstream)| {
            let tree = workstream.layout.tree()?;
            Some((workstream, tree.get(view)?))
        })
    }

    #[must_use]
    pub fn holding_terminal(&self, terminal: ViewId) -> Option<(&Workstream, &Terminal)> {
        let (workstream, view) = self.holding_view(terminal)?;
        Some((workstream, view.terminal()?))
    }

    pub fn terminals(&self) -> impl Iterator<Item = (&Workstream, &Terminal)> {
        self.workstreams().flat_map(|(_, workstream)| {
            workstream.layout.terminals().map(move |it| (workstream, it))
        })
    }

    #[must_use]
    pub fn holds(&self, id: Id) -> bool {
        self.projects.iter().any(|project| {
            project.id.0 == id
                || project.workstreams().any(|workstream| {
                    workstream.id.0 == id || workstream.layout.holds(ViewId(id))
                })
        })
    }

    fn close_view(&self, changed: &mut Projects, view: ViewId) -> Result<(), String> {
        let held_in = self.holding_view(view).map(|(workstream, _)| workstream.id());
        let workstream = held_in.ok_or_else(|| unknown("view", view))?;
        changed.layout_of(view)?.closed(view);
        assert!(changed.find(workstream).is_some(), "{workstream} stays, if empty");
        assert!(!changed.holds(view.0), "view {view} left its layout");
        Ok(())
    }

    fn layout_of(&mut self, terminal: ViewId) -> Result<&mut Layout, String> {
        self.projects
            .iter_mut()
            .flat_map(Project::workstreams_mut)
            .map(|workstream| &mut workstream.layout)
            .find(|layout| layout.holds(terminal))
            .ok_or_else(|| unknown("terminal", terminal))
    }

    fn project(&mut self, id: ProjectId) -> Result<&mut Project, String> {
        let project = self.projects.iter_mut().find(|project| project.id == id);
        project.ok_or_else(|| unknown("project", id))
    }

    fn without(&mut self, workstream: WorkstreamId) -> Result<Workstream, String> {
        let gone = self.projects.iter_mut().find_map(|project| {
            let Holds::Clone(rows) = &mut project.holds else {
                return None;
            };
            let at = rows.iter().position(|(_, it)| it.id == workstream)?;
            Some(rows.remove(at).1)
        });
        let gone = gone.ok_or_else(|| format!("no worktree's workstream has the id {workstream}"))?;
        assert!(self.find(workstream).is_none(), "{workstream} is gone");
        Ok(gone)
    }

    fn workstream_mut(&mut self, id: WorkstreamId) -> Result<&mut Workstream, String> {
        let mut all = self.projects.iter_mut().flat_map(Project::workstreams_mut);
        all.find(|it| it.id == id).ok_or_else(|| unknown("workstream", id))
    }
}

fn shaped(project: &Project) -> Result<(), String> {
    let Holds::Clone(rows) = &project.holds else {
        return Ok(());
    };
    let mut worktrees = BTreeSet::new();
    for (worktree, _) in rows {
        if !worktrees.insert(worktree) {
            let (folder, name) = (project.dir.at(worktree), &project.name.0);
            return Err(format!("{} holds two of project {name}'s workstreams", folder.0));
        }
    }
    Ok(())
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

pub fn follow(
    core: SyncSender<Command>,
    layouts: SyncSender<layouts::Command>,
    sweep: Duration,
) -> io::Result<()> {
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
            let Some(plain) = core::ask(&core, Command::Plain) else {
                return;
            };
            for root in plain.into_iter().filter(|it| !missing.contains(it) && spot::holds_bare(it)) {
                let spot = match Spot::of(&root, SPOT_LOOK) {
                    Ok(spot) => spot,
                    Err(why) => {
                        troubles.say(format!("{}: whose clone it is is unknown: {why}", root.0));
                        continue;
                    },
                };
                let rooted = spot.worktrees().filter(|it| *spot.dir() == root && !it.is_empty());
                let Some(worktrees) = rooted.cloned() else {
                    continue;
                };
                if layouts.send(layouts::Command::Rooted { root, worktrees }).is_err() {
                    return;
                }
            }
            let Some(clones) = core::ask(&core, Command::Clones) else {
                return;
            };
            for (root, seen) in clones.into_iter().filter(|(root, _)| !missing.contains(root)) {
                let worktrees = match spot::worktrees(&root, SPOT_LOOK) {
                    Ok(worktrees) => worktrees,
                    Err(why) => {
                        troubles.say(format!("{}: its worktrees are not listed: {why}", root.0));
                        continue;
                    },
                };
                if layouts.send(layouts::Command::Worktrees { root, worktrees, seen }).is_err() {
                    return;
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

    fn add_in(project: ProjectId, spot: Spot) -> Change {
        Change::AddWorkstream { project, spot, name: None }
    }

    fn add_named(project: ProjectId, spot: Spot, name: Option<Name>) -> Change {
        Change::AddWorkstream { project, spot, name }
    }

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn name(text: &str) -> Name {
        Name::parse(text).unwrap()
    }

    fn counting() -> impl FnMut() -> io::Result<Id> {
        let mut next = 0;
        move || {
            next += 1;
            Ok(Id::of(next).unwrap())
        }
    }

    fn refused(projects: &Projects, change: Change) -> Result<(Projects, Option<Made>), Refused> {
        let mut drawn = counting();
        let taken = |id: &Id| projects.holds(*id);
        projects.changed(change, || loop {
            let id = drawn()?;
            if !taken(&id) {
                return Ok(id);
            }
        })
    }

    fn change(projects: &Projects, change: Change) -> Result<(Projects, Option<Made>), String> {
        refused(projects, change).map_err(|refused| refused.to_string())
    }

    fn add(projects: &mut Projects, change_of: Change) -> Made {
        let (changed, made) = change(projects, change_of).unwrap();
        *projects = changed;
        made.unwrap()
    }

    fn folder(at: &str) -> Spot {
        Spot::in_folder(dir(at), dir(at))
    }

    fn tree(at: &str) -> Worktree {
        Worktree::parse(at).unwrap()
    }

    fn worktree(root: &str, at: &str) -> Spot {
        Spot::in_clone(dir(root), Some(tree(at)), BTreeSet::from([tree(at)]))
    }

    fn clone_of(root: &str, worktrees: &[&str]) -> Spot {
        Spot::in_clone(dir(root), None, worktrees.iter().copied().map(tree).collect())
    }

    fn is_clone(project: &Project) -> bool {
        matches!(project.holds(), Holds::Clone(_))
    }

    fn added(projects: &mut Projects, spot: Spot) -> ProjectId {
        let Made::Project(made) = add(projects, Change::AddProject { spot, name: None }) else {
            panic!("an added project is made");
        };
        made
    }

    fn project(projects: &mut Projects, at: &str) -> ProjectId {
        added(projects, folder(at))
    }

    fn clone(projects: &mut Projects, root: &str, worktrees: &[&str]) -> ProjectId {
        added(projects, clone_of(root, worktrees))
    }

    fn own(projects: &Projects, project: ProjectId) -> WorkstreamId {
        let Holds::Folder(own) = projects.project_of(project).unwrap().holds() else {
            panic!("a folder's project holds one workstream");
        };
        own.id()
    }

    fn at(projects: &Projects, folder: &str) -> Option<WorkstreamId> {
        let mut placed = projects.all().iter().flat_map(Project::placed);
        placed.find(|(at, _)| *at == dir(folder)).map(|(_, it)| it.id())
    }

    fn workstream(projects: &mut Projects, project: ProjectId, at: &str) -> WorkstreamId {
        let root = projects.project_of(project).unwrap().dir().as_str().to_owned();
        let below = at.strip_prefix(&root).and_then(|it| it.strip_prefix('/')).unwrap();
        let added = add_in(project, worktree(&root, below));
        let Made::Workstream(made) = add(projects, added) else {
            panic!("an added workstream is made");
        };
        made
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
            let refused = refused(&projects, Change::AddProject { spot: folder(at), name: None });
            let Err(Refused::Nests(said)) = refused else {
                panic!("{at} is refused as nested: {refused:?}");
            };
            assert!(said.contains(why), "{at}: {said}");
        }
        project(&mut projects, "/w/luneta-old");
        assert_eq!(projects.all().len(), 2);
    }

    #[test]
    fn a_clone_s_workstreams_are_its_worktrees_one_each() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &["main", "feat"]);
        let held = &projects.all()[0];
        assert_eq!((held.dir(), is_clone(held)), (&dir("/w/luneta"), true));
        let folders: Vec<Dir> = projects.workstream_folders().collect();
        assert_eq!(folders, ["/w/luneta/feat", "/w/luneta/main"].map(dir));
        let main = at(&projects, "/w/luneta/main").unwrap();
        assert_eq!(workstream(&mut projects, luneta, "/w/luneta/main"), main, "one per worktree");
        let fix = workstream(&mut projects, luneta, "/w/luneta/fix");
        assert_eq!(at(&projects, "/w/luneta/fix"), Some(fix));
        assert_eq!(projects.all()[0].workstreams().count(), 3);
        let root = add_in(luneta, clone_of("/w/luneta", &[]));
        assert!(change(&projects, root).unwrap_err().contains("is in no worktree of /w/luneta"));
        let elsewhere = add_in(luneta, worktree("/w/elo", "main"));
        let refused = change(&projects, elsewhere).unwrap_err();
        assert!(refused.contains("is in no worktree of /w/luneta"), "{refused}");
        let plain = add_in(luneta, folder("/w/luneta/main"));
        assert!(change(&projects, plain).unwrap_err().contains("is in no worktree"));
        let named = Some(name("issue 7"));
        let again = add_named(luneta, worktree("/w/luneta", "fix"), named);
        (projects, _) = change(&projects, again).unwrap();
        let renamed = projects.find(fix).unwrap().1.renamed();
        assert_eq!(renamed, Some(&name("issue 7")), "a name is given to one that has none");
        let other = add_named(luneta, worktree("/w/luneta", "fix"), Some(name("x")));
        (projects, _) = change(&projects, other).unwrap();
        assert_eq!(projects.find(fix).unwrap().1.renamed(), Some(&name("issue 7")), "and kept");
    }

    #[test]
    fn a_folder_s_project_holds_one_workstream_there_whatever_is_asked_below_it() {
        let mut projects = Projects::default();
        let vault = project(&mut projects, "/w/vault");
        let held = &projects.all()[0];
        let own = own(&projects, vault);
        assert_eq!(held.folder(own), Some(dir("/w/vault")));
        let below = add_in(vault, folder("/w/vault/notes"));
        let (same, found) = change(&projects, below).unwrap();
        assert_eq!((same == projects, found), (true, Some(Made::Workstream(own))));
        let outside = add_in(vault, folder("/w/elo"));
        assert!(change(&projects, outside).unwrap_err().contains("is not in /w/vault"));
        let forget = change(&projects, Change::ForgetWorkstream { workstream: own });
        assert!(forget.unwrap_err().contains("is project vault's own folder"));
    }

    #[test]
    fn a_start_at_a_clone_s_root_opens_its_last_used_and_makes_none() {
        let mut projects = Projects::default();
        let bare = clone(&mut projects, "/w/bare", &[]);
        let start = |at: &str| Change::Start { spot: clone_of(at, &[]), name: None };
        let (same, found) = change(&projects, start("/w/bare")).unwrap();
        assert_eq!((same == projects, found), (true, Some(Made::Project(bare))), "it holds none");
        clone(&mut projects, "/w/luneta", &["main", "feat"]);
        let feat = at(&projects, "/w/luneta/feat").unwrap();
        let main = at(&projects, "/w/luneta/main").unwrap();
        let (same, found) = change(&projects, start("/w/luneta")).unwrap();
        let first = (true, Some(Made::Workstream(feat)));
        assert_eq!((same == projects, found), first, "none used: the first");
        let went = Change::Went { workstream: main, at: Used::of_millis(5) };
        projects = change(&projects, went).unwrap().0;
        let (same, found) = change(&projects, start("/w/luneta")).unwrap();
        assert_eq!((same == projects, found), (true, Some(Made::Workstream(main))));
    }

    #[test]
    fn a_start_goes_to_its_worktree_s_workstream_and_in_no_project_makes_one() {
        let mut projects = Projects::default();
        clone(&mut projects, "/w/luneta", &["main"]);
        let main = at(&projects, "/w/luneta/main").unwrap();
        let start = |root: &str, at: &str| Change::Start { spot: worktree(root, at), name: None };
        let (same, found) = change(&projects, start("/w/luneta", "main")).unwrap();
        assert_eq!((same == projects, found), (true, Some(Made::Workstream(main))));
        let feat = add(&mut projects, start("/w/luneta", "feat"));
        assert_eq!(Some(feat), at(&projects, "/w/luneta/feat").map(Made::Workstream));
        assert_eq!(projects.all().len(), 1);

        let worktrees = BTreeSet::from([tree("fix"), tree("main")]);
        let spot = Spot::in_clone(dir("/w/elo"), Some(tree("main")), worktrees);
        let named = Change::Start { spot, name: Some(name("elo-app")) };
        let Made::Workstream(elo_main) = add(&mut projects, named) else {
            panic!("a start in a worktree of no project makes its project and goes there");
        };
        let (held, there) = projects.find(elo_main).unwrap();
        let held_as = (held.name(), held.dir(), held.folder(there.id()));
        assert_eq!(held_as, (&name("elo-app"), &dir("/w/elo"), Some(dir("/w/elo/main"))));
        assert_eq!(held.workstreams().count(), 2, "a workstream for each of its worktrees");

        let vault = Change::Start { spot: folder("/w/vault"), name: None };
        let Made::Workstream(notes) = add(&mut projects, vault) else {
            panic!("a start in a folder of no project makes its project");
        };
        let inside = Change::Start { spot: folder("/w/vault/notes"), name: None };
        assert_eq!(change(&projects, inside).unwrap().1, Some(Made::Workstream(notes)));
        let refused = refused(&projects, Change::Start { spot: folder("/w"), name: None });
        let Err(Refused::Nests(said)) = refused else {
            panic!("a start in a folder that holds a project is refused: {refused:?}");
        };
        assert!(said.contains("holds /w/"), "{said}");
    }

    #[test]
    fn a_join_finds_its_worktree_s_workstream_or_says_it_made_one() {
        let mut projects = Projects::default();
        clone(&mut projects, "/w/luneta", &["main"]);
        let main = at(&projects, "/w/luneta/main").unwrap();
        let join = |root: &str, at: &str| Change::Join { spot: worktree(root, at) };
        let (same, found) = change(&projects, join("/w/luneta", "main")).unwrap();
        assert_eq!((same == projects, found), (true, Some(Made::Found(main))));
        let Made::Workstream(feat) = add(&mut projects, join("/w/luneta", "feat")) else {
            panic!("a join where no workstream is makes one");
        };
        assert_eq!(at(&projects, "/w/luneta/feat"), Some(feat));
        let (_, again) = change(&projects, join("/w/luneta", "feat")).unwrap();
        assert_eq!(again, Some(Made::Found(feat)), "joined again, it is found");

        let Made::Workstream(elo) = add(&mut projects, join("/w/elo", "main")) else {
            panic!("a join in a clone of no project makes its project and its workstream");
        };
        let (held, _) = projects.find(elo).unwrap();
        assert_eq!((held.name(), held.dir()), (&name("elo"), &dir("/w/elo")));
        let root = Change::Join { spot: clone_of("/w/luneta", &["main", "feat"]) };
        assert!(change(&projects, root).unwrap_err().contains("is in no worktree of /w/luneta"));
        let around = refused(&projects, join("/w", "fix"));
        assert!(matches!(around, Err(Refused::Nests(_))), "{around:?}");
    }

    #[test]
    fn a_pull_makes_its_workstream_in_its_clone_s_project_or_in_one_made_there() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let pull = |root: &str| Change::Pull {
            spot: worktree(root, "pr-7"),
            project: name("cloned"),
            name: name("pr-7"),
        };
        let Made::Workstream(held) = add(&mut projects, pull("/w/luneta")) else {
            panic!("a pull makes a workstream");
        };
        let (project, workstream) = projects.find(held).unwrap();
        assert_eq!((project.id(), workstream.renamed()), (luneta, Some(&name("pr-7"))));
        assert_eq!(projects.all().len(), 1, "a pull into a held clone makes no project");
        let Made::Workstream(made) = add(&mut projects, pull("/w/elo")) else {
            panic!("a pull makes a workstream");
        };
        let (project, workstream) = projects.find(made).unwrap();
        assert_eq!((project.name(), project.dir()), (&name("cloned"), &dir("/w/elo")));
        assert_eq!(project.folder(workstream.id()), Some(dir("/w/elo/pr-7")));
        let (project, name) = (name("w"), name("pr-7"));
        let around = Change::Pull { spot: worktree("/w", "pr-7"), project, name };
        assert!(matches!(refused(&projects, around), Err(Refused::Nests(_))));
    }

    #[test]
    fn a_rename_of_a_workstream_is_kept_until_taken_back() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let to = Some(Name::parse("release").unwrap());
        let rename = Change::RenameWorkstream { workstream: main, to };
        (projects, _) = change(&projects, rename).unwrap();
        let renamed = projects.all()[0].workstreams().next().unwrap().renamed();
        assert_eq!(renamed.map(Name::as_str), Some("release"));
        let back = Change::RenameWorkstream { workstream: main, to: None };
        (projects, _) = change(&projects, back).unwrap();
        assert_eq!(projects.all()[0].workstreams().next().unwrap().renamed(), None);
    }

    #[test]
    fn a_reorder_takes_the_whole_order_and_only_on_the_order_kept() {
        let mut projects = Projects::default();
        let a = clone(&mut projects, "/a", &[]);
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
        let ids: Vec<_> = projects.all()[2].workstreams().map(Workstream::id).collect();
        assert_eq!(ids, [two, one]);
        assert!(is_stale(refused(&projects, swap(a, vec![one, two]))));
        assert!(is_stale(refused(&projects, swap(b, vec![one, two]))));
        let unknown = ProjectId::of(99).unwrap();
        let nowhere = refused(&projects, swap(unknown, vec![one, two]));
        assert!(matches!(nowhere, Err(Refused::Said(why)) if why.contains("no project")));
        let elsewhere = change(&projects, swap(b, vec![own(&projects, b)])).unwrap_err();
        assert!(elsewhere.contains("asked for are not the workstreams kept"), "{elsewhere}");
    }

    #[test]
    fn a_project_is_forgotten_by_hand_only_while_it_holds_no_terminal() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        opened(&mut projects, main, 100, Placed::Alone);
        let refused = change(&projects, Change::ForgetProject { project: luneta });
        assert!(refused.unwrap_err().contains("holds 1 views still"));
        let vault = project(&mut projects, "/w/vault");
        (projects, _) = change(&projects, Change::ForgetProject { project: vault }).unwrap();
        assert_eq!(projects.project_of(vault), None, "a project with no terminal is forgotten");
        assert!(projects.project_of(luneta).is_some());
        let gone = change(&projects, Change::ForgetProject { project: vault });
        assert!(gone.unwrap_err().contains("no project has the id"));
        assert!(projects.find(main).is_some());
    }

    #[test]
    fn a_project_closes_with_all_its_workstreams_and_terminals_in_one_change() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let fix = workstream(&mut projects, luneta, "/w/luneta/fix");
        let vault = clone(&mut projects, "/w/vault", &[]);
        let notes = workstream(&mut projects, vault, "/w/vault/notes");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let two = opened(&mut projects, fix, 101, Placed::Alone);
        let three = opened(&mut projects, notes, 102, Placed::Alone);
        projects = projects.closed(Closing::Project(luneta)).unwrap();
        assert_eq!(projects.project_of(luneta), None);
        assert_eq!((projects.find(main), projects.find(fix)), (None, None));
        assert!(projects.holding_terminal(one).is_none(), "its terminals go with it");
        assert!(projects.holding_terminal(two).is_none(), "its terminals go with it");
        assert!(projects.find(notes).is_some(), "another project stays");
        assert!(projects.holding_terminal(three).is_some(), "another project's terminal stays");
        let gone = projects.closed(Closing::Project(luneta));
        assert!(gone.unwrap_err().contains("no project has the id"));

        let empty = project(&mut projects, "/w/empty");
        projects = projects.closed(Closing::Project(empty)).unwrap();
        assert_eq!(projects.project_of(empty), None, "a project with no workstream closes too");
        assert!(projects.project_of(vault).is_some());
    }

    #[test]
    fn a_project_stays_with_its_workstreams_forgotten_or_closed() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let fix = workstream(&mut projects, luneta, "/w/luneta/fix");
        (projects, _) = change(&projects, Change::ForgetWorkstream { workstream: main }).unwrap();
        (projects, _) = change(&projects, Change::ForgetWorkstream { workstream: fix }).unwrap();
        let held = projects.project_of(luneta).map(Project::holds);
        assert_eq!(held, Some(&Holds::Clone(Vec::new())), "a clone may hold none");
        let gone = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(gone.unwrap_err().contains("no workstream has the id"));

        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        opened(&mut projects, main, 100, Placed::Alone);
        projects = projects.closed(Closing::Workstream(main)).unwrap();
        assert!(layout(&projects, main).is_empty(), "a workstream closes its terminals and stays");
        assert!(projects.project_of(luneta).is_some());
    }

    #[test]
    fn a_drawn_id_already_taken_is_drawn_again() {
        let mut projects = Projects::default();
        let first = project(&mut projects, "/a");
        let mut draws = [first.0, first.0, Id::of(7).unwrap(), Id::of(8).unwrap()].into_iter();
        let change = Change::AddProject { spot: folder("/b"), name: None };
        let (_, made) = projects.changed(change, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(made, Some(Made::Project(ProjectId(Id::of(7).unwrap()))));
        let always = Change::AddProject { spot: folder("/b"), name: None };
        let refused = projects.changed(always, || Ok(first.0));
        assert!(refused.unwrap_err().to_string().contains("were all taken"));
    }

    #[test]
    fn the_projects_and_workstreams_kept_are_bounded() {
        let mut projects = Projects::default();
        for at in 0..PROJECTS_MAX {
            project(&mut projects, &format!("/p{at}"));
        }
        let over = refused(&projects, Change::AddProject { spot: folder("/over"), name: None });
        let Err(Refused::Said(said)) = over else {
            panic!("a project over the bound is no nested one: {over:?}");
        };
        assert!(said.contains("as many as are kept"));
        let mut projects = Projects::default();
        let first = clone(&mut projects, "/c", &[]);
        for at in 0..WORKSTREAMS_MAX {
            workstream(&mut projects, first, &format!("/c/w{at}"));
        }
        let over = add_in(first, worktree("/c", "over"));
        assert!(change(&projects, over).unwrap_err().contains("as many as are kept"));
    }

    #[test]
    fn a_welcome_view_counts_toward_the_views_kept() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/c", &[]);
        let size = Size::of(1000, 1000).unwrap();
        let mut next = 100;
        for at in 0..VIEWS_MAX / PANES_MAX {
            let main = workstream(&mut projects, luneta, &format!("/c/w{at}"));
            for _ in 0..PANES_MAX {
                let view = ViewId::of(next).unwrap();
                next += 1;
                let placed = match layout(&projects, main).tree() {
                    None => Placed::Alone,
                    Some(tree) => {
                        let (at, towards, _) = tree.room_beside_largest(view, size).unwrap();
                        Placed::Beside { at, towards }
                    },
                };
                let (workstream, view) = (main, View::Welcome(view));
                let open = Change::Open { workstream, view, placed, size };
                (projects, _) = change(&projects, open).unwrap();
            }
        }
        assert_eq!(projects.terminals().count(), 0);
        let last = workstream(&mut projects, luneta, "/c/last");
        let made = Terminal::new(terminal_view(next), dir("/c/last"), Runs::Shell);
        let (view, placed) = (View::Terminal(made), Placed::Alone);
        let open = Change::Open { workstream: last, view, placed, size };
        let refused = change(&projects, open).unwrap_err();
        assert!(refused.contains(&format!("there are {VIEWS_MAX} views")), "{refused}");
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
    ) -> ViewId {
        let terminal = ViewId::of(id).unwrap();
        let made = Terminal::new(terminal_view(id), dir("/w/luneta"), Runs::Shell);
        let size = Size::of(80, 24).unwrap();
        let open = Change::Open { workstream, view: View::Terminal(made), placed, size };
        (*projects, _) = change(projects, open).unwrap();
        terminal
    }

    fn terminal_view(id: u64) -> TerminalViewId {
        TerminalViewId::new(ViewId::of(id).unwrap())
    }

    fn layout(projects: &Projects, workstream: WorkstreamId) -> &Layout {
        projects.find(workstream).unwrap().1.layout()
    }

    #[test]
    fn a_layout_opens_closes_and_fills_itself_with_one_of_its_terminals() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        assert!(layout(&projects, main).is_empty());
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, main, 101, right);
        assert_eq!(layout(&projects, main).tree().unwrap().panes(), [one, two]);
        assert_eq!(projects.holding_terminal(two).map(|(it, _)| it.id()), Some(main));

        (projects, _) = change(&projects, Change::Swap { one, other: two }).unwrap();
        assert_eq!(layout(&projects, main).tree().unwrap().panes(), [two, one]);
        let fill = |full| Change::Fill { workstream: main, full };
        (projects, _) = change(&projects, fill(Some(two))).unwrap();
        assert_eq!(layout(&projects, main).full(), Some(two));
        let swap = change(&projects, Change::Swap { one, other: two });
        assert!(swap.unwrap_err().contains("moves none"), "a full layout moves no terminal");
        let (sides, grow) = (layout::Sides::One(layout::Direction::Left), layout::Growth::Grow);
        let size = Size::of(80, 24).unwrap();
        let resize = Change::Resize { view: one, sides, growth: grow, size };
        assert!(change(&projects, resize).unwrap_err().contains("resized by none"));
        let gone = ViewId::of(7).unwrap();
        assert!(change(&projects, fill(Some(gone))).unwrap_err().contains("holds no view"));

        (projects, _) = change(&projects, Change::Close { view: two }).unwrap();
        assert_eq!(layout(&projects, main).full(), None, "a layout of one fills itself with none");
        assert!(change(&projects, fill(Some(one))).unwrap_err().contains("whole already"));
        let refused = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(refused.unwrap_err().contains("holds 1 views still"));
        (projects, _) = change(&projects, Change::Close { view: one }).unwrap();
        assert!(layout(&projects, main).is_empty(), "a workstream stays with its last terminal gone");
    }

    #[test]
    fn a_workstream_stays_when_its_last_terminal_closes() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, main, 101, right);
        (projects, _) = change(&projects, Change::Close { view: one }).unwrap();
        let held_in = projects.holding_terminal(two).map(|(it, _)| it.id());
        assert_eq!(held_in, Some(main), "a terminal not its layout's last leaves the workstream");
        (projects, _) = change(&projects, Change::Close { view: two }).unwrap();
        let left = projects.all()[0].workstreams().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [main]);
        assert_eq!(projects.terminals().count(), 0);
        let gone = change(&projects, Change::Close { view: two });
        assert!(gone.unwrap_err().contains("no view has the id"));
    }

    #[test]
    fn a_moved_terminal_leaves_its_layout_whole_for_another() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let feat = workstream(&mut projects, luneta, "/w/luneta/feat");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, main, 101, right);
        let name = Some(Name::parse("tests").unwrap());
        (projects, _) = change(&projects, Change::RenameTerminal { terminal: two, to: name }).unwrap();
        let size = Size::of(80, 24).unwrap();
        let moving = |terminal: ViewId, to| {
            Change::Move { terminal: TerminalViewId::new(terminal), to, size }
        };
        (projects, _) = change(&projects, moving(two, feat)).unwrap();
        assert_eq!(layout(&projects, main).tree().unwrap().panes(), [one]);
        assert_eq!(layout(&projects, feat).tree().unwrap().panes(), [two]);
        let (_, moved) = projects.holding_terminal(two).unwrap();
        assert_eq!(moved.name().map(Name::as_str), Some("tests"), "a terminal moves whole");
        assert!(change(&projects, moving(two, feat)).unwrap_err().contains("already"));
        let nowhere = WorkstreamId::of(99).unwrap();
        assert!(change(&projects, moving(two, nowhere)).unwrap_err().contains("no workstream"));

        (projects, _) = change(&projects, moving(one, feat)).unwrap();
        assert_eq!(layout(&projects, feat).tree().unwrap().panes(), [two, one]);
        assert!(layout(&projects, main).is_empty(), "a workstream stays with its last moved out");
    }

    #[test]
    fn a_terminal_s_home_is_its_worktree_s_workstream() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &["main", "feat"]);
        let vault = project(&mut projects, "/w/vault");
        let main = at(&projects, "/w/luneta/main").unwrap();
        let feat = at(&projects, "/w/luneta/feat").unwrap();
        let terminal = opened(&mut projects, main, 100, Placed::Alone);
        let home = |spot: Spot| projects.home(terminal, &spot);
        assert_eq!(home(worktree("/w/luneta", "main")), Home::Stays);
        assert_eq!(home(worktree("/w/luneta", "feat")), Home::In(feat));
        let fix = worktree("/w/luneta", "fix");
        assert_eq!(home(fix.clone()), Home::Made { project: luneta, spot: fix });
        assert_eq!(home(clone_of("/w/luneta", &[])), Home::Stays, "no worktree holds the root");
        assert_eq!(home(folder("/w/vault/notes")), Home::In(own(&projects, vault)));
        assert_eq!(home(folder("/home/me")), Home::Stays, "outside every project");
        assert_eq!(home(worktree("/home/me/elo", "main")), Home::Stays);
        let gone = ViewId::of(7).unwrap();
        assert_eq!(projects.home(gone, &worktree("/w/luneta", "feat")), Home::Stays);
    }

    #[test]
    fn an_open_taken_back_leaves_its_workstream_even_empty() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let unopen = || Change::Unopen { terminal: TerminalViewId::new(one) };
        (projects, _) = change(&projects, unopen()).unwrap();
        assert!(layout(&projects, main).is_empty());
        let left = projects.all()[0].workstreams().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [main], "a workstream an open failed in stays");
        let gone = change(&projects, unopen());
        assert!(gone.unwrap_err().contains("no view has the id"));
    }

    #[test]
    fn a_workstream_closes_its_terminals_where_a_forget_is_refused() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let kept = workstream(&mut projects, luneta, "/w/luneta/fix");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        opened(&mut projects, main, 101, right);
        let forget = change(&projects, Change::ForgetWorkstream { workstream: main });
        assert!(forget.unwrap_err().contains("holds 2 views still"));
        projects = projects.closed(Closing::Workstream(main)).unwrap();
        let left = projects.all()[0].workstreams().map(|it| it.id);
        assert_eq!(left.collect::<Vec<_>>(), [main, kept]);
        assert_eq!(projects.terminals().count(), 0);
        let unknown = projects.closed(Closing::Workstream(WorkstreamId::of(99).unwrap()));
        assert!(unknown.unwrap_err().contains("no workstream has the id"));
    }

    #[test]
    fn the_workstream_gone_to_last_is_the_one_used_last_whatever_the_clock_says() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
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
    fn a_workstream_stays_in_its_worktree_wherever_its_terminals_go() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let terminal = TerminalViewId::new(one);
        let ran = Ran { terminal, folder: dir("/w/luneta/fix/src"), runs: Runs::Shell };
        (projects, _) = change(&projects, Change::Ran(vec![ran])).unwrap();
        let (project, workstream) = projects.find(main).unwrap();
        assert_eq!(project.folder(workstream.id()), Some(dir("/w/luneta/main")));
        assert_eq!(projects.holding_terminal(one).unwrap().1.folder(), &dir("/w/luneta/fix/src"));
    }

    #[test]
    fn a_listing_of_a_clone_s_worktrees_makes_their_workstreams_and_forgets_the_empty_rest() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &["main", "gone", "held"]);
        let vault = project(&mut projects, "/w/vault");
        let held = at(&projects, "/w/luneta/held").unwrap();
        opened(&mut projects, held, 100, Placed::Alone);
        let main = at(&projects, "/w/luneta/main").unwrap();
        let listed = |projects: &Projects, root: &str, at: &[&str]| {
            let mut clones = projects.clones();
            let seen = clones.find(|(it, _)| *it == dir(root)).map(|(_, seen)| seen);
            let seen = seen.unwrap_or_default();
            let worktrees = at.iter().copied().map(tree).collect();
            Change::Listed { root: dir(root), worktrees, seen }
        };
        let stale = listed(&projects, "/w/luneta", &["main", "new"]);
        let late = workstream(&mut projects, luneta, "/w/luneta/late");
        (projects, _) = change(&projects, stale).unwrap();
        let kept = at(&projects, "/w/luneta/late");
        assert_eq!(kept, Some(late), "one made since it was listed stays");
        let again = listed(&projects, "/w/luneta", &["main", "new"]);
        (projects, _) = change(&projects, again).unwrap();
        assert_eq!(at(&projects, "/w/luneta/late"), None, "seen and not listed, it goes");
        let held = projects.project_of(luneta).unwrap();
        let folders: Vec<Dir> = held.placed().map(|(folder, _)| folder).collect();
        assert_eq!(folders, ["/w/luneta/held", "/w/luneta/main", "/w/luneta/new"].map(dir));
        assert_eq!(at(&projects, "/w/luneta/main"), Some(main), "listed again, it keeps its own");
        let (same, _) = change(&projects, listed(&projects, "/w/vault", &[])).unwrap();
        assert_eq!(same, projects, "a folder's project lists no worktrees");
        assert_eq!(own(&projects, vault), own(&same, vault));
        let unknown = change(&projects, listed(&projects, "/w/elo", &["main"])).unwrap().0;
        assert_eq!(unknown, projects, "a clone of no project");
    }

    #[test]
    fn a_worktree_gone_takes_a_workstream_that_holds_welcome_views_alone() {
        let mut projects = Projects::default();
        clone(&mut projects, "/w/luneta", &["main", "welcomed", "both"]);
        let welcome = |projects: &mut Projects, workstream, id, placed| {
            let view = View::Welcome(ViewId::of(id).unwrap());
            let size = Size::of(80, 24).unwrap();
            let open = Change::Open { workstream, view, placed, size };
            (*projects, _) = change(projects, open).unwrap();
        };
        let welcomed = at(&projects, "/w/luneta/welcomed").unwrap();
        welcome(&mut projects, welcomed, 100, Placed::Alone);
        let both = at(&projects, "/w/luneta/both").unwrap();
        let one = opened(&mut projects, both, 101, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        welcome(&mut projects, both, 102, right);
        let seen = projects.clones().find(|(it, _)| *it == dir("/w/luneta")).unwrap().1;
        let worktrees = [tree("main")].into();
        let listed = Change::Listed { root: dir("/w/luneta"), worktrees, seen };
        (projects, _) = change(&projects, listed).unwrap();
        assert_eq!(at(&projects, "/w/luneta/welcomed"), None, "a welcome view holds nothing");
        assert!(projects.holding_view(ViewId::of(100).unwrap()).is_none(), "it goes with it");
        assert_eq!(at(&projects, "/w/luneta/both"), Some(both), "a terminal keeps it");
        assert_eq!(layout(&projects, both).len(), 2, "with its welcome view");
    }

    #[test]
    fn a_folder_s_project_becomes_its_clone_s_with_its_workstream_where_its_terminals_are() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let own = own(&projects, luneta);
        let one = opened(&mut projects, own, 100, Placed::Alone);
        let right = Placed::Beside { at: one, towards: layout::Towards::Right };
        let two = opened(&mut projects, own, 101, right);
        let ran = |terminal: ViewId, at: &str| {
            Ran { terminal: TerminalViewId::new(terminal), folder: dir(at), runs: Runs::Shell }
        };
        let both = vec![ran(one, "/w/luneta/feat/src"), ran(two, "/w/luneta/feat")];
        (projects, _) = change(&projects, Change::Ran(both)).unwrap();
        let rooted = |root: &str, at: &[&str]| {
            let worktrees = at.iter().copied().map(tree).collect();
            Change::Rooted { root: dir(root), worktrees }
        };
        let (same, _) = change(&projects, rooted("/w/luneta", &[])).unwrap();
        assert_eq!(same, projects, "with no worktree yet, it stays a folder's");
        let (same, _) = change(&projects, rooted("/w/elo", &["main"])).unwrap();
        assert_eq!(same, projects, "a clone of no project");

        (projects, _) = change(&projects, rooted("/w/luneta", &["feat", "main"])).unwrap();
        let held = projects.project_of(luneta).unwrap();
        assert!(is_clone(held));
        assert_eq!(held.folder(own), Some(dir("/w/luneta/feat")), "it goes where its terminals are");
        let homes = [one, two].map(|it| projects.holding_terminal(it).map(|(home, _)| home.id()));
        assert_eq!(homes, [Some(own), Some(own)]);
        assert!(at(&projects, "/w/luneta/main").is_some_and(|it| it != own));
        let again = rooted("/w/luneta", &["feat", "fix", "main"]);
        let (same, _) = change(&projects, again).unwrap();
        assert_eq!(same, projects, "a clone's project is rooted once, and listed after");
    }

    #[test]
    fn a_clone_s_project_is_checked_to_hold_one_workstream_per_worktree() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &["main"]);
        project(&mut projects, "/w/vault");
        let with = |projects: &Projects, at: &str| {
            let mut all = projects.all().to_vec();
            let held = all.iter_mut().find(|it| it.id == luneta).unwrap();
            let Holds::Clone(rows) = &mut held.holds else { panic!("luneta is a clone") };
            let mut copy = rows[0].1.clone();
            copy.id = WorkstreamId::of(999).unwrap();
            rows.push((tree(at), copy));
            Projects::checked(all)
        };
        let twice = with(&projects, "main").unwrap_err();
        assert_eq!(twice, "/w/luneta/main holds two of project luneta's workstreams");
        assert!(with(&projects, "fix").is_ok());
    }

    #[test]
    fn a_terminal_keeps_the_name_it_is_given_until_taken_back() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let name = |projects: &Projects| projects.holding_terminal(one).unwrap().1.name().cloned();
        assert_eq!(name(&projects), None);
        let to = Some(Name::parse("tests").unwrap());
        (projects, _) = change(&projects, Change::RenameTerminal { terminal: one, to }).unwrap();
        assert_eq!(name(&projects).as_ref().map(Name::as_str), Some("tests"));
        let terminal = TerminalViewId::new(one);
        let ran = Ran { terminal, folder: dir("/w/luneta/src"), runs: Runs::Shell };
        (projects, _) = change(&projects, Change::Ran(vec![ran])).unwrap();
        assert!(name(&projects).is_some(), "a terminal keeps its name whatever it runs");
        let back = Change::RenameTerminal { terminal: one, to: None };
        (projects, _) = change(&projects, back).unwrap();
        assert_eq!(name(&projects), None);
        let other = ViewId::of(7).unwrap();
        let unknown = Change::RenameTerminal { terminal: other, to: None };
        assert!(change(&projects, unknown).unwrap_err().contains("no terminal has the id"));
    }

    #[test]
    fn a_welcome_view_is_laid_out_and_closed_as_a_terminal_is_but_runs_nothing() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let one = opened(&mut projects, main, 100, Placed::Alone);
        let welcome = ViewId::of(101).unwrap();
        let placed = Placed::Beside { at: one, towards: layout::Towards::Right };
        let size = Size::of(80, 24).unwrap();
        let open = Change::Open { workstream: main, view: View::Welcome(welcome), placed, size };
        (projects, _) = change(&projects, open).unwrap();
        assert_eq!(layout(&projects, main).tree().unwrap().panes(), [one, welcome]);
        let terminals = projects.terminals().map(|(_, it)| it.id().view());
        let terminals: Vec<ViewId> = terminals.collect();
        assert_eq!(terminals, [one], "a welcome view is no terminal");
        assert!(projects.holding_terminal(welcome).is_none());
        assert_eq!(projects.holding_view(welcome).map(|(it, _)| it.id()), Some(main));
        let rename = Change::RenameTerminal { terminal: welcome, to: None };
        assert!(change(&projects, rename).unwrap_err().contains("runs no terminal"));
        let twice = Change::Open { workstream: main, view: View::Welcome(welcome), placed, size };
        assert!(change(&projects, twice).unwrap_err().contains("is taken"));

        (projects, _) = change(&projects, Change::Close { view: one }).unwrap();
        assert_eq!(layout(&projects, main).tree().unwrap().panes(), [welcome]);
        (projects, _) = change(&projects, Change::Close { view: welcome }).unwrap();
        assert!(layout(&projects, main).is_empty(), "a workstream stays with its last view gone");
    }

    #[test]
    fn a_terminal_is_placed_only_where_its_layout_has_it_and_its_id_is_free() {
        let mut projects = Projects::default();
        let luneta = clone(&mut projects, "/w/luneta", &[]);
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let size = Size::of(80, 24).unwrap();
        let terminal = |id| Terminal::new(terminal_view(id), dir("/w/luneta"), Runs::Shell);
        let open = |id, placed| {
            Change::Open { workstream: main, view: View::Terminal(terminal(id)), placed, size }
        };
        let at = ViewId::of(9).unwrap();
        let nowhere = Placed::Beside { at, towards: layout::Towards::Down };
        assert!(change(&projects, open(100, nowhere)).unwrap_err().contains("layout is empty"));
        let one = opened(&mut projects, main, 100, Placed::Alone);
        assert!(change(&projects, open(101, Placed::Alone)).unwrap_err().contains("already"));
        assert!(change(&projects, open(100, nowhere)).unwrap_err().contains("is taken"));
        assert!(change(&projects, open(main.get(), nowhere)).unwrap_err().contains("is taken"));
        let tiny = Size::of(5, 3).unwrap();
        let beside = Placed::Beside { at: one, towards: layout::Towards::Right };
        let (terminal, placed) = (terminal(102), beside);
        let view = View::Terminal(terminal);
        let cramped = Change::Open { workstream: main, view, placed, size: tiny };
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
