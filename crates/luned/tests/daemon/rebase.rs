use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use lune_protocol::git::{GitStatus, Head};
use lune_protocol::said::Said;
use lune_waits::eventually;
use luned::Directory;
use luned::api::daemon_client::DaemonClient;
use luned::api::projects::Held;
use luned::api::remote::Reached;
use luned::api::{On, RebaseRequest};
use luned::rebases::{self, Came, Rebased, StatusRefresh};
use serde_json::json;
use tonic::Code;

use crate::host::{Host, LUNE, PATIENT, answered, folder_in, script, start};
use crate::own::Owned;
use crate::projects::{project, worktree};
use crate::remote::{FakeSsh, hosts, reached};
use crate::starts::{Window, makes_shells, shown, work_in};
use crate::worktree::{cloned_as_wt, git};

const UNFINISHED: i32 = 4;

const REBASE: &str = "\x1b[97;3ugr";

const SAYS_WHERE: &str = "printf '\\033]7;file://localhost%s\\007' \"$PWD\"\r";

pub struct Repository {
    pub up: PathBuf,
    pub fix: PathBuf,
}

impl Repository {
    pub fn new(scratch: &Path) -> Repository {
        let (up, clone) = (scratch.join("up"), scratch.join("n"));
        cloned_as_wt(scratch, &up, &clone);
        tracking(&clone);
        git(
            &clone.join(".bare"),
            &["worktree", "add", "-q", "-b", "fix", "../fix", "main"],
        );
        Repository {
            up,
            fix: clone.join("fix"),
        }
    }
}

fn tracking(clone: &Path) {
    let bare = clone.join(".bare");
    git(
        &bare,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    git(&bare, &["fetch", "-q", "origin"]);
    git(
        &bare,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&bare, &["config", key, value]);
    }
}

fn forked_on_github(clone: &Path, up: &Path) {
    let bare = clone.join(".bare");
    let (fork, parent) = ("git@github.com:me/n.git", "git@github.com:o/n.git");
    git(&bare, &["config", "remote.origin.url", fork]);
    git(&bare, &["remote", "add", "upstream", parent]);
    for github in [fork, parent] {
        git(
            &bare,
            &[
                "config",
                "--add",
                &format!("url.{}.insteadOf", up.display()),
                github,
            ],
        );
    }
}

fn pull(state: &str, url: &str, base: &str) -> serde_json::Value {
    json!({ "number": 7, "state": state, "isDraft": false, "url": url, "baseRefName": base })
}

fn tip(dir: &Path) -> String {
    let mut git = std::process::Command::new("git");
    luned::local_git(&mut git)
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"]);
    String::from_utf8(git.output().unwrap().stdout).unwrap()
}

pub fn commit(dir: &Path, file: &str, text: &str) {
    fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
}

