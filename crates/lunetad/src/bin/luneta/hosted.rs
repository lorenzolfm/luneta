use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::num::NonZeroU16;
use std::os::unix::ffi::OsStrExt as _;
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
use rustix::fs::{Mode, OFlags};
use rustix::termios::{self, LocalModes};

use crate::host::Event;

pub enum Said {
    Drew,
    Titled(String),
    Copied(ClipboardType, String),
    Answer(String),
    AskedSize(Arc<dyn Fn(WindowSize) -> String + Sync + Send>),
    AskedColour(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send>),
    Rang,
    Exited(ExitStatus),
    Ended,
}

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

#[derive(Clone)]
struct Listener {
    events: Sender<Event>,
    number: u64,
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
            Emitted::ColorRequest(index, answer) => Said::AskedColour(index, answer),
            Emitted::Bell => Said::Rang,
            Emitted::ChildExit(status) => Said::Exited(status),
            Emitted::Exit => Said::Ended,
            Emitted::ClipboardLoad(..)
            | Emitted::ResetTitle
            | Emitted::CursorBlinkingChange
            | Emitted::MouseCursorDirty => return,
        };
        let _ = self.events.send(Event::Hosted(self.number, said));
    }
}

pub struct Hosted {
    screen: Arc<FairMutex<Term<Listener>>>,
    pty: EventLoopSender,
    modes: File,
    _far: File,
    number: u64,
    size: Size,
    drew: Arc<AtomicBool>,
}

impl Hosted {
    pub fn start(
        program: String,
        args: Vec<String>,
        dir: &Path,
        size: Size,
        number: u64,
        events: Sender<Event>,
    ) -> io::Result<Self> {
        let options = tty::Options {
            shell: Some(tty::Shell::new(program, args)),
            working_directory: Some(dir.to_owned()),
            drain_on_exit: true,
            env: HashMap::new(),
        };
        let pty = tty::new(&options, size.into(), 0)?;
        let modes = pty.file().try_clone()?;
        let far = far_end(pty.file())?;
        let drew = Arc::new(AtomicBool::new(false));
        let listener = Listener { events, number, drew: Arc::clone(&drew) };
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
        Ok(Hosted { screen, pty, modes, _far: far, number, size, drew })
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn resize(&mut self, size: Size) {
        if (size.columns, size.lines) == (self.size.columns, self.size.lines) {
            self.size = size;
            return;
        }
        self.size = size;
        self.screen.lock().resize(size.dimensions());
        let _ = self.pty.send(Msg::Resize(size.into()));
    }

    pub fn send(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if bytes.is_empty() {
            return;
        }
        let _ = self.pty.send(Msg::Input(bytes));
    }

    pub fn answer(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let modes = termios::tcgetattr(&self.modes);
        if modes.is_ok_and(|modes| modes.local_modes.contains(LocalModes::ECHO)) {
            return;
        }
        self.send(bytes);
    }

    pub fn mode(&self) -> TermMode {
        *self.screen.lock().mode()
    }

    pub fn cursor_style(&self) -> (CursorShape, bool) {
        let style = self.screen.lock().cursor_style();
        (style.shape, style.blinking)
    }

    pub fn draw(&self, buffer: &mut Buffer, area: Rect) -> Option<Position> {
        self.drew.store(false, Ordering::SeqCst);
        paint(&self.screen.lock(), buffer, area)
    }

    pub fn text(&self) -> String {
        text(&self.screen.lock())
    }

    pub fn blank(&self) -> bool {
        let screen = self.screen.lock();
        screen.renderable_content().display_iter.all(|indexed| indexed.cell.is_empty())
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        let _ = self.pty.send(Msg::Shutdown);
    }
}

fn far_end(near: &File) -> io::Result<File> {
    let path = rustix::pty::ptsname(near, Vec::new())?;
    let path = Path::new(OsStr::from_bytes(path.as_bytes()));
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

fn paint<T: EventListener>(screen: &Term<T>, buffer: &mut Buffer, area: Rect) -> Option<Position> {
    show(screen, buffer, area, 0);
    let cursor = screen.renderable_content().cursor;
    if cursor.shape == CursorShape::Hidden {
        return None;
    }
    within(area, cursor.point.line.0, cursor.point.column.0)
}

fn within(area: Rect, line: i32, column: usize) -> Option<Position> {
    let row = u16::try_from(line).ok().filter(|row| *row < area.height)?;
    let column = u16::try_from(column).ok().filter(|column| *column < area.width)?;
    Some(Position::new(area.x + column, area.y + row))
}

pub fn show<T: EventListener>(screen: &Term<T>, buffer: &mut Buffer, area: Rect, top: usize) {
    let content = screen.renderable_content();
    assert_eq!(content.display_offset, 0);
    let top = i32::try_from(top).unwrap_or(i32::MAX);
    for indexed in content.display_iter {
        let line = indexed.point.line.0.saturating_sub(top);
        let Some(position) = within(area, line, indexed.point.column.0) else {
            continue;
        };
        let Some(drawn) = buffer.cell_mut(position) else {
            continue;
        };
        let cell = indexed.cell;
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            drawn.reset();
            continue;
        }
        let mut symbol = String::from(cell.c);
        symbol.extend(cell.zerowidth().into_iter().flatten());
        drawn.set_symbol(&symbol);
        drawn.set_style(style(cell.fg, cell.bg, cell.flags, cell.underline_color()));
    }
}

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

const _: () = assert!(NamedColor::Black as usize == 0);
const _: () = assert!(NamedColor::BrightWhite as usize == 15);
const _: () = assert!(
    NamedColor::DimWhite as usize - NamedColor::DimBlack as usize
        == NamedColor::White as usize - NamedColor::Black as usize
);

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
    use std::thread;
    use std::time::{Duration, Instant};

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    use super::*;
    use crate::answers::{Answer, Colours};

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
    fn the_screen_reads_as_its_text() {
        let dimensions = TermSize::new(12, 4);
        let mut screen = Term::new(Config::default(), &dimensions, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, "\r\n  \x1b[31m×\x1b[0m bad  \r\n日本\r\n".as_bytes());
        assert_eq!(text(&screen), "  × bad\n日本");
        let blank = Term::new(Config::default(), &dimensions, VoidListener);
        assert_eq!(text(&blank), "");
    }

