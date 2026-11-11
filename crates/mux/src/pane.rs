//! A pane's thread: the only one that holds its screen, which Ghostty's
//! emulator does not let another thread touch, and its pseudoterminal. It
//! reads what the program writes, answers what it asks, writes what the
//! clients on the pane do, and sends each of them the rows that changed since
//! the last frame it got.

use std::collections::HashMap;
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::process::{Child, ExitStatus};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::key::{Action, Key, Mods};
use rustix::event::{PollFd, PollFlags, Timespec};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Dumped, Input, Size};
use crate::frame;
use crate::mux::Mux;
use crate::pty::Pty;
use crate::screen::{Screen, View, Wheel};

/// The scrollback each pane keeps, in bytes.
const SCROLLBACK_BYTES: usize = 10_000_000;

/// The least time between two frames of a pane: what its program draws in
/// between goes in the next.
const FRAME_GAP: Duration = Duration::from_millis(4);

/// The longest a pane holds its frames while its program draws in one go
/// (mode 2026), as Ghostty holds them.
const SYNC_HOLD: Duration = Duration::from_secs(1);

/// How soon a pane tries again to send a frame to a client that had no room.
const OWED_RETRY: Duration = Duration::from_millis(8);

/// The bytes a read takes from the program at most.
const READ_BYTES: usize = 64 * 1024;

/// The reads a turn of the loop makes at most before it sends frames.
const READS_PER_TURN: usize = 16;

/// The most bytes a pane holds for its program that it has not read yet. A
/// client's input past them is dropped.
const UNREAD_BYTES_MAX: usize = 1024 * 1024;

/// The most a pane reads of what its program wrote as it ended.
const LAST_WORDS_BYTES_MAX: usize = 1024 * 1024;

/// The rows a turn of the wheel scrolls.
const WHEEL_ROWS: usize = 3;

/// The messages a client's stream holds.
pub const CLIENT_MESSAGES: usize = 64;

/// The messages of a client's stream kept for the server's words, which no
/// frame takes.
const FOR_WORDS: usize = 8;

/// What goes to a client.
pub type Out = tokio_mpsc::Sender<Result<api::ToClient, Status>>;

/// How a pane's program ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    fn of(status: ExitStatus) -> Exit {
        match (status.code(), status.signal()) {
            (Some(code), _) => Exit::Code(code),
            (None, Some(signal)) => Exit::Signal(signal),
            (None, None) => unreachable!("{status} is neither a code nor a signal"),
        }
    }

    #[must_use]
    pub fn said(self) -> api::Exit {
        let how = match self {
            Exit::Code(code) => api::exit::How::Code(code),
            Exit::Signal(signal) => api::exit::How::Signal(signal),
        };
        api::Exit { how: Some(how) }
    }
}

/// A client on a pane: where its frames go, and its size.
pub struct Viewer {
    pub client: u64,
    pub out: Out,
    pub size: Size,
}

/// What a pane's thread is asked: by the model, for its clients, and by the
/// thread that waits on its program.
pub enum ToPane {
    Watch(Viewer),
    Unwatch(u64),
    Input(u64, Input),
    Resize(u64, Size),
    Colours(Colours),
    Dump(Dumped, oneshot::Sender<Result<String, Status>>),
    Exited(Exit),
    /// Hangs up the program, and ends the thread.
    Close,
}

/// What reaches a pane's thread.
#[derive(Clone)]
pub struct Handle {
    to: Sender<ToPane>,
    wake: Arc<UnixStream>,
}

impl Handle {
    /// Sends `message` to the pane, which may have ended.
    pub fn send(&self, message: ToPane) {
        if self.to.send(message).is_ok() {
            match (&*self.wake).write(&[0]) {
                Ok(_) => {},
                Err(error) if error.kind() == ErrorKind::WouldBlock => {},
                Err(error) => eprintln!("luneta-mux: a pane's wake failed: {error}"),
            }
        }
    }
}

