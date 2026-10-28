//! The sidebar: every session on the host, the panes in each, and what the
//! agent in each pane is doing, or the command its shell runs, as the daemon
//! says, beside the zellij luneta runs. A row takes zellij's client there, and
//! `▌` marks the pane it is on. `•` dots a pane where something finished since
//! you were last there, and one you marked unread, and `u` takes the dot away
//! or gives it. The menu opens at its bottom, for the row highlighted, or
//! floats over zellij, for the pane zellij is on, and its Brief group asks
//! for a brief of the agents by that row, or that pane, as its `a` asks for
//! the ask box. Its title along the top is in the accent while it has the
//! keys, and dim while zellij or a box over zellij has them. Collapsed, it
//! keeps only what it counts of the dots, the agents at work and those in a
//! shell.
//!
//! The rows go oldest session first, until you move them, and each session's
//! panes in the order of their tabs, a bracket down the left joining those of
//! a tab that holds more than one, in a colour of the tab's own: `v` selects
//! the highlighted row's session, `j` and `k` grow the selection, and `J` and
//! `K` move the sessions it holds, as a visual selection does in vim; and the
//! mouse drags a session by its row. The order is kept for every luneta, and
//! the next.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::num::NonZeroU64;
use std::ops::Range;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::{SkimMatcherV2, SkimScoreConfig};
use luneta_protocol::git::{Changes, Head, Upstream, WorkTree};
use luneta_protocol::github::{PullRequest, State};
use luneta_protocol::tab;
use lunetad::api::agent::Status as Reported;
use lunetad::api::shell::Doing;
use lunetad::api::{Agent, Client, Front, Host, NameSource, Pane, Session, Shell, Status, Zellij};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::brief::{self, Brief, Note, Over, SessionId};
use crate::close::{self, Act, Deletes, Plan, Scope, Seat, Subject, Untold};
use crate::folds::Folds;
use crate::gruvbox;
use crate::icons;
use crate::ledger::{self, Key};
use crate::menu::{self, Command, Did, Done, Menu, Opens, Target, Top, Wants};
use crate::news::News;
use crate::order::Order;
use crate::own::{Look, Looked, OwnClient, Unplaced};
use crate::picker::{Listing, Live, Seated, Shown};
use crate::skills::Skill;
use crate::zellij::{self, Closes, Open, SessionName};

/// How many columns the sidebar takes until its edge is dragged.
pub const COLUMNS: u16 = 32;

/// The fewest columns the sidebar is dragged to, where the terminal has twice
/// as many: enough for a session's name on its row.
pub const NARROWEST: u16 = 16;

/// How many columns the sidebar takes collapsed: a count's glyph, two digits
/// of it, and the edge.
pub const COLLAPSED: u16 = 4;

/// What marks the pane luneta's zellij is on, in its first column, whether
/// the sidebar has the focus or not.
const HERE_MARK: &str = "▌";
/// The dot of a pane where something finished since you were last there, or
/// that you marked unread, before its mark; and on a folded session's row, of
/// one of its panes.
const NEW: &str = "•";
const UNFOLDED: &str = "▾";
const FOLDED: &str = "▸";
const RULE: &str = "─";
/// What runs down the sidebar's last column, its edge, between it and zellij,
/// open or collapsed.
const EDGE: &str = "│";
/// What joins the panes of a tab that holds more than one, down the column
/// after the mark of the pane luneta's zellij is on: from the first line of
/// the first pane, by each line between, to the last line of the last.
const BRACKET_TOP: &str = "╭";
const BRACKET_SIDE: &str = "│";
const BRACKET_FOOT: &str = "╰";
/// The colours of the brackets, a tab's by its id, as a browser colours its
/// groups of tabs: zellij keeps a tab's id as it moves and as tabs before it
/// close, so the tab keeps its colour, and two tabs opened one after the other
/// never share one. No bracket is red, which says what went wrong. No two
/// neighbours here are alike either, for tabs that go round them.
const TABS: [Color; 6] = [
    gruvbox::BLUE,
    gruvbox::YELLOW,
    gruvbox::PURPLE,
    gruvbox::AQUA,
    gruvbox::ORANGE,
    gruvbox::GREEN,
];
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How long the spinner shows each of its frames.
pub const SPIN: Duration = Duration::from_millis(100);
/// How long a session the mouse lifted is held on the first line of the rows,
/// or on their last, for each line they scroll under it.
pub const SCROLL: Duration = Duration::from_millis(60);

/// The colour of what wants you: an agent that waits for you, and the session
/// luneta's zellij is in; and of the sidebar's title while it has the keys.
pub const ACCENT: Color = gruvbox::PURPLE;
/// The colour of the session luneta's zellij is in.
pub const NAME: Color = gruvbox::AQUA;
/// The colour of an agent at work, and of the search's `/`.
pub const TAG: Color = gruvbox::YELLOW;
/// The colour of what went wrong: the notes at the bottom that say so, the
/// box over zellij that says so for the menu there, and the rule of the
/// sidebar collapsed, which has no room for them, while the daemon does not
/// answer.
pub const ERROR: Color = gruvbox::RED;
/// The background of the highlight while the sidebar has the focus, on the
/// row the keys took it to.
pub const LIT: Color = gruvbox::BG2;
/// The background of the highlight while it rests on your pane, the sidebar
/// without the focus: a step softer, as `▌` marks that row already.
const RESTING: Color = gruvbox::BG1;

/// How many columns a mark takes: one, as each glyph of `Mark` does. Each
/// pane's row keeps that many for one, whether it has one or not.
const MARK: usize = 1;

/// The column a pane's label starts at, and the branch under it: past the
/// indent, the mark and the icon, and a space after each of those two.
const LABEL: usize = 3 + MARK + 1 + icons::WIDTH + 1;

/// How long a command runs before its row says it runs, and says how it
/// ended once it does: an `ls` is no news.
const LONG: Duration = Duration::from_secs(1);

/// What the sidebar says as the menu cannot float over zellij for the pane
/// zellij is on, as it does not know which that is: it has yet to hear.
const NO_HERE_PANE: &str = "menu: luneta does not know yet which pane zellij is on; try again in a \
                            moment";
/// luneta cannot tell its zellij's client from the others of the session.
const UNSURE_PANE: &str = "menu: luneta cannot tell yet which of the session's clients is its own; \
                           go to a pane from the sidebar";
/// zellij has the focus on a plugin's pane.
const PLUGIN_PANE: &str = "menu: zellij is on a plugin's pane; the menu opens over a terminal's";
/// The daemon lists more than one session of the name zellij is in.
const TWIN_SESSIONS: &str = "menu: lunetad lists more than one session of the name zellij is in, \
                             and luneta cannot tell which it is";

/// A session as the sidebar tells it from another of its name: zellij
/// versions with sockets of their own can each run one under it. Its start
/// and its socket tell it from every other.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed {
    name: SessionName,
    /// The session, as the news keeps it, which says when it started.
    kept: ledger::Session,
}

impl Listed {
    /// `session`, or none for one with no start or no socket, as a daemon of
    /// another build's lists, or with a start no file of the news can be
    /// named for, before 1970, or with a name zellij refuses, which luneta
    /// can neither go to nor fold.
    fn of(session: &Session) -> Option<Self> {
        let name = SessionName::running(&session.name).ok()?;
        let started = SystemTime::try_from(session.started_at?).ok()?;
        let kept = ledger::Session::new(started, NonZeroU64::new(session.socket)?)?;
        Some(Listed { name, kept })
    }

    /// Its pane `pane`, as the news keeps it.
    fn key(&self, pane: u32) -> Key {
        Key { session: self.kept, pane }
    }

    /// A session of the tests', whose socket is the first there is.
    #[cfg(test)]
    pub fn new(name: &str, started: SystemTime) -> Self {
        let kept = ledger::Session::new(started, NonZeroU64::MIN).unwrap();
        Listed { name: SessionName::parse(name).unwrap(), kept }
    }
}

/// The session luneta sent its zellij to itself, which stands for the one
/// zellij is in until zellij titles luneta's terminal once it is there.
/// zellij sends a title only as it changes, in a tab, whichever client it
/// goes to: a client that comes to a tab whose title went to another, or to
/// it before, gets none, and its terminal keeps the last session's.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    session: String,
    /// The session the client leaves for this one, which it may be in still
    /// until it is there. None for one that starts there, as luneta's first
    /// does, and one that takes the place of a client that ended.
    from: Option<Leaving>,
    /// Whether the client is there. Until it is, a title of another session
    /// is of the one it is leaving.
    landed: bool,
    /// The number of the look that finds the client there, by which what
    /// comes back of a go luneta sent it on before is told from this one's.
    look: Option<u64>,
}

/// The session luneta's zellij leaves for the one it was sent to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Leaving {
    session: String,
    /// Whether it ends once the client has left it, as where the menu
    /// deletes the session luneta's zellij is in. The sidebar says so for as
    /// long as the go lasts, and no longer: zellij not taking the client
    /// there, or the client ending on its way, ends the go.
    ends: bool,
}

/// What a row stands for, by which the highlight keeps to it as rows come and
/// go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Session(Listed),
    Pane { session: Listed, pane: u32 },
}

impl Place {
    /// The name of its session. zellij's CLI goes to a session by its name
    /// alone, and to its own version's of two that share one.
    pub fn session(&self) -> &str {
        self.listed().name.as_str()
    }

    pub fn pane(&self) -> Option<u32> {
        match self {
            Place::Session(_) => None,
            Place::Pane { pane, .. } => Some(*pane),
        }
    }

    fn listed(&self) -> &Listed {
        match self {
            Place::Session(session) | Place::Pane { session, .. } => session,
        }
    }
}

/// What goes on in a pane, as its row marks it: what its agent is doing, or
/// the command its shell runs, or ran last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mark {
    /// An agent that waits for you, or a command that waits for you to type.
    Waiting,
    /// An agent done, which you have seen since it finished, or see: it is
    /// in the pane luneta's zellij is on.
    Idle,
    /// An agent that finished since you were last in its pane.
    Finished,
    /// An agent at work, or a command.
    Busy,
    Shell,
    /// A word the daemon passed on as Claude Code wrote it, by its initial as
    /// a capital, which takes the one column a mark does.
    Other(char),
    /// An agent that says nothing, or nothing a mark can show in its column.
    Unreported,
    /// A command that ended with an exit status of 0.
    Passed,
    /// A command that ended with any other.
    Failed,
    /// A command that ended, its shell not saying how, as fish does not after
    /// `fg`.
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

    /// Whether it says that something finished since you were last in its
    /// pane: an agent, or a command, which shows how it ended until then.
    fn is_news(&self) -> bool {
        matches!(self, Mark::Finished | Mark::Passed | Mark::Failed | Mark::Ran)
    }

    /// Its glyph, which the spinner shows at `frame`, and its style.
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

/// The initial of `word` as a capital, if it takes one column: the first
/// letter of a capital of two, as `ß`'s `SS`. A letter two wide, or a mark
/// that sits on the letter before it, would put the label off its column.
fn initial(word: &str) -> Option<char> {
    let initial = word.chars().next()?.to_uppercase().next()?;
    (initial.width() == Some(MARK)).then_some(initial)
}

/// What a pane's row marks, as it shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marked {
    mark: Mark,
    /// Since when it has been doing what it does, or since it ended.
    since: Option<SystemTime>,
}

impl Marked {
    fn of(agent: &Agent) -> Self {
        let since = agent.status_since.and_then(|at| at.try_into().ok());
        Marked { mark: Mark::of(agent), since }
    }

    /// The mark of the command `shell` runs, or ran last, at `now`, and the
    /// command. One that runs has the spinner, or the dot of an agent that
    /// waits for you while it waits for you to type, once it has run for
    /// `LONG`; and one that ended says how, if it ran that long. One that takes
    /// the terminal over has none while you use it, nor has a job stopped.
    fn of_shell(shell: &Shell, now: SystemTime) -> Option<(Self, &str)> {
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
                (mark, since, going.command.as_str())
            },
            Doing::Ended(ended) => {
                if Duration::try_from(ended.took?).ok()? < LONG {
                    return None;
                }
                let mark = match ended.status {
                    Some(0) => Mark::Passed,
                    Some(_) => Mark::Failed,
                    None => Mark::Ran,
                };
                (mark, told(ended.at)?, ended.command.as_str())
            },
            Doing::Stopped(_) => return None,
        };
        Some((Marked { mark, since: Some(since) }, command))
    }

    /// When the command it marks ended, if it did.
    fn ended(&self) -> Option<SystemTime> {
        match self.mark {
            Mark::Passed | Mark::Failed | Mark::Ran => self.since,
            _ => None,
        }
    }
}

/// What a pane's row shows of what goes on in it.
struct Shows<'a> {
    /// What its agent is doing, or else the command its shell runs, or ran.
    marked: Option<Marked>,
    /// That command, by which its row is named.
    ran: Option<&'a str>,
    /// Whether you marked it unread.
    unread: bool,
}

impl Shows<'_> {
    /// Whether its row has the dot.
    fn dotted(&self) -> bool {
        dotted(self.unread, self.marked.as_ref())
    }
}

/// Whether a pane's row has the dot: something finished in it since you were
/// last there, as its mark says, or you marked it unread.
fn dotted(unread: bool, marked: Option<&Marked>) -> bool {
    unread || marked.is_some_and(|marked| marked.mark.is_news())
}

/// The dot of `NEW`, and its style.
fn dot() -> Span<'static> {
    Span::styled(NEW, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
}

/// A moment the shell hook said, which it counts in whole seconds: the last
/// moment of its second, so that a command never ran longer than it did.
fn told(at: Option<impl TryInto<SystemTime>>) -> Option<SystemTime> {
    at?.try_into().ok()?.checked_add(Duration::from_millis(999))
}

/// When the command going in `shell` will have run for `LONG`.
fn long_at(shell: &Shell) -> Option<SystemTime> {
    match shell.doing.as_ref()? {
        Doing::Going(going) => told(going.since)?.checked_add(LONG),
        Doing::Ended(_) | Doing::Stopped(_) => None,
    }
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    Session {
        session: Listed,
        under: Under,
        /// Whether luneta's zellij is in it.
        here: bool,
        /// How many of its agents wait for you, whether one of its panes has
        /// the dot, and whether one is at work.
        waiting: usize,
        dot: bool,
        busy: bool,
    },
    Pane {
        session: Listed,
        pane: u32,
        /// Whether luneta's zellij is on it.
        here: bool,
        /// Whether you marked it unread.
        unread: bool,
        marked: Option<Marked>,
        /// The icon of what runs in it, if that has one.
        icon: Option<&'static str>,
        label: String,
        /// The branch checked out where it works, if one is.
        checkout: Option<Checkout>,
        /// Where it sits among the panes listed of its tab, if it shares it,
        /// and the colour of that tab's bracket.
        bracket: Option<(Bracket, Color)>,
    },
}

/// Where a pane's row sits among the rows of the panes that share its tab,
/// which a bracket down the gutter joins: the first, the last, or one between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bracket {
    First,
    Inner,
    Last,
}

impl Bracket {
    /// Where the pane `at` of the `count` listed of its tab sits among them:
    /// none for a pane alone in its tab.
    fn of(at: usize, count: usize) -> Option<Bracket> {
        match at {
            _ if count < 2 => None,
            0 => Some(Bracket::First),
            _ if at + 1 == count => Some(Bracket::Last),
            _ => Some(Bracket::Inner),
        }
    }
}

/// The branch checked out where a pane works, as its row shows it under its
/// label.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Checkout {
    branch: String,
    /// How its work tree stands, as starship shows it.
    status: String,
    /// The number of its pull request, and the colour of its state, if it
    /// has one to show.
    pull_request: Option<(u32, Color)>,
}

impl Row {
    fn place(&self) -> Place {
        match self {
            Row::Session { session, .. } => Place::Session(session.clone()),
            Row::Pane { session, pane, .. } => {
                Place::Pane { session: session.clone(), pane: *pane }
            },
        }
    }

    /// How many lines it takes: a pane in a repository has its branch under it.
    fn height(&self) -> usize {
        match self {
            Row::Pane { checkout: Some(_), .. } => 2,
            _ => 1,
        }
    }

    /// Whether the highlight stops on it: a pane, or a session that stands in
    /// for its panes, folded or with none to list. A session's row heads its
    /// panes, and is no place to go.
    fn stops(&self) -> bool {
        match self {
            Row::Session { under, .. } => *under != Under::Panes,
            Row::Pane { .. } => true,
        }
    }
}

/// What a session's row has under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Under {
    /// Its panes, or the ones of them a search finds.
    Panes,
    /// None, as it is folded.
    Folded,
    /// None, as zellij has listed none.
    Empty,
}

/// What the sidebar asks of luneta.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// Nothing: draw again, at most.
    Nothing,
    /// Take zellij's client there.
    Go(Place),
    /// Take the focus from zellij, as a click on the sidebar does.
    Take,
    /// Give the focus back to zellij.
    Leave,
    /// Ask zellij what closing the pane `pane` of `session` closes, for the
    /// menu numbered `menu` to plan its delete by.
    Closes { menu: u64, session: String, pane: u32 },
    /// Have zellij do what a delete key planned.
    Delete(Act),
    /// Have zellij open this, for the menu numbered `menu`, which takes its
    /// client there once zellij says what it opened.
    Open { menu: u64, open: Open },
    /// Take zellij's client to `place` first, and have zellij open this once
    /// it is there: a tab of another session, which may have no client to
    /// make room for it.
    OpenThere { menu: u64, place: Place, open: Open },
    /// Take zellij's client to a session it makes, that starts in `cwd`.
    Start { session: SessionName, cwd: String },
    /// Find the skills that `cwd` can run, for the menu numbered `menu`.
    Find { menu: u64, cwd: String },
    /// Have the model write this brief, and show it.
    Brief(Brief),
    /// Open the ask box, for this row.
    Ask(Target),
}

/// The rows the top of the sidebar takes, as `top` draws them.
const HEADER: usize = 3;

/// Where the keys go, as the sidebar sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Keys {
    /// To zellij: the highlight rests on its pane.
    #[default]
    Zellij,
    Sidebar,
    /// To a brief or the ask box the sidebar asked for, or a note over
    /// zellij of what went wrong, which gives them back as it closes: the
    /// sidebar stays as it stands, highlight and all.
    Lent,
    /// To the sidebar's menu, floating over zellij, until it closes: back to
    /// the sidebar where `lent`, which stays as it stands meanwhile, or else
    /// to zellij.
    Menu { lent: bool },
}

impl Keys {
    /// Whether the keys are the sidebar's, or come back to it.
    fn held(self) -> bool {
        match self {
            Keys::Sidebar | Keys::Lent | Keys::Menu { lent: true } => true,
            Keys::Zellij | Keys::Menu { lent: false } => false,
        }
    }
}

/// What the rule along the top counts, of the rows: the sessions, the agents
/// in their panes that wait for you, the panes with the dot, and the agents
/// that are at work and that are in a shell, a folded session's among them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    sessions: usize,
    waiting: usize,
    new: usize,
    busy: usize,
    shell: usize,
}

#[derive(Default)]
pub struct Sidebar {
    host: Option<Host>,
    /// Why the daemon does not answer, while it does not.
    away: Option<String>,
    /// The title zellij gives luneta's terminal: the name of the session it
    /// is in, and then ` | ` and the title of the pane it has the focus on,
    /// when that has one.
    title: Option<String>,
    /// The session luneta sent its zellij to, until zellij titles luneta's
    /// terminal after.
    sent: Option<Sent>,
    /// The sessions folded, by name.
    folds: Folds,
    /// What you saw in each pane, and the panes you marked unread.
    news: News,
    /// The order the rows go in, as you moved them.
    order: Order,
    /// The pane luneta's zellij is on, as the rows were last built.
    here_key: Option<Key>,
    /// When what finished last in each pane did, as the rows were last built.
    finished: HashMap<Key, SystemTime>,
    rows: Vec<Row>,
    counts: Counts,
    /// How many sessions the daemon lists with no start or no socket, which
    /// the rows leave out, as they tell sessions apart by both, or with a
    /// name zellij refuses.
    unlisted: usize,
    /// Which client of its session is luneta's zellij, and the pane it is on.
    own: OwnClient,
    keys: Keys,
    /// The row of the pane luneta's zellij is on, or of its session, while
    /// that one is folded. The highlight rests on it while the sidebar does
    /// not have the focus.
    here_row: Option<Place>,
    /// The row the keys took the highlight to, from `here_row` as the
    /// sidebar took the focus, or a click to.
    cursor: Option<Place>,
    /// Whether the key before was `Top::First`'s, which moves the highlight
    /// to the first row once pressed again. Any other key drops it, `Esc`
    /// doing nothing else, and so do a click, the wheel and the focus going.
    first_pressed: bool,
    /// What `v` and the left button do to the sessions' order.
    moving: Moving,
    /// The `here_row` the view last scrolled to show while the sidebar did
    /// not have the focus, after which the wheel can scroll it out of sight.
    revealed: Option<Place>,
    /// The first line of the rows on screen.
    top: usize,
    /// How many lines the rows had on screen when last drawn, under the rule
    /// along the top and over the notes at the bottom, which a click on
    /// either side of is not on.
    room: usize,
    /// The last thing that went wrong, until the next key.
    notice: Option<String>,
    /// What went wrong as the menu floated over zellij, for luneta to say
    /// there, where you look, and the sidebar collapsed has no room for.
    said: Option<String>,
    /// The search, while it is open.
    search: Option<Search>,
    /// The menu, while it is open.
    menu: Option<Menu>,
    /// How many menus the sidebar has opened. A menu takes the next number
    /// as it opens, which what zellij answers for it carries.
    menus: u64,
    /// What a delete took away, and the row the highlight goes to once it is
    /// gone.
    closing: Option<Closing>,
    /// When the sidebar was last drawn: the rows are built for then, by which
    /// a command has run long enough to show, or has yet to.
    drawn: Option<SystemTime>,
    /// The soonest a command that runs, and has yet to show, will have run
    /// long enough to: the rows are built again once it has.
    due: Option<SystemTime>,
}

/// What a delete takes away: the row the highlight was on, which goes, and
/// the row it goes to once that is gone, the first after it that stays.
#[derive(Debug)]
struct Closing {
    gone: Place,
    after: Option<Place>,
}

/// What `v` and the left button do to the sessions' order: one thing at a
/// time, so that no selection lasts under a session the mouse lifted.
#[derive(Debug, Default)]
enum Moving {
    #[default]
    Still,
    /// `v` selected the session `anchor`: the selection holds it, the
    /// highlight's session, and those between, each with its panes.
    Selected(Listed),
    /// The left button is down on the sidebar at `at`, on luneta's terminal:
    /// a click once it lets go on the line it went down on. On a session's
    /// row, `session` says which, and goes with the mouse once that moves a
    /// line. A pane's row goes nowhere.
    Down { at: Position, session: Option<Listed> },
    Lifted(Lift),
}

/// A session the mouse lifted by its row, which goes where the mouse takes
/// it, with its panes, once it lets go.
#[derive(Debug)]
struct Lift {
    session: Listed,
    /// The line of the sidebar the mouse is on.
    line: u16,
    /// Whether the mouse is over the sidebar. Let go anywhere else, the row
    /// goes back.
    over: bool,
    /// When the rows last scrolled under it, while it is held on the first
    /// line of them, or the last.
    scrolled: Option<Instant>,
}

/// The search, while it is open.
enum Search {
    /// Nothing typed: the rows under it are the rows without it.
    Empty,
    /// What is typed narrows the rows, and `seek` until the highlight has gone
    /// to the row that matches it best.
    Typed { term: String, seek: bool },
}

/// Where the highlight is, as the sidebar is drawn, by the index of its row.
#[derive(Clone, Copy)]
enum Highlight {
    /// On `LIT`, where the keys took it, while the sidebar has the focus.
    /// None while there is no row to take it to.
    Lit(Option<usize>),
    /// On `RESTING`, on the row of the pane luneta's zellij is on, while the
    /// sidebar does not have the focus.
    Resting(usize),
    /// Nowhere: the sidebar does not have the focus, and no row is the pane's.
    Off,
}

impl Sidebar {
    /// The sidebar, with the sessions `folds` has folded, what `news` has of
    /// what you saw and marked, and the rows in `order`.
    pub fn new(folds: Folds, news: News, order: Order) -> Self {
        Sidebar { folds, news, order, ..Sidebar::default() }
    }

