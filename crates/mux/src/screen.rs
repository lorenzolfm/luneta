//! The screen a pane's program draws on: what it wrote, read as Ghostty reads
//! it, and what it asked of its terminal, answered.

mod answering;
mod input;

use std::num::NonZeroU16;

use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::render::{CellIteration, CellIterator, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
    PointCoordinate, PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
    TertiaryDeviceAttributes,
};
use libghostty_vt::{Error, RenderState, TerminalOptions};

pub use libghostty_vt::render::CursorVisualStyle as CursorShape;
pub use libghostty_vt::style::{RgbColor, Style, StyleColor, Underline};

use answering::Answering;
pub use input::{Click, Wheel};

/// What luneta's server calls itself where a program asks its terminal's name
/// and version (XTVERSION).
const NAME: &str = concat!("luneta-mux ", env!("CARGO_PKG_VERSION"));

/// What the server says it is where a program asks (DA1, DA2 and DA3): a
/// VT220 with colours and a clipboard, as Ghostty says of itself.
const ATTRIBUTES: DeviceAttributes = DeviceAttributes {
    primary: PrimaryDeviceAttributes::new(
        ConformanceLevel::LEVEL_2,
        &[
            DeviceAttributeFeature::COLUMNS_132,
            DeviceAttributeFeature::SELECTIVE_ERASE,
            DeviceAttributeFeature::ANSI_COLOR,
            DeviceAttributeFeature::CLIPBOARD,
        ],
    ),
    secondary: SecondaryDeviceAttributes {
        device_type: DeviceType::VT220,
        firmware_version: 0,
        rom_cartridge: 0,
    },
    tertiary: TertiaryDeviceAttributes { unit_id: 0 },
};

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

/// A glyph, in the style it shows in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub glyph: Glyph,
    pub style: Style,
}

/// A line of the screen, glyph by glyph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    cells: Vec<Cell>,
    wraps_wide: bool,
}

impl Line {
    /// The cells of the line, from its first column on.
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Whether the line's last column shows nothing because the wide glyph
    /// that did not fit in it went on to the next line.
    #[must_use]
    pub fn wraps_wide(&self) -> bool {
        self.wraps_wide
    }
}

/// Where the cursor is in a view, and how it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

/// What a screen shows, looked at from some rows back in its scrollback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub lines: Vec<Line>,
    /// None where it is hidden, or out of the view.
    pub cursor: Option<Cursor>,
}

