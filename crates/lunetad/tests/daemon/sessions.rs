//! The host as `Watch` gives it: zellij's sessions and their panes, how the
//! work trees the panes work in stand, and when it answers.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, Permissions};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lunetad::api::checkout::Head;
use lunetad::api::work_tree::{NoCheckout, Stands};
use lunetad::api::{self, Branch, Changes, Checkout, PullRequestState, Upstream, Version};
use prost::Message as _;
use serde_json::json;

use crate::host::{eventually, start, Host, LUNETA};

impl Host {
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

pub fn session<'a>(host: &'a api::Host, name: &str) -> Option<&'a api::Session> {
    host.sessions.iter().find(|session| session.name == name)
}

/// A pane as zellij 0.45.1's `list-panes --json` spells it, cut down to what
/// tells one apart.
pub fn listed(id: u32, title: &str, tab: (u32, &str), cwd: Option<&str>) -> serde_json::Value {
    json!({
        "id": id,
        "is_plugin": false,
        "is_selectable": true,
        "is_suppressed": false,
        "is_floating": false,
        "is_held": false,
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
        titled: None,
        shell: None,
        tab_id: Some(tab.0),
        held: false,
    }
}

/// A session is on the host as soon as its socket is, before zellij has said
/// what panes it has, which takes zellij's own list of sessions about a
/// second. Its panes are its terminals on screen: a plugin's pane is none,
/// and a terminal another stands in the place of is off the screen. What
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

    let mut suppressed = listed(0, "tail -f /dev/null", (0, "Tab #1"), Some("/home/you"));
    suppressed["is_suppressed"] = json!(true);
    let mut plugin = listed(2, "luneta", (0, "Tab #1"), None);
    plugin["is_plugin"] = json!(true);
    let mut unsaid = listed(4, "Pane #2", (1, "review"), None);
    unsaid["pane_command"] = json!("");
    let shell = listed(3, "~/work", (0, "Tab #1"), Some("/home/you/work"));
    host.zellij.list("work", &json!([suppressed, plugin, shell, unsaid]));

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

/// zellij writes what it keeps of a session within a second of a pane or a
/// tab opening or closing, and the daemon asks about the session again as it
/// does, ahead of its sweep.
#[test]
fn a_session_is_asked_about_again_as_zellij_writes_that_it_changed() {
    let host = Host::new();
    let home = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    host.zellij.list("work", &json!([home]));
    host.write_metadata("work", &kept(&[(1, 0)], 1));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("zellij lists the panes of work", || session(&host.host(), "work")?.panes.clone());

    host.zellij.asked_again("work");
    let opened = listed(2, "~/work", (1, "Tab #2"), Some("/home/you/work"));
    host.zellij.list("work", &json!([home, opened]));
    host.write_metadata("work", &kept(&[(1, 0), (2, 1)], 2));
    let wrote = Instant::now();
    eventually("work has the pane opened", || {
        let panes = session(&host.host(), "work")?.panes.clone()?;
        (panes.panes.len() == 2).then_some(())
    });
    assert!(wrote.elapsed() < Duration::from_secs(1), "{:?}", wrote.elapsed());
}

/// A session whose server hangs holds the daemon up for as long as it waits
/// on zellij, and no longer, however often zellij writes what it keeps of
/// another: that one is asked about before the hung one is due again, though
/// the hung one, never listed, goes first.
#[test]
fn a_session_that_hangs_holds_the_others_up_no_longer_than_it_is_asked_about() {
    let host = Host::new();
    let home = listed(1, "~", (0, "Tab #1"), Some("/home/you"));
    host.zellij.list("work", &json!([home]));
    host.zellij.hang("stuck");
    let _work = host.serve("work");
    let _stuck = host.serve("stuck");
    let _daemon = start(host.vars()).unwrap();
    eventually("zellij lists the panes of work", || session(&host.host(), "work")?.panes.clone());
    let stuck = || host.zellij.asked().iter().any(|asked| asked == "stuck");
    eventually("zellij is asked about stuck", || stuck().then_some(()));

    let opened = listed(2, "~/work", (1, "Tab #2"), Some("/home/you/work"));
    host.zellij.list("work", &json!([home, opened]));
    let opening = Instant::now();
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        // As the pane opens, and as zellij is asked about work anew after,
        // for as long as `eventually` waits at the most.
        scope.spawn(|| {
            for age in 2..80 {
                if done.load(Ordering::Relaxed) {
                    return;
                }
                host.write_metadata("work", &kept(&[(1, 0), (2, 1)], age));
                thread::sleep(Duration::from_millis(200));
            }
        });
        eventually("work has the pane opened", || {
            let panes = session(&host.host(), "work")?.panes.clone()?;
            (panes.panes.len() == 2).then_some(())
        });
        done.store(true, Ordering::Relaxed);
    });
    // zellij is given 2s for the panes of stuck, and as long for its clients.
    assert!(opening.elapsed() < Duration::from_secs(7), "{:?}", opening.elapsed());
}

