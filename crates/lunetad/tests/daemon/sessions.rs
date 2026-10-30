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

use lunetad::api::checkout::{Detached, Head};
use lunetad::api::work_tree::{Outside, Stands};
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

    host.zellij.clients("work", &[(2, "plugin_4"), (1, "terminal_1")]);
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

/// How the work tree stands where a pane works in `cwd`, once git has said
/// that it is a checkout there.
fn checkout(host: &Host, cwd: &str) -> Option<Checkout> {
    let tree = host.host().work_trees.into_iter().find(|tree| tree.dir == cwd)?;
    match tree.stands? {
        Stands::Checkout(checkout) => Some(checkout),
        Stands::Outside(_) => None,
    }
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

    let on = |name: &'static str| {
        move |it: &Checkout| matches!(&it.head, Some(Head::Branch(branch)) if branch.name == name)
    };
    let made = || assert!(git(&src, &["init", "-q", "-b", "inner"]));
    shows_at_once(&host, &cwd, "a work tree made in src", made, on("inner"));
    let switched = || assert!(git(&src, &["switch", "-q", "-c", "other"]));
    shows_at_once(&host, &cwd, "a branch checked out in it", switched, on("other"));
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
/// Each comes with its page, which says the repository its number counts in.
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
    let main = json!({ "number": 7, "state": "OPEN", "isDraft": false, "url": page(7) });
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
