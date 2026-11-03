//! The picker: the sessions that run, and those zellij keeps to bring back,
//! the agents in their panes, and the directories you go to, as zoxide
//! ranks them, of every machine, each machine's under a rule of its own, in a
//! box floating over zellij's side. `Ctrl j` opens it on the sessions,
//! `Ctrl b` on the agents and `Ctrl d` on the directories, `Tab` goes from
//! one list to the next, and what you type narrows all three. `Enter` takes
//! zellij to the session highlighted, on its machine, bringing back one that
//! ended, or to the pane of the agent highlighted, or makes the session you
//! typed the name of, or one in the directory highlighted, named for it, on
//! its machine. `Del` deletes the session highlighted, or lets go of one
//! that ended, or closes the agent's pane, on its machine, asking first as
//! the menu does while an agent at work would end; `Ctrl r` renames the
//! session highlighted, on its machine. Beside the list, the preview
//! shows the pane highlighted, or the one a client coming to the session
//! highlighted is on, as zellij dumps its screen on its machine, or what is
//! in the directory highlighted, as eza lists it on its machine.

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

use lunetad::zoxide::{self, Dir};
use lunetad::{eza, screens, Destination, Directory};

use crate::away::{self, Away};
use crate::close::{Act, Deletes, Plan};
use crate::gruvbox;
use crate::host::Event;
use crate::hosted;
use crate::machines::Machine;
use crate::menu::{self, Hint};
use crate::sidebar::{
    self, ACCENT, ERROR, Finder, Fit, Keep, LIT, Listed, Mark, NAME, PanePlace, Place,
};
use crate::spots::{Drawn, Spots};
use crate::zellij::{self, Closes, Exited, SessionName};

/// What the rule over the box names it.
const TITLE: &str = "luneta";

/// How long the highlight stays on a row before the preview asks zellij for
/// its screen, so that going down the rows asks for none of those passed.
const PREVIEW_DELAY: Duration = Duration::from_millis(200);

/// The most screens the previews keep, and the most directories' contents.
/// Once they have that many, they forget them all, and ask again for those
/// shown next.
const PEEKS_MAX: usize = 32;

/// The most lines a preview keeps: the last of a screen, and the first of
/// what is in a directory.
const PEEK_LINES_MAX: usize = 200;

/// The most columns of a screen a preview keeps, the first of them.
const PEEK_COLUMNS_MAX: usize = 1000;

/// The rows the search takes, under the list: its box.
const INPUT_HEIGHT: u16 = 3;

/// The fewest rows the list takes, its box's two included, where it shows.
const MIN_RESULTS: u16 = 3;

/// The fewest columns the list and the preview each take, where both show.
const MIN_HALF: u16 = 26;

/// The fewest columns that the action `Enter` takes, after the search,
/// takes to show.
const MIN_ACTION: usize = 12;

/// What the picker lists of each machine, as the sidebar knows it.
#[derive(Debug, Default)]
pub struct Listing {
    /// Where luneta's zellij is, as far as the sidebar knows.
    pub here: Option<Here>,
    /// What runs on this machine.
    pub local: Runs,
    /// What runs on each other machine lunetad reaches, in the sidebar's
    /// order.
    pub remotes: Vec<(Destination, Runs)>,
    /// Why the daemon does not answer, while it does not.
    pub away: Option<String>,
    /// What this machine goes by, as the rule over its rows says.
    pub this: String,
}

/// Where luneta's zellij is, on `on`, the machine its client runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Here {
    /// In the session of this name there, or on its way to it, on a pane the
    /// sidebar does not know.
    Session { on: Machine, session: String },
    /// On this pane there, of the one session of its name.
    Pane { on: Machine, place: PanePlace },
}

impl Here {
    pub fn on(&self) -> &Machine {
        match self {
            Here::Session { on, .. } | Here::Pane { on, .. } => on,
        }
    }

    /// The session it is in, or on its way to.
    pub fn session(&self) -> &str {
        match self {
            Here::Session { session, .. } => session,
            Here::Pane { place, .. } => place.session.name().as_str(),
        }
    }

    /// The pane it is on, where the sidebar knows it.
    pub fn pane(&self) -> Option<&PanePlace> {
        match self {
            Here::Session { .. } => None,
            Here::Pane { place, .. } => Some(place),
        }
    }
}

/// The sessions that run on a machine, and the agents in them.
#[derive(Debug, Default)]
pub struct Runs {
    /// Newest first.
    pub sessions: Vec<Live>,
    /// Those of the session luneta's zellij is in first, where it is in one
    /// of these, then those of the others newest first, each session's in
    /// the order their panes opened.
    pub agents: Vec<Seated>,
}

/// A session that runs.
#[derive(Clone, Debug)]
pub struct Live {
    pub place: Place,
    pub started: SystemTime,
    /// Its panes, in the order of their tabs.
    pub panes: Vec<Shown>,
    /// The pane a client of it is on, which a client coming there is on
    /// too, if a client is in it.
    pub focus: Option<u32>,
    /// How many of its agents wait for you.
    pub waiting: usize,
}

/// A pane of a session, as its preview says what it shows.
#[derive(Clone, Debug)]
pub struct Shown {
    pub id: u32,
    pub tab: String,
    /// What the sidebar names its row.
    pub label: String,
}

/// An agent, in its pane.
#[derive(Clone, Debug)]
pub struct Seated {
    pub place: PanePlace,
    /// What the sidebar names its row.
    pub label: String,
    pub mark: Mark,
    /// What it is doing, in the word Claude Code says it in.
    pub status: String,
    /// Since when it has been doing it.
    pub since: Option<SystemTime>,
    /// Where it works, as its row and its preview show it, and a search finds
    /// it: its directory's last two parts, as a whole path has the letters for
    /// a few typed ones to land on nearly any of it.
    pub dir: String,
}

/// A list of the picker's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Sessions,
    Agents,
    Dirs,
}

impl List {
    /// The list after, which `Tab` goes to, from the last to the first.
    fn next(self) -> List {
        match self {
            List::Sessions => List::Agents,
            List::Agents => List::Dirs,
            List::Dirs => List::Sessions,
        }
    }

    /// The list before, which `Shift Tab` goes to, from the first to the last.
    fn back(self) -> List {
        match self {
            List::Sessions => List::Dirs,
            List::Agents => List::Sessions,
            List::Dirs => List::Agents,
        }
    }

    fn title(self) -> &'static str {
        match self {
            List::Sessions => "Sessions",
            List::Agents => "Agents",
            List::Dirs => "Directories",
        }
    }
}

/// What the picker asks of luneta.
#[derive(Debug, PartialEq, Eq)]
pub enum Picked {
    Nothing,
    /// Close the picker.
    Close,
    /// Take zellij's client to `place`, on `on`, and close the picker.
    Go { on: Machine, place: Place },
    /// Take zellij's client to the session of this name, which zellij makes
    /// on this machine, of no session it has listed there, nor one that
    /// ended, and close the picker; or say why zellij would make none.
    Start(SessionName),
    /// Take zellij's client to `session` on `on`, which ended, as zellij
    /// brings it back there, and close the picker.
    BringBack { on: Machine, session: SessionName },
    /// Take zellij's client to a session zellij makes on `on`, `session`,
    /// that starts in `cwd`, and close the picker; or say why zellij would
    /// make none.
    Make { on: Machine, session: SessionName, cwd: Directory },
    /// Plan deleting what `deletes` deletes of `session`, on `on`, and hand
    /// the plan to `planned`.
    Plan { on: Machine, session: Listed, deletes: Deletes },
    /// Have zellij on `on` do what a delete planned.
    Delete { on: Machine, act: Act },
}

/// What zellij answered what the picker asked of it, on threads of their
/// own.
pub enum Heard {
    /// The sessions zellij on `on` keeps to bring back, as it listed them
    /// for the picker's ask numbered `ask`, or why it did not list them.
    Exited { on: Machine, ask: u64, exited: Result<Vec<Exited>, String> },
    /// The screen of the terminal `asked` of `session` on `on`, or of the
    /// one a client coming there would be on, where it asked of none: the
    /// terminal, and what it shows.
    Peek { on: Machine, session: Listed, asked: Option<u32>, peek: Result<(u32, String), String> },
    /// What closing the pane at `place` on `on` closes, for its delete.
    Closes { on: Machine, place: PanePlace, closes: Result<Closes, String> },
    /// Whether the session `from` is called `to` now, on its machine, which
    /// what went wrong there names.
    Renamed { from: SessionName, to: SessionName, renamed: Result<(), String> },
    /// Whether zellij on `on` let go of the session `session`, one that
    /// ended.
    Deleted { on: Machine, session: SessionName, deleted: Result<(), String> },
    /// The directories zoxide knows on `on`, best first, as it listed them
    /// for the picker's ask numbered `ask`, or why it did not list them.
    Dirs { on: Machine, ask: u64, visited: Result<Vec<Dir>, String> },
    /// What eza listed of the directory `path` of `on`, or why it did not.
    Listed { on: Machine, path: Directory, listed: Result<String, String> },
}

/// What the picker asks of zellij, and of zoxide and eza, of this machine
/// or, through lunetad, of another, which luneta asks on a thread of its own
/// with `ask`, and hands back what they answer as `Heard`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// The sessions zellij on `on` keeps to bring back, for the picker's ask
    /// of them numbered `ask`: its asks of them count up from 1, of every
    /// machine.
    Exited { on: Machine, ask: u64 },
    /// The screen the preview shows: of the terminal `asked` of `session` on
    /// `on`, or else of the one a client coming there would be on.
    Peek { on: Machine, session: Listed, asked: Option<u32> },
    /// What closing the pane at `place` on `on` closes.
    Closes { on: Machine, place: PanePlace },
    /// To call the session `from` on `on` `to`.
    Rename { on: Machine, from: SessionName, to: SessionName },
    /// To let go of the session `session` on `on`, one that ended.
    Delete { on: Machine, session: SessionName },
    /// The directories zoxide knows on `on`, for the picker's ask of them
    /// numbered `ask`: its asks of them count up from 1, of every machine.
    Dirs { on: Machine, ask: u64 },
    /// What is in the directory at `path` of `on`, as eza lists it.
    List { on: Machine, path: Directory },
}

/// Asks zellij, zoxide or eza what `ask` asks, for the picker numbered
/// `number`, on a thread of its own, and hands back what it answers.
pub fn ask(number: u64, ask: Ask, events: &Sender<Event>) {
    let events = events.clone();
    thread::spawn(move || {
        let heard = match ask {
            Ask::Exited { on, ask } => {
                let exited = match &on {
                    Machine::Local => zellij::exited(),
                    // The notes over the list say why, of each machine.
                    Machine::Remote(destination) => reach(destination)
                        .and_then(|mut away| away.exited()),
                };
                Heard::Exited { on, ask, exited }
            },
            Ask::Peek { on, session, asked } => {
                let name = session.name();
                let peek = match &on {
                    Machine::Local => screens::dumped(name, asked),
                    Machine::Remote(destination) => Away::reach(destination, away::ANSWER)
                        .and_then(|mut away| away.screen(name, asked)),
                };
                Heard::Peek { on, session, asked, peek }
            },
            Ask::Closes { on, place } => {
                let (session, pane) = (place.session.name(), place.pane);
                let closes = match &on {
                    Machine::Local => zellij::closes_now(session, pane),
                    Machine::Remote(destination) => Away::reach(destination, away::ANSWER)
                        .and_then(|mut away| away.closes(session, pane)),
                };
                Heard::Closes { on, place, closes }
            },
            Ask::Rename { on, from, to } => {
                let renamed = match &on {
                    Machine::Local => zellij::renamed(&from, &to),
                    Machine::Remote(destination) => {
                        reach(destination).and_then(|mut away| away.rename(&from, &to))
                    },
                };
                Heard::Renamed { from, to, renamed }
            },
            Ask::Delete { on, session } => {
                let deleted = match &on {
                    Machine::Local => zellij::deleted(&session),
                    Machine::Remote(destination) => {
                        reach(destination).and_then(|mut away| away.delete(&session))
                    },
                };
                Heard::Deleted { on, session, deleted }
            },
            Ask::Dirs { on, ask } => {
                let visited = match &on {
                    Machine::Local => zoxide::visited(),
                    // The notes over the list say why, of each machine.
                    Machine::Remote(destination) => {
                        reach(destination).and_then(|mut away| away.dirs())
                    },
                };
                Heard::Dirs { on, ask, visited }
            },
            Ask::List { on, path } => {
                let listed = match &on {
                    Machine::Local => eza::listed(&path),
                    Machine::Remote(destination) => Away::reach(destination, away::ANSWER)
                        .and_then(|mut away| away.contents(&path)),
                };
                Heard::Listed { on, path, listed }
            },
        };
        // luneta is on its way out.
        let _ = events.send(Event::Picker { number, heard });
    });
}

/// The daemon of `destination`, as `Away::reach` reaches it, or why it does
/// not, after the machine's name, as what that daemon says has it: what went
/// wrong names its machine, as a note over the list does, of each.
fn reach(destination: &Destination) -> Result<Away, String> {
    Away::reach(destination, away::ANSWER).map_err(|why| format!("{destination}: {why}"))
}

