//! Clients: where the server says each is, what it sends each, and what each
//! does in its pane.

use luneta_mux::api;
use tonic::Code;

use crate::host::{
    Attached, Server, listed, new_session, new_tab, run, script, session, shell, size, tab, tabs,
    watched_until,
};

/// A client goes to another tab over its own stream, and is told where it is,
/// then sent that pane's frames. One that asks for the client that last typed
/// in a session takes that one.
#[test]
fn a_client_goes_between_tabs_and_is_told_where_it_is() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made =
            new_session(&mut mux, "work", script("echo one; read x", api::Ends::Holds)).await;
        let two =
            new_tab(&mut mux, made.session, script("echo two; read x", api::Ends::Holds)).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(20, 4)).await;
        attached.until_row("one").await;
        attached.send(api::to_server::Event::Go(tab(two.tab, &made))).await;
        attached
            .until("the client went to the second tab", |attached| attached.went.tab == two.tab)
            .await;
        assert_eq!(attached.went.pane, two.pane);
        attached.until_row("two").await;
        attached.type_line("").await;
        let back = api::GoRequest {
            who: Some(api::go_request::Who::LastTypedIn(made.session)),
            to: Some(tab(made.tab, &made)),
        };
        let went = mux.go(back).await.unwrap().into_inner();
        assert_eq!(went.client, attached.client);
        attached.until("the client went back", |attached| attached.went.tab == made.tab).await;
        attached.until_row("one").await;
    });
}

/// A client that goes over its stream to a tab that closed is told it
/// stays, and why, and stays on its pane, where it goes on typing.
#[test]
fn a_go_to_a_closed_tab_is_told_and_the_client_stays() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let gone = new_tab(&mut mux, made.session, shell()).await;
        mux.close_tab(api::CloseTabRequest { tab: gone.tab }).await.unwrap();
        let mut attached = Attached::new(&mut mux, session(&made), size(40, 10)).await;
        attached.send(api::to_server::Event::Go(tab(gone.tab, &made))).await;
        let reason = loop {
            match attached.next().await {
                Some(api::to_client::Event::Stayed(stayed)) => break stayed.reason,
                Some(_) => {},
                None => panic!("the stream ended before it said the client stayed"),
            }
        };
        assert_eq!(reason, format!("no tab {}", gone.tab));
        assert_eq!(attached.went.tab, made.tab);
        attached.type_line("echo $((6 * 7))").await;
        attached.until_row("42").await;
    });
}

/// A place says when its session was made, as the server said it made it,
/// and is taken where the server made it then. It is refused where at
/// another time, as of a server before this one, and where it says no time:
/// a go to the session or to its tab, and a hello.
#[test]
fn a_place_of_a_session_made_at_another_time_is_refused() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let then = made.made_at.expect("the server says when it made the session");
        assert_eq!(listed(&mut mux).await.sessions[0].made_at, Some(then));
        let before = prost_types::Timestamp { seconds: then.seconds - 1, ..then };
        let session = |made_at| api::Place { made_at, ..crate::host::session(&made) };
        let tab = |made_at| api::Place { made_at, ..crate::host::tab(made.tab, &made) };
        let attached = Attached::new(&mut mux, session(Some(then)), size(40, 10)).await;
        let go = |to| api::GoRequest {
            who: Some(api::go_request::Who::Client(attached.client)),
            to: Some(to),
        };
        let refused = format!("no session {} made then", made.session);
        let places: [&dyn Fn(Option<prost_types::Timestamp>) -> api::Place; 2] = [&session, &tab];
        for place in places {
            let stale = mux.go(go(place(Some(before)))).await.unwrap_err();
            assert_eq!((stale.code(), stale.message()), (Code::NotFound, refused.as_str()));
            let untold = mux.go(go(place(None))).await.unwrap_err();
            let missing = (Code::InvalidArgument, "to.made_at is missing");
            assert_eq!((untold.code(), untold.message()), missing);
            let went = mux.go(go(place(Some(then)))).await.unwrap().into_inner();
            assert_eq!(went.client, attached.client);
        }
        let hello = |to| api::Hello { to: Some(to), size: Some(size(40, 10)), colours: None };
        let Err(stale) = Attached::hello(&mut mux, hello(session(Some(before)))).await else {
            panic!("a client went to a session made at another time");
        };
        assert_eq!((stale.code(), stale.message()), (Code::NotFound, refused.as_str()));
        let Err(untold) = Attached::hello(&mut mux, hello(session(None))).await else {
            panic!("a client went to a session made at no time it said");
        };
        let missing = (Code::InvalidArgument, "hello.to.made_at is missing");
        assert_eq!((untold.code(), untold.message()), missing);
    });
}

