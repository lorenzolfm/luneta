//! `luneta prune`, and the daemon's `Prunes` and `Prune`, for a far row of the
//! menu's `d m`: real git on a repository of a test's own, and a gh that says
//! what the test gives it.

use std::fs;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Output;

use lunetad::api::On;
use lunetad::api::daemon_client::DaemonClient;
use lunetad::prunes::{self, Goes, Plan, Pushed};
use serde_json::json;
use tonic::Code;

use crate::host::{Daemon, Host, LUNETA, answered, eventually, folder_in, start};
use crate::remote::{FakeSsh, hosts, reached};
use crate::sessions::listed;
use crate::worktree::{cloned_as_wt, git};

const PLANNED: i32 = 3;

fn said_by(dir: &Path, args: &[&str]) -> String {
    let mut git = std::process::Command::new("git");
    lunetad::local_git(&mut git);
    let said = git.arg("-C").arg(dir).args(args).output().unwrap();
    assert!(said.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&said.stderr));
    String::from_utf8(said.stdout).unwrap().trim().to_owned()
}

/// A repository wt cloned, with `main` checked out at `main`, and its origin
/// beside it, on the same disk.
struct Repository {
    clone: PathBuf,
    up: PathBuf,
}

impl Repository {
    fn new(scratch: &Path) -> Self {
        let (up, clone) = (scratch.join("up"), scratch.join("n"));
        cloned_as_wt(scratch, &up, &clone);
        let bare = clone.join(".bare");
        git(&bare, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
        git(&bare, &["fetch", "-q", "origin"]);
        git(&bare, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/main"]);
        Repository { clone, up }
    }

    fn at(&self, name: &str) -> PathBuf {
        self.clone.join(name)
    }

    /// A worktree of a new branch `name`, with a commit on it pushed to origin,
    /// and that commit.
    fn pushed(&self, name: &str) -> String {
        let (bare, at) = (self.clone.join(".bare"), self.at(name));
        git(&bare, &["worktree", "add", "-q", "-b", name, &at.display().to_string(), "main"]);
        git(&at, &["commit", "-q", "--allow-empty", "-m", name]);
        git(&at, &["push", "-q", "-u", "origin", name]);
        said_by(&at, &["rev-parse", "HEAD"])
    }

    /// Whether origin has the branch `name`.
    fn on_origin(&self, name: &str) -> bool {
        let up = said_by(&self.up, &["for-each-ref", "--format=%(refname)"]);
        up.lines().any(|line| line == format!("refs/heads/{name}"))
    }

    /// Whether this clone has the branch `name`.
    fn on_clone(&self, name: &str) -> bool {
        let bare = self.clone.join(".bare");
        let heads = said_by(&bare, &["for-each-ref", "--format=%(refname)", "refs/heads"]);
        heads.lines().any(|line| line == format!("refs/heads/{name}"))
    }
}

/// A pull request of the repository `o/n`, as gh says it, merged at `head`, by
/// `author`.
fn merged(number: u32, head: &str, author: &str) -> serde_json::Value {
    json!({
        "number": number,
        "state": "MERGED",
        "isDraft": false,
        "url": format!("https://github.com/o/n/pull/{number}"),
        "headRefOid": head,
        "author": { "id": "U", "is_bot": false, "login": author, "name": author },
        "headRepository": { "id": "R", "name": "n" },
        "headRepositoryOwner": { "id": "O", "login": "o" },
    })
}

/// A daemon of `host` that lists a session `work` of a pane in each of `dirs`,
/// once it has heard where they work.
fn serving(host: &Host, dirs: &[&Path]) -> (UnixListener, Daemon) {
    let panes: Vec<serde_json::Value> = dirs
        .iter()
        .zip(1..)
        .map(|(dir, id)| listed(id, "~", (0, "Tab #1"), Some(&dir.display().to_string())))
        .collect();
    host.zellij.list("work", &json!(panes));
    let work = host.serve("work");
    let daemon = start(host.vars()).unwrap();
    eventually("the daemon lists the session's panes", || {
        let sessions = host.host().sessions;
        sessions
            .iter()
            .find(|session| session.name == "work" && session.panes.is_some())
            .map(|_| ())
    });
    (work, daemon)
}

fn prune(host: &Host, from: &Path, repo: &Path, more: &[&str]) -> Output {
    let mut command = host.vars().command(LUNETA);
    command.current_dir(from).args(["prune", "--repo"]).arg(repo).args(more);
    answered(&mut command)
}

/// What a plan says, and the plan it tells `--yes` that by, of a run that
/// deleted nothing.
fn planned(said: &Output) -> (String, String) {
    let why = String::from_utf8_lossy(&said.stderr);
    assert_eq!(said.status.code(), Some(PLANNED), "{why}");
    let plan = why.split_once("--yes ").and_then(|(_, after)| after.split_whitespace().next());
    let plan = plan.unwrap_or_else(|| panic!("a plan tells --yes its word: {why}")).to_owned();
    (String::from_utf8(said.stdout.clone()).unwrap(), plan)
}

/// A run lists each worktree with a merged pull request: what goes, and why the
/// rest stay, and deletes nothing. Told yes to that, it deletes the worktree,
/// what git ignores in it included, and the branch, which git would not count
/// as merged, here and on origin, but for a branch of someone else's, or one
/// origin moved on from. What it did not list it leaves.
#[test]
fn it_lists_what_is_merged_at_its_head_and_deletes_only_that_once_told_yes() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    host.gh.api("me\n");

    let mine = repository.pushed("mine");
    host.gh.open("mine", &merged(1, &mine, "me"));
    fs::create_dir_all(repository.at("mine").join("target/debug")).unwrap();
    fs::write(repository.at("mine").join("target/debug/built"), "binary").unwrap();
    fs::write(repository.at("mine").join(".gitignore"), "target\n.gitignore\n").unwrap();

    let theirs = repository.pushed("theirs");
    host.gh.open("theirs", &merged(2, &theirs, "them"));

    let moved_on = repository.pushed("moved-on");
    host.gh.open("moved-on", &merged(3, &moved_on, "me"));
    git(&repository.at("moved-on"), &["commit", "-q", "--allow-empty", "-m", "since"]);
    let since = said_by(&repository.at("moved-on"), &["rev-parse", "HEAD"]);
    git(&repository.at("moved-on"), &["push", "-q"]);
    git(&repository.at("moved-on"), &["reset", "-q", "--hard", &moved_on]);

    let renamed = repository.pushed("renamed");
    host.gh.open("renamed", &merged(4, "0000000000000000000000000000000000000000", "me"));
    assert_ne!(renamed, "0000000000000000000000000000000000000000");

    let changed = repository.pushed("changed");
    host.gh.open("changed", &merged(5, &changed, "me"));
    fs::write(repository.at("changed").join("new"), "not yet added").unwrap();

    let paned = repository.pushed("paned");
    host.gh.open("paned", &merged(6, &paned, "me"));
    fs::create_dir(repository.at("paned").join("src")).unwrap();

    let based = repository.pushed("based");
    host.gh.open("based", &merged(7, &based, "me"));
    host.gh.based_on("based", 70);

    let from = repository.pushed("from");
    host.gh.open("from", &merged(8, &from, "me"));

    let locked = repository.pushed("locked");
    host.gh.open("locked", &merged(9, &locked, "me"));
    git(
        &repository.clone.join(".bare"),
        &["worktree", "lock", &repository.at("locked").display().to_string()],
    );

    let main = said_by(&repository.at("main"), &["rev-parse", "HEAD"]);
    host.gh.open("main", &merged(10, &main, "me"));

    repository.pushed("open");
    let open = json!({ "number": 11, "state": "OPEN", "isDraft": false, "url": "https://github.com/o/n/pull/11" });
    host.gh.open("open", &open);

    repository.pushed("no-pull");

    let _serving = serving(&host, &[&repository.at("paned").join("src")]);

    let asked = prune(&host, &repository.at("from"), &repository.at("main"), &[]);
    let (said, plan) = planned(&asked);
    let lines: Vec<&str> = said.lines().collect();
    let wanted = [
        "keep changed #5: it has modified or untracked files",
        "keep from #8: this runs from it",
        "keep locked #9: it is locked",
        "keep main #10: it is the default branch",
        "keep paned #6: it has a pane",
        "keep renamed #4: its HEAD is not its pull request's head",
        "keep based #7: #70 is based on it",
    ];
    for keep in wanted {
        assert!(lines.contains(&keep), "{keep:?} in {said}");
    }
    let goes: Vec<&&str> = lines.iter().filter(|line| line.starts_with("delete ")).collect();
    assert_eq!(goes.len(), 3, "{said}");
    let line_of = |name: &str| {
        let starts = format!("delete {name:<8}");
        **goes
            .iter()
            .find(|line| line.starts_with(&starts))
            .unwrap_or_else(|| panic!("{name} in {said}"))
    };
    assert!(line_of("mine").ends_with("branch mine, and mine on origin"), "{said}");
    assert!(line_of("mine").contains(" #1  "), "{said}");
    assert!(
        line_of("theirs")
            .ends_with("what is pushed of it stays: someone else wrote its pull request"),
        "{said}"
    );
    assert!(
        line_of("moved-on")
            .ends_with("what is pushed of it stays: origin has it at another commit"),
        "{said}"
    );
    assert!(!said.contains("open"), "{said}");
    assert!(!said.contains("no-pull"), "{said}");
    assert!(repository.at("mine").exists(), "it deleted before it was told yes");

    let told = prune(&host, &repository.at("from"), &repository.at("main"), &["--yes", &plan]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let printed = String::from_utf8(told.stdout).unwrap();
    for name in ["mine", "theirs", "moved-on"] {
        assert!(printed.contains(&format!("deleted {name}\n")), "{printed}");
        assert!(!repository.at(name).exists(), "{name} stays");
        assert!(!repository.on_clone(name), "{name}'s branch stays");
    }
    assert!(!repository.on_origin("mine"));
    assert!(repository.on_origin("theirs"), "it deleted someone else's branch");
    assert_eq!(
        said_by(&repository.up, &["rev-parse", "refs/heads/moved-on"]),
        since,
        "it deleted a branch origin moved on from"
    );
    for name in
        ["changed", "from", "locked", "main", "paned", "renamed", "based", "open", "no-pull"]
    {
        assert!(repository.at(name).join(".git").is_file(), "{name} went");
        assert!(repository.on_clone(name), "{name}'s branch went");
    }
    let once_more = prune(&host, &repository.at("from"), &repository.at("main"), &[]);
    assert!(once_more.status.success(), "{}", String::from_utf8_lossy(&once_more.stderr));
    let why = String::from_utf8_lossy(&once_more.stderr);
    assert!(why.contains("n has no merged worktree to delete"), "{why}");
}

/// Told yes to a plan, it deletes nothing where what would go has changed, and
/// says what would go now, as a pane has come to a worktree meanwhile.
#[test]
fn a_plan_that_changed_deletes_nothing_and_says_the_new_one() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    host.gh.api("me\n");
    for name in ["one", "two"] {
        let head = repository.pushed(name);
        host.gh.open(name, &merged(1, &head, "me"));
    }
    let _serving = serving(&host, &[&repository.at("main")]);
    let (_, plan) = planned(&prune(&host, &scratch, &repository.at("main"), &[]));

    let in_dir = |id, dir: &Path| listed(id, "~", (0, "Tab #1"), Some(&dir.display().to_string()));
    host.zellij.list(
        "work",
        &json!([in_dir(1, &repository.at("main")), in_dir(2, &repository.at("one"))]),
    );
    eventually("the daemon lists the new pane", || {
        let sessions = host.host().sessions;
        let panes = sessions.iter().find_map(|session| session.panes.as_ref())?;
        (panes.panes.len() == 2).then_some(())
    });
    let told = prune(&host, &scratch, &repository.at("main"), &["--yes", &plan]);
    let (said, now) = planned(&told);
    assert_ne!(now, plan);
    assert!(said.contains("keep one #1: it has a pane"), "{said}");
    assert!(said.contains("delete two"), "{said}");
    let why = String::from_utf8_lossy(&told.stderr);
    assert!(why.contains("told yes to another plan"), "{why}");
    assert!(repository.at("one").exists() && repository.at("two").exists(), "it deleted");
}

/// With no lunetad to say where the panes are, it deletes nothing, says so, and
/// asks gh nothing.
#[test]
fn it_deletes_nothing_while_no_lunetad_says_where_the_panes_are() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    let head = repository.pushed("one");
    host.gh.open("one", &merged(1, &head, "me"));
    let said = prune(&host, &scratch, &repository.at("main"), &["--yes", "0"]);
    let why = String::from_utf8_lossy(&said.stderr);
    assert_eq!(said.status.code(), Some(1), "{why}");
    assert!(why.contains("cannot tell where the panes are, so it deletes nothing"), "{why}");
    assert_eq!(host.gh.asked(), [] as [String; 0]);
    assert!(repository.at("one").exists());
}