/// A started pane: what reaches its thread, and its program's pid.
pub struct Started {
    pub handle: Handle,
    pub pid: u32,
}

/// Starts the thread of pane `pane`, with its pseudoterminal `pty`, on which
/// `child` runs, of `size`.
pub fn start(
    mux: Arc<Mux>,
    pane: u32,
    pty: Pty,
    mut child: Child,
    size: Size,
) -> io::Result<Started> {
    let (to, inbox) = mpsc::channel();
    let (wake, woken) = UnixStream::pair()?;
    wake.set_nonblocking(true)?;
    woken.set_nonblocking(true)?;
    let handle = Handle { to, wake: Arc::new(wake) };
    let pid = child.id();
    let waiter = handle.clone();
    thread::Builder::new().name(format!("pane {pane} waits")).spawn(move || {
        match child.wait() {
            Ok(status) => waiter.send(ToPane::Exited(Exit::of(status))),
            Err(error) => {
                eprintln!("luneta-mux: pane {pane}'s program could not be waited for: {error}");
            },
        }
    })?;
    thread::Builder::new().name(format!("pane {pane}")).spawn(move || {
        let screen = match Screen::new(size.columns, size.rows, SCROLLBACK_BYTES) {
            Ok(screen) => screen,
            Err(error) => {
                eprintln!("luneta-mux: pane {pane}'s screen could not be made: {error}");
                drop(pty);
                mux.lost(pane);
                return;
            },
        };
        let mut thread = Thread {
            pane,
            mux,
            inbox,
            woken,
            screen,
            pty: Some(pty),
            unread: Vec::new(),
            watchers: Vec::new(),
            dirty: false,
            framed: Instant::now().checked_sub(FRAME_GAP).unwrap_or_else(Instant::now),
            synced: None,
            title: String::new(),
            pwd: String::new(),
        };
        if let Err(error) = thread.run() {
            eprintln!("luneta-mux: pane {pane} failed: {error}");
            thread.pty = None;
            thread.mux.lost(pane);
        }
    })?;
    Ok(Started { handle, pid })
}

/// What a client last got of a pane.
#[derive(Clone, PartialEq, Eq)]
struct Shown {
    size: (u16, u16),
    view: View,
    clusters: bool,
    tracks: bool,
    back: usize,
    scrollback: usize,
}

struct Watcher {
    viewer: Viewer,
    back: usize,
    shown: Option<Shown>,
    owed: bool,
}

struct Thread {
    pane: u32,
    mux: Arc<Mux>,
    inbox: Receiver<ToPane>,
    woken: UnixStream,
    screen: Screen,
    /// None once the program ended or was hung up.
    pty: Option<Pty>,
    /// What the program has yet to read: the clients' input, and the screen's
    /// answers.
    unread: Vec<u8>,
    watchers: Vec<Watcher>,
    dirty: bool,
    framed: Instant,
    /// Since when the program has held its frames, if it does.
    synced: Option<Instant>,
    title: String,
    pwd: String,
}

