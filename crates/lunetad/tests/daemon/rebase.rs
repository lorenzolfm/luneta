use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use luneta_waits::{eventually, PATIENCE};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::projects::Held;
use lunetad::api::On;
use lunetad::rebases::{self, Came, Rebased};
use lunetad::Directory;

use crate::host::{Host, LUNETA, answered, folder_in, script, start};
use crate::own::Owned;
use crate::remote::{FakeSsh, hosts, reached};
use crate::starts::{Window, makes_shells, shown, work_in};
use crate::worktree::{cloned_as_wt, git};

const UNFINISHED: i32 = 4;

const REBASE: &str = "\x1b[97;3ugr";

const SAYS_WHERE: &str = "printf '\\033]7;file://localhost%s\\007' \"$PWD\"\r";

struct Repository {
    up: PathBuf,
    fix: PathBuf,
}

impl Repository {
    fn new(scratch: &Path) -> Repository {
        let (up, clone) = (scratch.join("up"), scratch.join("n"));
        cloned_as_wt(scratch, &up, &clone);
        tracking(&clone);
        git(&clone.join(".bare"), &["worktree", "add", "-q", "-b", "fix", "../fix", "main"]);
        Repository { up, fix: clone.join("fix") }
    }
}

fn tracking(clone: &Path) {
    let bare = clone.join(".bare");
    git(&bare, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
    git(&bare, &["fetch", "-q", "origin"]);
    git(&bare, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
    for (key, value) in [("user.name", "t"), ("user.email", "t@t"), ("core.hooksPath", "/dev/null")] {
        git(&bare, &["config", key, value]);
    }
}

fn commit(dir: &Path, file: &str, text: &str) {
    fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
}

fn rebased(host: &Host, dir: &Path) -> Output {
    let mut luneta = host.vars().command(LUNETA);
    lunetad::local_git(&mut luneta);
    luneta.current_dir(dir).arg("rebase");
    answered(&mut luneta)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn terminals_in(host: &Host, folder: &Path) -> usize {
    let Some(Held::Kept(kept)) = host.host().projects.and_then(|projects| projects.held) else {
        panic!("lunetad keeps no projects");
    };
    let folder = folder.display().to_string();
    let mut held = kept.projects.iter().flat_map(|it| it.worktrees().unwrap_or_default());
    held.find(|it| it.folder == folder).map_or(0, |it| it.terminals.len())
}

#[test]
fn luneta_rebase_moves_a_branch_onto_upstream_and_hands_its_conflicts_to_an_agent() {
    let owned = Owned::new();
    let host = &owned.host;
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let repository = Repository::new(&scratch);
    let fix = &repository.fix;
    let asked = host.home.path().join("claude-asked");
    let claude = format!("printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n", asked.display());
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
    let conflicts = "luneta: the rebase of fix onto origin/main stopped on conflicts in a file\n";
    assert!(why.starts_with(conflicts), "{why}");
    assert!(why.ends_with("luneta: an agent resolves it, in a view of the worktree\n"), "{why}");
    let agent = eventually("an agent starts on the conflicts", || fs::read_to_string(&asked).ok());
    assert!(agent.starts_with("The rebase of fix onto origin/main stopped at "), "{agent}");
    assert!(agent.contains(" theirs, on conflicts in theirs. Resolve it: "), "{agent}");
    assert!(scratch.join("n/.bare/worktrees/fix/rebase-merge").is_dir(), "the rebase waits");
    assert_eq!(terminals_in(host, fix), 1, "the worktree opens with the agent alone");

    git(fix, &["rebase", "--abort"]);
    fs::remove_file(&asked).unwrap();
    let again = rebased(host, fix);
    assert_eq!(again.status.code(), Some(UNFINISHED), "{}", stderr(&again));
    eventually("a second agent starts", || fs::read_to_string(&asked).ok());
    assert_eq!(terminals_in(host, fix), 2, "beside the first");
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
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon: DaemonClient<_> = lunetad::connect(&folder, PATIENCE).await.unwrap();
        let on = || Some(On { destination: "far".to_owned() });
        let dir = Directory::parse(repository.fix.display().to_string()).unwrap();
        let mut rebase = async |came: Came| {
            let answer = daemon.rebase(rebases::asked(on(), &dir, Some("fix"))).await.unwrap();
            let answer = answer.into_inner().message().await.unwrap().unwrap();
            let rebased = Rebased::of_api(answer).unwrap();
            assert_eq!((rebased.branch.as_str(), rebased.onto.as_str()), ("fix", "origin/main"));
            assert_eq!(rebased.came, came);
        };
        rebase(Came::Clean { ahead: 1 }).await;
        rebase(Came::Already).await;
    });
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
    let claude = format!("printf '%s\\n' \"$@\" > {0}.part && mv {0}.part {0}\nexec sleep 600\n", asked.display());
    script(&host.bin().join("claude"), &claude);
    commit(&dir.path, "own", "o\n");
    commit(&up, "theirs", "t\n");

    let mut window = Window::open(host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    window.send(SAYS_WHERE);
    eventually("the view's row says its branch", || window.shows(" fix").then_some(()));
    window.send(REBASE);
    let clean = "fix is rebased onto origin/main, one commit on top";
    eventually("a note says it is rebased", || window.shows(clean).then_some(()));
    assert!(!asked.exists(), "a clean rebase starts no agent");

    commit(&dir.path, "theirs", "mine\n");
    commit(&up, "theirs", "yours\n");
    window.send(REBASE);
    let agent = eventually("an agent starts on the conflicts", || fs::read_to_string(&asked).ok());
    assert!(agent.starts_with("The rebase of fix onto origin/main stopped at "), "{agent}");
    assert!(agent.contains(", on conflicts in theirs. Resolve it: "), "{agent}");
    let terminals = || shown(&owned, &dir.path).map_or(0, |it| it.terminals.len());
    eventually("the agent opens beside the view", || (terminals() == 2).then_some(()));
}
