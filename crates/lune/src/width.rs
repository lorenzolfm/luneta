use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use crate::sidebar;
use crate::state;

pub struct Width {
    columns: u16,
    file: Result<PathBuf, state::Unset>,
}

impl Width {
    pub fn kept(state: &state::Folder) -> (Self, Option<String>) {
        match state.path("width") {
            Ok(file) => Width::within(file),
            Err(unset) => (Width { columns: sidebar::COLUMNS, file: Err(unset) }, None),
        }
    }

    fn within(file: PathBuf) -> (Self, Option<String>) {
        let read = match fs::read_to_string(&file) {
            Ok(text) => match text.trim().parse::<u16>() {
                Ok(columns) if columns < sidebar::NARROWEST => {
                    Err(format!("{columns} is narrower than {}", sidebar::NARROWEST))
                },
                parsed => parsed.map_err(|error| error.to_string()),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(sidebar::COLUMNS),
            Err(error) => Err(error.to_string()),
        };
        let (columns, unread) = match read {
            Ok(columns) => (columns, None),
            Err(error) => (sidebar::COLUMNS, Some(format!("{}: {error}", file.display()))),
        };
        (Width { columns, file: Ok(file) }, unread)
    }

    pub fn columns(&self) -> u16 {
        self.columns
    }

    pub fn drag(&mut self, column: u16, terminal: u16) {
        self.columns = column.saturating_add(1).min(terminal / 2).max(sidebar::NARROWEST);
    }

    pub fn keep(&self) -> Result<(), String> {
        let file = self.file.as_ref().map_err(ToString::to_string)?;
        let folder = file.parent().expect("a file of `state::path` is in a folder");
        let unfinished = folder.join(format!("width.{}", state::unfinished()));
        fs::create_dir_all(folder)
            .and_then(|()| fs::write(&unfinished, format!("{}\n", self.columns)))
            .and_then(|()| fs::rename(&unfinished, file))
            .map_err(|error| format!("{}: {error}", file.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_width_dragged_and_let_go_opens_the_next_lune() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("lune").join("width");
        let (mut width, unread) = Width::within(file.clone());
        assert_eq!((width.columns(), unread), (sidebar::COLUMNS, None));

        width.drag(44, 120);
        assert_eq!(width.columns(), 45, "the edge is under the mouse");
        width.keep().unwrap();
        assert_eq!(Width::within(file.clone()).0.columns(), 45);
    }

    #[test]
    fn a_drag_stays_between_the_narrowest_and_half_the_terminal() {
        let mut width = Width { columns: sidebar::COLUMNS, file: Err(state::Unset) };
        let narrowest = sidebar::NARROWEST;
        for (column, terminal, columns) in
            [(3, 120, narrowest), (100, 120, 60), (59, 120, 60), (20, 24, narrowest)]
        {
            width.drag(column, terminal);
            assert_eq!(width.columns(), columns, "column {column} of {terminal}");
        }
    }

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
        let (width, unread) = Width::within(file);
        assert_eq!(width.columns(), sidebar::COLUMNS);
        assert!(unread.is_some_and(|unread| unread.contains("3 is narrower than 16")));

        let blocker = scratch.path().join("not a folder");
        fs::write(&blocker, "").unwrap();
        let (width, unread) = Width::within(blocker.join("width"));
        assert_eq!((width.columns(), unread.is_some()), (sidebar::COLUMNS, true));
        assert!(width.keep().is_err_and(|why| why.contains("not a folder")));
    }
}