impl Thread {
    fn run(&mut self) -> io::Result<()> {
        loop {
            let timeout = self.timeout();
            let (readable, writable, woken) = self.poll(timeout)?;
            if woken {
                let mut drained = [0; 64];
                while matches!((&self.woken).read(&mut drained), Ok(read) if read > 0) {}
            }
            loop {
                match self.inbox.try_recv() {
                    Ok(message) => {
                        if self.take(message)? {
                            self.pty = None;
                            return Ok(());
                        }
                    },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            if readable {
                self.read(READS_PER_TURN * READ_BYTES)?;
            }
            if writable || !self.unread.is_empty() {
                self.write()?;
            }
            self.frame()?;
        }
    }

    /// How long the loop waits for the program or a message.
    fn timeout(&self) -> Option<Duration> {
        let owed = self.watchers.iter().any(|watcher| watcher.owed);
        if !self.dirty && !owed {
            return None;
        }
        let due = match self.synced {
            Some(since) => SYNC_HOLD.saturating_sub(since.elapsed()),
            None => FRAME_GAP.saturating_sub(self.framed.elapsed()),
        };
        Some(if owed { due.max(OWED_RETRY) } else { due })
    }

    fn poll(&self, timeout: Option<Duration>) -> io::Result<(bool, bool, bool)> {
        let wait = timeout.map(|timeout| Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        });
        let mut fds = vec![PollFd::new(&self.woken, PollFlags::IN)];
        if let Some(pty) = &self.pty {
            let mut flags = PollFlags::IN;
            if !self.unread.is_empty() {
                flags |= PollFlags::OUT;
            }
            fds.push(PollFd::from_borrowed_fd(pty.near.as_fd(), flags));
        }
        match rustix::event::poll(&mut fds, wait.as_ref()) {
            Ok(_) => {},
            Err(rustix::io::Errno::INTR) => return Ok((false, false, false)),
            Err(error) => return Err(error.into()),
        }
        let woken = !fds[0].revents().is_empty();
        let near = fds.get(1).map_or(PollFlags::empty(), PollFd::revents);
        let readable = near.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR);
        Ok((readable, near.contains(PollFlags::OUT), woken))
    }

    /// Takes `message`, and says whether the pane is done.
    fn take(&mut self, message: ToPane) -> io::Result<bool> {
        match message {
            ToPane::Watch(viewer) => {
                let client = viewer.client;
                self.watchers.retain(|watcher| watcher.viewer.client != client);
                self.watchers.push(Watcher { viewer, back: 0, shown: None, owed: false });
                self.follow(client)?;
                self.dirty = true;
            },
            ToPane::Unwatch(client) => {
                self.watchers.retain(|watcher| watcher.viewer.client != client);
            },
            ToPane::Input(client, input) => self.input(client, input)?,
            ToPane::Resize(client, size) => {
                if let Some(watcher) = self.watcher(client) {
                    watcher.viewer.size = size;
                    self.follow(client)?;
                }
            },
            ToPane::Colours(colours) => {
                self.screen
                    .colour(colours.foreground, colours.background)
                    .map_err(io::Error::other)?;
            },
            ToPane::Dump(dumped, answer) => {
                let text = match dumped {
                    Dumped::Screen => self.screen.dumped_screen(),
                    Dumped::Text => self.screen.dumped_text(),
                };
                let _ = answer.send(
                    text.map_err(|error| Status::internal(format!("the dump failed: {error}"))),
                );
            },
            ToPane::Exited(exit) => {
                if self.pty.is_some() {
                    self.read(LAST_WORDS_BYTES_MAX)?;
                }
                self.pty = None;
                self.unread.clear();
                self.dirty = true;
                self.frame()?;
                return Ok(self.mux.ended(self.pane, exit));
            },
            ToPane::Close => return Ok(true),
        }
        Ok(false)
    }

    fn watcher(&mut self, client: u64) -> Option<&mut Watcher> {
        self.watchers.iter_mut().find(|watcher| watcher.viewer.client == client)
    }

    /// Makes the pane the size of `client`, the latest to come to it, resize
    /// or do anything in it, as tmux's `window-size latest` does.
    fn follow(&mut self, client: u64) -> io::Result<()> {
        let Some(size) = self.watcher(client).map(|watcher| watcher.viewer.size) else {
            return Ok(());
        };
        let (columns, rows) = self.screen.size().map_err(io::Error::other)?;
        if (columns, rows) == (size.columns, size.rows) {
            return Ok(());
        }
        let answers = self.screen.resize(size.columns, size.rows).map_err(io::Error::other)?;
        if let Some(pty) = &self.pty {
            pty.resize(size)?;
            self.unread.extend_from_slice(&answers);
        }
        let scrollback = self.screen.scrollback().map_err(io::Error::other)?;
        for watcher in &mut self.watchers {
            watcher.back = watcher.back.min(scrollback);
        }
        self.dirty = true;
        Ok(())
    }

