//! PROTOTYPE. A client: it draws the grid the server sends into its own
//! terminal with ratatui, and sends keys encoded for the modes the pane's
//! program asked for. `Ctrl ]` then `d` detaches, `s` toggles the status line,
//! `Ctrl ]` again sends it. `Shift PageUp`/`PageDown` and the wheel scroll back.

use std::io::{self, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    self, BeginSynchronizedUpdate, EndSynchronizedUpdate, EnterAlternateScreen,
    LeaveAlternateScreen, SetTitle,
};
use crossterm::{execute, queue};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Position;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;

use crate::keys;
use crate::wire::{self, Cell, Colour, Rgb, Size, ToClient, ToServer};

enum Event {
    Input(Input),
    Server(ToClient, usize),
    Gone,
}

#[derive(Default)]
struct View {
    columns: u16,
    lines: u16,
    grid: Vec<Vec<Cell>>,
    mode: TermMode,
    cursor: Option<(u16, u16)>,
    shape: u8,
    blinking: bool,
    offset: u32,
    history: u32,
    read_us: u32,
}

struct Stats {
    typed_at: Option<Instant>,
    latency: Option<Duration>,
    worst: Duration,
    window: Instant,
    window_bytes: usize,
    rate: usize,
    rows: usize,
    frames: u64,
}

pub fn connect(socket: &Path, log: &Path) -> io::Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket) {
        return Ok(stream);
    }
    let errors = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    Command::new(std::env::current_exe()?)
        .arg("server")
        .env_remove("ZELLIJ")
        .env_remove("ZELLIJ_SESSION_NAME")
        .env_remove("ZELLIJ_PANE_ID")
        .env_remove("TERM_PROGRAM")
        .env_remove("TERM_PROGRAM_VERSION")
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(errors)
        .spawn()?;
    for _ in 0..300 {
        thread::sleep(Duration::from_millis(10));
        if let Ok(stream) = UnixStream::connect(socket) {
            return Ok(stream);
        }
    }
    Err(io::Error::other("the server did not start"))
}

pub fn attach(socket: &Path, log: &Path, name: &str, command: Vec<String>) -> io::Result<()> {
    let mut stream = connect(socket, log)?;
    terminal::enable_raw_mode()?;
    let (fg, bg) = ask_colours();
    let kitty = terminal::supports_keyboard_enhancement().unwrap_or(false);
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
    if kitty {
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
        execute!(io::stdout(), PushKeyboardEnhancementFlags(flags))?;
    }
    let ended = run(&mut stream, name, command, fg, bg);
    if kitty {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        io::stdout(),
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        SetCursorStyle::DefaultUserShape,
        crossterm::cursor::Show
    );
    let _ = terminal::disable_raw_mode();
    println!("{}", ended?);
    Ok(())
}

fn size(status: bool) -> io::Result<Size> {
    let window = terminal::window_size()?;
    let lines = window.rows.saturating_sub(u16::from(status)).max(1);
    let cell = |pixels: u16, cells: u16| if cells == 0 { 0 } else { pixels / cells };
    Ok(Size {
        columns: window.columns.max(1),
        lines,
        cell_width: cell(window.width, window.columns),
        cell_height: cell(window.height, window.rows),
    })
}

