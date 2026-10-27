//! luneta's own terminal: the sidebar on the left, zellij on the right, and
//! which of the two has the keys. `Ctrl b` hands them from one to the other,
//! and a click hands them to the one clicked. `Ctrl q` hides the sidebar,
//! zellij taking its columns, and brings it back, and `Alt Tab` takes zellij
//! to the next pane the sidebar lists, whichever has the keys.

use std::collections::VecDeque;
use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, ExitStatus};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::CursorShape;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::SetTitle;
use lunetad::api::Host;
use ratatui::layout::{Position, Rect};
use ratatui::DefaultTerminal;

use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange, Theme};
use crate::close::Act;
use crate::folds::{self, Folds};
use crate::hosted::{Hosted, Said, Size};
use crate::input;
use crate::keys;
use crate::news::{self, News};
use crate::own::{Look, Looked};
use crate::sidebar::{self, Asked, Place, Sidebar};
use crate::skills::{self, Skill};
use crate::zellij::{self, After, Closes, Refuge, SessionName};

/// What luneta's loop acts on, from the threads that wait on each source.
pub enum Event {
    /// A key, the mouse, a paste or a resize of luneta's terminal.
    Input(Input),
    /// An answer of luneta's terminal, or what it says of itself unasked,
    /// which comes in among the input whenever it comes.
    Answered(Answer),
    /// luneta's terminal cannot be read any more.
    InputEnded(io::Error),
    Hosted(Said),
    /// The host as the daemon says it stands, or why the daemon does not say.
    Daemon(Result<Host, String>),
    /// What zellij answered a look at the clients of a session.
    Looked(Looked),
    /// zellij did not take its client to the session luneta sent it to, on
    /// the go whose look, by its number, was to find it there.
    Stayed(u64),
    /// Something went wrong that the sidebar says.
    Notice(String),
    /// What zellij opened for the menu numbered `menu`: the terminal, or why
    /// none.
    Opened { menu: u64, opened: Result<u32, String> },
    /// What closing the pane of the menu numbered `menu` closes, as zellij
    /// lists its session, or why it does not say.
    Closes { menu: u64, closes: Result<Closes, String> },
    /// The skills the directory of the menu numbered `menu` can run, or why
    /// the finder found none.
    Skills { menu: u64, skills: Result<Vec<Skill>, String> },
    /// A fold was made or taken away where the folds are kept: by another
    /// luneta, or by this one.
    Folds,
    /// Something was kept of what you saw or marked unread: by another
    /// luneta, or by this one.
    News,
}

/// How often the sidebar is drawn again with nothing else to draw it: every
/// second for the ages, and at each turn of the spinner while one turns.
const TICK: Duration = Duration::from_secs(1);

/// The most events that go into one frame. Those that come faster wait for the
/// next, so that luneta draws at least once each this many events, however
/// fast they come.
const FRAME_EVENTS_MAX: usize = 256;

/// How long luneta holds the input for zellij at the most, for a zellij that
/// shows nothing luneta can tell from the blank screen it starts on, as a
/// pane with nothing in it and no frame or bar about it.
const WAITING_MAX: Duration = Duration::from_secs(2);

/// How often luneta looks at zellij's screen while it holds the input for
/// zellij: what zellij draws as luneta draws brings no `Said::Drew` of its
/// own.
const WAITING_LOOK: Duration = Duration::from_millis(50);

/// The key that hands the keys from zellij to the sidebar, and back.
const SWAP: (KeyCode, KeyModifiers) = (KeyCode::Char('b'), KeyModifiers::CONTROL);

/// The key that hides the sidebar, and brings it back.
const HIDE: (KeyCode, KeyModifiers) = (KeyCode::Char('q'), KeyModifiers::CONTROL);

/// The key that takes zellij to the pane the sidebar lists after the one it
/// is on.
const NEXT: (KeyCode, KeyModifiers) = (KeyCode::Tab, KeyModifiers::ALT);

