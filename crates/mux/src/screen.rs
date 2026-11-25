mod answering;
mod input;

use std::cell::RefCell;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::rc::Rc;
use std::time::SystemTime;

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
pub use answering::Answers;
pub use input::{Click, Wheel};

use crate::asked::Size;
use crate::marks::{Mark, Marks};
use crate::program_status::{EndedProgramStatuses, ProgramStatuses, Snapshot};

const NAME: &str = concat!("lune-mux ", env!("CARGO_PKG_VERSION"));

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

pub const SCROLLBACK_BYTES_MAX: usize = 50_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Glyph {
    Blank,
    Narrow(String),
    Wide(String),
}

impl Glyph {
    #[must_use]
    pub fn columns(&self) -> usize {
        match self {
            Glyph::Blank | Glyph::Narrow(_) => 1,
            Glyph::Wide(_) => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub glyph: Glyph,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Here,
    Wraps,
    WrapsWide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    cells: Vec<Cell>,
    ending: Ending,
}

impl Line {
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    #[must_use]
    pub fn ending(&self) -> Ending {
        self.ending
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

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

    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }
}

pub struct Written {
    pub answers: Answers,
    pub marks: Vec<Mark>,
    pub pushed: Pushed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pushed {
    Rows(usize),
    Lost,
}

impl Pushed {
    #[must_use]
    pub fn then(self, next: Pushed) -> Pushed {
        match (self, next) {
            (Pushed::Rows(rows), Pushed::Rows(more)) => Pushed::Rows(rows + more),
            (Pushed::Lost, _) | (_, Pushed::Lost) => Pushed::Lost,
        }
    }

    #[must_use]
    pub fn moved(self, back: usize, scrollback: usize) -> usize {
        if back == 0 {
            return 0;
        }
        let moved = match self {
            Pushed::Rows(rows) => back + rows,
            Pushed::Lost => scrollback,
        };
        moved.min(scrollback)
    }
}

pub struct Screen {
    answering: Answering,
    size: Size,
    marks: Marks,
    statuses: Rc<RefCell<ProgramStatuses>>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    input: input::Encoders,
}

impl Screen {
    pub fn new(size: Size, scrollback_bytes: usize) -> Result<Screen, Error> {
        let mut answering = Answering::new(size, scrollback_bytes)?;
        let statuses = Rc::new(RefCell::new(ProgramStatuses::default()));
        let heard = Rc::clone(&statuses);
        let (made, answers) = answering.drive(|terminal| {
            terminal
                .on_xtversion(|_| Some(NAME))?
                .on_device_attributes(|_| Some(ATTRIBUTES))?
                .on_program_status(move |_, report| {
                    heard.borrow_mut().apply(report, SystemTime::now());
                })?
                .set_mode(Mode::GRAPHEME_CLUSTER, true)
                .map(|_| ())
        });
        made?;
        assert!(answers.is_empty(), "a new screen answered {answers:?}");
        Ok(Screen {
            answering,
            size,
            marks: Marks::default(),
            statuses,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            input: input::Encoders::new()?,
        })
    }

    pub fn write(&mut self, bytes: &[u8]) -> Result<Written, Error> {
        let (columns, rows) = self.size.get();
        let scanner = &mut self.marks;
        let statuses = &self.statuses;
        let (wrote, answers) = self.answering.drive(|terminal| {
            let end = ScrollbackEnd::of(terminal);
            let mut marks = Vec::new();
            scanner.write(terminal, bytes, &mut marks, || {
                statuses.borrow_mut().prompt_started();
            })?;
            if (terminal.cols()?, terminal.rows()?) != (columns, rows) {
                terminal.resize(columns, rows, 0, 0)?;
            }
            assert_eq!((terminal.cols()?, terminal.rows()?), (columns, rows));
            let pushed = end.and_then(|end| end.pushed(terminal)).unwrap_or(Pushed::Lost);
            Ok((marks, pushed))
        });
        let (marks, pushed) = wrote?;
        Ok(Written { answers, marks, pushed })
    }

    #[must_use]
    pub fn program_statuses(&self) -> Snapshot {
        self.statuses.borrow().snapshot()
    }

    #[must_use]
    pub fn program_statuses_version(&self) -> u64 {
        self.statuses.borrow().version()
    }

    pub fn program_exited(&mut self) -> EndedProgramStatuses {
        self.statuses.borrow_mut().exited()
    }

    pub fn resize(&mut self, size: Size) -> Result<Answers, Error> {
        let (columns, rows) = size.get();
        let (resized, answers) =
            self.answering.drive(|terminal| terminal.resize(columns, rows, 0, 0));
        resized?;
        self.size = size;
        Ok(answers)
    }

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

    #[must_use]
    pub fn clusters(&self) -> bool {
        self.mode(Mode::GRAPHEME_CLUSTER)
    }

    #[must_use]
    pub fn tracks_mouse(&self) -> bool {
        let terminal = self.answering.terminal();
        terminal.is_mouse_tracking().expect("a terminal says whether it tracks the mouse")
    }

    #[must_use]
    pub fn syncs(&self) -> bool {
        self.mode(Mode::SYNC_OUTPUT)
    }

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

    #[must_use]
    pub fn scrollback(&self) -> usize {
        let terminal = self.answering.terminal();
        terminal.scrollback_rows().expect("a terminal says its scrollback")
    }

    #[must_use]
    pub fn title(&self) -> Option<String> {
        let title = self.answering.terminal().title().ok()?;
        str::from_utf8(title).ok().map(str::to_owned)
    }

    #[must_use]
    pub fn pwd(&self) -> Option<String> {
        let pwd = self.answering.terminal().pwd().ok()?;
        str::from_utf8(pwd).ok().map(str::to_owned)
    }

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

    fn shown(&mut self) -> Result<View, Error> {
        let terminal = self.answering.terminal();
        let (columns, rows) = self.size.get();
        let (column_count, row_count) = (usize::from(columns), usize::from(rows));
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
            let wraps = row.raw_row()?.is_wrapped()?;
            lines.push(line(&mut self.cells.update(row)?, column_count, wraps)?);
        }
        assert_eq!(lines.len(), row_count);
        Ok(View { size: self.size, lines, cursor })
    }

    pub fn dumped_screen(&self) -> Result<String, Error> {
        let terminal = self.answering.terminal();
        let mut formatter = shown_only(terminal)?;
        formatter.set_format(Format::Vt)?.set_trim(true)?.set_extras(Extras::NONE)?;
        let bytes = formatter.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn dumped_text(&self) -> Result<String, Error> {
        let mut formatter = Formatter::new(self.answering.terminal())?;
        formatter.set_format(Format::Plain)?.set_unwrap(true)?.set_trim(true)?;
        let bytes = formatter.format_alloc(None)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    pub fn looked(&self) -> Result<Looked, Error> {
        let terminal = self.answering.terminal();
        let mut formatter = shown_only(terminal)?;
        formatter.set_format(Format::Plain)?.set_unwrap(true)?.set_trim(true)?;
        let bytes = formatter.format_alloc(None)?;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let rows = usize::from(terminal.rows()?);
        assert!(text.lines().count() <= rows, "{} lines of {rows} rows", text.lines().count());
        Ok(Looked::of(text))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub text: String,
    pub shown: u64,
}

impl Looked {
    fn of(text: String) -> Looked {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        Looked { shown: hasher.finish(), text }
    }
}

fn shown_only<'t, 'alloc, 'cb>(
    terminal: &'t Terminal<'alloc, 'cb>,
) -> Result<Formatter<'t, 'alloc, 'cb>, Error> {
    let corner = |x, y| terminal.grid_ref(Point::Active(Coordinate { x, y }));
    let last = (terminal.cols()? - 1, u32::from(terminal.rows()?) - 1);
    let shown = Selection::new(corner(0, 0)?, corner(last.0, last.1)?, false);
    let mut formatter = Formatter::new(terminal)?;
    #[expect(
        unsafe_code,
        reason = "a formatter takes a range only as a selection, unchecked. Both corners are of \
                  `terminal`'s active screen, taken from it just now, and the formatter borrows \
                  it as they do. It copies the selection, so `shown` may end before it does"
    )]
    unsafe { formatter.set_selection(Some(&shown)) }?;
    Ok(formatter)
}

struct ScrollbackEnd {
    screen: libghostty_vt::screen::Screen,
    last: Option<TrackedGridRef>,
}

impl ScrollbackEnd {
    fn of(terminal: &Terminal<'_, '_>) -> Result<ScrollbackEnd, Error> {
        let rows = terminal.scrollback_rows()?;
        let last = match rows.checked_sub(1) {
            Some(index) => {
                let y = u32::try_from(index).expect("a scrollback row fits a screen coordinate");
                Some(terminal.track_grid_ref(Point::Screen(Coordinate { x: 0, y }))?)
            },
            None => None,
        };
        Ok(ScrollbackEnd { screen: terminal.active_screen()?, last })
    }

