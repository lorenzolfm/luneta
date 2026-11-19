use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use lunetad::api::change_projects_request::{
    AddProject, AddWorktree, Change, CloseWorktree, Pull, ReorderProjects, ReorderWorktrees, Start,
};
use lunetad::api::change_projects_response::Made;
use lunetad::api::project::clone::Listing;
use lunetad::api::project::{self, Holds};
use lunetad::api::projects::{Held, Kept};
use lunetad::api::worktree::Seen;
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
        Made::Project(id) | Made::Worktree(id) | Made::Found(id) | Made::Taken(id) => id,
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

pub const FIRST: [&str; 2] = ["sleep", "600"];

pub fn worktree(host: &Host, project: u64, folder: &Path) -> u64 {
    let folder = folder.to_str().unwrap().to_owned();
    let argv = FIRST.map(str::to_owned).to_vec();
    let add = AddWorktree { project, folder, name: None, argv };
    changed(host, Change::AddWorktree(add)).unwrap()
}

pub fn unopened_in(project: &api::Project) -> Option<Vec<String>> {
    let Some(Holds::Clone(clone)) = &project.holds else {
        return None;
    };
    let Some(Listing::Unopened(unopened)) = &clone.listing else {
        return None;
    };
    Some(unopened.subfolders.iter().map(|it| format!("{}/{it}", project.dir)).collect())
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

fn commands_of(held: &mut Held) -> Vec<Vec<Option<String>>> {
    let Held::Kept(kept) = held else {
        return Vec::new();
    };
    let mut commands = Vec::new();
    for project in &mut kept.projects {
        let worktrees: &mut [api::Worktree] = match &mut project.holds {
            Some(Holds::Clone(clone)) => &mut clone.worktrees,
            Some(Holds::Folder(own)) => std::slice::from_mut(own),
            None => &mut [],
        };
        for worktree in worktrees {
            let terminals = std::mem::take(&mut worktree.terminals);
            commands.push(terminals.into_iter().map(|it| it.command).collect());
        }
    }
    commands
}

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_owned()
}

#[test]
fn projects_and_worktrees_are_watched_and_come_back_when_lunetad_starts_again() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, vault) = (scratch.join("up"), scratch.join("luneta"), scratch.join("Vault"));
    cloned_as_wt(&scratch, &up, &clone);
    fs::create_dir_all(vault.join("notes")).unwrap();
    let daemon = start(host.vars()).unwrap();

    let luneta = project(&host, &clone, None);
    let main = worktree(&host, luneta, &clone.join("main"));
    let backup = project(&host, &vault, Some("bitwarden backup"));
    let notes = worktree(&host, backup, &vault.join("notes"));
    let want = Held::Kept(Kept {
        version: None,
        projects: vec![
            api::Project {
                id: luneta,
                name: "luneta".to_owned(),
                dir: text(&clone),
                missing: false,
                holds: Some(Holds::Clone(project::Clone {
                    worktrees: vec![api::Worktree {
                        id: main,
                        name: "main".to_owned(),
                        renamed: false,
                        folder: text(&clone.join("main")),
                        seen: Some(Seen::Stage(Stage::Working.into())),
                        terminals: Vec::new(),
                        used_at: None,
                    }],
                    listing: Some(Listing::Unopened(project::Unopened { subfolders: Vec::new() })),
                })),
            },
            api::Project {
                id: backup,
                name: "bitwarden backup".to_owned(),
                dir: text(&vault),
                missing: false,
                holds: Some(Holds::Folder(api::Worktree {
                    id: notes,
                    name: "Vault".to_owned(),
                    renamed: false,
                    folder: text(&vault),
                    seen: Some(Seen::Stage(Stage::NoBranch.into())),
                    terminals: Vec::new(),
                    used_at: None,
                })),
            },
        ],
    });
    let first = vec![vec![Some(FIRST.join(" "))], Vec::new()];
    let seen = |host: &Host| {
        let mut held = unversioned(host);
        let commands = commands_of(&mut held);
        (held == want && commands == first).then_some(()).ok_or(held)
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
    assert_eq!(worktree(&host, notes, &below), worktree(&host, notes, &top), "one, its own");
    let seen = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        (project.missing, project.worktrees().unwrap()[0].seen)
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
    let add = AddWorktree { project: notes, folder: nowhere, name: None, argv: Vec::new() };
    let refused = changed(&host, Change::AddWorktree(add)).unwrap_err();
    assert_eq!(refused.code(), Code::InvalidArgument);
    assert!(refused.message().starts_with("folder: "), "{}", refused.message());
    let inside = AddProject { dir: text(&below), name: None };
    let refused = changed(&host, Change::AddProject(inside)).unwrap_err();
    assert_eq!(refused.code(), Code::AlreadyExists);
    assert!(refused.message().contains("is inside"), "{}", refused.message());
}

