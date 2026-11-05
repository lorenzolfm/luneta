//! What zellij is asked to move a pane into a tab of its own, or the panes of
//! a tab into another, as the daemon answers `TurnIntoTab`, `TurnIntoPanes` and
//! `OtherTabs`: zellij's CLI has no command for it, and what it does lies in
//! the order of what it is asked, as those are measured against a real zellij.
//! Any floating asked for shows in `turned` here: zellij drops a pane it has no
//! room to float, so no move may ask for it.

use std::time::SystemTime;

use lunetad::api::other_tabs_response::Tab;
use lunetad::api::turn_into_panes_response::Came as PanesCame;
use lunetad::api::turn_into_panes_response::{Done as PanesDone, Partly};
use lunetad::api::turn_into_tab_response::Came as TabCame;
use lunetad::api::turn_into_tab_response::{Done as TabDone, NoClient};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::{OtherTabsRequest, TurnIntoPanesRequest, TurnIntoTabRequest};
use serde_json::{json, Value};
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::host::{folder_in, start, Host};
use crate::sessions::listed;

/// A terminal of the session, in the tab `tab`, as zellij lists it.
fn pane(id: u32, tab: (u32, &str)) -> Value {
    listed(id, "fish", tab, Some("/w"))
}

fn floating(mut pane: Value) -> Value {
    pane["is_floating"] = json!(true);
    pane
}

fn plugin(mut pane: Value) -> Value {
    pane["is_plugin"] = json!(true);
    pane
}

/// Has the daemon of `host` answer what `asked` asks of it.
fn asking<T>(host: &Host, asked: impl AsyncFnOnce(DaemonClient<Channel>) -> T) -> T {
    let _daemon = start(host.vars()).unwrap();
    let folder = folder_in(host.runtime.path()).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let daemon = lunetad::connect(&folder).await.unwrap();
        asked(daemon).await
    })
}

/// When the session started, as a question says it. No socket lists `work` in
/// most of these, so no session took its name.
fn long_ago() -> prost_types::Timestamp {
    SystemTime::UNIX_EPOCH.into()
}

/// What the daemon of `host` answers as it is asked to turn the terminal
/// `pane` of `work` into a tab.
fn into_tab(host: &Host, pane: u32) -> Result<Option<TabCame>, Status> {
    let session = "work".to_owned();
    let asked = TurnIntoTabRequest { on: None, session, pane, started_at: Some(long_ago()) };
    asking(host, async move |mut daemon| {
        daemon.turn_into_tab(asked).await.map(|answer| answer.into_inner().came)
    })
}

/// What the daemon of `host` answers as it is asked to turn the tab of the
/// terminal `pane` of `work` into panes of the tab of the terminal `beside`.
fn into_panes(host: &Host, pane: u32, beside: u32) -> Result<Option<PanesCame>, Status> {
    let session = "work".to_owned();
    let asked =
        TurnIntoPanesRequest { on: None, session, pane, beside, started_at: Some(long_ago()) };
    asking(host, async move |mut daemon| {
        daemon.turn_into_panes(asked).await.map(|answer| answer.into_inner().came)
    })
}

fn refused(status: &Status, why: &str) {
    assert_eq!(status.code(), Code::FailedPrecondition, "{status:?}");
    assert!(status.message().contains(why), "{status:?}");
}

const TAB_DONE: Option<TabCame> = Some(TabCame::Done(TabDone {}));
const PANES_DONE: Option<PanesCame> = Some(PanesCame::Done(PanesDone {}));

fn action(said: &str) -> String {
    format!("--session work action {said}")
}

/// A stand-in as zellij is asked to open one in the tab `tab`.
fn stand_in(tab: u32) -> String {
    opened_as(tab, "luneta stand-in")
}

/// What opens the second stand-in of a pane no client is taken to, in the tab `tab`.
fn second_stand_in(tab: u32) -> String {
    opened_as(tab, "luneta second stand-in")
}

/// A pane named `name` as zellij is asked to open a stand-in in the tab `tab`.
fn opened_as(tab: u32, name: &str) -> String {
    let flags = "--no-focus --close-on-exit";
    let said = format!("new-pane --tab-id {tab} {flags} --name={name} -- sleep 10");
    format!("- {}", action(&said))
}

/// What has zellij stack `pane` on the stand-in it opened: its fake says each
/// is `terminal_9`.
fn on_stand_in(pane: &str) -> String {
    action(&format!("stack-panes -- terminal_9 {pane}"))
}

