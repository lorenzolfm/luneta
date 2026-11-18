use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fmt;
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

use lunetad::mux::{Exited, Zellij};
use lunetad::projects::{self, Name, Seen, TerminalId};
use lunetad::transcripts::SessionId;
use lunetad::zoxide::{self, Dir};
use lunetad::{eza, screens, Destination, Directory};

use crate::alerts::Doing;
use crate::away::{self, Away};
use crate::close::{self, Act, Deletes, Ends, Plan, Untold};
use crate::gruvbox;
use crate::host::Event;
use crate::screen;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::sidebar::{
    self, ACCENT, ERROR, Finder, Fit, Keep, LIT, Listed, Mark, NAME, PaneKey, PanePlace, Place,
    ProjectRow, WorkstreamPlace,
};
use crate::spots::{Drawn, Spots};
use crate::projected::{self, Projected, Route};
use crate::workstreams;
use crate::zellij::{self, Closes, SessionName};

const TITLE: &str = "luneta";

const PREVIEW_DELAY: Duration = Duration::from_millis(200);

const PEEKS_MAX: usize = 32;

const PEEK_LINES_MAX: usize = 200;

const PEEK_COLUMNS_MAX: usize = 1000;

const INPUT_HEIGHT: u16 = 3;

const MIN_RESULTS: u16 = 3;

const MIN_HALF: u16 = 26;

const MIN_ACTION: usize = 12;

#[derive(Debug, Default)]
pub struct Listing {
    pub here: Option<Here>,
    pub shown: Option<(Machine, WorkstreamPlace)>,
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
    pub workstreams: Vec<Streamed>,
}

#[derive(Clone, Debug)]
pub struct Streamed {
    pub place: WorkstreamPlace,
    pub name: String,
    pub folder: projects::Dir,
    pub branch: Option<String>,
    pub seen: Seen,
    pub used: Option<SystemTime>,
    pub terminals: Vec<PaneKey>,
    pub held: Vec<TerminalId>,
    pub waiting: usize,
    pub ending: Result<Vec<close::Doing>, Untold>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Here {
    Session { on: Machine, session: String },
    Pane { on: Machine, place: PanePlace },
}

impl Here {
    pub fn on(&self) -> &Machine {
        match self {
            Here::Session { on, .. } | Here::Pane { on, .. } => on,
        }
    }

    pub fn session(&self) -> &str {
        match self {
            Here::Session { session, .. } => session,
            Here::Pane { place, .. } => place.session.name().as_str(),
        }
    }

    pub fn pane(&self) -> Option<&PanePlace> {
        match self {
            Here::Session { .. } => None,
            Here::Pane { place, .. } => Some(place),
        }
    }
}

#[derive(Debug, Default)]
pub struct Runs {
    pub agents: Vec<Seated>,
}

#[derive(Clone, Debug)]
pub struct Seated {
    pub place: PanePlace,
    pub label: String,
    pub mark: Mark,
    pub status: String,
    pub since: Option<SystemTime>,
    pub dir: String,
    pub pid: u32,
    pub transcript: Option<SessionId>,
    pub doing: Doing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Workstreams,
    Agents,
    Dirs,
}

impl List {
    fn next(self) -> List {
        match self {
            List::Workstreams => List::Agents,
            List::Agents => List::Dirs,
            List::Dirs => List::Workstreams,
        }
    }

    fn back(self) -> List {
        match self {
            List::Workstreams => List::Dirs,
            List::Agents => List::Workstreams,
            List::Dirs => List::Agents,
        }
    }

