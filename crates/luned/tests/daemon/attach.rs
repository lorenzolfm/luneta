use std::fs;
use std::path::Path;

use lune_waits::{QUIET, Span, eventually};
use luned::api::Arrangement;
use luned::api::terminal::Found;
use serde_json::json;

use crate::host::{LUNE, script};
use crate::own::Owned;
use crate::projects::{project, worktree};
use crate::starts::UNSHOWN;
use crate::starts::{SPLIT, Window, cloned, directory, mouse, projects_of, shown, work_in};
use crate::worktree::worktree_added;

const TYPED: &str = "printf 'own-%s\\n' 42\r";

const PRINTED: &str = "own-42";

const HERE: &str = "lune runs here";

const UNHOSTED: &str = "no remote on GitHub, so nothing is listed here";

const LEFT: &str = "\x1b[104;5u";

const RIGHT: &str = "\x1b[108;5u";

const GROW: &str = "\x1b[61;3u";

const SHRINK: &str = "\x1b[45;3u";

const SIDEBAR: &str = "\x1b[97;5u";

const MENU: &str = "\x1b[97;3u";

const PALETTE: &str = "\x1b[107;9u";

const PROJECTS: &str = "\x1b[111;9u";

const ESC: &str = "\x1b[27u";

const MENU_SHOWS: &str = "n  New ▸";

const FOR_TERMINALS: &str = "the menu is for terminals";

const LEGEND: &str = " /  Search";

const WELCOMES: &str = "d d  close it";

const CLOSE: &str = "\x1b[97;3udd";

const WELCOME: &str = "\x1b[97;3unh";

const PANES: &str = "\x1b[97;3us";

const TABS: &str = "\x1b[97;3ut";

fn as_panes(owned: &Owned, window: &mut Window, folder: &Path) {
    window.send(PANES);
    let panes = Some(Arrangement::Panes.into());
    eventually("the worktree is shown as panes", || {
        (shown(owned, folder)?.arranged == panes).then_some(())
    });
}

fn seated(window: &Window, view: &str) {
    let want = format!(" {view}");
    eventually(&format!("the sidebar seats {view}"), || {
        window
            .screen()
            .iter()
            .any(|line| {
                let sidebar = in_sidebar(line);
                sidebar.starts_with('▌') && sidebar.contains(&want)
            })
            .then_some(())
    });
}

fn frames(window: &Window) -> Option<usize> {
    window
        .screen()
        .iter()
        .map(|line| line.matches('╭').count())
        .max()
}

fn seam(window: &Window) -> Option<usize> {
    window.screen().iter().find_map(|line| {
        let (at, _) = line.char_indices().filter(|(_, c)| *c == '╭').nth(1)?;
        Some(line[..at].chars().count())
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

fn side(window: &Window, split: usize) -> Option<Side> {
    window.cursor().map(|(_, column)| {
        if column >= split {
            Side::Right
        } else {
            Side::Left
        }
    })
}

fn tops(window: &Window) -> Vec<(usize, usize)> {
    let screen = window.screen();
    let edge = screen
        .first()
        .and_then(|line| line.chars().position(|it| it == '╮'));
    let edge = edge.expect("the sidebar's edge is drawn");
    let tops = screen.iter().enumerate().flat_map(|(row, line)| {
        let right = line.chars().enumerate().skip(edge);
        right
            .filter(|(_, it)| *it == '╭')
            .map(move |(column, _)| (row, column))
    });
    tops.collect()
}

#[test]
fn lune_shows_its_worktree_and_what_its_shell_prints() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || {
        let screen = window.screen();
        screen
            .iter()
            .any(|line| line.contains(" work "))
            .then_some(())
    });

    window.send(TYPED);
    let (row, column) = window.spot_of(PRINTED);
    let end = (row, column + PRINTED.len() - 1);
    let swept = [
        mouse(0, (row, column), 'M'),
        mouse(32, end, 'M'),
        mouse(0, end, 'm'),
    ];
    window.send(&swept.concat());
    eventually("the drag copies what it went over", || {
        window.copied().contains(&PRINTED.to_owned()).then_some(())
    });
    let said = format!(" copied \"{PRINTED}\"");
    eventually("the sidebar says what was copied", || {
        window.shows(&said).then_some(())
    });
}

#[test]
fn a_click_on_a_pull_request_s_number_copies_its_link_and_says_so() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let page = "https://github.com/owner/repo/pull/7";
    let pull = json!({ "number": 7, "state": "OPEN", "isDraft": false, "url": page });
    owned.host.gh.open("main", &pull);
    let mut window = Window::open(&owned.host, &dir, &[]);
    let at = window.spot_of("#7");
    window.send(&[mouse(0, at, 'M'), mouse(0, at, 'm')].concat());
    eventually("the click copies the pull request's link", || {
        window.copied().contains(&page.to_owned()).then_some(())
    });
    eventually("the sidebar says what was copied", || {
        window.shows(" copied #7").then_some(())
    });
}