fn rebased(host: &Host, dir: &Path) -> Output {
    let mut lune = host.vars().command(&LUNE);
    luned::local_git(&mut lune);
    lune.current_dir(dir).arg("rebase");
    answered(&mut lune)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn terminals_in(host: &Host, folder: &Path) -> usize {
    let Some(Held::Kept(kept)) = host.host().projects.and_then(|projects| projects.held) else {
        panic!("luned keeps no projects");
    };
    let folder = folder.display().to_string();
    let mut held = kept
        .projects
        .iter()
        .flat_map(|it| it.worktrees().unwrap_or_default());
    held.find(|it| it.folder == folder)
        .map_or(0, |it| it.terminals.len())
}

#[test]
fn lune_rebase_moves_a_branch_onto_upstream_and_hands_its_conflicts_to_an_agent() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let fix = &repository.fix;
    let asked = host.home.path().join("claude-asked");
    let claude = format!(
        "printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n",
        asked.display()
    );
    script(&host.bin().join("claude"), &claude);

    commit(fix, "own", "o\n");
    commit(&repository.up, "theirs", "t\n");
    fs::write(fix.join("own"), "not committed\n").unwrap();
    let refused = rebased(host, fix);
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).ends_with("has changes not committed: commit or stash them first\n"));
    git(fix, &["checkout", "-q", "own"]);
    let clean = rebased(host, fix);
    assert!(clean.status.success(), "{}", stderr(&clean));
    let said = String::from_utf8_lossy(&clean.stdout);
    assert_eq!(said, "fix is rebased onto origin/main, one commit on top\n");
    assert!(!asked.exists(), "a clean rebase starts no agent");
    assert_eq!(terminals_in(host, fix), 0, "nor opens a worktree");

    commit(fix, "theirs", "mine\n");
    commit(&repository.up, "theirs", "yours\n");
    let stopped = rebased(host, fix);
    let why = stderr(&stopped);
    assert_eq!(stopped.status.code(), Some(UNFINISHED), "{why}");
    let conflicts = "lune: the rebase of fix onto origin/main stopped on conflicts in a file\n";
    assert!(why.starts_with(conflicts), "{why}");
    assert!(
        why.ends_with("lune: an agent resolves it, in a view of the worktree\n"),
        "{why}"
    );
    let agent = eventually("an agent starts on the conflicts", || {
        fs::read_to_string(&asked).ok()
    });
    assert!(
        agent.starts_with("--\nThe rebase of fix onto origin/main stopped at "),
        "{agent}"
    );
    assert!(
        agent.contains(" theirs, on conflicts in theirs. Resolve it: "),
        "{agent}"
    );
    assert!(
        scratch.join("n/.bare/worktrees/fix/rebase-merge").is_dir(),
        "the rebase waits"
    );
    assert_eq!(
        terminals_in(host, fix),
        1,
        "the worktree opens with the agent alone"
    );

    git(fix, &["rebase", "--abort"]);
    fs::remove_file(&asked).unwrap();
    let again = rebased(host, fix);
    assert_eq!(again.status.code(), Some(UNFINISHED), "{}", stderr(&again));
    eventually("a second agent starts", || fs::read_to_string(&asked).ok());
    assert_eq!(terminals_in(host, fix), 2, "beside the first");
}

#[test]
fn lune_rebase_moves_a_stacked_branch_onto_its_open_pull_request_s_base() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let (up, fix) = (&repository.up, &repository.fix);
    forked_on_github(&scratch.join("n"), up);
    git(up, &["switch", "-q", "-c", "base"]);
    commit(up, "beneath", "b\n");
    git(fix, &["pull", "-q", "--ff-only", "origin", "base"]);
    commit(fix, "own", "o\n");
    git(fix, &["push", "-q", "-u", "origin", "fix"]);
    commit(up, "beneath", "moved on\n");
    git(up, &["switch", "-q", "main"]);
    commit(up, "theirs", "t\n");

    let mut draft = pull("OPEN", "https://github.com/O/N/pull/7", "base");
    draft["isDraft"] = json!(true);
    host.gh.open("fix", &draft);
    let stacked = rebased(host, fix);
    assert!(stacked.status.success(), "{}", stderr(&stacked));
    let said = String::from_utf8_lossy(&stacked.stdout);
    assert_eq!(
        said,
        "fix is rebased onto upstream/base, one commit on top\n"
    );
    assert_eq!(
        fs::read_to_string(fix.join("beneath")).unwrap(),
        "moved on\n",
        "as fetched now"
    );
    assert!(!fix.join("theirs").exists(), "and not onto main");

    let refused = |why: &str| {
        let before = tip(fix);
        let refused = rebased(host, fix);
        assert_eq!(refused.status.code(), Some(1), "{}", stderr(&refused));
        assert!(stderr(&refused).ends_with(why), "{}", stderr(&refused));
        assert_eq!(tip(fix), before, "a refusal rebases nothing");
    };
    host.gh.open(
        "fix",
        &pull("OPEN", "https://github.com/elsewhere/n/pull/7", "base"),
    );
    refused(&format!(
        "which no remote of {} fetches: add one that does\n",
        fix.display()
    ));
    host.gh.open(
        "fix",
        &pull(
            "OPEN",
            "https://github.com/o/n/pull/7",
            "base:refs/heads/main",
        ),
    );
    refused("no branch\n");
    host.gh.fail("fix");
    refused("HTTP 401: Requires authentication (https://api.github.com/graphql)\n");
    host.gh.mend("fix");

    host.gh.open(
        "fix",
        &pull("MERGED", "https://github.com/o/n/pull/7", "base"),
    );
    let merged = rebased(host, fix);
    assert!(merged.status.success(), "{}", stderr(&merged));
    let said = String::from_utf8_lossy(&merged.stdout);
    assert_eq!(said, "fix is rebased onto origin/main, 3 commits on top\n");
    assert!(
        fix.join("theirs").exists(),
        "a merged pull request's branch goes onto main"
    );

    git(fix, &["branch", "-q", "--unset-upstream"]);
    host.gh.open(
        "fix",
        &pull("OPEN", "https://github.com/o/n/pull/7", "base"),
    );
    let unpushed = rebased(host, fix);
    let said = String::from_utf8_lossy(&unpushed.stdout);
    assert_eq!(
        said,
        "fix is on origin/main already\n",
        "{}",
        stderr(&unpushed)
    );
}