    #[test]
    fn a_colour_is_answered_where_lunetas_terminal_said_it_and_before_what_it_is() {
        let (events, inbox) = mpsc::channel();
        let listener = Listener { events, number: 1, drew: Arc::default() };
        let mut screen = Term::new(Config::default(), &TermSize::new(4, 1), listener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, b"\x1b]11;?\x07\x1b]10;?\x1b\\\x1b]4;1;?\x1b\\\x1b[c");
        let mut colours = Colours::default();
        colours.hear(Answer::Background(Rgb { r: 0x1e, g: 0x1e, b: 0x2e }));
        let answers: Vec<String> = inbox
            .try_iter()
            .filter_map(|event| match event {
                Event::Hosted(_, Said::AskedColour(index, spell)) => colours.answer(index, &*spell),
                Event::Hosted(_, Said::Answer(answer)) => Some(answer),
                _ => None,
            })
            .collect();
        assert_eq!(answers, ["\x1b]11;rgb:1e1e/1e1e/2e2e\x07", "\x1b[?6c"]);
    }

    #[test]
    fn what_a_program_says_as_it_fails_at_once_is_its_text() {
        let (events, inbox) = mpsc::channel();
        let size = Size {
            columns: NonZeroU16::new(40).unwrap(),
            lines: NonZeroU16::new(4).unwrap(),
            cell_width: 0,
            cell_height: 0,
        };
        let args = vec!["-c".to_owned(), "echo 'ssh: no such machine' >&2; exit 255".to_owned()];
        let hosted =
            Hosted::start("/bin/sh".to_owned(), args, Path::new("/"), size, 1, events).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let drawing = hosted.screen.lock();
        thread::sleep(Duration::from_millis(300));
        drop(drawing);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Event::Hosted(_, Said::Ended) = inbox.recv_timeout(left).unwrap() {
                break;
            }
        }
        assert_eq!(hosted.text(), "ssh: no such machine");
    }

    fn answered(stty: &str) -> String {
        let (events, inbox) = mpsc::channel();
        let size = Size {
            columns: NonZeroU16::new(20).unwrap(),
            lines: NonZeroU16::new(2).unwrap(),
            cell_width: 0,
            cell_height: 0,
        };
        let program = format!(
            r#"stty {stty}; printf ready; read -r line; printf '[%s]' "$line" | tr '\033' E"#
        );
        let args = vec!["-c".to_owned(), program];
        let hosted =
            Hosted::start("/bin/sh".to_owned(), args, Path::new("/"), size, 1, events).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while hosted.text() != "ready" {
            assert!(Instant::now() < deadline, "the program printed {:?}", hosted.text());
            thread::sleep(Duration::from_millis(10));
        }
        hosted.answer(&b"\x1b[?6c"[..]);
        hosted.send(&b"typed\n"[..]);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if let Event::Hosted(_, Said::Ended) = inbox.recv_timeout(left).unwrap() {
                break;
            }
        }
        hosted.text()
    }

    #[test]
    fn an_answer_to_a_terminal_that_echoes_is_left_out() {
        assert_eq!(answered("echo"), "readytyped\n[typed]");
    }

    #[test]
    fn an_answer_to_a_terminal_that_echoes_nothing_goes_in() {
        assert_eq!(answered("-echo"), "ready[E[?6ctyped]");
    }

    #[test]
    fn bold_and_underline_carry_over() {
        let (buffer, _) = drawn(2, 1, b"\x1b[1;4ma");
        let modifiers = buffer[(2, 1)].style().add_modifier;
        assert!(modifiers.contains(Modifier::BOLD | Modifier::UNDERLINED));
    }
}