/// What a click on a spot of the picker does: presses the key its help line
/// names, or picks the row that stands for this, as `Enter` does with the
/// highlight on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    Key(KeyEvent),
    Row(Key),
}

/// What a row of a list stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// A session that runs on `on`, the one luneta's zellij is in among
    /// them.
    Live { on: Machine, place: Place },
    /// A session of `on`'s that ended, which zellij keeps to bring back.
    Exited { on: Machine, session: SessionName },
    /// The pane of an agent on `on`.
    Agent { on: Machine, place: PanePlace },
    /// A directory zoxide knows on `on`, by its path, and the session `Enter`
    /// makes there, or why it makes none yet.
    Dir { on: Machine, path: Directory, session: Result<SessionName, &'static str> },
}

impl Key {
    /// The machine of what it stands for.
    fn machine(&self) -> &Machine {
        match self {
            Key::Live { on, .. }
            | Key::Exited { on, .. }
            | Key::Agent { on, .. }
            | Key::Dir { on, .. } => on,
        }
    }
}

/// A row of a list: what it stands for, the name it shows, the characters
/// a search found: of that name, or of a directory's path, which its row
/// shows too; and the rules over it.
#[derive(Clone, Debug)]
struct Row {
    key: Key,
    name: String,
    hits: Vec<usize>,
    rules: Rules,
}

/// The rules over a row, each a line of its own, in this order: its
/// machine's, by the name it goes by, over the first row of each machine's
/// where the list has more than one machine's; and the ended sessions',
/// over the first of them.
#[derive(Clone, Debug, Default)]
struct Rules {
    machine: Option<String>,
    ended: bool,
}

/// A line of a list: a rule, or the row at its place among the rows.
enum Item<'a> {
    Machine(&'a str),
    Ended,
    Row(usize),
}

/// The rows of a list, and the one highlighted: none while there are none.
#[derive(Debug, Default)]
struct Rows {
    rows: Vec<Row>,
    at: Option<usize>,
}

impl Rows {
    /// `rows` in the place of these, the highlight on the row of `held`
    /// where one is; or else, where that row went, on the row that took its
    /// place, as the one after a row deleted; or else on the row `start`. No
    /// further than the last.
    fn replace(&mut self, rows: Vec<Row>, held: Option<&Key>, start: usize) {
        let at = held.and_then(|held| rows.iter().position(|row| row.key == *held));
        let was = self.at.filter(|_| held.is_some()).unwrap_or(start);
        let last = rows.len().checked_sub(1);
        self.at = at.or_else(|| last.map(|last| was.min(last)));
        self.rows = rows;
    }

    fn selected(&self) -> Option<&Row> {
        self.rows.get(self.at?)
    }

    /// Moves the highlight `by` rows down, or up, and no further than the
    /// last row or the first.
    fn step(&mut self, by: isize) {
        let Some(at) = self.at else {
            return;
        };
        let last = self.rows.len().saturating_sub(1);
        let to = if by < 0 { at.saturating_sub(by.unsigned_abs()) } else { at + by.unsigned_abs() };
        self.at = Some(to.min(last));
    }
}

/// Where the picker is at.
#[derive(Debug)]
enum Stage {
    /// Narrowing the lists by what is typed.
    Search,
    /// Naming the session called `session` on `on` again, `input` typed so
    /// far.
    Rename { on: Machine, session: SessionName, input: String },
    /// Waiting on zellij, or on the plan, to delete what `target` stands
    /// for: going ahead with the plan once it is `agreed`, what you said yes
    /// to, or else asking first, if it asks.
    Planning { target: Key, agreed: Option<Plan> },
    /// Asking whether to go ahead with `plan`, as `asks`.
    Asking { target: Key, plan: Plan, asks: String },
}

/// Lines as a terminal shows them, in the colours they say, on a screen of
/// their own, and the lines of it that show anything: from `first` to before
/// `end`.
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
        // A line's bytes are at least as many as the columns it takes.
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

    /// Whether no line of it shows anything.
    fn is_blank(&self) -> bool {
        self.first == self.end
    }

    /// Draws its lines from `top` on into `area`, as many as fit.
    fn show(&self, buffer: &mut Buffer, area: Rect, top: usize) {
        hosted::show(&self.screen, buffer, area, top);
    }
}

/// What a preview shows of a terminal's screen.
#[derive(Debug)]
enum Peek {
    /// zellij is asked for it.
    Reading,
    /// The terminal `pane`'s screen, the last of its lines.
    Ready { pane: u32, shot: Shot },
    /// Why zellij did not dump it.
    Failed(String),
}

impl Peek {
    /// The screen zellij dumped of `pane`, `dumped`.
    fn of(pane: u32, dumped: &str) -> Peek {
        let lines: Vec<&str> = dumped.lines().collect();
        let lines = &lines[lines.len().saturating_sub(PEEK_LINES_MAX)..];
        Peek::Ready { pane, shot: Shot::of(lines) }
    }
}

/// What a directory's preview shows of what is in it.
#[derive(Debug)]
enum Contents {
    /// eza is asked for it.
    Reading,
    /// Its `total` entries, the first of them on `shot`.
    Ready { total: usize, shot: Shot },
    /// Why eza did not list it.
    Failed(String),
}

impl Contents {
    /// What eza `listed` of a directory, an entry a line.
    fn of(listed: &str) -> Contents {
        let entries: Vec<&str> = listed.lines().filter(|line| !line.trim().is_empty()).collect();
        let shot = Shot::of(&entries[..entries.len().min(PEEK_LINES_MAX)]);
        Contents::Ready { total: entries.len(), shot }
    }
}

/// What a preview shows: the terminal `pane` of `session` on the machine,
/// or, of none, the one a client coming there would be on. A session made
/// again by the name of one that ended has a screen of its own.
type Looked = (Machine, Listed, Option<u32>);

/// A directory of a machine's, by its path.
type Located = (Machine, Directory);

/// What the preview shows: a terminal's screen, or what is in a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Preview {
    Screen(Looked),
    Dir(Located),
}

/// What zoxide said of the directories of a machine's.
#[derive(Debug)]
enum Visited {
    /// Nothing yet, of the ask numbered so: only its answer is taken.
    Asking(u64),
    /// Best first.
    Listed(Vec<Dir>),
    /// Why it did not list them.
    Failed(String),
}

/// What zellij on a machine said of the sessions it keeps to bring back.
/// Only the answer to the last ask of them says how they are: one to an ask
/// before it can come after it, and leave out a session that ended in
/// between.
#[derive(Debug)]
enum Ended {
    /// Nothing yet, of the ask numbered so.
    Asking(u64),
    /// As it listed them at `at`, which their ages count back from, and none
    /// has ended, nor been let go of, since.
    Heard { exited: Vec<Exited>, at: SystemTime },
    /// As it listed them at `at`, before a session ended, or one was let go
    /// of: they show on, but free no name, as zellij keeps one that ended
    /// since, and could keep one let go of yet. Asked for again as `next`
    /// says.
    Stale { exited: Vec<Exited>, at: SystemTime, next: Next },
    /// Why it did not list them.
    Failed(String),
}

/// What a list of the sessions zellij keeps waits on, once it went stale.
#[derive(Debug)]
enum Next {
    /// The answer to the ask numbered so.
    Answer(u64),
    /// zellij letting go of these sessions, which it has yet to say it did:
    /// what it lists till then can list them yet. It is asked again once it
    /// said so of each, and that ask lists those that ended meanwhile too.
    LettingGo(HashSet<SessionName>),
}

impl Ended {
    /// What it is once asked for again, by the ask numbered `ask`: what
    /// zellij listed goes stale, as a session can have ended since, or been
    /// let go of. While zellij is letting go of one, the ask waits, as
    /// `awaits` says.
    fn asked(self, ask: u64) -> Ended {
        match self {
            Ended::Stale { exited, at, next: Next::LettingGo(going) } if !going.is_empty() => {
                Ended::Stale { exited, at, next: Next::LettingGo(going) }
            },
            Ended::Heard { exited, at } | Ended::Stale { exited, at, .. } => {
                Ended::Stale { exited, at, next: Next::Answer(ask) }
            },
            Ended::Asking(_) | Ended::Failed(_) => Ended::Asking(ask),
        }
    }

    /// What it is once zellij is asked to let go of `session`: off the list,
    /// which goes stale, whatever zellij lists before it said it let go of
    /// it.
    fn forgot(self, session: &SessionName) -> Ended {
        let (mut exited, at, mut going) = match self {
            Ended::Heard { exited, at } | Ended::Stale { exited, at, next: Next::Answer(_) } => {
                (exited, at, HashSet::new())
            },
            Ended::Stale { exited, at, next: Next::LettingGo(going) } => (exited, at, going),
            ended @ (Ended::Asking(_) | Ended::Failed(_)) => return ended,
        };
        exited.retain(|exited| exited.name != *session);
        going.insert(session.clone());
        Ended::Stale { exited, at, next: Next::LettingGo(going) }
    }

    /// What it is once zellij said whether it let go of `session`: one to
    /// ask for again, as `asked` says, once it said so of each.
    fn let_go(mut self, session: &SessionName) -> Ended {
        if let Ended::Stale { next: Next::LettingGo(going), .. } = &mut self {
            going.remove(session);
        }
        self
    }

    /// Whether the ask numbered `ask` is the one open, whose answer is
    /// taken.
    fn awaits(&self, ask: u64) -> bool {
        match self {
            Ended::Asking(open) | Ended::Stale { next: Next::Answer(open), .. } => *open == ask,
            Ended::Stale { next: Next::LettingGo(_), .. }
            | Ended::Heard { .. }
            | Ended::Failed(_) => false,
        }
    }

    /// The sessions zellij keeps, as far as it said, and when it listed
    /// them.
    fn listed(&self) -> Option<(&[Exited], SystemTime)> {
        match self {
            Ended::Heard { exited, at } | Ended::Stale { exited, at, .. } => Some((exited, *at)),
            Ended::Asking(_) | Ended::Failed(_) => None,
        }
    }
}

/// The picker.
#[derive(Debug)]
pub struct Picker {
    /// Its number, which what zellij answers it carries.
    number: u64,
    list: List,
    /// What is typed in the search.
    term: String,
    stage: Stage,
    listing: Listing,
    /// What zellij said of the sessions it keeps to bring back, on each
    /// machine it was asked of, which is each the picker lists.
    exited: HashMap<Machine, Ended>,
    /// The number of the last ask for a machine's sessions that ended.
    exited_asked: u64,
    /// What zoxide said of the directories of each machine it was asked of,
    /// which is each the picker lists.
    visited: HashMap<Machine, Visited>,
    /// The number of the last ask for a machine's directories.
    dirs_asked: u64,
    sessions: Rows,
    agents: Rows,
    dirs: Rows,
    peeks: HashMap<Looked, Peek>,
    contents: HashMap<Located, Contents>,
    /// What the preview shows now, and since when.
    wanted: Option<(Preview, Instant)>,
    /// What went wrong last, until the next key.
    error: Option<String>,
    /// What the picker has yet to ask of zellij, which `asks` hands over.
    asking: Vec<Ask>,
    /// The machine whose directories the highlight goes to: the one whose
    /// header `Ctrl d` was pressed on, or this one. To the first of them as
    /// they come, while `aiming`, and to the best of them that the search
    /// finds as what is typed changes.
    aim: Machine,
    /// Whether the highlight goes to the first of `aim`'s directories as
    /// they come: until a key moves it.
    aiming: bool,
}

impl Picker {
    /// The picker numbered `number`, open on `list`, of `listing`, its
    /// highlight on the directories of `aim` as they come, which asks zellij
    /// of each machine for the sessions it keeps to bring back, and zoxide
    /// for the directories it knows.
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
            sessions: Rows::default(),
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