    /// Takes up the folds as they are kept now, as another luneta left them.
    pub fn refold(&mut self) {
        if self.folds.reload() {
            self.rebuild();
        }
    }

    /// Takes up what you saw and marked as it is kept now, as another luneta
    /// left it.
    pub fn reread(&mut self) {
        if self.news.reload() {
            self.rebuild();
        }
    }

    /// Takes up the order of the rows as it is kept now, as another luneta
    /// left it.
    pub fn reorder(&mut self) {
        if self.order.reload() {
            self.rebuild();
        }
    }

    /// The daemon says the host stands as `host`. A look to ask zellij for,
    /// as luneta's zellij comes to a session.
    pub fn heard(&mut self, host: Host) -> Option<Look> {
        self.host = Some(host);
        self.away = None;
        let look = self.arrived();
        if look.is_none()
            && let Some(session) = self.own.session()
        {
            let listed = clients(self.host.as_ref(), session).map(<[Client]>::to_vec);
            self.own.heard(listed.as_deref(), Instant::now());
        }
        self.rebuild();
        look
    }

    /// Time went by, to `now` by the host's clock and `at` by luneta's own,
    /// with luneta's window `watched`, as it has the focus. What luneta saw
    /// of its zellij's client may have stood long enough against the
    /// daemon's list for that list to be the newer. The client may have
    /// stayed on its pane long enough for you to have been there, whether the
    /// sidebar shows or not: it has been on the pane, and the window has had
    /// the focus, since the first tick that found both. And the daemon may
    /// have left a session out of its lists for long enough for what was kept
    /// of it to go. And a session the mouse lifted may have been held on the
    /// first line of the rows, or the last, long enough for them to scroll.
    pub fn ticked(&mut self, now: SystemTime, at: Instant, watched: bool) {
        self.hold(at);
        if let Some(session) = self.own.session() {
            let listed = clients(self.host.as_ref(), session).map(<[Client]>::to_vec);
            let pane = self.own.pane();
            self.own.heard(listed.as_deref(), at);
            if self.own.pane() != pane {
                self.rebuild();
            }
        }
        let here = self.here_key.filter(|_| watched);
        let finished = &self.finished;
        if self.news.dwell(here, |key| finished.get(&key).copied(), now, at) {
            self.rebuild();
        }
        let live = self.live();
        self.news.listed(live.as_ref(), at);
    }

    /// The sessions that run, as the daemon lists them now, or none where its
    /// list may leave out one that does: it does not answer, and what it said
    /// last stands; or it could not read all of zellij's folder, or does not
    /// say whether it could, as one older than the count; or it lists one the
    /// news cannot tell from another.
    fn live(&self) -> Option<BTreeSet<ledger::Session>> {
        let host = self.host.as_ref()?;
        if self.away.is_some() || host.unread_sockets != Some(0) {
            return None;
        }
        host.sessions.iter().map(|session| Some(Listed::of(session)?.kept)).collect()
    }

    /// The daemon does not answer, for `why`. What it said last stands.
    pub fn lost(&mut self, why: String) {
        self.away = Some(why);
    }

    /// zellij titled luneta's terminal `title`. A look to ask zellij for, as
    /// its client comes to a session, which unfolds where zellij took it
    /// there itself.
    ///
    /// The title cannot tell apart two sessions whose names start alike
    /// before a ` | `: zellij's own switch to `odd | name`, which the daemon
    /// has yet to list, reads as one to `odd`, which unfolds instead.
    pub fn titled(&mut self, title: String) -> Option<Look> {
        if self.title.as_ref() == Some(&title) {
            return None;
        }
        // Of the session luneta sent its zellij to, which unfolded as it was
        // sent.
        let sent_there = self.sent.as_ref().is_some_and(|sent| in_session(&title, &sent.session));
        if sent_there || self.sent.as_ref().is_none_or(|sent| sent.landed) {
            self.sent = None;
        }
        self.title = Some(title);
        let look = self.arrived();
        if let Some(look) = &look
            && !sent_there
            && let Ok(session) = SessionName::running(&look.session)
        {
            self.unfold(&session);
        }
        self.rebuild();
        look
    }

    /// luneta sends its zellij to `session`, which it is in from now on, as
    /// far as the sidebar knows, and which unfolds: you go to a session for
    /// its panes. A look for zellij to answer once it is there.
    pub fn sending(&mut self, session: &SessionName) -> Option<Look> {
        self.send(session, false)
    }

    /// luneta sends its zellij to `session`, as `sending` says, and the
    /// session it leaves `ends` once it has left it.
    fn send(&mut self, session: &SessionName, ends: bool) -> Option<Look> {
        self.unfold(session);
        let session = session.as_str();
        let from = self.here().filter(|here| *here != session);
        let from = from.map(|here| Leaving { session: here.to_owned(), ends });
        self.sent = Some(Sent { session: session.to_owned(), from, landed: false, look: None });
        let look = self.arrived();
        if let Some(sent) = &mut self.sent {
            sent.look = look.as_ref().map(|look| look.number);
        }
        self.rebuild();
        look
    }

    /// A client of `session` takes the place of luneta's zellij's, which
    /// ended: it is in that session or none, which unfolds. A look for zellij
    /// to answer once it is there. luneta has yet to find it there, in the
    /// session it followed the one before to as well, as one on its way that
    /// ended.
    pub fn rejoining(&mut self, session: &SessionName) -> Look {
        self.unfold(session);
        let session = session.as_str();
        let look = self.came(session);
        let number = Some(look.number);
        self.sent = Some(Sent { session: session.to_owned(), from: None, landed: false, look: number });
        self.rebuild();
        look
    }

    /// The session luneta sent its zellij to, while zellij has yet to take
    /// its client there.
    pub fn going(&self) -> Option<&str> {
        let sent = self.sent.as_ref().filter(|sent| !sent.landed)?;
        Some(&sent.session)
    }

    /// zellij did not take luneta's zellij to the session it was sent to on
    /// the go that `look` was to find it there. A look at the one it is in
    /// still.
    pub fn stayed(&mut self, look: u64) -> Option<Look> {
        if self.sent.as_ref().is_none_or(|sent| sent.landed || sent.look != Some(look)) {
            return None;
        }
        self.sent = None;
        let look = self.arrived();
        self.rebuild();
        look
    }

    /// zellij answered a look so. One that says what it listed before
    /// luneta's zellij came, of the go luneta sent it on last, is from once
    /// it had.
    pub fn looked(&mut self, looked: Looked) {
        if let Some(sent) = &mut self.sent
            && looked.before.is_some()
            && sent.look == Some(looked.number)
        {
            sent.landed = true;
        }
        let listed = self.own.session().and_then(|session| clients(self.host.as_ref(), session));
        let listed = listed.map(<[Client]>::to_vec);
        self.own.looked(looked, listed.as_deref(), Instant::now());
        self.rebuild();
    }

    /// luneta takes its zellij to `place`. A look to ask zellij for once it
    /// has: in the session it is in, or the one it goes to.
    pub fn went(&mut self, place: &Place) -> Option<Look> {
        self.goes(place, false)
    }

    /// luneta takes its zellij out of the session it is in, to `place` in
    /// another, and kills that session once it has left it, which the
    /// sidebar says until then. A look to ask zellij for once it is there.
    pub fn leaving(&mut self, place: &Place) -> Option<Look> {
        self.goes(place, true)
    }

    /// luneta takes its zellij to `place`, as `went` says, and the session
    /// it leaves for another `ends` once it has left it.
    fn goes(&mut self, place: &Place, ends: bool) -> Option<Look> {
        let listed = clients(self.host.as_ref(), place.session()).map(<[Client]>::to_vec);
        let now = Instant::now();
        let look = self.own.went(place.session(), place.pane(), listed.as_deref(), now);
        if self.here() == Some(place.session()) {
            // A search goes to the panes of a folded session too.
            if place.pane().is_some() {
                self.unfold(&place.listed().name);
            }
            self.rebuild();
            return look;
        }
        // The client is on the pane as it comes to the other session.
        self.send(&place.listed().name, ends)
    }

    /// The sidebar takes the keys, or gives them back. The keys take the
    /// highlight on from where luneta's zellij is, and it goes back there as
    /// they leave. A search lasts while the sidebar has them: one left open
    /// would keep the rows it hides out of sight. So do the menu, a selection
    /// and a session lifted, which goes back. A brief or the menu over zellij
    /// lent the keys leaves all of it as it stands. The menu over zellij
    /// lasts while its box has the keys, and closes as the box gives them up.
    pub fn focus(&mut self, keys: Keys) {
        if self.floats() && !matches!(keys, Keys::Menu { .. }) {
            self.close_menu();
            // What luneta did not take to say over zellij goes to the notes.
            if let Some(said) = self.said.take() {
                self.notice = Some(said);
            }
        }
        let (had, holds) = (self.keys.held(), keys.held());
        self.keys = keys;
        if had == holds {
            return;
        }
        self.first_pressed = false;
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
        self.menu = None;
    }

    /// A look for the session zellij's client is in, as `here` says, if it
    /// has come to it since luneta last followed it.
    fn arrived(&mut self) -> Option<Look> {
        let here = self.here()?.to_owned();
        if self.own.session() == Some(here.as_str()) {
            return None;
        }
        Some(self.came(&here))
    }

    /// luneta's zellij came to `session`, or is back in it, as zellij did not
    /// take it out. A look for zellij to answer once it is there.
    fn came(&mut self, session: &str) -> Look {
        let listed = clients(self.host.as_ref(), session).map(<[Client]>::to_vec);
        self.own.came(session, listed.as_deref(), Instant::now())
    }

    /// Unfolds `session`, as luneta's zellij goes to it. It goes from the
    /// folder whether this luneta has it folded or not: another may have
    /// folded it since this one last heard.
    fn unfold(&mut self, session: &SessionName) {
        self.folds.set(session, false);
    }

    /// The session zellij's client is in: the one luneta sent it to, until
    /// zellij titles luneta's terminal after, or else by its title: the
    /// longest name of a session the daemon knows that the title is, or
    /// starts with before a ` | `, as a session's name can hold one too.
    /// Until the daemon says, what comes before the first.
    pub fn here(&self) -> Option<&str> {
        if let Some(sent) = &self.sent {
            return Some(&sent.session);
        }
        let title = self.title.as_deref()?;
        let named = |name: &&str| in_session(title, name);
        let sessions = self.host.iter().flat_map(|host| &host.sessions);
        sessions
            .map(|session| session.name.as_str())
            .filter(named)
            .max_by_key(|name| name.len())
            .or_else(|| Some(title.split_once(" | ").map_or(title, |(session, _)| session)))
    }

    /// The pane luneta's zellij is on, by its session's name now, while it
    /// is in a session, on its way to none, and on a pane, not a plugin's.
    pub fn here_pane(&self) -> Option<zellij::Pane> {
        if self.going().is_some() {
            return None;
        }
        let session = self.here()?;
        let pane = self.own.pane().filter(|_| self.own.session() == Some(session))?;
        Some(zellij::Pane { session: session.to_owned(), pane })
    }

    /// The sessions zellij's client may be in: the one luneta sent it to,
    /// and the one it left, until it is there; or else the one it is in, as
    /// `here` says.
    pub fn seats(&self) -> Vec<&str> {
        match &self.sent {
            Some(Sent { session, from, landed: false, .. }) => {
                from.iter().map(|from| &from.session).chain([session]).map(String::as_str).collect()
            },
            _ => self.here().into_iter().collect(),
        }
    }