fn run(stream: &mut UnixStream, name: &str, command: Vec<String>, fg: Option<Rgb>, bg: Option<Rgb>) -> io::Result<String> {
    let mut status = true;
    let dir = std::env::current_dir()?.to_string_lossy().into_owned();
    ToServer::Hello { session: name.to_owned(), command, dir, size: size(status)?, fg, bg }.send(stream)?;
    // Before the input thread, which holds crossterm's reader: ratatui asks
    // where the cursor is as it starts.
    let mut screen = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let (events, inbox) = mpsc::channel();
    {
        let events = events.clone();
        let mut stream = stream.try_clone()?;
        thread::spawn(move || loop {
            match ToClient::receive(&mut stream) {
                Ok((message, bytes)) => { if events.send(Event::Server(message, bytes)).is_err() { return; } },
                Err(_) => { let _ = events.send(Event::Gone); return; },
            }
        });
    }
    thread::spawn(move || {
        while let Ok(input) = event::read() {
            if events.send(Event::Input(input)).is_err() {
                return;
            }
        }
    });
    screen.clear()?;
    let mut view = View::default();
    let mut stats = Stats {
        typed_at: None,
        latency: None,
        worst: Duration::ZERO,
        window: Instant::now(),
        window_bytes: 0,
        rate: 0,
        rows: 0,
        frames: 0,
    };
    let mut prefix = false;
    let mut cursor_style = (255, false);
    loop {
        let event = inbox.recv().map_err(|_| io::Error::other("inbox closed"))?;
        let send = |stream: &mut UnixStream, stats: &mut Stats, bytes: Option<Vec<u8>>| -> io::Result<()> {
            if let Some(bytes) = bytes.filter(|b| !b.is_empty()) {
                stats.typed_at.get_or_insert_with(Instant::now);
                ToServer::Input(bytes).send(stream)?;
            }
            Ok(())
        };
        match event {
            Event::Gone => return Ok("[the server is gone]".to_owned()),
            Event::Server(ToClient::Ended(code), _) => return Ok(format!("[session {name} ended, exit {code:?}]")),
            Event::Server(ToClient::Title(title), _) => execute!(io::stdout(), SetTitle(title))?,
            Event::Server(ToClient::Bell, _) => { io::stdout().write_all(b"\x07")?; io::stdout().flush()?; },
            Event::Server(ToClient::Clipboard(text), _) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(text);
                write!(io::stdout(), "\x1b]52;c;{encoded}\x07")?;
                io::stdout().flush()?;
            },
            Event::Server(ToClient::Sessions(_), _) => {},
            Event::Server(ToClient::Frame(frame), bytes) => {
                let now = Instant::now();
                if let Some(typed) = stats.typed_at.take() {
                    let latency = now - typed;
                    stats.latency = Some(latency);
                    stats.worst = stats.worst.max(latency);
                }
                stats.frames += 1;
                stats.rows = frame.rows.len();
                stats.window_bytes += bytes;
                if now - stats.window >= Duration::from_secs(1) {
                    stats.rate = stats.window_bytes;
                    stats.window_bytes = 0;
                    stats.window = now;
                }
                if frame.whole {
                    view.grid = vec![Vec::new(); usize::from(frame.lines)];
                }
                for (line, cells) in frame.rows {
                    if let Some(row) = view.grid.get_mut(usize::from(line)) {
                        *row = cells;
                    }
                }
                view.columns = frame.columns;
                view.lines = frame.lines;
                view.mode = TermMode::from_bits_truncate(frame.mode);
                view.cursor = frame.cursor;
                view.shape = frame.shape;
                view.blinking = frame.blinking;
                view.offset = frame.offset;
                view.history = frame.history;
                view.read_us = frame.read_us;
                if (view.shape, view.blinking) != cursor_style {
                    cursor_style = (view.shape, view.blinking);
                    execute!(io::stdout(), style_of(view.shape, view.blinking))?;
                }
            },
            Event::Input(Input::Key(key)) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                let is_prefix = key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char(']' | '5'));
                if prefix {
                    prefix = false;
                    match key.code {
                        KeyCode::Char('d') => return Ok(format!("[detached from {name}]")),
                        KeyCode::Char('s') => {
                            status = !status;
                            stats.worst = Duration::ZERO;
                            ToServer::Resize(size(status)?).send(stream)?;
                        },
                        _ if is_prefix => send(stream, &mut stats, Some(vec![0x1d]))?,
                        _ => {},
                    }
                    continue;
                }
                if is_prefix {
                    prefix = true;
                    continue;
                }
                let half = i32::from(view.lines / 2).max(1);
                match (key.code, key.modifiers) {
                    (KeyCode::PageUp, KeyModifiers::SHIFT) => ToServer::Scroll(half).send(stream).map(drop)?,
                    (KeyCode::PageDown, KeyModifiers::SHIFT) => ToServer::Scroll(-half).send(stream).map(drop)?,
                    _ => send(stream, &mut stats, keys::key(&key, view.mode))?,
                }
            },
            Event::Input(Input::Mouse(mouse)) => {
                if mouse.row >= view.lines || mouse.column >= view.columns {
                    continue;
                }
                let wheel = match mouse.kind {
                    MouseEventKind::ScrollUp => 3,
                    MouseEventKind::ScrollDown => -3,
                    _ => 0,
                };
                let local = !view.mode.intersects(TermMode::MOUSE_MODE)
                    && !view.mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL);
                if wheel != 0 && (local || view.offset > 0) {
                    ToServer::Scroll(wheel).send(stream)?;
                } else {
                    send(stream, &mut stats, keys::mouse(mouse, mouse.column, mouse.row, view.mode))?;
                }
            },
            Event::Input(Input::Paste(text)) => send(stream, &mut stats, Some(keys::paste(&text, view.mode)))?,
            Event::Input(Input::FocusGained) => send(stream, &mut stats, keys::focus(true, view.mode).map(<[u8]>::to_vec))?,
            Event::Input(Input::FocusLost) => send(stream, &mut stats, keys::focus(false, view.mode).map(<[u8]>::to_vec))?,
            Event::Input(Input::Resize(..)) => ToServer::Resize(size(status)?).send(stream).map(drop)?,
        }
        draw(&mut screen, &view, &stats, name, status, prefix)?;
    }
}

