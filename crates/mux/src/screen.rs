//! The screen a pane's program draws on: what it wrote, read as Ghostty reads
//! it, and what it asked of its terminal, answered.

mod answering;

use std::num::NonZeroU16;

use libghostty_vt::render::{CellIteration, CellIterator, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::terminal::Mode;
use libghostty_vt::{Error, RenderState, TerminalOptions};

use answering::Answering;

/// What luneta's server calls itself where a program asks its terminal's name
/// and version (XTVERSION).
const NAME: &str = concat!("luneta-mux ", env!("CARGO_PKG_VERSION"));

/// The most scrollback a screen keeps, in bytes: Ghostty's own default.
pub const SCROLLBACK_BYTES_MAX: usize = 50_000_000;

/// What a line shows from a column on: nothing, or a grapheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// One column that nothing was written to.
    Blank,
    /// A grapheme of one column, as UTF-8.
    Narrow(String),
    /// A grapheme of two columns, its own and the one after it, as UTF-8.
    Wide(String),
}

impl Glyph {
    /// How many columns the glyph takes.
    #[must_use]
    pub fn columns(&self) -> usize {
        match self {
            Glyph::Blank | Glyph::Narrow(_) => 1,
            Glyph::Wide(_) => 2,
        }
    }
}

/// A line of the screen, glyph by glyph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    glyphs: Vec<Glyph>,
    wraps_wide: bool,
}

impl Line {
    /// The glyphs of the line, from its first column on.
    #[must_use]
    pub fn glyphs(&self) -> &[Glyph] {
        &self.glyphs
    }

    /// Whether the line's last column shows nothing because the wide glyph
    /// that did not fit in it went on to the next line.
    #[must_use]
    pub fn wraps_wide(&self) -> bool {
        self.wraps_wide
    }
}

/// A screen, and what the program asked of its terminal that it answers.
pub struct Screen {
    answering: Answering,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
}

impl Screen {
    /// A blank screen of `columns` by `rows`, which keeps up to
    /// `scrollback_bytes` of the lines that scroll off its top. It joins
    /// graphemes, as Ghostty does (mode 2027), until a program turns that off:
    /// [`Screen::clusters`] says whether it still does.
    pub fn new(
        columns: NonZeroU16,
        rows: NonZeroU16,
        scrollback_bytes: usize,
    ) -> Result<Screen, Error> {
        assert!(scrollback_bytes <= SCROLLBACK_BYTES_MAX, "{scrollback_bytes} of scrollback");
        let options = TerminalOptions {
            cols: columns.get(),
            rows: rows.get(),
            max_scrollback: scrollback_bytes,
        };
        let mut answering = Answering::new(options)?;
        let (made, answers) = answering.drive(|terminal| {
            terminal
                .on_xtversion(|_| Some(NAME))?
                .set_mode(Mode::GRAPHEME_CLUSTER, true)
                .map(|_| ())
        });
        made?;
        assert!(answers.is_empty(), "a new screen answered {answers:?}");
        Ok(Screen {
            answering,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
        })
    }

    /// Reads `bytes`, as the program wrote them, and gives back what the
    /// screen answered it, to be written back to it.
    #[must_use]
    pub fn write(&mut self, bytes: &[u8]) -> Vec<u8> {
        let ((), answers) = self.answering.drive(|terminal| terminal.vt_write(bytes));
        answers
    }

    /// Whether the screen joins graphemes (mode 2027), which the widths of
    /// its glyphs follow.
    pub fn clusters(&self) -> Result<bool, Error> {
        self.answering.terminal().mode(Mode::GRAPHEME_CLUSTER)
    }

    /// The lines the screen shows.
    pub fn lines(&mut self) -> Result<Vec<Line>, Error> {
        let terminal = self.answering.terminal();
        let column_count = usize::from(terminal.cols()?);
        let row_count = usize::from(terminal.rows()?);
        let snapshot = self.render.update(terminal)?;
        let mut rows = self.rows.update(&snapshot)?;
        let mut lines = Vec::with_capacity(row_count);
        while let Some(row) = rows.next() {
            assert!(lines.len() < row_count, "more lines than the screen's {row_count}");
            lines.push(line(&mut self.cells.update(row)?, column_count)?);
        }
        assert_eq!(lines.len(), row_count);
        Ok(lines)
    }
}

