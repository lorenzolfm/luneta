use std::time::Duration;

use lunetad::api::terminal::Found;

use crate::host::eventually;
use crate::own::Owned;
use crate::starts::{directory, mouse, projects_of, shown, Window};

const TYPED: &str = "printf 'own-%s\\n' 42\r";

const PRINTED: &str = "own-42";

#[test]
fn luneta_shows_its_workstream_and_what_its_shell_prints() {
    let owned = Owned::new();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
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
fn the_menu_opens_a_pane_beside_the_one_luneta_shows_and_closes_it() {
    let owned = Owned::new();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    window.send("\x1b[97;3unp");
    let split = eventually("a second terminal opens in the view", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals.iter().filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2).then_some(terminals)
    });
    assert_eq!(split.len(), 2);
    eventually("both frames are drawn beside each other", || {
        let tops = window.screen().iter().map(|line| line.matches('╭').count()).max()?;
        (tops == 2).then_some(())
    });
    window.send("\x1b[97;3udd");
    eventually("the pane luneta is on closes", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        (terminals.len() == 1).then_some(())
    });
}

fn both_show(owned: &Owned, name: &str, mut windows: [Window; 2]) {
    eventually(&format!("both lunetas show {name}"), || {
        for window in &mut windows {
            let ended = window.luneta.ended_within(Duration::ZERO);
            assert!(ended.is_none(), "a luneta ended, {ended:?}: {:?}", window.screen());
        }
        windows.iter().all(|window| window.shows(name)).then_some(())
    });
    let projects = projects_of(owned);
    let made = projects.iter().filter(|project| project.name == name);
    assert_eq!(made.count(), 1, "{projects:?}");
}

#[test]
fn bare_lunetas_started_at_once_show_the_project_one_of_them_made() {
    let owned = Owned::new();
    let dir = directory("project");
    let opening = || Window::opening(&owned.host, &dir, &[]);
    both_show(&owned, "project", [opening(), opening()]);
}

#[test]
fn named_lunetas_started_at_once_show_the_project_one_of_them_made() {
    let owned = Owned::new();
    let dir = directory("anywhere");
    let opening = || Window::opening(&owned.host, &dir, &["work"]);
    both_show(&owned, "work", [opening(), opening()]);
}