    fn pushed(&self, terminal: &Terminal<'_, '_>) -> Result<Pushed, Error> {
        if terminal.active_screen()? != self.screen {
            return Ok(Pushed::Lost);
        }
        let rows = terminal.scrollback_rows()?;
        let Some(last) = &self.last else {
            return Ok(Pushed::Rows(rows));
        };
        let Some(at) = last.point(Space::Screen)? else {
            return Ok(Pushed::Lost);
        };
        let kept = usize::try_from(at.y).expect("a screen coordinate fits a usize") + 1;
        Ok(rows.checked_sub(kept).map_or(Pushed::Lost, Pushed::Rows))
    }
}

fn line(
    cells: &mut CellIteration<'static, '_>,
    column_count: usize,
    wraps: bool,
) -> Result<Line, Error> {
    let mut kept = Vec::with_capacity(column_count);
    let mut column = 0;
    let mut spacer_owed = false;
    let mut padded = false;
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
                padded = true;
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
            kept.push(Cell { glyph, style });
        }
        column += 1;
    }
    assert_eq!(column, column_count);
    assert!(wraps || !padded, "a wide glyph wraps early only out of a row that wraps");
    let ending = match (padded, wraps) {
        (true, _) => Ending::WrapsWide,
        (false, true) => Ending::Wraps,
        (false, false) => Ending::Here,
    };
    assert_eq!(
        kept.iter().map(|cell| cell.glyph.columns()).sum::<usize>()
            + usize::from(ending == Ending::WrapsWide),
        column_count,
    );
    Ok(Line { cells: kept, ending })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;

    fn size(columns: u16, rows: u16) -> Size {
        Size { columns: NonZeroU16::new(columns).unwrap(), rows: NonZeroU16::new(rows).unwrap() }
    }

    fn screen(columns: u16, rows: u16) -> Screen {
        Screen::new(size(columns, rows), 0).unwrap()
    }

    fn scrolling(columns: u16, rows: u16) -> Screen {
        Screen::new(size(columns, rows), 10_000).unwrap()
    }

    fn answered(screen: &mut Screen, bytes: &[u8]) -> Vec<u8> {
        screen.write(bytes).unwrap().answers.bytes().to_vec()
    }

    fn read(bytes: &str) -> Vec<Line> {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        screen.view(0).unwrap().lines().to_vec()
    }

    fn glyphs(line: &Line) -> Vec<Glyph> {
        line.cells().iter().map(|cell| cell.glyph.clone()).collect()
    }

    fn first(bytes: &str, count: usize) -> Vec<Glyph> {
        glyphs(&read(bytes)[0])[..count].to_vec()
    }

    fn narrow(text: &str) -> Glyph {
        Glyph::Narrow(text.to_owned())
    }

    fn wide(text: &str) -> Glyph {
        Glyph::Wide(text.to_owned())
    }

    #[test]
    fn a_skin_tone_is_one_wide_glyph_with_its_emoji() {
        assert_eq!(first("👍🏽x", 2), [wide("👍🏽"), narrow("x")]);
    }

    #[test]
    fn a_family_of_emoji_is_one_wide_glyph() {
        assert_eq!(first("👨\u{200d}👩\u{200d}👧x", 2), [wide("👨\u{200d}👩\u{200d}👧"), narrow("x")]);
    }

    #[test]
    fn a_combining_mark_is_in_the_glyph_of_its_letter() {
        assert_eq!(first("e\u{301}x", 2), [narrow("e\u{301}"), narrow("x")]);
    }

    #[test]
    fn a_written_space_is_not_blank() {
        assert_eq!(first("a b", 4), [narrow("a"), narrow(" "), narrow("b"), Glyph::Blank]);
    }

    #[test]
    fn a_wide_glyph_too_long_for_its_line_wraps() {
        let lines = read("123456789👍");
        assert_eq!(lines[0].ending(), Ending::WrapsWide);
        assert_eq!(lines[1].ending(), Ending::Here);
        assert_eq!(lines[0].cells().len(), 9);
        assert_eq!(lines[1].cells()[0].glyph, wide("👍"));
    }

    #[test]
    fn a_row_wraps_where_its_text_goes_on_and_not_where_a_newline_ends_it() {
        let lines = read("0123456789ab");
        assert_eq!(lines[0].ending(), Ending::Wraps);
        assert_eq!(lines[1].ending(), Ending::Here);
        let lines = read("0123456789\r\nab");
        let ended = lines[0].ending();
        assert_eq!(ended, Ending::Here, "text that fills its row and then ends does not wrap");
    }

    #[test]
    fn a_program_may_turn_off_joining_graphemes() {
        let mut screen = screen(10, 2);
        assert!(screen.clusters());
        assert_eq!(answered(&mut screen, "\x1b[?2027l👍🏽".as_bytes()), b"");
        assert!(!screen.clusters());
        assert_eq!(glyphs(&screen.view(0).unwrap().lines()[0])[..2], [wide("👍"), wide("🏽")]);
    }

    #[test]
    fn a_program_that_asks_the_terminals_name_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(answered(&mut screen, b"\x1b[>q")).unwrap();
        assert_eq!(answer, format!("\x1bP>|{NAME}\x1b\\"));
        assert!(answered(&mut screen, b"x").is_empty(), "an answer is given once");
    }

    #[test]
    fn a_screen_stops_holding_what_its_program_draws_as_told() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b[?2026h"), b"");
        assert!(screen.syncs());
        screen.unsync().unwrap();
        assert!(!screen.syncs());
    }

    #[test]
    fn a_program_that_asks_the_terminals_attributes_is_answered() {
        let mut screen = screen(10, 2);
        let answer = String::from_utf8(answered(&mut screen, b"\x1b[c")).unwrap();
        assert_eq!(answer, "\x1b[?62;1;6;22;52c");
    }

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

    #[test]
    fn a_resize_answers_a_program_that_asked_to_be_told() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b[?2048h"), b"");
        let answer = screen.resize(size(20, 5));
        assert_eq!(answer.unwrap().bytes(), b"\x1b[48;5;20;0;0t");
        assert_eq!(screen.size(), size(20, 5));
    }

    #[test]
    fn a_pwd_that_is_not_utf_8_is_none() {
        let mut screen = screen(10, 2);
        assert_eq!(answered(&mut screen, b"\x1b]0;top\x07\x1b]7;file://h/\xfe\x07"), b"");
        assert_eq!(screen.title().as_deref(), Some("top"));
        assert_eq!(screen.pwd(), None);
    }

    #[test]
    fn the_dumps_have_the_text_and_the_screen() {
        let mut screen = scrolling(5, 2);
        assert_eq!(answered(&mut screen, b"one\r\ntwo\r\n\x1b[31mred\x1b[0m"), b"");
        assert_eq!(screen.dumped_text().unwrap(), "one\ntwo\nred");
        let dumped = screen.dumped_screen().unwrap();
        assert_eq!(dumped, "two\r\n\x1b[0m\x1b[38;5;1mred\x1b[0m", "{dumped:?}");
    }

    #[test]
    fn a_repeat_after_a_variation_selector_in_margins_is_read() {
        let mut screen = screen(6, 1);
        let bytes = "\x1b[?7l\x1b[?69h👍\u{fe0e}\x1b[?1049l\x1b[3b\x1b[2;5s\x1b[3b\u{fe0e}";
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        assert_eq!(screen.dumped_text().unwrap(), "👍👍👍\u{fe0e}");
    }

    #[test]
    fn a_joiner_after_a_repeat_in_margins_is_read() {
        let mut screen = screen(5, 2);
        let bytes = "\x1b[?69h\x1b[1;2r中\x1b[3;9s\x1b[3b☺\u{200d}👍";
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        assert_eq!(screen.dumped_text().unwrap(), "中中   ☺\u{200d}👍");
    }

    #[test]
    fn a_program_does_not_resize_its_screen() {
        let mut screen = screen(10, 2);
        let edge = b"\x1b[999C\x1b[6n";
        assert_eq!(answered(&mut screen, b"ab\x1b[?40h\x1b[?3h"), b"");
        assert_eq!(answered(&mut screen, edge), b"\x1b[1;10R");
        let view = screen.view(0).unwrap();
        assert!(view.lines().iter().all(|line| line.cells().len() == 10));
        assert_eq!(glyphs(&view.lines()[0])[0], Glyph::Blank);
        assert_eq!(answered(&mut screen, b"\x1b[?3hcd"), b"");
        let view = screen.view(0).unwrap();
        assert_eq!(glyphs(&view.lines()[0])[..3], [narrow("c"), narrow("d"), Glyph::Blank]);
        assert_eq!(view.cursor().map(|cursor| (cursor.row, cursor.column)), Some((0, 2)));
        assert_eq!(answered(&mut screen, b"\x1b[?3l"), b"");
        assert_eq!(answered(&mut screen, edge), b"\x1b[1;10R");
    }

    #[test]
    fn a_write_says_the_rows_it_pushed_into_the_scrollback() {
        let mut screen = scrolling(10, 2);
        assert_eq!(screen.write(b"1\r\n2\r\n3\r\n4").unwrap().pushed, Pushed::Rows(2));
        let pruned = (0..100_000).any(|_| {
            let before = screen.scrollback();
            assert_eq!(screen.write(b"\r\nx").unwrap().pushed, Pushed::Rows(1));
            screen.scrollback() < before
        });
        assert!(pruned, "the scrollback let go of no rows");
        let past = "\r\nx".repeat(2 * screen.scrollback());
        assert_eq!(screen.write(past.as_bytes()).unwrap().pushed, Pushed::Lost);
        assert_eq!(screen.write(b"\x1b[3J").unwrap().pushed, Pushed::Lost);
    }

    #[test]
    fn a_write_to_the_other_screen_loses_the_rows() {
        let mut screen = scrolling(10, 2);
        assert_eq!(screen.write(b"1\r\n2\r\n3\x1b[?1049h").unwrap().pushed, Pushed::Lost);
        assert_eq!(screen.write(b"a\r\nb\r\nc").unwrap().pushed, Pushed::Rows(0));
    }

    #[test]
    fn a_view_back_stays_on_its_rows_or_goes_to_the_top() {
        assert_eq!(Pushed::Rows(3).moved(2, 10), 5);
        assert_eq!(Pushed::Rows(3).moved(9, 10), 10);
        assert_eq!(Pushed::Lost.moved(2, 10), 10);
        assert_eq!([Pushed::Rows(3).moved(0, 10), Pushed::Lost.moved(0, 10)], [0, 0]);
        assert_eq!(Pushed::Rows(1).then(Pushed::Rows(2)), Pushed::Rows(3));
        assert_eq!(Pushed::Rows(1).then(Pushed::Lost), Pushed::Lost);
        assert_eq!(Pushed::Lost.then(Pushed::Rows(2)), Pushed::Lost);
    }

    #[test]
    fn a_write_gives_back_the_marks_it_ends() {
        let mut screen = screen(10, 2);
        assert_eq!(screen.write(b"\x1b]133;A\x07$ \x1b]133;D").unwrap().marks, [Mark::Prompt]);
        assert_eq!(screen.write(b";1\x07").unwrap().marks, [Mark::Done(Some(1))]);
    }

    #[test]
    fn an_osc_7501_probe_is_answered_before_da1() {
        let mut screen = screen(10, 2);
        let reply = String::from_utf8(answered(&mut screen, b"\x1b]7501;?\x1b\\\x1b[c")).unwrap();
        let probe = reply.find("\x1b]7501;?").expect("the terminal answers the OSC 7501 probe");
        let da1 = reply.find("\x1b[?62;").expect("the terminal answers DA1");
        assert!(probe < da1, "probe reply must precede DA1 reply: {reply:?}");
    }

    #[test]
    fn osc_7501_keeps_records_inherits_app_sanitizes_and_clears_subtrees() {
        use crate::program_status::{OscBlockedKind, ProgramPhase};

        let mut screen = screen(10, 2);
        let msg = "YWxsb3cg4oCuZGFuZ2Vy";
        let title = "c3Vi4oCLdGFzaw==";
        let bytes = format!(
            "\x1b]7501;state=blocked:kind=permission:app=claude-code:msg={msg}\x1b\\\
             \x1b]7501;state=working:id=agent-1:title={title}:progress=42\x1b\\\
             \x1b]7501;state=working:id=agent-1/step\x1b\\\
             \x1b]7501;state=done:id=agent-10\x1b\\"
        );
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        let snapshot = screen.program_statuses();
        let root = snapshot.root.as_ref().unwrap();
        assert_eq!(
            root.phase,
            ProgramPhase::Blocked {
                kind: Some(OscBlockedKind::Permission),
                message: Some(crate::program_status::Message::parse("message", "allow danger").unwrap()),
            }
        );
        assert_eq!(root.app.as_ref().map(crate::program_status::AppName::as_str), Some("claude-code"));
        assert_eq!(snapshot.children.len(), 3);
        assert_eq!(snapshot.children[0].id.as_str(), "agent-1");
        assert_eq!(
            snapshot.children[0].status.app.as_ref().map(crate::program_status::AppName::as_str),
            Some("claude-code")
        );
        assert_eq!(
            snapshot.children[0].status.title.as_ref().map(crate::program_status::Title::as_str),
            Some("subtask")
        );
        assert_eq!(
            snapshot.children[0].status.phase,
            ProgramPhase::Working { progress: crate::program_status::Progress::new(42) }
        );
        assert_eq!(snapshot.children[1].id.as_str(), "agent-1/step");
        assert_eq!(
            snapshot.children[1].status.app.as_ref().map(crate::program_status::AppName::as_str),
            Some("claude-code")
        );
        assert_eq!(snapshot.children[2].id.as_str(), "agent-10");
        assert_eq!(
            snapshot.children[2].status.app.as_ref().map(crate::program_status::AppName::as_str),
            Some("claude-code")
        );

        assert_eq!(answered(&mut screen, b"\x1b]7501;state=clear:id=agent-1\x07"), b"");
        let snapshot = screen.program_statuses();
        assert!(snapshot.root.is_some());
        let ids: Vec<&str> = snapshot.children.iter().map(|it| it.id.as_str()).collect();
        assert_eq!(ids, ["agent-10"]);

        assert_eq!(answered(&mut screen, b"\x1b]7501;state=done\x07"), b"");
        let snapshot = screen.program_statuses();
        let root = snapshot.root.as_ref().unwrap();
        assert_eq!(root.phase, ProgramPhase::Done { message: None });
        assert_eq!(root.app, None);

        assert_eq!(answered(&mut screen, b"\x1b]7501;state=clear\x07"), b"");
        assert!(screen.program_statuses().is_empty());
    }

    #[test]
    fn osc_133_a_drops_working_and_blocked_in_byte_order_and_keeps_done_and_error() {
        use crate::program_status::{ProgramPhase, TerminalPhase};

        let mut screen = screen(10, 2);
        let bytes = concat!(
            "\x1b]7501;state=working:app=claude-code\x07",
            "\x1b]7501;state=done:id=finished\x07",
            "\x1b]7501;state=error:id=failed\x07",
            "\x1b]133;A\x07",
            "\x1b]7501;state=working:app=claude-code:id=next\x07",
        );
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        let snapshot = screen.program_statuses();
        assert_eq!(snapshot.root, None);
        let kept: Vec<(&str, &ProgramPhase)> =
            snapshot.children.iter().map(|it| (it.id.as_str(), &it.status.phase)).collect();
        assert_eq!(
            kept,
            [
                ("failed", &ProgramPhase::Error { message: None }),
                ("finished", &ProgramPhase::Done { message: None }),
                ("next", &ProgramPhase::Working { progress: None }),
            ]
        );

        assert_eq!(answered(&mut screen, b"\x1b]133;A\x07"), b"");
        let kept: Vec<String> =
            screen.program_statuses().children.into_iter().map(|it| it.id.into()).collect();
        assert_eq!(kept, ["failed", "finished"]);

        assert_eq!(answered(&mut screen, b"\x1b]7501;state=idle:app=pi\x07"), b"");
        let ended = screen.program_exited();
        assert_eq!(ended.root, None);
        let ended_ids: Vec<(&str, &TerminalPhase)> =
            ended.children.iter().map(|it| (it.id.as_str(), &it.status.phase)).collect();
        assert_eq!(
            ended_ids,
            [
                ("failed", &TerminalPhase::Error { message: None }),
                ("finished", &TerminalPhase::Done { message: None }),
            ]
        );

        assert_eq!(answered(&mut screen, b"\x1bc"), b"");
        assert!(screen.program_statuses().is_empty());
    }

    #[test]
    fn osc_7501_evicts_oldest_child_without_evicting_root() {
        use std::fmt::Write as _;

        let mut screen = screen(10, 2);
        let mut bytes = String::from("\x1b]7501;state=working:app=claude-code\x07");
        for i in 0..=crate::program_status::RECORDS_MAX {
            let _ = write!(bytes, "\x1b]7501;state=working:id=r{i:03}\x07");
        }
        assert_eq!(answered(&mut screen, bytes.as_bytes()), b"");
        let snapshot = screen.program_statuses();
        assert_eq!(
            snapshot
                .root
                .as_ref()
                .and_then(|r| r.app.as_ref())
                .map(crate::program_status::AppName::as_str),
            Some("claude-code"),
            "subagent eviction must never evict the root record"
        );
        let ids: Vec<String> = snapshot.children.iter().map(|it| it.id.as_str().to_owned()).collect();
        assert_eq!(ids.len(), crate::program_status::RECORDS_MAX);
        assert!(!ids.contains(&"r000".to_owned()));
        assert!(ids.contains(&"r001".to_owned()));
        assert!(ids.contains(&format!("r{:03}", crate::program_status::RECORDS_MAX)));
        assert_eq!(
            snapshot.children[0].status.app.as_ref().map(crate::program_status::AppName::as_str),
            Some("claude-code"),
            "children still inherit app from the retained root record"
        );
    }

    #[test]
    fn osc_7501_bounds_id_depth_and_keeps_version_steady_on_plain_output() {
        let mut screen = screen(10, 2);
        assert_eq!(screen.program_statuses_version(), 0);
        assert_eq!(answered(&mut screen, b"plain output\r\n"), b"");
        assert_eq!(screen.program_statuses_version(), 0);

        let too_deep = (0..=crate::program_status::ID_DEPTH_MAX)
            .map(|index| format!("s{index}"))
            .collect::<Vec<_>>()
            .join("/");
        let sequence = format!("\x1b]7501;state=working:id={too_deep}\x07");
        assert_eq!(answered(&mut screen, sequence.as_bytes()), b"");
        assert!(screen.program_statuses().is_empty());
        assert_eq!(screen.program_statuses_version(), 0);

        assert_eq!(answered(&mut screen, b"\x1b]7501;state=working:app=pi\x07"), b"");
        let after_working = screen.program_statuses_version();
        assert!(after_working > 0);
        assert_eq!(answered(&mut screen, b"more plain output\r\n"), b"");
        assert_eq!(screen.program_statuses_version(), after_working);
    }
}
