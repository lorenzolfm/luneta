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
use lune_domain::mux::TerminalKey;
use lune_domain::names::SessionName;
use lune_domain::projects::{self, OwnedFolder, ProjectId, ProjectName, Used, WorktreeId};
use lune_domain::rows::Unopened;
use lune_protocol::said::Said;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use luned::links::{Branch, Work};
use luned::repositories::{self, Repository};
use luned::transcripts::SessionId;
use luned::{Destination, Directory, screens};

use crate::actions::{self, Action, Pasted, Spec};
use crate::alerts::Doing;
use crate::cadence::Cadence;
use crate::close::{self, Act, Plan};
use crate::fuzzy::{Fuzzy, Score, Typos};
use crate::host::Event;
use crate::machines::Machine;
use crate::menu::{self, Chain, Chained, Done, Hint, Origin, Pull, Target};
use crate::moonlight;
use crate::projected::{self, Projected};
use crate::reach::{self, Luned};
use crate::screen;
use crate::sidebar::{
    self, ACCENT, ERROR, Finder, Fit, Keep, LIT, Listed, Mark, NAME, Place, ProjectRow,
    SpinnerFrame, TerminalPlace, ViewKey, ViewPlace, WorktreePlace,
};
use crate::spots::{Drawn, Spots};
use crate::worktrees::Folder;

const TITLE: &str = "lune";

const PREVIEW_DELAY: Duration = Duration::from_millis(200);

const PEEKS_MAX: usize = 32;

const PEEK_LINES_MAX: usize = 200;

const PEEK_COLUMNS_MAX: usize = 1000;

const INPUT_HEIGHT: u16 = 3;

const MIN_RESULTS: u16 = 3;

const MIN_HALF: u16 = 26;

const MIN_ACTION: usize = 12;

const UNOPENED: &str = "not open";

const HERE: &str = " here";

const NO_PROJECTS: &str = "no project, and no clone wt made: type a folder's path to make one";

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
    pub holds: Holds,
}

#[derive(Clone, Debug)]
pub enum Holds {
    Folder(Worktree),
    Clone {
        worktrees: Vec<Worktree>,
        unopened: Unopened,
    },
}

impl Housed {
    fn place(&self, worktree: WorktreeId) -> WorktreePlace {
        WorktreePlace {
            project: self.project.clone(),
            worktree,
        }
    }

    fn worktrees(&self) -> &[Worktree] {
        match &self.holds {
            Holds::Folder(own) => std::slice::from_ref(own),
            Holds::Clone { worktrees, .. } => worktrees,
        }
    }

    fn unopened_folders(&self) -> &[projects::Dir] {
        match &self.holds {
            Holds::Clone {
                unopened: Unopened::Listed(folders),
                ..
            } => folders,
            Holds::Folder(_)
            | Holds::Clone {
                unopened: Unopened::Unlisted(_),
                ..
            } => &[],
        }
    }

    fn refuses_new_worktree(&self) -> Option<String> {
        match &self.holds {
            Holds::Folder(_) => Some(projects::folder_has_one_worktree(
                self.project.listed().name(),
            )),
            Holds::Clone { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Worktree {
    pub id: WorktreeId,
    pub named: sidebar::Named<Said>,
    pub folder: projects::Dir,
    pub used: Option<Used>,
    pub terminals: Vec<TerminalKey>,
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
            Here::View { place, .. } => place.listed().name().as_str(),
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
    pub place: TerminalPlace,
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
    At {
        target: Rc<Target>,
        worktree: Option<Rc<Target>>,
    },
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
    Projects,
}

impl List {
    #[must_use]
    pub fn opened_by(chord: actions::Chord) -> Option<List> {
        match chord {
            actions::PALETTE => Some(List::Actions),
            actions::WORKTREES => Some(List::Worktrees),
            actions::AGENTS => Some(List::Agents),
            actions::PROJECTS => Some(List::Projects),
            _ => None,
        }
    }

    fn next(self) -> List {
        match self {
            List::Actions => List::Worktrees,
            List::Worktrees => List::Agents,
            List::Agents => List::Projects,
            List::Projects => List::Actions,
        }
    }

    fn back(self) -> List {
        match self {
            List::Actions => List::Projects,
            List::Worktrees => List::Actions,
            List::Agents => List::Worktrees,
            List::Projects => List::Agents,
        }
    }

    fn previews(self) -> bool {
        match self {
            List::Actions => false,
            List::Worktrees | List::Agents | List::Projects => true,
        }
    }

    fn title(self) -> &'static str {
        match self {
            List::Actions => "Actions",
            List::Worktrees => "Worktrees",
            List::Agents => "Agents",
            List::Projects => "Projects",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    Close,
    Go {
        on: Machine,
        place: Place,
    },
    Add {
        on: Machine,
        project: ProjectRow,
        folder: Directory,
    },
    Make {
        on: Machine,
        folder: Folder,
    },
    Plan {
        on: Machine,
        session: Listed,
        scope: close::Scope,
    },
    Act {
        on: Machine,
        act: Act,
    },
    Chord(actions::Chord),
    Menu {
        target: Target,
        origin: Origin,
        action: Action,
    },
    Row {
        target: Target,
        action: Action,
    },
    Pull {
        target: Target,
        work: Work,
        pull: Pull,
        origin: Origin,
    },
    Done {
        done: Done,
        origin: Origin,
    },
}

pub enum Heard {
    Peek {
        on: Machine,
        session: Listed,
        asked: Option<TerminalKey>,
        peek: Result<(u32, String), String>,
    },
    Repositories {
        on: Machine,
        ask: u64,
        found: Result<repositories::Found, String>,
    },
    Listed {
        on: Machine,
        path: Directory,
        listed: Result<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    Peek {
        on: Machine,
        session: Listed,
        asked: Option<TerminalKey>,
    },
    Repositories {
        on: Machine,
        ask: u64,
    },
    List {
        on: Machine,
        path: Directory,
    },
}

pub fn ask(number: u64, ask: Ask, projected: &Projected, events: &Sender<Event>) {
    let events = events.clone();
    let hear: Box<dyn FnOnce() -> Heard + Send> = match ask {
        Ask::Peek { on, session, asked } => {
            let terminal = match asked {
                Some(terminal) => projected::route_terminal(&on, terminal),
                None => projected.whole(&on, &session),
            };
            Box::new(move || {
                let peek = terminal
                    .and_then(|key| screens::terminal(key).map(|screen| (key.id.get(), screen)));
                Heard::Peek {
                    on,
                    session,
                    asked,
                    peek,
                }
            })
        }
        Ask::Repositories { on, ask } => Box::new(move || {
            let found = Luned::on(&on, reach::ANSWER).and_then(|mut luned| luned.repositories());
            Heard::Repositories { on, ask, found }
        }),
        Ask::List { on, path } => Box::new(move || {
            let listed = Luned::on(&on, reach::ANSWER).and_then(|mut luned| luned.contents(&path));
            Heard::Listed { on, path, listed }
        }),
    };
    thread::spawn(move || {
        let heard = hear();
        let _ = events.send(Event::Picker { number, heard });
    });
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Key(KeyEvent),
    Row(Key),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Worktree {
        on: Machine,
        place: WorktreePlace,
    },
    Project {
        on: Machine,
        project: ProjectRow,
        lone: Lone,
    },
    Unopened {
        on: Machine,
        project: ProjectRow,
        folder: Directory,
    },
    Machine {
        on: Machine,
    },
    Agent {
        on: Machine,
        place: TerminalPlace,
    },
    Housed {
        on: Machine,
        dir: projects::Dir,
        goes: Goes,
    },
    Repository {
        on: Machine,
        repository: Repository,
        name: ProjectName,
    },
    Folder {
        on: Machine,
        folder: Folder,
    },
    New {
        on: Machine,
        project: ProjectRow,
        branch: Branch,
    },
    Action(Action, Run),
    Pasted {
        target: Rc<Target>,
        work: Work,
        pull: Pull,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Goes {
    Home(ProjectRow),
    Last(WorktreePlace),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lone {
    Clone,
    NoneOpen,
    Unlisted(String),
}

impl Lone {
    fn of(unopened: &Unopened) -> Lone {
        match unopened {
            Unopened::Listed(folders) if folders.is_empty() => Lone::Clone,
            Unopened::Listed(_) => Lone::NoneOpen,
            Unopened::Unlisted(why) => Lone::Unlisted(why.clone()),
        }
    }

    fn tag(&self) -> &'static str {
        match self {
            Lone::Clone => "no worktree",
            Lone::NoneOpen => "none open",
            Lone::Unlisted(_) => "not listed",
        }
    }
}

mod label {
    #[derive(Clone, Debug)]
    pub(super) struct Named {
        text: String,
        own_start: usize,
    }

    impl Named {
        pub(super) fn whole(text: String) -> Self {
            Self { text, own_start: 0 }
        }

        pub(super) fn placed(project: &str, own: &str) -> Self {
            let mut text = String::with_capacity(project.len() + " › ".len() + own.len());
            text.push_str(project);
            text.push_str(" › ");
            let own_start = text.len();
            text.push_str(own);
            Self { text, own_start }
        }

        pub(super) fn shown(&self) -> &str {
            &self.text
        }

        pub(super) fn own(&self) -> &str {
            &self.text[self.own_start..]
        }
    }
}

#[derive(Clone, Debug)]
struct Row {
    key: Key,
    name: label::Named,
    hits: Vec<usize>,
    rules: Rules,
}

impl Row {
    fn plain(key: Key, name: String) -> Row {
        Row::named(key, label::Named::whole(name), Vec::new())
    }

    fn named(key: Key, name: label::Named, hits: Vec<usize>) -> Row {
        Row {
            key,
            name,
            hits,
            rules: Rules::default(),
        }
    }

    fn shown(&self) -> &str {
        self.name.shown()
    }

    fn own(&self) -> &str {
        self.name.own()
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
            usize::from(matches!(
                rows.first().map(|row| &row.key),
                Some(Key::New { .. })
            ))
        };
        let was = self.at.filter(|_| held.is_some());
        let was = was.map_or(start, |was| {
            (was + over(&rows)).saturating_sub(over(&self.rows))
        });
        let last = rows.len().checked_sub(1);
        self.at = at.or_else(|| last.map(|last| was.min(last)));
        self.rows = rows;
        assert_eq!(
            self.at.is_some(),
            !self.rows.is_empty(),
            "a row is lit while there is one"
        );
        if let Some(at) = self.at {
            assert!(
                at < self.rows.len(),
                "the lit row {at} is one of {}",
                self.rows.len()
            );
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.at?)
    }

    fn light(&mut self, fits: impl Fn(&Key) -> bool) {
        if let Some(at) = self.rows.iter().position(|row| fits(&row.key)) {
            self.at = Some(at);
        }
        assert_eq!(
            self.at.is_some(),
            !self.rows.is_empty(),
            "a row is lit while there is one"
        );
    }

    fn step(&mut self, by: isize) {
        let Some(at) = self.at else {
            return;
        };
        let last = self.rows.len().saturating_sub(1);
        let to = if by < 0 {
            at.saturating_sub(by.unsigned_abs())
        } else {
            at + by.unsigned_abs()
        };
        self.at = Some(to.min(last));
    }
}

#[derive(Debug)]
enum Stage {
    Search,
    Planning {
        on: Machine,
        session: Listed,
        scope: close::Scope,
        agreed: Option<Plan>,
    },
    Asking {
        on: Machine,
        session: Listed,
        scope: close::Scope,
        plan: Plan,
        asks: String,
    },
    Chain {
        chain: Chain,
        on: Machine,
        aimed: Aimed,
    },
}

#[derive(Debug)]
enum Aimed {
    Palette,
    Project(ProjectRow),
    Worktree { place: WorktreePlace, name: String },
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
        let end = used
            .iter()
            .rposition(|used| *used)
            .map_or(first, |last| last + 1);
        Shot {
            screen: Box::new(screen),
            first,
            end,
        }
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
        Peek::Ready {
            shot: Shot::of(lines),
        }
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
        let entries: Vec<&str> = listed
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        let shot = Shot::of(&entries[..entries.len().min(PEEK_LINES_MAX)]);
        Contents::Ready {
            total: entries.len(),
            shot,
        }
    }
}

type Looked = (Machine, Listed, Option<TerminalKey>);

type Located = (Machine, Directory);

type Found = (Option<Used>, label::Named, Vec<String>, Key);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    Blank,
    Rule,
}

impl Fill {
    fn least(self) -> usize {
        match self {
            Fill::Blank => 1,
            Fill::Rule => 1 + "─".width() + HERE.width() + 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Preview {
    Screen(Looked),
    Dir(Located),
}

#[derive(Debug)]
enum Lookup {
    Asking(u64),
    Listed(repositories::Found),
    Failed(String),
}

#[derive(Debug)]
pub struct Picker {
    number: u64,
    list: List,
    term: String,
    stage: Stage,
    listing: Listing,
    lookups: HashMap<Machine, Lookup>,
    lookups_sent: u64,
    worktrees: Rows,
    agents: Rows,
    projects: Rows,
    peeks: HashMap<Looked, Peek>,
    contents: HashMap<Located, Contents>,
    wanted: Option<(Preview, Instant)>,
    error: Option<String>,
    asking: Vec<Ask>,
    aim: Machine,
    aiming: bool,
    scope: Option<Scope>,
    palette: Palette,
    actions: Rows,
}

#[derive(Debug)]
struct Scope {
    on: Machine,
    project: ProjectId,
    back: Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Back {
    Close,
    To(List),
}

impl Picker {
    pub fn opening(
        number: u64,
        listing: Listing,
        (on, project): (Machine, ProjectId),
        palette: Palette,
    ) -> Picker {
        let mut picker = Picker::open(number, List::Worktrees, listing, on.clone(), palette);
        picker.scope = Some(Scope {
            on,
            project,
            back: Back::Close,
        });
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
            lookups: HashMap::new(),
            lookups_sent: 0,
            worktrees: Rows::default(),
            agents: Rows::default(),
            projects: Rows::default(),
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
        picker.ask_repositories();
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
        self.lookups.retain(|on, _| machines.contains(on));
        self.ask_repositories();
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
                remotes
                    .find(|(at, _)| at == destination)
                    .map(|(_, runs)| runs)
            }
        }
    }

    fn shelved(&self, on: &Machine) -> Option<&Shelved> {
        self.listing
            .shelves
            .iter()
            .find(|shelved| shelved.on == *on)
    }

    fn projects_on(&self, on: &Machine) -> &[Housed] {
        self.shelved(on)
            .map_or(&[][..], |shelved| &shelved.projects)
    }

    fn found_on(&self, on: &Machine) -> &[Repository] {
        match self.lookups.get(on) {
            Some(Lookup::Listed(found)) => &found.repositories,
            Some(Lookup::Asking(_) | Lookup::Failed(_)) | None => &[],
        }
    }

    fn owned_on(&self, on: &Machine) -> Option<&OwnedFolder> {
        match self.lookups.get(on) {
            Some(Lookup::Listed(found)) => found.owned.as_ref(),
            Some(Lookup::Asking(_) | Lookup::Failed(_)) | None => None,
        }
    }

    fn housed(&self, on: &Machine, project: &ProjectRow) -> Option<&Housed> {
        let projects = &self.shelved(on)?.projects;
        projects
            .iter()
            .find(|housed| housed.project.id() == project.id())
    }

    fn worktree(&self, on: &Machine, place: &WorktreePlace) -> Option<&Worktree> {
        let worktrees = self.housed(on, &place.project)?.worktrees();
        worktrees
            .iter()
            .find(|worktree| worktree.id == place.worktree)
    }

    fn row_worktree(&self, on: &Machine, place: &WorktreePlace) -> &Worktree {
        let worktree = self.worktree(on, place);
        worktree.expect("a worktree's row is rebuilt with the listing that holds it")
    }

    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        self.shelved(on)?.away?;
        Some(format!(
            "luned does not reach {} now",
            self.machine_name(on)
        ))
    }

    fn is_shown(&self, on: &Machine, place: &WorktreePlace) -> bool {
        self.listing
            .shown
            .as_ref()
            .is_some_and(|(at, shown)| at == on && shown == place)
    }

    fn focus(&self, on: &Machine, worktree: &Worktree) -> Option<TerminalKey> {
        let here = self
            .here_view(on)
            .and_then(|place| place.view().terminal_key());
        let focused = here.filter(|terminal| worktree.terminals.contains(terminal));
        focused.or_else(|| worktree.terminals.first().copied())
    }

    fn seated(&self, on: &Machine, place: &TerminalPlace) -> Option<&Seated> {
        self.runs(on)?
            .agents
            .iter()
            .find(|seated| seated.place == *place)
    }

    fn ask_repositories(&mut self) {
        for on in self.machines() {
            if !self.lookups.contains_key(&on) {
                self.lookups_sent += 1;
                let ask = self.lookups_sent;
                self.lookups.insert(on.clone(), Lookup::Asking(ask));
                self.asking.push(Ask::Repositories { on, ask });
            }
        }
    }

    pub fn heard(&mut self, heard: Heard) -> Picked {
        match heard {
            Heard::Peek {
                on,
                session,
                asked,
                peek,
            } => {
                let peek = match peek {
                    Ok((_, dumped)) => Peek::of(&dumped),
                    Err(why) => Peek::Failed(why),
                };
                self.peeks.insert((on, session, asked), peek);
            }
            Heard::Repositories { on, ask, found } => {
                if !matches!(self.lookups.get(&on), Some(Lookup::Asking(asked)) if *asked == ask) {
                    return Picked::Nothing;
                }
                let lookup = match found {
                    Ok(found) => Lookup::Listed(found),
                    Err(why) => Lookup::Failed(why),
                };
                self.lookups.insert(on, lookup);
                self.rebuild(false);
            }
            Heard::Listed { on, path, listed } => {
                let contents = match listed {
                    Ok(listed) => Contents::of(&listed),
                    Err(why) => Contents::Failed(why),
                };
                self.contents.insert((on, path), contents);
            }
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
            Stage::Planning {
                on,
                session,
                scope,
                agreed,
            } => {
                if key.code != KeyCode::Esc {
                    self.stage = Stage::Planning {
                        on,
                        session,
                        scope,
                        agreed,
                    };
                }
                Picked::Nothing
            }
            Stage::Asking {
                on,
                session,
                scope,
                plan,
                ..
            } => {
                let yes = key.code == KeyCode::Char('y') && plain(key);
                if yes {
                    self.plan(on, session, scope, Some(plan))
                } else {
                    Picked::Nothing
                }
            }
            Stage::Chain { .. } if (key.code, key.modifiers) == actions::PALETTE => Picked::Close,
            Stage::Chain { chain, on, aimed } => match chain.key(key, &on) {
                Chained::Typing(chain) => {
                    self.stage = Stage::Chain { chain, on, aimed };
                    Picked::Nothing
                }
                Chained::Back => Picked::Nothing,
                Chained::Close => Picked::Close,
                Chained::Done(done) => Picked::Done {
                    done,
                    origin: self.palette.origin,
                },
            },
        };
        self.follow();
        picked
    }

    pub fn paste(&mut self, text: &str) {
        self.aiming = false;
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match &mut self.stage {
            Stage::Search => self.edit_term(|term| {
                term.push_str(&text);
                true
            }),
            Stage::Chain { chain, .. } => chain.paste(&text),
            Stage::Planning { .. } | Stage::Asking { .. } => {}
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
        let Stage::Planning {
            on,
            session,
            scope,
            agreed,
        } = std::mem::replace(&mut self.stage, Stage::Search)
        else {
            return Picked::Nothing;
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(why) => {
                self.error = Some(why);
                return Picked::Nothing;
            }
        };
        let asks = if agreed.as_ref() == Some(&plan) {
            None
        } else {
            plan.question()
        };
        if let Some(asks) = asks {
            self.stage = Stage::Asking {
                on,
                session,
                scope,
                plan,
                asks,
            };
            return Picked::Nothing;
        }
        Picked::Act {
            on,
            act: plan.into_act(),
        }
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
            }
            Preview::Dir((on, path)) => {
                if self.contents.len() >= PEEKS_MAX {
                    self.contents.clear();
                }
                self.contents
                    .insert((on.clone(), path.clone()), Contents::Reading);
                self.asking.push(Ask::List { on, path });
            }
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
        let (fewest, most) = (
            over.width.min(PALETTE_WIDTH_MIN),
            over.width.min(PALETTE_WIDTH_MAX),
        );
        let width = three_fifths.clamp(fewest, most);
        let height = PALETTE_HEIGHT.min(over.height);
        let x = over.x + (over.width - width) / 2;
        let y = (over.y + over.height / 5).min(over.bottom() - height);
        let area = Rect::new(x, y, width, height);
        assert!(
            area.right() <= over.right(),
            "the palette fits the width it is drawn over"
        );
        assert!(
            area.bottom() <= over.bottom(),
            "the palette fits the height it is drawn over"
        );
        area
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, wall: SystemTime) -> (Drawn, Cadence) {
        let mut cadence = Cadence::default();
        let palette = self.list == List::Actions;
        let area = if palette {
            Picker::compact(over)
        } else {
            Picker::area(over)
        };
        let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2))
        else {
            return (
                Drawn::Picker {
                    number: self.number,
                    area,
                    spots: Spots::default(),
                },
                cadence,
            );
        };
        framed(buffer, area, TITLE, "", Vec::new());
        let layout = Layout::of(Rect::new(area.x + 1, area.y + 1, width, height));
        let layout = if self.list.previews() {
            layout
        } else {
            Layout {
                results: layout.full,
                preview: None,
                ..layout
            }
        };
        if let Stage::Chain { chain, on, aimed } = &self.stage {
            let aimed = match aimed {
                Aimed::Palette => self.aimed_at(),
                Aimed::Project(project) => {
                    let named = self.housed(on, project).map_or(project, |it| &it.project);
                    there(named.listed().name().as_str(), on)
                }
                Aimed::Worktree { place, name } => {
                    let worktree = self.worktree(on, place);
                    there(worktree.map_or(name.as_str(), |it| it.named.name()), on)
                }
            };
            let spots = self.draw_chain(buffer, &layout, chain, &aimed);
            return (
                Drawn::Picker {
                    number: self.number,
                    area,
                    spots,
                },
                cadence,
            );
        }
        let mut spots = Spots::default();
        if let Some(results) = layout.results {
            let inner = usize::from(results.width.saturating_sub(2));
            let rows = usize::from(results.height.saturating_sub(2));
            let notes = self.notes(inner);
            let (body, picks, body_cadence) =
                self.body(inner, rows.saturating_sub(notes.len()), wall);
            cadence.merge(body_cadence);
            let count = count(self.rows().at, self.rows().rows.len());
            let (lines, body_at) = anchored(notes, body, rows);
            framed(buffer, results, self.list.title(), &count, lines);
            spots.extend(picks.down(body_at).placed(results.inner(Margin::new(1, 1))));
        }
        if let Some(preview) = layout.preview {
            cadence.merge(self.draw_preview(buffer, preview, wall));
        }
        let prompt = self.prompt(usize::from(layout.input.width.saturating_sub(2)));
        let title = if palette {
            self.aimed_at()
        } else {
            self.list.title().to_owned()
        };
        draw_input(buffer, &layout, &title, prompt);
        spots.extend(self.draw_help(buffer, layout.help));
        (
            Drawn::Picker {
                number: self.number,
                area,
                spots,
            },
            cadence,
        )
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
            framed(
                buffer,
                full,
                chain.title(),
                "",
                menu::chain_says(chain, inner),
            );
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
            List::Projects => &self.projects,
        }
    }

    fn rows_mut(&mut self) -> &mut Rows {
        match self.list {
            List::Actions => &mut self.actions,
            List::Worktrees => &mut self.worktrees,
            List::Agents => &mut self.agents,
            List::Projects => &mut self.projects,
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
            KeyCode::Esc => self.escape(),
            KeyCode::Tab => {
                self.list = self.list.next();
                Picked::Nothing
            }
            KeyCode::BackTab => {
                self.list = self.list.back();
                Picked::Nothing
            }
            KeyCode::Char('j') if held_super => {
                self.list = List::Worktrees;
                Picked::Nothing
            }
            KeyCode::Char('b') if control => {
                self.list = List::Agents;
                Picked::Nothing
            }
            KeyCode::Char('o') if held_super => {
                self.list = List::Projects;
                Picked::Nothing
            }
            KeyCode::Char('r') if control => {
                self.rename();
                Picked::Nothing
            }
            KeyCode::Enter => self.enter(),
            KeyCode::Delete => self.close_selected(),
            KeyCode::Down => {
                self.rows_mut().step(1);
                Picked::Nothing
            }
            KeyCode::Up => {
                self.rows_mut().step(-1);
                Picked::Nothing
            }
            KeyCode::Backspace => {
                self.edit_term(|term| term.pop().is_some());
                Picked::Nothing
            }
            KeyCode::Char(c) if plain(key) => {
                self.edit_term(|term| {
                    term.push(c);
                    true
                });
                Picked::Nothing
            }
            _ => Picked::Nothing,
        }
    }

    fn escape(&mut self) -> Picked {
        let Some(Scope {
            on,
            project,
            back: Back::To(list),
        }) = self
            .scope
            .take_if(|scope| matches!(scope.back, Back::To(_)))
        else {
            return Picked::Close;
        };
        self.list = list;
        self.term.clear();
        self.rebuild(true);
        let row = |key: &Key| matches!(key, Key::Project { on: at, project: row, .. } if *at == on && row.id() == project);
        self.rows_mut().light(row);
        Picked::Nothing
    }

    fn enter(&mut self) -> Picked {
        let Some(row) = self.rows().selected() else {
            return Picked::Nothing;
        };
        match &row.key {
            Key::Worktree { on, place } if self.is_shown(on, place) => Picked::Close,
            Key::Worktree { on, place } => Picked::Go {
                on: on.clone(),
                place: Place::Worktree(place.clone()),
            },
            Key::Project { on, project, lone } => match lone {
                Lone::Clone => {
                    let (chain, on) = (Chain::branch(project.id()), on.clone());
                    let aimed = Aimed::Project(project.clone());
                    self.stage = Stage::Chain { chain, on, aimed };
                    Picked::Nothing
                }
                Lone::NoneOpen => {
                    let (on, project) = (on.clone(), project.id());
                    self.scope = Some(Scope {
                        on,
                        project,
                        back: Back::To(self.list),
                    });
                    self.list = List::Worktrees;
                    self.term.clear();
                    self.rebuild(true);
                    let listed = self.worktrees.rows.len();
                    assert!(
                        listed > 0,
                        "a project with none open lists its worktrees not open"
                    );
                    Picked::Nothing
                }
                Lone::Unlisted(_) => Picked::Nothing,
            },
            Key::Unopened {
                on,
                project,
                folder,
            } => {
                let (on, project, folder) = (on.clone(), project.clone(), folder.clone());
                Picked::Add {
                    on,
                    project,
                    folder,
                }
            }
            Key::Machine { .. } => Picked::Nothing,
            Key::Agent { on, place } => Picked::Go {
                on: on.clone(),
                place: Place::View(place.view_place()),
            },
            Key::Housed {
                on,
                goes: Goes::Last(place),
                ..
            } if self.is_shown(on, place) => Picked::Close,
            Key::Housed { on, goes, .. } => {
                let place = match goes {
                    Goes::Last(place) => Place::Worktree(place.clone()),
                    Goes::Home(project) => Place::Session(project.listed().clone()),
                };
                Picked::Go {
                    on: on.clone(),
                    place,
                }
            }
            Key::Repository { on, repository, .. } => Picked::Make {
                on: on.clone(),
                folder: Folder::from(repository.path()),
            },
            Key::Folder { on, folder, .. } => Picked::Make {
                on: on.clone(),
                folder: folder.clone(),
            },
            Key::New {
                on,
                project,
                branch,
            } => {
                let done = Done::Worktree(on.clone(), project.id(), branch.clone());
                Picked::Done {
                    done,
                    origin: self.palette.origin,
                }
            }
            Key::Action(action, run) => {
                let (action, run) = (*action, run.clone());
                self.act(action, run)
            }
            Key::Pasted { target, work, pull } => {
                let parsed = Work::parse(&self.term, target.cwd().cloned());
                assert_eq!(
                    parsed.as_ref(),
                    Some(work),
                    "pasted rows are rebuilt with the term"
                );
                let (target, work, pull) = (Target::clone(target), work.clone(), *pull);
                Picked::Pull {
                    target,
                    work,
                    pull,
                    origin: self.palette.origin,
                }
            }
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
            }
            Run::Row(target) => Picked::Row {
                target: Target::clone(&target),
                action,
            },
            Run::Menu(target) => {
                if let Some(why) = self.refused(&target, action) {
                    self.error = Some(why);
                    return Picked::Nothing;
                }
                if let Some(chain) = Chain::of(&target, action) {
                    let on = target.machine().clone();
                    self.stage = Stage::Chain {
                        chain,
                        on,
                        aimed: Aimed::Palette,
                    };
                    return Picked::Nothing;
                }
                Picked::Menu {
                    target: Target::clone(&target),
                    origin: self.palette.origin,
                    action,
                }
            }
        }
    }

    fn refused(&self, target: &Target, action: Action) -> Option<String> {
        if action != Action::NewWorktree {
            return None;
        }
        let housed = self.housed(target.machine(), &target.project()?)?;
        housed.refuses_new_worktree()
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
                if action.fits(target.noun()) {
                    target.fits(action).then(|| Run::Menu(Rc::clone(target)))
                } else {
                    worktree
                        .as_ref()
                        .filter(|it| it.fits(action))
                        .map(|it| Run::Menu(Rc::clone(it)))
                }
            }
            (actions::Key::Row(_), Aim::At { target, .. }, Origin::Sidebar) => action
                .fits(target.noun())
                .then(|| Run::Row(Rc::clone(target))),
            (actions::Key::Row(_), Aim::At { .. }, Origin::Views)
            | (
                actions::Key::Menu(_) | actions::Key::RowAndMenu(_) | actions::Key::Row(_),
                Aim::Unplaced(_),
                _,
            )
            | (actions::Key::Pasted(_), ..) => None,
        }
    }

