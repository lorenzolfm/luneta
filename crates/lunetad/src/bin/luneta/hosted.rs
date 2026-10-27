//! The program luneta runs beside its sidebar, zellij's client, in a terminal
//! of luneta's own: a pseudoterminal it runs on, and a screen that keeps what
//! it draws there, as a terminal emulator does. luneta draws that screen into
//! its part of the terminal luneta itself runs in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::num::NonZeroU16;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use alacritty_terminal::event::{Event as Emitted, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions as _, GridCell as _};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{ClipboardType, Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as Colour, CursorShape, CursorStyle, NamedColor, Rgb};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};

use crate::host::Event;

/// What the program in luneta's terminal does that luneta has to act on.
pub enum Said {
    /// It drew: the screen is to be drawn again.
    Drew,
    /// It titled the terminal. zellij titles it with the session it is in.
    Titled(String),
    /// It asked for text to be copied, as zellij does for what you select.
    Copied(ClipboardType, String),
    /// It asked the terminal something, and this is the answer.
    Answer(String),
    /// It asked how big the terminal is, in pixels, and this writes the answer.
    AskedSize(Arc<dyn Fn(WindowSize) -> String + Sync + Send>),
    /// It asked for the colour alacritty numbers this, and this writes the
    /// answer: luneta's terminal's, which luneta draws the program's
    /// foreground and background in, whatever the program sets them to.
    AskedColour(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
    /// It rang the bell.
    Rang,
    /// It ended, with this status, where its terminal can tell. This comes
    /// before `Ended`.
    Exited(ExitStatus),
    /// Its terminal closed, with all it wrote on the screen.
    Ended,
}

/// How big the terminal luneta runs the program in is: in cells, of which it
/// has one each way at least, as alacritty's screen keeps its cursor on one,
/// and how big each cell is in pixels, or 0 where luneta's terminal does not
/// say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub columns: NonZeroU16,
    pub lines: NonZeroU16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl Size {
    fn dimensions(self) -> TermSize {
        TermSize::new(usize::from(self.columns.get()), usize::from(self.lines.get()))
    }
}

impl From<Size> for WindowSize {
    fn from(size: Size) -> Self {
        WindowSize {
            num_cols: size.columns.get(),
            num_lines: size.lines.get(),
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

/// Hands what the program does to luneta's loop.
#[derive(Clone)]
struct Listener {
    events: Sender<Event>,
    /// Whether the program drew since luneta last drew its screen. One
    /// `Said::Drew` stands for all it drew until then, so that luneta's queue
    /// holds one at the most, however fast the program draws.
    drew: Arc<AtomicBool>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Emitted) {
        let said = match event {
            Emitted::Wakeup => {
                if self.drew.swap(true, Ordering::SeqCst) {
                    return;
                }
                Said::Drew
            },
            Emitted::Title(title) => Said::Titled(title),
            Emitted::ClipboardStore(clipboard, text) => Said::Copied(clipboard, text),
            Emitted::PtyWrite(answer) => Said::Answer(answer),
            Emitted::TextAreaSizeRequest(answer) => Said::AskedSize(answer),
            // The loop answers it in its place, before what the program asked
            // after it, with the colour as luneta's terminal said it last.
            Emitted::ColorRequest(index, answer) => Said::AskedColour(index, answer),
            Emitted::Bell => Said::Rang,
            Emitted::ChildExit(status) => Said::Exited(status),
            Emitted::Exit => Said::Ended,
            // The rest is of the window a terminal emulator draws in.
            Emitted::ClipboardLoad(..)
            | Emitted::ResetTitle
            | Emitted::CursorBlinkingChange
            | Emitted::MouseCursorDirty => return,
        };
        // luneta is on its way out.
        let _ = self.events.send(Event::Hosted(said));
    }
}

/// The program, and the terminal luneta runs it in.
pub struct Hosted {
    screen: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    size: Size,
    /// The listener's, which `draw` clears.
    drew: Arc<AtomicBool>,
}

impl Hosted {
    /// Starts `program` in a terminal of `size`, in `dir`.
    pub fn start(
        program: String,
        args: Vec<String>,
        dir: &Path,
        size: Size,
        events: Sender<Event>,
    ) -> io::Result<Self> {
        // What the program writes as it ends is read before its terminal says
        // it closed, as zellij's error where it cannot start, which luneta says
        // again once its own screen is gone.
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: Some(dir.to_owned()),
            drain_on_exit: true,
            env: HashMap::new(),
        };
        let pty = tty::new(&options, size.into(), 0)?;
        let drew = Arc::new(AtomicBool::new(false));
        let listener = Listener { events, drew: Arc::clone(&drew) };
        // zellij draws on the alternate screen, and keeps the scrollback of
        // each pane itself. It asks for the kitty keyboard protocol, which
        // `keys` speaks. Its cursor is the hollow block a program cannot ask
        // for until it asks for another, so that luneta's terminal draws its
        // own meanwhile.
        let config = Config {
            scrolling_history: 0,
            kitty_keyboard: true,
            default_cursor_style: CursorStyle { shape: CursorShape::HollowBlock, blinking: false },
            ..Config::default()
        };
        let screen = Term::new(config, &size.dimensions(), listener.clone());
        let screen = Arc::new(FairMutex::new(screen));
        let drain_on_exit = options.drain_on_exit;
        let reader = EventLoop::new(Arc::clone(&screen), listener, pty, drain_on_exit, false)?;
        let pty = reader.channel();
        reader.spawn();
        Ok(Hosted { screen, pty, size, drew })
    }

    pub fn size(&self) -> Size {
        self.size
    }

    /// Makes the terminal `size`, and tells the program.
    pub fn resize(&mut self, size: Size) {
        if (size.columns, size.lines) == (self.size.columns, self.size.lines) {
            self.size = size;
            return;
        }
        self.size = size;
        self.screen.lock().resize(size.dimensions());
        // The reader is gone with the program, which needs no size.
        let _ = self.pty.send(Msg::Resize(size.into()));
    }

    /// Sends the program `bytes`, as typed into its terminal.
    pub fn send(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        // The reader is gone with the program, which reads no more.
        let _ = self.pty.send(Msg::Input(bytes));
    }

    /// What the program asked of its terminal: how it wants keys and the
    /// mouse, and the screen it draws on.
    pub fn mode(&self) -> TermMode {
        *self.screen.lock().mode()
    }

    /// The cursor as the program last styled it: its shape, and whether it
    /// blinks. A hollow block for one it never styled.
    pub fn cursor_style(&self) -> (CursorShape, bool) {
        let style = self.screen.lock().cursor_style();
        (style.shape, style.blinking)
    }

    /// Draws the screen into `area` of `buffer`, and says where the cursor is
    /// on it, if it shows.
    pub fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        // Cleared before the screen is read, so that what the program draws
        // from here on sends a `Said::Drew` of its own.
        self.drew.store(false, Ordering::SeqCst);
        paint(&self.screen.lock(), buffer, area)
    }

