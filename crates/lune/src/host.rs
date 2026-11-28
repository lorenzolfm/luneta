use std::collections::{HashMap, VecDeque};
use std::env;
use std::io::{self, Write as _};
use std::iter;
use std::mem;
use std::num::{NonZeroU16, NonZeroU32, NonZeroU64};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
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
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{self, Arranged, BrowserUrl, Opens, ProjectId, ViewId, WorktreeId};
use lune_domain::rows;
use luned::api::{Host, Version};
use luned::asks::Marked;
use luned::changes::Unchanged;
use luned::issues::{Filed, OnGitHub, Remote, Unfiled};
use luned::log::{self, Process};
use luned::mux::own::viewer::{Greeting, Seen};
use luned::prunes;
use luned::rebases::{Rebased, Unrebased};
use luned::work;
use luned::{Destination, Direction, Growth};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::{DefaultTerminal, Frame, Terminal};

use crate::actions::{COLLAPSE, Chord, DOWN, GROW, LEFT, MENU, RIGHT, SHRINK, SWAP, UP};
use crate::agents::{self, Kept};
use crate::alerts::{self, Alerts};
use crate::aloud::{self, Aloud};
use crate::answers::{
    self, Answer, CellPixels, Colours, DisableThemeChange, EnableThemeChange, WindowPixels,
};
use crate::ask::{self, Ask, Keyed};
use crate::attached;
use crate::brief::{self, Asking, Briefing, Written};
use crate::browser::Browser;
use crate::cadence::Cadence;
use crate::checks;
use crate::clock::Moment;
use crate::close::Act;
use crate::copied::{Clip, Copies, Excerpt};
use crate::desktop::{self, Desktop, Reader, Told};
use crate::dictation::{self, Dictated, Dictation};
use crate::embedded;
use crate::folds::{self, Folds};
use crate::home::{self, Home};
use crate::images::{self, Went};
use crate::input;
use crate::issue::{self, Issue};
use crate::machines::{self, Added, Machine};
use crate::menu::{self, Choice, Origin, Rename};
use crate::models::Fetched;
use crate::named;
use crate::news::{self, News};
use crate::peers;
use crate::picker::{self, List, Palette, Picked, Picker};
use crate::printed::{self, Printed};
use crate::projected::{self, Projected};
use crate::prune;
use crate::pull::{self, Found};
use crate::rebase;
use crate::review::{self, LoadedDiff, ReviewAction, RibbonCacheKey, RibbonOverlay};
use crate::sidebar::{
    self, Asked, Floating, HereAgent, Holding, Keys, Listed, Place, Reorder, Seat, SeatedLayout,
    Sidebar, SidebarView, Sitting, ViewKey,
};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Speech, Spoken};
use crate::spots::{Burst, Drawn, DrawnHover, Hit, Settling};
use crate::state;
use crate::viewed::{self, Across, Heeded, Viewed};
use crate::width::Width;
use crate::worktrees::{self, Arrival, Landing};

pub enum Made {
    Worktree {
        on: Machine,
        made: Result<Landing, String>,
    },
    Pulled {
        menu: u64,
        made: Result<Pulled, String>,
    },
}

pub struct Pulled {
    on: Machine,
    pulled: worktrees::Pulled,
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

const NAMING: &str = "lune names a session already: try again once it has made it";

const MAKE_PANICKED: &str = "lune could not ask luned for the worktree: asking panicked";

const PULL_PANICKED: &str = "lune could not ask luned for the pull: asking panicked";

const RENAME_PANICKED: &str = "lune could not ask luned for the rename: asking panicked";

const CLOSE_PANICKED: &str = "lune could not ask luned for the close: asking panicked";

const WORK_PANICKED: &str = "lune could not ask luned for the work: asking panicked";

const DIFF_PANICKED: &str = "lune could not ask luned for the changes: asking panicked";

const EXPLAIN_PROMPT: &str = "Explain why this is built this way.";

pub const NOT_YET: &str = "lune does not do that to a worktree yet";

const WELCOME_MENU: &str = "the menu is for terminals: d d closes this welcome view";

const BROWSER_MENU: &str =
    "Super k opens actions; Ctrl w closes the browser; Ctrl hjkl switches view";

const REVIEW_MENU: &str =
    "the menu is for terminals: d d closes this review view, e explains, ? asks";

const HOME_MENU: &str = "the menu is for terminals: the project's is on its row in the sidebar";

const UNSHOWN: [&str; 2] = [
    "No worktree to show yet.",
    "o w on a project's row opens one.",
];

const UNSHOWN_CURSOR: (CursorShape, bool) = (CursorShape::HollowBlock, false);

const ASKED_TO_QUIT: &str = "asked to quit";

const SLOW_END: Duration = Duration::from_secs(1);

const WAITS: &str = "ends once the changes it sent are done; Ctrl c ends it now";

pub enum Event {
    Input(Input),
    Answered(Answer),
    InputEnded(io::Error),
    Shown(u64, luned::Heard),
    Seen {
        number: u64,
        watcher: u64,
        seen: Seen,
    },
    Browser {
        number: u64,
        view: ViewId,
        heard: embedded::Heard,
    },
    Daemon(Result<Host, String>),
    Notice(String),
    Unclosed {
        on: Machine,
        act: Act,
        ticket: u64,
        unchanged: Unchanged,
    },
    Moved {
        ticket: u64,
        kept: Option<Version>,
    },
    Unmoved {
        on: Machine,
        ticket: u64,
        order: Reorder,
        unmoved: Unchanged,
    },
    Copy(Clip),
    Image(Went),
    ImageSlow(Destination),
    Named(Made),
    Skills {
        menu: u64,
        agent: agents::Harness,
        skills: Result<Vec<Skill>, String>,
    },
    Pulled {
        menu: u64,
        found: Result<Found, String>,
    },
    Prunes {
        menu: u64,
        plan: Result<prunes::Plan, String>,
    },
    Pruned {
        menu: u64,
        pruned: Result<prunes::Pruned, String>,
    },
    Printed {
        menu: u64,
        printed: Result<Printed, String>,
    },
    Checks {
        menu: u64,
        checks: Result<checks::Failing, String>,
    },
    Rebased {
        menu: u64,
        rebased: Result<Rebased, Unrebased>,
    },
    Briefed {
        number: u64,
        written: Written,
    },
    Heard {
        number: u64,
        heard: ask::Heard,
    },
    Remotes {
        number: u64,
        remotes: Result<Vec<Remote>, String>,
    },
    Filed {
        number: u64,
        repository: issue::Repository,
        to: OnGitHub,
        words: String,
        filed: Result<Filed, Unfiled>,
    },
    Picker {
        number: u64,
        heard: picker::Heard,
    },
    Spoken {
        number: u64,
        spoken: Spoken,
    },
    Nameable {
        number: u64,
        skills: Result<Vec<Skill>, String>,
    },
    Fetched(Fetched),
    Voiced {
        number: u64,
        ended: Result<(), String>,
    },
    VoiceFetched(Fetched),
    ReadAloud {
        from: u64,
        selected: String,
    },
    StopAloud,
    Folds,
    News,
    Agent,
    Work {
        number: u64,
        listed: Result<work::Listed, String>,
    },
    DiffLoaded {
        on: Machine,
        worktree: WorktreeId,
        revision: NonZeroU64,
        loaded: Result<Arc<LoadedDiff>, String>,
    },
    Alerted(alerts::Read),
    Clicked(alerts::Key),
    #[cfg(not(target_os = "macos"))]
    IssueClicked(desktop::Came),
    Peers(peers::Heard),
}

const TICK: Duration = Duration::from_secs(1);

const FRAME_INTERVAL: Duration = Duration::from_millis(16);

const _: () = assert!(FRAME_INTERVAL.as_millis() > 0);
const _: () = assert!(FRAME_INTERVAL.as_millis() < sidebar::SCROLL.as_millis());

const FRAME_EVENTS_MAX: usize = 256;

const GROWS: [Chord; 4] = [
    GROW,
    (KeyCode::Char('+'), KeyModifiers::ALT),
    (
        KeyCode::Char('='),
        KeyModifiers::ALT.union(KeyModifiers::SHIFT),
    ),
    (
        KeyCode::Char('+'),
        KeyModifiers::ALT.union(KeyModifiers::SHIFT),
    ),
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
            }
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
            }
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
    pub(crate) fn new(events: Receiver<Event>) -> Self {
        Inbox {
            events,
            aside: VecDeque::new(),
        }
    }

    pub fn answer(&mut self, deadline: Instant) -> Option<Answer> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(left).ok()? {
                Event::Answered(answer) => return Some(answer),
                ended @ Event::InputEnded(_) => {
                    self.aside.push_back(ended);
                    return None;
                }
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
        self.aside
            .pop_front()
            .or_else(|| self.events.try_recv().ok())
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "lune holds one right side, shown at a time"
)]
enum Right {
    Worktree(Viewed),
    Home(home::Key),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Homed {
    Named { changed: bool },
    Unknown,
    Gone { on: Machine },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Graphics {
    Plain,
    Kitty,
}

impl Graphics {
    fn of(supported: bool) -> Graphics {
        if supported {
            Graphics::Kitty
        } else {
            Graphics::Plain
        }
    }

    fn supported(self) -> bool {
        self == Graphics::Kitty
    }
}

const DEFAULT_CELL_PX: (NonZeroU16, NonZeroU16) = (
    NonZeroU16::new(8).expect("default cell width is positive"),
    NonZeroU16::new(16).expect("default cell height is positive"),
);

struct KittyRibbons {
    cell_px: (NonZeroU16, NonZeroU16),
    measured_area: Option<Rect>,
    pending: Vec<RibbonOverlay>,
    shown: Vec<(NonZeroU32, RibbonCacheKey)>,
}

impl KittyRibbons {
    fn new() -> KittyRibbons {
        KittyRibbons {
            cell_px: DEFAULT_CELL_PX,
            measured_area: None,
            pending: Vec::new(),
            shown: Vec::new(),
        }
    }

    fn cell_px(&mut self, frame_area: Rect) -> (NonZeroU16, NonZeroU16) {
        if self.measured_area != Some(frame_area) {
            self.measured_area = Some(frame_area);
            self.cell_px = answers::cell_px().unwrap_or(DEFAULT_CELL_PX);
        }
        self.cell_px
    }
}

enum Ribbons {
    Unsupported,
    Supported(KittyRibbons),
}

impl Ribbons {
    fn new(supported: bool) -> Ribbons {
        if supported {
            Ribbons::Supported(KittyRibbons::new())
        } else {
            Ribbons::Unsupported
        }
    }

    fn enable(&mut self) {
        if matches!(self, Ribbons::Unsupported) {
            *self = Ribbons::Supported(KittyRibbons::new());
        }
    }

    fn cell_px(&mut self, frame_area: Rect, covered: bool) -> Option<(NonZeroU16, NonZeroU16)> {
        if covered {
            return None;
        }
        match self {
            Ribbons::Unsupported => None,
            Ribbons::Supported(kitty) => Some(kitty.cell_px(frame_area)),
        }
    }

    fn set_pending(&mut self, overlays: Vec<RibbonOverlay>) {
        if let Ribbons::Supported(kitty) = self {
            kitty.pending = overlays;
        }
    }
}

enum FetchFlight {
    Idle,
    InFlight {
        on: Machine,
        worktree: WorktreeId,
        revision: NonZeroU64,
        refetch: bool,
    },
}

struct DiffFetch {
    last_revision: Option<NonZeroU64>,
    flight: FetchFlight,
    git_status: Option<(Machine, WorktreeId, luned::api::GitStatus)>,
}

impl DiffFetch {
    fn new() -> DiffFetch {
        DiffFetch {
            last_revision: None,
            flight: FetchFlight::Idle,
            git_status: None,
        }
    }

    fn next_revision(&mut self) -> NonZeroU64 {
        let next = match self.last_revision {
            None => NonZeroU64::MIN,
            Some(prev) => prev.checked_add(1).expect("diff revisions do not wrap"),
        };
        self.last_revision = Some(next);
        next
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Hover {
    link: Option<attached::Selection>,
    pointer_shape: Option<embedded::PointerShape>,
    drawn: DrawnHover,
    aloud: Option<Rect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageKey {
    Unhandled,
    Handled { dirty: bool },
}

#[derive(Debug)]
struct Painted {
    at: Moment,
    cadence: Cadence,
    sidebar_view: SidebarView,
}

#[derive(Debug, Default)]
enum Pacing {
    #[default]
    Unpainted,
    Drawn {
        dirty: bool,
        painted: Painted,
    },
}

impl Pacing {
    fn mark_dirty(&mut self) {
        if let Pacing::Drawn { dirty, .. } = self {
            *dirty = true;
        }
    }

    fn clean(&self) -> Option<&Painted> {
        match self {
            Pacing::Drawn {
                dirty: false,
                painted,
            } => Some(painted),
            Pacing::Unpainted | Pacing::Drawn { dirty: true, .. } => None,
        }
    }
}

pub struct App {
    right: Option<Right>,
    homes: home::Homes,
    shown_number_dropped: u64,
    runs_in: Option<TerminalKey>,
    projected: Projected,
    luned: PathBuf,
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
    pointer_shape: Option<embedded::PointerShape>,
    pending_graphics: Vec<u8>,
    graphics: Graphics,
    exact_cell: Option<CellPixels>,
    cell: CellPixels,
    window_pixels: Option<WindowPixels>,
    cursor: Option<(CursorShape, bool)>,
    title: Option<String>,
    pacing: Pacing,
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
    diff: DiffFetch,
    ribbons: Ribbons,
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
    let unkept = log::start(Process::Lune).err();
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
    let luned = match &folder {
        Ok(luned) => luned.clone(),
        Err(why) => return did_not_start(why),
    };
    let cwd = match worktrees::real(&dir) {
        Ok(cwd) => cwd,
        Err(why) => return did_not_start(&format!("no directory to start in: {why}")),
    };
    let seen = |project| news.last_seen(crate::ledger::Session::Project(project));
    let runs_in = match luned::mux::own::terminal_here() {
        Ok(runs_in) => runs_in,
        Err(why) => return did_not_start(&why),
    };
    let start = match ends.after(|| worktrees::start(&luned, named, &cwd, seen)) {
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
        let arriving = Arriving {
            start,
            runs_in,
            news,
            unkept,
            colours: answers.colours,
            releases: answers.kitty,
            graphics: Graphics::of(answers.graphics),
            cell: answers.cell,
            window_pixels: answers.window_pixels,
            desktop: Desktop::start(&events, folder.clone()),
        };
        run_in(
            &mut terminal,
            (folder, &dir),
            arriving,
            events,
            &mut inbox,
            &mut sent,
        )
    });
    if let Err(error) = &ran {
        say_ends(&error.to_string());
    }
    let popped = if answers.kitty {
        execute!(io::stdout(), PopKeyboardEnhancementFlags)
    } else {
        Ok(())
    };
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
        let _ = writeln!(said, "lune: {WAITS}");
    });
    for event in iter::from_fn(|| inbox.try_next()) {
        if let Event::Filed {
            to, words, filed, ..
        } = event
        {
            let _ = writeln!(said, "{}", issue::on_quit(&to, &words, &filed));
        }
    }
    match hidden.and(ran).and_then(|failed| put.map(|()| failed)) {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(failed)) => {
            let _ = writeln!(said, "{failed}");
            ExitCode::FAILURE
        }
        Err(error) => {
            let _ = writeln!(said, "lune: {error}");
            ExitCode::FAILURE
        }
    }
}

fn did_not_start(why: &str) -> ExitCode {
    say_ends(why);
    let _ = writeln!(io::stderr(), "lune: {why}");
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
                log::say("a change lune sent panicked");
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
    terminal.write_all(b"\x1b]22;\x1b\\")?;
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
    (folder, dir): (Result<PathBuf, String>, &Path),
    arriving: Arriving,
    events: Sender<Event>,
    inbox: &mut Inbox,
    sent: &mut Vec<JoinHandle<()>>,
) -> io::Result<Option<String>> {
    let area = Rect::from((Position::ORIGIN, terminal.size()?));
    let (columns, rows) = (area.width, area.height);
    let launch = Launch {
        folder,
        dir: dir.to_owned(),
        columns,
        rows,
    };
    let mut app = App::opened(launch, &state::Folder::of_user(), arriving, events);
    let ran = run_loop(terminal, &mut app, inbox);
    let mut deletes = std::mem::take(&mut app.pending_graphics);
    if let Some(right) = app.viewed_mut() {
        deletes.extend(right.drain_all_deletes());
    }
    let flushed = if deletes.is_empty() {
        Ok(())
    } else {
        terminal
            .backend_mut()
            .write_all(&deletes)
            .and_then(|()| terminal.backend_mut().flush())
    };
    if let Err(error) = app.clear_graphics(terminal.backend_mut()) {
        log::say(&format!("graphics cleanup failed: {error}"));
    }
    sent.extend(app.sent());
    ran?;
    flushed?;
    Ok(app.failure())
}

pub struct Launch {
    pub folder: Result<PathBuf, String>,
    pub dir: PathBuf,
    pub columns: u16,
    pub rows: u16,
}

struct Arriving {
    start: Option<Arrival>,
    runs_in: Option<TerminalKey>,
    news: News,
    unkept: Option<String>,
    colours: Colours,
    releases: bool,
    graphics: Graphics,
    cell: Option<CellPixels>,
    window_pixels: Option<WindowPixels>,
    desktop: Desktop,
}

impl App {
    #[must_use]
    pub fn open(launch: Launch, state_folder: &Path) -> (App, Inbox) {
        let news = match &launch.folder {
            Ok(runtime) => News::within(runtime),
            Err(why) => News::unshared(why),
        };
        let (events, inbox) = mpsc::channel();
        let arriving = Arriving {
            start: None,
            runs_in: None,
            news,
            unkept: None,
            colours: Colours::default(),
            releases: false,
            graphics: Graphics::Plain,
            cell: None,
            window_pixels: None,
            desktop: Desktop::unserved(&events),
        };
        let state_folder = state::Folder::at(state_folder.to_owned());
        (
            App::opened(launch, &state_folder, arriving, events),
            Inbox::new(inbox),
        )
    }

