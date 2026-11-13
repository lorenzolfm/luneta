//! The menu: a key, then another, and a pane opens where you work, as
//! which-key shows the keys in neovim. It opens at the bottom of the sidebar,
//! for the row highlighted then, or floats in a box at the bottom right of
//! zellij's side, for the pane zellij is on.
//!
//! Its keys are luneta's own. `/` searches every skill the directory of the
//! row's pane can run, for an agent to start on the one you pick, beside that
//! pane. `a` opens the ask box, for an agent to do what you say in words, as
//! the menu would. `c` opens the Skills group, a key for each of a few skills, which
//! starts an agent on it there, `f` and `e`, which start one on what the
//! command a shell ran last printed, to fix what went wrong or explain it, and
//! `c`, which starts one on the checks that failed on its pull request. Each
//! starts in the harness the line under the keys names, by its icon and its
//! name: the one that runs in the pane, which the line says, or else the one
//! you picked last, and `Tab` picks the next: in the search too, and in the
//! menu for a link.
//! `n` opens the New group: your shell beside the
//! pane, a tab of its session, or a session, in the directory it works in, or
//! on a machine's header in the home of its user there, or a machine for
//! lunetad to watch through ssh, by what you type ssh to it. `t`
//! opens the Turn group: the pane into a tab of its own, or its tab into panes
//! of another you pick, what runs in them running on. `d`
//! opens the Delete group, which closes the pane or deletes its session, and
//! asks first when that would end an agent at work, or on a pane's row in a
//! repository lists the worktrees of its merged pull requests, to delete once
//! you say yes, and on another machine's header forgets the machine. `b` opens the
//! Brief group, which briefs you on every session, the row's session, or the agent in its pane, or on a machine's
//! header, the machine's sessions. luneta has no
//! configuration to read keys for skills from, and the Skills group's are its
//! own. The menu lists only the keys that work on its kind of row: a session's
//! row and a machine's header have no pane, and none of the keys that need one,
//! and this machine's header nothing to delete.
//!
//! A pull request's link pasted with the sidebar's keys, or in the menu,
//! opens it for that pull request instead: `w` makes a worktree of it, in a
//! tab of the session that works in its repository, and `r` starts an agent
//! on a review of it there too. Where a pane has its branch checked out
//! already, `w` takes you there, and `r` starts the agent beside it. An
//! issue's link opens it for that issue: `w` makes a worktree of a branch for
//! it, and `t` starts an agent there on tackling it. Each acts on the row's
//! machine, or else on the first other machine that has the repository.
//!
//! What a key started is the sidebar's from the moment you press it, not the
//! menu's: the menu closing, as the keys leave it for zellij, or as `Esc`
//! closes it, does not end it. The sidebar keeps it, says in its notes what it
//! waits on, and opens the tab and starts the agent once it has, where they
//! would have been, without taking zellij's client from where you went.
//!
//! At the top of the sidebar, a key the sidebar has no use of its own for
//! opens the menu on the group it opens, and `m` opens it on its first group,
//! where every key is the menu's. While the sidebar has the focus and no menu
//! is open, the legend at its bottom lists the keys it takes there. A key
//! there that waits for the next, as a group's does, shows what can come next
//! only once a pause passes with none, as which-key does, and `?` lists every
//! key luneta has in the legend's place.

use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use luneta_protocol::github::{PullRequest, State};
use luneta_protocol::said::Said;
use lunetad::prunes;
use lunetad::{Destination, Directory};
use lunetad::turns::{OtherTab, Turn};
use luneta_protocol::shell::Ending;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::agents::{self, Harness, Picked};
use crate::brief;
use crate::checks::{self, Failing, PullPage};
use crate::close::{Act, Plan, Scope};
use crate::gruvbox;
use crate::host;
use crate::icons;
use crate::issue::Repository;
use crate::machines::Machine;
use crate::printed::{self, Printed};
use crate::prune;
use crate::pull::{At, Found, Kind, Readied, Ready, Wanted, Work};
use crate::sidebar::{self, ACCENT, LIT, Listed, Mark, NAME, PanePlace, Place, TAG};
use crate::skills::{Name, Search, Skills};
use crate::spots::{Drawn, Spots};
use crate::zellij::{SessionName, Starts};

/// The key that lists every key luneta has, at the bottom of the sidebar, and
/// what a list of keys short of room says it does on its last line.
pub const ALL_KEYS: char = '?';
const ALL_KEYS_DOES: &str = "All keys";

/// The keys a line of a list of keys names: one key, with what is held with
/// it, or two characters, as `g g` typed one after the other, or `j k`, each
/// one way of what the line says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Key(KeyCode, KeyModifiers),
    Two(char, char),
}

impl Label {
    /// The character `c`, with nothing held.
    pub const fn char(c: char) -> Label {
        Label::Key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// The key `code`, with nothing held.
    pub const fn code(code: KeyCode) -> Label {
        Label::Key(code, KeyModifiers::NONE)
    }

    /// The key a click on its line presses: none for two.
    fn pressed(self) -> Option<KeyEvent> {
        match self {
            Label::Key(code, modifiers) => Some(KeyEvent::new(code, modifiers)),
            Label::Two(..) => None,
        }
    }
}

impl std::fmt::Display for Label {
    /// What is held, as `Ctrl`, `Alt` and `Shift`, then the key, `Space`,
    /// `Enter`, `Esc`, `Tab` and `BS` by name; or the two characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (code, modifiers) = match *self {
            Label::Two(first, second) => return write!(f, "{first} {second}"),
            Label::Key(code, modifiers) => (code, modifiers),
        };
        let held = [
            (KeyModifiers::CONTROL, "Ctrl"),
            (KeyModifiers::ALT, "Alt"),
            (KeyModifiers::SHIFT, "Shift"),
        ];
        for (modifier, name) in held {
            if modifiers.contains(modifier) {
                write!(f, "{name} ")?;
            }
        }
        match code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Esc => f.write_str("Esc"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::Backspace => f.write_str("BS"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// The moves and folds at the top of the sidebar, as the list of every key
/// names them.
const MOVES: [(Label, &str); 10] = [
    (Label::Two('j', 'k'), "Next row, row before"),
    (Label::Two('g', 'g'), "First row"),
    (Label::char('G'), "Last row"),
    (Label::Two('h', 'l'), "Fold, unfold"),
    (Label::char(' '), "Fold or unfold"),
    (Label::Two('J', 'K'), "Move what v or V holds"),
    (Label::code(KeyCode::Enter), "Go to the row"),
    (Label::char('m'), "Menu"),
    (Label::char(ALL_KEYS), "All keys"),
    (Label::code(KeyCode::Esc), "Back to zellij"),
];

/// The keys of the menu besides its entries, as the list of every key names
/// them: those it takes in any group, and those of the menu for a pasted link.
const MENU_MOVES: [(Label, &str); 6] = [
    (Label::code(KeyCode::Tab), "use another agent"),
    (Label::code(KeyCode::Backspace), "back a group"),
    (Label::code(KeyCode::Esc), "close"),
    (Label::char('w'), "worktree of a pasted link"),
    (Label::char('r'), "review a pasted PR"),
    (Label::char('t'), "tackle a pasted issue"),
];

/// The Delete group's keys, as the list of every key names them, on no row in
/// particular: what `d d` deletes is the row's.
const DELETES: [(Label, &str); 3] = [
    (Label::Two(DELETE, 'd'), "delete the row"),
    (Label::Two(DELETE, 's'), "delete the session"),
    (Label::Two(DELETE, 'm'), DELETE_MERGED_DOES),
];

/// The key that opens the search over the skills, at the top of the menu, and
/// what the menu says it does.
const SEARCH: char = '/';
const SEARCH_DOES: &str = "Search skills";

/// The key that opens the ask box, at the top of the menu, and what the menu
/// says it does.
const ASK: char = 'a';
const ASK_DOES: &str = "Ask";

/// The key that opens the issue box, at the top of the menu, and what the
/// menu says it does.
const ISSUE: char = 'i';
const ISSUE_DOES: &str = "File issue";

/// The key of the Skills group, its name, and its keys, each with the skill
/// it starts an agent on: the skills the author runs most. luneta has no
/// configuration to read yours from.
const SKILLS: char = 'c';
const SKILLS_NAME: &str = "Skills";
const SKILL_KEYS: [(char, &str); 3] =
    [('t', REVIEW), ('m', "make-illegal-states-unrepresentable"), ('s', "tigerstyle")];

/// The key of the Skills group that starts an agent on the checks that failed
/// on the head of the pane's pull request, and what the menu says it does.
const CHECKS: char = 'c';
const CHECKS_DOES: &str = "fix the failing checks";

/// The skill an agent reviews a pull request with: the Skills group's `t`,
/// and the `r` of the menu for a pull request, which gives it the link.
const REVIEW: &str = "thermo-nuclear-review";

/// What the `t` of the menu for an issue tells an agent, before the link: in
/// words, as you would, with no skill.
const TACKLE: &str = "Tackle";

/// The key of luneta's own New group, and its name.
const NEW: char = 'n';
const NEW_NAME: &str = "New";

/// The key of luneta's own Turn group, and its name.
const TURN: char = 't';
const TURN_NAME: &str = "Turn";

/// What the menu names the list of tabs a tab's panes can turn into.
const TABS_TITLE: &str = "Into tab";

/// The keys of that list, a tab each, in their order. A session with more
/// tabs than this leaves the rest out.
const TAB_KEYS: &str = "123456789abcdefghijklmnopqrstuvwxyz";

/// The key of luneta's own Delete group, its name, and its keys: one deletes
/// what the menu is for, the other its session.
const DELETE: char = 'd';
const DELETE_NAME: &str = "Delete";
const DELETE_ROW: char = 'd';
const DELETE_SESSION: char = 's';
const DELETE_MERGED: char = 'm';
const DELETE_MERGED_DOES: &str = "delete merged worktrees";

/// The key of luneta's own Brief group, and its name.
const BRIEF: char = 'b';
const BRIEF_NAME: &str = "Brief";

/// What the key of the Delete group that forgets a machine says it does.
const FORGET_DOES: &str = "forget the machine";

/// What the rule over the menu's first group, and over the legend, names
/// them, and the sidebar's first line; and what it names the search box, and
/// the box a machine's destination is typed in.
pub const TITLE: &str = "luneta";
const SEARCH_TITLE: &str = "Skills";
const SSH_TITLE: &str = "ssh";

/// What the line under the keys that start agents says `Tab` does, in as
/// many words as it has room for.
const NEXT_DOES: [&str; 2] = ["another agent", "another"];

/// What the line under the keys that start agents says after the name of the
/// harness, where the agent in the pane picked it.
const PANE_S: &str = ", the pane's";

/// What follows a group's name: it opens, as a folded session does.
const GROUP: &str = "▸";

/// The columns between a key and what it does.
const GAP: usize = 2;

/// The most skills the search box shows at once. The list scrolls under it.
const SEARCH_ROWS: usize = 8;

/// The fewest and the most columns inside the box the menu floats in, past
/// its borders: as many as what it shows takes, the search box the most, and
/// what is longer wraps or is cut.
const FLOATING: (usize, usize) = (24, 62);

/// The key that waits for what to yank at the top of the sidebar, what the
/// legend names it, the key that then yanks a pull request's link, and what
/// is said of that key.
const YANK: char = 'y';
const YANK_NAME: &str = "Yank";
pub const YANK_PULL: char = 'p';
const YANK_PULL_DOES: &str = "PR link";

/// What `y` waits for: its group in the legend holds them, and the bottom of
/// the sidebar lists them while it waits.
const YANKS: [Entry; 1] = [Entry::YankPull];

/// A key of the sidebar's own that does more than move the highlight or fold
/// a session. The legend lists each before the menu's keys, and a key of the
/// menu's that one has stays the command's at the top of the sidebar: `/`
/// searches the rows there, and the skills only in the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Narrows the rows to those that match what you type.
    Search,
    /// Marks the highlighted pane read while its row has the dot, or else
    /// unread.
    Unread,
    /// Selects the session of the highlighted row, or the machine of the
    /// highlighted header, for `Top::Shift` to move, and ends the selection.
    Reorder,
    /// Picks the highlighted pane for `Top::Shift` to move among the panes,
    /// then its tab, and ends.
    MovePane,
    /// Opens the pull request of the highlighted row in your browser.
    Open,
}

impl Command {
    /// Every command, in the order the legend lists them.
    const ALL: [Command; 5] =
        [Command::Search, Command::Unread, Command::Reorder, Command::MovePane, Command::Open];

    const fn key(self) -> char {
        match self {
            Command::Search => '/',
            Command::Unread => 'u',
            Command::Reorder => 'v',
            Command::MovePane => 'V',
            Command::Open => 'o',
        }
    }

    /// What the legend says the command does, and the rule over the search
    /// names it.
    pub const fn does(self) -> &'static str {
        match self {
            Command::Search => "Search",
            Command::Unread => "Unread",
            Command::Reorder => "Reorder",
            Command::MovePane => "Move pane",
            Command::Open => "Open PR",
        }
    }

    /// The command `key` runs at the top of the sidebar, if any.
    fn of(key: char) -> Option<Command> {
        Command::ALL.into_iter().find(|command| command.key() == key)
    }
}

/// What a key does at the top of the sidebar, ahead of the menu's keys. The
/// sidebar does what `of` says, and the legend leaves out a key of the menu's
/// that `moves` takes, so the legend shows no key that does something else
/// there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Top {
    /// Moves the highlight so many rows down, or up.
    Move(isize),
    /// Moves the highlight to the last row, as vim's `G`.
    Last,
    /// Moves the highlight to the first row once pressed twice, as vim's
    /// `gg`. Pressed once, the sidebar waits for the second, and any other
    /// key drops the first: `Esc` does only that, as in vim.
    First,
    /// Moves the sessions, or the machines, a selection holds so many places
    /// down, or up, or where what `V` picked lands. Without either, nothing.
    Shift(isize),
    /// `Some(true)` folds, `Some(false)` unfolds, and `None` turns it over.
    Fold(Option<bool>),
    /// Opens the menu on its first group, where every key is the menu's.
    Menu,
    /// Waits for what to yank, as vim's `y` waits for a motion: `YANK_PULL`
    /// copies the link of the highlighted row's pull request. Any other key
    /// drops it, `Esc` doing only that, as after a lone `First`.
    Yank,
    /// Lists every key luneta has, in the legend's place.
    AllKeys,
    /// Runs a command of the sidebar's own.
    Command(Command),
}

impl Top {
    /// What `key` does at the top of the sidebar besides a command: moves the
    /// highlight or the sessions selected, folds, opens the menu, or waits for
    /// what to yank.
    const fn moves(key: char) -> Option<Top> {
        match key {
            'j' => Some(Top::Move(1)),
            'k' => Some(Top::Move(-1)),
            'g' => Some(Top::First),
            'G' => Some(Top::Last),
            'J' => Some(Top::Shift(1)),
            'K' => Some(Top::Shift(-1)),
            'h' => Some(Top::Fold(Some(true))),
            'l' => Some(Top::Fold(Some(false))),
            ' ' => Some(Top::Fold(None)),
            'm' => Some(Top::Menu),
            YANK => Some(Top::Yank),
            ALL_KEYS => Some(Top::AllKeys),
            _ => None,
        }
    }

    /// What `key` does at the top of the sidebar. None for a key the menu has
    /// the say on.
    pub fn of(key: char) -> Option<Top> {
        Top::moves(key).or_else(|| Command::of(key).map(Top::Command))
    }
}

/// No command takes the key of the ask box or the issue box, or of luneta's
/// Skills, New, Turn, Delete or Brief group, which would be out of reach at
/// the top of the sidebar, and gone from the legend; nor a key the sidebar
/// moves, folds or opens the menu with, which `Top::of` finds first. Nor does
/// the sidebar move with the ask box's or the issue box's.
const _: () = {
    let mut at = 0;
    while at < Command::ALL.len() {
        let key = Command::ALL[at].key();
        let group = key == ASK
            || key == ISSUE
            || key == SKILLS
            || key == NEW
            || key == TURN
            || key == DELETE
            || key == BRIEF;
        assert!(!group, "a command on a key of luneta's groups");
        assert!(Top::moves(key).is_none(), "a command on a key the sidebar moves with");
        at += 1;
    }
    assert!(Top::moves(ASK).is_none(), "the ask box on a key the sidebar moves with");
    assert!(Top::moves(ISSUE).is_none(), "the issue box on a key the sidebar moves with");
};

/// Why the menu makes no own session on another machine.
const OWN_HERE: &str = "menu: own sessions are of this machine alone, as yet";

/// What a key of the New group opens, for the pane the menu is for, in the
/// directory it works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum New {
    /// Your shell, in a pane of its own beside that pane.
    Pane,
    /// A tab of that pane's session.
    Tab,
    /// A session, named for the directory, as the picker's directories are;
    /// or on a machine's header, for the home of its user.
    Session,
    /// A session of luneta's own multiplexer, named as `Session` is: on this
    /// machine's rows, as own sessions are of this machine alone. It stands
    /// beside `Session` until zellij goes, when `Session` is an own one.
    Own,
    /// A machine for lunetad to watch through ssh, by its destination, typed
    /// in a box: on any row.
    Machine,
}

impl New {
    /// Each, in the order the New group shows them.
    const ALL: [New; 5] = [New::Pane, New::Tab, New::Session, New::Own, New::Machine];

    const fn key(self) -> char {
        match self {
            New::Pane => 'p',
            New::Tab => 't',
            New::Session => 's',
            New::Own => 'o',
            New::Machine => 'm',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            New::Pane => "new pane",
            New::Tab => "new tab",
            New::Session => "new session",
            New::Own => "new own session",
            New::Machine => "new machine",
        }
    }
}

/// A key of the Turn group, for the pane the menu is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnKey {
    /// That pane, into a tab of its own.
    PaneIntoTab,
    /// That pane's tab, into panes of another tab, which you pick where it
    /// has more than one.
    TabIntoPanes,
}

impl TurnKey {
    /// Each, in the order the Turn group shows them.
    const ALL: [TurnKey; 2] = [TurnKey::PaneIntoTab, TurnKey::TabIntoPanes];

    const fn key(self) -> char {
        match self {
            TurnKey::PaneIntoTab => 't',
            TurnKey::TabIntoPanes => 'p',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            TurnKey::PaneIntoTab => "pane into tab",
            TurnKey::TabIntoPanes => "tab into pane",
        }
    }
}

/// A key of the Skills group that starts an agent on what the command a
/// shell ran last printed, beside its pane: to fix what went wrong, or to
/// explain it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    Fix,
    Explain,
}

impl Last {
    /// Each, in the order the Skills group shows them, after its skills.
    const ALL: [Last; 2] = [Last::Fix, Last::Explain];

    const fn key(self) -> char {
        match self {
            Last::Fix => 'f',
            Last::Explain => 'e',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            Last::Fix => "fix the last command",
            Last::Explain => "explain the last command",
        }
    }

    /// What the agent is asked, before it is told of the command.
    const fn asks(self) -> &'static str {
        match self {
            Last::Fix => "Fix this.",
            Last::Explain => "Explain this.",
        }
    }
}

/// No key of the Skills group's skills is one of `Last`'s, or `CHECKS`, nor
/// one of `Last`'s `CHECKS`, which would hide it.
const _: () = {
    let mut at = 0;
    while at < Last::ALL.len() {
        let mut skill = 0;
        while skill < SKILL_KEYS.len() {
            let alike = SKILL_KEYS[skill].0 == Last::ALL[at].key();
            assert!(!alike, "two keys alike in the Skills group");
            assert!(SKILL_KEYS[skill].0 != CHECKS, "two keys alike in the Skills group");
            skill += 1;
        }
        assert!(Last::ALL[at].key() != CHECKS, "two keys alike in the Skills group");
        at += 1;
    }
};

/// What a key of the menu for a pull request, an issue or a file does, in a
/// tab of the session that works in its repository, or where its branch is
/// checked out. The menu for each has both, and `Asks` says what its agent is
/// on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// A worktree of the pull request, or of the branch of the issue or the
    /// file, as `wt add` makes it.
    Worktree,
    /// The same, with an agent on it: on a review of the pull request, or on
    /// tackling the issue or the file.
    Agent,
}

/// What the agent of a menu for a pull request, an issue or a file is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asks {
    Review,
    Tackle,
}

impl Asks {
    /// What the agent on `work` is asked: a review of a pull request, and of
    /// anything else, to tackle it.
    fn of(work: &Work) -> Asks {
        match work {
            Work::Link(link) if link.kind() == Kind::Pull => Asks::Review,
            Work::Link(_) | Work::File(_) => Asks::Tackle,
        }
    }
}

impl Pull {
    /// Each, in the order the menu shows them.
    const ALL: [Pull; 2] = [Pull::Worktree, Pull::Agent];

    /// Its key in a menu whose agent `asks`.
    const fn key(self, asks: Asks) -> char {
        match (self, asks) {
            (Pull::Worktree, _) => 'w',
            (Pull::Agent, Asks::Review) => 'r',
            (Pull::Agent, Asks::Tackle) => 't',
        }
    }

    /// What a menu whose agent `asks` says the key does.
    fn does(self, asks: Asks) -> String {
        match (self, asks) {
            (Pull::Worktree, _) => "worktree".to_owned(),
            (Pull::Agent, Asks::Review) => format!("/{REVIEW}"),
            (Pull::Agent, Asks::Tackle) => "tackle".to_owned(),
        }
    }

    const fn wanted(self, asks: Asks) -> Wanted {
        match (self, asks) {
            (Pull::Agent, Asks::Review) => Wanted::AtHead,
            (Pull::Worktree, _) | (Pull::Agent, Asks::Tackle) => Wanted::AsItIs,
        }
    }

    /// The one of a menu whose agent `asks` that `key` presses, if any: its
    /// key, with no modifier but `Shift`.
    fn pressed(key: &KeyEvent, asks: Asks) -> Option<Pull> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        Pull::ALL.into_iter().find(|pull| bare(key) && pull.key(asks) == c)
    }
}

