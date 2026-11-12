//! What the server holds of a pane's program, which a program cannot grow
//! past its bounds, and what of it a program cannot break.

use std::time::Duration;

use luneta_mux::api;
use tokio::time::Instant;

use crate::host::{
    Attached, Server, ended, new_session, run, script, session, size, tabs, watched_until,
};

/// A program that asks its terminal what it is, over and over, and never
/// reads the answers, is held back: the server keeps no more of them than
/// it bounds, however long the program asks.
#[test]
fn answers_a_program_never_reads_are_held_to_a_bound() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let asks = "stty raw -echo; \
            yes \"$(printf '\\033[c')\" | tr -d '\\n' | head -c 30000000; read x";
        new_session(&mut mux, "asks", script(asks, api::Ends::Holds)).await;
        let before = server.resident_kib();
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let grown = server.resident_kib().saturating_sub(before);
            assert!(grown < 64 * 1024, "the server grew by {grown} KiB");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
}

/// A program that says it works in a folder whose name is not UTF-8 keeps
/// its tab, and goes on.
#[test]
fn a_folder_named_in_no_utf_8_breaks_no_pane() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let says = "printf '\\033]7;file://host/\\376\\007'; echo after; read x";
        let made = new_session(&mut mux, "odd", script(says, api::Ends::Holds)).await;
        let mut attached = Attached::new(&mut mux, session(made.session), size(20, 4)).await;
        attached.until_row("after").await;
        let listing =
            watched_until(&mut watch, "the tab", |listing| !tabs(listing).is_empty()).await;
        assert_eq!(ended(&tabs(&listing)[0].panes[0]), None);
        attached.type_line("").await;
        watched_until(&mut watch, "the program ended", |listing| {
            tabs(listing).first().is_some_and(|tab| ended(&tab.panes[0]).is_some())
        })
        .await;
    });
}
