//! What zellij is asked to move a pane, or a tab, to another session, as the
//! daemon answers `Carry`: zellij moves no pane between sessions, so each
//! closes, and opens anew there, what it ran starting again, and nothing
//! closes that could not.

use std::fs::{self, File};
use std::thread;
use std::time::Duration;

use lunetad::api::carry_request::Lands;
use lunetad::api::carry_response::Carried;
use lunetad::api::{self, CarryRequest, CarryResponse};
use serde_json::{json, Value};
use tonic::{Code, Status};

use crate::agents::{file_for, process};
use crate::host::{eventually, folder_in, start, Host, Running};
use crate::sessions::{listed, session};

const ID: &str = "0b7e1c2a-5d1f-4c8e-9a43-1f2e3d4c5b6a";

/// A pane at a shell's prompt, in the tab `tab`, where it works in `cwd`.
fn shell(id: u32, tab: (u32, &str), cwd: &str) -> Value {
    listed(id, "~", tab, Some(cwd))
}

/// A pane that runs `command` in front, in the tab `tab`.
fn running(id: u32, tab: (u32, &str), command: &str) -> Value {
    let mut pane = shell(id, tab, "/home/you/work");
    pane["pane_command"] = json!(command);
    pane
}

/// When the session `name` started, as the daemon says, once it lists its
/// panes.
fn started(host: &Host, name: &str) -> prost_types::Timestamp {
    eventually(&format!("lunetad lists the panes of {name}"), || {
        let host = host.host();
        let session = session(&host, name)?;
        session.panes.as_ref()?;
        session.started_at
    })
}

/// The question that moves the pane `pane` of `work`, or its tab, to `play`,
/// where it `lands`.
fn asked(host: &Host, pane: u32, tab: bool, lands: Lands, ending: bool) -> CarryRequest {
    CarryRequest {
        on: None,
        session: "work".to_owned(),
        started_at: Some(started(host, "work")),
        pane,
        tab,
        to: "play".to_owned(),
        to_started_at: Some(started(host, "play")),
        lands: Some(lands),
        ending,
    }
}

/// What the daemon of `host` answers `asked`.
fn answered(host: &Host, asked: CarryRequest) -> Result<CarryResponse, Status> {
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let mut daemon = lunetad::connect(&folder).await.unwrap();
        let mut answer = daemon.carry(asked).await?.into_inner();
        Ok(answer.message().await?.expect("lunetad answers once"))
    })
}

/// The terminals an answer says opened, by the ones that closed.
fn opened(answer: &CarryResponse) -> Vec<(u32, u32)> {
    answer.carried.iter().map(|Carried { from, terminal }| (*from, *terminal)).collect()
}

/// What the daemon of `host` answers `asked`, where each pane opened again:
/// the terminals it opened, by the ones it closed.
fn carried(host: &Host, asked: CarryRequest) -> Result<Vec<(u32, u32)>, Status> {
    let answer = answered(host, asked)?;
    assert_eq!(answer.stranded, None, "{answer:?}");
    Ok(opened(&answer))
}

fn refused(status: &Status, why: &str) {
    assert_eq!(status.code(), Code::FailedPrecondition, "{status:?}");
    assert!(status.message().contains(why), "{status:?}");
}

/// A host with the sessions `work` and `play`, which zellij lists with these
/// panes, and what runs them until it goes: their sockets, and the daemon.
fn two_sessions(work: &Value, play: &Value) -> (Host, impl Sized) {
    let host = Host::new();
    host.zellij.list("work", work);
    host.zellij.list("play", play);
    let sockets = (host.serve("work"), host.serve("play"));
    let daemon = start(host.vars()).unwrap();
    (host, (sockets, daemon))
}

/// What has zellij open your shell beside the pane `beside` of play, in
/// `cwd`: as a command, as zellij starts its own shell where the pane beside
/// works, whatever `--cwd` says.
fn shell_opened(beside: u32, cwd: &str) -> String {
    format!("{beside} --session play action new-pane --no-focus --cwd {cwd} --close-on-exit -- ")
}

fn opened_as(opened: &[String], expected: &[String]) {
    assert_eq!(opened.len(), expected.len(), "{opened:?}");
    for (opened, expected) in opened.iter().zip(expected) {
        assert!(opened.starts_with(expected), "{opened:?}, not {expected:?}");
    }
}

