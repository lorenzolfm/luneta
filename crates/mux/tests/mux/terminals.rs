use luneta_mux::api;
use tonic::Code;

use crate::host::{
    Server, Viewer, gone, listed, new_session, new_terminal, run, script, shell, size,
    watched_until,
};

fn terminal<'a>(listing: &'a api::Listing, key: &api::TerminalKey) -> Option<&'a api::Terminal> {
    listing.terminals.iter().find(|terminal| terminal.key.as_ref() == Some(key))
}

fn made_at_another_time(key: api::TerminalKey) -> api::TerminalKey {
    let mut made_at = key.made_at.unwrap();
    made_at.nanos = if made_at.nanos > 0 { made_at.nanos - 1 } else { 1 };
    api::TerminalKey { made_at: Some(made_at), ..key }
}

fn nanos(key: api::TerminalKey) -> i128 {
    let made_at = key.made_at.unwrap();
    i128::from(made_at.seconds) * 1_000_000_000 + i128::from(made_at.nanos)
}

#[test]
fn a_terminal_is_listed_with_its_tag_and_size_and_a_viewer_types_in_it() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let key = new_terminal(&mut mux, shell(), b"work 1", size(40, 10)).await;
        let listing = listed(&mut mux).await;
        assert_eq!(listing.sessions, Vec::<api::Session>::new());
        let listed = terminal(&listing, &key).expect("the terminal is listed");
        assert_eq!(listing.terminals.len(), 1);
        assert_eq!(listed.tag, b"work 1");
        assert_eq!(listed.size, Some(size(40, 10)));
        assert!(matches!(listed.program, Some(api::terminal::Program::Pid(_))));
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.type_line(key.id, "echo $((6 * 7))").await;
        viewer.until_row(key.id, "42").await;
    });
}

#[test]
fn a_terminal_takes_the_size_it_is_given_and_not_its_viewer_s() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let key = new_terminal(&mut mux, shell(), b"sized", size(40, 10)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.type_line(key.id, "stty size").await;
        viewer.until_row(key.id, "10 40").await;
        let sized = api::SizeTerminalRequest { terminal: Some(key), size: Some(size(50, 12)) };
        mux.size_terminal(sized).await.unwrap();
        viewer
            .until("a frame of 50 by 12", |viewer| {
                viewer.frame_of(key.id).is_some_and(|frame| frame.size == Some(size(50, 12)))
            })
            .await;
        viewer.type_line(key.id, "stty size").await;
        viewer.until_row(key.id, "12 50").await;
        let listing = listed(&mut mux).await;
        assert_eq!(terminal(&listing, &key).unwrap().size, Some(size(50, 12)));
    });
}

#[test]
fn a_terminal_s_tag_changes_and_the_watch_says_so() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let key = new_terminal(&mut mux, shell(), b"before", size(40, 10)).await;
        let tagged = api::TagTerminalRequest { terminal: Some(key), tag: b"after".to_vec() };
        mux.tag_terminal(tagged).await.unwrap();
        watched_until(&mut watch, "the new tag", |listing| {
            terminal(listing, &key).is_some_and(|terminal| terminal.tag == b"after")
        })
        .await;
    });
}

#[test]
fn a_viewer_sees_the_terminals_it_shows_and_no_others() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let one = script("echo one; read x", api::Ends::Holds);
        let one = new_terminal(&mut mux, one, b"1", size(20, 4)).await;
        let two = script("echo two; read x", api::Ends::Holds);
        let two = new_terminal(&mut mux, two, b"2", size(20, 4)).await;
        let mut both = Viewer::new(&mut mux).await;
        both.show(&[one, two]).await;
        both.until_row(one.id, "one").await;
        both.until_row(two.id, "two").await;
        let mut alone = Viewer::new(&mut mux).await;
        alone.show(&[two]).await;
        alone.until_row(two.id, "two").await;
        assert_eq!(alone.rows_of(one.id), [] as [String; 0]);
    });
}