    fn input(&mut self, client: u64, input: Input) -> io::Result<()> {
        if self.watcher(client).is_none() {
            return Ok(());
        }
        self.follow(client)?;
        let (columns, rows) = self.screen.size().map_err(io::Error::other)?;
        let within = |column: u16, row: u16| column < columns.get() && row < rows.get();
        if input.types() {
            self.back(client, None);
        }
        let mut scrolled = None;
        let screen = &mut self.screen;
        let bytes = match input {
            Input::Key { action, key, mods, text, unshifted } => {
                screen.key(action, key, mods, text.as_deref(), unshifted)
            },
            Input::Paste(text) => screen.paste(&text),
            Input::Focus(focused) => screen.focus(focused),
            Input::Mouse { click, mods, column, row } if within(column, row) => {
                screen.mouse(click, mods, column, row)
            },
            Input::Wheel { wheel, mods, column, row } if within(column, row) => {
                if screen.tracks_mouse().map_err(io::Error::other)? {
                    screen.wheel(wheel, mods, column, row)
                } else if screen.wheel_is_arrows().map_err(io::Error::other)? {
                    arrows(screen, wheel)
                } else {
                    let rows = isize::try_from(WHEEL_ROWS).unwrap_or(isize::MAX);
                    scrolled = match wheel {
                        Wheel::Up => Some(rows),
                        Wheel::Down => Some(-rows),
                        Wheel::Left | Wheel::Right => None,
                    };
                    Ok(Vec::new())
                }
            },
            Input::Mouse { .. } | Input::Wheel { .. } => Ok(Vec::new()),
            Input::Scroll(rows) => {
                scrolled = Some(isize::try_from(rows).unwrap_or(0));
                Ok(Vec::new())
            },
        };
        if scrolled.is_some() {
            self.back(client, scrolled);
        }
        let bytes = bytes.map_err(io::Error::other)?;
        if self.pty.is_none() {
            return Ok(());
        }
        if self.unread.len() + bytes.len() > UNREAD_BYTES_MAX {
            eprintln!(
                "luneta-mux: pane {} has {} unread bytes: input dropped",
                self.pane,
                self.unread.len()
            );
            return Ok(());
        }
        self.unread.extend_from_slice(&bytes);
        Ok(())
    }

    /// Moves `client`'s view `by` rows back into the scrollback, or to the
    /// bottom where `by` is none.
    fn back(&mut self, client: u64, by: Option<isize>) {
        let scrollback = self.screen.scrollback().unwrap_or(0);
        let Some(watcher) = self.watcher(client) else {
            return;
        };
        let back = match by {
            None => 0,
            Some(by) => watcher.back.saturating_add_signed(by).min(scrollback),
        };
        if back != watcher.back {
            watcher.back = back;
            self.dirty = true;
        }
    }

