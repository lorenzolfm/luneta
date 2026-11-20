use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::{self, Write as _};
use std::iter;
use std::mem;
use std::num::NonZeroU16;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::vte::ansi::CursorShape;
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
use lunetad::changes::Unchanged;
use lunetad::issues::{Filed, OnGitHub, Remote, Unfiled};
use lunetad::log::{self, Process};
use lunetad::mux::own::viewer::{Greeting, Seen};
use lunetad::mux::TerminalKey;
use lunetad::projects::{self, ProjectId, ViewId, WorktreeId};
use lunetad::prunes;
use lunetad::asks::Marked;
use lunetad::rebases::{Rebased, Unrebased};
use lunetad::work;
use lunetad::{Destination, Direction, Growth};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::{DefaultTerminal, Terminal};

use crate::actions::{Chord, COLLAPSE, DOWN, GROW, LEFT, MENU, RIGHT, SHRINK, SWAP, UP};
use crate::alerts::{self, Alerts};
use crate::aloud::{self, Aloud};
use crate::answers::{self, Answer, Colours, DisableThemeChange, EnableThemeChange};
use crate::ask::{self, Ask, Keyed};
use crate::home::{self, Home};
use crate::attached;
use crate::agents::{self, Kept};
use crate::brief::{self, Asking, Briefing, Written};
use crate::browser::Browser;
use crate::checks;
use crate::close::Act;
use crate::copied::{Clip, Copies};
use crate::desktop::{self, Desktop, Reader, Told};
use crate::dictation::{self, Dictated, Dictation};
use crate::folds::{self, Folds};
use crate::images::{self, Went};
use crate::input;
use crate::issue::{self, Issue};
use crate::machines::{self, Added, Machine};
use crate::menu::{self, Choice, Origin, Rename};
use crate::models::Fetched;
use crate::named;
use crate::news::{self, News};
use crate::prune;
use crate::rebase;
use crate::peers;
use crate::picker::{self, List, Palette, Picked, Picker};
use crate::printed::{self, Printed};
use crate::pull::{self, Found};
use crate::sidebar::{
    self, Asked, Floating, HereAgent, Holding, Keys, Listed, Place, Reorder, Seat, Sidebar, Sitting,
    ViewKey, ViewPlace,
};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Speech, Spoken};
use crate::spots::{Burst, Drawn, Hit, Settling};
use crate::projected::{self, Projected};
use crate::viewed::{self, Heeded, Viewed};
use crate::worktrees::{self, Arrival, Landed, Landing};
use crate::width::Width;

pub enum Made {
    Project { on: Machine, made: Result<ProjectId, String> },
    Worktree { on: Machine, made: Result<Landing, String>, argv: Vec<String> },
    Pulled { menu: u64, made: Result<Pulled, String> },
}

pub struct Pulled {
    on: Machine,
    worktree: Landed,
    argv: Vec<String>,
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

const NAMING: &str = "luneta names a session already: try again once it has made it";

const NAMING_PANICKED: &str = "luneta could not name the session: naming it panicked";

const RENAME_PANICKED: &str = "luneta could not ask lunetad for the rename: asking panicked";

const CLOSE_PANICKED: &str = "luneta could not ask lunetad for the close: asking panicked";

const WORK_PANICKED: &str = "luneta could not ask lunetad for the work: asking panicked";

pub const NOT_YET: &str = "luneta does not do that to a worktree yet";

const WELCOME_MENU: &str = "the menu is for terminals: d d closes this welcome view";

const HOME_MENU: &str = "the menu is for terminals: the project's is on its row in the sidebar";

const UNSHOWN: [&str; 2] =
    ["No worktree to show yet.", "o w on a project's row opens one."];

const UNSHOWN_CURSOR: (CursorShape, bool) = (CursorShape::HollowBlock, false);

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
    Unclosed { on: Machine, act: Act, ticket: u64, unchanged: Unchanged },
    Moved { ticket: u64, kept: Option<Version> },
    Unmoved { on: Machine, ticket: u64, order: Reorder, unmoved: Unchanged },
    Copy(Clip),
    Image(Went),
    ImageSlow(Destination),
    Named(Made),
    Skills { menu: u64, agent: agents::Harness, skills: Result<Vec<Skill>, String> },
    Pulled { menu: u64, found: Result<Found, String> },
    Prunes { menu: u64, plan: Result<prunes::Plan, String> },
    Pruned { menu: u64, pruned: Result<prunes::Pruned, String> },
    Printed { menu: u64, printed: Result<Printed, String> },
    Checks { menu: u64, checks: Result<checks::Failing, String> },
    Rebased { menu: u64, rebased: Result<Rebased, Unrebased> },
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
    ReadAloud { from: u64, selected: String },
    StopAloud,
    Folds,
    News,
    Agent,
    Work { number: u64, listed: Result<work::Listed, String> },
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