/// What zellij 0.45 keeps of a session `age` seconds after it began, cut
/// down: each of its panes, by its id and the position of its tab.
fn kept(panes: &[(u32, u32)], age: u32) -> String {
    let pane = |&(id, tab): &(u32, u32)| {
        format!("    pane {{\n        id {id}\n        tab_position {tab}\n    }}\n")
    };
    format!("panes {{\n{}}}\ncreation_time {age}\n", panes.iter().map(pane).collect::<String>())
}

fn client(id: u32, pane: Option<u32>) -> api::Client {
    api::Client { id, pane }
}

/// A session has its clients once zellij lists them, none at first: each by
/// its id, and the terminal it has the focus on, or none for a plugin's pane.
/// One that moves its focus changes the host, and a `Watch` that waits for a
/// change answers with it.
#[test]
fn a_session_has_its_clients_once_zellij_lists_them() {
    let host = Host::new();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some("/home/you"))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();

    let none = eventually("zellij lists the clients of work", || {
        session(&host.host(), "work")?.clients.clone()
    });
    assert_eq!(none.clients, []);

    host.zellij.clients("work", &[(1, "terminal_1"), (2, "plugin_4")]);
    let (two, version) = eventually("zellij lists two clients of work", || {
        let now = host.host();
        let clients = session(&now, "work")?.clients.clone()?;
        (!clients.clients.is_empty()).then_some((clients, now.version?))
    });
    assert_eq!(two.clients, [client(1, Some(1)), client(2, None)]);

    let (moved, took) = thread::scope(|scope| {
        let waiting = scope.spawn(|| host.watch(Some(version)));
        thread::sleep(Duration::from_millis(300));
        host.zellij.clients("work", &[(1, "terminal_1"), (2, "terminal_1")]);
        waiting.join().unwrap()
    });
    let clients = session(&moved, "work").unwrap().clients.clone().unwrap();
    assert_eq!(clients.clients, [client(1, Some(1)), client(2, Some(1))]);
    assert!(took < Duration::from_secs(4), "{took:?}");
}

#[test]
fn a_row_a_command_quotes_is_no_client_of_the_session() {
    let command = "claude Fix this.\n\nzellij lists:\n2         terminal_4     N/A\nThanks";
    let mut quoting = listed(0, "~", (0, "Tab #1"), Some("/home/you"));
    quoting["pane_command"] = json!(command);
    let host = Host::new();
    host.zellij.list("work", &json!([quoting]));
    host.zellij.says_of_clients(
        "work",
        &format!(
            "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
             1         terminal_0     {command}\n\
             2         terminal_0     {command}\n"
        ),
    );
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();

    let clients = eventually("zellij lists the clients of work", || {
        session(&host.host(), "work")?.clients.clone()
    });
    assert_eq!(clients.clients, [client(1, Some(0)), client(2, Some(0))]);
}

