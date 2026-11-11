//! The screen a pane's program draws on: what it wrote, read as Ghostty reads
//! it, and what it asked of its terminal, answered.

use std::cell::RefCell;
use std::rc::Rc;

use libghostty_vt::render::{CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::terminal::Mode;
use libghostty_vt::{Error, RenderState, Terminal, TerminalOptions};

/// What luneta's server calls itself where a program asks its terminal's name
/// and version (XTVERSION).
const NAME: &str = concat!("luneta-mux ", env!("CARGO_PKG_VERSION"));

/// The most scrollback a screen keeps, in bytes: Ghostty's own default.
pub const SCROLLBACK_BYTES_MAX: usize = 50_000_000;

/// A cell of the screen: the grapheme it shows, as UTF-8, and how wide it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub text: String,
    pub width: Width,
}

/// How much of a line a cell takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// One column.
    Narrow,
    /// Two columns: this one and the spacer after it.
    Wide,
    /// The second column of the wide cell before it, which shows nothing of
    /// its own.
    Spacer,
    /// The last column of a line that a wide cell did not fit in, which went on
    /// to the next line instead. It shows nothing.
    Padding,
}

/// A screen, and what the program asked of its terminal that it answers.
pub struct Screen {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    /// What the screen answered the program during the write it reads, the
    /// bytes its terminal writes back to it.
    answers: Rc<RefCell<Vec<u8>>>,
}

impl Screen {
    /// A blank screen of `columns` by `rows`, which keeps up to
    /// `scrollback_bytes` of the lines that scroll off its top. It joins
    /// graphemes, as Ghostty does (mode 2027), which a program may turn off.
    pub fn new(columns: u16, rows: u16, scrollback_bytes: usize) -> Result<Screen, Error> {
        assert!(columns > 0, "a screen of no columns");
        assert!(rows > 0, "a screen of no rows");
        assert!(scrollback_bytes <= SCROLLBACK_BYTES_MAX, "{scrollback_bytes} of scrollback");
        let options = TerminalOptions { cols: columns, rows, max_scrollback: scrollback_bytes };
        let mut terminal = Terminal::new(options)?;
        let answers = Rc::new(RefCell::new(Vec::new()));
        let written = Rc::clone(&answers);
        terminal
            .on_pty_write(move |_, bytes| written.borrow_mut().extend_from_slice(bytes))?
            .on_xtversion(|_| Some(NAME))?
            .set_mode(Mode::GRAPHEME_CLUSTER, true)?;
        Ok(Screen {
            terminal,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            answers,
        })
    }

    /// Reads `bytes`, as the program wrote them, and gives back what the
    /// screen answered it, to be written back to it.
    #[must_use]
    pub fn write(&mut self, bytes: &[u8]) -> Vec<u8> {
        assert!(self.answers.borrow().is_empty(), "an answer outside a write");
        self.terminal.vt_write(bytes);
        self.answers.take()
    }

    /// The lines the screen shows, each cell by cell.
    pub fn lines(&mut self) -> Result<Vec<Vec<Cell>>, Error> {
        let column_count = usize::from(self.terminal.cols()?);
        let row_count = usize::from(self.terminal.rows()?);
        let snapshot = self.render.update(&self.terminal)?;
        let mut rows = self.rows.update(&snapshot)?;
        let mut lines = Vec::with_capacity(row_count);
        while let Some(row) = rows.next() {
            assert!(lines.len() < row_count, "more lines than the screen's {row_count}");
            let mut cells = self.cells.update(row)?;
            let mut line = Vec::with_capacity(column_count);
            while let Some(cell) = cells.next() {
                assert!(line.len() < column_count, "more cells than {column_count}");
                let mut text = String::new();
                cell.graphemes_utf8(&mut text)?;
                let width = match cell.raw_cell()?.wide()? {
                    CellWide::Narrow => Width::Narrow,
                    CellWide::Wide => Width::Wide,
                    CellWide::SpacerTail => Width::Spacer,
                    CellWide::SpacerHead => Width::Padding,
                };
                line.push(Cell { text, width });
            }
            assert_eq!(line.len(), column_count);
            lines_assert_spacers(&line);
            lines.push(line);
        }
        assert_eq!(lines.len(), row_count);
        Ok(lines)
    }
}

