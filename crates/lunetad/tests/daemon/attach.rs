use std::path::Path;
use std::thread;
use std::time::Duration;

use lunetad::api::terminal::Found;

use crate::host::{eventually, LUNETA};
use crate::own::Owned;
use crate::projects::worktree;
use crate::starts::{directory, mouse, projects_of, shown, work_in, Window, SPLIT, UNSHOWN};
use crate::worktree::worktree_added;

const TYPED: &str = "printf 'own-%s\\n' 42\r";

const PRINTED: &str = "own-42";

const HERE: &str = "luneta runs here";

const LEFT: &str = "\x1b[104;5u";

const RIGHT: &str = "\x1b[108;5u";

const GROW: &str = "\x1b[61;3u";

const SHRINK: &str = "\x1b[45;3u";

const SIDEBAR: &str = "\x1b[97;5u";

const MENU: &str = "\x1b[97;3u";

const WELCOMES: &str = "d d  close it";


const CLOSE: &str = "\x1b[97;3udd";

const WELCOME: &str = "\x1b[97;3unh";

fn seam(window: &Window) -> Option<usize> {
    window.screen().iter().find_map(|line| {
        let (at, _) = line.char_indices().filter(|(_, c)| *c == '╭').nth(1)?;
        Some(line[..at].chars().count())
    })
}

fn tops(window: &Window) -> Vec<(usize, usize)> {
    let screen = window.screen();
    let edge = screen.first().and_then(|line| line.chars().position(|it| it == '│'));
    let edge = edge.expect("the sidebar's edge is drawn");
    let tops = screen.iter().enumerate().flat_map(|(row, line)| {
        let right = line.chars().enumerate().skip(edge);
        right.filter(|(_, it)| *it == '╭').map(move |(column, _)| (row, column))
    });
    tops.collect()
}

#[test]
fn luneta_shows_its_worktree_and_what_its_shell_prints() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || {
        let screen = window.screen();
        screen.iter().any(|line| line.contains(" work ")).then_some(())
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
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    window.send(SPLIT);
    let split = eventually("a second terminal opens in the layout", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals.iter().filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2).then_some(terminals)
    });
    assert_eq!(split.len(), 2);
    eventually("both frames are drawn beside each other", || {
        let tops = window.screen().iter().map(|line| line.matches('╭').count()).max()?;
        (tops == 2).then_some(())
    });
    window.send(CLOSE);
    let id = eventually("the pane luneta is on closes", || {
        let worktree = shown(&owned, &dir.path)?;
        (worktree.terminals.len() == 1).then_some(worktree.id)
    });
    window.send(CLOSE);
    eventually("the worktree goes with its last pane, and luneta leaves it", || {
        let projects = projects_of(&owned);
        let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
        let gone = worktrees.all(|it| it.id != id);
        (gone && projects.len() == 1 && window.shows(UNSHOWN)).then_some(())
    });
    assert!(!window.shows("lunetad's layout"), "luneta leaves it with no note");
}

#[test]
fn the_menu_opens_a_welcome_view_beside_the_terminal_and_d_d_in_it_closes_it() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    let frames = |window: &Window| window.screen().iter().map(|it| it.matches('╭').count()).max();
    window.send(WELCOME);
    eventually("a welcome view is drawn beside the terminal", || {
        (frames(&window) == Some(2) && window.shows("runs no terminal")).then_some(())
    });
    let ids = || -> Vec<u64> {
        shown(&owned, &dir.path).expect("the worktree").terminals.iter().map(|it| it.id).collect()
    };
    let terminals = ids();
    assert_eq!(terminals.len(), 1, "a welcome view runs no terminal");
    window.send("dd");
    eventually("the welcome view closes and the terminal stays", || {
        (frames(&window) == Some(1) && !window.shows("runs no terminal")).then_some(())
    });
    assert_eq!(ids(), terminals);
}

#[test]
fn a_view_opened_from_the_sidebar_or_its_menu_takes_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    let mut before = tops(&window);
    for (keys, count) in [(format!("{SIDEBAR}nv"), 2), (format!("{SIDEBAR}{MENU}nv"), 3)] {
        window.send(&keys);
        let drawn = eventually(&format!("{count} terminals are drawn"), || {
            let terminals = shown(&owned, &dir.path)?.terminals;
            let found = terminals.iter().filter(|it| matches!(it.found, Some(Found::Key(_))));
            let drawn = tops(&window);
            (found.count() == count && drawn.len() == count).then_some(drawn)
        });
        window.send(&format!("printf 'own-%s\\n' {count}\r"));
        let (row, column) = window.spot_of(&format!("own-{count}"));
        let printer = drawn.iter().filter(|top| top.0 < row && top.1 < column).max();
        let printer = printer.expect("a drawn pane holds what was printed");
        let new = !before.contains(printer);
        assert!(new, "the new pane printed it at {row},{column}: {drawn:?} after {before:?}");
        before = drawn;
    }
    window.send(&format!("{SIDEBAR}nh"));
    eventually("a welcome view is drawn", || {
        (tops(&window).len() == 4 && window.shows(WELCOMES)).then_some(())
    });
    window.send("dd");
    eventually("d d in the welcome view closes it", || {
        (tops(&window).len() == 3 && !window.shows(WELCOMES)).then_some(())
    });
}

