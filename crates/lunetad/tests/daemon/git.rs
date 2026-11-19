use std::collections::BTreeMap;
use std::fs::{self, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::change_projects_request::{Change, ForgetProject};
use lunetad::api::checkout::Head;
use lunetad::api::git_status::{NoCheckout, Stands};
use lunetad::api::projects::Held;
use lunetad::api::{self, Branch, Changes, Checkout, PullRequestState, Upstream, Version};
use prost::Message as _;
use serde_json::json;

use crate::host::{eventually, start, Host, LUNETA};
use crate::projects::{asked, project};

impl Host {
    fn watch(&self, after: Option<Version>) -> (api::Host, Duration) {
        let mut command = self.vars().command(LUNETA);
        command.arg("watch");
        if let Some(after) = after {
            command.arg(format!("--after={}:{}", after.pid, after.seq));
        }
        let asked = Instant::now();
        let output = command.stderr(Stdio::inherit()).output().unwrap();
        let took = asked.elapsed();
        assert!(output.status.success(), "luneta watch failed");
        (api::Host::decode(output.stdout.as_slice()).unwrap(), took)
    }
}

fn watched(host: &Host, dir: &str) -> (u64, String) {
    let id = project(host, Path::new(dir), None);
    let folder = eventually("lunetad keeps the project", || {
        let held = host.host().projects.and_then(|projects| projects.held);
        let Some(Held::Kept(kept)) = held else {
            return None;
        };
        Some(kept.projects.into_iter().find(|it| it.id == id)?.dir)
    });
    (id, folder)
}

fn forgotten(host: &Host, project: u64) {
    asked(host, Change::ForgetProject(ForgetProject { project })).unwrap();
}

fn git(dir: &Path, args: &[&str]) -> bool {
    let mut git = Command::new("git");
    for repository in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        git.env_remove(repository);
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(["-c", "maintenance.auto=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn commit(dir: &Path, file: &str, text: &str) {
    fs::write(dir.join(file), text).unwrap();
    assert!(git(dir, &["add", file]));
    assert!(git(dir, &["commit", "-q", "-m", file]));
}

fn changed_every_way(dir: &Path, origin: &Path) {
    assert!(git(origin, &["init", "-q", "--bare", "-b", "main"]));
    assert!(git(dir, &["init", "-q", "-b", "main"]));
    let files = ["stashed", "modified", "retyped", "staged", "deleted", "renamed", "conflicted"];
    for file in files {
        commit(dir, file, "start\n");
    }
    assert!(git(dir, &["remote", "add", "origin", &origin.display().to_string()]));
    assert!(git(dir, &["push", "-q", "-u", "origin", "main"]));
    fs::write(dir.join("stashed"), "kept for later\n").unwrap();
    assert!(git(dir, &["stash", "-q"]));
    assert!(git(dir, &["switch", "-q", "-c", "other"]));
    commit(dir, "conflicted", "theirs\n");
    assert!(git(dir, &["switch", "-q", "main"]));
    commit(dir, "conflicted", "ours\n");
    assert!(!git(dir, &["merge", "-q", "other"]), "the merge should go wrong");
    fs::write(dir.join("modified"), "changed\n").unwrap();
    fs::remove_file(dir.join("retyped")).unwrap();
    symlink("modified", dir.join("retyped")).unwrap();
    fs::write(dir.join("staged"), "changed\n").unwrap();
    assert!(git(dir, &["add", "staged"]));
    assert!(git(dir, &["rm", "-q", "deleted"]));
    assert!(git(dir, &["mv", "renamed", "moved"]));
    fs::write(dir.join("untracked"), "new\n").unwrap();
}

#[test]
fn git_says_how_each_project_s_folder_stands() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let (tree, plain, detached) =
        (dirs.path().join("tree"), dirs.path().join("plain"), dirs.path().join("detached"));
    let origin = dirs.path().join("origin.git");
    for dir in [&tree, &plain, &detached, &origin] {
        fs::create_dir(dir).unwrap();
    }
    changed_every_way(&tree, &origin);
    assert!(git(&detached, &["clone", "-q", &origin.display().to_string(), "."]));
    assert!(git(&detached, &["checkout", "-q", "--detach"]));

    let spelled = |dir: &PathBuf| dir.display().to_string();
    let (tree, plain, detached) = (spelled(&tree), spelled(&plain), spelled(&detached));
    let _daemon = start(host.vars()).unwrap();
    let watching = [&tree, &plain, &detached].map(|dir| watched(&host, dir).0);

    let statuses = eventually("git says how all three stand", || {
        let statuses = host.host().git_statuses;
        (statuses.len() == 3).then_some(statuses)
    });
    let stands =
        |dir: &str| statuses.iter().find(|status| status.dir == dir).unwrap().stands.clone();
    let every =
        Changes { conflicted: 1, deleted: 1, renamed: 1, modified: 2, staged: 1, untracked: 1 };
    let main = Branch {
        name: "main".to_owned(),
        upstream: Some(Upstream { ahead: 1, behind: 0 }),
        changed: None,
    };
    let checkout = |dir: &str, head, changes, stashes| {
        let repository = Some(fs::canonicalize(dir).unwrap().display().to_string());
        Some(Stands::Checkout(Checkout {
            head: Some(head),
            changes: Some(changes),
            stashes,
            repository,
        }))
    };
    assert_eq!(stands(&tree), checkout(&tree, Head::Branch(main), every, 1));
    let none = Changes::default();
    assert_eq!(stands(&detached), checkout(&detached, Head::Detached(()), none, 0));
    assert_eq!(stands(&plain), Some(Stands::Outside(())));

    forgotten(&host, watching[1]);
    eventually("what git said of plain goes", || {
        let statuses = host.host().git_statuses;
        statuses.iter().all(|status| status.dir != plain).then_some(())
    });
}

#[test]
fn git_counts_the_lines_changed_on_a_branch_against_the_default() {
    fn at(dir: &str) -> &Path {
        Path::new(dir)
    }
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let dirs = dirs.path().canonicalize().unwrap();
    let (origin, work, other) =
        (dirs.join("origin.git"), dirs.join("work"), dirs.join("other"));
    for dir in [&work, &other, &origin] {
        fs::create_dir(dir).unwrap();
    }
    assert!(git(&origin, &["init", "-q", "--bare", "-b", "main"]));
    assert!(git(&work, &["clone", "-q", "--origin", "origin", &origin.display().to_string(), "."]));
    commit(&work, "README", "start\n");
    assert!(git(&work, &["push", "-q", "origin", "main"]));
    assert!(git(&work, &["remote", "set-head", "origin", "main"]));
    let base = ["a\n", "b\n", "c\n"].concat();
    commit(&work, "base", &base);
    assert!(git(&work, &["push", "-q", "origin", "main"]));
    assert!(git(&work, &["switch", "-q", "-c", "issue-299"]));
    commit(&work, "base", "a\nB\n\n");
    fs::write(work.join("new"), "d\n").unwrap();
    assert!(git(&work, &["add", "-N", "new"]));
    assert!(git(
        &other,
        &["clone", "-q", "--origin", "origin", &origin.display().to_string(), "."],
    ));
    assert!(git(&other, &["remote", "set-head", "origin", "main"]));

    let spelled = |dir: &PathBuf| dir.display().to_string();
    let (work, other) = (spelled(&work), spelled(&other));
    let mut daemon = start(host.vars()).unwrap();
    for dir in [&work, &other] {
        watched(&host, dir);
    }

    let changed = |dir: &str| {
        host.host().git_statuses.iter().find(|status| status.dir == dir).and_then(|status| {
            match status.stands.as_ref()? {
                Stands::Checkout(Checkout { head: Some(Head::Branch(branch)), .. }) => {
                    let changed = branch.changed.as_ref()?;
                    Some((changed.added, changed.deleted))
                },
                _ => None,
            }
        })
    };
    eventually("git counts the lines changed on issue-299", || {
        (changed(&work) == Some((3, 2))).then_some(())
    });
    let said = daemon.said_after(Duration::from_secs(1)).to_vec();
    assert!(!said.iter().any(|line| line.contains("git ")), "git said: {said:?}");
    eventually("git counts none on main", || changed(&other).is_none().then_some(()));

    assert!(git(at(&other), &["switch", "-q", "-c", "fold"]));
    commit(at(&other), "new", "d\n");
    eventually("git counts the lines changed on fold", || {
        (changed(&other) == Some((1, 0))).then_some(())
    });
    assert!(git(at(&other), &["switch", "-q", "main"]));
    eventually("git counts none on main", || changed(&other).is_none().then_some(()));
}

#[test]
fn what_git_said_stands_once_it_will_not_read_the_repository() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    assert!(git(&dir, &["init", "-q", "-b", "main"]));
    commit(&dir, "file", "text\n");
    let cwd = dir.display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let before = eventually("git says how it stands", || checkout(&host, &cwd));
    assert_eq!(before.repository.as_deref(), Some(cwd.as_str()));

    assert!(git(&dir, &["config", "core.repositoryformatversion", "99"]));
    let why = format!("git did not say what repository {cwd} is in");
    eventually("git will not read it", || {
        let logged = daemon.said_after(Duration::from_millis(50));
        logged.iter().any(|line| line.contains(&why)).then_some(())
    });
    assert_eq!(checkout(&host, &cwd), Some(before));
}

fn checkout(host: &Host, cwd: &str) -> Option<Checkout> {
    let status = host.host().git_statuses.into_iter().find(|status| status.dir == cwd)?;
    match status.stands? {
        Stands::Checkout(checkout) => Some(checkout),
        Stands::Outside(()) | Stands::NoCheckout(_) => None,
    }
}

fn in_no_repository(host: &Host, cwd: &str) {
    eventually("git says the directory is in no repository", || {
        let status = host.host().git_statuses.into_iter().find(|status| status.dir == cwd)?;
        (status.stands == Some(Stands::Outside(()))).then_some(())
    });
}

fn on_branch(name: &'static str) -> impl Fn(&Checkout) -> bool {
    move |it| matches!(&it.head, Some(Head::Branch(branch)) if branch.name == name)
}

fn shows_at_once(
    host: &Host,
    cwd: &str,
    what: &str,
    change: impl FnOnce(),
    shows: impl Fn(&Checkout) -> bool,
) {
    let started = Instant::now();
    change();
    eventually(what, || checkout(host, cwd).filter(|it| shows(it)));
    assert!(started.elapsed() < Duration::from_secs(4), "{what}: {:?}", started.elapsed());
}

fn says_at_once_as_a_checkout_changes(format: &str) {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let path = |name: &str| dirs.path().join(name);
    let (origin, repo, elsewhere) = (path("origin.git"), path("repo"), path("elsewhere"));
    let (tree, beside) = (path("tree"), path("beside"));
    for dir in [&origin, &repo] {
        fs::create_dir(dir).unwrap();
    }
    let format = format!("--ref-format={format}");
    assert!(git(&origin, &["init", "-q", "--bare", "-b", "main", &format]));
    assert!(git(&repo, &["init", "-q", "-b", "main", &format]));
    fs::create_dir(repo.join("src")).unwrap();
    commit(&repo, "src/lib.rs", "start\n");
    let spelled = |dir: &Path| dir.display().to_string();
    assert!(git(&repo, &["remote", "add", "origin", &spelled(&origin)]));
    assert!(git(&repo, &["push", "-q", "-u", "origin", "main"]));
    assert!(git(&repo, &["worktree", "add", "-q", "-b", "work", &spelled(&tree), "origin/main"]));
    assert!(git(&repo, &["worktree", "add", "-q", "--detach", &spelled(&beside)]));
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&origin), "elsewhere"]));
    let _daemon = start(host.vars()).unwrap();
    let (_, src) = watched(&host, &spelled(&tree.join("src")));
    assert_eq!(src, spelled(&tree), "a folder in a checkout is its checkout's project");
    eventually("git says how the tree stands", || checkout(&host, &src));

    let edit = |file: &str, text: &str| fs::write(tree.join(file), text).unwrap();
    let in_tree = |args: &[&str]| assert!(git(&tree, args));
    let changes = |it: &Checkout| it.changes.unwrap_or_default();
    let branch = |it: &Checkout| match &it.head {
        Some(Head::Branch(branch)) => Some(branch.clone()),
        _ => None,
    };
    let upstream = |it: &Checkout| branch(it)?.upstream.map(|it| (it.ahead, it.behind));
    let clean = |it: &Checkout| changes(it) == Changes::default();
    let (host, src) = (&host, src.as_str());

    let edited = || edit("src/lib.rs", "edited\n");
    shows_at_once(host, src, "an edit in src", edited, |it| changes(it).modified == 1);
    let made = || {
        fs::create_dir(tree.join("src/new")).unwrap();
        edit("src/new/mod.rs", "new\n");
    };
    shows_at_once(host, src, "a directory new in src", made, |it| changes(it).untracked == 1);
    let staged = |it: &Checkout| changes(it).staged == 1 && changes(it).untracked == 0;
    shows_at_once(host, src, "it staged", || in_tree(&["add", "src/new"]), staged);
    let committed = |it: &Checkout| clean(it) && upstream(it) == Some((1, 0));
    shows_at_once(host, src, "a commit", || in_tree(&["commit", "-q", "-am", "work"]), committed);
    let modified = |it: &Checkout| changes(it).modified == 1;
    let edited = || edit("src/new/mod.rs", "edited\n");
    shows_at_once(host, src, "an edit in the directory committed", edited, modified);
    let made_anew = || {
        fs::remove_dir_all(tree.join("src/new")).unwrap();
        made();
    };
    shows_at_once(host, src, "the directory made anew", made_anew, clean);
    shows_at_once(host, src, "an edit in the one made anew", edited, modified);
    let fetched = || {
        commit(&elsewhere, "README", "start\n");
        assert!(git(&elsewhere, &["push", "-q"]));
        assert!(git(&beside, &["fetch", "-q"]));
    };
    let behind = |it: &Checkout| upstream(it) == Some((1, 1));
    shows_at_once(host, src, "a fetch in another work tree", fetched, behind);
    let stashed = |count: u32| move |it: &Checkout| clean(it) && it.stashes == count;
    shows_at_once(host, src, "a stash", || in_tree(&["stash", "-q"]), stashed(1));
    let stash = || {
        edit("src/lib.rs", "kept\n");
        in_tree(&["stash", "-q"]);
    };
    shows_at_once(host, src, "another stash", stash, stashed(2));
    let dropped = || in_tree(&["stash", "drop", "-q", "stash@{1}"]);
    shows_at_once(host, src, "the first stash dropped", dropped, stashed(1));
    let other = |it: &Checkout| branch(it).is_some_and(|branch| branch.name == "other");
    shows_at_once(host, src, "a checkout", || in_tree(&["switch", "-q", "-c", "other"]), other);
}