#[test]
fn the_menu_opens_a_view_beside_the_one_lune_shows_and_closes_it() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    window.send(SPLIT);
    let split = eventually("a second terminal opens in the layout", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals
            .iter()
            .filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2).then_some(terminals)
    });
    assert_eq!(split.len(), 2);
    eventually("both frames are drawn beside each other", || {
        let tops = window
            .screen()
            .iter()
            .map(|line| line.matches('╭').count())
            .max()?;
        (tops == 2).then_some(())
    });
    window.send(CLOSE);
    let id = eventually("the view lune is on closes", || {
        let worktree = shown(&owned, &dir.path)?;
        (worktree.terminals.len() == 1).then_some(worktree.id)
    });
    window.send(CLOSE);
    eventually(
        "the worktree goes with its last view, and lune leaves it",
        || {
            let projects = projects_of(&owned);
            let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
            let gone = worktrees.all(|it| it.id != id);
            (gone && projects.len() == 1 && window.shows(UNSHOWN)).then_some(())
        },
    );
    assert!(
        !window.shows("luned's layout"),
        "lune leaves it with no note"
    );
}

#[test]
fn views_open_as_tabs_ctrl_h_and_ctrl_l_step_through_them_and_the_menu_shows_panes_and_back() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    window.send(SPLIT);
    eventually("a second terminal opens as a tab of the first", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals
            .iter()
            .filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2 && frames(&window) == Some(1)).then_some(())
    });
    assert!(
        seam(&window).is_none(),
        "one tab fills the view: {:?}",
        window.screen()
    );
    window.send(TYPED);
    eventually("the tab opened has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    window.send(LEFT);
    eventually("Ctrl h shows the tab before", || {
        (!window.shows(PRINTED)).then_some(())
    });
    window.send(RIGHT);
    eventually("Ctrl l shows the tab after", || {
        window.shows(PRINTED).then_some(())
    });
    as_panes(&owned, &mut window, &dir.path);
    eventually("panes are drawn beside each other", || seam(&window));
    window.send(TABS);
    eventually("tabs again, one filling the view", || {
        let tabbed = shown(&owned, &dir.path)?.arranged == Some(Arrangement::Tabs.into());
        let tabs = tabbed && seam(&window).is_none();
        (tabs && window.shows(PRINTED)).then_some(())
    });
}

#[test]
fn keys_typed_right_after_opening_a_tab_reach_the_tab_opened() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    window.send(&format!("{SPLIT}{TYPED}"));
    eventually("the tab opened has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    window.send(LEFT);
    eventually("the first tab got none of them", || {
        (!window.shows(PRINTED)).then_some(())
    });
}

#[test]
fn keys_typed_right_after_stepping_to_a_tab_reach_the_tab_stepped_to() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    window.send(SPLIT);
    eventually("a second terminal opens as a tab of the first", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals
            .iter()
            .filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2 && frames(&window) == Some(1)).then_some(())
    });
    window.send(&format!("{LEFT}{TYPED}"));
    eventually("the tab stepped to has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    window.send(RIGHT);
    eventually("the tab stepped away from got none of them", || {
        (!window.shows(PRINTED)).then_some(())
    });
}

#[test]
fn keys_typed_right_after_stepping_into_a_staying_right_edge_reach_the_terminal() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    window.send(&format!("{RIGHT}{TYPED}"));
    eventually("the terminal that kept focus gets the keys", || {
        window.shows(PRINTED).then_some(())
    });
}