    fn action_rows(&self, finder: Option<&Finder>) -> Vec<Row> {
        if let Some(pasted) = self.pasted_rows() {
            return pasted;
        }
        let fitting = Action::ALL
            .into_iter()
            .filter_map(|it| Some((it, self.run(it)?)));
        let worktrees = self.listed_worktrees(finder);
        let worktrees = worktrees
            .into_iter()
            .filter(|row| !matches!(row.key, Key::Machine { .. }));
        let Some(finder) = finder else {
            let row = |(action, run)| Row::plain(Key::Action(action, run), titled(action));
            let rows = fitting.map(row);
            return rows.chain(worktrees).collect();
        };
        let mut found: Vec<(Tier, Fit, Row)> = fitting
            .filter_map(|(action, run)| found_action(finder, action, run))
            .collect();
        found.sort_by_key(|(tier, fit, _)| (*tier, Reverse(*fit)));
        let actions = found
            .into_iter()
            .map(|(tier, _, row)| (tier, Kind::Action, row));
        let place = |row: Row| {
            (
                tier_of(finder, row.shown(), Some(row.own())),
                Kind::Place,
                row,
            )
        };
        let places = worktrees.map(place);
        let mut ranked: Vec<(Tier, Kind, Row)> = actions.chain(places).collect();
        ranked.sort_by_key(|(tier, kind, _)| (*tier, *kind));
        ranked.into_iter().map(|(.., row)| row).collect()
    }

    fn pasted_rows(&self) -> Option<Vec<Row>> {
        let Aim::At { target, .. } = &self.palette.aim else {
            return None;
        };
        let work = Work::parse(&self.term, target.cwd().cloned())?;
        let row = |(pull, pasted): (Pull, Pasted)| {
            let name = format!("{} {}", titled(pasted.action()), work.tab());
            let (target, work) = (Rc::clone(target), work.clone());
            Row::plain(Key::Pasted { target, work, pull }, name)
        };
        Some(Pull::offered(&work).into_iter().map(row).collect())
    }

    fn aimed_at(&self) -> String {
        let target = match &self.palette.aim {
            Aim::At { target, .. } => target.as_ref(),
            Aim::Unplaced(_) => return List::Actions.title().to_owned(),
        };
        let named = match target {
            Target::View(on, row) => {
                let project = row.listed().name();
                let view = match row.view() {
                    ViewKey::Terminal(key) => format!("view {}", key.id),
                    ViewKey::Browser(_) => "browser".to_owned(),
                };
                let housed = self.housed(on, &row.worktree.project);
                let mut worktrees = housed.iter().flat_map(|housed| housed.worktrees());
                match worktrees.find(|worktree| worktree.id == row.worktree.worktree) {
                    Some(worktree) => {
                        let named = in_project(project, worktree.named.name());
                        format!("{named} · {view}")
                    }
                    None => format!("{project} · {view}"),
                }
            }
            Target::Worktree(_, row) => in_project(row.place.project.listed().name(), &row.name),
            Target::Project(_, project) => project.project.listed().name().to_string(),
            Target::Machine(on) => return self.machine_name(on),
        };
        there(&named, target.machine())
    }

    fn here_project(&self) -> Option<(&Machine, &Housed)> {
        let (on, place) = self.listing.shown.as_ref()?;
        Some((on, self.housed(on, &place.project)?))
    }

    fn typed_rows(&self) -> Vec<Row> {
        let named = self.unopened_named();
        if !named.is_empty() {
            return named;
        }
        match self.new_key() {
            Some(Ok(key)) => vec![Row::plain(key, self.term.clone())],
            Some(Err(_)) | None => Vec::new(),
        }
    }

    fn unopened_named(&self) -> Vec<Row> {
        let Some((on, housed)) = self.here_project() else {
            return Vec::new();
        };
        let named = housed
            .unopened_folders()
            .iter()
            .filter(|folder| folder.last() == self.term);
        let found: Vec<Found> = named
            .map(|folder| unopened_found(on, housed, folder))
            .collect();
        matched(None, candidates_of(&found))
    }

    fn new_key(&self) -> Option<Result<Key, String>> {
        let (on, housed) = self.here_project()?;
        if let Some(why) = housed.refuses_new_worktree() {
            return Some(Err(why));
        }
        let (on, project) = (on.clone(), housed.project.clone());
        Some(Branch::parse(&self.term).map(|branch| Key::New {
            on,
            project,
            branch,
        }))
    }

    fn close_selected(&mut self) -> Picked {
        let Some(key) = self.rows().selected().map(|row| row.key.clone()) else {
            return Picked::Nothing;
        };
        match key {
            Key::Worktree { on, place } => {
                let session = place.project.listed().clone();
                self.plan(on, session, close::Scope::Worktree(place.worktree), None)
            }
            Key::Project { project, .. } => {
                let name = project.listed().name();
                self.error = Some(format!("{name} holds no worktree to close"));
                Picked::Nothing
            }
            Key::Agent { on, place } => self.plan(
                on,
                place.listed().clone(),
                close::Scope::View(ViewKey::Terminal(place.terminal())),
                None,
            ),
            Key::Unopened { .. } => {
                self.error = Some("a worktree not open has no views to close".to_owned());
                Picked::Nothing
            }
            Key::Machine { .. }
            | Key::Housed { .. }
            | Key::Repository { .. }
            | Key::Folder { .. }
            | Key::New { .. }
            | Key::Action(..)
            | Key::Pasted { .. } => Picked::Nothing,
        }
    }

    fn plan(
        &mut self,
        on: Machine,
        session: Listed,
        scope: close::Scope,
        agreed: Option<Plan>,
    ) -> Picked {
        let picked = Picked::Plan {
            on: on.clone(),
            session: session.clone(),
            scope,
        };
        self.stage = Stage::Planning {
            on,
            session,
            scope,
            agreed,
        };
        picked
    }

    fn rename(&mut self) {
        if self.list != List::Worktrees {
            return;
        }
        let (on, renames, aimed) = match self.worktrees.selected().map(|row| &row.key) {
            Some(Key::Worktree { on, place }) => {
                let name = self.row_worktree(on, place).named.name().to_owned();
                let renames = menu::Renames::Worktree(place.worktree);
                (
                    on.clone(),
                    renames,
                    Aimed::Worktree {
                        place: place.clone(),
                        name,
                    },
                )
            }
            Some(Key::Project { on, project, .. }) => {
                let renames = menu::Renames::Project(project.id());
                (on.clone(), renames, Aimed::Project(project.clone()))
            }
            Some(
                Key::Machine { .. }
                | Key::Agent { .. }
                | Key::Housed { .. }
                | Key::Repository { .. }
                | Key::Folder { .. }
                | Key::New { .. }
                | Key::Unopened { .. }
                | Key::Action(..)
                | Key::Pasted { .. },
            )
            | None => return,
        };
        assert!(
            matches!(self.stage, Stage::Search),
            "Ctrl r chains from the list alone"
        );
        self.stage = Stage::Chain {
            chain: Chain::rename(renames),
            on,
            aimed,
        };
    }

    fn taken(&self, on: &Machine) -> Vec<&str> {
        let projects = self
            .shelved(on)
            .into_iter()
            .flat_map(|shelved| &shelved.projects);
        let projects = projects.map(|housed| housed.project.listed().name().as_str());
        self.here_session(on).into_iter().chain(projects).collect()
    }

    fn edit_term(&mut self, edit: impl FnOnce(&mut String) -> bool) {
        if edit(&mut self.term) {
            self.rebuild(true);
        }
    }

