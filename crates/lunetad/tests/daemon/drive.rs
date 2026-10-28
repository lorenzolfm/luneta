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
    assert!(host.lists.closed().is_empty());
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
/// that names another for the box's goes by neither. Where the box names
/// none, as it cannot tell, a close in a turn of it closes nothing, as it
/// could take your pane first.
#[test]
fn the_ask_box_s_agent_goes_by_the_pane_it_names() {
    let host = Host::new();
    let _work = UnixListener::bind(host.contract().join("work")).unwrap();
    let _play = UnixListener::bind(host.contract().join("play")).unwrap();
    let tab = (0, "Tab #1");
    let work = json!([listed(2, "claude", tab, Some("/w")), listed(3, "fish", tab, Some("/w"))]);
    host.lists.list("work", &work);
    let box_pane = ("LUNETA_PANE", "work:2");

    let seat = asked(&host, &[box_pane], &["here"]);
    assert!(seat.status.success(), "{}", String::from_utf8_lossy(&seat.stderr));
    assert_eq!(String::from_utf8(seat.stdout).unwrap(), "work:2\n");
    let in_pane = [box_pane, ("ZELLIJ_SESSION_NAME", "work"), ("ZELLIJ_PANE_ID", "3")];
    let seat = asked(&host, &in_pane, &["here"]);
    let why = String::from_utf8_lossy(&seat.stderr);
    assert_eq!(seat.status.code(), Some(1), "{why}");
    assert!(why.contains("LUNETA_PANE names work:2"), "{why}");

    let (said, agreed) = planned(&asked(&host, &[box_pane], &["close", "work:2", "play"]));
    assert_eq!(
        said,
        format!("kill session play: {UNTOLD}\nclose pane work:2: {UNTOLD}\n")
    );
    let told = asked(&host, &[box_pane], &["close", "work:2", "play", "--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    let closed = ["kill-session play", "--session work action close-pane --pane-id terminal_2"];
    assert_eq!(host.lists.closed(), closed);

    let unseated = asked(&host, &[("LUNETA_TURN", "00000000000000aa")], &["close", "work:3"]);
    let why = String::from_utf8_lossy(&unseated.stderr);
    assert_eq!(unseated.status.code(), Some(1), "{why}");
    assert!(why.contains("cannot tell which pane the user asked from"), "{why}");
    assert!(unseated.stdout.is_empty(), "{}", String::from_utf8_lossy(&unseated.stdout));
    assert_eq!(host.lists.closed(), closed);
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
        let opened = asked(&host, &in_the_box, &args);
        assert_eq!(opened.status.code(), Some(1), "{what} {command:?}");
        String::from_utf8_lossy(&opened.stderr).into_owned()
    };
    // The fake zellij opens nothing, and says what it was asked.
    for (what, command) in [
        ("pane", &[][..]),
        ("pane", &["claude", "--name", "issue-42", "tackle it"]),
        ("pane", &["claude", "/thermo-nuclear-review"]),
        ("tab", &["claude"]),
    ] {
        let why = open(what, command);
        assert!(why.contains(&format!("was asked: action new-{what}")), "{command:?}: {why}");
    }
    for (what, command, refused) in [
        ("pane", &["touch", "x"][..], "it does not run touch x"),
        ("pane", &["sh", "-c", "claude"], "it does not run sh -c claude"),
        ("tab", &["claude", "--name", "x", "--model", "opus"], "it does not run"),
        ("tab", &["claude", "--dangerously-skip-permissions"], "no option but --name"),
        ("pane", &["claude", "--name", "-x"], "no option but --name"),
        ("pane", &["claude", "!rm -rf ~"], "no prompt with a ! first"),
    ] {
        let why = open(what, command);
        assert!(why.contains("in luneta's ask box, a pane runs your shell or"), "{command:?}: {why}");
        assert!(why.contains(refused) && !why.contains("was asked"), "{command:?}: {why}");
    }

    let opened = luneta(&host, ("work", 2), &["open", "pane", "--cwd", cwd, "--", "touch", "x"]);
    let why = String::from_utf8_lossy(&opened.stderr);
    assert!(why.contains("was asked: action new-pane"), "{why}");
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

    let seat = asked(&host, &[("LUNETA_PANE", "work:2")], &["here"]);
    assert!(seat.status.success(), "{}", String::from_utf8_lossy(&seat.stderr));
    assert_eq!(String::from_utf8(seat.stdout).unwrap(), "work:2\n");
    let seat = asked(&host, &[("LUNETA_PANE", "gone:2")], &["here"]);
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
    assert!(host.lists.closed().is_empty());

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
        "kill session play: no agent\n\
         kill session fix: 1 agent, 1 busy\n  \
         fix:2 (named work)  busy     {:>7}  here-name  (this runs here)\n",
        here.pid()
    );
    assert_eq!(said, plan);
    let told = close(&["--yes", &agreed]);
    assert!(told.status.success(), "{}", String::from_utf8_lossy(&told.stderr));
    assert_eq!(host.lists.closed(), ["kill-session play", "kill-session fix"]);
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
    assert!(host.lists.closed().is_empty());
}