#[test]
fn a_worktree_opened_or_made_while_luneta_shows_none_takes_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    let root = projects_of(&owned).into_iter().find(|it| it.name == "work").unwrap().dir;
    let other = directory("other");
    let typed_in = |window: &mut Window, folder: &Path, typed: u32| {
        eventually(&format!("a shell opens in {}", folder.display()), || {
            let terminals = shown(&owned, folder)?.terminals;
            let found = terminals.iter().filter(|it| matches!(it.found, Some(Found::Key(_))));
            (found.count() == 1 && tops(window).len() == 1).then_some(())
        });
        window.send(&format!("printf 'own-%s\\n' {typed}\r"));
        window.spot_of(&format!("own-{typed}"));
    };
    let unshow = |window: &mut Window| {
        window.send(CLOSE);
        eventually("luneta shows no worktree", || window.shows(UNSHOWN).then_some(()));
    };
    window.send(TYPED);
    window.spot_of(PRINTED);
    unshow(&mut window);
    window.send("ow");
    eventually("o w lists main", || window.shows("not open").then_some(()));
    window.send("\r");
    typed_in(&mut window, &dir.path, 1);
    unshow(&mut window);
    let erased = "\x7f".repeat(root.chars().count());
    window.send(&format!("np{erased}{}\r\r", other.path.display()));
    typed_in(&mut window, &other.path, 2);
}

#[test]
fn a_menu_opened_while_a_project_is_made_keeps_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || window.shows(" work ").then_some(()));
    let other = directory("other");
    let erased = "\x7f".repeat(dir.path.display().to_string().chars().count());
    window.send(&format!("{SIDEBAR}np{erased}{}\r\r{SIDEBAR}n", other.path.display()));
    eventually(&format!("luneta shows {}", other.path.display()), || {
        let terminals = shown(&owned, &other.path)?.terminals;
        let found = terminals.iter().filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 1 && tops(&window).len() == 1).then_some(())
    });
    eventually("the menu opened meanwhile stays open", || window.shows("─ New ─").then_some(()));
}

#[test]
fn ctrl_h_and_ctrl_l_move_between_panes_and_alt_equals_and_minus_resize_the_focused_one() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
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
fn ctrl_h_and_ctrl_l_at_the_edge_go_to_the_worktree_beside_but_not_past_the_project() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let first = eventually("luneta shows work", || shown(&owned, &dir.path)).id;
    let project = projects_of(&owned).into_iter().find(|it| it.name == "work").unwrap().id;
    let docs = dir.path.parent().unwrap().join("docs");
    worktree_added(dir.path.parent().unwrap(), "docs");
    let second = worktree(&owned.host, project, &docs);
    eventually("the tab bar lists docs", || window.shows("docs").then_some(()));
    let used = |id: u64| used(&owned, id);
    window.send(LEFT);
    thread::sleep(Duration::from_secs(1));
    assert_eq!(used(second), None, "Ctrl h stops at the project's first worktree");
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || (used(second) > used(first)).then_some(()));
    window.send(LEFT);
    eventually("Ctrl h goes back", || (used(first) > used(second)).then_some(()));
}

#[test]
fn a_worktree_s_last_pane_closed_takes_luneta_to_the_one_used_before_with_no_note() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    window.send(TYPED);
    eventually("main's shell prints", || window.shows(PRINTED).then_some(()));
    let project = projects_of(&owned).into_iter().find(|it| it.name == "work").unwrap().id;
    let root = dir.path.parent().unwrap();
    worktree_added(root, "docs");
    let docs = root.join("docs");
    let gone = worktree(&owned.host, project, &docs);
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || used(&owned, gone).is_some().then_some(()));
    window.send(CLOSE);
    eventually("docs goes with its last pane, and luneta goes back to main", || {
        let projects = projects_of(&owned);
        let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
        (worktrees.all(|it| it.id != gone) && window.shows(PRINTED)).then_some(())
    });
    thread::sleep(Duration::from_secs(1));
    assert!(window.shows(PRINTED), "luneta stays on main: {:?}", window.screen());
    assert!(!window.shows("lunetad's layout"), "luneta leaves docs with no note");
}

fn used(owned: &Owned, worktree: u64) -> Option<(i64, i32)> {
    let projects = projects_of(owned);
    let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
    let found = worktrees.find(|it| it.id == worktree);
    found.and_then(|it| it.used_at).map(|at| (at.seconds, at.nanos))
}

fn both_show(owned: &Owned, name: &str, mut windows: [Window; 2]) {
    eventually(&format!("both lunetas show {name}"), || {
        for window in &mut windows {
            let ended = window.luneta.ended_within(Duration::ZERO);
            assert!(ended.is_none(), "a luneta ended, {ended:?}: {:?}", window.screen());
        }
        let shows = |window: &Window| window.shows(name);
        windows.iter().all(shows).then_some(())
    });
    let projects = projects_of(owned);
    let made: Vec<_> = projects.iter().filter(|project| project.name == name).collect();
    assert_eq!(made.len(), 1, "{projects:?}");
    assert_eq!(made[0].worktrees().unwrap().len(), 1, "{projects:?}");
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
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("luneta shows work", || shown(&owned, &dir.path));
    let size = || owned.listing().terminals.first()?.size.map(|it| (it.columns, it.rows));
    let first = eventually("the shell is sized", size);
    window.send(&format!("{LUNETA}\r"));
    eventually("the inner luneta says it runs there", || window.shows(HERE).then_some(()));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(size(), Some(first), "{:?}", window.screen());
}