#[test]
fn the_menu_opens_a_welcome_view_beside_the_terminal_and_d_d_in_it_closes_it() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    window.send(WELCOME);
    eventually("a welcome view is drawn beside the terminal", || {
        (frames(&window) == Some(2) && window.shows("runs no terminal")).then_some(())
    });
    let ids = || -> Vec<u64> {
        shown(&owned, &dir.path)
            .expect("the worktree")
            .terminals
            .iter()
            .map(|it| it.id)
            .collect()
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
fn the_menu_and_its_note_over_the_view_give_the_keys_back_as_they_close() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    assert!(!window.shows(MENU_SHOWS), "no menu is open yet");
    window.send(MENU);
    window.spot_of(MENU_SHOWS);
    window.send(ESC);
    eventually("Esc closes the menu", || {
        (!window.shows(MENU_SHOWS)).then_some(())
    });
    window.send(TYPED);
    window.spot_of(PRINTED);
    window.send(WELCOME);
    window.spot_of(WELCOMES);
    window.send(MENU);
    window.spot_of(FOR_TERMINALS);
    window.send("x");
    eventually("a key closes the note", || {
        (!window.shows(FOR_TERMINALS)).then_some(())
    });
    window.send("dd");
    eventually("the welcome view has the keys again", || {
        (!window.shows(WELCOMES)).then_some(())
    });
}

#[test]
fn super_k_runs_an_action_and_asks_for_a_name_in_its_own_box() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    window.send(PALETTE);
    window.spot_of("─ Actions ");
    window.send("new view\r");
    eventually("the palette opens a second terminal", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        let found = terminals
            .iter()
            .filter(|it| matches!(it.found, Some(Found::Key(_))));
        (found.count() == 2 && !window.shows("─ Actions ")).then_some(())
    });
    window.send(PALETTE);
    window.spot_of("─ Actions ");
    window.send("rename\r");
    window.spot_of("its new name");
    window.send("logs\r");
    eventually("the view is named in the palette", || {
        let terminals = shown(&owned, &dir.path)?.terminals;
        terminals
            .iter()
            .any(|it| it.name.as_deref() == Some("logs"))
            .then_some(())
    });
    assert!(
        !window.shows("its new name"),
        "the palette closes once the name is given"
    );
}

#[test]
fn lune_started_with_no_worktree_gives_the_sidebar_the_keys() {
    let owned = Owned::new();
    let dir = directory("empty");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows no worktree", || {
        window.shows(UNSHOWN).then_some(())
    });
    assert!(!window.shows(LEGEND), "no legend shows yet");
    window.send("?");
    eventually("? in the sidebar shows its legend", || {
        window.shows(LEGEND).then_some(())
    });
}

#[test]
fn a_view_opened_from_the_sidebar_or_its_menu_takes_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    let mut before = tops(&window);
    for (keys, count) in [
        (format!("{SIDEBAR}nv"), 2),
        (format!("{SIDEBAR}{MENU}nv"), 3),
    ] {
        window.send(&keys);
        let drawn = eventually(&format!("{count} terminals are drawn"), || {
            let terminals = shown(&owned, &dir.path)?.terminals;
            let found = terminals
                .iter()
                .filter(|it| matches!(it.found, Some(Found::Key(_))));
            let drawn = tops(&window);
            (found.count() == count && drawn.len() == count).then_some(drawn)
        });
        window.send(&format!("printf 'own-%s\\n' {count}\r"));
        let (row, column) = window.spot_of(&format!("own-{count}"));
        let printer = drawn
            .iter()
            .filter(|top| top.0 < row && top.1 < column)
            .max();
        let printer = printer.expect("a drawn view holds what was printed");
        let new = !before.contains(printer);
        assert!(
            new,
            "the new view printed it at {row},{column}: {drawn:?} after {before:?}"
        );
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
fn a_worktree_opened_or_made_while_lune_shows_none_takes_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || {
        window.shows(" work ").then_some(())
    });
    let other = directory("other");
    let typed_in = |window: &mut Window, folder: &Path, typed: u32| {
        eventually(&format!("a shell opens in {}", folder.display()), || {
            let terminals = shown(&owned, folder)?.terminals;
            let found = terminals
                .iter()
                .filter(|it| matches!(it.found, Some(Found::Key(_))));
            (found.count() == 1 && tops(window).len() == 1).then_some(())
        });
        window.send(&format!("printf 'own-%s\\n' {typed}\r"));
        window.spot_of(&format!("own-{typed}"));
    };
    let unshow = |window: &mut Window| {
        window.send(CLOSE);
        eventually("lune shows no worktree", || {
            window.shows(UNSHOWN).then_some(())
        });
    };
    window.send(TYPED);
    window.spot_of(PRINTED);
    unshow(&mut window);
    window.send("ow");
    eventually("o w lists main", || window.shows("not open").then_some(()));
    window.send("\r");
    typed_in(&mut window, &dir.path, 1);
    unshow(&mut window);
    window.send(&format!("{PROJECTS}{}", other.path.display()));
    eventually("the typed folder is listed", || {
        window.shows(" new project ").then_some(())
    });
    window.send("\r");
    typed_in(&mut window, &other.path, 2);
}

