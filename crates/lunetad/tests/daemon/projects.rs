use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lunetad::api::change_projects_request::{
    AddProject, AddWorkstream, Change, Pull, ReorderProjects, ReorderWorkstreams, Start,
};
use lunetad::api::change_projects_response::Made;
use lunetad::api::projects::{Held, Kept};
use lunetad::api::workstream::Seen;
use lunetad::api::{self, ChangeProjectsRequest, ChangeProjectsResponse as Answered, On, Stage};
use lunetad::api::Version;
use tonic::{Code, Status};

use crate::host::{eventually, folder_in, start, Host};
use crate::remote::{hosts, reached, FakeSsh};
use crate::worktree::{cloned_as_wt, git, worktree_added};

pub fn asked(host: &Host, change: Change) -> Result<Option<u64>, Status> {
    asked_on(host, None, change)
}

fn asked_on(host: &Host, on: Option<&str>, change: Change) -> Result<Option<u64>, Status> {
    let made = |made| match made {
        Made::Project(id) | Made::Workstream(id) | Made::Found(id) | Made::Taken(id) => id,
    };
    answered_on(host, on, change).map(|answered| answered.made.map(made))
}

pub fn made(host: &Host, change: Change) -> Result<Option<Made>, Status> {
    answered_on(host, None, change).map(|answered| answered.made)
}

fn answered_on(host: &Host, on: Option<&str>, change: Change) -> Result<Answered, Status> {
    let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let on = on.map(|destination| On { destination: destination.to_owned() });
        let asked = ChangeProjectsRequest { on, change: Some(change) };
        Ok(daemon.change_projects(asked).await?.into_inner())
    })
}

fn changed(host: &Host, change: Change) -> Result<u64, Status> {
    Ok(asked(host, change)?.expect("an addition says what it made"))
}

pub fn project(host: &Host, dir: &Path, name: Option<&str>) -> u64 {
    let dir = dir.to_str().unwrap().to_owned();
    let add = AddProject { dir, name: name.map(str::to_owned) };
    changed(host, Change::AddProject(add)).unwrap()
}

pub fn workstream(host: &Host, project: u64, folder: &Path) -> u64 {
    let folder = folder.to_str().unwrap().to_owned();
    let add = AddWorkstream { project, folder, name: None };
    changed(host, Change::AddWorkstream(add)).unwrap()
}

pub fn kept(host: &Host) -> PathBuf {
    host.home.path().join(".local/state/luneta/projects")
}

fn held(host: &Host) -> Held {
    let projects = host.host().projects.and_then(|projects| projects.held);
    projects.expect("lunetad says whether it keeps projects")
}

fn unversioned(host: &Host) -> Held {
    let mut held = held(host);
    if let Held::Kept(kept) = &mut held {
        assert!(kept.version.take().is_some(), "lunetad says the version of the projects kept");
    }
    held
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
    fs::create_dir_all(vault.join("notes")).unwrap();
    let daemon = start(host.vars()).unwrap();

    let luneta = project(&host, &clone, None);
    let main = workstream(&host, luneta, &clone.join("main"));
    let backup = project(&host, &vault, Some("bitwarden backup"));
    let notes = workstream(&host, backup, &vault.join("notes"));
    let want = Held::Kept(Kept {
        version: None,
        projects: vec![
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
                    seen: Some(Seen::Stage(Stage::Working.into())),
                    terminals: Vec::new(),
                    used_at: None,
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
                    seen: Some(Seen::Stage(Stage::NoBranch.into())),
                    terminals: Vec::new(),
                    used_at: None,
                }],
            },
        ],
    });
    let seen = |host: &Host| {
        let held = unversioned(host);
        (held == want).then_some(()).ok_or(held)
    };
    eventually("the projects are watched, the branch read", || seen(&host).ok());
    let written = fs::read_to_string(kept(&host)).unwrap();

    drop(daemon);
    let _daemon = start(host.vars()).unwrap();
    eventually("the projects come back", || seen(&host).ok());
    assert_eq!(fs::read_to_string(kept(&host)).unwrap(), written);
}

#[test]
fn a_folder_that_goes_is_kept_and_said_to_be_missing() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (top, below) = (scratch.join("notes"), scratch.join("notes/drafts"));
    fs::create_dir_all(&below).unwrap();
    let _daemon = start(host.vars()).unwrap();
    let notes = project(&host, &top, None);
    assert_eq!(workstream(&host, notes, &below), workstream(&host, notes, &top), "one, its own");
    let seen = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        (project.missing, project.workstreams[0].seen)
    };
    let no_branch = Some(Seen::Stage(Stage::NoBranch.into()));

    let away = scratch.join("away");
    fs::rename(&top, &away).unwrap();
    let gone = || (seen(&host) == (true, Some(Seen::Missing(())))).then_some(());
    eventually("the folder is said to be missing", gone);
    fs::rename(&away, &top).unwrap();
    let back = || (seen(&host) == (false, no_branch)).then_some(());
    eventually("the folder is back", back);
    let nowhere = text(&scratch.join("nowhere"));
    let add = AddWorkstream { project: notes, folder: nowhere, name: None };
    let refused = changed(&host, Change::AddWorkstream(add)).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument);
    assert!(refused.message().starts_with("folder: "), "{}", refused.message());
    let inside = AddProject { dir: text(&below), name: None };
    let refused = changed(&host, Change::AddProject(inside)).unwrap_err();
    assert_eq!(refused.code(), Code::AlreadyExists);
    assert!(refused.message().contains("is inside"), "{}", refused.message());
}