/// A directory in no repository has nothing to list, and says so.
#[test]
fn a_directory_in_no_repository_says_so() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let _serving = serving(&host, &[&scratch]);
    let said = prune(&host, &scratch, &scratch, &[]);
    let why = String::from_utf8_lossy(&said.stderr);
    assert_eq!(said.status.code(), Some(1), "{why}");
    assert!(why.contains("is in no repository"), "{why}");
}

/// Where git refuses one worktree, as one that holds a submodule, that one
/// stays with its branch, the others go, and it says which.
#[test]
fn a_worktree_git_refuses_stays_and_the_others_go() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    host.gh.api("me\n");
    let other = repository.pushed("plain");
    host.gh.open("plain", &merged(1, &other, "me"));
    let before = repository.pushed("held");
    let at = repository.at("held");
    git(
        &at,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            &repository.up.display().to_string(),
            "sub",
        ],
    );
    git(&at, &["commit", "-q", "-m", "sub"]);
    let head = said_by(&at, &["rev-parse", "HEAD"]);
    git(&at, &["push", "-q"]);
    assert_ne!(head, before);
    host.gh.open("held", &merged(2, &head, "me"));
    let _serving = serving(&host, &[&repository.at("main")]);
    let (_, plan) = planned(&prune(&host, &scratch, &repository.at("main"), &[]));
    let told = prune(&host, &scratch, &repository.at("main"), &["--yes", &plan]);
    let why = String::from_utf8_lossy(&told.stderr);
    assert_eq!(told.status.code(), Some(1), "{why}");
    assert!(why.contains("held stays: "), "{why}");
    assert!(why.contains("submodules"), "{why}");
    assert!(!repository.at("plain").exists());
    assert!(at.exists());
    assert!(repository.on_clone("held"));
    assert!(repository.on_origin("held"));
    assert!(!repository.on_origin("plain"));
}

