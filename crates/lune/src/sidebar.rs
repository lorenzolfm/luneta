use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use lune_domain::mux::TerminalKey;
use lune_domain::names::SessionName;
use lune_domain::projects::{self, Argv, Dir, Opens, ProjectId, Used, ViewId};
use lune_domain::projects::{Arranged, ProjectName, WorktreeId};
use lune_domain::rows::{self, Told};
use lune_protocol::agent_status::AgentStatus;
use lune_protocol::git::{Changed, Changes, GitStatus, Head, ParsedGitStatuses, Upstream};
use lune_protocol::github::{Mergeability, ParsedPullRequests, PullRequest, State, Verdict};
use lune_protocol::said::Said;
use lune_protocol::shell::{Doing, Ending, Front};
use lune_protocol::unread;
#[cfg(test)]
use luned::api::Status;
#[cfg(test)]
use luned::api::agent::Status as Reported;
use luned::api::remote::unreached::Stopped;
use luned::api::remote::{self, Reached};
use luned::api::{self, Agent, Host, Saying, UnreadFile};
use luned::changes::Unchanged;
use luned::issues::{Filed, Unfiled};
use luned::links::{self, Branch};
use luned::prunes;
use luned::rebases::{Rebased, Unrebased};
use luned::seats::{self, Unread, seated};
use luned::transcripts::SessionId;
use luned::{Destination, Directory, Troubles, log};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::actions::{Action, Command, GIT, QUIT};
use crate::agents::{self, Kept};
use crate::alerts::{self, Reach};
use crate::aloud;
use crate::attached::border;
use crate::brief::{Brief, Note, Over, Transcript};
use crate::cadence::Cadence;
use crate::checks;
use crate::clock::Moment;
use crate::close::{self, Act, Plan, Scope, Untold};
use crate::copied::{Copied, Excerpt};
use crate::folds::{Fold, FoldSet, Folds};
use crate::fuzzy::{Fuzzy, Score, Typos};
use crate::icons;
use crate::issue::Repository;
use crate::ledger::{self, Key};
use crate::live::Lists;
use crate::machines::{self, Machine};
use crate::menu::{
    self, AllKeys, Briefed, Choice, Cwd, Did, Done, Folder, Hint, Label, Menu, Origin,
    ProjectTarget, Pull, Ran, Rename, Target, Top, ViewRow, Wants, WorktreeRow,
};
use crate::moonlight;
use crate::news::{Dwell, News};
use crate::picker::{Aim, Here, Housed, Listing, Runs, Seated, Shelved, Unopened, Worktree};
use crate::printed::Printed;
use crate::prune;
use crate::pull::{Found, Link, Others, Wanted, Work, Worked};
use crate::skills::{Name, Skill};
use crate::spots::{Drawn, Keyed, SidebarBottom, Spots};
use crate::sweep::{Page, Sweep, cells};

pub const COLUMNS: u16 = 32;

pub const PAUSE: Duration = Duration::from_millis(200);

const GO: &str = "Go";
const GO_KEYS: [(Label, &str); 1] = [(Label::char('g'), "First row")];
const GO_GIT: &str = "Go, or git";
const GO_GIT_KEYS: [(Label, &str); 2] = [
    (Label::char('g'), "First row"),
    (Label::char('r'), "Rebase onto upstream"),
];

const SELECTED_KEYS: [(Label, &str); 4] = [
    (Label::Two('j', 'k'), "Grow"),
    (Label::Two('J', 'K'), "Move"),
    (Label::char('v'), "Project, then done"),
    (Label::code(KeyCode::Esc), "Done"),
];

const COPIED_SHOWS: Duration = Duration::from_secs(3);

pub const NARROWEST: u16 = 16;

const COLLAPSED_ROWS: u16 = 4;

pub const COLLAPSED: u16 = COLLAPSED_ROWS + 1;

const HERE_MARK: &str = "▌";
const NEW: &str = "•";
const UNFOLDED: &str = "▾";
const FOLDED: &str = "▸";
const RULE: &str = "─";
const MOON: &str = "☾";
const PROJECT_DOT: &str = "●";
const MACHINE_LEAD: usize = 5;
const MISSING: &str = " missing";
const NO_PROJECTS: &str = "no projects";
const COMPUTER: &str = "\u{f108}";
const EDGE: &str = "│";
const INDENT: &str = " ";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPIN: Duration = Duration::from_millis(100);
pub const SCROLL: Duration = Duration::from_millis(60);
pub const ROWS_MAX: usize = 8192;

const _: () = assert!(
    SPIN.as_millis() > 0 && SPIN.as_nanos() == SPIN.as_millis() * 1_000_000,
    "spin steps are whole positive milliseconds so frame and next_spin agree"
);

const AGE_MINUTE_SECONDS: u64 = 60;
const AGE_HOUR_SECONDS: u64 = 3600;
const AGE_DAY_SECONDS: u64 = 86_400;
const AGE_WEEK_SECONDS: u64 = 604_800;

pub const ACCENT: Color = moonlight::MAUVE;
pub const NAME: Color = moonlight::TEXT;
pub const TAG: Color = moonlight::YELLOW;
pub const ERROR: Color = moonlight::RED;
pub const LIT: Color = moonlight::LIFTED;
const RESTING: Color = moonlight::SURFACE;

const MARK: usize = 1;

const LABEL: usize = 3 + MARK + 1 + icons::WIDTH + 1;

const WORKTREE: usize = 4;

const PROJECT: usize = 3;

const NAME_KEPT: usize = 12;

const LONG: Duration = Duration::from_secs(1);

const NO_HERE_VIEW: &str =
    "menu: the focused view holds no terminal, or lune has not heard which one yet";

const PALETTE_ROW_GONE: &str =
    "palette: the highlighted row changed while the palette was open, so nothing ran";

const PALETTE_UNPLACED: &str =
    "palette: lune does not know yet which view its client is on; try again in a moment";

const PALETTE_NO_ROW: &str = "palette: the sidebar highlights no row yet";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Listed {
    name: ProjectName,
    kept: ledger::Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectRow {
    listed: Listed,
    id: ProjectId,
}

impl ProjectRow {
    #[must_use]
    pub fn of(listed: Listed) -> Self {
        let id = listed.project();
        ProjectRow { listed, id }
    }

    #[must_use]
    pub fn listed(&self) -> &Listed {
        &self.listed
    }

    #[must_use]
    pub fn id(&self) -> ProjectId {
        self.id
    }

    #[cfg(test)]
    pub fn new(name: &str, id: u64) -> Self {
        ProjectRow::of(Listed::new_project(name, id))
    }
}

impl Listed {
    #[must_use]
    pub fn project(&self) -> ProjectId {
        match self.kept {
            ledger::Session::Project(project) => project,
        }
    }

    pub fn name(&self) -> &ProjectName {
        &self.name
    }

    fn key(&self, view: ViewId) -> Key {
        Key {
            session: self.kept,
            view,
        }
    }

    #[cfg(test)]
    pub fn new_project(name: &str, id: u64) -> Self {
        let kept = ledger::Session::Project(lune_domain::projects::ProjectId::of(id).unwrap());
        Listed {
            name: ProjectName::parse(name).unwrap(),
            kept,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Session(Listed),
    Worktree(WorktreePlace),
    View(ViewPlace),
}

impl Place {
    pub fn session(&self) -> &str {
        self.listed().name.as_str()
    }

    pub fn listed(&self) -> &Listed {
        match self {
            Place::Session(session) => session,
            Place::Worktree(place) => place.project.listed(),
            Place::View(place) => place.listed(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorktreePlace {
    pub project: ProjectRow,
    pub worktree: WorktreeId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ViewPlace {
    worktree: WorktreePlace,
    view: ViewKey,
}

impl ViewPlace {
    fn of(worktree: &WorktreePlace, view: &KnownView) -> Self {
        ViewPlace {
            worktree: worktree.clone(),
            view: ViewKey::Terminal(view.key),
        }
    }

    #[must_use]
    pub fn worktree(&self) -> &WorktreePlace {
        &self.worktree
    }

    #[must_use]
    pub fn view(&self) -> ViewKey {
        self.view
    }

    #[must_use]
    pub fn listed(&self) -> &Listed {
        self.worktree.project.listed()
    }

    #[cfg(test)]
    pub fn new(worktree: WorktreePlace, view: ViewKey) -> Self {
        ViewPlace { worktree, view }
    }

    #[cfg(test)]
    pub fn new_in(name: &str, project: u64, worktree: u64, view: u32) -> Self {
        let worktree = WorktreeId::of(worktree).unwrap();
        let worktree = WorktreePlace {
            project: ProjectRow::new(name, project),
            worktree,
        };
        ViewPlace::new(worktree, ViewKey::terminal(view))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewKey {
    Terminal(TerminalKey),
}

impl ViewKey {
    #[must_use]
    pub fn key(self) -> TerminalKey {
        match self {
            ViewKey::Terminal(key) => key,
        }
    }
}

impl ViewKey {
    #[cfg(test)]
    pub fn terminal(id: u32) -> Self {
        let made = luned::mux::own::OwnMade::from_nanos(0);
        ViewKey::Terminal(TerminalKey {
            id: NonZeroU32::new(id).unwrap(),
            made,
        })
    }
}

impl fmt::Display for ViewKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.key().id.fmt(f)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seat {
    pub on: Machine,
    pub project: ProjectId,
    pub at: Sitting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeatedLayout {
    Unlaid,
    Disconnected(TerminalKey),
    Live {
        arranged: Arranged,
        version: Option<api::Version>,
        terminal: Option<TerminalKey>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sitting {
    Home,
    Worktree {
        worktree: WorktreeId,
        layout: SeatedLayout,
    },
}

impl Seat {
    fn worktree(&self) -> Option<WorktreeId> {
        match self.at {
            Sitting::Worktree { worktree, .. } => Some(worktree),
            Sitting::Home => None,
        }
    }

    fn terminal(&self) -> Option<TerminalKey> {
        match self.at {
            Sitting::Worktree {
                layout: SeatedLayout::Live { terminal, .. },
                ..
            } => terminal,
            Sitting::Worktree {
                layout: SeatedLayout::Disconnected(terminal),
                ..
            } => Some(terminal),
            Sitting::Worktree {
                layout: SeatedLayout::Unlaid,
                ..
            }
            | Sitting::Home => None,
        }
    }

    fn live_arranged(&self) -> Option<(Arranged, Option<api::Version>)> {
        match self.at {
            Sitting::Worktree {
                layout:
                    SeatedLayout::Live {
                        arranged, version, ..
                    },
                ..
            } => Some((arranged, version)),
            Sitting::Worktree {
                layout: SeatedLayout::Unlaid | SeatedLayout::Disconnected(_),
                ..
            }
            | Sitting::Home => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereAgent {
    pub on: Machine,
    pub session: SessionName,
    pub view: ViewKey,
    pub dir: Directory,
    pub harness: agents::Harness,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Spot {
    Machine(Machine),
    Local(Place),
    Remote(Destination, Place),
}

impl Spot {
    fn on(machine: &Machine, place: Place) -> Spot {
        match machine {
            Machine::Local => Spot::Local(place),
            Machine::Remote(destination) => Spot::Remote(destination.clone(), place),
        }
    }

    fn machine(&self) -> Machine {
        match self {
            Spot::Machine(machine) => machine.clone(),
            Spot::Local(_) => Machine::Local,
            Spot::Remote(destination, _) => Machine::Remote(destination.clone()),
        }
    }

    fn is_on(&self, machine: &Machine) -> bool {
        match (self, machine) {
            (Spot::Machine(spot_machine), machine) => spot_machine == machine,
            (Spot::Local(_), Machine::Local) => true,
            (Spot::Remote(destination, _), Machine::Remote(other)) => destination == other,
            (Spot::Local(_), Machine::Remote(_)) | (Spot::Remote(..), Machine::Local) => false,
        }
    }

    fn place(&self) -> Option<&Place> {
        match self {
            Spot::Machine(_) => None,
            Spot::Local(place) | Spot::Remote(_, place) => Some(place),
        }
    }

    fn worktree(&self) -> Option<Spot> {
        match self.place()? {
            Place::Session(_) => None,
            Place::Worktree(_) => Some(self.clone()),
            Place::View(place) => Some(Spot::on(
                &self.machine(),
                Place::Worktree(place.worktree.clone()),
            )),
        }
    }

    fn folding(&self) -> Folding {
        match self.place() {
            None => Folding::Machine,
            Some(Place::Session(session)) => Folding::Project(session.project()),
            Some(Place::Worktree(place)) => Folding::Worktree(place.worktree),
            Some(Place::View(place)) => Folding::Worktree(place.worktree.worktree),
        }
    }

    fn placed(self) -> Option<(Machine, Place)> {
        match self {
            Spot::Machine(_) => None,
            Spot::Local(place) => Some((Machine::Local, place)),
            Spot::Remote(destination, place) => Some((Machine::Remote(destination), place)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Waiting,
    Idle,
    Finished,
    Busy,
    Error,
    Shell,
    Other(char),
    Unreported,
    Passed,
    Failed,
    Ran,
}

impl Mark {
    fn of(agent: &Agent) -> Self {
        match AgentStatus::of_api(agent) {
            AgentStatus::Blocked { .. } => Mark::Waiting,
            AgentStatus::Idle | AgentStatus::Stopped | AgentStatus::Done { .. } => Mark::Idle,
            AgentStatus::Working => Mark::Busy,
            AgentStatus::Error { .. } => Mark::Error,
            AgentStatus::Shell => Mark::Shell,
            AgentStatus::Other(word) => {
                initial(word.as_str()).map_or(Mark::Unreported, Mark::Other)
            }
            AgentStatus::Unreported => Mark::Unreported,
        }
    }

    fn is_news(&self) -> bool {
        matches!(
            self,
            Mark::Finished | Mark::Passed | Mark::Failed | Mark::Ran
        )
    }

    pub fn glyph(&self, frame: usize) -> (String, Style) {
        let style = Style::new();
        match self {
            Mark::Waiting => (
                "●".to_owned(),
                style.fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Mark::Idle => ("✓".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Finished | Mark::Passed => (
                "✓".to_owned(),
                style.fg(moonlight::GREEN).add_modifier(Modifier::BOLD),
            ),
            Mark::Busy => (SPINNER[frame].to_owned(), style.fg(TAG)),
            Mark::Error | Mark::Failed => (
                "✗".to_owned(),
                style.fg(moonlight::RED).add_modifier(Modifier::BOLD),
            ),
            Mark::Shell => ("$".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Other(letter) => (letter.to_string(), style.fg(TAG)),
            Mark::Unreported => ("?".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Ran => ("$".to_owned(), style),
        }
    }
}

fn initial(word: &str) -> Option<char> {
    let initial = word.chars().next()?.to_uppercase().next()?;
    (initial.width() == Some(MARK)).then_some(initial)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Marked {
    mark: Mark,
    since: Option<SystemTime>,
}

impl Marked {
    fn of(agent: &Agent) -> Self {
        let since = agent.status_since.and_then(|at| at.try_into().ok());
        Marked {
            mark: Mark::of(agent),
            since,
        }
    }

    fn of_shell(doing: &Doing, now: SystemTime) -> Option<(Self, Option<String>)> {
        let (mark, since, command) = match doing {
            Doing::Going {
                command,
                since,
                front,
            } => {
                let since = end_of_second(*since)?;
                if now < since.checked_add(LONG)? {
                    return None;
                }
                let mark = match front {
                    Front::Working => Mark::Busy,
                    Front::Asking => Mark::Waiting,
                    Front::Interactive => return None,
                };
                (mark, since, command)
            }
            Doing::Ended {
                command,
                at,
                ending,
                took,
            } => {
                if *took < LONG {
                    return None;
                }
                let mark = match ending {
                    Some(Ending::Exited(0)) => Mark::Passed,
                    Some(Ending::Exited(_)) => Mark::Failed,
                    Some(Ending::Resumed) | None => Mark::Ran,
                };
                (mark, end_of_second(*at)?, command)
            }
            Doing::Stopped { .. } | Doing::Ready => return None,
        };
        let command = command.as_ref().map(|command| command.as_str().to_owned());
        Some((
            Marked {
                mark,
                since: Some(since),
            },
            command,
        ))
    }

    fn ended(&self) -> Option<SystemTime> {
        match self.mark {
            Mark::Passed | Mark::Failed | Mark::Ran => self.since,
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Tally {
    waiting: usize,
    dots: usize,
    busy: usize,
    shell: usize,
}

impl Tally {
    fn of(by_view: &[Shows<'_>]) -> Tally {
        let marks = || {
            by_view
                .iter()
                .filter_map(|shows| Some(&shows.marked.as_ref()?.mark))
        };
        Tally {
            waiting: marks().filter(|mark| **mark == Mark::Waiting).count(),
            dots: by_view.iter().filter(|shows| shows.dotted()).count(),
            busy: marks().filter(|mark| **mark == Mark::Busy).count(),
            shell: marks().filter(|mark| **mark == Mark::Shell).count(),
        }
    }

    fn sum(tallies: impl Iterator<Item = Tally>) -> Tally {
        tallies.fold(Tally::default(), |sum, it| Tally {
            waiting: sum.waiting + it.waiting,
            dots: sum.dots + it.dots,
            busy: sum.busy + it.busy,
            shell: sum.shell + it.shell,
        })
    }
}

struct Shows<'a> {
    view: &'a KnownView,
    marked: Option<Marked>,
    label: String,
    unread: bool,
}

impl Shows<'_> {
    fn dotted(&self) -> bool {
        dotted(self.unread, self.marked.as_ref())
    }
}

fn dotted(unread: bool, marked: Option<&Marked>) -> bool {
    unread || marked.is_some_and(|marked| marked.mark.is_news())
}

fn dot() -> Span<'static> {
    Span::styled(NEW, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

fn end_of_second(at: SystemTime) -> Option<SystemTime> {
    at.checked_add(Duration::from_millis(999))
}

fn ran(told: &Told) -> Ran {
    let doing = match told {
        Told::Unhooked => return Ran::Unhooked,
        Told::Unread => return Ran::Unknown,
        Told::Doing(doing) => doing,
    };
    match doing {
        Doing::Going { command, .. } => Ran::Going(command.clone()),
        Doing::Stopped { command, .. } => Ran::Stopped(command.clone()),
        Doing::Ended {
            command: Some(command),
            at,
            ending,
            ..
        } => Ran::Ended(menu::Ended {
            command: command.clone(),
            ending: *ending,
            at: *at,
        }),
        Doing::Ended { command: None, .. } => Ran::EndedUnsaid,
        Doing::Ready => Ran::Ready,
    }
}

fn long_at(doing: &Doing) -> Option<SystemTime> {
    match doing {
        Doing::Going { since, .. } => end_of_second(*since)?.checked_add(LONG),
        Doing::Ended { .. } | Doing::Stopped { .. } | Doing::Ready => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Machine {
        machine: Machine,
        name: String,
        away: Option<Away>,
        heads: Heads,
    },
    Project {
        machine: Machine,
        project: ProjectRow,
        hits: Vec<usize>,
        under: Under,
        here: bool,
        colour: Color,
        waiting: usize,
        dot: bool,
        busy: bool,
    },
    Worktree {
        machine: Machine,
        place: WorktreePlace,
        named: Named<Checkout>,
        missing: bool,
        hits: Vec<usize>,
        under: Under,
        here: bool,
        colour: Color,
        waiting: usize,
        dot: bool,
        busy: bool,
    },
    View {
        machine: Machine,
        place: ViewPlace,
        view_id: ViewId,
        here: bool,
        unread: bool,
        marked: Option<Marked>,
        icon: Option<&'static str>,
        label: String,
        hits: Vec<usize>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heads {
    Nothing,
    Projects {
        folded: bool,
        here: bool,
        waiting: usize,
        dot: bool,
        busy: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Checkout {
    branch: Said,
    status: String,
    changed: Option<Changed>,
    pull_request: Option<Numbered>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Named<C> {
    Unbranched(String),
    Branch(C),
    Apart { name: String, checkout: C },
}

pub trait Branched {
    fn branch(&self) -> &str;
}

impl Branched for Checkout {
    fn branch(&self) -> &str {
        self.branch.as_str()
    }
}

impl Branched for String {
    fn branch(&self) -> &str {
        self
    }
}

impl<C: Branched> Named<C> {
    #[must_use]
    pub fn of(name: String, checkout: Option<C>) -> Named<C> {
        match checkout {
            None => Named::Unbranched(name),
            Some(checkout) if checkout.branch() == name => Named::Branch(checkout),
            Some(checkout) => Named::Apart { name, checkout },
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Named::Unbranched(name) | Named::Apart { name, .. } => name,
            Named::Branch(checkout) => checkout.branch(),
        }
    }

    #[must_use]
    pub fn apart(&self) -> Option<&C> {
        match self {
            Named::Apart { checkout, .. } => Some(checkout),
            Named::Unbranched(_) | Named::Branch(_) => None,
        }
    }

    fn checkout(&self) -> Option<&C> {
        match self {
            Named::Unbranched(_) => None,
            Named::Branch(checkout) | Named::Apart { checkout, .. } => Some(checkout),
        }
    }

    fn height(&self) -> usize {
        match self {
            Named::Unbranched(_) | Named::Branch(_) => 1,
            Named::Apart { .. } => 2,
        }
    }
}

fn changed_shown(changed: Changed) -> (Vec<Span<'static>>, usize) {
    let (added, deleted) = (changed.added(), changed.deleted());
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::styled(
            format!("+{added}"),
            Style::new().fg(moonlight::GREEN),
        ));
    }
    if deleted > 0 {
        if added > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("-{deleted}"),
            Style::new().fg(moonlight::RED),
        ));
    }
    let width = spans.iter().map(Span::width).sum();
    (spans, width)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Numbered {
    number: Number,
    state: NumberedState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberedState {
    Open {
        draft: bool,
        checks: Option<Verdict>,
        conflicting: bool,
        stale: bool,
    },
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Number {
    Linked(Box<Link>),
    Unlinked(NonZeroU32),
}

impl Numbered {
    fn shown(&self) -> String {
        match &self.number {
            Number::Linked(link) => link.tab(),
            Number::Unlinked(number) => format!("#{number}"),
        }
    }

    fn link(&self) -> Option<&Link> {
        match &self.number {
            Number::Linked(link) => Some(link),
            Number::Unlinked(_) => None,
        }
    }

    fn colour(&self) -> Color {
        match self.state {
            NumberedState::Open { draft: false, .. } => moonlight::GREEN,
            NumberedState::Open { draft: true, .. } => moonlight::MUTE,
            NumberedState::Merged => moonlight::MAUVE,
        }
    }
}

impl Row {
    fn spot(&self) -> Spot {
        match self {
            Row::Machine { machine, .. } => Spot::Machine(machine.clone()),
            Row::Project {
                machine, project, ..
            } => Spot::on(machine, Place::Session(project.listed().clone())),
            Row::Worktree { machine, place, .. } => {
                Spot::on(machine, Place::Worktree(place.clone()))
            }
            Row::View { machine, place, .. } => Spot::on(machine, Place::View(place.clone())),
        }
    }

    fn is_spot(&self, spot: &Spot) -> bool {
        match self {
            Row::Machine { machine, .. } => {
                matches!(spot, Spot::Machine(other) if other == machine)
            }
            Row::Project {
                machine, project, ..
            } => {
                spot.is_on(machine)
                    && matches!(spot.place(), Some(Place::Session(other)) if other == project.listed())
            }
            Row::Worktree { machine, place, .. } => {
                spot.is_on(machine)
                    && matches!(spot.place(), Some(Place::Worktree(other)) if other == place)
            }
            Row::View { machine, place, .. } => {
                spot.is_on(machine)
                    && matches!(spot.place(), Some(Place::View(other)) if other == place)
            }
        }
    }

    fn gap(&self) -> usize {
        match self {
            Row::Project { .. } => 1,
            Row::Machine { .. } | Row::Worktree { .. } | Row::View { .. } => 0,
        }
    }

    fn own(&self) -> usize {
        match self {
            Row::Worktree { named, .. } => named.height(),
            Row::Machine { .. } | Row::Project { .. } | Row::View { .. } => 1,
        }
    }

    fn height(&self) -> usize {
        self.gap() + self.own()
    }

    fn spinning(&self) -> bool {
        match self {
            Row::Machine {
                heads:
                    Heads::Projects {
                        folded,
                        waiting,
                        dot,
                        busy,
                        ..
                    },
                ..
            } => *folded && *busy && *waiting == 0 && !*dot,
            Row::Machine {
                heads: Heads::Nothing,
                ..
            } => false,
            Row::Project {
                under,
                waiting,
                dot,
                busy,
                ..
            }
            | Row::Worktree {
                under,
                waiting,
                dot,
                busy,
                ..
            } => *under == Under::Folded && *busy && *waiting == 0 && !*dot,
            Row::View { marked, .. } => marked
                .as_ref()
                .is_some_and(|marked| marked.mark == Mark::Busy),
        }
    }

    fn since(&self) -> Option<SystemTime> {
        match self {
            Row::Machine { away, .. } => away.as_ref().and_then(|away| away.said().1),
            Row::View { marked, .. } => marked.as_ref().and_then(|marked| marked.since),
            Row::Project { .. } | Row::Worktree { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Under {
    Rows,
    Folded,
    Empty,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Away {
    Reaching,
    Here,
    Unreached {
        since: Option<SystemTime>,
        word: &'static str,
        why: String,
        ssh_said: Option<String>,
    },
}

impl Away {
    fn of(unreached: &remote::Unreached) -> Away {
        let word = match unreached.stopped {
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(255) })) => "unreachable",
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(127) })) => "no lune there",
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(1) })) => "lune there failed",
            Some(Stopped::Ended(_)) => "ssh ended",
            Some(Stopped::Unstarted(())) => "ssh did not start",
            Some(Stopped::Said(())) => "its login prints",
            Some(Stopped::Silent(())) => "no answer",
            Some(Stopped::Lost(())) => "lost",
            Some(Stopped::Unwatched(())) => "not watched",
            Some(Stopped::Unmatched(())) => "another build",
            None => "unreached",
        };
        let since = unreached.since.and_then(|at| SystemTime::try_from(at).ok());
        let ssh_said = unreached.ssh_said.clone();
        Away::Unreached {
            since,
            word,
            why: unreached.why.clone(),
            ssh_said,
        }
    }

    fn said(&self) -> (&'static str, Option<SystemTime>) {
        match self {
            Away::Reaching => ("reaching…", None),
            Away::Here => ("this machine", None),
            Away::Unreached { since, word, .. } => (word, *since),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Known {
    project: ProjectRow,
    dir: Dir,
    worktrees: Vec<KnownWorktree>,
    unopened: Unopened,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KnownWorktree {
    place: WorktreePlace,
    name: String,
    folder: Dir,
    seen: projects::Seen,
    used: Option<Used>,
    terminals: Vec<KnownTerminal>,
    arranged: Option<Arranged>,
}

impl KnownWorktree {
    fn views(&self) -> impl Iterator<Item = &KnownView> {
        self.terminals.iter().filter_map(KnownTerminal::found)
    }

    fn terminal_ids(&self) -> impl Iterator<Item = ViewId> + '_ {
        self.terminals.iter().map(KnownTerminal::id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum KnownTerminal {
    Found(KnownView),
    Unfound(ViewId),
}

impl KnownTerminal {
    fn of(terminal: &rows::Terminal) -> Self {
        match &terminal.found {
            rows::Found::Running {
                key,
                command,
                titled,
                told,
            } => KnownTerminal::Found(KnownView {
                id: terminal.id,
                key: *key,
                cwd: terminal.cwd.clone(),
                command: command.as_ref().map(|said| said.as_str().to_owned()),
                topic: titled
                    .as_ref()
                    .and_then(|titled| titled.topic.as_ref())
                    .map(|said| said.as_str().to_owned()),
                told: told.clone(),
                ended: false,
            }),
            rows::Found::Ended { key, titled } => KnownTerminal::Found(KnownView {
                id: terminal.id,
                key: *key,
                cwd: terminal.cwd.clone(),
                command: None,
                topic: titled
                    .as_ref()
                    .and_then(|titled| titled.topic.as_ref())
                    .map(|said| said.as_str().to_owned()),
                told: Told::Unhooked,
                ended: true,
            }),
            rows::Found::Unlisted(key) => KnownTerminal::Found(KnownView {
                id: terminal.id,
                key: *key,
                cwd: terminal.cwd.clone(),
                command: None,
                topic: None,
                told: Told::Unhooked,
                ended: false,
            }),
            rows::Found::Lost | rows::Found::Looking => KnownTerminal::Unfound(terminal.id),
        }
    }

    fn id(&self) -> ViewId {
        match self {
            KnownTerminal::Found(view) => view.id,
            KnownTerminal::Unfound(id) => *id,
        }
    }

    fn found(&self) -> Option<&KnownView> {
        match self {
            KnownTerminal::Found(view) => Some(view),
            KnownTerminal::Unfound(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KnownView {
    id: ViewId,
    key: TerminalKey,
    cwd: Dir,
    command: Option<String>,
    topic: Option<String>,
    told: Told,
    ended: bool,
}

impl Known {
    fn of(said: &rows::Project) -> Known {
        let listed = Listed {
            name: said.name.clone(),
            kept: ledger::Session::Project(said.id),
        };
        let project = ProjectRow::of(listed);
        let worktrees = said.worktrees().iter().map(|worktree| KnownWorktree {
            place: WorktreePlace {
                project: project.clone(),
                worktree: worktree.id,
            },
            name: worktree.name.clone(),
            folder: worktree.folder.clone(),
            seen: worktree.seen,
            used: worktree.used,
            terminals: worktree.terminals.iter().map(KnownTerminal::of).collect(),
            arranged: worktree.arranged,
        });
        let worktrees = worktrees.collect();
        let unopened = match &said.holds {
            rows::Holds::Folder(_) => Unopened::Folder,
            rows::Holds::Clone {
                unopened: rows::Unopened::Listed(folders),
                ..
            } => Unopened::Listed(folders.clone()),
            rows::Holds::Clone {
                unopened: rows::Unopened::Unlisted(why),
                ..
            } => Unopened::Unlisted(why.clone()),
        };
        Known {
            project,
            dir: said.dir.clone(),
            worktrees,
            unopened,
        }
    }

    fn listed(&self) -> &Listed {
        self.project.listed()
    }

    fn order(&self) -> Vec<WorktreeId> {
        self.worktrees
            .iter()
            .map(|worktree| worktree.place.worktree)
            .collect()
    }

    fn terminals(&self) -> impl Iterator<Item = ViewId> {
        self.worktrees.iter().flat_map(KnownWorktree::terminal_ids)
    }

    fn views(&self) -> impl Iterator<Item = &KnownView> {
        self.worktrees.iter().flat_map(KnownWorktree::views)
    }

    fn placed_views(&self) -> impl Iterator<Item = (&WorktreePlace, &KnownView)> {
        let each = self.worktrees.iter();
        each.flat_map(|worktree| worktree.views().map(move |view| (&worktree.place, view)))
    }

    fn view(&self, key: TerminalKey) -> Option<&KnownView> {
        self.views().find(|view| view.key == key)
    }
}

fn project_of(spot: &Spot) -> Option<Spot> {
    let listed = spot.place()?.listed().clone();
    Some(Spot::on(&spot.machine(), Place::Session(listed)))
}

fn closed_by(act: &Act, on: &Machine, spot: &Spot) -> bool {
    if spot.machine() != *on {
        return false;
    }
    let Some(place) = spot.place() else {
        return false;
    };
    act.closes(place)
}

fn arranged_holding(
    shelves: &[Shelf],
    seat: Option<&Seat>,
    on: &Machine,
    listed: &Listed,
    view: ViewId,
) -> Option<Arranged> {
    let shelf = shelves.iter().find(|shelf| shelf.machine == *on)?;
    let known = shelf.projects().iter().find(|it| it.listed() == listed)?;
    let holding = known
        .worktrees
        .iter()
        .find(|it| it.terminal_ids().any(|id| id == view))?;
    let seated =
        seat.filter(|seat| seat.on == *on && seat.worktree() == Some(holding.place.worktree));
    let live = seated
        .and_then(Seat::live_arranged)
        .and_then(|(arranged, version)| {
            let current = match (version, shelf.version) {
                (Some(seat_version), Some(shelf_version)) => {
                    seat_version.pid == shelf_version.pid && seat_version.seq >= shelf_version.seq
                }
                (None, _) | (_, None) => true,
            };
            current.then_some(arranged)
        });
    live.or(holding.arranged)
}

fn view_id_of(shelves: &[Shelf], on: &Machine, listed: &Listed, view: ViewKey) -> Option<ViewId> {
    let ViewKey::Terminal(key) = view;
    let known = shelf(shelves, on).iter().find(|it| it.listed() == listed)?;
    known.view(key).map(|view| view.id)
}

fn news_key(shelves: &[Shelf], on: &Machine, place: &ViewPlace) -> Option<Key> {
    let listed = place.listed();
    Some(listed.key(view_id_of(shelves, on, listed, place.view)?))
}

#[derive(Debug, Default)]
struct Shelf {
    machine: Machine,
    version: Option<api::Version>,
    shelving: Shelving,
}

#[derive(Debug, Default)]
enum Shelving {
    #[default]
    Unkept,
    Kept(Vec<Known>),
    Refused(String),
}

impl Shelf {
    fn kept(&self) -> Option<&[Known]> {
        match &self.shelving {
            Shelving::Kept(projects) => Some(projects),
            Shelving::Unkept | Shelving::Refused(_) => None,
        }
    }

    fn projects(&self) -> &[Known] {
        self.kept().unwrap_or_default()
    }

    fn projects_mut(&mut self) -> &mut [Known] {
        match &mut self.shelving {
            Shelving::Kept(projects) => projects,
            Shelving::Unkept | Shelving::Refused(_) => &mut [],
        }
    }

    fn refused(&self) -> Option<String> {
        let Shelving::Refused(why) = &self.shelving else {
            return None;
        };
        Some(match &self.machine {
            Machine::Local => format!("luned's projects: {why}"),
            Machine::Remote(destination) => format!("{destination}'s projects: {why}"),
        })
    }

    fn known(&self, project: ProjectId) -> Option<&Known> {
        self.projects()
            .iter()
            .find(|known| known.project.id() == project)
    }

    fn order(&self) -> Vec<ProjectId> {
        self.projects()
            .iter()
            .map(|known| known.project.id())
            .collect()
    }

    fn orders(&self, order: &Order) -> bool {
        match order {
            Order::Projects(order) => permutes(&self.order(), order),
            Order::Worktrees { project, worktrees } => self
                .known(*project)
                .is_some_and(|known| permutes(&known.order(), worktrees)),
        }
    }

    fn reorder(&mut self, order: &Order) {
        assert!(self.orders(order), "an order reorders what is kept");
        match order {
            Order::Projects(order) => {
                self.projects_mut()
                    .sort_by_key(|known| position(order, known.project.id()));
            }
            Order::Worktrees { project, worktrees } => {
                let mut known = self.projects_mut().iter_mut();
                let known = known.find(|known| known.project.id() == *project);
                let known = known.expect("the project reordered is shelved");
                let at = |worktree: &KnownWorktree| position(worktrees, worktree.place.worktree);
                known.worktrees.sort_by_key(at);
            }
        }
    }
}

fn position<I: PartialEq + Copy>(order: &[I], id: I) -> usize {
    let at = order.iter().position(|it| *it == id);
    at.expect("an order holds every id it reorders")
}

fn permutes<I: Ord + Copy>(kept: &[I], order: &[I]) -> bool {
    let (mut kept, mut order) = (kept.to_vec(), order.to_vec());
    kept.sort_unstable();
    order.sort_unstable();
    kept == order
}

fn shelved(machine: Machine, projects: Option<&api::Projects>) -> Shelf {
    let Some(api::projects::Held::Kept(kept)) = projects.and_then(|it| it.held.as_ref()) else {
        return Shelf {
            machine,
            ..Shelf::default()
        };
    };
    let shelving = match rows::listed(&kept.projects) {
        Ok(listed) => {
            assert_eq!(
                listed.len(),
                kept.projects.len(),
                "a listing read is read whole"
            );
            Shelving::Kept(listed.iter().map(Known::of).collect())
        }
        Err(why) => Shelving::Refused(why),
    };
    Shelf {
        machine,
        version: kept.version,
        shelving,
    }
}

fn wanted<I: PartialEq + Copy>(all: &[I], listed: &[I]) -> Vec<I> {
    let mut slots = listed.iter();
    let slot = |id: &I, slots: &mut std::slice::Iter<'_, I>| {
        if listed.contains(id) {
            slots.next().copied().unwrap_or(*id)
        } else {
            *id
        }
    };
    all.iter().map(|id| slot(id, &mut slots)).collect()
}

fn shelves(host: &Host) -> Vec<Shelf> {
    let mut shelves = vec![shelved(Machine::Local, host.projects.as_ref())];
    for (remote, destination) in host.remotes.iter().zip(machines::named_once(&host.remotes)) {
        let Some(destination) = destination else {
            continue;
        };
        let projects = match &remote.reached {
            Some(Reached::Host(host)) => host.projects.as_ref(),
            Some(Reached::Unreached(unreached)) => unreached.projects.as_ref(),
            Some(Reached::Reaching(()) | Reached::Here(())) | None => None,
        };
        shelves.push(shelved(Machine::Remote(destination), projects));
    }
    shelves
}

fn shelf<'a>(shelves: &'a [Shelf], machine: &Machine) -> &'a [Known] {
    let found = shelves.iter().find(|shelf| shelf.machine == *machine);
    found.map_or(&[], Shelf::projects)
}

struct Seen<'a> {
    projects: &'a [Known],
    agents: &'a [Agent],
    git_statuses: &'a [luned::api::GitStatus],
    pull_requests: &'a [luned::api::PullRequest],
    unread: u32,
    unread_files: &'a [UnreadFile],
    sayings: &'a [Saying],
}

impl Seen<'_> {
    fn unread_seats(&self) -> Result<Vec<Unread>, unread::Unplaced> {
        seats::unread(self.unread, self.unread_files)
    }

    fn seated(&self) -> HashMap<TerminalKey, &Agent> {
        seated(self.agents)
    }

    fn held_by(&self, listed: &Listed) -> Vec<TerminalKey> {
        let known = self
            .projects
            .iter()
            .filter(|known| known.listed() == listed);
        known.flat_map(Known::views).map(|view| view.key).collect()
    }

    fn views(&self) -> impl Iterator<Item = (&WorktreePlace, &KnownView)> {
        self.projects.iter().flat_map(Known::placed_views)
    }
}

struct Machines<'a> {
    here: Seen<'a>,
    others: Vec<(Destination, Result<Seen<'a>, Away>)>,
    left_out: usize,
}

impl<'a> Machines<'a> {
    fn each(self) -> impl Iterator<Item = (Machine, Result<Seen<'a>, Away>)> {
        let others = self.others.into_iter();
        let others = others.map(|(destination, seen)| (Machine::Remote(destination), seen));
        std::iter::once((Machine::Local, Ok(self.here))).chain(others)
    }
}

fn machines<'a>(host: &'a Host, shelves: &'a [Shelf]) -> Machines<'a> {
    let here = Seen {
        projects: shelf(shelves, &Machine::Local),
        agents: &host.agents,
        git_statuses: &host.git_statuses,
        pull_requests: &host.pull_requests,
        unread: host.unread,
        unread_files: &host.unread_files,
        sayings: &host.sayings,
    };
    let mut others = Vec::new();
    let mut named = HashSet::new();
    let mut left_out = 0;
    for remote in &host.remotes {
        let destination = match remote.destination.parse::<Destination>() {
            Ok(destination) if named.insert(destination.clone()) => destination,
            Ok(_) | Err(_) => {
                left_out += 1;
                continue;
            }
        };
        let machine = Machine::Remote(destination.clone());
        let seen = match &remote.reached {
            Some(Reached::Host(host)) => Ok(Seen {
                projects: shelf(shelves, &machine),
                agents: &host.agents,
                git_statuses: &host.git_statuses,
                pull_requests: &host.pull_requests,
                unread: host.unread,
                unread_files: &host.unread_files,
                sayings: &host.sayings,
            }),
            Some(Reached::Reaching(())) => Err(Away::Reaching),
            Some(Reached::Here(())) => Err(Away::Here),
            Some(Reached::Unreached(unreached)) => Err(Away::of(unreached)),
            None => Err(Away::Unreached {
                since: None,
                word: "unreached",
                why: "luned says how it stands in words this lune cannot read, as a newer \
                      luned's"
                    .to_owned(),
                ssh_said: None,
            }),
        };
        others.push((destination, seen));
    }
    Machines {
        here,
        others,
        left_out,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    Nothing,
    OpenWorktree {
        on: Machine,
        project: ProjectRow,
    },
    Prunes {
        menu: u64,
        on: Machine,
        dir: Directory,
    },
    Prune {
        menu: u64,
        on: Machine,
        dir: Directory,
        agreed: BTreeSet<prunes::Deleting>,
    },
    Rebase {
        menu: u64,
        on: Machine,
        dir: Directory,
        branch: Said,
    },
    Go {
        on: Machine,
        place: Place,
    },
    Take,
    Copy(Link),
    CopyText(Excerpt),
    Browse(Link),
    Leave,
    Close {
        on: Machine,
        act: Act,
        ticket: u64,
    },
    Open {
        asking: Held,
        on: Machine,
        project: ProjectId,
        beside: ViewKey,
        opens: Opens,
    },
    NewWorktree {
        on: Machine,
        project: ProjectId,
        branch: Branch,
    },
    Welcome {
        asking: Held,
        on: Machine,
        session: Listed,
        view: ViewKey,
    },
    Browser {
        asking: Held,
        on: Machine,
        place: menu::BrowserPlace,
        url: Option<lune_domain::projects::BrowserUrl>,
    },
    Review {
        asking: Held,
        on: Machine,
        session: Listed,
        view: ViewKey,
    },
    Arrange {
        asking: Held,
        on: Machine,
        session: Listed,
        view: ViewKey,
        arranged: Arranged,
    },
    OpenIn {
        asking: Held,
        on: Machine,
        worktree: WorktreeId,
        command: Argv,
    },
    Rename {
        on: Machine,
        rename: Rename,
    },
    Make {
        menu: u64,
        on: Machine,
        cwd: Directory,
        name: String,
        agent: Option<Argv>,
    },
    Pull {
        menu: u64,
        on: Machine,
        work: Work,
        wanted: Wanted,
        worked: Vec<Worked>,
        others: Others,
    },
    Find {
        menu: u64,
        on: Machine,
        dir: Directory,
        agent: agents::Harness,
    },
    Printed {
        menu: u64,
        on: Machine,
        session: Listed,
        view: ViewKey,
        ended: menu::Ended,
    },
    Checks {
        menu: u64,
        on: Machine,
        page: checks::PullPage,
    },
    Brief(Brief),
    Ask(Target),
    Issue(Repository),
    Ssh(Destination),
    Forget(Destination),
    Move {
        on: Machine,
        order: Reorder,
        ticket: u64,
    },
    Quit,
}

impl Asked {
    fn menu(&self) -> Option<u64> {
        match self {
            Asked::Prunes { menu, .. }
            | Asked::Prune { menu, .. }
            | Asked::Rebase { menu, .. }
            | Asked::Make { menu, .. }
            | Asked::Pull { menu, .. }
            | Asked::Find { menu, .. }
            | Asked::Printed { menu, .. }
            | Asked::Checks { menu, .. } => Some(*menu),
            Asked::Nothing
            | Asked::OpenWorktree { .. }
            | Asked::Go { .. }
            | Asked::Take
            | Asked::Copy(_)
            | Asked::CopyText(_)
            | Asked::Browse(_)
            | Asked::Leave
            | Asked::Close { .. }
            | Asked::Open { .. }
            | Asked::NewWorktree { .. }
            | Asked::Welcome { .. }
            | Asked::Browser { .. }
            | Asked::Review { .. }
            | Asked::Arrange { .. }
            | Asked::OpenIn { .. }
            | Asked::Rename { .. }
            | Asked::Brief(_)
            | Asked::Ask(_)
            | Asked::Issue(_)
            | Asked::Ssh(_)
            | Asked::Forget(_)
            | Asked::Move { .. }
            | Asked::Quit => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    Open,
    Kept,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placed {
    Sidebar(Pause),
    Afloat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Taken {
    Open(Placed),
    Kept,
}

impl Placed {
    fn of(origin: Origin) -> Placed {
        match origin {
            Origin::Sidebar => Placed::Sidebar(Pause::Nothing),
            Origin::Views => Placed::Afloat,
        }
    }
}

impl Taken {
    fn held(self) -> Held {
        match self {
            Taken::Open(_) => Held::Open,
            Taken::Kept => Held::Kept,
        }
    }
}

const HEADER: usize = 2;

const TRAILING: usize = 1;

const TITLE_GAP_MIN: usize = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keys {
    #[default]
    Terminal,
    Sidebar,
    Lent,
}

impl Keys {
    fn held(self) -> bool {
        match self {
            Keys::Sidebar | Keys::Lent => true,
            Keys::Terminal => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holding {
    Nothing,
    Chord,
    Search,
    Reorder,
    Menu,
    Afloat,
    Press,
}

#[derive(Default)]
enum Mode {
    #[default]
    Idle,
    Pending(Pending),
    Search(Search),
    Reorder(Movable),
    Menu {
        menu: Menu,
        pause: Pause,
    },
    Afloat {
        menu: Menu,
        said: Option<String>,
    },
    AfloatNote(String),
}

pub enum Floating<'a> {
    Menu(&'a Menu),
    Note(&'a str),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    projects: usize,
    views: usize,
    waiting: usize,
    new: usize,
    busy: usize,
    shell: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SidebarView {
    Collapsed {
        away: bool,
        tinted: bool,
        tallies: (usize, usize, usize),
        paused: bool,
        title: Option<String>,
    },
    Expanded(Box<ExpandedView>),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ExpandedView {
    framed: bool,
    has_host: bool,
    empty: bool,
    counts: Counts,
    top: usize,
    rows: Vec<(usize, Row)>,
    lit: Option<usize>,
    here: Option<Spot>,
    holding: Holding,
    title: Option<String>,
    notes: Vec<Line<'static>>,
}

mod sealed {
    use super::SidebarState;

    pub struct Sidebar {
        state: SidebarState,
    }

    impl Sidebar {
        pub(super) fn from_state(state: SidebarState) -> Self {
            Sidebar { state }
        }

        pub(super) fn state(&self) -> &SidebarState {
            &self.state
        }

        pub(super) fn edit<R>(&mut self, change: impl FnOnce(&mut SidebarState) -> R) -> R {
            let result = change(&mut self.state);
            self.state.relayout();
            result
        }

        pub(super) fn edit_if(&mut self, change: impl FnOnce(&mut SidebarState) -> bool) -> bool {
            let changed = change(&mut self.state);
            if changed {
                self.state.relayout();
            }
            changed
        }

        pub(super) fn edit_without_layout<R>(
            &mut self,
            change: impl FnOnce(&mut SidebarState) -> R,
        ) -> R {
            change(&mut self.state)
        }
    }
}

pub use sealed::Sidebar;

pub struct SidebarState {
    host: Option<Host>,
    this: String,
    away: Option<String>,
    seat: Option<Seat>,
    seat_name: Option<String>,
    folds: Folds,
    news: News,
    agent: Kept,
    shelves: Vec<Shelf>,
    unheard: Vec<Unheard>,
    tickets: u64,
    here_key: Option<Key>,
    finished: HashMap<Key, SystemTime>,
    view_titles: HashMap<Machine, HashMap<WorktreeId, BTreeMap<ViewId, String>>>,
    view_titles_revision: u64,
    rows: Vec<Row>,
    counts: Counts,
    refused: Vec<String>,
    unlisted_machines: usize,
    keys: Keys,
    here_row: Option<Spot>,
    lit: Option<Lit>,
    mode: Mode,
    viewport: Viewport,
    revealed: Option<Spot>,
    notice: Option<String>,
    copied: Option<(Copied, Instant)>,
    image_going: Option<Destination>,
    aloud: Option<aloud::Shown>,
    working: BTreeMap<u64, Menu>,
    filing: BTreeMap<u64, String>,
    filed: Option<String>,
    menus: u64,
    due: Option<SystemTime>,
    anomalies: Troubles,
}

#[derive(Debug)]
enum Lit {
    On(Spot),
    Closing {
        spot: Spot,
        ticket: u64,
        landing: Option<Spot>,
    },
}

impl Lit {
    fn spot(&self) -> &Spot {
        match self {
            Lit::On(spot) | Lit::Closing { spot, .. } => spot,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChordWait {
    Until(Instant),
    Shown,
}

#[derive(Debug)]
enum Pending {
    Legend,
    First(ChordWait),
    Group { key: char, wait: ChordWait },
    AllKeys(AllKeys),
}

#[derive(Clone, Copy, Debug)]
enum Came {
    Typed,
    Read,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pause {
    #[default]
    Nothing,
    Until(Instant),
    Quiet,
}

impl Pause {
    fn pauses(self, menu: &Menu, steady: Instant) -> bool {
        match self {
            Pause::Nothing => false,
            Pause::Until(shows) => steady < shows,
            Pause::Quiet => menu.quiet(),
        }
    }

    fn is_paused(self, menu: &Menu) -> bool {
        match self {
            Pause::Nothing => false,
            Pause::Until(_) => true,
            Pause::Quiet => menu.quiet(),
        }
    }
}

#[derive(Debug)]
enum Pressed {
    Space,
    Row(Spot),
    Movable(Movable),
}

impl Pressed {
    fn spot(&self) -> Option<Spot> {
        match self {
            Pressed::Space => None,
            Pressed::Row(spot) => Some(spot.clone()),
            Pressed::Movable(movable) => Some(movable.spot()),
        }
    }

    fn movable(&self) -> Option<&Movable> {
        match self {
            Pressed::Space | Pressed::Row(_) => None,
            Pressed::Movable(movable) => Some(movable),
        }
    }
}

#[derive(Debug, Default)]
enum Moving {
    #[default]
    Still,
    Down {
        at: Position,
        on: Pressed,
    },
    Lifted(Lift),
    Sweeping(Sweep),
    Swept(Sweep, Layout),
}

static EMPTY_ALOUD_BUTTONS: Spots<aloud::Button> = Spots::empty();

#[derive(Debug)]
enum Viewport {
    Collapsed { area: Rect, top: usize },
    Expanded { laid: Layout, moving: Moving },
}

impl Viewport {
    fn expanded_area(&self) -> Option<Rect> {
        match self {
            Viewport::Collapsed { .. } => None,
            Viewport::Expanded { laid, .. } => Some(laid.area),
        }
    }

    fn laid(&self) -> Option<&Layout> {
        match self {
            Viewport::Collapsed { .. } => None,
            Viewport::Expanded { laid, .. } => Some(laid),
        }
    }

    fn top(&self) -> usize {
        match self {
            Viewport::Collapsed { top, .. } => *top,
            Viewport::Expanded { laid, .. } => laid.top,
        }
    }

    fn moving(&self) -> Option<&Moving> {
        match self {
            Viewport::Collapsed { .. } => None,
            Viewport::Expanded { moving, .. } => Some(moving),
        }
    }

    fn moving_mut(&mut self) -> Option<&mut Moving> {
        match self {
            Viewport::Collapsed { .. } => None,
            Viewport::Expanded { moving, .. } => Some(moving),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layout {
    area: Rect,
    room: usize,
    top: usize,
    rows: Vec<(Spot, usize)>,
    notes_y: u16,
    notes: Vec<String>,
    aloud_buttons: Spots<aloud::Button>,
}

#[derive(Clone, Copy)]
struct NotesGeometry {
    room: usize,
    notes_from: usize,
    notes_y: u16,
}

fn notes_geometry(area: Rect, notes_lines: usize) -> NotesGeometry {
    let height = usize::from(area.height);
    let room = height.saturating_sub(HEADER + notes_lines);
    let notes_from = height.saturating_sub(notes_lines);
    let notes_line = u16::try_from(notes_from).expect("the notes start inside the area");
    let notes_y = area
        .y
        .checked_add(notes_line)
        .expect("an area ends inside the screen");
    NotesGeometry {
        room,
        notes_from,
        notes_y,
    }
}

struct NotesAt {
    notes: Notes,
    bottom: SidebarBottom,
    buttons: Spots<aloud::Button>,
}

struct Rendered {
    page: Page,
    drawn: Drawn,
    cadence: Cadence,
}

impl Layout {
    fn keeps(&self, sweep: Sweep, laid: &Layout) -> bool {
        let rows_stay = self.area == laid.area && self.top == laid.top && self.rows == laid.rows;
        let notes_stay = self.notes_y == laid.notes_y && self.notes == laid.notes;
        match (rows_stay, notes_stay) {
            (true, true) => true,
            (true, false) => sweep.last_y() < self.notes_y.min(laid.notes_y),
            (false, _) => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Movable {
    Project(Machine, ProjectRow),
    Worktree(Machine, WorktreePlace),
}

impl Movable {
    fn of(spot: Spot) -> Option<Movable> {
        let machine = spot.machine();
        match spot {
            Spot::Machine(_) => None,
            Spot::Local(place) | Spot::Remote(_, place) => match place {
                Place::Session(session) => Some(Movable::Project(machine, ProjectRow::of(session))),
                Place::Worktree(place) => Some(Movable::Worktree(machine, place)),
                Place::View(_) => None,
            },
        }
    }

    fn spot(&self) -> Spot {
        match self {
            Movable::Project(machine, project) => {
                Spot::on(machine, Place::Session(project.listed().clone()))
            }
            Movable::Worktree(machine, place) => Spot::on(machine, Place::Worktree(place.clone())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Spans {
    Projects(Machine, Vec<(ProjectId, Range<usize>)>),
    Worktrees(Machine, ProjectRow, Vec<(WorktreeId, Range<usize>)>),
}

impl Spans {
    fn len(&self) -> usize {
        match self {
            Spans::Projects(_, projects) => projects.len(),
            Spans::Worktrees(_, _, worktrees) => worktrees.len(),
        }
    }

    fn rows(&self) -> Vec<Range<usize>> {
        match self {
            Spans::Projects(_, projects) => spanned_rows(projects),
            Spans::Worktrees(_, _, worktrees) => spanned_rows(worktrees),
        }
    }

    fn moved(&self, taken: Range<usize>, to: usize) -> Spans {
        match self {
            Spans::Projects(machine, projects) => {
                Spans::Projects(machine.clone(), moved(projects, taken, to))
            }
            Spans::Worktrees(machine, project, worktrees) => Spans::Worktrees(
                machine.clone(),
                project.clone(),
                moved(worktrees, taken, to),
            ),
        }
    }
}

fn spanned_rows<T>(spans: &[(T, Range<usize>)]) -> Vec<Range<usize>> {
    spans.iter().map(|(_, rows)| rows.clone()).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Folding {
    Machine,
    Project(ProjectId),
    Worktree(WorktreeId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reorder {
    Projects {
        seen: Vec<ProjectId>,
        wanted: Vec<ProjectId>,
    },
    Worktrees {
        project: ProjectId,
        seen: Vec<WorktreeId>,
        wanted: Vec<WorktreeId>,
    },
}

impl Reorder {
    fn seen(&self) -> Order {
        match self {
            Reorder::Projects { seen, .. } => Order::Projects(seen.clone()),
            Reorder::Worktrees { project, seen, .. } => Order::Worktrees {
                project: *project,
                worktrees: seen.clone(),
            },
        }
    }

    fn wanted(&self) -> Order {
        match self {
            Reorder::Projects { wanted, .. } => Order::Projects(wanted.clone()),
            Reorder::Worktrees {
                project, wanted, ..
            } => Order::Worktrees {
                project: *project,
                worktrees: wanted.clone(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Order {
    Projects(Vec<ProjectId>),
    Worktrees {
        project: ProjectId,
        worktrees: Vec<WorktreeId>,
    },
}

impl Order {
    fn of(&self) -> Ordered {
        match self {
            Order::Projects(_) => Ordered::Projects,
            Order::Worktrees { project, .. } => Ordered::Worktrees(*project),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ordered {
    Projects,
    Worktrees(ProjectId),
}

pub const MOVES_MAX: usize = 64;

#[derive(Debug)]
struct Unheard {
    ticket: u64,
    on: Machine,
    shown: Order,
    wait: Wait,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Wait {
    Answers {
        asked: NonZeroUsize,
        landed: Option<api::Version>,
    },
    Watch(api::Version),
}

impl Wait {
    fn asked(self) -> usize {
        match self {
            Wait::Answers { asked, .. } => asked.get(),
            Wait::Watch(_) => 0,
        }
    }

    fn again(self) -> Wait {
        match self {
            Wait::Answers { asked, landed } => {
                let asked = asked
                    .checked_add(1)
                    .expect("fewer than 2^64 moves are asked");
                Wait::Answers { asked, landed }
            }
            Wait::Watch(landed) => Wait::Answers {
                asked: NonZeroUsize::MIN,
                landed: Some(landed),
            },
        }
    }

    fn answered(self, kept: Option<api::Version>) -> Option<Wait> {
        let Wait::Answers { asked, .. } = self else {
            unreachable!("luned answers each move asked once");
        };
        Wait::left(asked, kept)
    }

    fn unasked(self) -> Option<Wait> {
        let Wait::Answers { asked, landed } = self else {
            unreachable!("a move goes unsent right as it is asked");
        };
        Wait::left(asked, landed)
    }

    fn left(asked: NonZeroUsize, landed: Option<api::Version>) -> Option<Wait> {
        match NonZeroUsize::new(asked.get() - 1) {
            Some(asked) => Some(Wait::Answers { asked, landed }),
            None => landed.map(Wait::Watch),
        }
    }

    fn holds(self, heard: Option<&api::Version>) -> bool {
        match self {
            Wait::Answers { .. } => true,
            Wait::Watch(landed) => {
                heard.is_some_and(|heard| heard.pid == landed.pid && heard.seq < landed.seq)
            }
        }
    }
}

#[derive(Debug)]
struct Lift {
    lifted: Movable,
    line: u16,
    over: bool,
    scrolled: Option<Instant>,
}

enum Search {
    Empty,
    Typed { term: String, seek: bool },
}

#[derive(Debug, PartialEq, Eq)]
enum Rebuilt {
    Done,
    Unfound,
}

#[derive(Clone, Copy)]
enum Highlight {
    Lit(Option<usize>),
    Resting(usize),
    Off,
}

impl Sidebar {
    pub fn new(folds: Folds, news: News, agent: Kept, this: String, framed: Rect) -> Self {
        Sidebar::from_state(SidebarState::new(folds, news, agent, this, framed))
    }

    pub fn place(&mut self, framed: Rect) {
        self.edit_without_layout(|state| state.place_rows(framed_rows(framed)));
    }

    pub fn collapse(&mut self, framed: Rect) {
        self.edit_without_layout(|state| state.collapse_rows(framed_rows(framed)));
    }

    pub fn reagent(&mut self) {
        self.edit_without_layout(SidebarState::reagent);
    }

    #[must_use]
    pub fn refold(&mut self, wall: SystemTime) -> bool {
        self.edit_if(|state| state.refold(wall))
    }

    #[must_use]
    pub fn reread(&mut self, wall: SystemTime) -> bool {
        self.edit_if(|state| state.reread(wall))
    }

    pub fn heard(&mut self, host: Host, wall: SystemTime) {
        self.edit(|state| state.heard(host, wall));
    }

    pub fn moved(&mut self, ticket: u64, kept: Option<api::Version>, wall: SystemTime) {
        self.edit(|state| state.moved(ticket, kept, wall));
    }

    pub fn unsent(&mut self, ticket: u64, order: &Reorder, why: &str, wall: SystemTime) {
        self.edit(|state| state.unsent(ticket, order, why, wall));
    }

    pub fn unmoved(
        &mut self,
        on: &Machine,
        ticket: u64,
        order: &Reorder,
        note: String,
        wall: SystemTime,
    ) {
        self.edit(|state| state.unmoved(on, ticket, order, note, wall));
    }

    #[must_use]
    pub fn ticked(&mut self, now: Moment, watched: bool) -> bool {
        self.edit_if(|state| state.ticked(now, watched))
    }

    #[must_use]
    pub fn lost(&mut self, why: String) -> bool {
        self.edit_if(|state| state.lost(why))
    }

    pub fn focus(&mut self, keys: Keys, wall: SystemTime) {
        self.edit(|state| state.focus(keys, wall));
    }

    pub fn holding(&self) -> Holding {
        self.state().holding()
    }

    pub fn floating(&self) -> Option<Floating<'_>> {
        self.state().floating()
    }

    pub fn float_note(&mut self, note: String) {
        self.edit(|state| state.float_note(note));
    }

    pub fn close_note(&mut self) {
        let _ = self.edit_if(SidebarState::close_note);
    }

    pub fn close_menu(&mut self) {
        let _ = self.edit_if(SidebarState::close_menu);
    }

    pub fn drop_menu(&mut self, menu: u64) {
        let _ = self.edit_if(|state| state.drop_menu(menu));
    }

    pub fn sit(&mut self, seat: Seat, now: SystemTime) {
        let _ = self.edit_if(|state| state.sit(seat, now));
    }

    pub fn unseat(&mut self, now: SystemTime) {
        let _ = self.edit_if(|state| state.unseat(now));
    }

    pub fn own_title(&self) -> Option<&str> {
        self.state().own_title()
    }

    #[must_use]
    pub fn view_titles(
        &self,
        on: &Machine,
        worktree: WorktreeId,
    ) -> Option<&BTreeMap<ViewId, String>> {
        self.state().view_titles(on, worktree)
    }

    #[must_use]
    pub fn view_titles_revision(&self) -> u64 {
        self.state().view_titles_revision()
    }

    pub fn on(&self) -> &Machine {
        self.state().on()
    }

    pub fn here_agent(&self) -> Option<HereAgent> {
        self.state().here_agent()
    }

    pub fn on_header(&self) -> Option<Machine> {
        self.state().on_header()
    }

    pub fn listing(&self) -> Listing {
        self.state().listing()
    }

    pub fn reaches(&self) -> Option<Vec<(Machine, Reach)>> {
        self.state().reaches()
    }

    pub fn whereabouts(&self) -> Option<Here> {
        self.state().whereabouts()
    }

    pub fn far_agent(&self) -> Option<&Destination> {
        self.state().far_agent()
    }

    pub fn sayings(&self) -> Option<Vec<(Machine, Option<Vec<Saying>>)>> {
        self.state().sayings()
    }

    #[must_use]
    pub fn aloud(&mut self, shown: Option<aloud::Shown>) -> bool {
        self.edit_if(|state| state.aloud(shown))
    }

    pub fn aloud_buttons(&self) -> &Spots<aloud::Button> {
        self.state().aloud_buttons()
    }

    pub fn notify(&mut self, notice: String) {
        self.edit(|state| state.notify(notice));
    }

    #[must_use]
    pub fn clear_notice(&mut self) -> bool {
        self.edit_if(SidebarState::clear_notice)
    }

    #[must_use]
    pub fn unsay(&mut self, notice: &str) -> bool {
        self.edit_if(|state| state.unsay(notice))
    }

    pub fn filing(&mut self, number: u64, repository: &str) {
        self.edit(|state| state.filing(number, repository));
    }

    pub fn filed(&mut self, number: u64, repository: &str, filed: &Result<Filed, Unfiled>) {
        self.edit(|state| state.filed(number, repository, filed));
    }

    #[must_use]
    pub fn image_going(&mut self, to: Option<Destination>) -> bool {
        self.edit_if(|state| state.image_going(to))
    }

    #[cfg(test)]
    pub fn turning(&self) -> bool {
        self.state().turning()
    }

    #[must_use]
    pub fn view(&self) -> SidebarView {
        self.state().view()
    }

    pub fn key(&mut self, key: &KeyEvent, now: Moment) -> Asked {
        self.edit(|state| state.key_from(key, now, Came::Typed))
    }

    #[must_use]
    pub fn due(&self, now: Moment) -> Option<Instant> {
        self.state().due(now)
    }

    pub fn number_clicked(&mut self, link: Link) -> Asked {
        self.edit(|state| state.number_clicked(link))
    }

    pub fn copied(&mut self, copied: Copied, steady: Instant) {
        self.edit(|state| state.copied(copied, steady));
    }

    pub fn float_menu(&mut self) {
        self.edit(SidebarState::float_menu);
    }

    pub fn aimed_row(&self) -> Aim {
        self.state().aimed_row()
    }

    pub fn aimed_view(&self) -> Aim {
        self.state().aimed_view()
    }

    pub fn aimed_worktree(&self) -> Aim {
        self.state().aimed_worktree()
    }

    pub fn act_on_row(&mut self, target: &Target, action: Action, wall: SystemTime) -> Asked {
        self.edit(|state| state.act_on_row(target, action, wall))
    }

    pub fn act_in_menu(&mut self, target: &Target, action: Action, origin: Origin) -> Asked {
        self.edit(|state| state.act_in_menu(target, action, origin))
    }

    pub fn chained(&mut self, done: Done, origin: Origin) -> Asked {
        self.edit(|state| state.chained(done, origin))
    }

    #[must_use]
    pub fn paste(&mut self, text: &str, wall: SystemTime) -> Asked {
        self.edit(|state| state.paste(text, wall))
    }

    pub fn pull_in_menu(
        &mut self,
        target: Target,
        work: Work,
        pull: Pull,
        origin: Origin,
    ) -> Asked {
        self.edit(|state| state.pull_in_menu(target, work, pull, origin))
    }

    pub fn chose(&mut self, menu: u64, choice: Choice, now: Moment) -> Asked {
        self.edit(|state| state.chose(menu, choice, now))
    }

    pub fn keys_clicked(&mut self, keyed: Keyed, key: &KeyEvent, now: Moment) -> Asked {
        self.edit(|state| state.keys_clicked(keyed, key, now))
    }

    pub fn scroll_menu(&mut self, menu: u64, by: isize) {
        let _ = self.edit_if(|state| state.scroll_menu(menu, by));
    }

    pub fn prunes(&mut self, menu: u64, plan: Result<prunes::Plan, String>) -> Asked {
        self.edit(|state| state.prunes(menu, plan))
    }

    pub fn pruned(&mut self, menu: u64, pruned: Result<prunes::Pruned, String>) -> Asked {
        self.edit(|state| state.pruned(menu, pruned))
    }

    pub fn pulled(&mut self, menu: u64, found: Result<Found, String>) -> Asked {
        self.edit(|state| state.pulled(menu, found))
    }

    pub fn printed(&mut self, menu: u64, printed: Result<Printed, String>) -> Asked {
        self.edit(|state| state.printed(menu, printed))
    }

    pub fn checks(&mut self, menu: u64, checks: Result<checks::Failing, String>) -> Asked {
        self.edit(|state| state.checks(menu, checks))
    }

    pub fn rebased(&mut self, menu: u64, rebased: Result<Rebased, Unrebased>) -> Asked {
        self.edit(|state| state.rebased(menu, rebased))
    }

    pub fn plan(&self, on: &Machine, listed: &Listed, scope: Scope) -> Result<Plan, String> {
        self.state().plan(on, listed, scope)
    }

    pub fn unclosed(&mut self, on: &Machine, act: &Act, ticket: u64, unchanged: Unchanged) {
        self.edit(|state| state.unclosed(on, act, ticket, unchanged));
    }

    pub fn ticket(&mut self) -> u64 {
        self.edit_without_layout(SidebarState::ticket)
    }

    pub fn made_for(&mut self, menu: u64) -> Option<Held> {
        self.edit(|state| state.made_for(menu))
    }

    #[must_use]
    pub fn found(
        &mut self,
        menu: u64,
        agent: agents::Harness,
        skills: Result<Vec<Skill>, String>,
    ) -> bool {
        self.edit_if(|state| state.found(menu, agent, skills))
    }

    pub fn mouse(&mut self, mouse: MouseEvent, wall: SystemTime) -> Asked {
        self.edit(|state| state.mouse(mouse, wall))
    }

    pub fn pressed(&self) -> bool {
        self.state().pressed()
    }

    pub fn unsweep(&mut self) {
        self.edit_without_layout(SidebarState::unsweep);
    }

    pub fn let_go(&mut self) {
        let _ = self.edit_if(SidebarState::let_go);
    }

    #[cfg(test)]
    pub fn scrolling(&self) -> bool {
        self.state().scrolling()
    }

    #[must_use]
    pub fn scroll_due(&self, steady: Instant) -> Option<Instant> {
        self.state().scroll_due(steady)
    }

    pub fn draw(&self, buffer: &mut Buffer, framed: Rect, wall: SystemTime) -> (Drawn, Cadence) {
        self.state().draw(buffer, framed, wall)
    }
}

impl SidebarState {
    fn new(folds: Folds, news: News, agent: Kept, this: String, framed: Rect) -> Self {
        let mut state = SidebarState {
            host: None,
            this,
            away: None,
            seat: None,
            seat_name: None,
            folds,
            news,
            agent,
            shelves: Vec::new(),
            unheard: Vec::new(),
            tickets: 0,
            here_key: None,
            finished: HashMap::new(),
            view_titles: HashMap::new(),
            view_titles_revision: 0,
            rows: Vec::new(),
            counts: Counts::default(),
            refused: Vec::new(),
            unlisted_machines: 0,
            keys: Keys::default(),
            here_row: None,
            lit: None,
            mode: Mode::default(),
            viewport: Viewport::Collapsed {
                area: Rect::default(),
                top: 0,
            },
            revealed: None,
            notice: None,
            copied: None,
            image_going: None,
            aloud: None,
            working: BTreeMap::new(),
            filing: BTreeMap::new(),
            filed: None,
            menus: 0,
            due: None,
            anomalies: Troubles::default(),
        };
        state.place_rows(framed_rows(framed));
        state
    }

    fn place_rows(&mut self, area: Rect) {
        if matches!(&self.viewport, Viewport::Expanded { laid, .. } if laid.area == area) {
            return;
        }
        self.relayout_at(area);
    }

    fn collapse_rows(&mut self, area: Rect) {
        let top = self.viewport.top();
        self.viewport = Viewport::Collapsed { area, top };
    }

    #[cfg(test)]
    fn set_top(&mut self, top: usize) {
        match &mut self.viewport {
            Viewport::Collapsed { top: saved, .. } => *saved = top,
            Viewport::Expanded { laid, .. } => laid.top = top,
        }
    }

    fn reagent(&mut self) {
        if let Err(why) = self.agent.reload() {
            log::say(&format!("agent: {why}"));
        }
    }

    fn refold(&mut self, wall: SystemTime) -> bool {
        if self.folds.reload() {
            self.rebuild(wall);
            return true;
        }
        false
    }

    fn reread(&mut self, wall: SystemTime) -> bool {
        if self.news.reload() {
            self.rebuild(wall);
            return true;
        }
        false
    }

    fn heard(&mut self, host: Host, wall: SystemTime) {
        self.shelve(&host);
        for shelf in &self.shelves {
            let Some(projects) = shelf.kept() else {
                continue;
            };
            let machine = &shelf.machine;
            if projects.is_empty() && self.folds.contains(Fold::Machine(machine)) {
                let _ = self.folds.set(Fold::Machine(machine), false);
            }
            let mut live = Vec::new();
            for known in projects {
                live.push(Fold::Project(machine, known.project.id()));
                let worktrees = known.worktrees.iter();
                live.extend(worktrees.map(|it| Fold::Worktree(machine, it.place.worktree)));
            }
            self.folds.listed(machine, &live);
        }
        self.host = Some(host);
        self.away = None;
        self.reran();
        self.rebuild(wall);
    }

    fn shelve(&mut self, host: &Host) {
        self.shelves = shelves(host);
        let shelves = &mut self.shelves;
        self.unheard.retain(|unheard| {
            let on = shelves.iter().find(|shelf| shelf.machine == unheard.on);
            on.is_some_and(|shelf| {
                shelf.orders(&unheard.shown) && unheard.wait.holds(shelf.version.as_ref())
            })
        });
        for unheard in &self.unheard {
            let on = shelves.iter_mut().find(|shelf| shelf.machine == unheard.on);
            on.expect("an unheard order's machine is shelved")
                .reorder(&unheard.shown);
        }
    }

    fn reshelve(&mut self, wall: SystemTime) {
        if let Some(host) = self.host.take() {
            self.shelve(&host);
            self.host = Some(host);
            self.rebuild(wall);
        }
    }

    fn moved(&mut self, ticket: u64, kept: Option<api::Version>, wall: SystemTime) {
        let Some(at) = self
            .unheard
            .iter()
            .position(|unheard| unheard.ticket == ticket)
        else {
            return;
        };
        match self.unheard[at].wait.answered(kept) {
            Some(wait @ Wait::Answers { .. }) => self.unheard[at].wait = wait,
            Some(wait @ Wait::Watch(_)) => {
                self.unheard[at].wait = wait;
                self.reshelve(wall);
            }
            None => {
                self.unheard.remove(at);
                self.reshelve(wall);
            }
        }
    }

    fn unsent(&mut self, ticket: u64, order: &Reorder, why: &str, wall: SystemTime) {
        self.notify(format!("not moved: {why}"));
        let Some(at) = self
            .unheard
            .iter()
            .position(|unheard| unheard.ticket == ticket)
        else {
            return;
        };
        let unheard = &mut self.unheard[at];
        assert_eq!(
            unheard.shown,
            order.wanted(),
            "only the move asked last goes unsent"
        );
        unheard.shown = order.seen();
        match unheard.wait.unasked() {
            Some(wait) => unheard.wait = wait,
            None => {
                self.unheard.remove(at);
            }
        }
        self.reshelve(wall);
    }

    fn unmoved(
        &mut self,
        on: &Machine,
        ticket: u64,
        order: &Reorder,
        note: String,
        wall: SystemTime,
    ) {
        let of = order.wanted().of();
        let mut newer = self
            .unheard
            .iter()
            .filter(|it| it.on == *on && it.shown.of() == of);
        if !newer.any(|it| it.ticket > ticket) {
            self.notify(note);
        }
        let before = self.unheard.len();
        self.unheard.retain(|unheard| unheard.ticket != ticket);
        assert!(
            self.unheard.len() + 1 >= before,
            "no two unheard orders share a ticket"
        );
        if self.unheard.len() < before {
            self.reshelve(wall);
        }
    }

    fn ticked(&mut self, now: Moment, watched: bool) -> bool {
        let mut changed = self.hold(now.steady);
        changed |= self.advance_timers(now.steady);
        if self.due.is_some_and(|due| due <= now.system) {
            self.rebuild(now.system);
            changed = true;
        }
        let here = self.here_key.filter(|_| watched);
        let finished = &self.finished;
        let Dwell { rows, lost } = self.news.dwell(
            here,
            |key| finished.get(&key).copied(),
            now.system,
            now.steady,
        );
        if rows {
            self.rebuild(now.system);
            changed = true;
        }
        changed |= lost;
        let live = self.live();
        changed |= self.news.listed(live.as_ref(), now.steady);
        changed
    }

    fn advance_timers(&mut self, steady: Instant) -> bool {
        let mut changed = false;
        let over = |then: Instant| {
            steady
                .checked_duration_since(then)
                .expect("steady clock does not move backwards")
                >= COPIED_SHOWS
        };
        if self.copied.take_if(|(_, then)| over(*then)).is_some() {
            changed = true;
        }
        let has_keys = self.has_keys();
        match &mut self.mode {
            Mode::Menu {
                pause: pause @ Pause::Until(_),
                ..
            } => {
                if let Pause::Until(shows) = *pause
                    && steady >= shows
                {
                    *pause = Pause::Nothing;
                    changed = true;
                }
            }
            Mode::Pending(
                Pending::First(wait @ ChordWait::Until(_))
                | Pending::Group {
                    wait: wait @ ChordWait::Until(_),
                    ..
                },
            ) if has_keys => {
                if let ChordWait::Until(shows) = *wait
                    && steady >= shows
                {
                    *wait = ChordWait::Shown;
                    changed = true;
                }
            }
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::Menu {
                pause: Pause::Nothing | Pause::Quiet,
                ..
            }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => {}
        }
        changed
    }

    fn live(&self) -> Option<Lists> {
        let host = self.host.as_ref()?;
        if self.away.is_some() {
            return None;
        }
        let mut live = Lists::default();
        for (machine, seen) in machines(host, &self.shelves).each() {
            let Ok(seen) = seen else {
                live.unreached(&machine);
                continue;
            };
            let whole = self
                .shelves
                .iter()
                .any(|it| it.machine == machine && it.kept().is_some());
            let listed = seen
                .projects
                .iter()
                .map(|known| (known.listed().kept, known.terminals()));
            live.lists(&machine, listed, whole);
        }
        Some(live)
    }

    fn lost(&mut self, why: String) -> bool {
        if self.away.as_ref() == Some(&why) {
            return false;
        }
        log::say(&format!("luned: {why}"));
        self.away = Some(why);
        true
    }

    fn focus(&mut self, keys: Keys, wall: SystemTime) {
        if let Mode::Afloat { .. } | Mode::AfloatNote(_) = self.mode {
            self.end_mode();
        }
        let (had, holds) = (self.keys.held(), keys.held());
        self.keys = keys;
        if had == holds {
            return;
        }
        self.unpend();
        if holds {
            if let Some(here) = self.here_row.clone() {
                self.light(here);
            }
        } else {
            self.end_mode();
            if let Some(moving) = self.viewport.moving_mut() {
                *moving = Moving::Still;
            }
        }
        self.rebuild(wall);
    }

    fn holding(&self) -> Holding {
        match (&self.mode, self.pressed()) {
            (Mode::Afloat { .. } | Mode::AfloatNote(_), _) => Holding::Afloat,
            (_, true) => Holding::Press,
            (Mode::Idle | Mode::Pending(Pending::Legend | Pending::AllKeys(_)), false) => {
                Holding::Nothing
            }
            (Mode::Pending(Pending::First { .. } | Pending::Group { .. }), false) => Holding::Chord,
            (Mode::Search(_), false) => Holding::Search,
            (Mode::Reorder(_), false) => Holding::Reorder,
            (Mode::Menu { .. }, false) => Holding::Menu,
        }
    }

    fn floating(&self) -> Option<Floating<'_>> {
        match &self.mode {
            Mode::Afloat { menu, .. } => Some(Floating::Menu(menu)),
            Mode::AfloatNote(note) => Some(Floating::Note(note)),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::Menu { .. } => None,
        }
    }

    fn float_note(&mut self, note: String) {
        self.end_mode();
        let _ = self.let_go();
        self.mode = Mode::AfloatNote(note);
    }

    fn close_note(&mut self) -> bool {
        if let Mode::AfloatNote(_) = self.mode {
            self.mode = Mode::Idle;
            return true;
        }
        false
    }

    fn close_menu(&mut self) -> bool {
        let menu = self.take_open().map(|(menu, _)| menu);
        let had = menu.is_some();
        self.keep_working(menu);
        had
    }

    fn drop_menu(&mut self, menu: u64) -> bool {
        if self.open().is_some_and(|open| open.number() == menu) {
            self.take_open();
            return true;
        }
        false
    }

    fn drop_menu_saying(&mut self, menu: u64, notice: String) {
        let open = self.open().is_some_and(|open| open.number() == menu);
        let taken = if open {
            let (_, placed) = self.take_open().expect("the menu is open");
            Taken::Open(placed)
        } else {
            Taken::Kept
        };
        self.say_of(taken, notice);
    }

    fn open(&self) -> Option<&Menu> {
        match &self.mode {
            Mode::Menu { menu, .. } | Mode::Afloat { menu, .. } => Some(menu),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::AfloatNote(_) => None,
        }
    }

    fn open_mut(&mut self) -> Option<&mut Menu> {
        match &mut self.mode {
            Mode::Menu { menu, .. } | Mode::Afloat { menu, .. } => Some(menu),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::AfloatNote(_) => None,
        }
    }

    fn take_open(&mut self) -> Option<(Menu, Placed)> {
        match std::mem::take(&mut self.mode) {
            Mode::Menu { menu, pause } => Some((menu, Placed::Sidebar(pause))),
            Mode::Afloat { menu, said } => {
                if let Some(said) = said {
                    self.mode = Mode::AfloatNote(said);
                }
                Some((menu, Placed::Afloat))
            }
            other => {
                self.mode = other;
                None
            }
        }
    }

    fn put(&mut self, menu: Menu, placed: Placed) {
        self.mode = match (placed, std::mem::take(&mut self.mode)) {
            (Placed::Sidebar(pause), Mode::Idle) => Mode::Menu { menu, pause },
            (Placed::Afloat, Mode::Idle) => Mode::Afloat { menu, said: None },
            (Placed::Afloat, Mode::AfloatNote(said)) => Mode::Afloat {
                menu,
                said: Some(said),
            },
            (
                Placed::Sidebar(_) | Placed::Afloat,
                Mode::Pending(_)
                | Mode::Search(_)
                | Mode::Reorder(_)
                | Mode::Menu { .. }
                | Mode::Afloat { .. }
                | Mode::AfloatNote(_),
            ) => unreachable!("a menu opens only where the sidebar's keys are free"),
        };
    }

    fn end_mode(&mut self) {
        match std::mem::take(&mut self.mode) {
            Mode::Menu { menu, .. } => self.keep_working(Some(menu)),
            Mode::Afloat { menu, said } => {
                self.keep_working(Some(menu));
                if let Some(said) = said {
                    self.notice = Some(said);
                }
            }
            Mode::AfloatNote(note) => self.notice = Some(note),
            Mode::Idle | Mode::Pending(_) | Mode::Search(_) | Mode::Reorder(_) => {}
        }
    }

    fn unpend(&mut self) {
        if let Mode::Pending(_) = self.mode {
            self.mode = Mode::Idle;
        }
    }

    fn take_pending(&mut self) -> Option<Pending> {
        match std::mem::take(&mut self.mode) {
            Mode::Pending(pending) => Some(pending),
            other => {
                self.mode = other;
                None
            }
        }
    }

    fn pend(&mut self, pending: Pending) {
        self.enter(Mode::Pending(pending));
    }

    fn enter(&mut self, mode: Mode) {
        self.end_mode();
        self.mode = mode;
    }

    #[cfg(test)]
    fn working_on(&self) -> bool {
        self.working.values().any(|menu| menu.doing().is_some())
    }

    fn went_said(place: &Place) -> String {
        let session = place.session();
        match place {
            Place::View(ViewPlace { view, .. }) => {
                format!("menu: {session}:{view} has it; nobody went")
            }
            Place::Session(_) | Place::Worktree(_) => {
                format!("menu: {session} has it; nobody went")
            }
        }
    }

    fn keep_working(&mut self, menu: Option<Menu>) {
        if let Some(menu) = menu.filter(Menu::waits) {
            self.working.insert(menu.number(), menu);
        }
    }

    fn take_waited(&mut self, menu: u64, keep: impl Fn(&Menu) -> bool) -> Option<(Menu, Taken)> {
        let (waited, taken, ()) = self.take_seen(menu, |it| keep(it).then_some(()))?;
        Some((waited, taken))
    }

    fn take_seen<T>(
        &mut self,
        menu: u64,
        see: impl Fn(&Menu) -> Option<T>,
    ) -> Option<(Menu, Taken, T)> {
        let open = self.open().filter(|it| it.number() == menu);
        if let Some(seen) = open.and_then(&see) {
            return self
                .take_open()
                .map(|(it, placed)| (it, Taken::Open(placed), seen));
        }
        let seen = self.working.get(&menu).and_then(see)?;
        self.working.remove(&menu).map(|it| (it, Taken::Kept, seen))
    }

    fn sit(&mut self, seat: Seat, now: SystemTime) -> bool {
        let was = self
            .seat
            .take()
            .filter(|was| was.on == seat.on && was.project == seat.project);
        let unmoved = was.as_ref() == Some(&seat);
        let (project, worktree, terminal) = (seat.project, seat.worktree(), seat.terminal());
        self.seat = Some(seat);
        if unmoved {
            return false;
        }
        if was.is_none() {
            self.unfold_project(project);
        }
        if let Some(worktree) = worktree
            && was.as_ref().and_then(Seat::worktree) != Some(worktree)
        {
            let on = self.on().clone();
            let _ = self.folds.set(Fold::Worktree(&on, worktree), false);
        }
        let moved = was.as_ref().and_then(Seat::terminal) != terminal;
        if moved && let Some(terminal) = terminal {
            self.unfold_home(project, terminal);
        }
        self.rebuild(now);
        true
    }

    fn unseat(&mut self, now: SystemTime) -> bool {
        let had = self.seat.take().is_some();
        if had {
            self.rebuild(now);
        }
        assert!(
            self.seat.is_none(),
            "lune sits nowhere when it shows no worktree"
        );
        had
    }

    fn unfold_home(&mut self, project: ProjectId, terminal: TerminalKey) {
        if let Some(place) = self.view_place(project, terminal) {
            let on = self.on().clone();
            let _ = self
                .folds
                .set(Fold::Worktree(&on, place.worktree.worktree), false);
        }
    }

    fn seated_in(&self) -> Option<ProjectId> {
        self.seat.as_ref().map(|seat| seat.project)
    }

    fn seated_on(&self, on: &Machine) -> Option<ProjectId> {
        self.seat
            .as_ref()
            .filter(|seat| seat.on == *on)
            .map(|seat| seat.project)
    }

    fn own_title(&self) -> Option<&str> {
        self.seat_name.as_deref()
    }

    #[must_use]
    fn view_titles(&self, on: &Machine, worktree: WorktreeId) -> Option<&BTreeMap<ViewId, String>> {
        self.view_titles.get(on)?.get(&worktree)
    }

    #[must_use]
    fn view_titles_revision(&self) -> u64 {
        self.view_titles_revision
    }

    fn listed_of(&self, project: ProjectId) -> Option<Listed> {
        Some(self.known_of(project)?.listed().clone())
    }

    fn known_of(&self, project: ProjectId) -> Option<&Known> {
        let mut known = shelf(&self.shelves, self.on()).iter();
        known.find(|known| known.project.id() == project)
    }

    fn view_place(&self, project: ProjectId, terminal: TerminalKey) -> Option<ViewPlace> {
        let mut placed = self.known_of(project)?.placed_views();
        let (worktree, view) = placed.find(|(_, view)| view.key == terminal)?;
        Some(ViewPlace::of(worktree, view))
    }

    fn here_worktree(&self) -> Option<WorktreePlace> {
        let seat = self.seat.as_ref()?;
        let known = self.known_of(seat.project)?;
        let mut worktrees = known.worktrees.iter();
        let seated = seat.worktree()?;
        let worktree = worktrees.find(|worktree| worktree.place.worktree == seated)?;
        Some(worktree.place.clone())
    }

    fn unfold_project(&mut self, project: ProjectId) {
        if self.known_of(project).is_some() {
            let on = self.on().clone();
            let _ = self.folds.set(Fold::Machine(&on), false);
            let _ = self.folds.set(Fold::Project(&on, project), false);
        }
    }

    fn seats_here(seated: Option<ProjectId>, listed: &Listed) -> bool {
        seated == Some(listed.project())
    }

    fn on(&self) -> &Machine {
        self.seat.as_ref().map_or(&Machine::Local, |seat| &seat.on)
    }

    fn here_place(&self) -> Result<ViewPlace, &'static str> {
        let seat = self.seat.as_ref().ok_or(NO_HERE_VIEW)?;
        let terminal = seat.terminal().ok_or(NO_HERE_VIEW)?;
        self.view_place(seat.project, terminal).ok_or(NO_HERE_VIEW)
    }

    fn here_agent(&self) -> Option<HereAgent> {
        let place = self.here_place().ok()?;
        let seen = self.seen(self.on())?;
        let seated = seen.seated();
        let agent = seated.get(&place.view.key())?;
        let view = self.view_of(self.on(), place.listed(), place.view())?;
        let harness = agents::of(agent).filter(|_| agent.at_a_prompt())?;
        let dir = Directory::from(&view.cwd);
        Some(HereAgent {
            on: self.on().clone(),
            session: place.listed().name().session().clone(),
            view: place.view(),
            dir,
            harness,
        })
    }

    fn on_header(&self) -> Option<Machine> {
        match self.lit_spot()? {
            Spot::Machine(machine) => Some(machine.clone()),
            Spot::Local(_) | Spot::Remote(..) => None,
        }
    }

    fn listing(&self) -> Listing {
        let here = self.whereabouts();
        let (away, this) = (self.away.clone(), self.this.clone());
        let mut listing = Listing {
            away,
            this,
            ..Listing::default()
        };
        if let Some(host) = &self.host {
            for (machine, seen) in machines(host, &self.shelves).each() {
                listing.shelves.push(self.shelved(&machine, seen.as_ref()));
                let Ok(seen) = seen else {
                    continue;
                };
                let runs = self.runs(&machine, &seen, here.as_ref());
                match machine {
                    Machine::Local => listing.local = runs,
                    Machine::Remote(destination) => listing.remotes.push((destination, runs)),
                }
            }
        }
        listing.shown = self.here_worktree().map(|place| (self.on().clone(), place));
        listing.here = here;
        listing
    }

    fn shelved(&self, machine: &Machine, seen: Result<&Seen<'_>, &Away>) -> Shelved {
        let heard = seen.ok();
        let statuses = git_statuses(heard.map_or(&[], |seen| seen.git_statuses));
        let mut projects = Vec::new();
        for known in shelf(&self.shelves, machine) {
            let mut worktrees = Vec::with_capacity(known.worktrees.len());
            for worktree in &known.worktrees {
                let branch = statuses
                    .get(worktree.folder.as_str())
                    .and_then(|status| Some(String::from(checkout(status, None)?.branch)));
                let terminals = worktree.views().map(|view| ViewKey::Terminal(view.key));
                worktrees.push(Worktree {
                    place: worktree.place.clone(),
                    named: Named::of(worktree.name.clone(), branch),
                    folder: worktree.folder.clone(),
                    used: worktree.used,
                    terminals: terminals.collect(),
                });
            }
            let (project, dir) = (known.project.clone(), known.dir.clone());
            let unopened = known.unopened.clone();
            projects.push(Housed {
                project,
                dir,
                worktrees,
                unopened,
            });
        }
        let away = seen.err().map(|away| away.said().0);
        Shelved {
            on: machine.clone(),
            away,
            projects,
        }
    }

    fn reaches(&self) -> Option<Vec<(Machine, Reach)>> {
        let host = self.host.as_ref()?;
        let here = self.whereabouts();
        let reach = |machine: &Machine, seen: Result<Seen<'_>, Away>| {
            let Ok(seen) = seen else {
                return Reach::Away;
            };
            let agents = self.runs(machine, &seen, here.as_ref()).agents;
            let listed = seen
                .projects
                .iter()
                .map(|known| known.listed().clone())
                .collect();
            Reach::Heard {
                agents,
                listed,
                unlisted: Vec::new(),
            }
        };
        let each = machines(host, &self.shelves).each();
        Some(
            each.map(|(machine, seen)| (machine.clone(), reach(&machine, seen)))
                .collect(),
        )
    }

    fn whereabouts(&self) -> Option<Here> {
        let project = self.seated_in()?;
        let on = self.on().clone();
        if let Ok(place) = self.here_place() {
            return Some(Here::View { on, place });
        }
        let session = self.listed_of(project)?.name.to_string();
        Some(Here::Session { on, session })
    }

    fn far_agent(&self) -> Option<&Destination> {
        let Machine::Remote(on) = self.on() else {
            return None;
        };
        let Some(Here::View { place, .. }) = self.whereabouts() else {
            return None;
        };
        let seen = self.seen(self.on())?;
        seen.seated().contains_key(&place.view.key()).then_some(on)
    }

    fn runs(&self, machine: &Machine, seen: &Seen<'_>, here: Option<&Here>) -> Runs {
        let here = here.filter(|here| here.on() == machine);
        let placed = here.and_then(Here::view);
        let here_key = placed.and_then(|place| news_key(&self.shelves, machine, place));
        let seated = seen.seated();
        let mut runs = Runs::default();
        for known in seen.projects.iter().rev() {
            let listed = known.listed();
            for (worktree, view) in known.placed_views() {
                let Some(agent) = seated.get(&view.key) else {
                    continue;
                };
                let place = ViewPlace::of(worktree, view);
                let key = listed.key(view.id);
                let (marked, _) = agent_marked(&self.news, agent, key, here_key == Some(key));
                runs.agents.push(Seated {
                    place,
                    label: label(Some(agent), view),
                    mark: marked.mark,
                    status: status_word(agent),
                    since: marked.since,
                    dir: short_cwd(view.cwd.as_str()),
                    pid: agent.pid,
                    transcript: transcript(agent).named(),
                    doing: alerts::Doing::of(agent),
                });
            }
        }
        let here = here.map(Here::session);
        runs.agents
            .sort_by_key(|seated| here != Some(seated.place.listed().name().as_str()));
        runs
    }

    fn sayings(&self) -> Option<Vec<(Machine, Option<Vec<Saying>>)>> {
        let each = machines(self.host.as_ref()?, &self.shelves).each();
        Some(
            each.map(|(on, seen)| (on, seen.ok().map(|seen| seen.sayings.to_vec())))
                .collect(),
        )
    }

    fn aloud(&mut self, shown: Option<aloud::Shown>) -> bool {
        if self.aloud == shown {
            return false;
        }
        self.aloud = shown;
        true
    }

    fn aloud_buttons(&self) -> &Spots<aloud::Button> {
        match &self.viewport {
            Viewport::Collapsed { .. } => &EMPTY_ALOUD_BUTTONS,
            Viewport::Expanded { laid, .. } => &laid.aloud_buttons,
        }
    }

    fn notify(&mut self, notice: String) {
        log::say(&notice);
        self.notice = Some(notice);
    }

    fn clear_notice(&mut self) -> bool {
        self.notice.take().is_some()
    }

    fn unsay(&mut self, notice: &str) -> bool {
        if self.notice.as_deref() == Some(notice) {
            self.notice = None;
            return true;
        }
        false
    }

    fn filing(&mut self, number: u64, repository: &str) {
        self.filing.insert(number, repository.to_owned());
    }

    fn filed(&mut self, number: u64, repository: &str, filed: &Result<Filed, Unfiled>) {
        self.filing.remove(&number);
        match filed {
            Ok(filed) => {
                log::say(&format!("issue: filed {}", filed.link().url()));
                let (number, title) = (filed.number(), filed.title());
                self.filed = Some(format!(
                    "filed #{number} in {repository}: {title}; link copied"
                ));
            }
            Err(Unfiled::Refused(why)) => {
                self.notify(format!(
                    "issue in {repository}: {why}; i has your words again"
                ));
            }
            Err(Unfiled::InDoubt(why)) => {
                let doubt = format!("issue in {repository} may be filed: {why}");
                self.notify(format!("{doubt}; i has your words, look first"));
            }
        }
    }

    fn image_going(&mut self, to: Option<Destination>) -> bool {
        let changed = self.image_going != to;
        self.image_going = to;
        changed
    }

    fn say(&mut self, notice: String) {
        log::say(&notice);
        match &mut self.mode {
            Mode::Afloat { said, .. } => *said = Some(notice),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::Menu { .. }
            | Mode::AfloatNote(_) => self.notice = Some(notice),
        }
    }

    fn say_of(&mut self, taken: Taken, notice: String) {
        let Taken::Open(Placed::Afloat) = taken else {
            return self.say(notice);
        };
        log::say(&notice);
        match &mut self.mode {
            Mode::Afloat { said, .. } => *said = Some(notice),
            Mode::AfloatNote(note) => *note = notice,
            Mode::Idle => self.mode = Mode::AfloatNote(notice),
            Mode::Pending(_) | Mode::Search(_) | Mode::Reorder(_) | Mode::Menu { .. } => {
                unreachable!("an afloat menu's words find the sidebar's keys free")
            }
        }
    }

    #[cfg(test)]
    fn turning(&self) -> bool {
        let menus = self.open().is_some_and(Menu::turning);
        self.row_turning() || menus || self.working_on() || !self.filing.is_empty()
    }

    #[cfg(test)]
    fn row_turning(&self) -> bool {
        let Some(laid) = self.viewport.laid() else {
            return false;
        };
        if laid.room == 0 {
            return false;
        }
        let shown = laid.top..laid.top + laid.room;
        let mut line = 0;
        for row in &self.rows {
            let first = line + row.gap();
            if row.spinning() && shown.contains(&first) {
                return true;
            }
            line += row.height();
        }
        false
    }

    fn view(&self) -> SidebarView {
        assert!(
            self.rows.len() <= ROWS_MAX,
            "sidebar rows stay within their upper bound"
        );
        let Viewport::Expanded { laid, moving } = &self.viewport else {
            return SidebarView::Collapsed {
                away: self.away.is_some(),
                tinted: self.image_going.is_some() || self.copied.is_some(),
                tallies: (self.counts.new, self.counts.busy, self.counts.shell),
                paused: self.aloud.as_ref().is_some_and(aloud::Shown::paused),
                title: self.seat_name.clone(),
            };
        };
        let width = usize::from(laid.area.width.saturating_sub(1));
        let height = usize::from(laid.area.height);
        let NotesAt { mut notes, .. } = self.notes_at(width, height, 0);
        assert_eq!(
            laid.room,
            height.saturating_sub(HEADER + notes.lines.len()),
            "the sidebar's laid-out room matches its state before diffing"
        );
        notes.lines.truncate(height);
        let all = matches!(moving, Moving::Lifted(_) | Moving::Swept(..))
            || matches!(self.mode, Mode::Reorder(_));
        let top = laid.top;
        let shown = top..top + laid.room;
        let mut line = 0;
        let mut rows = Vec::new();
        for row in &self.rows {
            let end = line + row.height();
            if all || (line < shown.end && end > shown.start) {
                rows.push((line, row.clone()));
            }
            line = end;
        }
        SidebarView::Expanded(Box::new(ExpandedView {
            framed: self.has_keys(),
            has_host: self.host.is_some(),
            empty: self.rows.is_empty(),
            counts: self.counts,
            top,
            rows,
            lit: if self.keys.held() {
                self.lit_index()
            } else {
                None
            },
            here: self.here_row.clone(),
            holding: self.holding(),
            title: self.seat_name.clone(),
            notes: notes.lines,
        }))
    }

    fn rebuild(&mut self, wall: SystemTime) {
        if self.rebuild_with(Typos::None, wall) == Rebuilt::Unfound {
            let rebuilt = self.rebuild_with(Typos::Forgiven, wall);
            assert!(
                rebuilt == Rebuilt::Done,
                "a search forgiving typos rebuilds even unfound"
            );
        }
    }

    fn rebuild_with(&mut self, typos: Typos, now: SystemTime) -> Rebuilt {
        let seat_listed = self.seated_in().and_then(|project| self.listed_of(project));
        if let Some(listed) = seat_listed {
            self.seat_name = Some(listed.name.to_string());
        }
        let here_view = self.here_place().ok();
        let here_worktree = self.here_worktree();
        let Some(host) = &self.host else {
            return Rebuilt::Done;
        };
        let (seated, sits_on) = (self.seated_in(), self.on().clone());
        let here_key = here_view
            .as_ref()
            .and_then(|place| news_key(&self.shelves, &sits_on, place));
        let mut due: Option<SystemTime> = None;
        let listed = machines(host, &self.shelves);
        let unlisted_machines = listed.left_out;
        let refused = self.shelves.iter().filter_map(Shelf::refused).collect();
        let term = match &self.mode {
            Mode::Search(Search::Typed { term, .. }) => Some(term.as_str()),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(Search::Empty)
            | Mode::Reorder(_)
            | Mode::Menu { .. }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => None,
        };
        let finder = term.map(|term| Finder::new(term, typos));
        let mut finished: HashMap<Key, SystemTime> = HashMap::new();
        let mut view_titles: HashMap<Machine, HashMap<WorktreeId, BTreeMap<ViewId, String>>> =
            HashMap::new();
        let mut counts = Counts::default();
        let mut rows = Vec::new();
        let mut best: Option<(Spot, Fit)> = None;
        let mut weigh = |at: Spot, fit: Fit| {
            if best.as_ref().is_none_or(|(_, top)| fit > *top) {
                best = Some((at, fit));
            }
        };
        for (machine, reached) in listed.each() {
            let on = machine == sits_on;
            let name = match &machine {
                Machine::Local => self.this.clone(),
                Machine::Remote(destination) => destination.to_string(),
            };
            let projects = shelf(&self.shelves, &machine);
            let (heard, away) = match reached {
                Ok(heard) => (Some(heard), None),
                Err(away) => (None, Some(away)),
            };
            let parsed_statuses = heard
                .as_ref()
                .map(|heard| ParsedGitStatuses::of_api(heard.git_statuses));
            let parsed_pulls = heard
                .as_ref()
                .map(|heard| ParsedPullRequests::of_api(heard.pull_requests));
            if let Some(why) = parsed_pulls
                .as_ref()
                .and_then(ParsedPullRequests::anomalies)
            {
                self.anomalies.say(anomaly_note(&machine, &why));
            }
            if let Some(why) = parsed_statuses
                .as_ref()
                .and_then(ParsedGitStatuses::anomalies)
            {
                self.anomalies.say(anomaly_note(&machine, &why));
            }
            if projects.is_empty() {
                if finder.is_none() {
                    rows.push(Row::Machine {
                        machine,
                        name,
                        away,
                        heads: Heads::Nothing,
                    });
                }
                continue;
            }
            let machine_folded = finder.is_none() && self.folds.contains(Fold::Machine(&machine));
            let mut machine_rows = Vec::new();
            let mut machine_titles: HashMap<WorktreeId, BTreeMap<ViewId, String>> = HashMap::new();
            let (mut machine_tally, mut machine_here) = (Tally::default(), false);
            let agents = heard.as_ref().map_or_default(Seen::seated);
            let statuses = parsed_statuses.map_or_default(|parsed| parsed.by_dir);
            let pull_requests = parsed_pulls.map_or_default(|parsed| parsed.by_dir);
            counts.projects += projects.len();
            for known in projects {
                let listed = known.listed();
                let project = known.project.id();
                let folded = self.folds.contains(Fold::Project(&machine, project));
                let colour = colour_of(project);
                let agent = |view: &KnownView| agents.get(&view.key).copied();
                let mut worktrees = Vec::with_capacity(known.worktrees.len());
                for worktree in &known.worktrees {
                    let mut by_view: Vec<Shows> = Vec::with_capacity(worktree.terminals.len());
                    for view in worktree.views() {
                        counts.views += 1;
                        let key = listed.key(view.id);
                        let here = on && here_key == Some(key);
                        let unread = self.news.is_unread(key);
                        if let Some(agent) = agent(view) {
                            let (marked, done) = agent_marked(&self.news, agent, key, here);
                            if let Some(done) = done {
                                finished.insert(key, done);
                            }
                            let label = label(Some(agent), view);
                            by_view.push(Shows {
                                view,
                                marked: Some(marked),
                                label,
                                unread,
                            });
                            continue;
                        }
                        let Told::Doing(doing) = &view.told else {
                            let label = label(None, view);
                            by_view.push(Shows {
                                view,
                                marked: None,
                                label,
                                unread,
                            });
                            continue;
                        };
                        if let Some(long) = long_at(doing).filter(|long| *long > now) {
                            due = Some(due.map_or(long, |due| due.min(long)));
                        }
                        let mut shell = Marked::of_shell(doing, now);
                        let ended = shell.as_ref().and_then(|(shell, _)| shell.ended());
                        if let Some(ended) = ended {
                            finished.insert(key, ended);
                            if here || !self.news.is_new(key, Some(ended)) {
                                shell = None;
                            }
                        }
                        let (marked, ran) = shell.unzip();
                        let ran = ran.flatten().filter(|command| !command.trim().is_empty());
                        let label = ran.unwrap_or_else(|| label(None, view));
                        assert!(!label.trim().is_empty(), "a view label is non-empty");
                        by_view.push(Shows {
                            view,
                            marked,
                            label,
                            unread,
                        });
                    }
                    let mut titles = BTreeMap::new();
                    for shows in &by_view {
                        let replaced = titles.insert(shows.view.id, shows.label.clone());
                        assert!(
                            replaced.is_none(),
                            "each view in a worktree has a distinct id"
                        );
                    }
                    let replaced = machine_titles.insert(worktree.place.worktree, titles);
                    assert!(
                        replaced.is_none(),
                        "each worktree on a machine has a distinct id"
                    );
                    let tally = Tally::of(&by_view);
                    counts.waiting += tally.waiting;
                    counts.new += tally.dots;
                    counts.busy += tally.busy;
                    counts.shell += tally.shell;
                    worktrees.push((worktree, by_view, tally));
                }
                let tally = Tally::sum(worktrees.iter().map(|(_, _, tally)| *tally));
                let project_here = on && Self::seats_here(seated, listed);
                machine_tally = Tally::sum([machine_tally, tally].into_iter());
                machine_here |= project_here;
                if machine_folded {
                    continue;
                }
                let (named, name_hits) = finder
                    .as_ref()
                    .and_then(|finder| finder.session(listed.name.as_str()))
                    .unzip();
                let mut project_rows = Vec::new();
                let mut lead: Option<(Spot, Fit)> = None;
                let mut found_at = |at: Spot, fit: Fit, weigh: &mut dyn FnMut(Spot, Fit)| {
                    weigh(at.clone(), fit);
                    if lead.as_ref().is_none_or(|(_, top)| fit > *top) {
                        lead = Some((at, fit));
                    }
                };
                if finder.is_some() || !folded {
                    for (worktree, by_view, tally) in worktrees {
                        let fold = Fold::Worktree(&machine, worktree.place.worktree);
                        let worktree_folded = finder.is_none() && self.folds.contains(fold);
                        let dir = worktree.folder.as_str();
                        let checkout = statuses
                            .get(dir)
                            .and_then(|status| checkout(status, pull_requests.get(dir)));
                        let branch = checkout.as_ref().map(|checkout| checkout.branch.as_str());
                        let (worktree_found, worktree_hits) = finder
                            .as_ref()
                            .and_then(|finder| finder.worktree(&worktree.name, branch, dir))
                            .unzip();
                        let mut shown = Vec::with_capacity(worktree.terminals.len());
                        let listing = by_view.into_iter().filter(|_| !worktree_folded);
                        for Shows {
                            view,
                            marked,
                            label,
                            unread,
                        } in listing
                        {
                            let command = view.command.as_deref();
                            let program = agent(view)
                                .and_then(|agent| icons::agent(agent.runs_in()))
                                .or_else(|| command.and_then(icons::program));
                            let dir = view.cwd.as_str();
                            let (found, hits) = finder
                                .as_ref()
                                .and_then(|finder| finder.view(&label, None, &worktree.name, dir))
                                .unzip();
                            if finder.is_some()
                                && found.is_none()
                                && worktree_found.is_none()
                                && named.is_none()
                            {
                                continue;
                            }
                            let place = ViewPlace::of(&worktree.place, view);
                            let here = on && here_view.as_ref() == Some(&place);
                            let icon = program.and_then(icons::of);
                            let found = found
                                .map(|fit| (Spot::on(&machine, Place::View(place.clone())), fit));
                            let (machine, view_id) = (machine.clone(), view.id);
                            let row = Row::View {
                                machine,
                                place,
                                view_id,
                                here,
                                unread,
                                marked,
                                icon,
                                label,
                                hits: hits.unwrap_or_default(),
                            };
                            shown.push((row, found));
                        }
                        if finder.is_some()
                            && named.is_none()
                            && worktree_found.is_none()
                            && shown.is_empty()
                        {
                            continue;
                        }
                        let under = if !shown.is_empty() {
                            Under::Rows
                        } else if worktree_folded {
                            Under::Folded
                        } else {
                            Under::Empty
                        };
                        let here = on && here_worktree.as_ref() == Some(&worktree.place);
                        project_rows.push(Row::Worktree {
                            machine: machine.clone(),
                            place: worktree.place.clone(),
                            named: Named::of(worktree.name.clone(), checkout),
                            missing: worktree.seen == projects::Seen::Missing,
                            hits: worktree_hits.unwrap_or_default(),
                            under,
                            here,
                            colour,
                            waiting: tally.waiting,
                            dot: tally.dots > 0,
                            busy: tally.busy > 0,
                        });
                        if let Some(found) = worktree_found {
                            let at = Spot::on(&machine, Place::Worktree(worktree.place.clone()));
                            found_at(at, found, &mut weigh);
                        }
                        for (row, found) in shown {
                            if let Some((at, found)) = found {
                                found_at(at, found, &mut weigh);
                            }
                            project_rows.push(row);
                        }
                    }
                }
                if finder.is_some() && named.is_none() && project_rows.is_empty() {
                    continue;
                }
                if let Some(named) = named {
                    let project_spot = || Spot::on(&machine, Place::Session(listed.clone()));
                    weigh(lead.map_or_else(project_spot, |(at, _)| at), named);
                }
                let under = if project_rows.is_empty() {
                    if folded { Under::Folded } else { Under::Empty }
                } else {
                    Under::Rows
                };
                machine_rows.push(Row::Project {
                    machine: machine.clone(),
                    project: known.project.clone(),
                    hits: name_hits.unwrap_or_default(),
                    under,
                    here: project_here,
                    colour,
                    waiting: tally.waiting,
                    dot: tally.dots > 0,
                    busy: tally.busy > 0,
                });
                machine_rows.append(&mut project_rows);
            }
            let replaced = view_titles.insert(machine.clone(), machine_titles);
            assert!(replaced.is_none(), "each machine appears once in listed");
            if finder.is_some() && machine_rows.is_empty() {
                continue;
            }
            let heads = Heads::Projects {
                folded: machine_folded,
                here: machine_here,
                waiting: machine_tally.waiting,
                dot: machine_tally.dots > 0,
                busy: machine_tally.busy > 0,
            };
            rows.push(Row::Machine {
                machine,
                name,
                away,
                heads,
            });
            rows.append(&mut machine_rows);
        }
        self.anomalies.done();
        if finder.is_none() {
            assert!(!rows.is_empty(), "this machine always has a row");
        } else if best.is_none() && typos == Typos::None {
            return Rebuilt::Unfound;
        }
        let seek = match &mut self.mode {
            Mode::Search(Search::Typed { seek, .. }) => std::mem::take(seek),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(Search::Empty)
            | Mode::Reorder(_)
            | Mode::Menu { .. }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => false,
        };
        let truncated = rows.len() > ROWS_MAX;
        if truncated {
            rows.truncate(ROWS_MAX);
        }
        assert!(
            rows.len() <= ROWS_MAX,
            "sidebar rows stay within their upper bound"
        );
        if let Some(kin) = self.kin_of_gone(&rows) {
            self.lit = Some(Lit::On(kin));
        }
        self.rows = rows;
        if self.view_titles != view_titles {
            self.view_titles = view_titles;
            self.view_titles_revision = self.view_titles_revision.wrapping_add(1);
        }
        self.counts = counts;
        self.refused = refused;
        self.unlisted_machines = unlisted_machines;
        self.here_key = here_key;
        self.finished = finished;
        let here_spot = here_view.map(|place| Spot::on(self.on(), Place::View(place)));
        self.here_row = here_spot.and_then(|spot| self.row_for(&spot));
        let gone = |spot: Spot| self.row_of(&spot).is_none();
        let reordered_gone = matches!(&self.mode, Mode::Reorder(movable) if gone(movable.spot()));
        let lifted_gone = matches!(
            self.viewport.moving(),
            Some(Moving::Lifted(Lift { lifted: movable, .. })) if gone(movable.spot())
        );
        let down_gone = matches!(
            self.viewport.moving(),
            Some(Moving::Down { on, .. }) if on.spot().is_some_and(gone)
        );
        if reordered_gone {
            self.mode = Mode::Idle;
        }
        if let Some(moving) = self.viewport.moving_mut() {
            if lifted_gone {
                *moving = Moving::Still;
            } else if down_gone && let Moving::Down { on, .. } = moving {
                *on = Pressed::Space;
            }
        }
        if let Some(Lit::Closing { spot, landing, .. }) = &self.lit
            && self.row_of(spot).is_none()
        {
            self.lit = landing.clone().map(Lit::On);
        }
        self.due = due;
        let best = best.and_then(|(at, _)| {
            let found = self.row_of(&at);
            if !truncated {
                assert!(found.is_some(), "a search's best match has a row");
            }
            found
        });
        self.settle(best, seek);
        Rebuilt::Done
    }

    fn kin_of_gone(&self, rows: &[Row]) -> Option<Spot> {
        let Some(Lit::On(spot)) = &self.lit else {
            return None;
        };
        let Some(Place::View(gone)) = spot.place() else {
            return None;
        };
        let listed = |spot: &Spot| rows.iter().any(|row| row.spot() == *spot);
        if listed(spot) {
            return None;
        }
        let at = self.row_of(spot)?;
        let machine = spot.machine();
        let kin = |row: &&Row| {
            matches!(row, Row::View { machine: of, place, .. }
                if *of == machine && place.worktree == gone.worktree)
        };
        let (above, below) = (self.rows[..at].iter().rev(), self.rows[at + 1..].iter());
        below.chain(above).filter(kin).map(Row::spot).find(listed)
    }

    fn row_for(&self, spot: &Spot) -> Option<Spot> {
        let rows = [
            Some(spot.clone()),
            spot.worktree(),
            project_of(spot),
            Some(Spot::Machine(spot.machine())),
        ];
        let listed = |spot: &Spot| self.rows.iter().any(|row| row.spot() == *spot);
        rows.into_iter().flatten().find(listed)
    }

    fn folded(&self, machine: &Machine, folding: Folding) -> bool {
        match folding {
            Folding::Machine => self.folds.contains(Fold::Machine(machine)),
            Folding::Project(project) => self.folds.contains(Fold::Project(machine, project)),
            Folding::Worktree(worktree) => self.folds.contains(Fold::Worktree(machine, worktree)),
        }
    }

    fn row_of(&self, spot: &Spot) -> Option<usize> {
        self.rows.iter().position(|row| row.spot() == *spot)
    }

    fn machine_row(&self, machine: &Machine) -> (&str, Option<&Away>, Heads) {
        let found = self.rows.iter().find_map(|row| match row {
            Row::Machine {
                machine: of,
                name,
                away,
                heads,
            } => (of == machine).then_some((name.as_str(), away.as_ref(), *heads)),
            Row::Project { .. } | Row::Worktree { .. } | Row::View { .. } => None,
        });
        found.expect("a machine's rows sit under its own")
    }

    fn home_row(&self, at: usize) -> Option<usize> {
        let row = &self.rows[at];
        if !matches!(row, Row::View { .. }) {
            return None;
        }
        let home = self.row_of(&row.spot().worktree()?)?;
        assert!(home < at, "a worktree's row sits above its views");
        Some(home)
    }

    fn settle(&mut self, best: Option<usize>, seek: bool) {
        let held = self
            .lit_spot()
            .and_then(|spot| self.row_of(&self.row_for(spot)?));
        let at = if seek { best.or(held) } else { held.or(best) };
        let at = at.unwrap_or(0);
        let stop = (at..self.rows.len())
            .chain((0..at).rev())
            .find(|index| self.stops(*index));
        match stop {
            Some(index) => self.light(self.rows[index].spot()),
            None => self.lit = None,
        }
    }

    fn stops(&self, at: usize) -> bool {
        let row = &self.rows[at];
        match &self.mode {
            Mode::Reorder(Movable::Project(of, _)) => {
                matches!(row, Row::Project { machine, .. } if machine == of)
            }
            Mode::Reorder(Movable::Worktree(of, held)) => matches!(
                row,
                Row::Worktree { machine, place, .. }
                    if machine == of && place.project == held.project
            ),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Menu { .. }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => match row {
                Row::Machine {
                    heads: Heads::Projects { folded, .. },
                    ..
                } => *folded,
                Row::Worktree {
                    under: Under::Rows, ..
                } => false,
                Row::Machine {
                    heads: Heads::Nothing,
                    ..
                }
                | Row::Project { .. }
                | Row::Worktree {
                    under: Under::Folded | Under::Empty,
                    ..
                }
                | Row::View { .. } => true,
            },
        }
    }

    fn lit_spot(&self) -> Option<&Spot> {
        self.lit.as_ref().map(Lit::spot)
    }

    fn light(&mut self, spot: Spot) {
        if self.lit_spot() != Some(&spot) {
            self.lit = Some(Lit::On(spot));
        }
    }

    fn lit_index(&self) -> Option<usize> {
        let spot = self.lit_spot()?;
        Some(self.row_of(spot).expect("the highlight is on a row"))
    }

    fn step(&mut self, by: isize) {
        let stops: Vec<usize> = (0..self.rows.len()).filter(|at| self.stops(*at)).collect();
        let Some(last) = stops.len().checked_sub(1) else {
            return;
        };
        let now = self
            .lit_index()
            .and_then(|at| stops.iter().position(|stop| *stop == at));
        let next = match now {
            Some(now) => now.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.light(self.rows[stops[next]].spot());
    }

    fn end(&mut self, last: bool) {
        let mut stops = (0..self.rows.len()).filter(|at| self.stops(*at));
        let stop = if last {
            stops.next_back()
        } else {
            stops.next()
        };
        if let Some(stop) = stop {
            self.light(self.rows[stop].spot());
        }
    }

    fn toggle(&mut self, now: SystemTime) {
        let Some(spot) = self.lit_spot().cloned() else {
            return;
        };
        let folding = spot.folding();
        let folded = self.folded(&spot.machine(), folding);
        self.set_fold(&spot, folding, !folded, now);
    }

    fn fold(&mut self, folded: bool, now: SystemTime) {
        let Some(spot) = self.lit_spot().cloned() else {
            return;
        };
        let folding = spot.folding();
        let lifted = match self.row_of(&spot).map(|at| &self.rows[at]) {
            Some(Row::Worktree { under, .. }) if folded && *under != Under::Rows => spot
                .place()
                .map(|place| Folding::Project(place.listed().project())),
            Some(Row::Project { under, .. }) if folded && *under != Under::Rows => {
                Some(Folding::Machine)
            }
            _ => None,
        };
        self.set_fold(&spot, lifted.unwrap_or(folding), folded, now);
    }

    fn set_fold(&mut self, spot: &Spot, folding: Folding, folded: bool, now: SystemTime) {
        let machine = spot.machine();
        if folding == Folding::Machine {
            let (_, _, heads) = self.machine_row(&machine);
            if heads == Heads::Nothing {
                return;
            }
        }
        let header = Spot::Machine(machine.clone());
        let (fold, row) = match folding {
            Folding::Machine => (Fold::Machine(&machine), Some(header)),
            Folding::Project(project) => (Fold::Project(&machine, project), project_of(spot)),
            Folding::Worktree(worktree) => (Fold::Worktree(&machine, worktree), spot.worktree()),
        };
        match self.folds.set(fold, folded) {
            FoldSet::Unchanged | FoldSet::SharingLost => return,
            FoldSet::Changed => {}
        }
        if folded {
            match row {
                Some(row) => self.light(row),
                None => self.lit = None,
            }
        }
        self.rebuild(now);
    }

    fn key_from(&mut self, key: &KeyEvent, now: Moment, came: Came) -> Asked {
        if self.pressed() {
            if key.code == KeyCode::Esc {
                let _ = self.let_go();
            }
            return Asked::Nothing;
        }
        self.notice = None;
        self.copied = None;
        self.filed = None;
        let paused = match &mut self.mode {
            Mode::Menu { menu, pause } => std::mem::take(pause).pauses(menu, now.steady),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => false,
        };
        let pending = self.take_pending();
        let wait = match (came, &pending) {
            (Came::Read, _) | (_, Some(Pending::Legend)) => ChordWait::Shown,
            (Came::Typed, _) => ChordWait::Until(now.steady + PAUSE),
        };
        if menu::closes(key) && self.open().is_some_and(Menu::waits) {
            let _ = self.close_menu();
            return Asked::Nothing;
        }
        if key.code == KeyCode::Char(menu::ALL_KEYS)
            && let Mode::Menu { menu, .. } = &self.mode
            && menu.lists()
        {
            let _ = self.close_menu();
            self.pend(Pending::AllKeys(AllKeys::default()));
            return Asked::Nothing;
        }
        if let Some((menu, placed)) = self.take_open() {
            let asked = self.menu_did(placed, menu.key(key));
            if paused
                && let Mode::Menu { menu, pause } = &mut self.mode
                && menu.quiet()
            {
                *pause = Pause::Quiet;
            }
            return asked;
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return Asked::Nothing;
        }
        if let Some(term) = self.search() {
            let term = term.to_owned();
            return self.search_key(key, term, now.system);
        }
        let pending = match pending {
            Some(Pending::Legend) => match key.code {
                KeyCode::Esc => return Asked::Nothing,
                KeyCode::Char(menu::ALL_KEYS) => {
                    self.pend(Pending::AllKeys(AllKeys::default()));
                    return Asked::Nothing;
                }
                _ => None,
            },
            Some(Pending::AllKeys(all)) => match self.all_keys_key(all, key) {
                Some(asked) => return asked,
                None => None,
            },
            pending => pending,
        };
        let chord = matches!(pending, Some(Pending::First(_) | Pending::Group { .. }));
        if let Mode::Reorder(_) = self.mode
            && let Some(asked) = self.selected_key(key, now.system)
        {
            return asked;
        }
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Home => self.end(false),
            KeyCode::End => self.end(true),
            KeyCode::Left => self.fold(true, now.system),
            KeyCode::Right => self.fold(false, now.system),
            KeyCode::Enter => return self.go(now.system),
            KeyCode::Esc if chord => {}
            KeyCode::Esc => return Asked::Leave,
            KeyCode::Char(c)
                if let Some(Pending::Group { key, .. }) = pending
                    && let Some(action) = menu::in_group(key, c) =>
            {
                return self.row_action(action);
            }
            KeyCode::Char(c)
                if let Some(Pending::First(_)) = pending
                    && menu::in_menu_group(GIT, c) =>
            {
                return self.open_menu_at(GIT, c);
            }
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Last) => self.end(true),
                Some(Top::First) if matches!(pending, Some(Pending::First(_))) => {
                    self.end(false);
                }
                Some(Top::First) => self.pend(Pending::First(wait)),
                Some(Top::Group(key)) => self.pend(Pending::Group { key, wait }),
                Some(Top::AllKeys) => self.pend(Pending::Legend),
                Some(Top::Fold(Some(folded))) => self.fold(folded, now.system),
                Some(Top::Fold(None)) => self.toggle(now.system),
                Some(Top::Menu) => return self.open_menu(None),
                Some(Top::Command(Command::Search)) => self.find(String::new(), now.system),
                Some(Top::Command(Command::Unread)) => self.toggle_unread(now.system),
                Some(Top::Command(Command::Reorder)) => self.select(),
                Some(Top::Command(Command::Quit)) if chord => {}
                Some(Top::Command(Command::Quit)) => return Asked::Quit,
                Some(Top::Shift(_)) => {}
                None => {
                    let asked = self.open_menu(Some(c));
                    if let (ChordWait::Until(shows), Mode::Menu { pause, .. }) =
                        (wait, &mut self.mode)
                    {
                        *pause = Pause::Until(shows);
                    }
                    return asked;
                }
            },
            _ => {}
        }
        Asked::Nothing
    }

    fn due(&self, now: Moment) -> Option<Instant> {
        let shows = match &self.mode {
            Mode::Pending(
                Pending::First(ChordWait::Until(shows))
                | Pending::Group {
                    wait: ChordWait::Until(shows),
                    ..
                },
            ) if self.has_keys() => Some(*shows),
            Mode::Menu {
                pause: Pause::Until(shows),
                ..
            } => Some(*shows),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Search(_)
            | Mode::Reorder(_)
            | Mode::Menu { .. }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => None,
        };
        if shows.is_some() {
            assert!(
                matches!(self.bottom(), Bottom::Nothing),
                "a paused hint or menu stays hidden while due"
            );
        }
        let copied = self.copied.as_ref().map(|(_, then)| *then + COPIED_SHOWS);
        let shell = self.due.and_then(|at| {
            now.steady
                .checked_add(at.duration_since(now.system).unwrap_or(Duration::ZERO))
        });
        [shows, copied, shell].into_iter().flatten().min()
    }

    fn all_keys_key(&mut self, mut all: AllKeys, key: &KeyEvent) -> Option<Asked> {
        let by = match key.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::Esc | KeyCode::Char(menu::ALL_KEYS | QUIT) => {
                return Some(Asked::Nothing);
            }
            _ => return None,
        };
        all.scroll(by, self.bottom_room());
        self.pend(Pending::AllKeys(all));
        Some(Asked::Nothing)
    }

    fn highlighted_link(&self) -> Result<Link, String> {
        let row = self.lit_spot().and_then(|spot| self.row_of(spot));
        let checked_out = |row: &Row| match row {
            Row::Worktree { named, .. } => named.checkout().cloned(),
            Row::View { .. } => self.row_of(&row.spot().worktree()?).and_then(|at| {
                let Row::Worktree { named, .. } = &self.rows[at] else {
                    return None;
                };
                named.checkout().cloned()
            }),
            Row::Machine { .. } | Row::Project { .. } => None,
        };
        let checkout = row.and_then(|at| checked_out(&self.rows[at]));
        let Some(Checkout {
            pull_request: Some(numbered),
            ..
        }) = checkout
        else {
            return Err("no pull request on this row".to_owned());
        };
        numbered.link().cloned().ok_or_else(|| {
            format!(
                "{}: luned says no link of it on github.com",
                numbered.shown()
            )
        })
    }

    fn yank(&mut self) -> Asked {
        match self.highlighted_link() {
            Ok(link) => Asked::Copy(link),
            Err(why) => {
                self.notify(why);
                Asked::Nothing
            }
        }
    }

    fn row_action(&mut self, action: Action) -> Asked {
        match action {
            Action::YankPull => self.yank(),
            Action::OpenPull => self.browse(),
            Action::OpenWorktree => self.open_worktree(),
            other => unreachable!("{other:?} is in a row group and has no handler"),
        }
    }

    fn open_worktree(&mut self) -> Asked {
        let target = self.target();
        let Some((on, project)) = target.and_then(|it| Some((it.machine().clone(), it.project()?)))
        else {
            self.notify("menu: a worktree is opened from a row of its project".to_owned());
            return Asked::Nothing;
        };
        Asked::OpenWorktree { on, project }
    }

    fn browse(&mut self) -> Asked {
        match self.highlighted_link() {
            Ok(link) => Asked::Browse(link),
            Err(why) => {
                self.notify(why);
                Asked::Nothing
            }
        }
    }

    fn number_clicked(&mut self, link: Link) -> Asked {
        self.notice = None;
        self.unpend();
        Asked::Copy(link)
    }

    fn copied(&mut self, copied: Copied, steady: Instant) {
        self.copied = Some((copied, steady));
    }

    fn selected_key(&mut self, key: &KeyEvent, wall: SystemTime) -> Option<Asked> {
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Esc => self.unselect(),
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Shift(by)) => return Some(self.shift(by, wall)),
                Some(Top::Command(Command::Reorder)) => self.widen(),
                Some(Top::Command(Command::Quit)) => self.unselect(),
                _ => {
                    self.unselect();
                    return None;
                }
            },
            _ => {
                self.unselect();
                return None;
            }
        }
        Some(Asked::Nothing)
    }

    fn select(&mut self) {
        let Some(spot) = self.lit_spot().cloned() else {
            return;
        };
        let Some(movable) = Movable::of(spot.worktree().unwrap_or(spot)) else {
            return;
        };
        self.light(movable.spot());
        self.enter(Mode::Reorder(movable));
    }

    fn widen(&mut self) {
        let Mode::Reorder(Movable::Worktree(machine, place)) = &self.mode else {
            self.unselect();
            return;
        };
        let project = Movable::Project(machine.clone(), place.project.clone());
        self.light(project.spot());
        self.mode = Mode::Reorder(project);
    }

    fn unselect(&mut self) {
        if let Mode::Reorder(_) = self.mode {
            self.mode = Mode::Idle;
            self.settle(None, false);
        }
    }

    fn shift(&mut self, by: isize, now: SystemTime) -> Asked {
        let Some((spans, held)) = self.selected() else {
            return Asked::Nothing;
        };
        let to = held
            .start
            .saturating_add_signed(by)
            .min(spans.len() - held.len());
        if to == held.start {
            return Asked::Nothing;
        }
        self.keep(&spans.moved(held, to), now)
    }

    fn selected(&self) -> Option<(Spans, Range<usize>)> {
        let Mode::Reorder(anchor) = &self.mode else {
            return None;
        };
        let spans = self.spans(anchor);
        let rows = spans.rows();
        let index = |at: usize| rows.iter().position(|span| span.contains(&at));
        let (from, to) = (
            index(self.row_of(&anchor.spot())?)?,
            index(self.lit_index()?)?,
        );
        Some((spans, from.min(to)..from.max(to) + 1))
    }

    fn spans(&self, movable: &Movable) -> Spans {
        match movable {
            Movable::Project(machine, _) => {
                let starts = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter_map(|(at, row)| match row {
                        Row::Project {
                            machine: of,
                            project,
                            ..
                        } if of == machine => Some((at, project.id())),
                        _ => None,
                    });
                let starts: Vec<(usize, ProjectId)> = starts.collect();
                let end = |start: usize| {
                    let after = self.rows.iter().enumerate().skip(start + 1);
                    let mut after = after.filter(|(_, row)| match row {
                        Row::Machine { .. } | Row::Project { .. } => true,
                        Row::Worktree { .. } | Row::View { .. } => false,
                    });
                    after.next().map_or(self.rows.len(), |(at, _)| at)
                };
                let spans = starts
                    .into_iter()
                    .map(|(start, id)| (id, start..end(start)));
                Spans::Projects(machine.clone(), spans.collect())
            }
            Movable::Worktree(machine, held) => {
                let project = Spot::on(machine, Place::Session(held.project.listed().clone()));
                let first = self.row_of(&project).map_or(self.rows.len(), |at| at + 1);
                let rows = self.rows.iter().enumerate().skip(first);
                let rows = rows.take_while(|(_, row)| match row {
                    Row::Worktree { .. } | Row::View { .. } => true,
                    Row::Machine { .. } | Row::Project { .. } => false,
                });
                let rows: Vec<(usize, &Row)> = rows.collect();
                let end = rows.last().map_or(first, |(at, _)| at + 1);
                let starts: Vec<(usize, WorktreeId)> = rows
                    .iter()
                    .filter_map(|(at, row)| match row {
                        Row::Worktree { place, .. } => Some((*at, place.worktree)),
                        _ => None,
                    })
                    .collect();
                let ends = starts.iter().skip(1).map(|(at, _)| *at).chain([end]);
                let spans = starts
                    .iter()
                    .zip(ends)
                    .map(|((start, id), end)| (*id, *start..end));
                Spans::Worktrees(machine.clone(), held.project.clone(), spans.collect())
            }
        }
    }

    fn keep(&mut self, spans: &Spans, now: SystemTime) -> Asked {
        let (on, order) = match spans {
            Spans::Projects(machine, spans) => {
                let kept = self.shelves.iter().find(|it| it.machine == *machine);
                let seen = kept.map_or_else(Vec::new, Shelf::order);
                let listed: Vec<ProjectId> = spans.iter().map(|(id, _)| *id).collect();
                let wanted = wanted(&seen, &listed);
                if wanted == seen {
                    return Asked::Nothing;
                }
                (machine.clone(), Reorder::Projects { seen, wanted })
            }
            Spans::Worktrees(machine, project, spans) => {
                let mut known = shelf(&self.shelves, machine).iter();
                let Some(known) = known.find(|known| known.project.id() == project.id()) else {
                    let name = project.listed().name();
                    self.notify(format!("{name} is no longer listed: nothing moved"));
                    return Asked::Nothing;
                };
                let listed: Vec<WorktreeId> = spans.iter().map(|(id, _)| *id).collect();
                let seen = known.order();
                let wanted = wanted(&seen, &listed);
                if wanted == seen {
                    return Asked::Nothing;
                }
                let project = project.id();
                (
                    machine.clone(),
                    Reorder::Worktrees {
                        project,
                        seen,
                        wanted,
                    },
                )
            }
        };
        let (seen, wanted) = (order.seen(), order.wanted());
        assert_ne!(seen, wanted, "a move changes the order");
        let on_its_way = self.unheard.iter().filter(|unheard| unheard.on == on);
        if on_its_way
            .map(|unheard| unheard.wait.asked())
            .sum::<usize>()
            >= MOVES_MAX
        {
            self.notify(format!(
                "not moved: luned has not answered the last {MOVES_MAX} moves"
            ));
            return Asked::Nothing;
        }
        let at = self
            .unheard
            .iter()
            .position(|it| it.on == on && it.shown.of() == wanted.of());
        let kept = self.shelves.iter_mut().find(|shelf| shelf.machine == on);
        kept.expect("the machine moved on is shelved")
            .reorder(&wanted);
        let ticket = if let Some(at) = at {
            let unheard = &mut self.unheard[at];
            assert_eq!(unheard.shown, seen, "the rows show the last order asked");
            unheard.shown = wanted;
            unheard.wait = unheard.wait.again();
            unheard.ticket
        } else {
            let wait = Wait::Answers {
                asked: NonZeroUsize::MIN,
                landed: None,
            };
            let ticket = self.ticket();
            self.unheard.push(Unheard {
                ticket,
                on: on.clone(),
                shown: wanted,
                wait,
            });
            ticket
        };
        self.rebuild(now);
        Asked::Move { on, order, ticket }
    }

    fn landed(&self, spans: &[Range<usize>]) -> Vec<usize> {
        let start = spans.iter().map(|span| span.start).min().unwrap_or(0);
        let end = spans.iter().map(|span| span.end).max().unwrap_or(0);
        let moved = spans.iter().flat_map(Clone::clone);
        (0..start)
            .chain(moved)
            .chain(end..self.rows.len())
            .collect()
    }

    fn landing(
        &self,
        movable: &Movable,
        line: u16,
        over: bool,
        top: usize,
    ) -> Option<(Spans, Range<usize>)> {
        let at = self.row_of(&movable.spot())?;
        let spans = self.spans(movable);
        let rows = spans.rows();
        let from = rows.iter().position(|span| span.contains(&at))?;
        let lifted = rows[from].clone();
        if !over {
            return Some((spans, lifted));
        }
        let lines = |span: Range<usize>| self.rows[span].iter().map(Row::height).sum::<usize>();
        let above = HEADER + lines(lifted.start..at) + self.rows[at].gap();
        let top = (top + usize::from(line)) as isize - above as isize;
        let mut line = lines(0..rows[0].start);
        let mut gaps = Vec::with_capacity(rows.len());
        for (index, span) in rows.iter().enumerate() {
            if index != from {
                gaps.push(line);
                line += lines(span.clone());
            }
        }
        gaps.push(line);
        let distance = |gap: &usize| ((gaps[*gap] as isize - top).unsigned_abs(), *gap != from);
        let to = (0..gaps.len()).min_by_key(distance).unwrap_or(from);
        Some((spans.moved(from..from + 1, to), lifted))
    }

    fn lifting(&self, at: usize) -> Option<Movable> {
        Movable::of(self.rows[at].spot())
    }

    fn open_menu(&mut self, key: Option<char>) -> Asked {
        let Some(target) = self.target() else {
            return Asked::Nothing;
        };
        self.menus += 1;
        let agent = self.agent_for(&target);
        let Some(did) = Menu::open(self.menus, target, key, agent) else {
            return Asked::Nothing;
        };
        self.menu_did(Placed::Sidebar(Pause::Nothing), did)
    }

    fn open_menu_at(&mut self, group: char, key: char) -> Asked {
        let Some(target) = self.target() else {
            return Asked::Nothing;
        };
        self.menus += 1;
        let agent = self.agent_for(&target);
        let opened = Menu::open(self.menus, target, Some(group), agent);
        let Some(Did::Stays(menu, Wants::Nothing)) = opened else {
            return Asked::Nothing;
        };
        let did = menu.key(&KeyEvent::from(KeyCode::Char(key)));
        self.menu_did(Placed::Sidebar(Pause::Nothing), did)
    }

    fn keep_agent(&mut self, harness: agents::Harness) {
        if let Err(why) = self.agent.keep(harness) {
            let name = agents::name(harness);
            self.say(format!("agent: {name} is for this lune alone: {why}"));
        }
    }

    fn agent_for(&self, target: &Target) -> agents::Picked {
        match self.harness_in(target) {
            Some(harness) => agents::Picked::ByView(harness),
            None => agents::Picked::ByYou(self.agent.harness()),
        }
    }

    fn harness_in(&self, target: &Target) -> Option<agents::Harness> {
        let Target::View(machine, row) = target else {
            return None;
        };
        let seen = self.seen(machine)?;
        let shown = self.view_of(machine, row.place.listed(), row.place.view())?;
        seen.seated()
            .get(&shown.key)
            .and_then(|agent| agents::of(agent))
    }

    fn target(&self) -> Option<Target> {
        let spot = self.lit_spot().cloned()?;
        self.row_of(&spot)?;
        match spot {
            Spot::Machine(machine) => Some(Target::Machine(machine)),
            Spot::Local(place) => self.target_at(&Machine::Local, place),
            Spot::Remote(destination, place) => {
                self.target_at(&Machine::Remote(destination), place)
            }
        }
    }

    fn target_at(&self, machine: &Machine, place: Place) -> Option<Target> {
        let place = match place {
            Place::Session(session) => return self.project_target(machine, &session),
            Place::Worktree(place) => return self.worktree_target(machine, place),
            Place::View(place) => place,
        };
        let (session, view) = (place.listed(), place.view);
        let seen = self.seen(machine);
        let shown = self.view_of(machine, session, view);
        let cwd = Cwd::of(shown.map(|shown| Directory::from(&shown.cwd)), |dir| {
            let (branch, repository) = git_in(seen.as_ref(), &dir);
            let pull_request = seen
                .as_ref()
                .and_then(|seen| pull_requests(seen.pull_requests).remove(dir.as_str()));
            Folder {
                dir,
                branch,
                repository,
                pull_request,
            }
        });
        let ran = self.ran_in(machine, session, view);
        let terminal = view_id_of(&self.shelves, machine, session, view);
        let arranged = terminal.and_then(|it| {
            arranged_holding(&self.shelves, self.seat.as_ref(), machine, session, it)
        });
        let row = ViewRow {
            place: place.clone(),
            terminal,
            cwd,
            ran,
            arranged,
        };
        Some(Target::View(machine.clone(), row))
    }

    fn project_target(&self, machine: &Machine, listed: &Listed) -> Option<Target> {
        let mut known = shelf(&self.shelves, machine).iter();
        let known = known.find(|it| it.listed() == listed)?;
        let (project, dir) = (known.project.clone(), known.dir.as_str().to_owned());
        Some(Target::Project(
            machine.clone(),
            ProjectTarget { project, dir },
        ))
    }

    fn worktree_target(&self, machine: &Machine, place: WorktreePlace) -> Option<Target> {
        let mut known = shelf(&self.shelves, machine).iter();
        let known = known.find(|it| it.project.id() == place.project.id())?;
        let worktree = known
            .worktrees
            .iter()
            .find(|worktree| worktree.place == place)?;
        let (name, folder) = (worktree.name.clone(), Directory::from(&worktree.folder));
        let (branch, repository) = git_in(self.seen(machine).as_ref(), &folder);
        let row = WorktreeRow {
            place,
            name,
            folder,
            branch,
            repository,
        };
        Some(Target::Worktree(machine.clone(), row))
    }

    fn ran_in(&self, machine: &Machine, session: &Listed, view: ViewKey) -> Ran {
        self.view_of(machine, session, view)
            .map_or(Ran::Unhooked, |shown| ran(&shown.told))
    }

    fn reran(&mut self) {
        let Some(Target::View(machine, row)) = self.open().map(Menu::target) else {
            return;
        };
        let ran = self.ran_in(machine, row.place.listed(), row.place.view());
        if let Some(menu) = self.open_mut() {
            menu.reran(ran);
        }
    }

    fn float_menu(&mut self) {
        self.notice = None;
        let place = match self.here_place() {
            Ok(place) => Place::View(place),
            Err(why) => {
                log::say(why);
                return self.float_note(why.to_owned());
            }
        };
        self.menus += 1;
        let Some(target) = self.target_at(self.on(), place) else {
            return self.float_note(NO_HERE_VIEW.to_owned());
        };
        let agent = self.agent_for(&target);
        self.end_mode();
        let _ = self.let_go();
        self.put(Menu::new(self.menus, target, agent), Placed::Afloat);
    }

    fn aimed_row(&self) -> Aim {
        match self.target() {
            Some(row) => Aim::At {
                target: Rc::new(row),
                worktree: None,
            },
            None => Aim::Unplaced(PALETTE_NO_ROW),
        }
    }

    fn aimed_view(&self) -> Aim {
        let view = self
            .here_place()
            .ok()
            .and_then(|place| self.here_target(Place::View(place)));
        let worktree = self.here_worktree().map(Place::Worktree);
        let worktree = worktree.and_then(|place| self.here_target(place));
        match view {
            Some(target) => Aim::At { target, worktree },
            None => Aim::Unplaced(PALETTE_UNPLACED),
        }
    }

    fn aimed_worktree(&self) -> Aim {
        match self
            .here_worktree()
            .and_then(|place| self.here_target(Place::Worktree(place)))
        {
            Some(target) => Aim::At {
                target,
                worktree: None,
            },
            None => Aim::Unplaced(PALETTE_UNPLACED),
        }
    }

    fn here_target(&self, place: Place) -> Option<Rc<Target>> {
        self.target_at(self.on(), place).map(Rc::new)
    }

    fn act_on_row(&mut self, target: &Target, action: Action, wall: SystemTime) -> Asked {
        self.notice = None;
        self.end_mode();
        assert!(
            self.has_keys(),
            "the sidebar has the keys its own keys act with"
        );
        let command = Command::of(action);
        if command == Some(Command::Search) {
            self.find(String::new(), wall);
            Asked::Nothing
        } else if self.highlights(target) {
            match command {
                Some(Command::Unread) => {
                    self.toggle_unread(wall);
                    Asked::Nothing
                }
                Some(Command::Reorder) => {
                    self.select();
                    Asked::Nothing
                }
                Some(Command::Search | Command::Quit) => {
                    unreachable!("Search ran above, and Quit is a menu key too")
                }
                None => self.row_action(action),
            }
        } else {
            self.notify(PALETTE_ROW_GONE.to_owned());
            Asked::Nothing
        }
    }

    fn act_in_menu(&mut self, target: &Target, action: Action, origin: Origin) -> Asked {
        self.notice = None;
        self.end_mode();
        if Command::of(action) == Some(Command::Quit) {
            Asked::Quit
        } else {
            self.menus += 1;
            let agent = self.agent_for(target);
            let did = Menu::acting(self.menus, target, action, agent);
            if origin == Origin::Views {
                let _ = self.let_go();
            }
            self.menu_did(Placed::of(origin), did)
        }
    }

    fn highlights(&self, target: &Target) -> bool {
        let at = |target: &Target| (target.machine().clone(), target.place());
        self.target()
            .is_some_and(|highlighted| at(&highlighted) == at(target))
    }

    fn chained(&mut self, done: Done, origin: Origin) -> Asked {
        self.notice = None;
        self.menu_did(Placed::of(origin), Did::Closed(done))
    }

    fn paste(&mut self, text: &str, wall: SystemTime) -> Asked {
        if let Mode::AfloatNote(_) = self.mode {
            return Asked::Nothing;
        }
        if self.open_mut().is_some_and(|menu| menu.paste(text)) {
            return Asked::Nothing;
        }
        let target = match self.open() {
            Some(menu) => Some(menu.target().clone()),
            None => self.target(),
        };
        let from = target.as_ref().and_then(Target::cwd).cloned();
        let Some(work) = Work::parse(text, from) else {
            return Asked::Nothing;
        };
        if self.notice_on_its_way(&work) {
            return Asked::Nothing;
        }
        let checked_out = match &work {
            Work::Link(link) => {
                self.checked_out_worktree(link, target.as_ref().map(Target::machine))
            }
            Work::File(_) => None,
        };
        let menu_afloat = matches!(self.mode, Mode::Afloat { .. });
        if let Some((on, place)) = checked_out {
            if menu_afloat {
                let _ = self.close_menu();
            } else {
                self.end_mode();
            }
            assert!(
                self.open().is_none(),
                "a paste that goes leaves no menu open"
            );
            return self.go_to(on, Place::Worktree(place), wall);
        }
        let Some(target) = target else {
            if self.lit_spot().is_none() {
                self.say(format!(
                    "menu: no row is highlighted for {work} to open the menu on"
                ));
            }
            return Asked::Nothing;
        };
        let placed = if menu_afloat {
            let _ = self.close_menu();
            Placed::Afloat
        } else {
            self.end_mode();
            Placed::Sidebar(Pause::Nothing)
        };
        self.menus += 1;
        self.put(
            Menu::pull(self.menus, target, work, self.agent.harness()),
            placed,
        );
        Asked::Nothing
    }

    fn checked_out_worktree(
        &self,
        link: &Link,
        preferred: Option<&Machine>,
    ) -> Option<(Machine, WorktreePlace)> {
        let host = self.host.as_ref()?;
        let reached = machines(host, &self.shelves).each();
        let reached = reached.filter_map(|(machine, seen)| Some((machine, seen.ok()?)));
        let reached: Vec<_> = reached.collect();
        let first = reached
            .iter()
            .filter(|(machine, _)| Some(machine) == preferred);
        let rest = reached
            .iter()
            .filter(|(machine, _)| Some(machine) != preferred);
        for (machine, seen) in first.chain(rest) {
            let pulls = pull_requests(seen.pull_requests);
            let checks_out = |worktree: &&KnownWorktree| {
                let checked_out = pull_request_link(&pulls, worktree.folder.as_str());
                checked_out.is_some_and(|it| it.is(link))
            };
            let worktrees = seen.projects.iter().flat_map(|known| &known.worktrees);
            let mut found = worktrees.filter(checks_out);
            let Some(worktree) = found.next() else {
                continue;
            };
            if found.next().is_some() {
                return None;
            }
            return Some((machine.clone(), worktree.place.clone()));
        }
        None
    }

    fn pull_in_menu(&mut self, target: Target, work: Work, pull: Pull, origin: Origin) -> Asked {
        if self.notice_on_its_way(&work) {
            return Asked::Nothing;
        }
        self.end_mode();
        self.menus += 1;
        let did = Menu::start_pull(self.menus, target, work, self.agent.harness(), pull);
        if origin == Origin::Views {
            let _ = self.let_go();
        }
        self.menu_did(Placed::of(origin), did)
    }

    fn notice_on_its_way(&mut self, work: &Work) -> bool {
        let making = self
            .open()
            .into_iter()
            .chain(self.working.values())
            .filter_map(Menu::making);
        let on_its_way = making.into_iter().any(|making| making == work);
        self.notice = on_its_way.then(|| format!("{} is on its way", work.tab()));
        on_its_way
    }

    fn chose(&mut self, menu: u64, choice: Choice, now: Moment) -> Asked {
        if self.open().is_none_or(|open| open.number() != menu) {
            return Asked::Nothing;
        }
        match choice {
            Choice::Key(key) => self.key_from(&key, now, Came::Typed),
            Choice::Skill(name) => self.pick(&name),
        }
    }

    fn keys_clicked(&mut self, keyed: Keyed, key: &KeyEvent, now: Moment) -> Asked {
        let there = match self.bottom() {
            Bottom::Legend | Bottom::Group(_) | Bottom::Keys(..) | Bottom::AllKeys(_) => {
                Some(Keyed::Legend)
            }
            Bottom::Search(_) => Some(Keyed::Search),
            Bottom::Menu(_) | Bottom::Nothing => None,
        };
        if !self.has_keys() || there != Some(keyed) {
            return Asked::Nothing;
        }
        self.key_from(key, now, Came::Read)
    }

    fn pick(&mut self, name: &Name) -> Asked {
        self.notice = None;
        self.unpend();
        let Some((menu, placed)) = self.take_open() else {
            return Asked::Nothing;
        };
        self.menu_did(placed, menu.pick(name))
    }

    fn scroll_menu(&mut self, menu: u64, by: isize) -> bool {
        if let Some(open) = self.open_mut().filter(|open| open.number() == menu) {
            open.scroll(by);
            return true;
        }
        false
    }

    fn has_keys(&self) -> bool {
        let afloat = matches!(self.mode, Mode::Afloat { .. } | Mode::AfloatNote(_));
        self.keys == Keys::Sidebar && !afloat
    }

    fn seen(&self, machine: &Machine) -> Option<Seen<'_>> {
        let machines = machines(self.host.as_ref()?, &self.shelves);
        match machine {
            Machine::Local => Some(machines.here),
            Machine::Remote(destination) => machines
                .others
                .into_iter()
                .find(|(other, _)| other == destination)?
                .1
                .ok(),
        }
    }

    fn view_of(&self, machine: &Machine, listed: &Listed, view: ViewKey) -> Option<&KnownView> {
        let mut known = shelf(&self.shelves, machine).iter();
        known
            .find(|known| known.listed() == listed)?
            .view(view.key())
    }

    fn menu_did(&mut self, placed: Placed, did: Did) -> Asked {
        self.menu_did_after(Taken::Open(placed), did)
    }

    fn menu_did_after(&mut self, taken: Taken, did: Did) -> Asked {
        let (menu, wants) = match did {
            Did::Stays(menu, wants) => (menu, wants),
            Did::Closed(done) => {
                return match done {
                    Done::Nothing => Asked::Nothing,
                    Done::Wrong(reason) => {
                        self.say_of(taken, reason);
                        Asked::Nothing
                    }
                    Done::Worktree(on, project, branch) => Asked::NewWorktree {
                        on,
                        project,
                        branch,
                    },
                    Done::Welcome { on, session, view } => Asked::Welcome {
                        asking: taken.held(),
                        on,
                        session,
                        view,
                    },
                    Done::Browser { on, place, url } => Asked::Browser {
                        asking: taken.held(),
                        on,
                        place,
                        url,
                    },
                    Done::Review { on, session, view } => Asked::Review {
                        asking: taken.held(),
                        on,
                        session,
                        view,
                    },
                    Done::Arrange {
                        on,
                        session,
                        view,
                        arranged,
                    } => Asked::Arrange {
                        asking: taken.held(),
                        on,
                        session,
                        view,
                        arranged,
                    },
                    Done::OpenIn {
                        on,
                        worktree,
                        command,
                    } => Asked::OpenIn {
                        asking: taken.held(),
                        on,
                        worktree,
                        command,
                    },
                    Done::Said(said) => {
                        self.say_of(taken, said);
                        Asked::Nothing
                    }
                    Done::Rename(on, rename) => Asked::Rename { on, rename },
                    Done::Go(on, place) if taken.held() == Held::Open => Asked::Go { on, place },
                    Done::Go(_, place) => {
                        let said = Self::went_said(&place);
                        self.say_of(taken, said);
                        Asked::Nothing
                    }
                    Done::Act(on, act) => self.close(on, act),
                    Done::Brief(briefed) => self.brief(taken, briefed),
                    Done::Ask(target) => Asked::Ask(target),
                    Done::Issue(repository) => Asked::Issue(repository),
                    Done::Ssh(destination) => Asked::Ssh(destination),
                    Done::Forget(destination) => Asked::Forget(destination),
                    Done::Quit => Asked::Quit,
                };
            }
        };
        let (number, on) = (menu.number(), menu.target().machine().clone());
        if let Wants::Open {
            session,
            view,
            opens,
        } = wants
        {
            let asking = taken.held();
            return Asked::Open {
                asking,
                on,
                project: session.project(),
                beside: view,
                opens,
            };
        }
        match taken {
            Taken::Open(placed) => self.put(menu, placed),
            Taken::Kept => {
                self.working.insert(number, menu);
            }
        }
        let asked = self.menu_wants(number, on, wants);
        if taken == Taken::Kept
            && asked.menu() != Some(number)
            && let Some(said) = self.working.remove(&number).as_ref().and_then(Menu::left)
        {
            self.say(said);
        }
        asked
    }

    fn menu_wants(&mut self, number: u64, on: Machine, wants: Wants) -> Asked {
        match wants {
            Wants::Nothing => Asked::Nothing,
            Wants::Cloned(project) => {
                let mut known = shelf(&self.shelves, &on).iter();
                let known = known.find(|it| it.project.id() == project);
                let refused = known.and_then(|it| it.unopened.refuses_new_worktree(&it.project));
                if let Some(refused) = refused {
                    self.drop_menu_saying(number, format!("menu: {refused}"));
                }
                Asked::Nothing
            }
            Wants::Keep(harness) => {
                self.keep_agent(harness);
                Asked::Nothing
            }
            Wants::Skills { dir, agent } => Asked::Find {
                menu: number,
                on,
                dir,
                agent,
            },
            Wants::Refind { dir, agent } => {
                self.keep_agent(agent);
                Asked::Find {
                    menu: number,
                    on,
                    dir,
                    agent,
                }
            }
            Wants::Open { .. } => {
                unreachable!("an open is asked of the host before the menu is kept")
            }
            Wants::Pull(work, wanted) => match self.worked(&on) {
                Ok(worked) => {
                    let others = self.others(&on);
                    Asked::Pull {
                        menu: number,
                        on,
                        work,
                        wanted,
                        worked,
                        others,
                    }
                }
                Err(why) => self.pulled(number, Err(why)),
            },
            Wants::Printed {
                session,
                view,
                ended,
            } => Asked::Printed {
                menu: number,
                on,
                session,
                view,
                ended,
            },
            Wants::Checks { page } => Asked::Checks {
                menu: number,
                on,
                page,
            },
            Wants::Make { cwd, name, agent } => Asked::Make {
                menu: number,
                on,
                cwd,
                name,
                agent,
            },
            Wants::Prunes { dir } => Asked::Prunes {
                menu: number,
                on,
                dir,
            },
            Wants::Rebase { dir, branch } => Asked::Rebase {
                menu: number,
                on,
                dir,
                branch,
            },
            Wants::Prune { dir, agreed } => Asked::Prune {
                menu: number,
                on,
                dir,
                agreed,
            },
            Wants::Plan { session, scope } => {
                let plan = self.plan(&on, &session, scope);
                self.planned(plan)
            }
        }
    }

    fn planned(&mut self, plan: Result<Plan, String>) -> Asked {
        let Some((menu, placed)) = self.take_open() else {
            return Asked::Nothing;
        };
        self.menu_did(placed, menu.planned(plan))
    }

    fn prunes(&mut self, menu: u64, plan: Result<prunes::Plan, String>) -> Asked {
        let Some((waiting, taken)) = self.take_waited(menu, Menu::sweeping) else {
            return Asked::Nothing;
        };
        if let Ok(plan) = &plan {
            log_unasked(plan);
        }
        self.menu_did_after(taken, waiting.swept(plan))
    }

    fn pruned(&mut self, menu: u64, pruned: Result<prunes::Pruned, String>) -> Asked {
        let waiting = self.take_waited(menu, Menu::pruning);
        let taken = waiting.as_ref().map_or(Taken::Kept, |(_, taken)| *taken);
        match pruned {
            Ok(prunes::Pruned::Changed(plan)) => {
                log_unasked(&plan);
                if let Some((waiting, taken @ Taken::Open(_))) = waiting {
                    self.menu_did_after(taken, waiting.changed(plan))
                } else {
                    self.say_of(
                        taken,
                        "menu: nothing deleted: what would go changed".to_owned(),
                    );
                    Asked::Nothing
                }
            }
            Ok(prunes::Pruned::Went(outcomes)) => {
                if let Some(note) = prune::note(&outcomes) {
                    self.say_of(taken, note);
                }
                Asked::Nothing
            }
            Err(why) => {
                self.say_of(
                    taken,
                    format!("menu: it may have deleted some worktrees: {why}"),
                );
                Asked::Nothing
            }
        }
    }

    fn pulled(&mut self, menu: u64, found: Result<Found, String>) -> Asked {
        let Some((waiting, taken)) = self.take_waited(menu, Menu::pulling) else {
            return Asked::Nothing;
        };
        if let Some(off_branch) = waiting.off_branch(&found) {
            self.say_of(taken, off_branch);
        }
        self.menu_did_after(taken, waiting.pulled(found))
    }

    fn printed(&mut self, menu: u64, printed: Result<Printed, String>) -> Asked {
        let Some((waiting, taken)) = self.take_waited(menu, Menu::reading) else {
            return Asked::Nothing;
        };
        self.menu_did_after(taken, waiting.printed(printed))
    }

    fn checks(&mut self, menu: u64, checks: Result<checks::Failing, String>) -> Asked {
        let Some((waiting, taken)) = self.take_waited(menu, Menu::fetching) else {
            return Asked::Nothing;
        };
        self.menu_did_after(taken, waiting.checked(checks))
    }

    fn rebased(&mut self, menu: u64, rebased: Result<Rebased, Unrebased>) -> Asked {
        let Some((waiting, taken)) = self.take_waited(menu, Menu::rebasing) else {
            return Asked::Nothing;
        };
        self.menu_did_after(taken, waiting.rebased(rebased))
    }

    fn worked(&self, on: &Machine) -> Result<Vec<Worked>, String> {
        let Some(seen) = self.seen(on) else {
            return match on {
                Machine::Local => Ok(Vec::new()),
                Machine::Remote(destination) => {
                    Err(format!("luned does not reach {destination} now"))
                }
            };
        };
        let pulls = pull_requests(seen.pull_requests);
        let mut worked = Vec::new();
        for (worktree, view) in seen.views() {
            let dir = Directory::from(&view.cwd);
            let pull = pull_request_link(&pulls, dir.as_str());
            let here = Self::seats_here(self.seated_on(on), worktree.project.listed());
            let place = ViewPlace::of(worktree, view);
            let it = links::Worked { dir, pull, here };
            worked.push(Worked { place, works: it });
        }
        Ok(worked)
    }

    fn others(&self, on: &Machine) -> Others {
        let Some(host) = &self.host else {
            return Vec::new();
        };
        let others = machines(host, &self.shelves)
            .each()
            .map(|(machine, _)| machine);
        others
            .filter(|machine| machine != on)
            .map(|machine| {
                let worked = self.worked(&machine);
                (machine, worked)
            })
            .collect()
    }

    fn plan(&self, on: &Machine, listed: &Listed, scope: Scope) -> Result<Plan, String> {
        if let Some(why) = self.out_of_reach(on) {
            return Err(why);
        }
        let act = match scope {
            Scope::View(view) => Act::View(view),
            Scope::Worktree(worktree) => self.worktree_closes(on, worktree)?,
            Scope::Project => self.project_closes(on, listed)?,
        };
        let agents = self.agents_in(on, listed);
        Ok(Plan::of(act, agents.as_deref().map_err(|untold| *untold)))
    }

    fn project_closes(&self, on: &Machine, listed: &Listed) -> Result<Act, String> {
        let unlisted = || format!("project {} is no longer listed", listed.name());
        let project = listed.project();
        let known = shelf(&self.shelves, on)
            .iter()
            .find(|known| known.project.id() == project);
        let known = known.ok_or_else(unlisted)?;
        let worktrees = known
            .worktrees
            .iter()
            .map(|worktree| worktree.place.worktree)
            .collect();
        let seen = known.worktrees.iter().flat_map(KnownWorktree::terminal_ids);
        let session = listed.name().session().clone();
        Ok(Act::Project {
            session,
            project,
            worktrees,
            seen: seen.collect(),
        })
    }

    fn worktree_closes(&self, on: &Machine, worktree: WorktreeId) -> Result<Act, String> {
        let worktrees = shelf(&self.shelves, on)
            .iter()
            .flat_map(|known| &known.worktrees);
        let mut held = worktrees.filter(|it| it.place.worktree == worktree);
        let Some(found) = held.next() else {
            return Err(format!("worktree {worktree} is no longer listed"));
        };
        let views = found.views().map(|view| view.key).collect();
        Ok(Act::Worktree {
            worktree,
            views,
            seen: found.terminal_ids().collect(),
        })
    }

    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        match on {
            Machine::Remote(destination) if self.seen(on).is_none() => {
                Some(format!("menu: luned does not reach {destination} now"))
            }
            Machine::Remote(_) | Machine::Local => None,
        }
    }

    fn agents_in(
        &self,
        on: &Machine,
        listed: &Listed,
    ) -> Result<Vec<(TerminalKey, close::Doing)>, Untold> {
        let Some(seen) = self.seen(on).filter(|_| self.away.is_none()) else {
            return Err(Untold::Away);
        };
        seen.unread_seats()
            .map_err(Untold::Unplaced)
            .map(|unread_seats| {
                let (seated, held) = (seen.seated(), seen.held_by(listed));
                let doing = seated.iter().filter(|(key, _)| held.contains(key));
                let doing = doing.map(|(key, agent)| (*key, close::Doing::from(*agent)));
                doing
                    .chain(close::unread_in(&unread_seats, &held))
                    .collect()
            })
    }

    fn brief(&mut self, taken: Taken, briefed: Briefed) -> Asked {
        let Some(host) = &self.host else {
            return Asked::Nothing;
        };
        let machines = machines(host, &self.shelves);
        let mut reached = vec![(Machine::Local, machines.here)];
        let mut unknown = Vec::new();
        for (destination, seen) in machines.others {
            match seen {
                Ok(seen) => reached.push((Machine::Remote(destination), seen)),
                Err(away) => unknown.push((destination, away.said().0)),
            }
        }
        let mut every: Vec<(Machine, SessionName)> = Vec::new();
        let mut notes = Vec::new();
        for (machine, seen) in &reached {
            let agents = seen.seated();
            let statuses = git_statuses(seen.git_statuses);
            for known in seen.projects {
                let listed = known.listed();
                every.push((machine.clone(), listed.name.session().clone()));
                for view in known.views() {
                    let Some(agent) = agents.get(&view.key) else {
                        continue;
                    };
                    let cwd = view.cwd.as_str();
                    let branch = statuses.get(cwd).and_then(|status| {
                        checkout(status, None).map(|checkout| checkout.branch.into())
                    });
                    notes.push(Note {
                        on: machine.clone(),
                        session: listed.name.session().clone(),
                        view: ViewKey::Terminal(view.key),
                        name: label(Some(agent), view),
                        status: status_word(agent),
                        since: agent.status_since.and_then(|at| at.try_into().ok()),
                        branch,
                        cwd: cwd.to_owned(),
                        transcript: transcript(agent),
                    });
                }
            }
        }
        let on = match &briefed {
            Briefed::Every => None,
            Briefed::Machine(on) | Briefed::Session(on, _) | Briefed::Agent(on, ..) => Some(on),
        };
        if let Some(on @ Machine::Remote(destination)) = on
            && !reached.iter().any(|(machine, _)| machine == on)
        {
            self.say_of(
                taken,
                format!("brief: luned does not reach {destination} now"),
            );
            return Asked::Nothing;
        }
        let over = match briefed {
            Briefed::Every => Over::Sessions {
                sessions: every,
                unknown,
            },
            Briefed::Machine(on) => {
                let of = every.into_iter().filter(|(machine, _)| *machine == on);
                let sessions = of.map(|(_, session)| session).collect();
                Over::Machine(on, sessions)
            }
            Briefed::Session(on, session) => Over::Session(on, session),
            Briefed::Agent(on, session, view) => Over::Agent(on, session, view),
        };
        match Brief::new(over, notes) {
            Ok(brief) => Asked::Brief(brief),
            Err(reason) => {
                self.say_of(taken, reason);
                Asked::Nothing
            }
        }
    }

    fn close(&mut self, on: Machine, act: Act) -> Asked {
        let ticket = self.ticket();
        let takes = |spot: &Spot| closed_by(&act, &on, spot);
        if let Some(spot) = self.lit_spot().filter(|lit| takes(lit)).cloned() {
            let at = self.row_of(&spot).expect("the highlight is on a row");
            let stays = |index: &usize| self.stops(*index) && !takes(&self.rows[*index].spot());
            let landing = (at..self.rows.len()).chain((0..at).rev()).find(stays);
            let landing = landing.map(|index| self.rows[index].spot());
            self.lit = Some(Lit::Closing {
                spot,
                ticket,
                landing,
            });
        }
        Asked::Close { on, act, ticket }
    }

    fn unclosed(&mut self, on: &Machine, act: &Act, ticket: u64, unchanged: Unchanged) {
        if let Some(Lit::Closing {
            spot,
            ticket: armed,
            ..
        }) = &self.lit
            && *armed == ticket
        {
            let closes = "the close a ticket names closes the row its d d left lit";
            assert!(closed_by(act, on, spot), "{closes}");
            let listed = "a closing row stays listed: rebuild lands past it once it goes";
            assert!(self.row_of(spot).is_some(), "{listed}");
            if let Unchanged::Refused(_) = unchanged {
                self.lit = Some(Lit::On(spot.clone()));
            }
        }
        let in_doubt = format!("the {} may be closed", act.noun());
        self.notify(format!("menu: {}", unchanged.said("not closed", &in_doubt)));
    }

    fn ticket(&mut self) -> u64 {
        self.tickets += 1;
        self.tickets
    }

    fn made_for(&mut self, menu: u64) -> Option<Held> {
        let made = |it: &Menu| it.opens_pulled().then_some(());
        self.take_seen(menu, made)
            .map(|(_, taken, ())| taken.held())
    }

    fn found(
        &mut self,
        menu: u64,
        agent: agents::Harness,
        skills: Result<Vec<Skill>, String>,
    ) -> bool {
        if let Some(open) = self.open_mut().filter(|it| it.number() == menu) {
            open.found(agent, skills);
            return true;
        }
        false
    }

    fn toggle_unread(&mut self, now: SystemTime) {
        let Some(spot) = self.lit_spot() else {
            return;
        };
        let Some(Row::View {
            place,
            view_id,
            unread,
            marked,
            ..
        }) = self.rows.iter().find(|row| row.spot() == *spot)
        else {
            return;
        };
        let key = place.listed().key(*view_id);
        if dotted(*unread, marked.as_ref()) {
            self.news.read(key, self.finished.get(&key).copied(), now);
        } else {
            self.news.mark_unread(key, now);
        }
        self.rebuild(now);
    }

    fn search_key(&mut self, key: &KeyEvent, mut term: String, wall: SystemTime) -> Asked {
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Home => self.end(false),
            KeyCode::End => self.end(true),
            KeyCode::Enter => return self.go(wall),
            KeyCode::Esc => self.close_search(wall),
            KeyCode::Backspace if term.is_empty() => self.close_search(wall),
            KeyCode::Backspace => {
                term.pop();
                self.find(term, wall);
            }
            KeyCode::Char(c) => {
                term.push(c);
                self.find(term, wall);
            }
            _ => {}
        }
        Asked::Nothing
    }

    fn search(&self) -> Option<&str> {
        match &self.mode {
            Mode::Search(Search::Empty) => Some(""),
            Mode::Search(Search::Typed { term, .. }) => Some(term.as_str()),
            Mode::Idle
            | Mode::Pending(_)
            | Mode::Reorder(_)
            | Mode::Menu { .. }
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => None,
        }
    }

    fn typed(&self) -> bool {
        matches!(self.mode, Mode::Search(Search::Typed { .. }))
    }

    fn find(&mut self, term: String, now: SystemTime) {
        let search = if term.is_empty() {
            Search::Empty
        } else {
            Search::Typed { term, seek: true }
        };
        self.enter(Mode::Search(search));
        self.rebuild(now);
    }

    fn close_search(&mut self, now: SystemTime) {
        if let Mode::Search(_) = self.mode {
            self.mode = Mode::Idle;
            self.rebuild(now);
        }
    }

    fn go(&mut self, now: SystemTime) -> Asked {
        let Some(spot) = self.lit_spot().cloned() else {
            return Asked::Nothing;
        };
        let Some((on, place)) = spot.placed() else {
            self.toggle(now);
            return Asked::Nothing;
        };
        self.go_to(on, place, now)
    }

    fn go_to(&mut self, on: Machine, place: Place, now: SystemTime) -> Asked {
        self.close_search(now);
        Asked::Go { on, place }
    }

    fn mouse(&mut self, mouse: MouseEvent, wall: SystemTime) -> Asked {
        let Some(area) = self.viewport.expanded_area() else {
            return Asked::Nothing;
        };
        self.mouse_in(mouse, area, wall)
    }

    fn mouse_in(&mut self, mouse: MouseEvent, area: Rect, wall: SystemTime) -> Asked {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
        ) {
            self.unpend();
        }
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                let max = self.lines().saturating_sub(1);
                if let Viewport::Expanded { laid, .. } = &mut self.viewport {
                    laid.top = (laid.top + 3).min(max);
                }
                Asked::Nothing
            }
            MouseEventKind::ScrollUp => {
                if let Viewport::Expanded { laid, .. } = &mut self.viewport {
                    laid.top = laid.top.saturating_sub(3);
                }
                Asked::Nothing
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let on = match self
                    .line_of(mouse.row, area)
                    .and_then(|line| self.row_at(line))
                {
                    None => Pressed::Space,
                    Some(at) => match self.lifting(at) {
                        Some(movable) => Pressed::Movable(movable),
                        None => Pressed::Row(self.rows[at].spot()),
                    },
                };
                let _ = self.close_menu();
                self.unselect();
                let at = Position::new(mouse.column, mouse.row);
                self.set_moving(Moving::Down { at, on });
                Asked::Nothing
            }
            MouseEventKind::Down(_) => Asked::Take,
            MouseEventKind::Drag(MouseButton::Left) => self.drag(mouse, area),
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse, area, wall),
            MouseEventKind::Moved => {
                let _ = self.let_go();
                Asked::Nothing
            }
            _ => Asked::Nothing,
        }
    }

    fn set_moving(&mut self, next: Moving) {
        let Viewport::Expanded { moving, .. } = &mut self.viewport else {
            unreachable!("only an expanded sidebar holds pointer motion");
        };
        *moving = next;
    }

    fn drag(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let line = mouse.row.saturating_sub(area.y);
        let over = (area.left()..area.right()).contains(&mouse.column);
        let pointer = Position::new(mouse.column, mouse.row);
        let (from, lifts) = match self.viewport.moving_mut() {
            Some(Moving::Lifted(lift)) => {
                lift.line = line;
                lift.over = over;
                return Asked::Nothing;
            }
            Some(Moving::Sweeping(sweep)) => {
                sweep.reach(pointer, inside(area));
                return Asked::Nothing;
            }
            Some(Moving::Down { at, on }) => {
                (*at, on.movable().cloned().filter(|_| mouse.row != at.y))
            }
            Some(Moving::Swept(..) | Moving::Still) | None => {
                return Asked::Nothing;
            }
        };
        let typed = self.typed();
        let lifts = lifts.filter(|lifts| {
            assert!(
                self.row_of(&lifts.spot()).is_some(),
                "a pressed movable stays listed"
            );
            !typed
        });
        if let Some(lifted) = lifts {
            self.set_moving(Moving::Lifted(Lift {
                lifted,
                line,
                over,
                scrolled: None,
            }));
            return Asked::Take;
        }
        if from.x.abs_diff(pointer.x) > 1 || from.y.abs_diff(pointer.y) > 1 {
            self.set_moving(Moving::Sweeping(Sweep::new(from, pointer, inside(area))));
        }
        Asked::Nothing
    }

    fn release(&mut self, mouse: MouseEvent, area: Rect, wall: SystemTime) -> Asked {
        let over = (area.left()..area.right()).contains(&mouse.column);
        let Some(moving) = self.viewport.moving_mut() else {
            return Asked::Nothing;
        };
        match std::mem::take(moving) {
            Moving::Down { at, on } if over && mouse.row == at.y => self.click(at, &on, area, wall),
            Moving::Lifted(Lift {
                lifted: movable, ..
            }) => {
                let line = mouse.row.saturating_sub(area.y);
                self.light(movable.spot());
                let spans = self.spans(&movable);
                match self.landing(&movable, line, over, self.viewport.top()) {
                    Some((landed, _)) if landed != spans => self.keep(&landed, wall),
                    _ => {
                        self.settle(None, false);
                        Asked::Nothing
                    }
                }
            }
            Moving::Sweeping(mut sweep) => {
                sweep.reach(Position::new(mouse.column, mouse.row), inside(area));
                let Viewport::Expanded { laid, .. } = &self.viewport else {
                    unreachable!("a sweeping sidebar is expanded");
                };
                let page = self.rendered(laid, &Moving::Still, wall).page;
                let Some(excerpt) = Excerpt::of(sweep.text(&page, inside(area))) else {
                    return Asked::Nothing;
                };
                let under = laid.clone();
                self.set_moving(Moving::Swept(sweep, under));
                Asked::CopyText(excerpt)
            }
            kept @ Moving::Swept(..) => {
                self.set_moving(kept);
                Asked::Nothing
            }
            Moving::Down { .. } | Moving::Still => Asked::Nothing,
        }
    }

    fn click(&mut self, at: Position, on: &Pressed, area: Rect, wall: SystemTime) -> Asked {
        let Some(spot) = on.spot() else {
            return Asked::Take;
        };
        let index = self.row_of(&spot).expect("a pressed row stays listed");
        let row = &self.rows[index];
        let stops = self.stops(index);
        let triangle = match row {
            Row::Project { .. } => Some(2),
            Row::Worktree { .. } => Some(3),
            Row::Machine { .. } | Row::View { .. } => None,
        };
        let typed = self.typed();
        let Some((on, place)) = spot.clone().placed() else {
            if !typed {
                self.light(spot);
                self.toggle(wall);
            }
            return Asked::Take;
        };
        let on_triangle = triangle.is_some_and(|triangle| at.x.saturating_sub(area.x) <= triangle);
        if on_triangle && !typed {
            self.light(spot);
            self.toggle(wall);
            return Asked::Take;
        }
        if stops {
            self.light(spot);
        }
        self.go_to(on, place, wall)
    }

    fn line_of(&self, y: u16, area: Rect) -> Option<usize> {
        let laid = self.viewport.laid()?;
        let line = usize::from(y.checked_sub(area.y)?);
        let shown = line.checked_sub(HEADER).filter(|line| *line < laid.room)?;
        Some(laid.top + shown)
    }

    fn pressed(&self) -> bool {
        matches!(
            self.viewport.moving(),
            Some(Moving::Down { .. } | Moving::Lifted(_) | Moving::Sweeping(_))
        )
    }

    fn unsweep(&mut self) {
        if let Some(moving @ Moving::Swept(..)) = self.viewport.moving_mut() {
            *moving = Moving::Still;
        }
    }

    fn let_go(&mut self) -> bool {
        if !self.pressed() {
            return false;
        }
        if let Some(moving) = self.viewport.moving_mut() {
            *moving = Moving::Still;
        }
        true
    }

    #[cfg(test)]
    fn scrolling(&self) -> bool {
        self.pull() != 0
    }

    fn scroll_due(&self, steady: Instant) -> Option<Instant> {
        if self.pull() == 0 {
            return None;
        }
        let Some(Moving::Lifted(lift)) = self.viewport.moving() else {
            return None;
        };
        Some(lift.scrolled.map_or(steady, |then| then + SCROLL))
    }

    fn pull(&self) -> isize {
        let Viewport::Expanded {
            laid,
            moving: Moving::Lifted(lift),
        } = &self.viewport
        else {
            return 0;
        };
        if !lift.over {
            return 0;
        }
        let room = laid.room;
        let line = usize::from(lift.line);
        let up = line <= HEADER && laid.top > 0;
        let down = line + 1 >= HEADER + room && laid.top + room < self.lines();
        isize::from(down) - isize::from(up)
    }

    fn hold(&mut self, at: Instant) -> bool {
        let by = self.pull();
        let Viewport::Expanded {
            laid,
            moving: Moving::Lifted(lift),
        } = &mut self.viewport
        else {
            return false;
        };
        if by == 0 {
            lift.scrolled = None;
            return false;
        }
        if lift
            .scrolled
            .is_some_and(|then| at.saturating_duration_since(then) < SCROLL)
        {
            return false;
        }
        lift.scrolled = Some(at);
        let was = laid.top;
        laid.top = laid.top.saturating_add_signed(by);
        laid.top != was
    }

    fn lines(&self) -> usize {
        self.rows.iter().map(Row::height).sum()
    }

    fn row_at(&self, line: usize) -> Option<usize> {
        let mut start = 0;
        for (at, row) in self.rows.iter().enumerate() {
            if line < start + row.gap() {
                return None;
            }
            let end = start + row.height();
            if line < end {
                return Some(at);
            }
            start = end;
        }
        None
    }

    fn highlight(&self) -> Highlight {
        if self.keys.held() {
            return Highlight::Lit(self.lit_index());
        }
        let row = self.here_row.as_ref().and_then(|spot| self.row_of(spot));
        row.map_or(Highlight::Off, Highlight::Resting)
    }

    fn lifted_landing_at(&self, top: usize) -> Option<(Spans, Range<usize>)> {
        match self.viewport.moving() {
            Some(Moving::Lifted(Lift {
                lifted, line, over, ..
            })) => self.landing(lifted, *line, *over, top),
            Some(Moving::Still | Moving::Down { .. } | Moving::Sweeping(_) | Moving::Swept(..))
            | None => None,
        }
    }

    fn layout_with(
        &self,
        area: Rect,
        top: usize,
        geometry: NotesGeometry,
        landing: Option<&(Spans, Range<usize>)>,
        notes: Notes,
        buttons: Spots<aloud::Button>,
    ) -> Layout {
        assert!(
            self.rows.len() <= ROWS_MAX,
            "sidebar rows stay within their upper bound"
        );
        let landed = landing.map(|(spans, _)| self.landed(&spans.rows()));
        let count = landed.as_ref().map_or(self.rows.len(), Vec::len);
        let mut rows = Vec::with_capacity(count);
        for index in 0..count {
            let at = landed.as_ref().map_or(index, |order| order[index]);
            let row = &self.rows[at];
            rows.push((row.spot(), row.height()));
        }
        let aloud_buttons = buttons.down(geometry.notes_from).placed(inside(area));
        Layout {
            area,
            room: geometry.room,
            top,
            rows,
            notes_y: geometry.notes_y,
            notes: notes.said,
            aloud_buttons,
        }
    }

    fn relayout(&mut self) {
        if let Some(area) = self.viewport.expanded_area() {
            self.relayout_at(area);
        }
    }

    fn relayout_at(&mut self, area: Rect) {
        assert!(
            self.rows.len() <= ROWS_MAX,
            "sidebar rows stay within their upper bound"
        );
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let NotesAt { notes, buttons, .. } = self.notes_at(width, height, 0);
        let geometry = notes_geometry(area, notes.lines.len());
        let mut top = self.viewport.top();
        let top_before = top;
        let mut landing = self.lifted_landing_at(top);
        let lifted = landing.is_some();
        match self.highlight() {
            Highlight::Lit(at) => {
                if let Some(at) = at
                    && !lifted
                {
                    reveal(&self.rows, &mut top, at, geometry.room);
                }
                self.revealed = None;
            }
            Highlight::Resting(at) if self.revealed != self.here_row => {
                reveal(&self.rows, &mut top, at, geometry.room);
                self.revealed.clone_from(&self.here_row);
            }
            Highlight::Resting(_) | Highlight::Off => {}
        }
        top = top.min(self.lines().saturating_sub(1));
        if top != top_before && lifted {
            landing = self.lifted_landing_at(top);
        }
        let laid = self.layout_with(area, top, geometry, landing.as_ref(), notes, buttons);
        let mut moving = match &mut self.viewport {
            Viewport::Collapsed { .. } => Moving::Still,
            Viewport::Expanded { moving, .. } => std::mem::take(moving),
        };
        if matches!(&moving, Moving::Swept(sweep, under) if !under.keeps(*sweep, &laid)) {
            moving = Moving::Still;
        }
        assert_eq!(laid.area, area, "relayout keeps the requested area");
        assert!(
            !matches!(&moving, Moving::Swept(sweep, under) if !under.keeps(*sweep, &laid)),
            "relayout clears any stale sweep"
        );
        self.viewport = Viewport::Expanded { laid, moving };
    }

    fn draw(&self, buffer: &mut Buffer, framed: Rect, wall: SystemTime) -> (Drawn, Cadence) {
        let area = framed_rows(framed);
        let drawn = match &self.viewport {
            Viewport::Collapsed {
                area: collapsed, ..
            } => {
                assert_eq!(
                    *collapsed, area,
                    "the sidebar is collapsed for the area it draws into"
                );
                self.draw_collapsed_rows(buffer, area);
                (Drawn::Nothing, Cadence::default())
            }
            Viewport::Expanded { laid, moving } => {
                assert_eq!(
                    laid.area, area,
                    "the sidebar is placed for the area it draws into"
                );
                self.draw_rows_painted(buffer, laid, moving, wall)
            }
        };
        self.frame(buffer, framed);
        drawn
    }

    #[cfg(test)]
    fn draw_rows(&self, buffer: &mut Buffer, area: Rect, wall: SystemTime) -> Drawn {
        let Viewport::Expanded { laid, moving } = &self.viewport else {
            unreachable!("an expanded sidebar is placed before draw_rows");
        };
        assert_eq!(
            laid.area, area,
            "the sidebar is placed for the area it draws into"
        );
        self.draw_rows_painted(buffer, laid, moving, wall).0
    }

    fn rendered(&self, laid: &Layout, moving: &Moving, wall: SystemTime) -> Rendered {
        assert!(
            self.rows.len() <= ROWS_MAX,
            "sidebar rows stay within their upper bound"
        );
        let area = laid.area;
        let top = laid.top;
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let frame = frame(wall);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        lines.extend(self.top(width));
        let NotesAt {
            notes,
            bottom: spotted,
            buttons,
        } = self.notes_at(width, height, frame);
        let geometry = notes_geometry(area, notes.lines.len());
        let room = geometry.room;
        let highlight = self.highlight();
        let landing = match moving {
            Moving::Lifted(Lift {
                lifted, line, over, ..
            }) => self.landing(lifted, *line, *over, top),
            Moving::Still | Moving::Down { .. } | Moving::Sweeping(_) | Moving::Swept(..) => None,
        };
        let lifted = landing.is_some();
        if self.host.is_none() {
            let waiting = if self.away.is_some() {
                ""
            } else {
                "asking luned…"
            };
            lines.push(Line::styled(
                format!(" {waiting}"),
                Style::new().add_modifier(Modifier::DIM),
            ));
        } else if self.rows.is_empty() {
            assert!(self.typed(), "only a search hides this machine's row");
            lines.push(Line::styled(
                " no match",
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        let lit = match (&landing, highlight) {
            (Some((_, lifted)), _) => Some((lifted.clone(), None, LIT)),
            (None, Highlight::Lit(at)) => {
                let held = self.selected().map(|(spans, held)| {
                    let rows = spans.rows();
                    (rows[held.start].start..rows[held.end - 1].end, None)
                });
                let one = at.map(|at| (at..at + 1, self.home_row(at)));
                held.or(one).map(|(rows, home)| (rows, home, LIT))
            }
            (None, Highlight::Resting(at)) => Some((at..at + 1, self.home_row(at), RESTING)),
            (None, Highlight::Off) => None,
        };
        let landed = landing
            .as_ref()
            .map(|(spans, _)| self.landed(&spans.rows()));
        let count = landed.as_ref().map_or(self.rows.len(), Vec::len);
        assert_eq!(
            laid.rows.len(),
            count,
            "the sidebar's laid-out row count matches its state before drawing"
        );
        let mut drawn: Vec<Line<'static>> = Vec::new();
        let mut numbers = Vec::new();
        let mut cuts = Vec::new();
        let mut marks = Vec::with_capacity(count);
        for index in 0..count {
            let at = landed.as_ref().map_or(index, |order| order[index]);
            let row = &self.rows[at];
            let (laid_spot, laid_height) = &laid.rows[index];
            assert!(
                row.is_spot(laid_spot),
                "each rendered row matches its laid-out spot"
            );
            assert_eq!(
                *laid_height,
                row.height(),
                "each rendered row matches its laid-out height"
            );
            let lights = |(rows, home, _): &&(Range<usize>, Option<usize>, Color)| {
                rows.contains(&at) || *home == Some(at)
            };
            let on = lit.as_ref().filter(lights);
            let background = on.map(|(_, _, background)| *background);
            let RowLines {
                lines: row_drawn,
                number,
                cuts: row_cuts,
            } = row_lines(row, width, frame, wall);
            assert_eq!(row_drawn.len(), row.own(), "a row draws the lines it takes");
            drawn.resize(drawn.len() + row.gap(), Line::default());
            if !lifted && let Some(number) = number {
                numbers.push(NumberAt {
                    line: drawn.len() + number.line,
                    ..number
                });
            }
            let first = drawn.len();
            marks.push((first, row.spinning(), row.since()));
            cuts.extend(row_cuts.into_iter().map(|cut| CutAt {
                line: first + cut.line,
                ..cut
            }));
            for mut line in row_drawn {
                if let Some(background) = background {
                    line = line.patch_style(Style::new().bg(background));
                }
                drawn.push(line);
            }
        }
        assert_eq!(
            top,
            top.min(drawn.len().saturating_sub(1)),
            "the scroll offset is clamped before drawing"
        );
        let inside = inside(area);
        assert_eq!(
            laid.room, room,
            "the sidebar's laid-out room matches its state before drawing"
        );
        assert_eq!(
            laid.notes_y, geometry.notes_y,
            "the sidebar's laid-out notes row matches its state before drawing"
        );
        assert_eq!(
            laid.notes, notes.said,
            "the sidebar's laid-out notes match its state before drawing"
        );
        assert_eq!(
            laid.aloud_buttons,
            buttons.down(geometry.notes_from).placed(inside),
            "the sidebar's laid-out aloud buttons match its state before drawing"
        );
        assert!(
            !matches!(moving, Moving::Swept(sweep, under) if !under.keeps(*sweep, laid)),
            "a stale sweep is cleared before drawing"
        );
        lines.extend(drawn.into_iter().skip(top).take(room));
        lines.resize(geometry.notes_from, Line::default());
        let goes_on = notes
            .goes_on
            .iter()
            .map(|line| geometry.notes_from + line)
            .collect();
        let spinners = notes.spinners;
        lines.extend(notes.lines);
        let mut spots = Spots::default();
        let mut cut_spots = Spots::default();
        let shown = top..top + room;
        let mut cadence = Cadence::default();
        if width > 0 {
            for (line, spinning, since) in marks {
                if !shown.contains(&line) {
                    continue;
                }
                if spinning {
                    cadence.spin();
                }
                if let Some(since) = since {
                    cadence.age(since, wall);
                }
            }
            if spinners
                .iter()
                .any(|line| geometry.notes_from + line < height)
            {
                cadence.spin();
            }
        }
        for NumberAt {
            line,
            columns,
            link,
        } in numbers
        {
            if shown.contains(&line) {
                spots.add(HEADER + line - top, columns, link.clone());
            }
        }
        for CutAt { line, column, took } in cuts {
            if shown.contains(&line) {
                cut_spots.add(HEADER + line - top, column..column + 1, took);
            }
        }
        let page = Page {
            lines,
            goes_on,
            cuts: cut_spots.placed(inside),
        };
        let drawn = Drawn::Sidebar {
            numbers: spots.placed(inside),
            bottom: spotted.placed(geometry.notes_from, inside),
        };
        Rendered {
            page,
            drawn,
            cadence,
        }
    }

    fn draw_rows_painted(
        &self,
        buffer: &mut Buffer,
        laid: &Layout,
        moving: &Moving,
        wall: SystemTime,
    ) -> (Drawn, Cadence) {
        let area = laid.area;
        let rendered = self.rendered(laid, moving, wall);
        put(buffer, area, &rendered.page.lines, self.framing());
        if let Moving::Sweeping(sweep) | Moving::Swept(sweep, _) = moving {
            sweep.draw(buffer, inside(area));
        }
        (rendered.drawn, rendered.cadence)
    }

    fn draw_collapsed_rows(&self, buffer: &mut Buffer, area: Rect) {
        assert!(
            matches!(self.viewport, Viewport::Collapsed { area: collapsed, .. } if collapsed == area),
            "a collapsed sidebar is collapsed for the area it draws into"
        );
        let width = usize::from(area.width.saturating_sub(1));
        let rule = if self.away.is_some() {
            Style::new().fg(ERROR)
        } else if self.image_going.is_some() || self.copied.is_some() {
            Style::new().fg(NAME)
        } else {
            Style::new().fg(moonlight::MUTE)
        };
        let mut lines = vec![Line::styled(RULE.repeat(width), rule), Line::default()];
        for (count, (glyph, style)) in tallies(self.counts) {
            if count == 0 {
                continue;
            }
            let count = cut(&count.to_string(), width.saturating_sub(glyph.width()));
            lines.push(Line::from(vec![
                Span::styled(glyph, style),
                Span::styled(count, style),
            ]));
        }
        if self.aloud.as_ref().is_some_and(aloud::Shown::paused) {
            lines.push(Line::styled(aloud::PLAY, Style::new().fg(ACCENT)));
        }
        put(buffer, area, &lines, self.framing());
    }

    fn top(&self, width: usize) -> [Line<'static>; HEADER] {
        [self.title(width), Line::default()]
    }

    fn frame(&self, buffer: &mut Buffer, framed: Rect) {
        let inside = buffer.area.intersection(framed);
        assert_eq!(inside, framed, "the sidebar is framed inside the buffer");
        let framing = self.framing();
        border(buffer, framed, framed, "", framing, framing);
    }

    fn framing(&self) -> Style {
        if self.has_keys() {
            Style::new().fg(ACCENT)
        } else {
            Style::new().fg(moonlight::LIFTED)
        }
    }

    fn title(&self, width: usize) -> Line<'static> {
        let focused = self.has_keys();
        let dim = Style::new().fg(moonlight::MUTE);
        let moon = if focused {
            Style::new().fg(ACCENT)
        } else {
            dim
        };
        let name = if focused {
            Style::new().fg(moonlight::TEXT)
        } else {
            dim
        };
        let Counts {
            projects, waiting, ..
        } = self.counts;
        let mut counts: Vec<Span<'static>> = Vec::new();
        let waits = (waiting, Mark::Waiting.glyph(0));
        for (count, (glyph, style)) in std::iter::once(waits).chain(tallies(self.counts)) {
            if count == 0 {
                continue;
            }
            if !counts.is_empty() {
                counts.push(Span::raw("  "));
            }
            counts.push(Span::styled(glyph, style));
            counts.push(Span::styled(count.to_string(), style));
        }
        let counted: usize = counts.iter().map(|span| span.content.width()).sum();
        let lead = format!(" {MOON} ");
        let named = lead.width() + menu::TITLE.width();
        let word = match projects {
            0 => String::new(),
            1 => "  1 project".to_owned(),
            n => format!("  {n} projects"),
        };
        let fits = named + word.width() + TITLE_GAP_MIN + counted + TRAILING <= width;
        let word = if fits { word } else { String::new() };
        let used = named + word.width() + counted;
        let gap = width.saturating_sub(used + TRAILING);
        let mut spans = vec![
            Span::styled(lead, moon),
            Span::styled(menu::TITLE, name.add_modifier(Modifier::BOLD)),
            Span::styled(word, dim),
            Span::raw(" ".repeat(gap)),
        ];
        spans.extend(counts);
        let line = Line::from(spans);
        if used + TRAILING <= width {
            assert_eq!(
                line.width(),
                width - TRAILING,
                "the title fills its row to the margin"
            );
        }
        line
    }

    fn notes_above(&self, width: usize, frame: usize) -> (Notes, Spots<aloud::Button>) {
        let mut notes = Notes::default();
        let error = Style::new().fg(ERROR);
        if let Some(away) = &self.away {
            notes.say(&format!("luned: {away}"), width, error);
        }
        for refused in &self.refused {
            notes.say(refused, width, error);
        }
        if self.unlisted_machines > 0 {
            let count = self.unlisted_machines;
            let machines = if count == 1 { "machine" } else { "machines" };
            let unlisted = format!(
                "luned: {count} {machines} with a destination lune does not read, or named \
                 twice, not listed"
            );
            notes.say(&unlisted, width, error);
        }
        if let Some(unread) = self.host.as_ref().and_then(Host::unread_count) {
            notes.say(&unread.to_string(), width, error);
        }
        if self
            .host
            .as_ref()
            .is_some_and(|host| host.unread_hosts.is_some_and(|it| it > 0))
        {
            let unread = "luned could not read all of lune's hosts file: its log says why";
            notes.say(unread, width, error);
        }
        if let Some(why) = self.unreached() {
            notes.say(&why, width, error);
        }
        if let Some(lost) = self.folds.lost() {
            notes.say(&format!("folds stay in this lune: {lost}"), width, error);
        }
        if let Some(lost) = self.news.lost() {
            notes.say(&format!("dots stay in this lune: {lost}"), width, error);
        }
        if let Some(notice) = &self.notice {
            notes.say(notice, width, error);
        }
        if let Some((copied, _)) = &self.copied {
            notes.say(&copied.said(note_room(width)), width, Style::new().fg(NAME));
        }
        if let Some(filed) = &self.filed {
            notes.say(filed, width, Style::new().fg(NAME));
        }
        for repository in self.filing.values() {
            let filing = format!("Filing an issue in {repository}…");
            notes.wait(&filing, width, Style::new().fg(NAME), frame);
        }
        if let Some(to) = &self.image_going {
            let going = format!("image on its way to {to}");
            notes.say(&going, width, Style::new().fg(NAME));
        }
        for working in self.working.values() {
            if let Some(doing) = working.doing() {
                notes.wait(&doing, width, Style::new().fg(NAME), frame);
            }
        }
        let mut buttons = Spots::default();
        if let Some(aloud) = &self.aloud {
            let (line, spots) = aloud.line(width);
            buttons = spots.down(notes.lines.len());
            notes.lines.push(line);
        }
        (notes, buttons)
    }

    fn bottom_room(&self) -> usize {
        let Some(area) = self.viewport.expanded_area() else {
            return 0;
        };
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let (notes, _) = self.notes_above(width, 0);
        height.saturating_sub(HEADER + 1 + notes.lines.len())
    }

    fn notes_at(&self, width: usize, height: usize, frame: usize) -> NotesAt {
        let (mut notes, buttons) = self.notes_above(width, frame);
        let room = height.saturating_sub(HEADER + 1 + notes.lines.len());
        let above = notes.lines.len();
        let (lines, spotted) = match self.bottom() {
            Bottom::Menu(menu) => {
                let (lines, spots, spinner) = menu.lines(width, room, frame);
                if let Some(at) = spinner {
                    notes.spinners.push(above + at);
                }
                let spots = spots.down(above);
                (
                    lines,
                    SidebarBottom::Menu {
                        number: menu.number(),
                        spots,
                    },
                )
            }
            Bottom::Search(search) => {
                let (lines, spots) = self.searching(search, width, room);
                (lines, SidebarBottom::Keys(Keyed::Search, spots.down(above)))
            }
            Bottom::Legend => {
                let (lines, spots) = menu::legend(self.target().as_ref(), width, room);
                (lines, SidebarBottom::Keys(Keyed::Legend, spots.down(above)))
            }
            Bottom::Group(key) => {
                let (lines, spots) = menu::grouped(key, width, room);
                (lines, SidebarBottom::Keys(Keyed::Legend, spots.down(above)))
            }
            Bottom::Keys(title, keys) => {
                let (lines, spots) = menu::shortcuts(title, keys, width, room);
                (lines, SidebarBottom::Keys(Keyed::Legend, spots.down(above)))
            }
            Bottom::AllKeys(all) => {
                let (lines, spots) = all.lines(width, room);
                (lines, SidebarBottom::Keys(Keyed::Legend, spots.down(above)))
            }
            Bottom::Nothing => (Vec::new(), SidebarBottom::Nothing),
        };
        notes.lines.extend(lines);
        NotesAt {
            notes,
            bottom: spotted,
            buttons,
        }
    }

    fn searching(
        &self,
        search: &Search,
        width: usize,
        room: usize,
    ) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
        let (term, counted) = match search {
            Search::Empty => ("", String::new()),
            Search::Typed { term, .. } => {
                let found = self
                    .rows
                    .iter()
                    .filter(|row| matches!(row, Row::View { .. }));
                (
                    term.as_str(),
                    format!("{}/{}", found.count(), self.counts.views),
                )
            }
        };
        let mut lines = Vec::with_capacity(3);
        if room >= 2 {
            lines.push(menu::rule(Action::Search.name(), &counted, width));
        }
        lines.extend((room >= 1).then(|| query(term, width)));
        if room < 3 || !self.has_keys() {
            return (lines, Spots::default());
        }
        let (keys, spots) = menu::keyed(width, &[(Hint::Enter, "Go"), (Hint::Esc, "Close")]);
        let at = lines.len();
        lines.push(keys);
        (lines, spots.down(at))
    }

    fn unreached(&self) -> Option<String> {
        if !self.has_keys() {
            return None;
        }
        let machine = self.lit_spot()?.machine();
        match self.machine_row(&machine) {
            (
                name,
                Some(Away::Unreached {
                    why,
                    ssh_said: Some(said),
                    ..
                }),
                _,
            ) => Some(format!("{name}: {why}; ssh said \"{said}\"")),
            (
                name,
                Some(Away::Unreached {
                    why,
                    ssh_said: None,
                    ..
                }),
                _,
            ) => Some(format!("{name}: {why}")),
            (name, Some(Away::Here), _) => Some(format!("{name}: {}", luned::THIS_MACHINE)),
            (_, None | Some(Away::Reaching), _) => None,
        }
    }

    fn bottom(&self) -> Bottom<'_> {
        match &self.mode {
            Mode::Menu { menu, pause } if pause.is_paused(menu) => Bottom::Nothing,
            Mode::Menu { menu, .. } => Bottom::Menu(menu),
            Mode::Search(search) => Bottom::Search(search),
            _ if !self.has_keys() => Bottom::Nothing,
            Mode::Pending(Pending::Legend) => Bottom::Legend,
            Mode::Pending(Pending::AllKeys(all)) => Bottom::AllKeys(all),
            Mode::Pending(Pending::Group {
                key,
                wait: ChordWait::Shown,
            }) => Bottom::Group(*key),
            Mode::Pending(Pending::First(ChordWait::Shown)) => {
                if self
                    .target()
                    .is_some_and(|target| target.fits(Action::Rebase))
                {
                    Bottom::Keys(GO_GIT, &GO_GIT_KEYS)
                } else {
                    Bottom::Keys(GO, &GO_KEYS)
                }
            }
            Mode::Reorder(_) => Bottom::Keys(Action::Reorder.name(), &SELECTED_KEYS),
            Mode::Idle
            | Mode::Pending(
                Pending::First(ChordWait::Until(_))
                | Pending::Group {
                    wait: ChordWait::Until(_),
                    ..
                },
            )
            | Mode::Afloat { .. }
            | Mode::AfloatNote(_) => Bottom::Nothing,
        }
    }
}

fn reveal(rows: &[Row], top: &mut usize, at: usize, room: usize) {
    let start: usize = rows[..at].iter().map(Row::height).sum();
    let end = start + rows[at].height();
    if start < *top {
        *top = start;
    } else if end > *top + room {
        *top = end.saturating_sub(room);
    }
    if end - start <= room {
        assert!(*top <= start);
        assert!(end <= *top + room);
    }
}

enum Bottom<'a> {
    Menu(&'a Menu),
    Search(&'a Search),
    Legend,
    Group(char),
    Keys(&'static str, &'static [(Label, &'static str)]),
    AllKeys(&'a AllKeys),
    Nothing,
}

fn tallies(counts: Counts) -> [(usize, (String, Style)); 3] {
    let dot = dot();
    [
        (counts.new, (dot.content.into_owned(), dot.style)),
        (counts.busy, Mark::Busy.glyph(0)),
        (counts.shell, Mark::Shell.glyph(0)),
    ]
}

fn framed_rows(framed: Rect) -> Rect {
    let x = framed.x.saturating_add(1).min(framed.right());
    let y = framed.y.saturating_add(1).min(framed.bottom());
    let width = framed.right() - x;
    let height = framed.bottom().saturating_sub(1).max(y) - y;
    let rows = Rect {
        x,
        y,
        width,
        height,
    };
    assert_eq!(
        rows.right(),
        framed.right(),
        "the rows share the frame's right column"
    );
    let height = framed.height.saturating_sub(2);
    assert_eq!(rows.height, height, "the frame takes a row above and below");
    assert!(
        framed.y <= rows.y && rows.bottom() <= framed.bottom(),
        "the rows are in the frame"
    );
    rows
}

pub fn edge(area: Rect) -> Option<u16> {
    area.width.checked_sub(1).map(|last| area.x + last)
}

fn inside(area: Rect) -> Rect {
    Rect {
        width: area.width.saturating_sub(1),
        ..area
    }
}

fn put(buffer: &mut Buffer, area: Rect, lines: &[Line<'static>], framing: Style) {
    buffer.set_style(area, Style::reset().fg(moonlight::TEXT));
    for (line, y) in lines.iter().zip(area.top()..area.bottom()) {
        buffer.set_line(area.x, y, line, area.width.saturating_sub(1));
    }
    if let Some(x) = edge(area) {
        for y in area.top()..area.bottom() {
            buffer.set_string(x, y, EDGE, framing);
        }
    }
}

pub fn query(term: &str, width: usize) -> Line<'static> {
    let typed = tail(&format!("{term}_"), width.saturating_sub(1));
    Line::from(vec![
        Span::raw(" "),
        Span::styled(typed, Style::new().fg(ACCENT)),
    ])
}

fn pull_requests(pull_requests: &[luned::api::PullRequest]) -> HashMap<&str, PullRequest> {
    ParsedPullRequests::of_api(pull_requests).by_dir
}

fn git_statuses(statuses: &[luned::api::GitStatus]) -> HashMap<&str, GitStatus> {
    ParsedGitStatuses::of_api(statuses).by_dir
}

fn anomaly_note(machine: &Machine, why: &str) -> String {
    match machine {
        Machine::Local => format!("local: {why}"),
        Machine::Remote(destination) => format!("{destination}: {why}"),
    }
}

fn pull_request_link(pulls: &HashMap<&str, PullRequest>, dir: &str) -> Option<Link> {
    Link::parse(pulls.get(dir)?.page.url())
}

fn moved<T: Clone>(items: &[T], taken: Range<usize>, to: usize) -> Vec<T> {
    let left = items
        .iter()
        .enumerate()
        .filter(|(at, _)| !taken.contains(at));
    let mut moved: Vec<T> = left.map(|(_, item)| item.clone()).collect();
    moved.splice(to..to, items[taken].iter().cloned());
    moved
}

fn transcript(agent: &Agent) -> Transcript {
    if agents::of(agent).is_some_and(|harness| harness.topics().has_transcript()) {
        agent
            .session_id
            .as_deref()
            .and_then(SessionId::parse)
            .map_or(Transcript::Unnamed, Transcript::Named)
    } else {
        Transcript::NotRead
    }
}

fn label(agent: Option<&Agent>, view: &KnownView) -> String {
    let view_topic = view
        .topic
        .as_deref()
        .map(str::trim)
        .filter(|topic| !topic.is_empty());
    let label = if let Some(agent) = agent {
        let uses_terminal_title =
            agents::of(agent).is_some_and(|harness| harness.topics().has_terminal_title());
        let view_topic = view_topic.filter(|_| uses_terminal_title);
        let topic = agent
            .live_topic()
            .or(view_topic)
            .or_else(|| auto_name(agent));
        let name = given_name(agent).or(topic);
        name.map_or_else(|| cwd_label(&view.cwd), str::to_owned)
    } else if view.ended {
        view_topic.map_or_else(|| cwd_label(&view.cwd), str::to_owned)
    } else {
        let mut words = view
            .command
            .iter()
            .flat_map(|command| command.split_whitespace());
        let program = words
            .next()
            .map(|program| {
                let program = program.rsplit('/').next().unwrap_or(program);
                program.strip_prefix('-').unwrap_or(program)
            })
            .filter(|program| !program.is_empty());
        let uses_terminal_title = program
            .and_then(agents::named)
            .is_some_and(|harness| harness.topics().has_terminal_title());
        if uses_terminal_title && let Some(topic) = view_topic {
            topic.to_owned()
        } else {
            match program {
                Some(program) if !icons::SHELLS.contains(&program) => [program]
                    .into_iter()
                    .chain(words)
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => cwd_label(&view.cwd),
            }
        }
    };
    assert!(!label.trim().is_empty(), "a view label is non-empty");
    label
}

fn cwd_label(cwd: &Dir) -> String {
    let short = short_cwd(cwd.as_str());
    let label = if short.trim().is_empty() {
        cwd.as_str().to_owned()
    } else {
        short
    };
    assert!(!label.trim().is_empty(), "a cwd label is non-empty");
    label
}

fn agent_marked(news: &News, agent: &Agent, key: Key, here: bool) -> (Marked, Option<SystemTime>) {
    let mut marked = Marked::of(agent);
    let done = marked
        .since
        .filter(|_| agent.is_finished_turn() || marked.mark == Mark::Error);
    if !here && news.is_new(key, done) {
        marked.mark = if marked.mark == Mark::Error {
            Mark::Failed
        } else {
            Mark::Finished
        };
    }
    (marked, done)
}

fn status_word(agent: &Agent) -> String {
    match AgentStatus::of_api(agent) {
        AgentStatus::Blocked { .. } => "waiting".to_owned(),
        AgentStatus::Idle => "idle".to_owned(),
        AgentStatus::Stopped => "stopped".to_owned(),
        AgentStatus::Done { .. } => "done".to_owned(),
        AgentStatus::Working => "busy".to_owned(),
        AgentStatus::Error { .. } => "error".to_owned(),
        AgentStatus::Shell => "shell".to_owned(),
        AgentStatus::Other(word) => word.into(),
        AgentStatus::Unreported => "unreported".to_owned(),
    }
}

fn given_name(agent: &Agent) -> Option<&str> {
    agent.name.as_ref()?.given()
}

fn auto_name(agent: &Agent) -> Option<&str> {
    agent.name.as_ref()?.auto()
}

pub fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_owned(),
        [only] => (*only).to_owned(),
        [.., parent, base] => format!("{parent}/{base}"),
    }
}

pub type Fit = (bool, Score);

pub struct Finder<'a> {
    fuzzy: Fuzzy,
    anywhere: Fuzzy,
    term: &'a str,
}

impl<'a> Finder<'a> {
    pub fn new(term: &'a str, typos: Typos) -> Self {
        Finder {
            fuzzy: Fuzzy::new(term, typos),
            anywhere: Fuzzy::anywhere(term, typos),
            term,
        }
    }

    fn session(&self, name: &str) -> Option<(Fit, Vec<usize>)> {
        self.find(name, &[])
    }

    fn worktree(
        &self,
        name: &str,
        branch: Option<&str>,
        folder: &str,
    ) -> Option<(Fit, Vec<usize>)> {
        let folder = short_cwd(folder);
        self.find(name, &[branch, Some(folder.as_str())])
    }

    fn view(
        &self,
        label: &str,
        branch: Option<&str>,
        worktree: &str,
        cwd: &str,
    ) -> Option<(Fit, Vec<usize>)> {
        let cwd = short_cwd(cwd);
        self.find(label, &[branch, Some(worktree), Some(cwd.as_str())])
    }

    pub fn find(&self, shown: &str, also: &[Option<&str>]) -> Option<(Fit, Vec<usize>)> {
        let exact = shown == self.term;
        let best = |fuzzy: &Fuzzy| {
            also.iter()
                .flatten()
                .filter_map(|text| fuzzy.score(text))
                .max()
        };
        let other = best(&self.fuzzy);
        let Some((score, hits)) = self.fuzzy.hits(shown) else {
            return Some(((exact, other?), Vec::new()));
        };
        let fits = |score: Score, other: Option<Score>| other.is_none_or(|other| score >= other);
        let lit = fits(score, other)
            || self
                .anywhere
                .score(shown)
                .is_some_and(|anywhere| fits(anywhere, best(&self.anywhere)));
        let fit = (exact, other.map_or(score, |other| score.max(other)));
        Some((fit, if lit { hits } else { Vec::new() }))
    }
}

fn log_unasked(plan: &prunes::Plan) {
    for unasked in &plan.unasked {
        log::say(&format!("{}: {}", unasked.folder, unasked.why));
    }
}

fn git_in(seen: Option<&Seen<'_>>, dir: &Directory) -> (Option<Said>, Option<String>) {
    let Some(status) = seen.and_then(|seen| git_statuses(seen.git_statuses).remove(dir.as_str()))
    else {
        return (None, None);
    };
    let branch = checkout(&status, None).map(|checkout| checkout.branch);
    (branch, repository_of(&status))
}

fn repository_of(status: &GitStatus) -> Option<String> {
    match status {
        GitStatus::NoCheckout { repository }
        | GitStatus::Checkout {
            repository: Some(repository),
            ..
        } => Some(repository.as_str().to_owned()),
        GitStatus::Outside
        | GitStatus::Checkout {
            repository: None, ..
        } => None,
    }
}

fn checkout(status: &GitStatus, pull_request: Option<&PullRequest>) -> Option<Checkout> {
    let GitStatus::Checkout {
        head: Head::Branch {
            name,
            upstream,
            changed,
        },
        changes,
        stashes,
        ..
    } = status
    else {
        return None;
    };
    Some(Checkout {
        branch: name.clone(),
        status: glyphs(changes, *stashes, *upstream),
        changed: *changed,
        pull_request: pull_request.and_then(numbered),
    })
}

fn numbered(pull_request: &PullRequest) -> Option<Numbered> {
    let state = match pull_request.state {
        State::Open {
            draft,
            checks,
            mergeability,
        } => NumberedState::Open {
            draft,
            checks: checks.and_then(|it| it.verdict()),
            conflicting: mergeability.is_some_and(Mergeability::is_conflicting),
            stale: pull_request.stale,
        },
        State::Merged => NumberedState::Merged,
        State::Closed => return None,
    };
    let number = pull_request.number();
    let link = Link::parse(pull_request.page.url());
    let number = link.map_or(Number::Unlinked(number), |link| {
        Number::Linked(Box::new(link))
    });
    Some(Numbered { number, state })
}

fn verdict_mark(verdict: Verdict, stale: bool) -> Span<'static> {
    let (mark, colour) = match verdict {
        Verdict::Failing => ("✗", moonlight::RED),
        Verdict::Pending => ("●", moonlight::YELLOW),
        Verdict::Passing => ("✓", moonlight::GREEN),
    };
    let colour = if stale { moonlight::MUTE } else { colour };
    Span::styled(mark, Style::new().fg(colour))
}

fn conflict_mark(stale: bool) -> Span<'static> {
    let colour = if stale {
        moonlight::MUTE
    } else {
        moonlight::RED
    };
    Span::styled("≠", Style::new().fg(colour))
}

fn glyphs(changes: &Changes, stashes: u32, upstream: Option<Upstream>) -> String {
    let kinds = [
        (changes.conflicted, '='),
        (stashes, '$'),
        (changes.deleted, '✘'),
        (changes.renamed, '»'),
        (changes.modified, '!'),
        (changes.staged, '+'),
        (changes.untracked, '?'),
    ];
    let mut glyphs: String = kinds
        .iter()
        .filter(|(count, _)| *count > 0)
        .map(|(_, glyph)| *glyph)
        .collect();
    match upstream.map(|upstream| (upstream.ahead, upstream.behind)) {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => glyphs.push('⇕'),
        Some((ahead, _)) if ahead > 0 => glyphs.push('⇡'),
        Some((_, behind)) if behind > 0 => glyphs.push('⇣'),
        _ => {}
    }
    glyphs
}

fn age_unit(seconds: u64) -> (u64, char) {
    match seconds {
        0..AGE_MINUTE_SECONDS => (1, 's'),
        AGE_MINUTE_SECONDS..AGE_HOUR_SECONDS => (AGE_MINUTE_SECONDS, 'm'),
        AGE_HOUR_SECONDS..AGE_DAY_SECONDS => (AGE_HOUR_SECONDS, 'h'),
        AGE_DAY_SECONDS..AGE_WEEK_SECONDS => (AGE_DAY_SECONDS, 'd'),
        AGE_WEEK_SECONDS.. => (AGE_WEEK_SECONDS, 'w'),
    }
}

pub fn age(since: SystemTime, wall: SystemTime) -> String {
    let seconds = wall
        .duration_since(since)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (step, unit) = age_unit(seconds);
    assert!(step > 0, "an age unit spans at least one second");
    format!("{}{unit}", seconds / step)
}

#[must_use]
pub fn next_age(since: SystemTime, wall: SystemTime) -> Option<SystemTime> {
    let seconds = wall
        .duration_since(since)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let (step, _) = age_unit(seconds);
    assert!(step > 0, "an age unit spans at least one second");
    let floored = seconds - (seconds % step);
    let next = floored.checked_add(step)?;
    let at = since.checked_add(Duration::from_secs(next))?;
    assert!(
        at > wall,
        "the next age tick lands strictly after wall time"
    );
    Some(at)
}

pub fn frame(wall: SystemTime) -> usize {
    let since = wall
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let turns = since.as_millis() / SPIN.as_millis();
    let frames = u128::try_from(SPINNER.len()).expect("a spinner of ten frames");
    usize::try_from(turns % frames).expect("a frame of the spinner's")
}

#[must_use]
pub fn next_spin(wall: SystemTime) -> Duration {
    let Ok(since) = wall.duration_since(SystemTime::UNIX_EPOCH) else {
        return SPIN;
    };
    let step = SPIN.as_nanos();
    let left = step - (since.as_nanos() % step);
    let left = u64::try_from(left).expect("a spin step fits in nanoseconds of u64");
    let wait = Duration::from_nanos(left);
    assert!(
        wait > Duration::ZERO,
        "the next spinner frame is in the future"
    );
    assert!(
        wait <= SPIN,
        "the next spinner frame is within one spin step"
    );
    wait
}

struct NumberAt<'a> {
    line: usize,
    columns: Range<usize>,
    link: &'a Link,
}

struct CutAt {
    line: usize,
    column: usize,
    took: String,
}

fn cut_at(line: usize, column: usize, text: &str, room: usize) -> Option<CutAt> {
    let shown = cut(text, room);
    if shown == text {
        return None;
    }
    let kept = shown.strip_suffix('…')?;
    let took = text
        .strip_prefix(kept)?
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    Some(CutAt {
        line,
        column: column + cells(kept),
        took,
    })
}

fn machine_label(name: &str, shown: &str) -> String {
    let Some(rest) = shown.strip_prefix(name) else {
        return shown.to_owned();
    };
    let label = format!("{}{rest}", name.to_ascii_uppercase());
    assert_eq!(
        cells(&label),
        cells(shown),
        "uppercasing keeps a machine's width"
    );
    label
}

struct RowLines<'a> {
    lines: Vec<Line<'static>>,
    number: Option<NumberAt<'a>>,
    cuts: Vec<CutAt>,
}

fn row_lines(row: &Row, width: usize, frame: usize, now: SystemTime) -> RowLines<'_> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match row {
        Row::Machine {
            machine,
            name,
            away,
            heads: Heads::Nothing,
        } => {
            let (word, since) = away.as_ref().map_or((NO_PROJECTS, None), Away::said);
            let age = since.map_or_default(|since| age(since, now));
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            let room = width.saturating_sub(MACHINE_LEAD + reserve + 1);
            let word = format!(" · {word}");
            let (text, word) = match machine {
                Machine::Local => (name.clone(), word),
                Machine::Remote(_) => (format!("{name}{word}"), String::new()),
            };
            let named = room.saturating_sub(cells(&word)).max(room.min(1));
            let shown = cut(&text, named);
            let worded = room - cells(&shown);
            let said = format!("{}{}", machine_label(name, &shown), cut(&word, worded));
            assert!(
                cells(&said) <= room,
                "a machine's name and word fit their room"
            );
            let gap = width.saturating_sub(MACHINE_LEAD + cells(&said) + age.width() + 1);
            let line = Line::from(vec![
                Span::raw(" "),
                Span::styled(FOLDED, dim),
                Span::raw(" "),
                Span::styled(COMPUTER, dim),
                Span::raw(" "),
                Span::styled(said, dim),
                Span::raw(" ".repeat(gap)),
                Span::styled(age, dim),
            ]);
            let cuts = [
                cut_at(0, MACHINE_LEAD, &text, named),
                cut_at(0, MACHINE_LEAD + cells(&shown), &word, worded),
            ];
            let cuts = cuts.into_iter().flatten().collect();
            RowLines {
                lines: vec![line],
                number: None,
                cuts,
            }
        }
        Row::Machine {
            name,
            away,
            heads:
                Heads::Projects {
                    folded,
                    here,
                    waiting,
                    dot,
                    busy,
                },
            ..
        } => {
            let badge = badge(*waiting, *folded && *dot, *folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let (word, since) = away.as_ref().map(Away::said).unzip();
            let word = word.map_or_default(|word| format!(" · {word}"));
            let age = since.flatten().map_or_default(|since| age(since, now));
            let aged = if age.is_empty() { 0 } else { age.width() + 1 };
            let room = width.saturating_sub(MACHINE_LEAD + 1 + aged + reserve + 1);
            let cuts = cut_at(0, MACHINE_LEAD, name, room).into_iter().collect();
            let shown = machine_label(name, &cut(name, room));
            let word = cut(&word, room.saturating_sub(cells(&shown)));
            let said = cells(&shown) + cells(&word);
            assert!(said <= room, "a machine's name and word fit their room");
            let gap = room - said;
            let colour = if *here && *folded {
                ACCENT
            } else {
                moonlight::MUTE
            };
            let label = Style::new().fg(colour).add_modifier(Modifier::BOLD);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if *folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
                Span::styled(COMPUTER, label),
                Span::raw(" "),
                Span::styled(shown, label),
                Span::styled(word, Style::new().fg(ERROR)),
                Span::raw(" "),
                Span::raw(" ".repeat(gap)),
            ];
            if !age.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(age, dim));
            }
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines {
                lines: vec![Line::from(spans)],
                number: None,
                cuts,
            }
        }
        Row::Project {
            project,
            hits,
            under,
            here,
            colour,
            waiting,
            dot,
            busy,
            ..
        } => {
            let session = project.listed();
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let fixed = PROJECT + reserve + 1;
            let room = width.saturating_sub(fixed);
            let bold = Style::new().fg(*colour).add_modifier(Modifier::BOLD);
            let (name, named) = lit(session.name.as_str(), room, Keep::Start, hits, bold);
            let cuts = cut_at(0, PROJECT, session.name.as_str(), room)
                .into_iter()
                .collect();
            let gap = room.saturating_sub(named);
            let glyph = if folded { FOLDED } else { PROJECT_DOT };
            let glyph_style = if *here && folded {
                Style::new().fg(ACCENT)
            } else {
                bold
            };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(glyph, glyph_style),
                Span::raw(" "),
            ];
            spans.extend(name);
            spans.push(Span::raw(" ".repeat(gap)));
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines {
                lines: vec![Line::from(spans)],
                number: None,
                cuts,
            }
        }
        Row::Worktree {
            named,
            missing,
            hits,
            under,
            here,
            colour,
            waiting,
            dot,
            busy,
            ..
        } => {
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let gone = if *missing { MISSING } else { "" };
            let name = named.name();
            let line_room = width.saturating_sub(WORKTREE + gone.width() + reserve + 1);
            let beside = match named {
                Named::Branch(checkout) => Some(Beside::fitting(checkout, name, line_room)),
                Named::Unbranched(_) | Named::Apart { .. } => None,
            };
            let held = beside.as_ref().map_or(0, Beside::width);
            let status = beside.as_ref().map_or("", |beside| beside.status.as_str());
            let room = line_room.saturating_sub(beside.as_ref().map_or(0, Beside::taken));
            let style = if *here {
                Style::new()
                    .fg(moonlight::TEXT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(moonlight::TEXT)
            };
            let (shown, wide) = lit(name, room, Keep::Start, hits, style);
            let mut cuts: Vec<CutAt> = cut_at(0, WORKTREE, name, room).into_iter().collect();
            let taken = WORKTREE + wide + gone.width() + held + status.width() + reserve + 1;
            let gap = width.saturating_sub(taken);
            let mut spans = vec![
                margin(*here),
                Span::raw(" "),
                Span::styled(
                    if folded { FOLDED } else { UNFOLDED },
                    Style::new().fg(*colour),
                ),
                Span::raw(" "),
            ];
            spans.extend(shown);
            spans.push(Span::styled(gone, Style::new().fg(ERROR)));
            let mut number = None;
            if let Some(beside) = beside {
                let start: usize = spans.iter().map(Span::width).sum();
                number = beside.number_at(0, start);
                spans.extend(beside.spans);
                spans.push(Span::raw(" ".repeat(gap)));
                spans.push(Span::styled(
                    beside.status,
                    Style::new().fg(moonlight::PEACH),
                ));
            } else {
                spans.push(Span::raw(" ".repeat(gap)));
            }
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            let mut lines = vec![Line::from(spans)];
            if let Named::Apart { name, checkout } = named {
                assert_ne!(
                    name,
                    checkout.branch.as_str(),
                    "a name apart is not its branch"
                );
                let at = lines.len();
                let (drawn, numbered, branch_room) = checkout_line(*here, checkout, at, width);
                lines.push(drawn);
                cuts.extend(cut_at(at, LABEL, checkout.branch.as_str(), branch_room));
                number = numbered;
            }
            RowLines {
                lines,
                number,
                cuts,
            }
        }
        Row::View {
            here,
            unread,
            marked,
            icon,
            label,
            hits,
            ..
        } => {
            let mark = marked.as_ref().map(|marked| &marked.mark);
            let since = marked.as_ref().and_then(|marked| marked.since);
            let age = since.map_or_default(|since| age(since, now));
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            let label_style = match mark {
                Some(Mark::Waiting) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(_) => Style::new(),
                None => dim,
            };
            let mut spans = vec![margin(*here), Span::raw(INDENT)];
            let dotted = dotted(*unread, marked.as_ref());
            spans.push(if dotted { dot() } else { Span::raw(" ") });
            match mark {
                Some(mark) => {
                    let (glyph, style) = mark.glyph(frame);
                    spans.push(Span::styled(glyph, style));
                }
                None => spans.push(Span::raw(" ".repeat(MARK))),
            }
            spans.push(Span::raw(" "));
            match icon {
                Some(icon) => spans.push(Span::styled(*icon, label_style)),
                None => spans.push(Span::raw(" ".repeat(icons::WIDTH))),
            }
            spans.push(Span::raw(" "));
            let room = width.saturating_sub(LABEL + reserve + 1);
            let (shown, wide) = lit(label, room, Keep::Start, hits, label_style);
            let cuts: Vec<CutAt> = cut_at(0, LABEL, label, room).into_iter().collect();
            let gap = width.saturating_sub(LABEL + wide + age.width() + 1);
            spans.extend(shown);
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(age, dim));
            RowLines {
                lines: vec![Line::from(spans)],
                number: None,
                cuts,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    Start,
    End,
}

pub fn lit(
    text: &str,
    room: usize,
    keep: Keep,
    hits: &[usize],
    style: Style,
) -> (Vec<Span<'static>>, usize) {
    let shown = match keep {
        Keep::Start => cut(text, room),
        Keep::End => tail(text, room),
    };
    let hit = if style.fg == Some(ACCENT) || style.bg.is_some() {
        style.add_modifier(Modifier::UNDERLINED)
    } else {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    };
    let (whole, count, all) = (shown == text, shown.chars().count(), text.chars().count());
    let place = |at: usize| match keep {
        _ if whole => Some(at),
        Keep::Start => (at + 1 < count).then_some(at),
        Keep::End => (at > 0).then(|| all - count + at),
    };
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (at, c) in shown.chars().enumerate() {
        let found = place(at).is_some_and(|place| hits.contains(&place));
        let style = if found { hit } else { style };
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    (spans, cells(&shown))
}

fn badge(waiting: usize, dotted: bool, spinning: bool, frame: usize) -> Option<Span<'static>> {
    if waiting > 0 {
        let (dot, style) = Mark::Waiting.glyph(frame);
        return Some(Span::styled(format!("{dot}{waiting}"), style));
    }
    if dotted {
        return Some(dot());
    }
    if spinning {
        let (spinner, style) = Mark::Busy.glyph(frame);
        return Some(Span::styled(spinner, style));
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Colour {
    Blue,
    Yellow,
    Purple,
    Aqua,
    Orange,
    Green,
}

impl Colour {
    const ALL: [Colour; 6] = [
        Colour::Blue,
        Colour::Yellow,
        Colour::Purple,
        Colour::Aqua,
        Colour::Orange,
        Colour::Green,
    ];
}

pub(crate) fn colour_of(project: ProjectId) -> Color {
    let count = u64::try_from(Colour::ALL.len()).expect("six colours");
    let at = usize::try_from(project.get() % count).expect("an index below six");
    hue(Colour::ALL[at])
}

fn hue(colour: Colour) -> Color {
    match colour {
        Colour::Blue => moonlight::BLUE,
        Colour::Yellow => moonlight::YELLOW,
        Colour::Purple => moonlight::MAUVE,
        Colour::Aqua => moonlight::CYAN,
        Colour::Orange => moonlight::PEACH,
        Colour::Green => moonlight::GREEN,
    }
}

fn margin(here: bool) -> Span<'static> {
    if here {
        Span::styled(
            HERE_MARK,
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::raw(" ")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keeps {
    All,
    NumberAndStatus,
    Number,
    Nothing,
}

impl Keeps {
    const WIDEST_FIRST: [Keeps; 4] = [
        Keeps::All,
        Keeps::NumberAndStatus,
        Keeps::Number,
        Keeps::Nothing,
    ];

    fn changed(self) -> bool {
        match self {
            Keeps::All => true,
            Keeps::NumberAndStatus | Keeps::Number | Keeps::Nothing => false,
        }
    }

    fn number(self) -> bool {
        match self {
            Keeps::All | Keeps::NumberAndStatus | Keeps::Number => true,
            Keeps::Nothing => false,
        }
    }

    fn status(self) -> bool {
        match self {
            Keeps::All | Keeps::NumberAndStatus => true,
            Keeps::Number | Keeps::Nothing => false,
        }
    }
}

struct Beside<'a> {
    spans: Vec<Span<'static>>,
    number: Option<(Range<usize>, &'a Link)>,
    status: String,
}

impl<'a> Beside<'a> {
    fn of(checkout: &'a Checkout, keeps: Keeps) -> Beside<'a> {
        let mut spans = Vec::new();
        if let Some(changed) = checkout.changed.filter(|_| keeps.changed()) {
            spans.push(Span::raw(" "));
            spans.extend(changed_shown(changed).0);
        }
        let mut number = None;
        if let Some(numbered) = checkout.pull_request.as_ref().filter(|_| keeps.number()) {
            spans.push(Span::raw(" "));
            let start: usize = spans.iter().map(Span::width).sum();
            let shown = Span::styled(numbered.shown(), Style::new().fg(numbered.colour()));
            let columns = start..start + shown.width();
            number = numbered.link().map(|link| (columns, link));
            spans.push(shown);
            if let NumberedState::Open {
                checks,
                conflicting,
                stale,
                ..
            } = numbered.state
            {
                spans.extend(checks.map(|checks| verdict_mark(checks, stale)));
                if conflicting {
                    spans.push(conflict_mark(stale));
                }
            }
        }
        let status = if keeps.status() {
            checkout.status.clone()
        } else {
            String::new()
        };
        Beside {
            spans,
            number,
            status,
        }
    }

    fn fitting(checkout: &'a Checkout, name: &str, room: usize) -> Beside<'a> {
        let kept = name.width().min(NAME_KEPT);
        let mut widest_first = Keeps::WIDEST_FIRST.into_iter();
        let keeps = widest_first.find(|keeps| Beside::of(checkout, *keeps).taken() + kept <= room);
        Beside::of(checkout, keeps.unwrap_or(Keeps::Nothing))
    }

    fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    fn taken(&self) -> usize {
        let status = if self.status.is_empty() {
            0
        } else {
            self.status.width() + 1
        };
        self.width() + status
    }

    fn number_at(&self, at: usize, start: usize) -> Option<NumberAt<'a>> {
        let (columns, link) = self.number.as_ref()?;
        let columns = start + columns.start..start + columns.end;
        Some(NumberAt {
            line: at,
            columns,
            link,
        })
    }
}

fn checkout_line(
    here: bool,
    checkout: &Checkout,
    at: usize,
    width: usize,
) -> (Line<'static>, Option<NumberAt<'_>>, usize) {
    let beside = Beside::of(checkout, Keeps::All);
    let held = beside.width();
    let status = &checkout.status;
    let room = width.saturating_sub(LABEL + beside.taken() + 1);
    let branch = cut(checkout.branch.as_str(), room);
    let gap = width.saturating_sub(LABEL + cells(&branch) + held + status.width() + 1);
    let mut spans = vec![
        margin(here),
        Span::raw(" ".repeat(LABEL - 1)),
        Span::styled(branch, Style::new().fg(moonlight::MUTE)),
    ];
    let start: usize = spans.iter().map(Span::width).sum();
    let number = beside.number_at(at, start);
    spans.extend(beside.spans);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::styled(
        beside.status,
        Style::new().fg(moonlight::PEACH),
    ));
    (Line::from(spans), number, room)
}

pub fn cut(text: &str, room: usize) -> String {
    if cells(text) <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut taken = 0;
    for shown in text.graphemes(true) {
        let width = cells(shown);
        if taken + width + 1 > room {
            break;
        }
        kept.push_str(shown);
        taken += width;
    }
    if room > 0 {
        kept.push('…');
    }
    kept
}

pub fn tail(text: &str, room: usize) -> String {
    if cells(text) <= room {
        return text.to_owned();
    }
    let mut start = text.len();
    let mut taken = 0;
    for (at, shown) in text.grapheme_indices(true).rev() {
        let width = cells(shown);
        if taken + width + 1 > room {
            break;
        }
        start = at;
        taken += width;
    }
    if room == 0 {
        return String::new();
    }
    format!("…{}", &text[start..])
}

#[derive(Debug, Default)]
struct Notes {
    lines: Vec<Line<'static>>,
    goes_on: Vec<usize>,
    said: Vec<String>,
    spinners: Vec<usize>,
}

impl Notes {
    fn say(&mut self, text: &str, width: usize, style: Style) {
        let lines = wrapped(text, width, style);
        let start = self.lines.len();
        self.goes_on.extend(start + 1..start + lines.len());
        self.lines.extend(lines);
        self.said.push(text.to_owned());
    }

    fn wait(&mut self, text: &str, width: usize, style: Style, frame: usize) {
        let start = self.lines.len();
        self.lines.extend(menu::waiting(text, width, style, frame));
        if start < self.lines.len() {
            self.spinners.push(start);
        }
    }
}

fn note_room(width: usize) -> usize {
    width.saturating_sub(2).max(1)
}

pub fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let room = note_room(width);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut taken = 0;
    for shown in text.graphemes(true) {
        let width = cells(shown);
        if taken + width > room {
            lines.push(Line::styled(format!(" {line}"), style));
            line.clear();
            taken = 0;
        }
        line.push_str(shown);
        taken += width;
    }
    if !line.is_empty() {
        lines.push(Line::styled(format!(" {line}"), style));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash as _, Hasher as _};
    use std::sync::LazyLock;

    use lune_protocol::github::PullPage;
    use lune_protocol::said::Said;
    use luned::api::shell::{Doing, Ended, Going, Stopped, ended};
    use luned::api::{self, Front, Harness, NameSource, Shell, UnreadFile, Version};
    use prost_types::Timestamp;
    use ratatui::buffer::Cell;

    use super::*;
    use crate::pull::{At, Readied, Ready};
    use crate::spots::Hit;
    use luned::rebases;

    impl std::ops::Deref for Sidebar {
        type Target = SidebarState;

        fn deref(&self) -> &SidebarState {
            self.state()
        }
    }

    impl Sidebar {
        fn light(&mut self, spot: Spot) {
            self.edit(|state| state.light(spot));
        }

        fn step(&mut self, by: isize) {
            self.edit(|state| state.step(by));
        }

        fn end(&mut self, last: bool) {
            self.edit(|state| state.end(last));
        }

        fn rebuild(&mut self, wall: SystemTime) {
            self.edit(|state| state.rebuild(wall));
        }

        fn place_rows(&mut self, area: Rect) {
            self.edit_without_layout(|state| state.place_rows(area));
        }

        fn collapse_rows(&mut self, area: Rect) {
            self.edit_without_layout(|state| state.collapse_rows(area));
        }

        fn mouse_rows(&mut self, mouse: MouseEvent, area: Rect, wall: SystemTime) -> Asked {
            self.edit(|state| {
                state.place_rows(area);
                state.mouse_in(mouse, area, wall)
            })
        }

        fn draw_rows(&self, buffer: &mut Buffer, area: Rect, wall: SystemTime) -> Drawn {
            self.state().draw_rows(buffer, area, wall)
        }

        fn draw_collapsed_rows(&self, buffer: &mut Buffer, area: Rect) {
            self.state().draw_collapsed_rows(buffer, area);
        }

        fn set_top(&mut self, top: usize) {
            self.edit(|state| state.set_top(top));
        }
    }

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    #[derive(Clone, Debug)]
    struct View {
        id: u32,
        tab: u32,
        tab_name: String,
        cwd: String,
        command: Option<String>,
        titled: Option<api::Titled>,
        shell: Option<Shell>,
        held: bool,
    }

    fn view(id: u32, tab: u32, cwd: &str, command: &str) -> View {
        View {
            id,
            tab,
            tab_name: format!("Tab #{}", tab + 1),
            cwd: cwd.to_owned(),
            command: Some(command.to_owned()),
            titled: None,
            shell: None,
            held: false,
        }
    }

    #[derive(Clone, Debug)]
    struct Session {
        name: String,
        views: Vec<View>,
        id: u64,
        made: Timestamp,
    }

    fn session(name: &str, started: i64, views: Vec<View>) -> Session {
        let id = u64::try_from(started).unwrap();
        made(
            name,
            id,
            Timestamp {
                seconds: started,
                nanos: 0,
            },
            views,
        )
    }

    fn own_session(name: &str, id: u64, views: Vec<View>) -> Session {
        made(name, id, Timestamp::default(), views)
    }

    fn known(view: &View) -> KnownView {
        KnownView {
            id: ViewId::of(u64::from(view.id)).unwrap(),
            key: TerminalKey::of(&key_in(1, view.id)).unwrap(),
            cwd: Dir::parse(&view.cwd).unwrap(),
            command: view.command.clone(),
            topic: view.titled.as_ref().and_then(|titled| titled.topic.clone()),
            told: Told::of(view.shell.as_ref()),
            ended: view.held,
        }
    }

    fn made(name: &str, id: u64, made: Timestamp, views: Vec<View>) -> Session {
        Session {
            name: name.to_owned(),
            views,
            id,
            made,
        }
    }

    fn key_in(project: u64, view: u32) -> api::TerminalKey {
        let made_at = Some(Timestamp {
            seconds: i64::try_from(project).unwrap(),
            nanos: 0,
        });
        api::TerminalKey { id: view, made_at }
    }

    fn view_key(project: u64, view: u32) -> ViewKey {
        ViewKey::Terminal(TerminalKey::of(&key_in(project, view)).unwrap())
    }

    fn named(session: &str) -> i32 {
        let mut hasher = DefaultHasher::new();
        session.hash(&mut hasher);
        i32::try_from(hasher.finish() % 1_000_000_000).unwrap()
    }

    fn in_named(session: &str, view: u32) -> api::TerminalKey {
        let made_at = Some(Timestamp {
            seconds: -1,
            nanos: named(session),
        });
        api::TerminalKey { id: view, made_at }
    }

    fn in_own(agent: Agent, id: u64, view: u32) -> Agent {
        Agent {
            terminal: Some(key_in(id, view)),
            ..agent
        }
    }

    fn listed(name: &str, started: u64) -> Listed {
        Listed::new_project(name, started)
    }

    fn view_of(name: &str, started: u64, view: u32) -> Place {
        Place::View(view_place(name, started, 0, view))
    }

    fn tab_view_of(name: &str, started: u64, tab: u32, view: u32) -> Place {
        Place::View(view_place(name, started, tab, view))
    }

    fn view_place(name: &str, project: u64, tab: u32, view: u32) -> ViewPlace {
        let worktree = WorktreePlace {
            project: project_row(name, project),
            worktree: tab_of(project, tab),
        };
        ViewPlace::new(worktree, view_key(project, view))
    }

    fn own_listed(name: &str, id: u64) -> Listed {
        Listed::new_project(name, id)
    }

    fn own_view_of(name: &str, id: u64, view: u32) -> Place {
        view_of(name, id, view)
    }

    fn session_name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    fn close_here(act: Act, ticket: u64) -> Asked {
        Asked::Close {
            on: Machine::Local,
            act,
            ticket,
        }
    }

    fn project_closed(name: &str, id: u64, worktrees: &[u64], seen: &[u64]) -> Act {
        let (project, worktrees) = (ProjectId::of(id).unwrap(), worktrees.iter());
        let worktrees = worktrees.map(|it| WorktreeId::of(*it).unwrap()).collect();
        let seen = seen.iter().map(|it| ViewId::of(*it).unwrap()).collect();
        Act::Project {
            session: session_name(name),
            project,
            worktrees,
            seen,
        }
    }

    fn go_here(place: Place) -> Asked {
        Asked::Go {
            on: Machine::Local,
            place,
        }
    }

    fn test_wall() -> SystemTime {
        moment(160_000).system
    }

    fn fresh() -> Sidebar {
        Sidebar::new(
            Folds::default(),
            News::default(),
            Kept::default(),
            "mac".to_owned(),
            Rect::new(0, 0, COLUMNS, 24),
        )
    }

    fn lit_place(sidebar: &Sidebar) -> Option<Place> {
        match sidebar.lit_spot()? {
            Spot::Local(place) => Some(place.clone()),
            Spot::Machine(_) | Spot::Remote(..) => None,
        }
    }

    fn step_onto_its_first_view(sidebar: &mut Sidebar) {
        let project = lit_place(sidebar).expect("the highlight is on a row of this machine");
        assert!(
            matches!(project, Place::Session(_)),
            "it starts on a project's row: {project:?}"
        );
        sidebar.step(1);
        let view = lit_place(sidebar).expect("a step keeps the highlight on this machine");
        assert!(
            matches!(view, Place::View(_)),
            "one step down lands on a view: {view:?}"
        );
        assert_eq!(
            view.listed(),
            project.listed(),
            "the view is the project's own"
        );
    }

    fn folded(sidebar: &Sidebar, name: &str) -> bool {
        sidebar
            .folds
            .contains(Fold::Project(&Machine::Local, project_named(sidebar, name)))
    }

    fn fold(sidebar: &mut Sidebar, name: &str) {
        let project = project_named(sidebar, name);
        sidebar.edit(|state| {
            let _ = state
                .folds
                .set(Fold::Project(&Machine::Local, project), true);
        });
    }

    fn project_row(name: &str, id: u64) -> ProjectRow {
        ProjectRow::of(listed(name, id))
    }

    fn projects(seen: &[u64], wanted: &[u64]) -> Reorder {
        let ids = |ids: &[u64]| ids.iter().map(|id| ProjectId::of(*id).unwrap()).collect();
        Reorder::Projects {
            seen: ids(seen),
            wanted: ids(wanted),
        }
    }

    fn project_named(sidebar: &Sidebar, name: &str) -> ProjectId {
        let mut known = shelf(&sidebar.shelves, &Machine::Local).iter();
        let known = known
            .find(|known| known.listed().name.as_str() == name)
            .expect("a project");
        known.listed().project()
    }

    fn agent(session: &str, view: u32, status: Status, name: Option<&str>) -> Agent {
        Agent {
            pid: view,
            harness: Some(Harness::ClaudeCode.into()),
            session_id: Some(format!("id-{view}")),
            cwd: "/w/lune/main".to_owned(),
            status: Some(Reported::KnownStatus(status.into())),
            status_since: Some(Timestamp {
                seconds: 100,
                nanos: 0,
            }),
            started_at: Some(Timestamp {
                seconds: 50,
                nanos: 0,
            }),
            name: name.map(|text| api::Name {
                text: text.to_owned(),
                source: None,
            }),
            entrypoint: Some("cli".to_owned()),
            terminal: Some(in_named(session, view)),
            waiting_for: None,
            topic: None,
            ..Agent::default()
        }
    }

    trait Hears {
        fn hear(&mut self, host: Host);
    }

    impl Hears for Sidebar {
        fn hear(&mut self, mut host: Host) {
            let projects = host
                .projects
                .get_or_insert_with(|| projects_of(Vec::new()))
                .clone();
            seat(&projects, &mut host.agents, &mut host.unread_files);
            for remote in &mut host.remotes {
                if let Some(Reached::Host(far)) = &mut remote.reached {
                    let projects = far
                        .projects
                        .get_or_insert_with(|| projects_of(Vec::new()))
                        .clone();
                    seat(&projects, &mut far.agents, &mut far.unread_files);
                }
            }
            self.heard(host, test_wall());
        }
    }

    fn tab_of(project: u64, tab: u32) -> WorktreeId {
        WorktreeId::of(project * 1000 + u64::from(tab)).unwrap()
    }

    fn placed(projects: &api::Projects, key: &mut api::TerminalKey) {
        let Some(asked) = key.made_at.filter(|it| it.seconds == -1) else {
            return;
        };
        let Some(api::projects::Held::Kept(kept)) = &projects.held else {
            return;
        };
        if let Some(project) = kept
            .projects
            .iter()
            .find(|it| named(&it.name) == asked.nanos)
        {
            *key = key_in(project.id, key.id);
        }
    }

    fn seat(projects: &api::Projects, agents: &mut [Agent], files: &mut [UnreadFile]) {
        let in_agents = agents
            .iter_mut()
            .filter_map(|agent| agent.terminal.as_mut());
        let in_files = files.iter_mut().filter_map(|file| file.terminal.as_mut());
        for key in in_agents.chain(in_files) {
            placed(projects, key);
        }
    }

    fn projects_of(mut sessions: Vec<Session>) -> api::Projects {
        sessions.sort_by_key(|session| (session.made.seconds, session.name.clone()));
        let project = |session: &Session| {
            let mut worktrees: Vec<api::Worktree> = Vec::new();
            let mut views: Vec<&View> = session.views.iter().collect();
            views.sort_by_key(|view| (view.tab, view.id));
            for view in views {
                let terminal = api::Terminal {
                    id: u64::from(view.id),
                    found: Some(api::terminal::Found::Key(key_in(session.id, view.id))),
                    folder: view.cwd.clone(),
                    command: view.command.clone(),
                    titled: view.titled.clone(),
                    shell: view.shell.clone(),
                    held: view.held,
                    name: None,
                    state: None,
                };
                let id = tab_of(session.id, view.tab).get();
                match worktrees.iter_mut().find(|it| it.id == id) {
                    Some(worktree) => worktree.terminals.push(terminal),
                    None => worktrees.push(api::Worktree {
                        id,
                        name: view.tab_name.clone(),
                        folder: view.cwd.clone(),
                        seen: Some(api::worktree::Seen::Unread(())),
                        terminals: vec![terminal],
                        arranged: Some(api::Arrangement::Tabs.into()),
                        ..api::Worktree::default()
                    }),
                }
            }
            api::Project {
                id: session.id,
                name: session.name.clone(),
                dir: "/w".to_owned(),
                missing: false,
                holds: Some(api::project::Holds::Clone(clone_of(worktrees))),
            }
        };
        let projects = sessions.iter().map(project).collect();
        let kept = api::projects::Kept {
            projects,
            version: None,
        };
        api::Projects {
            held: Some(api::projects::Held::Kept(kept)),
        }
    }

    fn host(sessions: Vec<Session>, agents: Vec<Agent>) -> Host {
        Host {
            version: Some(Version { pid: 1, seq: 1 }),
            git_statuses: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_files: Vec::new(),
            remotes: Vec::new(),
            unread_hosts: Some(0),
            sayings: Vec::new(),
            projects: Some(projects_of(sessions)),
        }
    }

    fn drawn(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Buffer {
        let now = moment(160_000);
        drawn_at(sidebar, focused, width, height, now)
    }

    fn drawn_at(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
        now: Moment,
    ) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        sidebar.focus(
            if focused {
                Keys::Sidebar
            } else {
                Keys::Terminal
            },
            now.system,
        );
        sidebar.draw_rows(&mut buffer, area, now.system);
        let lit: &[Color] = if focused { &[LIT, RESTING] } else { &[RESTING] };
        in_moonlight(&buffer, &[&[Color::Reset], lit].concat());
        buffer
    }

    fn collapsed(sidebar: &mut Sidebar, height: u16, _now: Moment) -> Buffer {
        let area = Rect::new(0, 0, COLLAPSED_ROWS, height);
        let mut buffer = Buffer::empty(area);
        sidebar.collapse_rows(area);
        sidebar.draw_collapsed_rows(&mut buffer, area);
        in_moonlight(&buffer, &[Color::Reset]);
        buffer
    }

    fn in_moonlight(buffer: &Buffer, backgrounds: &[Color]) {
        for cell in buffer.content() {
            assert!(
                moonlight::ALL.contains(&cell.fg),
                "{:?} in {:?}",
                cell.symbol(),
                cell.fg
            );
            let named = Colour::ALL.map(hue).contains(&cell.bg) && cell.fg == moonlight::INK;
            let on = backgrounds.contains(&cell.bg) || named;
            assert!(on, "{:?} on {:?}", cell.symbol(), cell.bg);
        }
    }

    fn screen(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Vec<String> {
        seen(sidebar, focused, width, height).0
    }

    fn seen(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
    ) -> (Vec<String>, Vec<u16>) {
        let now = moment(160_000);
        seen_at(sidebar, focused, width, height, now)
    }

    fn seen_at(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
        now: Moment,
    ) -> (Vec<String>, Vec<u16>) {
        let buffer = drawn_at(sidebar, focused, width, height, now);
        let lines = (0..height)
            .map(|y| {
                let line: String = (0..width - 1).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_owned()
            })
            .collect();
        let lit = (0..height)
            .filter(|y| buffer[(0, *y)].bg != Color::Reset)
            .collect();
        (lines, lit)
    }

    fn two_sessions() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(with_nvim());
        sidebar
    }

    fn with_nvim() -> Host {
        host(
            vec![
                session("newer", 20, vec![view(1, 0, "/w/b", "fish")]),
                session(
                    "older",
                    10,
                    vec![
                        view(2, 1, "/w/c", "nvim src/main.rs"),
                        view(1, 0, "/w/lune/main", "claude"),
                    ],
                ),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        )
    }

    fn own_two_sessions() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![
                own_session("young", 2, vec![view(1, 0, "/w/b", "fish")]),
                own_session(
                    "older",
                    1,
                    vec![
                        view(2, 1, "/w/c", "nvim src/main.rs"),
                        view(1, 0, "/w/lune/main", "claude"),
                    ],
                ),
            ],
            vec![in_own(
                agent("older", 1, Status::Waiting, Some("fold")),
                1,
                1,
            )],
        ));
        sidebar
    }

    fn owned(sessions: Vec<Session>) -> Vec<Session> {
        let each = sessions.into_iter().zip(1..);
        each.map(|(session, id)| own_session(&session.name, id, session.views))
            .collect()
    }

    fn floating_menu(sidebar: &Sidebar) -> Option<&Menu> {
        match sidebar.floating() {
            Some(Floating::Menu(menu)) => Some(menu),
            Some(Floating::Note(_)) | None => None,
        }
    }

    fn float_a_menu(sidebar: &mut Sidebar) {
        sidebar.float_menu();
        assert!(floating_menu(sidebar).is_some(), "the menu floats");
    }

    fn sit(sidebar: &mut Sidebar, place: &Place) {
        let Place::View(ViewPlace { worktree, view }) = place else {
            panic!("lune sits on a view: {place:?}");
        };
        let (project, terminal) = (worktree.project.id(), view.key());
        let at = Sitting::Worktree {
            worktree: worktree.worktree,
            layout: SeatedLayout::Disconnected(terminal),
        };
        sidebar.sit(
            Seat {
                on: Machine::Local,
                project,
                at,
            },
            test_wall(),
        );
    }

    fn seat_in(project: u64, view: Option<u32>) -> Seat {
        let layout = view.map_or(SeatedLayout::Unlaid, |view| {
            SeatedLayout::Disconnected(view_key(project, view).key())
        });
        let (worktree, project) = (tab_of(project, 0), ProjectId::of(project).unwrap());
        let at = Sitting::Worktree { worktree, layout };
        Seat {
            on: Machine::Local,
            project,
            at,
        }
    }

    fn kept(projects: Vec<api::Project>) -> api::Projects {
        let kept = api::projects::Kept {
            projects,
            version: None,
        };
        api::Projects {
            held: Some(api::projects::Held::Kept(kept)),
        }
    }

    fn worktree_of(id: u64, name: &str, folder: &str, views: &[u32]) -> api::Worktree {
        let made_at = Some(Timestamp::default());
        let terminal = |id: &u32| api::Terminal {
            id: u64::from(*id),
            found: Some(api::terminal::Found::Key(api::TerminalKey {
                id: *id,
                made_at,
            })),
            folder: folder.to_owned(),
            command: Some("fish".to_owned()),
            ..api::Terminal::default()
        };
        api::Worktree {
            id,
            name: name.to_owned(),
            folder: folder.to_owned(),
            seen: Some(api::worktree::Seen::Unread(())),
            terminals: views.iter().map(terminal).collect(),
            arranged: (!views.is_empty()).then_some(api::Arrangement::Tabs.into()),
            ..api::Worktree::default()
        }
    }

    fn clone_of(worktrees: Vec<api::Worktree>) -> api::project::Clone {
        api::project::Clone {
            worktrees,
            listing: None,
        }
    }

    fn a_project(id: u64, name: &str, worktrees: Vec<api::Worktree>) -> api::Project {
        let (name, dir) = (name.to_owned(), "/w".to_owned());
        let holds = Some(api::project::Holds::Clone(clone_of(worktrees)));
        api::Project {
            id,
            name,
            dir,
            missing: false,
            holds,
        }
    }

    #[test]
    fn an_empty_project_is_one_row_and_an_empty_worktree_is_one_to_go_to() {
        let mut sidebar = fresh();
        let fresh_one = worktree_of(31, "fresh", "/w/solo", &[]);
        let projects = vec![
            a_project(7, "empty", Vec::new()),
            a_project(8, "solo", vec![fresh_one]),
        ];
        sidebar.heard(
            Host {
                projects: Some(kept(projects)),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        assert_eq!(
            rows_shown(&mut sidebar),
            [" ● empty", "", " ● solo", "  ▾ fresh"]
        );
        sidebar.focus(Keys::Sidebar, test_wall());
        let empty = Place::Session(listed("empty", 7));
        assert_eq!(lit_place(&sidebar), Some(empty.clone()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(empty));
        type_in(&mut sidebar, "jj");
        let worktree = WorktreeId::of(31).unwrap();
        let solo = project_row("solo", 8);
        let place = Place::Worktree(WorktreePlace {
            project: solo,
            worktree,
        });
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(place));
    }

    #[test]
    fn space_or_a_click_folds_an_empty_row_itself_and_h_folds_what_holds_it() {
        let mut sidebar = fresh();
        let solo = a_project(8, "solo", vec![worktree_of(31, "fresh", "/w/solo", &[])]);
        let projects = vec![a_project(7, "empty", Vec::new()), solo];
        sidebar.heard(
            Host {
                projects: Some(kept(projects)),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        sidebar.focus(Keys::Sidebar, test_wall());
        let (empty, tab) = (ProjectId::of(7).unwrap(), WorktreeId::of(31).unwrap());
        let folded = |sidebar: &Sidebar| {
            let on = &Machine::Local;
            [
                Fold::Machine(on),
                Fold::Project(on, empty),
                Fold::Worktree(on, tab),
            ]
            .map(|fold| sidebar.folds.contains(fold))
        };
        type_in(&mut sidebar, " ");
        assert_eq!(folded(&sidebar), [false, true, false]);
        type_in(&mut sidebar, " jj ");
        assert_eq!(folded(&sidebar), [false, false, true]);
        let area = Rect::new(0, 0, 32, 10);
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert_eq!(folded(&sidebar), [false, true, true]);
        type_in(&mut sidebar, "h");
        assert_eq!(folded(&sidebar), [true, true, true]);
    }

    #[test]
    fn a_click_on_a_worktree_with_views_goes_to_it_and_on_its_triangle_folds_it() {
        let mut sidebar = fresh();
        let fix = worktree_of(31, "fix", "/w/fix", &[1, 2]);
        let main = worktree_of(32, "main", "/w/main", &[3]);
        let projects = vec![a_project(7, "lune", vec![fix, main])];
        sidebar.heard(
            Host {
                projects: Some(kept(projects)),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        let area = Rect::new(0, 0, 32, 12);
        let lines = screen(&mut sidebar, false, area.width, area.height);
        let fix = ["  ▾ fix", "     \u{e795} w/fix", "     \u{e795} w/fix"];
        assert_eq!(
            lines[5..10],
            [&fix[..], &["  ▾ main", "     \u{e795} w/main"]].concat()
        );
        let project = project_row("lune", 7);
        let worktree = |id| {
            let worktree = WorktreeId::of(id).unwrap();
            go_here(Place::Worktree(WorktreePlace {
                project: project.clone(),
                worktree,
            }))
        };
        assert_eq!(click(&mut sidebar, 10, 8, area), worktree(32));
        assert_eq!(click(&mut sidebar, 10, 5, area), worktree(31));
        assert_eq!(click(&mut sidebar, 2, 8, area), Asked::Take);
        let main = Fold::Worktree(&Machine::Local, WorktreeId::of(32).unwrap());
        assert!(sidebar.folds.contains(main));
        screen(&mut sidebar, false, area.width, area.height);
        assert_eq!(click(&mut sidebar, 10, 8, area), worktree(32));
    }

    #[test]
    fn the_blank_line_above_a_project_is_no_row_s() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 12);
        let lines = screen(&mut sidebar, false, area.width, area.height);
        assert_eq!(lines[9..11], ["", " ● newer"]);
        let newer = Fold::Project(&Machine::Local, ProjectId::of(20).unwrap());
        for column in [1, 10] {
            assert_eq!(
                click(&mut sidebar, column, 9, area),
                Asked::Take,
                "{column}"
            );
            assert!(!sidebar.folds.contains(newer), "{column}");
        }
        let down = mouse(MouseEventKind::Down(MouseButton::Left), 10, 9);
        sidebar.mouse_rows(down, area, test_wall());
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 10, 4);
        assert_eq!(
            sidebar.mouse_rows(drag, area, test_wall()),
            Asked::Nothing,
            "the gap lifts no row"
        );
        sidebar.let_go();
        assert_eq!(
            click(&mut sidebar, 10, 10, area),
            go_here(Place::Session(listed("newer", 20)))
        );
    }

    #[test]
    fn a_worktree_s_row_closes_the_worktree_with_its_views() {
        let mut sidebar = fresh();
        let fix = worktree_of(31, "fix", "/w/fix", &[1, 2]);
        let main = worktree_of(32, "main", "/w/main", &[3]);
        let projects = vec![a_project(7, "lune", vec![fix, main])];
        sidebar.heard(
            Host {
                projects: Some(kept(projects)),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        sidebar.focus(Keys::Sidebar, test_wall());
        let worktree = WorktreeId::of(31).unwrap();
        let project = project_row("lune", 7);
        let place = Place::Worktree(WorktreePlace { project, worktree });
        sidebar.light(Spot::Local(place));
        type_in(&mut sidebar, "d");
        let seen = [1, 2].map(|id| ViewId::of(id).unwrap()).to_vec();
        let views = [1, 2].map(|id| ViewKey::terminal(id).key()).to_vec();
        let act = Act::Worktree {
            worktree,
            views,
            seen,
        };
        let asked = sidebar.key(&char('d'), moment(160_000));
        assert_eq!(asked, close_here(act, 1));
        let landing = match &sidebar.lit {
            Some(Lit::Closing { landing, .. }) => landing.clone(),
            Some(Lit::On(_)) | None => None,
        };
        let past = "the highlight lands past the worktree's views";
        let main = WorktreePlace {
            project: project_row("lune", 7),
            worktree: WorktreeId::of(32).unwrap(),
        };
        let next = ViewPlace::new(main, ViewKey::terminal(3));
        assert_eq!(landing, Some(Spot::Local(Place::View(next))), "{past}");
    }

    #[test]
    fn the_fold_of_a_worktree_luned_no_longer_keeps_goes_with_it() {
        let mut sidebar = two_sessions();
        let (kept, gone) = (tab_of(10, 0), tab_of(10, 1));
        for tab in [kept, gone] {
            sidebar.edit(|state| {
                let _ = state.folds.set(Fold::Worktree(&Machine::Local, tab), true);
            });
        }
        let older = session("older", 10, vec![view(1, 0, "/w/lune/main", "claude")]);
        let newer = session("newer", 20, vec![view(1, 0, "/w/b", "fish")]);
        sidebar.hear(host(vec![newer, older], Vec::new()));
        assert!(
            sidebar
                .folds
                .contains(Fold::Worktree(&Machine::Local, kept))
        );
        assert!(
            !sidebar
                .folds
                .contains(Fold::Worktree(&Machine::Local, gone))
        );
        let unkept = api::projects::Held::Unkept("its file does not read".to_owned());
        let projects = Some(api::Projects { held: Some(unkept) });
        sidebar.hear(Host {
            projects,
            ..host(Vec::new(), Vec::new())
        });
        let still = sidebar
            .folds
            .contains(Fold::Worktree(&Machine::Local, kept));
        assert!(still, "nor on a list cut short");
    }

    #[test]
    fn a_worktree_whose_folder_is_gone_says_so() {
        let mut sidebar = fresh();
        let mut gone = worktree_of(31, "fix", "/w/fix", &[1]);
        gone.seen = Some(api::worktree::Seen::Missing(()));
        let main = worktree_of(32, "main", "/w", &[2]);
        let projects = vec![a_project(7, "lune", vec![gone, main])];
        sidebar.heard(
            Host {
                projects: Some(kept(projects)),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        let rows = rows_shown(&mut sidebar);
        assert_eq!(
            [rows[1].as_str(), rows[3].as_str()],
            ["  ▾ fix missing", "  ▾ main"]
        );
        let buffer = drawn(&mut sidebar, false, 32, 10);
        let missing = (0..32).find(|x| buffer[(*x, 5)].symbol() == "m").unwrap();
        assert_eq!(buffer[(missing + 1, 5)].fg, ERROR);
    }

    #[test]
    fn an_unreached_machine_shows_the_projects_luned_last_heard_marked() {
        let mut sidebar = fresh();
        let main = a_project(20, "main", vec![worktree_of(31, "fix", "/w/b", &[1])]);
        let mut unreached = down(100);
        if let Reached::Unreached(it) = &mut unreached {
            it.projects = Some(kept(vec![main]));
        }
        let here = session("here", 10, vec![view(1, 0, "/w/a", "fish")]);
        let mut host = host(vec![here], Vec::new());
        host.remotes = vec![
            remote("nixos", unreached),
            remote("pi", Reached::Reaching(())),
        ];
        sidebar.hear(host);
        let lines = screen(&mut sidebar, false, 32, 13);
        assert_eq!(
            lines[2..13],
            [
                " ▾ \u{f108} MAC",
                "",
                " ● here",
                "  ▾ Tab #1",
                "     \u{e795} w/a",
                " ▾ \u{f108} NIXOS · unreachable    1m",
                "",
                " ● main",
                "  ▾ fix",
                "     \u{e795} w/b",
                " ▸ \u{f108} PI · reaching…",
            ]
        );
        let buffer = drawn(&mut sidebar, false, 32, 13);
        assert_eq!([buffer[(10, 7)].fg, buffer[(13, 7)].fg], [ERROR, ERROR]);
        sidebar.focus(Keys::Sidebar, test_wall());
        let why = "it ended before it reached luned (exit status: 255)";
        let said = format!("nixos: {why}; ssh said \"ssh: Could not resolve hostname vps\"");
        for spot in [
            Spot::Machine(Machine::Remote(nixos())),
            Spot::Remote(nixos(), Place::Session(listed("main", 20))),
        ] {
            sidebar.light(spot);
            assert_eq!(sidebar.unreached(), Some(said.clone()));
        }
        sidebar.light(Spot::Local(Place::Session(listed("here", 10))));
        assert_eq!(sidebar.unreached(), None);
    }

    #[test]
    fn projects_hold_their_worktrees_and_those_their_views() {
        let mut sidebar = two_sessions();
        assert_eq!(
            screen(&mut sidebar, false, 32, 11),
            [
                " ☾ lune  2 projects         ●1",
                "",
                " ▾ \u{f108} MAC                    ●1",
                "",
                " ● older                    ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fold                 1m",
                "  ▾ Tab #2",
                "     \u{e6ae} nvim src/main.rs",
                "",
                " ● newer",
            ]
        );
    }

    #[test]
    fn a_project_s_colour_comes_from_its_id() {
        let chip = |buffer: &Buffer, y: u16| buffer[(3, y)].fg;
        let listing = |named: &[(&str, i64)]| {
            let shared = || vec![view(1, 0, "/w/a", "less"), view(2, 0, "/w/a", "less")];
            let sessions = named.iter().map(|(name, at)| session(name, *at, shared()));
            host(sessions.collect(), Vec::new())
        };
        let mut sidebar = fresh();
        sidebar.hear(listing(&[("a", 1), ("b", 2), ("c", 9)]));
        let buffer = drawn(&mut sidebar, false, 32, 17);
        let (y, p, a) = (moonlight::YELLOW, moonlight::MAUVE, moonlight::CYAN);
        assert_eq!(
            [chip(&buffer, 4), chip(&buffer, 9), chip(&buffer, 14)],
            [y, p, a]
        );
        assert_eq!(buffer[(1, 4)].symbol(), PROJECT_DOT);
        assert_eq!(buffer[(1, 4)].fg, y);

        let mut there = fresh();
        there.hear(listing(&[("e", 4), ("c", 9)]));
        let buffer = drawn(&mut there, false, 32, 17);
        assert_eq!([chip(&buffer, 4), chip(&buffer, 9)], [moonlight::PEACH, a]);
    }

    #[test]
    fn the_title_is_in_the_accent_while_the_sidebar_has_the_keys() {
        let mut sidebar = two_sessions();
        let look = |buffer: &Buffer, x: u16| {
            let cell = &buffer[(x, 0)];
            let has = |modifier| cell.modifier.contains(modifier);
            (cell.fg, has(Modifier::DIM), has(Modifier::BOLD))
        };
        let dim = [
            (moonlight::MUTE, false, false),
            (moonlight::MUTE, false, true),
        ];
        let lit = [(ACCENT, false, false), (moonlight::TEXT, false, true)];
        for (focused, looks) in [(false, dim), (true, lit), (false, dim)] {
            assert_eq!(
                screen(&mut sidebar, focused, 32, 3)[0],
                " ☾ lune  2 projects         ●1"
            );
            let buffer = drawn(&mut sidebar, focused, 32, 3);
            assert_eq!([1, 3].map(|x| look(&buffer, x)), looks, "{focused}");
        }
    }

    #[test]
    fn a_brief_lent_the_keys_dims_the_title_and_keeps_the_highlight() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 12);
        let draw = |sidebar: &mut Sidebar| {
            let mut buffer = Buffer::empty(area);
            sidebar.place_rows(area);
            sidebar.draw_rows(&mut buffer, area, test_wall());
            in_moonlight(&buffer, &[Color::Reset, LIT]);
            let start = |y: u16| (0..7).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            let titled = (0..area.height)
                .filter(|y| matches!(start(*y).as_str(), "─ lune " | " ☾ lune"))
                .count();
            let lit: Vec<u16> = (0..area.height)
                .filter(|y| buffer[(0, *y)].bg != Color::Reset)
                .collect();
            let word = &buffer[(1, 0)];
            (titled, lit, word.fg, word.modifier.contains(Modifier::DIM))
        };
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            moment(160_000),
        );
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            moment(160_000),
        );
        let cursor = sidebar.lit_spot().cloned();
        let (titled, lit, fg, dim) = draw(&mut sidebar);
        assert_eq!((titled, fg, dim, lit.is_empty()), (2, ACCENT, false, false));
        sidebar.focus(Keys::Lent, test_wall());
        assert_eq!(draw(&mut sidebar), (1, lit.clone(), moonlight::MUTE, false));
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(draw(&mut sidebar), (2, lit, ACCENT, false));
        assert_eq!(sidebar.lit_spot().cloned(), cursor);
    }

    #[test]
    fn the_title_counts_the_agents_the_rows_hold() {
        let mut sidebar = fresh();
        let mut outside = agent("s", 4, Status::Waiting, None);
        outside.terminal = None;
        let views = (1..=3).map(|id| view(id, 0, "/w/a", "claude")).collect();
        sidebar.hear(host(
            vec![session("s", 10, views)],
            vec![
                agent("s", 1, Status::Busy, None),
                agent("s", 2, Status::Shell, None),
                agent("s", 3, Status::Shell, None),
                outside,
            ],
        ));
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            moment(160_000),
        );
        let title = screen(&mut sidebar, false, 32, 4)[0].clone();
        assert_eq!(
            (title.as_str(), sidebar.turning()),
            (" ☾ lune  1 project      ⠋1  $2", false)
        );
        let buffer = drawn(&mut sidebar, false, 32, 5);
        assert!(sidebar.turning());
        let at = |glyph: char| {
            title
                .chars()
                .position(|c| c == glyph)
                .expect("on the title") as u16
        };
        let dim = |x: u16| buffer[(x, 0)].modifier.contains(Modifier::DIM);
        let (spinner, dollar) = (at('⠋'), at('$'));
        assert_eq!(
            [spinner, spinner + 1, dollar, dollar + 1].map(dim),
            [false, false, true, true]
        );
    }

    #[test]
    fn collapsed_the_sidebar_keeps_the_dots_the_work_and_the_shells() {
        let mut sidebar = fresh();
        let mut views: Vec<View> = (1..=103).map(|id| view(id, 0, "/w/a", "claude")).collect();
        views.push(in_shell(104, going("make", 159, Front::Working)));
        let mut agents = vec![
            agent("s", 1, Status::Idle, None),
            agent("s", 2, Status::Waiting, None),
        ];
        agents.extend((3..=103).map(|id| agent("s", id, Status::Shell, None)));
        sidebar.hear(host(vec![session("s", 10, views)], agents));
        let strip = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, false);
            let buffer = collapsed(sidebar, 6, now);
            let line = |y| {
                (0..COLLAPSED_ROWS - 1)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            (0..6)
                .map(|y| line(y).trim_end().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip(&mut sidebar, moment(160_500)),
            ["───", "", "•1", "$1…", "", ""]
        );
        assert_eq!(
            strip(&mut sidebar, moment(161_000)),
            ["───", "", "•1", "⠋1", "$1…", ""]
        );
    }

    #[test]
    fn collapsed_the_sidebar_reddens_its_rule_while_luned_does_not_answer() {
        let mut sidebar = fresh();
        sidebar.hear(idle_agent());
        let look = |buffer: &Buffer, x: u16, y: u16| {
            let cell = &buffer[(x, y)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let rule = |sidebar: &mut Sidebar| look(&collapsed(sidebar, 3, moment(160_000)), 0, 0);
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 1, 0);
        assert_eq!(rule(&mut sidebar), open);
        let _ = sidebar.lost("gone".to_owned());
        let buffer = drawn(&mut sidebar, false, COLUMNS, 6);
        assert_eq!(buffer[(1, 5)].symbol(), "l");
        let note = look(&buffer, 1, 5);
        assert_ne!(note, open);
        assert_eq!(rule(&mut sidebar), note);
        sidebar.hear(idle_agent());
        assert_eq!(rule(&mut sidebar), open);
    }

    #[test]
    fn collapsed_the_sidebar_tints_its_rule_while_an_image_goes() {
        let mut sidebar = fresh();
        sidebar.hear(idle_agent());
        let look = |buffer: &Buffer, x: u16, y: u16| {
            let cell = &buffer[(x, y)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let rule = |sidebar: &mut Sidebar| look(&collapsed(sidebar, 3, moment(160_000)), 0, 0);
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 1, 0);
        assert_eq!(rule(&mut sidebar), open);
        let _ = sidebar.image_going(Some("far".parse().unwrap()));
        let buffer = drawn(&mut sidebar, false, COLUMNS, 6);
        assert_eq!(buffer[(1, 5)].symbol(), "i");
        let note = look(&buffer, 1, 5);
        assert_ne!(note, open);
        assert_eq!(rule(&mut sidebar), note);
        let _ = sidebar.lost("gone".to_owned());
        assert_eq!(rule(&mut sidebar).0, ERROR);
        sidebar.hear(idle_agent());
        let _ = sidebar.image_going(None);
        assert_eq!(rule(&mut sidebar), open);
    }

    #[test]
    fn collapsed_the_sidebar_tints_its_rule_while_a_copy_s_note_shows() {
        let mut sidebar = fresh();
        sidebar.hear(idle_agent());
        let now = moment(160_000);
        let rule = |sidebar: &mut Sidebar, at| {
            let buffer = collapsed(sidebar, 3, at);
            let cell = &buffer[(0, 0)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let resting = rule(&mut sidebar, now);
        sidebar.copied(Copied::text(&excerpt("cargo")), now.steady);
        assert_eq!(rule(&mut sidebar, now), (NAME, false));
        let gone = now + COPIED_SHOWS;
        let _ = sidebar.ticked(gone, true);
        assert_eq!(rule(&mut sidebar, gone), resting);
    }

    #[test]
    fn the_sidebar_ends_in_an_edge_in_the_accent_while_it_has_the_keys() {
        let mut sidebar = two_sessions();
        let edge = |buffer: &Buffer| {
            let x = buffer.area.width - 1;
            let look = |y| {
                let cell = &buffer[(x, y)];
                (cell.symbol().to_owned(), cell.fg, cell.bg, cell.modifier)
            };
            (0..buffer.area.height).map(look).collect::<Vec<_>>()
        };
        let accent = ("│".to_owned(), ACCENT, Color::Reset, Modifier::empty());
        let dim = (
            "│".to_owned(),
            moonlight::LIFTED,
            Color::Reset,
            Modifier::empty(),
        );
        let open = drawn(&mut sidebar, true, COLUMNS, 10);
        assert!((0..10).any(|y| open[(0, y)].bg == LIT), "a row is lit");
        assert_eq!(edge(&open), vec![accent; 10]);
        sidebar.focus(Keys::Lent, test_wall());
        let area = Rect::new(0, 0, COLUMNS, 10);
        let mut lent = Buffer::empty(area);
        sidebar.place_rows(area);
        sidebar.draw_rows(&mut lent, area, test_wall());
        assert_eq!(edge(&lent), vec![dim.clone(); 10]);
        assert_eq!(
            edge(&drawn(&mut sidebar, false, COLUMNS, 10)),
            vec![dim.clone(); 10]
        );
        assert_eq!(
            edge(&collapsed(&mut sidebar, 10, moment(160_000))),
            vec![dim; 10]
        );
    }

    #[test]
    fn the_sidebar_is_a_rounded_box_round_its_rows() {
        let mut sidebar = two_sessions();
        let framed = Rect::new(0, 0, COLUMNS, 10);
        let rows = framed_rows(framed);
        assert_eq!(rows, Rect::new(1, 1, COLUMNS - 1, 8));
        assert_eq!(edge(rows), edge(framed));
        let mut buffer = Buffer::empty(framed);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.place(framed);
        let _ = sidebar.draw(&mut buffer, framed, test_wall());
        let (right, bottom) = (COLUMNS - 1, 9);
        let at = |x, y| (buffer[(x, y)].symbol().to_owned(), buffer[(x, y)].fg);
        let accent = |symbol: &str| (symbol.to_owned(), ACCENT);
        assert_eq!(at(0, 0), accent("╭"));
        assert_eq!(at(right, 0), accent("╮"));
        assert_eq!(at(0, bottom), accent("╰"));
        assert_eq!(at(right, bottom), accent("╯"));
        assert!((1..right).all(|x| at(x, 0) == accent("─") && at(x, bottom) == accent("─")));
        assert!((1..bottom).all(|y| at(0, y) == accent("│") && at(right, y) == accent("│")));
        assert_eq!(framed_rows(Rect::new(3, 2, 1, 1)), Rect::new(4, 3, 0, 0));
    }

    fn kept_mut(projects: Option<&mut api::Projects>) -> &mut Vec<api::Project> {
        let held = projects.and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = held else {
            panic!("kept projects");
        };
        &mut kept.projects
    }

    fn rows_mut(project: &mut api::Project) -> &mut Vec<api::Worktree> {
        let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
            panic!("a clone");
        };
        &mut clone.worktrees
    }

    #[test]
    fn a_listing_with_an_id_of_0_shows_none_of_its_projects_and_says_why() {
        type Zero = fn(&mut api::Project);
        let zeroed: [(Zero, &str); 3] = [
            (|project| project.id = 0, "project y has id 0"),
            (
                |project| rows_mut(project)[0].id = 0,
                "worktree Tab #1 of project y has id 0",
            ),
            (
                |project| rows_mut(project)[0].terminals[0].id = 0,
                "a terminal of worktree Tab #1 has id 0",
            ),
        ];
        let listing = || {
            let x = session("x", 10, vec![view(1, 0, "/w/a", "fish")]);
            vec![x, session("y", 20, vec![view(1, 0, "/w/b", "fish")])]
        };
        for (zero, why) in zeroed {
            let mut sidebar = fresh();
            let mut here = host(listing(), Vec::new());
            zero(&mut kept_mut(here.projects.as_mut())[1]);
            let mut nixos = up(listing(), Vec::new());
            let Reached::Host(far) = &mut nixos else {
                panic!("nixos is up");
            };
            zero(&mut kept_mut(far.projects.as_mut())[1]);
            here.remotes = vec![remote("nixos", nixos)];
            sidebar.hear(here);
            let lines = screen(&mut sidebar, false, 80, 9);
            let rows = [
                " ▸ \u{f108} MAC · no projects",
                " ▸ \u{f108} NIXOS · no projects",
            ];
            assert_eq!(lines[2..4], rows);
            assert_eq!(lines[7], format!(" luned's projects: {why}"));
            assert_eq!(lines[8], format!(" nixos's projects: {why}"));
        }
    }

    #[test]
    fn the_highlight_rests_on_your_view_until_the_sidebar_has_the_focus() {
        let mut sidebar = fresh();
        let mut host = host(
            vec![
                own_session("young", 1, vec![view(1, 0, "/w/b", "fish")]),
                own_session(
                    "older",
                    2,
                    vec![
                        view(2, 1, "/w/c", "nvim src/main.rs"),
                        view(1, 0, "/w/a", "fish"),
                    ],
                ),
            ],
            Vec::new(),
        );
        let changes = Some(api::Changes::default());
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "main".to_owned(),
            upstream: None,
            changed: None,
        }));
        let checkout = api::Checkout {
            head,
            changes,
            stashes: 0,
            repository: None,
        };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/c".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        sit(&mut sidebar, &tab_view_of("older", 2, 1, 2));

        let nvim = "▌    \u{e6ae} nvim src/main.rs";
        let (lines, lit) = seen(&mut sidebar, false, 32, 13);
        assert_eq!(lines[7..10], ["▌ ▾ Tab #2", "▌      main", nvim]);
        assert_eq!(lit, [7, 8, 9]);
        let (_, lit) = seen(&mut sidebar, true, 32, 7);
        assert_eq!(lit, [4, 5, 6]);
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            moment(160_000),
        );
        let (lines, lit) = seen(&mut sidebar, true, 32, 7);
        assert_eq!((lines[6].as_str(), lit.as_slice()), (nvim, &[2, 3][..]));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [4, 5, 6]);
        let fish = own_view_of("older", 2, 1);
        assert_eq!(
            click(&mut sidebar, 10, 3, Rect::new(0, 0, 32, 14)),
            go_here(fish)
        );
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [4, 5, 6]);
    }

    #[test]
    fn the_highlight_stops_on_a_project_s_row_and_goes_no_further_than_the_ends() {
        let mut sidebar = two_sessions();
        let older = Some(Place::Session(listed("older", 10)));
        let fold = Some(view_of("older", 10, 1));
        let nvim = Some(tab_view_of("older", 10, 1, 2));
        let newer = Some(Place::Session(listed("newer", 20)));
        let shell = Some(view_of("newer", 20, 1));
        assert_eq!(lit_place(&sidebar), older);
        let mut went = Vec::new();
        for _ in 0..5 {
            sidebar.step(1);
            went.push(lit_place(&sidebar));
        }
        assert_eq!(went, [fold, nvim, newer, shell.clone(), shell]);
        sidebar.step(-9);
        assert_eq!(lit_place(&sidebar), older);
    }

    #[test]
    fn h_folds_a_project_from_its_row_and_l_or_space_unfolds_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let newer = Some(Place::Session(listed("newer", 20)));
        sidebar.light(Spot::Local(tab_view_of("older", 10, 1, 2)));
        type_in(&mut sidebar, "j");
        let (lines, lit) = seen(&mut sidebar, true, 32, 24);
        let lit: Vec<&str> = lit
            .iter()
            .map(|y| lines[usize::from(*y)].as_str())
            .collect();
        assert_eq!(lit.len(), 1, "{lit:?}");
        assert_eq!(lit[0], " ● newer");
        type_in(&mut sidebar, "h");
        assert_eq!(lit_place(&sidebar), newer);
        assert!(folded(&sidebar, "newer"));
        assert!(!folded(&sidebar, "older"));
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        let tab = tab_of(20, 0);
        assert!(!sidebar.folds.contains(Fold::Worktree(&Machine::Local, tab)));
        assert_eq!(
            rows_shown(&mut sidebar).last().map(String::as_str),
            Some(" ▸ newer")
        );
        type_in(&mut sidebar, "l");
        assert!(!folded(&sidebar, "newer"));
        assert_eq!(lit_place(&sidebar), newer);
        type_in(&mut sidebar, "  ");
        assert!(!folded(&sidebar, "newer"));
        type_in(&mut sidebar, " ");
        assert!(folded(&sidebar, "newer"));
    }

    #[test]
    fn g_and_end_go_to_the_last_row_and_gg_and_home_to_the_first() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let first = Some(Place::Session(listed("older", 10)));
        let second = Some(Place::Session(listed("newer", 20)));
        let last = Some(view_of("newer", 20, 1));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let shift_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        let g = key(KeyCode::Char('g'));
        sidebar.key(&shift_g, moment(160_000));
        assert_eq!(lit_place(&sidebar), last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), first);
        sidebar.key(&key(KeyCode::End), moment(160_000));
        assert_eq!(lit_place(&sidebar), last);
        sidebar.key(&key(KeyCode::Home), moment(160_000));
        assert_eq!(lit_place(&sidebar), first);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        for pressed in [key(KeyCode::End), g, key(KeyCode::Char('k')), g, ctrl_x, g] {
            sidebar.key(&pressed, moment(160_000));
        }
        assert_eq!(lit_place(&sidebar), second);
        let area = Rect::new(0, 0, 32, 11);
        screen(&mut sidebar, true, area.width, area.height);
        sidebar.mouse_rows(mouse(MouseEventKind::ScrollUp, 0, 0), area, test_wall());
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), second);
        sidebar.mouse_rows(mouse(MouseEventKind::Moved, 0, 0), area, test_wall());
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), first);
        sidebar.key(&g, moment(160_000));
        assert_eq!(click(&mut sidebar, 1, 10, area), Asked::Take);
        let folded = Some(Place::Session(listed("newer", 20)));
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), folded);
        sidebar.focus(Keys::Terminal, test_wall());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.key(&g, moment(160_000));
        assert_eq!(lit_place(&sidebar), folded);
        let esc = key(KeyCode::Esc);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Leave);
    }

    #[test]
    fn the_highlight_goes_on_past_a_second_session_of_a_name() {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![
                session("x", 10, vec![]),
                session("y", 20, vec![view(1, 0, "/w/y", "fish")]),
                session("x", 30, vec![]),
                session("z", 40, vec![]),
            ],
            Vec::new(),
        ));
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        for _ in 0..4 {
            sidebar.key(&down, moment(160_000));
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let z = Place::Session(listed("z", 40));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(z));
    }

    #[test]
    fn folding_a_view_s_worktree_then_its_project_stops_the_highlight_on_its_row() {
        let mut sidebar = two_sessions();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        step_onto_its_first_view(&mut sidebar);
        type_in(&mut sidebar, "hh");
        let older = Place::Session(listed("older", 10));
        assert_eq!(lit_place(&sidebar), Some(older.clone()));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(older));
        let lines = screen(&mut sidebar, false, 32, 7);
        assert!(lines[4].starts_with(" ▸ older"), "{lines:?}");
        assert_eq!(lines[5..7], ["", " ● newer"], "{lines:?}");
        sidebar.key(
            &KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
            moment(160_000),
        );
        assert_eq!(
            sidebar.rows.len(),
            8,
            "the worktree folded first stays folded"
        );
    }

    #[test]
    fn the_title_keeps_the_name_of_the_last_session_listed_where_lune_sits() {
        let mut sidebar = own_two_sessions();
        assert_eq!(sidebar.own_title(), None);
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        assert_eq!(sidebar.own_title(), Some("older"));
        sidebar.sit(seat_in(3, None), test_wall());
        assert_eq!(sidebar.own_title(), Some("older"));
        sit(&mut sidebar, &own_view_of("young", 2, 1));
        assert_eq!(sidebar.own_title(), Some("young"));
    }

    #[test]
    fn lune_shown_another_worktree_of_its_project_is_on_no_view_until_it_focuses_one() {
        let mut sidebar = own_two_sessions();
        let older = |view| view_place("older", 1, u32::from(view == 2), view);
        sit(&mut sidebar, &Place::View(older(1)));
        assert_eq!(sidebar.here_place(), Ok(older(1)));
        let at = Sitting::Worktree {
            worktree: tab_of(1, 1),
            layout: SeatedLayout::Unlaid,
        };
        sidebar.sit(
            Seat {
                at,
                ..seat_in(1, None)
            },
            test_wall(),
        );
        assert_eq!(
            sidebar.here_place(),
            Err(NO_HERE_VIEW),
            "not the view of the worktree left"
        );
        sit(&mut sidebar, &Place::View(older(2)));
        assert_eq!(sidebar.here_place(), Ok(older(2)));
    }

    #[test]
    fn going_to_a_folded_session_unfolds_it() {
        let older = |view| own_view_of("older", 1, view);
        let away = || {
            let mut sidebar = own_two_sessions();
            fold(&mut sidebar, "older");
            sidebar
        };
        let mut sidebar = away();
        sit(&mut sidebar, &own_view_of("young", 2, 1));
        assert!(folded(&sidebar, "older"));
        sit(&mut sidebar, &older(1));
        assert!(!folded(&sidebar, "older"));
        let mut sidebar = away();
        sidebar.sit(seat_in(1, None), test_wall());
        assert!(!folded(&sidebar, "older"));
    }

    #[test]
    fn going_into_a_project_on_a_folded_machine_unfolds_the_machine() {
        let mut sidebar = own_two_sessions();
        sidebar.edit(|state| {
            let _ = state.folds.set(Fold::Machine(&Machine::Local), true);
        });
        sidebar.rebuild(test_wall());
        let folded = " ▸ \u{f108} MAC                    ●1";
        assert_eq!(screen(&mut sidebar, false, 32, 5)[2], folded);
        let view = own_view_of("older", 1, 1);
        sit(&mut sidebar, &view);
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        assert_eq!(sidebar.here_row, Some(Spot::Local(view)));
    }

    #[test]
    fn a_gone_view_leaves_the_highlight_in_its_session() {
        let mut sidebar = two_sessions();
        sidebar.step(2);
        assert_eq!(lit_place(&sidebar), Some(tab_view_of("older", 10, 1, 2)));
        sidebar.hear(host(
            vec![session(
                "older",
                10,
                vec![view(1, 0, "/w/lune/main", "claude")],
            )],
            Vec::new(),
        ));
        assert_eq!(
            lit_place(&sidebar),
            Some(Place::Session(listed("older", 10)))
        );
    }

    #[test]
    fn a_view_gone_by_itself_leaves_the_highlight_on_the_next_view_of_its_worktree() {
        let mut sidebar = fresh();
        let hear = |sidebar: &mut Sidebar, fix: &[u32]| {
            let fix = worktree_of(31, "fix", "/w/fix", fix);
            let main = worktree_of(32, "main", "/w/main", &[4]);
            let projects = vec![a_project(7, "lune", vec![fix, main])];
            sidebar.heard(
                Host {
                    projects: Some(kept(projects)),
                    ..host(Vec::new(), Vec::new())
                },
                SystemTime::UNIX_EPOCH,
            );
        };
        let worktree = WorktreeId::of(31).unwrap();
        let fix = WorktreePlace {
            project: project_row("lune", 7),
            worktree,
        };
        let view = |id| {
            Some(Place::View(ViewPlace::new(
                fix.clone(),
                ViewKey::terminal(id),
            )))
        };
        hear(&mut sidebar, &[1, 2, 3]);
        sidebar.light(Spot::Local(view(2).unwrap()));
        hear(&mut sidebar, &[1, 3]);
        assert_eq!(
            lit_place(&sidebar),
            view(3),
            "the view below it in its worktree"
        );
        hear(&mut sidebar, &[1]);
        let above = "the last of its worktree's views leaves it on the one above, not below it";
        assert_eq!(lit_place(&sidebar), view(1), "{above}");
        hear(&mut sidebar, &[]);
        let home = "the last view of all leaves it on its worktree's row";
        assert_eq!(
            lit_place(&sidebar),
            Some(Place::Worktree(fix.clone())),
            "{home}"
        );
    }

    #[test]
    fn the_rows_scroll_to_keep_the_highlight_on_screen() {
        let mut sidebar = two_sessions();
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let shown = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 5)[HEADER..5].to_vec();
        let older = " ● older                    ●1";
        assert_eq!(
            shown(&mut sidebar),
            [" ▾ \u{f108} MAC                    ●1", "", older]
        );
        sidebar.key(&down, moment(160_000));
        assert_eq!(
            shown(&mut sidebar),
            [
                older,
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fold                 1m"
            ]
        );
        for _ in 0..3 {
            sidebar.key(&down, moment(160_000));
        }
        assert_eq!(
            shown(&mut sidebar),
            [" ● newer", "  ▾ Tab #1", "     \u{e795} w/b"]
        );
        sidebar.key(&up, moment(160_000));
        sidebar.key(&up, moment(160_000));
        assert_eq!(
            shown(&mut sidebar),
            ["     \u{e6ae} nvim src/main.rs", "", " ● newer"]
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(sidebar: &mut Sidebar, column: u16, row: u16, area: Rect) -> Asked {
        let down = mouse(MouseEventKind::Down(MouseButton::Left), column, row);
        assert_eq!(sidebar.mouse_rows(down, area, test_wall()), Asked::Nothing);
        sidebar.mouse_rows(
            mouse(MouseEventKind::Up(MouseButton::Left), column, row),
            area,
            test_wall(),
        )
    }

    #[test]
    fn a_click_goes_to_a_view_and_folds_on_the_triangle() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 11);
        screen(&mut sidebar, false, area.width, area.height);
        let view = tab_view_of("older", 10, 1, 2);
        assert_eq!(click(&mut sidebar, 10, 8, area), go_here(view));
        let older = Place::Session(listed("older", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(older.clone()));
        assert!(!folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(older));
    }

    #[test]
    fn a_click_on_the_triangle_folds_until_a_search_is_typed() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 11);
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            moment(160_000),
        );
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(!folded(&sidebar, "older"));
        assert!(sidebar.search().is_some());
        type_in(&mut sidebar, "older");
        screen(&mut sidebar, true, area.width, area.height);
        let older = go_here(Place::Session(listed("older", 10)));
        assert_eq!(
            click(&mut sidebar, 1, 4, area),
            older,
            "a typed search folds nothing"
        );
        assert!(!folded(&sidebar, "older"));
    }

    #[test]
    fn a_click_on_a_session_with_no_views_goes_there() {
        let mut sidebar = fresh();
        sidebar.hear(host(vec![session("x", 10, Vec::new())], Vec::new()));
        let area = Rect::new(0, 0, 32, 6);
        screen(&mut sidebar, false, area.width, area.height);
        let x = Place::Session(listed("x", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(x.clone()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(x));
    }

    #[test]
    fn a_click_on_the_notes_goes_nowhere() {
        let mut sidebar = two_sessions();
        sidebar.notify("a word".to_owned());
        let area = Rect::new(0, 0, 32, 6);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[5], " a word", "{lines:?}");
        assert_eq!(click(&mut sidebar, 10, 5, area), Asked::Take);
        let older = Place::Session(listed("older", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(older));
    }

    #[test]
    fn a_view_is_named_for_what_runs_in_it() {
        let named = |command: &str| label(None, &known(&view(3, 0, "/w/lune/main", command)));
        assert_eq!(named("-fish"), "lune/main");
        assert_eq!(named("/bin/zsh -l"), "lune/main");
        assert_eq!(named("cargo test"), "cargo test");
        assert_eq!(
            named("/nix/store/a1p5-neovim/bin/nvim --clean"),
            "nvim --clean"
        );
    }

    #[test]
    fn an_agent_goes_by_its_name_then_its_topic_then_its_directory() {
        let titled = |topic: Option<&str>| {
            let titled = Some(api::Titled {
                topic: topic.map(str::to_owned),
            });
            known(&View {
                titled,
                ..view(1, 0, "/w/x", "claude")
            })
        };
        let untitled = view(1, 0, "/w/x", "claude");
        let named = agent("s", 1, Status::Idle, Some("fold"));
        let unnamed = agent("s", 1, Status::Idle, None);
        let topic = titled(Some("Fix the fold"));
        assert_eq!(label(Some(&named), &topic), "fold");
        assert_eq!(label(Some(&unnamed), &topic), "Fix the fold");
        assert_eq!(label(Some(&unnamed), &titled(None)), "w/x");
        assert_eq!(label(Some(&unnamed), &known(&untitled)), "w/x");
        let mut derived = agent("s", 1, Status::Idle, Some("lune-main"));
        let source = Some(NameSource::Derived.into());
        derived.name = derived.name.map(|name| api::Name { source, ..name });
        assert_eq!(label(Some(&derived), &topic), "Fix the fold");
        assert_eq!(label(Some(&derived), &titled(None)), "w/x");
        let mut auto = agent("s", 1, Status::Idle, Some("Feature parity"));
        auto.name = auto.name.map(|name| api::Name {
            source: Some(NameSource::Auto.into()),
            ..name
        });
        assert_eq!(label(Some(&auto), &titled(None)), "Feature parity");
        assert_eq!(label(Some(&auto), &topic), "Fix the fold");
        let kept = |agent: &Agent| Agent {
            topic: Some("Session folds".to_owned()),
            ..agent.clone()
        };
        assert_eq!(label(Some(&kept(&named)), &topic), "fold");
        assert_eq!(label(Some(&kept(&unnamed)), &topic), "Session folds");
        assert_eq!(label(Some(&kept(&derived)), &titled(None)), "Session folds");
        assert_eq!(label(Some(&kept(&auto)), &titled(None)), "Session folds");
        let blank_topic = |agent: &Agent| Agent {
            topic: Some("   ".to_owned()),
            ..agent.clone()
        };
        assert_eq!(
            label(Some(&blank_topic(&auto)), &titled(None)),
            "Feature parity"
        );
        assert_eq!(label(Some(&blank_topic(&auto)), &topic), "Fix the fold");
        assert_eq!(label(Some(&blank_topic(&unnamed)), &titled(None)), "w/x");

        let mut agy_auto = auto.clone();
        agy_auto.harness = Some(Harness::Antigravity.into());
        assert_eq!(label(Some(&agy_auto), &topic), "Feature parity");
        assert_eq!(label(Some(&kept(&agy_auto)), &topic), "Session folds");

        let mut pi_unnamed = unnamed.clone();
        pi_unnamed.harness = Some(Harness::Pi.into());
        assert_eq!(label(Some(&pi_unnamed), &topic), "w/x");

        let ended_titled = known(&View {
            command: None,
            held: true,
            titled: Some(api::Titled {
                topic: Some("Fix the fold".to_owned()),
            }),
            ..view(2, 0, "/w/x", "claude")
        });
        assert_eq!(label(None, &ended_titled), "Fix the fold");
        let ended_untitled = known(&View {
            command: None,
            held: true,
            titled: None,
            ..view(2, 0, "/w/x", "claude")
        });
        assert_eq!(label(None, &ended_untitled), "w/x");
        assert_eq!(label(None, &topic), "Fix the fold");
        let stale_shell = known(&View {
            titled: Some(api::Titled {
                topic: Some("Fix the fold".to_owned()),
            }),
            ..view(3, 0, "/w/x", "-fish")
        });
        assert_eq!(label(None, &stale_shell), "w/x");
        let stale_nvim = known(&View {
            titled: Some(api::Titled {
                topic: Some("Fix the fold".to_owned()),
            }),
            ..view(4, 0, "/w/x", "nvim a")
        });
        assert_eq!(label(None, &stale_nvim), "nvim a");
        let stale_agy = known(&View {
            titled: Some(api::Titled {
                topic: Some("Fix the fold".to_owned()),
            }),
            ..view(5, 0, "/w/x", "agy")
        });
        assert_eq!(label(None, &stale_agy), "agy");

        for weird_dir in ["/ ", "/   ", "/\u{00a0}"] {
            let weird_view = known(&view(6, 0, weird_dir, "-fish"));
            assert_eq!(label(None, &weird_view), weird_dir);
            assert_eq!(label(Some(&unnamed), &weird_view), weird_dir);
        }
    }

    #[test]
    fn view_titles_match_the_sidebar_row_for_agents_and_shells() {
        let mut sidebar = fresh();
        let renamed_agent = agent("s", 1, Status::Idle, Some("fold"));
        let mut agy_auto = agent("s", 2, Status::Busy, Some("Initial agy title"));
        agy_auto.harness = Some(Harness::Antigravity.into());
        agy_auto.name = agy_auto.name.map(|name| api::Name {
            source: Some(NameSource::Auto.into()),
            ..name
        });
        let mut agy_live = agent("s", 3, Status::Busy, Some("Initial agy title"));
        agy_live.harness = Some(Harness::Antigravity.into());
        agy_live.name = agy_live.name.map(|name| api::Name {
            source: Some(NameSource::Auto.into()),
            ..name
        });
        agy_live.topic = Some("View tabs".to_owned());

        let worktree_id = tab_of(10, 0);
        let ran = View {
            shell: Some(Shell {
                doing: Some(Doing::Ended(Ended {
                    command: Some("cargo test".to_owned()),
                    at: Some(Timestamp {
                        seconds: 100,
                        nanos: 0,
                    }),
                    ending: Some(ended::Ending::Status(0)),
                    took: Some(prost_types::Duration {
                        seconds: 6,
                        nanos: 0,
                    }),
                })),
            }),
            ..view(6, 0, "/w/lune/main", "-fish")
        };
        let blank_ran = View {
            shell: Some(Shell {
                doing: Some(Doing::Ended(Ended {
                    command: Some("   ".to_owned()),
                    at: Some(Timestamp {
                        seconds: 100,
                        nanos: 0,
                    }),
                    ending: Some(ended::Ending::Status(0)),
                    took: Some(prost_types::Duration {
                        seconds: 6,
                        nanos: 0,
                    }),
                })),
            }),
            ..view(7, 0, "/ ", "-fish")
        };
        let ended = View {
            command: None,
            held: true,
            titled: Some(api::Titled {
                topic: Some("Finished session".to_owned()),
            }),
            ..view(8, 0, "/w/lune/main", "claude")
        };
        let agy_with_stale_osc = View {
            titled: Some(api::Titled {
                topic: Some("Stale claude topic".to_owned()),
            }),
            ..view(2, 0, "/w/lune/main", "agy")
        };
        let views = vec![
            view(1, 0, "/w/lune/main", "claude"),
            agy_with_stale_osc,
            view(3, 0, "/w/lune/main", "agy"),
            view(4, 0, "/w/lune/main", "nvim Cargo.toml"),
            view(5, 0, "/w/lune/main", "-fish"),
            ran,
            blank_ran,
            ended,
        ];
        sidebar.hear(host(
            vec![session("s", 10, views)],
            vec![renamed_agent, agy_auto, agy_live],
        ));

        let titles = sidebar
            .view_titles(&Machine::Local, worktree_id)
            .expect("worktree titles");
        let at = |id: u64| titles.get(&ViewId::of(id).unwrap()).map(String::as_str);
        assert_eq!(at(1), Some("fold"));
        assert_eq!(at(2), Some("Initial agy title"));
        assert_eq!(at(3), Some("View tabs"));
        assert_eq!(at(4), Some("nvim Cargo.toml"));
        assert_eq!(at(5), Some("lune/main"));
        assert_eq!(at(6), Some("cargo test"));
        assert_eq!(at(7), Some("/ "));
        assert_eq!(at(8), Some("Finished session"));
    }

    #[test]
    fn a_view_s_icon_says_what_runs_in_it_in_a_column_of_its_own() {
        let mut sidebar = fresh();
        let sudo = "/nix/store/fdsygnh4wvr7g860ba3cwlcksn8v1pc5-sudo-1.9.17p2/bin/sudo -k nvim x";
        sidebar.hear(host(
            vec![session(
                "s",
                10,
                vec![
                    view(1, 0, "/w/a", "cargo test"),
                    view(2, 1, "/w/b", sudo),
                    view(3, 2, "/w/c", "make"),
                    view(4, 3, "/w/d", "dash"),
                ],
            )],
            vec![agent("s", 1, Status::Idle, Some("fold"))],
        ));
        let lines = screen(&mut sidebar, false, 32, 13);
        assert_eq!(
            lines[6..],
            [
                "  •✓ \u{ec82} fold                 1m",
                "  ▾ Tab #2",
                "     \u{e6ae} sudo -k nvim x",
                "  ▾ Tab #3",
                "       make",
                "  ▾ Tab #4",
                "     \u{e795} w/d",
            ]
        );
    }

    #[test]
    fn an_agent_s_icon_is_its_harness_s() {
        let mut sidebar = fresh();
        let of = |id: u32, harness: Option<i32>| Agent {
            harness,
            ..agent("s", id, Status::Idle, Some("x"))
        };
        let views = (1..=3).map(|id| view(id, id, "/w/a", "nvim")).collect();
        let agents = vec![
            of(1, Some(Harness::Pi.into())),
            of(2, None),
            of(3, Some(99)),
        ];
        sidebar.hear(host(vec![session("s", 10, views)], agents));
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 13);
        assert_eq!(
            [6, 8, 10].map(|at| start(&lines[at])),
            ["  •✓ \u{f03ff} x", "  •✓ \u{ec82} x", "  •✓ \u{e6ae} x"]
        );
    }

    #[test]
    fn a_word_an_agent_says_is_marked_in_the_one_column_a_mark_takes() {
        let words = ["compacting", "ßa", "日本", "\u{301}a"];
        let says = |view: u32, word: &str| Agent {
            status: Some(Reported::OtherStatus(word.to_owned())),
            ..agent("s", view, Status::Idle, Some("x"))
        };
        let mut sidebar = fresh();
        let views = (1..=4).map(|id| view(id, id, "/w/a", "claude")).collect();
        let agents = words
            .into_iter()
            .zip(1..)
            .map(|(word, id)| says(id, word))
            .collect();
        sidebar.hear(host(vec![session("s", 10, views)], agents));
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(
            [6, 8, 10, 12].map(|at| start(&lines[at])),
            [
                "   C \u{ec82} x",
                "   S \u{ec82} x",
                "   ? \u{ec82} x",
                "   ? \u{ec82} x"
            ]
        );
    }

    #[test]
    fn a_view_with_no_agent_in_it_is_dimmed() {
        let mut sidebar = two_sessions();
        let buffer = drawn(&mut sidebar, false, 32, 10);
        let dim = |y: u16| [5_u16, 7].map(|x| buffer[(x, y)].modifier.contains(Modifier::DIM));
        assert_eq!((dim(6), dim(8)), ([false, false], [true, true]));
    }

    #[test]
    fn a_view_holds_the_agent_started_at_a_prompt() {
        let mut prompt = agent("s", 1, Status::Idle, Some("prompt"));
        prompt.started_at = Some(Timestamp {
            seconds: 90,
            nanos: 0,
        });
        let mut headless = agent("s", 1, Status::Busy, Some("headless"));
        headless.entrypoint = Some("sdk-cli".to_owned());
        let agents = [in_own(headless, 1, 1), in_own(prompt, 1, 1)];
        let seated = seated(&agents);
        assert_eq!(
            seated
                .get(&view_key(1, 1).key())
                .and_then(|agent| agent.name.as_ref())
                .map(|name| name.text.as_str()),
            Some("prompt")
        );
    }

    #[test]
    fn a_view_holds_the_pi_agent_started_at_a_prompt() {
        let of = |name: &str, mode: &str, started: i64| Agent {
            harness: Some(Harness::Pi.into()),
            entrypoint: Some(mode.to_owned()),
            started_at: Some(Timestamp {
                seconds: started,
                nanos: 0,
            }),
            ..agent("s", 1, Status::Idle, Some(name))
        };
        let agents = [of("on its own", "print", 10), of("prompt", "tui", 90)];
        let agents = agents.map(|agent| in_own(agent, 1, 1));
        let seated = seated(&agents);
        let at = view_key(1, 1).key();
        let name = seated.get(&at).and_then(|agent| agent.name.as_ref());
        assert_eq!(name.map(|name| name.text.as_str()), Some("prompt"));
    }

    #[test]
    fn an_agent_is_at_a_prompt_by_its_harness_s_word() {
        let (claude, pi) = (Some(Harness::ClaudeCode.into()), Some(Harness::Pi.into()));
        let cases = [
            (claude, "cli", true),
            (None, "cli", true),
            (pi, "tui", true),
            (claude, "tui", false),
            (pi, "cli", false),
            (pi, "print", false),
            (Some(99), "cli", false),
        ];
        for (harness, word, at) in cases {
            let agent = Agent {
                harness,
                entrypoint: Some(word.to_owned()),
                ..agent("s", 1, Status::Idle, None)
            };
            assert_eq!(agent.at_a_prompt(), at, "{harness:?} {word}");
        }
    }

    fn in_shell(id: u32, doing: Doing) -> View {
        View {
            shell: Some(Shell { doing: Some(doing) }),
            ..view(id, 0, "/w/lune/main", "fish")
        }
    }

    fn going(command: &str, since: i64, front: Front) -> Doing {
        let since = Some(Timestamp {
            seconds: since,
            nanos: 0,
        });
        Doing::Going(Going {
            command: Some(command.to_owned()),
            since,
            front: front.into(),
        })
    }

    fn ended(command: &str, status: Option<i32>, millis: i32) -> Doing {
        let (seconds, nanos) = ((millis / 1000).into(), millis % 1000 * 1_000_000);
        let took = Some(prost_types::Duration { seconds, nanos });
        let at = Some(Timestamp {
            seconds: 130,
            nanos: 0,
        });
        let ending = status.map(ended::Ending::Status);
        Doing::Ended(Ended {
            command: Some(command.to_owned()),
            at,
            ending,
            took,
        })
    }

    #[test]
    fn what_a_shell_said_it_ran_is_read_as_it_comes() {
        let said = |doing| ran(&Told::of(Some(&Shell { doing: Some(doing) })));
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        let command = Said::new("make".to_owned()).unwrap();
        let make = Ran::Ended(menu::Ended {
            command,
            ending: Some(Ending::Exited(2)),
            at,
        });
        assert_eq!(said(ended("make", Some(2), 10)), make);
        assert_eq!(said(ended("", Some(0), 10)), Ran::Unknown);
        let Doing::Ended(untimed) = ended("make", Some(2), 10) else {
            panic!("an end");
        };
        assert_eq!(
            said(Doing::Ended(Ended {
                at: None,
                ..untimed
            })),
            Ran::Unknown
        );
        assert_eq!(said(going("", 100, Front::Working)), Ran::Unknown);
        let Doing::Going(working) = going("make", 100, Front::Working) else {
            panic!("a command going");
        };
        assert_eq!(
            said(Doing::Going(Going {
                command: None,
                ..working
            })),
            Ran::Going(None)
        );
        let unsaid = Ended {
            command: None,
            ..untimed
        };
        assert_eq!(said(Doing::Ended(unsaid.clone())), Ran::EndedUnsaid);
        assert_eq!(
            said(Doing::Ended(Ended { at: None, ..unsaid })),
            Ran::Unknown
        );
        assert_eq!(ran(&Told::of(Some(&Shell { doing: None }))), Ran::Unknown);
        assert_eq!(ran(&Told::of(None)), Ran::Unhooked);
        assert_eq!(said(Doing::Ready(())), Ran::Ready);
    }

    #[test]
    fn a_command_has_an_agent_s_marks_and_its_row_is_named_for_it() {
        let mut sidebar = fresh();
        let stopped = Doing::Stopped(Stopped {
            command: Some("make".to_owned()),
            at: Some(Timestamp {
                seconds: 130,
                nanos: 0,
            }),
        });
        let nvim = in_shell(3, going("nvim notes", 100, Front::Interactive));
        let nvim = View {
            command: Some("nvim notes".to_owned()),
            ..nvim
        };
        sidebar.hear(host(
            vec![session(
                "s",
                10,
                vec![
                    in_shell(1, going("cargo build --release", 100, Front::Working)),
                    in_shell(2, going("sudo -k true", 100, Front::Asking)),
                    nvim,
                    in_shell(4, ended("cargo test", Some(0), 41_873)),
                    in_shell(5, ended("cargo clippy", Some(101), 5_000)),
                    in_shell(6, ended("cargo run", None, 7_000)),
                    in_shell(7, ended("ls", Some(0), 20)),
                    in_shell(8, stopped),
                    in_shell(9, Doing::Ready(())),
                ],
            )],
            Vec::new(),
        ));
        assert_eq!(
            screen(&mut sidebar, false, 32, 15),
            [
                " ☾ lune  1 project  ●1  •3  ⠋1",
                "",
                " ▾ \u{f108} MAC                    ●1",
                "",
                " ● s                        ●1",
                "  ▾ Tab #1                  ●1",
                "   ⠋ \u{e795} cargo build --rele… 59s",
                "   ● \u{e795} sudo -k true        59s",
                "     \u{e6ae} nvim notes",
                "  •✓ \u{e795} cargo test          29s",
                "  •✗ \u{e795} cargo clippy        29s",
                "  •$ \u{e795} cargo run           29s",
                "     \u{e795} lune/main",
                "     \u{e795} lune/main",
                "     \u{e795} lune/main",
            ]
        );
        assert!(sidebar.turning());
    }

    fn moment(millis: u64) -> Moment {
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        let since = Duration::from_millis(millis);
        Moment {
            system: SystemTime::UNIX_EPOCH + since,
            steady: *START + since,
        }
    }

    fn tick(sidebar: &mut Sidebar, millis: u64) {
        let _ = sidebar.ticked(moment(millis), true);
    }

    fn dimmed(sidebar: &mut Sidebar, x: u16, y: u16, now: Moment) -> bool {
        drawn_at(sidebar, false, 32, 8, now)[(x, y)]
            .modifier
            .contains(Modifier::DIM)
    }

    #[test]
    fn the_picker_gets_an_agents_directory_as_its_last_two_parts() {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![session(
                "s",
                10,
                vec![view(1, 0, "/home/me/src/lune/pr-12", "claude")],
            )],
            vec![agent("s", 1, Status::Idle, None)],
        ));
        let agents = sidebar.listing().local.agents;
        let dirs: Vec<&str> = agents.iter().map(|seated| seated.dir.as_str()).collect();
        assert_eq!(dirs, ["lune/pr-12"]);
    }

    #[test]
    fn what_finished_has_the_dot_until_you_stay_in_its_view() {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![own_session(
                "s",
                1,
                vec![
                    view(1, 0, "/w/lune/main", "claude"),
                    in_shell(2, ended("cargo test", Some(0), 5_000)),
                    view(3, 3, "/w/a", "fish"),
                ],
            )],
            vec![in_own(agent("s", 1, Status::Idle, Some("fold")), 1, 1)],
        ));
        let lines = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, true);
            seen_at(sidebar, false, 32, 10, now).0
        };
        assert_eq!(
            lines(&mut sidebar, moment(160_000)),
            [
                " ☾ lune  1 project          •2",
                "",
                " ▾ \u{f108} MAC",
                "",
                " ● s",
                "  ▾ Tab #1",
                "  •✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          29s",
                "  ▾ Tab #4",
                "     \u{e795} w/a",
            ]
        );
        assert!(!dimmed(&mut sidebar, 3, 6, moment(160_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Finished);
        step_onto_its_first_view(&mut sidebar);
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            moment(160_000),
        );
        assert!(lines(&mut sidebar, moment(160_000))[5].ends_with(" •"));
        sidebar.key(
            &KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
            moment(160_000),
        );

        let s = |view| tab_view_of("s", 1, if view == 3 { 3 } else { 0 }, view);
        sit(&mut sidebar, &s(2));
        assert_eq!(
            lines(&mut sidebar, moment(160_000))[7],
            "▌    \u{e795} lune/main"
        );
        sit(&mut sidebar, &s(1));
        lines(&mut sidebar, moment(160_500));
        lines(&mut sidebar, moment(161_500));
        sit(&mut sidebar, &s(3));
        assert_eq!(
            lines(&mut sidebar, moment(162_000))[..8],
            [
                " ☾ lune  1 project          •1",
                "",
                " ▾ \u{f108} MAC",
                "",
                " ● s",
                "  ▾ Tab #1",
                "   ✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          31s",
            ]
        );
        assert!(dimmed(&mut sidebar, 3, 6, moment(162_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Idle);

        sit(&mut sidebar, &s(2));
        lines(&mut sidebar, moment(163_000));
        lines(&mut sidebar, moment(164_000));
        sit(&mut sidebar, &s(3));
        let seen = lines(&mut sidebar, moment(164_500));
        assert_eq!(seen[0], " ☾ lune  1 project");
        assert_eq!(seen[7], "     \u{e795} lune/main");
    }

    #[test]
    fn u_marks_a_view_unread_until_you_come_back_to_it() {
        let mut sidebar = own_two_sessions();
        let older = |view| own_view_of("older", 1, view);
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let nvim = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, true);
            let lines = seen_at(sidebar, true, 32, 12, now).0;
            lines
                .into_iter()
                .find(|line| line.contains("nvim"))
                .expect("nvim's row")
        };
        sit(&mut sidebar, &older(2));
        nvim(&mut sidebar, moment(160_000));
        sidebar.key(&u, moment(161_000));
        assert_eq!(
            nvim(&mut sidebar, moment(165_000)),
            "▌ •  \u{e6ae} nvim src/main.rs"
        );
        assert!(screen(&mut sidebar, true, 32, 7)[0].ends_with(" ●1  •1"));

        sit(&mut sidebar, &older(1));
        nvim(&mut sidebar, moment(166_000));
        sit(&mut sidebar, &older(2));
        assert_eq!(
            nvim(&mut sidebar, moment(167_000)),
            "▌ •  \u{e6ae} nvim src/main.rs"
        );
        assert_eq!(
            nvim(&mut sidebar, moment(168_000)),
            "▌    \u{e6ae} nvim src/main.rs"
        );

        sidebar.key(&u, moment(168_000));
        assert_eq!(
            nvim(&mut sidebar, moment(168_000)),
            "▌ •  \u{e6ae} nvim src/main.rs"
        );
        sidebar.key(&u, moment(168_000));
        assert_eq!(
            nvim(&mut sidebar, moment(168_000)),
            "▌    \u{e6ae} nvim src/main.rs"
        );
    }

    fn own_idle_agent() -> Host {
        host(
            vec![own_session(
                "s",
                1,
                vec![
                    view(1, 0, "/w/lune/main", "claude"),
                    view(3, 1, "/w/a", "fish"),
                ],
            )],
            vec![in_own(agent("s", 1, Status::Idle, Some("fold")), 1, 1)],
        )
    }

    fn idle_agent() -> Host {
        host(
            vec![session(
                "s",
                10,
                vec![
                    view(1, 0, "/w/lune/main", "claude"),
                    view(3, 1, "/w/a", "fish"),
                ],
            )],
            vec![agent("s", 1, Status::Idle, Some("fold"))],
        )
    }

    fn agent_row(sidebar: &mut Sidebar, now: Moment, watched: bool) -> String {
        let _ = sidebar.ticked(now, watched);
        seen_at(sidebar, false, 32, 10, now).0[6].clone()
    }

    #[test]
    fn a_view_is_yours_only_while_lune_s_window_has_the_focus() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        let s = |view| own_view_of("s", 1, view);
        sit(&mut sidebar, &s(1));
        agent_row(&mut sidebar, moment(160_000), false);
        agent_row(&mut sidebar, moment(165_000), false);
        sit(&mut sidebar, &s(3));
        let new = "  •✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(166_000), true), new);

        sit(&mut sidebar, &s(1));
        agent_row(&mut sidebar, moment(167_000), false);
        agent_row(&mut sidebar, moment(168_000), true);
        agent_row(&mut sidebar, moment(169_000), true);
        sit(&mut sidebar, &s(3));
        let seen = "   ✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(170_000), true), seen);
    }

    #[test]
    fn u_marks_a_view_with_the_dot_read() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        sit(&mut sidebar, &own_view_of("s", 1, 3));
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let row = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, true);
            seen_at(sidebar, true, 32, 10, now).0[6].clone()
        };
        let (new, seen) = (
            "  •✓ \u{ec82} fold                 1m",
            "   ✓ \u{ec82} fold                 1m",
        );
        assert_eq!(row(&mut sidebar, moment(160_000)), new);
        sidebar.key(&key('k'), moment(160_000));
        sidebar.key(&key('u'), moment(160_000));
        assert_eq!(row(&mut sidebar, moment(160_000)), seen);

        sidebar.key(&key('u'), moment(161_000));
        assert_eq!(row(&mut sidebar, moment(161_000)), new);
        sidebar.key(&key('u'), moment(161_000));
        assert_eq!(row(&mut sidebar, moment(161_000)), seen);
        assert!(dimmed(&mut sidebar, 3, 5, moment(161_000)));
    }

    #[test]
    fn projects_of_one_name_keep_their_dots_apart() {
        let mut sidebar = fresh();
        let claude = || vec![view(1, 0, "/w/lune/main", "claude")];
        let twin = own_session("b", 1, claude());
        let mut there = in_own(agent("b", 1, Status::Idle, Some("fold")), 1, 1);
        there.pid = 2;
        sidebar.hear(host(
            vec![own_session("a", 2, claude()), twin],
            vec![
                in_own(agent("a", 1, Status::Idle, Some("fold")), 2, 1),
                there,
            ],
        ));
        sit(&mut sidebar, &own_view_of("a", 2, 1));
        let lines = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, true);
            seen_at(sidebar, false, 32, 11, now).0
        };
        lines(&mut sidebar, moment(160_000));
        let lines = lines(&mut sidebar, moment(161_000));
        assert_eq!(
            [&lines[6], &lines[10]],
            [
                "▌  ✓ \u{ec82} fold                 1m",
                "  •✓ \u{ec82} fold                 1m"
            ]
        );
    }

    fn restored(mut host: Host, made: Timestamp, terminal: impl Fn(u32) -> u32) -> Host {
        let projects = host
            .projects
            .as_mut()
            .and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = projects else {
            panic!("kept projects");
        };
        for project in &mut kept.projects {
            let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
                panic!("a clone");
            };
            for held in clone
                .worktrees
                .iter_mut()
                .flat_map(|worktree| &mut worktree.terminals)
            {
                let Some(api::terminal::Found::Key(key)) = &mut held.found else {
                    panic!("a found terminal");
                };
                key.id = terminal(key.id);
                key.made_at = Some(made);
            }
        }
        host
    }

    #[test]
    fn a_view_keeps_its_marks_when_a_new_mux_gives_its_terminal_another_id() {
        let mut sidebar = fresh();
        let s = || {
            let views = vec![
                in_shell(1, ended("cargo test", Some(0), 5_000)),
                view(2, 0, "/w/c", "nvim src/main.rs"),
                view(3, 0, "/w/a", "fish"),
            ];
            host(vec![own_session("s", 1, views)], Vec::new())
        };
        sidebar.hear(s());
        sit(&mut sidebar, &own_view_of("s", 1, 3));
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let rows = |sidebar: &mut Sidebar| seen_at(sidebar, true, 32, 9, moment(160_000)).0;
        let marked = [
            "     \u{e795} lune/main",
            "  •  \u{e6ae} nvim src/main.rs",
            "▌    \u{e795} w/a",
        ];
        assert_eq!(
            rows(&mut sidebar)[6],
            "  •✓ \u{e795} cargo test          29s"
        );
        for asked in ['k', 'u', 'k', 'u'] {
            sidebar.key(&key(asked), moment(160_000));
        }
        assert_eq!(rows(&mut sidebar)[6..], marked);

        let made = Timestamp {
            seconds: 99,
            nanos: 0,
        };
        sidebar.hear(restored(s(), made, |id| id % 3 + 1));
        let fish = TerminalKey::of(&api::TerminalKey {
            id: 1,
            made_at: Some(made),
        })
        .unwrap();
        let s = WorktreePlace {
            project: project_row("s", 1),
            worktree: tab_of(1, 0),
        };
        let fish = ViewPlace::new(s, ViewKey::Terminal(fish));
        sit(&mut sidebar, &Place::View(fish));
        assert_eq!(rows(&mut sidebar)[6..], marked);
    }

    fn looked_for(mut host: Host, terminal: u64) -> Host {
        let projects = host
            .projects
            .as_mut()
            .and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = projects else {
            panic!("kept projects");
        };
        for project in &mut kept.projects {
            let Some(api::project::Holds::Clone(clone)) = &mut project.holds else {
                panic!("a clone");
            };
            let held = clone
                .worktrees
                .iter_mut()
                .flat_map(|worktree| &mut worktree.terminals);
            for held in held.filter(|held| held.id == terminal) {
                held.found = Some(api::terminal::Found::Looking(()));
                held.state = Some(api::terminal::State::StateLooking(()));
                held.command = None;
                held.titled = None;
                held.shell = None;
                held.held = false;
            }
        }
        host
    }

    #[test]
    fn a_view_luned_looks_for_keeps_its_marks_and_one_it_drops_loses_them() {
        let mut sidebar = fresh();
        let s = |nvim: bool| {
            let views = [
                view(2, 0, "/w/c", "nvim src/main.rs"),
                view(3, 0, "/w/a", "fish"),
            ];
            let views = views
                .into_iter()
                .filter(|view| nvim || view.id != 2)
                .collect();
            host(vec![own_session("s", 1, views)], Vec::new())
        };
        sidebar.hear(s(true));
        sit(&mut sidebar, &own_view_of("s", 1, 3));
        let rows = |sidebar: &mut Sidebar| seen_at(sidebar, true, 32, 9, moment(160_000)).0;
        let unmarked = rows(&mut sidebar);
        sidebar.key(&char('k'), moment(160_000));
        sidebar.key(&char('u'), moment(160_000));
        let marked = rows(&mut sidebar);
        assert_ne!(marked, unmarked);

        sidebar.hear(looked_for(s(true), 2));
        assert!(
            sidebar.shelves[0].kept().is_some(),
            "{:?}",
            sidebar.shelves[0].refused()
        );
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 171_000);
        sidebar.hear(s(true));
        assert_eq!(rows(&mut sidebar), marked);

        sidebar.hear(s(false));
        tick(&mut sidebar, 172_000);
        tick(&mut sidebar, 182_000);
        sidebar.hear(s(true));
        assert_eq!(rows(&mut sidebar), unmarked);
    }

    #[test]
    fn a_session_left_out_of_a_list_keeps_what_you_saw_until_it_is_gone() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        let s = |view| own_view_of("s", 1, view);
        sit(&mut sidebar, &s(1));
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 161_000);
        sit(&mut sidebar, &s(3));
        let seen = "   ✓ \u{ec82} fold                 1m";
        let new = "  •✓ \u{ec82} fold                 1m";
        for (left, back, row) in [(163_000, 172_000, seen), (173_000, 183_000, new)] {
            sidebar.hear(host(Vec::new(), Vec::new()));
            tick(&mut sidebar, left);
            tick(&mut sidebar, back);
            sidebar.hear(own_idle_agent());
            assert_eq!(agent_row(&mut sidebar, moment(back + 500), true), row);
        }
    }

    #[test]
    fn a_project_a_list_falls_short_of_keeps_what_you_saw() {
        let short: [fn(&mut Sidebar); 3] = [
            |sidebar| {
                sidebar.hear(host(Vec::new(), Vec::new()));
                let _ = sidebar.lost("luned does not answer".to_owned());
            },
            |sidebar| {
                let unkept = api::projects::Held::Unkept("its file does not read".to_owned());
                let projects = Some(api::Projects { held: Some(unkept) });
                sidebar.hear(Host {
                    projects,
                    ..host(Vec::new(), Vec::new())
                });
            },
            |sidebar| {
                sidebar.heard(
                    Host {
                        projects: None,
                        ..host(Vec::new(), Vec::new())
                    },
                    SystemTime::UNIX_EPOCH,
                );
            },
        ];
        for short in short {
            let mut sidebar = fresh();
            sidebar.hear(own_idle_agent());
            let s = |view| own_view_of("s", 1, view);
            sit(&mut sidebar, &s(1));
            tick(&mut sidebar, 160_000);
            tick(&mut sidebar, 161_000);
            sit(&mut sidebar, &s(3));
            short(&mut sidebar);
            tick(&mut sidebar, 163_000);
            tick(&mut sidebar, 183_000);
            sidebar.hear(own_idle_agent());
            let seen = "   ✓ \u{ec82} fold                 1m";
            assert_eq!(agent_row(&mut sidebar, moment(183_500), true), seen);
        }
    }

    #[test]
    fn a_command_shows_once_it_has_run_a_second() {
        let mut sidebar = fresh();
        let make = in_shell(1, going("make", 159, Front::Working));
        sidebar.hear(host(vec![session("s", 10, vec![make])], Vec::new()));
        let row = |sidebar: &mut Sidebar, now| {
            let _ = sidebar.ticked(now, false);
            seen_at(sidebar, false, 32, 7, now).0[6].clone()
        };
        assert_eq!(
            row(&mut sidebar, moment(160_500)),
            "     \u{e795} lune/main"
        );
        assert!(!sidebar.turning());
        assert_eq!(
            row(&mut sidebar, moment(161_000)),
            "   ⠋ \u{e795} make                 1s"
        );
        assert!(sidebar.turning());
    }

    #[test]
    fn the_git_status_shows_what_starship_shows() {
        let changes = Changes {
            conflicted: 1,
            deleted: 1,
            renamed: 1,
            modified: 1,
            staged: 1,
            untracked: 1,
        };
        let both = Some(Upstream {
            ahead: 1,
            behind: 2,
        });
        assert_eq!(glyphs(&changes, 1, both), "=$✘»!+?⇕");
        assert_eq!(
            glyphs(
                &Changes::default(),
                0,
                Some(Upstream {
                    ahead: 0,
                    behind: 2
                })
            ),
            "⇣"
        );
        assert_eq!(glyphs(&Changes::default(), 0, None), "");
    }

    #[test]
    fn a_branch_shows_the_number_of_its_pull_request_in_its_state_s_colour() {
        let views = ["/w/a", "/w/b", "/w/c", "/w/d"];
        let views = views
            .iter()
            .zip(1..)
            .map(|(cwd, id)| view(id, id, cwd, "fish"))
            .collect();
        let mut host = host(vec![session("s", 10, views)], Vec::new());
        let checkouts = [
            ("/w/a", "main", 0, 126, open_pull_state(false)),
            ("/w/b", "handoff-eng-11259-and-more", 1, 4321, State::Merged),
            ("/w/c", "fold", 0, 7, open_pull_state(true)),
            ("/w/d", "old", 0, 9, State::Closed),
        ];
        for (dir, branch, modified, number, state) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch {
                name,
                upstream: None,
                changed: None,
            };
            let changes = Changes {
                modified,
                ..Changes::default()
            };
            let status = GitStatus::Checkout {
                repository: None,
                head,
                changes,
                stashes: 0,
            };
            host.git_statuses.push(status.to_api(dir));
            let page = owner_s_page(number);
            let pull_request = PullRequest {
                page,
                state,
                stale: false,
            };
            host.pull_requests.push(pull_request.to_api(dir));
        }
        let mut sidebar = fresh();
        sidebar.hear(host);

        let buffer = drawn(&mut sidebar, false, 32, 17);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let number = |y: u16| (0..32).find(|x| buffer[(*x, y)].symbol() == "#");
        let colour = |y: u16| number(y).map(|x| buffer[(x, y)].fg);
        let branches: Vec<String> = [6, 9, 12, 15].into_iter().map(line).collect();
        assert_eq!(
            branches,
            [
                "       main #126",
                "       handoff-eng-11… #4321 !",
                "       fold #7",
                "       old",
            ]
        );
        assert_eq!(colour(6), Some(moonlight::GREEN));
        assert_eq!(colour(9), Some(moonlight::MAUVE));
        assert_eq!(colour(12), Some(moonlight::MUTE));
    }

    #[test]
    fn a_branch_shows_the_lines_changed_on_it() {
        let views = ["/w/a", "/w/b", "/w/c", "/w/d", "/w/e", "/w/f"];
        let views = views
            .iter()
            .zip(1..)
            .map(|(cwd, id)| view(id, id, cwd, "fish"))
            .collect();
        let mut host = host(vec![session("s", 10, views)], Vec::new());
        let checkouts = [
            ("/w/a", "issue-299", Some((120, 34)), 328),
            ("/w/b", "fix", Some((7, 0)), 12),
            ("/w/c", "tidy", Some((0, 5)), 13),
            ("/w/d", "main", None, 14),
            ("/w/e", "handoff-eng-11259-and-more", Some((9, 8)), 4321),
            ("/w/f", "gone", None, 0),
        ];
        for (dir, branch, changed, number) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let changed = changed.and_then(|(added, deleted)| Changed::new(added, deleted));
            let head = Head::Branch {
                name,
                upstream: None,
                changed,
            };
            let status = GitStatus::Checkout {
                repository: None,
                head,
                changes: Changes::default(),
                stashes: 0,
            };
            host.git_statuses.push(status.to_api(dir));
            if number > 0 {
                let (page, state) = (owner_s_page(number), open_pull_state(false));
                let pull_request = PullRequest {
                    page,
                    state,
                    stale: false,
                };
                host.pull_requests.push(pull_request.to_api(dir));
            }
        }
        let mut sidebar = fresh();
        sidebar.hear(host);

        let buffer = drawn(&mut sidebar, false, 34, 23);
        let line = |y: u16| {
            let line: String = (0..33).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let branches: Vec<String> = (0..6).map(|view| line(6 + view * 3)).collect();
        assert_eq!(
            branches,
            [
                "       issue-299 +120 -34 #328",
                "       fix +7 #12",
                "       tidy -5 #13",
                "       main #14",
                "       handoff-eng-… +9 -8 #4321",
                "       gone",
            ]
        );
        let colour = |y: u16, from: u16, mark: &str| {
            let x = (from..34)
                .find(|x| buffer[(*x, y)].symbol() == mark)
                .expect("the mark");
            buffer[(x, y)].fg
        };
        let first = 6;
        let added = (0..34)
            .find(|x| buffer[(*x, first)].symbol() == "+")
            .expect("the mark");
        assert_eq!(buffer[(added, first)].fg, moonlight::GREEN);
        assert_eq!(colour(first, added, "-"), moonlight::RED);
        assert_eq!(colour(first + 3, 0, "+"), moonlight::GREEN);
        assert_eq!(colour(first + 6, 0, "-"), moonlight::RED);
    }

    #[test]
    fn an_open_pull_request_shows_what_its_checks_come_to() {
        use lune_protocol::github::Checks;

        let checks = |pending, failed, passed| {
            Some(Checks {
                pending,
                failed,
                passed,
            })
        };
        let open = |draft, checks| State::Open {
            draft,
            checks,
            mergeability: None,
        };
        let checkouts = [
            ("/w/a", "main", 126, open(false, checks(1, 1, 3))),
            ("/w/b", "fold", 7, open(true, checks(2, 0, 3))),
            (
                "/w/c",
                "handoff-eng-11259-and-more",
                4321,
                open(false, checks(0, 0, 3)),
            ),
            ("/w/d", "none", 8, open(false, checks(0, 0, 0))),
            ("/w/e", "old", 9, open(false, None)),
            ("/w/f", "gone", 10, State::Merged),
            ("/w/g", "late", 11, open(false, checks(1, 0, 2))),
        ];
        let views = checkouts
            .iter()
            .zip(1..)
            .map(|((cwd, ..), id)| view(id, id, cwd, "fish"));
        let mut host = host(vec![session("s", 10, views.collect())], Vec::new());
        for (dir, branch, number, state) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch {
                name,
                upstream: None,
                changed: None,
            };
            let changes = Changes::default();
            let status = GitStatus::Checkout {
                repository: None,
                head,
                changes,
                stashes: 0,
            };
            host.git_statuses.push(status.to_api(dir));
            let (page, stale) = (owner_s_page(number), dir == "/w/g");
            let pull_request = PullRequest { page, state, stale };
            host.pull_requests.push(pull_request.to_api(dir));
        }
        let mut sidebar = fresh();
        sidebar.hear(host);

        let buffer = drawn(&mut sidebar, false, 32, 25);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let branches: Vec<String> = [6, 9, 12, 15, 18, 21, 24].into_iter().map(line).collect();
        assert_eq!(
            branches,
            [
                "       main #126✗",
                "       fold #7●",
                "       handoff-eng-112… #4321✓",
                "       none #8",
                "       old #9",
                "       gone #10",
                "       late #11●",
            ]
        );
        let mark = |y: u16, mark: &str| {
            let x = (0..32)
                .find(|x| buffer[(*x, y)].symbol() == mark)
                .expect("the mark");
            buffer[(x, y)].fg
        };
        assert_eq!(mark(6, "✗"), moonlight::RED);
        assert_eq!(mark(9, "●"), moonlight::YELLOW);
        assert_eq!(mark(12, "✓"), moonlight::GREEN);
        assert_eq!(mark(24, "●"), moonlight::MUTE);
    }

    #[test]
    fn an_open_pull_request_shows_when_it_has_merge_conflicts() {
        use lune_protocol::github::Checks;

        let checks = |pending, failed, passed| {
            Some(Checks {
                pending,
                failed,
                passed,
            })
        };
        let open = |draft, checks, mergeability| State::Open {
            draft,
            checks,
            mergeability,
        };
        let conflicting = Some(Mergeability::Conflicting);
        let mergeable = Some(Mergeability::Mergeable);
        let checkouts = [
            (
                "/w/a",
                "main",
                126,
                open(false, checks(0, 0, 3), conflicting),
                false,
            ),
            (
                "/w/b",
                "fail",
                7,
                open(false, checks(0, 1, 0), conflicting),
                false,
            ),
            (
                "/w/c",
                "wait",
                4321,
                open(false, checks(1, 0, 2), conflicting),
                false,
            ),
            ("/w/d", "draft", 8, open(true, None, conflicting), false),
            (
                "/w/e",
                "clean",
                9,
                open(false, checks(0, 0, 3), mergeable),
                false,
            ),
            ("/w/f", "gone", 10, State::Merged, false),
            (
                "/w/g",
                "late",
                11,
                open(false, checks(0, 0, 2), conflicting),
                true,
            ),
        ];
        let views = checkouts
            .iter()
            .zip(1..)
            .map(|((cwd, ..), id)| view(id, id, cwd, "fish"));
        let mut host = host(vec![session("s", 10, views.collect())], Vec::new());
        for (dir, branch, number, state, stale) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch {
                name,
                upstream: None,
                changed: None,
            };
            let status = GitStatus::Checkout {
                repository: None,
                head,
                changes: Changes::default(),
                stashes: 0,
            };
            host.git_statuses.push(status.to_api(dir));
            let page = owner_s_page(number);
            let pull_request = PullRequest { page, state, stale };
            host.pull_requests.push(pull_request.to_api(dir));
        }
        host.pull_requests.push(api::PullRequest {
            dir: "/w/f".to_owned(),
            state: api::PullRequestState::Merged.into(),
            url: owner_s_page(99).url().to_owned(),
            checks: None,
            stale: false,
            detail: Some(api::pull_request::Detail::Open(api::pull_request::Open {
                draft: false,
                checks: None,
                mergeability: api::Mergeability::Conflicting.into(),
            })),
        });
        let mut sidebar = fresh();
        sidebar.hear(host);

        let buffer = drawn(&mut sidebar, false, 32, 25);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let branches: Vec<String> = [6, 9, 12, 15, 18, 21, 24].into_iter().map(line).collect();
        assert_eq!(
            branches,
            [
                "       main #126✓≠",
                "       fail #7✗≠",
                "       wait #4321●≠",
                "       draft #8≠",
                "       clean #9✓",
                "       gone #10",
                "       late #11✓≠",
            ]
        );
        let mark = |y: u16, mark: &str| {
            let x = (0..32)
                .find(|x| buffer[(*x, y)].symbol() == mark)
                .expect("the mark");
            buffer[(x, y)].fg
        };
        assert_eq!(mark(6, "✓"), moonlight::GREEN);
        assert_eq!(mark(6, "≠"), moonlight::RED);
        assert_eq!(mark(9, "✗"), moonlight::RED);
        assert_eq!(mark(9, "≠"), moonlight::RED);
        assert_eq!(mark(12, "●"), moonlight::YELLOW);
        assert_eq!(mark(12, "≠"), moonlight::RED);
        assert_eq!(mark(15, "≠"), moonlight::RED);
        assert_eq!(mark(24, "✓"), moonlight::MUTE);
        assert_eq!(mark(24, "≠"), moonlight::MUTE);
    }

    #[test]
    fn a_worktree_named_after_its_branch_takes_one_line_folded_or_not() {
        let mut sidebar = fresh();
        let branched = worktree_of(31, "issue-489", "/w/issue-489", &[1]);
        let renamed = worktree_of(32, "tidy", "/w/fix", &[2]);
        let projects = vec![a_project(7, "lune", vec![branched, renamed])];
        let mut host = Host {
            projects: Some(kept(projects)),
            ..host(Vec::new(), Vec::new())
        };
        let name = Said::new("issue-489".to_owned()).expect("a branch's name");
        let head = Head::Branch {
            name,
            upstream: None,
            changed: Changed::new(120, 34),
        };
        let changes = Changes {
            modified: 1,
            ..Changes::default()
        };
        let status = GitStatus::Checkout {
            repository: None,
            head,
            changes,
            stashes: 0,
        };
        host.git_statuses.push(status.to_api("/w/issue-489"));
        let page = PullPage::parse(&LINK.replace("246", "538")).expect("a pull request's page");
        let pull_request = PullRequest {
            page,
            state: open_pull_state(false),
            stale: false,
        };
        host.pull_requests.push(pull_request.to_api("/w/issue-489"));
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        checked_out(
            statuses,
            pulls,
            "/w/fix",
            "fix",
            Some((open_pull_state(false), LINK)),
        );
        sidebar.heard(host, SystemTime::UNIX_EPOCH);

        let one = "  ▾ issue-489 +120 -34 #538  !";
        let lines = screen(&mut sidebar, false, 32, 12);
        let tidy = ["  ▾ tidy", "       fix #246", "     \u{e795} w/fix"];
        assert_eq!(
            lines[5..10],
            [&[one, "     \u{e795} w/issue-489"][..], &tidy[..]].concat()
        );
        let area = Rect::new(0, 0, 32, 12);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
        let columns = |y: u16, number: &str| {
            let cells: Vec<&str> = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            let start = cells
                .windows(number.len())
                .position(|it| it.concat() == number);
            let start = u16::try_from(start.expect(number)).unwrap();
            (start..start + u16::try_from(number.len()).unwrap()).collect::<Vec<_>>()
        };
        assert_eq!(number_spot(&drawn, 5), columns(5, "#538"));
        assert_eq!(number_spot(&drawn, 8), columns(8, "#246"));
        let on = drawn.at(Position::new(columns(8, "#246")[0], 8));
        assert_eq!(on, Some(Hit::Number(link())));
        let project = project_row("lune", 7);
        let view = |id| {
            let worktree = WorktreeId::of(30 + u64::from(id)).unwrap();
            let worktree = WorktreePlace {
                project: project.clone(),
                worktree,
            };
            go_here(Place::View(ViewPlace::new(worktree, ViewKey::terminal(id))))
        };
        let worktree = |id| {
            let worktree = WorktreeId::of(id).unwrap();
            go_here(Place::Worktree(WorktreePlace {
                project: project.clone(),
                worktree,
            }))
        };
        assert_eq!(click(&mut sidebar, 10, 6, area), view(1));
        assert_eq!(click(&mut sidebar, 10, 8, area), worktree(32));
        assert_eq!(click(&mut sidebar, 10, 9, area), view(2));

        sidebar.focus(Keys::Sidebar, test_wall());
        let place = |id| {
            let worktree = WorktreeId::of(id).unwrap();
            Spot::Local(Place::Worktree(WorktreePlace {
                project: project.clone(),
                worktree,
            }))
        };
        sidebar.light(place(31));
        type_in(&mut sidebar, " ");
        sidebar.light(place(32));
        type_in(&mut sidebar, " ");
        let lines = screen(&mut sidebar, false, 32, 12);
        let folded = [
            "  ▸ issue-489 +120 -34 #538  !",
            "  ▸ tidy",
            "       fix #246",
            "",
        ];
        assert_eq!(lines[5..9], folded);
        assert_eq!(
            screen(&mut sidebar, false, 22, 12)[5],
            "  ▸ issue-489 #538 !"
        );
        assert_eq!(screen(&mut sidebar, false, 20, 12)[5], "  ▸ issue-489 #538");
        assert_eq!(screen(&mut sidebar, false, 16, 12)[5], "  ▸ issue-489");
    }

    #[test]
    fn a_cut_leaves_room_for_its_ellipsis() {
        assert_eq!(cut("handoff-eng-11259", 8), "handoff…");
        assert_eq!(cut("short", 8), "short");
        assert_eq!(cut("日本語", 4), "日…");
        assert_eq!(tail("handoff-eng-11259", 8), "…g-11259");
        assert_eq!(tail("short", 8), "short");
        assert_eq!(tail("日本語", 4), "…語");
    }

    #[test]
    fn a_cut_a_tail_and_a_wrap_draw_within_the_cells_of_their_room() {
        let drawn = |text: &str| {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 40, 1));
            let (x, _) = buffer.set_line(0, 0, &Line::raw(text.to_owned()), 40);
            usize::from(x)
        };
        assert_eq!(cut("لا لا لا لا", 8), "لا لا ل…");
        assert_eq!(cut("ｶﾞｷﾞｸﾞｹﾞｺﾞ", 8), "ｶﾞｷﾞｸﾞ…");
        for text in ["لا لا لا لا", "ｶﾞｷﾞｸﾞｹﾞｺﾞ"] {
            assert!(
                drawn(&cut(text, 8)) <= 8,
                "a cut of {text:?} draws past 8 cells"
            );
            assert!(
                drawn(&tail(text, 8)) <= 8,
                "a tail of {text:?} draws past 8 cells"
            );
            for line in wrapped(text, 10, Style::new()) {
                let line = line.to_string();
                assert!(
                    drawn(&line) <= 9,
                    "{line:?} of {text:?} draws past its room"
                );
            }
        }
    }

    #[test]
    fn a_lit_name_fits_its_room() {
        let family = "👩\u{200d}👩\u{200d}👧";
        let texts = [
            "handoff-eng-11259".to_owned(),
            "日本語".to_owned(),
            format!("{family} team"),
            "⚠️ fix".to_owned(),
            "لا لا لا".to_owned(),
            "ｶﾞｷﾞｸﾞ".to_owned(),
            "e\u{301}e\u{301}e\u{301}".to_owned(),
            "a\u{7}b\u{1b}c".to_owned(),
            "x\u{200d}".to_owned(),
        ];
        for text in &texts {
            for room in 0..=20 {
                let (_, width) = lit(text, room, Keep::Start, &[0, 1, 2], Style::new());
                assert!(width <= room, "{text:?} was cut to {width} of {room}");
            }
        }
    }

    #[test]
    fn a_cut_or_a_wrap_goes_by_what_shows_as_one() {
        let family = "👩\u{200d}👩\u{200d}👧";
        assert_eq!(cut("⚠️ fix the build", 5), "⚠️ f…");
        assert_eq!(tail("fix the build ⚠️", 5), "…d ⚠️");
        assert_eq!(cut(&format!("{family} team"), 5), format!("{family} t…"));
        assert_eq!(tail(&format!("team {family}"), 5), format!("…m {family}"));
        assert_eq!(cut("⚠️ fix", 2), "…");
        assert_eq!(tail(&format!("fix {family}"), 2), "…");

        let lines: Vec<String> = wrapped("ab⚠️cd", 5, Style::new())
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(lines, [" ab", " ⚠️c", " d"]);
    }

    fn searched_sessions() -> Vec<Session> {
        vec![
            session(
                "bipa",
                10,
                vec![
                    view(1, 0, "/w/a", "a-p-i-x"),
                    view(2, 1, "/w/b", "dda-bank-slip"),
                    view(3, 2, "/w/c", "pix outflow"),
                ],
            ),
            session("notes", 20, vec![view(1, 0, "/home/me/Documents", "fish")]),
            session(
                "zed",
                30,
                vec![
                    view(1, 0, "/w/q", "htop"),
                    view(2, 1, "/w/r", "z-e-d"),
                    view(3, 2, "/w/s", "less logs"),
                ],
            ),
        ]
    }

    fn searched() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(host(searched_sessions(), Vec::new()));
        sidebar
    }

    fn branched(sidebar: &mut Sidebar, branch: &str) -> Asked {
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        type_in(sidebar, branch);
        sidebar.key(&KeyEvent::from(KeyCode::Enter), moment(160_000))
    }

    fn type_in(sidebar: &mut Sidebar, text: &str) {
        for c in text.chars() {
            sidebar.key(
                &KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                moment(160_000),
            );
        }
    }

    fn rows_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let lines = screen(sidebar, true, 32, 28);
        rows_in(&lines[sessions_from(&lines)..])
    }

    fn rows_in(lines: &[String]) -> Vec<String> {
        let ends =
            |at: usize| lines[at].is_empty() && lines.get(at + 1).is_none_or(String::is_empty);
        let end = (0..lines.len()).find(|at| ends(*at)).unwrap_or(lines.len());
        lines[..end]
            .iter()
            .map(|line| line.trim_end().to_owned())
            .collect()
    }

    fn sessions_from(lines: &[String]) -> usize {
        let header = lines
            .get(HEADER)
            .is_some_and(|line| line.starts_with(" ▾ \u{f108} MAC"));
        let gap = lines
            .get(HEADER + usize::from(header))
            .is_some_and(String::is_empty);
        HEADER + usize::from(header) + usize::from(gap)
    }

    fn bottom(sidebar: &mut Sidebar) -> String {
        screen(sidebar, true, 32, 18)[17].clone()
    }

    fn searching(sidebar: &mut Sidebar) -> bool {
        screen(sidebar, true, 32, 18)
            .iter()
            .any(|line| line.starts_with("─ Search ─"))
    }

    fn search_shown(sidebar: &mut Sidebar) -> Vec<String> {
        screen(sidebar, true, 32, 16)[13..].to_vec()
    }

    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        const PIX: [&str; 5] = [
            " ● bipa",
            "  ▾ Tab #1",
            "       a-p-i-x",
            "  ▾ Tab #3",
            "       pix outflow",
        ];
        const ZED: [&str; 7] = [
            " ● zed",
            "  ▾ Tab #1",
            "     \u{eb31} htop",
            "  ▾ Tab #2",
            "       z-e-d",
            "  ▾ Tab #3",
            "       less logs",
        ];
        let bipa = tab_view_of("bipa", 10, 2, 3);
        let notes = Place::Session(listed("notes", 20));
        let zed = tab_view_of("zed", 30, 1, 2);
        for (typed, rows, best) in [
            ("pix", &PIX[..], bipa),
            (
                "note",
                &[" ● notes", "  ▾ Tab #1", "     \u{e795} me/Documents"][..],
                notes,
            ),
            ("zed", &ZED[..], zed),
        ] {
            let mut sidebar = searched();
            type_in(&mut sidebar, &format!("/{typed}"));
            assert_eq!(rows_shown(&mut sidebar), rows, "{typed}");
            let title = screen(&mut sidebar, true, 32, 16)[0].clone();
            assert!(title.starts_with(" ☾ lune  3 projects"), "{typed}: {title}");
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                sidebar.key(&enter, moment(160_000)),
                go_here(best),
                "{typed}"
            );
        }
    }

    #[test]
    fn a_view_is_found_by_its_worktree_s_name_branch_and_folder() {
        let mut notes = view(1, 0, "/w/a", "nvim a");
        notes.tab_name = "notes".to_owned();
        let deep = view(2, 1, "/home/lorenzo/Projects/misc/lune", "nvim b");
        let branched = view(3, 2, "/w/c", "nvim c");
        let titled = View {
            titled: Some(api::Titled {
                topic: Some("Rename the rows".to_owned()),
            }),
            ..view(4, 3, "/w/fold/review", "claude")
        };
        let seated = agent("s", 4, Status::Idle, None);
        let views = vec![notes, deep, branched, titled];
        let mut host = host(vec![session("s", 10, views)], vec![seated]);
        let name = Said::new("feature/sidebar".to_owned()).expect("a branch's name");
        let head = Head::Branch {
            name,
            upstream: None,
            changed: None,
        };
        let changes = Changes::default();
        let status = GitStatus::Checkout {
            repository: None,
            head,
            changes,
            stashes: 0,
        };
        host.git_statuses.push(status.to_api("/w/c"));
        let found = |typed: &str| {
            let mut sidebar = fresh();
            sidebar.hear(host.clone());
            type_in(&mut sidebar, &format!("/{typed}"));
            rows_shown(&mut sidebar)
        };
        assert_eq!(
            found("notes"),
            [" ● s", "  ▾ notes", "     \u{e6ae} nvim a"]
        );
        assert_eq!(
            found("lune"),
            [" ● s", "  ▾ Tab #2", "     \u{e6ae} nvim b"]
        );
        assert_eq!(found("lnue"), found("lune"));
        let branched = [
            " ● s",
            "  ▾ Tab #3",
            "       feature/sidebar",
            "     \u{e6ae} nvim c",
        ];
        assert_eq!(found("sidebar"), branched);
        let seated = found("fold/review");
        assert!(
            seated.len() == 3 && seated[2].contains("Rename the rows"),
            "{seated:?}"
        );
        assert_eq!(found("zzz"), [" no match"]);
        assert_eq!(found("loren"), [" no match"]);
    }

    #[test]
    fn a_search_opens_a_folded_project_until_it_closes() {
        for close in [Some(KeyCode::Esc), Some(KeyCode::Enter), None] {
            let mut sidebar = searched();
            type_in(&mut sidebar, "jhh/pix");
            let found = [
                " ● bipa",
                "  ▾ Tab #1",
                "       a-p-i-x",
                "  ▾ Tab #3",
                "       pix outflow",
            ];
            assert_eq!(rows_shown(&mut sidebar), found, "{close:?}");
            let asked = if let Some(code) = close {
                sidebar.key(&KeyEvent::new(code, KeyModifiers::NONE), moment(160_000))
            } else {
                sidebar.focus(Keys::Terminal, test_wall());
                Asked::Nothing
            };
            let pix = tab_view_of("bipa", 10, 2, 3);
            let went = if close == Some(KeyCode::Enter) {
                go_here(pix)
            } else {
                Asked::Nothing
            };
            assert_eq!(asked, went, "{close:?}");
            assert_eq!(
                rows_shown(&mut sidebar)[..3],
                [" ▸ bipa", "", " ● notes"],
                "{close:?}"
            );
            assert!(!searching(&mut sidebar), "{close:?}");
        }
    }

    #[test]
    fn going_to_a_view_found_in_lune_s_folded_project_unfolds_it() {
        let mut sidebar = fresh();
        sidebar.hear(host(owned(searched_sessions()), Vec::new()));
        type_in(&mut sidebar, "jhh");
        assert_eq!(rows_shown(&mut sidebar)[..3], [" ▸ bipa", "", " ● notes"]);
        type_in(&mut sidebar, "/pix");
        let pix = tab_view_of("bipa", 1, 2, 3);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(pix.clone()));
        sit(&mut sidebar, &pix);
        let rows = rows_shown(&mut sidebar);
        assert_eq!(
            rows[..4],
            [
                " ● bipa",
                "  ▸ Tab #1",
                "  ▾ Tab #2",
                "       dda-bank-slip"
            ]
        );
        assert_eq!(rows[5], "▌      pix outflow");
    }

    #[test]
    fn what_is_typed_shows_where_the_keys_were() {
        let mut sidebar = searched();
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        let keys = " <ENTER> - Go, <ESC> - Close";
        type_in(&mut sidebar, "/");
        let rule = format!("─ Search {}", "─".repeat(22));
        assert_eq!(search_shown(&mut sidebar), [rule.as_str(), " _", keys]);
        type_in(&mut sidebar, "pixjh");
        let rule = format!("─ Search {} 0/7 ─", "─".repeat(16));
        assert_eq!(search_shown(&mut sidebar), [rule.as_str(), " pixjh_", keys]);
        assert_eq!(rows_shown(&mut sidebar), [" no match"]);
        sidebar.key(&backspace, moment(160_000));
        sidebar.key(&backspace, moment(160_000));
        let found = [
            " ● bipa",
            "  ▾ Tab #1",
            "       a-p-i-x",
            "  ▾ Tab #3",
            "       pix outflow",
        ];
        assert_eq!(rows_shown(&mut sidebar), found);
        assert!(search_shown(&mut sidebar)[0].ends_with(" 2/7 ─"));
        for _ in 0..4 {
            sidebar.key(&backspace, moment(160_000));
        }
        assert!(!searching(&mut sidebar));
        type_in(&mut sidebar, "/a-very-long-search-for-something");
        assert_eq!(
            search_shown(&mut sidebar)[1],
            " …ry-long-search-for-something_"
        );
    }

    #[test]
    fn a_click_on_a_key_under_the_search_presses_it_while_it_is_open() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "/pix");
        let area = Rect::new(0, 0, 32, 16);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
        let keys: String = (0..31).map(|x| buffer[(x, 15)].symbol()).collect();
        let at = |key: &str| Position::new(u16::try_from(keys.find(key).unwrap()).unwrap(), 15);
        let (enter, esc) = (KeyEvent::from(KeyCode::Enter), KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            drawn.at(at("<ENTER>")),
            Some(Hit::Keys(Keyed::Search, enter))
        );
        assert_eq!(drawn.at(at("<ESC>")), Some(Hit::Keys(Keyed::Search, esc)));
        assert_eq!(drawn.at(Position::new(2, 14)), None);
        assert_eq!(
            sidebar.keys_clicked(Keyed::Search, &esc, moment(160_000)),
            Asked::Nothing
        );
        assert!(!searching(&mut sidebar));
        assert_eq!(
            sidebar.keys_clicked(Keyed::Search, &enter, moment(160_000)),
            Asked::Nothing
        );
        assert!(!searching(&mut sidebar));
    }

    #[test]
    fn the_search_has_no_keys_under_a_box() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "/pix");
        let enter = KeyEvent::from(KeyCode::Enter);
        let area = Rect::new(0, 0, 32, 16);
        let rule = format!("─ Search {} 2/7 ─", "─".repeat(16));
        sidebar.focus(Keys::Lent, test_wall());
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        sidebar.draw_rows(&mut buffer, area, test_wall());
        let line = |y: u16| (0..31).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        assert_eq!(
            [line(14).trim_end(), line(15).trim_end()],
            [rule.as_str(), " pix_"]
        );
        let clicked = sidebar.keys_clicked(Keyed::Search, &enter, moment(160_000));
        assert_eq!(clicked, Asked::Nothing);
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(search_shown(&mut sidebar)[1], " pix_");
    }

    #[test]
    fn a_search_lights_what_it_found_of_each_row() {
        let mut sidebar = searched();
        type_in(&mut sidebar, "/zed");
        let zed = [
            " ● zed",
            "  ▾ Tab #1",
            "     \u{eb31} htop",
            "  ▾ Tab #2",
            "       z-e-d",
        ];
        assert_eq!(rows_shown(&mut sidebar)[..5], zed);
        let zed = u16::try_from(sessions_from(&screen(&mut sidebar, true, 32, 16))).unwrap();
        let now = moment(160_000);
        let buffer = drawn_at(&mut sidebar, true, 32, 16, now);
        let marked = |y: u16, mark: fn(&Cell) -> bool| -> String {
            let cells = (0..31).map(|x| &buffer[(x, y)]);
            cells.filter(|cell| mark(cell)).map(Cell::symbol).collect()
        };
        let lit = |cell: &Cell| cell.fg == ACCENT && cell.modifier.contains(Modifier::BOLD);
        assert_eq!(marked(zed, lit), "zed");
        assert_eq!(marked(zed + 2, lit), "");
        assert_eq!(marked(zed + 4, lit), "zed");
    }

    #[test]
    fn what_the_cut_took_lights_nothing() {
        let (plain, hit) = (
            Style::new(),
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
        );
        let (spans, wide) = lit("pix outflow", 8, Keep::Start, &[0, 7], plain);
        assert_eq!(
            spans,
            [Span::styled("p", hit), Span::styled("ix out…", plain)]
        );
        assert_eq!(wide, 8);
        let (spans, wide) = lit("/w/lune/main", 8, Keep::End, &[4, 10], plain);
        let (cut, n) = (Span::styled("…ne/ma", plain), Span::styled("n", plain));
        assert_eq!(spans, [cut, Span::styled("i", hit), n]);
        assert_eq!(wide, 8);
    }

    #[test]
    fn a_hit_on_a_text_in_the_accent_is_underlined() {
        let accent = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
        let (spans, _) = lit("zed", 8, Keep::Start, &[0], accent);
        let underlined = accent.add_modifier(Modifier::UNDERLINED);
        assert_eq!(
            spans,
            [Span::styled("z", underlined), Span::styled("ed", accent)]
        );
    }

    fn char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    const LINK: &str = "https://github.com/owner/repo/pull/246";

    fn link() -> Link {
        Link::parse(LINK).expect("a link")
    }

    fn owner_s_page(number: u32) -> PullPage {
        let page = PullPage::parse(&format!("https://github.com/owner/repo/pull/{number}"));
        page.expect("a pull request's page")
    }

    fn open_pull_state(draft: bool) -> State {
        State::Open {
            draft,
            checks: None,
            mergeability: None,
        }
    }

    fn checked_out(
        git_statuses: &mut Vec<api::GitStatus>,
        pull_requests: &mut Vec<api::PullRequest>,
        dir: &str,
        branch: &str,
        pull: Option<(State, &str)>,
    ) {
        let name = Said::new(branch.to_owned()).expect("a branch's name");
        let head = Head::Branch {
            name,
            upstream: None,
            changed: None,
        };
        let changes = Changes::default();
        let status = GitStatus::Checkout {
            repository: None,
            head,
            changes,
            stashes: 0,
        };
        git_statuses.push(status.to_api(dir));
        if let Some((state, url)) = pull {
            let page = PullPage::parse(url).expect("a pull request's page");
            let pull_request = PullRequest {
                page,
                state,
                stale: false,
            };
            pull_requests.push(pull_request.to_api(dir));
        }
    }

    fn pulls() -> Sidebar {
        pulls_with(Vec::new())
    }

    fn pulls_with(more: Vec<View>) -> Sidebar {
        let dirs = ["/w/a", "/w/b", "/w/c", "/w/d"];
        let views = dirs
            .iter()
            .zip(1..)
            .map(|(cwd, id)| view(id, id, cwd, "fish"));
        let mut host = host(
            vec![session("s", 10, views.chain(more).collect())],
            Vec::new(),
        );
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        let closed = "https://github.com/owner/repo/pull/9";
        checked_out(
            statuses,
            pulls,
            "/w/a",
            "fix",
            Some((open_pull_state(false), LINK)),
        );
        let off_github = "https://ghe.example/owner/repo/pull/7";
        checked_out(
            statuses,
            pulls,
            "/w/b",
            "old",
            Some((open_pull_state(false), off_github)),
        );
        checked_out(
            statuses,
            pulls,
            "/w/c",
            "gone",
            Some((State::Closed, closed)),
        );
        checked_out(statuses, pulls, "/w/d", "none", None);
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar
    }

    fn number_spot(drawn: &Drawn, y: u16) -> Vec<u16> {
        let hits = |x: &u16| matches!(drawn.at(Position::new(*x, y)), Some(Hit::Number(_)));
        (0..64).filter(hits).collect()
    }

    #[test]
    fn an_issue_shows_in_the_notes_as_it_is_filed_and_then_what_came_of_it() {
        let mut sidebar = two_sessions();
        let notes = |sidebar: &mut Sidebar, at| {
            let (lines, _) = seen_at(sidebar, false, 32, 19, at);
            let first = lines
                .iter()
                .rposition(String::is_empty)
                .map_or(0, |at| at + 1);
            lines[first..].to_vec()
        };
        assert!(!sidebar.turning());
        sidebar.filing(1, "lune");
        sidebar.filing(2, "zed");
        assert!(sidebar.turning());
        let now = moment(160_000);
        assert_eq!(
            notes(&mut sidebar, now),
            [" ⠋ Filing an issue in lune…", " ⠋ Filing an issue in zed…",]
        );
        let url = "https://github.com/o/lune/issues/355";
        let filed = Filed::page(url, "A red button").unwrap();
        sidebar.filed(1, "lune", &Ok(filed));
        sidebar.filed(
            2,
            "zed",
            &Err(Unfiled::Refused("gh: no GitHub remote".to_owned())),
        );
        assert!(!sidebar.turning());
        let later = moment(3_600_000);
        let buffer = drawn_at(&mut sidebar, false, 32, 16, later);
        let lines = notes(&mut sidebar, later);
        assert_eq!(
            lines,
            [
                " issue in zed: gh: no GitHub r",
                " emote; i has your words again",
                " filed #355 in lune: A red but",
                " ton; link copied",
            ]
        );
        let note = u16::try_from(16 - 2).unwrap();
        assert_eq!(buffer[(1, note)].fg, NAME);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.key(&char('j'), later);
        assert_eq!(
            (sidebar.filed.as_ref(), sidebar.notice.as_ref()),
            (None, None)
        );
        sidebar.filing(3, "lune");
        let stuck = Unfiled::InDoubt("gh: it did not answer in time".to_owned());
        sidebar.filed(3, "lune", &Err(stuck));
        assert_eq!(
            notes(&mut sidebar, later),
            [
                " issue in lune may be filed: g",
                " h: it did not answer in time;",
                "  i has your words, look first",
            ]
        );
    }

    #[test]
    fn y_p_asks_for_the_highlighted_row_s_link_and_its_keys_go() {
        let mut sidebar = pulls();
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('y'), now), Asked::Nothing);
        let _ = sidebar.ticked(now + PAUSE, true);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now + PAUSE);
        let rule = format!("─ Yank {}", "─".repeat(24));
        assert_eq!(lines[22..], [rule, " p  Yank the PR link".to_owned()]);

        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now);
        assert!(
            !lines.iter().any(|line| line.starts_with("─ Yank")),
            "the yank keys go"
        );
        assert!(lines[0].starts_with(" ☾ lune"), "the title stays");
        assert_eq!(sidebar.copied, None, "the note waits for the copy");
    }

    #[test]
    fn a_copy_s_note_names_what_went_until_three_seconds_pass_or_a_key() {
        let mut sidebar = pulls();
        let now = moment(160_000);
        sidebar.copied(Copied::Pull(link().number()), now.steady);
        let buffer = drawn_at(&mut sidebar, true, 32, 24, now);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now);
        let note = lines
            .iter()
            .position(|line| line == " copied #246")
            .expect("the note");
        assert_eq!(buffer[(1, u16::try_from(note).unwrap())].fg, NAME);

        let shown = |sidebar: &mut Sidebar, millis| {
            let at = moment(millis);
            let _ = sidebar.ticked(at, true);
            seen_at(sidebar, false, 32, 24, at)
                .0
                .contains(&" copied #246".to_owned())
        };
        assert!(shown(&mut sidebar, 162_999));
        assert!(!shown(&mut sidebar, 163_000));

        sidebar.copied(
            Copied::text(&excerpt("cargo nextest run --workspace\nmore")),
            now.steady,
        );
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now);
        let said = " copied 2 lines \"cargo nexte…\"";
        assert!(
            lines.iter().any(|line| line == said),
            "the note fits on one line: {lines:#?}"
        );
        sidebar.key(&char('j'), now);
        assert_eq!(sidebar.copied, None);
    }

    #[test]
    fn a_copy_s_note_goes_after_three_seconds_though_the_wall_clock_steps_back() {
        let mut sidebar = pulls();
        let now = moment(160_000);
        sidebar.copied(Copied::Pull(link().number()), now.steady);
        let shown = |sidebar: &mut Sidebar, at: Moment| {
            let _ = sidebar.ticked(at, true);
            seen_at(sidebar, false, 32, 24, at)
                .0
                .contains(&" copied #246".to_owned())
        };
        let gone = stepped_back(now, COPIED_SHOWS);
        assert!(shown(&mut sidebar, gone - Duration::from_millis(1)));
        assert!(!shown(&mut sidebar, gone));
    }

    #[test]
    #[should_panic(expected = "steady clock does not move backwards")]
    fn ticking_before_a_copy_s_steady_time_panics() {
        let mut sidebar = pulls();
        let now = moment(160_000);
        sidebar.copied(Copied::Pull(link().number()), now.steady);
        let _ = sidebar.ticked(now - Duration::from_millis(1), true);
    }

    #[test]
    fn y_waits_for_one_key_and_esc_only_drops_it() {
        let mut sidebar = pulls();
        let one = Spot::Local(tab_view_of("s", 10, 1, 1));
        sidebar.light(one.clone());
        let now = moment(160_000);
        let esc = KeyEvent::from(KeyCode::Esc);
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&esc, now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, now), Asked::Leave);

        sidebar.key(&char('y'), now);
        sidebar.key(&char('j'), now);
        assert_eq!(lit_place(&sidebar), Some(tab_view_of("s", 10, 2, 2)));
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);

        sidebar.light(one);
        let area = Rect::new(0, 0, 32, 24);
        sidebar.key(&char('y'), now);
        sidebar.mouse_rows(mouse(MouseEventKind::ScrollUp, 10, 4), area, test_wall());
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);

        sidebar.key(&char('y'), now);
        sidebar.mouse_rows(mouse(MouseEventKind::Moved, 10, 4), area, test_wall());
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
    }

    #[test]
    fn a_click_on_y_then_on_p_copies() {
        let mut sidebar = pulls();
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        let now = moment(160_000);
        sidebar.key(&char('?'), now);
        assert_eq!(
            sidebar.keys_clicked(Keyed::Legend, &char('y'), now),
            Asked::Nothing
        );
        let clicked = clicks(&mut sidebar, 18);
        assert_eq!(
            clicked[1],
            (" p  Yank the PR link".to_owned(), Some(on_legend('p')))
        );
        let copied = sidebar.keys_clicked(Keyed::Legend, &char('p'), now);
        assert_eq!(copied, Asked::Copy(link()));
    }

    #[test]
    fn the_keys_only_leave_a_note_on_a_row_with_no_link() {
        let none = "no pull request on this row";
        let old = "#7: luned says no link of it on github.com";
        let rows = [
            (Spot::Local(tab_view_of("s", 10, 2, 2)), old),
            (Spot::Local(tab_view_of("s", 10, 3, 3)), none),
            (Spot::Local(tab_view_of("s", 10, 4, 4)), none),
            (Spot::Local(Place::Session(listed("s", 10))), none),
            (Spot::Machine(Machine::Local), none),
        ];
        let now = moment(160_000);
        for (spot, note) in rows {
            let mut sidebar = pulls();
            sidebar.light(spot);
            sidebar.key(&char('y'), now);
            assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing, "{note}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
            assert_eq!(sidebar.copied, None);
            let _ = sidebar.clear_notice();
            assert_eq!(open_pull(&mut sidebar, now), Asked::Nothing, "{note}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
        }
    }

    fn pull_said(said: &str) -> Sidebar {
        let views = vec![view(1, 1, "/w/a", "fish")];
        let mut host = host(vec![session("s", 10, views)], Vec::new());
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        checked_out(
            statuses,
            pulls,
            "/w/a",
            "fix",
            Some((open_pull_state(false), said)),
        );
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        sidebar
    }

    #[test]
    fn a_page_off_github_stays_on_the_row() {
        let now = moment(160_000);
        let mut sidebar = pull_said("https://ghe.example/owner/repo/pull/5");
        let area = Rect::new(0, 0, 32, 24);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, now.system);
        let line: String = (0..31).map(|x| buffer[(x, 6)].symbol()).collect();
        assert!(line.contains("fix #5"), "{line}");
        assert_eq!(number_spot(&drawn, 6), [] as [u16; 0]);

        let note = "#5: luned says no link of it on github.com";
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some(note));
        assert_eq!(sidebar.copied, None);
        assert_eq!(open_pull(&mut sidebar, now), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some(note));
    }

    #[test]
    fn a_page_is_taken_as_lune_spells_it() {
        let now = moment(160_000);
        let mut sidebar = pull_said("https://www.github.com/owner/repo/pull/5");
        let Asked::Browse(link) = open_pull(&mut sidebar, now) else {
            panic!("the row's link opens");
        };
        assert_eq!(link.url(), "https://github.com/owner/repo/pull/5");
    }

    fn open_pull(sidebar: &mut Sidebar, now: Moment) -> Asked {
        assert_eq!(
            sidebar.key(&char('o'), now),
            Asked::Nothing,
            "o opens a group"
        );
        sidebar.key(&char('p'), now)
    }

    #[test]
    fn o_p_and_y_p_take_another_machine_s_link_too() {
        let mut sidebar = pulls();
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        let now = moment(160_000);
        assert_eq!(open_pull(&mut sidebar, now), Asked::Browse(link()));
        assert_eq!(sidebar.copied, None, "what opens shows");

        let far = "https://github.com/owner/repo/pull/8";
        let far_link = Link::parse(far).expect("a link");
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let (statuses, pulls) = (&mut there.git_statuses, &mut there.pull_requests);
        checked_out(
            statuses,
            pulls,
            "/w/b",
            "fix",
            Some((open_pull_state(false), far)),
        );
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        let place = view_place("main", 20, 0, 1);
        let place = Place::View(place);
        sidebar.light(Spot::Remote(nixos(), place));
        assert_eq!(
            open_pull(&mut sidebar, now),
            Asked::Browse(far_link.clone())
        );
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(far_link));
    }

    #[test]
    fn every_key_of_a_row_group_asks_or_says_why_not() {
        let now = moment(160_000);
        let grouped = Action::row().filter(|(_, path)| path.len() == 2);
        for (action, path) in grouped {
            let mut sidebar = pulls();
            sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
            assert_eq!(
                sidebar.key(&char(path[0]), now),
                Asked::Nothing,
                "{path:?} opens a group"
            );
            let asked = sidebar.key(&char(path[1]), now);
            let said = asked != Asked::Nothing || sidebar.notice.is_some();
            assert!(said, "{action:?} on {path:?} does nothing");
        }
    }

    #[test]
    fn o_w_asks_for_the_worktrees_not_open_of_the_row_s_project_on_its_machine() {
        let mut sidebar = pulls();
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        let now = moment(160_000);
        let opening = |sidebar: &mut Sidebar| {
            assert_eq!(
                sidebar.key(&char('o'), now),
                Asked::Nothing,
                "o opens a group"
            );
            match sidebar.key(&char('w'), now) {
                Asked::OpenWorktree { on, project } => (on, project.listed().name().to_string()),
                asked => panic!("o w asks for no worktree to open: {asked:?}"),
            }
        };
        assert_eq!(opening(&mut sidebar), (Machine::Local, "s".to_owned()));

        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let place = view_place("main", 20, 0, 1);
        sidebar.light(Spot::Remote(nixos(), Place::View(place)));
        assert_eq!(
            opening(&mut sidebar),
            (Machine::Remote(nixos()), "main".to_owned())
        );
    }

    #[test]
    fn the_number_of_a_pull_request_is_a_spot_of_its_own() {
        let views = ["/w/e", "/w/f"]
            .iter()
            .zip(5..)
            .map(|(cwd, id)| view(id, id, cwd, "fish"));
        let mut sidebar = pulls_with(views.collect());
        let mut host = sidebar.host.clone().unwrap();
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        let long = "handoff-eng-11259-and-more";
        let (merged, draft) = (LINK.replace("246", "4321"), LINK.replace("246", "12"));
        checked_out(
            statuses,
            pulls,
            "/w/e",
            long,
            Some((State::Merged, &merged)),
        );
        checked_out(
            statuses,
            pulls,
            "/w/f",
            "日本語の枝の名前が長い",
            Some((open_pull_state(true), &draft)),
        );
        sidebar.hear(host);

        for keys in [Keys::Terminal, Keys::Sidebar] {
            sidebar.focus(keys, test_wall());
            let area = Rect::new(0, 0, 32, 32);
            let mut buffer = Buffer::empty(area);
            sidebar.place_rows(area);
            let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
            let columns = |y: u16, number: &str| {
                let cells: Vec<&str> = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
                let start = cells
                    .windows(number.len())
                    .position(|it| it.concat() == number);
                let start = u16::try_from(start.expect(number)).unwrap();
                (start..start + u16::try_from(number.len()).unwrap()).collect::<Vec<_>>()
            };
            assert_eq!(number_spot(&drawn, 6), columns(6, "#246"));
            for (y, why) in [
                (9, "old: no link"),
                (12, "gone: closed"),
                (15, "none: no number"),
            ] {
                assert!(number_spot(&drawn, y).is_empty(), "{why}");
            }
            assert_eq!(number_spot(&drawn, 18), columns(18, "#4321"));
            assert_eq!(number_spot(&drawn, 21), columns(21, "#12"));
            let on = drawn.at(Position::new(columns(6, "#246")[0], 6));
            assert_eq!(on, Some(Hit::Number(link())));
        }
    }

    #[test]
    fn a_number_out_of_sight_takes_no_click() {
        let mut sidebar = pulls();
        sidebar.focus(Keys::Terminal, test_wall());
        let area = Rect::new(0, 0, 32, 8);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
        assert_eq!(number_spot(&drawn, 6).len(), "#246".len());

        sidebar.set_top(5);
        let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
        assert_eq!((0..8).flat_map(|y| number_spot(&drawn, y)).count(), 0);

        sidebar.set_top(0);
        sidebar.notify("something went wrong".to_owned());
        let short = Rect::new(0, 0, 32, 7);
        sidebar.place_rows(short);
        let drawn = sidebar.draw_rows(&mut Buffer::empty(short), short, test_wall());
        assert_eq!(number_spot(&drawn, 6), [] as [u16; 0]);
        let _ = sidebar.clear_notice();

        let area = Rect::new(0, 0, 32, 24);
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 5), area, test_wall());
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 7), area, test_wall()),
            Asked::Take
        );
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, test_wall());
        assert_eq!((0..24).flat_map(|y| number_spot(&drawn, y)).count(), 0);
    }

    #[test]
    fn a_note_no_longer_so_goes_and_leaves_any_other() {
        let mut sidebar = fresh();
        sidebar.notify("what goes wrong is kept in no log".to_owned());
        let _ = sidebar.unsay("no one reads");
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("what goes wrong is kept in no log")
        );
        sidebar.notify("no one reads".to_owned());
        let _ = sidebar.unsay("no one reads");
        assert_eq!(sidebar.notice, None);
    }

    #[test]
    fn a_click_on_a_number_copies_and_does_nothing_else() {
        let mut sidebar = pulls();
        sidebar.focus(Keys::Terminal, test_wall());
        let here = sidebar.lit_spot().cloned();
        let now = moment(160_000);
        sidebar.notify("what went wrong before".to_owned());
        assert_eq!(sidebar.number_clicked(link()), Asked::Copy(link()));
        assert_eq!(sidebar.lit_spot().cloned(), here);
        assert_eq!(sidebar.keys, Keys::Terminal);
        assert_eq!(sidebar.notice, None);

        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(tab_view_of("s", 10, 1, 1)));
        sidebar.key(&char('y'), now);
        sidebar.number_clicked(link());
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);
    }
    fn clicks(sidebar: &mut Sidebar, from: u16) -> Vec<(String, Option<Hit>)> {
        let now = moment(160_000);
        let _ = sidebar.ticked(now, true);
        let area = Rect::new(0, 0, 32, 20);
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        let drawn = sidebar.draw_rows(&mut buffer, area, now.system);
        let text = |y| (0..31).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        let click = |y| (text(y).trim_end().to_owned(), drawn.at(Position::new(4, y)));
        (from..20).map(click).collect()
    }

    fn on_legend(c: char) -> Hit {
        Hit::Keys(Keyed::Legend, char(c))
    }

    fn on_menu(code: KeyCode) -> Hit {
        Hit::Menu(1, Choice::Key(KeyEvent::from(code)))
    }

    #[test]
    fn a_click_on_the_legend_or_the_menu_under_it_presses_its_key() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        sidebar.key(&char('?'), moment(160_000));
        let clicked = clicks(&mut sidebar, 0);
        assert!(
            clicked[..6].iter().all(|(_, hit)| hit.is_none()),
            "{clicked:?}"
        );
        let legend: Vec<(&str, Option<Hit>)> = clicked[6..]
            .iter()
            .map(|(text, hit)| (text.as_str(), hit.clone()))
            .collect();
        let rule = format!("─ lune {}", "─".repeat(24));
        assert_eq!(
            legend,
            [
                (rule.as_str(), None),
                (" /  Search", Some(on_legend('/'))),
                (" u  Unread", Some(on_legend('u'))),
                (" v  Reorder", Some(on_legend('v'))),
                (" o  Open ▸", Some(on_legend('o'))),
                (" y  Yank ▸", Some(on_legend('y'))),
                (" q  Quit lune", Some(on_legend('q'))),
                (" a  Ask", Some(on_legend('a'))),
                (" c  Skills ▸", Some(on_legend('c'))),
                (" n  New ▸", Some(on_legend('n'))),
                (" s  Show as panes", Some(on_legend('s'))),
                (" r  Rename the view…", Some(on_legend('r'))),
                (" d  Close ▸", Some(on_legend('d'))),
                (" b  Brief ▸", Some(on_legend('b'))),
            ]
        );
        sidebar.keys_clicked(Keyed::Legend, &char('n'), moment(160_000));
        let clicked = clicks(&mut sidebar, 12);
        let menu: Vec<(&str, Option<Hit>)> = clicked
            .iter()
            .map(|(text, hit)| (text.as_str(), hit.clone()))
            .collect();
        assert_eq!(
            menu,
            [
                ("─ New ───────────── lune/main ─", None),
                (" v  new view", Some(on_menu(KeyCode::Char('v')))),
                (" h  new welcome view", Some(on_menu(KeyCode::Char('h')))),
                (" b  new browser view…", Some(on_menu(KeyCode::Char('b')))),
                (" r  new review view", Some(on_menu(KeyCode::Char('r')))),
                (" w  new worktree…", Some(on_menu(KeyCode::Char('w')))),
                (" m  new machine…", Some(on_menu(KeyCode::Char('m')))),
                (
                    " <BS> - Back, <ESC> - Close",
                    Some(on_menu(KeyCode::Backspace))
                ),
            ]
        );
    }

    #[test]
    fn q_quits_with_no_row_to_act_on_and_from_the_menu_but_is_typed_in_a_search() {
        let now = moment(160_000);
        let mut unheard = fresh();
        unheard.focus(Keys::Sidebar, test_wall());
        assert_eq!(unheard.key(&char('q'), now), Asked::Quit);
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.key(&char('q'), now), Asked::Quit);
        sidebar.key(&char('m'), now);
        assert_eq!(sidebar.key(&char('q'), now), Asked::Quit);
        sidebar.key(&char('/'), now);
        assert_eq!(sidebar.key(&char('q'), now), Asked::Nothing);
    }

    #[test]
    fn q_closes_a_group_the_list_of_all_keys_and_a_move_as_esc_does_but_quits_from_the_legend() {
        let now = moment(160_000);
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        for opens in ["o", "y", "g", "??"] {
            for key in opens.chars() {
                sidebar.key(&char(key), now);
            }
            assert_eq!(sidebar.key(&char('q'), now), Asked::Nothing, "{opens}");
            assert_eq!(
                sidebar.key(&char('q'), now),
                Asked::Quit,
                "{opens} is closed"
            );
        }
        sidebar.key(&char('?'), now);
        assert_eq!(
            sidebar.key(&char('q'), now),
            Asked::Quit,
            "the legend lists q"
        );
        let (mut moving, _) = bipa_selected();
        assert_eq!(moving.key(&char('q'), now), Asked::Nothing);
        assert!(matches!(moving.mode, Mode::Idle), "q ends the move");
        assert_eq!(moving.key(&char('q'), now), Asked::Quit);
    }

    #[test]
    fn a_click_on_the_legend_once_the_menu_took_its_place_does_nothing() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        sidebar.key(&char('?'), moment(160_000));
        sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        let group = clicks(&mut sidebar, 18);
        assert_eq!(group[0].1, Some(on_menu(KeyCode::Char('d'))), "{group:?}");
        let clicked = sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        assert_eq!(clicked, Asked::Nothing);
        assert_eq!(clicks(&mut sidebar, 18), group);
        let chosen = sidebar.chose(1, Choice::Key(char('d')), moment(160_000));
        assert_eq!(chosen, Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 20);
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(" Close the view?")),
            "{lines:#?}"
        );
    }

    fn shows_at(sidebar: &mut Sidebar, at: Moment, start: &str) -> bool {
        let _ = sidebar.ticked(at, true);
        seen_at(sidebar, true, 32, 20, at)
            .0
            .iter()
            .any(|line| line.starts_with(start))
    }

    #[test]
    fn a_group_shows_once_a_pause_passes_and_never_when_typed_fast() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        let now = moment(160_000);
        sidebar.key(&char('d'), now);
        assert_eq!(sidebar.due(now), Some(now.steady + PAUSE));
        let before = now + PAUSE - Duration::from_millis(1);
        assert!(!shows_at(&mut sidebar, before, "─ Close ─"));
        assert!(!shows_at(&mut sidebar, before, " d  Close ▸"));
        assert!(shows_at(&mut sidebar, now + PAUSE, "─ Close ─"));
        assert_eq!(sidebar.due(now + PAUSE), None);

        sidebar.close_menu();
        sidebar.step(1);
        sidebar.key(&char('d'), now);
        let fast = now + Duration::from_millis(50);
        let act = Act::View(view_key(10, 2));
        assert_eq!(sidebar.key(&char('d'), fast), close_here(act, 1));
        assert!(sidebar.open().is_none());
        let later = now + 10 * PAUSE;
        assert!(!shows_at(&mut sidebar, later, "─ Close ─"));
        assert!(!shows_at(&mut sidebar, later, " d  Close ▸"));
        assert_eq!(sidebar.due(later), None);
    }

    fn stepped_back(now: Moment, passed: Duration) -> Moment {
        Moment {
            system: now.system - Duration::from_mins(1),
            steady: now.steady + passed,
        }
    }

    #[test]
    fn a_group_shows_once_a_pause_passes_though_the_wall_clock_steps_back() {
        let now = moment(160_000);
        let shows = stepped_back(now, PAUSE);
        let before = shows - Duration::from_millis(1);
        for (pressed, header) in [('d', "─ Close ─"), ('g', "─ Go "), ('y', "─ Yank ")] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            step_onto_its_first_view(&mut sidebar);
            sidebar.key(&char(pressed), now);
            assert_eq!(sidebar.due(before), Some(shows.steady), "{pressed}");
            assert!(!shows_at(&mut sidebar, before, header), "{pressed}");
            assert!(shows_at(&mut sidebar, shows, header), "{pressed}");
            assert_eq!(sidebar.due(shows), None, "{pressed}");
        }
    }

    #[test]
    fn a_lone_g_a_selection_and_a_move_show_their_keys() {
        let mut sidebar = two_to_move();
        let earlier = moment(160_000) - PAUSE;
        let esc = || Some(Hit::Keys(Keyed::Legend, KeyEvent::from(KeyCode::Esc)));
        sidebar.key(&char('g'), earlier);
        assert_eq!(
            clicks(&mut sidebar, 18),
            [
                (format!("─ Go {}", "─".repeat(26)), None),
                (" g  First row".to_owned(), Some(on_legend('g'))),
            ]
        );
        sidebar.key(&char('g'), earlier);

        sidebar.key(&char('v'), earlier);
        assert_eq!(
            clicks(&mut sidebar, 15),
            [
                (format!("─ Reorder {}", "─".repeat(21)), None),
                (" j k  Grow".to_owned(), None),
                (" J K  Move".to_owned(), None),
                (" v    Project, then done".to_owned(), Some(on_legend('v'))),
                (" Esc  Done".to_owned(), esc()),
            ]
        );
    }

    #[test]
    fn question_mark_opens_the_legend_and_the_next_key_closes_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let now = moment(160_000);
        let legend = " b  Brief ▸";
        assert!(!shows_at(&mut sidebar, now, legend));
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(shows_at(&mut sidebar, now, legend));
        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), now),
            Asked::Nothing
        );
        assert!(!shows_at(&mut sidebar, now, legend));
        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), now),
            Asked::Leave
        );

        let here = sidebar.lit_spot().cloned();
        sidebar.key(&char('?'), now);
        sidebar.key(&char('j'), now);
        assert_ne!(sidebar.lit_spot().cloned(), here);
        assert!(!shows_at(&mut sidebar, now, legend));

        sidebar.key(&char('?'), now);
        sidebar.key(&char('d'), now);
        assert_eq!(sidebar.due(now), None);
        assert!(shows_at(&mut sidebar, now, "─ Close ─"));
        sidebar.close_menu();
        sidebar.key(&char('?'), now);
        sidebar.key(&char('y'), now);
        assert!(shows_at(&mut sidebar, now, " p  Yank the PR link"));
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);

        sidebar.key(&char('?'), now);
        let unfocused = screen(&mut sidebar, false, 32, 20);
        assert!(
            !unfocused.iter().any(|line| line == legend),
            "{unfocused:#?}"
        );
        assert!(!shows_at(&mut sidebar, now, legend));
    }

    #[test]
    fn question_mark_in_the_legend_lists_every_key_and_esc_closes_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let here = sidebar.lit_spot().cloned();
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(!shows_at(&mut sidebar, now, "─ All keys ─"));
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        let every = screen(&mut sidebar, true, 32, 120);
        let start = every
            .iter()
            .position(|line| line.starts_with("─ All keys ─"))
            .unwrap();
        let listed = &every[start + 1..every.len() - 1];
        let names: Vec<&str> = listed
            .iter()
            .filter(|line| !line.contains("  "))
            .map(String::as_str)
            .collect();
        assert_eq!(
            names,
            [" Move", " Sidebar", " Menu", " Pasted link", " Anywhere"]
        );
        for line in [
            " g g    First row",
            " ?      All keys",
            " y p  Yank the PR link",
            " n v  new view",
            " d d  close the view",
            " d d  close the worktree",
            " r    Rename the project…",
            " w  worktree",
            " BS   back a group",
            " Ctrl a       Sidebar or views",
            " Super k      Pick an action",
            " Alt Shift u  Stop reading",
        ] {
            assert!(
                listed.iter().any(|shown| shown == line),
                "{line:?} in {listed:#?}"
            );
        }
        let group = |name: &str| listed.iter().position(|line| line == name).unwrap();
        let sidebar_s = &listed[group(" Sidebar")..group(" Menu")];
        assert!(
            sidebar_s.iter().any(|line| line == " q    Quit lune"),
            "{sidebar_s:#?}"
        );
        assert_eq!(
            every.last().map(String::as_str),
            Some(" <↓↑> - Scroll, <ESC> - Close")
        );

        let total = listed.len();
        let short = screen(&mut sidebar, true, 32, 20);
        assert_eq!(short[3], format!("─ All keys ──────────── 15/{total} ─"));
        assert_eq!(short[4], " Move");
        sidebar.key(&char('j'), now);
        assert_eq!(
            screen(&mut sidebar, true, 32, 20)[4],
            " j k    Next row, row before"
        );
        for _ in 0..2 * total {
            sidebar.key(&char('j'), now);
        }
        let end = screen(&mut sidebar, true, 32, 20);
        assert_eq!(end[3], format!("─ All keys ──────────── {total}/{total} ─"));
        assert_eq!(end[18], " Alt Shift u  Stop reading");
        sidebar.key(&char('k'), now);
        assert_eq!(
            screen(&mut sidebar, true, 32, 20)[18],
            " Alt u        Read aloud, pause"
        );

        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), now),
            Asked::Nothing
        );
        assert!(!shows_at(&mut sidebar, now, "─ All keys ─"));
        assert!(!shows_at(&mut sidebar, now, " b  Brief ▸"));
        assert_eq!(sidebar.lit_spot().cloned(), here);
    }

    #[test]
    fn a_list_of_keys_short_of_room_ends_in_the_key_that_lists_them_all() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let now = moment(160_000);
        let area = Rect::new(0, 0, 32, 10);
        let drawn = |sidebar: &mut Sidebar| {
            let mut buffer = Buffer::empty(area);
            sidebar.place_rows(area);
            let drawn = sidebar.draw_rows(&mut buffer, area, now.system);
            let last = (0..31).map(|x| buffer[(x, 9)].symbol()).collect::<String>();
            (last.trim_end().to_owned(), drawn.at(Position::new(4, 9)))
        };
        sidebar.key(&char('?'), now);
        assert_eq!(
            drawn(&mut sidebar),
            (" ?  All keys".to_owned(), Some(on_legend('?')))
        );
        sidebar.keys_clicked(Keyed::Legend, &char('?'), now);
        assert!(matches!(sidebar.mode, Mode::Pending(Pending::AllKeys(_))));
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);

        sidebar.key(&char('m'), now);
        let all = Some(on_menu(KeyCode::Char('?')));
        let lines = screen(&mut sidebar, true, 32, 10);
        assert_eq!(lines[8], " ?  All keys", "{lines:#?}");
        let mut buffer = Buffer::empty(area);
        let at = sidebar
            .draw_rows(&mut buffer, area, now.system)
            .at(Position::new(4, 8));
        assert_eq!(at, all);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(sidebar.open().is_none());
        assert!(matches!(sidebar.mode, Mode::Pending(Pending::AllKeys(_))));
    }

    #[test]
    fn a_press_or_a_paste_closes_the_legend_and_the_list_of_every_key() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let now = moment(160_000);
        let area = Rect::new(0, 0, 32, 20);
        for opens in [&[char('?')][..], &[char('?'), char('?')]] {
            let shows = if opens.len() == 1 {
                " b  Brief ▸"
            } else {
                "─ All keys ─"
            };
            for key in opens {
                sidebar.key(key, now);
            }
            assert!(shows_at(&mut sidebar, now, shows), "{shows}");
            click(&mut sidebar, 10, 6, area);
            assert!(!shows_at(&mut sidebar, now, shows), "{shows}");

            for key in opens {
                sidebar.key(key, now);
            }
            let asked = sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall());
            assert_eq!(asked, Asked::Nothing);
            assert!(sidebar.open().is_some());
            sidebar.drop_menu(sidebar.menus);
            assert!(sidebar.open().is_none());
            assert!(!shows_at(&mut sidebar, now, shows), "{shows}");
        }
    }

    #[test]
    fn a_link_pasted_in_a_group_s_pause_shows_its_menu_at_once() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let now = moment(160_000);
        sidebar.key(&char('d'), now);
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let soon = now + Duration::from_millis(1);
        assert!(shows_at(&mut sidebar, soon, "─ #5 "));
        assert_eq!(sidebar.due(soon), None);
    }

    #[test]
    fn a_click_on_a_skill_of_the_menu_under_the_rows_starts_an_agent_on_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        sidebar.key(&char('m'), moment(160_000));
        let dir = directory("/w/lune/main");
        let agent = agents::Harness::ClaudeCode;
        let find = Asked::Find {
            menu: 1,
            on: Machine::Local,
            dir,
            agent,
        };
        assert_eq!(sidebar.key(&char('/'), moment(160_000)), find);
        let name = |name: &str| Name::new(name.to_owned()).expect("a name");
        let skill = |named: &str| Skill {
            name: name(named),
            description: String::new(),
            runs: crate::skills::Runs::ForAnyone,
        };
        let _ = sidebar.found(
            1,
            agent,
            Ok(vec![skill("code-review"), skill("tigerstyle")]),
        );
        let clicked = clicks(&mut sidebar, 14);
        let skills: Vec<(&str, Option<Hit>)> = clicked[1..5]
            .iter()
            .map(|(text, hit)| (text.as_str(), hit.clone()))
            .collect();
        let on = |skill| Some(Hit::Menu(1, Choice::Skill(name(skill))));
        let tab = Some(Hit::Menu(1, Choice::Key(KeyEvent::from(KeyCode::Tab))));
        assert_eq!(
            skills,
            [
                (" _", None),
                (" /code-review", on("code-review")),
                (" /tigerstyle", on("tigerstyle")),
                (" \u{ec82} claude, the view's  <TAB>", tab),
            ]
        );
        let tigerstyle = || Choice::Skill(name("tigerstyle"));
        assert_eq!(
            sidebar.chose(2, tigerstyle(), moment(160_000)),
            Asked::Nothing
        );
        let command = ["claude", "/tigerstyle"].map(str::to_owned).to_vec();
        let opens = Opens::Command(Argv::parse(command).unwrap());
        let (project, beside) = (listed("older", 10).project(), view_key(10, 1));
        let open = Asked::Open {
            asking: Held::Open,
            on: Machine::Local,
            project,
            beside,
            opens,
        };
        assert_eq!(sidebar.chose(1, tigerstyle(), moment(160_000)), open);
    }

    #[test]
    fn the_menu_starts_agents_in_the_view_s_harness_or_else_the_one_picked_last() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("agent");
        let mut sidebar = two_sessions();
        sidebar.edit(|state| {
            state.agent = Kept::within(file.clone()).0;
            if let Some(host) = &mut state.host {
                host.agents[0].harness = Some(Harness::Pi.into());
            }
        });
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        let started = |session: &str, beside, harness, spelled| {
            let command = [harness, spelled].map(str::to_owned).to_vec();
            let opens = Opens::Command(Argv::parse(command).unwrap());
            let id = if session == "older" { 10 } else { 20 };
            let (project, beside) = (listed(session, id).project(), view_key(id, beside));
            Asked::Open {
                asking: Held::Open,
                on: Machine::Local,
                project,
                beside,
                opens,
            }
        };
        type_in(&mut sidebar, "c");
        let pi = started("older", 1, "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);

        sidebar.focus(Keys::Terminal, test_wall());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.end(true);
        type_in(&mut sidebar, "c");
        let claude = started("newer", 1, "claude", "/tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), claude);

        sidebar.focus(Keys::Terminal, test_wall());
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "c");
        let tab = KeyEvent::from(KeyCode::Tab);
        assert_eq!(sidebar.key(&tab, moment(160_000)), Asked::Nothing);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "pi\n");
        let pi = started("newer", 1, "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
        sidebar.focus(Keys::Terminal, test_wall());
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "c");
        let pi = started("newer", 1, "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
    }

    #[test]
    fn a_key_of_the_menu_opens_it_for_the_highlighted_row() {
        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let _ = sidebar.ticked(moment(160_000) + PAUSE, true);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 17, moment(160_000) + PAUSE);
        assert_eq!(
            lines[9..],
            [
                "─ New ───────────── lune/main ─",
                " v  new view",
                " h  new welcome view",
                " b  new browser view…",
                " r  new review view",
                " w  new worktree…",
                " m  new machine…",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (project, branch) = (ProjectId::of(1).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree {
            on: Machine::Local,
            project,
            branch,
        };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        assert!(sidebar.open().is_none());

        sidebar.light(Spot::Local(own_view_of("young", 2, 1)));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(2).unwrap(), Branch::parse("fix").unwrap());
        let made = Asked::NewWorktree {
            on: Machine::Local,
            project,
            branch,
        };
        assert_eq!(branched(&mut sidebar, "fix"), made);
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(sidebar.open().is_none());
    }

    #[test]
    fn an_empty_project_is_closed_by_its_id() {
        let mut sidebar = fresh();
        let own = own_session("own", 1, Vec::new());
        let sessions = vec![own_session("here", 2, Vec::new()), own];
        sidebar.hear(host(sessions, Vec::new()));
        sidebar.focus(Keys::Sidebar, test_wall());
        let remove = |sidebar: &mut Sidebar| {
            let own = Listed::new_project("own", 1);
            sidebar.light(Spot::Local(Place::Session(own)));
            type_in(sidebar, "d");
            assert_eq!(
                sidebar.key(&char('d'), moment(160_000)),
                Asked::Nothing,
                "it asks first"
            );
            sidebar.key(&char('d'), moment(160_000))
        };
        let closed = |ticket| close_here(project_closed("own", 1, &[], &[]), ticket);
        assert_eq!(remove(&mut sidebar), closed(1));
        sidebar.sit(seat_in(2, None), test_wall());
        assert_eq!(remove(&mut sidebar), closed(2));
    }

    #[test]
    fn an_own_client_is_where_its_server_says_and_d_d_closes_its_project() {
        let mut sidebar = fresh();
        let views = || vec![view(1, 0, "/w/lune/main", "fish")];
        let sessions = vec![
            own_session("twin", 1, views()),
            own_session("newest", 2, Vec::new()),
        ];
        sidebar.hear(host(sessions, Vec::new()));
        sidebar.sit(seat_in(1, None), test_wall());
        assert_eq!(sidebar.here_place(), Err(NO_HERE_VIEW));
        sidebar.sit(seat_in(1, Some(1)), test_wall());
        let twin = Listed::new_project("twin", 1);
        assert_eq!(sidebar.here_place(), Ok(view_place("twin", 1, 0, 1)));
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(Place::Session(twin)));
        type_in(&mut sidebar, "d");
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            Asked::Nothing,
            "it asks first"
        );
        let closed = project_closed("twin", 1, &[1000], &[1]);
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            close_here(closed, 1)
        );
    }

    #[test]
    fn lune_unseated_sits_in_no_project_and_its_d_d_closes_the_view() {
        let mut sidebar = fresh();
        let views = || vec![view(1, 0, "/w/lune/main", "fish")];
        let sessions = vec![
            own_session("twin", 1, views()),
            own_session("newest", 2, Vec::new()),
        ];
        sidebar.hear(host(sessions, Vec::new()));
        sidebar.sit(seat_in(1, Some(1)), test_wall());
        sidebar.unseat(test_wall());
        assert_eq!(sidebar.seated_in(), None);
        let view = view_key(1, 1);
        sidebar.light(Spot::Local(view_of("twin", 1, 1)));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "d");
        let act = Act::View(view);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), close_here(act, 1));
    }

    fn check_out_pull(sidebar: &mut Sidebar, dir: &str, number: u32) {
        sidebar.edit(|state| {
            let Some(host) = &mut state.host else {
                panic!("the sidebar has heard of no host");
            };
            let url = format!("https://github.com/lorenzolfm/lune/pull/{number}");
            let pull = Some((open_pull_state(false), url.as_str()));
            checked_out(&mut Vec::new(), &mut host.pull_requests, dir, "feat", pull);
        });
    }

    #[test]
    fn a_pull_request_s_link_pasted_goes_to_its_worktree_when_one_is_present() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        check_out_pull(&mut sidebar, "/w/b", 5);
        sidebar.focus(Keys::Sidebar, test_wall());
        let young = WorktreePlace {
            project: project_row("young", 2),
            worktree: tab_of(2, 0),
        };
        let there = Asked::Go {
            on: Machine::Local,
            place: Place::Worktree(young),
        };
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5\n", test_wall()),
            there
        );
        assert!(sidebar.open().is_none());

        let link = Link::parse("https://github.com/lorenzolfm/lune/pull/5").unwrap();
        let worked = sidebar.worked(&Machine::Local).unwrap();
        let carries = |it: &&Worked| it.works.pull.as_ref().is_some_and(|it| it.is(&link));
        let worked = worked.iter().filter(carries);
        let sessions: Vec<&Listed> = worked.map(|it| it.place.listed()).collect();
        assert_eq!(sessions, [&own_listed("young", 2)]);

        sidebar.key(&char('m'), moment(160_000));
        assert!(sidebar.open().is_some());
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            there
        );
        assert!(sidebar.open().is_none());

        let as_an_issue = "https://github.com/lorenzolfm/lune/issues/5";
        assert_eq!(
            sidebar.paste(as_an_issue, test_wall()),
            there,
            "GitHub numbers issues and pulls as one"
        );
        let elsewhere = ["lorenzolfm/lune/pull/6", "lorenzolfm/zed/pull/5"];
        for elsewhere in elsewhere.map(|it| format!("https://github.com/{it}")) {
            assert_eq!(
                sidebar.paste(&elsewhere, test_wall()),
                Asked::Nothing,
                "{elsewhere}"
            );
            assert!(sidebar.open().is_some(), "{elsewhere}");
            sidebar.drop_menu(sidebar.menus);
        }
    }

    #[test]
    fn a_pull_request_s_link_pasted_opens_the_menu_when_two_worktrees_have_it() {
        let link = "https://github.com/lorenzolfm/lune/pull/5";
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        check_out_pull(&mut sidebar, "/w/b", 5);
        check_out_pull(&mut sidebar, "/w/c", 5);
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste(link, test_wall()), Asked::Nothing);
        assert!(sidebar.open().is_some());
    }

    #[test]
    fn a_pull_request_s_link_pasted_opens_the_menu_for_it() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        check_out_pull(&mut sidebar, "/w/b", 7);
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste("ls -la", test_wall()), Asked::Nothing);
        assert!(sidebar.open().is_none());
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5\n", test_wall()),
            Asked::Nothing
        );
        let lines = screen(&mut sidebar, true, 32, 17);
        assert_eq!(
            lines[12..],
            [
                "─ #5 ──────── lorenzolfm/lune ─",
                " r  /thermo-nuclear-review",
                " w  worktree",
                " \u{ec82} claude  <TAB> another agent",
                " <ESC> - Close",
            ]
        );
        let link = Link::parse("https://github.com/lorenzolfm/lune/pull/5").unwrap();
        let on_b = Link::parse("https://github.com/lorenzolfm/lune/pull/7").unwrap();
        let worked = |place, dir: &str, pull, here| Worked {
            place,
            works: links::Worked {
                dir: directory(dir),
                pull,
                here,
            },
        };
        let worked = vec![
            worked(view_place("older", 1, 0, 1), "/w/lune/main", None, true),
            worked(view_place("older", 1, 1, 2), "/w/c", None, true),
            worked(view_place("young", 2, 0, 1), "/w/b", Some(on_b), false),
        ];
        let work = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let on = Machine::Local;
        let others = Vec::new();
        let pull = Asked::Pull {
            menu: 1,
            on,
            work,
            wanted,
            worked,
            others,
        };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);

        let at = At::Unopened;
        let work = Readied::Link(link);
        let folder = directory("/w/n/feat");
        let ready = Ready {
            at,
            folder,
            branch: None,
            moved_off: false,
            work,
        };
        let asked = Asked::Make {
            menu: 1,
            on: Machine::Local,
            cwd: directory("/w/n/feat"),
            name: "#5".to_owned(),
            agent: None,
        };
        assert_eq!(
            sidebar.pulled(
                1,
                Ok(Found {
                    on: Machine::Local,
                    ready
                })
            ),
            asked
        );
        assert_eq!(sidebar.made_for(1), Some(Held::Open));
        assert!(sidebar.open().is_none());
        assert_eq!(sidebar.made_for(1), None);
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn what_a_key_of_a_pasted_link_started_goes_on_once_the_keys_leave_the_menu() {
        let page = "https://github.com/lorenzolfm/lune/pull/5";
        let link = || Link::parse(page).unwrap();
        let number = 1;
        let pulling = || {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
            let Asked::Pull { .. } = sidebar.key(&char('r'), moment(160_000)) else {
                panic!("no job for the key");
            };
            assert!(sidebar.open().is_some_and(Menu::pulling));
            sidebar
        };
        let ready = || {
            let work = Readied::Link(link());
            let at = At::Unopened;
            let folder = directory("/w/lune/feat");
            Ready {
                at,
                folder,
                branch: None,
                moved_off: false,
                work,
            }
        };

        let mut sidebar = pulling();
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(sidebar.open().is_none());
        assert_eq!(sidebar.working.len(), 1);
        let lines = screen(&mut sidebar, false, 32, 17);
        assert!(
            lines.iter().any(|line| line.contains("Syncing #5…")),
            "{lines:?}"
        );
        let found = || Found {
            on: Machine::Local,
            ready: ready(),
        };
        let made = |asked: &Asked| matches!(asked, Asked::Make { menu, .. } if *menu == number);
        let asked = sidebar.pulled(number, Ok(found()));
        assert!(made(&asked), "{asked:?}");
        assert_eq!(sidebar.made_for(number), Some(Held::Kept));
        assert!(sidebar.working.is_empty());

        let mut sidebar = pulling();
        let asked = sidebar.pulled(number, Ok(found()));
        assert!(made(&asked), "{asked:?}");
        assert_eq!(sidebar.made_for(number), Some(Held::Open));
        assert!(sidebar.open().is_none());

        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.focus(Keys::Terminal, test_wall());
        let there = || {
            let work = Readied::Link(link());
            let at = At::There(view_place("older", 1, 0, 4));
            Ready {
                at,
                folder: directory("/w/lune/feat"),
                branch: None,
                moved_off: false,
                work,
            }
        };
        let asked = sidebar.pulled(
            number,
            Ok(Found {
                on: Machine::Local,
                ready: there(),
            }),
        );
        assert_eq!(asked, Asked::Nothing);
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("menu: older:4 has it; nobody went")
        );
    }

    #[test]
    fn esc_closes_a_menu_that_waits_and_what_it_started_goes_on() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        let number = sidebar.open().map_or(0, Menu::number);
        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000)),
            Asked::Nothing
        );
        assert!(sidebar.open().is_none());
        assert!(sidebar.working.contains_key(&number));
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "m");
        assert!(sidebar.open().is_some());
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000));
        assert!(sidebar.open().is_none());
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_link_pasted_again_while_its_worktree_goes_on_makes_nothing_of_its_own() {
        let page = "https://github.com/lorenzolfm/lune/pull/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        let number = sidebar.open().map_or(0, Menu::number);
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        assert_eq!(sidebar.open().map(Menu::number), Some(number));
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(sidebar.working.contains_key(&number));
        let _ = sidebar.clear_notice();
        sidebar.focus(Keys::Terminal, test_wall());
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        assert!(sidebar.open().is_none());
        assert_eq!(sidebar.notice.as_deref(), Some("#5 is on its way"));
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/issues/6", test_wall()),
            Asked::Nothing
        );
        assert!(sidebar.open().is_some());
    }

    #[test]
    fn a_link_pasted_again_before_a_key_opens_its_menu_again() {
        let page = "https://github.com/lorenzolfm/lune/pull/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        assert_eq!(sidebar.open().map(Menu::number), Some(2));
        assert_eq!(sidebar.notice, None);
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_pull_picked_in_the_palette_asks_as_its_key_in_the_pasted_menu_does() {
        let page = "https://github.com/lorenzolfm/lune/pull/5";
        let mut pasted = own_two_sessions();
        pasted.focus(Keys::Sidebar, test_wall());
        assert_eq!(pasted.paste(page, test_wall()), Asked::Nothing);
        let keyed = pasted.key(&char('r'), moment(160_000));
        assert!(matches!(keyed, Asked::Pull { .. }), "{keyed:?}");
        let mut picked = own_two_sessions();
        picked.focus(Keys::Sidebar, test_wall());
        let target = picked.target().expect("a row is highlighted");
        let work = Work::parse(page, None).expect("a pull request's link");
        let pull = |sidebar: &mut Sidebar| {
            sidebar.pull_in_menu(target.clone(), work.clone(), Pull::Agent, Origin::Sidebar)
        };
        assert_eq!(pull(&mut picked), keyed);
        assert_eq!(pull(&mut picked), Asked::Nothing);
        assert_eq!(picked.notice.as_deref(), Some("#5 is on its way"));
    }

    #[test]
    fn a_menu_that_waits_goes_on_when_another_takes_its_place() {
        let waiting = || {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            let asked = sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall());
            assert_eq!(asked, Asked::Nothing);
            let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
                panic!("no job for the key");
            };
            sidebar
        };
        let mut sidebar = waiting();
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/issues/6", test_wall()),
            Asked::Nothing
        );
        assert_eq!(sidebar.open().map(Menu::number), Some(2));
        assert!(sidebar.working.contains_key(&1));

        let mut sidebar = waiting();
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        float_a_menu(&mut sidebar);
        assert_eq!(sidebar.open().map(Menu::number), Some(2));
        assert!(sidebar.working.contains_key(&1));
    }

    #[test]
    fn a_kept_menu_s_last_answer_leaves_the_open_menu_alone() {
        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000));
        let page = "https://github.com/lorenzolfm/lune/issues/6";
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(162_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(
            sidebar.pulled(1, Err("gh failed".to_owned())),
            Asked::Nothing
        );
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("menu: lorenzolfm/lune#5: gh failed")
        );
        assert!(sidebar.working.is_empty());
        assert_eq!(sidebar.open().map(Menu::number), Some(2));
        let work = Readied::Link(Link::parse(page).unwrap());
        let at = At::Unopened;
        let folder = directory("/w/lune/issue-6");
        let ready = Ready {
            at,
            folder,
            branch: None,
            moved_off: false,
            work,
        };
        let asked = sidebar.pulled(
            2,
            Ok(Found {
                on: Machine::Local,
                ready,
            }),
        );
        assert!(matches!(asked, Asked::Make { menu: 2, .. }), "{asked:?}");
        assert_eq!(sidebar.made_for(2), Some(Held::Open));
        assert!(sidebar.open().is_none());
    }

    #[test]
    fn a_md_file_s_path_pasted_opens_the_menu_for_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        for text in [
            "notes.txt",
            "two\nlines.md",
            "https://example.com/plan.md",
            "github.com/o/r/blob/main/README.md",
            ".md",
        ] {
            assert_eq!(sidebar.paste(text, test_wall()), Asked::Nothing);
            assert!(sidebar.open().is_none(), "{text:?}");
        }
        let Some(Target::View(on, row)) = sidebar.target() else {
            panic!("not a view's row")
        };
        let from = row.cwd.at().map(|folder| folder.dir.clone());
        assert_eq!(
            sidebar.paste("  docs/plans/sync-rework.md\n", test_wall()),
            Asked::Nothing
        );
        let lines = screen(&mut sidebar, true, 39, 17);
        assert_eq!(
            lines[12..],
            [
                "─ sync-rework ─ plans/sync-rework.md ─",
                " t  tackle",
                " w  worktree",
                " \u{ec82} claude  <TAB> another agent",
                " <ESC> - Close",
            ]
        );
        let Asked::Pull {
            on: asked_on, work, ..
        } = sidebar.key(&char('t'), moment(160_000))
        else {
            panic!("no job for the key");
        };
        assert_eq!(asked_on, on);
        assert_eq!(Some(work), Work::parse("docs/plans/sync-rework.md", from));

        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(Place::Session(listed("older", 10))));
        assert_eq!(sidebar.paste("/w/plan.md", test_wall()), Asked::Nothing);
        let Asked::Pull { work, .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(Some(work), Work::parse("/w/plan.md", None));
    }

    #[test]
    fn w_says_a_worktree_moved_off_its_branch_and_t_starts_no_agent_on_it() {
        let page = "https://github.com/lorenzolfm/lune/issues/5";
        let pressed = |key: char, branch: Option<&str>, moved_off: bool| {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
            let number = sidebar.open().map_or(0, Menu::number);
            let Asked::Pull { wanted, .. } = sidebar.key(&char(key), moment(160_000)) else {
                panic!("no job for the key");
            };
            assert_eq!(wanted, Wanted::AsItIs);
            let at = At::Unopened;
            let (folder, work) = ("/w/lune/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let branch = branch.and_then(|branch| Said::new(branch.to_owned()));
            let ready = Ready {
                at,
                folder: directory(folder),
                branch,
                moved_off,
                work,
            };
            let asked = sidebar.pulled(
                number,
                Ok(Found {
                    on: Machine::Local,
                    ready,
                }),
            );
            (matches!(asked, Asked::Make { .. }), sidebar.notice.clone())
        };
        let on_dev_docs = "#5's worktree is on dev-docs, not issue-5";
        assert_eq!(
            pressed('w', Some("dev-docs"), true),
            (true, Some(on_dev_docs.to_owned()))
        );
        let on_none = "#5's worktree is on no branch, not issue-5";
        assert_eq!(pressed('w', None, true), (true, Some(on_none.to_owned())));
        assert_eq!(pressed('w', Some("issue-5"), false), (true, None));
        assert_eq!(pressed('w', None, false), (true, None));

        assert_eq!(pressed('t', Some("dev-docs"), true), (false, None));
        assert_eq!(pressed('t', Some("issue-5"), false), (true, None));
        assert_eq!(pressed('t', None, false), (true, None));
    }

    #[test]
    fn w_goes_on_from_a_menu_whose_t_refused_a_worktree_that_moved_off() {
        let page = "https://github.com/lorenzolfm/lune/issues/5";
        for floats in [false, true] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            if floats {
                sit(&mut sidebar, &view_of("older", 10, 1));
                float_a_menu(&mut sidebar);
            }
            assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
            let number = sidebar.open().map_or(0, Menu::number);
            let Asked::Pull { .. } = sidebar.key(&char('t'), moment(160_000)) else {
                panic!("no job for the key");
            };
            let at = At::Unopened;
            let branch = Said::new("dev-docs".to_owned());
            let (folder, work) = ("/w/lune/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let ready = Ready {
                at,
                folder: directory(folder),
                branch,
                moved_off: true,
                work,
            };
            let asked = sidebar.pulled(
                number,
                Ok(Found {
                    on: Machine::Local,
                    ready,
                }),
            );
            assert_eq!(asked, Asked::Nothing);
            assert_eq!(floating_menu(&sidebar).is_some(), floats);
            assert_eq!(sidebar.notice.as_deref(), None);
            let Asked::Pull {
                wanted: Wanted::AsItIs,
                ..
            } = sidebar.key(&char('w'), moment(161_000))
            else {
                panic!("no job for the key");
            };
        }
    }

    #[test]
    fn the_menu_on_an_agent_is_for_where_its_row_says_it_works() {
        let mut sidebar = fresh();
        let claude = view(1, 0, "/w/lune/main", "claude");
        let agents = vec![agent("s", 1, Status::Idle, None)];
        sidebar.hear(host(vec![session("s", 10, vec![claude])], agents));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "j");
        assert_eq!(lit_place(&sidebar), Some(view_of("s", 10, 1)));
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let _ = sidebar.ticked(moment(160_000) + PAUSE, true);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 13, moment(160_000) + PAUSE);
        assert_eq!(lines[5], "─ New ───────────── lune/main ─", "{lines:?}");
        let (project, beside) = (listed("s", 10).project(), view_key(10, 1));
        let opens = Opens::Shell;
        let view = Asked::Open {
            asking: Held::Open,
            on: Machine::Local,
            project,
            beside,
            opens,
        };
        assert_eq!(sidebar.key(&char('v'), moment(160_000)), view);
    }

    #[test]
    fn the_menu_on_a_shell_s_row_starts_an_agent_on_its_last_command() {
        let mut sidebar = fresh();
        let failed = in_shell(1, ended("cargo test", Some(101), 3000));
        let busy = in_shell(2, going("make", 100, Front::Working));
        sidebar.hear(host(vec![session("s", 10, vec![failed, busy])], Vec::new()));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "j");
        assert_eq!(lit_place(&sidebar), Some(view_of("s", 10, 1)));
        type_in(&mut sidebar, "c");
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        let command = Said::new("cargo test".to_owned()).unwrap();
        let failed = menu::Ended {
            command,
            ending: Some(Ending::Exited(101)),
            at,
        };
        let (on, s) = (Machine::Local, listed("s", 10));
        let view = view_key(10, 1);
        let printed = Asked::Printed {
            menu: 1,
            on,
            session: s,
            view,
            ended: failed,
        };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        let nothing = || Ok(Printed::Nothing);
        assert_eq!(sidebar.printed(2, nothing()), Asked::Nothing);
        let prompt = "Fix this. In my shell, `cargo test` ended with exit status 101. It printed \
                      nothing.";
        let command = vec!["claude".to_owned(), "--".to_owned(), prompt.to_owned()];
        let opens = Opens::Command(Argv::parse(command).unwrap());
        let (project, beside) = (listed("s", 10).project(), view_key(10, 1));
        let open = Asked::Open {
            asking: Held::Open,
            on: Machine::Local,
            project,
            beside,
            opens,
        };
        assert_eq!(sidebar.printed(1, nothing()), open);
        assert!(sidebar.open().is_none());

        type_in(&mut sidebar, "jcf");
        assert!(sidebar.open().is_none());
        let why = "menu: view 2 runs `make` still, and what it prints is not over";
        assert_eq!(sidebar.notice.as_deref(), Some(why));

        type_in(&mut sidebar, "c");
        let shells = |busy: Doing| {
            let failed = in_shell(1, ended("cargo test", Some(101), 3000));
            host(
                vec![session("s", 10, vec![failed, in_shell(2, busy)])],
                Vec::new(),
            )
        };
        sidebar.hear(shells(ended("make", Some(2), 3000)));
        let command = Said::new("make".to_owned()).unwrap();
        let make = menu::Ended {
            command,
            ending: Some(Ending::Exited(2)),
            at,
        };
        let (on, s) = (Machine::Local, listed("s", 10));
        let view = view_key(10, 2);
        let printed = Asked::Printed {
            menu: 3,
            on,
            session: s,
            view,
            ended: make,
        };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        sidebar.hear(shells(going("make", 200, Front::Working)));
        assert_eq!(sidebar.printed(3, nothing()), Asked::Nothing);
        assert!(sidebar.open().is_none());
        let why = "menu: view 2's shell went on as lune read what `make` printed; ask again";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    #[test]
    fn a_last_command_s_agent_opens_where_the_keys_left_the_menu() {
        let mut sidebar = fresh();
        let failed = in_shell(1, ended("cargo test", Some(101), 3000));
        sidebar.hear(host(vec![own_session("s", 1, vec![failed])], Vec::new()));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "Gc");
        let asked = sidebar.key(&char('f'), moment(160_000));
        let project = own_listed("s", 1);
        let printed = matches!(&asked, Asked::Printed { session, .. } if *session == project);
        assert!(printed, "{asked:?}");
        let number = sidebar.open().map_or(0, Menu::number);
        sidebar.focus(Keys::Terminal, test_wall());
        assert_eq!(sidebar.working.len(), 1);
        let lines = screen(&mut sidebar, false, 40, 17);
        let notes: String = lines[14..].join(" ");
        assert!(
            notes.contains("⠋ Reading what `cargo test` printed…"),
            "{lines:?}"
        );
        let asked = sidebar.printed(number, Ok(Printed::Nothing));
        assert!(
            matches!(
                asked,
                Asked::Open {
                    asking: Held::Kept,
                    ..
                }
            ),
            "{asked:?}"
        );
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_kept_menu_whose_next_ask_is_not_made_keeps_nothing() {
        let page = "https://github.com/lorenzolfm/lune/pull/5";
        let left = |at| {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
            let number = sidebar.open().map_or(0, Menu::number);
            let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
                panic!("no job for the key");
            };
            sidebar.focus(Keys::Terminal, test_wall());
            let work = Readied::Link(Link::parse(page).unwrap());
            let folder = directory("/w/lune/feat");
            let ready = Ready {
                at,
                folder,
                branch: None,
                moved_off: false,
                work,
            };
            let asked = sidebar.pulled(
                number,
                Ok(Found {
                    on: Machine::Local,
                    ready,
                }),
            );
            (sidebar, number, asked)
        };
        let (mut sidebar, number, asked) = left(At::Unopened);
        assert!(
            matches!(asked, Asked::Make { menu, .. } if menu == number),
            "{asked:?}"
        );
        assert_eq!(sidebar.made_for(number), Some(Held::Kept));
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_kept_menu_that_starts_no_agent_on_a_worktree_moved_off_says_so() {
        let page = "https://github.com/lorenzolfm/lune/issues/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.paste(page, test_wall()), Asked::Nothing);
        let number = sidebar.open().map_or(0, Menu::number);
        let Asked::Pull { .. } = sidebar.key(&char('t'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.focus(Keys::Terminal, test_wall());
        let at = At::Unopened;
        let branch = Said::new("dev-docs".to_owned());
        let (folder, work) = ("/w/lune/issue-5", Readied::Link(Link::parse(page).unwrap()));
        let ready = Ready {
            at,
            folder: directory(folder),
            branch,
            moved_off: true,
            work,
        };
        let asked = sidebar.pulled(
            number,
            Ok(Found {
                on: Machine::Local,
                ready,
            }),
        );
        assert_eq!(asked, Asked::Nothing);
        assert!(sidebar.working.is_empty());
        let notice = sidebar.notice.as_deref().unwrap_or_default();
        assert!(notice.starts_with("menu: /w/lune/issue-5 is "), "{notice}");
        let tail = "so no agent starts there; w opens it once #5 is pasted again";
        assert!(notice.ends_with(tail), "{notice}");
    }

    #[test]
    fn the_menu_floats_over_the_project_for_the_view_lune_is_on() {
        let mut sidebar = fresh();
        let mut host = host(
            vec![
                own_session("young", 2, vec![view(1, 0, "/w/b", "fish")]),
                own_session("older", 1, vec![view(1, 0, "/w/lune/main", "claude")]),
            ],
            vec![in_own(
                agent("older", 1, Status::Waiting, Some("fold")),
                1,
                1,
            )],
        );
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/menu".to_owned(),
            upstream: None,
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout {
            head,
            changes,
            stashes: 0,
            repository: None,
        };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/lune/main".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        sidebar.sit(seat_in(1, None), test_wall());
        sidebar.float_menu();
        assert!(matches!(
            sidebar.floating(),
            Some(Floating::Note(NO_HERE_VIEW))
        ));
        sidebar.close_note();
        sit(&mut sidebar, &own_view_of("older", 1, 1));

        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "hhjj");
        assert!(folded(&sidebar, "older"));
        assert_eq!(lit_place(&sidebar), Some(own_view_of("young", 2, 1)));
        let standing = |sidebar: &mut Sidebar| {
            let area = Rect::new(0, 0, 32, 14);
            let mut buffer = Buffer::empty(area);
            sidebar.place_rows(area);
            sidebar.draw_rows(&mut buffer, area, test_wall());
            let row = |y| (0..32).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            (0..14).map(row).collect::<Vec<_>>()
        };
        sidebar.focus(Keys::Lent, test_wall());
        let lent = standing(&mut sidebar);
        sidebar.focus(Keys::Sidebar, test_wall());
        float_a_menu(&mut sidebar);
        assert_eq!(standing(&mut sidebar), lent);
        let target = floating_menu(&sidebar).map(Menu::target).cloned();
        assert_eq!(
            target,
            Some(Target::View(
                Machine::Local,
                ViewRow {
                    place: view_place("older", 1, 0, 1),
                    terminal: ViewId::of(1),
                    cwd: Cwd::At(Folder {
                        dir: directory("/w/lune/main"),
                        branch: Said::new("feat/menu".to_owned()),
                        repository: None,
                        pull_request: None,
                    }),
                    ran: Ran::Unhooked,
                    arranged: Some(Arranged::Tabs),
                }
            ))
        );

        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(1).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree {
            on: Machine::Local,
            project,
            branch,
        };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        assert!(sidebar.floating().is_none());
        sidebar.focus(Keys::Terminal, test_wall());

        float_a_menu(&mut sidebar);
        type_in(&mut sidebar, "c");
        let command = ["claude", "/thermo-nuclear-review"];
        let opens = Opens::Command(Argv::parse(command.map(str::to_owned).to_vec()).unwrap());
        let (project, beside) = (own_listed("older", 1).project(), view_key(1, 1));
        let view = Asked::Open {
            asking: Held::Open,
            on: Machine::Local,
            project,
            beside,
            opens,
        };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), view);
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(sidebar.open().is_none());
    }

    #[test]
    fn the_menu_uses_the_seated_worktree_s_live_arrangement_before_watch_catches_up() {
        let mut sidebar = own_two_sessions();
        let older = view_place("older", 1, 0, 1);
        let at = Sitting::Worktree {
            worktree: older.worktree.worktree,
            layout: SeatedLayout::Live {
                arranged: Arranged::Panes,
                version: None,
                terminal: Some(older.view.key()),
            },
        };
        let project = older.worktree.project.id();
        sidebar.sit(
            Seat {
                on: Machine::Local,
                project,
                at,
            },
            test_wall(),
        );
        float_a_menu(&mut sidebar);
        let expected = Asked::Arrange {
            asking: Held::Open,
            on: Machine::Local,
            session: older.listed().clone(),
            view: older.view,
            arranged: Arranged::Tabs,
        };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), expected);
        let other = sidebar.target_at(&Machine::Local, own_view_of("young", 2, 1));
        assert!(matches!(
            other,
            Some(Target::View(
                _,
                ViewRow {
                    arranged: Some(Arranged::Tabs),
                    ..
                }
            ))
        ));
    }

    #[test]
    fn live_seat_arrangement_yields_to_newer_watch_version_and_disconnected_seat() {
        let mut sidebar = fresh();
        let version = |seq| api::Version { pid: 7, seq };
        let mut built = host(
            vec![own_session(
                "older",
                1,
                vec![view(1, 0, "/w/lune/main", "fish")],
            )],
            Vec::new(),
        );
        if let Some(api::Projects {
            held: Some(api::projects::Held::Kept(kept)),
        }) = &mut built.projects
        {
            kept.version = Some(version(10));
        }
        sidebar.hear(built);
        let older = view_place("older", 1, 0, 1);
        let project = older.worktree.project.id();
        let view_spot = own_view_of("older", 1, 1);
        let arranged_of =
            |sidebar: &Sidebar| match sidebar.target_at(&Machine::Local, view_spot.clone()) {
                Some(Target::View(_, row)) => row.arranged,
                other => panic!("expected view target, got {other:?}"),
            };

        let seat_with = |layout| Seat {
            on: Machine::Local,
            project,
            at: Sitting::Worktree {
                worktree: older.worktree.worktree,
                layout,
            },
        };
        sidebar.sit(
            seat_with(SeatedLayout::Live {
                arranged: Arranged::Panes,
                version: Some(version(11)),
                terminal: Some(older.view.key()),
            }),
            test_wall(),
        );
        assert_eq!(
            arranged_of(&sidebar),
            Some(Arranged::Panes),
            "newer show beats older watch"
        );

        sidebar.sit(
            seat_with(SeatedLayout::Live {
                arranged: Arranged::Panes,
                version: Some(version(9)),
                terminal: Some(older.view.key()),
            }),
            test_wall(),
        );
        assert_eq!(
            arranged_of(&sidebar),
            Some(Arranged::Tabs),
            "newer watch beats stale show"
        );

        sidebar.sit(
            seat_with(SeatedLayout::Disconnected(older.view.key())),
            test_wall(),
        );
        assert_eq!(
            arranged_of(&sidebar),
            Some(Arranged::Tabs),
            "disconnected show yields to watch"
        );
        assert_eq!(sidebar.here_place(), Ok(older));
    }

    #[test]
    fn the_agent_here_is_the_one_in_the_view_lune_is_on() {
        let on_view = |id: u32| {
            let mut sidebar = fresh();
            let views = vec![
                view(1, 0, "/w/a", "claude"),
                view(2, 0, "/w/b", "fish"),
                view(3, 0, "/w/c", "fish"),
                view(4, 0, "/w/d", "pi"),
            ];
            let printing = agent("s", 3, Status::Busy, None);
            let printing = Agent {
                entrypoint: Some("sdk-cli".to_owned()),
                ..printing
            };
            let pi = Agent {
                harness: Some(Harness::Pi.into()),
                entrypoint: Some("tui".to_owned()),
                ..agent("s", 4, Status::Idle, None)
            };
            let agents = vec![agent("s", 1, Status::Idle, None), printing, pi];
            let agents = agents.into_iter().map(|agent| {
                let view = agent
                    .terminal
                    .as_ref()
                    .expect("an agent of a view of a project's")
                    .id;
                in_own(agent, 1, view)
            });
            sidebar.hear(host(vec![own_session("s", 1, views)], agents.collect()));
            assert_eq!(sidebar.here_agent(), None);
            sit(&mut sidebar, &own_view_of("s", 1, id));
            sidebar.here_agent()
        };
        let session = SessionName::listed("s").expect("a name");
        let (dir, harness) = (directory("/w/a"), agents::Harness::ClaudeCode);
        let here = HereAgent {
            on: Machine::Local,
            session,
            view: view_key(1, 1),
            dir,
            harness,
        };
        assert_eq!(on_view(1), Some(here));
        assert_eq!(on_view(2), None);
        assert_eq!(on_view(3), None);
        let session = SessionName::listed("s").expect("a name");
        let (dir, harness) = (directory("/w/d"), agents::Harness::Pi);
        let here = HereAgent {
            on: Machine::Local,
            session,
            view: view_key(1, 4),
            dir,
            harness,
        };
        assert_eq!(on_view(4), Some(here));
    }

    #[test]
    fn the_menu_over_the_session_says_over_it_what_went_wrong() {
        let mut sidebar = fresh();
        let own = worktree_of(31, "notes", "/w/notes", &[1]);
        let mut notes = a_project(8, "notes", Vec::new());
        notes.dir = "/w/notes".to_owned();
        notes.holds = Some(api::project::Holds::Folder(own));
        sidebar.heard(
            Host {
                projects: Some(kept(vec![notes])),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        let own = WorktreePlace {
            project: project_row("notes", 8),
            worktree: WorktreeId::of(31).unwrap(),
        };
        sit(
            &mut sidebar,
            &Place::View(ViewPlace::new(own, ViewKey::terminal(1))),
        );
        let wrong = "menu: notes is a folder, and its own is its one worktree";

        let floated = |sidebar: &Sidebar| match sidebar.floating() {
            Some(Floating::Note(note)) => Some(note.to_owned()),
            Some(Floating::Menu(_)) | None => None,
        };

        float_a_menu(&mut sidebar);
        type_in(&mut sidebar, "nw");
        assert_eq!(
            (floated(&sidebar).as_deref(), sidebar.notice.as_deref()),
            (Some(wrong), None)
        );
        sidebar.close_note();
        assert!(sidebar.floating().is_none());

        float_a_menu(&mut sidebar);
        type_in(&mut sidebar, "nw");
        sidebar.focus(Keys::Terminal, test_wall());
        assert_eq!(
            (floated(&sidebar).as_deref(), sidebar.notice.as_deref()),
            (None, Some(wrong))
        );

        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "nw");
        assert_eq!(
            (floated(&sidebar).as_deref(), sidebar.notice.as_deref()),
            (None, Some(wrong))
        );
        float_a_menu(&mut sidebar);
        assert!(sidebar.notice.is_none());
    }

    #[test]
    fn the_palette_aims_at_the_view_or_the_row_and_runs_its_action_where_it_opened() {
        let mut sidebar = two_sessions();
        let unplaced = Aim::Unplaced(PALETTE_UNPLACED);
        assert_eq!(sidebar.aimed_view(), unplaced, "no view is shown yet");
        assert_eq!(
            sidebar.aimed_worktree(),
            unplaced,
            "no worktree is shown yet"
        );
        sit(&mut sidebar, &view_of("older", 10, 1));
        let Aim::At { target, .. } = sidebar.aimed_view() else {
            panic!("the palette aims at the view shown");
        };
        let Target::View(_, row) = target.as_ref() else {
            panic!("the palette aims at the view shown: {target:?}");
        };
        assert_eq!(row.place, view_place("older", 10, 0, 1));
        let Aim::At {
            target: welcome, ..
        } = sidebar.aimed_worktree()
        else {
            panic!("over a welcome view, the palette aims at its worktree");
        };
        let Target::Worktree(_, worktree) = welcome.as_ref() else {
            panic!("over a welcome view, the palette aims at its worktree: {welcome:?}");
        };
        assert_eq!(worktree.place.worktree, tab_of(10, 0));
        let Aim::At {
            worktree: Some(beside),
            ..
        } = sidebar.aimed_view()
        else {
            panic!("over a view, the palette aims at its worktree too");
        };
        assert_eq!(beside, welcome, "the view's worktree is the one shown");
        let asked = sidebar.act_in_menu(&target, Action::CloseView, Origin::Views);
        assert_eq!(
            asked,
            Asked::Nothing,
            "a view's close is planned before it is asked"
        );
        assert_eq!(
            sidebar.holding(),
            Holding::Afloat,
            "over the views, its menu floats"
        );
        sidebar.close_menu();

        sidebar.focus(Keys::Sidebar, test_wall());
        let Aim::At { target, .. } = sidebar.aimed_row() else {
            panic!("in the sidebar, it aims at the highlighted row");
        };
        assert_eq!(
            Some(target.as_ref()),
            sidebar.target().as_ref(),
            "it aims at the row"
        );
        sidebar.act_in_menu(&target, Action::NewMachine, Origin::Sidebar);
        assert_eq!(
            sidebar.holding(),
            Holding::Menu,
            "in the sidebar, its menu opens there"
        );
        sidebar.act_on_row(&target, Action::Search, moment(160_000).system);
        assert_eq!(
            sidebar.holding(),
            Holding::Search,
            "a sidebar key's action runs as its key"
        );
        let vps: Destination = "me@vps".parse().unwrap();
        let asked = sidebar.chained(Done::Ssh(vps.clone()), Origin::Sidebar);
        assert_eq!(asked, Asked::Ssh(vps));
    }

    #[test]
    fn a_sidebar_key_from_the_palette_runs_on_the_row_it_named_and_on_no_other() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        let bipa = Spot::Local(Place::Session(listed("bipa", 10)));
        sidebar.edit(|state| state.lit = None);
        assert_eq!(sidebar.aimed_row(), Aim::Unplaced(PALETTE_NO_ROW));
        sidebar.light(bipa.clone());
        let Aim::At { target, .. } = sidebar.aimed_row() else {
            panic!("the palette aims at the highlighted row");
        };
        sidebar.key(&char('j'), moment(160_000));
        assert_ne!(
            sidebar.lit_spot().cloned(),
            Some(bipa.clone()),
            "j moves the highlight"
        );
        let asked = sidebar.act_on_row(&target, Action::Reorder, moment(160_000).system);
        assert_eq!(asked, Asked::Nothing);
        assert_eq!(
            sidebar.holding(),
            Holding::Nothing,
            "its row is no longer highlighted"
        );
        assert_eq!(sidebar.notice.as_deref(), Some(PALETTE_ROW_GONE));
        sidebar.light(bipa);
        sidebar.act_on_row(&target, Action::Reorder, moment(160_000).system);
        assert_eq!(
            sidebar.holding(),
            Holding::Reorder,
            "on the row it named, it runs"
        );
    }

    #[test]
    fn a_floating_menu_ends_the_search_under_it_and_holds_the_keys_until_it_closes() {
        let mut sidebar = two_sessions();
        sit(&mut sidebar, &view_of("older", 10, 1));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "/old");
        assert_eq!(sidebar.holding(), Holding::Search);
        float_a_menu(&mut sidebar);
        assert_eq!(sidebar.holding(), Holding::Afloat);
        assert!(
            sidebar.search().is_none(),
            "the menu floats where the search was"
        );
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000));
        assert!(sidebar.floating().is_none());
        assert_eq!(
            sidebar.holding(),
            Holding::Nothing,
            "Esc gives the keys back"
        );
        float_a_menu(&mut sidebar);
        sidebar.focus(Keys::Lent, test_wall());
        assert!(
            sidebar.floating().is_none(),
            "a box over the sidebar closes the menu"
        );
        assert_eq!(sidebar.holding(), Holding::Nothing);
    }

    #[test]
    fn the_brief_s_keys_brief_you_by_the_row_the_menu_was_for() {
        let mut sidebar = two_sessions();
        let mut host = sidebar.host.clone().expect("a host");
        let id = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";
        host.agents[0].session_id = Some(id.to_owned());
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/brief".to_owned(),
            upstream: None,
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout {
            head,
            changes,
            stashes: 0,
            repository: None,
        };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/lune/main".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        step_onto_its_first_view(&mut sidebar);
        let older = || SessionName::parse("older").unwrap();
        let fold = Note {
            on: Machine::Local,
            session: older(),
            view: view_key(10, 1),
            name: "fold".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: Some("feat/brief".to_owned()),
            cwd: "/w/lune/main".to_owned(),
            transcript: Transcript::Named(SessionId::parse(id).unwrap()),
        };
        let brief = |over: Over| Asked::Brief(Brief::new(over, vec![fold.clone()]).unwrap());
        let here = |sessions: [&str; 2]| {
            let sessions = sessions.map(|it| (Machine::Local, SessionName::parse(it).unwrap()));
            Over::Sessions {
                sessions: sessions.into(),
                unknown: Vec::new(),
            }
        };
        let every = || here(["older", "newer"]);
        let session = || Over::Session(Machine::Local, older());
        let agent = Over::Agent(Machine::Local, older(), view_key(10, 1));
        for (key, over) in [('z', every()), ('s', session()), ('h', agent)] {
            type_in(&mut sidebar, "b");
            assert_eq!(
                sidebar.key(&char(key), moment(160_000)),
                brief(over),
                "{key}"
            );
            assert!(sidebar.open().is_none(), "{key}");
        }
        sidebar.step(1);
        type_in(&mut sidebar, "bh");
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("brief: no agent in this view")
        );
        sidebar.step(1);
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents in newer"));
        sidebar.step(-2);
        type_in(&mut sidebar, "h");
        let tab = WorktreePlace {
            project: project_row("older", 10),
            worktree: tab_of(10, 0),
        };
        assert_eq!(lit_place(&sidebar), Some(Place::Worktree(tab)));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), brief(session()));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(every()));
    }

    #[test]
    fn a_menu_held_over_a_mux_restart_briefs_no_view_that_took_its_number() {
        let mut sidebar = fresh();
        let views = || vec![session("work", 10, vec![view(1, 0, "/w/a", "claude")])];
        let fold = in_own(agent("work", 1, Status::Idle, Some("fold")), 10, 1);
        sidebar.hear(host(views(), vec![fold]));
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(view_of("work", 10, 1)));
        type_in(&mut sidebar, "b");
        let restarted = |key: api::TerminalKey| api::TerminalKey {
            made_at: key.made_at.map(|at| Timestamp { nanos: 1, ..at }),
            ..key
        };
        let lint = agent("work", 1, Status::Idle, Some("lint"));
        let lint = Agent {
            terminal: Some(restarted(key_in(10, 1))),
            ..lint
        };
        let mut renumbered = host(views(), vec![lint]);
        let held = renumbered
            .projects
            .as_mut()
            .and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = held else {
            panic!("kept projects")
        };
        let terminals = kept
            .projects
            .iter_mut()
            .flat_map(|project| match &mut project.holds {
                Some(api::project::Holds::Clone(clone)) => clone.worktrees.iter_mut(),
                _ => panic!("a clone"),
            });
        for terminal in terminals.flat_map(|worktree| &mut worktree.terminals) {
            let Some(api::terminal::Found::Key(key)) = &mut terminal.found else {
                continue;
            };
            *key = restarted(*key);
        }
        sidebar.hear(renumbered);
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), Asked::Nothing);
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("brief: no agent in this view")
        );
    }

    #[test]
    fn a_brief_of_pi_s_agents_alone_leaves_a_note() {
        let mut sidebar = fresh();
        let (of_pi_id, of_claude_id) = ("01a05a24-2bcb-7198", "7c22-5d4c-3b2a");
        let of_pi = |agent: Agent| Agent {
            harness: Some(Harness::Pi.into()),
            session_id: Some(of_pi_id.to_owned()),
            ..agent
        };
        let pi = of_pi(agent("both", 1, Status::Idle, Some("pi")));
        let claude = Agent {
            session_id: Some(of_claude_id.to_owned()),
            ..agent("both", 2, Status::Idle, Some("claude"))
        };
        let alone = of_pi(agent("alone", 3, Status::Idle, Some("alone")));
        let sessions = vec![
            session(
                "both",
                10,
                vec![view(1, 0, "/w/a", "pi"), view(2, 1, "/w/b", "claude")],
            ),
            session("alone", 20, vec![view(3, 0, "/w/c", "pi")]),
        ];
        sidebar.hear(host(sessions, vec![pi, claude, alone]));
        sidebar.focus(Keys::Sidebar, test_wall());
        let note = "brief: lune reads only Claude Code's and Antigravity's transcripts so far";

        for (place, key) in [
            (view_of("both", 10, 1), 'h'),
            (view_of("alone", 20, 3), 's'),
        ] {
            sidebar.light(Spot::Local(place));
            type_in(&mut sidebar, "b");
            assert_eq!(
                sidebar.key(&char(key), moment(160_000)),
                Asked::Nothing,
                "{key}"
            );
            assert_eq!(sidebar.notice.as_deref(), Some(note), "{key}");
        }

        sidebar.light(Spot::Local(view_of("both", 10, 1)));
        type_in(&mut sidebar, "b");
        let told = |id: u32, name: &str, cwd: &str, transcript| Note {
            on: Machine::Local,
            session: SessionName::parse("both").unwrap(),
            view: view_key(10, id),
            name: name.to_owned(),
            status: "idle".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: None,
            cwd: cwd.to_owned(),
            transcript,
        };
        let notes = vec![
            told(1, "pi", "/w/a", Transcript::NotRead),
            told(
                2,
                "claude",
                "/w/b",
                Transcript::Named(SessionId::parse(of_claude_id).unwrap()),
            ),
        ];
        let over = Over::Session(Machine::Local, SessionName::parse("both").unwrap());
        let brief = Asked::Brief(Brief::new(over, notes).unwrap());
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), brief);
    }

    #[test]
    fn the_brief_goes_in_luned_s_order_with_this_machine_first() {
        let mut sidebar = fresh();
        sidebar.hear(two_each());
        let on = || Machine::Remote(nixos());
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "b");
        let name = |name: &str| SessionName::parse(name).unwrap();
        let sessions = vec![
            (Machine::Local, name("main")),
            (Machine::Local, name("work")),
            (on(), name("main")),
            (on(), name("dev")),
        ];
        let unknown = [("me@vps", "unreachable"), ("pi", "reaching…")];
        let unknown = unknown.map(|(it, word)| (it.parse().unwrap(), word)).into();
        let fix = Note {
            on: on(),
            session: name("main"),
            view: view_key(20, 1),
            name: "fix".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: None,
            cwd: "/w/b".to_owned(),
            transcript: Transcript::Unnamed,
        };
        let every = Brief::new(Over::Sessions { sessions, unknown }, vec![fix]).unwrap();
        assert_eq!(
            sidebar.key(&char('z'), moment(160_000)),
            Asked::Brief(every)
        );
    }

    #[test]
    fn the_brief_s_keys_brief_you_on_another_machine_s_agents() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let on = || Machine::Remote(nixos());
        let main = || SessionName::parse("main").unwrap();
        let fix = Note {
            on: on(),
            session: main(),
            view: view_key(20, 1),
            name: "fix".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: None,
            cwd: "/w/b".to_owned(),
            transcript: Transcript::Unnamed,
        };
        let brief = |over: Over| Asked::Brief(Brief::new(over, vec![fix.clone()]).unwrap());
        let unknown = [("me@vps", "unreachable"), ("pi", "reaching…")];
        let unknown = unknown.map(|(it, word)| (it.parse().unwrap(), word)).into();
        let every = Over::Sessions {
            sessions: vec![(Machine::Local, main()), (on(), main())],
            unknown,
        };
        let session = || Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        let view = view_of("main", 20, 1);
        for (spot, key, over) in [
            (session(), 'z', every),
            (session(), 's', Over::Session(on(), main())),
            (session(), 'h', Over::Session(on(), main())),
            (
                Spot::Remote(nixos(), view.clone()),
                'h',
                Over::Agent(on(), main(), view_key(20, 1)),
            ),
        ] {
            sidebar.light(spot);
            type_in(&mut sidebar, "b");
            assert_eq!(
                sidebar.key(&char(key), moment(160_000)),
                brief(over),
                "{key}"
            );
        }
        let here = || Spot::Local(Place::Session(listed("main", 10)));
        sidebar.light(here());
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents in main"));
        sidebar.light(Spot::Remote(nixos(), view));
        type_in(&mut sidebar, "b");
        let mut host = machines();
        host.remotes[0] = remote("nixos", down(100));
        sidebar.hear(host.clone());
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), Asked::Nothing);
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("brief: luned does not reach nixos now")
        );
        sidebar.light(here());
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), Asked::Nothing);
        let nowhere = "brief: no agents to brief you on; luned does not reach nixos, me@vps, pi \
                       now";
        assert_eq!(sidebar.notice.as_deref(), Some(nowhere));
        sidebar.hear(machines());
        sidebar.light(session());
        type_in(&mut sidebar, "b");
        host.remotes.remove(0);
        sidebar.hear(host);
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("brief: luned does not reach nixos now")
        );
    }

    #[test]
    fn a_close_asks_first_while_lune_cannot_tell_the_agents() {
        let unread = "luned cannot read 1 of Claude Code's session files";
        let unplaced = format!("{unread}, nor tell where its agent runs");
        let away: fn(&mut Sidebar) = |sidebar| {
            let _ = sidebar.lost("gone".to_owned());
        };
        let unreadable: fn(&mut Sidebar) = |sidebar| {
            let host = sidebar
                .host
                .clone()
                .expect("the sidebar has heard the host");
            sidebar.hear(Host { unread: 1, ..host });
        };
        let away = (away, "luned: gone", "luned does not answer");
        for (untold, note, why) in [away, (unreadable, unread, unplaced.as_str())] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            untold(&mut sidebar);
            sidebar.end(true);
            type_in(&mut sidebar, "d");
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
            let lines = screen(&mut sidebar, true, 160, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let asks = format!("Close the view? lune cannot tell which agents it ends: {why}");
            assert!(lines.contains(&note), "{lines:#?}");
            assert!(lines.contains(&asks.as_str()), "{lines:#?}");
            let act = Act::View(view_key(20, 1));
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), close_here(act, 1));
        }
    }

    #[test]
    fn a_close_asks_first_where_an_agent_s_session_file_does_not_read() {
        for (view, asks) in [(1, true), (7, false)] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar, test_wall());
            let host = sidebar
                .host
                .clone()
                .expect("the sidebar has heard the host");
            let file = UnreadFile {
                pid: 200,
                terminal: Some(in_named("newer", view)),
            };
            let unread_files = vec![file];
            sidebar.hear(Host {
                unread: 1,
                unread_files,
                ..host
            });
            sidebar.end(true);
            type_in(&mut sidebar, "d");
            let act = Act::View(view_key(20, 1));
            let went = sidebar.key(&char('d'), moment(160_000));
            let lines = screen(&mut sidebar, true, 120, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let note = "luned cannot read 1 of Claude Code's session files";
            assert!(lines.contains(&note), "{lines:#?}");
            let asked = lines.contains(&"Close the view? 1 agent, 1 unknown");
            assert_eq!(
                (went == close_here(act, 1), asked),
                (!asks, asks),
                "view {view}"
            );
        }
    }

    #[test]
    fn a_close_of_a_worktree_asks_first_where_a_file_in_it_does_not_read() {
        let in_own = UnreadFile {
            pid: 200,
            terminal: Some(key_in(1, 1)),
        };
        let in_other = UnreadFile {
            terminal: Some(key_in(10, 1)),
            ..in_own
        };
        let worktree = tab_of(1, 0);
        let (views, seen) = (vec![view_key(1, 1).key()], vec![ViewId::of(1).unwrap()]);
        let close = Act::Worktree {
            worktree,
            views,
            seen,
        };
        for (file, asks) in [(in_own, true), (in_other, false)] {
            let mut sidebar = fresh();
            let views = || vec![view(1, 0, "/w/lune/main", "fish")];
            let sessions = vec![
                session("twin", 10, views()),
                own_session("twin", 1, views()),
            ];
            let unread_files = vec![file];
            sidebar.hear(Host {
                unread: 1,
                unread_files,
                ..host(sessions, Vec::new())
            });
            let place = WorktreePlace {
                project: project_row("twin", 1),
                worktree,
            };
            sidebar.focus(Keys::Sidebar, test_wall());
            sidebar.light(Spot::Local(Place::Worktree(place)));
            type_in(&mut sidebar, "d");
            let went = sidebar.key(&char('d'), moment(160_000));
            let lines = screen(&mut sidebar, true, 60, 16);
            let question = " Close the worktree? 1 agent, 1 unknown";
            let asked = lines.iter().any(|line| line.trim_end() == question);
            assert_eq!(
                (went == close_here(close.clone(), 1), asked),
                (!asks, asks),
                "{lines:#?}"
            );
        }
    }

    #[test]
    fn a_close_goes_past_the_view_to_a_project_s_row_whose_close_asks_first() {
        let mut sidebar = on_nvim();
        close_nvim(&mut sidebar);
        assert!(sidebar.open().is_none());
        sidebar.hear(with_nvim());
        let waits = "a listing heard before the close lands leaves the highlight on the view";
        assert_eq!(
            lit_place(&sidebar),
            Some(tab_view_of("older", 10, 1, 2)),
            "{waits}"
        );
        sidebar.hear(without_nvim());
        assert_eq!(lit_place(&sidebar), Some(past_nvim()));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 40, 16);
        assert_eq!(lines[14], " Close project newer? no agent", "{lines:?}");
        let asked = sidebar.key(&char('d'), moment(160_000));
        let newer = matches!(&asked, Asked::Close { act: Act::Project { session, .. }, .. }
            if session.as_str() == "newer");
        assert!(newer, "{asked:?}");
    }

    fn without_nvim() -> Host {
        host(
            vec![
                session("newer", 20, vec![view(1, 0, "/w/b", "fish")]),
                session("older", 10, vec![view(1, 0, "/w/lune/main", "claude")]),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        )
    }

    fn on_nvim() -> Sidebar {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.step(2);
        assert_eq!(lit_place(&sidebar), Some(tab_view_of("older", 10, 1, 2)));
        sidebar
    }

    fn past_nvim() -> Place {
        Place::Session(listed("newer", 20))
    }

    fn closes_nvim() -> Act {
        Act::View(view_key(10, 2))
    }

    fn close_nvim(sidebar: &mut Sidebar) -> u64 {
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let asked = sidebar.key(&char('d'), moment(160_000));
        let Asked::Close { ticket, .. } = asked else {
            panic!("d d closes the view: {asked:?}");
        };
        assert_eq!(asked, close_here(closes_nvim(), ticket));
        ticket
    }

    fn where_nvim_going_by_itself_leaves_it() -> Option<Place> {
        let mut sidebar = on_nvim();
        sidebar.hear(without_nvim());
        let left = lit_place(&sidebar);
        assert_ne!(
            left,
            Some(past_nvim()),
            "it lands apart from where a close of it goes"
        );
        left
    }

    #[test]
    fn a_close_s_landing_goes_once_the_highlight_moves() {
        let mut sidebar = on_nvim();
        close_nvim(&mut sidebar);
        type_in(&mut sidebar, "hl");
        let back = "h folds its worktree and l unfolds it, back onto the view";
        assert_eq!(
            lit_place(&sidebar),
            Some(tab_view_of("older", 10, 1, 2)),
            "{back}"
        );
        sidebar.hear(without_nvim());
        assert_eq!(lit_place(&sidebar), where_nvim_going_by_itself_leaves_it());
    }

    fn refuse(sidebar: &mut Sidebar, ticket: u64) {
        let refused = Unchanged::Refused("luned refused".to_owned());
        sidebar.unclosed(&Machine::Local, &closes_nvim(), ticket, refused);
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("menu: not closed: luned refused")
        );
        assert_eq!(lit_place(sidebar), Some(tab_view_of("older", 10, 1, 2)));
    }

    fn nvim_closed_twice_with_the_others_refused() -> (Sidebar, u64) {
        let mut sidebar = on_nvim();
        let earlier = close_nvim(&mut sidebar);
        let picked = sidebar.ticket();
        let later = close_nvim(&mut sidebar);
        refuse(&mut sidebar, earlier);
        refuse(&mut sidebar, picked);
        (sidebar, later)
    }

    #[test]
    fn a_refused_close_drops_the_landing_of_its_own_close_only() {
        let (mut sidebar, _) = nvim_closed_twice_with_the_others_refused();
        sidebar.hear(without_nvim());
        let kept = "an earlier d d's refusal or the picker's leaves the later d d's landing";
        assert_eq!(lit_place(&sidebar), Some(past_nvim()), "{kept}");
        let (mut sidebar, later) = nvim_closed_twice_with_the_others_refused();
        refuse(&mut sidebar, later);
        sidebar.hear(without_nvim());
        assert_eq!(lit_place(&sidebar), where_nvim_going_by_itself_leaves_it());
        let (mut sidebar, later) = nvim_closed_twice_with_the_others_refused();
        let late = Unchanged::InDoubt("luned did not answer in 15s".to_owned());
        sidebar.unclosed(&Machine::Local, &closes_nvim(), later, late);
        let maybe = "menu: luned did not answer in 15s, so the view may be closed";
        assert_eq!(sidebar.notice.as_deref(), Some(maybe));
        sidebar.hear(without_nvim());
        assert_eq!(
            lit_place(&sidebar),
            Some(past_nvim()),
            "a close in doubt keeps its landing"
        );
    }

    fn last_with(views: &[u32]) -> Host {
        let shells = views
            .iter()
            .map(|id| view(*id, *id - 1, "/w/lune/main", "fish"))
            .collect();
        let first = own_session("first", 1, vec![view(1, 0, "/w/lune/main", "fish")]);
        host(vec![first, own_session("last", 2, shells)], Vec::new())
    }

    fn on_the_last_row() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(last_with(&[1, 2]));
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.step(9);
        assert_eq!(lit_place(&sidebar), Some(tab_view_of("last", 2, 1, 2)));
        sidebar
    }

    #[test]
    fn a_key_that_leaves_the_highlight_on_its_row_keeps_the_landing() {
        let mut sidebar = on_the_last_row();
        sidebar.hear(last_with(&[1]));
        let landing = Some(own_view_of("last", 2, 1));
        let apart = "a view gone by itself leaves the highlight apart from the landing";
        assert_ne!(lit_place(&sidebar), landing, "{apart}");
        let mut sidebar = on_the_last_row();
        type_in(&mut sidebar, "d");
        let closes = Act::View(view_key(2, 2));
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            close_here(closes, 1)
        );
        type_in(&mut sidebar, "j");
        sidebar.hear(last_with(&[1]));
        assert_eq!(
            lit_place(&sidebar),
            landing,
            "j on the last row moves nothing"
        );
    }

    fn twins(first: bool) -> Host {
        let views = || vec![view(1, 0, "/w/lune/main", "fish")];
        let mut sessions = vec![
            own_session("twin", 2, views()),
            own_session("last", 3, views()),
        ];
        if first {
            sessions.insert(0, own_session("twin", 1, views()));
        }
        host(sessions, Vec::new())
    }

    #[test]
    fn a_project_s_close_lands_on_another_project_of_its_name() {
        let mut sidebar = fresh();
        sidebar.hear(twins(true));
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(Place::Session(Listed::new_project("twin", 1))));
        type_in(&mut sidebar, "d");
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            Asked::Nothing,
            "it asks first"
        );
        let closed = project_closed("twin", 1, &[1000], &[1]);
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            close_here(closed, 1)
        );
        sidebar.hear(twins(false));
        let twin = Some(Place::Session(Listed::new_project("twin", 2)));
        assert_eq!(
            lit_place(&sidebar),
            twin,
            "the other twin stays, so the highlight lands on it"
        );
    }

    #[test]
    fn a_close_that_ends_an_agent_at_work_asks_and_looks_again() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_view_of("older", 1, 1));
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        assert_eq!(bottom(&mut sidebar), " <y> Yes <BS> Back <ESC> Close");
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13], " Close the view? 1 agent, 1");
        let act = Act::View(view_key(1, 1));
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), close_here(act, 1));
        sidebar.light(Spot::Local(Place::Session(own_listed("older", 1))));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(
            lines[13..15],
            [" Close project older? 1 agent,", " 1 waiting"]
        );
        let closed = project_closed("older", 1, &[1000, 1001], &[1, 2]);
        assert_eq!(
            sidebar.key(&char('d'), moment(160_000)),
            close_here(closed, 2)
        );
        assert!(sidebar.open().is_none());
    }

    #[test]
    fn the_highlight_seeks_the_best_match_only_as_the_search_changes() {
        let mut sidebar = searched();
        let view = |view| Some(tab_view_of("bipa", 10, view - 1, view));
        type_in(&mut sidebar, "/pi");
        assert_eq!(lit_place(&sidebar), view(3));
        sidebar.key(
            &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            moment(160_000),
        );
        sidebar.hear(sidebar.host.clone().expect("heard"));
        assert_eq!(lit_place(&sidebar), view(1));
        type_in(&mut sidebar, "x");
        assert_eq!(lit_place(&sidebar), view(3));
    }

    fn drawn_rows(sidebar: &mut Sidebar) -> (Vec<String>, Vec<usize>) {
        let (lines, lit) = seen(sidebar, true, 32, 28);
        let from = sessions_from(&lines);
        let rows = rows_in(&lines[from..]);
        (rows, lit.iter().map(|y| usize::from(*y) - from).collect())
    }

    #[test]
    fn v_selects_worktrees_that_shifted_j_and_k_move() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "jvjjk");
        let (_, lit) = drawn_rows(&mut sidebar);
        assert_eq!(lit, [1, 2, 3, 4]);
        sidebar.mouse_rows(
            mouse(MouseEventKind::Moved, 10, 4),
            Rect::new(0, 0, 32, 26),
            test_wall(),
        );
        let shifted = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
        let seen = vec![tab_of(10, 0), tab_of(10, 1), tab_of(10, 2)];
        let wanted = vec![tab_of(10, 2), tab_of(10, 0), tab_of(10, 1)];
        let order = Reorder::Worktrees {
            project: ProjectId::of(10).unwrap(),
            seen,
            wanted,
        };
        let asked = Asked::Move {
            on: Machine::Local,
            order,
            ticket: 1,
        };
        assert_eq!(sidebar.key(&shifted('J'), moment(160_000)), asked);
        assert_eq!(
            drawn_rows(&mut sidebar).1,
            [3, 4, 5, 6],
            "the rows move as asked"
        );

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert_eq!(drawn_rows(&mut sidebar).1, [5, 6]);
        assert_eq!(sidebar.key(&shifted('K'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Leave);
    }

    #[test]
    fn a_folded_project_is_selected_from_its_row() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "Ghhv");
        let asked = sidebar.key(&char('K'), moment(160_000));
        let order = projects(&[10, 20, 30], &[10, 30, 20]);
        assert_eq!(
            asked,
            Asked::Move {
                on: Machine::Local,
                order,
                ticket: 1
            }
        );
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let zed = Place::Session(listed("zed", 30));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(zed));
        assert!(matches!(sidebar.mode, Mode::Idle));
    }

    #[test]
    fn a_project_dragged_lands_where_it_shows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 28);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        let row = |name: &str| {
            let at = lines
                .iter()
                .position(|line| line.starts_with(name))
                .unwrap();
            u16::try_from(at).unwrap()
        };
        let (bipa, notes) = (row(" ● bipa"), row(" ● notes"));
        let left = MouseButton::Left;
        sidebar.mouse_rows(
            mouse(MouseEventKind::Down(left), 10, notes),
            area,
            test_wall(),
        );
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 20), area, test_wall()),
            Asked::Take
        );
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[16], " ● notes", "{rows:#?}");
        assert_eq!(lit, [16, 17, 18]);
        let order = projects(&[10, 20, 30], &[10, 30, 20]);
        let asked = Asked::Move {
            on: Machine::Local,
            order,
            ticket: 1,
        };
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 20), area, test_wall()),
            asked
        );

        let lines = screen(&mut sidebar, true, area.width, area.height);
        let notes = lines
            .iter()
            .position(|line| line.starts_with(" ● notes"))
            .unwrap();
        let notes = u16::try_from(notes).unwrap();
        sidebar.mouse_rows(
            mouse(MouseEventKind::Down(left), 10, notes),
            area,
            test_wall(),
        );
        sidebar.mouse_rows(
            mouse(MouseEventKind::Drag(left), 10, bipa),
            area,
            test_wall(),
        );
        let order = projects(&[10, 30, 20], &[20, 10, 30]);
        let asked = Asked::Move {
            on: Machine::Local,
            order,
            ticket: 1,
        };
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, bipa), area, test_wall()),
            asked
        );
    }

    #[test]
    fn a_folded_project_dragged_onto_the_name_above_or_below_lands_past_it() {
        let left = MouseButton::Left;
        let area = Rect::new(0, 0, 32, 28);
        for (onto, wanted) in [(" ▸ bipa", [20, 10, 30]), (" ▸ zed", [10, 30, 20])] {
            let mut sidebar = searched();
            for id in [10, 20, 30] {
                sidebar.edit(|state| {
                    let _ = state.folds.set(
                        Fold::Project(&Machine::Local, ProjectId::of(id).unwrap()),
                        true,
                    );
                });
            }
            sidebar.rebuild(test_wall());
            let lines = screen(&mut sidebar, true, area.width, area.height);
            let row = |name: &str| {
                let at = lines
                    .iter()
                    .position(|line| line.starts_with(name))
                    .expect(name);
                u16::try_from(at).unwrap()
            };
            let (notes, to) = (row(" ▸ notes"), row(onto));
            sidebar.mouse_rows(
                mouse(MouseEventKind::Down(left), 10, notes),
                area,
                test_wall(),
            );
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, to), area, test_wall());
            let order = projects(&[10, 20, 30], &wanted);
            let asked = Asked::Move {
                on: Machine::Local,
                order,
                ticket: 1,
            };
            let up = sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, to), area, test_wall());
            assert_eq!(up, asked, "{onto}");
        }
    }

    #[test]
    fn a_project_lifted_goes_back_off_the_sidebar_or_on_esc() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        let lift_notes = |sidebar: &mut Sidebar| {
            sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 12), area, test_wall());
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 5), area, test_wall());
            assert_ne!(drawn_rows(sidebar).0, listed);
        };
        lift_notes(&mut sidebar);
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 40, 5), area, test_wall());
        assert_eq!(
            drawn_rows(&mut sidebar),
            (listed.clone(), (8..11).collect())
        );
        sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 40, 5), area, test_wall());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_notes(&mut sidebar);
        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)),
            Asked::Nothing
        );
        assert!(!sidebar.pressed());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_notes(&mut sidebar);
        sidebar.mouse_rows(mouse(MouseEventKind::Moved, 10, 5), area, test_wall());
        sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 5), area, test_wall());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    #[test]
    fn a_row_reordered_by_v_or_dragged_holds_the_keys_until_it_lands() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        assert_eq!(sidebar.holding(), Holding::Nothing);
        sidebar.light(Spot::Local(Place::Session(listed("bipa", 10))));
        type_in(&mut sidebar, "v");
        assert_eq!(sidebar.holding(), Holding::Reorder, "v reorders");
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000));
        assert_eq!(sidebar.holding(), Holding::Nothing, "Esc ends the reorder");

        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 12), area, test_wall());
        assert_eq!(sidebar.holding(), Holding::Press, "the press");
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 5), area, test_wall());
        assert_ne!(drawn_rows(&mut sidebar).0, listed, "a row is lifted");
        assert_eq!(sidebar.holding(), Holding::Press, "the drag");
        sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 5), area, test_wall());
        assert_eq!(sidebar.holding(), Holding::Nothing, "the row landed");
    }

    #[test]
    fn the_left_button_down_has_the_keys() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        let area = Rect::new(0, 0, 32, 26);
        drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 12), area, test_wall());
        type_in(&mut sidebar, "vn");
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 4), area, test_wall());
        let asked = sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 4), area, test_wall());
        let order = projects(&[10, 20, 30], &[20, 10, 30]);
        assert_eq!(
            asked,
            Asked::Move {
                on: Machine::Local,
                order,
                ticket: 1
            }
        );
        assert!(sidebar.open().is_none());

        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 6), area, test_wall());
        assert_eq!(
            sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)),
            Asked::Nothing
        );
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 6), area, test_wall()),
            Asked::Nothing
        );
    }

    #[test]
    fn a_search_typed_lifts_no_session() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "/z");
        let area = Rect::new(0, 0, 32, 22);
        let (listed, _) = drawn_rows(&mut sidebar);
        assert_eq!(listed[0], " ● zed");
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 4), area, test_wall());
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 6), area, test_wall()),
            Asked::Nothing
        );
        sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 10, 6), area, test_wall());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    fn framed(sidebar: &mut Sidebar, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        sidebar.place_rows(area);
        sidebar.draw_rows(&mut buffer, area, test_wall());
        buffer
    }

    fn reversed(buffer: &Buffer) -> Vec<(u16, u16)> {
        let on = |at: &Position| buffer[*at].modifier.contains(Modifier::REVERSED);
        buffer
            .area
            .positions()
            .filter(on)
            .map(|at| (at.x, at.y))
            .collect()
    }

    fn dragged(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) {
        let left = MouseButton::Left;
        sidebar.mouse_rows(
            mouse(MouseEventKind::Down(left), from.0, from.1),
            area,
            test_wall(),
        );
        sidebar.mouse_rows(
            mouse(MouseEventKind::Drag(left), to.0, to.1),
            area,
            test_wall(),
        );
    }

    fn swept(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) -> Asked {
        dragged(sidebar, area, from, to);
        sidebar.mouse_rows(
            mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1),
            area,
            test_wall(),
        )
    }

    fn excerpt(text: &str) -> Excerpt {
        Excerpt::of(text.to_owned()).expect("a test excerpts some text")
    }

    fn copied(text: &str) -> Asked {
        Asked::CopyText(excerpt(text))
    }

    #[test]
    fn a_drag_copies_what_it_went_over_as_it_lets_go() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let here = sidebar.lit_spot().cloned();
        assert_eq!(
            swept(&mut sidebar, area, (7, 6), (13, 6)),
            copied("a-p-i-x")
        );
        assert_eq!(swept(&mut sidebar, area, (13, 6), (9, 6)), copied("p-i-x"));
        dragged(&mut sidebar, area, (7, 6), (9, 6));
        let up = sidebar.mouse_rows(
            mouse(MouseEventKind::Up(MouseButton::Left), 13, 6),
            area,
            test_wall(),
        );
        assert_eq!(up, copied("a-p-i-x"));
        assert_eq!(swept(&mut sidebar, area, (3, 0), (6, 0)), copied("lune"));
        assert_eq!(sidebar.lit_spot().cloned(), here);

        dragged(&mut sidebar, area, (9, 6), (11, 6));
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 40, 8), area, test_wall());
        assert!(sidebar.pressed());
        let up = sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 40, 8), area, test_wall());
        assert_eq!(up, copied("p-i-x\n  ▾ Tab #2\n       dda-bank-slip"));
        assert!(!sidebar.pressed());

        assert_eq!(swept(&mut sidebar, area, (5, 1), (25, 1)), Asked::Nothing);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    #[test]
    fn a_click_is_a_click_though_the_hand_moves_a_cell() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let api = || go_here(view_of("bipa", 10, 1));
        assert_eq!(swept(&mut sidebar, area, (10, 6), (11, 6)), api());
        assert_eq!(swept(&mut sidebar, area, (10, 6), (9, 6)), api());
        assert_eq!(swept(&mut sidebar, area, (10, 6), (12, 6)), copied("-i-"));
        assert_eq!(swept(&mut sidebar, area, (10, 6), (11, 7)), Asked::Nothing);
    }

    #[test]
    fn a_drag_along_a_project_s_row_sweeps_and_one_off_it_lifts() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 3, 4), area, test_wall());
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 6, 4), area, test_wall());
        assert_eq!(
            sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 7, 5), area, test_wall()),
            Asked::Nothing
        );
        let up = sidebar.mouse_rows(mouse(MouseEventKind::Up(left), 7, 5), area, test_wall());
        assert_eq!(up, copied("bipa\n  ▾ Tab"));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        assert_eq!(swept(&mut sidebar, area, (3, 4), (6, 4)), copied("bipa"));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 12), area, test_wall());
        let drag = |sidebar: &mut Sidebar, row| {
            sidebar.mouse_rows(
                mouse(MouseEventKind::Drag(left), 11, row),
                area,
                test_wall(),
            )
        };
        assert_eq!(drag(&mut sidebar, 12), Asked::Nothing);
        assert_eq!(drag(&mut sidebar, 13), Asked::Take);
    }

    #[test]
    fn a_sweep_shows_reversed_until_the_next_click() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
        dragged(&mut sidebar, area, (7, 6), (9, 6));
        let cells = [(7, 6), (8, 6), (9, 6)];
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        let up = sidebar.mouse_rows(
            mouse(MouseEventKind::Up(MouseButton::Left), 9, 6),
            area,
            test_wall(),
        );
        assert_eq!(up, copied("a-p"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.mouse_rows(mouse(MouseEventKind::Moved, 20, 13), area, test_wall());
        sidebar.hear(searched().host.clone().unwrap());
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.hear(first_view_is(1, "a-p-i-y"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        sidebar.unsweep();
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    fn first_view_is(id: u32, command: &str) -> Host {
        let mut sessions = searched_sessions();
        let first = &mut sessions[0].views[0];
        first.id = id;
        first.command = Some(command.to_owned());
        host(sessions, Vec::new())
    }

    #[test]
    fn a_sweep_goes_once_the_text_moves_from_under_it() {
        let area = Rect::new(0, 0, 32, 26);
        let more_rows = || {
            let views = vec![view(1, 0, "/w/a", "a-p-i-x"), view(2, 1, "/w/b", "b")];
            host(vec![session("bipa", 10, views)], Vec::new())
        };
        let branch_too = || {
            let mut listed = searched().host.clone().unwrap();
            let (statuses, pulls) = (&mut listed.git_statuses, &mut listed.pull_requests);
            checked_out(statuses, pulls, "/w/a", "main", None);
            listed
        };
        let moves: [&dyn Fn(&mut Sidebar); 7] = [
            &|sidebar| {
                sidebar.mouse_rows(mouse(MouseEventKind::ScrollDown, 7, 5), area, test_wall());
            },
            &|sidebar| {
                sidebar.hear(more_rows());
            },
            &|sidebar| {
                sidebar.hear(first_view_is(9, "other"));
            },
            &|sidebar| {
                sidebar.notify("other word ".repeat(60));
            },
            &|sidebar| {
                sidebar.hear(branch_too());
            },
            &|sidebar| {
                framed(sidebar, Rect::new(0, 0, 28, 26));
            },
            &|sidebar| {
                sidebar.collapse_rows(area);
                sidebar.draw_collapsed_rows(&mut Buffer::empty(area), area);
            },
        ];
        for (case, moved) in moves.iter().enumerate() {
            let mut sidebar = searched();
            sidebar.notify("first word".to_owned());
            framed(&mut sidebar, area);
            assert_eq!(swept(&mut sidebar, area, (7, 6), (9, 6)), copied("a-p"));
            moved(&mut sidebar);
            assert_eq!(reversed(&framed(&mut sidebar, area)), [], "case {case}");
        }
    }

    #[test]
    fn a_sweep_stays_while_only_the_notes_below_it_change() {
        let area = Rect::new(0, 0, 32, 26);
        let mut sidebar = searched();
        sidebar.notify("first word".to_owned());
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 6), (9, 6)), copied("a-p"));
        let cells = [(7, 6), (8, 6), (9, 6)];
        let now = moment(160_000);
        sidebar.copied(Copied::text(&excerpt("a-p")), now.steady);
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.notify("other word".to_owned());
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        let gone = now + COPIED_SHOWS;
        let _ = sidebar.ticked(gone, true);
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.key(&char('?'), gone);
        assert_eq!(
            reversed(&framed(&mut sidebar, area)),
            cells,
            "the legend is below it"
        );
    }

    #[test]
    fn a_sweep_over_a_note_goes_once_another_note_moves_it() {
        let area = Rect::new(0, 0, 32, 26);
        let mut sidebar = searched();
        sidebar.notify("first word".to_owned());
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (1, 25), (5, 25)), copied("first"));
        assert_eq!(reversed(&framed(&mut sidebar, area)).len(), 5);
        sidebar.copied(Copied::text(&excerpt("first")), moment(160_000).steady);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    #[test]
    fn a_sweep_let_go_otherwise_copies_nothing() {
        let area = Rect::new(0, 0, 32, 26);
        let esc = KeyEvent::from(KeyCode::Esc);
        let lets_go: [&dyn Fn(&mut Sidebar); 2] = [
            &|sidebar| {
                sidebar.key(&esc, moment(160_000));
            },
            &|sidebar| {
                sidebar.mouse_rows(mouse(MouseEventKind::Moved, 9, 6), area, test_wall());
            },
        ];
        for (case, let_go) in lets_go.iter().enumerate() {
            let mut sidebar = searched();
            sidebar.notify("a note".to_owned());
            framed(&mut sidebar, area);
            dragged(&mut sidebar, area, (7, 6), (9, 6));
            let_go(&mut sidebar);
            assert!(!sidebar.pressed(), "case {case}");
            assert_eq!(sidebar.notice.as_deref(), Some("a note"), "case {case}");
            assert_eq!(reversed(&framed(&mut sidebar, area)), [], "case {case}");
            let up = sidebar.mouse_rows(
                mouse(MouseEventKind::Up(MouseButton::Left), 9, 6),
                area,
                test_wall(),
            );
            assert_eq!(up, Asked::Nothing, "case {case}");
        }
    }

    #[test]
    fn a_sweep_has_each_note_as_it_was_said() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (why, notice) = (
            "the daemon stopped answering a while ago",
            "something went wrong with what you asked for just now",
        );
        let _ = sidebar.lost(why.to_owned());
        sidebar.notify(notice.to_owned());
        framed(&mut sidebar, area);
        let said = format!(" luned: {why}\n {notice}");
        assert_eq!(swept(&mut sidebar, area, (0, 22), (30, 25)), copied(&said));
    }

    #[test]
    fn a_sweep_has_a_text_cut_short_whole() {
        let long = "a-text-too-long-for-the-sidebar";
        let views = vec![view(1, 1, "/w/a", long)];
        let mut local = host(vec![session(long, 10, views)], Vec::new());
        let (statuses, pulls) = (&mut local.git_statuses, &mut local.pull_requests);
        checked_out(
            statuses,
            pulls,
            "/w/a",
            long,
            Some((open_pull_state(false), LINK)),
        );
        local.remotes = vec![remote("me@a-host-too-long-for-the-sidebar", down(100))];
        let mut sidebar = fresh();
        sidebar.edit(|state| state.this = long.to_owned());
        sidebar.hear(local);
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        for (line, from, whole) in [
            (2, 5, long.to_owned()),
            (4, 3, long.to_owned()),
            (6, 7, format!("{long} #246")),
            (7, 7, long.to_owned()),
            (
                8,
                5,
                "me@a-host-too-long-for-the-sidebar · unreachable 1m".to_owned(),
            ),
        ] {
            assert_eq!(
                swept(&mut sidebar, area, (from, line), (22, line)),
                copied(&whole)
            );
        }
        assert_eq!(swept(&mut sidebar, area, (7, 7), (11, 7)), copied("a-tex"));

        sidebar.mouse_rows(mouse(MouseEventKind::ScrollDown, 7, 5), area, test_wall());
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 4), (22, 4)), copied(long));
    }

    #[test]
    fn a_sweep_finds_the_cut_past_a_halfwidth_sound_mark() {
        let long = "\u{ff8a}\u{ff9e}-a-text-too-long-for-the-sidebar";
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![session("s", 10, vec![view(1, 1, "/w/a", long)])],
            Vec::new(),
        ));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 6), (22, 6)), copied(long));
    }

    #[test]
    fn a_sweep_has_no_control_character_of_what_a_cut_took() {
        let command = "make a-target-too-long-for-the-sidebar\x07echo\x1b[201~end";
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![session("s", 10, vec![view(1, 1, "/w/a", command)])],
            Vec::new(),
        ));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        let whole = "make a-target-too-long-for-the-sidebarecho[201~end";
        assert_eq!(swept(&mut sidebar, area, (7, 6), (22, 6)), copied(whole));
    }

    #[test]
    fn a_project_held_on_the_edge_scrolls_the_rows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 7);
        let first = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 7)[HEADER].clone();
        assert_eq!(first(&mut sidebar), " ▾ \u{f108} MAC");
        let left = MouseButton::Left;
        sidebar.mouse_rows(mouse(MouseEventKind::Down(left), 10, 4), area, test_wall());
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 6), area, test_wall());
        let tick = |sidebar: &mut Sidebar, millis: u64| {
            let _ = sidebar.ticked(moment(160_000 + millis), true);
            first(sidebar)
        };
        assert!(sidebar.scrolling());
        let mut firsts = vec![tick(&mut sidebar, 0), tick(&mut sidebar, 30)];
        firsts.push(tick(&mut sidebar, 60));
        for millis in (120..=1200).step_by(60) {
            firsts.push(tick(&mut sidebar, millis));
        }
        let end = sidebar.scrolling();
        sidebar.mouse_rows(mouse(MouseEventKind::Drag(left), 10, 3), area, test_wall());
        let lifted = [
            "",
            "",
            " ● notes",
            "  ▾ Tab #1",
            "     \u{e795} me/Documents",
        ];
        assert_eq!(firsts[..5], lifted, "one line a SCROLL");
        assert_eq!(firsts[13..15], ["", " ● bipa"], "bipa carried along");
        assert!(!end, "it stops at the last row");
        assert_eq!(tick(&mut sidebar, 1260), "       a-p-i-x");
    }

    fn remote(destination: &str, reached: Reached) -> api::Remote {
        api::Remote {
            destination: destination.to_owned(),
            reached: Some(reached),
        }
    }

    fn up(sessions: Vec<Session>, agents: Vec<Agent>) -> Reached {
        Reached::Host(remote::Host {
            git_statuses: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_files: Vec::new(),
            sayings: Vec::new(),
            projects: Some(projects_of(sessions)),
        })
    }

    fn down(since: i64) -> Reached {
        Reached::Unreached(remote::Unreached {
            since: Some(Timestamp {
                seconds: since,
                nanos: 0,
            }),
            why: "it ended before it reached luned (exit status: 255)".to_owned(),
            stopped: Some(remote::unreached::Stopped::Ended(
                remote::unreached::Ended { code: Some(255) },
            )),
            projects: None,
            ssh_said: Some("ssh: Could not resolve hostname vps".to_owned()),
        })
    }

    fn nixos() -> Destination {
        "nixos".parse().unwrap()
    }

    fn machines() -> Host {
        machines_with(Vec::new(), Vec::new())
    }

    fn machines_with(here: Vec<Session>, there: Vec<Session>) -> Host {
        let main = session("main", 20, vec![view(1, 0, "/w/b", "claude")]);
        let nixos = up(
            [main].into_iter().chain(there).collect(),
            vec![agent("main", 1, Status::Waiting, Some("fix"))],
        );
        let main = session("main", 10, vec![view(1, 0, "/w/a", "fish")]);
        Host {
            remotes: vec![
                remote("nixos", nixos),
                remote("me@vps", down(100)),
                remote("pi", Reached::Reaching(())),
            ],
            ..host([main].into_iter().chain(here).collect(), Vec::new())
        }
    }

    #[test]
    fn each_machine_is_a_row_with_its_projects_under_it_this_one_s_first() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        assert_eq!(
            screen(&mut sidebar, false, 32, 14),
            [
                " ☾ lune  2 projects         ●1",
                "",
                " ▾ \u{f108} MAC",
                "",
                " ● main",
                "  ▾ Tab #1",
                "     \u{e795} w/a",
                " ▾ \u{f108} NIXOS                  ●1",
                "",
                " ● main                     ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fix                  1m",
                " ▸ \u{f108} ME@VPS · unreachable   1m",
                " ▸ \u{f108} PI · reaching…",
            ]
        );
        let buffer = drawn(&mut sidebar, false, 32, 14);
        let dim = |y: u16| buffer[(3, y)].modifier.contains(Modifier::DIM);
        assert_eq!((dim(7), dim(12)), (false, true));
    }

    #[test]
    fn a_machine_folds_its_projects_away_and_unfolds_them() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let nixos_machine = Machine::Remote(nixos());
        let header = Spot::Machine(nixos_machine.clone());
        let project = Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        sidebar.light(Spot::Remote(nixos(), view_of("main", 20, 1)));
        type_in(&mut sidebar, "hh");
        assert_eq!(sidebar.lit_spot().cloned(), Some(project.clone()));
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert_eq!(sidebar.lit_spot().cloned(), Some(header.clone()));
        let folded = [
            " ▾ \u{f108} MAC",
            "",
            " ● main",
            "  ▾ Tab #1",
            "     \u{e795} w/a",
            " ▸ \u{f108} NIXOS                  ●1",
            " ▸ \u{f108} ME@VPS · unreachable   1m",
            " ▸ \u{f108} PI · reaching…",
        ];
        assert_eq!(screen(&mut sidebar, false, 32, 14)[2..10], folded);
        assert!(
            sidebar.row_of(&project).is_none(),
            "its projects go with it"
        );
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));

        type_in(&mut sidebar, "l");
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert!(sidebar.row_of(&project).is_some());
        sidebar.light(header.clone());
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
        assert!(sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        let area = Rect::new(0, 0, 32, 14);
        assert_eq!(click(&mut sidebar, 20, 7, area), Asked::Take);
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert_eq!(click(&mut sidebar, 20, 2, area), Asked::Take);
        assert!(sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(lines[2], " ▸ \u{f108} MAC");
        assert_eq!(lines[3], " ▾ \u{f108} NIXOS                  ●1");

        let vps = Machine::Remote("me@vps".parse().unwrap());
        sidebar.light(Spot::Machine(vps.clone()));
        sidebar.key(&enter, moment(160_000));
        assert!(!sidebar.folds.contains(Fold::Machine(&vps)));
        assert_eq!(
            sidebar.lit_spot(),
            Some(&Spot::Machine(vps)),
            "it has no projects to fold"
        );
    }

    #[test]
    fn a_machine_s_fold_goes_once_it_lists_no_projects() {
        let mut sidebar = fresh();
        let folded = |sidebar: &Sidebar| {
            [Machine::Local, Machine::Remote(nixos())]
                .map(|machine| sidebar.folds.contains(Fold::Machine(&machine)))
        };
        sidebar.hear(machines());
        sidebar.edit(|state| {
            let _ = state.folds.set(Fold::Machine(&Machine::Local), true);
            let _ = state
                .folds
                .set(Fold::Machine(&Machine::Remote(nixos())), true);
        });
        sidebar.hear(machines());
        assert_eq!(folded(&sidebar), [true, true]);
        let none = up(Vec::new(), Vec::new());
        sidebar.hear(Host {
            remotes: vec![remote("nixos", none)],
            ..host(Vec::new(), Vec::new())
        });
        assert_eq!(folded(&sidebar), [false, false]);
        sidebar.hear(machines());
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(lines[2], " ▾ \u{f108} MAC");
        assert_eq!(lines[7], " ▾ \u{f108} NIXOS                  ●1");
    }

    #[test]
    fn a_machine_s_fold_no_lune_saw_go_goes_on_the_first_listing() {
        let mut sidebar = fresh();
        sidebar.edit(|state| {
            let _ = state.folds.set(Fold::Machine(&Machine::Local), true);
        });
        sidebar.hear(host(Vec::new(), Vec::new()));
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        sidebar.hear(host(vec![session("x", 10, Vec::new())], Vec::new()));
        let lines = screen(&mut sidebar, false, 32, 6);
        assert_eq!(lines[2..5], [" ▾ \u{f108} MAC", "", " ● x"]);
    }

    #[test]
    fn h_folds_a_view_s_worktree_then_its_project_and_l_unfolds() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let machine = Machine::Remote(nixos());
        let (main, tab) = (ProjectId::of(20).unwrap(), tab_of(20, 0));
        let fix = view_of("main", 20, 1);
        sidebar.light(Spot::Remote(nixos(), fix));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Worktree(&machine, tab)));
        assert!(!sidebar.folds.contains(Fold::Project(&machine, main)));
        let place = WorktreePlace {
            project: project_row("main", 20),
            worktree: tab,
        };
        assert_eq!(
            sidebar.lit_spot(),
            Some(&Spot::Remote(nixos(), Place::Worktree(place)))
        );
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Project(&machine, main)));
        let project = Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        assert_eq!(sidebar.lit_spot().cloned(), Some(project));
        assert!(!folded(&sidebar, "main"), "this machine's main folds apart");
        type_in(&mut sidebar, "l");
        assert!(!sidebar.folds.contains(Fold::Project(&machine, main)));
        assert!(sidebar.folds.contains(Fold::Worktree(&machine, tab)));
    }

    #[test]
    fn g_r_on_a_far_row_rebases_there_and_g_g_still_goes_to_the_first_row() {
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let name = Said::new("fix".to_owned()).expect("a branch's name");
        let head = Head::Branch {
            name,
            upstream: None,
            changed: None,
        };
        let repository = Said::new("/w".to_owned());
        let stashes = 0;
        let status = GitStatus::Checkout {
            repository,
            head,
            changes: Changes::default(),
            stashes,
        };
        there.git_statuses.push(status.to_api("/w/b"));
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        let place = view_place("main", 20, 0, 1);
        let spot = Spot::Remote(nixos(), Place::View(place));
        sidebar.light(spot.clone());
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('g'), now - PAUSE), Asked::Nothing);
        assert_eq!(
            clicks(&mut sidebar, 17),
            [
                (format!("─ Go, or git {}", "─".repeat(18)), None),
                (" g  First row".to_owned(), Some(on_legend('g'))),
                (" r  Rebase onto upstream".to_owned(), Some(on_legend('r'))),
            ]
        );
        let dir = Directory::parse("/w/b".to_owned()).unwrap();
        let on = Machine::Remote(nixos());
        let number = sidebar.menus + 1;
        let branch = Said::new("fix".to_owned()).unwrap();
        assert_eq!(
            sidebar.key(&char('r'), now),
            Asked::Rebase {
                menu: number,
                on,
                dir,
                branch
            }
        );
        assert!(sidebar.open().expect("the menu").rebasing());
        let rebased = rebases::Rebased {
            branch: Said::new("fix".to_owned()).unwrap(),
            onto: Said::new("origin/main".to_owned()).unwrap(),
            came: rebases::Came::Already,
        };
        assert_eq!(sidebar.rebased(number, Ok(rebased)), Asked::Nothing);
        assert!(sidebar.open().is_none());
        assert_eq!(
            sidebar.notice.as_deref(),
            Some("menu: fix is on origin/main already")
        );

        sidebar.key(&char('g'), now);
        sidebar.key(&char('g'), now);
        assert!(sidebar.open().is_none(), "g g opens no menu");
        assert_ne!(
            sidebar.lit_spot().cloned(),
            Some(spot),
            "g g goes to the first row"
        );
    }

    #[test]
    fn d_m_on_a_far_row_goes_by_that_machine_and_a_note_says_what_stays() {
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let name = Said::new("fix".to_owned()).expect("a branch's name");
        let head = Head::Branch {
            name,
            upstream: None,
            changed: None,
        };
        let repository = Said::new("/w".to_owned());
        let stashes = 0;
        let status = GitStatus::Checkout {
            repository,
            head,
            changes: Changes::default(),
            stashes,
        };
        there.git_statuses.push(status.to_api("/w/b"));
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        let place = view_place("main", 20, 0, 1);
        let place = Place::View(place);
        sidebar.light(Spot::Remote(nixos(), place));
        let now = moment(160_000);
        let on = Machine::Remote(nixos());
        sidebar.key(&char('d'), now);
        let number = sidebar.open().expect("the menu").number();
        let dir = Directory::parse("/w/b".to_owned()).unwrap();
        let asks = Asked::Prunes {
            menu: number,
            on: on.clone(),
            dir: dir.clone(),
        };
        assert_eq!(sidebar.key(&char('m'), now), asks);
        let goes = prunes::Goes {
            folder: "/w/one".to_owned(),
            branch: "one".to_owned(),
            head: "abc".to_owned(),
            number: 1,
            pushed: prunes::Pushed::Nowhere,
            bytes: None,
        };
        let plan = prunes::Plan {
            repository: "/w".to_owned(),
            goes: vec![goes.clone()],
            kept: Vec::new(),
            unasked: Vec::new(),
        };
        assert_eq!(sidebar.prunes(number + 1, Ok(plan.clone())), Asked::Nothing);
        assert_eq!(sidebar.prunes(number, Ok(plan.clone())), Asked::Nothing);
        let deletes = Asked::Prune {
            menu: number,
            on,
            dir,
            agreed: plan.deletes(),
        };
        assert_eq!(sidebar.key(&char('y'), now), deletes);
        assert!(sidebar.open().expect("the menu").pruning());
        let changed = Ok(prunes::Pruned::Changed(plan.clone()));
        assert_eq!(sidebar.pruned(number, changed.clone()), Asked::Nothing);
        assert!(!sidebar.open().expect("the menu").pruning());
        assert_eq!(sidebar.key(&char('y'), now), deletes);
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);
        assert_eq!(sidebar.pruned(number, changed), Asked::Nothing);
        let note = "menu: nothing deleted: what would go changed";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
        sidebar.key(&char('d'), now);
        let number = sidebar.open().expect("the menu").number();
        sidebar.key(&char('m'), now);
        sidebar.prunes(number, Ok(plan.clone()));
        sidebar.key(&char('y'), now);
        let came = prunes::Came::Stays("it has a view".to_owned());
        let stays = prunes::Outcome {
            folder: goes.folder,
            came,
        };
        assert_eq!(
            sidebar.pruned(number, Ok(prunes::Pruned::Went(vec![stays]))),
            Asked::Nothing
        );
        assert!(sidebar.open().is_none());
        let note = "menu: deleted 0 of 1 worktrees; one stays: it has a view";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
        sidebar.key(&char('d'), now);
        let number = sidebar.open().expect("the menu").number();
        sidebar.key(&char('m'), now);
        sidebar.focus(Keys::Terminal, test_wall());
        assert_eq!(sidebar.prunes(number, Ok(plan)), Asked::Nothing);
        assert!(sidebar.working.is_empty());
        let note = "menu: nothing deleted: the menu closed before you said yes; d m asks again";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
    }

    #[test]
    fn a_pasted_pull_request_goes_to_its_worktree_on_the_row_s_machine_first_then_on_any() {
        let link = "https://github.com/lorenzolfm/lune/pull/5";
        let pulled = |dir: &str| {
            let mut pull_requests = Vec::new();
            let pull = Some((open_pull_state(false), link));
            checked_out(&mut Vec::new(), &mut pull_requests, dir, "feat", pull);
            pull_requests
        };
        let mut host = machines();
        let views = vec![view(1, 0, "/w/b", "claude")];
        let mut nixos_host = up(vec![session("main", 20, views)], Vec::new());
        if let Reached::Host(far) = &mut nixos_host {
            far.pull_requests = pulled("/w/b");
        }
        host.remotes[0] = remote("nixos", nixos_host);
        let (here, there) = (project_row("main", 10), project_row("main", 20));
        let here = Place::Worktree(WorktreePlace {
            project: here,
            worktree: tab_of(10, 0),
        });
        let there = Place::Worktree(WorktreePlace {
            project: there,
            worktree: tab_of(20, 0),
        });
        let mut sidebar = fresh();
        sidebar.hear(host.clone());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(view_of("main", 10, 1)));
        let far = Asked::Go {
            on: Machine::Remote(nixos()),
            place: there.clone(),
        };
        assert_eq!(sidebar.paste(link, test_wall()), far);

        host.pull_requests = pulled("/w/a");
        sidebar.hear(host);
        sidebar.light(Spot::Local(view_of("main", 10, 1)));
        assert_eq!(
            sidebar.paste(link, test_wall()),
            Asked::Go {
                on: Machine::Local,
                place: here
            }
        );
        sidebar.light(Spot::Remote(nixos(), view_of("main", 20, 1)));
        assert_eq!(sidebar.paste(link, test_wall()), far);
    }

    #[test]
    fn another_machine_s_rows_go_there_and_open_there() {
        let mut sidebar = fresh();
        let area = Rect::new(0, 0, 32, 26);
        sidebar.place_rows(area);
        let mut host = machines();
        host.remotes[0] = remote(
            "nixos",
            up(
                vec![session("main", 20, vec![view(1, 0, "/w/b", "claude")])],
                vec![agent("main", 1, Status::Idle, Some("fix"))],
            ),
        );
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "Gkk");
        let fix = view_of("main", 20, 1);
        let there = Asked::Go {
            on: Machine::Remote(nixos()),
            place: fix,
        };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), there);
        type_in(&mut sidebar, "mn");
        let on = Machine::Remote(nixos());
        let dir = directory("/w/b");
        let cwd = Cwd::At(Folder {
            dir,
            branch: None,
            repository: None,
            pull_request: None,
        });
        let (place, terminal) = (view_place("main", 20, 0, 1), ViewId::of(1));
        let (ran, arranged) = (Ran::Unhooked, Some(Arranged::Tabs));
        let row = ViewRow {
            place,
            terminal,
            cwd,
            ran,
            arranged,
        };
        let menu = sidebar.open().expect("the menu");
        assert_eq!(menu.target(), &Target::View(on.clone(), row.clone()));
        let (project, beside) = (listed("main", 20).project(), view_key(20, 1));
        let opens = Opens::Shell;
        let asked = Asked::Open {
            asking: Held::Open,
            on: on.clone(),
            project,
            beside,
            opens,
        };
        assert_eq!(sidebar.key(&char('v'), moment(160_000)), asked);
        assert!(sidebar.open().is_none());

        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[11], "  •✓ \u{ec82} fix                  1m");
        assert_eq!(click(&mut sidebar, 10, 11, area), there);

        type_in(&mut sidebar, "u");
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[11], "   ✓ \u{ec82} fix                  1m");

        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let pasted = sidebar.open().map(Menu::target);
        assert_eq!(
            pasted,
            Some(&Target::View(
                on,
                ViewRow {
                    ran: Ran::Unhooked,
                    ..row
                }
            ))
        );
    }

    #[test]
    fn a_link_pasted_on_another_machine_s_row_makes_its_worktree_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = || session("work", 20, vec![view(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work()], Vec::new()));
        sidebar.hear(host.clone());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Remote(nixos(), Place::Session(listed("work", 20))));
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let number = sidebar.open().map_or(0, Menu::number);
        let on = Machine::Remote(nixos());
        let link = Link::parse("https://github.com/lorenzolfm/lune/pull/5").unwrap();
        let worked = links::Worked {
            dir: directory("/w/b"),
            pull: None,
            here: false,
        };
        let worked = vec![Worked {
            place: view_place("work", 20, 0, 3),
            works: worked,
        }];
        let local = links::Worked {
            dir: directory("/w/a"),
            pull: None,
            here: false,
        };
        let local = vec![Worked {
            place: view_place("main", 10, 0, 1),
            works: local,
        }];
        let unreached = |name: &str| {
            let machine = Machine::Remote(name.parse().unwrap());
            (machine, Err(format!("luned does not reach {name} now")))
        };
        let others = vec![
            (Machine::Local, Ok(local)),
            unreached("me@vps"),
            unreached("pi"),
        ];
        let asked = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let (menu, on_it, work) = (number, on.clone(), asked);
        let pull = Asked::Pull {
            menu,
            on: on_it,
            work,
            wanted,
            worked,
            others,
        };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);
        let at = At::Unopened;
        let folder = directory("/w/n/feat");
        let of_link = Readied::Link(link);
        let ready = Ready {
            at,
            folder,
            branch: None,
            moved_off: false,
            work: of_link,
        };
        let asked = sidebar.pulled(
            number,
            Ok(Found {
                on: on.clone(),
                ready,
            }),
        );
        assert!(
            matches!(&asked, Asked::Make { on: there, .. } if *there == on),
            "{asked:?}"
        );
        assert_eq!(sidebar.made_for(number), Some(Held::Open));
    }

    #[test]
    fn a_link_found_on_another_machine_than_the_row_s_opens_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = own_session("work", 1, vec![view(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work], Vec::new()));
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(view_of("main", 10, 1)));
        sidebar.key(&char('h'), moment(160_000));
        let place = WorktreePlace {
            project: project_row("main", 10),
            worktree: tab_of(10, 0),
        };
        assert_eq!(
            sidebar.lit_spot().cloned(),
            Some(Spot::Local(Place::Worktree(place)))
        );

        let nixos = Machine::Remote(nixos());
        let vps = Machine::Remote("me@vps".parse().unwrap());
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        let number = sidebar.open().map_or(0, Menu::number);
        let asked = sidebar.key(&char('r'), moment(160_000));
        let Asked::Pull {
            on, wanted, others, ..
        } = asked
        else {
            panic!("no job for the key: {asked:?}");
        };
        assert_eq!(on, Machine::Local);
        assert_eq!(wanted, Wanted::AtHead);
        let works = links::Worked {
            dir: directory("/w/b"),
            pull: None,
            here: false,
        };
        let work = Worked {
            place: view_place("work", 1, 0, 3),
            works,
        };
        let unreached = |name: &str| Err(format!("luned does not reach {name} now"));
        let pi = Machine::Remote("pi".parse().unwrap());
        assert_eq!(
            others,
            [
                (nixos.clone(), Ok(vec![work])),
                (vps, unreached("me@vps")),
                (pi, unreached("pi")),
            ]
        );

        let link = Link::parse("https://github.com/lorenzolfm/lune/pull/5").unwrap();
        let at = At::Unopened;
        let folder = directory("/w/b/feat");
        let of_link = Readied::Link(link);
        let ready = Ready {
            at,
            folder,
            branch: None,
            moved_off: false,
            work: of_link,
        };
        let asked = sidebar.pulled(
            number,
            Ok(Found {
                on: nixos.clone(),
                ready,
            }),
        );
        let there = |asked: &Asked| matches!(asked, Asked::Make { on, .. } if *on == nixos);
        assert!(there(&asked), "{asked:?}");
    }

    #[test]
    fn a_link_pasted_on_a_machine_luned_does_not_reach_finds_nothing() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Machine(Machine::Remote("me@vps".parse().unwrap())));
        assert_eq!(
            sidebar.paste("https://github.com/lorenzolfm/lune/pull/5", test_wall()),
            Asked::Nothing
        );
        assert!(sidebar.open().is_some());
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        assert!(sidebar.open().is_none());
        let why = "menu: lorenzolfm/lune#5: luned does not reach me@vps now";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    #[test]
    fn another_machine_s_view_and_worktree_close_there_by_their_agents() {
        let mut sidebar = fresh();
        let mut host = machines();
        let views = vec![view(1, 0, "/w/b", "claude"), view(2, 0, "/w/b", "fish")];
        let agents = vec![agent("main", 1, Status::Waiting, Some("fix"))];
        host.remotes[0] = remote("nixos", up(vec![session("main", 20, views)], agents));
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Remote(nixos(), view_of("main", 20, 1)));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 120, 16);
        let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
        assert!(
            lines.contains(&"Close the view? 1 agent, 1 waiting"),
            "{lines:#?}"
        );
        let act = Act::View(view_key(20, 1));
        let there = Asked::Close {
            on: Machine::Remote(nixos()),
            act,
            ticket: 1,
        };
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), there);
        assert!(sidebar.open().is_none());
        let place = WorktreePlace {
            project: project_row("main", 20),
            worktree: tab_of(20, 0),
        };
        sidebar.light(Spot::Remote(nixos(), Place::Worktree(place.clone())));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 120, 16);
        let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
        assert!(
            lines.contains(&"Close the worktree? 1 agent, 1 waiting"),
            "{lines:#?}"
        );
        let act = Act::Worktree {
            worktree: place.worktree,
            views: vec![view_key(20, 1).key(), view_key(20, 2).key()],
            seen: vec![ViewId::of(1).unwrap(), ViewId::of(2).unwrap()],
        };
        let on = Machine::Remote(nixos());
        assert_eq!(
            sidebar.key(&char('y'), moment(160_000)),
            Asked::Close { on, act, ticket: 2 }
        );
        assert!(sidebar.open().is_none());
    }

    #[test]
    fn a_close_on_a_machine_luned_no_longer_reaches_closes_nothing() {
        for answers in [true, false] {
            let mut sidebar = fresh();
            let mut host = machines();
            sidebar.hear(host.clone());
            sidebar.focus(Keys::Sidebar, test_wall());
            let (project, worktree) = (project_row("main", 20), tab_of(20, 0));
            let place = WorktreePlace { project, worktree };
            sidebar.light(Spot::Remote(nixos(), Place::Worktree(place)));
            type_in(&mut sidebar, "d");
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
            host.remotes[0] = remote("nixos", down(100));
            sidebar.hear(host);
            if !answers {
                let _ = sidebar.lost("luned stopped".to_owned());
            }
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), Asked::Nothing);
            let refused = Some("menu: luned does not reach nixos now");
            assert_eq!(sidebar.notice.as_deref(), refused, "answers: {answers}");
        }
    }

    #[test]
    fn the_picker_lists_the_machines_luned_reaches() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let listing = sidebar.listing();
        let names = |runs: &Runs| {
            runs.agents
                .iter()
                .map(|seated| seated.label.clone())
                .collect()
        };
        assert_eq!(
            (listing.this.as_str(), names(&listing.local)),
            ("mac", Vec::<String>::new())
        );
        let remotes: Vec<_> = listing
            .remotes
            .iter()
            .map(|(at, runs)| (at, names(runs)))
            .collect();
        assert_eq!(remotes, [(&nixos(), vec!["fix".to_owned()])]);
        let each = listing.shelves.iter().map(|shelved| {
            let projects = shelved
                .projects
                .iter()
                .map(|housed| housed.project.listed().name());
            (
                shelved.on.clone(),
                shelved.away,
                projects.map(ToString::to_string).collect(),
            )
        });
        let main = || vec!["main".to_owned()];
        let far = |at: &str| Machine::Remote(at.parse().unwrap());
        let shelves: Vec<(Machine, Option<&str>, Vec<String>)> = each.collect();
        assert_eq!(
            shelves,
            [
                (Machine::Local, None, main()),
                (Machine::Remote(nixos()), None, main()),
                (far("me@vps"), Some("unreachable"), Vec::new()),
                (far("pi"), Some("reaching…"), Vec::new()),
            ]
        );
        sidebar.light(Spot::Machine(Machine::Remote(nixos())));
        assert_eq!(sidebar.on_header(), Some(Machine::Remote(nixos())));
        sidebar.light(Spot::Remote(nixos(), Place::Session(listed("main", 20))));
        assert_eq!(sidebar.on_header(), None);
    }

    #[test]
    fn the_picker_is_told_the_worktree_lune_shows_on_the_machine_it_sits_on() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let there = listed("main", 20);
        let (on, project, worktree) = (Machine::Remote(nixos()), there.project(), tab_of(20, 0));
        let at = Sitting::Worktree {
            worktree,
            layout: SeatedLayout::Unlaid,
        };
        sidebar.sit(Seat { on, project, at }, test_wall());
        let project = ProjectRow::of(there);
        let place = WorktreePlace {
            project,
            worktree: tab_of(20, 0),
        };
        assert_eq!(
            sidebar.listing().shown,
            Some((Machine::Remote(nixos()), place))
        );
    }

    #[test]
    fn n_w_on_a_worktree_makes_a_branch_s_in_its_project_on_its_machine() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let tab = |id| WorktreePlace {
            project: project_row("main", id),
            worktree: tab_of(id, 0),
        };
        sidebar.light(Spot::Local(Place::Worktree(tab(10))));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(10).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree {
            on: Machine::Local,
            project,
            branch,
        };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        sidebar.light(Spot::Remote(nixos(), Place::Worktree(tab(20))));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(20).unwrap(), Branch::parse("feat").unwrap());
        let far = Asked::NewWorktree {
            on: Machine::Remote(nixos()),
            project,
            branch,
        };
        assert_eq!(branched(&mut sidebar, "feat"), far);
    }

    #[test]
    fn n_w_in_a_folder_says_its_one_worktree_is_its_own_and_asks_no_branch() {
        let mut sidebar = fresh();
        let own = worktree_of(31, "notes", "/w/notes", &[]);
        let mut notes = a_project(8, "notes", Vec::new());
        notes.dir = "/w/notes".to_owned();
        notes.holds = Some(api::project::Holds::Folder(own));
        sidebar.heard(
            Host {
                projects: Some(kept(vec![notes])),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        sidebar.focus(Keys::Sidebar, test_wall());
        let worktree = WorktreeId::of(31).unwrap();
        let place = WorktreePlace {
            project: project_row("notes", 8),
            worktree,
        };
        sidebar.light(Spot::Local(Place::Worktree(place)));
        type_in(&mut sidebar, "n");
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        assert!(sidebar.open().is_none(), "no box asks for a branch");
        let said = sidebar.notice.as_deref();
        assert_eq!(
            said,
            Some("menu: notes is a folder, and its own is its one worktree")
        );
    }

    #[test]
    fn n_w_in_a_floating_menu_over_a_folder_floats_its_note() {
        let mut sidebar = fresh();
        let own = worktree_of(31, "notes", "/w/notes", &[1]);
        let mut notes = a_project(8, "notes", Vec::new());
        notes.dir = "/w/notes".to_owned();
        notes.holds = Some(api::project::Holds::Folder(own));
        sidebar.heard(
            Host {
                projects: Some(kept(vec![notes])),
                ..host(Vec::new(), Vec::new())
            },
            SystemTime::UNIX_EPOCH,
        );
        let own = WorktreePlace {
            project: project_row("notes", 8),
            worktree: WorktreeId::of(31).unwrap(),
        };
        sit(
            &mut sidebar,
            &Place::View(ViewPlace::new(own, ViewKey::terminal(1))),
        );
        float_a_menu(&mut sidebar);
        type_in(&mut sidebar, "n");
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        let said = "menu: notes is a folder, and its own is its one worktree";
        assert!(matches!(sidebar.floating(), Some(Floating::Note(note)) if note == said));
        assert_eq!(sidebar.notice, None);
    }

    #[test]
    fn what_is_pasted_in_the_box_for_a_machine_goes_in_it() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        type_in(&mut sidebar, "nm");
        assert_eq!(sidebar.paste("me@vps\n", test_wall()), Asked::Nothing);
        assert_eq!(
            sidebar.key(&enter, moment(160_000)),
            Asked::Ssh("me@vps".parse().unwrap())
        );
        let link = "https://github.com/lorenzolfm/lune/pull/5";
        type_in(&mut sidebar, "nm");
        assert_eq!(sidebar.paste(link, test_wall()), Asked::Nothing);
        assert_eq!(
            sidebar.key(&enter, moment(160_000)),
            Asked::Ssh(link.parse().unwrap())
        );
    }

    #[test]
    fn an_unreached_machine_says_why_in_the_notes_while_highlighted() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "Gk");
        let why = " me@vps: it ended before it reached luned (exit status: 255); ssh said \"ssh: \
                   Could not resolve hostname vps\"";
        let says = |sidebar: &mut Sidebar| screen(sidebar, true, 128, 24).contains(&why.to_owned());
        assert!(says(&mut sidebar));
        type_in(&mut sidebar, "j");
        assert!(!says(&mut sidebar));
        type_in(&mut sidebar, "k");
        sidebar.focus(Keys::Terminal, test_wall());
        assert!(!screen(&mut sidebar, false, 128, 24).contains(&why.to_owned()));
    }

    #[test]
    fn a_machine_named_as_another_that_is_this_one_says_so_on_its_header() {
        let mut sidebar = fresh();
        let mut host = machines();
        host.remotes[1] = remote("me@mac", Reached::Here(()));
        sidebar.hear(host);
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(lines[12], " ▸ \u{f108} ME@MAC · this machine", "{lines:?}");
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Machine(Machine::Remote("me@mac".parse().unwrap())));
        let said = format!("me@mac: {}", luned::THIS_MACHINE);
        assert_eq!(sidebar.unreached(), Some(said));
    }

    #[test]
    fn a_machine_named_twice_or_by_no_destination_is_left_out_and_said() {
        let mut sidebar = fresh();
        let mut host = machines();
        host.remotes.push(remote("nixos", down(100)));
        host.remotes.push(remote("-oProxyCommand=sh", down(100)));
        sidebar.hear(host);
        let lines = screen(&mut sidebar, false, 32, 18);
        let headers = lines
            .iter()
            .filter(|line| line.starts_with(" ▾ \u{f108} NIXOS"))
            .count();
        assert_eq!(headers, 1);
        assert!(!lines.iter().any(|line| line.contains("ProxyCommand")));
        assert_eq!(
            lines[15..],
            [
                " luned: 2 machines with a dest",
                " ination lune does not read, o",
                " r named twice, not listed",
            ]
        );
    }

    #[test]
    fn the_menu_adds_a_machine_from_any_row_and_forgets_one_from_its_row() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "nm-x");
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 26);
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(" not a destination:")),
            "{lines:?}"
        );
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        sidebar.key(&backspace, moment(160_000));
        sidebar.key(&backspace, moment(160_000));
        type_in(&mut sidebar, "me@box");
        let added = Asked::Ssh("me@box".parse().unwrap());
        assert_eq!(sidebar.key(&enter, moment(160_000)), added);
        assert!(sidebar.open().is_none());

        type_in(&mut sidebar, "Gk");
        let vps = "me@vps".parse().unwrap();
        assert_eq!(
            sidebar.lit_spot().cloned(),
            Some(Spot::Machine(Machine::Remote(vps)))
        );
        type_in(&mut sidebar, "d");
        let forget = Asked::Forget("me@vps".parse().unwrap());
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), forget);
    }

    fn two_each() -> Host {
        let work = session("work", 30, vec![view(1, 0, "/w/c", "fish")]);
        machines_with(
            vec![work],
            vec![session("dev", 40, vec![view(1, 0, "/w/d", "fish")])],
        )
    }

    #[test]
    fn j_and_k_move_a_project_among_its_machine_s_by_luned() {
        let mut sidebar = fresh();
        sidebar.hear(two_each());
        sidebar.focus(Keys::Sidebar, test_wall());
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        sidebar.light(Spot::Local(Place::Session(listed("main", 10))));
        type_in(&mut sidebar, "v");
        let asked = |on: Machine, seen: &[u64], ids: &[u64], ticket| Asked::Move {
            on,
            order: projects(seen, ids),
            ticket,
        };
        assert_eq!(
            shift(&mut sidebar, 'K'),
            Asked::Nothing,
            "the first stays the first"
        );
        assert_eq!(
            shift(&mut sidebar, 'J'),
            asked(Machine::Local, &[10, 30], &[30, 10], 1)
        );
        assert_eq!(
            shift(&mut sidebar, 'J'),
            Asked::Nothing,
            "the last stays the last"
        );
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        sidebar.key(&esc, moment(160_000));

        let far = |name: &str, id| Spot::Remote(nixos(), Place::Session(listed(name, id)));
        sidebar.light(far("dev", 40));
        type_in(&mut sidebar, "v");
        let nixos_moved = asked(Machine::Remote(nixos()), &[20, 40], &[40, 20], 2);
        assert_eq!(shift(&mut sidebar, 'K'), nixos_moved);
        assert_eq!(
            shift(&mut sidebar, 'K'),
            Asked::Nothing,
            "the first stays the first"
        );
    }

    #[test]
    fn j_and_k_move_a_worktree_among_its_project_s_by_luned() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, test_wall());
        let first = WorktreePlace {
            project: project_row("older", 10),
            worktree: tab_of(10, 0),
        };
        sidebar.light(Spot::Local(view_of("older", 10, 1)));
        type_in(&mut sidebar, "v");
        assert_eq!(
            sidebar.lit_spot().cloned(),
            Some(Spot::Local(Place::Worktree(first)))
        );
        let asked = sidebar.key(&char('J'), moment(160_000));
        let (first, second) = (tab_of(10, 0), tab_of(10, 1));
        let (seen, wanted) = (vec![first, second], vec![second, first]);
        let order = Reorder::Worktrees {
            project: ProjectId::of(10).unwrap(),
            seen,
            wanted,
        };
        assert_eq!(
            asked,
            Asked::Move {
                on: Machine::Local,
                order,
                ticket: 1
            }
        );
    }

    fn version(seq: u64) -> api::Version {
        api::Version { pid: 7, seq }
    }

    fn kept_of(host: &mut Host) -> &mut api::projects::Kept {
        let held = host
            .projects
            .as_mut()
            .and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = held else {
            panic!("the projects are kept");
        };
        kept
    }

    fn in_order(host: &Host, ids: &[u64], seq: u64) -> Host {
        let mut host = host.clone();
        let kept = kept_of(&mut host);
        kept.projects.retain(|it| ids.contains(&it.id));
        kept.projects
            .sort_by_key(|it| ids.iter().position(|id| *id == it.id));
        kept.version = Some(version(seq));
        host
    }

    fn projects_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let rows = rows_shown(sidebar).into_iter();
        let names = rows.filter_map(|row| Some(row.strip_prefix(" ● ")?.to_owned()));
        names.collect()
    }

    fn bipa_selected() -> (Sidebar, Host) {
        let mut sidebar = searched();
        let heard = sidebar.host.clone().expect("heard");
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar.light(Spot::Local(Place::Session(listed("bipa", 10))));
        type_in(&mut sidebar, "v");
        (sidebar, heard)
    }

    fn moved(seen: &[u64], wanted: &[u64], ticket: u64) -> Asked {
        Asked::Move {
            on: Machine::Local,
            order: projects(seen, wanted),
            ticket,
        }
    }

    #[test]
    fn quick_presses_build_on_the_order_asked_until_watch_shows_the_version_answered() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        assert_eq!(
            shift(&mut sidebar, 'J'),
            moved(&[10, 20, 30], &[20, 10, 30], 1)
        );
        assert_eq!(
            shift(&mut sidebar, 'J'),
            moved(&[20, 10, 30], &[20, 30, 10], 1)
        );
        assert_eq!(
            shift(&mut sidebar, 'J'),
            Asked::Nothing,
            "the last stays the last"
        );
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"]);
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "zed", "bipa"],
            "on its way still"
        );

        sidebar.moved(1, Some(version(2)), test_wall());
        sidebar.moved(1, Some(version(3)), test_wall());
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "zed", "bipa"],
            "answered, not shown"
        );
        sidebar.hear(in_order(&heard, &[20, 30, 10], 3));
        sidebar.hear(in_order(&heard, &[10, 20, 30], 4));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes", "zed"],
            "moved back elsewhere"
        );

        assert_eq!(
            shift(&mut sidebar, 'J'),
            moved(&[10, 20, 30], &[20, 10, 30], 2)
        );
        sidebar.moved(2, Some(version(5)), test_wall());
        sidebar.hear(in_order(&heard, &[10, 20, 30], 6));
        let back = "moved back elsewhere before Watch showed it";
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes", "zed"],
            "{back}"
        );
        assert_eq!(
            shift(&mut sidebar, 'J'),
            moved(&[10, 20, 30], &[20, 10, 30], 3)
        );
        sidebar.hear(in_order(&heard, &[10, 20], 7));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes"],
            "forgotten elsewhere"
        );
    }

    #[test]
    fn an_order_asked_again_shows_until_watch_shows_the_last_version_answered() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        shift(&mut sidebar, 'J');
        shift(&mut sidebar, 'J');
        assert_eq!(
            shift(&mut sidebar, 'K'),
            moved(&[20, 30, 10], &[20, 10, 30], 1)
        );
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        sidebar.hear(in_order(&heard, &[20, 30, 10], 3));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "on its way still"
        );
        assert_eq!(
            shift(&mut sidebar, 'K'),
            moved(&[20, 10, 30], &[10, 20, 30], 1)
        );

        for seq in 2..6 {
            sidebar.moved(1, Some(version(seq)), test_wall());
        }
        sidebar.hear(in_order(&heard, &[20, 10, 30], 4));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes", "zed"],
            "an older version"
        );
        sidebar.hear(in_order(&heard, &[10, 20, 30], 5));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 6));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "landed, luned says"
        );
    }

    #[test]
    fn a_version_from_another_luned_or_none_shows_luned_s_order() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        shift(&mut sidebar);
        sidebar.moved(1, Some(version(9)), test_wall());
        let mut restarted = in_order(&heard, &[10, 20, 30], 1);
        kept_of(&mut restarted).version = Some(api::Version { pid: 8, seq: 1 });
        sidebar.hear(restarted);
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes", "zed"],
            "luned started again"
        );

        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 2));
        sidebar.moved(2, None, test_wall());
        assert_eq!(
            projects_shown(&mut sidebar),
            ["bipa", "notes", "zed"],
            "it says no version"
        );
    }

    #[test]
    fn a_move_luned_turns_down_puts_the_rows_back_as_it_keeps_them() {
        let (mut sidebar, _) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 1));
        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 1));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"]);
        let stale = format!("not moved: {}", "they changed since");
        let refused = |sidebar: &mut Sidebar, ticket| {
            let order = projects(&[10, 20, 30], &[20, 10, 30]);
            sidebar.unmoved(&Machine::Local, ticket, &order, stale.clone(), test_wall());
        };
        refused(&mut sidebar, 1);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);
        assert_eq!(sidebar.notice, Some(stale.clone()));

        let _ = sidebar.clear_notice();
        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 2));
        refused(&mut sidebar, 1);
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "an older refusal"
        );
        assert_eq!(
            sidebar.notice, None,
            "a refusal a newer move replaced says nothing"
        );
        let elsewhere = Machine::Remote(nixos());
        let order = projects(&[20, 40], &[40, 20]);
        sidebar.unmoved(&elsewhere, 1, &order, stale.clone(), test_wall());
        assert_eq!(
            sidebar.notice,
            Some(stale.clone()),
            "another machine's refusal says so"
        );

        let _ = sidebar.clear_notice();
        refused(&mut sidebar, 2);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);
        assert_eq!(sidebar.notice, Some(stale));
    }

    #[test]
    fn a_move_never_sent_takes_back_that_press_alone() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        shift(&mut sidebar);
        sidebar.moved(1, Some(version(2)), test_wall());
        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 1));
        sidebar.unsent(
            1,
            &projects(&[20, 10, 30], &[20, 30, 10]),
            "full",
            test_wall(),
        );
        assert_eq!(sidebar.notice, Some("not moved: full".to_owned()));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "the first still"
        );
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "not shown yet"
        );
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));

        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 2));
        sidebar.unsent(
            2,
            &projects(&[20, 10, 30], &[20, 30, 10]),
            "full",
            test_wall(),
        );
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"]);
        assert_eq!(
            shift(&mut sidebar),
            moved(&[20, 10, 30], &[20, 30, 10], 3),
            "nothing waits"
        );
    }

    #[test]
    fn moves_luned_has_not_answered_stop_at_the_most_the_way_holds() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        for at in 0..MOVES_MAX {
            let press = if at % 2 == 0 { 'J' } else { 'K' };
            let asked = shift(&mut sidebar, press);
            assert!(
                matches!(asked, Asked::Move { ticket: 1, .. }),
                "{at}: {asked:?}"
            );
        }
        assert_eq!(shift(&mut sidebar, 'J'), Asked::Nothing);
        let held = format!("not moved: luned has not answered the last {MOVES_MAX} moves");
        assert_eq!(sidebar.notice, Some(held));
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);

        sidebar.moved(1, Some(version(1)), test_wall());
        assert!(matches!(
            shift(&mut sidebar, 'J'),
            Asked::Move { ticket: 1, .. }
        ));
        let last = u64::try_from(MOVES_MAX).unwrap() + 1;
        for seq in 2..=last {
            sidebar.moved(1, Some(version(seq)), test_wall());
        }
        sidebar.hear(in_order(&heard, &[20, 10, 30], last - 1));
        assert_eq!(
            projects_shown(&mut sidebar),
            ["notes", "bipa", "zed"],
            "not the last yet"
        );
        sidebar.hear(in_order(&heard, &[20, 10, 30], last));
        assert_eq!(
            shift(&mut sidebar, 'J'),
            moved(&[20, 10, 30], &[20, 30, 10], 2)
        );
    }

    #[test]
    fn a_move_asks_for_the_whole_order_with_what_is_not_listed_in_place() {
        assert_eq!(wanted(&[1, 2, 3], &[3, 1, 2]), [3, 1, 2]);
        let kept = wanted(&[1, 2, 3, 4], &[4, 2]);
        assert_eq!(kept, [1, 4, 3, 2], "what is not listed keeps its place");
    }

    #[test]
    fn another_machine_s_project_dragged_lands_among_its_own() {
        let mut sidebar = fresh();
        sidebar.hear(two_each());
        let area = Rect::new(0, 0, 32, 27);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        let row = |name: &str| {
            let at = lines
                .iter()
                .position(|line| line.starts_with(name))
                .unwrap();
            u16::try_from(at).unwrap()
        };
        let (dev, first) = (row(" ● dev"), row(" ▾ \u{f108} NIXOS") + 2);
        let left = MouseButton::Left;
        sidebar.mouse_rows(
            mouse(MouseEventKind::Down(left), 10, dev),
            area,
            test_wall(),
        );
        assert_eq!(
            sidebar.mouse_rows(
                mouse(MouseEventKind::Drag(left), 10, first),
                area,
                test_wall()
            ),
            Asked::Take
        );
        let order = projects(&[20, 40], &[40, 20]);
        let asked = Asked::Move {
            on: Machine::Remote(nixos()),
            order,
            ticket: 1,
        };
        assert_eq!(
            sidebar.mouse_rows(
                mouse(MouseEventKind::Up(left), 10, first),
                area,
                test_wall()
            ),
            asked
        );
    }

    #[test]
    fn a_search_finds_another_machine_s_rows_folded() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let main = ProjectId::of(20).unwrap();
        sidebar.edit(|state| {
            let _ = state
                .folds
                .set(Fold::Machine(&Machine::Remote(nixos())), true);
            let _ = state
                .folds
                .set(Fold::Project(&Machine::Remote(nixos()), main), true);
            state.rebuild(test_wall());
        });
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "/fix");
        let lines = screen(&mut sidebar, true, 32, 26);
        assert_eq!(
            lines[HEADER..HEADER + 5],
            [
                " ▾ \u{f108} NIXOS                  ●1",
                "",
                " ● main                     ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fix                  1m",
            ]
        );
        assert_eq!(lines[HEADER + 5], "");
    }

    #[test]
    fn a_search_lights_its_best_match_under_rows_found_on_machines_and_projects_above() {
        let notes = session("quartz notes", 50, vec![view(1, 0, "/w/n", "vim")]);
        let old = session("quartz old", 30, vec![view(1, 0, "/w/o", "top")]);
        let views = vec![view(1, 0, "/w/x", "htop"), view(2, 0, "/w/x", "quartz")];
        let lab = session(
            "quartz lab",
            40,
            [views, vec![view(3, 0, "/w/x", "quill")]].concat(),
        );
        let mut sidebar = fresh();
        sidebar.hear(machines_with(vec![notes], vec![old, lab]));
        type_in(&mut sidebar, "/quartz");
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows.len(), 14, "{rows:?}");
        assert!(
            rows[12].ends_with(" quartz") && rows[13].ends_with(" quill"),
            "{rows:?}"
        );
        assert_eq!(lit, [10, 12], "the view and its worktree: {rows:?}");
        let quartz = Spot::on(&Machine::Remote(nixos()), view_of("quartz lab", 40, 2));
        assert_eq!(sidebar.lit_spot(), Some(&quartz));
    }

    #[test]
    fn this_machine_cuts_its_long_name_to_say_no_projects_and_another_keeps_its_name() {
        let mut sidebar = fresh();
        sidebar.edit(|state| state.this = "Mac-1763578123456".to_owned());
        let remotes = ["lorenzo@nixos", "me@vps-frankfurt"].map(|it| remote(it, down(100)));
        sidebar.hear(Host {
            remotes: remotes.to_vec(),
            ..host(Vec::new(), Vec::new())
        });
        let lines = screen(&mut sidebar, false, 32, 8);
        assert_eq!(
            lines[HEADER..HEADER + 3],
            [
                " ▸ \u{f108} Mac-176357… · no projects",
                " ▸ \u{f108} LORENZO@NIXOS · unrea… 1m",
                " ▸ \u{f108} ME@VPS-FRANKFURT · un… 1m",
            ]
        );
    }

    #[test]
    fn what_you_saw_on_another_machine_outlasts_its_being_unreached() {
        let mut sidebar = fresh();
        let reached = |sessions: bool| {
            let idle = agent("main", 1, Status::Idle, Some("fix"));
            let main = session("main", 20, vec![view(1, 0, "/w/b", "claude")]);
            let nixos = if sessions {
                up(vec![main], vec![idle])
            } else {
                up(Vec::new(), Vec::new())
            };
            Host {
                remotes: vec![remote("nixos", nixos)],
                ..host(Vec::new(), Vec::new())
            }
        };
        let row = |sidebar: &mut Sidebar| screen(sidebar, false, 32, 9)[7].clone();
        let seen = "   ✓ \u{ec82} fix                  1m";
        let new = "  •✓ \u{ec82} fix                  1m";
        sidebar.hear(reached(true));
        assert_eq!(row(&mut sidebar), new);
        let empty = screen(&mut sidebar, false, 32, 8)[2].clone();
        assert_eq!(
            empty, " ▸ \u{f108} MAC · no projects",
            "a machine with none is one row"
        );
        sidebar.focus(Keys::Sidebar, test_wall());
        type_in(&mut sidebar, "Gu");
        sidebar.focus(Keys::Terminal, test_wall());
        tick(&mut sidebar, 140_000);
        tick(&mut sidebar, 155_000);
        assert_eq!(row(&mut sidebar), seen);

        let unreached = Host {
            remotes: vec![remote("nixos", down(150))],
            ..host(Vec::new(), Vec::new())
        };
        sidebar.hear(unreached);
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 175_000);
        sidebar.hear(reached(true));
        assert_eq!(row(&mut sidebar), seen);

        sidebar.hear(reached(false));
        tick(&mut sidebar, 176_000);
        tick(&mut sidebar, 187_000);
        sidebar.hear(reached(true));
        assert_eq!(row(&mut sidebar), new);
    }

    #[test]
    fn what_you_saw_here_goes_while_another_machine_is_unreached() {
        let mut sidebar = fresh();
        let unreached = |host: Host| Host {
            remotes: vec![remote("nixos", down(150))],
            ..host
        };
        sidebar.hear(unreached(own_idle_agent()));
        let s = |view| own_view_of("s", 1, view);
        sit(&mut sidebar, &s(1));
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 161_000);
        sit(&mut sidebar, &s(3));
        sidebar.hear(unreached(host(Vec::new(), Vec::new())));
        tick(&mut sidebar, 173_000);
        tick(&mut sidebar, 183_000);
        sidebar.hear(unreached(own_idle_agent()));
        let new = "  •✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(183_500), true), new);
    }

    #[test]
    fn what_is_read_aloud_has_a_line() {
        let views = vec![view(1, 0, "/one", "one"), view(2, 0, "/two", "two")];
        let mut sidebar = fresh();
        sidebar.hear(host(vec![session("work", 100, views)], Vec::new()));
        let marked = |sidebar: &mut Sidebar| {
            let shown = screen(sidebar, false, 40, 12);
            shown
                .into_iter()
                .filter(|row| row.contains(aloud::MARK))
                .collect::<Vec<_>>()
        };
        assert_eq!(marked(&mut sidebar), [""; 0]);

        let whose = aloud::Whose::Said {
            on: Machine::Remote(nixos()),
        };
        let text = "the tests pass".to_owned();
        let _ = sidebar.aloud(Some(aloud::Shown::Reading {
            whose,
            text,
            paused: false,
        }));
        let rows = marked(&mut sidebar);
        assert_eq!(
            rows.len(),
            1,
            "what is read aloud marks no project's row: {rows:#?}"
        );
        assert!(rows[0].contains("nixos · the tests pass"), "{rows:#?}");

        let _ = sidebar.aloud(None);
        assert_eq!(marked(&mut sidebar), [""; 0]);
    }

    #[test]
    fn the_buttons_of_what_is_read_aloud_are_where_they_show() {
        let reading = |paused| {
            let (whose, text) = (aloud::Whose::Selection, "the tests pass".repeat(9));
            Some(aloud::Shown::Reading {
                whose,
                text,
                paused,
            })
        };
        for width in [NARROWEST, 40] {
            let area = Rect::new(2, 1, width, 12);
            let mut sidebar = fresh();
            let at = |buffer: &Buffer, icon: &str| {
                let found = buffer
                    .content()
                    .iter()
                    .position(|cell| cell.symbol() == icon)?;
                let (x, y) = buffer.pos_of(found);
                Some(Position::new(x, y))
            };
            for (paused, toggle, other, button) in [
                (false, aloud::PAUSE, aloud::PLAY, aloud::Button::Pause),
                (true, aloud::PLAY, aloud::PAUSE, aloud::Button::GoOn),
            ] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
                let _ = sidebar.aloud(reading(paused));
                sidebar.place(area);
                let _ = sidebar.draw(&mut buffer, area, moment(0).system);
                assert_eq!(at(&buffer, other), None);
                let toggle = at(&buffer, toggle).expect("the button shows");
                let stop = at(&buffer, aloud::STOP).expect("the button shows");
                assert_eq!(toggle.y, stop.y);
                let hit = |x, y| sidebar.aloud_buttons().at(Position::new(x, y)).copied();
                assert_eq!(hit(toggle.x, toggle.y), Some(button));
                assert_eq!(hit(stop.x, stop.y), Some(aloud::Button::Stop));
                assert_eq!(hit(toggle.x, toggle.y - 1), None);
                assert_eq!(hit(toggle.x - 2, toggle.y), None, "on what it says");
                assert_eq!(
                    usize::from(stop.x),
                    usize::from(area.right()) - 3,
                    "at the right"
                );
            }
            let mut buffer = Buffer::empty(area);
            sidebar.collapse(area);
            let _ = sidebar.draw(&mut buffer, area, test_wall());
            assert_eq!(sidebar.aloud_buttons(), &Spots::default());
        }
    }

    #[test]
    fn a_pause_shows_on_the_sidebar_collapsed() {
        let marked = |shown| {
            let area = Rect::new(0, 0, COLLAPSED_ROWS, 6);
            let mut sidebar = fresh();
            let _ = sidebar.aloud(Some(shown));
            let mut buffer = Buffer::empty(area);
            sidebar.collapse_rows(area);
            sidebar.draw_collapsed_rows(&mut buffer, area);
            buffer
                .content()
                .iter()
                .any(|cell| cell.symbol() == aloud::PLAY)
        };
        for paused in [false, true] {
            let (whose, text) = (aloud::Whose::Selection, String::new());
            assert_eq!(
                marked(aloud::Shown::Reading {
                    whose,
                    text,
                    paused
                }),
                paused
            );
            assert_eq!(marked(aloud::Shown::Fetching { got: 0, paused }), paused);
        }
    }

    fn two_to_move() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(two_to_move_host());
        sidebar.focus(Keys::Sidebar, test_wall());
        sidebar
    }

    fn two_to_move_host() -> Host {
        let auth = |view: View| View {
            tab_name: "auth".to_owned(),
            ..view
        };
        let work = vec![
            view(1, 0, "/w/a", "fish"),
            auth(view(2, 1, "/w/b", "fish")),
            auth(view(3, 1, "/w/lune/main", "claude")),
        ];
        let play = vec![view(5, 0, "/w/p", "fish")];
        let fold = Agent {
            session_id: Some("0b7e1c2a-5d1f-4c8e-9a43-1f2e3d4c5b6a".to_owned()),
            status_since: None,
            ..agent("work", 3, Status::Idle, Some("fold"))
        };
        host(
            vec![session("work", 10, work), session("play", 20, play)],
            vec![fold],
        )
    }

    #[test]
    fn an_errored_agent_clears_its_unread_dot_when_seen_or_read() {
        let mut error_host = own_idle_agent();
        error_host.agents[0].reported_status =
            Some(api::agent::ReportedStatus::Error(api::ErrorStatus {
                message: Some("boom".to_owned()),
            }));
        let mut sidebar = fresh();
        sidebar.hear(error_host);
        let s = |view| own_view_of("s", 1, view);
        sit(&mut sidebar, &s(3));
        let unseen = "  •✗ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(160_000), true), unseen);

        sit(&mut sidebar, &s(1));
        let here = "▌  ✗ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(161_000), true), here);
        agent_row(&mut sidebar, moment(162_000), true);

        sit(&mut sidebar, &s(3));
        let seen = "   ✗ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(163_000), true), seen);
    }

    #[test]
    fn next_age_and_next_spin_land_on_the_next_visible_tick() {
        let base = moment(10_000).system;
        let check = |elapsed_ms: u64, expected_before: &str, expected_at: &str| {
            let now = base + Duration::from_millis(elapsed_ms);
            let next = next_age(base, now).expect("a next age");
            assert!(next > now, "{next:?} is after {now:?}");
            assert_eq!(age(base, next - Duration::from_millis(1)), expected_before);
            assert_eq!(age(base, next), expected_at);
        };
        check(0, "0s", "1s");
        check(500, "0s", "1s");
        check(59_400, "59s", "1m");
        check(60_000, "1m", "2m");
        check(3_599_500, "59m", "1h");
        check(86_399_000, "23h", "1d");
        check(604_799_000, "6d", "1w");
        check(604_800_000, "1w", "2w");
        let future = base + Duration::from_secs(5);
        assert_eq!(
            next_age(future, base),
            Some(future + Duration::from_secs(1))
        );

        for ms in [0, 1, 40, 99, 100, 150, 999] {
            let now = moment(ms).system;
            let left = next_spin(now);
            assert!(left > Duration::ZERO && left <= SPIN, "{ms}ms -> {left:?}");
            assert_eq!(frame(now + left - Duration::from_millis(1)), frame(now));
            assert_ne!(frame(now + left), frame(now));
        }
    }

    #[test]
    fn only_on_screen_spinners_spin_and_the_tallies_stay_still() {
        let mut busy = agent("s", 1, Status::Busy, Some("work"));
        busy.status_since = None;
        let views = vec![view(1, 0, "/w/a", "claude")];
        let mut sidebar = fresh();
        sidebar.hear(host(vec![session("s", 10, views)], vec![busy]));

        let title_at = |sidebar: &mut Sidebar, at| seen_at(sidebar, false, 32, 8, at).0[0].clone();
        assert_eq!(
            title_at(&mut sidebar, moment(160_000)),
            title_at(&mut sidebar, moment(160_300))
        );
        assert!(title_at(&mut sidebar, moment(160_300)).contains("⠋1"));

        let strip_at = |sidebar: &mut Sidebar, at| {
            let buffer = collapsed(sidebar, 6, at);
            (0..6)
                .map(|y| {
                    (0..COLLAPSED_ROWS - 1)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            strip_at(&mut sidebar, moment(160_000)),
            strip_at(&mut sidebar, moment(160_300))
        );
        assert!(
            !sidebar.turning(),
            "collapsed sidebar holds no on-screen spinner"
        );

        let drawn = moment(160_000);
        let paint = |sidebar: &mut Sidebar, height: u16| {
            let area = Rect::new(0, 0, 32, height);
            let mut buffer = Buffer::empty(area);
            sidebar.place(area);
            sidebar.draw(&mut buffer, area, drawn.system).1
        };
        assert_eq!(paint(&mut sidebar, 9).wait(drawn, drawn), Some(SPIN));
        assert!(sidebar.turning());
        assert_eq!(
            paint(&mut sidebar, 6).wait(drawn, drawn),
            None,
            "busy row below the viewport does not spin"
        );
        assert!(!sidebar.turning());

        fold(&mut sidebar, "s");
        sidebar.rebuild(test_wall());
        assert_eq!(
            paint(&mut sidebar, 7).wait(drawn, drawn),
            Some(SPIN),
            "folded project shows a busy spinner badge"
        );

        let mut waiting = agent("s", 2, Status::Waiting, Some("ask"));
        waiting.status_since = None;
        let mut busy2 = agent("s", 1, Status::Busy, Some("work"));
        busy2.status_since = None;
        let views = vec![view(1, 0, "/w/a", "claude"), view(2, 0, "/w/a", "claude")];
        sidebar.hear(host(vec![session("s", 10, views)], vec![busy2, waiting]));
        assert_eq!(
            paint(&mut sidebar, 7).wait(drawn, drawn),
            None,
            "a waiting badge hides the folded busy spinner, so nothing spins"
        );
    }

    #[test]
    fn sidebar_ticked_reports_only_visible_time_transitions() {
        let mut sidebar = fresh();
        let make = in_shell(1, going("make", 159, Front::Working));
        sidebar.hear(host(vec![session("s", 10, vec![make])], Vec::new()));
        let t0 = moment(160_500);
        let _ = seen_at(&mut sidebar, false, 32, 7, t0);
        assert_eq!(
            sidebar.due(t0),
            Some(t0.steady + Duration::from_millis(499))
        );
        assert!(!sidebar.ticked(moment(160_998), true));
        assert!(sidebar.ticked(moment(160_999), true));
        assert!(!sidebar.ticked(moment(161_500), true));

        let now = moment(170_000);
        sidebar.copied(
            Copied::text(&Excerpt::of("copied".to_owned()).unwrap()),
            now.steady,
        );
        assert_eq!(sidebar.due(now), Some(now.steady + COPIED_SHOWS));
        assert!(!sidebar.ticked(now + COPIED_SHOWS - Duration::from_millis(1), true));
        assert!(sidebar.ticked(now + COPIED_SHOWS, true));
        assert!(!sidebar.ticked(now + COPIED_SHOWS + Duration::from_secs(1), true));

        let pressed = moment(180_000);
        sidebar.focus(Keys::Sidebar, pressed.system);
        let _ = seen_at(&mut sidebar, true, 32, 12, pressed);
        sidebar.key(&char('g'), pressed);
        assert_eq!(sidebar.due(pressed), Some(pressed.steady + PAUSE));
        assert!(!sidebar.ticked(pressed + PAUSE - Duration::from_millis(1), true));
        assert!(sidebar.ticked(pressed + PAUSE, true));
        assert!(!sidebar.ticked(pressed + PAUSE + Duration::from_secs(1), true));
    }

    #[test]
    fn monotonic_ui_timers_survive_backward_wall_clock_steps() {
        let mut sidebar = two_sessions();
        let t0 = moment(200_000);
        sidebar.focus(Keys::Sidebar, t0.system);
        let area = Rect::new(0, 0, 32, 14);
        let mut buffer = Buffer::empty(area);
        sidebar.place(area);
        let _ = sidebar.draw(&mut buffer, area, t0.system);

        sidebar.key(&char('g'), t0);
        let shown_at = t0 + PAUSE;
        assert!(sidebar.ticked(shown_at, true));
        let _ = sidebar.draw(&mut buffer, area, shown_at.system);
        assert!(matches!(
            sidebar.mode,
            Mode::Pending(Pending::First(ChordWait::Shown))
        ));

        let jumped_back = Moment {
            steady: shown_at.steady + Duration::from_millis(50),
            system: t0.system - Duration::from_secs(10),
        };
        assert_eq!(
            sidebar.due(jumped_back),
            None,
            "shown chord hint does not reschedule"
        );
        assert!(
            !sidebar.ticked(jumped_back, true),
            "shown chord hint does not re-dirty"
        );
        assert!(
            matches!(sidebar.bottom(), Bottom::Keys(GO | GO_GIT, _)),
            "shown chord hint never vanishes on a backward wall-clock jump"
        );

        sidebar.key(&KeyEvent::from(KeyCode::Esc), jumped_back);
        sidebar.copied(
            Copied::text(&Excerpt::of("link".to_owned()).unwrap()),
            jumped_back.steady,
        );
        let expired = Moment {
            steady: jumped_back.steady + COPIED_SHOWS,
            system: jumped_back.system - Duration::from_secs(30),
        };
        assert!(
            sidebar.ticked(expired, true),
            "copied toast expires monotonically"
        );
        assert!(sidebar.copied.is_none());
    }

    #[test]
    fn sidebar_view_tracks_drawn_notes_and_ignores_unshown_chord_state() {
        let mut sidebar = two_sessions();
        let t0 = moment(160_000);
        sidebar.focus(Keys::Sidebar, t0.system);
        let area = Rect::new(0, 0, 32, 14);
        let mut buffer = Buffer::empty(area);
        sidebar.place(area);
        let _ = sidebar.draw(&mut buffer, area, t0.system);

        let before = sidebar.view();
        sidebar.copied(
            Copied::text(&Excerpt::of("copied".to_owned()).unwrap()),
            t0.steady,
        );
        assert_ne!(sidebar.view(), before, "SidebarView reflects copied toast");
        sidebar.edit(|state| state.copied = None);
        assert_eq!(sidebar.view(), before);

        sidebar.key(&char('g'), t0);
        let waiting_view = sidebar.view();
        assert!(matches!(sidebar.bottom(), Bottom::Nothing));

        let shown_at = t0 + PAUSE;
        assert!(sidebar.ticked(shown_at, true));
        assert_ne!(
            sidebar.view(),
            waiting_view,
            "SidebarView changes once the chord hint becomes shown"
        );
    }

    #[test]
    fn heard_after_idle_uses_current_wall_time_instead_of_stale_drawn_time() {
        let mut sidebar = fresh();
        let start = moment(160_000);
        let _ = seen_at(&mut sidebar, false, 32, 8, start);
        let after_idle = moment(260_000);
        let make = in_shell(1, going("make", 250, Front::Working));
        sidebar.heard(
            host(vec![session("s", 10, vec![make])], Vec::new()),
            after_idle.system,
        );
        assert_eq!(
            sidebar.due(after_idle),
            None,
            "a 10-second shell heard after idle is already long rather than waiting on stale drawn time"
        );
        assert_eq!(sidebar.counts.busy, 1);
    }

    #[test]
    fn far_future_shell_due_does_not_overflow_instant() {
        let mut sidebar = fresh();
        let far_future = i64::MAX - 10;
        let make = in_shell(1, going("make", far_future, Front::Working));
        let before_epoch = SystemTime::UNIX_EPOCH - Duration::from_secs(100);
        sidebar.heard(
            host(vec![session("s", 10, vec![make])], Vec::new()),
            before_epoch,
        );
        assert!(sidebar.due.is_some());

        let steady_now = Instant::now();
        let now = Moment {
            steady: steady_now,
            system: before_epoch,
        };
        assert_eq!(
            sidebar.due(now),
            None,
            "overflowing shell deadline degrades to None"
        );

        sidebar.copied(
            Copied::text(&Excerpt::of("copied".to_owned()).unwrap()),
            steady_now,
        );
        assert_eq!(
            sidebar.due(now),
            Some(steady_now + COPIED_SHOWS),
            "overflowing shell deadline does not suppress other timers"
        );
    }

    #[test]
    fn input_between_resize_and_draw_uses_the_resized_layout_and_draw_takes_shared_self() {
        let mut sidebar = two_sessions();
        let wall = test_wall();
        sidebar.focus(Keys::Sidebar, wall);
        let tall = Rect::new(0, 0, 32, 16);
        sidebar.place(tall);
        let _ = sidebar.aloud(Some(aloud::Shown::Reading {
            whose: aloud::Whose::Selection,
            text: "reading aloud".to_owned(),
            paused: false,
        }));
        let tall_stop = Position::new(tall.right() - 3, tall.bottom() - 2);
        assert_eq!(
            sidebar.aloud_buttons().at(tall_stop).copied(),
            Some(aloud::Button::Stop),
            "aloud buttons update immediately on place before any draw"
        );

        for _ in 0..4 {
            sidebar.key(&char('j'), moment(160_000));
        }
        assert_eq!(sidebar.viewport.top(), 0);

        let short = Rect::new(0, 0, 32, 8);
        sidebar.place(short);
        let short_stop = Position::new(short.right() - 3, short.bottom() - 2);
        assert_eq!(
            sidebar.aloud_buttons().at(tall_stop).copied(),
            None,
            "old button spot is gone before any draw"
        );
        assert_eq!(
            sidebar.aloud_buttons().at(short_stop).copied(),
            Some(aloud::Button::Stop),
            "new button spot is ready before any draw"
        );
        assert!(
            sidebar.viewport.top() > 0,
            "resizing smaller reveals the lit row immediately before any draw"
        );
        let top_before_draw = sidebar.viewport.top();
        let expected_lit = sidebar.lit_spot().cloned();
        let rows = framed_rows(short);
        let lit_row = sidebar.lit_index().unwrap();
        let lit_line: usize = sidebar.rows[..lit_row].iter().map(Row::height).sum();
        let lit_y = rows.y + u16::try_from(HEADER + lit_line - sidebar.viewport.top()).unwrap();

        let draw_shared =
            |sidebar: &Sidebar, buffer: &mut Buffer| sidebar.draw(buffer, short, wall);
        let mut buffer = Buffer::empty(short);
        let _ = draw_shared(&sidebar, &mut buffer);
        assert_eq!(sidebar.viewport.top(), top_before_draw);
        assert_eq!(sidebar.lit_spot().cloned(), expected_lit);

        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, lit_y), wall);
        let asked = sidebar.mouse(mouse(MouseEventKind::Up(left), 10, lit_y), wall);
        assert_eq!(
            asked,
            Asked::Go {
                on: Machine::Local,
                place: lit_place(&sidebar).unwrap(),
            }
        );

        let narrow = Rect::new(0, 0, COLLAPSED, 8);
        sidebar.collapse(narrow);
        assert_eq!(sidebar.aloud_buttons(), &Spots::default());
        let draw_collapsed_shared =
            |sidebar: &Sidebar, buffer: &mut Buffer| sidebar.draw(buffer, narrow, wall);
        let mut collapsed_buffer = Buffer::empty(narrow);
        let _ = draw_collapsed_shared(&sidebar, &mut collapsed_buffer);
    }

    #[test]
    fn event_rebuilds_use_their_own_wall_time_without_a_prior_draw_or_tick() {
        let mut sidebar = fresh();
        let make = in_shell(1, going("make", 159, Front::Working));
        sidebar.heard(
            host(vec![session("s", 10, vec![make])], Vec::new()),
            moment(160_000).system,
        );
        assert_eq!(sidebar.counts.busy, 0);
        assert!(sidebar.due.is_some());

        let later = moment(162_000);
        sidebar.focus(Keys::Sidebar, later.system);
        assert_eq!(
            sidebar.counts.busy, 1,
            "focus rebuild uses the event's wall time rather than stale or epoch time"
        );
        assert_eq!(sidebar.due, None);
    }

    #[test]
    fn clicking_an_open_menu_closes_it_without_activating_or_lifting_hidden_rows() {
        let mut sidebar = two_sessions();
        let wall = test_wall();
        let now = moment(160_000);
        let framed = Rect::new(0, 0, 32, 16);
        let rows_area = framed_rows(framed);
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);
        step_onto_its_first_view(&mut sidebar);

        assert_eq!(sidebar.key(&char('m'), now), Asked::Nothing);
        assert!(sidebar.open().is_some());
        let menu_top = sidebar.viewport.laid().unwrap().notes_y;
        assert!(
            usize::from(menu_top - rows_area.y) < HEADER + sidebar.lines(),
            "the open menu covers rows underneath it"
        );

        let left = MouseButton::Left;
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 5, menu_top), wall),
            Asked::Nothing
        );
        assert!(sidebar.open().is_none());
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Drag(left), 5, menu_top + 2), wall),
            Asked::Nothing
        );
        assert!(
            !matches!(sidebar.viewport.moving(), Some(Moving::Lifted(_))),
            "dragging from a row that was covered by the open menu does not lift a hidden row"
        );
        let _ = sidebar.mouse(mouse(MouseEventKind::Up(left), 5, menu_top + 2), wall);

        assert_eq!(sidebar.key(&char('m'), now), Asked::Nothing);
        assert!(sidebar.open().is_some());
        let menu_top = sidebar.viewport.laid().unwrap().notes_y;
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 5, menu_top), wall),
            Asked::Nothing
        );
        assert!(sidebar.open().is_none());
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Up(left), 5, menu_top), wall),
            Asked::Take,
            "clicking on the open menu closes it without activating the hidden row underneath"
        );
    }

    #[test]
    fn collapse_clears_active_mouse_drag_and_allows_draw_collapsed() {
        let mut sidebar = two_sessions();
        let wall = test_wall();
        let framed = Rect::new(0, 0, 32, 16);
        let rows_area = framed_rows(framed);
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);

        let project_row = sidebar
            .rows
            .iter()
            .position(|row| matches!(row, Row::Project { .. }))
            .unwrap();
        let project_line: usize = sidebar.rows[..project_row]
            .iter()
            .map(Row::height)
            .sum::<usize>()
            + sidebar.rows[project_row].gap();
        let y = rows_area.y + u16::try_from(HEADER + project_line).unwrap();
        let left = MouseButton::Left;

        let narrow = Rect::new(0, 0, COLLAPSED, 16);
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, y), wall),
            Asked::Nothing
        );
        assert!(matches!(
            sidebar.viewport.moving(),
            Some(Moving::Down { .. })
        ));
        sidebar.collapse(narrow);
        assert!(sidebar.viewport.moving().is_none());

        sidebar.place(framed);
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, y), wall),
            Asked::Nothing
        );
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, y + 2), wall),
            Asked::Take
        );
        assert!(matches!(sidebar.viewport.moving(), Some(Moving::Lifted(_))));

        sidebar.collapse(narrow);
        assert!(matches!(sidebar.viewport, Viewport::Collapsed { .. }));
        assert!(sidebar.viewport.moving().is_none());

        let mut buffer = Buffer::empty(narrow);
        let _ = sidebar.draw(&mut buffer, narrow, wall);
    }

    #[test]
    fn all_keys_scrolls_from_relayout_without_a_prior_draw() {
        let mut sidebar = two_sessions();
        let wall = test_wall();
        let now = moment(160_000);
        let framed = Rect::new(0, 0, 33, 22);
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);

        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        for _ in 0..200 {
            assert_eq!(sidebar.key(&char('j'), now), Asked::Nothing);
        }
        assert_eq!(sidebar.key(&char('k'), now), Asked::Nothing);

        let mut buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut buffer, framed, wall);
        let rows_area = framed_rows(framed);
        let second_to_last: String = (rows_area.x..rows_area.right() - 1)
            .map(|x| buffer[(x, rows_area.bottom() - 2)].symbol())
            .collect();
        assert_eq!(second_to_last.trim_end(), " Alt u        Read aloud, pause");
    }

    #[test]
    fn idle_tick_leaves_layout_clean_and_returns_false() {
        let mut sidebar = two_sessions();
        let now = moment(160_000);
        let framed = Rect::new(0, 0, 32, 16);
        sidebar.focus(Keys::Sidebar, now.system);
        sidebar.place(framed);

        assert!(!sidebar.ticked(now + Duration::from_millis(50), false));
        let mut buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut buffer, framed, now.system);
    }

    #[test]
    fn clicking_a_row_does_not_activate_a_different_row_when_rows_shift_before_release() {
        let wall = test_wall();
        let framed = Rect::new(0, 0, 32, 16);
        let rows_area = framed_rows(framed);
        let left = MouseButton::Left;

        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);
        let older_view = Spot::Local(view_of("older", 10, 1));
        let view_index = sidebar.row_of(&older_view).unwrap();
        let view_line: usize = sidebar.rows[..view_index].iter().map(Row::height).sum();
        let view_y = rows_area.y + u16::try_from(HEADER + view_line).unwrap();

        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, view_y), wall),
            Asked::Nothing
        );
        sidebar.hear(host(
            vec![session(
                "newer",
                20,
                vec![view(1, 0, "/w/b", "fish"), view(2, 0, "/w/c", "fish")],
            )],
            Vec::new(),
        ));
        assert!(
            sidebar
                .line_of(view_y, rows_area)
                .and_then(|line| sidebar.row_at(line))
                .is_some(),
            "another row shifted into the pressed line"
        );
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Up(left), 10, view_y), wall),
            Asked::Take,
            "releasing after the pressed row disappears never activates the shifted row"
        );

        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);
        let newer_spot = Spot::Local(Place::Session(listed("newer", 20)));
        let newer_index = sidebar.row_of(&newer_spot).unwrap();
        let newer_line: usize = sidebar.rows[..newer_index]
            .iter()
            .map(Row::height)
            .sum::<usize>()
            + sidebar.rows[newer_index].gap();
        let newer_y = rows_area.y + u16::try_from(HEADER + newer_line).unwrap();

        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, newer_y), wall),
            Asked::Nothing
        );
        sidebar.hear(host(
            vec![
                session("alpha", 5, vec![view(1, 0, "/w/a", "fish")]),
                session(
                    "older",
                    10,
                    vec![
                        view(2, 1, "/w/c", "nvim src/main.rs"),
                        view(1, 0, "/w/lune/main", "claude"),
                    ],
                ),
                session("newer", 20, vec![view(1, 0, "/w/b", "fish")]),
            ],
            Vec::new(),
        ));
        let shifted_at = sidebar
            .line_of(newer_y, rows_area)
            .and_then(|line| sidebar.row_at(line))
            .map(|at| sidebar.rows[at].spot());
        assert_ne!(shifted_at, Some(newer_spot));
        assert_eq!(
            sidebar.mouse(mouse(MouseEventKind::Up(left), 10, newer_y), wall),
            go_here(Place::Session(listed("newer", 20))),
            "releasing activates the pressed row rather than the row that shifted under the pointer"
        );
    }

    #[test]
    fn all_keys_scroll_uses_post_clear_bottom_room_when_a_notice_clears() {
        let mut sidebar = two_sessions();
        let wall = test_wall();
        let now = moment(160_000);
        let framed = Rect::new(0, 0, 33, 22);
        sidebar.focus(Keys::Sidebar, wall);
        sidebar.place(framed);

        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        for _ in 0..200 {
            assert_eq!(sidebar.key(&char('j'), now), Asked::Nothing);
        }
        sidebar.notify("a long error note ".repeat(8));
        assert_eq!(sidebar.key(&char('j'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('k'), now), Asked::Nothing);

        let mut buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut buffer, framed, wall);
        let rows_area = framed_rows(framed);
        let second_to_last: String = (rows_area.x..rows_area.right() - 1)
            .map(|x| buffer[(x, rows_area.bottom() - 2)].symbol())
            .collect();
        assert_eq!(second_to_last.trim_end(), " Alt u        Read aloud, pause");
    }

    #[test]
    fn sidebar_view_tracks_viewport_and_draw_handles_collapsed_and_expanded() {
        let wall = test_wall();
        let framed = Rect::new(0, 0, COLUMNS, 16);
        let mut sidebar = Sidebar::new(
            Folds::default(),
            News::default(),
            Kept::default(),
            "mac".to_owned(),
            framed,
        );
        sidebar.hear(with_nvim());
        assert!(matches!(sidebar.view(), SidebarView::Expanded(_)));

        let mut expanded_buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut expanded_buffer, framed, wall);

        let narrow = Rect::new(0, 0, COLLAPSED, 16);
        sidebar.collapse(narrow);
        assert!(matches!(sidebar.view(), SidebarView::Collapsed { .. }));
        let mut collapsed_buffer = Buffer::empty(narrow);
        let (drawn, cadence) = sidebar.draw(&mut collapsed_buffer, narrow, wall);
        assert!(matches!(drawn, Drawn::Nothing));
        assert_eq!(cadence, Cadence::default());

        let taller = Rect::new(0, 0, COLLAPSED, 20);
        sidebar.collapse(taller);
        let mut taller_buffer = Buffer::empty(taller);
        let _ = sidebar.draw(&mut taller_buffer, taller, wall);

        sidebar.place(framed);
        assert!(matches!(sidebar.view(), SidebarView::Expanded(_)));
        let _ = sidebar.draw(&mut expanded_buffer, framed, wall);
    }

    #[test]
    fn set_fold_relayouts_when_fold_folder_is_lost_on_an_unchanged_fold() {
        let dir = tempfile::tempdir().unwrap();
        let folder = crate::state::Folder::at(dir.path().to_owned());
        let folds = Folds::shared(&folder);
        let framed = Rect::new(0, 0, 32, 16);
        let wall = test_wall();
        let now = moment(160_000);
        let mut sidebar = Sidebar::new(
            folds,
            News::default(),
            Kept::default(),
            "mac".to_owned(),
            framed,
        );
        sidebar.hear(with_nvim());
        sidebar.focus(Keys::Sidebar, wall);

        let sessions = dir.path().join("folded");
        std::fs::remove_dir_all(&sessions).unwrap();
        std::fs::write(&sessions, "blocked").unwrap();

        assert_eq!(sidebar.key(&char('l'), now), Asked::Nothing);
        assert!(sidebar.folds.lost().is_some());

        let mut buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut buffer, framed, wall);
    }

    #[test]
    fn ticked_relayouts_when_news_loses_sharing_without_row_changes() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("lune");
        let news = News::within(&runtime);
        assert_eq!(news.lost(), None);
        let framed = Rect::new(0, 0, 32, 16);
        let start = moment(160_000);
        let mut sidebar = Sidebar::new(
            Folds::default(),
            news,
            Kept::default(),
            "mac".to_owned(),
            framed,
        );
        sidebar.hear(host(
            vec![own_session("s", 1, vec![view(1, 0, "/w/a", "fish")])],
            Vec::new(),
        ));
        sit(&mut sidebar, &own_view_of("s", 1, 1));
        assert!(!sidebar.ticked(start, true));

        let stayed = runtime.join("stayed");
        std::fs::remove_dir_all(&stayed).unwrap();
        std::fs::write(&stayed, "blocked").unwrap();

        let after_dwell = start + Duration::from_secs(2);
        assert!(
            sidebar.ticked(after_dwell, true),
            "losing news sharing on a row-neutral dwell reports changed"
        );
        assert!(sidebar.news.lost().is_some());

        let mut buffer = Buffer::empty(framed);
        let _ = sidebar.draw(&mut buffer, framed, after_dwell.system);
    }
}
