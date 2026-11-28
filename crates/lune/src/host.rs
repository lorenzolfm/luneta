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
use lune_domain::agent::FlowHarness;
use lune_domain::flows::{self, FlowBadgeKind, FlowControl, FlowMode};
use lune_domain::mux::TerminalKey;
use lune_domain::projects::{self, Arranged, BrowserUrl, Opens, ProjectId, ViewId, WorktreeId};
use lune_domain::rows;
use lune_protocol::telemetry::AgentUsage;
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

use crate::actions::{COLLAPSE, Chord, DOWN, GROW, LEFT, MENU, RIGHT, SHRINK, SWAP, UP, revives};
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
use crate::menu::{self, Choice, MenuId, Origin, Rename};
use crate::models::Fetched;
use crate::named;
use crate::news::{self, News};
use crate::peers;
use crate::picker::{self, List, Palette, Picked, Picker};
use crate::printed::{self, Printed};
use crate::projected::{self, Projected};
use crate::prune;
use crate::pull::{self, Found};
use crate::reach::{self, Luned};
use crate::rebase;
use crate::review::{self, LoadedDiff, ReviewAction, RibbonCacheKey, RibbonOverlay};
use crate::sidebar::{
    self, Asked, Floating, HereAgent, Holding, Keys, Listed, Place, Reorder, RightTarget, Seat,
    SeatedLayout, Sidebar, SidebarView, Sitting, SpinnerFrame, ViewKey,
};
use crate::skills::{self, Skill, Skills};
use crate::speech::{self, Speech, Spoken};
use crate::spots::{Burst, Drawn, DrawnHover, Hit, Settling, Spots};
use crate::state;
use crate::statusbar;
use crate::viewed::{self, Across, Heeded, Viewed};
use crate::width::Width;
use crate::worktrees::{self, Arrival, Landing};