    fn title(self) -> &'static str {
        match self {
            List::Workstreams => "Workstreams",
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
    Add { on: Machine, project: ProjectRow, folder: Directory, name: Option<Name> },
    Make { on: Machine, session: SessionName, cwd: Directory },
    Plan { on: Machine, session: Listed, deletes: Deletes },
    Delete { on: Machine, act: Act },
}

pub enum Heard {
    Exited { on: Machine, ask: u64, exited: Result<Vec<Exited>, String> },
    Peek {
        on: Machine,
        session: Listed,
        asked: Option<PaneKey>,
        peek: Result<(u32, String), String>,
    },
    Closes { on: Machine, place: PanePlace, closes: Result<Closes, String> },
    Renamed { from: String, to: Name, renamed: Result<(), String> },
    Closed { name: String, closed: Result<(), String> },
    Dirs { on: Machine, ask: u64, visited: Result<Vec<Dir>, String> },
    Listed { on: Machine, path: Directory, listed: Result<String, String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Exited { on: Machine, ask: u64 },
    Peek { on: Machine, session: Listed, asked: Option<PaneKey> },
    Closes { on: Machine, place: PanePlace },
    Rename { on: Machine, renames: Renames, to: Name },
    Close { on: Machine, place: WorkstreamPlace, name: String, seen: Vec<TerminalId> },
    Dirs { on: Machine, ask: u64 },
    List { on: Machine, path: Directory },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Renames {
    Workstream { place: WorkstreamPlace, name: String },
    Project(ProjectRow),
}

impl Renames {
    fn name(&self) -> &str {
        match self {
            Renames::Workstream { name, .. } => name,
            Renames::Project(project) => project.listed().name().as_str(),
        }
    }
}

pub fn ask(number: u64, ask: Ask, projected: &Projected, events: &Sender<Event>) {
    let events = events.clone();
    let hear: Box<dyn FnOnce() -> Heard + Send> = match ask {
        Ask::Exited { on, ask } => Box::new(move || {
            let exited = match &on {
                Machine::Local => Zellij::exited(),
                Machine::Remote(destination) => reach(destination)
                    .and_then(|mut away| away.exited()),
            };
            Heard::Exited { on, ask, exited }
        }),
        Ask::Peek { on, session, asked } => {
            let route = match asked {
                Some(pane) => projected::route(&on, &session, pane).map(|route| route.map(Some)),
                None => projected.whole(&on, &session),
            };
            Box::new(move || {
                let peek = match route {
                    Err(why) => Err(why),
                    Ok(Route::Terminal(key)) => {
                        screens::terminal(key).map(|screen| (key.id.get(), screen))
                    },
                    Ok(Route::Zellij { on: Machine::Local, session: name, pane }) => {
                        screens::dumped(&name, pane)
                    },
                    Ok(Route::Zellij { on: Machine::Remote(destination), session: name, pane }) => {
                        Away::reach(&destination, away::ANSWER)
                            .and_then(|mut away| away.screen(&name, pane))
                    },
                };
                Heard::Peek { on, session, asked, peek }
            })
        },
        Ask::Closes { on, place } => {
            let route = projected::route(&on, &place.session, place.pane);
            Box::new(move || {
                let closes = match route {
                    Err(why) => Err(why),
                    Ok(Route::Terminal(_)) => Ok(Closes::Pane),
                    Ok(Route::Zellij { on: Machine::Local, session: name, pane }) => {
                        zellij::closes_now(&name, pane)
                    },
                    Ok(Route::Zellij { on: Machine::Remote(destination), session: name, pane }) => {
                        Away::reach(&destination, away::ANSWER)
                            .and_then(|mut away| away.closes(&name, pane))
                    },
                };
                Heard::Closes { on, place, closes }
            })
        },
        Ask::Rename { on, renames, to } => Box::new(move || {
            let renamed = match &renames {
                Renames::Workstream { place, .. } => {
                    workstreams::renamed_workstream(&on, place.workstream, to.as_str())
                },
                Renames::Project(project) => {
                    workstreams::renamed_project(&on, project.id(), to.as_str())
                },
            };
            Heard::Renamed { from: renames.name().to_owned(), to, renamed }
        }),
        Ask::Close { on, place, name, seen } => Box::new(move || {
            let closed = workstreams::closed_there(&on, place.workstream, &seen);
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
    Workstream { on: Machine, place: WorkstreamPlace },
    Project { on: Machine, project: ProjectRow, folder: Directory },
    Machine { on: Machine },
    Agent { on: Machine, place: PanePlace },
    Dir { on: Machine, path: Directory, session: Result<SessionName, &'static str> },
    New { on: Machine, project: ProjectRow, folder: Directory, name: Name },
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
    Planning { on: Machine, place: PanePlace, agreed: Option<Plan> },
    Asking { on: Machine, place: PanePlace, plan: Plan, asks: String },
    Closing { on: Machine, place: WorkstreamPlace, name: String, ends: Ends, asks: String },
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

type Looked = (Machine, Listed, Option<PaneKey>);

type Located = (Machine, Directory);

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
enum Ended {
    Asking(u64),
    Heard(Vec<Exited>),
    Failed,
}

#[derive(Debug)]
pub struct Picker {
    number: u64,
    list: List,
    term: String,
    stage: Stage,
    listing: Listing,
    exited: HashMap<Machine, Ended>,
    exited_asked: u64,
    visited: HashMap<Machine, Visited>,
    dirs_asked: u64,
    workstreams: Rows,
    agents: Rows,
    dirs: Rows,
    peeks: HashMap<Looked, Peek>,
    contents: HashMap<Located, Contents>,
    wanted: Option<(Preview, Instant)>,
    error: Option<String>,
    asking: Vec<Ask>,
    aim: Machine,
    aiming: bool,
}

impl Picker {
    pub fn open(number: u64, list: List, listing: Listing, aim: Machine) -> Picker {
        let mut picker = Picker {
            number,
            list,
            term: String::new(),
            stage: Stage::Search,
            listing,
            exited: HashMap::new(),
            exited_asked: 0,
            visited: HashMap::new(),
            dirs_asked: 0,
            workstreams: Rows::default(),
            agents: Rows::default(),
            dirs: Rows::default(),
            peeks: HashMap::new(),
            contents: HashMap::new(),
            wanted: None,
            error: None,
            asking: Vec::new(),
            aim,
            aiming: true,
        };
        picker.ask_kept();
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
        self.exited.retain(|on, _| machines.contains(on));
        self.ask_kept();
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

    fn streamed(&self, on: &Machine, place: &WorkstreamPlace) -> Option<&Streamed> {
        let workstreams = &self.housed(on, &place.project)?.workstreams;
        workstreams.iter().find(|streamed| streamed.place.workstream == place.workstream)
    }

    fn row_streamed(&self, on: &Machine, place: &WorkstreamPlace) -> &Streamed {
        let streamed = self.streamed(on, place);
        streamed.expect("a workstream's row is rebuilt with the listing that holds it")
    }

    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        self.shelved(on)?.away?;
        Some(format!("lunetad does not reach {} now", self.machine_name(on)))
    }

    fn is_shown(&self, on: &Machine, place: &WorkstreamPlace) -> bool {
        self.listing.shown.as_ref().is_some_and(|(at, shown)| at == on && shown == place)
    }

    fn focus(&self, on: &Machine, streamed: &Streamed) -> Option<PaneKey> {
        let here = self.here_pane(on).map(|place| place.pane);
        let focused = here.filter(|pane| streamed.terminals.contains(pane));
        focused.or_else(|| streamed.terminals.first().copied())
    }

    fn seated(&self, on: &Machine, place: &PanePlace) -> Option<&Seated> {
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

    fn ask_kept(&mut self) {
        for on in self.machines() {
            if !self.exited.contains_key(&on) {
                self.exited_asked += 1;
                let ask = self.exited_asked;
                self.exited.insert(on.clone(), Ended::Asking(ask));
                self.asking.push(Ask::Exited { on, ask });
            }
        }
    }

    pub fn heard(&mut self, heard: Heard) -> Picked {
        match heard {
            Heard::Exited { on, ask, exited } => {
                if !matches!(self.exited.get(&on), Some(Ended::Asking(asked)) if *asked == ask) {
                    return Picked::Nothing;
                }
                let ended = match exited {
                    Ok(exited) => Ended::Heard(exited),
                    Err(_) => Ended::Failed,
                };
                self.exited.insert(on, ended);
                self.rebuild(false);
            },
            Heard::Peek { on, session, asked, peek } => {
                let peek = match peek {
                    Ok((_, dumped)) => Peek::of(&dumped),
                    Err(why) => Peek::Failed(why),
                };
                self.peeks.insert((on, session, asked), peek);
            },
            Heard::Closes { on, place, closes } => {
                let planning = matches!(
                    &self.stage,
                    Stage::Planning { on: planned, place: at, .. }
                        if (planned, at) == (&on, &place)
                );
                if !planning {
                    return Picked::Nothing;
                }
                return match closes {
                    Ok(closes) => Picked::Plan {
                        on,
                        session: place.session,
                        deletes: Deletes::Pane { pane: place.pane, closes },
                    },
                    Err(why) => {
                        self.stage = Stage::Search;
                        self.error = Some(format!("nothing closed: {why}"));
                        Picked::Nothing
                    },
                };
            },
            Heard::Renamed { from, to, renamed } => {
                if let Err(why) = renamed {
                    self.error = Some(format!("{from} is not called {}: {why}", to.as_str()));
                }
            },
            Heard::Closed { name, closed } => {
                if let Err(why) = closed {
                    self.error = Some(format!("{name} stays: {why}"));
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

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) -> Drawn {
        let area = Picker::area(over);
        let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2))
        else {
            return Drawn::Picker { number: self.number, area, spots: Spots::default() };
        };
        framed(buffer, area, TITLE, "", Vec::new());
        let layout = Layout::of(Rect::new(area.x + 1, area.y + 1, width, height));
        if let Stage::Rename { on, renames, input } = &self.stage {
            let spots = self.draw_rename(buffer, &layout, on, renames, input);
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
        draw_input(buffer, &layout, self.list.title(), prompt);
        spots.extend(self.draw_help(buffer, layout.help));
        Drawn::Picker { number: self.number, area, spots }
    }

    fn picks(&self) -> bool {
        matches!(self.stage, Stage::Search)
    }

    fn rows(&self) -> &Rows {
        match self.list {
            List::Workstreams => &self.workstreams,
            List::Agents => &self.agents,
            List::Dirs => &self.dirs,
        }
    }

    fn rows_mut(&mut self) -> &mut Rows {
        match self.list {
            List::Workstreams => &mut self.workstreams,
            List::Agents => &mut self.agents,
            List::Dirs => &mut self.dirs,
        }
    }

    fn search_key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        let held_super = key.modifiers == KeyModifiers::SUPER;
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
                self.list = List::Workstreams;
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
            Key::Workstream { on, place } if self.is_shown(on, place) => Picked::Close,
            Key::Workstream { on, place } => {
                Picked::Go { on: on.clone(), place: Place::Workstream(place.clone()) }
            },
            Key::Project { on, project, folder } => {
                let (on, project, folder) = (on.clone(), project.clone(), folder.clone());
                Picked::Add { on, project, folder, name: None }
            },
            Key::Machine { .. } | Key::Dir { session: Err(_), .. } => Picked::Nothing,
            Key::Agent { on, place } => {
                Picked::Go { on: on.clone(), place: Place::Pane(place.clone()) }
            },
            Key::Dir { on, path, session: Ok(session) } => {
                Picked::Make { on: on.clone(), session: session.clone(), cwd: path.clone() }
            },
            Key::New { on, project, folder, name } => {
                let (on, project, folder) = (on.clone(), project.clone(), folder.clone());
                Picked::Add { on, project, folder, name: Some(name.clone()) }
            },
        }
    }

    fn here_project(&self) -> Option<(&Machine, &Housed)> {
        let (on, place) = self.listing.shown.as_ref()?;
        Some((on, self.housed(on, &place.project)?))
    }

    fn new_key(&self) -> Option<Key> {
        let (on, housed) = self.here_project()?;
        let name = Name::parse(&self.term).ok()?;
        let (on, project, folder) = (on.clone(), housed.project.clone(), (&housed.dir).into());
        Some(Key::New { on, project, folder, name })
    }

    fn delete(&mut self) -> Picked {
        let Some(key) = self.rows().selected().map(|row| row.key.clone()) else {
            return Picked::Nothing;
        };
        match key {
            Key::Workstream { on, place } => {
                let name = self.row_streamed(&on, &place).name.clone();
                self.close(on, place, name, None);
                Picked::Nothing
            },
            Key::Project { project, .. } => {
                let name = project.listed().name();
                self.error = Some(format!("{name} holds no workstream to close"));
                Picked::Nothing
            },
            Key::Agent { on, place } => self.plan(on, place, None),
            Key::Machine { .. } | Key::Dir { .. } | Key::New { .. } => Picked::Nothing,
        }
    }

    fn close(&mut self, on: Machine, place: WorkstreamPlace, name: String, agreed: Option<&Ends>) {
        let Some(streamed) = self.streamed(&on, &place) else {
            self.error = Some(format!("{name} is listed no more"));
            return;
        };
        let ends = Ends::of(&streamed.held, streamed.ending.as_deref().map_err(|untold| *untold));
        if let Some(why) = self.out_of_reach(&on) {
            self.error = Some(format!("{name} stays: {why}"));
            return;
        }
        if agreed != Some(&ends)
            && let Some(asks) = ends.question(&format!("Close workstream {name}"))
        {
            self.stage = Stage::Closing { on, place, name, ends, asks };
            return;
        }
        self.asking.push(Ask::Close { on, place, name, seen: ends.seen });
    }

    fn plan(&mut self, on: Machine, place: PanePlace, agreed: Option<Plan>) -> Picked {
        self.asking.push(Ask::Closes { on: on.clone(), place: place.clone() });
        self.stage = Stage::Planning { on, place, agreed };
        Picked::Nothing
    }

    fn rename(&mut self) {
        if self.list != List::Workstreams {
            return;
        }
        let (on, renames) = match self.workstreams.selected().map(|row| &row.key) {
            Some(Key::Workstream { on, place }) => {
                let name = self.row_streamed(on, place).name.clone();
                (on.clone(), Renames::Workstream { place: place.clone(), name })
            },
            Some(Key::Project { on, project, .. }) => {
                (on.clone(), Renames::Project(project.clone()))
            },
            Some(Key::Machine { .. } | Key::Agent { .. } | Key::Dir { .. } | Key::New { .. })
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
        let name = Name::parse(name)?;
        if name.as_str() == renames.name() {
            return Err("already called that".to_owned());
        }
        let listed = match renames {
            Renames::Workstream { place, .. } => self.streamed(on, place).is_some(),
            Renames::Project(project) => self.housed(on, project).is_some(),
        };
        if !listed {
            return Err(format!("{} is listed no more", renames.name()));
        }
        Ok(name)
    }

    fn taken(&self, on: &Machine) -> Result<Vec<&str>, &'static str> {
        let exited = match self.exited.get(on) {
            Some(Ended::Heard(exited)) => exited,
            Some(Ended::Asking(_)) | None => {
                return Err("zellij has yet to say which sessions ended");
            },
            Some(Ended::Failed) => return Err("zellij did not say which sessions ended"),
        };
        let projects = self.shelved(on).into_iter().flat_map(|shelved| &shelved.projects);
        let projects = projects.map(|housed| housed.project.listed().name().as_str());
        let ended = exited.iter().map(|exited| exited.name.as_str());
        Ok(self.here_session(on).into_iter().chain(projects).chain(ended).collect())
    }

    fn rebuild(&mut self, snap: bool) {
        let finder = (!self.term.is_empty()).then(|| Finder::new(&self.term));
        let ruled = !self.listing.remotes.is_empty();
        let mut agents = Vec::new();
        for (on, runs) in self.each() {
            let rule = ruled.then(|| self.machine_name(&on));
            grouped(&mut agents, self.agent_rows(finder.as_ref(), &on, runs), rule);
        }
        let mut workstreams = self.workstream_rows(finder.as_ref());
        if workstreams.is_empty()
            && let Some(key) = self.new_key()
        {
            workstreams.push(Row::plain(key, self.term.clone()));
        }
        let (dirs, aimed) = self.dir_rows();
        let workstream_start = match workstreams.first().map(|row| &row.key) {
            Some(Key::Workstream { on, place }) if self.is_shown(on, place) => {
                usize::from(self.term.is_empty())
            },
            Some(_) | None => 0,
        };
        let held = |rows: &Rows| rows.selected().map(|row| row.key.clone()).filter(|_| !snap);
        let (held_workstream, held_agent) = (held(&self.workstreams), held(&self.agents));
        let held_dir = held(&self.dirs).filter(|_| !self.aiming);
        self.workstreams.replace(workstreams, held_workstream.as_ref(), workstream_start);
        self.agents.replace(agents, held_agent.as_ref(), 0);
        self.dirs.replace(dirs, held_dir.as_ref(), aimed.unwrap_or(0));
        self.follow();
    }

    fn workstream_rows(&self, finder: Option<&Finder<'_>>) -> Vec<Row> {
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
                if housed.workstreams.is_empty() {
                    let (row, folder) = (housed.project.clone(), (&housed.dir).into());
                    let key = Key::Project { on: on.clone(), project: row, folder };
                    let dir = sidebar::short_cwd(housed.dir.as_str());
                    found.push((None, project.to_owned(), None, dir, key));
                }
                for streamed in &housed.workstreams {
                    let name = format!("{project} › {}", streamed.name);
                    let folder = sidebar::short_cwd(streamed.folder.as_str());
                    let key = Key::Workstream { on: on.clone(), place: streamed.place.clone() };
                    found.push((streamed.used, name, streamed.branch.as_deref(), folder, key));
                }
            }
        }
        found.sort_by_key(|(used, ..)| Reverse(*used));
        let candidates = found.iter().map(|(_, name, branch, folder, key)| {
            (name.clone(), vec![*branch, Some(folder.as_str())], key.clone())
        });
        let mut rows = matched(finder, candidates);
        if finder.is_none() {
            rows.extend(machines);
        }
        rows
    }

    fn agent_rows(&self, finder: Option<&Finder<'_>>, on: &Machine, runs: &Runs) -> Vec<Row> {
        let yours = self.here_pane(on);
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
                    Machine::Local => taken.as_deref().map(|taken| dir.name.free(taken)),
                    Machine::Remote(_) => Ok(dir.name.clone()),
                };
                (dir, session.map_err(|why| *why))
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
        let preview = match self.rows().selected().map(|row| &row.key) {
            Some(Key::Workstream { on, place }) => {
                let focus = self.focus(on, self.row_streamed(on, place));
                let looked = |focus| (on.clone(), place.project.listed().clone(), Some(focus));
                focus.map(|focus| Preview::Screen(looked(focus)))
            },
            Some(Key::Agent { on, place }) => {
                Some(Preview::Screen((on.clone(), place.session.clone(), Some(place.pane))))
            },
            Some(Key::Dir { on, path, .. }) => Some(Preview::Dir((on.clone(), path.clone()))),
            Some(Key::Project { .. } | Key::Machine { .. } | Key::New { .. }) | None => None,
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
            List::Workstreams => {},
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

    fn here_pane(&self, on: &Machine) -> Option<&PanePlace> {
        self.listing.here.as_ref().filter(|here| here.on() == on)?.pane()
    }

    fn yours(&self) -> Option<(&Seated, &[Seated])> {
        let here = self.listing.here.as_ref()?;
        let (pane, agents) = (here.pane()?, &self.runs(here.on())?.agents);
        Some((agents.iter().find(|seated| seated.place == *pane)?, agents))
    }

    fn row_line(&self, row: &Row, width: usize, frame: usize, now: SystemTime) -> Line<'static> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let grey = Style::new().fg(gruvbox::GREY);
        let (lead, name_style, right) = match &row.key {
            Key::Workstream { on, place } => {
                let streamed = self.row_streamed(on, place);
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = Vec::new();
                if streamed.waiting > 0 {
                    let (dot, style) = Mark::Waiting.glyph(frame);
                    right.push(Span::styled(format!("{dot}{} ", streamed.waiting), style));
                }
                right.extend(self.tag(on, away));
                let (stage, colour) = staged(streamed.seen);
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
            Key::Project { on, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = self.tag(on, away);
                right.push(Span::styled("no workstream", grey));
                (None, if away.is_some() { dim } else { bold }, right)
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
                (Some(lead), bold, vec![Span::styled("new workstream", grey)])
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
        if matches!(&row.key, Key::Workstream { on, place } if self.is_shown(on, place)) {
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

    fn empty(&self) -> String {
        if !self.term.is_empty() {
            return format!("no match for \"{}\"", self.term);
        }
        match self.list {
            List::Workstreams if self.listing.away.is_some() => "no workstreams".to_owned(),
            List::Workstreams => "lunetad keeps no workstream yet".to_owned(),
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
            if self.list == List::Workstreams && self.here_project().is_some() {
                lines.push(Line::default());
                lines.push(Line::styled(" type a name to make a workstream of it", grey));
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
            Key::Dir { on, path, .. } => {
                self.draw_contents(buffer, area, &row.name, on, path);
                return;
            },
            Key::New { project, folder, name, .. } => {
                let project = project.listed().name();
                let words = format!(
                    "no workstream matches: Enter makes one named {}, in {project}'s folder, \
                     {folder}, and goes to it",
                    name.as_str(),
                );
                let lines = said(vec![Span::styled(" new", grey)], &words);
                framed(buffer, area, &row.name, "", lines);
                return;
            },
            Key::Project { on, folder, .. } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                caption.push(Span::styled("no workstream", grey));
                let words = format!("Enter starts a workstream in {folder}");
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
            Key::Workstream { on, place } => {
                let streamed = self.row_streamed(on, place);
                let count = streamed.terminals.len();
                let right =
                    if count == 1 { "1 terminal".to_owned() } else { format!("{count} terminals") };
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                let room = inner.saturating_sub(1 + caption.iter().map(Span::width).sum::<usize>());
                caption.push(Span::styled(sidebar::tail(streamed.folder.as_str(), room), grey));
                let Some(focus) = self.focus(on, streamed) else {
                    let lines = said(caption, "it holds no terminal to look at");
                    framed(buffer, area, &row.name, &right, lines);
                    return;
                };
                let looked = (on.clone(), place.project.listed().clone(), Some(focus));
                (right, Line::from(caption), looked)
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
                let looked = (on.clone(), place.session.clone(), Some(place.pane));
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
                Key::Workstream { on, place } if self.is_shown(on, place) => {
                    Ok("you are in it".to_owned())
                },
                Key::Workstream { on, .. } => Ok(there("Go", on)),
                Key::Project { on, .. } => Ok(there("Start a workstream", on)),
                Key::Machine { on } => Err(self.out_of_reach(on).unwrap_or_else(|| {
                    format!("{} keeps no project", self.machine_name(on))
                })),
                Key::Agent { on, .. } => Ok(there("Go to agent", on)),
                Key::Dir { on, session: Ok(session), .. } => {
                    Ok(there(&format!("Make \"{session}\""), on))
                },
                Key::Dir { session: Err(why), .. } => Err((*why).to_owned()),
                Key::New { project, name, .. } => {
                    let project = project.listed().name();
                    Ok(format!("New workstream \"{}\" in {project}", name.as_str()))
                },
            },
            None if self.list == List::Workstreams && !self.term.is_empty() => {
                Name::parse(&self.term).map(|_| String::new())
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
            (Stage::Search, List::Workstreams) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Workstreams.next().title()),
                (Hint::Ctrl('r'), "Rename"),
                (Hint::Delete, "Close it"),
                (Hint::Esc, "Close"),
            ],
            (Stage::Search, List::Agents) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Agents.next().title()),
                (Hint::Delete, "Close its pane"),
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
                Renames::Workstream { place, .. } if self.is_shown(on, place) => {
                    what.push_str(", the workstream you are in");
                },
                Renames::Workstream { .. } => {},
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

fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

fn agent_name(agents: &[Seated], seated: &Seated) -> String {
    let alike = agents.iter().filter(|other| other.label == seated.label);
    let sessions: HashSet<&SessionName> =
        alike.clone().map(|other| other.place.session.name()).collect();
    let (session, pane) = (seated.place.session.name(), seated.place.pane);
    match (alike.count(), sessions.len()) {
        (0 | 1, _) => seated.label.clone(),
        (_, 1) => format!("{}:{pane}", seated.label),
        _ => format!("{} ({session}:{pane})", seated.label),
    }
}

fn grouped(into: &mut Vec<Row>, mut rows: Vec<Row>, rule: Option<String>) -> Option<usize> {
    let first = rows.first_mut()?;
    first.rules.machine = rule;
    let at = into.len();
    into.extend(rows);
    Some(at)
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
    dirs: impl Iterator<Item = (&'a Dir, Result<SessionName, &'static str>)>,
) -> Vec<Row> {
    let mut found: Vec<(bool, i64, usize, Row)> = Vec::new();
    for (at, (dir, session)) in dirs.enumerate() {
        let name = session.as_ref().unwrap_or(&dir.name).to_string();
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

    use lunetad::projects::WorkstreamId;
    use ratatui::layout::Position;

    use super::*;
    use crate::close::Doing;
    use crate::spots::Hit;

    const NOW: Duration = Duration::from_secs(1000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    fn listed(name: &str, started: u64) -> Listed {
        Listed::new(name, UNIX_EPOCH + Duration::from_secs(started))
    }

    fn seated(name: &str, started: u64, pane: u32, label: &str, mark: Mark) -> Seated {
        Seated {
            place: PanePlace { session: listed(name, started), pane: PaneKey::Zellij(pane) },
            label: label.to_owned(),
            mark,
            status: "idle".to_owned(),
            since: Some(now() - Duration::from_mins(1)),
            dir: "luneta/main".to_owned(),
            pid: pane,
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

    fn ended(name: &str, ago: u64) -> Exited {
        Exited { name: SessionName::running(name).unwrap(), ago: Duration::from_secs(ago) }
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

    fn kept_here(exited: Result<Vec<Exited>, String>) -> Heard {
        Heard::Exited { on: Machine::Local, ask: 1, exited }
    }

    fn exited_asks(picker: &mut Picker) -> Vec<Ask> {
        picker.asks().into_iter().filter(|ask| matches!(ask, Ask::Exited { .. })).collect()
    }

    fn project(name: &str, id: u64) -> ProjectRow {
        ProjectRow::new(name, id)
    }

    fn place(project: &str, id: u64, workstream: u64) -> WorkstreamPlace {
        let workstream = WorkstreamId::of(id * 10 + workstream).unwrap();
        WorkstreamPlace { project: self::project(project, id), workstream }
    }

    fn stream(
        place: WorkstreamPlace,
        name: &str,
        used: Option<u64>,
        terminals: &[u32],
    ) -> Streamed {
        Streamed {
            place,
            name: name.to_owned(),
            folder: projects::Dir::parse(&format!("/home/you/{name}")).unwrap(),
            branch: None,
            seen: Seen::Read(projects::Stage::Working),
            used: used.map(|at| UNIX_EPOCH + Duration::from_secs(at)),
            terminals: terminals.iter().map(|id| PaneKey::terminal(*id)).collect(),
            held: held(terminals),
            waiting: 0,
            ending: Ok(Vec::new()),
        }
    }

    fn held(terminals: &[u32]) -> Vec<TerminalId> {
        terminals.iter().map(|id| TerminalId::of(u64::from(*id)).unwrap()).collect()
    }

    fn housed(name: &str, id: u64, workstreams: Vec<Streamed>) -> Housed {
        let dir = projects::Dir::parse(&format!("/home/you/{name}")).unwrap();
        Housed { project: project(name, id), dir, workstreams }
    }

    fn shelved(on: Machine, projects: Vec<Housed>) -> Shelved {
        Shelved { on, away: None, projects }
    }

    fn fix() -> WorkstreamPlace {
        place("luneta", 1, 2)
    }

    fn api() -> WorkstreamPlace {
        place("web", 3, 1)
    }

    fn main_place() -> WorkstreamPlace {
        place("luneta", 1, 1)
    }

    fn luneta() -> Housed {
        housed("luneta", 1, vec![
            stream(main_place(), "main", Some(30), &[1, 2]),
            stream(fix(), "fix", Some(50), &[5, 3]),
            stream(place("luneta", 1, 3), "docs", None, &[]),
        ])
    }

    fn works() -> Listing {
        let projects = vec![
            luneta(),
            housed("notes", 2, Vec::new()),
            housed("web", 3, vec![stream(api(), "api", Some(40), &[4])]),
        ];
        let at = PanePlace { session: fix().project.listed().clone(), pane: PaneKey::terminal(3) };
        Listing {
            here: Some(Here::Pane { on: Machine::Local, place: at }),
            shown: Some((Machine::Local, fix())),
            shelves: vec![shelved(Machine::Local, projects)],
            ..Listing::default()
        }
    }

    fn opened(listing: Listing) -> Picker {
        let mut picker = Picker::open(1, List::Workstreams, listing, Machine::Local);
        picker.heard(kept_here(Ok(Vec::new())));
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

    fn go(on: Machine, place: WorkstreamPlace) -> Picked {
        Picked::Go { on, place: Place::Workstream(place) }
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
    fn the_workstreams_go_by_their_last_use_and_an_empty_project_is_one_row() {
        let mut picker = Picker::open(1, List::Workstreams, works(), Machine::Local);
        let on = Machine::Local;
        let asks = vec![Ask::Exited { on: on.clone(), ask: 1 }, Ask::Dirs { on, ask: 1 }];
        assert_eq!(picker.asks(), asks);
        let shown = ["luneta › fix", "web › api", "luneta › main", "luneta › docs", "notes"];
        assert_eq!(rows(&picker), shown);
        let (lines, _) = list(&picker);
        assert!(lines[0].contains("─ here") && lines[0].ends_with("working"), "{lines:?}");
        assert!(lines[4].ends_with("no workstream"), "{lines:?}");
        assert_eq!(lit(&picker), "web › api");
        assert!(said(&picker).contains("<ENTER> Go"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), go(Machine::Local, api()));
        press(&mut picker, KeyCode::Up);
        assert!(said(&picker).contains("you are in it"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        down_to(&mut picker, "notes");
        assert!(said(&picker).contains("<ENTER> Start a workstream"), "{}", said(&picker));
        let added = Picked::Add {
            on: Machine::Local,
            project: project("notes", 2),
            folder: directory("/home/you/notes"),
            name: None,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), added);

        let mut later = works();
        let shelf = &mut later.shelves[0].projects[0].workstreams;
        shelf[0].used = Some(UNIX_EPOCH + Duration::from_secs(60));
        picker.listed(later);
        assert_eq!(rows(&picker)[..3], ["luneta › main", "luneta › fix", "web › api"]);
        assert_eq!(lit(&picker), "notes");
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
        let shelf = &mut listing.shelves[0].projects[0].workstreams;
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

        let place = PanePlace { session: listed("c", 30), pane: PaneKey::Zellij(4) };
        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
        press(&mut picker, KeyCode::Delete);
        let heard = Heard::Closes { on: Machine::Local, place, closes: Ok(Closes::Pane) };
        assert!(matches!(picker.heard(heard), Picked::Plan { .. }));
        let (rows, give_up) = clicks(&picker, "<ESC>");
        assert!(!rows.is_empty() && rows.iter().all(|(_, pick)| pick.is_none()), "{rows:?}");
        assert_eq!(give_up, Some(Pick::Key(KeyEvent::from(KeyCode::Esc))));
    }

    #[test]
    fn what_is_typed_narrows_the_list_and_enter_makes_a_workstream_no_row_matches() {
        let mut picker = opened(works());
        typed(&mut picker, "api");
        assert_eq!(rows(&picker), ["web › api"]);
        retyped(&mut picker, "luneta");
        assert_eq!(rows(&picker), ["luneta › fix", "luneta › main", "luneta › docs"]);
        assert_eq!(lit(&picker), "luneta › fix");
        retyped(&mut picker, "zzz");
        let (lines, lit) = list(&picker);
        let new = lines[0].starts_with("+ zzz ") && lines[0].ends_with(" new workstream");
        assert!(new, "{lines:?}");
        assert_eq!(lit.as_ref(), lines.first());
        let words = r#"<ENTER> New workstream "zzz" in luneta"#;
        assert!(said(&picker).contains(words), "{}", said(&picker));
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(shown.contains("named zzz, in luneta's folder, /home/you/luneta"), "{shown}");
        let added = Picked::Add {
            on: Machine::Local,
            project: project("luneta", 1),
            folder: directory("/home/you/luneta"),
            name: Some(Name::parse("zzz").unwrap()),
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), added);
        typed(&mut picker, " ");
        assert_eq!(list(&picker).0, [r#"no match for "zzz ""#]);
        assert!(said(&picker).contains("begins or ends with a space"), "{}", said(&picker));
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
        assert!(titled(&picker, "Workstreams"));
        for title in ["Agents", "Directories"] {
            press(&mut picker, KeyCode::Tab);
            assert!(titled(&picker, title), "{title}");
            assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        }
        for title in ["Agents", "Workstreams", "Directories"] {
            press(&mut picker, KeyCode::BackTab);
            assert!(titled(&picker, title), "{title}");
        }
    }

    #[test]
    fn del_closes_a_workstream_and_asks_first_where_an_agent_at_work_or_unread_files_end() {
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
        let asks = "Close workstream main? 2 agents, 1 busy";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
        press(&mut picker, KeyCode::Char('n'));
        assert!(!said(&picker).contains(asks), "{}", said(&picker));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        press(&mut picker, KeyCode::Delete);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [close(main_place(), "main", &[1, 2])]);
        let closed = Err("luneta-mux did not answer".to_owned());
        picker.heard(Heard::Closed { name: "main".to_owned(), closed });
        let stays = "main stays: luneta-mux did not answer";
        assert!(said(&picker).contains(stays), "{}", said(&picker));

        down_to(&mut picker, "luneta › docs");
        press(&mut picker, KeyCode::Delete);
        let asks = "Close workstream docs? 1 agent, 1 unknown";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
        let mut gone = busy(works());
        gone.shelves[0].projects[0].workstreams.pop();
        picker.listed(gone);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert!(said(&picker).contains("docs is listed no more"), "{}", said(&picker));

        down_to(&mut picker, "notes");
        press(&mut picker, KeyCode::Delete);
        assert!(said(&picker).contains("notes holds no workstream to close"), "{}", said(&picker));

        let mut untold = works();
        untold.shelves[0].projects[2].workstreams[0].ending = Err(Untold::Away);
        let mut picker = opened(untold);
        press(&mut picker, KeyCode::Delete);
        let asks = "Close workstream api? luneta cannot tell which agents";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
    }

    #[test]
    fn a_yes_to_closing_a_workstream_asks_again_when_what_would_end_changed_since() {
        let mut picker = opened(busy(works()));
        down_to(&mut picker, "luneta › main");
        press(&mut picker, KeyCode::Delete);
        let busy_asks = "Close workstream main? 2 agents, 1 busy";
        assert!(said(&picker).contains(busy_asks), "{}", said(&picker));
        let mut untold = busy(works());
        let shelf = &mut untold.shelves[0].projects[0].workstreams;
        let main = shelf.iter_mut().find(|streamed| streamed.name == "main").unwrap();
        main.ending = Err(Untold::Away);
        picker.listed(untold);
        press(&mut picker, KeyCode::Char('y'));
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        let untold_asks = "Close workstream main? luneta cannot tell which agents";
        assert!(said(&picker).contains(untold_asks), "{}", said(&picker));
        let mut grown = busy(works());
        let shelf = &mut grown.shelves[0].projects[0].workstreams;
        let main = shelf.iter_mut().find(|streamed| streamed.name == "main").unwrap();
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
    fn ctrl_r_renames_the_highlighted_workstream_or_empty_project() {
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
        let renames = Renames::Workstream { place: api(), name: "api".to_owned() };
        let to = Name::parse("gateway").unwrap();
        assert_eq!(picker.asks(), [Ask::Rename { on: Machine::Local, renames, to: to.clone() }]);
        assert!(!said(&picker).contains("renaming"), "{}", said(&picker));
        let refused = Err("lunetad: no".to_owned());
        picker.heard(Heard::Renamed { from: "api".to_owned(), to, renamed: refused });
        assert!(said(&picker).contains("api is not called gateway: lunetad: no"));

        press(&mut picker, KeyCode::Up);
        control(&mut picker, 'r');
        let yours = r#"renaming "fix", the workstream you are in"#;
        assert!(said(&picker).contains(yours), "{}", said(&picker));
        press(&mut picker, KeyCode::Esc);
        down_to(&mut picker, "notes");
        control(&mut picker, 'r');
        assert!(said(&picker).contains(r#"renaming "notes", a project"#), "{}", said(&picker));
        typed(&mut picker, "jots");
        press(&mut picker, KeyCode::Enter);
        let renames = Renames::Project(project("notes", 2));
        let to = Name::parse("jots").unwrap();
        assert_eq!(picker.asks(), [Ask::Rename { on: Machine::Local, renames, to }]);

        press(&mut picker, KeyCode::Up);
        control(&mut picker, 'r');
        typed(&mut picker, "docs-2");
        let mut gone = works();
        gone.shelves[0].projects[0].workstreams.pop();
        picker.listed(gone);
        press(&mut picker, KeyCode::Enter);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert!(said(&picker).contains("docs is listed no more"), "{}", said(&picker));
    }

    fn machines() -> Listing {
        let mut far = shelved(Machine::Remote("far".parse().unwrap()), vec![housed(
            "old",
            5,
            vec![stream(place("old", 5, 1), "stale", Some(10), &[7])],
        )]);
        far.away = Some("unreachable");
        let gateway = stream(place("api", 4, 1), "gateway", Some(60), &[6]);
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
    fn every_machine_s_workstreams_are_listed_an_unreachable_one_s_greyed() {
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
        let renames = Renames::Workstream { place: gateway, name: "gateway".to_owned() };
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
    fn the_preview_shows_the_terminal_luneta_is_on_else_the_workstream_s_first() {
        let mut picker = opened(works());
        let peek = |session: Listed, pane| Ask::Peek {
            on: Machine::Local,
            session,
            asked: Some(PaneKey::terminal(pane)),
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
        let (session, asked) = (api().project.listed().clone(), Some(PaneKey::terminal(4)));
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
    fn the_preview_of_another_machine_s_workstream_is_its_screen_there() {
        let mut picker = opened(machines());
        picker.ticked(picker.due().unwrap());
        let session = place("api", 4, 1).project.listed().clone();
        let asked = Some(PaneKey::terminal(6));
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
        let (session, asked) = (api().project.listed().clone(), Some(PaneKey::terminal(4)));
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
        let mut picker = Picker::open(1, List::Agents, Listing::default(), Machine::Local);
        control(&mut picker, 'j');
        assert_eq!(picker.list, List::Agents, "Ctrl j is the panes' Down");
        picker.key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SUPER));
        assert_eq!(picker.list, List::Workstreams);
        control(&mut picker, 'd');
        assert_eq!(picker.list, List::Dirs);
        control(&mut picker, 'b');
        assert_eq!(picker.list, List::Agents);
        assert_eq!(picker.term, "", "no key typed into the search");
    }

    #[test]
    fn del_on_an_agent_asks_before_ending_it_at_work_and_goes_ahead_on_yes_to_the_same_plan() {
        let place = PanePlace { session: listed("c", 30), pane: PaneKey::Zellij(4) };
        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
        picker.asks();
        let c = listed("c", 30);
        let deletes = || Deletes::Pane { pane: PaneKey::Zellij(4), closes: Closes::Pane };
        let planning = Picked::Plan { on: Machine::Local, session: c.clone(), deletes: deletes() };
        let act = || {
            Act::Pane { session: c.name().clone(), whose: c.whose(), pane: PaneKey::Zellij(4) }
        };
        let plan = |doing: Doing| Plan::of(act(), Ok(&[(4, doing)]));
        let closes = |picker: &mut Picker| {
            assert_eq!(picker.asks(), [Ask::Closes { on: Machine::Local, place: place.clone() }]);
            let closes = Ok(Closes::Pane);
            picker.heard(Heard::Closes { on: Machine::Local, place: place.clone(), closes })
        };
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(closes(&mut picker), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Busy))), Picked::Nothing);
        let asks = |picker: &Picker| said(picker).contains("Close the pane? 1 agent, 1 busy");
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('n')), Picked::Nothing);
        assert!(!asks(&picker));

        press(&mut picker, KeyCode::Delete);
        closes(&mut picker);
        picker.planned(Ok(plan(Doing::Busy)));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), Picked::Nothing);
        assert_eq!(closes(&mut picker), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), Picked::Nothing);
        press(&mut picker, KeyCode::Char('y'));
        closes(&mut picker);
        let ended = Picked::Delete { on: Machine::Local, act: act() };
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), ended);

        press(&mut picker, KeyCode::Delete);
        closes(&mut picker);
        assert_eq!(picker.planned(Ok(plan(Doing::Idle))), ended);
    }

    #[test]
    fn del_on_an_agent_asks_zellij_what_closing_its_pane_closes_first() {
        let place = PanePlace { session: listed("c", 30), pane: PaneKey::Zellij(4) };
        let listing = Listing {
            local: agents(vec![seated("c", 30, 4, "fold", Mark::Idle)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
        picker.asks();
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let ask = Ask::Closes { on: Machine::Local, place: place.clone() };
        assert_eq!(picker.asks(), vec![ask]);
        let heard = |on, place| Heard::Closes { on, place, closes: Ok(Closes::Pane) };
        let other = PanePlace { session: listed("c", 30), pane: PaneKey::Zellij(5) };
        assert_eq!(picker.heard(heard(Machine::Local, other)), Picked::Nothing);
        assert_eq!(picker.heard(heard(nixos(), place.clone())), Picked::Nothing);
        let deletes = Deletes::Pane { pane: PaneKey::Zellij(4), closes: Closes::Pane };
        let plan = Picked::Plan { on: Machine::Local, session: listed("c", 30), deletes };
        assert_eq!(picker.heard(heard(Machine::Local, place)), plan);
    }

    #[test]
    fn agents_named_alike_are_told_apart_and_yours_is_pinned_over_them() {
        let place = PanePlace { session: listed("work", 20), pane: PaneKey::Zellij(9) };
        let listing = Listing {
            here: Some(Here::Pane { on: Machine::Local, place }),
            local: agents(vec![
                seated("work", 20, 9, "mine", Mark::Busy),
                seated("work", 20, 1, "fix", Mark::Waiting),
                seated("work", 20, 2, "fix", Mark::Idle),
                seated("work", 20, 3, "docs", Mark::Idle),
                seated("review", 10, 4, "docs", Mark::Idle),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
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
        let at = |pane, label, dir: &str| Seated {
            dir: dir.to_owned(),
            ..seated("work", 20, pane, label, Mark::Idle)
        };
        let listing = Listing {
            local: agents(vec![
                at(1, "Zero-length lines in jobs", "src/zellij"),
                at(2, "fix", "src/zellij"),
                at(3, "zellij panics", "misc/luneta"),
                at(4, "docs", "luneta/main"),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
        typed(&mut picker, "zellij");
        let rows = found(&picker);
        let shown = [("zellij panics", "zellij"), ("Zero-length lines in jobs", ""), ("fix", "")];
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
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local);
        assert_eq!(list(&picker).0, ["asking zoxide…"]);
        picker.heard(kept_here(Ok(vec![ended("bipa", 100)])));
        picker.heard(heard_here(Ok(vec![
            visited("/home/you/luneta", "luneta"),
            visited("/home/you/luneta/main", "luneta-main"),
            visited("/home/you/Work/bipa", "bipa"),
            visited("/home/you/notes", "notes"),
        ])));
        assert_eq!(names(&picker), ["luneta-2", "luneta-main", "bipa-2", "notes-2"]);
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
    fn no_session_takes_the_name_luneta_s_zellij_is_on_its_way_to() {
        let listing = Listing {
            here: Some(here("notes")),
            shelves: vec![shelved(Machine::Local, vec![housed("luneta", 1, Vec::new())])],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local);
        picker.heard(kept_here(Ok(Vec::new())));
        picker.heard(heard_here(Ok(vec![
            visited("/home/you/notes", "notes"),
            visited("/home/you/luneta", "luneta"),
        ])));
        assert_eq!(names(&picker), ["notes-2", "luneta-2"]);
    }

    #[test]
    fn a_directory_makes_no_session_until_zellij_says_which_ended() {
        let said = |picker: &Picker| {
            let buffer = drawn(picker);
            text(&buffer, buffer.area).join(" ")
        };
        let dirs = || vec![visited("/home/you/notes", "notes"), visited("/home/you/bipa", "bipa")];
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
        picker.heard(heard_here(Ok(dirs())));
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "bipa");
        let why = "zellij has yet to say which sessions ended";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        picker.heard(kept_here(Ok(vec![ended("bipa", 100)])));
        assert_eq!(lit(&picker), "bipa-2");
        let session = SessionName::parse("bipa-2").unwrap();
        let cwd = directory("/home/you/bipa");
        let made = Picked::Make { on: Machine::Local, session, cwd };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);

        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
        let failed = Err("zellij: it did not answer in time".to_owned());
        picker.heard(kept_here(failed));
        picker.heard(heard_here(Ok(dirs())));
        let why = "zellij did not say which sessions ended";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
    }

    #[test]
    fn a_search_finds_a_directory_by_its_path_or_its_name() {
        let far = "/home/you/Projects/misc/far/away/down/luneta/main";
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
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
        let mut picker = Picker::open(1, List::Dirs, two_machines(), Machine::Local);
        let dirs = |on: Machine, ask: u64| Ask::Dirs { on, ask };
        let exited = |on: Machine, ask: u64| Ask::Exited { on, ask };
        let asks = vec![
            exited(Machine::Local, 1),
            exited(nixos(), 2),
            dirs(Machine::Local, 1),
            dirs(nixos(), 2),
        ];
        assert_eq!(picker.asks(), asks);
        picker.heard(kept_here(Ok(Vec::new())));
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
        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos());
        picker.heard(here());
        assert_eq!(lit(&picker), "notes");
        picker.heard(there());
        assert_eq!(lit(&picker), "api");
        press(&mut picker, KeyCode::Down);
        picker.listed(two_machines());
        assert_eq!(lit(&picker), "blog");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos());
        picker.heard(there());
        picker.heard(here());
        assert_eq!(lit(&picker), "api");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos());
        picker.heard(here());
        press(&mut picker, KeyCode::Up);
        picker.heard(there());
        assert_eq!(lit(&picker), "notes");

        let mut picker = Picker::open(1, List::Dirs, two_machines(), nixos());
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
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
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
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
        picker.heard(heard_here(Err("zoxide is not on the PATH".to_owned())));
        assert_eq!(list(&picker).0, ["zoxide is not on the PATH", "no directories"]);
    }

    #[test]
    fn each_machine_s_zellij_is_asked_which_sessions_ended_as_it_is_listed() {
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: Machine::Local, ask: 1 }]);
        picker.heard(kept_here(Ok(Vec::new())));
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: nixos(), ask: 2 }]);
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [] as [Ask; 0]);

        let gone = || Listing { remotes: Vec::new(), ..two_machines() };
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: nixos(), ask: 3 }]);
    }

    #[test]
    fn a_project_of_another_machine_takes_no_name_here() {
        let shelves = vec![shelved(nixos(), vec![housed("api", 1, Vec::new())])];
        let listing = Listing { shelves, ..two_machines() };
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local);
        picker.heard(kept_here(Ok(Vec::new())));
        picker.heard(heard_here(Ok(vec![visited("/home/you/api", "api")])));
        assert_eq!(grouped(&picker), ["── desk", "api"]);
    }

    #[test]
    fn luneta_s_agent_on_another_machine_is_pinned_over_that_machine_s_rows() {
        let at = |pane| PanePlace { session: listed("api", 40), pane };
        let there = agents(vec![
            seated("api", 40, 9, "mine", Mark::Busy),
            seated("api", 40, 2, "fix", Mark::Idle),
        ]);
        let listing = Listing {
            here: Some(Here::Pane { on: nixos(), place: at(PaneKey::Zellij(9)) }),
            local: agents(vec![seated("api", 30, 1, "fix", Mark::Idle)]),
            ..on_nixos(there)
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local);
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
        let fix = Picked::Go { on: nixos(), place: Place::Pane(at(PaneKey::Zellij(2))) };
        assert_eq!(press(&mut picker, KeyCode::Enter), fix);
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), [Ask::Closes { on: nixos(), place: at(PaneKey::Zellij(2)) }]);
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
    fn a_project_row_s_pane_closes_alone_by_key_with_no_word_to_zellij() {
        let projected = Projected::default();
        let session = Listed::new_project("work", 7);
        let place = PanePlace { session: session.clone(), pane: PaneKey::terminal(4) };
        let close = Ask::Closes { on: Machine::Local, place };
        let Heard::Closes { closes, .. } = asked(close, &projected) else {
            panic!("a close is answered with its plan");
        };
        assert_eq!(closes, Ok(Closes::Pane));
        let far = Machine::Remote("far".parse().unwrap());
        let peek = Ask::Peek { on: far, session, asked: Some(PaneKey::terminal(4)) };
        let peeked = asked(peek, &projected);
        let Heard::Peek { peek, .. } = peeked else {
            panic!("a peek is answered with a screen");
        };
        assert_eq!(peek, Err(crate::host::NOT_YET.to_owned()));
    }
}