/// What has zellij stack `pane` on the stand-in it opened with no client moved:
/// it focuses the last it is told to stack, and that is none of the session's.
fn unfocused(pane: &str) -> String {
    action(&format!("stack-panes -- terminal_9 {pane} terminal_4294967295"))
}

fn stand_in_closed() -> String {
    action("close-pane --pane-id terminal_9")
}

/// A pane moves onto what a tab made for it holds, which then closes. The tab
/// is named as the one it left, and moves no client, whatever the name starts
/// with, as zellij's CLI takes no `-` for a value it does not find joined to its
/// option. What holds it in the meanwhile ends on its own, and closes it, where
/// luneta is gone by then.
#[test]
fn a_pane_turns_into_a_tab_named_as_the_one_it_left() {
    for name in ["auth", "-work"] {
        let host = Host::new();
        let old = (0, name);
        host.zellij.list("work", &json!([pane(1, old), pane(2, old)]));
        host.zellij.clients("work", &[(1, "terminal_1")]);
        host.zellij.opens_into("work", &json!([pane(1, old), pane(2, old), pane(4, (3, name))]));
        let new = (3, name);
        host.zellij.stacks_into("work", 1, &json!([pane(2, old), pane(1, new), pane(4, new)]));

        assert_eq!(into_tab(&host, 1).unwrap(), TAB_DONE);

        let opened = action(&format!(
            "new-tab --no-focus --close-on-exit --name={name} -- sleep 10"
        ));
        assert_eq!(host.zellij.opened(), [format!("- {opened}")]);
        assert_eq!(host.zellij.turned(), [action("stack-panes -- terminal_4 terminal_1")]);
        assert_eq!(host.zellij.closed(), [action("close-pane --pane-id terminal_4")]);
    }
}

/// A pane stacked with no client taken to it has all the room of the stack, and
/// closing a stand-in of one row would leave it a stack of one, which zellij
/// splits for nothing after it. A second stand-in takes the room from it, and goes
/// after the first, which leaves it a pane like any other.
#[test]
fn a_pane_no_client_is_taken_to_ends_a_pane_like_any_other() {
    let host = Host::new();
    let (old_tab, new) = ((0, "Tab #1"), (3, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, old_tab), pane(2, old_tab)]));
    host.zellij.clients("work", &[(1, "terminal_2")]);
    host.zellij.opens_into("work", &json!([pane(1, old_tab), pane(2, old_tab), pane(4, new)]));
    host.zellij.stacks_into("work", 1, &json!([pane(2, old_tab), pane(1, new), pane(4, new)]));

    assert_eq!(into_tab(&host, 1).unwrap(), TAB_DONE);

    let nowhere = "terminal_4294967295";
    let stacked = action(&format!("stack-panes -- terminal_4 terminal_1 {nowhere}"));
    assert_eq!(host.zellij.turned(), [stacked]);
    let tucked = action(&format!("stack-panes -- terminal_4 terminal_9 {nowhere}"));
    assert_eq!(host.zellij.tucked(), [tucked]);
    let first = action("close-pane --pane-id terminal_4");
    assert_eq!(host.zellij.closed(), [first, stand_in_closed()]);
}

