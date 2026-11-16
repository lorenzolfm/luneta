use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;

use serde::{Deserialize, Serialize};

use super::{Below, Change, Dir, Id, Name, Project, Projects, Workstream};
use super::{NAME_CHARS_MAX, PATH_BYTES_MAX, PROJECTS_MAX, WORKSTREAMS_MAX};
use crate::core::Command;
use crate::state::{self, Unset};

pub const FILE: &str = "projects";

const VERSION: u32 = 1;

const CHAR_BYTES_MAX: usize = 4;

const ESCAPED_BYTES_PER_BYTE: usize = 2;

const NAME_BYTES_MAX: usize = CHAR_BYTES_MAX * NAME_CHARS_MAX;

const ENTRY_BYTES_MAX: usize = ESCAPED_BYTES_PER_BYTE * (PATH_BYTES_MAX + NAME_BYTES_MAX) + 256;

const FILE_BYTES_MAX: usize = 8 * 1024 * 1024;

const _: () = assert!((PROJECTS_MAX + WORKSTREAMS_MAX) * ENTRY_BYTES_MAX <= FILE_BYTES_MAX);

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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    below: String,
}

pub struct Store {
    file: Result<PathBuf, String>,
    projects: Projects,
    core: SyncSender<Command>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    NotKept(String),
    KeptUntold,
}

impl Store {
    pub fn open(core: SyncSender<Command>) -> io::Result<Store> {
        Store::at(state::path(FILE), core)
    }

    pub fn at(file: Result<PathBuf, Unset>, core: SyncSender<Command>) -> io::Result<Store> {
        let (file, projects) = match file {
            Err(unset) => (Err(format!("projects are kept nowhere: {unset}")), Projects::default()),
            Ok(file) => match read(&file) {
                Ok(projects) => (Ok(file), projects),
                Err(why) => {
                    let why = format!("{}: {why}, and so it is left as it is", file.display());
                    (Err(why), Projects::default())
                },
            },
        };
        let store = Store { file, projects, core };
        store.tell().map_err(|_| io::Error::other("the daemon's core has stopped"))?;
        Ok(store)
    }

    #[must_use]
    pub fn unkept(&self) -> Option<&str> {
        self.file.as_ref().err().map(String::as_str)
    }

    pub fn change(&mut self, change: Change) -> Result<Option<Id>, Failed> {
        let file = self.file.as_ref().map_err(|why| Failed::NotKept(why.clone()))?;
        let changed = self.projects.changed(change, Id::random);
        let (projects, made) = changed.map_err(Failed::NotKept)?;
        let unwritten = |error: io::Error| Failed::NotKept(format!("{}: {error}", file.display()));
        write(file, &projects).map_err(unwritten)?;
        self.projects = projects;
        self.tell()?;
        Ok(made)
    }

    fn tell(&self) -> Result<(), Failed> {
        let told = self.core.send(Command::Projects(self.projects.clone()));
        told.map_err(|_| Failed::KeptUntold)
    }
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
    if version != VERSION {
        return Err(format!("it is of version {version}, and this lunetad reads only {VERSION}"));
    }
    let kept: Kept = serde_json::from_str(text).map_err(|error| error.to_string())?;
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
        Ok(Workstream {
            id: self.id.parse()?,
            renamed: self.name.as_deref().map(Name::parse).transpose()?,
            below: Below::parse(&self.below)?,
        })
    }

    fn of(workstream: &Workstream) -> KeptWorkstream {
        KeptWorkstream {
            id: workstream.id.to_string(),
            name: workstream.renamed.as_ref().map(|name| name.0.clone()),
            below: workstream.below.0.clone(),
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
    File::open(folder)?.sync_all()?;
    File::open(above)?.sync_all()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{self, Receiver};

    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn channel() -> (SyncSender<Command>, Receiver<Command>) {
        mpsc::sync_channel(16)
    }

    fn heard(commands: &Receiver<Command>) -> Vec<Projects> {
        let told = |command| {
            let Command::Projects(projects) = command else {
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
        let vault = made.unwrap().unwrap();
        store.change(Change::AddProject { dir: dir("/w/luneta"), name: None }).unwrap();
        let top = dir("/home/me/Vault");
        store.change(Change::AddWorkstream { project: vault, folder: top, name: None }).unwrap();
        let (sub, named) = (dir("/home/me/Vault/a/b"), Some(Name::parse("release").unwrap()));
        store.change(Change::AddWorkstream { project: vault, folder: sub, name: named }).unwrap();

        let mut told = heard(&commands);
        assert_eq!(told.len(), 5);
        assert_eq!(told[0], Projects::default());
        let kept = told.pop().unwrap();
        assert_eq!(kept.all().len(), 2);

        let again = Store::at(Ok(file.clone()), core).unwrap();
        assert_eq!(again.unkept(), None);
        assert_eq!(heard(&commands), [kept]);
        let left: Vec<String> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, [FILE]);
    }

    #[test]
    fn the_file_says_its_version_and_only_what_differs_from_the_defaults() {
        let mut projects = Projects::default();
        let ids = [1, 2, 3].map(|id| Id::of(id).unwrap());
        let mut drawn = ids.into_iter();
        let mut draw = || Ok(drawn.next().unwrap());
        let change = Change::AddProject { dir: dir("/w/luneta"), name: None };
        (projects, _) = projects.changed(change, &mut draw).unwrap();
        let top = Change::AddWorkstream { project: ids[0], folder: dir("/w/luneta"), name: None };
        (projects, _) = projects.changed(top, &mut draw).unwrap();
        let named = Some(Name::parse("release").unwrap());
        let folder = dir("/w/luneta/main");
        let main = Change::AddWorkstream { project: ids[0], folder, name: named };
        (projects, _) = projects.changed(main, &mut draw).unwrap();
        let want = r#"{
  "version": 1,
  "projects": [
    {
      "id": "0000000000000001",
      "name": "luneta",
      "dir": "/w/luneta",
      "workstreams": [
        {
          "id": "0000000000000002"
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
        assert_eq!(parse(want), Ok(projects));
    }

    #[test]
    fn a_file_it_cannot_read_is_never_written_over() {
        let project = r#"{"id": "0000000000000001", "name": "a", "dir": "/a"}"#;
        let refused = [
            ("{", "EOF"),
            (r#"{"version": 2, "projects": []}"#, "of version 2, and this lunetad reads only 1"),
            (r#"{"version": 1, "projects": [], "views": []}"#, "unknown field `views`"),
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
            assert_eq!(heard(&commands), [Projects::default()]);
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
        assert!(store.unkept().unwrap().contains("over 8388608 bytes"));
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

        let (core, commands) = channel();
        Store::at(Ok(file), core).unwrap();
        let [kept] = heard(&commands).try_into().unwrap();
        assert_eq!(kept.all()[0].dir().as_str(), "/c");
    }
}
