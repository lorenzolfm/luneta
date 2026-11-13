//! A pane's thread: the only one that holds its screen, which Ghostty's
//! emulator does not let another thread touch, and its pseudoterminal. It
//! reads what the program writes, answers what it asks, writes what the
//! clients on the pane do, and sends each of them the rows that changed since
//! the last frame it got.

use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self as std_mpsc, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::Error;
use libghostty_vt::key::{Action, Key, Mods};
use rustix::event::{PollFd, PollFlags, Timespec};
use tokio::sync::mpsc::{self, OwnedPermit, error::TryRecvError, error::TrySendError};
use tokio::sync::oneshot;
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Dumped, Input, Size};
use crate::frame::{self, Flags};
use crate::front::Front;
use crate::id::{ClientId, PaneId};
use crate::marks::{Mark, Marks};
use crate::mux::Mux;
use crate::process::{self, Group};
use crate::pty::Pty;
use crate::ran::{Job, Now, Shell};
use crate::screen::{Screen, View, Wheel};

/// The scrollback each pane keeps, in bytes.
const SCROLLBACK_BYTES: usize = 10_000_000;

/// The least time between two frames of a pane: what its program draws in
/// between goes in the next.
const FRAME_GAP: Duration = Duration::from_millis(4);

/// The longest a pane holds its frames while its program draws in one go
/// (mode 2026), as Ghostty holds them: past it, the pane turns the mode off,
/// as Ghostty does, and frames again.
const SYNC_HOLD: Duration = Duration::from_secs(1);

/// How soon a pane tries again to send a frame to a client that had no room.
const OWED_RETRY: Duration = Duration::from_millis(8);

/// The bytes a read takes from the program at most.
const READ_BYTES: usize = 64 * 1024;

/// The reads a turn of the loop makes at most before it sends frames.
const READS_PER_TURN: usize = 16;

/// The most messages a pane's queue holds.
const MESSAGES_MAX: usize = 256;

/// The messages a turn of the loop takes at most before it reads the program.
const MESSAGES_PER_TURN: usize = 64;

/// The most bytes a pane holds for its program that it has not read yet. A
/// client's input past them is dropped, and the pane reads no more of the
/// program until it reads them.
pub const UNREAD_BYTES_MAX: usize = 1024 * 1024;

/// The most a pane reads of what its program wrote as it ended.
const LAST_WORDS_BYTES_MAX: usize = 1024 * 1024;

/// How long after it last heard the program a pane looks again at what runs
/// in front of its terminal: a shell writes as it hands the terminal to a
/// command, and the command has it a little after.
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// How often a pane looks at what runs in front of its terminal while its
/// shell runs a command, which may come to read the terminal, or to stop,
/// with no word.
const LOOK_WHILE_GOING: Duration = Duration::from_secs(1);

/// The rows a turn of the wheel scrolls.
const WHEEL_ROWS: isize = 3;

/// The messages a client's stream holds.
pub const CLIENT_MESSAGES: usize = 64;

/// The messages of a client's stream kept for the server's words, which no
/// frame takes.
const FOR_WORDS: usize = 8;

const _: () = assert!(FOR_WORDS < CLIENT_MESSAGES);
const _: () = assert!(MESSAGES_PER_TURN <= MESSAGES_MAX);

/// What goes to a client.
pub type Out = mpsc::Sender<Result<api::ToClient, Status>>;

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

/// A client on a pane: where its frames go, its size and its terminal's
/// colours.
pub struct Viewer {
    pub client: ClientId,
    pub out: Out,
    pub size: Size,
    pub colours: Colours,
}

/// What a pane's thread is asked, in the order it was asked: by the model,
/// for its clients, and by the clients.
pub enum ToPane {
    Watch(Viewer),
    Unwatch(ClientId),
    Input(ClientId, Input),
    Resize(ClientId, Size),
    Dump(Dumped, oneshot::Sender<Result<String, Status>>),
}

/// What reaches a pane's thread: its queue, which is bounded, and its hang
/// up, which is not in the queue, as no full queue may hold it back.
#[derive(Clone)]
pub struct Handle {
    to: mpsc::Sender<ToPane>,
    closed: Arc<AtomicBool>,
    wake: Wake,
}