/// Two clients on a pane each look at it from where they scrolled to: one
/// back in the scrollback, the other at the bottom.
#[test]
fn each_client_looks_back_in_the_scrollback_on_its_own() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let lines = "i=1; while [ $i -le 30 ]; do echo line$i; i=$((i + 1)); done; read x";
        let made = new_session(&mut mux, "work", script(lines, api::Ends::Holds)).await;
        let mut reading = Attached::new(&mut mux, session(&made), size(20, 5)).await;
        let mut watching = Attached::new(&mut mux, session(&made), size(20, 5)).await;
        reading.until_row("line30").await;
        watching.until_row("line30").await;
        reading.send(api::to_server::Event::Scroll(api::Scroll { back: 10 })).await;
        reading.until_row("line20").await;
        let frame = reading.last.clone().unwrap();
        assert_eq!(frame.scrolled_back, 10);
        assert_eq!(frame.cursor, None, "the cursor is below the view");
        assert!(watching.rows.iter().any(|row| row == "line30"), "{}", watching.text());
        let back = watching.last.clone().unwrap();
        assert_eq!(back.scrolled_back, 0);
    });
}

/// A client back in the scrollback stays on the rows it looks at as the
/// program writes more below them.
#[test]
fn a_client_back_in_the_scrollback_stays_on_its_rows() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let lines = "i=1; while [ $i -le 30 ]; do echo line$i; i=$((i + 1)); done; read x; \
                     echo more1; echo more2; echo more3; read x";
        let made = new_session(&mut mux, "work", script(lines, api::Ends::Holds)).await;
        let mut reading = Attached::new(&mut mux, session(&made), size(20, 5)).await;
        let mut typing = Attached::new(&mut mux, session(&made), size(20, 5)).await;
        reading.until_row("line30").await;
        reading.send(api::to_server::Event::Scroll(api::Scroll { back: 10 })).await;
        reading.until_row("line20").await;
        let rows = reading.rows.clone();
        let before = reading.last.clone().unwrap();
        assert_eq!(before.scrolled_back, 10);
        typing.type_line("").await;
        typing.until_row("more3").await;
        reading
            .until("the program's three rows more", |attached| {
                let frame = attached.last.as_ref().unwrap();
                frame.scrollback >= before.scrollback + 3
            })
            .await;
        let after = reading.last.clone().unwrap();
        let pushed = after.scrollback - before.scrollback;
        assert_eq!(after.scrolled_back, before.scrolled_back + pushed);
        assert_eq!(reading.rows, rows);
    });
}

/// The pane takes the size of the client that last came, resized or typed,
/// and its program is told.
#[test]
fn the_pane_takes_the_size_of_the_latest_client() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut mine = Attached::new(&mut mux, session(&made), size(40, 10)).await;
        mine.type_line("stty size").await;
        mine.until_row("10 40").await;
        mine.send(api::to_server::Event::Resize(size(50, 12))).await;
        mine.until("a frame of the new size", |attached| {
            attached.last.as_ref().and_then(|frame| frame.size) == Some(size(50, 12))
        })
        .await;
        mine.type_line("stty size").await;
        mine.until_row("12 50").await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        watched_until(&mut watch, "the client at its new size", |listing| {
            let Some(tab) = tabs(listing).first() else {
                return false;
            };
            let clients = &tab.panes[0].clients;
            clients.len() == 1
                && clients[0].id == mine.client
                && clients[0].size == Some(size(50, 12))
        })
        .await;
    });
}

/// A client that says it has no columns is refused, by the field's name.
#[test]
fn a_hello_of_no_columns_is_refused_by_its_field() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let hello =
            api::Hello { to: Some(session(&made)), size: Some(size(0, 10)), colours: None };
        let Err(refused) = Attached::hello(&mut mux, hello).await else {
            panic!("a client of no columns was taken");
        };
        assert_eq!(
            (refused.code(), refused.message()),
            (Code::InvalidArgument, "hello.size.columns is 0")
        );
    });
}