    /// Reads up to `most` bytes of what the program wrote, as far as it wrote.
    fn read(&mut self, most: usize) -> io::Result<()> {
        let Some(pty) = &mut self.pty else {
            return Ok(());
        };
        let before = self.screen.scrollback().map_err(io::Error::other)?;
        let mut buffer = vec![0; READ_BYTES];
        let mut total = 0;
        while total < most {
            match pty.near.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    total += read;
                    let answers = self.screen.write(&buffer[..read]);
                    self.unread.extend_from_slice(&answers);
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {},
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    break;
                },
                Err(error) => return Err(error),
            }
        }
        if total == 0 {
            return Ok(());
        }
        self.dirty = true;
        let after = self.screen.scrollback().map_err(io::Error::other)?;
        let grown = after.saturating_sub(before);
        for watcher in &mut self.watchers {
            if watcher.back > 0 {
                watcher.back = (watcher.back + grown).min(after);
            }
        }
        let syncs = self.screen.syncs().map_err(io::Error::other)?;
        self.synced = match (syncs, self.synced) {
            (true, None) => Some(Instant::now()),
            (true, since) => since,
            (false, _) => None,
        };
        let title = self.screen.title().map_err(io::Error::other)?;
        let pwd = self.screen.pwd().map_err(io::Error::other)?;
        if title != self.title || pwd != self.pwd {
            self.title.clone_from(&title);
            self.pwd.clone_from(&pwd);
            self.mux.titled(self.pane, title, pwd);
        }
        Ok(())
    }

    /// Writes what the program has yet to read, as much as it takes now.
    fn write(&mut self) -> io::Result<()> {
        let Some(pty) = &mut self.pty else {
            self.unread.clear();
            return Ok(());
        };
        while !self.unread.is_empty() {
            match pty.near.write(&self.unread) {
                Ok(written) => {
                    self.unread.drain(..written);
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {},
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    self.unread.clear();
                },
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Sends each client on the pane what changed since its last frame, once
    /// a frame is due.
    fn frame(&mut self) -> io::Result<()> {
        let owed = self.watchers.iter().any(|watcher| watcher.owed);
        if (!self.dirty && !owed) || self.watchers.is_empty() {
            self.dirty = false;
            return Ok(());
        }
        let held = self.synced.is_some_and(|since| since.elapsed() < SYNC_HOLD);
        if held || self.framed.elapsed() < FRAME_GAP {
            return Ok(());
        }
        self.framed = Instant::now();
        self.dirty = false;
        let fail = io::Error::other;
        let (columns, rows) = self.screen.size().map_err(fail)?;
        let clusters = self.screen.clusters().map_err(fail)?;
        let tracks = self.screen.tracks_mouse().map_err(fail)?;
        let scrollback = self.screen.scrollback().map_err(fail)?;
        let mut views: HashMap<usize, View> = HashMap::new();
        for watcher in &mut self.watchers {
            if watcher.viewer.out.capacity() <= FOR_WORDS {
                watcher.owed = true;
                continue;
            }
            let view = if let Some(view) = views.get(&watcher.back) {
                view.clone()
            } else {
                let view = self.screen.view(watcher.back).map_err(fail)?;
                views.insert(watcher.back, view.clone());
                view
            };
            let shown = Shown {
                size: (columns.get(), rows.get()),
                view,
                clusters,
                tracks,
                back: watcher.back,
                scrollback,
            };
            if watcher.shown.as_ref() == Some(&shown) {
                watcher.owed = false;
                continue;
            }
            let said = frame::said(
                self.pane,
                &shown.view,
                watcher.shown.as_ref().map(|shown| (shown.size, &shown.view)),
                shown.size,
                frame::Flags { clusters, tracks, back: watcher.back, scrollback },
            );
            let message = api::ToClient { event: Some(api::to_client::Event::Frame(said)) };
            match watcher.viewer.out.try_send(Ok(message)) {
                Ok(()) => {
                    watcher.shown = Some(shown);
                    watcher.owed = false;
                },
                Err(tokio_mpsc::error::TrySendError::Full(_)) => watcher.owed = true,
                Err(tokio_mpsc::error::TrySendError::Closed(_)) => watcher.owed = false,
            }
        }
        Ok(())
    }
}

/// What the program reads of a turn of the wheel as arrow keys.
fn arrows(screen: &mut Screen, wheel: Wheel) -> Result<Vec<u8>, libghostty_vt::Error> {
    let key = match wheel {
        Wheel::Up => Key::ArrowUp,
        Wheel::Down => Key::ArrowDown,
        Wheel::Left => Key::ArrowLeft,
        Wheel::Right => Key::ArrowRight,
    };
    let mut bytes = Vec::new();
    for _ in 0..WHEEL_ROWS {
        bytes.extend(screen.key(Action::Press, key, Mods::empty(), None, None)?);
    }
    Ok(bytes)
}
