use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::{Dimensions as _, GridCell as _};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use lunetad::changes::Unchanged;
use lunetad::links::Branch;
use lunetad::projects::{self, Name, ProjectId, Seen, Used, ViewId};
use lunetad::transcripts::SessionId;
use lunetad::zoxide::{self, Dir};
use lunetad::{eza, screens, Destination, Directory, SessionName};

use crate::actions::{self, Action, Spec};
use crate::alerts::Doing;
use crate::away::{self, Away};
use crate::close::{self, Act, Deletes, Ends, Plan, Untold};
use crate::gruvbox;
use crate::host::Event;
use crate::screen;
use crate::machines::Machine;
use crate::menu::{self, Chain, Chained, Done, Hint, Origin, Target};
use crate::sidebar::{
    self, ACCENT, ERROR, Finder, Fit, Keep, LIT, Listed, Mark, NAME, ViewKey, ViewPlace, Place,
    ProjectRow, WorktreePlace,
};
use crate::spots::{Drawn, Spots};
use crate::projected::{self, Projected};
use crate::worktrees;

const TITLE: &str = "luneta";

const PREVIEW_DELAY: Duration = Duration::from_millis(200);

const PEEKS_MAX: usize = 32;

const PEEK_LINES_MAX: usize = 200;

const PEEK_COLUMNS_MAX: usize = 1000;

const INPUT_HEIGHT: u16 = 3;

const MIN_RESULTS: u16 = 3;

const MIN_HALF: u16 = 26;

const MIN_ACTION: usize = 12;

const UNOPENED: &str = "not open";

const BORDER_LINES: u16 = 2;

const HELP_LINES: u16 = 1;

const PALETTE_ROWS: u16 = 8;

const PALETTE_HEIGHT: u16 = BORDER_LINES + PALETTE_ROWS + BORDER_LINES + INPUT_HEIGHT + HELP_LINES;

const PALETTE_WIDTH_MIN: u16 = 48;

const PALETTE_WIDTH_MAX: u16 = 80;

const _: () = assert!(PALETTE_WIDTH_MIN <= PALETTE_WIDTH_MAX);

const _: () = assert!(PALETTE_ROWS + BORDER_LINES >= MIN_RESULTS);

#[derive(Debug, Default)]
pub struct Listing {
    pub here: Option<Here>,
    pub shown: Option<(Machine, WorktreePlace)>,
    pub local: Runs,
    pub remotes: Vec<(Destination, Runs)>,
    pub shelves: Vec<Shelved>,
    pub away: Option<String>,
    pub this: String,
}

#[derive(Clone, Debug)]
pub struct Shelved {
    pub on: Machine,
    pub away: Option<&'static str>,
    pub projects: Vec<Housed>,
}

#[derive(Clone, Debug)]
pub struct Housed {
    pub project: ProjectRow,
    pub dir: projects::Dir,
    pub worktrees: Vec<Worktree>,
    pub unopened: Unopened,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unopened {
    Folder,
    Unlisted(String),
    Listed(Vec<projects::Dir>),
}

impl Unopened {
    #[must_use]
    pub fn refuses_new_worktree(&self, project: &ProjectRow) -> Option<String> {
        match self {
            Unopened::Folder => Some(projects::folder_has_one_worktree(project.listed().name())),
            Unopened::Unlisted(_) | Unopened::Listed(_) => None,
        }
    }

    #[must_use]
    pub fn folders(&self) -> &[projects::Dir] {
        match self {
            Unopened::Listed(folders) => folders,
            Unopened::Folder | Unopened::Unlisted(_) => &[],
        }
    }
}

#[derive(Clone, Debug)]
pub struct Worktree {
    pub place: WorktreePlace,
    pub name: String,
    pub folder: projects::Dir,
    pub branch: Option<String>,
    pub seen: Seen,
    pub used: Option<Used>,
    pub terminals: Vec<ViewKey>,
    pub held: Vec<ViewId>,
    pub waiting: usize,
    pub ending: Result<Vec<close::Doing>, Untold>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Here {
    Session { on: Machine, session: String },
    View { on: Machine, place: ViewPlace },
}

impl Here {
    pub fn on(&self) -> &Machine {
        match self {
            Here::Session { on, .. } | Here::View { on, .. } => on,
        }
    }

    pub fn session(&self) -> &str {
        match self {
            Here::Session { session, .. } => session,
            Here::View { place, .. } => place.session.name().as_str(),
        }
    }

    pub fn view(&self) -> Option<&ViewPlace> {
        match self {
            Here::Session { .. } => None,
            Here::View { place, .. } => Some(place),
        }
    }
}

#[derive(Debug, Default)]
pub struct Runs {
    pub agents: Vec<Seated>,
}

#[derive(Clone, Debug)]
pub struct Seated {
    pub place: ViewPlace,
    pub label: String,
    pub mark: Mark,
    pub status: String,
    pub since: Option<SystemTime>,
    pub dir: String,
    pub pid: u32,
    pub transcript: Option<SessionId>,
    pub doing: Doing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Aim {
    At { target: Rc<Target>, worktree: Option<Rc<Target>> },
    Unplaced(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub aim: Aim,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Run {
    Chord(actions::Chord),
    Menu(Rc<Target>),
    Row(Rc<Target>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Actions,
    Worktrees,
    Agents,
    Dirs,
}

impl List {
    #[must_use]
    pub fn opened_by(chord: actions::Chord) -> Option<List> {
        match chord {
            actions::PALETTE => Some(List::Actions),
            actions::WORKTREES => Some(List::Worktrees),
            actions::AGENTS => Some(List::Agents),
            actions::DIRS => Some(List::Dirs),
            _ => None,
        }
    }

    fn next(self) -> List {
        match self {
            List::Actions => List::Worktrees,
            List::Worktrees => List::Agents,
            List::Agents => List::Dirs,
            List::Dirs => List::Actions,
        }
    }

    fn back(self) -> List {
        match self {
            List::Actions => List::Dirs,
            List::Worktrees => List::Actions,
            List::Agents => List::Worktrees,
            List::Dirs => List::Agents,
        }
    }

    fn previews(self) -> bool {
        match self {
            List::Actions => false,
            List::Worktrees | List::Agents | List::Dirs => true,
        }
    }

    fn title(self) -> &'static str {
        match self {
            List::Actions => "Actions",
            List::Worktrees => "Worktrees",
            List::Agents => "Agents",
            List::Dirs => "Directories",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    Close,
    Go { on: Machine, place: Place },
    Add { on: Machine, project: ProjectRow, folder: Directory },
    Make { on: Machine, session: SessionName, cwd: Directory },
    Plan { on: Machine, session: Listed, deletes: Deletes },
    Delete { on: Machine, act: Act },
    Chord(actions::Chord),
    Menu { target: Target, origin: Origin, action: Action },
    Row { target: Target, action: Action },
    Done { done: Done, origin: Origin },
}

pub enum Heard {
    Peek {
        on: Machine,
        session: Listed,
        asked: Option<ViewKey>,
        peek: Result<(u32, String), String>,
    },
    Renamed { from: String, to: Name, renamed: Result<(), Unchanged> },
    Closed { name: String, closed: Result<(), Unchanged> },
    Dirs { on: Machine, ask: u64, visited: Result<Vec<Dir>, String> },
    Listed { on: Machine, path: Directory, listed: Result<String, String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Peek { on: Machine, session: Listed, asked: Option<ViewKey> },
    Rename { on: Machine, renames: Renames, to: Name },
    Close { on: Machine, place: WorktreePlace, name: String, seen: Vec<ViewId> },
    Dirs { on: Machine, ask: u64 },
    List { on: Machine, path: Directory },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Renames {
    Worktree { place: WorktreePlace, name: String },
    Project(ProjectRow),
}

impl Renames {
    fn name(&self) -> &str {
        match self {
            Renames::Worktree { name, .. } => name,
            Renames::Project(project) => project.listed().name().as_str(),
        }
    }
}

pub fn ask(number: u64, ask: Ask, projected: &Projected, events: &Sender<Event>) {
    let events = events.clone();
    let hear: Box<dyn FnOnce() -> Heard + Send> = match ask {
        Ask::Peek { on, session, asked } => {
            let terminal = match asked {
                Some(view) => projected::route(&on, view),
                None => projected.whole(&on, &session),
            };
            Box::new(move || {
                let peek = terminal.and_then(|key| {
                    screens::terminal(key).map(|screen| (key.id.get(), screen))
                });
                Heard::Peek { on, session, asked, peek }
            })
        },
        Ask::Rename { on, renames, to } => Box::new(move || {
            let renamed = match &renames {
                Renames::Worktree { place, .. } => {
                    worktrees::renamed_worktree(&on, place.worktree, Some(&to))
                },
                Renames::Project(project) => {
                    worktrees::renamed_project(&on, project.id(), &to)
                },
            };
            Heard::Renamed { from: renames.name().to_owned(), to, renamed }
        }),
        Ask::Close { on, place, name, seen } => Box::new(move || {
            let closed = worktrees::closed_there(&on, place.worktree, &seen);
            Heard::Closed { name, closed }
        }),
        Ask::Dirs { on, ask } => Box::new(move || {
            let visited = match &on {
                Machine::Local => zoxide::visited(),
                Machine::Remote(destination) => {
                    reach(destination).and_then(|mut away| away.dirs())
                },
            };
            Heard::Dirs { on, ask, visited }
        }),
        Ask::List { on, path } => Box::new(move || {
            let listed = match &on {
                Machine::Local => eza::listed(&path),
                Machine::Remote(destination) => Away::reach(destination, away::ANSWER)
                    .and_then(|mut away| away.contents(&path)),
            };
            Heard::Listed { on, path, listed }
        }),
    };
    thread::spawn(move || {
        let heard = hear();
        let _ = events.send(Event::Picker { number, heard });
    });
}

fn reach(destination: &Destination) -> Result<Away, String> {
    Away::reach(destination, away::ANSWER).map_err(|why| format!("{destination}: {why}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Key(KeyEvent),
    Row(Key),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Worktree { on: Machine, place: WorktreePlace },
    Project { on: Machine, project: ProjectRow, lone: Lone },
    Unopened { on: Machine, project: ProjectRow, folder: Directory },
    Machine { on: Machine },
    Agent { on: Machine, place: ViewPlace },
    Dir { on: Machine, path: Directory, session: SessionName },
    New { on: Machine, project: ProjectRow, branch: Branch },
    Action(Action, Run),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lone {
    Folder(Directory),
    Clone,
    Closed,
    Unlisted(String),
}

impl Lone {
    fn of(housed: &Housed) -> Option<Lone> {
        if !housed.worktrees.is_empty() {
            return None;
        }
        match &housed.unopened {
            Unopened::Folder => Some(Lone::Folder((&housed.dir).into())),
            Unopened::Listed(folders) if folders.is_empty() => Some(Lone::Clone),
            Unopened::Listed(_) => Some(Lone::Closed),
            Unopened::Unlisted(why) => Some(Lone::Unlisted(why.clone())),
        }
    }

    fn tag(&self) -> &'static str {
        match self {
            Lone::Folder(_) | Lone::Clone => "no worktree",
            Lone::Closed => "none open",
            Lone::Unlisted(_) => "not listed",
        }
    }
}

#[derive(Clone, Debug)]
struct Row {
    key: Key,
    name: String,
    hits: Vec<usize>,
    rules: Rules,
}

impl Row {
    fn plain(key: Key, name: String) -> Row {
        Row { key, name, hits: Vec::new(), rules: Rules::default() }
    }
}

#[derive(Clone, Debug, Default)]
struct Rules {
    machine: Option<String>,
}

enum Item<'a> {
    Machine(&'a str),
    Row(usize),
}

#[derive(Debug, Default)]
struct Rows {
    rows: Vec<Row>,
    at: Option<usize>,
}

impl Rows {
    fn replace(&mut self, rows: Vec<Row>, held: Option<&Key>, start: usize) {
        let at = held.and_then(|held| rows.iter().position(|row| row.key == *held));
        let over = |rows: &[Row]| {
            usize::from(matches!(rows.first().map(|row| &row.key), Some(Key::New { .. })))
        };
        let was = self.at.filter(|_| held.is_some());
        let was = was.map_or(start, |was| (was + over(&rows)).saturating_sub(over(&self.rows)));
        let last = rows.len().checked_sub(1);
        self.at = at.or_else(|| last.map(|last| was.min(last)));
        self.rows = rows;
        assert_eq!(self.at.is_some(), !self.rows.is_empty(), "a row is lit while there is one");
        if let Some(at) = self.at {
            assert!(at < self.rows.len(), "the lit row {at} is one of {}", self.rows.len());
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.at?)
    }

    fn step(&mut self, by: isize) {
        let Some(at) = self.at else {
            return;
        };
        let last = self.rows.len().saturating_sub(1);
        let to = if by < 0 { at.saturating_sub(by.unsigned_abs()) } else { at + by.unsigned_abs() };
        self.at = Some(to.min(last));
    }
}

#[derive(Debug)]
enum Stage {
    Search,
    Rename { on: Machine, renames: Renames, input: String },
    Planning { on: Machine, place: ViewPlace, agreed: Option<Plan> },
    Asking { on: Machine, place: ViewPlace, plan: Plan, asks: String },
    Closing { on: Machine, place: WorktreePlace, name: String, ends: Ends, asks: String },
    Chain { chain: Chain, on: Machine, aimed: Aimed },
}

#[derive(Debug)]
enum Aimed {
    Palette,
    Project(ProjectRow),
}

struct Shot {
    screen: Box<Term<VoidListener>>,
    first: usize,
    end: usize,
}

impl fmt::Debug for Shot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Shot {{ lines: {}..{} }}", self.first, self.end)
    }
}

impl Shot {
    fn of(lines: &[&str]) -> Shot {
        let widest = lines.iter().map(|line| line.len()).max().unwrap_or(0);
        let columns = widest.clamp(1, PEEK_COLUMNS_MAX);
        let size = TermSize::new(columns, lines.len().max(1));
        let mut screen = Term::new(Config::default(), &size, VoidListener);
        let mut parser: Processor<StdSyncHandler> = Processor::new();
        parser.advance(&mut screen, lines.join("\r\n").as_bytes());
        let mut used = vec![false; screen.screen_lines()];
        for indexed in screen.renderable_content().display_iter {
            let line = usize::try_from(indexed.point.line.0).ok();
            if let Some(used) = line.and_then(|line| used.get_mut(line)) {
                *used |= !indexed.cell.is_empty();
            }
        }
        let first = used.iter().position(|used| *used).unwrap_or(0);
        let end = used.iter().rposition(|used| *used).map_or(first, |last| last + 1);
        Shot { screen: Box::new(screen), first, end }
    }

    fn is_blank(&self) -> bool {
        self.first == self.end
    }

    fn show(&self, buffer: &mut Buffer, area: Rect, top: usize) {
        screen::show(&self.screen, buffer, area, top);
    }
}

#[derive(Debug)]
enum Peek {
    Reading,
    Ready { shot: Shot },
    Failed(String),
}

impl Peek {
    fn of(dumped: &str) -> Peek {
        let lines: Vec<&str> = dumped.lines().collect();
        let lines = &lines[lines.len().saturating_sub(PEEK_LINES_MAX)..];
        Peek::Ready { shot: Shot::of(lines) }
    }
}

#[derive(Debug)]
enum Contents {
    Reading,
    Ready { total: usize, shot: Shot },
    Failed(String),
}

impl Contents {
    fn of(listed: &str) -> Contents {
        let entries: Vec<&str> = listed.lines().filter(|line| !line.trim().is_empty()).collect();
        let shot = Shot::of(&entries[..entries.len().min(PEEK_LINES_MAX)]);
        Contents::Ready { total: entries.len(), shot }
    }
}

type Looked = (Machine, Listed, Option<ViewKey>);

type Located = (Machine, Directory);

type Found<'a> = (Option<Used>, String, Option<&'a str>, String, Key);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Preview {
    Screen(Looked),
    Dir(Located),
}

#[derive(Debug)]
enum Visited {
    Asking(u64),
    Listed(Vec<Dir>),
    Failed(String),
}

#[derive(Debug)]
pub struct Picker {
    number: u64,
    list: List,
    term: String,
    stage: Stage,
    listing: Listing,
    visited: HashMap<Machine, Visited>,
    dirs_asked: u64,
    worktrees: Rows,
    agents: Rows,
    dirs: Rows,
    peeks: HashMap<Looked, Peek>,
    contents: HashMap<Located, Contents>,
    wanted: Option<(Preview, Instant)>,
    error: Option<String>,
    asking: Vec<Ask>,
    aim: Machine,
    aiming: bool,
    scope: Option<(Machine, ProjectId)>,
    palette: Palette,
    actions: Rows,
}

impl Picker {
    pub fn opening(
        number: u64,
        listing: Listing,
        (on, project): (Machine, ProjectId),
        palette: Palette,
    ) -> Picker {
        let mut picker = Picker::open(number, List::Worktrees, listing, on.clone(), palette);
        picker.scope = Some((on, project));
        picker.rebuild(true);
        picker
    }

    pub fn open(
        number: u64,
        list: List,
        listing: Listing,
        aim: Machine,
        palette: Palette,
    ) -> Picker {
        let mut picker = Picker {
            number,
            list,
            term: String::new(),
            stage: Stage::Search,
            listing,
            visited: HashMap::new(),
            dirs_asked: 0,
            worktrees: Rows::default(),
            agents: Rows::default(),
            dirs: Rows::default(),
            peeks: HashMap::new(),
            contents: HashMap::new(),
            wanted: None,
            error: None,
            asking: Vec::new(),
            aim,
            aiming: true,
            scope: None,
            palette,
            actions: Rows::default(),
        };
        picker.ask_dirs();
        picker.rebuild(true);
        picker
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asking)
    }

    pub fn listed(&mut self, listing: Listing) {
        self.listing = listing;
        let machines = self.machines();
        self.visited.retain(|on, _| machines.contains(on));
        self.ask_dirs();
        self.rebuild(false);
    }

    fn machines(&self) -> Vec<Machine> {
        self.each().map(|(on, _)| on).collect()
    }

    fn each(&self) -> impl Iterator<Item = (Machine, &Runs)> {
        let others = self.listing.remotes.iter();
        let others = others.map(|(destination, runs)| (Machine::Remote(destination.clone()), runs));
        std::iter::once((Machine::Local, &self.listing.local)).chain(others)
    }

    fn runs(&self, on: &Machine) -> Option<&Runs> {
        match on {
            Machine::Local => Some(&self.listing.local),
            Machine::Remote(destination) => {
                let mut remotes = self.listing.remotes.iter();
                remotes.find(|(at, _)| at == destination).map(|(_, runs)| runs)
            },
        }
    }

    fn shelved(&self, on: &Machine) -> Option<&Shelved> {
        self.listing.shelves.iter().find(|shelved| shelved.on == *on)
    }

    fn housed(&self, on: &Machine, project: &ProjectRow) -> Option<&Housed> {
        let projects = &self.shelved(on)?.projects;
        projects.iter().find(|housed| housed.project.id() == project.id())
    }

    fn worktree(&self, on: &Machine, place: &WorktreePlace) -> Option<&Worktree> {
        let worktrees = &self.housed(on, &place.project)?.worktrees;
        worktrees.iter().find(|worktree| worktree.place.worktree == place.worktree)
    }

    fn row_worktree(&self, on: &Machine, place: &WorktreePlace) -> &Worktree {
        let worktree = self.worktree(on, place);
        worktree.expect("a worktree's row is rebuilt with the listing that holds it")
    }

    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        self.shelved(on)?.away?;
        Some(format!("lunetad does not reach {} now", self.machine_name(on)))
    }

    fn is_shown(&self, on: &Machine, place: &WorktreePlace) -> bool {
        self.listing.shown.as_ref().is_some_and(|(at, shown)| at == on && shown == place)
    }

    fn focus(&self, on: &Machine, worktree: &Worktree) -> Option<ViewKey> {
        let here = self.here_view(on).map(|place| place.view);
        let focused = here.filter(|view| worktree.terminals.contains(view));
        focused.or_else(|| worktree.terminals.first().copied())
    }

    fn seated(&self, on: &Machine, place: &ViewPlace) -> Option<&Seated> {
        self.runs(on)?.agents.iter().find(|seated| seated.place == *place)
    }

    fn ask_dirs(&mut self) {
        for on in self.machines() {
            if !self.visited.contains_key(&on) {
                self.dirs_asked += 1;
                let ask = self.dirs_asked;
                self.visited.insert(on.clone(), Visited::Asking(ask));
                self.asking.push(Ask::Dirs { on, ask });
            }
        }
    }

    pub fn heard(&mut self, heard: Heard) -> Picked {
        match heard {
            Heard::Peek { on, session, asked, peek } => {
                let peek = match peek {
                    Ok((_, dumped)) => Peek::of(&dumped),
                    Err(why) => Peek::Failed(why),
                };
                self.peeks.insert((on, session, asked), peek);
            },
            Heard::Renamed { from, to, renamed } => {
                if let Err(unchanged) = renamed {
                    let to = to.as_str();
                    let refused = format!("{from} is not called {to}");
                    let in_doubt = format!("{from} may be called {to}");
                    self.error = Some(unchanged.said(&refused, &in_doubt));
                }
            },
            Heard::Closed { name, closed } => {
                if let Err(unchanged) = closed {
                    let refused = format!("{name} stays");
                    let in_doubt = format!("{name} may be closed");
                    self.error = Some(unchanged.said(&refused, &in_doubt));
                }
            },
            Heard::Dirs { on, ask, visited } => {
                if !matches!(self.visited.get(&on), Some(Visited::Asking(asked)) if *asked == ask) {
                    return Picked::Nothing;
                }
                let visited = match visited {
                    Ok(dirs) => Visited::Listed(dirs),
                    Err(why) => Visited::Failed(why),
                };
                self.visited.insert(on, visited);
                self.rebuild(false);
            },
            Heard::Listed { on, path, listed } => {
                let contents = match listed {
                    Ok(listed) => Contents::of(&listed),
                    Err(why) => Contents::Failed(why),
                };
                self.contents.insert((on, path), contents);
            },
        }
        Picked::Nothing
    }

    pub fn key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        if control && key.code == KeyCode::Char('c') {
            return Picked::Close;
        }
        self.error = None;
        self.aiming = false;
        let picked = match std::mem::replace(&mut self.stage, Stage::Search) {
            Stage::Search => self.search_key(key),
            Stage::Rename { on, renames, input } => self.rename_key(key, on, renames, input),
            Stage::Planning { on, place, agreed } => {
                if key.code != KeyCode::Esc {
                    self.stage = Stage::Planning { on, place, agreed };
                }
                Picked::Nothing
            },
            Stage::Asking { on, place, plan, .. } => {
                let yes = key.code == KeyCode::Char('y') && plain(key);
                if yes { self.plan(on, place, Some(plan)) } else { Picked::Nothing }
            },
            Stage::Closing { on, place, name, ends, .. } => {
                if key.code == KeyCode::Char('y') && plain(key) {
                    self.close(on, place, name, Some(&ends));
                }
                Picked::Nothing
            },
            Stage::Chain { .. } if (key.code, key.modifiers) == actions::PALETTE => Picked::Close,
            Stage::Chain { chain, on, aimed } => match chain.key(key, &on) {
                Chained::Typing(chain) => {
                    self.stage = Stage::Chain { chain, on, aimed };
                    Picked::Nothing
                },
                Chained::Back => Picked::Nothing,
                Chained::Close => Picked::Close,
                Chained::Done(done) => Picked::Done { done, origin: self.palette.origin },
            },
        };
        self.follow();
        picked
    }

    pub fn paste(&mut self, text: &str) {
        self.aiming = false;
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match &mut self.stage {
            Stage::Search => {
                self.term.push_str(&text);
                self.rebuild(true);
            },
            Stage::Rename { input, .. } => input.push_str(&text),
            Stage::Chain { chain, .. } => chain.paste(&text),
            Stage::Planning { .. } | Stage::Asking { .. } | Stage::Closing { .. } => {},
        }
    }

    pub fn scroll(&mut self, by: isize) {
        self.aiming = false;
        if let Stage::Search = self.stage {
            self.rows_mut().step(by);
            self.follow();
        }
    }

    pub fn click(&mut self, pick: Pick) -> Picked {
        let row = match pick {
            Pick::Key(key) => return self.key(&key),
            Pick::Row(row) => row,
        };
        let at = self.rows().rows.iter().position(|it| it.key == row);
        let Some(at) = at.filter(|_| self.picks()) else {
            return Picked::Nothing;
        };
        self.error = None;
        self.aiming = false;
        self.rows_mut().at = Some(at);
        self.follow();
        self.enter()
    }

    pub fn planned(&mut self, plan: Result<Plan, String>) -> Picked {
        let Stage::Planning { on, place, agreed } =
            std::mem::replace(&mut self.stage, Stage::Search)
        else {
            return Picked::Nothing;
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(why) => {
                self.error = Some(why);
                return Picked::Nothing;
            },
        };
        let asks = if agreed.as_ref() == Some(&plan) { None } else { plan.question() };
        if let Some(asks) = asks {
            self.stage = Stage::Asking { on, place, plan, asks };
            return Picked::Nothing;
        }
        Picked::Delete { on, act: plan.act }
    }

    pub fn due(&self) -> Option<Instant> {
        let (preview, since) = self.wanted.as_ref()?;
        let asked = match preview {
            Preview::Screen(looked) => self.peeks.contains_key(looked),
            Preview::Dir(located) => self.contents.contains_key(located),
        };
        (!asked).then(|| *since + PREVIEW_DELAY)
    }

    pub fn ticked(&mut self, now: Instant) {
        if self.due().is_none_or(|due| now < due) {
            return;
        }
        let Some((preview, _)) = self.wanted.clone() else {
            return;
        };
        match preview {
            Preview::Screen(looked) => {
                if self.peeks.len() >= PEEKS_MAX {
                    self.peeks.clear();
                }
                self.peeks.insert(looked.clone(), Peek::Reading);
                let (on, session, asked) = looked;
                self.asking.push(Ask::Peek { on, session, asked });
            },
            Preview::Dir((on, path)) => {
                if self.contents.len() >= PEEKS_MAX {
                    self.contents.clear();
                }
                self.contents.insert((on.clone(), path.clone()), Contents::Reading);
                self.asking.push(Ask::List { on, path });
            },
        }
    }

    pub fn area(over: Rect) -> Rect {
        Rect::new(
            over.x + over.width / 5,
            over.y + over.height / 5,
            over.width * 3 / 5,
            over.height * 3 / 5,
        )
    }

    fn compact(over: Rect) -> Rect {
        let three_fifths = u16::try_from(u32::from(over.width) * 3 / 5);
        let three_fifths = three_fifths.expect("three fifths of a width fits its type");
        let (fewest, most) = (over.width.min(PALETTE_WIDTH_MIN), over.width.min(PALETTE_WIDTH_MAX));
        let width = three_fifths.clamp(fewest, most);
        let height = PALETTE_HEIGHT.min(over.height);
        let x = over.x + (over.width - width) / 2;
        let y = (over.y + over.height / 5).min(over.bottom() - height);
        let area = Rect::new(x, y, width, height);
        assert!(area.right() <= over.right(), "the palette fits the width it is drawn over");
        assert!(area.bottom() <= over.bottom(), "the palette fits the height it is drawn over");
        area
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) -> Drawn {
        let palette = self.list == List::Actions;
        let area = if palette { Picker::compact(over) } else { Picker::area(over) };
        let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2))
        else {
            return Drawn::Picker { number: self.number, area, spots: Spots::default() };
        };
        framed(buffer, area, TITLE, "", Vec::new());
        let layout = Layout::of(Rect::new(area.x + 1, area.y + 1, width, height));
        let layout = if self.list.previews() {
            layout
        } else {
            Layout { results: layout.full, preview: None, ..layout }
        };
        if let Stage::Rename { on, renames, input } = &self.stage {
            let spots = self.draw_rename(buffer, &layout, on, renames, input);
            return Drawn::Picker { number: self.number, area, spots };
        }
        if let Stage::Chain { chain, on, aimed } = &self.stage {
            let aimed = match aimed {
                Aimed::Palette => self.aimed_at(),
                Aimed::Project(project) => {
                    let named = self.housed(on, project).map_or(project, |it| &it.project);
                    there(named.listed().name().as_str(), on)
                },
            };
            let spots = self.draw_chain(buffer, &layout, chain, &aimed);
            return Drawn::Picker { number: self.number, area, spots };
        }
        let mut spots = Spots::default();
        if let Some(results) = layout.results {
            let inner = usize::from(results.width.saturating_sub(2));
            let rows = usize::from(results.height.saturating_sub(2));
            let notes = self.notes(inner);
            let (body, picks) = self.body(inner, rows.saturating_sub(notes.len()), now);
            let count = count(self.rows().at, self.rows().rows.len());
            let (lines, body_at) = anchored(notes, body, rows);
            framed(buffer, results, self.list.title(), &count, lines);
            spots.extend(picks.down(body_at).placed(results.inner(Margin::new(1, 1))));
        }
        if let Some(preview) = layout.preview {
            self.draw_preview(buffer, preview, now);
        }
        let prompt = self.prompt(usize::from(layout.input.width.saturating_sub(2)));
        let title = if palette { self.aimed_at() } else { self.list.title().to_owned() };
        draw_input(buffer, &layout, &title, prompt);
        spots.extend(self.draw_help(buffer, layout.help));
        Drawn::Picker { number: self.number, area, spots }
    }

    fn draw_chain(
        &self,
        buffer: &mut Buffer,
        layout: &Layout,
        chain: &Chain,
        aimed: &str,
    ) -> Spots<Pick> {
        if let Some(full) = layout.full {
            let inner = usize::from(full.width.saturating_sub(2));
            framed(buffer, full, chain.title(), &chain.right(), menu::chain_says(chain, inner));
        }
        let width = usize::from(layout.input.width.saturating_sub(2));
        let prompt = typed(chain.typed(), Ok(chain.enter().to_owned()), width);
        draw_input(buffer, layout, aimed, prompt);
        self.draw_help(buffer, layout.help)
    }

    fn picks(&self) -> bool {
        matches!(self.stage, Stage::Search)
    }

    fn rows(&self) -> &Rows {
        match self.list {
            List::Actions => &self.actions,
            List::Worktrees => &self.worktrees,
            List::Agents => &self.agents,
            List::Dirs => &self.dirs,
        }
    }

    fn rows_mut(&mut self) -> &mut Rows {
        match self.list {
            List::Actions => &mut self.actions,
            List::Worktrees => &mut self.worktrees,
            List::Agents => &mut self.agents,
            List::Dirs => &mut self.dirs,
        }
    }

    fn search_key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        let held_super = key.modifiers == KeyModifiers::SUPER;
        if (key.code, key.modifiers) == actions::PALETTE {
            if self.list == List::Actions {
                return Picked::Close;
            }
            self.list = List::Actions;
            return Picked::Nothing;
        }
        match key.code {
            KeyCode::Esc => Picked::Close,
            KeyCode::Tab => {
                self.list = self.list.next();
                Picked::Nothing
            },
            KeyCode::BackTab => {
                self.list = self.list.back();
                Picked::Nothing
            },
            KeyCode::Char('j') if held_super => {
                self.list = List::Worktrees;
                Picked::Nothing
            },
            KeyCode::Char('b') if control => {
                self.list = List::Agents;
                Picked::Nothing
            },
            KeyCode::Char('d') if control => {
                self.list = List::Dirs;
                Picked::Nothing
            },
            KeyCode::Char('r') if control => {
                self.rename();
                Picked::Nothing
            },
            KeyCode::Enter => self.enter(),
            KeyCode::Delete => self.delete(),
            KeyCode::Down => {
                self.rows_mut().step(1);
                Picked::Nothing
            },
            KeyCode::Up => {
                self.rows_mut().step(-1);
                Picked::Nothing
            },
            KeyCode::Backspace => {
                if self.term.pop().is_some() {
                    self.rebuild(true);
                }
                Picked::Nothing
            },
            KeyCode::Char(c) if plain(key) => {
                self.term.push(c);
                self.rebuild(true);
                Picked::Nothing
            },
            _ => Picked::Nothing,
        }
    }