enum Entry {
    /// A key that opens a group, the name the menu shows for it, and the keys
    /// in it.
    Group { key: char, name: &'static str, entries: Vec<Entry> },
    /// A key of the Skills group: starts an agent on the skill so named.
    Start { key: char, skill: &'static str },
    /// A key of the Skills group: starts an agent on what the command the
    /// pane's shell ran last printed.
    Last(Last),
    /// The key of the Skills group that starts an agent on the checks that
    /// failed on the pane's pull request.
    Checks,
    /// A key of the New group: opens a pane, a tab or a session.
    New(New),
    /// A key of the Turn group: turns the pane the menu is for, or its tab.
    Turn(TurnKey),
    /// A key of the list of tabs a tab's panes can turn into: the tab of this
    /// name, which that list shows.
    Tab { key: char, name: String },
    /// A key of the Delete group: closes the pane the menu is for, or deletes
    /// its session, `session`.
    Close { key: char, session: Listed, scope: Scope },
    /// The key of the Delete group on a pane's row in a repository: lists the
    /// worktrees of its merged pull requests, by a directory of it.
    Prune { dir: Directory },
    /// The key of the Delete group on another machine's header: forgets the
    /// machine.
    Forget(Destination),
    /// A key of the Brief group: briefs you on what the agents it covers are
    /// doing.
    Brief(brief::Scope),
    /// A command of the sidebar's own, which the legend lists beside the
    /// menu's keys.
    Command(Command),
    /// The key that yanks a pull request's link, which the bottom of the
    /// sidebar lists while `y` waits.
    YankPull,
    /// A key of the menu for what was pasted, a pull request's link, an
    /// issue's or a file's path, which that menu shows.
    Pull(Pull, Asks),
    /// The key that opens the search over the skills.
    Search,
    /// The key that opens the ask box.
    Ask,
    /// The key that opens the issue box, on a pane's row in a repository.
    Issue,
}

impl Entry {
    /// The entry as the menu for `target` lists it: a key that works on its
    /// kind of row and of session, or a group with those of its keys that do; or none, for
    /// a key that does not, or a group left with none. A key built for the
    /// row, as the Delete group's are, works on it.
    fn on(self, target: &Target) -> Option<Entry> {
        let pane = matches!(target, Target::Pane(..));
        let session = matches!(target, Target::Session(..));
        match self {
            Entry::Group { key, name, entries } => {
                let entries: Vec<Entry> =
                    entries.into_iter().filter_map(|entry| entry.on(target)).collect();
                (!entries.is_empty()).then_some(Entry::Group { key, name, entries })
            },
            Entry::Search
            | Entry::Start { .. }
            | Entry::Last(_)
            | Entry::Checks
            | Entry::New(New::Tab) => pane.then_some(self),
            Entry::New(New::Pane) | Entry::Turn(_) => (pane && !target.own()).then_some(self),
            Entry::New(New::Session) => (!session).then_some(self),
            Entry::New(New::Own) => {
                (!session && *target.machine() == Machine::Local).then_some(self)
            },
            Entry::Issue => target.repository().is_some().then_some(self),
            Entry::New(New::Machine)
            | Entry::Ask
            | Entry::Brief(_)
            | Entry::Close { .. }
            | Entry::Prune { .. }
            | Entry::Forget(_)
            | Entry::Tab { .. }
            | Entry::Command(_)
            | Entry::YankPull
            | Entry::Pull(..) => Some(self),
        }
    }

    fn key(&self) -> char {
        match self {
            Entry::Group { key, .. }
            | Entry::Start { key, .. }
            | Entry::Close { key, .. }
            | Entry::Tab { key, .. } => *key,
            Entry::Prune { .. } => DELETE_MERGED,
            Entry::Forget(_) => DELETE_ROW,
            Entry::New(new) => new.key(),
            Entry::Turn(turning) => turning.key(),
            Entry::Last(last) => last.key(),
            Entry::Checks => CHECKS,
            Entry::Brief(scope) => scope.key(),
            Entry::Command(command) => command.key(),
            Entry::YankPull => YANK_PULL,
            Entry::Pull(pull, asks) => pull.key(*asks),
            Entry::Search => SEARCH,
            Entry::Ask => ASK,
            Entry::Issue => ISSUE,
        }
    }

    /// What the menu says the key does: for a group, its name.
    fn does(&self) -> String {
        match self {
            Entry::Group { name, .. } => (*name).to_owned(),
            Entry::Start { skill, .. } => format!("/{skill}"),
            Entry::Last(last) => last.does().to_owned(),
            Entry::Checks => CHECKS_DOES.to_owned(),
            Entry::New(new) => new.does().to_owned(),
            Entry::Turn(turning) => turning.does().to_owned(),
            Entry::Tab { name, .. } => name.clone(),
            Entry::Close { scope, .. } => scope.does().to_owned(),
            Entry::Prune { .. } => DELETE_MERGED_DOES.to_owned(),
            Entry::Forget(_) => FORGET_DOES.to_owned(),
            Entry::Brief(scope) => scope.does().to_owned(),
            Entry::Command(command) => command.does().to_owned(),
            Entry::YankPull => YANK_PULL_DOES.to_owned(),
            Entry::Pull(pull, asks) => pull.does(*asks),
            Entry::Search => SEARCH_DOES.to_owned(),
            Entry::Ask => ASK_DOES.to_owned(),
            Entry::Issue => ISSUE_DOES.to_owned(),
        }
    }

    /// The keys under it, each the keys that press it from the group it is
    /// in, after `before`, and what it does, as the list of every key names
    /// them: a group's are those of its entries, and the Delete group's are
    /// `DELETES`, on no row in particular.
    fn spelled(&self, before: &str, into: &mut Vec<(String, String)>) {
        let keys = format!("{before}{}", self.key());
        match self {
            Entry::Group { key: DELETE, .. } => {
                into.extend(DELETES.map(|(label, does)| (label.to_string(), does.to_owned())));
            },
            Entry::Group { entries, .. } => {
                for entry in entries {
                    entry.spelled(&format!("{keys} "), into);
                }
            },
            _ => into.push((keys, self.does())),
        }
    }
}

/// What a key did to the keys.
#[derive(Debug, PartialEq, Eq)]
enum Pressed {
    /// Opened a group, or went back to the one before, or typed in the search
    /// box, moved its cursor or closed it.
    Moved,
    /// Asks for the search box over the skills.
    Search,
    /// Starts an agent on the skill so named.
    Start(String),
    /// Picks this harness for the agents the menu starts.
    Switched(Harness),
    /// Starts an agent on what the pane's shell printed last.
    Last(Last),
    /// Starts an agent on the checks that failed on the pane's pull request.
    Checks,
    /// Opens a pane, a tab or a session.
    New(New),
    /// Turns the pane the menu is for, or its tab.
    Turn(TurnKey),
    /// Closes the pane the menu is for, or deletes its session, `session`,
    /// by `key`.
    Delete { key: char, session: Listed, scope: Scope },
    /// Lists the worktrees of the merged pull requests of a repository, by a
    /// directory of it.
    Prune(Directory),
    /// Forgets the machine the menu is for.
    Forget(Destination),
    /// Asks for a brief of what the scope covers.
    Brief(brief::Scope),
    /// Opens the ask box.
    Ask,
    /// Opens the issue box.
    Issue,
    /// Closes the menu.
    Close,
    /// Nothing: the group open has no such key.
    Nothing,
}

/// The menu's entries, the groups opened so far, the search box over the
/// skills while it is open, and the harness its keys start agents in.
struct Keys {
    root: Vec<Entry>,
    /// Each group opened, by where it stands in the one before.
    open: Vec<usize>,
    search: Option<Search>,
    agent: Picked,
}

impl Keys {
    /// The keys of the menu for `target`: those of `every` that work on its
    /// kind of row, as `Entry::on` says. A pane's row has them all, but `i`
    /// and `d m` where it works in no repository; a
    /// session's row `a`, `n m`, `d d`, `d s` and the Brief group; another
    /// machine's header `a`, `n s`, `n m`, `d d` and the Brief group; and
    /// this one's the same but `d d`. They start agents in `agent`.
    fn new(target: &Target, agent: Picked) -> Self {
        let every = Keys::every(target.removes()).into_iter();
        let root = every.filter_map(|entry| entry.on(target)).collect();
        Keys { root, open: Vec::new(), search: None, agent }
    }

    /// The menu's keys, on whatever row: `/`, which opens the search over
    /// the skills, `a`, which opens the ask box, `i`, which opens the issue
    /// box, and the Claude, New, Turn,
    /// Delete and Brief groups: `c t`, `c m` and `c s` start an
    /// agent on a skill, `c f` and `c e` one on what the pane's shell printed
    /// last, `c c` one on the checks that failed on its pull request, `n p`,
    /// `n t` and `n s` open a pane, a tab and a session, and `n m` the box for
    /// a machine's destination, `t t` turns the pane into a tab and `t p` its
    /// tab into panes of another, `d d` deletes
    /// what the menu is for, as `removes` says, `d s` deletes its session,
    /// and on a row in a repository, `d m` its merged worktrees, or on
    /// another machine's header `d d` alone forgets the machine, and
    /// `b z`, `b s` and `b h` brief you on every session, its session and
    /// what the menu is for, or on a machine's header, its sessions.
    fn every(removes: Option<Removes>) -> Vec<Entry> {
        let start = |(key, skill)| Entry::Start { key, skill };
        vec![
            Entry::Search,
            Entry::Ask,
            Entry::Issue,
            Entry::Group {
                key: SKILLS,
                name: SKILLS_NAME,
                entries: SKILL_KEYS
                    .into_iter()
                    .map(start)
                    .chain(Last::ALL.map(Entry::Last))
                    .chain([Entry::Checks])
                    .collect(),
            },
            Entry::Group {
                key: NEW,
                name: NEW_NAME,
                entries: New::ALL.into_iter().map(Entry::New).collect(),
            },
            Entry::Group {
                key: TURN,
                name: TURN_NAME,
                entries: TurnKey::ALL.into_iter().map(Entry::Turn).collect(),
            },
            Entry::Group {
                key: DELETE,
                name: DELETE_NAME,
                entries: match removes {
                    Some(Removes::Row { session, scope, merged_in }) => {
                        let mut entries = vec![
                            Entry::Close { key: DELETE_ROW, session: session.clone(), scope },
                            Entry::Close { key: DELETE_SESSION, session, scope: Scope::Session },
                        ];
                        entries.extend(merged_in.map(|dir| Entry::Prune { dir }));
                        entries
                    },
                    Some(Removes::Machine(destination)) => vec![Entry::Forget(destination)],
                    None => Vec::new(),
                },
            },
            Entry::Group {
                key: BRIEF,
                name: BRIEF_NAME,
                entries: brief::Scope::ALL.into_iter().map(Entry::Brief).collect(),
            },
        ]
    }

    /// The legend: the sidebar's commands, and what `y` waits on, which it
    /// lists as a group, then the keys at the top of the menu on any row, but
    /// for those a command has, or the sidebar moves with, which `m` reaches.
    /// It shows the Delete group's key and not what it deletes, which is the
    /// row's.
    fn legend() -> Vec<Entry> {
        let mut root: Vec<Entry> = Command::ALL.into_iter().map(Entry::Command).collect();
        root.push(Entry::Group { key: YANK, name: YANK_NAME, entries: YANKS.into() });
        let shown = |entry: &Entry| Top::of(entry.key()).is_none();
        root.extend(Keys::every(None).into_iter().filter(shown));
        root
    }

    /// The groups opened, each its name and its entries.
    fn trail(&self) -> Vec<(&'static str, &[Entry])> {
        let mut trail = Vec::with_capacity(self.open.len());
        let mut entries = self.root.as_slice();
        for at in &self.open {
            if let Some(Entry::Group { name, entries: inner, .. }) = entries.get(*at) {
                trail.push((*name, inner.as_slice()));
                entries = inner;
            }
        }
        trail
    }

    /// The entries of the group open.
    fn entries(&self) -> &[Entry] {
        self.trail().last().map_or(self.root.as_slice(), |(_, entries)| entries)
    }

    /// Whether what is open starts agents, and names their harness: the
    /// Skills group, or the search box over the skills.
    fn starts(&self) -> bool {
        self.search.is_some() || self.trail().last().is_some_and(|(name, _)| *name == SKILLS_NAME)
    }

    /// What `key` does: a key the group open shows opens its group, opens or
    /// closes what it opens or closes, or asks for the search box,
    /// `Backspace` goes back a group, and out of the menu from the first, and
    /// `Esc` closes it. While the search box is open, it takes the keys
    /// instead. Where what is open starts agents, `Tab` picks the next
    /// harness for them.
    fn key(&mut self, key: &KeyEvent) -> Pressed {
        if key.code == KeyCode::Tab && bare(key) && self.starts() {
            self.agent = self.agent.next();
            return Pressed::Switched(self.agent.harness());
        }
        if let Some(search) = &mut self.search {
            let (pressed, open) = searched(search, key);
            if !open {
                self.search = None;
            }
            return pressed;
        }
        if closes(key) {
            return Pressed::Close;
        }
        if !bare(key) {
            return Pressed::Nothing;
        }
        match key.code {
            KeyCode::Backspace => match self.open.pop() {
                Some(_) => Pressed::Moved,
                None => Pressed::Close,
            },
            KeyCode::Char(c) => self.press(c),
            _ => Pressed::Nothing,
        }
    }

    fn press(&mut self, key: char) -> Pressed {
        let entries = self.entries();
        let Some(at) = entries.iter().position(|entry| entry.key() == key) else {
            return Pressed::Nothing;
        };
        match &entries[at] {
            Entry::Start { skill, .. } => return Pressed::Start((*skill).to_owned()),
            Entry::Last(last) => return Pressed::Last(*last),
            Entry::Checks => return Pressed::Checks,
            Entry::New(new) => return Pressed::New(*new),
            Entry::Turn(turning) => return Pressed::Turn(*turning),
            Entry::Close { key, session, scope } => {
                return Pressed::Delete { key: *key, session: session.clone(), scope: *scope };
            },
            Entry::Prune { dir } => return Pressed::Prune(dir.clone()),
            Entry::Forget(destination) => return Pressed::Forget(destination.clone()),
            Entry::Brief(scope) => return Pressed::Brief(*scope),
            Entry::Search => return Pressed::Search,
            Entry::Ask => return Pressed::Ask,
            Entry::Issue => return Pressed::Issue,
            // The legend only shows, and runs nothing, as do the menu for a
            // pull request and the list of tabs, which take their keys themselves.
            Entry::Command(_) | Entry::YankPull | Entry::Pull(..) | Entry::Tab { .. } => {
                return Pressed::Nothing;
            },
            Entry::Group { .. } => {},
        }
        self.open.push(at);
        Pressed::Moved
    }
}

/// Whether `key` is pressed with no modifier but `Shift`, which is in the
/// character it types.
fn bare(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

/// Whether `key` closes the menu, whatever group is open.
pub fn closes(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('c') => key.modifiers == KeyModifiers::CONTROL,
        KeyCode::Esc => bare(key),
        _ => false,
    }
}

/// What `key` does in the search box, as in the sidebar's: a character types,
/// `Backspace` takes one back, `Down` and `Up` move the cursor, and `Enter`
/// starts an agent on the skill it is on. `Esc` closes the box, back to the
/// keys, and so does `Backspace` once there is nothing to take back; `Ctrl c`
/// closes the menu. Says too whether the box stays open.
fn searched(search: &mut Search, key: &KeyEvent) -> (Pressed, bool) {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        return (Pressed::Close, true);
    }
    if !bare(key) {
        return (Pressed::Nothing, true);
    }
    match key.code {
        KeyCode::Esc => (Pressed::Moved, false),
        KeyCode::Backspace => (Pressed::Moved, search.pop()),
        KeyCode::Down => {
            search.move_by(1);
            (Pressed::Moved, true)
        },
        KeyCode::Up => {
            search.move_by(-1);
            (Pressed::Moved, true)
        },
        KeyCode::Enter => match search.chosen() {
            Some(skill) => (Pressed::Start(skill.name.to_string()), true),
            None => (Pressed::Nothing, true),
        },
        KeyCode::Char(c) => {
            search.push(c);
            (Pressed::Moved, true)
        },
        _ => (Pressed::Nothing, true),
    }
}

/// What the box for a machine's destination says under what is typed, until
/// `Enter` finds it is none.
const SSH_HINT: &str = "what you type ssh to it: a Tailscale name, a Host of your ssh config, or \
                        me@vps";

/// What a key did in the box for a machine's destination.
enum Typed {
    Typing,
    Enter,
    /// Back to the keys: `Esc`, or `Backspace` with nothing typed.
    Back,
    Close,
}

/// What `key` does in the box for a machine's destination, with `typed` in
/// it, as in the search box: a character types, `Backspace` takes one back,
/// and `Enter` adds the machine. `Esc` goes back to the keys, and so does
/// `Backspace` once there is nothing to take back; `Ctrl c` closes the menu.
fn ssh_key(key: &KeyEvent, typed: &mut String) -> Typed {
    if key.code == KeyCode::Char('c') && key.modifiers == KeyModifiers::CONTROL {
        return Typed::Close;
    }
    if !bare(key) {
        return Typed::Typing;
    }
    match key.code {
        KeyCode::Esc => Typed::Back,
        KeyCode::Backspace if typed.is_empty() => Typed::Back,
        KeyCode::Backspace => {
            typed.pop();
            Typed::Typing
        },
        KeyCode::Enter => Typed::Enter,
        KeyCode::Char(c) => {
            typed.push(c);
            Typed::Typing
        },
        _ => Typed::Typing,
    }
}

/// What a key says to a delete key that asks before it goes ahead.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Yes,
    Back,
    Close,
    Nothing,
}

/// `y`, or the delete key `asked` again, goes ahead; `n` and `Backspace` go
/// back to the keys; `Esc` closes the menu.
fn answer(key: &KeyEvent, asked: char) -> Answer {
    if closes(key) {
        return Answer::Close;
    }
    if !bare(key) {
        return Answer::Nothing;
    }
    match key.code {
        KeyCode::Char(c) if c == 'y' || c == asked => Answer::Yes,
        KeyCode::Char('n') | KeyCode::Backspace => Answer::Back,
        _ => Answer::Nothing,
    }
}

/// The entries of the list of `tabs`, on a key each, as far as there are keys
/// for them.
fn tab_entries(tabs: &[OtherTab]) -> Vec<Entry> {
    let entry = |(key, tab): (char, &OtherTab)| Entry::Tab { key, name: tab.name.clone() };
    TAB_KEYS.chars().zip(tabs).map(entry).collect()
}

/// The terminal that names the tab among `tabs` that `key` picks, if any: its
/// key, with no modifier but `Shift`.
fn picked(key: &KeyEvent, tabs: &[OtherTab]) -> Option<u32> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    let at = TAB_KEYS.chars().position(|key| key == c).filter(|_| bare(key))?;
    tabs.get(at).map(|tab| tab.pane)
}

/// A key of the Delete group: the key, which goes ahead pressed again once
/// it asks, and what it deletes, of `session`.
#[derive(Clone, Debug)]
struct Delete {
    key: char,
    session: Listed,
    scope: Scope,
}

/// What the Delete group's keys take away: on a session's row or a pane's,
/// the pane or the session, of `session`; on another machine's header, the
/// machine, from those luneta lists.
enum Removes {
    /// `merged_in` is a directory of the repository the row is in, if any.
    Row { session: Listed, scope: Scope, merged_in: Option<Directory> },
    Machine(Destination),
}

/// What the menu is for: the row highlighted as it opened, of the machine
/// its session or pane is on, which what a key opens there is of too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A session's row, which has no pane.
    Session(Machine, Listed),
    Pane(Machine, PaneRow),
    /// A machine's header, which has neither a session nor a pane.
    Machine(Machine),
}

/// A pane's row, as the menu is for it: the directory the pane works in, or
/// its agent while zellij has not said, the branch checked out there, its
/// pull request, and what its shell said it ran last, as the daemon last said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRow {
    pub session: Listed,
    pub pane: u32,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    /// The directory that holds the repository it works in, where git has said.
    pub repository: Option<String>,
    /// The pull request of that branch, where gh has found one.
    pub pull_request: Option<PullRequest>,
    pub ran: Ran,
}

/// What the shell in the pane of the menu's row said last through luneta's
/// hook, as the daemon passed it on: a key that starts an agent on what its
/// last command printed takes one that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    /// No shell with the hook runs in the pane: one with it says it is ready
    /// from its first prompt. In an own pane, the shell writes no marks of the
    /// commands it runs (OSC 133), which its server reads in place of a hook.
    Unhooked,
    /// A shell with the hook, at its prompt, that has run nothing yet.
    Ready,
    /// What this luneta cannot read: what a daemon newer than its own says,
    /// or a command of no words, which the protocol never sends.
    Unknown,
    /// A command it runs still.
    Going(Said),
    /// A job it stopped, with `Ctrl z`, which waits to be brought back.
    Stopped(Said),
    /// The command it ran last, which ended.
    Ended(Ended),
}

/// The command a shell ran last, which ended: as `ending` says, where the
/// shell can say how, at `at`, to the second, which tells it from another
/// run of the same command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub command: Said,
    pub ending: Option<Ending>,
    pub at: SystemTime,
}

impl Target {
    /// The machine it is of.
    pub fn machine(&self) -> &Machine {
        match self {
            Target::Session(machine, _) | Target::Pane(machine, _) | Target::Machine(machine) => {
                machine
            },
        }
    }

    /// Whether it is of an own session, which opens no pane beside another,
    /// and turns none.
    fn own(&self) -> bool {
        match self {
            Target::Session(_, session) => session.own(),
            Target::Pane(_, row) => row.session.own(),
            Target::Machine(_) => false,
        }
    }

    /// The directory its pane works in, where the daemon has said it.
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Target::Session(..) | Target::Machine(_) => None,
            Target::Pane(_, row) => row.cwd.as_deref(),
        }
    }

    /// The name of the repository its pane works in, where git has said.
    fn repository_name(&self) -> &str {
        match self {
            Target::Pane(_, PaneRow { repository: Some(repository), .. }) => {
                prunes::name_of(repository)
            },
            Target::Pane(..) | Target::Session(..) | Target::Machine(_) => "",
        }
    }

    /// The repository an issue the issue box files goes to: the one its
    /// pane works in, where git has said, by the directory the pane works in,
    /// on its machine. None on a session's row, or a machine's header, which
    /// work in no repository.
    fn repository(&self) -> Option<Repository> {
        let Target::Pane(on, PaneRow { cwd: Some(cwd), repository: Some(home), .. }) = self else {
            return None;
        };
        let dir = Directory::parse(cwd.clone()).ok()?;
        let (home, name) = (home.clone(), self.repository_name().to_owned());
        Some(Repository { on: on.clone(), dir, home, name })
    }

    /// The branch checked out where its pane works.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Target::Session(..) | Target::Machine(_) => None,
            Target::Pane(_, row) => row.branch.as_deref(),
        }
    }

    /// What luneta's Delete group takes away on the row: its pane, or on a
    /// session's row, its session; or on another machine's header, the
    /// machine. None on this one's, which luneta lists always.
    fn removes(&self) -> Option<Removes> {
        match self {
            Target::Session(_, session) => {
                let session = session.clone();
                Some(Removes::Row { session, scope: Scope::Session, merged_in: None })
            },
            Target::Pane(_, row) => Some(Removes::Row {
                session: row.session.clone(),
                scope: Scope::Pane(row.pane),
                merged_in: row
                    .repository
                    .as_ref()
                    .and(row.cwd.clone())
                    .and_then(|cwd| Directory::parse(cwd).ok()),
            }),
            Target::Machine(Machine::Remote(destination)) => {
                Some(Removes::Machine(destination.clone()))
            },
            Target::Machine(Machine::Local) => None,
        }
    }

    /// What a key of the Brief group briefs you on, as `scope` says, by the
    /// row, of whichever machine: every session of every machine; or on a
    /// machine's header, its sessions; or the row's session, or the agent in
    /// its pane.
    fn brief(&self, scope: brief::Scope) -> Briefed {
        match (self, scope) {
            (_, brief::Scope::Every) => Briefed::Every,
            (Target::Machine(machine), brief::Scope::Session | brief::Scope::Highlighted) => {
                Briefed::Machine(machine.clone())
            },
            (Target::Pane(machine, row), brief::Scope::Highlighted) => {
                Briefed::Agent(machine.clone(), row.session.name().clone(), row.pane)
            },
            (
                Target::Session(machine, session)
                | Target::Pane(machine, PaneRow { session, .. }),
                brief::Scope::Session | brief::Scope::Highlighted,
            ) => Briefed::Session(machine.clone(), session.name().clone()),
        }
    }
}

/// A pane's row, as a key opens what it opens beside the pane: its session,
/// the pane, and the directory it works in, where a tab or a session opens
/// too.
struct Beside {
    session: Listed,
    pane: u32,
    cwd: Directory,
}

impl PaneRow {
    /// The pane a key starts an agent or opens a pane beside, and the
    /// directory it works in, where a tab or a session opens too, or why
    /// there is none: a pane the daemon has said no directory of yet, nor of
    /// an agent in it, opens nothing, rather than wherever zellij runs, nor
    /// one it said by no absolute path.
    fn beside(&self) -> Result<Beside, String> {
        let pane = self.pane;
        let Some(cwd) = self.cwd.clone() else {
            return Err(format!(
                "menu: luneta does not know yet where pane {pane} works; try again in a moment"
            ));
        };
        let cwd =
            Directory::parse(cwd).map_err(|why| format!("menu: where pane {pane} works, {why}"))?;
        Ok(Beside { session: self.session.clone(), pane, cwd })
    }

    /// The pull request a key that starts an agent on its checks that failed
    /// asks of, by its page, beside the pane, or why there is none: one open,
    /// or a draft, that its daemon says the page of, of its number.
    fn pulled(&self) -> Result<(Beside, PullPage), String> {
        let beside = self.beside()?;
        let pane = beside.pane;
        let Some(PullRequest { number, state, url, .. }) = &self.pull_request else {
            return Err(format!("menu: gh has found no pull request of pane {pane}'s branch"));
        };
        match (state, url) {
            (State::Open | State::Draft, Some(url)) => {
                let pull = PullPage::parse(url);
                let pull = pull.map_err(|why| format!("menu: #{number}: {why}"))?;
                if pull.number() != *number {
                    return Err(format!("menu: the page of #{number} is of #{}", pull.number()));
                }
                Ok((beside, pull))
            },
            (State::Merged, _) => Err(format!("menu: #{number} is merged")),
            (State::Closed, _) => Err(format!("menu: #{number} is closed")),
            (State::Open | State::Draft, None) => Err(format!(
                "menu: pane {pane}'s lunetad does not say the page of #{number}, as an older \
                 one does not"
            )),
        }
    }

    /// What a key that starts an agent on what the pane's shell printed
    /// last starts it on, beside the pane, in the directory it works in, or
    /// why it starts none: the shell must be at its prompt, after a command
    /// whose end its hook said.
    fn ended(&self) -> Result<(Beside, Ended), String> {
        let beside = self.beside()?;
        let pane = beside.pane;
        match &self.ran {
            Ran::Ended(ended) => Ok((beside, ended.clone())),
            Ran::Going(command) => Err(format!(
                "menu: pane {pane} runs `{}` still, and what it prints is not over",
                command.as_str()
            )),
            Ran::Stopped(command) => Err(format!(
                "menu: `{}` is stopped in pane {pane}, and not over; bring it back with `fg`",
                command.as_str()
            )),
            Ran::Unknown => Err(format!(
                "menu: what pane {pane}'s shell ran is in words this luneta cannot read, as a \
                 newer lunetad's"
            )),
            Ran::Ready => Err(format!("menu: pane {pane}'s shell has run nothing yet")),
            Ran::Unhooked if self.session.own() => Err(format!(
                "menu: pane {pane}'s shell marks none of the commands it runs; a shell that \
                 writes OSC 133, as fish 4 does, marks them"
            )),
            Ran::Unhooked => Err(format!(
                "menu: pane {pane} runs no shell with luneta's hook; a fish started since the \
                 hook was installed has it"
            )),
        }
    }
}

/// What a key of the New group opens in the menu's session, or an agent a
/// skill starts. A new session is none of them: luneta goes to that itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opens {
    /// `command`, or your shell for none, in a pane beside the pane `beside`.
    Pane { beside: u32, command: Vec<String> },
    /// A tab of the pane's session, named `name` or as zellij numbers it,
    /// with `command` in it, or your shell for none.
    Tab { name: Option<String>, command: Vec<String> },
}

impl Opens {
    /// An agent that runs `command`, beside the pane `beside` of `session`:
    /// in a tab of its own, in an own session, which opens no pane beside
    /// another.
    fn agent(session: &Listed, beside: u32, command: Vec<String>) -> Opens {
        if session.own() {
            Opens::Tab { name: None, command }
        } else {
            Opens::Pane { beside, command }
        }
    }
}

/// What a key of the menu leaves the sidebar to do: the menu open still,
/// and what it wants asked for, or what is left to do once it closed.
#[expect(clippy::large_enum_variant, reason = "it lasts a key; a box would allocate on each")]
pub enum Did {
    Stays(Menu, Wants),
    Closed(Done),
}

