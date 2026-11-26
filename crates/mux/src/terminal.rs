use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self as std_mpsc, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use libghostty_vt::Error;
use libghostty_vt::key::{Action, Key, Mods};
use rustix::event::{PollFd, PollFlags, Timespec};
use tokio::sync::mpsc::{self, error::TryRecvError, error::TrySendError};
use tokio::sync::{oneshot, watch};
use tonic::Status;

use crate::api;
use crate::asked::{Colours, Dumped, Input, Size, TerminalKey};
use crate::frame::{self, Flags};
use crate::front;
use crate::id::{ClientId, TerminalId};
use crate::leader::Leader;
use crate::looker::{Asked, Gone, Looker, Seen};
use crate::marks::Mark;
use crate::mux::Mux;
use crate::panics::{self, Panicked};
use crate::process::{self, Group};
use crate::pty::{Led, Pty, Queue, Raise, SignalKey};
use crate::ran::{Job, Now, Shell};
use crate::screen::{Answers, Looked, Pushed, Screen, View, Wheel};
use crate::unread::{Kept, Oldest, Refused, TYPED_BYTES_MAX, Unread};

const SCROLLBACK_BYTES: usize = 10_000_000;

const FRAME_GAP: Duration = Duration::from_millis(4);

const SYNC_HOLD: Duration = Duration::from_secs(1);

const OWED_RETRY: Duration = Duration::from_millis(8);

const READ_BYTES: usize = 64 * 1024;

const READS_PER_TURN: usize = 16;

const MESSAGES_MAX: usize = 256;

const MESSAGES_PER_TURN: usize = 64;

const LAST_WORDS_BYTES_MAX: usize = 1024 * 1024;

const LOOK_AGAIN: Duration = Duration::from_millis(100);

const LOOK_WHILE_GOING: Duration = Duration::from_secs(1);

const WHEEL_ROWS: isize = 3;

pub const CLIENT_MESSAGES: usize = 64;

const FOR_WORDS: usize = 8;

const _: () = assert!(FOR_WORDS < CLIENT_MESSAGES);
const _: () = assert!(MESSAGES_PER_TURN <= MESSAGES_MAX);

pub type ViewerOut = mpsc::Sender<Result<api::ToViewer, Status>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Code(i32),
    Signal(i32),
}

