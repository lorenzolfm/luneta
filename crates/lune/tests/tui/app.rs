use std::fs;
use std::path::Path;

use lune::{App, Launch};
use ratatui::backend::TestBackend;
use ratatui::Terminal;

const COLUMNS: u16 = 120;

const ROWS: u16 = 40;

const UNSHOWN: &str = "No worktree to show yet.";

const LOST: &str = "luned: none runs";

const SIDEBAR_EDGE: &str = "╮";

const KEPT: u16 = 60;

fn screen(state_folder: &Path, folder: Result<&Path, &str>) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    let launch = Launch {
        folder: folder.map(Path::to_owned).map_err(str::to_owned),
        dir: dir.path().to_owned(),
        columns: COLUMNS,
        rows: ROWS,
    };
    let (mut app, _inbox) = App::open(launch, state_folder);
    let mut terminal = Terminal::new(TestBackend::new(COLUMNS, ROWS)).unwrap();
    terminal.draw(|frame| app.paint(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let row = |y| (0..COLUMNS).map(|x| buffer[(x, y)].symbol()).collect::<String>();
    (0..ROWS).map(row).collect()
}

fn column_of(screen: &[String], text: &str) -> usize {
    let found = screen.iter().find_map(|row| row.find(text).map(|at| row[..at].chars().count()));
    found.unwrap_or_else(|| panic!("{text:?} is not on the screen:\n{}", screen.join("\n")))
}

#[test]
fn an_app_opened_with_no_luned_says_why_beside_no_worktree() {
    let state = tempfile::tempdir().unwrap();
    let shown = screen(state.path(), Err("none runs"));
    let edge = column_of(&shown, SIDEBAR_EDGE);
    assert!(column_of(&shown, LOST) < edge);
    assert!(column_of(&shown, UNSHOWN) > edge);
}

#[test]
fn an_app_opens_its_sidebar_as_wide_as_its_state_folder_kept_it() {
    let (narrow, wide) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    fs::write(wide.path().join("width"), format!("{KEPT}\n")).unwrap();
    let why = Err("none runs");
    let (narrow, wide) = (screen(narrow.path(), why), screen(wide.path(), why));
    assert_eq!(column_of(&wide, SIDEBAR_EDGE) + 1, usize::from(KEPT));
    assert!(column_of(&narrow, SIDEBAR_EDGE) + 1 < usize::from(KEPT));
}
