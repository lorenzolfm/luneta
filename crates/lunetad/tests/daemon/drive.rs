//! What the CLI has zellij do, for an agent that drives luneta: a close says
//! what goes and which agents end with it, and closes only that, a tab opens
//! only where it has room, and a session only from one that runs. What goes
//! by the session it runs in goes by the name lunetad says it has now, and
//! does nothing where luneta cannot tell which that is.

use std::fs;
use std::os::unix::net::UnixListener;
use std::process::Output;

use serde_json::json;

use crate::agents::{file_for, now_in, process, Host};
use crate::host::{answered, eventually, start, LUNETA};
use crate::sessions::listed;

/// The CLI, run with `args` in the pane `here`, as an agent there runs it.
fn luneta(host: &Host, here: (&str, u32), args: &[&str]) -> Output {
    let mut command = host.vars().command(LUNETA);
    command.env("ZELLIJ_SESSION_NAME", here.0).env("ZELLIJ_PANE_ID", here.1.to_string());
    answered(command.args(args))
}

/// What a close exits with where it closes nothing, as it was not told yes to
/// the plan it says.
const PLANNED: i32 = 3;

/// What a close says it would close, and the plan it tells `--yes` that by.
fn planned(said: &Output) -> (String, String) {
    assert_eq!(said.status.code(), Some(PLANNED), "{}", String::from_utf8_lossy(&said.stderr));
    let why = String::from_utf8_lossy(&said.stderr);
    let plan = why.split_once("--yes ").and_then(|(_, after)| after.split_whitespace().next());
    let plan = plan.unwrap_or_else(|| panic!("a close tells --yes its plan: {why}")).to_owned();
    (String::from_utf8(said.stdout.clone()).unwrap(), plan)
}

/// What a close says of the agents it ends while no lunetad runs.
const UNTOLD: &str = "luneta cannot tell which agents it ends: lunetad does not answer";

/// A close says what each pane and session it is given takes along, and the
/// agents that end with it, and closes nothing until it is told `--yes`. Then
/// what holds the pane it runs in goes last, however it was given: the agent
/// that ran it ends with it.
#[test]
fn a_close_says_what_goes_and_closes_it_once_told_yes_where_it_runs_last() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    // The shell in pane 3 keeps the tab open once pane 2 closes.
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    host.lists.list("play", &json!([listed(1, "claude", tab, Some("/p"))]));
    let here = process(Some(("work", 2)));
    host.write(here.pid(), &file_for("here", &here, "busy"));
    let there = process(Some(("play", 1)));
    host.write(there.pid(), &file_for("there", &there, "idle"));
    let _daemon = start(host.vars()).unwrap();
    eventually("the daemon has both agents in their sessions", || {
        let agents = host.agents();
        (agents.len() == 2 && agents.iter().all(|agent| now_in(agent).is_some())).then_some(())
    });

    let asked = luneta(&host, ("work", 2), &["close", "work:2", "play"]);
    let (said, agreed) = planned(&asked);
    let (here, there) = (here.pid(), there.pid());
    let plan = format!(
        "delete session play: 1 agent\n  \
         play:1  idle     {there:>7}  there-name\n\
         close pane work:2: 1 agent, 1 busy\n  \
         work:2  busy     {here:>7}  here-name  (this runs here)\n"
    );
    assert_eq!(said, plan);
    assert!(String::from_utf8_lossy(&asked.stderr).contains("closed nothing"));
    assert_eq!(host.lists.closed(), [] as [String; 0]);

    let told = luneta(&host, ("work", 2), &["close", "work:2", "play", "--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(String::from_utf8(told.stdout).unwrap(), plan);
    let closed =
        ["delete-session --force play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);
}

/// The agent of a session file lunetad cannot read is in the pane its
/// process runs in, doing what no one can tell: a close of that pane says it
/// ends it, and lists it by its pid, and a close of another does not. One
/// whose session lunetad cannot tell may be in any pane: a close says it
/// cannot tell which agents it ends, and why, and lists those it can tell.
#[test]
fn a_close_counts_the_agent_of_a_session_file_lunetad_cannot_read() {
    let host = Host::new();
    let work = host.contract().join("work");
    let _work = UnixListener::bind(&work).unwrap();
    let tab = (0, "Tab #1");
    let panes = json!([listed(2, "claude", tab, Some("/w")), listed(3, "claude", tab, Some("/w"))]);
    host.lists.list("work", &panes);
    let here = process(Some(("work", 2)));
    host.write(here.pid(), &file_for("here", &here, "busy"));
    let unread = process(Some(("work", 3)));
    let unnamed = |running| {
        let mut file = file_for("unread", running, "busy");
        file["sessionId"] = json!("");
        file
    };
    host.write(unread.pid(), &unnamed(&unread));
    let _daemon = start(host.vars()).unwrap();
    let (here, unread) = (here.pid(), unread.pid());

    let (said, _) = planned(&luneta(&host, ("work", 2), &["close", "work:2"]));
    let plan = format!(
        "close pane work:2: 1 agent, 1 busy\n  \
         work:2  busy     {here:>7}  here-name  (this runs here)\n"
    );
    assert_eq!(said, plan);
    let (said, _) = planned(&luneta(&host, ("work", 2), &["close", "work:3"]));
    let plan = format!(
        "close pane work:3: 1 agent, 1 unknown\n  \
         work:3  ?        {unread:>7}  its session file does not read\n"
    );
    assert_eq!(said, plan);

    // In a session no socket is for, which lunetad cannot follow it to. The
    // close says it cannot read both files, as its stderr does, and cannot
    // place one of them.
    let lost = process(Some(("gone", 4)));
    host.write(lost.pid(), &unnamed(&lost));
    eventually("the daemon cannot read two files", || (host.list().unread == 2).then_some(()));
    let (said, _) = planned(&luneta(&host, ("work", 2), &["close", "work:2"]));
    let plan = format!(
        "close pane work:2: luneta cannot tell which agents it ends: lunetad cannot read 2 of \
         Claude Code's session files, nor tell where 1 of their agents runs\n  \
         work:2  busy     {here:>7}  here-name  (this runs here)\n"
    );
    assert_eq!(said, plan);
}

/// What a close is given in a session closes together: the panes that are
/// all that holds a tab open close the tab, once, and a session given takes
/// along the panes of it given too.
#[test]
fn a_close_closes_what_it_is_given_in_a_session_together() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let (tab, other) = ((0, "Tab #1"), (1, "Tab #2"));
    let work = json!([
        listed(2, "claude", tab, Some("/w")),
        listed(3, "fish", tab, Some("/w")),
        listed(4, "fish", other, Some("/w")),
    ]);
    host.lists.list("work", &work);
    host.lists.list("play", &json!([listed(1, "fish", tab, Some("/p"))]));

    let given = ["close", "work:2", "play:1", "work:3", "play", "work:2"];
    let (said, agreed) = planned(&luneta(&host, ("work", 4), &given));
    let plan =
        format!("delete session play: {UNTOLD}\nclose the tab of work:2, work:3: {UNTOLD}\n");
    assert_eq!(said, plan);

    let told = luneta(&host, ("work", 4), &[&given[..], &["--yes", &agreed]].concat());
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let closed = ["delete-session --force play", "--session work action close-tab-by-id 0"];
    assert_eq!(host.lists.closed(), closed);
}

/// A close told yes to a plan closes nothing where what it would close is
/// not that plan now: it says what it would close instead, and the plan to
/// tell `--yes`. A session made again by the same name is another plan, its
/// panes numbered as the last one's were.
#[test]
fn a_close_closes_only_the_plan_it_was_told_yes_to() {
    let host = Host::new();
    let socket = host.contract().join("work");
    let work = UnixListener::bind(&socket).unwrap();
    // The session the close runs in.
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let tab = (0, "Tab #1");
    let both = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &both);
    let close = |yes: &[&str]| luneta(&host, ("play", 1), &[&["close", "work:2"], yes].concat());
    let (said, agreed) = planned(&close(&[]));
    assert_eq!(said, format!("close pane work:2: {UNTOLD}\n"));

    // The shell in pane 3 ends, which leaves pane 2 all that holds the
    // session's only tab open.
    host.lists.list("work", &json!([listed(2, "claude", tab, Some("/w"))]));
    let (now, _) = planned(&close(&["--yes", &agreed]));
    assert_eq!(now, format!("delete session work: {UNTOLD}\n"));

    host.lists.list("work", &both);
    drop(work);
    fs::remove_file(&socket).unwrap();
    let _work = UnixListener::bind(&socket).unwrap();
    let (again, _) = planned(&close(&["--yes", &agreed]));
    assert_eq!(again, said);
    assert_eq!(host.lists.closed(), [] as [String; 0]);
}