#[test]
fn a_clone_s_worktrees_made_or_removed_outside_luneta_come_and_go_on_the_sweep() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("luneta"));
    cloned_as_wt(&scratch, &up, &clone);
    let _daemon = start(host.vars()).unwrap();
    let luneta = project(&host, &clone.join("main"), None);
    let folders = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        assert_eq!((project.id, project.dir.as_str()), (luneta, text(&clone).as_str()));
        project.workstreams.iter().map(|it| it.folder.clone()).collect::<Vec<_>>()
    };
    assert_eq!(folders(&host), [text(&clone.join("main"))], "a project is its clone's");

    worktree_added(&clone, "feat-2");
    let both = [text(&clone.join("main")), text(&clone.join("feat-2"))];
    eventually("a worktree made is listed", || (folders(&host) == both).then_some(()));
    git(&clone.join(".bare"), &["worktree", "remove", "../feat-2"]);
    let one = [text(&clone.join("main"))];
    eventually("a worktree removed goes", || (folders(&host) == one).then_some(()));
}

#[test]
fn a_folder_s_project_becomes_its_clone_s_when_wt_clones_into_its_folder() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, folder) = (scratch.join("up"), scratch.join("luneta"));
    fs::create_dir_all(&folder).unwrap();
    let _daemon = start(host.vars()).unwrap();
    let luneta = project(&host, &folder, None);
    let rows = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        assert_eq!((project.id, project.dir.as_str()), (luneta, text(&folder).as_str()));
        let rows = project.workstreams.iter().map(|it| (it.id, it.folder.clone()));
        rows.collect::<Vec<_>>()
    };
    let [(own, at)] = rows(&host).try_into().unwrap();
    assert_eq!(at, text(&folder), "a plain folder's project holds the folder");

    fs::remove_dir(&folder).unwrap();
    cloned_as_wt(&scratch, &up, &folder);
    worktree_added(&folder, "feat-2");
    let both = [text(&folder.join("feat-2")), text(&folder.join("main"))];
    let rooted = |host: &Host| {
        let held = rows(host);
        let mut folders: Vec<String> = held.iter().map(|(_, it)| it.clone()).collect();
        folders.sort();
        let kept = held.iter().any(|(id, _)| *id == own);
        (kept && folders == both).then_some(())
    };
    eventually("the project is its clone's, its workstream on a worktree", || rooted(&host));
}

#[test]
fn a_projects_file_lunetad_cannot_read_is_left_as_it_is_and_no_change_is_kept() {
    let host = Host::new();
    let file = kept(&host);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let newer = "{\"version\": 9, \"projects\": [], \"views\": []}\n";
    fs::write(&file, newer).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let said = daemon.said_after(Duration::ZERO).join("\n");
    assert!(said.contains("of version 9, and this lunetad reads only 1 to 8"), "{said}");

    let add = AddProject { dir: text(scratch.path()), name: None };
    let refused = changed(&host, Change::AddProject(add)).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition);
    assert!(refused.message().ends_with("and so it is left as it is"), "{}", refused.message());
    let Held::Unkept(unkept) = held(&host) else {
        panic!("no projects are kept");
    };
    assert_eq!(unkept, refused.message());
    assert_eq!(fs::read_to_string(&file).unwrap(), newer);
}

fn order(host: &Host) -> Vec<u64> {
    let Held::Kept(Kept { projects, .. }) = held(host) else {
        panic!("the projects are kept");
    };
    projects.iter().map(|project| project.id).collect()
}

fn version(host: &Host) -> Version {
    let Held::Kept(Kept { version, .. }) = held(host) else {
        panic!("the projects are kept");
    };
    version.expect("lunetad says the version of the projects kept")
}