/// In luneta's ask box, a plan takes its yes only in the turn after the one
/// that said it, so that the user's answer comes between them, as a close's.
#[test]
fn in_the_ask_box_a_plan_takes_its_yes_only_in_the_next_turn() {
    let host = Host::new();
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    host.gh.api("me\n");
    let head = repository.pushed("one");
    host.gh.open("one", &merged(1, &head, "me"));
    let _serving = serving(&host, &[&repository.at("main")]);
    let in_turn = |vars: &[(&str, &str)], more: &[&str]| {
        let mut command = host.vars().command(LUNETA);
        command
            .current_dir(&scratch)
            .args(["prune", "--repo"])
            .arg(repository.at("main"))
            .args(more);
        for (name, value) in vars {
            command.env(name, value);
        }
        answered(&mut command)
    };
    let pane = ("LUNETA_PANE", "work:1");
    let first = [pane, ("LUNETA_TURN", "00000000000000aa")];
    let second =
        [pane, ("LUNETA_TURN", "00000000000000bb"), ("LUNETA_TURN_BEFORE", "00000000000000aa")];
    let (_, plan) = planned(&in_turn(&first, &[]));
    let same_turn = in_turn(&first, &["--yes", &plan]);
    assert_eq!(same_turn.status.code(), Some(PLANNED));
    let why = String::from_utf8_lossy(&same_turn.stderr);
    assert!(why.contains("only in the turn after"), "{why}");
    assert!(repository.at("one").exists());
    let next = in_turn(&second, &["--yes", &plan]);
    assert!(next.status.success(), "{}", String::from_utf8_lossy(&next.stderr));
    assert!(!repository.at("one").exists());
}