    fn rebuild(&mut self, snap: bool) {
        let ruled = !self.listing.remotes.is_empty();
        let agents = Typos::found(|typos| {
            let finder = Finder::of(&self.term, typos);
            let mut agents = Vec::new();
            for (on, runs) in self.each() {
                let rule = ruled.then(|| self.machine_name(&on));
                let found = self.agent_rows(finder.as_ref(), &on, runs);
                grouped(&mut agents, found, rule);
            }
            agents
        });
        let mut worktrees = Typos::found(|typos| {
            let finder = Finder::of(&self.term, typos);
            self.worktree_rows(finder.as_ref())
        });
        if worktrees.is_empty() && self.scope.is_none() {
            worktrees = self.typed_rows();
        }
        let (projects, aimed) = self.project_rows();
        let mut actions =
            Typos::found(|typos| self.action_rows(Finder::of(&self.term, typos).as_ref()));
        if actions.is_empty() && !self.term.is_empty() {
            actions = self.typed_rows();
        }
        let worktree_start = match worktrees.first().map(|row| &row.key) {
            Some(Key::Worktree { on, place }) if self.is_shown(on, place) => {
                usize::from(self.term.is_empty())
            }
            Some(_) | None => 0,
        };
        let held = |rows: &Rows| rows.selected().map(|row| row.key.clone()).filter(|_| !snap);
        let (held_worktree, held_agent) = (held(&self.worktrees), held(&self.agents));
        let held_project = held(&self.projects).filter(|_| !self.aiming);
        let held_action = held(&self.actions);
        self.actions.replace(actions, held_action.as_ref(), 0);
        self.worktrees
            .replace(worktrees, held_worktree.as_ref(), worktree_start);
        self.agents.replace(agents, held_agent.as_ref(), 0);
        self.projects
            .replace(projects, held_project.as_ref(), aimed.unwrap_or(0));
        self.follow();
    }

    fn worktree_rows(&self, finder: Option<&Finder>) -> Vec<Row> {
        if let Some(Scope { on, project, .. }) = &self.scope {
            return self.worktree_rows_scoped(finder, on, *project);
        }
        self.listed_worktrees(finder)
    }

    fn listed_worktrees(&self, finder: Option<&Finder>) -> Vec<Row> {
        let tagged = self.tagged();
        let mut found = Vec::new();
        let mut machines = Vec::new();
        for shelved in &self.listing.shelves {
            let on = &shelved.on;
            if shelved.projects.is_empty() && (tagged || shelved.away.is_some()) {
                machines.push(Row::plain(
                    Key::Machine { on: on.clone() },
                    self.machine_name(on),
                ));
            }
            for housed in &shelved.projects {
                let project = housed.project.listed().name().as_str();
                if let Holds::Clone {
                    worktrees,
                    unopened,
                } = &housed.holds
                    && worktrees.is_empty()
                {
                    let (on, row) = (on.clone(), housed.project.clone());
                    let key = Key::Project {
                        on,
                        project: row,
                        lone: Lone::of(unopened),
                    };
                    let dir = sidebar::short_cwd(housed.dir.as_str());
                    found.push((
                        None,
                        label::Named::whole(project.to_owned()),
                        vec![dir],
                        key,
                    ));
                    continue;
                }
                for worktree in housed.worktrees() {
                    let name = label::Named::placed(project, worktree.named.name());
                    let folder = sidebar::short_cwd(worktree.folder.as_str());
                    let also = match worktree.named.apart() {
                        Some(branch) => vec![branch.as_str().to_owned(), folder],
                        None => vec![folder],
                    };
                    let key = Key::Worktree {
                        on: on.clone(),
                        place: housed.place(worktree.id),
                    };
                    found.push((worktree.used, name, also, key));
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
        finder: Option<&Finder>,
        on: &Machine,
        project: ProjectId,
    ) -> Vec<Row> {
        let shelved = self
            .listing
            .shelves
            .iter()
            .filter(|shelved| shelved.on == *on);
        let housed = shelved.flat_map(|shelved| &shelved.projects);
        let scoped = housed.filter(|housed| housed.project.id() == project);
        let found: Vec<Found> = scoped.flat_map(|housed| unopened_of(on, housed)).collect();
        matched(finder, candidates_of(&found))
    }

    fn agent_rows(&self, finder: Option<&Finder>, on: &Machine, runs: &Runs) -> Vec<Row> {
        let yours = self.here_view(on);
        let agents = runs
            .agents
            .iter()
            .filter(|seated| yours.is_none_or(|here| !seated.place.matches_view(here)));
        let agents: Vec<(label::Named, Vec<Option<&str>>, Key)> = agents
            .map(|seated| {
                let session = seated.place.listed().name().as_str();
                let also = vec![Some(session), Some(seated.dir.as_str())];
                let key = Key::Agent {
                    on: on.clone(),
                    place: seated.place.clone(),
                };
                (
                    label::Named::whole(agent_name(&runs.agents, seated)),
                    also,
                    key,
                )
            })
            .collect();
        matched(finder, agents.into_iter())
    }

    fn project_rows(&self) -> (Vec<Row>, Option<usize>) {
        let machines = self.machines();
        let ruled = machines.len() > 1;
        let typed = Folder::typed(&self.term);
        let per_machine = Typos::found(|typos| {
            let fuzzy = Fuzzy::new(&self.term, typos);
            let per_machine = machines.iter().filter_map(|on| {
                let taken = self.taken(on);
                let projects = self.projects_on(on);
                let mut kept: Vec<(&Housed, Option<(Used, WorktreeId)>)> = projects
                    .iter()
                    .map(|housed| (housed, last_used(housed)))
                    .collect();
                kept.sort_by_key(|(_, last)| Reverse(last.map(|(used, _)| used)));
                let kept = kept.into_iter().map(|(housed, last)| {
                    let goes = match last {
                        Some((_, id)) => Goes::Last(housed.place(id)),
                        None => Goes::Home(housed.project.clone()),
                    };
                    let name = housed.project.listed().name().to_string();
                    let dir = housed.dir.clone();
                    (
                        Key::Housed {
                            on: on.clone(),
                            dir: dir.clone(),
                            goes,
                        },
                        name,
                        dir,
                    )
                });
                let unkept = self
                    .found_on(on)
                    .iter()
                    .filter(|it| unhoused(projects, it.path()));
                let unkept = unkept.map(|repository| {
                    let name = repository.name().free(&taken);
                    let shown = name.to_string();
                    let (on, path) = (on.clone(), repository.path().clone());
                    (
                        Key::Repository {
                            on,
                            repository: repository.clone(),
                            name,
                        },
                        shown,
                        path,
                    )
                });
                let mut rows = place_rows(&fuzzy, &self.term, kept.chain(unkept));
                let owned = self.owned_on(on);
                if let Some(folder) = typed.as_ref().filter(|it| !it.is_in(owned)) {
                    let key = Key::Folder {
                        on: on.clone(),
                        folder: folder.clone(),
                    };
                    rows.push(Row::plain(key, folder.to_string()));
                }
                (!rows.is_empty()).then_some((on, rows))
            });
            per_machine.collect()
        });
        let (mut rows, mut aimed) = (Vec::new(), None);
        for (on, found) in per_machine {
            let first = grouped(&mut rows, found, ruled.then(|| self.machine_name(on)));
            if self.aim == *on {
                aimed = first;
            }
        }
        if let Some(aimed) = aimed {
            assert!(aimed < rows.len(), "the aimed machine's first row is a row");
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
            }
            Some(Key::Agent { on, place }) => Some(Preview::Screen((
                on.clone(),
                place.listed().clone(),
                Some(place.terminal()),
            ))),
            Some(Key::Housed { on, dir, .. }) => Some(Preview::Dir((on.clone(), dir.into()))),
            Some(Key::Repository { on, repository, .. }) => {
                Some(Preview::Dir((on.clone(), repository.path().into())))
            }
            Some(Key::Unopened { on, folder, .. }) => {
                Some(Preview::Dir((on.clone(), folder.clone())))
            }
            Some(
                Key::Project { .. }
                | Key::Machine { .. }
                | Key::Folder { .. }
                | Key::New { .. }
                | Key::Action(..)
                | Key::Pasted { .. },
            )
            | None => None,
        };
        match (preview, &self.wanted) {
            (Some(preview), Some((wanted, _))) if preview == *wanted => {}
            (Some(preview), _) => {
                match &preview {
                    Preview::Screen(looked) => {
                        if matches!(self.peeks.get(looked), Some(Peek::Failed(_))) {
                            self.peeks.remove(looked);
                        }
                    }
                    Preview::Dir(located) => {
                        if matches!(self.contents.get(located), Some(Contents::Failed(_))) {
                            self.contents.remove(located);
                        }
                    }
                }
                self.wanted = Some((preview, Instant::now()));
            }
            (None, _) => self.wanted = None,
        }
    }

    fn notes(&self, width: usize) -> Vec<Line<'static>> {
        let error = Style::new().fg(ERROR);
        let mut notes = Vec::new();
        if let Stage::Asking { asks, .. } = &self.stage {
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
            }
            List::Worktrees => {}
            List::Agents => {
                if let Some(why) = &self.listing.away {
                    let why = format!("luned does not answer: {why}");
                    notes.extend(menu::wrap(&why, width, error));
                }
            }
            List::Projects => {
                for on in self.machines() {
                    match self.lookups.get(&on) {
                        Some(Lookup::Failed(why)) => notes.extend(menu::wrap(why, width, error)),
                        Some(Lookup::Listed(found)) => {
                            for why in &found.unread {
                                notes.extend(menu::wrap(why, width, error));
                            }
                            let projects = self.projects_on(&on);
                            let holding = found.repositories.iter();
                            let holding = holding.filter(|it| holds_a_project(projects, it.path()));
                            let holding = holding.count();
                            if holding > 0 {
                                let there = match &on {
                                    Machine::Local => String::new(),
                                    Machine::Remote(destination) => format!("{destination}: "),
                                };
                                let why = format!("left out {holding} clones a project lies in");
                                notes.extend(menu::wrap(&format!("{there}{why}"), width, error));
                            }
                        }
                        Some(Lookup::Asking(_)) | None => {}
                    }
                }
            }
        }
        notes
    }

    fn body(
        &self,
        width: usize,
        room: usize,
        wall: SystemTime,
    ) -> (Vec<Line<'static>>, Spots<Pick>, Cadence) {
        let mut lines = Vec::new();
        let mut spots = Spots::default();
        let mut cadence = Cadence::default();
        let mut room = room;
        if self.list == List::Agents
            && let Some((yours, agents)) = self.yours()
        {
            if room == 0 {
                return (lines, spots, cadence);
            }
            lines.push(here_line(&agent_name(agents, yours), width));
            room -= 1;
        }
        if room == 0 {
            return (lines, spots, cadence);
        }
        let rows = self.rows();
        if rows.rows.is_empty() {
            lines.extend(menu::wrap(
                &self.empty(),
                width,
                Style::new().fg(moonlight::MUTE),
            ));
            return (lines, spots, cadence);
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
        let frame = sidebar::frame(wall);
        for item in &items[start..end] {
            let row = match item {
                Item::Machine(name) => {
                    lines.push(machine_rule(name, width));
                    continue;
                }
                Item::Row(row) => *row,
            };
            let lit = rows.at == Some(row);
            let shown = &rows.rows[row];
            if width > 0
                && let Key::Agent { on, place } = &shown.key
                && let Some(seated) = self.seated(on, place)
            {
                if seated.mark == Mark::Busy {
                    cadence.spin();
                }
                if let Some(since) = seated.since {
                    cadence.age(since, wall);
                }
            }
            let line = self.row_line(shown, width, frame, wall);
            if self.picks() {
                spots.add(lines.len(), 0..width, Pick::Row(shown.key.clone()));
            }
            lines.push(if lit {
                line.patch_style(Style::new().bg(LIT))
            } else {
                line
            });
        }
        (lines, spots, cadence)
    }

    fn here_session(&self, on: &Machine) -> Option<&str> {
        let here = self.listing.here.as_ref().filter(|here| here.on() == on)?;
        Some(here.session())
    }

    fn here_view(&self, on: &Machine) -> Option<&ViewPlace> {
        self.listing
            .here
            .as_ref()
            .filter(|here| here.on() == on)?
            .view()
    }

    fn yours(&self) -> Option<(&Seated, &[Seated])> {
        let here = self.listing.here.as_ref()?;
        let (view, agents) = (here.view()?, &self.runs(here.on())?.agents);
        Some((
            agents
                .iter()
                .find(|seated| seated.place.matches_view(view))?,
            agents,
        ))
    }

    fn row_line(
        &self,
        row: &Row,
        width: usize,
        frame: SpinnerFrame,
        now: SystemTime,
    ) -> Line<'static> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let grey = Style::new().fg(moonlight::MUTE);
        let fill = match &row.key {
            Key::Worktree { on, place } if self.is_shown(on, place) => Fill::Rule,
            _ => Fill::Blank,
        };
        let fixed = |lead: usize| 1 + lead + fill.least() + 1;
        let (lead, name_style, right) = match &row.key {
            Key::Worktree { on, place } => {
                let worktree = self.row_worktree(on, place);
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let usable = width.saturating_sub(fixed(0));
                let spare = usable.saturating_sub(row.shown().width().min(usable / 2));
                let style = if away.is_some() { dim } else { grey };
                let branch = worktree.named.apart().into_iter();
                let right = branch.map(|branch| Span::styled(sidebar::cut(branch, spare), style));
                let style = match (away, fill) {
                    (Some(_), _) => dim,
                    (None, Fill::Rule) => Style::new().fg(NAME).add_modifier(Modifier::BOLD),
                    (None, Fill::Blank) => bold,
                };
                (None, style, right.collect())
            }
            Key::Project { on, lone, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = self.tag(on, away);
                right.push(Span::styled(lone.tag(), grey));
                (None, if away.is_some() { dim } else { bold }, right)
            }
            Key::Unopened { on, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let mut right = self.tag(on, away);
                right.push(Span::styled(UNOPENED, grey));
                (None, dim, right)
            }
            Key::Machine { on } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away);
                let said = match away {
                    Some(word) => Span::styled(word, Style::new().fg(ERROR)),
                    None => Span::styled("no projects", grey),
                };
                (None, dim, vec![said])
            }
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
                let age = seated
                    .since
                    .map_or_default(|since| sidebar::age(since, now));
                let mut right = Vec::new();
                if width >= row.shown().width() + seated.dir.width() + age.width() + 10 {
                    right.push(Span::styled(
                        seated.dir.clone(),
                        Style::new().fg(moonlight::MUTE),
                    ));
                    right.push(Span::raw("  "));
                }
                right.push(Span::styled(age, dim));
                (Some(Span::styled(glyph, style)), name_style, right)
            }
            Key::Housed { on, dir, .. } => {
                let away = self.shelved(on).and_then(|shelved| shelved.away).is_some();
                return place_line(row, dir.as_str(), if away { dim } else { bold }, width);
            }
            Key::Repository { repository, .. } => {
                let path = repository.path().as_str();
                return place_line(row, path, Style::new().fg(moonlight::TEXT), width);
            }
            Key::Folder { .. } => {
                let lead = Span::styled("+", Style::new().fg(moonlight::GREEN));
                (Some(lead), bold, vec![Span::styled("new project", grey)])
            }
            Key::New { .. } => {
                let lead = Span::styled("+", Style::new().fg(moonlight::GREEN));
                (Some(lead), bold, vec![Span::styled("new worktree", grey)])
            }
            Key::Action(action, _) => {
                let key = menu::spelled(*action, self.palette.origin);
                (
                    None,
                    Style::new().fg(moonlight::TEXT),
                    vec![Span::styled(key, dim)],
                )
            }
            Key::Pasted { work, .. } => {
                let whence = vec![Span::styled(menu::whence(work), dim)];
                (None, Style::new().fg(moonlight::TEXT), whence)
            }
        };
        let right_width: usize = right.iter().map(|span| span.content.width()).sum();
        let lead_width = lead.as_ref().map_or(0, |lead| lead.content.width() + 1);
        let room = width.saturating_sub(fixed(lead_width) + right_width);
        let (name, named) = sidebar::lit(row.shown(), room, Keep::Start, &row.hits, name_style);
        let mut spans = vec![Span::raw(" ")];
        if let Some(lead) = lead {
            spans.push(lead);
            spans.push(Span::raw(" "));
        }
        spans.extend(name);
        let gap = width.saturating_sub(1 + lead_width + named + right_width + 1);
        match fill {
            Fill::Blank => spans.push(Span::raw(" ".repeat(gap))),
            Fill::Rule => {
                let accent = Style::new().fg(ACCENT);
                let rule = gap.saturating_sub(1 + HERE.width() + 1);
                spans.push(Span::raw(" "));
                spans.push(Span::styled("─".repeat(rule), accent));
                spans.push(Span::styled(HERE, accent));
                spans.push(Span::raw(" "));
            }
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

    fn scoped_empty(&self, Scope { on, project, .. }: &Scope) -> String {
        let shelved = self
            .listing
            .shelves
            .iter()
            .filter(|shelved| shelved.on == *on);
        let mut housed = shelved.flat_map(|shelved| &shelved.projects);
        let Some(housed) = housed.find(|housed| housed.project.id() == *project) else {
            return "luned keeps that project no more".to_owned();
        };
        let name = housed.project.listed().name();
        match &housed.holds {
            Holds::Folder(_) => projects::folder_has_one_worktree(name),
            Holds::Clone {
                unopened: Unopened::Unlisted(why),
                ..
            } => unlisted(name, why),
            Holds::Clone {
                unopened: Unopened::Listed(_),
                ..
            } => "every worktree is open".to_owned(),
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
            List::Worktrees => "luned keeps no worktree yet".to_owned(),
            List::Agents if self.listing.away.is_some() => "no agents".to_owned(),
            List::Agents if self.yours().is_some() => "no other agents".to_owned(),
            List::Agents => "no agents running".to_owned(),
            List::Projects => {
                let lookups = self
                    .machines()
                    .into_iter()
                    .filter_map(|on| self.lookups.get(&on));
                let lookups: Vec<&Lookup> = lookups.collect();
                if lookups
                    .iter()
                    .any(|lookup| matches!(lookup, Lookup::Asking(_)))
                {
                    "looking for repositories…".to_owned()
                } else {
                    NO_PROJECTS.to_owned()
                }
            }
        }
    }

    fn draw_preview(&self, buffer: &mut Buffer, area: Rect, wall: SystemTime) -> Cadence {
        let mut cadence = Cadence::default();
        let inner = usize::from(area.width.saturating_sub(2));
        let grey = Style::new().fg(moonlight::MUTE);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Some(row) = self.rows().selected() else {
            let mut lines = vec![Line::styled(" nothing highlighted", grey)];
            let makes = |(_, housed): (_, &Housed)| housed.refuses_new_worktree().is_none();
            let offers = self.list == List::Worktrees && self.scope.is_none();
            if offers && self.here_project().is_some_and(makes) {
                lines.push(Line::default());
                lines.push(Line::styled(" type a branch to make its worktree", grey));
            }
            framed(buffer, area, "Preview", "", lines);
            return cadence;
        };
        let said = |caption: Vec<Span<'static>>, said: &str| {
            let mut lines = vec![Line::from(caption), Line::default()];
            lines.extend(menu::wrap(said, inner, grey));
            lines
        };
        let (title, right, caption, looked) = match &row.key {
            Key::Housed { on, dir, .. } => {
                self.draw_contents(buffer, area, row.shown(), on, &dir.into());
                return cadence;
            }
            Key::Repository { on, repository, .. } => {
                self.draw_contents(buffer, area, row.shown(), on, &repository.path().into());
                return cadence;
            }
            Key::Unopened {
                on, folder: path, ..
            } => {
                self.draw_contents(buffer, area, row.shown(), on, path);
                return cadence;
            }
            Key::Folder { on, folder } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                caption.push(Span::styled("new project", grey));
                let words = format!(
                    "Enter makes the project of {folder}, named for it or for the repository it \
                     is in, and goes to it. In a folder of a project already, it goes to that \
                     project",
                );
                framed(buffer, area, row.shown(), "", said(caption, &words));
                return cadence;
            }
            Key::New {
                project, branch, ..
            } => {
                let project = project.listed().name();
                let words = format!(
                    "no worktree matches: Enter makes the worktree of branch {branch} in \
                     {project}, as n w does, and goes to it. A branch git has is checked out, \
                     and any other is made from the default branch",
                );
                let lines = said(vec![Span::styled(" new", grey)], &words);
                framed(buffer, area, row.shown(), "", lines);
                return cadence;
            }
            Key::Project { on, project, lone } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                caption.push(Span::styled(lone.tag(), grey));
                let name = project.listed().name();
                let words = match lone {
                    Lone::Clone => format!(
                        "{name} is a clone with no worktree: Enter asks for a branch and makes \
                         its worktree, as n w does",
                    ),
                    Lone::NoneOpen => format!(
                        "{name} has no worktree open: Enter lists its worktrees not open, as o w \
                         does",
                    ),
                    Lone::Unlisted(why) => unlisted(name, why),
                };
                framed(buffer, area, row.shown(), "", said(caption, &words));
                return cadence;
            }
            Key::Machine { on } => {
                let words = match self.out_of_reach(on) {
                    Some(why) => format!("{why}, and heard of no project there before"),
                    None => format!("luned on {} keeps no project", self.machine_name(on)),
                };
                framed(buffer, area, row.shown(), "", said(Vec::new(), &words));
                return cadence;
            }
            Key::Worktree { on, place } => {
                let worktree = self.row_worktree(on, place);
                let title = row.shown().to_owned();
                let count = worktree.terminals.len();
                let right = if count == 1 {
                    "1 terminal".to_owned()
                } else {
                    format!("{count} terminals")
                };
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                let room = inner.saturating_sub(1 + caption.iter().map(Span::width).sum::<usize>());
                caption.push(Span::styled(
                    sidebar::tail(worktree.folder.as_str(), room),
                    grey,
                ));
                let Some(focus) = self.focus(on, worktree) else {
                    let lines = said(caption, "it holds no terminal to look at");
                    framed(buffer, area, &title, &right, lines);
                    return cadence;
                };
                let looked = (on.clone(), place.project.listed().clone(), Some(focus));
                (title, right, Line::from(caption), looked)
            }
            Key::Action(..) | Key::Pasted { .. } => {
                unreachable!("actions are listed in the Actions list, which previews none")
            }
            Key::Agent { on, place } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                if let Some(seated) = self.seated(on, place) {
                    let status = if seated.mark == Mark::Waiting {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(moonlight::TEXT)
                    };
                    if inner > 0
                        && let Some(since) = seated.since
                    {
                        cadence.age(since, wall);
                    }
                    let age = seated.since.map(|since| sidebar::age(since, wall));
                    caption.extend([
                        Span::styled(seated.status.clone(), status),
                        Span::styled(" · ", dim),
                        Span::styled(age.unwrap_or_default(), dim),
                        Span::styled(" · ", dim),
                        Span::styled(seated.dir.clone(), grey),
                    ]);
                }
                let looked = (on.clone(), place.listed().clone(), Some(place.terminal()));
                (
                    row.shown().to_owned(),
                    String::new(),
                    Line::from(caption),
                    looked,
                )
            }
        };
        let peek = self.peeks.get(&looked);
        let mut lines = vec![caption, Line::default()];
        match peek {
            None | Some(Peek::Reading) => lines.push(Line::styled(" reading…", grey)),
            Some(Peek::Failed(why)) => {
                lines.extend(menu::wrap(why, inner, Style::new().fg(ERROR)));
            }
            Some(Peek::Ready { shot, .. }) if shot.is_blank() => {
                lines.push(Line::styled(" nothing on this screen", grey));
            }
            Some(Peek::Ready { .. }) => {}
        }
        framed(buffer, area, &title, &right, lines);
        if let Some(Peek::Ready { shot, .. }) = peek
            && !shot.is_blank()
        {
            let shown = under_caption(area);
            let top = shot
                .end
                .saturating_sub(usize::from(shown.height))
                .max(shot.first);
            shot.show(buffer, shown, top);
        }
        cadence
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
        let grey = Style::new().fg(moonlight::MUTE);
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
            }
            Some(Contents::Ready { total, .. }) => {
                right = if *total == 1 {
                    "1 item".to_owned()
                } else {
                    format!("{total} items")
                };
                if *total == 0 {
                    lines.push(Line::styled(" empty", grey));
                }
            }
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
                Key::Worktree { on, place }
                | Key::Housed {
                    on,
                    goes: Goes::Last(place),
                    ..
                } if self.is_shown(on, place) => Ok("you are in it".to_owned()),
                Key::Worktree { on, .. } | Key::Housed { on, .. } => Ok(there("Go", on)),
                Key::Project {
                    on,
                    lone: Lone::Clone,
                    ..
                } => Ok(there("New worktree", on)),
                Key::Project {
                    on,
                    lone: Lone::NoneOpen,
                    ..
                } => Ok(there("Open a worktree", on)),
                Key::Project {
                    project,
                    lone: Lone::Unlisted(why),
                    ..
                } => Err(unlisted(project.listed().name(), why)),
                Key::Unopened { on, .. } => Ok(there("Open", on)),
                Key::Machine { on } => Err(self
                    .out_of_reach(on)
                    .unwrap_or_else(|| format!("{} keeps no project", self.machine_name(on)))),
                Key::Agent { on, .. } => Ok(there("Go to agent", on)),
                Key::Repository { on, name, .. } => Ok(there(&format!("Make \"{name}\""), on)),
                Key::Folder { on, .. } => Ok(there("Make the project", on)),
                Key::New {
                    project, branch, ..
                } => {
                    let project = project.listed().name();
                    Ok(format!("New worktree \"{branch}\" in {project}"))
                }
                Key::Action(..) | Key::Pasted { .. } => Ok("Run".to_owned()),
            },
            None if self.list == List::Worktrees
                && self.scope.is_none()
                && !self.term.is_empty() =>
            {
                match self.new_key() {
                    Some(Err(why)) => Err(why),
                    Some(Ok(_)) | None => Ok(String::new()),
                }
            }
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
            (Stage::Asking { .. }, _) => {
                vec![(Hint::Char('y'), "Yes"), (Hint::AnyOtherKey, "No")]
            }
            (Stage::Planning { .. }, _) => vec![(Hint::Esc, "Give up")],
            (Stage::Chain { chain, .. }, _) => {
                vec![(Hint::Enter, chain.enter()), (Hint::Esc, "Back")]
            }
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
            (Stage::Search, List::Projects) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Open"),
                (Hint::Tab, List::Projects.next().title()),
                (Hint::Esc, "Close"),
            ],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    Exact,
    Leads,
    Word,
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
    chars
        .next()
        .map_or_default(|first| first.to_uppercase().chain(chars).collect())
}

