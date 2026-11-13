//! Sessions, their tabs and the panes in them, as the server makes, lists and
//! ends them.

use luneta_mux::api;
use tonic::Code;

use crate::host::{
    Attached, Server, ended, gone, listed, new_session, new_tab, run, script, session, shell, size,
    tabs, watched_until,
};

/// A session's shell runs on the pane's terminal: what a client types there
/// reaches it as keys, and what it prints comes back in frames.
#[test]
fn a_session_runs_a_shell_that_a_client_types_in() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(40, 10)).await;
        assert_eq!(
            (attached.went.session, attached.went.tab, attached.went.pane),
            (made.session, made.tab, made.pane),
        );
        attached.type_line("echo $((6 * 7))").await;
        attached.until_row("42").await;
    });
}

/// A server started in a pane of zellij's gives its panes none of zellij's
/// variables, and names each pane by its own ids, and by when its session was
/// made, in nanoseconds since the epoch, as the server lists it.
#[test]
fn a_pane_is_named_by_its_own_ids_and_not_by_zellij() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let made = new_session(&mut mux, "env", script("export -p", api::Ends::Holds)).await;
        let listing = watched_until(&mut watch, "the program ended", |listing| {
            tabs(listing).first().is_some_and(|tab| ended(&tab.panes[0]).is_some())
        })
        .await;
        let dumped = api::DumpRequest { pane: made.pane, dumped: api::Dumped::Text.into() };
        let text = mux.dump(dumped).await.unwrap().into_inner().text;
        assert!(!text.contains("ZELLIJ"), "{text}");
        let named = |name: &str, id: String| {
            text.lines().any(|line| line.contains(name) && line.contains(&id))
        };
        assert!(named("LUNETA_MUX_SESSION=", made.session.to_string()), "{text}");
        assert!(named("LUNETA_MUX_PANE=", made.pane.to_string()), "{text}");
        let made_at = listing.sessions[0].made_at.unwrap();
        let nanos = i128::from(made_at.seconds) * 1_000_000_000 + i128::from(made_at.nanos);
        assert!(named("LUNETA_MUX_MADE=", nanos.to_string()), "{text}");
    });
}

/// The watch says each session, tab and pane as it comes and goes. An id is
/// never given again, and a rename changes the name alone.
#[test]
fn a_watch_follows_the_tabs_and_ids_are_never_given_again() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let made = new_session(&mut mux, "work", shell()).await;
        let named = api::NewTabRequest {
            session: made.session,
            name: Some("logs".to_owned()),
            opens: Some(shell()),
        };
        let logs = mux.new_tab(named).await.unwrap().into_inner();
        let listing =
            watched_until(&mut watch, "two tabs", |listing| tabs(listing).len() == 2).await;
        let tab_names: Vec<&str> = tabs(&listing).iter().map(|tab| tab.name.as_str()).collect();
        assert_eq!(tab_names, ["Tab #1", "logs"]);
        mux.close_tab(api::CloseTabRequest { tab: logs.tab }).await.unwrap();
        watched_until(&mut watch, "one tab", |listing| tabs(listing).len() == 1).await;
        let next = new_tab(&mut mux, made.session, shell()).await;
        assert!(next.tab > logs.tab && next.pane > logs.pane, "{next:?} after {logs:?}");
        let rename = api::RenameRequest { session: made.session, name: "play".to_owned() };
        mux.rename(rename).await.unwrap();
        let listing =
            watched_until(&mut watch, "the new name", |listing| listing.sessions[0].name == "play")
                .await;
        assert_eq!(listing.sessions[0].id, made.session);
        let tabs: Vec<(u32, &str)> =
            listing.sessions[0].tabs.iter().map(|tab| (tab.id, tab.name.as_str())).collect();
        assert_eq!(tabs, [(made.tab, "Tab #1"), (next.tab, "Tab #2")]);
    });
}

/// A tab closes as its program ends, where it was opened to, and holds the
/// pane, with how its program ended, where it was not.
#[test]
fn a_tab_closes_or_holds_as_its_program_ends() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let made = new_session(&mut mux, "work", shell()).await;
        let held = new_tab(&mut mux, made.session, script("exit 3", api::Ends::Holds)).await;
        let closed = new_tab(&mut mux, made.session, script("exit 4", api::Ends::Closes)).await;
        let listing =
            watched_until(&mut watch, "the held pane ended, the other tab gone", |listing| {
                let tabs = tabs(listing);
                !tabs.is_empty()
                    && tabs.iter().all(|tab| tab.id != closed.tab)
                    && tabs.iter().any(|tab| tab.id == held.tab && ended(&tab.panes[0]).is_some())
            })
            .await;
        let held = &tabs(&listing).iter().find(|tab| tab.id == held.tab).unwrap().panes[0];
        assert_eq!(
            held.program,
            Some(api::pane::Program::Ended(api::Exit { how: Some(api::exit::How::Code(3)) })),
        );
        let runs = &tabs(&listing)[0].panes[0];
        assert!(matches!(runs.program, Some(api::pane::Program::Pid(pid)) if pid > 0), "{runs:?}");
    });
}

/// A pane works where it opened, until its program says it works in another
/// folder of this machine's: a folder of another machine's is not where it
/// works.
#[test]
fn a_pane_works_where_it_says_on_this_machine() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let says = "printf '\\033]7;file://elsewhere/tmp\\007'; echo one; read x; \
            printf '\\033]7;file:///tmp\\007'; echo two; read x";
        let made = new_session(&mut mux, "cwd", script(says, api::Ends::Holds)).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(20, 4)).await;
        attached.until_row("one").await;
        assert_eq!(tabs(&listed(&mut mux).await)[0].panes[0].cwd, "/");
        attached.type_line("").await;
        attached.until_row("two").await;
        assert_eq!(tabs(&listed(&mut mux).await)[0].panes[0].cwd, "/tmp");
    });
}

/// No two sessions have a name, as the server makes them or renames them.
#[test]
fn a_name_is_one_sessions() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let work = new_session(&mut mux, "work", shell()).await;
        new_session(&mut mux, "play", shell()).await;
        let again = api::NewSessionRequest { name: "work".to_owned(), opens: Some(shell()) };
        let refused = mux.new_session(again).await.unwrap_err();
        assert_eq!(
            (refused.code(), refused.message()),
            (Code::AlreadyExists, "a session is named work")
        );
        let rename = api::RenameRequest { session: work.session, name: "play".to_owned() };
        assert_eq!(mux.rename(rename).await.unwrap_err().code(), Code::AlreadyExists);
    });
}

/// Killing the last session tells its client it ended, ends the watch, and
/// ends the server, which takes its socket away.
#[test]
fn the_server_ends_with_its_last_session() {
    run(async {
        let mut server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let made = new_session(&mut mux, "work", shell()).await;
        let mut attached = Attached::new(&mut mux, session(&made), size(40, 10)).await;
        mux.kill(api::KillRequest { session: made.session }).await.unwrap();
        loop {
            match attached.next().await {
                Some(api::to_client::Event::Ended(_)) => break,
                Some(_) => {},
                None => panic!("the stream ended before it said the session ended"),
            }
        }
        assert!(attached.next().await.is_none(), "the stream ends after the session");
        while let Ok(Some(_)) = watch.message().await {}
        drop((attached, watch, mux));
        assert!(server.ended().await.success());
        assert!(gone(&server.socket()));
    });
}
