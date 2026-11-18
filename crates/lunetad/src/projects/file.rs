use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::layout::{Leaf as _, Preordered, Towards, NODES_MAX};
use super::{Below, Change, Dir, Id, Line, Name, Project, Projects, Refused, Runs, Session};
use super::{Terminal, TerminalId, Used, WorkstreamId};
use super::{View, Workstream};
use super::{LINE_BYTES_MAX, NAME_CHARS_MAX, PATH_BYTES_MAX, PROJECTS_MAX, SESSION_BYTES_MAX};
use super::{TERMINALS_MAX, WORKSTREAMS_MAX};
use crate::agent::Harness;
use crate::api;
use crate::core::Command;
use crate::log;
use crate::state::{self, Unset};

pub const FILE: &str = "projects";

const LOCK: &str = "projects.lock";

const HELD_TRIES: u32 = 40;

const HELD_PAUSE: Duration = Duration::from_millis(50);

const VERSION: u32 = 4;

const WITHOUT_VIEWS: u32 = 1;

const WITHOUT_UNRUN: u32 = 2;

const WITHOUT_NAMES_OR_USE: u32 = 3;

const CHAR_BYTES_MAX: usize = 4;

const ESCAPED_BYTES_PER_BYTE: usize = 2;

const NAME_BYTES_MAX: usize = CHAR_BYTES_MAX * NAME_CHARS_MAX;

const ENTRY_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE * (PATH_BYTES_MAX + NAME_BYTES_MAX) + 256;

const TERMINAL_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE
    * (PATH_BYTES_MAX + LINE_BYTES_MAX + SESSION_BYTES_MAX + NAME_BYTES_MAX)
    + 256;

const SPLIT_BYTES_MAX: usize = 128;

const FILE_BYTES_MAX: usize = 32 * 1024 * 1024;

