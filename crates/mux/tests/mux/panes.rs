use luneta_mux::api::{self, to_server::Event};
use tonic::Code;

use crate::host::{
    Attached, Server, listed, new_session, new_tab, run, script, session, shell, size, tabs,
    watched_until,
};

fn rect(column: u32, row: u32, columns: u32, rows: u32) -> api::Rect {
    api::Rect { column, row, columns, rows }
}

fn rects(layout: &api::Layout) -> Vec<(u32, api::Rect)> {
    layout.panes.iter().map(|placed| (placed.pane, placed.rect.unwrap())).collect()
}

fn open(towards: api::Towards) -> Event {
    Event::Open(api::Open { at: Some(api::open::At::Beside(towards.into())) })
}

fn focus(direction: api::Direction) -> Event {
    Event::FocusPane(api::FocusPane { to: Some(api::focus_pane::To::Towards(direction.into())) })
}

fn into_new_tab(name: Option<&str>) -> api::Into {
    let name = name.map(str::to_owned);
    api::Into { is: Some(api::into::Is::NewTab(api::NewTab { name })) }
}

fn into_tab(tab: u32) -> api::Into {
    api::Into { is: Some(api::into::Is::Tab(tab)) }
}

fn beside(pane: u32, towards: api::Towards) -> api::new_pane_request::At {
    api::new_pane_request::At::Beside(api::Beside { pane, towards: towards.into() })
}

fn panes_of(listing: &api::Listing, tab: u32) -> Vec<u32> {
    let found = tabs(listing).iter().find(|found| found.id == tab);
    found.map_or_else(Vec::new, |tab| tab.panes.iter().map(|pane| pane.id).collect())
}

async fn opened(attached: &mut Attached, towards: api::Towards) -> u32 {
    let first = attached.went.pane;
    attached.send(open(towards)).await;
    attached.until("the client went to a new pane", |attached| attached.went.pane != first).await;
    attached.went.pane
}

async fn refusal(attached: &mut Attached) -> String {
    loop {
        match attached.next().await {
            Some(api::to_client::Event::Refused(refused)) => return refused.reason,
            Some(_) => {},
            None => panic!("the stream ended before a refusal"),
        }
    }
}

#[test]
fn a_split_halves_the_pane_and_the_client_goes_to_the_new_one() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        attached
            .until_laid("two panes side by side", |layout| {
                rects(layout) == [(made.pane, rect(0, 0, 40, 24)), (second, rect(40, 0, 40, 24))]
            })
            .await;
        assert_eq!(attached.layout.as_ref().unwrap().focused, second);
        attached.type_line("stty size").await;
        attached.until_pane_row(second, "22 38").await;
        assert_eq!(panes_of(&listed(&mut mux).await, made.tab), [made.pane, second]);
        attached.send(focus(api::Direction::Left)).await;
        attached.until("the focus went left", |attached| attached.went.pane == made.pane).await;
        attached.type_line("stty size").await;
        attached.until_pane_row(made.pane, "22 38").await;
    });
}

#[test]
fn a_closed_pane_leaves_its_room_and_its_clients_to_its_sibling() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        opened(&mut attached, api::Towards::Down).await;
        attached.send(Event::ClosePane(api::ClosePane {})).await;
        attached.until("the client went back", |attached| attached.went.pane == made.pane).await;
        attached
            .until_laid("the one pane in the whole tab", |layout| {
                rects(layout) == [(made.pane, rect(0, 0, 80, 24))]
            })
            .await;
        attached.type_line("stty size").await;
        attached.until_row("22 78").await;
        assert_eq!(panes_of(&listed(&mut mux).await, made.tab), [made.pane]);
    });
}

#[test]
fn a_new_pane_goes_beside_a_pane_or_in_a_tab_s_largest_and_never_where_there_is_no_room() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let request = |at| api::NewPaneRequest { at: Some(at), opens: Some(shell()) };
        let down = beside(made.pane, api::Towards::Down);
        let second = mux.new_pane(request(down)).await.unwrap().into_inner().pane;
        let at_tab = api::new_pane_request::At::Tab(made.tab);
        let third = mux.new_pane(request(at_tab)).await.unwrap().into_inner().pane;
        attached
            .until_laid("the largest pane split along its longer side", |layout| {
                rects(layout)
                    == [
                        (made.pane, rect(0, 0, 40, 12)),
                        (third, rect(40, 0, 40, 12)),
                        (second, rect(0, 12, 80, 12)),
                    ]
            })
            .await;
        assert_eq!(attached.went.pane, made.pane, "a pane made by a call moves no client");
        let small = new_session(&mut mux, "small", shell()).await;
        let _cramped = Attached::new(&mut mux, session(&small), size(9, 5)).await;
        let full = |status: tonic::Status| (status.code(), status.message().to_owned());
        let no_room =
            (Code::FailedPrecondition, format!("tab {} has no room for another pane", small.tab));
        let right = beside(small.pane, api::Towards::Right);
        assert_eq!(full(mux.new_pane(request(right)).await.unwrap_err()), no_room);
        let largest = api::new_pane_request::At::Tab(small.tab);
        assert_eq!(full(mux.new_pane(request(largest)).await.unwrap_err()), no_room);
        let nowhere = beside(99, api::Towards::Right);
        let absent = (Code::NotFound, "no pane 99".to_owned());
        assert_eq!(full(mux.new_pane(request(nowhere)).await.unwrap_err()), absent);
        let unsaid = beside(made.pane, api::Towards::Unspecified);
        let unsaid = full(mux.new_pane(request(unsaid)).await.unwrap_err());
        let said = "at.beside.towards is not one of RIGHT or DOWN".to_owned();
        assert_eq!(unsaid, (Code::InvalidArgument, said));
    });
}