pub enum Made {
    Worktree {
        on: Machine,
        made: Result<Landing, String>,
    },
    Pulled {
        menu: MenuId,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reviving {
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

const REVIVING: &str = "lune revives a view already: try again once it has answered";

const MAKE_PANICKED: &str = "lune could not ask luned for the worktree: asking panicked";

const PULL_PANICKED: &str = "lune could not ask luned for the pull: asking panicked";

const RENAME_PANICKED: &str = "lune could not ask luned for the rename: asking panicked";

const CLOSE_PANICKED: &str = "lune could not ask luned for the close: asking panicked";

const REVIVE_PANICKED: &str = "lune could not ask luned to revive the view: asking panicked";

const WORK_PANICKED: &str = "lune could not ask luned for the work: asking panicked";

const DIFF_PANICKED: &str = "lune could not ask luned for the changes: asking panicked";

const FLOW_PANICKED: &str = "lune could not ask luned for the flow: asking panicked";

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
    Revived {
        on: Machine,
        revived: Result<worktrees::Revived, Unchanged>,
    },
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
        menu: MenuId,
        agent: agents::Harness,
        skills: Result<Vec<Skill>, String>,
    },
    Pulled {
        menu: MenuId,
        found: Result<Found, String>,
    },
    Prunes {
        menu: MenuId,
        plan: Result<prunes::Plan, String>,
    },
    Pruned {
        menu: MenuId,
        pruned: Result<prunes::Pruned, String>,
    },
    Printed {
        menu: MenuId,
        printed: Result<Printed, String>,
    },
    Checks {
        menu: MenuId,
        checks: Result<checks::Failing, String>,
    },
    Rebased {
        menu: MenuId,
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

fn is_chord(chord: Chord) -> bool {
    chord == MENU
        || List::opened_by(chord).is_some()
        || chord == SWAP
        || chord == COLLAPSE
        || stepped(chord).is_some()
        || resized(chord).is_some()
        || revives(chord)
}

#[derive(Clone, Copy, Debug)]
enum Holder<'a> {
    Terminal,
    Sidebar(Holding),
    Box(&'a Over),
}

impl<'a> Holder<'a> {
    fn of(side: SplitSide, over: Option<&'a Over>, holding: Holding) -> Holder<'a> {
        if let Some(over) = over {
            assert_ne!(holding, Holding::Afloat, "nothing floats under a box");
            return Holder::Box(over);
        }
        match (holding, side) {
            (holding @ Holding::Afloat, _) | (holding, SplitSide::Sidebar) => {
                Holder::Sidebar(holding)
            }
            (Holding::Nothing | Holding::Press, SplitSide::Session | SplitSide::Collapsed) => {
                Holder::Terminal
            }
            (
                Holding::Chord | Holding::Search | Holding::Reorder | Holding::Menu,
                SplitSide::Session | SplitSide::Collapsed,
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
enum SplitSide {
    Session,
    Sidebar,
    Collapsed,
}

impl SplitSide {
    fn session(self) -> SplitSide {
        match self {
            SplitSide::Collapsed => SplitSide::Collapsed,
            SplitSide::Session | SplitSide::Sidebar => SplitSide::Session,
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "lune holds one right side, shown at a time"
)]
enum Pane {
    SidebarOnly,
    Split { right: Right, side: SplitSide },
}

impl Pane {
    fn side(&self) -> SplitSide {
        match self {
            Pane::SidebarOnly => SplitSide::Sidebar,
            Pane::Split { side, .. } => *side,
        }
    }

    fn collapsed(&self) -> bool {
        matches!(
            self,
            Pane::Split {
                side: SplitSide::Collapsed,
                ..
            }
        )
    }

    fn right(&self) -> Option<&Right> {
        match self {
            Pane::SidebarOnly => None,
            Pane::Split { right, .. } => Some(right),
        }
    }

    fn right_mut(&mut self) -> Option<&mut Right> {
        match self {
            Pane::SidebarOnly => None,
            Pane::Split { right, .. } => Some(right),
        }
    }

    fn set_side(&mut self, next: SplitSide) {
        if let Pane::Split { side, .. } = self {
            *side = next;
        }
    }

    fn keys(&self, over: Option<&Over>) -> Keys {
        match (over, self.side()) {
            (None, SplitSide::Sidebar) => Keys::Sidebar,
            (
                Some(
                    Over::Brief(_)
                    | Over::Ask(_)
                    | Over::Issue(_)
                    | Over::Picker(_)
                    | Over::Dictation(..),
                ),
                SplitSide::Sidebar,
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
                SplitSide::Session | SplitSide::Collapsed,
            ) => Keys::Terminal,
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
    WorktreeSelecting,
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

impl Right {
    fn machine(&self) -> &Machine {
        match self {
            Right::Worktree(right) => right.machine(),
            Right::Home(shown) => &shown.on,
        }
    }
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
    bar: Option<Rect>,
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
    status_bar: statusbar::Snapshot,
}

#[derive(Debug, Default)]
enum Pacing {
    #[default]
    Unpainted,
    Drawn {
        dirty: bool,
        painted: Box<Painted>,
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

type FlowStates = HashMap<(Machine, WorktreeId), (FlowBadgeKind, Option<String>)>;
#[derive(Clone, Debug, PartialEq, Eq)]
struct SyncedTabs {
    viewed: u64,
    on: Machine,
    worktree: WorktreeId,
    projected_revision: u64,
    titles_revision: u64,
}

pub struct App {
    pane: Pane,
    homes: home::Homes,
    shown_number_dropped: u64,
    runs_in: Option<TerminalKey>,
    projected: Projected,
    projected_revision: u64,
    tabs_synced: Option<SyncedTabs>,
    luned: PathBuf,
    movers: HashMap<Machine, Mover>,
    sidebar: Sidebar,
    frame: Rect,
    width: Width,
    held: Held,
    last_press: LastPress,
    over: Option<Over>,
    reported: Reported,
    focused: bool,
    burst: Option<Burst>,
    settling: Settling,
    drawn: Drawn,
    drawn_aloud: Spots<aloud::Button>,
    drawn_bar_links: Spots<pull::Link>,
    last_usage: Option<(agents::Harness, AgentUsage)>,
    pointer: Option<Position>,
    pointer_shape: Option<embedded::PointerShape>,
    pending_graphics: Vec<u8>,
    graphics: Graphics,
    exact_cell: Option<CellPixels>,
    cell: CellPixels,
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
    reviving: Reviving,
    alerts: Alerts,
    desktop: Desktop,
    flow_states: Option<FlowStates>,
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
        let wall = SystemTime::now();
        let frame = Rect::new(0, 0, columns, rows);
        let initial_areas = areas(frame, width.columns());
        let mut sidebar = Sidebar::new(folds, news, agent, machines::local_name(), initial_areas.0);
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
        let luned = daemon.clone().unwrap_or_default();
        let exact_cell = cell;
        let cell = embedded::cell_pixels(exact_cell, window_pixels, columns, rows);
        let browser_config = embedded::Config {
            mode: browser_mode(graphics, &luned),
            cell,
            colours,
        };
        let (size, greeting, here) = (
            layout_size(initial_areas.1),
            greeting(colours),
            Machine::Local,
        );
        let (start, root) = match start {
            Some(Arrival::Worktree(start)) => (Some(start), None),
            Some(Arrival::Home(root)) => (None, Some(root)),
            None => (None, None),
        };
        let pane = match start {
            Some(start) => {
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
                Pane::Split {
                    right: Right::Worktree(viewed),
                    side: SplitSide::Session,
                }
            }
            None => Pane::SidebarOnly,
        };
        let over: Option<Over> = None;
        sidebar.focus(pane.keys(over.as_ref()), wall);
        let reported = Holder::of(pane.side(), over.as_ref(), sidebar.holding()).reported();
        let focused = true;
        let mut app = App {
            pane,
            homes: home::Homes::default(),
            shown_number_dropped: 0,
            runs_in,
            projected: Projected::default(),
            projected_revision: 0,
            tabs_synced: None,
            luned,
            movers: HashMap::new(),
            sidebar,
            frame,
            width,
            held: Held::Nothing,
            last_press: LastPress::Elsewhere,
            over,
            reported,
            focused,
            burst: None,
            settling: Settling::default(),
            drawn: Drawn::Nothing,
            drawn_aloud: Spots::default(),
            drawn_bar_links: Spots::default(),
            last_usage: None,
            pointer: None,
            pointer_shape: None,
            pending_graphics: Vec::new(),
            graphics,
            exact_cell,
            cell,
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
            reviving: Reviving::Idle,
            alerts: Alerts::new(focused),
            desktop,
            flow_states: None,
            quit_asked: false,
        };
        app.watch();
        if let Some(root) = root {
            app.go_home(&Machine::Local, root, wall);
            app.give(SplitSide::Session, None, wall);
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

fn areas(frame: Rect, own: u16) -> (Rect, Rect, Rect) {
    let bar_rows = if frame.height > statusbar::ROWS {
        statusbar::ROWS
    } else {
        0
    };
    let top_height = frame.height - bar_rows;
    let sidebar = own.min(frame.width / 2);
    let left = Rect::new(frame.x, frame.y, sidebar, top_height);
    let right = Rect::new(
        frame.x + sidebar,
        frame.y,
        frame.width - sidebar,
        top_height,
    );
    let bar = Rect::new(frame.x, frame.y + top_height, frame.width, bar_rows);
    assert_eq!(left.height + bar.height, frame.height);
    assert_eq!(right.height + bar.height, frame.height);
    assert_eq!(left.width + right.width, frame.width);
    (left, right, bar)
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
                        self.heeded(heeded, now.system)
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
                    self.heeded(heeded, now.system)
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
                    self.heeded(heeded, now.system)
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
                let projected = Projected::adapted(&host).unwrap_or_else(|why| {
                    log::say(&format!("luned's projects: {why}"));
                    Projected::default()
                });
                if self.projected != projected {
                    self.projected = projected;
                    self.projected_revision = self.projected_revision.wrapping_add(1);
                }
                let mut changed = false;
                match self.name_home() {
                    Some(Homed::Named { changed: named }) => changed |= named,
                    Some(Homed::Gone { on }) => {
                        self.go_other_than(&on, None, now.system);
                        changed = true;
                    }
                    Some(Homed::Unknown) | None => {}
                }
                let projected = &self.projected;
                self.homes.forget(|home| gone(projected, home));
                let review_status = self.review_git_status(&host);
                self.check_flow_transitions();
                self.sidebar.heard(host, now.system);
                self.seat(now.system);
                self.record_usage();
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
                    || matches!(self.over, Some(Over::Picker(_)))
                    || self.chrome_changed(now.system)
            }
            Event::Daemon(Err(why)) => self.sidebar.lost(why),
            Event::Notice(notice) => {
                self.sidebar.notify(notice);
                true
            }
            Event::Revived { on, revived } => {
                self.revived(&on, revived, now.system);
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
                self.sidebar.moved(ticket, kept, now.system);
                self.sidebar_changed()
            }
            Event::Unmoved {
                on,
                ticket,
                order,
                unmoved,
            } => {
                let note = unmoved.said("not moved", "the rows may have moved");
                self.sidebar.unmoved(&on, ticket, &order, note, now.system);
                self.sidebar_changed()
            }
            Event::Copy(clip) => self.clip(terminal, clip, now.steady)?,
            Event::Image(went) => self.image_went(went, now.system),
            Event::ImageSlow(on) => self.sidebar.image_going(Some(on)),
            Event::Folds => self.sidebar.refold(now.system) && self.sidebar_changed(),
            Event::Alerted(read) => {
                if self.alerts.read(&read) {
                    self.desktop.post(read.alert);
                }
                false
            }
            Event::Clicked(key) => self.clicked(&key, now.system),
            #[cfg(not(target_os = "macos"))]
            Event::IssueClicked(desktop::Came::Filed(link)) => self.browse_link(&link, now),
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
                self.named(made, now.system);
                true
            }
            Event::Pulled { menu, found } => {
                let asked = self.sidebar.pulled(menu, found);
                self.follow(asked, now);
                true
            }
            Event::Printed { menu, printed } => {
                let asked = self.sidebar.printed(menu, printed);
                self.follow(asked, now);
                true
            }
            Event::Checks { menu, checks } => {
                let asked = self.sidebar.checks(menu, checks);
                self.follow(asked, now);
                true
            }
            Event::Rebased { menu, rebased } => {
                let asked = self.sidebar.rebased(menu, rebased);
                self.follow(asked, now);
                true
            }
            Event::Prunes { menu, plan } => {
                let asked = self.sidebar.prunes(menu, plan);
                self.follow(asked, now);
                true
            }
            Event::Pruned { menu, pruned } => {
                let asked = self.sidebar.pruned(menu, pruned);
                self.follow(asked, now);
                true
            }
            Event::Briefed { number, written } => {
                if let Some(Over::Brief(briefing)) = &mut self.over
                    && briefing.number() == number
                {
                    briefing.written(written);
                    true
                } else {
                    false
                }
            }
            Event::Heard { number, heard } => {
                if let Some(Over::Ask(ask)) = &mut self.over
                    && ask.number() == number
                {
                    ask.heard(heard);
                    true
                } else if let Some(Right::Home(shown)) = self.pane.right()
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
                    &mut self.over
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
                if let Some(Over::Picker(picker)) = &mut self.over
                    && picker.number() == number
                {
                    let did = picker.heard(heard);
                    self.picked(did, now);
                    true
                } else {
                    false
                }
            }
            Event::Spoken { number, spoken } => {
                if let Some(Over::Dictation(dictation, _)) = &mut self.over
                    && dictation.number() == number
                {
                    if let Some(dictated) = dictation.spoken(spoken, now.steady) {
                        self.undictate(Some(dictated), now.system);
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
                if let Some(Over::Dictation(dictation, _)) = &mut self.over
                    && dictation.number() == number
                {
                    if let Some(dictated) = dictation.found(skills) {
                        self.undictate(Some(dictated), now.system);
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
            Event::News => self.sidebar.reread(now.system) && self.sidebar_changed(),
            Event::Agent => self.sidebar.reagent(),
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

    fn right_target(&self) -> RightTarget {
        match self.pane.right() {
            None => RightTarget::None,
            Some(Right::Home(shown)) => RightTarget::Home {
                on: shown.on.clone(),
                project: shown.project,
            },
            Some(Right::Worktree(viewed)) => RightTarget::Worktree {
                on: viewed.machine().clone(),
                worktree: viewed.worktree(),
                focused: viewed.focused_view(),
            },
        }
    }

    fn record_usage(&mut self) {
        let target = self.right_target();
        if let Some(usage) = self.sidebar.focused_subscription_usage(&target) {
            self.last_usage = Some(usage);
        }
    }

    fn status_bar_snapshot(&self, wall: SystemTime) -> statusbar::Snapshot {
        let target = self.right_target();
        let sidebar_focused = self.pane.side() == SplitSide::Sidebar;
        let sidebar_collapsed = self.pane.collapsed();
        self.sidebar.status_bar(
            &target,
            self.last_usage.as_ref(),
            sidebar_focused,
            sidebar_collapsed,
            wall,
        )
    }

    fn sidebar_changed(&self) -> bool {
        let Some(painted) = self.pacing.clean() else {
            return false;
        };
        painted.sidebar_view != self.sidebar.view()
    }

    fn chrome_changed(&self, wall: SystemTime) -> bool {
        let Some(painted) = self.pacing.clean() else {
            return false;
        };
        painted.sidebar_view != self.sidebar.view()
            || painted.status_bar != self.status_bar_snapshot(wall)
    }

    fn relist(&mut self) {
        if !matches!(self.over, Some(Over::Picker(_))) {
            return;
        }
        let listing = self.sidebar.listing();
        if let Some(Over::Picker(picker)) = &mut self.over {
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
        let picker = match &self.over {
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
        dirty |= self.sync_tabs();
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

    #[must_use]
    fn sync_tabs(&mut self) -> bool {
        let Some(Right::Worktree(right)) = self.pane.right_mut() else {
            self.tabs_synced = None;
            return false;
        };
        let viewed = right.number();
        let on = right.machine();
        let worktree = right.worktree();
        let projected_revision = self.projected_revision;
        let titles_revision = self.sidebar.view_titles_revision();
        if let Some(synced) = &self.tabs_synced
            && synced.viewed == viewed
            && synced.on == *on
            && synced.worktree == worktree
            && synced.projected_revision == projected_revision
            && synced.titles_revision == titles_revision
        {
            return false;
        }
        let on = on.clone();
        let tabs = self.projected.tabs(&on, worktree);
        let titles = self.sidebar.view_titles(&on, worktree);
        let changed = right.tabs(tabs, titles);
        self.tabs_synced = Some(SyncedTabs {
            viewed,
            on,
            worktree,
            projected_revision,
            titles_revision,
        });
        changed
    }

    fn hover(&self) -> Hover {
        let Some(pointer) = self.pointer else {
            return Hover {
                link: None,
                pointer_shape: None,
                drawn: DrawnHover::None,
                aloud: None,
                bar: None,
            };
        };
        let (_, right, _) = self.areas();
        let covered = self.covered();
        let spot = (!covered && right.contains(pointer)).then(|| attached::Spot {
            row: pointer.y - right.y,
            column: pointer.x - right.x,
        });
        let link = spot.and_then(|spot| self.viewed().and_then(|viewed| viewed.pointed(spot)));
        let pointer_shape = self
            .viewed()
            .and_then(|viewed| viewed.pointer_shape(spot))
            .cloned();
        let bar = (!covered)
            .then(|| self.drawn_bar_links.hovered(pointer))
            .flatten();
        Hover {
            link,
            pointer_shape,
            drawn: self.drawn.hovered(pointer),
            aloud: self.drawn_aloud.hovered(pointer),
            bar,
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
    fn clicked(&mut self, key: &alerts::Key, now: SystemTime) -> bool {
        let Some(place) = self.alerts.clicked(key) else {
            return false;
        };
        self.desktop.close(key.clone());
        if self.desktop.hand(key) {
            return false;
        }
        desktop::raise(self.title.as_deref());
        self.go(&key.on, &Place::View(place), now);
        self.leave(now);
        true
    }

    fn ticked(&mut self, now: Instant) {
        if let Some(Over::Picker(picker)) = &mut self.over {
            picker.ticked(now);
        }
        self.ask_picker();
        match self.pane.right_mut() {
            Some(Right::Worktree(right)) => right.ticked(),
            Some(Right::Home(_)) => self.ask_work(now),
            None => {}
        }
    }

    fn ask_picker(&mut self) {
        if let Some(Over::Picker(picker)) = &mut self.over {
            let number = picker.number();
            for ask in picker.asks() {
                picker::ask(number, ask, &self.projected, &self.events);
            }
        }
    }

    #[must_use]
    fn settle(&mut self) -> bool {
        self.ask_picker();
        let dirty = self.sync_aloud() | self.sync_tabs();
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
                let was_pressed = self.sidebar.pressed()
                    || matches!(self.held, Held::Edge { .. })
                    || self.ask_pressed();
                self.mouse(mouse, now);
                !moved || was_pressed || Some(self.hover()) != hover_before
            }
            Input::Paste(text) => match &mut self.over {
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
                None => {
                    let left = self.sidebar.leave_floating_waiting().left();
                    assert!(
                        !self.sidebar.floating_waits(),
                        "no waiting floating menu remains open before paste"
                    );
                    if left {
                        assert!(
                            !matches!(self.holder(), Holder::Sidebar(Holding::Afloat)),
                            "leaving a waiting floating menu releases the floating hold"
                        );
                    }
                    match self.holder() {
                        Holder::Sidebar(_) => {
                            let asked = self.sidebar.paste(&text, now.system);
                            self.follow(asked, now);
                            true
                        }
                        Holder::Terminal => self.paste(&text) | left,
                        Holder::Box(_) => unreachable!("a box takes the paste"),
                    }
                }
            },
            Input::FocusGained => self.focus_window(true),
            Input::FocusLost => self.focus_window(false),
            Input::Resize(columns, rows) => {
                self.resize_frame(Rect::new(0, 0, columns, rows));
                true
            }
        }
    }

    #[must_use]
    fn key(&mut self, key: &KeyEvent, now: Moment) -> bool {
        let key = self.repeats.read(*key, now.steady);
        let key = &self.aloud_repeats.read(key, now.steady);
        if let Some(Over::Dictation(dictation, _)) = &mut self.over {
            return match dictation.key(key, now.steady) {
                dictation::Keyed::Nothing => false,
                dictation::Keyed::Changed => true,
                dictation::Keyed::Close => {
                    self.undictate(None, now.system);
                    true
                }
                dictation::Keyed::Dictate => {
                    self.undictate(None, now.system);
                    self.dictate(now);
                    true
                }
            };
        }
        let chord = (key.code, key.modifiers);
        if key.kind == KeyEventKind::Release {
            return false;
        }
        if key.kind == KeyEventKind::Repeat && ignores_repeat(chord) {
            return false;
        }
        if chord == dictation::KEY && self.dictate(now) {
            return true;
        }
        if chord == aloud::KEY {
            return self.read_aloud();
        }
        if aloud::stops(chord) {
            return self.stop_aloud();
        }
        match &mut self.over {
            Some(Over::Ask(ask)) => {
                match ask.key(key) {
                    Keyed::Nothing => {}
                    Keyed::Send => self.converse(),
                    Keyed::Close | Keyed::Interrupt => {
                        self.give(self.pane.side(), None, now.system);
                    }
                }
                return true;
            }
            Some(Over::Issue(issue)) => {
                match issue.key(key) {
                    issue::Keyed::Nothing => {}
                    issue::Keyed::File => self.file_issue(now.system),
                    issue::Keyed::Close => self.close_issue(now.system),
                }
                return true;
            }
            Some(Over::Picker(picker)) => {
                let did = picker.key(key);
                self.picked(did, now);
                return true;
            }
            Some(Over::Brief(briefing)) => {
                if briefing.closes(key) {
                    self.give(self.pane.side(), None, now.system);
                    return true;
                }
                return false;
            }
            Some(Over::Dictation(..)) => return false,
            None => {}
        }
        let mut dirty = false;
        let mut kept_notice = false;
        match self.sidebar.floating() {
            Some(Floating::Waiting(_)) => {
                if key.kind == KeyEventKind::Repeat {
                    return false;
                }
                if menu::closes(key) {
                    self.sidebar.close_menu();
                    return true;
                }
                if matches!(key.code, KeyCode::Modifier(_)) {
                    return false;
                }
                if self.pane.side() == SplitSide::Sidebar && !menu::bare(key) && !is_chord(chord) {
                    return false;
                }
                let left = self.sidebar.leave_floating_waiting();
                assert!(left.left(), "a waiting floating menu was left");
                kept_notice = left.kept_notice();
                dirty = true;
            }
            Some(Floating::Menu(_)) => {
                if chord == MENU {
                    self.sidebar.close_menu();
                } else {
                    let asked = self.sidebar.key(key, now);
                    self.follow(asked, now);
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
        let ran_chord = self.chord(chord, now.system);
        assert_eq!(
            ran_chord,
            is_chord(chord),
            "is_chord agrees with App::chord"
        );
        if ran_chord {
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
                self.key_to_session(key, now.system) | dirty
            }
            Holder::Terminal | Holder::Sidebar(_) => {
                let asked = if kept_notice {
                    self.sidebar.key_keeping_notice(key, now)
                } else {
                    self.sidebar.key(key, now)
                };
                self.follow(asked, now);
                true
            }
            Holder::Box(_) => unreachable!("a box takes the key"),
        }
    }

    fn chord(&mut self, chord: Chord, now: SystemTime) -> bool {
        if chord == MENU {
            self.let_go_chrome();
            if matches!(self.pane.right(), Some(Right::Home(_))) {
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
            let aim = match (list, self.pane.side()) {
                (List::Projects, SplitSide::Sidebar) => self.sidebar.on_header(),
                _ => None,
            };
            let aim = aim.unwrap_or_default();
            let (listing, palette) = (self.sidebar.listing(), self.palette());
            let picker = Box::new(Picker::open(self.pickers, list, listing, aim, palette));
            self.give(self.pane.side(), Some(Over::Picker(picker)), now);
            return true;
        }
        if chord == SWAP {
            let on_terminal = matches!(self.holder(), Holder::Terminal);
            self.give(
                if on_terminal {
                    SplitSide::Sidebar
                } else {
                    SplitSide::Session
                },
                None,
                now,
            );
            return true;
        }
        if chord == COLLAPSE {
            self.give(
                if self.pane.collapsed() {
                    SplitSide::Session
                } else {
                    SplitSide::Collapsed
                },
                None,
                now,
            );
            return true;
        }
        if let Some(direction) = stepped(chord) {
            self.step(direction, now);
            return true;
        }
        if let Some(change) = resized(chord) {
            self.leave(now);
            if let Some(right) = self.viewed_mut() {
                right.grow(change);
            }
            return true;
        }
        if revives(chord) {
            self.revive_view();
            return true;
        }
        false
    }

    fn palette(&self) -> Palette {
        let aims_worktree = self
            .viewed()
            .is_some_and(|right| right.welcomes() || right.reviews());
        let (aim, origin) = match self.pane.side() {
            SplitSide::Sidebar => (self.sidebar.aimed_row(), Origin::Sidebar),
            SplitSide::Session | SplitSide::Collapsed if aims_worktree => {
                (self.sidebar.aimed_worktree(), Origin::Views)
            }
            SplitSide::Session | SplitSide::Collapsed => (self.sidebar.aimed_view(), Origin::Views),
        };
        Palette { aim, origin }
    }

    #[must_use]
    fn key_to_session(&mut self, key: &KeyEvent, now: SystemTime) -> bool {
        let (heeded, action, home_dirty) = match self.pane.right_mut() {
            Some(Right::Worktree(right)) => {
                let (heeded, action) = right.key(key, self.inputs);
                (heeded, action, false)
            }
            Some(Right::Home(shown)) => {
                let picked = self.sidebar.kept_harness();
                self.homes
                    .shown_mut(shown)
                    .key(key, &mut self.asks, picked, &self.events);
                (Heeded::Quiet, ReviewAction::None, true)
            }
            None => (Heeded::Quiet, ReviewAction::None, false),
        };
        let mut dirty = home_dirty | self.heeded(heeded, now);
        match action {
            ReviewAction::None => {}
            ReviewAction::Reload => self.fetch_diff(),
            ReviewAction::Explain(excerpt) => {
                self.open_review_ask(&excerpt, true, now);
                dirty = true;
            }
            ReviewAction::Ask(excerpt) => {
                self.open_review_ask(&excerpt, false, now);
                dirty = true;
            }
        }
        dirty
    }

    #[must_use]
    fn paste(&mut self, text: &str) -> bool {
        match self.pane.right_mut() {
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
        match self.pane.right() {
            Some(Right::Worktree(right)) => Some(right),
            Some(Right::Home(_)) | None => None,
        }
    }

    fn viewed_mut(&mut self) -> Option<&mut Viewed> {
        match self.pane.right_mut() {
            Some(Right::Worktree(right)) => Some(right),
            Some(Right::Home(_)) | None => None,
        }
    }

    fn home(&self) -> Option<&Home> {
        match self.pane.right() {
            Some(Right::Home(shown)) => Some(self.homes.shown(shown)),
            Some(Right::Worktree(_)) | None => None,
        }
    }

    fn ask_work(&mut self, now: Instant) {
        let Some(Right::Home(shown)) = self.pane.right() else {
            return;
        };
        let home = self.homes.shown_mut(shown);
        if home.relist(now) {
            fetch_work(&self.luned, &self.events, home);
        }
    }

    fn go_home(&mut self, on: &Machine, project: ProjectId, now: SystemTime) {
        let key = home::Key {
            on: on.clone(),
            project,
        };
        if matches!(self.pane.right(), Some(Right::Home(shown)) if *shown == key) {
            return;
        }
        self.unshow_right();
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
        self.replace_right(Right::Home(key));
        let homed = self.name_home();
        let fresh = matches!(homed, Some(Homed::Named { .. } | Homed::Unknown));
        assert!(
            fresh,
            "a home just shown was made now or held while its project was: {homed:?}"
        );
        self.seat(now);
    }

    #[must_use]
    fn name_home(&mut self) -> Option<Homed> {
        let Some(Right::Home(shown)) = self.pane.right() else {
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

    fn replace_right(&mut self, next: Right) {
        let side = self.pane.side();
        let old = mem::replace(&mut self.pane, Pane::Split { right: next, side });
        self.finish_dropped(old);
    }

    fn drop_shown(&mut self) {
        self.unshow_right();
        let old = mem::replace(&mut self.pane, Pane::SidebarOnly);
        self.finish_dropped(old);
    }

    fn unshow_right(&mut self) {
        if let Some(Right::Home(shown)) = self.pane.right() {
            self.homes.shown_mut(shown).unshown();
        }
        if matches!(self.held, Held::Session | Held::WorktreeSelecting) {
            self.held = Held::Nothing;
        }
    }

    fn finish_dropped(&mut self, old: Pane) {
        if let Pane::Split {
            right: Right::Worktree(mut right),
            ..
        } = old
        {
            self.pending_graphics.extend(right.drain_all_deletes());
            let number = right.number();
            let dropped = self.shown_number_dropped;
            assert!(
                number > dropped,
                "a shown number grows past the last dropped"
            );
            self.shown_number_dropped = number;
        }
    }

    fn home_pressed(&self) -> bool {
        self.home().is_some_and(Home::pressed)
    }

    fn ask_pressed(&self) -> bool {
        let over = matches!(&self.over, Some(Over::Ask(ask)) if ask.pressed());
        over || self.home_pressed()
    }

    fn unsweep_ask(&mut self) {
        if let Some(Over::Ask(ask)) = &mut self.over {
            ask.unsweep();
        }
        if let Some(Right::Home(shown)) = self.pane.right() {
            self.homes.shown_mut(shown).unsweep();
        }
    }

    fn let_go_ask(&mut self) {
        if let Some(Over::Ask(ask)) = &mut self.over {
            ask.let_go();
        }
        if let Some(Right::Home(shown)) = self.pane.right() {
            self.homes.shown_mut(shown).let_go();
        }
    }

    fn copy_excerpt(&self, excerpt: Excerpt) {
        if let Err(error) = self.events.send(Event::Copy(Clip::Excerpt(excerpt))) {
            log::say(&format!("copy event dropped on closed channel: {error}"));
        }
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
    fn image_went(&mut self, went: Went, now: SystemTime) -> bool {
        self.image.heard();
        let mut dirty = self.sidebar.image_going(None);
        match went {
            Went::NoImage { key, on } => {
                let on_terminal = matches!(self.holder(), Holder::Terminal);
                if on_terminal && self.sidebar.far_agent() == Some(&on) {
                    dirty |= self.key_to_session(&key, now);
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
        self.notify_or_float(format!("image not pasted: {why}"));
    }

    fn notify_or_float(&mut self, note: String) {
        assert!(!note.is_empty(), "notifications carry text");
        let replaces_note = matches!(self.sidebar.floating(), Some(Floating::Note(_)));
        match (self.holder(), self.pane.side()) {
            (Holder::Terminal, SplitSide::Collapsed) => {
                log::say(&note);
                self.sidebar.float_note(note);
            }
            (Holder::Sidebar(Holding::Afloat), SplitSide::Collapsed) if replaces_note => {
                self.sidebar.close_note();
                log::say(&note);
                self.sidebar.float_note(note);
            }
            (Holder::Terminal, SplitSide::Session | SplitSide::Sidebar)
            | (Holder::Sidebar(_) | Holder::Box(_), _) => self.sidebar.notify(note),
        }
    }

    fn revive_machine(&self) -> Machine {
        match &self.pane {
            Pane::SidebarOnly => self.sidebar.lit_machine().unwrap_or(Machine::Local),
            Pane::Split {
                right,
                side: SplitSide::Sidebar,
            } => self
                .sidebar
                .lit_machine()
                .unwrap_or_else(|| right.machine().clone()),
            Pane::Split {
                right,
                side: SplitSide::Session | SplitSide::Collapsed,
            } => right.machine().clone(),
        }
    }

    fn revive_view(&mut self) {
        if self.reviving == Reviving::Busy {
            self.notify_or_float(REVIVING.to_owned());
            return;
        }
        assert_eq!(
            self.reviving,
            Reviving::Idle,
            "lune revives one view at a time"
        );
        self.reviving = Reviving::Busy;
        let on = self.revive_machine();
        let (luned, events) = (self.luned.clone(), self.events.clone());
        self.keep_change(thread::spawn(move || {
            let revived = doubted(REVIVE_PANICKED, || worktrees::revive_view(&luned, &on));
            let _ = events.send(Event::Revived { on, revived });
        }));
    }

    fn revived(
        &mut self,
        on: &Machine,
        revived: Result<worktrees::Revived, Unchanged>,
        now: SystemTime,
    ) {
        assert_eq!(
            self.reviving,
            Reviving::Busy,
            "a view was revived only while revive_view was in flight"
        );
        self.reviving = Reviving::Idle;
        let revived = match revived {
            Ok(revived) => revived,
            Err(unchanged) => {
                let note = unchanged.said("no view revived", "a view may be revived");
                self.notify_or_float(note);
                return;
            }
        };
        let _ = self.sidebar.clear_notice();
        if matches!(self.sidebar.floating(), Some(Floating::Note(note)) if note == REVIVING) {
            self.sidebar.close_note();
        }
        self.go_worktree(on, revived.worktree, Some(revived.view), now);
        assert_eq!(
            self.viewed().map(Viewed::worktree),
            Some(revived.worktree),
            "lune shows the revived worktree"
        );
        self.give_opened(Opener::LetGo, now);
    }

    fn mouse(&mut self, mouse: MouseEvent, now: Moment) {
        let at = Position::new(mouse.column, mouse.row);
        self.pointer = Some(at);
        let down = matches!(mouse.kind, MouseEventKind::Down(_));
        let under =
            matches!(self.held, Held::Session | Held::WorktreeSelecting) || self.home_pressed();
        if self.covered() && under && !down {
            if !self.select(mouse, now.system) {
                if let MouseEventKind::Up(_) = mouse.kind {
                    self.held = Held::Nothing;
                }
                self.session_mouse(mouse, now.system);
            }
            return;
        }
        if down {
            let _ = self.cancel_capture();
            self.last_press = LastPress::Elsewhere;
            self.sidebar.unsweep();
            self.unsweep_ask();
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
        let button = || self.drawn_aloud.at(at).copied().map(Hit::Aloud);
        let bar_link = || {
            (!self.covered())
                .then(|| self.drawn_bar_links.at(at).cloned().map(Hit::Number))
                .flatten()
        };
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(hit) = self.drawn.at(at).or_else(button).or_else(bar_link)
        {
            self.burst = Some(Burst::new(at, now.steady));
            self.hit(hit, now);
            self.held = Held::Taken;
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
        if !down && self.held == Held::Taken {
            match mouse.kind {
                MouseEventKind::Drag(_) => return,
                MouseEventKind::Up(_) => {
                    self.held = Held::Nothing;
                    return;
                }
                _ => self.held = Held::Nothing,
            }
        }
        let off = !self.drawn.boxed().is_some_and(|area| area.contains(at));
        if let Some(Over::Picker(picker)) = &mut self.over {
            match (mouse.kind, wheel) {
                (_, Some(by)) => picker.scroll(by),
                (MouseEventKind::Down(_), _) if off => {
                    self.give(self.pane.side(), None, now.system);
                    self.held = Held::Taken;
                }
                _ => {}
            }
            return;
        }
        match &mut self.over {
            Some(Over::Ask(ask)) => {
                if let ask::Mouse::Copied(excerpt) = ask.mouse(mouse) {
                    self.copy_excerpt(excerpt);
                }
                return;
            }
            Some(Over::Issue(_) | Over::Picker(_) | Over::Dictation(..)) => return,
            Some(Over::Brief(briefing)) => {
                if down && briefing.clicked() {
                    self.give(self.pane.side(), None, now.system);
                    self.held = Held::Taken;
                }
                return;
            }
            None => {}
        }
        match self.sidebar.floating() {
            Some(Floating::Waiting(_)) => {
                let scrolls = matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp
                        | MouseEventKind::ScrollDown
                        | MouseEventKind::ScrollLeft
                        | MouseEventKind::ScrollRight
                );
                if scrolls || (down && off) {
                    let left = self.sidebar.leave_floating_waiting().left();
                    assert!(left, "a waiting floating menu was left");
                } else {
                    return;
                }
            }
            Some(Floating::Menu(_)) => {
                if down && off {
                    self.sidebar.close_menu();
                    self.held = Held::Taken;
                }
                return;
            }
            Some(Floating::Note(_)) => {
                if down {
                    self.sidebar.close_note();
                    self.held = Held::Taken;
                }
                return;
            }
            None => {}
        }
        let (sidebar, right, bar) = self.areas();
        if matches!(self.held, Held::Edge { .. }) && self.resize(mouse) {
            return;
        }
        let held =
            matches!(self.held, Held::Session | Held::WorktreeSelecting) || self.home_pressed();
        if !held && !self.sidebar.pressed() && bar.contains(at) {
            if down {
                self.held = Held::Taken;
            }
            return;
        }
        let over_session = mouse.column >= right.x;
        if held || (over_session && !self.sidebar.pressed()) {
            let of_a_press = matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_));
            if of_a_press && self.last_press != LastPress::Session {
                return;
            }
            match self.pane.right() {
                Some(Right::Home(_)) => {
                    if !self.select_home(mouse, now.system) && down {
                        self.leave(now.system);
                    }
                    return;
                }
                Some(Right::Worktree(_)) => {
                    if self.select_worktree(mouse, now.system) {
                        return;
                    }
                    match mouse.kind {
                        MouseEventKind::Down(_) => {
                            self.held = Held::Session;
                            self.last_press = LastPress::Session;
                            self.leave(now.system);
                        }
                        MouseEventKind::Up(_) => self.held = Held::Nothing,
                        _ => {}
                    }
                    self.session_mouse(mouse, now.system);
                    return;
                }
                None => return,
            }
        }
        if self.pane.collapsed() {
            if down {
                self.give(SplitSide::Session, None, now.system);
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
        let asked = self.sidebar.mouse(mouse, now.system);
        self.follow(asked, now);
    }

    fn hit(&mut self, hit: Hit, now: Moment) {
        match hit {
            Hit::Keys(keyed, key) => {
                let asked = self.sidebar.keys_clicked(keyed, &key, now);
                self.follow(asked, now);
            }
            Hit::Menu(menu, choice) if self.over.is_none() => {
                let asked = self.sidebar.chose(menu, choice, now);
                self.follow(asked, now);
            }
            Hit::Picker(number, pick) => {
                if let Some(Over::Picker(picker)) = &mut self.over
                    && picker.number() == number
                {
                    let did = picker.click(pick);
                    self.picked(did, now);
                }
            }
            Hit::Number(link) => {
                let asked = self.sidebar.number_clicked(link);
                self.follow(asked, now);
            }
            Hit::Menu(..) => {}
            Hit::Aloud(button) => self.aloud.press(button),
        }
    }

    fn session_mouse(&mut self, mouse: MouseEvent, now: SystemTime) {
        let at = self.session_cell(mouse);
        let width = self.areas().1.width;
        let Some(right) = self.viewed_mut() else {
            return;
        };
        if let Some(worktree) = right.mouse(mouse, at, width) {
            let on = right.machine().clone();
            self.go_worktree(&on, worktree, None, now);
        }
    }

    fn session_cell(&self, mouse: MouseEvent) -> attached::Spot {
        let (_, right, _) = self.areas();
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

    fn select(&mut self, mouse: MouseEvent, now: SystemTime) -> bool {
        match self.pane.right() {
            Some(Right::Worktree(_)) => self.select_worktree(mouse, now),
            Some(Right::Home(_)) => self.select_home(mouse, now),
            None => false,
        }
    }

    fn select_worktree(&mut self, mouse: MouseEvent, now: SystemTime) -> bool {
        let at = self.session_cell(mouse);
        let Some(Right::Worktree(own)) = self.pane.right_mut() else {
            return false;
        };
        match (mouse.kind, self.held) {
            (MouseEventKind::Down(_), _) if own.selects(mouse, at) => {
                own.select_from(at);
                self.held = Held::WorktreeSelecting;
                self.last_press = LastPress::Session;
                self.leave(now);
                true
            }
            (MouseEventKind::Down(_), _) => {
                let _ = own.unselect();
                false
            }
            (MouseEventKind::Drag(_), Held::WorktreeSelecting) => {
                own.select_to(at);
                true
            }
            (MouseEventKind::Up(_), Held::WorktreeSelecting) => {
                self.held = Held::Nothing;
                if let Some(excerpt) = own.selected() {
                    let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
                }
                true
            }
            (
                _,
                Held::Nothing
                | Held::Session
                | Held::WorktreeSelecting
                | Held::Edge { .. }
                | Held::Taken,
            ) => false,
        }
    }

    fn select_home(&mut self, mouse: MouseEvent, now: SystemTime) -> bool {
        let Some(Right::Home(shown)) = self.pane.right() else {
            return false;
        };
        assert!(
            !matches!(self.held, Held::Session | Held::WorktreeSelecting),
            "worktree captures never coexist with a home view"
        );
        match self.homes.shown_mut(shown).mouse(mouse) {
            ask::Mouse::Ignored => false,
            ask::Mouse::Started => {
                self.last_press = LastPress::Session;
                self.leave(now);
                true
            }
            ask::Mouse::Active => true,
            ask::Mouse::Copied(excerpt) => {
                self.copy_excerpt(excerpt);
                true
            }
        }
    }

    fn resize(&mut self, mouse: MouseEvent) -> bool {
        let sidebar = self.areas().0;
        if let MouseEventKind::Drag(_) = mouse.kind {
            let terminal = self.frame.width;
            self.width
                .drag(mouse.column.saturating_sub(sidebar.x), terminal);
            self.held = Held::Edge { dragged: true };
            self.sync_areas();
            return true;
        }
        self.let_go_edge();
        matches!(mouse.kind, MouseEventKind::Up(_))
    }

    fn let_go_chrome(&mut self) {
        self.let_go_edge();
        if self.held == Held::Taken {
            self.held = Held::Nothing;
        }
        self.sidebar.let_go();
        self.let_go_ask();
    }

    fn cancel_capture(&mut self) -> bool {
        self.let_go_edge();
        self.held = Held::Nothing;
        self.sidebar.let_go();
        self.let_go_ask();
        self.viewed_mut().is_some_and(Viewed::let_go)
    }

    fn let_go_edge(&mut self) {
        let Held::Edge { dragged } = self.held else {
            return;
        };
        self.held = Held::Nothing;
        if dragged && let Err(why) = self.width.keep() {
            self.sidebar
                .notify(format!("the sidebar's width stays in this lune: {why}"));
        }
    }

    fn follow(&mut self, asked: Asked, now: Moment) {
        let wall = now.system;
        match asked {
            Asked::Nothing => {}
            Asked::OpenWorktree { on, project } => self.open_worktree(on, project.id(), wall),
            Asked::Take => self.give(SplitSide::Sidebar, None, wall),
            Asked::Leave => self.leave(wall),
            Asked::Copy(link) => {
                let _ = self.events.send(Event::Copy(Clip::Pull(link)));
            }
            Asked::CopyText(excerpt) => {
                let _ = self.events.send(Event::Copy(Clip::Excerpt(excerpt)));
            }
            Asked::Browse(link) => {
                let _ = self.browse_link(&link, now);
            }
            Asked::Go { on, place } => {
                self.go(&on, &place, wall);
                self.leave(wall);
            }
            Asked::Close { on, act, ticket } => self.project_act(on, act, ticket),
            Asked::Open {
                asking,
                on,
                project,
                beside,
                opens,
            } => {
                self.project_open(asking, &on, project, beside, opens, wall);
            }
            Asked::Welcome {
                asking,
                on,
                session,
                view,
            } => {
                self.welcome(asking, &on, &session, view, wall);
            }
            Asked::Browser {
                asking,
                on,
                place,
                url,
            } => {
                self.open_browser(asking, &on, &place, url, wall);
            }
            Asked::Review {
                asking,
                on,
                session,
                view,
            } => {
                self.review(asking, &on, &session, view, wall);
            }
            Asked::Arrange {
                asking,
                on,
                session,
                view,
                arranged,
            } => {
                self.arrange(asking, &on, &session, view, arranged, wall);
            }
            Asked::OpenIn {
                asking,
                on,
                worktree,
                command,
            } => {
                self.go_worktree(&on, worktree, None, wall);
                self.shown().open_beside_focused(Opens::Command(command));
                self.give_opened(Opener::of(asking), wall);
            }
            Asked::NewWorktree {
                on,
                project,
                branch,
            } => {
                self.make_worktree(on, move |luned, on| {
                    worktrees::branched(luned, on, project, &branch).map(Landing::Worktree)
                });
                self.leave(wall);
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
            Asked::MakeAndShip {
                menu,
                on,
                cwd,
                name,
                harness,
                prompt,
            } => {
                let busy = |busy: &str| format!("menu: {}", worktrees::unopened(&cwd, &name, busy));
                if self.refuse_naming(Some(menu), busy) {
                    return;
                }
                let (luned, events) = (self.luned.clone(), self.events.clone());
                self.naming_on(move || {
                    let said = |unchanged| worktrees::said_unopened(unchanged, &cwd, &name);
                    let made = unmade_caught(PULL_PANICKED, said, || {
                        let pulled = worktrees::pulled(&luned, &on, &cwd, &name, None)?;
                        let worktree = match &pulled {
                            worktrees::Pulled::Went(worktree)
                            | worktrees::Pulled::Opened { worktree, .. }
                            | worktrees::Pulled::Unopened { worktree, .. } => *worktree,
                        };
                        let request = flows::start_request(
                            worktree,
                            FlowMode::Unattended,
                            harness,
                            None,
                            prompt,
                        );
                        let started = Luned::at(&luned, &on, reach::ANSWER)
                            .and_then(|mut client| client.start_flow(request));
                        if let Err(why) = started {
                            let notice =
                                format!("flow: {name} was made, but no flow started: {why}");
                            let _app_gone = events.send(Event::Notice(notice));
                        }
                        Ok(Pulled { on, pulled })
                    });
                    Made::Pulled { menu, made }
                });
            }
            Asked::StartFlow {
                on,
                worktree,
                mode,
                harness,
                prompt,
                go,
            } => {
                if let Some(place) = go {
                    self.go(&on, &place, wall);
                    self.leave(wall);
                }
                self.start_flow(on, worktree, mode, harness, prompt);
            }
            Asked::ControlFlow {
                on,
                worktree,
                control,
            } => {
                self.control_flow(on, worktree, control);
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
                terminal,
                ended,
                ..
            } => {
                let terminal = projected::route_terminal(&on, terminal);
                printed::read(menu, on, terminal, ended, &self.events);
            }
            Asked::Checks { menu, on, page } => checks::fetch(menu, &on, page, &self.events),
            Asked::Brief(brief) => {
                self.briefs += 1;
                let briefing = brief::ask(
                    self.briefs,
                    brief,
                    self.sidebar.kept_harness(),
                    &self.events,
                );
                self.give(self.pane.side(), Some(Over::Brief(briefing)), wall);
            }
            Asked::Ask(target) => {
                self.asks += 1;
                let ask = Ask::new(self.asks, &target, &self.dir);
                self.give(self.pane.side(), Some(Over::Ask(ask)), wall);
            }
            Asked::Issue(repository) => {
                self.issues += 1;
                let picked = self.sidebar.kept_harness();
                let issue =
                    Issue::open(self.issues, repository, picked, &self.unfiled, &self.events);
                self.give(self.pane.side(), Some(Over::Issue(issue)), wall);
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
                        self.sidebar.unsent(ticket, &order, &why, wall);
                    }
                    Err(TrySendError::Disconnected(Move { ticket, order })) => {
                        if let Some(mover) = self.movers.remove(&on) {
                            self.keep_change(mover.stopped());
                        }
                        let why = "the moves have stopped: move it again";
                        self.sidebar.unsent(ticket, &order, why, wall);
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

    fn start_flow(
        &mut self,
        on: Machine,
        worktree: WorktreeId,
        mode: FlowMode,
        harness: FlowHarness,
        prompt: Option<String>,
    ) {
        let (luned, events) = (self.luned.clone(), self.events.clone());
        let request = flows::start_request(worktree, mode, harness, None, prompt);
        self.keep_change(thread::spawn(move || {
            let result = caught(FLOW_PANICKED, || {
                Luned::at(&luned, &on, reach::ANSWER)?.start_flow(request)
            });
            if let Err(why) = result {
                let _ = events.send(Event::Notice(format!("flow: {why}")));
            }
        }));
    }

    fn control_flow(&mut self, on: Machine, worktree: WorktreeId, control: FlowControl) {
        let (luned, events) = (self.luned.clone(), self.events.clone());
        let api_control = control.to_api();
        self.keep_change(thread::spawn(move || {
            let result = caught(FLOW_PANICKED, || {
                Luned::at(&luned, &on, reach::ANSWER)?.control_flow(worktree, api_control)
            });
            if let Err(why) = result {
                let _ = events.send(Event::Notice(format!("flow: {why}")));
            }
        }));
    }

    fn check_flow_transitions(&mut self) {
        let mut current = FlowStates::new();
        let mut transitions: Vec<(String, rows::FlowRow)> = Vec::new();
        for (machine, worktree_id, name, flow) in self.projected.flow_worktrees() {
            let key = (machine, worktree_id);
            let next = (flow.badge_kind, flow.detail.clone());
            if let Some(previous) = self.flow_states.as_ref() {
                let before = previous.get(&key);
                let kind_before = before.map(|(kind, _)| *kind);
                let tells = match flow.badge_kind {
                    FlowBadgeKind::Paused => before != Some(&next),
                    FlowBadgeKind::ConfirmMerge
                    | FlowBadgeKind::Merged
                    | FlowBadgeKind::ReadyToMerge => kind_before != Some(flow.badge_kind),
                    FlowBadgeKind::Running | FlowBadgeKind::WaitingInteractive => false,
                };
                if tells {
                    transitions.push((name.to_owned(), flow.clone()));
                }
            }
            current.insert(key, next);
        }
        self.flow_states = Some(current);
        for (name, flow) in transitions {
            match flow.badge_kind {
                FlowBadgeKind::Paused => {
                    let detail = flow.detail.as_deref().unwrap_or("flow paused");
                    self.sidebar.notify(format!(
                        "flow {name} paused ({}): {detail}",
                        flow.stage_label
                    ));
                    self.desktop.tell(Told::flow(
                        format!("Flow paused — {name}"),
                        format!("{}: {detail}", flow.stage_label),
                    ));
                }
                FlowBadgeKind::ConfirmMerge => {
                    self.sidebar
                        .notify(format!("flow {name}: ready to merge, advance to land it"));
                    self.desktop.tell(Told::flow(
                        format!("Flow ready to merge — {name}"),
                        flow.detail
                            .clone()
                            .unwrap_or_else(|| flow.stage_label.clone()),
                    ));
                }
                FlowBadgeKind::Merged => {
                    self.sidebar.notify(format!("flow {name}: merged"));
                }
                FlowBadgeKind::ReadyToMerge => {
                    self.sidebar.notify(format!("flow {name}: ready to merge"));
                }
                FlowBadgeKind::Running | FlowBadgeKind::WaitingInteractive => {}
            }
        }
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
                Act::View(ViewKey::Browser(view)) => {
                    worktrees::closed_browser_view(&luned, &on, *view)
                }
                Act::Worktree {
                    worktree,
                    seen,
                    seen_browsers,
                    ..
                } => worktrees::closed(&luned, &on, *worktree, seen, seen_browsers),
                Act::Project {
                    project,
                    worktrees,
                    seen,
                    seen_browsers,
                    ..
                } => {
                    worktrees::closed_project(&luned, &on, *project, worktrees, seen, seen_browsers)
                }
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

    fn welcome(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        session: &Listed,
        view: ViewKey,
        now: SystemTime,
    ) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| view_location(project, view));
        let Some((worktree, target)) = held else {
            return self.sidebar.notify(no_view(view));
        };
        self.go_worktree(on, worktree, Some(target), now);
        self.shown().welcome();
        self.give_opened(Opener::of(asking), now);
    }

    fn open_browser(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        place: &menu::BrowserPlace,
        url: Option<BrowserUrl>,
        now: SystemTime,
    ) {
        match place {
            menu::BrowserPlace::View { session, view } => {
                let project = self.projected.project(on, session.project());
                let held = project.and_then(|project| view_location(project, *view));
                let Some((worktree, target)) = held else {
                    return self.sidebar.notify(no_view(*view));
                };
                self.go_worktree(on, worktree, Some(target), now);
            }
            menu::BrowserPlace::Worktree(worktree) => {
                self.go_worktree(on, *worktree, None, now);
            }
        }
        self.shown().open_browser(url);
        self.give_opened(Opener::of(asking), now);
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
    fn browse_link(&mut self, link: &crate::pull::Link, now: Moment) -> bool {
        if self.can_embed_browser() && self.viewed().is_some() {
            match BrowserUrl::parse(&link.url()) {
                Ok(url) => {
                    self.shown().open_browser(Some(url));
                    self.leave(now.system);
                    return true;
                }
                Err(why) => {
                    log::say(&format!("pull link did not parse as browser URL: {why}"));
                }
            }
        }
        match self.browser.open(link, &self.events, now.steady) {
            Ok(()) => false,
            Err(why) => {
                self.sidebar.notify(why);
                true
            }
        }
    }

    fn review(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        session: &Listed,
        view: ViewKey,
        now: SystemTime,
    ) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| view_location(project, view));
        let Some((worktree, target)) = held else {
            return self.sidebar.notify(no_view(view));
        };
        self.go_worktree(on, worktree, Some(target), now);
        self.shown().review();
        self.give_opened(Opener::of(asking), now);
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

    fn open_review_ask(
        &mut self,
        excerpt: &lune_diff::SelectionExcerpt,
        send_now: bool,
        now: SystemTime,
    ) {
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
        let picked = self.sidebar.kept_harness();
        if send_now {
            ask.send(Marked::default(), picked, &self.events);
        }
        self.give(self.pane.side(), Some(Over::Ask(ask)), now);
    }

    fn arrange(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        session: &Listed,
        view: ViewKey,
        arranged: Arranged,
        now: SystemTime,
    ) {
        let project = self.projected.project(on, session.project());
        let held = project.and_then(|project| view_location(project, view));
        let Some((worktree, target)) = held else {
            return self.sidebar.notify(no_view(view));
        };
        self.go_worktree(on, worktree, Some(target), now);
        self.shown().arrange(arranged);
        self.give_opened(Opener::of(asking), now);
    }

    fn project_open(
        &mut self,
        asking: sidebar::Held,
        on: &Machine,
        project: ProjectId,
        beside: ViewKey,
        opens: Opens,
        now: SystemTime,
    ) {
        let Some(project) = self.projected.project(on, project) else {
            return self
                .sidebar
                .notify("menu: luned keeps no such project".to_owned());
        };
        let Some((worktree, target)) = view_location(project, beside) else {
            return self.sidebar.notify(no_view(beside));
        };
        self.go_worktree(on, worktree, Some(target), now);
        self.shown().open_beside_focused(opens);
        self.give_opened(Opener::of(asking), now);
    }

    fn open_worktree(&mut self, on: Machine, project: ProjectId, now: SystemTime) {
        self.pickers += 1;
        let (listing, palette) = (self.sidebar.listing(), self.palette());
        let picker = Box::new(Picker::opening(
            self.pickers,
            listing,
            (on, project),
            palette,
        ));
        self.give(self.pane.side(), Some(Over::Picker(picker)), now);
    }

    fn picked(&mut self, picked: Picked, now: Moment) {
        let wall = now.system;
        match picked {
            Picked::Nothing => {}
            Picked::Close => self.give(self.pane.side(), None, wall),
            Picked::Go { on, place } => {
                self.go(&on, &place, wall);
                self.give(self.pane.side().session(), None, wall);
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
                self.give(self.pane.side().session(), None, wall);
            }
            Picked::Make { on, folder } => {
                self.make_worktree(on, move |luned, on| worktrees::made_at(luned, on, &folder));
                self.give(self.pane.side().session(), None, wall);
            }
            Picked::Plan { on, session, scope } => {
                let plan = self.sidebar.plan(&on, &session, scope);
                if let Some(Over::Picker(picker)) = &mut self.over {
                    let did = picker.planned(plan);
                    self.picked(did, now);
                }
            }
            Picked::Act { on, act } => {
                let ticket = self.sidebar.ticket();
                self.project_act(on, act, ticket);
                self.give(self.pane.side().session(), None, wall);
            }
            Picked::Chord(chord) => {
                self.give(self.pane.side(), None, wall);
                match chord {
                    aloud::KEY => {
                        let _ = self.read_aloud();
                    }
                    chord if aloud::stops(chord) => {
                        let _ = self.stop_aloud();
                    }
                    chord => {
                        let ran = self.chord(chord, wall);
                        assert!(ran, "every chord the palette lists is one App::chord runs");
                    }
                }
            }
            Picked::Menu {
                target,
                origin,
                action,
            } => {
                self.give(self.pane.side(), None, wall);
                let asked = self.sidebar.act_in_menu(&target, action, origin);
                self.follow(asked, now);
            }
            Picked::Pull {
                target,
                work,
                pull,
                origin,
            } => {
                self.give(self.pane.side(), None, wall);
                let asked = self.sidebar.pull_in_menu(target, work, pull, origin);
                self.follow(asked, now);
            }
            Picked::Row { target, action } => {
                self.give(self.pane.side(), None, wall);
                let asked = self.sidebar.act_on_row(&target, action, wall);
                self.follow(asked, now);
            }
            Picked::Done { done, origin } => {
                self.give(self.pane.side(), None, wall);
                let asked = self.sidebar.chained(done, origin);
                self.follow(asked, now);
            }
        }
    }

    fn converse(&mut self) {
        let picked = self.sidebar.kept_harness();
        if let Some(Over::Ask(ask)) = &mut self.over {
            ask.send(Marked::default(), picked, &self.events);
        }
    }

    fn file_issue(&mut self, now: SystemTime) {
        let open = self.over.take_if(|over| matches!(over, Over::Issue(_)));
        let Some(Over::Issue(issue)) = open else {
            return;
        };
        if let Some((number, to, filing)) = issue.send(&self.events, &mut self.unfiled) {
            self.sidebar.filing(number, &to.to_string());
            self.keep_change(filing);
        }
        self.give(self.pane.side(), None, now);
    }

    fn close_issue(&mut self, now: SystemTime) {
        let open = self.over.take_if(|over| matches!(over, Over::Issue(_)));
        if let Some(Over::Issue(issue)) = open {
            issue.close(&mut self.unfiled);
        }
        self.give(self.pane.side(), None, now);
    }

    fn take_dictation_target(&mut self) -> Option<Where> {
        let waiting_on_session = matches!(
            (self.holder(), self.pane.side()),
            (
                Holder::Sidebar(Holding::Afloat),
                SplitSide::Session | SplitSide::Collapsed
            )
        ) && self.sidebar.floating_waits();
        let on_session = matches!(self.holder(), Holder::Terminal) || waiting_on_session;
        match self.over.take() {
            None if on_session => {
                if waiting_on_session {
                    let left = self.sidebar.leave_floating_waiting().left();
                    assert!(left, "a waiting floating menu was left before dictation");
                }
                assert!(
                    matches!(self.holder(), Holder::Terminal),
                    "session dictation starts with the terminal holding focus"
                );
                self.unsweep_ask();
                Some(Where::Session(self.sidebar.here_agent()))
            }
            Some(Over::Ask(mut ask)) if !ask.turning() => {
                ask.clear_selection();
                Some(Where::Ask(ask))
            }
            Some(Over::Issue(issue)) => Some(Where::Issue(issue)),
            other => {
                self.over = other;
                None
            }
        }
    }

    fn dictate(&mut self, now: Moment) -> bool {
        let Some(under) = self.take_dictation_target() else {
            return false;
        };
        assert!(
            self.over.is_none(),
            "dictation takes any prior overlay before opening"
        );
        assert!(
            !self.sidebar.floating_waits(),
            "no waiting floating menu remains open once dictation starts"
        );
        self.dictations += 1;
        let number = self.dictations;
        let dictation = if speech::fetched() {
            let skills = self.nameable(number, &under);
            if let Skills::Failed(why) = &skills {
                self.sidebar.notify(why.clone());
            }
            let speech = self.speech.get_or_insert_with(Speech::start);
            let listening = speech.listen(number, &self.events);
            Dictation::hearing(number, listening, skills, self.releases, now.steady)
        } else {
            let events = &self.events;
            let fetching = || Fetch {
                _asking: speech::fetch(events),
                got: 0,
            };
            Dictation::fetching(number, self.fetching.get_or_insert_with(fetching).got)
        };
        let over = Some(Over::Dictation(dictation, under));
        self.give(self.pane.side(), over, now.system);
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
        let here = matches!(self.over, Some(Over::Dictation(..)));
        self.desktop.dictating(here);
        self.aloud.hold(here || self.desktop.another_dictates());
    }

    fn undictate(&mut self, dictated: Option<Dictated>, now: SystemTime) {
        let open = self
            .over
            .take_if(|over| matches!(over, Over::Dictation(..)));
        let Some(Over::Dictation(_, under)) = open else {
            return;
        };
        let side = self.pane.side();
        match under {
            Where::Session(agent) => {
                self.give(side, None, now);
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
                    let _ = self
                        .key_to_session(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now);
                }
            }
            Where::Ask(mut ask) => {
                if let Some(dictated) = &dictated {
                    ask.dictated(&dictated.written.to_string());
                }
                self.give(side, Some(Over::Ask(ask)), now);
                if dictated.is_some_and(|dictated| dictated.sends) {
                    self.converse();
                }
            }
            Where::Issue(mut issue) => {
                if let Some(dictated) = &dictated {
                    issue.dictated(&dictated.written.to_string());
                }
                let sends = dictated.is_some_and(|dictated| dictated.sends) && issue.ready();
                self.give(side, Some(Over::Issue(issue)), now);
                if sends {
                    self.file_issue(now);
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
                let shown = matches!(self.over, Some(Over::Dictation(..)));
                if let (Err(why), false) = (ended, shown) {
                    self.sidebar
                        .notify(format!("the speech model did not come: {why}"));
                    dirty = true;
                }
            }
        }
        if let Some(Over::Dictation(dictation, _)) = &mut self.over {
            dictation.fetched(fetched);
            dirty = true;
        }
        dirty
    }

    fn step(&mut self, direction: Direction, now: SystemTime) {
        assert!(self.over.is_none(), "a box takes the key before a step");
        match (self.pane.side(), direction) {
            (SplitSide::Sidebar, Direction::Left) => {}
            (SplitSide::Sidebar, Direction::Right) => self.leave(now),
            (SplitSide::Session | SplitSide::Collapsed, Direction::Left) => {
                if self.home().is_some() {
                    self.step_to_sidebar(now);
                } else {
                    self.step_asked(direction, now);
                }
            }
            (SplitSide::Sidebar, Direction::Up | Direction::Down)
            | (
                SplitSide::Session | SplitSide::Collapsed,
                Direction::Right | Direction::Up | Direction::Down,
            ) => self.step_asked(direction, now),
        }
    }

    fn step_asked(&mut self, direction: Direction, now: SystemTime) {
        self.leave(now);
        if let Some(right) = self.viewed_mut() {
            right.step(direction);
        }
    }

    fn step_to_sidebar(&mut self, now: SystemTime) {
        self.give(SplitSide::Sidebar, None, now);
        let held = matches!(self.holder(), Holder::Sidebar(_));
        assert!(
            held,
            "the sidebar holds the keys past the project's left edge"
        );
    }

    #[must_use]
    fn cross(&mut self, across: Across, now: SystemTime) -> bool {
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
                self.go_worktree(&on, beside, None, now);
                true
            }
            (Across::Before, None, _) => {
                self.step_to_sidebar(now);
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

    fn go(&mut self, machine: &Machine, place: &Place, now: SystemTime) -> bool {
        let gone = self.go_project(machine, place, now);
        if let Err(why) = &gone {
            self.sidebar.notify(why.clone());
        }
        gone.is_ok()
    }

    fn go_project(
        &mut self,
        machine: &Machine,
        place: &Place,
        now: SystemTime,
    ) -> Result<(), String> {
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
            Place::Session(_) => self.go_home(machine, project.id, now),
            Place::Worktree(place) => {
                let held = project.worktrees().iter().any(|it| it.id == place.worktree);
                if !held {
                    return Err(format!("{session} holds no such worktree"));
                }
                if !shows(place.worktree) {
                    self.go_worktree(machine, place.worktree, None, now);
                }
            }
            Place::View(place) => {
                let view_key = place.view();
                let (worktree, view) = view_location(project, view_key)
                    .ok_or_else(|| format!("luned lists no view {view_key} in {session}"))?;
                self.go_worktree(machine, worktree, Some(view), now);
            }
        }
        Ok(())
    }

    fn go_worktree(
        &mut self,
        on: &Machine,
        worktree: WorktreeId,
        terminal: Option<ViewId>,
        now: SystemTime,
    ) {
        if let Some(right) = self.viewed_mut() {
            let deletes = right.go(on, worktree);
            self.pending_graphics.extend(deletes);
        } else {
            self.unshow_right();
            let (size, greeting) = (layout_size(self.areas().1), greeting(self.colours));
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
            self.replace_right(Right::Worktree(started));
        }
        let right = self.shown();
        assert_eq!(right.machine(), on, "lune shows the machine it went to");
        assert_eq!(
            right.worktree(),
            worktree,
            "lune shows the worktree it went to"
        );
        if let Some(terminal) = terminal {
            right.focus_view(terminal);
        }
        self.seat(now);
        self.record_usage();
        let _ = self.sync_tabs();
    }

    fn shown(&mut self) -> &mut Viewed {
        self.viewed_mut().expect("lune went to a worktree")
    }

    fn leave_view(&mut self, now: SystemTime) {
        let (on, left) = match self.pane.right() {
            Some(Right::Worktree(right)) => (right.machine().clone(), Some(right.worktree())),
            Some(Right::Home(shown)) => (shown.on.clone(), None),
            None => return,
        };
        self.go_other_than(&on, left, now);
    }

    fn go_other_than(&mut self, on: &Machine, left: Option<WorktreeId>, now: SystemTime) {
        let held = |machine: &Machine| {
            let projects = self.projected.projects(machine).iter();
            let worktrees = projects.flat_map(rows::Project::worktrees);
            let open = worktrees.filter(|it| !it.views.is_empty());
            let others = open.filter(|it| machine != on || Some(it.id) != left);
            projects::last_used(others.map(|it| (it.id, it.used)))
        };
        let other = held(on).map(|id| (on.clone(), id));
        let other = other.or_else(|| held(&Machine::Local).map(|id| (Machine::Local, id)));
        match other {
            Some((machine, other)) => self.go_worktree(&machine, other, None, now),
            None => self.unview(now),
        }
    }

    fn unview(&mut self, now: SystemTime) {
        self.drop_shown();
        self.sidebar.unseat(now);
        self.give(SplitSide::Sidebar, None, now);
    }

    fn seat(&mut self, now: SystemTime) {
        let right = match self.pane.right() {
            None => return,
            Some(Right::Home(shown)) => {
                let (on, project) = (shown.on.clone(), shown.project);
                if self.projected.project(&on, project).is_some() {
                    self.sidebar.sit(
                        Seat {
                            on,
                            project,
                            at: Sitting::Home,
                        },
                        now,
                    );
                } else {
                    self.sidebar.unseat(now);
                }
                return;
            }
            Some(Right::Worktree(right)) => right,
        };
        let (on, worktree) = (right.machine().clone(), right.worktree());
        let Some((project, shown_worktree)) = self.projected.holding(&on, worktree) else {
            return;
        };
        let view = right
            .focused()
            .and_then(|focused| match shown_worktree.view(focused)? {
                rows::View::Terminal(terminal) => terminal.key().map(ViewKey::Terminal),
                rows::View::Browser(browser) => Some(ViewKey::Browser(browser.id)),
            });
        let layout = match (right.arranged(), view) {
            (Some((arranged, version)), view) => SeatedLayout::Live {
                arranged,
                version,
                view,
            },
            (None, Some(view)) => {
                assert!(
                    !right.live(),
                    "a live worktree with a focused view has an arrangement"
                );
                SeatedLayout::Disconnected(view)
            }
            (None, None) => SeatedLayout::Unlaid,
        };
        let at = Sitting::Worktree { worktree, layout };
        self.sidebar.sit(
            Seat {
                on,
                project: project.id,
                at,
            },
            now,
        );
    }

    #[must_use]
    fn heeded(&mut self, heeded: Heeded, now: SystemTime) -> bool {
        let dirty = match heeded {
            Heeded::Quiet => return false,
            Heeded::Changed | Heeded::Followed => true,
            Heeded::Edged(across) => self.cross(across, now),
            Heeded::Sent(worktree) => {
                let Some(right) = self.viewed() else {
                    return false;
                };
                let on = right.machine().clone();
                self.go_worktree(&on, worktree, None, now);
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
                self.leave_view(now);
                true
            }
            Heeded::Forgotten(why) => {
                self.leave_view(now);
                self.sidebar.notify(format!("luned's layout: {why}"));
                true
            }
            Heeded::Lost(why) => {
                self.sidebar.notify(format!("luned's layout: {why}"));
                true
            }
        };
        self.seat(now);
        self.record_usage();
        dirty | self.sync_tabs()
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

    fn refuse_naming(&mut self, menu: Option<MenuId>, said: impl FnOnce(&str) -> String) -> bool {
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

    fn named(&mut self, made: Made, now: SystemTime) {
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
                self.go_worktree(&on, made.worktree(), None, now);
                self.give_opened(Opener::LetGo, now);
            }
            Made::Worktree {
                on,
                made: Ok(Landing::Unopened(project)),
            } => {
                self.go_home(&on, project, now);
                self.give_opened(Opener::LetGo, now);
            }
            Made::Worktree { made: Err(why), .. } => {
                self.sidebar.notify(why);
            }
            Made::Pulled { menu, made } => self.pulled_made(menu, made, now),
        }
    }

    fn pulled_made(&mut self, menu: MenuId, made: Result<Pulled, String>, now: SystemTime) {
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
        self.go_worktree(&on, worktree, focus, now);
        self.give_opened(opener, now);
        if let Some(why) = unrun {
            self.sidebar.notify(format!("menu: {why}"));
        }
    }

    fn give_opened(&mut self, opener: Opener, now: SystemTime) {
        assert!(self.pane.right().is_some(), "lune shows the view it opened");
        match (opener, self.holder()) {
            (Opener::Holds, _) => {
                self.leave(now);
                let afloat = matches!(self.holder(), Holder::Sidebar(Holding::Afloat));
                assert!(
                    !afloat,
                    "the menu that opened the view holds the keys no longer"
                );
            }
            (Opener::LetGo, Holder::Terminal | Holder::Sidebar(Holding::Nothing)) => {
                self.give(self.pane.side().session(), None, now);
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
        Holder::of(self.pane.side(), self.over.as_ref(), self.sidebar.holding())
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

    fn areas(&self) -> (Rect, Rect, Rect) {
        let own = if self.pane.collapsed() {
            sidebar::COLLAPSED
        } else {
            self.width.columns()
        };
        areas(self.frame, own)
    }

    fn sync_areas(&mut self) {
        let (sidebar, right, _) = self.areas();
        if self.pane.collapsed() {
            self.sidebar.collapse(sidebar);
        } else {
            self.sidebar.place(sidebar);
        }
        let size = window(right);
        if let Some(right) = self.viewed_mut() {
            right.resize(size.columns, size.lines);
        }
    }

    fn resize_frame(&mut self, frame: Rect) {
        if frame == self.frame {
            return;
        }
        self.frame = frame;
        self.sync_areas();
    }

    fn give(&mut self, side: SplitSide, over: Option<Over>, wall: SystemTime) {
        self.pane.set_side(side);
        self.over = over;
        let collapsed = self.pane.collapsed();
        if collapsed || self.over.is_some() {
            self.let_go_chrome();
        }
        if let Some(right) = self.viewed_mut() {
            right.forget_step();
        }
        if collapsed {
            self.sync_areas();
        }
        self.sidebar.focus(self.pane.keys(self.over.as_ref()), wall);
        assert!(
            self.sidebar.floating().is_none(),
            "a focus change closes what floats"
        );
        if !collapsed {
            self.sync_areas();
        }
        self.hold_aloud();
        self.tell();
    }

    fn leave(&mut self, wall: SystemTime) {
        let leaves = match self.holder() {
            Holder::Sidebar(_) => true,
            Holder::Terminal | Holder::Box(_) => false,
        };
        if leaves {
            self.give(self.pane.side().session(), None, wall);
        }
    }

    #[must_use]
    fn focus_window(&mut self, focused: bool) -> bool {
        self.focused = focused;
        self.desktop.focus(focused);
        self.focus_alerts();
        let mut dirty = false;
        if !focused
            && let Some(Over::Dictation(dictation, _)) = &mut self.over
            && dictation.unfocused()
        {
            dirty = true;
        }
        if !focused {
            let hover_before = self.hover();
            let was_pressed = self.sidebar.pressed()
                || matches!(self.held, Held::Edge { .. })
                || self.ask_pressed();
            self.pointer = None;
            let unselected = self.cancel_capture();
            if was_pressed || unselected || self.hover() != hover_before {
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
                let cols = self.frame.width;
                let rows = self.frame.height;
                let cell = embedded::cell_pixels(self.exact_cell, Some(window_pixels), cols, rows);
                if cell != self.cell {
                    self.cell = cell;
                    if let Some(right) = self.viewed_mut() {
                        right.cell_resized(cell);
                    }
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
            painted: Box::new(painted),
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
        terminal.autoresize()?;
        let area = terminal.current_buffer_mut().area;
        self.resize_frame(area);
        let mut graphics = std::mem::take(&mut self.pending_graphics);
        if let Some(right) = self.viewed_mut() {
            graphics.extend(right.drain_graphics());
        }
        if !graphics.is_empty() {
            terminal.backend_mut().write_all(&graphics)?;
        }
        let (painted, cursor_position, aloud_buttons) = {
            let mut frame = terminal.get_frame();
            self.paint_at(&mut frame, now)
        };
        terminal.apply_buffer_with_cursor(cursor_position)?;
        self.drawn_aloud = aloud_buttons;
        self.settling.drawn(&self.drawn_aloud, now.steady);
        self.paint_graphics(terminal.backend_mut())?;
        let cursor = self.viewed().map_or(UNSHOWN_CURSOR, Viewed::cursor_style);
        if self.cursor != Some(cursor) {
            execute!(terminal.backend_mut(), cursor_style(cursor))?;
            self.cursor = Some(cursor);
        }
        let (_, right_area, _) = self.areas();
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
        let now = Moment::now();
        let _ = self.sync_aloud();
        self.resize_frame(frame.area());
        let (painted, _, aloud_buttons) = self.paint_at(frame, now);
        self.drawn_aloud = aloud_buttons;
        self.settling.drawn(&self.drawn_aloud, now.steady);
        self.pacing = Pacing::Drawn {
            dirty: false,
            painted: Box::new(painted),
        };
    }

    fn paint_at(
        &mut self,
        frame: &mut Frame<'_>,
        now: Moment,
    ) -> (Painted, Option<Position>, Spots<aloud::Button>) {
        let mut cadence = Cadence::default();
        let frame_area = frame.area();
        assert_eq!(
            frame_area, self.frame,
            "the app's layout matches the frame it paints"
        );
        let (expected_sidebar, expected_right, expected_bar) = self.areas();
        let (top_clean, bar) = attached::Clean::of_frame(frame).trim_bottom(expected_bar.height);
        assert_eq!(
            bar, expected_bar,
            "the painted status bar matches its laid-out area"
        );
        let ((sidebar, beside, sidebar_spinning), right_clean) =
            top_clean.paint_left(expected_sidebar.width, |buffer, sidebar| {
                assert_eq!(
                    sidebar, expected_sidebar,
                    "the painted sidebar matches the laid-out area"
                );
                let (drawn, sidebar_cadence) = self.sidebar.draw(buffer, sidebar, now.system);
                let spinning = sidebar_cadence.spinning();
                cadence.merge(sidebar_cadence);
                (sidebar, drawn, spinning)
            });
        let right = right_clean.area();
        assert_eq!(
            (sidebar, right),
            (expected_sidebar, expected_right),
            "the painted areas match the laid-out areas"
        );
        let covered = self.covered();
        let uncovered = |at: &Position| !covered && right.contains(*at);
        let spot = |at: Position| attached::Spot {
            row: at.y - right.y,
            column: at.x - right.x,
        };
        let hover = self.pointer.filter(uncovered).map(spot);
        let kitty_cell_px = self.ribbons.cell_px(frame_area, covered);
        let (cursor, overlays) = match self.pane.right_mut() {
            Some(Right::Worktree(shown)) => shown.draw(right_clean, hover, kitty_cell_px),
            Some(Right::Home(shown)) => {
                let (buffer, right) = right_clean.into_buffer();
                let home = self.homes.shown_mut(shown);
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
        let drawn = match &mut self.over {
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
                Some(Floating::Waiting(menu)) => {
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
        let bar_snapshot = self.status_bar_snapshot(now.system);
        let bar_spinner = if sidebar_spinning {
            sidebar::frame(now.system)
        } else {
            SpinnerFrame::REST
        };
        let (bar_links, bar_cadence) =
            statusbar::draw(buffer, bar, &bar_snapshot, bar_spinner, now.system);
        cadence.merge(bar_cadence);
        let aloud_buttons = self.sidebar.aloud_buttons().clone();
        if let Some(pointer) = self.pointer {
            drawn.light(buffer, pointer);
            aloud_buttons.light(buffer, pointer);
            if !covered {
                bar_links.raise(buffer, pointer);
            }
        }
        let cursor_position = matches!(self.holder(), Holder::Terminal)
            .then_some(cursor)
            .flatten();
        if let Some(cursor) = cursor_position {
            frame.set_cursor_position(cursor);
        }
        self.drawn = drawn;
        self.drawn_bar_links = bar_links;
        let painted = Painted {
            at: now,
            cadence,
            sidebar_view: self.sidebar.view(),
            status_bar: bar_snapshot,
        };
        (painted, cursor_position, aloud_buttons)
    }
}

fn view_location(project: &rows::Project, view: ViewKey) -> Option<(WorktreeId, ViewId)> {
    match view {
        ViewKey::Terminal(key) => project.view_of(key),
        ViewKey::Browser(id) => project.browser_of(id),
    }
}

fn no_view(view: ViewKey) -> String {
    match view {
        ViewKey::Terminal(key) => format!("menu: luned lists no view {}", key.id),
        ViewKey::Browser(_) => "menu: luned lists no browser view".to_owned(),
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

fn ignores_repeat(chord: Chord) -> bool {
    chord == dictation::KEY || chord == aloud::KEY || revives(chord)
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

        let collapsed_at = Duration::from_secs(4);
        app.go_worktree(
            &Machine::Local,
            WorktreeId::of(100).unwrap(),
            None,
            at(collapsed_at).system,
        );
        app.give(SplitSide::Collapsed, None, at(collapsed_at).system);
        app.pacing.mark_dirty();
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
                Event::Daemon(Ok(busy_host(8, "step 2"))),
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

        let renamed_at = collapsed_at + Duration::from_secs(2);
        app.handle_at(
            Event::Daemon(Ok(busy_host(8, "live topic"))),
            &mut terminal,
            at(renamed_at),
        )
        .unwrap();
        app.tick(at(renamed_at));
        assert!(
            drew(&mut app, &mut terminal, &written, at(renamed_at)),
            "a live topic update on the shown worktree redraws its view tab"
        );
        written.borrow_mut().clear();

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
        let shown_at = Duration::from_secs(10);
        app.give(SplitSide::Sidebar, None, at(shown_at).system);
        app.pacing.mark_dirty();
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
        app.over = Some(Over::Dictation(dictation, Where::Session(None)));
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

    fn start_test_pull(app: &mut App, url: &str, pull: menu::Pull, origin: Origin) {
        let target = match app.sidebar.aimed_row() {
            picker::Aim::At { target, .. } => (*target).clone(),
            picker::Aim::Unplaced(why) => panic!("expected an aimed target: {why}"),
        };
        let work = luned::links::Work::parse(url, None).expect("valid GitHub link");
        let asked = app.sidebar.pull_in_menu(target, work, pull, origin);
        assert!(matches!(asked, Asked::Pull { .. }), "{asked:?}");
    }

    fn press_key(
        app: &mut App,
        terminal: &mut TestTerminal,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: Moment,
    ) {
        let key = KeyEvent::new(code, modifiers);
        app.handle_at(Event::Input(Input::Key(key)), terminal, now)
            .unwrap();
    }

    fn home_prompt_text(app: &mut App, system: SystemTime) -> String {
        let area = Rect::new(0, 0, 50, 14);
        let mut buffer = Buffer::empty(area);
        let Some(Right::Home(shown)) = app.pane.right() else {
            panic!("home is shown");
        };
        let _ = app.homes.shown_mut(shown).draw(&mut buffer, area, system);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn keyboard_and_paste_stay_responsive_while_tackle_and_worktree_load() {
        use crate::actions::PALETTE;
        use crate::menu::Pull;

        let (mut terminal, written) = test_terminal(80, 24);
        let (mut app, _inbox, _state) = test_app(80, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let mut tick = 0u64;
        let mut next = || {
            tick += 1;
            let offset = Duration::from_secs(tick);
            Moment {
                steady: start + offset,
                system: system + offset,
            }
        };

        let now = next();
        app.handle_at(Event::Daemon(Ok(busy_host(2, "work"))), &mut terminal, now)
            .unwrap();
        app.go_home(&Machine::Local, ProjectId::of(1).unwrap(), now.system);
        app.give(SplitSide::Session, None, now.system);
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/1",
            Pull::Agent,
            Origin::Views,
        );
        assert!(app.sidebar.floating_waits());
        app.pacing.mark_dirty();
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        written.borrow_mut().clear();

        let now = next();
        press_key(&mut app, &mut terminal, SWAP.0, SWAP.1, now);
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "Ctrl+a switches to sidebar while tackle loads"
        );
        assert!(app.sidebar.floating().is_none());
        assert!(
            app.sidebar.turning(),
            "tackle keeps running in the background"
        );
        assert!(
            drew(&mut app, &mut terminal, &written, now),
            "dismissing the box redraws"
        );

        let before_step = app.sidebar.aimed_row();
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/2",
            Pull::Agent,
            Origin::Sidebar,
        );
        assert_eq!(app.sidebar.holding(), Holding::Menu);
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            now,
        );
        assert_ne!(
            app.sidebar.aimed_row(),
            before_step,
            "j moves sidebar while tackle loads"
        );
        assert_eq!(app.sidebar.holding(), Holding::Nothing);

        app.give(SplitSide::Session, None, now.system);
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/3",
            Pull::Worktree,
            Origin::Views,
        );
        let now = next();
        press_key(&mut app, &mut terminal, PALETTE.0, PALETTE.1, now);
        assert!(matches!(app.over, Some(Over::Picker(_))));
        assert!(app.sidebar.floating().is_none());
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Esc,
            KeyModifiers::NONE,
            now,
        );
        assert!(app.over.is_none());

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/4",
            Pull::Worktree,
            Origin::Views,
        );
        let now = next();
        press_key(&mut app, &mut terminal, COLLAPSE.0, COLLAPSE.1, now);
        assert_eq!(app.pane.side(), SplitSide::Collapsed);
        assert!(app.sidebar.floating().is_none());

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/5",
            Pull::Agent,
            Origin::Views,
        );
        let now = next();
        press_key(&mut app, &mut terminal, MENU.0, MENU.1, now);
        assert!(app.sidebar.floating().is_some());
        assert!(!app.sidebar.floating_waits());
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Esc,
            KeyModifiers::NONE,
            now,
        );

        app.give(SplitSide::Session, None, now.system);
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/6",
            Pull::Agent,
            Origin::Views,
        );
        let now = next();
        press_key(&mut app, &mut terminal, LEFT.0, LEFT.1, now);
        assert_eq!(app.pane.side(), SplitSide::Sidebar);

        app.give(SplitSide::Session, None, now.system);
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/7",
            Pull::Agent,
            Origin::Views,
        );
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            now,
        );
        assert!(app.sidebar.floating().is_none());
        let now = next();
        app.handle_at(
            Event::Input(Input::Paste("bc".to_owned())),
            &mut terminal,
            now,
        )
        .unwrap();
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/8",
            Pull::Worktree,
            Origin::Views,
        );
        let now = next();
        app.handle_at(
            Event::Input(Input::Paste("d".to_owned())),
            &mut terminal,
            now,
        )
        .unwrap();
        assert!(app.sidebar.floating().is_none());
        assert!(home_prompt_text(&mut app, system).contains("abcd"));

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/9",
            Pull::Worktree,
            Origin::Views,
        );
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Esc,
            KeyModifiers::NONE,
            now,
        );
        assert!(app.sidebar.floating().is_none());
        assert_eq!(app.pane.side(), SplitSide::Session);
        assert!(home_prompt_text(&mut app, system).contains("abcd"));
    }

    #[test]
    fn mouse_clicks_and_scrolls_pass_through_while_tackle_or_worktree_loads() {
        use crate::menu::Pull;

        let (mut terminal, written) = test_terminal(80, 24);
        let (mut app, _inbox, _state) = test_app(80, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let mut tick = 0u64;
        let mut next = || {
            tick += 1;
            let offset = Duration::from_secs(tick);
            Moment {
                steady: start + offset,
                system: system + offset,
            }
        };

        let now = next();
        app.handle_at(Event::Daemon(Ok(busy_host(2, "work"))), &mut terminal, now)
            .unwrap();
        app.go_home(&Machine::Local, ProjectId::of(1).unwrap(), now.system);
        app.give(SplitSide::Session, None, now.system);
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/10",
            Pull::Agent,
            Origin::Views,
        );
        app.pacing.mark_dirty();
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        let boxed = app.drawn.boxed().expect("floating waiting box is drawn");

        let send_mouse = |app: &mut App,
                          terminal: &mut TestTerminal,
                          kind: MouseEventKind,
                          column: u16,
                          row: u16,
                          now: Moment| {
            let mouse = MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            };
            app.handle_at(Event::Input(Input::Mouse(mouse)), terminal, now)
                .unwrap();
        };

        let now = next();
        send_mouse(&mut app, &mut terminal, MouseEventKind::Moved, 1, 1, now);
        assert!(
            app.sidebar.floating_waits(),
            "hovering does not dismiss the waiting box"
        );

        let now = next();
        send_mouse(
            &mut app,
            &mut terminal,
            MouseEventKind::Down(MouseButton::Left),
            boxed.x,
            boxed.y,
            now,
        );
        assert!(
            app.sidebar.floating_waits(),
            "clicking the waiting box frame stays put"
        );

        let now = next();
        send_mouse(
            &mut app,
            &mut terminal,
            MouseEventKind::ScrollDown,
            1,
            1,
            now,
        );
        assert!(
            app.sidebar.floating().is_none(),
            "scrolling dismisses the waiting box"
        );

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/11",
            Pull::Worktree,
            Origin::Views,
        );
        app.pacing.mark_dirty();
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        let now = next();
        send_mouse(
            &mut app,
            &mut terminal,
            MouseEventKind::Down(MouseButton::Left),
            1,
            2,
            now,
        );
        let now = next();
        send_mouse(
            &mut app,
            &mut terminal,
            MouseEventKind::Up(MouseButton::Left),
            1,
            2,
            now,
        );
        assert!(
            app.sidebar.floating().is_none(),
            "clicking outside dismisses the waiting box"
        );
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "clicking the sidebar focuses it on first click"
        );
    }

    #[test]
    fn waiting_floating_menus_keep_modifiers_protect_interactive_paste_and_allow_dictation() {
        use crate::menu::Pull;
        use crossterm::event::ModifierKeyCode;

        let (mut terminal, written) = test_terminal(80, 24);
        let (mut app, _inbox, _state) = test_app(80, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let mut tick = 0u64;
        let mut next = || {
            tick += 1;
            let offset = Duration::from_secs(tick);
            Moment {
                steady: start + offset,
                system: system + offset,
            }
        };

        let now = next();
        app.handle_at(Event::Daemon(Ok(busy_host(2, "work"))), &mut terminal, now)
            .unwrap();
        app.go_home(&Machine::Local, ProjectId::of(1).unwrap(), now.system);
        app.give(SplitSide::Session, None, now.system);
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/20",
            Pull::Agent,
            Origin::Views,
        );
        app.pacing.mark_dirty();
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        written.borrow_mut().clear();

        let now = next();
        let repeat_enter =
            KeyEvent::new_with_kind(KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat);
        app.handle_at(Event::Input(Input::Key(repeat_enter)), &mut terminal, now)
            .unwrap();
        assert!(
            app.sidebar.floating_waits(),
            "a repeated key leaves a floating waiting menu open"
        );
        assert!(
            !drew(&mut app, &mut terminal, &written, now),
            "a repeated key does not dirty the frame"
        );

        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Modifier(ModifierKeyCode::LeftShift),
            KeyModifiers::NONE,
            now,
        );
        assert!(
            app.sidebar.floating_waits(),
            "a bare modifier leaves a floating waiting menu open"
        );
        assert!(
            !drew(&mut app, &mut terminal, &written, now),
            "a bare modifier does not dirty the frame"
        );

        assert!(
            matches!(app.take_dictation_target(), Some(Where::Session(_))),
            "dictation starts on Session while a waiting menu floats"
        );
        assert!(app.sidebar.floating().is_none());
        assert!(app.sidebar.turning());

        app.give(SplitSide::Collapsed, None, now.system);
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/21",
            Pull::Agent,
            Origin::Views,
        );
        assert!(
            matches!(app.take_dictation_target(), Some(Where::Session(_))),
            "dictation starts on Collapsed while a waiting menu floats"
        );
        assert!(app.sidebar.floating().is_none());

        app.give(SplitSide::Sidebar, None, now.system);
        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/issues/22",
            Pull::Agent,
            Origin::Views,
        );
        app.sidebar.notify("keep me".to_owned());
        assert!(
            app.take_dictation_target().is_none(),
            "dictation does not start while the sidebar has focus"
        );
        assert!(app.sidebar.floating_waits());
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            dictation::KEY.0,
            dictation::KEY.1,
            now,
        );
        assert!(
            app.sidebar.floating_waits(),
            "pressing dictation key while sidebar has focus leaves waiting floating menu open"
        );
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            now,
        );
        assert!(
            app.sidebar.floating_waits(),
            "pressing an unbound modified key on sidebar leaves waiting floating menu open"
        );
        let sidebar_area = app.areas().0;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let _ = app.sidebar.draw(&mut buffer, sidebar_area, now.system);
        let sidebar_text: String = (sidebar_area.top()..sidebar_area.bottom())
            .map(|y| {
                (sidebar_area.left()..sidebar_area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(
            sidebar_text.contains("keep me"),
            "ignored modified keys on sidebar preserve an existing notice: {sidebar_text}"
        );

        let now = next();
        app.handle_at(
            Event::Pruned {
                menu: MenuId::of(999),
                pruned: Err("boom".to_owned()),
            },
            &mut terminal,
            now,
        )
        .unwrap();
        let before_step = app.sidebar.aimed_row();
        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            now,
        );
        assert!(app.sidebar.floating().is_none());
        assert_ne!(app.sidebar.aimed_row(), before_step);
        let sidebar_area = app.areas().0;
        let mut buffer = Buffer::empty(Rect::new(0, 0, 80, 24));
        let _ = app.sidebar.draw(&mut buffer, sidebar_area, now.system);
        let sidebar_text: String = (sidebar_area.top()..sidebar_area.bottom())
            .map(|y| {
                (sidebar_area.left()..sidebar_area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect();
        assert!(
            sidebar_text.contains("boom"),
            "promoted floating notice survives the pass-through sidebar key: {sidebar_text}"
        );

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/24",
            Pull::Worktree,
            Origin::Views,
        );
        assert!(app.sidebar.floating_waits());
        let now = next();
        press_key(&mut app, &mut terminal, SWAP.0, SWAP.1, now);
        assert!(app.sidebar.floating().is_none());
        assert!(app.sidebar.turning());
        assert_eq!(app.pane.side(), SplitSide::Session);

        app.go_worktree(
            &Machine::Local,
            WorktreeId::of(100).unwrap(),
            None,
            now.system,
        );
        app.give(SplitSide::Session, None, now.system);
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        written.borrow_mut().clear();

        start_test_pull(
            &mut app,
            "https://github.com/lorenzolfm/lune/pull/23",
            Pull::Worktree,
            Origin::Views,
        );
        app.pacing.mark_dirty();
        let now = next();
        assert!(drew(&mut app, &mut terminal, &written, now));
        written.borrow_mut().clear();

        let now = next();
        press_key(
            &mut app,
            &mut terminal,
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            now,
        );
        assert!(app.sidebar.floating().is_none());
        assert!(
            drew(&mut app, &mut terminal, &written, now),
            "leaving a waiting menu still redraws when key_to_session is quiet"
        );

        app.sidebar.sit(
            Seat {
                on: Machine::Local,
                project: ProjectId::of(1).unwrap(),
                at: Sitting::Worktree {
                    worktree: WorktreeId::of(100).unwrap(),
                    layout: SeatedLayout::Disconnected(ViewKey::terminal(1)),
                },
            },
            now.system,
        );
        app.sidebar.float_menu();
        assert!(app.sidebar.floating().is_some());
        assert!(!app.sidebar.floating_waits());
        assert!(
            app.take_dictation_target().is_none(),
            "dictation does not dismiss an interactive floating menu"
        );
        assert!(app.sidebar.floating().is_some());

        let now = next();
        app.handle_at(
            Event::Input(Input::Paste("plain text".to_owned())),
            &mut terminal,
            now,
        )
        .unwrap();
        assert!(
            app.sidebar.floating().is_some(),
            "pasting while an interactive floating menu is open stays in the menu"
        );
    }

    fn drain_copies(app: &mut App, inbox: &mut Inbox, terminal: &mut TestTerminal, now: Moment) {
        while let Some(event) = inbox.try_next() {
            if matches!(event, Event::Copy(_)) {
                app.handle_at(event, terminal, now).unwrap();
            }
        }
    }

    fn home_ask_in_a_turn(app: &mut App) {
        let Some(Right::Home(shown)) = app.pane.right() else {
            panic!("a home is shown");
        };
        app.homes
            .shown_mut(shown)
            .ask_numbered(app.asks)
            .expect("the shown home holds the newest ask")
            .in_a_turn(agents::Harness::ClaudeCode, None);
    }

    fn painted_buffer(app: &mut App, columns: u16, rows: u16, now: Moment) -> Buffer {
        let mut view = Terminal::new(ratatui::backend::TestBackend::new(columns, rows)).unwrap();
        view.draw(|frame| {
            let _ = app.paint_at(frame, now);
        })
        .unwrap();
        view.backend().buffer().clone()
    }

    fn find_text(buffer: &Buffer, needle: &str) -> (u16, u16) {
        let area = buffer.area;
        for y in area.top()..area.bottom() {
            let row: String = (area.left()..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect();
            if let Some(byte) = row.find(needle) {
                let col = row[..byte].chars().count();
                return (u16::try_from(col).unwrap(), y);
            }
        }
        panic!("{needle:?} not found on terminal");
    }

    fn osc52_texts(written: &Written) -> Vec<String> {
        use base64::Engine as _;
        let bytes: Vec<u8> = written.borrow().iter().flatten().copied().collect();
        let raw = String::from_utf8_lossy(&bytes);
        raw.split("\x1b]52;c;")
            .skip(1)
            .filter_map(|part| part.split('\x07').next())
            .map(|encoded| {
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap();
                String::from_utf8(decoded).unwrap()
            })
            .collect()
    }

    fn mouse_input(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Input(Input::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    #[test]
    fn dragging_in_the_overlay_ask_box_copies_via_osc52_and_focus_loss_cancels_active_drag() {
        let (mut terminal, written) = test_terminal(100, 24);
        let (mut app, mut inbox, _state) = test_app(100, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };

        let mut ask = Ask::new(1, &menu::Target::Machine(Machine::Local), Path::new("/w"));
        ask.in_a_turn(agents::Harness::ClaudeCode, None);
        ask.heard(ask::Heard::Step(luned::asks::Step::Said(
            "copy this answer".to_owned(),
        )));
        ask.heard(ask::Heard::Ended(Ok(())));
        app.give(SplitSide::Sidebar, Some(Over::Ask(ask)), system);
        app.draw_due(&mut terminal, at(0)).unwrap();
        let initial = painted_buffer(&mut app, 100, 24, at(0));
        written.borrow_mut().clear();

        let (x, y) = find_text(&initial, "copy this answer");
        let left = MouseButton::Left;
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(20),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 8, y),
            &mut terminal,
            at(40),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x + 8, y),
            &mut terminal,
            at(60),
        )
        .unwrap();
        drain_copies(&mut app, &mut inbox, &mut terminal, at(60));
        app.draw_due(&mut terminal, at(80)).unwrap();
        let after_copy = painted_buffer(&mut app, 100, 24, at(80));

        assert_eq!(osc52_texts(&written), ["copy this"]);
        assert_eq!(app.copies.excerpt(), Some("copy this"));
        assert!(after_copy[(x, y)].modifier.contains(Modifier::REVERSED));
        let _ = find_text(&after_copy, "copied \"copy this\"");

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), 1, 1),
            &mut terminal,
            at(100),
        )
        .unwrap();
        app.draw_due(&mut terminal, at(120)).unwrap();
        let after_click = painted_buffer(&mut app, 100, 24, at(120));
        assert!(
            !after_click[(x, y)].modifier.contains(Modifier::REVERSED),
            "clicking outside clears the ask highlight"
        );

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(140),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(160),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.handle_at(Event::Input(Input::FocusLost), &mut terminal, at(180))
            .unwrap();
        assert!(!app.ask_pressed());
        written.borrow_mut().clear();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x + 4, y),
            &mut terminal,
            at(200),
        )
        .unwrap();
        drain_copies(&mut app, &mut inbox, &mut terminal, at(200));
        assert!(
            osc52_texts(&written).is_empty(),
            "a drag dropped by focus loss copies nothing"
        );
    }

    #[test]
    fn dragging_in_the_home_ask_box_copies_via_osc52_even_past_the_sidebar_edge() {
        let (mut terminal, written) = test_terminal(100, 24);
        let (mut app, mut inbox, _state) = test_app(100, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            at(0),
        )
        .unwrap();
        let project = ProjectId::of(1).unwrap();
        app.go_home(&Machine::Local, project, system);
        app.give(SplitSide::Sidebar, None, system);
        home_ask_in_a_turn(&mut app);
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Step(luned::asks::Step::Said("home box text".to_owned())),
            },
            &mut terminal,
            at(20),
        )
        .unwrap();
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Ended(Ok(())),
            },
            &mut terminal,
            at(30),
        )
        .unwrap();
        app.draw_due(&mut terminal, at(40)).unwrap();
        let initial = painted_buffer(&mut app, 100, 24, at(40));
        written.borrow_mut().clear();

        let (x, y) = find_text(&initial, "home box text");
        let left = MouseButton::Left;
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x + 7, y),
            &mut terminal,
            at(60),
        )
        .unwrap();
        assert_eq!(
            app.pane.side(),
            SplitSide::Session,
            "pressing in the home ask box focuses it"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), 1, y),
            &mut terminal,
            at(80),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), 1, y),
            &mut terminal,
            at(100),
        )
        .unwrap();
        drain_copies(&mut app, &mut inbox, &mut terminal, at(100));
        app.draw_due(&mut terminal, at(120)).unwrap();
        let after_copy = painted_buffer(&mut app, 100, 24, at(120));

        assert_eq!(osc52_texts(&written), ["home box"]);
        assert_eq!(app.copies.excerpt(), Some("home box"));
        assert!(after_copy[(x, y)].modifier.contains(Modifier::REVERSED));
        let _ = find_text(&after_copy, "copied \"home box\"");
        assert_eq!(app.held, Held::Nothing);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, 0),
            &mut terminal,
            at(140),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x, 0),
            &mut terminal,
            at(160),
        )
        .unwrap();
        app.draw_due(&mut terminal, at(180)).unwrap();
        let after_click = painted_buffer(&mut app, 100, 24, at(180));
        assert!(
            !after_click[(x, y)].modifier.contains(Modifier::REVERSED),
            "clicking in the home lists clears the ask highlight"
        );

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(200),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(220),
        )
        .unwrap();
        assert!(app.ask_pressed());
        assert_eq!(app.held, Held::Nothing);
        app.go_worktree(&Machine::Local, WorktreeId::of(100).unwrap(), None, system);
        assert!(
            !app.ask_pressed(),
            "leaving home clears an active selection drag"
        );
        assert_eq!(app.held, Held::Nothing);
    }

    #[test]
    fn home_ask_drag_stays_in_sync_across_resize_right_click_overlay_and_view_switch() {
        let (mut terminal, _written) = test_terminal(100, 24);
        let (mut app, mut inbox, _state) = test_app(100, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            at(0),
        )
        .unwrap();
        let project = ProjectId::of(1).unwrap();
        app.go_home(&Machine::Local, project, system);
        home_ask_in_a_turn(&mut app);
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Step(luned::asks::Step::Said("sync drag text".to_owned())),
            },
            &mut terminal,
            at(20),
        )
        .unwrap();
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Ended(Ok(())),
            },
            &mut terminal,
            at(30),
        )
        .unwrap();
        app.draw_due(&mut terminal, at(40)).unwrap();
        let initial = painted_buffer(&mut app, 100, 24, at(40));
        let (x, y) = find_text(&initial, "sync drag text");
        let left = MouseButton::Left;

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(60),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(80),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.handle_at(Event::Input(Input::Resize(100, 30)), &mut terminal, at(90))
            .unwrap();
        let _ = painted_buffer(&mut app, 100, 30, at(100));
        assert!(!app.ask_pressed(), "resize clears active ask drag");
        app.handle_at(Event::Input(Input::FocusLost), &mut terminal, at(120))
            .unwrap();
        assert_eq!(app.held, Held::Nothing);
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), 2, 1),
            &mut terminal,
            at(140),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), 2, 1),
            &mut terminal,
            at(160),
        )
        .unwrap();
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "sidebar click after cancelled home drag is not swallowed"
        );

        app.handle_at(Event::Input(Input::Resize(100, 24)), &mut terminal, at(170))
            .unwrap();
        let redrawn = painted_buffer(&mut app, 100, 24, at(180));
        let (x, y) = find_text(&redrawn, "sync drag text");
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(200),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(220),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.handle_at(
            mouse_input(MouseEventKind::Down(MouseButton::Right), x + 2, y),
            &mut terminal,
            at(240),
        )
        .unwrap();
        assert!(
            !app.ask_pressed(),
            "right-click during left-drag clears the drag instead of stranding it"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x + 4, y),
            &mut terminal,
            at(260),
        )
        .unwrap();
        let after_right = painted_buffer(&mut app, 100, 24, at(280));
        assert!(!after_right[(x, y)].modifier.contains(Modifier::REVERSED));

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(300),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(320),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.give(
            SplitSide::Sidebar,
            Some(Over::Ask(Ask::new(
                99,
                &menu::Target::Machine(Machine::Local),
                Path::new("/w"),
            ))),
            system,
        );
        assert!(
            !app.home_pressed(),
            "opening an overlay cancels an active home drag"
        );
        app.give(SplitSide::Session, None, system);

        let restored = painted_buffer(&mut app, 100, 24, at(340));
        let (x, y) = find_text(&restored, "sync drag text");
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(360),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 4, y),
            &mut terminal,
            at(380),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x + 4, y),
            &mut terminal,
            at(400),
        )
        .unwrap();
        drain_copies(&mut app, &mut inbox, &mut terminal, at(400));
        let swept = painted_buffer(&mut app, 100, 24, at(420));
        assert!(swept[(x, y)].modifier.contains(Modifier::REVERSED));

