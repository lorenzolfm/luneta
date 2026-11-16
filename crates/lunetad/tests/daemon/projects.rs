use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lunetad::api::change_projects_request::{AddProject, AddWorkstream, Change};
use lunetad::api::{self, ChangeProjectsRequest, Stage};
use tonic::{Code, Status};

use crate::host::{eventually, folder_in, start, Host};
use crate::worktree::cloned_as_wt;

fn changed(host: &Host, change: Change) -> Result<u64, Status> {
    let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let asked = ChangeProjectsRequest { on: None, change: Some(change) };
        let made = daemon.change_projects(asked).await?.into_inner().made;
        Ok(made.expect("an addition says what it made"))
    })
}

fn project(host: &Host, dir: &Path, name: Option<&str>) -> u64 {
    let dir = dir.to_str().unwrap().to_owned();
    let add = AddProject { dir, name: name.map(str::to_owned) };
    changed(host, Change::AddProject(add)).unwrap()
}

fn workstream(host: &Host, project: u64, folder: &Path) -> u64 {
    let folder = folder.to_str().unwrap().to_owned();
    let add = AddWorkstream { project, folder, name: None };
    changed(host, Change::AddWorkstream(add)).unwrap()
}

fn kept(host: &Host) -> PathBuf {
    host.home.path().join(".local/state/luneta/projects")
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn projects_and_workstreams_are_watched_and_come_back_when_lunetad_starts_again() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, vault) = (scratch.join("up"), scratch.join("luneta"), scratch.join("Vault"));
    cloned_as_wt(&scratch, &up, &clone);
    fs::create_dir(&vault).unwrap();
    let daemon = start(host.vars()).unwrap();

    let luneta = project(&host, &clone, None);
    let main = workstream(&host, luneta, &clone.join("main"));
    let backup = project(&host, &vault, Some("bitwarden backup"));
    let notes = workstream(&host, backup, &vault);
    let want = vec![
        api::Project {
            id: luneta,
            name: "luneta".to_owned(),
            dir: text(&clone),
            missing: false,
            workstreams: vec![api::Workstream {
                id: main,
                name: "main".to_owned(),
                renamed: false,
                folder: text(&clone.join("main")),
                missing: false,
                stage: Stage::Working.into(),
            }],
        },
        api::Project {
            id: backup,
            name: "bitwarden backup".to_owned(),
            dir: text(&vault),
            missing: false,
            workstreams: vec![api::Workstream {
                id: notes,
                name: "Vault".to_owned(),
                renamed: false,
                folder: text(&vault),
                missing: false,
                stage: Stage::NoBranch.into(),
            }],
        },
    ];
    let seen = |host: &Host| {
        let projects = host.host().projects;
        (projects == want).then_some(()).ok_or(projects)
    };
    eventually("the projects are watched, the branch read", || seen(&host).ok());
    let written = fs::read_to_string(kept(&host)).unwrap();

    drop(daemon);
    let _daemon = start(host.vars()).unwrap();
    eventually("the projects come back", || seen(&host).ok());
    assert_eq!(fs::read_to_string(kept(&host)).unwrap(), written);
}

#[test]
fn a_workstream_whose_folder_goes_is_kept_and_said_to_be_missing() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (top, below) = (scratch.join("notes"), scratch.join("notes/drafts"));
    fs::create_dir_all(&below).unwrap();
    let _daemon = start(host.vars()).unwrap();
    let notes = project(&host, &top, None);
    workstream(&host, notes, &below);
    let missing = |host: &Host| {
        let projects = host.host().projects;
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        (project.missing, project.workstreams[0].missing)
    };

    fs::remove_dir(&below).unwrap();
    let gone = || (missing(&host) == (false, true)).then_some(());
    eventually("the folder is said to be missing", gone);
    fs::create_dir(&below).unwrap();
    let back = || (missing(&host) == (false, false)).then_some(());
    eventually("the folder is back", back);
    let nowhere = text(&scratch.join("nowhere"));
    let add = AddWorkstream { project: notes, folder: nowhere, name: None };
    let refused = changed(&host, Change::AddWorkstream(add)).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument);
    assert!(refused.message().starts_with("folder: "), "{}", refused.message());
}

#[test]
fn a_projects_file_lunetad_cannot_read_is_left_as_it_is_and_no_change_is_kept() {
    let host = Host::new();
    let file = kept(&host);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let newer = "{\"version\": 2, \"projects\": [], \"views\": []}\n";
    fs::write(&file, newer).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let said = daemon.said_after(Duration::ZERO).join("\n");
    assert!(said.contains("of version 2, and this lunetad reads only 1"), "{said}");

    let add = AddProject { dir: text(scratch.path()), name: None };
    let refused = changed(&host, Change::AddProject(add)).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert!(refused.message().ends_with("and so it is left as it is"), "{}", refused.message());
    assert_eq!(host.host().projects, []);
    assert_eq!(fs::read_to_string(&file).unwrap(), newer);
}
