use std::time::Duration;

use lunetad::api::{kill_request, KillRequest, NewOwnRequest};

use crate::host::eventually;
use crate::own::{own, Owned};
use crate::sessions::session;
use crate::starts::{clients, directory, luneta, mouse, on_terminal, runs, Window};

const TYPED: &str = "printf 'own-%s\\n' 42\r";

const PRINTED: &str = "own-42";

#[test]
fn luneta_attaches_to_its_session_and_shows_what_its_shell_prints() {
    let owned = Owned::new();
    let request = NewOwnRequest {
        on: None,
        name: "work".to_owned(),
        cwd: owned.home(),
        command: vec!["sh".to_owned()],
        tab: None,
    };
    owned.ask(|mut daemon| async move { daemon.new_own(request).await }).unwrap();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta is the session's client", || (clients(&owned, "work")? == 1).then_some(()));
    assert!(own(&owned.listed("work")).is_some());
    eventually("the sidebar lists it", || {
        let screen = window.screen();
        screen.iter().any(|line| line.contains(" work ") && line.contains('◇')).then_some(())
    });

    window.send(TYPED);
    let (row, column) = window.spot_of(PRINTED);
    let end = (row, column + PRINTED.len() - 1);
    let swept = [mouse(0, (row, column), 'M'), mouse(32, end, 'M'), mouse(0, end, 'm')];
    window.send(&swept.concat());
    eventually("the drag copies what it went over", || {
        window.copied().contains(&PRINTED.to_owned()).then_some(())
    });
}

#[test]
fn luneta_refuses_a_name_a_session_of_zellij_s_has() {
    let owned = Owned::new();
    let _project = runs(&owned.host, "project");
    let dir = directory("anywhere");
    let mut command = luneta(owned.host.vars(), &dir.path);
    command.arg("project");
    let ended = on_terminal(command);
    assert!(!ended.status.success(), "{}", ended.said);
    assert!(ended.said.contains("project is a session of zellij's"), "{}", ended.said);
    let host = owned.host.host();
    let named = host.sessions.iter().filter(|session| session.name == "project");
    assert!(named.clone().all(|session| own(session).is_none()), "{:?}", host.sessions);
    assert_eq!(clients(&owned, "project").unwrap_or(0), 0);
}

fn both_join(owned: &Owned, name: &str, mut windows: [Window; 2]) {
    eventually(&format!("both lunetas are {name}'s clients"), || {
        for window in &mut windows {
            let ended = window.luneta.ended_within(Duration::ZERO);
            assert!(ended.is_none(), "a luneta ended, {ended:?}: {:?}", window.screen());
        }
        (clients(owned, name)? == 2).then_some(())
    });
    let host = owned.host.host();
    let made = host.sessions.iter().filter(|session| session.name == name);
    assert_eq!(made.count(), 1, "{:?}", host.sessions);
}

#[test]
fn bare_lunetas_started_at_once_join_the_session_one_of_them_made() {
    let owned = Owned::new();
    let dir = directory("project");
    let opening = || Window::opening(&owned.host, &dir, &[]);
    both_join(&owned, "project", [opening(), opening()]);
}

#[test]
fn named_lunetas_started_at_once_join_the_session_one_of_them_made() {
    let owned = Owned::new();
    let dir = directory("anywhere");
    let opening = || Window::opening(&owned.host, &dir, &["work"]);
    both_join(&owned, "work", [opening(), opening()]);
}

#[test]
fn the_newest_session_takes_the_place_of_an_own_one_that_ended() {
    let owned = Owned::new();
    let _zellij = runs(&owned.host, "older");
    owned.make("first").unwrap();
    let dir = directory("anywhere");
    let mut window = Window::open(&owned.host, &dir, &["first"]);
    eventually("luneta is in first", || (clients(&owned, "first")? == 1).then_some(()));
    owned.make("newer").unwrap();
    let first = own(&owned.listed("first")).unwrap();
    let request = KillRequest {
        on: None,
        session: "first".to_owned(),
        whose: Some(kill_request::Whose::Own(first)),
    };
    owned.ask(|mut daemon| async move { daemon.kill(request).await }).unwrap();
    eventually("luneta goes on in newer", || (clients(&owned, "newer")? == 1).then_some(()));
    assert!(owned.host.zellij.attached().is_empty(), "{:?}", owned.host.zellij.attached());
    assert!(window.luneta.ended_within(Duration::ZERO).is_none(), "luneta goes on");
}

#[test]
fn a_delete_of_the_own_session_luneta_is_in_goes_on_in_the_newest_and_kills_it() {
    let owned = Owned::new();
    owned.make("first").unwrap();
    let dir = directory("anywhere");
    let mut window = Window::open(&owned.host, &dir, &["first"]);
    eventually("luneta is in first", || (clients(&owned, "first")? == 1).then_some(()));
    owned.make("newer").unwrap();
    eventually("the sidebar lists newer", || window.shows("newer").then_some(()));
    window.send("\x1b[97;5uds");
    eventually("luneta goes on in newer", || (clients(&owned, "newer")? == 1).then_some(()));
    eventually("first is gone", || session(&owned.host.host(), "first").is_none().then_some(()));
    assert!(window.luneta.ended_within(Duration::ZERO).is_none(), "luneta goes on");
}
