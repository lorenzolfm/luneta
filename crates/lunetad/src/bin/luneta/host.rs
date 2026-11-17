use std::collections::VecDeque;
use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU16;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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
use lunetad::api::Host;
use lunetad::issues::{Filed, OnGitHub, Remote, Unfiled};
use lunetad::log::{self, Process};
use lunetad::mux::own::viewer::{Greeting, Seen};
use lunetad::mux::own::{OwnClientId, OwnKey, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{TerminalId, WorkstreamId};
use lunetad::prunes;
use lunetad::turns::{IntoPanes, OtherTab};
use lunetad::{Destination, Directory};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::{DefaultTerminal, Terminal};

use crate::alerts::{self, Alerts};
use crate::aloud::{self, Aloud};
use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange};
use crate::ask::{self, Ask, Keyed};
use crate::attached;
use crate::agents::{self, Kept};
use crate::brief::{self, Asking, Briefing, Written};
use crate::browser::Browser;
use crate::checks;
use crate::close::{Act, Whose};
use crate::desktop::{self, Desktop, Reader, Told};
use crate::dictation::{self, Dictated, Dictation};
use crate::due::Due;
use crate::folds::{self, Folds};
use crate::images::{self, Went};
use crate::input;
use crate::issue::{self, Issue};
use crate::machines::{self, Added, Machine};
use crate::menu::{self, Choice};
use crate::models::Fetched;
use crate::named;
use crate::news::{self, News};
use crate::order::{self, Order};
use crate::prune;
use crate::peers;
use crate::picker::{self, List, Picked, Picker};
use crate::printed::{self, Printed};
use crate::pull::{self, Found, Link};
use crate::sidebar::{self, Asked, HereAgent, Keys, PaneKey, Place, Sidebar};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Speech, Spoken};
use crate::spots::{Burst, Drawn, Hit, Settling};
use crate::projected::{self, Projected, Route};
use crate::viewed::{self, Heeded, Viewed};
use crate::workstreams;
use crate::width::Width;
use crate::zellij::{self, Close, Closes, Open, SessionName, Turned};

pub enum Made {
    Workstream { made: Result<WorkstreamId, String>, argv: Vec<String> },
    Pulled { menu: u64, made: Result<Pulled, String> },
}

