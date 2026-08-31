//! A list and the cursor into it, as one value.
//!
//! Three screens each held `rows: Vec<T>` beside `selected: Option<usize>`, with one invariant
//! between them: `None` only when `rows` is empty. It was written into two of the three doc
//! comments and left implied by the third, and nothing enforced any of them. Every read
//! re-checked it — `self.selected.and_then(|i| self.rows.get(i))` — and the three
//! `move_selection` bodies were byte-identical, so the clamp that keeps the cursor inside the
//! list was maintained in triplicate.
//!
//! The cursor is an index and not a key, and it is worth saying why, because a key is the
//! obvious answer. A rebuild wants a key: sessions hold by name, directories by path, agents by
//! `(session, pane)`. A draw wants the index, and cannot be talked out of it — the counter
//! prints `3/12`, the viewport scrolls to keep the selection on screen, and every row asks
//! whether it is the selected one. A key-only cursor would have to hand out an index anyway,
//! found by a search per frame. Both are true at once, so [`Cursor::replace`] takes the key as a
//! predicate and derives the index from it once, here, instead of at the end of three rebuilds.
//!
//! Reads reach the rows through `Deref<Target = [T]>`, so indexing, slicing, `len` and `iter`
//! are the slice's own and cost nothing to keep. Writes have only two doors, [`Cursor::replace`]
//! and [`Cursor::move_selection`], and neither can leave the cursor outside the list.

/// The rows of one screen and the cursor in them.
pub struct Cursor<T> {
    rows: Vec<T>,
    /// An index into `rows`, `None` if and only if `rows` is empty. Private, because that "if
    /// and only if" is the point: it now holds by construction instead of by comment.
    selected: Option<usize>,
}

/// Derived by hand. `#[derive(Default)]` would demand `T: Default`, and an empty list needs
/// nothing of its element type.
impl<T> Default for Cursor<T> {
    fn default() -> Self {
        Cursor { rows: Vec::new(), selected: None }
    }
}

impl<T> Cursor<T> {
    /// Where the cursor is, for the renderer that needs the number. `None` only when the list
    /// is empty.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// The row under the cursor, or `None` when there are no rows.
    ///
    /// The `get` is how a slice is indexed safely, not a doubt about the index: this is the one
    /// place that re-check now lives, rather than at each of its callers.
    pub fn selected_row(&self) -> Option<&T> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    /// Replace the rows, keeping the cursor on the row that `held` recognises.
    ///
    /// `held` is whichever key comparison the caller holds by. A rebuild that means "snap to
    /// the top" passes a predicate that matches nothing, which is deliberately the same path as
    /// a held row the filter has removed: in both cases there is no row to return to.
    pub fn replace(&mut self, rows: Vec<T>, held: impl Fn(&T) -> bool) {
        self.rows = rows;
        self.selected = match self.rows.iter().position(held) {
            Some(index) => Some(index),
            // Nothing matched: the top, or nothing at all when there is no top. That `None` is
            // what tells the session screen `Enter` must act on the typed text instead of on a
            // row. See [`crate::render::enter_action`].
            None => (!self.rows.is_empty()).then_some(0),
        };
    }

    /// Move the cursor. It stops at both ends and does not wrap, so that you can hold a key
    /// down to reach the top match.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }

    /// A list with the cursor already at `index`, for the render tests, which draw lists they
    /// did not rebuild.
    ///
    /// It goes through [`Cursor::replace`] and [`Cursor::move_selection`] rather than setting
    /// the two fields, so a test cannot ask for the state this type exists to forbid. An index
    /// past the end clamps, and an index into an empty list is `None`.
    #[cfg(test)]
    pub fn seeded(rows: Vec<T>, index: usize) -> Self {
        let mut cursor = Cursor::default();
        cursor.replace(rows, |_| false);
        cursor.move_selection(index as isize);
        cursor
    }
}

/// Read-only access to the rows. Everything the renderer does to a list — index it, slice a
/// window out of it, count it, iterate it — is a slice operation, and none of it can move the
/// cursor.
impl<T> std::ops::Deref for Cursor<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant, at the only two doors that could break it: an empty list has no cursor,
    /// and a list with rows always has one. This was three prose comments.
    #[test]
    fn a_cursor_is_absent_exactly_when_there_are_no_rows() {
        let mut cursor: Cursor<&str> = Cursor::default();
        assert_eq!(cursor.selected(), None);

        cursor.replace(vec!["a", "b"], |_| false);
        assert_eq!(cursor.selected(), Some(0));

        cursor.replace(Vec::new(), |_| false);
        assert_eq!(cursor.selected(), None);
        assert_eq!(cursor.selected_row(), None);

        // A move on an empty list is not an error and does not invent a cursor.
        cursor.move_selection(1);
        assert_eq!(cursor.selected(), None);
    }

    /// `replace` holds by the key, not by the index. The row moves and the cursor follows it.
    #[test]
    fn the_cursor_follows_the_row_it_was_holding() {
        let mut cursor = Cursor::default();
        cursor.replace(vec!["a", "b", "c"], |_| false);
        cursor.move_selection(2);
        assert_eq!(cursor.selected_row(), Some(&"c"));

        // Same row, different index.
        cursor.replace(vec!["c", "a", "b"], |row| *row == "c");
        assert_eq!(cursor.selected(), Some(0));

        // The held row is gone, so the cursor falls back to the top.
        cursor.replace(vec!["a", "b"], |row| *row == "c");
        assert_eq!(cursor.selected(), Some(0));
    }

    /// The cursor stops at both ends. One body now, where there were three.
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

    /// The rows are reachable as a slice, which is how the renderer reads them.
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