const _: () = assert!(
    (PROJECTS_MAX + WORKSTREAMS_MAX) * ENTRY_BYTES_MAX
        + TERMINALS_MAX * TERMINAL_BYTES_MAX
        + WORKSTREAMS_MAX * NODES_MAX * SPLIT_BYTES_MAX
        <= FILE_BYTES_MAX
);

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept {
    version: u32,
    projects: Vec<KeptProject>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptProject {
    id: String,
    name: String,
    dir: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    workstreams: Vec<KeptWorkstream>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptWorkstream {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default)]
    below: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    view: Option<KeptView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    used_millis: Option<u64>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    full: Option<String>,
    tree: Vec<KeptNode>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum KeptNode {
    Split(KeptSplit),
    Terminal(KeptTerminal),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptSplit {
    towards: KeptTowards,
    share: u32,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptTowards {
    Right,
    Down,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptTerminal {
    id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    folder: String,
    runs: KeptRuns,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
enum KeptRuns {
    Shell,
    Command(String),
    Unrun(String),
    Agent(KeptAgent),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptAgent {
    harness: KeptHarness,
    session: String,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum KeptHarness {
    ClaudeCode,
    Pi,
}

pub struct Store {
    keeping: Keeping,
    revision: Revision,
    core: SyncSender<Command>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Revision(u64);

impl Revision {
    #[must_use]
    pub fn to_api(self) -> api::Version {
        api::Version { pid: std::process::id(), seq: self.0 }
    }

    fn next(self) -> Revision {
        Revision(self.0.checked_add(1).expect("projects change fewer than 2^64 times"))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Changed {
    pub made: Option<Id>,
    pub kept: Revision,
}

enum Keeping {
    Kept { file: PathBuf, projects: Projects, written: Projects, _lock: File },
    Unkept(String),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    NotKept(String),
    Nests(String),
    Stale(String),
    Refused(String),
    Unwritten(String),
    Unclosed(String),
    KeptUntold,
}

impl Store {
    pub fn open(core: SyncSender<Command>) -> io::Result<Store> {
        Store::at(state::path(FILE), core)
    }

    pub fn at(file: Result<PathBuf, Unset>, core: SyncSender<Command>) -> io::Result<Store> {
        let keeping = match file {
            Err(unset) => Keeping::Unkept(format!("projects are kept nowhere: {unset}")),
            Ok(file) => Keeping::of(file),
        };
        let store = Store { keeping, revision: Revision::default(), core };
        store.tell().map_err(|_| io::Error::other("the daemon's core has stopped"))?;
        Ok(store)
    }

    #[must_use]
    pub fn unkept(&self) -> Option<&str> {
        match &self.keeping {
            Keeping::Kept { .. } => None,
            Keeping::Unkept(why) => Some(why),
        }
    }

    #[must_use]
    pub fn projects(&self) -> Option<&Projects> {
        match &self.keeping {
            Keeping::Kept { projects, .. } => Some(projects),
            Keeping::Unkept(_) => None,
        }
    }

    pub fn fresh(&self) -> Result<TerminalId, Failed> {
        match &self.keeping {
            Keeping::Kept { projects, .. } => {
                let fresh = projects.fresh(&mut Id::random);
                fresh.map(TerminalId::new).map_err(Failed::Refused)
            },
            Keeping::Unkept(why) => Err(Failed::NotKept(why.clone())),
        }
    }

    pub fn change(&mut self, change: Change) -> Result<Option<Id>, Failed> {
        let at_once = change.kept_at_once();
        let (changed, made) = self.applied(change)?;
        if self.projects() == Some(&changed) {
            return Ok(made);
        }
        if at_once {
            self.written_as(&changed)?;
        }
        self.took(changed)?;
        Ok(made)
    }

    pub fn followed(&mut self, change: Change) -> Result<Option<String>, Failed> {
        let (changed, made) = self.applied(change)?;
        assert!(made.is_none(), "a change that follows the mux makes no id");
        if self.projects() == Some(&changed) {
            return Ok(None);
        }
        let unwritten = match self.written_as(&changed) {
            Ok(()) => None,
            Err(Failed::Unwritten(why)) => Some(why),
            Err(failed) => return Err(failed),
        };
        self.took(changed)?;
        Ok(unwritten)
    }

    fn applied(&self, change: Change) -> Result<(Projects, Option<Id>), Failed> {
        let projects = match &self.keeping {
            Keeping::Kept { projects, .. } => projects,
            Keeping::Unkept(why) => return Err(Failed::NotKept(why.clone())),
        };
        projects.changed(change, Id::random).map_err(|refused| match refused {
            Refused::Nests(why) => Failed::Nests(why),
            Refused::Stale(why) => Failed::Stale(why),
            Refused::Said(why) => Failed::Refused(why),
        })
    }

    fn took(&mut self, changed: Projects) -> Result<(), Failed> {
        let Keeping::Kept { projects, .. } = &mut self.keeping else {
            unreachable!("projects changed are kept");
        };
        *projects = changed;
        self.revision = self.revision.next();
        self.tell()
    }

    #[must_use]
    pub fn revision(&self) -> Revision {
        self.revision
    }

    pub fn flush(&mut self) -> Result<(), Failed> {
        let Keeping::Kept { projects, written, .. } = &self.keeping else {
            return Ok(());
        };
        if projects == written {
            return Ok(());
        }
        let projects = projects.clone();
        self.written_as(&projects)
    }

    fn written_as(&mut self, changed: &Projects) -> Result<(), Failed> {
        let Keeping::Kept { file, written, .. } = &mut self.keeping else {
            unreachable!("only kept projects are written");
        };
        if let Err(why) = unchanged(file, written) {
            log::say(&why);
            self.keeping = Keeping::Unkept(why.clone());
            let _ = self.tell();
            return Err(Failed::NotKept(why));
        }
        let unwritten = |error: io::Error| Failed::Unwritten(format!("{}: {error}", file.display()));
        write(file, changed).map_err(unwritten)?;
        *written = changed.clone();
        Ok(())
    }

    fn tell(&self) -> Result<(), Failed> {
        let told = match &self.keeping {
            Keeping::Kept { projects, .. } => Ok(projects.clone()),
            Keeping::Unkept(why) => Err(why.clone()),
        };
        let told = Command::Projects { projects: told, revision: self.revision };
        self.core.send(told).map_err(|_| Failed::KeptUntold)
    }
}

impl Keeping {
    fn of(file: PathBuf) -> Keeping {
        let lock = match locked(&file) {
            Ok(lock) => lock,
            Err(why) => return Keeping::Unkept(why),
        };
        match read(&file) {
            Ok(projects) => {
                let written = projects.clone();
                Keeping::Kept { file, projects, written, _lock: lock }
            },
            Err(why) => Keeping::Unkept(left(&file, &why)),
        }
    }
}

fn locked(file: &Path) -> Result<File, String> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let path = folder.join(LOCK);
    let failed = |at: &Path, error: io::Error| {
        format!("{}: {error}, and so no projects are kept", at.display())
    };
    fs::create_dir_all(folder).map_err(|error| failed(folder, error))?;
    let mut options = OpenOptions::new();
    let opened = options.create(true).truncate(false).write(true).open(&path);
    let lock = opened.map_err(|error| failed(&path, error))?;
    for _ in 0..HELD_TRIES {
        match lock.try_lock() {
            Ok(()) => return Ok(lock),
            Err(TryLockError::WouldBlock) => thread::sleep(HELD_PAUSE),
            Err(TryLockError::Error(error)) => return Err(failed(&path, error)),
        }
    }
    let folder = folder.display();
    Err(format!("another lunetad keeps the projects in {folder}, and so none are kept here"))
}

fn unchanged(file: &Path, projects: &Projects) -> Result<(), String> {
    match read(file) {
        Ok(now) if now == *projects => Ok(()),
        Ok(_) => Err(left(file, "it changed since lunetad read it")),
        Err(why) => Err(left(file, &why)),
    }
}

fn left(file: &Path, why: &str) -> String {
    format!("{}: {why}, and so it is left as it is", file.display())
}

fn read(file: &Path) -> Result<Projects, String> {
    let mut text = String::new();
    let opened = match File::open(file) {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Projects::default()),
        Err(error) => return Err(error.to_string()),
    };
    let limit = u64::try_from(FILE_BYTES_MAX).expect("8 MiB fits a u64") + 1;
    opened.take(limit).read_to_string(&mut text).map_err(|error| error.to_string())?;
    if text.len() > FILE_BYTES_MAX {
        return Err(format!("it is over {FILE_BYTES_MAX} bytes"));
    }
    parse(&text)
}

fn parse(text: &str) -> Result<Projects, String> {
    let Versioned { version } = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if !(WITHOUT_VIEWS..=VERSION).contains(&version) {
        let read = format!("this lunetad reads only {WITHOUT_VIEWS} to {VERSION}");
        return Err(format!("it is of version {version}, and {read}"));
    }
    let kept: Kept = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let workstreams = || kept.projects.iter().flat_map(|project| &project.workstreams);
    if version == WITHOUT_VIEWS && workstreams().any(|it| it.view.is_some()) {
        return Err(format!("it is of version {WITHOUT_VIEWS}, which holds no views"));
    }
    let mut nodes = workstreams().filter_map(|it| it.view.as_ref()).flat_map(|view| &view.tree);
    let unrun = |node: &KeptNode| {
        matches!(node, KeptNode::Terminal(KeptTerminal { runs: KeptRuns::Unrun(_), .. }))
    };
    if version <= WITHOUT_UNRUN && nodes.clone().any(unrun) {
        return Err(format!("it is of version {version}, which holds no line left unrun"));
    }
    let named = |node: &KeptNode| {
        matches!(node, KeptNode::Terminal(KeptTerminal { name: Some(_), .. }))
    };
    if version <= WITHOUT_NAMES_OR_USE {
        if nodes.any(named) {
            return Err(format!("it is of version {version}, which holds no terminal's name"));
        }
        if workstreams().any(|it| it.used_millis.is_some()) {
            return Err(format!("it is of version {version}, which holds no workstream's use"));
        }
    }
    let projects = kept.projects.into_iter().map(KeptProject::read);
    Projects::checked(projects.collect::<Result<_, _>>()?)
}

impl KeptProject {
    fn read(self) -> Result<Project, String> {
        let workstreams = self.workstreams.into_iter().map(KeptWorkstream::read);
        Ok(Project {
            id: self.id.parse()?,
            name: Name::parse(&self.name)?,
            dir: Dir::parse(&self.dir)?,
            workstreams: workstreams.collect::<Result<_, _>>()?,
        })
    }

    fn of(project: &Project) -> KeptProject {
        KeptProject {
            id: project.id.to_string(),
            name: project.name.0.clone(),
            dir: project.dir.0.clone(),
            workstreams: project.workstreams.iter().map(KeptWorkstream::of).collect(),
        }
    }
}

impl KeptWorkstream {
    fn read(self) -> Result<Workstream, String> {
        let id: WorkstreamId = self.id.parse()?;
        let view = match self.view {
            None => View::default(),
            Some(view) => view.read().map_err(|why| format!("workstream {id}'s view: {why}"))?,
        };
        Ok(Workstream {
            id,
            renamed: self.name.as_deref().map(Name::parse).transpose()?,
            below: Below::parse(&self.below)?,
            view,
            used: self.used_millis.map(Used::read).transpose().map_err(|why| {
                format!("workstream {id}: {why}")
            })?,
        })
    }

    fn of(workstream: &Workstream) -> KeptWorkstream {
        KeptWorkstream {
            id: workstream.id.to_string(),
            name: workstream.renamed.as_ref().map(|name| name.0.clone()),
            below: workstream.below.0.clone(),
            view: KeptView::of(&workstream.view),
            used_millis: workstream.used.map(Used::millis),
        }
    }
}

impl KeptView {
    fn read(self) -> Result<View, String> {
        let full = self.full.as_deref().map(str::parse).transpose()?;
        let tree = self.tree.into_iter().map(|node| match node {
            KeptNode::Split(KeptSplit { towards, share }) => {
                Ok(Preordered::Split { towards: towards.read(), share })
            },
            KeptNode::Terminal(terminal) => terminal.read().map(Preordered::Pane),
        });
        View::read(tree.collect::<Result<_, String>>()?, full)
    }

    fn of(view: &View) -> Option<KeptView> {
        let tree = view.tree()?.preorder().into_iter().map(|node| match node {
            Preordered::Split { towards, share } => {
                KeptNode::Split(KeptSplit { towards: KeptTowards::of(towards), share })
            },
            Preordered::Pane(terminal) => KeptNode::Terminal(KeptTerminal::of(terminal)),
        });
        Some(KeptView { full: view.full().map(|full| full.to_string()), tree: tree.collect() })
    }
}

impl KeptTowards {
    fn read(self) -> Towards {
        match self {
            KeptTowards::Right => Towards::Right,
            KeptTowards::Down => Towards::Down,
        }
    }

    fn of(towards: Towards) -> KeptTowards {
        match towards {
            Towards::Right => KeptTowards::Right,
            Towards::Down => KeptTowards::Down,
        }
    }
}

impl KeptTerminal {
    fn read(self) -> Result<Terminal, String> {
        let id: TerminalId = self.id.parse()?;
        let why = |why: String| format!("terminal {id}: {why}");
        let folder = Dir::parse(&self.folder).map_err(why)?;
        let name = self.name.as_deref().map(Name::parse).transpose().map_err(why)?;
        let runs = match self.runs {
            KeptRuns::Shell => Runs::Shell,
            KeptRuns::Command(line) => Runs::Command(Line::parse(&line).map_err(why)?),
            KeptRuns::Unrun(line) => Runs::Unrun(Line::parse(&line).map_err(why)?),
            KeptRuns::Agent(KeptAgent { harness, session }) => Runs::Agent {
                harness: harness.read(),
                session: Session::parse(&session).map_err(why)?,
            },
        };
        Ok(Terminal::new(id, folder, runs).named(name))
    }

    fn of(terminal: &Terminal) -> KeptTerminal {
        let runs = match terminal.runs() {
            Runs::Shell => KeptRuns::Shell,
            Runs::Command(line) => KeptRuns::Command(line.as_str().to_owned()),
            Runs::Unrun(line) => KeptRuns::Unrun(line.as_str().to_owned()),
            Runs::Agent { harness, session } => KeptRuns::Agent(KeptAgent {
                harness: KeptHarness::of(*harness),
                session: session.as_str().to_owned(),
            }),
        };
        KeptTerminal {
            id: terminal.id().to_string(),
            name: terminal.name().map(|name| name.as_str().to_owned()),
            folder: terminal.folder().as_str().to_owned(),
            runs,
        }
    }
}

impl KeptHarness {
    fn read(self) -> Harness {
        match self {
            KeptHarness::ClaudeCode => Harness::ClaudeCode,
            KeptHarness::Pi => Harness::Pi,
        }
    }

    fn of(harness: Harness) -> KeptHarness {
        match harness {
            Harness::ClaudeCode => KeptHarness::ClaudeCode,
            Harness::Pi => KeptHarness::Pi,
        }
    }
}

fn written(projects: &Projects) -> String {
    let projects = projects.projects.iter().map(KeptProject::of).collect();
    let kept = Kept { version: VERSION, projects };
    let mut text = serde_json::to_string_pretty(&kept).expect("strings and numbers serialize");
    text.push('\n');
    assert!(text.len() <= FILE_BYTES_MAX, "what is kept is bounded to what is read back");
    text
}

fn write(file: &Path, projects: &Projects) -> io::Result<()> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let above = folder.parent().expect("a folder of `state::path` is in a folder");
    fs::create_dir_all(folder)?;
    let mut unfinished = tempfile::Builder::new().prefix(".projects.").tempfile_in(folder)?;
    unfinished.write_all(written(projects).as_bytes())?;
    unfinished.as_file().sync_all()?;
    unfinished.persist(file).map_err(|error| error.error)?;
    let synced = File::open(folder).and_then(|it| it.sync_all());
    let synced = synced.and_then(|()| File::open(above)?.sync_all());
    if let Err(error) = synced {
        let folder = folder.display();
        log::say(&format!("{folder}: {error}, and so a crash may take back the change kept"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;
    use std::sync::mpsc::{self, Receiver};

    use super::super::{Placed, ProjectId};
    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn channel() -> (SyncSender<Command>, Receiver<Command>) {
        mpsc::sync_channel(16)
    }

    fn heard(commands: &Receiver<Command>) -> Vec<Result<Projects, String>> {
        let told = |command| {
            let Command::Projects { projects, .. } = command else {
                panic!("a store tells the core only what it keeps");
            };
            projects
        };
        commands.try_iter().map(told).collect()
    }

    fn file_in(scratch: &tempfile::TempDir) -> PathBuf {
        scratch.path().join("luneta").join(FILE)
    }

    #[test]
    fn what_a_store_keeps_comes_back_when_it_opens_again() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core.clone()).unwrap();
        assert_eq!(store.unkept(), None);
        let vault = Some(Name::parse("bitwarden backup").unwrap());
        let made = store.change(Change::AddProject { dir: dir("/home/me/Vault"), name: vault });
        let vault = ProjectId::new(made.unwrap().unwrap());
        store.change(Change::AddProject { dir: dir("/w/luneta"), name: None }).unwrap();
        let top = dir("/home/me/Vault/notes");
        store.change(Change::AddWorkstream { project: vault, folder: top, name: None }).unwrap();
        let (sub, named) = (dir("/home/me/Vault/a/b"), Some(Name::parse("release").unwrap()));
        store.change(Change::AddWorkstream { project: vault, folder: sub, name: named }).unwrap();

        let mut told = heard(&commands);
        assert_eq!(told.len(), 5);
        assert_eq!(told[0], Ok(Projects::default()));
        let kept = told.pop().unwrap();
        assert_eq!(kept.as_ref().map(|kept| kept.all().len()), Ok(2));

        drop(store);
        let again = Store::at(Ok(file.clone()), core).unwrap();
        assert_eq!(again.unkept(), None);
        assert_eq!(heard(&commands), [kept]);
        let mut left: Vec<String> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [FILE, LOCK]);
    }

    #[test]
    fn the_file_says_its_version_and_only_what_differs_from_the_defaults() {
        let mut projects = Projects::default();
        let ids = [1, 2, 3].map(|id| Id::of(id).unwrap());
        let mut drawn = ids.into_iter();
        let mut draw = || Ok(drawn.next().unwrap());
        let change = Change::AddProject { dir: dir("/w/luneta"), name: None };
        (projects, _) = projects.changed(change, &mut draw).unwrap();
        let project = ProjectId::new(ids[0]);
        let top = Change::AddWorkstream { project, folder: dir("/w/luneta/top"), name: None };
        (projects, _) = projects.changed(top, &mut draw).unwrap();
        let named = Some(Name::parse("release").unwrap());
        let folder = dir("/w/luneta/main");
        let main = Change::AddWorkstream { project, folder, name: named };
        (projects, _) = projects.changed(main, &mut draw).unwrap();
        let want = r#"{
  "version": 4,
  "projects": [
    {
      "id": "0000000000000001",
      "name": "luneta",
      "dir": "/w/luneta",
      "workstreams": [
        {
          "id": "0000000000000002",
          "below": "top"
        },
        {
          "id": "0000000000000003",
          "name": "release",
          "below": "main"
        }
      ]
    }
  ]
}
"#;
        assert_eq!(written(&projects), want);
        assert_eq!(parse(want), Ok(projects.clone()));
        for before in [1, 2, 3] {
            let before = want.replace(r#""version": 4"#, &format!(r#""version": {before}"#));
            assert_eq!(parse(&before), Ok(projects.clone()));
        }
    }

    #[test]
    fn a_view_is_kept_as_its_terminals_in_preorder_with_what_each_runs() {
        let mut projects = Projects::default();
        let ids = [1, 2].map(|id| Id::of(id).unwrap());
        let mut drawn = ids.into_iter();
        let mut draw = || Ok(drawn.next().unwrap());
        let change = Change::AddProject { dir: dir("/w/luneta"), name: None };
        (projects, _) = projects.changed(change, &mut draw).unwrap();
        let project = ProjectId::new(ids[0]);
        let top = Change::AddWorkstream { project, folder: dir("/w/luneta/top"), name: None };
        (projects, _) = projects.changed(top, &mut draw).unwrap();
        let workstream = WorkstreamId::new(ids[1]);
        let size = super::super::Size::of(80, 24).unwrap();
        let [shell, vim, agent] = [4, 5, 6].map(|id| TerminalId::of(id).unwrap());
        let runs = [
            (shell, Runs::Shell, Placed::Alone),
            (vim, Runs::Command(Line::parse("vim notes").unwrap()), Placed::Beside {
                at: shell,
                towards: Towards::Right,
            }),
            (
                agent,
                Runs::Agent {
                    harness: Harness::ClaudeCode,
                    session: Session::parse("7c22-ab").unwrap(),
                },
                Placed::Beside { at: vim, towards: Towards::Down },
            ),
        ];
        for (id, runs, placed) in runs {
            let terminal = Terminal::new(id, dir("/w/luneta/main"), runs);
            let open = Change::Open { workstream, terminal, placed, size };
            (projects, _) = projects.changed(open, &mut draw).unwrap();
        }
        let fill = Change::Fill { workstream, full: Some(vim) };
        (projects, _) = projects.changed(fill, &mut draw).unwrap();
        let want = r#"{
  "version": 4,
  "projects": [
    {
      "id": "0000000000000001",
      "name": "luneta",
      "dir": "/w/luneta",
      "workstreams": [
        {
          "id": "0000000000000002",
          "below": "top",
          "view": {
            "full": "0000000000000005",
            "tree": [
              {
                "split": {
                  "towards": "right",
                  "share": 500000
                }
              },
              {
                "terminal": {
                  "id": "0000000000000004",
                  "folder": "/w/luneta/main",
                  "runs": "shell"
                }
              },
              {
                "split": {
                  "towards": "down",
                  "share": 500000
                }
              },
              {
                "terminal": {
                  "id": "0000000000000005",
                  "folder": "/w/luneta/main",
                  "runs": {
                    "command": "vim notes"
                  }
                }
              },
              {
                "terminal": {
                  "id": "0000000000000006",
                  "folder": "/w/luneta/main",
                  "runs": {
                    "agent": {
                      "harness": "claude-code",
                      "session": "7c22-ab"
                    }
                  }
                }
              }
            ]
          }
        }
      ]
    }
  ]
}
"#;
        assert_eq!(written(&projects), want);
        assert_eq!(parse(want), Ok(projects));
        let before = want.replace(r#""version": 4"#, r#""version": 1"#);
        assert_eq!(parse(&before), Err("it is of version 1, which holds no views".to_owned()));
    }

    #[test]
    fn a_workstream_kept_in_its_project_s_own_folder_is_read_and_written_back() {
        let text = r#"{"version": 4, "projects": [{"id": "0000000000000001", "name": "a",
            "dir": "/a", "workstreams": [{"id": "0000000000000002"},
            {"id": "0000000000000003", "below": "b"}]}]}"#;
        let kept = parse(text).unwrap();
        let project = &kept.all()[0];
        let folders: Vec<Dir> = project.workstreams().iter().map(|it| project.folder(it)).collect();
        assert_eq!(folders, [dir("/a"), dir("/a/b")]);
        assert_eq!(parse(&written(&kept)), Ok(kept));
    }

    #[test]
    fn a_line_left_unrun_is_kept_apart_from_a_command_and_only_from_version_3() {
        let text = |version: u32, runs: &str| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "workstreams": [{{"id": "0000000000000002", "below": "b",
                "view": {{"tree": [
                {{"terminal": {{"id": "0000000000000003", "folder": "/a", "runs": {runs}}}}}]}}}}]}}]}}"#
            )
        };
        let unrun = parse(&text(3, r#"{"unrun": "make test"}"#)).unwrap();
        let (_, held) = unrun.holding(TerminalId::of(3).unwrap()).unwrap();
        assert_eq!(held.runs(), &Runs::Unrun(Line::parse("make test").unwrap()));
        assert!(written(&unrun).contains(r#""unrun": "make test""#), "{}", written(&unrun));
        assert_eq!(parse(&written(&unrun)), Ok(unrun.clone()));
        let command = parse(&text(2, r#"{"command": "make test"}"#)).unwrap();
        assert_ne!(command, unrun, "a command run is not a line left unrun");
        let refused = parse(&text(2, r#"{"unrun": "make test"}"#));
        assert_eq!(refused, Err("it is of version 2, which holds no line left unrun".to_owned()));
    }

    #[test]
    fn a_terminal_s_name_and_a_workstream_s_use_are_kept_only_from_version_4() {
        let text = |version: u32, name: &str, used: &str| {
            format!(
                r#"{{"version": {version}, "projects": [{{"id": "0000000000000001", "name": "a",
                "dir": "/a", "workstreams": [{{"id": "0000000000000002", "below": "b",
                "view": {{"tree": [
                {{"terminal": {{"id": "0000000000000003", {name} "folder": "/a", "runs": "shell"}}}}
                ]}} {used}}}]}}]}}"#
            )
        };
        let (name, used) = (r#""name": "tests","#, r#", "used_millis": 1763460000123"#);
        let kept = parse(&text(4, name, used)).unwrap();
        let (workstream, held) = kept.holding(TerminalId::of(3).unwrap()).unwrap();
        assert_eq!(held.name().map(Name::as_str), Some("tests"));
        assert_eq!(workstream.used(), Some(Used::of_millis(1_763_460_000_123)));
        let again = written(&kept);
        assert!(again.contains(r#""name": "tests""#));
        assert!(again.contains(r#""used_millis": 1763460000123"#));
        assert_eq!(parse(&again), Ok(kept));
        let refusals = [(name, "", "terminal's name"), ("", used, "workstream's use")];
        for (name, used, without) in refusals {
            let refused = parse(&text(3, name, used));
            assert_eq!(refused, Err(format!("it is of version 3, which holds no {without}")));
        }
        assert!(parse(&text(4, r#""name": "","#, "")).unwrap_err().contains("a name is empty"));
        let past = format!(r#", "used_millis": {}"#, u64::MAX);
        let refused = parse(&text(4, "", &past)).unwrap_err();
        assert!(refused.contains("workstream 0000000000000002: a use at"), "{refused}");
    }

    #[test]
    fn a_file_it_cannot_read_is_never_written_over() {
        let project = r#"{"id": "0000000000000001", "name": "a", "dir": "/a"}"#;
        let refused = [
            ("{", "EOF"),
            (r#"{"version": 5, "projects": []}"#, "version 5, and this lunetad reads only 1 to 4"),
            (r#"{"version": 2, "projects": [], "views": []}"#, "unknown field `views`"),
            (r#"{"version": 1}"#, "missing field `projects`"),
            (
                r#"{"version": 1, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a"},
                {"id": "0000000000000002", "name": "b", "dir": "/a/b"}]}"#,
                "/a/b is inside /a",
            ),
            (&format!(r#"{{"version": 1, "projects": [{project}, {project}]}}"#), "comes twice"),
            (
                r#"{"version": 1, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "../b"}]}]}"#,
                "`..`",
            ),
            (
                r#"{"version": 2, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "b",
                "view": {"tree": [{"terminal": {
                "id": "0000000000000001", "folder": "/a", "runs": "shell"}}]}}]}]}"#,
                "comes twice",
            ),
            (
                r#"{"version": 2, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "b",
                "view": {"full": "0000000000000003",
                "tree": [{"terminal": {"id": "0000000000000003", "folder": "/a",
                "runs": "shell"}}]}}]}]}"#,
                "a view of one terminal shows it whole already",
            ),
            (
                r#"{"version": 2, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "b",
                "view": {"tree": []}}]}]}"#,
                "workstream 0000000000000002's view: a view holds no terminal",
            ),
            (
                r#"{"version": 2, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "b",
                "view": {"tree": [{"terminal": {
                "id": "0000000000000003", "folder": "/a", "runs": {"command": "a\u0007"}}}]}}]}]}"#,
                "terminal 0000000000000003: a command line holds a control character",
            ),
        ];
        for (text, why) in refused {
            let scratch = tempfile::tempdir().unwrap();
            let file = file_in(&scratch);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(&file, text).unwrap();
            let (core, commands) = channel();
            let mut store = Store::at(Ok(file.clone()), core).unwrap();
            let unkept = store.unkept().unwrap().to_owned();
            assert!(unkept.contains(why), "{unkept:?} for {text}");
            assert!(unkept.ends_with("and so it is left as it is"), "{unkept}");
            assert_eq!(heard(&commands), [Err(unkept.clone())]);
            let change = Change::AddProject { dir: dir("/c"), name: None };
            assert_eq!(store.change(change), Err(Failed::NotKept(unkept)));
            assert_eq!(heard(&commands), []);
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn a_file_too_big_is_not_read_whole() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, " ".repeat(FILE_BYTES_MAX + 1)).unwrap();
        let store = Store::at(Ok(file), channel().0).unwrap();
        assert!(store.unkept().unwrap().contains("over 33554432 bytes"));
    }

    #[test]
    fn with_no_state_folder_nothing_is_kept_and_it_says_why() {
        let (core, _commands) = channel();
        let mut store = Store::at(Err(Unset), core).unwrap();
        let unkept = store.unkept().unwrap().to_owned();
        assert!(unkept.starts_with("projects are kept nowhere: neither XDG_STATE_HOME"));
        let change = Change::AddProject { dir: dir("/c"), name: None };
        assert_eq!(store.change(change), Err(Failed::NotKept(unkept)));
    }

    #[test]
    fn a_change_kept_with_no_core_to_hear_it_says_so() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core).unwrap();
        drop(commands);
        let change = Change::AddProject { dir: dir("/c"), name: None };
        assert_eq!(store.change(change), Err(Failed::KeptUntold));

        drop(store);
        let (core, commands) = channel();
        Store::at(Ok(file), core).unwrap();
        let [kept] = heard(&commands).try_into().unwrap();
        let kept = kept.unwrap();
        assert_eq!(kept.all()[0].dir().as_str(), "/c");
    }

    #[test]
    fn a_refused_or_unwritten_change_says_so_and_the_store_keeps_on() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core).unwrap();
        store.change(Change::AddProject { dir: dir("/a"), name: None }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        assert_eq!(heard(&commands).len(), 2);

        let inside = Change::AddProject { dir: dir("/a/b"), name: None };
        let Err(Failed::Nests(why)) = store.change(inside) else {
            panic!("a project inside another is kept");
        };
        assert!(why.contains("/a/b is inside /a"), "{why}");
        let (project, to) = (ProjectId::new(Id::of(9).unwrap()), Name::parse("x").unwrap());
        let Err(Failed::Refused(why)) = store.change(Change::RenameProject { project, to }) else {
            panic!("a rename of no project is kept");
        };
        assert!(why.contains("no project"), "{why}");

        let folder = file.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o500)).unwrap();
        let unwritten = store.change(Change::AddProject { dir: dir("/c"), name: None });
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        let Err(Failed::Unwritten(why)) = unwritten else {
            panic!("a change into a folder that refuses writes is kept: {unwritten:?}");
        };
        assert!(why.starts_with(&format!("{}: ", file.display())), "{why}");

        assert_eq!(store.unkept(), None);
        assert_eq!(heard(&commands), []);
        assert_eq!(fs::read_to_string(&file).unwrap(), written);
        store.change(Change::AddProject { dir: dir("/c"), name: None }).unwrap();
        let [kept] = heard(&commands).try_into().unwrap();
        assert_eq!(kept.map(|kept| kept.all().len()), Ok(2));
    }

    #[test]
    fn a_change_that_follows_the_mux_is_kept_unwritten_until_a_flush_writes_it() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core).unwrap();
        let made = store.change(Change::AddProject { dir: dir("/a"), name: None }).unwrap();
        let project = ProjectId::new(made.unwrap());
        let add = Change::AddWorkstream { project, folder: dir("/a/b"), name: None };
        let workstream = WorkstreamId::new(store.change(add).unwrap().unwrap());
        let terminal = Terminal::new(TerminalId::of(7).unwrap(), dir("/a"), Runs::Shell);
        let (placed, size) = (Placed::Alone, super::super::Size::of(80, 24).unwrap());
        store.change(Change::Open { workstream, terminal, placed, size }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        heard(&commands);

        let folder = file.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o500)).unwrap();
        let unwritten = store.followed(Change::CloseWorkstream { workstream });
        let flushed = store.flush();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        let why = unwritten.unwrap().expect("a folder that refuses writes writes nothing");
        assert!(why.starts_with(&format!("{}: ", file.display())), "{why}");
        assert!(matches!(flushed, Err(Failed::Unwritten(_))), "{flushed:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), written);
        let [told] = heard(&commands).try_into().unwrap();
        assert!(told.unwrap().find(workstream).is_none(), "the core hears of the close");
        assert!(store.projects().unwrap().find(workstream).is_none());

        store.flush().unwrap();
        assert!(read(&file).unwrap().find(workstream).is_none(), "a flush writes the close");
        let again = store.followed(Change::CloseWorkstream { workstream });
        assert!(matches!(again, Err(Failed::Refused(_))), "{again:?}");
    }

    #[test]
    fn a_second_store_of_one_file_waits_for_the_first_then_keeps_nothing() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, _commands) = channel();
        let first = Store::at(Ok(file.clone()), core.clone()).unwrap();
        let mut second = Store::at(Ok(file.clone()), core.clone()).unwrap();
        let unkept = second.unkept().unwrap().to_owned();
        assert!(unkept.starts_with("another lunetad keeps the projects in "), "{unkept}");
        let change = Change::AddProject { dir: dir("/c"), name: None };
        assert_eq!(second.change(change), Err(Failed::NotKept(unkept)));

        let released = thread::spawn(move || {
            thread::sleep(HELD_PAUSE * 4);
            drop(first);
        });
        let third = Store::at(Ok(file), core).unwrap();
        released.join().unwrap();
        assert_eq!(third.unkept(), None);
    }

    #[test]
    fn a_file_changed_behind_its_back_is_never_written_over() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core).unwrap();
        store.change(Change::AddProject { dir: dir("/a"), name: None }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        let edited = written.replace(r#""name": "a""#, r#""name": "edited""#);
        assert_ne!(edited, written);
        fs::write(&file, &edited).unwrap();

        let change = Change::AddProject { dir: dir("/b"), name: None };
        let Err(Failed::NotKept(why)) = store.change(change) else {
            panic!("a change over an edited file is kept");
        };
        assert!(why.ends_with("it changed since lunetad read it, and so it is left as it is"));
        assert_eq!(store.unkept(), Some(why.as_str()));
        assert_eq!(heard(&commands).last(), Some(&Err(why)));
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);
    }

    #[test]
    fn a_change_to_a_view_alone_is_written_when_flushed_or_with_the_next_change() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = Store::at(Ok(file.clone()), core).unwrap();
        let project = store.change(Change::AddProject { dir: dir("/a"), name: None });
        let project = ProjectId::new(project.unwrap().unwrap());
        let add = Change::AddWorkstream { project, folder: dir("/a/b"), name: None };
        let workstream = WorkstreamId::new(store.change(add).unwrap().unwrap());
        let size = super::super::Size::of(80, 24).unwrap();
        let [one, two] = [store.fresh().unwrap(), TerminalId::of(u64::MAX).unwrap()];
        let opened = |id, placed| {
            let terminal = Terminal::new(id, dir("/a"), Runs::Shell);
            Change::Open { workstream, terminal, placed, size }
        };
        store.change(opened(one, Placed::Alone)).unwrap();
        let beside = Placed::Beside { at: one, towards: Towards::Right };
        store.change(opened(two, beside)).unwrap();
        let read = || parse(&fs::read_to_string(&file).unwrap()).unwrap();
        let full = |projects: &Projects| projects.find(workstream).unwrap().1.view().full();

        store.change(Change::Fill { workstream, full: Some(two) }).unwrap();
        assert_eq!(full(store.projects().unwrap()), Some(two));
        assert_eq!(full(&read()), None, "a fill waits for the flush");
        let told = heard(&commands).pop().unwrap().unwrap();
        assert_eq!(full(&told), Some(two), "the core hears a fill at once");
        store.flush().unwrap();
        assert_eq!(full(&read()), Some(two));

        store.change(Change::Fill { workstream, full: None }).unwrap();
        store.change(Change::RenameProject { project, to: Name::parse("b").unwrap() }).unwrap();
        let kept = *store.projects().unwrap() == read();
        assert!(kept, "a change kept at once takes the rest along");
        store.flush().unwrap();
        assert_eq!(read(), *store.projects().unwrap());
    }
}