/// A tab zellij opened as a pane closing cut its answer short is asked for
/// again, not said to have failed: the first holds only a stand-in, which ends
/// on its own, and the tab with it.
#[test]
fn a_tab_a_closing_pane_cut_the_answer_of_is_asked_for_again() {
    let host = Host::new();
    let (old, new) = ((0, "Tab #1"), (3, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, old), pane(2, old)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.opens_into("work", &json!([pane(1, old), pane(2, old), pane(4, new)]));
    host.zellij.stacks_into("work", 1, &json!([pane(2, old), pane(1, new), pane(4, new)]));
    host.zellij.silences("work", "new-tab");

    assert_eq!(into_tab(&host, 1).unwrap(), TAB_DONE);

    let opened = action("new-tab --no-focus --close-on-exit -- sleep 10");
    assert_eq!(host.zellij.opened(), [format!("- {opened}"), format!("- {opened}")]);
    assert_eq!(host.zellij.turned(), [action("stack-panes -- terminal_4 terminal_1")]);
}

/// zellij says nothing of a stack it has no room for: the pane stays where it
/// was, and what holds the tab made for it closes, which takes the tab along.
/// luneta closes no tab: one late to land there would end with it.
#[test]
fn a_pane_zellij_finds_no_room_for_stays_and_only_its_stand_in_closes() {
    let host = Host::new();
    let tab = (0, "Tab #1");
    host.zellij.list("work", &json!([pane(1, tab), pane(2, tab)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.opens_into("work", &json!([pane(1, tab), pane(2, tab), pane(4, (3, "Tab #2"))]));

    refused(&into_tab(&host, 1).unwrap_err(), "no room for terminal_1 in tab 3");

    // A tab zellij numbered gives no name to the one made for its pane.
    let opened = "- --session work action new-tab --no-focus --close-on-exit -- sleep 10";
    assert_eq!(host.zellij.opened(), [opened]);
    assert_eq!(host.zellij.closed(), [action("close-pane --pane-id terminal_4")]);
}

/// zellij says nothing of a pane that ended either: it is not said to have had
/// no room, and what holds the tab made for it closes all the same.
#[test]
fn a_pane_that_ends_as_it_turns_into_a_tab_is_said_to_have_gone() {
    let host = Host::new();
    let tab = (0, "Tab #1");
    host.zellij.list("work", &json!([pane(1, tab), pane(2, tab)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.opens_into("work", &json!([pane(1, tab), pane(2, tab), pane(4, (3, "Tab #2"))]));
    host.zellij.stacks_into("work", 1, &json!([pane(2, tab), pane(4, (3, "Tab #2"))]));

    refused(&into_tab(&host, 1).unwrap_err(), "pane 1 is gone");

    assert_eq!(host.zellij.closed(), [action("close-pane --pane-id terminal_4")]);
}

/// What stays asks zellij for nothing: no tab opens beside a pane that has
/// nothing to do, whatever clients its session has.
#[test]
fn what_stays_opens_no_tab() {
    let tab = (0, "Tab #1");
    for (panes, why) in [
        (json!([pane(1, tab), pane(2, (1, "Tab #2"))]), "pane 1 is alone in its tab"),
        (json!([pane(1, tab), floating(pane(2, tab))]), "floating panes would end"),
    ] {
        let host = Host::new();
        host.zellij.list("work", &panes);
        refused(&into_tab(&host, 1).unwrap_err(), why);
        assert!(host.zellij.opened().is_empty());
        assert!(host.zellij.turned().is_empty());
    }
}

/// zellij makes a tab with no room for its panes in a session no client is
/// in: the answer has a client come first, and nothing has moved.
#[test]
fn a_session_no_client_is_in_has_one_come_first() {
    let host = Host::new();
    let tab = (0, "Tab #1");
    host.zellij.list("work", &json!([pane(1, tab), pane(2, tab)]));

    assert_eq!(into_tab(&host, 1).unwrap(), Some(TabCame::NoClient(NoClient {})));

    assert!(host.zellij.opened().is_empty());
    assert!(host.zellij.turned().is_empty());
}

/// The panes of a tab go one at a time onto a stand-in opened for each in the
/// tab of the terminal picked, which then closes and leaves it tiled, the
/// floating ones ahead of the last tiled one, whose leaving would end them. None
/// floats on the way. A client's zellij closes the tab they left of itself.
#[test]
fn the_panes_of_a_tab_turn_into_panes_of_another_tiled() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    let (tool, moved_tool) = (plugin(pane(7, from)), plugin(pane(7, to)));
    let picked = pane(3, to);
    let there = |panes: &[&Value]| {
        let all = [&picked].into_iter().chain(panes.iter().copied());
        Value::Array(all.cloned().collect())
    };
    let (one, five) = (pane(1, from), floating(pane(5, from)));
    let (one_there, five_there) = (pane(1, to), pane(5, to));
    host.zellij.list("work", &there(&[&one, &five, &tool]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.stacks_into("work", 1, &there(&[&one, &five_there, &tool]));
    host.zellij.stacks_into("work", 2, &there(&[&one_there, &five_there, &tool]));
    host.zellij.stacks_into("work", 3, &there(&[&one_there, &five_there, &moved_tool]));

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    // One for each, and another for each that no client went to.
    let (first, second) = (stand_in(1), second_stand_in(1));
    let opened = [first.clone(), second.clone(), first.clone(), first, second];
    assert_eq!(host.zellij.opened(), opened);
    let stacked = [unfocused("terminal_5"), on_stand_in("terminal_1"), unfocused("plugin_7")];
    assert_eq!(host.zellij.turned(), stacked);
    assert_eq!(host.zellij.closed(), vec![stand_in_closed(); 5]);
}

/// A session no client is in lists the tab its panes left on, with nothing in
/// it, as zellij closes one only as it draws for a client: luneta closes it.
#[test]
fn a_session_no_client_is_in_loses_the_tab_they_left_too() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(3, to)]));
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    let closed = [stand_in_closed(), stand_in_closed(), action("close-tab-by-id 0")];
    assert_eq!(host.zellij.closed(), closed);
}

/// A pane zellij finds no room for stops the rest, with what came before it
/// tiled where it went, its stand-in gone, and the tab that still holds it
/// staying. The answer says how many moved, as some did.
#[test]
fn a_pane_zellij_finds_no_room_for_stays_in_its_tab_and_the_answer_says_some_moved() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    let laid = json!([pane(1, from), pane(2, from), pane(4, from), pane(3, to)]);
    host.zellij.list("work", &laid);
    let first_there = json!([pane(1, to), pane(2, from), pane(4, from), pane(3, to)]);
    host.zellij.stacks_into("work", 1, &first_there);

    let came = into_panes(&host, 1, 3).unwrap();

    let why = "zellij found no room for terminal_2 in tab 1, so it stays".to_owned();
    assert_eq!(came, Some(PanesCame::Partly(Partly { moved: 1, of: 3, why })));
    let stacked = [unfocused("terminal_1"), unfocused("terminal_2"), on_stand_in("terminal_2")];
    assert_eq!(host.zellij.turned(), stacked);
    // The first moved, with a second stand-in, and the last did not.
    assert_eq!(host.zellij.closed(), vec![stand_in_closed(); 3]);
}

/// With no room for the first, nothing moved, which is a refusal and no answer
/// of how many did.
#[test]
fn a_tab_zellij_finds_no_room_for_moves_nothing() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(2, from), pane(3, to)]));

    refused(&into_panes(&host, 1, 3).unwrap_err(), "no room for terminal_1 in tab 1");

    assert_eq!(host.zellij.turned(), [unfocused("terminal_1"), on_stand_in("terminal_1")]);
    assert_eq!(host.zellij.closed(), [stand_in_closed()]);
}

