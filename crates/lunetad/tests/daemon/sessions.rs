//! The host as `Watch` gives it: zellij's sessions and their panes, how the
//! work trees the panes work in stand, and when it answers.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::checkout::{Detached, Head};
use lunetad::api::work_tree::{Outside, Stands};
use lunetad::api::{self, Branch, Changes, Checkout, Upstream, Version, WatchRequest};
use prost::Message as _;
use serde_json::json;
use tempfile::TempDir;

use crate::host::{eventually, start, FakeZellij, Vars, LUNETA};

/// The folders of a host of a test's own: the daemon's, and zellij's sockets,
/// with a zellij that lists the panes the test gives it.
struct Host {
    runtime: TempDir,
    sockets: TempDir,
    zellij: FakeZellij,
}

impl Host {
    fn new() -> Self {
        let host = Host {
            runtime: tempfile::tempdir().unwrap(),
            sockets: tempfile::tempdir().unwrap(),
            zellij: FakeZellij::new(),
        };
        fs::create_dir_all(host.contract()).unwrap();
        host
    }

    fn vars(&self) -> Vars<'_> {
        Vars {
            runtime: Some(self.runtime.path()),
            zellij: Some(self.sockets.path()),
            bin: Some(self.zellij.bin()),
            ..Vars::default()
        }
    }

    /// The folder zellij 0.45 keeps its sessions' sockets in.
    fn contract(&self) -> PathBuf {
        self.sockets.path().join("contract_version_1")
    }

    /// A session's server, by the socket it listens on.
    fn serve(&self, session: &str) -> UnixListener {
        UnixListener::bind(self.contract().join(session)).unwrap()
    }

    /// The host, as the daemon answers a first look.
    fn host(&self) -> api::Host {
        let socket = self.runtime.path().join(crate::host::SOCKET);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async {
            let mut daemon = lunetad::connect(&socket).await.unwrap();
            daemon.watch(WatchRequest { after: None }).await.unwrap().into_inner()
        })
    }

    /// `luneta watch`, with `--after` as `after` gives it, and how long it
    /// took to answer.
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

fn session<'a>(host: &'a api::Host, name: &str) -> Option<&'a api::Session> {
    host.sessions.iter().find(|session| session.name == name)
}

/// A pane as zellij 0.45.1's `list-panes --json` spells it, cut down to what
/// tells one apart.
fn listed(id: u32, title: &str, tab: (u32, &str), cwd: Option<&str>) -> serde_json::Value {
    json!({
        "id": id,
        "is_plugin": false,
        "is_suppressed": false,
        "is_floating": false,
        "title": title,
        "tab_id": tab.0,
        "tab_position": tab.0,
        "tab_name": tab.1,
        "terminal_command": null,
        "pane_command": cwd.map(|_| "fish"),
        "pane_cwd": cwd,
    })
}

fn pane(id: u32, tab: (u32, &str), cwd: Option<&str>) -> api::Pane {
    api::Pane {
        id,
        tab: tab.0,
        tab_name: tab.1.to_owned(),
        cwd: cwd.map(str::to_owned),
        command: cwd.map(|_| "fish".to_owned()),
    }
}

/// A session is on the host as soon as its socket is, before zellij has said
/// what panes it has, which takes zellij's own list of sessions about a
/// second. Its panes are its terminals on screen: a plugin's pane is none,
/// and the column the sidebar stands in the place of is off the screen. What
/// zellij leaves empty, the daemon says nothing of, but what it said before
/// stands when it says nothing for being busy.
#[test]
fn a_session_is_there_as_its_socket_is_and_has_its_panes_once_zellij_lists_them() {
    let host = Host::new();
    let _daemon = start(host.vars()).unwrap();
    let _work = host.serve("work");

    let work = eventually("work runs", || session(&host.host(), "work").cloned());
    assert!(work.started_at.is_some());
    assert_eq!(work.panes, None);

    let mut column = listed(0, "tail -f /dev/null", (0, "Tab #1"), Some("/home/you"));
    column["is_suppressed"] = json!(true);
    let mut sidebar = listed(2, "luneta", (0, "Tab #1"), None);
    sidebar["is_plugin"] = json!(true);
    let mut unsaid = listed(4, "Pane #2", (1, "review"), None);
    unsaid["pane_command"] = json!("");
    let shell = listed(3, "~/work", (0, "Tab #1"), Some("/home/you/work"));
    host.zellij.list("work", &json!([column, sidebar, shell, unsaid]));

    let panes = eventually("zellij lists the panes of work", || {
        session(&host.host(), "work")?.panes.clone()
    });
    assert_eq!(
        panes.panes,
        [pane(3, (0, "Tab #1"), Some("/home/you/work")), pane(4, (1, "review"), None)]
    );

    let mut busy = shell.clone();
    busy["pane_cwd"] = json!(null);
    busy["pane_command"] = json!(null);
    let mut renamed = unsaid.clone();
    renamed["tab_name"] = json!("reviewed");
    host.zellij.list("work", &json!([busy, renamed]));
    let panes = eventually("zellij lists the panes of work again", || {
        let panes = session(&host.host(), "work")?.panes.clone()?;
        panes.panes.iter().any(|pane| pane.tab_name == "reviewed").then_some(panes)
    });
    assert_eq!(panes.panes[0], pane(3, (0, "Tab #1"), Some("/home/you/work")));
}

