use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use luneta_waits::{eventually, Span};
use lunetad::api::change_projects_request::{
    AddProject, AddWorktree, Change, CloseWorktree, Pull, ReorderProjects, ReorderWorktrees, Start,
};
use lunetad::api::change_projects_response::Made;
use lunetad::api::project::clone::Listing;
use lunetad::api::project::{self, Holds};
use lunetad::api::projects::{Held, Kept};
use lunetad::api::worktree::Seen;
use lunetad::api::{self, ChangeProjectsRequest, ChangeProjectsResponse as Answered, On, Stage};
use lunetad::api::make_worktree_response::Landed;
use lunetad::api::list_work_response::work::Issues;
use lunetad::api::list_work_response::{Issue, Listed as Said, OpenIssues, Pull as ListedPull, Work};
use lunetad::api::{ListWorkRequest, ListWorkResponse, MakeWorktreeRequest};
use lunetad::api::Version;
use lunetad::changes::Unchanged;
use lunetad::SWEEP;
use tonic::{Code, Status};

use crate::host::{folder_in, start, Host};
use crate::remote::{hosts, reached, FakeSsh};
use crate::worktree::{cloned_as_wt, git, worktree_added};

pub fn asked(host: &Host, change: Change) -> Result<Option<u64>, Status> {
    asked_on(host, None, change)
}

fn asked_on(host: &Host, on: Option<&str>, change: Change) -> Result<Option<u64>, Status> {
    let made = |made| match made {
        Made::Project(id) | Made::Worktree(id) | Made::Found(id) | Made::Taken(id) => id,
        Made::Untaken(()) => panic!("lunetad took luneta nowhere, and names no id"),
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

fn branched_on(
    host: &Host,
    on: Option<&str>,
    project: u64,
    branch: &str,
) -> Result<Option<Landed>, Status> {
    let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let on = on.map(|on| On { destination: on.to_owned() });
        let asked = MakeWorktreeRequest { on, project, branch: branch.to_owned() };
        let mut answer = daemon.make_worktree(asked).await?.into_inner();
        let made = answer.message().await?.expect("lunetad answers once");
        assert!(answer.message().await?.is_none(), "and only once");
        Ok(made.landed)
    })
}

fn work_on(host: &Host, on: Option<&str>, project: u64) -> Result<ListWorkResponse, Status> {
    let folder = folder_in(host.runtime.path()).expect("no daemon has made its folder");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let on = on.map(|on| On { destination: on.to_owned() });
        let mut answer = daemon.list_work(ListWorkRequest { on, project }).await?.into_inner();
        let listed = answer.message().await?.expect("lunetad answers once");
        assert!(answer.message().await?.is_none(), "and only once");
        Ok(listed)
    })
}

fn work_of(listed: ListWorkResponse) -> Work {
    match listed.listed {
        Some(Said::Work(work)) => work,
        listed => panic!("lunetad listed no work but {listed:?}"),
    }
}

const GITHUB: &str = "git@github.com:o/n.git";