#[test]
fn git_says_at_once_as_a_checkout_changes_that_keeps_its_refs_in_files() {
    says_at_once_as_a_checkout_changes("files");
}

#[test]
fn git_says_at_once_as_a_checkout_changes_that_keeps_its_refs_in_a_reftable() {
    says_at_once_as_a_checkout_changes("reftable");
}

#[test]
fn git_says_at_once_as_a_repository_made_in_another_changes() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    let (top, src) = (tree.path(), tree.path().join("src"));
    fs::create_dir(&src).unwrap();
    let cwd = src.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    assert_eq!(watched(&host, &cwd).1, cwd, "src is in no repository yet");
    assert!(git(top, &["init", "-q", "-b", "main"]));
    commit(top, "src/lib.rs", "start\n");
    eventually("git says how the tree stands", || checkout(&host, &cwd));

    let made = || assert!(git(&src, &["init", "-q", "-b", "inner"]));
    shows_at_once(&host, &cwd, "a work tree made in src", made, on_branch("inner"));
    let switched = || assert!(git(&src, &["switch", "-q", "-c", "other"]));
    shows_at_once(&host, &cwd, "a branch checked out in it", switched, on_branch("other"));
}

#[test]
fn a_git_folder_that_does_not_read_leaves_the_checkout_it_is_in_watched() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    let (top, src) = (tree.path(), tree.path().join("src"));
    fs::create_dir(&src).unwrap();
    let cwd = src.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    assert_eq!(watched(&host, &cwd).1, cwd, "src is in no repository yet");
    assert!(git(top, &["init", "-q", "-b", "main"]));
    commit(top, "src/lib.rs", "start\n");
    let unread = src.join(".git");
    let mode = |mode| fs::set_permissions(&unread, Permissions::from_mode(mode)).unwrap();
    fs::create_dir(&unread).unwrap();
    mode(0o000);
    eventually("git says how the tree stands", || checkout(&host, &cwd));

    let modified = |it: &Checkout| it.changes.unwrap_or_default().modified == 1;
    let edited = || fs::write(src.join("lib.rs"), "edited\n").unwrap();
    shows_at_once(&host, &cwd, "an edit beside it", edited, modified);
    mode(0o755);
}