/// What zellij lists that does not read, the daemon says it cannot, and what
/// zellij listed before stands: a pane that leaves out whether it is a
/// plugin's, which a default would read as a terminal, and a pane listed
/// twice. The test waits out two sweeps.
#[test]
fn what_zellij_lists_that_does_not_read_leaves_what_it_listed_before() {
    let host = Host::new();
    let shell = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    host.zellij.list("work", &json!([shell]));
    host.zellij.list("play", &json!([shell]));
    let (_work, _play) = (host.serve("work"), host.serve("play"));
    let mut daemon = start(host.vars()).unwrap();
    let before = [pane(1, (0, "Tab #1"), Some("/home/you"))];
    for name in ["work", "play"] {
        eventually("zellij lists the panes of both", || {
            let panes = session(&host.host(), name)?.panes.clone()?;
            (panes.panes == before).then_some(())
        });
    }

    let mut sidebar = listed(2, "luneta", (0, "Tab #1"), None);
    sidebar.as_object_mut().unwrap().remove("is_plugin");
    host.zellij.list("work", &json!([shell, sidebar]));
    host.zellij.list("play", &json!([shell, shell]));
    let said = daemon.said_after(Duration::from_secs(5)).to_vec();
    let unread = |name: &str, why: &str| {
        let about = format!("the panes of {name}: zellij listed what does not read: {why}");
        said.iter().any(|line| line.contains(&about))
    };
    assert!(unread("work", "missing field `is_plugin`"), "{said:?}");
    assert!(unread("play", "pane 1 comes twice"), "{said:?}");
    let now = host.host();
    for name in ["work", "play"] {
        assert_eq!(session(&now, name).unwrap().panes.as_ref().unwrap().panes, before, "{name}");
    }
}

/// zellij is asked for a session's panes by name, and of two sessions that
/// share one, as those of zellij versions with contracts of their own can, it
/// answers for one: the daemon cannot tell which, and gives neither the panes.
/// A session renamed keeps its panes, whatever zellij says under its new
/// name, until another comes to share it: then the panes it had would only
/// go stale, as zellij is no longer asked. The test waits for zellij to be
/// asked about the session that has a name of its own twice: one not listed
/// yet is asked about first, so by then the daemon has asked about every
/// session it means to.
#[test]
fn sessions_that_share_a_name_have_no_panes_and_a_renamed_one_keeps_its() {
    let host = Host::new();
    let shell = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    host.zellij.list("work", &json!([shell]));
    host.zellij.list("play", &json!([shell]));
    let newer = host.sockets.path().join("contract_version_2");
    fs::create_dir(&newer).unwrap();
    let _work = (host.serve("work"), UnixListener::bind(newer.join("work")).unwrap());
    let _play = host.serve("play");
    let _daemon = start(host.vars()).unwrap();

    eventually("zellij is asked about play twice", || {
        let play = host.zellij.asked().into_iter().filter(|name| name == "play").count();
        (play >= 2).then_some(())
    });
    let now = host.host();
    let work: Vec<&api::Session> = now.sessions.iter().filter(|it| it.name == "work").collect();
    assert_eq!(work.len(), 2);
    assert!(work.iter().all(|session| session.panes.is_none()), "{work:?}");

    fs::rename(host.contract().join("play"), host.contract().join("game")).unwrap();
    let game = eventually("play is called game", || session(&host.host(), "game")?.panes.clone());
    assert_eq!(game.panes, [pane(1, (0, "Tab #1"), Some("/home/you"))]);

    let _game = UnixListener::bind(newer.join("game")).unwrap();
    eventually("neither session called game has panes", || {
        let now = host.host();
        let game: Vec<&api::Session> = now.sessions.iter().filter(|it| it.name == "game").collect();
        (game.len() == 2 && game.iter().all(|session| session.panes.is_none())).then_some(())
    });
}

