//! The screen a pane's program draws on: what it wrote, read as Ghostty reads
//! it, and what it asked of its terminal, answered.

mod answering;
mod input;

use libghostty_vt::effect::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType,
    PrimaryDeviceAttributes, SecondaryDeviceAttributes, TertiaryDeviceAttributes,
};
use libghostty_vt::fmt::{Extras, Format, Formatter};
use libghostty_vt::grid_ref::TrackedGridRef;
use libghostty_vt::mode::Mode;
use libghostty_vt::point::{Coordinate, Point, Space};
use libghostty_vt::render::{CellIteration, CellIterator, RowIterator};
use libghostty_vt::screen::{CellContentTag, CellWide};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::ScrollViewport;
use libghostty_vt::{Error, RenderState, Terminal};

pub use libghostty_vt::color::Rgb as RgbColor;
pub use libghostty_vt::render::CursorVisualStyle as CursorShape;
pub use libghostty_vt::style::{Style, StyleColor, Underline};

use answering::Answering;
pub use input::{Click, Wheel};

use crate::asked::Size;
use crate::marks::{Mark, Marks};

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

/// What a screen of `size` shows, looked at from some rows back in its
/// scrollback: a line for each of its rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    size: Size,
    lines: Vec<Line>,
    cursor: Option<Cursor>,
}

impl View {
    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    /// None where it is hidden, or out of the view.
    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }
}

/// What the screen made of what its program wrote.
pub struct Written {
    /// What it answered the program, to be written back to it.
    pub answers: Vec<u8>,
    /// The marks the program's shell put in it.
    pub marks: Vec<Mark>,
    /// The rows it pushed into the scrollback: none where the rows that were
    /// there went, or the program went to its other screen.
    pub rows_pushed: Option<usize>,
}

/// A screen, and what the program asked of its terminal that it answers.
pub struct Screen {
    answering: Answering,
    /// The size it was made or last resized to, which no program changes.
    size: Size,
    marks: Marks,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    input: input::Encoders,
}