/// Which side has the keys, and whether the sidebar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    /// zellij, beside the sidebar.
    Zellij,
    Sidebar,
    /// zellij, with the sidebar's columns too: the sidebar is hidden, and
    /// takes the keys only once it shows again.
    Hidden,
}

impl Focus {
    /// Whether zellij has the keys.
    fn zellij(self) -> bool {
        self != Focus::Sidebar
    }
}

/// The input luneta holds for zellij until zellij can take it.
struct Waiting {
    input: Vec<Input>,
    /// When zellij is handed it all the same.
    until: Instant,
}

/// zellij's client, which luneta runs beside the sidebar, and what luneta
/// knows of it. One that takes the place of a client that ended knows only
/// what the first did as it started.
struct Client {
    hosted: Hosted,
    /// How it ended, once its terminal says.
    exited: Option<ExitStatus>,
    /// Whether a button went down over zellij and is still held: what it
    /// drags goes to zellij, wherever the mouse goes.
    dragging: bool,
    /// The input that came before zellij was in its session, as keys typed as
    /// it starts: zellij 0.45 drops keys that reach it before it puts its
    /// client on its screen. None once zellij is in, which it says as it first
    /// draws for its client, as it draws only for one on its screen: once it
    /// titles its terminal, or once anything shows there. zellij titles a tab
    /// once for all its clients, so that a client that comes to a session
    /// already titled gets none.
    waiting: Option<Waiting>,
}

impl Client {
    /// Starts `program`, zellij's client, in a terminal of `size`, in `dir`,
    /// with the input held for it until zellij can take it.
    fn start(
        program: String,
        args: Vec<String>,
        dir: &Path,
        size: Size,
        events: Sender<Event>,
    ) -> io::Result<Self> {
        let hosted = Hosted::start(program, args, dir, size, events)?;
        let waiting = Some(Waiting { input: Vec::new(), until: Instant::now() + WAITING_MAX });
        Ok(Client { hosted, exited: None, dragging: false, waiting })
    }
}

/// luneta's queue of events, with what came on it as luneta waited for its
/// terminal's answers put aside, to come first.
pub struct Inbox {
    events: Receiver<Event>,
    aside: VecDeque<Event>,
}

impl Inbox {
    pub fn new(events: Receiver<Event>) -> Self {
        Inbox { events, aside: VecDeque::new() }
    }

    /// The next answer of luneta's terminal by `deadline`, with what comes
    /// before it put aside. None once `deadline` passes, or once the terminal
    /// cannot be read.
    pub fn answer(&mut self, deadline: Instant) -> Option<Answer> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left).ok()? {
                Event::Answered(answer) => return Some(answer),
                ended @ Event::InputEnded(_) => {
                    self.aside.push_back(ended);
                    return None;
                },
                event => self.aside.push_back(event),
            }
        }
    }

    /// The next event, those put aside first, waiting `wait` at the most.
    fn next(&mut self, wait: Duration) -> Result<Event, RecvTimeoutError> {
        match self.aside.pop_front() {
            Some(event) => Ok(event),
            None => self.events.recv_timeout(wait),
        }
    }

    /// The next event, where one has come.
    pub fn try_next(&mut self) -> Option<Event> {
        self.aside.pop_front().or_else(|| self.events.try_recv().ok())
    }
}

struct App {
    client: Client,
    sidebar: Sidebar,
    /// Where the sidebar and zellij were last drawn, which the mouse goes by.
    areas: (Rect, Rect),
    focus: Focus,
    /// Whether the window luneta's terminal is in has the focus. The sidebar
    /// sees you in a pane only while it does.
    focused: bool,
    /// The cursor as luneta's terminal was last told to draw it.
    cursor: Option<(CursorShape, bool)>,
    /// luneta's terminal's colours as it said them last, which zellij's
    /// terminal answers with.
    colours: Colours,
    /// What luneta's terminal turned to, until the colours it turned to are
    /// in, when zellij is told.
    turned: Option<Theme>,
    events: Sender<Event>,
    /// What watches the folders the folds and the news are kept in, for as
    /// long as luneta runs.
    _watching: Vec<notify::RecommendedWatcher>,
    /// The directory luneta started in, which zellij's client runs in.
    dir: PathBuf,
    ended: bool,
}