#[test]
fn git_says_at_once_as_a_repository_begins_where_there_was_none() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    in_no_repository(&host, &cwd);

    let made = || assert!(git(dir.path(), &["init", "-q", "-b", "first"]));
    let first = on_branch("first");
    shows_at_once(&host, &cwd, "a work tree made where there was none", made, first);
    let switched = || assert!(git(dir.path(), &["switch", "-q", "-c", "other"]));
    shows_at_once(&host, &cwd, "a branch checked out in it", switched, on_branch("other"));
}

#[test]
fn git_says_at_once_as_a_repository_begins_in_a_directory_made_again() {
    let host = Host::new();
    let noted = host.note_git();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    in_no_repository(&host, &cwd);

    let (asked, started) = (noted.statuses(), Instant::now());
    fs::remove_dir(&work).unwrap();
    fs::create_dir(&work).unwrap();
    eventually("git is asked again", || (noted.statuses() > asked).then_some(()));
    assert!(started.elapsed() < Duration::from_secs(4), "asked again: {:?}", started.elapsed());
    let made = || assert!(git(&work, &["init", "-q", "-b", "first"]));
    shows_at_once(&host, &cwd, "a work tree made in it", made, on_branch("first"));
}

#[test]
fn a_checkout_taken_away_is_said_of_no_more() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    fs::create_dir(&work).unwrap();
    assert!(git(&work, &["init", "-q", "-b", "main"]));
    commit(&work, "file", "text\n");
    let cwd = work.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    eventually("git says it is a checkout", || checkout(&host, &cwd));

    fs::remove_dir_all(&work).unwrap();
    eventually("git says nothing of it", || {
        let statuses = host.host().git_statuses;
        statuses.iter().all(|status| status.dir != cwd).then_some(())
    });
}

