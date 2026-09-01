pub struct Cursor<T> {
    rows: Vec<T>,
    selected: Option<usize>,
}

impl<T> Default for Cursor<T> {
    fn default() -> Self {
        Cursor { rows: Vec::new(), selected: None }
    }
}

impl<T> Cursor<T> {
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_row(&self) -> Option<&T> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    pub fn replace(&mut self, rows: Vec<T>, held: impl Fn(&T) -> bool) {
        self.rows = rows;
        self.selected = match self.rows.iter().position(held) {
            Some(index) => Some(index),
            None => (!self.rows.is_empty()).then_some(0),
        };
    }

    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }

    #[cfg(test)]
    pub fn seeded(rows: Vec<T>, index: usize) -> Self {
        let mut cursor = Cursor::default();
        cursor.replace(rows, |_| false);
        cursor.move_selection(index as isize);
        cursor
    }
}

impl<T> std::ops::Deref for Cursor<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_is_absent_exactly_when_there_are_no_rows() {
        let mut cursor: Cursor<&str> = Cursor::default();
        assert_eq!(cursor.selected(), None);

        cursor.replace(vec!["a", "b"], |_| false);
        assert_eq!(cursor.selected(), Some(0));

        cursor.replace(Vec::new(), |_| false);
        assert_eq!(cursor.selected(), None);
        assert_eq!(cursor.selected_row(), None);

        cursor.move_selection(1);
        assert_eq!(cursor.selected(), None);
    }

    #[test]
    fn the_cursor_follows_the_row_it_was_holding() {
        let mut cursor = Cursor::default();
        cursor.replace(vec!["a", "b", "c"], |_| false);
        cursor.move_selection(2);
        assert_eq!(cursor.selected_row(), Some(&"c"));

        cursor.replace(vec!["c", "a", "b"], |row| *row == "c");
        assert_eq!(cursor.selected(), Some(0));

        cursor.replace(vec!["a", "b"], |row| *row == "c");
        assert_eq!(cursor.selected(), Some(0));
    }

    #[test]
    fn the_cursor_stops_at_both_ends_and_does_not_wrap() {
        let mut cursor = Cursor::default();
        cursor.replace(vec!["a", "b", "c"], |_| false);

        cursor.move_selection(-1);
        assert_eq!(cursor.selected(), Some(0));
        cursor.move_selection(99);
        assert_eq!(cursor.selected(), Some(2));
        cursor.move_selection(-99);
        assert_eq!(cursor.selected(), Some(0));
    }

    #[test]
    fn the_rows_deref_to_a_slice() {
        let cursor = Cursor::seeded(vec!["a", "b", "c"], 1);
        assert_eq!(cursor.len(), 3);
        assert_eq!(cursor[1], "b");
        assert_eq!(&cursor[1..], ["b", "c"]);
        assert_eq!(cursor.selected(), Some(1));
        assert!(!cursor.is_empty());
    }
}