/// Runs zellij, attached to `session`, beside the sidebar, until zellij ends:
/// its client detaches or fails, or its session ends and no other runs. Where
/// another does, a client of the newest takes the place of the one that
/// ended. The sidebar is of what the daemon in `folder` says, which is
/// started if none runs.
pub fn run(session: Option<SessionName>, folder: Result<PathBuf, String>) -> ExitCode {
    let dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("luneta: no directory to start in: {error}");
            return ExitCode::FAILURE;
        },
    };
    let session = session.or_else(|| zellij::session_for(&dir));
    if let Some(refusal) = session.as_ref().and_then(zellij::refusal) {
        eprintln!("luneta: {refusal}");
        return ExitCode::FAILURE;
    }
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
            DisableFocusChange,
            DisableThemeChange
        );
        restore(panic);
    }));
    // Not a `sync_channel` of a bound: alacritty hands on what the program
    // does with the screen locked, which the loop locks to draw, so that a
    // full queue would hold each up for the other for good. What fills it are
    // the keys and the mouse, the daemon's answers, zellij's actions and what
    // zellij does with its terminal, of which `hosted` keeps one draw at most.
    let (events, inbox) = mpsc::channel();
    let mut inbox = Inbox::new(inbox);
    input::read(events.clone());
    // The kitty keyboard protocol, where luneta's terminal speaks it, tells
    // apart keys that zellij's client can then tell apart too, as `Shift
    // Enter` from `Enter`. Its colours are what zellij asks its own terminal
    // for, as a program in a pane asks zellij.
    let answers = answers::ask(&mut inbox);
    let ran = execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableThemeChange
    )
    .and_then(|()| {
        if answers.kitty {
            let disambiguate = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
            execute!(io::stdout(), PushKeyboardEnhancementFlags(disambiguate))?;
        }
        run_in(&mut terminal, session.as_ref(), folder, &dir, answers.colours, events, &mut inbox)
    });
    // Each mode is put back however the rest went, and the first error is the
    // one said.
    let popped =
        if answers.kitty { execute!(io::stdout(), PopKeyboardEnhancementFlags) } else { Ok(()) };
    let restored = execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableThemeChange,
        SetCursorStyle::DefaultUserShape
    );
    let left = ratatui::try_restore();
    match ran.and_then(|failed| popped.and(restored).and(left).map(|()| failed)) {
        Ok(None) => ExitCode::SUCCESS,
        // What zellij said as it failed went with luneta's screen.
        Ok(Some(failed)) => {
            eprintln!("{failed}");
            ExitCode::FAILURE
        },
        Err(error) => {
            eprintln!("luneta: {error}");
            ExitCode::FAILURE
        },
    }
}

