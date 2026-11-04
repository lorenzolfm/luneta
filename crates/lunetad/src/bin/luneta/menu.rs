//! The menu: a key, then another, and a pane opens where you work, as
//! which-key shows the keys in neovim. It opens at the bottom of the sidebar,
//! for the row highlighted then, or floats in a box at the bottom right of
//! zellij's side, for the pane zellij is on.
//!
//! Its keys are luneta's own. `/` searches every skill the directory of the
//! row's pane can run, for an agent to start on the one you pick, beside that
//! pane. `a` opens the ask box, for an agent to do what you say in words, as
//! the menu would. `c` opens the Claude group, a key for each of a few skills, which
//! starts an agent on it there, and `f` and `e`, which start one on what the
//! command a shell ran last printed, to fix what went wrong or explain it.
//! `n` opens the New group: your shell beside the
//! pane, a tab of its session, or a session, in the directory it works in, or
//! a machine for lunetad to watch through ssh, by what you type ssh to it. `t`
//! opens the Turn group: the pane into a tab of its own, or its tab into panes
//! of another you pick, what runs in them running on. `d`
//! opens the Delete group, which closes the pane or deletes its session, and
//! asks first when that would end an agent at work, and on a machine's header
//! forgets the machine. `b` opens the Brief group, which briefs you on every
//! session, the row's session, or the agent in its pane, or on a machine's
//! header, the machine's sessions. luneta has no
//! configuration to read keys for skills from, and the Claude group's are its
//! own.
//!
//! A pull request's link pasted with the sidebar's keys, or in the menu,
//! opens it for that pull request instead: `w` makes a worktree of it, in a
//! tab of the session that works in its repository, and `r` starts an agent
//! on a review of it there too. Where a pane has its branch checked out
//! already, `w` takes you there, and `r` starts the agent beside it. An
//! issue's link opens it for that issue: `w` makes a worktree of a branch for
//! it, and `t` starts an agent there on tackling it.
//!
//! At the top of the sidebar, a key the sidebar has no use of its own for
//! opens the menu on the group it opens, and `m` opens it on its first group,
//! where every key is the menu's. While the sidebar has the focus and no menu
//! is open, the legend at its bottom lists the keys it takes there.

use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use luneta_protocol::said::Said;
use lunetad::reviews::{self, Move, SKILLS as REVIEW_SKILLS};
use lunetad::Destination;
use lunetad::turns::{OtherTab, Turn};
use luneta_protocol::shell::Ending;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::brief;
use crate::close::{Act, Plan, Scope};
use crate::gruvbox;
use crate::machines::Machine;
use crate::printed::{self, Printed};
use crate::pull::{At, Kind, Readied, Ready, Work};
use crate::sidebar::{self, ACCENT, LIT, Listed, Mark, NAME, PanePlace, Place, TAG};
use crate::skills::{Name, Search, Skills};
use crate::spots::{Drawn, Spots};
use crate::zellij::SessionName;

/// The key that opens the search over the skills, at the top of the menu, and
/// what the menu says it does.
const SEARCH: char = '/';
const SEARCH_DOES: &str = "Search skills";

/// The key that opens the ask box, at the top of the menu, and what the menu
/// says it does.
const ASK: char = 'a';
const ASK_DOES: &str = "Ask";

/// The key of the Claude group, its name, and its keys, each with the skill
/// it starts an agent on: the skills the author runs most. luneta has no
/// configuration to read yours from.
const SKILLS: char = 'c';
const SKILLS_NAME: &str = "Claude";
const SKILL_KEYS: [(char, &str); 3] = [
    ('t', REVIEW_SKILLS[0]),
    ('m', REVIEW_SKILLS[1]),
    ('s', REVIEW_SKILLS[2]),
];

/// What the `r` of the menu for a pull request says it does: it starts an
/// agent on a review of it, the skills above in turn.
const REVIEW_DOES: &str = "review";

/// What the `t` of the menu for an issue tells an agent, before the link: in
/// words, as you would, with no skill.
const TACKLE: &str = "Tackle";

/// The key of the Review group, for the row of an agent on a review, and its
/// name.
const REVIEW: char = 'r';
const REVIEW_NAME: &str = "Review";

/// The keys of the Review group.
const REVIEW_HOLD: char = 'h';
const REVIEW_NEXT: char = 'n';
const REVIEW_SKIP: char = 's';

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

/// What a key that starts an agent starts.
const CLAUDE: &str = "claude";

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

/// What the menu says of a key that starts an agent, or opens a pane, a tab
/// or a session, on a session's row, which the highlight is only on while it
/// lists none of the session's panes: folded, or with none to list.
const NO_ROW_PANE: &str = "menu: a session's row has no pane to open beside, nor a directory to \
                           open in; highlight a pane, unfolding its session first if it is folded";

/// What the menu says of such a key on a machine's header.
const NO_MACHINE_PANE: &str = "menu: a machine's header has no pane to open beside, nor a \
                               directory to open in; highlight a pane, unfolding the machine first \
                               if it is folded";

/// What the menu says of a key of the Turn group on a session's row, which
/// lists no pane while the highlight is on it.
const NO_ROW_TURN: &str =
    "menu: a session's row has no pane to turn; highlight a pane, unfolding its session first if \
     it is folded";

/// What the menu says of such a key on a machine's header.
const NO_MACHINE_TURN: &str =
    "menu: a machine's header has no pane to turn; highlight a pane, unfolding the machine first \
     if it is folded";

/// What the menu says of the key that forgets a machine, on this one's
/// header.
const LOCAL_STAYS: &str = "menu: luneta lists this machine always; it forgets only the others";

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
    /// Opens the pull request of the highlighted row in your browser.
    Open,
}

impl Command {
    /// Every command, in the order the legend lists them.
    const ALL: [Command; 4] = [Command::Search, Command::Unread, Command::Reorder, Command::Open];

    const fn key(self) -> char {
        match self {
            Command::Search => '/',
            Command::Unread => 'u',
            Command::Reorder => 'v',
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
    /// down, or up. Without a selection, nothing.
    Shift(isize),
    /// `Some(true)` folds, `Some(false)` unfolds, and `None` turns it over.
    Fold(Option<bool>),
    /// Opens the menu on its first group, where every key is the menu's.
    Menu,
    /// Waits for what to yank, as vim's `y` waits for a motion: `YANK_PULL`
    /// copies the link of the highlighted row's pull request. Any other key
    /// drops it, `Esc` doing only that, as after a lone `First`.
    Yank,
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
            _ => None,
        }
    }

    /// What `key` does at the top of the sidebar. None for a key the menu has
    /// the say on.
    pub fn of(key: char) -> Option<Top> {
        Top::moves(key).or_else(|| Command::of(key).map(Top::Command))
    }
}

/// No command takes the key of the ask box, or of luneta's Claude, New, Turn,
/// Delete or Brief group, which would be out of reach at the top of the
/// sidebar, and gone from the legend; nor a key the sidebar moves, folds or
/// opens the menu with, which `Top::of` finds first. Nor does the sidebar
/// move with the ask box's.
const _: () = {
    let mut at = 0;
    while at < Command::ALL.len() {
        let key = Command::ALL[at].key();
        let group = key == ASK
            || key == SKILLS
            || key == REVIEW
            || key == NEW
            || key == TURN
            || key == DELETE
            || key == BRIEF;
        assert!(!group, "a command on a key of luneta's groups");
        assert!(Top::moves(key).is_none(), "a command on a key the sidebar moves with");
        at += 1;
    }
    assert!(Top::moves(ASK).is_none(), "the ask box on a key the sidebar moves with");
};

