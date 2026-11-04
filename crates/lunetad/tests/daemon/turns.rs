//! What zellij is asked to move a pane into a tab of its own, or the panes of
//! a tab into another, as the daemon answers `Turn` and `OtherTabs`: zellij's
//! CLI has no command for it, and what it does lies in the order of what it
//! is asked, as those are measured against a real zellij.

use lunetad::api::other_tabs_response::Tab;
use lunetad::api::turn_request::{Becomes, NewTab};
use lunetad::api::daemon_client::DaemonClient;
use lunetad::api::{OtherTabsRequest, TurnRequest, TurnResponse};
use serde_json::{json, Value};
use tonic::transport::Channel;
use tonic::{Code, Status};

use crate::host::{folder_in, start, Host};
use crate::sessions::listed;

/// A terminal of the session, in the tab `tab`, as zellij lists it.
fn pane(id: u32, tab: (u32, &str)) -> Value {
    let mut pane = listed(id, "fish", tab, Some("/w"));
    pane["pane_rows"] = json!(30);
    pane
}

fn floating(mut pane: Value) -> Value {
    pane["is_floating"] = json!(true);
    pane
}

fn plugin(mut pane: Value) -> Value {
    pane["is_plugin"] = json!(true);
    pane
}

const NEW_TAB: Becomes = Becomes::NewTab(NewTab {});

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

/// What the daemon of `host` answers as it is asked to turn the terminal
/// `pane` of `work`.
fn turned(host: &Host, pane: u32, becomes: Option<Becomes>) -> Result<TurnResponse, Status> {
    let session = "work".to_owned();
    let asked = TurnRequest { on: None, session, pane, becomes };
    asking(host, async move |mut daemon| daemon.turn(asked).await.map(tonic::Response::into_inner))
}

fn refused(status: &Status, why: &str) {
    assert_eq!(status.code(), Code::FailedPrecondition, "{status:?}");
    assert!(status.message().contains(why), "{status:?}");
}

/// A pane moves onto what a tab made for it holds, which then closes. The tab
/// is named as the one it left, and moves no client. What holds it in the
/// meanwhile ends on its own, and closes it, where luneta is gone by then.
#[test]
fn a_pane_turns_into_a_tab_named_as_the_one_it_left() {
    let host = Host::new();
    let auth = (0, "auth");
    host.zellij.list("work", &json!([pane(1, auth), pane(2, auth)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.opens_into("work", &json!([pane(1, auth), pane(2, auth), pane(4, (3, "auth"))]));
    let new = (3, "auth");
    host.zellij.stacks_into("work", 1, &json!([pane(2, auth), pane(1, new), pane(4, new)]));

    assert!(!turned(&host, 1, Some(NEW_TAB)).unwrap().no_client);

    let opened =
        "- --session work action new-tab --no-focus --close-on-exit --name auth -- sleep 10";
    assert_eq!(host.zellij.opened(), [opened]);
    let stacked = "--session work action stack-panes -- terminal_4 terminal_1";
    assert_eq!(host.zellij.turned(), [stacked]);
    assert_eq!(host.zellij.closed(), ["--session work action close-pane --pane-id terminal_4"]);
}

/// zellij says nothing of a stack it has no room for: the pane stays where it
/// was, and the tab made for it goes rather than stay beside it.
#[test]
fn a_pane_zellij_finds_no_room_for_stays_and_the_tab_made_for_it_goes() {
    let host = Host::new();
    let tab = (0, "Tab #1");
    host.zellij.list("work", &json!([pane(1, tab), pane(2, tab)]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.opens_into("work", &json!([pane(1, tab), pane(2, tab), pane(4, (3, "Tab #2"))]));

    refused(&turned(&host, 1, Some(NEW_TAB)).unwrap_err(), "no room for terminal_1 in tab 3");

    // A tab zellij numbered gives no name to the one made for its pane.
    let opened = "- --session work action new-tab --no-focus --close-on-exit -- sleep 10";
    assert_eq!(host.zellij.opened(), [opened]);
    assert_eq!(host.zellij.closed(), ["--session work action close-tab-by-id 3"]);
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
        refused(&turned(&host, 1, Some(NEW_TAB)).unwrap_err(), why);
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

    assert!(turned(&host, 1, Some(NEW_TAB)).unwrap().no_client);

    assert!(host.zellij.opened().is_empty());
    assert!(host.zellij.turned().is_empty());
}

/// The panes of a tab go one at a time onto the tallest tiled pane of the
/// other, floating there, which takes each out of the stack, the floating ones
/// ahead of the last tiled one, whose leaving would end them. Embedded once
/// all float, they are tiled, as no stack is left to go back to. A client's
/// zellij closes the tab they left of itself.
#[test]
fn the_panes_of_a_tab_turn_into_panes_of_another_tiled() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    let (tool, moved_tool) = (plugin(pane(7, from)), plugin(pane(7, to)));
    let mut short = pane(4, to);
    short["pane_rows"] = json!(10);
    let tall = pane(3, to);
    let there = |panes: &[&Value]| {
        let all = [&tall, &short].into_iter().chain(panes.iter().copied());
        Value::Array(all.cloned().collect())
    };
    let (one, five) = (pane(1, from), floating(pane(5, from)));
    let (one_there, five_there) = (pane(1, to), pane(5, to));
    host.zellij.list("work", &there(&[&one, &five, &tool]));
    host.zellij.clients("work", &[(1, "terminal_1")]);
    host.zellij.stacks_into("work", 1, &there(&[&one, &five_there, &tool]));
    host.zellij.stacks_into("work", 2, &there(&[&one_there, &five_there, &tool]));
    host.zellij.stacks_into("work", 3, &there(&[&one_there, &five_there, &moved_tool]));

    turned(&host, 1, Some(Becomes::PanesOf(1))).unwrap();

    let action = |said: &str| format!("--session work action {said}");
    let toggle = |pane: &str| action(&format!("toggle-pane-embed-or-floating --pane-id {pane}"));
    let stack = |pane: &str| action(&format!("stack-panes -- terminal_3 {pane}"));
    assert_eq!(
        host.zellij.turned(),
        [
            stack("terminal_5"),
            toggle("terminal_5"),
            stack("terminal_1"),
            toggle("terminal_1"),
            stack("plugin_7"),
            toggle("plugin_7"),
            toggle("terminal_5"),
            toggle("terminal_1"),
            toggle("plugin_7"),
        ]
    );
    assert!(host.zellij.closed().is_empty());
}

/// A session no client is in lists the tab its panes left on, with nothing in
/// it, as zellij closes one only as it draws for a client: luneta closes it.
#[test]
fn a_session_no_client_is_in_loses_the_tab_they_left_too() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(3, to)]));
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(3, to)]));

    turned(&host, 1, Some(Becomes::PanesOf(1))).unwrap();

    assert_eq!(host.zellij.closed(), ["--session work action close-tab-by-id 0"]);
}