fn tier(finder: &Finder, text: &str, own: Option<&str>) -> Option<(Tier, Fit, Vec<usize>)> {
    let (fit, hits) = finder.find(text, &[])?;
    Some((tier_of(finder, text, own), fit, hits))
}

fn tier_of(finder: &Finder, text: &str, own: Option<&str>) -> Tier {
    let term = finder.term();
    let lower = text.to_lowercase();
    let own = own.map(str::to_lowercase);
    let words = lower.match_indices([' ', '/', '-']).map(|(at, _)| at + 1);
    let starts = std::iter::once(0)
        .chain(words)
        .any(|at| lower[at..].starts_with(term));
    if lower == term || own.as_deref() == Some(term) {
        Tier::Exact
    } else if own.is_some_and(|own| own.starts_with(term)) {
        Tier::Leads
    } else if starts {
        Tier::Word
    } else {
        Tier::Fuzzy
    }
}

fn found_action(finder: &Finder, action: Action, run: Run) -> Option<(Tier, Fit, Row)> {
    let Spec { keywords, .. } = action.spec();
    let title = titled(action);
    let key = Key::Action(action, run);
    let row = |name, hits| Row::named(key.clone(), label::Named::whole(name), hits);
    let named = tier(finder, &title, Some(title.trim_start_matches('/')));
    let keyword = keywords
        .iter()
        .filter_map(|it| Some((tier(finder, it, None)?, *it)));
    let keyword = keyword.min_by_key(|((tier, fit, _), _)| (*tier, Reverse(*fit)));
    match (named, keyword) {
        (Some((by_name, fit, hits)), Some(((by_keyword, ..), _))) if by_name <= by_keyword => {
            Some((by_name, fit, row(title, hits)))
        }
        (Some((by_name, fit, hits)), None) => Some((by_name, fit, row(title, hits))),
        (_, Some(((by_keyword, fit, hits), keyword))) => {
            let lead = format!("{title} (");
            let shift = lead.chars().count();
            let hits = hits.into_iter().map(|hit| hit + shift).collect();
            Some((by_keyword, fit, row(format!("{lead}{keyword})"), hits)))
        }
        (None, None) => None,
    }
}

fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

fn agent_name(agents: &[Seated], seated: &Seated) -> String {
    let alike = agents.iter().filter(|other| other.label == seated.label);
    let sessions: HashSet<&SessionName> = alike
        .clone()
        .map(|other| other.place.listed().name().session())
        .collect();
    let (session, view) = (seated.place.listed().name(), seated.place.terminal().id);
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

fn unopened_of<'a>(on: &'a Machine, housed: &'a Housed) -> impl Iterator<Item = Found> {
    housed
        .unopened_folders()
        .iter()
        .map(move |unopened| unopened_found(on, housed, unopened))
}

fn unopened_found(on: &Machine, housed: &Housed, unopened: &projects::Dir) -> Found {
    let project = housed.project.listed().name().as_str();
    let name = label::Named::placed(project, unopened.last());
    let folder = sidebar::short_cwd(unopened.as_str());
    let (row, at) = (housed.project.clone(), unopened.into());
    let key = Key::Unopened {
        on: on.clone(),
        project: row,
        folder: at,
    };
    (None, name, vec![UNOPENED.to_owned(), folder], key)
}

fn candidates_of(found: &[Found]) -> impl Iterator<Item = (label::Named, Vec<Option<&str>>, Key)> {
    found.iter().map(|(_, name, also, key)| {
        (
            name.clone(),
            also.iter().map(|text| Some(text.as_str())).collect(),
            key.clone(),
        )
    })
}

fn matched<'a>(
    finder: Option<&Finder>,
    candidates: impl Iterator<Item = (label::Named, Vec<Option<&'a str>>, Key)>,
) -> Vec<Row> {
    let Some(finder) = finder else {
        let row = |(named, _, key)| Row::named(key, named, Vec::new());
        return candidates.map(row).collect();
    };
    let mut found: Vec<(Fit, Row)> = Vec::new();
    for (named, also, key) in candidates {
        if let Some((fit, hits)) = finder.find_own(named.shown(), Some(named.own()), &also) {
            found.push((fit, Row::named(key, named, hits)));
        }
    }
    found.sort_by_key(|(fit, _)| Reverse(*fit));
    found.into_iter().map(|(_, row)| row).collect()
}

fn last_used(housed: &Housed) -> Option<(Used, WorktreeId)> {
    let used = housed
        .worktrees()
        .iter()
        .filter_map(|it| Some((it.used?, it.id)));
    used.max_by_key(|(used, _)| *used)
}

fn unhoused(projects: &[Housed], root: &projects::Dir) -> bool {
    !projects.iter().any(|housed| housed.dir.nests(root))
}

fn holds_a_project(projects: &[Housed], root: &projects::Dir) -> bool {
    projects
        .iter()
        .any(|housed| housed.dir != *root && root.below(&housed.dir).is_some())
}

struct Matched {
    exact: bool,
    score: Score,
    at: usize,
    row: Row,
}

fn place_rows(
    fuzzy: &Fuzzy,
    term: &str,
    places: impl Iterator<Item = (Key, String, projects::Dir)>,
) -> Vec<Row> {
    let mut matched: Vec<Matched> = Vec::new();
    for (at, (key, name, path)) in places.enumerate() {
        let hit = if term.is_empty() {
            Some((0, Vec::new()))
        } else {
            fuzzy.hits(path.as_str()).or_else(|| {
                let score = fuzzy.score(&name)?;
                Some((score, Vec::new()))
            })
        };
        if let Some((score, hits)) = hit {
            assert!(
                hits.iter().all(|hit| *hit < path.as_str().len()),
                "a hit is in the path"
            );
            let exact = name == term || path.last() == term;
            let row = Row::named(key, label::Named::whole(name), hits);
            matched.push(Matched {
                exact,
                score,
                at,
                row,
            });
        }
    }
    matched.sort_by_key(|it| (Reverse(it.exact), Reverse(it.score), it.at));
    matched.into_iter().map(|it| it.row).collect()
}

fn place_line(row: &Row, path: &str, style: Style, width: usize) -> Line<'static> {
    assert!(
        row.hits.iter().all(|hit| *hit < path.len()),
        "a row's hits are in its path"
    );
    let name = sidebar::cut(
        row.shown(),
        width.saturating_sub(4 + path.width()).max(width / 3),
    );
    let room = width.saturating_sub(4 + name.width());
    let grey = Style::new().fg(moonlight::MUTE);
    let (shown, wide) = sidebar::lit(path, room, Keep::End, &row.hits, grey);
    let gap = width.saturating_sub(1 + name.width() + wide + 1);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(name, style),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(shown);
    spans.push(Span::raw(" "));
    Line::from(spans)
}

fn in_project(project: impl fmt::Display, name: impl fmt::Display) -> String {
    format!("{project} › {name}")
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
    vec![
        Span::styled(machine, Style::new().fg(NAME)),
        Span::styled(" · ", dim),
    ]
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
        Span::styled(
            name,
            Style::new()
                .fg(moonlight::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled("─".repeat(rule), dim),
    ])
}

fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = (selected + 1).saturating_sub(visible);
    (start, (start + visible).min(total))
}