    fn opened(
        launch: Launch,
        state_folder: &state::Folder,
        arriving: Arriving,
        events: Sender<Event>,
    ) -> App {
        let Launch {
            folder,
            dir,
            columns,
            rows,
        } = launch;
        let Arriving {
            start,
            runs_in,
            news,
            unkept,
            colours,
            releases,
            graphics,
            cell,
            window_pixels,
            desktop,
        } = arriving;
        let (width, width_unread) = Width::kept(state_folder);
        let folds = Folds::shared(state_folder);
        let folds_dirs = folds.dirs();
        let folds_watched = (!folds_dirs.is_empty())
            .then(|| folds::watch(&folds_dirs, events.clone()))
            .transpose();
        let news_watched = news::watch(&news, events.clone());
        let (agent, agent_unread) = Kept::shared(state_folder);
        let agent_watched = agent
            .file()
            .map(|file| agents::watch(file, events.clone()))
            .transpose();
        let mut sidebar = Sidebar::new(
            folds,
            news,
            agent,
            machines::local_name(),
            SystemTime::now(),
        );
        if let Some(why) = unkept {
            sidebar.notify(format!("what goes wrong is kept in no log: {why}"));
        }
        let mut watching = Vec::new();
        match folds_watched {
            Ok(watcher) => watching.extend(watcher),
            Err(error) => {
                let why = format!("folds made elsewhere show from lune's next start: {error}");
                sidebar.notify(why);
            }
        }
        match news_watched {
            Ok(watcher) => watching.push(watcher),
            Err(error) => {
                sidebar.notify(format!(
                    "dots of other lunes show from lune's next start: {error}"
                ));
            }
        }
        match agent_watched {
            Ok(watcher) => watching.extend(watcher),
            Err(error) => {
                let why =
                    format!("a harness picked elsewhere holds from lune's next start: {error}");
                sidebar.notify(why);
            }
        }
        if let Some(why) = agent_unread {
            sidebar.notify(format!("the menu starts agents in claude: {why}"));
        }
        if let Some(why) = width_unread {
            sidebar.notify(format!(
                "the sidebar opens {} wide: {why}",
                sidebar::COLUMNS
            ));
        }
        let daemon = match folder {
            Ok(folder) => Some(folder),
            Err(why) => {
                let _ = sidebar.lost(why);
                None
            }
        };
        let areas = areas(Rect::new(0, 0, columns, rows), width.columns());
        let luned = daemon.clone().unwrap_or_default();
        let exact_cell = cell;
        let cell = embedded::cell_pixels(exact_cell, window_pixels, columns, rows);
        let browser_config = embedded::Config {
            mode: browser_mode(graphics, &luned),
            cell,
            colours,
        };
        let (size, greeting, here) = (layout_size(areas.1), greeting(colours), Machine::Local);
        let (start, root) = match start {
            Some(Arrival::Worktree(start)) => (Some(start), None),
            Some(Arrival::Home(root)) => (None, Some(root)),
            None => (None, None),
        };
        let right = start.map(|start| {
            let viewed = Viewed::start(
                1,
                luned.clone(),
                here,
                start,
                size,
                greeting,
                runs_in,
                browser_config,
                &events,
            );
            Right::Worktree(viewed)
        });
        let focus = Focus::on(if right.is_some() {
            Side::Session
        } else {
            Side::Sidebar
        });
        sidebar.focus(focus.keys());
        let reported = Holder::of(&focus, sidebar.holding()).reported();
        let focused = true;
        let mut app = App {
            right,
            homes: home::Homes::default(),
            shown_number_dropped: 0,
            runs_in,
            projected: Projected::default(),
            luned,
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
            pointer_shape: None,
            pending_graphics: Vec::new(),
            graphics,
            exact_cell,
            cell,
            window_pixels,
            cursor: None,
            title: None,
            pacing: Pacing::default(),
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
            diff: DiffFetch::new(),
            ribbons: Ribbons::new(graphics.supported()),
            daemon,
            events,
            _watching: watching,
            dir,
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
        app
    }
}

fn browser_mode(graphics: Graphics, luned: &Path) -> embedded::Mode {
    if graphics.supported() && !desktop::elsewhere() {
        embedded::Folder::new(luned.to_path_buf())
            .map_or(embedded::Mode::Unsupported, embedded::Mode::Supported)
    } else {
        embedded::Mode::Unsupported
    }
}

fn greeting(colours: Colours) -> Greeting {
    let (foreground, background) = colours.both();
    Greeting {
        foreground,
        background,
    }
}

fn layout_size(area: Rect) -> lune_domain::projects::Size {
    let size = window(area);
    viewed::layout_size(size.columns, size.lines)
}

fn caught<T>(panicked: &str, asked: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    panic::catch_unwind(AssertUnwindSafe(asked)).unwrap_or_else(|_| Err(panicked.to_owned()))
}

fn unmade_caught<T>(
    panicked: &str,
    said: impl FnOnce(Unchanged) -> String,
    asked: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let in_doubt = || said(Unchanged::InDoubt(panicked.to_owned()));
    panic::catch_unwind(AssertUnwindSafe(asked)).unwrap_or_else(|_| Err(in_doubt()))
}

fn doubted<T>(
    panicked: &str,
    asked: impl FnOnce() -> Result<T, Unchanged>,
) -> Result<T, Unchanged> {
    panic::catch_unwind(AssertUnwindSafe(asked))
        .unwrap_or_else(|_| Err(Unchanged::InDoubt(panicked.to_owned())))
}

fn unrenamed(rename: &Rename, unchanged: Unchanged) -> String {
    let in_doubt = format!("the {} may be renamed", rename.noun());
    format!("menu: {}", unchanged.said("not renamed", &in_doubt))
}

fn gone(projected: &Projected, home: &Home) -> bool {
    home.known() && projected.lacks(home.machine(), home.project())
}

fn fetch_work(luned: &Path, events: &Sender<Event>, home: &Home) {
    assert!(
        home.fetching(),
        "a home is fetched for only once it asks, and so once at a time"
    );
    let (number, on, project) = (home.number(), home.machine().clone(), home.project());
    let (luned, events) = (luned.to_owned(), events.clone());
    thread::spawn(move || {
        let listed = caught(WORK_PANICKED, || worktrees::work(&luned, &on, project));
        let _ = events.send(Event::Work { number, listed });
    });
}

struct Move {
    ticket: u64,
    order: Reorder,
}

const MOVE_PANICKED: &str = "lune could not ask luned for the move: asking panicked";

struct Mover {
    moves: SyncSender<Move>,
    moving: JoinHandle<()>,
}

impl Mover {
    fn on(luned: &Path, on: &Machine, events: &Sender<Event>) -> Mover {
        let (moves, asked) = mpsc::sync_channel::<Move>(sidebar::MOVES_MAX);
        let (luned, on, events) = (luned.to_owned(), on.clone(), events.clone());
        let moving = thread::spawn(move || {
            let mut stopped = Stopped::default();
            for Move { ticket, order } in asked {
                if stopped.holds(ticket) {
                    continue;
                }
                let asked = || worktrees::reordered(&luned, &on, &order);
                let event = match doubted(MOVE_PANICKED, asked) {
                    Ok(kept) => Event::Moved { ticket, kept },
                    Err(unmoved) => {
                        stopped.stop(ticket);
                        Event::Unmoved {
                            on: on.clone(),
                            ticket,
                            order,
                            unmoved,
                        }
                    }
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

fn run_loop<W: io::Write>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    app: &mut App,
    inbox: &mut Inbox,
) -> io::Result<()> {
    assert!(
        !app.quit_asked,
        "a new run loop starts before quit is asked"
    );
    let now = Moment::now();
    app.tick(now);
    app.draw_due(terminal, now)?;
    loop {
        assert!(
            !app.quit_asked,
            "the run loop exits as soon as quit is asked"
        );
        let wait = app.wait(Moment::now());
        match inbox.next(wait) {
            Ok(event) => app.handle(event, terminal)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                unreachable!("the app holds a sender, for what it starts")
            }
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
        let now = Moment::now();
        app.tick(now);
        app.draw_due(terminal, now)?;
    }
}

fn areas(frame: Rect, own: u16) -> (Rect, Rect) {
    let sidebar = own.min(frame.width / 2);
    let left = Rect::new(frame.x, frame.y, sidebar, frame.height);
    let right = Rect::new(
        frame.x + sidebar,
        frame.y,
        frame.width - sidebar,
        frame.height,
    );
    (left, right)
}

struct Size {
    columns: NonZeroU16,
    lines: NonZeroU16,
}

fn window(area: Rect) -> Size {
    let cells = |count: u16| NonZeroU16::new(count).unwrap_or(NonZeroU16::MIN);
    Size {
        columns: cells(area.width),
        lines: cells(area.height),
    }
}

impl App {
    fn handle<W: io::Write>(
        &mut self,
        event: Event,
        terminal: &mut Terminal<CrosstermBackend<W>>,
    ) -> io::Result<()> {
        self.handle_at(event, terminal, Moment::now())
    }

    fn handle_at<W: io::Write>(
        &mut self,
        event: Event,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        now: Moment,
    ) -> io::Result<()> {
        let relists = matches!(
            event,
            Event::Daemon(_) | Event::Shown(_, luned::Heard::Laid { .. })
        );
        let mut dirty = match event {
            Event::Input(input) => self.input(input, now),
            Event::Answered(answer) => self.answered(answer, terminal)?,
            Event::InputEnded(error) => return Err(error),
            Event::Shown(number, heard) => {
                let dirty =
                    if let Some(right) = self.viewed_mut().filter(|it| it.number() == number) {
                        let heeded = right.heard(heard);
                        self.heeded(heeded)
                    } else {
                        false
                    };
                self.maybe_fetch_diff(false);
                dirty
            }
            Event::Seen {
                number,
                watcher,
                seen,
            } => {
                if let Some(right) = self.viewed_mut().filter(|it| it.number() == number) {
                    let heeded = right.seen(watcher, seen);
                    self.heeded(heeded)
                } else {
                    false
                }
            }
            Event::Browser {
                number,
                view,
                heard,
            } => {
                if let Some(right) = self.viewed_mut().filter(|it| it.number() == number) {
                    let heeded = right.browser(view, heard);
                    self.heeded(heeded)
                } else {
                    false
                }
            }
            Event::Work { number, listed } => {
                let shown = self.home().is_some_and(|home| home.number() == number);
                if let Some(home) = self.homes.iter_mut().find(|it| it.number() == number) {
                    home.listed(listed, now.steady);
                    shown
                } else {
                    false
                }
            }
            Event::DiffLoaded {
                on,
                worktree,
                revision,
                loaded,
            } => {
                let refetch = match &self.diff.flight {
                    FetchFlight::InFlight {
                        revision: inflight_rev,
                        refetch,
                        ..
                    } if *inflight_rev == revision => {
                        let should_refetch = *refetch;
                        self.diff.flight = FetchFlight::Idle;
                        should_refetch
                    }
                    FetchFlight::Idle | FetchFlight::InFlight { .. } => false,
                };
                let dirty = if let Some(right) = self.viewed_mut()
                    && *right.machine() == on
                    && right.worktree() == worktree
                {
                    right.diff_loaded(worktree, &loaded, revision)
                } else {
                    false
                };
                self.maybe_fetch_diff(refetch);
                dirty
            }
            Event::Daemon(Ok(host)) => {
                self.projected = Projected::adapted(&host).unwrap_or_else(|why| {
                    log::say(&format!("luned's projects: {why}"));
                    Projected::default()
                });
                let mut changed = false;
                if let Some(Right::Worktree(right)) = &mut self.right {
                    let (tabs, titles) = self.projected.tabs(right.machine(), right.worktree());
                    changed |= right.tabs(tabs, titles);
                }
                match self.name_home() {
                    Some(Homed::Named { changed: named }) => changed |= named,
                    Some(Homed::Gone { on }) => {
                        self.go_other_than(&on, None);
                        changed = true;
                    }
                    Some(Homed::Unknown) | None => {}
                }
                let projected = &self.projected;
                self.homes.forget(|home| gone(projected, home));
                let review_status = self.review_git_status(&host);
                self.sidebar.heard(host, now.system);
                self.seat();
                if review_status != self.diff.git_status {
                    self.diff.git_status = review_status;
                    self.maybe_fetch_diff(true);
                }
                if let Some(machines) = self.sidebar.reaches() {
                    let acts = self.alerts.heard(machines, now.steady);
                    self.alert(acts);
                }
                if let Some(sayings) = self.sidebar.sayings() {
                    self.aloud.hear(sayings, self.desktop.reads_aloud());
                }
                changed
                    || matches!(self.focus.over, Some(Over::Picker(_)))
                    || self.sidebar_changed()
            }
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Notice(notice) => {
                self.sidebar.notify(notice);
                true
            }
            Event::Unclosed {
                on,
                act,
                ticket,
                unchanged,
            } => {
                self.sidebar.unclosed(&on, &act, ticket, unchanged);
                true
            }
            Event::Moved { ticket, kept } => {
                self.sidebar.moved(ticket, kept);
                self.sidebar_changed()
            }
            Event::Unmoved {
                on,
                ticket,
                order,
                unmoved,
            } => {
                let note = unmoved.said("not moved", "the rows may have moved");
                self.sidebar.unmoved(&on, ticket, &order, note);
                self.sidebar_changed()
            }
            Event::Copy(clip) => self.clip(terminal, clip, now.steady)?,
            Event::Image(went) => self.image_went(went),
            Event::ImageSlow(on) => self.sidebar.image_going(Some(on)),
            Event::Folds => self.sidebar.refold() && self.sidebar_changed(),
            Event::Alerted(read) => {
                if self.alerts.read(&read) {
                    self.desktop.post(read.alert);
                }
                false
            }
            Event::Clicked(key) => self.clicked(&key),
            #[cfg(not(target_os = "macos"))]
            Event::IssueClicked(desktop::Came::Filed(link)) => self.browse_link(&link, now.steady),
            #[cfg(not(target_os = "macos"))]
            Event::IssueClicked(desktop::Came::Unfiled) => {
                desktop::raise(self.title.as_deref());
                false
            }
            Event::Peers(heard) => {
                if self.desktop.heard(heard, self.focused) {
                    let acts = self.alerts.again();
                    self.alert(acts);
                }
                self.focus_alerts();
                let dirty = self.sidebar.unsay(peers::NO_READER);
                self.hold_aloud();
                dirty
            }
            Event::Skills {
                menu,
                agent,
                skills,
            } => self.sidebar.found(menu, agent, skills),
            Event::Named(made) => {
                self.named(made);
                true
            }
            Event::Pulled { menu, found } => {
                let asked = self.sidebar.pulled(menu, found);
                self.follow(asked);
                true
            }
            Event::Printed { menu, printed } => {
                let asked = self.sidebar.printed(menu, printed);
                self.follow(asked);
                true
            }
            Event::Checks { menu, checks } => {
                let asked = self.sidebar.checks(menu, checks);
                self.follow(asked);
                true
            }
            Event::Rebased { menu, rebased } => {
                let asked = self.sidebar.rebased(menu, rebased);
                self.follow(asked);
                true
            }
            Event::Prunes { menu, plan } => {
                let asked = self.sidebar.prunes(menu, plan);
                self.follow(asked);
                true
            }
            Event::Pruned { menu, pruned } => {
                let asked = self.sidebar.pruned(menu, pruned);
                self.follow(asked);
                true
            }
            Event::Briefed { number, written } => {
                if let Some(Over::Brief(briefing)) = &mut self.focus.over
                    && briefing.number() == number
                {
                    briefing.written(written);
                    true
                } else {
                    false
                }
            }
            Event::Heard { number, heard } => {
                if let Some(Over::Ask(ask)) = &mut self.focus.over
                    && ask.number() == number
                {
                    ask.heard(heard);
                    true
                } else if let Some(Right::Home(shown)) = &self.right
                    && let Some(ask) = self.homes.shown_mut(shown).ask_numbered(number)
                {
                    ask.heard(heard);
                    true
                } else if let Some(ask) = self
                    .homes
                    .iter_mut()
                    .find_map(|home| home.ask_numbered(number))
                {
                    ask.heard(heard);
                    false
                } else {
                    false
                }
            }
            Event::Remotes { number, remotes } => {
                if let Some(Over::Issue(issue) | Over::Dictation(_, Where::Issue(issue))) =
                    &mut self.focus.over
                    && issue.number() == number
                {
                    issue.listed(remotes);
                    true
                } else {
                    false
                }
            }
            Event::Filed {
                number,
                repository,
                to,
                words,
                filed,
            } => {
                self.sidebar.filed(number, &to.to_string(), &filed);
                self.desktop.tell(Told::issue(&to.to_string(), &filed));
                match filed {
                    Ok(filed) => {
                        let _ = self.clip(terminal, Clip::Filed(filed), now.steady)?;
                    }
                    Err(_) => self.unfiled.keep(number, repository, to, words),
                }
                true
            }
            Event::Picker { number, heard } => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.heard(heard);
                    self.picked(did, now.system);
                    true
                } else {
                    false
                }
            }
            Event::Spoken { number, spoken } => {
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                {
                    if let Some(dictated) = dictation.spoken(spoken, now.steady) {
                        self.undictate(Some(dictated));
                    }
                    true
                } else {
                    false
                }
            }
            Event::Nameable { number, skills } => {
                let mut dirty = false;
                if let Err(why) = &skills {
                    self.sidebar.notify(why.clone());
                    dirty = true;
                }
                if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
                    && dictation.number() == number
                {
                    if let Some(dictated) = dictation.found(skills) {
                        self.undictate(Some(dictated));
                    }
                    dirty = true;
                }
                dirty
            }
            Event::Fetched(fetched) => self.fetched(&fetched),
            Event::Voiced { number, ended } => {
                if let Some(why) = self.aloud.ended(number, ended) {
                    self.sidebar.notify(why);
                    true
                } else {
                    false
                }
            }
            Event::VoiceFetched(fetched) => {
                if let Some(why) = self.aloud.fetch_went(fetched) {
                    self.sidebar.notify(why);
                    true
                } else {
                    false
                }
            }
            Event::ReadAloud { from, selected } => {
                if let Err(aloud::NothingToRead) = self.aloud.key(Some(&selected)) {
                    self.desktop.nothing_to_read(from);
                }
                false
            }
            Event::StopAloud => {
                self.aloud.press(aloud::Button::Stop);
                false
            }
            Event::News => self.sidebar.reread() && self.sidebar_changed(),
            Event::Agent => {
                self.sidebar.reagent();
                false
            }
        };
        if relists {
            self.relist();
        }
        dirty |= self.settle();
        dirty |= !self.pending_graphics.is_empty()
            || self.viewed().is_some_and(Viewed::has_pending_graphics);
        if dirty {
            self.pacing.mark_dirty();
        }
        Ok(())
    }

    fn sidebar_changed(&self) -> bool {
        let Some(painted) = self.pacing.clean() else {
            return false;
        };
        let collapsed = self.focus.side == Side::Collapsed;
        painted.sidebar_view != self.sidebar.view(collapsed)
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

    fn wait(&self, now: Moment) -> Duration {
        let painted = match &self.pacing {
            Pacing::Unpainted => return Duration::ZERO,
            Pacing::Drawn {
                dirty: true,
                painted,
            } => {
                assert!(
                    now.steady >= painted.at.steady,
                    "steady clock does not move backward"
                );
                return (painted.at.steady + FRAME_INTERVAL).saturating_duration_since(now.steady);
            }
            Pacing::Drawn {
                dirty: false,
                painted,
            } => {
                assert!(
                    now.steady >= painted.at.steady,
                    "steady clock does not move backward"
                );
                painted
            }
        };
        let mut wait = TICK;
        if let Some(due) = self.sidebar.scroll_due(now.steady) {
            wait = wait.min(due.saturating_duration_since(now.steady));
        }
        if let Some(next) = painted.cadence.wait(painted.at, now) {
            wait = wait.min(next);
        }
        if let Some(due) = self.due(now) {
            wait = wait.min(due.saturating_duration_since(now.steady));
        }
        assert!(
            wait <= TICK,
            "event wait stays bounded by the tick interval"
        );
        wait
    }

    fn due(&self, now: Moment) -> Option<Instant> {
        let picker = match &self.focus.over {
            Some(Over::Picker(picker)) => picker.due(),
            _ => None,
        };
        let shows = self.sidebar.due(now);
        let home = self.home().and_then(Home::due);
        picker
            .into_iter()
            .chain(self.alerts.due())
            .chain(shows)
            .chain(home)
            .min()
    }

    fn tick(&mut self, now: Moment) {
        self.ticked(now.steady);
        self.alerting(now.steady);
        let mut dirty = self.sidebar.ticked(now, self.focused);
        dirty |= self.sync_aloud();
        if let Some(painted) = self.pacing.clean()
            && painted.cadence.due(painted.at, now)
        {
            dirty = true;
        }
        if dirty {
            self.pacing.mark_dirty();
        }
    }

    #[must_use]
    fn sync_aloud(&mut self) -> bool {
        self.sidebar.aloud(self.aloud.shown())
    }

    fn hover(&self) -> Hover {
        let Some(pointer) = self.pointer else {
            return Hover {
                link: None,
                pointer_shape: None,
                drawn: DrawnHover::None,
                aloud: None,
            };
        };
        let (_, right) = self.areas;
        let spot = (!self.covered() && right.contains(pointer)).then(|| attached::Spot {
            row: pointer.y - right.y,
            column: pointer.x - right.x,
        });
        let link = spot.and_then(|spot| self.viewed().and_then(|viewed| viewed.pointed(spot)));
        let pointer_shape = self
            .viewed()
            .and_then(|viewed| viewed.pointer_shape(spot))
            .cloned();
        Hover {
            link,
            pointer_shape,
            drawn: self.drawn.hovered(pointer),
            aloud: self.sidebar.aloud_buttons().hovered(pointer),
        }
    }

    fn alerting(&mut self, now: Instant) {
        let here = self.sidebar.whereabouts();
        let mine = if self.focused {
            self.alerts.at(here.as_ref())
        } else {
            Vec::new()
        };
        self.desktop.looking(&mine);
        let mut looked = self.desktop.looks();
        looked.extend(mine);
        let mut acts = self.alerts.looked(&looked);
        acts.extend(self.alerts.ticked(now));
        self.alert(acts);
    }

    fn focus_alerts(&mut self) {
        let acts = self
            .alerts
            .focus(self.focused || self.desktop.another_focused());
        self.alert(acts);
    }

    fn alert(&mut self, acts: Vec<alerts::Act>) {
        for act in acts {
            match act {
                alerts::Act::Read(due) => {
                    alerts::read(due, &self.events);
                }
                alerts::Act::Close(key) => self.desktop.close(key),
            }
        }
    }

    #[must_use]
    fn clicked(&mut self, key: &alerts::Key) -> bool {
        let Some(place) = self.alerts.clicked(key) else {
            return false;
        };
        self.desktop.close(key.clone());
        if self.desktop.hand(key) {
            return false;
        }
        desktop::raise(self.title.as_deref());
        self.go(&key.on, &Place::View(place));
        self.leave();
        true
    }

    fn ticked(&mut self, now: Instant) {
        if let Some(Over::Picker(picker)) = &mut self.focus.over {
            picker.ticked(now);
        }
        self.ask_picker();
        match &mut self.right {
            Some(Right::Worktree(right)) => right.ticked(),
            Some(Right::Home(_)) => self.ask_work(now),
            None => {}
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

    #[must_use]
    fn settle(&mut self) -> bool {
        self.ask_picker();
        let dirty = self.sync_aloud();
        self.tell();
        dirty
    }

    fn watch(&mut self) {
        if let Some(folder) = self.daemon.take() {
            crate::watch::start(folder, self.events.clone());
        }
    }

    #[must_use]
    fn input(&mut self, input: Input, now: Moment) -> bool {
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
            Input::Key(key) => self.key(&key, now),
            Input::Mouse(mouse) => {
                let moved = mouse.kind == MouseEventKind::Moved;
                let hover_before = moved.then(|| self.hover());
                let was_pressed = self.sidebar.pressed() || matches!(self.held, Held::Edge { .. });
                self.mouse(mouse, now);
                !moved || was_pressed || Some(self.hover()) != hover_before
            }
            Input::Paste(text) => match &mut self.focus.over {
                Some(Over::Ask(ask)) => {
                    ask.paste(&text);
                    true
                }
                Some(Over::Issue(issue)) => {
                    issue.paste(&text);
                    true
                }
                Some(Over::Picker(picker)) => {
                    picker.paste(&text);
                    true
                }
                Some(Over::Brief(_) | Over::Dictation(..)) => false,
                None => match self.holder() {
                    Holder::Sidebar(_) => {
                        let asked = self.sidebar.paste(&text);
                        self.follow(asked);
                        true
                    }
                    Holder::Terminal => self.paste(&text),
                    Holder::Box(_) => unreachable!("a box takes the paste"),
                },
            },
            Input::FocusGained => self.focus_window(true),
            Input::FocusLost => self.focus_window(false),
            Input::Resize(..) => true,
        }
    }

    #[must_use]
    fn key(&mut self, key: &KeyEvent, now: Moment) -> bool {
        let key = self.repeats.read(*key, now.steady);
        let key = &self.aloud_repeats.read(key, now.steady);
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            return match dictation.key(key, now.steady) {
                dictation::Keyed::Nothing => false,
                dictation::Keyed::Changed => true,
                dictation::Keyed::Close => {
                    self.undictate(None);
                    true
                }
                dictation::Keyed::Dictate => {
                    self.undictate(None);
                    self.dictate(now.steady);
                    true
                }
            };
        }
        let chord = (key.code, key.modifiers);
        if key.kind == KeyEventKind::Release
            || (key.kind == KeyEventKind::Repeat
                && (chord == dictation::KEY || chord == aloud::KEY))
        {
            return false;
        }
        if chord == dictation::KEY && self.dictate(now.steady) {
            return true;
        }
        if chord == aloud::KEY {
            return self.read_aloud();
        }
        if aloud::stops(chord) {
            return self.stop_aloud();
        }
        match &mut self.focus.over {
            Some(Over::Ask(ask)) => {
                match ask.key(key) {
                    Keyed::Nothing => {}
                    Keyed::Send => self.converse(),
                    Keyed::Close | Keyed::Interrupt => self.give(Focus::on(self.focus.side)),
                }
                return true;
            }
            Some(Over::Issue(issue)) => {
                match issue.key(key) {
                    issue::Keyed::Nothing => {}
                    issue::Keyed::File => self.file_issue(),
                    issue::Keyed::Close => self.close_issue(),
                }
                return true;
            }
            Some(Over::Picker(picker)) => {
                let did = picker.key(key);
                self.picked(did, now.system);
                return true;
            }
            Some(Over::Brief(briefing)) => {
                if briefing.closes(key) {
                    self.give(Focus::on(self.focus.side));
                    return true;
                }
                return false;
            }
            Some(Over::Dictation(..)) => return false,
            None => {}
        }
        let mut dirty = false;
        match self.sidebar.floating() {
            Some(Floating::Menu(_)) => {
                if chord == MENU {
                    self.sidebar.close_menu();
                } else {
                    let asked = self.sidebar.key(key, now);
                    self.follow(asked);
                }
                return true;
            }
            Some(Floating::Note(_)) => {
                if key.kind == KeyEventKind::Repeat {
                    return false;
                }
                self.sidebar.close_note();
                dirty = true;
                if chord != MENU {
                    return true;
                }
            }
            None => {}
        }
        if self.chord(chord) {
            return true;
        }
        match self.holder() {
            Holder::Terminal if !self.sidebar.pressed() => {
                if chord == images::KEY {
                    match self.key_for_image(key) {
                        ImageKey::Handled { dirty: image_dirty } => return dirty || image_dirty,
                        ImageKey::Unhandled => {}
                    }
                }
                dirty || self.key_to_session(key)
            }
            Holder::Terminal | Holder::Sidebar(_) => {
                let asked = self.sidebar.key(key, now);
                self.follow(asked);
                true
            }
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
            } else if self.viewed().is_some_and(Viewed::browses) {
                self.sidebar.float_note(BROWSER_MENU.to_owned());
            } else if self.viewed().is_some_and(Viewed::reviews) {
                self.sidebar.float_note(REVIEW_MENU.to_owned());
            } else {
                self.sidebar.float_menu();
            }
            return true;
        }
        if let Some(list) = List::opened_by(chord) {
            self.pickers += 1;
            let aim = match (list, self.focus.side) {
                (List::Projects, Side::Sidebar) => self.sidebar.on_header(),
                _ => None,
            };
            let aim = aim.unwrap_or_default();
            let (listing, palette) = (self.sidebar.listing(), self.palette());
            let picker = Box::new(Picker::open(self.pickers, list, listing, aim, palette));
            self.give(Focus {
                side: self.focus.side,
                over: Some(Over::Picker(picker)),
            });
            return true;
        }
        if chord == SWAP {
            let on_terminal = matches!(self.holder(), Holder::Terminal);
            self.give(Focus::on(if on_terminal {
                Side::Sidebar
            } else {
                Side::Session
            }));
            return true;
        }
        if chord == COLLAPSE {
            let collapsed = self.focus.side == Side::Collapsed;
            self.give(Focus::on(if collapsed {
                Side::Session
            } else {
                Side::Collapsed
            }));
            return true;
        }
        if let Some(direction) = stepped(chord) {
            self.step(direction);
            return true;
        }
        if let Some(change) = resized(chord) {
            self.leave();
            if let Some(right) = self.viewed_mut() {
                right.grow(change);
            }
            return true;
        }
        false
    }

    fn palette(&self) -> Palette {
        let non_terminal = self
            .viewed()
            .is_some_and(|right| right.welcomes() || right.browses() || right.reviews());
        let (aim, origin) = match self.focus.side {
            Side::Sidebar => (self.sidebar.aimed_row(), Origin::Sidebar),
            Side::Session | Side::Collapsed if non_terminal => {
                (self.sidebar.aimed_worktree(), Origin::Views)
            }
            Side::Session | Side::Collapsed => (self.sidebar.aimed_view(), Origin::Views),
        };
        Palette { aim, origin }
    }

    #[must_use]
    fn key_to_session(&mut self, key: &KeyEvent) -> bool {
        let (heeded, action, home_dirty) = match &mut self.right {
            Some(Right::Worktree(right)) => {
                let (heeded, action) = right.key(key, self.inputs);
                (heeded, action, false)
            }
            Some(Right::Home(shown)) => {
                self.homes
                    .shown_mut(shown)
                    .key(key, &mut self.asks, &self.events);
                (Heeded::Quiet, ReviewAction::None, true)
            }
            None => (Heeded::Quiet, ReviewAction::None, false),
        };
        let mut dirty = home_dirty | self.heeded(heeded);
        match action {
            ReviewAction::None => {}
            ReviewAction::Reload => self.fetch_diff(),
            ReviewAction::Explain(excerpt) => {
                self.open_review_ask(&excerpt, true);
                dirty = true;
            }
            ReviewAction::Ask(excerpt) => {
                self.open_review_ask(&excerpt, false);
                dirty = true;
            }
        }
        dirty
    }

    #[must_use]
    fn paste(&mut self, text: &str) -> bool {
        match &mut self.right {
            Some(Right::Worktree(right)) => right.paste(text),
            Some(Right::Home(shown)) => {
                self.homes.shown_mut(shown).paste(text);
                true
            }
            None => false,
        }
    }

    fn tell_focus(&mut self, has: bool) {
        if let Some(right) = self.viewed_mut() {
            right.focus(has);
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
            fetch_work(&self.luned, &self.events, home);
        }
    }

    fn go_home(&mut self, on: &Machine, project: ProjectId) {
        let key = home::Key {
            on: on.clone(),
            project,
        };
        if matches!(&self.right, Some(Right::Home(shown)) if *shown == key) {
            return;
        }
        self.drop_shown();
        let (luned, events, asks) = (&self.luned, &self.events, &mut self.asks);
        let dropped = self.homes.show(&key, |number| {
            *asks += 1;
            let home = Home::new(number, *asks, key.on.clone(), key.project);
            fetch_work(luned, events, &home);
            home
        });
        if let Some(dropped) = dropped {
            self.dropped_home(&dropped);
        }
        self.right = Some(Right::Home(key));
        let homed = self.name_home();
        let fresh = matches!(homed, Some(Homed::Named { .. } | Homed::Unknown));
        assert!(
            fresh,
            "a home just shown was made now or held while its project was: {homed:?}"
        );
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
                let changed = home.named(project.name.as_str(), &project.dir);
                Some(Homed::Named { changed })
            }
            None if gone(&self.projected, home) => Some(Homed::Gone {
                on: shown.on.clone(),
            }),
            None => Some(Homed::Unknown),
        }
    }

    fn drop_shown(&mut self) {
        if let Some(Right::Worktree(mut right)) = self.right.take() {
            self.pending_graphics.extend(right.drain_all_deletes());
            let number = right.number();
            let dropped = self.shown_number_dropped;
            assert!(
                number > dropped,
                "a shown number grows past the last dropped"
            );
            self.shown_number_dropped = number;
        }
        assert!(
            self.right.is_none(),
            "lune shows nothing on the right once it dropped it"
        );
    }

    fn dropped_home(&mut self, dropped: &Home) {
        if dropped.turning() {
            let project = self.projected.project(dropped.machine(), dropped.project());
            let name = project.map_or("a project", |project| project.name.as_str());
            let most = home::KEPT_MAX;
            self.sidebar.notify(format!(
                "lune keeps {most} homes at most: {name}'s went, and its turn's agent ended"
            ));
        }
    }

    fn key_for_image(&mut self, key: &KeyEvent) -> ImageKey {
        let Some(on) = self.sidebar.far_agent() else {
            return ImageKey::Unhandled;
        };
        if desktop::elsewhere() {
            return ImageKey::Unhandled;
        }
        let dirty = self.image.send(on.clone(), *key, &self.events) && self.sidebar.clear_notice();
        ImageKey::Handled { dirty }
    }

    fn clip<W: io::Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        clip: Clip,
        steady: Instant,
    ) -> io::Result<bool> {
        if let Some(copied) = self.copies.clip(terminal.backend_mut(), clip)? {
            self.sidebar.copied(copied, steady);
            return Ok(true);
        }
        Ok(false)
    }