#[test]
fn a_menu_opened_while_a_project_is_made_keeps_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    eventually("the sidebar lists it", || {
        window.shows(" work ").then_some(())
    });
    let other = directory("other");
    window.send(&format!("{PROJECTS}{}", other.path.display()));
    eventually("the typed folder is listed", || {
        window.shows(" new project ").then_some(())
    });
    window.send(&format!("\r{SIDEBAR}n"));
    eventually("lune shows the new project's worktree", || {
        shown(&owned, &other.path)
    });
    eventually("the sidebar lists it", || {
        window.shows(" other ").then_some(())
    });
    eventually("the menu opened meanwhile stays open", || {
        window.shows("─ New ─").then_some(())
    });
}

#[test]
fn ctrl_h_and_ctrl_l_move_between_views_and_alt_equals_and_minus_resize_the_focused_one() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    window.send(SPLIT);
    let split = eventually("two frames are drawn beside each other", || seam(&window));
    for (key, to) in [(LEFT, Side::Left), (RIGHT, Side::Right), (LEFT, Side::Left)] {
        window.send(key);
        eventually(&format!("the cursor is in the {to:?} view"), || {
            (side(&window, split) == Some(to)).then_some(())
        });
    }
    window.send(GROW);
    let grown = eventually("the left view grows", || {
        seam(&window).filter(|at| *at > split)
    });
    window.send(SHRINK);
    eventually("the left view shrinks", || {
        seam(&window).filter(|at| *at < grown)
    });
    assert_eq!(
        side(&window, split),
        Some(Side::Left),
        "{:?}",
        window.screen()
    );
}

#[test]
fn ctrl_h_and_ctrl_l_at_the_edge_go_to_the_worktree_beside_or_ctrl_h_to_the_sidebar() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let first = eventually("lune shows work", || shown(&owned, &dir.path)).id;
    let project = projects_of(&owned)
        .into_iter()
        .find(|it| it.name == "work")
        .unwrap()
        .id;
    let docs = dir.path.parent().unwrap().join("docs");
    worktree_added(dir.path.parent().unwrap(), "docs");
    let second = worktree(&owned.host, project, &docs);
    eventually("the tab bar lists docs", || {
        window.shows("docs").then_some(())
    });
    eventually("the view has the keys", || {
        window.cursor().is_some().then_some(())
    });
    let used = |id: u64| used(&owned, id);
    window.send(LEFT);
    eventually(
        "Ctrl h past the first worktree gives the sidebar the keys",
        || window.cursor().is_none().then_some(()),
    );
    assert!(
        used(second).is_none(),
        "Ctrl h goes to no worktree past the project's first"
    );
    window.send("?");
    eventually("? in the sidebar shows its legend", || {
        window.shows(LEGEND).then_some(())
    });
    window.send(RIGHT);
    eventually("Ctrl l in the sidebar gives the view the keys", || {
        window.cursor().is_some().then_some(())
    });
    window.send(TYPED);
    eventually("the shell has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    assert!(
        used(second).is_none(),
        "Ctrl l from the sidebar stays on the worktree shown"
    );
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || {
        (used(second) > used(first)).then_some(())
    });
    window.send(LEFT);
    eventually("Ctrl h goes back", || {
        (used(first) > used(second)).then_some(())
    });
}

#[test]
fn an_edge_answered_after_the_keys_went_and_came_back_leaves_them_with_the_view() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    eventually("the view has the keys", || {
        window.cursor().is_some().then_some(())
    });
    window.send(&format!("{LEFT}{SIDEBAR}{SIDEBAR}{TYPED}"));
    eventually("the shell has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    lune_waits::never(
        "the edge of a step asked before Ctrl a gives the sidebar the keys",
        || window.cursor().is_none().then_some(()),
    );
}