/// Runs zellij beside the sidebar until it ends, and says what it said as it
/// failed, if it did. zellij's terminal answers it with `colours`, until
/// luneta's terminal says others.
fn run_in(
    terminal: &mut DefaultTerminal,
    session: Option<&SessionName>,
    folder: Result<PathBuf, String>,
    dir: &Path,
    colours: Colours,
    events: Sender<Event>,
    inbox: &mut Inbox,
) -> io::Result<Option<String>> {
    let areas = areas(Rect::from((Position::ORIGIN, terminal.size()?)), false);
    // The clients of the session before luneta's comes, of which none is
    // luneta's: none of a session zellij is to make.
    let before = session.map(|session| zellij::clients(&session.to_string()).unwrap_or_default());
    let (program, args) = zellij::client(session);
    let client = Client::start(program, args, dir, window(areas.1), events.clone())?;
    let folds = Folds::shared();
    // Kept beside the daemon, which lists the sessions it is kept of.
    let news = match &folder {
        Ok(runtime) => News::within(runtime),
        Err(why) => News::unshared(why),
    };
    let folds_watched = folds.dir().map(|dir| folds::watch(dir, events.clone())).transpose();
    let news_watched = news::watch(&news, events.clone());
    let mut sidebar = Sidebar::new(folds, news);
    let mut watching = Vec::new();
    match folds_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => sidebar
            .notify(format!("folds made elsewhere show from luneta's next start: {error}")),
    }
    match news_watched {
        Ok(watcher) => watching.push(watcher),
        Err(error) => sidebar
            .notify(format!("dots of other lunetas show from luneta's next start: {error}")),
    }
    if let (Some(session), Some(before)) = (session, before)
        && let Some(look) = sidebar.sending(&session.to_string())
    {
        zellij::arrive(look, before, &events);
    }
    match folder {
        Ok(folder) => crate::watch::start(folder, events.clone()),
        Err(why) => sidebar.lost(why),
    }
    let mut app = App {
        client,
        sidebar,
        areas,
        focus: Focus::Zellij,
        // Until the terminal says otherwise. One that never tells a program
        // of its window's focus has you in it all along.
        focused: true,
        cursor: None,
        colours,
        turned: None,
        events,
        _watching: watching,
        dir: dir.to_owned(),
        ended: false,
    };
    run_loop(terminal, &mut app, inbox)?;
    Ok(app.failure())
}

/// Draws a frame for each batch of events, until zellij ends with no client
/// to take the place of its own, or luneta's terminal can no longer be read.
fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    inbox: &mut Inbox,
) -> io::Result<()> {
    loop {
        let wait = if app.client.waiting.is_some() {
            WAITING_LOOK
        } else if app.sidebar.turning() {
            sidebar::SPIN
        } else {
            TICK
        };
        match inbox.next(wait) {
            Ok(event) => app.handle(event, terminal)?,
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the app holds a sender, for zellij's actions")
            },
        }
        // What came while luneta drew goes into one frame, with the event it
        // waited for: `FRAME_EVENTS_MAX` in all.
        for _ in 1..FRAME_EVENTS_MAX {
            let Some(event) = inbox.try_next() else {
                break;
            };
            app.handle(event, terminal)?;
        }
        if app.ended {
            return Ok(());
        }
        app.wait_on(Instant::now());
        app.sidebar.ticked(SystemTime::now(), Instant::now(), app.focused);
        app.draw(terminal)?;
    }
}