/// What a key of the New group opens, for the pane the menu is for, in the
/// directory it works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum New {
    /// Your shell, in a pane of its own beside that pane.
    Pane,
    /// A tab of that pane's session.
    Tab,
    /// A session, named for the directory, as the picker's directories are.
    Session,
    /// A machine for lunetad to watch through ssh, by its destination, typed
    /// in a box: on any row.
    Machine,
}

impl New {
    /// Each, in the order the New group shows them.
    const ALL: [New; 4] = [New::Pane, New::Tab, New::Session, New::Machine];

    const fn key(self) -> char {
        match self {
            New::Pane => 'p',
            New::Tab => 't',
            New::Session => 's',
            New::Machine => 'm',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            New::Pane => "new pane",
            New::Tab => "new tab",
            New::Session => "new session",
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

/// A key of the Claude group that starts an agent on what the command a
/// shell ran last printed, beside its pane: to fix what went wrong, or to
/// explain it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    Fix,
    Explain,
}

impl Last {
    /// Each, in the order the Claude group shows them, after its skills.
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

/// No key of the Claude group's skills is one of `Last`'s, which would hide
/// it.
const _: () = {
    let mut at = 0;
    while at < Last::ALL.len() {
        let mut skill = 0;
        while skill < SKILL_KEYS.len() {
            let alike = SKILL_KEYS[skill].0 == Last::ALL[at].key();
            assert!(!alike, "two keys alike in the Claude group");
            skill += 1;
        }
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
    const fn does(self, asks: Asks) -> &'static str {
        match (self, asks) {
            (Pull::Worktree, _) => "worktree",
            (Pull::Agent, Asks::Review) => REVIEW_DOES,
            (Pull::Agent, Asks::Tackle) => "tackle",
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
    /// A key of the Claude group: starts an agent on the prompt, a skill.
    Start { key: char, prompt: &'static str },
    /// A key of the Claude group: starts an agent on what the command the
    /// pane's shell ran last printed.
    Last(Last),
    /// A key of the Review group: moves the review, as `says` says.
    Review { key: char, does: Move, says: String },
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
    /// The key of the Delete group on a machine's header: forgets the machine.
    Forget(Machine),
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
}

impl Entry {
    fn key(&self) -> char {
        match self {
            Entry::Group { key, .. }
            | Entry::Start { key, .. }
            | Entry::Review { key, .. }
            | Entry::Close { key, .. }
            | Entry::Tab { key, .. } => *key,
            Entry::Forget(_) => DELETE_ROW,
            Entry::New(new) => new.key(),
            Entry::Turn(turning) => turning.key(),
            Entry::Last(last) => last.key(),
            Entry::Brief(scope) => scope.key(),
            Entry::Command(command) => command.key(),
            Entry::YankPull => YANK_PULL,
            Entry::Pull(pull, asks) => pull.key(*asks),
            Entry::Search => SEARCH,
            Entry::Ask => ASK,
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
    /// Starts an agent on the prompt.
    Start(String),
    /// Starts an agent on what the pane's shell printed last.
    Last(Last),
    /// Moves the review the menu's row's agent runs.
    Review(Move),
    /// Opens a pane, a tab or a session.
    New(New),
    /// Turns the pane the menu is for, or its tab.
    Turn(TurnKey),
    /// Closes the pane the menu is for, or deletes its session, `session`,
    /// by `key`.
    Delete { key: char, session: Listed, scope: Scope },
    /// Forgets the machine the menu is for.
    Forget(Machine),
    /// Asks for a brief of what the scope covers.
    Brief(brief::Scope),
    /// Opens the ask box.
    Ask,
    /// Closes the menu.
    Close,
    /// Nothing: the group open has no such key.
    Nothing,
}

/// The menu's entries, the groups opened so far, and the search box over the
/// skills while it is open.
struct Keys {
    root: Vec<Entry>,
    /// Each group opened, by where it stands in the one before.
    open: Vec<usize>,
    search: Option<Search>,
}

impl Keys {
    /// The menu's keys: `/`, which opens the search over the skills, `a`,
    /// which opens the ask box, and the Claude, New, Turn, Delete and Brief
    /// groups: `c t`, `c m` and `c s` start an
    /// agent on a skill, `c f` and `c e` one on what the pane's shell printed
    /// last, `n p`, `n t` and `n s` open a pane, a tab and a
    /// session, and `n m` the box for a machine's destination, `t t` turns the
    /// pane into a tab and `t p` its tab into panes of another, `d d` deletes
    /// what the menu is for, as `removes` says, and `d s` deletes its
    /// session, or on a machine's header `d d` alone forgets the machine, and
    /// `b z`, `b s` and `b h` brief you on every session, its session and
    /// what the menu is for, or on a machine's header, its sessions. On the
    /// row of an agent on a review with a skill still to go, `r h` holds it,
    /// or goes on from a hold, `r n` sends the next skill and `r s` skips it.
    fn new(removes: Removes, reviewing: Option<&Reviewing>) -> Self {
        let start = |(key, prompt)| Entry::Start { key, prompt };
        let mut root = vec![
            Entry::Search,
            Entry::Ask,
            Entry::Group {
                key: SKILLS,
                name: SKILLS_NAME,
                entries: SKILL_KEYS
                    .into_iter()
                    .map(start)
                    .chain(Last::ALL.map(Entry::Last))
                    .collect(),
            },
        ];
        root.extend(reviewing.map(|reviewing| Entry::Group {
            key: REVIEW,
            name: REVIEW_NAME,
            entries: reviewing.entries(),
        }));
        root.extend([
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
                    Removes::Row { session, scope } => vec![
                        Entry::Close { key: DELETE_ROW, session: session.clone(), scope },
                        Entry::Close { key: DELETE_SESSION, session, scope: Scope::Session },
                    ],
                    Removes::Machine(machine) => vec![Entry::Forget(machine)],
                },
            },
            Entry::Group {
                key: BRIEF,
                name: BRIEF_NAME,
                entries: brief::Scope::ALL.into_iter().map(Entry::Brief).collect(),
            },
        ]);
        Keys { root, open: Vec::new(), search: None }
    }

    /// The legend: the sidebar's commands, and what `y` waits on, which it
    /// lists as a group, then the keys at the top of the menu, but for those
    /// a command has, or the sidebar moves with, which `m` reaches. It shows
    /// the Delete group's key and not what it deletes, which is the row's, so
    /// any row's will do.
    fn legend() -> Self {
        let mut root: Vec<Entry> = Command::ALL.into_iter().map(Entry::Command).collect();
        root.push(Entry::Group { key: YANK, name: YANK_NAME, entries: YANKS.into() });
        let shown = |entry: &Entry| Top::of(entry.key()).is_none();
        let keys = Keys::new(Removes::Machine(Machine::Local), None);
        root.extend(keys.root.into_iter().filter(shown));
        Keys { root, open: Vec::new(), search: None }
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

    /// What `key` does: a key the group open shows opens its group, opens or
    /// closes what it opens or closes, or asks for the search box,
    /// `Backspace` goes back a group, and out of the menu from the first, and
    /// `Esc` closes it. While the search box is open, it takes the keys
    /// instead.
    fn key(&mut self, key: &KeyEvent) -> Pressed {
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
            Entry::Start { prompt, .. } => return Pressed::Start((*prompt).to_owned()),
            Entry::Last(last) => return Pressed::Last(*last),
            Entry::Review { does, .. } => return Pressed::Review(does.clone()),
            Entry::New(new) => return Pressed::New(*new),
            Entry::Turn(turning) => return Pressed::Turn(*turning),
            Entry::Close { key, session, scope } => {
                return Pressed::Delete { key: *key, session: session.clone(), scope: *scope };
            },
            Entry::Forget(machine) => return Pressed::Forget(machine.clone()),
            Entry::Brief(scope) => return Pressed::Brief(*scope),
            Entry::Search => return Pressed::Search,
            Entry::Ask => return Pressed::Ask,
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
fn closes(key: &KeyEvent) -> bool {
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
            Some(skill) => (Pressed::Start(format!("/{}", skill.name)), true),
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
/// the pane or the session, of `session`; on a machine's header, the
/// machine, from those luneta lists.
enum Removes {
    Row { session: Listed, scope: Scope },
    Machine(Machine),
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
/// its agent while zellij has not said, the branch checked out there, and
/// what its shell said it ran last, as the daemon last said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRow {
    pub session: Listed,
    pub pane: u32,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub ran: Ran,
    /// The review its agent runs, where a skill is still to go: the Review
    /// group's keys have nothing to do on the last.
    pub reviewing: Option<Reviewing>,
}

/// A review the agent of the menu's row runs, for the Review group's keys: by
/// the agent's pid, as the daemon of its machine keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reviewing {
    pub pid: u32,
    /// The skill that goes next.
    pub next: String,
    /// You hold it.
    pub held: bool,
}

impl Reviewing {
    /// The Review group's keys, each saying what it does to this one.
    fn entries(&self) -> Vec<Entry> {
        let (hold, says) = if self.held {
            (Move::Resume, "go on".to_owned())
        } else {
            (Move::Hold, "hold".to_owned())
        };
        let next = format!("send {}", self.next);
        let skip = format!("skip {}", self.next);
        vec![
            Entry::Review { key: REVIEW_HOLD, does: hold, says },
            Entry::Review { key: REVIEW_NEXT, does: Move::Next(self.next.clone()), says: next },
            Entry::Review { key: REVIEW_SKIP, does: Move::Skip(self.next.clone()), says: skip },
        ]
    }
}

/// What the shell in the pane of the menu's row said last through luneta's
/// hook, as the daemon passed it on: a key that starts an agent on what its
/// last command printed takes one that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    /// No shell with the hook runs in the pane: one with it says it is ready
    /// from its first prompt.
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

    /// The directory its pane works in, where the daemon has said it.
    pub fn cwd(&self) -> Option<&str> {
        match self {
            Target::Session(..) | Target::Machine(_) => None,
            Target::Pane(_, row) => row.cwd.as_deref(),
        }
    }

    /// The branch checked out where its pane works.
    pub fn branch(&self) -> Option<&str> {
        match self {
            Target::Session(..) | Target::Machine(_) => None,
            Target::Pane(_, row) => row.branch.as_deref(),
        }
    }

    /// What luneta's Delete group takes away on the row: its pane, or on a
    /// session's row, its session; or on a machine's header, the machine.
    fn removes(&self) -> Removes {
        match self {
            Target::Session(_, session) => {
                Removes::Row { session: session.clone(), scope: Scope::Session }
            },
            Target::Pane(_, row) => {
                Removes::Row { session: row.session.clone(), scope: Scope::Pane(row.pane) }
            },
            Target::Machine(machine) => Removes::Machine(machine.clone()),
        }
    }

    /// The review the agent of its row runs, where one has a skill to go.
    fn reviewing(&self) -> Option<&Reviewing> {
        match self {
            Target::Pane(_, row) => row.reviewing.as_ref(),
            Target::Session(..) | Target::Machine(_) => None,
        }
    }

    /// The pane's row a key of the Turn group turns, or why there is none: a
    /// session's row has no pane, nor a machine's header.
    fn turned(&self) -> Result<&PaneRow, String> {
        match self {
            Target::Session(..) => Err(NO_ROW_TURN.to_owned()),
            Target::Pane(_, row) => Ok(row),
            Target::Machine(_) => Err(NO_MACHINE_TURN.to_owned()),
        }
    }

    /// The pane a key starts an agent or opens a pane beside, or why there
    /// is none: a session's row has no pane, nor a machine's header.
    fn beside(&self) -> Result<Beside, String> {
        match self {
            Target::Session(..) => Err(NO_ROW_PANE.to_owned()),
            Target::Pane(_, row) => row.beside(),
            Target::Machine(_) => Err(NO_MACHINE_PANE.to_owned()),
        }
    }

    /// What a key that starts an agent on what the pane's shell printed
    /// last starts it on, beside the pane, or why it starts none: a
    /// session's row has no pane, nor a machine's header.
    fn ended(&self) -> Result<(Beside, Ended), String> {
        match self {
            Target::Session(..) => Err(NO_ROW_PANE.to_owned()),
            Target::Pane(_, row) => row.ended(),
            Target::Machine(_) => Err(NO_MACHINE_PANE.to_owned()),
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
    cwd: String,
}

impl PaneRow {
    /// The pane a key starts an agent or opens a pane beside, and the
    /// directory it works in, where a tab or a session opens too, or why
    /// there is none: a pane the daemon has said no directory of yet, nor of
    /// an agent in it, opens nothing, rather than wherever zellij runs.
    fn beside(&self) -> Result<Beside, String> {
        let Some(cwd) = self.cwd.clone() else {
            return Err(format!(
                "menu: luneta does not know yet where pane {} works; try again in a moment",
                self.pane
            ));
        };
        Ok(Beside { session: self.session.clone(), pane: self.pane, cwd })
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
            Ran::Unhooked => Err(format!(
                "menu: pane {pane} runs no shell with luneta's hook; a fish started since the \
                 hook was installed has it"
            )),
        }
    }
}

/// Claude Code on `prompt`, as a command line. A prompt that runs a skill
/// names the agent for the skill and the branch it works on, as `/rename`
/// would, so that the sidebar and the picker say what it is doing from the
/// start.
fn claude(prompt: &str, branch: Option<&str>) -> Vec<String> {
    let mut command = vec![CLAUDE.to_owned()];
    if let Some(skill) = skill(prompt) {
        command.push("--name".to_owned());
        command.push(match branch {
            Some(branch) => format!("{skill}: {branch}"),
            None => skill.to_owned(),
        });
    }
    command.push(prompt.to_owned());
    command
}

/// Claude Code on `prompt`, let read `file`, where there is one, with no
/// question asked: luneta keeps what a shell printed outside the directory
/// the agent works in, where Claude Code asks first. It asks for a file no
/// rule can name.
fn claude_reading(prompt: String, file: Option<&str>) -> Vec<String> {
    let mut command = vec![CLAUDE.to_owned()];
    if let Some(rule) = file.and_then(read_rule) {
        command.push("--allowedTools".to_owned());
        command.push(rule);
        // The flag takes every word after it as another tool, up to this.
        command.push("--".to_owned());
    }
    command.push(prompt);
    command
}

/// The rule that lets Claude Code read the file by the path `file` from the
/// root, and no other, but for one whose name differs by its case alone, which
/// a rule takes no account of; or none where no rule can name it alone. A rule
/// names a path from the root with another `/` before it, as a glob: `\` makes
/// `*`, `[` or `]` itself, and it takes four to make a `\`, as Claude Code reads
/// the rule, and then the glob. Nothing makes a `?` itself. `--allowedTools`
/// cuts its word at a space or a comma outside parentheses, which any `)`
/// leaves it outside of, and a line is another glob.
fn read_rule(file: &str) -> Option<String> {
    let mut rule = String::from("Read(/");
    let mut inside = true;
    for c in file.chars() {
        match c {
            '?' => return None,
            ' ' | ',' if !inside => return None,
            c if c.is_control() => return None,
            '(' => inside = true,
            ')' => inside = false,
            '*' | '[' | ']' => rule.push('\\'),
            '\\' => rule.push_str(r"\\\"),
            _ => {},
        }
        rule.push(c);
    }
    rule.push(')');
    Some(rule)
}

/// The skill a prompt runs, as `/tigerstyle` or `/plugin:skill`: the command
/// it starts with.
fn skill(prompt: &str) -> Option<&str> {
    let command = prompt.strip_prefix('/')?.split(char::is_whitespace).next()?;
    (!command.is_empty()).then_some(command)
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
    /// The skills that `cwd` can run, for the search box.
    Skills(String),
    /// What the delete key deletes planned, for `planned`: for a pane, once
    /// zellij has said what closing it closes: of `session`.
    Plan { session: Listed, scope: Scope },
    /// zellij to open what `opens` says, in `cwd`, in `session`, for luneta
    /// to take its client there: from its pane `pane`, the menu's row's,
    /// which it takes the client to first, for a tab of a session it is
    /// not in.
    Open { session: Listed, pane: u32, cwd: String, opens: Opens },
    /// zellij to list the tabs of `session` other than that of its pane
    /// `pane`, for `tabs`.
    Tabs { session: Listed, pane: u32 },
    /// zellij to turn the pane `pane` of `session` as `turn` says, for luneta
    /// to take its client there.
    Turn { session: Listed, pane: u32, turn: Turn },
    /// luneta to find where a key of the menu for the pull request, the issue
    /// or the file goes, and to have its worktree made, for `pulled`.
    Pull(Work),
    /// luneta to keep what the shell in the pane `pane` of `session` printed
    /// after the line `ended` was typed on, for `printed`.
    Printed { session: Listed, pane: u32, ended: Ended },
    /// zellij to make the session `session`, that starts in `cwd`, for luneta
    /// to take its client there, and then to open a tab of it in `tab_cwd`,
    /// named `name`, with `command` in it, or your shell for none, in place
    /// of the tabs zellij makes it with, for luneta to take its client there
    /// in turn.
    Make { session: SessionName, cwd: String, tab_cwd: String, name: String, command: Vec<String> },
}

/// What a menu that closed leaves the sidebar to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Nothing,
    /// Say why what a key asked went wrong.
    Wrong(String),
    /// Take luneta's zellij to a session it makes, on the machine, that
    /// starts in `cwd`.
    Start(Machine, String),
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
    /// Have lunetad watch the machine ssh reaches by this destination.
    Ssh(Destination),
    /// Have lunetad stop watching this machine.
    Forget(Destination),
    /// Move the review of the agent `pid`, on the machine.
    Review(Machine, u32, Move),
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
    /// The menu for `work`, which a paste opened, at `step`. Once luneta has
    /// found where its keys go, its target is there.
    Pull { work: Work, step: Step },
    /// The box `n m` opens, with what is typed in it for ssh to reach a
    /// machine by, and why that is no destination, once `Enter` found it is
    /// not, until the next key.
    Ssh { keys: Keys, typed: String, wrong: Option<String> },
}

/// Where a menu waits for zellij to open what a key asked for: in a session
/// lunetad lists, or in one zellij makes for it, by its name; or to turn what
/// a key of the Turn group asked for, in a session lunetad lists.
#[derive(Debug, PartialEq, Eq)]
pub enum Opening<'a> {
    In(&'a Listed),
    Made(&'a SessionName),
    Turned(&'a Listed),
}

/// How far the menu for a pull request, an issue or a file is, with its keys,
/// `w` and `r`, or `w` and `t`.
enum Step {
    /// Its keys wait for one.
    Choosing,
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
            Wait::Pulling(_) => format!("Making a worktree of {}…", work.tab()),
            Wait::Opening(_) => format!("Opening {}…", work.tab()),
            Wait::Making(session) => format!("Making session {session}…"),
        }
    }
}

impl Menu {
    /// The menu numbered `number` for `target`, on the group `key` opens, or
    /// on the first with no key, and what a key that does more than open a
    /// group leaves the sidebar to do. None, and no menu, for a key the menu
    /// has not.
    pub fn open(number: u64, target: Target, key: Option<char>) -> Option<Did> {
        let keys = Keys::new(target.removes(), target.reviewing());
        if key.is_some_and(|key| keys.root.iter().all(|entry| entry.key() != key)) {
            return None;
        }
        let menu = Menu { number, target, stage: Stage::Choosing(keys) };
        Some(match key {
            Some(key) => menu.key(&KeyEvent::from(KeyCode::Char(key))),
            None => Did::Stays(menu, Wants::Nothing),
        })
    }

    /// The menu numbered `number` for `target`, on its first group.
    pub fn new(number: u64, target: Target) -> Menu {
        let keys = Keys::new(target.removes(), target.reviewing());
        Menu { number, target, stage: Stage::Choosing(keys) }
    }

    /// The menu numbered `number` for `work`, pasted with the menu for
    /// `target` open, or the row highlighted.
    pub fn pull(number: u64, target: Target, work: Work) -> Menu {
        Menu { number, target, stage: Stage::Pull { work, step: Step::Choosing } }
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
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Pull { .. }
            | Stage::Ssh { .. } => None,
        }
    }

    /// Where the menu waits for zellij to open what a key asked for, or to
    /// turn it, while it does.
    pub fn opening(&self) -> Option<Opening<'_>> {
        match &self.stage {
            Stage::Opening { session, .. }
            | Stage::Pull { step: Step::Waiting(Wait::Opening(session)), .. } => {
                Some(Opening::In(session))
            },
            Stage::Pull { step: Step::Waiting(Wait::Making(session)), .. } => {
                Some(Opening::Made(session))
            },
            Stage::Turning { session, .. } => Some(Opening::Turned(session)),
            Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Reading { .. }
            | Stage::Pull { .. }
            | Stage::Ssh { .. } => None,
        }
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

    /// Whether a key of the menu for a pull request, an issue or a file waits
    /// for luneta to find where it goes.
    pub fn pulling(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Waiting(Wait::Pulling(_)), .. })
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
    /// but for a session, which leaves the menu done.
    pub fn key(self, key: &KeyEvent) -> Did {
        let Menu { number, target, stage } = self;
        let stays = |stage: Stage, wants: Wants| {
            Did::Stays(Menu { number, target: target.clone(), stage }, wants)
        };
        match stage {
            Stage::Choosing(mut keys) => match keys.key(key) {
                Pressed::Close => Did::Closed(Done::Nothing),
                Pressed::Moved | Pressed::Nothing => stays(Stage::Choosing(keys), Wants::Nothing),
                Pressed::Search => match target.beside() {
                    Ok(Beside { cwd, .. }) => {
                        keys.search = Some(Search::new());
                        stays(Stage::Choosing(keys), Wants::Skills(cwd))
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                Pressed::Start(prompt) => match target.beside() {
                    Ok(Beside { session, pane, cwd }) => {
                        let command = claude(&prompt, target.branch());
                        let opens = Opens::Pane { beside: pane, command };
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                Pressed::Last(last) => match target.ended() {
                    Ok((beside, ended)) => {
                        let (session, pane) = (beside.session.clone(), beside.pane);
                        let wants = Wants::Printed { session, pane, ended: ended.clone() };
                        stays(Stage::Reading { keys, last, beside, ended }, wants)
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                // A machine is added from any row: it needs no pane.
                Pressed::New(new) => match (new, target.beside()) {
                    (New::Machine, _) => {
                        let stage = Stage::Ssh { keys, typed: String::new(), wrong: None };
                        stays(stage, Wants::Nothing)
                    },
                    (_, Err(reason)) => Did::Closed(Done::Wrong(reason)),
                    (New::Pane, Ok(Beside { session, pane, cwd })) => {
                        let opens = Opens::Pane { beside: pane, command: Vec::new() };
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    (New::Tab, Ok(Beside { session, pane, cwd })) => {
                        let opens = Opens::Tab { name: None, command: Vec::new() };
                        let stage = Stage::Opening { keys, session: session.clone() };
                        stays(stage, Wants::Open { session, pane, cwd, opens })
                    },
                    (New::Session, Ok(Beside { cwd, .. })) => {
                        Did::Closed(Done::Start(target.machine().clone(), cwd))
                    },
                },
                Pressed::Turn(turning) => match target.turned() {
                    Ok(row) => {
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
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                Pressed::Delete { key, session, scope } => {
                    let wants = Wants::Plan { session: session.clone(), scope };
                    let delete = Delete { key, session, scope };
                    stays(Stage::Planning { keys, delete, agreed: None }, wants)
                },
                Pressed::Forget(Machine::Remote(destination)) => {
                    Did::Closed(Done::Forget(destination))
                },
                Pressed::Forget(Machine::Local) => Did::Closed(Done::Wrong(LOCAL_STAYS.to_owned())),
                Pressed::Review(does) => match target.reviewing() {
                    Some(Reviewing { pid, .. }) => {
                        Did::Closed(Done::Review(target.machine().clone(), *pid, does))
                    },
                    None => Did::Closed(Done::Nothing),
                },
                Pressed::Brief(scope) => Did::Closed(Done::Brief(target.brief(scope))),
                Pressed::Ask => Did::Closed(Done::Ask(target.clone())),
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
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Picking { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
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
            Stage::Pull { step: Step::Choosing, .. }
                if key.code == KeyCode::Backspace && bare(key) =>
            {
                Did::Closed(Done::Nothing)
            },
            Stage::Pull { work, step: Step::Choosing } => {
                match Pull::pressed(key, Asks::of(&work)) {
                    Some(pull) => {
                        let wants = Wants::Pull(work.clone());
                        let step = Step::Waiting(Wait::Pulling(pull));
                        stays(Stage::Pull { work, step }, wants)
                    },
                    None => stays(Stage::Pull { work, step: Step::Choosing }, Wants::Nothing),
                }
            },
            stage @ (Stage::Planning { .. }
            | Stage::Opening { .. }
            | Stage::Listing { .. }
            | Stage::Turning { .. }
            | Stage::Reading { .. }
            | Stage::Pull { .. }) => stays(stage, Wants::Nothing),
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

    /// What the key of the menu for a pull request, an issue or a file that
    /// waits does, now that luneta found where it goes, as `ready` says, or
    /// why it found nothing: on the machine of the row it was pasted on, where
    /// luneta found it. `w` takes luneta's zellij to the pane that has the
    /// branch checked out, or opens a tab in the worktree, named for what was
    /// pasted, in the session that works in its repository, or one luneta
    /// makes; `r` opens an agent on a review of the pull request beside that
    /// pane, or in that tab, and `t` one on tackling the issue or the file,
    /// let read the file where it is. The menu is then for where it opens
    /// that, and waits for zellij to open it.
    pub fn pulled(self, ready: Result<Ready, String>) -> Did {
        let Menu { number, target, stage } = self;
        let on = target.machine().clone();
        let Stage::Pull { work, step: Step::Waiting(Wait::Pulling(pull)) } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let Ready { at, worktree, branch, work: readied } = match ready {
            Ok(ready) => ready,
            Err(why) => return Did::Closed(Done::Wrong(format!("menu: {work}: {why}"))),
        };
        let command = match (pull, &readied) {
            (Pull::Worktree, _) => Vec::new(),
            (Pull::Agent, Readied::Link(link)) if link.kind() == Kind::Pull => {
                reviews::command(link, branch.as_deref())
            },
            (Pull::Agent, Readied::Link(link)) => {
                claude(&format!("{TACKLE} {}", link.url()), branch.as_deref())
            },
            (Pull::Agent, Readied::File { real }) => {
                claude_reading(format!("{TACKLE} {real}"), Some(real))
            },
        };
        let opening = |session: Listed, pane: u32, opens: Opens| {
            let (cwd, branch, ran) = (Some(worktree.clone()), branch.clone(), Ran::Unhooked);
            let reviewing = None;
            let row = PaneRow { session: session.clone(), pane, cwd, branch, ran, reviewing };
            let target = Target::Pane(on.clone(), row);
            let step = Step::Waiting(Wait::Opening(session.clone()));
            let stage = Stage::Pull { work: work.clone(), step };
            let cwd = worktree.clone();
            Did::Stays(Menu { number, target, stage }, Wants::Open { session, pane, cwd, opens })
        };
        match (at, pull) {
            (At::There((session, pane)), Pull::Worktree) => {
                Did::Closed(Done::Go(on, Place::Pane(PanePlace { session, pane })))
            },
            (At::There((session, pane)), Pull::Agent) => {
                opening(session, pane, Opens::Pane { beside: pane, command })
            },
            (At::Session((session, pane)), _) => {
                let opens = Opens::Tab { name: Some(work.tab()), command };
                opening(session, pane, opens)
            },
            (At::Clone { cwd, name }, _) => {
                let (session, tab_cwd, tab) = (name.clone(), worktree, work.tab());
                let wants = Wants::Make { session, cwd, tab_cwd, name: tab, command };
                let stage = Stage::Pull { work, step: Step::Waiting(Wait::Making(name)) };
                // The session is the machine's, as zellij makes it, and the
                // menu is for that, until it has a pane of it.
                Did::Stays(Menu { number, target: Target::Machine(on), stage }, wants)
            },
        }
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
        let opens = Opens::Pane { beside: pane, command: claude_reading(prompt, printed.file()) };
        let stage = Stage::Opening { keys, session: session.clone() };
        Did::Stays(Menu { number, target, stage }, Wants::Open { session, pane, cwd, opens })
    }

    /// Fills the search box with what the finder said, while it is open.
    pub fn found(&mut self, skills: Result<Vec<crate::skills::Skill>, String>) {
        if let Stage::Choosing(keys) = &mut self.stage
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
    /// A spinner that turns shows `frame`.
    pub fn lines(
        &self,
        width: usize,
        room: usize,
        frame: usize,
    ) -> (Vec<Line<'static>>, Spots<Choice>) {
        self.shown(width, room.saturating_sub(2), frame).lines(width, room)
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
    /// it.
    pub fn turning(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Waiting(_), .. })
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
        let right = self.target.cwd().map(sidebar::short_cwd).unwrap_or_default();
        let mut spots = Spots::default();
        let (title, right, body, (footer, keys)) = match &self.stage {
            Stage::Pull { work, step } => {
                let body = match step {
                    Step::Choosing => {
                        let asks = Asks::of(work);
                        let entries = Pull::ALL.map(|pull| Entry::Pull(pull, asks));
                        spots = entry_spots(&entries, width).map(Choice::Key);
                        entries.iter().map(|entry| entry_line(entry, inner)).collect()
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
                if let Some(search) = &keys.search {
                    let moves =
                        [(Hint::Moves, "Move"), (Hint::Enter, "Start"), (Hint::Esc, "Back")];
                    let (body, skills) = search_lines(search, width, body_room);
                    if choosing {
                        spots = skills;
                    }
                    (SEARCH_TITLE.to_owned(), right, body, keyed(width, &moves))
                } else {
                    let back: &[(Hint, &str)] = if keys.open.is_empty() {
                        &[(Hint::Esc, "Close")]
                    } else {
                        &[(Hint::Backspace, "Back"), (Hint::Esc, "Close")]
                    };
                    if choosing {
                        spots = entry_spots(keys.entries(), width).map(Choice::Key);
                    }
                    let body = keys.entries().iter().map(|entry| entry_line(entry, inner));
                    (title(keys), right, body.collect(), keyed(width, back))
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
    listed(TITLE, &Keys::legend().root, width, room)
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
    let mut lines = Vec::with_capacity(room);
    if room == 0 {
        return (lines, Spots::default());
    }
    lines.push(rule(title, "", width));
    let inner = width.saturating_sub(1);
    lines.extend(entries.iter().map(|entry| entry_line(entry, inner)).take(room - 1));
    let spots = entry_spots(entries, width).first(lines.len() - 1).down(1);
    (lines, spots)
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
    let room = inner.saturating_sub(key.width() + GAP);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(key, Style::new().fg(ACCENT)),
        Span::raw(" ".repeat(GAP)),
    ];
    let does = |does: &str| Span::styled(sidebar::cut(does, room), Style::new().fg(NAME));
    match entry {
        Entry::Group { name, .. } => {
            spans.push(Span::raw(sidebar::cut(name, room.saturating_sub(1 + GROUP.width()))));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(GROUP, Style::new().fg(TAG)));
        },
        Entry::Start { prompt, .. } => spans.push(does(prompt)),
        Entry::Last(last) => spans.push(does(last.does())),
        Entry::Review { says, .. } => spans.push(does(says)),
        Entry::New(new) => spans.push(does(new.does())),
        Entry::Turn(turning) => spans.push(does(turning.does())),
        Entry::Tab { name, .. } => spans.push(does(name)),
        Entry::Close { scope, .. } => spans.push(does(scope.does())),
        Entry::Forget(_) => spans.push(does(FORGET_DOES)),
        Entry::Brief(scope) => spans.push(does(scope.does())),
        Entry::Command(command) => spans.push(does(command.does())),
        Entry::YankPull => spans.push(does(YANK_PULL_DOES)),
        Entry::Pull(pull, asks) => spans.push(does(pull.does(*asks))),
        Entry::Search => spans.push(does(SEARCH_DOES)),
        Entry::Ask => spans.push(does(ASK_DOES)),
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
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use lunetad::Directory;
    use ratatui::layout::Position;

    use super::*;
    use crate::close::{self, Deletes, Doing, Seat, Subject};
    use crate::printed::Kept;
    use crate::pull::{At, Link, Readied, Ready, Work};
    use crate::sidebar::Listed;
    use crate::skills::Skill;
    use crate::spots::Hit;
    use crate::zellij::Closes;

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
            ran: Ran::Unhooked,
            reviewing: None,
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
        Wants::Open { session: listed(), pane: 4, cwd: "/w/luneta/main".to_owned(), opens }
    }

    fn opened(target: Target, key: Option<char>) -> Did {
        Menu::open(1, target, key).expect("a key of the menu")
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
                " m  new machine",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('p')));
        let cwd = "/w/luneta/main".to_owned();
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
        assert_eq!(closed(menu.key(&char('s'))), Done::Start(Machine::Local, cwd));
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
        assert_eq!(menu.opening(), Some(Opening::Turned(&listed())));
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
        assert_eq!(menu.opening(), Some(Opening::Turned(&listed())));

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
        assert_eq!(menu.opening(), Some(Opening::Turned(&listed())));

        let (menu, _) = stays(listing().tabs(Ok(vec![other_tab(7, "a"), other_tab(2, "b")])));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(menu.lines(31, 9, 0).0[1].to_string(), " t  pane into tab");
        let (menu, _) = stays(listing().tabs(Ok(vec![other_tab(7, "a"), other_tab(2, "b")])));
        assert_eq!(closed(menu.key(&esc())), Done::Nothing);
    }

    /// A session's row and a machine's header have no pane to turn, and say
    /// so as the key is pressed.
    #[test]
    fn a_key_of_the_turn_group_needs_a_pane_s_row() {
        for (target, why) in [
            (session_row(), NO_ROW_TURN),
            (Target::Machine(Machine::Local), NO_MACHINE_TURN),
        ] {
            for key in ['t', 'p'] {
                let (menu, _) = stays(opened(target.clone(), Some(TURN)));
                assert_eq!(closed(menu.key(&char(key))), Done::Wrong(why.to_owned()));
            }
        }
    }

    #[test]
    fn the_first_group_has_luneta_s_keys_and_backspace_goes_back_to_it() {
        let (menu, _) = stays(opened(pane_row(), None));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ luneta ──────── luneta/main ─",
                " /  Search skills",
                " a  Ask",
                " c  Claude ▸",
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
        assert!(Menu::open(1, pane_row(), Some('x')).is_none());
    }

    #[test]
    fn the_legend_lists_the_sidebar_s_commands_then_the_menu_s_groups() {
        assert_eq!(
            text(&legend(31, 12).0),
            [
                "─ luneta ──────────────────────",
                " /  Search",
                " u  Unread",
                " v  Reorder",
                " o  Open PR",
                " y  Yank ▸",
                " a  Ask",
                " c  Claude ▸",
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
        assert_eq!(Top::of('y'), Some(Top::Yank));
        assert_eq!(Top::of('a'), None);
        assert_eq!(Top::of('c'), None);
        assert_eq!(Top::of('n'), None);
        assert_eq!(Top::of('t'), None);
        assert_eq!(Top::of('d'), None);
        assert_eq!(Top::of('b'), None);
    }

    /// `c` opens the Claude group, and a key in it starts an agent on its
    /// skill beside the row's pane, in its directory, named for the skill
    /// and the branch, as the search's `Enter` does.
    #[test]
    fn a_key_of_the_claude_group_starts_an_agent_on_its_skill() {
        let (menu, wants) = stays(opened(pane_row(), Some('c')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Claude ──────── luneta/main ─",
                " t  /thermo-nuclear-review",
                " m  /make-illegal-states-unrep…",
                " s  /tigerstyle",
                " f  fix the last command",
                " e  explain the last command",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('t')));
        let named = "thermo-nuclear-review: feat/menu";
        let command = ["claude", "--name", named, "/thermo-nuclear-review"].map(str::to_owned);
        let opens = Opens::Pane { beside: 4, command: command.to_vec() };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
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

    /// Only a shell at its prompt, after a command its hook said the end of,
    /// has a last command to start an agent on. The menu says why it starts
    /// none on another.
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
        assert_eq!(
            refused(ran(Ran::Unknown)),
            "menu: what pane 4's shell ran is in words this luneta cannot read, as a newer \
             lunetad's"
        );
        assert_eq!(refused(session_row()), NO_ROW_PANE);
    }

    /// `a` closes the menu for the sidebar to open the ask box, for the row
    /// the menu was for, a session's row too: at the top of the sidebar, and
    /// in the menu's first group.
    #[test]
    fn a_opens_the_ask_box_for_the_row_the_menu_is_for() {
        for row in [pane_row(), session_row()] {
            let did = Menu::open(1, row.clone(), Some('a')).expect("the menu has `a`");
            assert_eq!(closed(did), Done::Ask(row.clone()));
            let (menu, _) = stays(opened(row.clone(), None));
            assert_eq!(closed(menu.key(&char('a'))), Done::Ask(row));
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
        let cwd = "/w/luneta/main".to_owned();
        assert_eq!(closed(menu.key(&char('s'))), Done::Start(vps(), cwd));

        let (menu, _) = stays(opened(pane.clone(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan { session: listed(), scope: Scope::Pane(4) });
        let done = closed(menu.planned(plan(Closes::Pane, Vec::new())));
        let name = || listed().name().clone();
        let act = Act::Pane { session: name(), started: listed().started(), pane: 4 };
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

    /// On a machine's header, the Delete group forgets the machine, and on
    /// this one's says it cannot; the Brief group briefs you on every
    /// session of every machine, or on the machine's, from any header; and a
    /// key that opens beside a pane says the header has none.
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
        let (menu, _) = stays(opened(local_header(), Some('d')));
        assert_eq!(closed(menu.key(&char('d'))), Done::Wrong(LOCAL_STAYS.to_owned()));

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
        let (menu, _) = stays(opened(remote_header(), Some('n')));
        assert_eq!(closed(menu.key(&char('p'))), Done::Wrong(NO_MACHINE_PANE.to_owned()));
    }

    #[test]
    fn a_menu_that_cannot_open_beside_its_row_says_why() {
        let (menu, _) = stays(opened(session_row(), Some('n')));
        assert_eq!(closed(menu.key(&char('p'))), Done::Wrong(NO_ROW_PANE.to_owned()));
        let unknown = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        let (menu, _) = stays(opened(unknown, None));
        let done = closed(menu.key(&char('/')));
        assert!(matches!(done, Done::Wrong(why) if why.contains("where pane 4 works")));
    }

    #[test]
    fn a_skill_names_the_agent_for_it_and_its_branch() {
        assert_eq!(
            claude("/tigerstyle", Some("feat/menu")),
            ["claude", "--name", "tigerstyle: feat/menu", "/tigerstyle"]
        );
        assert_eq!(
            claude("/code-review since main", None),
            ["claude", "--name", "code-review", "/code-review since main"]
        );
        assert_eq!(claude("fix the fold", None), ["claude", "fix the fold"]);
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
        assert_eq!(wants, Wants::Skills("/w/luneta/main".to_owned()));
        assert_eq!(
            text(&menu.lines(31, 6, 0).0)[..3],
            ["─ Skills ──────── luneta/main ─", " _", " finding skills…"]
        );
        menu.found(Ok(skills(&["code-review", "tigerstyle"])));
        // The one the cursor is on is lit, as the sidebar's row is while it
        // has the focus, which it has while the search is open.
        let lit: Vec<_> = menu.lines(31, 6, 0).0[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [Some(LIT), None]);
        let (menu, _) = stays(menu.key(&char('t')));
        let (menu, _) = stays(menu.key(&char('i')));
        assert_eq!(text(&menu.lines(31, 6, 0).0)[..3], [
            "─ Skills ──────── luneta/main ─".to_owned(),
            " ti_".to_owned(),
            " /tigerstyle  tigerstyle it".to_owned(),
        ]);
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = claude("/tigerstyle", Some("feat/menu"));
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
    }

    /// Each skill the search box shows is a spot on its line that picks it:
    /// a click starts an agent on it, as `Enter` does with the cursor on it.
    /// The wheel over them moves the cursor. What is typed is no spot.
    #[test]
    fn a_click_on_a_skill_starts_an_agent_on_it_as_enter_does() {
        let (mut menu, _) = stays(opened(pane_row(), Some('/')));
        menu.found(Ok(skills(&["code-review", "tigerstyle"])));
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
        let command = claude("/code-review", Some("feat/menu"));
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
        let (session, started) = (listed().name().clone(), listed().started());
        let act = Act::Pane { session, started, pane: 4 };
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
        let tab = || Closes::Tab { id: 2, panes: vec![4, 5] };
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
        let (session, started) = (listed().name().clone(), listed().started());
        let act = Act::Tab { session, started, tab: 2, panes: vec![4, 5] };
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
        let started = session.started();
        let leave = Act::Leave { session: listed().name().clone(), started, to: elo };
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
        let menu = Menu::new(1, pane_row());
        let boxed = |inner: &str| format!("{}│{inner:<24}│", "z".repeat(24));
        assert_eq!(floated(drawing(&menu), 50, 11), [
            "z".repeat(50),
            format!("{}┌─ luneta ─ luneta/main ─┐", "z".repeat(24)),
            boxed(" /  Search skills"),
            boxed(" a  Ask"),
            boxed(" c  Claude ▸"),
            boxed(" n  New ▸"),
            boxed(" t  Turn ▸"),
            boxed(" d  Delete ▸"),
            boxed(" b  Brief ▸"),
            boxed(" <ESC> - Close"),
            format!("{}└{}┘", "z".repeat(24), "─".repeat(24)),
        ]);

        let (menu, _) = stays(menu.key(&char('c')));
        let zellij = "z".repeat(7);
        let rule = format!("─ Claude {} luneta/main ─", "─".repeat(18));
        assert_eq!(floated(drawing(&menu), 50, 9)[1..], [
            format!("{zellij}┌{rule}┐"),
            format!("{zellij}│{:<41}│", " t  /thermo-nuclear-review"),
            format!("{zellij}│{:<41}│", " m  /make-illegal-states-unrepresentable"),
            format!("{zellij}│{:<41}│", " s  /tigerstyle"),
            format!("{zellij}│{:<41}│", " f  fix the last command"),
            format!("{zellij}│{:<41}│", " e  explain the last command"),
            format!("{zellij}│{:<41}│", " <BS> - Back, <ESC> - Close"),
            format!("{zellij}└{}┘", "─".repeat(41)),
        ]);
        // Narrower than the keys take, the box is as wide as zellij's side.
        let narrow = floated(drawing(&menu), 30, 9);
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

    /// A row of an agent on a review, the skill `next` still to go.
    fn reviewing(next: &str, held: bool) -> Target {
        let next = next.to_owned();
        let reviewing = Some(Reviewing { pid: 33, next, held });
        Target::Pane(Machine::Local, PaneRow { reviewing, ..row() })
    }

    /// The row of an agent on a review has a Review group, whose keys say
    /// what they do to it, and move that one; a key `h` holds it, or goes on
    /// from a hold. A row with no review to move has no such group.
    #[test]
    fn the_review_group_moves_the_review_of_the_row_s_agent() {
        let lines = |menu: &Menu| text(&menu.lines(31, 12, 0).0);
        let (menu, wants) = stays(opened(reviewing("/tigerstyle", false), Some('r')));
        assert_eq!(wants, Wants::Nothing);
        let shown = lines(&menu);
        assert!(shown.contains(&" h  hold".to_owned()), "{shown:?}");
        assert!(shown.contains(&" n  send /tigerstyle".to_owned()), "{shown:?}");
        assert!(shown.contains(&" s  skip /tigerstyle".to_owned()), "{shown:?}");
        let pressed = |held, key| {
            let (menu, _) = stays(opened(reviewing("/tigerstyle", held), Some('r')));
            closed(menu.key(&char(key)))
        };
        let moved = |does| Done::Review(Machine::Local, 33, does);
        assert_eq!(pressed(false, 'h'), moved(Move::Hold));
        assert_eq!(pressed(true, 'h'), moved(Move::Resume));
        let skill = || "/tigerstyle".to_owned();
        assert_eq!(pressed(false, 'n'), moved(Move::Next(skill())));
        assert_eq!(pressed(false, 's'), moved(Move::Skip(skill())));
        let (held, _) = stays(opened(reviewing("/tigerstyle", true), Some('r')));
        assert!(lines(&held).contains(&" h  go on".to_owned()));

        assert!(Menu::open(1, pane_row(), Some('r')).is_none());
        let first = lines(&Menu::new(1, reviewing("/tigerstyle", false)));
        assert!(first.iter().any(|line| line.contains("Review ▸")), "{first:?}");
        let first = lines(&Menu::new(1, pane_row()));
        assert!(!first.iter().any(|line| line.contains("Review")), "{first:?}");
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
        let menu = Menu::new(1, pane_row());
        assert_eq!(clicked(&menu, "Claude ▸"), Some(pressed(KeyCode::Char('c'))));
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
        let (menu, wants) = stays(Menu::pull(1, row, link()).key(&char(key)));
        assert_eq!(wants, Wants::Pull(link()));
        assert!(menu.pulling());
        menu
    }

    /// The same, pasted on a pane's row of this machine.
    fn pulling(key: char) -> Menu {
        pulling_on(pane_row(), key)
    }

    /// A paste opens the menu for the pull request, named for it, with its
    /// repository where another menu says where its pane works. A key waits
    /// for luneta to find where it goes and to make the worktree, and says
    /// so after a spinner that turns, and does nothing else meanwhile, but
    /// close the menu.
    #[test]
    fn the_menu_for_a_pull_request_has_a_key_for_a_worktree_and_one_for_a_review() {
        let menu = Menu::pull(1, pane_row(), link());
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ #2736 ──────── cashubtc/cdk ─",
                " w  worktree",
                " r  review",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
        assert!(!menu.turning());
        let (menu, wants) = stays(menu.key(&char('w')));
        assert_eq!(wants, Wants::Pull(link()));
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
            Ok(Ready { at, worktree, branch, work: Readied::Link(pull_request()) })
        };
        let review = reviews::command(&pull_request(), Some("feat/x"));
        let session = || At::Session((other.clone(), 7));
        for (key, command) in [('w', Vec::new()), ('r', review.clone())] {
            let (menu, wants) = stays(pulling(key).pulled(ready(session())));
            let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
            let cwd = worktree.clone();
            assert_eq!(wants, Wants::Open { session: other.clone(), pane: 7, cwd, opens });
            assert_eq!(menu.opening(), Some(Opening::In(&other)));
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
        let cwd = worktree.clone();
        assert_eq!(wants, Wants::Open { session: other.clone(), pane: 9, cwd, opens });
        let Target::Pane(_, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((&row.session, row.pane), (&other, 9));
    }

    /// A paste of an issue's link opens the menu for the issue, named for it,
    /// with `t` where the menu for a pull request has `r`. `t` opens a tab in
    /// the worktree of the issue's branch, named for it, with an agent told in
    /// words to tackle it, which no skill names.
    #[test]
    fn the_menu_for_an_issue_has_a_key_for_a_worktree_and_one_to_tackle_it() {
        let link = Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link");
        let issue = Work::Link(link.clone());
        let menu = Menu::pull(1, pane_row(), issue.clone());
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            ["─ #2736 ──────── cashubtc/cdk ─", " w  worktree", " t  tackle", " <ESC> - Close"]
        );
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Pull(issue));
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let worktree = "/w/cdk/issue-2736".to_owned();
        let ready = Ready {
            at: At::Session((other.clone(), 7)),
            worktree: worktree.clone(),
            branch: Some("issue-2736".to_owned()),
            work: Readied::Link(link),
        };
        let (menu, wants) = stays(menu.pulled(Ok(ready)));
        let tackle = ["claude", "Tackle https://github.com/cashubtc/cdk/issues/2736"];
        let command = tackle.map(str::to_owned).to_vec();
        let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
        assert_eq!(wants, Wants::Open { session: other, pane: 7, cwd: worktree, opens });
        assert!(menu.opening().is_some());
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
        let menu = Menu::pull(1, pane_row(), file());
        assert_eq!(
            text(&menu.lines(38, 9, 0).0),
            [
                "─ sync-rework ─ plans/sync-rework.md ─",
                " w  worktree",
                " t  tackle",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('t')));
        assert_eq!(wants, Wants::Pull(file()));
        assert_eq!(text(&menu.lines(38, 9, 0).0)[1], " ⠋ Making a worktree of sync-rework…");
        let other = Listed::new("cdk", SystemTime::UNIX_EPOCH + Duration::from_secs(2));
        let (worktree, real) = ("/w/cdk/md-sync-rework", "/w/cdk/main/docs/plans/sync-rework.md");
        let ready = || Ready {
            at: At::Session((other.clone(), 7)),
            worktree: worktree.to_owned(),
            branch: Some("md-sync-rework".to_owned()),
            work: Readied::File { real: real.to_owned() },
        };
        let (_, wants) = stays(menu.pulled(Ok(ready())));
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
        let cwd = worktree.to_owned();
        assert_eq!(wants, Wants::Open { session, pane: 7, cwd, opens });

        let pressed = |key: char| stays(Menu::pull(1, pane_row(), file()).key(&char(key))).0;
        let (_, wants) = stays(pressed('w').pulled(Ok(ready())));
        let opens = Opens::Tab { name: Some("sync-rework".to_owned()), command: Vec::new() };
        let session = other.clone();
        let cwd = worktree.to_owned();
        assert_eq!(wants, Wants::Open { session, pane: 7, cwd, opens });

        assert_eq!(
            closed(pressed('w').pulled(Err("no such file".to_owned()))),
            Done::Wrong("menu: docs/plans/sync-rework.md: no such file".to_owned())
        );
    }

    /// The agent is let read the file by a rule that names it alone, as a glob
    /// would name others, whatever the path holds, and told of it as it is. A
    /// path no rule can name gets none, and Claude Code asks first.
    #[test]
    fn an_agent_is_let_read_the_file_by_a_rule_that_names_it_alone() {
        for (file, rule) in [
            ("/w/plan.md", "Read(//w/plan.md)"),
            ("/w/My Plan (v2).md", "Read(//w/My Plan (v2).md)"),
            ("/w/a, b.md", "Read(//w/a, b.md)"),
            ("/w/b{a,c}.md", "Read(//w/b{a,c}.md)"),
            ("/w/[draft] a*.md", r"Read(//w/\[draft\] a\*.md)"),
            (r"/w/back\slash.md", r"Read(//w/back\\\\slash.md)"),
        ] {
            let prompt = format!("Tackle {file}");
            let command = ["claude", "--allowedTools", rule, "--", prompt.as_str()];
            assert_eq!(
                claude_reading(prompt.clone(), Some(file)),
                command.map(str::to_owned).to_vec(),
                "{file}"
            );
        }
        for file in [
            "/w/what?.md",
            "/w/Plan (v2) final.md",
            "/w/Plan (v2), final.md",
            "/w/two\nlines.md",
        ] {
            let prompt = format!("Tackle {file}");
            assert_eq!(
                claude_reading(prompt.clone(), Some(file)),
                ["claude".to_owned(), prompt],
                "{file:?}"
            );
        }
    }

    /// Where no session works in the repository, luneta makes one in its
    /// clone, and the menu waits for zellij to make it and open the tab, and
    /// says so.
    #[test]
    fn a_session_luneta_makes_for_a_pull_request_opens_its_tab_once_there() {
        let name = SessionName::parse("cdk").unwrap();
        let at = At::Clone { cwd: "/w/cdk".to_owned(), name: name.clone() };
        let work = Readied::Link(pull_request());
        let ready = Ready { at, worktree: "/w/cdk/feat-x".to_owned(), branch: None, work };
        let (menu, wants) = stays(pulling('w').pulled(Ok(ready)));
        assert_eq!(
            wants,
            Wants::Make {
                session: name.clone(),
                cwd: "/w/cdk".to_owned(),
                tab_cwd: "/w/cdk/feat-x".to_owned(),
                name: "#2736".to_owned(),
                command: Vec::new(),
            }
        );
        assert_eq!(menu.opening(), Some(Opening::Made(&name)));
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
            Ok(Ready { at, worktree: "/w/cdk/feat-x".to_owned(), branch: None, work })
        };
        let row = || Target::Pane(vps(), row());
        let place = Place::Pane(PanePlace { session: other.clone(), pane: 9 });
        let there = ready(At::There((other.clone(), 9)));
        assert_eq!(closed(pulling_on(row(), 'w').pulled(there)), Done::Go(vps(), place));
        let (menu, _) = stays(pulling_on(row(), 'w').pulled(ready(At::Session((other, 7)))));
        let Target::Pane(machine, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((machine, row.pane), (&vps(), 7));
        let name = SessionName::parse("cdk").unwrap();
        let clone = At::Clone { cwd: "/w/cdk".to_owned(), name };
        let header = Target::Machine(vps());
        let (menu, wants) = stays(pulling_on(header.clone(), 'w').pulled(ready(clone)));
        assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
        assert_eq!(menu.target(), &header);
    }
}
