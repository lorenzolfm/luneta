use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::{SkimMatcherV2, SkimScoreConfig};
use luneta_protocol::git::{Changed, Changes, GitStatus, Head, Upstream};
use luneta_protocol::github::{PullRequest, State, Verdict};
use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;
use luneta_protocol::{tab, unread};
use lunetad::api::agent::Status as Reported;
use lunetad::api::remote::unreached::Stopped;
use lunetad::api::remote::{self, Reached};
use lunetad::api::shell::Doing;
use lunetad::api::{self, Agent, Front, Harness, Host, Pane, Saying, Shell, Status, UnreadFile};
use lunetad::asks::User;
use lunetad::issues::{Filed, Unfiled};
use lunetad::links::{self, Branch};
use lunetad::mux::own::{OwnClientId, OwnKey, OwnPane};
use lunetad::mux::TerminalKey;
use lunetad::projects::{self, Dir, ProjectId, ViewId, WorktreeId, CLONE_WORKTREES_MAX};
use lunetad::prunes;
use lunetad::seats::{self, seated, Where};
use lunetad::transcripts::SessionId;
use lunetad::{log, Destination, Directory};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::actions::Action;
use crate::agents::{self, Kept};
use crate::alerts::{self, Reach};
use crate::aloud;
use crate::brief::{Brief, Note, Over, Transcript};
use crate::checks;
use crate::close::{self, Act, Deletes, Plan, Scope, Seat, Subject, Untold, Whose};
use crate::folds::{Fold, Folds};
use crate::gruvbox;
use crate::icons;
use crate::issue::Repository;
use crate::ledger::{self, Key};
use crate::live::Lists;
use crate::machines::{self, Machine};
use crate::menu::{
    self, AllKeys, Briefed, Choice, Did, Done, Hint, Label, Menu, Opens, PaneRow, ProjectTarget,
    Ran, Rename, Target, Top, Wants, WorktreeRow,
};
use crate::news::News;
use crate::picker::{Here, Housed, Listing, Runs, Seated, Shelved, Unopened, Worktree};
use crate::printed::Printed;
use crate::projected;
use crate::prune;
use crate::pull::{Found, Kind, Link, Others, Wanted, Work, Worked};
use crate::skills::{Name, Skill};
use crate::spots::{Drawn, Keyed, Spots};
use crate::sweep::{cells, Page, Sweep};
use crate::zellij::{Closes, Open, SessionName, Tab};

pub const COLUMNS: u16 = 32;

pub const PAUSE: Duration = Duration::from_millis(200);

const GO: &str = "Go";
const GO_KEYS: [(Label, &str); 1] = [(Label::char('g'), "First row")];

const SELECTED_KEYS: [(Label, &str); 4] = [
    (Label::Two('j', 'k'), "Grow"),
    (Label::Two('J', 'K'), "Move"),
    (Label::char('v'), "Project, then done"),
    (Label::code(KeyCode::Esc), "Done"),
];

const COPIED_SHOWS: Duration = Duration::from_secs(3);

pub const NARROWEST: u16 = 16;

pub const COLLAPSED: u16 = 4;

const HERE_MARK: &str = "▌";
const NEW: &str = "•";
const UNFOLDED: &str = "▾";
const FOLDED: &str = "▸";
const RULE: &str = "─";
const MACHINE_RULE: &str = "═";
const MACHINE_LEAD: usize = 5;
const MISSING: &str = " missing";
const NO_PROJECTS: &str = "no projects";
const COMPUTER: &str = "\u{f108}";
const EDGE: &str = "│";
const BRACKET_TOP: &str = "╭";
const BRACKET_SIDE: &str = "│";
const BRACKET_FOOT: &str = "╰";
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub const SPIN: Duration = Duration::from_millis(100);
pub const SCROLL: Duration = Duration::from_millis(60);

pub const ACCENT: Color = gruvbox::PURPLE;
pub const NAME: Color = gruvbox::AQUA;
pub const TAG: Color = gruvbox::YELLOW;
pub const ERROR: Color = gruvbox::RED;
pub const LIT: Color = gruvbox::BG2;
const RESTING: Color = gruvbox::BG1;

const MARK: usize = 1;

const LABEL: usize = 3 + MARK + 1 + icons::WIDTH + 1;

const WORKTREE: usize = 4;

const LONG: Duration = Duration::from_secs(1);

const NO_HERE_PANE: &str =
    "menu: luneta does not know yet which view its client is on; try again in a moment";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Listed {
    name: SessionName,
    kept: ledger::Session,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProjectRow {
    listed: Listed,
    id: ProjectId,
}

impl ProjectRow {
    #[must_use]
    pub fn of(listed: Listed) -> Option<Self> {
        let id = listed.project()?;
        Some(ProjectRow { listed, id })
    }

    fn of_api(project: &api::Project) -> Option<Self> {
        let name = SessionName::running(&project.name).ok()?;
        let id = ProjectId::of(project.id)?;
        Some(ProjectRow { listed: Listed { name, kept: ledger::Session::Project(id) }, id })
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
        ProjectRow::of(Listed::new_project(name, id)).unwrap()
    }
}

impl Listed {

    fn project(&self) -> Option<ProjectId> {
        match self.kept {
            ledger::Session::Project(project) => Some(project),
            ledger::Session::Zellij { .. } => None,
        }
    }

    pub fn name(&self) -> &SessionName {
        &self.name
    }

    pub fn own_key(&self) -> Option<OwnKey> {
        match self.whose() {
            Whose::Own(key) => Some(key),
            Whose::Zellij(_) => None,
        }
    }

    pub fn whose(&self) -> Whose {
        match self.kept {
            ledger::Session::Zellij { started_ns, .. } => Whose::Zellij(ledger::moment(started_ns)),
            ledger::Session::Project(project) => Whose::Own(projected::key_of(project)),
        }
    }

    pub fn at(&self) -> Where<'_> {
        self.own_key().map_or_else(|| Where::Zellij(self.name.as_str()), Where::Own)
    }

    pub fn own(&self) -> bool {
        self.own_key().is_some()
    }

    fn key(&self, pane: u32) -> Key {
        Key { session: self.kept, pane }
    }

    #[cfg(test)]
    pub fn new(name: &str, started: SystemTime) -> Self {
        let kept = ledger::Session::new(started, std::num::NonZeroU64::MIN).unwrap();
        Listed { name: SessionName::parse(name).unwrap(), kept }
    }

    #[cfg(test)]
    pub fn new_project(name: &str, id: u64) -> Self {
        let kept = ledger::Session::Project(lunetad::projects::ProjectId::of(id).unwrap());
        Listed { name: SessionName::parse(name).unwrap(), kept }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
    Session(Listed),
    Worktree(WorktreePlace),
    Pane(PanePlace),
}

impl Place {
    pub fn session(&self) -> &str {
        self.listed().name.as_str()
    }

    pub fn pane(&self) -> Option<u32> {
        match self {
            Place::Session(_) | Place::Worktree(_) => None,
            Place::Pane(place) => Some(place.pane.id()),
        }
    }

    pub fn listed(&self) -> &Listed {
        match self {
            Place::Session(session) | Place::Pane(PanePlace { session, .. }) => session,
            Place::Worktree(WorktreePlace { project, .. }) => project.listed(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WorktreePlace {
    pub project: ProjectRow,
    pub worktree: WorktreeId,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PanePlace {
    pub session: Listed,
    pub pane: PaneKey,
}

impl PanePlace {
    pub fn new(session: Listed, pane: PaneKey) -> Option<Self> {
        let agrees = match (session.whose(), pane) {
            (Whose::Zellij(_), PaneKey::Zellij(_)) | (Whose::Own(_), PaneKey::Terminal(_)) => true,
            (Whose::Zellij(_), PaneKey::Terminal(_)) | (Whose::Own(_), PaneKey::Zellij(_)) => false,
        };
        agrees.then_some(PanePlace { session, pane })
    }

    pub fn of(session: Listed, pane: &Pane) -> Option<Self> {
        let pane = match session.whose() {
            Whose::Zellij(_) => PaneKey::Zellij(pane.id),
            Whose::Own(_) => {
                let key = TerminalKey::of(pane.key.as_ref()?).ok()?;
                (key.id.get() == pane.id).then_some(PaneKey::Terminal(key))?
            },
        };
        PanePlace::new(session, pane)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PaneKey {
    Zellij(u32),
    Terminal(TerminalKey),
}

impl PaneKey {
    #[must_use]
    pub fn id(self) -> u32 {
        match self {
            PaneKey::Zellij(pane) => pane,
            PaneKey::Terminal(key) => key.id.get(),
        }
    }
}

impl PaneKey {
    #[cfg(test)]
    pub fn terminal(id: u32) -> Self {
        let made = lunetad::mux::own::OwnMade::from_nanos(0);
        PaneKey::Terminal(TerminalKey { id: NonZeroU32::new(id).unwrap(), made })
    }
}

impl fmt::Display for PaneKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.id().fmt(f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnSeat {
    Coming(OwnKey),
    On { pane: OwnPane, client: OwnClientId },
}

impl OwnSeat {
    #[must_use]
    pub fn session(self) -> OwnKey {
        match self {
            OwnSeat::Coming(session) => session,
            OwnSeat::On { pane, .. } => pane.session,
        }
    }

    #[must_use]
    pub fn on(self) -> Option<OwnPane> {
        match self {
            OwnSeat::Coming(_) => None,
            OwnSeat::On { pane, .. } => Some(pane),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sitting {
    on: Machine,
    own: OwnSeat,
    shown: Option<WorktreeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereAgent {
    pub on: Machine,
    pub session: SessionName,
    pub pane: u32,
    pub dir: String,
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

    fn place(&self) -> Option<&Place> {
        match self {
            Spot::Machine(_) => None,
            Spot::Local(place) | Spot::Remote(_, place) => Some(place),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    Waiting,
    Idle,
    Finished,
    Busy,
    Shell,
    Other(char),
    Unreported,
    Passed,
    Failed,
    Ran,
}

impl Mark {
    fn of(agent: &Agent) -> Self {
        match &agent.status {
            Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
                Ok(Status::Waiting) => Mark::Waiting,
                Ok(Status::Idle) => Mark::Idle,
                Ok(Status::Busy) => Mark::Busy,
                Ok(Status::Shell) => Mark::Shell,
                Ok(Status::Unspecified) | Err(_) => Mark::Unreported,
            },
            Some(Reported::OtherStatus(word)) => {
                initial(word).map_or(Mark::Unreported, Mark::Other)
            },
            None => Mark::Unreported,
        }
    }

    fn is_news(&self) -> bool {
        matches!(self, Mark::Finished | Mark::Passed | Mark::Failed | Mark::Ran)
    }

    pub fn glyph(&self, frame: usize) -> (String, Style) {
        let style = Style::new();
        match self {
            Mark::Waiting => ("●".to_owned(), style.fg(ACCENT).add_modifier(Modifier::BOLD)),
            Mark::Idle => ("✓".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Finished | Mark::Passed => {
                ("✓".to_owned(), style.fg(gruvbox::GREEN).add_modifier(Modifier::BOLD))
            },
            Mark::Busy => (SPINNER[frame].to_owned(), style.fg(TAG)),
            Mark::Shell => ("$".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Other(letter) => (letter.to_string(), style.fg(TAG)),
            Mark::Unreported => ("?".to_owned(), style.add_modifier(Modifier::DIM)),
            Mark::Failed => ("✗".to_owned(), style.fg(gruvbox::RED).add_modifier(Modifier::BOLD)),
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
        Marked { mark: Mark::of(agent), since }
    }

    fn of_shell(shell: &Shell, now: SystemTime) -> Option<(Self, Option<&str>)> {
        let (mark, since, command) = match shell.doing.as_ref()? {
            Doing::Going(going) => {
                let since = told(going.since)?;
                if now < since.checked_add(LONG)? {
                    return None;
                }
                let mark = match Front::try_from(going.front) {
                    Ok(Front::Working) => Mark::Busy,
                    Ok(Front::Asking) => Mark::Waiting,
                    Ok(Front::Interactive | Front::Unspecified) | Err(_) => return None,
                };
                (mark, since, going.command.as_deref())
            },
            Doing::Ended(ended) => {
                if Duration::try_from(ended.took?).ok()? < LONG {
                    return None;
                }
                let mark = match ended.ending.as_ref().map(Ending::of_api) {
                    Some(Ending::Exited(0)) => Mark::Passed,
                    Some(Ending::Exited(_)) => Mark::Failed,
                    Some(Ending::Resumed) | None => Mark::Ran,
                };
                (mark, told(ended.at)?, ended.command.as_deref())
            },
            Doing::Stopped(_) | Doing::Ready(()) => return None,
        };
        Some((Marked { mark, since: Some(since) }, command))
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
    fn of(by_pane: &[Shows<'_>]) -> Tally {
        let marks = || by_pane.iter().filter_map(|shows| Some(&shows.marked.as_ref()?.mark));
        Tally {
            waiting: marks().filter(|mark| **mark == Mark::Waiting).count(),
            dots: by_pane.iter().filter(|shows| shows.dotted()).count(),
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
    marked: Option<Marked>,
    ran: Option<&'a str>,
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

fn told(at: Option<impl TryInto<SystemTime>>) -> Option<SystemTime> {
    at?.try_into().ok()?.checked_add(Duration::from_millis(999))
}

fn ran(shell: &Shell) -> Ran {
    let ran = match shell.doing.as_ref() {
        Some(Doing::Going(going)) => said(going.command.as_ref(), Ran::Going),
        Some(Doing::Stopped(stopped)) => said(stopped.command.as_ref(), Ran::Stopped),
        Some(Doing::Ended(ended)) => ended_last(ended),
        Some(Doing::Ready(())) => Some(Ran::Ready),
        None => None,
    };
    ran.unwrap_or(Ran::Unknown)
}

fn said(command: Option<&String>, ran: impl FnOnce(Option<Said>) -> Ran) -> Option<Ran> {
    match command {
        None => Some(ran(None)),
        Some(command) => Some(ran(Some(Said::new(command.clone())?))),
    }
}

fn ended_last(ended: &lunetad::api::shell::Ended) -> Option<Ran> {
    let at = SystemTime::try_from(ended.at?).ok()?;
    let ending = ended.ending.as_ref().map(Ending::of_api);
    said(ended.command.as_ref(), |command| match command {
        Some(command) => Ran::Ended(menu::Ended { command, ending, at }),
        None => Ran::EndedUnsaid,
    })
}

fn long_at(shell: &Shell) -> Option<SystemTime> {
    match shell.doing.as_ref()? {
        Doing::Going(going) => told(going.since)?.checked_add(LONG),
        Doing::Ended(_) | Doing::Stopped(_) | Doing::Ready(()) => None,
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
        name: String,
        missing: bool,
        hits: Vec<usize>,
        under: Under,
        here: bool,
        colour: Color,
        waiting: usize,
        dot: bool,
        busy: bool,
        checkout: Option<Checkout>,
    },
    Pane {
        machine: Machine,
        session: Listed,
        pane: PaneKey,
        here: bool,
        unread: bool,
        marked: Option<Marked>,
        icon: Option<&'static str>,
        label: String,
        hits: Vec<usize>,
        bracket: Option<(Bracket, Color)>,
        speaks: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Heads {
    Nothing,
    Projects { folded: bool, here: bool, waiting: usize, dot: bool, busy: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bracket {
    First,
    Inner,
    Last,
}

impl Bracket {
    fn of(at: usize, count: usize) -> Option<Bracket> {
        match at {
            _ if count < 2 => None,
            0 => Some(Bracket::First),
            _ if at + 1 == count => Some(Bracket::Last),
            _ => Some(Bracket::Inner),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Checkout {
    branch: String,
    status: String,
    changed: Option<Changed>,
    pull_request: Option<Numbered>,
}

fn changed_shown(changed: Changed) -> (Vec<Span<'static>>, usize) {
    let (added, deleted) = (changed.added(), changed.deleted());
    let mut spans = Vec::new();
    if added > 0 {
        spans.push(Span::styled(format!("+{added}"), Style::new().fg(gruvbox::GREEN)));
    }
    if deleted > 0 {
        if added > 0 {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(format!("-{deleted}"), Style::new().fg(gruvbox::RED)));
    }
    let width = spans.iter().map(Span::width).sum();
    (spans, width)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Numbered {
    number: Number,
    colour: Color,
    checks: Option<Verdict>,
    stale: bool,
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
}

impl Row {
    fn spot(&self) -> Spot {
        match self {
            Row::Machine { machine, .. } => Spot::Machine(machine.clone()),
            Row::Project { machine, project, .. } => {
                Spot::on(machine, Place::Session(project.listed().clone()))
            },
            Row::Worktree { machine, place, .. } => {
                Spot::on(machine, Place::Worktree(place.clone()))
            },
            Row::Pane { machine, session, pane, .. } => {
                Spot::on(machine, Place::Pane(PanePlace { session: session.clone(), pane: *pane }))
            },
        }
    }

    fn height(&self) -> usize {
        match self {
            Row::Worktree { checkout: Some(_), .. } => 2,
            _ => 1,
        }
    }

    fn stops(&self) -> bool {
        match self {
            Row::Machine { heads: Heads::Projects { folded, .. }, .. } => *folded,
            Row::Project { under, .. } | Row::Worktree { under, .. } => *under != Under::Rows,
            Row::Machine { heads: Heads::Nothing, .. } | Row::Pane { .. } => true,
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
    Unreached { since: Option<SystemTime>, word: &'static str, why: String },
}

impl Away {
    fn of(unreached: &remote::Unreached) -> Away {
        let word = match unreached.stopped {
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(255) })) => "unreachable",
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(127) })) => "no luneta there",
            Some(Stopped::Ended(remote::unreached::Ended { code: Some(1) })) => {
                "luneta there failed"
            },
            Some(Stopped::Ended(_)) => "ssh ended",
            Some(Stopped::Unstarted(())) => "ssh did not start",
            Some(Stopped::Said(())) => "its login prints",
            Some(Stopped::Silent(())) => "no answer",
            Some(Stopped::Lost(())) => "lost",
            Some(Stopped::Unwatched(())) => "not watched",
            None => "unreached",
        };
        let since = unreached.since.and_then(|at| SystemTime::try_from(at).ok());
        Away::Unreached { since, word, why: unreached.why.clone() }
    }

    fn said(&self) -> (&'static str, Option<SystemTime>) {
        match self {
            Away::Reaching => ("reaching…", None),
            Away::Unreached { since, word, .. } => (word, *since),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Known {
    project: ProjectRow,
    dir: Dir,
    worktrees: Vec<KnownWorktree>,
    order: Vec<WorktreeId>,
    unopened: Unopened,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct KnownWorktree {
    place: WorktreePlace,
    name: String,
    folder: Dir,
    seen: projects::Seen,
    used: Option<SystemTime>,
    panes: Vec<Pane>,
    terminals: Vec<ViewId>,
    in_panes: Vec<(u32, ViewId)>,
}

impl Known {
    fn of(said: &api::Project) -> Option<Known> {
        let project = ProjectRow::of_api(said)?;
        let rows = said.worktrees()?;
        let worktrees = rows.iter().zip(0..).filter_map(|(worktree, tab)| {
            let id = WorktreeId::of(worktree.id)?;
            let place = WorktreePlace { project: project.clone(), worktree: id };
            let terminals = worktree.terminals.iter().map(|terminal| {
                ViewId::of(terminal.id).expect("lunetad says a terminal's id, and no id is 0")
            });
            let terminals: Vec<ViewId> = terminals.collect();
            let found = worktree.terminals.iter().zip(&terminals);
            let held = found.filter_map(|(it, id)| {
                Some((*id, pane_of(it, worktree, tab)?))
            });
            let held: Vec<(ViewId, Pane)> = held.collect();
            let in_panes = held.iter().map(|(id, pane)| (pane.id, *id)).collect();
            let panes = held.into_iter().map(|(_, pane)| pane).collect();
            let seen = worktree.seen.and_then(projects::Seen::of_api);
            let seen = seen.expect("lunetad says a worktree's stage luneta knows");
            let used = worktree.used_at.map(|at| {
                SystemTime::try_from(at).expect("lunetad says a worktree's last use as a time")
            });
            let folder = Dir::parse(&worktree.folder);
            let folder = folder.expect("lunetad says a worktree's folder as a folder it keeps");
            let name = worktree.name.clone();
            Some(KnownWorktree { place, name, folder, seen, used, panes, terminals, in_panes })
        });
        let worktrees: Vec<KnownWorktree> = worktrees.collect();
        let order = rows.iter().filter_map(|it| WorktreeId::of(it.id)).collect();
        let dir = Dir::parse(&said.dir).expect("lunetad says a project's folder as one it keeps");
        let unopened = Known::unopened_of(said, &dir, &worktrees);
        Some(Known { project, dir, worktrees, order, unopened })
    }

    fn unopened_of(said: &api::Project, dir: &Dir, worktrees: &[KnownWorktree]) -> Unopened {
        use api::project::clone::Listing as Said;
        let clone = match &said.holds {
            Some(api::project::Holds::Clone(clone)) => clone,
            Some(api::project::Holds::Folder(_)) | None => return Unopened::Folder,
        };
        let listed = match &clone.listing {
            Some(Said::Unopened(listed)) => &listed.subfolders,
            Some(Said::Unlisted(why)) => return Unopened::Unlisted(why.clone()),
            None => return Unopened::Unlisted("lunetad says nothing of them".to_owned()),
        };
        let max = CLONE_WORKTREES_MAX;
        assert!(listed.len() <= max, "a clone lists at most {max} worktrees");
        let open: BTreeSet<&Dir> = worktrees.iter().map(|it| &it.folder).collect();
        let unopened = listed.iter().map(|it| {
            let subfolder = projects::Subfolder::parse(it);
            let subfolder = subfolder.expect("lunetad says a worktree not open as a subfolder");
            let folder = dir.at(&subfolder);
            assert!(!open.contains(&folder), "{it} is said not open, so no open worktree is there");
            folder
        });
        Unopened::Listed(unopened.collect())
    }

    fn listed(&self) -> &Listed {
        self.project.listed()
    }

    fn panes(&self) -> impl Iterator<Item = &Pane> {
        self.worktrees.iter().flat_map(|worktree| &worktree.panes)
    }

    fn pane(&self, id: u32) -> Option<&Pane> {
        self.panes().find(|pane| pane.id == id)
    }
}

fn project_of(spot: &Spot) -> Option<Spot> {
    let listed = spot.place()?.listed().clone();
    Some(Spot::on(&spot.machine(), Place::Session(listed)))
}

fn pane_of(terminal: &api::Terminal, worktree: &api::Worktree, tab: u32) -> Option<Pane> {
    let Some(api::terminal::Found::Key(key)) = &terminal.found else {
        return None;
    };
    let id = TerminalKey::of(key).ok()?.id.get();
    Some(Pane {
        id,
        tab,
        tab_name: worktree.name.clone(),
        cwd: Some(terminal.folder.clone()).filter(|folder| !folder.is_empty()),
        command: terminal.command.clone(),
        titled: terminal.titled.clone(),
        shell: terminal.shell.clone(),
        tab_id: tab.checked_add(1),
        held: terminal.held,
        key: Some(*key),
    })
}

#[derive(Debug, Default)]
struct Shelf {
    machine: Machine,
    projects: Vec<Known>,
    unlisted: usize,
    order: Vec<ProjectId>,
    version: Option<api::Version>,
    kept: bool,
}

impl Shelf {
    fn known(&self, project: ProjectId) -> Option<&Known> {
        self.projects.iter().find(|known| known.project.id() == project)
    }

    fn orders(&self, order: &Order) -> bool {
        match order {
            Order::Projects(order) => permutes(&self.order, order),
            Order::Worktrees { project, worktrees } => {
                self.known(*project).is_some_and(|known| permutes(&known.order, worktrees))
            },
        }
    }

    fn reorder(&mut self, order: &Order) {
        assert!(self.orders(order), "an order reorders what is kept");
        match order {
            Order::Projects(order) => {
                order.clone_into(&mut self.order);
                self.projects.sort_by_key(|known| position(order, known.project.id()));
            },
            Order::Worktrees { project, worktrees } => {
                let mut known = self.projects.iter_mut();
                let known = known.find(|known| known.project.id() == *project);
                let known = known.expect("the project reordered is shelved");
                worktrees.clone_into(&mut known.order);
                let at = |worktree: &KnownWorktree| position(worktrees, worktree.place.worktree);
                known.worktrees.sort_by_key(at);
            },
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
    let (kept, version, held) = match projects.and_then(|projects| projects.held.as_ref()) {
        Some(api::projects::Held::Kept(kept)) => (kept.projects.as_slice(), kept.version, true),
        Some(api::projects::Held::Unkept(_)) | None => (&[][..], None, false),
    };
    let projects: Vec<Known> = kept.iter().filter_map(Known::of).collect();
    let unlisted = kept.len() - projects.len();
    let order = kept.iter().filter_map(|project| ProjectId::of(project.id)).collect();
    Shelf { machine, projects, unlisted, order, version, kept: held }
}

fn wanted<I: PartialEq + Copy>(all: &[I], listed: &[I]) -> Vec<I> {
    let mut slots = listed.iter();
    let slot = |id: &I, slots: &mut std::slice::Iter<'_, I>| {
        if listed.contains(id) { slots.next().copied().unwrap_or(*id) } else { *id }
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
            Some(Reached::Reaching(())) | None => None,
        };
        shelves.push(shelved(Machine::Remote(destination), projects));
    }
    shelves
}

fn shelf<'a>(shelves: &'a [Shelf], machine: &Machine) -> &'a [Known] {
    let found = shelves.iter().find(|shelf| shelf.machine == *machine);
    found.map_or(&[], |shelf| shelf.projects.as_slice())
}

struct Seen<'a> {
    projects: &'a [Known],
    agents: &'a [Agent],
    git_statuses: &'a [lunetad::api::GitStatus],
    pull_requests: &'a [lunetad::api::PullRequest],
    unread: u32,
    unread_files: &'a [UnreadFile],
    sayings: &'a [Saying],
}

impl Seen<'_> {
    fn unread_seats(&self) -> Result<Vec<(Where<'_>, u32)>, unread::Unplaced> {
        seats::unread(self.unread, self.unread_files)
    }

    fn seated(&self) -> HashMap<(Where<'_>, u32), &Agent> {
        seated(self.agents, &[])
    }

    fn panes(&self) -> impl Iterator<Item = (&Listed, &Pane)> {
        let each = self.projects.iter();
        each.flat_map(|known| known.panes().map(move |pane| (known.listed(), pane)))
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
            },
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
            Some(Reached::Unreached(unreached)) => Err(Away::of(unreached)),
            None => Err(Away::Unreached {
                since: None,
                word: "unreached",
                why: "lunetad says how it stands in words this luneta cannot read, as a newer \
                      lunetad's"
                    .to_owned(),
            }),
        };
        others.push((destination, seen));
    }
    Machines { here, others, left_out }
}

fn opens_own(asking: Held, on: &Machine, wants: Wants) -> Result<Asked, Wants> {
    match wants {
        Wants::Open { session, pane, cwd, opens } => match session.own_key() {
            Some(key) => {
                let session = session.name;
                let open = match opens {
                    Opens::Pane { beside, command } => Open::Pane { session, beside, cwd, command },
                    Opens::Tab { name, command } => {
                        let tab = Tab { cwd, name, command };
                        Open::Tab { session, tab, alone_since: None }
                    },
                };
                Ok(Asked::Open { asking, on: on.clone(), key, open })
            },
            None => Err(Wants::Open { session, pane, cwd, opens }),
        },
        wants => Err(wants),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    Nothing,
    OpenWorktree { on: Machine, project: ProjectRow },
    Prunes { menu: u64, on: Machine, dir: Directory },
    Prune { menu: u64, on: Machine, dir: Directory, agreed: BTreeSet<prunes::Deleting> },
    Go { on: Machine, place: Place },
    Take,
    Copy(Link),
    CopyText(String),
    Browse(Link),
    Leave,
    Closes { menu: u64, on: Machine, session: Listed, pane: PaneKey },
    Delete { on: Machine, act: Act },
    Open { asking: Held, on: Machine, key: OwnKey, open: Open },
    OpenZellij { menu: u64, on: Machine, open: Open },
    NewWorktree { on: Machine, project: ProjectId, branch: Branch },
    Welcome { asking: Held, on: Machine, session: Listed, pane: PaneKey },
    AddProject { on: Machine, folder: String, name: Option<projects::Name> },
    Rename { on: Machine, rename: Rename },
    Make {
        menu: u64,
        on: Machine,
        session: SessionName,
        holder: Directory,
        cwd: Directory,
        name: String,
        command: Vec<String>,
    },
    Pull { menu: u64, on: Machine, work: Work, wanted: Wanted, worked: Vec<Worked>, others: Others },
    Find { menu: u64, on: Machine, cwd: String, agent: agents::Harness },
    Printed { menu: u64, on: Machine, session: Listed, pane: PaneKey, ended: menu::Ended },
    Checks { menu: u64, on: Machine, page: checks::PullPage },
    Brief(Brief),
    Ask(Target),
    Issue(Repository),
    Ssh(Destination),
    Forget(Destination),
    Move { on: Machine, order: Reorder, ticket: u64 },
}

impl Asked {
    fn menu(&self) -> Option<u64> {
        match self {
            Asked::Prunes { menu, .. }
            | Asked::Prune { menu, .. }
            | Asked::Closes { menu, .. }
            | Asked::OpenZellij { menu, .. }
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
            | Asked::Delete { .. }
            | Asked::Open { .. }
            | Asked::NewWorktree { .. }
            | Asked::Welcome { .. }
            | Asked::AddProject { .. }
            | Asked::Rename { .. }
            | Asked::Brief(_)
            | Asked::Ask(_)
            | Asked::Issue(_)
            | Asked::Ssh(_)
            | Asked::Forget(_)
            | Asked::Move { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    Open,
    Kept,
}

const HEADER: usize = 3;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keys {
    #[default]
    Zellij,
    Sidebar,
    Lent,
    Menu { lent: bool },
}

impl Keys {
    fn held(self) -> bool {
        match self {
            Keys::Sidebar | Keys::Lent | Keys::Menu { lent: true } => true,
            Keys::Zellij | Keys::Menu { lent: false } => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    projects: usize,
    panes: usize,
    waiting: usize,
    new: usize,
    busy: usize,
    shell: usize,
}

#[derive(Default)]
pub struct Sidebar {
    host: Option<Host>,
    this: String,
    away: Option<String>,
    seat: Option<Sitting>,
    seat_name: Option<String>,
    folds: Folds,
    news: News,
    agent: Kept,
    shelves: Vec<Shelf>,
    unheard: Vec<Unheard>,
    tickets: u64,
    here_key: Option<Key>,
    finished: HashMap<Key, SystemTime>,
    rows: Vec<Row>,
    counts: Counts,
    unlisted: usize,
    unlisted_machines: usize,
    keys: Keys,
    here_row: Option<Spot>,
    cursor: Option<Spot>,
    pending: Pending,
    holds: Holds,
    moving: Moving,
    homes: HashMap<Spot, Spot>,
    revealed: Option<Spot>,
    top: usize,
    page: Page,
    laid: Layout,
    room: usize,
    notice: Option<String>,
    copied: Option<(NonZeroU32, SystemTime)>,
    image_going: Option<Destination>,
    aloud: Option<aloud::Shown>,
    aloud_buttons: Spots<aloud::Button>,
    said: Option<String>,
    search: Option<Search>,
    menu: Option<Menu>,
    working: BTreeMap<u64, Menu>,
    filing: BTreeMap<u64, String>,
    filed: Option<String>,
    menus: u64,
    closing: Option<Closing>,
    drawn: Option<SystemTime>,
    due: Option<SystemTime>,
}

#[derive(Debug)]
struct Closing {
    gone: Spot,
    after: Option<Spot>,
}

#[derive(Debug, Default)]
enum Pending {
    #[default]
    Nothing,
    Legend,
    First { shows: SystemTime },
    Group { key: char, shows: SystemTime },
    AllKeys(AllKeys),
}

impl Pending {
    fn shows(&self) -> Option<SystemTime> {
        match self {
            Pending::First { shows } | Pending::Group { shows, .. } => Some(*shows),
            Pending::Nothing | Pending::Legend | Pending::AllKeys(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Came {
    Typed,
    Read,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Holds {
    #[default]
    Nothing,
    Until(u64, SystemTime),
    Quiet(u64),
}

#[derive(Debug, Default)]
enum Moving {
    #[default]
    Still,
    Selected(Movable),
    Down { at: Position, lifts: Option<Movable> },
    Lifted(Lift),
    Sweeping(Sweep),
    Swept(Sweep, Layout),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layout {
    area: Rect,
    top: usize,
    rows: Vec<(Spot, usize)>,
    bottom: usize,
    notes: Vec<String>,
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
                Place::Session(session) => {
                    Some(Movable::Project(machine, ProjectRow::of(session)?))
                },
                Place::Worktree(place) => Some(Movable::Worktree(machine, place)),
                Place::Pane(_) => None,
            },
        }
    }

    fn spot(&self) -> Spot {
        match self {
            Movable::Project(machine, project) => {
                Spot::on(machine, Place::Session(project.listed().clone()))
            },
            Movable::Worktree(machine, place) => {
                Spot::on(machine, Place::Worktree(place.clone()))
            },
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
            },
            Spans::Worktrees(machine, project, worktrees) => {
                Spans::Worktrees(machine.clone(), project.clone(), moved(worktrees, taken, to))
            },
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
    Projects { seen: Vec<ProjectId>, wanted: Vec<ProjectId> },
    Worktrees { project: ProjectId, seen: Vec<WorktreeId>, wanted: Vec<WorktreeId> },
}

impl Reorder {
    fn seen(&self) -> Order {
        match self {
            Reorder::Projects { seen, .. } => Order::Projects(seen.clone()),
            Reorder::Worktrees { project, seen, .. } => {
                Order::Worktrees { project: *project, worktrees: seen.clone() }
            },
        }
    }

    fn wanted(&self) -> Order {
        match self {
            Reorder::Projects { wanted, .. } => Order::Projects(wanted.clone()),
            Reorder::Worktrees { project, wanted, .. } => {
                Order::Worktrees { project: *project, worktrees: wanted.clone() }
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Order {
    Projects(Vec<ProjectId>),
    Worktrees { project: ProjectId, worktrees: Vec<WorktreeId> },
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
    Answers { asked: NonZeroUsize, landed: Option<api::Version> },
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
                let asked = asked.checked_add(1).expect("fewer than 2^64 moves are asked");
                Wait::Answers { asked, landed }
            },
            Wait::Watch(landed) => Wait::Answers { asked: NonZeroUsize::MIN, landed: Some(landed) },
        }
    }

    fn answered(self, kept: Option<api::Version>) -> Option<Wait> {
        let Wait::Answers { asked, .. } = self else {
            unreachable!("lunetad answers each move asked once");
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
            },
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

#[derive(Clone, Copy)]
enum Highlight {
    Lit(Option<usize>),
    Resting(usize),
    Off,
}

impl Sidebar {
    pub fn new(folds: Folds, news: News, agent: Kept, this: String) -> Self {
        Sidebar { this, folds, news, agent, ..Sidebar::default() }
    }

    pub fn reagent(&mut self) {
        if let Err(why) = self.agent.reload() {
            log::say(&format!("agent: {why}"));
        }
    }

    pub fn refold(&mut self) {
        if self.folds.reload() {
            self.rebuild();
        }
    }

    pub fn reread(&mut self) {
        if self.news.reload() {
            self.rebuild();
        }
    }

    pub fn heard(&mut self, host: Host) {
        self.shelve(&host);
        for shelf in self.shelves.iter().filter(|shelf| shelf.kept && shelf.unlisted == 0) {
            let machine = &shelf.machine;
            if shelf.projects.is_empty() && self.folds.contains(Fold::Machine(machine)) {
                self.folds.set(Fold::Machine(machine), false);
            }
            let mut live = Vec::new();
            for known in &shelf.projects {
                live.push(Fold::Project(machine, known.project.id()));
                let worktrees = known.worktrees.iter();
                live.extend(worktrees.map(|it| Fold::Worktree(machine, it.place.worktree)));
            }
            self.folds.listed(machine, &live);
        }
        self.host = Some(host);
        self.away = None;
        self.rebuild();
        self.reran();
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
            on.expect("an unheard order's machine is shelved").reorder(&unheard.shown);
        }
    }

    fn reshelve(&mut self) {
        if let Some(host) = self.host.take() {
            self.shelve(&host);
            self.host = Some(host);
            self.rebuild();
        }
    }

    pub fn moved(&mut self, ticket: u64, kept: Option<api::Version>) {
        let Some(at) = self.unheard.iter().position(|unheard| unheard.ticket == ticket) else {
            return;
        };
        match self.unheard[at].wait.answered(kept) {
            Some(wait @ Wait::Answers { .. }) => self.unheard[at].wait = wait,
            Some(wait @ Wait::Watch(_)) => {
                self.unheard[at].wait = wait;
                self.reshelve();
            },
            None => {
                self.unheard.remove(at);
                self.reshelve();
            },
        }
    }

    pub fn unsent(&mut self, ticket: u64, order: &Reorder, why: &str) {
        self.notify(format!("not moved: {why}"));
        let Some(at) = self.unheard.iter().position(|unheard| unheard.ticket == ticket) else {
            return;
        };
        let unheard = &mut self.unheard[at];
        assert_eq!(unheard.shown, order.wanted(), "only the move asked last goes unsent");
        unheard.shown = order.seen();
        match unheard.wait.unasked() {
            Some(wait) => unheard.wait = wait,
            None => {
                self.unheard.remove(at);
            },
        }
        self.reshelve();
    }

    pub fn unmoved(&mut self, on: &Machine, ticket: u64, order: &Reorder, note: String) {
        let of = order.wanted().of();
        let mut newer = self.unheard.iter().filter(|it| it.on == *on && it.shown.of() == of);
        if !newer.any(|it| it.ticket > ticket) {
            self.notify(note);
        }
        let before = self.unheard.len();
        self.unheard.retain(|unheard| unheard.ticket != ticket);
        assert!(self.unheard.len() + 1 >= before, "no two unheard orders share a ticket");
        if self.unheard.len() < before {
            self.reshelve();
        }
    }

    pub fn ticked(&mut self, now: SystemTime, at: Instant, watched: bool) {
        self.hold(at);
        let over = |then: SystemTime| now.duration_since(then).is_ok_and(|it| it >= COPIED_SHOWS);
        self.copied.take_if(|(_, then)| over(*then));
        let here = self.here_key.filter(|_| watched);
        let finished = &self.finished;
        if self.news.dwell(here, |key| finished.get(&key).copied(), now, at) {
            self.rebuild();
        }
        let live = self.live();
        self.news.listed(live.as_ref(), at);
    }

    fn live(&self) -> Option<Lists> {
        let host = self.host.as_ref()?;
        if self.away.is_some() {
            return None;
        }
        let mut live = Lists::default();
        for (machine, seen) in machines(host, &self.shelves).each() {
            let Ok(seen) = seen else {
                live.lists(&machine, [], false);
                continue;
            };
            let whole = self.shelves.iter().any(|it| {
                it.machine == machine && it.kept && it.unlisted == 0
            });
            live.lists(&machine, seen.projects.iter().map(|known| known.listed().kept), whole);
        }
        Some(live)
    }

    pub fn lost(&mut self, why: String) {
        if self.away.as_ref() != Some(&why) {
            log::say(&format!("lunetad: {why}"));
        }
        self.away = Some(why);
    }

    pub fn focus(&mut self, keys: Keys) {
        if self.floats() && !matches!(keys, Keys::Menu { .. }) {
            self.close_menu();
            if let Some(said) = self.said.take() {
                self.notice = Some(said);
            }
        }
        let (had, holds) = (self.keys.held(), keys.held());
        self.keys = keys;
        if had == holds {
            return;
        }
        self.pending = Pending::Nothing;
        if holds {
            if self.here_row.is_some() {
                self.cursor.clone_from(&self.here_row);
            }
        } else {
            self.search = None;
            self.moving = Moving::Still;
            self.close_menu();
        }
        self.rebuild();
    }

    pub fn close_menu(&mut self) {
        let menu = self.menu.take();
        self.keep_working(menu);
    }

    pub fn drop_menu(&mut self, menu: u64) {
        self.menu.take_if(|open| open.number() == menu);
    }

    fn working_on(&self) -> bool {
        self.working.values().any(Menu::turning)
    }

    fn opened_said(place: &Place) -> String {
        let session = place.session();
        match place.pane() {
            Some(pane) => format!("menu: opened {session}:{pane}"),
            None => format!("menu: opened {session}"),
        }
    }

    fn went_said(place: &Place) -> String {
        let session = place.session();
        match place.pane() {
            Some(pane) => format!("menu: {session}:{pane} has it; nobody went"),
            None => format!("menu: {session} has it; nobody went"),
        }
    }

    fn keep_working(&mut self, menu: Option<Menu>) {
        if let Some(menu) = menu.filter(Menu::waits) {
            self.working.insert(menu.number(), menu);
        }
    }

    fn take_waited(&mut self, menu: u64, keep: impl Fn(&Menu) -> bool) -> Option<(Menu, Held)> {
        let (waited, held, ()) = self.take_seen(menu, |it| keep(it).then_some(()))?;
        Some((waited, held))
    }

    fn take_seen<T>(
        &mut self,
        menu: u64,
        see: impl Fn(&Menu) -> Option<T>,
    ) -> Option<(Menu, Held, T)> {
        let open = self.menu.as_ref().filter(|it| it.number() == menu);
        if let Some(seen) = open.and_then(&see) {
            return self.menu.take().map(|it| (it, Held::Open, seen));
        }
        let seen = self.working.get(&menu).and_then(see)?;
        self.working.remove(&menu).map(|it| (it, Held::Kept, seen))
    }

    pub fn attaching(&mut self, on: Machine, session: OwnKey) {
        self.seat = Some(Sitting { on, own: OwnSeat::Coming(session), shown: None });
        self.unfold_own(session);
        self.rebuild();
    }

    pub fn unseat(&mut self) {
        if self.seat.take().is_some() {
            self.rebuild();
        }
        assert!(self.seat.is_none(), "luneta sits nowhere when it shows no worktree");
    }

    pub fn went_own(&mut self, pane: OwnPane, client: OwnClientId) {
        let seat = self.seat.as_mut().expect("only a client of an own session goes among them");
        let was = seat.own.session();
        seat.own = OwnSeat::On { pane, client };
        if was != pane.session {
            self.unfold_own(pane.session);
        }
        if let Some(place) = self.own_place(pane) {
            let on = self.on().clone();
            let spot = Spot::on(&on, Place::Pane(place));
            if let Some(Place::Worktree(home)) = self.homes.get(&spot).and_then(Spot::place) {
                self.folds.set(Fold::Worktree(&on, home.worktree), false);
            }
        }
        self.rebuild();
    }

    pub fn showing(&mut self, worktree: WorktreeId) {
        let seat = self.seat.as_mut().expect("luneta shows a worktree where it sits");
        if seat.shown != Some(worktree) {
            seat.shown = Some(worktree);
            self.folds.set(Fold::Worktree(&seat.on, worktree), false);
            self.rebuild();
        }
    }

    pub fn own_seat(&self) -> Option<OwnSeat> {
        self.seat.as_ref().map(|seat| seat.own)
    }

    pub fn own_seat_on(&self, on: &Machine) -> Option<OwnSeat> {
        self.seat.as_ref().filter(|seat| seat.on == *on).map(|seat| seat.own)
    }

    pub fn own_title(&self) -> Option<&str> {
        self.seat_name.as_deref()
    }

    pub fn own_listed(&self, key: OwnKey) -> Option<Listed> {
        Some(self.own_known(key)?.listed().clone())
    }

    fn own_known(&self, key: OwnKey) -> Option<&Known> {
        let mut known = shelf(&self.shelves, self.on()).iter();
        known.find(|known| known.listed().own_key() == Some(key))
    }

    fn own_place(&self, pane: OwnPane) -> Option<PanePlace> {
        let known = self.own_known(pane.session)?;
        PanePlace::of(known.listed().clone(), known.pane(pane.id.get())?)
    }

    fn here_worktree(&self) -> Option<WorktreePlace> {
        let known = self.own_known(self.own_seat()?.session())?;
        let shown = self.seat.as_ref()?.shown?;
        let worktree = known.worktrees.iter().find(|worktree| worktree.place.worktree == shown)?;
        Some(worktree.place.clone())
    }

    fn unfold_own(&mut self, key: OwnKey) {
        if let Some(project) = self.own_listed(key).as_ref().and_then(Listed::project) {
            let on = self.on().clone();
            self.folds.set(Fold::Machine(&on), false);
            self.folds.set(Fold::Project(&on, project), false);
        }
    }

    fn seats_here(own_seat: Option<OwnSeat>, listed: &Listed) -> bool {
        own_seat.is_some_and(|seat| listed.own_key() == Some(seat.session()))
    }

    pub fn on(&self) -> &Machine {
        self.seat.as_ref().map_or(&Machine::Local, |seat| &seat.on)
    }

    pub fn user(&self, machine: &Machine) -> User {
        if self.on() != machine {
            return User::Away;
        }
        User::Untold
    }

    fn here_place(&self) -> Result<PanePlace, &'static str> {
        let seat = self.own_seat().ok_or(NO_HERE_PANE)?;
        self.own_place(seat.on().ok_or(NO_HERE_PANE)?).ok_or(NO_HERE_PANE)
    }

    pub fn here_agent(&self) -> Option<HereAgent> {
        let place = self.here_place().ok()?;
        let seen = self.seen(self.on())?;
        let seated = seen.seated();
        let agent = seated.get(&(place.session.at(), place.pane.id()))?;
        let pane = self.pane_of(self.on(), &place.session, place.pane.id())?;
        let harness = agents::of(agent).filter(|_| agent.at_a_prompt())?;
        Some(HereAgent {
            on: self.on().clone(),
            session: place.session.name().clone(),
            pane: place.pane.id(),
            dir: agent_dir(agent, pane).to_owned(),
            harness,
        })
    }

    pub fn on_header(&self) -> Option<Machine> {
        match self.cursor.as_ref()? {
            Spot::Machine(machine) => Some(machine.clone()),
            Spot::Local(_) | Spot::Remote(..) => None,
        }
    }

    pub fn listing(&self) -> Listing {
        let here = self.whereabouts();
        let (away, this) = (self.away.clone(), self.this.clone());
        let mut listing = Listing { away, this, ..Listing::default() };
        if let Some(host) = &self.host {
            for (machine, seen) in machines(host, &self.shelves).each() {
                listing.shelves.push(self.shelved(&machine, seen.as_ref(), here.as_ref()));
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

    fn shelved(
        &self,
        machine: &Machine,
        seen: Result<&Seen<'_>, &Away>,
        here: Option<&Here>,
    ) -> Shelved {
        let here = here.filter(|here| here.on() == machine);
        let here_key = here.and_then(Here::pane).map(|place| place.session.key(place.pane.id()));
        let heard = seen.ok();
        let seated = heard.map_or_default(|seen| seen.seated());
        let unread = heard.filter(|_| self.away.is_none()).map(|seen| seen.unread_seats());
        let statuses: HashMap<&str, &lunetad::api::GitStatus> = heard
            .iter()
            .flat_map(|seen| seen.git_statuses)
            .map(|status| (status.dir.as_str(), status))
            .collect();
        let mut projects = Vec::new();
        for known in shelf(&self.shelves, machine) {
            let listed = known.listed();
            let mut worktrees = Vec::with_capacity(known.worktrees.len());
            for worktree in &known.worktrees {
                let held = |pane: u32| worktree.panes.iter().any(|it| it.id == pane);
                let agents = worktree.panes.iter().filter_map(|pane| {
                    Some((listed.key(pane.id), *seated.get(&(listed.at(), pane.id))?))
                });
                let waiting = agents.clone().filter(|(key, agent)| {
                    let (marked, _) = agent_marked(&self.news, agent, *key, here_key == Some(*key));
                    marked.mark == Mark::Waiting
                });
                let ending = match &unread {
                    Some(Ok(seats)) => {
                        let doing = agents.clone().map(|(_, agent)| close::Doing::from(agent));
                        let files = close::unread_in(seats, listed.at());
                        let files = files.filter(|(pane, _)| held(*pane)).map(|(_, doing)| doing);
                        Ok(doing.chain(files).collect())
                    },
                    Some(Err(unplaced)) => Err(Untold::Unplaced(*unplaced)),
                    None => Err(Untold::Away),
                };
                let branch = statuses.get(worktree.folder.as_str()).and_then(|status| {
                    Some(checkout(&GitStatus::of_api(status)?, None)?.branch)
                });
                let terminals = worktree.panes.iter().filter_map(|pane| {
                    PanePlace::of(listed.clone(), pane).map(|place| place.pane)
                });
                worktrees.push(Worktree {
                    place: worktree.place.clone(),
                    name: worktree.name.clone(),
                    folder: worktree.folder.clone(),
                    branch,
                    seen: worktree.seen,
                    used: worktree.used,
                    terminals: terminals.collect(),
                    held: worktree.terminals.clone(),
                    waiting: waiting.count(),
                    ending,
                });
            }
            let (project, dir) = (known.project.clone(), known.dir.clone());
            let unopened = known.unopened.clone();
            projects.push(Housed { project, dir, worktrees, unopened });
        }
        let away = seen.err().map(|away| away.said().0);
        Shelved { on: machine.clone(), away, projects }
    }

    pub fn reaches(&self) -> Option<Vec<(Machine, Reach)>> {
        let host = self.host.as_ref()?;
        let here = self.whereabouts();
        let reach = |machine: &Machine, seen: Result<Seen<'_>, Away>| {
            let Ok(seen) = seen else {
                return Reach::Away;
            };
            let agents = self.runs(machine, &seen, here.as_ref()).agents;
            let listed = seen.projects.iter().map(|known| known.listed().clone()).collect();
            Reach::Heard { agents, listed, unlisted: Vec::new() }
        };
        let each = machines(host, &self.shelves).each();
        Some(each.map(|(machine, seen)| (machine.clone(), reach(&machine, seen))).collect())
    }

    pub fn whereabouts(&self) -> Option<Here> {
        let seat = self.own_seat()?;
        let on = self.on().clone();
        if let Ok(place) = self.here_place() {
            return Some(Here::Pane { on, place });
        }
        let session = self.own_listed(seat.session())?.name.to_string();
        Some(Here::Session { on, session })
    }

    pub fn far_agent(&self) -> Option<&Destination> {
        let Machine::Remote(on) = self.on() else {
            return None;
        };
        let Some(Here::Pane { place, .. }) = self.whereabouts() else {
            return None;
        };
        let seen = self.seen(self.on())?;
        let seat = (place.session.at(), place.pane.id());
        seen.seated().contains_key(&seat).then_some(on)
    }

    fn runs(&self, machine: &Machine, seen: &Seen<'_>, here: Option<&Here>) -> Runs {
        let here = here.filter(|here| here.on() == machine);
        let here_key = here.and_then(Here::pane).map(|place| place.session.key(place.pane.id()));
        let seated = seen.seated();
        let mut runs = Runs::default();
        for known in seen.projects.iter().rev() {
            let listed = known.listed();
            for pane in known.panes() {
                let Some(agent) = seated.get(&(listed.at(), pane.id)) else {
                    continue;
                };
                let Some(place) = PanePlace::of(listed.clone(), pane) else {
                    continue;
                };
                let key = listed.key(pane.id);
                let (marked, _) = agent_marked(&self.news, agent, key, here_key == Some(key));
                runs.agents.push(Seated {
                    place,
                    label: label(Some(agent), pane),
                    mark: marked.mark,
                    status: status_word(agent),
                    since: marked.since,
                    dir: short_cwd(agent_dir(agent, pane)),
                    pid: agent.pid,
                    transcript: transcript(agent).named(),
                    doing: alerts::Doing::of(agent),
                });
            }
        }
        let here = here.map(Here::session);
        runs.agents.sort_by_key(|seated| here != Some(seated.place.session.name().as_str()));
        runs
    }

    pub fn sayings(&self) -> Option<Vec<(Machine, Option<Vec<Saying>>)>> {
        let each = machines(self.host.as_ref()?, &self.shelves).each();
        Some(each.map(|(on, seen)| (on, seen.ok().map(|seen| seen.sayings.to_vec()))).collect())
    }

    pub fn aloud(&mut self, shown: Option<aloud::Shown>) {
        if self.aloud != shown {
            self.aloud = shown;
            self.rebuild();
        }
    }

    pub fn aloud_buttons(&self) -> &Spots<aloud::Button> {
        &self.aloud_buttons
    }

    pub fn notify(&mut self, notice: String) {
        log::say(&notice);
        self.notice = Some(notice);
    }

    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    pub fn unsay(&mut self, notice: &str) {
        if self.notice.as_deref() == Some(notice) {
            self.notice = None;
        }
    }

    pub fn filing(&mut self, number: u64, repository: &str) {
        self.filing.insert(number, repository.to_owned());
    }

    pub fn filed(&mut self, number: u64, repository: &str, filed: &Result<Filed, Unfiled>) {
        self.filing.remove(&number);
        match filed {
            Ok(filed) => {
                log::say(&format!("issue: filed {}", filed.link().url()));
                let (number, title) = (filed.number(), filed.title());
                self.filed = Some(format!("filed #{number} in {repository}: {title}; link copied"));
            },
            Err(Unfiled::Refused(why)) => {
                self.notify(format!("issue in {repository}: {why}; i has your words again"));
            },
            Err(Unfiled::InDoubt(why)) => {
                let doubt = format!("issue in {repository} may be filed: {why}");
                self.notify(format!("{doubt}; i has your words, look first"));
            },
        }
    }

    pub fn image_going(&mut self, to: Option<Destination>) {
        self.image_going = to;
    }

    fn say(&mut self, notice: String) {
        log::say(&notice);
        if self.floats() {
            self.said = Some(notice);
        } else {
            self.notice = Some(notice);
        }
    }

    pub fn said(&mut self) -> Option<String> {
        self.said.take()
    }

    pub fn turning(&self) -> bool {
        let menus = self.menu.as_ref().is_some_and(Menu::turning);
        self.counts.busy > 0 || menus || self.working_on() || !self.filing.is_empty()
    }

    fn rebuild(&mut self) {
        let seat_listed = self.own_seat().and_then(|seat| self.own_listed(seat.session()));
        if let Some(listed) = seat_listed {
            self.seat_name = Some(listed.name.to_string());
        }
        let here_pane = self.here_place().ok();
        let here_worktree = self.here_worktree();
        let aloud = self.aloud.as_ref().and_then(aloud::Shown::from);
        let Some(host) = &self.host else {
            return;
        };
        let (own_seat, sits_on) = (self.own_seat(), self.on().clone());
        let now = self.drawn.unwrap_or(SystemTime::UNIX_EPOCH);
        let mut due: Option<SystemTime> = None;
        let listed = machines(host, &self.shelves);
        let unlisted_machines = listed.left_out;
        let unlisted = self.shelves.iter().map(|shelf| shelf.unlisted).sum();
        let (term, seek) = match &mut self.search {
            Some(Search::Typed { term, seek }) => (Some(term.as_str()), std::mem::take(seek)),
            Some(Search::Empty) | None => (None, false),
        };
        let finder = term.map(Finder::new);
        let here_key = here_pane.as_ref().map(|place| place.session.key(place.pane.id()));
        let mut finished: HashMap<Key, SystemTime> = HashMap::new();
        let mut counts = Counts::default();
        let mut rows = Vec::new();
        let mut homes = HashMap::new();
        let mut best: Option<(usize, Fit)> = None;
        let mut weigh = |at: usize, fit: Fit| {
            if best.is_none_or(|(_, top)| fit > top) {
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
            if projects.is_empty() {
                if finder.is_none() {
                    rows.push(Row::Machine { machine, name, away, heads: Heads::Nothing });
                }
                continue;
            }
            let machine_folded = finder.is_none() && self.folds.contains(Fold::Machine(&machine));
            let machine_at = rows.len();
            let heads = Heads::Nothing;
            rows.push(Row::Machine { machine: machine.clone(), name, away, heads });
            let (mut machine_tally, mut machine_here) = (Tally::default(), false);
            let agents = heard.as_ref().map_or_default(Seen::seated);
            let statuses: HashMap<&str, &lunetad::api::GitStatus> = heard
                .iter()
                .flat_map(|heard| heard.git_statuses)
                .map(|status| (status.dir.as_str(), status))
                .collect();
            let pull_requests = pull_requests(heard.as_ref().map_or(&[], |it| it.pull_requests));
            counts.projects += projects.len();
            for known in projects {
                let listed = known.listed();
                let project = known.project.id();
                let folded = self.folds.contains(Fold::Project(&machine, project));
                let colour = colour_of(project);
                let agent = |pane: &Pane| agents.get(&(listed.at(), pane.id)).copied();
                let mut worktrees = Vec::with_capacity(known.worktrees.len());
                for worktree in &known.worktrees {
                    let mut by_pane: Vec<Shows> = Vec::with_capacity(worktree.panes.len());
                    for pane in &worktree.panes {
                        counts.panes += 1;
                        let key = listed.key(pane.id);
                        let here = on && here_key == Some(key);
                        let unread = self.news.is_unread(key);
                        let place = PanePlace::of(listed.clone(), pane);
                        let home = Place::Worktree(worktree.place.clone());
                        if let Some(place) = place {
                            let pane = Spot::on(&machine, Place::Pane(place));
                            homes.insert(pane, Spot::on(&machine, home));
                        }
                        if let Some(agent) = agent(pane) {
                            let (agent, done) = agent_marked(&self.news, agent, key, here);
                            if let Some(done) = done {
                                finished.insert(key, done);
                            }
                            by_pane.push(Shows { marked: Some(agent), ran: None, unread });
                            continue;
                        }
                        let Some(shell) = &pane.shell else {
                            by_pane.push(Shows { marked: None, ran: None, unread });
                            continue;
                        };
                        if let Some(long) = long_at(shell).filter(|long| *long > now) {
                            due = Some(due.map_or(long, |due| due.min(long)));
                        }
                        let mut shell = Marked::of_shell(shell, now);
                        let ended = shell.as_ref().and_then(|(shell, _)| shell.ended());
                        if let Some(ended) = ended {
                            finished.insert(key, ended);
                            if here || !self.news.is_new(key, Some(ended)) {
                                shell = None;
                            }
                        }
                        let (marked, ran) = shell.unzip();
                        by_pane.push(Shows { marked, ran: ran.flatten(), unread });
                    }
                    let tally = Tally::of(&by_pane);
                    counts.waiting += tally.waiting;
                    counts.new += tally.dots;
                    counts.busy += tally.busy;
                    counts.shell += tally.shell;
                    worktrees.push((worktree, by_pane, tally));
                }
                let tally = Tally::sum(worktrees.iter().map(|(_, _, tally)| *tally));
                let project_here = on && Self::seats_here(own_seat, listed);
                machine_tally = Tally::sum([machine_tally, tally].into_iter());
                machine_here |= project_here;
                if machine_folded {
                    continue;
                }
                let (named, name_hits) = finder
                    .as_ref()
                    .and_then(|finder| finder.session(listed.name.as_str()))
                    .unzip();
                let project_at = rows.len();
                rows.push(Row::Project {
                    machine: machine.clone(),
                    project: known.project.clone(),
                    hits: name_hits.unwrap_or_default(),
                    under: Under::Empty,
                    here: project_here,
                    colour,
                    waiting: tally.waiting,
                    dot: tally.dots > 0,
                    busy: tally.busy > 0,
                });
                let mut lead: Option<(usize, Fit)> = None;
                let mut found_at = |at: usize, fit: Fit, weigh: &mut dyn FnMut(usize, Fit)| {
                    weigh(at, fit);
                    if lead.is_none_or(|(_, top)| fit > top) {
                        lead = Some((at, fit));
                    }
                };
                if finder.is_some() || !folded {
                    for (worktree, by_pane, tally) in worktrees {
                        let fold = Fold::Worktree(&machine, worktree.place.worktree);
                        let worktree_folded = finder.is_none() && self.folds.contains(fold);
                        let dir = worktree.folder.as_str();
                        let checkout = statuses.get(dir).and_then(|status| {
                            checkout(&GitStatus::of_api(status)?, pull_requests.get(dir))
                        });
                        let branch = checkout.as_ref().map(|checkout| checkout.branch.as_str());
                        let (worktree_found, worktree_hits) = finder
                            .as_ref()
                            .and_then(|finder| finder.worktree(&worktree.name, branch, dir))
                            .unzip();
                        let mut shown = Vec::with_capacity(worktree.panes.len());
                        let listing =
                            if worktree_folded { &[][..] } else { worktree.panes.as_slice() };
                        for (pane, Shows { marked, ran, unread }) in listing.iter().zip(by_pane) {
                            let cwd = pane.cwd.as_deref();
                            let command = pane.command.as_deref();
                            let program = agent(pane)
                                .and_then(|agent| icons::agent(agent.runs_in()))
                                .or_else(|| command.and_then(icons::program));
                            let label = match ran {
                                Some(ran) => ran.to_owned(),
                                None => label(agent(pane), pane),
                            };
                            let dir = agent(pane).map_or(cwd, |agent| Some(agent_dir(agent, pane)));
                            let (found, hits) = finder
                                .as_ref()
                                .and_then(|finder| finder.pane(&label, None, &pane.tab_name, dir))
                                .unzip();
                            if finder.is_some()
                                && found.is_none()
                                && worktree_found.is_none()
                                && named.is_none()
                            {
                                continue;
                            }
                            let Some(place) = PanePlace::of(listed.clone(), pane) else {
                                continue;
                            };
                            let here = on && here_pane.as_ref() == Some(&place);
                            let icon = program.and_then(icons::of);
                            let (machine, session) = (machine.clone(), place.session);
                            let id = place.pane;
                            let speaks = aloud.is_some_and(|(on, at, id)| {
                                *on == machine && at == session.at() && id == pane.id
                            });
                            let row = move |bracket| Row::Pane {
                                machine,
                                session,
                                pane: id,
                                here,
                                unread,
                                marked,
                                icon,
                                label,
                                hits: hits.unwrap_or_default(),
                                bracket,
                                speaks,
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
                        let worktree_at = rows.len();
                        let here = on && here_worktree.as_ref() == Some(&worktree.place);
                        rows.push(Row::Worktree {
                            machine: machine.clone(),
                            place: worktree.place.clone(),
                            name: worktree.name.clone(),
                            missing: worktree.seen == projects::Seen::Missing,
                            hits: worktree_hits.unwrap_or_default(),
                            under,
                            here,
                            colour,
                            waiting: tally.waiting,
                            dot: tally.dots > 0,
                            busy: tally.busy > 0,
                            checkout,
                        });
                        if let Some(found) = worktree_found {
                            found_at(worktree_at, found, &mut weigh);
                        }
                        let count = shown.len();
                        for (at, (row, found)) in shown.into_iter().enumerate() {
                            if let Some(found) = found {
                                found_at(rows.len(), found, &mut weigh);
                            }
                            let bracket = Bracket::of(at, count).map(|sits| (sits, colour));
                            rows.push(row(bracket));
                        }
                    }
                }
                if finder.is_some() && named.is_none() && rows.len() == project_at + 1 {
                    rows.truncate(project_at);
                    continue;
                }
                if let Some(named) = named {
                    weigh(lead.map_or(project_at, |(at, _)| at), named);
                }
                let under = if rows.len() > project_at + 1 {
                    Under::Rows
                } else if folded {
                    Under::Folded
                } else {
                    Under::Empty
                };
                if let Row::Project { under: was, .. } = &mut rows[project_at] {
                    *was = under;
                }
            }
            if finder.is_some() && rows.len() == machine_at + 1 {
                rows.truncate(machine_at);
                continue;
            }
            let Row::Machine { heads, .. } = &mut rows[machine_at] else {
                unreachable!("row {machine_at} is this machine's");
            };
            assert_eq!(*heads, Heads::Nothing, "a machine's heads are set once");
            *heads = Heads::Projects {
                folded: machine_folded,
                here: machine_here,
                waiting: machine_tally.waiting,
                dot: machine_tally.dots > 0,
                busy: machine_tally.busy > 0,
            };
        }
        if finder.is_none() {
            assert!(!rows.is_empty(), "this machine always has a row");
        }
        self.rows = rows;
        self.homes = homes;
        self.counts = counts;
        self.unlisted = unlisted;
        self.unlisted_machines = unlisted_machines;
        self.here_key = here_key;
        self.finished = finished;
        let here_spot = here_pane.map(|place| Spot::on(self.on(), Place::Pane(place)));
        self.here_row = here_spot.and_then(|spot| self.row_for(&spot));
        let gone = |spot: Spot| self.row_of(&spot).is_none();
        match &self.moving {
            Moving::Selected(movable) | Moving::Lifted(Lift { lifted: movable, .. })
                if gone(movable.spot()) =>
            {
                self.moving = Moving::Still;
            },
            _ => {},
        }
        if let Some(closing) = &self.closing {
            let gone = &closing.gone;
            if self.cursor.as_ref() == Some(gone) && self.rows.iter().all(|row| row.spot() != *gone)
            {
                self.cursor.clone_from(&closing.after);
                self.closing = None;
            }
        }
        self.due = due;
        self.settle(best.map(|(at, _)| at), seek);
    }

    fn row_for(&self, spot: &Spot) -> Option<Spot> {
        let rows = [
            Some(spot.clone()),
            self.worktree_of(spot),
            project_of(spot),
            Some(Spot::Machine(spot.machine())),
        ];
        let listed = |spot: &Spot| self.rows.iter().any(|row| row.spot() == *spot);
        rows.into_iter().flatten().find(listed)
    }

    fn worktree_of(&self, spot: &Spot) -> Option<Spot> {
        match spot.place()? {
            Place::Session(_) => None,
            Place::Worktree(_) => Some(spot.clone()),
            Place::Pane(_) => self.homes.get(spot).cloned(),
        }
    }


    fn folding(&self, spot: &Spot) -> Option<Folding> {
        match spot.place() {
            None => Some(Folding::Machine),
            Some(Place::Session(session)) => Some(Folding::Project(session.project()?)),
            Some(Place::Worktree(place)) => Some(Folding::Worktree(place.worktree)),
            Some(Place::Pane(_)) => match self.homes.get(spot)?.place()? {
                Place::Worktree(place) => Some(Folding::Worktree(place.worktree)),
                Place::Session(_) | Place::Pane(_) => None,
            },
        }
    }

    fn folded(&self, machine: &Machine, folding: Folding) -> bool {
        match folding {
            Folding::Machine => self.folds.contains(Fold::Machine(machine)),
            Folding::Project(project) => self.folds.contains(Fold::Project(machine, project)),
            Folding::Worktree(worktree) => {
                self.folds.contains(Fold::Worktree(machine, worktree))
            },
        }
    }

    fn row_of(&self, spot: &Spot) -> Option<usize> {
        self.rows.iter().position(|row| row.spot() == *spot)
    }

    fn machine_row(&self, machine: &Machine) -> (&str, Option<&Away>, Heads) {
        let found = self.rows.iter().find_map(|row| match row {
            Row::Machine { machine: of, name, away, heads } => {
                (of == machine).then_some((name.as_str(), away.as_ref(), *heads))
            },
            Row::Project { .. } | Row::Worktree { .. } | Row::Pane { .. } => None,
        });
        found.expect("a machine's rows sit under its own")
    }

    fn home_row(&self, at: usize) -> Option<usize> {
        let row = &self.rows[at];
        if !matches!(row, Row::Pane { .. }) {
            return None;
        }
        let home = self.row_of(self.homes.get(&row.spot())?)?;
        assert!(home < at, "a worktree's row sits above its panes");
        Some(home)
    }

    fn settle(&mut self, best: Option<usize>, seek: bool) {
        let held = self.cursor.as_ref().and_then(|spot| self.row_of(&self.row_for(spot)?));
        let at = if seek { best.or(held) } else { held.or(best) };
        let at = at.unwrap_or(0);
        let stop = (at..self.rows.len()).chain((0..at).rev()).find(|index| self.stops(*index));
        self.cursor = stop.map(|index| self.rows[index].spot());
    }

    fn stops(&self, at: usize) -> bool {
        let row = &self.rows[at];
        match &self.moving {
            Moving::Selected(Movable::Project(of, _)) => {
                matches!(row, Row::Project { machine, .. } if machine == of)
            },
            Moving::Selected(Movable::Worktree(of, held)) => matches!(
                row,
                Row::Worktree { machine, place, .. }
                    if machine == of && place.project == held.project
            ),
            Moving::Still
            | Moving::Down { .. }
            | Moving::Lifted(_)
            | Moving::Sweeping(_)
            | Moving::Swept(..) => row.stops(),
        }
    }

    fn cursor_index(&self) -> Option<usize> {
        let spot = self.cursor.as_ref()?;
        Some(self.row_of(spot).expect("the highlight is on a row"))
    }

    fn step(&mut self, by: isize) {
        self.closing = None;
        let stops: Vec<usize> = (0..self.rows.len()).filter(|at| self.stops(*at)).collect();
        let Some(last) = stops.len().checked_sub(1) else {
            return;
        };
        let now = self.cursor_index().and_then(|at| stops.iter().position(|stop| *stop == at));
        let next = match now {
            Some(now) => now.saturating_add_signed(by).min(last),
            None => 0,
        };
        self.cursor = Some(self.rows[stops[next]].spot());
    }

    fn end(&mut self, last: bool) {
        self.closing = None;
        let mut stops = self.rows.iter().filter(|row| row.stops());
        let stop = if last { stops.next_back() } else { stops.next() };
        if let Some(stop) = stop {
            self.cursor = Some(stop.spot());
        }
    }

    fn toggle(&mut self) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let Some(folding) = self.folding(&spot) else {
            return;
        };
        let folded = self.folded(&spot.machine(), folding);
        self.set_fold(&spot, folding, !folded);
    }

    fn fold(&mut self, folded: bool) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let Some(folding) = self.folding(&spot) else {
            return;
        };
        let lifted = match self.row_of(&spot).map(|at| &self.rows[at]) {
            Some(Row::Worktree { under, .. }) if folded && *under != Under::Rows => {
                spot.place().and_then(|place| place.listed().project()).map(Folding::Project)
            },
            Some(Row::Project { under, .. }) if folded && *under != Under::Rows => {
                Some(Folding::Machine)
            },
            _ => None,
        };
        self.set_fold(&spot, lifted.unwrap_or(folding), folded);
    }

    fn set_fold(&mut self, spot: &Spot, folding: Folding, folded: bool) {
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
            Folding::Worktree(worktree) => {
                (Fold::Worktree(&machine, worktree), self.worktree_of(spot))
            },
        };
        if !self.folds.set(fold, folded) {
            return;
        }
        if folded {
            self.cursor = row;
        }
        self.rebuild();
    }

    pub fn key(&mut self, key: &KeyEvent, now: SystemTime) -> Asked {
        self.key_from(key, now, Came::Typed)
    }

    fn key_from(&mut self, key: &KeyEvent, now: SystemTime, came: Came) -> Asked {
        if self.pressed() {
            if key.code == KeyCode::Esc {
                self.let_go();
            }
            return Asked::Nothing;
        }
        self.notice = None;
        self.copied = None;
        self.filed = None;
        let held = self.menu.as_ref().is_some_and(|menu| self.menu_paused(menu, now));
        self.holds = Holds::Nothing;
        let pending = std::mem::take(&mut self.pending);
        let shows = match (came, &pending) {
            (Came::Read, _) | (_, Pending::Legend) => now,
            (Came::Typed, _) => now + PAUSE,
        };
        if menu::closes(key) && self.menu.as_ref().is_some_and(Menu::waits) {
            self.close_menu();
            return Asked::Nothing;
        }
        if key.code == KeyCode::Char(menu::ALL_KEYS)
            && self.menu.as_ref().is_some_and(Menu::lists)
            && !self.floats()
        {
            self.close_menu();
            self.pending = Pending::AllKeys(AllKeys::default());
            return Asked::Nothing;
        }
        if let Some(menu) = self.menu.take() {
            let asked = self.menu_did(menu.key(key));
            if let Some(menu) = self.menu.as_ref().filter(|menu| held && menu.quiet()) {
                self.holds = Holds::Quiet(menu.number());
            }
            return asked;
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return Asked::Nothing;
        }
        if let Some(term) = self.search() {
            let term = term.to_owned();
            return self.search_key(key, term);
        }
        let pending = match pending {
            Pending::Legend => match key.code {
                KeyCode::Esc => return Asked::Nothing,
                KeyCode::Char(menu::ALL_KEYS) => {
                    self.pending = Pending::AllKeys(AllKeys::default());
                    return Asked::Nothing;
                },
                _ => Pending::Nothing,
            },
            Pending::AllKeys(all) => match self.all_keys_key(all, key) {
                Some(asked) => return asked,
                None => Pending::Nothing,
            },
            pending => pending,
        };
        if let Moving::Selected(_) = self.moving
            && let Some(asked) = self.selected_key(key)
        {
            return asked;
        }
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Home => self.end(false),
            KeyCode::End => self.end(true),
            KeyCode::Left => self.fold(true),
            KeyCode::Right => self.fold(false),
            KeyCode::Enter => return self.go(),
            KeyCode::Esc if pending.shows().is_some() => {},
            KeyCode::Esc => return Asked::Leave,
            KeyCode::Char(c)
                if let Pending::Group { key, .. } = pending
                    && let Some(action) = menu::in_group(key, c) =>
            {
                return self.row_action(action, now);
            },
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Last) => self.end(true),
                Some(Top::First) if matches!(pending, Pending::First { .. }) => self.end(false),
                Some(Top::First) => self.pending = Pending::First { shows },
                Some(Top::Group(key)) => self.pending = Pending::Group { key, shows },
                Some(Top::AllKeys) => self.pending = Pending::Legend,
                Some(Top::Fold(Some(folded))) => self.fold(folded),
                Some(Top::Fold(None)) => self.toggle(),
                Some(Top::Menu) => return self.open_menu(None),
                Some(Top::Command(Action::Search)) => self.find(String::new()),
                Some(Top::Command(Action::Unread)) => self.toggle_unread(now),
                Some(Top::Command(Action::Reorder)) => self.select(),
                Some(Top::Command(_) | Top::Shift(_)) => {},
                None => {
                    let asked = self.open_menu(Some(c));
                    if let Some(menu) = &self.menu {
                        self.holds = Holds::Until(menu.number(), shows);
                    }
                    return asked;
                },
            },
            _ => {},
        }
        Asked::Nothing
    }

    fn menu_paused(&self, menu: &Menu, now: SystemTime) -> bool {
        match self.holds {
            Holds::Nothing => false,
            Holds::Until(number, shows) => menu.number() == number && now < shows,
            Holds::Quiet(number) => menu.number() == number && menu.quiet(),
        }
    }

    pub fn due(&self, now: SystemTime) -> Option<Duration> {
        let menu = match (self.holds, &self.menu) {
            (Holds::Until(number, shows), Some(menu)) if menu.number() == number => Some(shows),
            _ => None,
        };
        let shows = self.pending.shows().into_iter().chain(menu).filter(|at| now < *at).min()?;
        shows.duration_since(now).ok()
    }

    fn all_keys_key(&mut self, mut all: AllKeys, key: &KeyEvent) -> Option<Asked> {
        let by = match key.code {
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::Esc | KeyCode::Char(menu::ALL_KEYS) => return Some(Asked::Nothing),
            _ => return None,
        };
        all.scroll(by);
        self.pending = Pending::AllKeys(all);
        Some(Asked::Nothing)
    }

    fn highlighted_link(&self) -> Result<Link, String> {
        let row = self.cursor.as_ref().and_then(|spot| self.row_of(spot));
        let checked_out = |row: &Row| match row {
            Row::Worktree { checkout: Some(checkout), .. } => Some(checkout.clone()),
            Row::Pane { .. } => self.row_of(&self.worktree_of(&row.spot())?).and_then(|at| {
                let Row::Worktree { checkout, .. } = &self.rows[at] else {
                    return None;
                };
                checkout.clone()
            }),
            Row::Machine { .. } | Row::Project { .. } | Row::Worktree { .. } => None,
        };
        let checkout = row.and_then(|at| checked_out(&self.rows[at]));
        let Some(Checkout { pull_request: Some(numbered), .. }) = checkout else {
            return Err("no pull request on this row".to_owned());
        };
        numbered.link().cloned().ok_or_else(|| {
            format!("{}: lunetad says no link of it on github.com", numbered.shown())
        })
    }

    fn yank(&mut self, now: SystemTime) -> Asked {
        match self.highlighted_link() {
            Ok(link) => self.copy(link, now),
            Err(why) => {
                self.notify(why);
                Asked::Nothing
            },
        }
    }

    fn copy(&mut self, link: Link, now: SystemTime) -> Asked {
        self.copied = Some((link.number(), now));
        Asked::Copy(link)
    }

    fn row_action(&mut self, action: Action, now: SystemTime) -> Asked {
        match action {
            Action::YankPull => self.yank(now),
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
            },
        }
    }

    pub fn number_clicked(&mut self, link: Link, now: SystemTime) -> Asked {
        self.notice = None;
        self.pending = Pending::Nothing;
        self.copy(link, now)
    }

    fn selected_key(&mut self, key: &KeyEvent) -> Option<Asked> {
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Esc => self.unselect(),
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Shift(by)) => return Some(self.shift(by)),
                Some(Top::Command(Action::Reorder)) => self.widen(),
                _ => {
                    self.unselect();
                    return None;
                },
            },
            _ => {
                self.unselect();
                return None;
            },
        }
        Some(Asked::Nothing)
    }

    fn select(&mut self) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let Some(movable) = Movable::of(self.worktree_of(&spot).unwrap_or(spot)) else {
            return;
        };
        self.cursor = Some(movable.spot());
        self.moving = Moving::Selected(movable);
    }

    fn widen(&mut self) {
        let Moving::Selected(Movable::Worktree(machine, place)) = &self.moving else {
            self.unselect();
            return;
        };
        let project = Movable::Project(machine.clone(), place.project.clone());
        self.cursor = Some(project.spot());
        self.moving = Moving::Selected(project);
    }

    fn unselect(&mut self) {
        if let Moving::Selected(_) = self.moving {
            self.moving = Moving::Still;
            self.settle(None, false);
        }
    }

    fn shift(&mut self, by: isize) -> Asked {
        let Some((spans, held)) = self.selected() else {
            return Asked::Nothing;
        };
        let to = held.start.saturating_add_signed(by).min(spans.len() - held.len());
        if to == held.start {
            return Asked::Nothing;
        }
        self.keep(&spans.moved(held, to))
    }

    fn selected(&self) -> Option<(Spans, Range<usize>)> {
        let Moving::Selected(anchor) = &self.moving else {
            return None;
        };
        let spans = self.spans(anchor);
        let rows = spans.rows();
        let index = |at: usize| rows.iter().position(|span| span.contains(&at));
        let (from, to) = (index(self.row_of(&anchor.spot())?)?, index(self.cursor_index()?)?);
        Some((spans, from.min(to)..from.max(to) + 1))
    }

    fn spans(&self, movable: &Movable) -> Spans {
        match movable {
            Movable::Project(machine, _) => {
                let starts = self.rows.iter().enumerate().filter_map(|(at, row)| match row {
                    Row::Project { machine: of, project, .. } if of == machine => {
                        Some((at, project.id()))
                    },
                    _ => None,
                });
                let starts: Vec<(usize, ProjectId)> = starts.collect();
                let end = |start: usize| {
                    let after = self.rows.iter().enumerate().skip(start + 1);
                    let mut after = after.filter(|(_, row)| match row {
                        Row::Machine { .. } | Row::Project { .. } => true,
                        Row::Worktree { .. } | Row::Pane { .. } => false,
                    });
                    after.next().map_or(self.rows.len(), |(at, _)| at)
                };
                let spans = starts.into_iter().map(|(start, id)| (id, start..end(start)));
                Spans::Projects(machine.clone(), spans.collect())
            },
            Movable::Worktree(machine, held) => {
                let project = Spot::on(machine, Place::Session(held.project.listed().clone()));
                let first = self.row_of(&project).map_or(self.rows.len(), |at| at + 1);
                let rows = self.rows.iter().enumerate().skip(first);
                let rows = rows.take_while(|(_, row)| match row {
                    Row::Worktree { .. } | Row::Pane { .. } => true,
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
                let spans = starts.iter().zip(ends).map(|((start, id), end)| (*id, *start..end));
                Spans::Worktrees(machine.clone(), held.project.clone(), spans.collect())
            },
        }
    }

    fn keep(&mut self, spans: &Spans) -> Asked {
        let (on, order) = match spans {
            Spans::Projects(machine, spans) => {
                let kept = self.shelves.iter().find(|it| it.machine == *machine);
                let seen = kept.map_or(&[][..], |it| it.order.as_slice());
                let listed: Vec<ProjectId> = spans.iter().map(|(id, _)| *id).collect();
                let wanted = wanted(seen, &listed);
                if wanted == seen {
                    return Asked::Nothing;
                }
                (machine.clone(), Reorder::Projects { seen: seen.to_vec(), wanted })
            },
            Spans::Worktrees(machine, project, spans) => {
                let mut known = shelf(&self.shelves, machine).iter();
                let Some(known) = known.find(|known| known.project.id() == project.id()) else {
                    let name = project.listed().name();
                    self.notify(format!("{name} is no longer listed: nothing moved"));
                    return Asked::Nothing;
                };
                let listed: Vec<WorktreeId> = spans.iter().map(|(id, _)| *id).collect();
                let wanted = wanted(&known.order, &listed);
                if wanted == known.order {
                    return Asked::Nothing;
                }
                let (project, seen) = (project.id(), known.order.clone());
                (machine.clone(), Reorder::Worktrees { project, seen, wanted })
            },
        };
        let (seen, wanted) = (order.seen(), order.wanted());
        assert_ne!(seen, wanted, "a move changes the order");
        let on_its_way = self.unheard.iter().filter(|unheard| unheard.on == on);
        if on_its_way.map(|unheard| unheard.wait.asked()).sum::<usize>() >= MOVES_MAX {
            self.notify(format!("not moved: lunetad has not answered the last {MOVES_MAX} moves"));
            return Asked::Nothing;
        }
        let at = self.unheard.iter().position(|it| it.on == on && it.shown.of() == wanted.of());
        let kept = self.shelves.iter_mut().find(|shelf| shelf.machine == on);
        kept.expect("the machine moved on is shelved").reorder(&wanted);
        let ticket = if let Some(at) = at {
            let unheard = &mut self.unheard[at];
            assert_eq!(unheard.shown, seen, "the rows show the last order asked");
            unheard.shown = wanted;
            unheard.wait = unheard.wait.again();
            unheard.ticket
        } else {
            self.tickets += 1;
            let wait = Wait::Answers { asked: NonZeroUsize::MIN, landed: None };
            let ticket = self.tickets;
            self.unheard.push(Unheard { ticket, on: on.clone(), shown: wanted, wait });
            ticket
        };
        self.rebuild();
        Asked::Move { on, order, ticket }
    }

    fn landed(&self, spans: &[Range<usize>]) -> Vec<usize> {
        let start = spans.iter().map(|span| span.start).min().unwrap_or(0);
        let end = spans.iter().map(|span| span.end).max().unwrap_or(0);
        let moved = spans.iter().flat_map(Clone::clone);
        (0..start).chain(moved).chain(end..self.rows.len()).collect()
    }

    fn landing(&self, movable: &Movable, line: u16, over: bool) -> Option<(Spans, Range<usize>)> {
        let at = self.row_of(&movable.spot())?;
        let spans = self.spans(movable);
        let rows = spans.rows();
        let from = rows.iter().position(|span| span.contains(&at))?;
        let lifted = rows[from].clone();
        if !over {
            return Some((spans, lifted));
        }
        let lines = |span: Range<usize>| self.rows[span].iter().map(Row::height).sum::<usize>();
        let above = HEADER + lines(lifted.start..at);
        let top = (self.top + usize::from(line)) as isize - above as isize;
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
        self.menu_did(did)
    }

    pub fn agent(&self) -> agents::Harness {
        self.agent.harness()
    }

    fn keep_agent(&mut self, harness: agents::Harness) {
        if let Err(why) = self.agent.keep(harness) {
            let name = agents::name(harness);
            self.say(format!("agent: {name} is for this luneta alone: {why}"));
        }
    }

    fn agent_for(&self, target: &Target) -> agents::Picked {
        match self.harness_in(target) {
            Some(harness) => agents::Picked::ByPane(harness),
            None => agents::Picked::ByYou(self.agent.harness()),
        }
    }

    fn harness_in(&self, target: &Target) -> Option<agents::Harness> {
        let Target::Pane(machine, row) = target else {
            return None;
        };
        let seen = self.seen(machine)?;
        let shown = self.pane_of(machine, &row.session, row.pane.id())?;
        let seats = seen.seated();
        seats.get(&(row.session.at(), shown.id)).and_then(|agent| agents::of(agent))
    }

    fn target(&self) -> Option<Target> {
        let spot = self.cursor.clone()?;
        self.row_of(&spot)?;
        match spot {
            Spot::Machine(machine) => Some(Target::Machine(machine)),
            Spot::Local(place) => self.target_at(&Machine::Local, place),
            Spot::Remote(destination, place) => {
                self.target_at(&Machine::Remote(destination), place)
            },
        }
    }

    fn target_at(&self, machine: &Machine, place: Place) -> Option<Target> {
        let (session, pane) = match place {
            Place::Session(session) => return self.project_target(machine, &session),
            Place::Worktree(place) => return self.worktree_target(machine, place),
            Place::Pane(PanePlace { session, pane }) => (session, pane),
        };
        let seen = self.seen(machine);
        let shown = self.pane_of(machine, &session, pane.id());
        let agents = seen.as_ref().map(Seen::seated);
        let cwd = shown.and_then(|shown| {
            match agents.as_ref()?.get(&(session.at(), shown.id)) {
                Some(agent) => Some(agent_dir(agent, shown).to_owned()),
                None => shown.cwd.clone(),
            }
        });
        let statuses = seen.iter().flat_map(|seen| seen.git_statuses);
        let status =
            cwd.as_deref().and_then(|cwd| statuses.into_iter().find(|status| status.dir == cwd));
        let status = status.and_then(GitStatus::of_api);
        let checkout = status.as_ref().and_then(|status| checkout(status, None));
        let branch = checkout.map(|checkout| checkout.branch);
        let repository = status.as_ref().and_then(repository_of);
        let pulls = seen.iter().flat_map(|seen| seen.pull_requests);
        let pull_request =
            cwd.as_deref().and_then(|cwd| pulls.into_iter().find(|pull| pull.dir == cwd));
        let pull_request = pull_request.and_then(PullRequest::of_api);
        let ran = self.ran_in(machine, &session, pane.id());
        let terminal = self.terminal_of(machine, &session, pane);
        let row = PaneRow { session, pane, terminal, cwd, branch, repository, pull_request, ran };
        Some(Target::Pane(machine.clone(), row))
    }

    fn project_target(&self, machine: &Machine, listed: &Listed) -> Option<Target> {
        let mut known = shelf(&self.shelves, machine).iter();
        let known = known.find(|it| it.listed() == listed)?;
        let (project, dir) = (known.project.clone(), known.dir.as_str().to_owned());
        Some(Target::Project(machine.clone(), ProjectTarget { project, dir }))
    }

    fn terminal_of(&self, machine: &Machine, listed: &Listed, pane: PaneKey) -> Option<ViewId> {
        let PaneKey::Terminal(key) = pane else {
            return None;
        };
        let mut known = shelf(&self.shelves, machine).iter();
        let worktrees = &known.find(|it| it.listed() == listed)?.worktrees;
        let mut held = worktrees.iter().flat_map(|worktree| &worktree.in_panes);
        held.find(|(id, _)| *id == key.id.get()).map(|(_, terminal)| *terminal)
    }

    fn worktree_target(&self, machine: &Machine, place: WorktreePlace) -> Option<Target> {
        let mut known = shelf(&self.shelves, machine).iter();
        let known = known.find(|it| it.project.id() == place.project.id())?;
        let worktree = known.worktrees.iter().find(|worktree| worktree.place == place)?;
        let (name, folder) = (worktree.name.clone(), worktree.folder.as_str().to_owned());
        let seen = self.seen(machine);
        let statuses = seen.iter().flat_map(|seen| seen.git_statuses);
        let status = statuses.into_iter().find(|status| status.dir == folder);
        let status = status.and_then(GitStatus::of_api);
        let branch = status.as_ref().and_then(|status| checkout(status, None)).map(|it| it.branch);
        let repository = status.as_ref().and_then(repository_of);
        let row = WorktreeRow { place, name, folder, branch, repository };
        Some(Target::Worktree(machine.clone(), row))
    }

    fn ran_in(&self, machine: &Machine, session: &Listed, pane: u32) -> Ran {
        let shell = self.pane_of(machine, session, pane).and_then(|shown| shown.shell.as_ref());
        shell.map_or(Ran::Unhooked, ran)
    }

    fn reran(&mut self) {
        let Some(Target::Pane(machine, row)) = self.menu.as_ref().map(Menu::target) else {
            return;
        };
        let ran = self.ran_in(machine, &row.session, row.pane.id());
        if let Some(menu) = &mut self.menu {
            menu.reran(ran);
        }
    }

    pub fn float_menu(&mut self) -> Result<(), &'static str> {
        self.notice = None;
        let place = Place::Pane(self.here_place().inspect_err(|why| log::say(why))?);
        self.menus += 1;
        let target = self.target_at(self.on(), place).ok_or(NO_HERE_PANE)?;
        let agent = self.agent_for(&target);
        self.close_menu();
        self.menu = Some(Menu::new(self.menus, target, agent));
        Ok(())
    }

    pub fn paste(&mut self, text: &str) {
        if self.menu.as_mut().is_some_and(|menu| menu.paste(text)) {
            return;
        }
        let target = match &self.menu {
            Some(menu) => Some(menu.target().clone()),
            None => self.target(),
        };
        let from = target.as_ref().and_then(Target::cwd);
        let from = from.and_then(|from| Directory::parse(from.to_owned()).ok());
        let Some(work) = Work::parse(text, from) else {
            return;
        };
        let making = std::iter::once(self.menu.as_ref())
            .chain(self.working.values().map(Some))
            .flatten()
            .filter_map(Menu::making);
        if making.into_iter().any(|making| *making == work) {
            self.notice = Some(format!("{} is on its way", work.tab()));
            return;
        }
        self.notice = None;
        let Some(target) = target else {
            if self.cursor.is_none() {
                self.say(format!("menu: no row is highlighted for {work} to open the menu on"));
            }
            return;
        };
        self.close_menu();
        self.pending = Pending::Nothing;
        self.menus += 1;
        self.menu = Some(Menu::pull(self.menus, target, work, self.agent.harness()));
    }

    pub fn floating(&self) -> Option<&Menu> {
        self.menu.as_ref().filter(|_| self.floats())
    }

    pub fn holds_input(&self) -> bool {
        let chord = match self.pending {
            Pending::Nothing | Pending::Legend | Pending::AllKeys(_) => false,
            Pending::First { .. } | Pending::Group { .. } => true,
        };
        let moving = match &self.moving {
            Moving::Still | Moving::Swept(..) => false,
            Moving::Selected(_) | Moving::Down { .. } | Moving::Lifted(_) | Moving::Sweeping(_) => {
                true
            },
        };
        self.menu.is_some() || self.search.is_some() || chord || moving
    }

    pub fn chose(&mut self, menu: u64, choice: Choice, now: SystemTime) -> Asked {
        if self.menu.as_ref().is_none_or(|open| open.number() != menu) {
            return Asked::Nothing;
        }
        match choice {
            Choice::Key(key) => self.key(&key, now),
            Choice::Skill(name) => self.pick(&name),
        }
    }

    pub fn keys_clicked(&mut self, keyed: Keyed, key: &KeyEvent, now: SystemTime) -> Asked {
        let there = match self.bottom(now) {
            Bottom::Legend | Bottom::Group(_) | Bottom::Keys(..) | Bottom::AllKeys(_) => {
                Some(Keyed::Legend)
            },
            Bottom::Search(_) => Some(Keyed::Search),
            Bottom::Menu(_) | Bottom::Nothing => None,
        };
        if self.keys != Keys::Sidebar || there != Some(keyed) {
            return Asked::Nothing;
        }
        self.key_from(key, now, Came::Read)
    }

    fn pick(&mut self, name: &Name) -> Asked {
        self.notice = None;
        self.pending = Pending::Nothing;
        let Some(menu) = self.menu.take() else {
            return Asked::Nothing;
        };
        self.menu_did(menu.pick(name))
    }

    pub fn scroll_menu(&mut self, menu: u64, by: isize) {
        if let Some(open) = self.menu.as_mut().filter(|open| open.number() == menu) {
            open.scroll(by);
        }
    }

    fn floats(&self) -> bool {
        matches!(self.keys, Keys::Menu { .. })
    }

    fn seen(&self, machine: &Machine) -> Option<Seen<'_>> {
        let machines = machines(self.host.as_ref()?, &self.shelves);
        match machine {
            Machine::Local => Some(machines.here),
            Machine::Remote(destination) => {
                machines.others.into_iter().find(|(other, _)| other == destination)?.1.ok()
            },
        }
    }

    fn pane_of(&self, machine: &Machine, listed: &Listed, id: u32) -> Option<&Pane> {
        let mut known = shelf(&self.shelves, machine).iter();
        known.find(|known| known.listed() == listed)?.pane(id)
    }

    fn menu_did(&mut self, did: Did) -> Asked {
        self.menu_did_after(Held::Open, did)
    }

    fn menu_did_after(&mut self, held: Held, did: Did) -> Asked {
        let (menu, wants) = match did {
            Did::Stays(menu, wants) => (menu, wants),
            Did::Closed(done) => {
                if held == Held::Open {
                    self.menu = None;
                }
                return match done {
                    Done::Nothing => Asked::Nothing,
                    Done::Wrong(reason) => {
                        self.say(reason);
                        Asked::Nothing
                    },
                    Done::Worktree(on, project, branch) => {
                        Asked::NewWorktree { on, project, branch }
                    },
                    Done::Welcome { on, session, pane } => {
                        Asked::Welcome { asking: held, on, session, pane }
                    },
                    Done::Project { on, folder, name } => Asked::AddProject { on, folder, name },
                    Done::Rename(on, rename) => Asked::Rename { on, rename },
                    Done::Go(on, place) if held == Held::Open => Asked::Go { on, place },
                    Done::Go(_, place) => {
                        let said = Self::went_said(&place);
                        self.say(said);
                        Asked::Nothing
                    },
                    Done::Act(on, act) => self.delete(on, act),
                    Done::Brief(briefed) => self.brief(briefed),
                    Done::Ask(target) => Asked::Ask(target),
                    Done::Issue(repository) => Asked::Issue(repository),
                    Done::Ssh(destination) => Asked::Ssh(destination),
                    Done::Forget(destination) => Asked::Forget(destination),
                };
            },
        };
        let (number, on) = (menu.number(), menu.target().machine().clone());
        let wants = match opens_own(held, &on, wants) {
            Ok(asked) => return asked,
            Err(wants) => wants,
        };
        match held {
            Held::Open => self.menu = Some(menu),
            Held::Kept => {
                self.working.insert(number, menu);
            },
        }
        let asked = self.menu_wants(number, on, wants);
        if held == Held::Kept
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
                if let Some(known) = known.filter(|it| it.unopened == Unopened::Folder) {
                    let refused = projects::folder_has_one_worktree(known.listed().name());
                    self.drop_menu(number);
                    self.say(format!("menu: {refused}"));
                }
                Asked::Nothing
            },
            Wants::Keep(harness) => {
                self.keep_agent(harness);
                Asked::Nothing
            },
            Wants::Skills { cwd, agent } => Asked::Find { menu: number, on, cwd, agent },
            Wants::Refind { cwd, agent } => {
                self.keep_agent(agent);
                Asked::Find { menu: number, on, cwd, agent }
            },
            Wants::Open { session, cwd, opens: Opens::Pane { beside, command }, .. } => {
                let open = Open::Pane { session: session.name, beside, cwd, command };
                Asked::OpenZellij { menu: number, on, open }
            },
            Wants::Open { session, opens: Opens::Tab { .. }, .. } => {
                self.drop_menu(number);
                let refused = format!("{} is one of zellij's sessions", session.name());
                self.say(format!("menu: {refused}: {}", menu::OWN_ONLY));
                Asked::Nothing
            },
            Wants::Pull(work, wanted) => match self.worked(&on) {
                Ok(worked) => {
                    let others = self.others(&on);
                    Asked::Pull { menu: number, on, work, wanted, worked, others }
                },
                Err(why) => self.pulled(number, Err(why)),
            },
            Wants::Printed { session, pane, ended } => {
                Asked::Printed { menu: number, on, session, pane, ended }
            },
            Wants::Checks { page } => Asked::Checks { menu: number, on, page },
            Wants::Make { session, holder, cwd, name, command } => {
                Asked::Make { menu: number, on, session, holder, cwd, name, command }
            },
            Wants::Prunes { dir } => Asked::Prunes { menu: number, on, dir },
            Wants::Prune { dir, agreed } => Asked::Prune { menu: number, on, dir, agreed },
            Wants::Plan { session, scope: Scope::Pane(pane) } => {
                Asked::Closes { menu: number, on, session, pane }
            },
            Wants::Plan { session, scope: Scope::Worktree(worktree) } => {
                let plan = self.deletes_of(&on, worktree).and_then(|deletes| {
                    self.plan(&on, &session, deletes)
                });
                self.planned(plan)
            },
            Wants::Plan { session, scope: Scope::Project } => {
                let plan = self.project_deletes(&on, &session).and_then(|deletes| {
                    self.plan(&on, &session, deletes)
                });
                self.planned(plan)
            },
        }
    }

    fn planned(&mut self, plan: Result<Plan, String>) -> Asked {
        let Some(menu) = self.menu.take() else {
            return Asked::Nothing;
        };
        self.menu_did(menu.planned(plan))
    }

    pub fn prunes(&mut self, menu: u64, plan: Result<prunes::Plan, String>) -> Asked {
        let Some((waiting, held)) = self.take_waited(menu, Menu::sweeping) else {
            return Asked::Nothing;
        };
        if let Ok(plan) = &plan {
            log_unasked(plan);
        }
        self.menu_did_after(held, waiting.swept(plan))
    }

    pub fn pruned(&mut self, menu: u64, pruned: Result<prunes::Pruned, String>) -> Asked {
        let waiting = self.take_waited(menu, Menu::pruning);
        match pruned {
            Ok(prunes::Pruned::Changed(plan)) => {
                log_unasked(&plan);
                if let Some((waiting, Held::Open)) = waiting {
                    return self.menu_did_after(Held::Open, waiting.changed(plan));
                }
                self.say("menu: nothing deleted: what would go changed".to_owned());
            },
            Ok(prunes::Pruned::Went(outcomes)) => {
                if let Some(note) = prune::note(&outcomes) {
                    self.say(note);
                }
            },
            Err(why) => self.say(format!("menu: it may have deleted some worktrees: {why}")),
        }
        Asked::Nothing
    }

    pub fn pulled(&mut self, menu: u64, found: Result<Found, String>) -> Asked {
        let Some((waiting, held)) = self.take_waited(menu, Menu::pulling) else {
            return Asked::Nothing;
        };
        if let Some(off_branch) = waiting.off_branch(&found) {
            self.say(off_branch);
        }
        self.menu_did_after(held, waiting.pulled(found))
    }

    pub fn printed(&mut self, menu: u64, printed: Result<Printed, String>) -> Asked {
        let Some((waiting, held)) = self.take_waited(menu, Menu::reading) else {
            return Asked::Nothing;
        };
        self.menu_did_after(held, waiting.printed(printed))
    }

    pub fn checks(&mut self, menu: u64, checks: Result<checks::Failing, String>) -> Asked {
        let Some((waiting, held)) = self.take_waited(menu, Menu::fetching) else {
            return Asked::Nothing;
        };
        self.menu_did_after(held, waiting.checked(checks))
    }

    fn worked(&self, on: &Machine) -> Result<Vec<Worked>, String> {
        let Some(seen) = self.seen(on) else {
            return match on {
                Machine::Local => Ok(Vec::new()),
                Machine::Remote(destination) => {
                    Err(format!("lunetad does not reach {destination} now"))
                },
            };
        };
        let agents = seen.seated();
        let pulls = pull_requests(seen.pull_requests);
        let mut worked = Vec::new();
        for (listed, pane) in seen.panes() {
            let dir = match agents.get(&(listed.at(), pane.id)) {
                Some(agent) => Some(agent_dir(agent, pane)),
                None => pane.cwd.as_deref(),
            };
            let Some(dir) = dir else {
                continue;
            };
            let pull = pulls.get(dir).and_then(|it| Link::parse(it.url.as_deref()?));
            let here = Self::seats_here(self.own_seat_on(on), listed);
            let Some(PanePlace { session, pane }) = PanePlace::of(listed.clone(), pane) else {
                continue;
            };
            let it = links::Worked { dir: dir.to_owned(), pull, here };
            worked.push(Worked { session, pane, works: it });
        }
        Ok(worked)
    }

    fn others(&self, on: &Machine) -> Others {
        let Some(host) = &self.host else {
            return Vec::new();
        };
        let others = machines(host, &self.shelves).each().map(|(machine, _)| machine);
        others
            .filter(|machine| machine != on)
            .map(|machine| {
                let worked = self.worked(&machine);
                (machine, worked)
            })
            .collect()
    }

    pub fn plan(&self, on: &Machine, listed: &Listed, deletes: Deletes) -> Result<Plan, String> {
        if let Some(why) = self.out_of_reach(on) {
            return Err(why);
        }
        Ok(close::plan(&self.subject(on, listed, deletes)))
    }

    fn project_deletes(&self, on: &Machine, listed: &Listed) -> Result<Deletes, String> {
        let unlisted = || format!("project {} is no longer listed", listed.name());
        let (project, key) = listed.project().zip(listed.own_key()).ok_or_else(unlisted)?;
        let known = shelf(&self.shelves, on).iter().find(|known| known.project.id() == project);
        let known = known.ok_or_else(unlisted)?;
        let worktrees = known.worktrees.iter().map(|worktree| worktree.place.worktree).collect();
        let seen = known.worktrees.iter().flat_map(|worktree| worktree.terminals.iter().copied());
        Ok(Deletes::Project { key, project, worktrees, seen: seen.collect() })
    }

    fn deletes_of(&self, on: &Machine, worktree: WorktreeId) -> Result<Deletes, String> {
        if let Some(why) = self.out_of_reach(on) {
            return Err(why);
        }
        let worktrees = shelf(&self.shelves, on).iter().flat_map(|known| &known.worktrees);
        let mut held = worktrees.filter(|it| it.place.worktree == worktree);
        let Some(found) = held.next() else {
            return Err(format!("worktree {worktree} is no longer listed"));
        };
        let panes = found.panes.iter().map(|pane| pane.id).collect();
        Ok(Deletes::Worktree { worktree, panes, seen: found.terminals.clone() })
    }

    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        match on {
            Machine::Remote(destination) if self.seen(on).is_none() => {
                Some(format!("menu: lunetad does not reach {destination} now"))
            },
            Machine::Remote(_) | Machine::Local => None,
        }
    }

    fn subject<'a>(&'a self, on: &Machine, listed: &'a Listed, deletes: Deletes) -> Subject<'a> {
        let seat = if Self::seats_here(self.own_seat_on(on), listed) {
            Seat::In
        } else {
            Seat::Elsewhere
        };
        let mut agents = Err(Untold::Away);
        let mut refuge: Option<Listed> = None;
        if let Some(seen) = self.seen(on) {
            if self.away.is_none() {
                agents = seen.unread_seats().map_err(Untold::Unplaced).map(|unread_seats| {
                    let seated = seen.seated();
                    let doing = seated.iter().filter(|((at, _), _)| *at == listed.at());
                    let doing =
                        doing.map(|((_, pane), agent)| (*pane, close::Doing::from(*agent)));
                    doing.chain(close::unread_in(&unread_seats, listed.at())).collect()
                });
            }
            let mut others = seen.projects.iter().map(|known| known.listed().clone());
            let local = *on == Machine::Local;
            refuge = others.rfind(|other| local && other != listed && other.own());
        }
        Subject { session: listed, seat, deletes, agents, refuge: refuge.map(Place::Session) }
    }

    fn brief(&mut self, briefed: Briefed) -> Asked {
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
            let statuses: HashMap<&str, &lunetad::api::GitStatus> =
                seen.git_statuses.iter().map(|status| (status.dir.as_str(), status)).collect();
            for known in seen.projects {
                let listed = known.listed();
                every.push((machine.clone(), listed.name.clone()));
                for pane in known.panes() {
                    let Some(agent) = agents.get(&(listed.at(), pane.id)) else {
                        continue;
                    };
                    let cwd = agent_dir(agent, pane);
                    let branch = statuses.get(cwd).and_then(|status| {
                        checkout(&GitStatus::of_api(status)?, None).map(|checkout| checkout.branch)
                    });
                    notes.push(Note {
                        on: machine.clone(),
                        session: listed.name.clone(),
                        pane: pane.id,
                        name: label(Some(agent), pane),
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
            self.say(format!("brief: lunetad does not reach {destination} now"));
            return Asked::Nothing;
        }
        let over = match briefed {
            Briefed::Every => Over::Sessions { sessions: every, unknown },
            Briefed::Machine(on) => {
                let of = every.into_iter().filter(|(machine, _)| *machine == on);
                let sessions = of.map(|(_, session)| session).collect();
                Over::Machine(on, sessions)
            },
            Briefed::Session(on, session) => Over::Session(on, session),
            Briefed::Agent(on, session, pane) => Over::Agent(on, session, pane),
        };
        match Brief::new(over, notes) {
            Ok(brief) => Asked::Brief(brief),
            Err(reason) => {
                self.say(reason);
                Asked::Nothing
            },
        }
    }

    fn delete(&mut self, on: Machine, act: Act) -> Asked {
        let takes_place = |place: &Place| match (&act, place.pane()) {
            (Act::Pane { session, pane, .. }, Some(at)) => {
                place.session() == session.as_str() && at == pane.id()
            },
            (
                Act::Tab { session, panes, .. } | Act::Worktree { session, panes, .. },
                Some(at),
            ) => place.session() == session.as_str() && panes.contains(&at),
            (Act::Worktree { worktree, .. }, None) => {
                matches!(place, Place::Worktree(at) if at.worktree == *worktree)
            },
            (Act::Pane { .. } | Act::Tab { .. }, None) => false,
            (
                Act::Project { session, .. } | Act::Kill { session, .. } | Act::Leave { session, .. },
                _,
            ) => place.session() == session.as_str(),
        };
        let takes = |spot: &Spot| {
            spot.place().is_some_and(|place| spot.machine() == on && takes_place(place))
        };
        if let Some(gone) = self.cursor.clone().filter(|cursor| takes(cursor)) {
            let at = self.cursor_index().unwrap_or(0);
            let stays = |index: &usize| {
                let row = &self.rows[*index];
                row.stops() && !takes(&row.spot())
            };
            let after = (at..self.rows.len()).chain((0..at).rev()).find(stays);
            let after = after.map(|index| self.rows[index].spot());
            self.closing = Some(Closing { gone, after });
        }
        Asked::Delete { on, act }
    }

    pub fn opened(&mut self, menu: u64, opened: Result<PaneKey, String>) -> Asked {
        let Some((waiting, held, opening)) = self.take_seen(menu, Menu::opening) else {
            return Asked::Nothing;
        };
        let on = waiting.target().machine().clone();
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                self.say(format!("menu: nothing opened: {why}"));
                return Asked::Nothing;
            },
        };
        let session = opening;
        let named = session.name().clone();
        let Some(place) = PanePlace::new(session, pane) else {
            self.say(format!("menu: {named} and view {pane} are of different muxes"));
            return Asked::Nothing;
        };
        let place = Place::Pane(place);
        if held == Held::Open {
            return Asked::Go { on, place };
        }
        let said = Self::opened_said(&place);
        self.say(said);
        Asked::Nothing
    }

    pub fn made_for(&mut self, menu: u64) -> Option<Held> {
        let made = |it: &Menu| it.made().map(|_| ());
        self.take_seen(menu, made).map(|(_, held, ())| held)
    }

    pub fn closes(&mut self, menu: u64, closes: Result<Closes, String>) -> Asked {
        let waiting = self.menu.as_ref().filter(|it| it.number() == menu);
        let Some(((session, pane), on)) =
            waiting.and_then(|it| Some((it.planning()?, it.target().machine())))
        else {
            return Asked::Nothing;
        };
        let (session, on) = (session.clone(), on.clone());
        let plan = match closes {
            Ok(closes) => self.plan(&on, &session, Deletes::Pane { pane, closes }),
            Err(why) => Err(format!("menu: nothing closed: {why}")),
        };
        self.planned(plan)
    }

    pub fn found(&mut self, menu: u64, agent: agents::Harness, skills: Result<Vec<Skill>, String>) {
        if let Some(open) = self.menu.as_mut().filter(|it| it.number() == menu) {
            open.found(agent, skills);
        }
    }

    fn toggle_unread(&mut self, now: SystemTime) {
        let Some(spot) = &self.cursor else {
            return;
        };
        let Some(Row::Pane { session, pane, unread, marked, .. }) =
            self.rows.iter().find(|row| row.spot() == *spot)
        else {
            return;
        };
        let key = session.key(pane.id());
        if dotted(*unread, marked.as_ref()) {
            self.news.read(key, self.finished.get(&key).copied(), now);
        } else {
            self.news.mark_unread(key, now);
        }
        self.rebuild();
    }

    fn search_key(&mut self, key: &KeyEvent, mut term: String) -> Asked {
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Home => self.end(false),
            KeyCode::End => self.end(true),
            KeyCode::Enter => return self.go(),
            KeyCode::Esc => self.close_search(),
            KeyCode::Backspace if term.is_empty() => self.close_search(),
            KeyCode::Backspace => {
                term.pop();
                self.find(term);
            },
            KeyCode::Char(c) => {
                term.push(c);
                self.find(term);
            },
            _ => {},
        }
        Asked::Nothing
    }

    fn search(&self) -> Option<&str> {
        self.search.as_ref().map(|search| match search {
            Search::Empty => "",
            Search::Typed { term, .. } => term.as_str(),
        })
    }

    fn find(&mut self, term: String) {
        let search =
            if term.is_empty() { Search::Empty } else { Search::Typed { term, seek: true } };
        self.search = Some(search);
        self.rebuild();
    }

    fn close_search(&mut self) {
        if self.search.take().is_some() {
            self.rebuild();
        }
    }

    fn go(&mut self) -> Asked {
        let Some(spot) = self.cursor.clone() else {
            return Asked::Nothing;
        };
        let on = spot.machine();
        match spot {
            Spot::Machine(_) => {
                self.toggle();
                Asked::Nothing
            },
            Spot::Local(place) | Spot::Remote(_, place) => {
                self.close_search();
                Asked::Go { on, place }
            },
        }
    }

    pub fn mouse(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
        ) {
            self.pending = Pending::Nothing;
        }
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.top = (self.top + 3).min(self.lines().saturating_sub(1));
                Asked::Nothing
            },
            MouseEventKind::ScrollUp => {
                self.top = self.top.saturating_sub(3);
                Asked::Nothing
            },
            MouseEventKind::Down(MouseButton::Left) => {
                self.close_menu();
                self.unselect();
                let row = self.line_of(mouse.row, area).and_then(|line| self.row_at(line));
                let lifts = row.and_then(|at| self.lifting(at));
                let at = Position::new(mouse.column, mouse.row);
                self.moving = Moving::Down { at, lifts };
                Asked::Nothing
            },
            MouseEventKind::Down(_) => Asked::Take,
            MouseEventKind::Drag(MouseButton::Left) => self.drag(mouse, area),
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse, area),
            MouseEventKind::Moved => {
                self.let_go();
                Asked::Nothing
            },
            _ => Asked::Nothing,
        }
    }

    fn drag(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let line = mouse.row.saturating_sub(area.y);
        let over = (area.left()..area.right()).contains(&mouse.column);
        let pointer = Position::new(mouse.column, mouse.row);
        let (from, lifts) = match &mut self.moving {
            Moving::Lifted(lift) => {
                lift.line = line;
                lift.over = over;
                return Asked::Nothing;
            },
            Moving::Sweeping(sweep) => {
                sweep.reach(pointer, inside(area));
                return Asked::Nothing;
            },
            Moving::Down { at, lifts } => (*at, lifts.clone().filter(|_| mouse.row != at.y)),
            Moving::Selected(_) | Moving::Swept(..) | Moving::Still => {
                return Asked::Nothing;
            },
        };
        let typed = matches!(self.search, Some(Search::Typed { .. }));
        let lifts = lifts.filter(|lifts| !typed && self.row_of(&lifts.spot()).is_some());
        if let Some(lifted) = lifts {
            self.moving = Moving::Lifted(Lift { lifted, line, over, scrolled: None });
            return Asked::Take;
        }
        if from.x.abs_diff(pointer.x) > 1 || from.y.abs_diff(pointer.y) > 1 {
            self.moving = Moving::Sweeping(Sweep::new(from, pointer, inside(area)));
        }
        Asked::Nothing
    }

    fn release(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let over = (area.left()..area.right()).contains(&mouse.column);
        match std::mem::take(&mut self.moving) {
            Moving::Down { at, .. } if over && mouse.row == at.y => self.click(at, area),
            Moving::Lifted(Lift { lifted: movable, .. }) => {
                let line = mouse.row.saturating_sub(area.y);
                self.cursor = Some(movable.spot());
                let spans = self.spans(&movable);
                match self.landing(&movable, line, over) {
                    Some((landed, _)) if landed != spans => self.keep(&landed),
                    _ => {
                        self.settle(None, false);
                        Asked::Nothing
                    },
                }
            },
            Moving::Sweeping(mut sweep) => {
                sweep.reach(Position::new(mouse.column, mouse.row), inside(area));
                let text = sweep.text(&self.page, inside(area));
                if text.is_empty() {
                    return Asked::Nothing;
                }
                self.moving = Moving::Swept(sweep, self.laid.clone());
                Asked::CopyText(text)
            },
            kept @ (Moving::Selected(_) | Moving::Swept(..)) => {
                self.moving = kept;
                Asked::Nothing
            },
            Moving::Down { .. } | Moving::Still => Asked::Nothing,
        }
    }

    fn click(&mut self, at: Position, area: Rect) -> Asked {
        let Some(index) = self.line_of(at.y, area).and_then(|line| self.row_at(line)) else {
            return Asked::Take;
        };
        let row = &self.rows[index];
        let spot = row.spot();
        let typed = matches!(self.search, Some(Search::Typed { .. }));
        if matches!(row, Row::Machine { .. }) {
            if !typed {
                self.cursor = Some(spot);
                self.toggle();
            }
            return Asked::Take;
        }
        let triangle = match row {
            Row::Project { .. } => Some(2),
            Row::Worktree { .. } => Some(3),
            Row::Machine { .. } | Row::Pane { .. } => None,
        };
        if let Some(triangle) = triangle {
            let on_triangle = at.x.saturating_sub(area.x) <= triangle;
            if on_triangle && !typed {
                self.cursor = Some(spot);
                self.toggle();
                return Asked::Take;
            }
            if !row.stops() {
                return Asked::Take;
            }
        }
        self.cursor = Some(spot);
        match self.go() {
            Asked::Nothing => Asked::Take,
            asked => asked,
        }
    }

    fn line_of(&self, y: u16, area: Rect) -> Option<usize> {
        let line = usize::from(y.saturating_sub(area.y));
        let shown = line.checked_sub(HEADER).filter(|line| *line < self.room)?;
        Some(self.top + shown)
    }

    pub fn pressed(&self) -> bool {
        matches!(self.moving, Moving::Down { .. } | Moving::Lifted(_) | Moving::Sweeping(_))
    }

    pub fn unsweep(&mut self) {
        if let Moving::Swept(..) = self.moving {
            self.moving = Moving::Still;
        }
    }

    pub fn let_go(&mut self) {
        if self.pressed() {
            self.moving = Moving::Still;
        }
    }

    pub fn scrolling(&self) -> bool {
        self.pull() != 0
    }

    fn pull(&self) -> isize {
        let Moving::Lifted(lift) = &self.moving else {
            return 0;
        };
        if !lift.over {
            return 0;
        }
        let line = usize::from(lift.line);
        let up = line <= HEADER && self.top > 0;
        let down = line + 1 >= HEADER + self.room && self.top + self.room < self.lines();
        isize::from(down) - isize::from(up)
    }

    fn hold(&mut self, at: Instant) {
        let by = self.pull();
        let Moving::Lifted(lift) = &mut self.moving else {
            return;
        };
        if by == 0 {
            lift.scrolled = None;
            return;
        }
        if lift.scrolled.is_some_and(|then| at.saturating_duration_since(then) < SCROLL) {
            return;
        }
        lift.scrolled = Some(at);
        self.top = self.top.saturating_add_signed(by);
    }

    fn lines(&self) -> usize {
        self.rows.iter().map(Row::height).sum()
    }

    fn row_at(&self, line: usize) -> Option<usize> {
        let mut start = 0;
        for (at, row) in self.rows.iter().enumerate() {
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
            return Highlight::Lit(self.cursor_index());
        }
        let row = self.here_row.as_ref().and_then(|spot| self.row_of(spot));
        row.map_or(Highlight::Off, Highlight::Resting)
    }

    fn reveal(&mut self, at: usize, room: usize) {
        let start: usize = self.rows[..at].iter().map(Row::height).sum();
        let end = start + self.rows[at].height();
        if start < self.top {
            self.top = start;
        } else if end > self.top + room {
            self.top = end.saturating_sub(room);
        }
        if end - start <= room {
            assert!(self.top <= start);
            assert!(end <= self.top + room);
        }
    }

    pub fn draw(&mut self, buffer: &mut Buffer, area: Rect, now: SystemTime) -> Drawn {
        self.drawing(now);
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let frame = frame(now);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        lines.extend(self.top(width, frame));
        let (notes, spotted, buttons) = self.notes(width, height, now);
        let room = height.saturating_sub(HEADER + notes.lines.len());
        self.room = room;
        let highlight = self.highlight();
        let landing = match &self.moving {
            Moving::Lifted(Lift { lifted, line, over, .. }) => self.landing(lifted, *line, *over),
            _ => None,
        };
        let lifted = landing.is_some();
        match highlight {
            Highlight::Lit(at) => {
                if let Some(at) = at
                    && !lifted
                {
                    self.reveal(at, room);
                }
                self.revealed = None;
            },
            Highlight::Resting(at) if self.revealed != self.here_row => {
                self.reveal(at, room);
                self.revealed.clone_from(&self.here_row);
            },
            Highlight::Resting(_) | Highlight::Off => {},
        }
        if self.host.is_none() {
            let waiting = if self.away.is_some() { "" } else { "asking lunetad…" };
            lines.push(Line::styled(
                format!(" {waiting}"),
                Style::new().add_modifier(Modifier::DIM),
            ));
        } else if self.rows.is_empty() {
            assert!(
                matches!(self.search, Some(Search::Typed { .. })),
                "only a search hides this machine's row"
            );
            lines.push(Line::styled(" no match", Style::new().add_modifier(Modifier::DIM)));
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
            },
            (None, Highlight::Resting(at)) => Some((at..at + 1, self.home_row(at), RESTING)),
            (None, Highlight::Off) => None,
        };
        let order = match &landing {
            Some((spans, _)) => self.landed(&spans.rows()),
            None => (0..self.rows.len()).collect::<Vec<_>>(),
        };
        let mut drawn: Vec<Line<'static>> = Vec::new();
        let mut numbers = Vec::new();
        let mut cuts = Vec::new();
        let mut rows = Vec::with_capacity(order.len());
        for at in order {
            let row = &self.rows[at];
            let lights = |(rows, home, _): &&(Range<usize>, Option<usize>, Color)| {
                rows.contains(&at) || *home == Some(at)
            };
            let on = lit.as_ref().filter(lights);
            let background = on.map(|(_, _, background)| *background);
            let RowLines { lines: row_drawn, number, cuts: row_cuts } =
                row_lines(row, width, frame, now);
            if !lifted
                && let Some(number) = number
            {
                numbers.push(NumberAt { line: drawn.len() + number.line, ..number });
            }
            let first = drawn.len();
            cuts.extend(row_cuts.into_iter().map(|cut| CutAt { line: first + cut.line, ..cut }));
            rows.push((row.spot(), row_drawn.len()));
            for mut line in row_drawn {
                if let Some(background) = background {
                    line = line.patch_style(Style::new().bg(background));
                }
                drawn.push(line);
            }
        }
        self.top = self.top.min(drawn.len().saturating_sub(1));
        let bottom = notes.lines.len();
        let laid = Layout { area, top: self.top, rows, bottom, notes: notes.said };
        lines.extend(drawn.into_iter().skip(self.top).take(room));
        let notes_from = height.saturating_sub(notes.lines.len());
        lines.resize(notes_from, Line::default());
        let goes_on = notes.goes_on.iter().map(|line| notes_from + line).collect();
        lines.extend(notes.lines);
        put(buffer, area, &lines, self.framing());
        let mut spots = Spots::default();
        let mut cut_spots = Spots::default();
        let shown = self.top..self.top + room;
        for NumberAt { line, columns, link } in numbers {
            if shown.contains(&line) {
                spots.add(HEADER + line - self.top, columns, link.clone());
            }
        }
        for CutAt { line, column, took } in cuts {
            if shown.contains(&line) {
                cut_spots.add(HEADER + line - self.top, column..column + 1, took);
            }
        }
        let inside = inside(area);
        if matches!(&self.moving, Moving::Swept(_, under) if *under != laid) {
            self.moving = Moving::Still;
        }
        if let Moving::Sweeping(sweep) | Moving::Swept(sweep, _) = &self.moving {
            sweep.draw(buffer, inside);
        }
        self.laid = laid;
        self.page = Page { lines, goes_on, cuts: cut_spots.placed(inside) };
        self.aloud_buttons = buttons.down(notes_from).placed(inside);
        Drawn::Sidebar {
            numbers: spots.placed(inside),
            bottom: Box::new(spotted.placed(notes_from, inside)),
        }
    }

    pub fn draw_collapsed(&mut self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        self.drawing(now);
        self.unsweep();
        self.aloud_buttons = Spots::default();
        let width = usize::from(area.width.saturating_sub(1));
        let rule = if self.away.is_some() {
            Style::new().fg(ERROR)
        } else if self.image_going.is_some() {
            Style::new().fg(NAME)
        } else {
            Style::new().add_modifier(Modifier::DIM)
        };
        let mut lines = vec![Line::styled(RULE.repeat(width), rule), Line::default()];
        for (count, (glyph, style)) in tallies(self.counts, frame(now)) {
            if count == 0 {
                continue;
            }
            let count = cut(&count.to_string(), width.saturating_sub(glyph.width()));
            lines.push(Line::from(vec![Span::styled(glyph, style), Span::styled(count, style)]));
        }
        if self.aloud.as_ref().is_some_and(aloud::Shown::paused) {
            lines.push(Line::styled(aloud::PLAY, Style::new().fg(ACCENT)));
        }
        put(buffer, area, &lines, self.framing());
    }

    fn drawing(&mut self, now: SystemTime) {
        self.drawn = Some(now);
        if self.due.is_some_and(|due| due <= now) {
            self.rebuild();
        }
    }

    fn top(&self, width: usize, frame: usize) -> [Line<'static>; HEADER] {
        [self.title(width), self.header(width, frame), Line::default()]
    }

    fn framing(&self) -> Style {
        match self.keys {
            Keys::Sidebar => Style::new().fg(ACCENT),
            Keys::Zellij | Keys::Lent | Keys::Menu { .. } => {
                Style::new().add_modifier(Modifier::DIM)
            },
        }
    }

    fn title(&self, width: usize) -> Line<'static> {
        let rule = self.framing();
        let name =
            if self.keys == Keys::Sidebar { rule.add_modifier(Modifier::BOLD) } else { rule };
        let word = format!(" {} ", menu::TITLE);
        let rest = width.saturating_sub(RULE.width() + word.width());
        Line::from(vec![
            Span::styled(RULE, rule),
            Span::styled(word, name),
            Span::styled(RULE.repeat(rest), rule),
        ])
    }

    fn header(&self, width: usize, frame: usize) -> Line<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Counts { projects, waiting, .. } = self.counts;
        let mut counts: Vec<Span<'static>> = Vec::new();
        let waits = (waiting, Mark::Waiting.glyph(frame));
        for (count, (glyph, style)) in std::iter::once(waits).chain(tallies(self.counts, frame)) {
            if count == 0 {
                continue;
            }
            if !counts.is_empty() {
                counts.push(Span::raw(" "));
            }
            counts.push(Span::styled(glyph, style));
            counts.push(Span::styled(count.to_string(), style));
        }
        let word = match projects {
            0 => String::new(),
            1 => " 1 project ".to_owned(),
            n => format!(" {n} projects "),
        };
        let counted: usize = counts.iter().map(|span| span.content.width()).sum();
        let tail = if counted > 0 { counted + 3 } else { 0 };
        let rule = width.saturating_sub(1 + word.width() + tail);
        let mut spans = vec![Span::styled(RULE, dim), Span::styled(word, dim)];
        spans.push(Span::styled(RULE.repeat(rule), dim));
        if counted > 0 {
            spans.push(Span::raw(" "));
            spans.extend(counts);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(RULE, dim));
        }
        Line::from(spans)
    }

    fn notes(
        &self,
        width: usize,
        height: usize,
        now: SystemTime,
    ) -> (Notes, Drawn, Spots<aloud::Button>) {
        let frame = frame(now);
        let mut notes = Notes::default();
        let error = Style::new().fg(ERROR);
        if let Some(away) = &self.away {
            notes.say(&format!("lunetad: {away}"), width, error);
        }
        if self.unlisted > 0 {
            let count = self.unlisted;
            let projects = if count == 1 { "project" } else { "projects" };
            let unlisted = format!(
                "lunetad: {count} {projects} with an id of 0 or a name luneta refuses, not listed"
            );
            notes.say(&unlisted, width, error);
        }
        if self.unlisted_machines > 0 {
            let count = self.unlisted_machines;
            let machines = if count == 1 { "machine" } else { "machines" };
            let unlisted = format!(
                "lunetad: {count} {machines} with a destination luneta does not read, or named \
                 twice, not listed"
            );
            notes.say(&unlisted, width, error);
        }
        if let Some(unread) = self.host.as_ref().and_then(Host::unread_count) {
            notes.say(&unread.to_string(), width, error);
        }
        if self.host.as_ref().is_some_and(|host| host.unread_hosts.is_some_and(|it| it > 0)) {
            let unread = "lunetad could not read all of luneta's hosts file: its log says why";
            notes.say(unread, width, error);
        }
        if let Some(why) = self.unreached() {
            notes.say(&why, width, error);
        }
        if let Some(lost) = self.folds.lost() {
            notes.say(&format!("folds stay in this luneta: {lost}"), width, error);
        }
        if let Some(lost) = self.news.lost() {
            notes.say(&format!("dots stay in this luneta: {lost}"), width, error);
        }
        if let Some(notice) = &self.notice {
            notes.say(notice, width, error);
        }
        if let Some((number, _)) = &self.copied {
            notes.say(&format!("copied #{number}"), width, Style::new().fg(NAME));
        }
        if let Some(filed) = &self.filed {
            notes.say(filed, width, Style::new().fg(NAME));
        }
        for repository in self.filing.values() {
            let filing = format!("Filing an issue in {repository}…");
            notes.lines.extend(menu::waiting(&filing, width, Style::new().fg(NAME), frame));
        }
        if let Some(to) = &self.image_going {
            let going = format!("image on its way to {to}");
            notes.say(&going, width, Style::new().fg(NAME));
        }
        for working in self.working.values() {
            if let Some(doing) = working.doing() {
                notes.lines.extend(menu::waiting(&doing, width, Style::new().fg(NAME), frame));
            }
        }
        let mut buttons = Spots::default();
        if let Some(aloud) = &self.aloud {
            let (line, spots) = aloud.line(width);
            buttons = spots.down(notes.lines.len());
            notes.lines.push(line);
        }
        let room = height.saturating_sub(HEADER + 1 + notes.lines.len());
        let above = notes.lines.len();
        let (lines, spotted) = match self.bottom(now) {
            Bottom::Menu(menu) => {
                let (lines, spots) = menu.lines(width, room, frame);
                let spots = spots.down(above);
                (lines, Drawn::Menu { number: menu.number(), area: None, spots })
            },
            Bottom::Search(search) => {
                let (lines, spots) = self.searching(search, width, room);
                (lines, Drawn::Keys(Keyed::Search, spots.down(above)))
            },
            Bottom::Legend => {
                let (lines, spots) = menu::legend(self.target().as_ref(), width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::Group(key) => {
                let (lines, spots) = menu::grouped(key, width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::Keys(title, keys) => {
                let (lines, spots) = menu::shortcuts(title, keys, width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::AllKeys(all) => {
                let (lines, spots) = all.lines(width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::Nothing => (Vec::new(), Drawn::Nothing),
        };
        notes.lines.extend(lines);
        (notes, spotted, buttons)
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
                let found = self.rows.iter().filter(|row| matches!(row, Row::Pane { .. }));
                (term.as_str(), format!("{}/{}", found.count(), self.counts.panes))
            },
        };
        let mut lines = Vec::with_capacity(3);
        if room >= 2 {
            lines.push(menu::rule(Action::Search.name(), &counted, width));
        }
        lines.extend((room >= 1).then(|| query(term, width)));
        if room < 3 || self.keys != Keys::Sidebar {
            return (lines, Spots::default());
        }
        let (keys, spots) = menu::keyed(width, &[(Hint::Enter, "Go"), (Hint::Esc, "Close")]);
        let at = lines.len();
        lines.push(keys);
        (lines, spots.down(at))
    }

    fn unreached(&self) -> Option<String> {
        if self.keys != Keys::Sidebar {
            return None;
        }
        let machine = self.cursor.as_ref()?.machine();
        match self.machine_row(&machine) {
            (name, Some(Away::Unreached { why, .. }), _) => {
                Some(format!("{name}: {why}; what ssh said is in {}", lunetad::log::NAME))
            },
            (_, None | Some(Away::Reaching), _) => None,
        }
    }

    fn bottom(&self, now: SystemTime) -> Bottom<'_> {
        if let Some(menu) = self.menu.as_ref().filter(|_| !self.floats()) {
            if self.menu_paused(menu, now) { Bottom::Nothing } else { Bottom::Menu(menu) }
        } else if let Some(search) = &self.search {
            Bottom::Search(search)
        } else if self.keys != Keys::Sidebar {
            Bottom::Nothing
        } else {
            match &self.pending {
                Pending::Legend => Bottom::Legend,
                Pending::AllKeys(all) => Bottom::AllKeys(all),
                Pending::Group { key, shows } if *shows <= now => Bottom::Group(*key),
                Pending::First { shows } if *shows <= now => Bottom::Keys(GO, &GO_KEYS),
                Pending::Nothing | Pending::First { .. } | Pending::Group { .. } => {
                    match self.moving {
                        Moving::Selected(_) => {
                            Bottom::Keys(Action::Reorder.name(), &SELECTED_KEYS)
                        },
                        Moving::Still
                        | Moving::Down { .. }
                        | Moving::Lifted(_)
                        | Moving::Sweeping(_)
                        | Moving::Swept(..) => Bottom::Nothing,
                    }
                },
            }
        }
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

fn tallies(counts: Counts, frame: usize) -> [(usize, (String, Style)); 3] {
    let dot = dot();
    [
        (counts.new, (dot.content.into_owned(), dot.style)),
        (counts.busy, Mark::Busy.glyph(frame)),
        (counts.shell, Mark::Shell.glyph(frame)),
    ]
}

pub fn edge(area: Rect) -> Option<u16> {
    area.width.checked_sub(1).map(|last| area.x + last)
}

fn inside(area: Rect) -> Rect {
    Rect { width: area.width.saturating_sub(1), ..area }
}

fn put(buffer: &mut Buffer, area: Rect, lines: &[Line<'static>], framing: Style) {
    buffer.set_style(area, Style::reset().fg(gruvbox::FG1));
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
    Line::from(vec![Span::raw(" "), Span::styled(typed, Style::new().fg(ACCENT))])
}

fn pull_requests(pull_requests: &[lunetad::api::PullRequest]) -> HashMap<&str, PullRequest> {
    pull_requests
        .iter()
        .filter_map(|it| Some((it.dir.as_str(), PullRequest::of_api(it)?)))
        .collect()
}

fn moved<T: Clone>(items: &[T], taken: Range<usize>, to: usize) -> Vec<T> {
    let left = items.iter().enumerate().filter(|(at, _)| !taken.contains(at));
    let mut moved: Vec<T> = left.map(|(_, item)| item.clone()).collect();
    moved.splice(to..to, items[taken].iter().cloned());
    moved
}

fn transcript(agent: &Agent) -> Transcript {
    match agent.runs_in() {
        Harness::ClaudeCode => {
            SessionId::parse(&agent.session_id).map_or(Transcript::Unnamed, Transcript::Named)
        },
        Harness::Pi | Harness::Unspecified => Transcript::NotRead,
    }
}

fn agent_dir<'a>(agent: &'a Agent, pane: &'a Pane) -> &'a str {
    pane.cwd.as_deref().unwrap_or(&agent.cwd)
}

fn label(agent: Option<&Agent>, pane: &Pane) -> String {
    if let Some(agent) = agent {
        let titled = pane.titled.as_ref().and_then(|titled| titled.topic.as_deref());
        let topic = agent.topic.as_deref().or(titled);
        let name = given_name(agent).or(topic);
        return name.map_or_else(|| short_cwd(agent_dir(agent, pane)), str::to_owned);
    }
    let mut words = pane.command.iter().flat_map(|command| command.split_whitespace());
    let program = words.next().map(|program| {
        let program = program.rsplit('/').next().unwrap_or(program);
        program.strip_prefix('-').unwrap_or(program)
    });
    match program {
        Some(program) if !icons::SHELLS.contains(&program) => {
            [program].into_iter().chain(words).collect::<Vec<_>>().join(" ")
        },
        _ => pane.cwd.as_deref().map_or_else(|| format!("view {}", pane.id), short_cwd),
    }
}

fn agent_marked(news: &News, agent: &Agent, key: Key, here: bool) -> (Marked, Option<SystemTime>) {
    let mut marked = Marked::of(agent);
    let done = marked.since.filter(|_| marked.mark == Mark::Idle);
    if !here && news.is_new(key, done) {
        marked.mark = Mark::Finished;
    }
    (marked, done)
}

fn status_word(agent: &Agent) -> String {
    match &agent.status {
        Some(Reported::KnownStatus(known)) => match Status::try_from(*known) {
            Ok(Status::Waiting) => "waiting".to_owned(),
            Ok(Status::Idle) => "idle".to_owned(),
            Ok(Status::Busy) => "busy".to_owned(),
            Ok(Status::Shell) => "shell".to_owned(),
            Ok(Status::Unspecified) | Err(_) => "unknown".to_owned(),
        },
        Some(Reported::OtherStatus(word)) => word.clone(),
        None => "unreported".to_owned(),
    }
}

fn given_name(agent: &Agent) -> Option<&str> {
    agent.name.as_ref()?.given()
}

pub fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_owned(),
        [only] => (*only).to_owned(),
        [.., parent, base] => format!("{parent}/{base}"),
    }
}

pub type Fit = (bool, i64);

pub struct Finder<'a> {
    matcher: SkimMatcherV2,
    anywhere: SkimMatcherV2,
    term: &'a str,
}

impl<'a> Finder<'a> {
    pub fn new(term: &'a str) -> Self {
        let anywhere = SkimScoreConfig { bonus_first_char_multiplier: 1, ..Default::default() };
        let anywhere = SkimMatcherV2::default().score_config(anywhere);
        Finder { matcher: SkimMatcherV2::default(), anywhere, term }
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

    fn pane(
        &self,
        label: &str,
        branch: Option<&str>,
        tab_name: &str,
        cwd: Option<&str>,
    ) -> Option<(Fit, Vec<usize>)> {
        let cwd = cwd.map(short_cwd);
        self.find(label, &[branch, tab::named(tab_name), cwd.as_deref()])
    }

    pub fn find(&self, shown: &str, also: &[Option<&str>]) -> Option<(Fit, Vec<usize>)> {
        let exact = shown == self.term;
        let best = |matcher: &SkimMatcherV2| {
            also.iter().flatten().filter_map(|text| matcher.fuzzy_match(text, self.term)).max()
        };
        let other = best(&self.matcher);
        let Some((score, hits)) = self.matcher.fuzzy_indices(shown, self.term) else {
            return Some(((exact, other?), Vec::new()));
        };
        let fits = |score: i64, other: Option<i64>| other.is_none_or(|other| score >= other);
        let lit = fits(score, other)
            || self.anywhere.fuzzy_match(shown, self.term).is_some_and(|anywhere| {
                fits(anywhere, best(&self.anywhere))
            });
        let fit = (exact, other.map_or(score, |other| score.max(other)));
        Some((fit, if lit { hits } else { Vec::new() }))
    }
}

fn log_unasked(plan: &prunes::Plan) {
    for unasked in &plan.unasked {
        log::say(&format!("{}: {}", unasked.folder, unasked.why));
    }
}

fn repository_of(status: &GitStatus) -> Option<String> {
    match status {
        GitStatus::NoCheckout { repository }
        | GitStatus::Checkout { repository: Some(repository), .. } => {
            Some(repository.as_str().to_owned())
        },
        GitStatus::Outside | GitStatus::Checkout { repository: None, .. } => None,
    }
}

fn checkout(status: &GitStatus, pull_request: Option<&PullRequest>) -> Option<Checkout> {
    let GitStatus::Checkout {
        head: Head::Branch { name, upstream, changed },
        changes,
        stashes,
        ..
    } = status
    else {
        return None;
    };
    Some(Checkout {
        branch: name.as_str().to_owned(),
        status: glyphs(changes, *stashes, *upstream),
        changed: *changed,
        pull_request: pull_request.and_then(numbered),
    })
}

fn numbered(pull_request: &PullRequest) -> Option<Numbered> {
    let colour = match pull_request.state {
        State::Open => gruvbox::GREEN,
        State::Draft => gruvbox::GREY,
        State::Merged => gruvbox::PURPLE,
        State::Closed => return None,
    };
    let checks = pull_request.verdict();
    let number = pull_request.number;
    let link = pull_request.url.as_deref().and_then(Link::parse);
    let link = link.filter(|link| link.kind() == Kind::Pull && link.number() == number);
    let number = link.map_or(Number::Unlinked(number), |link| Number::Linked(Box::new(link)));
    Some(Numbered { number, colour, checks, stale: pull_request.stale })
}

fn verdict_mark(verdict: Verdict, stale: bool) -> Span<'static> {
    let (mark, colour) = match verdict {
        Verdict::Failing => ("✗", gruvbox::RED),
        Verdict::Pending => ("●", gruvbox::YELLOW),
        Verdict::Passing => ("✓", gruvbox::GREEN),
    };
    let colour = if stale { gruvbox::GREY } else { colour };
    Span::styled(mark, Style::new().fg(colour))
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
    let mut glyphs: String =
        kinds.iter().filter(|(count, _)| *count > 0).map(|(_, glyph)| *glyph).collect();
    match upstream.map(|upstream| (upstream.ahead, upstream.behind)) {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => glyphs.push('⇕'),
        Some((ahead, _)) if ahead > 0 => glyphs.push('⇡'),
        Some((_, behind)) if behind > 0 => glyphs.push('⇣'),
        _ => {},
    }
    glyphs
}

pub fn age(since: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(since).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

pub fn frame(now: SystemTime) -> usize {
    let since = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let turns = since.as_millis() / SPIN.as_millis();
    let frames = u128::try_from(SPINNER.len()).expect("a spinner of ten frames");
    usize::try_from(turns % frames).expect("a frame of the spinner's")
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
    let took = text.strip_prefix(kept)?.chars().filter(|c| !c.is_control()).collect();
    Some(CutAt { line, column: column + cells(kept), took })
}

struct RowLines<'a> {
    lines: Vec<Line<'static>>,
    number: Option<NumberAt<'a>>,
    cuts: Vec<CutAt>,
}

fn row_lines(row: &Row, width: usize, frame: usize, now: SystemTime) -> RowLines<'_> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match row {
        Row::Machine { name, away, heads: Heads::Nothing, .. } => {
            let (word, since) = away.as_ref().map_or((NO_PROJECTS, None), Away::said);
            let age = since.map_or_default(|since| age(since, now));
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            let whole = format!("{name} · {word}");
            let room = width.saturating_sub(MACHINE_LEAD + reserve + 1);
            let said = cut(&whole, room);
            let gap = width.saturating_sub(MACHINE_LEAD + said.width() + age.width() + 1);
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
            let cuts = cut_at(0, MACHINE_LEAD, &whole, room).into_iter().collect();
            RowLines { lines: vec![line], number: None, cuts }
        },
        Row::Machine {
            name, away, heads: Heads::Projects { folded, here, waiting, dot, busy }, ..
        } => {
            let badge = badge(*waiting, *folded && *dot, *folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let (word, since) = away.as_ref().map(Away::said).unzip();
            let word = word.map_or_default(|word| format!(" · {word}"));
            let age = since.flatten().map_or_default(|since| age(since, now));
            let aged = if age.is_empty() { 0 } else { age.width() + 1 };
            let room = width.saturating_sub(MACHINE_LEAD + 1 + aged + reserve + 1);
            let cuts = cut_at(0, MACHINE_LEAD, name, room).into_iter().collect();
            let bold = Style::new().add_modifier(Modifier::BOLD);
            let shown = cut(name, room);
            let word = cut(&word, room.saturating_sub(shown.width()));
            let said = shown.width() + word.width();
            assert!(said <= room, "a machine's name and word fit their room");
            let rule = room - said;
            let rule_style = if *here && *folded { Style::new().fg(ACCENT) } else { dim };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if *folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
                Span::styled(COMPUTER, bold),
                Span::raw(" "),
                Span::styled(shown, bold),
                Span::styled(word, Style::new().fg(ERROR)),
                Span::raw(" "),
                Span::styled(MACHINE_RULE.repeat(rule), rule_style),
            ];
            if !age.is_empty() {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(age, dim));
            }
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines { lines: vec![Line::from(spans)], number: None, cuts }
        },
        Row::Project { project, hits, under, here, colour, waiting, dot, busy, .. } => {
            let session = project.listed();
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let chip = Style::new().fg(gruvbox::BG0).bg(*colour);
            let fixed = 4 + reserve + 1;
            let room = width.saturating_sub(fixed);
            let bold = chip.add_modifier(Modifier::BOLD);
            let (name, named) = lit(session.name.as_str(), room, Keep::Start, hits, bold);
            let cuts = cut_at(0, 3, session.name.as_str(), room).into_iter().collect();
            let rule = room.saturating_sub(named);
            let rule_style = if *here { Style::new().fg(ACCENT) } else { dim };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if folded { FOLDED } else { UNFOLDED }, dim),
                Span::styled(" ", chip),
            ];
            spans.extend(name);
            spans.push(Span::styled(" ", chip));
            spans.push(Span::styled(RULE.repeat(rule), rule_style));
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines { lines: vec![Line::from(spans)], number: None, cuts }
        },
        Row::Worktree {
            name, missing, hits, under, here, colour, waiting, dot, busy, checkout, ..
        } => {
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let gone = if *missing { MISSING } else { "" };
            let fixed = WORKTREE + gone.width() + reserve + 1;
            let room = width.saturating_sub(fixed);
            let style = Style::new().fg(*colour).add_modifier(Modifier::BOLD);
            let (shown, wide) = lit(name, room, Keep::Start, hits, style);
            let mut cuts: Vec<CutAt> = cut_at(0, WORKTREE, name, room).into_iter().collect();
            let gap = width.saturating_sub(WORKTREE + wide + gone.width() + reserve + 1);
            let mut spans = vec![
                margin(*here),
                Span::raw(" "),
                Span::styled(if folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
            ];
            spans.extend(shown);
            spans.push(Span::styled(gone, Style::new().fg(ERROR)));
            spans.push(Span::raw(" ".repeat(gap)));
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            let mut lines = vec![Line::from(spans)];
            let mut number = None;
            if let Some(checkout) = checkout {
                let at = lines.len();
                let (drawn, columns, branch_room) =
                    checkout_line(*here, Span::raw(" "), checkout, width);
                lines.push(drawn);
                cuts.extend(cut_at(at, LABEL, &checkout.branch, branch_room));
                let link = checkout.pull_request.as_ref().and_then(Numbered::link);
                let number_at = |(columns, link)| NumberAt { line: at, columns, link };
                number = columns.zip(link).map(number_at);
            }
            RowLines { lines, number, cuts }
        },
        Row::Pane { here, unread, marked, icon, label, hits, bracket, speaks, .. } => {
            let mark = marked.as_ref().map(|marked| &marked.mark);
            let since = marked.as_ref().and_then(|marked| marked.since);
            let age = since.map_or_default(|since| age(since, now));
            let speaker = if *speaks { aloud::MARK.width() + 1 } else { 0 };
            let reserve = speaker + if age.is_empty() { 0 } else { age.width() + 1 };
            let label_style = match mark {
                Some(Mark::Waiting) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(_) => Style::new(),
                None => dim,
            };
            let mut spans = vec![margin(*here), gutter(*bracket)];
            let dotted = dotted(*unread, marked.as_ref());
            spans.push(if dotted { dot() } else { Span::raw(" ") });
            match mark {
                Some(mark) => {
                    let (glyph, style) = mark.glyph(frame);
                    spans.push(Span::styled(glyph, style));
                },
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
            let gap = width.saturating_sub(LABEL + wide + speaker + age.width() + 1);
            spans.extend(shown);
            spans.push(Span::raw(" ".repeat(gap)));
            if *speaks {
                spans.push(Span::styled(aloud::MARK, Style::new().fg(ACCENT)));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(age, dim));
            RowLines { lines: vec![Line::from(spans)], number: None, cuts }
        },
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
    (spans, shown.width())
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
    const ALL: [Colour; 6] =
        [Colour::Blue, Colour::Yellow, Colour::Purple, Colour::Aqua, Colour::Orange, Colour::Green];
}

fn colour_of(project: ProjectId) -> Color {
    let count = u64::try_from(Colour::ALL.len()).expect("six colours");
    let at = usize::try_from(project.get() % count).expect("an index below six");
    hue(Colour::ALL[at])
}

fn hue(colour: Colour) -> Color {
    match colour {
        Colour::Blue => gruvbox::BLUE,
        Colour::Yellow => gruvbox::YELLOW,
        Colour::Purple => gruvbox::PURPLE,
        Colour::Aqua => gruvbox::AQUA,
        Colour::Orange => gruvbox::ORANGE,
        Colour::Green => gruvbox::GREEN,
    }
}

fn margin(here: bool) -> Span<'static> {
    if here {
        Span::styled(HERE_MARK, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        Span::raw(" ")
    }
}

fn gutter(bracket: Option<(Bracket, Color)>) -> Span<'static> {
    let Some((sits, colour)) = bracket else {
        return Span::raw(" ");
    };
    let glyph = match sits {
        Bracket::First => BRACKET_TOP,
        Bracket::Last => BRACKET_FOOT,
        Bracket::Inner => BRACKET_SIDE,
    };
    Span::styled(glyph, Style::new().fg(colour))
}

fn checkout_line(
    here: bool,
    gutter: Span<'static>,
    checkout: &Checkout,
    width: usize,
) -> (Line<'static>, Option<Range<usize>>, usize) {
    let status = &checkout.status;
    let changed = checkout.changed.map(changed_shown);
    let number = checkout
        .pull_request
        .as_ref()
        .map(|numbered| Span::styled(numbered.shown(), Style::new().fg(numbered.colour)));
    let numbered = checkout.pull_request.as_ref();
    let mark = numbered.and_then(|it| Some(verdict_mark(it.checks?, it.stale)));
    let held = changed.as_ref().map_or(0, |(_, wide)| wide + 1)
        + number.as_ref().map_or(0, |number| number.content.width() + 1)
        + mark.as_ref().map_or(0, Span::width);
    let reserve = if status.is_empty() { 0 } else { status.width() + 1 };
    let room = width.saturating_sub(LABEL + held + reserve + 1);
    let branch = cut(&checkout.branch, room);
    let gap = width.saturating_sub(LABEL + branch.width() + held + status.width() + 1);
    let mut spans = vec![
        margin(here),
        gutter,
        Span::raw(" ".repeat(LABEL - 2)),
        Span::styled(branch, Style::new().fg(gruvbox::GREY)),
    ];
    let mut columns = None;
    if let Some((shown, _)) = changed {
        spans.push(Span::raw(" "));
        spans.extend(shown);
    }
    if let Some(number) = number {
        spans.push(Span::raw(" "));
        let start: usize = spans.iter().map(Span::width).sum();
        columns = Some(start..start + number.width());
        spans.push(number);
        spans.extend(mark);
    }
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::raw(status.clone()));
    (Line::from(spans), columns, room)
}

pub fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut taken = 0;
    for shown in text.graphemes(true) {
        let width = shown.width();
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
    if text.width() <= room {
        return text.to_owned();
    }
    let mut start = text.len();
    let mut taken = 0;
    for (at, shown) in text.grapheme_indices(true).rev() {
        let width = shown.width();
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
}

impl Notes {
    fn say(&mut self, text: &str, width: usize, style: Style) {
        let lines = wrapped(text, width, style);
        let start = self.lines.len();
        self.goes_on.extend(start + 1..start + lines.len());
        self.lines.extend(lines);
        self.said.push(text.to_owned());
    }
}

pub fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for shown in text.graphemes(true) {
        if line.width() + shown.width() > room {
            lines.push(Line::styled(format!(" {line}"), style));
            line.clear();
        }
        line.push_str(shown);
    }
    if !line.is_empty() {
        lines.push(Line::styled(format!(" {line}"), style));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::sync::LazyLock;

    use lunetad::api::shell::{ended, Ended, Going, Stopped};
    use lunetad::api::session::Mux;
    use lunetad::api::{
        self, Clients, NameSource, Pane, Panes, Session, UnreadFile, Version, Zellij,
    };
    use lunetad::mux::own::{OwnId, OwnMade};
    use lunetad::mux::TabId;
    use prost_types::Timestamp;
    use ratatui::buffer::Cell;

    use super::*;
    use crate::pull::{At, Readied, Ready};
    use crate::spots::Hit;

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn pane(id: u32, tab: u32, cwd: &str, command: &str) -> Pane {
        Pane {
            id,
            tab,
            tab_name: format!("Tab #{}", tab + 1),
            cwd: Some(cwd.to_owned()),
            command: Some(command.to_owned()),
            titled: None,
            shell: None,
            tab_id: Some(tab),
            held: false,
            key: None,
        }
    }

    fn session(name: &str, started: i64, panes: Vec<Pane>) -> Session {
        Session {
            name: name.to_owned(),
            started_at: Some(Timestamp { seconds: started, nanos: 0 }),
            panes: Some(Panes { panes }),
            clients: None,
            socket: started as u64,
            mux: None,
        }
    }

    fn own_session(name: &str, id: u64, mut panes: Vec<Pane>) -> Session {
        let made_at = Some(Timestamp::default());
        for pane in &mut panes {
            pane.key = Some(api::TerminalKey { id: pane.id, made_at });
        }
        Session {
            name: name.to_owned(),
            started_at: None,
            panes: Some(Panes { panes }),
            clients: Some(Clients { clients: Vec::new() }),
            socket: 0,
            mux: Some(Mux::Own(api::OwnSession { id, made_at })),
        }
    }

    fn in_own(agent: Agent, id: u64, pane: u32) -> Agent {
        let made_at = Some(Timestamp::default());
        let session = Some(api::OwnSession { id, made_at });
        Agent { pane: Some(api::agent::Pane::Own(api::Own { session, pane })), ..agent }
    }

    fn listed(name: &str, started: u64) -> Listed {
        Listed::new_project(name, started)
    }

    fn pane_of(name: &str, started: u64, pane: u32) -> Place {
        Place::Pane(PanePlace { session: listed(name, started), pane: PaneKey::terminal(pane) })
    }

    fn own_listed(name: &str, id: u64) -> Listed {
        Listed::new_project(name, id)
    }

    fn own_pane_of(name: &str, id: u64, pane: u32) -> Place {
        let session = Listed::new_project(name, id);
        Place::Pane(PanePlace { session, pane: PaneKey::terminal(pane) })
    }

    fn session_name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    fn closes_here(menu: u64, session: Listed, pane: PaneKey) -> Asked {
        Asked::Closes { menu, on: Machine::Local, session, pane }
    }

    fn delete_here(act: Act) -> Asked {
        Asked::Delete { on: Machine::Local, act }
    }

    fn project_deleted(name: &str, id: u64, worktrees: &[u64], seen: &[u64]) -> Act {
        let key = OwnKey { id: OwnId::new(id).unwrap(), made: OwnMade::from_nanos(0) };
        let (project, worktrees) = (ProjectId::of(id).unwrap(), worktrees.iter());
        let worktrees = worktrees.map(|it| WorktreeId::of(*it).unwrap()).collect();
        let seen = seen.iter().map(|it| ViewId::of(*it).unwrap()).collect();
        Act::Project { session: session_name(name), key, project, worktrees, seen }
    }

    fn go_here(place: Place) -> Asked {
        Asked::Go { on: Machine::Local, place }
    }

    fn fresh() -> Sidebar {
        Sidebar { this: "mac".to_owned(), ..Sidebar::default() }
    }

    fn cursor(sidebar: &Sidebar) -> Option<Place> {
        match sidebar.cursor.as_ref()? {
            Spot::Local(place) => Some(place.clone()),
            Spot::Machine(_) | Spot::Remote(..) => None,
        }
    }

    fn folded(sidebar: &Sidebar, name: &str) -> bool {
        sidebar.folds.contains(Fold::Project(&Machine::Local, project_named(sidebar, name)))
    }

    fn fold(sidebar: &mut Sidebar, name: &str) {
        let project = project_named(sidebar, name);
        sidebar.folds.set(Fold::Project(&Machine::Local, project), true);
    }

    fn project_row(name: &str, id: u64) -> ProjectRow {
        ProjectRow::of(listed(name, id)).unwrap()
    }

    fn projects(seen: &[u64], wanted: &[u64]) -> Reorder {
        let ids = |ids: &[u64]| ids.iter().map(|id| ProjectId::of(*id).unwrap()).collect();
        Reorder::Projects { seen: ids(seen), wanted: ids(wanted) }
    }

    fn project_named(sidebar: &Sidebar, name: &str) -> ProjectId {
        let mut known = shelf(&sidebar.shelves, &Machine::Local).iter();
        let known = known.find(|known| known.listed().name.as_str() == name).expect("a project");
        known.listed().project().unwrap()
    }

    fn agent(session: &str, pane: u32, status: Status, name: Option<&str>) -> Agent {
        Agent {
            pid: pane,
            harness: Some(Harness::ClaudeCode.into()),
            session_id: format!("id-{pane}"),
            cwd: "/w/luneta/main".to_owned(),
            status: Some(Reported::KnownStatus(status.into())),
            status_since: Some(Timestamp { seconds: 100, nanos: 0 }),
            started_at: Some(Timestamp { seconds: 50, nanos: 0 }),
            name: name.map(|text| api::Name { text: text.to_owned(), source: None }),
            entrypoint: Some("cli".to_owned()),
            pane: Some(api::agent::Pane::Zellij(Zellij {
                named: session.to_owned(),
                session: Some(session.to_owned()),
                pane,
            })),
            waiting_for: None,
            topic: None,
        }
    }

    trait Hears {
        fn hear(&mut self, host: Host);
    }

    impl Hears for Sidebar {
        fn hear(&mut self, mut host: Host) {
            seat_files(&host.sessions, &mut host.unread_files);
            if host.projects.is_none() || !host.sessions.is_empty() {
                host.projects = Some(projects_of(&host.sessions, &mut host.agents));
            }
            for remote in &mut host.remotes {
                if let Some(Reached::Host(far)) = &mut remote.reached
                    && (far.projects.is_none() || !far.sessions.is_empty())
                {
                    far.projects = Some(projects_of(&far.sessions, &mut far.agents));
                    seat_files(&far.sessions, &mut far.unread_files);
                }
            }
            self.heard(host);
        }
    }

    fn tab_of(project: u64, tab: u32) -> WorktreeId {
        WorktreeId::of(project * 1000 + u64::from(tab)).unwrap()
    }

    fn project_id(session: &Session) -> u64 {
        match &session.mux {
            Some(Mux::Own(own)) => own.id,
            Some(Mux::Zellij(_)) | None => session.socket,
        }
    }

    fn seat_files(sessions: &[Session], files: &mut [UnreadFile]) {
        for file in files {
            let Some(zellij) = file.zellij.take() else {
                continue;
            };
            let Some(session) = sessions.iter().find(|it| it.name == zellij.named) else {
                file.zellij = Some(zellij);
                continue;
            };
            let made_at = Some(Timestamp::default());
            let own = api::OwnSession { id: project_id(session), made_at };
            file.own = Some(api::Own { session: Some(own), pane: zellij.pane });
        }
    }

    fn projects_of(sessions: &[Session], agents: &mut [Agent]) -> api::Projects {
        let mut sessions: Vec<&Session> = sessions.iter().collect();
        sessions.sort_by_key(|session| {
            let started = session.started_at.filter(|_| !matches!(session.mux, Some(Mux::Own(_))));
            (started.map(|at| at.seconds), session.name.clone())
        });
        let made_at = Some(Timestamp::default());
        for agent in agents.iter_mut() {
            let Some(api::agent::Pane::Zellij(zellij)) = &agent.pane else {
                continue;
            };
            let Some(session) = sessions.iter().find(|it| it.name == zellij.named) else {
                continue;
            };
            let own = api::OwnSession { id: project_id(session), made_at };
            let own = api::Own { session: Some(own), pane: zellij.pane };
            agent.pane = Some(api::agent::Pane::Own(own));
        }
        let project = |session: &&Session| {
            let mut worktrees: Vec<api::Worktree> = Vec::new();
            let panes = session.panes.iter().flat_map(|panes| &panes.panes);
            let mut panes: Vec<&Pane> = panes.collect();
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            for pane in panes {
                let terminal = api::Terminal {
                    id: u64::from(pane.id),
                    found: Some(api::terminal::Found::Key(api::TerminalKey {
                        id: pane.id,
                        made_at,
                    })),
                    folder: pane.cwd.clone().unwrap_or_default(),
                    command: pane.command.clone(),
                    titled: pane.titled.clone(),
                    shell: pane.shell.clone(),
                    held: pane.held,
                    name: None,
                };
                let id = tab_of(project_id(session), pane.tab).get();
                match worktrees.iter_mut().find(|it| it.id == id) {
                    Some(worktree) => worktree.terminals.push(terminal),
                    None => worktrees.push(api::Worktree {
                        id,
                        name: pane.tab_name.clone(),
                        folder: kept_folder(pane.cwd.as_deref()),
                        seen: Some(api::worktree::Seen::Unread(())),
                        terminals: vec![terminal],
                        ..api::Worktree::default()
                    }),
                }
            }
            api::Project {
                id: project_id(session),
                name: session.name.clone(),
                dir: "/w".to_owned(),
                missing: false,
                holds: Some(api::project::Holds::Clone(clone_of(worktrees))),
            }
        };
        let projects = sessions.iter().map(project).collect();
        let kept = api::projects::Kept { projects, version: None };
        api::Projects { held: Some(api::projects::Held::Kept(kept)) }
    }

    fn host(sessions: Vec<Session>, agents: Vec<Agent>) -> Host {
        Host {
            version: Some(Version { pid: 1, seq: 1 }),
            sessions,
            git_statuses: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_sockets: Some(0),
            unread_files: Vec::new(),
            remotes: Vec::new(),
            unread_hosts: Some(0),
            sayings: Vec::new(),
            projects: None,
        }
    }

    fn drawn(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Buffer {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        drawn_at(sidebar, focused, width, height, now)
    }

    fn drawn_at(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
        now: SystemTime,
    ) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        sidebar.focus(if focused { Keys::Sidebar } else { Keys::Zellij });
        sidebar.draw(&mut buffer, area, now);
        let lit: &[Color] = if focused { &[LIT, RESTING] } else { &[RESTING] };
        in_gruvbox(&buffer, &[&[Color::Reset], lit].concat());
        buffer
    }

    fn collapsed(sidebar: &mut Sidebar, height: u16, now: SystemTime) -> Buffer {
        let area = Rect::new(0, 0, COLLAPSED, height);
        let mut buffer = Buffer::empty(area);
        sidebar.draw_collapsed(&mut buffer, area, now);
        in_gruvbox(&buffer, &[Color::Reset]);
        buffer
    }

    fn in_gruvbox(buffer: &Buffer, backgrounds: &[Color]) {
        for cell in buffer.content() {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
            let named = Colour::ALL.map(hue).contains(&cell.bg) && cell.fg == gruvbox::BG0;
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
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        seen_at(sidebar, focused, width, height, now)
    }

    fn seen_at(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
        now: SystemTime,
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
        sidebar.hear(host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session(
                    "older",
                    10,
                    vec![
                        pane(2, 1, "/w/c", "nvim src/main.rs"),
                        pane(1, 0, "/w/luneta/main", "claude"),
                    ],
                ),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        ));
        sidebar
    }

    fn own_two_sessions() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![
                own_session("young", 2, vec![pane(1, 0, "/w/b", "fish")]),
                own_session(
                    "older",
                    1,
                    vec![
                        pane(2, 1, "/w/c", "nvim src/main.rs"),
                        pane(1, 0, "/w/luneta/main", "claude"),
                    ],
                ),
            ],
            vec![in_own(agent("older", 1, Status::Waiting, Some("fold")), 1, 1)],
        ));
        sidebar
    }

    fn owned(mut sidebar: Sidebar) -> Sidebar {
        let mut host = sidebar.host.take().expect("heard");
        let mut made = Vec::new();
        for (at, session) in host.sessions.iter_mut().enumerate() {
            let id = u64::try_from(at).unwrap() + 1;
            made.push((session.name.clone(), id));
            let panes = session.panes.take().map_or_else(Vec::new, |panes| panes.panes);
            *session = own_session(&session.name, id, panes);
        }
        for agent in &mut host.agents {
            if let Some(api::agent::Pane::Zellij(zellij)) = agent.pane.clone() {
                let named = made.iter().find(|(name, ..)| *name == zellij.named);
                let (_, id) = named.expect("the agent's session");
                *agent = in_own(agent.clone(), *id, zellij.pane);
            }
        }
        sidebar.hear(host);
        sidebar
    }

    fn sit(sidebar: &mut Sidebar, place: &Place) {
        let key = place.listed().own_key().expect("an own session");
        let pane = OwnPane::new(key, place.pane().expect("a pane")).expect("a pane");
        if sidebar.own_seat().is_none() {
            sidebar.attaching(Machine::Local, key);
        }
        sidebar.went_own(pane, OwnClientId::new(1).unwrap());
    }

    fn kept(projects: Vec<api::Project>) -> api::Projects {
        let kept = api::projects::Kept { projects, version: None };
        api::Projects { held: Some(api::projects::Held::Kept(kept)) }
    }

    fn worktree_of(id: u64, name: &str, folder: &str, panes: &[u32]) -> api::Worktree {
        let made_at = Some(Timestamp::default());
        let terminal = |id: &u32| api::Terminal {
            id: u64::from(*id),
            found: Some(api::terminal::Found::Key(api::TerminalKey { id: *id, made_at })),
            folder: folder.to_owned(),
            command: Some("fish".to_owned()),
            ..api::Terminal::default()
        };
        api::Worktree {
            id,
            name: name.to_owned(),
            folder: folder.to_owned(),
            seen: Some(api::worktree::Seen::Unread(())),
            terminals: panes.iter().map(terminal).collect(),
            ..api::Worktree::default()
        }
    }

    fn kept_folder(cwd: Option<&str>) -> String {
        let kept = cwd.filter(|cwd| Dir::parse(cwd).is_ok());
        kept.unwrap_or("/w").to_owned()
    }

    fn clone_of(worktrees: Vec<api::Worktree>) -> api::project::Clone {
        api::project::Clone { worktrees, listing: None }
    }

    fn a_project(id: u64, name: &str, worktrees: Vec<api::Worktree>) -> api::Project {
        let (name, dir) = (name.to_owned(), "/w".to_owned());
        let holds = Some(api::project::Holds::Clone(clone_of(worktrees)));
        api::Project { id, name, dir, missing: false, holds }
    }

    #[test]
    fn an_empty_project_is_one_row_and_an_empty_worktree_is_one_to_go_to() {
        let mut sidebar = fresh();
        let fresh_one = worktree_of(31, "fresh", "/w/solo", &[]);
        let projects =
            vec![a_project(7, "empty", Vec::new()), a_project(8, "solo", vec![fresh_one])];
        sidebar.heard(Host { projects: Some(kept(projects)), ..host(Vec::new(), Vec::new()) });
        assert_eq!(rows_shown(&mut sidebar), [" ▾ empty", " ▾ solo", "  ▾ fresh"]);
        sidebar.focus(Keys::Sidebar);
        let empty = Place::Session(listed("empty", 7));
        assert_eq!(cursor(&sidebar), Some(empty.clone()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(empty));
        type_in(&mut sidebar, "j");
        let worktree = WorktreeId::of(31).unwrap();
        let solo = project_row("solo", 8);
        let place = Place::Worktree(WorktreePlace { project: solo, worktree });
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(place));
    }

    #[test]
    fn space_or_a_click_folds_an_empty_row_itself_and_h_folds_what_holds_it() {
        let mut sidebar = fresh();
        let solo = a_project(8, "solo", vec![worktree_of(31, "fresh", "/w/solo", &[])]);
        let projects = vec![a_project(7, "empty", Vec::new()), solo];
        sidebar.heard(Host { projects: Some(kept(projects)), ..host(Vec::new(), Vec::new()) });
        sidebar.focus(Keys::Sidebar);
        let (empty, tab) = (ProjectId::of(7).unwrap(), WorktreeId::of(31).unwrap());
        let folded = |sidebar: &Sidebar| {
            let on = &Machine::Local;
            [Fold::Machine(on), Fold::Project(on, empty), Fold::Worktree(on, tab)]
                .map(|fold| sidebar.folds.contains(fold))
        };
        type_in(&mut sidebar, " ");
        assert_eq!(folded(&sidebar), [false, true, false]);
        type_in(&mut sidebar, " j ");
        assert_eq!(folded(&sidebar), [false, false, true]);
        let area = Rect::new(0, 0, 32, 10);
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert_eq!(folded(&sidebar), [false, true, true]);
        type_in(&mut sidebar, "h");
        assert_eq!(folded(&sidebar), [true, true, true]);
    }

    #[test]
    fn a_worktree_s_row_deletes_the_worktree_with_its_panes() {
        let mut sidebar = fresh();
        let fix = worktree_of(31, "fix", "/w/fix", &[1, 2]);
        let main = worktree_of(32, "main", "/w/main", &[3]);
        let projects = vec![a_project(7, "luneta", vec![fix, main])];
        sidebar.heard(Host { projects: Some(kept(projects)), ..host(Vec::new(), Vec::new()) });
        sidebar.focus(Keys::Sidebar);
        let worktree = WorktreeId::of(31).unwrap();
        let project = project_row("luneta", 7);
        let place = Place::Worktree(WorktreePlace { project, worktree });
        sidebar.cursor = Some(Spot::Local(place));
        type_in(&mut sidebar, "d");
        let whose = listed("luneta", 7).whose();
        let session = session_name("luneta");
        let seen = [1, 2].map(|id| ViewId::of(id).unwrap()).to_vec();
        let act = Act::Worktree { session, whose, worktree, panes: vec![1, 2], seen };
        let asked = sidebar.key(&char('d'), moment(160_000));
        assert_eq!(asked, Asked::Delete { on: Machine::Local, act });
        let after = sidebar.closing.as_ref().and_then(|closing| closing.after.clone());
        let past = "the cursor goes past the worktree's panes";
        assert_eq!(after, Some(Spot::Local(pane_of("luneta", 7, 3))), "{past}");
    }

    #[test]
    fn the_fold_of_a_worktree_lunetad_no_longer_keeps_goes_with_it() {
        let mut sidebar = two_sessions();
        let (kept, gone) = (tab_of(10, 0), tab_of(10, 1));
        for tab in [kept, gone] {
            sidebar.folds.set(Fold::Worktree(&Machine::Local, tab), true);
        }
        let older = session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]);
        let newer = session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]);
        sidebar.hear(host(vec![newer, older], Vec::new()));
        assert!(sidebar.folds.contains(Fold::Worktree(&Machine::Local, kept)));
        assert!(!sidebar.folds.contains(Fold::Worktree(&Machine::Local, gone)));
        let unkept = api::projects::Held::Unkept("its file does not read".to_owned());
        let projects = Some(api::Projects { held: Some(unkept) });
        sidebar.hear(Host { projects, ..host(Vec::new(), Vec::new()) });
        let still = sidebar.folds.contains(Fold::Worktree(&Machine::Local, kept));
        assert!(still, "nor on a list cut short");
    }

    #[test]
    fn a_worktree_whose_folder_is_gone_says_so() {
        let mut sidebar = fresh();
        let mut gone = worktree_of(31, "fix", "/w/fix", &[1]);
        gone.seen = Some(api::worktree::Seen::Missing(()));
        let main = worktree_of(32, "main", "/w", &[2]);
        let projects = vec![a_project(7, "luneta", vec![gone, main])];
        sidebar.heard(Host { projects: Some(kept(projects)), ..host(Vec::new(), Vec::new()) });
        let rows = rows_shown(&mut sidebar);
        assert_eq!([rows[1].as_str(), rows[3].as_str()], ["  ▾ fix missing", "  ▾ main"]);
        let buffer = drawn(&mut sidebar, false, 32, 10);
        let missing = (0..32).find(|x| buffer[(*x, 5)].symbol() == "m").unwrap();
        assert_eq!(buffer[(missing + 1, 5)].fg, ERROR);
    }

    #[test]
    fn an_unreached_machine_shows_the_projects_lunetad_last_heard_marked() {
        let mut sidebar = fresh();
        let main = a_project(20, "main", vec![worktree_of(31, "fix", "/w/b", &[1])]);
        let mut unreached = down(100);
        if let Reached::Unreached(it) = &mut unreached {
            it.projects = Some(kept(vec![main]));
        }
        let here = session("here", 10, vec![pane(1, 0, "/w/a", "fish")]);
        let mut host = host(vec![here], Vec::new());
        host.remotes = vec![remote("nixos", unreached), remote("pi", Reached::Reaching(()))];
        sidebar.hear(host);
        let lines = screen(&mut sidebar, false, 32, 13);
        assert_eq!(
            lines[3..12],
            [
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ here ──────────────────────",
                "  ▾ Tab #1",
                "     \u{e795} w/a",
                " ▾ \u{f108} nixos · unreachable ══ 1m",
                " ▾ main ──────────────────────",
                "  ▾ fix",
                "     \u{e795} w/b",
                " ▸ \u{f108} pi · reaching…",
            ]
        );
        let buffer = drawn(&mut sidebar, false, 32, 13);
        assert_eq!([buffer[(10, 7)].fg, buffer[(13, 7)].fg], [ERROR, ERROR]);
        sidebar.focus(Keys::Sidebar);
        let why = "ssh: Could not resolve hostname vps";
        let said = format!("nixos: {why}; what ssh said is in {}", lunetad::log::NAME);
        for spot in [
            Spot::Machine(Machine::Remote(nixos())),
            Spot::Remote(nixos(), Place::Session(listed("main", 20))),
        ] {
            sidebar.cursor = Some(spot);
            assert_eq!(sidebar.unreached(), Some(said.clone()));
        }
        sidebar.cursor = Some(Spot::Local(Place::Session(listed("here", 10))));
        assert_eq!(sidebar.unreached(), None);
    }

    #[test]
    fn projects_hold_their_worktrees_and_those_their_panes() {
        let mut sidebar = two_sessions();
        assert_eq!(
            screen(&mut sidebar, false, 32, 10),
            [
                "─ luneta ──────────────────────",
                "─ 2 projects ───────────── ●1 ─",
                "",
                " ▾ \u{f108} mac ══════════════════ ●1",
                " ▾ older ────────────────── ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fold                 1m",
                "  ▾ Tab #2",
                "     \u{e6ae} nvim src/main.rs",
                " ▾ newer ─────────────────────",
            ]
        );
    }

    #[test]
    fn a_bracket_joins_the_panes_of_a_worktree() {
        let panes = vec![
            pane(1, 0, "/w/a", "less a"),
            pane(2, 1, "/w/b", "less b"),
            pane(3, 1, "/w/c", "less c"),
            pane(4, 2, "/w/c", "less d"),
            pane(5, 2, "/w/b", "less e"),
        ];
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let name = Said::new("main".to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let status = GitStatus::Checkout { repository: None, head, changes, stashes: 0 };
        host.git_statuses.push(status.to_api("/w/b"));
        let mut sidebar = fresh();
        sidebar.hear(host);

        assert_eq!(
            rows_shown(&mut sidebar),
            [
                " ▾ s",
                "  ▾ Tab #1",
                "       less a",
                "  ▾ Tab #2",
                "       main",
                " ╭     less b",
                " ╰     less c",
                "  ▾ Tab #3",
                " ╭     less d",
                " ╰     less e",
            ]
        );
        type_in(&mut sidebar, "/c");
        assert_eq!(
            rows_shown(&mut sidebar),
            [
                " ▾ s",
                "  ▾ Tab #2",
                "       main",
                "       less c",
                "  ▾ Tab #3",
                " ╭     less d",
                " ╰     less e",
            ]
        );
    }

    #[test]
    fn a_project_s_colour_comes_from_its_id_and_marks_its_panes() {
        let chip = |buffer: &Buffer, y: u16| buffer[(3, y)].bg;
        let bracket = |buffer: &Buffer, y: u16| buffer[(1, y)].fg;
        let listing = |named: &[(&str, i64)]| {
            let shared = || vec![pane(1, 0, "/w/a", "less"), pane(2, 0, "/w/a", "less")];
            let sessions = named.iter().map(|(name, at)| session(name, *at, shared()));
            host(sessions.collect(), Vec::new())
        };
        let mut sidebar = fresh();
        sidebar.hear(listing(&[("a", 1), ("b", 2), ("c", 9)]));
        let buffer = drawn(&mut sidebar, false, 32, 17);
        let (y, p, a) = (gruvbox::YELLOW, gruvbox::PURPLE, gruvbox::AQUA);
        assert_eq!([chip(&buffer, 4), chip(&buffer, 8), chip(&buffer, 12)], [y, p, a]);
        assert_eq!(buffer[(3, 4)].fg, gruvbox::BG0);
        assert_eq!([bracket(&buffer, 6), bracket(&buffer, 7)], [y, y]);
        assert_eq!([bracket(&buffer, 14), bracket(&buffer, 15)], [a, a]);

        let mut there = fresh();
        there.hear(listing(&[("e", 4), ("c", 9)]));
        let buffer = drawn(&mut there, false, 32, 17);
        assert_eq!([chip(&buffer, 4), chip(&buffer, 8)], [gruvbox::ORANGE, a]);
    }

    #[test]
    fn the_title_is_in_the_accent_while_the_sidebar_has_the_keys() {
        let mut sidebar = two_sessions();
        let look = |buffer: &Buffer, x: u16| {
            let cell = &buffer[(x, 0)];
            let has = |modifier| cell.modifier.contains(modifier);
            (cell.fg, has(Modifier::DIM), has(Modifier::BOLD))
        };
        let dim = (gruvbox::FG1, true, false);
        let accent = |bold| (ACCENT, false, bold);
        for (focused, rule, word) in
            [(false, dim, dim), (true, accent(false), accent(true)), (false, dim, dim)]
        {
            assert_eq!(screen(&mut sidebar, focused, 32, 3)[0], "─ luneta ──────────────────────");
            let buffer = drawn(&mut sidebar, focused, 32, 3);
            assert_eq!([0, 2, 30].map(|x| look(&buffer, x)), [rule, word, rule], "{focused}");
        }
    }

    #[test]
    fn a_brief_lent_the_keys_dims_the_title_and_keeps_the_highlight() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 12);
        let draw = |sidebar: &mut Sidebar| {
            let mut buffer = Buffer::empty(area);
            sidebar.draw(&mut buffer, area, moment(160_000));
            in_gruvbox(&buffer, &[Color::Reset, LIT]);
            let start = |y: u16| (0..9).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            let titled = (0..area.height).filter(|y| start(*y) == "─ luneta ").count();
            let lit: Vec<u16> =
                (0..area.height).filter(|y| buffer[(0, *y)].bg != Color::Reset).collect();
            let word = &buffer[(2, 0)];
            (titled, lit, word.fg, word.modifier.contains(Modifier::DIM))
        };
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), moment(160_000));
        sidebar.key(&KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE), moment(160_000));
        let cursor = sidebar.cursor.clone();
        let (titled, lit, fg, dim) = draw(&mut sidebar);
        assert_eq!((titled, fg, dim, lit.is_empty()), (2, ACCENT, false, false));
        sidebar.focus(Keys::Lent);
        assert_eq!(draw(&mut sidebar), (1, lit.clone(), gruvbox::FG1, true));
        sidebar.focus(Keys::Sidebar);
        assert_eq!(draw(&mut sidebar), (2, lit, ACCENT, false));
        assert_eq!(sidebar.cursor, cursor);
    }

    #[test]
    fn the_top_rule_counts_the_agents_the_rows_hold() {
        let mut sidebar = fresh();
        let mut outside = agent("s", 4, Status::Waiting, None);
        outside.pane = None;
        let panes = (1..=3).map(|id| pane(id, 0, "/w/a", "claude")).collect();
        sidebar.hear(host(
            vec![session("s", 10, panes)],
            vec![
                agent("s", 1, Status::Busy, None),
                agent("s", 2, Status::Shell, None),
                agent("s", 3, Status::Shell, None),
                outside,
            ],
        ));
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        let rule = screen(&mut sidebar, false, 32, 4)[1].clone();
        assert_eq!((rule.as_str(), sidebar.turning()), ("─ 1 project ─────────── ⠋1 $2 ─", true));
        let buffer = drawn(&mut sidebar, false, 32, 4);
        let at = |glyph: char| rule.chars().position(|c| c == glyph).expect("on the rule") as u16;
        let dim = |x: u16| buffer[(x, 1)].modifier.contains(Modifier::DIM);
        let (spinner, dollar) = (at('⠋'), at('$'));
        assert_eq!([spinner, spinner + 1, dollar, dollar + 1].map(dim), [false, false, true, true]);
    }

    #[test]
    fn collapsed_the_sidebar_keeps_the_dots_the_work_and_the_shells() {
        let mut sidebar = fresh();
        let mut panes: Vec<Pane> = (1..=103).map(|id| pane(id, 0, "/w/a", "claude")).collect();
        panes.push(in_shell(104, going("make", 159, Front::Working)));
        let mut agents =
            vec![agent("s", 1, Status::Idle, None), agent("s", 2, Status::Waiting, None)];
        agents.extend((3..=103).map(|id| agent("s", id, Status::Shell, None)));
        sidebar.hear(host(vec![session("s", 10, panes)], agents));
        let strip = |sidebar: &mut Sidebar, now| {
            let buffer = collapsed(sidebar, 6, now);
            let line = |y| (0..COLLAPSED - 1).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            (0..6).map(|y| line(y).trim_end().to_owned()).collect::<Vec<_>>()
        };
        assert_eq!(strip(&mut sidebar, moment(160_500)), ["───", "", "•1", "$1…", "", ""]);
        assert_eq!(strip(&mut sidebar, moment(161_000)), ["───", "", "•1", "⠋1", "$1…", ""]);
    }

    #[test]
    fn collapsed_the_sidebar_reddens_its_rule_while_lunetad_does_not_answer() {
        let mut sidebar = fresh();
        sidebar.hear(idle_agent());
        let look = |buffer: &Buffer, x: u16, y: u16| {
            let cell = &buffer[(x, y)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let rule = |sidebar: &mut Sidebar| look(&collapsed(sidebar, 3, moment(160_000)), 0, 0);
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 0, 1);
        assert_eq!(rule(&mut sidebar), open);
        sidebar.lost("gone".to_owned());
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
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 0, 1);
        assert_eq!(rule(&mut sidebar), open);
        sidebar.image_going(Some("far".parse().unwrap()));
        let buffer = drawn(&mut sidebar, false, COLUMNS, 6);
        assert_eq!(buffer[(1, 5)].symbol(), "i");
        let note = look(&buffer, 1, 5);
        assert_ne!(note, open);
        assert_eq!(rule(&mut sidebar), note);
        sidebar.lost("gone".to_owned());
        assert_eq!(rule(&mut sidebar).0, ERROR);
        sidebar.hear(idle_agent());
        sidebar.image_going(None);
        assert_eq!(rule(&mut sidebar), open);
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
        let dim = ("│".to_owned(), gruvbox::FG1, Color::Reset, Modifier::DIM);
        let open = drawn(&mut sidebar, true, COLUMNS, 10);
        assert!((0..10).any(|y| open[(0, y)].bg == LIT), "a row is lit");
        assert_eq!(edge(&open), vec![accent; 10]);
        sidebar.focus(Keys::Lent);
        let area = Rect::new(0, 0, COLUMNS, 10);
        let mut lent = Buffer::empty(area);
        sidebar.draw(&mut lent, area, moment(160_000));
        assert_eq!(edge(&lent), vec![dim.clone(); 10]);
        assert_eq!(edge(&drawn(&mut sidebar, false, COLUMNS, 10)), vec![dim.clone(); 10]);
        assert_eq!(edge(&collapsed(&mut sidebar, 10, moment(160_000))), vec![dim; 10]);
    }

    #[test]
    fn a_project_of_id_0_or_a_refused_name_is_left_out_and_said() {
        let mut sidebar = fresh();
        let mut unstarted = session("x", 0, vec![]);
        unstarted.started_at = None;
        let mut unsocketed = session("z", 30, vec![]);
        unsocketed.socket = 0;
        let misnamed = session("a/b", 40, vec![]);
        let long = session(&"w".repeat(66), 50, vec![]);
        let sessions = vec![unstarted, session("y", 20, vec![]), unsocketed, misnamed, long];
        sidebar.hear(host(sessions, Vec::new()));
        let lines = screen(&mut sidebar, false, 60, 9);
        assert_eq!(lines[1], format!("─ 2 projects {}", "─".repeat(46)));
        assert_eq!(lines[4], format!(" ▾ y {}", "─".repeat(53)));
        assert!(lines[5].starts_with(" ▾ wwww"), "{:?}", lines[5]);
        assert_eq!(lines[7], " lunetad: 3 projects with an id of 0 or a name luneta refu");
        assert_eq!(lines[8], " ses, not listed");
    }

    #[test]
    fn the_highlight_rests_on_your_pane_until_the_sidebar_has_the_focus() {
        let mut sidebar = fresh();
        let mut host = host(
            vec![
                own_session("young", 1, vec![pane(1, 0, "/w/b", "fish")]),
                own_session(
                    "older",
                    2,
                    vec![pane(2, 1, "/w/c", "nvim src/main.rs"), pane(1, 0, "/w/a", "fish")],
                ),
            ],
            Vec::new(),
        );
        let changes = Some(api::Changes::default());
        let head = Some(api::checkout::Head::Branch(api::Branch { name: "main".to_owned(), upstream: None, changed: None }));
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/c".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        sit(&mut sidebar, &own_pane_of("older", 2, 2));

        let nvim = "▌    \u{e6ae} nvim src/main.rs";
        let (lines, lit) = seen(&mut sidebar, false, 32, 13);
        assert_eq!(lines[7..10], ["  ▾ Tab #2", "       main", nvim]);
        assert_eq!(lit, [7, 8, 9]);
        let (_, lit) = seen(&mut sidebar, true, 32, 7);
        assert_eq!(lit, [4, 5, 6]);
        sidebar.key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), moment(160_000));
        let (lines, lit) = seen(&mut sidebar, true, 32, 7);
        assert_eq!((lines[6].as_str(), lit.as_slice()), (nvim, &[3][..]));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [4, 5, 6]);
        let fish = own_pane_of("older", 2, 1);
        assert_eq!(click(&mut sidebar, 10, 3, Rect::new(0, 0, 32, 14)), go_here(fish));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [4, 5, 6]);
    }

    #[test]
    fn the_highlight_skips_a_session_s_row_and_goes_no_further_than_the_ends() {
        let mut sidebar = two_sessions();
        let older = Some(pane_of("older", 10, 1));
        let newer = Some(pane_of("newer", 20, 1));
        assert_eq!(cursor(&sidebar), older);
        sidebar.step(1);
        sidebar.step(1);
        assert_eq!(cursor(&sidebar), newer);
        sidebar.step(1);
        assert_eq!(cursor(&sidebar), newer);
        sidebar.step(-5);
        assert_eq!(cursor(&sidebar), older);
    }

    #[test]
    fn g_and_end_go_to_the_last_row_and_gg_and_home_to_the_first() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let first = Some(pane_of("older", 10, 1));
        let second = Some(pane_of("older", 10, 2));
        let last = Some(pane_of("newer", 20, 1));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        let shift_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        let g = key(KeyCode::Char('g'));
        sidebar.key(&shift_g, moment(160_000));
        assert_eq!(cursor(&sidebar), last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), first);
        sidebar.key(&key(KeyCode::End), moment(160_000));
        assert_eq!(cursor(&sidebar), last);
        sidebar.key(&key(KeyCode::Home), moment(160_000));
        assert_eq!(cursor(&sidebar), first);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        for pressed in [key(KeyCode::End), g, key(KeyCode::Char('k')), g, ctrl_x, g] {
            sidebar.key(&pressed, moment(160_000));
        }
        assert_eq!(cursor(&sidebar), second);
        let area = Rect::new(0, 0, 32, 11);
        screen(&mut sidebar, true, area.width, area.height);
        sidebar.mouse(mouse(MouseEventKind::ScrollUp, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), second);
        sidebar.mouse(mouse(MouseEventKind::Moved, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), first);
        sidebar.key(&g, moment(160_000));
        assert_eq!(click(&mut sidebar, 1, 9, area), Asked::Take);
        let folded = Some(Place::Session(listed("newer", 20)));
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), folded);
        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), folded);
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
                session("y", 20, vec![pane(1, 0, "/w/y", "fish")]),
                session("x", 30, vec![]),
                session("z", 40, vec![]),
            ],
            Vec::new(),
        ));
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        for _ in 0..3 {
            sidebar.key(&down, moment(160_000));
        }
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let z = Place::Session(listed("z", 40));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(z));
    }

    #[test]
    fn folding_a_pane_s_worktree_then_its_project_stops_the_highlight_on_its_row() {
        let mut sidebar = two_sessions();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        type_in(&mut sidebar, "hh");
        let older = Place::Session(listed("older", 10));
        assert_eq!(cursor(&sidebar), Some(older.clone()));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(older));
        let lines = screen(&mut sidebar, false, 32, 7);
        assert!(lines[4].starts_with(" ▸ older"), "{lines:?}");
        assert!(lines[5].starts_with(" ▾ newer"), "{lines:?}");
        sidebar.key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), moment(160_000));
        assert_eq!(sidebar.rows.len(), 8, "the worktree folded first stays folded");
    }

    #[test]
    fn the_title_keeps_the_name_of_the_last_session_listed_where_luneta_sits() {
        let mut sidebar = own_two_sessions();
        assert_eq!(sidebar.own_title(), None);
        sit(&mut sidebar, &own_pane_of("older", 1, 1));
        assert_eq!(sidebar.own_title(), Some("older"));
        let unlisted = OwnKey { id: OwnId::new(3).unwrap(), made: OwnMade::from_nanos(0) };
        sidebar.attaching(Machine::Local, unlisted);
        assert_eq!(sidebar.own_title(), Some("older"));
        sit(&mut sidebar, &own_pane_of("young", 2, 1));
        assert_eq!(sidebar.own_title(), Some("young"));
    }

    #[test]
    fn going_to_a_folded_session_unfolds_it() {
        let older = |pane| own_pane_of("older", 1, pane);
        let away = || {
            let mut sidebar = own_two_sessions();
            fold(&mut sidebar, "older");
            sidebar
        };
        let mut sidebar = away();
        sit(&mut sidebar, &own_pane_of("young", 2, 1));
        assert!(folded(&sidebar, "older"));
        sit(&mut sidebar, &older(1));
        assert!(!folded(&sidebar, "older"));
        let mut sidebar = away();
        let made = OwnMade::from_nanos(0);
        sidebar.attaching(Machine::Local, OwnKey { id: OwnId::new(1).unwrap(), made });
        assert!(!folded(&sidebar, "older"));
    }

    #[test]
    fn going_into_a_project_on_a_folded_machine_unfolds_the_machine() {
        let mut sidebar = own_two_sessions();
        sidebar.folds.set(Fold::Machine(&Machine::Local), true);
        sidebar.rebuild();
        let folded = " ▸ \u{f108} mac ══════════════════ ●1";
        assert_eq!(screen(&mut sidebar, false, 32, 5)[3], folded);
        let pane = own_pane_of("older", 1, 1);
        sit(&mut sidebar, &pane);
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        assert_eq!(sidebar.here_row, Some(Spot::Local(pane)));
    }

    #[test]
    fn a_gone_pane_leaves_the_highlight_in_its_session() {
        let mut sidebar = two_sessions();
        sidebar.step(1);
        sidebar.hear(host(
            vec![session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")])],
            Vec::new(),
        ));
        assert_eq!(cursor(&sidebar), Some(pane_of("older", 10, 1)));
    }

    #[test]
    fn the_rows_scroll_to_keep_the_highlight_on_screen() {
        let mut sidebar = two_sessions();
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let shown = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 5)[3..5].to_vec();
        assert_eq!(
            shown(&mut sidebar),
            ["  ▾ Tab #1                  ●1", "   ● \u{ec82} fold                 1m"]
        );
        sidebar.key(&down, moment(160_000));
        sidebar.key(&down, moment(160_000));
        assert_eq!(shown(&mut sidebar), ["  ▾ Tab #1", "     \u{e795} w/b"]);
        sidebar.key(&up, moment(160_000));
        assert_eq!(
            shown(&mut sidebar),
            ["     \u{e6ae} nvim src/main.rs", " ▾ newer ─────────────────────"]
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    fn click(sidebar: &mut Sidebar, column: u16, row: u16, area: Rect) -> Asked {
        let down = mouse(MouseEventKind::Down(MouseButton::Left), column, row);
        assert_eq!(sidebar.mouse(down, area), Asked::Nothing);
        sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, row), area)
    }

    #[test]
    fn a_click_goes_to_a_pane_and_folds_on_the_triangle() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 11);
        screen(&mut sidebar, false, area.width, area.height);
        let pane = pane_of("older", 10, 2);
        assert_eq!(click(&mut sidebar, 10, 8, area), go_here(pane));
        assert_eq!(click(&mut sidebar, 10, 4, area), Asked::Take);
        assert!(!folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(folded(&sidebar, "older"));
        let older = Place::Session(listed("older", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(older));
    }

    #[test]
    fn a_click_on_the_triangle_folds_until_a_search_is_typed() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 11);
        sidebar.key(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE), moment(160_000));
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(!folded(&sidebar, "older"));
        assert!(sidebar.search.is_some());
        type_in(&mut sidebar, "older");
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(!folded(&sidebar, "older"));
    }

    #[test]
    fn a_click_on_a_session_with_no_panes_goes_there() {
        let mut sidebar = fresh();
        let mut starting = session("x", 10, Vec::new());
        starting.panes = None;
        sidebar.hear(host(vec![starting], Vec::new()));
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
        let fold = pane_of("older", 10, 1);
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(fold));
    }

    #[test]
    fn a_pane_is_named_for_what_runs_in_it() {
        let named = |command: &str| label(None, &pane(3, 0, "/w/luneta/main", command));
        assert_eq!(named("-fish"), "luneta/main");
        assert_eq!(named("/bin/zsh -l"), "luneta/main");
        assert_eq!(named("cargo test"), "cargo test");
        assert_eq!(named("/nix/store/a1p5-neovim/bin/nvim --clean"), "nvim --clean");
        let unsaid = Pane { cwd: None, command: None, ..pane(3, 0, "", "") };
        assert_eq!(label(None, &unsaid), "view 3");
        let tabbed = |pane: Pane| label(None, &Pane { tab_name: "review".to_owned(), ..pane });
        assert_eq!(tabbed(pane(3, 0, "/w/luneta/main", "-fish")), "luneta/main", "its row says it");
        assert_eq!(tabbed(unsaid), "view 3");
    }

    #[test]
    fn an_agent_goes_by_its_name_then_its_topic_then_its_directory() {
        let titled = |topic: Option<&str>, tab: &str| Pane {
            titled: Some(api::Titled { topic: topic.map(str::to_owned) }),
            tab_name: tab.to_owned(),
            ..pane(1, 0, "/w/x", "claude")
        };
        let untitled = |tab: &str| Pane { tab_name: tab.to_owned(), ..pane(1, 0, "/w/x", "claude") };
        let named = agent("s", 1, Status::Idle, Some("fold"));
        let unnamed = agent("s", 1, Status::Idle, None);
        let topic = titled(Some("Fix the fold"), "review");
        assert_eq!(label(Some(&named), &topic), "fold");
        assert_eq!(label(Some(&unnamed), &topic), "Fix the fold");
        assert_eq!(label(Some(&unnamed), &titled(None, "review")), "w/x");
        assert_eq!(label(Some(&unnamed), &untitled("review")), "w/x");
        assert_eq!(label(Some(&unnamed), &untitled(" ")), "w/x");
        let unsaid = Pane { cwd: None, ..untitled("Tab #1") };
        assert_eq!(label(Some(&unnamed), &unsaid), "luneta/main");
        let mut derived = agent("s", 1, Status::Idle, Some("luneta-main"));
        let source = Some(NameSource::Derived.into());
        derived.name = derived.name.map(|name| api::Name { source, ..name });
        assert_eq!(label(Some(&derived), &topic), "Fix the fold");
        let kept =
            |agent: &Agent| Agent { topic: Some("Session folds".to_owned()), ..agent.clone() };
        assert_eq!(label(Some(&kept(&named)), &topic), "fold");
        assert_eq!(label(Some(&kept(&unnamed)), &topic), "Session folds");
        assert_eq!(label(Some(&kept(&derived)), &titled(None, "review")), "Session folds");
    }

    #[test]
    fn a_pane_s_icon_says_what_runs_in_it_in_a_column_of_its_own() {
        let mut sidebar = fresh();
        let sudo = "/nix/store/fdsygnh4wvr7g860ba3cwlcksn8v1pc5-sudo-1.9.17p2/bin/sudo -k nvim x";
        sidebar.hear(host(
            vec![session(
                "s",
                10,
                vec![
                    pane(1, 0, "/w/a", "cargo test"),
                    pane(2, 1, "/w/b", sudo),
                    pane(3, 2, "/w/c", "make"),
                    pane(4, 3, "/w/d", "dash"),
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
        let panes = (1..=3).map(|id| pane(id, id, "/w/a", "nvim")).collect();
        let agents = vec![of(1, Some(Harness::Pi.into())), of(2, None), of(3, Some(99))];
        sidebar.hear(host(vec![session("s", 10, panes)], agents));
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
        let says = |pane: u32, word: &str| Agent {
            status: Some(Reported::OtherStatus(word.to_owned())),
            ..agent("s", pane, Status::Idle, Some("x"))
        };
        let mut sidebar = fresh();
        let panes = (1..=4).map(|id| pane(id, id, "/w/a", "claude")).collect();
        let agents = words.into_iter().zip(1..).map(|(word, id)| says(id, word)).collect();
        sidebar.hear(host(vec![session("s", 10, panes)], agents));
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(
            [6, 8, 10, 12].map(|at| start(&lines[at])),
            ["   C \u{ec82} x", "   S \u{ec82} x", "   ? \u{ec82} x", "   ? \u{ec82} x"]
        );
    }

    #[test]
    fn a_pane_with_no_agent_in_it_is_dimmed() {
        let mut sidebar = two_sessions();
        let buffer = drawn(&mut sidebar, false, 32, 10);
        let dim = |y: u16| [5_u16, 7].map(|x| buffer[(x, y)].modifier.contains(Modifier::DIM));
        assert_eq!((dim(6), dim(8)), ([false, false], [true, true]));
    }

    #[test]
    fn an_agent_the_daemon_cannot_follow_sits_where_its_pane_works() {
        let mut lost = agent("gone", 1, Status::Waiting, Some("fold"));
        let gone = Zellij { named: "gone".to_owned(), session: None, pane: 1 };
        lost.pane = Some(api::agent::Pane::Zellij(gone));
        let sessions = [
            session("renamed", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            session("other", 20, vec![pane(1, 0, "/w/elsewhere", "fish")]),
        ];
        let agents = [lost];
        let held = seated(&agents, &sessions);
        assert_eq!(held.keys().collect::<Vec<_>>(), [&(Where::Zellij("renamed"), 1)]);
        let own = own_session("own", 1, vec![pane(1, 0, "/w/luneta/main", "claude")]);
        let with_own = [session("renamed", 10, vec![pane(1, 0, "/w/luneta/main", "fish")]), own];
        let held = seated(&agents, &with_own);
        assert_eq!(held.keys().collect::<Vec<_>>(), [&(Where::Zellij("renamed"), 1)]);

        let titled = Pane {
            titled: Some(api::Titled { topic: None }),
            ..pane(1, 0, "/w/luneta/main", "fish")
        };
        let sessions = [
            session("renamed", 10, vec![titled]),
            session("other", 20, vec![pane(1, 0, "/w/luneta/main", "fish")]),
        ];
        let held = seated(&agents, &sessions);
        assert_eq!(held.keys().collect::<Vec<_>>(), [&(Where::Zellij("renamed"), 1)]);
    }

    #[test]
    fn a_pi_agent_the_daemon_cannot_follow_takes_no_claude_code_pane() {
        let lost = Agent {
            harness: Some(Harness::Pi.into()),
            pane: Some(api::agent::Pane::Zellij(Zellij {
                named: "gone".to_owned(),
                session: None,
                pane: 1,
            })),
            ..agent("gone", 1, Status::Idle, Some("pi"))
        };
        let sessions = [
            session("renamed", 10, vec![pane(1, 0, "/w/luneta/main", "pi")]),
            session("other", 20, vec![pane(1, 0, "/w/luneta/main", "claude")]),
        ];
        assert_eq!(seated(&[lost], &sessions).len(), 0);
    }

    #[test]
    fn a_pane_holds_the_agent_started_at_a_prompt() {
        let mut prompt = agent("s", 1, Status::Idle, Some("prompt"));
        prompt.started_at = Some(Timestamp { seconds: 90, nanos: 0 });
        let mut headless = agent("s", 1, Status::Busy, Some("headless"));
        headless.entrypoint = Some("sdk-cli".to_owned());
        let agents = [headless, prompt];
        let seated = seated(&agents, &[]);
        assert_eq!(
            seated
                .get(&(Where::Zellij("s"), 1))
                .and_then(|agent| agent.name.as_ref())
                .map(|name| name.text.as_str()),
            Some("prompt")
        );
    }

    #[test]
    fn a_pane_holds_the_pi_agent_started_at_a_prompt() {
        let of = |name: &str, mode: &str, started: i64| Agent {
            harness: Some(Harness::Pi.into()),
            entrypoint: Some(mode.to_owned()),
            started_at: Some(Timestamp { seconds: started, nanos: 0 }),
            ..agent("s", 1, Status::Idle, Some(name))
        };
        let agents = [of("on its own", "print", 10), of("prompt", "tui", 90)];
        let seated = seated(&agents, &[]);
        let name = seated.get(&(Where::Zellij("s"), 1)).and_then(|agent| agent.name.as_ref());
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

    fn in_shell(id: u32, doing: Doing) -> Pane {
        Pane { shell: Some(Shell { doing: Some(doing) }), ..pane(id, 0, "/w/luneta/main", "fish") }
    }

    fn going(command: &str, since: i64, front: Front) -> Doing {
        let since = Some(Timestamp { seconds: since, nanos: 0 });
        Doing::Going(Going { command: Some(command.to_owned()), since, front: front.into() })
    }

    fn ended(command: &str, status: Option<i32>, millis: i32) -> Doing {
        let (seconds, nanos) = ((millis / 1000).into(), millis % 1000 * 1_000_000);
        let took = Some(prost_types::Duration { seconds, nanos });
        let at = Some(Timestamp { seconds: 130, nanos: 0 });
        let ending = status.map(ended::Ending::Status);
        Doing::Ended(Ended { command: Some(command.to_owned()), at, ending, took })
    }

    #[test]
    fn what_a_shell_said_it_ran_is_read_as_it_comes() {
        let said = |doing| ran(&Shell { doing: Some(doing) });
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        let command = Said::new("make".to_owned()).unwrap();
        let make = Ran::Ended(menu::Ended { command, ending: Some(Ending::Exited(2)), at });
        assert_eq!(said(ended("make", Some(2), 10)), make);
        assert_eq!(said(ended("", Some(0), 10)), Ran::Unknown);
        let Doing::Ended(untimed) = ended("make", Some(2), 10) else {
            panic!("an end");
        };
        assert_eq!(said(Doing::Ended(Ended { at: None, ..untimed })), Ran::Unknown);
        assert_eq!(said(going("", 100, Front::Working)), Ran::Unknown);
        let Doing::Going(working) = going("make", 100, Front::Working) else {
            panic!("a command going");
        };
        assert_eq!(said(Doing::Going(Going { command: None, ..working })), Ran::Going(None));
        let unsaid = Ended { command: None, ..untimed };
        assert_eq!(said(Doing::Ended(unsaid.clone())), Ran::EndedUnsaid);
        assert_eq!(said(Doing::Ended(Ended { at: None, ..unsaid })), Ran::Unknown);
        assert_eq!(ran(&Shell { doing: None }), Ran::Unknown);
        assert_eq!(said(Doing::Ready(())), Ran::Ready);
    }

    #[test]
    fn a_command_has_an_agent_s_marks_and_its_row_is_named_for_it() {
        let mut sidebar = fresh();
        let stopped = Doing::Stopped(Stopped {
            command: Some("make".to_owned()),
            at: Some(Timestamp { seconds: 130, nanos: 0 }),
        });
        let nvim = in_shell(3, going("nvim notes", 100, Front::Interactive));
        let nvim = Pane { command: Some("nvim notes".to_owned()), ..nvim };
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
                "─ luneta ──────────────────────",
                "─ 1 project ──────── ●1 •3 ⠋1 ─",
                "",
                " ▾ \u{f108} mac ══════════════════ ●1",
                " ▾ s ────────────────────── ●1",
                "  ▾ Tab #1                  ●1",
                " ╭ ⠋ \u{e795} cargo build --rele… 59s",
                " │ ● \u{e795} sudo -k true        59s",
                " │   \u{e6ae} nvim notes",
                " │•✓ \u{e795} cargo test          29s",
                " │•✗ \u{e795} cargo clippy        29s",
                " │•$ \u{e795} cargo run           29s",
                " │   \u{e795} luneta/main",
                " │   \u{e795} luneta/main",
                " ╰   \u{e795} luneta/main",
            ]
        );
        assert!(sidebar.turning());
    }

    fn moment(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    fn steady(now: SystemTime) -> Instant {
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        *START + now.duration_since(SystemTime::UNIX_EPOCH).unwrap()
    }

    fn tick(sidebar: &mut Sidebar, millis: u64) {
        sidebar.ticked(moment(millis), steady(moment(millis)), true);
    }

    fn dimmed(sidebar: &mut Sidebar, x: u16, y: u16, now: SystemTime) -> bool {
        drawn_at(sidebar, false, 32, 8, now)[(x, y)].modifier.contains(Modifier::DIM)
    }

    #[test]
    fn the_picker_gets_an_agents_directory_as_its_last_two_parts() {
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(2, 0, "", "claude") };
        sidebar.hear(host(
            vec![session("s", 10, vec![pane(1, 0, "/home/me/src/luneta/pr-12", "claude"), unsaid])],
            vec![agent("s", 1, Status::Idle, None), agent("s", 2, Status::Idle, None)],
        ));
        let agents = sidebar.listing().local.agents;
        let dirs: Vec<&str> = agents.iter().map(|seated| seated.dir.as_str()).collect();
        assert_eq!(dirs, ["luneta/pr-12", "luneta/main"]);
    }

    #[test]
    fn what_finished_has_the_dot_until_you_stay_in_its_pane() {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![own_session(
                "s",
                1,
                vec![
                    pane(1, 0, "/w/luneta/main", "claude"),
                    in_shell(2, ended("cargo test", Some(0), 5_000)),
                    pane(3, 3, "/w/a", "fish"),
                ],
            )],
            vec![in_own(agent("s", 1, Status::Idle, Some("fold")), 1, 1)],
        ));
        let lines = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, false, 32, 10, now).0
        };
        assert_eq!(
            lines(&mut sidebar, moment(160_000)),
            [
                "─ luneta ──────────────────────",
                "─ 1 project ────────────── •2 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ s ─────────────────────────",
                "  ▾ Tab #1",
                " ╭•✓ \u{ec82} fold                 1m",
                " ╰•✓ \u{e795} cargo test          29s",
                "  ▾ Tab #4",
                "     \u{e795} w/a",
            ]
        );
        assert!(!dimmed(&mut sidebar, 3, 6, moment(160_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Finished);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        assert!(lines(&mut sidebar, moment(160_000))[5].ends_with(" •"));
        sidebar.key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), moment(160_000));

        let s = |pane| own_pane_of("s", 1, pane);
        sit(&mut sidebar, &s(2));
        assert_eq!(lines(&mut sidebar, moment(160_000))[7], "▌╰   \u{e795} luneta/main");
        sit(&mut sidebar, &s(1));
        lines(&mut sidebar, moment(160_500));
        lines(&mut sidebar, moment(161_500));
        sit(&mut sidebar, &s(3));
        assert_eq!(
            lines(&mut sidebar, moment(162_000))[..8],
            [
                "─ luneta ──────────────────────",
                "─ 1 project ────────────── •1 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ s ─────────────────────────",
                "  ▾ Tab #1",
                " ╭ ✓ \u{ec82} fold                 1m",
                " ╰•✓ \u{e795} cargo test          31s",
            ]
        );
        assert!(dimmed(&mut sidebar, 3, 6, moment(162_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Idle);

        sit(&mut sidebar, &s(2));
        lines(&mut sidebar, moment(163_000));
        lines(&mut sidebar, moment(164_000));
        sit(&mut sidebar, &s(3));
        let seen = lines(&mut sidebar, moment(164_500));
        assert_eq!(seen[1], format!("─ 1 project {}", "─".repeat(19)));
        assert_eq!(seen[7], " ╰   \u{e795} luneta/main");
    }

    #[test]
    fn u_marks_a_pane_unread_until_you_come_back_to_it() {
        let mut sidebar = own_two_sessions();
        let older = |pane| own_pane_of("older", 1, pane);
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let nvim = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            let lines = seen_at(sidebar, true, 32, 12, now).0;
            lines.into_iter().find(|line| line.contains("nvim")).expect("nvim's row")
        };
        sit(&mut sidebar, &older(2));
        nvim(&mut sidebar, moment(160_000));
        sidebar.key(&u, moment(161_000));
        assert_eq!(nvim(&mut sidebar, moment(165_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        assert!(screen(&mut sidebar, true, 32, 7)[1].ends_with(" ●1 •1 ─"));

        sit(&mut sidebar, &older(1));
        nvim(&mut sidebar, moment(166_000));
        sit(&mut sidebar, &older(2));
        assert_eq!(nvim(&mut sidebar, moment(167_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌    \u{e6ae} nvim src/main.rs");

        sidebar.key(&u, moment(168_000));
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        sidebar.key(&u, moment(168_000));
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌    \u{e6ae} nvim src/main.rs");
    }

    fn own_idle_agent() -> Host {
        host(
            vec![own_session(
                "s",
                1,
                vec![pane(1, 0, "/w/luneta/main", "claude"), pane(3, 1, "/w/a", "fish")],
            )],
            vec![in_own(agent("s", 1, Status::Idle, Some("fold")), 1, 1)],
        )
    }

    fn idle_agent() -> Host {
        host(
            vec![session(
                "s",
                10,
                vec![pane(1, 0, "/w/luneta/main", "claude"), pane(3, 1, "/w/a", "fish")],
            )],
            vec![agent("s", 1, Status::Idle, Some("fold"))],
        )
    }

    fn agent_row(sidebar: &mut Sidebar, now: SystemTime, watched: bool) -> String {
        sidebar.ticked(now, steady(now), watched);
        seen_at(sidebar, false, 32, 10, now).0[6].clone()
    }

    #[test]
    fn a_pane_is_yours_only_while_luneta_s_window_has_the_focus() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        let s = |pane| own_pane_of("s", 1, pane);
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
    fn u_marks_a_pane_with_the_dot_read() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        sit(&mut sidebar, &own_pane_of("s", 1, 3));
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let row = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, true, 32, 10, now).0[6].clone()
        };
        let (new, seen) =
            ("  •✓ \u{ec82} fold                 1m", "   ✓ \u{ec82} fold                 1m");
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
        let claude = || vec![pane(1, 0, "/w/luneta/main", "claude")];
        let mut twin = own_session("b", 1, claude());
        twin.socket = 11;
        let mut there = in_own(agent("b", 1, Status::Idle, Some("fold")), 1, 1);
        there.pid = 2;
        sidebar.hear(host(
            vec![own_session("a", 2, claude()), twin],
            vec![in_own(agent("a", 1, Status::Idle, Some("fold")), 2, 1), there],
        ));
        sit(&mut sidebar, &own_pane_of("a", 2, 1));
        let lines = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, false, 32, 10, now).0
        };
        lines(&mut sidebar, moment(160_000));
        let lines = lines(&mut sidebar, moment(161_000));
        assert_eq!(
            [&lines[6], &lines[9]],
            ["▌  ✓ \u{ec82} fold                 1m", "  •✓ \u{ec82} fold                 1m"]
        );
    }

    #[test]
    fn a_session_left_out_of_a_list_keeps_what_you_saw_until_it_is_gone() {
        let mut sidebar = fresh();
        sidebar.hear(own_idle_agent());
        let s = |pane| own_pane_of("s", 1, pane);
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
                sidebar.lost("lunetad does not answer".to_owned());
            },
            |sidebar| {
                let unkept = api::projects::Held::Unkept("its file does not read".to_owned());
                let projects = Some(api::Projects { held: Some(unkept) });
                sidebar.hear(Host { projects, ..host(Vec::new(), Vec::new()) });
            },
            |sidebar| sidebar.heard(Host { projects: None, ..host(Vec::new(), Vec::new()) }),
        ];
        for short in short {
            let mut sidebar = fresh();
            sidebar.hear(own_idle_agent());
            let s = |pane| own_pane_of("s", 1, pane);
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
        let at = |millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
        let row = |sidebar: &mut Sidebar, now| seen_at(sidebar, false, 32, 7, now).0[6].clone();
        assert_eq!(row(&mut sidebar, at(160_500)), "     \u{e795} luneta/main");
        assert!(!sidebar.turning());
        assert_eq!(row(&mut sidebar, at(161_000)), "   ⠋ \u{e795} make                 1s");
        assert!(sidebar.turning());
    }

    #[test]
    fn the_git_status_shows_what_starship_shows() {
        let changes =
            Changes { conflicted: 1, deleted: 1, renamed: 1, modified: 1, staged: 1, untracked: 1 };
        let both = Some(Upstream { ahead: 1, behind: 2 });
        assert_eq!(glyphs(&changes, 1, both), "=$✘»!+?⇕");
        assert_eq!(glyphs(&Changes::default(), 0, Some(Upstream { ahead: 0, behind: 2 })), "⇣");
        assert_eq!(glyphs(&Changes::default(), 0, None), "");
    }

    #[test]
    fn a_branch_shows_the_number_of_its_pull_request_in_its_state_s_colour() {
        let panes = ["/w/a", "/w/b", "/w/c", "/w/d"];
        let panes = panes.iter().zip(1..).map(|(cwd, id)| pane(id, id, cwd, "fish")).collect();
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let checkouts = [
            ("/w/a", "main", 0, 126, State::Open),
            ("/w/b", "handoff-eng-11259-and-more", 1, 4321, State::Merged),
            ("/w/c", "fold", 0, 7, State::Draft),
            ("/w/d", "old", 0, 9, State::Closed),
        ];
        for (dir, branch, modified, number, state) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch { name, upstream: None, changed: None };
            let changes = Changes { modified, ..Changes::default() };
            let status = GitStatus::Checkout { repository: None, head, changes, stashes: 0 };
            host.git_statuses.push(status.to_api(dir));
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let pull_request = PullRequest { number, state, url: None, checks: None, stale: false };
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
        assert_eq!(colour(6), Some(gruvbox::GREEN));
        assert_eq!(colour(9), Some(gruvbox::PURPLE));
        assert_eq!(colour(12), Some(gruvbox::GREY));
    }

    #[test]
    fn a_branch_shows_the_lines_changed_on_it() {
        let panes = ["/w/a", "/w/b", "/w/c", "/w/d", "/w/e", "/w/f"];
        let panes = panes.iter().zip(1..).map(|(cwd, id)| pane(id, id, cwd, "fish")).collect();
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
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
            let head = Head::Branch { name, upstream: None, changed };
            let status = GitStatus::Checkout {
                repository: None,
                head,
                changes: Changes::default(),
                stashes: 0,
            };
            host.git_statuses.push(status.to_api(dir));
            if number > 0 {
                let number = NonZeroU32::new(number).expect("a pull request's number");
                let (state, url) = (State::Open, None);
                let pull_request = PullRequest { number, state, url, checks: None, stale: false };
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
        let branches: Vec<String> = (0..6).map(|pane| line(6 + pane * 3)).collect();
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
            let x = (from..34).find(|x| buffer[(*x, y)].symbol() == mark).expect("the mark");
            buffer[(x, y)].fg
        };
        let first = 6;
        let added = (0..34).find(|x| buffer[(*x, first)].symbol() == "+").expect("the mark");
        assert_eq!(buffer[(added, first)].fg, gruvbox::GREEN);
        assert_eq!(colour(first, added, "-"), gruvbox::RED);
        assert_eq!(colour(first + 3, 0, "+"), gruvbox::GREEN);
        assert_eq!(colour(first + 6, 0, "-"), gruvbox::RED);
    }

    #[test]
    fn an_open_pull_request_shows_what_its_checks_come_to() {
        use luneta_protocol::github::Checks;

        let checks = |pending, failed, passed| Some(Checks { pending, failed, passed });
        let checkouts = [
            ("/w/a", "main", 126, State::Open, checks(1, 1, 3)),
            ("/w/b", "fold", 7, State::Draft, checks(2, 0, 3)),
            ("/w/c", "handoff-eng-11259-and-more", 4321, State::Open, checks(0, 0, 3)),
            ("/w/d", "none", 8, State::Open, checks(0, 0, 0)),
            ("/w/e", "old", 9, State::Open, None),
            ("/w/f", "gone", 10, State::Merged, checks(0, 1, 0)),
            ("/w/g", "late", 11, State::Open, checks(1, 0, 2)),
        ];
        let panes = checkouts.iter().zip(1..).map(|((cwd, ..), id)| pane(id, id, cwd, "fish"));
        let mut host = host(vec![session("s", 10, panes.collect())], Vec::new());
        for (dir, branch, number, state, checks) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch { name, upstream: None, changed: None };
            let changes = Changes::default();
            let status = GitStatus::Checkout { repository: None, head, changes, stashes: 0 };
            host.git_statuses.push(status.to_api(dir));
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let url = Some(format!("https://github.com/owner/repo/pull/{number}"));
            let stale = dir == "/w/g";
            let pull_request = PullRequest { number, state, url, checks, stale };
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
            let x = (0..32).find(|x| buffer[(*x, y)].symbol() == mark).expect("the mark");
            buffer[(x, y)].fg
        };
        assert_eq!(mark(6, "✗"), gruvbox::RED);
        assert_eq!(mark(9, "●"), gruvbox::YELLOW);
        assert_eq!(mark(12, "✓"), gruvbox::GREEN);
        assert_eq!(mark(24, "●"), gruvbox::GREY);
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

        let lines: Vec<String> =
            wrapped("ab⚠️cd", 5, Style::new()).iter().map(ToString::to_string).collect();
        assert_eq!(lines, [" ab", " ⚠️c", " d"]);
    }

    fn searched() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(host(
            vec![
                session(
                    "bipa",
                    10,
                    vec![
                        pane(1, 0, "/w/a", "a-p-i-x"),
                        pane(2, 1, "/w/b", "dda-bank-slip"),
                        pane(3, 2, "/w/c", "pix outflow"),
                    ],
                ),
                session("notes", 20, vec![pane(1, 0, "/home/me/Documents", "fish")]),
                session(
                    "zed",
                    30,
                    vec![
                        pane(1, 0, "/w/q", "htop"),
                        pane(2, 1, "/w/r", "z-e-d"),
                        pane(3, 2, "/w/s", "less logs"),
                    ],
                ),
            ],
            Vec::new(),
        ));
        sidebar
    }

    fn branched(sidebar: &mut Sidebar, branch: &str) -> Asked {
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        type_in(sidebar, branch);
        sidebar.key(&KeyEvent::from(KeyCode::Enter), moment(160_000))
    }

    fn type_in(sidebar: &mut Sidebar, text: &str) {
        for c in text.chars() {
            sidebar.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), moment(160_000));
        }
    }

    fn rows_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let lines = screen(sidebar, true, 32, 28);
        let rows = lines[sessions_from(&lines)..].iter().take_while(|line| !line.is_empty());
        rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect()
    }

    fn sessions_from(lines: &[String]) -> usize {
        let header = lines.get(HEADER).is_some_and(|line| line.starts_with(" ▾ \u{f108} mac ═"));
        HEADER + usize::from(header)
    }

    fn bottom(sidebar: &mut Sidebar) -> String {
        screen(sidebar, true, 32, 18)[17].clone()
    }

    fn searching(sidebar: &mut Sidebar) -> bool {
        screen(sidebar, true, 32, 18).iter().any(|line| line.starts_with("─ Search ─"))
    }

    fn search_shown(sidebar: &mut Sidebar) -> Vec<String> {
        screen(sidebar, true, 32, 16)[13..].to_vec()
    }

    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        const PIX: [&str; 5] =
            [" ▾ bipa", "  ▾ Tab #1", "       a-p-i-x", "  ▾ Tab #3", "       pix outflow"];
        const ZED: [&str; 7] = [
            " ▾ zed",
            "  ▾ Tab #1",
            "     \u{eb31} htop",
            "  ▾ Tab #2",
            "       z-e-d",
            "  ▾ Tab #3",
            "       less logs",
        ];
        let bipa = pane_of("bipa", 10, 3);
        let notes = pane_of("notes", 20, 1);
        let zed = pane_of("zed", 30, 2);
        for (typed, rows, best) in [
            ("pix", &PIX[..], bipa),
            ("note", &[" ▾ notes", "  ▾ Tab #1", "     \u{e795} me/Documents"][..], notes),
            ("zed", &ZED[..], zed),
        ] {
            let mut sidebar = searched();
            type_in(&mut sidebar, &format!("/{typed}"));
            assert_eq!(rows_shown(&mut sidebar), rows, "{typed}");
            let rule = screen(&mut sidebar, true, 32, 16)[1].clone();
            assert!(rule.starts_with("─ 3 projects ─"), "{typed}: {rule}");
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(best), "{typed}");
        }
    }

    #[test]
    fn a_pane_is_found_by_its_worktree_s_name_branch_and_folder() {
        let mut notes = pane(1, 0, "/w/a", "nvim a");
        notes.tab_name = "notes".to_owned();
        let deep = pane(2, 1, "/home/lorenzo/Projects/misc/luneta", "nvim b");
        let branched = pane(3, 2, "/w/c", "nvim c");
        let unsaid = Pane {
            cwd: None,
            titled: Some(api::Titled { topic: Some("Rename the rows".to_owned()) }),
            ..pane(4, 3, "", "claude")
        };
        let elsewhere = "/w/fold/review".to_owned();
        let seated = Agent { cwd: elsewhere, ..agent("s", 4, Status::Idle, None) };
        let panes = vec![notes, deep, branched, unsaid];
        let mut host = host(vec![session("s", 10, panes)], vec![seated]);
        let name = Said::new("feature/sidebar".to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let status = GitStatus::Checkout { repository: None, head, changes, stashes: 0 };
        host.git_statuses.push(status.to_api("/w/c"));
        let found = |typed: &str| {
            let mut sidebar = fresh();
            sidebar.hear(host.clone());
            type_in(&mut sidebar, &format!("/{typed}"));
            rows_shown(&mut sidebar)
        };
        assert_eq!(found("notes"), [" ▾ s", "  ▾ notes", "     \u{e6ae} nvim a"]);
        assert_eq!(found("luneta"), [" ▾ s", "  ▾ Tab #2", "     \u{e6ae} nvim b"]);
        let branched = [" ▾ s", "  ▾ Tab #3", "       feature/sidebar", "     \u{e6ae} nvim c"];
        assert_eq!(found("sidebar"), branched);
        let seated = found("fold/review");
        assert!(seated.len() == 3 && seated[2].contains("Rename the rows"), "{seated:?}");
        assert_eq!(found("zzz"), [" no match"]);
        assert_eq!(found("loren"), [" no match"]);
    }

    #[test]
    fn a_search_opens_a_folded_project_until_it_closes() {
        for close in [Some(KeyCode::Esc), Some(KeyCode::Enter), None] {
            let mut sidebar = searched();
            type_in(&mut sidebar, "hh/pix");
            let found =
                [" ▾ bipa", "  ▾ Tab #1", "       a-p-i-x", "  ▾ Tab #3", "       pix outflow"];
            assert_eq!(rows_shown(&mut sidebar), found, "{close:?}");
            let asked = if let Some(code) = close {
                sidebar.key(&KeyEvent::new(code, KeyModifiers::NONE), moment(160_000))
            } else {
                sidebar.focus(Keys::Zellij);
                Asked::Nothing
            };
            let pix = pane_of("bipa", 10, 3);
            let went = if close == Some(KeyCode::Enter) { go_here(pix) } else { Asked::Nothing };
            assert_eq!(asked, went, "{close:?}");
            assert_eq!(rows_shown(&mut sidebar)[..2], [" ▸ bipa", " ▾ notes"], "{close:?}");
            assert!(!searching(&mut sidebar), "{close:?}");
        }
    }

    #[test]
    fn going_to_a_pane_found_in_luneta_s_folded_project_unfolds_it() {
        let mut sidebar = owned(searched());
        type_in(&mut sidebar, "hh");
        assert_eq!(rows_shown(&mut sidebar)[..2], [" ▸ bipa", " ▾ notes"]);
        type_in(&mut sidebar, "/pix");
        let pix = own_pane_of("bipa", 1, 3);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(pix.clone()));
        sit(&mut sidebar, &pix);
        let rows = rows_shown(&mut sidebar);
        assert_eq!(rows[..4], [" ▾ bipa", "  ▸ Tab #1", "  ▾ Tab #2", "       dda-bank-slip"]);
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
        let found = [" ▾ bipa", "  ▾ Tab #1", "       a-p-i-x", "  ▾ Tab #3", "       pix outflow"];
        assert_eq!(rows_shown(&mut sidebar), found);
        assert!(search_shown(&mut sidebar)[0].ends_with(" 2/7 ─"));
        for _ in 0..4 {
            sidebar.key(&backspace, moment(160_000));
        }
        assert!(!searching(&mut sidebar));
        type_in(&mut sidebar, "/a-very-long-search-for-something");
        assert_eq!(search_shown(&mut sidebar)[1], " …ry-long-search-for-something_");
    }

    #[test]
    fn a_click_on_a_key_under_the_search_presses_it_while_it_is_open() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "/pix");
        let area = Rect::new(0, 0, 32, 16);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        let keys: String = (0..31).map(|x| buffer[(x, 15)].symbol()).collect();
        let at = |key: &str| Position::new(u16::try_from(keys.find(key).unwrap()).unwrap(), 15);
        let (enter, esc) = (KeyEvent::from(KeyCode::Enter), KeyEvent::from(KeyCode::Esc));
        assert_eq!(drawn.at(at("<ENTER>")), Some(Hit::Keys(Keyed::Search, enter)));
        assert_eq!(drawn.at(at("<ESC>")), Some(Hit::Keys(Keyed::Search, esc)));
        assert_eq!(drawn.at(Position::new(2, 14)), None);
        assert_eq!(sidebar.keys_clicked(Keyed::Search, &esc, moment(160_000)), Asked::Nothing);
        assert!(!searching(&mut sidebar));
        assert_eq!(sidebar.keys_clicked(Keyed::Search, &enter, moment(160_000)), Asked::Nothing);
        assert!(!searching(&mut sidebar));
    }

    #[test]
    fn the_search_has_no_keys_under_a_box() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "/pix");
        let enter = KeyEvent::from(KeyCode::Enter);
        let area = Rect::new(0, 0, 32, 16);
        let rule = format!("─ Search {} 2/7 ─", "─".repeat(16));
        for keys in [Keys::Lent, Keys::Menu { lent: true }] {
            sidebar.focus(keys);
            let mut buffer = Buffer::empty(area);
            sidebar.draw(&mut buffer, area, moment(160_000));
            let line = |y: u16| (0..31).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            assert_eq!([line(14).trim_end(), line(15).trim_end()], [rule.as_str(), " pix_"]);
            let clicked = sidebar.keys_clicked(Keyed::Search, &enter, moment(160_000));
            assert_eq!(clicked, Asked::Nothing);
        }
        sidebar.focus(Keys::Sidebar);
        assert_eq!(search_shown(&mut sidebar)[1], " pix_");
    }

    #[test]
    fn a_search_lights_what_it_found_of_each_row() {
        let mut sidebar = searched();
        type_in(&mut sidebar, "/zed");
        let zed = [" ▾ zed", "  ▾ Tab #1", "     \u{eb31} htop", "  ▾ Tab #2", "       z-e-d"];
        assert_eq!(rows_shown(&mut sidebar)[..5], zed);
        let zed = u16::try_from(sessions_from(&screen(&mut sidebar, true, 32, 16))).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        let buffer = drawn_at(&mut sidebar, true, 32, 16, now);
        let marked = |y: u16, mark: fn(&Cell) -> bool| -> String {
            let cells = (0..31).map(|x| &buffer[(x, y)]);
            cells.filter(|cell| mark(cell)).map(Cell::symbol).collect()
        };
        let underlined = |cell: &Cell| cell.modifier.contains(Modifier::UNDERLINED);
        let lit = |cell: &Cell| cell.fg == ACCENT && cell.modifier.contains(Modifier::BOLD);
        assert_eq!(marked(zed, underlined), "zed");
        assert_eq!(marked(zed + 2, lit), "");
        assert_eq!(marked(zed + 4, lit), "zed");
    }

    #[test]
    fn what_the_cut_took_lights_nothing() {
        let (plain, hit) = (Style::new(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD));
        let (spans, wide) = lit("pix outflow", 8, Keep::Start, &[0, 7], plain);
        assert_eq!(spans, [Span::styled("p", hit), Span::styled("ix out…", plain)]);
        assert_eq!(wide, 8);
        let (spans, wide) = lit("/w/luneta/main", 8, Keep::End, &[6, 12], plain);
        let (cut, n) = (Span::styled("…ta/ma", plain), Span::styled("n", plain));
        assert_eq!(spans, [cut, Span::styled("i", hit), n]);
        assert_eq!(wide, 8);
    }

    #[test]
    fn a_hit_on_a_text_in_the_accent_is_underlined() {
        let accent = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
        let (spans, _) = lit("zed", 8, Keep::Start, &[0], accent);
        let underlined = accent.add_modifier(Modifier::UNDERLINED);
        assert_eq!(spans, [Span::styled("z", underlined), Span::styled("ed", accent)]);
    }

    fn char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    const LINK: &str = "https://github.com/owner/repo/pull/246";

    fn link() -> Link {
        Link::parse(LINK).expect("a link")
    }

    fn checked_out(
        git_statuses: &mut Vec<api::GitStatus>,
        pull_requests: &mut Vec<api::PullRequest>,
        dir: &str,
        branch: &str,
        pull: Option<(u32, State, Option<&str>)>,
    ) {
        let name = Said::new(branch.to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let status = GitStatus::Checkout { repository: None, head, changes, stashes: 0 };
        git_statuses.push(status.to_api(dir));
        if let Some((number, state, url)) = pull {
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let url = url.map(str::to_owned);
            let pull_request = PullRequest { number, state, url, checks: None, stale: false };
            pull_requests.push(pull_request.to_api(dir));
        }
    }

    fn pulls() -> Sidebar {
        let dirs = ["/w/a", "/w/b", "/w/c", "/w/d"];
        let panes = dirs.iter().zip(1..).map(|(cwd, id)| pane(id, id, cwd, "fish")).collect();
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        let closed = "https://github.com/owner/repo/pull/9";
        checked_out(statuses, pulls, "/w/a", "fix", Some((246, State::Open, Some(LINK))));
        checked_out(statuses, pulls, "/w/b", "old", Some((7, State::Open, None)));
        checked_out(statuses, pulls, "/w/c", "gone", Some((9, State::Closed, Some(closed))));
        checked_out(statuses, pulls, "/w/d", "none", None);
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
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
            let (lines, _) = seen_at(sidebar, false, 32, 17, at);
            let first = lines.iter().rposition(String::is_empty).map_or(0, |at| at + 1);
            lines[first..].to_vec()
        };
        assert!(!sidebar.turning());
        sidebar.filing(1, "luneta");
        sidebar.filing(2, "zed");
        assert!(sidebar.turning());
        let now = moment(160_000);
        assert_eq!(notes(&mut sidebar, now), [
            " ⠋ Filing an issue in luneta…",
            " ⠋ Filing an issue in zed…",
        ]);
        let url = "https://github.com/o/luneta/issues/355";
        let filed = Filed::page(url, "A red button").unwrap();
        sidebar.filed(1, "luneta", &Ok(filed));
        sidebar.filed(2, "zed", &Err(Unfiled::Refused("gh: no GitHub remote".to_owned())));
        assert!(!sidebar.turning());
        let later = moment(3_600_000);
        let buffer = drawn_at(&mut sidebar, false, 32, 16, later);
        let lines = notes(&mut sidebar, later);
        assert_eq!(lines, [
            " issue in zed: gh: no GitHub r",
            " emote; i has your words again",
            " filed #355 in luneta: A red b",
            " utton; link copied",
        ]);
        let note = u16::try_from(16 - 2).unwrap();
        assert_eq!(buffer[(1, note)].fg, NAME);
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&char('j'), later);
        assert_eq!((sidebar.filed.as_ref(), sidebar.notice.as_ref()), (None, None));
        sidebar.filing(3, "luneta");
        let stuck = Unfiled::InDoubt("gh: it did not answer in time".to_owned());
        sidebar.filed(3, "luneta", &Err(stuck));
        assert_eq!(notes(&mut sidebar, later), [
            " issue in luneta may be filed:",
            "  gh: it did not answer in tim",
            " e; i has your words, look fir",
            " st",
        ]);
    }

    #[test]
    fn y_p_copies_the_highlighted_row_s_link_and_a_note_says_so() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('y'), now), Asked::Nothing);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now + PAUSE);
        let rule = format!("─ Yank {}", "─".repeat(24));
        assert_eq!(lines[22..], [rule, " p  PR link".to_owned()]);

        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
        let buffer = drawn_at(&mut sidebar, true, 32, 24, now);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now);
        let note = lines.iter().position(|line| line == " copied #246").expect("the note");
        assert_eq!(buffer[(1, u16::try_from(note).unwrap())].fg, NAME);
        assert!(lines.iter().any(|line| line.starts_with("─ luneta ──")), "the legend is back");

        let shown = |sidebar: &mut Sidebar, millis| {
            let at = moment(millis);
            sidebar.ticked(at, steady(at), true);
            seen_at(sidebar, false, 32, 24, at).0.contains(&" copied #246".to_owned())
        };
        assert!(shown(&mut sidebar, 162_999));
        assert!(!shown(&mut sidebar, 163_000));

        assert_eq!(sidebar.key(&char('y'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
        sidebar.key(&char('j'), now);
        assert_eq!(sidebar.copied, None);
    }

    #[test]
    fn y_waits_for_one_key_and_esc_only_drops_it() {
        let mut sidebar = pulls();
        let one = Spot::Local(pane_of("s", 10, 1));
        sidebar.cursor = Some(one.clone());
        let now = moment(160_000);
        let esc = KeyEvent::from(KeyCode::Esc);
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&esc, now), Asked::Nothing);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, now), Asked::Leave);

        sidebar.key(&char('y'), now);
        sidebar.key(&char('j'), now);
        assert_eq!(cursor(&sidebar), Some(pane_of("s", 10, 2)));
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);

        sidebar.cursor = Some(one);
        let area = Rect::new(0, 0, 32, 24);
        sidebar.key(&char('y'), now);
        sidebar.mouse(mouse(MouseEventKind::ScrollUp, 10, 4), area);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);

        sidebar.key(&char('y'), now);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), area);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
    }

    #[test]
    fn a_click_on_y_then_on_p_copies() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        sidebar.key(&char('?'), now);
        assert_eq!(sidebar.keys_clicked(Keyed::Legend, &char('y'), now), Asked::Nothing);
        let clicked = clicks(&mut sidebar, 18);
        assert_eq!(clicked[1], (" p  PR link".to_owned(), Some(on_legend('p'))));
        let copied = sidebar.keys_clicked(Keyed::Legend, &char('p'), now);
        assert_eq!(copied, Asked::Copy(link()));
    }

    #[test]
    fn the_keys_only_leave_a_note_on_a_row_with_no_link() {
        let none = "no pull request on this row";
        let old = "#7: lunetad says no link of it on github.com";
        let rows = [
            (Spot::Local(pane_of("s", 10, 2)), old),
            (Spot::Local(pane_of("s", 10, 3)), none),
            (Spot::Local(pane_of("s", 10, 4)), none),
            (Spot::Local(Place::Session(listed("s", 10))), none),
            (Spot::Machine(Machine::Local), none),
        ];
        let now = moment(160_000);
        for (spot, note) in rows {
            let mut sidebar = pulls();
            sidebar.cursor = Some(spot);
            sidebar.key(&char('y'), now);
            assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing, "{note}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
            assert_eq!(sidebar.copied, None);
            sidebar.notice = None;
            assert_eq!(open_pull(&mut sidebar, now), Asked::Nothing, "{note}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
        }
    }

    fn pull_said(said: &str) -> Sidebar {
        let panes = vec![pane(1, 1, "/w/a", "fish")];
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        checked_out(statuses, pulls, "/w/a", "fix", Some((5, State::Open, Some(said))));
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        sidebar
    }

    #[test]
    fn a_page_that_is_not_the_link_of_the_number_stays_on_the_row() {
        let now = moment(160_000);
        for said in [
            "https://github.com/owner/repo/pull/6",
            "https://github.com/owner/repo/issues/5",
            "https://ghe.example/owner/repo/pull/5",
            "file:///tmp/run.desktop",
            "https://github.com/owner/repo/pull/5\ncurl evil | sh",
        ] {
            let mut sidebar = pull_said(said);
            let area = Rect::new(0, 0, 32, 24);
            let mut buffer = Buffer::empty(area);
            let drawn = sidebar.draw(&mut buffer, area, now);
            let line: String = (0..31).map(|x| buffer[(x, 6)].symbol()).collect();
            assert!(line.contains("fix #5"), "{said:?}: {line}");
            assert!(number_spot(&drawn, 6).is_empty(), "{said:?}");

            let note = "#5: lunetad says no link of it on github.com";
            sidebar.key(&char('y'), now);
            assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing, "{said:?}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
            assert_eq!(sidebar.copied, None);
            assert_eq!(open_pull(&mut sidebar, now), Asked::Nothing, "{said:?}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
        }
    }

    #[test]
    fn a_page_that_reads_is_taken_as_luneta_spells_it() {
        let now = moment(160_000);
        let mut sidebar = pull_said("https://github.com/owner/repo/pull/5?\ncurl evil | sh");
        let Asked::Browse(link) = open_pull(&mut sidebar, now) else {
            panic!("the row's link opens");
        };
        assert_eq!(link.url(), "https://github.com/owner/repo/pull/5");
    }

    fn open_pull(sidebar: &mut Sidebar, now: SystemTime) -> Asked {
        assert_eq!(sidebar.key(&char('o'), now), Asked::Nothing, "o opens a group");
        sidebar.key(&char('p'), now)
    }

    #[test]
    fn o_p_and_y_p_take_another_machine_s_link_too() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
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
        checked_out(statuses, pulls, "/w/b", "fix", Some((8, State::Open, Some(far))));
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        let place = PanePlace { session: listed("main", 20), pane: PaneKey::terminal(1) };
        let place = Place::Pane(place);
        sidebar.cursor = Some(Spot::Remote(nixos(), place));
        assert_eq!(open_pull(&mut sidebar, now), Asked::Browse(far_link.clone()));
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(far_link));
    }

    #[test]
    fn every_key_of_a_row_group_asks_or_says_why_not() {
        let now = moment(160_000);
        let grouped = Action::row().filter(|(_, path)| path.len() == 2);
        for (action, path) in grouped {
            let mut sidebar = pulls();
            sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
            assert_eq!(sidebar.key(&char(path[0]), now), Asked::Nothing, "{path:?} opens a group");
            let asked = sidebar.key(&char(path[1]), now);
            let said = asked != Asked::Nothing || sidebar.notice.is_some();
            assert!(said, "{action:?} on {path:?} does nothing");
        }
    }

    #[test]
    fn o_w_asks_for_the_worktrees_not_open_of_the_row_s_project_on_its_machine() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        let opening = |sidebar: &mut Sidebar| {
            assert_eq!(sidebar.key(&char('o'), now), Asked::Nothing, "o opens a group");
            match sidebar.key(&char('w'), now) {
                Asked::OpenWorktree { on, project } => (on, project.listed().name().to_string()),
                asked => panic!("o w asks for no worktree to open: {asked:?}"),
            }
        };
        assert_eq!(opening(&mut sidebar), (Machine::Local, "s".to_owned()));

        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let place = PanePlace { session: listed("main", 20), pane: PaneKey::terminal(1) };
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Pane(place)));
        assert_eq!(opening(&mut sidebar), (Machine::Remote(nixos()), "main".to_owned()));
    }

    #[test]
    fn the_number_of_a_pull_request_is_a_spot_of_its_own() {
        let mut sidebar = pulls();
        let host = sidebar.host.as_mut().unwrap();
        let panes = ["/w/e", "/w/f"].iter().zip(5..).map(|(cwd, id)| pane(id, id, cwd, "fish"));
        host.sessions[0].panes.as_mut().unwrap().panes.extend(panes);
        let (statuses, pulls) = (&mut host.git_statuses, &mut host.pull_requests);
        let long = "handoff-eng-11259-and-more";
        let (merged, draft) = (LINK.replace("246", "4321"), LINK.replace("246", "12"));
        checked_out(statuses, pulls, "/w/e", long, Some((4321, State::Merged, Some(&merged))));
        checked_out(statuses, pulls, "/w/f", "日本語の枝の名前が長い", Some((12, State::Draft, Some(&draft))));
        let host = sidebar.host.clone().unwrap();
        sidebar.hear(host);

        for keys in [Keys::Zellij, Keys::Sidebar] {
            sidebar.focus(keys);
            let area = Rect::new(0, 0, 32, 32);
            let mut buffer = Buffer::empty(area);
            let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
            let columns = |y: u16, number: &str| {
                let cells: Vec<&str> = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
                let start = cells.windows(number.len()).position(|it| it.concat() == number);
                let start = u16::try_from(start.expect(number)).unwrap();
                (start..start + u16::try_from(number.len()).unwrap()).collect::<Vec<_>>()
            };
            assert_eq!(number_spot(&drawn, 6), columns(6, "#246"));
            for (y, why) in [(9, "old: no link"), (12, "gone: closed"), (15, "none: no number")] {
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
        sidebar.focus(Keys::Zellij);
        let area = Rect::new(0, 0, 32, 8);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!(number_spot(&drawn, 6).len(), "#246".len());

        sidebar.top = 5;
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!((0..8).flat_map(|y| number_spot(&drawn, y)).count(), 0);

        sidebar.top = 0;
        sidebar.notify("something went wrong".to_owned());
        let short = Rect::new(0, 0, 32, 7);
        let drawn = sidebar.draw(&mut Buffer::empty(short), short, moment(160_000));
        assert_eq!(number_spot(&drawn, 6), [] as [u16; 0]);
        sidebar.notice = None;

        let area = Rect::new(0, 0, 32, 24);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 5), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 7), area), Asked::Take);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!((0..24).flat_map(|y| number_spot(&drawn, y)).count(), 0);
    }

    #[test]
    fn a_note_no_longer_so_goes_and_leaves_any_other() {
        let mut sidebar = fresh();
        sidebar.notify("what goes wrong is kept in no log".to_owned());
        sidebar.unsay("no one reads");
        assert_eq!(sidebar.notice.as_deref(), Some("what goes wrong is kept in no log"));
        sidebar.notify("no one reads".to_owned());
        sidebar.unsay("no one reads");
        assert_eq!(sidebar.notice, None);
    }

    #[test]
    fn a_click_on_a_number_copies_and_does_nothing_else() {
        let mut sidebar = pulls();
        sidebar.focus(Keys::Zellij);
        let here = sidebar.cursor.clone();
        let now = moment(160_000);
        sidebar.notify("what went wrong before".to_owned());
        assert_eq!(sidebar.number_clicked(link(), now), Asked::Copy(link()));
        assert_eq!(sidebar.cursor, here);
        assert_eq!(sidebar.keys, Keys::Zellij);
        assert_eq!(sidebar.notice, None);
        assert_eq!(sidebar.copied, Some((link().number(), now)));

        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        sidebar.key(&char('y'), now);
        sidebar.number_clicked(link(), now);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Nothing);
    }
    fn clicks(sidebar: &mut Sidebar, from: u16) -> Vec<(String, Option<Hit>)> {
        let area = Rect::new(0, 0, 32, 20);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
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
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&char('?'), moment(160_000));
        let clicked = clicks(&mut sidebar, 0);
        assert!(clicked[..8].iter().all(|(_, hit)| hit.is_none()), "{clicked:?}");
        let legend: Vec<(&str, Option<Hit>)> =
            clicked[8..].iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        let rule = format!("─ luneta {}", "─".repeat(22));
        assert_eq!(legend, [
            (rule.as_str(), None),
            (" /  Search", Some(on_legend('/'))),
            (" u  Unread", Some(on_legend('u'))),
            (" v  Reorder", Some(on_legend('v'))),
            (" o  Open ▸", Some(on_legend('o'))),
            (" y  Yank ▸", Some(on_legend('y'))),
            (" a  Ask", Some(on_legend('a'))),
            (" c  Skills ▸", Some(on_legend('c'))),
            (" n  New ▸", Some(on_legend('n'))),
            (" r  Rename the view", Some(on_legend('r'))),
            (" d  Delete ▸", Some(on_legend('d'))),
            (" b  Brief ▸", Some(on_legend('b'))),
        ]);
        sidebar.keys_clicked(Keyed::Legend, &char('n'), moment(160_000));
        let clicked = clicks(&mut sidebar, 13);
        let menu: Vec<(&str, Option<Hit>)> =
            clicked.iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        assert_eq!(menu[..], [
            ("─ New ─────────── luneta/main ─", None),
            (" v  new view", Some(on_menu(KeyCode::Char('v')))),
            (" h  new welcome view", Some(on_menu(KeyCode::Char('h')))),
            (" w  new worktree", Some(on_menu(KeyCode::Char('w')))),
            (" p  new project", Some(on_menu(KeyCode::Char('p')))),
            (" m  new machine", Some(on_menu(KeyCode::Char('m')))),
            (" <BS> - Back, <ESC> - Close", Some(on_menu(KeyCode::Backspace))),
        ]);
    }

    #[test]
    fn a_click_on_the_legend_once_the_menu_took_its_place_does_nothing() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&char('?'), moment(160_000));
        sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        let delete = clicks(&mut sidebar, 18);
        assert_eq!(delete[0].1, Some(on_menu(KeyCode::Char('d'))), "{delete:?}");
        let clicked = sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        assert_eq!(clicked, Asked::Nothing);
        assert_eq!(clicks(&mut sidebar, 18), delete);
        let deleted = sidebar.chose(1, Choice::Key(char('d')), moment(160_000));
        assert_ne!(deleted, Asked::Nothing);
    }

    fn shows_at(sidebar: &mut Sidebar, at: SystemTime, start: &str) -> bool {
        seen_at(sidebar, true, 32, 20, at).0.iter().any(|line| line.starts_with(start))
    }

    #[test]
    fn a_group_shows_once_a_pause_passes_and_never_when_typed_fast() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let now = moment(160_000);
        sidebar.key(&char('d'), now);
        assert_eq!(sidebar.due(now), Some(PAUSE));
        let before = now + PAUSE - Duration::from_millis(1);
        assert!(!shows_at(&mut sidebar, before, "─ Delete ─"));
        assert!(!shows_at(&mut sidebar, before, " d  Delete ▸"));
        assert!(shows_at(&mut sidebar, now + PAUSE, "─ Delete ─"));
        assert_eq!(sidebar.due(now + PAUSE), None);

        sidebar.close_menu();
        sidebar.key(&char('d'), now);
        let fast = now + Duration::from_millis(50);
        assert_ne!(sidebar.key(&char('d'), fast), Asked::Nothing);
        assert!(sidebar.menu.as_ref().is_some_and(Menu::quiet));
        let later = now + 10 * PAUSE;
        assert!(!shows_at(&mut sidebar, later, "─ Delete ─"));
        assert!(!shows_at(&mut sidebar, later, " d  Delete ▸"));
        assert_eq!(sidebar.due(later), None);
    }

    #[test]
    fn a_lone_g_a_selection_and_a_move_show_their_keys() {
        let mut sidebar = two_to_move();
        let earlier = moment(160_000) - PAUSE;
        let esc = || Some(Hit::Keys(Keyed::Legend, KeyEvent::from(KeyCode::Esc)));
        sidebar.key(&char('g'), earlier);
        assert_eq!(clicks(&mut sidebar, 18), [
            (format!("─ Go {}", "─".repeat(26)), None),
            (" g  First row".to_owned(), Some(on_legend('g'))),
        ]);
        sidebar.key(&char('g'), earlier);

        sidebar.key(&char('v'), earlier);
        assert_eq!(clicks(&mut sidebar, 15), [
            (format!("─ Reorder {}", "─".repeat(21)), None),
            (" j k  Grow".to_owned(), None),
            (" J K  Move".to_owned(), None),
            (" v    Project, then done".to_owned(), Some(on_legend('v'))),
            (" Esc  Done".to_owned(), esc()),
        ]);
    }

    #[test]
    fn question_mark_opens_the_legend_and_the_next_key_closes_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let now = moment(160_000);
        let legend = " b  Brief ▸";
        assert!(!shows_at(&mut sidebar, now, legend));
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(shows_at(&mut sidebar, now, legend));
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), now), Asked::Nothing);
        assert!(!shows_at(&mut sidebar, now, legend));
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), now), Asked::Leave);

        let here = sidebar.cursor.clone();
        sidebar.key(&char('?'), now);
        sidebar.key(&char('j'), now);
        assert_ne!(sidebar.cursor, here);
        assert!(!shows_at(&mut sidebar, now, legend));

        sidebar.key(&char('?'), now);
        sidebar.key(&char('d'), now);
        assert_eq!(sidebar.due(now), None);
        assert!(shows_at(&mut sidebar, now, "─ Delete ─"));
        sidebar.close_menu();
        sidebar.key(&char('?'), now);
        sidebar.key(&char('y'), now);
        assert!(shows_at(&mut sidebar, now, " p  PR link"));
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);

        sidebar.key(&char('?'), now);
        let unfocused = screen(&mut sidebar, false, 32, 20);
        assert!(!unfocused.iter().any(|line| line == legend), "{unfocused:#?}");
        assert!(!shows_at(&mut sidebar, now, legend));
    }

    #[test]
    fn question_mark_in_the_legend_lists_every_key_and_esc_closes_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let here = sidebar.cursor.clone();
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(!shows_at(&mut sidebar, now, "─ All keys ─"));
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        let every = screen(&mut sidebar, true, 32, 80);
        let start = every.iter().position(|line| line.starts_with("─ All keys ─")).unwrap();
        let listed = &every[start + 1..every.len() - 1];
        let names: Vec<&str> =
            listed.iter().filter(|line| !line.contains("  ")).map(String::as_str).collect();
        assert_eq!(names, [" Move", " Sidebar", " Menu", " Pasted link", " Anywhere"]);
        for line in [
            " g g    First row",
            " ?      All keys",
            " y p  Yank PR link",
            " n v  new view",
            " d d  close the view",
            " d d  close the worktree",
            " r    Rename the project",
            " w  worktree",
            " BS   back a group",
            " Ctrl a       Sidebar or views",
            " Alt Shift u  Stop reading",
        ] {
            assert!(listed.iter().any(|shown| shown == line), "{line:?} in {listed:#?}");
        }
        assert_eq!(every.last().map(String::as_str), Some(" <↓↑> - Scroll, <ESC> - Close"));

        let total = listed.len();
        let short = screen(&mut sidebar, true, 32, 20);
        assert_eq!(short[4], format!("─ All keys ──────────── 14/{total} ─"));
        assert_eq!(short[5], " Move");
        sidebar.key(&char('j'), now);
        assert_eq!(screen(&mut sidebar, true, 32, 20)[5], " j k    Next row, row before");
        for _ in 0..2 * total {
            sidebar.key(&char('j'), now);
        }
        let end = screen(&mut sidebar, true, 32, 20);
        assert_eq!(end[4], format!("─ All keys ──────────── {total}/{total} ─"));
        assert_eq!(end[18], " Alt Shift u  Stop reading");
        sidebar.key(&char('k'), now);
        assert_eq!(screen(&mut sidebar, true, 32, 20)[18], " Alt u        Read aloud, pause");

        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), now), Asked::Nothing);
        assert!(!shows_at(&mut sidebar, now, "─ All keys ─"));
        assert!(!shows_at(&mut sidebar, now, " b  Brief ▸"));
        assert_eq!(sidebar.cursor, here);
    }

    #[test]
    fn a_list_of_keys_short_of_room_ends_in_the_key_that_lists_them_all() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let now = moment(160_000);
        let area = Rect::new(0, 0, 32, 11);
        let drawn = |sidebar: &mut Sidebar| {
            let mut buffer = Buffer::empty(area);
            let drawn = sidebar.draw(&mut buffer, area, now);
            let last = (0..31).map(|x| buffer[(x, 10)].symbol()).collect::<String>();
            (last.trim_end().to_owned(), drawn.at(Position::new(4, 10)))
        };
        sidebar.key(&char('?'), now);
        assert_eq!(drawn(&mut sidebar), (" ?  All keys".to_owned(), Some(on_legend('?'))));
        sidebar.keys_clicked(Keyed::Legend, &char('?'), now);
        assert!(matches!(sidebar.pending, Pending::AllKeys(_)));
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);

        sidebar.key(&char('m'), now);
        let all = Some(on_menu(KeyCode::Char('?')));
        let lines = screen(&mut sidebar, true, 32, 11);
        assert_eq!(lines[9], " ?  All keys", "{lines:#?}");
        let mut buffer = Buffer::empty(area);
        let at = sidebar.draw(&mut buffer, area, now).at(Position::new(4, 9));
        assert_eq!(at, all);
        assert_eq!(sidebar.key(&char('?'), now), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        assert!(matches!(sidebar.pending, Pending::AllKeys(_)));
    }

    #[test]
    fn a_press_or_a_paste_closes_the_legend_and_the_list_of_every_key() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let now = moment(160_000);
        let area = Rect::new(0, 0, 32, 20);
        for opens in [&[char('?')][..], &[char('?'), char('?')]] {
            let shows = if opens.len() == 1 { " b  Brief ▸" } else { "─ All keys ─" };
            for key in opens {
                sidebar.key(key, now);
            }
            assert!(shows_at(&mut sidebar, now, shows), "{shows}");
            click(&mut sidebar, 10, 6, area);
            assert!(!shows_at(&mut sidebar, now, shows), "{shows}");

            for key in opens {
                sidebar.key(key, now);
            }
            sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
            assert!(sidebar.menu.is_some());
            sidebar.drop_menu(sidebar.menus);
            assert!(sidebar.menu.is_none());
            assert!(!shows_at(&mut sidebar, now, shows), "{shows}");
        }
    }

    #[test]
    fn a_link_pasted_in_a_group_s_pause_shows_its_menu_at_once() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let now = moment(160_000);
        sidebar.key(&char('d'), now);
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let soon = now + Duration::from_millis(1);
        assert!(shows_at(&mut sidebar, soon, "─ #5 "));
        assert_eq!(sidebar.due(soon), None);
    }

    #[test]
    fn a_click_on_a_skill_of_the_menu_under_the_rows_starts_an_agent_on_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&char('m'), moment(160_000));
        let cwd = "/w/luneta/main".to_owned();
        let agent = agents::Harness::ClaudeCode;
        let find = Asked::Find { menu: 1, on: Machine::Local, cwd: cwd.clone(), agent };
        assert_eq!(sidebar.key(&char('/'), moment(160_000)), find);
        let name = |name: &str| Name::new(name.to_owned()).expect("a name");
        let skill = |named: &str| Skill {
            name: name(named),
            description: String::new(),
            runs: crate::skills::Runs::ForAnyone,
        };
        sidebar.found(1, agent, Ok(vec![skill("code-review"), skill("tigerstyle")]));
        let clicked = clicks(&mut sidebar, 14);
        let skills: Vec<(&str, Option<Hit>)> =
            clicked[1..5].iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        let on = |skill| Some(Hit::Menu(1, Choice::Skill(name(skill))));
        let tab = Some(Hit::Menu(1, Choice::Key(KeyEvent::from(KeyCode::Tab))));
        assert_eq!(skills, [
            (" _", None),
            (" /code-review", on("code-review")),
            (" /tigerstyle", on("tigerstyle")),
            (" \u{ec82} claude, the view's  <TAB>", tab),
        ]);
        let tigerstyle = || Choice::Skill(name("tigerstyle"));
        assert_eq!(sidebar.chose(2, tigerstyle(), moment(160_000)), Asked::Nothing);
        let command = ["claude", "/tigerstyle"].map(str::to_owned).to_vec();
        let (session, cwd) = (session_name("older"), directory(&cwd));
        let open = Open::Pane { session, beside: 1, cwd, command };
        let key = listed("older", 10).own_key().unwrap();
        let open = Asked::Open { asking: Held::Open, on: Machine::Local, key, open };
        assert_eq!(sidebar.chose(1, tigerstyle(), moment(160_000)), open);
    }

    #[test]
    fn the_menu_starts_agents_in_the_pane_s_harness_or_else_the_one_picked_last() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("agent");
        let mut sidebar = two_sessions();
        sidebar.agent = Kept::within(file.clone()).0;
        if let Some(host) = &mut sidebar.host {
            host.agents[0].harness = Some(Harness::Pi.into());
        }
        sidebar.focus(Keys::Sidebar);
        let started = |session: &str, beside, cwd: &str, harness, spelled| {
            let command = [harness, spelled].map(str::to_owned).to_vec();
            let key = listed(session, if session == "older" { 10 } else { 20 }).own_key().unwrap();
            let (session, cwd) = (session_name(session), directory(cwd));
            let open = Open::Pane { session, beside, cwd, command };
            Asked::Open { asking: Held::Open, on: Machine::Local, key, open }
        };
        type_in(&mut sidebar, "c");
        let pi = started("older", 1, "/w/luneta/main", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);

        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        sidebar.step(2);
        type_in(&mut sidebar, "c");
        let claude = started("newer", 1, "/w/b", "claude", "/tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), claude);

        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "c");
        let tab = KeyEvent::from(KeyCode::Tab);
        assert_eq!(sidebar.key(&tab, moment(160_000)), Asked::Nothing);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "pi\n");
        let pi = started("newer", 1, "/w/b", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "c");
        let pi = started("newer", 1, "/w/b", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
    }

    #[test]
    fn a_key_of_the_menu_opens_it_for_the_highlighted_row() {
        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar);
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 17, moment(160_000) + PAUSE);
        assert_eq!(lines[10..], [
            "─ New ─────────── luneta/main ─",
            " v  new view",
            " h  new welcome view",
            " w  new worktree",
            " p  new project",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        let (project, branch) = (ProjectId::of(1).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree { on: Machine::Local, project, branch };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        assert!(sidebar.menu.is_none());

        sidebar.cursor = Some(Spot::Local(own_pane_of("young", 2, 1)));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(2).unwrap(), Branch::parse("fix").unwrap());
        let made = Asked::NewWorktree { on: Machine::Local, project, branch };
        assert_eq!(branched(&mut sidebar, "fix"), made);
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    #[test]
    fn an_empty_project_is_deleted_by_its_id() {
        let mut sidebar = fresh();
        let own = own_session("own", 1, Vec::new());
        let sessions = vec![own_session("here", 2, Vec::new()), own];
        sidebar.hear(host(sessions, Vec::new()));
        sidebar.focus(Keys::Sidebar);
        let remove = |sidebar: &mut Sidebar| {
            let own = Listed::new_project("own", 1);
            sidebar.cursor = Some(Spot::Local(Place::Session(own)));
            type_in(sidebar, "d");
            sidebar.key(&char('d'), moment(160_000))
        };
        let deleted = delete_here(project_deleted("own", 1, &[], &[]));
        assert_eq!(remove(&mut sidebar), deleted);
        let made = OwnMade::from_nanos(0);
        sidebar.attaching(Machine::Local, OwnKey { id: OwnId::new(2).unwrap(), made });
        assert_eq!(remove(&mut sidebar), deleted);
    }

    #[test]
    fn an_own_client_is_where_its_server_says_and_closes_its_project_to_delete_it() {
        let mut sidebar = fresh();
        let panes = || vec![pane(1, 0, "/w/luneta/main", "fish")];
        let sessions = vec![own_session("twin", 1, panes()), own_session("newest", 2, Vec::new())];
        sidebar.hear(host(sessions, Vec::new()));
        let id = OwnId::new(1).unwrap();
        let key = OwnKey { id, made: OwnMade::from_nanos(0) };
        sidebar.attaching(Machine::Local, key);
        assert_eq!(sidebar.here_place(), Err(NO_HERE_PANE));
        assert_eq!(sidebar.user(&Machine::Local), User::Untold);
        let client = OwnClientId::new(4).unwrap();
        sidebar.went_own(OwnPane::new(key, 1).unwrap(), client);
        let twin = Listed::new_project("twin", 1);
        let here = PanePlace { session: twin.clone(), pane: PaneKey::terminal(1) };
        assert_eq!(sidebar.here_place(), Ok(here));
        assert_eq!(sidebar.user(&Machine::Local), User::Untold, "a project's row names no user");
        assert_eq!(sidebar.user(&Machine::Remote(nixos())), User::Away);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(Place::Session(twin)));
        type_in(&mut sidebar, "d");
        let deleted = project_deleted("twin", 1, &[1000], &[1]);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), delete_here(deleted));
    }

    #[test]
    fn luneta_unseated_sits_in_no_project_and_its_delete_leaves_none() {
        let mut sidebar = fresh();
        let panes = || vec![pane(1, 0, "/w/luneta/main", "fish")];
        let sessions = vec![own_session("twin", 1, panes()), own_session("newest", 2, Vec::new())];
        sidebar.hear(host(sessions, Vec::new()));
        let key = OwnKey { id: OwnId::new(1).unwrap(), made: OwnMade::from_nanos(0) };
        sidebar.attaching(Machine::Local, key);
        sidebar.went_own(OwnPane::new(key, 1).unwrap(), OwnClientId::new(4).unwrap());
        sidebar.unseat();
        assert_eq!(sidebar.own_seat(), None);
        let (twin, pane) = (Listed::new_project("twin", 1), PaneKey::terminal(1));
        sidebar.cursor = Some(Spot::Local(Place::Pane(PanePlace { session: twin.clone(), pane })));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes_here(1, twin, pane));
        let kill = Act::Kill { session: session_name("twin"), whose: Whose::Own(key) };
        assert_eq!(sidebar.closes(1, Ok(Closes::Session)), delete_here(kill));
    }

    #[test]
    fn a_pull_request_s_link_pasted_opens_the_menu_for_it() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_pane_of("older", 1, 1));
        if let Some(host) = &mut sidebar.host {
            let number = NonZeroU32::new(5).unwrap();
            let url = Some("https://github.com/lorenzolfm/luneta/pull/5".to_owned());
            let state = State::Open;
            let pull_request = PullRequest { number, state, url, checks: None, stale: false };
            host.pull_requests.push(pull_request.to_api("/w/b"));
        }
        sidebar.focus(Keys::Sidebar);
        sidebar.paste("ls -la");
        assert!(sidebar.menu.is_none());
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5\n");
        let lines = screen(&mut sidebar, true, 32, 17);
        assert_eq!(lines[12..], [
            "─ #5 ────── lorenzolfm/luneta ─",
            " w  worktree",
            " r  /thermo-nuclear-review",
            " \u{ec82} claude  <TAB> another agent",
            " <ESC> - Close",
        ]);
        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let worked = |session, pane, dir: &str, pull, here| Worked {
            session,
            pane,
            works: links::Worked { dir: dir.to_owned(), pull, here },
        };
        let worked = vec![
            worked(own_listed("older", 1), PaneKey::terminal(1), "/w/luneta/main", None, true),
            worked(own_listed("older", 1), PaneKey::terminal(2), "/w/c", None, true),
            worked(own_listed("young", 2), PaneKey::terminal(1), "/w/b", Some(link.clone()), false),
        ];
        let work = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let on = Machine::Local;
        let pull = Asked::Pull { menu: 1, on, work, wanted, worked, others: Vec::new() };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);

        let at = At::Clone { cwd: directory("/w/n"), name: session_name("newer-2") };
        let work = Readied::Link(link);
        let folder = "/w/n/feat".to_owned();
        let ready = Ready { at, folder, branch: None, moved_off: false, work };
        let asked = Asked::Make {
            menu: 1,
            on: Machine::Local,
            session: session_name("newer-2"),
            holder: directory("/w/n"),
            cwd: directory("/w/n/feat"),
            name: "#5".to_owned(),
            command: Vec::new(),
        };
        assert_eq!(sidebar.pulled(1, Ok(Found { on: Machine::Local, ready })), asked);
        assert_eq!(sidebar.made_for(1), Some(Held::Open));
        assert!(sidebar.menu.is_none());
        assert_eq!(sidebar.made_for(1), None);
    }

    #[test]
    fn what_a_key_of_a_pasted_link_started_goes_on_once_the_keys_leave_the_menu() {
        let page = "https://github.com/lorenzolfm/luneta/pull/5";
        let link = || Link::parse(page).unwrap();
        let number = 1;
        let pulling = || {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.paste(page);
            let Asked::Pull { .. } = sidebar.key(&char('r'), moment(160_000)) else {
                panic!("no job for the key");
            };
            assert!(sidebar.menu.as_ref().is_some_and(Menu::pulling));
            sidebar
        };
        let ready = || {
            let work = Readied::Link(link());
            let at = At::Session((own_listed("older", 1), PaneKey::terminal(1)));
            let folder = "/w/luneta/feat".to_owned();
            Ready { at, folder, branch: None, moved_off: false, work }
        };

        let mut sidebar = pulling();
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
        assert_eq!(sidebar.working.len(), 1);
        let lines = screen(&mut sidebar, false, 32, 17);
        assert!(lines.iter().any(|line| line.contains("Syncing #5…")), "{lines:?}");
        let found = || Found { on: Machine::Local, ready: ready() };
        let asked = sidebar.pulled(number, Ok(found()));
        assert!(matches!(asked, Asked::Open { asking: Held::Kept, .. }), "{asked:?}");
        assert!(sidebar.working.is_empty());

        let mut sidebar = pulling();
        let asked = sidebar.pulled(number, Ok(found()));
        assert!(matches!(asked, Asked::Open { asking: Held::Open, .. }), "{asked:?}");
        assert!(sidebar.menu.is_none());

        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste(page);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.focus(Keys::Zellij);
        let there = || {
            let work = Readied::Link(link());
            let at = At::There((own_listed("older", 1), PaneKey::terminal(4)));
            Ready {
                at,
                folder: "/w/luneta/feat".to_owned(),
                branch: None,
                moved_off: false,
                work,
            }
        };
        let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready: there() }));
        assert_eq!(asked, Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("menu: older:4 has it; nobody went"));
    }

    #[test]
    fn a_tab_a_pasted_link_s_key_opens_leaves_no_menu_waiting() {
        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let work = Readied::Link(link);
        let at = At::Session((own_listed("older", 1), PaneKey::terminal(1)));
        let folder = "/w/luneta/feat".to_owned();
        let ready = Ready { at, folder: folder.clone(), branch: None, moved_off: false, work };
        let (cwd, name) = (directory(&folder), Some("#5".to_owned()));
        let tab = Tab { cwd, name, command: Vec::new() };
        let open = Open::Tab { session: session_name("older"), tab, alone_since: None };
        let key = own_listed("older", 1).own_key().unwrap();
        let asked = Asked::Open { asking: Held::Open, on: Machine::Local, key, open };
        assert_eq!(sidebar.pulled(number, Ok(Found { on: Machine::Local, ready })), asked);
        assert!(sidebar.menu.is_none());
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn esc_closes_a_menu_that_waits_and_what_it_started_goes_on() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000)), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        assert!(sidebar.working.contains_key(&number));
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "m");
        assert!(sidebar.menu.is_some());
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000));
        assert!(sidebar.menu.is_none());
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_link_pasted_again_while_its_worktree_goes_on_makes_nothing_of_its_own() {
        let page = "https://github.com/lorenzolfm/luneta/pull/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste(page);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        sidebar.paste(page);
        assert_eq!(sidebar.menu.as_ref().map(Menu::number), Some(number));
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.working.contains_key(&number));
        sidebar.notice = None;
        sidebar.focus(Keys::Zellij);
        sidebar.paste(page);
        assert!(sidebar.menu.is_none());
        assert_eq!(sidebar.notice.as_deref(), Some("#5 is on its way"));
        sidebar.paste("https://github.com/lorenzolfm/luneta/issues/6");
        assert!(sidebar.menu.is_some());
    }

    #[test]
    fn a_link_pasted_again_before_a_key_opens_its_menu_again() {
        let page = "https://github.com/lorenzolfm/luneta/pull/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste(page);
        sidebar.paste(page);
        assert_eq!(sidebar.menu.as_ref().map(Menu::number), Some(2));
        assert_eq!(sidebar.notice, None);
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_menu_that_waits_goes_on_when_another_takes_its_place() {
        let waiting = || {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
            let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
                panic!("no job for the key");
            };
            sidebar
        };
        let mut sidebar = waiting();
        sidebar.paste("https://github.com/lorenzolfm/luneta/issues/6");
        assert_eq!(sidebar.menu.as_ref().map(Menu::number), Some(2));
        assert!(sidebar.working.contains_key(&1));

        let mut sidebar = waiting();
        sit(&mut sidebar, &own_pane_of("older", 1, 1));
        assert_eq!(sidebar.float_menu(), Ok(()));
        assert_eq!(sidebar.menu.as_ref().map(Menu::number), Some(2));
        assert!(sidebar.working.contains_key(&1));
    }

    #[test]
    fn a_kept_menu_s_last_answer_leaves_the_open_menu_alone() {
        let mut sidebar = own_two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(161_000));
        let page = "https://github.com/lorenzolfm/luneta/issues/6";
        sidebar.paste(page);
        let Asked::Pull { .. } = sidebar.key(&char('w'), moment(162_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(sidebar.pulled(1, Err("gh failed".to_owned())), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("menu: lorenzolfm/luneta#5: gh failed"));
        assert!(sidebar.working.is_empty());
        assert_eq!(sidebar.menu.as_ref().map(Menu::number), Some(2));
        let work = Readied::Link(Link::parse(page).unwrap());
        let at = At::Session((own_listed("older", 1), PaneKey::terminal(1)));
        let folder = "/w/luneta/issue-6".to_owned();
        let ready = Ready { at, folder, branch: None, moved_off: false, work };
        let asked = sidebar.pulled(2, Ok(Found { on: Machine::Local, ready }));
        assert!(matches!(asked, Asked::Open { asking: Held::Open, .. }), "{asked:?}");
        assert!(sidebar.menu.is_none());
    }

    #[test]
    fn a_md_file_s_path_pasted_opens_the_menu_for_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        for text in [
            "notes.txt",
            "two\nlines.md",
            "https://example.com/plan.md",
            "github.com/o/r/blob/main/README.md",
            ".md",
        ] {
            sidebar.paste(text);
            assert!(sidebar.menu.is_none(), "{text:?}");
        }
        let Some(Target::Pane(on, row)) = sidebar.target() else { panic!("not a pane's row") };
        let cwd = row.cwd.clone();
        sidebar.paste("  docs/plans/sync-rework.md\n");
        let lines = screen(&mut sidebar, true, 39, 17);
        assert_eq!(lines[12..], [
            "─ sync-rework ─ plans/sync-rework.md ─",
            " w  worktree",
            " t  tackle",
            " \u{ec82} claude  <TAB> another agent",
            " <ESC> - Close",
        ]);
        let Asked::Pull { on: asked_on, work, .. } = sidebar.key(&char('t'), moment(160_000))
        else {
            panic!("no job for the key");
        };
        assert_eq!(asked_on, on);
        let from = cwd.map(|cwd| Directory::parse(cwd).unwrap());
        assert_eq!(Some(work), Work::parse("docs/plans/sync-rework.md", from));

        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(Place::Session(listed("older", 10))));
        sidebar.paste("/w/plan.md");
        let Asked::Pull { work, .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(Some(work), Work::parse("/w/plan.md", None));
    }

    #[test]
    fn w_says_a_worktree_moved_off_its_branch_and_t_starts_no_agent_on_it() {
        let page = "https://github.com/lorenzolfm/luneta/issues/5";
        let pressed = |key: char, branch: Option<&str>, moved_off: bool| {
            let mut sidebar = own_two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.paste(page);
            let number = sidebar.menu.as_ref().map_or(0, Menu::number);
            let Asked::Pull { wanted, .. } = sidebar.key(&char(key), moment(160_000)) else {
                panic!("no job for the key");
            };
            assert_eq!(wanted, Wanted::AsItIs);
            let at = At::Session((own_listed("older", 1), PaneKey::terminal(1)));
            let (folder, work) = ("/w/luneta/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let branch = branch.map(str::to_owned);
            let ready = Ready { at, folder: folder.to_owned(), branch, moved_off, work };
            let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready }));
            (matches!(asked, Asked::Open { .. }), sidebar.notice)
        };
        let on_dev_docs = "#5's worktree is on dev-docs, not issue-5";
        assert_eq!(pressed('w', Some("dev-docs"), true), (true, Some(on_dev_docs.to_owned())));
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
        let page = "https://github.com/lorenzolfm/luneta/issues/5";
        for keys in [Keys::Sidebar, Keys::Menu { lent: true }] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.focus(keys);
            sidebar.paste(page);
            let number = sidebar.menu.as_ref().map_or(0, Menu::number);
            let Asked::Pull { .. } = sidebar.key(&char('t'), moment(160_000)) else {
                panic!("no job for the key");
            };
            let at = At::Session((listed("older", 10), PaneKey::terminal(1)));
            let branch = Some("dev-docs".to_owned());
            let (folder, work) = ("/w/luneta/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let ready = Ready { at, folder: folder.to_owned(), branch, moved_off: true, work };
            let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready }));
            assert_eq!(asked, Asked::Nothing);
            assert_eq!(sidebar.floating().is_some(), matches!(keys, Keys::Menu { .. }));
            assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, None));
            let Asked::Pull { wanted: Wanted::AsItIs, .. } =
                sidebar.key(&char('w'), moment(161_000))
            else {
                panic!("no job for the key");
            };
        }
    }

    #[test]
    fn a_row_by_no_absolute_path_is_no_directory_for_a_file_s_path() {
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(1, 0, "", "claude") };
        let elsewhere = Agent { cwd: "w/luneta".to_owned(), ..agent("s", 1, Status::Idle, None) };
        sidebar.hear(host(vec![session("s", 10, vec![unsaid])], vec![elsewhere]));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "j");
        assert_eq!(sidebar.target().as_ref().and_then(Target::cwd), Some("w/luneta"));
        sidebar.paste("docs/plan.md");
        let Asked::Pull { work, .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(Some(work), Work::parse("docs/plan.md", None));
    }

    #[test]
    fn the_menu_on_an_agent_is_for_where_its_row_says_it_works() {
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(1, 0, "", "claude") };
        let agents = vec![agent("s", 1, Status::Idle, None)];
        sidebar.hear(host(vec![session("s", 10, vec![unsaid])], agents));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "j");
        assert_eq!(cursor(&sidebar), Some(pane_of("s", 10, 1)));
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 13, moment(160_000) + PAUSE);
        assert_eq!(lines[6], "─ New ─────────── luneta/main ─", "{lines:?}");
        let (session, cwd) = (session_name("s"), directory("/w/luneta/main"));
        let pane = Open::Pane { session, beside: 1, cwd, command: Vec::new() };
        let key = listed("s", 10).own_key().unwrap();
        let pane = Asked::Open { asking: Held::Open, on: Machine::Local, key, open: pane };
        assert_eq!(sidebar.key(&char('v'), moment(160_000)), pane);
    }

    #[test]
    fn the_menu_on_a_shell_s_row_starts_an_agent_on_its_last_command() {
        let mut sidebar = fresh();
        let failed = in_shell(1, ended("cargo test", Some(101), 3000));
        let busy = in_shell(2, going("make", 100, Front::Working));
        sidebar.hear(host(vec![session("s", 10, vec![failed, busy])], Vec::new()));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "gg");
        assert_eq!(cursor(&sidebar), Some(pane_of("s", 10, 1)));
        type_in(&mut sidebar, "c");
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        let command = Said::new("cargo test".to_owned()).unwrap();
        let failed = menu::Ended { command, ending: Some(Ending::Exited(101)), at };
        let (on, s) = (Machine::Local, listed("s", 10));
        let pane = PaneKey::terminal(1);
        let printed = Asked::Printed { menu: 1, on, session: s, pane, ended: failed };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        let nothing = || Ok(Printed::Nothing);
        assert_eq!(sidebar.printed(2, nothing()), Asked::Nothing);
        let prompt = "Fix this. In my shell, `cargo test` ended with exit status 101. It printed \
                      nothing.";
        let command = vec!["claude".to_owned(), prompt.to_owned()];
        let cwd = directory("/w/luneta/main");
        let open = Open::Pane { session: session_name("s"), beside: 1, cwd, command };
        let key = listed("s", 10).own_key().unwrap();
        let open = Asked::Open { asking: Held::Open, on: Machine::Local, key, open };
        assert_eq!(sidebar.printed(1, nothing()), open);
        assert!(sidebar.menu.is_none());

        type_in(&mut sidebar, "jcf");
        assert!(sidebar.menu.is_none());
        let why = "menu: view 2 runs `make` still, and what it prints is not over";
        assert_eq!(sidebar.notice.as_deref(), Some(why));

        type_in(&mut sidebar, "c");
        let shells = |busy: Doing| {
            let failed = in_shell(1, ended("cargo test", Some(101), 3000));
            host(vec![session("s", 10, vec![failed, in_shell(2, busy)])], Vec::new())
        };
        sidebar.hear(shells(ended("make", Some(2), 3000)));
        let command = Said::new("make".to_owned()).unwrap();
        let make = menu::Ended { command, ending: Some(Ending::Exited(2)), at };
        let (on, s) = (Machine::Local, listed("s", 10));
        let pane = PaneKey::terminal(2);
        let printed = Asked::Printed { menu: 3, on, session: s, pane, ended: make };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        sidebar.hear(shells(going("make", 200, Front::Working)));
        assert_eq!(sidebar.printed(3, nothing()), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let why = "menu: view 2's shell went on as luneta read what `make` printed; ask again";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    #[test]
    fn a_last_command_s_agent_opens_where_the_keys_left_the_menu() {
        let mut sidebar = fresh();
        let failed = in_shell(1, ended("cargo test", Some(101), 3000));
        sidebar.hear(host(vec![own_session("s", 1, vec![failed])], Vec::new()));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "ggc");
        let asked = sidebar.key(&char('f'), moment(160_000));
        let project = own_listed("s", 1);
        let printed = matches!(&asked, Asked::Printed { session, .. } if *session == project);
        assert!(printed, "{asked:?}");
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        sidebar.focus(Keys::Zellij);
        assert_eq!(sidebar.working.len(), 1);
        let lines = screen(&mut sidebar, false, 40, 17);
        let notes: String = lines[14..].join(" ");
        assert!(notes.contains("⠋ Reading what `cargo test` printed…"), "{lines:?}");
        let asked = sidebar.printed(number, Ok(Printed::Nothing));
        assert!(matches!(asked, Asked::Open { asking: Held::Kept, .. }), "{asked:?}");
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_kept_menu_whose_next_ask_is_not_made_keeps_nothing() {
        let page = "https://github.com/lorenzolfm/luneta/pull/5";
        let left = |at| {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.paste(page);
            let number = sidebar.menu.as_ref().map_or(0, Menu::number);
            let Asked::Pull { .. } = sidebar.key(&char('w'), moment(160_000)) else {
                panic!("no job for the key");
            };
            sidebar.focus(Keys::Zellij);
            let work = Readied::Link(Link::parse(page).unwrap());
            let folder = "/w/luneta/feat".to_owned();
            let ready = Ready { at, folder, branch: None, moved_off: false, work };
            let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready }));
            (sidebar, number, asked)
        };
        let made = At::Clone { cwd: directory("/w/n"), name: session_name("luneta") };
        let (mut sidebar, number, asked) = left(made);
        assert!(matches!(asked, Asked::Make { menu, .. } if menu == number), "{asked:?}");
        assert_eq!(sidebar.made_for(number), Some(Held::Kept));
        assert!(sidebar.working.is_empty());
    }

    #[test]
    fn a_kept_menu_that_starts_no_agent_on_a_worktree_moved_off_says_so() {
        let page = "https://github.com/lorenzolfm/luneta/issues/5";
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.paste(page);
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let Asked::Pull { .. } = sidebar.key(&char('t'), moment(160_000)) else {
            panic!("no job for the key");
        };
        sidebar.focus(Keys::Zellij);
        let at = At::Session((listed("older", 10), PaneKey::terminal(1)));
        let branch = Some("dev-docs".to_owned());
        let (folder, work) = ("/w/luneta/issue-5", Readied::Link(Link::parse(page).unwrap()));
        let ready = Ready { at, folder: folder.to_owned(), branch, moved_off: true, work };
        let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready }));
        assert_eq!(asked, Asked::Nothing);
        assert!(sidebar.working.is_empty());
        let notice = sidebar.notice.as_deref().unwrap_or_default();
        assert!(notice.starts_with("menu: /w/luneta/issue-5 is "), "{notice}");
        let tail = "so no agent starts there; w opens it once #5 is pasted again";
        assert!(notice.ends_with(tail), "{notice}");
    }

    #[test]
    fn the_menu_floats_over_the_project_for_the_pane_luneta_is_on() {
        let mut sidebar = fresh();
        let mut host = host(
            vec![
                own_session("young", 2, vec![pane(1, 0, "/w/b", "fish")]),
                own_session("older", 1, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            ],
            vec![in_own(agent("older", 1, Status::Waiting, Some("fold")), 1, 1)],
        );
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/menu".to_owned(),
            upstream: None,
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/luneta/main".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        let made = OwnMade::from_nanos(0);
        sidebar.attaching(Machine::Local, OwnKey { id: OwnId::new(1).unwrap(), made });
        assert_eq!(sidebar.float_menu(), Err(NO_HERE_PANE));
        assert!(sidebar.floating().is_none());
        sit(&mut sidebar, &own_pane_of("older", 1, 1));

        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "hhj");
        assert!(folded(&sidebar, "older"));
        assert_eq!(cursor(&sidebar), Some(own_pane_of("young", 2, 1)));
        let standing = |sidebar: &mut Sidebar| {
            let area = Rect::new(0, 0, 32, 14);
            let mut buffer = Buffer::empty(area);
            sidebar.draw(&mut buffer, area, moment(160_000));
            let row = |y| (0..32).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            (0..14).map(row).collect::<Vec<_>>()
        };
        sidebar.focus(Keys::Lent);
        let lent = standing(&mut sidebar);
        sidebar.focus(Keys::Sidebar);
        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: true });
        assert_eq!(standing(&mut sidebar), lent);
        let target = sidebar.floating().map(Menu::target).cloned();
        assert_eq!(target, Some(Target::Pane(Machine::Local, PaneRow {
            session: Listed::new_project("older", 1),
            pane: PaneKey::terminal(1),
            terminal: ViewId::of(1),
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/menu".to_owned()),
            repository: None,
            pull_request: None,
            ran: Ran::Unhooked,
        })));

        let (older, cwd) = (session_name("older"), directory("/w/luneta/main"));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(1).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree { on: Machine::Local, project, branch };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        assert!(sidebar.floating().is_none());
        sidebar.focus(Keys::Zellij);

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "c");
        let command = ["claude", "/thermo-nuclear-review"];
        let command = command.map(str::to_owned).to_vec();
        let pane = Open::Pane { session: older, beside: 1, cwd, command };
        let key = own_listed("older", 1).own_key().unwrap();
        let pane = Asked::Open { asking: Held::Open, on: Machine::Local, key, open: pane };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), pane);
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    #[test]
    fn the_agent_here_is_the_one_in_the_pane_luneta_s_zellij_is_on() {
        let on_pane = |id: u32| {
            let mut sidebar = fresh();
            let panes = vec![
                pane(1, 0, "/w/a", "claude"),
                pane(2, 0, "/w/b", "fish"),
                pane(3, 0, "/w/c", "fish"),
                pane(4, 0, "/w/d", "pi"),
            ];
            let printing = agent("s", 3, Status::Busy, None);
            let printing = Agent { entrypoint: Some("sdk-cli".to_owned()), ..printing };
            let pi = Agent {
                harness: Some(Harness::Pi.into()),
                entrypoint: Some("tui".to_owned()),
                ..agent("s", 4, Status::Idle, None)
            };
            let agents = vec![agent("s", 1, Status::Idle, None), printing, pi];
            let agents = agents.into_iter().map(|agent| {
                let pane = agent.pane.clone();
                let Some(api::agent::Pane::Zellij(zellij)) = pane else {
                    panic!("an agent of a pane of zellij's");
                };
                in_own(agent, 1, zellij.pane)
            });
            sidebar.hear(host(vec![own_session("s", 1, panes)], agents.collect()));
            assert_eq!(sidebar.here_agent(), None);
            sit(&mut sidebar, &own_pane_of("s", 1, id));
            sidebar.here_agent()
        };
        let session = SessionName::running("s").expect("a name");
        let (dir, harness) = ("/w/a".to_owned(), agents::Harness::ClaudeCode);
        let here = HereAgent { on: Machine::Local, session, pane: 1, dir, harness };
        assert_eq!(on_pane(1), Some(here));
        assert_eq!(on_pane(2), None);
        assert_eq!(on_pane(3), None);
        let session = SessionName::running("s").expect("a name");
        let (dir, harness) = ("/w/d".to_owned(), agents::Harness::Pi);
        let here = HereAgent { on: Machine::Local, session, pane: 4, dir, harness };
        assert_eq!(on_pane(4), Some(here));
    }

    #[test]
    fn the_menu_over_the_session_says_over_it_what_went_wrong() {
        let mut sidebar = fresh();
        let unknown = Pane { cwd: None, ..pane(1, 0, "/w/b", "fish") };
        sidebar.hear(host(vec![own_session("only", 1, vec![unknown])], Vec::new()));
        sit(&mut sidebar, &own_pane_of("only", 1, 1));
        let wrong = "menu: luneta does not know yet where view 1 works; try again in a moment";

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "nv");
        assert!(sidebar.floating().is_none());
        assert_eq!(sidebar.said().as_deref(), Some(wrong));
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, None));
        sidebar.focus(Keys::Zellij);

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "nv");
        sidebar.focus(Keys::Zellij);
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, Some(wrong)));

        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "nv");
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, Some(wrong)));
        assert_eq!(sidebar.float_menu(), Ok(()));
        assert!(sidebar.notice.is_none());
    }

    #[test]
    fn the_brief_s_keys_brief_you_by_the_row_the_menu_was_for() {
        let mut sidebar = two_sessions();
        let mut host = sidebar.host.clone().expect("a host");
        let id = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";
        host.agents[0].session_id = id.to_owned();
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/brief".to_owned(),
            upstream: None,
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
        host.git_statuses = vec![api::GitStatus {
            dir: "/w/luneta/main".to_owned(),
            stands: Some(api::git_status::Stands::Checkout(checkout)),
        }];
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        let older = || SessionName::parse("older").unwrap();
        let fold = Note {
            on: Machine::Local,
            session: older(),
            pane: 1,
            name: "fold".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: Some("feat/brief".to_owned()),
            cwd: "/w/luneta/main".to_owned(),
            transcript: Transcript::Named(SessionId::parse(id).unwrap()),
        };
        let brief = |over: Over| Asked::Brief(Brief::new(over, vec![fold.clone()]).unwrap());
        let here = |sessions: [&str; 2]| {
            let sessions = sessions.map(|it| (Machine::Local, SessionName::parse(it).unwrap()));
            Over::Sessions { sessions: sessions.into(), unknown: Vec::new() }
        };
        let every = || here(["older", "newer"]);
        let session = || Over::Session(Machine::Local, older());
        let agent = Over::Agent(Machine::Local, older(), 1);
        for (key, over) in [('z', every()), ('s', session()), ('h', agent)] {
            type_in(&mut sidebar, "b");
            assert_eq!(sidebar.key(&char(key), moment(160_000)), brief(over), "{key}");
            assert!(sidebar.menu.is_none(), "{key}");
        }
        sidebar.step(1);
        type_in(&mut sidebar, "bh");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agent in this view"));
        sidebar.step(1);
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents in newer"));
        sidebar.step(-2);
        type_in(&mut sidebar, "h");
        let tab = WorktreePlace { project: project_row("older", 10), worktree: tab_of(10, 0) };
        assert_eq!(cursor(&sidebar), Some(Place::Worktree(tab)));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), brief(session()));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(every()));
    }

    #[test]
    fn a_brief_of_pi_s_agents_alone_leaves_a_note() {
        let mut sidebar = fresh();
        let (of_pi_id, of_claude_id) = ("01a05a24-2bcb-7198", "7c22-5d4c-3b2a");
        let of_pi = |agent: Agent| Agent {
            harness: Some(Harness::Pi.into()),
            session_id: of_pi_id.to_owned(),
            ..agent
        };
        let pi = of_pi(agent("both", 1, Status::Idle, Some("pi")));
        let claude = Agent {
            session_id: of_claude_id.to_owned(),
            ..agent("both", 2, Status::Idle, Some("claude"))
        };
        let alone = of_pi(agent("alone", 3, Status::Idle, Some("alone")));
        let sessions = vec![
            session("both", 10, vec![pane(1, 0, "/w/a", "pi"), pane(2, 1, "/w/b", "claude")]),
            session("alone", 20, vec![pane(3, 0, "/w/c", "pi")]),
        ];
        sidebar.hear(host(sessions, vec![pi, claude, alone]));
        sidebar.focus(Keys::Sidebar);
        let note = "brief: luneta reads only Claude Code's transcripts so far";

        for (place, key) in [(pane_of("both", 10, 1), 'h'), (pane_of("alone", 20, 3), 's')] {
            sidebar.cursor = Some(Spot::Local(place));
            type_in(&mut sidebar, "b");
            assert_eq!(sidebar.key(&char(key), moment(160_000)), Asked::Nothing, "{key}");
            assert_eq!(sidebar.notice.as_deref(), Some(note), "{key}");
        }

        sidebar.cursor = Some(Spot::Local(pane_of("both", 10, 1)));
        type_in(&mut sidebar, "b");
        let told = |id: u32, name: &str, cwd: &str, transcript| Note {
            on: Machine::Local,
            session: SessionName::parse("both").unwrap(),
            pane: id,
            name: name.to_owned(),
            status: "idle".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: None,
            cwd: cwd.to_owned(),
            transcript,
        };
        let notes = vec![
            told(1, "pi", "/w/a", Transcript::NotRead),
            told(2, "claude", "/w/b", Transcript::Named(SessionId::parse(of_claude_id).unwrap())),
        ];
        let over = Over::Session(Machine::Local, SessionName::parse("both").unwrap());
        let brief = Asked::Brief(Brief::new(over, notes).unwrap());
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), brief);
    }

    #[test]
    fn the_brief_goes_in_lunetad_s_order_with_this_machine_first() {
        let mut sidebar = fresh();
        sidebar.hear(two_each());
        let on = || Machine::Remote(nixos());
        sidebar.focus(Keys::Sidebar);
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
            pane: 1,
            name: "fix".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: None,
            cwd: "/w/b".to_owned(),
            transcript: Transcript::Unnamed,
        };
        let every = Brief::new(Over::Sessions { sessions, unknown }, vec![fix]).unwrap();
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), Asked::Brief(every));
    }

    #[test]
    fn the_brief_s_keys_brief_you_on_another_machine_s_agents() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let on = || Machine::Remote(nixos());
        let main = || SessionName::parse("main").unwrap();
        let fix = Note {
            on: on(),
            session: main(),
            pane: 1,
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
        let every = Over::Sessions { sessions: vec![(Machine::Local, main()), (on(), main())], unknown };
        let session = || Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        let pane = pane_of("main", 20, 1);
        for (spot, key, over) in [
            (session(), 'z', every),
            (session(), 's', Over::Session(on(), main())),
            (session(), 'h', Over::Session(on(), main())),
            (Spot::Remote(nixos(), pane.clone()), 'h', Over::Agent(on(), main(), 1)),
        ] {
            sidebar.cursor = Some(spot);
            type_in(&mut sidebar, "b");
            assert_eq!(sidebar.key(&char(key), moment(160_000)), brief(over), "{key}");
        }
        let here = || Spot::Local(Place::Session(listed("main", 10)));
        sidebar.cursor = Some(here());
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents in main"));
        sidebar.cursor = Some(Spot::Remote(nixos(), pane));
        type_in(&mut sidebar, "b");
        let mut host = machines();
        host.remotes[0] = remote("nixos", down(100));
        sidebar.hear(host.clone());
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("brief: lunetad does not reach nixos now"));
        sidebar.cursor = Some(here());
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), Asked::Nothing);
        let nowhere = "brief: no agents to brief you on; lunetad does not reach nixos, me@vps, pi \
                       now";
        assert_eq!(sidebar.notice.as_deref(), Some(nowhere));
        sidebar.hear(machines());
        sidebar.cursor = Some(session());
        type_in(&mut sidebar, "b");
        host.remotes.remove(0);
        sidebar.hear(host);
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("brief: lunetad does not reach nixos now"));
    }

    #[test]
    fn a_delete_asks_first_while_luneta_cannot_tell_the_agents() {
        let unread = "lunetad cannot read 1 of Claude Code's session files";
        let unplaced = format!("{unread}, nor tell where its agent runs");
        let away: fn(&mut Sidebar) = |sidebar| sidebar.lost("gone".to_owned());
        let unreadable: fn(&mut Sidebar) = |sidebar| {
            let host = sidebar.host.clone().expect("the sidebar has heard the host");
            sidebar.hear(Host { unread: 1, ..host });
        };
        let away = (away, "lunetad: gone", "lunetad does not answer");
        for (untold, note, why) in [away, (unreadable, unread, unplaced.as_str())] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            untold(&mut sidebar);
            sidebar.step(2);
            type_in(&mut sidebar, "d");
            let closes = || closes_here(1, listed("newer", 20), PaneKey::terminal(1));
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), Asked::Nothing);
            let lines = screen(&mut sidebar, true, 160, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let asks = format!("Close the view? luneta cannot tell which agents it ends: {why}");
            assert!(lines.contains(&note), "{lines:#?}");
            assert!(lines.contains(&asks.as_str()), "{lines:#?}");
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
            let act = Act::Pane {
                session: session_name("newer"),
                whose: listed("newer", 20).whose(),
                pane: PaneKey::terminal(1),
            };
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), delete_here(act));
        }
    }

    #[test]
    fn a_delete_asks_first_where_an_agent_s_session_file_does_not_read() {
        for (pane, asks) in [(1, true), (7, false)] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            let host = sidebar.host.clone().expect("the sidebar has heard the host");
            let session = Some("newer".to_owned());
            let zellij = Zellij { named: "newer".to_owned(), session, pane };
            let file = UnreadFile { pid: 200, zellij: Some(zellij), own: None, terminal: None };
            let unread_files = vec![file];
            sidebar.hear(Host { unread: 1, unread_files, ..host });
            sidebar.step(2);
            type_in(&mut sidebar, "d");
            let closes = closes_here(1, listed("newer", 20), PaneKey::terminal(1));
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
            let act = Act::Pane {
                session: session_name("newer"),
                whose: listed("newer", 20).whose(),
                pane: PaneKey::terminal(1),
            };
            let went = sidebar.closes(1, Ok(Closes::Pane));
            let lines = screen(&mut sidebar, true, 120, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let note = "lunetad cannot read 1 of Claude Code's session files";
            assert!(lines.contains(&note), "{lines:#?}");
            let asked = lines.contains(&"Close the view? 1 agent, 1 unknown");
            assert_eq!((went == delete_here(act), asked), (!asks, asks), "pane {pane}");
        }
    }

    #[test]
    fn a_close_of_a_worktree_asks_first_where_a_file_in_it_does_not_read() {
        let made_at = Some(Timestamp::default());
        let own = Some(api::OwnSession { id: 1, made_at });
        let own = Some(api::Own { session: own, pane: 1 });
        let in_own = UnreadFile { pid: 200, zellij: None, own, terminal: None };
        let zellij = Zellij { named: "twin".to_owned(), session: Some("twin".to_owned()), pane: 1 };
        let in_zellij = UnreadFile { pid: 200, zellij: Some(zellij), own: None, terminal: None };
        let made = OwnMade::from_nanos(0);
        let whose = Whose::Own(OwnKey { id: OwnId::new(1).unwrap(), made });
        let worktree = tab_of(1, 0);
        let (twin, panes) = (session_name("twin"), vec![1]);
        let seen = vec![ViewId::of(1).unwrap()];
        let close = Act::Worktree { session: twin, whose, worktree, panes, seen };
        for (file, asks) in [(in_own, true), (in_zellij, false)] {
            let mut sidebar = fresh();
            let panes = || vec![pane(1, 0, "/w/luneta/main", "fish")];
            let sessions = vec![session("twin", 10, panes()), own_session("twin", 1, panes())];
            let unread_files = vec![file];
            sidebar.hear(Host { unread: 1, unread_files, ..host(sessions, Vec::new()) });
            let place = WorktreePlace { project: project_row("twin", 1), worktree };
            sidebar.focus(Keys::Sidebar);
            sidebar.cursor = Some(Spot::Local(Place::Worktree(place)));
            type_in(&mut sidebar, "d");
            let went = sidebar.key(&char('d'), moment(160_000));
            let lines = screen(&mut sidebar, true, 60, 16);
            let question = " Close the worktree? 1 agent, 1 unknown";
            let asked = lines.iter().any(|line| line.trim_end() == question);
            assert_eq!((went == delete_here(close.clone()), asked), (!asks, asks), "{lines:#?}");
        }
    }

    #[test]
    fn a_delete_goes_by_what_zellij_closes_and_the_highlight_past_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.step(1);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let closes = closes_here(1, listed("older", 10), PaneKey::terminal(2));
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        let act = Act::Pane {
            session: session_name("older"),
            whose: listed("older", 10).whose(),
            pane: PaneKey::terminal(2),
        };
        assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), delete_here(act));
        assert!(sidebar.menu.is_none());
        sidebar.hear(host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        ));
        assert_eq!(cursor(&sidebar), Some(pane_of("newer", 20, 1)));
    }

    #[test]
    fn a_delete_that_ends_an_agent_at_work_asks_and_looks_again() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_pane_of("older", 1, 1));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "d");
        let closes = || closes_here(1, own_listed("older", 1), PaneKey::terminal(1));
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
        let tab = || Closes::Tab { id: TabId(0), panes: vec![1] };
        assert_eq!(sidebar.closes(1, Ok(tab())), Asked::Nothing);
        assert_eq!(bottom(&mut sidebar), " <y> Yes <BS> Back <ESC> Close");
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13], " Close its tab? 1 agent, 1");
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
        let act = Act::Tab {
            session: session_name("older"),
            whose: own_listed("older", 1).whose(),
            tab: TabId(0),
            panes: vec![1],
        };
        assert_eq!(sidebar.closes(1, Ok(tab())), delete_here(act));
        sidebar.cursor = Some(Spot::Local(Place::Session(own_listed("older", 1))));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13..15], [" Delete project older? 1", " agent, 1 waiting"]);
        let deleted = project_deleted("older", 1, &[1000, 1001], &[1, 2]);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), delete_here(deleted));
        assert!(sidebar.menu.is_none());
    }

    #[test]
    fn a_planned_close_of_a_project_pane_keeps_the_terminal_it_planned_when_the_mux_remakes_it() {
        let mut sidebar = own_two_sessions();
        sit(&mut sidebar, &own_pane_of("young", 2, 1));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "d");
        let planned = PaneKey::terminal(1);
        let closes = closes_here(1, own_listed("young", 2), planned);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        let mut remade = own_session("young", 2, vec![pane(1, 0, "/w/b", "fish")]);
        let panes = remade.panes.as_mut().map_or(&mut [][..], |panes| &mut panes.panes);
        let made_at = Some(Timestamp { seconds: 1, nanos: 0 });
        panes[0].key = Some(api::TerminalKey { id: 1, made_at });
        sidebar.hear(host(vec![remade], Vec::new()));
        let whose = own_listed("young", 2).whose();
        let act = Act::Pane { session: session_name("young"), whose, pane: planned };
        assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), delete_here(act));
    }

    #[test]
    fn a_project_pane_is_placed_by_its_whole_key() {
        let placed = |session, pane: &Pane| PanePlace::of(session, pane).map(|it| it.pane);
        let mut shown = pane(3, 0, "/w", "fish");
        assert_eq!(placed(own_listed("work", 1), &shown), None, "a project's pane with no key");
        let made_at = Some(Timestamp::default());
        shown.key = Some(api::TerminalKey { id: 3, made_at });
        assert_eq!(placed(own_listed("work", 1), &shown), Some(PaneKey::terminal(3)));
        shown.key = Some(api::TerminalKey { id: 4, made_at });
        assert_eq!(placed(own_listed("work", 1), &shown), None, "the key of another pane");
    }

    #[test]
    fn the_highlight_seeks_the_best_match_only_as_the_search_changes() {
        let mut sidebar = searched();
        let pane = |pane| Some(pane_of("bipa", 10, pane));
        type_in(&mut sidebar, "/pi");
        assert_eq!(cursor(&sidebar), pane(3));
        sidebar.key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), moment(160_000));
        sidebar.hear(sidebar.host.clone().expect("heard"));
        assert_eq!(cursor(&sidebar), pane(1));
        type_in(&mut sidebar, "x");
        assert_eq!(cursor(&sidebar), pane(3));
    }

    fn drawn_rows(sidebar: &mut Sidebar) -> (Vec<String>, Vec<usize>) {
        let (lines, lit) = seen(sidebar, true, 32, 28);
        let from = sessions_from(&lines);
        let rows = lines[from..].iter().take_while(|line| !line.is_empty());
        let rows = rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect();
        (rows, lit.iter().map(|y| usize::from(*y) - from).collect())
    }

    #[test]
    fn v_selects_worktrees_that_shifted_j_and_k_move() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "vjjk");
        let (_, lit) = drawn_rows(&mut sidebar);
        assert_eq!(lit, [1, 2, 3, 4]);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), Rect::new(0, 0, 32, 26));
        let shifted = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
        let seen = vec![tab_of(10, 0), tab_of(10, 1), tab_of(10, 2)];
        let wanted = vec![tab_of(10, 2), tab_of(10, 0), tab_of(10, 1)];
        let order = Reorder::Worktrees { project: ProjectId::of(10).unwrap(), seen, wanted };
        let asked = Asked::Move { on: Machine::Local, order, ticket: 1 };
        assert_eq!(sidebar.key(&shifted('J'), moment(160_000)), asked);
        assert_eq!(drawn_rows(&mut sidebar).1, [3, 4, 5, 6], "the rows move as asked");

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert_eq!(drawn_rows(&mut sidebar).1, [5, 6]);
        assert_eq!(sidebar.key(&shifted('K'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Leave);
    }

    #[test]
    fn a_folded_project_is_selected_from_its_row() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "Ghhv");
        let asked = sidebar.key(&char('K'), moment(160_000));
        let order = projects(&[10, 20, 30], &[10, 30, 20]);
        assert_eq!(asked, Asked::Move { on: Machine::Local, order, ticket: 1 });
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let zed = Place::Session(listed("zed", 30));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(zed));
        assert!(matches!(sidebar.moving, Moving::Still));
    }

    #[test]
    fn a_project_dragged_lands_where_it_shows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 28);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        let row = |name: &str| {
            let at = lines.iter().position(|line| line.starts_with(name)).unwrap();
            u16::try_from(at).unwrap()
        };
        let (bipa, notes) = (row(" ▾ bipa"), row(" ▾ notes"));
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, notes), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 20), area), Asked::Take);
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[14], " ▾ notes", "{rows:#?}");
        assert_eq!(lit, [14, 15, 16]);
        let order = projects(&[10, 20, 30], &[10, 30, 20]);
        let asked = Asked::Move { on: Machine::Local, order, ticket: 1 };
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 20), area), asked);

        let lines = screen(&mut sidebar, true, area.width, area.height);
        let notes = lines.iter().position(|line| line.starts_with(" ▾ notes")).unwrap();
        let notes = u16::try_from(notes).unwrap();
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, notes), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, bipa), area);
        let order = projects(&[10, 30, 20], &[20, 10, 30]);
        let asked = Asked::Move { on: Machine::Local, order, ticket: 1 };
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, bipa), area), asked);
    }

    #[test]
    fn a_project_lifted_goes_back_off_the_sidebar_or_on_esc() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        let lift_notes = |sidebar: &mut Sidebar| {
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 11), area);
            sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 5), area);
            assert_ne!(drawn_rows(sidebar).0, listed);
        };
        lift_notes(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 40, 5), area);
        assert_eq!(drawn_rows(&mut sidebar), (listed.clone(), (7..10).collect()));
        sidebar.mouse(mouse(MouseEventKind::Up(left), 40, 5), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_notes(&mut sidebar);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert!(!sidebar.pressed());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_notes(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 5), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 5), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    #[test]
    fn a_row_reordered_by_v_or_dragged_holds_the_keys_until_it_lands() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        assert!(!sidebar.holds_input());
        sidebar.cursor = Some(Spot::Local(Place::Session(listed("bipa", 10))));
        type_in(&mut sidebar, "v");
        assert!(sidebar.holds_input(), "v reorders");
        sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000));
        assert!(!sidebar.holds_input(), "Esc ends the reorder");

        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 10), area);
        assert!(sidebar.holds_input(), "the press");
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area);
        assert_ne!(drawn_rows(&mut sidebar).0, listed, "a row is lifted");
        assert!(sidebar.holds_input(), "the drag");
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        assert!(!sidebar.holds_input(), "the row landed");
    }

    #[test]
    fn the_left_button_down_has_the_keys() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        let area = Rect::new(0, 0, 32, 26);
        drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 11), area);
        type_in(&mut sidebar, "vn");
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area);
        let asked = sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        let order = projects(&[10, 20, 30], &[20, 10, 30]);
        assert_eq!(asked, Asked::Move { on: Machine::Local, order, ticket: 1 });
        assert!(sidebar.menu.is_none());

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 6), area);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 6), area), Asked::Nothing);
    }

    #[test]
    fn a_search_typed_lifts_no_session() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "/z");
        let area = Rect::new(0, 0, 32, 22);
        let (listed, _) = drawn_rows(&mut sidebar);
        assert_eq!(listed[0], " ▾ zed");
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 3), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 6), area), Asked::Nothing);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 6), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    fn framed(sidebar: &mut Sidebar, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        sidebar.draw(&mut buffer, area, moment(160_000));
        buffer
    }

    fn reversed(buffer: &Buffer) -> Vec<(u16, u16)> {
        let on = |at: &Position| buffer[*at].modifier.contains(Modifier::REVERSED);
        buffer.area.positions().filter(on).map(|at| (at.x, at.y)).collect()
    }

    fn dragged(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) {
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), from.0, from.1), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), to.0, to.1), area);
    }

    fn swept(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) -> Asked {
        dragged(sidebar, area, from, to);
        sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1), area)
    }

    fn copied(text: &str) -> Asked {
        Asked::CopyText(text.to_owned())
    }

    #[test]
    fn a_drag_copies_what_it_went_over_as_it_lets_go() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let here = sidebar.cursor.clone();
        assert_eq!(swept(&mut sidebar, area, (7, 6), (13, 6)), copied("a-p-i-x"));
        assert_eq!(swept(&mut sidebar, area, (13, 6), (9, 6)), copied("p-i-x"));
        dragged(&mut sidebar, area, (7, 6), (9, 6));
        let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 13, 6), area);
        assert_eq!(up, copied("a-p-i-x"));
        assert_eq!(swept(&mut sidebar, area, (2, 0), (8, 0)), copied("luneta"));
        assert_eq!(sidebar.cursor, here);

        dragged(&mut sidebar, area, (9, 6), (11, 6));
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 40, 8), area);
        assert!(sidebar.pressed());
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 40, 8), area);
        assert_eq!(up, copied("p-i-x\n  ▾ Tab #2\n       dda-bank-slip"));
        assert!(!sidebar.pressed());

        assert_eq!(swept(&mut sidebar, area, (5, 2), (25, 2)), Asked::Nothing);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    #[test]
    fn a_click_is_a_click_though_the_hand_moves_a_cell() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let api = || go_here(pane_of("bipa", 10, 1));
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
        sidebar.mouse(mouse(MouseEventKind::Down(left), 3, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 6, 4), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 7, 5), area), Asked::Nothing);
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 7, 5), area);
        assert_eq!(up, copied(&format!("bipa {}\n  ▾ Tab", "─".repeat(22))));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        assert_eq!(swept(&mut sidebar, area, (3, 4), (6, 4)), copied("bipa"));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 11), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 11, 11), area), Asked::Nothing);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 11, 12), area), Asked::Take);
    }

    #[test]
    fn a_sweep_shows_reversed_until_the_next_click() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
        dragged(&mut sidebar, area, (7, 6), (9, 6));
        let cells = [(7, 6), (8, 6), (9, 6)];
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 9, 6), area);
        assert_eq!(up, copied("a-p"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.mouse(mouse(MouseEventKind::Moved, 20, 13), area);
        sidebar.hear(searched().host.unwrap());
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.hear(first_pane_is(1, "a-p-i-y"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        sidebar.unsweep();
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    fn first_pane_is(id: u32, command: &str) -> Host {
        let mut listed = searched().host.unwrap();
        let first = &mut listed.sessions[0].panes.as_mut().unwrap().panes[0];
        first.id = id;
        first.command = Some(command.to_owned());
        listed
    }

    #[test]
    fn a_sweep_goes_once_the_text_moves_from_under_it() {
        let area = Rect::new(0, 0, 32, 26);
        let more_rows = || {
            let panes = vec![pane(1, 0, "/w/a", "a-p-i-x"), pane(2, 1, "/w/b", "b")];
            host(vec![session("bipa", 10, panes)], Vec::new())
        };
        let branch_too = || {
            let mut listed = searched().host.unwrap();
            let (statuses, pulls) = (&mut listed.git_statuses, &mut listed.pull_requests);
            checked_out(statuses, pulls, "/w/a", "main", None);
            listed
        };
        let moves: [&dyn Fn(&mut Sidebar); 8] = [
            &|sidebar| {
                sidebar.mouse(mouse(MouseEventKind::ScrollDown, 7, 5), area);
            },
            &|sidebar| {
                sidebar.hear(more_rows());
            },
            &|sidebar| {
                sidebar.hear(first_pane_is(9, "other"));
            },
            &|sidebar| {
                sidebar.notify("other word".to_owned());
            },
            &|sidebar| {
                sidebar.hear(branch_too());
            },
            &|sidebar| {
                sidebar.focus(Keys::Sidebar);
                sidebar.key(&char('?'), moment(160_000));
            },
            &|sidebar| {
                framed(sidebar, Rect::new(0, 0, 28, 26));
            },
            &|sidebar| {
                sidebar.draw_collapsed(&mut Buffer::empty(area), area, moment(160_000));
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
    fn a_sweep_let_go_otherwise_copies_nothing() {
        let area = Rect::new(0, 0, 32, 26);
        let esc = KeyEvent::from(KeyCode::Esc);
        let lets_go: [&dyn Fn(&mut Sidebar); 2] = [
            &|sidebar| {
                sidebar.key(&esc, moment(160_000));
            },
            &|sidebar| {
                sidebar.mouse(mouse(MouseEventKind::Moved, 9, 6), area);
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
            let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 9, 6), area);
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
        sidebar.lost(why.to_owned());
        sidebar.notify(notice.to_owned());
        framed(&mut sidebar, area);
        let said = format!(" lunetad: {why}\n {notice}");
        assert_eq!(swept(&mut sidebar, area, (0, 22), (30, 25)), copied(&said));
    }

    #[test]
    fn a_sweep_has_a_text_cut_short_whole() {
        let long = "a-text-too-long-for-the-sidebar";
        let panes = vec![pane(1, 1, "/w/a", long)];
        let mut local = host(vec![session(long, 10, panes)], Vec::new());
        let (statuses, pulls) = (&mut local.git_statuses, &mut local.pull_requests);
        checked_out(statuses, pulls, "/w/a", long, Some((246, State::Open, Some(LINK))));
        local.remotes = vec![remote("me@a-host-too-long-for-the-sidebar", down(100))];
        let mut sidebar = fresh();
        sidebar.this = long.to_owned();
        sidebar.hear(local);
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        for (line, from, whole) in [
            (3, 5, long.to_owned()),
            (4, 3, long.to_owned()),
            (6, 7, format!("{long} #246")),
            (7, 7, long.to_owned()),
            (8, 5, "me@a-host-too-long-for-the-sidebar · unreachable 1m".to_owned()),
        ] {
            assert_eq!(swept(&mut sidebar, area, (from, line), (22, line)), copied(&whole));
        }
        assert_eq!(swept(&mut sidebar, area, (7, 7), (11, 7)), copied("a-tex"));

        sidebar.mouse(mouse(MouseEventKind::ScrollDown, 7, 5), area);
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 4), (22, 4)), copied(long));
    }

    #[test]
    fn a_sweep_finds_the_cut_past_a_halfwidth_sound_mark() {
        let long = "\u{ff8a}\u{ff9e}-a-text-too-long-for-the-sidebar";
        let mut sidebar = fresh();
        sidebar.hear(host(vec![session("s", 10, vec![pane(1, 1, "/w/a", long)])], Vec::new()));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 6), (22, 6)), copied(long));
    }

    #[test]
    fn a_sweep_has_no_control_character_of_what_a_cut_took() {
        let cwd = "/w/a-directory-too-long-for-the-sidebar\necho hidden\x1b[201~\tend";
        let mut sidebar = fresh();
        sidebar.hear(host(vec![session("s", 10, vec![pane(1, 1, cwd, "fish")])], Vec::new()));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        let whole = "w/a-directory-too-long-for-the-sidebarecho hidden[201~end";
        assert_eq!(swept(&mut sidebar, area, (7, 6), (22, 6)), copied(whole));
    }

    #[test]
    fn a_project_held_on_the_edge_scrolls_the_rows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 7);
        let first = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 7)[HEADER].clone();
        assert_eq!(first(&mut sidebar), format!(" ▾ \u{f108} mac {}", "═".repeat(21)));
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 6), area);
        let start = steady(moment(160_000));
        let tick = |sidebar: &mut Sidebar, millis: u64| {
            let at = moment(160_000 + millis);
            sidebar.ticked(at, start + Duration::from_millis(millis), true);
            first(sidebar)
        };
        assert!(sidebar.scrolling());
        let mut firsts = vec![tick(&mut sidebar, 0), tick(&mut sidebar, 30)];
        firsts.push(tick(&mut sidebar, 60));
        for millis in (120..=1200).step_by(60) {
            firsts.push(tick(&mut sidebar, millis));
        }
        let end = sidebar.scrolling();
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
        let bipa = format!(" ▾ bipa {}", "─".repeat(22));
        let notes = format!(" ▾ notes {}", "─".repeat(21));
        let lifted = [&notes, &notes, "  ▾ Tab #1", "     \u{e795} me/Documents", &bipa];
        assert_eq!(firsts[..5], lifted, "one line a SCROLL, bipa carried along");
        assert!(!end, "it stops at the last row");
        assert_eq!(tick(&mut sidebar, 1260), "       a-p-i-x");
    }

    fn remote(destination: &str, reached: Reached) -> api::Remote {
        api::Remote { destination: destination.to_owned(), reached: Some(reached) }
    }

    fn up(sessions: Vec<Session>, agents: Vec<Agent>) -> Reached {
        Reached::Host(remote::Host {
            sessions,
            git_statuses: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_sockets: Some(0),
            unread_files: Vec::new(),
            sayings: Vec::new(),
            projects: None,
        })
    }

    fn down(since: i64) -> Reached {
        Reached::Unreached(remote::Unreached {
            since: Some(Timestamp { seconds: since, nanos: 0 }),
            why: "ssh: Could not resolve hostname vps".to_owned(),
            stopped: Some(remote::unreached::Stopped::Ended(remote::unreached::Ended {
                code: Some(255),
            })),
            projects: None,
        })
    }

    fn nixos() -> Destination {
        "nixos".parse().unwrap()
    }

    fn machines() -> Host {
        let nixos = up(
            vec![session("main", 20, vec![pane(1, 0, "/w/b", "claude")])],
            vec![agent("main", 1, Status::Waiting, Some("fix"))],
        );
        Host {
            remotes: vec![
                remote("nixos", nixos),
                remote("me@vps", down(100)),
                remote("pi", Reached::Reaching(())),
            ],
            ..host(vec![session("main", 10, vec![pane(1, 0, "/w/a", "fish")])], Vec::new())
        }
    }

    #[test]
    fn each_machine_is_a_row_with_its_projects_under_it_this_one_s_first() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        assert_eq!(
            screen(&mut sidebar, false, 32, 14),
            [
                "─ luneta ──────────────────────",
                "─ 2 projects ───────────── ●1 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ main ──────────────────────",
                "  ▾ Tab #1",
                "     \u{e795} w/a",
                " ▾ \u{f108} nixos ════════════════ ●1",
                " ▾ main ─────────────────── ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fix                  1m",
                " ▸ \u{f108} me@vps · unreachable   1m",
                " ▸ \u{f108} pi · reaching…",
                "",
            ]
        );
        let buffer = drawn(&mut sidebar, false, 32, 14);
        let dim = |y: u16| buffer[(3, y)].modifier.contains(Modifier::DIM);
        assert_eq!((dim(7), dim(11)), (false, true));
    }

    #[test]
    fn a_machine_folds_its_projects_away_and_unfolds_them() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let nixos_machine = Machine::Remote(nixos());
        let header = Spot::Machine(nixos_machine.clone());
        let project = Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        sidebar.cursor = Some(Spot::Remote(nixos(), pane_of("main", 20, 1)));
        type_in(&mut sidebar, "hh");
        assert_eq!(sidebar.cursor, Some(project.clone()));
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert_eq!(sidebar.cursor, Some(header.clone()));
        let folded = [
            " ▾ \u{f108} mac ═════════════════════",
            " ▾ main ──────────────────────",
            "  ▾ Tab #1",
            "     \u{e795} w/a",
            " ▸ \u{f108} nixos ════════════════ ●1",
            " ▸ \u{f108} me@vps · unreachable   1m",
            " ▸ \u{f108} pi · reaching…",
        ];
        assert_eq!(screen(&mut sidebar, false, 32, 14)[3..10], folded);
        assert!(sidebar.row_of(&project).is_none(), "its projects go with it");
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));

        type_in(&mut sidebar, "l");
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert!(sidebar.row_of(&project).is_some());
        sidebar.cursor = Some(header.clone());
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
        assert!(sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        let area = Rect::new(0, 0, 32, 14);
        assert_eq!(click(&mut sidebar, 20, 7, area), Asked::Take);
        assert!(!sidebar.folds.contains(Fold::Machine(&nixos_machine)));
        assert_eq!(click(&mut sidebar, 20, 3, area), Asked::Take);
        assert!(sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(lines[3], " ▸ \u{f108} mac ═════════════════════");
        assert_eq!(lines[4], " ▾ \u{f108} nixos ════════════════ ●1");

        let vps = Machine::Remote("me@vps".parse().unwrap());
        sidebar.cursor = Some(Spot::Machine(vps.clone()));
        sidebar.key(&enter, moment(160_000));
        assert!(!sidebar.folds.contains(Fold::Machine(&vps)));
        assert_eq!(sidebar.cursor, Some(Spot::Machine(vps)), "it has no projects to fold");
    }

    #[test]
    fn a_machine_s_fold_goes_once_it_lists_no_projects() {
        let mut sidebar = fresh();
        let folded = |sidebar: &Sidebar| {
            [Machine::Local, Machine::Remote(nixos())]
                .map(|machine| sidebar.folds.contains(Fold::Machine(&machine)))
        };
        sidebar.hear(machines());
        sidebar.folds.set(Fold::Machine(&Machine::Local), true);
        sidebar.folds.set(Fold::Machine(&Machine::Remote(nixos())), true);
        sidebar.hear(machines());
        assert_eq!(folded(&sidebar), [true, true]);
        let none = up(Vec::new(), Vec::new());
        sidebar.hear(Host { remotes: vec![remote("nixos", none)], ..host(Vec::new(), Vec::new()) });
        assert_eq!(folded(&sidebar), [false, false]);
        sidebar.hear(machines());
        let lines = screen(&mut sidebar, false, 32, 14);
        assert_eq!(lines[3], " ▾ \u{f108} mac ═════════════════════");
        assert_eq!(lines[7], " ▾ \u{f108} nixos ════════════════ ●1");
    }

    #[test]
    fn a_machine_s_fold_no_luneta_saw_go_goes_on_the_first_listing() {
        let mut sidebar = fresh();
        sidebar.folds.set(Fold::Machine(&Machine::Local), true);
        sidebar.hear(host(Vec::new(), Vec::new()));
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
        sidebar.hear(host(vec![session("x", 10, Vec::new())], Vec::new()));
        let lines = screen(&mut sidebar, false, 32, 6);
        assert_eq!(
            lines[3..5],
            [" ▾ \u{f108} mac ═════════════════════", " ▾ x ─────────────────────────"]
        );
    }

    #[test]
    fn h_folds_a_pane_s_worktree_then_its_project_and_l_unfolds() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let machine = Machine::Remote(nixos());
        let (main, tab) = (ProjectId::of(20).unwrap(), tab_of(20, 0));
        let fix = pane_of("main", 20, 1);
        sidebar.cursor = Some(Spot::Remote(nixos(), fix));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Worktree(&machine, tab)));
        assert!(!sidebar.folds.contains(Fold::Project(&machine, main)));
        let place = WorktreePlace { project: project_row("main", 20), worktree: tab };
        assert_eq!(sidebar.cursor, Some(Spot::Remote(nixos(), Place::Worktree(place))));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Project(&machine, main)));
        let project = Spot::Remote(nixos(), Place::Session(listed("main", 20)));
        assert_eq!(sidebar.cursor, Some(project));
        assert!(!folded(&sidebar, "main"), "this machine's main folds apart");
        type_in(&mut sidebar, "l");
        assert!(!sidebar.folds.contains(Fold::Project(&machine, main)));
        assert!(sidebar.folds.contains(Fold::Worktree(&machine, tab)));
    }

    #[test]
    fn d_m_on_a_far_row_goes_by_that_machine_and_a_note_says_what_stays() {
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let name = Said::new("fix".to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let repository = Said::new("/w".to_owned());
        let stashes = 0;
        let status = GitStatus::Checkout { repository, head, changes: Changes::default(), stashes };
        there.git_statuses.push(status.to_api("/w/b"));
        let mut sidebar = fresh();
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        let place = PanePlace { session: listed("main", 20), pane: PaneKey::terminal(1) };
        let place = Place::Pane(place);
        sidebar.cursor = Some(Spot::Remote(nixos(), place));
        let now = moment(160_000);
        let on = Machine::Remote(nixos());
        sidebar.key(&char('d'), now);
        let number = sidebar.menu.as_ref().expect("the menu").number();
        let dir = Directory::parse("/w/b".to_owned()).unwrap();
        let asks = Asked::Prunes { menu: number, on: on.clone(), dir: dir.clone() };
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
        let deletes = Asked::Prune { menu: number, on, dir, agreed: plan.deletes() };
        assert_eq!(sidebar.key(&char('y'), now), deletes);
        assert!(sidebar.menu.as_ref().expect("the menu").pruning());
        let changed = Ok(prunes::Pruned::Changed(plan.clone()));
        assert_eq!(sidebar.pruned(number, changed.clone()), Asked::Nothing);
        assert!(!sidebar.menu.as_ref().expect("the menu").pruning());
        assert_eq!(sidebar.key(&char('y'), now), deletes);
        sidebar.key(&KeyEvent::from(KeyCode::Esc), now);
        assert_eq!(sidebar.pruned(number, changed), Asked::Nothing);
        let note = "menu: nothing deleted: what would go changed";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
        sidebar.key(&char('d'), now);
        let number = sidebar.menu.as_ref().expect("the menu").number();
        sidebar.key(&char('m'), now);
        sidebar.prunes(number, Ok(plan.clone()));
        sidebar.key(&char('y'), now);
        let came = prunes::Came::Stays("it has a pane".to_owned());
        let stays = prunes::Outcome { folder: goes.folder, came };
        assert_eq!(sidebar.pruned(number, Ok(prunes::Pruned::Went(vec![stays]))), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let note = "menu: deleted 0 of 1 worktrees; one stays: it has a pane";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
        sidebar.key(&char('d'), now);
        let number = sidebar.menu.as_ref().expect("the menu").number();
        sidebar.key(&char('m'), now);
        sidebar.focus(Keys::Zellij);
        assert_eq!(sidebar.prunes(number, Ok(plan)), Asked::Nothing);
        assert!(sidebar.working.is_empty());
        let note = "menu: nothing deleted: the menu closed before you said yes; d m asks again";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
    }

    #[test]
    fn another_machine_s_rows_go_there_and_open_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        host.remotes[0] = remote(
            "nixos",
            up(
                vec![session("main", 20, vec![pane(1, 0, "/w/b", "claude")])],
                vec![agent("main", 1, Status::Idle, Some("fix"))],
            ),
        );
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "Gkk");
        let fix = pane_of("main", 20, 1);
        let there = Asked::Go { on: Machine::Remote(nixos()), place: fix };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), there);
        type_in(&mut sidebar, "mn");
        let on = Machine::Remote(nixos());
        let (session, cwd) = (listed("main", 20), Some("/w/b".to_owned()));
        let (branch, repository, ran) = (None, None, Ran::Unhooked);
        let pane = PaneKey::terminal(1);
        let terminal = ViewId::of(1);
        let row =
            PaneRow { session, pane, terminal, cwd, branch, repository, pull_request: None, ran };
        let menu = sidebar.menu.as_ref().expect("the menu");
        assert_eq!(menu.target(), &Target::Pane(on.clone(), row.clone()));
        let (session, cwd) = (session_name("main"), directory("/w/b"));
        let open = Open::Pane { session, beside: 1, cwd, command: Vec::new() };
        let key = listed("main", 20).own_key().unwrap();
        let asked = Asked::Open { asking: Held::Open, on: on.clone(), key, open };
        assert_eq!(sidebar.key(&char('v'), moment(160_000)), asked);
        assert!(sidebar.menu.is_none());

        let area = Rect::new(0, 0, 32, 26);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[10], "  •✓ \u{ec82} fix                  1m");
        assert_eq!(click(&mut sidebar, 10, 10, area), there);

        type_in(&mut sidebar, "u");
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[10], "   ✓ \u{ec82} fix                  1m");

        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let pasted = sidebar.menu.as_ref().map(Menu::target);
        assert_eq!(pasted, Some(&Target::Pane(on, PaneRow { ran: Ran::Unhooked, ..row })));
    }

    #[test]
    fn a_link_pasted_on_another_machine_s_row_makes_its_session_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = || session("work", 20, vec![pane(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work()], Vec::new()));
        sidebar.hear(host.clone());
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("work", 20))));
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let on = Machine::Remote(nixos());
        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let worked = links::Worked { dir: "/w/b".to_owned(), pull: None, here: false };
        let pane = PaneKey::terminal(3);
        let worked = vec![Worked { session: listed("work", 20), pane, works: worked }];
        let local = links::Worked { dir: "/w/a".to_owned(), pull: None, here: false };
        let pane = PaneKey::terminal(1);
        let local = vec![Worked { session: listed("main", 10), pane, works: local }];
        let unreached = |name: &str| {
            let machine = Machine::Remote(name.parse().unwrap());
            (machine, Err(format!("lunetad does not reach {name} now")))
        };
        let others = vec![(Machine::Local, Ok(local)), unreached("me@vps"), unreached("pi")];
        let asked = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let pull = Asked::Pull { menu: number, on: on.clone(), work: asked, wanted, worked, others };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);
        let at = At::Clone { cwd: directory("/w/n"), name: session_name("main") };
        let folder = "/w/n/feat".to_owned();
        let of_link = Readied::Link(link);
        let ready = Ready { at, folder, branch: None, moved_off: false, work: of_link };
        let asked = sidebar.pulled(number, Ok(Found { on: on.clone(), ready }));
        assert!(matches!(&asked, Asked::Make { on: there, .. } if *there == on), "{asked:?}");
        assert_eq!(sidebar.made_for(number), Some(Held::Open));
    }

    #[test]
    fn a_link_found_on_another_machine_than_the_row_s_opens_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = own_session("work", 1, vec![pane(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work], Vec::new()));
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(pane_of("main", 10, 1)));
        sidebar.key(&char('h'), moment(160_000));
        let place = WorktreePlace { project: project_row("main", 10), worktree: tab_of(10, 0) };
        assert_eq!(sidebar.cursor, Some(Spot::Local(Place::Worktree(place))));

        let nixos = Machine::Remote(nixos());
        let vps = Machine::Remote("me@vps".parse().unwrap());
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let asked = sidebar.key(&char('r'), moment(160_000));
        let Asked::Pull { on, wanted, others, .. } = asked else {
            panic!("no job for the key: {asked:?}");
        };
        assert_eq!(on, Machine::Local);
        assert_eq!(wanted, Wanted::AtHead);
        let works = links::Worked { dir: "/w/b".to_owned(), pull: None, here: false };
        let work = Worked { session: own_listed("work", 1), pane: PaneKey::terminal(3), works };
        let unreached = |name: &str| Err(format!("lunetad does not reach {name} now"));
        let pi = Machine::Remote("pi".parse().unwrap());
        assert_eq!(others, [
            (nixos.clone(), Ok(vec![work])),
            (vps, unreached("me@vps")),
            (pi, unreached("pi")),
        ]);

        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let at = At::Session((own_listed("work", 1), PaneKey::terminal(3)));
        let folder = "/w/b/feat".to_owned();
        let of_link = Readied::Link(link);
        let ready = Ready { at, folder, branch: None, moved_off: false, work: of_link };
        let asked = sidebar.pulled(number, Ok(Found { on: nixos.clone(), ready }));
        let there = |asked: &Asked| matches!(asked, Asked::Open { on, .. } if *on == nixos);
        assert!(there(&asked), "{asked:?}");
    }

    #[test]
    fn a_link_pasted_on_a_machine_lunetad_does_not_reach_finds_nothing() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Machine(Machine::Remote("me@vps".parse().unwrap())));
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        assert!(sidebar.menu.is_some());
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let why = "menu: lorenzolfm/luneta#5: lunetad does not reach me@vps now";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    #[test]
    fn another_machine_s_worktree_closes_there_by_its_agents_and_its_pane_stays() {
        let mut sidebar = fresh();
        let mut host = machines();
        let panes = vec![pane(1, 0, "/w/b", "claude"), pane(2, 0, "/w/b", "fish")];
        let agents = vec![agent("main", 1, Status::Waiting, Some("fix"))];
        host.remotes[0] = remote("nixos", up(vec![session("main", 20, panes)], agents));
        sidebar.hear(host);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Remote(nixos(), pane_of("main", 20, 1)));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let place = WorktreePlace { project: project_row("main", 20), worktree: tab_of(20, 0) };
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Worktree(place.clone())));
        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 120, 16);
        let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
        assert!(lines.contains(&"Close the worktree? 1 agent, 1 waiting"), "{lines:#?}");
        let act = Act::Worktree {
            session: session_name("main"),
            whose: listed("main", 20).whose(),
            worktree: place.worktree,
            panes: vec![1, 2],
            seen: vec![ViewId::of(1).unwrap(), ViewId::of(2).unwrap()],
        };
        let on = Machine::Remote(nixos());
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), Asked::Delete { on, act });
        assert!(sidebar.menu.is_none());
    }

    #[test]
    fn a_delete_on_a_machine_lunetad_no_longer_reaches_deletes_nothing() {
        for answers in [true, false] {
            let mut sidebar = fresh();
            let mut host = machines();
            sidebar.hear(host.clone());
            sidebar.focus(Keys::Sidebar);
            let (project, worktree) = (project_row("main", 20), tab_of(20, 0));
            let place = WorktreePlace { project, worktree };
            sidebar.cursor = Some(Spot::Remote(nixos(), Place::Worktree(place)));
            type_in(&mut sidebar, "d");
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
            host.remotes[0] = remote("nixos", down(100));
            sidebar.hear(host);
            if !answers {
                sidebar.lost("lunetad stopped".to_owned());
            }
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), Asked::Nothing);
            let refused = Some("menu: lunetad does not reach nixos now");
            assert_eq!(sidebar.notice.as_deref(), refused, "answers: {answers}");
        }
    }

    #[test]
    fn the_picker_lists_the_machines_lunetad_reaches() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let listing = sidebar.listing();
        let names = |runs: &Runs| runs.agents.iter().map(|seated| seated.label.clone()).collect();
        assert_eq!((listing.this.as_str(), names(&listing.local)), ("mac", Vec::<String>::new()));
        let remotes: Vec<_> = listing.remotes.iter().map(|(at, runs)| (at, names(runs))).collect();
        assert_eq!(remotes, [(&nixos(), vec!["fix".to_owned()])]);
        let each = listing.shelves.iter().map(|shelved| {
            let projects = shelved.projects.iter().map(|housed| housed.project.listed().name());
            (shelved.on.clone(), shelved.away, projects.map(ToString::to_string).collect())
        });
        let main = || vec!["main".to_owned()];
        let far = |at: &str| Machine::Remote(at.parse().unwrap());
        let shelves: Vec<(Machine, Option<&str>, Vec<String>)> = each.collect();
        assert_eq!(shelves, [
            (Machine::Local, None, main()),
            (Machine::Remote(nixos()), None, main()),
            (far("me@vps"), Some("unreachable"), Vec::new()),
            (far("pi"), Some("reaching…"), Vec::new()),
        ]);
        sidebar.cursor = Some(Spot::Machine(Machine::Remote(nixos())));
        assert_eq!(sidebar.on_header(), Some(Machine::Remote(nixos())));
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("main", 20))));
        assert_eq!(sidebar.on_header(), None);
    }

    #[test]
    fn the_picker_is_told_the_worktree_luneta_shows_on_the_machine_it_sits_on() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let there = listed("main", 20);
        sidebar.attaching(Machine::Remote(nixos()), there.own_key().unwrap());
        sidebar.showing(tab_of(20, 0));
        let project = ProjectRow::of(there).unwrap();
        let place = WorktreePlace { project, worktree: tab_of(20, 0) };
        assert_eq!(sidebar.listing().shown, Some((Machine::Remote(nixos()), place)));
    }

    #[test]
    fn n_w_on_a_worktree_makes_a_branch_s_in_its_project_on_its_machine() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let tab = |id| WorktreePlace { project: project_row("main", id),
            worktree: tab_of(id, 0),
        };
        sidebar.cursor = Some(Spot::Local(Place::Worktree(tab(10))));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(10).unwrap(), Branch::parse("feat").unwrap());
        let made = Asked::NewWorktree { on: Machine::Local, project, branch };
        assert_eq!(branched(&mut sidebar, "feat"), made);
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Worktree(tab(20))));
        type_in(&mut sidebar, "n");
        let (project, branch) = (ProjectId::of(20).unwrap(), Branch::parse("feat").unwrap());
        let far = Asked::NewWorktree { on: Machine::Remote(nixos()), project, branch };
        assert_eq!(branched(&mut sidebar, "feat"), far);
    }

    #[test]
    fn n_w_in_a_folder_says_its_one_worktree_is_its_own_and_asks_no_branch() {
        let mut sidebar = fresh();
        let own = worktree_of(31, "notes", "/w/notes", &[]);
        let mut notes = a_project(8, "notes", Vec::new());
        notes.dir = "/w/notes".to_owned();
        notes.holds = Some(api::project::Holds::Folder(own));
        sidebar.heard(Host { projects: Some(kept(vec![notes])), ..host(Vec::new(), Vec::new()) });
        sidebar.focus(Keys::Sidebar);
        let worktree = WorktreeId::of(31).unwrap();
        let place = WorktreePlace { project: project_row("notes", 8), worktree };
        sidebar.cursor = Some(Spot::Local(Place::Worktree(place)));
        type_in(&mut sidebar, "n");
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        assert!(sidebar.menu.is_none(), "no box asks for a branch");
        let said = sidebar.notice.as_deref();
        assert_eq!(said, Some("menu: notes is a folder, and its own is its one worktree"));
    }

    #[test]
    fn what_is_pasted_in_the_box_for_a_machine_goes_in_it() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        type_in(&mut sidebar, "nm");
        sidebar.paste("me@vps\n");
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Ssh("me@vps".parse().unwrap()));
        let link = "https://github.com/lorenzolfm/luneta/pull/5";
        type_in(&mut sidebar, "nm");
        sidebar.paste(link);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Ssh(link.parse().unwrap()));
    }

    #[test]
    fn an_unreached_machine_says_why_in_the_notes_while_highlighted() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "Gk");
        let why = " me@vps: ssh: Could not resolve hostname vps; what ssh said is in luneta.log";
        let says = |sidebar: &mut Sidebar| screen(sidebar, true, 96, 24).contains(&why.to_owned());
        assert!(says(&mut sidebar));
        type_in(&mut sidebar, "j");
        assert!(!says(&mut sidebar));
        type_in(&mut sidebar, "k");
        sidebar.focus(Keys::Zellij);
        assert!(!screen(&mut sidebar, false, 96, 24).contains(&why.to_owned()));
    }

    #[test]
    fn a_machine_named_twice_or_by_no_destination_is_left_out_and_said() {
        let mut sidebar = fresh();
        let mut host = machines();
        host.remotes.push(remote("nixos", down(100)));
        host.remotes.push(remote("-oProxyCommand=sh", down(100)));
        sidebar.hear(host);
        let lines = screen(&mut sidebar, false, 32, 18);
        let headers = lines.iter().filter(|line| line.starts_with(" ▾ \u{f108} nixos ")).count();
        assert_eq!(headers, 1);
        assert!(!lines.iter().any(|line| line.contains("ProxyCommand")));
        assert_eq!(
            lines[15..],
            [
                " lunetad: 2 machines with a de",
                " stination luneta does not rea",
                " d, or named twice, not listed",
            ]
        );
    }

    #[test]
    fn the_menu_adds_a_machine_from_any_row_and_forgets_one_from_its_row() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "nm-x");
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 26);
        assert!(lines.iter().any(|line| line.starts_with(" not a destination:")), "{lines:?}");
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        sidebar.key(&backspace, moment(160_000));
        sidebar.key(&backspace, moment(160_000));
        type_in(&mut sidebar, "me@box");
        let added = Asked::Ssh("me@box".parse().unwrap());
        assert_eq!(sidebar.key(&enter, moment(160_000)), added);
        assert!(sidebar.menu.is_none());

        type_in(&mut sidebar, "Gk");
        let vps = "me@vps".parse().unwrap();
        assert_eq!(sidebar.cursor, Some(Spot::Machine(Machine::Remote(vps))));
        type_in(&mut sidebar, "d");
        let forget = Asked::Forget("me@vps".parse().unwrap());
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), forget);
    }

    fn two_each() -> Host {
        let mut host = machines();
        host.sessions.push(session("work", 30, vec![pane(1, 0, "/w/c", "fish")]));
        let Some(Reached::Host(nixos)) = &mut host.remotes[0].reached else {
            panic!("nixos is reached");
        };
        nixos.sessions.push(session("dev", 40, vec![pane(1, 0, "/w/d", "fish")]));
        host
    }

    #[test]
    fn j_and_k_move_a_project_among_its_machine_s_by_lunetad() {
        let mut sidebar = fresh();
        sidebar.hear(two_each());
        sidebar.focus(Keys::Sidebar);
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        sidebar.cursor = Some(Spot::Local(Place::Session(listed("main", 10))));
        type_in(&mut sidebar, "v");
        let asked = |on: Machine, seen: &[u64], ids: &[u64], ticket| {
            Asked::Move { on, order: projects(seen, ids), ticket }
        };
        assert_eq!(shift(&mut sidebar, 'K'), Asked::Nothing, "the first stays the first");
        assert_eq!(shift(&mut sidebar, 'J'), asked(Machine::Local, &[10, 30], &[30, 10], 1));
        assert_eq!(shift(&mut sidebar, 'J'), Asked::Nothing, "the last stays the last");
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        sidebar.key(&esc, moment(160_000));

        let far = |name: &str, id| Spot::Remote(nixos(), Place::Session(listed(name, id)));
        sidebar.cursor = Some(far("dev", 40));
        type_in(&mut sidebar, "v");
        let nixos_moved = asked(Machine::Remote(nixos()), &[20, 40], &[40, 20], 2);
        assert_eq!(shift(&mut sidebar, 'K'), nixos_moved);
        assert_eq!(shift(&mut sidebar, 'K'), Asked::Nothing, "the first stays the first");
    }

    #[test]
    fn j_and_k_move_a_worktree_among_its_project_s_by_lunetad() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let first = WorktreePlace { project: project_row("older", 10),
            worktree: tab_of(10, 0),
        };
        sidebar.cursor = Some(Spot::Local(pane_of("older", 10, 1)));
        type_in(&mut sidebar, "v");
        assert_eq!(sidebar.cursor, Some(Spot::Local(Place::Worktree(first))));
        let asked = sidebar.key(&char('J'), moment(160_000));
        let (first, second) = (tab_of(10, 0), tab_of(10, 1));
        let (seen, wanted) = (vec![first, second], vec![second, first]);
        let order = Reorder::Worktrees { project: ProjectId::of(10).unwrap(), seen, wanted };
        assert_eq!(asked, Asked::Move { on: Machine::Local, order, ticket: 1 });
    }

    fn version(seq: u64) -> api::Version {
        api::Version { pid: 7, seq }
    }

    fn kept_of(host: &mut Host) -> &mut api::projects::Kept {
        let held = host.projects.as_mut().and_then(|projects| projects.held.as_mut());
        let Some(api::projects::Held::Kept(kept)) = held else {
            panic!("the projects are kept");
        };
        kept
    }

    fn in_order(host: &Host, ids: &[u64], seq: u64) -> Host {
        let mut host = host.clone();
        host.sessions.clear();
        let kept = kept_of(&mut host);
        kept.projects.retain(|it| ids.contains(&it.id));
        kept.projects.sort_by_key(|it| ids.iter().position(|id| *id == it.id));
        kept.version = Some(version(seq));
        host
    }

    fn projects_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let rows = rows_shown(sidebar).into_iter();
        let names = rows.filter_map(|row| Some(row.strip_prefix(" ▾ ")?.to_owned()));
        names.collect()
    }

    fn bipa_selected() -> (Sidebar, Host) {
        let mut sidebar = searched();
        let heard = sidebar.host.clone().expect("heard");
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(Place::Session(listed("bipa", 10))));
        type_in(&mut sidebar, "v");
        (sidebar, heard)
    }

    fn moved(seen: &[u64], wanted: &[u64], ticket: u64) -> Asked {
        Asked::Move { on: Machine::Local, order: projects(seen, wanted), ticket }
    }

    #[test]
    fn quick_presses_build_on_the_order_asked_until_watch_shows_the_version_answered() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        assert_eq!(shift(&mut sidebar, 'J'), moved(&[10, 20, 30], &[20, 10, 30], 1));
        assert_eq!(shift(&mut sidebar, 'J'), moved(&[20, 10, 30], &[20, 30, 10], 1));
        assert_eq!(shift(&mut sidebar, 'J'), Asked::Nothing, "the last stays the last");
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"]);
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"], "on its way still");

        sidebar.moved(1, Some(version(2)));
        sidebar.moved(1, Some(version(3)));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"], "answered, not shown");
        sidebar.hear(in_order(&heard, &[20, 30, 10], 3));
        sidebar.hear(in_order(&heard, &[10, 20, 30], 4));
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"], "moved back elsewhere");

        assert_eq!(shift(&mut sidebar, 'J'), moved(&[10, 20, 30], &[20, 10, 30], 2));
        sidebar.moved(2, Some(version(5)));
        sidebar.hear(in_order(&heard, &[10, 20, 30], 6));
        let back = "moved back elsewhere before Watch showed it";
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"], "{back}");
        assert_eq!(shift(&mut sidebar, 'J'), moved(&[10, 20, 30], &[20, 10, 30], 3));
        sidebar.hear(in_order(&heard, &[10, 20], 7));
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes"], "forgotten elsewhere");
    }

    #[test]
    fn an_order_asked_again_shows_until_watch_shows_the_last_version_answered() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        shift(&mut sidebar, 'J');
        shift(&mut sidebar, 'J');
        assert_eq!(shift(&mut sidebar, 'K'), moved(&[20, 30, 10], &[20, 10, 30], 1));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));
        sidebar.hear(in_order(&heard, &[20, 30, 10], 3));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "on its way still");
        assert_eq!(shift(&mut sidebar, 'K'), moved(&[20, 10, 30], &[10, 20, 30], 1));

        for seq in 2..6 {
            sidebar.moved(1, Some(version(seq)));
        }
        sidebar.hear(in_order(&heard, &[20, 10, 30], 4));
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"], "an older version");
        sidebar.hear(in_order(&heard, &[10, 20, 30], 5));
        sidebar.hear(in_order(&heard, &[20, 10, 30], 6));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "landed, lunetad says");
    }

    #[test]
    fn a_version_from_another_lunetad_or_none_shows_lunetad_s_order() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        shift(&mut sidebar);
        sidebar.moved(1, Some(version(9)));
        let mut restarted = in_order(&heard, &[10, 20, 30], 1);
        kept_of(&mut restarted).version = Some(api::Version { pid: 8, seq: 1 });
        sidebar.hear(restarted);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"], "lunetad started again");

        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 2));
        sidebar.moved(2, None);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"], "it says no version");
    }

    #[test]
    fn a_move_lunetad_turns_down_puts_the_rows_back_as_it_keeps_them() {
        let (mut sidebar, _) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 1));
        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 1));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "zed", "bipa"]);
        let stale = format!("not moved: {}", "they changed since");
        let refused = |sidebar: &mut Sidebar, ticket| {
            let order = projects(&[10, 20, 30], &[20, 10, 30]);
            sidebar.unmoved(&Machine::Local, ticket, &order, stale.clone());
        };
        refused(&mut sidebar, 1);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);
        assert_eq!(sidebar.notice, Some(stale.clone()));

        sidebar.notice = None;
        assert_eq!(shift(&mut sidebar), moved(&[10, 20, 30], &[20, 10, 30], 2));
        refused(&mut sidebar, 1);
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "an older refusal");
        assert_eq!(sidebar.notice, None, "a refusal a newer move replaced says nothing");
        let elsewhere = Machine::Remote(nixos());
        let order = projects(&[20, 40], &[40, 20]);
        sidebar.unmoved(&elsewhere, 1, &order, stale.clone());
        assert_eq!(sidebar.notice, Some(stale.clone()), "another machine's refusal says so");

        sidebar.notice = None;
        refused(&mut sidebar, 2);
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);
        assert_eq!(sidebar.notice, Some(stale));
    }

    #[test]
    fn a_move_never_sent_takes_back_that_press_alone() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar| sidebar.key(&char('J'), moment(160_000));
        shift(&mut sidebar);
        sidebar.moved(1, Some(version(2)));
        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 1));
        sidebar.unsent(1, &projects(&[20, 10, 30], &[20, 30, 10]), "full");
        assert_eq!(sidebar.notice, Some("not moved: full".to_owned()));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "the first still");
        sidebar.hear(in_order(&heard, &[10, 20, 30], 1));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "not shown yet");
        sidebar.hear(in_order(&heard, &[20, 10, 30], 2));

        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 2));
        sidebar.unsent(2, &projects(&[20, 10, 30], &[20, 30, 10]), "full");
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"]);
        assert_eq!(shift(&mut sidebar), moved(&[20, 10, 30], &[20, 30, 10], 3), "nothing waits");
    }

    #[test]
    fn moves_lunetad_has_not_answered_stop_at_the_most_the_way_holds() {
        let (mut sidebar, heard) = bipa_selected();
        let shift = |sidebar: &mut Sidebar, c: char| sidebar.key(&char(c), moment(160_000));
        for at in 0..MOVES_MAX {
            let press = if at % 2 == 0 { 'J' } else { 'K' };
            let asked = shift(&mut sidebar, press);
            assert!(matches!(asked, Asked::Move { ticket: 1, .. }), "{at}: {asked:?}");
        }
        assert_eq!(shift(&mut sidebar, 'J'), Asked::Nothing);
        let held = format!("not moved: lunetad has not answered the last {MOVES_MAX} moves");
        assert_eq!(sidebar.notice, Some(held));
        assert_eq!(projects_shown(&mut sidebar), ["bipa", "notes", "zed"]);

        sidebar.moved(1, Some(version(1)));
        assert!(matches!(shift(&mut sidebar, 'J'), Asked::Move { ticket: 1, .. }));
        let last = u64::try_from(MOVES_MAX).unwrap() + 1;
        for seq in 2..=last {
            sidebar.moved(1, Some(version(seq)));
        }
        sidebar.hear(in_order(&heard, &[20, 10, 30], last - 1));
        assert_eq!(projects_shown(&mut sidebar), ["notes", "bipa", "zed"], "not the last yet");
        sidebar.hear(in_order(&heard, &[20, 10, 30], last));
        assert_eq!(shift(&mut sidebar, 'J'), moved(&[20, 10, 30], &[20, 30, 10], 2));
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
            let at = lines.iter().position(|line| line.contains(name)).unwrap();
            u16::try_from(at).unwrap()
        };
        let (dev, first) = (row(" dev "), row("\u{f108} nixos ") + 1);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, dev), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, first), area), Asked::Take);
        let order = projects(&[20, 40], &[40, 20]);
        let asked = Asked::Move { on: Machine::Remote(nixos()), order, ticket: 1 };
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, first), area), asked);
    }

    #[test]
    fn a_search_finds_another_machine_s_rows_folded() {
        let mut sidebar = fresh();
        sidebar.hear(machines());
        let main = ProjectId::of(20).unwrap();
        sidebar.folds.set(Fold::Machine(&Machine::Remote(nixos())), true);
        sidebar.folds.set(Fold::Project(&Machine::Remote(nixos()), main), true);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "/fix");
        let lines = screen(&mut sidebar, true, 32, 26);
        assert_eq!(
            lines[HEADER..HEADER + 4],
            [
                " ▾ \u{f108} nixos ════════════════ ●1",
                " ▾ main ─────────────────── ●1",
                "  ▾ Tab #1                  ●1",
                "   ● \u{ec82} fix                  1m",
            ]
        );
        assert_eq!(lines[HEADER + 4], "");
    }

    #[test]
    fn what_you_saw_on_another_machine_outlasts_its_being_unreached() {
        let mut sidebar = fresh();
        let reached = |sessions: bool| {
            let idle = agent("main", 1, Status::Idle, Some("fix"));
            let main = session("main", 20, vec![pane(1, 0, "/w/b", "claude")]);
            let nixos =
                if sessions { up(vec![main], vec![idle]) } else { up(Vec::new(), Vec::new()) };
            Host { remotes: vec![remote("nixos", nixos)], ..host(Vec::new(), Vec::new()) }
        };
        let row = |sidebar: &mut Sidebar| screen(sidebar, false, 32, 9)[7].clone();
        let seen = "   ✓ \u{ec82} fix                  1m";
        let new = "  •✓ \u{ec82} fix                  1m";
        sidebar.hear(reached(true));
        assert_eq!(row(&mut sidebar), new);
        let empty = screen(&mut sidebar, false, 32, 8)[3].clone();
        assert_eq!(empty, " ▸ \u{f108} mac · no projects", "a machine with none is one row");
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "Gu");
        sidebar.focus(Keys::Zellij);
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
        let unreached = |host: Host| Host { remotes: vec![remote("nixos", down(150))], ..host };
        sidebar.hear(unreached(own_idle_agent()));
        let s = |pane| own_pane_of("s", 1, pane);
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
        let panes = vec![pane(1, 0, "/one", "one"), pane(2, 0, "/two", "two")];
        let mut sidebar = Sidebar::default();
        sidebar.hear(host(vec![session("work", 100, panes)], Vec::new()));
        let marked = |sidebar: &mut Sidebar| {
            let shown = screen(sidebar, false, 40, 12);
            shown.into_iter().filter(|row| row.contains(aloud::MARK)).collect::<Vec<_>>()
        };
        assert_eq!(marked(&mut sidebar), [""; 0]);

        let speaker = Zellij { named: "old".to_owned(), session: Some("work".to_owned()), pane: 2 };
        let speaker = lunetad::sayings::Speaker(speaker);
        let whose = aloud::Whose::Said { on: Machine::Local, speaker: Some(speaker) };
        let text = "the tests pass".to_owned();
        sidebar.aloud(Some(aloud::Shown::Reading { whose, text, paused: false }));
        let rows = marked(&mut sidebar);
        assert_eq!(rows.len(), 1, "a zellij speaker marks no project's row: {rows:#?}");
        assert!(rows[0].contains("work · the tests pass"), "{rows:#?}");

        sidebar.aloud(None);
        assert_eq!(marked(&mut sidebar), [""; 0]);
    }

    #[test]
    fn the_buttons_of_what_is_read_aloud_are_where_they_show() {
        let reading = |paused| {
            let (whose, text) = (aloud::Whose::Selection, "the tests pass".repeat(9));
            Some(aloud::Shown::Reading { whose, text, paused })
        };
        for width in [NARROWEST, 40] {
            let area = Rect::new(2, 1, width, 12);
            let mut sidebar = Sidebar::default();
            let at = |buffer: &Buffer, icon: &str| {
                let found = buffer.content().iter().position(|cell| cell.symbol() == icon)?;
                let (x, y) = buffer.pos_of(found);
                Some(Position::new(x, y))
            };
            for (paused, toggle, other, button) in [
                (false, aloud::PAUSE, aloud::PLAY, aloud::Button::Pause),
                (true, aloud::PLAY, aloud::PAUSE, aloud::Button::GoOn),
            ] {
                let mut buffer = Buffer::empty(Rect::new(0, 0, area.right(), area.bottom()));
                sidebar.aloud(reading(paused));
                let _ = sidebar.draw(&mut buffer, area, SystemTime::UNIX_EPOCH);
                assert_eq!(at(&buffer, other), None);
                let toggle = at(&buffer, toggle).expect("the button shows");
                let stop = at(&buffer, aloud::STOP).expect("the button shows");
                assert_eq!(toggle.y, stop.y);
                let hit = |x, y| sidebar.aloud_buttons().at(Position::new(x, y)).copied();
                assert_eq!(hit(toggle.x, toggle.y), Some(button));
                assert_eq!(hit(stop.x, stop.y), Some(aloud::Button::Stop));
                assert_eq!(hit(toggle.x, toggle.y - 1), None);
                assert_eq!(hit(toggle.x - 2, toggle.y), None, "on what it says");
                assert_eq!(usize::from(stop.x), usize::from(area.right()) - 3, "at the right");
            }
            let mut buffer = Buffer::empty(area);
            sidebar.draw_collapsed(&mut buffer, area, SystemTime::UNIX_EPOCH);
            assert_eq!(sidebar.aloud_buttons(), &Spots::default());
        }
    }

    #[test]
    fn a_pause_shows_on_the_sidebar_collapsed() {
        let marked = |shown| {
            let area = Rect::new(0, 0, COLLAPSED, 6);
            let mut sidebar = Sidebar::default();
            sidebar.aloud(Some(shown));
            let mut buffer = Buffer::empty(area);
            sidebar.draw_collapsed(&mut buffer, area, SystemTime::UNIX_EPOCH);
            buffer.content().iter().any(|cell| cell.symbol() == aloud::PLAY)
        };
        for paused in [false, true] {
            let (whose, text) = (aloud::Whose::Selection, String::new());
            assert_eq!(marked(aloud::Shown::Reading { whose, text, paused }), paused);
            assert_eq!(marked(aloud::Shown::Fetching { got: 0, paused }), paused);
        }
    }

    fn two_to_move() -> Sidebar {
        let mut sidebar = fresh();
        sidebar.hear(two_to_move_host());
        sidebar.focus(Keys::Sidebar);
        sidebar
    }

    fn two_to_move_host() -> Host {
        let auth = |pane: Pane| Pane { tab_name: "auth".to_owned(), ..pane };
        let work = vec![
            pane(1, 0, "/w/a", "fish"),
            auth(pane(2, 1, "/w/b", "fish")),
            auth(pane(3, 1, "/w/luneta/main", "claude")),
        ];
        let play = vec![pane(5, 0, "/w/p", "fish")];
        let fold = Agent {
            session_id: "0b7e1c2a-5d1f-4c8e-9a43-1f2e3d4c5b6a".to_owned(),
            status_since: None,
            ..agent("work", 3, Status::Idle, Some("fold"))
        };
        host(vec![session("work", 10, work), session("play", 20, play)], vec![fold])
    }

}