fn draw(screen: &mut Terminal<CrosstermBackend<io::Stdout>>, view: &View, stats: &Stats, name: &str, status: bool, prefix: bool) -> io::Result<()> {
    queue!(screen.backend_mut(), BeginSynchronizedUpdate)?;
    screen.draw(|frame| {
        let area = frame.area();
        let buffer = frame.buffer_mut();
        let pane_lines = area.height.saturating_sub(u16::from(status));
        for (y, row) in view.grid.iter().enumerate().take(usize::from(pane_lines)) {
            for (x, cell) in row.iter().enumerate().take(usize::from(area.width)) {
                let Some(drawn) = buffer.cell_mut(Position::new(x as u16, y as u16)) else { continue };
                let flags = Flags::from_bits_truncate(cell.flags & !wire::DIM_FG);
                if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    drawn.reset();
                    continue;
                }
                let mut symbol = String::from(cell.c);
                symbol.extend(cell.zerowidth.iter());
                drawn.set_symbol(&symbol);
                drawn.set_style(style(cell, flags));
            }
        }
        if status {
            let line = status_line(view, stats, name, prefix);
            let y = area.height.saturating_sub(1);
            buffer.set_stringn(0, y, format!("{line:<width$}", width = usize::from(area.width)), usize::from(area.width), Style::new().add_modifier(Modifier::REVERSED));
        }
        if let Some((line, column)) = view.cursor
            && line < pane_lines
        {
            frame.set_cursor_position(Position::new(column, line));
        }
    })?;
    execute!(screen.backend_mut(), EndSynchronizedUpdate)?;
    Ok(())
}

fn status_line(view: &View, stats: &Stats, name: &str, prefix: bool) -> String {
    let m = view.mode;
    let mut modes = Vec::new();
    for (flag, label) in [
        (TermMode::ALT_SCREEN, "alt"),
        (TermMode::APP_CURSOR, "appcur"),
        (TermMode::BRACKETED_PASTE, "paste"),
        (TermMode::FOCUS_IN_OUT, "focus"),
        (TermMode::SGR_MOUSE, "sgr"),
        (TermMode::MOUSE_REPORT_CLICK, "click"),
        (TermMode::MOUSE_DRAG, "drag"),
        (TermMode::MOUSE_MOTION, "motion"),
    ] {
        if m.contains(flag) {
            modes.push(label.to_owned());
        }
    }
    let kitty = (m & TermMode::KITTY_KEYBOARD_PROTOCOL).bits() >> 18;
    if kitty != 0 {
        modes.push(format!("kitty{kitty}"));
    }
    let ms = |d: Duration| format!("{:.1}ms", d.as_secs_f64() * 1000.0);
    let latency = stats.latency.map_or_else(|| "-".to_owned(), ms);
    let prefix = if prefix { " │ C-] …" } else { "" };
    format!(
        " proto-mux │ {name} │ {}x{} │ {} │ key→frame {latency} (worst {}) │ {}KB/s {} rows #{} │ read {}µs │ back {}/{}{prefix} │ C-] d detach, s status",
        view.columns,
        view.lines,
        if modes.is_empty() { "-".to_owned() } else { modes.join(" ") },
        ms(stats.worst),
        stats.rate / 1024,
        stats.rows,
        stats.frames,
        view.read_us,
        view.offset,
        view.history,
    )
}