        app.go_worktree(&Machine::Local, WorktreeId::of(100).unwrap(), None, system);
        app.go_home(&Machine::Local, project, system);
        let returned = painted_buffer(&mut app, 100, 24, at(440));
        assert!(
            !returned[(x, y)].modifier.contains(Modifier::REVERSED),
            "switching away via keyboard clears the kept home highlight"
        );
    }

    #[test]
    fn home_ask_drag_cancels_on_menu_note_sidebar_click_and_never_sets_held_session() {
        let (mut terminal, written) = test_terminal(100, 24);
        let (mut app, mut inbox, _state) = test_app(100, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            at(0),
        )
        .unwrap();
        let project = ProjectId::of(1).unwrap();
        app.go_home(&Machine::Local, project, system);
        home_ask_in_a_turn(&mut app);
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Step(luned::asks::Step::Said("capture sync text".to_owned())),
            },
            &mut terminal,
            at(20),
        )
        .unwrap();
        app.handle_at(
            Event::Heard {
                number: app.asks,
                heard: ask::Heard::Ended(Ok(())),
            },
            &mut terminal,
            at(30),
        )
        .unwrap();
        app.draw_due(&mut terminal, at(40)).unwrap();
        let initial = painted_buffer(&mut app, 100, 24, at(40));
        let (x, y) = find_text(&initial, "capture sync text");
        let left = MouseButton::Left;

        app.handle_at(
            mouse_input(MouseEventKind::Down(MouseButton::Right), x, y),
            &mut terminal,
            at(60),
        )
        .unwrap();
        assert_eq!(app.held, Held::Nothing);
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, 0),
            &mut terminal,
            at(80),
        )
        .unwrap();
        assert_eq!(app.held, Held::Nothing);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(100),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 6, y),
            &mut terminal,
            at(120),
        )
        .unwrap();
        assert!(app.ask_pressed());
        assert!(app.chord(MENU, system));
        assert!(
            !app.ask_pressed(),
            "floating home menu note cancels an active home drag"
        );
        assert_eq!(app.held, Held::Nothing);
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(140),
        )
        .unwrap();
        assert_eq!(app.held, Held::Taken);
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), x, y),
            &mut terminal,
            at(160),
        )
        .unwrap();
        assert_eq!(app.held, Held::Nothing);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), x, y),
            &mut terminal,
            at(180),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), x + 6, y),
            &mut terminal,
            at(200),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), 2, 1),
            &mut terminal,
            at(220),
        )
        .unwrap();
        assert!(!app.ask_pressed());
        assert_eq!(app.held, Held::Nothing);
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), 2, 1),
            &mut terminal,
            at(240),
        )
        .unwrap();
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "sidebar click after a missed home release focuses the sidebar instead of being swallowed"
        );
        assert_eq!(app.held, Held::Nothing);

        let mut ask = Ask::new(99, &menu::Target::Machine(Machine::Local), Path::new("/w"));
        ask.in_a_turn(agents::Harness::ClaudeCode, None);
        ask.heard(ask::Heard::Step(luned::asks::Step::Said(
            "overlay burst text".to_owned(),
        )));
        ask.heard(ask::Heard::Ended(Ok(())));
        app.give(SplitSide::Sidebar, Some(Over::Ask(ask)), system);
        let overlay = painted_buffer(&mut app, 100, 24, at(260));
        let (ox, oy) = find_text(&overlay, "overlay burst text");
        written.borrow_mut().clear();

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), ox, oy),
            &mut terminal,
            at(280),
        )
        .unwrap();
        app.handle_at(
            mouse_input(MouseEventKind::Drag(left), ox + 7, oy),
            &mut terminal,
            at(300),
        )
        .unwrap();
        assert!(app.ask_pressed());
        app.burst = Some(Burst::new(Position::new(ox + 7, oy), at(300).steady));
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), ox + 7, oy),
            &mut terminal,
            at(320),
        )
        .unwrap();
        assert!(
            !app.ask_pressed(),
            "burst capture cancels active overlay ask drag"
        );
        assert_eq!(app.held, Held::Taken);
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), ox + 7, oy),
            &mut terminal,
            at(340),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "Up after burst hit releases Held::Taken even while Over::Ask is open"
        );
        drain_copies(&mut app, &mut inbox, &mut terminal, at(340));
        assert_eq!(osc52_texts(&written), Vec::<String>::new());
    }

    #[test]
    fn worktree_mouse_capture_survives_overlays_and_menu_but_clears_on_focus_loss_or_down() {
        let (mut terminal, _written) = test_terminal(100, 24);
        let (mut app, _inbox, _state) = test_app(100, 24);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            at(0),
        )
        .unwrap();
        app.go_worktree(&Machine::Local, WorktreeId::of(100).unwrap(), None, system);
        app.draw_due(&mut terminal, at(20)).unwrap();
        let (_, right, _) = app.areas();
        let left = MouseButton::Left;

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(40),
        )
        .unwrap();
        assert_eq!(app.held, Held::Session);
        app.give(
            SplitSide::Sidebar,
            Some(Over::Ask(Ask::new(
                1,
                &menu::Target::Machine(Machine::Local),
                Path::new("/w"),
            ))),
            system,
        );
        assert_eq!(
            app.held,
            Held::Session,
            "opening an overlay preserves worktree session capture for its matching Up"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(60),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "Up under an overlay finishes and clears Held::Session"
        );
        app.give(SplitSide::Session, None, system);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(80),
        )
        .unwrap();
        assert_eq!(app.held, Held::Session);
        assert!(app.chord(MENU, system));
        assert_eq!(
            app.held,
            Held::Session,
            "opening the floating menu preserves worktree session capture"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(100),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "Up under the floating menu finishes and clears Held::Session"
        );
        app.give(SplitSide::Session, None, system);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(110),
        )
        .unwrap();
        assert_eq!(app.held, Held::Session);
        app.give(SplitSide::Collapsed, None, system);
        assert_eq!(
            app.held,
            Held::Session,
            "collapsing the sidebar preserves worktree session capture"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), 0, right.y + 2),
            &mut terminal,
            at(115),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "Up over the collapsed strip finishes and clears Held::Session"
        );
        app.give(SplitSide::Session, None, system);

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(120),
        )
        .unwrap();
        assert_eq!(app.held, Held::Session);
        app.handle_at(Event::Input(Input::FocusLost), &mut terminal, at(140))
            .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "FocusLost clears active worktree capture"
        );

        app.handle_at(
            mouse_input(MouseEventKind::Down(left), right.x + 2, right.y + 2),
            &mut terminal,
            at(160),
        )
        .unwrap();
        assert_eq!(app.held, Held::Session);
        app.handle_at(
            mouse_input(MouseEventKind::Down(left), 2, 1),
            &mut terminal,
            at(180),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Nothing,
            "a new Down in the sidebar clears a stranded worktree capture"
        );
        app.handle_at(
            mouse_input(MouseEventKind::Up(left), 2, 1),
            &mut terminal,
            at(200),
        )
        .unwrap();
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "the sidebar click focuses the sidebar instead of routing to the session"
        );
    }

    #[test]
    fn resize_and_collapse_update_layout_before_drawing() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };

        app.handle_at(
            Event::Daemon(Ok(busy_host(8, "work"))),
            &mut terminal,
            at(Duration::ZERO),
        )
        .unwrap();
        let initial_view = app.sidebar.view();

        app.handle_at(
            Event::Input(Input::Resize(100, 8)),
            &mut terminal,
            at(Duration::from_millis(10)),
        )
        .unwrap();
        let expected = areas(Rect::new(0, 0, 100, 8), app.width.columns());
        assert_eq!(app.frame, Rect::new(0, 0, 100, 8));
        assert_eq!(app.areas(), expected);
        assert_ne!(
            app.sidebar.view(),
            initial_view,
            "sidebar layout updates on Input::Resize before any draw"
        );

        let collapse_at = at(Duration::from_millis(20));
        app.go_worktree(
            &Machine::Local,
            WorktreeId::of(100).unwrap(),
            None,
            collapse_at.system,
        );
        app.give(SplitSide::Collapsed, None, collapse_at.system);
        assert_eq!(
            app.areas(),
            areas(Rect::new(0, 0, 100, 8), sidebar::COLLAPSED),
            "collapsing updates areas before any draw"
        );

        app.handle_at(
            Event::Input(Input::Resize(80, 20)),
            &mut terminal,
            at(Duration::from_millis(30)),
        )
        .unwrap();
        terminal.draw(|frame| app.paint(frame)).unwrap();
    }

    #[test]
    fn paint_and_draw_due_adapt_when_terminal_area_changes_without_resize_event() {
        let (mut terminal, _written) = test_terminal(100, 12);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let first = Moment::now();

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            first,
        )
        .unwrap();
        terminal.draw(|frame| app.paint(frame)).unwrap();
        assert_eq!(app.frame, Rect::new(0, 0, 100, 12));
        assert_eq!(
            app.areas(),
            areas(Rect::new(0, 0, 100, 12), app.width.columns())
        );

        let resized = Rect::new(0, 0, 92, 14);
        let (mut resized_terminal, resized_written) = test_terminal(resized.width, resized.height);
        let later = Moment::now() + sidebar::SPIN;
        app.tick(later);
        assert!(drew(
            &mut app,
            &mut resized_terminal,
            &resized_written,
            later
        ));
        assert_eq!(app.frame, resized);
        assert_eq!(app.areas(), areas(resized, app.width.columns()));
    }

    #[test]
    fn unview_while_collapsed_returns_to_expanded_sidebar_and_paints_cleanly() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let now = Moment::now();

        app.handle_at(Event::Daemon(Ok(busy_host(1, "work"))), &mut terminal, now)
            .unwrap();
        app.go_worktree(
            &Machine::Local,
            WorktreeId::of(100).unwrap(),
            None,
            now.system,
        );
        app.give(SplitSide::Collapsed, None, now.system);
        assert!(app.pane.collapsed());
        assert!(matches!(app.sidebar.view(), SidebarView::Collapsed { .. }));

        app.unview(now.system);
        assert!(matches!(app.pane, Pane::SidebarOnly));
        assert!(!app.pane.collapsed());
        assert!(matches!(app.sidebar.view(), SidebarView::Expanded(_)));
        assert_eq!(
            app.areas(),
            areas(Rect::new(0, 0, 80, 20), app.width.columns())
        );
        terminal.draw(|frame| app.paint(frame)).unwrap();
    }

    #[test]
    fn public_paint_syncs_aloud_state_before_drawing() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let now = Moment::now();

        assert!(app.sidebar.aloud(Some(aloud::Shown::Fetching {
            got: 0,
            paused: false,
        })));
        assert_ne!(app.sidebar.aloud_buttons(), &crate::spots::Spots::default());

        terminal.draw(|frame| app.paint(frame)).unwrap();
        assert_eq!(app.sidebar.aloud_buttons(), &crate::spots::Spots::default());

        app.pacing.mark_dirty();
        app.draw_due(&mut terminal, now + FRAME_INTERVAL).unwrap();
    }

    #[test]
    fn resize_preserves_cell_pixels_derived_from_window_pixels() {
        let (mut terminal, _written) = test_terminal(120, 40);
        let (mut app, _inbox, _state) = test_app(120, 40);
        let nz = |value| NonZeroU16::new(value).unwrap();
        let expected = CellPixels {
            width: nz(10),
            height: nz(20),
        };
        assert!(
            !app.answered(
                Answer::WindowPixels(WindowPixels {
                    width: nz(1200),
                    height: nz(800),
                }),
                &mut terminal,
            )
            .unwrap()
        );
        assert_eq!(app.cell, expected);

        app.handle_at(
            Event::Input(Input::Resize(60, 20)),
            &mut terminal,
            Moment::now(),
        )
        .unwrap();
        assert_eq!(
            app.cell, expected,
            "resizing the window must not recompute cell size from startup window_pixels"
        );

        let (mut larger, _larger_written) = test_terminal(180, 60);
        larger.draw(|frame| app.paint(frame)).unwrap();
        assert_eq!(
            app.cell, expected,
            "painting a resized frame must preserve cell size derived from startup window_pixels"
        );
    }

    #[test]
    fn aloud_buttons_match_painted_frame_and_settling_on_clicks() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |ms| Moment {
            steady: start + Duration::from_millis(ms),
            system: system + Duration::from_millis(ms),
        };
        let mouse = |kind, pos: Position| MouseEvent {
            kind,
            column: pos.x,
            row: pos.y,
            modifiers: KeyModifiers::NONE,
        };
        let down = |pos| mouse(MouseEventKind::Down(MouseButton::Left), pos);
        let up = |pos| mouse(MouseEventKind::Up(MouseButton::Left), pos);

        app.handle_at(
            Event::Daemon(Ok(busy_host(1, "work"))),
            &mut terminal,
            at(0),
        )
        .unwrap();
        app.tick(at(0));
        app.draw_due(&mut terminal, at(0)).unwrap();

        assert!(app.sidebar.aloud(Some(aloud::Shown::Fetching {
            got: 0,
            paused: false,
        })));
        let button = (0..20)
            .flat_map(|y| (0..80).map(move |x| Position::new(x, y)))
            .find(|pos| app.sidebar.aloud_buttons().at(*pos).is_some())
            .expect("fetching aloud lays out clickable buttons");

        app.pointer = Some(button);
        assert_eq!(
            app.hover().aloud,
            None,
            "undrawn aloud buttons do not highlight on hover"
        );
        app.mouse(down(button), at(500));
        assert_eq!(
            app.held,
            Held::Nothing,
            "undrawn aloud buttons must not intercept clicks before paint"
        );
        assert!(app.sidebar.pressed());
        app.mouse(up(button), at(510));

        app.frame_at(&mut terminal, at(1200)).unwrap();
        app.pointer = Some(button);
        assert!(
            app.hover().aloud.is_some(),
            "drawn aloud buttons highlight on hover"
        );

        app.mouse(down(button), at(1300));
        assert_eq!(
            app.held,
            Held::Taken,
            "newly drawn aloud buttons are absorbed during the settling window"
        );
        assert!(!app.sidebar.pressed());
        app.mouse(up(button), at(1310));

        assert!(app.sidebar.aloud(None));
        assert_eq!(app.sidebar.aloud_buttons(), &Spots::default());
        app.mouse(down(button), at(1900));
        assert_eq!(
            app.held,
            Held::Taken,
            "still-painted aloud buttons keep absorbing clicks until the frame redraws"
        );
        assert!(!app.sidebar.pressed());
        app.mouse(up(button), at(1910));

        app.frame_at(&mut terminal, at(2600)).unwrap();
        app.pointer = Some(button);
        assert_eq!(app.hover().aloud, None);

        app.mouse(down(button), at(2700));
        assert_eq!(
            app.held,
            Held::Taken,
            "cleared aloud buttons remain protected during the settling window"
        );
        assert!(!app.sidebar.pressed());
        app.mouse(up(button), at(2710));

        app.mouse(down(button), at(3300));
        assert_eq!(
            app.held,
            Held::Nothing,
            "after settling expires, clicks reach the sidebar"
        );
        assert!(app.sidebar.pressed());
        app.mouse(up(button), at(3310));
    }

    #[test]
    fn revive_view_event_navigates_focuses_or_notifies_and_ignores_key_repeat() {
        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let now = Moment::now();

        let repeat = KeyEvent::new_with_kind(
            KeyCode::Char('t'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
            KeyEventKind::Repeat,
        );
        assert!(
            !app.key(&repeat, now),
            "holding Super+Shift+T does not repeatedly revive views"
        );
        assert_eq!(app.reviving, Reviving::Idle);

        let worktree = WorktreeId::of(100).unwrap();
        let view = ViewId::of(200).unwrap();
        assert!(app.pane.right().is_none());
        assert_eq!(app.pane.side(), SplitSide::Sidebar);
        app.give(SplitSide::Session, None, now.system);
        assert_eq!(
            app.pane.side(),
            SplitSide::Sidebar,
            "an empty pane keeps focus on the sidebar"
        );
        assert_eq!(app.revive_machine(), Machine::Local);

        app.reviving = Reviving::Busy;
        let press = KeyEvent::new_with_kind(
            KeyCode::Char('t'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
            KeyEventKind::Press,
        );
        let before_threads = app.changes.len();
        assert!(app.key(&press, now));
        assert_eq!(
            app.changes.len(),
            before_threads,
            "pressing Super+Shift+T while reviving spawns no second thread"
        );
        assert_eq!(app.reviving, Reviving::Busy);

        app.handle_at(
            Event::Revived {
                on: Machine::Local,
                revived: Err(Unchanged::Refused("nothing closed to revive".to_owned())),
            },
            &mut terminal,
            now,
        )
        .unwrap();
        assert_eq!(app.reviving, Reviving::Idle);
        assert!(app.pane.right().is_none());

        app.reviving = Reviving::Busy;
        app.handle_at(
            Event::Revived {
                on: Machine::Local,
                revived: Ok(worktrees::Revived { worktree, view }),
            },
            &mut terminal,
            now,
        )
        .unwrap();
        assert_eq!(app.reviving, Reviving::Idle);
        assert_eq!(app.viewed().map(Viewed::worktree), Some(worktree));
        assert_eq!(app.pane.side(), SplitSide::Session);

        app.give(SplitSide::Collapsed, None, now.system);
        app.reviving = Reviving::Busy;
        assert!(app.key(&press, now));
        assert!(
            matches!(app.sidebar.floating(), Some(Floating::Note(note)) if note == REVIVING),
            "second press while collapsed floats the busy note"
        );
        app.handle_at(
            Event::Revived {
                on: Machine::Local,
                revived: Err(Unchanged::Refused("nothing closed to revive".to_owned())),
            },
            &mut terminal,
            now,
        )
        .unwrap();
        assert_eq!(app.reviving, Reviving::Idle);
        assert!(
            matches!(
                app.sidebar.floating(),
                Some(Floating::Note(note)) if note == "no view revived: nothing closed to revive"
            ),
            "refusal replaces the collapsed busy floating note"
        );
        assert!(
            !app.sidebar.clear_notice(),
            "replacing a collapsed floating note leaves sidebar notice clean"
        );

        app.give(SplitSide::Collapsed, None, now.system);
        app.reviving = Reviving::Busy;
        assert!(app.key(&press, now));
        assert!(matches!(
            app.sidebar.floating(),
            Some(Floating::Note(note)) if note == REVIVING
        ));
        app.handle_at(
            Event::Revived {
                on: Machine::Local,
                revived: Ok(worktrees::Revived { worktree, view }),
            },
            &mut terminal,
            now,
        )
        .unwrap();
        assert_eq!(app.reviving, Reviving::Idle);
        assert!(
            app.sidebar.floating().is_none(),
            "successful revive dismisses the stale collapsed busy note"
        );
        assert!(
            matches!(app.holder(), Holder::Terminal),
            "revived view regains terminal focus when collapsed"
        );
        assert_eq!(app.pane.side(), SplitSide::Collapsed);
    }

    #[test]
    fn browser_views_resolve_in_project_and_keep_their_worktree_open() {
        use luned::api::worktree::{View as ApiView, Views as ApiViews, view::Kind};
        use luned::api::{self, projects};

        let browser = api::Browser {
            id: 30,
            url: Some("https://example.com".to_owned()),
        };
        let worktree = api::Worktree {
            id: 100,
            name: "docs".to_owned(),
            folder: "/w/lune/docs".to_owned(),
            seen: Some(api::worktree::Seen::Unread(())),
            terminals: Vec::new(),
            views: Some(ApiViews {
                items: vec![ApiView {
                    kind: Some(Kind::Browser(browser)),
                }],
            }),
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
        let host = Host {
            projects: Some(api::Projects {
                held: Some(projects::Held::Kept(kept)),
            }),
            ..Host::default()
        };
        let projected = Projected::adapted(&host).unwrap();
        let project_id = ProjectId::of(1).unwrap();
        let worktree_id = WorktreeId::of(100).unwrap();
        let browser_id = ViewId::of(30).unwrap();
        let parsed = projected.project(&Machine::Local, project_id).unwrap();
        assert_eq!(
            view_location(parsed, ViewKey::Browser(browser_id)),
            Some((worktree_id, browser_id))
        );
        assert_eq!(
            view_location(parsed, ViewKey::Browser(ViewId::of(99).unwrap())),
            None
        );
        assert_eq!(
            no_view(ViewKey::Browser(browser_id)),
            "menu: luned lists no browser view"
        );
        assert_eq!(no_view(ViewKey::terminal(4)), "menu: luned lists no view 4");

        let (mut terminal, _written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        app.handle_at(Event::Daemon(Ok(host)), &mut terminal, Moment::now())
            .unwrap();
        app.go_other_than(&Machine::Local, None, Moment::now().system);
        assert_eq!(app.viewed().map(Viewed::worktree), Some(worktree_id));
    }

    #[test]
    fn a_worktree_view_s_titles_follow_its_agy_agent_from_auto_title_to_live_topics() {
        use luned::api::{self, Harness, NameSource, Status, agent::Status as Reported, projects};

        let (mut terminal, written) = test_terminal(80, 20);
        let (mut app, _inbox, _state) = test_app(80, 20);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };
        let worktree_id = WorktreeId::of(100).unwrap();
        let view_id = ViewId::of(1).unwrap();

        let host_with = |topic: Option<&str>| {
            let made_at = Some(prost_types::Timestamp::default());
            let term = api::Terminal {
                id: 1,
                found: Some(api::terminal::Found::Key(api::TerminalKey {
                    id: 1,
                    made_at,
                })),
                folder: "/w/lune".to_owned(),
                command: Some("agy".to_owned()),
                ..api::Terminal::default()
            };
            let worktree = api::Worktree {
                id: 100,
                name: "main".to_owned(),
                folder: "/w/lune".to_owned(),
                seen: Some(api::worktree::Seen::Unread(())),
                terminals: vec![term],
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
                pid: 42,
                harness: Some(Harness::Antigravity.into()),
                cwd: "/w/lune".to_owned(),
                status: Some(Reported::KnownStatus(Status::Idle.into())),
                terminal: Some(api::TerminalKey { id: 1, made_at }),
                name: Some(api::Name {
                    text: "Initial agy title".to_owned(),
                    source: Some(NameSource::Auto.into()),
                }),
                topic: topic.map(str::to_owned),
                ..api::Agent::default()
            };
            Host {
                projects: Some(api::Projects {
                    held: Some(projects::Held::Kept(kept)),
                }),
                agents: vec![agent],
                ..Host::default()
            }
        };

        app.handle_at(
            Event::Daemon(Ok(host_with(None))),
            &mut terminal,
            at(Duration::ZERO),
        )
        .unwrap();
        app.go_worktree(
            &Machine::Local,
            worktree_id,
            None,
            at(Duration::ZERO).system,
        );
        app.give(SplitSide::Collapsed, None, at(Duration::ZERO).system);
        assert_eq!(
            app.sidebar
                .view_titles(&Machine::Local, worktree_id)
                .and_then(|titles| titles.get(&view_id))
                .map(String::as_str),
            Some("Initial agy title")
        );
        app.tick(at(Duration::ZERO));
        assert!(drew(&mut app, &mut terminal, &written, at(Duration::ZERO)));
        written.borrow_mut().clear();

        let first = Duration::from_secs(1);
        app.handle_at(
            Event::Daemon(Ok(host_with(Some("Session folds")))),
            &mut terminal,
            at(first),
        )
        .unwrap();
        assert_eq!(
            app.sidebar
                .view_titles(&Machine::Local, worktree_id)
                .and_then(|titles| titles.get(&view_id))
                .map(String::as_str),
            Some("Session folds")
        );
        app.tick(at(first));
        assert!(drew(&mut app, &mut terminal, &written, at(first)));
        written.borrow_mut().clear();

        let second = Duration::from_secs(2);
        app.handle_at(
            Event::Daemon(Ok(host_with(Some("View tab titles")))),
            &mut terminal,
            at(second),
        )
        .unwrap();
        assert_eq!(
            app.sidebar
                .view_titles(&Machine::Local, worktree_id)
                .and_then(|titles| titles.get(&view_id))
                .map(String::as_str),
            Some("View tab titles")
        );
        app.tick(at(second));
        assert!(drew(&mut app, &mut terminal, &written, at(second)));
    }

    #[test]
    fn status_bar_updates_on_telemetry_changes_and_blocks_clicks_from_session() {
        let (mut terminal, written) = test_terminal(120, 12);
        let (mut app, _inbox, _state) = test_app(120, 12);
        let start = Instant::now();
        let system = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_007);
        let at = |offset| Moment {
            steady: start + offset,
            system: system + offset,
        };

        let make_host = |used_permille: u32| {
            let mut host = busy_host(1, "Work on status bar");
            host.agents[0].telemetry = Some(luned::api::AgentTelemetry {
                model: Some("Opus 4.7".to_owned()),
                effort: Some("high".to_owned()),
                context: Some(luned::api::ContextWindow {
                    used_permille: 19,
                    window_tokens: Some(1_000_000),
                    used_tokens: Some(19_000),
                }),
                usage: Some(luned::api::AgentUsage {
                    windows: vec![luned::api::UsageWindow {
                        label: "5h".to_owned(),
                        used_permille,
                        resets_at: None,
                    }],
                    session_cost_micro_usd: None,
                }),
            });
            host
        };

        app.handle_at(
            Event::Daemon(Ok(make_host(350))),
            &mut terminal,
            at(Duration::ZERO),
        )
        .unwrap();
        app.go_worktree(
            &Machine::Local,
            WorktreeId::of(100).unwrap(),
            None,
            at(Duration::ZERO).system,
        );
        app.give(SplitSide::Collapsed, None, at(Duration::ZERO).system);
        app.tick(at(Duration::ZERO));
        assert!(drew(&mut app, &mut terminal, &written, at(Duration::ZERO)));

        let mut screen = Term::new(Config::default(), &TermSize::new(120, 12), VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        let mut bottom_row = |written: &Written| -> String {
            for write in written.borrow_mut().drain(..) {
                parser.advance(&mut screen, &write);
            }
            (0..120)
                .map(|column| screen.grid()[Point::new(Line(11), Column(column))].c)
                .collect()
        };
        let initial_bar = bottom_row(&written);
        assert!(initial_bar.contains("☾ ⠋1 › lune › main"), "{initial_bar}");
        assert!(initial_bar.contains("5h 35%"), "{initial_bar}");

        let later = Duration::from_secs(1);
        app.handle_at(Event::Daemon(Ok(make_host(420))), &mut terminal, at(later))
            .unwrap();
        app.tick(at(later));
        assert!(
            drew(&mut app, &mut terminal, &written, at(later)),
            "usage telemetry update redraws the status bar"
        );
        let updated_bar = bottom_row(&written);
        assert!(updated_bar.contains("5h 42%"), "{updated_bar}");

        let (_, _, bar) = app.areas();
        assert_eq!(bar.height, statusbar::ROWS);
        let click_at = later + Duration::from_millis(50);
        app.handle_at(
            Event::Input(Input::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: bar.x + 2,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            })),
            &mut terminal,
            at(click_at),
        )
        .unwrap();
        assert_eq!(
            app.held,
            Held::Taken,
            "clicking the status bar must not capture session or expand collapsed sidebar"
        );
        assert!(app.pane.collapsed());
    }
}