fn under_caption(area: Rect) -> Rect {
    Rect::new(
        area.x + 2,
        area.y + 3,
        area.width.saturating_sub(4),
        area.height.saturating_sub(4),
    )
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
        spans.push(Span::raw(
            " ".repeat(width.saturating_sub(used + action.width() + 1)),
        ));
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
        let height = if bordered {
            INPUT_HEIGHT
        } else {
            area.height.min(1)
        };
        let input_y = area
            .bottom()
            .saturating_sub(u16::from(help.is_some()) + height);
        let input = Rect::new(area.x, input_y, area.width, height);
        let above = input_y - area.y;
        let full = (above >= MIN_RESULTS).then(|| Rect::new(area.x, area.y, area.width, above));
        let split = full.filter(|full| full.width >= MIN_HALF * 2).map(|full| {
            let left = full.width / 2;
            let right = Rect {
                x: full.x + left,
                width: full.width - left,
                ..full
            };
            (
                Rect {
                    width: left,
                    ..full
                },
                right,
            )
        });
        let (results, preview) = match split {
            Some((results, preview)) => (Some(results), Some(preview)),
            None => (full, None),
        };
        Layout {
            results,
            preview,
            full,
            input,
            bordered,
            help,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use lune_domain::projects::{Name, ProjectName, WorktreeId};
    use ratatui::layout::Position;

    use super::*;
    use crate::close::Doing;
    use crate::menu::{Cwd, Folder, Ran, ViewKind, ViewRow, WorktreeRow};
    use crate::spots::Hit;
    use lune_domain::projects::ViewId;

    const NOW: Duration = Duration::from_secs(1000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    fn listed(name: &str, id: u64) -> Listed {
        Listed::new_project(name, id)
    }

    fn terminal_key(id: u32) -> TerminalKey {
        ViewKey::terminal(id).terminal_key().unwrap()
    }

    fn seated(name: &str, started: u64, view: u32, label: &str, mark: Mark) -> Seated {
        Seated {
            place: TerminalPlace::new(place(name, started, 1), terminal_key(view)),
            label: label.to_owned(),
            mark,
            status: "idle".to_owned(),
            since: Some(now() - Duration::from_mins(1)),
            dir: "lune/main".to_owned(),
            pid: view,
            transcript: None,
            doing: crate::alerts::Doing::Idle,
        }
    }

    fn here(session: &str) -> Here {
        Here::Session {
            on: Machine::Local,
            session: session.to_owned(),
        }
    }

    fn agents(agents: Vec<Seated>) -> Runs {
        Runs { agents }
    }

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn repository(path: &str) -> Repository {
        Repository::at(projects::Dir::parse(path).unwrap()).unwrap()
    }

    fn found_in(owned: &str, repositories: Vec<Repository>) -> repositories::Found {
        let owned = Some(OwnedFolder::of(projects::Dir::parse(owned).unwrap(), None));
        repositories::Found {
            repositories,
            owned,
            unread: Vec::new(),
        }
    }

    fn heard_here(found: Result<Vec<Repository>, String>) -> Heard {
        let found = found.map(|repositories| found_in("/home/you/lune", repositories));
        Heard::Repositories {
            on: Machine::Local,
            ask: 1,
            found,
        }
    }

    fn heard_there(ask: u64, found: Result<Vec<Repository>, String>) -> Heard {
        let found = found.map(|repositories| found_in("/home/me/lune", repositories));
        Heard::Repositories {
            on: nixos(),
            ask,
            found,
        }
    }

    fn project(name: &str, id: u64) -> ProjectRow {
        ProjectRow::new(name, id)
    }

    fn non_empty(text: &str) -> Said {
        Said::new(text.to_owned()).unwrap()
    }

    fn worktree_id(project: u64, worktree: u64) -> WorktreeId {
        WorktreeId::of(project * 10 + worktree).unwrap()
    }

    fn place(project: &str, id: u64, worktree: u64) -> WorktreePlace {
        WorktreePlace {
            project: self::project(project, id),
            worktree: worktree_id(id, worktree),
        }
    }

    fn worktree(id: WorktreeId, name: &str, used: Option<u64>, terminals: &[u32]) -> Worktree {
        Worktree {
            id,
            named: sidebar::Named::of(non_empty(name), None),
            folder: projects::Dir::parse(&format!("/home/you/{name}")).unwrap(),
            used: used.map(|at| Used::read(at * 1_000).unwrap()),
            terminals: terminals.iter().map(|id| terminal_key(*id)).collect(),
        }
    }

    fn branched(worktree: &mut Worktree, branch: &str) {
        let name = non_empty(worktree.named.name());
        worktree.named = sidebar::Named::of(name, Some(non_empty(branch)));
    }

    fn worktrees_mut(housed: &mut Housed) -> &mut [Worktree] {
        match &mut housed.holds {
            Holds::Folder(own) => std::slice::from_mut(own),
            Holds::Clone { worktrees, .. } => worktrees,
        }
    }

    fn set_unopened(housed: &mut Housed, unopened: Unopened) {
        let Holds::Clone { unopened: held, .. } = &mut housed.holds else {
            panic!("only a clone holds unopened worktrees");
        };
        *held = unopened;
    }

    fn housed(name: &str, id: u64, worktrees: Vec<Worktree>) -> Housed {
        let dir = projects::Dir::parse(&format!("/home/you/{name}")).unwrap();
        Housed {
            project: project(name, id),
            dir,
            holds: Holds::Clone {
                worktrees,
                unopened: Unopened::Listed(Vec::new()),
            },
        }
    }

    fn housed_folder(name: &str, id: u64, own: Worktree) -> Housed {
        let dir = projects::Dir::parse(&format!("/home/you/{name}")).unwrap();
        Housed {
            project: project(name, id),
            dir,
            holds: Holds::Folder(own),
        }
    }

    fn shelved(on: Machine, projects: Vec<Housed>) -> Shelved {
        Shelved {
            on,
            away: None,
            projects,
        }
    }

    fn fix() -> WorktreePlace {
        place("lune", 1, 2)
    }

    fn api() -> WorktreePlace {
        place("web", 3, 1)
    }

    fn main_place() -> WorktreePlace {
        place("lune", 1, 1)
    }

    fn lune() -> Housed {
        housed(
            "lune",
            1,
            vec![
                worktree(main_place().worktree, "main", Some(30), &[1, 2]),
                worktree(fix().worktree, "fix", Some(50), &[5, 3]),
                worktree(worktree_id(1, 3), "docs", None, &[]),
            ],
        )
    }

    fn works() -> Listing {
        let projects = vec![
            lune(),
            housed("notes", 2, Vec::new()),
            housed(
                "web",
                3,
                vec![worktree(api().worktree, "api", Some(40), &[4])],
            ),
        ];
        let at = ViewPlace::new(fix(), ViewKey::terminal(3));
        Listing {
            here: Some(Here::View {
                on: Machine::Local,
                place: at,
            }),
            shown: Some((Machine::Local, fix())),
            shelves: vec![shelved(Machine::Local, projects)],
            ..Listing::default()
        }
    }

    fn at_host() -> Palette {
        let target = Rc::new(Target::Machine(Machine::Local));
        Palette {
            aim: Aim::At {
                target,
                worktree: None,
            },
            origin: Origin::Views,
        }
    }

    fn at_fix(origin: Origin) -> Palette {
        let worktree = match origin {
            Origin::Views => Some(Rc::new(fix_worktree())),
            Origin::Sidebar => None,
        };
        Palette {
            aim: Aim::At {
                target: Rc::new(fix_view()),
                worktree,
            },
            origin,
        }
    }

    fn fix_worktree() -> Target {
        let row = WorktreeRow {
            place: fix(),
            name: "fix".to_owned(),
            folder: directory("/home/you/fix"),
            branch: None,
            repository: None,
            flow: None,
        };
        Target::Worktree(Machine::Local, row)
    }

    fn fix_view() -> Target {
        let row = ViewRow {
            worktree: fix(),
            kind: ViewKind::Terminal {
                key: terminal_key(3),
                id: ViewId::of(3),
                ran: Ran::Unhooked,
            },
            cwd: Cwd::At(Folder {
                dir: directory("/home/you/fix"),
                branch: None,
                repository: None,
                pull_request: None,
            }),
            arranged: None,
            flow: None,
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
        (
            lines.into_iter().filter(|line| !line.is_empty()).collect(),
            lit,
        )
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
        let line = |y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        };
        (area.y..area.bottom())
            .map(|y| line(y).trim().to_owned())
            .collect()
    }

    fn parts() -> (Rect, Rect) {
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        let inner = |rect: Rect| Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
        let layout = Layout::of(inner(area));
        (
            inner(layout.results.unwrap()),
            inner(layout.preview.unwrap()),
        )
    }

    fn list(picker: &Picker) -> (Vec<String>, Option<String>) {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lines = text(&buffer, results);
        let lit = (results.y..results.bottom()).position(|y| buffer[(results.x, y)].bg == LIT);
        let lit = lit.map(|at| lines[at].clone());
        (
            lines.into_iter().filter(|line| !line.is_empty()).collect(),
            lit,
        )
    }

    fn found(picker: &Picker) -> Vec<(String, String)> {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lit = |y| -> String {
            let cells = (results.x..results.right()).map(|x| &buffer[(x, y)]);
            cells
                .filter(|cell| cell.fg == ACCENT)
                .map(ratatui::buffer::Cell::symbol)
                .collect()
        };
        let lines = (results.y..results.bottom()).zip(text(&buffer, results));
        lines
            .map(|(y, line)| (line, lit(y)))
            .filter(|(line, _)| !line.is_empty())
            .collect()
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
        lines
            .iter()
            .map(|line| line.split(' ').next().unwrap_or("").to_owned())
            .collect()
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
        Listing {
            this: "desk".to_owned(),
            remotes,
            ..Listing::default()
        }
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
        Picked::Go {
            on,
            place: Place::Worktree(place),
        }
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
        let mut listing = works();
        branched(
            &mut worktrees_mut(&mut listing.shelves[0].projects[2])[0],
            "feat/api",
        );
        let mut picker = Picker::open(1, List::Worktrees, listing, Machine::Local, at_host());
        let on = Machine::Local;
        assert_eq!(picker.asks(), [Ask::Repositories { on, ask: 1 }]);
        let shown = [
            "lune › fix",
            "web › api",
            "lune › main",
            "lune › docs",
            "notes",
        ];
        assert_eq!(rows(&picker), shown);
        let (lines, _) = list(&picker);
        assert!(lines[0].ends_with("─ here"), "{lines:?}");
        let words: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(
            words,
            ["web", "›", "api", "feat/api"],
            "its project, its name and its branch, and nothing else"
        );
        assert!(lines[4].ends_with("no worktree"), "{lines:?}");
        assert_eq!(lit(&picker), "web › api");
        assert!(said(&picker).contains("<ENTER> Go"), "{}", said(&picker));
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            go(Machine::Local, api())
        );
        press(&mut picker, KeyCode::Up);
        assert!(said(&picker).contains("you are in it"), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        down_to(&mut picker, "notes");

        let mut later = works();
        let shelf = worktrees_mut(&mut later.shelves[0].projects[0]);
        shelf[0].used = Some(Used::read(60_000).unwrap());
        picker.listed(later);
        assert_eq!(
            rows(&picker)[..3],
            ["lune › main", "lune › fix", "web › api"]
        );
        assert_eq!(lit(&picker), "notes");
    }

    #[test]
    fn a_long_branch_is_cut_and_leaves_the_name_its_share_of_the_row() {
        let long = format!("feat/{}", "x".repeat(120));
        let mut listing = works();
        branched(
            &mut worktrees_mut(&mut listing.shelves[0].projects[2])[0],
            &long,
        );
        branched(
            &mut worktrees_mut(&mut listing.shelves[0].projects[0])[1],
            &long,
        );
        let mut wide = worktree(worktree_id(4, 1), "fix", Some(35), &[6]);
        branched(&mut wide, &long);
        listing.shelves[0]
            .projects
            .push(housed("antigravity-cli", 4, vec![wide]));
        let picker = Picker::open(1, List::Worktrees, listing, Machine::Local, at_host());
        let (lines, _) = list(&picker);
        let width = usize::from(parts().0.width);
        let expected = ["lune › fix", "web › api", "antigravity-cli › fix"];
        for (line, name) in lines.iter().zip(expected) {
            assert!(line.starts_with(&format!("{name} ")), "{lines:?}");
            assert!(line.ends_with('…'), "the branch is cut: {lines:?}");
            assert!(line.width() < width, "{line:?} fits its row");
        }
        assert!(
            lines[0].contains(HERE),
            "the rule keeps its room: {lines:?}"
        );
    }

    #[test]
    fn a_long_name_on_the_shown_row_is_cut_and_leaves_the_rule_and_the_branch_their_room() {
        let width = usize::from(parts().0.width);
        let mut listing = works();
        let shown = &mut worktrees_mut(&mut listing.shelves[0].projects[0])[1];
        shown.named = sidebar::Named::of(non_empty(&"f".repeat(width)), Some(non_empty("main")));
        let picker = Picker::open(1, List::Worktrees, listing, Machine::Local, at_host());
        let (lines, _) = list(&picker);
        assert!(lines[0].ends_with("f… ─ here main"), "{lines:?}");
        assert_eq!(
            lines[0].width(),
            width - 2,
            "one blank each side: {lines:?}"
        );
    }

    #[test]
    fn a_worktree_named_for_its_branch_says_the_name_once() {
        let mut listing = works();
        branched(
            &mut worktrees_mut(&mut listing.shelves[0].projects[2])[0],
            "api",
        );
        let mut picker = Picker::open(1, List::Worktrees, listing, Machine::Local, at_host());
        let (lines, _) = list(&picker);
        assert_eq!(lines[1], "web › api", "{lines:?}");
        typed(&mut picker, "api");
        assert_eq!(rows(&picker), ["web › api"]);
    }

    #[test]
    fn a_clone_with_no_worktree_asks_a_branch_or_says_why_not_and_a_folder_lists_its_own() {
        let alone = |unopened: Unopened| {
            let mut listing = works();
            set_unopened(&mut listing.shelves[0].projects[1], unopened);
            let mut picker = opened(listing);
            down_to(&mut picker, "notes");
            picker
        };
        let preview = |picker: &Picker| text(&drawn(picker), parts().1).join(" ");
        let tagged = |picker: &Picker, tag: &str| list(picker).1.unwrap().ends_with(tag);

        let mut folder = works();
        let own = worktree(worktree_id(2, 1), "notes", Some(5), &[6]);
        folder.shelves[0].projects[1] = housed_folder("notes", 2, own);
        let picker = opened(folder);
        assert!(
            rows(&picker).contains(&"notes › notes".to_owned()),
            "a folder project lists its single worktree: {:?}",
            rows(&picker)
        );

        let mut clone = alone(Unopened::Listed(Vec::new()));
        assert!(tagged(&clone, "no worktree"), "{:?}", list(&clone));
        assert!(
            said(&clone).contains("<ENTER> New worktree"),
            "{}",
            said(&clone)
        );
        assert!(
            !said(&clone).contains("Start a worktree"),
            "{}",
            said(&clone)
        );
        let words = "notes is a clone with no worktree: Enter asks for a branch and makes its \
                     worktree, as n w does";
        assert!(preview(&clone).contains(words), "{}", preview(&clone));
        assert_eq!(
            press(&mut clone, KeyCode::Enter),
            Picked::Nothing,
            "no worktree at its root"
        );
        let asked = said(&clone);
        assert!(asked.contains("─ New worktree ─"), "{asked}");
        assert!(
            asked.contains("─ notes ─"),
            "the clone is named over the input: {asked}"
        );
        let mut renamed = works();
        renamed.shelves[0].projects[1] = housed("jots", 2, Vec::new());
        clone.listed(renamed);
        let asked = said(&clone);
        assert!(
            asked.contains("─ jots ─"),
            "its name follows the listing: {asked}"
        );
        assert_eq!(press(&mut clone, KeyCode::Esc), Picked::Nothing);
        assert_eq!(lit(&clone), "jots", "Esc goes back to the row");
        press(&mut clone, KeyCode::Enter);
        typed(&mut clone, "fix/notes");
        let branch = Branch::parse("fix/notes").unwrap();
        let done = Done::Worktree(Machine::Local, project("notes", 2).id(), branch);
        let made = Picked::Done {
            done,
            origin: Origin::Views,
        };
        assert_eq!(press(&mut clone, KeyCode::Enter), made);

        let mut unlisted = alone(Unopened::Unlisted("/home/you/notes is missing".to_owned()));
        assert!(tagged(&unlisted, "not listed"), "{:?}", list(&unlisted));
        let why = "notes's worktrees: /home/you/notes is missing";
        assert!(said(&unlisted).contains(why), "{}", said(&unlisted));
        assert!(preview(&unlisted).contains(why), "{}", preview(&unlisted));
        let promised = said(&unlisted).contains("Start a worktree");
        assert!(
            !promised && !preview(&unlisted).contains("starts a worktree"),
            "no start"
        );
        assert_eq!(press(&mut unlisted, KeyCode::Enter), Picked::Nothing);
        assert!(
            !said(&unlisted).contains("─ New worktree ─"),
            "{}",
            said(&unlisted)
        );
    }

    fn with_unopened(mut listing: Listing) -> Listing {
        let folder = |at: &str| projects::Dir::parse(at).unwrap();
        let projects = &mut listing.shelves[0].projects;
        let lune = |it: &str| folder(&format!("/home/you/lune/{it}"));
        set_unopened(
            &mut projects[0],
            Unopened::Listed(vec![lune("old"), lune("spike")]),
        );
        set_unopened(
            &mut projects[2],
            Unopened::Listed(vec![folder("/home/you/web/legacy")]),
        );
        listing
    }

    #[test]
    fn super_j_lists_the_open_worktrees_alone_and_a_project_with_none_open_once() {
        let mut picker = opened(with_unopened(works()));
        let open = [
            "lune › fix",
            "web › api",
            "lune › main",
            "lune › docs",
            "notes",
        ];
        assert_eq!(
            rows(&picker),
            open,
            "no worktree that is not open is listed"
        );
        typed(&mut picker, "spik");
        assert_eq!(
            rows(&picker),
            ["+ spik"],
            "a part of a name not open is n w's row"
        );
        typed(&mut picker, "e");
        assert_eq!(
            rows(&picker),
            ["lune › spike"],
            "a whole folder name not open is that folder"
        );
        let spike = Picked::Add {
            on: Machine::Local,
            project: project("lune", 1),
            folder: directory("/home/you/lune/spike"),
        };
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            spike,
            "whatever branch spike holds"
        );

        let mut picker = opened(none_open());
        assert_eq!(rows(&picker), open, "notes is one row, not its worktree");
        down_to(&mut picker, "notes");
        assert!(
            list(&picker).1.unwrap().ends_with("none open"),
            "{:?}",
            list(&picker)
        );
        assert!(
            said(&picker).contains("<ENTER> Open a worktree"),
            "{}",
            said(&picker)
        );
        let shown = text(&drawn(&picker), parts().1).join(" ");
        let words = "notes has no worktree open: Enter lists its worktrees not open, as o w does";
        assert!(shown.contains(words), "{shown}");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert_eq!(
            rows(&picker),
            ["notes › jots"],
            "Enter lists notes's as o w does"
        );
        assert!(said(&picker).contains("<ENTER> Open"), "{}", said(&picker));
        let opens = Picked::Add {
            on: Machine::Local,
            project: project("notes", 2),
            folder: directory("/home/you/notes/jots"),
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), opens);
    }

    fn none_open() -> Listing {
        let mut listing = with_unopened(works());
        let jots = projects::Dir::parse("/home/you/notes/jots").unwrap();
        set_unopened(
            &mut listing.shelves[0].projects[1],
            Unopened::Listed(vec![jots]),
        );
        listing
    }

    #[test]
    fn esc_in_a_project_with_none_open_goes_back_to_its_row_and_esc_again_closes() {
        let mut picker = opened(none_open());
        down_to(&mut picker, "notes");
        press(&mut picker, KeyCode::Enter);
        typed(&mut picker, "jo");
        assert_eq!(rows(&picker), ["notes › jots"]);
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Nothing);
        let open = [
            "lune › fix",
            "web › api",
            "lune › main",
            "lune › docs",
            "notes",
        ];
        assert_eq!(
            rows(&picker),
            open,
            "back on Super j's list, the search let go"
        );
        assert_eq!(lit(&picker), "notes", "on the row Enter was pressed on");
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Close);
    }

    #[test]
    fn enter_on_a_project_with_none_open_in_the_palette_lists_its_worktrees_and_esc_goes_back() {
        let mut picker = palette_on(none_open(), at_fix(Origin::Views));
        typed(&mut picker, "notes");
        assert_eq!(keyed_action(&picker).0, "notes");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert_eq!(
            rows(&picker),
            ["notes › jots"],
            "o w's list for notes, not the palette"
        );
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Nothing);
        assert_eq!(
            keyed_action(&picker).0,
            "notes",
            "back in the palette, on notes"
        );
    }

    #[test]
    fn enter_on_a_project_with_none_open_from_o_w_s_palette_lists_that_project_s_instead() {
        let lune = project("lune", 1).id();
        let mut picker = Picker::opening(1, none_open(), (Machine::Local, lune), at_host());
        assert_eq!(rows(&picker), ["lune › old", "lune › spike"]);
        assert_eq!(press(&mut picker, KeyCode::BackTab), Picked::Nothing);
        typed(&mut picker, "notes");
        assert_eq!(keyed_action(&picker).0, "notes");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert_eq!(rows(&picker), ["notes › jots"], "notes's list, not lune's");
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Nothing);
        assert_eq!(
            keyed_action(&picker).0,
            "notes",
            "back in the palette, on notes"
        );
        assert_eq!(press(&mut picker, KeyCode::Tab), Picked::Nothing);
        let open = [
            "lune › fix",
            "web › api",
            "lune › main",
            "lune › docs",
            "notes",
        ];
        assert_eq!(
            rows(&picker),
            open,
            "lune's o w scope went with the step back"
        );
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Close);
    }

    #[test]
    fn o_w_lists_the_worktrees_of_its_project_alone_that_are_not_open() {
        let lune = project("lune", 1).id();
        let listing = with_unopened(works());
        let mut picker = Picker::opening(1, listing, (Machine::Local, lune), at_host());
        assert_eq!(rows(&picker), ["lune › old", "lune › spike"]);
        typed(&mut picker, "zzz");
        assert_eq!(
            rows(&picker),
            ["no match for \"zzz\""],
            "no worktree is made of what is typed"
        );
        typed(&mut picker, "#3");
        let unbranched = "names an issue or a pull request";
        assert!(
            !said(&picker).contains(unbranched),
            "no branch is asked for: {}",
            said(&picker)
        );
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(
            !shown.contains("type a branch"),
            "o w makes no worktree: {shown}"
        );
        let mut all_open = works();
        set_unopened(
            &mut all_open.shelves[0].projects[2],
            Unopened::Listed(vec![projects::Dir::parse("/home/you/web/legacy").unwrap()]),
        );
        let picker = Picker::opening(2, all_open, (Machine::Local, lune), at_host());
        assert_eq!(rows(&picker), ["every worktree is open"]);
        let mut unlisted = works();
        set_unopened(
            &mut unlisted.shelves[0].projects[0],
            Unopened::Unlisted("not listed yet".to_owned()),
        );
        let picker = Picker::opening(3, unlisted, (Machine::Local, lune), at_host());
        let said = "lune's worktrees: not listed yet";
        assert_eq!(
            rows(&picker),
            [said],
            "a clone not listed yet is not said all open"
        );
        let mut folder = works();
        folder.shelves[0].projects[0] = housed_folder(
            "lune",
            1,
            worktree(fix().worktree, "fix", Some(50), &[5, 3]),
        );
        let picker = Picker::opening(4, folder, (Machine::Local, lune), at_host());
        let said = "lune is a folder, and its own is its one worktree";
        assert_eq!(rows(&picker), [said]);
    }

    #[test]
    fn o_w_lists_no_other_machine_even_one_that_keeps_no_project() {
        let mut listing = machines();
        let old = projects::Dir::parse("/home/you/lune/old").unwrap();
        set_unopened(
            &mut listing.shelves[0].projects[0],
            Unopened::Listed(vec![old]),
        );
        let lune = project("lune", 1).id();
        let picker = Picker::opening(1, listing, (Machine::Local, lune), at_host());
        assert_eq!(rows(&picker), ["lune › old"]);
    }

    fn clicks(picker: &Picker, key: &str) -> (Vec<(String, Option<Pick>)>, Option<Pick>) {
        let over = Rect::new(0, 0, 200, 50);
        let mut buffer = Buffer::empty(over);
        let (drawn, _) = picker.draw(&mut buffer, over, now());
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
            Some(Position::new(
                u16::try_from(line[..at].chars().count()).ok()?,
                y,
            ))
        });
        let spot = help.and_then(pick);
        (rows.collect(), spot)
    }

    #[test]
    fn a_click_on_a_row_does_what_enter_does_on_it() {
        let mut picker = opened(works());
        let (rows, tab) = clicks(&picker, "<TAB>");
        assert!(
            rows.len() == 5 && rows.iter().all(|(_, pick)| pick.is_some()),
            "{rows:?}"
        );
        let row = |word: &str| {
            let shown = rows.iter().find(|(it, _)| it == word);
            shown.and_then(|(_, pick)| pick.clone()).expect("a row")
        };
        assert_eq!(tab, Some(Pick::Key(KeyEvent::from(KeyCode::Tab))));
        assert_eq!(clicks(&picker, "<↓↑>").1, None);
        let main = go(Machine::Local, main_place());
        assert_eq!(picker.click(row("lune › main")), main);
        assert_eq!(picker.click(row("lune › fix")), Picked::Close);
        typed(&mut picker, "api");
        assert_eq!(picker.click(row("lune › main")), Picked::Nothing);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        press(&mut picker, KeyCode::Backspace);
        assert_eq!(picker.click(row("lune › main")), main);
        control(&mut picker, 'r');
        assert_eq!(picker.click(row("web › api")), Picked::Nothing);
        let rename = clicks(&picker, "<ENTER>").1;
        assert_eq!(rename, Some(Pick::Key(KeyEvent::from(KeyCode::Enter))));
    }

    #[test]
    fn no_row_is_a_spot_while_a_close_waits_or_asks() {
        let mut picker = opened(works());
        down_to(&mut picker, "lune › main");
        assert!(matches!(
            press(&mut picker, KeyCode::Delete),
            Picked::Plan { .. }
        ));
        let (rows, give_up) = clicks(&picker, "<ESC>");
        assert!(
            rows.len() > 1 && rows.iter().all(|(_, pick)| pick.is_none()),
            "{rows:?}"
        );
        assert_eq!(give_up, Some(Pick::Key(KeyEvent::from(KeyCode::Esc))));
        let terminal = ViewKey::terminal(1).terminal_key().unwrap();
        let (worktree, views) = (main_place().worktree, vec![terminal]);
        let act = Act::Worktree {
            worktree,
            views,
            seen: Vec::new(),
            seen_browsers: Vec::new(),
        };
        let busy = Plan::of(act, Ok(&[(terminal, Doing::Busy)]));
        assert_eq!(picker.planned(Ok(busy)), Picked::Nothing);
        let (rows, yes) = clicks(&picker, "<y>");
        assert!(
            rows.len() > 1 && rows.iter().all(|(_, pick)| pick.is_none()),
            "{rows:?}"
        );
        assert_eq!(yes, Some(Pick::Key(KeyEvent::from(KeyCode::Char('y')))));
    }

    #[test]
    fn what_is_typed_narrows_the_list_and_enter_makes_the_worktree_of_a_branch_no_row_matches() {
        let mut shared = works();
        let api_main = worktree(worktree_id(4, 1), "main", Some(10), &[6]);
        shared.shelves[0]
            .projects
            .push(housed("api", 4, vec![api_main]));
        let mut picker = opened(shared);
        typed(&mut picker, "main");
        assert_eq!(rows(&picker), ["lune › main", "api › main"]);
        retyped(&mut picker, "api");
        assert_eq!(rows(&picker), ["web › api", "api › main"]);
        assert_eq!(found(&picker)[0].1, "api");
        assert_eq!(found(&picker)[1].1, "api");
        let mut cased = works();
        cased.shelves[0].projects.push(housed(
            "Ops",
            4,
            vec![
                worktree(worktree_id(4, 1), "Api-v2", Some(90), &[6]),
                worktree(worktree_id(4, 2), "Api", Some(10), &[7]),
            ],
        ));
        cased.shelves[0].projects.push(housed(
            "Api",
            5,
            vec![worktree(worktree_id(5, 1), "main", Some(80), &[8])],
        ));
        let mut cased_picker = opened(cased);
        typed(&mut cased_picker, "ops › api");
        assert_eq!(rows(&cased_picker), ["Ops › Api", "Ops › Api-v2"]);
        retyped(&mut cased_picker, "Api");
        assert_eq!(
            rows(&cased_picker),
            ["Ops › Api", "Ops › Api-v2", "Api › main"]
        );
        assert!(Finder::of("", Typos::None).is_none());

        let mut ranked = works();
        let mut scratch = worktree(worktree_id(4, 1), "scratch", Some(10), &[6]);
        branched(&mut scratch, "fix-old");
        let mut renamed_api = worktree(worktree_id(4, 2), "Api", Some(20), &[7]);
        renamed_api.folder = projects::Dir::parse("/home/you/ops/api").unwrap();
        branched(&mut renamed_api, "api-v2");
        let mut dot = worktree(worktree_id(4, 3), ".Github", Some(15), &[8]);
        dot.folder = projects::Dir::parse("/home/you/ops/.github").unwrap();
        branched(&mut dot, ".github-ci");
        ranked.shelves[0].projects.push(housed(
            "ops",
            4,
            vec![
                worktree(worktree_id(4, 4), "fix-875", Some(50), &[9]),
                scratch,
                renamed_api,
                dot,
            ],
        ));
        let mut ranked_picker = opened(ranked);
        typed(&mut ranked_picker, "fix-");
        assert_eq!(rows(&ranked_picker), ["ops › fix-875", "ops › scratch"]);
        assert_eq!(found(&ranked_picker)[0].1, "fix-");
        assert_eq!(found(&ranked_picker)[1].1, "");
        retyped(&mut ranked_picker, "api");
        assert_eq!(rows(&ranked_picker), ["web › api", "ops › Api"]);
        assert_eq!(found(&ranked_picker)[1].1, "Api");
        retyped(&mut ranked_picker, "ap");
        assert_eq!(rows(&ranked_picker), ["web › api", "ops › Api"]);
        assert_eq!(found(&ranked_picker)[1].1, "Ap");
        retyped(&mut ranked_picker, ".git");
        assert_eq!(rows(&ranked_picker), ["ops › .Github"]);
        assert_eq!(found(&ranked_picker)[0].1, ".Git");

        let mut picker = opened(works());
        typed(&mut picker, "api");
        assert_eq!(rows(&picker), ["web › api"]);
        assert_eq!(found(&picker)[0].1, "api");
        retyped(&mut picker, "lune");
        assert_eq!(rows(&picker), ["lune › fix", "lune › main", "lune › docs"]);
        assert_eq!(lit(&picker), "lune › fix");
        assert!(found(&picker)[0].1.starts_with("lune─"));
        assert_eq!(found(&picker)[1].1, "lune");
        assert_eq!(found(&picker)[2].1, "lune");
        retyped(&mut picker, "zzz");
        let (lines, lit) = list(&picker);
        let new = lines[0].starts_with("+ zzz ") && lines[0].ends_with(" new worktree");
        assert!(new, "{lines:?}");
        assert_eq!(lit.as_ref(), lines.first());
        let words = r#"<ENTER> New worktree "zzz" in lune"#;
        assert!(said(&picker).contains(words), "{}", said(&picker));
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(
            shown.contains("the worktree of branch zzz in lune, as n w does"),
            "{shown}"
        );
        let branch = Branch::parse("zzz").unwrap();
        let lune = project("lune", 1).id();
        let done = Done::Worktree(Machine::Local, lune, branch);
        let made = Picked::Done {
            done,
            origin: Origin::Views,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        typed(&mut picker, "#3");
        assert_eq!(list(&picker).0, [r#"no match for "zzz#3""#]);
        let numbered = "zzz#3 names an issue or a pull request: paste its link";
        assert!(said(&picker).contains(numbered), "{}", said(&picker));
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(
            shown.contains("type a branch to make its worktree"),
            "{shown}"
        );
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        let mut folder = works();
        folder.shelves[0].projects[0] = housed_folder(
            "lune",
            1,
            worktree(fix().worktree, "fix", Some(50), &[5, 3]),
        );
        let mut picker = opened(folder);
        typed(&mut picker, "zzz");
        assert_eq!(list(&picker).0, [r#"no match for "zzz""#]);
        let refused = projects::folder_has_one_worktree("lune");
        assert!(said(&picker).contains(&refused), "{}", said(&picker));
        let shown = text(&drawn(&picker), preview).join(" ");
        assert!(
            !shown.contains("type a branch"),
            "a folder makes no worktree: {shown}"
        );
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        let mut picker = opened(Listing {
            shown: None,
            ..works()
        });
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
        for title in ["Agents", "Projects"] {
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
        assert!(titled(&picker, "Projects"));
    }

    #[test]
    fn del_on_a_worktree_plans_its_close_as_d_d_does() {
        let mut picker = opened(works());
        assert_eq!(lit(&picker), "web › api");
        let session = api().project.listed().clone();
        let scope = close::Scope::Worktree(api().worktree);
        let planning = Picked::Plan {
            on: Machine::Local,
            session,
            scope,
        };
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        let terminal = ViewKey::terminal(4).terminal_key().unwrap();
        let (worktree, views) = (api().worktree, vec![terminal]);
        let act = Act::Worktree {
            worktree,
            views,
            seen: Vec::new(),
            seen_browsers: Vec::new(),
        };
        let plan = || Plan::of(act.clone(), Ok(&[(terminal, Doing::Busy)]));
        assert_eq!(picker.planned(Ok(plan())), Picked::Nothing);
        let asks = "Close the worktree? 1 agent, 1 busy";
        assert!(said(&picker).contains(asks), "{}", said(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        assert_eq!(
            picker.planned(Ok(plan())),
            Picked::Act {
                on: Machine::Local,
                act
            }
        );
        assert_eq!(
            picker.asks(),
            [] as [Ask; 0],
            "the host closes it, as it closes a view"
        );

        let gone = "worktree 31 is no longer listed";
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Err(gone.to_owned())), Picked::Nothing);
        assert!(said(&picker).contains(gone), "{}", said(&picker));

        down_to(&mut picker, "notes");
        press(&mut picker, KeyCode::Delete);
        assert!(
            said(&picker).contains("notes holds no worktree to close"),
            "{}",
            said(&picker)
        );
    }

    fn renamed(on: Machine, rename: menu::Rename) -> Picked {
        Picked::Done {
            done: Done::Rename(on, rename),
            origin: Origin::Views,
        }
    }

    #[test]
    fn ctrl_r_renames_the_highlighted_worktree_or_empty_project_in_the_chain() {
        let mut picker = opened(works());
        control(&mut picker, 'r');
        let asked = said(&picker);
        assert!(asked.contains("─ Rename ─"), "{asked}");
        assert!(
            asked.contains("its new name; with none, it goes back"),
            "{asked}"
        );
        assert_eq!(press(&mut picker, KeyCode::Esc), Picked::Nothing);
        assert_eq!(lit(&picker), "web › api", "Esc goes back to the list");

        press(&mut picker, KeyCode::Up);
        control(&mut picker, 'r');
        let mut renamed_fix = works();
        let relisted = &mut worktrees_mut(&mut renamed_fix.shelves[0].projects[0])[1];
        relisted.named = sidebar::Named::of(non_empty("fixed"), None);
        picker.listed(renamed_fix);
        typed(&mut picker, "patch");
        let input = drawn(&picker);
        let rule = text(&input, Rect::new(0, 0, 200, 50)).join(" ");
        assert!(
            rule.contains("─ fixed ─"),
            "the box names the worktree as listed: {rule}"
        );
        let patch = Some(Name::parse("patch").unwrap());
        let fixed = menu::Rename::Worktree(fix().worktree, patch);
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            renamed(Machine::Local, fixed)
        );
        assert!(
            picker.asks().is_empty(),
            "the sidebar asks luned, not the picker"
        );

        down_to(&mut picker, "notes");
        control(&mut picker, 'r');
        assert!(said(&picker).contains("its new name"), "{}", said(&picker));
        assert!(
            !said(&picker).contains("with none"),
            "a project keeps a name"
        );
        typed(&mut picker, "jots/old");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert!(
            said(&picker).contains("names no project"),
            "{}",
            said(&picker)
        );
        for _ in "jots/old".chars() {
            press(&mut picker, KeyCode::Backspace);
        }
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert!(
            said(&picker).contains("a project keeps a name"),
            "{}",
            said(&picker)
        );
        typed(&mut picker, "jots");
        let jots = ProjectName::parse("jots").unwrap();
        let jots = menu::Rename::Project(project("notes", 2).id(), jots);
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            renamed(Machine::Local, jots)
        );
    }

    #[test]
    fn a_worktree_renamed_from_its_row_or_the_palette_reads_the_same_names_the_same_way() {
        let from_row = |to: &str| {
            let mut picker = opened(works());
            press(&mut picker, KeyCode::Up);
            assert_eq!(lit(&picker), "lune › fix");
            control(&mut picker, 'r');
            typed(&mut picker, to);
            (press(&mut picker, KeyCode::Enter), picker)
        };
        let from_palette = |to: &str| {
            let mut picker = palette(Origin::Views);
            typed(&mut picker, "rename the worktree");
            assert_eq!(keyed_action(&picker).0, "Rename the worktree…");
            press(&mut picker, KeyCode::Enter);
            typed(&mut picker, to);
            (press(&mut picker, KeyCode::Enter), picker)
        };
        let named = |to: &str| Some(Name::parse(to).unwrap());
        let long = "x".repeat(projects::NAME_CHARS_MAX + 1);
        let cases = [
            ("", Some(None)),
            ("fix", Some(named("fix"))),
            ("fixed", Some(named("fixed"))),
            (long.as_str(), None),
        ];
        for (to, outcome) in cases {
            let ((row, row_picker), (palette, palette_picker)) = (from_row(to), from_palette(to));
            assert_eq!(row, palette, "{to:?}");
            let Some(name) = outcome else {
                assert_eq!(row, Picked::Nothing, "{to:?}");
                let why = format!("longer than {}", projects::NAME_CHARS_MAX);
                assert!(said(&row_picker).contains(&why), "{}", said(&row_picker));
                assert!(
                    said(&palette_picker).contains(&why),
                    "{}",
                    said(&palette_picker)
                );
                continue;
            };
            let rename = menu::Rename::Worktree(fix().worktree, name);
            assert_eq!(row, renamed(Machine::Local, rename), "{to:?}");
        }
    }

    fn machines() -> Listing {
        let mut far = shelved(
            Machine::Remote("far".parse().unwrap()),
            vec![housed(
                "old",
                5,
                vec![worktree(worktree_id(5, 1), "stale", Some(10), &[7])],
            )],
        );
        far.away = Some("unreachable");
        let gateway = worktree(worktree_id(4, 1), "gateway", Some(60), &[6]);
        let shelves = vec![
            shelved(Machine::Local, vec![lune()]),
            shelved(nixos(), vec![housed("api", 4, vec![gateway])]),
            far,
            shelved(Machine::Remote("mini".parse().unwrap()), Vec::new()),
        ];
        let mini = ("mini".parse().unwrap(), Runs::default());
        let remotes = vec![("nixos".parse().unwrap(), Runs::default()), mini];
        let (shown, this) = (Some((Machine::Local, fix())), "desk".to_owned());
        Listing {
            shown,
            remotes,
            shelves,
            this,
            ..Listing::default()
        }
    }

    #[test]
    fn every_machine_s_worktrees_are_listed_an_unreachable_one_s_greyed() {
        let mut listing = machines();
        branched(
            &mut worktrees_mut(&mut listing.shelves[2].projects[0])[0],
            "relics",
        );
        let mut picker = opened(listing);
        let shown = [
            "api › gateway",
            "lune › fix",
            "lune › main",
            "old › stale",
            "lune › docs",
            "mini",
        ];
        assert_eq!(rows(&picker), shown);
        let (lines, _) = list(&picker);
        let stale: Vec<&str> = lines[3].split_whitespace().collect();
        assert_eq!([lines[0].as_str()], ["api › gateway"], "no machine tag");
        assert_eq!(stale, ["old", "›", "stale", "relics"], "no machine tag");
        assert!(lines[5].ends_with("no projects"), "{lines:?}");
        let buffer = drawn(&picker);
        let (results, _) = parts();
        let shown = text(&buffer, results);
        let dimmed = |row: &str| {
            let y = shown
                .iter()
                .position(|line| line.starts_with(row))
                .expect("a row");
            let y = results.y + u16::try_from(y).unwrap();
            buffer[(results.x + 1, y)].modifier.contains(Modifier::DIM)
        };
        assert!(dimmed("old › stale"));
        assert!(!dimmed("lune › main"));
        let at = shown
            .iter()
            .position(|line| line.starts_with("old › stale"))
            .expect("a row");
        let last = buffer[(results.right() - 2, results.y + u16::try_from(at).unwrap())].clone();
        assert_eq!(last.symbol(), "s", "stale's branch ends its row");
        assert!(
            last.modifier.contains(Modifier::DIM),
            "and is dimmed with it"
        );
        assert_eq!(lit(&picker), "api › gateway");
        assert!(
            said(&picker).contains("<ENTER> Go on nixos"),
            "{}",
            said(&picker)
        );
        let gateway = place("api", 4, 1);
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            go(nixos(), gateway.clone())
        );
        let session = gateway.project.listed().clone();
        let scope = close::Scope::Worktree(gateway.worktree);
        let planning = Picked::Plan {
            on: nixos(),
            session,
            scope,
        };
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        press(&mut picker, KeyCode::Esc);
        control(&mut picker, 'r');
        assert!(
            said(&picker).contains("─ gateway on nixos ─"),
            "{}",
            said(&picker)
        );
        typed(&mut picker, "gw");
        let gw = menu::Rename::Worktree(gateway.worktree, Some(Name::parse("gw").unwrap()));
        assert_eq!(press(&mut picker, KeyCode::Enter), renamed(nixos(), gw));

        down_to(&mut picker, "old › stale");
        let far = Machine::Remote("far".parse().unwrap());
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            go(far.clone(), place("old", 5, 1))
        );
        let stale = place("old", 5, 1);
        let scope = close::Scope::Worktree(stale.worktree);
        let planning = Picked::Plan {
            on: far,
            session: stale.project.listed().clone(),
            scope,
        };
        assert_eq!(
            press(&mut picker, KeyCode::Delete),
            planning,
            "the sidebar says it is away"
        );
        press(&mut picker, KeyCode::Esc);
        down_to(&mut picker, "mini");
        assert!(
            said(&picker).contains("mini keeps no project"),
            "{}",
            said(&picker)
        );
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);

        typed(&mut picker, "a");
        assert!(
            !rows(&picker).contains(&"mini".to_owned()),
            "{:?}",
            rows(&picker)
        );
    }

    #[test]
    fn the_preview_shows_the_terminal_lune_is_on_else_the_worktree_s_first() {
        let mut picker = opened(works());
        let peek = |session: Listed, view| Ask::Peek {
            on: Machine::Local,
            session,
            asked: Some(terminal_key(view)),
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
        let lune = || fix().project.listed().clone();
        assert_eq!(picker.asks(), [peek(lune(), 3)]);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [peek(lune(), 1)]);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.due(), None);
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview);
        assert_eq!(
            shown[..3],
            ["/home/you/docs", "", "it holds no terminal to look at"]
        );
    }

    #[test]
    fn the_preview_shows_the_last_lines_of_the_screen_inside_its_box() {
        let mut picker = opened(works());
        let dumped = (1..=40)
            .map(|n| format!("\x1b[mline {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let peek = Ok((4, format!("{dumped}\n\n\n")));
        let (session, asked) = (api().project.listed().clone(), Some(terminal_key(4)));
        picker.heard(Heard::Peek {
            on: Machine::Local,
            session,
            asked,
            peek,
        });
        let buffer = drawn(&picker);
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        assert_eq!((area.x, area.y, area.width, area.height), (40, 10, 120, 30));
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(buffer[(area.right() - 1, area.bottom() - 1)].symbol(), "┘");
        let (_, preview) = parts();
        let lines = text(&buffer, preview);
        assert_eq!(lines[0], "/home/you/api");
        let rule = text(
            &buffer,
            Rect {
                y: preview.y - 1,
                height: 1,
                ..preview
            },
        );
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
        let asked = Some(terminal_key(6));
        assert_eq!(
            picker.asks(),
            [Ask::Peek {
                on: nixos(),
                session: session.clone(),
                asked
            }]
        );
        let peek = |on, session, screen: &str| {
            let peek = Ok((6, screen.to_owned()));
            Heard::Peek {
                on,
                session,
                asked,
                peek,
            }
        };
        let shown = |picker: &Picker| text(&drawn(picker), parts().1);
        picker.heard(peek(Machine::Local, session.clone(), "here"));
        assert_eq!(
            shown(&picker)[..3],
            ["nixos · /home/you/gateway", "", "reading…"]
        );
        picker.heard(peek(nixos(), session, "there"));
        assert_eq!(
            shown(&picker)[..3],
            ["nixos · /home/you/gateway", "", "there"]
        );
    }

    #[test]
    fn a_preview_that_failed_is_asked_for_again_as_the_highlight_comes_back() {
        let mut picker = opened(works());
        picker.ticked(picker.due().unwrap());
        let (session, asked) = (api().project.listed().clone(), Some(terminal_key(4)));
        let peek = || Ask::Peek {
            on: Machine::Local,
            session: session.clone(),
            asked,
        };
        assert_eq!(picker.asks(), [peek()]);
        let failed = Err("lune-mux did not answer".to_owned());
        let on = Machine::Local;
        picker.heard(Heard::Peek {
            on,
            session: session.clone(),
            asked,
            peek: failed,
        });
        assert_eq!(picker.due(), None);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Up);
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [peek()]);

        let path = || directory("/home/you/lune");
        let super_o = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::SUPER);
        picker.key(&super_o);
        picker.heard(heard_here(Ok(vec![repository("/home/you/lune")])));
        picker.ticked(picker.due().unwrap());
        let list = || Ask::List {
            on: Machine::Local,
            path: path(),
        };
        assert_eq!(picker.asks(), [list()]);
        let failed = Err("Permission denied".to_owned());
        picker.heard(Heard::Listed {
            on: Machine::Local,
            path: path(),
            listed: failed,
        });
        assert_eq!(picker.due(), None);
        picker.key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::SUPER));
        picker.key(&super_o);
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
        assert_eq!(picker.list, List::Worktrees, "Ctrl d is the terminal's");
        picker.key(&KeyEvent::new(KeyCode::Char('o'), KeyModifiers::SUPER));
        assert_eq!(picker.list, List::Projects);
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
        let scope = close::Scope::View(ViewKey::terminal(4));
        let planning = Picked::Plan {
            on: Machine::Local,
            session: c.clone(),
            scope,
        };
        let act = || Act::View(ViewKey::terminal(4));
        let plan = |doing: Doing| {
            Plan::of(
                act(),
                Ok(&[(ViewKey::terminal(4).terminal_key().unwrap(), doing)]),
            )
        };
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
        let ended = Picked::Act {
            on: Machine::Local,
            act: act(),
        };
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
        let scope = close::Scope::View(ViewKey::terminal(4));
        let plan = Picked::Plan {
            on: Machine::Local,
            session: listed("c", 30),
            scope,
        };
        assert_eq!(press(&mut picker, KeyCode::Delete), plan);
        assert_eq!(picker.asks(), []);
    }

    #[test]
    fn agents_named_alike_are_told_apart_and_yours_is_pinned_over_them() {
        let place = ViewPlace::new(self::place("work", 20, 1), ViewKey::terminal(9));
        let listing = Listing {
            here: Some(Here::View {
                on: Machine::Local,
                place,
            }),
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
        assert!(
            lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"),
            "{lines:?}"
        );
        let name = |line: &String| {
            let rest: String = line.chars().skip(2).collect();
            rest.split("  ").next().unwrap_or("").to_owned()
        };
        let named: Vec<String> = lines[1..].iter().map(name).collect();
        assert_eq!(
            named,
            ["fix:1", "fix:2", "docs (work:3)", "docs (review:4)"]
        );
        assert!(lit.is_some_and(|lit| lit.contains("fix:1")));
        typed(&mut picker, "review");
        let rows = found(&picker);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let (line, lit) = &rows[1];
        assert!(
            line.contains("docs (review:4)") && lit == "review",
            "{rows:?}"
        );
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
                at(3, "mux panics", "misc/lune"),
                at(4, "docs", "lune/main"),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        typed(&mut picker, "mux");
        let rows = found(&picker);
        let shown = [
            ("mux panics", "mux"),
            ("Zero-length lines in jobs", ""),
            ("fix", ""),
        ];
        assert_eq!(rows.len(), shown.len(), "{rows:?}");
        for ((line, lit), (label, lights)) in rows.iter().zip(shown) {
            assert!(line.contains(label) && lit == lights, "{rows:?}");
        }
    }

    #[test]
    fn the_projects_go_by_their_last_use_then_the_clones_wt_made_that_are_not_projects() {
        let mut picker = Picker::open(1, List::Projects, works(), Machine::Local, at_host());
        assert_eq!(names(&picker), ["lune", "web", "notes"]);
        picker.heard(heard_here(Ok(vec![
            repository("/home/you/lune"),
            repository("/home/you/Work/bipa"),
            repository("/srv/notes"),
        ])));
        assert_eq!(names(&picker), ["lune", "web", "notes", "bipa", "notes-2"]);
        let (lines, _) = list(&picker);
        assert!(lines[0].ends_with(" /home/you/lune"), "{lines:?}");
        assert!(
            said(&picker).contains("<ENTER> you are in it"),
            "{}",
            said(&picker)
        );
        let used_last = "lune is on fix, used last";
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Picked::Close,
            "{used_last}"
        );
        down_to(&mut picker, "web");
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            go(Machine::Local, api())
        );
        down_to(&mut picker, "notes");
        let home = Place::Session(project("notes", 2).listed().clone());
        let home = Picked::Go {
            on: Machine::Local,
            place: home,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), home);
        down_to(&mut picker, "notes-2");
        assert!(
            said(&picker).contains(r#"<ENTER> Make "notes-2""#),
            "{}",
            said(&picker)
        );
        let folder = crate::worktrees::Folder::from(&projects::Dir::parse("/srv/notes").unwrap());
        let made = Picked::Make {
            on: Machine::Local,
            folder,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        picker.asks();
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), [] as [Ask; 0]);
    }

    #[test]
    fn a_typed_folder_makes_the_project_of_it_on_each_machine() {
        let mut picker = Picker::open(1, List::Projects, two_machines(), Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![repository("/home/you/lune")])));
        picker.heard(heard_there(2, Ok(Vec::new())));
        typed(&mut picker, "code/app");
        assert_eq!(
            list(&picker).0,
            ["no match for \"code/app\""],
            "no path, no folder to make"
        );
        retyped(&mut picker, "~/code/app");
        assert_eq!(grouped(&picker), ["── desk", "+", "── nixos", "+"]);
        let (lines, _) = list(&picker);
        let words: Vec<&str> = lines[1].split_whitespace().collect();
        assert_eq!(words, ["+", "~/code/app", "new", "project"]);
        assert!(said(&picker).contains("Enter makes the project of ~/code/app, named for it"));
        assert!(
            said(&picker).contains("<ENTER> Make the project"),
            "{}",
            said(&picker)
        );
        let folder = crate::worktrees::Folder::typed("~/code/app").unwrap();
        let made = |on| Picked::Make {
            on,
            folder: folder.clone(),
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), made(Machine::Local));
        press(&mut picker, KeyCode::Down);
        assert_eq!(press(&mut picker, KeyCode::Enter), made(nixos()));
        retyped(&mut picker, "/home/you/lune/");
        let held_here = ["── nixos", "+"];
        assert_eq!(
            grouped(&picker),
            held_here,
            "each machine's own clone folder"
        );
        for held in [
            "~",
            "~/",
            "~/lune",
            "~/lune/",
            "/",
            "//",
            "/home",
            "~/code/..",
            "~/./code",
        ] {
            retyped(&mut picker, held);
            let rows = grouped(&picker);
            assert!(
                !rows.contains(&"+".to_owned()),
                "no project holds ~/lune: {held} {rows:?}"
            );
        }
        retyped(&mut picker, "/home/you/lune/app");
        let both = ["── desk", "+", "── nixos", "+"];
        assert_eq!(
            grouped(&picker),
            both,
            "a folder in it holds no clone folder"
        );
    }

    #[test]
    fn no_project_takes_the_name_of_the_one_lune_is_on_its_way_to() {
        let listing = Listing {
            here: Some(here("notes")),
            shelves: vec![shelved(Machine::Local, vec![housed("lune", 1, Vec::new())])],
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Projects, listing, Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![
            repository("/srv/notes"),
            repository("/srv/lune"),
        ])));
        assert_eq!(names(&picker), ["lune", "notes-2", "lune-2"]);
    }

    #[test]
    fn a_search_finds_a_repository_by_its_path_or_its_name() {
        let far = "/home/you/Projects/misc/far/away/down/lune-main";
        let none = Listing::default();
        let mut picker = Picker::open(1, List::Projects, none, Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![
            repository(far),
            repository("/home/you/lune"),
            repository("/srv/app"),
        ])));
        typed(&mut picker, "lune");
        assert_eq!(names(&picker), ["lune", "lune-main"]);
        typed(&mut picker, "-m");
        assert_eq!(names(&picker), ["lune-main"]);
        retyped(&mut picker, "down/l");
        let buffer = drawn(&picker);
        let (results, _) = parts();
        let y = (results.y..results.bottom())
            .find(|y| buffer[(results.x, *y)].bg == LIT)
            .unwrap();
        let cells: Vec<_> = (results.x..results.right())
            .map(|x| &buffer[(x, y)])
            .collect();
        let row: String = cells.iter().map(|cell| cell.symbol()).collect();
        let lit: String = cells
            .iter()
            .filter(|cell| cell.fg == ACCENT)
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            row.trim_end()
                .ends_with("…e/you/Projects/misc/far/away/down/lune-main"),
            "{row}"
        );
        assert_eq!(lit, "down/l");
        retyped(&mut picker, "lnue");
        assert_eq!(
            names(&picker),
            ["lune-main", "lune"],
            "a typo is forgiven where none fits"
        );
    }

    #[test]
    fn the_preview_lists_what_eza_says_is_in_the_folder() {
        let path = "/home/you/lune";
        let mut picker = opened(works());
        picker.key(&KeyEvent::new(KeyCode::Char('o'), KeyModifiers::SUPER));
        picker.heard(heard_here(Ok(Vec::new())));
        picker.asks();
        let due = picker.due().unwrap();
        picker.ticked(due);
        assert_eq!(
            picker.asks(),
            vec![Ask::List {
                on: Machine::Local,
                path: directory(path)
            }]
        );
        let preview = |picker: &Picker| {
            let buffer = drawn(picker);
            let (_, preview) = parts();
            (text(&buffer, preview), text(&buffer, buffer.area).join(" "))
        };
        assert_eq!(preview(&picker).0[..3], [path, "", "reading…"]);
        let listed = Ok("\x1b[34msrc\x1b[0m/\nCargo.toml\nREADME.md\n".to_owned());
        picker.heard(Heard::Listed {
            on: Machine::Local,
            path: directory(path),
            listed,
        });
        let (lines, said) = preview(&picker);
        assert_eq!(lines[..5], [path, "", "src/", "Cargo.toml", "README.md"]);
        assert!(said.contains("3 items"), "{said}");
        let listed = Err("Permission denied".to_owned());
        picker.heard(Heard::Listed {
            on: Machine::Local,
            path: directory(path),
            listed,
        });
        assert_eq!(preview(&picker).0[..3], [path, "", "Permission denied"]);
    }

    #[test]
    fn every_machine_s_projects_go_under_its_rule_this_one_s_first() {
        let mut picker = Picker::open(1, List::Projects, two_machines(), Machine::Local, at_host());
        let sought = |on: Machine, ask: u64| Ask::Repositories { on, ask };
        assert_eq!(
            picker.asks(),
            [sought(Machine::Local, 1), sought(nixos(), 2)]
        );
        let api = "/home/me/Projects/api";
        let there = vec![repository(api), repository("/home/me/blog")];
        picker.heard(heard_there(2, Ok(there)));
        let lune = repository("/home/you/lune");
        let here = vec![lune, repository("/home/you/blog")];
        picker.heard(heard_here(Ok(here)));
        let rows = ["── desk", "lune", "blog", "── nixos", "api", "blog"];
        assert_eq!(grouped(&picker), rows);
        assert_eq!(lit(&picker), "lune");

        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "api");
        assert!(
            said(&picker).contains(r#"<ENTER> Make "api" on nixos"#),
            "{}",
            said(&picker)
        );
        let folder = crate::worktrees::Folder::from(&projects::Dir::parse(api).unwrap());
        let made = Picked::Make {
            on: nixos(),
            folder,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), made);
        picker.ticked(picker.due().unwrap());
        assert_eq!(
            picker.asks(),
            vec![Ask::List {
                on: nixos(),
                path: directory(api)
            }]
        );
        let listed = Ok("src/\nCargo.toml\n".to_owned());
        picker.heard(Heard::Listed {
            on: nixos(),
            path: directory(api),
            listed,
        });
        let (_, preview) = parts();
        let shown = text(&drawn(&picker), preview);
        assert_eq!(
            shown[..4],
            ["nixos · /home/me/Projects/api", "", "src/", "Cargo.toml"]
        );

        typed(&mut picker, "blog");
        assert_eq!(grouped(&picker), ["── desk", "blog", "── nixos", "blog"]);
        assert_eq!(lit(&picker), "blog");
    }

    #[test]
    fn super_o_on_a_machine_s_header_has_the_highlight_on_its_projects() {
        let here = || heard_here(Ok(vec![repository("/home/you/notes")]));
        let there = || {
            let api = repository("/home/me/api");
            let found = vec![api, repository("/home/me/blog")];
            heard_there(2, Ok(found))
        };
        let mut picker = Picker::open(1, List::Projects, two_machines(), nixos(), at_host());
        picker.heard(here());
        assert_eq!(lit(&picker), "notes");
        picker.heard(there());
        assert_eq!(lit(&picker), "api");
        press(&mut picker, KeyCode::Down);
        picker.listed(two_machines());
        assert_eq!(lit(&picker), "blog");

        let mut picker = Picker::open(1, List::Projects, two_machines(), nixos(), at_host());
        picker.heard(there());
        picker.heard(here());
        assert_eq!(lit(&picker), "api");

        let mut picker = Picker::open(1, List::Projects, two_machines(), nixos(), at_host());
        picker.heard(here());
        press(&mut picker, KeyCode::Up);
        picker.heard(there());
        assert_eq!(lit(&picker), "notes");
    }

    #[test]
    fn each_machine_s_repositories_are_asked_once_and_say_why_none_were_found() {
        let sought = |picker: &mut Picker| -> Vec<Ask> {
            let asks = picker.asks().into_iter();
            asks.filter(|ask| matches!(ask, Ask::Repositories { .. }))
                .collect()
        };
        let none = Listing::default();
        let mut picker = Picker::open(1, List::Projects, none, Machine::Local, at_host());
        assert_eq!(
            sought(&mut picker),
            [Ask::Repositories {
                on: Machine::Local,
                ask: 1
            }]
        );
        picker.listed(two_machines());
        assert_eq!(
            sought(&mut picker),
            [Ask::Repositories {
                on: nixos(),
                ask: 2
            }]
        );
        picker.listed(two_machines());
        assert_eq!(picker.asks(), [] as [Ask; 0]);
        assert_eq!(list(&picker).0, ["looking for repositories…"]);
        picker.heard(heard_here(Ok(vec![repository("/home/you/notes")])));
        let older = "nixos: its luned, or this one, is older than what lune asked of it";
        let failed = Err(older.to_owned());
        picker.heard(heard_there(2, failed));
        let (lines, _) = list(&picker);
        assert_eq!(lines[..2].join(" "), older);
        assert_eq!(grouped(&picker)[2..], ["── desk", "notes"]);

        let gone = || Listing {
            remotes: Vec::new(),
            ..two_machines()
        };
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(
            sought(&mut picker),
            [Ask::Repositories {
                on: nixos(),
                ask: 3
            }]
        );
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(
            sought(&mut picker),
            [Ask::Repositories {
                on: nixos(),
                ask: 4
            }]
        );
        let lost = Err("nixos: luned does not reach nixos now".to_owned());
        picker.heard(heard_there(3, lost));
        let there = Ok(vec![repository("/home/me/api")]);
        picker.heard(heard_there(4, there));
        assert_eq!(grouped(&picker), ["── desk", "notes", "── nixos", "api"]);
    }

    #[test]
    fn the_projects_say_why_no_repository_was_found() {
        let none = Listing::default();
        let mut picker = Picker::open(1, List::Projects, none, Machine::Local, at_host());
        picker.heard(heard_here(Err("luned does not reach nixos now".to_owned())));
        let (lines, _) = list(&picker);
        assert_eq!(lines[0], "luned does not reach nixos now");
        assert_eq!(lines[1..].join(" "), NO_PROJECTS);
    }

    #[test]
    fn the_clones_found_are_listed_and_what_was_left_unread_is_said_above_them() {
        let none = Listing::default();
        let mut picker = Picker::open(1, List::Projects, none, Machine::Local, at_host());
        let said = "zoxide is not on the PATH, so only the clones in ~/lune are listed";
        let repositories = vec![repository("/home/you/lune/app")];
        let unread = vec![said.to_owned()];
        let found = Ok(repositories::Found {
            repositories,
            owned: None,
            unread,
        });
        picker.heard(Heard::Repositories {
            on: Machine::Local,
            ask: 1,
            found,
        });
        let (lines, _) = list(&picker);
        assert_eq!(lines[..2].join(" "), said);
        assert_eq!(names(&picker).last().map(String::as_str), Some("app"));
    }

    #[test]
    fn another_machine_s_clone_takes_a_name_free_there_and_one_holding_a_project_is_left_out() {
        let shelves = vec![shelved(nixos(), vec![housed("api", 1, Vec::new())])];
        let listing = Listing {
            shelves,
            ..two_machines()
        };
        let mut picker = Picker::open(1, List::Projects, listing, Machine::Local, at_host());
        picker.heard(heard_here(Ok(Vec::new())));
        let there = vec![repository("/srv/api"), repository("/home/you")];
        picker.heard(heard_there(2, Ok(there)));
        assert_eq!(grouped(&picker)[1..], ["── nixos", "api", "api-2"]);
        let (lines, _) = list(&picker);
        assert_eq!(lines[0], "nixos: left out 1 clones a project lies in");
    }

    #[test]
    fn a_project_of_another_machine_takes_no_name_here() {
        let shelves = vec![shelved(nixos(), vec![housed("api", 1, Vec::new())])];
        let listing = Listing {
            shelves,
            ..two_machines()
        };
        let mut picker = Picker::open(1, List::Projects, listing, Machine::Local, at_host());
        picker.heard(heard_here(Ok(vec![repository("/srv/api")])));
        assert_eq!(grouped(&picker), ["── desk", "api", "── nixos", "api"]);
    }

    #[test]
    fn lune_s_agent_on_another_machine_is_pinned_over_that_machine_s_rows() {
        let at = |view| ViewPlace::new(place("api", 40, 1), view);
        let there = agents(vec![
            seated("api", 40, 9, "mine", Mark::Busy),
            seated("api", 40, 2, "fix", Mark::Idle),
        ]);
        let listing = Listing {
            here: Some(Here::View {
                on: nixos(),
                place: at(ViewKey::terminal(9)),
            }),
            local: agents(vec![seated("api", 30, 1, "fix", Mark::Idle)]),
            ..on_nixos(there)
        };
        let mut picker = Picker::open(1, List::Agents, listing, Machine::Local, at_host());
        picker.asks();
        let (lines, _) = list(&picker);
        assert!(
            lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"),
            "{lines:?}"
        );
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
        let fix = Picked::Go {
            on: nixos(),
            place: Place::View(at(ViewKey::terminal(2))),
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), fix);
        let scope = close::Scope::View(ViewKey::terminal(2));
        let plan = Picked::Plan {
            on: nixos(),
            session: listed("api", 40),
            scope,
        };
        assert_eq!(press(&mut picker, KeyCode::Delete), plan);
    }

    fn asked(ask: Ask, projected: &Projected) -> Heard {
        let (events, heard) = std::sync::mpsc::channel();
        super::ask(1, ask, projected, &events);
        match lune_waits::heard("the picker's ask", &heard) {
            Event::Picker { number: 1, heard } => heard,
            _ => panic!("the picker hears another event"),
        }
    }

    #[test]
    fn a_peek_at_another_machine_s_view_is_not_done_yet() {
        let projected = Projected::default();
        let session = Listed::new_project("work", 7);
        let far = Machine::Remote("far".parse().unwrap());
        let peek = Ask::Peek {
            on: far,
            session,
            asked: Some(terminal_key(4)),
        };
        let peeked = asked(peek, &projected);
        let Heard::Peek { peek, .. } = peeked else {
            panic!("a peek is answered with a screen");
        };
        assert_eq!(peek, Err(crate::host::NOT_YET.to_owned()));
    }

    #[test]
    fn the_palette_names_a_worktree_in_its_project_and_a_typed_project_starts_it() {
        let mut listing = works();
        branched(
            &mut worktrees_mut(&mut listing.shelves[0].projects[2])[0],
            "feat/api",
        );
        let mut picker = palette_on(listing, at_fix(Origin::Views));
        typed(&mut picker, "web");
        let (shown, _) = shown_actions(&picker);
        let words: Vec<&str> = shown[0].split_whitespace().collect();
        assert_eq!(words, ["web", "›", "api", "feat/api"], "{shown:?}");
        retyped(&mut picker, "lune");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        let started = ["Quit lune", "lune › fix", "lune › main", "lune › docs"];
        assert_eq!(shown[..4], started, "before any action lune fits loosely");
    }

    #[test]
    fn the_palette_lists_the_actions_fitting_its_view_then_the_worktrees_by_their_last_use() {
        let mut picker = palette(Origin::Views);
        let every = every_action(&mut picker);
        let worktrees = every.iter().position(|row| row == "lune › fix").unwrap();
        let (actions, worktrees) = every.split_at(worktrees);
        let by_use = [
            "lune › fix",
            "web › api",
            "lune › main",
            "lune › docs",
            "notes",
        ];
        assert_eq!(worktrees, by_use);
        assert_eq!(
            actions[..3],
            ["Search skills", "Ask", "/thermo-nuclear-review"]
        );
        let fitting = [
            "Rename the view…",
            "Close the view",
            "Rename the worktree…",
            "Close the worktree",
            "New worktree…",
            "Pick a worktree",
        ];
        for fitting in fitting {
            assert!(
                actions.iter().any(|action| action == fitting),
                "{fitting}: {actions:?}"
            );
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
            assert!(
                !actions.iter().any(|action| action == unfitting),
                "{unfitting}: {actions:?}"
            );
        }
        let mut picker = palette(Origin::Sidebar);
        let every = every_action(&mut picker);
        for fitting in ["Search", "Unread", "Rename the view…"] {
            assert!(
                every.iter().any(|action| action == fitting),
                "{fitting}: {every:?}"
            );
        }
        let worktree = "Rename the worktree…";
        assert!(
            !every.iter().any(|action| action == worktree),
            "the row alone: {every:?}"
        );
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
            ("quit", "Quit lune", "Alt a q"),
            ("pick a worktree", "Pick a worktree", "Super j"),
            (
                "view left",
                "View left, the worktree before, or the sidebar",
                "Ctrl h",
            ),
        ];
        for (query, name, spelled) in views {
            assert_eq!(
                key(Origin::Views, query),
                (name.to_owned(), spelled.to_owned())
            );
        }
        let sidebar = [
            ("new view", "New view", "n v"),
            ("search skills", "Search skills", "m /"),
            ("quit", "Quit lune", "q"),
            ("unread", "Unread", "u"),
        ];
        for (query, name, spelled) in sidebar {
            assert_eq!(
                key(Origin::Sidebar, query),
                (name.to_owned(), spelled.to_owned())
            );
        }
    }

    #[test]
    fn a_typed_word_ranks_what_it_names_then_what_it_starts_then_loose_matches() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "fix");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        let fix = [
            "lune › fix",
            "Fix the last command",
            "Fix the failing checks",
        ];
        assert_eq!(shown[..3], fix, "fix is exact for the worktree named fix");
        retyped(&mut picker, "fix the");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        assert_eq!(
            shown[..2],
            ["Fix the last command", "Fix the failing checks"]
        );
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
    fn a_letter_lights_the_action_whose_title_it_starts_over_a_keyword_or_a_later_word() {
        for origin in [Origin::Views, Origin::Sidebar] {
            for query in ["c", "cl"] {
                let mut picker = palette(origin);
                typed(&mut picker, query);
                assert_eq!(
                    keyed_action(&picker).0,
                    "Close the view",
                    "{origin:?} {query:?}"
                );
                let closes = Picked::Menu {
                    target: fix_view(),
                    origin,
                    action: Action::CloseView,
                };
                assert_eq!(
                    press(&mut picker, KeyCode::Enter),
                    closes,
                    "{origin:?} {query:?}"
                );
            }
        }
    }

    #[test]
    fn a_worktree_whose_name_the_query_starts_ranks_over_an_action_with_a_later_word_it_starts() {
        for name in ["launch", "launch › retry"] {
            let mut listing = works();
            let launch = worktree(worktree_id(4, 1), name, Some(10), &[5]);
            listing.shelves[0]
                .projects
                .push(housed("site", 4, vec![launch]));
            let mut picker = palette_on(listing, at_fix(Origin::Views));
            typed(&mut picker, "la");
            let (shown, _) = shown_actions(&picker);
            let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
            assert_eq!(shown[0], format!("site › {name}"), "{shown:?}");
            assert!(
                shown.contains(&"Fix the last command".to_owned()),
                "{shown:?}"
            );
        }
    }

    #[test]
    fn a_typed_phrase_ranks_the_action_it_starts_over_a_worktree_it_fits_loosely() {
        let mut listing = works();
        let mapper = worktree(worktree_id(4, 1), "mapper", Some(10), &[5]);
        listing.shelves[0]
            .projects
            .push(housed("renew", 4, vec![mapper]));
        let mut picker = palette_on(listing, at_fix(Origin::Views));
        typed(&mut picker, "new ma");
        let (shown, _) = shown_actions(&picker);
        let shown: Vec<String> = shown.iter().map(|line| named(line)).collect();
        assert_eq!(shown[..2], ["New machine…", "renew › mapper"]);
    }

    #[test]
    fn an_action_that_asks_a_name_asks_it_in_the_palette_and_backspace_goes_back() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "rename");
        assert_eq!(keyed_action(&picker).0, "Rename the view…");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        let asked = said(&picker);
        assert!(asked.contains("─ Rename ─"), "{asked}");
        assert!(
            asked.contains("its new name; with none, it goes back"),
            "{asked}"
        );
        let target = "lune › fix · view 3";
        assert!(
            asked.contains(target),
            "the target is named over the input: {asked}"
        );
        let mut renamed = works();
        let relisted = &mut worktrees_mut(&mut renamed.shelves[0].projects[0])[1];
        relisted.named = sidebar::Named::of(non_empty("fixed"), None);
        picker.listed(renamed);
        let asked = said(&picker);
        assert!(
            asked.contains("lune › fixed · view 3"),
            "it follows the listing: {asked}"
        );
        assert_eq!(press(&mut picker, KeyCode::Backspace), Picked::Nothing);
        assert_eq!(
            keyed_action(&picker).0,
            "Rename the view…",
            "back on the list, as typed"
        );
        press(&mut picker, KeyCode::Enter);
        typed(&mut picker, "logs");
        press(&mut picker, KeyCode::Esc);
        assert_eq!(
            keyed_action(&picker).0,
            "Rename the view…",
            "Esc goes back too"
        );
        press(&mut picker, KeyCode::Enter);
        typed(&mut picker, "logs");
        let logs = Some(Name::parse("logs").unwrap());
        let rename = menu::Rename::Terminal(ViewId::of(3).unwrap(), logs);
        let done = Done::Rename(Machine::Local, rename);
        let origin = Origin::Views;
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Picked::Done { done, origin }
        );
        press(&mut picker, KeyCode::Enter);
        assert_eq!(
            super_k(&mut picker),
            Picked::Close,
            "the key again closes it, chained or not"
        );
    }

    #[test]
    fn enter_on_an_action_hands_it_on_with_the_palette_s_target() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "close the view");
        let (target, origin) = (fix_view(), Origin::Views);
        let act = Picked::Menu {
            target,
            origin,
            action: Action::CloseView,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), act);
        retyped(&mut picker, "sidebar or views");
        assert_eq!(
            press(&mut picker, KeyCode::Enter),
            Picked::Chord(actions::SWAP)
        );
        retyped(&mut picker, "pick an agent");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        assert!(
            said(&picker).contains("─ Agents"),
            "the finder's own list opens in the box"
        );

        let mut picker = palette(Origin::Sidebar);
        typed(&mut picker, "unread");
        let unread = Picked::Row {
            target: fix_view(),
            action: Action::Unread,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), unread);

        let mut listing = works();
        listing.shelves[0].projects[0] = housed_folder(
            "lune",
            1,
            worktree(fix().worktree, "fix", Some(50), &[5, 3]),
        );
        let mut picker = palette_on(listing, at_fix(Origin::Views));
        typed(&mut picker, "new worktree");
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        let refused = projects::folder_has_one_worktree(fix().project.listed().name());
        assert!(said(&picker).contains(&refused), "{}", said(&picker));
    }

    #[test]
    fn an_unplaced_palette_lists_only_the_keys_that_work_anywhere_and_says_why() {
        let why = "palette: lune does not know yet which view its client is on";
        let unplaced = Palette {
            aim: Aim::Unplaced(why),
            origin: Origin::Views,
        };
        let mut picker = palette_on(works(), unplaced);
        let every = every_action(&mut picker);
        let worktrees = every.iter().position(|row| row == "lune › fix").unwrap();
        let anywhere = Action::anywhere()
            .map(|(action, _)| action)
            .filter(|it| it.in_palette());
        let anywhere: Vec<String> = anywhere.map(titled).collect();
        assert_eq!(every[..worktrees], anywhere);
        assert!(
            said(&picker).contains("lune does not know yet"),
            "{}",
            said(&picker)
        );
    }

    #[test]
    fn a_worktree_action_from_a_view_runs_on_its_worktree_and_nothing_found_offers_one() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "close the worktree");
        let (target, origin) = (fix_worktree(), Origin::Views);
        let close = Picked::Menu {
            target,
            origin,
            action: Action::CloseWorktree,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), close);
        retyped(&mut picker, "zzz");
        let made = press(&mut picker, KeyCode::Enter);
        let Picked::Done {
            done: Done::Worktree(_, project, branch),
            ..
        } = &made
        else {
            panic!("nothing found offers to make what was typed: {made:?}");
        };
        assert_eq!((branch.as_str(), *project), ("zzz", fix().project.id()));
    }

    #[test]
    fn enter_on_a_fresh_paste_starts_the_agent() {
        let issue = "https://github.com/lorenzolfm/lune/issues/648";
        let mut picker = palette(Origin::Views);
        picker.paste(&format!("{issue}\n"));
        let work = Work::parse(issue, None).expect("an issue's link");
        let origin = Origin::Views;
        let tackle = Picked::Pull {
            target: fix_view(),
            work,
            pull: Pull::Agent,
            origin,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), tackle);
    }

    #[test]
    fn a_pasted_link_lists_what_the_menu_offers_for_it_and_enter_pulls_it_on_the_target() {
        let issue = "https://github.com/lorenzolfm/lune/issues/648";
        let mut picker = palette(Origin::Views);
        picker.paste(&format!("{issue}\n"));
        assert_eq!(
            every_action(&mut picker),
            ["Tackle #648", "Ship #648", "Worktree #648"]
        );
        assert_eq!(keyed_action(&picker).1, "lorenzolfm/lune");
        let work = Work::parse(issue, None).expect("an issue's link");
        let origin = Origin::Views;
        let worktree = Picked::Pull {
            target: fix_view(),
            work,
            pull: Pull::Worktree,
            origin,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), worktree);

        let pr = "https://github.com/lorenzolfm/lune/pull/650";
        let mut picker = palette(Origin::Sidebar);
        typed(&mut picker, pr);
        assert_eq!(
            every_action(&mut picker),
            ["/thermo-nuclear-review #650", "Ship #650", "Worktree #650"]
        );
        press(&mut picker, KeyCode::Up);
        press(&mut picker, KeyCode::Up);
        let work = Work::parse(pr, None).expect("a pull request's link");
        let origin = Origin::Sidebar;
        let review = Picked::Pull {
            target: fix_view(),
            work,
            pull: Pull::Agent,
            origin,
        };
        assert_eq!(press(&mut picker, KeyCode::Enter), review);

        let why = "palette: lune does not know yet which view its client is on";
        let unplaced = Palette {
            aim: Aim::Unplaced(why),
            origin: Origin::Views,
        };
        let mut picker = palette_on(works(), unplaced);
        picker.paste(issue);
        assert!(
            !every_action(&mut picker)
                .iter()
                .any(|row| row.contains("#648"))
        );
    }

    #[test]
    fn the_palette_asks_for_no_preview_of_a_worktree_it_lights() {
        let mut picker = palette(Origin::Views);
        typed(&mut picker, "fix");
        assert_eq!(keyed_action(&picker).0, "lune › fix");
        assert_eq!(
            picker.due(),
            None,
            "the Actions list draws no preview to ask for"
        );
    }

    #[test]
    fn super_k_opens_the_palette_from_another_list_and_closes_it_and_tab_cycles_all_four() {
        let mut picker = palette(Origin::Views);
        assert_eq!(super_k(&mut picker), Picked::Close);
        assert_eq!(
            press(&mut palette(Origin::Views), KeyCode::Esc),
            Picked::Close
        );
        let aimed = at_fix(Origin::Views);
        let mut picker = Picker::open(1, List::Worktrees, works(), Machine::Local, aimed);
        assert_eq!(super_k(&mut picker), Picked::Nothing);
        assert!(said(&picker).contains("─ Actions"), "{}", said(&picker));
        for list in ["Worktrees", "Agents", "Projects", "Actions"] {
            press(&mut picker, KeyCode::Tab);
            assert!(
                said(&picker).contains(&format!("─ {list}")),
                "{list}: {}",
                said(&picker)
            );
        }
    }

    #[test]
    fn a_browser_palette_omits_brief_highlighted_and_names_itself_browser() {
        let browser = Target::View(
            Machine::Local,
            ViewRow {
                worktree: fix(),
                kind: ViewKind::Browser(ViewId::of(30).unwrap()),
                cwd: Cwd::At(Folder {
                    dir: directory("/home/you/fix"),
                    branch: None,
                    repository: None,
                    pull_request: None,
                }),
                arranged: Some(projects::Arranged::Tabs),
                flow: None,
            },
        );
        let aimed = Palette {
            aim: Aim::At {
                target: Rc::new(browser),
                worktree: Some(Rc::new(fix_worktree())),
            },
            origin: Origin::Views,
        };
        let mut picker = palette_on(works(), aimed);
        assert_eq!(picker.aimed_at(), "lune › fix · browser");
        let actions = every_action(&mut picker);
        assert!(
            !actions
                .iter()
                .any(|action| action == "Brief the highlighted"),
            "BriefHighlighted must not leak onto a browser view via its worktree: {actions:?}"
        );
        for expected in [
            "Brief this session",
            "Close the view",
            "Close the worktree",
            "Rename the worktree…",
        ] {
            assert!(
                actions.iter().any(|action| action == expected),
                "{expected}: {actions:?}"
            );
        }
        assert!(
            !actions.iter().any(|action| action == "Rename the view…"),
            "{actions:?}"
        );
    }
}