/// The columns the sidebar takes of the terminal, and those left for zellij:
/// the sidebar's own, or half the terminal's, where that is narrower, and
/// none while it is `hidden`.
fn areas(frame: Rect, hidden: bool) -> (Rect, Rect) {
    let sidebar = if hidden { 0 } else { sidebar::COLUMNS.min(frame.width / 2) };
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
            Event::Input(input) => match &mut self.client.waiting {
                Some(waiting) => waiting.input.push(input),
                None => self.input(input),
            },
            Event::Answered(answer) => self.answered(answer, terminal)?,
            Event::InputEnded(error) => return Err(error),
            Event::Hosted(said) => self.hosted_said(said, terminal)?,
            Event::Daemon(Ok(host)) => {
                let look = self.sidebar.heard(host);
                self.look(look);
            },
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Looked(looked) => self.sidebar.looked(looked),
            Event::Stayed(look) => {
                let look = self.sidebar.stayed(look);
                self.look(look);
            },
            Event::Notice(notice) => self.sidebar.notify(notice),
            Event::Folds => self.sidebar.refold(),
            Event::Opened { menu, opened } => {
                let asked = self.sidebar.opened(menu, opened);
                self.follow(asked);
            },
            Event::Closes { menu, closes } => {
                let asked = self.sidebar.closes(menu, closes);
                self.follow(asked);
            },
            Event::Skills { menu, skills } => self.sidebar.found(menu, skills),
            Event::News => self.sidebar.reread(),
        }
        Ok(())
    }

    /// Hands zellij the input held for it once anything shows on its screen,
    /// or once it waited long enough.
    fn wait_on(&mut self, now: Instant) {
        let client = &self.client;
        let ready = |waiting: &Waiting| now >= waiting.until || !client.hosted.blank();
        if client.waiting.as_ref().is_some_and(ready) {
            self.waited();
        }
    }

    /// Hands zellij the input held for it, if any is. A client that took the
    /// place of one that ended heard nothing of where the keys are, and has
    /// them as far as it knows: it hears first where it does not, as the
    /// sidebar has them, or another window.
    fn waited(&mut self) {
        let Some(waiting) = self.client.waiting.take() else {
            return;
        };
        if !(self.focused && self.focus.zellij())
            && let Some(bytes) = keys::focus(false, self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
        for input in waiting.input {
            self.input(input);
        }
    }

    fn input(&mut self, input: Input) {
        match input {
            Input::Key(key) => self.key(&key),
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Paste(text) => {
                if self.focus.zellij() {
                    self.client.hosted.send(keys::paste(&text, self.client.hosted.mode()));
                }
            },
            Input::FocusGained => self.focus_window(true),
            Input::FocusLost => self.focus_window(false),
            // The next frame is of the new size, and zellij's terminal with it.
            Input::Resize(..) => {},
        }
    }

    fn key(&mut self, key: &KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        let chord = (key.code, key.modifiers);
        if chord == SWAP {
            // A hidden sidebar comes back to take the keys.
            let other = if self.focus.zellij() { Focus::Sidebar } else { Focus::Zellij };
            self.give(other);
            return;
        }
        if chord == HIDE {
            let other = if self.focus == Focus::Hidden { Focus::Zellij } else { Focus::Hidden };
            self.give(other);
            return;
        }
        if chord == NEXT {
            self.next();
            return;
        }
        if self.focus.zellij() {
            if let Some(bytes) = keys::key(key, self.client.hosted.mode()) {
                self.client.hosted.send(bytes);
            }
        } else {
            let asked = self.sidebar.key(key, SystemTime::now());
            self.follow(asked);
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let (sidebar, zellij) = self.areas;
        let over_zellij = mouse.column >= zellij.x;
        if self.client.dragging || over_zellij {
            match mouse.kind {
                MouseEventKind::Down(_) => {
                    self.client.dragging = true;
                    self.leave();
                },
                MouseEventKind::Up(_) => self.client.dragging = false,
                _ => {},
            }
            let column = mouse.column.saturating_sub(zellij.x).min(zellij.width.saturating_sub(1));
            let row = mouse.row.saturating_sub(zellij.y);
            if let Some(bytes) = keys::mouse(mouse, column, row, self.client.hosted.mode()) {
                self.client.hosted.send(bytes);
            }
            return;
        }
        let asked = self.sidebar.mouse(mouse, sidebar);
        if asked == Asked::Nothing && matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.give(Focus::Sidebar);
        }
        self.follow(asked);
    }

    /// Does what the sidebar asked. What the menu opens is where you go, with
    /// the keys; a delete leaves you in the sidebar, to go on along its rows.
    fn follow(&mut self, asked: Asked) {
        match asked {
            Asked::Nothing => {},
            Asked::Leave => self.leave(),
            Asked::Go(place) => {
                self.go(&place, None);
                self.leave();
            },
            Asked::Closes { menu, session, pane } => {
                zellij::closes(menu, &session, pane, &self.events);
            },
            Asked::Delete(act) => match act {
                Act::Pane { session, pane } => zellij::close_pane(&session, pane, &self.events),
                Act::Tab { session, tab, .. } => zellij::close_tab(&session, tab, &self.events),
                Act::Kill(session) => zellij::kill(&session, &self.events),
                // The plan found zellij's client in `session`, and on its
                // way nowhere, as the key was pressed.
                Act::Leave { session, to } => {
                    self.go_from(&session, &to, Some(After::Kill(session.clone())));
                },
            },
            Asked::Open { menu, open } => zellij::open(menu, open, &self.events),
            Asked::OpenThere { menu, place, open } => {
                if !self.go(&place, Some(After::Open(menu, open))) {
                    self.sidebar.close_menu();
                }
            },
            Asked::Start { session, cwd } => {
                self.start(&session, &cwd);
                self.leave();
            },
            Asked::Find { menu, cwd } => skills::find(menu, &cwd, &self.events),
        }
    }

    /// Takes zellij's client to the pane the sidebar lists after the one it
    /// is on. The sidebar gives the keys up first, which closes a search: the
    /// pane after is the one after in every row, not in the rows a search
    /// leaves, which pass over panes and open folded sessions.
    fn next(&mut self) {
        self.leave();
        if let Some(place) = self.sidebar.next_pane() {
            self.go(&place, None);
        }
    }

    /// Takes zellij's client to `place`, and does `after` once it is there,
    /// in another session. Whether it went.
    fn go(&mut self, place: &Place, after: Option<After>) -> bool {
        let Some(here) = self.here() else {
            return false;
        };
        self.go_from(&here, place, after);
        true
    }

    /// Takes zellij's client from `here`, the session it is in, to `place`,
    /// and does `after` once it is there, in another session.
    fn go_from(&mut self, here: &str, place: &Place, after: Option<After>) {
        let then = self.sidebar.went(place);
        self.client.hosted.send(zellij::NUDGE);
        zellij::go(here, place.session(), place.pane(), then, after, &self.events);
    }

    /// Takes zellij's client to a session it makes, `session`, that starts in
    /// `cwd`.
    fn start(&mut self, session: &SessionName, cwd: &str) {
        let Some(here) = self.here() else {
            return;
        };
        let then = self.sidebar.sending(session.as_str());
        self.client.hosted.send(zellij::NUDGE);
        zellij::start(&here, session, cwd, then, &self.events);
    }

    /// The session zellij's client is in, for an action to take it from, or
    /// none while it is on its way to another, which the sidebar then says.
    fn here(&mut self) -> Option<String> {
        let Some(here) = self.sidebar.here().map(str::to_owned) else {
            self.sidebar.notify("zellij has yet to say which session it is in".to_owned());
            return None;
        };
        // An action would move the client that last typed in a session that
        // luneta's has yet to come to.
        if let Some(going) = self.sidebar.going() {
            self.sidebar.notify(format!("zellij has yet to take its client to {going}"));
            return None;
        }
        Some(here)
    }

    /// Asks zellij for the clients of a session, if the sidebar wants to know.
    fn look(&self, look: Option<Look>) {
        if let Some(look) = look {
            zellij::look(look, &self.events);
        }
    }

    /// Hands the keys to `side`, and tells zellij when it takes them or loses
    /// them, as a terminal tells a program in it of the window's focus, and
    /// the sidebar when it loses them.
    fn give(&mut self, side: Focus) {
        let had = self.focus.zellij();
        self.focus = side;
        self.sidebar.focus(side == Focus::Sidebar);
        if self.focused
            && side.zellij() != had
            && let Some(bytes) = keys::focus(side.zellij(), self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
    }

    /// Hands zellij the keys, where the sidebar has them.
    fn leave(&mut self) {
        if self.focus == Focus::Sidebar {
            self.give(Focus::Zellij);
        }
    }

    fn focus_window(&mut self, focused: bool) {
        self.focused = focused;
        if self.focus.zellij()
            && let Some(bytes) = keys::focus(focused, self.client.hosted.mode())
        {
            self.client.hosted.send(bytes);
        }
    }

    fn hosted_said(&mut self, said: Said, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match said {
            // The frame this event goes into draws it.
            Said::Drew => {},
            Said::Titled(title) => {
                execute!(terminal.backend_mut(), SetTitle(&title))?;
                if !title.is_empty() {
                    let look = self.sidebar.titled(title);
                    self.look(look);
                    self.waited();
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
            Said::Answer(answer) => self.client.hosted.send(answer.into_bytes()),
            Said::AskedSize(answer) => {
                self.client.hosted.send(answer(self.client.hosted.size().into()).into_bytes());
            },
            // A colour luneta's terminal did not say, or one of the rest, as
            // the sixteen and the cursor's, goes without an answer.
            Said::AskedColour(index, spell) => {
                if let Some(answer) = self.colours.answer(index, &*spell) {
                    self.client.hosted.send(answer.into_bytes());
                }
            },
            Said::Rang => {
                let backend = terminal.backend_mut();
                backend.write_all(b"\x07")?;
                backend.flush()?;
            },
            Said::Exited(status) => self.client.exited = Some(status),
            Said::Ended => self.client_ended()?,
        }
        Ok(())
    }

    /// zellij's client ended. Where it ended well, and its session with it, as
    /// zellij quits, the session's last pane exits, or the session is killed,
    /// a client of the newest session that answers takes its place, and
    /// luneta goes on. luneta ends where none does, and where the client
    /// detached, or failed, as where zellij cannot attach.
    fn client_ended(&mut self) -> io::Result<()> {
        let left = self.client.exited.filter(ExitStatus::success).and(self.sidebar.here());
        let Some(Refuge { session: to, before }) = left.and_then(zellij::refuge) else {
            self.ended = true;
            return Ok(());
        };
        let (program, args) = zellij::rejoin(&to);
        let size = window(self.areas.1);
        self.client = Client::start(program, args, &self.dir, size, self.events.clone())?;
        if let Some(look) = self.sidebar.sending(&to) {
            zellij::arrive(look, before, &self.events);
        }
        Ok(())
    }

    /// Takes in what luneta's terminal answered, or said of itself unasked.
    fn answered(&mut self, answer: Answer, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match answer {
            Answer::Foreground(_) | Answer::Background(_) => self.colours.hear(answer),
            // It turned dark or light: its colours are asked for again, and
            // zellij is told once they are in, so that a program zellij tells
            // that asks for them hears the new ones.
            Answer::Turned(theme) => {
                self.turned = Some(theme);
                let backend = terminal.backend_mut();
                backend.write_all(answers::TURNED)?;
                backend.flush()?;
            },
            // What it is, the last of what it was asked: the colours it turned
            // to, if it turned, are in.
            Answer::Device => {
                if let Some(theme) = self.turned.take() {
                    self.client.hosted.send(theme.report());
                }
            },
            // Too late to count, as luneta started.
            Answer::Kitty => {},
        }
        Ok(())
    }

    /// What zellij said as it failed, to be said again once luneta's screen is
    /// gone: the text on its terminal, and how it ended. None for a zellij that
    /// detached or quit, or that ended in a way its terminal could not tell.
    fn failure(&self) -> Option<String> {
        let status = self.client.exited.filter(|status| !status.success())?;
        let ended = format!("luneta: zellij ended ({status})");
        let said = self.client.hosted.text();
        Some(if said.is_empty() { ended } else { format!("{said}\n{ended}") })
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let now = SystemTime::now();
        terminal.draw(|frame| {
            let hidden = self.focus == Focus::Hidden;
            let (sidebar, zellij) = areas(frame.area(), hidden);
            self.areas = (sidebar, zellij);
            let size = self.client.hosted.size();
            if (zellij.width, zellij.height) != (size.columns.get(), size.lines.get()) {
                self.client.hosted.resize(window(zellij));
            }
            let buffer = frame.buffer_mut();
            if !hidden {
                self.sidebar.draw(buffer, sidebar, now);
            }
            let cursor = self.client.hosted.draw(buffer, zellij);
            if self.focus.zellij()
                && let Some(cursor) = cursor
            {
                frame.set_cursor_position(cursor);
            }
        })?;
        let cursor = self.client.hosted.cursor_style();
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