    /// What the screen shows, as text.
    pub fn text(&self) -> String {
        text(&self.screen.lock())
    }

    /// Whether the screen shows nothing: each cell of it as blank as the
    /// program found it.
    pub fn blank(&self) -> bool {
        let screen = self.screen.lock();
        screen.renderable_content().display_iter.all(|indexed| indexed.cell.is_empty())
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // The reader is gone with the program already.
        let _ = self.pty.send(Msg::Shutdown);
    }
}

/// Draws `screen` into `area` of `buffer`, and says where its cursor is on
/// it, if it shows.
fn paint<T: EventListener>(screen: &Term<T>, buffer: &mut Buffer, area: Rect) -> Option<Position> {
    let content = screen.renderable_content();
    // luneta never scrolls the screen back, and keeps no scrollback for it to
    // go back into, as zellij keeps each pane's: its lines count from 0.
    assert_eq!(content.display_offset, 0);
    let at = |line: i32, column: usize| {
        let row = u16::try_from(line).ok().filter(|row| *row < area.height)?;
        let column = u16::try_from(column).ok().filter(|column| *column < area.width)?;
        Some(Position::new(area.x + column, area.y + row))
    };
    for indexed in content.display_iter {
        let Some(position) = at(indexed.point.line.0, indexed.point.column.0) else {
            continue;
        };
        let Some(drawn) = buffer.cell_mut(position) else {
            continue;
        };
        let cell = indexed.cell;
        // The second column of a wide character is drawn with it, from the
        // first.
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            drawn.reset();
            continue;
        }
        let mut symbol = String::from(cell.c);
        symbol.extend(cell.zerowidth().into_iter().flatten());
        drawn.set_symbol(&symbol);
        drawn.set_style(style(cell.fg, cell.bg, cell.flags, cell.underline_color()));
    }
    if content.cursor.shape == CursorShape::Hidden {
        return None;
    }
    at(content.cursor.point.line.0, content.cursor.point.column.0)
}

