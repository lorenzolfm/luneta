use lune_mux::api;
use lune_waits::{Span, QUIET};

use crate::host::{Server, Viewer, new_terminal, run, script, size, watched_until};

#[test]
fn answers_a_program_never_reads_are_held_to_a_bound() {
    run(async {
        let server = Server::start().await;
        let mut mux = server.client().await;
        let asks = "stty raw -echo; \
            yes \"$(printf '\\033[c')\" | tr -d '\\n' | head -c 30000000; read x";
        new_terminal(&mut mux, script(asks, api::Ends::Holds), b"asks", size(80, 24)).await;
        let before = server.resident_kib();
        let quiet = const { Span::of(QUIET.saturating_mul(4)) };
        lune_waits::through_async(quiet, "the server grows by 64 MiB or more, in KiB", || {
            Some(server.resident_kib().saturating_sub(before)).filter(|grown| *grown >= 64 * 1024)
        })
        .await;
    });
}

#[test]
fn a_folder_named_in_no_utf_8_breaks_no_terminal() {
    run(async {
        let server = Server::start().await;
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let says = "printf '\\033]7;file://host/\\376\\007'; echo after; read x";
        let key = new_terminal(&mut mux, script(says, api::Ends::Holds), b"odd", size(20, 4)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.until_row(key.id, "after").await;
        let ended = |listing: &api::Listing| {
            let program = listing.terminals.first().and_then(|terminal| terminal.program.as_ref());
            matches!(program, Some(api::terminal::Program::Ended(_)))
        };
        assert!(!ended(&mux.list(api::ListRequest {}).await.unwrap().into_inner()));
        viewer.type_line(key, "").await;
        watched_until(&mut watch, "the program ended", ended).await;
    });
}

#[test]
fn a_server_holds_more_files_than_it_started_with_and_its_programs_get_that_back() {
    run(async {
        let server = Server::start_with_files(64).await;
        let mut mux = server.client().await;
        let says = script("ulimit -Sn; read x", api::Ends::Holds);
        let key = new_terminal(&mut mux, says, b"many", size(20, 4)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.until_row(key.id, "64").await;
        for _ in 0..20 {
            new_terminal(&mut mux, script("read x", api::Ends::Holds), b"more", size(20, 4)).await;
        }
    });
}

#[test]
fn a_flood_of_answers_leaves_a_terminal_drawing_and_taking_ctrl_c() {
    run(async {
        let server = Server::start().await;
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let asks = "stty -icanon -echo; \
            yes \"$(printf '\\033[6n')\" | tr -d '\\n' | head -c 4000000; \
            echo; echo drawn; sleep 1000";
        let opens = script(asks, api::Ends::Holds);
        let key = new_terminal(&mut mux, opens, b"asks", size(80, 24)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.until_row(key.id, "drawn").await;
        viewer.press_ctrl(key, 'c').await;
        let interrupted = api::Exit { how: Some(api::exit::How::Signal(2)) };
        watched_until(&mut watch, "the program ended on SIGINT", |listing| {
            let program = listing.terminals.first().and_then(|terminal| terminal.program.as_ref());
            program == Some(&api::terminal::Program::Ended(interrupted))
        })
        .await;
    });
}