fn style(cell: &Cell, flags: Flags) -> Style {
    let mut style = Style::new().fg(colour(cell.fg)).bg(colour(cell.bg));
    if let Some(underline) = cell.underline {
        style = style.underline_color(colour(underline));
    }
    for (flag, modifier) in [
        (Flags::BOLD, Modifier::BOLD),
        (Flags::DIM, Modifier::DIM),
        (Flags::ITALIC, Modifier::ITALIC),
        (Flags::ALL_UNDERLINES, Modifier::UNDERLINED),
        (Flags::INVERSE, Modifier::REVERSED),
        (Flags::HIDDEN, Modifier::HIDDEN),
        (Flags::STRIKEOUT, Modifier::CROSSED_OUT),
    ] {
        if flags.intersects(flag) {
            style = style.add_modifier(modifier);
        }
    }
    if cell.flags & wire::DIM_FG != 0 {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

fn colour(colour: Colour) -> Color {
    match colour {
        Colour::Reset => Color::Reset,
        Colour::Indexed(i) => Color::Indexed(i),
        Colour::Rgb(Rgb(r, g, b)) => Color::Rgb(r, g, b),
    }
}

fn style_of(shape: u8, blinking: bool) -> SetCursorStyle {
    match (shape, blinking) {
        (1, true) => SetCursorStyle::BlinkingUnderScore,
        (1, false) => SetCursorStyle::SteadyUnderScore,
        (2, true) => SetCursorStyle::BlinkingBar,
        (2, false) => SetCursorStyle::SteadyBar,
        (0, true) => SetCursorStyle::BlinkingBlock,
        (0, false) => SetCursorStyle::SteadyBlock,
        _ => SetCursorStyle::DefaultUserShape,
    }
}

/// The foreground and background of this terminal (OSC 10 and 11), asked in
/// raw mode before crossterm reads anything, until it says what it is.
fn ask_colours() -> (Option<Rgb>, Option<Rgb>) {
    let mut out = io::stdout();
    if out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c").and_then(|()| out.flush()).is_err() {
        return (None, None);
    }
    let stdin = io::stdin();
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut said = Vec::new();
    while !device(&said) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let timeout = rustix::event::Timespec { tv_sec: 0, tv_nsec: left.as_nanos() as i64 };
        let fd = stdin.as_fd();
        let mut fds = [rustix::event::PollFd::new(&fd, rustix::event::PollFlags::IN)];
        if rustix::event::poll(&mut fds, Some(&timeout)).unwrap_or(0) == 0 {
            break;
        }
        let mut buffer = [0; 256];
        match stdin.lock().read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => said.extend_from_slice(&buffer[..n]),
        }
    }
    (osc(&said, b"\x1b]10;"), osc(&said, b"\x1b]11;"))
}

fn device(said: &[u8]) -> bool {
    let Some(at) = said.windows(3).position(|w| w == b"\x1b[?") else { return false };
    said[at..].contains(&b'c')
}

fn osc(said: &[u8], start: &[u8]) -> Option<Rgb> {
    let at = said.windows(start.len()).position(|w| w == start)? + start.len();
    let rest = &said[at..];
    let end = rest.iter().position(|b| *b == 0x07 || *b == 0x1b)?;
    let channels = std::str::from_utf8(&rest[..end]).ok()?.strip_prefix("rgb:")?;
    let channels: Vec<u8> = channels
        .split('/')
        .map(|d| {
            let value = u32::from_str_radix(d, 16).ok()?;
            let most = (1u32 << (4 * d.len())) - 1;
            u8::try_from(value * 255 / most).ok()
        })
        .collect::<Option<_>>()?;
    let [r, g, b] = channels[..] else { return None };
    Some(Rgb(r, g, b))
}