impl Screen {
    /// A blank screen of `size`, which keeps up to `scrollback_bytes` of the
    /// lines that scroll off its top. It joins graphemes, as Ghostty does
    /// (mode 2027), until a program turns that off: [`Screen::clusters`] says
    /// whether it still does.
    pub fn new(size: Size, scrollback_bytes: usize) -> Result<Screen, Error> {
        let mut answering = Answering::new(size, scrollback_bytes)?;
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
            size,
            marks: Marks::default(),
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            input: input::Encoders::new()?,
        })
    }

    /// Reads `bytes`, as the program wrote them. A program that sets 80 or
    /// 132 columns (DECCOLM) has the screen erased, as tmux does, and it
    /// keeps its size. The size comes back at the end of the write, as
    /// Ghostty's parse has no stop at a CSI: what the same write holds after
    /// the DECCOLM is drawn 80 or 132 wide and then reflowed, where a later
    /// write draws it at the screen's size. A program that asked for 132
    /// columns draws for 132 either way.
    pub fn write(&mut self, bytes: &[u8]) -> Result<Written, Error> {
        let (size, marks) = (self.size, &mut self.marks);
        let (columns, rows) = (size.columns.get(), size.rows.get());
        let (made, answers) = self.answering.drive(|terminal| {
            // The rows pushed only move a scrolled-back view: where Ghostty
            // cannot say them, the view goes to the top, as where the rows
            // it was on went, and the pane goes on.
            let before = Before::of(terminal).ok();
            let mut marked = Vec::new();
            marks.write(terminal, bytes, &mut marked)?;
            if (terminal.cols()?, terminal.rows()?) != (columns, rows) {
                terminal.resize(columns, rows, 0, 0)?;
            }
            assert_eq!((terminal.cols()?, terminal.rows()?), (columns, rows));
            let rows_pushed =
                before.and_then(|before| before.rows_pushed(terminal).ok().flatten());
            Ok((marked, rows_pushed))
        });
        let (marks, rows_pushed) = made?;
        Ok(Written { answers, marks, rows_pushed })
    }

    /// Makes the screen `size`, and gives back what it answered the program
    /// as it did: its new size, where the program asked to be told (mode
    /// 2048).
    pub fn resize(&mut self, size: Size) -> Result<Vec<u8>, Error> {
        let (resized, answers) = self
            .answering
            .drive(|terminal| terminal.resize(size.columns.get(), size.rows.get(), 0, 0));
        resized?;
        self.size = size;
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

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
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

    /// Has the screen stop holding what its program draws (mode 2026), as a
    /// terminal does once the program held it too long.
    pub fn unsync(&mut self) -> Result<(), Error> {
        let (set, answers) = self
            .answering
            .drive(|terminal| terminal.set_mode(Mode::SYNC_OUTPUT, false).map(|_| ()));
        assert!(answers.is_empty(), "turning off mode 2026 answered {answers:?}");
        set
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
        let title = self.answering.terminal().title().ok()?;
        str::from_utf8(title).ok().map(str::to_owned)
    }

    /// Where the program last said it works (OSC 7), or nothing: none where
    /// it is not UTF-8.
    #[must_use]
    pub fn pwd(&self) -> Option<String> {
        let pwd = self.answering.terminal().pwd().ok()?;
        str::from_utf8(pwd).ok().map(str::to_owned)
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
        let column_count = usize::from(self.size.columns.get());
        let row_count = usize::from(self.size.rows.get());
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
        Ok(View { size: self.size, lines, cursor })
    }

    /// The lines the screen shows, in their styles, as escape sequences.
    pub fn dumped_screen(&self) -> Result<String, Error> {
        let terminal = self.answering.terminal();
        let corner = |x, y| terminal.grid_ref(Point::Active(Coordinate { x, y }));
        let last = (terminal.cols()? - 1, u32::from(terminal.rows()?) - 1);
        let shown = Selection::new(corner(0, 0)?, corner(last.0, last.1)?, false);
        let mut formatter = Formatter::new(terminal)?;
        // Only the lines and their styles: a new formatter also writes the
        // palette ahead of them and the cursor's style and link after.
        formatter.set_format(Format::Vt)?.set_trim(true)?.set_extras(Extras::NONE)?;
        // SAFETY: both corners are of `terminal`'s active screen, taken from
        // it just now, and the formatter borrows it as they do.
        #[expect(unsafe_code, reason = "a formatter takes a range only as a selection, unchecked")]
        unsafe { formatter.set_selection(Some(&shown)) }?;
        let bytes = formatter.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// All the screen was written that it keeps, its scrollback with it: a
    /// line for each line written, with no styles and no spaces at its end.
    pub fn dumped_text(&self) -> Result<String, Error> {
        let mut formatter = Formatter::new(self.answering.terminal())?;
        formatter.set_format(Format::Plain)?.set_unwrap(true)?.set_trim(true)?;
        let bytes = formatter.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Where the scrollback ended before a write: at the row it tracks, on the
/// screen that was the program's, or none where it had no rows.
struct Before {
    screen: libghostty_vt::screen::Screen,
    last: Option<TrackedGridRef>,
}

impl Before {
    fn of(terminal: &Terminal<'_, '_>) -> Result<Before, Error> {
        let rows = terminal.scrollback_rows()?;
        let last = match rows.checked_sub(1) {
            Some(index) => {
                let y = u32::try_from(index).expect("a scrollback row fits a screen coordinate");
                Some(terminal.track_grid_ref(Point::Screen(Coordinate { x: 0, y }))?)
            },
            None => None,
        };
        Ok(Before { screen: terminal.active_screen()?, last })
    }

    /// The rows that went into the scrollback since: those after the row it
    /// tracks, which only the scrollback's pruning takes from it. None where
    /// that row is out of the scrollback, as a reflow may put it.
    fn rows_pushed(&self, terminal: &Terminal<'_, '_>) -> Result<Option<usize>, Error> {
        if terminal.active_screen()? != self.screen {
            return Ok(None);
        }
        let rows = terminal.scrollback_rows()?;
        let Some(last) = &self.last else {
            return Ok(Some(rows));
        };
        let Some(at) = last.point(Space::Screen)? else {
            return Ok(None);
        };
        let kept = usize::try_from(at.y).expect("a screen coordinate fits a usize") + 1;
        Ok(rows.checked_sub(kept))
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
    use std::num::NonZeroU16;

    use super::*;

    pub(super) fn size(columns: u16, rows: u16) -> Size {
        Size { columns: NonZeroU16::new(columns).unwrap(), rows: NonZeroU16::new(rows).unwrap() }
    }

    /// A screen of `columns` by `rows`, with no scrollback.
    fn screen(columns: u16, rows: u16) -> Screen {
        Screen::new(size(columns, rows), 0).unwrap()
    }

    /// A screen of `columns` by `rows`, which keeps what scrolls off its top.
    fn scrolling(columns: u16, rows: u16) -> Screen {
        Screen::new(size(columns, rows), 10_000).unwrap()
    }

    /// What `screen` answered the program that wrote `bytes`.
    pub(super) fn answered(screen: &mut Screen, bytes: &[u8]) -> Vec<u8> {
        screen.write(bytes).unwrap().answers
    }

    /// The lines of a screen of 10 by 2 that read `bytes`, which ask it
    /// nothing.
    fn read(bytes: &str) -> Vec<Line> {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        screen.view(0).unwrap().lines().to_vec()
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
        assert_eq!(answered(&mut screen, "\x1b[?2027l👍🏽".as_bytes()), b"");
        assert!(!screen.clusters());
        assert_eq!(glyphs(&screen.view(0).unwrap().lines()[0])[..2], [wide("👍"), wide("🏽")]);
    }

    /// A program that asks its terminal's name is told luneta's server's, by
    /// the write that asked it, and by no later one.
    #[test]
    fn a_program_that_asks_the_terminals_name_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(answered(&mut screen, b"\x1b[>q")).unwrap();
        assert_eq!(answer, format!("\x1bP>|{NAME}\x1b\\"));
        assert!(answered(&mut screen, b"x").is_empty(), "an answer is given once");
    }

    /// A screen that held what its program drew stops, as told.
    #[test]
    fn a_screen_stops_holding_what_its_program_draws_as_told() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b[?2026h"), b"");
        assert!(screen.syncs());
        screen.unsync().unwrap();
        assert!(!screen.syncs());
    }

    /// A program that asks what its terminal is (DA1) is told, as fish asks
    /// before it draws its first prompt.
    #[test]
    fn a_program_that_asks_the_terminals_attributes_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(answered(&mut screen, b"\x1b[c")).unwrap();
        assert_eq!(answer, "\x1b[?62;1;6;22;52c");
    }

    /// A glyph keeps the colour and the weight it was written in, and a
    /// column erased in a colour is blank in it.
    #[test]
    fn a_glyph_keeps_its_style() {
        let mut screen = screen(4, 1);
        assert_eq!(answered(&mut screen, b"\x1b[1;31ma\x1b[0;44m\x1b[K"), b"");
        let cells = screen.view(0).unwrap().lines()[0].cells().to_vec();
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
        assert_eq!(answered(&mut screen, b"1\r\n2\r\n3\r\n4"), b"");
        assert_eq!(screen.scrollback(), 2);
        let live = screen.view(0).unwrap();
        assert_eq!(glyphs(&live.lines()[0])[0], narrow("3"));
        assert_eq!(live.cursor().map(|cursor| (cursor.row, cursor.column)), Some((1, 1)));
        let back = screen.view(2).unwrap();
        assert_eq!(glyphs(&back.lines()[0])[0], narrow("1"));
        assert_eq!(glyphs(&back.lines()[1])[0], narrow("2"));
        assert_eq!(back.cursor(), None);
        assert_eq!(screen.view(0).unwrap(), live, "the view goes back to the bottom");
    }

    /// A resized screen is the new size, and tells a program that asked to be
    /// told (mode 2048).
    #[test]
    fn a_resize_answers_a_program_that_asked_to_be_told() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b[?2048h"), b"");
        let answer = screen.resize(size(20, 5));
        assert_eq!(String::from_utf8(answer.unwrap()).unwrap(), "\x1b[48;5;20;0;0t");
        assert_eq!(screen.size(), size(20, 5));
    }

    /// A folder named in what is not UTF-8 is no folder, and the title
    /// stays.
    #[test]
    fn a_pwd_that_is_not_utf_8_is_none() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b]0;top\x07\x1b]7;file://h/\xfe\x07"), b"");
        assert_eq!(screen.title().as_deref(), Some("top"));
        assert_eq!(screen.pwd(), None);
    }

    /// The text dump has every line written, the scrollback's too, and none of
    /// their styles. The screen dump has the lines shown, styled.
    #[test]
    fn the_dumps_have_the_text_and_the_screen() {
        let mut screen = scrolling(5, 2);
        assert_eq!(answered(&mut screen, b"one\r\ntwo\r\n\x1b[31mred\x1b[0m"), b"");
        assert_eq!(screen.dumped_text().unwrap(), "one\ntwo\nred");
        let dumped = screen.dumped_screen().unwrap();
        assert_eq!(dumped, "two\r\n\x1b[0m\x1b[38;5;1mred\x1b[0m", "{dumped:?}");
    }

    /// A repeat inside left and right margins, after a text variation
    /// selector, is read: Ghostty's print once overflowed on it (#384).
    #[test]
    fn a_repeat_after_a_variation_selector_in_margins_is_read() {
        let mut screen = screen(6, 1);
        let bytes = "\x1b[?7l\x1b[?69h👍\u{fe0e}\x1b[?1049l\x1b[3b\x1b[2;5s\x1b[3b\u{fe0e}";
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        assert_eq!(screen.dumped_text().unwrap(), "👍👍👍\u{fe0e}");
    }

    /// A joiner after a repeat inside left and right margins is read:
    /// Ghostty's print once unwrapped a null on it (#384).
    #[test]
    fn a_joiner_after_a_repeat_in_margins_is_read() {
        let mut screen = screen(5, 2);
        let bytes = "\x1b[?69h\x1b[1;2r中\x1b[3;9s\x1b[3b☺\u{200d}👍";
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        assert_eq!(screen.dumped_text().unwrap(), "中中   ☺\u{200d}👍");
    }

    /// A program that sets 132 columns (DECCOLM), as it may once it allows
    /// that (mode 40), has the screen erased, and of the size it had.
    #[test]
    fn a_program_does_not_resize_its_screen() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"ab\x1b[?40h\x1b[?3h"), b"");
        assert_eq!(screen.size(), size(10, 2));
        let view = screen.view(0).unwrap();
        assert_eq!(view.size(), size(10, 2));
        assert!(view.lines().iter().all(|line| line.cells().len() == 10));
        assert_eq!(glyphs(&view.lines()[0])[0], Glyph::Blank);
        assert_eq!(answered(&mut screen, b"\x1b[?3hcd"), b"");
        let view = screen.view(0).unwrap();
        assert_eq!(view.size(), size(10, 2));
        assert_eq!(glyphs(&view.lines()[0])[..3], [narrow("c"), narrow("d"), Glyph::Blank]);
        assert_eq!(view.cursor().map(|cursor| (cursor.row, cursor.column)), Some((0, 2)));
        assert_eq!(answered(&mut screen, b"\x1b[?3l"), b"");
        assert_eq!(screen.size(), size(10, 2));
    }

    /// A write says how many rows it pushed into the scrollback, also where
    /// the scrollback, at its most, lets go of older ones as it does.
    #[test]
    fn a_write_says_the_rows_it_pushed_into_the_scrollback() {
        let mut screen = scrolling(10, 2);
        assert_eq!(screen.write(b"1\r\n2\r\n3\r\n4").unwrap().rows_pushed, Some(2));
        // 10 kB holds some thousands of rows of 10 columns, so Ghostty lets
        // go of some well before 100,000.
        let pruned = (0..100_000).any(|_| {
            let before = screen.scrollback();
            assert_eq!(screen.write(b"\r\nx").unwrap().rows_pushed, Some(1));
            screen.scrollback() < before
        });
        assert!(pruned, "the scrollback let go of no rows");
        assert_eq!(screen.write(b"\x1b[3J").unwrap().rows_pushed, None);
    }

    /// A program that goes to its other screen pushed no rows the client
    /// may look back at.
    #[test]
    fn a_write_to_the_other_screen_pushes_none() {
        let mut screen = scrolling(10, 2);
        assert_eq!(screen.write(b"1\r\n2\r\n3\x1b[?1049h").unwrap().rows_pushed, None);
        assert_eq!(screen.write(b"a\r\nb\r\nc").unwrap().rows_pushed, Some(0));
    }

    /// The marks a shell writes come back from the write that ends them.
    #[test]
    fn a_write_gives_back_the_marks_it_ends() {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(b"\x1b]133;A\x07$ \x1b]133;D").unwrap().marks, [Mark::Prompt]);
        assert_eq!(screen.write(b";1\x07").unwrap().marks, [Mark::Done(Some(1))]);
    }
}