#[test]
fn an_edge_answered_after_a_click_on_another_view_leaves_the_keys_with_the_view() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    window.send(SPLIT);
    let split = eventually("two frames are drawn beside each other", || seam(&window));
    window.send(LEFT);
    eventually("the cursor is in the left view", || {
        (side(&window, split) == Some(Side::Left)).then_some(())
    });
    let right = (5, split + 5);
    let click = [mouse(0, right, 'M'), mouse(0, right, 'm')].concat();
    window.send(&format!("{LEFT}{click}{TYPED}"));
    eventually("the shell has the keys", || {
        window.shows(PRINTED).then_some(())
    });
    lune_waits::never(
        "the edge of a step asked before the click gives the sidebar the keys",
        || window.cursor().is_none().then_some(()),
    );
}

#[test]
fn ctrl_h_on_a_project_s_home_gives_the_sidebar_the_keys() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    let row = sidebar_row(&window, "● work");
    click(&mut window, row);
    eventually("a click on work's row shows its home", || {
        window.shows(UNHOSTED).then_some(())
    });
    window.send(&format!("{LEFT}?"));
    eventually("? in the sidebar shows its legend", || {
        window.shows(LEGEND).then_some(())
    });
}

#[test]
fn a_click_on_a_worktree_s_row_goes_to_the_view_last_focused_there_and_on_a_view_s_row_to_it() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let first = eventually("lune shows work", || shown(&owned, &dir.path)).id;
    seated(&window, "work/main");
    as_panes(&owned, &mut window, &dir.path);
    window.send(SPLIT);
    let split = eventually("two frames are drawn beside each other", || seam(&window));
    eventually("the view opened has the keys", || {
        (side(&window, split) == Some(Side::Right)).then_some(())
    });
    let project = projects_of(&owned)
        .into_iter()
        .find(|it| it.name == "work")
        .unwrap()
        .id;
    let root = dir.path.parent().unwrap();
    worktree_added(root, "docs");
    let second = worktree(&owned.host, project, &root.join("docs"));
    eventually("the tab bar lists docs", || {
        window.shows("docs").then_some(())
    });
    let used = |id: u64| used(&owned, id);
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || {
        (used(second) > used(first)).then_some(())
    });
    let main = sidebar_row(&window, "▾ main");
    let back_on = |window: &Window, to: Side| {
        let back = used(first) > used(second) && seam(window) == Some(split);
        (back && side(window, split) == Some(to)).then_some(())
    };
    click(&mut window, main);
    eventually(
        "a click on main's row goes back to the view it left",
        || back_on(&window, Side::Right),
    );
    let left = main + 1;
    let screen = window.screen();
    let below = in_sidebar(&screen[left]);
    assert!(
        below.contains(" work/main"),
        "main's first view sits below its row: {screen:?}"
    );
    click(&mut window, left);
    eventually("a click on a view's row focuses it", || {
        (side(&window, split) == Some(Side::Left)).then_some(())
    });
    let docs = sidebar_row(&window, "▾ docs");
    click(&mut window, docs);
    eventually("a click on docs's row goes there", || {
        (used(second) > used(first)).then_some(())
    });
    click(&mut window, main);
    eventually("main's row goes to the view focused last", || {
        back_on(&window, Side::Left)
    });
}

#[test]
fn a_click_on_a_project_s_row_goes_to_its_home_and_one_on_a_worktree_s_back() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    let main = eventually("lune shows work", || shown(&owned, &dir.path)).id;
    let project = projects_of(&owned)
        .into_iter()
        .find(|it| it.name == "work")
        .unwrap()
        .id;
    let root = dir.path.parent().unwrap();
    worktree_added(root, "docs");
    let docs = worktree(&owned.host, project, &root.join("docs"));
    let used = |id: u64| used(&owned, id);
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || {
        (used(docs) > used(main)).then_some(())
    });
    let row = sidebar_row(&window, "● work");
    click(&mut window, row);
    eventually("a click on work's row shows its home", || {
        window.shows(UNHOSTED).then_some(())
    });
    assert!(used(docs) > used(main), "the home goes to no worktree");
    let row = sidebar_row(&window, "▾ main");
    click(&mut window, row);
    eventually("a click on main's row goes there, out of the home", || {
        (used(main) > used(docs) && !window.shows(UNHOSTED)).then_some(())
    });
}