#[test]
fn a_reorder_made_on_a_stale_order_is_refused_here_and_on_another_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let scratch = tempfile::tempdir().unwrap();

    let scratch = fs::canonicalize(scratch.path()).unwrap();
    for (keeper, on) in [(&here, None), (&far, Some("far"))] {
        let dir = |name: &str| {
            let dir = scratch.join(on.unwrap_or("here")).join(name);
            fs::create_dir_all(&dir).unwrap();
            dir
        };
        let clone = dir("a");
        cloned_as_wt(clone.parent().unwrap(), &dir("up"), &clone);
        worktree_added(&clone, "one");
        worktree_added(&clone, "two");
        let [a, b, c] = ["a", "b", "c"].map(|name| project(keeper, &dir(name), None));
        let reorder = |seen: &[u64], projects: &[u64]| {
            let (seen, projects) = (seen.to_vec(), projects.to_vec());
            asked_on(&here, on, Change::ReorderProjects(ReorderProjects { projects, seen }))
        };
        let kept = |want: Vec<u64>| {
            let kept = || (order(keeper) == want).then_some(());
            eventually(&format!("{on:?} keeps {want:?}"), kept);
        };
        reorder(&[a, b, c], &[c, a, b]).unwrap();
        kept(vec![c, a, b]);
        for seen in [&[a, b, c][..], &[]] {
            let stale = reorder(seen, &[b, a, c]).unwrap_err();
            assert_eq!(stale.code(), Code::Aborted, "{on:?}: {stale:?}");
            assert!(stale.message().contains("they changed since"), "{on:?}: {stale:?}");
        }
        let unlike = reorder(&[c, a, b], &[a, b]).unwrap_err();
        assert_eq!(unlike.code(), Code::InvalidArgument, "{on:?}: {unlike:?}");
        kept(vec![c, a, b]);
        reorder(&[c, a, b], &[a, c, b]).unwrap();
        let last = ReorderProjects { projects: vec![a, b, c], seen: vec![a, c, b] };
        let answered = answered_on(&here, on, Change::ReorderProjects(last)).unwrap();
        let landed = answered.kept.expect("lunetad says the version it keeps a move at");
        let shown = || {
            let now = version(keeper);
            (now.pid == landed.pid && now.seq >= landed.seq).then(|| order(keeper))
        };
        let shown = eventually(&format!("{on:?} shows the version {landed:?}"), shown);
        assert_eq!(shown, [a, b, c], "{on:?}: the version a move is kept at shows it");

        let main = workstream(keeper, a, &dir("a/main"));
        let one = workstream(keeper, a, &dir("a/one"));
        let two = workstream(keeper, a, &dir("a/two"));
        let swap = |seen: Vec<u64>| {
            let workstreams = vec![main, two, one];
            let swap = ReorderWorkstreams { project: a, workstreams, seen };
            asked_on(&here, on, Change::ReorderWorkstreams(swap))
        };
        swap(vec![main, one, two]).unwrap();
        let stale = swap(vec![main, one, two]).unwrap_err();
        assert!(stale.message().contains("they changed since"), "{on:?}: {stale:?}");
    }
}

#[test]
fn a_start_or_a_pull_on_another_machine_finds_its_project_through_a_link_there() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let scratch = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(scratch.path()).unwrap().join("real");
    for folder in ["luneta", "elo", "elsewhere"] {
        fs::create_dir_all(real.join(folder)).unwrap();
    }
    cloned_as_wt(&real, &real.join("up"), &real.join("luneta"));
    git(&real.join("luneta/.bare"), &["worktree", "add", "-q", "../feat", "feat"]);
    let link = scratch.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let luneta = project(&far, &real.join("luneta"), None);
    let asked = |change| answered_on(&here, Some("far"), change).unwrap().made;
    let start = |at: &str| {
        let start = Start { folder: text(&link.join(at)), name: None };
        asked(Change::Start(start))
    };
    let folders = || {
        let Held::Kept(kept) = held(&far) else { panic!("far keeps its projects") };
        let projects = kept.projects.into_iter();
        let folders = |it: api::Project| it.workstreams.into_iter().map(|it| (it.id, it.folder));
        projects.flat_map(folders).collect::<Vec<_>>()
    };

    let Some(Made::Workstream(feat)) = start("luneta/feat") else {
        panic!("a start in a worktree of a project far keeps goes to a workstream there");
    };
    let listed = folders();
    let main = listed.iter().find(|(_, it)| *it == text(&real.join("luneta/main"))).unwrap().0;
    let want = [(feat, text(&real.join("luneta/feat"))), (main, text(&real.join("luneta/main")))];
    assert_eq!(listed, want, "one for each worktree of project {luneta}");
    assert_eq!(start("luneta/feat"), Some(Made::Workstream(feat)), "the same one again");
    assert_eq!(start("luneta"), Some(Made::Workstream(feat)), "its clone's root opens the first");
    worktree_added(&real.join("luneta"), "pr-7");

    let pull = Pull {
        holder: text(&link.join("elsewhere")),
        folder: text(&link.join("luneta/pr-7")),
        project: "cloned".to_owned(),
        name: "pr-7".to_owned(),
    };
    let Some(Made::Workstream(pr)) = asked(Change::Pull(pull)) else {
        panic!("a pull makes a workstream");
    };
    assert!(folders().contains(&(pr, text(&real.join("luneta/pr-7")))), "in project luneta");

    let Some(Made::Workstream(elo)) = start("elo") else {
        panic!("a start in no project makes one and goes to its workstream");
    };
    let Held::Kept(kept) = held(&far) else { panic!("far keeps its projects") };
    let held_in = |it: &&api::Project| it.workstreams.iter().any(|it| it.id == elo);
    let made = kept.projects.iter().find(held_in).expect("far keeps elo");
    assert_eq!(made.dir, text(&real.join("elo")));
    let holds = Start { folder: text(&link), name: None };
    let refused = answered_on(&here, Some("far"), Change::Start(holds)).unwrap_err();
    assert_eq!(refused.code(), Code::AlreadyExists, "{refused:?}");
}