    /// The pane luneta's zellij is on, in the one session of the name it is
    /// in: both as luneta last followed its client, so that the pane is one
    /// of that session's, whose ids another session has too. Or else why the
    /// sidebar does not know it, as the menu says.
    fn here_place(&self) -> Result<Place, &'static str> {
        let (here, pane) = self.own.place().map_err(|unplaced| match unplaced {
            Unplaced::Waiting => NO_HERE_PANE,
            Unplaced::Unsure => UNSURE_PANE,
            Unplaced::Plugin => PLUGIN_PANE,
        })?;
        let host = self.host.as_ref().ok_or(NO_HERE_PANE)?;
        let (sessions, _) = oldest_first(&host.sessions);
        let mut named = sessions.into_iter().filter(|(listed, _)| listed.name.as_str() == here);
        match (named.next(), named.next()) {
            (Some((listed, _)), None) => Ok(Place::Pane { session: listed, pane }),
            (None, _) => Err(NO_HERE_PANE),
            (Some(_), Some(_)) => Err(TWIN_SESSIONS),
        }
    }

    /// What the picker lists, as the sidebar knows the host now: every
    /// session newest first, each with its panes in the order of their tabs,
    /// and the agents in them, those of the session luneta's zellij is in
    /// first, then those of the others newest first, each session's in the
    /// order their panes opened.
    pub fn listing(&self) -> Listing {
        let here = self.here().map(str::to_owned);
        let mut listing = Listing {
            here_pane: self.here_place().ok(),
            away: self.away.clone(),
            ..Listing::default()
        };
        let Some(host) = &self.host else {
            listing.here = here;
            return listing;
        };
        let seated = seated(&host.agents, &host.sessions);
        let here_key =
            listing.here_pane.as_ref().and_then(|place| Some(place.listed().key(place.pane()?)));
        let (sessions, _) = oldest_first(&host.sessions);
        for (listed, session) in sessions.into_iter().rev() {
            let mut panes: Vec<&Pane> = session.panes.iter().flat_map(|panes| &panes.panes).collect();
            panes.sort_by_key(|pane| pane.id);
            let mut waiting = 0;
            for pane in &panes {
                let Some(agent) = seated.get(&(session.name.as_str(), pane.id)) else {
                    continue;
                };
                let key = listed.key(pane.id);
                let (marked, _) = agent_marked(&self.news, agent, key, here_key == Some(key));
                waiting += usize::from(marked.mark == Mark::Waiting);
                listing.agents.push(Seated {
                    place: Place::Pane { session: listed.clone(), pane: pane.id },
                    label: label(Some(agent), pane),
                    mark: marked.mark,
                    status: status_word(agent),
                    since: marked.since,
                    dir: short_cwd(agent_dir(agent, pane)),
                });
            }
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            let shown = |pane: &&Pane| Shown {
                id: pane.id,
                tab: pane.tab_name.clone(),
                label: label(seated.get(&(session.name.as_str(), pane.id)).copied(), pane),
            };
            let mut clients = session.clients.iter().flat_map(|clients| &clients.clients);
            listing.sessions.push(Live {
                started: listed.kept.started(),
                panes: panes.iter().map(shown).collect(),
                on: clients.find_map(|client| client.pane),
                waiting,
                place: Place::Session(listed),
            });
        }
        // Those of the session luneta's zellij is in first, and the rest as
        // they are.
        listing.agents.sort_by_key(|seated| here.as_deref() != Some(seated.place.session()));
        listing.here = here;
        listing
    }

    pub fn notify(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    /// Says what went wrong with what the menu did: in the notes, or over
    /// zellij while the menu floats there.
    fn say(&mut self, notice: String) {
        if self.floats() {
            self.said = Some(notice);
        } else {
            self.notice = Some(notice);
        }
    }

    /// What went wrong as the menu floated over zellij, for luneta to say
    /// there, once.
    pub fn said(&mut self) -> Option<String> {
        self.said.take()
    }

    /// Whether a spinner turns: the rule along the top has one while an agent
    /// on the rows is at work, and so does the sidebar collapsed.
    pub fn turning(&self) -> bool {
        self.counts.busy > 0
    }

    /// Makes the rows again from what the daemon said last.
    ///
    /// While the search holds something typed, only the rows that match it
    /// are kept, in their order, and a session with them, unfolded. A session
    /// found by its name keeps all its panes. The highlight goes to the best
    /// match as what is typed changes, and stays on its row otherwise. A
    /// session that matches best by its name has it on the pane of it that
    /// matches best, or else on its first.
    fn rebuild(&mut self) {
        let here_pane = self.here_place().ok();
        let Some(host) = &self.host else {
            return;
        };
        let here = self.here().map(str::to_owned);
        let now = self.drawn.unwrap_or(SystemTime::UNIX_EPOCH);
        let mut due: Option<SystemTime> = None;
        let agents = seated(&host.agents, &host.sessions);
        let trees: HashMap<&str, &lunetad::api::WorkTree> =
            host.work_trees.iter().map(|tree| (tree.dir.as_str(), tree)).collect();
        let pull_requests: HashMap<&str, PullRequest> = host
            .pull_requests
            .iter()
            .filter_map(|it| Some((it.dir.as_str(), PullRequest::of_api(it)?)))
            .collect();
        let (mut sessions, unlisted) = oldest_first(&host.sessions);
        self.order.sessions(&mut sessions, |(listed, _)| listed.name.as_str());
        let (term, seek) = match &mut self.search {
            Some(Search::Typed { term, seek }) => (Some(term.as_str()), std::mem::take(seek)),
            Some(Search::Empty) | None => (None, false),
        };
        let finder = term.map(Finder::new);
        let here_key =
            here_pane.as_ref().and_then(|place| Some(place.listed().key(place.pane()?)));
        let mut finished: HashMap<Key, SystemTime> = HashMap::new();
        let mut counts = Counts { sessions: sessions.len(), ..Counts::default() };
        let mut rows = Vec::new();
        let mut best: Option<(usize, Fit)> = None;
        let mut weigh = |at: usize, fit: Fit| {
            if best.is_none_or(|(_, top)| fit > top) {
                best = Some((at, fit));
            }
        };
        for (listed, session) in sessions {
            let mut panes: Vec<_> = session.panes.iter().flat_map(|panes| &panes.panes).collect();
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            let agent = |pane: u32| agents.get(&(session.name.as_str(), pane)).copied();
            // A pane's agent, or else the command its shell runs, by which
            // its row is named, and whether its row has the dot. What
            // finished in the pane luneta's zellij is on is no news: you see
            // it. A command that ended shows until you have seen it end.
            let mut by_pane: Vec<Shows> = Vec::with_capacity(panes.len());
            for pane in &panes {
                let key = listed.key(pane.id);
                let here = here_key == Some(key);
                let unread = self.news.is_unread(key);
                if let Some(agent) = agent(pane.id) {
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
                by_pane.push(Shows { marked, ran, unread });
            }
            let marks = || by_pane.iter().filter_map(|shows| Some(&shows.marked.as_ref()?.mark));
            let waiting = marks().filter(|mark| **mark == Mark::Waiting).count();
            let busy = marks().filter(|mark| **mark == Mark::Busy).count();
            let dots = by_pane.iter().filter(|shows| shows.dotted()).count();
            counts.waiting += waiting;
            counts.new += dots;
            counts.busy += busy;
            counts.shell += marks().filter(|mark| **mark == Mark::Shell).count();
            let folded = self.folds.contains(listed.name.as_str());
            let named = finder.as_ref().and_then(|finder| finder.session(listed.name.as_str()));
            // A search looks inside a folded session too.
            let listing = if finder.is_some() || !folded { panes } else { Vec::new() };
            // The rows of the panes listed, by their tab, its position and its
            // id, each made once its bracket is known: it joins the panes
            // listed of a tab, as a search can leave some of them out.
            let mut shown: BTreeMap<(u32, Option<u32>), Vec<_>> = BTreeMap::new();
            for (pane, Shows { marked, ran, unread }) in listing.into_iter().zip(by_pane) {
                let cwd = pane.cwd.as_deref();
                let checkout = cwd.and_then(|cwd| {
                    let tree = WorkTree::of_api(trees.get(cwd)?)?;
                    checkout(&tree, pull_requests.get(cwd).copied())
                });
                let command = pane.command.as_deref();
                // An agent's pane runs Claude Code, whatever its terminal has
                // in front. It goes by the agent, as the label does, and not
                // by the mark: a mark need not be an agent's.
                let program = match agent(pane.id) {
                    Some(_) => Some(icons::AGENT),
                    None => command.and_then(icons::program),
                };
                let label = match ran {
                    Some(ran) => ran.to_owned(),
                    None => label(agent(pane.id), pane),
                };
                let branch = checkout.as_ref().map(|checkout| checkout.branch.as_str());
                // An agent's pane is found by where the agent works, as its
                // row and the picker say it.
                let dir = agent(pane.id).map_or(cwd, |agent| Some(agent_dir(agent, pane)));
                let tab = &pane.tab_name;
                let found =
                    finder.as_ref().and_then(|finder| finder.pane(&label, branch, tab, dir));
                if finder.is_some() && found.is_none() && named.is_none() {
                    continue;
                }
                let place = Place::Pane { session: listed.clone(), pane: pane.id };
                let here = here_pane.as_ref() == Some(&place);
                let icon = program.and_then(icons::of);
                let session = listed.clone();
                let row = move |bracket| Row::Pane {
                    session,
                    pane: pane.id,
                    here,
                    unread,
                    marked,
                    icon,
                    label,
                    checkout,
                    bracket,
                };
                shown.entry((pane.tab, pane.tab_id)).or_default().push((row, found));
            }
            if finder.is_some() && named.is_none() && shown.is_empty() {
                continue;
            }
            let under = if !shown.is_empty() {
                Under::Panes
            } else if folded {
                Under::Folded
            } else {
                Under::Empty
            };
            let head = rows.len();
            rows.push(Row::Session {
                session: listed.clone(),
                under,
                here: here.as_deref() == Some(listed.name.as_str()),
                waiting,
                dot: dots > 0,
                busy: busy > 0,
            });
            // The pane of the session that matches best, which a session found
            // by its name hands the highlight on to. With none, its row hands it
            // on to its first pane.
            let mut lead: Option<(usize, Fit)> = None;
            for ((_, id), of_tab) in shown {
                let count = of_tab.len();
                // A daemon older than `tab_id` does not say which tab it is:
                // its bracket stays grey, as every bracket was.
                let colour = id.map_or(gruvbox::GREY, |id| TABS[id as usize % TABS.len()]);
                for (at, (row, found)) in of_tab.into_iter().enumerate() {
                    if let Some(found) = found {
                        weigh(rows.len(), found);
                        if lead.is_none_or(|(_, top)| found > top) {
                            lead = Some((rows.len(), found));
                        }
                    }
                    rows.push(row(Bracket::of(at, count).map(|sits| (sits, colour))));
                }
            }
            if let Some(named) = named {
                weigh(lead.map_or(head, |(at, _)| at), named);
            }
        }
        self.rows = rows;
        self.counts = counts;
        self.unlisted = unlisted;
        self.here_key = here_key;
        self.finished = finished;
        self.here_row = here_pane.as_ref().and_then(|place| self.row_for(place));
        // A selection, and a session lifted, go with the session they
        // started on.
        if let Moving::Selected(session) | Moving::Lifted(Lift { session, .. }) = &self.moving
            && self.session_at(session).is_none()
        {
            self.moving = Moving::Still;
        }
        // The highlight goes past what a delete took away, once it is gone.
        if let Some(closing) = &self.closing
            && self.cursor.as_ref() == Some(&closing.gone)
            && self.rows.iter().all(|row| row.place() != closing.gone)
        {
            self.cursor.clone_from(&closing.after);
            self.closing = None;
        }
        self.due = due;
        self.settle(best.map(|(at, _)| at), seek);
    }

    /// The row that stands for `place`: its own, or its session's, once that
    /// is folded.
    fn row_for(&self, place: &Place) -> Option<Place> {
        let session = Place::Session(place.listed().clone());
        let listed = |place: &&Place| self.rows.iter().any(|row| row.place() == **place);
        [place, &session].into_iter().find(listed).cloned()
    }

    /// Puts the highlight back on a row it stops on: the one it was on, or
    /// else the one where that was, or else the row that matches the search
    /// `best`. It goes to that one first while it `seek`s it. A pane gone, or
    /// folded away, leaves the highlight on its session.
    fn settle(&mut self, best: Option<usize>, seek: bool) {
        let held = self.cursor.as_ref().and_then(|place| {
            let row = self.row_for(place)?;
            self.rows.iter().position(|it| it.place() == row)
        });
        let at = if seek { best.or(held) } else { held.or(best) };
        let at = at.unwrap_or(0);
        let stop = (at..self.rows.len()).chain((0..at).rev()).find(|index| self.stops(*index));
        self.cursor = stop.map(|index| self.rows[index].place());
    }

    /// Whether the highlight stops on the row `at`: while a selection lasts,
    /// a session's, and otherwise one that `Row::stops`.
    fn stops(&self, at: usize) -> bool {
        let row = &self.rows[at];
        match self.moving {
            Moving::Selected(_) => matches!(row, Row::Session { .. }),
            Moving::Still | Moving::Down { .. } | Moving::Lifted(_) => row.stops(),
        }
    }

    fn cursor_index(&self) -> Option<usize> {
        let place = self.cursor.as_ref()?;
        // Each time the rows are built, `settle` puts the highlight on one of
        // them, and `step` and a click move it only onto one.
        let at = self.rows.iter().position(|row| row.place() == *place);
        Some(at.expect("the highlight is on a row"))
    }

    /// Moves the highlight `by` rows it stops on, down or up, and no further
    /// than the ends. It no longer waits for a delete to take its row away.
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
        self.cursor = Some(self.rows[stops[next]].place());
    }

    /// Moves the highlight to the last row it stops on, or the first. It no
    /// longer waits for a delete to take its row away.
    fn end(&mut self, last: bool) {
        self.closing = None;
        let mut stops = self.rows.iter().filter(|row| row.stops());
        let stop = if last { stops.next_back() } else { stops.next() };
        if let Some(stop) = stop {
            self.cursor = Some(stop.place());
        }
    }

    /// Folds the session of the highlighted row the other way.
    fn toggle(&mut self) {
        let Some(place) = &self.cursor else {
            return;
        };
        let folded = self.folds.contains(place.session());
        self.fold(!folded);
    }

    /// Folds the session of the highlighted row, or unfolds it. The highlight
    /// goes to the session's row as it folds.
    fn fold(&mut self, folded: bool) {
        let Some(place) = &self.cursor else {
            return;
        };
        let session = place.listed().clone();
        if !self.folds.set(&session.name, folded) {
            return;
        }
        if folded {
            self.cursor = Some(Place::Session(session));
        }
        self.rebuild();
    }

    /// The pane listed after the one luneta's zellij is on, and from the last
    /// back to the first: where `Alt Tab` goes. None where that pane is the
    /// only one listed. From a folded session, or a pane no row lists, as a
    /// plugin's or one luneta is not sure its zellij is on, it is the pane
    /// listed after the session's row, and from a session no row lists, the
    /// first.
    pub fn next_pane(&self) -> Option<Place> {
        let row = |place: &Place| self.rows.iter().position(|row| row.place() == *place);
        let here = self.here_row.as_ref().and_then(row);
        let start = here.or_else(|| {
            let session = self.here()?;
            self.rows.iter().position(|row| {
                let named = |listed: &Listed| listed.name.as_str() == session;
                matches!(row, Row::Session { session: listed, .. } if named(listed))
            })
        });
        let from = start.map_or(0, |at| at + 1);
        let count = self.rows.len();
        let next = (0..count)
            .map(|step| (from + step) % count)
            .find(|at| matches!(self.rows[*at], Row::Pane { .. }))?;
        (Some(next) != here).then(|| self.rows[next].place())
    }

    /// What a key does while the sidebar has the focus, pressed at `now`. The
    /// left button down on the sidebar takes every key, and `Esc` lets go of
    /// what it holds: a session lifted goes back, and a click is no longer
    /// one. The menu takes every key while it is open. Without it, a
    /// character does what `Top::of` says, which the legend goes by too, and
    /// one it has not is the menu's.
    pub fn key(&mut self, key: &KeyEvent, now: SystemTime) -> Asked {
        self.notice = None;
        let first_pressed = std::mem::take(&mut self.first_pressed);
        if self.pressed() {
            if key.code == KeyCode::Esc {
                self.let_go();
            }
            return Asked::Nothing;
        }
        if let Some(menu) = self.menu.take() {
            return self.menu_did(menu.key(key));
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return Asked::Nothing;
        }
        if let Some(term) = self.search() {
            let term = term.to_owned();
            return self.search_key(key, term);
        }
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
            // As in vim, it only takes the `g` back.
            KeyCode::Esc if first_pressed => {},
            KeyCode::Esc => return Asked::Leave,
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Last) => self.end(true),
                Some(Top::First) if first_pressed => self.end(false),
                Some(Top::First) => self.first_pressed = true,
                Some(Top::Fold(Some(folded))) => self.fold(folded),
                Some(Top::Fold(None)) => self.toggle(),
                Some(Top::Menu) => return self.open_menu(None),
                Some(Top::Command(Command::Search)) => self.find(String::new()),
                Some(Top::Command(Command::Unread)) => self.toggle_unread(now),
                Some(Top::Command(Command::Reorder)) => self.select(),
                // Without a selection, there is nothing to move.
                Some(Top::Shift(_)) => {},
                None => return self.open_menu(Some(c)),
            },
            _ => {},
        }
        Asked::Nothing
    }

    /// What a key does while a selection lasts: `j` and `k` move its end at
    /// the highlight, `J` and `K` move the sessions it holds, and `v` and
    /// `Esc` end it, `Esc` doing nothing else, as in vim. Any other key ends
    /// it too, and does what it does without one, for which it is None.
    fn selected_key(&mut self, key: &KeyEvent) -> Option<Asked> {
        match key.code {
            KeyCode::Down => self.step(1),
            KeyCode::Up => self.step(-1),
            KeyCode::Esc => self.unselect(),
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Shift(by)) => self.shift(by),
                Some(Top::Command(Command::Reorder)) => self.unselect(),
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

    /// Selects the session of the highlighted row, for `J` and `K` to move,
    /// with the sessions `j` and `k` grow the selection to, each with its
    /// panes.
    fn select(&mut self) {
        let Some(place) = &self.cursor else {
            return;
        };
        let session = place.listed().clone();
        self.cursor = Some(Place::Session(session.clone()));
        self.moving = Moving::Selected(session);
    }

    /// Ends the selection, the highlight on a row it stops on without one.
    fn unselect(&mut self) {
        if let Moving::Selected(_) = self.moving {
            self.moving = Moving::Still;
            self.settle(None, false);
        }
    }

    /// Moves the sessions the selection holds `by` places down, or up, and
    /// no further than the ends, among the rows as the order is kept now: a
    /// move another luneta made stays, though this one has yet to hear of
    /// it, or never will, as one whose watcher failed. They stay selected,
    /// for the next move.
    fn shift(&mut self, by: isize) {
        self.reorder();
        let Some((sessions, held)) = self.selected() else {
            return;
        };
        let to = held.start.saturating_add_signed(by).min(sessions.len() - held.len());
        if to != held.start {
            self.keep(&moved(&sessions, held, to));
        }
    }

    /// The sessions, each by the rows it spans, and which of them the
    /// selection holds, while it lasts.
    fn selected(&self) -> Option<(Vec<Range<usize>>, Range<usize>)> {
        let Moving::Selected(anchor) = &self.moving else {
            return None;
        };
        let sessions = self.sessions();
        let index = |at: usize| sessions.iter().position(|span| span.contains(&at));
        let (from, to) = (index(self.session_at(anchor)?)?, index(self.cursor_index()?)?);
        Some((sessions, from.min(to)..from.max(to) + 1))
    }

    /// The sessions, each by the rows it spans: its own and its panes'.
    /// Sessions of one name next to each other go as one, as the order goes
    /// by name, and cannot move one without the other.
    fn sessions(&self) -> Vec<Range<usize>> {
        let mut starts = Vec::new();
        let mut named: Option<&str> = None;
        for (at, row) in self.rows.iter().enumerate() {
            if let Row::Session { session, .. } = row
                && named.replace(session.name.as_str()) != Some(session.name.as_str())
            {
                starts.push(at);
            }
        }
        let ends = starts.iter().skip(1).copied().chain([self.rows.len()]);
        starts.iter().zip(ends).map(|(start, end)| *start..end).collect()
    }

    /// The row of `session`, while it has one.
    fn session_at(&self, session: &Listed) -> Option<usize> {
        let of = |row: &Row| matches!(row, Row::Session { session: it, .. } if it == session);
        self.rows.iter().position(of)
    }

    /// Keeps `sessions`, each by the rows it spans, in their order now, for
    /// every luneta, and builds the rows again in it.
    fn keep(&mut self, sessions: &[Range<usize>]) {
        let name = |span: &Range<usize>| self.rows[span.start].place().session().to_owned();
        self.order.keep_sessions(sessions.iter().map(name).collect());
        self.rebuild();
    }

    /// Where `lift` lands as things stand: the sessions, each by the rows it
    /// spans, in the order they take once the mouse lets go, and the rows the
    /// lifted one spans, with those of its name next to it, which go with it.
    /// It lands where its row is nearest to the line the mouse holds it on,
    /// and goes back while the mouse is off the sidebar.
    fn landing(&self, lift: &Lift) -> Option<(Vec<Range<usize>>, Range<usize>)> {
        let at = self.session_at(&lift.session)?;
        let sessions = self.sessions();
        let from = sessions.iter().position(|span| span.contains(&at))?;
        let lifted = sessions[from].clone();
        if !lift.over {
            return Some((sessions, lifted));
        }
        let lines = |span: Range<usize>| self.rows[span].iter().map(Row::height).sum::<usize>();
        // The line the lifted rows start on: the mouse holds the row of the
        // session it lifted, under any of its name it goes with.
        let above = HEADER + lines(lifted.start..at);
        let top = (self.top + usize::from(lift.line)) as isize - above as isize;
        // The line each place among the others starts on, with the lifted
        // rows out of them, and the line after the last.
        let mut line = 0;
        let mut gaps = Vec::with_capacity(sessions.len());
        for (index, span) in sessions.iter().enumerate() {
            if index != from {
                gaps.push(line);
                line += lines(span.clone());
            }
        }
        gaps.push(line);
        let distance = |gap: &usize| ((gaps[*gap] as isize - top).unsigned_abs(), *gap != from);
        let to = (0..gaps.len()).min_by_key(distance).unwrap_or(from);
        Some((moved(&sessions, from..from + 1, to), lifted))
    }

    /// Opens the menu for the highlighted row, on the group `key` opens, or on
    /// the first with no key. A key the menu has not opens nothing.
    fn open_menu(&mut self, key: Option<char>) -> Asked {
        let Some(target) = self.target() else {
            return Asked::Nothing;
        };
        self.menus += 1;
        let Some(did) = Menu::open(self.menus, target, key) else {
            return Asked::Nothing;
        };
        self.menu_did(did)
    }

    /// What the menu is for, as it opens on the highlighted row.
    fn target(&self) -> Option<Target> {
        let place = self.cursor.clone()?;
        self.rows.iter().any(|row| row.place() == place).then(|| self.target_at(place))
    }

    /// What the menu is for, as it opens on `place`: the place, the directory
    /// its pane works in, and the branch checked out there. A session has
    /// neither, having no pane.
    fn target_at(&self, place: Place) -> Target {
        let pane = place.pane().and_then(|pane| self.pane_of(place.listed(), pane));
        let cwd = pane.and_then(|pane| pane.cwd.clone());
        let trees = self.host.iter().flat_map(|host| &host.work_trees);
        let tree = cwd.as_deref().and_then(|cwd| trees.into_iter().find(|tree| tree.dir == cwd));
        let checkout = tree.and_then(|tree| checkout(&WorkTree::of_api(tree)?, None));
        Target { cwd, branch: checkout.map(|checkout| checkout.branch), place }
    }

    /// Opens the menu for the pane luneta's zellij is on, on its first group,
    /// in the place of one open at the bottom, to float over zellij once the
    /// keys are its own. None opens while the sidebar does not know which
    /// pane that is: why, for luneta to say over zellij. What went wrong
    /// before goes from the notes, as it does for any key the sidebar takes.
    pub fn float_menu(&mut self) -> Result<(), &'static str> {
        self.notice = None;
        let place = self.here_place()?;
        self.menus += 1;
        self.menu = Some(Menu::new(self.menus, self.target_at(place)));
        Ok(())
    }

    /// The menu, while it floats over zellij.
    pub fn floating(&self) -> Option<&Menu> {
        self.menu.as_ref().filter(|_| self.floats())
    }

    /// Whether the menu floats over zellij, rather than standing at the
    /// bottom of the sidebar: it does while the keys are its own there.
    fn floats(&self) -> bool {
        matches!(self.keys, Keys::Menu { .. })
    }

    /// The pane `id` of the session `listed`, as the daemon last said.
    fn pane_of(&self, listed: &Listed, id: u32) -> Option<&Pane> {
        let host = self.host.as_ref()?;
        let session = host.sessions.iter().find(|it| Listed::of(it).as_ref() == Some(listed))?;
        session.panes.iter().flat_map(|panes| &panes.panes).find(|pane| pane.id == id)
    }

    /// Keeps the menu a key left open, if any, does what the key left the
    /// sidebar to do, and asks luneta for what that takes.
    fn menu_did(&mut self, did: Did) -> Asked {
        let (menu, wants) = match did {
            Did::Stays(menu, wants) => (menu, wants),
            Did::Closed(done) => {
                self.menu = None;
                return match done {
                    Done::Nothing => Asked::Nothing,
                    Done::Wrong(reason) => {
                        self.say(reason);
                        Asked::Nothing
                    },
                    Done::Start(cwd) => self.start(&cwd),
                    Done::Act(act) => self.delete(act),
                    Done::Brief { scope, place } => self.brief(scope, &place),
                    Done::Ask(target) => Asked::Ask(target),
                };
            },
        };
        let (number, target) = (menu.number(), menu.target().clone());
        self.menu = Some(menu);
        let session = target.place.session().to_owned();
        match wants {
            Wants::Nothing => Asked::Nothing,
            Wants::Skills(cwd) => Asked::Find { menu: number, cwd },
            Wants::Open { cwd, opens: Opens::Pane { beside, command } } => {
                Asked::Open { menu: number, open: Open::Pane { session, beside, cwd, command } }
            },
            // A tab needs a client of its session to make room for it. The
            // session luneta's zellij is in has one, and another opens it once
            // luneta's zellij is there: its last client may have left since
            // lunetad listed them.
            Wants::Open { cwd, opens: Opens::Tab } => {
                let (name, command) = (None, Vec::new());
                let open = Open::Tab { session: session.clone(), cwd, name, command };
                if self.going().is_none() && self.here() == Some(session.as_str()) {
                    Asked::Open { menu: number, open }
                } else {
                    Asked::OpenThere { menu: number, place: target.place, open }
                }
            },
            Wants::Plan(Scope::Pane(pane)) => Asked::Closes { menu: number, session, pane },
            Wants::Plan(Scope::Session) => {
                let plan = self.plan(target.place.session(), Deletes::Session);
                self.planned(plan)
            },
        }
    }

    /// The menu planned what its delete key deletes so: what it asks of
    /// luneta.
    fn planned(&mut self, plan: Result<Plan, String>) -> Asked {
        let Some(menu) = self.menu.take() else {
            return Asked::Nothing;
        };
        self.menu_did(menu.planned(plan))
    }

    /// What deleting what `deletes` deletes of `session` does, as the
    /// sidebar knows the host now: for the menu, and for the picker.
    pub fn plan(&self, session: &str, deletes: Deletes) -> Result<Plan, String> {
        close::plan(&self.subject(session, deletes))
    }

    /// What the delete is for, as the sidebar knows it: `session`, where
    /// luneta's zellij is, and what it `deletes`.
    fn subject<'a>(&'a self, session: &'a str, deletes: Deletes) -> Subject<'a> {
        let seat = match (self.going(), self.here()) {
            (None, Some(here)) if here == session => Seat::In,
            (None, Some(_)) => Seat::Elsewhere,
            (Some(_), _) | (None, None) => Seat::Moving,
        };
        let mut agents = Err(Untold::Away);
        let mut refuge: Option<Listed> = None;
        if let Some(host) = &self.host {
            // What lunetad said last of its agents may be out of date while it
            // does not answer: one may have gone to work, or started since.
            if self.away.is_none() {
                agents = host.unread_seats().map_err(Untold::Unplaced).map(|unread| {
                    let seated = seated(&host.agents, &host.sessions);
                    let doing = seated.iter().filter(|((at, _), _)| *at == session);
                    let doing =
                        doing.map(|((_, pane), agent)| (*pane, close::Doing::from(*agent)));
                    doing.chain(close::unread_in(&unread, session)).collect()
                });
            }
            // The newest of the other sessions, which the picker lists first.
            let (sessions, _) = oldest_first(&host.sessions);
            let mut others = sessions.into_iter().map(|(listed, _)| listed);
            refuge = others.rfind(|listed| listed.name.as_str() != session);
        }
        Subject { session, seat, deletes, agents, refuge: refuge.map(Place::Session) }
    }

    /// Asks for a brief of what `scope` covers, by `place`, the row the menu
    /// was for: every session, in the rows' order, or the row's session, or
    /// the agent in the row's pane, and on a session's row, its session. An
    /// agent is told of as its row shows it, a folded session's too, by the
    /// name, the status and the branch on its row. With no agent there, the
    /// sidebar says so where the menu was.
    fn brief(&mut self, scope: brief::Scope, place: &Place) -> Asked {
        let Some(host) = &self.host else {
            return Asked::Nothing;
        };
        // In the order of their rows, whatever orders them, and any without
        // one after them.
        let (mut sessions, _) = oldest_first(&host.sessions);
        let row = |listed: &Listed| {
            let of = |row: &Row| matches!(row, Row::Session { session, .. } if session == listed);
            self.rows.iter().position(of).unwrap_or(usize::MAX)
        };
        sessions.sort_by_key(|(listed, _)| row(listed));
        let over = match (scope, place) {
            (brief::Scope::Every, _) => {
                Over::Sessions(sessions.iter().map(|(listed, _)| listed.name.to_string()).collect())
            },
            (brief::Scope::Highlighted, Place::Pane { session, pane }) => {
                Over::Agent(session.name.to_string(), *pane)
            },
            (brief::Scope::Session | brief::Scope::Highlighted, _) => {
                Over::Session(place.session().to_owned())
            },
        };
        let agents = seated(&host.agents, &host.sessions);
        let trees: HashMap<&str, &lunetad::api::WorkTree> =
            host.work_trees.iter().map(|tree| (tree.dir.as_str(), tree)).collect();
        let mut notes = Vec::new();
        for (_, session) in sessions {
            let mut panes: Vec<_> = session.panes.iter().flat_map(|panes| &panes.panes).collect();
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            for pane in panes {
                let Some(agent) = agents.get(&(session.name.as_str(), pane.id)) else {
                    continue;
                };
                let cwd = agent_dir(agent, pane);
                let branch = trees.get(cwd).and_then(|tree| {
                    checkout(&WorkTree::of_api(tree)?, None).map(|checkout| checkout.branch)
                });
                notes.push(Note {
                    session: session.name.clone(),
                    pane: pane.id,
                    name: label(Some(agent), pane),
                    status: status_word(agent),
                    since: agent.status_since.and_then(|at| at.try_into().ok()),
                    branch,
                    cwd: cwd.to_owned(),
                    transcript: SessionId::parse(&agent.session_id),
                });
            }
        }
        match Brief::new(over, notes) {
            Ok(brief) => Asked::Brief(brief),
            Err(reason) => {
                self.say(reason);
                Asked::Nothing
            },
        }
    }

    /// Takes luneta's zellij to a session that starts in `cwd`, named for it
    /// as the picker's directories name it, with a `-2` where a session has
    /// that name.
    fn start(&mut self, cwd: &str) -> Asked {
        let Some(name) = zellij::session_for(Path::new(cwd)) else {
            self.say(format!("menu: no session can be named for {cwd}"));
            return Asked::Nothing;
        };
        let sessions = self.host.iter().flat_map(|host| &host.sessions);
        let names: Vec<&str> = sessions.map(|session| session.name.as_str()).collect();
        Asked::Start { session: name.free(&names), cwd: cwd.to_owned() }
    }

    /// Has zellij do what a delete key planned, and the highlight go past the
    /// rows it takes away, once they are gone.
    fn delete(&mut self, act: Act) -> Asked {
        let takes = |place: &Place| match (&act, place.pane()) {
            (Act::Pane { session, pane }, Some(at)) => place.session() == session && at == *pane,
            (Act::Tab { session, panes, .. }, Some(at)) => {
                place.session() == session && panes.contains(&at)
            },
            (Act::Pane { .. } | Act::Tab { .. }, None) => false,
            (Act::Kill(session) | Act::Leave { session, .. }, _) => place.session() == session,
        };
        if let Some(gone) = self.cursor.clone().filter(|cursor| takes(cursor)) {
            let at = self.cursor_index().unwrap_or(0);
            let stays = |index: &usize| {
                let row = &self.rows[*index];
                row.stops() && !takes(&row.place())
            };
            let after = (at..self.rows.len()).chain((0..at).rev()).find(stays);
            let after = after.map(|index| self.rows[index].place());
            self.closing = Some(Closing { gone, after });
        }
        Asked::Delete(act)
    }

    /// zellij opened the terminal `opened` for the menu numbered `menu`, or
    /// says why it did not. luneta takes its client there while the menu
    /// still waits on it: one closed since is no longer what you are on.
    pub fn opened(&mut self, menu: u64, opened: Result<u32, String>) -> Asked {
        let waiting = self.menu.as_ref().filter(|it| it.number() == menu && it.opening());
        let Some(session) = waiting.map(|it| it.target().place.listed().clone()) else {
            return Asked::Nothing;
        };
        self.menu = None;
        match opened {
            Ok(pane) => Asked::Go(Place::Pane { session, pane }),
            Err(why) => {
                self.say(format!("menu: nothing opened: {why}"));
                Asked::Nothing
            },
        }
    }

    /// zellij said what closing the pane of the menu numbered `menu` closes,
    /// or why it could not: the menu plans its delete key by it, while it
    /// waits on it.
    pub fn closes(&mut self, menu: u64, closes: Result<Closes, String>) -> Asked {
        let waiting = self.menu.as_ref().filter(|it| it.number() == menu);
        let Some((pane, target)) =
            waiting.and_then(|it| Some((it.planning()?, it.target().clone())))
        else {
            return Asked::Nothing;
        };
        let plan = match closes {
            Ok(closes) => self.plan(target.place.session(), Deletes::Pane { pane, closes }),
            Err(why) => Err(format!("menu: nothing closed: {why}")),
        };
        self.planned(plan)
    }

    /// The finder found `skills` for the menu numbered `menu`, or says why it
    /// found none, for its search box.
    pub fn found(&mut self, menu: u64, skills: Result<Vec<Skill>, String>) {
        if let Some(open) = self.menu.as_mut().filter(|it| it.number() == menu) {
            open.found(skills);
        }
    }

    /// Marks the highlighted pane read at `now`, as going there would, while
    /// its row has the dot, whatever gave it the dot. Otherwise marks it
    /// unread, so that its row has the dot until you go there again: a visit
    /// any luneta is on already started before now, and the next starts
    /// after.
    fn toggle_unread(&mut self, now: SystemTime) {
        let Some(place) = &self.cursor else {
            return;
        };
        let Some(Row::Pane { session, pane, unread, marked, .. }) =
            self.rows.iter().find(|row| row.place() == *place)
        else {
            return;
        };
        let key = session.key(*pane);
        if dotted(*unread, marked.as_ref()) {
            self.news.read(key, self.finished.get(&key).copied(), now);
        } else {
            self.news.mark_unread(key, now);
        }
        self.rebuild();
    }

    /// What a key does while the search is open, with `term` typed in it:
    /// every character types, `j` and `h` too. `Esc` closes it, and so does
    /// `Backspace` once there is nothing left to take back, as in less and vim.
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

    /// What is typed in the search, while it is open.
    fn search(&self) -> Option<&str> {
        self.search.as_ref().map(|search| match search {
            Search::Empty => "",
            Search::Typed { term, .. } => term.as_str(),
        })
    }

    /// Opens the search with `term` in it, or puts `term` in the open one.
    /// The highlight goes to the row that matches it best. An empty `term`
    /// narrows nothing.
    fn find(&mut self, term: String) {
        let search =
            if term.is_empty() { Search::Empty } else { Search::Typed { term, seek: true } };
        self.search = Some(search);
        self.rebuild();
    }

    /// Closes the search, and brings back every row, the highlight on the row
    /// it was on.
    fn close_search(&mut self) {
        if self.search.take().is_some() {
            self.rebuild();
        }
    }

    /// Goes to the highlighted row. Going anywhere closes the search, so that
    /// the sidebar you come back to shows every row.
    fn go(&mut self) -> Asked {
        let Some(place) = self.cursor.clone() else {
            return Asked::Nothing;
        };
        self.close_search();
        Asked::Go(place)
    }

    /// What the mouse does over the sidebar, drawn in `area`. A click, the
    /// left button let go on the line it went down on, on a pane goes there,
    /// and on a session's `▾` or `▸` folds it. A session with no panes under
    /// it, folded or not, goes to the session. A click on the notes goes
    /// nowhere, as the rows under them are not on screen. A click that goes
    /// nowhere takes the focus, as any other button does. The left button
    /// lifts the session whose row it went down on once the mouse moves a
    /// line, and lets go of it where it shows; and the wheel scrolls.
    pub fn mouse(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        // A click or the wheel drops a `g` pressed before it, as a key does.
        // The pointer moving does not: luneta hears every move.
        if matches!(
            mouse.kind,
            MouseEventKind::Down(_) | MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
        ) {
            self.first_pressed = false;
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
                let session = row.and_then(|at| match &self.rows[at] {
                    Row::Session { session, .. } => Some(session.clone()),
                    Row::Pane { .. } => None,
                });
                let at = Position::new(mouse.column, mouse.row);
                self.moving = Moving::Down { at, session };
                Asked::Nothing
            },
            MouseEventKind::Down(_) => Asked::Take,
            MouseEventKind::Drag(MouseButton::Left) => self.drag(mouse, area),
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse, area),
            // The button let go where luneta did not hear it, as outside its
            // window: a session lifted goes back. A selection is the keys'.
            MouseEventKind::Moved => {
                self.let_go();
                Asked::Nothing
            },
            _ => Asked::Nothing,
        }
    }

    /// The mouse moves with the left button down: a session lifted goes with
    /// it, and one pressed on its row is lifted once the mouse moves a line,
    /// and takes the focus, for `Esc` to put it back. What is typed in the
    /// search lifts none: it leaves out of sight the places a session could
    /// take.
    fn drag(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let line = mouse.row.saturating_sub(area.y);
        let over = (area.left()..area.right()).contains(&mouse.column);
        let session = match &mut self.moving {
            Moving::Lifted(lift) => {
                lift.line = line;
                lift.over = over;
                return Asked::Nothing;
            },
            Moving::Down { at, session } if mouse.row != at.y => session.clone(),
            Moving::Down { .. } | Moving::Selected(_) | Moving::Still => return Asked::Nothing,
        };
        let typed = matches!(self.search, Some(Search::Typed { .. }));
        let Some(session) = session.filter(|it| !typed && self.session_at(it).is_some()) else {
            return Asked::Nothing;
        };
        self.moving = Moving::Lifted(Lift { session, line, over, scrolled: None });
        Asked::Take
    }

    /// The left button lets go: a press that is on the line it went down on
    /// is a click there, and a session lifted lands where it shows, the
    /// highlight on it, among the rows as the order is kept now, as `shift`
    /// moves them.
    fn release(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let over = (area.left()..area.right()).contains(&mouse.column);
        match std::mem::take(&mut self.moving) {
            Moving::Down { at, .. } if over && mouse.row == at.y => self.click(at, area),
            Moving::Lifted(mut lift) => {
                lift.line = mouse.row.saturating_sub(area.y);
                lift.over = over;
                self.reorder();
                self.cursor = Some(Place::Session(lift.session.clone()));
                match self.landing(&lift) {
                    Some((sessions, _)) if sessions != self.sessions() => self.keep(&sessions),
                    _ => self.settle(None, false),
                }
                Asked::Nothing
            },
            // A selection is the keys', and no press of the sidebar's.
            selected @ Moving::Selected(_) => {
                self.moving = selected;
                Asked::Nothing
            },
            Moving::Down { .. } | Moving::Still => Asked::Nothing,
        }
    }

    /// A click at `at`, on luneta's terminal.
    fn click(&mut self, at: Position, area: Rect) -> Asked {
        let Some(index) = self.line_of(at.y, area).and_then(|line| self.row_at(line)) else {
            return Asked::Take;
        };
        let row = &self.rows[index];
        let place = row.place();
        if matches!(row, Row::Session { .. }) {
            // What is typed shows the panes it finds whatever the folds, so
            // there the triangle is the name's. An empty search keeps them.
            let on_triangle = at.x.saturating_sub(area.x) <= 2;
            let typed = matches!(self.search, Some(Search::Typed { .. }));
            if on_triangle && !typed {
                self.cursor = Some(place);
                self.toggle();
                return Asked::Take;
            }
            // The name of a session that lists its panes is no place to go.
            if !row.stops() {
                return Asked::Take;
            }
        }
        self.cursor = Some(place);
        self.go()
    }

    /// The line of all the rows that the line `y` of luneta's terminal shows
    /// of them, with the sidebar in `area`: none on the title or the rule
    /// along the top, nor on the notes at the bottom.
    fn line_of(&self, y: u16, area: Rect) -> Option<usize> {
        let line = usize::from(y.saturating_sub(area.y));
        let shown = line.checked_sub(HEADER).filter(|line| *line < self.room)?;
        Some(self.top + shown)
    }

    /// Whether the left button is down on the sidebar, which has what the
    /// mouse does until it lets go, wherever the mouse goes.
    pub fn pressed(&self) -> bool {
        matches!(self.moving, Moving::Down { .. } | Moving::Lifted(_))
    }

    /// Lets go of what the left button holds on the sidebar, as a release it
    /// never hears: a session lifted goes back, and the press is no click.
    /// A selection is the keys', and stays.
    pub fn let_go(&mut self) {
        if self.pressed() {
            self.moving = Moving::Still;
        }
    }

    /// Whether the rows scroll under a session lifted, as `hold` scrolls them.
    pub fn scrolling(&self) -> bool {
        self.pull() != 0
    }

    /// Which way the rows scroll under a session lifted that the mouse holds
    /// on their first line, or above, or on their last, or below: up while
    /// there are lines above the first, and down while there are below the
    /// last.
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

    /// Scrolls the rows a line under a session lifted that the mouse holds on
    /// their first line or their last, at `at`, once each `SCROLL`.
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

    /// How many lines all the rows take.
    fn lines(&self) -> usize {
        self.rows.iter().map(Row::height).sum()
    }

    /// The row on `line` of all the rows, if one is.
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

    /// Where the highlight is, and which of its two it is.
    fn highlight(&self) -> Highlight {
        if self.keys.held() {
            return Highlight::Lit(self.cursor_index());
        }
        let row = |place: &Place| self.rows.iter().position(|row| row.place() == *place);
        self.here_row.as_ref().and_then(row).map_or(Highlight::Off, Highlight::Resting)
    }

    /// Scrolls so that the row `at` shows on `room` lines.
    fn reveal(&mut self, at: usize, room: usize) {
        let start: usize = self.rows[..at].iter().map(Row::height).sum();
        let end = start + self.rows[at].height();
        if start < self.top {
            self.top = start;
        } else if end > self.top + room {
            self.top = end.saturating_sub(room);
        }
        // The row shows whole, where it fits.
        if end - start <= room {
            assert!(self.top <= start);
            assert!(end <= self.top + room);
        }
    }

    /// Draws the sidebar into `area`, the highlight on its row: where the keys
    /// took it while the sidebar has the focus, over the rows of each session
    /// a selection holds, and on the pane luneta's zellij is on while it does
    /// not, softer, which the view scrolls to show as the pane changes. A
    /// session the mouse lifted has it, where it lands, the others making room
    /// for it. Its last column is the edge, drawn as the title's rule is.
    pub fn draw(&mut self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        self.drawing(now);
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let frame = frame(now);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        lines.extend(self.top(width, frame));
        let notes = self.notes(width, height);
        let room = height.saturating_sub(HEADER + notes.len());
        self.room = room;
        let highlight = self.highlight();
        let landing = match &self.moving {
            Moving::Lifted(lift) => self.landing(lift),
            Moving::Still | Moving::Selected(_) | Moving::Down { .. } => None,
        };
        match highlight {
            Highlight::Lit(at) => {
                // The mouse scrolls the rows under a session it lifted.
                if let Some(at) = at
                    && landing.is_none()
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
            let typed = matches!(self.search, Some(Search::Typed { .. }));
            let none = if typed { " no match" } else { " no sessions" };
            lines.push(Line::styled(none, Style::new().add_modifier(Modifier::DIM)));
        }
        let lit = match (&landing, highlight) {
            (Some((_, lifted)), _) => Some((lifted.clone(), LIT)),
            (None, Highlight::Lit(at)) => {
                let held = self.selected().map(|(sessions, held)| {
                    sessions[held.start].start..sessions[held.end - 1].end
                });
                held.or(at.map(|at| at..at + 1)).map(|rows| (rows, LIT))
            },
            (None, Highlight::Resting(at)) => Some((at..at + 1, RESTING)),
            (None, Highlight::Off) => None,
        };
        let order = match &landing {
            Some((sessions, _)) => sessions.iter().flat_map(Clone::clone).collect(),
            None => (0..self.rows.len()).collect::<Vec<_>>(),
        };
        let mut drawn: Vec<Line<'static>> = Vec::new();
        for at in order {
            let row = &self.rows[at];
            let on = lit.as_ref().filter(|(rows, _)| rows.contains(&at));
            let background = on.map(|(_, background)| *background);
            for mut line in row_lines(row, width, frame, now) {
                if let Some(background) = background {
                    line = line.patch_style(Style::new().bg(background));
                }
                drawn.push(line);
            }
        }
        self.top = self.top.min(drawn.len().saturating_sub(1));
        lines.extend(drawn.into_iter().skip(self.top).take(room));
        lines.resize(height.saturating_sub(notes.len()), Line::default());
        lines.extend(notes);
        put(buffer, area, &lines, self.framing());
    }

    /// Draws the sidebar collapsed into `area`: a rule along the top, and
    /// under it the counts of `tallies` that are not 0, one to a line, each cut
    /// to the room it has. Its last column is the edge, as the sidebar's is.
    /// The rule is in `ERROR` while the daemon does not answer, as the note
    /// that says so is, which the strip has no room for.
    pub fn draw_collapsed(&mut self, buffer: &mut Buffer, area: Rect, now: SystemTime) {
        self.drawing(now);
        let width = usize::from(area.width.saturating_sub(1));
        let rule = if self.away.is_some() {
            Style::new().fg(ERROR)
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
        put(buffer, area, &lines, self.framing());
    }

    /// Builds the rows again, as the sidebar is drawn at `now`, where a
    /// command has run long enough by then to show.
    fn drawing(&mut self, now: SystemTime) {
        self.drawn = Some(now);
        if self.due.is_some_and(|due| due <= now) {
            self.rebuild();
        }
    }

    /// The top of the sidebar, over the rows: its title, its rule, and a
    /// blank line.
    fn top(&self, width: usize, frame: usize) -> [Line<'static>; HEADER] {
        [self.title(width), self.header(width, frame), Line::default()]
    }

    /// How the title's rule and the edge are drawn: in `ACCENT` while the
    /// sidebar has the keys, and dim while zellij or a box over it has them.
    fn framing(&self) -> Style {
        match self.keys {
            Keys::Sidebar => Style::new().fg(ACCENT),
            Keys::Zellij | Keys::Lent | Keys::Menu { .. } => {
                Style::new().add_modifier(Modifier::DIM)
            },
        }
    }

    /// The first line: `menu::TITLE` on a rule out to the edge, drawn as
    /// `framing` says, the word bold while the sidebar has the keys.
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

    /// The rule under the title: how many sessions the rows list, how many
    /// agents on them wait for you, how many panes have the dot, and how many
    /// agents are at work, and how many are in a shell.
    fn header(&self, width: usize, frame: usize) -> Line<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let Counts { sessions, waiting, .. } = self.counts;
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
        let word = match sessions {
            0 => String::new(),
            1 => " 1 session ".to_owned(),
            n => format!(" {n} sessions "),
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

    /// What goes at the bottom of a sidebar `height` tall: why the daemon
    /// does not answer, the sessions it lists that the rows leave out, the
    /// session files of Claude Code's it cannot read, what went wrong last,
    /// and the menu while it is open, or else the search while that is, or
    /// else the legend while the sidebar has the focus.
    /// The menu and the legend leave the rows a line.
    fn notes(&self, width: usize, height: usize) -> Vec<Line<'static>> {
        let mut notes = Vec::new();
        let error = Style::new().fg(ERROR);
        if let Some(away) = &self.away {
            notes.extend(wrapped(&format!("lunetad: {away}"), width, error));
        }
        if self.unlisted > 0 {
            let count = self.unlisted;
            let sessions = if count == 1 { "session" } else { "sessions" };
            let unlisted = format!(
                "lunetad: {count} {sessions} with no start or socket, or a name zellij refuses, \
                 not listed"
            );
            notes.extend(wrapped(&unlisted, width, error));
        }
        if let Some(unread) = self.host.as_ref().and_then(Host::unread_count) {
            notes.extend(wrapped(&unread.to_string(), width, error));
        }
        if let Some(lost) = self.folds.lost() {
            notes.extend(wrapped(&format!("folds stay in this luneta: {lost}"), width, error));
        }
        if let Some(lost) = self.news.lost() {
            notes.extend(wrapped(&format!("dots stay in this luneta: {lost}"), width, error));
        }
        if let Some(lost) = self.order.lost() {
            notes.extend(wrapped(&format!("order stays in this luneta: {lost}"), width, error));
        }
        if let Some(unread) = self.order.unread() {
            notes.extend(wrapped(&format!("order not read: {unread}"), width, error));
        }
        if let Some(notice) = &self.notice {
            notes.extend(wrapped(notice, width, error));
        }
        if let Some(Sent { session, from: Some(Leaving { ends: true, .. }), landed: false, .. }) =
            &self.sent
        {
            let going = format!("Going to {session}; this session ends once you are there");
            notes.extend(wrapped(&going, width, Style::new().fg(NAME)));
        }
        let room = height.saturating_sub(HEADER + 1 + notes.len());
        if let Some(menu) = self.menu.as_ref().filter(|_| !self.floats()) {
            notes.extend(menu.lines(width, room));
        } else if let Some(term) = self.search() {
            notes.push(query(term, width));
        } else if self.keys == Keys::Sidebar {
            notes.extend(menu::legend(width, room));
        }
        notes
    }
}

/// The counts the sidebar keeps collapsed, each by the glyph of what it
/// counts, which the spinner shows at `frame`, and its style: how many panes
/// have the dot, and how many agents are at work, and how many are in a shell.
fn tallies(counts: Counts, frame: usize) -> [(usize, (String, Style)); 3] {
    let dot = dot();
    [
        (counts.new, (dot.content.into_owned(), dot.style)),
        (counts.busy, Mark::Busy.glyph(frame)),
        (counts.shell, Mark::Shell.glyph(frame)),
    ]
}

/// The column of the edge of a sidebar drawn into `area`, its last, where it
/// has any: where it is drawn, and where a press takes it to drag.
pub fn edge(area: Rect) -> Option<u16> {
    area.width.checked_sub(1).map(|last| area.x + last)
}

/// Puts `lines` into `area` of `buffer`, one to each of its lines, with what
/// was there before gone, and the edge down its last column in `framing`, on
/// every line of it, however few `lines` there are.
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

/// The search, `width` wide: a `/`, and then what is typed, before a `_` for
/// the cursor, its end where it is too long.
pub fn query(term: &str, width: usize) -> Line<'static> {
    let typed = tail(&format!("{term}_"), width.saturating_sub(2));
    Line::from(vec![
        Span::raw(" "),
        Span::styled("/", Style::new().fg(TAG)),
        Span::styled(typed, Style::new().fg(ACCENT)),
    ])
}