/// A pane's queue had no room.
#[derive(Debug)]
pub struct Busy;

/// A place in a pane's queue, held for a message.
pub struct Reserved {
    permit: OwnedPermit<ToPane>,
    wake: Wake,
}

impl Reserved {
    pub fn send(self, message: ToPane) {
        self.permit.send(message);
        self.wake.wake();
    }
}

impl Handle {
    /// Sends `message` to the pane once its queue has room: for what a
    /// client asks, who waits for that.
    pub async fn send(&self, message: ToPane) {
        if self.to.send(message).await.is_ok() {
            self.wake.wake();
        }
    }

    /// A place in the pane's queue, where it has room now: for the model,
    /// which may not wait under its lock. None where the pane's thread
    /// ended, which takes no more.
    pub fn reserve(&self) -> Result<Option<Reserved>, Busy> {
        match self.to.clone().try_reserve_owned() {
            Ok(permit) => Ok(Some(Reserved { permit, wake: self.wake.clone() })),
            Err(TrySendError::Full(_)) => Err(Busy),
            Err(TrySendError::Closed(_)) => Ok(None),
        }
    }

    /// Hangs up the pane's program, and ends its thread.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wake.wake();
    }
}

/// What wakes a pane's thread from its poll.
#[derive(Clone)]
struct Wake(Arc<UnixStream>);

impl Wake {
    fn wake(&self) {
        match (&*self.0).write(&[0]) {
            Ok(written) => assert_eq!(written, 1),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {},
            Err(error) => eprintln!("luneta-mux: a pane's wake failed: {error}"),
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
    pane: PaneId,
    pty: Pty,
    mut child: Child,
    size: Size,
) -> io::Result<Started> {
    let (to, inbox) = mpsc::channel(MESSAGES_MAX);
    let (exited, exits) = std_mpsc::sync_channel(1);
    let (wake, woken) = UnixStream::pair()?;
    wake.set_nonblocking(true)?;
    woken.set_nonblocking(true)?;
    let wake = Wake(Arc::new(wake));
    let closed = Arc::new(AtomicBool::new(false));
    let handle = Handle { to, closed: Arc::clone(&closed), wake: wake.clone() };
    let pid = child.id();
    thread::Builder::new().name(format!("pane {pane} waits")).spawn(move || {
        waits(pane, &mut child, &exited, &wake);
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
            exits,
            closed,
            woken,
            screen,
            program: Program::Runs { pty, unread: Vec::new() },
            buffer: vec![0; READ_BYTES].into_boxed_slice(),
            watchers: Vec::new(),
            dirty: false,
            framed: Instant::now().checked_sub(FRAME_GAP).unwrap_or_else(Instant::now),
            synced: None,
            title: String::new(),
            pwd: String::new(),
            host: host(),
            pid,
            marks: Marks::default(),
            shell: Shell::default(),
            looks: Looks::first(),
            told: Told::default(),
        };
        let failed = match panic::catch_unwind(AssertUnwindSafe(|| thread.run())) {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("it panicked".to_owned()),
        };
        if let Some(failed) = failed {
            eprintln!("luneta-mux: pane {pane} failed: {failed}");
            thread.program = Program::Gone;
            thread.mux.lost(pane);
        }
    })?;
    Ok(Started { handle, pid })
}

/// Waits for pane `pane`'s program to end, and tells the pane how it did.
fn waits(pane: PaneId, child: &mut Child, exited: &SyncSender<Exit>, wake: &Wake) {
    match child.wait() {
        Ok(status) => {
            if exited.try_send(Exit::of(status)).is_ok() {
                wake.wake();
            }
        },
        Err(error) => {
            eprintln!("luneta-mux: pane {pane}'s program could not be waited for: {error}");
        },
    }
}

/// What a client last got of a pane.
struct Shown {
    size: Size,
    view: View,
    flags: Flags,
}

struct Watcher {
    viewer: Viewer,
    back: usize,
    shown: Option<Shown>,
    owed: bool,
}

/// What the program does with a client's input.
enum Does {
    Write(Vec<u8>),
    Scroll(isize),
    Nothing,
}

/// The program, as the pane reaches it: by its pseudoterminal, with what it
/// has yet to read, the clients' input and the screen's answers, until it
/// ends or is hung up.
enum Program {
    Runs { pty: Pty, unread: Vec<u8> },
    Gone,
}

impl Program {
    /// Whether the program has something to read.
    fn owed(&self) -> bool {
        matches!(self, Program::Runs { unread, .. } if !unread.is_empty())
    }
}

struct Thread {
    pane: PaneId,
    mux: Arc<Mux>,
    inbox: mpsc::Receiver<ToPane>,
    exits: Receiver<Exit>,
    closed: Arc<AtomicBool>,
    woken: UnixStream,
    screen: Screen,
    program: Program,
    buffer: Box<[u8]>,
    watchers: Vec<Watcher>,
    dirty: bool,
    framed: Instant,
    /// Since when the program has held its frames, if it does.
    synced: Option<Instant>,
    title: String,
    /// What the program last said in OSC 7.
    pwd: String,
    /// This machine's name, as an OSC 7 names it.
    host: String,
    /// The program's pid, which leads its session.
    pid: u32,
    marks: Marks,
    shell: Shell,
    looks: Looks,
    told: Told,
}

/// What the model was last told runs in front, and what the shell said.
#[derive(Clone, Default, PartialEq)]
struct Told {
    foreground: Option<api::Foreground>,
    ran: Option<api::Ran>,
}

/// When a pane looks next at what runs in front of its terminal.
#[derive(Default)]
struct Looks {
    due: Option<Instant>,
    last: Option<Instant>,
    /// Whether the program wrote since the last look.
    heard: bool,
}

impl Looks {
    /// A look as the pane starts, at what its program runs: a program that
    /// writes nothing would have none. Its program has run its own by then,
    /// as `spawn` returns once it has.
    fn first() -> Looks {
        Looks { due: Some(Instant::now()), last: None, heard: false }
    }

