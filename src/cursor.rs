/// Which rows of a list the cursor can be on: every row, unless the rows say
/// otherwise. A row that heads others can be one the cursor passes over, but
/// a list that is not empty has a row to be on, so that the cursor is absent
/// only where there are no rows.
pub trait Stops: Sized {
    fn stops(_rows: &[Self], _at: usize) -> bool {
        true
    }
}

pub struct Cursor<T> {
    rows: Vec<T>,
    selected: Option<usize>,
}

impl<T> Default for Cursor<T> {
    fn default() -> Self {
        Cursor { rows: Vec::new(), selected: None }
    }
}

impl<T: Stops> Cursor<T> {
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn selected_row(&self) -> Option<&T> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    pub fn replace(&mut self, rows: Vec<T>, held: impl Fn(&T) -> bool) {
        let at = rows.iter().position(held);
        self.replace_at(rows, at);
    }

    /// `replace`, for a caller that has found the row to hold itself. A row
    /// the cursor cannot be on hands it to the first after it that it can,
    /// or else to the last before it.
    pub fn replace_at(&mut self, rows: Vec<T>, at: Option<usize>) {
        self.rows = rows;
        let from = at.filter(|index| *index < self.rows.len()).unwrap_or(0);
        self.selected = self.stop_from(from).or_else(|| self.stop_before(from));
    }

    /// Puts the cursor on row `index`, and says whether it can be there. A row
    /// it cannot be on leaves the cursor where it was.
    pub fn select(&mut self, index: usize) -> bool {
        let stops = self.stops_at(index);
        if stops {
            self.selected = Some(index);
        }
        stops
    }

    /// Moves the cursor `delta` rows it can be on, down or up, and no further
    /// than the last of them or the first.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(mut at) = self.selected else { return };
        for _ in 0..delta.unsigned_abs() {
            let next = if delta > 0 { self.stop_from(at + 1) } else { self.stop_before(at) };
            let Some(next) = next else { break };
            at = next;
        }
        self.selected = Some(at);
    }

    /// Whether the cursor can be on row `index`.
    fn stops_at(&self, index: usize) -> bool {
        index < self.rows.len() && T::stops(&self.rows, index)
    }

    fn stop_from(&self, from: usize) -> Option<usize> {
        (from..self.rows.len()).find(|index| self.stops_at(*index))
    }

    fn stop_before(&self, before: usize) -> Option<usize> {
        (0..before).rev().find(|index| self.stops_at(*index))
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

    /// A `#` row heads the rows under it, and is passed over while it heads
    /// any.
    impl Stops for &str {
        fn stops(rows: &[Self], at: usize) -> bool {
            !rows[at].starts_with('#') || rows.get(at + 1).is_none_or(|next| next.starts_with('#'))
        }
    }

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
    fn a_row_that_is_not_there_cannot_be_selected() {
        let mut cursor = Cursor::seeded(vec!["a", "b"], 0);
        assert!(cursor.select(1));
        assert_eq!(cursor.selected(), Some(1));
        assert!(!cursor.select(2));
        assert_eq!(cursor.selected(), Some(1));
        let mut empty: Cursor<&str> = Cursor::default();
        assert!(!empty.select(0));
        assert_eq!(empty.selected(), None);
    }

    /// A row that heads others is passed over, whether the cursor moves or
    /// its rows are replaced, and a row that heads none is a stop.
    #[test]
    fn the_cursor_passes_over_the_rows_it_does_not_stop_on() {
        let rows = || vec!["# a", "a1", "a2", "# b", "b1", "# c"];
        let mut cursor = Cursor::default();
        cursor.replace_at(rows(), None);
        assert_eq!(cursor.selected_row(), Some(&"a1"));
        cursor.move_selection(-1);
        assert_eq!(cursor.selected_row(), Some(&"a1"));
        cursor.move_selection(2);
        assert_eq!(cursor.selected_row(), Some(&"b1"));
        cursor.move_selection(99);
        assert_eq!(cursor.selected_row(), Some(&"# c"));
        cursor.move_selection(-2);
        assert_eq!(cursor.selected_row(), Some(&"a2"));

        assert!(!cursor.select(3));
        assert_eq!(cursor.selected_row(), Some(&"a2"));
        cursor.replace_at(rows(), Some(3));
        assert_eq!(cursor.selected_row(), Some(&"b1"));
        cursor.replace_at(vec!["# a", "a1", "# b"], Some(2));
        assert_eq!(cursor.selected_row(), Some(&"# b"));
        cursor.replace_at(vec!["# a", "a1", "# b", "b1"], Some(2));
        assert_eq!(cursor.selected_row(), Some(&"b1"));
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