/// A pane that ended on its way is no reason to stop, nor to say that zellij
/// had no room: the rest go, and all that were there have moved.
#[test]
fn a_pane_that_ends_on_its_way_does_not_stop_the_rest() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(2, from), pane(3, to)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.stacks_into("work", 1, &json!([pane(2, from), pane(3, to)]));
    host.zellij.stacks_into("work", 2, &json!([pane(2, to), pane(3, to)]));

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    assert_eq!(host.zellij.turned(), [on_stand_in("terminal_1"), unfocused("terminal_2")]);
}

/// A pane closing cuts the answer of a stand-in short, which opens all the same:
/// it is found by its name, and no second opens to stand in the tab till it ends.
#[test]
fn a_stand_in_whose_answer_was_cut_short_is_found_by_its_name() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    let mut opened = pane(8, to);
    opened["title"] = json!("luneta stand-in");
    host.zellij.list("work", &json!([pane(1, from), pane(3, to), opened]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));
    host.zellij.silences("work", "new-pane");

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    assert_eq!(host.zellij.opened(), [stand_in(1)]);
    assert_eq!(host.zellij.turned(), [action("stack-panes -- terminal_8 terminal_1")]);
    assert_eq!(host.zellij.closed(), [action("close-pane --pane-id terminal_8")]);
}

/// A session of `work` whose one pane in the tab of terminal 1 moves to the tab
/// of terminal 3 once zellij stacks it, with a client there.
fn one_pane_to_move() -> Host {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(3, to)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));
    host
}

/// Each move closes a stand-in, and zellij cuts other calls short as a pane
/// closes: one that waits ends well with nothing said, though what it asked for
/// goes on, and one yet to reach it asked nothing, which it says as many ways as
/// other sessions answered it, one of them ending well. Each is asked again, so
/// that what came of it is said as it was, and a stack that did go goes once.
#[test]
fn calls_a_closing_pane_cuts_short_are_asked_again() {
    for others in 0..=2 {
        let host = one_pane_to_move();
        host.zellij.silences("work", "list-panes");
        host.zellij.silences("work", "list-clients");
        host.zellij.silences("work", "new-pane");
        for call in ["list-panes", "list-clients", "new-pane", "stack-panes"] {
            host.zellij.unreaches("work", call, others);
        }

        assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

        // The first opened all the same, and ends on its own.
        assert_eq!(host.zellij.opened(), [stand_in(1), stand_in(1)]);
        assert_eq!(host.zellij.turned(), [on_stand_in("terminal_1")]);
    }
}