/// zellij makes a tab as big as a client of its session, and one with no room
/// for its panes in a session with none, where a client sent to it later has
/// nothing to show. So no tab opens in a session no client is in.
#[test]
fn a_tab_opens_only_in_a_session_a_client_is_in() {
    let host = Host::new();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    host.lists.list("play", &json!([listed(1, "fish", (0, "Tab #1"), Some("/p"))]));
    let cwd = tempfile::tempdir().unwrap();
    let cwd = cwd.path().to_str().unwrap();

    let asked = luneta(&host, ("work", 2), &["open", "tab", "--session", "play", "--cwd", cwd]);
    assert!(!asked.status.success());
    let why = String::from_utf8_lossy(&asked.stderr);
    assert!(why.contains("no client is in play"), "{why}");
}

/// A terminal in the tab `tab`, as zellij lists it.
fn pane(id: u32, tab: (u32, &str)) -> serde_json::Value {
    listed(id, "fish", tab, Some("/w"))
}

/// A plugin's pane, as zellij lists it, by an id of the plugins'.
fn plugin(mut pane: serde_json::Value) -> serde_json::Value {
    pane["is_plugin"] = json!(true);
    pane
}

/// The session `work`, its panes 1 and 2 in one tab, where zellij moves pane 1
/// to a tab it opens, as `luneta turn pane work:1` has it do. It runs as long as
/// what this gives back is held.
fn pane_to_turn(host: &Host) -> UnixListener {
    let work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (old, new) = ((0, "Tab #1"), (3, "Tab #2"));
    host.lists.list("work", &json!([pane(1, old), pane(2, old)]));
    host.lists.opens_into("work", &json!([pane(1, old), pane(2, old), pane(4, new)]));
    host.lists.stacks_into("work", 1, &json!([pane(2, old), pane(1, new), pane(4, new)]));
    work
}

/// A pane zellij lists as shown over the others of its tab.
fn fullscreen(mut pane: serde_json::Value) -> serde_json::Value {
    pane["is_fullscreen"] = json!(true);
    pane
}

/// What has zellij stack `moved` on `root` with no client moved: it focuses the
/// last it is told to stack, and that is none of the session's.
fn unfocused(root: &str, moved: &str) -> String {
    format!("--session work action stack-panes -- {root} {moved} terminal_4294967295")
}

/// What a command said on its stdout, having gone well.
fn said(ran: &Output) -> String {
    assert!(ran.status.success(), "{}", String::from_utf8_lossy(&ran.stderr));
    String::from_utf8(ran.stdout.clone()).unwrap()
}

/// What a command said on its stderr, having failed.
fn why(ran: &Output) -> String {
    assert!(!ran.status.success(), "{}", String::from_utf8_lossy(&ran.stdout));
    String::from_utf8_lossy(&ran.stderr).into_owned()
}