#[test]
fn a_viewer_that_shows_a_terminal_again_gets_its_whole_screen_again() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let one = script("echo one; read x", api::Ends::Holds);
        let key = new_terminal(&mut mux, one, b"1", size(20, 4)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.until_row(key.id, "one").await;
        viewer.show(&[]).await;
        viewer.show(&[key]).await;
        loop {
            let event = viewer.next().await.unwrap().expect("the stream goes on");
            if let api::to_viewer::Event::Frame(frame) = event
                && frame.pane == key.id
                && matches!(frame.rows, Some(api::frame::Rows::Whole(_)))
            {
                break;
            }
        }
    });
}

#[test]
fn typing_in_a_terminal_not_shown_is_refused_and_the_viewer_goes_on() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let key = new_terminal(&mut mux, shell(), b"t", size(40, 10)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.type_line(key.id, "").await;
        viewer.until("a refusal", |viewer| !viewer.refused.is_empty()).await;
        assert_eq!(viewer.refused[0], format!("terminal {} is not shown", key.id));
        viewer.show(&[key]).await;
        viewer.type_line(key.id, "echo $((6 * 7))").await;
        viewer.until_row(key.id, "42").await;
    });
}

#[test]
fn a_viewer_never_shows_the_terminal_it_runs_in() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let home = new_terminal(&mut mux, shell(), b"home", size(40, 10)).await;
        let greeting = api::Greeting { colours: None, runs_in: Some(home) };
        let mut viewer = Viewer::hello(&mut mux, greeting).await.unwrap();
        viewer.show(&[home]).await;
        viewer.until("a refusal", |viewer| !viewer.refused.is_empty()).await;
        let refused = format!("the viewer runs in terminal {}, and never shows itself", home.id);
        assert_eq!(viewer.refused, [refused]);
    });
}

#[test]
fn a_terminal_named_with_another_time_is_not_found() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let key = new_terminal(&mut mux, shell(), b"t", size(40, 10)).await;
        let other = Some(made_at_another_time(key));
        let not_found = format!("no terminal {} made then", key.id);
        let closed = mux.close_terminal(api::CloseTerminalRequest { terminal: other }).await;
        let sized = api::SizeTerminalRequest { terminal: other, size: Some(size(20, 4)) };
        let sized = mux.size_terminal(sized).await;
        let tagged = api::TagTerminalRequest { terminal: other, tag: b"u".to_vec() };
        let tagged = mux.tag_terminal(tagged).await;
        let dumped =
            api::DumpRequest { pane: 0, dumped: api::Dumped::Text.into(), terminal: other };
        let dumped = mux.dump(dumped).await;
        let refusals =
            [closed.unwrap_err(), sized.unwrap_err(), tagged.unwrap_err(), dumped.unwrap_err()];
        for refusal in refusals {
            assert_eq!((refusal.code(), refusal.message()), (Code::NotFound, not_found.as_str()));
        }
        assert!(terminal(&listed(&mut mux).await, &key).is_some());
    });
}

#[test]
fn a_terminal_s_program_sees_the_terminal_s_ids_and_no_session_s() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let says = "echo \"$LUNETA_MUX_TERMINAL ${LUNETA_MUX_SESSION-none}\"; \
            echo \"$LUNETA_MUX_TERMINAL_MADE\"; read x";
        let key = new_terminal(&mut mux, script(says, api::Ends::Holds), b"t", size(40, 4)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        viewer.until_row(key.id, &format!("{} none", key.id)).await;
        viewer.until_row(key.id, &nanos(key).to_string()).await;
    });
}