#[test]
fn a_tie_for_the_worktree_used_last_goes_to_the_first_listed_wherever_lune_picks_one() {
    let owned = Owned::new();
    let opened = |name: &str| {
        let clone = cloned(name, &["two"]);
        let root = fs::canonicalize(&clone.path).unwrap();
        let made = project(&owned.host, &root, None);
        let worktrees = ["main", "two"].map(|it| worktree(&owned.host, made, &root.join(it)));
        (clone, worktrees)
    };
    let (work, [one, two]) = opened("work");
    let (_other, [three, four]) = opened("other");
    let (_named, [five, six]) = opened("named");
    let projects = projects_of(&owned);
    let listed = projects
        .iter()
        .flat_map(|it| it.worktrees().unwrap())
        .map(|it| it.id);
    assert_eq!(
        listed.collect::<Vec<u64>>(),
        [one, two, three, four, five, six]
    );
    let gone_to = || {
        let all = [one, two, three, four, five, six].into_iter();
        all.filter(|it| used(&owned, *it).is_some())
            .collect::<Vec<u64>>()
    };
    assert!(
        gone_to().is_empty(),
        "a worktree made over luned's API is gone to never"
    );
    let mut window = Window::open(&owned.host, &work, &[]);
    eventually("luned's start at work's root goes to its first", || {
        (gone_to() == [one]).then_some(())
    });
    window.send(CLOSE);
    eventually(
        "one's last pane closed, lune goes to the first open of those left",
        || (gone_to() == [two]).then_some(()),
    );
    let row = sidebar_row(&window, "● other");
    click(&mut window, row);
    eventually("a click on other's row shows its home", || {
        window.shows(UNHOSTED).then_some(())
    });
    assert_eq!(gone_to(), [two], "and goes to none of other's worktrees");
    drop(window);
    let window = Window::open(&owned.host, &directory("anywhere"), &["named"]);
    eventually(
        "a lune started by the project's name shows its home",
        || window.shows(UNHOSTED).then_some(()),
    );
    assert_eq!(gone_to(), [two], "and goes to none of named's worktrees");
}

#[test]
fn a_home_s_talk_goes_on_while_lune_shows_a_worktree_and_esc_stops_only_its_turn() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let home = owned.host.home.path().to_owned();
    let at = home.display();
    let claude = format!(
        r#"n=$(cat "{at}/turns" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$@" > "{at}/args-$n"
echo "${{LUNE_TURN_BEFORE:-none}}" > "{at}/before-$n"
echo "$n" > "{at}/turns.tmp"
mv "{at}/turns.tmp" "{at}/turns"
cat > /dev/null
echo '{{"type":"system","subtype":"init","session_id":"7c22"}}'
case "$n" in
    1) sleep 2; echo '{{"type":"result","subtype":"success","is_error":false,"result":"Started all three."}}'; touch "{at}/answered" ;;
    2) sleep 60 ;;
    *) echo '{{"type":"result","subtype":"success","is_error":false,"result":"Went on."}}' ;;
esac
"#
    );
    script(&owned.host.bin().join("claude"), &claude);
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    let row = sidebar_row(&window, "● work");
    click(&mut window, row);
    eventually("a click on work's row shows its home", || {
        window.shows(UNHOSTED).then_some(())
    });
    window.send("tackle these\r");
    eventually("the agent's turn began", || {
        home.join("turns").exists().then_some(())
    });
    let row = sidebar_row(&window, "▾ main");
    click(&mut window, row);
    eventually("lune shows main, out of the home", || {
        (!window.shows(UNHOSTED)).then_some(())
    });
    eventually("the agent answered while the home was not shown", || {
        home.join("answered").exists().then_some(())
    });
    let row = sidebar_row(&window, "● work");
    click(&mut window, row);
    eventually("the home comes back with its talk and the answer", || {
        (window.shows("tackle these") && window.shows("Started all three.")).then_some(())
    });
    window.send("more\r");
    eventually("the second turn began", || {
        (fs::read_to_string(home.join("turns")).ok()?.trim() == "2").then_some(())
    });
    window.send(ESC);
    eventually("Esc stops the turn", || {
        window.shows("stopped: the agent was ended").then_some(())
    });
    assert!(window.shows("Started all three."), "and the talk stays");
    window.send("again\r");
    eventually("the next turn answers", || {
        window.shows("Went on.").then_some(())
    });
    let args = fs::read_to_string(home.join("args-3")).unwrap();
    assert!(
        args.contains("--resume 7c22"),
        "it goes on in the same session: {args}"
    );
    let before = |turn: u32| fs::read_to_string(home.join(format!("before-{turn}"))).unwrap();
    assert_ne!(
        before(2).trim(),
        "none",
        "a turn after one that answered has that one before it"
    );
    assert_eq!(
        before(3).trim(),
        "none",
        "and a turn after a stopped one has none, so prune plans again"
    );
    assert!(
        !window.shows("it ended with no answer"),
        "the stopped turn's end is heard by none"
    );
}