/// What a menu that stays open wants the sidebar to ask for.
#[derive(Debug, PartialEq, Eq)]
pub enum Wants {
    Nothing,
    /// The harness picked for the agents the menu starts kept, for every
    /// luneta.
    Keep(Harness),
    /// The skills that `cwd` can run in `agent`, for the search box.
    Skills { cwd: String, agent: Harness },
    /// The harness picked for the agents the menu starts kept, as `Keep`
    /// does, and the skills `cwd` can run in it, for the search box, which
    /// waits on them again.
    Refind { cwd: String, agent: Harness },
    /// What the delete key deletes planned, for `planned`: for a pane, once
    /// zellij has said what closing it closes: of `session`.
    Plan { session: Listed, scope: Scope },
    /// What deleting the merged worktrees of the repository `dir` is in
    /// deletes, for `prunes`.
    Prunes { dir: Directory },
    /// What would go of the repository `dir` is in deleted where that is
    /// `agreed`, for `pruned`.
    Prune { dir: Directory, agreed: BTreeSet<prunes::Deleting> },
    /// zellij to open what `opens` says, in `cwd`, in `session`, for luneta
    /// to take its client there: from its pane `pane`, the menu's row's,
    /// which it takes the client to first, for a tab of a session it is
    /// not in.
    Open { session: Listed, pane: u32, cwd: Directory, opens: Opens },
    /// zellij to list the tabs of `session` other than that of its pane
    /// `pane`, for `tabs`.
    Tabs { session: Listed, pane: u32 },
    /// zellij to turn the pane `pane` of `session` as `turn` says, for luneta
    /// to take its client there.
    Turn { session: Listed, pane: u32, turn: Turn },
    /// luneta to find where a key of the menu for the pull request, the issue
    /// or the file goes, and to have its worktree made, for `pulled`.
    Pull(Work, Wanted),
    /// luneta to keep what the shell in the pane `pane` of `session` printed
    /// after the line `ended` was typed on, for `printed`.
    Printed { session: Listed, pane: u32, ended: Ended },
    /// luneta to keep the log of each check that failed on the head of the
    /// pull request of `page`, for `checked`.
    Checks { page: PullPage },
    /// zellij to make the session `session`, that starts in `cwd`, for luneta
    /// to take its client there, and then to open a tab of it in `tab_cwd`,
    /// named `name`, with `command` in it, or your shell for none, in place
    /// of the tabs zellij makes it with, for luneta to take its client there
    /// in turn.
    Make {
        session: SessionName,
        cwd: Directory,
        tab_cwd: Directory,
        name: String,
        command: Vec<String>,
    },
}

/// What a menu that closed leaves the sidebar to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Nothing,
    /// Say why what a key asked went wrong.
    Wrong(String),
    /// Take luneta's zellij to a session it makes, on the machine, that
    /// starts where `Starts` says.
    Start(Machine, Starts),
    /// Take luneta's client to an own session it makes on this machine, that
    /// starts where `Starts` says.
    Own(Starts),
    /// Take luneta's zellij to the pane that has the pull request's branch
    /// checked out, on the machine.
    Go(Machine, Place),
    /// Have zellij on the machine do what a delete key planned for a session
    /// there: for the session luneta's zellij is in, once it has left it.
    Act(Machine, Act),
    /// Brief you on what the agents it covers are doing.
    Brief(Briefed),
    /// Open the ask box, for the row the menu was for.
    Ask(Target),
    /// Open the issue box, for the repository of the row the menu was for.
    Issue(Repository),
    /// Have lunetad watch the machine ssh reaches by this destination.
    Ssh(Destination),
    /// Have lunetad stop watching this machine.
    Forget(Destination),
}

/// What a key of the Brief group briefs you on, by the row the menu was for.
#[derive(Debug, PartialEq, Eq)]
pub enum Briefed {
    /// The agents of every session of every machine.
    Every,
    /// The agents of every session of a machine.
    Machine(Machine),
    /// The agents of a session of a machine.
    Session(Machine, SessionName),
    /// The agent in the pane of a session of a machine.
    Agent(Machine, SessionName, u32),
}

/// What a click on a spot of the menu does: presses the key of an entry, or
/// one the line of keys names, or picks the skill so named of those its
/// search finds, as `Enter` does with the cursor on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Key(KeyEvent),
    Skill(Name),
}

/// The menu at the bottom of the sidebar, for the row highlighted when it
/// opened, or floating over zellij, for the pane zellij was on.
pub struct Menu {
    /// Its number among the menus the sidebar has opened, which what zellij
    /// answers for it carries.
    number: u64,
    target: Target,
    stage: Stage,
}

enum Stage {
    /// The keys of the group open, pressed one after another.
    Choosing(Keys),
    /// A delete key waits for what it deletes to be planned: for zellij to
    /// say what closing a pane closes, before it asks, and again before it
    /// goes ahead once you said yes to `agreed`, what it asked of.
    Planning { keys: Keys, delete: Delete, agreed: Option<Plan> },
    /// A delete key that asks before it goes ahead: what it would do, and
    /// what it asks.
    Asking { keys: Keys, delete: Delete, plan: Plan, asks: String },
    /// `d m` waits to hear what deleting the merged worktrees of the repository
    /// `dir` is in deletes, before it asks. Keys do nothing but close the menu.
    Sweeping { keys: Keys, dir: Directory },
    /// What `d m` would delete, for you to say yes to.
    Swept { keys: Keys, dir: Directory, plan: prunes::Plan },
    /// `d m` deletes the `count` worktrees you said yes to, of the repository
    /// `dir` is in, or hears that what would go changed. Keys do nothing but close
    /// the menu, which does not stop it.
    Pruning { keys: Keys, dir: Directory, count: usize },
    /// zellij opens what a key asked for, in `session`, for luneta to take
    /// its client there. Keys do nothing but close the menu.
    Opening { keys: Keys, session: Listed },
    /// A key of the Turn group waits for zellij to list the tabs of `session`
    /// other than that of its pane `pane`. Keys do nothing but close the menu.
    Listing { keys: Keys, session: Listed, pane: u32 },
    /// The tabs of `session` the tab of its pane `pane` can turn into, on a
    /// key each, which you pick one of.
    Picking { keys: Keys, session: Listed, pane: u32, tabs: Vec<OtherTab> },
    /// zellij turns what a key of the Turn group asked for, in `session`, for
    /// luneta to take its client there. Keys do nothing but close the menu.
    Turning { keys: Keys, session: Listed },
    /// The key `last` waits for luneta to keep what the pane's shell
    /// printed after `ended`, for an agent on it beside the pane. Keys do
    /// nothing but close the menu.
    Reading { keys: Keys, last: Last, beside: Beside, ended: Ended },
    /// `c c` waits for luneta to keep the log of each check that failed on
    /// the head of the pull request of `page`, for an agent on them beside
    /// the pane. Keys do nothing but close the menu.
    Fetching { keys: Keys, beside: Beside, page: PullPage },
    /// The menu for `work`, which a paste opened, at `step`, whose agent
    /// starts in `agent`. Once luneta has found where its keys go, its target
    /// is there.
    Pull { work: Work, agent: Harness, step: Step },
    /// The box `n m` opens, with what is typed in it for ssh to reach a
    /// machine by, and why that is no destination, once `Enter` found it is
    /// not, until the next key.
    Ssh { keys: Keys, typed: String, wrong: Option<String> },
}

/// Where a menu waits for zellij to open what a key asked for: in a session
/// lunetad lists, or in one zellij makes for it, by its name; or to turn what
/// a key of the Turn group asked for, in a session lunetad lists.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening {
    In(Listed),
    Made(SessionName),
    Turned(Listed),
}

/// How far the menu for a pull request, an issue or a file is, with its keys,
/// `w` and `r`, or `w` and `t`.
enum Step {
    /// Its keys wait for one.
    Choosing { wrong: Option<String> },
    /// A key waits, on what it says after a spinner that turns. Keys do
    /// nothing but close the menu.
    Waiting(Wait),
}

/// What a key of the menu for a pull request, an issue or a file waits on.
enum Wait {
    /// The key `pull` waits for luneta to find where it goes, and for `wt
    /// add` to make the worktree.
    Pulling(Pull),
    /// zellij opens what the key asked for, in this session, for luneta to
    /// take its client there.
    Opening(Listed),
    /// zellij makes this session, and opens a tab of it, for luneta to take
    /// its client there.
    Making(SessionName),
}

impl Wait {
    /// What luneta waits on for `work`.
    fn doing(&self, work: &Work) -> String {
        match self {
            Wait::Pulling(pull) => match pull.wanted(Asks::of(work)) {
                Wanted::AtHead => format!("Syncing {}…", work.tab()),
                Wanted::AsItIs => format!("Making a worktree of {}…", work.tab()),
            },
            Wait::Opening(_) => format!("Opening {}…", work.tab()),
            Wait::Making(session) => format!("Making session {session}…"),
        }
    }
}

impl Menu {
    /// The menu numbered `number` for `target`, on the group `key` opens, or
    /// on the first with no key, and what a key that does more than open a
    /// group leaves the sidebar to do, its keys starting agents in `agent`.
    /// None, and no menu, for a key the menu has not.
    pub fn open(number: u64, target: Target, key: Option<char>, agent: Picked) -> Option<Did> {
        let keys = Keys::new(&target, agent);
        if key.is_some_and(|key| keys.root.iter().all(|entry| entry.key() != key)) {
            return None;
        }
        let menu = Menu { number, target, stage: Stage::Choosing(keys) };
        Some(match key {
            Some(key) => menu.key(&KeyEvent::from(KeyCode::Char(key))),
            None => Did::Stays(menu, Wants::Nothing),
        })
    }

    /// The menu numbered `number` for `target`, on its first group, its keys
    /// starting agents in `agent`.
    pub fn new(number: u64, target: Target, agent: Picked) -> Menu {
        let keys = Keys::new(&target, agent);
        Menu { number, target, stage: Stage::Choosing(keys) }
    }

