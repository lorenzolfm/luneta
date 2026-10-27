//! How wide the sidebar opens: as wide as its edge was last dragged, in any
//! luneta on the host, and kept for the next, in the file `luneta/width` of
//! `$XDG_STATE_HOME`, or of `~/.local/state` where that is not set. A luneta
//! reads it as it starts, and writes it as the edge is let go: what another
//! drags shows from the next start, so that one window does not resize
//! another.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::sidebar;
use crate::state;

pub struct Width {
    columns: u16,
    /// The file it is kept in. Without one, it is this luneta's alone.
    file: Option<PathBuf>,
}

impl Width {
    /// The width kept for every luneta on the host, or `sidebar::COLUMNS`
    /// where none is, and why the file kept could not be read, where it
    /// could not.
    pub fn kept() -> (Self, Option<String>) {
        match state::path("width") {
            Some(file) => Width::within(file),
            None => (Width { columns: sidebar::COLUMNS, file: None }, None),
        }
    }

    fn within(file: PathBuf) -> (Self, Option<String>) {
        let read = match fs::read_to_string(&file) {
            Ok(text) => text.trim().parse::<u16>().map_err(|error| error.to_string()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(sidebar::COLUMNS),
            Err(error) => Err(error.to_string()),
        };
        let (columns, unread) = match read {
            Ok(columns) => (columns.max(sidebar::NARROWEST), None),
            Err(error) => (sidebar::COLUMNS, Some(format!("{}: {error}", file.display()))),
        };
        (Width { columns, file: Some(file) }, unread)
    }

    /// How many columns the sidebar takes open, where the terminal has twice
    /// as many.
    pub fn columns(&self) -> u16 {
        self.columns
    }

    /// Puts the sidebar's edge, its last column, at `column` of a terminal
    /// `terminal` columns wide, counted from the sidebar's first: as far as
    /// half the terminal, and no nearer than `sidebar::NARROWEST`.
    pub fn drag(&mut self, column: u16, terminal: u16) {
        self.columns = column.saturating_add(1).min(terminal / 2).max(sidebar::NARROWEST);
    }

    /// Keeps the width for the next luneta, or says why it cannot.
    pub fn keep(&self) -> Result<(), String> {
        let Some(file) = &self.file else {
            return Err("neither XDG_STATE_HOME nor HOME is set, to keep it in".to_owned());
        };
        let folder = file.parent().expect("a file of `state::path` is in a folder");
        fs::create_dir_all(folder)
            .and_then(|()| fs::write(file, format!("{}\n", self.columns)))
            .map_err(|error| format!("{}: {error}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The edge as it was let go is where the next luneta opens the sidebar.
    #[test]
    fn a_width_dragged_and_let_go_opens_the_next_luneta() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join("width");
        let (mut width, unread) = Width::within(file.clone());
        assert_eq!((width.columns(), unread), (sidebar::COLUMNS, None));

        width.drag(44, 120);
        assert_eq!(width.columns(), 45, "the edge is under the mouse");
        width.keep().unwrap();
        assert_eq!(Width::within(file.clone()).0.columns(), 45);
    }

    /// The edge goes as far as half the terminal, as the sidebar did before
    /// it could be dragged, and stays far enough off the sidebar's first
    /// column for a name. A terminal too narrow for both has the sidebar at
    /// half of it, as `host` draws it.
    #[test]
    fn a_drag_stays_between_the_narrowest_and_half_the_terminal() {
        let mut width = Width { columns: sidebar::COLUMNS, file: None };
        let narrowest = sidebar::NARROWEST;
        for (column, terminal, columns) in
            [(3, 120, narrowest), (100, 120, 60), (59, 120, 60), (20, 24, narrowest)]
        {
            width.drag(column, terminal);
            assert_eq!(width.columns(), columns, "column {column} of {terminal}");
        }
    }

    /// A file that names no width, or one too narrow, as one written by
    /// hand, opens the sidebar as wide as it may, and the next drag writes
    /// over it. One that cannot be written says why.
    #[test]
    fn a_file_that_fails_the_width_says_why() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("width");
        fs::write(&file, "wide\n").unwrap();
        let (mut width, unread) = Width::within(file.clone());
        assert_eq!(width.columns(), sidebar::COLUMNS);
        assert!(unread.is_some_and(|unread| unread.contains("width")));
        width.drag(39, 120);
        width.keep().unwrap();
        assert_eq!(Width::within(file.clone()).0.columns(), 40);

        fs::write(&file, "3").unwrap();
        assert_eq!(Width::within(file).0.columns(), sidebar::NARROWEST);

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let (width, unread) = Width::within(blocker.join("width"));
        assert_eq!((width.columns(), unread.is_some()), (sidebar::COLUMNS, true));
        assert!(width.keep().is_err_and(|why| why.contains("not a folder")));
    }
}
