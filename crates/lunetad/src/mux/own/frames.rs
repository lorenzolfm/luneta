use std::num::NonZeroU16;

use super::{api, TerminalKey};

pub const SIDE_MAX: u16 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub rows: NonZeroU16,
}

impl Size {
    #[must_use]
    pub fn within(columns: NonZeroU16, rows: NonZeroU16) -> Size {
        let max = NonZeroU16::new(SIDE_MAX).expect("the most is not 0");
        Size { columns: columns.min(max), rows: rows.min(max) }
    }

    #[must_use]
    pub fn said(self) -> api::Size {
        api::Size { columns: self.columns.get().into(), rows: self.rows.get().into() }
    }

    fn heard(what: &str, size: Option<api::Size>) -> Result<Size, String> {
        let size = size.ok_or_else(|| format!("{what} has no size"))?;
        let cells = |name: &str, count: u32| {
            let cells = u16::try_from(count).ok().filter(|cells| *cells <= SIDE_MAX);
            let cells = cells.and_then(NonZeroU16::new);
            cells.ok_or_else(|| format!("{what}'s {name} are {count}"))
        };
        Ok(Size { columns: cells("columns", size.columns)?, rows: cells("rows", size.rows)? })
    }
}

pub(super) fn colours(foreground: Option<[u8; 3]>, background: Option<[u8; 3]>) -> api::Colours {
    let rgb = |[red, green, blue]: [u8; 3]| api::Rgb {
        red: red.into(),
        green: green.into(),
        blue: blue.into(),
    };
    api::Colours { foreground: foreground.map(rgb), background: background.map(rgb) }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grapheme(String);

impl Grapheme {
    pub fn new(grapheme: String) -> Result<Grapheme, String> {
        if grapheme.is_empty() {
            return Err("a glyph has no grapheme".to_owned());
        }
        if grapheme.chars().any(char::is_control) {
            return Err(format!("the glyph {grapheme:?} has a control character"));
        }
        Ok(Grapheme(grapheme))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shows {
    Blank,
    Narrow(Grapheme),
    Wide(Grapheme),
}

impl Shows {
    #[must_use]
    pub fn columns(&self) -> u16 {
        match self {
            Shows::Blank | Shows::Narrow(_) => 1,
            Shows::Wide(_) => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub shows: Shows,
    pub style: Style,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Palette(u8),
    Rgb([u8; 3]),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an attribute a terminal sets apart from the rest, in any combination"
)]
pub struct Style {
    pub foreground: Option<Colour>,
    pub background: Option<Colour>,
    pub underline_colour: Option<Colour>,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underlined: bool,
    pub blink: bool,
    pub inverse: bool,
    pub invisible: bool,
    pub strikethrough: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Here,
    Wraps,
    WrapsWide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    glyphs: Vec<Glyph>,
    ending: Ending,
}

impl Line {
    #[must_use]
    pub fn glyphs(&self) -> &[Glyph] {
        &self.glyphs
    }

    #[must_use]
    pub fn wraps_wide(&self) -> bool {
        self.ending == Ending::WrapsWide
    }

    #[must_use]
    pub fn wraps(&self) -> bool {
        self.ending != Ending::Here
    }

    pub fn new(glyphs: Vec<Glyph>, ending: Ending, columns: NonZeroU16) -> Result<Line, String> {
        let line = Line { glyphs, ending };
        let taken = line.columns();
        if taken != u32::from(columns.get()) {
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        Ok(line)
    }

    fn columns(&self) -> u32 {
        let taken: u32 = self.glyphs.iter().map(|glyph| u32::from(glyph.shows.columns())).sum();
        taken + u32::from(self.wraps_wide())
    }

    fn heard(line: Option<api::Line>, columns: NonZeroU16) -> Result<Line, String> {
        let line = line.ok_or("a row of a frame has no line")?;
        let glyphs = line.glyphs.into_iter().map(glyph).collect::<Result<Vec<_>, _>>()?;
        let ending = match (line.wraps_wide, line.wraps) {
            (true, _) => Ending::WrapsWide,
            (false, true) => Ending::Wraps,
            (false, false) => Ending::Here,
        };
        Line::new(glyphs, ending, columns)
    }
}

fn glyph(glyph: api::Glyph) -> Result<Glyph, String> {
    let shows = match glyph.shows.ok_or("a glyph shows nothing")? {
        api::glyph::Shows::Blank(api::Blank {}) => Shows::Blank,
        api::glyph::Shows::Narrow(api::Narrow { grapheme }) => {
            Shows::Narrow(Grapheme::new(grapheme)?)
        },
        api::glyph::Shows::Wide(api::Wide { grapheme }) => Shows::Wide(Grapheme::new(grapheme)?),
    };
    Ok(Glyph { shows, style: style_heard(glyph.style)? })
}

fn style_heard(style: Option<api::Style>) -> Result<Style, String> {
    let Some(style) = style else {
        return Ok(Style::default());
    };
    let underlined = !matches!(
        api::Underline::try_from(style.underline),
        Ok(api::Underline::None | api::Underline::Unspecified) | Err(_)
    );
    Ok(Style {
        foreground: colour_heard(style.foreground)?,
        background: colour_heard(style.background)?,
        underline_colour: colour_heard(style.underline_colour)?,
        bold: style.bold,
        faint: style.faint,
        italic: style.italic,
        underlined,
        blink: style.blink,
        inverse: style.inverse,
        invisible: style.invisible,
        strikethrough: style.strikethrough,
    })
}

fn colour_heard(colour: Option<api::Colour>) -> Result<Option<Colour>, String> {
    let Some(colour) = colour else {
        return Ok(None);
    };
    let byte = |value: u32| u8::try_from(value).ok();
    let read = match colour.is.as_ref().ok_or("a colour is none")? {
        api::colour::Is::Palette(index) => byte(*index).map(Colour::Palette),
        api::colour::Is::Rgb(rgb) => match (byte(rgb.red), byte(rgb.green), byte(rgb.blue)) {
            (Some(red), Some(green), Some(blue)) => Some(Colour::Rgb([red, green, blue])),
            _ => None,
        },
    };
    let none = || format!("the colour {colour:?} is of none a terminal has");
    read.map(Some).ok_or_else(none)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rows {
    Whole(Vec<Line>),
    Changed(Vec<(u16, Line)>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
    pub blinking: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    terminal: TerminalKey,
    size: Size,
    rows: Rows,
    cursor: Option<Cursor>,
    tracks_mouse: bool,
}

impl Frame {
    pub fn new(
        terminal: TerminalKey,
        size: Size,
        rows: Rows,
        cursor: Option<Cursor>,
        tracks_mouse: bool,
    ) -> Result<Frame, String> {
        let columns = u32::from(size.columns.get());
        let lines: Vec<&Line> = match &rows {
            Rows::Whole(lines) => {
                if lines.len() != usize::from(size.rows.get()) {
                    return Err(format!("a frame of {} rows has {}", size.rows, lines.len()));
                }
                lines.iter().collect()
            },
            Rows::Changed(rows) => {
                let mut changed = vec![false; usize::from(size.rows.get())];
                for (index, _) in rows {
                    let Some(seen) = changed.get_mut(usize::from(*index)) else {
                        return Err(format!("a frame has no row {index}"));
                    };
                    if std::mem::replace(seen, true) {
                        return Err(format!("a frame changes row {index} twice"));
                    }
                }
                rows.iter().map(|(_, line)| line).collect()
            },
        };
        if let Some(line) = lines.into_iter().find(|line| line.columns() != columns) {
            let taken = line.columns();
            return Err(format!("a row of {columns} columns has glyphs of {taken}"));
        }
        if let Some(cursor) = cursor
            && (cursor.row >= size.rows.get() || cursor.column >= size.columns.get())
        {
            let at = format!("{}:{}", cursor.row, cursor.column);
            return Err(format!("the cursor is off the frame, at {at}"));
        }
        Ok(Frame { terminal, size, rows, cursor, tracks_mouse })
    }

    #[must_use]
    pub fn terminal(&self) -> TerminalKey {
        self.terminal
    }

    #[must_use]
    pub fn size(&self) -> Size {
        self.size
    }

    #[must_use]
    pub fn rows(&self) -> &Rows {
        &self.rows
    }

    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        self.cursor
    }

    #[must_use]
    pub fn tracks_mouse(&self) -> bool {
        self.tracks_mouse
    }

    pub(super) fn heard(frame: api::Frame) -> Result<Frame, String> {
        let terminal = TerminalKey::read(frame.terminal)?;
        let size = Size::heard("a frame", frame.size)?;
        let rows = match frame.rows.ok_or("a frame has no rows")? {
            api::frame::Rows::Whole(api::Whole { lines }) => {
                if lines.len() != usize::from(size.rows.get()) {
                    return Err(format!("a frame of {} rows has {}", size.rows, lines.len()));
                }
                let lines = lines.into_iter().map(|line| Line::heard(Some(line), size.columns));
                Rows::Whole(lines.collect::<Result<_, _>>()?)
            },
            api::frame::Rows::Changed(api::Changed { rows }) => {
                let row = |row: api::Row| {
                    let index = u16::try_from(row.index).ok().filter(|it| *it < size.rows.get());
                    let index = index.ok_or_else(|| format!("a frame has no row {}", row.index))?;
                    Ok((index, Line::heard(row.line, size.columns)?))
                };
                Rows::Changed(rows.into_iter().map(row).collect::<Result<_, String>>()?)
            },
        };
        let cursor = frame.cursor.map(|cursor| cursor_heard(&cursor)).transpose()?;
        Frame::new(terminal, size, rows, cursor, frame.mouse_tracking)
    }
}

fn cursor_heard(cursor: &api::Cursor) -> Result<Cursor, String> {
    let off = || format!("the cursor is off the frame, at {}:{}", cursor.row, cursor.column);
    let row = u16::try_from(cursor.row).map_err(|_| off())?;
    let column = u16::try_from(cursor.column).map_err(|_| off())?;
    let shape = match api::CursorShape::try_from(cursor.shape) {
        Ok(api::CursorShape::Bar) => CursorShape::Bar,
        Ok(api::CursorShape::Underline) => CursorShape::Underline,
        Ok(api::CursorShape::BlockHollow) => CursorShape::BlockHollow,
        Ok(api::CursorShape::Block | api::CursorShape::Unspecified) | Err(_) => CursorShape::Block,
    };
    Ok(Cursor { row, column, shape, blinking: cursor.blinking })
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;
    use crate::mux::own::OwnMade;

    fn key() -> TerminalKey {
        TerminalKey { id: NonZeroU32::new(2).unwrap(), made: OwnMade::from_nanos(60) }
    }

    fn narrow(grapheme: &str) -> api::Glyph {
        let shows = api::glyph::Shows::Narrow(api::Narrow { grapheme: grapheme.to_owned() });
        api::Glyph { shows: Some(shows), style: None }
    }

    fn wide(grapheme: &str) -> api::Glyph {
        let shows = api::glyph::Shows::Wide(api::Wide { grapheme: grapheme.to_owned() });
        api::Glyph { shows: Some(shows), style: None }
    }

    fn frame(columns: u32, lines: Vec<api::Line>) -> api::Frame {
        let rows = u32::try_from(lines.len()).unwrap();
        api::Frame {
            terminal: Some(key().said()),
            size: Some(api::Size { columns, rows }),
            rows: Some(api::frame::Rows::Whole(api::Whole { lines })),
            cursor: None,
            ..api::Frame::default()
        }
    }

    #[test]
    fn a_frame_reads_where_its_rows_add_up_to_its_size() {
        let line = |glyphs: Vec<api::Glyph>, wraps_wide| {
            api::Line { glyphs, wraps_wide, wraps: false }
        };
        let wrapped = |line: api::Line| api::Line { wraps: true, ..line };
        let read = Frame::heard(frame(
            3,
            vec![
                line(vec![narrow("a"), wide("日")], false),
                line(vec![narrow("a"), narrow("b")], true),
                wrapped(line(vec![narrow("a"), narrow("b")], true)),
                wrapped(line(vec![narrow("a"), narrow("b"), narrow("c")], false)),
            ],
        ))
        .unwrap();
        let Rows::Whole(lines) = &read.rows else { panic!("whole rows") };
        let two = Shows::Wide(Grapheme::new("日".to_owned()).unwrap());
        assert_eq!(lines[0].glyphs()[1].shows, two);
        assert!(!lines[0].wraps());
        assert!(lines[1].wraps_wide());
        assert!(lines[1].wraps(), "a mux that says only that a wide glyph wraps, still wraps");
        assert!(lines[2].wraps_wide());
        assert!(lines[3].wraps());
        assert!(!lines[3].wraps_wide());
        let short = Frame::heard(frame(3, vec![line(vec![narrow("a")], false)]));
        assert_eq!(short.unwrap_err(), "a row of 3 columns has glyphs of 1");
        let long = Frame::heard(frame(2, vec![line(vec![narrow("a"), wide("日")], false)]));
        assert_eq!(long.unwrap_err(), "a row of 2 columns has glyphs of 3");
    }

    #[test]
    fn a_frame_that_does_not_fit_its_size_does_not_read() {
        let row = api::Line { glyphs: vec![narrow("a")], wraps_wide: false, wraps: false };
        let mut two = frame(1, vec![row.clone(), row.clone()]);
        two.size = Some(api::Size { columns: 1, rows: 1 });
        assert_eq!(Frame::heard(two).unwrap_err(), "a frame of 1 rows has 2");
        let nobody = api::Frame { terminal: None, ..frame(1, vec![row.clone()]) };
        assert_eq!(Frame::heard(nobody).unwrap_err(), "a terminal with no key");
        let changed = api::Changed { rows: vec![api::Row { index: 1, line: Some(row.clone()) }] };
        let past = api::Frame {
            rows: Some(api::frame::Rows::Changed(changed)),
            ..frame(1, vec![row.clone()])
        };
        assert_eq!(Frame::heard(past).unwrap_err(), "a frame has no row 1");
        let twice = |index| api::Row { index, line: Some(row.clone()) };
        let changed = api::Changed { rows: vec![twice(0), twice(0)] };
        let again = api::Frame {
            rows: Some(api::frame::Rows::Changed(changed)),
            ..frame(1, vec![row.clone()])
        };
        assert_eq!(Frame::heard(again).unwrap_err(), "a frame changes row 0 twice");
        let cursor = Some(api::Cursor { row: 0, column: 1, ..api::Cursor::default() });
        let off = api::Frame { cursor, ..frame(1, vec![row.clone()]) };
        assert_eq!(Frame::heard(off).unwrap_err(), "the cursor is off the frame, at 0:1");
        let none =
            api::Frame { size: Some(api::Size { columns: 0, rows: 1 }), ..frame(1, vec![row]) };
        assert_eq!(Frame::heard(none).unwrap_err(), "a frame's columns are 0");
        let escape = api::Line { glyphs: vec![narrow("\u{1b}")], wraps_wide: false, wraps: false };
        assert!(Frame::heard(frame(1, vec![escape])).unwrap_err().contains("control character"));
    }

    #[test]
    fn a_glyph_of_a_colour_no_terminal_has_does_not_read() {
        let coloured = |is| {
            let foreground = Some(api::Colour { is });
            let style = Some(api::Style { foreground, ..api::Style::default() });
            glyph(api::Glyph { style, ..narrow("a") })
        };
        let foreground = |glyph: Result<Glyph, String>| glyph.unwrap().style.foreground;
        let rgb = |red| Some(api::colour::Is::Rgb(api::Rgb { red, green: 0, blue: 255 }));
        let palette = foreground(coloured(Some(api::colour::Is::Palette(255))));
        assert_eq!(palette, Some(Colour::Palette(255)));
        assert_eq!(foreground(coloured(rgb(255))), Some(Colour::Rgb([255, 0, 255])));
        assert!(coloured(Some(api::colour::Is::Palette(256))).unwrap_err().contains("none a"));
        assert!(coloured(rgb(256)).unwrap_err().contains("is of none a terminal has"));
        assert_eq!(coloured(None).unwrap_err(), "a colour is none");
        let plain = glyph(api::Glyph { style: Some(api::Style::default()), ..narrow("a") });
        assert_eq!(plain, glyph(narrow("a")));
        assert_eq!(glyph(narrow("a")).unwrap().style, Style::default());
    }

    #[test]
    fn a_frame_is_made_only_of_its_size() {
        let size = Size { columns: NonZeroU16::new(2).unwrap(), rows: NonZeroU16::MIN };
        let line = |columns: u16| {
            let glyph = Glyph { shows: Shows::Blank, style: Style::default() };
            let glyphs = vec![glyph; usize::from(columns)];
            Line::new(glyphs, Ending::Here, NonZeroU16::new(columns).unwrap()).unwrap()
        };
        let two = line(2);
        let cursor = |column| {
            Some(Cursor { row: 0, column, shape: CursorShape::Bar, blinking: false })
        };
        let made = |rows, cursor| Frame::new(key(), size, rows, cursor, false);
        assert!(made(Rows::Whole(vec![two.clone()]), cursor(1)).is_ok());
        let none = made(Rows::Whole(Vec::new()), None);
        assert_eq!(none.unwrap_err(), "a frame of 1 rows has 0");
        let past = made(Rows::Changed(vec![(1, two.clone())]), None);
        assert_eq!(past.unwrap_err(), "a frame has no row 1");
        let twice = made(Rows::Changed(vec![(0, two.clone()), (0, two.clone())]), None);
        assert_eq!(twice.unwrap_err(), "a frame changes row 0 twice");
        let off = made(Rows::Whole(vec![two]), cursor(2));
        assert_eq!(off.unwrap_err(), "the cursor is off the frame, at 0:2");
        let narrow = made(Rows::Whole(vec![line(1)]), None);
        assert_eq!(narrow.unwrap_err(), "a row of 2 columns has glyphs of 1");
        assert!(Grapheme::new("\u{1b}[2J".to_owned()).unwrap_err().contains("control character"));
        assert_eq!(Grapheme::new(String::new()).unwrap_err(), "a glyph has no grapheme");
    }

    #[test]
    fn a_size_is_held_to_what_the_server_takes() {
        let cells = |count| NonZeroU16::new(count).unwrap();
        let size = Size::within(cells(4000), cells(30));
        assert_eq!((size.columns.get(), size.rows.get()), (SIDE_MAX, 30));
    }
}