#[test]
fn lune_rebase_with_no_luned_moves_nothing() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let fix = &repository.fix;
    commit(fix, "own", "o\n");
    commit(&repository.up, "theirs", "t\n");
    let before = tip(fix);
    let refused = rebased(&host, fix);
    let why = stderr(&refused);
    assert_eq!(refused.status.code(), Some(1), "{why}");
    assert!(
        why.starts_with("lune: no rebase: no luned runs in "),
        "{why}"
    );
    assert_eq!(tip(fix), before, "the branch stays where it was");
}

#[test]
fn a_rebase_asked_of_luned_for_a_branch_with_no_name_is_refused() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let _luned = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = repository.fix.display().to_string();
    let request = RebaseRequest {
        on: None,
        dir,
        branch: Some(String::new()),
    };
    let refused = runtime.block_on(async {
        let mut daemon: DaemonClient<_> = luned::connect(&folder, PATIENT).await.unwrap();
        daemon.rebase(request).await.map(drop).unwrap_err()
    });
    let said = (refused.code(), refused.message());
    assert_eq!(said, (Code::InvalidArgument, "branch: it names no branch"));
}

#[test]
fn a_rebase_in_a_repository_a_folder_s_project_holds_starts_no_agent_in_the_folder() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let notes = scratch.join("notes");
    let plain = notes.join("foo");
    git(
        &scratch,
        &[
            "clone",
            "-q",
            &repository.up.display().to_string(),
            &plain.display().to_string(),
        ],
    );
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(&plain, &["config", key, value]);
    }
    project(host, &notes, None);
    let asked = host.home.path().join("claude-asked");
    script(
        &host.bin().join("claude"),
        &format!("touch {}\nexec sleep 600\n", asked.display()),
    );

    commit(&plain, "theirs", "mine\n");
    commit(&repository.up, "theirs", "yours\n");
    let stopped = rebased(host, &plain);
    let why = stderr(&stopped);
    assert_eq!(stopped.status.code(), Some(UNFINISHED), "{why}");
    let one = "notes is a folder, and its own is its one worktree";
    let (plain_s, notes_s) = (plain.display(), notes.display());
    let held = format!("{plain_s} is in {notes_s}, project notes's folder; {one}");
    let waits = format!("lune: no agent started, and the rebase waits for you: {held}\n");
    assert!(why.ends_with(&waits), "{why}");
    assert!(plain.join(".git/rebase-merge").is_dir(), "the rebase waits");
    assert!(!asked.exists(), "no agent starts in the folder");
    assert_eq!(
        terminals_in(host, &notes),
        0,
        "nor does the folder's worktree open"
    );
}