/// A screen, and what the program asked of its terminal that it answers.
pub struct Screen {
    answering: Answering,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    input: input::Encoders,
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
                .on_device_attributes(|_| Some(ATTRIBUTES))?
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
            input: input::Encoders::new()?,
        })
    }

    /// Reads `bytes`, as the program wrote them, and gives back what the
    /// screen answered it, to be written back to it.
    #[must_use]
    pub fn write(&mut self, bytes: &[u8]) -> Vec<u8> {
        let ((), answers) = self.answering.drive(|terminal| terminal.vt_write(bytes));
        answers
    }

    /// Makes the screen `columns` by `rows`, and gives back what it answered
    /// the program as it did: its new size, where the program asked to be told
    /// (mode 2048).
    pub fn resize(&mut self, columns: NonZeroU16, rows: NonZeroU16) -> Result<Vec<u8>, Error> {
        let (resized, answers) = self
            .answering
            .drive(|terminal| terminal.resize(columns.get(), rows.get(), 0, 0));
        resized?;
        Ok(answers)
    }

    /// Has the screen answer its program's questions of colour with these,
    /// none being the terminal's own default.
    pub fn colour(
        &mut self,
        foreground: Option<RgbColor>,
        background: Option<RgbColor>,
    ) -> Result<(), Error> {
        let (set, answers) = self.answering.drive(|terminal| {
            terminal.set_default_fg_color(foreground)?.set_default_bg_color(background).map(|_| ())
        });
        assert!(answers.is_empty(), "setting colours answered {answers:?}");
        set
    }

    /// The screen's size: its columns, then its rows.
    #[must_use]
    pub fn size(&self) -> (NonZeroU16, NonZeroU16) {
        let terminal = self.answering.terminal();
        let columns = terminal.cols().expect("a terminal says its columns");
        let rows = terminal.rows().expect("a terminal says its rows");
        (
            NonZeroU16::new(columns).expect("a terminal has columns"),
            NonZeroU16::new(rows).expect("a terminal has rows"),
        )
    }

    /// Whether the screen joins graphemes (mode 2027), which the widths of
    /// its glyphs follow.
    #[must_use]
    pub fn clusters(&self) -> bool {
        self.mode(Mode::GRAPHEME_CLUSTER)
    }

    /// Whether the program takes the mouse.
    #[must_use]
    pub fn tracks_mouse(&self) -> bool {
        let terminal = self.answering.terminal();
        terminal.is_mouse_tracking().expect("a terminal says whether it tracks the mouse")
    }

    /// Whether the program has the screen hold what it draws until it is done
    /// (mode 2026).
    #[must_use]
    pub fn syncs(&self) -> bool {
        self.mode(Mode::SYNC_OUTPUT)
    }

    fn mode(&self, mode: Mode) -> bool {
        self.answering.terminal().mode(mode).expect("a terminal says its modes")
    }

    /// How many rows the scrollback has.
    #[must_use]
    pub fn scrollback(&self) -> usize {
        let terminal = self.answering.terminal();
        terminal.scrollback_rows().expect("a terminal says its scrollback")
    }

    /// The title the program last set, or nothing: none where it is not
    /// UTF-8.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        self.answering.terminal().title().ok().map(str::to_owned)
    }

    /// Where the program last said it works (OSC 7), or nothing: none where
    /// it is not UTF-8.
    #[must_use]
    pub fn pwd(&self) -> Option<String> {
        self.answering.terminal().pwd().ok().map(str::to_owned)
    }

    /// What the screen shows, looked at from `back` rows back in its
    /// scrollback, which has at least as many.
    pub fn view(&mut self, back: usize) -> Result<View, Error> {
        assert!(back <= self.scrollback(), "{back} rows back");
        if back == 0 {
            return self.shown();
        }
        let delta = isize::try_from(back).map_err(|_| Error::InvalidValue)?;
        self.scroll(ScrollViewport::Delta(-delta));
        let view = self.shown();
        self.scroll(ScrollViewport::Bottom);
        view
    }

    fn scroll(&mut self, to: ScrollViewport) {
        let ((), answers) = self.answering.drive(|terminal| terminal.scroll_viewport(to));
        assert!(answers.is_empty(), "scrolling answered {answers:?}");
    }

    /// The lines the screen shows, and its cursor.
    fn shown(&mut self) -> Result<View, Error> {
        let terminal = self.answering.terminal();
        let column_count = usize::from(terminal.cols()?);
        let row_count = usize::from(terminal.rows()?);
        let snapshot = self.render.update(terminal)?;
        let cursor = match snapshot.cursor_viewport()? {
            Some(at) if snapshot.cursor_visible()? => Some(Cursor {
                row: at.y,
                column: at.x,
                shape: snapshot.cursor_visual_style()?,
                blinking: snapshot.cursor_blinking()?,
            }),
            _ => None,
        };
        let mut rows = self.rows.update(&snapshot)?;
        let mut lines = Vec::with_capacity(row_count);
        while let Some(row) = rows.next() {
            assert!(lines.len() < row_count, "more lines than the screen's {row_count}");
            lines.push(line(&mut self.cells.update(row)?, column_count)?);
        }
        assert_eq!(lines.len(), row_count);
        Ok(View { lines, cursor })
    }

    /// The lines the screen shows, in their styles, as escape sequences.
    pub fn dumped_screen(&self) -> Result<String, Error> {
        let terminal = self.answering.terminal();
        let corner = |x, y| terminal.grid_ref(Point::Active(PointCoordinate { x, y }));
        let last = (terminal.cols()? - 1, u32::from(terminal.rows()?) - 1);
        let shown = Selection::new(corner(0, 0)?, corner(last.0, last.1)?, false);
        let options = FormatterOptions::new()
            .with_format(Format::Vt)
            .with_trim(true)
            .with_selection(&shown);
        let bytes = Formatter::new(terminal, options)?.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// All the screen was written that it keeps, its scrollback with it: a
    /// line for each line written, with no styles and no spaces at its end.
    pub fn dumped_text(&self) -> Result<String, Error> {
        let options =
            FormatterOptions::new().with_format(Format::Plain).with_unwrap(true).with_trim(true);
        let terminal = self.answering.terminal();
        let bytes = Formatter::new(terminal, options)?.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// The line of `column_count` columns that Ghostty holds in `cells`, where a
/// wide cell is followed by the spacer of its second column, and only the last
/// column may be the padding a wide cell that went on left.
fn line(cells: &mut CellIteration<'static, '_>, column_count: usize) -> Result<Line, Error> {
    let mut line = Line { cells: Vec::with_capacity(column_count), wraps_wide: false };
    let mut column = 0;
    let mut spacer_owed = false;
    while let Some(cell) = cells.next() {
        assert!(column < column_count, "more cells than {column_count}");
        let mut text = String::new();
        cell.graphemes_utf8(&mut text)?;
        let wide = cell.raw_cell()?.wide()?;
        assert_eq!(spacer_owed, wide == CellWide::SpacerTail, "{wide:?} in column {column}");
        spacer_owed = wide == CellWide::Wide;
        let glyph = match wide {
            CellWide::Narrow if text.is_empty() => Some(Glyph::Blank),
            CellWide::Narrow => Some(Glyph::Narrow(text)),
            CellWide::Wide => {
                assert!(!text.is_empty(), "a wide cell that shows nothing");
                Some(Glyph::Wide(text))
            },
            CellWide::SpacerTail => {
                assert!(text.is_empty(), "a spacer that shows {text:?}");
                None
            },
            CellWide::SpacerHead => {
                assert_eq!(column + 1, column_count, "padding in column {column}");
                assert!(text.is_empty(), "padding that shows {text:?}");
                line.wraps_wide = true;
                None
            },
        };
        if let Some(glyph) = glyph {
            let raw = cell.raw_cell()?;
            let mut style = cell.style()?;
            match raw.content_tag()? {
                CellContentTag::BgColorPalette => {
                    style.bg_color = StyleColor::Palette(raw.bg_color_palette()?);
                },
                CellContentTag::BgColorRgb => style.bg_color = StyleColor::Rgb(raw.bg_color_rgb()?),
                CellContentTag::Codepoint | CellContentTag::CodepointGrapheme => {},
            }
            line.cells.push(Cell { glyph, style });
        }
        column += 1;
    }
    assert_eq!(column, column_count);
    assert_eq!(
        line.cells.iter().map(|cell| cell.glyph.columns()).sum::<usize>()
            + usize::from(line.wraps_wide),
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

    /// A screen of `columns` by `rows`, which keeps what scrolls off its top.
    fn scrolling(columns: u16, rows: u16) -> Screen {
        let (columns, rows) = (NonZeroU16::new(columns).unwrap(), NonZeroU16::new(rows).unwrap());
        Screen::new(columns, rows, 10_000).unwrap()
    }

    /// The lines of a screen of 10 by 2 that read `bytes`, which ask it
    /// nothing.
    fn read(bytes: &str) -> Vec<Line> {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(bytes.as_bytes()), b"");
        screen.view(0).unwrap().lines
    }

    fn glyphs(line: &Line) -> Vec<Glyph> {
        line.cells().iter().map(|cell| cell.glyph.clone()).collect()
    }

    /// The first glyphs of the first line of a screen of 10 by 2 that read
    /// `bytes`.
    fn first(bytes: &str, count: usize) -> Vec<Glyph> {
        glyphs(&read(bytes)[0])[..count].to_vec()
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
        assert_eq!(lines[0].cells().len(), 9);
        assert_eq!(lines[1].cells()[0].glyph, wide("👍"));
    }

    /// A program that turns off joining graphemes gets an emoji and its skin
    /// tone as two glyphs, and the screen says it no longer joins them.
    #[test]
    fn a_program_may_turn_off_joining_graphemes() {
        let mut screen = screen(10, 2);
        assert!(screen.clusters());
        assert_eq!(screen.write("\x1b[?2027l👍🏽".as_bytes()), b"");
        assert!(!screen.clusters());
        assert_eq!(glyphs(&screen.view(0).unwrap().lines[0])[..2], [wide("👍"), wide("🏽")]);
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

    /// A program that asks what its terminal is (DA1) is told, as fish asks
    /// before it draws its first prompt.
    #[test]
    fn a_program_that_asks_the_terminals_attributes_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(screen.write(b"\x1b[c")).unwrap();
        assert_eq!(answer, "\x1b[?62;1;6;22;52c");
    }

    /// A glyph keeps the colour and the weight it was written in, and a
    /// column erased in a colour is blank in it.
    #[test]
    fn a_glyph_keeps_its_style() {
        let mut screen = screen(4, 1);
        assert_eq!(screen.write(b"\x1b[1;31ma\x1b[0;44m\x1b[K"), b"");
        let cells = screen.view(0).unwrap().lines[0].cells().to_vec();
        assert_eq!(cells[0].glyph, narrow("a"));
        assert!(cells[0].style.bold);
        assert!(matches!(cells[0].style.fg_color, StyleColor::Palette(index) if index.0 == 1));
        assert_eq!(cells[1].glyph, Glyph::Blank);
        assert!(matches!(cells[1].style.bg_color, StyleColor::Palette(index) if index.0 == 4));
    }

    /// Looked at from back in the scrollback, the screen shows the lines that
    /// went off its top, and no cursor, as that is below.
    #[test]
    fn a_view_back_in_the_scrollback_shows_older_lines() {
        let mut screen = scrolling(4, 2);
        assert_eq!(screen.write(b"1\r\n2\r\n3\r\n4"), b"");
        assert_eq!(screen.scrollback(), 2);
        let live = screen.view(0).unwrap();
        assert_eq!(glyphs(&live.lines[0])[0], narrow("3"));
        assert_eq!(live.cursor.map(|cursor| (cursor.row, cursor.column)), Some((1, 1)));
        let back = screen.view(2).unwrap();
        assert_eq!(glyphs(&back.lines[0])[0], narrow("1"));
        assert_eq!(glyphs(&back.lines[1])[0], narrow("2"));
        assert_eq!(back.cursor, None);
        assert_eq!(screen.view(0).unwrap(), live, "the view goes back to the bottom");
    }

    /// A resized screen is the new size, and tells a program that asked to be
    /// told (mode 2048).
    #[test]
    fn a_resize_answers_a_program_that_asked_to_be_told() {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(b"\x1b[?2048h"), b"");
        let answer = screen.resize(NonZeroU16::new(20).unwrap(), NonZeroU16::new(5).unwrap());
        assert_eq!(String::from_utf8(answer.unwrap()).unwrap(), "\x1b[48;5;20;0;0t");
        let size = screen.size();
        assert_eq!((size.0.get(), size.1.get()), (20, 5));
    }

    /// A folder named in what is not UTF-8 is no folder, and the title
    /// stays.
    #[test]
    fn a_pwd_that_is_not_utf_8_is_none() {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(b"\x1b]0;top\x07\x1b]7;file://h/\xfe\x07"), b"");
        assert_eq!(screen.title().as_deref(), Some("top"));
        assert_eq!(screen.pwd(), None);
    }

    /// The text dump has every line written, the scrollback's too, and none of
    /// their styles. The screen dump has the lines shown, styled.
    #[test]
    fn the_dumps_have_the_text_and_the_screen() {
        let mut screen = scrolling(5, 2);
        assert_eq!(screen.write(b"one\r\ntwo\r\n\x1b[31mred\x1b[0m"), b"");
        assert_eq!(screen.dumped_text().unwrap(), "one\ntwo\nred");
        let dumped = screen.dumped_screen().unwrap();
        assert!(dumped.contains("two"), "{dumped:?}");
        assert!(dumped.contains("\x1b[38;5;1mred"), "{dumped:?}");
        assert!(!dumped.contains("one"), "{dumped:?}");
    }
}