    /// The menu numbered `number` for `work`, pasted with the menu for
    /// `target` open, or the row highlighted, whose agent starts in `agent`.
    pub fn pull(number: u64, target: Target, work: Work, agent: Harness) -> Menu {
        let step = Step::Choosing { wrong: None };
        Menu { number, target, stage: Stage::Pull { work, agent, step } }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The pane a delete key waits on, for zellij to say what closing it
    /// closes, and its session. A session's is planned at once.
    pub fn planning(&self) -> Option<(&Listed, u32)> {
        match &self.stage {
            Stage::Planning { delete: Delete { session, scope: Scope::Pane(pane), .. }, .. } => {
                Some((session, *pane))
            },
            Stage::Planning { .. }
            | Stage::Choosing(_)
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// Where the menu waits for zellij to open what a key asked for, or to
    /// turn it, while it does.
    pub fn opening(&self) -> Option<Opening> {
        match &self.stage {
            Stage::Opening { session, .. }
            | Stage::Pull { step: Step::Waiting(Wait::Opening(session)), .. } => {
                Some(Opening::In(session.clone()))
            },
            Stage::Pull { step: Step::Waiting(Wait::Making(session)), .. } => {
                Some(Opening::Made(session.clone()))
            },
            Stage::Turning { session, .. } => Some(Opening::Turned(session.clone())),
            Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// Whether `d m` waits to hear what it would delete.
    pub fn sweeping(&self) -> bool {
        matches!(self.stage, Stage::Sweeping { .. })
    }

    /// Whether `d m` waits for what you said yes to to be deleted.
    pub fn pruning(&self) -> bool {
        matches!(self.stage, Stage::Pruning { .. })
    }

    /// Whether a key of the Turn group waits for zellij to list the other
    /// tabs.
    pub fn listing(&self) -> bool {
        matches!(self.stage, Stage::Listing { .. })
    }

    /// Whether a key waits for luneta to keep what the pane's shell printed.
    pub fn reading(&self) -> bool {
        matches!(self.stage, Stage::Reading { .. })
    }

    /// Whether `c c` waits for luneta to keep the logs of the checks that
    /// failed.
    pub fn fetching(&self) -> bool {
        matches!(self.stage, Stage::Fetching { .. })
    }

    /// Whether a key of the menu for a pull request, an issue or a file waits
    /// for luneta to find where it goes.
    pub fn pulling(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Waiting(Wait::Pulling(_)), .. })
    }

    /// Whether the menu waits on what a key of it started: the work itself,
    /// as `wt add` makes a worktree, the agent that starts once luneta kept
    /// what a pane printed or the checks that failed, or zellij opening or
    /// turning what the key asked for. The sidebar keeps such a menu going
    /// once the keys leave it, so that what the key started does not end with
    /// the menu: the tab opens and the agent starts where they would have, and
    /// the notes say where, while luneta's client goes nowhere.
    pub fn waits(&self) -> bool {
        matches!(
            self.stage,
            Stage::Sweeping { .. }
                | Stage::Pruning { .. }
                | Stage::Reading { .. }
                | Stage::Fetching { .. }
                | Stage::Opening { .. }
                | Stage::Listing { .. }
                | Stage::Turning { .. }
                | Stage::Pull { step: Step::Waiting(_), .. }
        )
    }

    /// What the menu waits on, in words, for the notes while the keys are
    /// elsewhere: none for what it shows itself while it is open.
    pub fn doing(&self) -> Option<String> {
        match &self.stage {
            Stage::Sweeping { .. } => Some(sweeping(self.target.repository_name())),
            Stage::Pruning { count, .. } => Some(pruning(*count)),
            Stage::Reading { ended, .. } => Some(reading(ended)),
            Stage::Fetching { page, .. } => Some(fetching(page.number())),
            Stage::Pull { work, step: Step::Waiting(wait), .. } => Some(wait.doing(work)),
            Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Swept { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Pull { step: Step::Choosing { .. }, .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// What the menu is for, while it is for what was pasted and a key of it
    /// is making the worktree: pasting the same link again makes nothing of
    /// its own meanwhile, which would be a second worktree of one.
    pub fn making(&self) -> Option<&Work> {
        match &self.stage {
            Stage::Pull { work, step: Step::Waiting(Wait::Pulling(_)), .. } => Some(work),
            Stage::Pull {
                step: Step::Choosing { .. } | Step::Waiting(Wait::Opening(_) | Wait::Making(_)),
                ..
            }
            | Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// What the notes say of the menu, once the keys left it and an answer
    /// left it waiting on keys: what it would have shown, and how to have it
    /// again.
    pub fn left(&self) -> Option<String> {
        match &self.stage {
            Stage::Pull { work, step: Step::Choosing { wrong: Some(wrong) }, .. } => {
                Some(format!("menu: {wrong} once {} is pasted again", work.tab()))
            },
            Stage::Swept { .. } => Some(
                "menu: nothing deleted: the menu closed before you said yes; d m asks again"
                    .to_owned(),
            ),
            Stage::Picking { session, tabs, .. } => Some(format!(
                "menu: not turned: {} has {} other tabs to pick from; t p asks again",
                session.name(),
                tabs.len()
            )),
            Stage::Pull { step: Step::Choosing { wrong: None } | Step::Waiting(_), .. }
            | Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// Types `text`, pasted, on one line, in the box for a machine's
    /// destination, and says whether that is open to type it in.
    pub fn paste(&mut self, text: &str) -> bool {
        let Stage::Ssh { typed, wrong, .. } = &mut self.stage else {
            return false;
        };
        typed.extend(text.chars().filter(|c| !c.is_control()));
        *wrong = None;
        true
    }

    /// What `key` does to the menu, and what that leaves the sidebar to do.
    /// What a key opens, it opens beside the pane of the menu's row, in the
    /// directory it works in, and the menu waits for zellij to say it has,
    /// but for a session, which leaves the menu done: on a machine's header,
    /// one in the home of its user. A key the menu does not list for its row
    /// does nothing, as `Entry::on` says.
    pub fn key(self, key: &KeyEvent) -> Did {
        let Menu { number, target, stage } = self;
        let stays = |stage: Stage, wants: Wants| {
            Did::Stays(Menu { number, target: target.clone(), stage }, wants)
        };
        match stage {
            Stage::Choosing(mut keys) => match (keys.key(key), &target) {
                (Pressed::Close, _) => Did::Closed(Done::Nothing),
                (Pressed::Switched(agent), Target::Pane(_, row))
                    if let Some(search) = &mut keys.search =>
                {
                    search.refind();
                    let wants = match row.beside() {
                        Ok(Beside { cwd, .. }) => Wants::Refind { cwd: cwd.into(), agent },
                        Err(_) => Wants::Keep(agent),
                    };
                    stays(Stage::Choosing(keys), wants)
                },
                (Pressed::Switched(agent), _) => stays(Stage::Choosing(keys), Wants::Keep(agent)),
                (Pressed::Moved | Pressed::Nothing, _)
                | (
                    Pressed::Search
                    | Pressed::Start(_)
                    | Pressed::Last(_)
                    | Pressed::Checks
                    | Pressed::New(New::Pane | New::Tab)
                    | Pressed::Turn(_),
                    Target::Session(..) | Target::Machine(_),
                )
                | (Pressed::New(New::Session | New::Own), Target::Session(..)) => {
                    stays(Stage::Choosing(keys), Wants::Nothing)
                },
                (Pressed::Search, Target::Pane(_, row)) => match row.beside() {
                    Ok(Beside { cwd, .. }) => {
                        keys.search = Some(Search::new());
                        let wants = Wants::Skills { cwd: cwd.into(), agent: keys.agent.harness() };
                        stays(Stage::Choosing(keys), wants)
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::Start(skill), Target::Pane(_, row)) => match row.beside() {
                    Ok(Beside { session, pane, cwd }) => {
                        let command = agents::on_skill(keys.agent.harness(), &skill, None);
                        let opens = Opens::agent(&session, pane, command);
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::Last(last), Target::Pane(_, row)) => match row.ended() {
                    Ok((beside, ended)) => {
                        let (session, pane) = (beside.session.clone(), beside.pane);
                        let wants = Wants::Printed { session, pane, ended: ended.clone() };
                        stays(Stage::Reading { keys, last, beside, ended }, wants)
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::Checks, Target::Pane(_, row)) => match row.pulled() {
                    Ok((beside, page)) => {
                        let wants = Wants::Checks { page: page.clone() };
                        stays(Stage::Fetching { keys, beside, page }, wants)
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::New(New::Machine), _) => {
                    let stage = Stage::Ssh { keys, typed: String::new(), wrong: None };
                    stays(stage, Wants::Nothing)
                },
                (Pressed::New(New::Session), Target::Machine(machine)) => {
                    Did::Closed(Done::Start(machine.clone(), Starts::Home))
                },
                (Pressed::New(New::Pane), Target::Pane(_, row)) => match row.beside() {
                    Ok(Beside { session, pane, cwd }) => {
                        let opens = Opens::Pane { beside: pane, command: Vec::new() };
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::New(New::Tab), Target::Pane(_, row)) => match row.beside() {
                    Ok(Beside { session, pane, cwd }) => {
                        let opens = Opens::Tab { name: None, command: Vec::new() };
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::New(New::Session), Target::Pane(machine, row)) => match row.beside() {
                    Ok(Beside { cwd, .. }) => {
                        Did::Closed(Done::Start(machine.clone(), Starts::In(cwd)))
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::New(New::Own), Target::Machine(Machine::Local)) => {
                    Did::Closed(Done::Own(Starts::Home))
                },
                (Pressed::New(New::Own), Target::Pane(Machine::Local, row)) => match row.beside() {
                    Ok(Beside { cwd, .. }) => Did::Closed(Done::Own(Starts::In(cwd))),
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                (Pressed::New(New::Own), Target::Machine(_) | Target::Pane(..)) => {
                    Did::Closed(Done::Wrong(OWN_HERE.to_owned()))
                },
                (Pressed::Turn(turning), Target::Pane(_, row)) => {
                    let (session, pane) = (row.session.clone(), row.pane);
                    match turning {
                        TurnKey::PaneIntoTab => {
                            let stage = Stage::Turning { keys, session: session.clone() };
                            stays(stage, Wants::Turn { session, pane, turn: Turn::PaneIntoTab })
                        },
                        TurnKey::TabIntoPanes => {
                            let stage = Stage::Listing { keys, session: session.clone(), pane };
                            stays(stage, Wants::Tabs { session, pane })
                        },
                    }
                },
                (Pressed::Delete { key, session, scope }, _) => {
                    let wants = Wants::Plan { session: session.clone(), scope };
                    let delete = Delete { key, session, scope };
                    stays(Stage::Planning { keys, delete, agreed: None }, wants)
                },
                (Pressed::Prune(dir), _) => {
                    let wants = Wants::Prunes { dir: dir.clone() };
                    stays(Stage::Sweeping { keys, dir }, wants)
                },
                (Pressed::Forget(destination), _) => Did::Closed(Done::Forget(destination)),
                (Pressed::Brief(scope), _) => Did::Closed(Done::Brief(target.brief(scope))),
                (Pressed::Ask, _) => Did::Closed(Done::Ask(target.clone())),
                (Pressed::Issue, _) => match target.repository() {
                    Some(repository) => Did::Closed(Done::Issue(repository)),
                    None => Did::Closed(Done::Nothing),
                },
            },
            Stage::Ssh { keys, mut typed, .. } => match ssh_key(key, &mut typed) {
                Typed::Close => Did::Closed(Done::Nothing),
                Typed::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Typed::Typing => stays(Stage::Ssh { keys, typed, wrong: None }, Wants::Nothing),
                Typed::Enter => match typed.parse::<Destination>() {
                    Ok(destination) => Did::Closed(Done::Ssh(destination)),
                    Err(why) => {
                        let wrong = Some(format!("not a destination: {why}"));
                        stays(Stage::Ssh { keys, typed, wrong }, Wants::Nothing)
                    },
                },
            },
            Stage::Planning { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
                if closes(key) =>
            {
                Did::Closed(Done::Nothing)
            },
            Stage::Picking { keys, .. } if key.code == KeyCode::Backspace && bare(key) => {
                stays(Stage::Choosing(keys), Wants::Nothing)
            },
            Stage::Picking { keys, session, pane, tabs } => match picked(key, &tabs) {
                Some(beside) => {
                    let turn = Turn::TabIntoPanes { beside };
                    let stage = Stage::Turning { keys, session: session.clone() };
                    stays(stage, Wants::Turn { session, pane, turn })
                },
                None => stays(Stage::Picking { keys, session, pane, tabs }, Wants::Nothing),
            },
            // `Backspace` goes out of the menu from its first group, and the
            // menu for a pull request, an issue or a file has no other.
            Stage::Pull { step: Step::Choosing { .. }, .. }
                if key.code == KeyCode::Backspace && bare(key) =>
            {
                Did::Closed(Done::Nothing)
            },
            Stage::Pull { work, agent, step: step @ Step::Choosing { .. } }
                if key.code == KeyCode::Tab && bare(key) =>
            {
                let agent = agents::next(agent);
                stays(Stage::Pull { work, agent, step }, Wants::Keep(agent))
            },
            Stage::Pull { work, agent, step: step @ Step::Choosing { .. } } => {
                let asks = Asks::of(&work);
                match Pull::pressed(key, asks) {
                    Some(pull) => {
                        let wants = Wants::Pull(work.clone(), pull.wanted(asks));
                        let step = Step::Waiting(Wait::Pulling(pull));
                        stays(Stage::Pull { work, agent, step }, wants)
                    },
                    None => stays(Stage::Pull { work, agent, step }, Wants::Nothing),
                }
            },
            stage @ (Stage::Planning { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }) => stays(stage, Wants::Nothing),
            Stage::Swept { keys, dir, plan } => match answer(key, DELETE_MERGED) {
                Answer::Yes if !plan.goes.is_empty() => {
                    let wants = Wants::Prune { dir: dir.clone(), agreed: plan.deletes() };
                    stays(Stage::Pruning { keys, dir, count: plan.goes.len() }, wants)
                },
                Answer::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Answer::Close => Did::Closed(Done::Nothing),
                Answer::Yes | Answer::Nothing => {
                    stays(Stage::Swept { keys, dir, plan }, Wants::Nothing)
                },
            },
            Stage::Asking { keys, delete, plan, asks } => match answer(key, delete.key) {
                Answer::Yes => {
                    let session = delete.session.clone();
                    let wants = Wants::Plan { session, scope: delete.scope };
                    stays(Stage::Planning { keys, delete, agreed: Some(plan) }, wants)
                },
                Answer::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Answer::Close => Did::Closed(Done::Nothing),
                Answer::Nothing => {
                    stays(Stage::Asking { keys, delete, plan, asks }, Wants::Nothing)
                },
            },
        }
    }

    /// What the key of the Turn group that waits for the other tabs of its
    /// session does, as `tabs` says, or why zellij did not say: its tab's
    /// panes turn into those of the tab there is, and the menu waits for
    /// zellij to turn them, or it lists the tabs there are, for you to pick
    /// one. With none, nothing turns.
    pub fn tabs(self, tabs: Result<Vec<OtherTab>, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Listing { keys, session, pane } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let tabs = match tabs {
            Ok(tabs) => tabs,
            Err(why) => return Did::Closed(Done::Wrong(format!("menu: not turned: {why}"))),
        };
        match tabs.as_slice() {
            [] => Did::Closed(Done::Wrong(format!(
                "menu: not turned: {} has no other tab for pane {pane}'s to turn into",
                session.name()
            ))),
            [only] => {
                let turn = Turn::TabIntoPanes { beside: only.pane };
                let stage = Stage::Turning { keys, session: session.clone() };
                let wants = Wants::Turn { session, pane, turn };
                Did::Stays(Menu { number, target, stage }, wants)
            },
            _ => {
                let stage = Stage::Picking { keys, session, pane, tabs };
                Did::Stays(Menu { number, target, stage }, Wants::Nothing)
            },
        }
    }

    /// What the delete key the menu waits on does, as `plan` says: asks
    /// first, or goes ahead, or says why it does nothing. Going ahead after
    /// you said yes does what you were asked about: the same, ending the
    /// same agents, doing the same. Once that changed while you read, it
    /// asks again, while an agent at work would end.
    pub fn planned(self, plan: Result<Plan, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Planning { keys, delete, agreed } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(reason) => return Did::Closed(Done::Wrong(reason)),
        };
        let asks = if agreed.as_ref() == Some(&plan) { None } else { plan.question() };
        if let Some(asks) = asks {
            let stage = Stage::Asking { keys, delete, plan, asks };
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        }
        Did::Closed(Done::Act(target.machine().clone(), plan.act))
    }

    /// What `d m` does, now that it heard what it would delete, as `plan` says,
    /// or why it did not: lists it for you to say yes to.
    pub fn swept(self, plan: Result<prunes::Plan, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Sweeping { keys, dir } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(why) => return Did::Closed(Done::Wrong(format!("menu: nothing deleted: {why}"))),
        };
        Did::Stays(Menu { number, target, stage: Stage::Swept { keys, dir, plan } }, Wants::Nothing)
    }

    /// What `d m` does, now that what it would delete has changed from what you
    /// said yes to, as `plan` says, and nothing went: lists it again.
    pub fn changed(self, plan: prunes::Plan) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Pruning { keys, dir, .. } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        Did::Stays(Menu { number, target, stage: Stage::Swept { keys, dir, plan } }, Wants::Nothing)
    }

    /// What the key of the menu for a pull request, an issue or a file that
    /// waits does, now that luneta found where it goes, as `found` says, or
    /// why it found nothing: on the machine it found it on, the row's or
    /// another that has a link's repository. `w` takes luneta's zellij to the
    /// pane that has the
    /// branch checked out, or opens a tab in the worktree, named for what was
    /// pasted, in the session that works in its repository, or one luneta
    /// makes; `r` opens an agent on a review of the pull request beside that
    /// pane, or in that tab, and `t` one on tackling the issue or the file,
    /// let read the file where it is. The menu is then for where it opens
    /// that, and waits for zellij to open it.
    pub fn pulled(self, found: Result<Found, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Pull { work, agent, step: Step::Waiting(Wait::Pulling(pull)) } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let Found { on, ready: Ready { at, worktree, branch, moved_off, work: readied } } =
            match found {
                Ok(found) => found,
                Err(why) => return Did::Closed(Done::Wrong(format!("menu: {work}: {why}"))),
            };
        let off = work.off_branch(branch.as_deref()).filter(|_| moved_off);
        if let (Pull::Agent, Some(off)) = (pull, off) {
            let wrong = Some(format!("{worktree} is {off}, so no agent starts there; w opens it"));
            let stage = Stage::Pull { work, agent, step: Step::Choosing { wrong } };
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        }
        let command = match (pull, &readied) {
            (Pull::Worktree, _) => Vec::new(),
            (Pull::Agent, Readied::Link(link)) if link.kind() == Kind::Pull => {
                agents::on_skill(agent, REVIEW, Some(&link.url()))
            },
            (Pull::Agent, Readied::Link(link)) => {
                agents::on_words(agent, format!("{TACKLE} {}", link.url()), &[])
            },
            (Pull::Agent, Readied::File { real }) => {
                agents::on_words(agent, format!("{TACKLE} {real}"), &[real.as_str()])
            },
        };
        let dir = match Directory::parse(worktree.clone()) {
            Ok(dir) => dir,
            Err(why) => return Did::Closed(Done::Wrong(format!("menu: {work}: {why}"))),
        };
        let opening = |session: Listed, pane: u32, opens: Opens| {
            let (cwd, branch, ran) = (Some(worktree.clone()), branch.clone(), Ran::Unhooked);
            let row = PaneRow {
                session: session.clone(),
                pane,
                cwd,
                branch,
                repository: None,
                pull_request: None,
                ran,
            };
            let target = Target::Pane(on.clone(), row);
            let step = Step::Waiting(Wait::Opening(session.clone()));
            let stage = Stage::Pull { work: work.clone(), agent, step };
            let cwd = dir.clone();
            Did::Stays(Menu { number, target, stage }, Wants::Open { session, pane, cwd, opens })
        };
        match (at, pull) {
            (At::There((session, pane)), Pull::Worktree) => {
                Did::Closed(Done::Go(on, Place::Pane(PanePlace { session, pane })))
            },
            (At::There((session, pane)), Pull::Agent) => {
                let opens = Opens::agent(&session, pane, command);
                opening(session, pane, opens)
            },
            (At::Session((session, pane)), _) => {
                let opens = Opens::Tab { name: Some(work.tab()), command };
                opening(session, pane, opens)
            },
            (At::Clone { cwd, name }, _) => {
                let (session, tab_cwd, tab) = (name.clone(), dir, work.tab());
                let wants = Wants::Make { session, cwd, tab_cwd, name: tab, command };
                let stage = Stage::Pull { work, agent, step: Step::Waiting(Wait::Making(name)) };
                // The session is the machine's, as zellij makes it, and the
                // menu is for that, until it has a pane of it.
                Did::Stays(Menu { number, target: Target::Machine(on), stage }, wants)
            },
        }
    }

    pub fn off_branch(&self, found: &Result<Found, String>) -> Option<String> {
        let Stage::Pull { work, step: Step::Waiting(Wait::Pulling(Pull::Worktree)), .. } =
            &self.stage
        else {
            return None;
        };
        let Ready { moved_off: true, branch, .. } = &found.as_ref().ok()?.ready else {
            return None;
        };
        Some(format!("{}'s worktree is {}", work.tab(), work.off_branch(branch.as_deref())?))
    }

    /// What the shell of the menu's row ran, as the daemon says it now: one
    /// may have ended, or begun, since the menu opened.
    pub fn reran(&mut self, ran: Ran) {
        if let Target::Pane(_, row) = &mut self.target {
            row.ran = ran;
        }
    }

    /// What the key that waits on what the pane's shell printed does, now
    /// that luneta kept it, as `printed` says, or why it kept nothing: opens
    /// an agent on it beside the pane, and waits for zellij to open it. One
    /// that the shell ran another command since, as the daemon says, or
    /// went, kept what that printed, or some of it, and opens none. One run
    /// in the moment before the daemon heard of it goes unseen.
    pub fn printed(self, printed: Result<Printed, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Reading { keys, last, beside: Beside { session, pane, cwd }, ended } = stage
        else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let printed = match printed {
            Ok(printed) => printed,
            Err(why) => {
                return Did::Closed(Done::Wrong(format!("menu: what pane {pane} printed: {why}")));
            },
        };
        let still = |row: &PaneRow| matches!(&row.ran, Ran::Ended(now) if *now == ended);
        if !matches!(&target, Target::Pane(_, row) if still(row)) {
            return Did::Closed(Done::Wrong(format!(
                "menu: pane {pane}'s shell went on as luneta read what `{}` printed; ask again",
                ended.command.as_str()
            )));
        }
        let prompt = printed::prompt(&printed, last.asks(), &ended);
        let files: Vec<&str> = printed.file().into_iter().collect();
        let command = agents::on_words(keys.agent.harness(), prompt, &files);
        let opens = Opens::agent(&session, pane, command);
        let stage = Stage::Opening { keys, session: session.clone() };
        let wants = Wants::Open { session, pane, cwd, opens };
        Did::Stays(Menu { number, target, stage }, wants)
    }

    /// What `c c` does, now that luneta kept the logs of the checks that
    /// failed, as `checks` says, or why it kept none: opens an agent on them
    /// beside the pane, and waits for zellij to open it. Where none failed,
    /// it opens none, and says so.
    pub fn checked(self, checks: Result<Failing, String>) -> Did {
        let Menu { number: menu, target, stage } = self;
        let Stage::Fetching { keys, beside: Beside { session, pane, cwd }, page } = stage else {
            return Did::Stays(Menu { number: menu, target, stage }, Wants::Nothing);
        };
        let number = page.number();
        let failing = match checks {
            Ok(failing) => failing,
            Err(why) => {
                return Did::Closed(Done::Wrong(format!("menu: the checks of #{number}: {why}")));
            },
        };
        if failing.failed.is_empty() {
            let pending = match failing.pending {
                0 => String::new(),
                1 => ", and one has yet to end".to_owned(),
                pending => format!(", and {pending} have yet to end"),
            };
            return Did::Closed(Done::Wrong(format!(
                "menu: no check failed on the head of #{number}{pending}"
            )));
        }
        let prompt = checks::prompt(&page, &failing);
        let command = agents::on_words(keys.agent.harness(), prompt, &checks::files(&failing));
        let opens = Opens::agent(&session, pane, command);
        let stage = Stage::Opening { keys, session: session.clone() };
        let wants = Wants::Open { session, pane, cwd, opens };
        Did::Stays(Menu { number: menu, target, stage }, wants)
    }

    /// Fills the search box with what the finder said, while it is open.
    /// What the finder found for `agent` is for the search box only while
    /// the menu starts agents in that harness: `Tab` asked for another's.
    pub fn found(&mut self, agent: Harness, skills: Result<Vec<crate::skills::Skill>, String>) {
        if let Stage::Choosing(keys) = &mut self.stage
            && keys.agent.harness() == agent
            && let Some(search) = &mut keys.search
        {
            search.fill(skills);
        }
    }

    /// What a click on the skill called `name` in the search box does: what
    /// `Enter` does with the cursor on it. Nothing where it matches no more,
    /// nor while the keys do nothing but close the menu.
    pub fn pick(mut self, name: &Name) -> Did {
        if let Stage::Choosing(keys) = &mut self.stage
            && let Some(search) = &mut keys.search
            && search.select(name)
        {
            return self.key(&KeyEvent::from(KeyCode::Enter));
        }
        Did::Stays(self, Wants::Nothing)
    }

    /// Moves the cursor of the search box `by` skills down, or up, as the
    /// wheel does over them.
    pub fn scroll(&mut self, by: isize) {
        if let Stage::Choosing(keys) = &mut self.stage
            && let Some(search) = &mut keys.search
        {
            search.move_by(by);
        }
    }

    /// What the menu shows at the bottom of the sidebar, `width` wide, in
    /// `room` lines at most: a rule that names the groups opened and says
    /// where the pane works, what the menu shows under it, and the keys that
    /// go back and close under them. Short of room, what it shows goes first,
    /// then the rule. And the spots on those lines, as `Shown::lines` says.
    /// A spinner that turns shows `frame`. Short of room for the keys of the
    /// group open, the last line under the rule says that `ALL_KEYS` lists
    /// the rest, as it does there.
    pub fn lines(
        &self,
        width: usize,
        room: usize,
        frame: usize,
    ) -> (Vec<Line<'static>>, Spots<Choice>) {
        let body = room.saturating_sub(2);
        let mut shown = self.shown(width, body, frame);
        if self.lists() && shown.body.len() > body && body > 0 {
            shown.body.truncate(body - 1);
            shown.spots = shown.spots.first(body - 1);
            let all = Choice::Key(KeyEvent::from(KeyCode::Char(ALL_KEYS)));
            shown.spots.add(body - 1, 0..width, all);
            shown.body.push(all_keys_line(width.saturating_sub(1)));
        }
        shown.lines(width, room)
    }

    /// Whether a key of the group open works on what it asked, the menu
    /// showing that group still, its keys doing nothing but close it: zellij
    /// plans a delete, opens or turns what the key asked for, or lists the
    /// tabs to turn into, or luneta keeps what a shell printed.
    pub fn quiet(&self) -> bool {
        matches!(
            self.stage,
            Stage::Planning { .. }
                | Stage::Opening { .. }
                | Stage::Listing { .. }
                | Stage::Turning { .. }
                | Stage::Reading { .. }
        )
    }

    /// Whether the keys of a group choose, with no search box open: where
    /// `ALL_KEYS` lists every key, as it types in the search box.
    pub fn lists(&self) -> bool {
        matches!(&self.stage, Stage::Choosing(keys) if keys.search.is_none())
    }

    /// Draws the menu floating at the bottom right of `over`, zellij's side,
    /// at `now`, as `float` does. The search box takes the most columns, so
    /// that it keeps its width as you type.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) -> Drawn {
        let frame = sidebar::frame(now);
        let shown = |width, body_room| self.shown(width, body_room, frame);
        let floated = float(buffer, over, self.searching(), shown);
        let drawn = |(area, spots)| Drawn::Menu { number: self.number, area: Some(area), spots };
        floated.map_or(Drawn::Nothing, drawn)
    }

    /// Whether its spinner turns: while a key of the menu for a pull request,
    /// an issue or a file waits for its worktree, and then for zellij to open
    /// it, and while `d m` waits.
    pub fn turning(&self) -> bool {
        matches!(
            self.stage,
            Stage::Pull { step: Step::Waiting(_), .. }
                | Stage::Sweeping { .. }
                | Stage::Pruning { .. }
        )
    }

    /// Whether the search box over the skills is open.
    fn searching(&self) -> bool {
        match &self.stage {
            Stage::Choosing(keys)
            | Stage::Planning { keys, .. }
            | Stage::Opening { keys, .. }
            | Stage::Reading { keys, .. } => keys.search.is_some(),
            // It keeps its width as you type too.
            Stage::Ssh { .. } => true,
            Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. } => false,
        }
    }

    /// What the menu shows `width` wide, with `body_room` lines at most for
    /// what it shows under the rule. The menu for a pull request, an issue or
    /// a file is named for it, and says its repository, or the file's path,
    /// where another says where its pane works. An entry, or a skill, is a
    /// spot for a click only while the keys choose, as they do nothing else
    /// but close the menu once a key opens something, or deletes. Once a key
    /// of the menu for a pull request, an issue or a file is pressed, it shows
    /// what luneta waits on for it, after the spinner at `frame`.
    fn shown(&self, width: usize, body_room: usize, frame: usize) -> Shown {
        let inner = width.saturating_sub(1);
        let right = self.target.cwd().map_or_default(sidebar::short_cwd);
        let mut spots = Spots::default();
        let (title, right, body, (footer, keys)) = match &self.stage {
            Stage::Pull { work, agent, step } => {
                let body = match step {
                    Step::Choosing { wrong } => {
                        let asks = Asks::of(work);
                        let entries = Pull::ALL.map(|pull| Entry::Pull(pull, asks));
                        spots = entry_spots(&entries, width).map(Choice::Key);
                        let mut body: Vec<_> =
                            entries.iter().map(|entry| entry_line(entry, inner)).collect();
                        spots.add(body.len(), 0..width, Choice::Key(KeyEvent::from(KeyCode::Tab)));
                        body.push(agent_line(Picked::ByYou(*agent), inner));
                        if let Some(wrong) = wrong {
                            body.extend(wrap(wrong, width, Style::new().fg(gruvbox::RED)));
                        }
                        body
                    },
                    Step::Waiting(wait) => {
                        waiting(&wait.doing(work), width, Style::new().fg(NAME), frame)
                    },
                };
                let from = match work {
                    Work::Link(link) => link.repository(),
                    Work::File(file) => sidebar::short_cwd(file.path()),
                };
                (work.tab(), from, body, keyed(width, &[(Hint::Esc, "Close")]))
            },
            Stage::Picking { tabs, .. } => {
                let entries = tab_entries(tabs);
                spots = entry_spots(&entries, width).map(Choice::Key);
                let body = entries.iter().map(|entry| entry_line(entry, inner)).collect();
                let back = [(Hint::Backspace, "Back"), (Hint::Esc, "Close")];
                (TABS_TITLE.to_owned(), right, body, keyed(width, &back))
            },
            Stage::Choosing(keys)
            | Stage::Planning { keys, .. }
            | Stage::Opening { keys, .. }
            | Stage::Listing { keys, .. }
            | Stage::Turning { keys, .. }
            | Stage::Reading { keys, .. } => {
                let choosing = matches!(self.stage, Stage::Choosing(_));
                let tab = Choice::Key(KeyEvent::from(KeyCode::Tab));
                if let Some(search) = &keys.search {
                    let moves =
                        [(Hint::Moves, "Move"), (Hint::Enter, "Start"), (Hint::Esc, "Back")];
                    // The line that names the agent goes under the skills,
                    // where it leaves room for the term and one of them.
                    let listing = body_room.checked_sub(1).filter(|room| *room > 1);
                    let (mut body, skills) =
                        search_lines(search, width, listing.unwrap_or(body_room));
                    if choosing {
                        spots = skills;
                    }
                    if listing.is_some() {
                        if choosing {
                            spots.add(body.len(), 0..width, tab);
                        }
                        body.push(agent_line(keys.agent, inner));
                    }
                    (SEARCH_TITLE.to_owned(), right, body, keyed(width, &moves))
                } else {
                    let back: &[(Hint, &str)] = if keys.open.is_empty() {
                        &[(Hint::Esc, "Close")]
                    } else {
                        &[(Hint::Backspace, "Back"), (Hint::Esc, "Close")]
                    };
                    let entries = keys.entries();
                    let mut body: Vec<_> =
                        entries.iter().map(|entry| entry_line(entry, inner)).collect();
                    if choosing {
                        spots = entry_spots(entries, width).map(Choice::Key);
                    }
                    if keys.starts() {
                        if choosing {
                            spots.add(body.len(), 0..width, tab);
                        }
                        body.push(agent_line(keys.agent, inner));
                    }
                    (title(keys), right, body, keyed(width, back))
                }
            },
            Stage::Asking { keys, asks, .. } => (
                title(keys),
                right,
                wrap(asks, width, Style::new().fg(NAME)),
                keyed(
                    width,
                    &[(Hint::Char('y'), "Yes"), (Hint::Backspace, "Back"), (Hint::Esc, "Close")],
                ),
            ),
            Stage::Sweeping { keys, .. } => {
                let repository = self.target.repository_name();
                let doing = sweeping(repository);
                let body = waiting(&doing, width, Style::new().fg(NAME), frame);
                (title(keys), repository.to_owned(), body, keyed(width, &[(Hint::Esc, "Close")]))
            },
            Stage::Fetching { keys, page, .. } => {
                let doing = fetching(page.number());
                let body = waiting(&doing, width, Style::new().fg(NAME), frame);
                (title(keys), right, body, keyed(width, &[(Hint::Esc, "Close")]))
            },
            Stage::Pruning { keys, count, .. } => {
                let doing = pruning(*count);
                let body = waiting(&doing, width, Style::new().fg(NAME), frame);
                let repository = self.target.repository_name().to_owned();
                (title(keys), repository, body, keyed(width, &[(Hint::Esc, "Close")]))
            },
            Stage::Swept { keys, plan, .. } => {
                let back = [(Hint::Backspace, "Back"), (Hint::Esc, "Close")];
                let yes = [(Hint::Char('y'), "Yes"), back[0], back[1]];
                let hints: &[(Hint, &str)] = if plan.goes.is_empty() { &back } else { &yes };
                let body = swept_lines(plan, width, body_room);
                (title(keys), plan.repository_name().to_owned(), body, keyed(width, hints))
            },
            Stage::Ssh { typed, wrong, .. } => {
                let typed = sidebar::tail(&format!("{typed}_"), width.saturating_sub(2));
                let mut body = vec![Line::from(vec![
                    Span::raw(" "),
                    Span::styled(typed, Style::new().fg(ACCENT)),
                ])];
                match wrong {
                    Some(wrong) => body.extend(wrap(wrong, width, Style::new().fg(gruvbox::RED))),
                    None => body.extend(wrap(SSH_HINT, width, Style::new().fg(NAME))),
                }
                let keys = [(Hint::Enter, "Add"), (Hint::Esc, "Back")];
                (SSH_TITLE.to_owned(), String::new(), body, keyed(width, &keys))
            },
        };
        Shown { title, right, body, spots, footer, keys }
    }
}

/// What the menu shows: the title of the rule over it and where its pane
/// works, at the rule's right, the lines under the rule, and the keys that go
/// back and close under them; and the spots on the lines under the rule, by
/// their place among them, and on the keys' line.
struct Shown {
    title: String,
    right: String,
    body: Vec<Line<'static>>,
    spots: Spots<Choice>,
    footer: Line<'static>,
    keys: Spots<KeyEvent>,
}

impl Shown {
    /// The columns it takes past the space each line starts with and one
    /// after: those of its widest line, and the rule's, with room for all of
    /// both its title and its right.
    fn takes(&self) -> usize {
        let lines = self.body.iter().chain([&self.footer]);
        let widest = lines.map(|line| line.width() + 1).max().unwrap_or(0);
        // `─ `, the title, a space and a `─`; and a space, the right, a space
        // and a last `─`, as `rule` has it.
        let tail = if self.right.is_empty() { 0 } else { self.right.width() + 3 };
        widest.max(4 + self.title.width() + tail)
    }

    /// Its lines, `width` wide, in `room` lines at most: the rule, what it
    /// shows under it, and the keys under them. Short of room, what it shows
    /// goes first, then the rule. And the spots on the lines that show, by
    /// their place among them.
    fn lines(self, width: usize, room: usize) -> (Vec<Line<'static>>, Spots<Choice>) {
        let Shown { title, right, body, spots, footer, keys } = self;
        let mut lines = Vec::with_capacity(room);
        if room >= 2 {
            lines.push(rule(&title, &right, width));
        }
        let ruled = lines.len();
        lines.extend(body.into_iter().take(room.saturating_sub(2)));
        let mut spots = spots.first(lines.len() - ruled).down(ruled);
        if room >= 1 {
            spots.extend(keys.map(Choice::Key).down(lines.len()));
            lines.push(footer);
        }
        (lines, spots)
    }
}

/// Draws what `shown` shows, for a width and the lines its body has room
/// for, floating at the bottom right of `over`, zellij's side: in a box as
/// wide as that takes, or the `widest` the box can be, within `FLOATING`,
/// and as tall as its lines, the rule over them its top. Says where the box
/// is, and the spots in it, where it had the room for one.
fn float(
    buffer: &mut Buffer,
    over: Rect,
    widest: bool,
    shown: impl Fn(usize, usize) -> Shown,
) -> Option<(Rect, Spots<Choice>)> {
    let (fewest, most) = FLOATING;
    let most = most.min(usize::from(over.width.saturating_sub(2)));
    // The rule is the box's top, and its bottom takes a line of its own.
    let room = usize::from(over.height.saturating_sub(1));
    if most == 0 || room < 2 {
        return None;
    }
    let inner =
        if widest { most } else { shown(most, room - 2).takes().clamp(fewest.min(most), most) };
    let (lines, spots) = shown(inner, room - 2).lines(inner, room);
    let (rule, rest) = lines.split_first()?;
    // Both fit `over`, as `inner` and `lines` were made to.
    let width = u16::try_from(inner + 2).unwrap_or(over.width);
    let height = u16::try_from(lines.len() + 1).unwrap_or(over.height);
    let area = Rect::new(over.right() - width, over.bottom() - height, width, height);
    boxed(buffer, area, rule, rest);
    // The lines go between the box's sides, from its top down.
    let inside = Rect::new(area.x + 1, area.y, width - 2, height - 1);
    Some((area, spots.placed(inside)))
}

/// Draws `note` floating at the bottom right of `over`, where the menu over
/// zellij floats: what went wrong as it did what a key asked, or why it did
/// not open, which any key or click closes.
pub fn draw_note(buffer: &mut Buffer, over: Rect, note: &str) {
    float(buffer, over, false, |width, _| Shown {
        title: TITLE.to_owned(),
        right: String::new(),
        body: wrap(note, width, Style::new().fg(sidebar::ERROR)),
        spots: Spots::default(),
        footer: keys_line(width, &[(Hint::AnyKey, "Close")]),
        keys: Spots::default(),
    });
}

/// Draws `lines` in a box that covers `area`, under `rule` between `┌` and
/// `┐` along its top, each line between `│` and `│`, and `─` along its
/// bottom, between `└` and `┘`. What it covers is blanked first.
pub fn boxed(buffer: &mut Buffer, area: Rect, rule: &Line<'_>, lines: &[Line<'_>]) {
    let (Some(inner), Some(last)) = (area.width.checked_sub(2), area.bottom().checked_sub(1)) else {
        return;
    };
    let dim = Style::new().add_modifier(Modifier::DIM);
    let plain = Style::reset().fg(gruvbox::FG1);
    for row in area.rows() {
        buffer.set_string(row.x, row.y, " ".repeat(usize::from(row.width)), plain);
    }
    let mut top = vec![Span::styled("┌", dim)];
    top.extend(rule.spans.iter().cloned());
    top.push(Span::styled("┐", dim));
    buffer.set_line(area.x, area.y, &Line::from(top), area.width);
    for (line, y) in lines.iter().zip(area.y + 1..last) {
        buffer.set_string(area.x, y, "│", dim);
        buffer.set_line(area.x + 1, y, line, inner);
        buffer.set_string(area.right() - 1, y, "│", dim);
    }
    buffer.set_string(area.x, last, format!("└{}┘", "─".repeat(usize::from(inner))), dim);
}

/// `text` on as many lines of `width` as it takes, a space in from the edge,
/// broken between words where it can be.
pub fn wrap(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    let room = width.saturating_sub(2).max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let spaced = usize::from(!line.is_empty());
        if !line.is_empty() && line.width() + spaced + word.width() > room {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    let lines = lines.into_iter().flat_map(|line| sidebar::wrapped(&line, width, style));
    lines.collect()
}

/// What luneta asks gh about the worktrees of `repository`, `d m`.
fn sweeping(repository: &str) -> String {
    format!("Asking gh about the worktrees of {repository}…")
}

/// What `d m` does with `count` merged worktrees.
fn pruning(count: usize) -> String {
    format!("Deleting {}…", merged_worktrees(count))
}

/// What luneta keeps of what a pane printed, for an agent on it.
fn reading(ended: &Ended) -> String {
    format!("Reading what `{}` printed…", ended.command.as_str())
}

/// What luneta asks gh about the checks of the pull request `number`.
fn fetching(number: NonZeroU32) -> String {
    format!("Asking gh about the checks of #{number}…")
}

/// What luneta waits on, `doing`, wrapped as `wrap` wraps it, after the
/// spinner at `frame`, the one a busy agent has: the lines after the first
/// start under its first word.
pub fn waiting(doing: &str, width: usize, style: Style, frame: usize) -> Vec<Line<'static>> {
    let (spinner, turning) = Mark::Busy.glyph(frame);
    let lines = wrap(doing, width.saturating_sub(2), style).into_iter().enumerate();
    let lines = lines.map(|(at, line)| {
        let before = if at == 0 { Span::styled(spinner.clone(), turning) } else { Span::raw(" ") };
        Line::from(vec![Span::raw(" "), before, Span::styled(line.to_string(), style)])
    });
    lines.collect()
}

/// `count` merged worktrees, in words.
fn merged_worktrees(count: usize) -> String {
    if count == 1 { "1 merged worktree".to_owned() } else { format!("{count} merged worktrees") }
}

fn asked_of(plan: &prunes::Plan) -> String {
    let worktrees = merged_worktrees(plan.goes.len());
    let far = |goes: &&prunes::Goes| matches!(goes.pushed, prunes::Pushed::Goes(_));
    match plan.goes.iter().filter(far).count() {
        0 => format!("Delete {worktrees}?"),
        1 => format!("Delete {worktrees} and 1 remote branch?"),
        count => format!("Delete {worktrees} and {count} remote branches?"),
    }
}

/// What `d m` lists of `plan`, `width` wide, in `room` lines at most: what it
/// asks, each worktree that goes, biggest first, as far as there is room, and
/// then why the others stay.
fn swept_lines(plan: &prunes::Plan, width: usize, room: usize) -> Vec<Line<'static>> {
    let style = Style::new().fg(NAME);
    let mut lines = if plan.goes.is_empty() {
        wrap("No merged worktree to delete", width, style)
    } else {
        wrap(&asked_of(plan), width, style)
    };
    lines.push(Line::default());
    let kept = plan.kept.iter().map(|kept| (&kept.worktree, kept.why.as_str()));
    let unasked = plan.unasked.iter().map(|unasked| (&unasked.worktree, "luneta could not ask"));
    let keeps: Vec<Vec<Line<'static>>> = kept
        .chain(unasked)
        .map(|(worktree, why)| {
            wrap(&format!("Keeps {}: {why}", prunes::name_of(worktree)), width, style)
        })
        .collect();
    let left = room.saturating_sub(lines.len());
    let keeping: usize = keeps.iter().map(Vec::len).sum();
    let goes_room = left.saturating_sub(keeping.min(left / 2));
    let goes = goes_lines(&plan.goes, width, goes_room);
    let keep_room = left.saturating_sub(goes.len());
    lines.extend(goes);
    let mut used = 0;
    for (at, keep) in keeps.iter().enumerate() {
        let rest = keeps.len() - at;
        let room_for_more = if rest > 1 { keep_room.saturating_sub(1) } else { keep_room };
        if used + keep.len() > room_for_more {
            if keep_room > used {
                lines.push(Line::styled(format!(" …and {rest} more stay"), style));
            }
            break;
        }
        lines.extend(keep.iter().cloned());
        used += keep.len();
    }
    lines
}

/// The worktrees of `goes`, a line each, in `room` lines at most: its name, its
/// pull request's number and what it takes up, in columns, and how many more
/// there are, on the last, where they do not all fit.
fn goes_lines(goes: &[prunes::Goes], width: usize, room: usize) -> Vec<Line<'static>> {
    let shown = if goes.len() <= room { goes.len() } else { room.saturating_sub(1) };
    let numbers: Vec<String> = goes.iter().map(|goes| format!("#{}", goes.number)).collect();
    let size = |goes: &prunes::Goes| goes.bytes.map_or("?".to_owned(), prune::size);
    let sizes: Vec<String> = goes.iter().map(size).collect();
    let widest = |all: &[String]| all.iter().take(shown).map(|it| it.width()).max().unwrap_or(0);
    let (numbers_wide, sizes_wide) = (widest(&numbers), widest(&sizes));
    let names: Vec<&str> = goes.iter().map(|goes| prunes::name_of(&goes.worktree)).collect();
    let names_room = width.saturating_sub(numbers_wide + sizes_wide + 6);
    let longest = names.iter().take(shown).map(|name| name.width()).max().unwrap_or(0);
    let names_wide = longest.min(names_room);
    let mut lines = Vec::with_capacity(room);
    for at in 0..shown {
        let name = sidebar::cut(names[at], names_wide);
        let gap = " ".repeat(names_wide.saturating_sub(name.width()));
        let (number, size) = (&numbers[at], &sizes[at]);
        let number_gap = " ".repeat(numbers_wide - number.width());
        let size_gap = " ".repeat(sizes_wide - size.width());
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(format!("{name}{gap}  "), Style::new().fg(NAME)),
            Span::styled(format!("{number}{number_gap}  "), Style::new().fg(gruvbox::PURPLE)),
            Span::styled(format!("{size_gap}{size}"), Style::new().fg(TAG)),
        ]));
    }
    if shown < goes.len() && room > 0 {
        let more = goes.len() - shown;
        lines.push(Line::styled(format!(" …and {more} more"), Style::new().fg(NAME)));
    }
    lines
}

/// What the rule over the menu names: the groups opened.
fn title(keys: &Keys) -> String {
    let names: Vec<&str> = keys.trail().into_iter().map(|(name, _)| name).collect();
    if names.is_empty() { TITLE.to_owned() } else { names.join(" › ") }
}

/// The legend at the bottom of the sidebar, `width` wide, in `room` lines at
/// most: under a rule, the keys the top of the sidebar takes, as `Top::of`
/// leaves them to the menu, and the sidebar's command, each with what it
/// does. Each is a spot that presses its key, by its line among them.
pub fn legend(width: usize, room: usize) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    listed(TITLE, &Keys::legend(), width, room)
}

/// What the bottom of the sidebar lists in the legend's place while a key
/// waits for the next, or `v` or `V` lasts: `keys`, each the keys and what
/// they do, under a rule that names them `title`, as `legend` lists its own.
/// Each that is one key is a spot that presses it.
pub fn shortcuts(
    title: &str,
    keys: &[(Label, &str)],
    width: usize,
    room: usize,
) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let inner = width.saturating_sub(1);
    let spelled: Vec<String> = keys.iter().map(|(label, _)| label.to_string()).collect();
    let column = spelled.iter().map(|spelled| spelled.width()).max().unwrap_or(0);
    let rows = keys.iter().zip(&spelled).map(|((label, does), spelled)| {
        (keys_line_of(spelled, column, does, inner), label.pressed())
    });
    fitted(title, rows.collect(), width, room)
}

/// What the bottom of the sidebar lists in the legend's place while `y` waits
/// for what to yank, as `legend` lists it.
pub fn yanks(width: usize, room: usize) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    listed(YANK_NAME, &YANKS, width, room)
}

/// `entries` under a rule that names them `title`, `width` wide, in `room`
/// lines at most, each a spot that presses its key, by its line among them.
fn listed(
    title: &str,
    entries: &[Entry],
    width: usize,
    room: usize,
) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let inner = width.saturating_sub(1);
    let line = |entry: &Entry| {
        (entry_line(entry, inner), Some(KeyEvent::from(KeyCode::Char(entry.key()))))
    };
    fitted(title, entries.iter().map(line).collect(), width, room)
}

/// `rows` under a rule that names them `title`, `width` wide, in `room` lines
/// at most, each a spot that presses its key, where it has one, by its line
/// among them. Short of room, the last line says that `ALL_KEYS` lists the
/// rest, and is a spot that presses it; and short of a line for the rule
/// too, it is all there is.
fn fitted(
    title: &str,
    rows: Vec<(Line<'static>, Option<KeyEvent>)>,
    width: usize,
    room: usize,
) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let mut lines = Vec::with_capacity(room);
    let mut spots = Spots::default();
    if room == 0 {
        return (lines, spots);
    }
    let all = rows.len() < room;
    if all || room >= 2 {
        lines.push(rule(title, "", width));
    }
    let shown = if all { rows.len() } else { room.saturating_sub(2) };
    for (line, pressed) in rows.into_iter().take(shown) {
        if let Some(pressed) = pressed {
            spots.add(lines.len(), 0..width, pressed);
        }
        lines.push(line);
    }
    if !all {
        spots.add(lines.len(), 0..width, KeyEvent::from(KeyCode::Char(ALL_KEYS)));
        lines.push(all_keys_line(width.saturating_sub(1)));
    }
    (lines, spots)
}

/// Every key luneta has, as `ALL_KEYS` lists them at the bottom of the
/// sidebar, in groups: the moves and folds at the top of the sidebar, its
/// commands, the menu's keys, and those luneta takes before zellij. And how
/// far down the list is scrolled.
#[derive(Debug, Default)]
pub struct AllKeys {
    /// The first line of the list on screen.
    first: usize,
    /// How many lines of the list it had on screen when drawn last, for
    /// `scroll` to stop at its end.
    room: Cell<usize>,
}

impl AllKeys {
    /// Each group, its name and its keys, each the keys and what they do.
    fn groups() -> [(&'static str, Vec<(String, String)>); 4] {
        let named = |keys: &[(Label, &str)]| -> Vec<(String, String)> {
            keys.iter().map(|(label, does)| (label.to_string(), (*does).to_owned())).collect()
        };
        let anywhere =
            host::ANYWHERE.map(|((code, modifiers), does)| (Label::Key(code, modifiers), does));
        let mut sidebar: Vec<(String, String)> = Command::ALL
            .into_iter()
            .map(|command| (command.key().to_string(), command.does().to_owned()))
            .collect();
        sidebar.push((format!("{YANK} {YANK_PULL}"), format!("{YANK_NAME} {YANK_PULL_DOES}")));
        let mut menu = Vec::new();
        for entry in Keys::every(None) {
            entry.spelled("", &mut menu);
        }
        menu.extend(named(&MENU_MOVES));
        [
            ("Move", named(&MOVES)),
            ("Sidebar", sidebar),
            ("Menu", menu),
            ("Anywhere", named(&anywhere)),
        ]
    }

    /// The lines of the whole list, `width` wide: each group's name, and its
    /// keys under it, what they do in a column of the group's own.
    fn rows(width: usize) -> Vec<Line<'static>> {
        let inner = width.saturating_sub(1);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut rows = Vec::new();
        for (name, keys) in AllKeys::groups() {
            rows.push(Line::from(vec![Span::raw(" "), Span::styled(name, bold)]));
            let column = keys.iter().map(|(keys, _)| keys.width()).max().unwrap_or(0);
            rows.extend(keys.iter().map(|(keys, does)| keys_line_of(keys, column, does, inner)));
        }
        rows
    }

    /// Scrolls the list `by` lines down, or up, as far as its last line at
    /// the bottom, as last drawn, and its first at the top.
    pub fn scroll(&mut self, by: isize) {
        let last = AllKeys::rows(0).len().saturating_sub(self.room.get());
        self.first = self.first.min(last).saturating_add_signed(by).min(last);
    }

    /// The list, `width` wide, in `room` lines at most: under a rule that
    /// names it, and says how far down it is where it has more than shows,
    /// as many of its lines as fit from the first scrolled to, and the keys
    /// it takes, each a spot that presses it.
    pub fn lines(&self, width: usize, room: usize) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
        let rows = AllKeys::rows(width);
        let body = room.saturating_sub(2);
        self.room.set(body);
        let first = self.first.min(rows.len().saturating_sub(body));
        let shown = rows.len().min(first + body);
        let right =
            if rows.len() > body { format!("{shown}/{}", rows.len()) } else { String::new() };
        let mut lines = Vec::with_capacity(room);
        if room >= 2 {
            lines.push(rule(ALL_KEYS_DOES, &right, width));
        }
        lines.extend(rows.into_iter().skip(first).take(body));
        if room == 0 {
            return (lines, Spots::default());
        }
        let (keys, spots) = keyed(width, &[(Hint::Moves, "Scroll"), (Hint::Esc, "Close")]);
        let at = lines.len();
        lines.push(keys);
        (lines, spots.down(at))
    }
}

/// A spot on the line of each of `entries`, `width` wide, one to a line, that
/// presses its key.
fn entry_spots(entries: &[Entry], width: usize) -> Spots<KeyEvent> {
    let mut spots = Spots::default();
    for (line, entry) in entries.iter().enumerate() {
        spots.add(line, 0..width, KeyEvent::from(KeyCode::Char(entry.key())));
    }
    spots
}

/// The rule over the menu, `width` wide: its title, and where its pane works
/// while the rule has room for all of both.
pub fn rule(title: &str, right: &str, width: usize) -> Line<'static> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    // `─ `, the title, a space and at least one `─` of the rule.
    let head = 2 + 1 + 1;
    // A space, the right, a space and a last `─`.
    let tail = right.width() + 3;
    let fits = !right.is_empty() && head + title.width() + tail <= width;
    let tail = if fits { tail } else { 0 };
    let title = sidebar::cut(title, width.saturating_sub(head + tail));
    let stretch = width.saturating_sub(head - 1 + title.width() + tail);
    let mut spans = vec![
        Span::styled("─ ", dim),
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        Span::styled("─".repeat(stretch), dim),
    ];
    if fits {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(right.to_owned(), Style::new().fg(TAG)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled("─", dim));
    }
    Line::from(spans)
}

/// An entry of the menu, `inner` wide past the space it starts with: its key,
/// and what it does.
fn entry_line(entry: &Entry, inner: usize) -> Line<'static> {
    let key = entry.key().to_string();
    let Entry::Group { name, .. } = entry else {
        return keys_line_of(&key, key.width(), &entry.does(), inner);
    };
    let room = inner.saturating_sub(key.width() + GAP);
    Line::from(vec![
        Span::raw(" "),
        Span::styled(key, Style::new().fg(ACCENT)),
        Span::raw(" ".repeat(GAP)),
        Span::raw(sidebar::cut(name, room.saturating_sub(1 + GROUP.width()))),
        Span::raw(" "),
        Span::styled(GROUP, Style::new().fg(TAG)),
    ])
}

/// A line of a list of keys, `inner` wide past the space it starts with: the
/// keys, in a column `column` wide, and what they do.
fn keys_line_of(keys: &str, column: usize, does: &str, inner: usize) -> Line<'static> {
    let room = inner.saturating_sub(column + GAP);
    let pad = column.saturating_sub(keys.width()) + GAP;
    Line::from(vec![
        Span::raw(" "),
        Span::styled(keys.to_owned(), Style::new().fg(ACCENT)),
        Span::raw(" ".repeat(pad)),
        Span::styled(sidebar::cut(does, room), Style::new().fg(NAME)),
    ])
}

/// The line a list of keys short of room ends with, `inner` wide past the
/// space it starts with: `ALL_KEYS`, which lists those it left out.
fn all_keys_line(inner: usize) -> Line<'static> {
    keys_line_of(&ALL_KEYS.to_string(), 1, ALL_KEYS_DOES, inner)
}

