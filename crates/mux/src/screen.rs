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
}

/// A screen, and what the program asked of its terminal that it answers.
pub struct Screen {
    terminal: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    /// What the screen answered the program since it was last taken: the
    /// bytes its terminal writes back to it.
    answers: Rc<RefCell<Vec<u8>>>,
}

impl Screen {
    /// A blank screen of `columns` by `lines`, which keeps `scrollback` lines
    /// that scroll off its top. It joins graphemes, as Ghostty does (mode
    /// 2027), which a program may turn off.
    pub fn new(columns: u16, lines: u16, scrollback: usize) -> Result<Screen, Error> {
        assert!(columns > 0 && lines > 0, "a screen of {columns} by {lines}");
        let options = TerminalOptions { cols: columns, rows: lines, max_scrollback: scrollback };
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

    /// Reads `bytes`, as the program wrote them.
    pub fn write(&mut self, bytes: &[u8]) {
        self.terminal.vt_write(bytes);
    }

    /// What the screen answered the program since this was last asked, to be
    /// written back to it.
    #[must_use]
    pub fn answers(&self) -> Vec<u8> {
        self.answers.take()
    }

    /// The lines the screen shows, each cell by cell.
    pub fn lines(&mut self) -> Result<Vec<Vec<Cell>>, Error> {
        let snapshot = self.render.update(&self.terminal)?;
        let mut rows = self.rows.update(&snapshot)?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next() {
            let mut cells = self.cells.update(row)?;
            let mut line = Vec::new();
            while let Some(cell) = cells.next() {
                let mut text = String::new();
                cell.graphemes_utf8(&mut text)?;
                let width = match cell.raw_cell()?.wide()? {
                    CellWide::Wide => Width::Wide,
                    CellWide::SpacerTail | CellWide::SpacerHead => Width::Spacer,
                    CellWide::Narrow => Width::Narrow,
                };
                line.push(Cell { text, width });
            }
            lines.push(line);
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first cells of the first line of a screen of 10 by 2 that read
    /// `bytes`.
    fn first(bytes: &str, cells: usize) -> Vec<Cell> {
        let mut screen = Screen::new(10, 2, 0).unwrap();
        screen.write(bytes.as_bytes());
        let mut lines = screen.lines().unwrap();
        lines.swap_remove(0).into_iter().take(cells).collect()
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
        assert_eq!(first("e\u{301}x", 2), [cell("e\u{301}", Width::Narrow), cell("x", Width::Narrow)]);
    }

    /// A program that asks its terminal's name is told luneta's server's.
    #[test]
    fn a_program_that_asks_the_terminals_name_is_answered() {
        let mut screen = Screen::new(10, 2, 0).unwrap();
        screen.write(b"\x1b[>q");
        let answer = String::from_utf8(screen.answers()).unwrap();
        assert_eq!(answer, format!("\x1bP>|{NAME}\x1b\\"));
        assert!(screen.answers().is_empty(), "an answer is taken once");
    }
}
