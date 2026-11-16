use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _};
use std::num::NonZeroU64;
use std::path::{Component, Path};
use std::str::FromStr;
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use luneta_protocol::github::{PullRequest, State, Verdict};

use crate::api;
use crate::core::{self, Command};
use crate::watch::Troubles;

mod file;

pub use file::{Failed, Store};

pub const PROJECTS_MAX: usize = 64;

pub const WORKSTREAMS_MAX: usize = 512;

pub const NAME_CHARS_MAX: usize = 64;

pub const PATH_BYTES_MAX: usize = 4096;

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

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
        let below = if folder == self {
            Below(String::new())
        } else {
            Below(folder.0.strip_prefix(&self.0)?.strip_prefix('/')?.to_owned())
        };
        assert_eq!(&self.join(&below), folder);
        Some(below)
    }

    #[must_use]
    pub fn join(&self, below: &Below) -> Dir {
        if below.0.is_empty() {
            return self.clone();
        }
        Dir(format!("{}/{}", self.0, below.0))
    }

    pub fn found(path: &str) -> Result<Dir, String> {
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Below(String);

impl Below {
    pub fn parse(text: &str) -> Result<Below, String> {
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Project {
    id: Id,
    name: Name,
    dir: Dir,
    workstreams: Vec<Workstream>,
}

impl Project {
    #[must_use]
    pub fn id(&self) -> Id {
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
    id: Id,
    renamed: Option<Name>,
    below: Below,
}

impl Workstream {
    #[must_use]
    pub fn id(&self) -> Id {
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
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Projects {
    projects: Vec<Project>,
}

#[derive(Debug)]
pub enum Change {
    AddProject { dir: Dir, name: Option<Name> },
    AddWorkstream { project: Id, folder: Dir, name: Option<Name> },
    RenameProject { project: Id, to: Name },
    RenameWorkstream { workstream: Id, to: Option<Name> },
    MoveProject { project: Id, to: usize },
    MoveWorkstream { workstream: Id, to: usize },
    ForgetProject { project: Id },
    ForgetWorkstream { workstream: Id },
}

impl Change {
    pub fn of_api(change: api::change_projects_request::Change) -> Result<Change, String> {
        use api::change_projects_request::{self as asked, Change as Asked};
        let field = |field: &'static str| move |why: String| format!("{field}: {why}");
        let id = |field: &'static str, id: u64| {
            Id::of(id).ok_or_else(|| format!("{field}: no id is 0"))
        };
        let name = |name: Option<String>| name.as_deref().map(Name::parse).transpose();
        let place = |to: u32| usize::try_from(to).expect("a u32 fits a usize");
        Ok(match change {
            Asked::AddProject(asked::AddProject { dir, name: named }) => Change::AddProject {
                dir: Dir::found(&dir).map_err(field("dir"))?,
                name: name(named).map_err(field("name"))?,
            },
            Asked::AddWorkstream(asked::AddWorkstream { project, folder, name: named }) => {
                Change::AddWorkstream {
                    project: id("project", project)?,
                    folder: Dir::found(&folder).map_err(field("folder"))?,
                    name: name(named).map_err(field("name"))?,
                }
            },
            Asked::RenameProject(asked::RenameProject { project, to }) => Change::RenameProject {
                project: id("project", project)?,
                to: Name::parse(&to).map_err(field("to"))?,
            },
            Asked::RenameWorkstream(asked::RenameWorkstream { workstream, to }) => {
                Change::RenameWorkstream {
                    workstream: id("workstream", workstream)?,
                    to: name(to).map_err(field("to"))?,
                }
            },
            Asked::MoveProject(asked::MoveProject { project, to }) => {
                Change::MoveProject { project: id("project", project)?, to: place(to) }
            },
            Asked::MoveWorkstream(asked::MoveWorkstream { workstream, to }) => {
                Change::MoveWorkstream { workstream: id("workstream", workstream)?, to: place(to) }
            },
            Asked::ForgetProject(asked::ForgetProject { project }) => {
                Change::ForgetProject { project: id("project", project)? }
            },
            Asked::ForgetWorkstream(asked::ForgetWorkstream { workstream }) => {
                Change::ForgetWorkstream { workstream: id("workstream", workstream)? }
            },
        })
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
        let Projects { projects } = projects;
        let mut ids = BTreeSet::new();
        for (at, project) in projects.iter().enumerate() {
            let workstreams = project.workstreams.iter().map(|workstream| workstream.id);
            for id in std::iter::once(project.id).chain(workstreams) {
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
        mut draw: impl FnMut() -> io::Result<Id>,
    ) -> Result<(Projects, Option<Id>), String> {
        let mut changed = self.clone();
        let made = match change {
            Change::AddProject { dir, name } => {
                let name = match name {
                    Some(name) => name,
                    None => Name::parse(dir.last()).map_err(|why| format!("name it: {why}"))?,
                };
                if let Some(other) = self.projects.iter().find(|other| other.dir.nests(&dir)) {
                    return Err(nested(&dir, other));
                }
                if self.projects.len() >= PROJECTS_MAX {
                    return Err(format!("there are {PROJECTS_MAX} projects, as many as are kept"));
                }
                let id = self.fresh(&mut draw)?;
                changed.projects.push(Project { id, name, dir, workstreams: Vec::new() });
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
                project.workstreams.push(Workstream { id, renamed: name, below });
                Some(id)
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
            Change::MoveProject { project, to } => {
                let at = changed.projects.iter().position(|it| it.id == project);
                let at = at.ok_or_else(|| unknown("project", project))?;
                moved(&mut changed.projects, at, to)?;
                None
            },
            Change::MoveWorkstream { workstream, to } => {
                let (project, at) = changed.workstream(workstream)?;
                moved(&mut project.workstreams, at, to)?;
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
                project.workstreams.remove(at);
                None
            },
        };
        assert_eq!(Projects::checked(changed.projects.clone()).as_ref(), Ok(&changed));
        Ok((changed, made))
    }

    fn fresh(&self, draw: &mut impl FnMut() -> io::Result<Id>) -> Result<Id, String> {
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

    fn holds(&self, id: Id) -> bool {
        self.projects.iter().any(|project| {
            project.id == id || project.workstreams.iter().any(|workstream| workstream.id == id)
        })
    }

    fn project(&mut self, id: Id) -> Result<&mut Project, String> {
        let project = self.projects.iter_mut().find(|project| project.id == id);
        project.ok_or_else(|| unknown("project", id))
    }

    fn workstream(&mut self, id: Id) -> Result<(&mut Project, usize), String> {
        self.projects
            .iter_mut()
            .find_map(|project| {
                let at = project.workstreams.iter().position(|it| it.id == id)?;
                Some((project, at))
            })
            .ok_or_else(|| unknown("workstream", id))
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

fn unknown(what: &str, id: Id) -> String {
    format!("no {what} has the id {id}")
}

fn moved<T>(items: &mut Vec<T>, at: usize, to: usize) -> Result<(), String> {
    assert!(at < items.len());
    if to >= items.len() {
        let len = items.len();
        return Err(format!("{to} is past the last of {len} places, which is {}", len - 1));
    }
    let item = items.remove(at);
    items.insert(to, item);
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

    fn change(projects: &Projects, change: Change) -> Result<(Projects, Option<Id>), String> {
        let mut drawn = counting();
        let taken = |id: &Id| projects.holds(*id);
        projects.changed(change, || loop {
            let id = drawn()?;
            if !taken(&id) {
                return Ok(id);
            }
        })
    }

    fn add(projects: &mut Projects, change_of: Change) -> Id {
        let (changed, made) = change(projects, change_of).unwrap();
        *projects = changed;
        made.unwrap()
    }

    fn project(projects: &mut Projects, at: &str) -> Id {
        add(projects, Change::AddProject { dir: dir(at), name: None })
    }

    fn workstream(projects: &mut Projects, project: Id, at: &str) -> Id {
        add(projects, Change::AddWorkstream { project, folder: dir(at), name: None })
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
    fn a_dir_is_an_absolute_path_made_only_of_names() {
        assert_eq!(dir("/home/me/luneta").last(), "luneta");
        for refused in ["/", "home", "/a/", "/a//b", "/a/./b", "/a/../b", "/a\tb"] {
            assert!(Dir::parse(refused).is_err(), "{refused:?}");
        }
        assert!(Below::parse("").is_ok());
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
        let nowhere = real.join("nowhere");
        assert!(Dir::found(nowhere.to_str().unwrap()).unwrap_err().contains("No such file"));
    }

    #[test]
    fn a_folder_is_below_a_dir_only_by_whole_names() {
        let luneta = dir("/w/luneta");
        assert_eq!(luneta.below(&dir("/w/luneta")).unwrap().as_str(), "");
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
            let refused = change(&projects, Change::AddProject { dir: dir(at), name: None });
            assert!(refused.unwrap_err().contains(why), "{at}");
        }
        project(&mut projects, "/w/luneta-old");
        assert_eq!(projects.all().len(), 2);
    }

    #[test]
    fn a_workstream_is_in_its_project_s_folder_or_below_it_and_may_share_it() {
        let mut projects = Projects::default();
        let luneta = project(&mut projects, "/w/luneta");
        let main = workstream(&mut projects, luneta, "/w/luneta/main");
        let again = workstream(&mut projects, luneta, "/w/luneta/main");
        let top = workstream(&mut projects, luneta, "/w/luneta");
        assert_eq!(projects.all()[0].workstreams().len(), 3);
        assert!(main != again && again != top);
        let folders: Vec<Dir> = projects.folders().collect();
        let want = ["/w/luneta", "/w/luneta/main", "/w/luneta/main", "/w/luneta"].map(dir);
        assert_eq!(folders, want);
        let outside = Change::AddWorkstream { project: luneta, folder: dir("/w/elo"), name: None };
        assert!(change(&projects, outside).unwrap_err().contains("is not in /w/luneta"));
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
    fn a_move_puts_it_at_the_place_asked_among_its_own() {
        let mut projects = Projects::default();
        let a = project(&mut projects, "/a");
        let b = project(&mut projects, "/b");
        let c = project(&mut projects, "/c");
        (projects, _) = change(&projects, Change::MoveProject { project: c, to: 0 }).unwrap();
        let ids: Vec<Id> = projects.all().iter().map(Project::id).collect();
        assert_eq!(ids, [c, a, b]);
        let past = change(&projects, Change::MoveProject { project: a, to: 3 });
        assert!(past.unwrap_err().contains("past the last of 3 places"));

        let one = workstream(&mut projects, b, "/b/one");
        let two = workstream(&mut projects, b, "/b/two");
        let first = Change::MoveWorkstream { workstream: two, to: 0 };
        (projects, _) = change(&projects, first).unwrap();
        let ids: Vec<Id> = projects.all()[2].workstreams().iter().map(Workstream::id).collect();
        assert_eq!(ids, [two, one]);
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
        let mut draws = [first, first, Id::of(7).unwrap()].into_iter();
        let change = Change::AddProject { dir: dir("/b"), name: None };
        let (_, made) = projects.changed(change, || Ok(draws.next().unwrap())).unwrap();
        assert_eq!(made, Id::of(7));
        let always = Change::AddProject { dir: dir("/b"), name: None };
        let refused = projects.changed(always, || Ok(first));
        assert!(refused.unwrap_err().contains("were all taken"));
    }

    #[test]
    fn the_projects_and_workstreams_kept_are_bounded() {
        let mut projects = Projects::default();
        for at in 0..PROJECTS_MAX {
            project(&mut projects, &format!("/p{at}"));
        }
        let over = change(&projects, Change::AddProject { dir: dir("/over"), name: None });
        assert!(over.unwrap_err().contains("as many as are kept"));
        let first = projects.all()[0].id();
        for _ in 0..WORKSTREAMS_MAX {
            workstream(&mut projects, first, "/p0");
        }
        let over = Change::AddWorkstream { project: first, folder: dir("/p0"), name: None };
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

    fn pull_request(state: State, failed: u32) -> PullRequest {
        let checks = Some(Checks { pending: 0, failed, passed: 1 });
        let number = NonZeroU32::new(1).unwrap();
        PullRequest { number, state, url: None, checks, stale: false }
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