#[test]
fn a_clone_s_worktrees_made_or_removed_outside_luneta_are_listed_unopened_on_the_sweep() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("luneta"));
    cloned_as_wt(&scratch, &up, &clone);
    let _daemon = start(host.vars()).unwrap();
    let luneta = project(&host, &clone.join("main"), None);
    let listed = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        assert_eq!((project.id, project.dir.as_str()), (luneta, text(&clone).as_str()));
        let opened = project.worktrees().unwrap().iter().map(|it| it.folder.clone());
        (opened.collect::<Vec<_>>(), unopened_in(project).unwrap_or_default())
    };
    let (main, feat) = (text(&clone.join("main")), text(&clone.join("feat-2")));
    let none = Vec::<String>::new;
    let unopened = (none(), vec![main.clone()]);
    eventually("a project is its clone's, none open", || (listed(&host) == unopened).then_some(()));

    worktree_added(&clone, "feat-2");
    let both = (none(), vec![feat.clone(), main.clone()]);
    eventually("a worktree made is listed, not opened", || (listed(&host) == both).then_some(()));
    let made = worktree(&host, luneta, &clone.join("feat-2"));
    let opened = (vec![feat.clone()], vec![main.clone()]);
    eventually("one opened is a row", || (listed(&host) == opened).then_some(()));
    git(&clone.join(".bare"), &["worktree", "remove", "--force", "../feat-2"]);
    thread::sleep(Duration::from_secs(3));
    assert_eq!(listed(&host), opened, "a worktree removed keeps its row while it holds a terminal");
    let Held::Kept(Kept { projects, .. }) = held(&host) else { panic!("the projects are kept") };
    let seen = projects[0].worktrees().unwrap()[0].terminals.iter().map(|it| it.id).collect();
    let closing = Change::CloseWorktree(CloseWorktree { worktree: made, seen });
    assert_eq!(asked(&host, closing).unwrap(), None);
    eventually("closed, it is offered no more", || (listed(&host) == unopened).then_some(()));
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
        let rows = project.worktrees().unwrap().iter().map(|it| (it.id, it.folder.clone()));
        rows.collect::<Vec<_>>()
    };
    let [(_, at)] = rows(&host).try_into().unwrap();
    assert_eq!(at, text(&folder), "a plain folder's project holds the folder");

    fs::remove_dir(&folder).unwrap();
    cloned_as_wt(&scratch, &up, &folder);
    worktree_added(&folder, "feat-2");
    let both = vec![text(&folder.join("feat-2")), text(&folder.join("main"))];
    let rooted = |host: &Host| {
        let Held::Kept(Kept { projects, .. }) = held(host) else {
            panic!("the projects are kept");
        };
        let [project] = projects.as_slice() else {
            panic!("{projects:?}");
        };
        let opened = project.worktrees().unwrap();
        let held = (project.id, opened.is_empty(), unopened_in(project));
        (held == (luneta, true, Some(both.clone()))).then_some(())
    };
    let none = "the project is its clone's, keeping its id, and its own with no view goes";
    eventually(none, || rooted(&host));
}

#[test]
fn an_old_file_forgets_the_worktrees_it_kept_with_no_view_and_is_written_again() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let clone = text(&scratch.join("n"));
    let file = kept(&host);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let laid = r#""layout": {"tree": [{"terminal": {"id": "0000000000000004", "folder": "/n/main",
        "runs": {"command": "sleep 600"}}}]}"#;
    let old = format!(
        r#"{{"version": 8, "projects": [{{"id": "0000000000000001", "name": "n", "dir": "{clone}",
        "holds": {{"clone": [{{"id": "0000000000000002", "worktree": "old"}},
        {{"id": "0000000000000003", "worktree": "main", {laid}}}]}}}}]}}"#
    );
    fs::write(&file, old).unwrap();
    let _daemon = start(host.vars()).unwrap();
    let Held::Kept(Kept { projects, .. }) = held(&host) else { panic!("the old file is read") };
    let rows: Vec<u64> = projects[0].worktrees().unwrap().iter().map(|it| it.id).collect();
    assert_eq!(rows, [3], "the worktree kept with no view goes, and the one with a view stays");
    let written = fs::read_to_string(&file).unwrap();
    assert!(written.starts_with("{\n  \"version\": 9,"), "written at once: {written}");
    assert!(!written.contains("\"old\""), "{written}");
}

