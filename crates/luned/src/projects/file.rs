use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Read as _, Seek as _, SeekFrom, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::thread;
use std::time::Duration;

use lune_domain::projects::{parse, written, Change, Closing, Dir, Made, MoveTo, Parsed, Projects};
use lune_domain::projects::{Refused, Relaid, Size, Spot, TerminalViewId, ViewId, WorktreeId};
use lune_domain::projects::{FILE_BYTES_MAX, PATH_BYTES_MAX};

use super::{gone, random_id, spot, SPOT_LOOK};
use crate::api;
use crate::core::Command;
use crate::state::{self, Unset};
use crate::{log, repositories};

pub const FILE: &str = "projects";

const LOCK: &str = "projects.lock";

const HELD_TRIES: u32 = 40;

const HELD_PAUSE: Duration = Duration::from_millis(50);

const SAID_TRIES: u32 = 20;

const SAID_PAUSE: Duration = Duration::from_millis(10);

pub struct Store {
    keeping: Keeping,
    revision: Revision,
    core: SyncSender<Command>,
    _held: Option<File>,
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
    pub made: Option<Made>,
    pub opened: Option<Opened>,
    pub kept: Revision,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    View(ViewId),
    Unopened(String),
}

impl Opened {
    pub fn of_api(
        said: Option<api::change_projects_response::Opened>,
    ) -> Result<Option<Opened>, String> {
        use api::change_projects_response::Opened as Said;
        match said {
            None => Ok(None),
            Some(Said::View(view)) => {
                let view = ViewId::of(view).ok_or("opened: it names view 0")?;
                Ok(Some(Opened::View(view)))
            },
            Some(Said::Unopened(why)) => Ok(Some(Opened::Unopened(why))),
        }
    }

    #[must_use]
    pub fn to_api(self) -> api::change_projects_response::Opened {
        use api::change_projects_response::Opened as Said;
        match self {
            Opened::View(view) => Said::View(view.get()),
            Opened::Unopened(why) => Said::Unopened(why),
        }
    }
}

enum Keeping {
    Kept { file: PathBuf, projects: Projects, written: Projects },
    Unkept(String),
}

pub struct Lock {
    file: PathBuf,
    lock: File,
}

#[derive(Debug)]
pub enum Unheld {
    Elsewhere(Elsewhere),
    Unkept(String),
}

#[derive(Debug, PartialEq, Eq)]
pub struct Elsewhere {
    folder: PathBuf,
    holder: Option<PathBuf>,
}

impl fmt::Display for Elsewhere {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let folder = self.folder.display();
        match &self.holder {
            Some(holder) => write!(
                f,
                "another luned keeps the projects in {folder}, and listens in {}: `lune \
                 terminate` ends it",
                holder.display()
            ),
            None => write!(
                f,
                "another luned keeps the projects in {folder}, one too old to say where it \
                 listens"
            ),
        }
    }
}

pub fn take(listens_in: &Path) -> Result<Lock, Unheld> {
    take_at(state::path(FILE), listens_in)
}

fn take_at(file: Result<PathBuf, Unset>, listens_in: &Path) -> Result<Lock, Unheld> {
    let nowhere = |unset| Unheld::Unkept(format!("projects are kept nowhere: {unset}"));
    let file = file.map_err(nowhere)?;
    let lock = locked(&file, listens_in)?;
    Ok(Lock { file, lock })
}

#[must_use]
pub fn holder() -> Option<PathBuf> {
    let file = state::path(FILE).ok()?;
    let lock = File::open(file.parent()?.join(LOCK)).ok()?;
    match lock.try_lock() {
        Err(TryLockError::WouldBlock) => said_by(&lock),
        Ok(()) | Err(TryLockError::Error(_)) => None,
    }
}

fn said_by(lock: &File) -> Option<PathBuf> {
    for _ in 0..SAID_TRIES {
        let said = said_now(lock)?;
        if !said.is_empty() {
            let holder = PathBuf::from(OsStr::from_bytes(&said));
            return (said.len() <= PATH_BYTES_MAX && holder.is_absolute()).then_some(holder);
        }
        thread::sleep(SAID_PAUSE);
    }
    None
}

fn said_now(mut lock: &File) -> Option<Vec<u8>> {
    let mut said = Vec::new();
    let limit = u64::try_from(PATH_BYTES_MAX).expect("a path's bytes fit a u64");
    lock.seek(SeekFrom::Start(0)).ok()?;
    lock.take(limit + 1).read_to_end(&mut said).ok()?;
    Some(said)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    NotKept(String),
    Nests(String),
    Stale(String),
    Refused(String),
    Unwritten(String),
    Unclosed(String),
    Unopened(String),
    Gone(String),
    KeptUntold,
}

