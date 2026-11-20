use luneta_mux::api;
use luneta_waits::QUIET;

use crate::host::{Server, Viewer, new_terminal, run, script, size, watched_until};

#[test]
fn answers_a_program_never_reads_are_held_to_a_bound() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let asks = "stty raw -echo; \
            yes \"$(printf '\\033[c')\" | tr -d '\\n' | head -c 30000000; read x";
        new_terminal(&mut mux, script(asks, api::Ends::Holds), b"asks", size(80, 24)).await;
        let before = server.resident_kib();
        luneta_waits::through(QUIET * 4, "the server grows by 64 MiB or more, in KiB", || {
            Some(server.resident_kib().saturating_sub(before)).filter(|grown| *grown >= 64 * 1024)
        });
    });
}

#[test]
fn a_folder_named_in_no_utf_8_breaks_no_terminal() {
    run(async {
        let server = Server::start();
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
        let server = Server::start_with_files(64);
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