#[test]
fn a_program_that_ends_closes_only_its_own_pane() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let held = api::NewPaneRequest {
            at: Some(beside(made.pane, api::Towards::Right)),
            opens: Some(script("exit 3", api::Ends::Holds)),
        };
        let held = mux.new_pane(held).await.unwrap().into_inner().pane;
        let listing = watched_until(&mut watch, "the held pane's end", |listing| {
            tabs(listing)[0].panes.iter().any(|pane| crate::host::ended(pane).is_some())
        })
        .await;
        assert_eq!(panes_of(&listing, made.tab), [made.pane, held]);
        let closes = api::NewPaneRequest {
            at: Some(beside(made.pane, api::Towards::Down)),
            opens: Some(script("sleep 0.2", api::Ends::Closes)),
        };
        let closes = mux.new_pane(closes).await.unwrap().into_inner().pane;
        watched_until(&mut watch, "the second pane", |listing| {
            panes_of(listing, made.tab).contains(&closes)
        })
        .await;
        watched_until(&mut watch, "the pane gone with its program", |listing| {
            panes_of(listing, made.tab) == [made.pane, held]
        })
        .await;
    });
}

#[test]
fn a_resize_moves_the_border_and_the_panes_take_their_new_sizes() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        let resize = |change: api::Change, towards: Option<api::Direction>| {
            Event::ResizePane(api::ResizePane {
                change: change.into(),
                towards: towards.map(Into::into),
            })
        };
        attached.send(resize(api::Change::Grow, Some(api::Direction::Left))).await;
        attached
            .until_laid("the border moved left", |layout| {
                rects(layout) == [(made.pane, rect(0, 0, 36, 24)), (second, rect(36, 0, 44, 24))]
            })
            .await;
        attached.type_line("stty size").await;
        attached.until_pane_row(second, "22 42").await;
        attached.send(resize(api::Change::Shrink, None)).await;
        attached
            .until_laid("the border back", |layout| {
                rects(layout) == [(made.pane, rect(0, 0, 40, 24)), (second, rect(40, 0, 40, 24))]
            })
            .await;
    });
}

#[test]
fn a_fullscreen_pane_takes_the_tab_until_the_focus_moves() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        attached.send(Event::Fullscreen(api::Fullscreen {})).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        assert!(!attached.layout.as_ref().unwrap().fullscreen, "a lone pane has no fullscreen");
        let first = api::Place { is: Some(api::place::Is::Pane(made.pane)), made_at: made.made_at };
        let mut other = Attached::new(&mut mux, first, size(80, 24)).await;
        assert_eq!(other.went.pane, made.pane);
        attached.send(Event::Fullscreen(api::Fullscreen {})).await;
        attached
            .until_laid("the pane over the whole tab", |layout| {
                layout.fullscreen && rects(layout) == [(second, rect(0, 0, 80, 24))]
            })
            .await;
        other
            .until_laid("the other client on the pane that fills the tab", |layout| {
                layout.fullscreen && layout.focused == second
            })
            .await;
        assert_eq!(other.went.pane, second);
        attached.type_line("stty size").await;
        attached.until_pane_row(second, "22 78").await;
        attached.send(focus(api::Direction::Left)).await;
        attached
            .until_laid("both panes again", |layout| {
                !layout.fullscreen && layout.focused == made.pane && layout.panes.len() == 2
            })
            .await;
    });
}

#[test]
fn a_moved_pane_trades_places_and_keeps_the_focus() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        let towards = api::move_pane::To::Towards(api::Direction::Left.into());
        attached.send(Event::MovePane(api::MovePane { to: Some(towards) })).await;
        attached
            .until_laid("the panes traded places", |layout| {
                rects(layout) == [(second, rect(0, 0, 40, 24)), (made.pane, rect(40, 0, 40, 24))]
            })
            .await;
        assert_eq!(attached.went.pane, second);
        assert_eq!(panes_of(&listed(&mut mux).await, made.tab), [second, made.pane]);
    });
}

