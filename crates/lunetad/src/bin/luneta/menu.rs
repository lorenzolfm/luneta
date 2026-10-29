//! The menu: a key, then another, and a pane opens where you work, as
//! which-key shows the keys in neovim. It opens at the bottom of the sidebar,
//! for the row highlighted then, or floats in a box at the bottom right of
//! zellij's side, for the pane zellij is on.
//!
//! Its keys are luneta's own. `/` searches every skill the directory of the
//! row's pane can run, for an agent to start on the one you pick, beside that
//! pane. `a` opens the ask box, for an agent to do what you say in words, as
//! the menu would. `c` opens the Claude group, a key for each of a few skills, which
//! starts an agent on it there. `n` opens the New group: your shell beside the
//! pane, a tab of its session, or a session, in the directory it works in. `d`
//! opens the Delete group, which closes the pane or deletes its session, and
//! asks first when that would end an agent at work. `b` opens the Brief group,
//! which briefs you on every session, the row's session, or the agent in its
//! pane. The plugin's menu reads its keys for skills from its configuration;
//! luneta has none, and the Claude group's are its own.
//!
//! A pull request's link pasted with the sidebar's keys, or in the menu,
//! opens it for that pull request instead: `w` makes a worktree of it, in a
//! tab of the session that works in its repository, and `r` starts an agent
//! on a review of it there too. Where a pane has its branch checked out
//! already, `w` takes you there, and `r` starts the agent beside it.
//!
//! At the top of the sidebar, a key the sidebar has no use of its own for
//! opens the menu on the group it opens, and `m` opens it on its first group,
//! where every key is the menu's. While the sidebar has the focus and no menu
//! is open, the legend at its bottom lists the keys it takes there.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::brief;
use crate::close::{Act, Plan, Scope};
use crate::gruvbox;
use crate::pull::{At, Link, Ready};
use crate::sidebar::{self, ACCENT, LIT, NAME, Place, TAG};
use crate::skills::{Search, Skills};
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
/// it starts an agent on: the skills the plugin's menu has keys for in the
/// author's configuration. luneta has no configuration to read yours from.
const SKILLS: char = 'c';
const SKILLS_NAME: &str = "Claude";
const SKILL_KEYS: [(char, &str); 3] = [
    ('t', REVIEW),
    ('m', "/make-illegal-states-unrepresentable"),
    ('s', "/tigerstyle"),
];

/// The skill an agent reviews a pull request with: the Claude group's `t`,
/// and the `r` of the menu for a pull request, which gives it the link.
const REVIEW: &str = "/thermo-nuclear-review";

/// The key of luneta's own New group, and its name.
const NEW: char = 'n';
const NEW_NAME: &str = "New";

/// The key of luneta's own Delete group, its name, and its keys: one deletes
/// what the menu is for, the other its session.
const DELETE: char = 'd';
const DELETE_NAME: &str = "Delete";
const DELETE_ROW: char = 'd';
const DELETE_SESSION: char = 's';

/// The key of luneta's own Brief group, and its name.
const BRIEF: char = 'b';
const BRIEF_NAME: &str = "Brief";

/// What the rule over the menu's first group, and over the legend, names
/// them, and the sidebar's first line; and what it names the search box.
pub const TITLE: &str = "luneta";
const SEARCH_TITLE: &str = "Skills";

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

/// What the menu says of a key that starts an agent, or opens a pane, a tab
/// or a session, on a session's row, which the highlight is only on while it
/// lists none of the session's panes: folded, or with none to list.
const NO_ROW_PANE: &str = "menu: a session's row has no pane to open beside, nor a directory to \
                           open in; highlight a pane, unfolding its session first if it is folded";

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
    /// Selects the session of the highlighted row, for `Top::Shift` to move,
    /// and ends the selection.
    Reorder,
}

impl Command {
    /// Every command, in the order the legend lists them.
    const ALL: [Command; 3] = [Command::Search, Command::Unread, Command::Reorder];

    const fn key(self) -> char {
        match self {
            Command::Search => '/',
            Command::Unread => 'u',
            Command::Reorder => 'v',
        }
    }

    /// What the legend says the command does.
    const fn does(self) -> &'static str {
        match self {
            Command::Search => "Search",
            Command::Unread => "Unread",
            Command::Reorder => "Reorder",
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
    /// Moves the sessions a selection holds so many places down, or up.
    /// Without a selection, nothing.
    Shift(isize),
    /// `Some(true)` folds, `Some(false)` unfolds, and `None` turns it over.
    Fold(Option<bool>),
    /// Opens the menu on its first group, where every key is the menu's.
    Menu,
    /// Runs a command of the sidebar's own.
    Command(Command),
}

impl Top {
    /// What `key` does at the top of the sidebar besides a command: moves the
    /// highlight or the sessions selected, folds, or opens the menu.
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
            _ => None,
        }
    }

    /// What `key` does at the top of the sidebar. None for a key the menu has
    /// the say on.
    pub fn of(key: char) -> Option<Top> {
        Top::moves(key).or_else(|| Command::of(key).map(Top::Command))
    }
}

