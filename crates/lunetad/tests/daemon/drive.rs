//! What the CLI has zellij do, for an agent that drives luneta: a close says
//! what goes and which agents end with it, and a tab opens only where it has
//! room.

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
    assert!(!asked.status.success());
    let said = String::from_utf8(asked.stdout).unwrap();
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

    let told = luneta(&host, ("work", 2), &["close", "work:2", "play", "--yes"]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(String::from_utf8(told.stdout).unwrap(), plan);
    let closed = ["kill-session play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);
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