#[test]
fn names_of_panes_and_tabs_show_in_the_layout_and_the_listing() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let two = new_tab(&mut mux, made.session, shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let named =
            |name: Option<&str>| Event::NamePane(api::NamePane { name: name.map(str::to_owned) });
        attached.send(named(Some("logs"))).await;
        attached.until_laid("the pane's name", |layout| layout.panes[0].title == "logs").await;
        let pane = |listing: &api::Listing| tabs(listing)[0].panes[0].name.clone();
        assert_eq!(pane(&listed(&mut mux).await), Some("logs".to_owned()));
        attached.send(named(None)).await;
        attached.until_laid("the name gone", |layout| layout.panes[0].title != "logs").await;
        assert_eq!(pane(&listed(&mut mux).await), None);
        let tab = Event::NameTab(api::NameTab { name: "build".to_owned() });
        attached.send(tab).await;
        attached.until_laid("the tab's name", |layout| layout.tabs[0].name == "build").await;
        let right = Event::MoveTab(api::MoveTab { towards: api::Direction::Right.into() });
        attached.send(right).await;
        attached
            .until_laid("the tab moved right", |layout| {
                layout.tabs.iter().map(|tab| tab.id).collect::<Vec<_>>() == [two.tab, made.tab]
            })
            .await;
        let order =
            |listing: &api::Listing| tabs(listing).iter().map(|tab| tab.id).collect::<Vec<_>>();
        assert_eq!(order(&listed(&mut mux).await), [two.tab, made.tab]);
        assert_eq!(listed(&mut mux).await.sessions[0].current_tab, made.tab);
    });
}

#[test]
fn a_pane_turns_into_a_tab_and_back_and_its_clients_follow_it() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        attached.send(Event::Turn(into_new_tab(Some("side")))).await;
        attached
            .until("the client went with its pane", |attached| attached.went.tab != made.tab)
            .await;
        let side = attached.went.tab;
        assert_eq!(attached.went.pane, second);
        let listing = listed(&mut mux).await;
        let split = (panes_of(&listing, made.tab), panes_of(&listing, side));
        assert_eq!(split, (vec![made.pane], vec![second]));
        assert_eq!(tabs(&listing)[1].name, "side");
        attached
            .until_laid("the pane alone in its tab", |layout| {
                rects(layout) == [(second, rect(0, 0, 80, 24))]
            })
            .await;
        let back = api::TurnRequest { pane: second, into: Some(into_tab(made.tab)) };
        assert_eq!(mux.turn(back).await.unwrap().into_inner().tab, made.tab);
        attached.until("the client went back", |attached| attached.went.tab == made.tab).await;
        let listing = listed(&mut mux).await;
        assert_eq!(tabs(&listing).len(), 1, "the tab the pane left alone closed");
        assert_eq!(panes_of(&listing, made.tab), [made.pane, second]);
        attached.type_line("echo $((6 * 7))").await;
        attached.until_pane_row(second, "42").await;
        let refused = |status: tonic::Status| (status.code(), status.message().to_owned());
        let solo = new_session(&mut mux, "solo", shell()).await;
        let turn = |pane, into| api::TurnRequest { pane, into: Some(into) };
        let alone = mux.turn(turn(solo.pane, into_new_tab(None))).await.unwrap_err();
        let said = format!("pane {} is alone in its tab", solo.pane);
        assert_eq!(refused(alone), (Code::FailedPrecondition, said.clone()));
        let there = mux.turn(turn(second, into_tab(made.tab))).await.unwrap_err();
        let already = format!("pane {second} is in tab {} already", made.tab);
        assert_eq!(refused(there), (Code::FailedPrecondition, already));
        let away = mux.turn(turn(second, into_tab(solo.tab))).await.unwrap_err();
        let other = format!("tab {} is in a session other than pane {second}'s", solo.tab);
        assert_eq!(refused(away), (Code::FailedPrecondition, other));
        let mut lone = Attached::new(&mut mux, session(&solo), size(80, 24)).await;
        lone.send(Event::Turn(into_new_tab(None))).await;
        assert_eq!(refusal(&mut lone).await, said);
    });
}

#[test]
fn the_focus_crosses_to_the_next_tab_from_the_edge_and_goes_to_a_pane_by_its_id() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let two = new_tab(&mut mux, made.session, shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(80, 24)).await;
        let second = opened(&mut attached, api::Towards::Right).await;
        let or_tab = |direction: api::Direction| {
            let to = api::focus_pane::To::TowardsOrTab(direction.into());
            Event::FocusPane(api::FocusPane { to: Some(to) })
        };
        attached.send(or_tab(api::Direction::Right)).await;
        attached
            .until("the client crossed to tab two", |attached| attached.went.tab == two.tab)
            .await;
        attached.send(or_tab(api::Direction::Left)).await;
        attached.until("the client crossed back", |attached| attached.went.tab == made.tab).await;
        assert_eq!(attached.went.pane, second, "a tab keeps the pane last in focus");
        let pane = api::Place { is: Some(api::place::Is::Pane(made.pane)), made_at: made.made_at };
        attached.send(Event::Go(pane)).await;
        attached
            .until("the client went to the pane", |attached| attached.went.pane == made.pane)
            .await;
        attached.type_line("echo $((6 * 7))").await;
        attached.until_pane_row(made.pane, "42").await;
    });
}