fn on_github(folder: &Path) {
    git(folder.parent().unwrap(), &["init", "-q", "-b", "main", &folder.display().to_string()]);
    git(folder, &["remote", "add", "upstream", GITHUB]);
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
fn a_clone_s_worktree_is_named_for_its_folder_unless_its_branch_is_the_one_wt_makes_it_for() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, notes) = (scratch.join("up"), scratch.join("luneta"), scratch.join("notes"));
    cloned_as_wt(&scratch, &up, &clone);
    git(&clone.join(".bare"), &["worktree", "add", "-q", "-b", "fix/x", "../fix-x"]);
    git(&scratch, &["init", "-q", "-b", "main", &text(&notes)]);
    git(&notes, &["commit", "-q", "--allow-empty", "-m", "one"]);
    let _daemon = start(host.vars()).unwrap();
    let luneta = project(&host, &clone, None);
    worktree(&host, luneta, &clone.join("main"));
    worktree(&host, luneta, &clone.join("fix-x"));
    let folder = project(&host, &notes, None);
    worktree(&host, folder, &notes);
    let named = |want: [&str; 3]| {
        let Held::Kept(Kept { projects, .. }) = held(&host) else {
            panic!("the projects are kept");
        };
        let worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
        let names: Vec<&str> = worktrees.map(|it| it.name.as_str()).collect();
        (names == want).then_some(())
    };

    let branched = "each is named for the branch its folder is for";
    eventually(branched, || named(["main", "fix/x", "main"]));
    git(&clone.join("fix-x"), &["switch", "-q", "-c", "other"]);
    git(&notes, &["switch", "-q", "-c", "other"]);
    let switched = "the clone's is named for its folder, the folder's for its branch";
    eventually(switched, || named(["main", "fix-x", "other"]));
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
    let keeps = "a worktree removed keeps its row while it holds a terminal";
    let sweeps = const { Span::of(SWEEP.saturating_mul(2)) };
    luneta_waits::through(sweeps, keeps, || Some(listed(&host)).filter(|now| *now != opened));
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
    assert!(written.starts_with("{\n  \"version\": 10,"), "written at once: {written}");
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
    let branched = branched_on(&owned.host, None, luneta, "two").unwrap_err();
    assert_eq!(branched.code(), Code::Aborted, "{branched:?}");
    let said = branched.message();
    let two = text(&clone.join("two"));
    let unopened = format!("{two} is two's worktree, and it is not opened: ");
    assert!(said.starts_with(&unopened), "{said}");
    assert!(said.contains("no terminal opens in worktree"), "{said}");
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
    let newer = "{\"version\": 11, \"projects\": [], \"views\": []}\n";
    fs::write(&file, newer).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let mut daemon = start(host.vars()).unwrap();
    let unread = "of version 11, and this lunetad reads only 1 to 10";
    daemon.says("lunetad says it cannot read the file", |line| line.contains(unread));

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
    FakeSsh::new(here.bin()).reaches("far", far.vars());
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
    FakeSsh::new(here.bin()).reaches("far", far.vars());
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
    let close = Change::CloseWorktree(CloseWorktree { worktree: 1, seen: Vec::new() });
    let unsent = answered_on(&here, Some("nowhere"), close).unwrap_err();
    let unreached = Unchanged::Refused("lunetad does not reach nowhere now".to_owned());
    assert_eq!(Unchanged::of(&unsent), unreached, "a change no bridge sent is refused");
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
        folder: text(&link.join("luneta/pr-7")),
        name: "pr-7".to_owned(),
        argv: FIRST.map(str::to_owned).to_vec(),
    };
    let Some(Made::Worktree(pr) | Made::Found(pr)) = asked(Change::Pull(pull)) else {
        panic!("a pull goes to the worktree, made by it or by the sweep");
    };
    assert!(folders().contains(&(pr, text(&real.join("luneta/pr-7")))), "in project luneta");
    cloned_as_wt(&real, &real.join("up-cdk"), &real.join("cdk"));
    worktree_added(&real.join("cdk"), "fix");
    let made_for = Pull {
        folder: text(&link.join("cdk/fix")),
        name: "fix".to_owned(),
        argv: FIRST.map(str::to_owned).to_vec(),
    };
    let Some(Made::Worktree(fix) | Made::Found(fix)) = asked(Change::Pull(made_for)) else {
        panic!("a pull into a clone of no project goes to the worktree in one made for it");
    };
    let Held::Kept(kept) = held(&far) else { panic!("far keeps its projects") };
    let held_in = |it: &&api::Project| it.worktrees().unwrap().iter().any(|it| it.id == fix);
    let made = kept.projects.iter().find(held_in).expect("far keeps cdk/fix");
    assert_eq!((made.name.as_str(), made.dir.clone()), ("cdk", text(&real.join("cdk"))));

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