impl Exit {
    pub(crate) fn of(status: ExitStatus) -> Exit {
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

#[derive(Clone)]
pub struct Viewer {
    pub client: ClientId,
    pub frames: ViewerOut,
    pub colours: Colours,
    pub came: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sent {
    Taken,
    Full,
    Closed,
}

fn sent(frames: &ViewerOut, frame: api::Frame) -> Sent {
    let event = Some(api::to_viewer::Event::Frame(frame));
    match frames.try_send(Ok(api::ToViewer { event })) {
        Ok(()) => Sent::Taken,
        Err(TrySendError::Full(_)) => Sent::Full,
        Err(TrySendError::Closed(_)) => Sent::Closed,
    }
}

#[derive(Clone)]
pub struct Wanted {
    pub size: Size,
    pub viewers: Arc<[Viewer]>,
}

impl Wanted {
    fn same(&self, other: &Wanted) -> bool {
        let same = |one: &Viewer, other: &Viewer| {
            (one.client, one.colours, one.came) == (other.client, other.colours, other.came)
        };
        let viewers = self.viewers.iter().zip(other.viewers.iter());
        self.size == other.size
            && self.viewers.len() == other.viewers.len()
            && viewers.into_iter().all(|(one, other)| same(one, other))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Still,
    LetGo,
}

pub enum ToTerminal {
    Input(ClientId, Input),
    Dump(Dumped, oneshot::Sender<Result<String, Status>>),
    Look(oneshot::Sender<Result<Looked, Status>>),
    Press(u64, Vec<Input>, oneshot::Sender<Result<(), Status>>),
}

#[derive(Clone)]
pub struct Handle {
    to: mpsc::Sender<ToTerminal>,
    closed: Arc<AtomicBool>,
    wake: Wake,
    wanted: Arc<watch::Sender<Wanted>>,
}

impl Handle {
    pub async fn send(&self, message: ToTerminal) {
        if self.to.send(message).await.is_ok() {
            self.wake.wake();
        }
    }

    pub fn want(&self, wanted: Wanted) {
        let changed = self.wanted.send_if_modified(|was| {
            if was.same(&wanted) {
                return false;
            }
            *was = wanted;
            true
        });
        if changed {
            self.wake.wake();
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.wake.wake();
    }
}

#[derive(Clone)]
struct Wake(Arc<UnixStream>);

impl Wake {
    fn wake(&self) {
        match (&*self.0).write(&[0]) {
            Ok(written) => assert_eq!(written, 1),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {},
            Err(error) => eprintln!("lune-mux: a terminal's wake failed: {error}"),
        }
    }
}

pub struct Started {
    pub handle: Handle,
    pub pid: u32,
    pub placing: Placing,
}

pub struct Placing(SyncSender<()>);

impl Placing {
    pub fn placed(self) {
        self.0.send(()).expect("the terminal's thread waits to be placed");
    }
}

pub fn start(
    mux: Arc<Mux>,
    key: TerminalKey,
    pty: Pty,
    child: Led,
    size: Size,
) -> io::Result<Started> {
    let terminal = key.id;
    let (to, inbox) = mpsc::channel(MESSAGES_MAX);
    let (placing, placed) = std_mpsc::sync_channel(1);
    let (wake, woken) = UnixStream::pair()?;
    wake.set_nonblocking(true)?;
    woken.set_nonblocking(true)?;
    let wake = Wake(Arc::new(wake));
    let closed = Arc::new(AtomicBool::new(false));
    let (wanted, wants) = watch::channel(Wanted { size, viewers: Arc::from(Vec::new()) });
    let wanted = Arc::new(wanted);
    let handle = Handle { to, closed: Arc::clone(&closed), wake: wake.clone(), wanted };
    let waking = wake.clone();
    let (leader, exits) = Leader::waited(terminal, child, mux.leaders(), move || waking.wake())?;
    let pid = leader.pid();
    let waking = wake.clone();
    let looker = Looker::started(terminal, move || waking.wake())?;
    thread::Builder::new().name(format!("terminal {terminal}")).spawn(move || {
        if placed.recv().is_err() {
            return;
        }
        let screen = match Screen::new(size, SCROLLBACK_BYTES) {
            Ok(screen) => screen,
            Err(error) => {
                eprintln!("lune-mux: terminal {terminal}'s screen could not be made: {error}");
                drop(pty);
                drop(looker);
                mux.lost(terminal);
                leader.end();
                return;
            },
        };
        {
            let mut thread = Thread {
                key,
                mux,
                inbox,
                wants,
                exits,
                closed,
                woken,
                screen,
                program: Program::Runs(Box::new(Running {
                    pty,
                    unread: Unread::default(),
                    looker,
                    looks: Looks::first(),
                })),
                buffer: vec![0; READ_BYTES].into_boxed_slice(),
                watchers: Vec::new(),
                dirty: false,
                framed: Instant::now().checked_sub(FRAME_GAP).unwrap_or_else(Instant::now),
                synced: None,
                title: String::new(),
                pwd: String::new(),
                host: host(),
                pid,
                shell: Shell::default(),
                told: Told::default(),
                statuses_version: 0,
            };
            let failed = match panics::contained(|| thread.run()) {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(Panicked) => Some("it panicked".to_owned()),
            };
            thread.program = Program::Gone;
            if let Some(failed) = failed {
                eprintln!("lune-mux: terminal {terminal} failed: {failed}");
                thread.mux.lost(terminal);
            }
        }
        leader.end();
    })?;
    Ok(Started { handle, pid, placing: Placing(placing) })
}

struct Shown {
    view: View,
    flags: Flags,
}

struct Watcher {
    viewer: Viewer,
    back: usize,
    shown: Option<Shown>,
    owed: bool,
}

enum Does {
    Write(Vec<u8>),
    Scroll(isize),
    Nothing,
}

struct Running {
    pty: Pty,
    unread: Unread,
    looker: Looker,
    looks: Looks,
}

enum Program {
    Runs(Box<Running>),
    Gone,
}

impl Program {
    fn owed(&self) -> bool {
        matches!(self, Program::Runs(running) if !running.unread.is_empty())
    }
}

struct Thread {
    key: TerminalKey,
    mux: Arc<Mux>,
    inbox: mpsc::Receiver<ToTerminal>,
    wants: watch::Receiver<Wanted>,
    exits: Receiver<Exit>,
    closed: Arc<AtomicBool>,
    woken: UnixStream,
    screen: Screen,
    program: Program,
    buffer: Box<[u8]>,
    watchers: Vec<Watcher>,
    dirty: bool,
    framed: Instant,
    synced: Option<Instant>,
    title: String,
    pwd: String,
    host: String,
    pid: u32,
    shell: Shell,
    told: Told,
    statuses_version: u64,
}

#[derive(Clone, Default, PartialEq)]
struct Told {
    foreground: Option<api::Foreground>,
    ran: Option<api::Ran>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pace {
    Idle { due: Option<Instant> },
    InFlight,
}

struct Looks {
    pace: Pace,
    last: Option<Instant>,
    heard: bool,
}

impl Looks {
    fn first() -> Looks {
        Looks { pace: Pace::Idle { due: Some(Instant::now()) }, last: None, heard: false }
    }

    #[must_use]
    fn waits_until(&self) -> Option<Instant> {
        match self.pace {
            Pace::Idle { due } => due,
            Pace::InFlight => None,
        }
    }

    #[must_use]
    fn is_in_flight(&self) -> bool {
        self.pace == Pace::InFlight
    }

    fn asked(&mut self) {
        assert!(matches!(self.pace, Pace::Idle { due: Some(_) }), "a look is due before it is asked");
        self.pace = Pace::InFlight;
    }

    fn answered(&mut self, now: Instant, going: bool) {
        assert_eq!(self.pace, Pace::InFlight, "a seen look was asked");
        self.schedule_next(now, going);
    }

    fn looked_locally(&mut self, now: Instant, going: bool) {
        assert!(matches!(self.pace, Pace::Idle { due: Some(_) }), "a look is due before it runs");
        self.schedule_next(now, going);
    }

    fn heard(&mut self, now: Instant) {
        self.heard = true;
        let Pace::Idle { due } = self.pace else {
            return;
        };
        let soonest = self.last.map_or(now, |last| (last + LOOK_AGAIN).max(now));
        self.pace = Pace::Idle { due: Some(due.map_or(soonest, |due| due.min(soonest))) };
        self.assert_paced();
    }

    fn schedule_next(&mut self, now: Instant, going: bool) {
        if let Some(last) = self.last {
            assert!(now >= last, "time does not go backwards");
        }
        self.last = Some(now);
        let due = if std::mem::take(&mut self.heard) {
            Some(now + LOOK_AGAIN)
        } else if going {
            Some(now + LOOK_WHILE_GOING)
        } else {
            None
        };
        self.pace = Pace::Idle { due };
        self.assert_paced();
    }

    fn assert_paced(&self) {
        if let (Pace::Idle { due: Some(due) }, Some(last)) = (self.pace, self.last) {
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
            if self.want()? == Held::LetGo {
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
            self.seen()?;
            if let Program::Runs(running) = &self.program
                && running.looks.waits_until().is_some_and(|due| due <= Instant::now())
            {
                self.look()?;
            }
            self.frame()?;
        }
    }

    fn look(&mut self) -> io::Result<()> {
        let now = Instant::now();
        let terminal = self.key.id;
        let Program::Runs(running) = &mut self.program else {
            return Ok(());
        };
        assert!(!running.looks.is_in_flight(), "a terminal asks one look at a time");
        assert!(running.looks.waits_until().is_some(), "a look is due before it is asked");
        let going = self.shell.going();
        let Some(group) = running.pty.front_group() else {
            running.looks.looked_locally(now, going);
            self.tell(None);
            return Ok(());
        };
        let front = going.then(|| front::Snapshot {
            modes: pty_modes(terminal, &running.pty),
            device: running.pty.device(),
        });
        let asked = Asked {
            program: self.pid,
            group,
            front,
            shell_version: self.shell.version(),
        };
        running.looker.ask(asked).map_err(looks_gone)?;
        running.looks.asked();
        Ok(())
    }

    fn seen(&mut self) -> io::Result<()> {
        let Program::Runs(running) = &mut self.program else {
            return Ok(());
        };
        let Some(Seen { foreground, front, shell_version }) =
            running.looker.take_seen().map_err(looks_gone)?
        else {
            return Ok(());
        };
        let going = self.shell.going();
        running.looks.answered(Instant::now(), going);
        let current = self.shell.version();
        assert!(shell_version <= current, "a look cannot come from future marks");
        if shell_version != current {
            return Ok(());
        }
        assert_eq!(front.is_some(), self.shell.going());
        if let Some((group, front_state)) = front {
            self.shell.saw(group, self.pid, front_state);
        }
        self.tell(foreground);
        Ok(())
    }

    fn tell(&mut self, foreground: Option<api::Foreground>) {
        let told = Told { foreground, ran: self.shell.said() };
        if told != self.told {
            self.told = told.clone();
            self.mux.looked(self.key.id, told.foreground, told.ran);
        }
    }

    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let look = match &self.program {
            Program::Runs(running) => {
                running.looks.waits_until().map(|due| due.saturating_duration_since(now))
            },
            Program::Gone => None,
        };
        match (self.framing(), look) {
            (Some(framing), Some(look)) => Some(framing.min(look)),
            (framing, look) => framing.or(look),
        }
    }

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

    fn poll(&self, timeout: Option<Duration>) -> io::Result<(bool, bool, bool)> {
        let wait = timeout.map(|timeout| Timespec {
            tv_sec: timeout.as_secs().try_into().unwrap_or(i64::MAX),
            tv_nsec: timeout.subsec_nanos().into(),
        });
        let mut fds = [
            PollFd::new(&self.woken, PollFlags::IN),
            PollFd::new(&self.woken, PollFlags::empty()),
        ];
        let mut count = 1;
        if let Program::Runs(running) = &self.program {
            let mut flags = PollFlags::IN;
            if !running.unread.is_empty() {
                flags |= PollFlags::OUT;
            }
            fds[1] = PollFd::from_borrowed_fd(running.pty.near.as_fd(), flags);
            count = 2;
        }
        match rustix::event::poll(&mut fds[..count], wait.as_ref()) {
            Ok(_) => {},
            Err(rustix::io::Errno::INTR) => return Ok((false, false, false)),
            Err(error) => return Err(error.into()),
        }
        let woken = !fds[0].revents().is_empty();
        let near = if count == 2 { fds[1].revents() } else { PollFlags::empty() };
        let readable = near.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR);
        Ok((readable, near.contains(PollFlags::OUT), woken))
    }

    fn want(&mut self) -> io::Result<Held> {
        match self.wants.has_changed() {
            Ok(true) => {},
            Ok(false) => return Ok(Held::Still),
            Err(_) => return Ok(Held::LetGo),
        }
        let Wanted { size, viewers } = self.wants.borrow_and_update().clone();
        self.watchers.retain(|watcher| viewers.iter().any(|it| it.client == watcher.viewer.client));
        for viewer in viewers.iter().cloned() {
            if let Some(watcher) =
                self.watchers.iter_mut().find(|watcher| watcher.viewer.client == viewer.client)
            {
                if watcher.viewer.came != viewer.came {
                    watcher.shown = None;
                    self.dirty = true;
                }
                watcher.viewer = viewer;
                continue;
            }
            if viewer.colours != Colours::default() {
                let Colours { foreground, background } = viewer.colours;
                self.screen.colour(foreground, background).map_err(io::Error::other)?;
            }
            self.watchers.push(Watcher { viewer, back: 0, shown: None, owed: false });
            self.dirty = true;
        }
        assert_eq!(self.watchers.len(), viewers.len(), "a watcher for each viewer the model wants");
        self.resized(size)?;
        Ok(Held::Still)
    }

    fn take(&mut self, message: ToTerminal) -> io::Result<()> {
        match message {
            ToTerminal::Input(client, input) => match self.want()? {
                Held::Still => self.input(client, input)?,
                Held::LetGo => {},
            },
            ToTerminal::Dump(dumped, answer) => {
                let text = match dumped {
                    Dumped::Screen => self.screen.dumped_screen(),
                    Dumped::Text => self.screen.dumped_text(),
                };
                let failed = |error| Status::internal(format!("the dump failed: {error}"));
                let _ = answer.send(text.map_err(failed));
            },
            ToTerminal::Look(answer) => {
                let failed = |error| Status::internal(format!("the look failed: {error}"));
                let _ = answer.send(self.screen.looked().map_err(failed));
            },
            ToTerminal::Press(shown, inputs, answer) => {
                let pressed = match self.want()? {
                    Held::Still => self.press(shown, inputs),
                    Held::LetGo => {
                        Err(Status::not_found(format!("terminal {} ended", self.key.id)))
                    },
                };
                let _ = answer.send(pressed);
            },
        }
        Ok(())
    }

    fn press(&mut self, shown: u64, inputs: Vec<Input>) -> Result<(), Status> {
        assert!(!inputs.is_empty(), "something is pressed");
        let failed = |error: Error| Status::internal(format!("the press failed: {error}"));
        if self.screen.looked().map_err(failed)?.shown != shown {
            return Err(Status::aborted("the screen changed since it was looked at"));
        }
        let mut bytes = Vec::new();
        for input in inputs {
            assert!(matches!(input, Input::Key { .. } | Input::Paste(_)), "{input:?} is pressed");
            match self.does(input).map_err(failed)? {
                Does::Write(written) => bytes.extend(written),
                Does::Scroll(_) | Does::Nothing => {},
            }
        }
        let terminal = self.key.id;
        let Program::Runs(running) = &mut self.program else {
            let ended = format!("terminal {terminal}'s program ended");
            return Err(Status::failed_precondition(ended));
        };
        type_into(terminal, &running.pty, &mut running.unread, &bytes).map_err(|refused| {
            let told = refused_text(terminal, refused);
            match refused {
                Refused::Behind { .. } => Status::resource_exhausted(told),
                Refused::Larger { .. } => Status::invalid_argument(told),
            }
        })
    }

    fn exited(&mut self, exit: Exit) -> io::Result<bool> {
        if let Program::Runs(running) = &mut self.program {
            running.unread.clear();
        }
        self.read(LAST_WORDS_BYTES_MAX)?;
        let ended_statuses = self.screen.program_exited();
        self.statuses_version = self.screen.program_statuses_version();
        self.program = Program::Gone;
        self.shell.gone();
        self.told = Told::default();
        self.dirty = true;
        self.frame()?;
        Ok(self.mux.ended(self.key.id, exit, ended_statuses))
    }

    fn tell_statuses(&mut self) {
        let version = self.screen.program_statuses_version();
        if version == self.statuses_version {
            return;
        }
        self.statuses_version = version;
        let statuses = self.screen.program_statuses();
        assert!(statuses.children.len() <= crate::program_status::RECORDS_MAX);
        self.mux.program_statuses(self.key.id, statuses);
    }

    fn watcher(&mut self, client: ClientId) -> Option<&mut Watcher> {
        self.watchers.iter_mut().find(|watcher| watcher.viewer.client == client)
    }

    fn resized(&mut self, size: Size) -> io::Result<()> {
        if self.screen.size() == size {
            return Ok(());
        }
        let answers = self.screen.resize(size).map_err(io::Error::other)?;
        if let Program::Runs(running) = &self.program {
            running.pty.resize(size)?;
        }
        self.answered(&answers);
        let scrollback = self.screen.scrollback();
        for watcher in &mut self.watchers {
            watcher.back = watcher.back.min(scrollback);
        }
        self.dirty = true;
        Ok(())
    }

    fn input(&mut self, client: ClientId, input: Input) -> io::Result<()> {
        if input.types() {
            self.back(client, None);
        }
        match self.does(input).map_err(io::Error::other)? {
            Does::Write(bytes) => {
                let terminal = self.key.id;
                if let Program::Runs(running) = &mut self.program
                    && let Err(refused) =
                        type_into(terminal, &running.pty, &mut running.unread, &bytes)
                {
                    eprintln!("lune-mux: {}: input dropped", refused_text(terminal, refused));
                }
            },
            Does::Scroll(rows) => self.back(client, Some(rows)),
            Does::Nothing => {},
        }
        Ok(())
    }

    fn does(&mut self, input: Input) -> Result<Does, Error> {
        let size = self.screen.size();
        let within = |column, row| size.holds(column, row);
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

    fn answered(&mut self, answers: &Answers) {
        let Program::Runs(running) = &mut self.program else {
            return;
        };
        answers_lost(self.key.id, running.unread.answer(answers));
    }

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

    fn read(&mut self, most: usize) -> io::Result<()> {
        let Program::Runs(running) = &mut self.program else {
            return Ok(());
        };
        let Running { pty, unread, looks, .. } = &mut **running;
        let mut total = 0;
        let mut marks: Vec<Mark> = Vec::new();
        let mut pushed = Pushed::Rows(0);
        let mut too_long = 0;
        let mut oldest = Oldest::Kept;
        while total < most {
            match pty.near.read(&mut self.buffer) {
                Ok(0) => break,
                Ok(read) => {
                    total += read;
                    let written =
                        self.screen.write(&self.buffer[..read]).map_err(io::Error::other)?;
                    let kept = unread.answer(&written.answers);
                    too_long += kept.too_long;
                    if kept.oldest == Oldest::Lost {
                        oldest = Oldest::Lost;
                    }
                    marks.extend(written.marks);
                    pushed = pushed.then(written.pushed);
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
        answers_lost(self.key.id, Kept { too_long, oldest });
        if total == 0 {
            return Ok(());
        }
        looks.heard(Instant::now());
        if !marks.is_empty() {
            let now = Now::current();
            let in_front = pty.front_group() == Some(Group(self.pid));
            let job = |group: Group| {
                let leader = process::process(group.leader())?;
                Some(if leader.stopped { Job::Stopped } else { Job::Runs })
            };
            self.shell.read(marks, now, in_front, job);
        }
        self.dirty = true;
        let scrollback = self.screen.scrollback();
        for watcher in &mut self.watchers {
            watcher.back = pushed.moved(watcher.back, scrollback);
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
            self.mux.titled(self.key.id, title, cwd);
        }
        self.tell_statuses();
        Ok(())
    }

    fn write(&mut self) -> io::Result<()> {
        let Program::Runs(running) = &mut self.program else {
            return Ok(());
        };
        let Running { pty, unread, .. } = &mut **running;
        while let Some(lined_up) = unread.line_up() {
            match pty.near.write(lined_up) {
                Ok(written) => {
                    assert!(written > 0, "the program took none of {} bytes", lined_up.len());
                    unread.sent(written);
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

    fn frame(&mut self) -> io::Result<()> {
        self.watchers.retain(|watcher| !watcher.viewer.frames.is_closed());
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
        let (clusters, tracks) = (self.screen.clusters(), self.screen.tracks_mouse());
        let scrollback = self.screen.scrollback();
        let mut views: Vec<(usize, View)> = Vec::new();
        for watcher in &mut self.watchers {
            if watcher.viewer.frames.capacity() <= FOR_WORDS {
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
            let unchanged = watcher
                .shown
                .as_ref()
                .is_some_and(|shown| shown.flags == flags && shown.view == *view);
            if unchanged {
                watcher.owed = false;
                continue;
            }
            let before = watcher.shown.as_ref().map(|shown| &shown.view);
            let said = frame::said(self.key, view, before, flags);
            match sent(&watcher.viewer.frames, said) {
                Sent::Taken => {
                    watcher.shown = Some(Shown { view: view.clone(), flags });
                    watcher.owed = false;
                },
                Sent::Full => watcher.owed = true,
                Sent::Closed => watcher.owed = false,
            }
        }
        assert!(views.len() <= self.watchers.len());
        Ok(())
    }
}

fn type_into(
    terminal: TerminalId,
    pty: &Pty,
    unread: &mut Unread,
    bytes: &[u8],
) -> Result<(), Refused> {
    let [byte] = *bytes else {
        return unread.type_in(bytes);
    };
    let key = match key_behind(pty, unread, byte) {
        Ok(Some(key)) => key,
        Ok(None) => return unread.type_in(bytes),
        Err(error) => {
            eprintln!("lune-mux: terminal {terminal}'s input could not be read: {error}");
            return unread.type_in(bytes);
        },
    };
    let signal = key.signal.name();
    let Raise { queue, signalled } = pty.raise(key);
    if queue == Queue::Flushed {
        unread.clear();
    }
    let Err(error) = signalled else {
        eprintln!(
            "lune-mux: terminal {terminal}'s program is behind on its input: {signal} raised"
        );
        return Ok(());
    };
    let queue = queue.name();
    eprintln!(
        "lune-mux: terminal {terminal}'s {signal} could not be raised, its input {queue}: {error}: \
        typed"
    );
    unread.type_in(bytes)
}

fn key_behind(pty: &Pty, unread: &Unread, byte: u8) -> io::Result<Option<SignalKey>> {
    if unread.is_empty() && !pty.line_full()? {
        return Ok(None);
    }
    pty.signal_key(byte)
}

fn refused_text(terminal: TerminalId, refused: Refused) -> String {
    match refused {
        Refused::Behind { typed_bytes } => {
            format!("terminal {terminal} has {typed_bytes} typed bytes unread")
        },
        Refused::Larger { bytes } => {
            format!("terminal {terminal} takes {TYPED_BYTES_MAX} typed bytes at most, not {bytes}")
        },
    }
}

fn answers_lost(terminal: TerminalId, kept: Kept) {
    if kept.too_long > 0 {
        let too_long = kept.too_long;
        eprintln!("lune-mux: terminal {terminal}'s screen gave {too_long} answers too long to keep");
    }
    if kept.oldest == Oldest::Lost {
        eprintln!(
            "lune-mux: terminal {terminal}'s program reads too few answers: the oldest are lost"
        );
    }
}

fn pty_modes(terminal: TerminalId, pty: &Pty) -> Option<crate::pty::Modes> {
    match pty.modes() {
        Ok(modes) => Some(modes),
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error()) => None,
        Err(error) => {
            eprintln!("lune-mux: terminal {terminal}'s modes could not be read: {error}");
            None
        },
    }
}

fn looks_gone(Gone: Gone) -> io::Error {
    io::Error::other("its looks are gone")
}

fn host() -> String {
    rustix::system::uname().nodename().to_string_lossy().into_owned()
}

fn local_cwd(said: &str, host: &str) -> Option<String> {
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
    Some(path.into_owned()).filter(|path| Path::new(path).is_absolute())
}

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
    use std::num::NonZeroU16;

    use super::*;

    #[test]
    fn a_signal_key_no_group_takes_is_typed_behind_what_waits() {
        let columns = NonZeroU16::new(80).unwrap();
        let rows = NonZeroU16::new(24).unwrap();
        let pty = Pty::opened(Size { columns, rows }).unwrap();
        assert!(pty.signal_key(0x03).unwrap().is_some());
        let mut unread = Unread::default();
        unread.type_in(b"waits").unwrap();
        type_into(TerminalId::next(&mut None), &pty, &mut unread, b"\x03").unwrap();
        assert_eq!(unread.line_up(), Some(&b"waits\x03"[..]));
    }

    #[test]
    fn what_the_program_writes_brings_a_later_look_forward() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut looks = Looks::first();
        looks.heard(start);
        assert_eq!(looks.waits_until(), Some(start));
        looks.asked();
        looks.answered(start, true);
        assert_eq!(looks.waits_until(), Some(at(100)));
        looks.asked();
        looks.answered(at(100), true);
        assert_eq!(looks.waits_until(), Some(at(1100)));
        looks.heard(at(300));
        assert_eq!(looks.waits_until(), Some(at(300)));
        looks.asked();
        looks.answered(at(300), true);
        looks.heard(at(310));
        assert_eq!(looks.waits_until(), Some(at(400)));
        looks.asked();
        looks.answered(at(400), false);
        assert_eq!(looks.waits_until(), Some(at(500)));
        looks.asked();
        looks.answered(at(500), false);
        assert_eq!(looks.waits_until(), None);
    }

    #[test]
    fn a_look_asked_holds_the_next_and_waits_after_it_finishes() {
        let start = Instant::now();
        let at = |millis| start + Duration::from_millis(millis);
        let mut looks = Looks::first();
        looks.asked();
        looks.heard(at(50));
        assert_eq!(looks.waits_until(), None);
        looks.answered(at(250), true);
        assert_eq!(looks.waits_until(), Some(at(350)));
        looks.looked_locally(at(350), false);
        assert_eq!(looks.waits_until(), None);
    }

    #[test]
    #[should_panic(expected = "a seen look was asked")]
    fn answering_an_idle_look_panics() {
        let mut looks = Looks::first();
        looks.answered(Instant::now(), false);
    }

    #[test]
    fn a_cwd_is_one_this_machine_has() {
        let cwd = |said: &str| local_cwd(said, "here");
        assert_eq!(cwd("file://here/home/a%20b"), Some("/home/a b".to_owned()));
        assert_eq!(cwd("file://HERE/tmp"), Some("/tmp".to_owned()));
        assert_eq!(cwd("file:///tmp"), Some("/tmp".to_owned()));
        assert_eq!(cwd("file://localhost/tmp"), Some("/tmp".to_owned()));
        assert_eq!(cwd("file://there/tmp"), None);
        assert_eq!(cwd("/tmp"), None);
        assert_eq!(cwd("file://here"), None);
        assert_eq!(cwd("file://here/%FE"), None);
        assert_eq!(cwd("file://here/a%00b"), None);
    }
}