#[test]
fn a_terminal_dumps_its_text() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let key =
            new_terminal(&mut mux, script("echo said", api::Ends::Holds), b"t", size(20, 4)).await;
        watched_until(&mut watch, "the program ended", |listing| {
            terminal(listing, &key).is_some_and(|terminal| {
                matches!(terminal.program, Some(api::terminal::Program::Ended(_)))
            })
        })
        .await;
        let dumped =
            api::DumpRequest { pane: 0, dumped: api::Dumped::Text.into(), terminal: Some(key) };
        let text = mux.dump(dumped).await.unwrap().into_inner().text;
        assert!(text.lines().any(|line| line == "said"), "{text}");
        let both = api::DumpRequest {
            pane: key.id,
            dumped: api::Dumped::Text.into(),
            terminal: Some(key),
        };
        let refusal = mux.dump(both).await.unwrap_err();
        assert_eq!(refusal.message(), "pane and terminal are both set");
    });
}

#[test]
fn a_held_terminal_says_how_its_program_ended_and_a_closing_one_leaves() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let mut watch = mux.watch(api::WatchRequest {}).await.unwrap().into_inner();
        let held =
            new_terminal(&mut mux, script("exit 3", api::Ends::Holds), b"held", size(20, 4)).await;
        let closes = script("read x", api::Ends::Closes);
        let closes = new_terminal(&mut mux, closes, b"closes", size(20, 4)).await;
        let ended =
            Some(api::terminal::Program::Ended(api::Exit { how: Some(api::exit::How::Code(3)) }));
        watched_until(&mut watch, "the held program ended with 3", |listing| {
            terminal(listing, &held).is_some_and(|terminal| terminal.program == ended)
        })
        .await;
        let viewer = Viewer::new(&mut mux).await;
        viewer.show(&[closes]).await;
        viewer.type_line(closes.id, "").await;
        let listing = watched_until(&mut watch, "the closing terminal left", |listing| {
            terminal(listing, &closes).is_none()
        })
        .await;
        assert!(terminal(&listing, &held).is_some());
    });
}

#[test]
fn a_terminal_keeps_the_server_after_its_last_session_and_the_last_terminal_ends_it() {
    run(async {
        let mut server = Server::start();
        let mut mux = server.client().await;
        let made = new_session(&mut mux, "work", shell()).await;
        let key = new_terminal(&mut mux, shell(), b"t", size(40, 10)).await;
        mux.kill(api::KillRequest { session: made.session }).await.unwrap();
        let listing = listed(&mut mux).await;
        assert_eq!(listing.sessions, Vec::<api::Session>::new());
        assert!(terminal(&listing, &key).is_some());
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key]).await;
        mux.close_terminal(api::CloseTerminalRequest { terminal: Some(key) }).await.unwrap();
        while viewer.next().await.unwrap().is_some() {}
        drop((viewer, mux));
        assert!(server.ended().await.success());
        assert!(gone(&server.socket()));
    });
}

#[test]
fn a_tag_is_said_and_bounded() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let made = |tag: Vec<u8>| api::NewTerminalRequest {
            opens: Some(shell()),
            tag,
            size: Some(size(40, 10)),
        };
        let empty = mux.new_terminal(made(Vec::new())).await.unwrap_err();
        assert_eq!((empty.code(), empty.message()), (Code::InvalidArgument, "tag is empty"));
        let long = mux.new_terminal(made(vec![b'x'; 257])).await.unwrap_err();
        assert_eq!(long.message(), "tag is over 256 bytes");
        mux.new_terminal(made(vec![0xff; 256])).await.unwrap();
    });
}

#[test]
fn a_showing_that_repeats_a_terminal_ends_the_viewer() {
    run(async {
        let server = Server::start();
        let mut mux = server.client().await;
        let key = new_terminal(&mut mux, shell(), b"t", size(40, 10)).await;
        let mut viewer = Viewer::new(&mut mux).await;
        viewer.show(&[key, key]).await;
        let refusal = loop {
            match viewer.next().await {
                Ok(Some(_)) => {},
                Ok(None) => panic!("the stream ended without a refusal"),
                Err(refusal) => break refusal,
            }
        };
        let repeated = format!("showing.terminals repeat {}", key.id);
        assert_eq!((refusal.code(), refusal.message()), (Code::InvalidArgument, repeated.as_str()));
    });
}