    fn enter(&mut self) -> Picked {
        let Some(row) = self.rows().selected() else {
            return Picked::Nothing;
        };
        match &row.key {
            Key::Worktree { on, place } if self.is_shown(on, place) => Picked::Close,
            Key::Worktree { on, place } => {
                Picked::Go { on: on.clone(), place: Place::Worktree(place.clone()) }
            },
            Key::Project { on, project, lone } => match lone {
                Lone::Folder(folder) => {
                    let (on, project, folder) = (on.clone(), project.clone(), folder.clone());
                    Picked::Add { on, project, folder }
                },
                Lone::Clone => {
                    let (chain, on) = (Chain::branch(project.id()), on.clone());
                    let aimed = Aimed::Project(project.clone());
                    self.stage = Stage::Chain { chain, on, aimed };
                    Picked::Nothing
                },
                Lone::Closed => {
                    self.scope = Some((on.clone(), project.id()));
                    self.term.clear();
                    self.rebuild(true);
                    Picked::Nothing
                },
                Lone::Unlisted(_) => Picked::Nothing,
            },
            Key::Unopened { on, project, folder } => {
                let (on, project, folder) = (on.clone(), project.clone(), folder.clone());
                Picked::Add { on, project, folder }
            },
            Key::Machine { .. } => Picked::Nothing,
            Key::Agent { on, place } => {
                Picked::Go { on: on.clone(), place: Place::View(place.clone()) }
            },
            Key::Dir { on, path, session } => {
                Picked::Make { on: on.clone(), session: session.clone(), cwd: path.clone() }
            },
            Key::New { on, project, branch } => {
                let done = Done::Worktree(on.clone(), project.id(), branch.clone());
                Picked::Done { done, origin: self.palette.origin }
            },
            Key::Action(action, run) => {
                let (action, run) = (*action, run.clone());
                self.act(action, run)
            },
        }
    }

    fn act(&mut self, action: Action, run: Run) -> Picked {
        match run {
            Run::Chord(chord) => {
                let Some(list) = List::opened_by(chord) else {
                    return Picked::Chord(chord);
                };
                self.list = list;
                Picked::Nothing
            },
            Run::Row(target) => Picked::Row { target: Target::clone(&target), action },
            Run::Menu(target) => {
                if let Some(why) = self.refused(&target, action) {
                    self.error = Some(why);
                    return Picked::Nothing;
                }
                if let Some(chain) = Chain::of(&target, action) {
                    let on = target.machine().clone();
                    self.stage = Stage::Chain { chain, on, aimed: Aimed::Palette };
                    return Picked::Nothing;
                }
                Picked::Menu { target: Target::clone(&target), origin: self.palette.origin, action }
            },
        }
    }

    fn refused(&self, target: &Target, action: Action) -> Option<String> {
        if action != Action::NewWorktree {
            return None;
        }
        let housed = self.housed(target.machine(), &target.project()?)?;
        housed.unopened.refuses_new_worktree(&housed.project)
    }

    fn run(&self, action: Action) -> Option<Run> {
        if !action.in_palette() {
            return None;
        }
        let Palette { aim, origin } = &self.palette;
        match (action.key(), aim, origin) {
            (actions::Key::Chord(chord), ..) => Some(Run::Chord(chord)),
            (
                actions::Key::Menu(_) | actions::Key::RowAndMenu(_),
                Aim::At { target, worktree },
                _,
            ) => {
                let mut targets = std::iter::once(target).chain(worktree);
                targets.find(|it| it.fits(action)).map(|it| Run::Menu(Rc::clone(it)))
            },
            (actions::Key::Row(_), Aim::At { target, .. }, Origin::Sidebar) => {
                action.fits(target.noun()).then(|| Run::Row(Rc::clone(target)))
            },
            (actions::Key::Row(_), Aim::At { .. }, Origin::Views)
            | (
                actions::Key::Menu(_) | actions::Key::RowAndMenu(_) | actions::Key::Row(_),
                Aim::Unplaced(_),
                _,
            )
            | (actions::Key::Pasted(_), ..) => None,
        }
    }

    fn action_rows(&self, finder: Option<&Finder<'_>>) -> Vec<Row> {
        let fitting = Action::ALL.into_iter().filter_map(|it| Some((it, self.run(it)?)));
        let worktrees = self.listed_worktrees(finder);
        let worktrees = worktrees.into_iter().filter(|row| !matches!(row.key, Key::Machine { .. }));
        let Some(finder) = finder else {
            let row = |(action, run)| Row::plain(Key::Action(action, run), titled(action));
            let rows = fitting.map(row);
            return rows.chain(worktrees).collect();
        };
        let term = self.term.to_lowercase();
        let mut found: Vec<(Tier, Fit, Row)> =
            fitting.filter_map(|(action, run)| found_action(finder, &term, action, run)).collect();
        found.sort_by_key(|(tier, fit, _)| (*tier, Reverse(*fit)));
        let actions = found.into_iter().map(|(tier, _, row)| (tier, Kind::Action, row));
        let places = worktrees.map(|row| (tier_of(&term, &row.name), Kind::Place, row));
        let mut ranked: Vec<(Tier, Kind, Row)> = actions.chain(places).collect();
        ranked.sort_by_key(|(tier, kind, _)| (*tier, *kind));
        let mut rows: Vec<Row> = ranked.into_iter().map(|(.., row)| row).collect();
        if rows.is_empty()
            && let Some(Ok(key)) = self.new_key()
        {
            rows.push(Row::plain(key, self.term.clone()));
        }
        rows
    }