#[test]
fn a_far_worktree_is_rebased_on_its_own_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    commit(&repository.fix, "own", "o\n");
    commit(&repository.up, "theirs", "t\n");
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |_| true);

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut daemon: DaemonClient<_> = luned::connect(&folder, PATIENT).await.unwrap();
        let on = || {
            Some(On {
                destination: "far".to_owned(),
            })
        };
        let dir = Directory::parse(repository.fix.display().to_string()).unwrap();
        let fix = Said::new("fix".to_owned()).unwrap();
        let mut rebase = async |came: Came| {
            let answer = daemon
                .rebase(rebases::asked(on(), &dir, Some(&fix)))
                .await
                .unwrap();
            let answer = answer.into_inner().message().await.unwrap().unwrap();
            let (rebased, refresh) = Rebased::of_api_with_refresh(answer, Some(&fix)).unwrap();
            assert_eq!(
                (rebased.branch.as_str(), rebased.onto.as_str()),
                ("fix", "origin/main")
            );
            assert_eq!(rebased.came, came);
            refresh
        };
        assert_eq!(
            rebase(Came::Clean { ahead: 1 }).await,
            Some(StatusRefresh::Untracked)
        );
        assert_eq!(rebase(Came::Already).await, Some(StatusRefresh::Untracked));
    });

    let tracked = project(&far, &scratch.join("n"), None);
    worktree(&far, tracked, &repository.fix);
    let fix_dir = repository.fix.to_str().unwrap();
    let far_branch = |host: &luned::api::remote::Host| {
        let status = host
            .git_statuses
            .iter()
            .find(|status| status.dir == fix_dir)?;
        match GitStatus::of_api(status)? {
            GitStatus::Checkout {
                head: Head::Branch { name, .. },
                ..
            } => Some(name.as_str().to_owned()),
            _ => None,
        }
    };
    reached(&here, "far", |host| {
        far_branch(host).as_deref() == Some("fix")
    });

    commit(&repository.up, "more", "m\n");
    runtime.block_on(async {
        let mut daemon: DaemonClient<_> = luned::connect(&folder, PATIENT).await.unwrap();
        let dir = Directory::parse(fix_dir.to_owned()).unwrap();
        let fix = Said::new("fix".to_owned()).unwrap();
        let answer = daemon
            .rebase(rebases::asked(
                Some(On {
                    destination: "far".to_owned(),
                }),
                &dir,
                Some(&fix),
            ))
            .await
            .unwrap();
        let answer = answer.into_inner().message().await.unwrap().unwrap();
        let (rebased, refresh) = Rebased::of_api_with_refresh(answer, Some(&fix)).unwrap();
        assert_eq!(rebased.came, Came::Clean { ahead: 1 });
        assert!(
            matches!(refresh, Some(StatusRefresh::Refreshed(_))),
            "bridged tracked rebase reports Refreshed: {refresh:?}"
        );
    });
    let far_host = here
        .host()
        .remotes
        .into_iter()
        .find(|remote| remote.destination == "far")
        .and_then(|remote| match remote.reached {
            Some(Reached::Host(host)) => Some(host),
            _ => None,
        })
        .expect("far remote host is present immediately after bridged rebase");
    assert_eq!(
        far_branch(&far_host).as_deref(),
        Some("fix"),
        "local KnownRemotes reflects the post-rebase branch before bridged RebaseResponse returns"
    );
}