impl Failed {
    #[must_use]
    pub fn said(self) -> String {
        match self {
            Failed::NotKept(why)
            | Failed::Nests(why)
            | Failed::Stale(why)
            | Failed::Refused(why)
            | Failed::Unwritten(why)
            | Failed::Unclosed(why)
            | Failed::Unopened(why)
            | Failed::Gone(why) => why,
            Failed::KeptUntold => "it is kept, but the daemon's core has stopped".to_owned(),
        }
    }
}

impl Store {
    pub fn open(kept: Result<Lock, String>, core: SyncSender<Command>) -> io::Result<Store> {
        let (keeping, held) = match kept {
            Ok(Lock { file, lock }) => (Keeping::of(file), Some(lock)),
            Err(why) => (Keeping::Unkept(why), None),
        };
        let store = Store { keeping, revision: Revision::default(), core, _held: held };
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

    pub fn fresh(&self) -> Result<ViewId, Failed> {
        let fresh = self.kept()?.fresh(&mut random_id);
        fresh.map(ViewId::new).map_err(Failed::Refused)
    }

    pub fn fresh_terminal(&self) -> Result<TerminalViewId, Failed> {
        self.kept()?.fresh_terminal(&mut random_id).map_err(Failed::Refused)
    }

    pub fn change(&mut self, change: Change) -> Result<Option<Made>, Failed> {
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

    pub fn moved(
        &mut self,
        terminal: TerminalViewId,
        to: MoveTo,
        size: Size,
        focused: bool,
    ) -> Result<WorktreeId, Failed> {
        let (changed, to) =
            self.kept()?.moved(terminal, to, size, focused, random_id).map_err(Failed::Refused)?;
        assert_ne!(self.projects(), Some(&changed), "a terminal moved changes the projects");
        self.written_as(&changed)?;
        self.took(changed)?;
        Ok(to)
    }

    pub fn relaid(&mut self, relaid: Relaid) -> Result<(), Failed> {
        let changed = self.kept()?.relaid(relaid).map_err(Failed::Refused)?;
        let Keeping::Kept { projects, .. } = &mut self.keeping else {
            unreachable!("projects relaid are kept");
        };
        if *projects == changed {
            return Ok(());
        }
        *projects = changed;
        self.tell()
    }

    pub fn followed(&mut self, change: Change) -> Result<Option<String>, Failed> {
        let (changed, made) = self.applied(change)?;
        assert!(made.is_none(), "a change that follows the mux or git hands back nothing made");
        self.follows(changed)
    }

    pub fn closed(&mut self, closing: Closing) -> Result<Option<String>, Failed> {
        let changed = self.kept()?.closed(closing).map_err(Failed::Refused)?;
        self.follows(changed)
    }

    fn follows(&mut self, changed: Projects) -> Result<Option<String>, Failed> {
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

    fn kept(&self) -> Result<&Projects, Failed> {
        match &self.keeping {
            Keeping::Kept { projects, .. } => Ok(projects),
            Keeping::Unkept(why) => Err(Failed::NotKept(why.clone())),
        }
    }

    fn applied(&self, change: Change) -> Result<(Projects, Option<Made>), Failed> {
        let owned = repositories::owned_folder();
        let changed = self.kept()?.changed(change, owned.as_ref(), random_id);
        changed.map_err(|refused| match refused {
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
        match read(&file) {
            Ok(Parsed { projects, migrated: false }) => {
                let written = projects.clone();
                Keeping::Kept { file, projects, written }
            },
            Ok(Parsed { projects, migrated: true }) => match write(&file, &projects) {
                Ok(()) => {
                    let written = projects.clone();
                    Keeping::Kept { file, projects, written }
                },
                Err(error) => {
                    Keeping::Unkept(left(&file, &format!("migrated, it is unwritten: {error}")))
                },
            },
            Err(why) => Keeping::Unkept(left(&file, &why)),
        }
    }
}

fn locked(file: &Path, listens_in: &Path) -> Result<File, Unheld> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let path = folder.join(LOCK);
    let failed = |at: &Path, error: io::Error| {
        Unheld::Unkept(format!("{}: {error}, and so no projects are kept", at.display()))
    };
    fs::create_dir_all(folder).map_err(|error| failed(folder, error))?;
    let mut options = OpenOptions::new();
    let opened = options.create(true).truncate(false).read(true).write(true).open(&path);
    let mut lock = opened.map_err(|error| failed(&path, error))?;
    if held(&lock).map_err(|error| failed(&path, error))? {
        let said = lock.set_len(0).and_then(|()| lock.write_all(listens_in.as_os_str().as_bytes()));
        said.map_err(|error| failed(&path, error))?;
        return Ok(lock);
    }
    Err(Unheld::Elsewhere(Elsewhere { folder: folder.to_owned(), holder: said_by(&lock) }))
}

fn held(lock: &File) -> io::Result<bool> {
    for _ in 0..HELD_TRIES {
        match lock.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) => thread::sleep(HELD_PAUSE),
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
    Ok(false)
}

pub fn kept() -> Result<Projects, String> {
    let file = state::path(FILE).map_err(|unset| unset.to_string())?;
    let read = read(&file).map_err(|why| format!("{}: {why}", file.display()))?;
    Ok(read.projects)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Forgotten {
    Read(Projects),
    Unread(String),
}

pub fn forget() -> Result<Forgotten, String> {
    forget_at(&state::path(FILE).map_err(|unset| unset.to_string())?)
}

fn forget_at(file: &Path) -> Result<Forgotten, String> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let path = folder.join(LOCK);
    let failed = |error: io::Error| format!("{}: {error}", path.display());
    let lock = match OpenOptions::new().create(true).truncate(false).write(true).open(&path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Forgotten::Read(Projects::default()));
        },
        Err(error) => return Err(failed(error)),
    };
    if !held(&lock).map_err(failed)? {
        return Err(format!("a luned keeps them still, as it holds {}", path.display()));
    }
    let held = match read(file) {
        Ok(Parsed { projects, .. }) => Forgotten::Read(projects),
        Err(why) => Forgotten::Unread(why),
    };
    match fs::remove_file(file) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::NotFound => {},
        Err(error) => return Err(format!("{}: {error}", file.display())),
    }
    drop(lock);
    Ok(held)
}

fn unchanged(file: &Path, projects: &Projects) -> Result<(), String> {
    match read(file) {
        Ok(Parsed { projects: now, migrated: false }) if now == *projects => Ok(()),
        Ok(_) => Err(left(file, "it changed since luned read it")),
        Err(why) => Err(left(file, &why)),
    }
}

fn left(file: &Path, why: &str) -> String {
    format!("{}: {why}, and so it is left as it is", file.display())
}

fn read(file: &Path) -> Result<Parsed, String> {
    let mut text = String::new();
    let opened = match File::open(file) {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Parsed { projects: Projects::default(), migrated: false });
        },
        Err(error) => return Err(error.to_string()),
    };
    let limit = u64::try_from(FILE_BYTES_MAX).expect("FILE_BYTES_MAX fits a u64") + 1;
    opened.take(limit).read_to_string(&mut text).map_err(|error| error.to_string())?;
    if text.len() > FILE_BYTES_MAX {
        return Err(format!("it is over {FILE_BYTES_MAX} bytes"));
    }
    parse(&text, &mut spot_of_kept, &mut random_id)
}