#[test]
fn a_repository_folder_has_git_asked_nothing_as_git_writes_there() {
    let host = Host::new();
    let noted = host.note_git();
    let tree = tempfile::tempdir().unwrap();
    let top = tree.path();
    assert!(git(top, &["init", "-q", "-b", "main"]));
    let cwd = top.join(".git").display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let repository = fs::canonicalize(top).unwrap().display().to_string();
    let of_its_clone = Some(Stands::NoCheckout(NoCheckout { repository }));
    eventually("git says it is no checkout of its clone", || {
        let status = host.host().git_statuses.into_iter().find(|status| status.dir == cwd)?;
        (status.stands == of_its_clone).then_some(())
    });

    let asked = noted.statuses();
    for step in 0..5 {
        commit(top, "lib.rs", &format!("{step}\n"));
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(noted.statuses(), asked, "git was asked as git wrote in its folder");
}

#[test]
fn git_says_at_once_as_a_directory_moved_into_a_checkout_changes() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let path = |name: &str| dirs.path().join(name);
    let top = path("repo");
    fs::create_dir_all(top.join("src")).unwrap();
    assert!(git(&top, &["init", "-q", "-b", "main"]));
    commit(&top, "src/lib.rs", "start\n");
    let cwd = top.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    eventually("git says how the tree stands", || checkout(&host, &cwd));

    let changes = |it: &Checkout| it.changes.unwrap_or_default();
    let moved_in = || {
        fs::create_dir(path("came")).unwrap();
        fs::write(path("came/lib.rs"), "came\n").unwrap();
        fs::rename(top.join("src"), path("went")).unwrap();
        fs::rename(path("came"), top.join("src")).unwrap();
    };
    let modified = |it: &Checkout| changes(it).modified == 1;
    shows_at_once(&host, &cwd, "a directory moved in", moved_in, modified);
    let edited = || fs::write(top.join("src/lib.rs"), "start\n").unwrap();
    let clean = |it: &Checkout| changes(it) == Changes::default();
    shows_at_once(&host, &cwd, "an edit in the one moved in", edited, clean);
}

#[test]
fn a_build_in_what_git_ignores_has_git_asked_nothing() {
    let host = Host::new();
    let noted = host.note_git();
    let tree = tempfile::tempdir().unwrap();
    let top = tree.path();
    assert!(git(top, &["init", "-q", "-b", "main"]));
    commit(top, ".gitignore", "/target/\n");
    let build = top.join("target/debug");
    fs::create_dir_all(&build).unwrap();
    symlink(top.join("target"), top.join(".git/refs/target")).unwrap();
    let cwd = top.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let untracked = |count| {
        let changes = |it: &Checkout| it.changes.unwrap_or_default();
        move |it: &Checkout| changes(it).untracked == count
    };
    eventually("git says how the tree stands", || checkout(&host, &cwd).filter(untracked(0)));

    let turns_up = |name: &'static str| move || fs::write(top.join(name), "").unwrap();
    shows_at_once(&host, &cwd, "a file that turns up", turns_up("one"), untracked(1));
    let asked = noted.statuses();
    for step in 0..20 {
        let made = build.join(format!("step{step}"));
        fs::create_dir(&made).unwrap();
        fs::write(made.join("out"), "built\n").unwrap();
        fs::write(build.join("log"), format!("{step}\n")).unwrap();
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(noted.statuses(), asked, "git was asked as the build wrote");
    shows_at_once(&host, &cwd, "another that turns up", turns_up("two"), untracked(2));
}

#[test]
fn git_runs_no_command_a_repository_names() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let (top, ran) = (dirs.path().join("tree"), dirs.path().join("ran"));
    fs::create_dir(&top).unwrap();
    assert!(git(&top, &["init", "-q", "-b", "main"]));
    commit(&top, "file", "start\n");
    let named = format!("touch '{}'", ran.display());
    assert!(git(&top, &["config", "core.fsmonitor", &named]));
    let cwd = top.display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    eventually("git says how the tree stands", || checkout(&host, &cwd));
    assert!(!ran.exists(), "git ran the command the repository named");
    assert!(git(&top, &["ls-files"]));
    assert!(ran.exists(), "git runs no command the repository names here");
}

#[test]
fn the_behind_count_is_of_what_the_remote_has() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let path = |name: &str| dirs.path().join(name);
    let (origin, tree, elsewhere, ran) =
        (path("origin.git"), path("tree"), path("elsewhere"), path("ran"));
    let (lost, lone, merged) = (path("lost.git"), path("lone"), path("merged"));
    fs::create_dir(&origin).unwrap();
    assert!(git(&origin, &["init", "-q", "--bare", "-b", "main"]));
    let spelled = |dir: &Path| dir.display().to_string();
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&origin), "elsewhere"]));
    commit(&elsewhere, "README", "start\n");
    assert!(git(&elsewhere, &["push", "-q", "-u", "origin", "main"]));
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&origin), "tree"]));
    assert!(git(dirs.path(), &["clone", "-q", "--bare", &spelled(&origin), "lost.git"]));
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&lost), "lone"]));
    fs::remove_dir_all(&lost).unwrap();
    for file in ["one", "two", "three"] {
        commit(&elsewhere, file, "new\n");
    }
    assert!(git(&elsewhere, &["push", "-q"]));
    assert!(git(&elsewhere, &["push", "-q", "origin", "HEAD:refs/heads/done"]));
    assert!(git(dirs.path(), &["clone", "-q", "-b", "done", &spelled(&origin), "merged"]));
    assert!(git(&elsewhere, &["push", "-q", "origin", "--delete", "done"]));
    let hook = tree.join(".git/hooks/reference-transaction");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", ran.display())).unwrap();
    fs::set_permissions(&hook, Permissions::from_mode(0o755)).unwrap();
    let here = fs::read_to_string(tree.join(".git/refs/heads/main")).unwrap();

    let (cwd, alone, done) = (spelled(&tree), spelled(&lone), spelled(&merged));
    let _daemon = start(host.vars()).unwrap();
    for dir in [&cwd, &alone, &done] {
        watched(&host, dir);
    }
    let upstream = |it: Checkout| match it.head {
        Some(Head::Branch(branch)) => branch.upstream.map(|it| (it.ahead, it.behind)),
        _ => None,
    };
    eventually("the branch is three behind", || {
        checkout(&host, &cwd).and_then(upstream).filter(|it| *it == (0, 3))
    });
    assert_eq!(fs::read_to_string(tree.join(".git/refs/heads/main")).unwrap(), here);
    assert!(!ran.exists(), "the fetch ran the hook the repository names");
    assert!(git(&tree, &["update-ref", "refs/heads/moved", "HEAD"]));
    assert!(ran.exists(), "git runs no hook the repository names here");

    let log = host.home.path().join(".local/state/luneta/luneta.log");
    let failed = format!("git did not fetch the upstream of main in {alone}");
    let gone = "the remote no longer has the upstream of done in ";
    eventually("the log says the fetches failed and the branch is gone", || {
        fs::read_to_string(&log).ok().filter(|log| log.contains(&failed) && log.contains(gone))
    });
    thread::sleep(Duration::from_secs(1));
    let said = fs::read_to_string(&log).unwrap();
    assert_eq!(said.matches(&failed).count(), 1, "{said}");
    assert_eq!(said.matches(gone).count(), 1, "{said}");
    assert!(!said.contains("git did not fetch the upstream of done"), "{said}");
}