/// A shell at its prompt moves beside a pane of another session: its pane
/// closes, and your shell opens there, in its directory, with no client moved.
#[test]
fn a_shell_at_its_prompt_moves_beside_a_pane_of_another_session() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), shell(2, (0, "Tab #1"), "/w/b")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);

    let carried = carried(&host, asked(&host, 2, false, Lands::Beside(5), false)).unwrap();

    assert_eq!(carried, [(2, 9)]);
    assert_eq!(host.zellij.closed(), ["--session work action close-pane --pane-id terminal_2"]);
    opened_as(&host.zellij.opened(), &[shell_opened(5, "/w/b")]);
    assert_eq!(host.zellij.went(), Vec::<String>::new());
}

/// One move at a time on a machine: while another holds the lock, a move
/// closes nothing, as two of one pane would both close it, and both open it
/// again. Once that one lets go, it moves.
#[test]
fn a_move_goes_only_while_no_other_is_under_way() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), shell(2, (0, "Tab #1"), "/w/b")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);
    let folder = folder_in(host.runtime.path()).unwrap();
    let another = File::create(folder.path().join(lunetad::carries::MOVING)).unwrap();
    another.try_lock().unwrap();

    let stays = carried(&host, asked(&host, 2, false, Lands::Beside(5), false)).unwrap_err();
    refused(&stays, "another move is under way");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());

    drop(another);
    let carried = carried(&host, asked(&host, 2, false, Lands::Beside(5), false)).unwrap();
    assert_eq!(carried, [(2, 9)]);
}

/// What runs in front of a shell, a program, stays, and nothing closes.
#[test]
fn what_runs_stays_and_nothing_closes() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), running(2, (0, "Tab #1"), "cargo test")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);

    let stays = carried(&host, asked(&host, 2, false, Lands::Beside(5), false)).unwrap_err();

    refused(&stays, "it runs `cargo test`");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());
    assert_eq!(host.zellij.opened(), Vec::<String>::new());
}

/// The last pane of a session ends it, which it does only where the question
/// says it may: zellij deletes the session, so that it keeps nothing to bring
/// it back with, and the pane opens again in the other.
#[test]
fn the_last_pane_ends_its_session_only_where_it_may() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);

    let stays = carried(&host, asked(&host, 1, false, Lands::Beside(5), false)).unwrap_err();
    refused(&stays, "it is all work has");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());

    let carried = carried(&host, asked(&host, 1, false, Lands::Beside(5), true)).unwrap();
    assert_eq!(carried, [(1, 9)]);
    assert_eq!(host.zellij.closed(), ["delete-session --force work"]);
    opened_as(&host.zellij.opened(), &[shell_opened(5, "/w/a")]);
}

/// A pane that would take floating panes along as its tab closes stays: a tab
/// goes whole, or not at all.
#[test]
fn a_pane_that_would_close_floating_panes_stays() {
    let mut floating = shell(3, (1, "auth"), "/w/f");
    floating["is_floating"] = json!(true);
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), shell(2, (1, "auth"), "/w/b"), floating]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);

    let stays = carried(&host, asked(&host, 2, false, Lands::Beside(5), false)).unwrap_err();

    refused(&stays, "floating panes and all");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());
}

/// A tab goes whole, its panes in the order of their ids, to a tab of its
/// own, named as it was: the first opens the tab, which needs a client there
/// to make room for it, and the others open beside it. One with no client
/// moves nothing.
#[test]
fn a_tab_goes_whole_to_a_tab_of_its_own_named_as_it_was() {
    let tab = (1, "auth");
    let work =
        json!([shell(1, (0, "Tab #1"), "/w/a"), shell(4, tab, "/w/d"), shell(2, tab, "/w/b")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);

    let stays = carried(&host, asked(&host, 4, true, Lands::NewTab(()), false)).unwrap_err();
    refused(&stays, "no client is in play");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());

    host.zellij.clients("play", &[(1, "terminal_5")]);
    let opened = json!([shell(5, (0, "Tab #1"), "/w/p"), shell(7, (3, "auth"), "/w/b")]);
    host.zellij.opens_into("play", &opened);
    let carried = carried(&host, asked(&host, 4, true, Lands::NewTab(()), false)).unwrap();

    assert_eq!(carried, [(2, 7), (4, 9)]);
    assert_eq!(host.zellij.closed(), ["--session work action close-tab-by-id 1"]);
    let tab = "- --session play action new-tab --no-focus --cwd /w/b --name auth --close-on-exit";
    opened_as(&host.zellij.opened(), &[tab.to_owned(), shell_opened(7, "/w/d")]);
}