    /// The program wrote: the pane looks now, or `LOOK_AGAIN` after the last
    /// look, where that is later, and again after that. A look due later
    /// comes as soon.
    fn heard(&mut self, now: Instant) {
        let soonest = self.last.map_or(now, |last| (last + LOOK_AGAIN).max(now));
        self.due = Some(self.due.map_or(soonest, |due| due.min(soonest)));
        self.heard = true;
        self.assert_paced();
    }

    /// The pane looked at `now`, while its shell runs a command where it is
    /// `going`.
    fn looked(&mut self, now: Instant, going: bool) {
        self.last = Some(now);
        self.due = if std::mem::take(&mut self.heard) {
            Some(now + LOOK_AGAIN)
        } else if going {
            Some(now + LOOK_WHILE_GOING)
        } else {
            None
        };
        self.assert_paced();
    }

    /// Asserts that no look is due sooner than `LOOK_AGAIN` after the last.
    fn assert_paced(&self) {
        if let (Some(due), Some(last)) = (self.due, self.last) {
            assert!(due >= last + LOOK_AGAIN, "{:?} after the last", due.duration_since(last));
        }
    }
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
            if self.closed.load(Ordering::Acquire) {
                self.program = Program::Gone;
                return Ok(());
            }
            if let Ok(exit) = self.exits.try_recv()
                && self.exited(exit)?
            {
                return Ok(());
            }
            for _ in 0..MESSAGES_PER_TURN {
                match self.inbox.try_recv() {
                    Ok(message) => self.take(message)?,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(()),
                }
            }
            if readable {
                self.read(READS_PER_TURN * READ_BYTES)?;
            }
            if writable || self.program.owed() {
                self.write()?;
            }
            if self.looks.due.is_some_and(|due| due <= Instant::now()) {
                self.look();
            }
            self.frame()?;
        }
    }

    /// Looks at what runs in front of the terminal, and, while the shell
    /// runs a command, at what that does.
    fn look(&mut self) {
        let now = Instant::now();
        let Program::Runs { pty, .. } = &self.program else {
            self.looks = Looks::default();
            return;
        };
        let group = pty.front_group();
        let foreground =
            group.and_then(|group| foreground(group.leader()).or_else(|| foreground(self.pid)));
        if self.shell.going()
            && let Some(group) = group
        {
            self.shell.saw(group, self.pid, Front::of_pane(pty, self.pid, group));
        }
        self.looks.looked(now, self.shell.going());
        self.tell(foreground);
    }

    /// Tells the model what runs in front, `foreground`, and what the shell
    /// said, where either changed since it was told last.
    fn tell(&mut self, foreground: Option<api::Foreground>) {
        let told = Told { foreground, ran: self.shell.said() };
        if told != self.told {
            self.told = told.clone();
            self.mux.looked(self.pane, told.foreground, told.ran);
        }
    }

    /// How long the loop waits for the program, a message, or the next look.
    fn timeout(&self) -> Option<Duration> {
        let look = self.looks.due.map(|due| due.saturating_duration_since(Instant::now()));
        match (self.framing(), look) {
            (Some(framing), Some(look)) => Some(framing.min(look)),
            (framing, look) => framing.or(look),
        }
    }

    /// How long the loop waits for the program or a message, for its frames.
    fn framing(&self) -> Option<Duration> {
        if !self.inbox.is_empty() {
            return Some(Duration::ZERO);
        }
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

    /// Waits for the wake, or the program, and says whether the program can
    /// be read, can be written, and whether the wake came. The program is
    /// not read while it has `UNREAD_BYTES_MAX` to read itself.
    fn poll(&self, timeout: Option<Duration>) -> io::Result<(bool, bool, bool)> {
        let wait = timeout.map(|timeout| Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        });
        let mut reads = false;
        let mut fds = [
            PollFd::new(&self.woken, PollFlags::IN),
            PollFd::new(&self.woken, PollFlags::empty()),
        ];
        let mut count = 1;
        if let Program::Runs { pty, unread } = &self.program {
            reads = unread.len() < UNREAD_BYTES_MAX;
            let mut flags = PollFlags::empty();
            if reads {
                flags |= PollFlags::IN;
            }
            if !unread.is_empty() {
                flags |= PollFlags::OUT;
            }
            fds[1] = PollFd::from_borrowed_fd(pty.near.as_fd(), flags);
            count = 2;
        }
        match rustix::event::poll(&mut fds[..count], wait.as_ref()) {
            Ok(_) => {},
            Err(rustix::io::Errno::INTR) => return Ok((false, false, false)),
            Err(error) => return Err(error.into()),
        }
        let woken = !fds[0].revents().is_empty();
        let near = if count == 2 { fds[1].revents() } else { PollFlags::empty() };
        let readable = reads && near.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR);
        Ok((readable, near.contains(PollFlags::OUT), woken))
    }

    fn take(&mut self, message: ToPane) -> io::Result<()> {
        match message {
            ToPane::Watch(viewer) => {
                let client = viewer.client;
                if viewer.colours != Colours::default() {
                    let Colours { foreground, background } = viewer.colours;
                    self.screen.colour(foreground, background).map_err(io::Error::other)?;
                }
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
            ToPane::Dump(dumped, answer) => {
                let text = match dumped {
                    Dumped::Screen => self.screen.dumped_screen(),
                    Dumped::Text => self.screen.dumped_text(),
                };
                let failed = |error| Status::internal(format!("the dump failed: {error}"));
                let _ = answer.send(text.map_err(failed));
            },
        }
        Ok(())
    }

    /// Reads what the program wrote as it ended, shows it, and says whether
    /// the pane went with the program.
    fn exited(&mut self, exit: Exit) -> io::Result<bool> {
        if let Program::Runs { unread, .. } = &mut self.program {
            unread.clear();
        }
        self.read(LAST_WORDS_BYTES_MAX)?;
        self.program = Program::Gone;
        self.shell.gone();
        self.looks = Looks::default();
        self.told = Told::default();
        self.dirty = true;
        self.frame()?;
        Ok(self.mux.ended(self.pane, exit))
    }

    fn watcher(&mut self, client: ClientId) -> Option<&mut Watcher> {
        self.watchers.iter_mut().find(|watcher| watcher.viewer.client == client)
    }

    /// Makes the pane the size of `client`, the latest to come to it, resize
    /// or do anything in it, as tmux's `window-size latest` does.
    fn follow(&mut self, client: ClientId) -> io::Result<()> {
        let Some(size) = self.watcher(client).map(|watcher| watcher.viewer.size) else {
            return Ok(());
        };
        if self.screen.size() == (size.columns, size.rows) {
            return Ok(());
        }
        let answers = self.screen.resize(size.columns, size.rows).map_err(io::Error::other)?;
        if let Program::Runs { pty, unread } = &mut self.program {
            pty.resize(size)?;
            unread.extend_from_slice(&answers);
        }
        let scrollback = self.screen.scrollback();
        for watcher in &mut self.watchers {
            watcher.back = watcher.back.min(scrollback);
        }
        self.dirty = true;
        Ok(())
    }

    /// Takes `client`'s input, which reaches the program, or scrolls the
    /// client's view. A client that has left the pane may still type in it,
    /// where it typed before it left.
    fn input(&mut self, client: ClientId, input: Input) -> io::Result<()> {
        self.follow(client)?;
        if input.types() {
            self.back(client, None);
        }
        match self.does(input).map_err(io::Error::other)? {
            Does::Write(bytes) => self.unread_push(&bytes),
            Does::Scroll(rows) => self.back(client, Some(rows)),
            Does::Nothing => {},
        }
        Ok(())
    }

    /// What the program does with `input`.
    fn does(&mut self, input: Input) -> Result<Does, Error> {
        let (columns, rows) = self.screen.size();
        let within = |column: u16, row: u16| column < columns.get() && row < rows.get();
        let screen = &mut self.screen;
        Ok(match input {
            Input::Key { action, key, mods, text, unshifted } => {
                Does::Write(screen.key(action, key, mods, text.as_deref(), unshifted)?)
            },
            Input::Paste(text) => Does::Write(screen.paste(&text)?),
            Input::Focus(focused) => Does::Write(screen.focus(focused)?),
            Input::Mouse { click, mods, column, row } if within(column, row) => {
                Does::Write(screen.mouse(click, mods, column, row)?)
            },
            Input::Wheel { wheel, mods, column, row } if within(column, row) => {
                wheeled(screen, wheel, mods, column, row)?
            },
            Input::Mouse { .. } | Input::Wheel { .. } => Does::Nothing,
            Input::Scroll(rows) => {
                Does::Scroll(isize::try_from(rows).expect("an i32 fits in an isize"))
            },
        })
    }

    /// Gives the program `bytes` to read, where it runs and has room.
    fn unread_push(&mut self, bytes: &[u8]) {
        let Program::Runs { unread, .. } = &mut self.program else {
            return;
        };
        if unread.len() + bytes.len() <= UNREAD_BYTES_MAX {
            unread.extend_from_slice(bytes);
        } else {
            eprintln!(
                "luneta-mux: pane {} has {} unread bytes: input dropped",
                self.pane,
                unread.len()
            );
        }
    }

    /// Moves `client`'s view `by` rows back into the scrollback, or to the
    /// bottom where `by` is none.
    fn back(&mut self, client: ClientId, by: Option<isize>) {
        let scrollback = self.screen.scrollback();
        let Some(watcher) = self.watcher(client) else {
            return;
        };
        let back = match by {
            None => 0,
            Some(by) => watcher.back.saturating_add_signed(by).min(scrollback),
        };
        assert!(back <= scrollback);
        if back != watcher.back {
            watcher.back = back;
            self.dirty = true;
        }
    }

    /// Reads up to `most` bytes of what the program wrote, as far as it
    /// wrote, and while it has less than `UNREAD_BYTES_MAX` to read itself.
    fn read(&mut self, most: usize) -> io::Result<()> {
        let Program::Runs { pty, unread } = &mut self.program else {
            return Ok(());
        };
        let before = self.screen.scrollback();
        let mut total = 0;
        let mut marks: Vec<Mark> = Vec::new();
        while total < most && unread.len() < UNREAD_BYTES_MAX {
            match pty.near.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(read) => {
                    total += read;
                    self.marks.read(&self.buffer[..read], &mut marks);
                    let answers = self.screen.write(&self.buffer[..read]);
                    unread.extend_from_slice(&answers);
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
        self.looks.heard(Instant::now());
        if !marks.is_empty() {
            let now = Now::current();
            let job = |group: Group| {
                let leader = process::process(group.leader())?;
                Some(if leader.stopped { Job::Stopped } else { Job::Runs })
            };
            for mark in marks {
                self.shell.marked(mark, now, job);
            }
        }
        self.dirty = true;
        let after = self.screen.scrollback();
        let grown = after.saturating_sub(before);
        for watcher in &mut self.watchers {
            if watcher.back > 0 {
                watcher.back = (watcher.back + grown).min(after);
            }
        }
        self.synced = match (self.screen.syncs(), self.synced) {
            (true, None) => Some(Instant::now()),
            (true, since) => since,
            (false, _) => None,
        };
        let title = self.screen.title().unwrap_or_else(|| self.title.clone());
        let pwd = self.screen.pwd().unwrap_or_else(|| self.pwd.clone());
        if title != self.title || pwd != self.pwd {
            let cwd = (pwd != self.pwd).then(|| local_cwd(&pwd, &self.host)).flatten();
            self.title.clone_from(&title);
            self.pwd = pwd;
            self.mux.titled(self.pane, title, cwd);
        }
        Ok(())
    }

    /// Writes what the program has yet to read, as much as it takes now.
    fn write(&mut self) -> io::Result<()> {
        let Program::Runs { pty, unread } = &mut self.program else {
            return Ok(());
        };
        while !unread.is_empty() {
            match pty.near.write(unread) {
                Ok(written) => {
                    assert!(written > 0, "the program took none of {} bytes", unread.len());
                    unread.drain(..written);
                },
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == ErrorKind::Interrupted => {},
                Err(error)
                    if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) =>
                {
                    unread.clear();
                },
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    /// Sends each client on the pane what changed since its last frame, once
    /// a frame is due. A client whose stream ended is let go of.
    fn frame(&mut self) -> io::Result<()> {
        self.watchers.retain(|watcher| !watcher.viewer.out.is_closed());
        if self.watchers.is_empty() {
            self.dirty = false;
            return Ok(());
        }
        let owed = self.watchers.iter().any(|watcher| watcher.owed);
        let due = self.dirty || owed;
        if !due {
            return Ok(());
        }
        if self.synced.is_some_and(|since| since.elapsed() >= SYNC_HOLD) {
            self.screen.unsync().map_err(io::Error::other)?;
            self.synced = None;
        }
        if self.synced.is_some() || self.framed.elapsed() < FRAME_GAP {
            return Ok(());
        }
        self.framed = Instant::now();
        self.dirty = false;
        let (columns, rows) = self.screen.size();
        let size = Size { columns, rows };
        let (clusters, tracks) = (self.screen.clusters(), self.screen.tracks_mouse());
        let scrollback = self.screen.scrollback();
        let mut views: Vec<(usize, View)> = Vec::new();
        for watcher in &mut self.watchers {
            if watcher.viewer.out.capacity() <= FOR_WORDS {
                watcher.owed = true;
                continue;
            }
            if !views.iter().any(|(back, _)| *back == watcher.back) {
                let view = self.screen.view(watcher.back).map_err(io::Error::other)?;
                views.push((watcher.back, view));
            }
            let view = &views
                .iter()
                .find(|(back, _)| *back == watcher.back)
                .expect("a view from where the client looks, drawn above")
                .1;
            let flags = Flags { clusters, tracks, back: watcher.back, scrollback };
            let unchanged = watcher.shown.as_ref().is_some_and(|shown| {
                (shown.size, shown.flags) == (size, flags) && shown.view == *view
            });
            if unchanged {
                watcher.owed = false;
                continue;
            }
            let before = watcher.shown.as_ref().map(|shown| (shown.size, &shown.view));
            let said = frame::said(self.pane, view, before, size, flags);
            let message = api::ToClient { event: Some(api::to_client::Event::Frame(said)) };
            match watcher.viewer.out.try_send(Ok(message)) {
                Ok(()) => {
                    watcher.shown = Some(Shown { size, view: view.clone(), flags });
                    watcher.owed = false;
                },
                Err(TrySendError::Full(_)) => watcher.owed = true,
                Err(TrySendError::Closed(_)) => watcher.owed = false,
            }
        }
        assert!(views.len() <= self.watchers.len());
        Ok(())
    }
}

/// The process `pid`, as the protocol says what runs in front: none where it
/// is gone.
fn foreground(pid: u32) -> Option<api::Foreground> {
    Some(api::Foreground { pid, argv: process::argv(pid)? })
}

/// This machine's name.
fn host() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

/// The path on this machine, named `host`, that an OSC 7 `said`: none where
/// it names another machine, or no absolute path in UTF-8.
fn local_cwd(said: &str, host: &str) -> Option<PathBuf> {
    let after = said.strip_prefix("file://")?;
    let (named, path) = after.split_at(after.find('/')?);
    let local = named.is_empty()
        || named.eq_ignore_ascii_case("localhost")
        || named.eq_ignore_ascii_case(host);
    if !local {
        return None;
    }
    let path = percent_encoding::percent_decode_str(path).decode_utf8().ok()?;
    if path.contains('\0') {
        return None;
    }
    Some(PathBuf::from(path.as_ref())).filter(|path| path.is_absolute())
}

/// What the program does with a turn of the wheel: a click, where it takes
/// the mouse, arrow keys, where it asked for them, and else a scroll of the
/// client's view.
fn wheeled(
    screen: &mut Screen,
    wheel: Wheel,
    mods: Mods,
    column: u16,
    row: u16,
) -> Result<Does, Error> {
    if screen.tracks_mouse() {
        return Ok(Does::Write(screen.wheel(wheel, mods, column, row)?));
    }
    if screen.wheel_is_arrows() {
        return Ok(Does::Write(arrows(screen, wheel)?));
    }
    Ok(match wheel {
        Wheel::Up => Does::Scroll(WHEEL_ROWS),
        Wheel::Down => Does::Scroll(-WHEEL_ROWS),
        Wheel::Left | Wheel::Right => Does::Nothing,
    })
}

/// What the program reads of a turn of the wheel as arrow keys.
fn arrows(screen: &mut Screen, wheel: Wheel) -> Result<Vec<u8>, Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// What the program writes brings a look due later forward, to no
    /// sooner than `LOOK_AGAIN` after the last, and has another follow it.
    #[test]
    fn what_the_program_writes_brings_a_later_look_forward() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut looks = Looks::first();
        looks.heard(start);
        assert_eq!(looks.due, Some(start));
        looks.looked(start, true);
        assert_eq!(looks.due, Some(at(100)));
        looks.looked(at(100), true);
        assert_eq!(looks.due, Some(at(1100)));
        looks.heard(at(300));
        assert_eq!(looks.due, Some(at(300)));
        looks.looked(at(300), true);
        looks.heard(at(310));
        assert_eq!(looks.due, Some(at(400)));
        looks.looked(at(400), false);
        assert_eq!(looks.due, Some(at(500)));
        looks.looked(at(500), false);
        assert_eq!(looks.due, None);
    }

    /// An OSC 7 names a folder of this machine's, which it may spell in
    /// escapes, or a folder of another's, which is none of this one's.
    #[test]
    fn a_cwd_is_one_this_machine_has() {
        let cwd = |said: &str| local_cwd(said, "here");
        assert_eq!(cwd("file://here/home/a%20b"), Some(PathBuf::from("/home/a b")));
        assert_eq!(cwd("file://HERE/tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(cwd("file:///tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(cwd("file://localhost/tmp"), Some(PathBuf::from("/tmp")));
        assert_eq!(cwd("file://there/tmp"), None);
        assert_eq!(cwd("/tmp"), None);
        assert_eq!(cwd("file://here"), None);
        assert_eq!(cwd("file://here/%FE"), None);
        assert_eq!(cwd("file://here/a%00b"), None);
    }
}