/// The line under the keys that start agents, `inner` wide past the space it
/// starts with: the icon and the name of the harness of `agent`, and that the
/// agent in the pane picked it, where it did; and after them `<TAB>`, which
/// picks the next, and what it does. Where the line has no room for all of
/// it, what `Tab` does goes in fewer words, then in none, then the pane's
/// part goes, and then `<TAB>`, for the name alone, cut to fit.
fn agent_line(agent: Picked, inner: usize) -> Line<'static> {
    let name = agents::name(agent.harness());
    let pane = match agent {
        Picked::ByPane(_) => Some(PANE_S),
        Picked::ByYou(_) => None,
    };
    let key = Hint::Tab.label();
    let wide = |(pane, does): &(Option<&str>, Option<&str>)| {
        let pane = pane.map_or(0, UnicodeWidthStr::width);
        let does = does.map_or(0, |does| 1 + does.width());
        icons::WIDTH + 1 + name.width() + pane + GAP + key.width() + does
    };
    let does = NEXT_DOES.map(Some);
    let tries = [(pane, does[0]), (pane, does[1]), (pane, None), (None, None)];
    let icon = icons::harness(agent.harness());
    let mut spans = vec![Span::raw(" "), Span::raw(icon), Span::raw(" ")];
    match tries.into_iter().find(|tried| wide(tried) <= inner) {
        Some((pane, does)) => {
            spans.push(Span::raw(name));
            spans.extend(pane.map(|pane| Span::styled(pane, Style::new().fg(NAME))));
            spans.push(Span::raw(" ".repeat(GAP)));
            spans.push(Span::styled(key, Style::new().fg(ACCENT)));
            if let Some(does) = does {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(does, Style::new().fg(NAME)));
            }
        },
        None => spans.push(Span::raw(sidebar::cut(name, inner.saturating_sub(icons::WIDTH + 1)))),
    }
    Line::from(spans)
}

/// A key a line of keys names, as `<ESC>`, `<y>` or `<Ctrl r>`, or what
/// takes more than one, as `<↓↑>` or `<any key>`, which no click presses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Esc,
    Backspace,
    Enter,
    Tab,
    Delete,
    Char(char),
    Ctrl(char),
    Alt(char),
    /// The arrows down and up.
    Moves,
    AnyKey,
    AnyOtherKey,
}

impl Hint {
    /// How a line of keys names it.
    fn label(self) -> String {
        match self {
            Hint::Esc => "<ESC>".to_owned(),
            Hint::Backspace => "<BS>".to_owned(),
            Hint::Enter => "<ENTER>".to_owned(),
            Hint::Tab => "<TAB>".to_owned(),
            Hint::Delete => "<Del>".to_owned(),
            Hint::Char(c) => format!("<{c}>"),
            Hint::Ctrl(c) => format!("<Ctrl {c}>"),
            Hint::Alt(c) => format!("<Alt {c}>"),
            Hint::Moves => "<↓↑>".to_owned(),
            Hint::AnyKey => "<any key>".to_owned(),
            Hint::AnyOtherKey => "<any other key>".to_owned(),
        }
    }

    /// The key a click on it presses: none for what takes more than one.
    fn key(self) -> Option<KeyEvent> {
        let (code, modifiers) = match self {
            Hint::Esc => (KeyCode::Esc, KeyModifiers::NONE),
            Hint::Backspace => (KeyCode::Backspace, KeyModifiers::NONE),
            Hint::Enter => (KeyCode::Enter, KeyModifiers::NONE),
            Hint::Tab => (KeyCode::Tab, KeyModifiers::NONE),
            Hint::Delete => (KeyCode::Delete, KeyModifiers::NONE),
            Hint::Char(c) => (KeyCode::Char(c), KeyModifiers::NONE),
            Hint::Ctrl(c) => (KeyCode::Char(c), KeyModifiers::CONTROL),
            Hint::Alt(c) => (KeyCode::Char(c), KeyModifiers::ALT),
            Hint::Moves | Hint::AnyKey | Hint::AnyOtherKey => return None,
        };
        Some(KeyEvent::new(code, modifiers))
    }
}

/// The keys that go back and close, and what each does, in as many words as
/// `width` has room for: all of them, or else the keys alone.
pub fn keys_line(width: usize, keys: &[(Hint, &str)]) -> Line<'static> {
    keyed(width, keys).0
}

/// The line of `keys` that `keys_line` draws, and on it a spot for each key
/// a click can press: the key, and the words after it that say what it does.
pub fn keyed(width: usize, keys: &[(Hint, &str)]) -> (Line<'static>, Spots<KeyEvent>) {
    let labels: Vec<String> = keys.iter().map(|(hint, _)| hint.label()).collect();
    let spelled = |sep: &str| -> Vec<String> {
        let each = labels.iter().zip(keys);
        each.map(|(label, (_, does))| format!("{label}{sep}{does}")).collect()
    };
    // Each key, with what it does or alone, and what goes between two; or
    // else the keys alone, run together.
    let candidates: [(Vec<String>, &str); 4] =
        [(spelled(" - "), ", "), (spelled(" "), "  "), (spelled(" "), " "), (labels.clone(), "/")];
    let room = width.saturating_sub(1);
    let fits = |(each, joiner): &(Vec<String>, &str)| each.join(joiner).width() <= room;
    let (each, joiner) =
        candidates.into_iter().find(fits).unwrap_or_else(|| (labels.clone(), ""));
    let dim = Style::new().add_modifier(Modifier::DIM);
    // Each key in the accent, and the words between them dimmed.
    let mut spans = vec![Span::raw(" ")];
    let mut spots = Spots::default();
    let mut column = 1;
    for (at, ((shown, label), (hint, _))) in each.iter().zip(&labels).zip(keys).enumerate() {
        if at > 0 {
            spans.push(Span::styled(joiner.to_owned(), dim));
            column += joiner.width();
        }
        spans.push(Span::styled(label.clone(), Style::new().fg(ACCENT)));
        let words = &shown[label.len()..];
        if !words.is_empty() {
            spans.push(Span::styled(words.to_owned(), dim));
        }
        if let Some(pressed) = hint.key() {
            spots.add(0, column..column + shown.width(), pressed);
        }
        column += shown.width();
    }
    (Line::from(spans), spots)
}

/// The search box over the skills, in at most `room` lines `width` wide: what
/// is typed, and under it the skills that match, best first, as many as
/// `SEARCH_ROWS` and the room show around the one the cursor is on, or a line
/// that says why there are none. It keeps the lines the skills found fill,
/// however many of them match, so the menu keeps its size as you type. Each
/// skill shown is a spot that picks it, by its line among them.
fn search_lines(
    search: &Search,
    width: usize,
    room: usize,
) -> (Vec<Line<'static>>, Spots<Choice>) {
    let mut spots = Spots::default();
    let Some(list) = room.checked_sub(1) else {
        return (Vec::new(), spots);
    };
    let total = match search.skills() {
        Skills::Found(skills) => skills.len(),
        Skills::Finding | Skills::Failed(_) => 0,
    };
    let list = total.clamp(1, SEARCH_ROWS).min(list);
    let mut lines = Vec::with_capacity(1 + list);
    lines.push(sidebar::query(search.term(), width));
    let dim = Style::new().add_modifier(Modifier::DIM);
    let note = match search.skills() {
        Skills::Finding => Some(("finding skills…".to_owned(), dim)),
        Skills::Failed(why) => Some((why.clone(), Style::new().fg(gruvbox::RED))),
        Skills::Found(skills) if skills.is_empty() => Some(("no skills here".to_owned(), dim)),
        Skills::Found(_) if search.count() == 0 => Some(("no match".to_owned(), dim)),
        Skills::Found(_) => None,
    };
    if let Some((note, style)) = note {
        let inner = width.saturating_sub(1);
        let note = Line::styled(format!(" {}", sidebar::cut(&note, inner)), style);
        lines.extend((list > 0).then_some(note));
    } else {
        let selected = search.selected().unwrap_or(0);
        let start = (selected + 1).saturating_sub(list);
        let shown = search.found().enumerate().skip(start).take(list);
        for (at, (skill, hits)) in shown {
            spots.add(lines.len(), 0..width, Choice::Skill(skill.name.clone()));
            let name = skill.name.to_string();
            lines.push(skill_line(&name, &skill.description, hits, at == selected, width));
        }
    }
    lines.resize(1 + list, Line::default());
    (lines, spots)
}