#[test]
fn a_worktree_whose_first_terminal_does_not_start_is_not_made_nor_a_project_made_with_it() {
    let owned = crate::own::Owned::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("luneta"));
    cloned_as_wt(&scratch, &up, &clone);
    worktree_added(&clone, "two");
    let elo = scratch.join("elo");
    cloned_as_wt(&scratch, &scratch.join("elo-up"), &elo);
    let plain = scratch.join("plain");
    fs::create_dir(&plain).unwrap();
    let luneta = project(&owned.host, &clone, None);
    let main = worktree(&owned.host, luneta, &clone.join("main"));
    let stalled = owned.stall_mux();
    let argv = FIRST.map(str::to_owned).to_vec();
    let add = AddWorktree { project: luneta, folder: text(&clone.join("two")), name: None, argv };
    let start = |folder: &Path| Change::Start(Start { folder: text(folder), name: None });
    for change in [Change::AddWorktree(add), start(&plain), start(&elo.join("main"))] {
        let refused = changed(&owned.host, change).unwrap_err();
        assert_eq!(refused.code(), Code::Unavailable, "{refused:?}");
        let said = refused.message();
        assert!(said.contains("no terminal opens in worktree"), "{said}");
    }
    drop(stalled);
    let Held::Kept(Kept { projects, .. }) = held(&owned.host) else { panic!("projects are kept") };
    let ids: Vec<u64> = projects.iter().map(|it| it.id).collect();
    assert_eq!(ids, [luneta], "a project made with a worktree that did not open goes with it");
    let rows: Vec<u64> = projects[0].worktrees().unwrap().iter().map(|it| it.id).collect();
    assert_eq!(rows, [main], "no worktree stays with no view");
    let two = worktree(&owned.host, luneta, &clone.join("two"));
    assert_ne!(two, main, "and lunetad opens one again once the mux answers");
}

#[test]
fn a_projects_file_lunetad_cannot_read_is_left_as_it_is_and_no_change_is_kept() {
    let host = Host::new();
    let file = kept(&host);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let newer = "{\"version\": 10, \"projects\": [], \"views\": []}\n";
    fs::write(&file, newer).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let said = daemon.said_after(Duration::ZERO).join("\n");
    assert!(said.contains("of version 10, and this lunetad reads only 1 to 9"), "{said}");

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

        let main = worktree(keeper, a, &dir("a/main"));
        let one = worktree(keeper, a, &dir("a/one"));
        let two = worktree(keeper, a, &dir("a/two"));
        let swap = |seen: Vec<u64>| {
            let worktrees = vec![main, two, one];
            let swap = ReorderWorktrees { project: a, worktrees, seen };
            asked_on(&here, on, Change::ReorderWorktrees(swap))
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
        let worktrees = kept.projects.iter().flat_map(|it| it.worktrees().unwrap());
        worktrees.map(|it| (it.id, it.folder.clone())).collect::<Vec<_>>()
    };

    let Some(Made::Worktree(feat)) = start("luneta/feat") else {
        panic!("a start in a worktree of a project far keeps goes to it there");
    };
    let want = [(feat, text(&real.join("luneta/feat")))];
    assert_eq!(folders(), want, "only the one started in, of project {luneta}'s worktrees");
    assert_eq!(start("luneta/feat"), Some(Made::Found(feat)), "the same one again");
    assert_eq!(start("luneta"), Some(Made::Found(feat)), "its clone's root opens the first");
    worktree_added(&real.join("luneta"), "pr-7");

    let pull = Pull {
        holder: text(&link.join("elsewhere")),
        folder: text(&link.join("luneta/pr-7")),
        project: "cloned".to_owned(),
        name: "pr-7".to_owned(),
        argv: FIRST.map(str::to_owned).to_vec(),
    };
    let Some(Made::Worktree(pr) | Made::Found(pr)) = asked(Change::Pull(pull)) else {
        panic!("a pull goes to the worktree, made by it or by the sweep");
    };
    assert!(folders().contains(&(pr, text(&real.join("luneta/pr-7")))), "in project luneta");

    let Some(Made::Worktree(elo)) = start("elo") else {
        panic!("a start in no project makes one and goes to its worktree");
    };
    let Held::Kept(kept) = held(&far) else { panic!("far keeps its projects") };
    let held_in = |it: &&api::Project| it.worktrees().unwrap().iter().any(|it| it.id == elo);
    let made = kept.projects.iter().find(held_in).expect("far keeps elo");
    assert_eq!(made.dir, text(&real.join("elo")));
    let holds = Start { folder: text(&link), name: None };
    let refused = answered_on(&here, Some("far"), Change::Start(holds)).unwrap_err();
    assert_eq!(refused.code(), Code::AlreadyExists, "{refused:?}");
}