/// Whether zellij's `title` for a terminal can be of the session `name`: it is
/// the name, or starts with it before a ` | `.
pub fn in_session(title: &str, name: &str) -> bool {
    title.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with(" | "))
}

/// The clients the daemon lists of the session called `name`, unless another
/// runs under that name too, which it lists none of either.
fn clients<'a>(host: Option<&'a Host>, name: &str) -> Option<&'a [Client]> {
    let mut named = host?.sessions.iter().filter(|session| session.name == name);
    let (Some(session), None) = (named.next(), named.next()) else {
        return None;
    };
    session.clients.as_ref().map(|clients| clients.clients.as_slice())
}

/// The sessions the daemon lists, as the sidebar tells them apart, oldest
/// first, so that a row never moves under your eye or your mouse but as you
/// move it; and how many it lists that `Listed` leaves out.
fn oldest_first(sessions: &[Session]) -> (Vec<(Listed, &Session)>, usize) {
    let mut listed: Vec<(Listed, &Session)> = sessions
        .iter()
        .filter_map(|session| Some((Listed::of(session)?, session)))
        .collect();
    let unlisted = sessions.len() - listed.len();
    listed.sort_by(|(a, _), (b, _)| {
        (a.kept.started(), a.name.as_str()).cmp(&(b.kept.started(), b.name.as_str()))
    });
    (listed, unlisted)
}

/// `sessions` with those `taken` put back before the one at `to` of those
/// left, or after the last of them.
fn moved(sessions: &[Range<usize>], taken: Range<usize>, to: usize) -> Vec<Range<usize>> {
    let left = sessions.iter().enumerate().filter(|(at, _)| !taken.contains(at));
    let mut moved: Vec<Range<usize>> = left.map(|(_, span)| span.clone()).collect();
    moved.splice(to..to, sessions[taken].iter().cloned());
    moved
}

/// The agent in each pane, by its session and the pane's id. A pane holds one:
/// the agent started at a prompt, or else the one that started first. An
/// agent is in the session the daemon follows its pane to, or, where the
/// daemon cannot tell, the one `found` finds.
fn seated<'a>(agents: &'a [Agent], sessions: &'a [Session]) -> HashMap<(&'a str, u32), &'a Agent> {
    let mut seated: HashMap<(&str, u32), &Agent> = HashMap::new();
    for agent in agents {
        let Some(zellij) = &agent.zellij else {
            continue;
        };
        let Some(session) = zellij.session.as_deref().or_else(|| found(agent, zellij, sessions))
        else {
            continue;
        };
        let rank = |agent: &Agent| {
            let started = agent.started_at.as_ref().map(|at| (at.seconds, at.nanos));
            (agent.entrypoint.as_deref() != Some("cli"), started)
        };
        seated
            .entry((session, zellij.pane))
            .and_modify(|seat| {
                if rank(agent) < rank(seat) {
                    *seat = agent;
                }
            })
            .or_insert(agent);
    }
    seated
}

/// The session of an agent the daemon cannot follow to one, as it cannot one
/// renamed before it started, found by where it works: the one session that
/// lists a pane of the agent's id that works where the agent does. Of more
/// than one, the session the agent's environment names, or else the one whose
/// pane runs `claude` or an agent titled, if one alone does.
fn found<'a>(agent: &Agent, zellij: &Zellij, sessions: &'a [Session]) -> Option<&'a str> {
    if agent.cwd.is_empty() {
        return None;
    }
    let pane = |session: &'a Session| {
        let mut panes = session.panes.iter().flat_map(|panes| &panes.panes);
        let pane = panes.find(|pane| {
            pane.id == zellij.pane && pane.cwd.as_deref() == Some(agent.cwd.as_str())
        })?;
        Some((session.name.as_str(), pane))
    };
    let holders: Vec<(&str, &Pane)> = sessions.iter().filter_map(pane).collect();
    if let [(only, _)] = holders.as_slice() {
        return Some(only);
    }
    if let Some((named, _)) = holders.iter().find(|(name, _)| *name == zellij.named) {
        return Some(named);
    }
    let claude = |pane: &Pane| {
        pane.command.as_deref().is_some_and(|run| run.contains("claude")) || pane.titled.is_some()
    };
    let mut claudes = holders.iter().filter(|(_, pane)| claude(pane));
    match (claudes.next(), claudes.next()) {
        (Some((one, _)), None) => Some(one),
        _ => None,
    }
}

/// Where an agent works, as its row, the picker and a search say it: where
/// zellij says its pane works, which its branch is found by, or else, while
/// zellij has not said, where Claude Code says.
fn agent_dir<'a>(agent: &'a Agent, pane: &'a Pane) -> &'a str {
    pane.cwd.as_deref().unwrap_or(&agent.cwd)
}

/// What a pane's row is named. Its agent goes by the name someone gave it, or
/// else by its topic, as the daemon reads it in the title Claude Code gives
/// its pane; or else by its tab, if someone named it, or else by where it
/// works. A program goes by its command line, the program by its binary's
/// name, and a shell at its prompt by its tab, if someone named it, as an
/// agent does, or else by its directory's last two parts.
fn label(agent: Option<&Agent>, pane: &Pane) -> String {
    if let Some(agent) = agent {
        let topic = pane.titled.as_ref().and_then(|titled| titled.topic.as_deref());
        let name = given_name(agent).or(topic).or(tab::named(&pane.tab_name));
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
        _ => match (tab::named(&pane.tab_name), pane.cwd.as_deref()) {
            (Some(tab), _) => tab.to_owned(),
            (None, Some(cwd)) => short_cwd(cwd),
            (None, None) => format!("pane {}", pane.id),
        },
    }
}

/// The mark of `agent`, in the pane of `key`, as its row shows it, and when
/// it finished, where it is done: what it is doing, or that it finished since
/// you were last in its pane, but in the pane luneta's zellij is on, `here`,
/// where you see it.
fn agent_marked(news: &News, agent: &Agent, key: Key, here: bool) -> (Marked, Option<SystemTime>) {
    let mut marked = Marked::of(agent);
    let done = marked.since.filter(|_| marked.mark == Mark::Idle);
    if !here && news.is_new(key, done) {
        marked.mark = Mark::Finished;
    }
    (marked, done)
}

/// What an agent is doing, in the word Claude Code says it in, for the brief.
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

/// The name of an agent's session, if someone gave it one, as `/rename` does:
/// one Claude Code derives from the directory, or makes up, is no name a row
/// goes by. A name from before Claude Code said who chose it is taken for
/// given.
fn given_name(agent: &Agent) -> Option<&str> {
    let name = agent.name.as_ref()?;
    let source = name.source.map(NameSource::try_from);
    let given = matches!(source, None | Some(Ok(NameSource::User | NameSource::Peer)));
    Some(name.text.trim()).filter(|text| given && !text.is_empty())
}

/// A directory as luneta shows one: its last two parts.
pub fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_owned(),
        [only] => (*only).to_owned(),
        [.., parent, base] => format!("{parent}/{base}"),
    }
}

/// How well a row matches a search: a row that shows what is typed itself
/// beats any other, and then the higher score wins.
pub type Fit = (bool, i64);

/// What is typed in a search, and the matcher the sidebar and the picker
/// find their rows with.
pub struct Finder<'a> {
    matcher: SkimMatcherV2,
    /// The matcher, but for a match at the start of a text, which counts as
    /// one at the start of any word in it.
    anywhere: SkimMatcherV2,
    term: &'a str,
}

impl<'a> Finder<'a> {
    pub fn new(term: &'a str) -> Self {
        let anywhere = SkimScoreConfig { bonus_first_char_multiplier: 1, ..Default::default() };
        let anywhere = SkimMatcherV2::default().score_config(anywhere);
        Finder { matcher: SkimMatcherV2::default(), anywhere, term }
    }

    fn session(&self, name: &str) -> Option<Fit> {
        self.find(name, &[]).map(|(fit, _)| fit)
    }

    /// A pane by its row, its branch, its tab and the directory it, or its
    /// agent, works in. A tab zellij named `Tab #N` is no name to find, and a
    /// directory is its last two parts, as a shell's row shows it: a whole
    /// path has the letters for a few typed ones to land on nearly any of it.
    fn pane(
        &self,
        label: &str,
        branch: Option<&str>,
        tab_name: &str,
        cwd: Option<&str>,
    ) -> Option<Fit> {
        let cwd = cwd.map(short_cwd);
        self.find(label, &[branch, tab::named(tab_name), cwd.as_deref()]).map(|(fit, _)| fit)
    }