#[test]
fn the_fetch_runs_no_command_a_repository_names() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let path = |name: &str| dirs.path().join(name);
    let (origin, tree, elsewhere, ran) =
        (path("origin.git"), path("tree"), path("elsewhere"), path("ran"));
    fs::create_dir(&origin).unwrap();
    assert!(git(&origin, &["init", "-q", "--bare", "-b", "main"]));
    let spelled = |dir: &Path| dir.display().to_string();
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&origin), "elsewhere"]));
    commit(&elsewhere, "README", "start\n");
    assert!(git(&elsewhere, &["push", "-q", "-u", "origin", "main"]));
    assert!(git(dirs.path(), &["clone", "-q", &spelled(&origin), "tree"]));
    assert!(git(&tree, &["config", "remote.origin.url", "ssh://nowhere.invalid/x"]));
    let named = format!("touch '{}'; :", ran.display());
    assert!(git(&tree, &["config", "core.sshCommand", &named]));

    let cwd = spelled(&tree);
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let log = host.home.path().join(".local/state/luneta/luneta.log");
    let refused = format!("git did not fetch the upstream of main in {cwd}");
    let why = |line: &str| line.contains(&refused) && line.contains("core.sshcommand");
    eventually("the log says why the branch was not fetched", || {
        fs::read_to_string(&log).ok().filter(|log| log.lines().any(why))
    });
    assert!(!ran.exists(), "the fetch ran the command the repository named");
    let said = fs::read_to_string(&log).unwrap();
    assert_eq!(said.matches(&refused).count(), 1, "{said}");
    let _ = git(&tree, &["ls-remote", "origin"]);
    assert!(ran.exists(), "git runs no command the repository names here");
}

#[test]
fn gh_is_asked_once_for_the_pull_request_of_each_branch() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let (tree, twin, other) =
        (dirs.path().join("tree"), dirs.path().join("twin"), dirs.path().join("other"));
    fs::create_dir(&tree).unwrap();
    fs::create_dir(&other).unwrap();
    for dir in [&tree, &other] {
        assert!(git(dir, &["init", "-q", "-b", "main"]));
        commit(dir, "README", "start\n");
    }
    assert!(git(&tree, &["worktree", "add", "-q", "-f", &twin.display().to_string(), "main"]));
    let page = |number| format!("https://github.com/owner/repo/pull/{number}");
    let run = |conclusion: &str| json!({
        "__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": conclusion,
    });
    let rollup = json!([run("FAILURE"), run("SUCCESS"), { "__typename": "StatusContext",
        "context": "ci", "state": "PENDING" }]);
    let main = json!({
        "number": 7, "state": "OPEN", "isDraft": false, "url": page(7),
        "statusCheckRollup": rollup,
    });
    host.gh.open("main", &main);
    host.gh.open("fold", &json!({ "number": 8, "state": "OPEN", "isDraft": true, "url": page(8) }));
    let spelled = |dir: &Path| dir.display().to_string();
    let (top, src, beside) = (spelled(&tree), spelled(&twin), spelled(&other));
    let mut daemon = start(host.vars()).unwrap();
    for dir in [&top, &src, &beside] {
        watched(&host, dir);
    }
    let found = || -> BTreeMap<String, (u32, PullRequestState, String)> {
        let pull_requests = host.host().pull_requests.into_iter();
        pull_requests.map(|it| (it.dir.clone(), (it.number, it.state(), it.url))).collect()
    };

    let open = (7, PullRequestState::Open, page(7));
    let all = eventually("gh finds main's pull request in all three", || {
        let found = found();
        (found.len() == 3).then_some(found)
    });
    let wanted = [top.clone(), src.clone(), beside.clone()].map(|dir| (dir, open.clone()));
    assert_eq!(all, BTreeMap::from(wanted));
    assert_eq!(host.gh.asked(), ["main", "main"]);
    let checks = host.host().pull_requests.into_iter().map(|it| it.checks).collect::<Vec<_>>();
    let failing = Some(api::Checks { pending: 1, failed: 1, passed: 1 });
    assert_eq!(checks, vec![failing; 3]);

    assert!(git(&tree, &["switch", "-q", "-c", "fold"]));
    let draft = (8, PullRequestState::Draft, page(8));
    eventually("gh finds fold's pull request in tree", || {
        (found().get(&top) == Some(&draft)).then_some(())
    });
    assert_eq!((found().get(&src), found().get(&beside)), (Some(&open), Some(&open)));
    assert_eq!(host.gh.asked(), ["main", "main", "fold"]);

    assert!(git(&other, &["switch", "-q", "-c", "bare"]));
    eventually("main's pull request is no longer other's", || {
        (!found().contains_key(&beside)).then_some(())
    });
    eventually("gh is asked about bare", || {
        host.gh.asked().iter().any(|branch| branch == "bare").then_some(())
    });
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh pr view")), "{said:?}");
    assert!(!found().contains_key(&beside));
    assert_eq!(host.gh.asked(), ["main", "main", "fold", "bare"]);
}

#[test]
fn a_branch_gh_hangs_on_holds_up_no_other() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let (stuck, other) = (dirs.path().join("a"), dirs.path().join("b"));
    for (dir, branch) in [(&stuck, "stuck"), (&other, "main")] {
        fs::create_dir(dir).unwrap();
        assert!(git(dir, &["init", "-q", "-b", branch]));
        commit(dir, "README", "start\n");
    }
    host.gh.hang("stuck");
    let page = "https://github.com/owner/repo/pull/7";
    host.gh.open("main", &json!({ "number": 7, "state": "OPEN", "isDraft": false, "url": page }));
    let (stuck, other) = (stuck.display().to_string(), other.display().to_string());
    let _daemon = start(host.vars()).unwrap();
    for dir in [&stuck, &other] {
        watched(&host, dir);
    }

    eventually("gh is asked about stuck", || {
        host.gh.asked().iter().any(|branch| branch == "stuck").then_some(())
    });
    eventually("main's pull request shows as gh hangs on stuck", || {
        let pull_requests = host.host().pull_requests;
        pull_requests.iter().any(|it| it.dir == other && it.number == 7).then_some(())
    });
    host.gh.answer("stuck");
}