/// What Ghostty holds of the cells of a line: a spacer is right after the
/// wide cell it is the second column of, padding is only in the last column,
/// and neither shows anything.
fn lines_assert_spacers(line: &[Cell]) {
    for (column, cell) in line.iter().enumerate() {
        match cell.width {
            Width::Narrow | Width::Wide => {}
            Width::Spacer => {
                assert!(column > 0, "a spacer in the first column");
                assert_eq!(line[column - 1].width, Width::Wide, "a spacer in column {column}");
                assert!(cell.text.is_empty(), "a spacer that shows {:?}", cell.text);
            }
            Width::Padding => {
                assert_eq!(column + 1, line.len(), "padding in column {column}");
                assert!(cell.text.is_empty(), "padding that shows {:?}", cell.text);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of a screen of 10 by 2 that read `bytes`, which ask it
    /// nothing.
    fn read(bytes: &str) -> Vec<Vec<Cell>> {
        let mut screen = Screen::new(10, 2, 0).unwrap();
        assert_eq!(screen.write(bytes.as_bytes()), b"");
        screen.lines().unwrap()
    }

    /// The first cells of the first line of a screen of 10 by 2 that read
    /// `bytes`.
    fn first(bytes: &str, cells: usize) -> Vec<Cell> {
        read(bytes).swap_remove(0).into_iter().take(cells).collect()
    }

    fn cell(text: &str, width: Width) -> Cell {
        Cell { text: text.to_owned(), width }
    }

    /// A skin tone joins the emoji before it, which takes two columns, as
    /// Ghostty draws it: the cell after it is left as the spacer, and the
    /// next character goes after that.
    #[test]
    fn a_skin_tone_is_one_wide_cell_with_its_emoji() {
        assert_eq!(
            first("👍🏽x", 3),
            [cell("👍🏽", Width::Wide), cell("", Width::Spacer), cell("x", Width::Narrow)],
        );
    }

    /// Emoji joined by zero-width joiners are one grapheme, of two columns.
    #[test]
    fn a_family_of_emoji_is_one_wide_cell() {
        assert_eq!(
            first("👨\u{200d}👩\u{200d}👧x", 3),
            [
                cell("👨\u{200d}👩\u{200d}👧", Width::Wide),
                cell("", Width::Spacer),
                cell("x", Width::Narrow),
            ],
        );
    }

    /// A combining mark goes on the letter before it.
    #[test]
    fn a_combining_mark_is_in_the_cell_of_its_letter() {
        assert_eq!(
            first("e\u{301}x", 2),
            [cell("e\u{301}", Width::Narrow), cell("x", Width::Narrow)],
        );
    }

    /// A wide cell that reaches the last column goes on to the next line,
    /// and leaves that column as padding.
    #[test]
    fn a_wide_cell_too_long_for_its_line_leaves_padding() {
        let lines = read("123456789👍");
        assert_eq!(lines[0][9], cell("", Width::Padding));
        assert_eq!(lines[1][..2], [cell("👍", Width::Wide), cell("", Width::Spacer)]);
    }

    /// A program that asks its terminal's name is told luneta's server's, by
    /// the write that asked it, and by no later one.
    #[test]
    fn a_program_that_asks_the_terminals_name_is_answered() {
        let mut screen = Screen::new(10, 2, 0).unwrap();
        let answer = String::from_utf8(screen.write(b"\x1b[>q")).unwrap();
        assert_eq!(answer, format!("\x1bP>|{NAME}\x1b\\"));
        assert!(screen.write(b"x").is_empty(), "an answer is given once");
    }
}
