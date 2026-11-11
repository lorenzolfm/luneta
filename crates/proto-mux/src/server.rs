//! PROTOTYPE. One server for every session. Each session is one pane: a
//! pseudoterminal, and alacritty's emulator reading it. Clients get the grid,
//! as the rows that changed since the last frame they got, never the bytes.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Cell as Grid;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use rustix::fs::{Mode, OFlags};

use crate::wire::{self, Cell, Colour, Frame, Rgb, Size, ToClient, ToServer};

/// The least time between two frames to one client: what a program draws in
/// between goes in the next.
const FRAME_GAP: Duration = Duration::from_millis(4);
const SCROLLBACK: usize = 10_000;

struct Server {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    socket: PathBuf,
}

struct Session {
    term: Arc<FairMutex<Term<Listener>>>,
    shared: Arc<Shared>,
    pid: u32,
    _far: File,
}

/// What the emulator's listener reaches, which is called with the grid
/// locked: nothing here locks it.
struct Shared {
    name: String,
    peers: Mutex<Vec<Arc<Peer>>>,
    pty: OnceLock<EventLoopSender>,
    colours: Mutex<(Option<Rgb>, Option<Rgb>)>,
    size: Mutex<Size>,
    title: Mutex<String>,
    status: Mutex<Option<i32>>,
    server: Weak<Server>,
}

struct Peer {
    notes: Sender<Note>,
    /// Whether a `Note::Dirty` is on its way, which stands for all drawing until it is read.
    pending: AtomicBool,
    offset: AtomicU32,
}

enum Note {
    Dirty,
    Title(String),
    Bell,
    Clipboard(String),
    Ended(Option<i32>),
    Gone,
}

impl Peer {
    fn dirty(&self) {
        if !self.pending.swap(true, Ordering::SeqCst) {
            let _ = self.notes.send(Note::Dirty);
        }
    }
}

impl Shared {
    fn each(&self, f: impl Fn(&Peer)) {
        for peer in self.peers.lock().unwrap().iter() {
            f(peer);
        }
    }

    fn answer(&self, text: String) {
        if let Some(pty) = self.pty.get() {
            let _ = pty.send(Msg::Input(text.into_bytes().into()));
        }
    }
}

#[derive(Clone)]
struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let shared = &self.0;
        match event {
            Event::Wakeup => shared.each(Peer::dirty),
            Event::Title(title) => {
                *shared.title.lock().unwrap() = title.clone();
                shared.each(|p| { let _ = p.notes.send(Note::Title(title.clone())); });
            },
            Event::ResetTitle => {
                shared.title.lock().unwrap().clear();
                shared.each(|p| { let _ = p.notes.send(Note::Title(String::new())); });
            },
            Event::ClipboardStore(_, text) => {
                shared.each(|p| { let _ = p.notes.send(Note::Clipboard(text.clone())); });
            },
            Event::ClipboardLoad(..) => log::warn!(target: "proto", "unanswered: clipboard read (OSC 52 ?)"),
            Event::PtyWrite(text) => {
                log::debug!(target: "proto", "answered {text:?}");
                shared.answer(text);
            },
            Event::ColorRequest(index, spell) => {
                let (fg, bg) = *shared.colours.lock().unwrap();
                let colour = if index == NamedColor::Foreground as usize {
                    fg
                } else if index == NamedColor::Background as usize {
                    bg
                } else {
                    None
                };
                match colour {
                    Some(Rgb(r, g, b)) => {
                        let text = spell(alacritty_terminal::vte::ansi::Rgb { r, g, b });
                        log::debug!(target: "proto", "answered colour {index}: {text:?}");
                        shared.answer(text);
                    },
                    None => log::warn!(target: "proto", "unanswered: colour {index}"),
                }
            },
            Event::TextAreaSizeRequest(spell) => {
                let size = *shared.size.lock().unwrap();
                shared.answer(spell(window(size)));
            },
            Event::Bell => shared.each(|p| { let _ = p.notes.send(Note::Bell); }),
            Event::ChildExit(status) => *shared.status.lock().unwrap() = status.code(),
            Event::Exit => {
                let code = *shared.status.lock().unwrap();
                shared.each(|p| { let _ = p.notes.send(Note::Ended(code)); });
                if let Some(server) = shared.server.upgrade() {
                    let mut sessions = server.sessions.lock().unwrap();
                    sessions.remove(&shared.name);
                    log::info!(target: "proto", "session {} ended ({code:?})", shared.name);
                    if sessions.is_empty() {
                        let socket = server.socket.clone();
                        thread::spawn(move || {
                            // Long enough for the clients to hear it ended.
                            thread::sleep(Duration::from_millis(300));
                            let _ = std::fs::remove_file(socket);
                            std::process::exit(0);
                        });
                    }
                }
            },
            Event::CursorBlinkingChange | Event::MouseCursorDirty => {},
        }
    }
}