/// What an agent says in a pane's title is on the host, without the glyph the
/// agent leads it with, so that the frame Claude Code turns there while it
/// works leaves the host as it was. A bare `Claude Code` is an agent's title
/// with no topic, and a title that leads with no such glyph, as a shell's, is
/// no agent's. The pane works nowhere zellij says, so that git's word on where
/// it works changes the host at no moment the test counts on. The test waits
/// out two sweeps.
#[test]
fn an_agent_s_title_is_on_the_host_without_the_frame_it_turns() {
    let host = Host::new();
    let titled = |title: &str| json!([listed(1, title, (0, "Tab #1"), None)]);
    host.zellij.list("work", &titled("◐ Fix the fold"));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    let said = |host: &api::Host| {
        let panes = session(host, "work")?.panes.clone()?;
        panes.panes[0].titled.clone()
    };
    // zellij lists the session's clients after its panes, which changes the
    // host once more as it first does.
    let topic_is = |what: &str, wanted: Option<&str>| {
        eventually(what, || {
            let now = host.host();
            session(&now, "work")?.clients.as_ref()?;
            (said(&now)?.topic.as_deref() == wanted).then_some(now.version.unwrap())
        })
    };

    let busy = topic_is("the agent's topic is on the host", Some("Fix the fold"));
    host.zellij.list("work", &titled("◑ Fix the fold"));
    let asked = host.zellij.asked().len();
    eventually("zellij lists the next frame", || {
        (host.zellij.asked().len() >= asked + 2).then_some(())
    });
    assert_eq!(host.host().version, Some(busy));

    host.zellij.list("work", &titled("✳ Claude Code"));
    let rested = topic_is("the agent has no topic", None);
    assert!(rested.seq > busy.seq);

    host.zellij.list("work", &titled("~"));
    eventually("a shell's title is no agent's", || {
        let panes = session(&host.host(), "work")?.panes.clone()?;
        panes.panes[0].titled.is_none().then_some(())
    });
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

    let mut plugin = listed(2, "luneta", (0, "Tab #1"), None);
    plugin.as_object_mut().unwrap().remove("is_plugin");
    host.zellij.list("work", &json!([shell, plugin]));
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

/// A session that no longer shares its name is asked about at once, however
/// lately it was before it came to share it: its panes went as it did, and
/// nothing else would have it asked about before the sweep.
#[test]
fn a_session_that_no_longer_shares_its_name_is_asked_about_at_once() {
    let host = Host::new();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some("/home/you"))]));
    let newer = host.sockets.path().join("contract_version_2");
    fs::create_dir(&newer).unwrap();
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("zellij lists the panes of work", || session(&host.host(), "work")?.panes.clone());

    host.zellij.asked_again("work");
    let _shared = UnixListener::bind(newer.join("work")).unwrap();
    eventually("neither session called work has panes", || {
        let now = host.host();
        let work: Vec<&api::Session> = now.sessions.iter().filter(|it| it.name == "work").collect();
        (work.len() == 2 && work.iter().all(|session| session.panes.is_none())).then_some(())
    });
    fs::remove_file(newer.join("work")).unwrap();
    let ended = Instant::now();
    eventually("work has its panes again", || session(&host.host(), "work")?.panes.clone());
    assert!(ended.elapsed() < Duration::from_secs(1), "{:?}", ended.elapsed());
}

