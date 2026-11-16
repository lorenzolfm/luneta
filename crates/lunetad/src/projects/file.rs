use std::fs::{self, File};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Below, Change, Dir, Id, Name, Project, Projects, Workstream};
use super::{NAME_MAX, PATH_BYTES_MAX, PROJECTS_MAX, WORKSTREAMS_MAX};
use crate::state::{self, Unset};

pub const FILE: &str = "projects";

const VERSION: u32 = 1;

const ENTRY_BYTES_MAX: usize = 2 * (PATH_BYTES_MAX + 4 * NAME_MAX) + 256;

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
}

impl Store {
    #[must_use]
    pub fn open() -> Store {
        Store::at(state::path(FILE))
    }

    #[must_use]
    pub fn at(file: Result<PathBuf, Unset>) -> Store {
        let file = match file {
            Ok(file) => file,
            Err(unset) => {
                let why = format!("projects are kept nowhere: {unset}");
                return Store { file: Err(why), projects: Projects::default() };
            },
        };
        match read(&file) {
            Ok(projects) => Store { file: Ok(file), projects },
            Err(why) => {
                let why = format!("{}: {why}, and so it is left as it is", file.display());
                Store { file: Err(why), projects: Projects::default() }
            },
        }
    }

    #[must_use]
    pub fn unkept(&self) -> Option<&str> {
        self.file.as_ref().err().map(String::as_str)
    }

    #[must_use]
    pub fn projects(&self) -> &Projects {
        &self.projects
    }

    pub fn change(&mut self, change: Change) -> Result<Option<Id>, String> {
        let file = self.file.as_ref().map_err(Clone::clone)?;
        let (projects, made) = self.projects.changed(change, Id::random)?;
        write(file, &projects).map_err(|error| format!("{}: {error}", file.display()))?;
        self.projects = projects;
        Ok(made)
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
    fs::create_dir_all(folder)?;
    let mut unfinished = tempfile::Builder::new().prefix(".projects.").tempfile_in(folder)?;
    unfinished.write_all(written(projects).as_bytes())?;
    unfinished.as_file().sync_all()?;
    unfinished.persist(file).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn file_in(scratch: &tempfile::TempDir) -> PathBuf {
        scratch.path().join("luneta").join(FILE)
    }

    #[test]
    fn what_a_store_keeps_comes_back_when_it_opens_again() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        let mut store = Store::at(Ok(file.clone()));
        assert_eq!(store.unkept(), None);
        let vault = Some(Name::parse("bitwarden backup").unwrap());
        let made = store.change(Change::AddProject { dir: dir("/home/me/Vault"), name: vault });
        let vault = made.unwrap().unwrap();
        store.change(Change::AddProject { dir: dir("/w/luneta"), name: None }).unwrap();
        let top = dir("/home/me/Vault");
        store.change(Change::AddWorkstream { project: vault, folder: top, name: None }).unwrap();
        let (sub, named) = (dir("/home/me/Vault/a/b"), Some(Name::parse("release").unwrap()));
        store.change(Change::AddWorkstream { project: vault, folder: sub, name: named }).unwrap();

        let again = Store::at(Ok(file.clone()));
        assert_eq!(again.unkept(), None);
        assert_eq!(again.projects(), store.projects());
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
            let mut store = Store::at(Ok(file.clone()));
            let unkept = store.unkept().unwrap().to_owned();
            assert!(unkept.contains(why), "{unkept:?} for {text}");
            assert!(unkept.ends_with("and so it is left as it is"), "{unkept}");
            assert_eq!(store.projects(), &Projects::default());
            let change = Change::AddProject { dir: dir("/c"), name: None };
            assert_eq!(store.change(change), Err(unkept));
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn a_file_too_big_is_not_read_whole() {
        let scratch = tempfile::tempdir().unwrap();
        let file = file_in(&scratch);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, " ".repeat(FILE_BYTES_MAX + 1)).unwrap();
        let store = Store::at(Ok(file));
        assert!(store.unkept().unwrap().contains("over 8388608 bytes"));
    }

    #[test]
    fn with_no_state_folder_nothing_is_kept_and_it_says_why() {
        let mut store = Store::at(Err(Unset));
        let unkept = store.unkept().unwrap().to_owned();
        assert!(unkept.starts_with("projects are kept nowhere: neither XDG_STATE_HOME"));
        let change = Change::AddProject { dir: dir("/c"), name: None };
        assert_eq!(store.change(change), Err(unkept));
    }
}