    /// What is typed against what a row shows, and against what else the row
    /// goes by, of which it need not have each: how well the best of them
    /// fits, and the characters of what the row shows that the search found.
    /// None light up where something else fits better, but for its start:
    /// `review` fits a session named `review` better than its agent's row
    /// `docs (review:4)` only by starting it, and lights up in the row.
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

/// The branch checked out in a work tree, how the tree stands, and its
/// `pull_request`, or none for a detached `HEAD` or a directory in no
/// repository.
fn checkout(tree: &WorkTree, pull_request: Option<PullRequest>) -> Option<Checkout> {
    let WorkTree::Checkout { head: Head::Branch { name, upstream }, changes, stashes } = tree
    else {
        return None;
    };
    Some(Checkout {
        branch: name.as_str().to_owned(),
        status: glyphs(changes, *stashes, *upstream),
        pull_request: pull_request.and_then(numbered),
    })
}

/// The number of a pull request and the colour of its state, which gh lists
/// it in: green while it is open, grey while it is a draft, and purple once
/// it is merged. A closed one is none.
fn numbered(pull_request: PullRequest) -> Option<(u32, Color)> {
    let colour = match pull_request.state {
        State::Open => gruvbox::GREEN,
        State::Draft => gruvbox::GREY,
        State::Merged => gruvbox::PURPLE,
        State::Closed => return None,
    };
    Some((pull_request.number.get(), colour))
}

/// The glyph of each kind of change there is, then the one for where the
/// branch stands against its upstream, where it has one: what starship's
/// `git_status` shows, with its default glyphs and in its order.
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

/// How long ago `since` was, in the unit that says the most of it.
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

/// The frame the spinner shows at `now`: the same on every row, and the next
/// one each `SPIN`.
pub fn frame(now: SystemTime) -> usize {
    let since = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let turns = since.as_millis() / SPIN.as_millis();
    let frames = u128::try_from(SPINNER.len()).expect("a spinner of ten frames");
    usize::try_from(turns % frames).expect("a frame of the spinner's")
}

/// The lines a row is drawn on, `width` wide.
fn row_lines(row: &Row, width: usize, frame: usize, now: SystemTime) -> Vec<Line<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match row {
        Row::Session { session, under, here, waiting, dot, busy } => {
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let name_style = if *here {
                Style::new().fg(NAME).add_modifier(Modifier::BOLD)
            } else {
                Style::new().add_modifier(Modifier::BOLD)
            };
            let name = cut(session.name.as_str(), width.saturating_sub(4 + reserve + 1));
            let rule = width.saturating_sub(3 + name.width() + 1 + reserve + 1);
            let rule_style = if *here { Style::new().fg(ACCENT) } else { dim };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
                Span::styled(name, name_style),
                Span::raw(" "),
                Span::styled(RULE.repeat(rule), rule_style),
            ];
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            vec![Line::from(spans)]
        },
        Row::Pane { here, unread, marked, icon, label, checkout, bracket, .. } => {
            let mark = marked.as_ref().map(|marked| &marked.mark);
            let since = marked.as_ref().and_then(|marked| marked.since);
            let age = since.map(|since| age(since, now)).unwrap_or_default();
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            // A pane with no mark, of an agent or of a command, is dimmed,
            // and its icon is drawn as its label is.
            let label_style = match mark {
                Some(Mark::Waiting) => Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                Some(_) => Style::new(),
                None => dim,
            };
            let height = row.height();
            let mut spans = vec![margin(*here), gutter(*bracket, 0, height)];
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
            let label = cut(label, width.saturating_sub(LABEL + reserve + 1));
            let gap = width.saturating_sub(LABEL + label.width() + age.width() + 1);
            spans.push(Span::styled(label, label_style));
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(age, dim));
            let mut lines = vec![Line::from(spans)];
            if let Some(checkout) = checkout {
                lines.push(checkout_line(*here, gutter(*bracket, 1, height), checkout, width));
            }
            lines
        },
    }
}

/// What a session's row ends in: how many of its agents wait for you, or else
/// the dot, while it is `dotted`, or else the spinner, while it is `spinning`.
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

/// The first column of a pane's lines: `HERE_MARK` on the pane luneta's
/// zellij is on, `here`.
fn margin(here: bool) -> Span<'static> {
    if here {
        Span::styled(HERE_MARK, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
    } else {
        Span::raw(" ")
    }
}

/// The column after a pane's mark, on its line `at` of the `height` it
/// takes: the bracket that joins it to the panes of its tab, where it sits
/// among them as it `sits`, in its tab's colour, or else blank.
fn gutter(bracket: Option<(Bracket, Color)>, at: usize, height: usize) -> Span<'static> {
    let Some((sits, colour)) = bracket else {
        return Span::raw(" ");
    };
    let glyph = match sits {
        Bracket::First if at == 0 => BRACKET_TOP,
        Bracket::Last if at + 1 == height => BRACKET_FOOT,
        Bracket::First | Bracket::Inner | Bracket::Last => BRACKET_SIDE,
    };
    Span::styled(glyph, Style::new().fg(colour))
}

/// The line under a pane's row, `width` wide: the branch checked out where it
/// works, under its label, then the number of its pull request, and how its
/// work tree stands. The branch is cut to leave the other two whole. The mark
/// of the pane luneta's zellij is on, `here`, runs down it too, and the
/// `gutter` after it.
fn checkout_line(
    here: bool,
    gutter: Span<'static>,
    checkout: &Checkout,
    width: usize,
) -> Line<'static> {
    let status = &checkout.status;
    let number = checkout.pull_request.map(|(number, colour)| {
        Span::styled(format!("#{number}"), Style::new().fg(colour))
    });
    let held = number.as_ref().map_or(0, |number| number.content.width() + 1);
    let reserve = if status.is_empty() { 0 } else { status.width() + 1 };
    let branch = cut(&checkout.branch, width.saturating_sub(LABEL + held + reserve + 1));
    let gap = width.saturating_sub(LABEL + branch.width() + held + status.width() + 1);
    let mut spans = vec![
        margin(here),
        gutter,
        Span::raw(" ".repeat(LABEL - 2)),
        Span::styled(branch, Style::new().fg(gruvbox::GREY)),
    ];
    if let Some(number) = number {
        spans.push(Span::raw(" "));
        spans.push(number);
    }
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::raw(status.clone()));
    Line::from(spans)
}

/// `text` cut to `room` columns, with a `…` for what it took.
pub fn cut(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut kept = String::new();
    let mut taken = 0;
    for c in text.chars() {
        let width = c.width().unwrap_or(0);
        if taken + width + 1 > room {
            break;
        }
        kept.push(c);
        taken += width;
    }
    if room > 0 {
        kept.push('…');
    }
    kept
}