/// The line of `column_count` columns that Ghostty holds in `cells`, where a
/// wide cell is followed by the spacer of its second column, and only the last
/// column may be the padding a wide cell that went on left.
fn line(cells: &mut CellIteration<'static, '_>, column_count: usize) -> Result<Line, Error> {
    let mut glyphs = Vec::with_capacity(column_count);
    let mut column = 0;
    let mut wraps_wide = false;
    let mut spacer_owed = false;
    while let Some(cell) = cells.next() {
        assert!(column < column_count, "more cells than {column_count}");
        let mut text = String::new();
        cell.graphemes_utf8(&mut text)?;
        let wide = cell.raw_cell()?.wide()?;
        assert_eq!(spacer_owed, wide == CellWide::SpacerTail, "{wide:?} in column {column}");
        spacer_owed = wide == CellWide::Wide;
        match wide {
            CellWide::Narrow if text.is_empty() => glyphs.push(Glyph::Blank),
            CellWide::Narrow => glyphs.push(Glyph::Narrow(text)),
            CellWide::Wide => {
                assert!(!text.is_empty(), "a wide cell that shows nothing");
                glyphs.push(Glyph::Wide(text));
            }
            CellWide::SpacerTail => assert!(text.is_empty(), "a spacer that shows {text:?}"),
            CellWide::SpacerHead => {
                assert_eq!(column + 1, column_count, "padding in column {column}");
                assert!(text.is_empty(), "padding that shows {text:?}");
                wraps_wide = true;
            }
        }
        column += 1;
    }
    assert_eq!(column, column_count);
    let line = Line { glyphs, wraps_wide };
    assert_eq!(
        line.glyphs.iter().map(Glyph::columns).sum::<usize>() + usize::from(line.wraps_wide),
        column_count,
    );
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A screen of `columns` by `rows`, with no scrollback.
    fn screen(columns: u16, rows: u16) -> Screen {
        Screen::new(NonZeroU16::new(columns).unwrap(), NonZeroU16::new(rows).unwrap(), 0).unwrap()
    }

    /// The lines of a screen of 10 by 2 that read `bytes`, which ask it
    /// nothing.
    fn read(bytes: &str) -> Vec<Line> {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(bytes.as_bytes()), b"");
        screen.lines().unwrap()
    }

    /// The first glyphs of the first line of a screen of 10 by 2 that read
    /// `bytes`.
    fn first(bytes: &str, glyphs: usize) -> Vec<Glyph> {
        read(bytes)[0].glyphs()[..glyphs].to_vec()
    }

    fn narrow(text: &str) -> Glyph {
        Glyph::Narrow(text.to_owned())
    }

    fn wide(text: &str) -> Glyph {
        Glyph::Wide(text.to_owned())
    }

    /// A skin tone joins the emoji before it, in one glyph of two columns, as
    /// Ghostty draws it, and the next character goes after both.
    #[test]
    fn a_skin_tone_is_one_wide_glyph_with_its_emoji() {
        assert_eq!(first("👍🏽x", 2), [wide("👍🏽"), narrow("x")]);
    }

    /// Emoji joined by zero-width joiners are one grapheme, of two columns.
    #[test]
    fn a_family_of_emoji_is_one_wide_glyph() {
        assert_eq!(first("👨\u{200d}👩\u{200d}👧x", 2), [wide("👨\u{200d}👩\u{200d}👧"), narrow("x")]);
    }

    /// A combining mark goes on the letter before it.
    #[test]
    fn a_combining_mark_is_in_the_glyph_of_its_letter() {
        assert_eq!(first("e\u{301}x", 2), [narrow("e\u{301}"), narrow("x")]);
    }

    /// A space the program wrote shows, where a column nothing was written
    /// to is blank.
    #[test]
    fn a_written_space_is_not_blank() {
        assert_eq!(first("a b", 4), [narrow("a"), narrow(" "), narrow("b"), Glyph::Blank]);
    }

    /// A wide glyph that reaches the last column goes on to the next line,
    /// and leaves that column showing nothing.
    #[test]
    fn a_wide_glyph_too_long_for_its_line_wraps() {
        let lines = read("123456789👍");
        assert!(lines[0].wraps_wide());
        assert_eq!(lines[0].glyphs().len(), 9);
        assert_eq!(lines[1].glyphs()[0], wide("👍"));
    }

    /// A program that turns off joining graphemes gets an emoji and its skin
    /// tone as two glyphs, and the screen says it no longer joins them.
    #[test]
    fn a_program_may_turn_off_joining_graphemes() {
        let mut screen = screen(10, 2);
        assert!(screen.clusters().unwrap());
        assert_eq!(screen.write("\x1b[?2027l👍🏽".as_bytes()), b"");
        assert!(!screen.clusters().unwrap());
        assert_eq!(screen.lines().unwrap()[0].glyphs()[..2], [wide("👍"), wide("🏽")]);
    }

    /// A program that asks its terminal's name is told luneta's server's, by
    /// the write that asked it, and by no later one.
    #[test]
    fn a_program_that_asks_the_terminals_name_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(screen.write(b"\x1b[>q")).unwrap();
        assert_eq!(answer, format!("\x1bP>|{NAME}\x1b\\"));
        assert!(screen.write(b"x").is_empty(), "an answer is given once");
    }
}