#[test]
fn a_branch_s_worktree_is_made_in_a_clone_on_its_machine_and_opens_with_a_shell() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (clone, vault) = (scratch.join("luneta"), scratch.join("Vault"));
    cloned_as_wt(&scratch, &scratch.join("up"), &clone);
    git(&clone.join(".bare"), &["update-ref", "refs/remotes/origin/main", "main"]);
    fs::create_dir_all(&vault).unwrap();
    let luneta = project(&far, &clone, None);
    let backup = project(&far, &vault, None);
    let opened = || {
        let Held::Kept(kept) = held(&far) else { panic!("far keeps its projects") };
        let luneta = kept.projects.into_iter().find(|it| it.id == luneta).unwrap();
        let worktrees = luneta.worktrees().unwrap().iter();
        let opened = worktrees.map(|it| (it.id, it.folder.clone(), it.terminals.len()));
        opened.collect::<Vec<_>>()
    };

    let branched = |project: u64, branch: &str| branched_on(&here, Some("far"), project, branch);
    let Some(Landed::Made(feat)) = branched(luneta, "feat").unwrap() else {
        panic!("a branch of the clone's origin gets a worktree made there");
    };
    assert!(clone.join("feat/.git").is_file(), "wt made the worktree on far");
    assert_eq!(opened(), [(feat, text(&clone.join("feat")), 1)], "with its one shell");
    let again = branched(luneta, " feat ").unwrap();
    assert_eq!(again, Some(Landed::Found(feat)), "the worktree that has the branch, again");
    let Some(Landed::Made(fresh)) = branched(luneta, "fresh").unwrap() else {
        panic!("a branch git has nowhere is made");
    };
    let fresh_at = text(&clone.join("fresh"));
    let head = fs::read_to_string(clone.join(".bare/worktrees/fresh/HEAD")).unwrap();
    assert_eq!(head, "ref: refs/heads/fresh\n", "on its own branch");
    assert!(opened().contains(&(fresh, fresh_at, 1)));

    let refused = branched(backup, "feat").unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert_eq!(refused.message(), "Vault is a folder, and its own is its one worktree");
    let refused = branched(luneta, "a..b").unwrap_err();
    assert!(refused.message().contains("a..b is not a valid branch name"), "{refused:?}");
    let refused = branched(luneta, "--help").unwrap_err();
    assert!(refused.message().contains("--help is not a valid branch name"), "{refused:?}");
    let refused = branched(luneta, "https://github.com/o/r/pull/3").unwrap_err();
    let pasted = "o/r#3 is a pull request: paste its link to make its worktree";
    assert_eq!((refused.code(), refused.message()), (Code::InvalidArgument, pasted));
    let refused = branched(luneta, "#12").unwrap_err();
    let numbered = "#12 names an issue or a pull request: paste its link";
    assert_eq!((refused.code(), refused.message()), (Code::InvalidArgument, numbered));
    let refused = branched(luneta, "  ").unwrap_err();
    assert_eq!((refused.code(), refused.message()), (Code::InvalidArgument, "it names no branch"));
    let refused = branched(0, "feat").unwrap_err();
    let unnamed = (Code::InvalidArgument, "project: no id is 0");
    assert_eq!((refused.code(), refused.message()), unnamed);
    let refused = branched(u64::MAX, "feat").unwrap_err();
    assert_eq!(refused.code(), Code::NotFound, "{refused:?}");
    assert_eq!(opened().len(), 2, "no refusal made a row");
}