fn window(size: Size) -> WindowSize {
    WindowSize { num_cols: size.columns, num_lines: size.lines, cell_width: size.cell_width, cell_height: size.cell_height }
}

/// A file every log line of the emulator goes to, for what it does not handle.
struct Logger(Mutex<File>);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Debug
    }
    fn log(&self, r: &log::Record) {
        let line = format!("{} {} {}\n", r.level(), r.target(), r.args());
        let _ = self.0.lock().unwrap().write_all(line.as_bytes());
    }
    fn flush(&self) {}
}

pub fn run(socket: &Path, log_path: &Path) -> io::Result<()> {
    // Out of the client's session and terminal: its hangup is not ours.
    let _ = rustix::process::setsid();
    let file = OpenOptions::new().create(true).append(true).open(log_path)?;
    let _ = log::set_boxed_logger(Box::new(Logger(Mutex::new(file))));
    log::set_max_level(log::LevelFilter::Debug);
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    log::info!(target: "proto", "listening on {}", socket.display());
    let server = Arc::new(Server { sessions: Mutex::new(HashMap::new()), socket: socket.to_owned() });
    for stream in listener.incoming() {
        let stream = stream?;
        let server = Arc::clone(&server);
        thread::spawn(move || {
            if let Err(error) = serve(&server, stream) {
                log::info!(target: "proto", "client gone: {error}");
            }
        });
    }
    Ok(())
}

fn serve(server: &Arc<Server>, mut stream: UnixStream) -> io::Result<()> {
    let (name, command, dir, size, fg, bg) = match ToServer::receive(&mut stream)? {
        ToServer::List => {
            let mut names: Vec<String> = server.sessions.lock().unwrap().keys().cloned().collect();
            names.sort();
            ToClient::Sessions(names).send(&mut stream)?;
            return Ok(());
        },
        ToServer::Kill(name) => {
            let session = server.sessions.lock().unwrap().get(&name).cloned();
            if let Some(session) = session {
                let pid = rustix::process::Pid::from_raw(session.pid as i32).unwrap();
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::HUP);
            }
            ToClient::Sessions(Vec::new()).send(&mut stream)?;
            return Ok(());
        },
        ToServer::Send(name, bytes) => {
            let session = server.sessions.lock().unwrap().get(&name).cloned();
            if let Some(pty) = session.as_ref().and_then(|s| s.shared.pty.get()) {
                let _ = pty.send(Msg::Input(bytes.into()));
            }
            ToClient::Sessions(Vec::new()).send(&mut stream)?;
            return Ok(());
        },
        ToServer::Hello { session, command, dir, size, fg, bg } => (session, command, dir, size, fg, bg),
        _ => return Err(io::Error::other("no hello")),
    };
    let session = {
        let mut sessions = server.sessions.lock().unwrap();
        match sessions.get(&name) {
            Some(session) => Arc::clone(session),
            None => {
                let session = Arc::new(start(server, &name, command, &dir, size, (fg, bg))?);
                sessions.insert(name.clone(), Arc::clone(&session));
                session
            },
        }
    };
    *session.shared.colours.lock().unwrap() = (fg, bg);
    session.resize(size);
    let (notes, inbox) = mpsc::channel();
    let peer = Arc::new(Peer { notes, pending: AtomicBool::new(false), offset: AtomicU32::new(0) });
    session.shared.peers.lock().unwrap().push(Arc::clone(&peer));
    let title = session.shared.title.lock().unwrap().clone();
    if !title.is_empty() {
        let _ = peer.notes.send(Note::Title(title));
    }
    peer.dirty();
    let writer = {
        let (session, peer, stream) = (Arc::clone(&session), Arc::clone(&peer), stream.try_clone()?);
        thread::spawn(move || write(&session, &peer, &inbox, stream))
    };
    let read = read(&session, &peer, &mut stream);
    session.shared.peers.lock().unwrap().retain(|p| !Arc::ptr_eq(p, &peer));
    let _ = peer.notes.send(Note::Gone);
    let _ = writer.join();
    read
}

