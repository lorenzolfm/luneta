//! luneta's own terminal: the sidebar on the left, zellij on the right, and
//! which of the two has the keys. `Ctrl b` hands them from one to the other,
//! and a click hands them to the one clicked.

use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::CursorShape;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::SetTitle;
use lunetad::api::Host;
use ratatui::layout::{Position, Rect};
use ratatui::DefaultTerminal;

use crate::hosted::{Hosted, Said, Size};
use crate::keys;
use crate::sidebar::{self, Asked, Place, Sidebar};
use crate::zellij::{self, SessionName};

/// What luneta's loop acts on, from the threads that wait on each source.
pub enum Event {
    /// A key, the mouse, a paste or a resize of luneta's terminal.
    Input(Input),
    /// luneta's terminal cannot be read any more.
    InputEnded(io::Error),
    Hosted(Said),
    /// The host as the daemon says it stands, or why the daemon does not say.
    Daemon(Result<Host, String>),
    /// Something went wrong that the sidebar says.
    Notice(String),
}

/// How often the sidebar is drawn again with nothing else to draw it: every
/// second for the ages, and at each turn of the spinner while one turns.
const TICK: Duration = Duration::from_secs(1);

/// The most events that go into one frame. Those that come faster wait for the
/// next, so that luneta draws at least once each this many events, however
/// fast they come.
const FRAME_EVENTS_MAX: usize = 256;

/// Which side has the keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Zellij,
    Sidebar,
}

struct App {
    hosted: Hosted,
    sidebar: Sidebar,
    /// Where the sidebar and zellij were last drawn, which the mouse goes by.
    areas: (Rect, Rect),
    focus: Focus,
    /// Whether the window luneta's terminal is in has the focus.
    focused: bool,
    /// Whether a button went down over zellij and is still held: what it
    /// drags goes to zellij, wherever the mouse goes.
    dragging: bool,
    /// The cursor as luneta's terminal was last told to draw it.
    cursor: Option<(CursorShape, bool)>,
    events: Sender<Event>,
    ended: bool,
}

/// Runs zellij, attached to `session`, beside the sidebar, until zellij ends:
/// it detaches, or its session does.
pub fn run(session: Option<SessionName>, socket: Result<PathBuf, String>) -> ExitCode {
    let dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("luneta: no directory to start in: {error}");
            return ExitCode::FAILURE;
        },
    };
    let session = session.or_else(|| zellij::session_for(&dir));
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("luneta: no terminal: {error}");
            return ExitCode::FAILURE;
        },
    };
    // ratatui's hook puts the terminal back as it found it before a panic is
    // printed, all but the modes luneta turns on itself.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let _ = execute!(
            io::stdout(),
            PopKeyboardEnhancementFlags,
            DisableMouseCapture,
            DisableBracketedPaste,
            DisableFocusChange
        );
        restore(panic);
    }));
    // The kitty keyboard protocol, where luneta's terminal speaks it, tells
    // apart keys that zellij's client can then tell apart too, as `Shift
    // Enter` from `Enter`. Asked before anything reads the terminal's input.
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    let ran = execute!(io::stdout(), EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)
        .and_then(|()| {
            if kitty {
                let disambiguate = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
                execute!(io::stdout(), PushKeyboardEnhancementFlags(disambiguate))?;
            }
            run_in(&mut terminal, session.as_ref(), socket, &dir)
        });
    // Each mode is put back however the rest went, and the first error is the
    // one said.
    let popped = if kitty { execute!(io::stdout(), PopKeyboardEnhancementFlags) } else { Ok(()) };
    let restored = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        SetCursorStyle::DefaultUserShape
    );
    let left = ratatui::try_restore();
    match ran.and(popped).and(restored).and(left) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("luneta: {error}");
            ExitCode::FAILURE
        },
    }
}

fn run_in(
    terminal: &mut DefaultTerminal,
    session: Option<&SessionName>,
    socket: Result<PathBuf, String>,
    dir: &std::path::Path,
) -> io::Result<()> {
    // Not a `sync_channel` of a bound: alacritty hands on what the program
    // does with the screen locked, which the loop locks to draw, so that a
    // full queue would hold each up for the other for good. What fills it are
    // the keys and the mouse, the daemon's answers, zellij's actions and what
    // zellij does with its terminal, of which `hosted` keeps one draw at most.
    let (events, inbox) = mpsc::channel();
    let areas = areas(Rect::from((Position::ORIGIN, terminal.size()?)));
    let (program, args) = zellij::client(session);
    let hosted = Hosted::start(program, args, dir, window(areas.1), events.clone())?;
    let mut sidebar = Sidebar::default();
    match socket {
        Ok(socket) => crate::watch::start(socket, events.clone()),
        Err(why) => sidebar.lost(why),
    }
    read_input(events.clone());
    let mut app = App {
        hosted,
        sidebar,
        areas,
        focus: Focus::Zellij,
        focused: true,
        dragging: false,
        cursor: None,
        events,
        ended: false,
    };
    run_loop(terminal, &mut app, &inbox)
}