    fn aimed_at(&self) -> String {
        let target = match &self.palette.aim {
            Aim::At { target, .. } => target.as_ref(),
            Aim::Unplaced(_) => return List::Actions.title().to_owned(),
        };
        let named = match target {
            Target::View(on, row) => {
                let project = row.session.name();
                let housed = self.housed(on, &ProjectRow::of(row.session.clone()));
                let mut worktrees = housed.iter().flat_map(|housed| &housed.worktrees);
                match worktrees.find(|worktree| worktree.terminals.contains(&row.view)) {
                    Some(worktree) => format!("{project} › {} · view {}", worktree.name, row.view),
                    None => format!("{project} · view {}", row.view),
                }
            },
            Target::Worktree(_, row) => {
                format!("{} › {}", row.place.project.listed().name(), row.name)
            },
            Target::Project(_, project) => project.project.listed().name().to_string(),
            Target::Machine(on) => return self.machine_name(on),
        };
        there(&named, target.machine())
    }

    fn here_project(&self) -> Option<(&Machine, &Housed)> {
        let (on, place) = self.listing.shown.as_ref()?;
        Some((on, self.housed(on, &place.project)?))
    }

    fn new_key(&self) -> Option<Result<Key, String>> {
        let (on, housed) = self.here_project()?;
        if let Some(why) = housed.unopened.refuses_new_worktree(&housed.project) {
            return Some(Err(why));
        }
        let (on, project) = (on.clone(), housed.project.clone());
        Some(Branch::parse(&self.term).map(|branch| Key::New { on, project, branch }))
    }

    fn delete(&mut self) -> Picked {
        let Some(key) = self.rows().selected().map(|row| row.key.clone()) else {
            return Picked::Nothing;
        };
        match key {
            Key::Worktree { on, place } => {
                let name = self.row_worktree(&on, &place).name.clone();
                self.close(on, place, name, None);
                Picked::Nothing
            },
            Key::Project { project, .. } => {
                let name = project.listed().name();
                self.error = Some(format!("{name} holds no worktree to close"));
                Picked::Nothing
            },
            Key::Agent { on, place } => self.plan(on, place, None),
            Key::Unopened { .. } => {
                self.error = Some("a worktree not open has no views to close".to_owned());
                Picked::Nothing
            },
            Key::Machine { .. } | Key::Dir { .. } | Key::New { .. } | Key::Action(..) => {
                Picked::Nothing
            },
        }
    }

    fn close(&mut self, on: Machine, place: WorktreePlace, name: String, agreed: Option<&Ends>) {
        let Some(worktree) = self.worktree(&on, &place) else {
            self.error = Some(format!("{name} is listed no more"));
            return;
        };
        let ends = Ends::of(&worktree.held, worktree.ending.as_deref().map_err(|untold| *untold));
        if let Some(why) = self.out_of_reach(&on) {
            self.error = Some(format!("{name} stays: {why}"));
            return;
        }
        if agreed != Some(&ends)
            && let Some(asks) = ends.question(&format!("Close worktree {name}"))
        {
            self.stage = Stage::Closing { on, place, name, ends, asks };
            return;
        }
        self.asking.push(Ask::Close { on, place, name, seen: ends.seen });
    }

    fn plan(&mut self, on: Machine, place: ViewPlace, agreed: Option<Plan>) -> Picked {
        let deletes = Deletes::View(place.view);
        let picked = Picked::Plan { on: on.clone(), session: place.session.clone(), deletes };
        self.stage = Stage::Planning { on, place, agreed };
        picked
    }

    fn rename(&mut self) {
        if self.list != List::Worktrees {
            return;
        }
        let (on, renames) = match self.worktrees.selected().map(|row| &row.key) {
            Some(Key::Worktree { on, place }) => {
                let name = self.row_worktree(on, place).name.clone();
                (on.clone(), Renames::Worktree { place: place.clone(), name })
            },
            Some(Key::Project { on, project, .. }) => {
                (on.clone(), Renames::Project(project.clone()))
            },
            Some(
                Key::Machine { .. }
                | Key::Agent { .. }
                | Key::Dir { .. }
                | Key::New { .. }
                | Key::Unopened { .. }
                | Key::Action(..),
            )
            | None => return,
        };
        if let Some(why) = self.out_of_reach(&on) {
            self.error = Some(format!("{} keeps its name: {why}", renames.name()));
            return;
        }
        self.stage = Stage::Rename { on, renames, input: String::new() };
    }

    fn rename_key(
        &mut self,
        key: &KeyEvent,
        on: Machine,
        renames: Renames,
        mut input: String,
    ) -> Picked {
        match key.code {
            KeyCode::Esc => return Picked::Nothing,
            KeyCode::Enter => {
                let Ok(to) = self.rename_to(&on, &renames, &input) else {
                    self.stage = Stage::Rename { on, renames, input };
                    return Picked::Nothing;
                };
                self.asking.push(Ask::Rename { on, renames, to });
                return Picked::Nothing;
            },
            KeyCode::Backspace => {
                input.pop();
            },
            KeyCode::Char(c) if plain(key) => input.push(c),
            _ => {},
        }
        self.stage = Stage::Rename { on, renames, input };
        Picked::Nothing
    }

    fn rename_to(&self, on: &Machine, renames: &Renames, name: &str) -> Result<Name, String> {
        let name = match renames {
            Renames::Worktree { .. } => Name::parse(name)?,
            Renames::Project(_) => Name::of_project(name)?,
        };
        if name.as_str() == renames.name() {
            return Err("already called that".to_owned());
        }
        let listed = match renames {
            Renames::Worktree { place, .. } => self.worktree(on, place).is_some(),
            Renames::Project(project) => self.housed(on, project).is_some(),
        };
        if !listed {
            return Err(format!("{} is listed no more", renames.name()));
        }
        Ok(name)
    }

    fn taken(&self, on: &Machine) -> Vec<&str> {
        let projects = self.shelved(on).into_iter().flat_map(|shelved| &shelved.projects);
        let projects = projects.map(|housed| housed.project.listed().name().as_str());
        self.here_session(on).into_iter().chain(projects).collect()
    }

    fn rebuild(&mut self, snap: bool) {
        let finder = (!self.term.is_empty()).then(|| Finder::new(&self.term));
        let ruled = !self.listing.remotes.is_empty();
        let mut agents = Vec::new();
        for (on, runs) in self.each() {
            let rule = ruled.then(|| self.machine_name(&on));
            grouped(&mut agents, self.agent_rows(finder.as_ref(), &on, runs), rule);
        }
        let mut worktrees = self.worktree_rows(finder.as_ref());
        if worktrees.is_empty()
            && self.scope.is_none()
            && let Some(Ok(key)) = self.new_key()
        {
            worktrees.push(Row::plain(key, self.term.clone()));
        }
        let (dirs, aimed) = self.dir_rows();
        let actions = self.action_rows(finder.as_ref());
        let worktree_start = match worktrees.first().map(|row| &row.key) {
            Some(Key::Worktree { on, place }) if self.is_shown(on, place) => {
                usize::from(self.term.is_empty())
            },
            Some(_) | None => 0,
        };
        let held = |rows: &Rows| rows.selected().map(|row| row.key.clone()).filter(|_| !snap);
        let (held_worktree, held_agent) = (held(&self.worktrees), held(&self.agents));
        let held_dir = held(&self.dirs).filter(|_| !self.aiming);
        let held_action = held(&self.actions);
        self.actions.replace(actions, held_action.as_ref(), 0);
        self.worktrees.replace(worktrees, held_worktree.as_ref(), worktree_start);
        self.agents.replace(agents, held_agent.as_ref(), 0);
        self.dirs.replace(dirs, held_dir.as_ref(), aimed.unwrap_or(0));
        self.follow();
    }

    fn worktree_rows(&self, finder: Option<&Finder<'_>>) -> Vec<Row> {
        if let Some((on, project)) = &self.scope {
            return self.worktree_rows_scoped(finder, on, *project);
        }
        self.listed_worktrees(finder)
    }

    fn listed_worktrees(&self, finder: Option<&Finder<'_>>) -> Vec<Row> {
        let tagged = self.tagged();
        let mut found = Vec::new();
        let mut machines = Vec::new();
        for shelved in &self.listing.shelves {
            let on = &shelved.on;
            if shelved.projects.is_empty() && (tagged || shelved.away.is_some()) {
                machines.push(Row::plain(Key::Machine { on: on.clone() }, self.machine_name(on)));
            }
            for housed in &shelved.projects {
                let project = housed.project.listed().name().as_str();
                if let Some(lone) = Lone::of(housed) {
                    assert!(housed.worktrees.is_empty(), "{project} alone has no worktree open");
                    let (on, row) = (on.clone(), housed.project.clone());
                    let key = Key::Project { on, project: row, lone };
                    let dir = sidebar::short_cwd(housed.dir.as_str());
                    found.push((None, project.to_owned(), None, dir, key));
                }
                for worktree in &housed.worktrees {
                    let name = format!("{project} › {}", worktree.name);
                    let folder = sidebar::short_cwd(worktree.folder.as_str());
                    let key = Key::Worktree { on: on.clone(), place: worktree.place.clone() };
                    found.push((worktree.used, name, worktree.branch.as_deref(), folder, key));
                }
            }
        }
        found.sort_by_key(|(used, ..)| Reverse(*used));
        let mut rows = matched(finder, candidates_of(&found));
        if finder.is_none() {
            rows.extend(machines);
        }
        rows
    }

    fn worktree_rows_scoped(
        &self,
        finder: Option<&Finder<'_>>,
        on: &Machine,
        project: ProjectId,
    ) -> Vec<Row> {
        let shelved = self.listing.shelves.iter().filter(|shelved| shelved.on == *on);
        let housed = shelved.flat_map(|shelved| &shelved.projects);
        let scoped = housed.filter(|housed| housed.project.id() == project);
        let found: Vec<Found<'_>> = scoped.flat_map(|housed| unopened_of(on, housed)).collect();
        matched(finder, candidates_of(&found))
    }

    fn agent_rows(&self, finder: Option<&Finder<'_>>, on: &Machine, runs: &Runs) -> Vec<Row> {
        let yours = self.here_view(on);
        let agents = runs.agents.iter().filter(|seated| Some(&seated.place) != yours);
        let agents: Vec<(String, Vec<Option<&str>>, Key)> = agents
            .map(|seated| {
                let session = seated.place.session.name().as_str();
                let also = vec![Some(session), Some(seated.dir.as_str())];
                let key = Key::Agent { on: on.clone(), place: seated.place.clone() };
                (agent_name(&runs.agents, seated), also, key)
            })
            .collect();
        matched(finder, agents.into_iter())
    }

    fn dir_rows(&self) -> (Vec<Row>, Option<usize>) {
        let matcher = SkimMatcherV2::default();
        let taken = self.taken(&Machine::Local);
        let machines = self.machines();
        let ruled = machines.len() > 1;
        let (mut rows, mut aimed) = (Vec::new(), None);
        for on in machines {
            let Some(Visited::Listed(visited)) = self.visited.get(&on) else {
                continue;
            };
            let named = visited.iter().map(|dir| {
                let session = match &on {
                    Machine::Local => dir.name.free(&taken),
                    Machine::Remote(_) => dir.name.clone(),
                };
                (dir, session)
            });
            let found = dir_rows(&matcher, &self.term, &on, named);
            let first = grouped(&mut rows, found, ruled.then(|| self.machine_name(&on)));
            if self.aim == on {
                aimed = first;
            }
        }
        (rows, aimed)
    }

    fn machine_name(&self, on: &Machine) -> String {
        match on {
            Machine::Local => self.listing.this.clone(),
            Machine::Remote(destination) => destination.to_string(),
        }
    }

    fn follow(&mut self) {
        let selected = self.rows().selected().filter(|_| self.list.previews());
        let preview = match selected.map(|row| &row.key) {
            Some(Key::Worktree { on, place }) => {
                let focus = self.focus(on, self.row_worktree(on, place));
                let looked = |focus| (on.clone(), place.project.listed().clone(), Some(focus));
                focus.map(|focus| Preview::Screen(looked(focus)))
            },
            Some(Key::Agent { on, place }) => {
                Some(Preview::Screen((on.clone(), place.session.clone(), Some(place.view))))
            },
            Some(Key::Dir { on, path, .. }) => Some(Preview::Dir((on.clone(), path.clone()))),
            Some(Key::Unopened { on, folder, .. }) => {
                Some(Preview::Dir((on.clone(), folder.clone())))
            },
            Some(Key::Project { .. } | Key::Machine { .. } | Key::New { .. } | Key::Action(..))
            | None => None,
        };
        match (preview, &self.wanted) {
            (Some(preview), Some((wanted, _))) if preview == *wanted => {},
            (Some(preview), _) => {
                match &preview {
                    Preview::Screen(looked) => {
                        if matches!(self.peeks.get(looked), Some(Peek::Failed(_))) {
                            self.peeks.remove(looked);
                        }
                    },
                    Preview::Dir(located) => {
                        if matches!(self.contents.get(located), Some(Contents::Failed(_))) {
                            self.contents.remove(located);
                        }
                    },
                }
                self.wanted = Some((preview, Instant::now()));
            },
            (None, _) => self.wanted = None,
        }
    }

    fn notes(&self, width: usize) -> Vec<Line<'static>> {
        let error = Style::new().fg(ERROR);
        let mut notes = Vec::new();
        if let Stage::Asking { asks, .. } | Stage::Closing { asks, .. } = &self.stage {
            let asking = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
            notes.extend(menu::wrap(asks, width, asking));
        }
        if let Some(why) = &self.error {
            notes.extend(menu::wrap(why, width, error));
        }
        match self.list {
            List::Actions => {
                if let Aim::Unplaced(why) = &self.palette.aim {
                    notes.extend(menu::wrap(why, width, error));
                }
            },
            List::Worktrees => {},
            List::Agents => {
                if let Some(why) = &self.listing.away {
                    let why = format!("lunetad does not answer: {why}");
                    notes.extend(menu::wrap(&why, width, error));
                }
            },
            List::Dirs => {
                for on in self.machines() {
                    if let Some(Visited::Failed(why)) = self.visited.get(&on) {
                        notes.extend(menu::wrap(why, width, error));
                    }
                }
            },
        }
        notes
    }

    fn body(
        &self,
        width: usize,
        room: usize,
        now: SystemTime,
    ) -> (Vec<Line<'static>>, Spots<Pick>) {
        let mut lines = Vec::new();
        let mut spots = Spots::default();
        let mut room = room;
        if self.list == List::Agents
            && let Some((yours, agents)) = self.yours()
        {
            if room == 0 {
                return (lines, spots);
            }
            lines.push(here_line(&agent_name(agents, yours), width));
            room -= 1;
        }
        if room == 0 {
            return (lines, spots);
        }
        let rows = self.rows();
        if rows.rows.is_empty() {
            lines.extend(menu::wrap(&self.empty(), width, Style::new().fg(gruvbox::GREY)));
            return (lines, spots);
        }
        let mut items = Vec::new();
        let mut lit_at = 0;
        for (at, row) in rows.rows.iter().enumerate() {
            items.extend(row.rules.machine.as_deref().map(Item::Machine));
            if rows.at == Some(at) {
                lit_at = items.len();
            }
            items.push(Item::Row(at));
        }
        let (start, end) = viewport(lit_at, items.len(), room);
        let frame = sidebar::frame(now);
        for item in &items[start..end] {
            let row = match item {
                Item::Machine(name) => {
                    lines.push(machine_rule(name, width));
                    continue;
                },
                Item::Row(row) => *row,
            };
            let lit = rows.at == Some(row);
            let line = self.row_line(&rows.rows[row], width, frame, now);
            if self.picks() {
                spots.add(lines.len(), 0..width, Pick::Row(rows.rows[row].key.clone()));
            }
            lines.push(if lit { line.patch_style(Style::new().bg(LIT)) } else { line });
        }
        (lines, spots)
    }

    fn here_session(&self, on: &Machine) -> Option<&str> {
        let here = self.listing.here.as_ref().filter(|here| here.on() == on)?;
        Some(here.session())
    }

    fn here_view(&self, on: &Machine) -> Option<&ViewPlace> {
        self.listing.here.as_ref().filter(|here| here.on() == on)?.view()
    }

    fn yours(&self) -> Option<(&Seated, &[Seated])> {
        let here = self.listing.here.as_ref()?;
        let (view, agents) = (here.view()?, &self.runs(here.on())?.agents);
        Some((agents.iter().find(|seated| seated.place == *view)?, agents))
    }

    fn row_line(&self, row: &Row, width: usize, frame: usize, now: SystemTime) -> Line<'static> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let grey = Style::new().fg(gruvbox::GREY);
        let (lead, name_style, right) = match &row.key {
            Key::Worktree { on, place } => {
                let worktree = self.row_worktree(on, place);
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = Vec::new();
                if worktree.waiting > 0 {
                    let (dot, style) = Mark::Waiting.glyph(frame);
                    right.push(Span::styled(format!("{dot}{} ", worktree.waiting), style));
                }
                right.extend(self.tag(on, away));
                let (stage, colour) = staged(worktree.seen);
                let style = if away.is_some() { dim } else { Style::new().fg(colour) };
                right.push(Span::styled(stage, style));
                let style = match away {
                    Some(_) => dim,
                    None if self.is_shown(on, place) => {
                        Style::new().fg(NAME).add_modifier(Modifier::BOLD)
                    },
                    None => bold,
                };
                (None, style, right)
            },
            Key::Project { on, lone, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = self.tag(on, away);
                right.push(Span::styled(lone.tag(), grey));
                (None, if away.is_some() { dim } else { bold }, right)
            },
            Key::Unopened { on, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = self.tag(on, away);
                right.push(Span::styled(UNOPENED, grey));
                (None, dim, right)
            },
            Key::Machine { on } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let said = match away {
                    Some(word) => Span::styled(word, Style::new().fg(ERROR)),
                    None => Span::styled("no projects", grey),
                };
                (None, dim, vec![said])
            },
            Key::Agent { on, place } => {
                let Some(seated) = self.seated(on, place) else {
                    return Line::from(Span::raw(" ".repeat(width)));
                };
                let (glyph, style) = seated.mark.glyph(frame);
                let name_style = if seated.mark == Mark::Waiting {
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                };
                let age = seated.since.map_or_default(|since| sidebar::age(since, now));
                let mut right = Vec::new();
                if width >= row.name.width() + seated.dir.width() + age.width() + 10 {
                    right.push(Span::styled(seated.dir.clone(), Style::new().fg(gruvbox::GREY)));
                    right.push(Span::raw("  "));
                }
                right.push(Span::styled(age, dim));
                (Some(Span::styled(glyph, style)), name_style, right)
            },
            Key::Dir { path, .. } => return dir_line(row, path.as_str(), width),
            Key::New { .. } => {
                let lead = Span::styled("+", Style::new().fg(gruvbox::GREEN));
                (Some(lead), bold, vec![Span::styled("new worktree", grey)])
            },
            Key::Action(action, _) => {
                let key = menu::spelled(*action, self.palette.origin);
                (None, Style::new().fg(gruvbox::FG1), vec![Span::styled(key, dim)])
            },
        };
        let right_width: usize = right.iter().map(|span| span.content.width()).sum();
        let lead_width = lead.as_ref().map_or(0, |lead| lead.content.width() + 1);
        let room = width.saturating_sub(2 + lead_width + right_width + 1);
        let (name, named) = sidebar::lit(&row.name, room, Keep::Start, &row.hits, name_style);
        let mut spans = vec![Span::raw(" ")];
        if let Some(lead) = lead {
            spans.push(lead);
            spans.push(Span::raw(" "));
        }
        spans.extend(name);
        let gap = width.saturating_sub(1 + lead_width + named + right_width + 1);
        if matches!(&row.key, Key::Worktree { on, place } if self.is_shown(on, place)) {
            let here = Span::styled(" here", Style::new().fg(ACCENT));
            let rule = gap.saturating_sub(1 + here.width() + 1);
            spans.push(Span::raw(" "));
            spans.push(Span::styled("─".repeat(rule), Style::new().fg(ACCENT)));
            spans.push(here);
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.extend(right);
        spans.push(Span::raw(" "));
        Line::from(spans)
    }