fn sidebar_row(window: &Window, text: &str) -> usize {
    eventually(&format!("the sidebar shows {text:?}"), || {
        window
            .screen()
            .iter()
            .position(|line| in_sidebar(line).contains(text))
    })
}

fn in_sidebar(line: &str) -> String {
    line.chars()
        .skip_while(|it| *it == '│')
        .take_while(|it| *it != '│')
        .collect()
}

fn click(window: &mut Window, row: usize) {
    window.send(&[mouse(0, (row, 10), 'M'), mouse(0, (row, 10), 'm')].concat());
}

#[test]
fn a_worktree_s_last_view_closed_takes_lune_to_the_one_used_before_with_no_note() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    window.send(TYPED);
    eventually("main's shell prints", || {
        window.shows(PRINTED).then_some(())
    });
    let project = projects_of(&owned)
        .into_iter()
        .find(|it| it.name == "work")
        .unwrap()
        .id;
    let root = dir.path.parent().unwrap();
    worktree_added(root, "docs");
    let docs = root.join("docs");
    let gone = worktree(&owned.host, project, &docs);
    window.send(RIGHT);
    eventually("Ctrl l goes to docs", || {
        used(&owned, gone).is_some().then_some(())
    });
    window.send(CLOSE);
    eventually(
        "docs goes with its last view, and lune goes back to main",
        || {
            let projects = projects_of(&owned);
            let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
            (worktrees.all(|it| it.id != gone) && window.shows(PRINTED)).then_some(())
        },
    );
    lune_waits::never("lune leaves main", || {
        (!window.shows(PRINTED)).then(|| window.screen())
    });
    assert!(
        !window.shows("luned's layout"),
        "lune leaves docs with no note"
    );
}

fn used(owned: &Owned, worktree: u64) -> Option<(i64, i32)> {
    let projects = projects_of(owned);
    let mut worktrees = projects.iter().flat_map(|it| it.worktrees().unwrap());
    let found = worktrees.find(|it| it.id == worktree);
    found
        .and_then(|it| it.used_at)
        .map(|at| (at.seconds, at.nanos))
}

fn both_show(owned: &Owned, name: &str, mut windows: [Window; 2]) {
    eventually(&format!("both lunes show {name}"), || {
        for window in &mut windows {
            let ended = window.lune.exit_status();
            assert!(
                ended.is_none(),
                "a lune ended, {ended:?}: {:?}",
                window.screen()
            );
        }
        let shows = |window: &Window| window.shows(name);
        windows.iter().all(shows).then_some(())
    });
    let projects = projects_of(owned);
    let made: Vec<_> = projects
        .iter()
        .filter(|project| project.name == name)
        .collect();
    assert_eq!(made.len(), 1, "{projects:?}");
    assert_eq!(made[0].worktrees().unwrap().len(), 1, "{projects:?}");
}

#[test]
fn named_lunes_started_at_once_show_the_project_one_of_them_made() {
    let owned = Owned::new();
    let dir = directory("anywhere");
    let opening = || Window::opening(&owned.host, &dir, &["work"]);
    both_show(&owned, "work", [opening(), opening()]);
}

#[test]
fn a_lune_started_in_a_shell_of_the_view_it_shows_leaves_the_view_its_size() {
    let owned = Owned::new();
    let dir = work_in(&owned, "work");
    let mut window = Window::open(&owned.host, &dir, &[]);
    eventually("lune shows work", || shown(&owned, &dir.path));
    let size = || {
        owned
            .listing()
            .terminals
            .first()?
            .size
            .map(|it| (it.columns, it.rows))
    };
    let first = eventually("the shell is sized", size);
    window.send(&format!("{}\r", LUNE.as_str()));
    eventually("the inner lune says it runs there", || {
        window.shows(HERE).then_some(())
    });
    let resized = || (size() != Some(first)).then(|| (size(), window.screen()));
    let quiet = const { Span::of(QUIET.saturating_mul(2)) };
    lune_waits::through(quiet, "the inner lune resizes its view", resized);
}