    #[must_use]
    fn image_went(&mut self, went: Went) -> bool {
        self.image.heard();
        let mut dirty = self.sidebar.image_going(None);
        match went {
            Went::NoImage { key, on } => {
                let on_terminal = matches!(self.holder(), Holder::Terminal);
                if on_terminal && self.sidebar.far_agent() == Some(&on) {
                    dirty |= self.key_to_session(&key);
                }
            }
            Went::Kept { on, path } => {
                if matches!(self.sidebar.on(), Machine::Remote(there) if *there == on) {
                    dirty |= self.paste(&path);
                } else {
                    self.image_not_pasted(&format!("lune left {on}, where it is at {path}"));
                    dirty = true;
                }
            }
            Went::Failed(why) => {
                self.image_not_pasted(&why);
                dirty = true;
            }
        }
        dirty
    }

    fn image_not_pasted(&mut self, why: &str) {
        let note = format!("image not pasted: {why}");
        match (self.holder(), self.focus.side) {
            (Holder::Terminal, Side::Collapsed) => {
                log::say(&note);
                self.sidebar.float_note(note);
            }
            (Holder::Terminal, Side::Session | Side::Sidebar)
            | (Holder::Sidebar(_) | Holder::Box(_), _) => self.sidebar.notify(note),
        }
    }