    /// What the picker has yet to ask of zellij, for luneta to ask it.
    pub fn asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asking)
    }

    /// Takes in what the sidebar knows of each machine now. zellij of a
    /// machine is asked again for the sessions it keeps to bring back once a
    /// session of it ran that runs no more, as zellij keeps one killed; and
    /// zellij and zoxide of a machine listed for the first time, or for the
    /// first time since it went off the listing, for those sessions and the
    /// directories it knows.
    pub fn listed(&mut self, listing: Listing) {
        let names = |runs: &Runs| -> HashSet<String> {
            runs.sessions.iter().map(|live| live.place.session().to_owned()).collect()
        };
        let before: HashMap<Machine, HashSet<String>> =
            self.each().map(|(on, runs)| (on, names(runs))).collect();
        self.listing = listing;
        // What a machine that went off the listing said is of a reach that
        // ended, and so is why it did not answer, and what an ask of it that
        // is still open will say.
        let machines = self.machines();
        self.visited.retain(|on, _| machines.contains(on));
        self.exited.retain(|on, _| machines.contains(on));
        let ended: Vec<Machine> = self
            .each()
            .filter(|(on, runs)| before.get(on).is_some_and(|ran| !ran.is_subset(&names(runs))))
            .map(|(on, _)| on)
            .filter(|on| self.exited.contains_key(on))
            .collect();
        for on in ended {
            self.ask_exited(on);
        }
        self.ask_kept();
        self.ask_dirs();
        self.rebuild(false);
    }

    /// The machines listed: this one, then the others lunetad reaches.
    fn machines(&self) -> Vec<Machine> {
        self.each().map(|(on, _)| on).collect()
    }

    /// What runs on each machine listed, this one's first.
    fn each(&self) -> impl Iterator<Item = (Machine, &Runs)> {
        let others = self.listing.remotes.iter();
        let others = others.map(|(destination, runs)| (Machine::Remote(destination.clone()), runs));
        std::iter::once((Machine::Local, &self.listing.local)).chain(others)
    }

    /// What runs on `on`, while it is listed.
    fn runs(&self, on: &Machine) -> Option<&Runs> {
        match on {
            Machine::Local => Some(&self.listing.local),
            Machine::Remote(destination) => {
                let mut remotes = self.listing.remotes.iter();
                remotes.find(|(at, _)| at == destination).map(|(_, runs)| runs)
            },
        }
    }

    /// The session at `place` on `on`, while it runs.
    fn live(&self, on: &Machine, place: &Place) -> Option<&Live> {
        self.runs(on)?.sessions.iter().find(|live| live.place == *place)
    }

    /// The agent at `place` on `on`, while it is in its pane.
    fn seated(&self, on: &Machine, place: &PanePlace) -> Option<&Seated> {
        self.runs(on)?.agents.iter().find(|seated| seated.place == *place)
    }

    /// Asks zoxide of each machine listed that it was not asked of yet for
    /// the directories it knows.
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

    /// Asks zellij of each machine listed that it was not asked of yet for
    /// the sessions it keeps to bring back.
    fn ask_kept(&mut self) {
        for on in self.machines() {
            if !self.exited.contains_key(&on) {
                self.ask_exited(on);
            }
        }
    }

    /// Asks zellij on `on` for the sessions it keeps to bring back, again,
    /// where it was asked before, as `Ended::asked` has it: not while it is
    /// letting go of one, after which it is asked.
    fn ask_exited(&mut self, on: Machine) {
        let ask = self.exited_asked + 1;
        let ended = self.exited.remove(&on).map_or(Ended::Asking(ask), |ended| ended.asked(ask));
        if ended.awaits(ask) {
            self.exited_asked = ask;
            self.asking.push(Ask::Exited { on: on.clone(), ask });
        }
        self.exited.insert(on, ended);
    }

    /// Takes in what zellij answered, and says what that leaves luneta to do.
    pub fn heard(&mut self, heard: Heard) -> Picked {
        match heard {
            Heard::Exited { on, ask, exited } => {
                // What zellij answered an ask before the last can leave out
                // a session that ended since, which the last asks after; and
                // what it answered of a machine that went off the listing
                // since is of a reach that ended.
                if !self.exited.get(&on).is_some_and(|ended| ended.awaits(ask)) {
                    return Picked::Nothing;
                }
                let ended = match exited {
                    Ok(exited) => Ended::Heard { exited, at: SystemTime::now() },
                    Err(why) => Ended::Failed(why),
                };
                self.exited.insert(on, ended);
                self.rebuild(false);
            },
            Heard::Peek { on, session, asked, peek } => {
                let peek = match peek {
                    Ok((pane, dumped)) => Peek::of(pane, &dumped),
                    Err(why) => Peek::Failed(why),
                };
                self.peeks.insert((on, session, asked), peek);
            },
            Heard::Closes { on, place, closes } => {
                let planning = matches!(
                    &self.stage,
                    Stage::Planning { target: Key::Agent { on: planned, place: at }, .. }
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
                    self.error = Some(format!("{from} is not called {to}: {why}"));
                }
            },
            Heard::Deleted { on, session, deleted } => {
                if let Err(why) = deleted {
                    self.error = Some(format!("{session} stays: {why}"));
                }
                if let Some(ended) = self.exited.remove(&on) {
                    self.exited.insert(on.clone(), ended.let_go(&session));
                    self.ask_exited(on);
                }
            },
            Heard::Dirs { on, ask, visited } => {
                // What zoxide answered an ask made before its machine went off
                // the listing is of a reach that ended.
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

    /// What `key` does, and what it leaves luneta to do.
    pub fn key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
        if control && key.code == KeyCode::Char('c') {
            return Picked::Close;
        }
        self.error = None;
        self.aiming = false;
        let picked = match std::mem::replace(&mut self.stage, Stage::Search) {
            Stage::Search => self.search_key(key),
            Stage::Rename { on, session, input } => self.rename_key(key, on, session, input),
            // Only `Esc` does anything while zellij is asked, or the plan:
            // it gives up the delete.
            Stage::Planning { target, agreed } => {
                if key.code != KeyCode::Esc {
                    self.stage = Stage::Planning { target, agreed };
                }
                Picked::Nothing
            },
            // `y` goes ahead, once the plan is the same again, and any other
            // key does not.
            Stage::Asking { target, plan, .. } => {
                let yes = key.code == KeyCode::Char('y') && plain(key);
                if yes { self.plan(target, Some(plan)) } else { Picked::Nothing }
            },
        };
        self.follow();
        picked
    }

    /// Types `text` where you type: in the search, or the new name.
    pub fn paste(&mut self, text: &str) {
        self.aiming = false;
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        match &mut self.stage {
            Stage::Search => {
                self.term.push_str(&text);
                self.rebuild(true);
            },
            Stage::Rename { input, .. } => input.push_str(&text),
            Stage::Planning { .. } | Stage::Asking { .. } => {},
        }
    }

    /// Moves the highlight `by` rows, as the wheel does.
    pub fn scroll(&mut self, by: isize) {
        self.aiming = false;
        if matches!(self.stage, Stage::Search) {
            self.rows_mut().step(by);
            self.follow();
        }
    }

    /// What a click on `pick` does: what the key it names does, or what
    /// `Enter` does with the highlight on its row. A row picks nothing once
    /// the list shows it no more, nor while it picks none, as `picks` says.
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

    /// What the delete waited on does, as `plan` says: asks first, or goes
    /// ahead, or says why it does nothing. Going ahead after you said yes
    /// does what you were asked about; once that changed while you read, it
    /// asks again.
    pub fn planned(&mut self, plan: Result<Plan, String>) -> Picked {
        let Stage::Planning { target, agreed } = std::mem::replace(&mut self.stage, Stage::Search)
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
            self.stage = Stage::Asking { target, plan, asks };
            return Picked::Nothing;
        }
        Picked::Delete { on: target.machine().clone(), act: plan.act }
    }

    /// When the preview asks zellij for the screen it shows, or eza for what
    /// is in the directory, if it is to ask.
    pub fn due(&self) -> Option<Instant> {
        let (preview, since) = self.wanted.as_ref()?;
        let asked = match preview {
            Preview::Screen(looked) => self.peeks.contains_key(looked),
            Preview::Dir(located) => self.contents.contains_key(located),
        };
        (!asked).then(|| *since + PREVIEW_DELAY)
    }

    /// Asks for what the preview shows, once it is `due`.
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

    /// The box over zellij's side, `over`, the picker is drawn in: the
    /// middle three fifths of it each way.
    pub fn area(over: Rect) -> Rect {
        Rect::new(
            over.x + over.width / 5,
            over.y + over.height / 5,
            over.width * 3 / 5,
            over.height * 3 / 5,
        )
    }

    /// Draws the picker over `over`, zellij's side, at `now`, and says where,
    /// and the spots on its rows and its keys.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) -> Drawn {
        let area = Picker::area(over);
        let (Some(width), Some(height)) = (area.width.checked_sub(2), area.height.checked_sub(2))
        else {
            return Drawn::Picker { number: self.number, area, spots: Spots::default() };
        };
        framed(buffer, area, TITLE, "", Vec::new());
        let layout = Layout::of(Rect::new(area.x + 1, area.y + 1, width, height));
        if let Stage::Rename { on, session, input } = &self.stage {
            let spots = self.draw_rename(buffer, &layout, on, session, input);
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
            // The lines go between the box's sides, under its rule.
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

    /// Whether a click on a row picks it, as `Enter` would: while the lists
    /// are searched, and not as a delete waits or asks, nor once you rename a
    /// session. A row is a spot only then.
    fn picks(&self) -> bool {
        matches!(self.stage, Stage::Search)
    }

    /// The rows of the list shown.
    fn rows(&self) -> &Rows {
        match self.list {
            List::Sessions => &self.sessions,
            List::Agents => &self.agents,
            List::Dirs => &self.dirs,
        }
    }

    fn rows_mut(&mut self) -> &mut Rows {
        match self.list {
            List::Sessions => &mut self.sessions,
            List::Agents => &mut self.agents,
            List::Dirs => &mut self.dirs,
        }
    }

    /// What a key does to the search, and what it leaves luneta to do.
    fn search_key(&mut self, key: &KeyEvent) -> Picked {
        let control = key.modifiers == KeyModifiers::CONTROL;
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
            KeyCode::Char('j') if control => {
                self.list = List::Sessions;
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

    /// What `Enter` does: takes zellij to the row highlighted, or makes the
    /// session typed, where no session is highlighted, or one in the
    /// directory highlighted, as its row names it. Where no agent or
    /// directory is, it does nothing: what is typed there names none.
    fn enter(&mut self) -> Picked {
        let Some(row) = self.rows().selected() else {
            return match self.new_name() {
                Ok(Some(name)) if self.list == List::Sessions => Picked::Start(name),
                Ok(_) | Err(_) => Picked::Nothing,
            };
        };
        match &row.key {
            Key::Live { on, place } if self.is_here(on, place) => Picked::Close,
            Key::Live { on, place } => Picked::Go { on: on.clone(), place: place.clone() },
            Key::Agent { on, place } => {
                Picked::Go { on: on.clone(), place: Place::Pane(place.clone()) }
            },
            Key::Exited { on, session } => {
                Picked::BringBack { on: on.clone(), session: session.clone() }
            },
            Key::Dir { on, path, session: Ok(session) } => {
                Picked::Make { on: on.clone(), session: session.clone(), cwd: path.clone() }
            },
            Key::Dir { session: Err(_), .. } => Picked::Nothing,
        }
    }

    /// The session typed, which `Enter` makes on this machine: none of
    /// nothing typed, and none of the name of the session luneta's zellij is
    /// in here. None until zellij here said which sessions ended, either: one
    /// of those is a row of its own once it did, which `Enter` brings back.
    fn new_name(&self) -> Result<Option<SessionName>, &'static str> {
        if self.term.is_empty() {
            return Ok(None);
        }
        if self.here_session(&Machine::Local) == Some(self.term.as_str()) {
            return Err("you are in it");
        }
        let name = SessionName::parse(&self.term)?;
        self.taken(&Machine::Local)?;
        Ok(Some(name))
    }

    /// What `Del` does: plans deleting the session highlighted, or closing the
    /// pane of the agent highlighted, or lets go of the session highlighted
    /// that ended, on its machine. What is typed stays, and the highlight goes
    /// on to the row after once the row goes: `Del` again deletes the row it
    /// shows, not one off the top of the list.
    fn delete(&mut self) -> Picked {
        let Some(key) = self.rows().selected().map(|row| row.key.clone()) else {
            return Picked::Nothing;
        };
        match key {
            Key::Exited { on, session } => {
                if let Some(ended) = self.exited.remove(&on) {
                    self.exited.insert(on.clone(), ended.forgot(&session));
                }
                self.asking.push(Ask::Delete { on, session });
                self.rebuild(false);
                Picked::Nothing
            },
            Key::Dir { .. } => Picked::Nothing,
            target => self.plan(target, None),
        }
    }

    /// Plans deleting what `target` stands for, going ahead once the plan is
    /// `agreed`: of a session, at once; of a pane, once zellij says what
    /// closing it closes, on its machine.
    fn plan(&mut self, target: Key, agreed: Option<Plan>) -> Picked {
        let picked = match &target {
            Key::Live { on, place } => {
                let (on, session) = (on.clone(), place.listed().clone());
                Picked::Plan { on, session, deletes: Deletes::Session }
            },
            Key::Agent { on, place } => {
                self.asking.push(Ask::Closes { on: on.clone(), place: place.clone() });
                Picked::Nothing
            },
            Key::Exited { .. } | Key::Dir { .. } => return Picked::Nothing,
        };
        self.stage = Stage::Planning { target, agreed };
        picked
    }

    /// Starts naming the session highlighted again, on its machine, which
    /// must run: zellij renames none that ended.
    fn rename(&mut self) {
        if self.list != List::Sessions {
            return;
        }
        match self.sessions.selected().map(|row| &row.key) {
            Some(Key::Live { on, place }) => {
                let (on, session) = (on.clone(), place.listed().name().clone());
                self.stage = Stage::Rename { on, session, input: String::new() };
            },
            Some(Key::Exited { session, .. }) => {
                self.error = Some(format!(
                    "{session} ended, and zellij renames only a session that runs: Enter \
                     brings it back"
                ));
            },
            Some(Key::Agent { .. } | Key::Dir { .. }) | None => {},
        }
    }

    /// What a key does to the new name of `session` on `on`, `input` typed
    /// so far.
    fn rename_key(
        &mut self,
        key: &KeyEvent,
        on: Machine,
        session: SessionName,
        mut input: String,
    ) -> Picked {
        match key.code {
            KeyCode::Esc => return Picked::Nothing,
            KeyCode::Enter => {
                let Ok(to) = self.rename_to(&on, &session, &input) else {
                    self.stage = Stage::Rename { on, session, input };
                    return Picked::Nothing;
                };
                self.asking.push(Ask::Rename { on, from: session, to });
                return Picked::Nothing;
            },
            KeyCode::Backspace => {
                input.pop();
            },
            KeyCode::Char(c) if plain(key) => input.push(c),
            _ => {},
        }
        self.stage = Stage::Rename { on, session, input };
        Picked::Nothing
    }

    /// The name `session` on `on` takes, of what was typed, or why it takes
    /// none.
    fn rename_to(
        &self,
        on: &Machine,
        session: &SessionName,
        name: &str,
    ) -> Result<SessionName, &'static str> {
        let name = SessionName::parse(name)?;
        if name == *session {
            return Err("already called that");
        }
        if self.taken(on)?.contains(&name.as_str()) {
            return Err("a session by that name already exists");
        }
        Ok(name)
    }

    /// The names of the sessions of `on` that run and of those that ended,
    /// which no other session there can take; or why they are not known
    /// yet, as zellij there has yet to list those that ended, or to list them
    /// again since. A name zellij has yet to say ended is one
    /// `switch-session` would bring back.
    fn taken(&self, on: &Machine) -> Result<Vec<&str>, &'static str> {
        let exited = match self.exited.get(on) {
            Some(Ended::Heard { exited, .. }) => exited,
            Some(Ended::Asking(_) | Ended::Stale { .. }) | None => {
                return Err("zellij has yet to say which sessions ended");
            },
            Some(Ended::Failed(_)) => return Err("zellij did not say which sessions ended"),
        };
        let sessions = self.runs(on).map_or(&[][..], |runs| &runs.sessions);
        let live = sessions.iter().map(|live| live.place.session());
        Ok(live.chain(exited.iter().map(|exited| exited.name.as_str())).collect())
    }

    /// The sessions zellij on `on` keeps to bring back, as far as it said,
    /// and when it listed them.
    fn kept(&self, on: &Machine) -> Option<(&[Exited], SystemTime)> {
        self.exited.get(on)?.listed()
    }

    /// Builds the lists again, of what is typed, each machine's rows in
    /// turn, this one's first, under its rule where more than one machine is
    /// listed: the highlight on the row it was on, or the one that took its
    /// place where that went; or else, `snap`, as what is typed changed, on
    /// the best match of the first machine's that has one, which of the
    /// directories is the best of the aim's machine's where it has one; or
    /// on the first session after yours.
    fn rebuild(&mut self, snap: bool) {
        let finder = (!self.term.is_empty()).then(|| Finder::new(&self.term));
        let ruled = !self.listing.remotes.is_empty();
        let (mut sessions, mut agents) = (Vec::new(), Vec::new());
        for (on, runs) in self.each() {
            let rule = || ruled.then(|| self.machine_name(&on));
            grouped(&mut sessions, self.session_rows(finder.as_ref(), &on, runs), rule());
            grouped(&mut agents, self.agent_rows(finder.as_ref(), &on, runs), rule());
        }
        let (dirs, aimed) = self.dir_rows();
        let start = |rows: &[Row]| {
            let first = rows.first().map(|row| &row.key);
            let yours = matches!(first, Some(Key::Live { on, place }) if self.is_here(on, place));
            usize::from(yours && self.term.is_empty())
        };
        let (session_start, agent_start) = (start(&sessions), start(&agents));
        let held = |rows: &Rows| rows.selected().map(|row| row.key.clone()).filter(|_| !snap);
        let (held_session, held_agent) = (held(&self.sessions), held(&self.agents));
        // The aim holds the highlight on its machine's directories, the
        // first of them, once they come.
        let held_dir = held(&self.dirs).filter(|_| !self.aiming);
        self.sessions.replace(sessions, held_session.as_ref(), session_start);
        self.agents.replace(agents, held_agent.as_ref(), agent_start);
        self.dirs.replace(dirs, held_dir.as_ref(), aimed.unwrap_or(0));
        self.follow();
    }

    /// The rows of the sessions that `runs` on `on`, of those `finder`
    /// finds: the one luneta's zellij is in first, then those that run, then
    /// those that ended there, under their rule, each newest first, as they
    /// are listed.
    fn session_rows(&self, finder: Option<&Finder<'_>>, on: &Machine, runs: &Runs) -> Vec<Row> {
        let mut sessions = Vec::new();
        for live in &runs.sessions {
            let rank = u8::from(!self.is_here(on, &live.place));
            let key = Key::Live { on: on.clone(), place: live.place.clone() };
            sessions.push((rank, live.place.session().to_owned(), key));
        }
        let mut exited: Vec<&Exited> = self.kept(on).map_or(&[][..], |(it, _)| it).iter().collect();
        exited.sort_by_key(|exited| exited.ago);
        for exited in exited {
            let key = Key::Exited { on: on.clone(), session: exited.name.clone() };
            sessions.push((2, exited.name.to_string(), key));
        }
        sessions.sort_by_key(|(rank, ..)| *rank);
        let sessions = sessions.into_iter().map(|(_, name, key)| (name, Vec::new(), key));
        let mut sessions = matched(finder, sessions);
        let first = sessions.iter_mut().find(|row| matches!(row.key, Key::Exited { .. }));
        if let Some(first) = first {
            first.rules.ended = true;
        }
        sessions
    }

    /// The rows of the agents that `runs` on `on`, of those `finder` finds,
    /// each by its session's name, and by its directory too; but for the one
    /// in the pane luneta's zellij is on, which the list pins over them.
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

    /// The rows of the directories zoxide knows, of what is typed: each
    /// machine's in turn, this one's first, as `dir_rows` has them, under its
    /// rule where more than one machine is listed. A row of this machine's
    /// has the session named for it free of the names zellij has here; one
    /// of another's, the name its daemon gave it, free there. And the row the
    /// first of the machine `aim` names is, which is its best match, where it
    /// has one.
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

    /// What `on` goes by.
    fn machine_name(&self, on: &Machine) -> String {
        match on {
            Machine::Local => self.listing.this.clone(),
            Machine::Remote(destination) => destination.to_string(),
        }
    }

    /// Points the preview at the row highlighted now.
    fn follow(&mut self) {
        let preview = match self.rows().selected().map(|row| &row.key) {
            Some(Key::Live { on, place }) => {
                let focus = self.live(on, place).and_then(|live| live.focus);
                Some(Preview::Screen((on.clone(), place.listed().clone(), focus)))
            },
            Some(Key::Agent { on, place }) => {
                Some(Preview::Screen((on.clone(), place.session.clone(), Some(place.pane))))
            },
            Some(Key::Dir { on, path, .. }) => Some(Preview::Dir((on.clone(), path.clone()))),
            Some(Key::Exited { .. }) | None => None,
        };
        match (preview, &self.wanted) {
            (Some(preview), Some((wanted, _))) if preview == *wanted => {},
            (Some(preview), _) => {
                // What did not come is asked for again, as the highlight
                // comes back to it, not as long as it rests there.
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

    /// What goes over the list, `width` wide: what went wrong, what the
    /// picker asks, and why it cannot tell.
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
            List::Sessions => {
                for on in self.machines() {
                    if let Some(Ended::Failed(why)) = self.exited.get(&on) {
                        let why = format!("no ended sessions: {why}");
                        notes.extend(menu::wrap(&why, width, error));
                    }
                }
            },
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

    /// The lines of the list shown, `width` wide, `room` of them at most: the
    /// highlight's row among them. Each row is a spot that picks it, by its
    /// line among them, while one does.
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
        // Each rule over a row takes a line of its own.
        let mut items = Vec::new();
        let mut lit_at = 0;
        for (at, row) in rows.rows.iter().enumerate() {
            items.extend(row.rules.machine.as_deref().map(Item::Machine));
            items.extend(row.rules.ended.then_some(Item::Ended));
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
                Item::Ended => {
                    lines.push(separator("ended", width));
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

    /// The session luneta's zellij is in, or on its way to, where that is
    /// on `on`.
    fn here_session(&self, on: &Machine) -> Option<&str> {
        let here = self.listing.here.as_ref().filter(|here| here.on() == on)?;
        Some(here.session())
    }

    /// The pane luneta's zellij is on, where that is on `on` and the sidebar
    /// knows it.
    fn here_pane(&self, on: &Machine) -> Option<&PanePlace> {
        self.listing.here.as_ref().filter(|here| here.on() == on)?.pane()
    }

    /// Whether `place` on `on` is of the session luneta's zellij is in.
    fn is_here(&self, on: &Machine, place: &Place) -> bool {
        self.here_session(on) == Some(place.session())
    }

    /// The agent in the pane luneta's zellij is on, if one is, and the
    /// agents of its machine.
    fn yours(&self) -> Option<(&Seated, &[Seated])> {
        let here = self.listing.here.as_ref()?;
        let (pane, agents) = (here.pane()?, &self.runs(here.on())?.agents);
        Some((agents.iter().find(|seated| seated.place == *pane)?, agents))
    }

    /// The line of `row`, `width` wide, at `now`, the spinner at `frame`.
    fn row_line(&self, row: &Row, width: usize, frame: usize, now: SystemTime) -> Line<'static> {
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (lead, name_style, right) = match &row.key {
            Key::Live { on, place } => {
                let live = self.live(on, place);
                if self.is_here(on, place) {
                    let style = Style::new().fg(NAME).add_modifier(Modifier::BOLD);
                    let rule = Span::styled("here", Style::new().fg(ACCENT));
                    (None, style, vec![rule])
                } else {
                    let mut right = Vec::new();
                    if let Some(live) = live.filter(|live| live.waiting > 0) {
                        let (dot, style) = Mark::Waiting.glyph(frame);
                        right.push(Span::styled(format!("{dot}{} ", live.waiting), style));
                    }
                    if let Some(live) = live {
                        let age = format!("{} ago", sidebar::age(live.started, now));
                        right.push(Span::styled(age, Style::new().fg(gruvbox::GREY)));
                    }
                    (None, bold, right)
                }
            },
            Key::Exited { on, session } => {
                let made = self.kept(on).and_then(|(kept, at)| {
                    let exited = kept.iter().find(|exited| exited.name == *session)?;
                    at.checked_sub(exited.ago)
                });
                let age = made.map(|made| format!("{} ago", sidebar::age(made, now)));
                let grey = Style::new().fg(gruvbox::GREY);
                (None, dim, age.into_iter().map(|age| Span::styled(age, grey)).collect())
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
                let age = seated.since.map(|since| sidebar::age(since, now)).unwrap_or_default();
                let mut right = Vec::new();
                // The directory goes first, where the row is short of room.
                if width >= row.name.width() + seated.dir.width() + age.width() + 10 {
                    right.push(Span::styled(seated.dir.clone(), Style::new().fg(gruvbox::GREY)));
                    right.push(Span::raw("  "));
                }
                right.push(Span::styled(age, dim));
                (Some(Span::styled(glyph, style)), name_style, right)
            },
            Key::Dir { path, .. } => return dir_line(row, path.as_str(), width),
        };
        let right_width: usize = right.iter().map(|span| span.content.width()).sum();
        let lead_width = lead.as_ref().map_or(0, |lead| lead.content.width() + 1);
        // A space either side, and a space between the name and the right.
        let room = width.saturating_sub(2 + lead_width + right_width + 1);
        let (name, named) = sidebar::lit(&row.name, room, Keep::Start, &row.hits, name_style);
        let mut spans = vec![Span::raw(" ")];
        if let Some(lead) = lead {
            spans.push(lead);
            spans.push(Span::raw(" "));
        }
        spans.extend(name);
        let gap = width.saturating_sub(1 + lead_width + named + right_width + 1);
        if matches!(&row.key, Key::Live { on, place } if self.is_here(on, place)) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled("─".repeat(gap.saturating_sub(2)), Style::new().fg(ACCENT)));
            spans.push(Span::raw(" "));
        } else {
            spans.push(Span::raw(" ".repeat(gap)));
        }
        spans.extend(right);
        spans.push(Span::raw(" "));
        Line::from(spans)
    }

    /// What the list says while it has no rows.
    fn empty(&self) -> String {
        if !self.term.is_empty() {
            return format!("no match for \"{}\"", self.term);
        }
        match self.list {
            List::Sessions if self.exited.values().any(|it| matches!(it, Ended::Asking(_))) => {
                "asking zellij…".to_owned()
            },
            List::Sessions => "no sessions".to_owned(),
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

    /// Draws the preview of the row highlighted into `area`.
    fn draw_preview(&self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        let inner = usize::from(area.width.saturating_sub(2));
        let grey = Style::new().fg(gruvbox::GREY);
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Some(row) = self.rows().selected() else {
            let lines = vec![
                Line::styled(" nothing highlighted", grey),
                Line::default(),
                Line::styled(" Enter takes what you type", grey),
            ];
            framed(buffer, area, "Preview", "", lines);
            return;
        };
        let (right, caption, looked) = match &row.key {
            Key::Exited { on, .. } => {
                let mut caption = vec![Span::raw(" ")];
                caption.extend(machined(on, inner));
                caption.push(Span::styled("ended", grey));
                let mut lines = vec![Line::from(caption), Line::default()];
                let said = "zellij kept how it was laid out to bring it back from, and nothing \
                            runs in it to look at";
                lines.extend(menu::wrap(said, inner, grey));
                framed(buffer, area, &row.name, "", lines);
                return;
            },
            Key::Dir { on, path, .. } => {
                self.draw_contents(buffer, area, &row.name, on, path);
                return;
            },
            Key::Live { on, place } => {
                let live = self.live(on, place);
                let panes = live.map_or(0, |live| live.panes.len());
                let right = if panes == 1 { "1 pane".to_owned() } else { format!("{panes} panes") };
                let focus = live.and_then(|live| live.focus);
                (right, None, (on.clone(), place.listed().clone(), focus))
            },
            Key::Agent { on, place } => {
                let caption = self.seated(on, place).map(|seated| {
                    let status = if seated.mark == Mark::Waiting {
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(gruvbox::FG1)
                    };
                    let age = seated.since.map(|since| sidebar::age(since, now));
                    let mut caption = vec![Span::raw(" ")];
                    caption.extend(machined(on, inner));
                    caption.extend([
                        Span::styled(seated.status.clone(), status),
                        Span::styled(" · ", dim),
                        Span::styled(age.unwrap_or_default(), dim),
                        Span::styled(" · ", dim),
                        Span::styled(seated.dir.clone(), grey),
                    ]);
                    Line::from(caption)
                });
                (String::new(), caption, (on.clone(), place.session.clone(), Some(place.pane)))
            },
        };
        let peek = self.peeks.get(&looked);
        // A session's caption names its machine, where that is another, and
        // the pane it shows, once zellij says which.
        let caption = caption.unwrap_or_else(|| {
            let (on, session, asked) = &looked;
            let pane = match peek {
                Some(Peek::Ready { pane, .. }) => Some(*pane),
                _ => *asked,
            };
            let sessions = self.runs(on).map_or(&[][..], |runs| &runs.sessions);
            let live = sessions.iter().find(|live| live.place.listed() == session);
            let panes = live.map_or(&[][..], |live| &live.panes);
            let shown = pane.and_then(|pane| panes.iter().find(|shown| shown.id == pane));
            let mut caption = vec![Span::raw(" ")];
            caption.extend(machined(on, inner));
            if let Some(shown) = shown {
                caption.extend([
                    Span::styled(shown.tab.clone(), grey),
                    Span::styled(" · ", dim),
                    Span::styled(shown.label.clone(), dim),
                ]);
            }
            Line::from(caption)
        });
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
            // The last lines of the screen that fit.
            let shown = under_caption(area);
            let top = shot.end.saturating_sub(usize::from(shown.height)).max(shot.first);
            shot.show(buffer, shown, top);
        }
    }

    /// Draws into `area` the preview of the directory at `path` of `on`,
    /// where `Enter` makes the session `name`: its path, after the name of
    /// its machine where that is another, and under it the first of what eza
    /// listed in it that fit.
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

    /// The search, `width` wide: what is typed, and what `Enter` does, on
    /// the right, where it has the room.
    fn prompt(&self, width: usize) -> Line<'static> {
        let action = match self.rows().selected() {
            Some(row) => match &row.key {
                Key::Live { on, place } if self.is_here(on, place) => {
                    Ok("you are in it".to_owned())
                },
                Key::Live { on, .. } => Ok(there("Attach", on)),
                Key::Exited { on, .. } => Ok(there("Bring back", on)),
                Key::Agent { on, .. } => Ok(there("Go to agent", on)),
                Key::Dir { on, session: Ok(session), .. } => {
                    Ok(there(&format!("Make \"{session}\""), on))
                },
                Key::Dir { session: Err(why), .. } => Err((*why).to_owned()),
            },
            None if self.list == List::Sessions => match self.new_name() {
                Ok(Some(name)) => Ok(format!("Make \"{name}\"")),
                Ok(None) => Ok(String::new()),
                Err(why) => Err(why.to_owned()),
            },
            None => Ok(String::new()),
        };
        typed(&self.term, action, width)
    }

    /// Draws the keys the help line names into `help`, if it has the room,
    /// and says the spots on them.
    fn draw_help(&self, buffer: &mut Buffer, help: Option<Rect>) -> Spots<Pick> {
        let Some(help) = help else {
            return Spots::default();
        };
        let (keys, spots) = menu::keyed(usize::from(help.width), &self.keys());
        buffer.set_line(help.x, help.y, &keys, help.width);
        spots.map(Pick::Key).placed(help)
    }

    /// The keys the help line names.
    fn keys(&self) -> Vec<(Hint, &'static str)> {
        match (&self.stage, self.list) {
            (Stage::Asking { .. }, _) => vec![(Hint::Char('y'), "Yes"), (Hint::AnyOtherKey, "No")],
            (Stage::Planning { .. }, _) => vec![(Hint::Esc, "Give up")],
            (Stage::Rename { .. }, _) => vec![(Hint::Enter, "Rename"), (Hint::Esc, "Cancel")],
            (Stage::Search, List::Sessions) => vec![
                (Hint::Moves, "Move"),
                (Hint::Enter, "Go"),
                (Hint::Tab, List::Sessions.next().title()),
                (Hint::Ctrl('r'), "Rename"),
                (Hint::Delete, "Delete"),
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

    /// Draws the box the new name of `session` on `on` is typed in, `input`
    /// so far, and says the spots on its keys.
    fn draw_rename(
        &self,
        buffer: &mut Buffer,
        layout: &Layout,
        on: &Machine,
        session: &SessionName,
        input: &str,
    ) -> Spots<Pick> {
        if let Some(full) = layout.full {
            let inner = usize::from(full.width.saturating_sub(2));
            let mut lines = vec![Line::default()];
            let mut what = there(&format!("renaming \"{session}\""), on);
            if self.here_session(on) == Some(session.as_str()) {
                what.push_str(", the session you are in");
            }
            lines.extend(menu::wrap(&what, inner, Style::new().fg(gruvbox::GREY)));
            framed(buffer, full, "Rename", "", lines);
        }
        let width = usize::from(layout.input.width.saturating_sub(2));
        let action = self.rename_to(on, session, input).map(|_| there("Rename", on));
        draw_input(buffer, layout, "Rename", typed(input, action.map_err(str::to_owned), width));
        self.draw_help(buffer, layout.help)
    }
}

/// Whether `key` types what it says: with no modifier, or only `Shift`.
fn plain(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

/// The name the row of the agent `seated` shows, one of `agents`, its
/// machine's: its label, and where another of them shows it too, its pane,
/// and its session where the two sit in two, as `luneta/main:4`, or
/// `luneta/main (review:4)`.
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

/// Puts `rows`, of a machine's, after those `into` has, the first of them
/// under `rule`, its machine's, where it has one; and says where the first
/// of them went, where it has any.
fn grouped(into: &mut Vec<Row>, mut rows: Vec<Row>, rule: Option<String>) -> Option<usize> {
    let first = rows.first_mut()?;
    first.rules.machine = rule;
    let at = into.len();
    into.extend(rows);
    Some(at)
}

/// The rows of `candidates`, each a name, what else a search finds it by,
/// and what it stands for, that `finder` finds: all of them, in their order,
/// for no search; or else as the sidebar's search weighs them, the best
/// first, and those that fit alike in their order.
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
    // A stable sort, which keeps those that fit alike in their order.
    found.sort_by_key(|(fit, _)| Reverse(*fit));
    found.into_iter().map(|(_, row)| row).collect()
}

/// The rows of `dirs` of `on`, each with the session `Enter` makes there, or
/// why it makes none yet, that match `term`: all of them, in zoxide's order,
/// for no term; or else those whose path matches it, or the name the row
/// shows, one named `term` first, or whose own name is, then the best
/// matches. A row shows the session's name, or the directory's own while
/// there is none. Its hits are of its path: what you type of a directory is
/// mostly its path.
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

/// The line of a directory's row, `width` wide: the name of the session
/// `Enter` makes there, and at its right the directory's `path`, the
/// characters a search found of it lit, its start cut where the row is short
/// of room. The name keeps a third of the row at least.
fn dir_line(row: &Row, path: &str, width: usize) -> Line<'static> {
    // A space either side, and two between the name and the path.
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

/// What `Enter` does, `action`, on a row of `on`'s: on that machine, where
/// it is another.
fn there(action: &str, on: &Machine) -> String {
    match on {
        Machine::Local => action.to_owned(),
        Machine::Remote(destination) => format!("{action} on {destination}"),
    }
}

/// What a preview's caption says first of a row of `on`'s, `width` wide: the
/// name of its machine, where that is another, in half of it at most.
fn machined(on: &Machine, width: usize) -> Vec<Span<'static>> {
    let Machine::Remote(destination) = on else {
        return Vec::new();
    };
    let machine = sidebar::cut(&destination.to_string(), width / 2);
    let dim = Style::new().add_modifier(Modifier::DIM);
    vec![Span::styled(machine, Style::new().fg(NAME)), Span::styled(" · ", dim)]
}

/// The line of what is in the pane luneta's zellij is on, pinned over the
/// list, `width` wide.
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

/// The rule over the rows of the machine that goes by `name`, `width` wide.
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

/// The rule over the rows of the sessions that ended, `width` wide.
fn separator(label: &str, width: usize) -> Line<'static> {
    let rule = width.saturating_sub(1 + label.width() + 1 + 1);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(label.to_owned(), Style::new().fg(gruvbox::GREY)),
        Span::raw(" "),
        Span::styled("─".repeat(rule), Style::new().add_modifier(Modifier::DIM)),
    ])
}

/// Which of `total` lines show in `visible`, the line `selected` among them:
/// from the first, until it goes past the last that shows.
fn viewport(selected: usize, total: usize, visible: usize) -> (usize, usize) {
    if visible >= total {
        return (0, total);
    }
    let start = (selected + 1).saturating_sub(visible);
    (start, (start + visible).min(total))
}

/// Where a preview in the box at `area` shows what it read: under its
/// caption and the blank line after it, a column in from the box's edge.
fn under_caption(area: Rect) -> Rect {
    Rect::new(area.x + 2, area.y + 3, area.width.saturating_sub(4), area.height.saturating_sub(4))
}

/// `notes` and `body` at the bottom of `rows` lines, blank ones over them,
/// as the list grows up from the search; and the line the body starts on.
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

/// Which row of how many the highlight is on, as `3/7`.
fn count(at: Option<usize>, total: usize) -> String {
    match at {
        Some(at) if total > 0 => format!("{}/{total}", at + 1),
        _ if total > 0 => total.to_string(),
        _ => String::new(),
    }
}

/// What is typed, `width` wide, and what `Enter` does with it on the right,
/// or why it does nothing, where it has the room.
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

/// Draws `lines` in a box at `area`, under a rule that names it `title`, and
/// says `right` at its right: its sides run down every line it holds, as
/// many as `lines` or not.
fn framed(buffer: &mut Buffer, area: Rect, title: &str, right: &str, mut lines: Vec<Line<'_>>) {
    let inner = usize::from(area.width.saturating_sub(2));
    lines.resize(usize::from(area.height.saturating_sub(2)), Line::default());
    menu::boxed(buffer, area, &menu::rule(title, right, inner), &lines);
}

/// Draws the search's box, titled `title`, with `prompt` in it.
fn draw_input(buffer: &mut Buffer, layout: &Layout, title: &str, prompt: Line<'static>) {
    let input = layout.input;
    if layout.bordered {
        framed(buffer, input, title, "", vec![prompt]);
    } else {
        buffer.set_line(input.x, input.y, &prompt, input.width);
    }
}

/// Where the picker's parts go in the box: the search over the help line at
/// the bottom, and the list over it, with the preview at its right where both
/// have the room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    results: Option<Rect>,
    preview: Option<Rect>,
    /// The list and the preview, together.
    full: Option<Rect>,
    input: Rect,
    /// Whether the search has its box, as it does where it has the rows.
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

    use ratatui::layout::Position;

    use super::*;
    use crate::close::Doing;
    use crate::spots::Hit;

    const NOW: Duration = Duration::from_secs(1000);

    fn now() -> SystemTime {
        UNIX_EPOCH + NOW
    }

    /// A session that started `started` seconds after the epoch.
    fn listed(name: &str, started: u64) -> Listed {
        Listed::new(name, UNIX_EPOCH + Duration::from_secs(started))
    }

    /// A session that runs, with one pane, and the client on `focus`, if
    /// one is in it.
    fn live(name: &str, started: u64, focus: Option<u32>) -> Live {
        Live {
            place: Place::Session(listed(name, started)),
            started: UNIX_EPOCH + Duration::from_secs(started),
            panes: vec![Shown { id: 0, tab: "Tab #1".to_owned(), label: format!("{name}/w") }],
            focus,
            waiting: 0,
        }
    }

    /// An agent in the pane `pane` of the session `name`, which started at
    /// `started`.
    fn seated(name: &str, started: u64, pane: u32, label: &str, mark: Mark) -> Seated {
        Seated {
            place: PanePlace { session: listed(name, started), pane },
            label: label.to_owned(),
            mark,
            status: "idle".to_owned(),
            since: Some(now() - Duration::from_mins(1)),
            dir: "luneta/main".to_owned(),
        }
    }

    /// luneta's zellij in the session `session` of this machine's.
    fn here(session: &str) -> Here {
        Here::Session { on: Machine::Local, session: session.to_owned() }
    }

    /// The sessions that run on a machine, and no agent.
    fn sessions(sessions: Vec<Live>) -> Runs {
        Runs { sessions, agents: Vec::new() }
    }

    /// The agents of a machine, and none of their sessions.
    fn agents(agents: Vec<Seated>) -> Runs {
        Runs { sessions: Vec::new(), agents }
    }

    fn ended(name: &str, ago: u64) -> Exited {
        Exited { name: SessionName::running(name).unwrap(), ago: Duration::from_secs(ago) }
    }

    /// The directory at `path`.
    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    /// A directory zoxide knows, for which a session is named `name`.
    fn visited(path: &str, name: &str) -> Dir {
        Dir { path: directory(path), name: SessionName::parse(name).unwrap() }
    }

    /// What zoxide on this machine said of its directories, as the picker
    /// asks it first.
    fn heard_here(visited: Result<Vec<Dir>, String>) -> Heard {
        Heard::Dirs { on: Machine::Local, ask: 1, visited }
    }

    /// What zellij on this machine said of the sessions it keeps to bring
    /// back, as the picker asks it first.
    fn kept_here(exited: Result<Vec<Exited>, String>) -> Heard {
        Heard::Exited { on: Machine::Local, ask: 1, exited }
    }

    /// The asks of `picker` of the sessions zellij keeps to bring back,
    /// and of nothing else.
    fn exited_asks(picker: &mut Picker) -> Vec<Ask> {
        picker.asks().into_iter().filter(|ask| matches!(ask, Ask::Exited { .. })).collect()
    }

    /// Three sessions that run, `b` the one luneta's zellij is in, `c` the
    /// newest, and two that ended, `y` later than `x`.
    fn three() -> Picker {
        let listing = Listing {
            here: Some(here("b")),
            local: sessions(vec![live("c", 30, None), live("b", 20, Some(1)), live("a", 10, None)]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Sessions, listing, Machine::Local);
        picker.heard(kept_here(Ok(vec![ended("x", 100), ended("y", 10)])));
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

    /// The picker drawn over a zellij side of 200 by 50.
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

    /// Where the list is drawn, and the preview, over that zellij side.
    fn parts() -> (Rect, Rect) {
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        let inner = |rect: Rect| Rect::new(rect.x + 1, rect.y + 1, rect.width - 2, rect.height - 2);
        let layout = Layout::of(inner(area));
        (inner(layout.results.unwrap()), inner(layout.preview.unwrap()))
    }

    /// The lines of the list, top to bottom, and the one highlighted.
    fn list(picker: &Picker) -> (Vec<String>, Option<String>) {
        let buffer = drawn(picker);
        let (results, _) = parts();
        let lines = text(&buffer, results);
        let lit = (results.y..results.bottom()).position(|y| buffer[(results.x, y)].bg == LIT);
        let lit = lit.map(|at| lines[at].clone());
        (lines.into_iter().filter(|line| !line.is_empty()).collect(), lit)
    }

    /// The lines of the list, top to bottom, each with what of it is in the
    /// accent, as a search lights what it found.
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

    /// The first word of each line of the list.
    fn names(picker: &Picker) -> Vec<String> {
        let (lines, _) = list(picker);
        lines.iter().map(|line| line.split(' ').next().unwrap_or("").to_owned()).collect()
    }

    /// The first word of each line of the list, and of a machine's rule
    /// its name after `──`.
    fn grouped(picker: &Picker) -> Vec<String> {
        let (lines, _) = list(picker);
        let word = |line: &String| match line.strip_prefix("─ ") {
            Some(rule) => format!("── {}", rule.split(' ').next().unwrap_or("")),
            None => line.split(' ').next().unwrap_or("").to_owned(),
        };
        lines.iter().map(word).collect()
    }

    /// Another machine, `nixos`.
    fn nixos() -> Machine {
        Machine::Remote("nixos".parse().unwrap())
    }

    /// What luneta lists of this machine, `desk`, and of `nixos`, which
    /// lunetad reaches.
    fn two_machines() -> Listing {
        on_nixos(Runs::default())
    }

    /// What luneta lists of this machine, `desk`, which runs nothing, and of
    /// `nixos`, which lunetad reaches, on which `runs` runs.
    fn on_nixos(runs: Runs) -> Listing {
        let remotes = vec![("nixos".parse().unwrap(), runs)];
        Listing { this: "desk".to_owned(), remotes, ..Listing::default() }
    }

    /// What the picker says, all of it, a line after another.
    fn said(picker: &Picker) -> String {
        let buffer = drawn(picker);
        text(&buffer, buffer.area).join(" ")
    }

    fn lit(picker: &Picker) -> String {
        let (_, lit) = list(picker);
        lit.unwrap_or_default().split(' ').next().unwrap_or("").to_owned()
    }

    /// The picker asks zellij for the sessions it keeps to bring back as it
    /// opens, and lists the one luneta's zellij is in first, then those that
    /// run, then those that ended under a rule, each newest first, the
    /// highlight on the newest after yours. `Enter` takes zellij to a session
    /// that runs, brings back one that ended, and on yours closes.
    #[test]
    fn the_sessions_go_yours_first_then_those_that_run_then_those_that_ended() {
        let listing = Listing { here: Some(here("b")), ..Listing::default() };
        let on = Machine::Local;
        let asks = vec![Ask::Exited { on: on.clone(), ask: 1 }, Ask::Dirs { on, ask: 1 }];
        assert_eq!(Picker::open(1, List::Sessions, listing, Machine::Local).asks(), asks);
        let mut picker = three();
        assert_eq!(names(&picker), ["b", "c", "a", "ended", "y", "x"]);
        assert_eq!(lit(&picker), "c");
        let c = Place::Session(listed("c", 30));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Go { on: Machine::Local, place: c });
        press(&mut picker, KeyCode::Up);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        for _ in 0..3 {
            press(&mut picker, KeyCode::Down);
        }
        assert_eq!(lit(&picker), "y");
        let y = SessionName::running("y").unwrap();
        let back = Picked::BringBack { on: Machine::Local, session: y };
        assert_eq!(press(&mut picker, KeyCode::Enter), back);
    }

    /// The picker drawn as `drawn` draws it, the first word of each line of
    /// its list with what a click a few columns into that line hits, and
    /// what a click on `key` of its help line hits.
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
            let word = line.split(' ').next().unwrap_or("").to_owned();
            (word, pick(Position::new(results.x + 2, y)))
        });
        // The help line is the box's last but one, under what may name its
        // keys too, as what `Enter` does.
        let help = (over.top()..over.bottom()).rev().find_map(|y| {
            let cells = (over.left()..over.right()).map(|x| buffer[(x, y)].symbol());
            let line: String = cells.collect();
            let at = line.find(key)?;
            Some(Position::new(u16::try_from(line[..at].chars().count()).ok()?, y))
        });
        let spot = help.and_then(pick);
        (rows.collect(), spot)
    }

    /// Each row of the list is a spot on its line that picks it: a click
    /// does what `Enter` does with the highlight on it, bringing back a
    /// session that ended, and closing the picker on yours. It picks the row
    /// drawn there, wherever the list has it since, and nothing once the
    /// list shows it no more. The rule over those that ended is none. A key
    /// of the help line presses it, but for what takes more than a key. No
    /// row picks while the session highlighted is renamed.
    #[test]
    fn a_click_on_a_row_does_what_enter_does_on_it() {
        let mut picker = three();
        let (rows, tab) = clicks(&picker, "<TAB>");
        let spotted: Vec<(&str, bool)> =
            rows.iter().map(|(word, pick)| (word.as_str(), pick.is_some())).collect();
        let rule = ("ended", false);
        assert_eq!(spotted, [("b", true), ("c", true), ("a", true), rule, ("y", true), ("x", true)]);
        let row = |word: &str| {
            let shown = rows.iter().find(|(it, _)| it == word);
            shown.and_then(|(_, pick)| pick.clone()).expect("a row")
        };
        assert_eq!(tab, Some(Pick::Key(KeyEvent::from(KeyCode::Tab))));
        assert_eq!(clicks(&picker, "<↓↑>").1, None);
        // `d` starts once the list is drawn, and takes the line `c` had.
        let running = vec![
            live("d", 40, None),
            live("c", 30, None),
            live("b", 20, Some(1)),
            live("a", 10, None),
        ];
        let local = sessions(running);
        picker.listed(Listing { here: Some(here("b")), local, ..Listing::default() });
        let c = Picked::Go { on: Machine::Local, place: Place::Session(listed("c", 30)) };
        assert_eq!(picker.click(row("c")), c);
        assert_eq!(picker.click(row("b")), Picked::Close);
        let y = SessionName::running("y").unwrap();
        assert_eq!(picker.click(row("y")), Picked::BringBack { on: Machine::Local, session: y });
        typed(&mut picker, "c");
        assert_eq!(picker.click(row("a")), Picked::Nothing);
        press(&mut picker, KeyCode::Backspace);
        assert_eq!(picker.click(row("c")), c);
        control(&mut picker, 'r');
        assert_eq!(picker.click(row("a")), Picked::Nothing);
        let rename = clicks(&picker, "<ENTER>").1;
        assert_eq!(rename, Some(Pick::Key(KeyEvent::from(KeyCode::Enter))));
    }

    /// While a delete waits on its plan, or asks, no row is a spot, as a
    /// click on one picks nothing then: only the keys the help line names
    /// are.
    #[test]
    fn no_row_is_a_spot_while_a_delete_waits_or_asks() {
        let mut picker = three();
        let session = listed("c", 30);
        let planning = Picked::Plan { on: Machine::Local, session, deletes: Deletes::Session };
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        let (rows, give_up) = clicks(&picker, "<ESC>");
        assert!(rows.len() > 1 && rows.iter().all(|(_, pick)| pick.is_none()), "{rows:?}");
        assert_eq!(give_up, Some(Pick::Key(KeyEvent::from(KeyCode::Esc))));
        let c = listed("c", 30);
        let kill = Act::Kill { session: c.name().clone(), started: c.started() };
        let plan = Plan::of(kill, Ok(&[(0, Doing::Busy)]));
        assert_eq!(picker.planned(Ok(plan)), Picked::Nothing);
        let (rows, yes) = clicks(&picker, "<y>");
        assert!(rows.len() > 1 && rows.iter().all(|(_, pick)| pick.is_none()), "{rows:?}");
        assert_eq!(yes, Some(Pick::Key(KeyEvent::from(KeyCode::Char('y')))));
    }

    /// What is typed narrows the list, a name that is what is typed first,
    /// and `Enter` makes a session of a name no row has, once zellij said
    /// which sessions ended, as a name it keeps would be a row then; but of
    /// one zellij refuses, or of yours, or on the agents or the directories,
    /// which `Tab` goes to one after the other, and back to the sessions, and
    /// `Shift Tab` the other way round, the rule over the list naming each.
    #[test]
    fn what_is_typed_narrows_the_list_and_enter_makes_a_session_no_row_has() {
        let listing = Listing {
            here: Some(here("elo")),
            local: sessions(vec![
                live("luneta-main", 30, None),
                live("luneta", 20, None),
                live("elo", 10, None),
            ]),
            ..Listing::default()
        };
        let mut picker = Picker::open(1, List::Sessions, listing, Machine::Local);
        typed(&mut picker, "luneta");
        assert_eq!(names(&picker), ["luneta", "luneta-main"]);
        typed(&mut picker, "-new");
        assert_eq!(list(&picker).0, [r#"no match for "luneta-new""#]);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        let why = "zellij has yet to say which sessions ended";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        picker.heard(kept_here(Ok(Vec::new())));
        let new = SessionName::parse("luneta-new").unwrap();
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Start(new));
        let titled = |picker: &Picker, title: &str| {
            let buffer = drawn(picker);
            let (results, _) = parts();
            let rule = Rect::new(results.x - 1, results.y - 1, results.width + 2, 1);
            text(&buffer, rule)[0].starts_with(&format!("┌─ {title} ─"))
        };
        assert!(titled(&picker, "Sessions"));
        for title in ["Agents", "Directories"] {
            press(&mut picker, KeyCode::Tab);
            assert!(titled(&picker, title), "{title}");
            assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
        }
        for title in ["Agents", "Sessions", "Directories"] {
            press(&mut picker, KeyCode::BackTab);
            assert!(titled(&picker, title), "{title}");
        }
        press(&mut picker, KeyCode::Tab);
        for (term, why) in [("a/b", "name cannot contain '/'"), ("elo", "you are in it")] {
            for _ in 0..20 {
                press(&mut picker, KeyCode::Backspace);
            }
            typed(&mut picker, term);
            if term == "elo" {
                // Yours is listed: Enter on it closes, as it would.
                assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
                continue;
            }
            assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Nothing);
            let buffer = drawn(&picker);
            assert!(text(&buffer, buffer.area).iter().any(|line| line.contains(why)), "{term}");
        }
    }

    /// `Del` on a session plans deleting it, and asks first, as the menu
    /// does, while the plan ends an agent at work: `y` plans again, and goes
    /// ahead with the plan it asked about, but asks again of one that changed
    /// meanwhile. Any other key gives it up.
    #[test]
    fn del_asks_before_ending_an_agent_at_work_and_goes_ahead_on_yes_to_the_same_plan() {
        let mut picker = three();
        let c = listed("c", 30);
        let deletes = Deletes::Session;
        let planning = Picked::Plan { on: Machine::Local, session: c.clone(), deletes };
        let kill = || Act::Kill { session: c.name().clone(), started: c.started() };
        let plan = |doing: Doing| Plan::of(kill(), Ok(&[(0, doing)]));
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Busy))), Picked::Nothing);
        let asks = |picker: &Picker| {
            let buffer = drawn(picker);
            text(&buffer, buffer.area).iter().any(|line| line.contains("Delete session c?"))
        };
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('n')), Picked::Nothing);
        assert!(!asks(&picker));

        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        picker.planned(Ok(plan(Doing::Busy)));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), Picked::Nothing);
        assert!(asks(&picker));
        assert_eq!(press(&mut picker, KeyCode::Char('y')), planning);
        let killed = Picked::Delete { on: Machine::Local, act: kill() };
        assert_eq!(picker.planned(Ok(plan(Doing::Waiting))), killed);

        // An idle agent ends without a question.
        assert_eq!(press(&mut picker, KeyCode::Delete), planning);
        assert_eq!(picker.planned(Ok(plan(Doing::Idle))), killed);
    }

    /// `Del` on an agent asks zellij first what closing its pane closes, and
    /// plans by what it answers: only for that pane, while it waits on it.
    #[test]
    fn del_on_an_agent_asks_zellij_what_closing_its_pane_closes_first() {
        let place = PanePlace { session: listed("c", 30), pane: 4 };
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
        let other = PanePlace { session: listed("c", 30), pane: 5 };
        assert_eq!(picker.heard(heard(Machine::Local, other)), Picked::Nothing);
        assert_eq!(picker.heard(heard(nixos(), place.clone())), Picked::Nothing);
        let deletes = Deletes::Pane { pane: 4, closes: Closes::Pane };
        let plan = Picked::Plan { on: Machine::Local, session: listed("c", 30), deletes };
        assert_eq!(picker.heard(heard(Machine::Local, place)), plan);
    }

    /// `Del` on a session that ended lets go of it at once, as nothing runs
    /// in it to end, and asks zellij again for those it keeps once it did.
    /// The highlight goes on to the row after, and what is typed stays, so
    /// that `Del` again deletes no row off the top of the list.
    #[test]
    fn del_lets_go_of_a_session_that_ended_at_once() {
        let mut picker = three();
        picker.asks();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "y");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let delete = |session| Ask::Delete { on: Machine::Local, session };
        let y = SessionName::running("y").unwrap();
        assert_eq!(picker.asks(), vec![delete(y.clone())]);
        assert!(!names(&picker).contains(&"y".to_owned()));
        assert_eq!(lit(&picker), "x");
        let deleted = Err("zellij: no".to_owned());
        picker.heard(Heard::Deleted { on: Machine::Local, session: y, deleted });
        assert_eq!(picker.asks(), vec![Ask::Exited { on: Machine::Local, ask: 2 }]);
        let (lines, _) = list(&picker);
        assert!(lines.iter().any(|line| line.contains("y stays: zellij: no")), "{lines:?}");

        typed(&mut picker, "x");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert_eq!(picker.asks(), vec![delete(SessionName::running("x").unwrap())]);
        assert_eq!(list(&picker).0, [r#"no match for "x""#]);
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        assert!(picker.asks().is_empty());
    }

    /// A session `Del` let go of stays off the list till zellij said it let
    /// go of it, as what zellij lists before can list it yet: zellij is asked
    /// again which sessions ended only once it said so of each it was letting
    /// go of, `Del` on the row after among them, and of one that ended
    /// meanwhile then.
    #[test]
    fn a_session_let_go_of_stays_off_the_list_till_zellij_said_it_let_go() {
        let mut picker = three();
        picker.asks();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "y");
        press(&mut picker, KeyCode::Delete);
        assert_eq!(lit(&picker), "x");
        press(&mut picker, KeyCode::Delete);
        picker.asks();
        let a_ended = Listing {
            here: Some(here("b")),
            local: sessions(vec![live("c", 30, None), live("b", 20, Some(1))]),
            ..Listing::default()
        };
        picker.listed(a_ended);
        assert!(exited_asks(&mut picker).is_empty());
        let let_go = |picker: &mut Picker, name: &str| {
            let session = SessionName::running(name).unwrap();
            picker.heard(Heard::Deleted { on: Machine::Local, session, deleted: Ok(()) });
            exited_asks(picker)
        };
        assert!(let_go(&mut picker, "y").is_empty());
        assert_eq!(names(&picker), ["b", "c"]);
        assert_eq!(let_go(&mut picker, "x"), [Ask::Exited { on: Machine::Local, ask: 2 }]);
        picker.heard(Heard::Exited { on: Machine::Local, ask: 2, exited: Ok(vec![ended("a", 5)]) });
        assert_eq!(names(&picker), ["b", "c", "ended", "a"]);
    }

    /// `Ctrl r` renames the session highlighted, yours or another, to a name
    /// no session has, those that ended among them. zellij renames none that
    /// ended.
    #[test]
    fn ctrl_r_renames_the_highlighted_session_to_a_name_no_session_has() {
        let mut picker = three();
        picker.asks();
        let renaming = |picker: &Picker| {
            let buffer = drawn(picker);
            text(&buffer, buffer.area).iter().any(|line| line.contains(r#"renaming "c""#))
        };
        control(&mut picker, 'r');
        assert!(renaming(&picker));
        for taken in ["a", "x", "c"] {
            typed(&mut picker, taken);
            press(&mut picker, KeyCode::Enter);
            assert!(picker.asks().is_empty(), "{taken}");
            press(&mut picker, KeyCode::Backspace);
        }
        typed(&mut picker, "d");
        press(&mut picker, KeyCode::Enter);
        let to = SessionName::parse("d").unwrap();
        let from = SessionName::running("c").unwrap();
        assert_eq!(picker.asks(), vec![Ask::Rename { on: Machine::Local, from, to }]);
        assert!(!renaming(&picker));

        typed(&mut picker, "x");
        control(&mut picker, 'r');
        let buffer = drawn(&picker);
        let said = text(&buffer, buffer.area).join(" ");
        assert!(!said.contains("renaming"));
        assert!(said.contains("zellij renames only a session that runs"), "{said}");
    }

    /// A session that runs no more can be one zellij keeps to bring back, so
    /// no name is free until zellij says again which sessions ended: a
    /// rename waits on it, and those it said ended show on meanwhile. An
    /// answer to an ask before the last frees none, though it comes after a
    /// session ended: it can leave that session out.
    #[test]
    fn no_name_is_free_until_zellij_says_again_which_sessions_ended() {
        let mut picker = three();
        picker.asks();
        let listing = |newer: &[Live]| {
            let running = [newer, &[live("c", 30, None), live("b", 20, Some(1))][..]].concat();
            Listing { here: Some(here("b")), local: sessions(running), ..Listing::default() }
        };
        // `a` ends; then `d` starts, and ends while zellij is asked after `a`.
        picker.listed(listing(&[]));
        picker.listed(listing(&[live("d", 40, None)]));
        picker.listed(listing(&[]));
        let exited = |ask| Ask::Exited { on: Machine::Local, ask };
        assert_eq!(picker.asks(), vec![exited(2), exited(3)]);
        assert!(names(&picker).contains(&"x".to_owned()));
        control(&mut picker, 'r');
        typed(&mut picker, "d");
        press(&mut picker, KeyCode::Enter);
        assert!(picker.asks().is_empty());
        let before_d = vec![ended("x", 100), ended("y", 10)];
        picker.heard(Heard::Exited { on: Machine::Local, ask: 2, exited: Ok(before_d.clone()) });
        press(&mut picker, KeyCode::Enter);
        assert!(picker.asks().is_empty());
        let since_d = [vec![ended("d", 1)], before_d].concat();
        picker.heard(Heard::Exited { on: Machine::Local, ask: 3, exited: Ok(since_d) });
        press(&mut picker, KeyCode::Enter);
        assert!(picker.asks().is_empty());
        press(&mut picker, KeyCode::Backspace);
        typed(&mut picker, "a");
        press(&mut picker, KeyCode::Enter);
        let (from, to) = (SessionName::running("c").unwrap(), SessionName::parse("a").unwrap());
        assert_eq!(picker.asks(), vec![Ask::Rename { on: Machine::Local, from, to }]);
    }

    /// Agents named alike are told apart by their panes, and by their
    /// sessions too where they sit in two; the one in the pane luneta's
    /// zellij is on is pinned over the list, and is no row; and a search
    /// finds an agent by its session's name, lit where its row shows it.
    #[test]
    fn agents_named_alike_are_told_apart_and_yours_is_pinned_over_them() {
        let place = PanePlace { session: listed("work", 20), pane: 9 };
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
        // Past each row's mark and the space after it, up to the gap before
        // where it works.
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
        // `review` fits the session's name better than the row only by
        // starting it, and lights up where the row shows it.
        let (line, lit) = &rows[1];
        assert!(line.contains("docs (review:4)") && lit == "review", "{rows:?}");
    }

    /// A search finds an agent by its directory as its row and its preview
    /// show it, and weighs it by what fits best, its name or another: its
    /// name lights up only where it fits as well. `zellij` lands on `zellij
    /// panics` whole, on `src/zellij` after its `/`, and on `Zero-length lines
    /// in jobs` letter by letter.
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

    /// The preview asks zellij for the screen it shows only once the
    /// highlight has stayed on its row a while: of the pane a client of the
    /// session is on, or else of the one zellij says a client coming there
    /// would be on. It asks once for each.
    #[test]
    fn the_preview_asks_zellij_once_the_highlight_rests() {
        let mut picker = three();
        picker.asks();
        let due = picker.due().unwrap();
        picker.ticked(due.checked_sub(Duration::from_millis(1)).unwrap());
        assert!(picker.asks().is_empty());
        picker.ticked(due);
        let peek = |session, asked| Ask::Peek { on: Machine::Local, session, asked };
        assert_eq!(picker.asks(), vec![peek(listed("c", 30), None)]);
        assert_eq!(picker.due(), None);
        press(&mut picker, KeyCode::Up);
        let due = picker.due().unwrap();
        picker.ticked(due);
        assert_eq!(picker.asks(), vec![peek(listed("b", 20), Some(1))]);
        press(&mut picker, KeyCode::Down);
        assert_eq!(picker.due(), None);
    }

    /// The picker floats in the middle three fifths of zellij's side, the
    /// list at the left and the preview at the right, which shows the last
    /// lines of what zellij dumped of the pane, inside its box.
    #[test]
    fn the_preview_shows_the_last_lines_zellij_dumped_inside_its_box() {
        let mut picker = three();
        let dumped = (1..=40).map(|n| format!("\x1b[mline {n}")).collect::<Vec<_>>().join("\n");
        let peek = Ok((0, format!("{dumped}\n\n\n")));
        let c = listed("c", 30);
        picker.heard(Heard::Peek { on: Machine::Local, session: c, asked: None, peek });
        let buffer = drawn(&picker);
        let area = Picker::area(Rect::new(0, 0, 200, 50));
        assert_eq!((area.x, area.y, area.width, area.height), (40, 10, 120, 30));
        assert_eq!(buffer[(area.x, area.y)].symbol(), "┌");
        assert_eq!(buffer[(area.right() - 1, area.bottom() - 1)].symbol(), "┘");
        let (_, preview) = parts();
        let lines = text(&buffer, preview);
        assert_eq!(lines[0], "Tab #1 · c/w");
        assert_eq!(lines.last().map(String::as_str), Some("line 40"));
        assert_eq!(lines[2], format!("line {}", 40 - (lines.len() - 3)));
        for y in preview.y..preview.bottom() {
            assert_eq!(buffer[(preview.x - 1, y)].symbol(), "│");
            assert_eq!(buffer[(preview.right(), y)].symbol(), "│");
        }
    }

    /// The directories are zoxide's, in its order, each named for the
    /// session `Enter` makes there, with a postfix where a session that runs
    /// or that ended has that name, yours among them, and its path at the
    /// right. `Del` does nothing to one.
    #[test]
    fn each_directory_is_named_for_the_session_enter_makes_there() {
        let listing = Listing {
            here: Some(here("notes")),
            local: sessions(vec![live("notes", 20, Some(1)), live("luneta", 10, None)]),
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
        assert!(picker.asks().is_empty());
    }

    /// Where zoxide answers before zellij says which sessions ended, or
    /// zellij does not say, a directory shows its own name, and `Enter`
    /// makes nothing there, saying why: `switch-session` would bring back a
    /// session of that name that ended. Once zellij says, the name has its
    /// postfix, and the highlight stays on the row.
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

    /// A search finds a directory by its path, which it lights, or else by
    /// the name of the session `Enter` makes there; a directory named what
    /// is typed comes first. Where its row is short of room, a path loses
    /// its start, and what a search found in the rest stays lit.
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
        // No path has the `-`.
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

    /// `Ctrl d` goes to the directories, and the preview asks eza what is in
    /// the one highlighted once the highlight rests, and lists the first of
    /// it under its path, or says why eza did not list it.
    #[test]
    fn the_preview_lists_what_eza_says_is_in_the_directory() {
        let path = "/home/you/luneta";
        let mut picker = three();
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

    /// Each machine's directories go under a rule of its own, this one's
    /// first, each in its zoxide's order, and a search keeps them so. One of
    /// another machine's is named for the session its daemon said `Enter`
    /// makes there, which it makes there, and its preview asks eza there, and
    /// names the machine over the path.
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

    /// `Ctrl d` on a machine's header has the highlight on the first of its
    /// directories once they come, before this machine's or after, until a
    /// key moves it; and on the best of them the search finds, as what is
    /// typed changes, which `Enter` makes there.
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

    /// zoxide of a machine is asked once, as the picker first lists it.
    /// One that did not list its directories says why over the others'.
    /// One that goes off the listing and comes back is asked again, and
    /// what it answers an ask made before is not taken.
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
        assert!(picker.asks().is_empty());
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

    /// Where zoxide does not answer, the directories say why, and list none.
    #[test]
    fn the_directories_say_why_zoxide_listed_none() {
        let mut picker = Picker::open(1, List::Dirs, Listing::default(), Machine::Local);
        picker.heard(heard_here(Err("zoxide is not on the PATH".to_owned())));
        assert_eq!(list(&picker).0, ["zoxide is not on the PATH", "no directories"]);
    }

    /// Each machine's sessions go under a rule of its own, this one's first,
    /// with those that ended there under each, and a search keeps them so.
    /// `Enter` on another machine's takes zellij there, as the search's line
    /// says, and brings one that ended back there, whose preview names the
    /// machine. One that ends there asks zellij there again which ended, and
    /// zellij here nothing.
    #[test]
    fn every_machine_s_sessions_go_under_its_rule_this_one_s_first() {
        let there = || sessions(vec![live("api", 40, None), live("blog", 5, None)]);
        let listing = || Listing {
            here: Some(here("b")),
            local: sessions(vec![live("c", 30, None), live("b", 20, Some(1))]),
            ..on_nixos(there())
        };
        let mut picker = Picker::open(1, List::Sessions, listing(), Machine::Local);
        let exited = |on, ask| Ask::Exited { on, ask };
        assert_eq!(exited_asks(&mut picker), [exited(Machine::Local, 1), exited(nixos(), 2)]);
        picker.heard(kept_here(Ok(vec![ended("x", 100)])));
        picker.heard(Heard::Exited { on: nixos(), ask: 2, exited: Ok(vec![ended("old", 50)]) });
        let rows = ["── desk", "b", "c", "ended", "x", "── nixos", "api", "blog", "ended", "old"];
        assert_eq!(grouped(&picker), rows);
        assert_eq!(lit(&picker), "c");
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "api");
        assert!(said(&picker).contains("<ENTER> Attach on nixos"), "{}", said(&picker));
        let api = Place::Session(listed("api", 40));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Go { on: nixos(), place: api });
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "old");
        assert!(said(&picker).contains("<ENTER> Bring back on nixos"), "{}", said(&picker));
        assert_eq!(text(&drawn(&picker), parts().1)[0], "nixos · ended");
        let old = SessionName::running("old").unwrap();
        let back = Picked::BringBack { on: nixos(), session: old };
        assert_eq!(press(&mut picker, KeyCode::Enter), back);

        typed(&mut picker, "b");
        assert_eq!(grouped(&picker), ["── desk", "b", "── nixos", "blog"]);
        let ended_there = sessions(vec![live("api", 40, None)]);
        picker.listed(Listing { remotes: on_nixos(ended_there).remotes, ..listing() });
        assert_eq!(picker.asks(), [exited(nixos(), 3)]);
    }

    /// `Del` and `Ctrl r` act on another machine's rows there. A session
    /// that runs there is planned for deleting there, an agent's pane is
    /// asked of zellij there what closing it closes, and one that ended
    /// there is let go of there, after which zellij there is asked again
    /// which ended. A session there takes a name no session there has, those
    /// that ended there among them, once zellij there said which: a name only
    /// this machine's has is free there.
    #[test]
    fn del_and_ctrl_r_act_on_another_machine_s_rows_there() {
        let there = Runs {
            sessions: vec![live("api", 40, None), live("web", 20, None)],
            agents: vec![seated("api", 40, 2, "fix", Mark::Idle)],
        };
        let listing = Listing { local: sessions(vec![live("here", 30, None)]), ..on_nixos(there) };
        let mut picker = Picker::open(1, List::Sessions, listing, Machine::Local);
        picker.heard(kept_here(Ok(vec![ended("gone", 100)])));
        picker.asks();
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "api");
        control(&mut picker, 'r');
        assert!(said(&picker).contains(r#"renaming "api" on nixos"#), "{}", said(&picker));
        let rename = |picker: &mut Picker, to: &str| {
            for _ in 0..10 {
                press(picker, KeyCode::Backspace);
            }
            typed(picker, to);
            press(picker, KeyCode::Enter);
            picker.asks()
        };
        assert!(rename(&mut picker, "here").is_empty());
        let why = "zellij has yet to say which sessions ended";
        assert!(said(&picker).contains(why), "{}", said(&picker));
        picker.heard(Heard::Exited { on: nixos(), ask: 2, exited: Ok(vec![ended("old", 10)]) });
        for taken in ["web", "old"] {
            assert!(rename(&mut picker, taken).is_empty(), "{taken}");
        }
        let from = SessionName::running("api").unwrap();
        let to = SessionName::parse("here").unwrap();
        assert_eq!(rename(&mut picker, "here"), [Ask::Rename { on: nixos(), from, to }]);

        let deletes = Deletes::Session;
        let plan = Picked::Plan { on: nixos(), session: listed("api", 40), deletes };
        assert_eq!(press(&mut picker, KeyCode::Delete), plan);
        press(&mut picker, KeyCode::Esc);
        press(&mut picker, KeyCode::Down);
        press(&mut picker, KeyCode::Down);
        assert_eq!(lit(&picker), "old");
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let old = SessionName::running("old").unwrap();
        assert_eq!(picker.asks(), [Ask::Delete { on: nixos(), session: old.clone() }]);
        assert!(!names(&picker).contains(&"old".to_owned()));
        picker.heard(Heard::Deleted { on: nixos(), session: old, deleted: Ok(()) });
        assert_eq!(picker.asks(), [Ask::Exited { on: nixos(), ask: 3 }]);

        control(&mut picker, 'b');
        assert_eq!(press(&mut picker, KeyCode::Delete), Picked::Nothing);
        let place = PanePlace { session: listed("api", 40), pane: 2 };
        assert_eq!(picker.asks(), [Ask::Closes { on: nixos(), place }]);
    }

    /// zellij of a machine is asked which sessions ended as the picker first
    /// lists it, and again as it comes back once it went off the listing,
    /// with what it said: what it answers an ask made before is not taken.
    /// One that did not say says why over the list.
    #[test]
    fn each_machine_s_zellij_is_asked_which_sessions_ended_as_it_is_listed() {
        let mut picker = Picker::open(1, List::Sessions, Listing::default(), Machine::Local);
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: Machine::Local, ask: 1 }]);
        picker.heard(kept_here(Ok(Vec::new())));
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: nixos(), ask: 2 }]);
        assert_eq!(list(&picker).0, ["asking zellij…"]);
        let older = "nixos: its lunetad, or this one, is older than what luneta asked of it";
        picker.heard(Heard::Exited { on: nixos(), ask: 2, exited: Err(older.to_owned()) });
        let said = list(&picker).0.join(" ");
        assert!(said.contains(&format!("no ended sessions: {older}")), "{said}");

        let gone = || Listing { remotes: Vec::new(), ..two_machines() };
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: nixos(), ask: 3 }]);
        picker.listed(gone());
        picker.listed(two_machines());
        assert_eq!(exited_asks(&mut picker), [Ask::Exited { on: nixos(), ask: 4 }]);
        let before = Ok(vec![ended("before", 5)]);
        picker.heard(Heard::Exited { on: nixos(), ask: 3, exited: before });
        picker.heard(Heard::Exited { on: nixos(), ask: 4, exited: Ok(vec![ended("old", 5)]) });
        assert_eq!(grouped(&picker), ["── nixos", "ended", "old"]);
        picker.listed(gone());
        assert_eq!(list(&picker).0, ["no sessions"]);
    }

    /// A preview that did not come is asked for again as the highlight comes
    /// back to its row, not while it rests there: a screen zellij did not
    /// dump, and a directory eza did not list.
    #[test]
    fn a_preview_that_failed_is_asked_for_again_as_the_highlight_comes_back() {
        let mut picker = three();
        picker.asks();
        picker.ticked(picker.due().unwrap());
        let peek = || Ask::Peek { on: Machine::Local, session: listed("c", 30), asked: None };
        assert_eq!(picker.asks(), [peek()]);
        let failed = Err("zellij: it did not answer in time".to_owned());
        let c = listed("c", 30);
        picker.heard(Heard::Peek { on: Machine::Local, session: c, asked: None, peek: failed });
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
        control(&mut picker, 'j');
        control(&mut picker, 'd');
        picker.ticked(picker.due().unwrap());
        assert_eq!(picker.asks(), [list()]);
    }

    /// A session another machine runs takes no name here: a directory of
    /// this machine's named for one is named so all the same.
    #[test]
    fn a_session_another_machine_runs_takes_no_name_here() {
        let listing = on_nixos(sessions(vec![live("api", 40, None)]));
        let mut picker = Picker::open(1, List::Dirs, listing, Machine::Local);
        picker.heard(kept_here(Ok(Vec::new())));
        picker.heard(heard_here(Ok(vec![visited("/home/you/api", "api")])));
        assert_eq!(grouped(&picker), ["── desk", "api"]);
    }

    /// Each machine's agents go under its rule too, each told from those of
    /// its own machine alone. Where luneta's zellij is on another machine,
    /// its session there is the first of that machine's, `here`, which
    /// `Enter` closes the picker on, and its agent there is pinned over the
    /// list. `Enter` on another machine's agent takes zellij there.
    #[test]
    fn luneta_s_zellij_on_another_machine_is_here_among_that_machine_s_rows() {
        let at = |pane| PanePlace { session: listed("api", 40), pane };
        let there = Runs {
            sessions: vec![live("web", 50, None), live("api", 40, Some(9))],
            agents: vec![
                seated("api", 40, 9, "mine", Mark::Busy),
                seated("api", 40, 2, "fix", Mark::Idle),
            ],
        };
        let listing = Listing {
            here: Some(Here::Pane { on: nixos(), place: at(9) }),
            local: Runs {
                sessions: vec![live("api", 30, None)],
                agents: vec![seated("api", 30, 1, "fix", Mark::Idle)],
            },
            ..on_nixos(there)
        };
        let mut picker = Picker::open(1, List::Sessions, listing, Machine::Local);
        picker.heard(kept_here(Ok(Vec::new())));
        assert_eq!(grouped(&picker), ["── desk", "api", "── nixos", "api", "web"]);
        let (lines, _) = list(&picker);
        assert!(!lines[1].ends_with("here") && lines[3].ends_with("─ here"), "{lines:?}");
        assert_eq!(lit(&picker), "api");
        press(&mut picker, KeyCode::Down);
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Close);
        press(&mut picker, KeyCode::Down);
        let web = Place::Session(listed("web", 50));
        assert_eq!(press(&mut picker, KeyCode::Enter), Picked::Go { on: nixos(), place: web });

        control(&mut picker, 'b');
        let (lines, _) = list(&picker);
        assert!(lines[0].starts_with("mine ─") && lines[0].ends_with("─ here"), "{lines:?}");
        // A rule's machine, and past each row's mark, its name.
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
        let fix = Picked::Go { on: nixos(), place: Place::Pane(at(2)) };
        assert_eq!(press(&mut picker, KeyCode::Enter), fix);
    }

    /// The preview of another machine's session asks zellij there for its
    /// screen, which it shows under the machine's name; a session of the
    /// same name here has a screen of its own, and so has one there that
    /// ended before it started.
    #[test]
    fn the_preview_of_another_machine_s_session_is_its_screen_there() {
        let listing = Listing {
            local: sessions(vec![live("main", 30, None)]),
            ..on_nixos(sessions(vec![live("main", 40, Some(0))]))
        };
        let mut picker = Picker::open(1, List::Sessions, listing, Machine::Local);
        picker.asks();
        press(&mut picker, KeyCode::Down);
        picker.ticked(picker.due().unwrap());
        let main = listed("main", 40);
        let asked = Some(0);
        assert_eq!(picker.asks(), [Ask::Peek { on: nixos(), session: main.clone(), asked }]);
        let peek = |on, session, screen: &str| {
            let peek = Ok((0, screen.to_owned()));
            Heard::Peek { on, session, asked, peek }
        };
        let shown = |picker: &Picker| text(&drawn(picker), parts().1);
        picker.heard(peek(Machine::Local, main.clone(), "here"));
        picker.heard(peek(nixos(), listed("main", 10), "ended"));
        assert_eq!(shown(&picker)[..3], ["nixos · Tab #1 · main/w", "", "reading…"]);
        picker.heard(peek(nixos(), main, "there"));
        assert_eq!(shown(&picker)[..3], ["nixos · Tab #1 · main/w", "", "there"]);
    }
}