fn spot_of_kept(dir: &Dir) -> Result<Option<Spot>, String> {
    match gone(dir) {
        Ok(true) => Ok(None),
        Ok(false) => spot::of(dir, SPOT_LOOK).map(Some),
        Err(error) => Err(format!("{}: {error}", dir.as_str())),
    }
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

    use lune_domain::projects::layout::Towards;
    use lune_domain::projects::{Arranged, Id, Listed, Name, Opens, Placed, ProjectId, ProjectName};
    use lune_domain::projects::Runs;
    use lune_domain::projects::{Size, Subfolder, Terminal, Used, View, USED_MILLIS_MAX};
    use lune_waits::{Gap, apart};

    use super::*;

    fn add_in(project: ProjectId, spot: Spot) -> Change {
        Change::AddWorktree { project, spot, name: None, opens: Opens::Shell }
    }

    fn add_named(project: ProjectId, spot: Spot, name: Option<Name>) -> Change {
        Change::AddWorktree { project, spot, name, opens: Opens::Shell }
    }

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn alone(at: &str) -> Spot {
        Spot::in_folder(dir(at), None)
    }

    fn tree(at: &str) -> Subfolder {
        Subfolder::parse(at).unwrap()
    }

    fn listing(at: &[&str]) -> Listed {
        Listed::of(at.iter().copied().map(tree).collect()).unwrap()
    }

    fn clone_of(root: &str, subfolders: &[&str]) -> Spot {
        Spot::in_clone(dir(root), listing(subfolders), None)
    }

    fn spot_at(root: &str, at: &str) -> Spot {
        Spot::in_clone(dir(root), listing(&[at]), Some(tree(at)))
    }

    fn laid_row(id: u64, at: &str, terminal: u64) -> String {
        let terminal = format!(r#"{{"id": "{terminal:016x}", "folder": "/a", "runs": "shell"}}"#);
        let layout = format!(r#""layout": {{"tree": [{{"terminal": {terminal}}}]}}"#);
        format!(r#"{{"id": "{id:016x}", "worktree": "{at}", {layout}}}"#)
    }

    fn counting(from: u64) -> impl FnMut() -> io::Result<Id> {
        let mut next = from;
        move || {
            next += 1;
            Ok(Id::of(next).unwrap())
        }
    }

    fn rooted_as(
        text: &str,
        mut root: impl FnMut(&Dir) -> Result<Option<Spot>, String>,
    ) -> Result<Parsed, String> {
        lune_domain::projects::parse(text, &mut root, &mut counting(100))
    }

    fn parse(text: &str) -> Result<Projects, String> {
        let alone = |dir: &Dir| Ok(Some(Spot::in_folder(dir.clone(), None)));
        rooted_as(text, alone).map(|read| read.projects)
    }

    fn channel() -> (SyncSender<Command>, Receiver<Command>) {
        mpsc::sync_channel(16)
    }

    const LISTENS_IN: &str = "/run/user/1000/lune";

    fn store_at(file: Result<PathBuf, Unset>, core: SyncSender<Command>) -> Store {
        let kept = take_at(file, Path::new(LISTENS_IN)).map_err(|unheld| match unheld {
            Unheld::Unkept(why) => why,
            Unheld::Elsewhere(elsewhere) => panic!("{elsewhere}"),
        });
        Store::open(kept, core).unwrap()
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
        scratch.path().join("lune").join(FILE)
    }
    #[test]
    fn what_a_store_keeps_comes_back_when_it_opens_again() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core.clone());
        assert_eq!(store.unkept(), None);
        let vault = Some(ProjectName::parse("bitwarden backup").unwrap());
        store.change(Change::AddProject { spot: alone("/home/me/Vault"), name: vault }).unwrap();
        let lune = Change::AddProject { spot: clone_of("/w/lune", &["main"]), name: None };
        let made = store.change(lune);
        let Some(Made::Project(lune)) = made.unwrap() else { panic!("lune is made") };
        let named = Some(Name::parse("release").unwrap());
        let fix = add_named(lune, spot_at("/w/lune", "fix"), named);
        store.change(fix).unwrap();

        let mut told = heard(&commands);
        assert_eq!(told.len(), 4);
        assert_eq!(told[0], Ok(Projects::default()));
        let kept = told.pop().unwrap();
        assert_eq!(kept.as_ref().map(|kept| kept.worktrees().count()), Ok(2));

        drop(store);
        let again = store_at(Ok(file.clone()), core);
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
    fn projects_are_forgotten_only_once_no_store_keeps_them() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        assert_eq!(forget_at(&file), Ok(Forgotten::Read(Projects::default())));
        assert!(!file.parent().unwrap().exists(), "nothing is made to forget nothing");

        let (core, _commands) = channel();
        let mut store = store_at(Ok(file.clone()), core.clone());
        let lune = Change::AddProject { spot: clone_of("/w/lune", &[]), name: None };
        let made = store.change(lune);
        let Some(Made::Project(lune)) = made.unwrap() else { panic!("lune is made") };
        let spot = spot_at("/w/lune", "main");
        store.change(add_in(lune, spot)).unwrap();
        let held = store.projects().unwrap().clone();
        let refused = forget_at(&file).unwrap_err();
        assert!(refused.starts_with("a luned keeps them still"), "{refused}");
        assert!(file.exists());

        drop(store);
        assert_eq!(forget_at(&file), Ok(Forgotten::Read(held)));
        assert!(!file.exists());
        let again = store_at(Ok(file.clone()), core);
        assert_eq!(again.projects(), Some(&Projects::default()));
        drop(again);

        fs::write(&file, "{").unwrap();
        assert!(matches!(forget_at(&file), Ok(Forgotten::Unread(_))));
        assert!(!file.exists(), "a file that does not read is forgotten too");
    }

    #[test]
    fn a_file_it_cannot_read_is_never_written_over() {
        let project = r#"{"id": "0000000000000001", "name": "a", "dir": "/a"}"#;
        let refused = [
            ("{", "EOF"),
            (
                r#"{"version": 12, "projects": []}"#,
                "version 12, and this luned reads only 1 to 11",
            ),
            (r#"{"version": 2, "projects": [], "views": []}"#, "unknown field `views`"),
            (r#"{"version": 1}"#, "missing field `projects`"),
            (
                r#"{"version": 1, "projects": [
                {"id": "0000000000000001", "name": "a/b", "dir": "/a"}]}"#,
                "\"a/b\" names no project: name cannot contain '/'",
            ),
            (
                r#"{"version": 10, "projects": [{"id": "0000000000000001", "name": "..",
                "dir": "/a", "holds": {"clone": []}}]}"#,
                "\"..\" names no project: name cannot be '.' or '..'",
            ),
            (
                r#"{"version": 1, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a"},
                {"id": "0000000000000002", "name": "b", "dir": "/a/b"}]}"#,
                "/a/b is inside /a",
            ),
            (&format!(r#"{{"version": 1, "projects": [{project}, {project}]}}"#), "comes twice"),
            (
                &format!(
                    r#"{{"version": 9, "projects": [{{"id": "0000000000000001", "name": "a",
                    "dir": "/a", "holds": {{"clone": [{}, {}]}}}}]}}"#,
                    laid_row(2, "b", 4),
                    laid_row(3, "b", 5),
                ),
                "/a/b holds two of project a's worktrees",
            ),
            (
                &format!(
                    r#"{{"version": 9, "projects": [{{"id": "0000000000000001", "name": "a",
                    "dir": "/a", "holds": {{"clone": [{}]}}}}]}}"#,
                    laid_row(2, "", 4),
                ),
                "worktree 0000000000000002: a worktree is below its clone's root",
            ),
            (
                r#"{"version": 9, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "holds": {"clone": [{"id": "0000000000000002", "worktree": "b"}]}}]}"#,
                "worktree 0000000000000002 in /a/b holds no view, as no clone's may",
            ),
            (
                r#"{"version": 6, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "holds": {"folder": {"id": "0000000000000002", "worktree": "b"}}}]}"#,
                "unknown field `worktree`",
            ),
            (
                r#"{"version": 6, "projects": [{"id": "0000000000000001", "name": "a",
                "dir": "/a"}]}"#,
                "missing field `holds`",
            ),
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
                "a layout of one view shows it whole already",
            ),
            (
                r#"{"version": 2, "projects": [{"id": "0000000000000001", "name": "a", "dir": "/a",
                "workstreams": [{"id": "0000000000000002", "below": "b",
                "view": {"tree": []}}]}]}"#,
                "worktree 0000000000000002's layout: a layout holds no view",
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
            let mut store = store_at(Ok(file.clone()), core);
            let unkept = store.unkept().unwrap().to_owned();
            assert!(unkept.contains(why), "{unkept:?} for {text}");
            assert!(unkept.ends_with("and so it is left as it is"), "{unkept}");
            assert_eq!(heard(&commands), [Err(unkept.clone())]);
            let change = Change::AddProject { spot: alone("/c"), name: None };
            assert_eq!(store.change(change), Err(Failed::NotKept(unkept)));
            assert_eq!(heard(&commands), []);
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn a_file_too_big_is_not_read_whole_and_its_lock_stays_held() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, " ".repeat(FILE_BYTES_MAX + 1)).unwrap();
        let store = store_at(Ok(file.clone()), channel().0);
        assert!(store.unkept().unwrap().contains("over 33554432 bytes"));
        let second = take_at(Ok(file), Path::new("/tmp/b"));
        assert!(matches!(second, Err(Unheld::Elsewhere(_))), "{:?}", second.err());
    }

    #[test]
    fn with_no_state_folder_nothing_is_kept_and_it_says_why() {
        let (core, _commands) = channel();
        let mut store = store_at(Err(Unset), core);
        let unkept = store.unkept().unwrap().to_owned();
        assert!(unkept.starts_with("projects are kept nowhere: neither XDG_STATE_HOME"));
        let change = Change::AddProject { spot: alone("/c"), name: None };
        assert_eq!(store.change(change), Err(Failed::NotKept(unkept)));
    }

    #[test]
    fn a_change_kept_with_no_core_to_hear_it_says_so() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core);
        drop(commands);
        let change = Change::AddProject { spot: alone("/c"), name: None };
        assert_eq!(store.change(change), Err(Failed::KeptUntold));

        drop(store);
        let (core, commands) = channel();
        store_at(Ok(file), core);
        let [kept] = heard(&commands).try_into().unwrap();
        let kept = kept.unwrap();
        assert_eq!(kept.all()[0].dir().as_str(), "/c");
    }

    #[test]
    fn a_refused_or_unwritten_change_says_so_and_the_store_keeps_on() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core);
        store.change(Change::AddProject { spot: alone("/a"), name: None }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        assert_eq!(heard(&commands).len(), 2);

        let inside = Change::AddProject { spot: alone("/a/b"), name: None };
        let Err(Failed::Nests(why)) = store.change(inside) else {
            panic!("a project inside another is kept");
        };
        assert!(why.contains("/a/b is inside /a"), "{why}");
        let (project, to) = (ProjectId::new(Id::of(9).unwrap()), ProjectName::parse("x").unwrap());
        let Err(Failed::Refused(why)) = store.change(Change::RenameProject { project, to }) else {
            panic!("a rename of no project is kept");
        };
        assert!(why.contains("no project"), "{why}");

        let folder = file.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o500)).unwrap();
        let unwritten = store.change(Change::AddProject { spot: alone("/c"), name: None });
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        let Err(Failed::Unwritten(why)) = unwritten else {
            panic!("a change into a folder that refuses writes is kept: {unwritten:?}");
        };
        assert!(why.starts_with(&format!("{}: ", file.display())), "{why}");

        assert_eq!(store.unkept(), None);
        assert_eq!(heard(&commands), []);
        assert_eq!(fs::read_to_string(&file).unwrap(), written);
        store.change(Change::AddProject { spot: alone("/c"), name: None }).unwrap();
        let [kept] = heard(&commands).try_into().unwrap();
        assert_eq!(kept.map(|kept| kept.all().len()), Ok(2));
    }

    #[test]
    fn a_use_stepped_up_to_the_latest_kept_is_read_back_from_the_file() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, _commands) = channel();
        let mut store = store_at(Ok(file.clone()), core.clone());
        let lune = Change::AddProject { spot: clone_of("/w/lune", &[]), name: None };
        let Some(Made::Project(lune)) = store.change(lune).unwrap() else {
            panic!("lune is made");
        };
        let mut worktree = |at| match store.change(add_in(lune, spot_at("/w/lune", at))) {
            Ok(Some(Made::Worktree(worktree))) => worktree,
            made => panic!("{at} is made: {made:?}"),
        };
        let (one, two) = (worktree("one"), worktree("two"));
        let went = |worktree, millis| Change::Went { worktree, at: Used::of_millis(millis) };
        store.change(went(one, USED_MILLIS_MAX - 1)).unwrap();
        store.flush().unwrap();
        store.change(went(two, 1)).unwrap();
        store.flush().unwrap();
        let Err(Failed::Refused(why)) = store.change(went(one, 1)) else {
            panic!("a go steps past the latest use kept");
        };
        assert!(why.contains("no worktree is used past"), "{why}");
        store.flush().unwrap();

        drop(store);
        let again = store_at(Ok(file), core);
        assert_eq!(again.unkept(), None);
        let used = |worktree| again.projects().unwrap().find(worktree).unwrap().1.used();
        assert_eq!(used(one), Some(Used::of_millis(USED_MILLIS_MAX - 1)));
        assert_eq!(used(two), Some(Used::of_millis(USED_MILLIS_MAX)));
    }

    #[test]
    fn a_change_that_follows_the_mux_is_kept_unwritten_until_a_flush_writes_it() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core);
        let made = store.change(Change::AddProject { spot: alone("/a"), name: None }).unwrap();
        let Some(Made::Project(project)) = made else { panic!("/a is made") };
        let add = add_in(project, alone("/a/b"));
        let Some(Made::Found(worktree)) = store.change(add).unwrap() else {
            panic!("/a/b is in /a's own worktree");
        };
        let terminal = Terminal::new(store.fresh_terminal().unwrap(), dir("/a"), Runs::Shell);
        let (placed, size) = (Placed::Alone, Size::of(80, 24).unwrap());
        let view = View::Terminal(terminal);
        store.change(Change::Open { worktree, view, placed, size }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        heard(&commands);

        let folder = file.parent().unwrap();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o500)).unwrap();
        let unwritten = store.closed(Closing::Worktree(worktree));
        let flushed = store.flush();
        fs::set_permissions(folder, fs::Permissions::from_mode(0o700)).unwrap();
        let why = unwritten.unwrap().expect("a folder that refuses writes writes nothing");
        assert!(why.starts_with(&format!("{}: ", file.display())), "{why}");
        assert!(matches!(flushed, Err(Failed::Unwritten(_))), "{flushed:?}");
        assert_eq!(fs::read_to_string(&file).unwrap(), written);
        let emptied = |projects: &Projects| projects.find(worktree).unwrap().1.layout().is_empty();
        let [told] = heard(&commands).try_into().unwrap();
        assert!(emptied(&told.unwrap()), "the core hears of the close");
        assert!(emptied(store.projects().unwrap()));

        store.flush().unwrap();
        assert!(emptied(&parse(&fs::read_to_string(&file).unwrap()).unwrap()), "a flush writes it");
        assert_eq!(store.closed(Closing::Worktree(worktree)), Ok(None), "closed, it stays");
    }

    #[test]
    fn a_second_lock_of_one_file_waits_for_the_first_then_names_where_it_listens() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, _commands) = channel();
        let first = store_at(Ok(file.clone()), core);
        let lock = file.parent().unwrap().join(LOCK);
        assert_eq!(fs::read_to_string(&lock).unwrap(), LISTENS_IN);
        let Err(Unheld::Elsewhere(elsewhere)) = take_at(Ok(file.clone()), Path::new("/tmp/b"))
        else {
            panic!("a second lock of one file is taken");
        };
        let folder = file.parent().unwrap().to_owned();
        let holder = Some(PathBuf::from(LISTENS_IN));
        assert_eq!(elsewhere, Elsewhere { folder: folder.clone(), holder });
        let said = elsewhere.to_string();
        assert!(said.contains(&format!("listens in {LISTENS_IN}: `lune terminate`")), "{said}");

        fs::write(&lock, "").unwrap();
        let Err(Unheld::Elsewhere(older)) = take_at(Ok(file.clone()), Path::new("/tmp/b")) else {
            panic!("a second lock of one file is taken");
        };
        assert_eq!(older, Elsewhere { folder, holder: None });
        assert!(older.to_string().ends_with("one too old to say where it listens"), "{older}");

        let released = thread::spawn(move || {
            apart(const { Gap::of(HELD_PAUSE.saturating_mul(4)) });
            drop(first);
        });
        let third = take_at(Ok(file), Path::new("/tmp/b"));
        released.join().unwrap();
        assert!(third.is_ok(), "{:?}", third.err());
        assert_eq!(fs::read_to_string(&lock).unwrap(), "/tmp/b");
    }

    #[test]
    fn a_holder_s_folder_written_a_moment_after_its_lock_is_read() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join(LOCK);
        fs::write(&path, "").unwrap();
        let lock = File::open(&path).unwrap();
        let writing = thread::spawn(move || {
            apart(const { Gap::of(SAID_PAUSE) });
            fs::write(path, LISTENS_IN).unwrap();
        });
        assert_eq!(said_by(&lock), Some(PathBuf::from(LISTENS_IN)));
        writing.join().unwrap();
    }

    #[test]
    fn a_file_changed_behind_its_back_is_never_written_over_and_its_lock_stays_held() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core);
        store.change(Change::AddProject { spot: alone("/a"), name: None }).unwrap();
        let written = fs::read_to_string(&file).unwrap();
        let edited = written.replace(r#""name": "a""#, r#""name": "edited""#);
        assert_ne!(edited, written);
        fs::write(&file, &edited).unwrap();

        let change = Change::AddProject { spot: alone("/b"), name: None };
        let Err(Failed::NotKept(why)) = store.change(change) else {
            panic!("a change over an edited file is kept");
        };
        assert!(why.ends_with("it changed since luned read it, and so it is left as it is"));
        assert_eq!(store.unkept(), Some(why.as_str()));
        assert_eq!(heard(&commands).last(), Some(&Err(why)));
        assert_eq!(fs::read_to_string(&file).unwrap(), edited);
        let second = take_at(Ok(file), Path::new("/tmp/b"));
        assert!(matches!(second, Err(Unheld::Elsewhere(_))), "{:?}", second.err());
    }

    #[test]
    fn a_change_to_a_layout_alone_is_written_when_flushed_or_with_the_next_change() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let (core, commands) = channel();
        let mut store = store_at(Ok(file.clone()), core);
        let project = store.change(Change::AddProject { spot: alone("/a"), name: None });
        let Some(Made::Project(project)) = project.unwrap() else { panic!("/a is made") };
        let add = add_in(project, alone("/a/b"));
        let Some(Made::Found(worktree)) = store.change(add).unwrap() else {
            panic!("/a/b is in /a's own worktree");
        };
        let size = Size::of(80, 24).unwrap();
        let [first, second] = [(); 2].map(|()| store.fresh_terminal().unwrap());
        let [one, two] = [first, second].map(TerminalViewId::view);
        let opened = |terminal, placed| {
            let terminal = Terminal::new(terminal, dir("/a"), Runs::Shell);
            Change::Open { worktree, view: View::Terminal(terminal), placed, size }
        };
        store.change(opened(first, Placed::Alone)).unwrap();
        let beside = Placed::Beside { at: one, towards: Towards::Right };
        store.change(opened(second, beside)).unwrap();
        let read = || parse(&fs::read_to_string(&file).unwrap()).unwrap();
        let full = |projects: &Projects| projects.find(worktree).unwrap().1.layout().full();
        store.change(Change::Arrange { worktree, arranged: Arranged::Panes }).unwrap();
        let tabbed = |projects: &Projects| projects.find(worktree).unwrap().1.layout().tabbed();
        assert!(tabbed(&read()), "an arrangement too waits for the flush");

        store.relaid(Relaid::Fill { worktree, full: Some(two) }).unwrap();
        assert_eq!(full(store.projects().unwrap()), Some(two));
        assert_eq!(full(&read()), None, "a fill waits for the flush");
        let told = heard(&commands).pop().unwrap().unwrap();
        assert_eq!(full(&told), Some(two), "the core hears a fill at once");
        store.flush().unwrap();
        assert_eq!(full(&read()), Some(two));

        store.relaid(Relaid::Fill { worktree, full: None }).unwrap();
        let to = ProjectName::parse("b").unwrap();
        store.change(Change::RenameProject { project, to }).unwrap();
        let kept = *store.projects().unwrap() == read();
        assert!(kept, "a change kept at once takes the rest along");
        store.flush().unwrap();
        assert_eq!(read(), *store.projects().unwrap());
    }

    #[test]
    fn a_layout_relaid_sends_watchers_no_new_listing() {
        use lune_domain::projects::layout::{Direction, Growth, Sides};
        use crate::core::{self, Wakes};

        let scratch = tempfile::tempdir().unwrap();
        let (git, _git) = mpsc::sync_channel(1);
        let (github, _github) = mpsc::sync_channel(1);
        let core = core::start(Wakes { git, github }).unwrap();
        let mut store = store_at(Ok(file_in(&scratch)), core.clone());
        let project = store.change(Change::AddProject { spot: alone("/a"), name: None });
        let Some(Made::Project(project)) = project.unwrap() else { panic!("/a is made") };
        let Some(Made::Found(worktree)) = store.change(add_in(project, alone("/a/b"))).unwrap()
        else {
            panic!("/a/b is in /a's own worktree");
        };
        let size = Size::of(80, 24).unwrap();
        let [first, second] = [(); 2].map(|()| store.fresh_terminal().unwrap());
        let [one, two] = [first, second].map(TerminalViewId::view);
        let opened = |terminal, placed| {
            let terminal = Terminal::new(terminal, dir("/a"), Runs::Shell);
            Change::Open { worktree, view: View::Terminal(terminal), placed, size }
        };
        store.change(opened(first, Placed::Alone)).unwrap();
        store.change(opened(second, Placed::Beside { at: one, towards: Towards::Right })).unwrap();
        let listing = || core::ask(&core, Command::Host).expect("the core answers").version;
        let tabbed = (listing(), store.revision());
        store.change(Change::Arrange { worktree, arranged: Arranged::Panes }).unwrap();
        let before = (listing(), store.revision());
        assert_ne!(before, tabbed, "watchers hear of panes, as a listing says it");

        let (sides, growth) = (Sides::One(Direction::Left), Growth::Grow);
        let relaid = [
            Relaid::Focus { view: one },
            Relaid::Resize { view: two, sides, growth, size },
            Relaid::Fill { worktree, full: Some(two) },
            Relaid::Fill { worktree, full: None },
        ];
        for relaid in relaid {
            let said = format!("{relaid:?}");
            let was = store.projects().unwrap().clone();
            store.relaid(relaid).unwrap();
            assert_ne!(store.projects(), Some(&was), "{said} changes the projects");
            assert_eq!((listing(), store.revision()), before, "{said} sends no new listing");
        }

        store.change(Change::Swap { one, other: two }).unwrap();
        let (version, revision) = (listing(), store.revision());
        assert_ne!(version, before.0, "a swap sends watchers a new listing");
        assert_eq!(revision, before.1.next(), "a swap is a revision of the projects");
    }
}