pub struct Pulled {
    workstream: WorkstreamId,
    argv: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Naming {
    Idle,
    Busy,
}

const NAMING: &str = "menu: luneta names a session already: try again once it has made it";

const NAMING_PANICKED: &str = "menu: luneta could not name the session: naming it panicked";

pub const NOT_YET: &str = "luneta does not do that to a workstream yet";

const CLIENT: u64 = 1;

pub enum Event {
    Input(Input),
    Answered(Answer),
    InputEnded(io::Error),
    Shown(u64, lunetad::Heard),
    Seen { number: u64, watcher: u64, seen: Seen },
    Reached { claude_folder: Option<PathBuf> },
    Daemon(Result<Host, String>),
    Notice(String),
    Copy(Link),
    CopyText(String),
    Image(Went),
    ImageSlow(Destination),
    Opened { menu: u64, opened: Result<u32, String> },
    Named(Made),
    Closes { menu: u64, closes: Result<Closes, String> },
    Tabs { menu: u64, tabs: Result<Vec<OtherTab>, String> },
    IntoPanes { menu: u64, pane: u32, came: Result<IntoPanes, String> },
    NoClient { menu: u64, owed: Due<u32>, on: Machine, turned: Turned },
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
    Order,
    Agent,
    Alerted(alerts::Read),
    Clicked(alerts::Key),
    #[cfg(not(target_os = "macos"))]
    IssueClicked(desktop::Came),
    Peers(peers::Heard),
}

const TICK: Duration = Duration::from_secs(1);

const FRAME_EVENTS_MAX: usize = 256;

const SWAP: (KeyCode, KeyModifiers) = (KeyCode::Char('a'), KeyModifiers::CONTROL);

const COLLAPSE: (KeyCode, KeyModifiers) = (KeyCode::Char('q'), KeyModifiers::CONTROL);

const NEXT: (KeyCode, KeyModifiers) = (KeyCode::Tab, KeyModifiers::ALT);

const MENU: (KeyCode, KeyModifiers) = (KeyCode::Char('a'), KeyModifiers::ALT);

const SESSIONS: (KeyCode, KeyModifiers) = (KeyCode::Char('j'), KeyModifiers::CONTROL);

const AGENTS: (KeyCode, KeyModifiers) = (KeyCode::Char('b'), KeyModifiers::CONTROL);

const DIRS: (KeyCode, KeyModifiers) = (KeyCode::Char('d'), KeyModifiers::CONTROL);

pub const ANYWHERE: [((KeyCode, KeyModifiers), &str); 10] = [
    (SWAP, "Sidebar or panes"),
    (NEXT, "Next pane"),
    (COLLAPSE, "Collapse sidebar"),
    (MENU, "Menu over panes"),
    (SESSIONS, "Pick a session"),
    (AGENTS, "Pick an agent"),
    (DIRS, "Pick a directory"),
    (dictation::KEY, "Hold to dictate"),
    (aloud::KEY, "Read aloud, pause"),
    (aloud::STOPS, "Stop reading"),
];

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
    right: Viewed,
    projected: Projected,
    seated: Option<OwnPane>,
    lunetad: PathBuf,
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
    ended: bool,
    gave_up: Option<String>,
    deleting: Vec<JoinHandle<()>>,
    naming: Naming,
    alerts: Alerts,
    desktop: Desktop,
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
    let cwd = workstreams::real(&dir);
    let named = session.map(SessionName::as_str);
    let seen = |project| {
        news.last_seen(crate::ledger::Session::Project(project))
    };
    let start = match workstreams::start(&lunetad, named, &cwd, seen) {
        Ok(start) => start,
        Err(refusal) => return did_not_start(&format!("no workstream to show: {refusal}")),
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
    (start, runs_in): (WorkstreamId, Option<TerminalKey>),
    news: News,
    folder: Result<PathBuf, String>,
    unkept: Option<String>,
    dir: &Path,
    colours: Colours,
    releases: bool,
    events: Sender<Event>,
    inbox: &mut Inbox,
) -> io::Result<Option<String>> {
    let (width, width_unread) = Width::kept();
    let folds = Folds::shared();
    let order = Order::shared();
    let folds_dirs = folds.dirs();
    let folds_watched =
        (!folds_dirs.is_empty()).then(|| folds::watch(&folds_dirs, events.clone())).transpose();
    let news_watched = news::watch(&news, events.clone());
    let order_watched = order.file().map(|file| order::watch(file, events.clone())).transpose();
    let (agent, agent_unread) = Kept::shared();
    let agent_watched = agent.file().map(|file| agents::watch(file, events.clone())).transpose();
    let mut sidebar = Sidebar::new(folds, news, order, agent, machines::local_name());
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
    match order_watched {
        Ok(watcher) => watching.extend(watcher),
        Err(error) => {
            sidebar.notify(format!("rows moved elsewhere show from luneta's next start: {error}"));
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
    let (size, greeting) = (view_size(areas.1), greeting(runs_in, colours));
    let right = Viewed::start(1, lunetad.clone(), start, size, greeting, &events);
    let desktop = Desktop::start(&events);
    let focused = true;
    let mut app = App {
        right,
        projected: Projected::default(),
        seated: None,
        lunetad,
        sidebar,
        areas,
        width,
        held: Held::Nothing,
        last_press: LastPress::Elsewhere,
        focus: Focus::on(Side::Session),
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
        ended: false,
        gave_up: None,
        deleting: Vec::new(),
        naming: Naming::Idle,
        alerts: Alerts::new(focused),
        desktop,
    };
    app.watch();
    let ran = run_loop(terminal, &mut app, inbox);
    for deleting in app.deleting.drain(..) {
        let _ = deleting.join();
    }
    ran?;
    Ok(app.failure())
}

fn greeting(runs_in: Option<TerminalKey>, colours: Colours) -> Greeting {
    let (foreground, background) = colours.both();
    Greeting { runs_in, foreground, background }
}

fn view_size(area: Rect) -> lunetad::projects::Size {
    let size = window(area);
    viewed::view_size(size.columns, size.lines)
}

fn caught<T>(named: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(named)).unwrap_or_else(|_| Err(NAMING_PANICKED.to_owned()))
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
            let Some(event) = inbox.try_next() else {
                break;
            };
            app.handle(event, terminal)?;
        }
        if app.ended {
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
                if self.right.number() == number {
                    let heeded = self.right.heard(heard);
                    self.heeded(heeded);
                }
            },
            Event::Seen { number, watcher, seen } => {
                if self.right.number() == number {
                    let heeded = self.right.seen(watcher, seen);
                    self.heeded(heeded);
                }
            },
            Event::Reached { claude_folder } => self.claude_folder = claude_folder,
            Event::Daemon(Ok(mut host)) => {
                self.projected = Projected::adapted(&mut host).unwrap_or_else(|why| {
                    self.sidebar.notify(format!("lunetad's projects: {why}"));
                    Projected::default()
                });
                let (tabs, titles) = self.projected.tabs(self.right.workstream());
                self.right.tabs(tabs, titles);
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
            Event::Tabs { menu, tabs } => {
                let asked = self.sidebar.tabs(menu, tabs);
                self.follow(asked);
            },
            Event::IntoPanes { menu, pane, came } => {
                let asked = self.sidebar.turned_into_panes(menu, pane, came);
                self.follow(asked);
            },
            Event::NoClient { menu, owed, on, turned } => match self.sidebar.waits_on(menu) {
                Some(sidebar::Held::Open) => {
                    let place = Place::Pane(turned.place().clone());
                    let gone = self.go(&on, &place);
                    owed.pay(Err(zellij::NOT_THERE_YET.to_owned()));
                    if !gone {
                        self.sidebar.drop_menu(menu);
                    }
                },
                Some(sidebar::Held::Kept) => {
                    owed.pay(Err(lunetad::opens::no_client(turned.place().session.name())));
                },
                None => {},
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
            Event::Order => self.sidebar.reorder(),
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
        self.right.ticked();
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
            let over = match self.sidebar.float_menu() {
                Ok(()) => Over::Menu,
                Err(why) => Over::Note(why.to_owned()),
            };
            self.give(Focus { side: self.focus.side, over: Some(over) });
            return;
        }
        let list = match chord {
            SESSIONS => Some(List::Sessions),
            AGENTS => Some(List::Agents),
            DIRS => Some(List::Dirs),
            _ => None,
        };
        if let Some(list) = list {
            self.pickers += 1;
            let aim = match (list, self.focus.side) {
                (List::Sessions | List::Dirs, Side::Sidebar) => self.sidebar.on_header(),
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
        if chord == NEXT {
            self.next();
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
        self.right.key(key);
    }

    fn paste(&mut self, text: &str) {
        self.right.paste(text);
    }

    fn tell_focus(&mut self, has: bool) {
        self.right.focus(has);
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
        if let Some(workstream) = self.right.mouse(mouse, at, self.areas.1.width) {
            self.go_workstream(workstream, None);
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
        let own = &mut self.right;
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
                Act::Pane { session, whose, pane } => {
                    zellij::close(&on, session, whose, Close::Pane(pane.id()), &self.events);
                },
                Act::Tab { session, whose, tab, panes } => {
                    let close = Close::Tab { id: tab, panes };
                    zellij::close(&on, session, whose, close, &self.events);
                },
                Act::Kill { session, whose } => {
                    self.deleting.retain(|deleting| !deleting.is_finished());
                    self.deleting.push(zellij::kill(&on, session, whose, &self.events));
                },
                Act::Leave { session, to, .. } => {
                    self.go(&on, &to);
                    zellij::stays(&session, &self.events);
                },
            },
            Asked::Open { menu, on, whose: Whose::Own(key), open } => {
                self.project_open(menu, &on, key, &open);
            },
            Asked::Open { menu, on, whose: Whose::Zellij(_), open } => {
                zellij::open(menu, &on, open, &self.events);
            },
            Asked::Tabs { menu, on, turned } => {
                zellij::other_tabs(menu, &on, &turned, &self.events);
            },
            Asked::Turn { menu, on, turned, turn } => {
                zellij::turn(menu, &on, turned, turn, &self.events);
            },
            Asked::StartOwn { starts } => {
                match lunetad::names::free_at(starts) {
                    Ok((name, _, dir)) => self.make_workstream(dir, move |lunetad, cwd| {
                        workstreams::here(lunetad, cwd, Some(name.as_str()))
                    }),
                    Err(why) => self.sidebar.notify(format!("menu: no workstream made: {why}")),
                }
                self.leave();
            },
            Asked::Make { menu, on, session, cwd, name, command } => {
                if self.refuse_naming(Some(menu)) {
                    return;
                }
                let lunetad = self.lunetad.clone();
                self.naming_on(move || {
                    let made = caught(|| match on {
                        Machine::Local => {
                            let cwd = workstreams::real(Path::new(cwd.as_str()));
                            let session = session.as_str();
                            let made = workstreams::pulled(&lunetad, session, &cwd, &name)?;
                            Ok(Pulled { workstream: made, argv: command })
                        },
                        Machine::Remote(_) => Err(menu::HERE_ONLY.to_owned()),
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
                    self.deleting.retain(|deleting| !deleting.is_finished());
                    self.deleting.push(deleting);
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
        }
    }

    fn project_act(&mut self, on: &Machine, act: &Act) {
        let (Machine::Local, Act::Pane { pane: PaneKey::Terminal(terminal), .. }) = (on, act) else {
            return self.sidebar.notify(format!("menu: {NOT_YET}"));
        };
        let (terminal, events) = (*terminal, self.events.clone());
        thread::spawn(move || {
            if let Err(failure) = lunetad::mux::own::server::Terminals.closed(terminal) {
                let _ = events.send(Event::Notice(format!("menu: not closed: {failure}")));
            }
        });
    }

    fn project_open(&mut self, menu: u64, on: &Machine, key: OwnKey, open: &Open) {
        self.sidebar.drop_menu(menu);
        let Machine::Local = on else {
            return self.sidebar.notify(format!("menu: {NOT_YET}"));
        };
        let Some(project) = self.projected.project(key) else {
            return self.sidebar.notify("menu: lunetad keeps no such project".to_owned());
        };
        match open {
            Open::Pane { beside, command, .. } => {
                let pane = OwnPane::new(project.key, *beside);
                let Some((workstream, terminal)) = pane.and_then(|it| self.projected.of_pane(it))
                else {
                    return self.sidebar.notify(format!("menu: lunetad lists no terminal {beside}"));
                };
                self.go_workstream(workstream, Some(terminal));
                self.right.open_beside_focused(command.clone());
            },
            Open::Tab { tab, .. } => {
                let (lunetad, id) = (self.lunetad.clone(), project.id);
                let (cwd, name, argv) = (tab.cwd.clone(), tab.name.clone(), tab.command.clone());
                if self.refuse_naming(None) {
                    return;
                }
                self.naming_on(move || {
                    let made = caught(|| {
                        workstreams::add_workstream(&lunetad, id, cwd.as_str(), name.as_deref())
                    });
                    let made = made.map_err(|why| format!("menu: no workstream made: {why}"));
                    Made::Workstream { made, argv }
                });
            },
        }
    }

    fn picked(&mut self, picked: Picked) {
        match picked {
            Picked::Nothing => {},
            Picked::Close => self.give(Focus::on(self.focus.side)),
            Picked::Go { on, place } => {
                self.go(&on, &place);
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Start { on: Machine::Local, session } => {
                match Directory::parse(self.dir.to_string_lossy().into_owned()) {
                    Ok(cwd) => self.make_workstream(cwd, move |lunetad, cwd| {
                        workstreams::start(lunetad, Some(session.as_str()), cwd, |_| None)
                    }),
                    Err(why) => self.sidebar.notify(format!("no workstream made: {why}")),
                }
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Start { on: Machine::Remote(_), .. }
            | Picked::Make { on: Machine::Remote(_), .. } => {
                self.sidebar.notify(format!("picker: {}", menu::HERE_ONLY));
            },
            Picked::BringBack { session, .. } => {
                self.sidebar.notify(format!("picker: zellij keeps {session}: {}", menu::OWN_ONLY));
            },
            Picked::Make { on: Machine::Local, session, cwd } => {
                self.make_workstream(cwd, move |lunetad, cwd| {
                    workstreams::here(lunetad, cwd, Some(session.as_str()))
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
        if let Some((number, to)) = issue.send(&self.events, &mut self.unfiled) {
            self.sidebar.filing(number, &to.to_string());
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

    fn next(&mut self) {
        self.leave();
        if let Some(place) = self.sidebar.next_pane() {
            let machine = self.sidebar.on().clone();
            self.go(&machine, &place);
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
        if let Machine::Remote(destination) = machine {
            return Err(format!("luneta cannot go to {session}, a project of {destination}'s, yet"));
        }
        let project = self.projected.project(key);
        let project = project.ok_or_else(|| format!("{session} is no project lunetad keeps"))?;
        let Some(pane) = place.pane() else {
            let shown = self.right.workstream();
            if project.workstreams.iter().any(|it| it.id == shown) {
                return Ok(());
            }
            let first = project.workstreams.first();
            let first = first.ok_or_else(|| format!("{session} holds no workstream"))?;
            self.go_workstream(first.id, None);
            return Ok(());
        };
        let pane = OwnPane::new(key, pane).ok_or("a pane of id 0")?;
        let (workstream, terminal) = self.projected.of_pane(pane).ok_or_else(|| {
            format!("lunetad lists no terminal {} in {session}", pane.id)
        })?;
        self.go_workstream(workstream, Some(terminal));
        Ok(())
    }

    fn go_workstream(&mut self, workstream: WorkstreamId, terminal: Option<TerminalId>) {
        self.right.go(workstream);
        let (tabs, titles) = self.projected.tabs(workstream);
        self.right.tabs(tabs, titles);
        if let Some(terminal) = terminal {
            self.right.ask(lunetad::Asked::Focus(terminal));
        }
        self.seat();
    }

    fn seat(&mut self) {
        let workstream = self.right.workstream();
        let Some((project, _)) = self.projected.holding(workstream) else {
            return;
        };
        let key = project.key;
        let pane = self.right.focused().and_then(|focused| project.pane_of(focused));
        let seated = self.sidebar.own_seat().map(sidebar::OwnSeat::session);
        if seated != Some(key) {
            self.sidebar.attaching(key);
            self.seated = None;
        }
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
            Heeded::Note(note) => self.sidebar.notify(note),
            Heeded::Forgotten(why) => {
                let forgotten = self.right.workstream();
                let workstreams = self.projected.projects().iter().flat_map(|it| &it.workstreams);
                let other = workstreams.map(|it| it.id).find(|id| *id != forgotten);
                match other {
                    Some(other) => self.go_workstream(other, None),
                    None => self.end("no workstream is left to show"),
                }
                self.sidebar.notify(format!("lunetad's view: {why}"));
            },
            Heeded::Lost(why) => self.sidebar.notify(format!("lunetad's view: {why}")),
        }
        self.seat();
    }

    fn make_workstream(
        &mut self,
        cwd: Directory,
        made: impl FnOnce(&Path, &Path) -> Result<WorkstreamId, String> + Send + 'static,
    ) {
        if self.refuse_naming(None) {
            return;
        }
        let lunetad = self.lunetad.clone();
        self.naming_on(move || {
            let made = caught(|| made(&lunetad, &workstreams::real(Path::new(cwd.as_str()))));
            let made = made.map_err(|why| format!("no workstream made: {why}"));
            Made::Workstream { made, argv: Vec::new() }
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
        self.naming = Naming::Busy;
        let events = self.events.clone();
        thread::spawn(move || drop(events.send(Event::Named(made()))));
    }

    fn named(&mut self, made: Made) {
        assert_eq!(self.naming, Naming::Busy, "a session was named that `name` did not name");
        self.naming = Naming::Idle;
        match made {
            Made::Workstream { made: Ok(made), argv } => self.go_opening(made, argv),
            Made::Workstream { made: Err(why), .. } => self.sidebar.notify(why),
            Made::Pulled { menu, made } => self.pulled_made(menu, made),
        }
    }

    fn go_opening(&mut self, workstream: WorkstreamId, argv: Vec<String>) {
        self.go_workstream(workstream, None);
        if !argv.is_empty() {
            self.right.open_in(workstream, argv);
        }
    }

    fn pulled_made(&mut self, menu: u64, made: Result<Pulled, String>) {
        self.sidebar.made_for(menu);
        let made = match made {
            Ok(made) => made,
            Err(why) => return self.sidebar.notify(format!("menu: no session made: {why}")),
        };
        let Pulled { workstream, argv } = made;
        self.go_opening(workstream, argv);
    }

    fn give(&mut self, focus: Focus) {
        let had = self.focus.session_told();
        self.give_from(had, focus);
    }

    fn give_from(&mut self, had: bool, focus: Focus) {
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

    fn end(&mut self, why: &str) {
        say_ends(why);
        self.ended = true;
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
            self.right.resize(size.columns, size.lines);
            let buffer = frame.buffer_mut();
            let beside = if collapsed {
                self.sidebar.draw_collapsed(buffer, sidebar, now);
                Drawn::Nothing
            } else {
                self.sidebar.draw(buffer, sidebar, now)
            };
            self.settling.drawn(self.sidebar.aloud_buttons(), Instant::now());
            let cursor = self.right.draw(buffer, right);
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
        let cursor = self.right.cursor_style();
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
}