/// Reads luneta's terminal on a thread of its own, for as long as it can.
fn read_input(events: Sender<Event>) {
    thread::spawn(move || loop {
        match event::read() {
            Ok(input) => {
                if events.send(Event::Input(input)).is_err() {
                    return;
                }
            },
            Err(error) => {
                let _ = events.send(Event::InputEnded(error));
                return;
            },
        }
    });
}

/// Draws a frame for each batch of events, until zellij ends or luneta's
/// terminal can no longer be read.
fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    inbox: &Receiver<Event>,
) -> io::Result<()> {
    loop {
        let wait = if app.sidebar.turning() { sidebar::SPIN } else { TICK };
        match inbox.recv_timeout(wait) {
            Ok(event) => app.handle(event, terminal)?,
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the app holds a sender, for zellij's actions")
            },
        }
        // What came while luneta drew goes into one frame, with the event it
        // waited for: `FRAME_EVENTS_MAX` in all.
        for _ in 1..FRAME_EVENTS_MAX {
            let Ok(event) = inbox.try_recv() else {
                break;
            };
            app.handle(event, terminal)?;
        }
        if app.ended {
            return Ok(());
        }
        app.draw(terminal)?;
    }
}

/// The columns the sidebar takes of the terminal, and those left for zellij:
/// the sidebar's own, or half the terminal's, where that is narrower.
fn areas(frame: Rect) -> (Rect, Rect) {
    let sidebar = sidebar::COLUMNS.min(frame.width / 2);
    let left = Rect::new(frame.x, frame.y, sidebar, frame.height);
    let right = Rect::new(frame.x + sidebar, frame.y, frame.width - sidebar, frame.height);
    (left, right)
}

/// The size of zellij's terminal, for an `area` of luneta's: in cells, one
/// each way for an area with none, and in pixels where luneta's terminal says
/// how big its cells are.
fn window(area: Rect) -> Size {
    let cell =
        crossterm::terminal::window_size().ok().filter(|size| size.columns > 0 && size.rows > 0);
    let cells = |count: u16| NonZeroU16::new(count).unwrap_or(NonZeroU16::MIN);
    Size {
        columns: cells(area.width),
        lines: cells(area.height),
        cell_width: cell.as_ref().map_or(0, |size| size.width / size.columns),
        cell_height: cell.map_or(0, |size| size.height / size.rows),
    }
}

impl App {
    fn handle(&mut self, event: Event, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match event {
            Event::Input(Input::Key(key)) => self.key(&key),
            Event::Input(Input::Mouse(mouse)) => self.mouse(mouse),
            Event::Input(Input::Paste(text)) => {
                if self.focus == Focus::Zellij {
                    self.hosted.send(keys::paste(&text, self.hosted.mode()));
                }
            },
            Event::Input(Input::FocusGained) => self.focus_window(true),
            Event::Input(Input::FocusLost) => self.focus_window(false),
            // The next frame is of the new size, and zellij's terminal with it.
            Event::Input(Input::Resize(..)) => {},
            Event::InputEnded(error) => return Err(error),
            Event::Hosted(said) => self.hosted_said(said, terminal)?,
            Event::Daemon(Ok(host)) => self.sidebar.heard(host),
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Notice(notice) => self.sidebar.notify(notice),
        }
        Ok(())
    }

    fn key(&mut self, key: &KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('b') && key.modifiers == KeyModifiers::CONTROL {
            let other = match self.focus {
                Focus::Zellij => Focus::Sidebar,
                Focus::Sidebar => Focus::Zellij,
            };
            self.give(other);
            return;
        }
        match self.focus {
            Focus::Zellij => {
                if let Some(bytes) = keys::key(key, self.hosted.mode()) {
                    self.hosted.send(bytes);
                }
            },
            Focus::Sidebar => {
                let asked = self.sidebar.key(key);
                self.follow(asked);
            },
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let (sidebar, zellij) = self.areas;
        let over_zellij = mouse.column >= zellij.x;
        if self.dragging || over_zellij {
            match mouse.kind {
                MouseEventKind::Down(_) => {
                    self.dragging = true;
                    self.give(Focus::Zellij);
                },
                MouseEventKind::Up(_) => self.dragging = false,
                _ => {},
            }
            let column = mouse.column.saturating_sub(zellij.x).min(zellij.width.saturating_sub(1));
            let row = mouse.row.saturating_sub(zellij.y);
            if let Some(bytes) = keys::mouse(mouse, column, row, self.hosted.mode()) {
                self.hosted.send(bytes);
            }
            return;
        }
        let asked = self.sidebar.mouse(mouse, sidebar);
        if asked == Asked::Nothing && matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.give(Focus::Sidebar);
        }
        self.follow(asked);
    }