/// A call can hear what one a closing pane cut short was to, where that ended
/// well and said something: only what reads as its own answer is one.
#[test]
fn an_answer_meant_for_another_call_is_not_taken_for_this_one() {
    let host = one_pane_to_move();
    host.zellij.answers_another("work", "list-panes", "terminal_2055");
    host.zellij.answers_another("work", "list-clients", "terminal_2055");
    host.zellij.answers_another("work", "new-pane", "[]");

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    assert_eq!(host.zellij.turned(), [on_stand_in("terminal_1")]);
}

/// What closes a stand-in, or the tab that panes left, is asked again too where
/// it asked nothing: it ends well with two other sessions to list.
#[test]
fn closes_that_reached_no_server_are_asked_again() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(3, to)]));
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));
    host.zellij.unreaches("work", "close-pane", 2);
    host.zellij.unreaches("work", "close-tab-by-id", 2);

    assert_eq!(into_panes(&host, 1, 3).unwrap(), PANES_DONE);

    let closed = [stand_in_closed(), stand_in_closed(), action("close-tab-by-id 0")];
    assert_eq!(host.zellij.closed(), closed);
}

/// The terminal a tab was picked by may have closed by the key: nothing moves,
/// and no stand-in opens.
#[test]
fn a_tab_picked_by_a_terminal_that_closed_since_moves_nothing() {
    let host = Host::new();
    let (from, reused) = ((0, "Tab #1"), (1, "someone else's"));
    host.zellij.list("work", &json!([pane(1, from), pane(2, from), pane(8, reused)]));

    refused(&into_panes(&host, 1, 3).unwrap_err(), "pane 3, which the tab goes by, is gone");

    assert!(host.zellij.opened().is_empty());
    assert!(host.zellij.turned().is_empty());
}

/// zellij numbers panes from 0 for each server, so a session that took the name
/// since has panes of the same ids: nothing of it moves, and zellij is asked
/// for nothing. A question that says no start says nothing of which it means.
#[test]
fn a_session_that_took_the_name_since_stays() {
    let host = Host::new();
    let _work = host.serve("work");
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(2, from), pane(3, to)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    let session = || "work".to_owned();
    let tab = |started_at| TurnIntoTabRequest { on: None, session: session(), pane: 1, started_at };
    let panes = |started_at| TurnIntoPanesRequest {
        on: None,
        session: session(),
        pane: 1,
        beside: 3,
        started_at,
    };
    asking(&host, async move |mut daemon| {
        let since = "a session made since";
        refused(&daemon.turn_into_tab(tab(Some(long_ago()))).await.unwrap_err(), since);
        refused(&daemon.turn_into_panes(panes(Some(long_ago()))).await.unwrap_err(), since);
        let unsaid = daemon.turn_into_tab(tab(None)).await.unwrap_err();
        assert_eq!(unsaid.code(), Code::InvalidArgument, "{unsaid:?}");
        let unsaid = daemon.turn_into_panes(panes(None)).await.unwrap_err();
        assert_eq!(unsaid.code(), Code::InvalidArgument, "{unsaid:?}");
    });
    assert!(host.zellij.opened().is_empty());
    assert!(host.zellij.turned().is_empty());
}

/// The tabs a tab can turn into are told by a terminal in each and their names,
/// and a pane that is gone has no tab to leave out.
#[test]
fn the_other_tabs_are_told_by_a_terminal_and_their_names() {
    let host = Host::new();
    let (auth, two) = ((0, "auth"), (5, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, auth), pane(2, auth), pane(3, two)]));
    let tabs = |pane| OtherTabsRequest { on: None, session: "work".to_owned(), pane };
    asking(&host, async move |mut daemon| {
        let listed = daemon.other_tabs(tabs(1)).await.unwrap().into_inner();
        assert_eq!(listed.tabs, [Tab { pane: 3, name: "Tab #2".to_owned() }]);
        let gone = daemon.other_tabs(tabs(9)).await.unwrap_err();
        refused(&gone, "pane 9 is gone");
    });
}