    fn mouse(&mut self, mouse: MouseEvent, now: Moment) {
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
            if self
                .burst
                .as_mut()
                .is_some_and(|burst| burst.takes(at, now.steady))
            {
                self.held = Held::Taken;
                return;
            }
            self.burst = None;
            if self.settling.takes(at, now.steady) {
                self.held = Held::Taken;
                self.burst = Some(Burst::new(at, now.steady));
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
            self.burst = Some(Burst::new(at, now.steady));
            self.hit(hit, now);
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
                }
                _ => {}
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
            }
            None => {}
        }
        match self.sidebar.floating() {
            Some(Floating::Menu(_)) => {
                if down && off {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_menu();
                }
                return;
            }
            Some(Floating::Note(_)) => {
                if down {
                    self.let_go();
                    self.held = Held::Taken;
                    self.sidebar.close_note();
                }
                return;
            }
            None => {}
        }
        if self.held == Held::Taken {
            match mouse.kind {
                MouseEventKind::Drag(_) => return,
                MouseEventKind::Up(_) => {
                    self.held = Held::Nothing;
                    return;
                }
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
                }
                MouseEventKind::Up(_) => self.held = Held::Nothing,
                _ => {}
            }
            self.session_mouse(mouse);
            return;
        }
        if self.focus.side == Side::Collapsed {
            if down {
                self.give(Focus::on(Side::Session));
                self.burst = Some(Burst::new(at, now.steady));
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

    fn hit(&mut self, hit: Hit, now: Moment) {
        match hit {
            Hit::Keys(keyed, key) => {
                let asked = self.sidebar.keys_clicked(keyed, &key, now);
                self.follow(asked);
            }
            Hit::Menu(menu, choice) if self.focus.over.is_none() => {
                let asked = self.sidebar.chose(menu, choice, now);
                self.follow(asked);
            }
            Hit::Picker(number, pick) => {
                if let Some(Over::Picker(picker)) = &mut self.focus.over
                    && picker.number() == number
                {
                    let did = picker.click(pick);
                    self.picked(did, now.system);
                }
            }
            Hit::Number(link) => {
                let asked = self.sidebar.number_clicked(link);
                self.follow(asked);
            }
            Hit::Menu(..) => {}
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
        let column = mouse
            .column
            .saturating_sub(right.x)
            .min(right.width.saturating_sub(1));
        let row = mouse
            .row
            .saturating_sub(right.y)
            .min(right.height.saturating_sub(1));
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
            }
            (MouseEventKind::Down(_), _) => {
                let _ = own.unselect();
                false
            }
            (MouseEventKind::Drag(_), Held::Selecting) => {
                own.select_to(at);
                true
            }
            (MouseEventKind::Up(_), Held::Selecting) => {
                self.held = Held::Nothing;
                if let Some(excerpt) = own.selected() {
                    let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
                }
                true
            }
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
            self.width
                .drag(mouse.column.saturating_sub(sidebar.x), terminal);
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
            self.sidebar
                .notify(format!("the sidebar's width stays in this lune: {why}"));
        }
    }

    fn follow(&mut self, asked: Asked) {
        match asked {
            Asked::Nothing => {}
            Asked::OpenWorktree { on, project } => self.open_worktree(on, project.id()),
            Asked::Take => self.give(Focus::on(Side::Sidebar)),
            Asked::Leave => self.leave(),
            Asked::Copy(link) => {
                let _ = self.events.send(Event::Copy(Clip::Pull(link)));
            }
            Asked::CopyText(excerpt) => {
                let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
            }
            Asked::Browse(link) => {
                let _ = self.browse_link(&link, Instant::now());
            }
            Asked::Go { on, place } => {
                self.go(&on, &place);
                self.leave();
            }
            Asked::Close { on, act, ticket } => self.project_act(on, act, ticket),
            Asked::Open {
                asking,
                on,
                project,
                beside,
                opens,
            } => {
                self.project_open(asking, &on, project, beside, opens);
            }
            Asked::Welcome {
                asking,
                on,
                session,
                view,
            } => {
                self.welcome(asking, &on, &session, view);
            }
            Asked::Browser {
                asking,
                on,
                place,
                url,
            } => {
                self.open_browser(asking, &on, &place, url);
            }
            Asked::Review {
                asking,
                on,
                session,
                view,
            } => {
                self.review(asking, &on, &session, view);
            }
            Asked::Arrange {
                asking,
                on,
                session,
                view,
                arranged,
            } => {
                self.arrange(asking, &on, &session, view, arranged);
            }
            Asked::OpenIn {
                asking,
                on,
                worktree,
                command,
            } => {
                self.go_worktree(&on, worktree, None);
                self.shown().open_beside_focused(Opens::Command(command));
                self.give_opened(Opener::of(asking));
            }
            Asked::NewWorktree {
                on,
                project,
                branch,
            } => {
                self.make_worktree(on, move |luned, on| {
                    worktrees::branched(luned, on, project, &branch).map(Landing::Worktree)
                });
                self.leave();
            }
            Asked::Rename { on, rename } => {
                self.rename(on, rename);
            }
            Asked::Make {
                menu,
                on,
                cwd,
                name,
                agent,
            } => {
                let busy = |busy: &str| format!("menu: {}", worktrees::unopened(&cwd, &name, busy));
                if self.refuse_naming(Some(menu), busy) {
                    return;
                }
                let luned = self.luned.clone();
                self.naming_on(move || {
                    let said = |unchanged| worktrees::said_unopened(unchanged, &cwd, &name);
                    let made = unmade_caught(PULL_PANICKED, said, || {
                        let pulled = worktrees::pulled(&luned, &on, &cwd, &name, agent.as_ref())?;
                        Ok(Pulled { on, pulled })
                    });
                    Made::Pulled { menu, made }
                });
            }
            Asked::Pull {
                menu,
                on,
                work,
                wanted,
                worked,
                others,
            } => {
                let asking = pull::Asking {
                    on,
                    work,
                    wanted,
                    worked,
                    others,
                };
                pull::prepare(menu, asking, &self.events);
            }
            Asked::Prunes { menu, on, dir } => prune::plan(menu, on, dir, &self.events),
            Asked::Rebase {
                menu,
                on,
                dir,
                branch,
            } => {
                let here = on == Machine::Local;
                let rebasing = rebase::start(menu, on, dir, branch, &self.events);
                if here {
                    self.keep_change(rebasing);
                }
            }
            Asked::Prune {
                menu,
                on,
                dir,
                agreed,
            } => {
                let here = on == Machine::Local;
                let deleting = prune::delete(menu, on, dir, agreed, &self.events);
                if here {
                    self.keep_change(deleting);
                }
            }
            Asked::Find {
                menu,
                on,
                dir,
                agent,
            } => {
                let said = move |skills| Event::Skills {
                    menu,
                    agent,
                    skills,
                };
                skills::find(&on, &dir, agent, &self.events, said);
            }
            Asked::Printed {
                menu,
                on,
                view,
                ended,
                ..
            } => {
                let terminal = projected::route(&on, view);
                printed::read(menu, on, terminal, ended, &self.events);
            }
            Asked::Checks { menu, on, page } => checks::fetch(menu, &on, page, &self.events),
            Asked::Brief(brief) => {
                self.briefs += 1;
                let briefing = brief::ask(self.briefs, brief, &self.events);
                self.give(Focus {
                    side: self.focus.side,
                    over: Some(Over::Brief(briefing)),
                });
            }
            Asked::Ask(target) => {
                self.asks += 1;
                let ask = Ask::new(self.asks, &target, &self.dir);
                self.give(Focus {
                    side: self.focus.side,
                    over: Some(Over::Ask(ask)),
                });
            }
            Asked::Issue(repository) => {
                self.issues += 1;
                let issue = Issue::open(self.issues, repository, &self.unfiled, &self.events);
                self.give(Focus {
                    side: self.focus.side,
                    over: Some(Over::Issue(issue)),
                });
            }
            Asked::Ssh(destination) => match machines::add(&destination) {
                Ok(Added::Named) => {}
                Ok(Added::Already) => {
                    let named = format!("ssh: lune's hosts file names {destination} already");
                    self.sidebar.notify(named);
                }
                Err(why) => self.sidebar.notify(format!("ssh: {why}")),
            },
            Asked::Forget(destination) => {
                if let Err(why) = machines::forget(&destination) {
                    self.sidebar.notify(format!("forget {destination}: {why}"));
                }
            }
            Asked::Move { on, order, ticket } => {
                let (luned, events) = (&self.luned, &self.events);
                let mover = self.movers.entry(on.clone());
                let mover = mover.or_insert_with(|| Mover::on(luned, &on, events));
                match mover.moves.try_send(Move { ticket, order }) {
                    Ok(()) => {}
                    Err(TrySendError::Full(Move { ticket, order })) => {
                        let why = format!("{} moves are on their way already", sidebar::MOVES_MAX);
                        self.sidebar.unsent(ticket, &order, &why);
                    }
                    Err(TrySendError::Disconnected(Move { ticket, order })) => {
                        if let Some(mover) = self.movers.remove(&on) {
                            self.keep_change(mover.stopped());
                        }
                        let why = "the moves have stopped: move it again";
                        self.sidebar.unsent(ticket, &order, why);
                    }
                }
            }
            Asked::Quit => {
                assert!(!self.quit_asked, "lune follows nothing once asked to quit");
                self.quit_asked = true;
            }
        }
    }

    fn rename(&mut self, on: Machine, rename: Rename) {
        let events = self.events.clone();
        self.keep_change(thread::spawn(move || {
            let asked = || match &rename {
                Rename::Terminal(terminal, to) => {
                    worktrees::renamed_terminal(&on, *terminal, to.as_ref())
                }
                Rename::Worktree(worktree, to) => {
                    worktrees::renamed_worktree(&on, *worktree, to.as_ref())
                }
                Rename::Project(project, to) => worktrees::renamed_project(&on, *project, to),
            };
            if let Err(unchanged) = doubted(RENAME_PANICKED, asked) {
                let _ = events.send(Event::Notice(unrenamed(&rename, unchanged)));
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
        let (luned, events) = (self.luned.clone(), self.events.clone());
        self.keep_change(thread::spawn(move || {
            let asked = || match &act {
                Act::View(ViewKey::Terminal(key)) => worktrees::closed_view(&luned, &on, *key),
                Act::Worktree { worktree, seen, .. } => {
                    worktrees::closed(&luned, &on, *worktree, seen)
                }
                Act::Project {
                    project,
                    worktrees,
                    seen,
                    ..
                } => worktrees::closed_project(&luned, &on, *project, worktrees, seen),
            };
            if let Err(unchanged) = doubted(CLOSE_PANICKED, asked) {
                let _ = events.send(Event::Unclosed {
                    on,
                    act,
                    ticket,
                    unchanged,
                });
            }
        }));
    }

    fn welcome(&mut self, asking: sidebar::Held, on: &Machine, session: &Listed, view: ViewKey) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| project.view_of(view.key()));
        let Some((worktree, terminal)) = held else {
            return self
                .sidebar
                .notify(format!("menu: luned lists no view {view}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().welcome();
        self.give_opened(Opener::of(asking));
    }

    fn open_browser(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        place: &menu::BrowserPlace,
        url: Option<BrowserUrl>,
    ) {
        match place {
            menu::BrowserPlace::View { session, view } => {
                let project = self.projected.project(on, session.project());
                let held = project.and_then(|project| project.view_of(view.key()));
                let Some((worktree, terminal)) = held else {
                    return self
                        .sidebar
                        .notify(format!("menu: luned lists no view {view}"));
                };
                self.go_worktree(on, worktree, Some(terminal));
            }
            menu::BrowserPlace::Worktree(worktree) => {
                self.go_worktree(on, *worktree, None);
            }
        }
        self.shown().open_browser(url);
        self.give_opened(Opener::of(asking));
    }

    fn can_embed_browser(&self) -> bool {
        matches!(
            browser_mode(self.graphics, &self.luned),
            embedded::Mode::Supported(_)
        )
    }

    fn browser_config(&self) -> embedded::Config {
        embedded::Config {
            mode: browser_mode(self.graphics, &self.luned),
            cell: self.cell,
            colours: self.colours,
        }
    }

    #[must_use]
    fn browse_link(&mut self, link: &crate::pull::Link, steady: Instant) -> bool {
        if self.can_embed_browser() && self.viewed().is_some() {
            match BrowserUrl::parse(&link.url()) {
                Ok(url) => {
                    self.shown().open_browser(Some(url));
                    self.leave();
                    return true;
                }
                Err(why) => {
                    log::say(&format!("pull link did not parse as browser URL: {why}"));
                }
            }
        }
        match self.browser.open(link, &self.events, steady) {
            Ok(()) => false,
            Err(why) => {
                self.sidebar.notify(why);
                true
            }
        }
    }

    fn review(&mut self, asking: sidebar::Held, on: &Machine, session: &Listed, view: ViewKey) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| project.view_of(view.key()));
        let Some((worktree, terminal)) = held else {
            return self
                .sidebar
                .notify(format!("menu: luned lists no view {view}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().review();
        self.give_opened(Opener::of(asking));
    }

    fn review_git_status(
        &self,
        host: &Host,
    ) -> Option<(Machine, WorktreeId, luned::api::GitStatus)> {
        let right = self.viewed()?;
        if !right.has_review() {
            return None;
        }
        let (_, worktree) = self.projected.holding(right.machine(), right.worktree())?;
        let dir = worktree.folder.as_str();
        let statuses = match right.machine() {
            Machine::Local => host.git_statuses.as_slice(),
            Machine::Remote(destination) => {
                let spelled = destination.to_string();
                let found = host.remotes.iter().find(|r| r.destination == spelled)?;
                match &found.reached {
                    Some(luned::api::remote::Reached::Host(far)) => far.git_statuses.as_slice(),
                    _ => return None,
                }
            }
        };
        let status = statuses.iter().find(|status| status.dir == dir)?.clone();
        Some((right.machine().clone(), right.worktree(), status))
    }

    fn maybe_fetch_diff(&mut self, force: bool) {
        let Some(right) = self.viewed() else {
            return;
        };
        if !right.has_review() {
            return;
        }
        if !force && !right.review_needs_load() {
            return;
        }
        self.fetch_diff();
    }

    fn fetch_diff(&mut self) {
        let Some((on, worktree)) = self
            .viewed()
            .map(|right| (right.machine().clone(), right.worktree()))
        else {
            return;
        };
        if let FetchFlight::InFlight {
            on: inflight_on,
            worktree: inflight_worktree,
            refetch,
            ..
        } = &mut self.diff.flight
            && *inflight_on == on
            && *inflight_worktree == worktree
        {
            *refetch = true;
            return;
        }
        let revision = self.diff.next_revision();
        self.diff.flight = FetchFlight::InFlight {
            on: on.clone(),
            worktree,
            revision,
            refetch: false,
        };
        let (luned, events) = (self.luned.clone(), self.events.clone());
        thread::spawn(move || {
            let loaded = caught(DIFF_PANICKED, || {
                worktrees::read_diff(&luned, &on, worktree)
            });
            let _ = events.send(Event::DiffLoaded {
                on,
                worktree,
                revision,
                loaded,
            });
        });
    }

    fn open_review_ask(&mut self, excerpt: &lune_diff::SelectionExcerpt, send_now: bool) {
        let Some(right) = self.viewed() else {
            return;
        };
        let on = right.machine().clone();
        let Some((_, worktree)) = self.projected.holding(&on, right.worktree()) else {
            self.sidebar
                .notify("review: luned has not named this worktree's folder yet".to_owned());
            return;
        };
        let dir = worktree.folder.clone();
        self.asks += 1;
        let initial = send_now.then_some(EXPLAIN_PROMPT);
        let mut ask = Ask::reviewed(self.asks, &on, &dir, excerpt, initial);
        if send_now {
            ask.send(Marked::default(), &self.events);
        }
        self.give(Focus {
            side: self.focus.side,
            over: Some(Over::Ask(ask)),
        });
    }

    fn arrange(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        session: &Listed,
        view: ViewKey,
        arranged: Arranged,
    ) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| project.view_of(view.key()));
        let Some((worktree, terminal)) = held else {
            return self
                .sidebar
                .notify(format!("menu: luned lists no view {view}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().arrange(arranged);
        self.give_opened(Opener::of(asking));
    }

    fn project_open(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        project: ProjectId,
        beside: ViewKey,
        opens: Opens,
    ) {
        let Some(project) = self.projected.project(on, project) else {
            return self
                .sidebar
                .notify("menu: luned keeps no such project".to_owned());
        };
        let Some((worktree, terminal)) = project.view_of(beside.key()) else {
            return self
                .sidebar
                .notify(format!("menu: luned lists no view {beside}"));
        };
        self.go_worktree(on, worktree, Some(terminal));
        self.shown().open_beside_focused(opens);
        self.give_opened(Opener::of(asking));
    }

    fn open_worktree(&mut self, on: Machine, project: ProjectId) {
        self.pickers += 1;
        let (listing, palette) = (self.sidebar.listing(), self.palette());
        let picker = Box::new(Picker::opening(
            self.pickers,
            listing,
            (on, project),
            palette,
        ));
        self.give(Focus {
            side: self.focus.side,
            over: Some(Over::Picker(picker)),
        });
    }

    fn picked(&mut self, picked: Picked, wall: SystemTime) {
        match picked {
            Picked::Nothing => {}
            Picked::Close => self.give(Focus::on(self.focus.side)),
            Picked::Go { on, place } => {
                self.go(&on, &place);
                self.give(Focus::on(self.focus.side.session()));
            }
            Picked::Add {
                on,
                project,
                folder,
            } => {
                let project = project.id();
                self.make_worktree(on, move |luned, on| {
                    let added = worktrees::add_worktree(luned, on, (project, folder.as_str()));
                    added.map(Landing::Worktree)
                });
                self.give(Focus::on(self.focus.side.session()));
            }
            Picked::Make { on, folder } => {
                self.make_worktree(on, move |luned, on| worktrees::made_at(luned, on, &folder));
                self.give(Focus::on(self.focus.side.session()));
            }
            Picked::Plan { on, session, scope } => {
                let plan = self.sidebar.plan(&on, &session, scope);
                if let Some(Over::Picker(picker)) = &mut self.focus.over {
                    let did = picker.planned(plan);
                    self.picked(did, wall);
                }
            }
            Picked::Act { on, act } => {
                let ticket = self.sidebar.ticket();
                self.project_act(on, act, ticket);
                self.give(Focus::on(self.focus.side.session()));
            }
            Picked::Chord(chord) => {
                self.give(Focus::on(self.focus.side));
                match chord {
                    aloud::KEY => {
                        let _ = self.read_aloud();
                    }
                    chord if aloud::stops(chord) => {
                        let _ = self.stop_aloud();
                    }
                    chord => {
                        let ran = self.chord(chord);
                        assert!(ran, "every chord the palette lists is one App::chord runs");
                    }
                }
            }
            Picked::Menu {
                target,
                origin,
                action,
            } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.act_in_menu(&target, action, origin);
                self.follow(asked);
            }
            Picked::Pull {
                target,
                work,
                pull,
                origin,
            } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.pull_in_menu(target, work, pull, origin);
                self.follow(asked);
            }
            Picked::Row { target, action } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.act_on_row(&target, action, wall);
                self.follow(asked);
            }
            Picked::Done { done, origin } => {
                self.give(Focus::on(self.focus.side));
                let asked = self.sidebar.chained(done, origin);
                self.follow(asked);
            }
        }
    }

    fn converse(&mut self) {
        if let Some(Over::Ask(ask)) = &mut self.focus.over {
            ask.send(Marked::default(), &self.events);
        }
    }

    fn file_issue(&mut self) {
        let open = self
            .focus
            .over
            .take_if(|over| matches!(over, Over::Issue(_)));
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
        let open = self
            .focus
            .over
            .take_if(|over| matches!(over, Over::Issue(_)));
        if let Some(Over::Issue(issue)) = open {
            issue.close(&mut self.unfiled);
        }
        self.give(Focus::on(self.focus.side));
    }

    fn dictate(&mut self, now: Instant) -> bool {
        let on_session = matches!(self.holder(), Holder::Terminal);
        let under = match self.focus.over.take() {
            None if on_session => Where::Session(self.sidebar.here_agent()),
            Some(Over::Ask(ask)) if !ask.turning() => Where::Ask(ask),
            Some(Over::Issue(issue)) => Where::Issue(issue),
            other => {
                self.focus.over = other;
                return false;
            }
        };
        self.dictations += 1;
        let number = self.dictations;
        let dictation = if speech::fetched() {
            let skills = self.nameable(number, &under);
            if let Skills::Failed(why) = &skills {
                self.sidebar.notify(why.clone());
            }
            let speech = self.speech.get_or_insert_with(Speech::start);
            let listening = speech.listen(number, &self.events);
            Dictation::hearing(number, listening, skills, self.releases, now)
        } else {
            let events = &self.events;
            let fetching = || Fetch {
                _asking: speech::fetch(events),
                got: 0,
            };
            Dictation::fetching(number, self.fetching.get_or_insert_with(fetching).got)
        };
        let over = Some(Over::Dictation(dictation, under));
        self.give(Focus {
            side: self.focus.side,
            over,
        });
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

    #[must_use]
    fn read_aloud(&mut self) -> bool {
        let mut dirty = self.sidebar.clear_notice();
        match self.desktop.reader() {
            Reader::This => {
                if let Err(nothing) = self.aloud.key(self.copies.excerpt()) {
                    self.sidebar.notify(nothing.to_string());
                    dirty = true;
                }
            }
            Reader::Another(holder) => holder.selected(self.copies.excerpt().unwrap_or_default()),
            Reader::Nobody(why) => {
                self.sidebar.notify(why.to_owned());
                dirty = true;
            }
        }
        dirty
    }

    #[must_use]
    fn stop_aloud(&mut self) -> bool {
        let mut dirty = self.sidebar.clear_notice();
        match self.desktop.reader() {
            Reader::This => self.aloud.press(aloud::Button::Stop),
            Reader::Another(holder) => holder.stopped(),
            Reader::Nobody(why) => {
                self.sidebar.notify(why.to_owned());
                dirty = true;
            }
        }
        dirty
    }

    fn hold_aloud(&mut self) {
        let here = matches!(self.focus.over, Some(Over::Dictation(..)));
        self.desktop.dictating(here);
        self.aloud.hold(here || self.desktop.another_dictates());
    }

    fn undictate(&mut self, dictated: Option<Dictated>) {
        let open = self
            .focus
            .over
            .take_if(|over| matches!(over, Over::Dictation(..)));
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
                    let _ = self.paste(&paste);
                }
                if sends {
                    let _ = self.key_to_session(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            Where::Ask(mut ask) => {
                if let Some(dictated) = &dictated {
                    ask.dictated(&dictated.written.to_string());
                }
                self.give(Focus {
                    side,
                    over: Some(Over::Ask(ask)),
                });
                if dictated.is_some_and(|dictated| dictated.sends) {
                    self.converse();
                }
            }
            Where::Issue(mut issue) => {
                if let Some(dictated) = &dictated {
                    issue.dictated(&dictated.written.to_string());
                }
                let sends = dictated.is_some_and(|dictated| dictated.sends) && issue.ready();
                self.give(Focus {
                    side,
                    over: Some(Over::Issue(issue)),
                });
                if sends {
                    self.file_issue();
                }
            }
        }
    }

    #[must_use]
    fn fetched(&mut self, fetched: &Fetched) -> bool {
        let mut dirty = false;
        match fetched {
            Fetched::Got(got) => {
                if let Some(fetch) = &mut self.fetching {
                    fetch.got = *got;
                }
            }
            Fetched::Ended(ended) => {
                self.fetching = None;
                let shown = matches!(self.focus.over, Some(Over::Dictation(..)));
                if let (Err(why), false) = (ended, shown) {
                    self.sidebar
                        .notify(format!("the speech model did not come: {why}"));
                    dirty = true;
                }
            }
        }
        if let Some(Over::Dictation(dictation, _)) = &mut self.focus.over {
            dictation.fetched(fetched);
            dirty = true;
        }
        dirty
    }

    fn step(&mut self, direction: Direction) {
        assert!(
            self.focus.over.is_none(),
            "a box takes the key before a step"
        );
        match (self.focus.side, direction) {
            (Side::Sidebar, Direction::Left) => {}
            (Side::Sidebar, Direction::Right) => self.leave(),
            (Side::Session | Side::Collapsed, Direction::Left) => {
                if self.home().is_some() {
                    self.step_to_sidebar();
                } else {
                    self.step_asked(direction);
                }
            }
            (Side::Sidebar, Direction::Up | Direction::Down)
            | (
                Side::Session | Side::Collapsed,
                Direction::Right | Direction::Up | Direction::Down,
            ) => self.step_asked(direction),
        }
    }

    fn step_asked(&mut self, direction: Direction) {
        self.leave();
        if let Some(right) = self.viewed_mut() {
            right.step(direction);
        }
    }

    fn step_to_sidebar(&mut self) {
        self.give(Focus::on(Side::Sidebar));
        let held = matches!(self.holder(), Holder::Sidebar(_));
        assert!(
            held,
            "the sidebar holds the keys past the project's left edge"
        );
    }

    #[must_use]
    fn cross(&mut self, across: Across) -> bool {
        if !matches!(self.holder(), Holder::Terminal) {
            if let Some(right) = self.viewed_mut() {
                right.crossed_edge();
            }
            return false;
        }
        let Some(right) = self.viewed() else {
            return false;
        };
        let on = right.machine().clone();
        let (before, after) = self.projected.tabs_beside(&on, right.worktree());
        match (across, before, after) {
            (Across::Before, Some(beside), _) | (Across::After, _, Some(beside)) => {
                self.go_worktree(&on, beside, None);
                true
            }
            (Across::Before, None, _) => {
                self.step_to_sidebar();
                true
            }
            (Across::After, _, None) => {
                if let Some(right) = self.viewed_mut() {
                    right.stayed_at_edge();
                }
                false
            }
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
                return Err(format!("luned does not reach {destination} now"));
            }
            return Err(format!("{session} is no project luned keeps"));
        };
        let shows = |worktree| {
            self.viewed()
                .is_some_and(|right| right.machine() == machine && right.worktree() == worktree)
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
            }
            Place::View(place) => {
                let terminal_key = place.view().key();
                let (worktree, terminal) = project.view_of(terminal_key).ok_or_else(|| {
                    format!("luned lists no view {} in {session}", terminal_key.id)
                })?;
                self.go_worktree(machine, worktree, Some(terminal));
            }
        }
        Ok(())
    }

    fn go_worktree(&mut self, on: &Machine, worktree: WorktreeId, terminal: Option<ViewId>) {
        if let Some(right) = self.viewed_mut() {
            let deletes = right.go(on, worktree);
            self.pending_graphics.extend(deletes);
        } else {
            self.drop_shown();
            let (size, greeting) = (layout_size(self.areas.1), greeting(self.colours));
            let (folder, events) = (self.luned.clone(), &self.events);
            let (number, on) = (self.shown_number_dropped + 1, on.clone());
            let runs_in = self.runs_in;
            let browser_config = self.browser_config();
            let started = Viewed::start(
                number,
                folder,
                on,
                worktree,
                size,
                greeting,
                runs_in,
                browser_config,
                events,
            );
            self.right = Some(Right::Worktree(started));
        }
        let right = self.shown();
        assert_eq!(right.machine(), on, "lune shows the machine it went to");
        assert_eq!(
            right.worktree(),
            worktree,
            "lune shows the worktree it went to"
        );
        let (tabs, titles) = self.projected.tabs(on, worktree);
        let right = self.shown();
        let _ = right.tabs(tabs, titles);
        if let Some(terminal) = terminal {
            right.focus_view(terminal);
        }
        self.seat();
    }

    fn shown(&mut self) -> &mut Viewed {
        self.viewed_mut().expect("lune went to a worktree")
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
            let worktrees = projects.flat_map(rows::Project::worktrees);
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
        assert_eq!(
            self.focus.side,
            Side::Sidebar,
            "with no worktree shown, no keys go to it"
        );
    }

    fn seat(&mut self) {
        let right = match &self.right {
            None => return,
            Some(Right::Home(shown)) => {
                let (on, project) = (shown.on.clone(), shown.project);
                if self.projected.project(&on, project).is_some() {
                    self.sidebar.sit(Seat {
                        on,
                        project,
                        at: Sitting::Home,
                    });
                } else {
                    self.sidebar.unseat();
                }
                return;
            }
            Some(Right::Worktree(right)) => right,
        };
        let (on, worktree) = (right.machine().clone(), right.worktree());
        let Some((project, _)) = self.projected.holding(&on, worktree) else {
            return;
        };
        let terminal = right
            .focused()
            .and_then(|focused| project.terminal_key(focused));
        let layout = match (right.arranged(), terminal) {
            (Some((arranged, version)), terminal) => SeatedLayout::Live {
                arranged,
                version,
                terminal,
            },
            (None, Some(terminal)) => {
                assert!(
                    !right.live(),
                    "a live worktree with a focused terminal has an arrangement"
                );
                SeatedLayout::Disconnected(terminal)
            }
            (None, None) => SeatedLayout::Unlaid,
        };
        let at = Sitting::Worktree { worktree, layout };
        self.sidebar.sit(Seat {
            on,
            project: project.id,
            at,
        });
    }

    #[must_use]
    fn heeded(&mut self, heeded: Heeded) -> bool {
        let dirty = match heeded {
            Heeded::Quiet => return false,
            Heeded::Changed => true,
            Heeded::Followed => {
                let right = self.shown();
                let (on, worktree) = (right.machine().clone(), right.worktree());
                let (tabs, titles) = self.projected.tabs(&on, worktree);
                let _ = self.shown().tabs(tabs, titles);
                true
            }
            Heeded::Edged(across) => self.cross(across),
            Heeded::Sent(worktree) => {
                let Some(right) = self.viewed() else {
                    return false;
                };
                let on = right.machine().clone();
                self.go_worktree(&on, worktree, None);
                true
            }
            Heeded::Note(note) => {
                self.sidebar.notify(note);
                true
            }
            Heeded::Copy(text) => {
                if let Some(excerpt) = Excerpt::of(text) {
                    let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
                }
                false
            }
            Heeded::OpenOsBrowser(url) => {
                match self.browser.open_url(&url, &self.events, Instant::now()) {
                    Ok(()) => false,
                    Err(why) => {
                        self.sidebar.notify(why);
                        true
                    }
                }
            }
            Heeded::Emptied => {
                self.leave_view();
                true
            }
            Heeded::Forgotten(why) => {
                self.leave_view();
                self.sidebar.notify(format!("luned's layout: {why}"));
                true
            }
            Heeded::Lost(why) => {
                self.sidebar.notify(format!("luned's layout: {why}"));
                true
            }
        };
        self.seat();
        dirty
    }

    fn make_worktree(
        &mut self,
        on: Machine,
        made: impl FnOnce(&Path, &Machine) -> Result<Landing, String> + Send + 'static,
    ) {
        if self.refuse_naming(None, |busy| format!("menu: {busy}")) {
            return;
        }
        let luned = self.luned.clone();
        self.naming_on(move || {
            let said = |unchanged| worktrees::said_unmade(unchanged, "worktree");
            let made = unmade_caught(MAKE_PANICKED, said, || made(&luned, &on));
            Made::Worktree { on, made }
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
        assert_eq!(
            self.naming,
            Naming::Idle,
            "lune names one session at a time"
        );
        self.naming = Naming::Busy;
        let events = self.events.clone();
        self.keep_change(thread::spawn(move || {
            drop(events.send(Event::Named(made())));
        }));
    }

    fn named(&mut self, made: Made) {
        assert_eq!(
            self.naming,
            Naming::Busy,
            "a session was named that `name` did not name"
        );
        self.naming = Naming::Idle;
        match made {
            Made::Worktree {
                on,
                made: Ok(Landing::Worktree(made)),
            } => {
                self.go_worktree(&on, made.worktree(), None);
                self.give_opened(Opener::LetGo);
            }
            Made::Worktree {
                on,
                made: Ok(Landing::Unopened(project)),
            } => {
                self.go_home(&on, project);
                self.give_opened(Opener::LetGo);
            }
            Made::Worktree { made: Err(why), .. } => {
                self.sidebar.notify(why);
            }
            Made::Pulled { menu, made } => self.pulled_made(menu, made),
        }
    }

    fn pulled_made(&mut self, menu: u64, made: Result<Pulled, String>) {
        let opener = self
            .sidebar
            .made_for(menu)
            .map_or(Opener::LetGo, Opener::of);
        let made = match made {
            Ok(made) => made,
            Err(why) => return self.sidebar.notify(format!("menu: {why}")),
        };
        let Pulled { on, pulled } = made;
        let (worktree, focus, unrun) = match pulled {
            worktrees::Pulled::Went(worktree) => (worktree, None, None),
            worktrees::Pulled::Opened { worktree, view } => (worktree, Some(view), None),
            worktrees::Pulled::Unopened { worktree, why } => (worktree, None, Some(why)),
        };
        self.go_worktree(&on, worktree, focus);
        self.give_opened(opener);
        if let Some(why) = unrun {
            self.sidebar.notify(format!("menu: {why}"));
        }
    }

    fn give_opened(&mut self, opener: Opener) {
        assert!(self.right.is_some(), "lune shows the view it opened");
        match (opener, self.holder()) {
            (Opener::Holds, _) => {
                self.leave();
                let afloat = matches!(self.holder(), Holder::Sidebar(Holding::Afloat));
                assert!(
                    !afloat,
                    "the menu that opened the view holds the keys no longer"
                );
            }
            (Opener::LetGo, Holder::Terminal | Holder::Sidebar(Holding::Nothing)) => {
                self.give(Focus::on(self.focus.side.session()));
                let on_terminal = matches!(self.holder(), Holder::Terminal);
                assert!(on_terminal, "the opened view takes the keys");
            }
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
            ) => {}
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
            Side::Session | Side::Collapsed if self.right.is_none() => Focus {
                side: Side::Sidebar,
                ..focus
            },
            Side::Session | Side::Sidebar | Side::Collapsed => focus,
        };
        if focus.side == Side::Collapsed || focus.over.is_some() {
            self.let_go();
            self.sidebar.let_go();
        }
        if let Some(right) = self.viewed_mut() {
            right.forget_step();
        }
        self.sidebar.focus(focus.keys());
        assert!(
            self.sidebar.floating().is_none(),
            "a focus change closes what floats"
        );
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

    #[must_use]
    fn focus_window(&mut self, focused: bool) -> bool {
        self.focused = focused;
        self.desktop.focus(focused);
        self.focus_alerts();
        let mut dirty = false;
        if !focused
            && let Some(Over::Dictation(dictation, _)) = &mut self.focus.over
            && dictation.unfocused()
        {
            dirty = true;
        }
        if !focused {
            let hover_before = self.hover();
            let was_pressed = self.sidebar.pressed();
            self.pointer = None;
            self.sidebar.let_go();
            if was_pressed || self.hover() != hover_before {
                dirty = true;
            }
        }
        if self.reported == Reported::Focused {
            self.tell_focus(focused);
        }
        dirty
    }

    fn answered<W: io::Write>(
        &mut self,
        answer: Answer,
        terminal: &mut Terminal<CrosstermBackend<W>>,
    ) -> io::Result<bool> {
        let mut dirty = false;
        match answer {
            Answer::Foreground(_) | Answer::Background(_) => {
                self.colours.hear(answer);
                let colours = self.colours;
                if let Some(right) = self.viewed_mut() {
                    right.colours(colours);
                }
            }
            Answer::Turned(_) => {
                let backend = terminal.backend_mut();
                backend.write_all(answers::TURNED)?;
                backend.flush()?;
            }
            Answer::CellSize(cell) => {
                self.exact_cell = Some(cell);
                self.cell = cell;
                if let Some(right) = self.viewed_mut() {
                    right.cell_resized(cell);
                }
            }
            Answer::WindowPixels(window_pixels) => {
                self.window_pixels = Some(window_pixels);
                let cols = self.areas.0.width + self.areas.1.width;
                let rows = self.areas.1.height;
                let cell = embedded::cell_pixels(self.exact_cell, Some(window_pixels), cols, rows);
                self.cell = cell;
                if let Some(right) = self.viewed_mut() {
                    right.cell_resized(cell);
                }
            }
            Answer::Graphics => {
                dirty = self.graphics != Graphics::Kitty;
                self.graphics = Graphics::Kitty;
                self.ribbons.enable();
            }
            Answer::Device | Answer::Kitty => {}
        }
        Ok(dirty)
    }

    fn failure(&self) -> Option<String> {
        self.gave_up.as_ref().map(|why| format!("lune: {why}"))
    }

    fn draw_due<W: io::Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        now: Moment,
    ) -> io::Result<()> {
        match &self.pacing {
            Pacing::Unpainted => {}
            Pacing::Drawn {
                dirty: false,
                painted,
            } => {
                assert!(
                    now.steady >= painted.at.steady,
                    "steady clock does not move backward"
                );
                return Ok(());
            }
            Pacing::Drawn {
                dirty: true,
                painted,
            } => {
                assert!(
                    now.steady >= painted.at.steady,
                    "steady clock does not move backward"
                );
                if now.steady.saturating_duration_since(painted.at.steady) < FRAME_INTERVAL {
                    return Ok(());
                }
            }
        }
        let painted = self.draw_at(terminal, now)?;
        self.pacing = Pacing::Drawn {
            dirty: false,
            painted,
        };
        Ok(())
    }

    fn draw_at<W: io::Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        now: Moment,
    ) -> io::Result<Painted> {
        assert!(
            !self.sidebar.aloud(self.aloud.shown()),
            "aloud state is synced before drawing"
        );
        whole(terminal, |terminal| self.frame_at(terminal, now))
    }

    fn frame_at<W: io::Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        now: Moment,
    ) -> io::Result<Painted> {
        let mut graphics = std::mem::take(&mut self.pending_graphics);
        if let Some(right) = self.viewed_mut() {
            graphics.extend(right.drain_graphics());
        }
        if !graphics.is_empty() {
            terminal.backend_mut().write_all(&graphics)?;
        }
        let mut painted = None;
        terminal.draw(|frame| {
            painted = Some(self.paint_at(frame, now));
        })?;
        let painted = painted.expect("terminal.draw invokes its closure before returning Ok");
        self.paint_graphics(terminal.backend_mut())?;
        let cursor = self.viewed().map_or(UNSHOWN_CURSOR, Viewed::cursor_style);
        if self.cursor != Some(cursor) {
            execute!(terminal.backend_mut(), cursor_style(cursor))?;
            self.cursor = Some(cursor);
        }
        let (_, right_area) = self.areas;
        let covered = self.covered();
        let hover = self
            .pointer
            .filter(|at| !covered && right_area.contains(*at))
            .map(|at| attached::Spot {
                row: at.y - right_area.y,
                column: at.x - right_area.x,
            });
        let shape = self
            .viewed()
            .and_then(|right| right.pointer_shape(hover))
            .cloned();
        if self.pointer_shape != shape {
            self.pointer_shape.clone_from(&shape);
            let seq = match &shape {
                Some(shape) => format!("\x1b]22;{}\x1b\\", shape.as_str()),
                None => "\x1b]22;\x1b\\".to_owned(),
            };
            terminal.backend_mut().write_all(seq.as_bytes())?;
        }
        if let Some(title) = self.sidebar.own_title()
            && self.title.as_deref() != Some(title)
        {
            execute!(terminal.backend_mut(), SetTitle(title))?;
            self.title = Some(title.to_owned());
        }
        Ok(painted)
    }

    fn paint_graphics(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        let Ribbons::Supported(kitty) = &mut self.ribbons else {
            return Ok(());
        };
        let pending = mem::take(&mut kitty.pending);
        let next_keys: Vec<(NonZeroU32, RibbonCacheKey)> = pending
            .iter()
            .map(|overlay| (overlay.image_id, overlay.key))
            .collect();
        if next_keys == kitty.shown {
            return Ok(());
        }
        let mut wrote = false;
        for &(old_id, _) in &kitty.shown {
            if !pending.iter().any(|overlay| overlay.image_id == old_id) {
                review::clear_kitty_ribbon(out, old_id)?;
                wrote = true;
            }
        }
        for overlay in &pending {
            let unchanged = kitty
                .shown
                .iter()
                .any(|&(id, key)| id == overlay.image_id && key == overlay.key);
            if !unchanged {
                review::clear_kitty_ribbon(out, overlay.image_id)?;
                review::emit_kitty_ribbon(out, overlay)?;
                wrote = true;
            }
        }
        if wrote {
            out.flush()?;
        }
        kitty.shown = next_keys;
        Ok(())
    }

    fn clear_graphics(&mut self, out: &mut impl io::Write) -> io::Result<()> {
        let Ribbons::Supported(kitty) = &mut self.ribbons else {
            return Ok(());
        };
        if kitty.shown.is_empty() {
            return Ok(());
        }
        for (id, _) in kitty.shown.drain(..) {
            review::clear_kitty_ribbon(out, id)?;
        }
        out.flush()
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        let painted = self.paint_at(frame, Moment::now());
        self.pacing = Pacing::Drawn {
            dirty: false,
            painted,
        };
    }

    fn paint_at(&mut self, frame: &mut Frame<'_>, now: Moment) -> Painted {
        let mut cadence = Cadence::default();
        let collapsed = self.focus.side == Side::Collapsed;
        let own = if collapsed {
            sidebar::COLLAPSED
        } else {
            self.width.columns()
        };
        let frame_area = frame.area();
        let clean = attached::Clean::of_frame(frame);
        let width = own.min(clean.area().width / 2);
        let ((sidebar, beside), right_clean) = clean.paint_left(width, |buffer, sidebar| {
            let beside = if collapsed {
                self.sidebar.draw_collapsed(buffer, sidebar, now);
                Drawn::Nothing
            } else {
                let (drawn, sidebar_cadence) = self.sidebar.draw(buffer, sidebar, now);
                cadence.merge(sidebar_cadence);
                drawn
            };
            (sidebar, beside)
        });
        let right = right_clean.area();
        self.areas = (sidebar, right);
        let size = window(right);
        if let Some(right) = self.viewed_mut() {
            right.resize(size.columns, size.lines);
        }
        self.settling
            .drawn(self.sidebar.aloud_buttons(), now.steady);
        let covered = self.covered();
        let uncovered = |at: &Position| !covered && right.contains(*at);
        let spot = |at: Position| attached::Spot {
            row: at.y - right.y,
            column: at.x - right.x,
        };
        let hover = self.pointer.filter(uncovered).map(spot);
        let kitty_cell_px = self.ribbons.cell_px(frame_area, covered);
        let (cursor, overlays) = match &mut self.right {
            Some(Right::Worktree(shown)) => shown.draw(right_clean, hover, kitty_cell_px),
            Some(Right::Home(shown)) => {
                let (buffer, right) = right_clean.into_buffer();
                let home = self.homes.shown(shown);
                cadence.merge(home.draw(buffer, right, now.system));
                (None, Vec::new())
            }
            None => {
                let (buffer, right) = right_clean.into_buffer();
                draw_unshown(buffer, right);
                (None, Vec::new())
            }
        };
        self.ribbons.set_pending(overlays);
        let buffer = frame.buffer_mut();
        let drawn = match &self.focus.over {
            Some(Over::Brief(briefing)) => {
                cadence.merge(briefing.draw(buffer, right, now.system));
                Drawn::Nothing
            }
            Some(Over::Ask(ask)) => {
                cadence.merge(ask.draw(buffer, right, now.system));
                Drawn::Nothing
            }
            Some(Over::Issue(issue)) => {
                cadence.merge(issue.draw(buffer, right, now.system));
                Drawn::Nothing
            }
            Some(Over::Picker(picker)) => {
                let (drawn, picker_cadence) = picker.draw(buffer, right, now.system);
                cadence.merge(picker_cadence);
                drawn
            }
            Some(Over::Dictation(dictation, under)) => {
                match under {
                    Where::Ask(ask) => cadence.merge(ask.draw(buffer, right, now.system)),
                    Where::Issue(issue) => cadence.merge(issue.draw(buffer, right, now.system)),
                    Where::Session(_) => {}
                }
                cadence.merge(dictation.draw(buffer, right, now));
                Drawn::Nothing
            }
            None => match self.sidebar.floating() {
                Some(Floating::Menu(menu)) => {
                    let (drawn, menu_cadence) = menu.draw(buffer, right, now.system);
                    cadence.merge(menu_cadence);
                    drawn
                }
                Some(Floating::Note(note)) => {
                    menu::draw_note(buffer, right, note);
                    Drawn::Nothing
                }
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
        Painted {
            at: now,
            cadence,
            sidebar_view: self.sidebar.view(collapsed),
        }
    }
}

fn whole<W: io::Write, T>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    draw: impl FnOnce(&mut Terminal<CrosstermBackend<W>>) -> io::Result<T>,
) -> io::Result<T> {
    queue!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let drawn = draw(terminal);
    let ended = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    match (drawn, ended) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (Ok(_), Err(error)) => Err(error),
    }
}

fn pastes(written: &named::Written, harness: agents::Harness) -> Vec<String> {
    let spelled = |command: &skills::Name| agents::spelled(harness, command.as_str());
    let command = written
        .command
        .iter()
        .map(|command| format!("{} ", spelled(command)));
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
        use luned::api::projects::{Held, Kept};
        let host = |held: Held| luned::api::Host {
            projects: Some(luned::api::Projects { held: Some(held) }),
            ..luned::api::Host::default()
        };
        let none = Held::Kept(Kept {
            projects: Vec::new(),
            version: None,
        });
        let projected = Projected::adapted(&host(none)).unwrap();
        let project = ProjectId::of(3).unwrap();
        let named = |on: Machine| {
            let mut home = Home::new(1, 1, on, project);
            let _ = home.named(
                "lune",
                &lune_domain::projects::Dir::parse("/w/lune").unwrap(),
            );
            home
        };
        assert!(gone(&projected, &named(Machine::Local)));
        let far = Machine::Remote("nixos".parse().unwrap());
        assert!(
            !gone(&projected, &named(far)),
            "a machine not reached just now keeps its home"
        );
        let unnamed = Home::new(1, 1, Machine::Local, project);
        assert!(
            !gone(&projected, &unnamed),
            "a home never named has not gone"
        );
        let unkept = host(Held::Unkept("it changed since luned read it".to_owned()));
        let unread = [
            Projected::default(),
            Projected::adapted(&unkept).unwrap(),
            Projected::adapted(&luned::api::Host::default()).unwrap(),
        ];
        for unread in unread {
            assert!(
                !gone(&unread, &named(Machine::Local)),
                "no home is gone while luned's projects are unread"
            );
        }
    }

    #[test]
    fn a_rename_that_panics_says_the_rename_may_be_made_and_never_that_naming_failed() {
        let view = Rename::Terminal(ViewId::of(4).unwrap(), None);
        let panicked = doubted(RENAME_PANICKED, || -> Result<(), Unchanged> {
            panic!("dropped")
        });
        let said = unrenamed(&view, panicked.unwrap_err());
        let maybe = "menu: lune could not ask luned for the rename: asking panicked, so the view may be renamed";
        assert_eq!(said, maybe);
        assert!(!said.contains("name the session"), "{said}");
        let refused = doubted(RENAME_PANICKED, || {
            Err::<(), _>(Unchanged::Refused("taken".to_owned()))
        });
        assert_eq!(
            unrenamed(&view, refused.unwrap_err()),
            "menu: not renamed: taken"
        );
        assert_eq!(doubted(RENAME_PANICKED, || Ok(7)), Ok(7));
    }

    #[test]
    fn a_make_that_panics_says_it_may_be_made_and_never_that_naming_failed() {
        let panics = || -> Result<(), String> { panic!("dropped") };
        let worktree = |unchanged| worktrees::said_unmade(unchanged, "worktree");
        let said = unmade_caught(MAKE_PANICKED, worktree, panics).unwrap_err();
        let maybe = "lune could not ask luned for the worktree: asking panicked, so the worktree may be made";
        assert_eq!(said, maybe);
        let folder = luned::Directory::parse("/w/cdk/issue-5".to_owned()).unwrap();
        let pull = |unchanged| worktrees::said_unopened(unchanged, &folder, "#5");
        let said = unmade_caught(PULL_PANICKED, pull, panics).unwrap_err();
        let maybe = "lune could not ask luned for the pull: asking panicked, so /w/cdk/issue-5, #5's worktree, may be opened";
        assert_eq!(said, maybe);
        let refused = || Err::<(), _>("no worktree made: taken".to_owned());
        assert_eq!(unmade_caught(MAKE_PANICKED, worktree, refused), refused());
        assert_eq!(unmade_caught(MAKE_PANICKED, worktree, || Ok(7)), Ok(7));
    }

    #[test]
    fn the_end_waits_for_the_changes_sent_and_says_so_only_once_it_is_slow() {
        let mut said = 0;
        wait_for(vec![thread::spawn(|| {})], Duration::from_secs(60), || {
            said += 1;
        });
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
            assert_eq!(
                resized((KeyCode::Char(key), modifiers)),
                Some(Growth::Grow),
                "{key}"
            );
        }
        assert_eq!(resized((KeyCode::Char('-'), alt)), Some(Growth::Shrink));
        let none = [
            ('=', KeyModifiers::NONE),
            ('-', shifted),
            ('=', KeyModifiers::CONTROL),
        ];
        for (key, modifiers) in none {
            assert_eq!(
                resized((KeyCode::Char(key), modifiers)),
                None,
                "{key} {modifiers:?}"
            );
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
                self.written
                    .borrow_mut()
                    .push(std::mem::take(&mut self.writing));
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
            named::Written {
                command,
                text: text.to_owned(),
                only_first: Vec::new(),
            }
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
    fn putting_back_shows_a_frame_lune_ended_in() {
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
        assert_eq!(
            screen.cursor_style(),
            Config::default().default_cursor_style
        );
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

    #[test]
    fn embedded_browser_config_survives_watch_and_late_window_pixels_keep_exact_cell() {
        let dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let launch = Launch {
            folder: Ok(dir.path().to_owned()),
            dir: dir.path().to_owned(),
            columns: 120,
            rows: 40,
        };
        let (mut app, _inbox) = App::open(launch, state.path());
        app.graphics = Graphics::Kitty;
        assert!(
            app.daemon.is_none(),
            "App::watch already consumed self.daemon"
        );
        assert_eq!(app.can_embed_browser(), !desktop::elsewhere());
        assert_eq!(
            app.browser_config().mode,
            browser_mode(Graphics::Kitty, &app.luned)
        );
        if !desktop::elsewhere() {
            assert!(matches!(
                app.browser_config().mode,
                embedded::Mode::Supported(_)
            ));
        }

        let nz = |value| NonZeroU16::new(value).unwrap();
        let exact = CellPixels {
            width: nz(11),
            height: nz(23),
        };
        let viewport = Viewport::Fixed(Rect::new(0, 0, 120, 40));
        let backend = CrosstermBackend::new(Writes::default());
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        assert!(
            !app.answered(Answer::CellSize(exact), &mut terminal)
                .unwrap()
        );
        assert_eq!(app.cell, exact);
        assert!(
            !app.answered(
                Answer::WindowPixels(WindowPixels {
                    width: nz(1200),
                    height: nz(800)
                }),
                &mut terminal,
            )
            .unwrap()
        );
        assert_eq!(
            app.cell, exact,
            "late WindowPixels must not clobber exact CellSize"
        );
    }

    type Written = Rc<RefCell<Vec<Vec<u8>>>>;

    type TestTerminal = Terminal<CrosstermBackend<Writes>>;

    fn test_terminal(columns: u16, rows: u16) -> (TestTerminal, Written) {
        let viewport = Viewport::Fixed(Rect::new(0, 0, columns, rows));
        let writes = Writes::default();
        let written = Rc::clone(&writes.written);
        let backend = CrosstermBackend::new(writes);
        let terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        (terminal, written)
    }

    fn test_app(columns: u16, rows: u16) -> (App, Inbox, tempfile::TempDir) {
        let state = tempfile::tempdir().unwrap();
        let launch = Launch {
            folder: Err("none".to_owned()),
            dir: state.path().to_owned(),
            columns,
            rows,
        };
        let (app, inbox) = App::open(launch, state.path());
        (app, inbox, state)
    }

    fn busy_host(views: u32, topic: &str) -> Host {
        use luned::api::{self, Harness, Status, agent::Status as Reported, projects};
        let made_at = Some(prost_types::Timestamp::default());
        let terminals: Vec<api::Terminal> = (1..=views)
            .map(|id| api::Terminal {
                id: u64::from(id),
                found: Some(api::terminal::Found::Key(api::TerminalKey { id, made_at })),
                folder: "/w/lune".to_owned(),
                command: Some("claude".to_owned()),
                ..api::Terminal::default()
            })
            .collect();
        let worktree = api::Worktree {
            id: 100,
            name: "main".to_owned(),
            folder: "/w/lune".to_owned(),
            seen: Some(api::worktree::Seen::Unread(())),
            terminals,
            arranged: Some(api::Arrangement::Tabs.into()),
            ..api::Worktree::default()
        };
        let project = api::Project {
            id: 1,
            name: "lune".to_owned(),
            dir: "/w/lune".to_owned(),
            missing: false,
            holds: Some(api::project::Holds::Clone(api::project::Clone {
                worktrees: vec![worktree],
                listing: None,
            })),
        };
        let kept = projects::Kept {
            projects: vec![project],
            version: None,
        };
        let agent = api::Agent {
            pid: views,
            harness: Some(Harness::ClaudeCode.into()),
            cwd: "/w/lune".to_owned(),
            status: Some(Reported::KnownStatus(Status::Busy.into())),
            status_since: None,
            terminal: Some(api::TerminalKey { id: views, made_at }),
            topic: Some(topic.to_owned()),
            ..api::Agent::default()
        };
        Host {
            projects: Some(api::Projects {
                held: Some(projects::Held::Kept(kept)),
            }),
            agents: vec![agent],
            ..Host::default()
        }
    }

    fn drew(app: &mut App, terminal: &mut TestTerminal, written: &Written, now: Moment) -> bool {
        let before = written.borrow().len();
        app.draw_due(terminal, now).unwrap();
        written.borrow().len() > before
    }

    #[test]
    fn an_idle_app_with_a_busy_agent_off_screen_draws_no_frames() {
        let (mut terminal, written) = test_terminal(80, 8);
        let (mut app, _inbox, _state) = test_app(80, 8);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(8, "step 1"))),
            &mut terminal,
            at(Duration::ZERO),
        )
        .unwrap();
        app.tick(at(Duration::ZERO));
        assert!(drew(&mut app, &mut terminal, &written, at(Duration::ZERO)));
        written.borrow_mut().clear();

        assert_eq!(
            app.wait(at(Duration::ZERO)),
            TICK,
            "busy agent below the viewport does not spin"
        );

        for step in 1..=20 {
            let offset = Duration::from_millis(step * 100);
            app.tick(at(offset));
            assert!(!drew(&mut app, &mut terminal, &written, at(offset)));
        }
        assert!(written.borrow().is_empty(), "idle ticks drew no frames");

        let later = Duration::from_secs(3);
        app.handle_at(
            Event::Daemon(Ok(busy_host(8, "step 2"))),
            &mut terminal,
            at(later),
        )
        .unwrap();
        app.tick(at(later));
        assert!(!drew(&mut app, &mut terminal, &written, at(later)));
        assert!(
            written.borrow().is_empty(),
            "off-screen busy update drew no frames"
        );

        app.go_worktree(&Machine::Local, WorktreeId::of(100).unwrap(), None);
        app.give(Focus::on(Side::Collapsed));
        app.pacing.mark_dirty();
        let collapsed_at = Duration::from_secs(4);
        assert!(drew(&mut app, &mut terminal, &written, at(collapsed_at)));
        written.borrow_mut().clear();

        assert_eq!(
            app.wait(at(collapsed_at)),
            TICK,
            "collapsed sidebar never spins"
        );
        for step in 1..=10 {
            let offset = collapsed_at + Duration::from_millis(step * 100);
            app.handle_at(
                Event::Daemon(Ok(busy_host(8, &format!("step {}", step + 2)))),
                &mut terminal,
                at(offset),
            )
            .unwrap();
            app.tick(at(offset));
            assert!(!drew(&mut app, &mut terminal, &written, at(offset)));
        }
        assert!(
            written.borrow().is_empty(),
            "collapsed busy agent drew no frames"
        );

        let mut screen = Term::new(Config::default(), &TermSize::new(80, 20), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let mut title_row = |written: &Written| {
            for write in written.borrow_mut().drain(..) {
                parser.advance(&mut screen, &write);
            }
            (0..usize::from(sidebar::COLUMNS))
                .map(|column| screen.grid()[Point::new(Line(1), Column(column))].c)
                .collect::<String>()
        };
        let (mut tall, tall_written) = test_terminal(80, 20);
        app.give(Focus::on(Side::Sidebar));
        app.pacing.mark_dirty();
        let shown_at = Duration::from_secs(10);
        assert!(drew(&mut app, &mut tall, &tall_written, at(shown_at)));
        assert!(title_row(&tall_written).contains("⠋1"));

        assert_eq!(
            app.wait(at(shown_at)),
            sidebar::SPIN,
            "on-screen busy agent spins"
        );
        let same_frame = shown_at + Duration::from_millis(50);
        app.tick(at(same_frame));
        assert!(!drew(&mut app, &mut tall, &tall_written, at(same_frame)));
        let next_frame = shown_at + sidebar::SPIN;
        app.tick(at(next_frame));
        assert!(drew(&mut app, &mut tall, &tall_written, at(next_frame)));
        assert!(!tall_written.borrow().is_empty());
        assert!(title_row(&tall_written).contains("⠙1"));
    }

    #[test]
    fn rapid_updates_draw_at_most_once_per_frame_interval() {
        let (mut terminal, written) = test_terminal(80, 12);
        let (mut app, _inbox, _state) = test_app(80, 12);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };

        app.tick(at(Duration::ZERO));
        assert!(drew(&mut app, &mut terminal, &written, at(Duration::ZERO)));
        written.borrow_mut().clear();

        let mut draws = 0;
        for step in 1..=20 {
            let offset = Duration::from_millis(step * 4);
            let now = at(offset);
            app.handle_at(Event::Notice(format!("line {step}")), &mut terminal, now)
                .unwrap();
            app.tick(now);
            if drew(&mut app, &mut terminal, &written, now) {
                draws += 1;
            } else if step < 4 {
                assert_eq!(app.wait(now), FRAME_INTERVAL.saturating_sub(offset));
            }
        }
        assert_eq!(draws, 5, "80 ms of 4 ms updates drew once per 16 ms frame");

        let trailing = Duration::from_millis(84);
        app.handle_at(
            Event::Notice("final".to_owned()),
            &mut terminal,
            at(trailing),
        )
        .unwrap();
        app.tick(at(trailing));
        assert!(!drew(&mut app, &mut terminal, &written, at(trailing)));
        assert_eq!(app.wait(at(trailing)), Duration::from_millis(12));

        let due = Duration::from_millis(96);
        app.tick(at(due));
        assert!(drew(&mut app, &mut terminal, &written, at(due)));
    }

    #[test]
    fn mouse_moved_and_key_release_draw_only_when_visible_state_changes() {
        let (mut terminal, written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };

        app.tick(at(Duration::ZERO));
        assert!(drew(&mut app, &mut terminal, &written, at(Duration::ZERO)));
        written.borrow_mut().clear();

        let mut step = 1u64;
        let send = |app: &mut App,
                    terminal: &mut TestTerminal,
                    written: &Written,
                    step: &mut u64,
                    event: Event|
         -> bool {
            let offset = Duration::from_secs(*step);
            *step += 1;
            let now = at(offset);
            app.handle_at(event, terminal, now).unwrap();
            app.tick(now);
            drew(app, terminal, written, now)
        };

        let release = KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Input(Input::Key(release))
        ));
        assert!(
            !send(
                &mut app,
                &mut terminal,
                &written,
                &mut step,
                Event::Daemon(Err("none".to_owned())),
            ),
            "same error stays quiet"
        );
        assert!(send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Daemon(Err("down".to_owned())),
        ));
        assert!(!send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Daemon(Err("down".to_owned())),
        ));

        let question = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert!(send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Input(Input::Key(question))
        ));
        let spot = (0..20)
            .flat_map(|y| (0..80).map(move |x| Position::new(x, y)))
            .find(|pos| app.drawn.at(*pos).is_some())
            .expect("the legend draws a clickable spot");

        let moved = |x, y| {
            Event::Input(Input::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }))
        };
        assert!(!send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            moved(0, 0)
        ));
        assert!(!send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            moved(1, 0)
        ));
        assert!(
            send(
                &mut app,
                &mut terminal,
                &written,
                &mut step,
                moved(spot.x, spot.y)
            ),
            "entering a spot draws its highlight"
        );
        assert!(
            !send(
                &mut app,
                &mut terminal,
                &written,
                &mut step,
                moved(spot.x + 1, spot.y)
            ),
            "staying on the same spot stays quiet"
        );
        assert!(
            send(&mut app, &mut terminal, &written, &mut step, moved(0, 0)),
            "leaving a spot clears its highlight"
        );
        assert!(send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Input(Input::Resize(90, 24)),
        ));

        let (listening, _stop) = speech::Listening::new();
        let now = at(Duration::from_secs(step));
        let dictation =
            Dictation::hearing(1, listening, Skills::Found(Vec::new()), true, now.steady);
        app.focus.over = Some(Over::Dictation(dictation, Where::Session(None)));
        app.pacing.mark_dirty();
        assert!(drew(&mut app, &mut terminal, &written, now));
        step += 1;

        let other_release = KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert!(!send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Input(Input::Key(other_release)),
        ));
        let hold_release =
            KeyEvent::new_with_kind(KeyCode::Char('r'), KeyModifiers::ALT, KeyEventKind::Release);
        assert!(send(
            &mut app,
            &mut terminal,
            &written,
            &mut step,
            Event::Input(Input::Key(hold_release)),
        ));
    }

    #[test]
    fn wall_clock_step_backward_dirties_and_wakes_immediately_when_spinner_is_shown() {
        let (mut terminal, written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let first = Moment {
            steady: start,
            system,
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            first,
        )
        .unwrap();
        app.tick(first);
        assert!(drew(&mut app, &mut terminal, &written, first));
        assert_eq!(app.wait(first), sidebar::SPIN);

        let stepped_back = Moment {
            steady: start + FRAME_INTERVAL,
            system: system - Duration::from_secs(5),
        };
        assert_eq!(app.wait(stepped_back), Duration::ZERO);
        app.tick(stepped_back);
        assert!(drew(&mut app, &mut terminal, &written, stepped_back));
    }

    #[test]
    fn public_paint_commits_complete_pacing_state_before_tick() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let now = Moment::now();
        app.handle_at(Event::Daemon(Ok(busy_host(1, "work"))), &mut terminal, now)
            .unwrap();
        terminal.draw(|frame| app.paint(frame)).unwrap();

        let later = Moment::now();
        assert!(app.wait(later) <= sidebar::SPIN);
        app.tick(later);
    }

    struct FailingAfterFlush {
        fail_on_flush: Rc<RefCell<Option<usize>>>,
        flushes: usize,
    }

    impl io::Write for FailingAfterFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if *self.fail_on_flush.borrow() == Some(self.flushes) {
                return Err(io::Error::other("broken pipe"));
            }
            Ok(())
        }
    }

    #[test]
    fn mid_frame_write_error_does_not_split_pacing_state() {
        let fail_on_flush = Rc::new(RefCell::new(Some(2)));
        let backend = CrosstermBackend::new(FailingAfterFlush {
            fail_on_flush: Rc::clone(&fail_on_flush),
            flushes: 0,
        });
        let viewport = Viewport::Fixed(Rect::new(0, 0, 80, 20));
        let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport }).unwrap();
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let first = Moment {
            steady: start,
            system,
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            first,
        )
        .unwrap();
        app.tick(first);
        assert!(app.draw_due(&mut terminal, first).is_err());

        let retry = Moment {
            steady: start + Duration::from_millis(5),
            system,
        };
        assert_eq!(app.wait(retry), Duration::ZERO);
        app.tick(retry);

        *fail_on_flush.borrow_mut() = None;
        app.draw_due(&mut terminal, retry).unwrap();
        assert_eq!(app.wait(retry), sidebar::SPIN);
    }
}