/// `text` cut to its last `room` columns, with a `…` for what it took.
fn tail(text: &str, room: usize) -> String {
    if text.width() <= room {
        return text.to_owned();
    }
    let mut start = text.len();
    let mut taken = 0;
    for (at, c) in text.char_indices().rev() {
        let width = c.width().unwrap_or(0);
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

/// `text` on as many lines of `width` as it takes, a space in from the edge.
pub fn wrapped(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for c in text.chars() {
        if line.width() + c.width().unwrap_or(0) > room {
            lines.push(Line::styled(format!(" {line}"), style));
            line.clear();
        }
        line.push(c);
    }
    if !line.is_empty() {
        lines.push(Line::styled(format!(" {line}"), style));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::LazyLock;

    use luneta_protocol::said::Said;
    use lunetad::api::shell::{Ended, Going, Stopped};
    use lunetad::api::{self, Clients, Pane, Panes, UnreadFile, Version, Zellij};
    use prost_types::Timestamp;

    use super::*;
    use crate::own::Before;

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
        }
    }

    /// A session whose socket has the inode of the second it started at.
    fn session(name: &str, started: i64, panes: Vec<Pane>) -> Session {
        Session {
            name: name.to_owned(),
            started_at: Some(Timestamp { seconds: started, nanos: 0 }),
            panes: Some(Panes { panes }),
            clients: None,
            socket: started as u64,
        }
    }

    fn listed(name: &str, started: u64) -> Listed {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(started);
        let kept = ledger::Session::new(started_at, NonZeroU64::new(started).unwrap()).unwrap();
        Listed { name: session_name(name), kept }
    }

    fn session_name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    fn agent(session: &str, pane: u32, status: Status, name: Option<&str>) -> Agent {
        Agent {
            pid: pane,
            session_id: format!("id-{pane}"),
            cwd: "/w/luneta/main".to_owned(),
            status: Some(Reported::KnownStatus(status.into())),
            status_since: Some(Timestamp { seconds: 100, nanos: 0 }),
            started_at: Some(Timestamp { seconds: 50, nanos: 0 }),
            name: name.map(|text| api::Name { text: text.to_owned(), source: None }),
            entrypoint: Some("cli".to_owned()),
            zellij: Some(Zellij {
                named: session.to_owned(),
                session: Some(session.to_owned()),
                pane,
            }),
        }
    }

    fn host(sessions: Vec<Session>, agents: Vec<Agent>) -> Host {
        Host {
            version: Some(Version { pid: 1, seq: 1 }),
            sessions,
            work_trees: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_sockets: Some(0),
            unread_files: Vec::new(),
        }
    }

    /// The sidebar drawn `width` columns wide and `height` tall, with the
    /// focus or without.
    fn drawn(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Buffer {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        drawn_at(sidebar, focused, width, height, now)
    }

    /// The sidebar as `drawn` draws it, at `now`. Each cell is in gruvbox's
    /// colours, on the terminal's background or, on the highlight, the one
    /// that says whether the sidebar has the focus.
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
        let lit = if focused { LIT } else { RESTING };
        in_gruvbox(&buffer, &[Color::Reset, lit]);
        buffer
    }

    /// The sidebar collapsed, `height` tall, as it is drawn at `now`. Each
    /// cell is in gruvbox's colours, on the terminal's background.
    fn collapsed(sidebar: &mut Sidebar, height: u16, now: SystemTime) -> Buffer {
        let area = Rect::new(0, 0, COLLAPSED, height);
        let mut buffer = Buffer::empty(area);
        sidebar.draw_collapsed(&mut buffer, area, now);
        in_gruvbox(&buffer, &[Color::Reset]);
        buffer
    }

    /// Holds each cell of `buffer` to gruvbox's colours, on one of
    /// `backgrounds`: a colour named at a call site, as `Color::Red`, or a
    /// line drawn without `fg1`, would be the terminal's.
    fn in_gruvbox(buffer: &Buffer, backgrounds: &[Color]) {
        for cell in buffer.content() {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
            assert!(backgrounds.contains(&cell.bg), "{:?} on {:?}", cell.symbol(), cell.bg);
        }
    }

    /// The sidebar `width` columns wide and `height` tall, as text, up to its
    /// edge.
    fn screen(sidebar: &mut Sidebar, focused: bool, width: u16, height: u16) -> Vec<String> {
        seen(sidebar, focused, width, height).0
    }

    /// The sidebar as `screen` draws it, and the lines the highlight is on.
    fn seen(
        sidebar: &mut Sidebar,
        focused: bool,
        width: u16,
        height: u16,
    ) -> (Vec<String>, Vec<u16>) {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(160);
        seen_at(sidebar, focused, width, height, now)
    }

    /// The sidebar as `seen` draws it, at `now`.
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

    /// Two sessions, whose rows have Claude's icon, `\u{ec82}`, neovim's,
    /// `\u{e6ae}`, and a terminal's, `\u{e795}`.
    fn two_sessions() -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
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
        sidebar.titled("older | ~/w/luneta".to_owned());
        sidebar
    }

    #[test]
    fn sessions_go_oldest_first_with_their_panes_in_tab_order() {
        let mut sidebar = two_sessions();
        assert_eq!(
            screen(&mut sidebar, false, 32, 8),
            [
                "─ luneta ──────────────────────",
                "─ 2 sessions ───────────── ●1 ─",
                "",
                " ▾ older ────────────────── ●1",
                "   ● \u{ec82} fold                 1m",
                "     \u{e6ae} nvim src/main.rs",
                " ▾ newer ─────────────────────",
                "     \u{e795} w/b",
            ]
        );
    }

    /// A bracket down the column after `▌` joins the panes of a tab that
    /// holds more than one, from the first line of the first, down the branch
    /// under one, to the last line of the last. A pane alone in its tab has
    /// none, nor has one a search lists alone of its tab, as for `c` here.
    #[test]
    fn a_bracket_joins_the_panes_that_share_a_tab() {
        let panes = vec![
            pane(1, 0, "/w/a", "less a"),
            pane(2, 1, "/w/b", "less b"),
            pane(3, 1, "/w/c", "less c"),
            pane(4, 2, "/w/c", "less d"),
            pane(5, 2, "/w/b", "less e"),
        ];
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let name = Said::new("main".to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None };
        let tree = WorkTree::Checkout { head, changes: Changes::default(), stashes: 0 };
        host.work_trees.push(tree.to_api("/w/b"));
        let mut sidebar = Sidebar::default();
        sidebar.heard(host);

        assert_eq!(
            rows_shown(&mut sidebar),
            [
                " ▾ s",
                "       less a",
                " ╭     less b",
                " │     main",
                " ╰     less c",
                " ╭     less d",
                " │     less e",
                " ╰     main",
            ]
        );
        type_in(&mut sidebar, "/c");
        assert_eq!(rows_shown(&mut sidebar), [" ▾ s", "       less c", "       less d"]);
    }

    /// A tab's bracket is in a colour of the tab's own, by the id zellij gave
    /// it, which the tab keeps as a tab before it closes and its position goes
    /// down. A daemon older than the id does not say it, and the brackets it
    /// lists stay grey, rather than all in the first tab's colour.
    #[test]
    fn a_tab_keeps_its_colour_as_a_tab_before_it_closes() {
        let colours = |sidebar: &mut Sidebar, lines: usize| {
            let buffer = drawn(sidebar, true, 32, 24);
            (1..=lines).map(|line| buffer[(1, (HEADER + line) as u16)].fg).collect::<Vec<_>>()
        };
        let three = (0..6).map(|id| pane(id + 1, id / 2, "/w/a", "less")).collect();
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(vec![session("s", 10, three)], Vec::new()));
        let (b, y, p) = (gruvbox::BLUE, gruvbox::YELLOW, gruvbox::PURPLE);
        assert_eq!(colours(&mut sidebar, 6), [b, b, y, y, p, p]);

        // The first tab closed: the other two are at 0 and 1 now.
        let closed = |tab_id: fn(u32) -> Option<u32>| {
            let in_tab = |id: u32| Pane {
                tab_id: tab_id(id / 2),
                ..pane(id + 1, id / 2 - 1, "/w/a", "less")
            };
            host(vec![session("s", 10, (2..6).map(in_tab).collect())], Vec::new())
        };
        sidebar.heard(closed(Some));
        assert_eq!(colours(&mut sidebar, 4), [y, y, p, p]);
        sidebar.heard(closed(|_| None));
        assert_eq!(colours(&mut sidebar, 4), [gruvbox::GREY; 4]);
    }

    /// The first line says `luneta`, in the accent, the word bold, while the
    /// sidebar has the keys, and dim while zellij has them.
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
            // The first `─`, the word's `l` and the last `─`.
            assert_eq!([0, 2, 30].map(|x| look(&buffer, x)), [rule, word, rule], "{focused}");
        }
    }

    /// A brief the sidebar asked for has the keys until it closes: the title
    /// is dim, and the legend gone, while the highlight stays where the keys
    /// left it, for them to come back to.
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
        let cursor = sidebar.cursor.clone();
        let (titled, lit, fg, dim) = draw(&mut sidebar);
        // The title and the legend's rule.
        assert_eq!((titled, fg, dim, lit.is_empty()), (2, ACCENT, false, false));
        sidebar.focus(Keys::Lent);
        assert_eq!(draw(&mut sidebar), (1, lit.clone(), gruvbox::FG1, true));
        sidebar.focus(Keys::Sidebar);
        assert_eq!(draw(&mut sidebar), (2, lit, ACCENT, false));
        assert_eq!(sidebar.cursor, cursor);
    }

    /// The rule along the top counts the agents on the rows, a folded
    /// session's among them, and none that no row holds. Those in a shell
    /// are counted dim, as their mark is.
    #[test]
    fn the_top_rule_counts_the_agents_the_rows_hold() {
        let mut sidebar = Sidebar::default();
        let mut outside = agent("s", 4, Status::Waiting, None);
        outside.zellij = None;
        let panes = (1..=3).map(|id| pane(id, 0, "/w/a", "claude")).collect();
        sidebar.heard(host(
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
        assert_eq!((rule.as_str(), sidebar.turning()), ("─ 1 session ─────────── ⠋1 $2 ─", true));
        let buffer = drawn(&mut sidebar, false, 32, 4);
        let at = |glyph: char| rule.chars().position(|c| c == glyph).expect("on the rule") as u16;
        let dim = |x: u16| buffer[(x, 1)].modifier.contains(Modifier::DIM);
        let (spinner, dollar) = (at('⠋'), at('$'));
        assert_eq!(
            [spinner, spinner + 1, dollar, dollar + 1].map(dim),
            [false, false, true, true]
        );
    }

    /// Collapsed, the sidebar keeps under its rule only the counts of the
    /// panes with the dot, of the agents at work and of those in a shell, and
    /// none of the agents that wait for you. A count is cut to the columns it
    /// has before the edge, and a command counts once it has run a second, as
    /// on the rule along the top.
    #[test]
    fn collapsed_the_sidebar_keeps_the_dots_the_work_and_the_shells() {
        let mut sidebar = Sidebar::default();
        let mut panes: Vec<Pane> = (1..=103).map(|id| pane(id, 0, "/w/a", "claude")).collect();
        panes.push(in_shell(104, going("make", 159, Front::Working)));
        let mut agents =
            vec![agent("s", 1, Status::Idle, None), agent("s", 2, Status::Waiting, None)];
        agents.extend((3..=103).map(|id| agent("s", id, Status::Shell, None)));
        sidebar.heard(host(vec![session("s", 10, panes)], agents));
        let strip = |sidebar: &mut Sidebar, now| {
            let buffer = collapsed(sidebar, 6, now);
            let line = |y| (0..COLLAPSED - 1).map(|x| buffer[(x, y)].symbol()).collect::<String>();
            (0..6).map(|y| line(y).trim_end().to_owned()).collect::<Vec<_>>()
        };
        assert_eq!(strip(&mut sidebar, moment(160_500)), ["───", "", "•1", "$1…", "", ""]);
        assert_eq!(strip(&mut sidebar, moment(161_000)), ["───", "", "•1", "⠋1", "$1…", ""]);
    }

    /// Collapsed, the sidebar has no room for the note that says lunetad does
    /// not answer, so its rule turns the note's colour until lunetad answers
    /// again, and is drawn as the open sidebar's rule is otherwise.
    #[test]
    fn collapsed_the_sidebar_reddens_its_rule_while_lunetad_does_not_answer() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(idle_agent());
        let look = |buffer: &Buffer, x: u16, y: u16| {
            let cell = &buffer[(x, y)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let rule = |sidebar: &mut Sidebar| look(&collapsed(sidebar, 3, moment(160_000)), 0, 0);
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 0, 1);
        assert_eq!(rule(&mut sidebar), open);
        sidebar.lost("gone".to_owned());
        // The note, ` lunetad: gone`, takes the last line.
        let buffer = drawn(&mut sidebar, false, COLUMNS, 6);
        assert_eq!(buffer[(1, 5)].symbol(), "l");
        let note = look(&buffer, 1, 5);
        assert_ne!(note, open);
        assert_eq!(rule(&mut sidebar), note);
        sidebar.heard(idle_agent());
        assert_eq!(rule(&mut sidebar), open);
    }

    /// The sidebar's last column is its edge, open or collapsed: a `│` down
    /// every line, on the terminal's background beside the highlight too, and
    /// drawn as the title's rule is, in the accent while the sidebar has the
    /// keys, and dim while zellij or a brief has them.
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

    /// A daemon of another build can list a session with no start, or with
    /// no socket, which the sidebar cannot tell from another, or with a name
    /// zellij refuses, which luneta can neither go to nor fold. A name longer
    /// than luneta makes one is zellij's to refuse: where the user's id is
    /// short, it makes one of 66 bytes.
    #[test]
    fn a_session_with_no_start_or_socket_or_a_refused_name_is_left_out_and_said() {
        let mut sidebar = Sidebar::default();
        let mut unstarted = session("x", 0, vec![]);
        unstarted.started_at = None;
        let mut unsocketed = session("z", 30, vec![]);
        unsocketed.socket = 0;
        let misnamed = session("a/b", 40, vec![]);
        let long = session(&"w".repeat(66), 50, vec![]);
        let sessions = vec![unstarted, session("y", 20, vec![]), unsocketed, misnamed, long];
        sidebar.heard(host(sessions, Vec::new()));
        let lines = screen(&mut sidebar, false, 60, 7);
        assert_eq!(lines[1], format!("─ 2 sessions {}", "─".repeat(46)));
        assert_eq!(lines[3], format!(" ▾ y {}", "─".repeat(53)));
        assert!(lines[4].starts_with(" ▾ wwww"), "{:?}", lines[4]);
        assert_eq!(lines[5], " lunetad: 3 sessions with no start or socket, or a name ze");
        assert_eq!(lines[6], " llij refuses, not listed");
    }

    /// `▌` marks the pane luneta's zellij is on, down the line of its branch
    /// too, with the focus or without. While the sidebar does not have the
    /// focus, the highlight rests on that pane; once it has, the highlight
    /// goes with the keys, and comes back as the sidebar gives the focus
    /// back. A click on another pane leaves it on yours, as `▌` is, until
    /// luneta's zellij is there.
    #[test]
    fn the_highlight_rests_on_your_pane_until_the_sidebar_has_the_focus() {
        let mut sidebar = Sidebar::default();
        let mut host = host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session(
                    "older",
                    10,
                    vec![pane(2, 1, "/w/c", "nvim src/main.rs"), pane(1, 0, "/w/a", "fish")],
                ),
            ],
            Vec::new(),
        );
        let changes = Some(api::Changes::default());
        let head = Some(api::checkout::Head::Branch(api::Branch { name: "main".to_owned(), upstream: None }));
        let checkout = api::Checkout { head, changes, stashes: 0 };
        host.work_trees = vec![api::WorkTree {
            dir: "/w/c".to_owned(),
            stands: Some(api::work_tree::Stands::Checkout(checkout)),
        }];
        assert_eq!(sidebar.heard(host), None);
        let look = sidebar.titled("older | nvim".to_owned()).unwrap();
        assert_eq!(look.session, "older");
        let clients = vec![Client { id: 1, pane: Some(2) }];
        sidebar.looked(Looked { number: look.number, went: None, clients, before: None });

        let (lines, lit) = seen(&mut sidebar, false, 32, 13);
        assert_eq!(lines[5..7], ["▌    \u{e6ae} nvim src/main.rs", "▌      main"]);
        assert_eq!(lit, [5, 6]);
        let (_, lit) = seen(&mut sidebar, true, 32, 16);
        assert_eq!(lit, [5, 6]);
        sidebar.key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), moment(160_000));
        let (lines, lit) = seen(&mut sidebar, true, 32, 16);
        let nvim = "▌    \u{e6ae} nvim src/main.rs";
        assert_eq!((lines[5].as_str(), lit.as_slice()), (nvim, &[4][..]));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [5, 6]);
        let fish = Place::Pane { session: listed("older", 10), pane: 1 };
        assert_eq!(click(&mut sidebar, 10, 4, Rect::new(0, 0, 32, 14)), Asked::Go(fish));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [5, 6]);
    }

    #[test]
    fn the_highlight_skips_a_session_s_row_and_goes_no_further_than_the_ends() {
        let mut sidebar = two_sessions();
        let older = Some(Place::Pane { session: listed("older", 10), pane: 1 });
        let newer = Some(Place::Pane { session: listed("newer", 20), pane: 1 });
        assert_eq!(sidebar.cursor, older);
        sidebar.step(1);
        sidebar.step(1);
        assert_eq!(sidebar.cursor, newer);
        sidebar.step(1);
        assert_eq!(sidebar.cursor, newer);
        sidebar.step(-5);
        assert_eq!(sidebar.cursor, older);
    }

    /// `G` and `End` take the highlight to the last row, and `gg` and `Home`
    /// to the first. A key between the two `g`s drops the first, with `Ctrl`
    /// too, and so do the wheel, a click and the focus going, but not the
    /// pointer moving. `Esc` after a `g` does nothing else, and leaves the
    /// sidebar pressed again.
    #[test]
    fn g_and_end_go_to_the_last_row_and_gg_and_home_to_the_first() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let first = Some(Place::Pane { session: listed("older", 10), pane: 1 });
        let second = Some(Place::Pane { session: listed("older", 10), pane: 2 });
        let last = Some(Place::Pane { session: listed("newer", 20), pane: 1 });
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Shift and all, as a terminal sends it as text.
        let shift_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        let g = key(KeyCode::Char('g'));
        sidebar.key(&shift_g, moment(160_000));
        assert_eq!(sidebar.cursor, last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, last);
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, first);
        sidebar.key(&key(KeyCode::End), moment(160_000));
        assert_eq!(sidebar.cursor, last);
        sidebar.key(&key(KeyCode::Home), moment(160_000));
        assert_eq!(sidebar.cursor, first);
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        for pressed in [key(KeyCode::End), g, key(KeyCode::Char('k')), g, ctrl_x, g] {
            sidebar.key(&pressed, moment(160_000));
        }
        assert_eq!(sidebar.cursor, second);
        let area = Rect::new(0, 0, 32, 16);
        screen(&mut sidebar, true, area.width, area.height);
        sidebar.mouse(mouse(MouseEventKind::ScrollUp, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, second);
        // luneta hears every move of the pointer.
        sidebar.mouse(mouse(MouseEventKind::Moved, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, first);
        // The triangle folds newer, and the highlight goes to its row.
        sidebar.key(&g, moment(160_000));
        assert_eq!(click(&mut sidebar, 1, 6, area), Asked::Take);
        let folded = Some(Place::Session(listed("newer", 20)));
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, folded);
        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        sidebar.key(&g, moment(160_000));
        assert_eq!(sidebar.cursor, folded);
        let esc = key(KeyCode::Esc);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Leave);
    }

    /// zellij versions with sockets of their own can each run a session under
    /// one name, and the daemon lists them both, with no panes.
    #[test]
    fn the_highlight_goes_on_past_a_second_session_of_a_name() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
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
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(z));
    }

    #[test]
    fn folding_a_pane_s_session_stops_the_highlight_on_its_row() {
        let mut sidebar = two_sessions();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        let older = Place::Session(listed("older", 10));
        assert_eq!(sidebar.cursor, Some(older.clone()));
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(older));
        let lines = screen(&mut sidebar, false, 32, 5);
        assert!(lines[3].starts_with(" ▸ older"), "{lines:?}");
        assert!(lines[4].starts_with(" ▾ newer"), "{lines:?}");
        sidebar.key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), moment(160_000));
        assert_eq!(sidebar.rows.len(), 5);
    }

    /// `Alt Tab` goes to the pane listed after the one luneta's zellij is on,
    /// across sessions, and from the last back to the first.
    #[test]
    fn the_next_pane_is_the_one_listed_after_luneta_s() {
        let older = |pane| Place::Pane { session: listed("older", 10), pane };
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        let cases = [
            ("within its session", older(1), older(2)),
            ("into the next session", older(2), newer.clone()),
            ("from the last to the first", newer.clone(), older(1)),
        ];
        for (case, on, next) in cases {
            let mut sidebar = two_sessions();
            sidebar.went(&on);
            assert_eq!(sidebar.next_pane(), Some(next), "{case}");
        }
        // On a pane no row lists, as a plugin's, or one luneta is not sure
        // its zellij is on; and in a session no row lists.
        let mut sidebar = two_sessions();
        assert_eq!(sidebar.next_pane(), Some(older(1)));
        sidebar.titled("gone".to_owned());
        assert_eq!(sidebar.next_pane(), Some(older(1)));
    }

    /// Back in a session zellij titles nothing for, luneta's zellij goes on
    /// from where luneta sent it, and not from the session of the title, whose
    /// clients are another's.
    #[test]
    fn the_next_pane_follows_luneta_s_zellij_past_a_stale_title() {
        let mut sidebar = two_sessions();
        let older = |pane| Place::Pane { session: listed("older", 10), pane };
        sidebar.went(&Place::Pane { session: listed("newer", 20), pane: 1 });
        sidebar.titled("newer | fish".to_owned());
        sidebar.went(&older(1));
        assert_eq!(sidebar.next_pane(), Some(older(2)));
    }

    #[test]
    fn the_next_pane_passes_over_a_folded_session() {
        let mut sidebar = two_sessions();
        sidebar.went(&Place::Pane { session: listed("older", 10), pane: 2 });
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        assert_eq!(sidebar.next_pane(), Some(newer.clone()));
        // The only pane listed is luneta's own.
        sidebar.went(&newer);
        assert_eq!(sidebar.next_pane(), None);
    }

    /// Going to a folded session unfolds it, whichever way luneta's zellij
    /// goes: from the sidebar or the menu, by zellij's own switch, which the
    /// title follows, or as a client takes the place of one that ended. Its
    /// panes are listed, so `Alt Tab` goes on to the next of them, and not to
    /// the next session's.
    #[test]
    fn going_to_a_folded_session_unfolds_it() {
        let older = |pane| Place::Pane { session: listed("older", 10), pane };
        // luneta's zellij in newer, and older folded.
        let away = || {
            let mut sidebar = two_sessions();
            sidebar.titled("newer | fish".to_owned());
            sidebar.folds.set(&session_name("older"), true);
            sidebar
        };
        let mut sidebar = away();
        sidebar.went(&older(1));
        assert!(!sidebar.folds.contains("older"));
        assert_eq!(sidebar.next_pane(), Some(older(2)));
        let mut sidebar = away();
        sidebar.titled("older | ~/w/luneta".to_owned());
        assert!(!sidebar.folds.contains("older"));
        assert_eq!(sidebar.next_pane(), Some(older(1)));
        let mut sidebar = away();
        sidebar.rejoining(&session_name("older"));
        assert!(!sidebar.folds.contains("older"));
        assert_eq!(sidebar.next_pane(), Some(older(1)));
    }

    /// A go zellij does not make leaves luneta's zellij in the session it was
    /// in, which it did not go to: folded there, it stays folded.
    #[test]
    fn a_go_zellij_does_not_make_keeps_the_session_luneta_stays_in_folded() {
        let mut sidebar = two_sessions();
        sidebar.folds.set(&session_name("older"), true);
        let look = sidebar.went(&Place::Pane { session: listed("newer", 20), pane: 1 }).unwrap();
        sidebar.stayed(look.number);
        assert_eq!(sidebar.here(), Some("older"));
        assert!(sidebar.folds.contains("older"));
    }

    /// zellij titles luneta's terminal with the session's name and a ` | `,
    /// which a name can hold too: the title of `odd | name`, which the daemon
    /// has yet to list, reads as `odd`'s. Starting `odd | name` unfolds it,
    /// and leaves `odd` folded.
    #[test]
    fn going_to_a_session_named_with_a_bar_unfolds_it_alone() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(vec![session("odd", 2, vec![]), session("other", 3, vec![])], vec![]));
        sidebar.titled("other | fish".to_owned());
        sidebar.folds.set(&session_name("odd"), true);
        sidebar.folds.set(&session_name("odd | name"), true);
        sidebar.sending(&session_name("odd | name"));
        sidebar.titled("odd | name | fish".to_owned());
        assert!(sidebar.folds.contains("odd"));
        assert!(!sidebar.folds.contains("odd | name"));
    }

    #[test]
    fn a_gone_pane_leaves_the_highlight_in_its_session() {
        let mut sidebar = two_sessions();
        sidebar.step(1);
        sidebar.heard(host(
            vec![session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")])],
            Vec::new(),
        ));
        assert_eq!(sidebar.cursor, Some(Place::Pane { session: listed("older", 10), pane: 1 }));
    }

    /// Focused, on fewer lines than its rows take, the sidebar scrolls them
    /// to keep the highlight on screen as it moves down, and back up.
    #[test]
    fn the_rows_scroll_to_keep_the_highlight_on_screen() {
        let mut sidebar = two_sessions();
        let down = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        // Two lines, between the title and the rule along the top and the
        // legend at the bottom.
        let shown = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 14)[3..5].to_vec();
        assert_eq!(
            shown(&mut sidebar),
            [" ▾ older ────────────────── ●1", "   ● \u{ec82} fold                 1m"]
        );
        sidebar.key(&down, moment(160_000));
        sidebar.key(&down, moment(160_000));
        assert_eq!(shown(&mut sidebar), [" ▾ newer ─────────────────────", "     \u{e795} w/b"]);
        sidebar.key(&up, moment(160_000));
        assert_eq!(
            shown(&mut sidebar),
            ["     \u{e6ae} nvim src/main.rs", " ▾ newer ─────────────────────"]
        );
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE }
    }

    /// A click at `column` and `row` on the sidebar drawn in `area`: what the
    /// left button asks as it lets go there, having asked nothing as it went
    /// down.
    fn click(sidebar: &mut Sidebar, column: u16, row: u16, area: Rect) -> Asked {
        let down = mouse(MouseEventKind::Down(MouseButton::Left), column, row);
        assert_eq!(sidebar.mouse(down, area), Asked::Nothing);
        sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), column, row), area)
    }

    #[test]
    fn a_click_goes_to_a_pane_and_folds_on_the_triangle() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 10);
        screen(&mut sidebar, false, area.width, area.height);
        let pane = Place::Pane { session: listed("older", 10), pane: 2 };
        assert_eq!(click(&mut sidebar, 10, 5, area), Asked::Go(pane));
        assert_eq!(click(&mut sidebar, 10, 3, area), Asked::Take);
        assert!(!sidebar.folds.contains("older"));
        assert_eq!(click(&mut sidebar, 1, 3, area), Asked::Take);
        assert!(sidebar.folds.contains("older"));
        let older = Place::Session(listed("older", 10));
        assert_eq!(click(&mut sidebar, 10, 3, area), Asked::Go(older));
    }

    /// A search with nothing typed keeps the folds, and the triangle folds as
    /// it does without one. Once something is typed, it is the name's.
    #[test]
    fn a_click_on_the_triangle_folds_until_a_search_is_typed() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 10);
        sidebar.key(&KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE), moment(160_000));
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 3, area), Asked::Take);
        assert!(sidebar.folds.contains("older"));
        assert_eq!(click(&mut sidebar, 1, 3, area), Asked::Take);
        assert!(!sidebar.folds.contains("older"));
        assert!(sidebar.search.is_some());
        type_in(&mut sidebar, "older");
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 1, 3, area), Asked::Take);
        assert!(!sidebar.folds.contains("older"));
    }

    /// A session zellij has yet to list the panes of stands in for them, as a
    /// folded one does, for a click as for `Enter`.
    #[test]
    fn a_click_on_a_session_with_no_panes_goes_there() {
        let mut sidebar = Sidebar::default();
        let mut starting = session("x", 10, Vec::new());
        starting.panes = None;
        sidebar.heard(host(vec![starting], Vec::new()));
        let area = Rect::new(0, 0, 32, 5);
        screen(&mut sidebar, false, area.width, area.height);
        let x = Place::Session(listed("x", 10));
        assert_eq!(click(&mut sidebar, 10, 3, area), Asked::Go(x.clone()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(x));
    }

    /// The notes at the bottom stand over the rows that would be there, which
    /// a click on them does not go to.
    #[test]
    fn a_click_on_the_notes_goes_nowhere() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 14);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert!(lines[5].starts_with("─ luneta ─"), "{lines:?}");
        assert_eq!(click(&mut sidebar, 10, 5, area), Asked::Take);
        let fold = Place::Pane { session: listed("older", 10), pane: 1 };
        assert_eq!(click(&mut sidebar, 10, 4, area), Asked::Go(fold));
    }

    #[test]
    fn the_session_is_the_one_zellij_titles_the_terminal_for() {
        let mut sidebar = Sidebar::default();
        sidebar.titled("odd | name | ~/w".to_owned());
        assert_eq!(sidebar.here(), Some("odd"));
        sidebar
            .heard(host(vec![session("odd | name", 1, vec![]), session("odd", 2, vec![])], vec![]));
        assert_eq!(sidebar.here(), Some("odd | name"));
        sidebar.titled("odd".to_owned());
        assert_eq!(sidebar.here(), Some("odd"));
        sidebar.titled("oddly | ~/w".to_owned());
        assert_eq!(sidebar.here(), Some("oddly"));
    }

    /// zellij sends a client no title as it comes back to a tab that had the
    /// same title before. So the session luneta sends its zellij to is the
    /// one it is in, until zellij titles the terminal once it is there; a
    /// title of the session it leaves can come on the way. A session zellij
    /// did not take it to leaves it in the one the title names.
    #[test]
    fn the_session_is_the_one_luneta_sent_zellij_to_until_a_title_after() {
        let mut sidebar = two_sessions();
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        let look = sidebar.went(&newer).unwrap();
        assert_eq!((sidebar.here(), look.session.as_str()), (Some("newer"), "newer"));
        assert_eq!(sidebar.going(), Some("newer"));
        sidebar.titled("older | ~/w/luneta/main".to_owned());
        assert_eq!(sidebar.here(), Some("newer"));
        let clients = vec![Client { id: 1, pane: Some(1) }];
        let before = Some(Before::Listed(vec![]));
        sidebar.looked(Looked { number: look.number, went: Some(1), clients, before });
        assert_eq!((sidebar.here(), sidebar.going()), (Some("newer"), None));
        sidebar.titled("older".to_owned());
        assert_eq!(sidebar.here(), Some("older"));

        let look = sidebar.went(&newer).unwrap();
        let stayed = sidebar.stayed(look.number).map(|look| look.session);
        assert_eq!(stayed, Some("older".to_owned()));
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), None));
    }

    /// luneta sends its zellij to another session, and then to a third
    /// before it is there. What comes back of the first go says nothing of
    /// the second: neither that the client is there, nor that it stayed.
    #[test]
    fn what_comes_back_of_an_earlier_go_says_nothing_of_the_last() {
        let mut sidebar = two_sessions();
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        let older = Place::Pane { session: listed("older", 10), pane: 2 };
        let first = sidebar.went(&newer).unwrap();
        let last = sidebar.went(&older).unwrap();
        let clients = vec![Client { id: 1, pane: Some(1) }];
        let came = |look: &Look| Looked {
            number: look.number,
            went: look.went,
            clients: clients.clone(),
            before: Some(Before::Listed(vec![])),
        };
        sidebar.looked(came(&first));
        assert_eq!(sidebar.stayed(first.number), None);
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), Some("older")));
        sidebar.looked(came(&last));
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), None));
    }

    /// A shell at its prompt goes by its tab, if someone named it, as an agent
    /// does, and else by its directory; a program by its command, whatever the
    /// tab.
    #[test]
    fn a_pane_is_named_for_what_runs_in_it() {
        let named = |command: &str| label(None, &pane(3, 0, "/w/luneta/main", command));
        assert_eq!(named("-fish"), "luneta/main");
        assert_eq!(named("/bin/zsh -l"), "luneta/main");
        assert_eq!(named("cargo test"), "cargo test");
        assert_eq!(named("/nix/store/a1p5-neovim/bin/nvim --clean"), "nvim --clean");
        let unsaid = Pane { cwd: None, command: None, ..pane(3, 0, "", "") };
        assert_eq!(label(None, &unsaid), "pane 3");
        let tabbed = |pane: Pane| label(None, &Pane { tab_name: "review".to_owned(), ..pane });
        assert_eq!(tabbed(pane(3, 0, "/w/luneta/main", "-fish")), "review");
        assert_eq!(tabbed(pane(3, 0, "/w/luneta/main", "cargo test")), "cargo test");
        assert_eq!(tabbed(unsaid), "review");
    }

    /// An agent goes by the name someone gave it, then by its topic, the title
    /// Claude Code gives its pane, then by its tab if someone named it, then
    /// by where it works: where its pane works, or else, while zellij has not
    /// said, where Claude Code says. An agent's title with no topic, as a bare
    /// `Claude Code`, is passed over, and so is zellij's `Tab #N`.
    #[test]
    fn an_agent_goes_by_its_name_then_its_topic_then_its_tab_then_its_directory() {
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
        assert_eq!(label(Some(&unnamed), &titled(None, "review")), "review");
        assert_eq!(label(Some(&unnamed), &untitled("review")), "review");
        assert_eq!(label(Some(&unnamed), &titled(None, "Tab #12")), "w/x");
        assert_eq!(label(Some(&unnamed), &untitled(" ")), "w/x");
        let unsaid = Pane { cwd: None, ..untitled("Tab #1") };
        assert_eq!(label(Some(&unnamed), &unsaid), "luneta/main");
        // A name Claude Code gave the session itself is none a row goes by.
        let mut derived = agent("s", 1, Status::Idle, Some("luneta-main"));
        let source = Some(NameSource::Derived.into());
        derived.name = derived.name.map(|name| api::Name { source, ..name });
        assert_eq!(label(Some(&derived), &topic), "Fix the fold");
    }

    /// The icon of what runs in a pane has a column of its own, so that every
    /// label starts at the same column, and a program with none leaves it
    /// blank. An agent's is Claude's, whatever its terminal has in front, and
    /// `sudo` gives way to what it runs, though the row goes by its command.
    /// A shell the row is named for the directory of has a terminal's.
    #[test]
    fn a_pane_s_icon_says_what_runs_in_it_in_a_column_of_its_own() {
        let mut sidebar = Sidebar::default();
        let sudo = "/nix/store/fdsygnh4wvr7g860ba3cwlcksn8v1pc5-sudo-1.9.17p2/bin/sudo -k nvim x";
        sidebar.heard(host(
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
        let lines = screen(&mut sidebar, false, 32, 8);
        assert_eq!(
            lines[4..],
            [
                "  •✓ \u{ec82} fold                 1m",
                "     \u{e6ae} sudo -k nvim x",
                "       make",
                "     \u{e795} w/d",
            ]
        );
    }

    /// A word an agent says that luneta does not know is marked by its
    /// initial, as a capital, in the one column a mark takes, so that its
    /// label starts where every other does. An initial that takes another
    /// number of columns marks it as one that says nothing.
    #[test]
    fn a_word_an_agent_says_is_marked_in_the_one_column_a_mark_takes() {
        let words = ["compacting", "ßa", "日本", "\u{301}a"];
        let says = |pane: u32, word: &str| Agent {
            status: Some(Reported::OtherStatus(word.to_owned())),
            ..agent("s", pane, Status::Idle, Some("x"))
        };
        let mut sidebar = Sidebar::default();
        let panes = (1..=4).map(|id| pane(id, id, "/w/a", "claude")).collect();
        let agents = words.into_iter().zip(1..).map(|(word, id)| says(id, word)).collect();
        sidebar.heard(host(vec![session("s", 10, panes)], agents));
        // Up to the label's first letter, a character to a column.
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 8);
        assert_eq!(
            lines[4..].iter().map(start).collect::<Vec<_>>(),
            ["   C \u{ec82} x", "   S \u{ec82} x", "   ? \u{ec82} x", "   ? \u{ec82} x"]
        );
    }

    /// A pane with no agent in it is dimmed, its icon with its label.
    #[test]
    fn a_pane_with_no_agent_in_it_is_dimmed() {
        let mut sidebar = two_sessions();
        let buffer = drawn(&mut sidebar, false, 32, 7);
        // The icon and the label's first letter, on the agent's row and on
        // nvim's under it.
        let dim = |y: u16| [5_u16, 7].map(|x| buffer[(x, y)].modifier.contains(Modifier::DIM));
        assert_eq!((dim(4), dim(5)), ([false, false], [true, true]));
    }

    /// The daemon cannot follow an agent to the session it is in when the
    /// session was renamed before the daemon started. Its row is the pane of
    /// its id that works where it does, in the one session that lists one.
    #[test]
    fn an_agent_the_daemon_cannot_follow_sits_where_its_pane_works() {
        let mut lost = agent("gone", 1, Status::Waiting, Some("fold"));
        lost.zellij = Some(Zellij { named: "gone".to_owned(), session: None, pane: 1 });
        let sessions = [
            session("renamed", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            session("other", 20, vec![pane(1, 0, "/w/elsewhere", "fish")]),
        ];
        let agents = [lost];
        let held = seated(&agents, &sessions);
        assert_eq!(held.keys().collect::<Vec<_>>(), [&("renamed", 1)]);

        let mut sidebar = Sidebar::default();
        sidebar.heard(host(sessions.to_vec(), agents.to_vec()));
        let lines = screen(&mut sidebar, false, 32, 5);
        assert_eq!(lines[4], "   ● \u{ec82} fold                 1m");

        // Two sessions list a pane of its id where it works, and the agent
        // runs inside a shell: the one whose pane it titled holds it.
        let titled = Pane {
            titled: Some(api::Titled { topic: None }),
            ..pane(1, 0, "/w/luneta/main", "fish")
        };
        let sessions = [
            session("renamed", 10, vec![titled]),
            session("other", 20, vec![pane(1, 0, "/w/luneta/main", "fish")]),
        ];
        let held = seated(&agents, &sessions);
        assert_eq!(held.keys().collect::<Vec<_>>(), [&("renamed", 1)]);
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
                .get(&("s", 1))
                .and_then(|agent| agent.name.as_ref())
                .map(|name| name.text.as_str()),
            Some("prompt")
        );
    }

    /// A pane at its prompt, in `/w/luneta/main`, whose shell said `doing`,
    /// in a tab of its own.
    fn in_shell(id: u32, doing: Doing) -> Pane {
        Pane { shell: Some(Shell { doing: Some(doing) }), ..pane(id, id, "/w/luneta/main", "fish") }
    }

    fn going(command: &str, since: i64, front: Front) -> Doing {
        let since = Some(Timestamp { seconds: since, nanos: 0 });
        Doing::Going(Going { command: command.to_owned(), since, front: front.into() })
    }

    fn ended(command: &str, status: Option<i32>, millis: i32) -> Doing {
        let (seconds, nanos) = ((millis / 1000).into(), millis % 1000 * 1_000_000);
        let took = Some(prost_types::Duration { seconds, nanos });
        let at = Some(Timestamp { seconds: 130, nanos: 0 });
        Doing::Ended(Ended { command: command.to_owned(), at, status, took })
    }

    /// A command in a shell has an agent's marks, its row named for it: the
    /// spinner while it works, the dot of an agent that waits for you while
    /// it waits for you to type, and once it ends, how it ended, with the dot
    /// of what you have yet to see. The rule along the top counts them with
    /// the agents. One that takes the terminal
    /// over has no mark while you use it, nor has a job stopped, nor one that
    /// ended within a second. The hook counts whole seconds, and a command is
    /// dated to the last moment of its second. The icon goes by what zellij
    /// says runs in the pane, as for any other: here a shell's, `\u{e795}`,
    /// and neovim's, `\u{e6ae}`.
    #[test]
    fn a_command_has_an_agent_s_marks_and_its_row_is_named_for_it() {
        let mut sidebar = Sidebar::default();
        let stopped = Doing::Stopped(Stopped {
            command: "make".to_owned(),
            at: Some(Timestamp { seconds: 130, nanos: 0 }),
        });
        let nvim = in_shell(3, going("nvim notes", 100, Front::Interactive));
        let nvim = Pane { command: Some("nvim notes".to_owned()), ..nvim };
        sidebar.heard(host(
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
                ],
            )],
            Vec::new(),
        ));
        assert_eq!(
            screen(&mut sidebar, false, 32, 12),
            [
                "─ luneta ──────────────────────",
                "─ 1 session ──────── ●1 •3 ⠋1 ─",
                "",
                " ▾ s ────────────────────── ●1",
                "   ⠋ \u{e795} cargo build --rele… 59s",
                "   ● \u{e795} sudo -k true        59s",
                "     \u{e6ae} nvim notes",
                "  •✓ \u{e795} cargo test          29s",
                "  •✗ \u{e795} cargo clippy        29s",
                "  •$ \u{e795} cargo run           29s",
                "     \u{e795} luneta/main",
                "     \u{e795} luneta/main",
            ]
        );
        assert!(sidebar.turning());
    }

    fn moment(millis: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(millis)
    }

    /// luneta's own clock as the host's says `now`: as long after one moment
    /// of it as `now` is after the epoch.
    fn steady(now: SystemTime) -> Instant {
        static START: LazyLock<Instant> = LazyLock::new(Instant::now);
        *START + now.duration_since(SystemTime::UNIX_EPOCH).unwrap()
    }

    /// Time went by, to `moment(millis)` by both clocks, with luneta's window
    /// focused.
    fn tick(sidebar: &mut Sidebar, millis: u64) {
        sidebar.ticked(moment(millis), steady(moment(millis)), true);
    }

    /// Whether the cell on line `y` at column `x` is dimmed, drawn at `now`.
    fn dimmed(sidebar: &mut Sidebar, x: u16, y: u16, now: SystemTime) -> bool {
        drawn_at(sidebar, false, 32, 7, now)[(x, y)].modifier.contains(Modifier::DIM)
    }

    /// The picker gets an agent's directory as luneta shows one, its last two
    /// parts: its pane's, or else, while zellij has not said where the pane
    /// works, the agent's own.
    #[test]
    fn the_picker_gets_an_agents_directory_as_its_last_two_parts() {
        let mut sidebar = Sidebar::default();
        let unsaid = Pane { cwd: None, ..pane(2, 0, "", "claude") };
        sidebar.heard(host(
            vec![session("s", 10, vec![pane(1, 0, "/home/me/src/luneta/pr-12", "claude"), unsaid])],
            vec![agent("s", 1, Status::Idle, None), agent("s", 2, Status::Idle, None)],
        ));
        let agents = sidebar.listing().agents;
        let dirs: Vec<&str> = agents.iter().map(|seated| seated.dir.as_str()).collect();
        assert_eq!(dirs, ["luneta/pr-12", "luneta/main"]);
    }

    /// An agent that finished, and a command that ended, have the dot until
    /// luneta's zellij stays on their panes for a second, a folded session
    /// the dot of one of its panes, and the rule along the top counts them.
    /// Seen, the agent's `✓` is dimmed, and the command's row goes back to
    /// its shell; the picker marks the agent as its row does. On the pane
    /// luneta's zellij is on, an end is no news: you see it. A pane it only
    /// passed through keeps the dot.
    #[test]
    fn what_finished_has_the_dot_until_you_stay_in_its_pane() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
            vec![session(
                "s",
                10,
                vec![
                    pane(1, 0, "/w/luneta/main", "claude"),
                    in_shell(2, ended("cargo test", Some(0), 5_000)),
                    pane(3, 3, "/w/a", "fish"),
                ],
            )],
            vec![agent("s", 1, Status::Idle, Some("fold"))],
        ));
        sidebar.titled("s | fish".to_owned());
        let lines = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, false, 32, 7, now).0
        };
        assert_eq!(
            lines(&mut sidebar, moment(160_000)),
            [
                "─ luneta ──────────────────────",
                "─ 1 session ────────────── •2 ─",
                "",
                " ▾ s ─────────────────────────",
                "  •✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          29s",
                "     \u{e795} w/a",
            ]
        );
        assert!(!dimmed(&mut sidebar, 3, 4, moment(160_000)));
        assert_eq!(sidebar.listing().agents[0].mark, Mark::Finished);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        assert!(lines(&mut sidebar, moment(160_000))[3].ends_with(" •"));
        sidebar.key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), moment(160_000));

        let s = |pane| Place::Pane { session: listed("s", 10), pane };
        sidebar.went(&s(2));
        assert_eq!(lines(&mut sidebar, moment(160_000))[5], "▌    \u{e795} luneta/main");
        sidebar.went(&s(1));
        lines(&mut sidebar, moment(160_500));
        lines(&mut sidebar, moment(161_500));
        sidebar.went(&s(3));
        assert_eq!(
            lines(&mut sidebar, moment(162_000))[..6],
            [
                "─ luneta ──────────────────────",
                "─ 1 session ────────────── •1 ─",
                "",
                " ▾ s ─────────────────────────",
                "   ✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          31s",
            ]
        );
        assert!(dimmed(&mut sidebar, 3, 4, moment(162_000)));
        assert_eq!(sidebar.listing().agents[0].mark, Mark::Idle);

        sidebar.went(&s(2));
        lines(&mut sidebar, moment(163_000));
        lines(&mut sidebar, moment(164_000));
        sidebar.went(&s(3));
        let seen = lines(&mut sidebar, moment(164_500));
        assert_eq!(seen[1], format!("─ 1 session {}", "─".repeat(19)));
        assert_eq!(seen[5], "     \u{e795} luneta/main");
    }

    /// `u` marks the highlighted pane unread, and gives its row the dot, with
    /// nothing new in it, and on the pane luneta's zellij is on, until it
    /// leaves and comes back for a second. `u` again takes the mark away.
    #[test]
    fn u_marks_a_pane_unread_until_you_come_back_to_it() {
        let mut sidebar = two_sessions();
        let older = |pane| Place::Pane { session: listed("older", 10), pane };
        let u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let nvim = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            let lines = seen_at(sidebar, true, 32, 12, now).0;
            lines.into_iter().find(|line| line.contains("nvim")).expect("nvim's row")
        };
        sidebar.went(&older(2));
        nvim(&mut sidebar, moment(160_000));
        sidebar.key(&u, moment(161_000));
        assert_eq!(nvim(&mut sidebar, moment(165_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        assert!(screen(&mut sidebar, true, 32, 7)[1].ends_with(" ●1 •1 ─"));

        sidebar.went(&older(1));
        nvim(&mut sidebar, moment(166_000));
        sidebar.went(&older(2));
        assert_eq!(nvim(&mut sidebar, moment(167_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌    \u{e6ae} nvim src/main.rs");

        sidebar.key(&u, moment(168_000));
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌ •  \u{e6ae} nvim src/main.rs");
        sidebar.key(&u, moment(168_000));
        assert_eq!(nvim(&mut sidebar, moment(168_000)), "▌    \u{e6ae} nvim src/main.rs");
    }

    /// The session `s` of the tests of the dot: an agent that came to `idle`
    /// at 100 in pane 1, and a shell in pane 3.
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

    /// The row of the agent of `idle_agent`, once the sidebar ticks at `now`
    /// with luneta's window `watched`.
    fn agent_row(sidebar: &mut Sidebar, now: SystemTime, watched: bool) -> String {
        sidebar.ticked(now, steady(now), watched);
        seen_at(sidebar, false, 32, 6, now).0[4].clone()
    }

    /// luneta's zellij on a pane while luneta's window does not have the
    /// focus is not you there: what finished there keeps the dot, until you
    /// look for a second.
    #[test]
    fn a_pane_is_yours_only_while_luneta_s_window_has_the_focus() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        let s = |pane| Place::Pane { session: listed("s", 10), pane };
        sidebar.went(&s(1));
        agent_row(&mut sidebar, moment(160_000), false);
        agent_row(&mut sidebar, moment(165_000), false);
        sidebar.went(&s(3));
        let new = "  •✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(166_000), true), new);

        sidebar.went(&s(1));
        agent_row(&mut sidebar, moment(167_000), false);
        agent_row(&mut sidebar, moment(168_000), true);
        agent_row(&mut sidebar, moment(169_000), true);
        sidebar.went(&s(3));
        let seen = "   ✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(170_000), true), seen);
    }

    /// `u` on a row with the dot marks its pane read, as going there would,
    /// whatever gave it the dot: the agent's `✓` is dimmed. `u` again marks
    /// it unread, and the dot comes back, and once more takes it away.
    #[test]
    fn u_marks_a_pane_with_the_dot_read() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        sidebar.went(&Place::Pane { session: listed("s", 10), pane: 3 });
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let row = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, true, 32, 15, now).0[4].clone()
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
        assert!(dimmed(&mut sidebar, 3, 4, moment(161_000)));
    }

    /// Two sessions whose servers made their sockets in one tick of the
    /// clock, and so started at one moment, keep what you saw in each apart:
    /// their sockets tell them apart.
    #[test]
    fn sessions_that_started_at_once_keep_their_dots_apart() {
        let mut sidebar = Sidebar::default();
        let claude = || vec![pane(1, 0, "/w/luneta/main", "claude")];
        let mut twin = session("b", 10, claude());
        twin.socket = 11;
        let mut there = agent("b", 1, Status::Idle, Some("fold"));
        there.pid = 2;
        sidebar.heard(host(
            vec![session("a", 10, claude()), twin],
            vec![agent("a", 1, Status::Idle, Some("fold")), there],
        ));
        sidebar.titled("a | claude".to_owned());
        sidebar.went(&Place::Pane { session: listed("a", 10), pane: 1 });
        let lines = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, false, 32, 7, now).0
        };
        lines(&mut sidebar, moment(160_000));
        let lines = lines(&mut sidebar, moment(161_000));
        assert_eq!(
            [&lines[4], &lines[6]],
            ["▌  ✓ \u{ec82} fold                 1m", "  •✓ \u{ec82} fold                 1m"]
        );
    }

    /// A session the daemon leaves out of a list, as a rename made while it
    /// reads zellij's folder can, keeps what you saw in it. One it has not
    /// listed for `GONE` has it no more, should it come back.
    #[test]
    fn a_session_left_out_of_a_list_keeps_what_you_saw_until_it_is_gone() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        let s = |pane| Place::Pane { session: listed("s", 10), pane };
        sidebar.went(&s(1));
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 161_000);
        sidebar.went(&s(3));
        let seen = "   ✓ \u{ec82} fold                 1m";
        let new = "  •✓ \u{ec82} fold                 1m";
        for (left, back, row) in [(163_000, 172_000, seen), (173_000, 183_000, new)] {
            sidebar.heard(host(Vec::new(), Vec::new()));
            tick(&mut sidebar, left);
            tick(&mut sidebar, back);
            sidebar.heard(idle_agent());
            assert_eq!(agent_row(&mut sidebar, moment(back + 500), true), row);
        }
    }

    /// A list the daemon may have left a session out of takes nothing of
    /// what you saw in it, however long it stands: what it said last, while
    /// it does not answer, as a session may have started since; one it
    /// could not read all of zellij's folder for; and one from a daemon older
    /// than the count, which does not say whether it could.
    #[test]
    fn a_session_a_list_falls_short_of_keeps_what_you_saw() {
        let short: [fn(&mut Sidebar); 3] = [
            |sidebar| {
                sidebar.heard(host(Vec::new(), Vec::new()));
                sidebar.lost("lunetad does not answer".to_owned());
            },
            |sidebar| {
                sidebar.heard(Host { unread_sockets: Some(1), ..host(Vec::new(), Vec::new()) });
            },
            |sidebar| {
                sidebar.heard(Host { unread_sockets: None, ..host(Vec::new(), Vec::new()) });
            },
        ];
        for short in short {
            let mut sidebar = Sidebar::default();
            sidebar.heard(idle_agent());
            sidebar.titled("s | fish".to_owned());
            let s = |pane| Place::Pane { session: listed("s", 10), pane };
            sidebar.went(&s(1));
            tick(&mut sidebar, 160_000);
            tick(&mut sidebar, 161_000);
            sidebar.went(&s(3));
            short(&mut sidebar);
            tick(&mut sidebar, 163_000);
            tick(&mut sidebar, 183_000);
            sidebar.heard(idle_agent());
            let seen = "   ✓ \u{ec82} fold                 1m";
            assert_eq!(agent_row(&mut sidebar, moment(183_500), true), seen);
        }
    }

    /// A command shows once it has run for a second, with no word from the
    /// daemon since it started.
    #[test]
    fn a_command_shows_once_it_has_run_a_second() {
        let mut sidebar = Sidebar::default();
        let make = in_shell(1, going("make", 159, Front::Working));
        sidebar.heard(host(vec![session("s", 10, vec![make])], Vec::new()));
        let at = |millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
        let row = |sidebar: &mut Sidebar, now| seen_at(sidebar, false, 32, 5, now).0[4].clone();
        assert_eq!(row(&mut sidebar, at(160_500)), "     \u{e795} luneta/main");
        assert!(!sidebar.turning());
        assert_eq!(row(&mut sidebar, at(161_000)), "   ⠋ \u{e795} make                 1s");
        assert!(sidebar.turning());
    }

    /// Starship's `git_status`, with the defaults.
    #[test]
    fn the_work_tree_shows_what_starship_shows() {
        let changes =
            Changes { conflicted: 1, deleted: 1, renamed: 1, modified: 1, staged: 1, untracked: 1 };
        let both = Some(Upstream { ahead: 1, behind: 2 });
        assert_eq!(glyphs(&changes, 1, both), "=$✘»!+?⇕");
        assert_eq!(glyphs(&Changes::default(), 0, Some(Upstream { ahead: 0, behind: 2 })), "⇣");
        assert_eq!(glyphs(&Changes::default(), 0, None), "");
    }

    /// A branch's pull request follows it, in the colour gh lists its state
    /// in, and a branch too long for both gives way to the number and to how
    /// the work tree stands. A closed one shows nothing.
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
            let head = Head::Branch { name, upstream: None };
            let changes = Changes { modified, ..Changes::default() };
            host.work_trees.push(WorkTree::Checkout { head, changes, stashes: 0 }.to_api(dir));
            let number = NonZeroU32::new(number).expect("a pull request's number");
            host.pull_requests.push(PullRequest { number, state }.to_api(dir));
        }
        let mut sidebar = Sidebar::default();
        sidebar.heard(host);

        let buffer = drawn(&mut sidebar, false, 32, 12);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let number = |y: u16| (0..32).find(|x| buffer[(*x, y)].symbol() == "#");
        let colour = |y: u16| number(y).map(|x| buffer[(x, y)].fg);
        let branches: Vec<String> = [5, 7, 9, 11].into_iter().map(line).collect();
        assert_eq!(
            branches,
            [
                "       main #126",
                "       handoff-eng-11… #4321 !",
                "       fold #7",
                "       old",
            ]
        );
        assert_eq!(colour(5), Some(gruvbox::GREEN));
        assert_eq!(colour(7), Some(gruvbox::PURPLE));
        assert_eq!(colour(9), Some(gruvbox::GREY));
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

    /// Sessions to search, each pane named for what it runs.
    fn searched() -> Sidebar {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
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

    /// Types `text` into the sidebar, a key for each character.
    fn type_in(sidebar: &mut Sidebar, text: &str) {
        for c in text.chars() {
            sidebar.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), moment(160_000));
        }
    }

    /// The lines of the rows the sidebar shows, focused, with no rule after a
    /// session's name: those down to the blank above the legend.
    fn rows_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let lines = screen(sidebar, true, 32, 24);
        let rows = lines[HEADER..].iter().take_while(|line| !line.is_empty());
        rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect()
    }

    /// The last line of the sidebar, focused: the search, or else the last of
    /// the legend.
    fn bottom(sidebar: &mut Sidebar) -> String {
        screen(sidebar, true, 32, 16)[15].clone()
    }

    /// A session found by its name keeps all its panes, the highlight on the
    /// one of them that matches best, or else on the first, though the name
    /// matches better. The rule along the top counts what the search hides.
    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        let bipa = Place::Pane { session: listed("bipa", 10), pane: 3 };
        let notes = Place::Pane { session: listed("notes", 20), pane: 1 };
        let zed = Place::Pane { session: listed("zed", 30), pane: 2 };
        for (typed, rows, best) in [
            ("pix", &[" ▾ bipa", "       a-p-i-x", "       pix outflow"][..], bipa),
            ("note", &[" ▾ notes", "     \u{e795} me/Documents"][..], notes),
            ("zed", &[" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs"][..], zed),
        ] {
            let mut sidebar = searched();
            type_in(&mut sidebar, &format!("/{typed}"));
            assert_eq!(rows_shown(&mut sidebar), rows, "{typed}");
            let rule = screen(&mut sidebar, true, 32, 16)[1].clone();
            assert!(rule.starts_with("─ 3 sessions ─"), "{typed}: {rule}");
            let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(best), "{typed}");
        }
    }

    /// A tab zellij named, and the parts of a directory a row leaves out,
    /// find nothing. An agent's pane zellij has not said the directory of is
    /// found by where the agent works.
    #[test]
    fn a_pane_is_found_by_its_branch_its_named_tab_and_its_directory() {
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
        let head = Head::Branch { name, upstream: None };
        let tree = WorkTree::Checkout { head, changes: Changes::default(), stashes: 0 };
        host.work_trees.push(tree.to_api("/w/c"));
        let found = |typed: &str| {
            let mut sidebar = Sidebar::default();
            sidebar.heard(host.clone());
            type_in(&mut sidebar, &format!("/{typed}"));
            rows_shown(&mut sidebar)
        };
        assert_eq!(found("notes"), [" ▾ s", "     \u{e6ae} nvim a"]);
        assert_eq!(found("luneta"), [" ▾ s", "     \u{e6ae} nvim b"]);
        assert_eq!(found("sidebar"), [" ▾ s", "     \u{e6ae} nvim c", "       feature/sidebar"]);
        let seated = found("fold/review");
        assert!(seated.len() == 2 && seated[1].contains("Rename the rows"), "{seated:?}");
        assert_eq!(found("tab"), [" no match"]);
        assert_eq!(found("loren"), [" no match"]);
    }

    /// Closed by `Esc`, by `Enter` going to the row, or by zellij taking the
    /// keys.
    #[test]
    fn a_search_opens_a_folded_session_until_it_closes() {
        let keys = " b  Brief ▸";
        for close in [Some(KeyCode::Esc), Some(KeyCode::Enter), None] {
            let mut sidebar = searched();
            sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
            type_in(&mut sidebar, "/pix");
            let found = [" ▾ bipa", "       a-p-i-x", "       pix outflow"];
            assert_eq!(rows_shown(&mut sidebar), found, "{close:?}");
            let asked = if let Some(code) = close {
                sidebar.key(&KeyEvent::new(code, KeyModifiers::NONE), moment(160_000))
            } else {
                sidebar.focus(Keys::Zellij);
                Asked::Nothing
            };
            let pix = Place::Pane { session: listed("bipa", 10), pane: 3 };
            let went = if close == Some(KeyCode::Enter) { Asked::Go(pix) } else { Asked::Nothing };
            assert_eq!(asked, went, "{close:?}");
            assert_eq!(rows_shown(&mut sidebar)[..2], [" ▸ bipa", " ▾ notes"], "{close:?}");
            assert_eq!(bottom(&mut sidebar), keys, "{close:?}");
        }
    }

    /// A search goes to a pane of the folded session luneta's zellij is in,
    /// which unfolds as it goes. Folded there, the session stays so while
    /// luneta's zellij does.
    #[test]
    fn going_to_a_pane_found_in_luneta_s_folded_session_unfolds_it() {
        let mut sidebar = searched();
        sidebar.titled("bipa | a-p-i-x".to_owned());
        sidebar.key(&char('h'), moment(160_000));
        sidebar.titled("bipa | dda-bank-slip".to_owned());
        assert_eq!(rows_shown(&mut sidebar)[..2], [" ▸ bipa", " ▾ notes"]);
        type_in(&mut sidebar, "/pix");
        let pix = Place::Pane { session: listed("bipa", 10), pane: 3 };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(pix.clone()));
        sidebar.went(&pix);
        assert_eq!(rows_shown(&mut sidebar)[..2], [" ▾ bipa", "       a-p-i-x"]);
    }

    /// Every character types, `j` and `h` too, where the keys were. A long
    /// search shows its end. `Backspace` takes a character back, and closes
    /// the search once there is none.
    #[test]
    fn what_is_typed_shows_where_the_keys_were() {
        let mut sidebar = searched();
        let backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE);
        type_in(&mut sidebar, "/pixjh");
        assert_eq!(bottom(&mut sidebar), " /pixjh_");
        assert_eq!(rows_shown(&mut sidebar), [" no match"]);
        sidebar.key(&backspace, moment(160_000));
        sidebar.key(&backspace, moment(160_000));
        assert_eq!(rows_shown(&mut sidebar), [" ▾ bipa", "       a-p-i-x", "       pix outflow"]);
        for _ in 0..4 {
            sidebar.key(&backspace, moment(160_000));
        }
        assert_eq!(bottom(&mut sidebar), " b  Brief ▸");
        type_in(&mut sidebar, "/a-very-long-search-for-something");
        assert_eq!(bottom(&mut sidebar), " /…y-long-search-for-something_");
    }

    fn char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// A key of the menu opens it at the bottom, for the highlighted row, and
    /// what it opens is where luneta goes, once zellij says what it opened. A
    /// tab of another session opens once luneta's zellij is there, whatever
    /// lunetad says of its clients: the last may have left since.
    #[test]
    fn a_key_of_the_menu_opens_it_for_the_highlighted_row() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[11..], [
            "─ New ─────────── luneta/main ─",
            " p  new pane",
            " t  new tab",
            " s  new session",
            " <BS> - Back, <ESC> - Close",
        ]);
        let (older, cwd) = ("older".to_owned(), "/w/luneta/main".to_owned());
        let tab = Open::Tab { session: older, cwd, name: None, command: Vec::new() };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), Asked::Open { menu: 1, open: tab });
        let opened = Place::Pane { session: listed("older", 10), pane: 7 };
        assert_eq!(sidebar.opened(1, Ok(7)), Asked::Go(opened));
        // What zellij answers a menu that is gone takes luneta nowhere.
        assert_eq!(sidebar.opened(1, Ok(8)), Asked::Nothing);

        let client = Client { id: 9, pane: None };
        if let Some(host) = &mut sidebar.host {
            host.sessions[0].clients = Some(Clients { clients: vec![client] });
        }
        sidebar.step(2);
        type_in(&mut sidebar, "n");
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        let (session, cwd) = ("newer".to_owned(), "/w/b".to_owned());
        let tab = Open::Tab { session, cwd, name: None, command: Vec::new() };
        let asked = Asked::OpenThere { menu: 2, place: newer, open: tab };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), asked);
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    /// The menu floats over zellij for the pane luneta's zellij is on,
    /// wherever the highlight is, and in a folded session too, and the
    /// sidebar stands meanwhile as it does under a brief. Its keys open beside
    /// that pane, where it works and for the branch checked out there, and
    /// what they open is where luneta goes, as for the menu at the bottom.
    /// None floats until the sidebar knows which pane zellij is on, and it
    /// says why. It closes with its box.
    #[test]
    fn the_menu_floats_over_zellij_for_the_pane_zellij_is_on() {
        let mut sidebar = Sidebar::default();
        let mut host = host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        );
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/menu".to_owned(),
            upstream: None,
        }));
        let checkout = api::Checkout { head, changes: Some(api::Changes::default()), stashes: 0 };
        host.work_trees = vec![api::WorkTree {
            dir: "/w/luneta/main".to_owned(),
            stands: Some(api::work_tree::Stands::Checkout(checkout)),
        }];
        sidebar.heard(host);
        let look = sidebar.titled("older | fold".to_owned()).expect("a look");
        assert_eq!(sidebar.float_menu(), Err(NO_HERE_PANE));
        assert!(sidebar.floating().is_none());
        let clients = vec![Client { id: 1, pane: Some(1) }];
        sidebar.looked(Looked { number: look.number, went: None, clients, before: None });

        // The highlight on the newer session's pane, and the older folded.
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "hjj");
        assert!(sidebar.folds.contains("older"));
        assert_eq!(sidebar.cursor, Some(Place::Pane { session: listed("newer", 20), pane: 1 }));
        // The sidebar as it stands, the keys where they are.
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
        assert_eq!(target, Some(Target {
            place: Place::Pane { session: listed("older", 10), pane: 1 },
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/menu".to_owned()),
        }));

        let (older, cwd) = ("older".to_owned(), "/w/luneta/main".to_owned());
        let tab = Open::Tab { session: older.clone(), cwd: cwd.clone(), name: None, command: vec![] };
        type_in(&mut sidebar, "n");
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), Asked::Open { menu: 1, open: tab });
        let opened = Place::Pane { session: listed("older", 10), pane: 7 };
        assert_eq!(sidebar.opened(1, Ok(7)), Asked::Go(opened));
        assert!(sidebar.floating().is_none());
        sidebar.focus(Keys::Zellij);

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "c");
        let command = ["claude", "--name", "thermo-nuclear-review: feat/menu", "/thermo-nuclear-review"];
        let command = command.map(str::to_owned).to_vec();
        let pane = Open::Pane { session: older, beside: 1, cwd, command };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), Asked::Open { menu: 2, open: pane });
        // zellij has the keys again, which the sidebar never held meanwhile.
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    /// What goes wrong with what the menu over zellij does is said over
    /// zellij, where you look, and the sidebar collapsed has no room for:
    /// luneta takes it once, and it stays out of the notes, but for what
    /// luneta did not take as the box gave up the keys. The menu at the bottom
    /// says it in the notes. What went wrong before goes from the notes as the
    /// menu floats, and one that cannot float says why.
    #[test]
    fn the_menu_over_zellij_says_over_zellij_what_went_wrong() {
        let mut sidebar = Sidebar::default();
        let unknown = Pane { cwd: None, ..pane(1, 0, "/w/b", "fish") };
        sidebar.heard(host(vec![session("only", 10, vec![unknown])], Vec::new()));
        let look = sidebar.titled("only | fish".to_owned()).expect("a look");
        let clients = vec![Client { id: 1, pane: Some(1) }];
        sidebar.looked(Looked { number: look.number, went: None, clients, before: None });
        let wrong = "menu: luneta does not know yet where pane 1 works; try again in a moment";

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "np");
        assert!(sidebar.floating().is_none());
        assert_eq!(sidebar.said().as_deref(), Some(wrong));
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, None));
        sidebar.focus(Keys::Zellij);

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "np");
        sidebar.focus(Keys::Zellij);
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, Some(wrong)));

        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "np");
        assert_eq!((sidebar.said(), sidebar.notice.as_deref()), (None, Some(wrong)));
        assert_eq!(sidebar.float_menu(), Ok(()));
        assert!(sidebar.notice.is_none());

        let mut sidebar = Sidebar::default();
        let twin = || session("twin", 10, vec![pane(1, 0, "/w/b", "fish")]);
        sidebar.heard(host(vec![twin(), Session { socket: 11, ..twin() }], Vec::new()));
        let look = sidebar.titled("twin | fish".to_owned()).expect("a look");
        let clients = vec![Client { id: 1, pane: Some(1) }];
        sidebar.looked(Looked { number: look.number, went: None, clients, before: None });
        assert_eq!(sidebar.float_menu(), Err(TWIN_SESSIONS));
    }

    /// The Brief group's keys brief you by the row the menu was for: `b z` on
    /// every session, in the rows' order, as you moved them too, `b s` on the
    /// row's session, and `b h` on the agent in the row's pane, or on a
    /// session's row, on its session. An agent is told of by its row's name,
    /// its status and its branch, a folded session's too. With none there,
    /// the sidebar says so.
    #[test]
    fn the_brief_s_keys_brief_you_by_the_row_the_menu_was_for() {
        let mut sidebar = two_sessions();
        let mut host = sidebar.host.clone().expect("a host");
        let id = "0b7c5e1a-2f3d-4c6b-9a8e-1d2c3b4a5f60";
        host.agents[0].session_id = id.to_owned();
        let head = Some(api::checkout::Head::Branch(api::Branch {
            name: "feat/brief".to_owned(),
            upstream: None,
        }));
        let checkout = api::Checkout { head, changes: Some(api::Changes::default()), stashes: 0 };
        host.work_trees = vec![api::WorkTree {
            dir: "/w/luneta/main".to_owned(),
            stands: Some(api::work_tree::Stands::Checkout(checkout)),
        }];
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        let fold = Note {
            session: "older".to_owned(),
            pane: 1,
            name: "fold".to_owned(),
            status: "waiting".to_owned(),
            since: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(100)),
            branch: Some("feat/brief".to_owned()),
            cwd: "/w/luneta/main".to_owned(),
            transcript: SessionId::parse(id),
        };
        let brief = |over: Over| Asked::Brief(Brief::new(over, vec![fold.clone()]).unwrap());
        let every = || Over::Sessions(vec!["older".to_owned(), "newer".to_owned()]);
        let older = || Over::Session("older".to_owned());
        for (key, over) in [('z', every()), ('s', older()), ('h', Over::Agent("older".to_owned(), 1))]
        {
            type_in(&mut sidebar, "b");
            assert_eq!(sidebar.key(&char(key), moment(160_000)), brief(over), "{key}");
            assert!(sidebar.menu.is_none(), "{key}");
        }
        // nvim's pane has no agent, and the newer session none.
        sidebar.step(1);
        type_in(&mut sidebar, "bh");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agent in this pane"));
        sidebar.step(1);
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents in newer"));
        // Folded, the older session's row stands for its panes.
        sidebar.step(-2);
        type_in(&mut sidebar, "h");
        assert_eq!(sidebar.cursor, Some(Place::Session(listed("older", 10))));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), brief(older()));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(every()));
        // The older session moved under the newer one.
        type_in(&mut sidebar, "vJb");
        let moved = Over::Sessions(vec!["newer".to_owned(), "older".to_owned()]);
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(moved));
    }

    /// While lunetad does not answer, what it said last of the agents may be
    /// out of date; and while it cannot read one of Claude Code's session
    /// files, nor tell where its process runs, the file's agent may be in any
    /// pane. The sidebar says so at its bottom, and a delete asks first,
    /// whatever it ends, saying why, and `y` goes ahead.
    #[test]
    fn a_delete_asks_first_while_luneta_cannot_tell_the_agents() {
        let unread = "lunetad cannot read 1 of Claude Code's session files";
        let unplaced = format!("{unread}, nor tell where its agent runs");
        let away: fn(&mut Sidebar) = |sidebar| sidebar.lost("gone".to_owned());
        // As a daemon older than its list of the files says it.
        let unreadable: fn(&mut Sidebar) = |sidebar| {
            let host = sidebar.host.clone().expect("the sidebar has heard the host");
            sidebar.heard(Host { unread: 1, ..host });
        };
        let away = (away, "lunetad: gone", "lunetad does not answer");
        for (untold, note, why) in [away, (unreadable, unread, unplaced.as_str())] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            untold(&mut sidebar);
            sidebar.step(2);
            type_in(&mut sidebar, "d");
            let closes = || Asked::Closes { menu: 1, session: "newer".to_owned(), pane: 1 };
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), Asked::Nothing);
            let lines = screen(&mut sidebar, true, 160, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let asks = format!("Close the pane? luneta cannot tell which agents it ends: {why}");
            assert!(lines.contains(&note), "{lines:#?}");
            assert!(lines.contains(&asks.as_str()), "{lines:#?}");
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
            let act = Act::Pane { session: "newer".to_owned(), pane: 1 };
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), Asked::Delete(act));
        }
    }

    /// The agent of a session file lunetad cannot read is in the pane its
    /// process runs in, as lunetad says: what it is doing no one can tell,
    /// so a delete that ends it asks first, and one that does not goes at
    /// once. The sidebar says at its bottom that a file does not read.
    #[test]
    fn a_delete_asks_first_where_an_agent_s_session_file_does_not_read() {
        for (pane, asks) in [(1, true), (7, false)] {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            let host = sidebar.host.clone().expect("the sidebar has heard the host");
            let session = Some("newer".to_owned());
            let zellij = Zellij { named: "newer".to_owned(), session, pane };
            let unread_files = vec![UnreadFile { pid: 200, zellij: Some(zellij) }];
            sidebar.heard(Host { unread: 1, unread_files, ..host });
            sidebar.step(2);
            type_in(&mut sidebar, "d");
            let closes = Asked::Closes { menu: 1, session: "newer".to_owned(), pane: 1 };
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
            let act = Act::Pane { session: "newer".to_owned(), pane: 1 };
            let went = sidebar.closes(1, Ok(Closes::Pane));
            let lines = screen(&mut sidebar, true, 120, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let note = "lunetad cannot read 1 of Claude Code's session files";
            assert!(lines.contains(&note), "{lines:#?}");
            let asked = lines.contains(&"Close the pane? 1 agent, 1 unknown");
            assert_eq!((went == Asked::Delete(act), asked), (!asks, asks), "pane {pane}");
        }
    }

    /// `d d` plans by what zellij says closing the pane closes, and the
    /// highlight goes to the row after it once the pane is gone.
    #[test]
    fn a_delete_goes_by_what_zellij_closes_and_the_highlight_past_it() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.step(1);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        let closes = Asked::Closes { menu: 1, session: "older".to_owned(), pane: 2 };
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        let act = Act::Pane { session: "older".to_owned(), pane: 2 };
        assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), Asked::Delete(act));
        assert!(sidebar.menu.is_none());
        sidebar.heard(host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        ));
        assert_eq!(sidebar.cursor, Some(Place::Pane { session: listed("newer", 20), pane: 1 }));
    }

    /// A delete that would end an agent at work asks first, and `y` asks
    /// zellij again before it goes ahead. The session luneta's zellij is in
    /// goes once it is in the newest of the others.
    #[test]
    fn a_delete_that_ends_an_agent_at_work_asks_and_looks_again() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "d");
        let closes = || Asked::Closes { menu: 1, session: "older".to_owned(), pane: 1 };
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
        let tab = || Closes::Tab { id: 0, panes: vec![1] };
        assert_eq!(sidebar.closes(1, Ok(tab())), Asked::Nothing);
        assert_eq!(bottom(&mut sidebar), " <y> Yes <BS> Back <ESC> Close");
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13], " Close its tab? 1 agent, 1");
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
        let act = Act::Tab { session: "older".to_owned(), tab: 0, panes: vec![1] };
        assert_eq!(sidebar.closes(1, Ok(tab())), Asked::Delete(act));

        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13..15], [" Kill session older? 1 agent,", " 1 waiting"]);
        let to = Place::Session(listed("newer", 20));
        let leave = Act::Leave { session: "older".to_owned(), to };
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Delete(leave));
        assert!(sidebar.menu.is_none());
    }

    /// As luneta's zellij leaves the session a delete kills, the sidebar says
    /// where it goes, with the keys or without, until zellij titles it there
    /// or says it stayed. A client that ended on its way leaves nothing to
    /// say: the one that takes its place goes nowhere, whichever session it
    /// is in, and what zellij says late of the go changes nothing.
    #[test]
    fn the_sidebar_says_where_a_delete_goes_while_it_goes() {
        let going = |sidebar: &mut Sidebar, focused: bool| {
            let lines = screen(sidebar, focused, 32, 16);
            lines.iter().any(|line| line.contains("Going to newer; this session"))
        };
        let newer = Place::Session(listed("newer", 20));
        let mut sidebar = two_sessions();
        sidebar.leaving(&newer);
        assert!(going(&mut sidebar, true));
        assert!(going(&mut sidebar, false));
        sidebar.titled("newer | fish".to_owned());
        assert!(!going(&mut sidebar, false));

        let mut sidebar = two_sessions();
        let look = sidebar.leaving(&newer).expect("a look");
        sidebar.stayed(look.number);
        assert!(!going(&mut sidebar, false));

        let mut sidebar = two_sessions();
        let look = sidebar.leaving(&newer).expect("a look");
        let rejoined = sidebar.rejoining(&session_name("older"));
        assert!(!going(&mut sidebar, false));
        sidebar.stayed(look.number);
        let clients = vec![Client { id: 2, pane: Some(1) }];
        let before = Some(Before::Listed(vec![]));
        sidebar.looked(Looked { number: rejoined.number, went: None, clients, before });
        assert!(!going(&mut sidebar, false));
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), None));
    }

    /// While luneta's zellij is on its way to a session, it may be in that
    /// one or the one it leaves: a key that would kill either says so, and
    /// kills neither, rather than luneta's own with it, or wait for ever on a
    /// go that zellij would not be sent. Once it is there, it does.
    #[test]
    fn no_session_goes_while_luneta_s_zellij_is_on_its_way_to_one() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.sending(&session_name("newer"));
        let moving = |sidebar: &Sidebar| {
            sidebar.menu.is_none()
                && sidebar.notice.as_ref().is_some_and(|it| it.contains("which session it is in"))
        };
        let older = Place::Pane { session: listed("older", 10), pane: 1 };
        let newer = Place::Pane { session: listed("newer", 20), pane: 1 };
        for place in [older, newer.clone()] {
            sidebar.cursor = Some(place);
            type_in(&mut sidebar, "d");
            assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
            assert!(moving(&sidebar));
        }
        // Nor does its last pane close it.
        type_in(&mut sidebar, "d");
        let closes = Asked::Closes { menu: 3, session: "newer".to_owned(), pane: 1 };
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        assert_eq!(sidebar.closes(3, Ok(Closes::Session)), Asked::Nothing);
        assert!(moving(&sidebar));

        sidebar.titled("newer | fish".to_owned());
        type_in(&mut sidebar, "d");
        let to = Place::Session(listed("older", 10));
        let leave = Act::Leave { session: "newer".to_owned(), to };
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Delete(leave));
    }

    /// luneta's zellij may be in either session while it goes from one to
    /// the other, and is in the one it went to once zellij titles it there.
    /// A client that takes the place of one that ended is in its session or
    /// none, whatever the one before was in.
    #[test]
    fn luneta_s_zellij_may_be_in_either_session_while_on_its_way() {
        let mut sidebar = two_sessions();
        assert_eq!(sidebar.seats(), ["older"]);
        sidebar.sending(&session_name("newer"));
        assert_eq!(sidebar.seats(), ["older", "newer"]);
        sidebar.titled("newer | fish".to_owned());
        assert_eq!(sidebar.seats(), ["newer"]);
        sidebar.rejoining(&session_name("older"));
        assert_eq!(sidebar.seats(), ["older"]);
    }

    /// A client that takes the place of one that ended on its way to a
    /// session is one luneta has yet to find, in that session too: zellij
    /// sends it no title where the tab had the same one before, and luneta
    /// looks for it, and has it there once zellij lists it.
    #[test]
    fn a_client_that_rejoins_where_the_last_was_going_is_looked_for() {
        let mut sidebar = two_sessions();
        sidebar.went(&Place::Session(listed("newer", 20)));
        let look = sidebar.rejoining(&session_name("newer"));
        assert_eq!(look.session, "newer");
        let clients = vec![Client { id: 2, pane: Some(1) }];
        let before = Some(Before::Listed(vec![]));
        sidebar.looked(Looked { number: look.number, went: None, clients, before });
        assert_eq!((sidebar.here(), sidebar.going()), (Some("newer"), None));
    }

    /// The highlight goes to the best match as what is typed changes, and
    /// stays where it is moved to as the daemon says more.
    #[test]
    fn the_highlight_seeks_the_best_match_only_as_the_search_changes() {
        let mut sidebar = searched();
        let pane = |pane| Some(Place::Pane { session: listed("bipa", 10), pane });
        type_in(&mut sidebar, "/pi");
        assert_eq!(sidebar.cursor, pane(3));
        sidebar.key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), moment(160_000));
        sidebar.heard(sidebar.host.clone().expect("heard"));
        assert_eq!(sidebar.cursor, pane(1));
        type_in(&mut sidebar, "x");
        assert_eq!(sidebar.cursor, pane(3));
    }

    /// The rows the sidebar draws, focused, with no rule after a session's
    /// name, and which of them the highlight is on.
    fn drawn_rows(sidebar: &mut Sidebar) -> (Vec<String>, Vec<usize>) {
        let (lines, lit) = seen(sidebar, true, 32, 22);
        let rows = lines[HEADER..].iter().take_while(|line| !line.is_empty());
        let rows = rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect();
        (rows, lit.iter().map(|y| usize::from(*y) - HEADER).collect())
    }

    /// `v` selects the session of the highlighted row, `j` and `k` grow the
    /// selection from session to session, and `J` and `K` move what it holds,
    /// each session with its panes, no further than the ends, while it stays
    /// selected, whatever the mouse does with no button down. The order
    /// outlasts the daemon's next list. `Esc` only ends the selection, the
    /// highlight going on from its end to a row it stops on without one, and
    /// leaves the sidebar after. With no selection, `K` moves nothing.
    #[test]
    fn v_selects_sessions_that_shifted_j_and_k_move() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "vjjk");
        let (_, lit) = drawn_rows(&mut sidebar);
        assert_eq!(lit, (0..6).collect::<Vec<_>>());
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), Rect::new(0, 0, 32, 22));
        // Shift and all, as a terminal sends it as text.
        let shifted = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT);
        sidebar.key(&shifted('J'), moment(160_000));
        sidebar.key(&shifted('J'), moment(160_000));
        let moved = [" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs", " ▾ bipa"];
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[..5], moved);
        assert_eq!(rows[8], " ▾ notes");
        assert_eq!(lit, (4..10).collect::<Vec<_>>());
        sidebar.heard(searched().host.unwrap());
        assert_eq!(&drawn_rows(&mut sidebar).0[..5], moved);

        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert_eq!(drawn_rows(&mut sidebar).1, [9]);
        sidebar.key(&shifted('K'), moment(160_000));
        assert_eq!(&drawn_rows(&mut sidebar).0[..5], moved);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Leave);
    }

    /// `v` selects a folded session from its row, and any other key ends the
    /// selection and does what it does: `Enter` goes.
    #[test]
    fn a_folded_session_is_selected_from_its_row() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "GhvkK");
        let (rows, _) = drawn_rows(&mut sidebar);
        assert_eq!(rows[..4], [" ▾ notes", "     \u{e795} me/Documents", " ▸ zed", " ▾ bipa"]);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let fish = Place::Pane { session: listed("notes", 20), pane: 1 };
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Go(fish));
        assert!(matches!(sidebar.moving, Moving::Still));
    }

    /// A press and release on one line is a click, wherever the mouse went
    /// along it. A press on a pane's row lifts nothing. One on a session's
    /// row lifts the session once the mouse moves a line, and takes the
    /// focus: it shows where it lands, lit, with its panes, the others making
    /// room. Let go, it stays where it showed, for the daemon's next list too.
    #[test]
    fn a_session_dragged_lands_where_it_shows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 22);
        let (before, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        let mut act = |kind, column, row| sidebar.mouse(mouse(kind, column, row), area);
        assert_eq!(act(MouseEventKind::Down(left), 10, 4), Asked::Nothing);
        assert_eq!(act(MouseEventKind::Drag(left), 20, 4), Asked::Nothing);
        let api = Place::Pane { session: listed("bipa", 10), pane: 1 };
        assert_eq!(act(MouseEventKind::Up(left), 20, 4), Asked::Go(api));

        assert_eq!(act(MouseEventKind::Down(left), 10, 4), Asked::Nothing);
        assert_eq!(act(MouseEventKind::Drag(left), 10, 6), Asked::Nothing);
        assert_eq!(act(MouseEventKind::Up(left), 10, 6), Asked::Nothing);
        assert_eq!(drawn_rows(&mut sidebar).0, before);

        // notes's row, under bipa's, past zed's.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 7), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 12), area), Asked::Take);
        let last = [" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs", " ▾ notes"];
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[4..9], last);
        assert_eq!(lit, [8, 9]);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 12), area), Asked::Nothing);
        assert_eq!(sidebar.cursor, Some(Place::Pane { session: listed("notes", 20), pane: 1 }));
        sidebar.heard(searched().host.unwrap());
        assert_eq!(&drawn_rows(&mut sidebar).0[4..9], last);

        // zed's row, under bipa's, to the top.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 7), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 3), area);
        let (rows, _) = drawn_rows(&mut sidebar);
        assert_eq!(rows[..5], [" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs", " ▾ bipa"]);
    }

    /// A session lifted goes back where it was once let go off the sidebar,
    /// on `Esc`, and once the mouse moves with no button down, after a
    /// release luneta did not hear. Off the sidebar, it shows where it was.
    #[test]
    fn a_session_lifted_goes_back_off_the_sidebar_or_on_esc() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 22);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        // zed's row, to the top.
        let lift_zed = |sidebar: &mut Sidebar| {
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 9), area);
            sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
            assert_ne!(drawn_rows(sidebar).0, listed);
        };
        lift_zed(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 40, 3), area);
        assert_eq!(drawn_rows(&mut sidebar), (listed.clone(), (6..10).collect()));
        sidebar.mouse(mouse(MouseEventKind::Up(left), 40, 3), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_zed(&mut sidebar);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert!(!sidebar.pressed());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_zed(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 3), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 3), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    /// While the left button is down on the sidebar, it has the keys: `v`
    /// selects nothing under the session it lifts, nor `n` opens the menu.
    /// `Esc` lets go, so that the release is no click.
    #[test]
    fn the_left_button_down_has_the_keys() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        let area = Rect::new(0, 0, 32, 22);
        drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        // zed's row, to the top.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 9), area);
        type_in(&mut sidebar, "vn");
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 3), area);
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[0], " ▾ zed");
        assert_eq!(lit, [1]);
        assert!(sidebar.menu.is_none());

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 4), area);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area), Asked::Nothing);
    }

    /// What is typed in the search narrows the rows, and a press on a
    /// session's row lifts nothing.
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
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 6), area), Asked::Nothing);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    /// A session lifted and held on the last line of the rows scrolls them a
    /// line each `SCROLL`, as far as the last row, and on the first line,
    /// back.
    #[test]
    fn a_session_held_on_the_edge_scrolls_the_rows() {
        let mut sidebar = searched();
        // Four lines of rows, between the rule and the legend.
        let area = Rect::new(0, 0, 32, 16);
        let first = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 16)[HEADER].clone();
        assert_eq!(first(&mut sidebar), " ▾ bipa ──────────────────────");
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 3), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 6), area);
        let start = steady(moment(160_000));
        let tick = |sidebar: &mut Sidebar, millis: u64| {
            let at = moment(160_000 + millis);
            sidebar.ticked(at, start + Duration::from_millis(millis), true);
            first(sidebar)
        };
        assert!(sidebar.scrolling());
        assert_eq!(tick(&mut sidebar, 0), "     \u{e795} me/Documents");
        assert_eq!(tick(&mut sidebar, 30), "     \u{e795} me/Documents");
        assert_eq!(tick(&mut sidebar, 60), " ▾ zed ───────────────────────");
        for millis in (120..=600).step_by(60) {
            tick(&mut sidebar, millis);
        }
        // The last four rows: bipa's, as it lands last.
        assert_eq!(first(&mut sidebar), " ▾ bipa ──────────────────────");
        assert!(!sidebar.scrolling());
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
        assert_eq!(tick(&mut sidebar, 660), "       less logs");
    }

    /// The names of the sessions the rows list, in their order.
    fn names_listed(sidebar: &Sidebar) -> Vec<&str> {
        let rows = sidebar.rows.iter();
        rows.filter_map(|row| match row {
            Row::Session { session, .. } => Some(session.name.as_str()),
            Row::Pane { .. } => None,
        })
        .collect()
    }

    /// A move is made to the rows as the order is kept now, though this
    /// luneta has yet to hear of a move made elsewhere, or never will, as one
    /// whose watcher failed: by the keys or by the mouse, it undoes none.
    #[test]
    fn a_move_keeps_one_made_elsewhere_unheard() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        // A luneta that takes up the order only as it moves a session.
        let deaf = || {
            let order = Order::within(file.clone());
            let mut sidebar = Sidebar::new(Folds::default(), News::default(), order);
            sidebar.heard(searched().host.unwrap());
            sidebar.focus(Keys::Sidebar);
            sidebar
        };
        let (mut here, mut there) = (deaf(), deaf());
        // zed to the top, there.
        type_in(&mut there, "GvKK");
        assert_eq!(names_listed(&there), ["zed", "bipa", "notes"]);
        // notes up one, here, which lists bipa, notes and zed still.
        type_in(&mut here, "jjjvK");
        assert_eq!(names_listed(&here), ["zed", "notes", "bipa"]);

        // zed to the bottom, there, by its row, as it lists bipa over notes
        // still.
        let area = Rect::new(0, 0, 32, 22);
        drawn_rows(&mut there);
        let left = MouseButton::Left;
        there.mouse(mouse(MouseEventKind::Down(left), 10, 3), area);
        there.mouse(mouse(MouseEventKind::Drag(left), 10, 12), area);
        there.mouse(mouse(MouseEventKind::Up(left), 10, 12), area);
        assert_eq!(names_listed(&there), ["notes", "bipa", "zed"]);
        assert_eq!(names_listed(&deaf()), ["notes", "bipa", "zed"]);
    }

    /// Sessions of one name move together, as the order goes by name: by the
    /// later of them as by the first, past a session at a time, by the keys or
    /// by the mouse. Apart, as the daemon lists them oldest first, the one
    /// moved goes beside the other.
    #[test]
    fn sessions_of_a_name_move_together() {
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(
            vec![
                session("x", 10, vec![]),
                session("y", 20, vec![pane(1, 0, "/w/y", "fish")]),
                session("x", 30, vec![]),
                session("z", 40, vec![]),
            ],
            Vec::new(),
        ));
        sidebar.focus(Keys::Sidebar);
        // The later x, up beside the first, and down with it, both lit.
        type_in(&mut sidebar, "jjvK");
        assert_eq!(names_listed(&sidebar), ["x", "x", "y", "z"]);
        type_in(&mut sidebar, "J");
        assert_eq!(names_listed(&sidebar), ["y", "x", "x", "z"]);
        assert_eq!(drawn_rows(&mut sidebar).1, [2, 3]);

        // The later x, by its row, under z, the first with it.
        let area = Rect::new(0, 0, 32, 22);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 6), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 7), area);
        assert_eq!(drawn_rows(&mut sidebar).1, [3, 4]);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 7), area);
        assert_eq!(names_listed(&sidebar), ["y", "z", "x", "x"]);
    }
}
