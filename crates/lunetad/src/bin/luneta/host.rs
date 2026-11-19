use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::{self, Write as _};
use std::iter;
use std::num::NonZeroU16;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::term::ClipboardType;
use alacritty_terminal::vte::ansi::CursorShape;
use base64::Engine as _;
use crossterm::cursor::SetCursorStyle;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event as Input, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, KeyboardEnhancementFlags, MouseButton, MouseEvent, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate, SetTitle};
use crossterm::{execute, queue};
use lunetad::api::{Host, Version};
use lunetad::issues::{Filed, OnGitHub, Remote, Unfiled};
use lunetad::log::{self, Process};
use lunetad::mux::own::viewer::{Greeting, Seen};
use lunetad::mux::own::{OwnClientId, OwnKey, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{Name, ProjectId, ViewId, WorktreeId};
use lunetad::prunes;
use lunetad::links::Unready;
use lunetad::{Destination, Direction, Growth};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::{DefaultTerminal, Terminal};

use crate::actions::{
    Chord, AGENTS, COLLAPSE, DIRS, DOWN, GROW, LEFT, MENU, RIGHT, WORKTREES, SHRINK, SWAP, UP,
};
use crate::alerts::{self, Alerts};
use crate::aloud::{self, Aloud};
use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange};
use crate::ask::{self, Ask, Keyed};
use crate::attached;
use crate::agents::{self, Kept};
use crate::brief::{self, Asking, Briefing, Written};
use crate::browser::Browser;
use crate::checks;
use crate::close::Act;
use crate::desktop::{self, Desktop, Reader, Told};
use crate::dictation::{self, Dictated, Dictation};
use crate::folds::{self, Folds};
use crate::images::{self, Went};
use crate::input;
use crate::issue::{self, Issue};
use crate::machines::{self, Added, Machine};
use crate::menu::{self, Choice, Rename};
use crate::models::Fetched;
use crate::named;
use crate::news::{self, News};
use crate::prune;
use crate::peers;
use crate::picker::{self, List, Picked, Picker};
use crate::printed::{self, Printed};
use crate::pull::{self, Found, Link};
use crate::sidebar::{self, Asked, HereAgent, Keys, Listed, PaneKey, Place, Reorder, Sidebar};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Speech, Spoken};
use crate::spots::{Burst, Drawn, Hit, Settling};
use crate::projected::{self, Projected, Route};
use crate::viewed::{self, Heeded, Viewed};
use crate::worktrees::{self, Landed, Landing, Unmoved};
use crate::width::Width;
use crate::zellij::{self, Close, Closes, Open, SessionName};

pub enum Made {
    Worktree { on: Machine, made: Result<Landing, String>, argv: Vec<String> },
    Pulled { menu: u64, made: Result<Pulled, String> },
}

pub struct Pulled {
    on: Machine,
    worktree: Landed,
    argv: Vec<String>,
}

#[derive(Debug)]
enum Doomed {
    Worktree { worktree: WorktreeId, seen: Vec<ViewId> },
    Project { project: ProjectId, worktrees: Vec<WorktreeId>, seen: Vec<ViewId> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Naming {
    Idle,
    Busy,
}

#[derive(Clone, Copy, Debug)]
enum Opener {
    Holds,
    LetGo,
}

impl Opener {
    fn of(asking: sidebar::Held) -> Opener {
        match asking {
            sidebar::Held::Open => Opener::Holds,
            sidebar::Held::Kept => Opener::LetGo,
        }
    }
}

const NAMING: &str = "menu: luneta names a session already: try again once it has made it";

const NAMING_PANICKED: &str = "menu: luneta could not name the session: naming it panicked";

pub const NOT_YET: &str = "luneta does not do that to a worktree yet";

const WELCOME_MENU: &str = "the menu is for terminals: d d closes this welcome view";

const UNSHOWN: [&str; 2] =
    ["No worktree to show yet.", "o w on a project's row opens one."];

const UNSHOWN_CURSOR: (CursorShape, bool) = (CursorShape::HollowBlock, false);

const CLIENT: u64 = 1;

const ASKED_TO_QUIT: &str = "asked to quit";

const SLOW_END: Duration = Duration::from_secs(1);

const WAITS: &str = "ends once the changes it sent are done; Ctrl c ends it now";

pub enum Event {
    Input(Input),
    Answered(Answer),
    InputEnded(io::Error),
    Shown(u64, lunetad::Heard),
    Seen { number: u64, watcher: u64, seen: Seen },
    Reached { claude_folder: Option<PathBuf> },
    Daemon(Result<Host, String>),
    Notice(String),
    Moved { ticket: u64, kept: Option<Version> },
    Unmoved { on: Machine, ticket: u64, order: Reorder, unmoved: Unmoved },
    Copy(Link),
    CopyText(String),
    Image(Went),
    ImageSlow(Destination),
    Opened { menu: u64, opened: Result<u32, String> },
    Named(Made),
    Closes { menu: u64, closes: Result<Closes, String> },
    Skills { menu: u64, agent: agents::Harness, skills: Result<Vec<Skill>, String> },
    Pulled { menu: u64, found: Result<Found, String> },
    Prunes { menu: u64, plan: Result<prunes::Plan, String> },
    Pruned { menu: u64, pruned: Result<prunes::Pruned, String> },
    Printed { menu: u64, printed: Result<Printed, String> },
    Checks { menu: u64, checks: Result<checks::Failing, String> },
    Briefed { number: u64, written: Written },
    Heard { number: u64, heard: ask::Heard },
    Remotes { number: u64, remotes: Result<Vec<Remote>, String> },
    Filed {
        number: u64,
        repository: issue::Repository,
        to: OnGitHub,
        words: String,
        filed: Result<Filed, Unfiled>,
    },
    Picker { number: u64, heard: picker::Heard },
    Spoken { number: u64, spoken: Spoken },
    Nameable { number: u64, skills: Result<Vec<Skill>, String> },
    Fetched(Fetched),
    Voiced { number: u64, ended: Result<(), String> },
    VoiceFetched(Fetched),
    ReadAloud(String),
    StopAloud,
    Folds,
    News,
    Agent,
    Alerted(alerts::Read),
    Clicked(alerts::Key),
    #[cfg(not(target_os = "macos"))]
    IssueClicked(desktop::Came),
    Peers(peers::Heard),
}

const TICK: Duration = Duration::from_secs(1);

const FRAME_EVENTS_MAX: usize = 256;

const GROWS: [Chord; 4] = [
    GROW,
    (KeyCode::Char('+'), KeyModifiers::ALT),
    (KeyCode::Char('='), KeyModifiers::ALT.union(KeyModifiers::SHIFT)),
    (KeyCode::Char('+'), KeyModifiers::ALT.union(KeyModifiers::SHIFT)),
];

fn stepped(chord: Chord) -> Option<Direction> {
    match chord {
        LEFT => Some(Direction::Left),
        DOWN => Some(Direction::Down),
        UP => Some(Direction::Up),
        RIGHT => Some(Direction::Right),
        _ => None,
    }
}

fn resized(chord: Chord) -> Option<Growth> {
    if GROWS.contains(&chord) {
        return Some(Growth::Grow);
    }
    (chord == SHRINK).then_some(Growth::Shrink)
}

#[derive(Debug)]
struct Focus {
    side: Side,
    over: Option<Over>,
}

impl Focus {
    fn on(side: Side) -> Focus {
        Focus { side, over: None }
    }

    fn session(&self) -> bool {
        match (self.side, &self.over) {
            (Side::Session | Side::Collapsed, None) => true,
            (Side::Sidebar, None) | (_, Some(_)) => false,
        }
    }

    fn session_told(&self) -> bool {
        match (self.side, &self.over) {
            (
                Side::Session | Side::Collapsed,
                None | Some(Over::Dictation(_, Where::Session(_))),
            ) => true,
            (Side::Sidebar, _) | (_, Some(_)) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Session,
    Sidebar,
    Collapsed,
}

impl Side {
    fn session(self) -> Side {
        match self {
            Side::Collapsed => Side::Collapsed,
            Side::Session | Side::Sidebar => Side::Session,
        }
    }
}

#[derive(Debug)]
enum Over {
    Menu,
    Note(String),
    Brief(Briefing),
    Ask(Ask),
    Issue(Issue),
    Picker(Box<Picker>),
    Dictation(Dictation, Where),
}

struct Fetch {
    _asking: Asking,
    got: u64,
}

#[derive(Debug)]
enum Where {
    Session(Option<HereAgent>),
    Ask(Ask),
    Issue(Issue),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LastPress {
    Session,
    Elsewhere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    Nothing,
    Session,
    Selecting,
    Edge { dragged: bool },
    Taken,
}

pub struct Inbox {
    events: Receiver<Event>,
    aside: VecDeque<Event>,
}

impl Inbox {
    pub fn new(events: Receiver<Event>) -> Self {
        Inbox { events, aside: VecDeque::new() }
    }

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

    fn next(&mut self, wait: Duration) -> Result<Event, RecvTimeoutError> {
        match self.aside.pop_front() {
            Some(event) => Ok(event),
            None => self.events.recv_timeout(wait),
        }
    }

    pub fn try_next(&mut self) -> Option<Event> {
        self.aside.pop_front().or_else(|| self.events.try_recv().ok())
    }
}

struct App {
    right: Option<Viewed>,
    shown_number_dropped: u64,
    runs_in: Option<TerminalKey>,
    projected: Projected,
    seated: Option<OwnPane>,
    lunetad: PathBuf,
    movers: HashMap<Machine, Mover>,
    sidebar: Sidebar,
    areas: (Rect, Rect),
    width: Width,
    held: Held,
    last_press: LastPress,
    focus: Focus,
    focused: bool,
    burst: Option<Burst>,
    settling: Settling,
    drawn: Drawn,
    pointer: Option<Position>,
    cursor: Option<(CursorShape, bool)>,
    title: Option<String>,
    colours: Colours,
    briefs: u64,
    asks: u64,
    issues: u64,
    unfiled: issue::Kept,
    pickers: u64,
    inputs: u64,
    dictations: u64,
    image: images::Going,
    repeats: dictation::Repeats,
    aloud_repeats: dictation::Repeats,
    aloud: Aloud,
    copied: Option<String>,
    browser: Browser,
    speech: Option<Speech>,
    fetching: Option<Fetch>,
    releases: bool,
    claude_folder: Option<PathBuf>,
    daemon: Option<PathBuf>,
    events: Sender<Event>,
    _watching: Vec<notify::RecommendedWatcher>,
    dir: PathBuf,
    gave_up: Option<String>,
    changes: Vec<JoinHandle<()>>,
    naming: Naming,
    alerts: Alerts,
    desktop: Desktop,
    quit_asked: bool,
}

pub fn run(session: Option<&SessionName>, folder: Result<PathBuf, String>) -> ExitCode {
    let unkept = log::start(Process::Luneta).err();
    let dir = match env::current_dir() {
        Ok(dir) => dir,
        Err(error) => return did_not_start(&format!("no directory to start in: {error}")),
    };
    let news = match &folder {
        Ok(runtime) => News::within(runtime),
        Err(why) => News::unshared(why),
    };
    let lunetad = match &folder {
        Ok(lunetad) => lunetad.clone(),
        Err(why) => return did_not_start(why),
    };
    let cwd = match worktrees::real(&dir) {
        Ok(cwd) => cwd,
        Err(why) => return did_not_start(&format!("no directory to start in: {why}")),
    };
    let named = session.map(SessionName::as_str);
    let seen = |project| {
        news.last_seen(crate::ledger::Session::Project(project))
    };
    let start = match worktrees::start(&lunetad, named, &cwd, seen) {
        Ok(start) => start,
        Err(refusal) => return did_not_start(&format!("no worktree to show: {refusal}")),
    };
    let runs_in = lunetad::mux::own::terminal_here();
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => return did_not_start(&format!("no terminal: {error}")),
    };
    let hidden = terminal.hide_cursor();
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        log::panicked(panic);
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = put_back(&mut io::stdout());
        restore(panic);
    }));
    let (events, inbox) = mpsc::channel();
    let mut inbox = Inbox::new(inbox);
    input::read(events.clone());
    let answers = answers::ask(&mut inbox);
    let mut sent = Vec::new();
    let ran = execute!(
        io::stdout(),
        EnableMouseCapture,
        EnableBracketedPaste,
        EnableFocusChange,
        EnableThemeChange
    )
    .and_then(|()| {
        if answers.kitty {
            let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES;
            execute!(io::stdout(), PushKeyboardEnhancementFlags(flags))?;
        }
        run_in(
            &mut terminal,
            (start, runs_in),
            news,
            folder,
            unkept,
            &dir,
            answers.colours,
            answers.kitty,
            events,
            &mut inbox,
            &mut sent,
        )
    });
    if let Err(error) = &ran {
        say_ends(&error.to_string());
    }
    let popped =
        if answers.kitty { execute!(io::stdout(), PopKeyboardEnhancementFlags) } else { Ok(()) };
    let restored = put_back(&mut io::stdout());
    let left = ratatui::try_restore();
    let put = popped.and(restored).and(left);
    if let Err(error) = &put {
        log::say(&format!("its terminal was not put back: {error}"));
    }
    let mut said = io::stderr();
    wait_for(sent, SLOW_END, || {
        log::say(WAITS);
        let _ = writeln!(said, "luneta: {WAITS}");
    });
    for event in iter::from_fn(|| inbox.try_next()) {
        if let Event::Filed { to, words, filed, .. } = event {
            let _ = writeln!(said, "{}", issue::on_quit(&to, &words, &filed));
        }
    }
    match hidden.and(ran).and_then(|failed| put.map(|()| failed)) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(failed)) => {
            let _ = writeln!(said, "{failed}");
            ExitCode::FAILURE
        },
        Err(error) => {
            let _ = writeln!(said, "luneta: {error}");
            ExitCode::FAILURE
        },
    }
}