    fn keys(&self) -> Keys {
        match (&self.over, self.side) {
            (None, Side::Sidebar) => Keys::Sidebar,
            (
                Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Issue(_)
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
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                Side::Session | Side::Collapsed,
            ) => Keys::Terminal,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Holder<'a> {
    Terminal,
    Sidebar(Holding),
    Box(&'a Over),
}

impl<'a> Holder<'a> {
    fn of(focus: &'a Focus, holding: Holding) -> Holder<'a> {
        if let Some(over) = &focus.over {
            assert_ne!(holding, Holding::Afloat, "nothing floats under a box");
            return Holder::Box(over);
        }
        match (holding, focus.side) {
            (holding @ Holding::Afloat, _) | (holding, Side::Sidebar) => Holder::Sidebar(holding),
            (Holding::Nothing | Holding::Press, Side::Session | Side::Collapsed) => {
                Holder::Terminal
            },
            (
                Holding::Chord | Holding::Search | Holding::Reorder | Holding::Menu,
                Side::Session | Side::Collapsed,
            ) => unreachable!("the sidebar's own modes end as its keys go"),
        }
    }

    fn reported(self) -> Reported {
        match self {
            Holder::Terminal | Holder::Box(Over::Dictation(_, Where::Session(_))) => {
                Reported::Focused
            },
            Holder::Sidebar(_)
            | Holder::Box(
                Over::Brief(_)
                | Over::Ask(_)
                | Over::Issue(_)
                | Over::Picker(_)
                | Over::Dictation(..),
            ) => Reported::Unfocused,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reported {
    Focused,
    Unfocused,
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

#[expect(clippy::large_enum_variant, reason = "luneta holds one right side, shown at a time")]
enum Right {
    Worktree(Viewed),
    Home(home::Key),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Homed {
    Named,
    Unknown,
    Gone { on: Machine },
}

struct App {
    right: Option<Right>,
    homes: home::Homes,
    shown_number_dropped: u64,
    runs_in: Option<TerminalKey>,
    projected: Projected,
    lunetad: PathBuf,
    movers: HashMap<Machine, Mover>,
    sidebar: Sidebar,
    areas: (Rect, Rect),
    width: Width,
    held: Held,
    last_press: LastPress,
    focus: Focus,
    reported: Reported,
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
    copies: Copies,
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

pub fn run(named: Option<&str>, folder: Result<PathBuf, String>) -> ExitCode {
    let heard = input::Ends::heard();
    let unkept = log::start(Process::Luneta).err();
    let mut ends = match heard {
        Ok(ends) => ends,
        Err(error) => return did_not_start(&format!("its end signals are not heard: {error}")),
    };
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
    let seen = |project| {
        news.last_seen(crate::ledger::Session::Project(project))
    };
    let runs_in = match lunetad::mux::own::terminal_here() {
        Ok(runs_in) => runs_in,
        Err(why) => return did_not_start(&why),
    };
    let start = match ends.after(|| worktrees::start(&lunetad, named, &cwd, seen)) {
        Ok(Ok(start)) => start,
        Ok(Err(refusal)) => return did_not_start(&format!("no worktree to show: {refusal}")),
        Err(ended) => return did_not_start(&ended.to_string()),
    };
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
    input::read(events.clone(), ends);
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
    let shown = give_back(terminal);
    let left = ratatui::try_restore();
    let put = popped.and(restored).and(shown).and(left);
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
    let _ = writeln!(io::stderr(), "luneta: {why}");
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

fn give_back(mut terminal: DefaultTerminal) -> io::Result<()> {
    let shown = terminal.show_cursor();
    mem::forget(terminal);
    shown
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
    (start, runs_in): (Option<Arrival>, Option<TerminalKey>),
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
        Some(Arrival::Worktree(start)) => (Some(start), None),
        Some(Arrival::Home(root)) => (None, Some(root)),
        None => (None, None),
    };
    let right = start.map(|start| {
        let viewed = Viewed::start(1, lunetad.clone(), here, start, size, greeting, runs_in, &events);
        Right::Worktree(viewed)
    });
    let focus = Focus::on(if right.is_some() { Side::Session } else { Side::Sidebar });
    sidebar.focus(focus.keys());
    let reported = Holder::of(&focus, sidebar.holding()).reported();
    let desktop = Desktop::start(&events);
    let focused = true;
    let mut app = App {
        right,
        homes: home::Homes::default(),
        shown_number_dropped: 0,
        runs_in,
        projected: Projected::default(),
        lunetad,
        movers: HashMap::new(),
        sidebar,
        areas,
        width,
        held: Held::Nothing,
        last_press: LastPress::Elsewhere,
        focus,
        reported,
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
        copies: Copies::default(),
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
        app.go_home(&Machine::Local, root);
        app.give(Focus::on(Side::Session));
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

fn caught<T>(panicked: &str, asked: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(asked)).unwrap_or_else(|_| Err(panicked.to_owned()))
}

fn gone(projected: &Projected, home: &Home) -> bool {
    home.known() && projected.lacks(home.machine(), home.project())
}

fn fetch_work(lunetad: &Path, events: &Sender<Event>, home: &Home) {
    assert!(home.fetching(), "a home is fetched for only once it asks, and so once at a time");
    let (number, on, project) = (home.number(), home.machine().clone(), home.project());
    let (lunetad, events) = (lunetad.to_owned(), events.clone());
    thread::spawn(move || {
        let listed = caught(WORK_PANICKED, || worktrees::work(&lunetad, &on, project));
        let _ = events.send(Event::Work { number, listed });
    });
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
                let unsure = |_| Err(Unchanged::InDoubt(MOVE_PANICKED.to_owned()));
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
                if let Some(right) = self.viewed_mut().filter(|it| it.number() == number) {
                    let heeded = right.heard(heard);
                    self.heeded(heeded);
                }
            },
            Event::Seen { number, watcher, seen } => {
                if let Some(right) = self.viewed_mut().filter(|it| it.number() == number) {
                    let heeded = right.seen(watcher, seen);
                    self.heeded(heeded);
                }
            },
            Event::Work { number, listed } => {
                if let Some(home) = self.homes.iter_mut().find(|it| it.number() == number) {
                    home.listed(listed, Instant::now());
                }
            },
            Event::Reached { claude_folder } => self.claude_folder = claude_folder,
            Event::Daemon(Ok(host)) => {
                self.projected = Projected::adapted(&host).unwrap_or_else(|why| {
                    self.sidebar.notify(format!("lunetad's projects: {why}"));
                    Projected::default()
                });
                if let Some(Right::Worktree(right)) = &mut self.right {
                    let (tabs, titles) = self.projected.tabs(right.machine(), right.worktree());
                    right.tabs(tabs, titles);
                }
                if let Some(Homed::Gone { on }) = self.name_home() {
                    self.go_other_than(&on, None);
                }
                let projected = &self.projected;
                self.homes.forget(|home| gone(projected, home));
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
            Event::Unclosed { on, act, ticket, unchanged } => {
                self.sidebar.unclosed(&on, &act, ticket, unchanged);
            },
            Event::Moved { ticket, kept } => self.sidebar.moved(ticket, kept),
            Event::Unmoved { on, ticket, order, unmoved } => {
                let note = unmoved.said("not moved", "the rows may have moved");
                self.sidebar.unmoved(&on, ticket, &order, note);
            },
            Event::Copy(clip) => self.clip(terminal, clip)?,
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
            Event::Rebased { menu, rebased } => {
                let asked = self.sidebar.rebased(menu, rebased);
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
                } else if let Some(ask) = self.homes.iter_mut().find_map(|home| home.ask_numbered(number)) {
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
                    Ok(filed) => self.clip(terminal, Clip::Filed(filed))?,
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
            Event::ReadAloud { from, selected } => {
                if let Err(aloud::NothingToRead) = self.aloud.key(Some(&selected)) {
                    self.desktop.nothing_to_read(from);
                }
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
            Some(Over::Picker(_)) | None => false,
        };
        let home = self.home().is_some_and(Home::turning);
        over || home || self.sidebar.turning()
    }

    fn due(&self) -> Option<Instant> {
        let picker = match &self.focus.over {
            Some(Over::Picker(picker)) => picker.due(),
            _ => None,
        };
        let shows = self.sidebar.due(SystemTime::now()).map(|left| Instant::now() + left);
        let home = self.home().and_then(Home::due);
        picker.into_iter().chain(self.alerts.due()).chain(shows).chain(home).min()
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
        self.go(&key.on, &Place::View(place));
        self.leave();
    }

    fn ticked(&mut self, now: Instant) {
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.ticked(now);
        }
        self.ask_picker();
        match &mut self.right {
            Some(Right::Worktree(right)) => right.ticked(),
            Some(Right::Home(_)) => self.ask_work(now),
            None => {},
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
        self.tell();
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
                Some(Over::Brief(_) | Over::Dictation(..)) => {},
                None => match self.holder() {
                    Holder::Sidebar(_) => self.sidebar.paste(&text),
                    Holder::Terminal => self.paste(&text),
                    Holder::Box(_) => unreachable!("a box takes the paste"),
                },
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
                    Keyed::Close | Keyed::Interrupt => self.give(Focus::on(self.focus.side)),
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
            Some(Over::Dictation(..)) => return,
            None => {},
        }
        match self.sidebar.floating() {
            Some(Floating::Menu(_)) => {
                if chord == MENU {
                    self.sidebar.close_menu();
                } else {
                    let asked = self.sidebar.key(key, SystemTime::now());
                    self.follow(asked);
                }
                return;
            },
            Some(Floating::Note(_)) => {
                if key.kind == KeyEventKind::Repeat {
                    return;
                }
                self.sidebar.close_note();
                if chord != MENU {
                    return;
                }
            },
            None => {},
        }
        if self.chord(chord) {
            return;
        }
        match self.holder() {
            Holder::Terminal if !self.sidebar.pressed() => {
                if chord == images::KEY && self.key_for_image(key) {
                    return;
                }
                self.key_to_session(key);
            },
            Holder::Terminal | Holder::Sidebar(_) => {
                let asked = self.sidebar.key(key, SystemTime::now());
                self.follow(asked);
            },
            Holder::Box(_) => unreachable!("a box takes the key"),
        }
    }

    fn chord(&mut self, chord: Chord) -> bool {
        if chord == MENU {
            self.let_go();
            if matches!(&self.right, Some(Right::Home(_))) {
                self.sidebar.float_note(HOME_MENU.to_owned());
            } else if self.viewed().is_some_and(Viewed::welcomes) {
                self.sidebar.float_note(WELCOME_MENU.to_owned());
            } else {
                self.sidebar.float_menu();
            }
            return true;
        }
        if let Some(list) = List::opened_by(chord) {
            self.pickers += 1;
            let aim = match (list, self.focus.side) {
                (List::Dirs, Side::Sidebar) => self.sidebar.on_header(),
                _ => None,
            };
            let aim = aim.unwrap_or_default();
            let (listing, palette) = (self.sidebar.listing(), self.palette());
            let picker = Box::new(Picker::open(self.pickers, list, listing, aim, palette));
            self.give(Focus { side: self.focus.side, over: Some(Over::Picker(picker)) });
            return true;
        }
        if chord == SWAP {
            let on_terminal = matches!(self.holder(), Holder::Terminal);
            self.give(Focus::on(if on_terminal { Side::Sidebar } else { Side::Session }));
            return true;
        }
        if chord == COLLAPSE {
            let collapsed = self.focus.side == Side::Collapsed;
            self.give(Focus::on(if collapsed { Side::Session } else { Side::Collapsed }));
            return true;
        }
        if let Some(direction) = stepped(chord) {
            self.step(direction);
            return true;
        }
        if let Some(change) = resized(chord) {
            self.leave();
            self.ask_right(lunetad::Asked::Resize(change));
            return true;
        }
        false
    }

    fn palette(&self) -> Palette {
        let welcomes = self.viewed().is_some_and(Viewed::welcomes);
        let (aim, origin) = match self.focus.side {
            Side::Sidebar => (self.sidebar.aimed_row(), Origin::Sidebar),
            Side::Session | Side::Collapsed if welcomes => {
                (self.sidebar.aimed_worktree(), Origin::Views)
            },
            Side::Session | Side::Collapsed => (self.sidebar.aimed_view(), Origin::Views),
        };
        Palette { aim, origin }
    }

    fn key_to_session(&mut self, key: &KeyEvent) {
        match &mut self.right {
            Some(Right::Worktree(right)) => right.key(key, self.inputs),
            Some(Right::Home(shown)) => self.homes.shown_mut(shown).key(key, &mut self.asks, &self.events),
            None => {},
        }
    }

    fn paste(&mut self, text: &str) {
        match &mut self.right {
            Some(Right::Worktree(right)) => right.paste(text),
            Some(Right::Home(shown)) => self.homes.shown_mut(shown).paste(text),
            None => {},
        }
    }

    fn tell_focus(&mut self, has: bool) {
        if let Some(right) = self.viewed_mut() {
            right.focus(has);
        }
    }

    fn ask_right(&self, asked: lunetad::Asked) {
        if let Some(right) = self.viewed() {
            right.ask(asked);
        }
    }

    fn viewed(&self) -> Option<&Viewed> {
        match &self.right {
            Some(Right::Worktree(right)) => Some(right),
            Some(Right::Home(_)) | None => None,
        }
    }

    fn viewed_mut(&mut self) -> Option<&mut Viewed> {
        match &mut self.right {
            Some(Right::Worktree(right)) => Some(right),
            Some(Right::Home(_)) | None => None,
        }
    }

    fn home(&self) -> Option<&Home> {
        match &self.right {
            Some(Right::Home(shown)) => Some(self.homes.shown(shown)),
            Some(Right::Worktree(_)) | None => None,
        }
    }

    fn ask_work(&mut self, now: Instant) {
        let Some(Right::Home(shown)) = &self.right else {
            return;
        };
        let home = self.homes.shown_mut(shown);
        if home.relist(now) {
            fetch_work(&self.lunetad, &self.events, home);
        }
    }

    fn go_home(&mut self, on: &Machine, project: ProjectId) {
        let key = home::Key { on: on.clone(), project };
        if matches!(&self.right, Some(Right::Home(shown)) if *shown == key) {
            return;
        }
        self.drop_shown();
        let (lunetad, events, asks) = (&self.lunetad, &self.events, &mut self.asks);
        let dropped = self.homes.show(&key, |number| {
            *asks += 1;
            let home = Home::new(number, *asks, key.on.clone(), key.project);
            fetch_work(lunetad, events, &home);
            home
        });
        if let Some(dropped) = dropped {
            self.dropped_home(&dropped);
        }
        self.right = Some(Right::Home(key));
        let homed = self.name_home();
        let fresh = matches!(homed, Some(Homed::Named | Homed::Unknown));
        assert!(fresh, "a home just shown was made now or held while its project was: {homed:?}");
        self.seat();
    }

    #[must_use]
    fn name_home(&mut self) -> Option<Homed> {
        let Some(Right::Home(shown)) = &self.right else {
            return None;
        };
        let home = self.homes.shown_mut(shown);
        match self.projected.project(&shown.on, shown.project) {
            Some(project) => {
                home.named(&project.name, &project.dir);
                Some(Homed::Named)
            },
            None if gone(&self.projected, home) => Some(Homed::Gone { on: shown.on.clone() }),
            None => Some(Homed::Unknown),
        }
    }

    fn drop_shown(&mut self) {
        if let Some(Right::Worktree(right)) = self.right.take() {
            let number = right.number();
            let dropped = self.shown_number_dropped;
            assert!(number > dropped, "a shown number grows past the last dropped");
            self.shown_number_dropped = number;
        }
        assert!(self.right.is_none(), "luneta shows nothing on the right once it dropped it");
    }

    fn dropped_home(&mut self, dropped: &Home) {
        if dropped.turning() {
            let project = self.projected.project(dropped.machine(), dropped.project());
            let name = project.map_or("a project", |project| project.name.as_str());
            let most = home::KEPT_MAX;
            self.sidebar.notify(format!("luneta keeps {most} homes at most: {name}'s went, and its turn's agent ended"));
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

    fn clip(&mut self, terminal: &mut DefaultTerminal, clip: Clip) -> io::Result<()> {
        if let Some(copied) = self.copies.clip(terminal.backend_mut(), clip)? {
            self.sidebar.copied(copied, SystemTime::now());
        }
        Ok(())
    }

    fn image_went(&mut self, went: Went) {
        self.image.heard();
        self.sidebar.image_going(None);
        match went {
            Went::NoImage { key, on } => {
                let on_terminal = matches!(self.holder(), Holder::Terminal);
                if on_terminal && self.sidebar.far_agent() == Some(&on) {
                    self.key_to_session(&key);
                }
            },
            Went::Kept { on, path } => {
                if matches!(self.sidebar.on(), Machine::Remote(there) if *there == on) {
                    self.paste(&path);
                } else {
                    self.image_not_pasted(&format!("luneta left {on}, where it is at {path}"));
                }
            },
            Went::Failed(why) => self.image_not_pasted(&why),
        }
    }

    fn image_not_pasted(&mut self, why: &str) {
        let note = format!("image not pasted: {why}");
        match (self.holder(), self.focus.side) {
            (Holder::Terminal, Side::Collapsed) => {
                log::say(&note);
                self.sidebar.float_note(note);
            },
            (Holder::Terminal, Side::Session | Side::Sidebar)
            | (Holder::Sidebar(_) | Holder::Box(_), _) => self.sidebar.notify(note),
        }
    }

    fn mouse(&mut self, mouse: MouseEvent) {
        let at = Position::new(mouse.column, mouse.row);
        self.pointer = Some(at);
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        let under = matches!(self.held, Held::Session | Held::Selecting);
        if self.covered() && under && !down {
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
            None => {},
        }
        match self.sidebar.floating() {
            Some(Floating::Menu(_)) => {
                if down && off {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_menu();
                }
                return;
            },
            Some(Floating::Note(_)) => {
                if down {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_note();
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
            Hit::Menu(menu, choice) if self.focus.over.is_none() => {
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
                let asked = self.sidebar.number_clicked(link);
                self.follow(asked);
            },
            Hit::Menu(..) => {},
            Hit::Aloud(button) => self.aloud.press(button),
        }
    }

    fn session_mouse(&mut self, mouse: MouseEvent) {
        let at = self.session_cell(mouse);
        let width = self.areas.1.width;
        let Some(right) = self.viewed_mut() else {
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
        let Some(Right::Worktree(own)) = &mut self.right else {
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
                if let Some(excerpt) = own.selected() {
                    let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
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
                let _ = self.events.send(Event::Copy(Clip::Pull(link)));
            },
            Asked::CopyText(excerpt) => {
                let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
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
            Asked::Delete { on, act, ticket } => self.project_act(on, act, ticket),
            Asked::Open { asking, on, project, beside, command } => {
                self.project_open(asking, &on, project, beside, command);
            },
            Asked::Welcome { asking, on, session, view } => {
                self.welcome(asking, &on, &session, view);
            },
            Asked::OpenIn { asking, on, worktree, command } => {
                self.go_worktree(&on, worktree, None);
                self.shown().open_beside_focused(command);
                self.give_opened(Opener::of(asking));
            },
            Asked::NewWorktree { on, project, branch } => {
                self.make_worktree(on, move |lunetad, on| {
                    worktrees::branched(lunetad, on, project, &branch).map(Landing::Worktree)
                });
                self.leave();
            },
            Asked::AddProject { on, folder, name } => {
                if self.refuse_naming(None, |busy| format!("menu: {busy}")) {
                    return;
                }
                self.naming_on(move || {
                    let made = || worktrees::project_made(&on, &folder, name.as_ref());
                    let made = caught(NAMING_PANICKED, made).map_err(|why| format!("menu: {why}"));
                    Made::Project { on, made }
                });
                self.leave();
            },
            Asked::Rename { on, rename } => {
                self.change(on, RENAME_PANICKED, move |on| {
                    let (renamed, noun) = match &rename {
                        Rename::Terminal(terminal, to) => {
                            (worktrees::renamed_terminal(on, *terminal, to.as_ref()), "view")
                        },
                        Rename::Worktree(worktree, to) => {
                            (worktrees::renamed_worktree(on, *worktree, to.as_ref()), "worktree")
                        },
                        Rename::Project(project, to) => {
                            (worktrees::renamed_project(on, *project, to), "project")
                        },
                    };
                    let in_doubt = format!("the {noun} may be renamed");
                    renamed.map_err(|unchanged| unchanged.said("not renamed", &in_doubt))
                });
            },
            Asked::Make { menu, on, cwd, name, command } => {
                let busy = |busy: &str| format!("menu: {}", worktrees::unopened(&cwd, &name, busy));
                if self.refuse_naming(Some(menu), busy) {
                    return;
                }
                let lunetad = self.lunetad.clone();
                self.naming_on(move || {
                    let made = caught(NAMING_PANICKED, || {
                        let made = worktrees::pulled(&lunetad, &on, &cwd, &name, command.clone())?;
                        Ok(Pulled { on, worktree: made, argv: command })
                    });
                    Made::Pulled { menu, made }
                });
            },
            Asked::Pull { menu, on, work, wanted, worked, others, held_here } => {
                let asking = pull::Asking { on, work, wanted, worked, others, held_here };
                pull::prepare(menu, asking, &self.events);
            },
            Asked::Prunes { menu, on, dir } => prune::plan(menu, on, dir, &self.events),
            Asked::Rebase { menu, on, dir, branch } => {
                let here = on == Machine::Local;
                let rebasing = rebase::start(menu, on, dir, branch, &self.events);
                if here {
                    self.keep_change(rebasing);
                }
            },
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
            Asked::Printed { menu, on, view, ended, .. } => {
                printed::read(menu, projected::route(&on, view), ended, &self.events);
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
        panicked: &'static str,
        change: impl FnOnce(&Machine) -> Result<(), String> + Send + 'static,
    ) {
        let events = self.events.clone();
        self.keep_change(thread::spawn(move || {
            if let Err(why) = caught(panicked, || change(&on)) {
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

    fn project_act(&mut self, on: Machine, act: Act, ticket: u64) {
        let (lunetad, events) = (self.lunetad.clone(), self.events.clone());
        self.keep_change(thread::spawn(move || {
            let asked = AssertUnwindSafe(|| match &act {
                Act::View { view: ViewKey::Terminal(key), .. } => {
                    worktrees::closed_view(&lunetad, &on, *key)
                },
                Act::Worktree { worktree, seen, .. } => {
                    worktrees::closed(&lunetad, &on, *worktree, seen)
                },
                Act::Project { project, worktrees, seen, .. } => {
                    worktrees::closed_project(&lunetad, &on, *project, worktrees, seen)
                },
            });
            let unsure = |_| Err(Unchanged::InDoubt(CLOSE_PANICKED.to_owned()));
            if let Err(unchanged) = panic::catch_unwind(asked).unwrap_or_else(unsure) {
                let _ = events.send(Event::Unclosed { on, act, ticket, unchanged });
            }
        }));
    }

    fn welcome(&mut self, asking: sidebar::Held, on: &Machine, session: &Listed, view: ViewKey) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| project.view_of(view.key()));
        let Some((worktree, terminal)) = held else {
            return self.sidebar.notify(format!("menu: lunetad lists no view {view}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().welcome();
        self.give_opened(Opener::of(asking));
    }

    fn project_open(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        project: ProjectId,
        beside: ViewKey,
        command: Vec<String>,
    ) {
        let Some(project) = self.projected.project(on, project) else {
            return self.sidebar.notify("menu: lunetad keeps no such project".to_owned());
        };
        let Some((worktree, terminal)) = project.view_of(beside.key()) else {
            return self.sidebar.notify(format!("menu: lunetad lists no view {beside}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().open_beside_focused(command);
        self.give_opened(Opener::of(asking));
    }

    fn open_worktree(&mut self, on: Machine, project: ProjectId) {
        self.pickers += 1;
        let (listing, palette) = (self.sidebar.listing(), self.palette());
        let picker = Box::new(Picker::opening(self.pickers, listing, (on, project), palette));
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
            Picked::Add { on, project, folder } => {
                let project = project.id();
                self.make_worktree(on, move |lunetad, on| {
                    let added = worktrees::add_worktree(lunetad, on, (project, folder.as_str()));
                    added.map(Landing::Worktree)
                });
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Make { on, session, cwd } => {
                self.make_worktree(on, move |lunetad, on| {
                    worktrees::made_at(lunetad, on, &cwd, Some(session.as_str()))
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
            Picked::Delete { on, act } => {
                let ticket = self.sidebar.ticket();
                self.project_act(on, act, ticket);
                self.give(Focus::on(self.focus.side.session()));
            },
            Picked::Chord(chord) => {
                self.give(Focus::on(self.focus.side));
                match chord {
                    aloud::KEY => self.read_aloud(),
                    chord if aloud::stops(chord) => self.stop_aloud(),
                    chord => {
                        let ran = self.chord(chord);
                        assert!(ran, "every chord the palette lists is one App::chord runs");
                    },
                }
            },
            Picked::Menu { target, origin, action } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.act_in_menu(&target, action, origin);
                self.follow(asked);
            },
            Picked::Row { target, action } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.act_on_row(&target, action, SystemTime::now());
                self.follow(asked);
            },
            Picked::Done { done, origin } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.chained(done, origin);
                self.follow(asked);
            },
        }
    }

    fn converse(&mut self) {
        if let Some(Over::Ask(ask)) = &mut self.focus.over {
            ask.send(Marked::default(), &self.events);
        }
    }

    fn file_issue(&mut self) {
        let open = self.focus.over.take_if(|over| matches!(over, Over::Issue(_)));
        let Some(Over::Issue(issue)) = open else {
            return;
        };
        if let Some((number, to, filing)) = issue.send(&self.events, &mut self.unfiled) {
            self.sidebar.filing(number, &to.to_string());
            self.keep_change(filing);
        }
        self.give(Focus::on(self.focus.side));
    }

    fn close_issue(&mut self) {
        let open = self.focus.over.take_if(|over| matches!(over, Over::Issue(_)));
        if let Some(Over::Issue(issue)) = open {
            issue.close(&mut self.unfiled);
        }
        self.give(Focus::on(self.focus.side));
    }

    fn dictate(&mut self) -> bool {
        let on_session = matches!(self.holder(), Holder::Terminal);
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
        self.give(Focus { side: self.focus.side, over });
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
                if let Err(nothing) = self.aloud.key(self.copies.excerpt()) {
                    self.sidebar.notify(nothing.to_string());
                }
            },
            Reader::Another(holder) => holder.selected(self.copies.excerpt().unwrap_or_default()),
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

    fn undictate(&mut self, dictated: Option<Dictated>) {
        let open = self.focus.over.take_if(|over| matches!(over, Over::Dictation(..)));
        let Some(Over::Dictation(_, under)) = open else {
            return;
        };
        let side = self.focus.side;
        match under {
            Where::Session(agent) => {
                self.give(Focus::on(side));
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
                self.give(Focus { side, over: Some(Over::Ask(ask)) });
                if dictated.is_some_and(|dictated| dictated.sends) {
                    self.converse();
                }
            },
            Where::Issue(mut issue) => {
                if let Some(dictated) = &dictated {
                    issue.dictated(&dictated.written.to_string());
                }
                let sends = dictated.is_some_and(|dictated| dictated.sends) && issue.ready();
                self.give(Focus { side, over: Some(Over::Issue(issue)) });
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
        let Some(right) = self.viewed() else {
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
        let Some(project) = self.projected.project(machine, listed.project()) else {
            if let Machine::Remote(destination) = machine
                && !self.projected.reaches(machine)
            {
                return Err(format!("lunetad does not reach {destination} now"));
            }
            return Err(format!("{session} is no project lunetad keeps"));
        };
        let shows = |worktree| {
            self.viewed().is_some_and(|right| {
                right.machine() == machine && right.worktree() == worktree
            })
        };
        match place {
            Place::Session(_) => self.go_home(machine, project.id),
            Place::Worktree(place) => {
                let held = project.worktrees().iter().any(|it| it.id == place.worktree);
                if !held {
                    return Err(format!("{session} holds no such worktree"));
                }
                if !shows(place.worktree) {
                    self.go_worktree(machine, place.worktree, None);
                }
            },
            Place::View(ViewPlace { view: ViewKey::Terminal(terminal_key), .. }) => {
                let (worktree, terminal) = project.view_of(*terminal_key).ok_or_else(|| {
                    format!("lunetad lists no view {} in {session}", terminal_key.id)
                })?;
                self.go_worktree(machine, worktree, Some(terminal));
            },
        }
        Ok(())
    }

    fn go_worktree(
        &mut self,
        on: &Machine,
        worktree: WorktreeId,
        terminal: Option<ViewId>,
    ) {
        if let Some(right) = self.viewed_mut() {
            right.go(on, worktree);
        } else {
            self.drop_shown();
            let (size, greeting) = (layout_size(self.areas.1), greeting(self.colours));
            let (folder, events) = (self.lunetad.clone(), &self.events);
            let (number, on) = (self.shown_number_dropped + 1, on.clone());
            let runs_in = self.runs_in;
            let started =
                Viewed::start(number, folder, on, worktree, size, greeting, runs_in, events);
            self.right = Some(Right::Worktree(started));
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
        self.viewed_mut().expect("luneta went to a worktree")
    }

    fn leave_view(&mut self) {
        let (on, left) = match &self.right {
            Some(Right::Worktree(right)) => (right.machine().clone(), Some(right.worktree())),
            Some(Right::Home(shown)) => (shown.on.clone(), None),
            None => return,
        };
        self.go_other_than(&on, left);
    }

    fn go_other_than(&mut self, on: &Machine, left: Option<WorktreeId>) {
        let held = |machine: &Machine| {
            let projects = self.projected.projects(machine).iter();
            let worktrees = projects.flat_map(projected::Project::worktrees);
            let open = worktrees.filter(|it| !it.terminals.is_empty());
            let others = open.filter(|it| machine != on || Some(it.id) != left);
            projects::last_used(others.map(|it| (it.id, it.used)))
        };
        let other = held(on).map(|id| (on.clone(), id));
        let other = other.or_else(|| held(&Machine::Local).map(|id| (Machine::Local, id)));
        match other {
            Some((machine, other)) => self.go_worktree(&machine, other, None),
            None => self.unview(),
        }
    }

    fn unview(&mut self) {
        self.drop_shown();
        self.sidebar.unseat();
        self.give(Focus::on(Side::Sidebar));
        assert_eq!(self.focus.side, Side::Sidebar, "with no worktree shown, no keys go to it");
    }

    fn seat(&mut self) {
        let right = match &self.right {
            None => return,
            Some(Right::Home(shown)) => {
                let (on, project) = (shown.on.clone(), shown.project);
                if self.projected.project(&on, project).is_some() {
                    self.sidebar.sit(Seat { on, project, at: Sitting::Home });
                } else {
                    self.sidebar.unseat();
                }
                return;
            },
            Some(Right::Worktree(right)) => right,
        };
        let (on, worktree) = (right.machine().clone(), right.worktree());
        let Some((project, _)) = self.projected.holding(&on, worktree) else {
            return;
        };
        let terminal = right.focused().and_then(|focused| project.terminal_key(focused));
        let at = Sitting::Worktree { worktree, terminal };
        self.sidebar.sit(Seat { on, project: project.id, at });
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
                let Some(right) = self.viewed() else {
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
        made: impl FnOnce(&Path, &Machine) -> Result<Landing, String> + Send + 'static,
    ) {
        if self.refuse_naming(None, |busy| format!("menu: {busy}")) {
            return;
        }
        let lunetad = self.lunetad.clone();
        self.naming_on(move || {
            let made = caught(NAMING_PANICKED, || made(&lunetad, &on));
            Made::Worktree { on, made, argv: Vec::new() }
        });
    }

    fn refuse_naming(&mut self, menu: Option<u64>, said: impl FnOnce(&str) -> String) -> bool {
        if self.naming == Naming::Idle {
            return false;
        }
        self.sidebar.notify(said(NAMING));
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
            Made::Project { on, made: Ok(project) }
            | Made::Worktree { on, made: Ok(Landing::Unopened(project)), .. } => {
                self.go_home(&on, project);
                self.give_opened(Opener::LetGo);
            },
            Made::Project { made: Err(why), .. } | Made::Worktree { made: Err(why), .. } => {
                self.sidebar.notify(why);
            },
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
            Err(why) => return self.sidebar.notify(format!("menu: {why}")),
        };
        let Pulled { on, worktree, argv } = made;
        self.go_opening(&on, worktree, argv);
        self.give_opened(opener);
    }

    fn give_opened(&mut self, opener: Opener) {
        assert!(self.right.is_some(), "luneta shows the view it opened");
        match (opener, self.holder()) {
            (Opener::Holds, _) => {
                self.leave();
                let afloat = matches!(self.holder(), Holder::Sidebar(Holding::Afloat));
                assert!(!afloat, "the menu that opened the view holds the keys no longer");
            },
            (Opener::LetGo, Holder::Terminal | Holder::Sidebar(Holding::Nothing)) => {
                self.give(Focus::on(self.focus.side.session()));
                let on_terminal = matches!(self.holder(), Holder::Terminal);
                assert!(on_terminal, "the opened view takes the keys");
            },
            (
                Opener::LetGo,
                Holder::Sidebar(
                    Holding::Chord
                    | Holding::Search
                    | Holding::Reorder
                    | Holding::Menu
                    | Holding::Afloat
                    | Holding::Press,
                )
                | Holder::Box(_),
            ) => {},
        }
    }

    fn holder(&self) -> Holder<'_> {
        Holder::of(&self.focus, self.sidebar.holding())
    }

    fn covered(&self) -> bool {
        match self.holder() {
            Holder::Box(_) | Holder::Sidebar(Holding::Afloat) => true,
            Holder::Terminal
            | Holder::Sidebar(
                Holding::Nothing
                | Holding::Chord
                | Holding::Search
                | Holding::Reorder
                | Holding::Menu
                | Holding::Press,
            ) => false,
        }
    }

    fn tell(&mut self) {
        let reported = self.holder().reported();
        if reported == self.reported {
            return;
        }
        self.reported = reported;
        if self.focused {
            self.tell_focus(reported == Reported::Focused);
        }
    }

    fn give(&mut self, focus: Focus) {
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
        self.sidebar.focus(focus.keys());
        assert!(self.sidebar.floating().is_none(), "a focus change closes what floats");
        self.focus = focus;
        self.hold_aloud();
        self.tell();
    }

    fn leave(&mut self) {
        let leaves = match self.holder() {
            Holder::Sidebar(_) => true,
            Holder::Terminal | Holder::Box(_) => false,
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
        if self.reported == Reported::Focused {
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
            if let Some(right) = self.viewed_mut() {
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
            let covered = self.covered();
            let uncovered = |at: &Position| !covered && right.contains(*at);
            let spot = |at: Position| attached::Spot { row: at.y - right.y, column: at.x - right.x };
            let hover = self.pointer.filter(uncovered).map(spot);
            let cursor = match &self.right {
                Some(Right::Worktree(shown)) => shown.draw(buffer, right, hover),
                Some(Right::Home(shown)) => {
                    self.homes.shown(shown).draw(buffer, right, now);
                    None
                },
                None => {
                    draw_unshown(buffer, right);
                    None
                },
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
                None => match self.sidebar.floating() {
                    Some(Floating::Menu(menu)) => menu.draw(buffer, right, now),
                    Some(Floating::Note(note)) => {
                        menu::draw_note(buffer, right, note);
                        Drawn::Nothing
                    },
                    None => beside,
                },
            };
            if let Some(pointer) = self.pointer {
                drawn.light(buffer, pointer);
                self.sidebar.aloud_buttons().light(buffer, pointer);
            }
            if matches!(self.holder(), Holder::Terminal)
                && let Some(cursor) = cursor
            {
                frame.set_cursor_position(cursor);
            }
            self.drawn = drawn;
        })?;
        let cursor = self.viewed().map_or(UNSHOWN_CURSOR, Viewed::cursor_style);
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
    fn a_home_is_gone_only_from_a_machine_reached_whose_projects_read_and_hold_it_no_more() {
        use lunetad::api::projects::{Held, Kept};
        let host = |held: Held| lunetad::api::Host {
            projects: Some(lunetad::api::Projects { held: Some(held) }),
            ..lunetad::api::Host::default()
        };
        let none = Held::Kept(Kept { projects: Vec::new(), version: None });
        let projected = Projected::adapted(&host(none)).unwrap();
        let project = ProjectId::of(3).unwrap();
        let named = |on: Machine| {
            let mut home = Home::new(1, 1, on, project);
            home.named("luneta", &lunetad::projects::Dir::parse("/w/luneta").unwrap());
            home
        };
        assert!(gone(&projected, &named(Machine::Local)));
        let far = Machine::Remote("nixos".parse().unwrap());
        assert!(!gone(&projected, &named(far)), "a machine not reached just now keeps its home");
        let unnamed = Home::new(1, 1, Machine::Local, project);
        assert!(!gone(&projected, &unnamed), "a home never named has not gone");
        let unkept = host(Held::Unkept("it changed since lunetad read it".to_owned()));
        let unread = [
            Projected::default(),
            Projected::adapted(&unkept).unwrap(),
            Projected::adapted(&lunetad::api::Host::default()).unwrap(),
        ];
        for unread in unread {
            assert!(!gone(&unread, &named(Machine::Local)), "no home is gone while lunetad's projects are unread");
        }
    }

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