    fn tagged(&self) -> bool {
        self.listing.shelves.len() > 1
    }

    fn tag(&self, on: &Machine, away: Option<&'static str>) -> Vec<Span<'static>> {
        if !self.tagged() {
            return Vec::new();
        }
        let machine = self.machine_name(on);
        let tag = match away {
            Some(word) => Span::styled(format!("{machine} · {word}"), Style::new().fg(ERROR)),
            None => Span::styled(machine, Style::new().add_modifier(Modifier::DIM)),
        };
        vec![tag, Span::raw("  ")]
    }

    fn scoped_empty(&self, (on, project): &(Machine, ProjectId)) -> String {
        let shelved = self.listing.shelves.iter().filter(|shelved| shelved.on == *on);
        let mut housed = shelved.flat_map(|shelved| &shelved.projects);
        let Some(housed) = housed.find(|housed| housed.project.id() == *project) else {
            return "lunetad keeps that project no more".to_owned();
        };
        let name = housed.project.listed().name();
        match &housed.unopened {
            Unopened::Folder => projects::folder_has_one_worktree(name),
            Unopened::Unlisted(why) => unlisted(name, why),
            Unopened::Listed(_) => "every worktree is open".to_owned(),
        }
    }

    fn empty(&self) -> String {
        if !self.term.is_empty() {
            return format!("no match for \"{}\"", self.term);
        }
        match self.list {
            List::Actions => "no action fits here".to_owned(),
            List::Worktrees if let Some(scope) = &self.scope => self.scoped_empty(scope),
            List::Worktrees if self.listing.away.is_some() => "no worktrees".to_owned(),
            List::Worktrees => "lunetad keeps no worktree yet".to_owned(),
            List::Agents if self.listing.away.is_some() => "no agents".to_owned(),
            List::Agents if self.yours().is_some() => "no other agents".to_owned(),
            List::Agents => "no agents running".to_owned(),
            List::Dirs => {
                let visited = self.machines().into_iter().filter_map(|on| self.visited.get(&on));
                let visited: Vec<&Visited> = visited.collect();
                if visited.iter().any(|visited| matches!(visited, Visited::Asking(_))) {
                    "asking zoxide…".to_owned()
                } else if visited.iter().all(|visited| matches!(visited, Visited::Failed(_))) {
                    "no directories".to_owned()
                } else {
                    "zoxide knows nowhere yet".to_owned()
                }
            },
        }
    }

    fn draw_preview(&self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        let inner = usize::from(area.width.saturating_sub(2));
        let grey = Style::new().fg(gruvbox::GREY);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Some(row) = self.rows().selected() else {
            let mut lines = vec![Line::styled(" nothing highlighted", grey)];
            let makes = |(_, housed): (_, &Housed)| {
                housed.unopened.refuses_new_worktree(&housed.project).is_none()
            };
            let offers = self.list == List::Worktrees && self.scope.is_none();
            if offers && self.here_project().is_some_and(makes) {
                lines.push(Line::default());
                lines.push(Line::styled(" type a branch to make its worktree", grey));
            }
            framed(buffer, area, "Preview", "", lines);
            return;
        };
        let said = |caption: Vec<Span<'static>>, said: &str| {
            let mut lines = vec![Line::from(caption), Line::default()];
            lines.extend(menu::wrap(said, inner, grey));
            lines
        };
        let (right, caption, looked) = match &row.key {
            Key::Dir { on, path, .. } | Key::Unopened { on, folder: path, .. } => {
                self.draw_contents(buffer, area, &row.name, on, path);
                return;
            },
            Key::New { project, branch, .. } => {
                let project = project.listed().name();
                let words = format!(
                    "no worktree matches: Enter makes the worktree of branch {branch} in \
                     {project}, as n w does, and goes to it. A branch git has is checked out, \
                     and any other is made from the default branch",
                );
                let lines = said(vec![Span::styled(" new", grey)], &words);
                framed(buffer, area, &row.name, "", lines);
                return;
            },
            Key::Project { on, project, lone } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                caption.push(Span::styled(lone.tag(), grey));
                let name = project.listed().name();
                let words = match lone {
                    Lone::Folder(folder) => format!("Enter starts a worktree in {folder}"),
                    Lone::Clone => format!(
                        "{name} is a clone with no worktree: Enter asks for a branch and makes \
                         its worktree, as n w does",
                    ),
                    Lone::Closed => format!(
                        "{name} has no worktree open: Enter lists its worktrees not open, as o w \
                         does",
                    ),
                    Lone::Unlisted(why) => unlisted(name, why),
                };
                framed(buffer, area, &row.name, "", said(caption, &words));
                return;
            },
            Key::Machine { on } => {
                let words = match self.out_of_reach(on) {
                    Some(why) => format!("{why}, and heard of no project there before"),
                    None => format!("lunetad on {} keeps no project", self.machine_name(on)),
                };
                framed(buffer, area, &row.name, "", said(Vec::new(), &words));
                return;
            },
            Key::Worktree { on, place } => {
                let worktree = self.row_worktree(on, place);
                let count = worktree.terminals.len();
                let right =
                    if count == 1 { "1 terminal".to_owned() } else { format!("{count} terminals") };
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                let room = inner.saturating_sub(1 + caption.iter().map(Span::width).sum::<usize>());
                caption.push(Span::styled(sidebar::tail(worktree.folder.as_str(), room), grey));
                let Some(focus) = self.focus(on, worktree) else {
                    let lines = said(caption, "it holds no terminal to look at");
                    framed(buffer, area, &row.name, &right, lines);
                    return;
                };
                let looked = (on.clone(), place.project.listed().clone(), Some(focus));
                (right, Line::from(caption), looked)
            },
            Key::Action(..) => {
                unreachable!("actions are listed in the Actions list, which previews none")
            },
            Key::Agent { on, place } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                if let Some(seated) = self.seated(on, place) {
                    let status = if seated.mark == Mark::Waiting {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(gruvbox::FG1)
                    };
                    let age = seated.since.map(|since| sidebar::age(since, now));
                    caption.extend([
                        Span::styled(seated.status.clone(), status),
                        Span::styled(" · ", dim),
                        Span::styled(age.unwrap_or_default(), dim),
                        Span::styled(" · ", dim),
                        Span::styled(seated.dir.clone(), grey),
                    ]);
                }
                let looked = (on.clone(), place.session.clone(), Some(place.view));
                (String::new(), Line::from(caption), looked)
            },
        };
        let peek = self.peeks.get(&looked);
        let mut lines = vec![caption, Line::default()];
        match peek {
            None | Some(Peek::Reading) => lines.push(Line::styled(" reading…", grey)),
            Some(Peek::Failed(why)) => {
                lines.extend(menu::wrap(why, inner, Style::new().fg(ERROR)));
            },
            Some(Peek::Ready { shot, .. }) if shot.is_blank() => {
                lines.push(Line::styled(" nothing on this screen", grey));
            },
            Some(Peek::Ready { .. }) => {},
        }
        framed(buffer, area, &row.name, &right, lines);
        if let Some(Peek::Ready { shot, .. }) = peek
            && !shot.is_blank()
        {
            let shown = under_caption(area);
            let top = shot.end.saturating_sub(usize::from(shown.height)).max(shot.first);
            shot.show(buffer, shown, top);
        }
    }

    fn draw_contents(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        name: &str,
        on: &Machine,
        path: &Directory,
    ) {
        let inner = usize::from(area.width.saturating_sub(2));
        let grey = Style::new().fg(gruvbox::GREY);
        let mut caption = vec![Span::raw(" ")];
        caption.extend(machined(on, inner));
        let room = inner.saturating_sub(1 + caption.iter().map(Span::width).sum::<usize>());
        caption.push(Span::styled(sidebar::tail(path.as_str(), room), grey));
        let mut lines = vec![Line::from(caption), Line::default()];
        let contents = self.contents.get(&(on.clone(), path.clone()));
        let mut right = String::new();
        match contents {
            None | Some(Contents::Reading) => lines.push(Line::styled(" reading…", grey)),
            Some(Contents::Failed(why)) => {
                lines.extend(menu::wrap(why, inner, Style::new().fg(ERROR)));
            },
            Some(Contents::Ready { total, .. }) => {
                right = if *total == 1 { "1 item".to_owned() } else { format!("{total} items") };
                if *total == 0 {
                    lines.push(Line::styled(" empty", grey));
                }
            },
        }
        framed(buffer, area, name, &right, lines);
        if let Some(Contents::Ready { shot, .. }) = contents
            && !shot.is_blank()
        {
            shot.show(buffer, under_caption(area), shot.first);
        }
    }

    fn prompt(&self, width: usize) -> Line<'static> {
        let action = match self.rows().selected() {
            Some(row) => match &row.key {
                Key::Worktree { on, place } if self.is_shown(on, place) => {
                    Ok("you are in it".to_owned())
                },
                Key::Worktree { on, .. } => Ok(there("Go", on)),
                Key::Project { on, lone: Lone::Folder(_), .. } => {
                    Ok(there("Start a worktree", on))
                },
                Key::Project { on, lone: Lone::Clone, .. } => Ok(there("New worktree", on)),
                Key::Project { on, lone: Lone::Closed, .. } => Ok(there("Open a worktree", on)),
                Key::Project { project, lone: Lone::Unlisted(why), .. } => {
                    Err(unlisted(project.listed().name(), why))
                },
                Key::Unopened { on, .. } => Ok(there("Open", on)),
                Key::Machine { on } => Err(self.out_of_reach(on).unwrap_or_else(|| {
                    format!("{} keeps no project", self.machine_name(on))
                })),
                Key::Agent { on, .. } => Ok(there("Go to agent", on)),
                Key::Dir { on, session, .. } => Ok(there(&format!("Make \"{session}\""), on)),
                Key::New { project, branch, .. } => {
                    let project = project.listed().name();
                    Ok(format!("New worktree \"{branch}\" in {project}"))
                },
                Key::Action(..) => Ok("Run".to_owned()),
            },
            None if self.list == List::Worktrees
                && self.scope.is_none()
                && !self.term.is_empty() =>
            {
                match self.new_key() {
                    Some(Err(why)) => Err(why),
                    Some(Ok(_)) | None => Ok(String::new()),
                }
            },
            None => Ok(String::new()),
        };
        typed(&self.term, action, width)
    }

    fn draw_help(&self, buffer: &mut Buffer, help: Option<Rect>) -> Spots<Pick> {
        let Some(help) = help else {
            return Spots::default();
        };
        let (keys, spots) = menu::keyed(usize::from(help.width), &self.keys());
        buffer.set_line(help.x, help.y, &keys, help.width);
        spots.map(Pick::Key).placed(help)
    }

    fn keys(&self) -> Vec<(Hint, &'static str)> {
        match (&self.stage, self.list) {
            (Stage::Asking { .. } | Stage::Closing { .. }, _) => {
                vec![(Hint::Char('y'), "Yes"), (Hint::AnyOtherKey, "No")]
            },
            (Stage::Planning { .. }, _) => vec![(Hint::Esc, "Give up")],
            (Stage::Rename { .. }, _) => vec![(Hint::Enter, "Rename"), (Hint::Esc, "Cancel")],
            (Stage::Chain { chain, .. }, _) => {
                vec![(Hint::Enter, chain.enter()), (Hint::Esc, "Back")]
            },
            (Stage::Search, List::Actions) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Run"),
                (Hint::Tab, List::Actions.next().title()),
                (Hint::Esc, "Close"),
            ],
            (Stage::Search, List::Worktrees) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Worktrees.next().title()),
                (Hint::Ctrl('r'), "Rename"),
                (Hint::Delete, "Close it"),
                (Hint::Esc, "Close"),
            ],
            (Stage::Search, List::Agents) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Agents.next().title()),
                (Hint::Delete, "Close its view"),
                (Hint::Esc, "Close"),
            ],
            (Stage::Search, List::Dirs) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Dirs.next().title()),
                (Hint::Esc, "Close"),
            ],
        }
    }

    fn draw_rename(
        &self,
        buffer: &mut Buffer,
        layout: &Layout,
        on: &Machine,
        renames: &Renames,
        input: &str,
    ) -> Spots<Pick> {
        if let Some(full) = layout.full {
            let inner = usize::from(full.width.saturating_sub(2));
            let mut lines = vec![Line::default()];
            let mut what = there(&format!("renaming \"{}\"", renames.name()), on);
            match renames {
                Renames::Worktree { place, .. } if self.is_shown(on, place) => {
                    what.push_str(", the worktree you are in");
                },
                Renames::Worktree { .. } => {},
                Renames::Project(_) => what.push_str(", a project"),
            }
            lines.extend(menu::wrap(&what, inner, Style::new().fg(gruvbox::GREY)));
            framed(buffer, full, "Rename", "", lines);
        }
        let width = usize::from(layout.input.width.saturating_sub(2));
        let action = self.rename_to(on, renames, input).map(|_| there("Rename", on));
        draw_input(buffer, layout, "Rename", typed(input, action, width));
        self.draw_help(buffer, layout.help)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Exact,
    Prefix,
    Fuzzy,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Action,
    Place,
}

fn titled(action: Action) -> String {
    let name = action.name();
    let mut chars = name.chars();
    chars.next().map_or_default(|first| first.to_uppercase().chain(chars).collect())
}

fn tier(finder: &Finder<'_>, term: &str, text: &str) -> Option<(Tier, Fit, Vec<usize>)> {
    let (fit, hits) = finder.find(text, &[])?;
    Some((tier_of(term, text), fit, hits))
}

fn tier_of(term: &str, text: &str) -> Tier {
    let lower = text.to_lowercase();
    let words = lower.match_indices([' ', '/', '-']).map(|(at, _)| at + 1);
    let starts = std::iter::once(0).chain(words).any(|at| lower[at..].starts_with(term));
    if lower == term || lower.rsplit(" › ").next() == Some(term) {
        Tier::Exact
    } else if starts {
        Tier::Prefix
    } else {
        Tier::Fuzzy
    }
}

fn found_action(
    finder: &Finder<'_>,
    term: &str,
    action: Action,
    run: Run,
) -> Option<(Tier, Fit, Row)> {
    let Spec { keywords, .. } = action.spec();
    let title = titled(action);
    let key = Key::Action(action, run);
    let row = |name, hits| Row { key: key.clone(), name, hits, rules: Rules::default() };
    let named = tier(finder, term, &title);
    let keyword = keywords.iter().filter_map(|it| Some((tier(finder, term, it)?, *it)));
    let keyword = keyword.min_by_key(|((tier, fit, _), _)| (*tier, Reverse(*fit)));
    match (named, keyword) {
        (Some((by_name, fit, hits)), Some(((by_keyword, ..), _))) if by_name <= by_keyword => {
            Some((by_name, fit, row(title, hits)))
        },
        (Some((by_name, fit, hits)), None) => Some((by_name, fit, row(title, hits))),
        (_, Some(((by_keyword, fit, hits), keyword))) => {
            let lead = format!("{title} (");
            let shift = lead.chars().count();
            let hits = hits.into_iter().map(|hit| hit + shift).collect();
            Some((by_keyword, fit, row(format!("{lead}{keyword})"), hits)))
        },
        (None, None) => None,
    }
}

fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

fn agent_name(agents: &[Seated], seated: &Seated) -> String {
    let alike = agents.iter().filter(|other| other.label == seated.label);
    let sessions: HashSet<&SessionName> =
        alike.clone().map(|other| other.place.session.name()).collect();
    let (session, view) = (seated.place.session.name(), seated.place.view);
    match (alike.count(), sessions.len()) {
        (0 | 1, _) => seated.label.clone(),
        (_, 1) => format!("{}:{view}", seated.label),
        _ => format!("{} ({session}:{view})", seated.label),
    }
}

fn grouped(into: &mut Vec<Row>, mut rows: Vec<Row>, rule: Option<String>) -> Option<usize> {
    let first = rows.first_mut()?;
    first.rules.machine = rule;
    let at = into.len();
    into.extend(rows);
    Some(at)
}

fn unopened_of<'a>(on: &'a Machine, housed: &'a Housed) -> impl Iterator<Item = Found<'a>> {
    let project = housed.project.listed().name().as_str();
    housed.unopened.folders().iter().map(move |unopened| {
        let name = format!("{project} › {}", unopened.last());
        let folder = sidebar::short_cwd(unopened.as_str());
        let (row, at) = (housed.project.clone(), unopened.into());
        let key = Key::Unopened { on: on.clone(), project: row, folder: at };
        (None, name, Some(UNOPENED), folder, key)
    })
}

fn candidates_of<'a>(
    found: &'a [Found<'a>],
) -> impl Iterator<Item = (String, Vec<Option<&'a str>>, Key)> {
    found.iter().map(|(_, name, branch, folder, key)| {
        (name.clone(), vec![*branch, Some(folder.as_str())], key.clone())
    })
}

fn matched<'a>(
    finder: Option<&Finder<'_>>,
    candidates: impl Iterator<Item = (String, Vec<Option<&'a str>>, Key)>,
) -> Vec<Row> {
    let Some(finder) = finder else {
        let row = |(name, _, key)| Row { key, name, hits: Vec::new(), rules: Rules::default() };
        return candidates.map(row).collect();
    };
    let mut found: Vec<(Fit, Row)> = Vec::new();
    for (name, also, key) in candidates {
        if let Some((fit, hits)) = finder.find(&name, &also) {
            found.push((fit, Row { key, name, hits, rules: Rules::default() }));
        }
    }
    found.sort_by_key(|(fit, _)| Reverse(*fit));
    found.into_iter().map(|(_, row)| row).collect()
}