/// A folder of zellij's sockets the daemon cannot read could hold a session
/// that runs: the host says how many it could not read, so that a client
/// takes nothing it leaves out for gone, and lists what it holds once it
/// reads again.
#[test]
fn the_host_says_how_many_of_zellij_s_folders_it_could_not_read() {
    let host = Host::new();
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("work runs", || session(&host.host(), "work").map(|_| ()));

    let mode = |mode| fs::set_permissions(host.contract(), Permissions::from_mode(mode)).unwrap();
    mode(0o000);
    let unread = eventually("the folder of work does not read", || {
        Some(host.host()).filter(|now| now.unread_sockets == Some(1))
    });
    mode(0o755);
    assert!(session(&unread, "work").is_none(), "{:?}", unread.sessions);
    eventually("work runs again, and every folder reads", || {
        let now = host.host();
        (now.unread_sockets == Some(0) && session(&now, "work").is_some()).then_some(())
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

    host.zellij.list("work", &json!([in_dir(1, &tree), in_dir(3, &detached)]));
    eventually("what git said of plain goes", || {
        let trees = host.host().work_trees;
        trees.iter().all(|tree| tree.dir != plain).then_some(())
    });
}

/// The daemon counts the lines changed on a branch against the branch its
/// repository takes as its default, work tree changes included, as a diff of
/// the pull request would show them: `origin/HEAD` says the default, where
/// the repository has it. The default branch itself counts none, nor does a
/// branch with nothing changed on it.
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
    // The clone knows the default only once it has heard of the branch it
    // names: `clone` of an empty repository sets none.
    assert!(git(&work, &["push", "-q", "origin", "main"]));
    assert!(git(&work, &["remote", "set-head", "origin", "main"]));
    let base = ["a\n", "b\n", "c\n"].concat();
    commit(&work, "base", &base);
    assert!(git(&work, &["push", "-q", "origin", "main"]));
    assert!(git(&work, &["switch", "-q", "-c", "issue-299"]));
    commit(&work, "base", "a\nB\n\n");
    // A change nothing committed holds: `git add -N` has the work tree
    // counted for it, as a diff of the pull request would show it.
    fs::write(work.join("new"), "d\n").unwrap();
    assert!(git(&work, &["add", "-N", "new"]));
    assert!(git(
        &other,
        &["clone", "-q", "--origin", "origin", &origin.display().to_string(), "."],
    ));
    assert!(git(&other, &["remote", "set-head", "origin", "main"]));

    let spelled = |dir: &PathBuf| dir.display().to_string();
    let (work, other) = (spelled(&work), spelled(&other));
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    host.zellij.list("work", &json!([in_dir(1, &work), in_dir(2, &other)]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();

    // The lines changed on `issue-299`: `b` became `B` and `c` an empty
    // line — one added and one deleted of each — and `new` one more added;
    // none on `main`, which is the repository's default.
    let changed = |dir: &str| {
        host.host().work_trees.iter().find(|tree| tree.dir == dir).and_then(|tree| {
            match tree.stands.as_ref()? {
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

    // A branch made from the default counts what it changed, and one back
    // on the default counts none.
    assert!(git(at(&other), &["switch", "-q", "-c", "fold"]));
    commit(at(&other), "new", "d\n");
    eventually("git counts the lines changed on fold", || {
        (changed(&other) == Some((1, 0))).then_some(())
    });
    assert!(git(at(&other), &["switch", "-q", "main"]));
    eventually("git counts none on main", || changed(&other).is_none().then_some(()));
}

#[test]
fn luneta_sessions_names_the_repository_each_pane_works_in() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let dirs = dirs.path().canonicalize().unwrap();
    let (seed, app, clone, plain) =
        (dirs.join("seed"), dirs.join("app"), dirs.join("clone"), dirs.join("plain"));
    for dir in [&seed, &app, &plain] {
        fs::create_dir(dir).unwrap();
    }
    assert!(git(&seed, &["init", "-q", "-b", "main"]));
    commit(&seed, "file", "text\n");
    let seeded = seed.display().to_string();
    assert!(git(&dirs, &["clone", "-q", &seeded, "clone"]));
    assert!(git(&app, &["clone", "-q", "--bare", &seeded, ".bare"]));
    fs::write(app.join(".git"), "gitdir: ./.bare\n").unwrap();
    assert!(git(&app, &["worktree", "add", "-q", "main"]));
    assert!(git(&app, &["worktree", "add", "-q", "-b", "other", "issue-1"]));

    let spelled = |dir: &Path| dir.display().to_string();
    let (repo, clone, plain) = (spelled(&app), spelled(&clone), spelled(&plain));
    let (main, other) = (format!("{repo}/main"), format!("{repo}/issue-1"));
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    let panes = [
        in_dir(1, &main),
        in_dir(2, &other),
        in_dir(3, &repo),
        in_dir(4, &clone),
        in_dir(5, &plain),
    ];
    host.zellij.list("work", &json!(panes));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("git says of all five", || (host.host().work_trees.len() == 5).then_some(()));

    let said = host.vars().command(LUNETA).arg("sessions").output().unwrap();
    let said = String::from_utf8(said.stdout).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines,
        [
            "work".to_owned(),
            format!("  Tab #1:1  {main}  main  repo {repo}"),
            format!("  Tab #1:2  {other}  other  repo {repo}"),
            format!("  Tab #1:3  {repo}  no checkout  repo {repo}"),
            format!("  Tab #1:4  {clone}  main  repo {clone}"),
            format!("  Tab #1:5  {plain}  no repository"),
        ],
    );
}

#[test]
fn luneta_sessions_lists_the_sessions_as_the_sidebar_does() {
    let host = Host::new();
    let _zeta = host.serve("zeta");
    thread::sleep(Duration::from_millis(50));
    let _alpha = host.serve("alpha");
    let _daemon = start(host.vars()).unwrap();
    eventually("both are listed", || (host.host().sessions.len() == 2).then_some(()));
    let listed = || {
        let said = host.vars().command(LUNETA).arg("sessions").output().unwrap();
        let said = String::from_utf8(said.stdout).unwrap();
        let first = |line: &str| line.split_whitespace().next().unwrap().to_owned();
        said.lines().map(first).collect::<Vec<String>>()
    };
    assert_eq!(listed(), ["zeta", "alpha"]);

    let kept = host.home.path().join(".local/state/luneta");
    fs::create_dir_all(&kept).unwrap();
    fs::write(kept.join("order"), r#"{"sessions":["alpha"]}"#).unwrap();
    assert_eq!(listed(), ["alpha", "zeta"]);
}

/// What git will not read is said nothing of, as it could be any repository:
/// a repository it refuses is not `no repository`, nor a work tree whose index
/// does not read `no checkout`. Why goes to the log. The directory named last
/// is asked about last, so once git has said of it, it has been asked of all.
#[test]
fn luneta_sessions_says_nothing_of_a_directory_git_will_not_read() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let dirs = dirs.path().canonicalize().unwrap();
    let (seed, plain) = (dirs.join("seed"), dirs.join("zz-plain"));
    for dir in [&seed, &plain] {
        fs::create_dir(dir).unwrap();
    }
    assert!(git(&seed, &["init", "-q", "-b", "main"]));
    commit(&seed, "file", "text\n");
    let seeded = seed.display().to_string();
    assert!(git(&dirs, &["clone", "-q", "--bare", &seeded, ".bare"]));
    assert!(git(&dirs, &["clone", "-q", &seeded, "broken"]));
    assert!(git(&dirs, &["clone", "-q", &seeded, "newer"]));
    fs::write(dirs.join("broken/.git/index"), "no index\n").unwrap();
    assert!(git(&dirs.join("newer"), &["config", "core.repositoryformatversion", "99"]));

    let spelled = |dir: &Path| dir.display().to_string();
    let (bare, broken) = (spelled(&dirs.join(".bare")), spelled(&dirs.join("broken")));
    let (newer, plain) = (spelled(&dirs.join("newer")), spelled(&plain));
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    let panes = [in_dir(1, &bare), in_dir(2, &broken), in_dir(3, &newer), in_dir(4, &plain)];
    host.zellij.list("work", &json!(panes));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
    eventually("git says of the last", || (!host.host().work_trees.is_empty()).then_some(()));

    let said = host.vars().command(LUNETA).arg("sessions").output().unwrap();
    let said = String::from_utf8(said.stdout).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(
        lines,
        [
            "work".to_owned(),
            format!("  Tab #1:1  {bare}"),
            format!("  Tab #1:2  {broken}"),
            format!("  Tab #1:3  {newer}"),
            format!("  Tab #1:4  {plain}  no repository"),
        ],
    );
    let logged = daemon.said_after(Duration::from_secs(1));
    for why in [
        format!("git did not say what repository {bare} is in"),
        format!("git status in {broken} said"),
        format!("git did not say what repository {newer} is in"),
    ] {
        assert!(logged.iter().any(|line| line.contains(&why)), "{why}: {logged:?}");
    }
}

/// What git said of a directory stands once it will not say it again, the
/// repository with it, as git not answering at all leaves it too.
#[test]
fn what_git_said_stands_once_it_will_not_read_the_repository() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    assert!(git(&dir, &["init", "-q", "-b", "main"]));
    commit(&dir, "file", "text\n");
    let cwd = dir.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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

/// How the work tree stands where a pane works in `cwd`, once git has said
/// that it is a checkout there.
fn checkout(host: &Host, cwd: &str) -> Option<Checkout> {
    let tree = host.host().work_trees.into_iter().find(|tree| tree.dir == cwd)?;
    match tree.stands? {
        Stands::Checkout(checkout) => Some(checkout),
        Stands::Outside(()) | Stands::NoCheckout(_) => None,
    }
}

/// Waits for git to say that `cwd`, where a pane works, is in no work tree.
fn in_no_work_tree(host: &Host, cwd: &str) {
    eventually("git says the directory is in no work tree", || {
        let tree = host.host().work_trees.into_iter().find(|tree| tree.dir == cwd)?;
        (tree.stands == Some(Stands::Outside(()))).then_some(())
    });
}

/// Whether the branch `name` is checked out.
fn on_branch(name: &'static str) -> impl Fn(&Checkout) -> bool {
    move |it| matches!(&it.head, Some(Head::Branch(branch)) if branch.name == name)
}

/// Has `change` made, and waits for the work tree to stand as `shows` takes
/// it to where a pane works in `cwd`: within 4s of the change's start, though
/// git would not be asked again on its sweep for 5s, as each change comes as
/// soon as git said the one before. The CI's Mac takes up to 2.7s, with each
/// test's daemon and gits at once on its 3 cores, and the daemon resting from
/// git as long as git took before it asks again.
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

/// git is asked again as the work tree a pane works in changes, well before
/// its sweep would have it asked: as a file is written in a directory git
/// tracks, or a directory turns up there, a path is staged, a commit made, a
/// stash made or dropped, and a branch checked out; as a file is written in a
/// directory the index has only since a commit, or in one made anew in the
/// place of one that went; and as another work tree of the repository
/// fetches where its upstream moved, which touches nothing of this one's own.
/// Each change comes as soon as git said the one before, when the sweep is
/// furthest off. The repository keeps its refs as `format` has it.
fn says_at_once_as_a_work_tree_changes(format: &str) {
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
    let src = spelled(&tree.join("src"));
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&src))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
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
fn git_says_at_once_as_a_work_tree_changes_that_keeps_its_refs_in_files() {
    says_at_once_as_a_work_tree_changes("files");
}

#[test]
fn git_says_at_once_as_a_work_tree_changes_that_keeps_its_refs_in_a_reftable() {
    says_at_once_as_a_work_tree_changes("reftable");
}

/// A directory of a work tree that becomes a work tree of its own, as `git
/// init` makes one there, is watched as that one from then on: a branch
/// checked out in it, which writes nothing but the `HEAD` in its own git
/// folder, shows at once.
#[test]
fn git_says_at_once_as_a_work_tree_made_in_another_changes() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    let (top, src) = (tree.path(), tree.path().join("src"));
    assert!(git(top, &["init", "-q", "-b", "main"]));
    fs::create_dir(&src).unwrap();
    commit(top, "src/lib.rs", "start\n");
    let cwd = src.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("git says how the tree stands", || checkout(&host, &cwd));

    let made = || assert!(git(&src, &["init", "-q", "-b", "inner"]));
    shows_at_once(&host, &cwd, "a work tree made in src", made, on_branch("inner"));
    let switched = || assert!(git(&src, &["switch", "-q", "-c", "other"]));
    shows_at_once(&host, &cwd, "a branch checked out in it", switched, on_branch("other"));
}

/// A `.git` folder in a directory of a work tree that does not read, as one
/// another user made there, is passed by as git passes it by: the work tree
/// is watched all the same, and an edit beside it shows at once.
#[test]
fn a_git_folder_that_does_not_read_leaves_the_work_tree_it_is_in_watched() {
    let host = Host::new();
    let tree = tempfile::tempdir().unwrap();
    let (top, src) = (tree.path(), tree.path().join("src"));
    assert!(git(top, &["init", "-q", "-b", "main"]));
    fs::create_dir(&src).unwrap();
    commit(top, "src/lib.rs", "start\n");
    let unread = src.join(".git");
    let mode = |mode| fs::set_permissions(&unread, Permissions::from_mode(mode)).unwrap();
    fs::create_dir(&unread).unwrap();
    mode(0o000);
    let cwd = src.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("git says how the tree stands", || checkout(&host, &cwd));

    let modified = |it: &Checkout| it.changes.unwrap_or_default().modified == 1;
    let edited = || fs::write(src.join("lib.rs"), "edited\n").unwrap();
    shows_at_once(&host, &cwd, "an edit beside it", edited, modified);
    mode(0o755);
}

/// A directory in no work tree that becomes one, as `git init` makes one
/// there, is watched as that one from then on: the work tree shows at once,
/// and so does a branch checked out in it, which writes nothing but the
/// `HEAD` in its own git folder.
#[test]
fn git_says_at_once_as_a_work_tree_begins_where_there_was_none() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    in_no_work_tree(&host, &cwd);

    let made = || assert!(git(dir.path(), &["init", "-q", "-b", "first"]));
    let first = on_branch("first");
    shows_at_once(&host, &cwd, "a work tree made where there was none", made, first);
    let switched = || assert!(git(dir.path(), &["switch", "-q", "-c", "other"]));
    shows_at_once(&host, &cwd, "a branch checked out in it", switched, on_branch("other"));
}

/// A directory in no work tree that is taken away and made again where a
/// pane works is watched as the one made: git is asked about it again at
/// once, and a work tree that `git init` makes in it after shows at once.
#[test]
fn git_says_at_once_as_a_work_tree_begins_in_a_directory_made_again() {
    let host = Host::new();
    let noted = host.note_git();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    fs::create_dir(&work).unwrap();
    let cwd = work.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    in_no_work_tree(&host, &cwd);

    let (asked, started) = (noted.statuses(), Instant::now());
    fs::remove_dir(&work).unwrap();
    fs::create_dir(&work).unwrap();
    eventually("git is asked again", || (noted.statuses() > asked).then_some(()));
    assert!(started.elapsed() < Duration::from_secs(4), "asked again: {:?}", started.elapsed());
    let made = || assert!(git(&work, &["init", "-q", "-b", "first"]));
    shows_at_once(&host, &cwd, "a work tree made in it", made, on_branch("first"));
}

#[test]
fn a_checkout_taken_away_is_in_no_work_tree() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    fs::create_dir(&work).unwrap();
    assert!(git(&work, &["init", "-q", "-b", "main"]));
    commit(&work, "file", "text\n");
    let cwd = work.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("git says it is a checkout", || checkout(&host, &cwd));

    fs::remove_dir_all(&work).unwrap();
    in_no_work_tree(&host, &cwd);
}

/// A pane that works in a repository's own folder, which git places in no
/// work tree, has git asked nothing as git writes there, as a commit does.
#[test]
fn a_repository_folder_has_git_asked_nothing_as_git_writes_there() {
    let host = Host::new();
    let noted = host.note_git();
    let tree = tempfile::tempdir().unwrap();
    let top = tree.path();
    assert!(git(top, &["init", "-q", "-b", "main"]));
    let cwd = top.join(".git").display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    let repository = fs::canonicalize(top).unwrap().display().to_string();
    let of_its_clone = Some(Stands::NoCheckout(NoCheckout { repository }));
    eventually("git says it is no checkout of its clone", || {
        let tree = host.host().work_trees.into_iter().find(|tree| tree.dir == cwd)?;
        (tree.stands == of_its_clone).then_some(())
    });

    let asked = noted.statuses();
    for step in 0..5 {
        commit(top, "lib.rs", &format!("{step}\n"));
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(noted.statuses(), asked, "git was asked as git wrote in its folder");
}

/// A directory that moves into a work tree, in the place of one git tracks
/// that moved away, is watched as one made anew there is: an edit in it shows
/// at once.
#[test]
fn git_says_at_once_as_a_directory_moved_into_a_work_tree_changes() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let path = |name: &str| dirs.path().join(name);
    let top = path("repo");
    fs::create_dir_all(top.join("src")).unwrap();
    assert!(git(&top, &["init", "-q", "-b", "main"]));
    commit(&top, "src/lib.rs", "start\n");
    let cwd = top.display().to_string();
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
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

/// What git ignores, the daemon does not watch, nor where a link among git's
/// refs leads, which could be anywhere: a build that writes in `target` all
/// along has git asked nothing, though a link to `target` is among the refs,
/// and a file that turns up beside it has git asked at once.
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
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

/// git runs no command that a repository's own config names, though the
/// daemon asks it about any directory a pane works in, trusted or not: not
/// the one `core.fsmonitor` names, which git runs as it reads the index, as
/// it does to say which directories to watch and how the work tree stands.
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
    eventually("git says how the tree stands", || checkout(&host, &cwd));
    assert!(!ran.exists(), "git ran the command the repository named");
    // As it would have, let read the index.
    assert!(git(&top, &["ls-files"]));
    assert!(ran.exists(), "git runs no command the repository names here");
}

/// gh is asked for the pull request of the branch checked out where each pane
/// works: once for a branch of a repository, however many of its directories
/// panes work in, and at once as a pane comes to a branch. What gh found of a
/// branch is no longer a directory's once another is checked out there, and
/// a branch with no pull request has none, which is no trouble to the daemon.
/// Each comes with its page, which says the repository its number counts in,
/// and how the checks of its head stand, as gh says them with it.
#[test]
fn gh_is_asked_once_for_the_pull_request_of_each_branch() {
    let host = Host::new();
    let dirs = tempfile::tempdir().unwrap();
    let (tree, other) = (dirs.path().join("tree"), dirs.path().join("other"));
    fs::create_dir_all(tree.join("src")).unwrap();
    fs::create_dir(&other).unwrap();
    for dir in [&tree, &other] {
        assert!(git(dir, &["init", "-q", "-b", "main"]));
        commit(dir, "README", "start\n");
    }
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
    let (top, src, beside) = (spelled(&tree), spelled(&tree.join("src")), spelled(&other));
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    host.zellij.list("work", &json!([in_dir(1, &top), in_dir(2, &src), in_dir(3, &beside)]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    eventually("gh finds fold's pull request in both of tree's", || {
        let found = found();
        (found.get(&top) == Some(&draft) && found.get(&src) == Some(&draft)).then_some(())
    });
    assert_eq!(found().get(&beside), Some(&open));
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

/// A branch gh does not answer for holds up no other: gh is asked about the
/// others as it hangs, and what it finds of them shows at once.
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
    let in_dir = |id, dir: &str| listed(id, "~", (0, "Tab #1"), Some(dir));
    host.zellij.list("work", &json!([in_dir(1, &stuck), in_dir(2, &other)]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();

    eventually("gh is asked about stuck", || {
        host.gh.asked().iter().any(|branch| branch == "stuck").then_some(())
    });
    eventually("main's pull request shows as gh hangs on stuck", || {
        let pull_requests = host.host().pull_requests;
        pull_requests.iter().any(|it| it.dir == other && it.number == 7).then_some(())
    });
    host.gh.answer("stuck");
}

/// gh is asked again soon about a branch whose pull request has checks yet
/// to end, as they are what change. Where it fails, what it said before
/// stands, as stale, until it answers again. The daemon says how the checks
/// changed, and how long it was since gh had said them, failing in between.
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
    let shown = || {
        let pull_requests = host.host().pull_requests;
        pull_requests.into_iter().find(|it| it.dir == cwd).map(|it| (it.checks, it.stale))
    };
    let soon = |what: &str, wanted| {
        let deadline = Instant::now() + Duration::from_secs(25);
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let _daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();

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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
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

/// gh finds the pull request of the branch checked out as it is asked, which
/// git can have yet to see: git looks again only every few seconds. What gh
/// says then is of the branch checked out since, and never shows under the
/// one before, nor is it trouble to the daemon. Once git sees the checkout,
/// the new branch has its pull request.
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
    host.zellij.list("work", &json!([listed(1, "~", (0, "Tab #1"), Some(&cwd))]));
    let _work = host.serve("work");
    let mut daemon = start(host.vars()).unwrap();
    // The branch git says is checked out in the tree, and the number of the
    // pull request listed there.
    let seen = || {
        let host = host.host();
        let tree = host.work_trees.iter().find(|tree| tree.dir == cwd);
        let branch = match tree.and_then(|tree| tree.stands.as_ref()) {
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