#[test]
fn pending_checks_are_asked_about_again_soon_and_stand_stale_where_gh_fails() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    assert!(git(tree.path(), &["init", "-q", "-b", "main"]));
    commit(tree.path(), "README", "start\n");
    let page = "https://github.com/owner/repo/pull/7";
    let checked = |state: &str| {
        let rollup = json!([{ "__typename": "StatusContext", "context": "ci", "state": state }]);
        let mut pull = json!({ "number": 7, "state": "OPEN", "isDraft": false, "url": page });
        pull["statusCheckRollup"] = rollup;
        pull
    };
    host.gh.open("main", &checked("PENDING"));
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let shown = || {
        let pull_requests = host.host().pull_requests;
        pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| (it.checks, it.stale))
    };
    let soon = |what: &str, wanted| {
        let deadline = Instant::now() + Duration::from_secs(40);
        while shown() != Some(wanted) {
            assert!(Instant::now() < deadline, "never came to be: {what}");
            thread::sleep(Duration::from_millis(50));
        }
    };
    let pending = Some(api::Checks { pending: 1, failed: 0, passed: 0 });
    eventually("main's checks show pending", || (shown() == Some((pending, false))).then_some(()));

    host.gh.fail("main");
    soon("they stand stale as gh fails", (pending, true));

    host.gh.open("main", &checked("SUCCESS"));
    host.gh.mend("main");
    let passed = Some(api::Checks { pending: 0, failed: 0, passed: 1 });
    soon("they pass as gh answers again", (passed, false));

    let said = daemon.said_after(Duration::from_secs(1));
    let failed = format!("gh pr view in {cwd}: HTTP 401: Requires authentication");
    assert!(said.iter().any(|line| line.contains(&failed)), "{said:?}");
    let changed = format!("the checks of {page} went from pending to passing, ");
    let once = "failing once in between";
    let changed = |line: &&String| line.contains(&changed) && line.ends_with(once);
    assert_eq!(said.iter().filter(changed).count(), 1, "{said:?}");
    assert_eq!(host.gh.asked(), ["main"; 3]);
}

const PARENT: &str = "github.com/upstream/repo";

fn forked_tree(dir: &Path) {
    assert!(git(dir, &["init", "-q", "-b", "main"]));
    assert!(git(dir, &["remote", "add", "origin", "https://github.com/owner/repo.git"]));
    commit(dir, "README", "start\n");
}

fn tracks(dir: &Path, branch: &str, remote: &str, head: &str) {
    assert!(git(dir, &["config", &format!("branch.{branch}.remote"), remote]));
    assert!(git(dir, &["config", &format!("branch.{branch}.merge"), &format!("refs/heads/{head}")]));
}

fn pushes_to_origin(dir: &Path, branch: &str) {
    tracks(dir, branch, "origin", branch);
}

fn pull(number: u32, state: &str) -> serde_json::Value {
    let url = format!("https://github.com/upstream/repo/pull/{number}");
    json!({ "number": number, "state": state, "isDraft": false, "url": url })
}

#[test]
fn only_a_branch_git_names_a_head_of_is_asked_about_in_a_fork_s_parent() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    assert!(git(tree.path(), &["remote", "add", "upstream", "https://github.com/upstream/repo.git"]));
    let unnamed = ["current", "from_parent", "from_local", "merge_only"];
    for branch in unnamed.into_iter().chain(["tracked", "pushing", "defaulted"]) {
        assert!(git(tree.path(), &["branch", branch]));
    }
    tracks(tree.path(), "from_parent", "upstream", "main");
    tracks(tree.path(), "from_local", ".", "main");
    assert!(git(tree.path(), &["config", "branch.merge_only.merge", "refs/heads/merge_only"]));
    host.gh.fork_of("upstream", "repo");
    let branches = [
        "main",
        "current",
        "from_parent",
        "from_local",
        "merge_only",
        "tracked",
        "pushing",
        "defaulted",
    ];
    for (number, branch) in (1..).zip(branches) {
        host.gh.open_in(PARENT, branch, &pull(number, "OPEN"));
    }
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };
    let asked_of = |branch: &str| host.gh.asked().iter().any(|it| it == branch).then_some(());

    eventually("gh asks about main", || asked_of("main"));

    assert!(git(tree.path(), &["config", "push.default", "current"]));
    assert!(git(tree.path(), &["switch", "-q", "current"]));
    eventually("gh asks about current", || asked_of("current"));

    assert!(git(tree.path(), &["switch", "-q", "from_parent"]));
    eventually("gh asks about from_parent", || asked_of("from_parent"));

    assert!(git(tree.path(), &["switch", "-q", "from_local"]));
    eventually("gh asks about from_local", || asked_of("from_local"));

    assert!(git(tree.path(), &["switch", "-q", "merge_only"]));
    eventually("gh asks about merge_only", || asked_of("merge_only"));

    pushes_to_origin(tree.path(), "tracked");
    assert!(git(tree.path(), &["switch", "-q", "tracked"]));
    eventually("the parent's pull request is tracked's", || (found() == Some(6)).then_some(()));

    assert!(git(tree.path(), &["config", "branch.pushing.pushRemote", "origin"]));
    assert!(git(tree.path(), &["switch", "-q", "pushing"]));
    eventually("the parent's pull request is pushing's", || (found() == Some(7)).then_some(()));

    assert!(git(tree.path(), &["config", "remote.pushDefault", "origin"]));
    assert!(git(tree.path(), &["switch", "-q", "defaulted"]));
    eventually("the parent's pull request is defaulted's", || (found() == Some(8)).then_some(()));

    let in_parent = |branch: &str| format!("{PARENT} {branch}");
    assert_eq!(
        host.gh.asked(),
        [
            "main",
            "current",
            "from_parent",
            "from_local",
            "merge_only",
            "tracked",
            &in_parent("tracked"),
            "pushing",
            &in_parent("pushing"),
            "defaulted",
            &in_parent("defaulted"),
        ]
    );
    assert_eq!(host.gh.repositories(), 1);
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ")), "{said:?}");
}

#[test]
fn a_closed_pull_request_in_a_fork_gives_way_to_its_parent_s() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    for branch in ["merged", "alone"] {
        assert!(git(tree.path(), &["branch", branch]));
    }
    for branch in ["main", "merged", "alone"] {
        pushes_to_origin(tree.path(), branch);
    }
    host.gh.fork_of("upstream", "repo");
    host.gh.open("main", &pull(1, "CLOSED"));
    host.gh.open_in(PARENT, "main", &pull(2, "OPEN"));
    host.gh.open("merged", &pull(3, "MERGED"));
    host.gh.open_in(PARENT, "merged", &pull(4, "OPEN"));
    host.gh.open("alone", &pull(5, "CLOSED"));
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };

    eventually("the parent's open one is main's", || (found() == Some(2)).then_some(()));

    assert!(git(tree.path(), &["switch", "-q", "merged"]));
    eventually("the fork's merged one stands", || (found() == Some(3)).then_some(()));

    assert!(git(tree.path(), &["switch", "-q", "alone"]));
    eventually("the fork's closed one stands alone", || (found() == Some(5)).then_some(()));

    let in_parent = |branch: &str| format!("{PARENT} {branch}");
    assert_eq!(
        host.gh.asked(),
        ["main", &in_parent("main"), "merged", "alone", &in_parent("alone")]
    );
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ")), "{said:?}");
}

