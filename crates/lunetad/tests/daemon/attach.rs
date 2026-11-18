use std::fs;
use std::thread;
use std::time::Duration;

use lunetad::api::terminal::Found;

use crate::host::{eventually, LUNETA};
use crate::own::Owned;
use crate::projects::workstream;
use crate::starts::{directory, mouse, projects_of, shown, Window};

const TYPED: &str = "printf 'own-%s\\n' 42\r";

const PRINTED: &str = "own-42";

const HERE: &str = "luneta runs here";

const LEFT: &str = "\x1b[104;5u";

const RIGHT: &str = "\x1b[108;5u";

const GROW: &str = "\x1b[61;3u";

const SHRINK: &str = "\x1b[45;3u";

const SPLIT: &str = "\x1b[97;3unp";

fn seam(window: &Window) -> Option<usize> {
    window.screen().iter().find_map(|line| {
        let (at, _) = line.char_indices().filter(|(_, c)| *c == '╭').nth(1)?;
        Some(line[..at].chars().count())
    })
}

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
    window.send(SPLIT);
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

#[test]
fn ctrl_h_and_ctrl_l_move_between_panes_and_alt_equals_and_minus_resize_the_focused_one() {
    let owned = Owned::new();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    window.send(SPLIT);
    let split = eventually("two frames are drawn beside each other", || seam(&window));
    let side = |window: &Window| if window.cursor().1 >= split { "right" } else { "left" };
    for (key, to) in [(LEFT, "left"), (RIGHT, "right"), (LEFT, "left")] {
        window.send(key);
        eventually(&format!("the cursor is in the {to} pane"), || {
            (side(&window) == to).then_some(())
        });
    }
    window.send(GROW);
    let grown = eventually("the left pane grows", || seam(&window).filter(|at| *at > split));
    window.send(SHRINK);
    eventually("the left pane shrinks", || seam(&window).filter(|at| *at < grown));
    assert_eq!(side(&window), "left", "{:?}", window.screen());
}

#[test]
fn ctrl_h_and_ctrl_l_at_the_edge_go_to_the_workstream_beside_but_not_past_the_project() {
    let owned = Owned::new();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let first = eventually("luneta shows work", || shown(&owned, &dir.path)).id;
    let project = projects_of(&owned).into_iter().find(|it| it.name == "work").unwrap().id;
    let docs = dir.path.join("docs");
    fs::create_dir(&docs).unwrap();
    let second = workstream(&owned.host, project, &docs);
    eventually("the tab bar lists docs", || window.shows("docs").then_some(()));
    let used = |id: u64| {
        let projects = projects_of(&owned);
        let found = projects.iter().flat_map(|it| &it.workstreams).find(|it| it.id == id);
        found.and_then(|it| it.used_at).map(|at| (at.seconds, at.nanos))
    };
    window.send(LEFT);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(shown(&owned, &docs), None, "Ctrl h stops at the project's first workstream");
    window.send(RIGHT);
    eventually("Ctrl l goes to docs, where a shell opens", || shown(&owned, &docs));
    assert!(used(second) > used(first), "{:?}", projects_of(&owned));
    window.send(LEFT);
    eventually("Ctrl h goes back", || (used(first) > used(second)).then_some(()));
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
    let made: Vec<_> = projects.iter().filter(|project| project.name == name).collect();
    assert_eq!(made.len(), 1, "{projects:?}");
    assert_eq!(made[0].workstreams.len(), 1, "{projects:?}");
    let terminals = || {
        let projects = projects_of(owned);
        let made = projects.into_iter().find(|project| project.name == name)?;
        Some(made.workstreams.iter().map(|it| it.terminals.len()).sum::<usize>())
    };
    eventually("a shell opens in the empty view", || terminals().filter(|count| *count > 0));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(terminals(), Some(1), "{:?}", projects_of(owned));
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

#[test]
fn a_luneta_started_in_a_shell_of_the_view_it_shows_leaves_the_view_its_size() {
    let owned = Owned::new();
    let dir = directory("work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    let size = || owned.listing().terminals.first()?.size.map(|it| (it.columns, it.rows));
    let first = eventually("the shell is sized", size);
    window.send(&format!("{LUNETA}\r"));
    eventually("the inner luneta says it runs there", || window.shows(HERE).then_some(()));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(size(), Some(first), "{:?}", window.screen());
}