/// A pane zellij finds no room for is not floated, which would float it in
/// the tab it stays in. Those that came before it are tiled where they went,
/// and the tab that still holds it stays.
#[test]
fn a_pane_zellij_finds_no_room_for_stays_in_its_tab_and_is_not_floated() {
    let host = Host::new();
    let (from, to) = ((0, "Tab #1"), (1, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, from), pane(2, from), pane(3, to)]));
    host.zellij.stacks_into("work", 1, &json!([pane(1, to), pane(2, from), pane(3, to)]));

    refused(
        &turned(&host, 1, Some(Becomes::PanesOf(1))).unwrap_err(),
        "no room for terminal_2 in tab 1",
    );

    let action = |said: &str| format!("--session work action {said}");
    let toggle = action("toggle-pane-embed-or-floating --pane-id terminal_1");
    assert_eq!(
        host.zellij.turned(),
        [
            action("stack-panes -- terminal_3 terminal_1"),
            toggle.clone(),
            action("stack-panes -- terminal_3 terminal_2"),
            toggle,
        ]
    );
    assert!(host.zellij.closed().is_empty());
}

#[test]
fn a_question_that_says_what_it_turns_into_is_the_only_one_answered() {
    let host = Host::new();
    let invalid = turned(&host, 1, None).unwrap_err();
    assert_eq!(invalid.code(), Code::InvalidArgument, "{invalid:?}");
}

/// The tabs a tab can turn into are told by their ids and their names, and a
/// pane that is gone has no tab to leave out.
#[test]
fn the_other_tabs_are_told_by_their_ids_and_names() {
    let host = Host::new();
    let (auth, two) = ((0, "auth"), (5, "Tab #2"));
    host.zellij.list("work", &json!([pane(1, auth), pane(2, auth), pane(3, two)]));
    let tabs = |pane| OtherTabsRequest { on: None, session: "work".to_owned(), pane };
    asking(&host, async move |mut daemon| {
        let listed = daemon.other_tabs(tabs(1)).await.unwrap().into_inner();
        assert_eq!(listed.tabs, [Tab { id: 5, name: "Tab #2".to_owned() }]);
        let gone = daemon.other_tabs(tabs(9)).await.unwrap_err();
        refused(&gone, "pane 9 is gone");
    });
}