/// No command takes the key of the ask box, or of luneta's Claude, New,
/// Delete or Brief group, which would be out of reach at the top of the
/// sidebar, and gone from the legend; nor a key the sidebar moves, folds or
/// opens the menu with, which `Top::of` finds first. Nor does the sidebar
/// move with the ask box's.
const _: () = {
    let mut at = 0;
    while at < Command::ALL.len() {
        let key = Command::ALL[at].key();
        let group = key == ASK || key == SKILLS || key == NEW || key == DELETE || key == BRIEF;
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
}

impl New {
    /// Each, in the order the New group shows them.
    const ALL: [New; 3] = [New::Pane, New::Tab, New::Session];

    const fn key(self) -> char {
        match self {
            New::Pane => 'p',
            New::Tab => 't',
            New::Session => 's',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            New::Pane => "new pane",
            New::Tab => "new tab",
            New::Session => "new session",
        }
    }
}

/// What a key of the menu for a pull request does, in a tab of the session
/// that works in its repository, or where its branch is checked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    /// A worktree of the pull request, as `wt add` makes it.
    Worktree,
    /// The same, with an agent on a review of it.
    Review,
}

impl Pull {
    /// Each, in the order the menu shows them.
    const ALL: [Pull; 2] = [Pull::Worktree, Pull::Review];

    const fn key(self) -> char {
        match self {
            Pull::Worktree => 'w',
            Pull::Review => 'r',
        }
    }

    /// What the menu says the key does.
    const fn does(self) -> &'static str {
        match self {
            Pull::Worktree => "worktree",
            Pull::Review => REVIEW,
        }
    }

    /// The one `key` presses, if any: its key, with no modifier but `Shift`.
    fn pressed(key: &KeyEvent) -> Option<Pull> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        Pull::ALL.into_iter().find(|pull| bare(key) && pull.key() == c)
    }
}

