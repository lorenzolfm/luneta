//! The program luneta runs beside its sidebar, zellij's client, in a terminal
//! of luneta's own: a pseudoterminal it runs on, and a screen that keeps what
//! it draws there, as a terminal emulator does. luneta draws that screen into
//! its part of the terminal luneta itself runs in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use alacritty_terminal::event::{Event as Emitted, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{ClipboardType, Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color as Colour, CursorShape, CursorStyle, NamedColor};
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
    /// It rang the bell.
    Rang,
    /// It ended.
    Ended,
}

/// Hands what the program does to luneta's loop.
#[derive(Clone)]
struct Listener(Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Emitted) {
        let said = match event {
            Emitted::Wakeup => Said::Drew,
            Emitted::Title(title) => Said::Titled(title),
            Emitted::ClipboardStore(clipboard, text) => Said::Copied(clipboard, text),
            Emitted::PtyWrite(answer) => Said::Answer(answer),
            Emitted::TextAreaSizeRequest(answer) => Said::AskedSize(answer),
            Emitted::Bell => Said::Rang,
            Emitted::ChildExit(_) | Emitted::Exit => Said::Ended,
            // The terminal's colours are luneta's terminal's, which luneta
            // does not know; a program that asks goes without an answer. The
            // rest is of the window a terminal emulator draws in.
            Emitted::ColorRequest(..)
            | Emitted::ClipboardLoad(..)
            | Emitted::ResetTitle
            | Emitted::CursorBlinkingChange
            | Emitted::MouseCursorDirty => return,
        };
        // luneta is on its way out.
        let _ = self.0.send(Event::Hosted(said));
    }
}

/// The program, and the terminal luneta runs it in.
pub struct Hosted {
    screen: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    size: WindowSize,
}

impl Hosted {
    /// Starts `program` in a terminal of `size`, in `dir`.
    pub fn start(
        program: String,
        args: Vec<String>,
        dir: &Path,
        size: WindowSize,
        events: Sender<Event>,
    ) -> io::Result<Self> {
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: Some(dir.to_owned()),
            drain_on_exit: false,
            env: HashMap::new(),
        };
        let pty = tty::new(&options, size, 0)?;
        let listener = Listener(events);
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
        let dimensions = TermSize::new(usize::from(size.num_cols), usize::from(size.num_lines));
        let screen = Arc::new(FairMutex::new(Term::new(config, &dimensions, listener.clone())));
        let reader = EventLoop::new(Arc::clone(&screen), listener, pty, false, false)?;
        let pty = reader.channel();
        reader.spawn();
        Ok(Hosted { screen, pty, size })
    }

    pub fn size(&self) -> WindowSize {
        self.size
    }

    /// Makes the terminal `size`, and tells the program.
    pub fn resize(&mut self, size: WindowSize) {
        if (size.num_cols, size.num_lines) == (self.size.num_cols, self.size.num_lines) {
            self.size = size;
            return;
        }
        self.size = size;
        let dimensions = TermSize::new(usize::from(size.num_cols), usize::from(size.num_lines));
        self.screen.lock().resize(dimensions);
        // The reader is gone with the program, which needs no size.
        let _ = self.pty.send(Msg::Resize(size));
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
        paint(&self.screen.lock(), buffer, area)
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
    let offset = content.display_offset as i32;
    let at = |line: i32, column: usize| {
        let row = u16::try_from(line + offset).ok().filter(|row| *row < area.height)?;
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
                let index = index - NamedColor::DimBlack as usize;
                return (Color::Indexed(u8::try_from(index).unwrap_or(7)), true);
            }
            (Color::Reset, named == NamedColor::DimForeground)
        },
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;

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

    #[test]
    fn bold_and_underline_carry_over() {
        let (buffer, _) = drawn(2, 1, b"\x1b[1;4ma");
        let modifiers = buffer[(2, 1)].style().add_modifier;
        assert!(modifiers.contains(Modifier::BOLD | Modifier::UNDERLINED));
    }
}