/// An agent turns the pane it runs in, whose variables zellij must not go by.
/// It says that pane, which keeps its id. zellij focuses the last pane it is told
/// to stack, and here that is none of the session's, so that it takes no client
/// there, and what you type goes on reaching your pane.
#[test]
fn a_pane_turns_into_a_tab_and_no_client_moves() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    host.lists.clients("work", &[(1, "terminal_2")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_1")]);

    let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    assert_eq!(String::from_utf8_lossy(&turned.stderr), "");
    assert_eq!(host.lists.turned(), [unfocused("terminal_4", "terminal_1")]);
    assert!(host.lists.went().is_empty(), "{:?}", host.lists.went());
    let opened = host.lists.opened();
    assert!(opened.iter().all(|opened| opened.starts_with("- ")), "{opened:?}");
}

/// A tab with room for a pane but not for one more beside it has zellij stack it
/// the plain way, as it would have had no room for neither: the pane moves all the
/// same, and the client zellij takes there goes back.
#[test]
fn a_pane_with_little_room_moves_all_the_same_and_the_client_goes_back() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    host.lists.leave_little_room("work");
    host.lists.clients("work", &[(1, "terminal_2")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_1")]);
    host.lists.focuses_clients_into("work", &[(1, "terminal_2")]);

    let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    assert_eq!(String::from_utf8_lossy(&turned.stderr), "");
    let plain = "--session work action stack-panes -- terminal_4 terminal_1";
    assert_eq!(host.lists.turned(), [unfocused("terminal_4", "terminal_1"), plain.to_owned()]);
    assert_eq!(host.lists.went(), ["--session work action focus-pane-id terminal_2"]);
}

/// A client is on the pane that moves, so that it is to go with it, and zellij
/// is told nothing else to focus: but it takes the client that last typed, here
/// another. That one goes back, and the turn says of the one it left behind.
#[test]
fn a_client_zellij_took_to_a_pane_another_was_on_goes_back() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    host.lists.clients("work", &[(1, "terminal_2"), (2, "terminal_1")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_1"), (2, "terminal_2")]);
    host.lists.focuses_clients_into("work", &[(1, "terminal_2"), (2, "terminal_2")]);

    let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    let plain = "--session work action stack-panes -- terminal_4 terminal_1";
    assert_eq!(host.lists.turned(), [plain]);
    assert_eq!(host.lists.went(), ["--session work action focus-pane-id terminal_2"]);
    let warned = String::from_utf8_lossy(&turned.stderr);
    assert!(warned.contains("may not be on the pane they were on"), "{warned}");
}

/// Of the panes of a tab, only the one a client is on is stacked for that client
/// to go with it, and the rest take it no further.
#[test]
fn a_client_goes_with_its_pane_and_no_further_as_a_tab_turns() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    let (three, there) = (pane(3, to), |id| pane(id, to));
    host.lists.list("work", &json!([pane(1, from), pane(2, from), pane(5, from), three]));
    host.lists.stacks_into("work", 1, &json!([there(1), pane(2, from), pane(5, from), three]));
    host.lists.stacks_into("work", 2, &json!([there(1), there(2), pane(5, from), three]));
    host.lists.stacks_into("work", 3, &json!([there(1), there(2), there(5), three]));
    host.lists.clients("work", &[(1, "terminal_2")]);

    let turned = luneta(&host, ("work", 1), &["turn", "tab", "work:1", "--into", "work:3"]);

    assert_eq!(said(&turned), "work:1\n");
    assert_eq!(String::from_utf8_lossy(&turned.stderr), "");
    let plain = "--session work action stack-panes -- terminal_9 terminal_2";
    let (one, five) = (unfocused("terminal_9", "terminal_1"), unfocused("terminal_9", "terminal_5"));
    assert_eq!(host.lists.turned(), [one, plain.to_owned(), five]);
    assert!(host.lists.went().is_empty(), "{:?}", host.lists.went());
}

/// A pane leaving its tab ends the fullscreen of another there, and a stand-in
/// opening in a tab that of a pane there. Each shows so again, once the turn is
/// over, with nobody moved.
#[test]
fn a_turn_shows_fullscreen_again_what_it_ended() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (old, new) = ((0, "Tab #1"), (3, "Tab #2"));
    host.lists.list("work", &json!([pane(1, old), fullscreen(pane(2, old))]));
    host.lists.opens_into("work", &json!([pane(1, old), fullscreen(pane(2, old)), pane(4, new)]));
    host.lists.stacks_into("work", 1, &json!([pane(2, old), pane(1, new), pane(4, new)]));
    host.lists.clients("work", &[(1, "terminal_2")]);

    let turned = luneta(&host, ("work", 2), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    let toggled = "--session work action toggle-fullscreen --pane-id terminal_2";
    assert_eq!(host.lists.turned(), [unfocused("terminal_4", "terminal_1"), toggled.to_owned()]);

    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.lists.list("work", &json!([pane(1, from), fullscreen(pane(3, to)), pane(5, to)]));
    host.lists.stacks_into("work", 1, &json!([pane(1, to), pane(3, to), pane(5, to)]));
    host.lists.clients("work", &[(1, "terminal_3")]);

    let turned = luneta(&host, ("work", 3), &["turn", "tab", "work:1", "--into", "work:3"]);

    assert_eq!(said(&turned), "work:1\n");
    let toggled = "--session work action toggle-fullscreen --pane-id terminal_3";
    assert_eq!(host.lists.turned(), [unfocused("terminal_9", "terminal_1"), toggled.to_owned()]);
}

/// A pane stays out of fullscreen where the turn did not end that, or where it
/// moved along itself, or where showing it would take a client off its pane:
/// here one that went with its pane into the tab.
#[test]
fn a_turn_leaves_fullscreen_as_it_is_where_it_ended_none_or_would_move_a_client() {
    let (old, new) = ((0, "Tab #1"), (3, "Tab #2"));
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    host.lists.list("work", &json!([pane(1, old), fullscreen(pane(2, old))]));
    host.lists.opens_into("work", &json!([pane(1, old), fullscreen(pane(2, old)), pane(4, new)]));
    let still = json!([fullscreen(pane(2, old)), pane(1, new), pane(4, new)]);
    host.lists.stacks_into("work", 1, &still);
    host.lists.clients("work", &[(1, "terminal_2")]);
    said(&luneta(&host, ("work", 2), &["turn", "pane", "work:1"]));
    assert_eq!(host.lists.turned(), [unfocused("terminal_4", "terminal_1")]);

    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    host.lists.list("work", &json!([fullscreen(pane(1, old)), pane(2, old)]));
    host.lists.opens_into("work", &json!([fullscreen(pane(1, old)), pane(2, old), pane(4, new)]));
    host.lists.stacks_into("work", 1, &json!([pane(2, old), pane(1, new), pane(4, new)]));
    host.lists.clients("work", &[(1, "terminal_2")]);
    said(&luneta(&host, ("work", 2), &["turn", "pane", "work:1"]));
    assert_eq!(host.lists.turned(), [unfocused("terminal_4", "terminal_1")]);

    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.lists.list("work", &json!([pane(1, from), fullscreen(pane(3, to)), pane(5, to)]));
    host.lists.stacks_into("work", 1, &json!([pane(1, to), pane(3, to), pane(5, to)]));
    host.lists.clients("work", &[(1, "terminal_1")]);
    said(&luneta(&host, ("work", 1), &["turn", "tab", "work:1", "--into", "work:3"]));
    let plain = "--session work action stack-panes -- terminal_9 terminal_1";
    assert_eq!(host.lists.turned(), [plain]);
}

/// A client on the pane that moves goes with it, and one that typed nothing is
/// not moved: neither has anywhere to go back to.
#[test]
fn a_turn_leaves_a_client_zellij_did_not_take_elsewhere() {
    for on in ["terminal_1", "terminal_2"] {
        let host = Host::new();
        let _work = pane_to_turn(&host);
        host.lists.clients("work", &[(1, on)]);

        let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

        assert_eq!(said(&turned), "work:1\n");
        assert!(host.lists.went().is_empty(), "{:?}", host.lists.went());
    }
}

/// A client goes back to a plugin's pane as to a terminal, by the id it has of
/// the plugins': here the same as that of the terminal that moved.
#[test]
fn a_turn_takes_a_client_back_to_a_plugins_pane() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    host.lists.leave_little_room("work");
    host.lists.clients("work", &[(1, "plugin_1")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_1")]);
    host.lists.focuses_clients_into("work", &[(1, "plugin_1")]);

    let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    assert_eq!(String::from_utf8_lossy(&turned.stderr), "");
    assert_eq!(host.lists.went(), ["--session work action focus-pane-id plugin_1"]);
}

/// A tab whose one tiled pane is a plugin's has it move last, and the client
/// zellij took ends on it: from a plugin's pane too, which is another pane, and
/// it goes back there.
#[test]
fn a_client_zellij_took_from_a_plugins_pane_to_another_goes_back() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to, third) = ((0, "Tab #1"), (1, "Tab #2"), (2, "Tab #3"));
    let mut floats = pane(1, from);
    floats["is_floating"] = json!(true);
    let stays = plugin(pane(5, third));
    let list = json!([plugin(pane(2, from)), floats, pane(3, to), pane(4, third), stays]);
    host.lists.list("work", &list);
    let moved_one = json!([plugin(pane(2, from)), pane(1, to), pane(3, to), pane(4, third), stays]);
    let moved_both = json!([plugin(pane(2, to)), pane(1, to), pane(3, to), pane(4, third), stays]);
    host.lists.stacks_into("work", 1, &moved_one);
    host.lists.stacks_into("work", 2, &moved_both);
    host.lists.leave_little_room("work");
    host.lists.clients("work", &[(1, "plugin_5")]);
    host.lists.stacks_clients_into("work", &[(1, "plugin_2")]);
    host.lists.focuses_clients_into("work", &[(1, "plugin_5")]);

    let turned = luneta(&host, ("work", 4), &["turn", "tab", "work:1", "--into", "work:3"]);

    assert_eq!(said(&turned), "work:1\n");
    assert_eq!(String::from_utf8_lossy(&turned.stderr), "");
    assert_eq!(host.lists.went(), ["--session work action focus-pane-id plugin_5"]);
}

/// What turned stays turned where a client has no way back, as one that typed
/// nothing, whose pane left without it: the agent is told, for it to tell the
/// user.
#[test]
fn a_turn_says_where_it_could_not_take_a_client_back() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    host.lists.clients("work", &[(1, "terminal_1")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_2")]);

    let turned = luneta(&host, ("work", 1), &["turn", "pane", "work:1"]);

    assert_eq!(said(&turned), "work:1\n");
    let warned = String::from_utf8_lossy(&turned.stderr);
    assert!(warned.contains("may not be on the pane they were on"), "{warned}");
}

/// A turn refused before it asked zellij to move anything moved nobody: a client
/// that goes elsewhere as it runs, the pane it was given too, went there of its
/// own, and stays there, and the turn says nothing of panes moving.
#[test]
fn a_refused_turn_leaves_a_client_that_moves_as_it_runs() {
    let alone: &[&str] = &["turn", "pane", "work:3"];
    let beside: &[&str] = &["turn", "tab", "work:1", "--into", "work:2"];
    let cases = [
        (alone, "terminal_1", "terminal_3"),
        (alone, "terminal_1", "terminal_2"),
        (beside, "terminal_3", "terminal_1"),
    ];
    for (refused, was, goes) in cases {
        let host = Host::new();
        let _work = UnixListener::bind(host.contract().join("work")).unwrap();
        let (one, two) = ((0, "Tab #1"), (1, "Tab #2"));
        host.lists.list("work", &json!([pane(1, one), pane(2, one), pane(3, two)]));
        host.lists.clients("work", &[(1, was)]);
        host.lists.lists_clients_into("work", &[(1, goes)]);

        let turned = luneta(&host, ("work", 3), refused);

        let why = why(&turned);
        assert!(why.contains("already") && !why.contains("may not be on"), "{refused:?} {why}");
        assert!(host.lists.went().is_empty(), "{refused:?} {:?}", host.lists.went());
    }
}

/// A session no client is in has no room for a tab, and nobody is taken there
/// to make it, as `open tab` takes nobody. What `lunetad::turns` refuses reaches
/// the agent as a failure too.
#[test]
fn a_pane_that_cannot_turn_into_a_tab_fails_and_says_why() {
    let host = Host::new();
    let _work = pane_to_turn(&host);
    let turned = luneta(&host, ("elsewhere", 7), &["turn", "pane", "work:1"]);
    assert!(why(&turned).contains("no client is in work"), "{turned:?}");
    assert_eq!(host.lists.opened(), [] as [String; 0]);

    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    host.lists.list("work", &json!([pane(1, (0, "Tab #1")), pane(2, (1, "Tab #2"))]));
    host.lists.clients("work", &[(1, "terminal_2")]);
    let turned = luneta(&host, ("work", 2), &["turn", "pane", "work:1"]);
    assert!(why(&turned).contains("pane 1 is alone in its tab"), "{turned:?}");
    assert_eq!(host.lists.opened(), [] as [String; 0]);
}

/// `--into` names a tab by a pane of it. A pane of another session is refused
/// before anything moves, and so is one of the same tab.
#[test]
fn a_tab_turns_into_panes_only_of_another_tab_of_its_session() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let tab = (0, "Tab #1");
    host.lists.list("work", &json!([pane(1, tab), pane(2, tab), pane(3, (1, "Tab #2"))]));

    let elsewhere = ["turn", "tab", "work:1", "--into", "play:3"];
    let turned = luneta(&host, ("work", 1), &elsewhere);
    assert!(why(&turned).contains("play:3 is of another session"), "{turned:?}");

    let beside = ["turn", "tab", "work:1", "--into", "work:2"];
    let turned = luneta(&host, ("work", 1), &beside);
    assert!(why(&turned).contains("in the tab of pane 2 already"), "{turned:?}");

    assert_eq!(host.lists.opened(), [] as [String; 0]);
    assert_eq!(host.lists.turned(), [] as [String; 0]);
}

/// A tab turns into panes with no client in its session too, as the tab they go
/// into has its room already, and the tab they left goes.
#[test]
fn a_tab_turns_into_panes_where_no_client_is_and_says_the_pane_it_was_given() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.lists.list("work", &json!([pane(1, from), pane(3, to)]));
    host.lists.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));

    let turned = luneta(&host, ("elsewhere", 7), &["turn", "tab", "work:1", "--into", "work:3"]);

    assert_eq!(said(&turned), "work:1\n");
    let closed = host.lists.closed();
    assert!(closed.contains(&"--session work action close-tab-by-id 0".to_owned()), "{closed:?}");
}

