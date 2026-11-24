use std::sync::Arc;
use std::time::Duration;

use lune_protocol::mux::v1 as mux;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::Status;

use crate::api::{self, frames_request};
use crate::core::Daemon;
use crate::model::TerminalKey;
use crate::unanswered::{Refusals, Unanswered};

pub const SAID_HELD: usize = 256;

pub const SIDE_MAX: u16 = 1000;

const CLOSED: &str = "the phone closed this terminal";

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a way a run is drawn apart from the rest, in any combination"
)]
pub struct Run {
    pub column: u16,
    pub columns: u16,
    pub text: String,
    pub foreground: Rgb,
    pub background: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underlined: bool,
    pub struck: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Row {
    pub runs: Vec<Run>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Cursor {
    pub row: u16,
    pub column: u16,
    pub shape: CursorShape,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Grid {
    pub columns: u16,
    pub rows: Vec<Row>,
    pub cursor: Option<Cursor>,
    pub scrolled_back: u32,
    pub scrollback: u32,
    pub foreground: Rgb,
    pub background: Rgb,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Framed {
    Shown { grid: Grid },
    Lost { why: String },
    Refused { why: String },
}

impl Framed {
    fn ends(&self) -> bool {
        match self {
            Framed::Shown { .. } => false,
            Framed::Lost { .. } | Framed::Refused { .. } => true,
        }
    }

    fn of(status: &Status) -> Framed {
        match Unanswered::of(status, Refusals::Others) {
            Unanswered::Refused { why } => Framed::Refused { why },
            Unanswered::InDoubt { why } => Framed::Lost { why },
        }
    }

    fn unread(why: impl std::fmt::Display) -> Framed {
        Framed::Lost { why: Unanswered::unread(why).why().to_owned() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum KeyName {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
}

impl KeyName {
    fn named(self) -> mux::Named {
        match self {
            KeyName::Enter => mux::Named::Enter,
            KeyName::Tab => mux::Named::Tab,
            KeyName::Backspace => mux::Named::Backspace,
            KeyName::Escape => mux::Named::Escape,
            KeyName::Up => mux::Named::Up,
            KeyName::Down => mux::Named::Down,
            KeyName::Left => mux::Named::Left,
            KeyName::Right => mux::Named::Right,
        }
    }
}

struct Palette {
    foreground: Rgb,
    background: Rgb,
    ansi: [Rgb; 16],
}

const fn rgb(red: u8, green: u8, blue: u8) -> Rgb {
    Rgb { red, green, blue }
}

const DARK: Palette = Palette {
    foreground: rgb(229, 229, 234),
    background: rgb(28, 28, 30),
    ansi: [
        rgb(44, 44, 46),
        rgb(255, 105, 97),
        rgb(48, 209, 88),
        rgb(255, 214, 10),
        rgb(64, 156, 255),
        rgb(218, 143, 255),
        rgb(100, 210, 255),
        rgb(209, 209, 214),
        rgb(99, 99, 102),
        rgb(255, 140, 133),
        rgb(102, 230, 133),
        rgb(255, 230, 102),
        rgb(112, 182, 255),
        rgb(232, 182, 255),
        rgb(150, 228, 255),
        rgb(255, 255, 255),
    ],
};

const LIGHT: Palette = Palette {
    foreground: rgb(28, 28, 30),
    background: rgb(255, 255, 255),
    ansi: [
        rgb(28, 28, 30),
        rgb(196, 40, 30),
        rgb(36, 138, 61),
        rgb(160, 110, 0),
        rgb(0, 88, 208),
        rgb(137, 68, 171),
        rgb(0, 113, 164),
        rgb(142, 142, 147),
        rgb(99, 99, 102),
        rgb(215, 0, 21),
        rgb(36, 160, 70),
        rgb(178, 128, 0),
        rgb(0, 100, 230),
        rgb(160, 80, 200),
        rgb(0, 130, 190),
        rgb(199, 199, 204),
    ],
};

impl Appearance {
    fn palette(self) -> &'static Palette {
        match self {
            Appearance::Light => &LIGHT,
            Appearance::Dark => &DARK,
        }
    }

    fn greeting(self) -> mux::Greeting {
        let palette = self.palette();
        let said = |Rgb { red, green, blue }: Rgb| mux::Rgb {
            red: red.into(),
            green: green.into(),
            blue: blue.into(),
        };
        let colours = mux::Colours {
            foreground: Some(said(palette.foreground)),
            background: Some(said(palette.background)),
        };
        mux::Greeting { colours: Some(colours), runs_in: None }
    }
}

fn indexed(palette: &Palette, index: u8) -> Rgb {
    let level = |step: u8| if step == 0 { 0 } else { 55 + 40 * step };
    match index {
        0..=15 => palette.ansi[usize::from(index)],
        16..=231 => {
            let cube = index - 16;
            rgb(level(cube / 36), level(cube / 6 % 6), level(cube % 6))
        },
        232..=255 => {
            let grey = 8 + 10 * (index - 232);
            rgb(grey, grey, grey)
        },
    }
}

fn colour(palette: &Palette, colour: Option<&mux::Colour>) -> Result<Option<Rgb>, String> {
    let Some(colour) = colour else {
        return Ok(None);
    };
    let byte = |value: u32| u8::try_from(value).ok();
    let read = match colour.is.as_ref().ok_or("a colour is none")? {
        mux::colour::Is::Palette(index) => byte(*index).map(|index| indexed(palette, index)),
        mux::colour::Is::Rgb(said) => match (byte(said.red), byte(said.green), byte(said.blue)) {
            (Some(red), Some(green), Some(blue)) => Some(rgb(red, green, blue)),
            _ => None,
        },
    };
    read.map(Some).ok_or_else(|| format!("the colour {colour:?} is of none a terminal has"))
}

fn halfway(from: Rgb, to: Rgb) -> Rgb {
    let mid = u8::midpoint;
    rgb(mid(from.red, to.red), mid(from.green, to.green), mid(from.blue, to.blue))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a way a run is drawn apart from the rest, in any combination"
)]
struct Look {
    foreground: Rgb,
    background: Option<Rgb>,
    bold: bool,
    italic: bool,
    underlined: bool,
    struck: bool,
}

impl Look {
    fn of(palette: &Palette, style: Option<&mux::Style>) -> Result<Look, String> {
        let Some(style) = style else {
            let plain = Look {
                foreground: palette.foreground,
                background: None,
                bold: false,
                italic: false,
                underlined: false,
                struck: false,
            };
            return Ok(plain);
        };
        let mut foreground = colour(palette, style.foreground.as_ref())?.unwrap_or(palette.foreground);
        let mut background = colour(palette, style.background.as_ref())?;
        if style.inverse {
            (foreground, background) = (background.unwrap_or(palette.background), Some(foreground));
        }
        let behind = background.unwrap_or(palette.background);
        if style.faint {
            foreground = halfway(foreground, behind);
        }
        if style.invisible {
            foreground = behind;
        }
        let underlined = !matches!(
            mux::Underline::try_from(style.underline),
            Ok(mux::Underline::None | mux::Underline::Unspecified) | Err(_)
        );
        Ok(Look {
            foreground,
            background,
            bold: style.bold,
            italic: style.italic,
            underlined,
            struck: style.strikethrough,
        })
    }

    fn run(self, column: u16, columns: u16, text: String) -> Run {
        let Look { foreground, background, bold, italic, underlined, struck } = self;
        Run { column, columns, text, foreground, background, bold, italic, underlined, struck }
    }

    fn draws(self, text: &str) -> bool {
        self.background.is_some() || self.underlined || self.struck || text.chars().any(|it| it != ' ')
    }
}

fn grapheme(grapheme: String) -> Result<String, String> {
    if grapheme.is_empty() {
        return Err("a glyph has no grapheme".to_owned());
    }
    if grapheme.chars().any(char::is_control) {
        return Err(format!("the glyph {grapheme:?} has a control character"));
    }
    Ok(grapheme)
}

fn row(palette: &Palette, line: mux::Line, columns: u16) -> Result<Row, String> {
    let ending = mux::Ending::try_from(line.ending).unwrap_or(mux::Ending::Unspecified);
    let blank_end = match ending {
        mux::Ending::Here | mux::Ending::Wraps => 0,
        mux::Ending::WrapsWide => 1,
        mux::Ending::Unspecified => return Err("a row of a frame does not say how it ends".to_owned()),
    };
    let mut runs: Vec<Run> = Vec::new();
    let mut open: Option<(Look, u16, String, u16)> = None;
    let mut column: u16 = 0;
    let close = |runs: &mut Vec<Run>, open: Option<(Look, u16, String, u16)>| {
        if let Some((look, at, text, width)) = open
            && look.draws(&text)
        {
            runs.push(look.run(at, width, text));
        }
    };
    for glyph in line.glyphs {
        let look = Look::of(palette, glyph.style.as_ref())?;
        let (text, width) = match glyph.shows.ok_or("a glyph shows nothing")? {
            mux::glyph::Shows::Blank(mux::Blank {}) => (" ".to_owned(), 1),
            mux::glyph::Shows::Narrow(mux::Narrow { grapheme: said }) => (grapheme(said)?, 1),
            mux::glyph::Shows::Wide(mux::Wide { grapheme: said }) => (grapheme(said)?, 2),
        };
        let past = column.checked_add(width).filter(|past| *past <= columns);
        let past = past.ok_or_else(|| format!("a row of {columns} columns has glyphs past it"))?;
        match &mut open {
            Some((was, _, held, held_width)) if width == 1 && *was == look => {
                held.push_str(&text);
                *held_width += 1;
            },
            _ => {
                close(&mut runs, open.take());
                open = Some((look, column, text, width));
            },
        }
        if width == 2 {
            close(&mut runs, open.take());
        }
        column = past;
    }
    close(&mut runs, open.take());
    if column + blank_end != columns {
        return Err(format!("a row of {columns} columns has glyphs of {}", column + blank_end));
    }
    Ok(Row { runs })
}

fn side(what: &str, count: u32) -> Result<u16, String> {
    let fits = u16::try_from(count).ok().filter(|side| (1..=SIDE_MAX).contains(side));
    fits.ok_or_else(|| format!("a frame's {what} are {count}"))
}

struct Drawn {
    terminal: mux::TerminalKey,
    palette: &'static Palette,
    columns: u16,
    rows: Vec<Row>,
}

impl Drawn {
    fn new(terminal: mux::TerminalKey, appearance: Appearance) -> Drawn {
        Drawn { terminal, palette: appearance.palette(), columns: 0, rows: Vec::new() }
    }

    fn apply(&mut self, frame: mux::Frame) -> Result<Grid, String> {
        if frame.terminal.as_ref() != Some(&self.terminal) {
            return Err(format!("a frame is of {:?}, which the phone does not show", frame.terminal));
        }
        let size = frame.size.ok_or("a frame has no size")?;
        let (columns, height) = (side("columns", size.columns)?, side("rows", size.rows)?);
        let palette = self.palette;
        match frame.rows.ok_or("a frame has no rows")? {
            mux::frame::Rows::Whole(mux::Whole { lines }) => {
                if lines.len() != usize::from(height) {
                    return Err(format!("a frame of {height} rows has {}", lines.len()));
                }
                let lines = lines.into_iter().map(|line| row(palette, line, columns));
                self.rows = lines.collect::<Result<_, _>>()?;
                self.columns = columns;
            },
            mux::frame::Rows::Changed(mux::Changed { rows }) => {
                if (self.columns, self.rows.len()) != (columns, usize::from(height)) {
                    return Err(format!("a frame changes rows of a {columns}x{height} screen the phone has not seen whole"));
                }
                let mut changed = vec![false; self.rows.len()];
                for mux::Row { index, line } in rows {
                    let at = usize::try_from(index).ok().filter(|at| *at < self.rows.len());
                    let at = at.ok_or_else(|| format!("a frame has no row {index}"))?;
                    if std::mem::replace(&mut changed[at], true) {
                        return Err(format!("a frame changes row {index} twice"));
                    }
                    self.rows[at] = row(palette, line.ok_or("a row of a frame has no line")?, columns)?;
                }
            },
        }
        let cursor = frame.cursor.map(|cursor| cursor_of(&cursor, columns, height)).transpose()?;
        Ok(Grid {
            columns,
            rows: self.rows.clone(),
            cursor,
            scrolled_back: frame.scrolled_back,
            scrollback: frame.scrollback,
            foreground: palette.foreground,
            background: palette.background,
        })
    }
}

fn cursor_of(cursor: &mux::Cursor, columns: u16, rows: u16) -> Result<Cursor, String> {
    let off = || format!("the cursor is off the frame, at {}:{}", cursor.row, cursor.column);
    let row = u16::try_from(cursor.row).ok().filter(|row| *row < rows).ok_or_else(off)?;
    let column = u16::try_from(cursor.column).ok().filter(|column| *column < columns).ok_or_else(off)?;
    let shape = match mux::CursorShape::try_from(cursor.shape) {
        Ok(mux::CursorShape::Bar) => CursorShape::Bar,
        Ok(mux::CursorShape::Underline) => CursorShape::Underline,
        Ok(mux::CursorShape::BlockHollow) => CursorShape::BlockHollow,
        Ok(mux::CursorShape::Block | mux::CursorShape::Unspecified) | Err(_) => CursorShape::Block,
    };
    Ok(Cursor { row, column, shape })
}

fn modifiers(ctrl: bool, shift: bool) -> mux::Modifiers {
    mux::Modifiers { shift, alt: false, ctrl, super_key: false }
}

fn key(named: mux::Named, ctrl: bool) -> mux::typed::Event {
    mux::typed::Event::Key(mux::Key {
        press: mux::Press::Down.into(),
        modifiers: Some(modifiers(ctrl, false)),
        key: Some(mux::key::Key::Named(named.into())),
        text: String::new(),
    })
}

fn typed(text: &str, ctrl: bool) -> Option<mux::typed::Event> {
    let mut chars = text.chars();
    let (Some(typed), None) = (chars.next(), chars.next()) else {
        let pasted = mux::typed::Event::Paste(mux::Paste { text: text.to_owned() });
        return (!text.is_empty()).then_some(pasted);
    };
    match typed {
        '\n' | '\r' => return Some(key(mux::Named::Enter, ctrl)),
        '\t' => return Some(key(mux::Named::Tab, ctrl)),
        typed if typed.is_control() => return None,
        _ => {},
    }
    let mut lower = typed.to_lowercase();
    let unshifted = match (lower.next(), lower.next()) {
        (Some(lower), None) => lower,
        _ => typed,
    };
    Some(mux::typed::Event::Key(mux::Key {
        press: mux::Press::Down.into(),
        modifiers: Some(modifiers(ctrl, unshifted != typed)),
        key: Some(mux::key::Key::Character(unshifted.into())),
        text: typed.to_string(),
    }))
}

fn told(tell: &watch::Sender<Option<Framed>>, framed: Framed) {
    tell.send_if_modified(|now| {
        if now.as_ref().is_some_and(Framed::ends) {
            return false;
        }
        *now = Some(framed);
        true
    });
}

async fn read(
    mut daemon: Daemon,
    patience: Duration,
    mut drawn: Drawn,
    said: mpsc::Receiver<api::FramesRequest>,
    tell: &watch::Sender<Option<Framed>>,
) -> Framed {
    let opened = timeout(patience, daemon.frames(ReceiverStream::new(said))).await;
    let mut frames = match opened {
        Ok(Ok(frames)) => frames.into_inner(),
        Ok(Err(status)) => return Framed::of(&status),
        Err(_) => return Framed::Lost { why: Unanswered::late(patience).why().to_owned() },
    };
    loop {
        let event = match frames.message().await {
            Ok(Some(mux::ToViewer { event: Some(event) })) => event,
            Ok(Some(mux::ToViewer { event: None })) => return Framed::unread("a word of the frames says nothing"),
            Ok(None) => return Framed::Lost { why: "luned ended the terminal's frames".to_owned() },
            Err(status) => return Framed::of(&status),
        };
        match event {
            mux::to_viewer::Event::Frame(frame) => match drawn.apply(frame) {
                Ok(grid) => told(tell, Framed::Shown { grid }),
                Err(why) => return Framed::unread(why),
            },
            mux::to_viewer::Event::Refused(mux::Refused { reason }) => return Framed::Refused { why: reason },
        }
    }
}

#[derive(uniffi::Object)]
pub struct TerminalView {
    terminal: mux::TerminalKey,
    says: mpsc::Sender<api::FramesRequest>,
    tell: Arc<watch::Sender<Option<Framed>>>,
    seen: tokio::sync::Mutex<watch::Receiver<Option<Framed>>>,
    reading: JoinHandle<()>,
}

fn said(event: mux::from_viewer::Event) -> api::FramesRequest {
    let said = mux::FromViewer { event: Some(event) };
    api::FramesRequest { event: Some(frames_request::Event::Said(said)) }
}

impl TerminalView {
    pub(crate) fn open(
        daemon: Daemon,
        handle: &Handle,
        patience: Duration,
        key: &TerminalKey,
        appearance: Appearance,
    ) -> Arc<TerminalView> {
        let (on, api::TerminalKey { id, made_at }) = key.to_api();
        let terminal = mux::TerminalKey { id, made_at };
        let (says, heard) = mpsc::channel(SAID_HELD);
        let hello = frames_request::Hello { on, greeting: Some(appearance.greeting()) };
        let hello = api::FramesRequest { event: Some(frames_request::Event::Hello(hello)) };
        let showing = mux::Showing { terminals: vec![terminal] };
        for first in [hello, said(mux::from_viewer::Event::Showing(showing))] {
            says.try_send(first).expect("the first words fit before any other is said");
        }
        let (tell, seen) = watch::channel(None);
        let tell = Arc::new(tell);
        let drawn = Drawn::new(terminal, appearance);
        let telling = Arc::clone(&tell);
        let reading = handle.spawn(async move {
            let ended = read(daemon, patience, drawn, heard, &telling).await;
            told(&telling, ended);
        });
        let seen = tokio::sync::Mutex::new(seen);
        Arc::new(TerminalView { terminal, says, tell, seen, reading })
    }

    fn say(&self, event: mux::typed::Event) -> Result<(), Unanswered> {
        if self.tell.borrow().as_ref().is_some_and(Framed::ends) {
            return Err(Unanswered::refused("the terminal is shown no more"));
        }
        let typed = mux::Typed { terminal: Some(self.terminal), event: Some(event) };
        match self.says.try_send(said(mux::from_viewer::Event::Typed(typed))) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                Err(Unanswered::refused(format!("{SAID_HELD} words to luned wait already")))
            },
            Err(mpsc::error::TrySendError::Closed(_)) => {
                Err(Unanswered::refused("the terminal is shown no more"))
            },
        }
    }
}

#[uniffi::export]
impl TerminalView {
    pub async fn next(&self) -> Framed {
        let mut seen = self.seen.lock().await;
        let mut fresh = seen.has_changed().unwrap_or(false);
        loop {
            let now = seen.borrow_and_update().clone();
            if let Some(framed) = now
                && (fresh || framed.ends())
            {
                return framed;
            }
            if seen.changed().await.is_err() {
                return Framed::Lost { why: "the core's reader of the terminal ended".to_owned() };
            }
            fresh = true;
        }
    }

    pub fn typed(&self, text: &str, ctrl: bool) -> Result<(), Unanswered> {
        match self::typed(text, ctrl) {
            Some(event) => self.say(event),
            None => Ok(()),
        }
    }

    pub fn pressed(&self, key: KeyName, ctrl: bool) -> Result<(), Unanswered> {
        self.say(self::key(key.named(), ctrl))
    }

    pub fn scroll(&self, back: i32) -> Result<(), Unanswered> {
        self.say(mux::typed::Event::Scroll(mux::Scroll { back }))
    }

    pub fn close(&self) {
        self.reading.abort();
        told(&self.tell, Framed::Lost { why: CLOSED.to_owned() });
    }
}

impl Drop for TerminalView {
    fn drop(&mut self) {
        self.reading.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_of() -> mux::TerminalKey {
        mux::TerminalKey { id: 2, made_at: Some(prost_types::Timestamp { seconds: 60, nanos: 0 }) }
    }

    fn narrow(grapheme: &str) -> mux::Glyph {
        let shows = mux::glyph::Shows::Narrow(mux::Narrow { grapheme: grapheme.to_owned() });
        mux::Glyph { shows: Some(shows), style: None }
    }

    fn wide(grapheme: &str) -> mux::Glyph {
        let shows = mux::glyph::Shows::Wide(mux::Wide { grapheme: grapheme.to_owned() });
        mux::Glyph { shows: Some(shows), style: None }
    }

    fn blank() -> mux::Glyph {
        mux::Glyph { shows: Some(mux::glyph::Shows::Blank(mux::Blank {})), style: None }
    }

    fn styled(glyph: mux::Glyph, style: mux::Style) -> mux::Glyph {
        mux::Glyph { style: Some(style), ..glyph }
    }

    fn line(glyphs: Vec<mux::Glyph>) -> mux::Line {
        mux::Line { glyphs, ending: mux::Ending::Here.into() }
    }

    fn whole(columns: u32, lines: Vec<mux::Line>) -> mux::Frame {
        let rows = u32::try_from(lines.len()).unwrap();
        mux::Frame {
            terminal: Some(key_of()),
            size: Some(mux::Size { columns, rows }),
            rows: Some(mux::frame::Rows::Whole(mux::Whole { lines })),
            ..mux::Frame::default()
        }
    }

    fn changed(columns: u32, rows: u32, changed: Vec<(u32, mux::Line)>) -> mux::Frame {
        let rows_said = changed.into_iter().map(|(index, line)| mux::Row { index, line: Some(line) });
        mux::Frame {
            terminal: Some(key_of()),
            size: Some(mux::Size { columns, rows }),
            rows: Some(mux::frame::Rows::Changed(mux::Changed { rows: rows_said.collect() })),
            ..mux::Frame::default()
        }
    }

    fn texts<'a>(grid: &'a Grid) -> Vec<Vec<(u16, &'a str)>> {
        let run = |run: &'a Run| (run.column, run.text.as_str());
        grid.rows.iter().map(|row| row.runs.iter().map(run).collect()).collect()
    }

    fn palette_colour(index: u32) -> mux::Colour {
        mux::Colour { is: Some(mux::colour::Is::Palette(index)) }
    }

    #[test]
    fn a_row_is_drawn_in_runs_of_one_look_with_a_wide_glyph_alone_and_blank_ends_left_out() {
        let mut drawn = Drawn::new(key_of(), Appearance::Dark);
        let red = mux::Style { foreground: Some(palette_colour(1)), ..mux::Style::default() };
        let glyphs = vec![
            narrow("l"),
            narrow("s"),
            blank(),
            styled(narrow("x"), red),
            wide("日"),
            narrow("y"),
            blank(),
            blank(),
        ];
        let grid = drawn.apply(whole(9, vec![line(glyphs), line(vec![blank(); 9])])).unwrap();
        assert_eq!(texts(&grid), vec![vec![(0, "ls "), (3, "x"), (4, "日"), (6, "y  ")], vec![]]);
        let runs = &grid.rows[0].runs;
        assert_eq!((runs[2].columns, runs[3].columns), (2, 3));
        assert_eq!(runs[1].foreground, DARK.ansi[1]);
        assert_eq!(runs[0].foreground, DARK.foreground, "a glyph of no colour is in the appearance's");
        assert_eq!((grid.foreground, grid.background), (DARK.foreground, DARK.background));
    }

    #[test]
    fn a_changed_frame_redraws_only_its_rows_of_the_screen_seen_whole_before() {
        let mut drawn = Drawn::new(key_of(), Appearance::Light);
        let unseen = drawn.apply(changed(1, 2, vec![(0, line(vec![narrow("a")]))]));
        assert!(unseen.unwrap_err().contains("has not seen whole"));
        drawn.apply(whole(1, vec![line(vec![narrow("a")]), line(vec![narrow("b")])])).unwrap();
        let grid = drawn.apply(changed(1, 2, vec![(1, line(vec![narrow("c")]))])).unwrap();
        assert_eq!(texts(&grid), vec![vec![(0, "a")], vec![(0, "c")]]);
        let resized = drawn.apply(changed(2, 2, vec![(1, line(vec![narrow("c"), blank()]))]));
        assert!(resized.unwrap_err().contains("has not seen whole"), "a new size comes whole");
        let twice = changed(1, 2, vec![(0, line(vec![narrow("d")])), (0, line(vec![narrow("e")]))]);
        assert_eq!(drawn.apply(twice).unwrap_err(), "a frame changes row 0 twice");
        let past = changed(1, 2, vec![(2, line(vec![narrow("d")]))]);
        assert_eq!(drawn.apply(past).unwrap_err(), "a frame has no row 2");
    }

    #[test]
    fn a_frame_that_does_not_fit_its_size_or_terminal_does_not_draw() {
        let mut drawn = Drawn::new(key_of(), Appearance::Dark);
        let short = drawn.apply(whole(3, vec![line(vec![narrow("a")])]));
        assert_eq!(short.unwrap_err(), "a row of 3 columns has glyphs of 1");
        let long = drawn.apply(whole(2, vec![line(vec![narrow("a"), wide("日")])]));
        assert_eq!(long.unwrap_err(), "a row of 2 columns has glyphs past it");
        let early = mux::Line { glyphs: vec![narrow("a")], ending: mux::Ending::WrapsWide.into() };
        assert!(drawn.apply(whole(2, vec![early])).is_ok(), "a wide glyph wrapped early leaves its cell blank");
        let rows = mux::Frame { size: Some(mux::Size { columns: 1, rows: 2 }), ..whole(1, vec![line(vec![narrow("a")])]) };
        assert_eq!(drawn.apply(rows).unwrap_err(), "a frame of 2 rows has 1");
        let other = mux::TerminalKey { id: 3, ..key_of() };
        let elsewhere = mux::Frame { terminal: Some(other), ..whole(1, vec![line(vec![narrow("a")])]) };
        assert!(drawn.apply(elsewhere).unwrap_err().contains("which the phone does not show"));
        let huge = whole(u32::from(SIDE_MAX) + 1, vec![line(vec![blank(); usize::from(SIDE_MAX) + 1])]);
        assert_eq!(drawn.apply(huge).unwrap_err(), "a frame's columns are 1001");
        let escape = drawn.apply(whole(1, vec![line(vec![narrow("\u{1b}")])]));
        assert!(escape.unwrap_err().contains("control character"));
        let cursor = Some(mux::Cursor { row: 0, column: 1, ..mux::Cursor::default() });
        let off = mux::Frame { cursor, ..whole(1, vec![line(vec![narrow("a")])]) };
        assert_eq!(drawn.apply(off).unwrap_err(), "the cursor is off the frame, at 0:1");
        let cursor = Some(mux::Cursor { row: 0, column: 0, shape: mux::CursorShape::Bar.into(), blinking: true });
        let on = drawn.apply(mux::Frame { cursor, ..whole(1, vec![line(vec![narrow("a")])]) }).unwrap();
        assert_eq!(on.cursor, Some(Cursor { row: 0, column: 0, shape: CursorShape::Bar }));
    }

    #[test]
    fn a_look_turns_inverse_faint_and_invisible_into_the_colours_it_paints() {
        let look = |style: mux::Style| Look::of(&DARK, Some(&style)).unwrap();
        let blue = mux::Colour { is: Some(mux::colour::Is::Rgb(mux::Rgb { red: 0, green: 0, blue: 200 })) };
        let inverse = look(mux::Style { background: Some(blue), inverse: true, ..mux::Style::default() });
        assert_eq!((inverse.foreground, inverse.background), (rgb(0, 0, 200), Some(DARK.foreground)));
        let plain_inverse = look(mux::Style { inverse: true, ..mux::Style::default() });
        assert_eq!((plain_inverse.foreground, plain_inverse.background), (DARK.background, Some(DARK.foreground)));
        let white = Some(palette_colour(15));
        let faint = look(mux::Style { foreground: white, faint: true, ..mux::Style::default() });
        assert_eq!(faint.foreground, halfway(rgb(255, 255, 255), DARK.background));
        let hidden = look(mux::Style { invisible: true, ..mux::Style::default() });
        assert_eq!(hidden.foreground, DARK.background);
        let curly = look(mux::Style { underline: mux::Underline::Curly.into(), ..mux::Style::default() });
        assert!(curly.underlined);
        assert!(!look(mux::Style { underline: mux::Underline::None.into(), ..mux::Style::default() }).underlined);
        let past = mux::Style { foreground: Some(palette_colour(256)), ..mux::Style::default() };
        assert!(Look::of(&DARK, Some(&past)).unwrap_err().contains("is of none a terminal has"));
    }

    #[test]
    fn the_256_colours_are_the_phone_s_sixteen_then_xterm_s_cube_and_greys() {
        assert_eq!(indexed(&LIGHT, 4), LIGHT.ansi[4]);
        assert_eq!(indexed(&DARK, 4), DARK.ansi[4]);
        assert_eq!(indexed(&DARK, 16), rgb(0, 0, 0));
        assert_eq!(indexed(&DARK, 196), rgb(255, 0, 0));
        assert_eq!(indexed(&DARK, 110), rgb(135, 175, 215));
        assert_eq!(indexed(&DARK, 231), rgb(255, 255, 255));
        assert_eq!(indexed(&DARK, 232), rgb(8, 8, 8));
        assert_eq!(indexed(&DARK, 255), rgb(238, 238, 238));
    }

    fn pressed(event: Option<mux::typed::Event>) -> (mux::key::Key, bool, bool, String) {
        match event {
            Some(mux::typed::Event::Key(mux::Key { key: Some(key), modifiers: Some(modifiers), text, press })) => {
                assert_eq!(press, i32::from(mux::Press::Down));
                (key, modifiers.ctrl, modifiers.shift, text)
            },
            other => panic!("a key, not {other:?}"),
        }
    }

    fn character(typed: char) -> mux::key::Key {
        mux::key::Key::Character(typed.into())
    }

    fn named(named: mux::Named) -> mux::key::Key {
        mux::key::Key::Named(named.into())
    }

    #[test]
    fn a_keystroke_goes_up_as_a_key_and_more_than_one_character_as_a_paste() {
        assert_eq!(pressed(typed("a", false)), (character('a'), false, false, "a".to_owned()));
        assert_eq!(pressed(typed("A", false)), (character('a'), false, true, "A".to_owned()));
        assert_eq!(pressed(typed("c", true)), (character('c'), true, false, "c".to_owned()));
        assert_eq!(pressed(typed("\n", false)), (named(mux::Named::Enter), false, false, String::new()));
        assert_eq!(pressed(typed("\t", true)), (named(mux::Named::Tab), true, false, String::new()));
        assert_eq!(pressed(typed("!", false)), (character('!'), false, false, "!".to_owned()));
        let pasted = typed("git status\n", true);
        assert_eq!(pasted, Some(mux::typed::Event::Paste(mux::Paste { text: "git status\n".to_owned() })));
        assert_eq!(typed("", true), None);
        assert_eq!(typed("\u{7}", false), None, "a control character alone is no keystroke");
        assert_eq!(pressed(Some(key(KeyName::Escape.named(), false))), (named(mux::Named::Escape), false, false, String::new()));
    }

    #[test]
    fn a_greeting_tells_luned_the_appearance_s_colours() {
        let colours = Appearance::Light.greeting().colours.unwrap();
        let white = mux::Rgb { red: 255, green: 255, blue: 255 };
        assert_eq!(colours.background, Some(white));
        assert_eq!(Appearance::Dark.greeting().runs_in, None);
    }

    #[test]
    fn a_frame_after_the_end_changes_nothing() {
        let (tell, seen) = watch::channel(None);
        told(&tell, Framed::Lost { why: CLOSED.to_owned() });
        told(&tell, Framed::Refused { why: "later".to_owned() });
        assert_eq!(*seen.borrow(), Some(Framed::Lost { why: CLOSED.to_owned() }));
    }
}