fn read(session: &Session, peer: &Peer, stream: &mut UnixStream) -> io::Result<()> {
    loop {
        match ToServer::receive(stream)? {
            ToServer::Input(bytes) => {
                if peer.offset.swap(0, Ordering::SeqCst) != 0 {
                    peer.dirty();
                }
                if let Some(pty) = session.shared.pty.get() {
                    let _ = pty.send(Msg::Input(bytes.into()));
                }
            },
            ToServer::Resize(size) => session.resize(size),
            ToServer::Scroll(delta) => {
                let history = session.term.lock().grid().history_size() as i64;
                let offset = i64::from(peer.offset.load(Ordering::SeqCst)) + i64::from(delta);
                peer.offset.store(offset.clamp(0, history) as u32, Ordering::SeqCst);
                peer.dirty();
            },
            _ => {},
        }
    }
}

fn write(session: &Session, peer: &Peer, inbox: &Receiver<Note>, mut stream: UnixStream) {
    let mut last: Vec<Vec<Cell>> = Vec::new();
    let mut sent = Instant::now() - FRAME_GAP;
    for note in inbox {
        let message = match note {
            Note::Dirty => {
                let since = sent.elapsed();
                if since < FRAME_GAP {
                    thread::sleep(FRAME_GAP - since);
                }
                peer.pending.store(false, Ordering::SeqCst);
                sent = Instant::now();
                ToClient::Frame(frame(session, peer, &mut last))
            },
            Note::Title(title) => ToClient::Title(title),
            Note::Bell => ToClient::Bell,
            Note::Clipboard(text) => ToClient::Clipboard(text),
            Note::Ended(code) => ToClient::Ended(code),
            Note::Gone => break,
        };
        let ended = matches!(message, ToClient::Ended(_));
        if message.send(&mut stream).is_err() || ended {
            break;
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// The rows of the grid, as this client looks at it, that changed since `last`.
fn frame(session: &Session, peer: &Peer, last: &mut Vec<Vec<Cell>>) -> Frame {
    let start = Instant::now();
    let term = session.term.lock();
    let grid = term.grid();
    let (lines, columns, history) = (grid.screen_lines(), grid.columns(), grid.history_size());
    let offset = (peer.offset.load(Ordering::SeqCst) as usize).min(history);
    let mode = term.mode().bits();
    let cursor = term.renderable_content().cursor;
    let style = term.cursor_style();
    let shown = cursor.shape != CursorShape::Hidden && offset == 0;
    let cursor = shown.then(|| (cursor.point.line.0 as u16, cursor.point.column.0 as u16));
    let shape = match if shown { style.shape } else { CursorShape::Hidden } {
        CursorShape::Block => 0,
        CursorShape::Underline => 1,
        CursorShape::Beam => 2,
        CursorShape::HollowBlock => 3,
        CursorShape::Hidden => 4,
    };
    let rows: Vec<Vec<Cell>> = (0..lines)
        .map(|l| {
            let row = &grid[Line(l as i32 - offset as i32)];
            (0..columns).map(|c| cell(&row[Column(c)])).collect()
        })
        .collect();
    drop(term);
    let read_us = start.elapsed().as_micros() as u32;
    let whole = last.len() != lines || last.first().map(Vec::len) != Some(columns);
    let changed = rows
        .iter()
        .enumerate()
        .filter(|(l, row)| whole || last[*l] != **row)
        .map(|(l, row)| (l as u16, row.clone()))
        .collect();
    *last = rows;
    Frame {
        columns: columns as u16,
        lines: lines as u16,
        mode,
        cursor,
        shape,
        blinking: style.blinking,
        offset: offset as u32,
        history: history as u32,
        whole,
        rows: changed,
        read_us,
    }
}

fn cell(grid: &Grid) -> Cell {
    let (fg, dim) = colour(grid.fg);
    let mut flags = grid.flags.bits();
    if dim {
        flags |= wire::DIM_FG;
    }
    Cell {
        c: grid.c,
        zerowidth: grid.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
        fg,
        bg: colour(grid.bg).0,
        underline: grid.underline_color().map(|c| colour(c).0),
        flags,
        link: grid.hyperlink().map(|link| link.uri().to_owned()),
    }
}

/// As `hosted.rs` does: the sixteen and the defaults are left to the client's
/// terminal's theme.
fn colour(colour: Color) -> (Colour, bool) {
    match colour {
        Color::Spec(c) => (Colour::Rgb(Rgb(c.r, c.g, c.b)), false),
        Color::Indexed(i) => (Colour::Indexed(i), false),
        Color::Named(named) => {
            let index = named as usize;
            if index <= 15 {
                return (Colour::Indexed(index as u8), false);
            }
            let dim = NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize;
            if dim.contains(&index) {
                return (Colour::Indexed((index - NamedColor::DimBlack as usize) as u8), true);
            }
            (Colour::Reset, named == NamedColor::DimForeground)
        },
    }
}

impl Session {
    fn resize(&self, size: Size) {
        let mut current = self.shared.size.lock().unwrap();
        let changed = (current.columns, current.lines) != (size.columns, size.lines);
        *current = size;
        drop(current);
        if changed {
            self.term.lock().resize(TermSize::new(usize::from(size.columns), usize::from(size.lines)));
            if let Some(pty) = self.shared.pty.get() {
                let _ = pty.send(Msg::Resize(window(size)));
            }
        }
        self.shared.each(Peer::dirty);
    }
}

fn start(
    server: &Arc<Server>,
    name: &str,
    mut command: Vec<String>,
    dir: &str,
    size: Size,
    colours: (Option<Rgb>, Option<Rgb>),
) -> io::Result<Session> {
    if command.is_empty() {
        command.push(std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_owned()));
    }
    let program = command.remove(0);
    log::info!(target: "proto", "session {name}: {program} {command:?} in {dir} at {size:?}");
    let env = HashMap::from([
        ("TERM".to_owned(), "xterm-256color".to_owned()),
        ("COLORTERM".to_owned(), "truecolor".to_owned()),
        ("PROTO_MUX_SESSION".to_owned(), name.to_owned()),
    ]);
    let options = tty::Options {
        shell: Some(tty::Shell::new(program, command)),
        working_directory: Some(dir.into()),
        drain_on_exit: true,
        env,
    };
    let pty = tty::new(&options, window(size), 0)?;
    let pid = pty.child().id();
    let far = far_end(pty.file())?;
    let shared = Arc::new(Shared {
        name: name.to_owned(),
        peers: Mutex::new(Vec::new()),
        pty: OnceLock::new(),
        colours: Mutex::new(colours),
        size: Mutex::new(size),
        title: Mutex::new(String::new()),
        status: Mutex::new(None),
        server: Arc::downgrade(server),
    });
    let config = Config { scrolling_history: SCROLLBACK, kitty_keyboard: true, ..Config::default() };
    let dimensions = TermSize::new(usize::from(size.columns), usize::from(size.lines));
    let term = Arc::new(FairMutex::new(Term::new(config, &dimensions, Listener(Arc::clone(&shared)))));
    let reader = EventLoop::new(Arc::clone(&term), Listener(Arc::clone(&shared)), pty, true, false)?;
    let _ = shared.pty.set(reader.channel());
    reader.spawn();
    Ok(Session { term, shared, pid, _far: far })
}

/// As in `hosted.rs`: the program's end, held open so that what a program
/// writes as it ends is read.
fn far_end(near: &File) -> io::Result<File> {
    let path = rustix::pty::ptsname(near, Vec::new())?;
    let path = Path::new(OsStr::from_bytes(path.as_bytes()));
    let flags = OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}