/// zellij runs out of room partway: the agent hears how many moved, as those
/// stay moved, and the client zellij took along goes back all the same.
#[test]
fn a_tab_that_turns_only_partly_fails_saying_so_and_the_client_goes_back() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let (from, to, third) = ((0, "Tab #1"), (1, "Tab #2"), (2, "Tab #3"));
    let laid = json!([pane(1, from), pane(2, from), pane(3, to), pane(7, third)]);
    host.lists.list("work", &laid);
    let first_there = json!([pane(1, to), pane(2, from), pane(3, to), pane(7, third)]);
    host.lists.stacks_into("work", 1, &first_there);
    host.lists.leave_little_room("work");
    host.lists.clients("work", &[(1, "terminal_7")]);
    host.lists.stacks_clients_into("work", &[(1, "terminal_1")]);
    host.lists.focuses_clients_into("work", &[(1, "terminal_7")]);

    let turned = luneta(&host, ("work", 7), &["turn", "tab", "work:1", "--into", "work:3"]);

    let why = why(&turned);
    assert!(why.contains("turned 1 of the 2 panes"), "{why}");
    assert!(why.contains("no room for terminal_2"), "{why}");
    assert_eq!(host.lists.went(), ["--session work action focus-pane-id terminal_7"]);
}

/// A pane keeps the name its session had as it opened. Where no session runs
/// by it now, and no lunetad says where the pane's agents are, luneta cannot
/// tell which session it runs in, and what goes by that does nothing: a pane
/// or a tab opens in no session that is gone, a session opens from none,
/// which would make none and say it had, `go` moves nobody, and a close,
/// which takes what holds this pane last, closes nothing.
#[test]
fn what_goes_by_the_session_it_runs_in_refuses_where_none_runs_by_its_name() {
    let host = Host::new();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    fs::create_dir(&cwd).unwrap();
    let cwd = cwd.to_str().unwrap();

    for args in [
        &["open", "pane", "--cwd", cwd][..],
        &["open", "tab", "--cwd", cwd],
        &["open", "session", "--cwd", cwd],
        &["go", "play"],
        &["close", "play"],
        &["here"],
    ] {
        let asked = luneta(&host, ("work", 2), args);
        let why = String::from_utf8_lossy(&asked.stderr);
        assert_eq!(asked.status.code(), Some(1), "{args:?}: {why}");
        assert!(why.contains("no session runs by that name now"), "{args:?}: {why}");
    }
    assert_eq!(host.lists.closed(), [] as [String; 0]);
}