#[test]
fn a_fork_s_default_branch_shows_no_merged_pull_request_of_its_parent() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    assert!(git(tree.path(), &["branch", "trunk"]));
    for branch in ["main", "trunk"] {
        pushes_to_origin(tree.path(), branch);
    }
    host.gh.fork_of_defaulting_to("upstream", "repo", "trunk");
    host.gh.open_in(PARENT, "main", &pull(1, "MERGED"));
    host.gh.open_in(PARENT, "trunk", &pull(2, "MERGED"));
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };

    eventually("another branch's merged one shows", || (found() == Some(1)).then_some(()));

    let trunk_in_parent = format!("{PARENT} trunk");
    assert!(git(tree.path(), &["switch", "-q", "trunk"]));
    eventually("gh asks the parent about trunk", || {
        host.gh.asked().contains(&trunk_in_parent).then_some(())
    });
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ")), "{said:?}");
    assert_eq!(found(), None);
}

#[test]
fn a_fork_s_default_branch_keeps_what_the_fork_says_over_its_parent_s_closed_one() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    pushes_to_origin(tree.path(), "main");
    host.gh.fork_of("upstream", "repo");
    host.gh.open("main", &pull(1, "CLOSED"));
    host.gh.open_in(PARENT, "main", &pull(2, "CLOSED"));
    let cwd = tree.path().display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };

    eventually("the fork's closed one stands", || (found() == Some(1)).then_some(()));

    assert_eq!(host.gh.asked(), ["main", &format!("{PARENT} main")]);
}

fn on_pull_ref(dir: &Path, branch: &str, remote: Option<&str>, number: u32) {
    assert!(git(dir, &["branch", branch]));
    if let Some(remote) = remote {
        assert!(git(dir, &["config", &format!("branch.{branch}.remote"), remote]));
    }
    let merge = format!("refs/pull/{number}/head");
    assert!(git(dir, &["config", &format!("branch.{branch}.merge"), &merge]));
}

#[test]
fn a_pull_ref_is_asked_about_in_a_fork_s_parent_only_where_its_remote_is_the_parent() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    let dir = tree.path();
    forked_tree(dir);
    assert!(git(dir, &["remote", "add", "theirs", "https://github.com/Upstream/Repo.git"]));
    assert!(git(dir, &["config", "url.https://github.com/.insteadOf", "hub:"]));
    let of_parent = [
        ("by_url", "https://github.com/upstream/repo.git"),
        ("by_ssh", "git@github.com:upstream/repo.git"),
        ("by_name", "theirs"),
        ("by_short", "hub:upstream/repo.git"),
    ];
    for (number, (branch, remote)) in (1..).zip(of_parent) {
        on_pull_ref(dir, branch, Some(remote), number);
        host.gh.open_in(PARENT, branch, &pull(number, "OPEN"));
    }
    let third = "https://github.com/third/repo.git";
    on_pull_ref(dir, "of_a_third", Some(third), 11);
    on_pull_ref(dir, "of_a_third_pushing", Some(third), 12);
    assert!(git(dir, &["config", "branch.of_a_third_pushing.pushRemote", "origin"]));
    on_pull_ref(dir, "of_no_remote", None, 13);
    assert!(git(dir, &["config", "branch.of_no_remote.pushRemote", "origin"]));
    let of_others = ["of_a_third", "of_a_third_pushing", "of_no_remote"];
    for (number, branch) in (11..).zip(of_others) {
        host.gh.open_in(PARENT, branch, &pull(number, "OPEN"));
    }
    on_pull_ref(dir, "of_the_fork", Some("origin"), 14);
    host.gh.open("of_the_fork", &pull(14, "CLOSED"));
    host.gh.open_in(PARENT, "of_the_fork", &pull(15, "OPEN"));
    host.gh.fork_of("upstream", "repo");
    let cwd = dir.display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };
    let asked_of = |branch: &str| host.gh.asked().iter().any(|it| it == branch).then_some(());
    let in_parent = |branch: &str| format!("{PARENT} {branch}");
    let mut asked = vec!["main".to_owned()];

    eventually("gh asks about main", || asked_of("main"));

    for (number, (branch, _)) in (1..).zip(of_parent) {
        assert!(git(dir, &["switch", "-q", branch]));
        eventually(&format!("the parent's pull request is {branch}'s"), || {
            (found() == Some(number)).then_some(())
        });
        asked.extend([branch.to_owned(), in_parent(branch)]);
    }

    for branch in of_others {
        assert!(git(dir, &["switch", "-q", branch]));
        eventually(&format!("gh asks about {branch}"), || asked_of(branch));
        asked.push(branch.to_owned());
    }

    assert!(git(dir, &["switch", "-q", "of_the_fork"]));
    eventually("the fork's closed one stands", || (found() == Some(14)).then_some(()));
    asked.push("of_the_fork".to_owned());

    assert_eq!(host.gh.asked(), asked);
    assert_eq!(host.gh.repositories(), 1);
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ") || line.contains("git ")), "{said:?}");
}

#[test]
fn a_fork_s_parent_is_looked_up_again_as_its_remotes_change() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    for branch in ["second", "third", "same"] {
        assert!(git(tree.path(), &["branch", branch]));
    }
    for branch in ["main", "second", "third", "same"] {
        pushes_to_origin(tree.path(), branch);
    }
    host.gh.fork_of("first", "repo");
    host.gh.open_in("github.com/first/repo", "main", &pull(1, "OPEN"));
    host.gh.open_in("github.com/second/repo", "second", &pull(2, "OPEN"));
    host.gh.open_in("github.com/third/repo", "third", &pull(3, "OPEN"));
    host.gh.open_in("github.com/third/repo", "same", &pull(4, "OPEN"));
    let cwd = tree.path().display().to_string();
    let _daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| it.number)
    };

    eventually("the first parent's pull request is main's", || (found() == Some(1)).then_some(()));

    host.gh.fork_of("second", "repo");
    let moved = ["remote", "set-url", "origin", "https://github.com/other/repo.git"];
    assert!(git(tree.path(), &moved));
    assert!(git(tree.path(), &["switch", "-q", "second"]));
    eventually("a new url asks for the parent again", || (found() == Some(2)).then_some(()));

    host.gh.fork_of("third", "repo");
    assert!(git(tree.path(), &["config", "remote.origin.gh-resolved", "base"]));
    assert!(git(tree.path(), &["switch", "-q", "third"]));
    eventually("a new base asks for the parent again", || (found() == Some(3)).then_some(()));

    host.gh.fork_of("unasked", "repo");
    assert!(git(tree.path(), &["switch", "-q", "same"]));
    eventually("the same remotes keep the parent", || (found() == Some(4)).then_some(()));

    assert_eq!(host.gh.repositories(), 3);
}