fn dir_rows<'a>(
    matcher: &SkimMatcherV2,
    term: &str,
    on: &Machine,
    dirs: impl Iterator<Item = (&'a Dir, SessionName)>,
) -> Vec<Row> {
    let mut found: Vec<(bool, i64, usize, Row)> = Vec::new();
    for (at, (dir, session)) in dirs.enumerate() {
        let name = session.to_string();
        let hit = if term.is_empty() {
            Some((0, Vec::new()))
        } else {
            matcher.fuzzy_indices(dir.path.as_str(), term).or_else(|| {
                let score = matcher.fuzzy_match(&name, term)?;
                Some((score, Vec::new()))
            })
        };
        if let Some((score, hits)) = hit {
            let exact = name == term || dir.name.as_str() == term;
            let key = Key::Dir { on: on.clone(), path: dir.path.clone(), session };
            found.push((exact, score, at, Row { key, name, hits, rules: Rules::default() }));
        }
    }
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    found.into_iter().map(|(.., row)| row).collect()
}

fn dir_line(row: &Row, path: &str, width: usize) -> Line<'static> {
    let name = sidebar::cut(&row.name, width.saturating_sub(4 + path.width()).max(width / 3));
    let room = width.saturating_sub(4 + name.width());
    let grey = Style::new().fg(gruvbox::GREY);
    let (shown, wide) = sidebar::lit(path, room, Keep::End, &row.hits, grey);
    let gap = width.saturating_sub(1 + name.width() + wide + 1);
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" "), Span::styled(name, bold), Span::raw(" ".repeat(gap))];
    spans.extend(shown);
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn unlisted(name: impl fmt::Display, why: &str) -> String {
    format!("{name}'s worktrees: {why}")
}

fn there(action: &str, on: &Machine) -> String {
    match on {
        Machine::Local => action.to_owned(),
        Machine::Remote(destination) => format!("{action} on {destination}"),
    }
}

fn machined(on: &Machine, width: usize) -> Vec<Span<'static>> {
    let Machine::Remote(destination) = on else {
        return Vec::new();
    };
    let machine = sidebar::cut(&destination.to_string(), width / 2);
    let dim = Style::new().add_modifier(Modifier::DIM);
    vec![Span::styled(machine, Style::new().fg(NAME)), Span::styled(" · ", dim)]
}

fn here_line(name: &str, width: usize) -> Line<'static> {
    let name = sidebar::cut(name, width.saturating_sub(8));
    let rule = width.saturating_sub(1 + name.width() + 1 + 1 + 4 + 1);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(name, Style::new().fg(NAME).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().fg(ACCENT)),
        Span::raw(" "),
        Span::styled("here", Style::new().fg(ACCENT)),
        Span::raw(" "),
    ])
}

fn machine_rule(name: &str, width: usize) -> Line<'static> {
    let name = sidebar::cut(name, width.saturating_sub(4));
    let rule = width.saturating_sub(2 + name.width() + 1);
    let dim = Style::new().add_modifier(Modifier::DIM);
    Line::from(vec![
        Span::styled("─ ", dim),
        Span::styled(name, Style::new().fg(gruvbox::FG1).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), dim),
    ])
}

fn staged(seen: Seen) -> (&'static str, ratatui::style::Color) {
    match seen {
        Seen::Missing => ("missing", ERROR),
        Seen::Unread => ("…", gruvbox::GREY),
        Seen::Read(projects::Stage::NoBranch) => ("no branch", gruvbox::GREY),
        Seen::Read(projects::Stage::Working) => ("working", gruvbox::BLUE),
        Seen::Read(projects::Stage::PullRequestOpen) => ("PR open", gruvbox::AQUA),
        Seen::Read(projects::Stage::ChecksFailing) => ("checks failing", gruvbox::RED),
        Seen::Read(projects::Stage::Merged) => ("merged", gruvbox::PURPLE),
    }
}

fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = (selected + 1).saturating_sub(visible);
    (start, (start + visible).min(total))
}

fn under_caption(area: Rect) -> Rect {
    Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), area.height.saturating_sub(4))
}

fn anchored(
    notes: Vec<Line<'static>>,
    body: Vec<Line<'static>>,
    rows: usize,
) -> (Vec<Line<'static>>, usize) {
    let notes: Vec<Line<'static>> = notes.into_iter().take(rows).collect();
    let body: Vec<Line<'static>> = body.into_iter().take(rows - notes.len()).collect();
    let blank = notes.len() + body.len()..rows;
    let mut lines: Vec<Line<'static>> = blank.map(|_| Line::default()).collect();
    lines.extend(notes);
    let body_at = lines.len();
    lines.extend(body);
    (lines, body_at)
}