/// The CLI, run with `args` by the agent of luneta's ask box, with `vars`:
/// the pane you asked from, the turn it is at and the one before.
fn asked(host: &Host, vars: &[(&str, &str)], args: &[&str]) -> Output {
    let mut command = host.vars().command(LUNETA);
    answered(command.envs(vars.iter().copied()).args(args))
}

/// The agent luneta's ask box runs is in no pane, and goes by the pane you
/// asked from, as the box names it: it is where `here` says it runs, and
/// what holds it is what a close takes last. A command in a pane of zellij
/// that names another for the box's goes by neither, nor does one the box
/// names a pane for outside a turn. Where the box names none, as it cannot
/// tell, a close in a turn of it closes nothing, as it could take your pane
/// first, and `here`, `go`, `open session`, and `open pane` and `open tab`
/// without a pane or a session to open in, say that it cannot tell, and ask
/// zellij nothing.
#[test]
fn the_ask_box_s_agent_goes_by_the_pane_it_names() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    let box_pane = ("LUNETA_PANE", "work:2");
    let first = [box_pane, ("LUNETA_TURN", "00000000000000aa")];
    let second =
        [box_pane, ("LUNETA_TURN", "00000000000000bb"), ("LUNETA_TURN_BEFORE", "00000000000000aa")];

    let seat = asked(&host, &first, &["here"]);
    assert!(seat.status.success(), "{}", String::from_utf8_lossy(&seat.stderr));
    assert_eq!(String::from_utf8(seat.stdout).unwrap(), "work:2\n");
    let in_zellij = [("ZELLIJ_SESSION_NAME", "work"), ("ZELLIJ_PANE_ID", "3")];
    for (vars, said) in [
        (&[box_pane][..], "LUNETA_PANE is set, and LUNETA_TURN is not"),
        (&[first[0], first[1], in_zellij[0], in_zellij[1]], "LUNETA_PANE names work:2"),
    ] {
        let seat = asked(&host, vars, &["here"]);
        let why = String::from_utf8_lossy(&seat.stderr);
        assert_eq!(seat.status.code(), Some(1), "{why}");
        assert!(why.contains(said), "{why}");
    }

    let (said, agreed) = planned(&asked(&host, &first, &["close", "work:2", "play"]));
    assert_eq!(
        said,
        format!("delete session play: {UNTOLD}\nclose pane work:2: {UNTOLD}\n")
    );
    let told = asked(&host, &second, &["close", "work:2", "play", "--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let closed =
        ["delete-session --force play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);

    let untold = [("LUNETA_TURN", "00000000000000aa")];
    let unseated = asked(&host, &untold, &["close", "work:3"]);
    let why = String::from_utf8_lossy(&unseated.stderr);
    assert_eq!(unseated.status.code(), Some(1), "{why}");
    assert!(why.contains("cannot tell which pane the user asked from"), "{why}");
    assert!(unseated.stdout.is_empty(), "{}", String::from_utf8_lossy(&unseated.stdout));
    assert_eq!(host.lists.closed(), closed);
    for args in [
        &["here"][..],
        &["go", "work"],
        &["open", "session", "--cwd", "/w"],
        &["open", "pane", "--cwd", "/w"],
        &["open", "tab", "--cwd", "/w"],
    ] {
        let refused = asked(&host, &untold, args);
        let why = String::from_utf8_lossy(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{args:?}: {why}");
        assert!(why.contains("cannot tell which pane the user asked from"), "{args:?}: {why}");
    }
    assert_eq!(host.lists.went(), [] as [String; 0]);
    assert_eq!(host.lists.opened(), [] as [String; 0]);
}

/// Asked from another machine, where luneta's zellij is, the ask box's
/// agent goes by no pane of this one's, as none is yours: a close takes a
/// yes in the turn after its plan, as ever, and keeps no pane for last.
/// `here`, `go`, `open session`, and `open pane` and `open tab` without a
/// pane or a session to open in, say why they have none to go by, and ask
/// zellij nothing. A command in a pane of zellij that the box says was asked
/// from another machine goes by neither, nor one the box says both of, nor
/// one it says so of outside a turn, or by anything but luneta's `1`.
#[test]
fn the_ask_box_s_agent_asked_from_another_machine_goes_by_no_pane_here() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    let away = ("LUNETA_AWAY", "1");
    let first = [away, ("LUNETA_TURN", "00000000000000aa")];
    let second =
        [away, ("LUNETA_TURN", "00000000000000bb"), ("LUNETA_TURN_BEFORE", "00000000000000aa")];

    let (said, agreed) = planned(&asked(&host, &first, &["close", "work:2", "play"]));
    assert_eq!(
        said,
        format!("delete session play: {UNTOLD}\nclose pane work:2: {UNTOLD}\n")
    );
    let told = asked(&host, &second, &["close", "work:2", "play", "--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let closed =
        ["delete-session --force play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);

    for args in [
        &["here"][..],
        &["go", "work"],
        &["open", "session", "--cwd", "/w"],
        &["open", "pane", "--cwd", "/w"],
        &["open", "tab", "--cwd", "/w"],
    ] {
        let refused = asked(&host, &first, args);
        let why = String::from_utf8_lossy(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{args:?}: {why}");
        assert!(why.contains("asked from another machine"), "{args:?}: {why}");
    }
    let in_zellij = [("ZELLIJ_SESSION_NAME", "work"), ("ZELLIJ_PANE_ID", "3")];
    for (vars, said) in [
        (&[first[0], first[1], in_zellij[0], in_zellij[1]][..], "LUNETA_AWAY says"),
        (&[first[0], first[1], ("LUNETA_PANE", "work:2")], "LUNETA_PANE names work:2"),
        (&[away], "LUNETA_AWAY is set, and LUNETA_TURN is not"),
        (&[("LUNETA_AWAY", "0"), first[1]], "LUNETA_AWAY is \"0\""),
    ] {
        let seat = asked(&host, vars, &["here"]);
        let why = String::from_utf8_lossy(&seat.stderr);
        assert_eq!(seat.status.code(), Some(1), "{why}");
        assert!(why.contains(said), "{why}");
    }
    assert_eq!(host.lists.closed(), closed);
    assert_eq!(host.lists.went(), [] as [String; 0]);
    assert_eq!(host.lists.opened(), [] as [String; 0]);
}

/// The ask box's agent runs what it may without anyone asking you, and a
/// pane runs what it is given: so a pane it opens, or a tab, runs your shell
/// or Claude Code, as `claude [--name <name>] [<prompt>]`, which asks you or
/// looks at what it does, in your permission mode. Any other command, an
/// option of claude's but `--name`, and a prompt that is a line for your
/// shell, are refused before zellij is asked anything. Out of the box, a
/// pane runs what it is given.
#[test]
fn a_pane_the_ask_box_s_agent_opens_runs_your_shell_or_claude() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let tab = (0, "Tab #1");
    host.lists.list("work", &json!([listed(2, "fish", tab, Some("/w"))]));
    host.lists.clients("work", &[(1, "terminal_2")]);
    let cwd = tempfile::tempdir().unwrap();
    let cwd = cwd.path().to_str().unwrap();
    let in_the_box = [("LUNETA_PANE", "work:2"), ("LUNETA_TURN", "00000000000000aa")];
    let open = |what: &str, command: &[&str]| {
        let args = [&["open", what, "--cwd", cwd, "--"][..], command].concat();
        asked(&host, &in_the_box, &args)
    };
    // The fake zellij opens nothing, and notes what it was asked.
    let last_opened = || host.lists.opened().last().cloned().unwrap_or_default();
    for (what, command) in [
        ("pane", &[][..]),
        ("pane", &["claude", "--name", "issue-42", "tackle it"]),
        ("pane", &["claude", "/thermo-nuclear-review"]),
        ("tab", &["claude"]),
        ("pane", &["pi", "--name", "issue-42", "tackle it"]),
        ("tab", &["pi", "/skill:tigerstyle"]),
    ] {
        open(what, command);
        let asked = last_opened();
        let runs = command.join(" ");
        let opens = asked.contains(&format!("action new-{what}")) && asked.ends_with(&runs);
        assert!(opens, "{command:?}: {asked}");
    }
    let opened = host.lists.opened();
    for (what, command, refused) in [
        ("pane", &["touch", "x"][..], "it does not run touch x"),
        ("pane", &["sh", "-c", "claude"], "it does not run sh -c claude"),
        ("tab", &["claude", "--name", "x", "--model", "opus"], "it does not run"),
        ("tab", &["claude", "--dangerously-skip-permissions"], "no option but --name"),
        ("pane", &["claude", "--name", "-x"], "no option but --name"),
        ("pane", &["claude", "!rm -rf ~"], "no prompt with a ! first"),
        ("pane", &["pi", "--tools", "bash"], "it does not run"),
        ("pane", &["pi", "-p"], "pi takes no option but --name"),
        ("pane", &["pi", "!rm -rf ~"], "no prompt with a ! first"),
        ("pane", &["pi", "@/etc/shadow"], "pi takes no prompt with an @ first"),
        ("pane", &["codex", "tackle it"], "it does not run codex"),
    ] {
        let refusal = open(what, command);
        let why = String::from_utf8_lossy(&refusal.stderr);
        assert_eq!(refusal.status.code(), Some(1), "{command:?}: {why}");
        let only = "in luneta's ask box, a pane runs your shell, `claude";
        assert!(why.contains(only), "{command:?}: {why}");
        assert!(why.contains(refused), "{command:?}: {why}");
    }
    assert_eq!(host.lists.opened(), opened);

    let opened = luneta(&host, ("work", 2), &["open", "pane", "--cwd", cwd, "--", "touch", "x"]);
    assert!(opened.status.success(), "{}", String::from_utf8_lossy(&opened.stderr));
    assert_eq!(String::from_utf8_lossy(&opened.stdout), "work:9\n");
    let touch = format!("action new-pane --no-focus --cwd {cwd} -- touch x");
    assert!(last_opened().ends_with(&touch), "{}", last_opened());
}

/// The ask box names the pane you asked from by its session's name as you
/// asked, which an agent's pane need not have opened under. So where one
/// session was renamed, and another made by its old name since, the box's
/// agent goes by the pane of the one it names, though an agent in the pane of
/// that id in the one renamed says that name.
#[test]
fn the_ask_box_s_agent_goes_by_the_name_the_session_has_now() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let there = process(Some(("work", 2)));
    host.write(there.pid(), &file_for("there", &there, "busy"));
    let _daemon = start(host.vars()).unwrap();
    let in_session = |session| host.agent("there").filter(|agent| now_in(agent) == Some(session));
    eventually("the daemon has the agent in work", || in_session("work"));
    fs::rename(host.contract().join("work"), host.contract().join("fix")).unwrap();
    eventually("the agent is in fix", || in_session("fix"));
    let _taken = UnixListener::bind(host.contract().join("work")).unwrap();

    let turn = ("LUNETA_TURN", "00000000000000aa");
    let seat = asked(&host, &[("LUNETA_PANE", "work:2"), turn], &["here"]);
    assert!(seat.status.success(), "{}", String::from_utf8_lossy(&seat.stderr));
    assert_eq!(String::from_utf8(seat.stdout).unwrap(), "work:2\n");
    let seat = asked(&host, &[("LUNETA_PANE", "gone:2"), turn], &["here"]);
    let why = String::from_utf8_lossy(&seat.stderr);
    assert_eq!(seat.status.code(), Some(1), "{why}");
    assert!(why.contains("no session runs by that name now"), "{why}");
}

/// The ask box's agent runs a close without anyone asking you first, and so
/// a close it runs takes a yes to a plan only in the turn after the one that
/// said it, with your answer between them: not in the same turn, not in the
/// first, nor to a plan said out of the box. Out of it, a close takes a yes
/// to the plan it says.
#[test]
fn a_close_in_the_ask_box_takes_a_yes_only_in_the_turn_after_its_plan() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    let close = |vars: &[(&str, &str)], yes: &[&str]| {
        asked(&host, vars, &[&["close", "work:3"][..], yes].concat())
    };
    let box_pane = ("LUNETA_PANE", "work:2");
    let first = [box_pane, ("LUNETA_TURN", "00000000000000aa")];
    let second =
        [box_pane, ("LUNETA_TURN", "00000000000000bb"), ("LUNETA_TURN_BEFORE", "00000000000000aa")];

    let (said, agreed) = planned(&close(&first, &[]));
    assert_eq!(said, format!("close pane work:3: {UNTOLD}\n"));
    let same_turn = close(&first, &["--yes", &agreed]);
    let (_, again) = planned(&same_turn);
    assert_eq!(again, agreed);
    let why = String::from_utf8_lossy(&same_turn.stderr);
    assert!(why.contains("only in the turn after the one that said it"), "{why}");
    let (_, out_of_the_box) = planned(&close(&[], &[]));
    assert_ne!(out_of_the_box, agreed);
    planned(&close(&second, &["--yes", &out_of_the_box]));
    assert_eq!(host.lists.closed(), [] as [String; 0]);

    let told = close(&second, &["--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(host.lists.closed(), ["--session work action close-pane --pane-id terminal_3"]);

    let malformed = close(&[("LUNETA_TURN", "forged")], &[]);
    let why = String::from_utf8_lossy(&malformed.stderr);
    assert_eq!(malformed.status.code(), Some(1), "{why}");
    assert!(why.contains("LUNETA_TURN is no key"), "{why}");
}

/// zellij's variables keep the name a session had as the pane opened, and
/// lunetad follows a rename. So in an agent's pane whose session was renamed,
/// and another made by its old name since, the CLI says it runs in the one
/// renamed, and a close marks the agent's line in it and takes it last.
#[test]
fn a_close_takes_the_session_it_runs_in_last_by_the_name_it_has_now() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let here = process(Some(("work", 2)));
    host.write(here.pid(), &file_for("here", &here, "busy"));
    let _daemon = start(host.vars()).unwrap();
    let in_session = |session| host.agent("here").filter(|agent| now_in(agent) == Some(session));
    eventually("the daemon has the agent in work", || in_session("work"));
    fs::rename(host.contract().join("work"), host.contract().join("fix")).unwrap();
    eventually("the agent is in fix", || in_session("fix"));
    let _taken = UnixListener::bind(host.contract().join("work")).unwrap();

    let seat = luneta(&host, ("work", 2), &["here"]);
    assert!(seat.status.success(), "{}", String::from_utf8_lossy(&seat.stderr));
    assert_eq!(String::from_utf8(seat.stdout).unwrap(), "fix:2\n");

    let given = ["close", "fix", "play"];
    let close = |yes: &[&str]| luneta(&host, ("work", 2), &[&given[..], yes].concat());
    let (said, agreed) = planned(&close(&[]));
    let plan = format!(
        "delete session play: no agent\n\
         delete session fix: 1 agent, 1 busy\n  \
         fix:2 (named work)  busy     {:>7}  here-name  (this runs here)\n",
        here.pid()
    );
    assert_eq!(said, plan);
    let told = close(&["--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(host.lists.closed(), ["delete-session --force play", "delete-session --force fix"]);
}

/// What holds the pane a close runs in goes last, as the agent that ran it
/// ends with it. Where anything before it failed to close, it stays, so that
/// the agent can say what failed.
#[test]
fn a_close_leaves_what_holds_its_pane_where_anything_before_it_failed() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    host.lists.refuse("play");

    let given = ["close", "work:2", "play"];
    let close = |yes: &[&str]| luneta(&host, ("work", 2), &[&given[..], yes].concat());
    let (_, agreed) = planned(&close(&[]));
    let told = close(&["--yes", &agreed]);
    let why = String::from_utf8_lossy(&told.stderr);
    assert_eq!(told.status.code(), Some(1), "{why}");
    assert!(why.contains("zellij refused to close anything of play"), "{why}");
    assert!(why.contains("did not close pane work:2"), "{why}");
    assert_eq!(host.lists.closed(), [] as [String; 0]);
}

/// zellij keeps nothing of a session in its first second, and fails to
/// delete a session that young once it has killed it: the close takes it
/// for deleted. A session zellij finds nothing of, as one that ended since
/// the plan, is not deleted, and what holds the pane the close runs in stays.
#[test]
fn a_close_deletes_a_session_zellij_kills_though_it_kept_nothing_of_it() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let _fix = UnixListener::bind(host.contract().join("fix")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    host.lists.unkept("play");
    host.lists.gone("fix");

    let given = ["close", "work:2", "play", "fix"];
    let close = |yes: &[&str]| luneta(&host, ("work", 2), &[&given[..], yes].concat());
    let (_, agreed) = planned(&close(&[]));
    let told = close(&["--yes", &agreed]);
    let why = String::from_utf8_lossy(&told.stderr);
    assert_eq!(told.status.code(), Some(1), "{why}");
    assert!(why.contains(r#"Session: "fix" not found."#), "{why}");
    assert!(!why.contains(r#""play""#), "{why}");
    assert!(why.contains("did not close pane work:2"), "{why}");
    assert_eq!(host.lists.closed(), ["delete-session --force play"]);
}

/// zellij takes away what it keeps of a session before the session's server
/// has ended, and the server's writer can write it anew meanwhile, which
/// zellij would bring the session back with: the close lets go of it again
/// once the server has ended.
#[test]
fn a_close_lets_go_again_of_what_the_server_of_a_session_it_deletes_writes_anew() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    host.lists.rewrites("play");

    let close = |yes: &[&str]| luneta(&host, ("work", 2), &[&["close", "play"][..], yes].concat());
    let (_, agreed) = planned(&close(&[]));
    let told = close(&["--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(host.lists.closed(), ["delete-session --force play", "delete-session play"]);
}

/// A session made by the name of one a close deletes, as the close goes, is
/// another than the one it was told yes to, which zellij would kill all the
/// same: it stays.
#[test]
fn a_close_leaves_a_session_made_by_the_name_of_one_it_deletes_as_it_goes() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _away = UnixListener::bind(host.contract().join("away")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    // A close deletes the sessions it is given in the order of their names.
    host.lists.renews("away", "play");

    let given = ["close", "away", "play"];
    let close = |yes: &[&str]| luneta(&host, ("work", 2), &[&given[..], yes].concat());
    let (_, agreed) = planned(&close(&[]));
    let told = close(&["--yes", &agreed]);
    let why = String::from_utf8_lossy(&told.stderr);
    assert_eq!(told.status.code(), Some(1), "{why}");
    assert!(why.contains(r#""play" is a session made since, which stays"#), "{why}");
    assert_eq!(host.lists.closed(), ["delete-session --force away"]);
}