/// What zellij fails to open once the tab closed is said, with what starts it
/// by hand, beside what did open.
#[test]
fn what_does_not_open_again_is_said_beside_what_did() {
    let tab = (1, "auth");
    let work =
        json!([shell(1, (0, "Tab #1"), "/w/a"), shell(4, tab, "/w/d"), shell(2, tab, "/w/b")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);
    host.zellij.clients("play", &[(1, "terminal_5")]);
    let opened_into = json!([shell(5, (0, "Tab #1"), "/w/p"), shell(7, (3, "auth"), "/w/b")]);
    host.zellij.opens_into("play", &opened_into);
    host.zellij.fails("play", "new-pane", "no room");

    let answer = answered(&host, asked(&host, 4, true, Lands::NewTab(()), false)).unwrap();

    assert_eq!(opened(&answer), [(2, 7)]);
    let stranded = answer.stranded.unwrap_or_default();
    let said = "closed the tab of pane 4 of work, but zellij opened 1 of 2 again in play";
    assert!(stranded.starts_with(said), "{stranded}");
    assert!(stranded.contains("no room"), "{stranded}");
    assert!(stranded.ends_with("Start the rest by hand: your shell in /w/d"), "{stranded}");
    assert_eq!(host.zellij.closed(), ["--session work action close-tab-by-id 1"]);
}

/// Has the daemon of `host` read `agent` as Claude Code's, with `status`.
fn write_agent(host: &Host, agent: &Running, status: &str) {
    let sessions = host.home.path().join(".claude/sessions");
    fs::create_dir_all(&sessions).unwrap();
    let file = file_for(ID, agent, status);
    fs::write(sessions.join(format!("{}.json", agent.pid())), file.to_string()).unwrap();
}

/// Waits for `host`'s daemon to list the agent of `ID` with `status`.
fn agent_listed(host: &Host, status: api::Status) {
    eventually("lunetad lists the agent", || {
        let agents = host.host().agents;
        let agent = agents.into_iter().find(|agent| agent.session_id == ID)?;
        let known = api::agent::Status::KnownStatus(status.into());
        (agent.status == Some(known)).then_some(())
    });
}

/// Claude Code's agent at its prompt resumes its session in the other session,
/// where it started, by the program it runs, a sleep standing for Claude Code,
/// and not with what that was asked as it started: once its pane has closed,
/// and it has ended, so that no two agents write one transcript.
#[test]
fn an_agent_at_its_prompt_resumes_once_it_has_ended() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), running(3, (0, "Tab #1"), "claude")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);
    let mut claude = process(Some(("work", 3)));
    write_agent(&host, &claude, "idle");
    agent_listed(&host, api::Status::Idle);

    let asked = asked(&host, 3, false, Lands::Beside(5), false);
    let (closed, carried) = thread::scope(|scope| {
        let moving = scope.spawn(|| carried(&host, asked));
        let closed = eventually("zellij closes the agent's pane", || {
            Some(host.zellij.closed()).filter(|closed| !closed.is_empty())
        });
        thread::sleep(Duration::from_millis(200));
        let opened_early = host.zellij.opened();
        claude.kill();
        assert_eq!(opened_early, Vec::<String>::new(), "it opened before the agent ended");
        (closed, moving.join().unwrap())
    });

    assert_eq!(closed, ["--session work action close-pane --pane-id terminal_3"]);
    assert_eq!(carried.unwrap(), [(3, 9)]);
    let resumes = format!(
        "5 --session play action new-pane --no-focus --cwd /home/you/work -- sleep --resume {ID}"
    );
    assert_eq!(host.zellij.opened(), [resumes]);
}

/// An agent at work stays, and nothing closes.
#[test]
fn an_agent_at_work_stays() {
    let work = json!([shell(1, (0, "Tab #1"), "/w/a"), running(3, (0, "Tab #1"), "claude")]);
    let play = json!([shell(5, (0, "Tab #1"), "/w/p")]);
    let (host, _kept) = two_sessions(&work, &play);
    let claude = process(Some(("work", 3)));
    write_agent(&host, &claude, "busy");
    agent_listed(&host, api::Status::Busy);

    let stays = carried(&host, asked(&host, 3, false, Lands::Beside(5), false)).unwrap_err();

    refused(&stays, "its agent is at work");
    assert_eq!(host.zellij.closed(), Vec::<String>::new());
}