#[test]
fn a_project_s_work_is_what_gh_lists_open_in_its_folder_on_its_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let _far = start(far.vars()).unwrap();
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (clone, vault) = (scratch.join("luneta"), scratch.join("Vault"));
    let (notes, down, off) = (scratch.join("notes"), scratch.join("down"), scratch.join("off"));
    cloned_as_wt(&scratch, &scratch.join("up"), &clone);
    git(&clone.join(".bare"), &["remote", "add", "upstream", GITHUB]);
    on_github(&vault);
    on_github(&down);
    on_github(&off);
    fs::create_dir_all(&notes).unwrap();
    let luneta = project(&far, &clone, None);
    let backup = project(&far, &vault, None);
    let notes_project = project(&far, &notes, None);
    let down_project = project(&far, &down, None);
    let off_project = project(&far, &off, None);
    let issues = serde_json::json!([
        {"number": 507, "title": "Open a project on a home view", "labels": [{"name": "enhancement"}]},
        {"number": 498, "title": "Menu says no project", "labels": []},
    ]);
    let pulls = serde_json::json!([
        {"number": 524, "title": "Say view, not pane", "isDraft": true, "headRefName": "issue-511",
         "author": {"login": "lorenzolfm"}},
        {"number": 526, "title": "Match each place once", "isDraft": false,
         "headRefName": "issue-523", "author": null},
    ]);
    far.gh.work(&issues, &pulls);

    let listed = work_of(work_on(&here, Some("far"), luneta).unwrap());
    let labels = vec!["enhancement".to_owned()];
    let issue = |number, title: &str, labels| Issue { number, title: title.to_owned(), labels };
    let open = vec![
        issue(507, "Open a project on a home view", labels),
        issue(498, "Menu says no project", vec![]),
    ];
    assert_eq!(listed.issues, Some(Issues::Open(OpenIssues { issues: open, more: false })));
    let pull = |number, title: &str, draft, branch: &str, author: Option<&str>| ListedPull {
        number,
        title: title.to_owned(),
        draft,
        branch: branch.to_owned(),
        author: author.map(str::to_owned),
    };
    assert_eq!(
        listed.pulls,
        [
            pull(524, "Say view, not pane", true, "issue-511", Some("lorenzolfm")),
            pull(526, "Match each place once", false, "issue-523", None),
        ]
    );
    assert!(!listed.more_pulls, "gh listed all that are open");
    let mut worked = far.gh.worked();
    worked.sort();
    let at = text(&clone);
    assert_eq!(
        worked,
        [
            format!("{at} issue list --state open --limit 51 --json number,title,labels"),
            format!(
                "{at} pr list --state open --limit 51 --json number,title,isDraft,headRefName,author"
            ),
        ],
        "gh lists the open ones and one past the most, in the clone's root, on far"
    );
    assert!(here.gh.worked().is_empty(), "and not where the asker is");

    let many: Vec<_> = (1..=51).map(|number| serde_json::json!({"number": number, "title": "t"})).collect();
    far.gh.work(&serde_json::Value::Array(many), &pulls);
    let listed = work_of(work_on(&far, None, backup).unwrap());
    let Some(Issues::Open(open)) = &listed.issues else {
        panic!("lunetad listed no open issues but {:?}", listed.issues);
    };
    let numbers: Vec<u32> = open.issues.iter().map(|issue| issue.number).collect();
    assert_eq!(numbers, (1..=50).collect::<Vec<_>>(), "a folder's work is listed in the folder");
    assert!(open.more && !listed.more_pulls, "and past 50 it says there are more");
    assert!(far.gh.worked().iter().any(|it| it.starts_with(&format!("{} ", text(&vault)))));

    let unhosted = work_on(&here, Some("far"), notes_project).unwrap();
    assert_eq!(unhosted.listed, Some(Said::Unhosted(())), "a folder with no remote on GitHub");
    let in_notes = format!("{} ", text(&notes));
    assert!(!far.gh.worked().iter().any(|it| it.starts_with(&in_notes)), "and gh is not run there");

    far.gh.issues_off(true);
    let listed = work_of(work_on(&here, Some("far"), off_project).unwrap());
    assert_eq!(listed.issues, Some(Issues::Off(())), "a repository with its issues off says so");
    assert_eq!(listed.pulls.len(), 2, "and lists its pull requests still");
    let viewed = format!("{} repo view --json hasIssuesEnabled", text(&off));
    assert!(far.gh.worked().contains(&viewed), "{:?}", far.gh.worked());
    far.gh.issues_off(false);

    far.gh.refuse_work("HTTP 502: Bad Gateway (https://api.github.com/graphql)");
    let refused = work_on(&here, Some("far"), down_project).unwrap_err();
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    let said = "gh issue list --state open: HTTP 502: Bad Gateway (https://api.github.com/graphql)";
    assert_eq!(refused.message(), said, "where the issues are on, a list gh refused is refused");
    let refused = work_on(&here, Some("far"), 0).unwrap_err();
    let unnamed = (Code::InvalidArgument, "project: no id is 0");
    assert_eq!((refused.code(), refused.message()), unnamed);
    let refused = work_on(&here, Some("far"), u64::MAX).unwrap_err();
    assert_eq!(refused.code(), Code::NotFound, "{refused:?}");
}

#[test]
fn a_folder_s_work_is_listed_once_for_all_who_ask_within_a_while() {
    let host = Host::new();
    let _lunetad = start(host.vars()).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let notes = fs::canonicalize(scratch.path()).unwrap().join("notes");
    on_github(&notes);
    let notes = project(&host, &notes, None);
    host.gh.slow_work();
    thread::scope(|scope| {
        let asks: Vec<_> = (0..3).map(|_| scope.spawn(|| work_on(&host, None, notes))).collect();
        for ask in asks {
            assert!(ask.join().unwrap().is_ok());
        }
    });
    assert_eq!(host.gh.worked().len(), 2, "one issue list and one pull request list for all three");
    work_on(&host, None, notes).unwrap();
    assert_eq!(host.gh.worked().len(), 2, "and the next ask is answered from what they listed");
}