fn did_not_start(why: &str) -> ExitCode {
    say_ends(why);
    eprintln!("luneta: {why}");
    ExitCode::FAILURE
}

fn say_ends(why: &str) {
    log::say(&format!("ends: {why}"));
}

fn wait_for(sent: Vec<JoinHandle<()>>, slow: Duration, said: impl FnOnce()) {
    let (ended, ending) = mpsc::channel();
    thread::spawn(move || {
        for change in sent {
            if change.join().is_err() {
                log::say("a change luneta sent panicked");
            }
        }
        let _ = ended.send(());
    });
    if ending.recv_timeout(slow).is_err() {
        said();
        let _ = ending.recv();
    }
}

fn put_back(terminal: &mut impl io::Write) -> io::Result<()> {
    execute!(
        terminal,
        EndSynchronizedUpdate,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableThemeChange,
        SetCursorStyle::DefaultUserShape
    )
}

fn run_in(
    terminal: &mut DefaultTerminal,
    (start, runs_in): (Option<Landing>, Option<TerminalKey>),
    news: News,
    folder: Result<PathBuf, String>,
    unkept: Option<String>,
    dir: &Path,
    colours: Colours,
    releases: bool,
    events: Sender<Event>,
    inbox: &mut Inbox,
    sent: &mut Vec<JoinHandle<()>>,
) -> io::Result<Option<String>> {
    let (width, width_unread) = Width::kept();
    let folds = Folds::shared();
    let folds_dirs = folds.dirs();
    let folds_watched =
        (!folds_dirs.is_empty()).then(|| folds::watch(&folds_dirs, events.clone())).transpose();
    let news_watched = news::watch(&news, events.clone());
    let (agent, agent_unread) = Kept::shared();
    let agent_watched = agent.file().map(|file| agents::watch(file, events.clone())).transpose();
    let mut sidebar = Sidebar::new(folds, news, agent, machines::local_name());
    if let Some(why) = unkept {
        sidebar.notify(format!("what goes wrong is kept in no log: {why}"));
    }
    let mut watching = Vec::new();
    match folds_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => {
            sidebar.notify(format!("folds made elsewhere show from luneta's next start: {error}"));
        },
    }
    match news_watched {
        Ok(watcher) => watching.push(watcher),
        Err(error) => {
            sidebar.notify(format!("dots of other lunetas show from luneta's next start: {error}"));
        },
    }
    match agent_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => {
            let why = format!("a harness picked elsewhere holds from luneta's next start: {error}");
            sidebar.notify(why);
        },
    }
    if let Some(why) = agent_unread {
        sidebar.notify(format!("the menu starts agents in claude: {why}"));
    }
    if let Some(why) = width_unread {
        sidebar.notify(format!("the sidebar opens {} wide: {why}", sidebar::COLUMNS));
    }
    let daemon = match folder {
        Ok(folder) => Some(folder),
        Err(why) => {
            sidebar.lost(why);
            None
        },
    };
    let areas = areas(Rect::from((Position::ORIGIN, terminal.size()?)), width.columns());
    let lunetad = daemon.clone().unwrap_or_default();
    let (size, greeting, here) = (layout_size(areas.1), greeting(colours), Machine::Local);
    let (start, root) = match start {
        Some(Landing::Worktree(start)) => (Some(start.worktree()), None),
        Some(Landing::Unopened(root)) => (None, Some(root)),
        None => (None, None),
    };
    let right = start.map(|start| {
        Viewed::start(1, lunetad.clone(), here, start, size, greeting, runs_in, &events)
    });
    let keys = if right.is_some() { Side::Session } else { Side::Sidebar };
    let desktop = Desktop::start(&events);
    let focused = true;
    let mut app = App {
        right,
        shown_number_dropped: 0,
        runs_in,
        projected: Projected::default(),
        seated: None,
        lunetad,
        movers: HashMap::new(),
        sidebar,
        areas,
        width,
        held: Held::Nothing,
        last_press: LastPress::Elsewhere,
        focus: Focus::on(keys),
        focused,
        burst: None,
        settling: Settling::default(),
        drawn: Drawn::Nothing,
        pointer: None,
        cursor: None,
        title: None,
        colours,
        briefs: 0,
        asks: 0,
        issues: 0,
        unfiled: issue::Kept::default(),
        pickers: 0,
        inputs: 0,
        dictations: 0,
        image: images::Going::default(),
        repeats: dictation::Repeats::of(dictation::KEY),
        aloud_repeats: dictation::Repeats::of(aloud::KEY),
        aloud: Aloud::new(events.clone()),
        copied: None,
        browser: Browser::default(),
        speech: None,
        fetching: None,
        releases,
        claude_folder: None,
        daemon,
        events,
        _watching: watching,
        dir: dir.to_owned(),
        gave_up: None,
        changes: Vec::new(),
        naming: Naming::Idle,
        alerts: Alerts::new(focused),
        desktop,
        quit_asked: false,
    };
    app.watch();
    if let Some(root) = root {
        app.open_worktree(Machine::Local, root);
    }
    let ran = run_loop(terminal, &mut app, inbox);
    sent.extend(app.sent());
    ran?;
    Ok(app.failure())
}

fn greeting(colours: Colours) -> Greeting {
    let (foreground, background) = colours.both();
    Greeting { foreground, background }
}

fn layout_size(area: Rect) -> lunetad::projects::Size {
    let size = window(area);
    viewed::layout_size(size.columns, size.lines)
}

fn caught<T>(named: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(named)).unwrap_or_else(|_| Err(NAMING_PANICKED.to_owned()))
}

struct Move {
    ticket: u64,
    order: Reorder,
}

const MOVE_PANICKED: &str = "luneta could not ask lunetad for the move: asking panicked";

struct Mover {
    moves: SyncSender<Move>,
    moving: JoinHandle<()>,
}

impl Mover {
    fn on(lunetad: &Path, on: &Machine, events: &Sender<Event>) -> Mover {
        let (moves, asked) = mpsc::sync_channel::<Move>(sidebar::MOVES_MAX);
        let (lunetad, on, events) = (lunetad.to_owned(), on.clone(), events.clone());
        let moving = thread::spawn(move || {
            let mut stopped = Stopped::default();
            for Move { ticket, order } in asked {
                if stopped.holds(ticket) {
                    continue;
                }
                let asked = AssertUnwindSafe(|| worktrees::reordered(&lunetad, &on, &order));
                let unsure = |_| Err(Unmoved::Unsure(MOVE_PANICKED.to_owned()));
                let event = match panic::catch_unwind(asked).unwrap_or_else(unsure) {
                    Ok(kept) => Event::Moved { ticket, kept },
                    Err(unmoved) => {
                        stopped.stop(ticket);
                        Event::Unmoved { on: on.clone(), ticket, order, unmoved }
                    },
                };
                if events.send(event).is_err() {
                    return;
                }
            }
        });
        Mover { moves, moving }
    }

    fn stopped(self) -> JoinHandle<()> {
        drop(self.moves);
        self.moving
    }
}

#[derive(Default)]
struct Stopped {
    tickets: VecDeque<u64>,
}

impl Stopped {
    fn holds(&self, ticket: u64) -> bool {
        self.tickets.contains(&ticket)
    }

    fn stop(&mut self, ticket: u64) {
        assert!(!self.holds(ticket), "a ticket stopped sends no more moves");
        if self.tickets.len() == sidebar::MOVES_MAX {
            self.tickets.pop_front();
        }
        self.tickets.push_back(ticket);
        assert!(self.tickets.len() <= sidebar::MOVES_MAX);
    }
}

fn run_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    inbox: &mut Inbox,
) -> io::Result<()> {
    loop {
        let wait = if app.sidebar.scrolling() {
            sidebar::SCROLL
        } else if app.turning() {
            sidebar::SPIN
        } else {
            TICK
        };
        let due = app.due().map(|due| due.saturating_duration_since(Instant::now()));
        let wait = due.map_or(wait, |due| wait.min(due));
        match inbox.next(wait) {
            Ok(event) => app.handle(event, terminal)?,
            Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the app holds a sender, for what it starts")
            },
        }
        for _ in 1..FRAME_EVENTS_MAX {
            if app.quit_asked {
                break;
            }
            let Some(event) = inbox.try_next() else {
                break;
            };
            app.handle(event, terminal)?;
        }
        if app.quit_asked {
            say_ends(ASKED_TO_QUIT);
            return Ok(());
        }
        app.ticked(Instant::now());
        app.alerting(Instant::now());
        app.sidebar.ticked(SystemTime::now(), Instant::now(), app.focused);
        app.draw(terminal)?;
    }
}