#[test]
fn g_r_over_a_view_rebases_its_worktree_and_starts_an_agent_beside_it_on_conflicts() {
    if !makes_shells() {
        return;
    }
    let owned = Owned::new();
    let host = &owned.host;
    let dir = work_in(&owned, "work");
    let clone = dir.path.parent().unwrap().to_owned();
    let up = clone.parent().unwrap().join("up");
    tracking(&clone);
    git(&dir.path, &["switch", "-q", "-c", "fix"]);
    let asked = host.home.path().join("claude-asked");
    let claude = format!(
        "printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n",
        asked.display()
    );
    script(&host.bin().join("claude"), &claude);
    commit(&dir.path, "own", "o\n");
    commit(&up, "theirs", "t\n");

    let mut window = Window::open(host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    window.send(SAYS_WHERE);
    window.seated("work/main");
    eventually("the view's row says its branch", || {
        window.shows(" fix").then_some(())
    });
    window.send(REBASE);
    let clean = "fix is rebased onto origin/main, one commit on top";
    eventually("a note says it is rebased", || {
        window.shows(clean).then_some(())
    });
    assert!(!asked.exists(), "a clean rebase starts no agent");
    let work_dir = fs::canonicalize(&dir.path).unwrap();
    let status = host
        .host()
        .git_statuses
        .into_iter()
        .find(|status| status.dir == work_dir.to_str().unwrap())
        .and_then(|status| GitStatus::of_api(&status));
    assert!(
        matches!(
            status,
            Some(GitStatus::Checkout { head: Head::Branch { ref name, .. }, .. })
                if name.as_str() == "fix"
        ),
        "luned refreshed the branch status before reporting rebase completion: {status:?}"
    );

    commit(&dir.path, "theirs", "mine\n");
    commit(&up, "theirs", "yours\n");
    eventually("the view's row sees the commit on fix", || {
        window.shows(" fix +2 -1").then_some(())
    });
    window.send(REBASE);
    let agent = eventually("an agent starts on the conflicts", || {
        fs::read_to_string(&asked).ok()
    });
    assert!(
        agent.starts_with("--\nThe rebase of fix onto origin/main stopped at "),
        "{agent}"
    );
    assert!(
        agent.contains(", on conflicts in theirs. Resolve it: "),
        "{agent}"
    );
    let terminals = || shown(&owned, &dir.path).map_or(0, |it| it.terminals.len());
    eventually("the agent opens beside the view", || {
        (terminals() == 2).then_some(())
    });
}

#[test]
fn a_rebase_from_a_deleted_subdirectory_still_refreshes_the_tracked_worktree() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let (up, fix) = (&repository.up, &repository.fix);
    let up_sub = up.join("sub");
    fs::create_dir(&up_sub).unwrap();
    commit(up, "sub/gone", "a\n");
    git(fix, &["fetch", "-q", "origin"]);
    git(fix, &["reset", "-q", "--hard", "origin/main"]);
    commit(fix, "own", "o\n");
    git(up, &["rm", "-q", "sub/gone"]);
    git(up, &["commit", "-q", "-m", "drop sub"]);

    let sub = fix.join("sub");
    assert!(sub.is_dir(), "subdirectory exists before rebase");

    let tracked = project(host, &scratch.join("n"), None);
    worktree(host, tracked, fix);
    let fix_dir = fix.to_str().unwrap();
    let branch_of = || {
        let status = host
            .host()
            .git_statuses
            .into_iter()
            .find(|status| status.dir == fix_dir)?;
        match GitStatus::of_api(&status)? {
            GitStatus::Checkout {
                head: Head::Branch { name, .. },
                ..
            } => Some(name.as_str().to_owned()),
            _ => None,
        }
    };
    eventually("luned tracks the worktree branch before rebase", || {
        (branch_of().as_deref() == Some("fix")).then_some(())
    });

    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let refresh = runtime.block_on(async {
        let mut daemon: DaemonClient<_> = luned::connect(&folder, PATIENT).await.unwrap();
        let sub_dir = Directory::parse(sub.to_str().unwrap().to_owned()).unwrap();
        let fix_branch = Said::new("fix".to_owned()).unwrap();
        let answer = daemon
            .rebase(rebases::asked(None, &sub_dir, Some(&fix_branch)))
            .await
            .unwrap();
        let answer = answer.into_inner().message().await.unwrap().unwrap();
        let (rebased, refresh) = Rebased::of_api_with_refresh(answer, Some(&fix_branch)).unwrap();
        assert_eq!(rebased.came, Came::Clean { ahead: 1 });
        refresh
    });
    assert!(
        !sub.exists(),
        "rebase deleted the subdirectory it was invoked from"
    );
    assert!(
        matches!(refresh, Some(StatusRefresh::Refreshed(_))),
        "rebase from a deleted subdirectory still refreshes its tracked worktree: {refresh:?}"
    );
    assert_eq!(branch_of().as_deref(), Some("fix"));
}