#[test]
fn a_checkout_during_an_ask_of_a_fork_s_parent_is_no_trouble() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    pushes_to_origin(tree.path(), "main");
    assert!(git(tree.path(), &["branch", "fold"]));
    host.gh.fork_of("upstream", "repo");
    host.gh.open_in(PARENT, "fold", &json!({ "number": 1 }));
    host.gh.switch("fold");
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let in_parent = format!("{PARENT} fold");
    let asked = ["fold", in_parent.as_str(), "fold"];

    eventually("gh asks about fold once git sees it", || {
        (host.gh.asked() == asked).then_some(())
    });

    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ")), "{said:?}");
}

#[test]
fn what_goes_wrong_in_a_fork_s_parent_names_it_and_leaves_what_the_fork_said() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    pushes_to_origin(tree.path(), "main");
    host.gh.fork_of("upstream", "repo");
    host.gh.open("main", &pull(1, "CLOSED"));
    host.gh.open_in(PARENT, "main", &json!({ "number": 2 }));
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);

    let named = format!("gh pr view of {PARENT} in {cwd} said what does not read");
    eventually("the daemon says whose answer does not read", || {
        let said = daemon.said_after(Duration::from_millis(100));
        said.iter().any(|line| line.contains(&named)).then_some(())
    });
    eventually("the fork's closed one shows", || {
        let host = host.host();
        let found = host.pull_requests.into_iter().find(|it| it.dir == cwd);
        (found.map(|it| it.number) == Some(1)).then_some(())
    });
}

#[test]
fn a_branch_with_no_pull_request_in_a_fork_is_asked_about_in_its_parent() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    forked_tree(tree.path());
    assert!(git(tree.path(), &["branch", "theirs"]));
    assert!(git(tree.path(), &["branch", "ours"]));
    for branch in ["main", "theirs", "ours"] {
        pushes_to_origin(tree.path(), branch);
    }
    let in_parent = "https://github.com/upstream/repo/pull/2840";
    let in_fork = "https://github.com/owner/repo/pull/9";
    let pull = |number, url| {
        json!({ "number": number, "state": "OPEN", "isDraft": false, "url": url })
    };
    host.gh.fork_of("upstream", "repo");
    host.gh.open_in(PARENT, "theirs", &pull(2840, in_parent));
    host.gh.open_in(PARENT, "ours", &pull(2845, "https://github.com/upstream/repo/pull/2845"));
    host.gh.open("ours", &pull(9, in_fork));
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let found = || {
        let host = host.host();
        host.pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| (it.number, it.url))
    };

    let main_in_parent = format!("{PARENT} main");
    eventually("gh asks the parent about main", || {
        host.gh.asked().contains(&main_in_parent).then_some(())
    });

    assert!(git(tree.path(), &["switch", "-q", "theirs"]));
    eventually("the parent's pull request is theirs", || {
        (found() == Some((2840, in_parent.to_owned()))).then_some(())
    });

    assert!(git(tree.path(), &["switch", "-q", "ours"]));
    eventually("the fork's pull request is ours, though the parent has one too", || {
        (found() == Some((9, in_fork.to_owned()))).then_some(())
    });

    assert_eq!(
        host.gh.asked(),
        ["main", main_in_parent.as_str(), "theirs", &format!("{PARENT} theirs"), "ours"]
    );
    assert_eq!(host.gh.repositories(), 1);
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh ")), "{said:?}");
}

#[test]
fn a_checkout_git_has_yet_to_see_keeps_its_pull_request_off_the_branch_before() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    assert!(git(tree.path(), &["init", "-q", "-b", "main"]));
    commit(tree.path(), "README", "start\n");
    assert!(git(tree.path(), &["branch", "fold"]));
    let page = "https://github.com/owner/repo/pull/8";
    host.gh.open("fold", &json!({ "number": 8, "state": "OPEN", "isDraft": true, "url": page }));
    host.gh.switch("fold");
    let cwd = tree.path().display().to_string();
    let mut daemon = start(host.vars()).unwrap();
    watched(&host, &cwd);
    let seen = || {
        let host = host.host();
        let status = host.git_statuses.iter().find(|status| status.dir == cwd);
        let branch = match status.and_then(|status| status.stands.as_ref()) {
            Some(Stands::Checkout(Checkout { head: Some(Head::Branch(branch)), .. })) => {
                Some(branch.name.clone())
            },
            _ => None,
        };
        let number = host.pull_requests.iter().find(|it| it.dir == cwd).map(|it| it.number);
        (branch, number)
    };

    eventually("git sees fold checked out, and gh finds its pull request", || {
        let (branch, number) = seen();
        let seen = (branch.as_deref(), number);
        assert_ne!(seen, (Some("main"), Some(8)), "fold's pull request under main");
        (seen == (Some("fold"), Some(8))).then_some(())
    });
    let said = daemon.said_after(Duration::from_secs(1));
    assert!(!said.iter().any(|line| line.contains("gh pr view")), "{said:?}");
}

#[test]
fn luneta_watch_waits_for_the_host_to_change_but_not_for_long() {
    let host = Host::new();
    let daemon = start(host.vars()).unwrap();

    let (first, took) = host.watch(None);
    let version = first.version.unwrap();
    assert_eq!(version.pid, daemon.pid());
    assert!(took < Duration::from_secs(2), "{took:?}");

    let (same, took) = host.watch(Some(version));
    assert_eq!(same.version, Some(version));
    assert!(took >= Duration::from_secs(3), "{took:?}");

    let play = tempfile::tempdir().unwrap();
    let play = play.path().join("play");
    fs::create_dir(&play).unwrap();
    let (changed, took) = thread::scope(|scope| {
        let waiting = scope.spawn(|| host.watch(Some(version)));
        thread::sleep(Duration::from_millis(300));
        watched(&host, &play.display().to_string());
        waiting.join().unwrap()
    });
    assert!(changed.version.unwrap().seq > version.seq);
    let kept = changed.projects.and_then(|projects| projects.held);
    let Some(Held::Kept(kept)) = kept else { panic!("lunetad keeps no projects: {kept:?}") };
    assert!(kept.projects.iter().any(|project| project.name == "play"), "{kept:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");

    let another = Version { pid: version.pid + 1, ..changed.version.unwrap() };
    let (_, took) = host.watch(Some(another));
    assert!(took < Duration::from_secs(2), "{took:?}");
}
