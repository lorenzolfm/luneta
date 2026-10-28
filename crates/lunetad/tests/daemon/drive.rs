//! What the CLI has zellij do, for an agent that drives luneta: a close says
//! what goes and which agents end with it, and closes only that, a tab opens
//! only where it has room, and a session only from one that runs.

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
const UNTOLD: &str = "luneta cannot tell which agents it ends";

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
        "kill session play: 1 agent\n  \
         play:1  idle     {there:>7}  there-name\n\
         close pane work:2: 1 agent, 1 busy\n  \
         work:2  busy     {here:>7}  here-name  (this runs here)\n"
    );
    assert_eq!(said, plan);
    assert!(String::from_utf8_lossy(&asked.stderr).contains("closed nothing"));
    assert!(host.lists.closed().is_empty());

    let told = luneta(&host, ("work", 2), &["close", "work:2", "play", "--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(String::from_utf8(told.stdout).unwrap(), plan);
    let closed = ["kill-session play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);
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
    let plan = format!("kill session play: {UNTOLD}\nclose the tab of work:2, work:3: {UNTOLD}\n");
    assert_eq!(said, plan);

    let told = luneta(&host, ("work", 4), &[&given[..], &["--yes", &agreed]].concat());
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let closed = ["kill-session play", "--session work action close-tab-by-id 0"];
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
    assert_eq!(now, format!("kill session work: {UNTOLD}\n"));

    host.lists.list("work", &both);
    drop(work);
    fs::remove_file(&socket).unwrap();
    let _work = UnixListener::bind(&socket).unwrap();
    let (again, _) = planned(&close(&["--yes", &agreed]));
    assert_eq!(again, said);
    assert!(host.lists.closed().is_empty());
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

/// A pane keeps the name its session had as it opened. A session opens from
/// a pane only while its session runs by that name: one renamed since is not
/// taken for another of that name, which would make none and say it had.
#[test]
fn a_session_opens_only_from_a_pane_whose_session_runs_by_its_name() {
    let host = Host::new();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("work");
    fs::create_dir(&cwd).unwrap();

    let asked = luneta(&host, ("work", 2), &["open", "session", "--cwd", cwd.to_str().unwrap()]);
    assert!(!asked.status.success());
    let why = String::from_utf8_lossy(&asked.stderr);
    assert!(why.contains("no session runs by that name now"), "{why}");
}