/// git as the test runs it: on the repository at `dir` and no other, as one
/// that a git hook ran the tests from names, with no config but its own, and
/// as no one in particular.
fn git(dir: &Path, args: &[&str]) -> bool {
    let mut git = Command::new("git");
    for repository in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        git.env_remove(repository);
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
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

/// A work tree with a path of each kind of change, a stash, and a commit its
/// upstream does not have yet, in the middle of a merge that went wrong. One
/// more path is modified by its type: a file that is a link now.
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

/// git says how the work tree stands where each pane works: the branch, how
/// many paths changed in each way, the stashes, and where the branch is from
/// its upstream. A detached `HEAD` is a checkout of no branch, and a directory
/// in no repository is outside one, not a checkout with nothing in it. What
/// git said of a directory goes once no pane works there.
#[test]
fn git_says_how_the_work_tree_stands_where_each_pane_works() {
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
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    host.zellij.list("work", &json!([in_dir(1, &tree), in_dir(2, &plain), in_dir(3, &detached)]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();

    let trees = eventually("git says how all three stand", || {
        let trees = host.host().work_trees;
        (trees.len() == 3).then_some(trees)
    });
    let stands = |dir: &str| trees.iter().find(|tree| tree.dir == dir).unwrap().stands.clone();
    let every =
        Changes { conflicted: 1, deleted: 1, renamed: 1, modified: 2, staged: 1, untracked: 1 };
    let main = Branch { name: "main".to_owned(), upstream: Some(Upstream { ahead: 1, behind: 0 }) };
    let checkout = |head, changes, stashes| {
        Some(Stands::Checkout(Checkout { head: Some(head), changes: Some(changes), stashes }))
    };
    assert_eq!(stands(&tree), checkout(Head::Branch(main), every, 1));
    assert_eq!(stands(&detached), checkout(Head::Detached(Detached {}), Changes::default(), 0));
    assert_eq!(stands(&plain), Some(Stands::Outside(Outside {})));

    host.zellij.list("work", &json!([in_dir(1, &tree), in_dir(3, &detached)]));
    eventually("what git said of plain goes", || {
        let trees = host.host().work_trees;
        trees.iter().all(|tree| tree.dir != plain).then_some(())
    });
}

/// `luneta watch` writes out the host at once. With the version it last
/// wrote, it waits: it answers as soon as the host changes, and with the
/// host as it was once it has waited four seconds, so that a client that
/// asks again and again asks no more often than that. Sessions that share a
/// name, as those of zellij versions with contracts of their own can, leave
/// the host as it was from one look to the next. The version of another
/// daemon's, as one that ran before this one, is answered at once.
#[test]
fn luneta_watch_waits_for_the_host_to_change_but_not_for_long() {
    let host = Host::new();
    // Each starts in a tick of its own of the clock the kernel dates files
    // by, as sessions do: three that started as one would be the same.
    let _work: Vec<UnixListener> = (1..=3)
        .map(|version| {
            thread::sleep(Duration::from_millis(20));
            let contract = host.sockets.path().join(format!("contract_version_{version}"));
            fs::create_dir_all(&contract).unwrap();
            UnixListener::bind(contract.join("work")).unwrap()
        })
        .collect();
    let daemon = start(host.vars()).unwrap();

    let (first, took) = host.watch(None);
    let started: BTreeSet<(i64, i32)> = first
        .sessions
        .iter()
        .filter(|session| session.name == "work")
        .map(|session| session.started_at.map(|at| (at.seconds, at.nanos)).unwrap())
        .collect();
    assert_eq!(started.len(), 3, "{started:?}");
    let version = first.version.unwrap();
    assert_eq!(version.pid, daemon.pid());
    assert!(took < Duration::from_secs(2), "{took:?}");

    let (same, took) = host.watch(Some(version));
    assert_eq!(same.version, Some(version));
    assert!(took >= Duration::from_secs(3), "{took:?}");

    let (changed, took) = thread::scope(|scope| {
        let waiting = scope.spawn(|| host.watch(Some(version)));
        thread::sleep(Duration::from_millis(300));
        let _play = host.serve("play");
        waiting.join().unwrap()
    });
    assert!(changed.version.unwrap().seq > version.seq);
    assert!(session(&changed, "play").is_some());
    assert!(took < Duration::from_secs(3), "{took:?}");

    let another = Version { pid: version.pid + 1, ..changed.version.unwrap() };
    let (_, took) = host.watch(Some(another));
    assert!(took < Duration::from_secs(2), "{took:?}");
}