/// A far row's `d m` is planned and deleted on the row's machine, through the
/// daemon luneta watches: git and gh run there, and what the panes there say.
#[test]
fn a_far_row_lists_and_deletes_on_its_own_machine() {
    let here = Host::new();
    let far = Host::new();
    FakeSsh::new(here.zellij.bin()).reaches("far", far.vars());
    let scratch = tempfile::tempdir().unwrap();
    let scratch = scratch.path().canonicalize().unwrap();
    let repository = Repository::new(&scratch);
    far.gh.api("me\n");
    let head = repository.pushed("one");
    far.gh.open("one", &merged(1, &head, "me"));
    let head = repository.pushed("paned");
    far.gh.open("paned", &merged(2, &head, "me"));
    let _serving = serving(&far, &[&repository.at("paned")]);
    hosts(&here, "far\n");
    let _here = start(here.vars()).unwrap();
    reached(&here, "far", |far| far.sessions.iter().any(|session| session.panes.is_some()));

    let folder = folder_in(here.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon: DaemonClient<_> = lunetad::connect(&folder).await.unwrap();
        let on = || Some(On { destination: "far".to_owned() });
        let main = repository.at("main").display().to_string();
        let asked = prunes::asked(on(), &main);
        let answer = daemon.prunes(asked).await.unwrap().into_inner().message().await;
        let plan: Plan = answer.unwrap().unwrap().into();
        let goes: Vec<&str> = plan.goes.iter().map(|goes| goes.branch.as_str()).collect();
        assert_eq!(goes, ["one"]);
        assert_eq!(plan.kept[0].why, "it has a pane");
        let Goes { pushed, .. } = &plan.goes[0];
        let origin = Pushed::Goes { remote: "origin".to_owned(), branch: "one".to_owned() };
        assert_eq!(pushed, &origin);
        assert_eq!(here.gh.asked(), [] as [String; 0]);

        let told = prunes::told(on(), &main, &plan.goes);
        let answer = daemon.prune(told).await.unwrap().into_inner().message().await;
        let outcomes = prunes::outcomes_of(answer.unwrap().unwrap());
        assert!(
            outcomes.iter().all(|outcome| outcome.gone && outcome.trouble.is_none()),
            "{outcomes:?}"
        );
        assert!(!repository.at("one").exists());
        assert!(!repository.on_origin("one"));
        assert!(repository.at("paned").exists());

        let relative = prunes::asked(on(), "n/main");
        let refused = daemon.prunes(relative).await.unwrap_err();
        assert_eq!(refused.code(), Code::InvalidArgument, "{refused:?}");
    });
}