fn areas(frame: Rect, own: u16) -> (Rect, Rect) {
    let sidebar = own.min(frame.width / 2);
    let left = Rect::new(frame.x, frame.y, sidebar, frame.height);
    let right = Rect::new(frame.x + sidebar, frame.y, frame.width - sidebar, frame.height);
    (left, right)
}

struct Size {
    columns: NonZeroU16,
    lines: NonZeroU16,
}

fn window(area: Rect) -> Size {
    let cells = |count: u16| NonZeroU16::new(count).unwrap_or(NonZeroU16::MIN);
    Size { columns: cells(area.width), lines: cells(area.height) }
}

impl App {
    fn handle(&mut self, event: Event, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let relists = matches!(
            event,
            Event::Daemon(_) | Event::Shown(_, lunetad::Heard::Laid(_))
        );
        match event {
            Event::Input(input) => self.input(input),
            Event::Answered(answer) => self.answered(answer, terminal)?,
            Event::InputEnded(error) => return Err(error),
            Event::Shown(number, heard) => {
                if let Some(right) = self.right.as_mut().filter(|it| it.number() == number) {
                    let heeded = right.heard(heard);
                    self.heeded(heeded);
                }
            },
            Event::Seen { number, watcher, seen } => {
                if let Some(right) = self.right.as_mut().filter(|it| it.number() == number) {
                    let heeded = right.seen(watcher, seen);
                    self.heeded(heeded);
                }
            },
            Event::Reached { claude_folder } => self.claude_folder = claude_folder,
            Event::Daemon(Ok(mut host)) => {
                self.projected = Projected::adapted(&mut host).unwrap_or_else(|why| {
                    self.sidebar.notify(format!("lunetad's projects: {why}"));
                    Projected::default()
                });
                if let Some(right) = &mut self.right {
                    let (tabs, titles) = self.projected.tabs(right.machine(), right.worktree());
                    right.tabs(tabs, titles);
                }
                self.sidebar.heard(host);
                self.seat();
                if let Some(machines) = self.sidebar.reaches() {
                    let acts = self.alerts.heard(machines, Instant::now());
                    self.alert(acts);
                }
                if let Some(sayings) = self.sidebar.sayings() {
                    self.aloud.hear(sayings, self.desktop.reads_aloud());
                }
            },
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Notice(notice) => self.sidebar.notify(notice),
            Event::Moved { ticket, kept } => self.sidebar.moved(ticket, kept),
            Event::Unmoved { on, ticket, order, unmoved } => {
                let note = match unmoved {
                    Unmoved::Refused(why) => format!("not moved: {why}"),
                    Unmoved::Unsure(why) => format!("may have moved: {why}"),
                };
                self.sidebar.unmoved(&on, ticket, &order, note);
            },
            Event::Copy(link) => clip(terminal, ClipboardType::Clipboard, &link.url())?,
            Event::CopyText(text) => {
                clip(terminal, ClipboardType::Clipboard, &text)?;
                self.copied = Some(text);
            },
            Event::Image(went) => self.image_went(went),
            Event::ImageSlow(on) => self.sidebar.image_going(Some(on)),
            Event::Folds => self.sidebar.refold(),
            Event::Alerted(read) => {
                if self.alerts.read(&read) {
                    self.desktop.post(read.alert);
                }
            },
            Event::Clicked(key) => self.clicked(&key),
            #[cfg(not(target_os = "macos"))]
            Event::IssueClicked(desktop::Came::Filed(link)) => {
                if let Err(why) = self.browser.open(&link, &self.events, Instant::now()) {
                    self.sidebar.notify(why);
                }
            },
            #[cfg(not(target_os = "macos"))]
            Event::IssueClicked(desktop::Came::Unfiled) => {
                desktop::raise(self.title.as_deref());
            },
            Event::Peers(heard) => {
                if self.desktop.heard(heard, self.focused) {
                    let acts = self.alerts.again();
                    self.alert(acts);
                }
                self.focus_alerts();
                self.sidebar.unsay(peers::NO_READER);
                self.hold_aloud();
            },
            Event::Opened { menu, opened } => {
                let asked = self.sidebar.opened(menu, opened.map(PaneKey::Zellij));
                self.follow(asked);
            },
            Event::Closes { menu, closes } => {
                let asked = self.sidebar.closes(menu, closes);
                self.follow(asked);
            },
            Event::Skills { menu, agent, skills } => self.sidebar.found(menu, agent, skills),
            Event::Named(made) => self.named(made),
            Event::Pulled { menu, found } => {
                let asked = self.sidebar.pulled(menu, found);
                self.follow(asked);
            },
            Event::Printed { menu, printed } => {
                let asked = self.sidebar.printed(menu, printed);
                self.follow(asked);
            },
            Event::Checks { menu, checks } => {
                let asked = self.sidebar.checks(menu, checks);
                self.follow(asked);
            },
            Event::Prunes { menu, plan } => {
                let asked = self.sidebar.prunes(menu, plan);
                self.follow(asked);
            },
            Event::Pruned { menu, pruned } => {
                let asked = self.sidebar.pruned(menu, pruned);
                self.follow(asked);
            },
            Event::Briefed { number, written } => {
                if let Some(Over::Brief(briefing)) = &mut self.focus.over
                    && briefing.number() == number
                {
                    briefing.written(written);
                }
            },
            Event::Heard { number, heard } => {
                if let Some(Over::Ask(ask)) = &mut self.focus.over
                    && ask.number() == number
                {
                    ask.heard(heard);
                }
            },
            Event::Remotes { number, remotes } => {
                if let Some(Over::Issue(issue) | Over::Dictation(_, Where::Issue(issue))) =
                    &mut self.focus.over
                    && issue.number() == number
                {
                    issue.listed(remotes);
                }
            },
            Event::Filed { number, repository, to, words, filed } => {
                self.sidebar.filed(number, &to.to_string(), &filed);
                self.desktop.tell(Told::issue(&to.to_string(), &filed));
                match filed {
                    Ok(filed) => {
                        let url = filed.link().url();
                        clip(terminal, ClipboardType::Clipboard, &url)?;
                        self.copied = Some(url);
                    },
                    Err(_) => self.unfiled.keep(number, repository, to, words),
                }
            },
            Event::Picker { number, heard } => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.heard(heard);
                    self.picked(did);
                }
            },
            Event::Spoken { number, spoken } => {
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                    && let Some(dictated) = dictation.spoken(spoken, Instant::now())
                {
                    self.undictate(Some(dictated));
                }
            },
            Event::Nameable { number, skills } => {
                if let Err(why) = &skills {
                    self.sidebar.notify(why.clone());
                }
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                    && let Some(dictated) = dictation.found(skills)
                {
                    self.undictate(Some(dictated));
                }
            },
            Event::Fetched(fetched) => self.fetched(&fetched),
            Event::Voiced { number, ended } => {
                if let Some(why) = self.aloud.ended(number, ended) {
                    self.sidebar.notify(why);
                }
            },
            Event::VoiceFetched(fetched) => {
                if let Some(why) = self.aloud.fetch_went(fetched) {
                    self.sidebar.notify(why);
                }
            },
            Event::ReadAloud(selected) => {
                let read = self.aloud.key(Some(&selected));
                self.read_aloud_went(read);
            },
            Event::StopAloud => self.aloud.press(aloud::Button::Stop),
            Event::News => self.sidebar.reread(),
            Event::Agent => self.sidebar.reagent(),
        }
        if relists {
            self.relist();
        }
        self.settle();
        Ok(())
    }

    fn relist(&mut self) {
        if !matches!(self.focus.over, Some(Over::Picker(_))) {
            return;
        }
        let listing = self.sidebar.listing();
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.listed(listing);
        }
    }

    fn turning(&self) -> bool {
        let over = match &self.focus.over {
            Some(Over::Brief(briefing)) => briefing.turning(),
            Some(Over::Ask(ask)) => ask.turning(),
            Some(Over::Dictation(dictation, under)) => {
                dictation.turning() || matches!(under, Where::Issue(issue) if issue.turning())
            },
            Some(Over::Issue(issue)) => issue.turning(),
            Some(Over::Menu | Over::Picker(_) | Over::Note(_)) | None => false,
        };
        over || self.sidebar.turning()
    }

    fn due(&self) -> Option<Instant> {
        let picker = match &self.focus.over {
            Some(Over::Picker(picker)) => picker.due(),
            _ => None,
        };
        let shows = self.sidebar.due(SystemTime::now()).map(|left| Instant::now() + left);
        picker.into_iter().chain(self.alerts.due()).chain(shows).min()
    }

    fn alerting(&mut self, now: Instant) {
        let here = self.sidebar.whereabouts();
        let mine = if self.focused { self.alerts.at(here.as_ref()) } else { Vec::new() };
        self.desktop.looking(&mine);
        let mut looked = self.desktop.looks();
        looked.extend(mine);
        let mut acts = self.alerts.looked(&looked);
        acts.extend(self.alerts.ticked(now));
        self.alert(acts);
    }

    fn focus_alerts(&mut self) {
        let acts = self.alerts.focus(self.focused || self.desktop.another_focused());
        self.alert(acts);
    }

    fn alert(&mut self, acts: Vec<alerts::Act>) {
        for act in acts {
            match act {
                alerts::Act::Read(due) => {
                    alerts::read(due, self.claude_folder.clone(), &self.events);
                },
                alerts::Act::Close(key) => self.desktop.close(key),
            }
        }
    }

    fn clicked(&mut self, key: &alerts::Key) {
        let Some(place) = self.alerts.clicked(key) else {
            return;
        };
        self.desktop.close(key.clone());
        if self.desktop.hand(key) {
            return;
        }
        desktop::raise(self.title.as_deref());
        self.go(&key.on, &Place::Pane(place));
        self.leave();
    }

    fn ticked(&mut self, now: Instant) {
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.ticked(now);
        }
        self.ask_picker();
        if let Some(right) = &mut self.right {
            right.ticked();
        }
    }

    fn ask_picker(&mut self) {
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            let number = picker.number();
            for ask in picker.asks() {
                picker::ask(number, ask, &self.projected, &self.events);
            }
        }
    }

    fn settle(&mut self) {
        self.ask_picker();
        if matches!(self.focus.over, Some(Over::Menu)) && self.sidebar.floating().is_none() {
            let over = self.sidebar.said().map(Over::Note);
            self.give(Focus { side: self.focus.side, over });
        }
    }

    fn watch(&mut self) {
        if let Some(folder) = self.daemon.take() {
            crate::watch::start(folder, self.events.clone());
        }
    }

    fn input(&mut self, input: Input) {
        let heard = match &input {
            Input::Key(key) => key.kind != KeyEventKind::Release,
            Input::Mouse(mouse) => mouse.kind != MouseEventKind::Moved,
            Input::Paste(_) | Input::FocusGained | Input::FocusLost => true,
            Input::Resize(..) => false,
        };
        if heard {
            self.inputs += 1;
        }
        match input {
            Input::Key(key) => self.key(&key),
            Input::Mouse(mouse) => self.mouse(mouse),
            Input::Paste(text) => match &mut self.focus.over {
                Some(Over::Ask(ask)) => ask.paste(&text),
                Some(Over::Issue(issue)) => issue.paste(&text),
                Some(Over::Picker(picker)) => picker.paste(&text),
                Some(Over::Menu) => self.sidebar.paste(&text),
                Some(Over::Brief(_) | Over::Note(_) | Over::Dictation(..)) => {},
                None if self.focus.side == Side::Sidebar => self.sidebar.paste(&text),
                None => self.paste(&text),
            },
            Input::FocusGained => self.focus_window(true),
            Input::FocusLost => self.focus_window(false),
            Input::Resize(..) => {},
        }
        self.settle();
    }

    fn key(&mut self, key: &KeyEvent) {
        let now = Instant::now();
        let key = self.repeats.read(*key, now);
        let key = &self.aloud_repeats.read(key, now);
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            match dictation.key(key, now) {
                dictation::Keyed::Nothing => {},
                dictation::Keyed::Close => self.undictate(None),
                dictation::Keyed::Dictate => {
                    self.undictate(None);
                    self.dictate();
                },
            }
            return;
        }
        let chord = (key.code, key.modifiers);
        if key.kind == KeyEventKind::Release
            || (key.kind == KeyEventKind::Repeat
                && (chord == dictation::KEY || chord == aloud::KEY))
        {
            return;
        }
        if chord == dictation::KEY && self.dictate() {
            return;
        }
        if chord == aloud::KEY {
            self.read_aloud();
            return;
        }
        if aloud::stops(chord) {
            self.stop_aloud();
            return;
        }
        match &mut self.focus.over {
            Some(Over::Ask(ask)) => {
                match ask.key(key) {
                    Keyed::Nothing => {},
                    Keyed::Send => self.converse(),
                    Keyed::Close => self.give(Focus::on(self.focus.side)),
                }
                return;
            },
            Some(Over::Issue(issue)) => {
                match issue.key(key) {
                    issue::Keyed::Nothing => {},
                    issue::Keyed::File => self.file_issue(),
                    issue::Keyed::Close => self.close_issue(),
                }
                return;
            },
            Some(Over::Picker(picker)) => {
                let did = picker.key(key);
                self.picked(did);
                return;
            },
            Some(Over::Brief(briefing)) => {
                if briefing.closes(key) {
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            Some(Over::Menu) => {
                if chord == MENU {
                    self.sidebar.close_menu();
                } else {
                    let asked = self.sidebar.key(key, SystemTime::now());
                    self.follow(asked);
                }
                return;
            },
            Some(Over::Note(_)) => {
                if key.kind == KeyEventKind::Repeat {
                    return;
                }
                self.give(Focus::on(self.focus.side));
                if chord != MENU {
                    return;
                }
            },
            Some(Over::Dictation(..)) => return,
            None => {},
        }
        if chord == MENU {
            let over = if self.right.as_ref().is_some_and(Viewed::welcomes) {
                Over::Note(WELCOME_MENU.to_owned())
            } else {
                match self.sidebar.float_menu() {
                    Ok(()) => Over::Menu,
                    Err(why) => Over::Note(why.to_owned()),
                }
            };
            self.give(Focus { side: self.focus.side, over: Some(over) });
            return;
        }
        let list = match chord {
            WORKTREES => Some(List::Worktrees),
            AGENTS => Some(List::Agents),
            DIRS => Some(List::Dirs),
            _ => None,
        };
        if let Some(list) = list {
            self.pickers += 1;
            let aim = match (list, self.focus.side) {
                (List::Dirs, Side::Sidebar) => self.sidebar.on_header(),
                _ => None,
            };
            let aim = aim.unwrap_or_default();
            let listing = self.sidebar.listing();
            let picker = Box::new(Picker::open(self.pickers, list, listing, aim));
            self.give(Focus { side: self.focus.side, over: Some(Over::Picker(picker)) });
            return;
        }
        if chord == SWAP {
            let other = if self.focus.session() { Side::Sidebar } else { Side::Session };
            self.give(Focus::on(other));
            return;
        }
        if chord == COLLAPSE {
            let collapsed = self.focus.side == Side::Collapsed;
            self.give(Focus::on(if collapsed { Side::Session } else { Side::Collapsed }));
            return;
        }
        if let Some(direction) = stepped(chord) {
            self.step(direction);
            return;
        }
        if let Some(change) = resized(chord) {
            self.leave();
            self.ask_right(lunetad::Asked::Resize(change));
            return;
        }
        if self.focus.session() && !self.sidebar.pressed() {
            if chord == images::KEY && self.key_for_image(key) {
                return;
            }
            self.key_to_session(key);
        } else {
            let asked = self.sidebar.key(key, SystemTime::now());
            self.follow(asked);
        }
    }

    fn key_to_session(&mut self, key: &KeyEvent) {
        if let Some(right) = &mut self.right {
            right.key(key, self.inputs);
        }
    }

    fn paste(&mut self, text: &str) {
        if let Some(right) = &mut self.right {
            right.paste(text);
        }
    }

    fn tell_focus(&mut self, has: bool) {
        if let Some(right) = &mut self.right {
            right.focus(has);
        }
    }

    fn ask_right(&self, asked: lunetad::Asked) {
        if let Some(right) = &self.right {
            right.ask(asked);
        }
    }

    fn key_for_image(&mut self, key: &KeyEvent) -> bool {
        let Some(on) = self.sidebar.far_agent() else {
            return false;
        };
        if desktop::elsewhere() {
            return false;
        }
        if self.image.send(on.clone(), *key, &self.events) {
            self.sidebar.clear_notice();
        }
        true
    }

    fn image_went(&mut self, went: Went) {
        self.image.heard();
        self.sidebar.image_going(None);
        match went {
            Went::NoImage { key, on } => {
                if self.focus.session() && self.sidebar.far_agent() == Some(&on) {
                    self.key_to_session(&key);
                }
            },
            Went::Kept { on, path } => {
                if matches!(self.sidebar.on(), Machine::Remote(there) if *there == on) {
                    self.paste(&path);
                } else {
                    self.image_not_pasted(&format!("zellij left {on}, where it is at {path}"));
                }
            },
            Went::Failed(why) => self.image_not_pasted(&why),
        }
    }

    fn image_not_pasted(&mut self, why: &str) {
        let note = format!("image not pasted: {why}");
        if self.focus.side == Side::Collapsed && self.focus.over.is_none() {
            log::say(&note);
            self.give(Focus { side: Side::Collapsed, over: Some(Over::Note(note)) });
        } else {
            self.sidebar.notify(note);
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        self.pointer = Some(at);
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        let under = matches!(self.held, Held::Session | Held::Selecting);
        if self.focus.over.is_some() && under && !down {
            if !self.select(mouse) {
                if let MouseEventKind::Up(_) = mouse.kind {
                    self.held = Held::Nothing;
                }
                self.session_mouse(mouse);
            }
            return;
        }
        if down {
            self.last_press = LastPress::Elsewhere;
            self.sidebar.unsweep();
            if self.burst.as_mut().is_some_and(|burst| burst.takes(at, Instant::now())) {
                self.held = Held::Taken;
                return;
            }
            self.burst = None;
            if self.settling.takes(at, Instant::now()) {
                self.held = Held::Taken;
                self.burst = Some(Burst::new(at, Instant::now()));
                return;
            }
        }
        let button = || self.sidebar.aloud_buttons().at(at).copied().map(Hit::Aloud);
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(hit) = self.drawn.at(at).or_else(button)
        {
            self.let_go();
            self.sidebar.let_go();
            self.held = Held::Taken;
            self.burst = Some(Burst::new(at, Instant::now()));
            self.hit(hit);
            return;
        }
        let wheel = match mouse.kind {
            MouseEventKind::ScrollDown => Some(1),
            MouseEventKind::ScrollUp => Some(-1),
            _ => None,
        };
        if let Some(by) = wheel
            && let Some(Hit::Menu(menu, Choice::Skill(_))) = self.drawn.at(at)
        {
            self.sidebar.scroll_menu(menu, by);
            return;
        }
        let off = !self.drawn.boxed().is_some_and(|area| area.contains(at));
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            match (mouse.kind, wheel) {
                (_, Some(by)) => picker.scroll(by),
                (MouseEventKind::Down(_), _) if off => {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                },
                _ => {},
            }
            return;
        }
        match &self.focus.over {
            Some(Over::Ask(_) | Over::Issue(_) | Over::Picker(_) | Over::Dictation(..)) => return,
            Some(Over::Brief(briefing)) => {
                if down && briefing.clicked() {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            Some(Over::Menu) => {
                if down && off {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_menu();
                }
                return;
            },
            Some(Over::Note(_)) => {
                if down {
                    self.let_go();
                    self.held = Held::Taken;
                    self.give(Focus::on(self.focus.side));
                }
                return;
            },
            None => {},
        }
        if self.held == Held::Taken {
            match mouse.kind {
                MouseEventKind::Drag(_) => return,
                MouseEventKind::Up(_) => {
                    self.held = Held::Nothing;
                    return;
                },
                _ => self.held = Held::Nothing,
            }
        }
        let (sidebar, right) = self.areas;
        if matches!(self.held, Held::Edge { .. }) && self.resize(mouse) {
            return;
        }
        let over_session = mouse.column >= right.x;
        let held = matches!(self.held, Held::Session | Held::Selecting);
        if held || (over_session && !self.sidebar.pressed()) {
            let of_a_press = matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_));
            if of_a_press && self.last_press != LastPress::Session {
                return;
            }
            if self.select(mouse) {
                return;
            }
            match mouse.kind {
                MouseEventKind::Down(_) => {
                    self.held = Held::Session;
                    self.last_press = LastPress::Session;
                    self.leave();
                },
                MouseEventKind::Up(_) => self.held = Held::Nothing,
                _ => {},
            }
            self.session_mouse(mouse);
            return;
        }
        if self.focus.side == Side::Collapsed {
            if down {
                self.give(Focus::on(Side::Session));
                self.burst = Some(Burst::new(at, Instant::now()));
            }
            return;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && sidebar::edge(sidebar) == Some(mouse.column)
        {
            self.held = Held::Edge { dragged: false };
            return;
        }
        let asked = self.sidebar.mouse(mouse, sidebar);
        self.follow(asked);
    }

    fn hit(&mut self, hit: Hit) {
        match hit {
            Hit::Keys(keyed, key) => {
                let asked = self.sidebar.keys_clicked(keyed, &key, SystemTime::now());
                self.follow(asked);
            },
            Hit::Menu(menu, choice) if matches!(self.focus.over, None | Some(Over::Menu)) => {
                let asked = self.sidebar.chose(menu, choice, SystemTime::now());
                self.follow(asked);
            },
            Hit::Picker(number, pick) => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.click(pick);
                    self.picked(did);
                }
            },
            Hit::Number(link) => {
                let asked = self.sidebar.number_clicked(link, SystemTime::now());
                self.follow(asked);
            },
            Hit::Menu(..) => {},
            Hit::Aloud(button) => self.aloud.press(button),
        }
    }

    fn session_mouse(&mut self, mouse: MouseEvent) {
        let at = self.session_cell(mouse);
        let width = self.areas.1.width;
        let Some(right) = &mut self.right else {
            return;
        };
        if let Some(worktree) = right.mouse(mouse, at, width) {
            let on = right.machine().clone();
            self.go_worktree(&on, worktree, None);
        }
    }

    fn session_cell(&self, mouse: MouseEvent) -> attached::Spot {
        let (_, right) = self.areas;
        let column = mouse.column.saturating_sub(right.x).min(right.width.saturating_sub(1));
        let row = mouse.row.saturating_sub(right.y).min(right.height.saturating_sub(1));
        attached::Spot { row, column }
    }

    fn select(&mut self, mouse: MouseEvent) -> bool {
        let at = self.session_cell(mouse);
        let Some(own) = &mut self.right else {
            return false;
        };
        match (mouse.kind, self.held) {
            (MouseEventKind::Down(_), _) if own.selects(mouse, at) => {
                own.select_from(at);
                self.held = Held::Selecting;
                self.last_press = LastPress::Session;
                self.leave();
                true
            },
            (MouseEventKind::Down(_), _) => {
                own.unselect();
                false
            },
            (MouseEventKind::Drag(_), Held::Selecting) => {
                own.select_to(at);
                true
            },
            (MouseEventKind::Up(_), Held::Selecting) => {
                self.held = Held::Nothing;
                if let Some(text) = own.selected() {
                    let _ = self.events.send(Event::CopyText(text));
                }
                true
            },
            (
                _,
                Held::Nothing | Held::Session | Held::Selecting | Held::Edge { .. } | Held::Taken,
            ) => false,
        }
    }

    fn resize(&mut self, mouse: MouseEvent) -> bool {
        let (sidebar, right) = self.areas;
        if let MouseEventKind::Drag(_) = mouse.kind {
            let terminal = sidebar.width + right.width;
            self.width.drag(mouse.column.saturating_sub(sidebar.x), terminal);
            self.held = Held::Edge { dragged: true };
            return true;
        }
        self.let_go();
        matches!(mouse.kind, MouseEventKind::Up(_))
    }

    fn let_go(&mut self) {
        let Held::Edge { dragged } = self.held else {
            return;
        };
        self.held = Held::Nothing;
        if dragged && let Err(why) = self.width.keep() {
            self.sidebar.notify(format!("the sidebar's width stays in this luneta: {why}"));
        }
    }

    fn follow(&mut self, asked: Asked) {
        match asked {
            Asked::Nothing => {},
            Asked::OpenWorktree { on, project } => self.open_worktree(on, project.id()),
            Asked::Take => self.give(Focus::on(Side::Sidebar)),
            Asked::Leave => self.leave(),
            Asked::Copy(link) => {
                let _ = self.events.send(Event::Copy(link));
            },
            Asked::CopyText(text) => {
                let _ = self.events.send(Event::CopyText(text));
            },
            Asked::Browse(link) => {
                if let Err(why) = self.browser.open(&link, &self.events, Instant::now()) {
                    self.sidebar.notify(why);
                }
            },
            Asked::Go { on, place } => {
                self.go(&on, &place);
                self.leave();
            },
            Asked::Closes { menu, on, session, pane } => {
                match projected::route(&on, &session, pane) {
                    Ok(Route::Zellij { on, session, pane }) => {
                        zellij::closes(menu, &on, session, pane, &self.events);
                    },
                    Ok(Route::Terminal(_)) => {
                        let asked = self.sidebar.closes(menu, Ok(Closes::Pane));
                        self.follow(asked);
                    },
                    Err(why) => {
                        let asked = self.sidebar.closes(menu, Err(why));
                        self.follow(asked);
                    },
                }
            },
            Asked::Delete { on, act } if act.own() => self.project_act(&on, &act),
            Asked::Delete { on, act } => match act {
                Act::Worktree { .. } | Act::Project { .. } => self.project_act(&on, &act),
                Act::Pane { session, whose, pane } => {
                    let close = Close::Pane(pane.id());
                    self.keep_change(zellij::close(&on, session, whose, close, &self.events));
                },
                Act::Tab { session, whose, tab, panes } => {
                    let close = Close::Tab { id: tab, panes };
                    self.keep_change(zellij::close(&on, session, whose, close, &self.events));
                },
                Act::Kill { session, whose } => {
                    self.keep_change(zellij::kill(&on, session, whose, &self.events));
                },
                Act::Leave { session, to, .. } => {
                    self.go(&on, &to);
                    zellij::stays(&session, &self.events);
                },
            },
            Asked::Open { asking, on, key, open } => self.project_open(asking, &on, key, &open),
            Asked::OpenZellij { menu, on, open } => {
                self.keep_change(zellij::open(menu, &on, open, &self.events));
            },
            Asked::Welcome { asking, on, session, pane } => {
                self.welcome(asking, &on, &session, pane);
            },
            Asked::NewWorktree { on, project, branch } => {
                self.make_worktree(on, move |lunetad, on| {
                    worktrees::branched(lunetad, on, project, &branch).map(Landing::Worktree)
                });
                self.leave();
            },
            Asked::AddProject { on, folder, name } => {
                if self.refuse_naming(None) {
                    return;
                }
                self.naming_on(move || {
                    let made = || worktrees::project_made(&on, &folder, name.as_ref());
                    let made = caught(made).map_err(|why| format!("menu: {why}"));
                    Made::Worktree { on, made, argv: Vec::new() }
                });
                self.leave();
            },
            Asked::Rename { on, rename } => {
                self.change(on, move |on| {
                    let renamed = match &rename {
                        Rename::Terminal(terminal, to) => {
                            worktrees::renamed_terminal(on, *terminal, to.as_ref())
                        },
                        Rename::Worktree(worktree, to) => {
                            worktrees::renamed_worktree(on, *worktree, to.as_ref())
                        },
                        Rename::Project(project, to) => {
                            worktrees::renamed_project(on, *project, to)
                        },
                    };
                    renamed.map_err(|why| format!("not renamed: {why}"))
                });
            },
            Asked::Make { menu, on, session, holder, cwd, name, command } => {
                if self.refuse_naming(Some(menu)) {
                    return;
                }
                let lunetad = self.lunetad.clone();
                self.naming_on(move || {
                    let made = caught(|| {
                        let (session, at) = (session.as_str(), (&holder, &cwd));
                        let first = command.clone();
                        let made = worktrees::pulled(&lunetad, &on, session, at, &name, first)?;
                        Ok(Pulled { on, worktree: made, argv: command })
                    });
                    Made::Pulled { menu, made }
                });
            },
            Asked::Pull { menu, on, work, wanted, worked, others } => {
                pull::prepare(menu, on, work, wanted, worked, others, &self.events);
            },
            Asked::Prunes { menu, on, dir } => prune::plan(menu, on, dir, &self.events),
            Asked::Prune { menu, on, dir, agreed } => {
                let here = on == Machine::Local;
                let deleting = prune::delete(menu, on, dir, agreed, &self.events);
                if here {
                    self.keep_change(deleting);
                }
            },
            Asked::Find { menu, on, cwd, agent } => {
                let said = move |skills| Event::Skills { menu, agent, skills };
                skills::find(&on, &cwd, agent, &self.events, said);
            },
            Asked::Printed { menu, on, session, pane, ended } => {
                let route = projected::route(&on, &session, pane);
                printed::read(menu, route, ended, &self.events);
            },
            Asked::Checks { menu, on, page } => checks::fetch(menu, &on, page, &self.events),
            Asked::Brief(brief) => {
                self.briefs += 1;
                let folder = self.claude_folder.clone();
                let briefing = brief::ask(self.briefs, brief, folder, &self.events);
                self.give(Focus { side: self.focus.side, over: Some(Over::Brief(briefing)) });
            },
            Asked::Ask(target) => {
                self.asks += 1;
                let ask = Ask::new(self.asks, &target, &self.dir);
                self.give(Focus { side: self.focus.side, over: Some(Over::Ask(ask)) });
            },
            Asked::Issue(repository) => {
                self.issues += 1;
                let issue = Issue::open(self.issues, repository, &self.unfiled, &self.events);
                self.give(Focus { side: self.focus.side, over: Some(Over::Issue(issue)) });
            },
            Asked::Ssh(destination) => match machines::add(&destination) {
                Ok(Added::Named) => {},
                Ok(Added::Already) => {
                    let named = format!("ssh: luneta's hosts file names {destination} already");
                    self.sidebar.notify(named);
                },
                Err(why) => self.sidebar.notify(format!("ssh: {why}")),
            },
            Asked::Forget(destination) => {
                if let Err(why) = machines::forget(&destination) {
                    self.sidebar.notify(format!("forget {destination}: {why}"));
                }
            },
            Asked::Move { on, order, ticket } => {
                let (lunetad, events) = (&self.lunetad, &self.events);
                let mover = self.movers.entry(on.clone());
                let mover = mover.or_insert_with(|| Mover::on(lunetad, &on, events));
                match mover.moves.try_send(Move { ticket, order }) {
                    Ok(()) => {},
                    Err(TrySendError::Full(Move { ticket, order })) => {
                        let why = format!("{} moves are on their way already", sidebar::MOVES_MAX);
                        self.sidebar.unsent(ticket, &order, &why);
                    },
                    Err(TrySendError::Disconnected(Move { ticket, order })) => {
                        if let Some(mover) = self.movers.remove(&on) {
                            self.keep_change(mover.stopped());
                        }
                        let why = "the moves have stopped: move it again";
                        self.sidebar.unsent(ticket, &order, why);
                    },
                }
            },
            Asked::Quit => {
                assert!(!self.quit_asked, "luneta follows nothing once asked to quit");
                self.quit_asked = true;
            },
        }
    }

    fn change(
        &mut self,
        on: Machine,
        change: impl FnOnce(&Machine) -> Result<(), String> + Send + 'static,
    ) {
        let events = self.events.clone();
        self.keep_change(thread::spawn(move || {
            if let Err(why) = caught(|| change(&on)) {
                let _ = events.send(Event::Notice(format!("menu: {why}")));
            }
        }));
    }

    fn keep_change(&mut self, change: JoinHandle<()>) {
        self.changes.retain(|change| !change.is_finished());
        self.changes.push(change);
    }

    fn sent(&mut self) -> Vec<JoinHandle<()>> {
        let movers = self.movers.drain().map(|(_, mover)| mover.stopped());
        movers.chain(self.changes.drain(..)).collect()
    }

    fn project_act(&mut self, on: &Machine, act: &Act) {
        match (on, act) {
            (_, Act::Worktree { worktree, seen, .. }) => {
                let (worktree, seen) = (*worktree, seen.clone());
                self.project_act_delete(on, Doomed::Worktree { worktree, seen });
            },
            (_, Act::Project { project, worktrees, seen, .. }) => {
                let (project, worktrees, seen) = (*project, worktrees.clone(), seen.clone());
                self.project_act_delete(on, Doomed::Project { project, worktrees, seen });
            },
            (Machine::Local, Act::Pane { pane: PaneKey::Terminal(terminal), .. }) => {
                let (terminal, events) = (*terminal, self.events.clone());
                self.keep_change(thread::spawn(move || {
                    if let Err(failure) = lunetad::mux::own::server::Terminals.closed(terminal) {
                        let _ = events.send(Event::Notice(format!("menu: not closed: {failure}")));
                    }
                }));
            },
            _ => self.sidebar.notify(format!("menu: {NOT_YET}")),
        }
    }

    fn project_act_delete(&mut self, on: &Machine, doomed: Doomed) {
        let (lunetad, events, on) = (self.lunetad.clone(), self.events.clone(), on.clone());
        self.keep_change(thread::spawn(move || {
            let closed = caught(|| match &doomed {
                Doomed::Worktree { worktree, seen } => {
                    worktrees::closed(&lunetad, &on, *worktree, seen)
                },
                Doomed::Project { project, worktrees, seen } => {
                    worktrees::closed_project(&lunetad, &on, *project, worktrees, seen)
                },
            });
            if let Err(why) = closed {
                let _ = events.send(Event::Notice(format!("menu: not closed: {why}")));
            }
        }));
    }

    fn welcome(&mut self, asking: sidebar::Held, on: &Machine, session: &Listed, pane: PaneKey) {
        let Some(key) = session.own_key() else {
            return self.sidebar.notify(format!("menu: {}", menu::OWN_ONLY));
        };
        let held = self.projected.project(on, key).and_then(|project| {
            let pane = OwnPane::new(project.key, pane.id())?;
            self.projected.of_pane(on, pane)
        });
        let Some((worktree, terminal)) = held else {
            return self.sidebar.notify(format!("menu: lunetad lists no view {}", pane.id()));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().welcome();
        self.give_opened(Opener::of(asking));
    }

    fn project_open(&mut self, asking: sidebar::Held, on: &Machine, key: OwnKey, open: &Open) {
        let Some(project) = self.projected.project(on, key) else {
            return self.sidebar.notify("menu: lunetad keeps no such project".to_owned());
        };
        match open {
            Open::Pane { beside, command, .. } => {
                let pane = OwnPane::new(project.key, *beside);
                let held = pane.and_then(|it| self.projected.of_pane(on, it));
                let Some((worktree, terminal)) = held else {
                    return self.sidebar.notify(format!("menu: lunetad lists no view {beside}"));
                };
                self.go_worktree(on, worktree, Some(terminal));
                self.shown().open_beside_focused(command.clone());
                self.give_opened(Opener::of(asking));
            },
            Open::Tab { tab, .. } => {
                let (lunetad, id, on) = (self.lunetad.clone(), project.id, on.clone());
                let (cwd, name, argv) = (tab.cwd.clone(), tab.name.clone(), tab.command.clone());
                if self.refuse_naming(None) {
                    return;
                }
                self.naming_on(move || {
                    let made = caught(|| {
                        let named = name.as_deref();
                        let folder = cwd.as_str();
                        let (at, first) = ((id, folder), argv.clone());
                        let added = worktrees::add_worktree(&lunetad, &on, at, named, first);
                        added.map(Landing::Worktree)
                    });
                    let made = made.map_err(|why| format!("menu: no worktree made: {why}"));
                    Made::Worktree { on, made, argv }
                });
            },
        }
    }

    fn open_worktree(&mut self, on: Machine, project: ProjectId) {
        self.pickers += 1;
        let listing = self.sidebar.listing();
        let picker = Box::new(Picker::opening(self.pickers, listing, on, project));
        self.give(Focus { side: self.focus.side, over: Some(Over::Picker(picker)) });
    }

    fn picked(&mut self, picked: Picked) {
        match picked {
            Picked::Nothing => {},
            Picked::Close => self.give(Focus::on(self.focus.side)),
            Picked::Go { on, place } => {
                self.go(&on, &place);
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Add { on, project, folder, name } => {
                let project = project.id();
                self.make_worktree(on, move |lunetad, on| {
                    let named = name.as_ref().map(Name::as_str);
                    let at = (project, folder.as_str());
                    let added = worktrees::add_worktree(lunetad, on, at, named, Vec::new());
                    added.map(Landing::Worktree).map_err(Unready::Failed)
                });
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Make { on, session, cwd } => {
                self.make_worktree(on, move |lunetad, on| {
                    worktrees::made_at(lunetad, on, &cwd, Some(session.as_str()))
                        .map_err(Unready::Failed)
                });
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Plan { on, session, deletes } => {
                let plan = self.sidebar.plan(&on, &session, deletes);
                if let Some(Over::Picker(picker)) = &mut self.focus.over {
                    let did = picker.planned(plan);
                    self.picked(did);
                }
            },
            Picked::Delete { on, act } if act.own() => {
                self.project_act(&on, &act);
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Delete { on, act: Act::Leave { session, to, .. } } => {
                self.go(&on, &to);
                zellij::stays(&session, &self.events);
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Delete { on, act } => self.follow(Asked::Delete { on, act }),
        }
    }

    fn converse(&mut self) {
        let Some(Over::Ask(ask)) = &self.focus.over else {
            return;
        };
        let user = self.sidebar.user(&ask.machine());
        if let Some(Over::Ask(ask)) = &mut self.focus.over {
            ask.send(user, self.sidebar.agent(), &self.events);
        }
    }

    fn file_issue(&mut self) {
        let had = self.focus.session_told();
        let open = self.focus.over.take_if(|over| matches!(over, Over::Issue(_)));
        let Some(Over::Issue(issue)) = open else {
            return;
        };
        if let Some((number, to, filing)) = issue.send(&self.events, &mut self.unfiled) {
            self.sidebar.filing(number, &to.to_string());
            self.keep_change(filing);
        }
        self.give_from(had, Focus::on(self.focus.side));
    }

    fn close_issue(&mut self) {
        let had = self.focus.session_told();
        let open = self.focus.over.take_if(|over| matches!(over, Over::Issue(_)));
        if let Some(Over::Issue(issue)) = open {
            issue.close(&mut self.unfiled);
        }
        self.give_from(had, Focus::on(self.focus.side));
    }

    fn dictate(&mut self) -> bool {
        let (on_session, had) = (self.focus.session(), self.focus.session_told());
        let under = match self.focus.over.take() {
            None if on_session => Where::Session(self.sidebar.here_agent()),
            Some(Over::Ask(ask)) if !ask.turning() => Where::Ask(ask),
            Some(Over::Issue(issue)) => Where::Issue(issue),
            other => {
                self.focus.over = other;
                return false;
            },
        };
        self.dictations += 1;
        let number = self.dictations;
        let dictation = if speech::fetched() {
            let skills = self.nameable(number, &under);
            let speech = self.speech.get_or_insert_with(Speech::start);
            let listening = speech.listen(number, &self.events);
            Dictation::hearing(number, listening, skills, self.releases, Instant::now())
        } else {
            let events = &self.events;
            let fetching = || Fetch { _asking: speech::fetch(events), got: 0 };
            Dictation::fetching(number, self.fetching.get_or_insert_with(fetching).got)
        };
        let over = Some(Over::Dictation(dictation, under));
        self.give_from(had, Focus { side: self.focus.side, over });
        true
    }

    fn nameable(&self, number: u64, under: &Where) -> Skills {
        let Where::Session(Some(agent)) = under else {
            return Skills::Found(Vec::new());
        };
        let said = move |skills| Event::Nameable { number, skills };
        skills::find(&agent.on, &agent.dir, agent.harness, &self.events, said);
        Skills::Finding
    }

    fn read_aloud(&mut self) {
        self.sidebar.clear_notice();
        match self.desktop.reader() {
            Reader::This => {
                let read = self.aloud.key(self.copied.as_deref());
                self.read_aloud_went(read);
            },
            Reader::Another(holder) => holder.selected(self.copied.as_deref().unwrap_or_default()),
            Reader::Nobody(why) => self.sidebar.notify(why.to_owned()),
        }
    }

    fn stop_aloud(&mut self) {
        self.sidebar.clear_notice();
        match self.desktop.reader() {
            Reader::This => self.aloud.press(aloud::Button::Stop),
            Reader::Another(holder) => holder.stopped(),
            Reader::Nobody(why) => self.sidebar.notify(why.to_owned()),
        }
    }

    fn hold_aloud(&mut self) {
        let here = matches!(self.focus.over, Some(Over::Dictation(..)));
        self.desktop.dictating(here);
        self.aloud.hold(here || self.desktop.another_dictates());
    }

    fn read_aloud_went(&mut self, read: Result<(), &str>) {
        if let Err(why) = read {
            self.sidebar.notify(why.to_owned());
        }
    }

    fn undictate(&mut self, dictated: Option<Dictated>) {
        let had = self.focus.session_told();
        let open = self.focus.over.take_if(|over| matches!(over, Over::Dictation(..)));
        let Some(Over::Dictation(_, under)) = open else {
            return;
        };
        let side = self.focus.side;
        match under {
            Where::Session(agent) => {
                self.give_from(had, Focus::on(side));
                let Some(dictated) = dictated else {
                    return;
                };
                let here = self.sidebar.here_agent() == agent;
                let dictated = if here { dictated } else { dictated.moved() };
                let Dictated { written, sends, .. } = dictated;
                let harness = agent.map_or(agents::Harness::ClaudeCode, |agent| agent.harness);
                for paste in pastes(&written, harness) {
                    self.paste(&paste);
                }
                if sends {
                    self.key_to_session(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            },
            Where::Ask(mut ask) => {
                if let Some(dictated) = &dictated {
                    ask.dictated(&dictated.written.to_string());
                }
                self.give_from(had, Focus { side, over: Some(Over::Ask(ask)) });
                if dictated.is_some_and(|dictated| dictated.sends) {
                    self.converse();
                }
            },
            Where::Issue(mut issue) => {
                if let Some(dictated) = &dictated {
                    issue.dictated(&dictated.written.to_string());
                }
                let sends = dictated.is_some_and(|dictated| dictated.sends) && issue.ready();
                self.give_from(had, Focus { side, over: Some(Over::Issue(issue)) });
                if sends {
                    self.file_issue();
                }
            },
        }
    }

    fn fetched(&mut self, fetched: &Fetched) {
        match fetched {
            Fetched::Got(got) => {
                if let Some(fetch) = &mut self.fetching {
                    fetch.got = *got;
                }
            },
            Fetched::Ended(ended) => {
                self.fetching = None;
                let shown = matches!(self.focus.over, Some(Over::Dictation(..)));
                if let (Err(why), false) = (ended, shown) {
                    self.sidebar.notify(format!("the speech model did not come: {why}"));
                }
            },
        }
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            dictation.fetched(fetched);
        }
    }

    fn step(&mut self, direction: Direction) {
        self.leave();
        self.ask_right(lunetad::Asked::Step(direction));
    }

    fn cross(&mut self, direction: Direction) {
        let Some(right) = &self.right else {
            return;
        };
        let on = right.machine().clone();
        let (before, after) = self.projected.tabs_beside(&on, right.worktree());
        let beside = match direction {
            Direction::Left => before,
            Direction::Right => after,
            Direction::Up | Direction::Down => None,
        };
        if let Some(beside) = beside {
            self.go_worktree(&on, beside, None);
        }
    }

    fn go(&mut self, machine: &Machine, place: &Place) -> bool {
        let gone = self.go_project(machine, place);
        if let Err(why) = &gone {
            self.sidebar.notify(why.clone());
        }
        gone.is_ok()
    }

    fn go_project(&mut self, machine: &Machine, place: &Place) -> Result<(), String> {
        let listed = place.listed();
        let session = listed.name();
        let Some(key) = listed.own_key() else {
            return Err(format!("{session} is one of zellij's sessions: {}", menu::OWN_ONLY));
        };
        let Some(project) = self.projected.project(machine, key) else {
            if let Machine::Remote(destination) = machine
                && !self.projected.reaches(machine)
            {
                return Err(format!("lunetad does not reach {destination} now"));
            }
            return Err(format!("{session} is no project lunetad keeps"));
        };
        let shows = |worktree| {
            self.right.as_ref().is_some_and(|right| {
                right.machine() == machine && right.worktree() == worktree
            })
        };
        if let Place::Worktree(place) = place {
            let held = project.worktrees().iter().any(|it| it.id == place.worktree);
            if !held {
                return Err(format!("{session} holds no such worktree"));
            }
            if !shows(place.worktree) {
                self.go_worktree(machine, place.worktree, None);
            }
            return Ok(());
        }
        let Some(pane) = place.pane() else {
            if project.worktrees().iter().any(|it| shows(it.id)) {
                return Ok(());
            }
            let first = project.worktrees().first();
            let first = first.ok_or_else(|| format!("{session} holds no worktree"))?;
            self.go_worktree(machine, first.id, None);
            return Ok(());
        };
        let pane = OwnPane::new(key, pane).ok_or("a view of id 0")?;
        let (worktree, terminal) = self.projected.of_pane(machine, pane).ok_or_else(|| {
            format!("lunetad lists no view {} in {session}", pane.id)
        })?;
        self.go_worktree(machine, worktree, Some(terminal));
        Ok(())
    }

    fn go_worktree(
        &mut self,
        on: &Machine,
        worktree: WorktreeId,
        terminal: Option<ViewId>,
    ) {
        if let Some(right) = &mut self.right {
            right.go(on, worktree);
        } else {
            let (size, greeting) = (layout_size(self.areas.1), greeting(self.colours));
            let (folder, events) = (self.lunetad.clone(), &self.events);
            let (number, on) = (self.shown_number_dropped + 1, on.clone());
            let runs_in = self.runs_in;
            let started =
                Viewed::start(number, folder, on, worktree, size, greeting, runs_in, events);
            self.right = Some(started);
        }
        let right = self.shown();
        assert_eq!(right.machine(), on, "luneta shows the machine it went to");
        assert_eq!(right.worktree(), worktree, "luneta shows the worktree it went to");
        let (tabs, titles) = self.projected.tabs(on, worktree);
        let right = self.shown();
        right.tabs(tabs, titles);
        if let Some(terminal) = terminal {
            right.ask(lunetad::Asked::Focus(terminal));
        }
        self.seat();
    }

    fn shown(&mut self) -> &mut Viewed {
        self.right.as_mut().expect("luneta went to a worktree")
    }

    fn leave_view(&mut self) {
        let Some(right) = &self.right else {
            return;
        };
        let (on, left) = (right.machine().clone(), right.worktree());
        let held = |machine: &Machine| {
            let projects = self.projected.projects(machine).iter();
            let worktrees = projects.flat_map(projected::Project::worktrees);
            let open = worktrees.filter(|it| !it.terminals.is_empty());
            let others = open.filter(|it| *machine != on || it.id != left);
            others.max_by_key(|it| it.used).map(|it| it.id)
        };
        let other = held(&on).map(|id| (on.clone(), id));
        let other = other.or_else(|| held(&Machine::Local).map(|id| (Machine::Local, id)));
        match other {
            Some((machine, other)) => self.go_worktree(&machine, other, None),
            None => self.unview(),
        }
    }

    fn unview(&mut self) {
        if let Some(right) = self.right.take() {
            let number = right.number();
            let dropped = self.shown_number_dropped;
            assert!(number > dropped, "a shown number grows past the last dropped");
            self.shown_number_dropped = number;
        }
        self.sidebar.unseat();
        self.seated = None;
        self.give(Focus::on(Side::Sidebar));
        assert_eq!(self.focus.side, Side::Sidebar, "with no worktree shown, no keys go to it");
    }

    fn seat(&mut self) {
        let Some(right) = &self.right else {
            return;
        };
        let (on, worktree) = (right.machine().clone(), right.worktree());
        let Some((project, _)) = self.projected.holding(&on, worktree) else {
            return;
        };
        let key = project.key;
        let pane = right.focused().and_then(|focused| project.pane_of(focused));
        let seated = |sidebar: &Sidebar| sidebar.own_seat_on(&on).map(sidebar::OwnSeat::session);
        if seated(&self.sidebar) != Some(key) {
            self.sidebar.attaching(on.clone(), key);
            self.seated = None;
        }
        assert_eq!(seated(&self.sidebar), Some(key), "the sidebar sits where luneta shows");
        self.sidebar.showing(worktree);
        if let Some(pane) = pane
            && self.seated != Some(pane)
        {
            let client = OwnClientId::new(CLIENT).expect("the client's id is not 0");
            self.sidebar.went_own(pane, client);
            self.seated = Some(pane);
        }
    }

    fn heeded(&mut self, heeded: Heeded) {
        match heeded {
            Heeded::Quiet => {},
            Heeded::Followed => {
                let right = self.shown();
                let (on, worktree) = (right.machine().clone(), right.worktree());
                let (tabs, titles) = self.projected.tabs(&on, worktree);
                self.shown().tabs(tabs, titles);
            },
            Heeded::Edged(direction) => self.cross(direction),
            Heeded::Sent(worktree) => {
                let Some(right) = &self.right else {
                    return;
                };
                let on = right.machine().clone();
                self.go_worktree(&on, worktree, None);
            },
            Heeded::Note(note) => self.sidebar.notify(note),
            Heeded::Emptied => self.leave_view(),
            Heeded::Forgotten(why) => {
                self.leave_view();
                self.sidebar.notify(format!("lunetad's layout: {why}"));
            },
            Heeded::Lost(why) => self.sidebar.notify(format!("lunetad's layout: {why}")),
        }
        self.seat();
    }

    fn make_worktree(
        &mut self,
        on: Machine,
        made: impl FnOnce(&Path, &Machine) -> Result<Landing, Unready> + Send + 'static,
    ) {
        if self.refuse_naming(None) {
            return;
        }
        let lunetad = self.lunetad.clone();
        self.naming_on(move || {
            let made = caught(|| Ok(made(&lunetad, &on)));
            let made = made.unwrap_or_else(|panicked| Err(Unready::Failed(panicked)));
            let made = made.map_err(|why| match why {
                Unready::Nowhere(why) | Unready::Failed(why) => format!("no worktree made: {why}"),
                Unready::InDoubt(why) => why,
            });
            Made::Worktree { on, made, argv: Vec::new() }
        });
    }

    fn refuse_naming(&mut self, menu: Option<u64>) -> bool {
        if self.naming == Naming::Idle {
            return false;
        }
        self.sidebar.notify(NAMING.to_owned());
        if let Some(menu) = menu {
            self.sidebar.drop_menu(menu);
        }
        true
    }

    fn naming_on(&mut self, made: impl FnOnce() -> Made + Send + 'static) {
        assert_eq!(self.naming, Naming::Idle, "luneta names one session at a time");
        self.naming = Naming::Busy;
        let events = self.events.clone();
        self.keep_change(thread::spawn(move || drop(events.send(Event::Named(made())))));
    }

    fn named(&mut self, made: Made) {
        assert_eq!(self.naming, Naming::Busy, "a session was named that `name` did not name");
        self.naming = Naming::Idle;
        match made {
            Made::Worktree { on, made: Ok(Landing::Worktree(made)), argv } => {
                self.go_opening(&on, made, argv);
                self.give_opened(Opener::LetGo);
            },
            Made::Worktree { on, made: Ok(Landing::Unopened(project)), .. } => {
                self.open_worktree(on, project);
            },
            Made::Worktree { made: Err(why), .. } => self.sidebar.notify(why),
            Made::Pulled { menu, made } => self.pulled_made(menu, made),
        }
    }

    fn go_opening(&mut self, on: &Machine, landed: Landed, argv: Vec<String>) {
        let worktree = landed.worktree();
        self.go_worktree(on, worktree, None);
        if let Landed::Found(_) = landed
            && !argv.is_empty()
        {
            self.shown().open_in(worktree, argv);
        }
    }

    fn pulled_made(&mut self, menu: u64, made: Result<Pulled, String>) {
        let opener = self.sidebar.made_for(menu).map_or(Opener::LetGo, Opener::of);
        let made = match made {
            Ok(made) => made,
            Err(why) => return self.sidebar.notify(format!("menu: no session made: {why}")),
        };
        let Pulled { on, worktree, argv } = made;
        self.go_opening(&on, worktree, argv);
        self.give_opened(opener);
    }

    fn give_opened(&mut self, opener: Opener) {
        assert!(self.right.is_some(), "luneta shows the view it opened");
        let held = self.focus.over.is_some() || self.sidebar.holds_input();
        match (opener, held) {
            (Opener::Holds, _) => {
                self.leave();
                let menu = matches!(self.focus.over, Some(Over::Menu));
                assert!(!menu, "the menu that opened the view holds the keys no longer");
            },
            (Opener::LetGo, false) => {
                self.give(Focus::on(self.focus.side.session()));
                assert!(self.focus.session(), "the opened view takes the keys");
            },
            (Opener::LetGo, true) => {},
        }
    }

    fn give(&mut self, focus: Focus) {
        let had = self.focus.session_told();
        self.give_from(had, focus);
    }

    fn give_from(&mut self, had: bool, focus: Focus) {
        let focus = match focus.side {
            Side::Session | Side::Collapsed if self.right.is_none() => {
                Focus { side: Side::Sidebar, ..focus }
            },
            Side::Session | Side::Sidebar | Side::Collapsed => focus,
        };
        if focus.side == Side::Collapsed || focus.over.is_some() {
            self.let_go();
            self.sidebar.let_go();
        }
        let has = focus.session_told();
        self.sidebar.focus(match (&focus.over, focus.side) {
            (None, Side::Sidebar) => Keys::Sidebar,
            (Some(Over::Menu), side) => Keys::Menu { lent: side == Side::Sidebar },
            (
                Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Issue(_)
                    | Over::Note(_)
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                Side::Sidebar,
            ) => Keys::Lent,
            (
                None
                | Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Issue(_)
                    | Over::Note(_)
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                Side::Session | Side::Collapsed,
            ) => Keys::Zellij,
        });
        self.focus = focus;
        self.hold_aloud();
        if self.focused && has != had {
            self.tell_focus(has);
        }
    }

    fn leave(&mut self) {
        let leaves = match &self.focus.over {
            None => self.focus.side == Side::Sidebar,
            Some(Over::Menu) => true,
            Some(
                Over::Brief(_)
                | Over::Ask(_)
                | Over::Issue(_)
                | Over::Note(_)
                | Over::Picker(_)
                | Over::Dictation(..),
            ) => false,
        };
        if leaves {
            self.give(Focus::on(self.focus.side.session()));
        }
    }

    fn focus_window(&mut self, focused: bool) {
        self.focused = focused;
        self.desktop.focus(focused);
        self.focus_alerts();
        if !focused && let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            dictation.unfocused();
        }
        if !focused {
            self.pointer = None;
            self.sidebar.let_go();
        }
        if self.focus.session_told() {
            self.tell_focus(focused);
        }
    }

    fn answered(&mut self, answer: Answer, terminal: &mut DefaultTerminal) -> io::Result<()> {
        match answer {
            Answer::Foreground(_) | Answer::Background(_) => self.colours.hear(answer),
            Answer::Turned(_) => {
                let backend = terminal.backend_mut();
                backend.write_all(answers::TURNED)?;
                backend.flush()?;
            },
            Answer::Device | Answer::Kitty => {},
        }
        Ok(())
    }

    fn failure(&self) -> Option<String> {
        self.gave_up.as_ref().map(|why| format!("luneta: {why}"))
    }

    fn draw(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        self.sidebar.aloud(self.aloud.shown());
        whole(terminal, |terminal| self.frame(terminal))
    }

    fn frame(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let now = SystemTime::now();
        terminal.draw(|frame| {
            let collapsed = self.focus.side == Side::Collapsed;
            let own = if collapsed { sidebar::COLLAPSED } else { self.width.columns() };
            let (sidebar, right) = areas(frame.area(), own);
            self.areas = (sidebar, right);
            let size = window(right);
            if let Some(right) = &mut self.right {
                right.resize(size.columns, size.lines);
            }
            let buffer = frame.buffer_mut();
            let beside = if collapsed {
                self.sidebar.draw_collapsed(buffer, sidebar, now);
                Drawn::Nothing
            } else {
                self.sidebar.draw(buffer, sidebar, now)
            };
            self.settling.drawn(self.sidebar.aloud_buttons(), Instant::now());
            let uncovered = |at: &Position| self.focus.over.is_none() && right.contains(*at);
            let spot = |at: Position| attached::Spot { row: at.y - right.y, column: at.x - right.x };
            let hover = self.pointer.filter(uncovered).map(spot);
            let cursor = if let Some(shown) = &self.right {
                shown.draw(buffer, right, hover)
            } else {
                draw_unshown(buffer, right);
                None
            };
            let drawn = match &self.focus.over {
                Some(Over::Brief(briefing)) => {
                    briefing.draw(buffer, right, now);
                    Drawn::Nothing
                },
                Some(Over::Ask(ask)) => {
                    ask.draw(buffer, right, now);
                    Drawn::Nothing
                },
                Some(Over::Issue(issue)) => {
                    issue.draw(buffer, right, now);
                    Drawn::Nothing
                },
                Some(Over::Picker(picker)) => picker.draw(buffer, right, now),
                Some(Over::Dictation(dictation, under)) => {
                    match under {
                        Where::Ask(ask) => ask.draw(buffer, right, now),
                        Where::Issue(issue) => issue.draw(buffer, right, now),
                        Where::Session(_) => {},
                    }
                    dictation.draw(buffer, right, (now, Instant::now()));
                    Drawn::Nothing
                },
                Some(Over::Note(note)) => {
                    menu::draw_note(buffer, right, note);
                    Drawn::Nothing
                },
                Some(Over::Menu) => {
                    let floating = self.sidebar.floating();
                    floating.map_or_default(|menu| menu.draw(buffer, right, now))
                },
                None => beside,
            };
            if let Some(pointer) = self.pointer {
                drawn.light(buffer, pointer);
                self.sidebar.aloud_buttons().light(buffer, pointer);
            }
            if self.focus.session()
                && let Some(cursor) = cursor
            {
                frame.set_cursor_position(cursor);
            }
            self.drawn = drawn;
        })?;
        let cursor = self.right.as_ref().map_or(UNSHOWN_CURSOR, Viewed::cursor_style);
        if self.cursor != Some(cursor) {
            self.cursor = Some(cursor);
            execute!(terminal.backend_mut(), cursor_style(cursor))?;
        }
        if let Some(title) = self.sidebar.own_title()
            && self.title.as_deref() != Some(title)
        {
            execute!(terminal.backend_mut(), SetTitle(title))?;
            self.title = Some(title.to_owned());
        }
        Ok(())
    }
}

fn clip(terminal: &mut DefaultTerminal, clipboard: ClipboardType, text: &str) -> io::Result<()> {
    let backend = terminal.backend_mut();
    write!(backend, "{}", osc52(clipboard, text))?;
    backend.flush()
}

fn osc52(clipboard: ClipboardType, text: &str) -> String {
    let which = match clipboard {
        ClipboardType::Clipboard => 'c',
        ClipboardType::Selection => 'p',
    };
    let text = base64::engine::general_purpose::STANDARD.encode(text);
    format!("\x1b]52;{which};{text}\x07")
}

fn whole<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    draw: impl FnOnce(&mut Terminal<CrosstermBackend<W>>) -> io::Result<()>,
) -> io::Result<()> {
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let drawn = draw(terminal);
    let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    drawn.and(ended)
}

fn pastes(written: &named::Written, harness: agents::Harness) -> Vec<String> {
    let spelled = |command: &skills::Name| agents::spelled(harness, command.as_str());
    let command = written.command.iter().map(|command| format!("{} ", spelled(command)));
    let text = (!written.text.is_empty()).then(|| format!("{} ", written.text));
    command.chain(text).collect()
}

fn draw_unshown(buffer: &mut Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.reset();
            }
        }
    }
    let dim = Style::new().add_modifier(Modifier::DIM);
    let rows = u16::try_from(UNSHOWN.len()).expect("a few rows");
    let top = area.y + area.height.saturating_sub(rows) / 2;
    for (line, y) in UNSHOWN.iter().zip(top..area.bottom()) {
        let width = u16::try_from(line.chars().count()).unwrap_or(u16::MAX);
        let x = area.x + area.width.saturating_sub(width) / 2;
        buffer.set_stringn(x, y, line, usize::from(area.width), dim);
    }
}

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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
    use ratatui::{TerminalOptions, Viewport};

    use super::*;

    #[test]
    fn the_end_waits_for_the_changes_sent_and_says_so_only_once_it_is_slow() {
        let mut said = 0;
        wait_for(vec![thread::spawn(|| {})], Duration::from_secs(60), || said += 1);
        assert_eq!(said, 0);
        let (go, gate) = mpsc::channel::<()>();
        let (done, ended) = mpsc::channel();
        let slow = thread::spawn(move || {
            let _ = gate.recv();
            let _ = done.send(());
        });
        wait_for(vec![slow], Duration::ZERO, || {
            said += 1;
            let _ = go.send(());
        });
        assert_eq!(said, 1);
        assert_eq!(ended.try_recv(), Ok(()), "the end outlasts the slow change");
    }

    #[test]
    fn alt_equals_or_plus_grows_with_or_without_shift_and_alt_minus_shrinks() {
        let (alt, shifted) = (KeyModifiers::ALT, KeyModifiers::ALT | KeyModifiers::SHIFT);
        let grows = [('=', alt), ('+', alt), ('=', shifted), ('+', shifted)];
        for (key, modifiers) in grows {
            assert_eq!(resized((KeyCode::Char(key), modifiers)), Some(Growth::Grow), "{key}");
        }
        assert_eq!(resized((KeyCode::Char('-'), alt)), Some(Growth::Shrink));
        let none = [('=', KeyModifiers::NONE), ('-', shifted), ('=', KeyModifiers::CONTROL)];
        for (key, modifiers) in none {
            assert_eq!(resized((KeyCode::Char(key), modifiers)), None, "{key} {modifiers:?}");
        }
    }

    #[test]
    fn what_to_copy_goes_out_as_osc_52() {
        let url = "https://github.com/o/r/pull/1";
        let spelled = "aHR0cHM6Ly9naXRodWIuY29tL28vci9wdWxsLzE=";
        assert_eq!(osc52(ClipboardType::Clipboard, url), format!("\x1b]52;c;{spelled}\x07"));
        assert_eq!(osc52(ClipboardType::Selection, url), format!("\x1b]52;p;{spelled}\x07"));
    }

    #[derive(Default)]
    struct Writes {
        written: Rc<RefCell<Vec<Vec<u8>>>>,
        writing: Vec<u8>,
    }

    impl io::Write for Writes {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writing.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            if !self.writing.is_empty() {
                self.written.borrow_mut().push(std::mem::take(&mut self.writing));
            }
            Ok(())
        }
    }

    fn frame(terminal: &mut Terminal<CrosstermBackend<Writes>>, glyph: &str, cursor: Position) {
        whole(terminal, |terminal| {
            terminal.draw(|frame| {
                frame.buffer_mut()[(1, 1)].set_symbol(glyph);
                frame.set_cursor_position(cursor);
            })?;
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn what_is_dictated_is_pasted_and_a_command_on_its_own() {
        let written = |command: Option<&str>, text: &str| {
            let command = command.map(|name| skills::Name::new(name.to_owned()).expect("a name"));
            named::Written { command, text: text.to_owned(), only_first: Vec::new() }
        };
        let pasted = |command: Option<&str>, text: &str| {
            pastes(&written(command, text), agents::Harness::ClaudeCode)
        };
        let pi = pastes(&written(Some("handoff"), "now"), agents::Harness::Pi);
        assert_eq!(pi, ["/skill:handoff ", "now "]);
        assert_eq!(
            pasted(Some("handoff"), "the next session fixes CI"),
            ["/handoff ", "the next session fixes CI "]
        );
        assert_eq!(pasted(Some("handoff"), ""), ["/handoff "]);
        assert_eq!(pasted(None, "hand this off"), ["hand this off "]);
    }

    fn shown(screen: &Term<VoidListener>) -> Option<(i32, usize)> {
        let cursor = screen.renderable_content().cursor;
        let point = (cursor.point.line.0, cursor.point.column.0);
        (cursor.shape != CursorShape::Hidden).then_some(point)
    }

    #[test]
    fn the_cursor_shows_only_where_a_frame_puts_it() {
        let viewport = Viewport::Fixed(Rect::new(0, 0, 20, 4));
        let writes = Writes::default();
        let written = Rc::clone(&writes.written);
        let backend = CrosstermBackend::new(writes);
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        let mut screen = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let right = Position::new(15, 3);
        frame(&mut terminal, "⠋", right);
        for write in written.take() {
            parser.advance(&mut screen, &write);
        }
        assert_eq!(shown(&screen), Some((3, 15)));
        frame(&mut terminal, "⠙", right);
        let writes = written.take();
        assert!(writes.len() > 1, "a frame goes out in more than one write");
        for write in writes {
            parser.advance(&mut screen, &write);
            assert_eq!(shown(&screen), Some((3, 15)));
        }
        assert_eq!(screen.grid()[Point::new(Line(1), Column(1))].c, '⠙');
    }

    #[test]
    fn putting_back_shows_a_frame_luneta_ended_in() {
        let mut screen = Term::new(Config::default(), &TermSize::new(20, 4), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let mut ended = Vec::new();
        execute!(ended, SetCursorStyle::SteadyBar, BeginSynchronizedUpdate).unwrap();
        ended.extend_from_slice("⠙".as_bytes());
        parser.advance(&mut screen, &ended);
        let spinner = Point::new(Line(0), Column(0));
        assert_eq!(screen.grid()[spinner].c, ' ', "the frame is held");
        assert_eq!(screen.cursor_style().shape, CursorShape::Beam);
        let mut put = Vec::new();
        put_back(&mut put).unwrap();
        parser.advance(&mut screen, &put);
        assert_eq!(screen.grid()[spinner].c, '⠙');
        assert_eq!(screen.cursor_style(), Config::default().default_cursor_style);
    }

    #[test]
    fn a_ticket_stopped_sends_no_more_and_the_last_ones_stopped_are_held() {
        let mut stopped = Stopped::default();
        assert!(!stopped.holds(1));
        stopped.stop(1);
        assert!(stopped.holds(1));
        assert!(!stopped.holds(2), "another ticket goes on");
        let last = u64::try_from(sidebar::MOVES_MAX).unwrap() + 1;
        for ticket in 2..=last {
            stopped.stop(ticket);
        }
        assert!(!stopped.holds(1), "the oldest goes once the most are held");
        assert!((2..=last).all(|ticket| stopped.holds(ticket)));
    }
}