    /// Does what the sidebar asked.
    fn follow(&mut self, asked: Asked) {
        match asked {
            Asked::Nothing => {},
            Asked::Leave => self.give(Focus::Zellij),
            Asked::Go(place) => {
                self.go(&place);
                self.give(Focus::Zellij);
            },
        }
    }

    /// Takes zellij's client to `place`.
    fn go(&mut self, place: &Place) {
        let Some(here) = self.sidebar.here() else {
            self.sidebar.notify("zellij has yet to say which session it is in".to_owned());
            return;
        };
        self.hosted.send(zellij::NUDGE);
        zellij::go(here, place.session(), place.pane(), &self.events);
    }

    /// Hands the keys to `side`, and tells zellij when it takes them or loses
    /// them, as a terminal tells a program in it of the window's focus.
    fn give(&mut self, side: Focus) {
        if self.focus == side {
            return;
        }
        self.focus = side;
        if self.focused {
            let gained = side == Focus::Zellij;
            if let Some(bytes) = keys::focus(gained, self.hosted.mode()) {
                self.hosted.send(bytes);
            }
        }
    }

    fn focus_window(&mut self, focused: bool) {
        self.focused = focused;
        if self.focus == Focus::Zellij
            && let Some(bytes) = keys::focus(focused, self.hosted.mode())
        {
            self.hosted.send(bytes);
        }
    }

    fn hosted_said(&mut self, said: Said, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match said {
            // The frame this event goes into draws it.
            Said::Drew => {},
            Said::Titled(title) => {
                execute!(terminal.backend_mut(), SetTitle(&title))?;
                if !title.is_empty() {
                    self.sidebar.titled(title);
                }
            },
            Said::Copied(clipboard, text) => {
                let which = match clipboard {
                    ClipboardType::Clipboard => 'c',
                    ClipboardType::Selection => 'p',
                };
                let text = base64::engine::general_purpose::STANDARD.encode(text);
                let backend = terminal.backend_mut();
                write!(backend, "\x1b]52;{which};{text}\x07")?;
                backend.flush()?;
            },
            Said::Answer(answer) => self.hosted.send(answer.into_bytes()),
            Said::AskedSize(answer) => {
                self.hosted.send(answer(self.hosted.size().into()).into_bytes());
            },
            Said::Rang => {
                let backend = terminal.backend_mut();
                backend.write_all(b"\x07")?;
                backend.flush()?;
            },
            Said::Ended => self.ended = true,
        }
        Ok(())
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let now = SystemTime::now();
        terminal.draw(|frame| {
            let (sidebar, zellij) = areas(frame.area());
            self.areas = (sidebar, zellij);
            let size = self.hosted.size();
            if (zellij.width, zellij.height) != (size.columns.get(), size.lines.get()) {
                self.hosted.resize(window(zellij));
            }
            let buffer = frame.buffer_mut();
            self.sidebar.draw(buffer, sidebar, self.focus == Focus::Sidebar, now);
            let cursor = self.hosted.draw(buffer, zellij);
            if self.focus == Focus::Zellij
                && let Some(cursor) = cursor
            {
                frame.set_cursor_position(cursor);
            }
        })?;
        let cursor = self.hosted.cursor_style();
        if self.cursor != Some(cursor) {
            self.cursor = Some(cursor);
            execute!(terminal.backend_mut(), cursor_style(cursor))?;
        }
        Ok(())
    }
}

/// How luneta's terminal is told to draw a cursor of `shape` that blinks or
/// not: as it would draw one of its own, for the shape a program never asked
/// for, which `hosted` gives as hollow.
fn cursor_style((shape, blinking): (CursorShape, bool)) -> SetCursorStyle {
    match (shape, blinking) {
        (CursorShape::HollowBlock, _) => SetCursorStyle::DefaultUserShape,
        (CursorShape::Underline, true) => SetCursorStyle::BlinkingUnderScore,
        (CursorShape::Underline, false) => SetCursorStyle::SteadyUnderScore,
        (CursorShape::Beam, true) => SetCursorStyle::BlinkingBar,
        (CursorShape::Beam, false) => SetCursorStyle::SteadyBar,
        (_, true) => SetCursorStyle::BlinkingBlock,
        (_, false) => SetCursorStyle::SteadyBlock,
    }
}