/// A skill in the search box, `width` wide: its name as a prompt runs it,
/// with the characters the term lit in the accent, and what it does, dimmed,
/// cut to the room its name leaves. The one the cursor is on is lit.
fn skill_line(
    name: &str,
    description: &str,
    hits: &[usize],
    selected: bool,
    width: usize,
) -> Line<'static> {
    let inner = width.saturating_sub(1);
    let name = sidebar::cut(&format!("/{name}"), inner);
    let lit = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::raw(" ")];
    // The `/` before the name is none of the characters the term lit.
    for (at, c) in name.chars().enumerate() {
        let hit = at > 0 && hits.contains(&(at - 1));
        spans.push(Span::styled(c.to_string(), if hit { lit } else { Style::new().fg(NAME) }));
    }
    let room = inner.saturating_sub(name.width() + GAP);
    if room > 0 && !description.is_empty() {
        spans.push(Span::raw(" ".repeat(GAP)));
        let dim = Style::new().add_modifier(Modifier::DIM);
        spans.push(Span::styled(sidebar::cut(description, room), dim));
    }
    let line = Line::from(spans);
    if selected { line.patch_style(Style::new().bg(LIT)) } else { line }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use lunetad::mux::TabId;
    use lunetad::Directory;
    use ratatui::layout::Position;

    use super::*;
    use crate::close::{self, Deletes, Doing, Seat, Subject};
    use crate::printed::Kept;
    use crate::pull::{At, Found, Link, Readied, Ready, Work};
    use crate::sidebar::Listed;
    use crate::skills::Skill;
    use crate::spots::Hit;
    use crate::zellij::Closes;

    const CLAUDE: Harness = Harness::ClaudeCode;
    const PICKED: Picked = Picked::ByYou(CLAUDE);

    fn listed() -> Listed {
        Listed::new("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    fn said(text: &str) -> Said {
        Said::new(text.to_owned()).unwrap()
    }

    fn row() -> PaneRow {
        PaneRow {
            session: listed(),
            pane: 4,
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/menu".to_owned()),
            repository: Some("/w/luneta".to_owned()),
            pull_request: None,
            ran: Ran::Unhooked,
        }
    }

    fn pane_row() -> Target {
        Target::Pane(Machine::Local, row())
    }

    fn session_row() -> Target {
        Target::Session(Machine::Local, listed())
    }

    fn char(key: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(key))
    }

    /// What a key on `row()` wants opened: `opens`, from its pane, in the
    /// directory it works in.
    fn beside_row(opens: Opens) -> Wants {
        Wants::Open { session: listed(), pane: 4, cwd: main_dir(), opens }
    }

    fn opened(target: Target, key: Option<char>) -> Did {
        Menu::open(1, target, key, PICKED).expect("a key of the menu")
    }

    /// The menu a key left open, and what it wants asked for.
    fn stays(did: Did) -> (Menu, Wants) {
        match did {
            Did::Stays(menu, wants) => (menu, wants),
            Did::Closed(done) => panic!("the menu closed, leaving {done:?}"),
        }
    }

    /// What a key that closed the menu left to do.
    fn closed(did: Did) -> Done {
        match did {
            Did::Closed(done) => done,
            Did::Stays(_, wants) => panic!("the menu stays open, wanting {wants:?}"),
        }
    }

    /// The text of each line.
    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

    /// The list of every key names each key the top of the sidebar moves,
    /// folds or opens with, as `Top::moves` takes it, and none it does not:
    /// but `y`, which the Sidebar group names with what it yanks.
    #[test]
    fn the_list_of_every_key_names_the_moves_of_the_top() {
        let named: Vec<char> = MOVES
            .iter()
            .flat_map(|(label, _)| match *label {
                Label::Key(KeyCode::Char(c), _) => vec![c],
                Label::Two(first, second) => vec![first, second],
                Label::Key(..) => Vec::new(),
            })
            .collect();
        for c in &named {
            assert!(Top::moves(*c).is_some(), "{c:?}");
        }
        for c in (' '..='~').filter(|c| *c != YANK && Top::moves(*c).is_some()) {
            assert!(named.contains(&c), "{c:?}");
        }
    }

    #[test]
    fn a_key_opens_its_group_and_a_key_in_it_opens_a_pane_beside_the_row() {
        let (menu, wants) = stays(opened(pane_row(), Some('n')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ New ─────────── luneta/main ─",
                " p  new pane",
                " t  new tab",
                " s  new session",
                " o  new own session",
                " m  new machine",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('p')));
        let cwd = main_dir();
        let opens = Opens::Pane { beside: 4, command: Vec::new() };
        assert_eq!(wants, Wants::Open { session: listed(), pane: 4, cwd: cwd.clone(), opens });
        assert!(menu.opening().is_some());
        // Keys do nothing while zellij opens it, but close the menu.
        let (menu, wants) = stays(menu.key(&char('p')));
        assert_eq!(wants, Wants::Nothing);
        assert!(menu.opening().is_some());
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);

        // luneta goes to a new session itself.
        let (menu, _) = stays(opened(pane_row(), Some('n')));
        let start = Done::Start(Machine::Local, Starts::In(main_dir()));
        assert_eq!(closed(menu.key(&char('s'))), start);
    }

    fn esc() -> KeyEvent {
        KeyEvent::from(KeyCode::Esc)
    }

    fn other_tab(pane: u32, name: &str) -> OtherTab {
        OtherTab { pane, name: name.to_owned() }
    }

    /// The menu `t p` leaves waiting for the other tabs of the session of
    /// `row()`.
    fn listing() -> Menu {
        let (menu, _) = stays(opened(pane_row(), Some(TURN)));
        let (menu, wants) = stays(menu.key(&char('p')));
        assert_eq!(wants, Wants::Tabs { session: listed(), pane: 4 });
        assert!(menu.listing());
        menu
    }

    /// `t t` turns the pane of the row into a tab, and the menu waits for
    /// zellij to, where its keys do nothing but close it. luneta knows
    /// nothing yet of where a pane works that it turns all the same.
    #[test]
    fn a_key_turns_the_pane_of_the_row_into_a_tab() {
        let unsaid = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        let (menu, wants) = stays(opened(unsaid, Some(TURN)));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Turn ────────────────────────",
                " t  pane into tab",
                " p  tab into pane",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Turn { session: listed(), pane: 4, turn: Turn::PaneIntoTab });
        assert_eq!(menu.opening(), Some(Opening::Turned(listed())));
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(closed(menu.key(&esc())), Done::Nothing);
    }

    /// `t p` needs no list where the session has only one other tab, and has
    /// nothing to turn into where it has none. Keys do nothing but close the
    /// menu until zellij has said.
    #[test]
    fn a_key_turns_the_tab_of_the_row_into_panes_of_its_only_other_tab() {
        let (menu, wants) = stays(listing().key(&char('1')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.tabs(Ok(vec![other_tab(7, "Tab #2")])));
        let turn = Turn::TabIntoPanes { beside: 7 };
        assert_eq!(wants, Wants::Turn { session: listed(), pane: 4, turn });
        assert_eq!(menu.opening(), Some(Opening::Turned(listed())));

        let Done::Wrong(none) = closed(listing().tabs(Ok(Vec::new()))) else {
            panic!("no reason");
        };
        assert!(none.contains("luneta has no other tab"), "{none}");
        let wrong = Done::Wrong("menu: not turned: pane 4 is gone".to_owned());
        assert_eq!(closed(listing().tabs(Err("pane 4 is gone".to_owned()))), wrong);
        assert_eq!(closed(listing().key(&esc())), Done::Nothing);
    }

    /// With more than one other tab, the menu lists them on a key each, by
    /// their names, in the order it was given them, and a key or a click on
    /// one turns the tab of the row into panes of that. `Backspace` goes back
    /// to the group, and any other key does nothing.
    #[test]
    fn a_key_picks_the_tab_a_tab_of_the_row_turns_into() {
        let tabs = vec![other_tab(7, "auth"), other_tab(2, "Tab #3")];
        let (menu, wants) = stays(listing().tabs(Ok(tabs)));
        assert_eq!(wants, Wants::Nothing);
        let (lines, spots) = menu.lines(31, 9, 0);
        assert_eq!(
            text(&lines),
            [
                "─ Into tab ────── luneta/main ─",
                " 1  auth",
                " 2  Tab #3",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        assert_eq!(spots.at(Position::new(3, 2)), Some(&Choice::Key(char('2'))));

        let (menu, wants) = stays(menu.key(&char('3')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT)));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('2')));
        let turn = Turn::TabIntoPanes { beside: 2 };
        assert_eq!(wants, Wants::Turn { session: listed(), pane: 4, turn });
        assert_eq!(menu.opening(), Some(Opening::Turned(listed())));

        let (menu, _) = stays(listing().tabs(Ok(vec![other_tab(7, "a"), other_tab(2, "b")])));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(menu.lines(31, 9, 0).0[1].to_string(), " t  pane into tab");
        let (menu, _) = stays(listing().tabs(Ok(vec![other_tab(7, "a"), other_tab(2, "b")])));
        assert_eq!(closed(menu.key(&esc())), Done::Nothing);
    }

    #[test]
    fn the_first_group_has_luneta_s_keys_and_backspace_goes_back_to_it() {
        let (menu, _) = stays(opened(pane_row(), None));
        assert_eq!(
            text(&menu.lines(31, 10, 0).0),
            [
                "─ luneta ──────── luneta/main ─",
                " /  Search skills",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " t  Turn ▸",
                " d  Delete ▸",
                " b  Brief ▸",
                " <ESC> - Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[0], "─ luneta ──────── luneta/main ─");
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Backspace))), Done::Nothing);
        // A key the menu has not opens none.
        assert!(Menu::open(1, pane_row(), Some('x'), PICKED).is_none());
    }

    /// An own session opens no pane beside another, and turns none: its
    /// pane's menu has no `n p` and no Turn group, and an agent a key starts
    /// opens in a tab of the session, in the pane's directory.
    #[test]
    fn an_own_session_s_pane_opens_its_agents_in_a_tab_and_turns_none() {
        let own = Listed::new_own("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1), 3);
        let own_row = || Target::Pane(Machine::Local, PaneRow { session: own.clone(), ..row() });
        let (menu, _) = stays(opened(own_row(), None));
        let lines = text(&menu.lines(31, 10, 0).0);
        assert!(lines.iter().all(|line| !line.contains("Turn")), "{lines:?}");
        let (menu, _) = stays(opened(own_row(), Some('n')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..5], [
            " t  new tab",
            " s  new session",
            " o  new own session",
            " m  new machine",
        ]);
        let (_, wants) = stays(stays(opened(own_row(), Some('c'))).0.key(&char('t')));
        let command = ["claude", "/thermo-nuclear-review"].map(str::to_owned).to_vec();
        let opens = Opens::Tab { name: None, command };
        assert_eq!(wants, Wants::Open { session: own.clone(), pane: 4, cwd: main_dir(), opens });
        assert!(Menu::open(1, own_row(), Some('t'), PICKED).is_none());
    }

    #[test]
    fn the_legend_lists_the_sidebar_s_commands_then_the_menu_s_groups() {
        assert_eq!(
            text(&legend(31, 14).0),
            [
                "─ luneta ──────────────────────",
                " /  Search",
                " u  Unread",
                " v  Reorder",
                " V  Move pane",
                " o  Open PR",
                " y  Yank ▸",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " t  Turn ▸",
                " d  Delete ▸",
                " b  Brief ▸",
            ]
        );
        // `m` and the keys that move are the sidebar's, and `/` and `u` its
        // commands.
        assert_eq!(Top::of('m'), Some(Top::Menu));
        assert_eq!(Top::of('/'), Some(Top::Command(Command::Search)));
        assert_eq!(Top::of('u'), Some(Top::Command(Command::Unread)));
        assert_eq!(Top::of('o'), Some(Top::Command(Command::Open)));
        assert_eq!(Top::of('V'), Some(Top::Command(Command::MovePane)));
        assert_eq!(Top::of('y'), Some(Top::Yank));
        assert_eq!(Top::of('a'), None);
        assert_eq!(Top::of('c'), None);
        assert_eq!(Top::of('n'), None);
        assert_eq!(Top::of('t'), None);
        assert_eq!(Top::of('d'), None);
        assert_eq!(Top::of('b'), None);
    }

    /// `c` opens the Skills group, and a key in it starts an agent on its
    /// skill beside the row's pane, in its directory, with no name, as the
    /// search's `Enter` does: in the harness the line
    /// under the keys names, by its icon and its name, which `Tab` switches,
    /// and wants kept for the next menu. The rule names the group alone.
    #[test]
    fn a_key_of_the_skills_group_starts_an_agent_on_its_skill_in_the_harness_named() {
        let (menu, wants) = stays(opened(pane_row(), Some('c')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 10, 0).0),
            [
                "─ Skills ──────── luneta/main ─",
                " t  /thermo-nuclear-review",
                " m  /make-illegal-states-unrep…",
                " s  /tigerstyle",
                " f  fix the last command",
                " e  explain the last command",
                " c  fix the failing checks",
                " \u{ec82} claude  <TAB> another agent",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('t')));
        let command = ["claude", "/thermo-nuclear-review"].map(str::to_owned);
        assert_eq!(wants, beside_row(Opens::Pane { beside: 4, command: command.to_vec() }));
        assert!(menu.opening().is_some());

        let (menu, _) = stays(opened(pane_row(), Some('c')));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Keep(Harness::Pi));
        let lines = text(&menu.lines(31, 10, 0).0);
        assert_eq!([lines[0].as_str(), lines[7].as_str()], [
            "─ Skills ──────── luneta/main ─",
            " \u{f03ff} pi  <TAB> another agent",
        ]);
        let (menu, wants) = stays(menu.key(&char('t')));
        let command = ["pi", "/skill:thermo-nuclear-review"].map(str::to_owned);
        assert_eq!(wants, beside_row(Opens::Pane { beside: 4, command: command.to_vec() }));
        assert!(menu.opening().is_some());
    }

    /// On a pane an agent runs in, the line under the keys says that agent
    /// picked the harness, until `Tab` picks the next. Where the line has
    /// less room, what `Tab` does goes in fewer words, then in none, then the
    /// pane's part goes, and then `<TAB>`.
    #[test]
    fn the_agent_line_says_where_the_pane_s_agent_picked_the_harness() {
        let pi = Picked::ByPane(Harness::Pi);
        let (menu, _) = stays(Menu::open(1, pane_row(), Some('c'), pi).unwrap());
        let line = |menu: &Menu, width| text(&menu.lines(width, 10, 0).0)[7].clone();
        assert_eq!(line(&menu, 38), " \u{f03ff} pi, the pane's  <TAB> another agent");
        assert_eq!(line(&menu, 37), " \u{f03ff} pi, the pane's  <TAB> another");
        assert_eq!(line(&menu, 31), " \u{f03ff} pi, the pane's  <TAB>");
        assert_eq!(line(&menu, 23), " \u{f03ff} pi  <TAB>");
        assert_eq!(line(&menu, 11), " \u{f03ff} pi");
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Keep(CLAUDE));
        assert_eq!(line(&menu, 31), " \u{ec82} claude  <TAB> another agent");
    }

    /// `Tab` switches the harness only where the menu starts agents: its
    /// first group starts none.
    #[test]
    fn tab_does_nothing_where_the_menu_starts_no_agent() {
        let (menu, _) = stays(opened(pane_row(), None));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Nothing);
        let (_, wants) = stays(menu.key(&char('c')));
        assert_eq!(wants, Wants::Nothing);
    }

    /// The command a row's shell ran last: `cargo test`, which failed.
    fn cargo_test() -> Ended {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        Ended { command: said("cargo test"), ending: Some(Ending::Exited(101)), at }
    }

    /// A row whose shell said the command it ran last ended.
    fn ended_row() -> Target {
        Target::Pane(Machine::Local, PaneRow { ran: Ran::Ended(cargo_test()), ..row() })
    }

    /// What luneta kept once the row's shell went on, as the daemon says,
    /// to another command, or to another run of the same, or went, is not
    /// what the key was pressed on, and opens no agent. A command that
    /// ended while the menu was open is one a key starts an agent on.
    #[test]
    fn a_key_for_the_last_command_holds_to_the_run_it_was_pressed_on() {
        let went = |ran: Ran| {
            let (menu, _) = stays(opened(ended_row(), Some('c')));
            let (mut menu, _) = stays(menu.key(&char('f')));
            menu.reran(ran);
            closed(menu.printed(Ok(Printed::Nothing)))
        };
        let why =
            "menu: pane 4's shell went on as luneta read what `cargo test` printed; ask again";
        let again = Ended { at: cargo_test().at + Duration::from_secs(5), ..cargo_test() };
        for ran in [Ran::Going(said("ls")), Ran::Ended(again), Ran::Ready, Ran::Unhooked] {
            assert_eq!(went(ran), Done::Wrong(why.to_owned()));
        }

        let busy =
            Target::Pane(Machine::Local, PaneRow { ran: Ran::Going(said("cargo test")), ..row() });
        let (mut menu, _) = stays(opened(busy, Some('c')));
        menu.reran(Ran::Ended(cargo_test()));
        let (_, wants) = stays(menu.key(&char('f')));
        assert_eq!(wants, Wants::Printed { session: listed(), pane: 4, ended: cargo_test() });
    }

    /// `c f` and `c e` have luneta keep what the command the row's shell ran
    /// last printed, and once it is kept, start an agent on it beside the
    /// pane, in its directory, let read the file it is kept in: to fix what
    /// went wrong, or to explain it. Keys do nothing meanwhile but close the
    /// menu.
    #[test]
    fn a_key_for_the_last_command_starts_an_agent_on_what_it_printed() {
        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, wants) = stays(menu.key(&char('e')));
        assert_eq!(wants, Wants::Printed { session: listed(), pane: 4, ended: cargo_test() });
        assert!(menu.reading());
        let (menu, wants) = stays(menu.key(&char('e')));
        assert_eq!(wants, Wants::Nothing);
        let file = "/run/luneta/printed/1.txt".to_owned();
        let kept = Kept { file, lines: 3, left_out: 0 };
        let printed = Printed::After(kept);
        let (menu, wants) = stays(menu.printed(Ok(printed)));
        let prompt = "Explain this. In my shell, `cargo test` ended with exit status 101. What it \
                      printed is in /run/luneta/printed/1.txt.";
        let read = "Read(//run/luneta/printed/1.txt)";
        let command = ["claude", "--allowedTools", read, "--", prompt];
        let opens = Opens::Pane { beside: 4, command: command.map(str::to_owned).to_vec() };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());

        // pi reads it with no rule, as it asks nothing before a read.
        let pi = Picked::ByPane(Harness::Pi);
        let (menu, _) = stays(Menu::open(1, ended_row(), Some('c'), pi).unwrap());
        let (menu, _) = stays(menu.key(&char('e')));
        let kept = Kept { file: "/run/luneta/printed/1.txt".to_owned(), lines: 3, left_out: 0 };
        let (_, wants) = stays(menu.printed(Ok(Printed::After(kept))));
        let opens = Opens::Pane { beside: 4, command: vec!["pi".to_owned(), prompt.to_owned()] };
        assert_eq!(wants, beside_row(opens));

        // Nothing printed leaves the agent no file to read.
        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, _) = stays(menu.key(&char('f')));
        let (_, wants) = stays(menu.printed(Ok(Printed::Nothing)));
        let prompt = "Fix this. In my shell, `cargo test` ended with exit status 101. It printed \
                      nothing.";
        let command = vec!["claude".to_owned(), prompt.to_owned()];
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));

        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, _) = stays(menu.key(&char('f')));
        let done = closed(menu.printed(Err("zellij: no such pane".to_owned())));
        assert_eq!(done, Done::Wrong("menu: what pane 4 printed: zellij: no such pane".to_owned()));
    }

    /// The pull request of `row()`'s branch, open and on GitHub, which `c c`
    /// asks of.
    fn pull_row(state: State, url: Option<&str>) -> Target {
        let pull_request = Some(PullRequest {
            number: NonZeroU32::new(328).unwrap(),
            state,
            url: url.map(str::to_owned),
            checks: None,
            stale: false,
        });
        Target::Pane(Machine::Local, PaneRow { pull_request, ..row() })
    }

    const PULL: &str = "https://github.com/owner/luneta/pull/328";

    /// The checks of `PULL` that failed on its head, the first `count` of
    /// `ci / test`, whose log is kept, and `ext`, which has none; and how many
    /// have yet to end.
    fn failing(count: usize, pending: u32) -> Failing {
        let kept = Kept { file: "/run/luneta/checks/1.log".to_owned(), lines: 40, left_out: 0 };
        let failed = |name: &str, log| checks::Failed { name: name.to_owned(), link: None, log };
        let failed = [
            failed("ci / test", checks::Log::Kept(kept)),
            failed("ext", checks::Log::Unkept("no job".to_owned())),
        ];
        Failing { head: "abc".to_owned(), failed: failed[..count].to_vec(), pending, passed: 1 }
    }

    /// `c c` has luneta keep the log of each check that failed on the head of
    /// the row's pull request, saying so meanwhile, and once they are kept,
    /// starts an agent on them beside the pane, in its directory, let read
    /// each log. Keys do nothing meanwhile but close the menu.
    #[test]
    fn a_key_for_the_checks_starts_an_agent_on_those_that_failed() {
        let (menu, _) = stays(opened(pull_row(State::Draft, Some(PULL)), Some('c')));
        let (menu, wants) = stays(menu.key(&char('c')));
        assert_eq!(wants, Wants::Checks { page: PullPage::parse(PULL).unwrap() });
        assert!(menu.fetching());
        let body = text(&menu.lines(40, 4, 0).0);
        assert!(body.iter().any(|line| line.ends_with("Asking gh about the checks of #328…")));
        let (menu, wants) = stays(menu.key(&char('c')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.checked(Ok(failing(2, 0))));
        let prompt = checks::prompt(&PullPage::parse(PULL).unwrap(), &failing(2, 0));
        let read = "Read(//run/luneta/checks/1.log)";
        let command = ["claude", "--allowedTools", read, "--", &prompt];
        let opens = Opens::Pane { beside: 4, command: command.map(str::to_owned).to_vec() };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());

        let fetched = |checks: Result<Failing, String>| {
            let (menu, _) = stays(opened(pull_row(State::Open, Some(PULL)), Some('c')));
            let (menu, _) = stays(menu.key(&char('c')));
            closed(menu.checked(checks))
        };
        let wrong = |why: &str| Done::Wrong(why.to_owned());
        assert_eq!(
            fetched(Err("gh: not logged in".to_owned())),
            wrong("menu: the checks of #328: gh: not logged in")
        );
        assert_eq!(fetched(Ok(failing(0, 0))), wrong("menu: no check failed on the head of #328"));
        assert_eq!(
            fetched(Ok(failing(0, 1))),
            wrong("menu: no check failed on the head of #328, and one has yet to end")
        );
        assert_eq!(
            fetched(Ok(failing(0, 3))),
            wrong("menu: no check failed on the head of #328, and 3 have yet to end")
        );
    }

    /// Only an open pull request, or a draft, whose page its daemon says, as
    /// the page of its number, has checks to start an agent on: the menu says
    /// why it starts none on a row with another, or none.
    #[test]
    fn a_key_for_the_checks_needs_an_open_pull_request() {
        let refused = |target: Target| {
            let (menu, _) = stays(opened(target, Some('c')));
            closed(menu.key(&char('c')))
        };
        let wrong = |why: &str| Done::Wrong(why.to_owned());
        let none = "menu: gh has found no pull request of pane 4's branch";
        assert_eq!(refused(pane_row()), wrong(none));
        assert_eq!(refused(pull_row(State::Merged, Some(PULL))), wrong("menu: #328 is merged"));
        assert_eq!(refused(pull_row(State::Closed, Some(PULL))), wrong("menu: #328 is closed"));
        let issue = "https://github.com/owner/luneta/issues/328";
        assert_eq!(
            refused(pull_row(State::Open, Some(issue))),
            wrong(&format!("menu: #328: {issue} is no page of a pull request"))
        );
        let other = "https://github.com/owner/luneta/pull/329";
        assert_eq!(
            refused(pull_row(State::Open, Some(other))),
            wrong("menu: the page of #328 is of #329")
        );
        assert_eq!(
            refused(pull_row(State::Open, None)),
            wrong("menu: pane 4's lunetad does not say the page of #328, as an older one does not")
        );
        let unsaid = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        assert!(matches!(refused(unsaid), Done::Wrong(_)));
    }

    /// Only a shell at its prompt, after a command its hook said the end of,
    /// or its marks in an own pane, has a last command to start an agent on.
    /// The menu says why it starts none on another.
    #[test]
    fn a_key_for_the_last_command_needs_a_shell_at_its_prompt_after_one() {
        let refused = |target: Target| {
            let (menu, _) = stays(opened(target, Some('c')));
            match closed(menu.key(&char('f'))) {
                Done::Wrong(why) => why,
                done => panic!("it did {done:?}"),
            }
        };
        let ran = |ran| Target::Pane(Machine::Local, PaneRow { ran, ..row() });
        assert_eq!(
            refused(ran(Ran::Going(said("make")))),
            "menu: pane 4 runs `make` still, and what it prints is not over"
        );
        assert_eq!(
            refused(ran(Ran::Stopped(said("vim")))),
            "menu: `vim` is stopped in pane 4, and not over; bring it back with `fg`"
        );
        assert_eq!(refused(ran(Ran::Ready)), "menu: pane 4's shell has run nothing yet");
        assert_eq!(
            refused(ran(Ran::Unhooked)),
            "menu: pane 4 runs no shell with luneta's hook; a fish started since the hook was \
             installed has it"
        );
        let own = Listed::new_own("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1), 3);
        let unmarked = PaneRow { session: own, ran: Ran::Unhooked, ..row() };
        assert_eq!(
            refused(Target::Pane(Machine::Local, unmarked)),
            "menu: pane 4's shell marks none of the commands it runs; a shell that writes OSC \
             133, as fish 4 does, marks them"
        );
        assert_eq!(
            refused(ran(Ran::Unknown)),
            "menu: what pane 4's shell ran is in words this luneta cannot read, as a newer \
             lunetad's"
        );
    }

    /// `a` closes the menu for the sidebar to open the ask box, for the row
    /// the menu was for, a session's row too: at the top of the sidebar, and
    /// in the menu's first group.
    #[test]
    fn a_opens_the_ask_box_for_the_row_the_menu_is_for() {
        for row in [pane_row(), session_row()] {
            let did = Menu::open(1, row.clone(), Some('a'), PICKED).expect("the menu has `a`");
            assert_eq!(closed(did), Done::Ask(row.clone()));
            let (menu, _) = stays(opened(row.clone(), None));
            assert_eq!(closed(menu.key(&char('a'))), Done::Ask(row));
        }
    }

    /// `i` closes the menu for the sidebar to open the issue box, for the
    /// repository the pane of the row works in, by the pane's directory, on
    /// its machine: at the top of the sidebar, and in the menu's first group.
    /// A session's row, a machine's header and a pane in no repository, or
    /// in a directory the daemon has not said, have no `i`.
    #[test]
    fn i_opens_the_issue_box_for_the_repository_of_the_row() {
        let vps = Machine::Remote("vps".parse().unwrap());
        for on in [Machine::Local, vps] {
            let row = Target::Pane(on.clone(), row());
            let (home, name) = ("/w/luneta".to_owned(), "luneta".to_owned());
            let repository = Repository { on, dir: main_dir(), home, name };
            let did = Menu::open(1, row.clone(), Some('i'), PICKED).expect("the menu has `i`");
            assert_eq!(closed(did), Done::Issue(repository.clone()));
            let (menu, _) = stays(opened(row, None));
            assert_eq!(closed(menu.key(&char('i'))), Done::Issue(repository));
        }
        let unrepositoried = Target::Pane(Machine::Local, PaneRow { repository: None, ..row() });
        let nowhere = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        for row in [session_row(), Target::Machine(Machine::Local), unrepositoried, nowhere] {
            assert!(Menu::open(1, row.clone(), Some('i'), PICKED).is_none(), "{row:?}");
            let (menu, _) = stays(opened(row.clone(), None));
            let lines = text(&menu.lines(31, 12, 0).0);
            assert!(!lines.contains(&" i  File issue".to_owned()), "{lines:?}");
        }
    }

    /// `b` opens the Brief group, on any row, and a key in it closes the menu
    /// for the sidebar to brief you by the row the menu was for: `z` on
    /// every session, `s` on the row's session, and `h` on what the row is.
    #[test]
    fn the_brief_group_briefs_you_by_the_row_the_menu_is_for() {
        let (menu, _) = stays(opened(session_row(), Some('b')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Brief ───────────────────────",
                " z  Every session",
                " s  This session",
                " h  Highlighted",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let luneta = || listed().name().clone();
        for (row, key, briefed) in [
            (pane_row(), 'z', Briefed::Every),
            (pane_row(), 's', Briefed::Session(Machine::Local, luneta())),
            (pane_row(), 'h', Briefed::Agent(Machine::Local, luneta(), 4)),
            (session_row(), 'h', Briefed::Session(Machine::Local, luneta())),
        ] {
            let (menu, _) = stays(opened(row, Some('b')));
            assert_eq!(closed(menu.key(&char(key))), Done::Brief(briefed), "{key}");
        }
    }

    fn local_header() -> Target {
        Target::Machine(Machine::Local)
    }

    fn remote_header() -> Target {
        Target::Machine(Machine::Remote("me@vps".parse().unwrap()))
    }

    /// `n o` makes an own session where `n s` makes one: where the pane works,
    /// or in your home on this machine's header.
    #[test]
    fn n_o_makes_an_own_session_where_n_s_makes_one() {
        let own = |target| closed(stays(opened(target, Some(NEW))).0.key(&char('o')));
        assert_eq!(own(pane_row()), Done::Own(Starts::In(main_dir())));
        assert_eq!(own(local_header()), Done::Own(Starts::Home));
    }

    /// `n m` opens a box, on any row, for what ssh reaches a machine by: a
    /// character types, `Backspace` takes one back, and `Enter` adds the
    /// machine, once what is typed is a destination, and else says why it
    /// is not, until the next key. `Esc` goes back to the keys, and so does
    /// `Backspace` with nothing typed.
    #[test]
    fn n_m_opens_a_box_for_a_machine_s_destination() {
        for target in [pane_row(), session_row(), local_header(), remote_header()] {
            let (menu, wants) = stays(opened(target, Some('n')));
            let (menu, _) = stays(menu.key(&char('m')));
            assert_eq!(wants, Wants::Nothing);
            let menu = "me x".chars().fold(menu, |menu, c| stays(menu.key(&char(c))).0);
            let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
            assert_eq!(
                text(&menu.lines(31, 9, 0).0),
                [
                    "─ ssh ─────────────────────────",
                    " me x_",
                    " not a destination: \"me x\" is",
                    " more than one word",
                    " <ENTER> - Add, <ESC> - Back",
                ]
            );
            let menu = (0..2).fold(menu, |menu, _| {
                stays(menu.key(&KeyEvent::from(KeyCode::Backspace))).0
            });
            let (menu, _) = stays(menu.key(&char('@')));
            let (menu, _) = stays(menu.key(&char('v')));
            let lines = text(&menu.lines(31, 9, 0).0);
            assert_eq!(lines[1..3], [" me@v_", " what you type ssh to it: a"]);
            let added = closed(menu.key(&KeyEvent::from(KeyCode::Enter)));
            assert_eq!(added, Done::Ssh("me@v".parse().unwrap()));
        }
        let (menu, _) = stays(opened(pane_row(), Some('n')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(title(match &menu.stage {
            Stage::Choosing(keys) => keys,
            _ => panic!("back to the keys"),
        }), NEW_NAME);
    }

    /// On another machine's pane, a key opens beside the pane what it opens
    /// on this machine's, and a session in its directory, which the menu
    /// stays for, of that machine. The Delete group plans what it deletes as
    /// on this machine's, and has it deleted on that machine, and the Brief
    /// group briefs you on its session or its agent. The ask box says it is
    /// this machine's alone, on another's pane or session.
    #[test]
    fn another_machine_s_row_opens_and_deletes_there() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let (pane, session) = (Target::Pane(vps(), row()), Target::Session(vps(), listed()));
        let (menu, _) = stays(opened(pane.clone(), Some('n')));
        let (menu, wants) = stays(menu.key(&char('p')));
        assert_eq!(wants, beside_row(Opens::Pane { beside: 4, command: Vec::new() }));
        assert_eq!(menu.target(), &pane);
        let (menu, _) = stays(opened(pane.clone(), Some('n')));
        assert_eq!(closed(menu.key(&char('s'))), Done::Start(vps(), Starts::In(main_dir())));

        let (menu, _) = stays(opened(pane.clone(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Pane(4) });
        let done = closed(menu.planned(plan(Closes::Pane, Vec::new())));
        let name = || listed().name().clone();
        let act = Act::Pane { session: name(), whose: listed().whose(), pane: 4 };
        assert_eq!(done, Done::Act(vps(), act));
        for (row, highlighted) in [
            (pane, Briefed::Agent(vps(), name(), 4)),
            (session, Briefed::Session(vps(), name())),
        ] {
            let (menu, _) = stays(opened(row.clone(), Some('d')));
            let (_, wants) = stays(menu.key(&char('s')));
            assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Session });
            for (key, briefed) in [
                ('z', Briefed::Every),
                ('s', Briefed::Session(vps(), name())),
                ('h', highlighted),
            ] {
                let (menu, _) = stays(opened(row.clone(), Some('b')));
                assert_eq!(closed(menu.key(&char(key))), Done::Brief(briefed), "{key}");
            }
            assert_eq!(closed(opened(row.clone(), Some('a'))), Done::Ask(row));
        }
    }

    fn main_dir() -> Directory {
        Directory::parse("/w/luneta/main".to_owned()).unwrap()
    }

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn merged_plan(names: &[(&str, u32, u64)]) -> prunes::Plan {
        let goes = names.iter().map(|(name, number, megabytes)| prunes::Goes {
            worktree: format!("/w/luneta/{name}"),
            branch: (*name).to_owned(),
            head: "abc".to_owned(),
            number: *number,
            pushed: prunes::Pushed::Nowhere,
            bytes: Some(megabytes * 1024 * 1024),
        });
        prunes::Plan {
            repository: "/w/luneta".to_owned(),
            goes: goes.collect(),
            kept: Vec::new(),
            unasked: Vec::new(),
        }
    }

    /// `d m` is offered on a pane's row in a repository, by the directory it
    /// works in, and nowhere else: not on one git has yet to place, nor a row of
    /// no directory, nor a machine's header.
    #[test]
    fn the_merged_worktrees_key_is_on_a_row_in_a_repository_alone() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Delete ──────── luneta/main ─",
                " d  close the pane",
                " s  delete the session",
                " m  delete merged worktrees",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('m')));
        assert_eq!(wants, Wants::Prunes { dir: main_dir() });
        assert!(menu.sweeping() && menu.turning());

        let outside = Target::Pane(Machine::Local, PaneRow { repository: None, ..row() });
        let unplaced = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        for target in [outside, unplaced, session_row(), remote_header()] {
            let (menu, _) = stays(opened(target.clone(), Some('d')));
            let (menu, wants) = stays(menu.key(&char('m')));
            assert_eq!(wants, Wants::Nothing, "{target:?}");
            assert!(!menu.sweeping(), "{target:?}");
        }
    }

    /// What `d m` lists fills what room there is: what it asks, each worktree
    /// that goes, biggest first as it was planned, why the others stay, and the
    /// keys. Short of room, it says how many more go.
    #[test]
    fn the_list_says_what_goes_as_far_as_there_is_room_and_why_the_rest_stay() {
        let goes = [("issue-282", 297, 4608), ("dev-check", 293, 916), ("dev-ci", 303, 6)];
        let mut plan = merged_plan(&goes);
        plan.kept.push(prunes::Kept {
            worktree: "/w/luneta/issue-308".to_owned(),
            number: 310,
            why: "it has a pane".to_owned(),
        });
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, wants) = stays(menu.swept(Ok(plan)));
        assert_eq!(wants, Wants::Nothing);
        assert!(!menu.turning());
        assert_eq!(
            text(&menu.lines(33, 20, 0).0),
            [
                "─ Delete ─────────────── luneta ─",
                " Delete 3 merged worktrees?",
                "",
                " issue-282  #297  4.5 GB",
                " dev-check  #293  916 MB",
                " dev-ci     #303    6 MB",
                " Keeps issue-308: it has a pane",
                " <y> Yes  <BS> Back  <ESC> Close",
            ]
        );
        assert_eq!(
            text(&menu.lines(33, 7, 0).0),
            [
                "─ Delete ─────────────── luneta ─",
                " Delete 3 merged worktrees?",
                "",
                " issue-282  #297  4.5 GB",
                " …and 2 more",
                " Keeps issue-308: it has a pane",
                " <y> Yes  <BS> Back  <ESC> Close",
            ]
        );
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let mut plan = merged_plan(&[]);
        for name in ["k1", "k2", "k3"] {
            plan.kept.push(prunes::Kept {
                worktree: format!("/w/luneta/{name}"),
                number: 1,
                why: "it is locked".to_owned(),
            });
        }
        let (menu, _) = stays(menu.swept(Ok(plan)));
        assert_eq!(
            text(&menu.lines(33, 6, 0).0),
            [
                "─ Delete ─────────────── luneta ─",
                " No merged worktree to delete",
                "",
                " Keeps k1: it is locked",
                " …and 2 more stay",
                " <BS> - Back, <ESC> - Close",
            ]
        );
    }

    /// `y`, or the key again, tells the row's machine what you were asked about;
    /// once that changed, it lists it again. Nothing goes at the first answer,
    /// nor on any other key.
    #[test]
    fn the_list_goes_ahead_on_yes_with_what_it_listed_alone() {
        let (one, two) = (("one", 1, 5), ("two", 2, 5));
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        for key in ['y', 'm'] {
            let (asked, _) = stays(opened(pane_row(), Some('d')));
            let (asked, _) = stays(asked.key(&char('m')));
            let (asked, wants) = stays(asked.swept(Ok(merged_plan(&[one, two]))));
            assert_eq!(wants, Wants::Nothing);
            let (asked, wants) = stays(asked.key(&char(key)));
            let agreed = merged_plan(&[two, one]).deletes();
            assert_eq!(wants, Wants::Prune { dir: main_dir(), agreed });
            assert!(asked.pruning() && asked.turning());
            assert_eq!(
                text(&asked.lines(40, 9, 0).0)[1],
                format!(" {} Deleting 2 merged worktrees…", Mark::Busy.glyph(0).0)
            );
        }
        let (menu, _) = stays(menu.swept(Ok(merged_plan(&[one, two]))));
        let (menu, wants) = stays(menu.key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, _) = stays(menu.key(&char('y')));
        let (menu, wants) = stays(menu.changed(merged_plan(&[one])));
        assert_eq!(wants, Wants::Nothing, "it deleted what changed since you read it");
        assert!(!menu.pruning());
        assert_eq!(text(&menu.lines(40, 9, 0).0)[1], " Delete 1 merged worktree?");
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " d  close the pane");
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.swept(Ok(merged_plan(&[one]))));
        let (menu, _) = stays(menu.key(&char('n')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " d  close the pane");
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.swept(Ok(merged_plan(&[one]))));
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    #[test]
    fn the_list_asks_of_the_branches_that_go_on_a_remote_too() {
        let asked = |plan: prunes::Plan| {
            let (menu, _) = stays(opened(pane_row(), Some('d')));
            let (menu, _) = stays(menu.key(&char('m')));
            let (menu, _) = stays(menu.swept(Ok(plan)));
            text(&menu.lines(33, 20, 0).0)[1..3].to_vec()
        };
        let on_origin = |branch: &str| {
            prunes::Pushed::Goes(prunes::RemoteBranch {
                remote: "origin".to_owned(),
                branch: branch.to_owned(),
            })
        };
        let mut plan = merged_plan(&[("one", 1, 5), ("two", 2, 5), ("tre", 3, 5)]);
        plan.goes[0].pushed = on_origin("one");
        plan.goes[1].pushed = prunes::Pushed::Stays("someone else wrote it".to_owned());
        assert_eq!(asked(plan.clone()), [" Delete 3 merged worktrees and 1", " remote branch?"]);
        plan.goes[1].pushed = on_origin("two");
        assert_eq!(asked(plan), [" Delete 3 merged worktrees and 2", " remote branches?"]);
    }

    /// With nothing to delete there is no yes to say. What goes wrong says
    /// nothing went, and while it waits a key but `Esc` does nothing.
    #[test]
    fn nothing_to_delete_asks_nothing() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (waiting, wants) = stays(menu.key(&char('y')));
        assert_eq!(wants, Wants::Nothing);
        assert!(waiting.sweeping());
        let (listed, _) = stays(waiting.swept(Ok(merged_plan(&[]))));
        assert_eq!(
            text(&listed.lines(33, 9, 0).0),
            [
                "─ Delete ─────────────── luneta ─",
                " No merged worktree to delete",
                "",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (listed, wants) = stays(listed.key(&char('y')));
        assert_eq!(wants, Wants::Nothing);
        assert!(!listed.sweeping());

        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let wrong = closed(menu.swept(Err("lunetad does not answer".to_owned())));
        assert_eq!(wrong, Done::Wrong("menu: nothing deleted: lunetad does not answer".to_owned()));

        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    /// On another machine's header, the Delete group forgets the machine,
    /// and this one's has none, as luneta lists it always; the Brief group
    /// briefs you on every session of every machine, or on the machine's,
    /// from any header.
    #[test]
    fn a_machine_s_header_has_the_menu_forget_it() {
        let (menu, _) = stays(opened(remote_header(), Some('d')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Delete ──────────────────────",
                " d  forget the machine",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        assert_eq!(closed(menu.key(&char('d'))), Done::Forget("me@vps".parse().unwrap()));
        assert!(Menu::open(1, local_header(), Some(DELETE), PICKED).is_none());

        let vps = || Machine::Remote("me@vps".parse().unwrap());
        for (header, machine) in [(local_header(), Machine::Local), (remote_header(), vps())] {
            let (menu, _) = stays(opened(header.clone(), Some('b')));
            assert_eq!(closed(menu.key(&char('z'))), Done::Brief(Briefed::Every));
            for key in ['s', 'h'] {
                let (menu, _) = stays(opened(header.clone(), Some('b')));
                let briefed = Briefed::Machine(machine.clone());
                assert_eq!(closed(menu.key(&char(key))), Done::Brief(briefed), "{key}");
            }
        }
    }

    /// The menu for a session's row or a machine's header lists only the
    /// keys that work there, each group with those of its own, and none left
    /// with none. A key it does not list does nothing, at the top of the
    /// sidebar too, where it opens no menu.
    #[test]
    fn the_menu_lists_only_the_keys_that_work_on_its_row() {
        let root = |target: Target| {
            let (menu, _) = stays(opened(target, None));
            text(&menu.lines(31, 9, 0).0)
        };
        let session = [
            "─ luneta ──────────────────────",
            " a  Ask",
            " n  New ▸",
            " d  Delete ▸",
            " b  Brief ▸",
            " <ESC> - Close",
        ];
        assert_eq!(root(session_row()), session);
        assert_eq!(root(remote_header()), session);
        assert_eq!(root(local_header()), [
            "─ luneta ──────────────────────",
            " a  Ask",
            " n  New ▸",
            " b  Brief ▸",
            " <ESC> - Close",
        ]);
        let new = |target: Target| {
            let (menu, _) = stays(opened(target, Some(NEW)));
            text(&menu.lines(31, 9, 0).0)
        };
        assert_eq!(new(session_row()), [
            "─ New ─────────────────────────",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(new(remote_header()), [
            "─ New ─────────────────────────",
            " s  new session",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(new(local_header()), [
            "─ New ─────────────────────────",
            " s  new session",
            " o  new own session",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        let (menu, _) = stays(opened(session_row(), Some(DELETE)));
        let deletes = [" d  delete the session", " s  delete the session"];
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], deletes);

        let press = |target: &Target, group: Option<char>, key: char| {
            let (menu, _) = stays(opened(target.clone(), group));
            let (menu, wants) = stays(menu.key(&char(key)));
            (text(&menu.lines(31, 9, 0).0)[0].clone(), wants)
        };
        let first = || ("─ luneta ──────────────────────".to_owned(), Wants::Nothing);
        let new = || ("─ New ─────────────────────────".to_owned(), Wants::Nothing);
        for target in [session_row(), local_header(), remote_header()] {
            for group in [SEARCH, SKILLS, TURN] {
                let menu = Menu::open(1, target.clone(), Some(group), PICKED);
                assert!(menu.is_none(), "{group}");
                assert_eq!(press(&target, None, group), first(), "{group}");
            }
            for key in ['p', 't'] {
                assert_eq!(press(&target, Some(NEW), key), new(), "{key}");
            }
        }
        assert_eq!(press(&session_row(), Some(NEW), 's'), new());
    }

    #[test]
    fn a_menu_that_cannot_open_beside_its_row_says_why() {
        let unknown = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        let (menu, _) = stays(opened(unknown, None));
        let done = closed(menu.key(&char('/')));
        assert!(matches!(done, Done::Wrong(why) if why.contains("where pane 4 works")));
        let relative = Target::Pane(Machine::Local, PaneRow { cwd: Some("w".to_owned()), ..row() });
        let (menu, _) = stays(opened(relative, Some(NEW)));
        let why = "menu: where pane 4 works, \"w\" is no absolute path".to_owned();
        assert_eq!(closed(menu.key(&char('s'))), Done::Wrong(why));
    }

    fn skills(names: &[&str]) -> Vec<Skill> {
        let skill = |name: &&str| Skill {
            name: Name::new((*name).to_owned()).expect("a name"),
            description: format!("{name} it"),
            runs: crate::skills::Runs::ForAnyone,
        };
        names.iter().map(skill).collect()
    }

    #[test]
    fn slash_searches_the_skills_and_enter_starts_an_agent_on_the_one_picked() {
        let (menu, _) = stays(opened(pane_row(), None));
        let (mut menu, wants) = stays(menu.key(&char('/')));
        assert_eq!(wants, Wants::Skills { cwd: "/w/luneta/main".to_owned(), agent: CLAUDE });
        assert_eq!(
            text(&menu.lines(31, 6, 0).0)[..3],
            ["─ Skills ──────── luneta/main ─", " _", " finding skills…"]
        );
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        // The one the cursor is on is lit, as the sidebar's row is while it
        // has the focus, which it has while the search is open.
        let lit: Vec<_> = menu.lines(31, 6, 0).0[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [Some(LIT), None]);
        // Under the skills, the line that names the agent, as in the groups.
        assert_eq!(text(&menu.lines(31, 6, 0).0)[4], " \u{ec82} claude  <TAB> another agent");
        let (menu, _) = stays(menu.key(&char('t')));
        let (menu, _) = stays(menu.key(&char('i')));
        assert_eq!(text(&menu.lines(31, 6, 0).0)[..3], [
            "─ Skills ──────── luneta/main ─".to_owned(),
            " ti_".to_owned(),
            " /tigerstyle  tigerstyle it".to_owned(),
        ]);
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = agents::on_skill(CLAUDE, "tigerstyle", None);
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
    }

    /// `Tab` in the search box picks the other harness and has the finder
    /// asked for its skills, keeping what is typed. What the finder says
    /// for the harness the box left goes nowhere. `Enter` starts the one
    /// picked in the harness the line under the skills names.
    #[test]
    fn tab_in_the_search_finds_the_skills_of_the_other_harness() {
        let (mut menu, _) = stays(opened(pane_row(), Some('/')));
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        let (menu, _) = stays(menu.key(&char('t')));
        let (mut menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        let cwd = "/w/luneta/main".to_owned();
        assert_eq!(wants, Wants::Refind { cwd, agent: Harness::Pi });
        assert_eq!(text(&menu.lines(31, 6, 0).0)[..3], [
            "─ Skills ──────── luneta/main ─",
            " t_",
            " finding skills…",
        ]);
        menu.found(CLAUDE, Ok(skills(&["tigerstyle"])));
        assert_eq!(text(&menu.lines(31, 6, 0).0)[2], " finding skills…");
        menu.found(Harness::Pi, Ok(skills(&["tdd", "code-review"])));
        assert_eq!(text(&menu.lines(31, 6, 0).0)[2], " /tdd  tdd it");
        assert_eq!(text(&menu.lines(31, 6, 0).0)[4], " \u{f03ff} pi  <TAB> another agent");
        let (_, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = agents::on_skill(Harness::Pi, "tdd", None);
        assert_eq!(wants, beside_row(Opens::Pane { beside: 4, command }));
    }

    /// Each skill the search box shows is a spot on its line that picks it:
    /// a click starts an agent on it, as `Enter` does with the cursor on it.
    /// The wheel over them moves the cursor. What is typed is no spot.
    #[test]
    fn a_click_on_a_skill_starts_an_agent_on_it_as_enter_does() {
        let (mut menu, _) = stays(opened(pane_row(), Some('/')));
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        let skill = |name: &str| Name::new(name.to_owned()).expect("a name");
        // The rule, what is typed, then the skills.
        let (_, spots) = menu.lines(31, 6, 0);
        assert_eq!(spots.at(Position::new(4, 1)), None);
        assert_eq!(spots.at(Position::new(4, 2)), Some(&Choice::Skill(skill("code-review"))));
        assert_eq!(spots.at(Position::new(30, 3)), Some(&Choice::Skill(skill("tigerstyle"))));
        menu.scroll(1);
        let lit: Vec<_> = menu.lines(31, 6, 0).0[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [None, Some(LIT)]);
        // A skill that no longer matches picks nothing, not the one in its
        // place.
        let (menu, _) = stays(menu.key(&char('g')));
        let (menu, wants) = stays(menu.pick(&skill("code-review")));
        assert_eq!(wants, Wants::Nothing);
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        let (menu, wants) = stays(menu.pick(&skill("code-review")));
        let command = agents::on_skill(CLAUDE, "code-review", None);
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
        // Waiting on zellij, the menu takes no click on a skill.
        assert_eq!(menu.lines(31, 6, 0).1.at(Position::new(4, 3)), None);
        let (_, wants) = stays(menu.pick(&skill("tigerstyle")));
        assert_eq!(wants, Wants::Nothing);
    }

    /// Each key a line of keys names is a spot, with the words after it,
    /// that presses the key it names, in the words the line has room for;
    /// what takes more than a key is none.
    #[test]
    fn a_click_on_a_key_a_line_of_keys_names_presses_it() {
        let keys = [(Hint::Moves, "Move"), (Hint::Enter, "Start"), (Hint::Ctrl('r'), "Rename")];
        let (line, spots) = keyed(50, &keys);
        assert_eq!(line.to_string(), " <↓↑> - Move, <ENTER> - Start, <Ctrl r> - Rename");
        let at = |x| spots.at(Position::new(x, 0)).copied();
        assert_eq!(at(1), None);
        assert_eq!([at(14), at(28)], [Some(KeyEvent::from(KeyCode::Enter)); 2]);
        assert_eq!(at(29), None);
        let rename = Some(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!([at(31), at(47)], [rename, rename]);
        assert_eq!(at(48), None);
        // Short of room, the keys alone; and with less, run together.
        let yes_or_delete = [(Hint::Char('y'), "Yes"), (Hint::Delete, "Delete")];
        let (line, spots) = keyed(12, &yes_or_delete);
        assert_eq!(line.to_string(), " <y>/<Del>");
        let at = |x| spots.at(Position::new(x, 0)).copied();
        let yes = Some(KeyEvent::from(KeyCode::Char('y')));
        let delete = Some(KeyEvent::from(KeyCode::Delete));
        assert_eq!([at(3), at(4), at(5)], [yes, None, delete]);
        assert_eq!(keyed(9, &yes_or_delete).0.to_string(), " <y><Del>");
        let any = [(Hint::AnyKey, "Close"), (Hint::AnyOtherKey, "No")];
        assert_eq!(keyed(40, &any).0.to_string(), " <any key> - Close, <any other key> - No");
        assert_eq!(keyed(40, &any).1, Spots::default());
    }

    /// What closing pane 4 of `luneta` does, as zellij says what it closes,
    /// with `agents` at work there.
    fn plan(closes: Closes, agents: Vec<(u32, Doing)>) -> Result<Plan, String> {
        let session = listed();
        let subject = Subject {
            session: &session,
            seat: Seat::Elsewhere,
            deletes: Deletes::Pane { pane: 4, closes },
            agents: Ok(agents),
            refuge: None,
        };
        close::plan(&subject)
    }

    #[test]
    fn a_delete_with_no_agent_at_work_to_end_goes_ahead_at_once() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Pane(4) });
        assert_eq!(menu.planning(), Some((&listed(), 4)));
        let done = closed(menu.planned(plan(Closes::Pane, vec![(4, Doing::Idle)])));
        let (session, whose) = (listed().name().clone(), listed().whose());
        let act = Act::Pane { session, whose, pane: 4 };
        assert_eq!(done, Done::Act(Machine::Local, act));
        // On a session's row, `d d` deletes the session, which zellij need not
        // say anything of first.
        let (menu, _) = stays(opened(session_row(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Session });
        assert_eq!(menu.planning(), None);
    }

    /// A delete that would end an agent at work asks first. `y`, or the key
    /// again, plans it again and goes ahead with what you were asked about;
    /// once that changed, what it closes or the agents it ends, it asks
    /// again. `n` goes back to the keys.
    #[test]
    fn a_delete_that_would_end_an_agent_at_work_asks_first() {
        let busy = || vec![(4, Doing::Busy)];
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, wants) = stays(menu.planned(plan(Closes::Pane, busy())));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Delete ──────── luneta/main ─",
                " Close the pane? 1 agent, 1",
                " busy",
                " <y> Yes <BS> Back <ESC> Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('n')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.planned(plan(Closes::Pane, busy())));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Pane(4) });
        // Another pane closes with it now: its tab, which it asks of again.
        let tab = || Closes::Tab { id: TabId(2), panes: vec![4, 5] };
        let at_work = vec![(4, Doing::Busy), (5, Doing::Waiting)];
        let (menu, wants) = stays(menu.planned(plan(tab(), at_work)));
        assert_eq!(wants, Wants::Nothing);
        let asking = text(&menu.lines(31, 9, 0).0);
        assert_eq!(asking[1..3], [" Close its tab? 2 agents, 1", " busy, 1 waiting"]);
        // The tab is the same, but its other agent went to work as you read.
        let (menu, _) = stays(menu.key(&char('y')));
        let busier = || vec![(4, Doing::Busy), (5, Doing::Busy)];
        let (menu, wants) = stays(menu.planned(plan(tab(), busier())));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [" Close its tab? 2 agents, 2", " busy"]);
        let (menu, _) = stays(menu.key(&char('y')));
        let (session, whose) = (listed().name().clone(), listed().whose());
        let act = Act::Tab { session, whose, tab: TabId(2), panes: vec![4, 5] };
        assert_eq!(closed(menu.planned(plan(tab(), busier()))), Done::Act(Machine::Local, act));
    }

    /// A delete of the session luneta's zellij is in closes the menu as any
    /// other does: what waits on zellij's client leaving it is the go's.
    #[test]
    fn a_delete_of_the_session_luneta_is_in_closes_the_menu() {
        let (menu, _) = stays(opened(session_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('s')));
        let elo = Place::Session(Listed::new("elo", SystemTime::UNIX_EPOCH));
        let session = listed();
        let subject = Subject {
            session: &session,
            seat: Seat::In,
            deletes: Deletes::Session,
            agents: Ok(Vec::new()),
            refuge: Some(elo.clone()),
        };
        let whose = session.whose();
        let leave = Act::Leave { session: listed().name().clone(), whose, to: elo };
        let done = closed(menu.planned(close::plan(&subject)));
        assert_eq!(done, Done::Act(Machine::Local, leave));
    }

    /// zellij's side, `width` by `height` past `ZELLIJ_X` columns of the
    /// sidebar's, with what `draw` draws floating over it, as text. Each cell
    /// of the box is in gruvbox's colours.
    fn floated<R>(
        draw: impl FnOnce(&mut Buffer, Rect) -> R,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        const ZELLIJ_X: u16 = 10;
        let over = Rect::new(ZELLIJ_X, 0, width, height);
        let mut buffer = Buffer::empty(Rect::new(0, 0, ZELLIJ_X + width, height));
        // What zellij drew there goes under the box.
        for row in over.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        draw(&mut buffer, over);
        let content = buffer.content().iter();
        for cell in content.filter(|cell| !matches!(cell.symbol(), "z" | " ")) {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        let row = |y| (ZELLIJ_X..ZELLIJ_X + width).map(|x| buffer[(x, y)].symbol()).collect();
        (0..height).map(row).collect()
    }

    /// Draws `menu` floating over zellij, its spinner at its first frame.
    fn drawing(menu: &Menu) -> impl FnOnce(&mut Buffer, Rect) -> Drawn + '_ {
        move |buffer, over| menu.draw(buffer, over, UNIX_EPOCH)
    }

    /// The menu over zellij floats at the bottom right of zellij's side, in
    /// a box as wide as the keys of the group open and the rule over them
    /// take, and as tall as its lines.
    #[test]
    fn the_menu_floats_at_the_bottom_right_of_zellij_s_side() {
        let menu = Menu::new(1, pane_row(), PICKED);
        let boxed = |inner: &str| format!("{}│{inner:<24}│", "z".repeat(24));
        assert_eq!(floated(drawing(&menu), 50, 12), [
            "z".repeat(50),
            format!("{}┌─ luneta ─ luneta/main ─┐", "z".repeat(24)),
            boxed(" /  Search skills"),
            boxed(" a  Ask"),
            boxed(" i  File issue"),
            boxed(" c  Skills ▸"),
            boxed(" n  New ▸"),
            boxed(" t  Turn ▸"),
            boxed(" d  Delete ▸"),
            boxed(" b  Brief ▸"),
            boxed(" <ESC> - Close"),
            format!("{}└{}┘", "z".repeat(24), "─".repeat(24)),
        ]);

        let (menu, _) = stays(menu.key(&char('c')));
        let zellij = "z".repeat(7);
        let rule = format!("─ Skills {} luneta/main ─", "─".repeat(18));
        assert_eq!(floated(drawing(&menu), 50, 11)[1..], [
            format!("{zellij}┌{rule}┐"),
            format!("{zellij}│{:<41}│", " t  /thermo-nuclear-review"),
            format!("{zellij}│{:<41}│", " m  /make-illegal-states-unrepresentable"),
            format!("{zellij}│{:<41}│", " s  /tigerstyle"),
            format!("{zellij}│{:<41}│", " f  fix the last command"),
            format!("{zellij}│{:<41}│", " e  explain the last command"),
            format!("{zellij}│{:<41}│", " c  fix the failing checks"),
            format!("{zellij}│{:<41}│", " \u{ec82} claude  <TAB> another agent"),
            format!("{zellij}│{:<41}│", " <BS> - Back, <ESC> - Close"),
            format!("{zellij}└{}┘", "─".repeat(41)),
        ]);
        // Narrower than the keys take, the box is as wide as zellij's side.
        let narrow = floated(drawing(&menu), 30, 11);
        assert_eq!(narrow[2], format!("│{:<28}│", " t  /thermo-nuclear-review"));
    }

    /// Where `text` first shows on `buffer`, each cell before it a character.
    fn drawn_at(buffer: &Buffer, text: &str) -> Option<Position> {
        let area = buffer.area;
        (area.top()..area.bottom()).find_map(|y| {
            let cells = (area.left()..area.right()).map(|x| buffer[(x, y)].symbol());
            let line: String = cells.collect();
            let at = line.find(text)?;
            let x = u16::try_from(line[..at].chars().count()).ok()?;
            Some(Position::new(area.x + x, y))
        })
    }

    /// The spot a click on `text` hits, of the menu floating over zellij's
    /// side, 50 by 10 past 10 columns of the sidebar's: the menu's own.
    fn clicked(menu: &Menu, text: &str) -> Option<Choice> {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 60, 10));
        let drawn = menu.draw(&mut buffer, Rect::new(10, 0, 50, 10), UNIX_EPOCH);
        let at = drawn_at(&buffer, text).expect("drawn");
        assert!(drawn.boxed().is_some_and(|area| area.contains(at)), "{text:?} is in the box");
        drawn.at(at).map(|hit| match hit {
            Hit::Menu(number, choice) if number == menu.number() => choice,
            hit => panic!("{hit:?} is not the menu's"),
        })
    }

    fn pressed(code: KeyCode) -> Choice {
        Choice::Key(KeyEvent::from(code))
    }

    /// Each entry of the menu floating over zellij is a spot where it is
    /// drawn that presses its key, and so is each key the line under them
    /// names, with the words after it. The rule over them is none.
    #[test]
    fn a_click_on_an_entry_or_a_key_of_the_menu_presses_its_key() {
        let menu = Menu::new(1, pane_row(), PICKED);
        assert_eq!(clicked(&menu, "Skills ▸"), Some(pressed(KeyCode::Char('c'))));
        assert_eq!(clicked(&menu, " /  Search"), Some(pressed(KeyCode::Char('/'))));
        assert_eq!(clicked(&menu, "<ESC>"), Some(pressed(KeyCode::Esc)));
        assert_eq!(clicked(&menu, "Close"), Some(pressed(KeyCode::Esc)));
        assert_eq!(clicked(&menu, "─ luneta"), None);

        let (menu, _) = stays(menu.key(&char('c')));
        assert_eq!(clicked(&menu, "/tigerstyle"), Some(pressed(KeyCode::Char('s'))));
        assert_eq!(clicked(&menu, "<BS> - Back"), Some(pressed(KeyCode::Backspace)));
    }

    /// A delete that asks first takes a click on its keys, and none on what
    /// it asks; and once it goes ahead, waiting on zellij, the keys do
    /// nothing but close the menu, and no entry takes a click.
    #[test]
    fn a_click_on_a_delete_asks_first_as_its_key_does() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        assert_eq!(clicked(&menu, "close the pane"), Some(pressed(KeyCode::Char('d'))));
        let (menu, _) = stays(menu.key(&char('d')));
        assert_eq!(clicked(&menu, "close the pane"), None);
        let busy = vec![(4, Doing::Busy)];
        let (menu, _) = stays(menu.planned(plan(Closes::Pane, busy)));
        assert_eq!(clicked(&menu, "<y>"), Some(pressed(KeyCode::Char('y'))));
        assert_eq!(clicked(&menu, "Close the pane?"), None);
    }

    /// What went wrong with the menu over zellij floats where it did, in a
    /// box as wide as what it says takes, over the key that closes it.
    #[test]
    fn a_note_floats_where_the_menu_does() {
        let note = "menu: luneta does not know yet which pane zellij is on; try again in a moment";
        let zellij = "z".repeat(5);
        let boxed = |inner: &str| format!("{zellij}│{inner:<43}│");
        assert_eq!(floated(|buffer, over| draw_note(buffer, over, note), 50, 7), [
            "z".repeat(50),
            "z".repeat(50),
            format!("{zellij}┌─ luneta {}┐", "─".repeat(34)),
            boxed(" menu: luneta does not know yet which pane"),
            boxed(" zellij is on; try again in a moment"),
            boxed(" <any key> - Close"),
            format!("{zellij}└{}┘", "─".repeat(43)),
        ]);
    }

    fn pull_request() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    fn link() -> Work {
        Work::Link(pull_request())
    }

    /// The menu for the pull request, pasted on `row`, its key `key` pressed,
    /// waiting for luneta to find where it goes.
    fn pulling_on(row: Target, key: char) -> Menu {
        let (menu, wants) = stays(Menu::pull(1, row, link(), CLAUDE).key(&char(key)));
        let wanted = if key == 'r' { Wanted::AtHead } else { Wanted::AsItIs };
        assert_eq!(wants, Wants::Pull(link(), wanted));
        assert!(menu.pulling());
        menu
    }

    /// The same, pasted on a pane's row of this machine.
    fn pulling(key: char) -> Menu {
        pulling_on(pane_row(), key)
    }

    /// What luneta found, on this machine.
    fn here(ready: Ready) -> Found {
        Found { on: Machine::Local, ready }
    }

    /// A paste opens the menu for the pull request, named for it, with its
    /// repository where another menu says where its pane works. A key waits
    /// for luneta to find where it goes and to make the worktree, and says
    /// so after a spinner that turns, and does nothing else meanwhile, but
    /// close the menu.
    #[test]
    fn the_menu_for_a_pull_request_has_a_key_for_a_worktree_and_one_for_a_review() {
        let menu = Menu::pull(1, pane_row(), link(), CLAUDE);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ #2736 ──────── cashubtc/cdk ─",
                " w  worktree",
                " r  /thermo-nuclear-review",
                " \u{ec82} claude  <TAB> another agent",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
        assert!(!menu.turning());
        let syncing = Menu::pull(1, pane_row(), link(), CLAUDE);
        let (syncing, wants) = stays(syncing.key(&char('r')));
        assert_eq!(wants, Wants::Pull(link(), Wanted::AtHead));
        assert_eq!(text(&syncing.lines(31, 9, 0).0)[1], " ⠋ Syncing #2736…");
        let (menu, wants) = stays(menu.key(&char('w')));
        assert_eq!(wants, Wants::Pull(link(), Wanted::AsItIs));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            ["─ #2736 ──────── cashubtc/cdk ─", " ⠋ Making a worktree of #2736…", " <ESC> - Close"]
        );
        assert_eq!(text(&menu.lines(31, 9, 3).0)[1], " ⠸ Making a worktree of #2736…");
        assert!(menu.turning());
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        assert!(menu.pulling());
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    /// `w` opens a tab in the worktree, named for the pull request, in the
    /// session found, and `r` the same with an agent on a review of it named
    /// for the branch; or else, where a pane has the branch checked out, `w`
    /// goes there and `r` opens the agent beside it. The menu is then for
    /// where it opens, for luneta to take its client there.
    #[test]
    fn a_key_for_a_pull_request_opens_where_luneta_found_it_goes() {
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let worktree = "/w/cdk/feat-x".to_owned();
        let branch = Some("feat/x".to_owned());
        let ready = |at| {
            let (worktree, branch) = (worktree.clone(), branch.clone());
            let work = Readied::Link(pull_request());
            Ok(here(Ready { at, worktree, branch, moved_off: false, work }))
        };
        let review = [
            "claude",
            "/thermo-nuclear-review https://github.com/cashubtc/cdk/pull/2736",
        ]
        .map(str::to_owned)
        .to_vec();
        let session = || At::Session((other.clone(), 7));
        for (key, command) in [('w', Vec::new()), ('r', review.clone())] {
            let (menu, wants) = stays(pulling(key).pulled(ready(session())));
            let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
            let cwd = directory(&worktree);
            assert_eq!(wants, Wants::Open { session: other.clone(), pane: 7, cwd, opens });
            assert_eq!(menu.opening(), Some(Opening::In(other.clone())));
            let Target::Pane(_, row) = menu.target() else { panic!("not a pane's row") };
            assert_eq!((&row.session, row.pane), (&other, 7));
            assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " ⠋ Opening #2736…");
            assert!(menu.turning());
        }
        let there = || At::There((other.clone(), 9));
        let place = Place::Pane(PanePlace { session: other.clone(), pane: 9 });
        assert_eq!(closed(pulling('w').pulled(ready(there()))), Done::Go(Machine::Local, place));
        let (menu, wants) = stays(pulling('r').pulled(ready(there())));
        let opens = Opens::Pane { beside: 9, command: review };
        let cwd = directory(&worktree);
        assert_eq!(wants, Wants::Open { session: other.clone(), pane: 9, cwd, opens });
        let Target::Pane(_, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((&row.session, row.pane), (&other, 9));
    }

    /// `Tab` in the menu for a pull request picks the next harness, and
    /// wants it kept, and its line then names that one; `r` then
    /// starts the review in that one, by its spelling of the skill. A click
    /// on that line is a `Tab`. Once a key waits, `Tab` does nothing.
    #[test]
    fn tab_in_the_menu_for_a_pull_request_starts_its_agent_in_the_other_harness() {
        let menu = Menu::pull(1, pane_row(), link(), CLAUDE);
        let tab = KeyEvent::from(KeyCode::Tab);
        assert_eq!(menu.lines(31, 9, 0).1.at(Position::new(5, 3)), Some(&Choice::Key(tab)));
        let (menu, wants) = stays(menu.key(&tab));
        assert_eq!(wants, Wants::Keep(Harness::Pi));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[3], " \u{f03ff} pi  <TAB> another agent");
        let (menu, _) = stays(menu.key(&char('r')));
        let (menu, wants) = stays(menu.key(&tab));
        assert_eq!(wants, Wants::Nothing);
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let worktree = "/w/cdk/feat-x".to_owned();
        let work = Readied::Link(pull_request());
        let at = At::Session((other.clone(), 7));
        let branch = Some("feat/x".to_owned());
        let ready = Ready { at, worktree: worktree.clone(), branch, moved_off: false, work };
        let (_, wants) = stays(menu.pulled(Ok(here(ready))));
        let review = [
            "pi",
            "/skill:thermo-nuclear-review https://github.com/cashubtc/cdk/pull/2736",
        ];
        let command = review.map(str::to_owned).to_vec();
        let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
        let cwd = directory(&worktree);
        assert_eq!(wants, Wants::Open { session: other, pane: 7, cwd, opens });
    }

    /// A paste of an issue's link opens the menu for the issue, named for it,
    /// with `t` where the menu for a pull request has `r`. `t` opens a tab in
    /// the worktree of the issue's branch, named for it, with an agent told in
    /// words to tackle it, which no skill names.
    #[test]
    fn the_menu_for_an_issue_has_a_key_for_a_worktree_and_one_to_tackle_it() {
        let link = Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link");
        let issue = Work::Link(link.clone());
        let menu = Menu::pull(1, pane_row(), issue.clone(), CLAUDE);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ #2736 ──────── cashubtc/cdk ─",
                " w  worktree",
                " t  tackle",
                " \u{ec82} claude  <TAB> another agent",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Pull(issue, Wanted::AsItIs));
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let worktree = "/w/cdk/issue-2736".to_owned();
        let ready = Ready {
            at: At::Session((other.clone(), 7)),
            worktree: worktree.clone(),
            branch: Some("issue-2736".to_owned()),
            moved_off: false,
            work: Readied::Link(link),
        };
        let (menu, wants) = stays(menu.pulled(Ok(here(ready))));
        let tackle = ["claude", "Tackle https://github.com/cashubtc/cdk/issues/2736"];
        let command = tackle.map(str::to_owned).to_vec();
        let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
        let cwd = directory(&worktree);
        assert_eq!(wants, Wants::Open { session: other, pane: 7, cwd, opens });
        assert!(menu.opening().is_some());
    }

    #[test]
    fn t_on_a_worktree_that_moved_off_stays_open_saying_why_and_w_goes_on_from_there() {
        let link = Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link");
        let issue = Work::Link(link.clone());
        let (menu, _) = stays(Menu::pull(1, pane_row(), issue.clone(), CLAUDE).key(&char('t')));
        let name = SessionName::parse("cdk").unwrap();
        let ready = Ready {
            at: At::Clone { cwd: directory("/w/cdk"), name },
            worktree: "/w/cdk/issue-2736".to_owned(),
            branch: Some("dev-docs".to_owned()),
            moved_off: true,
            work: Readied::Link(link),
        };
        let (menu, wants) = stays(menu.pulled(Ok(here(ready))));
        assert_eq!(wants, Wants::Nothing);
        assert!(!menu.turning());
        let refused = [
            "─ #2736 ──────── cashubtc/cdk ─",
            " w  worktree",
            " t  tackle",
            " \u{ec82} claude  <TAB> another agent",
            " /w/cdk/issue-2736 is on",
            " dev-docs, not issue-2736, so",
            " no agent starts there; w",
            " opens it",
            " <ESC> - Close",
        ];
        let (lines, spots) = menu.lines(31, 9, 0);
        assert_eq!(text(&lines), refused);
        assert_eq!(spots.at(Position::new(4, 1)), Some(&pressed(KeyCode::Char('w'))));

        let (menu, wants) = stays(menu.key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9, 0).0), refused);

        let (menu, wants) = stays(menu.key(&char('w')));
        assert_eq!(wants, Wants::Pull(issue, Wanted::AsItIs));
        assert!(menu.pulling());
    }

    /// A paste of a file's path opens the menu for the file, named for it,
    /// with where the file is where another says its repository. `t` opens a
    /// tab in the worktree of the file's branch, named for it, with an agent
    /// told in words to tackle the file by its real path, and let read it
    /// there, outside the worktree. `w` opens the tab alone. Why luneta found
    /// nothing says what was pasted, as it was.
    #[test]
    fn the_menu_for_a_file_has_a_key_for_a_worktree_and_one_to_tackle_it() {
        let file = || {
            let from = Directory::parse("/w/cdk/main".to_owned()).unwrap();
            Work::parse("docs/plans/sync-rework.md\n", Some(from)).unwrap()
        };
        let menu = Menu::pull(1, pane_row(), file(), CLAUDE);
        assert_eq!(
            text(&menu.lines(38, 9, 0).0),
            [
                "─ sync-rework ─ plans/sync-rework.md ─",
                " w  worktree",
                " t  tackle",
                " \u{ec82} claude  <TAB> another agent",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Pull(file(), Wanted::AsItIs));
        assert_eq!(text(&menu.lines(38, 9, 0).0)[1], " ⠋ Making a worktree of sync-rework…");
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let (worktree, real) = ("/w/cdk/md-sync-rework", "/w/cdk/main/docs/plans/sync-rework.md");
        let ready = || Ready {
            at: At::Session((other.clone(), 7)),
            worktree: worktree.to_owned(),
            branch: Some("md-sync-rework".to_owned()),
            moved_off: false,
            work: Readied::File { real: real.to_owned() },
        };
        let (_, wants) = stays(menu.pulled(Ok(here(ready()))));
        let command = [
            "claude",
            "--allowedTools",
            "Read(//w/cdk/main/docs/plans/sync-rework.md)",
            "--",
            "Tackle /w/cdk/main/docs/plans/sync-rework.md",
        ]
        .map(str::to_owned)
        .to_vec();
        let opens = Opens::Tab { name: Some("sync-rework".to_owned()), command };
        let session = other.clone();
        let cwd = directory(worktree);
        assert_eq!(wants, Wants::Open { session, pane: 7, cwd, opens });

        let pressed = |key: char| {
            stays(Menu::pull(1, pane_row(), file(), CLAUDE).key(&char(key))).0
        };
        let (_, wants) = stays(pressed('w').pulled(Ok(here(ready()))));
        let opens = Opens::Tab { name: Some("sync-rework".to_owned()), command: Vec::new() };
        let session = other.clone();
        let cwd = directory(worktree);
        assert_eq!(wants, Wants::Open { session, pane: 7, cwd, opens });

        assert_eq!(
            closed(pressed('w').pulled(Err("no such file".to_owned()))),
            Done::Wrong("menu: docs/plans/sync-rework.md: no such file".to_owned())
        );
    }

    /// Where no session works in the repository, luneta makes one in its
    /// clone, and the menu waits for zellij to make it and open the tab, and
    /// says so.
    #[test]
    fn a_session_luneta_makes_for_a_pull_request_opens_its_tab_once_there() {
        let name = SessionName::parse("cdk").unwrap();
        let at = At::Clone { cwd: directory("/w/cdk"), name: name.clone() };
        let work = Readied::Link(pull_request());
        let worktree = "/w/cdk/feat-x".to_owned();
        let ready = Ready { at, worktree, branch: None, moved_off: false, work };
        let (menu, wants) = stays(pulling('w').pulled(Ok(here(ready))));
        assert_eq!(
            wants,
            Wants::Make {
                session: name.clone(),
                cwd: directory("/w/cdk"),
                tab_cwd: directory("/w/cdk/feat-x"),
                name: "#2736".to_owned(),
                command: Vec::new(),
            }
        );
        assert_eq!(menu.opening(), Some(Opening::Made(name)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " ⠋ Making session cdk…");
        assert!(menu.turning());
        // Why luneta found nothing closes the menu, and says so.
        let why = "no session works in cashubtc/cdk".to_owned();
        assert_eq!(
            closed(pulling('r').pulled(Err(why))),
            Done::Wrong("menu: cashubtc/cdk#2736: no session works in cashubtc/cdk".to_owned())
        );
    }

    /// A link pasted on a row of another machine is found there, and what
    /// its keys open is that machine's: the pane with the branch, a tab of a
    /// session there, or a session made there, which the menu is for until
    /// zellij opens a pane of it.
    #[test]
    fn the_keys_for_a_link_pasted_on_another_machine_s_row_open_there() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let ready = |at| {
            let work = Readied::Link(pull_request());
            let worktree = "/w/cdk/feat-x".to_owned();
            let ready = Ready { at, worktree, branch: None, moved_off: false, work };
            Ok(Found { on: vps(), ready })
        };
        let row = || Target::Pane(vps(), row());
        let place = Place::Pane(PanePlace { session: other.clone(), pane: 9 });
        let there = ready(At::There((other.clone(), 9)));
        assert_eq!(closed(pulling_on(row(), 'w').pulled(there)), Done::Go(vps(), place));
        let (menu, _) = stays(pulling_on(row(), 'w').pulled(ready(At::Session((other, 7)))));
        let Target::Pane(machine, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((machine, row.pane), (&vps(), 7));
        let name = SessionName::parse("cdk").unwrap();
        let clone = At::Clone { cwd: directory("/w/cdk"), name };
        let header = Target::Machine(vps());
        let (menu, wants) = stays(pulling_on(header.clone(), 'w').pulled(ready(clone)));
        assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
        assert_eq!(menu.target(), &header);
    }

    /// A link found on another machine than the row's opens there, whichever
    /// row of this machine it was pasted on, a folded session's among them:
    /// the menu is for that machine from then on, and takes you there.
    #[test]
    fn what_a_key_found_on_another_machine_opens_there_from_any_row() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let found = |at| {
            let work = Readied::Link(pull_request());
            let worktree = "/w/cdk/feat-x".to_owned();
            let ready = Ready { at, worktree, branch: None, moved_off: false, work };
            Ok(Found { on: vps(), ready })
        };
        let rows = [session_row(), pane_row(), Target::Machine(Machine::Local)];
        for row in rows {
            let (menu, wants) =
                stays(pulling_on(row.clone(), 'r').pulled(found(At::There((other.clone(), 9)))));
            assert!(matches!(wants, Wants::Open { pane: 9, .. }), "{wants:?}");
            let Target::Pane(machine, opening) = menu.target() else { panic!("not a pane's row") };
            assert_eq!((machine, opening.pane), (&vps(), 9));

            let place = Place::Pane(PanePlace { session: other.clone(), pane: 9 });
            let goes = pulling_on(row.clone(), 'w').pulled(found(At::There((other.clone(), 9))));
            assert_eq!(closed(goes), Done::Go(vps(), place));

            let name = SessionName::parse("cdk").unwrap();
            let clone = At::Clone { cwd: directory("/w/cdk"), name };
            let (menu, wants) = stays(pulling_on(row, 'r').pulled(found(clone)));
            assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
            assert_eq!(menu.target(), &Target::Machine(vps()));
        }
    }
}