/// What `screen` shows, as text: each of its lines, less the blanks it ends
/// in, and none of the blank lines above and below the rest.
fn text<T: EventListener>(screen: &Term<T>) -> String {
    let mut lines = vec![String::new(); screen.screen_lines()];
    for indexed in screen.renderable_content().display_iter {
        let cell = indexed.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let line = usize::try_from(indexed.point.line.0).ok();
        let Some(line) = line.and_then(|line| lines.get_mut(line)) else {
            continue;
        };
        line.push(cell.c);
        line.extend(cell.zerowidth().into_iter().flatten());
    }
    let lines: Vec<&str> = lines.iter().map(|line| line.trim_end()).collect();
    let said = |line: &&str| !line.is_empty();
    let (Some(first), Some(last)) = (lines.iter().position(said), lines.iter().rposition(said))
    else {
        return String::new();
    };
    lines[first..=last].join("\n")
}

/// The style a cell is drawn in. The colours a terminal names are left for
/// luneta's terminal to choose, as its theme does, and so are the program's
/// own foreground and background.
fn style(fg: Colour, bg: Colour, flags: Flags, underline: Option<Colour>) -> Style {
    let (fg, dim_fg) = colour(fg);
    let (bg, _) = colour(bg);
    let mut style = Style::new().fg(fg).bg(bg);
    if let Some(underline) = underline {
        style = style.underline_color(colour(underline).0);
    }
    let modifiers = [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::HIDDEN, Modifier::HIDDEN),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ];
    for (flag, modifier) in modifiers {
        if flags.intersects(flag) {
            style = style.add_modifier(modifier);
        }
    }
    if dim_fg {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

// `colour` reads a named colour as its index: the sixteen a terminal names are
// 0 to 15, and the eight dim ones are in the order of the first eight.
const _: () = assert!(NamedColor::Black as usize == 0);
const _: () = assert!(NamedColor::BrightWhite as usize == 15);
const _: () = assert!(
    NamedColor::DimWhite as usize - NamedColor::DimBlack as usize
        == NamedColor::White as usize - NamedColor::Black as usize
);

/// A colour as luneta's terminal is told it, and whether it is a dim one of
/// the sixteen, which a terminal draws as that colour, dimmed.
fn colour(colour: Colour) -> (Color, bool) {
    match colour {
        Colour::Spec(rgb) => (Color::Rgb(rgb.r, rgb.g, rgb.b), false),
        Colour::Indexed(index) => (Color::Indexed(index), false),
        Colour::Named(named) => {
            let index = named as usize;
            if let Ok(index @ 0..=15) = u8::try_from(index) {
                return (Color::Indexed(index), false);
            }
            let dim = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            if dim.contains(&index) {
                let index = u8::try_from(index - NamedColor::DimBlack as usize)
                    .expect("one of the eight dim colours");
                return (Color::Indexed(index), true);
            }
            (Color::Reset, named == NamedColor::DimForeground)
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;
    use crate::answers::{Answer, Colours};

    /// A screen of `columns` by `lines` that has read `bytes`, drawn into a
    /// buffer two columns and a line in from its corner.
    fn drawn(columns: u16, lines: u16, bytes: &[u8]) -> (Buffer, Option<Position>) {
        let dimensions = TermSize::new(usize::from(columns), usize::from(lines));
        let mut screen = Term::new(Config::default(), &dimensions, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, bytes);
        let mut buffer = Buffer::empty(Rect::new(0, 0, columns + 4, lines + 2));
        let cursor = paint(&screen, &mut buffer, Rect::new(2, 1, columns, lines));
        (buffer, cursor)
    }

    fn line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    #[test]
    fn the_screen_is_drawn_where_its_area_starts() {
        let (buffer, cursor) = drawn(6, 2, b"ab\r\ncd");
        assert_eq!(line(&buffer, 1), "  ab      ");
        assert_eq!(line(&buffer, 2), "  cd      ");
        assert_eq!(cursor, Some(Position::new(4, 2)));
    }

    #[test]
    fn a_wide_character_takes_two_columns_and_draws_once() {
        let (buffer, _) = drawn(4, 1, "日x".as_bytes());
        assert_eq!(buffer[(2, 1)].symbol(), "日");
        assert_eq!(buffer[(4, 1)].symbol(), "x");
    }

    #[test]
    fn a_hidden_cursor_is_not_drawn() {
        let (_, cursor) = drawn(4, 1, b"\x1b[?25l");
        assert_eq!(cursor, None);
    }

    #[test]
    fn the_colours_a_terminal_names_are_left_to_its_theme() {
        let (buffer, _) = drawn(4, 1, b"\x1b[31;44ma\x1b[38;5;200mb\x1b[38;2;1;2;3mc\x1b[0md");
        assert_eq!(buffer[(2, 1)].style().fg, Some(Color::Indexed(1)));
        assert_eq!(buffer[(2, 1)].style().bg, Some(Color::Indexed(4)));
        assert_eq!(buffer[(3, 1)].style().fg, Some(Color::Indexed(200)));
        assert_eq!(buffer[(4, 1)].style().fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(buffer[(5, 1)].style().fg, Some(Color::Reset));
    }

    /// What a program printed as it ended reads as the text it was, as
    /// zellij's error where it cannot start.
    #[test]
    fn the_screen_reads_as_its_text() {
        let dimensions = TermSize::new(12, 4);
        let mut screen = Term::new(Config::default(), &dimensions, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, "\r\n  \x1b[31m×\x1b[0m bad  \r\n日本\r\n".as_bytes());
        assert_eq!(text(&screen), "  × bad\n日本");
        let blank = Term::new(Config::default(), &dimensions, VoidListener);
        assert_eq!(text(&blank), "");
    }

    /// zellij hands on what a program in a pane asks of its terminal, and asks
    /// what the terminal is after it, to know when the answers are all in: a
    /// colour is asked for in its place in the loop's queue, and answered
    /// there where luneta's terminal said it.
    #[test]
    fn a_colour_is_answered_where_lunetas_terminal_said_it_and_before_what_it_is() {
        let (events, inbox) = mpsc::channel();
        let listener = Listener { events, drew: Arc::default() };
        let mut screen = Term::new(Config::default(), &TermSize::new(4, 1), listener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        // The background, the foreground, the second of the sixteen, and what
        // the terminal is.
        parser.advance(&mut screen, b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]4;1;?\x1b\\\x1b[c");
        let mut colours = Colours::default();
        colours.hear(Answer::Background(Rgb { r: 0x1e, g: 0x1e, b: 0x2e }));
        let answers: Vec<String> = inbox
            .try_iter()
            .filter_map(|event| match event {
                Event::Hosted(Said::AskedColour(index, spell)) => colours.answer(index, &*spell),
                Event::Hosted(Said::Answer(answer)) => Some(answer),
                _ => None,
            })
            .collect();
        assert_eq!(answers, ["\x1b]11;rgb:1e1e/1e1e/2e2e\x07", "\x1b[?6c"]);
    }

    #[test]
    fn bold_and_underline_carry_over() {
        let (buffer, _) = drawn(2, 1, b"\x1b[1;4ma");
        let modifiers = buffer[(2, 1)].style().add_modifier;
        assert!(modifiers.contains(Modifier::BOLD | Modifier::UNDERLINED));
    }
}
