//! Where things go, in rows.
//!
//! Vertical placement is the one piece of geometry the three list screens genuinely share.
//! `render_search`, `render_dirs` and `render_agents` each computed it inline, from the same
//! four expressions, and `render_dirs`' doc comment names the reason they must agree: `Tab`
//! swaps the *contents* of a screen rather than the screen, so a row that moves on one screen
//! and not the others turns a filter into a jump.
//!
//! Three copies of an invariant is three places to break it. One copy, with the arithmetic
//! named, is also the only form of it that can be tested — the horizontal picture is assembled
//! by the *host* from a serialized `Table`, so nothing in this crate can see it, but the row a
//! thing lands on is decided here.

/// The rows a list screen is cut into, top to bottom.
///
/// Everything is derived from two numbers: how tall the pane is, and how many note lines the
/// screen has to say this frame. Notes are the only variable-height element — they appear and
/// vanish on a keystroke — and they are paid for out of the list's rows rather than by moving
/// anything below them.
pub struct Screen {
    /// The prompt — the row you are typing into.
    pub prompt_y: usize,
    /// The first row of the results table.
    ///
    /// Directly under the prompt with no blank row of its own: the table's own title row is
    /// blank and supplies the gap. This is the built-in session manager's spacing exactly — an
    /// extra blank here pushes the list one row further from the thing that filters it.
    pub table_y: usize,
    /// How many rows the table may occupy, title row included.
    pub list_rows: usize,
    /// The first note row.
    pub notes_y: usize,
    /// The help line — always the pane's last row.
    pub help_y: usize,
}

impl Screen {
    pub fn new(rows: usize, notes: usize) -> Self {
        // A blank row above the prompt, but only once there is a pane tall enough to spend one
        // on. Below that every row is load-bearing.
        let prompt_y = if rows > 4 { 1 } else { 0 };
        let table_y = prompt_y + 1;
        // The bottom rows are spoken for before the list gets to ask: the help line, a blank
        // row above it, and a note line each. Taking them off the top of the budget is what
        // keeps the list from growing into them.
        let list_rows = rows.saturating_sub(notes + 2).saturating_sub(table_y);
        let notes_y = rows.saturating_sub(1).saturating_sub(notes);
        let help_y = rows.saturating_sub(1);
        Self { prompt_y, table_y, list_rows, notes_y, help_y }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ordinary pane: blank row, prompt, table, blank, help.
    #[test]
    fn tall_pane_without_notes() {
        let screen = Screen::new(30, 0);
        assert_eq!(screen.prompt_y, 1);
        assert_eq!(screen.table_y, 2);
        assert_eq!(screen.help_y, 29);
        // 2..=27 is the table, 28 is the blank row the help line stands on top of.
        assert_eq!(screen.list_rows, 26);
        assert_eq!(screen.table_y + screen.list_rows, 28);
    }

    /// A note is paid for by the list, not by moving the help line.
    #[test]
    fn notes_come_out_of_the_list() {
        let bare = Screen::new(30, 0);
        for notes in 1..=2 {
            let screen = Screen::new(30, notes);
            assert_eq!(screen.help_y, bare.help_y);
            assert_eq!(screen.prompt_y, bare.prompt_y);
            assert_eq!(screen.table_y, bare.table_y);
            assert_eq!(screen.list_rows, bare.list_rows - notes);
        }
    }

    /// Notes stack upwards from just above the help line, and never collide with it.
    #[test]
    fn notes_sit_above_the_help_line() {
        let screen = Screen::new(30, 2);
        assert_eq!(screen.notes_y, 27);
        assert_eq!(screen.notes_y + 2, screen.help_y);
        assert!(screen.table_y + screen.list_rows <= screen.notes_y);
    }

    /// At five rows the blank row above the prompt is still affordable; at four it is not.
    #[test]
    fn the_leading_blank_row_is_the_first_thing_dropped() {
        assert_eq!(Screen::new(5, 0).prompt_y, 1);
        assert_eq!(Screen::new(4, 0).prompt_y, 0);
        assert_eq!(Screen::new(1, 0).prompt_y, 0);
    }

    /// A pane too small to hold a list gives it no rows — the caller's `list_rows == 0` branch
    /// keeps the prompt, which still answers "what am I typing?". Nothing here may underflow,
    /// and the list never runs past the note rows it was budgeted against.
    #[test]
    fn tiny_panes_starve_the_list_rather_than_wrapping() {
        for rows in 0..=8 {
            for notes in 0..=2 {
                let screen = Screen::new(rows, notes);
                // Vacuous when there is no list — a zero-row pane puts `table_y` past its own
                // last row, and the caller's `list_rows == 0` branch draws nothing into it.
                if screen.list_rows > 0 {
                    assert!(
                        screen.table_y + screen.list_rows <= screen.notes_y,
                        "{rows} rows, {notes} notes"
                    );
                }
            }
        }
        assert_eq!(Screen::new(4, 0).list_rows, 1);
        assert_eq!(Screen::new(3, 0).list_rows, 0);
        assert_eq!(Screen::new(0, 2).list_rows, 0);
    }
}