enum Entry {
    /// A key that opens a group, the name the menu shows for it, and the keys
    /// in it.
    Group { key: char, name: &'static str, entries: Vec<Entry> },
    /// A key of the Claude group: starts an agent on the prompt, a skill.
    Start { key: char, prompt: &'static str },
    /// A key of the New group: opens a pane, a tab or a session.
    New(New),
    /// A key of the Delete group: closes the pane the menu is for, or deletes
    /// its session.
    Close { key: char, scope: Scope },
    /// A key of the Brief group: briefs you on what the agents it covers are
    /// doing.
    Brief(brief::Scope),
    /// A command of the sidebar's own, which the legend lists beside the
    /// menu's keys.
    Command(Command),
    /// A key of the menu for a pull request, which that menu shows.
    Pull(Pull),
    /// The key that opens the search over the skills.
    Search,
    /// The key that opens the ask box.
    Ask,
}

impl Entry {
    fn key(&self) -> char {
        match self {
            Entry::Group { key, .. } | Entry::Start { key, .. } | Entry::Close { key, .. } => *key,
            Entry::New(new) => new.key(),
            Entry::Brief(scope) => scope.key(),
            Entry::Command(command) => command.key(),
            Entry::Pull(pull) => pull.key(),
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
    /// Opens a pane, a tab or a session.
    New(New),
    /// Closes the pane the menu is for, or deletes its session, by `key`.
    Delete { key: char, scope: Scope },
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
    /// which opens the ask box, and the Claude, New, Delete and Brief groups: `c t`, `c m` and `c s` start an
    /// agent on a skill, `n p`, `n t` and `n s` open a pane, a tab and a
    /// session, `d d` deletes what the menu is for, as `row` says, and `d s`
    /// deletes its session, and `b z`, `b s` and `b h` brief you on every
    /// session, its session and what the menu is for.
    fn new(row: Scope) -> Self {
        let start = |(key, prompt)| Entry::Start { key, prompt };
        let root = vec![
            Entry::Search,
            Entry::Ask,
            Entry::Group {
                key: SKILLS,
                name: SKILLS_NAME,
                entries: SKILL_KEYS.into_iter().map(start).collect(),
            },
            Entry::Group {
                key: NEW,
                name: NEW_NAME,
                entries: New::ALL.into_iter().map(Entry::New).collect(),
            },
            Entry::Group {
                key: DELETE,
                name: DELETE_NAME,
                entries: vec![
                    Entry::Close { key: DELETE_ROW, scope: row },
                    Entry::Close { key: DELETE_SESSION, scope: Scope::Session },
                ],
            },
            Entry::Group {
                key: BRIEF,
                name: BRIEF_NAME,
                entries: brief::Scope::ALL.into_iter().map(Entry::Brief).collect(),
            },
        ];
        Keys { root, open: Vec::new(), search: None }
    }

    /// The legend: the sidebar's commands, then the keys at the top of the
    /// menu, but for those a command has, or the sidebar moves with, which
    /// `m` reaches. It shows the Delete group's key and not what it deletes,
    /// which is the row's, so any row's will do.
    fn legend() -> Self {
        let mut root: Vec<Entry> = Command::ALL.into_iter().map(Entry::Command).collect();
        let shown = |entry: &Entry| Top::of(entry.key()).is_none();
        root.extend(Keys::new(Scope::Session).root.into_iter().filter(shown));
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
            Entry::New(new) => return Pressed::New(*new),
            Entry::Close { key, scope } => return Pressed::Delete { key: *key, scope: *scope },
            Entry::Brief(scope) => return Pressed::Brief(*scope),
            Entry::Search => return Pressed::Search,
            Entry::Ask => return Pressed::Ask,
            // The legend only shows, and runs nothing, as does the menu for a
            // pull request, which takes its keys itself.
            Entry::Command(_) | Entry::Pull(_) => return Pressed::Nothing,
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

/// A key of the Delete group: the key, which goes ahead pressed again once
/// it asks, and what it deletes.
#[derive(Clone, Copy, Debug)]
struct Delete {
    key: char,
    scope: Scope,
}

/// What the menu is for: the row highlighted as it opened, the directory
/// its pane works in, or its agent while zellij has not said, and the branch
/// checked out there, as the daemon last said. A session's row has neither,
/// having no pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub place: Place,
    pub cwd: Option<String>,
    pub branch: Option<String>,
}

impl Target {
    /// What luneta's Delete group deletes on the row: its pane, or on a
    /// session's row, its session.
    fn deletes(&self) -> Scope {
        self.place.pane().map_or(Scope::Session, Scope::Pane)
    }

    /// The pane a key starts an agent or opens a pane beside, and the
    /// directory it works in, where a tab or a session opens too, or why
    /// there is none: a session's row has no pane, and a pane the daemon has
    /// said no directory of yet, nor of an agent in it, opens nothing, rather
    /// than wherever zellij runs.
    fn beside(&self) -> Result<(u32, String), String> {
        let Some(pane) = self.place.pane() else {
            return Err(NO_ROW_PANE.to_owned());
        };
        let Some(cwd) = self.cwd.clone() else {
            return Err(format!(
                "menu: luneta does not know yet where pane {pane} works; try again in a moment"
            ));
        };
        Ok((pane, cwd))
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
    /// zellij has said what closing it closes.
    Plan(Scope),
    /// zellij to open what `opens` says, in `cwd`, for luneta to take its
    /// client there.
    Open { cwd: String, opens: Opens },
    /// luneta to find where a key of the menu for the pull request goes, and
    /// to have its worktree made, for `pulled`.
    Pull(Link),
    /// zellij to make the session `session`, that starts in `cwd`, for luneta
    /// to take its client there, and then to open a tab of it in `tab_cwd`,
    /// named `name`, with `command` in it, or your shell for none, for luneta
    /// to take its client there in turn.
    Make { session: SessionName, cwd: String, tab_cwd: String, name: String, command: Vec<String> },
}

/// What a menu that closed leaves the sidebar to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Nothing,
    /// Say why what a key asked went wrong.
    Wrong(String),
    /// Take luneta's zellij to a session it makes, that starts in `cwd`.
    Start(String),
    /// Take luneta's zellij to the pane that has the pull request's branch
    /// checked out.
    Go(Place),
    /// Have zellij do what a delete key planned: for the session luneta's
    /// zellij is in, once it has left it.
    Act(Act),
    /// Brief you on what the agents `scope` covers are doing, by `place`, the
    /// row the menu was for.
    Brief { scope: brief::Scope, place: Place },
    /// Open the ask box, for the row the menu was for.
    Ask(Target),
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
    /// zellij opens what a key asked for, for luneta to take its client
    /// there. Keys do nothing but close the menu.
    Opening(Keys),
    /// The menu for the pull request `link`, which a paste opened, at `step`.
    /// Once luneta has found where its keys go, its target is there.
    Pull { link: Link, step: Step },
}

/// How far the menu for a pull request is, with its keys, `w` and `r`.
enum Step {
    /// Its keys wait for one.
    Choosing,
    /// The key `pull` waits for luneta to find where it goes, and for `wt
    /// add` to make the worktree. Keys do nothing but close the menu.
    Pulling(Pull),
    /// zellij opens what the key asked for, for luneta to take its client
    /// there. Keys do nothing but close the menu.
    Opening,
    /// zellij makes this session, and opens a tab of it, for luneta to take
    /// its client there. Keys do nothing but close the menu.
    Making(SessionName),
}

impl Menu {
    /// The menu numbered `number` for `target`, on the group `key` opens, or
    /// on the first with no key, and what a key that does more than open a
    /// group leaves the sidebar to do. None, and no menu, for a key the menu
    /// has not.
    pub fn open(number: u64, target: Target, key: Option<char>) -> Option<Did> {
        let keys = Keys::new(target.deletes());
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
        let keys = Keys::new(target.deletes());
        Menu { number, target, stage: Stage::Choosing(keys) }
    }

    /// The menu numbered `number` for the pull request `link`, pasted with
    /// the menu for `target` open, or the row highlighted.
    pub fn pull(number: u64, target: Target, link: Link) -> Menu {
        Menu { number, target, stage: Stage::Pull { link, step: Step::Choosing } }
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// The pane a delete key waits on, for zellij to say what closing it
    /// closes. A session's is planned at once.
    pub fn planning(&self) -> Option<u32> {
        match &self.stage {
            Stage::Planning { delete: Delete { scope: Scope::Pane(pane), .. }, .. } => Some(*pane),
            Stage::Planning { .. }
            | Stage::Choosing(_)
            | Stage::Asking { .. }
            | Stage::Opening(_)
            | Stage::Pull { .. } => None,
        }
    }

    /// Whether the menu waits for zellij to open what a key asked for.
    pub fn opening(&self) -> bool {
        matches!(
            self.stage,
            Stage::Opening(_) | Stage::Pull { step: Step::Opening | Step::Making(_), .. }
        )
    }

    /// The session zellij makes for what a key asked for, while the menu
    /// waits for it to open a tab of it.
    pub fn making(&self) -> Option<&SessionName> {
        match &self.stage {
            Stage::Pull { step: Step::Making(session), .. } => Some(session),
            _ => None,
        }
    }

    /// Whether a key of the menu for a pull request waits for luneta to find
    /// where it goes.
    pub fn pulling(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Pulling(_), .. })
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
                    Ok((_, cwd)) => {
                        keys.search = Some(Search::new());
                        stays(Stage::Choosing(keys), Wants::Skills(cwd))
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                Pressed::Start(prompt) => match target.beside() {
                    Ok((beside, cwd)) => {
                        let command = claude(&prompt, target.branch.as_deref());
                        let opens = Opens::Pane { beside, command };
                        stays(Stage::Opening(keys), Wants::Open { cwd, opens })
                    },
                    Err(reason) => Did::Closed(Done::Wrong(reason)),
                },
                Pressed::New(new) => match (target.beside(), new) {
                    (Err(reason), _) => Did::Closed(Done::Wrong(reason)),
                    (Ok((beside, cwd)), New::Pane) => {
                        let opens = Opens::Pane { beside, command: Vec::new() };
                        stays(Stage::Opening(keys), Wants::Open { cwd, opens })
                    },
                    (Ok((_, cwd)), New::Tab) => {
                        let opens = Opens::Tab { name: None, command: Vec::new() };
                        stays(Stage::Opening(keys), Wants::Open { cwd, opens })
                    },
                    (Ok((_, cwd)), New::Session) => Did::Closed(Done::Start(cwd)),
                },
                Pressed::Delete { key, scope } => {
                    let delete = Delete { key, scope };
                    stays(Stage::Planning { keys, delete, agreed: None }, Wants::Plan(scope))
                },
                Pressed::Brief(scope) => {
                    Did::Closed(Done::Brief { scope, place: target.place.clone() })
                },
                Pressed::Ask => Did::Closed(Done::Ask(target.clone())),
            },
            Stage::Planning { .. } | Stage::Opening(_) | Stage::Pull { .. } if closes(key) => {
                Did::Closed(Done::Nothing)
            },
            // `Backspace` goes out of the menu from its first group, and the
            // menu for a pull request has no other.
            Stage::Pull { step: Step::Choosing, .. }
                if key.code == KeyCode::Backspace && bare(key) =>
            {
                Did::Closed(Done::Nothing)
            },
            Stage::Pull { link, step: Step::Choosing } => match Pull::pressed(key) {
                Some(pull) => {
                    let wants = Wants::Pull(link.clone());
                    stays(Stage::Pull { link, step: Step::Pulling(pull) }, wants)
                },
                None => stays(Stage::Pull { link, step: Step::Choosing }, Wants::Nothing),
            },
            stage @ (Stage::Planning { .. } | Stage::Opening(_) | Stage::Pull { .. }) => {
                stays(stage, Wants::Nothing)
            },
            Stage::Asking { keys, delete, plan, asks } => match answer(key, delete.key) {
                Answer::Yes => {
                    let planning = Stage::Planning { keys, delete, agreed: Some(plan) };
                    stays(planning, Wants::Plan(delete.scope))
                },
                Answer::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Answer::Close => Did::Closed(Done::Nothing),
                Answer::Nothing => {
                    stays(Stage::Asking { keys, delete, plan, asks }, Wants::Nothing)
                },
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
        Did::Closed(Done::Act(plan.act))
    }

    /// What the key of the menu for a pull request that waits does, now that
    /// luneta found where it goes, as `ready` says, or why it found nothing.
    /// `w` takes luneta's zellij to the pane that has the pull request's
    /// branch checked out, or opens a tab in the worktree, named for the pull
    /// request, in the session that works in its repository, or one luneta
    /// makes; `r` opens an agent on a review of it beside that pane, or in
    /// that tab. The menu is then for where it opens that, and waits for
    /// zellij to open it.
    pub fn pulled(self, ready: Result<Ready, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Pull { link, step: Step::Pulling(pull) } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let Ready { at, worktree, branch } = match ready {
            Ok(ready) => ready,
            Err(why) => return Did::Closed(Done::Wrong(format!("menu: {link}: {why}"))),
        };
        let command = match pull {
            Pull::Worktree => Vec::new(),
            Pull::Review => claude(&format!("{REVIEW} {}", link.url()), branch.as_deref()),
        };
        let opening = |place: Place, opens: Opens| {
            let target = Target { place, cwd: Some(worktree.clone()), branch: branch.clone() };
            let stage = Stage::Pull { link: link.clone(), step: Step::Opening };
            Did::Stays(Menu { number, target, stage }, Wants::Open { cwd: worktree.clone(), opens })
        };
        match (at, pull) {
            (At::There { session, pane }, Pull::Worktree) => {
                Did::Closed(Done::Go(Place::Pane { session, pane }))
            },
            (At::There { session, pane }, Pull::Review) => {
                opening(Place::Pane { session, pane }, Opens::Pane { beside: pane, command })
            },
            (At::Session { session, pane }, _) => {
                let opens = Opens::Tab { name: Some(link.tab()), command };
                opening(Place::Pane { session, pane }, opens)
            },
            (At::Clone { cwd, name }, _) => {
                let (session, tab_cwd, tab) = (name.clone(), worktree, link.tab());
                let wants = Wants::Make { session, cwd, tab_cwd, name: tab, command };
                let stage = Stage::Pull { link, step: Step::Making(name) };
                Did::Stays(Menu { number, target, stage }, wants)
            },
        }
    }

    /// Fills the search box with what the finder said, while it is open.
    pub fn found(&mut self, skills: Result<Vec<crate::skills::Skill>, String>) {
        if let Stage::Choosing(keys) = &mut self.stage
            && let Some(search) = &mut keys.search
        {
            search.fill(skills);
        }
    }

    /// What the menu shows at the bottom of the sidebar, `width` wide, in
    /// `room` lines at most: a rule that names the groups opened and says
    /// where the pane works, what the menu shows under it, and the keys that
    /// go back and close under them. Short of room, what it shows goes first,
    /// then the rule.
    pub fn lines(&self, width: usize, room: usize) -> Vec<Line<'static>> {
        self.shown(width, room.saturating_sub(2)).lines(width, room)
    }

    /// Draws the menu floating at the bottom right of `over`, zellij's side,
    /// as `float` does. The search box takes the most columns, so that it
    /// keeps its width as you type.
    pub fn draw(&self, buffer: &mut Buffer, over: Rect) {
        float(buffer, over, self.searching(), |width, body_room| self.shown(width, body_room));
    }

    /// Whether the search box over the skills is open.
    fn searching(&self) -> bool {
        match &self.stage {
            Stage::Choosing(keys) | Stage::Planning { keys, .. } | Stage::Opening(keys) => {
                keys.search.is_some()
            },
            Stage::Asking { .. } | Stage::Pull { .. } => false,
        }
    }

    /// What the menu shows `width` wide, with `body_room` lines at most for
    /// what it shows under the rule. The menu for a pull request is named for
    /// it, and says its repository where another says where its pane works.
    fn shown(&self, width: usize, body_room: usize) -> Shown {
        let inner = width.saturating_sub(1);
        let right = self.target.cwd.as_deref().map(sidebar::short_cwd).unwrap_or_default();
        let (title, right, body, footer) = match &self.stage {
            Stage::Pull { link, step: Step::Pulling(_) } => {
                let making = format!("Making a worktree of {}…", link.tab());
                let body = wrap(&making, width, Style::new().fg(NAME));
                (link.tab(), link.repository(), body, keys_line(width, &[("<ESC>", "Close")]))
            },
            Stage::Pull { link, step: Step::Choosing | Step::Opening | Step::Making(_) } => {
                let body = Pull::ALL.map(|pull| entry_line(&Entry::Pull(pull), inner));
                let footer = keys_line(width, &[("<ESC>", "Close")]);
                (link.tab(), link.repository(), body.to_vec(), footer)
            },
            Stage::Choosing(keys) | Stage::Planning { keys, .. } | Stage::Opening(keys) => {
                if let Some(search) = &keys.search {
                    let moves = [("<↓↑>", "Move"), ("<ENTER>", "Start"), ("<ESC>", "Back")];
                    let body = search_lines(search, width, body_room);
                    (SEARCH_TITLE.to_owned(), right, body, keys_line(width, &moves))
                } else {
                    let back: &[(&str, &str)] = if keys.open.is_empty() {
                        &[("<ESC>", "Close")]
                    } else {
                        &[("<BS>", "Back"), ("<ESC>", "Close")]
                    };
                    let body = keys.entries().iter().map(|entry| entry_line(entry, inner));
                    (title(keys), right, body.collect(), keys_line(width, back))
                }
            },
            Stage::Asking { keys, asks, .. } => (
                title(keys),
                right,
                wrap(asks, width, Style::new().fg(NAME)),
                keys_line(width, &[("<y>", "Yes"), ("<BS>", "Back"), ("<ESC>", "Close")]),
            ),
        };
        Shown { title, right, body, footer }
    }
}

/// What the menu shows: the title of the rule over it and where its pane
/// works, at the rule's right, the lines under the rule, and the keys that go
/// back and close under them.
struct Shown {
    title: String,
    right: String,
    body: Vec<Line<'static>>,
    footer: Line<'static>,
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
    /// goes first, then the rule.
    fn lines(self, width: usize, room: usize) -> Vec<Line<'static>> {
        let Shown { title, right, body, footer } = self;
        let mut lines = Vec::with_capacity(room);
        if room >= 2 {
            lines.push(rule(&title, &right, width));
        }
        lines.extend(body.into_iter().take(room.saturating_sub(2)));
        if room >= 1 {
            lines.push(footer);
        }
        lines
    }
}

/// Draws what `shown` shows, for a width and the lines its body has room
/// for, floating at the bottom right of `over`, zellij's side: in a box as
/// wide as that takes, or the `widest` the box can be, within `FLOATING`,
/// and as tall as its lines, the rule over them its top.
fn float(buffer: &mut Buffer, over: Rect, widest: bool, shown: impl Fn(usize, usize) -> Shown) {
    let (fewest, most) = FLOATING;
    let most = most.min(usize::from(over.width.saturating_sub(2)));
    // The rule is the box's top, and its bottom takes a line of its own.
    let room = usize::from(over.height.saturating_sub(1));
    if most == 0 || room < 2 {
        return;
    }
    let inner =
        if widest { most } else { shown(most, room - 2).takes().clamp(fewest.min(most), most) };
    let lines = shown(inner, room - 2).lines(inner, room);
    let Some((rule, rest)) = lines.split_first() else {
        return;
    };
    // Both fit `over`, as `inner` and `lines` were made to.
    let width = u16::try_from(inner + 2).unwrap_or(over.width);
    let height = u16::try_from(lines.len() + 1).unwrap_or(over.height);
    let area = Rect::new(over.right() - width, over.bottom() - height, width, height);
    boxed(buffer, area, rule, rest);
}

/// Draws `note` floating at the bottom right of `over`, where the menu over
/// zellij floats: what went wrong as it did what a key asked, or why it did
/// not open, which any key or click closes.
pub fn draw_note(buffer: &mut Buffer, over: Rect, note: &str) {
    float(buffer, over, false, |width, _| Shown {
        title: TITLE.to_owned(),
        right: String::new(),
        body: wrap(note, width, Style::new().fg(sidebar::ERROR)),
        footer: keys_line(width, &[("<any key>", "Close")]),
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

/// What the rule over the menu names: the groups opened.
fn title(keys: &Keys) -> String {
    let names: Vec<&str> = keys.trail().into_iter().map(|(name, _)| name).collect();
    if names.is_empty() { TITLE.to_owned() } else { names.join(" › ") }
}

/// The legend at the bottom of the sidebar, `width` wide, in `room` lines at
/// most: under a rule, the keys the top of the sidebar takes, as `Top::of`
/// leaves them to the menu, and the sidebar's command, each with what it
/// does.
pub fn legend(width: usize, room: usize) -> Vec<Line<'static>> {
    let keys = Keys::legend();
    let mut lines = Vec::with_capacity(room);
    if room == 0 {
        return lines;
    }
    lines.push(rule(TITLE, "", width));
    let inner = width.saturating_sub(1);
    lines.extend(keys.root.iter().map(|entry| entry_line(entry, inner)).take(room - 1));
    lines
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
        Entry::New(new) => spans.push(does(new.does())),
        Entry::Close { scope, .. } => spans.push(does(scope.does())),
        Entry::Brief(scope) => spans.push(does(scope.does())),
        Entry::Command(command) => spans.push(does(command.does())),
        Entry::Pull(pull) => spans.push(does(pull.does())),
        Entry::Search => spans.push(does(SEARCH_DOES)),
        Entry::Ask => spans.push(does(ASK_DOES)),
    }
    Line::from(spans)
}

/// The keys that go back and close, and what each does, in as many words as
/// `width` has room for: all of them, or else the keys alone.
pub fn keys_line(width: usize, keys: &[(&str, &str)]) -> Line<'static> {
    let spelled = |sep: &str, joiner: &str| {
        let each = keys.iter().map(|(key, does)| format!("{key}{sep}{does}"));
        each.collect::<Vec<_>>().join(joiner)
    };
    let bare = |joiner: &str| keys.iter().map(|(key, _)| *key).collect::<Vec<_>>().join(joiner);
    let candidates =
        [spelled(" - ", ", "), spelled(" ", "  "), spelled(" ", " "), bare("/"), bare("")];
    let room = width.saturating_sub(1);
    let line = candidates.into_iter().find(|line| line.width() <= room).unwrap_or_else(|| bare(""));
    let dim = Style::new().add_modifier(Modifier::DIM);
    // Each key in the accent, and the words between them dimmed.
    let mut spans = vec![Span::raw(" ")];
    let mut rest = line.as_str();
    for (key, _) in keys {
        let Some(at) = rest.find(key) else {
            continue;
        };
        if at > 0 {
            spans.push(Span::styled(rest[..at].to_owned(), dim));
        }
        spans.push(Span::styled((*key).to_owned(), Style::new().fg(ACCENT)));
        rest = &rest[at + key.len()..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_owned(), dim));
    }
    Line::from(spans)
}

/// The search box over the skills, in at most `room` lines `width` wide: what
/// is typed, and under it the skills that match, best first, as many as
/// `SEARCH_ROWS` and the room show around the one the cursor is on, or a line
/// that says why there are none. It keeps the lines the skills found fill,
/// however many of them match, so the menu keeps its size as you type.
fn search_lines(search: &Search, width: usize, room: usize) -> Vec<Line<'static>> {
    let Some(list) = room.checked_sub(1) else {
        return Vec::new();
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
        lines.extend(shown.map(|(at, (skill, hits))| {
            skill_line(&skill.name.to_string(), &skill.description, hits, at == selected, width)
        }));
    }
    lines.resize(1 + list, Line::default());
    lines
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
    use std::time::{Duration, SystemTime};

    use super::*;
    use crate::close::{self, Deletes, Doing, Seat, Subject};
    use crate::pull::{At, Link, Ready};
    use crate::sidebar::Listed;
    use crate::skills::{Name, Skill};
    use crate::zellij::Closes;

    fn listed() -> Listed {
        Listed::new("luneta", SystemTime::UNIX_EPOCH + Duration::from_secs(1))
    }

    fn pane_row() -> Target {
        Target {
            place: Place::Pane { session: listed(), pane: 4 },
            cwd: Some("/w/luneta/main".to_owned()),
            branch: Some("feat/menu".to_owned()),
        }
    }

    fn session_row() -> Target {
        Target { place: Place::Session(listed()), cwd: None, branch: None }
    }

    fn char(key: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(key))
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
            text(&menu.lines(31, 9)),
            [
                "─ New ─────────── luneta/main ─",
                " p  new pane",
                " t  new tab",
                " s  new session",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('p')));
        let cwd = "/w/luneta/main".to_owned();
        let opens = Opens::Pane { beside: 4, command: Vec::new() };
        assert_eq!(wants, Wants::Open { cwd: cwd.clone(), opens });
        assert!(menu.opening());
        // Keys do nothing while zellij opens it, but close the menu.
        let (menu, wants) = stays(menu.key(&char('p')));
        assert_eq!(wants, Wants::Nothing);
        assert!(menu.opening());
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);

        // luneta goes to a new session itself.
        let (menu, _) = stays(opened(pane_row(), Some('n')));
        assert_eq!(closed(menu.key(&char('s'))), Done::Start(cwd));
    }

    #[test]
    fn the_first_group_has_luneta_s_keys_and_backspace_goes_back_to_it() {
        let (menu, _) = stays(opened(pane_row(), None));
        assert_eq!(
            text(&menu.lines(31, 9)),
            [
                "─ luneta ──────── luneta/main ─",
                " /  Search skills",
                " a  Ask",
                " c  Claude ▸",
                " n  New ▸",
                " d  Delete ▸",
                " b  Brief ▸",
                " <ESC> - Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(text(&menu.lines(31, 9))[0], "─ luneta ──────── luneta/main ─");
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Backspace))), Done::Nothing);
        // A key the menu has not opens none.
        assert!(Menu::open(1, pane_row(), Some('x')).is_none());
    }

    #[test]
    fn the_legend_lists_the_sidebar_s_commands_then_the_menu_s_groups() {
        assert_eq!(
            text(&legend(31, 9)),
            [
                "─ luneta ──────────────────────",
                " /  Search",
                " u  Unread",
                " v  Reorder",
                " a  Ask",
                " c  Claude ▸",
                " n  New ▸",
                " d  Delete ▸",
                " b  Brief ▸",
            ]
        );
        // `m` and the keys that move are the sidebar's, and `/` and `u` its
        // commands.
        assert_eq!(Top::of('m'), Some(Top::Menu));
        assert_eq!(Top::of('/'), Some(Top::Command(Command::Search)));
        assert_eq!(Top::of('u'), Some(Top::Command(Command::Unread)));
        assert_eq!(Top::of('a'), None);
        assert_eq!(Top::of('c'), None);
        assert_eq!(Top::of('n'), None);
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
            text(&menu.lines(31, 9)),
            [
                "─ Claude ──────── luneta/main ─",
                " t  /thermo-nuclear-review",
                " m  /make-illegal-states-unrep…",
                " s  /tigerstyle",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('t')));
        let named = "thermo-nuclear-review: feat/menu";
        let command = ["claude", "--name", named, "/thermo-nuclear-review"].map(str::to_owned);
        let opens = Opens::Pane { beside: 4, command: command.to_vec() };
        assert_eq!(wants, Wants::Open { cwd: "/w/luneta/main".to_owned(), opens });
        assert!(menu.opening());
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
            text(&menu.lines(31, 9)),
            [
                "─ Brief ───────────────────────",
                " z  Every session",
                " s  This session",
                " h  Highlighted",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        for (key, scope) in [
            ('z', brief::Scope::Every),
            ('s', brief::Scope::Session),
            ('h', brief::Scope::Highlighted),
        ] {
            let (menu, _) = stays(opened(pane_row(), Some('b')));
            let place = pane_row().place;
            assert_eq!(closed(menu.key(&char(key))), Done::Brief { scope, place }, "{key}");
        }
    }

    #[test]
    fn a_menu_that_cannot_open_beside_its_row_says_why() {
        let (menu, _) = stays(opened(session_row(), Some('n')));
        assert_eq!(closed(menu.key(&char('p'))), Done::Wrong(NO_ROW_PANE.to_owned()));
        let unknown = Target { cwd: None, ..pane_row() };
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
        };
        names.iter().map(skill).collect()
    }

    #[test]
    fn slash_searches_the_skills_and_enter_starts_an_agent_on_the_one_picked() {
        let (menu, _) = stays(opened(pane_row(), None));
        let (mut menu, wants) = stays(menu.key(&char('/')));
        assert_eq!(wants, Wants::Skills("/w/luneta/main".to_owned()));
        assert_eq!(
            text(&menu.lines(31, 6))[..3],
            ["─ Skills ──────── luneta/main ─", " /_", " finding skills…"]
        );
        menu.found(Ok(skills(&["code-review", "tigerstyle"])));
        // The one the cursor is on is lit, as the sidebar's row is while it
        // has the focus, which it has while the search is open.
        let lit: Vec<_> = menu.lines(31, 6)[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [Some(LIT), None]);
        let (menu, _) = stays(menu.key(&char('t')));
        let (menu, _) = stays(menu.key(&char('i')));
        assert_eq!(text(&menu.lines(31, 6))[..3], [
            "─ Skills ──────── luneta/main ─".to_owned(),
            " /ti_".to_owned(),
            " /tigerstyle  tigerstyle it".to_owned(),
        ]);
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = claude("/tigerstyle", Some("feat/menu"));
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, Wants::Open { cwd: "/w/luneta/main".to_owned(), opens });
        assert!(menu.opening());
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
        assert_eq!(wants, Wants::Plan(Scope::Pane(4)));
        assert_eq!(menu.planning(), Some(4));
        let done = closed(menu.planned(plan(Closes::Pane, vec![(4, Doing::Idle)])));
        assert_eq!(done, Done::Act(Act::Pane { session: "luneta".to_owned(), pane: 4 }));
        // On a session's row, `d d` deletes the session, which zellij need not
        // say anything of first.
        let (menu, _) = stays(opened(session_row(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        assert_eq!(wants, Wants::Plan(Scope::Session));
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
            text(&menu.lines(31, 9)),
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
        assert_eq!(wants, Wants::Plan(Scope::Pane(4)));
        // Another pane closes with it now: its tab, which it asks of again.
        let tab = || Closes::Tab { id: 2, panes: vec![4, 5] };
        let at_work = vec![(4, Doing::Busy), (5, Doing::Waiting)];
        let (menu, wants) = stays(menu.planned(plan(tab(), at_work)));
        assert_eq!(wants, Wants::Nothing);
        let asking = text(&menu.lines(31, 9));
        assert_eq!(asking[1..3], [" Close its tab? 2 agents, 1", " busy, 1 waiting"]);
        // The tab is the same, but its other agent went to work as you read.
        let (menu, _) = stays(menu.key(&char('y')));
        let busier = || vec![(4, Doing::Busy), (5, Doing::Busy)];
        let (menu, wants) = stays(menu.planned(plan(tab(), busier())));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9))[1..3], [" Close its tab? 2 agents, 2", " busy"]);
        let (menu, _) = stays(menu.key(&char('y')));
        let act = Act::Tab { session: "luneta".to_owned(), tab: 2, panes: vec![4, 5] };
        assert_eq!(closed(menu.planned(plan(tab(), busier()))), Done::Act(act));
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
        let leave = Act::Leave { session: "luneta".to_owned(), started, to: elo };
        assert_eq!(closed(menu.planned(close::plan(&subject))), Done::Act(leave));
    }

    /// zellij's side, `width` by `height` past `ZELLIJ_X` columns of the
    /// sidebar's, with what `draw` draws floating over it, as text. Each cell
    /// of the box is in gruvbox's colours.
    fn floated(draw: impl FnOnce(&mut Buffer, Rect), width: u16, height: u16) -> Vec<String> {
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

    /// The menu over zellij floats at the bottom right of zellij's side, in
    /// a box as wide as the keys of the group open and the rule over them
    /// take, and as tall as its lines.
    #[test]
    fn the_menu_floats_at_the_bottom_right_of_zellij_s_side() {
        let menu = Menu::new(1, pane_row());
        let boxed = |inner: &str| format!("{}│{inner:<24}│", "z".repeat(24));
        assert_eq!(floated(|buffer, over| menu.draw(buffer, over), 50, 10), [
            "z".repeat(50),
            format!("{}┌─ luneta ─ luneta/main ─┐", "z".repeat(24)),
            boxed(" /  Search skills"),
            boxed(" a  Ask"),
            boxed(" c  Claude ▸"),
            boxed(" n  New ▸"),
            boxed(" d  Delete ▸"),
            boxed(" b  Brief ▸"),
            boxed(" <ESC> - Close"),
            format!("{}└{}┘", "z".repeat(24), "─".repeat(24)),
        ]);

        let (menu, _) = stays(menu.key(&char('c')));
        let zellij = "z".repeat(7);
        let rule = format!("─ Claude {} luneta/main ─", "─".repeat(18));
        assert_eq!(floated(|buffer, over| menu.draw(buffer, over), 50, 7)[1..], [
            format!("{zellij}┌{rule}┐"),
            format!("{zellij}│{:<41}│", " t  /thermo-nuclear-review"),
            format!("{zellij}│{:<41}│", " m  /make-illegal-states-unrepresentable"),
            format!("{zellij}│{:<41}│", " s  /tigerstyle"),
            format!("{zellij}│{:<41}│", " <BS> - Back, <ESC> - Close"),
            format!("{zellij}└{}┘", "─".repeat(41)),
        ]);
        // Narrower than the keys take, the box is as wide as zellij's side.
        let narrow = floated(|buffer, over| menu.draw(buffer, over), 30, 7);
        assert_eq!(narrow[2], format!("│{:<28}│", " t  /thermo-nuclear-review"));
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

    fn link() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    /// The menu for the pull request, its key `key` pressed, waiting for
    /// luneta to find where it goes.
    fn pulling(key: char) -> Menu {
        let (menu, wants) = stays(Menu::pull(1, pane_row(), link()).key(&char(key)));
        assert_eq!(wants, Wants::Pull(link()));
        assert!(menu.pulling());
        menu
    }

    /// A paste opens the menu for the pull request, named for it, with its
    /// repository where another menu says where its pane works. A key waits
    /// for luneta to find where it goes and to make the worktree, and does
    /// nothing else meanwhile, but close the menu.
    #[test]
    fn the_menu_for_a_pull_request_has_a_key_for_a_worktree_and_one_for_a_review() {
        let menu = Menu::pull(1, pane_row(), link());
        assert_eq!(
            text(&menu.lines(31, 9)),
            [
                "─ #2736 ──────── cashubtc/cdk ─",
                " w  worktree",
                " r  /thermo-nuclear-review",
                " <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
        let (menu, wants) = stays(menu.key(&char('w')));
        assert_eq!(wants, Wants::Pull(link()));
        assert_eq!(
            text(&menu.lines(31, 9)),
            ["─ #2736 ──────── cashubtc/cdk ─", " Making a worktree of #2736…", " <ESC> - Close"]
        );
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
        let ready = |at| Ok(Ready { at, worktree: worktree.clone(), branch: branch.clone() });
        let review = [
            "claude",
            "--name",
            "thermo-nuclear-review: feat/x",
            "/thermo-nuclear-review https://github.com/cashubtc/cdk/pull/2736",
        ]
        .map(str::to_owned)
        .to_vec();
        let session = || At::Session { session: other.clone(), pane: 7 };
        for (key, command) in [('w', Vec::new()), ('r', review.clone())] {
            let (menu, wants) = stays(pulling(key).pulled(ready(session())));
            let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
            assert_eq!(wants, Wants::Open { cwd: worktree.clone(), opens });
            assert!(menu.opening());
            assert_eq!(menu.target().place, Place::Pane { session: other.clone(), pane: 7 });
        }
        let there = || At::There { session: other.clone(), pane: 9 };
        let place = Place::Pane { session: other.clone(), pane: 9 };
        assert_eq!(closed(pulling('w').pulled(ready(there()))), Done::Go(place.clone()));
        let (menu, wants) = stays(pulling('r').pulled(ready(there())));
        let opens = Opens::Pane { beside: 9, command: review };
        assert_eq!(wants, Wants::Open { cwd: worktree.clone(), opens });
        assert_eq!(menu.target().place, place);
    }

    /// Where no session works in the repository, luneta makes one in its
    /// clone, and the menu waits for zellij to make it and open the tab.
    #[test]
    fn a_session_luneta_makes_for_a_pull_request_opens_its_tab_once_there() {
        let name = SessionName::parse("cdk").unwrap();
        let at = At::Clone { cwd: "/w/cdk".to_owned(), name: name.clone() };
        let ready = Ready { at, worktree: "/w/cdk/feat-x".to_owned(), branch: None };
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
        assert!(menu.opening());
        assert_eq!(menu.making(), Some(&name));
        // Why luneta found nothing closes the menu, and says so.
        let why = "no session works in cashubtc/cdk".to_owned();
        assert_eq!(
            closed(pulling('r').pulled(Err(why))),
            Done::Wrong("menu: cashubtc/cdk#2736: no session works in cashubtc/cdk".to_owned())
        );
    }
}