fn count(at: Option<usize>, total: usize) -> String {
    match at {
        Some(at) if total > 0 => format!("{}/{total}", at + 1),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

fn typed(input: &str, action: Result<String, String>, width: usize) -> Line<'static> {
    let mut spans = sidebar::query(input, width).spans;
    let used: usize = spans.iter().map(|span| span.content.width()).sum();
    let (action, style) = match action {
        Ok(action) if !action.is_empty() => (format!("<ENTER> {action}"), Style::new().fg(NAME)),
        Ok(_) => (String::new(), Style::new()),
        Err(why) => (why, Style::new().fg(ERROR)),
    };
    let room = width.saturating_sub(used + 2);
    if !action.is_empty() && room >= MIN_ACTION {
        let action = sidebar::cut(&action, room);
        spans.push(Span::raw(" ".repeat(width.saturating_sub(used + action.width() + 1))));
        spans.push(Span::styled(action, style));
    }
    Line::from(spans)
}

fn framed(buffer: &mut Buffer, area: Rect, title: &str, right: &str, mut lines: Vec<Line<'_>>) {
    let inner = usize::from(area.width.saturating_sub(2));
    lines.resize(usize::from(area.height.saturating_sub(2)), Line::default());
    menu::boxed(buffer, area, &menu::rule(title, right, inner), &lines);
}

fn draw_input(buffer: &mut Buffer, layout: &Layout, title: &str, prompt: Line<'static>) {
    let input = layout.input;
    if layout.bordered {
        framed(buffer, input, title, "", vec![prompt]);
    } else {
        buffer.set_line(input.x, input.y, &prompt, input.width);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    results: Option<Rect>,
    preview: Option<Rect>,
    full: Option<Rect>,
    input: Rect,
    bordered: bool,
    help: Option<Rect>,
}

impl Layout {
    fn of(area: Rect) -> Layout {
        let help = (area.height > INPUT_HEIGHT)
            .then(|| Rect::new(area.x, area.bottom() - 1, area.width, 1));
        let bordered = area.height >= INPUT_HEIGHT;
        let height = if bordered { INPUT_HEIGHT } else { area.height.min(1) };
        let input_y = area.bottom().saturating_sub(u16::from(help.is_some()) + height);
        let input = Rect::new(area.x, input_y, area.width, height);
        let above = input_y - area.y;
        let full = (above >= MIN_RESULTS).then(|| Rect::new(area.x, area.y, area.width, above));
        let split = full.filter(|full| full.width >= MIN_HALF * 2).map(|full| {
            let left = full.width / 2;
            let right = Rect { x: full.x + left, width: full.width - left, ..full };
            (Rect { width: left, ..full }, right)
        });
        let (results, preview) = match split {
            Some((results, preview)) => (Some(results), Some(preview)),
            None => (full, None),
        };
        Layout { results, preview, full, input, bordered, help }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use lunetad::projects::WorktreeId;
    use ratatui::layout::Position;

    use super::*;
    use crate::close::Doing;
    use crate::menu::{Cwd, Ran, ViewRow, WorktreeRow};
    use crate::spots::Hit;

    const NOW: Duration = Duration::from_secs(1000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    fn listed(name: &str, id: u64) -> Listed {
        Listed::new_project(name, id)
    }

    fn seated(name: &str, started: u64, view: u32, label: &str, mark: Mark) -> Seated {
        Seated {
            place: ViewPlace { session: listed(name, started), view: ViewKey::terminal(view) },
            label: label.to_owned(),
            mark,
            status: "idle".to_owned(),
            since: Some(now() - Duration::from_mins(1)),
            dir: "luneta/main".to_owned(),
            pid: view,
            transcript: None,
            doing: crate::alerts::Doing::Idle,
        }
    }

    fn here(session: &str) -> Here {
        Here::Session { on: Machine::Local, session: session.to_owned() }
    }

    fn agents(agents: Vec<Seated>) -> Runs {
        Runs { agents }
    }

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn visited(path: &str, name: &str) -> Dir {
        Dir { path: directory(path), name: SessionName::parse(name).unwrap() }
    }

    fn heard_here(visited: Result<Vec<Dir>, String>) -> Heard {
        Heard::Dirs { on: Machine::Local, ask: 1, visited }
    }

    fn project(name: &str, id: u64) -> ProjectRow {
        ProjectRow::new(name, id)
    }

    fn place(project: &str, id: u64, worktree: u64) -> WorktreePlace {
        let worktree = WorktreeId::of(id * 10 + worktree).unwrap();
        WorktreePlace { project: self::project(project, id), worktree }
    }

    fn worktree(
        place: WorktreePlace,
        name: &str,
        used: Option<u64>,
        terminals: &[u32],
    ) -> Worktree {
        Worktree {
            place,
            name: name.to_owned(),
            folder: projects::Dir::parse(&format!("/home/you/{name}")).unwrap(),
            branch: None,
            seen: Seen::Read(projects::Stage::Working),
            used: used.map(|at| Used::read(at * 1_000).unwrap()),
            terminals: terminals.iter().map(|id| ViewKey::terminal(*id)).collect(),
            held: held(terminals),
            waiting: 0,
            ending: Ok(Vec::new()),
        }
    }

    fn held(terminals: &[u32]) -> Vec<ViewId> {
        terminals.iter().map(|id| ViewId::of(u64::from(*id)).unwrap()).collect()
    }

    fn housed(name: &str, id: u64, worktrees: Vec<Worktree>) -> Housed {
        let dir = projects::Dir::parse(&format!("/home/you/{name}")).unwrap();
        let unopened = Unopened::Listed(Vec::new());
        Housed { project: project(name, id), dir, worktrees, unopened }
    }

    fn shelved(on: Machine, projects: Vec<Housed>) -> Shelved {
        Shelved { on, away: None, projects }
    }

    fn fix() -> WorktreePlace {
        place("luneta", 1, 2)
    }

    fn api() -> WorktreePlace {
        place("web", 3, 1)
    }

    fn main_place() -> WorktreePlace {
        place("luneta", 1, 1)
    }

    fn luneta() -> Housed {
        housed("luneta", 1, vec![
            worktree(main_place(), "main", Some(30), &[1, 2]),
            worktree(fix(), "fix", Some(50), &[5, 3]),
            worktree(place("luneta", 1, 3), "docs", None, &[]),
        ])
    }

    fn works() -> Listing {
        let projects = vec![
            luneta(),
            housed("notes", 2, Vec::new()),
            housed("web", 3, vec![worktree(api(), "api", Some(40), &[4])]),
        ];
        let at = ViewPlace { session: fix().project.listed().clone(), view: ViewKey::terminal(3) };
        Listing {
            here: Some(Here::View { on: Machine::Local, place: at }),
            shown: Some((Machine::Local, fix())),
            shelves: vec![shelved(Machine::Local, projects)],
            ..Listing::default()
        }
    }

    fn at_host() -> Palette {
        let target = Rc::new(Target::Machine(Machine::Local));
        Palette { aim: Aim::At { target, worktree: None }, origin: Origin::Views }
    }

    fn at_fix(origin: Origin) -> Palette {
        let worktree = match origin {
            Origin::Views => Some(Rc::new(fix_worktree())),
            Origin::Sidebar => None,
        };
        Palette { aim: Aim::At { target: Rc::new(fix_view()), worktree }, origin }
    }

    fn fix_worktree() -> Target {
        let row = WorktreeRow {
            place: fix(),
            name: "fix".to_owned(),
            folder: directory("/home/you/fix"),
            branch: None,
            repository: None,
        };
        Target::Worktree(Machine::Local, row)
    }

    fn fix_view() -> Target {
        let row = ViewRow {
            session: fix().project.listed().clone(),
            view: ViewKey::terminal(3),
            terminal: ViewId::of(3),
            cwd: Cwd::At(directory("/home/you/fix")),
            branch: None,
            repository: None,
            pull_request: None,
            ran: Ran::Unhooked,
        };
        Target::View(Machine::Local, row)
    }

    fn palette_on(listing: Listing, palette: Palette) -> Picker {
        let mut picker = Picker::open(1, List::Actions, listing, Machine::Local, palette);
        picker.asks();
        picker
    }

    fn palette(origin: Origin) -> Picker {
        palette_on(works(), at_fix(origin))
    }

    fn shown_actions(picker: &Picker) -> (Vec<String>, Option<String>) {
        let buffer = drawn(picker);
        let area = Picker::compact(Rect::new(0, 0, 200, 50));
        let inside = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
        let results = Layout::of(inside).full.unwrap();
        let results = results.inner(Margin::new(1, 1));
        let lines = text(&buffer, results);
        let lit = (results.y..results.bottom()).position(|y| buffer[(results.x, y)].bg == LIT);
        let lit = lit.map(|at| lines[at].clone());
        (lines.into_iter().filter(|line| !line.is_empty()).collect(), lit)
    }

    fn keyed_action(picker: &Picker) -> (String, String) {
        let lit = shown_actions(picker).1.unwrap_or_default();
        let key = lit.rsplit("  ").next().unwrap_or("").trim().to_owned();
        (named(&lit), key)
    }

    fn every_action(picker: &mut Picker) -> Vec<String> {
        let mut every = vec![keyed_action(picker).0];
        for _ in 0..80 {
            press(picker, KeyCode::Down);
            let (lit, _) = keyed_action(picker);
            if every.last() == Some(&lit) {
                return every;
            }
            every.push(lit);
        }
        panic!("the palette's rows never end: {every:?}");
    }

    fn super_k(picker: &mut Picker) -> Picked {
        picker.key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::SUPER))
    }

    fn opened(listing: Listing) -> Picker {
        let mut picker = Picker::open(1, List::Worktrees, listing, Machine::Local, at_host());
        picker.asks();
        picker
    }

    fn press(picker: &mut Picker, code: KeyCode) -> Picked {
        picker.key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn control(picker: &mut Picker, c: char) -> Picked {
        picker.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn typed(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            press(picker, KeyCode::Char(c));
        }
    }

    fn retyped(picker: &mut Picker, text: &str) {
        for _ in 0..20 {
            press(picker, KeyCode::Backspace);
        }
        typed(picker, text);
    }

    fn drawn(picker: &Picker) -> Buffer {
        let over = Rect::new(0, 0, 200, 50);
        let mut buffer = Buffer::empty(over);
        picker.draw(&mut buffer, over, now());
        buffer
    }

    fn text(buffer: &Buffer, area: Rect) -> Vec<String> {
        let line = |y| (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>();
        (area.y..area.bottom()).map(|y| line(y).trim().to_owned()).collect()
    }

    fn parts() -> (Rect, Rect) {
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        let inner = |rect: Rect| Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
        let layout = Layout::of(inner(area));
        (inner(layout.results.unwrap()), inner(layout.preview.unwrap()))
    }

    fn list(picker: &Picker) -> (Vec<String>, Option<String>) {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lines = text(&buffer, results);
        let lit = (results.y..results.bottom()).position(|y| buffer[(results.x, y)].bg == LIT);
        let lit = lit.map(|at| lines[at].clone());
        (lines.into_iter().filter(|line| !line.is_empty()).collect(), lit)
    }

    fn found(picker: &Picker) -> Vec<(String, String)> {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lit = |y| -> String {
            let cells = (results.x..results.right()).map(|x| &buffer[(x, y)]);
            cells.filter(|cell| cell.fg == ACCENT).map(ratatui::buffer::Cell::symbol).collect()
        };
        let lines = (results.y..results.bottom()).zip(text(&buffer, results));
        lines.map(|(y, line)| (line, lit(y))).filter(|(line, _)| !line.is_empty()).collect()
    }

    fn named(line: &str) -> String {
        let name = line.split("  ").next().unwrap_or("");
        name.split(" ─").next().unwrap_or("").trim().to_owned()
    }

    fn rows(picker: &Picker) -> Vec<String> {
        list(picker).0.iter().map(|line| named(line)).collect()
    }

    fn names(picker: &Picker) -> Vec<String> {
        let (lines, _) = list(picker);
        lines.iter().map(|line| line.split(' ').next().unwrap_or("").to_owned()).collect()
    }

    fn grouped(picker: &Picker) -> Vec<String> {
        let (lines, _) = list(picker);
        let word = |line: &String| match line.strip_prefix("─ ") {
            Some(rule) => format!("── {}", rule.split(' ').next().unwrap_or("")),
            None => line.split(' ').next().unwrap_or("").to_owned(),
        };
        lines.iter().map(word).collect()
    }

    fn nixos() -> Machine {
        Machine::Remote("nixos".parse().unwrap())
    }

    fn two_machines() -> Listing {
        on_nixos(Runs::default())
    }

    fn on_nixos(runs: Runs) -> Listing {
        let remotes = vec![("nixos".parse().unwrap(), runs)];
        Listing { this: "desk".to_owned(), remotes, ..Listing::default() }
    }

    fn said(picker: &Picker) -> String {
        let buffer = drawn(picker);
        text(&buffer, buffer.area).join(" ")
    }

    fn lit(picker: &Picker) -> String {
        let (_, lit) = list(picker);
        named(&lit.unwrap_or_default())
    }

    fn go(on: Machine, place: WorktreePlace) -> Picked {
        Picked::Go { on, place: Place::Worktree(place) }
    }

    fn down_to(picker: &mut Picker, row: &str) {
        for _ in 0..10 {
            if lit(picker) == row {
                return;
            }
            press(picker, KeyCode::Down);
        }
        panic!("no row {row}: {:?}", rows(picker));
    }

    #[test]
    fn the_worktrees_go_by_their_last_use_and_an_empty_project_is_one_row() {
        let mut picker = Picker::open(1, List::Worktrees, works(), Machine::Local, at_host());
        let on = Machine::Local;
        assert_eq!(picker.asks(), [Ask::Dirs { on, ask: 1 }]);
        let shown = ["luneta › fix", "web › api", "luneta › main", "luneta › docs", "notes"];
        assert_eq!(rows(&picker), shown);
        let (lines, _) = list(&picker);
        assert!(lines[0].contains("─ here") && lines[0].ends_with("working"), "{lines:?}");
        assert!(lines[4].ends_with("no worktree"), "{lines:?}");
        assert_eq!(lit(&picker), "web › api");
        assert!(said(&picker).contains("<ENTER> Go"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), go(Machine::Local, api()));
        press(&mut picker, KeyCode::Up);
        assert!(said(&picker).contains("you are in it"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        down_to(&mut picker, "notes");

        let mut later = works();
        let shelf = &mut later.shelves[0].projects[0].worktrees;
        shelf[0].used = Some(Used::read(60_000).unwrap());
        picker.listed(later);
        assert_eq!(rows(&picker)[..3], ["luneta › main", "luneta › fix", "web › api"]);
        assert_eq!(lit(&picker), "notes");
    }

    #[test]
    fn a_project_with_no_worktree_starts_its_folder_asks_its_clone_a_branch_or_says_why_not() {
        let alone = |unopened: Unopened| {
            let mut listing = works();
            listing.shelves[0].projects[1].unopened = unopened;
            let mut picker = opened(listing);
            down_to(&mut picker, "notes");
            picker
        };
        let preview = |picker: &Picker| text(&drawn(picker), parts().1).join(" ");
        let tagged = |picker: &Picker, tag: &str| list(picker).1.unwrap().ends_with(tag);

        let mut folder = alone(Unopened::Folder);
        assert!(tagged(&folder, "no worktree"), "{:?}", list(&folder));
        assert!(said(&folder).contains("<ENTER> Start a worktree"), "{}", said(&folder));
        let shown = preview(&folder);
        assert!(shown.contains("Enter starts a worktree in /home/you/notes"), "{shown}");
        let at = directory("/home/you/notes");
        let added = Picked::Add { on: Machine::Local, project: project("notes", 2), folder: at };
        assert_eq!(press(&mut folder, KeyCode::Enter), added);

        let mut clone = alone(Unopened::Listed(Vec::new()));
        assert!(tagged(&clone, "no worktree"), "{:?}", list(&clone));
        assert!(said(&clone).contains("<ENTER> New worktree"), "{}", said(&clone));
        assert!(!said(&clone).contains("Start a worktree"), "{}", said(&clone));
        let words = "notes is a clone with no worktree: Enter asks for a branch and makes its \
                     worktree, as n w does";
        assert!(preview(&clone).contains(words), "{}", preview(&clone));
        assert_eq!(press(&mut clone, KeyCode::Enter), Picked::Nothing, "no worktree at its root");
        let asked = said(&clone);
        assert!(asked.contains("─ New worktree ─"), "{asked}");
        assert!(asked.contains("─ notes ─"), "the clone is named over the input: {asked}");
        let mut renamed = works();
        renamed.shelves[0].projects[1] = housed("jots", 2, Vec::new());
        clone.listed(renamed);
        let asked = said(&clone);
        assert!(asked.contains("─ jots ─"), "its name follows the listing: {asked}");
        assert_eq!(press(&mut clone, KeyCode::Esc), Picked::Nothing);
        assert_eq!(lit(&clone), "jots", "Esc goes back to the row");
        press(&mut clone, KeyCode::Enter);
        typed(&mut clone, "fix/notes");
        let branch = Branch::parse("fix/notes").unwrap();
        let done = Done::Worktree(Machine::Local, project("notes", 2).id(), branch);
        let made = Picked::Done { done, origin: Origin::Views };
        assert_eq!(press(&mut clone, KeyCode::Enter), made);

        let mut unlisted = alone(Unopened::Unlisted("/home/you/notes is missing".to_owned()));
        assert!(tagged(&unlisted, "not listed"), "{:?}", list(&unlisted));
        let why = "notes's worktrees: /home/you/notes is missing";
        assert!(said(&unlisted).contains(why), "{}", said(&unlisted));
        assert!(preview(&unlisted).contains(why), "{}", preview(&unlisted));
        let promised = said(&unlisted).contains("Start a worktree");
        assert!(!promised && !preview(&unlisted).contains("starts a worktree"), "no start");
        assert_eq!(press(&mut unlisted, KeyCode::Enter), Picked::Nothing);
        assert!(!said(&unlisted).contains("─ New worktree ─"), "{}", said(&unlisted));
    }

    fn with_unopened(mut listing: Listing) -> Listing {
        let folder = |at: &str| projects::Dir::parse(at).unwrap();
        let projects = &mut listing.shelves[0].projects;
        let luneta = |it: &str| folder(&format!("/home/you/luneta/{it}"));
        projects[0].unopened = Unopened::Listed(vec![luneta("old"), luneta("spike")]);
        projects[2].unopened = Unopened::Listed(vec![folder("/home/you/web/legacy")]);
        listing
    }

    #[test]
    fn super_j_lists_the_open_worktrees_alone_and_a_project_with_none_open_once() {
        let mut picker = opened(with_unopened(works()));
        let open = ["luneta › fix", "web › api", "luneta › main", "luneta › docs", "notes"];
        assert_eq!(rows(&picker), open, "no worktree that is not open is listed");
        typed(&mut picker, "spike");
        assert_eq!(rows(&picker), ["+ spike"], "only n w's row, which opens spike as n w does");

        let mut listing = with_unopened(works());
        let jots = projects::Dir::parse("/home/you/notes/jots").unwrap();
        listing.shelves[0].projects[1].unopened = Unopened::Listed(vec![jots]);
        let mut picker = opened(listing);
        assert_eq!(rows(&picker), open, "notes is one row, not its worktree");
        down_to(&mut picker, "notes");
        assert!(list(&picker).1.unwrap().ends_with("none open"), "{:?}", list(&picker));
        assert!(said(&picker).contains("<ENTER> Open a worktree"), "{}", said(&picker));
        let shown = text(&drawn(&picker), parts().1).join(" ");
        let words = "notes has no worktree open: Enter lists its worktrees not open, as o w does";
        assert!(shown.contains(words), "{shown}");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert_eq!(rows(&picker), ["notes › jots"], "Enter lists notes's as o w does");
        assert!(said(&picker).contains("<ENTER> Open"), "{}", said(&picker));
        let opens = Picked::Add {
            on: Machine::Local,
            project: project("notes", 2),
            folder: directory("/home/you/notes/jots"),
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), opens);
    }

    #[test]
    fn o_w_lists_the_worktrees_of_its_project_alone_that_are_not_open() {
        let luneta = project("luneta", 1).id();
        let listing = with_unopened(works());
        let mut picker = Picker::opening(1, listing, (Machine::Local, luneta), at_host());
        assert_eq!(rows(&picker), ["luneta › old", "luneta › spike"]);
        typed(&mut picker, "zzz");
        assert_eq!(rows(&picker), ["no match for \"zzz\""], "no worktree is made of what is typed");
        typed(&mut picker, "#3");
        let unbranched = "names an issue or a pull request";
        assert!(!said(&picker).contains(unbranched), "no branch is asked for: {}", said(&picker));
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(!shown.contains("type a branch"), "o w makes no worktree: {shown}");
        let mut all_open = works();
        let web = with_unopened(works()).shelves[0].projects[2].unopened.clone();
        all_open.shelves[0].projects[2].unopened = web;
        let picker = Picker::opening(2, all_open, (Machine::Local, luneta), at_host());
        assert_eq!(rows(&picker), ["every worktree is open"]);
        let said_of = |number: u64, unopened: Unopened| {
            let mut listing = works();
            listing.shelves[0].projects[0].unopened = unopened;
            let picker = Picker::opening(number, listing, (Machine::Local, luneta), at_host());
            rows(&picker)
        };
        let unlisted = Unopened::Unlisted("not listed yet".to_owned());
        let said = "luneta's worktrees: not listed yet";
        assert_eq!(said_of(3, unlisted), [said], "a clone not listed yet is not said all open");
        let said = "luneta is a folder, and its own is its one worktree";
        assert_eq!(said_of(4, Unopened::Folder), [said]);
    }

    #[test]
    fn o_w_lists_no_other_machine_even_one_that_keeps_no_project() {
        let mut listing = machines();
        let old = projects::Dir::parse("/home/you/luneta/old").unwrap();
        listing.shelves[0].projects[0].unopened = Unopened::Listed(vec![old]);
        let luneta = project("luneta", 1).id();
        let picker = Picker::opening(1, listing, (Machine::Local, luneta), at_host());
        assert_eq!(rows(&picker), ["luneta › old"]);
    }

    fn clicks(picker: &Picker, key: &str) -> (Vec<(String, Option<Pick>)>, Option<Pick>) {
        let over = Rect::new(0, 0, 200, 50);
        let mut buffer = Buffer::empty(over);
        let drawn = picker.draw(&mut buffer, over, now());
        assert_eq!(drawn.boxed(), Some(Picker::area(over)));
        let pick = |at| {
            drawn.at(at).map(|hit| match hit {
                Hit::Picker(number, pick) if number == picker.number() => pick,
                hit => panic!("{hit:?} is not the picker's"),
            })
        };
        let (results, _) = parts();
        let lines = (results.y..results.bottom()).zip(text(&buffer, results));
        let rows = lines.filter(|(_, line)| !line.is_empty()).map(|(y, line)| {
            let word = match line.strip_prefix("─ ") {
                Some(rule) => format!("── {}", rule.split(' ').next().unwrap_or("")),
                None => named(&line),
            };
            (word, pick(Position::new(results.x + 2, y)))
        });
        let help = (over.top()..over.bottom()).rev().find_map(|y| {
            let cells = (over.left()..over.right()).map(|x| buffer[(x, y)].symbol());
            let line: String = cells.collect();
            let at = line.find(key)?;
            Some(Position::new(u16::try_from(line[..at].chars().count()).ok()?, y))
        });
        let spot = help.and_then(pick);
        (rows.collect(), spot)
    }

    #[test]
    fn a_click_on_a_row_does_what_enter_does_on_it() {
        let mut picker = opened(works());
        let (rows, tab) = clicks(&picker, "<TAB>");
        assert!(rows.len() == 5 && rows.iter().all(|(_, pick)| pick.is_some()), "{rows:?}");
        let row = |word: &str| {
            let shown = rows.iter().find(|(it, _)| it == word);
            shown.and_then(|(_, pick)| pick.clone()).expect("a row")
        };
        assert_eq!(tab, Some(Pick::Key(KeyEvent::from(KeyCode::Tab))));
        assert_eq!(clicks(&picker, "<↓↑>").1, None);
        let main = go(Machine::Local, main_place());
        assert_eq!(picker.click(row("luneta › main")), main);
        assert_eq!(picker.click(row("luneta › fix")), Picked::Close);
        typed(&mut picker, "api");
        assert_eq!(picker.click(row("luneta › main")), Picked::Nothing);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        assert_eq!(picker.click(row("luneta › main")), main);
        control(&mut picker, 'r');
        assert_eq!(picker.click(row("web › api")), Picked::Nothing);
        let rename = clicks(&picker, "<ENTER>").1;
        assert_eq!(rename, Some(Pick::Key(KeyEvent::from(KeyCode::Enter))));
    }

    fn busy(mut listing: Listing) -> Listing {
        let shelf = &mut listing.shelves[0].projects[0].worktrees;
        shelf[0].ending = Ok(vec![Doing::Busy, Doing::Idle]);
        shelf[2].ending = Ok(vec![Doing::Unknown]);
        listing
    }

    #[test]
    fn no_row_is_a_spot_while_a_delete_waits_or_asks() {
        let mut picker = opened(busy(works()));
        down_to(&mut picker, "luneta › main");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let (rows, yes) = clicks(&picker, "<y>");
        assert!(rows.len() > 1 && rows.iter().all(|(_, pick)| pick.is_none()), "{rows:?}");
        assert_eq!(yes, Some(Pick::Key(KeyEvent::from(KeyCode::Char('y')))));
        press(&mut picker, KeyCode::Esc);

        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        assert!(matches!(press(&mut picker, KeyCode::Delete), Picked::Plan { .. }));
        let (rows, give_up) = clicks(&picker, "<ESC>");
        assert!(!rows.is_empty() && rows.iter().all(|(_, pick)| pick.is_none()), "{rows:?}");
        assert_eq!(give_up, Some(Pick::Key(KeyEvent::from(KeyCode::Esc))));
    }

    #[test]
    fn what_is_typed_narrows_the_list_and_enter_makes_the_worktree_of_a_branch_no_row_matches() {
        let mut picker = opened(works());
        typed(&mut picker, "api");
        assert_eq!(rows(&picker), ["web › api"]);
        retyped(&mut picker, "luneta");
        assert_eq!(rows(&picker), ["luneta › fix", "luneta › main", "luneta › docs"]);
        assert_eq!(lit(&picker), "luneta › fix");
        retyped(&mut picker, "zzz");
        let (lines, lit) = list(&picker);
        let new = lines[0].starts_with("+ zzz ") && lines[0].ends_with(" new worktree");
        assert!(new, "{lines:?}");
        assert_eq!(lit.as_ref(), lines.first());
        let words = r#"<ENTER> New worktree "zzz" in luneta"#;
        assert!(said(&picker).contains(words), "{}", said(&picker));
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(shown.contains("the worktree of branch zzz in luneta, as n w does"), "{shown}");
        let branch = Branch::parse("zzz").unwrap();
        let luneta = project("luneta", 1).id();
        let done = Done::Worktree(Machine::Local, luneta, branch);
        let made = Picked::Done { done, origin: Origin::Views };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        typed(&mut picker, "#3");
        assert_eq!(list(&picker).0, [r#"no match for "zzz#3""#]);
        let numbered = "zzz#3 names an issue or a pull request: paste its link";
        assert!(said(&picker).contains(numbered), "{}", said(&picker));
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(shown.contains("type a branch to make its worktree"), "{shown}");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        let mut folder = works();
        folder.shelves[0].projects[0].unopened = Unopened::Folder;
        let mut picker = opened(folder);
        typed(&mut picker, "zzz");
        assert_eq!(list(&picker).0, [r#"no match for "zzz""#]);
        let refused = projects::folder_has_one_worktree("luneta");
        assert!(said(&picker).contains(&refused), "{}", said(&picker));
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(!shown.contains("type a branch"), "a folder makes no worktree: {shown}");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        let mut picker = opened(Listing { shown: None, ..works() });
        typed(&mut picker, "zzz");
        assert_eq!(list(&picker).0, [r#"no match for "zzz""#]);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        let titled = |picker: &Picker, title: &str| {
            let buffer = drawn(picker);
            let (results, _) = parts();
            let rule = Rect::new(results.x - 1, results.y - 1, results.width + 2, 1);
            text(&buffer, rule)[0].starts_with(&format!("┌─ {title} ─"))
        };
        assert!(titled(&picker, "Worktrees"));
        for title in ["Agents", "Directories"] {
            press(&mut picker, KeyCode::Tab);
            assert!(titled(&picker, title), "{title}");
            assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        }
        for title in ["Agents", "Worktrees"] {
            press(&mut picker, KeyCode::BackTab);
            assert!(titled(&picker, title), "{title}");
        }
        press(&mut picker, KeyCode::BackTab);
        assert!(said(&picker).contains("─ Actions"), "{}", said(&picker));
        press(&mut picker, KeyCode::BackTab);
        assert!(titled(&picker, "Directories"));
    }

    #[test]
    fn del_closes_a_worktree_and_asks_first_where_an_agent_at_work_or_unread_files_end() {
        let mut picker = opened(busy(works()));
        assert_eq!(lit(&picker), "web › api");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let close = |place, name: &str, terminals: &[u32]| {
            Ask::Close { on: Machine::Local, place, name: name.to_owned(), seen: held(terminals) }
        };
        assert_eq!(picker.asks(), [close(api(), "api", &[4])]);

        down_to(&mut picker, "luneta › main");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        let asks = "Close worktree main? 2 agents, 1 busy";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
        press(&mut picker, KeyCode::Char('n'));
        assert!(!said(&picker).contains(asks), "{}", said(&picker));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        press(&mut picker, KeyCode::Delete);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [close(main_place(), "main", &[1, 2])]);
        let closed = Err(Unchanged::Refused("no layout holds the view 3".to_owned()));
        picker.heard(Heard::Closed { name: "main".to_owned(), closed });
        let stays = "main stays: no layout holds the view 3";
        assert!(said(&picker).contains(stays), "{}", said(&picker));
        let late = Err(Unchanged::InDoubt("lunetad did not answer in 15s".to_owned()));
        picker.heard(Heard::Closed { name: "main".to_owned(), closed: late });
        let maybe = "lunetad did not answer in 15s, so main may be closed";
        assert!(said(&picker).contains(maybe), "{}", said(&picker));
        assert!(!said(&picker).contains("stays"), "{}", said(&picker));

        down_to(&mut picker, "luneta › docs");
        press(&mut picker, KeyCode::Delete);
        let asks = "Close worktree docs? 1 agent, 1 unknown";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
        let mut gone = busy(works());
        gone.shelves[0].projects[0].worktrees.pop();
        picker.listed(gone);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert!(said(&picker).contains("docs is listed no more"), "{}", said(&picker));

        down_to(&mut picker, "notes");
        press(&mut picker, KeyCode::Delete);
        assert!(said(&picker).contains("notes holds no worktree to close"), "{}", said(&picker));

        let mut untold = works();
        untold.shelves[0].projects[2].worktrees[0].ending = Err(Untold::Away);
        let mut picker = opened(untold);
        press(&mut picker, KeyCode::Delete);
        let asks = "Close worktree api? luneta cannot tell which agents";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
    }

    #[test]
    fn a_yes_to_closing_a_worktree_asks_again_when_what_would_end_changed_since() {
        let mut picker = opened(busy(works()));
        down_to(&mut picker, "luneta › main");
        press(&mut picker, KeyCode::Delete);
        let busy_asks = "Close worktree main? 2 agents, 1 busy";
        assert!(said(&picker).contains(busy_asks), "{}", said(&picker));
        let mut untold = busy(works());
        let shelf = &mut untold.shelves[0].projects[0].worktrees;
        let main = shelf.iter_mut().find(|worktree| worktree.name == "main").unwrap();
        main.ending = Err(Untold::Away);
        picker.listed(untold);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        let untold_asks = "Close worktree main? luneta cannot tell which agents";
        assert!(said(&picker).contains(untold_asks), "{}", said(&picker));
        let mut grown = busy(works());
        let shelf = &mut grown.shelves[0].projects[0].worktrees;
        let main = shelf.iter_mut().find(|worktree| worktree.name == "main").unwrap();
        (main.ending, main.held) = (Err(Untold::Away), held(&[2, 9, 1]));
        picker.listed(grown);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [] as [Ask; 0], "a terminal came since, as the question did not");
        assert!(said(&picker).contains(untold_asks), "{}", said(&picker));
        press(&mut picker, KeyCode::Char('y'));
        let (place, name, seen) = (main_place(), "main".to_owned(), held(&[1, 2, 9]));
        assert_eq!(picker.asks(), [Ask::Close { on: Machine::Local, place, name, seen }]);
    }

    #[test]
    fn ctrl_r_renames_the_highlighted_worktree_or_empty_project() {
        let mut picker = opened(works());
        control(&mut picker, 'r');
        assert!(said(&picker).contains(r#"renaming "api""#), "{}", said(&picker));
        let refused = [("", "a name is empty"), ("api", "already called that")];
        for (to, why) in refused {
            retyped(&mut picker, to);
            press(&mut picker, KeyCode::Enter);
            assert!(picker.asks().is_empty(), "{to}");
            assert!(said(&picker).contains(why), "{to}: {}", said(&picker));
        }
        retyped(&mut picker, "gateway");
        assert!(said(&picker).contains("<ENTER> Rename"), "{}", said(&picker));
        press(&mut picker, KeyCode::Enter);
        let renames = Renames::Worktree { place: api(), name: "api".to_owned() };
        let to = Name::parse("gateway").unwrap();
        assert_eq!(picker.asks(), [Ask::Rename { on: Machine::Local, renames, to: to.clone() }]);
        assert!(!said(&picker).contains("renaming"), "{}", said(&picker));
        let refused = Err(Unchanged::Refused("lunetad: no".to_owned()));
        let heard = |renamed| Heard::Renamed { from: "api".to_owned(), to: to.clone(), renamed };
        picker.heard(heard(refused));
        assert!(said(&picker).contains("api is not called gateway: lunetad: no"));
        picker.heard(heard(Err(Unchanged::InDoubt("it dropped".to_owned()))));
        let maybe = "it dropped, so api may be called gateway";
        assert!(said(&picker).contains(maybe), "{}", said(&picker));
        assert!(!said(&picker).contains("not called"), "{}", said(&picker));

        press(&mut picker, KeyCode::Up);
        control(&mut picker, 'r');
        let yours = r#"renaming "fix", the worktree you are in"#;
        assert!(said(&picker).contains(yours), "{}", said(&picker));
        press(&mut picker, KeyCode::Esc);
        down_to(&mut picker, "notes");
        control(&mut picker, 'r');
        assert!(said(&picker).contains(r#"renaming "notes", a project"#), "{}", said(&picker));
        typed(&mut picker, "jots/old");
        press(&mut picker, KeyCode::Enter);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert!(said(&picker).contains("names no project"), "{}", said(&picker));
        retyped(&mut picker, "jots");
        press(&mut picker, KeyCode::Enter);
        let renames = Renames::Project(project("notes", 2));
        let to = Name::parse("jots").unwrap();
        assert_eq!(picker.asks(), [Ask::Rename { on: Machine::Local, renames, to }]);

        press(&mut picker, KeyCode::Up);
        control(&mut picker, 'r');
        typed(&mut picker, "docs-2");
        let mut gone = works();
        gone.shelves[0].projects[0].worktrees.pop();
        picker.listed(gone);
        press(&mut picker, KeyCode::Enter);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert!(said(&picker).contains("docs is listed no more"), "{}", said(&picker));
    }

    fn machines() -> Listing {
        let mut far = shelved(Machine::Remote("far".parse().unwrap()), vec![housed(
            "old",
            5,
            vec![worktree(place("old", 5, 1), "stale", Some(10), &[7])],
        )]);
        far.away = Some("unreachable");
        let gateway = worktree(place("api", 4, 1), "gateway", Some(60), &[6]);
        let shelves = vec![
            shelved(Machine::Local, vec![luneta()]),
            shelved(nixos(), vec![housed("api", 4, vec![gateway])]),
            far,
            shelved(Machine::Remote("mini".parse().unwrap()), Vec::new()),
        ];
        let mini = ("mini".parse().unwrap(), Runs::default());
        let remotes = vec![("nixos".parse().unwrap(), Runs::default()), mini];
        let (shown, this) = (Some((Machine::Local, fix())), "desk".to_owned());
        Listing { shown, remotes, shelves, this, ..Listing::default() }
    }

    #[test]
    fn every_machine_s_worktrees_are_listed_an_unreachable_one_s_greyed() {
        let mut picker = opened(machines());
        let shown = [
            "api › gateway",
            "luneta › fix",
            "luneta › main",
            "old › stale",
            "luneta › docs",
            "mini",
        ];
        assert_eq!(rows(&picker), shown);
        let (lines, _) = list(&picker);
        assert!(lines[0].ends_with("nixos  working"), "{lines:?}");
        assert!(lines[1].contains("desk  working"), "{lines:?}");
        assert!(lines[3].ends_with("far · unreachable  working"), "{lines:?}");
        assert!(lines[5].ends_with("no projects"), "{lines:?}");
        let buffer = drawn(&picker);
        let (results, _) = parts();
        let shown = text(&buffer, results);
        let dimmed = |row: &str| {
            let y = shown.iter().position(|line| line.starts_with(row)).expect("a row");
            let y = results.y + u16::try_from(y).unwrap();
            buffer[(results.x + 1, y)].modifier.contains(Modifier::DIM)
        };
        assert!(dimmed("old › stale") && !dimmed("luneta › main"));
        assert_eq!(lit(&picker), "api › gateway");
        assert!(said(&picker).contains("<ENTER> Go on nixos"), "{}", said(&picker));
        let gateway = place("api", 4, 1);
        assert_eq!(press(&mut picker, KeyCode::Enter), go(nixos(), gateway.clone()));
        press(&mut picker, KeyCode::Delete);
        let (name, seen) = ("gateway".to_owned(), held(&[6]));
        let close = Ask::Close { on: nixos(), place: gateway.clone(), name, seen };
        assert_eq!(picker.asks(), [close]);
        control(&mut picker, 'r');
        assert!(said(&picker).contains(r#"renaming "gateway" on nixos"#), "{}", said(&picker));
        typed(&mut picker, "gw");
        press(&mut picker, KeyCode::Enter);
        let renames = Renames::Worktree { place: gateway, name: "gateway".to_owned() };
        let to = Name::parse("gw").unwrap();
        assert_eq!(picker.asks(), [Ask::Rename { on: nixos(), renames, to }]);

        down_to(&mut picker, "old › stale");
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(press(&mut picker, KeyCode::Enter), go(far, place("old", 5, 1)));
        press(&mut picker, KeyCode::Delete);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        let why = "stale stays: lunetad does not reach far now";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        control(&mut picker, 'r');
        assert!(!said(&picker).contains("renaming"), "{}", said(&picker));
        let why = "stale keeps its name: lunetad does not reach far now";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        down_to(&mut picker, "mini");
        assert!(said(&picker).contains("mini keeps no project"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        typed(&mut picker, "a");
        assert!(!rows(&picker).contains(&"mini".to_owned()), "{:?}", rows(&picker));
    }

    #[test]
    fn the_preview_shows_the_terminal_luneta_is_on_else_the_worktree_s_first() {
        let mut picker = opened(works());
        let peek = |session: Listed, view| Ask::Peek {
            on: Machine::Local,
            session,
            asked: Some(ViewKey::terminal(view)),
        };
        let due = picker.due().unwrap();
        picker.ticked(due.checked_sub(Duration::from_millis(1)).unwrap());
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        picker.ticked(due);
        let web = || api().project.listed().clone();
        assert_eq!(picker.asks(), [peek(web(), 4)]);
        assert_eq!(picker.due(), None);
        press(&mut picker, KeyCode::Up);
        picker.ticked(picker.due().unwrap());
        let luneta = || fix().project.listed().clone();
        assert_eq!(picker.asks(), [peek(luneta(), 3)]);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [peek(luneta(), 1)]);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.due(), None);
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview);
        assert_eq!(shown[..3], ["/home/you/docs", "", "it holds no terminal to look at"]);
    }

    #[test]
    fn the_preview_shows_the_last_lines_of_the_screen_inside_its_box() {
        let mut picker = opened(works());
        let dumped = (1..=40).map(|n| format!("\x1b[mline {n}")).collect::<Vec<_>>().join("\n");
        let peek = Ok((4, format!("{dumped}\n\n\n")));
        let (session, asked) = (api().project.listed().clone(), Some(ViewKey::terminal(4)));
        picker.heard(Heard::Peek { on: Machine::Local, session, asked, peek });
        let buffer = drawn(&picker);
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        assert_eq!((area.x, area.y, area.width, area.height), (40, 10, 120, 30));
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(buffer[(area.right() - 1, area.bottom() - 1)].symbol(), "┘");
        let (_, preview) = parts();
        let lines = text(&buffer, preview);
        assert_eq!(lines[0], "/home/you/api");
        let rule = text(&buffer, Rect { y: preview.y - 1, height: 1, ..preview });
        assert!(rule[0].starts_with("─ web › api ─") && rule[0].ends_with(" 1 terminal ─"));
        assert_eq!(lines.last().map(String::as_str), Some("line 40"));
        assert_eq!(lines[2], format!("line {}", 40 - (lines.len() - 3)));
        for y in preview.y..preview.bottom() {
            assert_eq!(buffer[(preview.x - 1, y)].symbol(), "│");
            assert_eq!(buffer[(preview.right(), y)].symbol(), "│");
        }
    }

    #[test]
    fn the_preview_of_another_machine_s_worktree_is_its_screen_there() {
        let mut picker = opened(machines());
        picker.ticked(picker.due().unwrap());
        let session = place("api", 4, 1).project.listed().clone();
        let asked = Some(ViewKey::terminal(6));
        assert_eq!(picker.asks(), [Ask::Peek { on: nixos(), session: session.clone(), asked }]);
        let peek = |on, session, screen: &str| {
            let peek = Ok((6, screen.to_owned()));
            Heard::Peek { on, session, asked, peek }
        };
        let shown = |picker: &Picker| text(&drawn(picker), parts().1);
        picker.heard(peek(Machine::Local, session.clone(), "here"));
        assert_eq!(shown(&picker)[..3], ["nixos · /home/you/gateway", "", "reading…"]);
        picker.heard(peek(nixos(), session, "there"));
        assert_eq!(shown(&picker)[..3], ["nixos · /home/you/gateway", "", "there"]);
    }

    #[test]
    fn a_preview_that_failed_is_asked_for_again_as_the_highlight_comes_back() {
        let mut picker = opened(works());
        picker.ticked(picker.due().unwrap());
        let (session, asked) = (api().project.listed().clone(), Some(ViewKey::terminal(4)));
        let peek = || Ask::Peek { on: Machine::Local, session: session.clone(), asked };
        assert_eq!(picker.asks(), [peek()]);
        let failed = Err("luneta-mux did not answer".to_owned());
        let on = Machine::Local;
        picker.heard(Heard::Peek { on, session: session.clone(), asked, peek: failed });
        assert_eq!(picker.due(), None);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Up);
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [peek()]);

        let path = || directory("/home/you/luneta");
        control(&mut picker, 'd');
        picker.heard(heard_here(Ok(vec![visited("/home/you/luneta", "luneta")])));
        picker.ticked(picker.due().unwrap());
        let list = || Ask::List { on: Machine::Local, path: path() };
        assert_eq!(picker.asks(), [list()]);
        let failed = Err("Permission denied".to_owned());
        picker.heard(Heard::Listed { on: Machine::Local, path: path(), listed: failed });
        assert_eq!(picker.due(), None);
        picker.key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SUPER));
        control(&mut picker, 'd');
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [list()]);
    }

    #[test]
    fn the_keys_that_open_each_list_go_to_it_and_ctrl_j_goes_to_none() {
        let none = Listing::default();
        let mut picker = Picker::open(1, List::Agents, none, Machine::Local, at_host());
        control(&mut picker, 'j');
        assert_eq!(picker.list, List::Agents, "Ctrl j is the views' Down");
        picker.key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SUPER));
        assert_eq!(picker.list, List::Worktrees);
        control(&mut picker, 'd');
        assert_eq!(picker.list, List::Dirs);
        control(&mut picker, 'b');
        assert_eq!(picker.list, List::Agents);
        assert_eq!(picker.term, "", "no key typed into the search");
    }

    #[test]
    fn del_on_an_agent_asks_before_ending_it_at_work_and_goes_ahead_on_yes_to_the_same_plan() {
        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        picker.asks();
        let c = listed("c", 30);
        let deletes = || Deletes::View(ViewKey::terminal(4));
        let planning = Picked::Plan { on: Machine::Local, session: c.clone(), deletes: deletes() };
        let act = || Act::View { session: c.name().clone(), view: ViewKey::terminal(4) };
        let plan = |doing: Doing| Plan::of(act(), Ok(&[(4, doing)]));
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Busy))), Picked::Nothing);
        let asks = |picker: &Picker| said(picker).contains("Close the view? 1 agent, 1 busy");
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('n')), Picked::Nothing);
        assert!(!asks(&picker));

        press(&mut picker, KeyCode::Delete);
        picker.planned(Ok(plan(Doing::Busy)));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), Picked::Nothing);
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        let ended = Picked::Delete { on: Machine::Local, act: act() };
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), ended);

        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Idle))), ended);
    }

    #[test]
    fn del_on_an_agent_plans_closing_its_view_and_asks_nothing_of_another_thread() {
        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        picker.asks();
        let deletes = Deletes::View(ViewKey::terminal(4));
        let plan = Picked::Plan { on: Machine::Local, session: listed("c", 30), deletes };
        assert_eq!(press(&mut picker, KeyCode::Delete), plan);
        assert_eq!(picker.asks(), []);
    }

    #[test]
    fn agents_named_alike_are_told_apart_and_yours_is_pinned_over_them() {
        let place = ViewPlace { session: listed("work", 20), view: ViewKey::terminal(9) };
        let listing = Listing {
            here: Some(Here::View { on: Machine::Local, place }),
            local: agents(vec![
                seated("work", 20, 9, "mine", Mark::Busy),
                seated("work", 20, 1, "fix", Mark::Waiting),
                seated("work", 20, 2, "fix", Mark::Idle),
                seated("work", 20, 3, "docs", Mark::Idle),
                seated("review", 10, 4, "docs", Mark::Idle),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        let (lines, lit) = list(&picker);
        assert!(lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"), "{lines:?}");
        let name = |line: &String| {
            let rest: String = line.chars().skip(2).collect();
            rest.split("  ").next().unwrap_or("").to_owned()
        };
        let named: Vec<String> = lines[1..].iter().map(name).collect();
        assert_eq!(named, ["fix:1", "fix:2", "docs (work:3)", "docs (review:4)"]);
        assert!(lit.is_some_and(|lit| lit.contains("fix:1")));
        typed(&mut picker, "review");
        let rows = found(&picker);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let (line, lit) = &rows[1];
        assert!(line.contains("docs (review:4)") && lit == "review", "{rows:?}");
    }

    #[test]
    fn a_search_weighs_an_agent_by_its_name_or_its_directory_whichever_fits_best() {
        let at = |view, label, dir: &str| Seated {
            dir: dir.to_owned(),
            ..seated("work", 20, view, label, Mark::Idle)
        };
        let listing = Listing {
            local: agents(vec![
                at(1, "Zero-length lines in jobs", "src/mux"),
                at(2, "fix", "src/mux"),
                at(3, "mux panics", "misc/luneta"),
                at(4, "docs", "luneta/main"),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        typed(&mut picker, "mux");
        let rows = found(&picker);
        let shown = [("mux panics", "mux"), ("Zero-length lines in jobs", ""), ("fix", "")];
        assert_eq!(rows.len(), shown.len(), "{rows:?}");
        for ((line, lit), (label, lights)) in rows.iter().zip(shown) {
            assert!(line.contains(label) && lit == lights, "{rows:?}");
        }
    }

    #[test]
    fn each_directory_is_named_for_the_session_enter_makes_there() {
        let projects = vec![housed("notes", 1, Vec::new()), housed("luneta", 2, Vec::new())];
        let listing = Listing {
            here: Some(here("notes")),
            shelves: vec![shelved(Machine::Local, projects)],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local, at_host());
        assert_eq!(list(&picker).0, ["asking zoxide…"]);
        picker.heard(heard_here(Ok(vec![
            visited("/home/you/luneta", "luneta"),
            visited("/home/you/luneta/main", "luneta-main"),
            visited("/home/you/Work/bipa", "bipa"),
            visited("/home/you/notes", "notes"),
        ])));
        assert_eq!(names(&picker), ["luneta-2", "luneta-main", "bipa", "notes-2"]);
        let (lines, _) = list(&picker);
        assert!(lines[0].ends_with(" /home/you/luneta"), "{lines:?}");
        assert_eq!(lit(&picker), "luneta-2");
        let buffer = drawn(&picker);
        let said = text(&buffer, buffer.area).join(" ");
        assert!(said.contains(r#"<ENTER> Make "luneta-2""#), "{said}");
        let session = SessionName::parse("luneta-2").unwrap();
        let cwd = directory("/home/you/luneta");
        let made = Picked::Make { on: Machine::Local, session, cwd };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        picker.asks();
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
    }

    #[test]
    fn no_session_takes_the_name_of_the_one_luneta_is_on_its_way_to() {
        let listing = Listing {
            here: Some(here("notes")),
            shelves: vec![shelved(Machine::Local, vec![housed("luneta", 1, Vec::new())])],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![
            visited("/home/you/notes", "notes"),
            visited("/home/you/luneta", "luneta"),
        ])));
        assert_eq!(names(&picker), ["notes-2", "luneta-2"]);
    }

    #[test]
    fn a_search_finds_a_directory_by_its_path_or_its_name() {
        let far = "/home/you/Projects/misc/far/away/down/luneta/main";
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![
            visited(far, "luneta-main"),
            visited("/home/you/luneta", "luneta"),
            visited("/srv/app", "app"),
        ])));
        typed(&mut picker, "luneta");
        assert_eq!(names(&picker), ["luneta", "luneta-main"]);
        typed(&mut picker, "-m");
        assert_eq!(names(&picker), ["luneta-main"]);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        typed(&mut picker, "/m");
        let buffer = drawn(&picker);
        let (results, _) = parts();
        let y = (results.y..results.bottom()).find(|y| buffer[(results.x, *y)].bg == LIT).unwrap();
        let cells: Vec<_> = (results.x..results.right()).map(|x| &buffer[(x, y)]).collect();
        let row: String = cells.iter().map(|cell| cell.symbol()).collect();
        let lit: String =
            cells.iter().filter(|cell| cell.fg == ACCENT).map(|cell| cell.symbol()).collect();
        assert!(row.trim_end().ends_with("…u/Projects/misc/far/away/down/luneta/main"), "{row}");
        assert_eq!(lit, "luneta/m");
    }

    #[test]
    fn the_preview_lists_what_eza_says_is_in_the_directory() {
        let path = "/home/you/luneta";
        let mut picker = opened(works());
        control(&mut picker, 'd');
        picker.heard(heard_here(Ok(vec![visited(path, "luneta")])));
        picker.asks();
        let due = picker.due().unwrap();
        picker.ticked(due);
        assert_eq!(picker.asks(), vec![Ask::List { on: Machine::Local, path: directory(path) }]);
        let preview = |picker: &Picker| {
            let buffer = drawn(picker);
            let (_, preview) = parts();
            (text(&buffer, preview), text(&buffer, buffer.area).join(" "))
        };
        assert_eq!(preview(&picker).0[..3], [path, "", "reading…"]);
        let listed = Ok("\x1b[34msrc\x1b[0m/\nCargo.toml\nREADME.md\n".to_owned());
        picker.heard(Heard::Listed { on: Machine::Local, path: directory(path), listed });
        let (lines, said) = preview(&picker);
        assert_eq!(lines[..5], [path, "", "src/", "Cargo.toml", "README.md"]);
        assert!(said.contains("3 items"), "{said}");
        let listed = Err("Permission denied".to_owned());
        picker.heard(Heard::Listed { on: Machine::Local, path: directory(path), listed });
        assert_eq!(preview(&picker).0[..3], [path, "", "Permission denied"]);
    }

    #[test]
    fn every_machine_s_directories_go_under_its_rule_this_one_s_first() {
        let mut picker = Picker::open(1, List::Dirs, two_machines(), Machine::Local, at_host());
        let dirs = |on: Machine, ask: u64| Ask::Dirs { on, ask };
        assert_eq!(picker.asks(), [dirs(Machine::Local, 1), dirs(nixos(), 2)]);
        let api = "/home/me/Projects/api";
        let there = vec![visited(api, "api"), visited("/home/me/blog", "blog-2")];
        picker.heard(Heard::Dirs { on: nixos(), ask: 2, visited: Ok(there) });
        let here = vec![visited("/home/you/luneta", "luneta"), visited("/home/you/blog", "blog")];
        picker.heard(heard_here(Ok(here)));
        let rows = ["── desk", "luneta", "blog", "── nixos", "api", "blog-2"];
        assert_eq!(grouped(&picker), rows);
        assert_eq!(lit(&picker), "luneta");

        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "api");
        let buffer = drawn(&picker);
        let said = text(&buffer, buffer.area).join(" ");
        assert!(said.contains(r#"<ENTER> Make "api" on nixos"#), "{said}");
        let (session, cwd) = (SessionName::parse("api").unwrap(), directory(api));
        let made = Picked::Make { on: nixos(), session, cwd };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), vec![Ask::List { on: nixos(), path: directory(api) }]);
        let listed = Ok("src/\nCargo.toml\n".to_owned());
        picker.heard(Heard::Listed { on: nixos(), path: directory(api), listed });
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview);
        assert_eq!(shown[..4], ["nixos · /home/me/Projects/api", "", "src/", "Cargo.toml"]);

        typed(&mut picker, "blog");
        assert_eq!(grouped(&picker), ["── desk", "blog", "── nixos", "blog-2"]);
        assert_eq!(lit(&picker), "blog");
    }

    #[test]
    fn ctrl_d_on_a_machine_s_header_has_the_highlight_on_its_directories() {
        let here = || heard_here(Ok(vec![visited("/home/you/notes", "notes")]));
        let there = || {
            let dirs = vec![visited("/home/me/api", "api"), visited("/home/me/blog", "blog")];
            Heard::Dirs { on: nixos(), ask: 2, visited: Ok(dirs) }
        };
        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos(), at_host());
        picker.heard(here());
        assert_eq!(lit(&picker), "notes");
        picker.heard(there());
        assert_eq!(lit(&picker), "api");
        press(&mut picker, KeyCode::Down);
        picker.listed(two_machines());
        assert_eq!(lit(&picker), "blog");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos(), at_host());
        picker.heard(there());
        picker.heard(here());
        assert_eq!(lit(&picker), "api");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos(), at_host());
        picker.heard(here());
        press(&mut picker, KeyCode::Up);
        picker.heard(there());
        assert_eq!(lit(&picker), "notes");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos(), at_host());
        picker.heard(heard_here(Ok(vec![visited("/home/you/luneta", "luneta")])));
        let dirs = vec![visited("/home/me/api", "api"), visited("/home/me/luneta", "luneta")];
        picker.heard(Heard::Dirs { on: nixos(), ask: 2, visited: Ok(dirs) });
        typed(&mut picker, "lun");
        let session = SessionName::parse("luneta").unwrap();
        let made = Picked::Make { on: nixos(), session, cwd: directory("/home/me/luneta") };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
    }

    #[test]
    fn each_machine_s_zoxide_is_asked_once_and_says_why_it_listed_none() {
        let dirs_asks = |picker: &mut Picker| -> Vec<Ask> {
            picker.asks().into_iter().filter(|ask| matches!(ask, Ask::Dirs { .. })).collect()
        };
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local, at_host());
        assert_eq!(dirs_asks(&mut picker), [Ask::Dirs { on: Machine::Local, ask: 1 }]);
        picker.listed(two_machines());
        assert_eq!(dirs_asks(&mut picker), [Ask::Dirs { on: nixos(), ask: 2 }]);
        picker.listed(two_machines());
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert_eq!(list(&picker).0, ["asking zoxide…"]);
        picker.heard(heard_here(Ok(vec![visited("/home/you/notes", "notes")])));
        let older = "nixos: its lunetad, or this one, is older than what luneta asked of it";
        picker.heard(Heard::Dirs { on: nixos(), ask: 2, visited: Err(older.to_owned()) });
        let (lines, _) = list(&picker);
        assert_eq!(lines[..2].join(" "), older);
        assert_eq!(grouped(&picker)[2..], ["── desk", "notes"]);

        let gone = || Listing { remotes: Vec::new(), ..two_machines() };
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(dirs_asks(&mut picker), [Ask::Dirs { on: nixos(), ask: 3 }]);
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(dirs_asks(&mut picker), [Ask::Dirs { on: nixos(), ask: 4 }]);
        let lost = "nixos: lunetad does not reach nixos now";
        picker.heard(Heard::Dirs { on: nixos(), ask: 3, visited: Err(lost.to_owned()) });
        let there = vec![visited("/home/me/api", "api")];
        picker.heard(Heard::Dirs { on: nixos(), ask: 4, visited: Ok(there) });
        assert_eq!(grouped(&picker), ["── desk", "notes", "── nixos", "api"]);
    }

    #[test]
    fn the_directories_say_why_zoxide_listed_none() {
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local, at_host());
        picker.heard(heard_here(Err("zoxide is not on the PATH".to_owned())));
        assert_eq!(list(&picker).0, ["zoxide is not on the PATH", "no directories"]);
    }

    #[test]
    fn a_project_of_another_machine_takes_no_name_here() {
        let shelves = vec![shelved(nixos(), vec![housed("api", 1, Vec::new())])];
        let listing = Listing { shelves, ..two_machines() };
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![visited("/home/you/api", "api")])));
        assert_eq!(grouped(&picker), ["── desk", "api"]);
    }

    #[test]
    fn luneta_s_agent_on_another_machine_is_pinned_over_that_machine_s_rows() {
        let at = |view| ViewPlace { session: listed("api", 40), view };
        let there = agents(vec![
            seated("api", 40, 9, "mine", Mark::Busy),
            seated("api", 40, 2, "fix", Mark::Idle),
        ]);
        let listing = Listing {
            here: Some(Here::View { on: nixos(), place: at(ViewKey::terminal(9)) }),
            local: agents(vec![seated("api", 30, 1, "fix", Mark::Idle)]),
            ..on_nixos(there)
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        picker.asks();
        let (lines, _) = list(&picker);
        assert!(lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"), "{lines:?}");
        let name = |line: &String| {
            if let Some(rule) = line.strip_prefix("─ ") {
                return format!("── {}", rule.split(' ').next().unwrap_or(""));
            }
            let rest: String = line.chars().skip(2).collect();
            rest.split("  ").next().unwrap_or("").to_owned()
        };
        let named: Vec<String> = lines[1..].iter().map(name).collect();
        assert_eq!(named, ["── desk", "fix", "── nixos", "fix"]);
        press(&mut picker, KeyCode::Down);
        let fix = Picked::Go { on: nixos(), place: Place::View(at(ViewKey::terminal(2))) };
        assert_eq!(press(&mut picker, KeyCode::Enter), fix);
        let deletes = Deletes::View(ViewKey::terminal(2));
        let plan = Picked::Plan { on: nixos(), session: listed("api", 40), deletes };
        assert_eq!(press(&mut picker, KeyCode::Delete), plan);
    }

    fn asked(ask: Ask, projected: &Projected) -> Heard {
        let (events, heard) = std::sync::mpsc::channel();
        super::ask(1, ask, projected, &events);
        match heard.recv_timeout(Duration::from_secs(10)).expect("the picker hears its ask") {
            Event::Picker { number: 1, heard } => heard,
            _ => panic!("the picker hears another event"),
        }
    }

    #[test]
    fn a_peek_at_another_machine_s_view_is_not_done_yet() {
        let projected = Projected::default();
        let session = Listed::new_project("work", 7);
        let far = Machine::Remote("far".parse().unwrap());
        let peek = Ask::Peek { on: far, session, asked: Some(ViewKey::terminal(4)) };
        let peeked = asked(peek, &projected);
        let Heard::Peek { peek, .. } = peeked else {
            panic!("a peek is answered with a screen");
        };
        assert_eq!(peek, Err(crate::host::NOT_YET.to_owned()));
    }

    #[test]
    fn the_palette_lists_the_actions_fitting_its_view_then_the_worktrees_by_their_last_use() {
        let mut picker = palette(Origin::Views);
        let every = every_action(&mut picker);
        let worktrees = every.iter().position(|row| row.contains(" › ")).unwrap();
        let (actions, worktrees) = every.split_at(worktrees);
        let by_use = ["luneta › fix", "web › api", "luneta › main", "luneta › docs", "notes"];
        assert_eq!(worktrees, by_use);
        assert_eq!(actions[..3], ["Search skills", "Ask", "/thermo-nuclear-review"]);
        let fitting = [
            "Rename the view…",
            "Close the view",
            "Rename the worktree…",
            "Close the worktree",
            "New worktree…",
            "Pick a worktree",
        ];
        for fitting in fitting {
            assert!(actions.iter().any(|action| action == fitting), "{fitting}: {actions:?}");
        }
        let unfitting = [
            "Rename the project…",
            "Forget the machine",
            "Search",
            "Hold to dictate",
            "Pick an action",
            "Tackle",
        ];
        for unfitting in unfitting {
            assert!(!actions.iter().any(|action| action == unfitting), "{unfitting}: {actions:?}");
        }
        let mut picker = palette(Origin::Sidebar);
        let every = every_action(&mut picker);
        for fitting in ["Search", "Unread", "Rename the view…"] {
            assert!(every.iter().any(|action| action == fitting), "{fitting}: {every:?}");
        }
        let worktree = "Rename the worktree…";
        assert!(!every.iter().any(|action| action == worktree), "the row alone: {every:?}");
    }

    #[test]
    fn each_action_shows_its_key_as_it_is_typed_where_the_palette_opened() {
        let key = |origin: Origin, query: &str| {
            let mut picker = palette(origin);
            typed(&mut picker, query);
            keyed_action(&picker)
        };
        let views = [
            ("new view", "New view", "Alt a n v"),
            ("search skills", "Search skills", "Alt a /"),
            ("quit", "Quit luneta", "Alt a q"),
            ("pick a worktree", "Pick a worktree", "Super j"),
            ("view left", "View left, or the worktree before", "Ctrl h"),
        ];
        for (query, name, spelled) in views {
            assert_eq!(key(Origin::Views, query), (name.to_owned(), spelled.to_owned()));
        }
        let sidebar = [
            ("new view", "New view", "n v"),
            ("search skills", "Search skills", "m /"),
            ("quit", "Quit luneta", "q"),
            ("unread", "Unread", "u"),
        ];
        for (query, name, spelled) in sidebar {
            assert_eq!(key(Origin::Sidebar, query), (name.to_owned(), spelled.to_owned()));
        }
    }

    #[test]
    fn a_typed_word_ranks_what_it_names_then_what_it_starts_then_loose_matches() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "fix");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        assert_eq!(shown[..3], ["luneta › fix", "Fix the last command", "Fix the failing checks"]);
        retyped(&mut picker, "fix the");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        assert_eq!(shown[..2], ["Fix the last command", "Fix the failing checks"]);
        retyped(&mut picker, "ask");
        assert_eq!(keyed_action(&picker).0, "Ask");
        retyped(&mut picker, "kill");
        let lit = shown_actions(&picker).1.unwrap_or_default();
        assert_eq!(named(&lit), "Close the view (kill)");
        let buffer = drawn(&picker);
        let area = Picker::compact(Rect::new(0, 0, 200, 50));
        let line = |y| text(&buffer, Rect::new(area.x, y, area.width, 1)).remove(0);
        let row = (area.y..area.bottom()).find(|y| line(*y).contains("(kill)"));
        let row = row.expect("the row is drawn");
        let lit: String = (area.x..area.right())
            .map(|x| &buffer[(x, row)])
            .filter(|cell| cell.fg == ACCENT)
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert_eq!(lit, "kill", "the keyword that matched is lit");
    }

    #[test]
    fn a_typed_phrase_ranks_the_action_it_starts_over_a_worktree_it_fits_loosely() {
        let mut listing = works();
        let parser = worktree(place("renew", 4, 1), "parser", Some(10), &[5]);
        listing.shelves[0].projects.push(housed("renew", 4, vec![parser]));
        let mut picker = palette_on(listing, at_fix(Origin::Views));
        typed(&mut picker, "new p");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        assert_eq!(shown[..2], ["New project…", "renew › parser"]);
    }

    #[test]
    fn an_action_that_asks_a_name_asks_it_in_the_palette_and_backspace_goes_back() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "rename");
        assert_eq!(keyed_action(&picker).0, "Rename the view…");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        let asked = said(&picker);
        assert!(asked.contains("─ Rename ─"), "{asked}");
        assert!(asked.contains("its new name; with none, it goes back"), "{asked}");
        let target = "luneta › fix · view 3";
        assert!(asked.contains(target), "the target is named over the input: {asked}");
        let mut renamed = works();
        renamed.shelves[0].projects[0].worktrees[1].name = "fixed".to_owned();
        picker.listed(renamed);
        let asked = said(&picker);
        assert!(asked.contains("luneta › fixed · view 3"), "it follows the listing: {asked}");
        assert_eq!(press(&mut picker, KeyCode::Backspace), Picked::Nothing);
        assert_eq!(keyed_action(&picker).0, "Rename the view…", "back on the list, as typed");
        press(&mut picker, KeyCode::Enter);
        typed(&mut picker, "logs");
        press(&mut picker, KeyCode::Esc);
        assert_eq!(keyed_action(&picker).0, "Rename the view…", "Esc goes back too");
        press(&mut picker, KeyCode::Enter);
        typed(&mut picker, "logs");
        let logs = Some(Name::parse("logs").unwrap());
        let rename = menu::Rename::Terminal(ViewId::of(3).unwrap(), logs);
        let done = Done::Rename(Machine::Local, rename);
        let origin = Origin::Views;
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Done { done, origin });
        press(&mut picker, KeyCode::Enter);
        assert_eq!(super_k(&mut picker), Picked::Close, "the key again closes it, chained or not");
    }

    #[test]
    fn a_new_project_goes_back_from_its_name_to_the_folder_typed() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "new project");
        press(&mut picker, KeyCode::Enter);
        assert!(said(&picker).contains("/home/you/fix"), "the folder starts as the view's");
        press(&mut picker, KeyCode::Enter);
        assert!(said(&picker).contains("its name; with none"), "{}", said(&picker));
        press(&mut picker, KeyCode::Esc);
        assert!(said(&picker).contains("/home/you/fix"), "{}", said(&picker));
        assert!(said(&picker).contains("the folder it holds"), "{}", said(&picker));
    }

    #[test]
    fn enter_on_an_action_hands_it_on_with_the_palette_s_target() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "close the view");
        let (target, origin) = (fix_view(), Origin::Views);
        let act = Picked::Menu { target, origin, action: Action::CloseView };
        assert_eq!(press(&mut picker, KeyCode::Enter), act);
        retyped(&mut picker, "sidebar or views");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Chord(actions::SWAP));
        retyped(&mut picker, "pick an agent");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert!(said(&picker).contains("─ Agents"), "the finder's own list opens in the box");

        let mut picker = palette(Origin::Sidebar);
        typed(&mut picker, "unread");
        let unread = Picked::Row { target: fix_view(), action: Action::Unread };
        assert_eq!(press(&mut picker, KeyCode::Enter), unread);

        let mut listing = works();
        listing.shelves[0].projects[0].unopened = Unopened::Folder;
        let mut picker = palette_on(listing, at_fix(Origin::Views));
        typed(&mut picker, "new worktree");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        let refused = projects::folder_has_one_worktree(fix().project.listed().name());
        assert!(said(&picker).contains(&refused), "{}", said(&picker));
    }

    #[test]
    fn an_unplaced_palette_lists_only_the_keys_that_work_anywhere_and_says_why() {
        let why = "palette: luneta does not know yet which view its client is on";
        let unplaced = Palette { aim: Aim::Unplaced(why), origin: Origin::Views };
        let mut picker = palette_on(works(), unplaced);
        let every = every_action(&mut picker);
        let worktrees = every.iter().position(|row| row.contains(" › ")).unwrap();
        let anywhere = Action::anywhere().map(|(action, _)| action).filter(|it| it.in_palette());
        let anywhere: Vec<String> = anywhere.map(titled).collect();
        assert_eq!(every[..worktrees], anywhere);
        assert!(said(&picker).contains("luneta does not know yet"), "{}", said(&picker));
    }

    #[test]
    fn a_worktree_action_from_a_view_runs_on_its_worktree_and_nothing_found_offers_one() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "close the worktree");
        let (target, origin) = (fix_worktree(), Origin::Views);
        let close = Picked::Menu { target, origin, action: Action::CloseWorktree };
        assert_eq!(press(&mut picker, KeyCode::Enter), close);
        retyped(&mut picker, "zzz");
        let made = press(&mut picker, KeyCode::Enter);
        let Picked::Done { done: Done::Worktree(_, project, branch), .. } = &made else {
            panic!("nothing found offers to make what was typed: {made:?}");
        };
        assert_eq!((branch.as_str(), *project), ("zzz", fix().project.id()));
    }

    #[test]
    fn the_palette_asks_for_no_preview_of_a_worktree_it_lights() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "fix");
        assert_eq!(keyed_action(&picker).0, "luneta › fix");
        assert_eq!(picker.due(), None, "the Actions list draws no preview to ask for");
    }

    #[test]
    fn super_k_opens_the_palette_from_another_list_and_closes_it_and_tab_cycles_all_four() {
        let mut picker = palette(Origin::Views);
        assert_eq!(super_k(&mut picker), Picked::Close);
        assert_eq!(press(&mut palette(Origin::Views), KeyCode::Esc), Picked::Close);
        let aimed = at_fix(Origin::Views);
        let mut picker = Picker::open(1, List::Worktrees, works(), Machine::Local, aimed);
        assert_eq!(super_k(&mut picker), Picked::Nothing);
        assert!(said(&picker).contains("─ Actions"), "{}", said(&picker));
        for list in ["Worktrees", "Agents", "Directories", "Actions"] {
            press(&mut picker, KeyCode::Tab);
            assert!(said(&picker).contains(&format!("─ {list}")), "{list}: {}", said(&picker));
        }
    }
}
