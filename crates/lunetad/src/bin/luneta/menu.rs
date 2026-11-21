use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use luneta_protocol::github::{PullRequest, State};
use luneta_protocol::said::Said;
use lunetad::links::Branch;
use lunetad::projects::{self, Argv, Opens, ProjectId, ViewId, WorktreeId};
use lunetad::prunes;
use lunetad::rebases::{self, Came, Rebased, Unrebased};
use lunetad::{Destination, Directory, SessionName};
use luneta_protocol::shell::Ending;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::actions::{
    self, Action, Command, Key, Noun, Pasted, MENU_GROUPS, OPEN, REVIEW, ROW_GROUPS, YANK,
};
use crate::agents::{self, Harness, Picked};
use crate::brief;
use crate::checks::{self, Failing, PullPage};
use crate::close::{Act, Plan, Scope};
use crate::gruvbox;
use crate::icons;
use crate::issue::Repository;
use crate::machines::Machine;
use crate::printed::{self, Printed};
use crate::prune;
use crate::rebase;
use crate::pull::{At, Found, Kind, Readied, Ready, Wanted, Work};
use crate::sidebar::{
    self, ACCENT, LIT, Listed, Mark, NAME, ViewKey, ViewPlace, Place, ProjectRow, TAG,
    WorktreePlace,
};
use crate::skills::{Name, Search, Skills};
use crate::spots::{Drawn, Spots};

pub const ALL_KEYS: char = '?';
const ALL_KEYS_DOES: &str = "All keys";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Label {
    Key(KeyCode, KeyModifiers),
    Two(char, char),
}

impl Label {
    pub const fn char(c: char) -> Label {
        Label::Key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    pub const fn code(code: KeyCode) -> Label {
        Label::Key(code, KeyModifiers::NONE)
    }

    fn pressed(self) -> Option<KeyEvent> {
        match self {
            Label::Key(code, modifiers) => Some(KeyEvent::new(code, modifiers)),
            Label::Two(..) => None,
        }
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (code, modifiers) = match *self {
            Label::Two(first, second) => return write!(f, "{first} {second}"),
            Label::Key(code, modifiers) => (code, modifiers),
        };
        let held = [
            (KeyModifiers::CONTROL, "Ctrl"),
            (KeyModifiers::ALT, "Alt"),
            (KeyModifiers::SHIFT, "Shift"),
            (KeyModifiers::SUPER, "Super"),
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

pub(crate) const MOVES: [(Label, &str); 10] = [
    (Label::Two('j', 'k'), "Next row, row before"),
    (Label::Two('g', 'g'), "First row"),
    (Label::char('G'), "Last row"),
    (Label::Two('h', 'l'), "Fold, unfold"),
    (Label::char(' '), "Fold or unfold"),
    (Label::Two('J', 'K'), "Move what v holds"),
    (Label::code(KeyCode::Enter), "Go to the row"),
    (Label::char('m'), "Menu"),
    (Label::char(ALL_KEYS), "All keys"),
    (Label::code(KeyCode::Esc), "Back to the view"),
];

pub(crate) const MENU_MOVES: [(Label, &str); 3] = [
    (Label::code(KeyCode::Tab), "use another agent"),
    (Label::code(KeyCode::Backspace), "back a group"),
    (Label::code(KeyCode::Esc), "close"),
];

pub const TITLE: &str = "luneta";
const SEARCH_TITLE: &str = "Skills";
const SSH_TITLE: &str = "ssh";
const PROJECT_TITLE: &str = "New project";
const WORKTREE_TITLE: &str = "New worktree";
const RENAME_TITLE: &str = "Rename";

const NEXT_DOES: [&str; 2] = ["another agent", "another"];

const VIEW_S: &str = ", the view's";

const GROUP: &str = "▸";

const GAP: usize = 2;

const SEARCH_ROWS: usize = 8;

const FLOATING: (usize, usize) = (24, 62);

const TACKLE: &str = "Tackle";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Top {
    Move(isize),
    Last,
    First,
    Shift(isize),
    Fold(Option<bool>),
    Menu,
    Group(char),
    AllKeys,
    Command(Command),
}

impl Top {
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
            YANK => Some(Top::Group(YANK)),
            OPEN => Some(Top::Group(OPEN)),
            ALL_KEYS => Some(Top::AllKeys),
            _ => None,
        }
    }

    pub fn of(key: char) -> Option<Top> {
        let command = Action::row().find(|(_, path)| *path == [key]);
        let command = command.and_then(|(action, _)| Command::of(action));
        Top::moves(key).or_else(|| command.map(Top::Command))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Last {
    Fix,
    Explain,
}

impl Last {
    const fn asks(self) -> &'static str {
        match self {
            Last::Fix => "Fix this.",
            Last::Explain => "Explain this.",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pull {
    Worktree,
    Agent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asks {
    Review,
    Tackle,
}

impl Asks {
    fn of(work: &Work) -> Asks {
        match work {
            Work::Link(link) if link.kind() == Kind::Pull => Asks::Review,
            Work::Link(_) | Work::File(_) => Asks::Tackle,
        }
    }
}

impl Pull {
    const ALL: [Pull; 2] = [Pull::Worktree, Pull::Agent];

    pub fn offered(work: &Work) -> [(Pull, Pasted); 2] {
        let asks = Asks::of(work);
        Pull::ALL.map(|pull| (pull, pull.pasted(asks)))
    }

    const fn pasted(self, asks: Asks) -> Pasted {
        match (self, asks) {
            (Pull::Worktree, _) => Pasted::Worktree,
            (Pull::Agent, Asks::Review) => Pasted::Review,
            (Pull::Agent, Asks::Tackle) => Pasted::Tackle,
        }
    }

    const fn wanted(self, asks: Asks) -> Wanted {
        match (self, asks) {
            (Pull::Agent, Asks::Review) => Wanted::AtHead,
            (Pull::Worktree, _) | (Pull::Agent, Asks::Tackle) => Wanted::AsItIs,
        }
    }

    fn pressed(key: &KeyEvent, work: &Work) -> Option<Pull> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        if !bare(key) {
            return None;
        }
        let mut offered = Pull::offered(work).into_iter();
        offered.find(|(_, pasted)| pasted.key() == c).map(|(pull, _)| pull)
    }
}

enum Entry {
    Group { key: char, name: &'static str, entries: Vec<Entry> },
    Act { key: char, action: Action },
}

impl Entry {
    fn key(&self) -> char {
        match self {
            Entry::Group { key, .. } | Entry::Act { key, .. } => *key,
        }
    }

    fn does(&self) -> &'static str {
        match self {
            Entry::Group { name, .. } => name,
            Entry::Act { action, .. } => action.name(),
        }
    }
}

fn tree(
    actions: impl Iterator<Item = (Action, &'static [char])>,
    groups: &[(char, &'static str)],
) -> Vec<Entry> {
    let mut root: Vec<Entry> = Vec::new();
    for (action, path) in actions {
        let (group, key) = match path {
            [key] => (None, *key),
            [group, key] => (Some(*group), *key),
            _ => unreachable!("{action:?} on {path:?}: actions.rs keeps a path to one or two keys"),
        };
        let Some(group) = group else {
            root.push(Entry::Act { key, action });
            continue;
        };
        let held = |entry: &Entry| matches!(entry, Entry::Group { key, .. } if *key == group);
        let at = root.iter().position(held).unwrap_or_else(|| {
            let name = actions::group_name(groups, group);
            let name = name.expect("actions.rs names the group of every path two keys long");
            root.push(Entry::Group { key: group, name, entries: Vec::new() });
            root.len() - 1
        });
        let Some(Entry::Group { entries, .. }) = root.get_mut(at) else {
            unreachable!("entry {at} of the root is the group {group:?}");
        };
        entries.push(Entry::Act { key, action });
    }
    root
}

#[derive(Debug, PartialEq, Eq)]
enum Pressed {
    Moved,
    Start(String),
    Switched(Harness),
    Act(Action),
    Close,
    Nothing,
}

struct Keys {
    root: Vec<Entry>,
    open: Vec<usize>,
    search: Option<Search>,
    agent: Picked,
}

impl Keys {
    fn new(target: &Target, agent: Picked) -> Self {
        let fitting = Action::menu().filter(|(action, _)| target.fits(*action));
        Keys { root: tree(fitting, &MENU_GROUPS), open: Vec::new(), search: None, agent }
    }

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

    fn entries(&self) -> &[Entry] {
        self.trail().last().map_or(self.root.as_slice(), |(_, entries)| entries)
    }

    fn starts(&self) -> bool {
        if self.search.is_some() {
            return true;
        }
        if self.open.is_empty() {
            return false;
        }
        let starting =
            |entry: &Entry| matches!(entry, Entry::Act { action, .. } if action.starts_agent());
        self.entries().iter().any(starting)
    }

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
        if let Entry::Act { action, .. } = &entries[at] {
            return Pressed::Act(*action);
        }
        self.open.push(at);
        Pressed::Moved
    }
}

fn bare(key: &KeyEvent) -> bool {
    key.modifiers.difference(KeyModifiers::SHIFT).is_empty()
}

pub fn closes(key: &KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('c') => key.modifiers == KeyModifiers::CONTROL,
        KeyCode::Esc => bare(key),
        _ => false,
    }
}

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

const SSH_HINT: &str = "what you type ssh to it: a Tailscale name, a Host of your ssh config, or \
                        me@vps";

const FOLDER_HINT: &str =
    "the folder it holds, on the row's machine: an absolute path, or one in ~";

const PROJECT_NAME_HINT: &str = "its name; with none, it is named for its folder";

const UNRENAME_HINT: &str = "its new name; with none, it goes back to the name luneta gives it";

const RENAME_HINT: &str = "its new name";

const BRANCH_HINT: &str = "its branch: one git has is checked out, and any other is made from the \
                           default branch";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Typing {
    Machine,
    Folder,
    Name(String),
    Branch(ProjectId),
    Rename(Renames),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Renames {
    Terminal(ViewId),
    Worktree(WorktreeId),
    Project(ProjectId),
}

impl Renames {
    fn parse(self) -> fn(&str) -> Result<projects::Name, String> {
        match self {
            Renames::Terminal(_) | Renames::Worktree(_) => projects::Name::parse,
            Renames::Project(_) => projects::Name::of_project,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rename {
    Terminal(ViewId, Option<projects::Name>),
    Worktree(WorktreeId, Option<projects::Name>),
    Project(ProjectId, projects::Name),
}

impl Rename {
    #[must_use]
    pub fn noun(&self) -> &'static str {
        match self {
            Rename::Terminal(..) => "view",
            Rename::Worktree(..) => "worktree",
            Rename::Project(..) => "project",
        }
    }
}

#[expect(clippy::large_enum_variant, reason = "one is made on each Enter, and goes at once")]
enum Entered {
    Done(Done),
    Folder(String),
    Wrong(String),
}

impl Typing {
    fn title(&self) -> &'static str {
        match self {
            Typing::Machine => SSH_TITLE,
            Typing::Folder | Typing::Name(_) => PROJECT_TITLE,
            Typing::Branch(_) => WORKTREE_TITLE,
            Typing::Rename(_) => RENAME_TITLE,
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Typing::Machine => SSH_HINT,
            Typing::Folder => FOLDER_HINT,
            Typing::Name(_) => PROJECT_NAME_HINT,
            Typing::Branch(_) => BRANCH_HINT,
            Typing::Rename(Renames::Terminal(_) | Renames::Worktree(_)) => UNRENAME_HINT,
            Typing::Rename(Renames::Project(_)) => RENAME_HINT,
        }
    }

    fn enter(&self) -> &'static str {
        match self {
            Typing::Machine => "Add",
            Typing::Folder => "Next",
            Typing::Name(_) | Typing::Branch(_) => "Make",
            Typing::Rename(_) => "Rename",
        }
    }

    fn typed(self, typed: &str, on: &Machine) -> Entered {
        let named = |parse: fn(&str) -> Result<projects::Name, String>| {
            let named = Some(typed.trim()).filter(|name| !name.is_empty());
            named.map(parse).transpose()
        };
        let renamed = |rename: Rename| Entered::Done(Done::Rename(on.clone(), rename));
        match self {
            Typing::Machine => match typed.parse::<Destination>() {
                Ok(destination) => Entered::Done(Done::Ssh(destination)),
                Err(why) => Entered::Wrong(format!("not a destination: {why}")),
            },
            Typing::Folder => match folder_typed(typed) {
                Ok(folder) => Entered::Folder(folder),
                Err(why) => Entered::Wrong(why),
            },
            Typing::Name(folder) => match named(projects::Name::of_project) {
                Ok(name) => Entered::Done(Done::Project { on: on.clone(), folder, name }),
                Err(why) => Entered::Wrong(why),
            },
            Typing::Branch(project) => match Branch::parse(typed) {
                Ok(branch) => Entered::Done(Done::Worktree(on.clone(), project, branch)),
                Err(why) => Entered::Wrong(why),
            },
            Typing::Rename(asked) => match (asked, named(asked.parse())) {
                (_, Err(why)) => Entered::Wrong(why),
                (Renames::Terminal(terminal), Ok(name)) => {
                    renamed(Rename::Terminal(terminal, name))
                },
                (Renames::Worktree(worktree), Ok(name)) => {
                    renamed(Rename::Worktree(worktree, name))
                },
                (Renames::Project(project), Ok(Some(name))) => {
                    renamed(Rename::Project(project, name))
                },
                (Renames::Project(_), Ok(None)) => {
                    Entered::Wrong("a project keeps a name: type one".to_owned())
                },
            },
        }
    }
}

fn folder_typed(typed: &str) -> Result<String, String> {
    let folder = typed.trim();
    if projects::Dir::below_home(folder).is_some() || folder.starts_with('/') {
        return Ok(folder.to_owned());
    }
    Err(format!("not a folder: {folder:?} is no absolute path, nor one in ~"))
}

fn own_folder() -> String {
    let here = std::env::current_dir().ok();
    here.and_then(|here| here.to_str().map(str::to_owned)).unwrap_or_else(|| "~".to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chain {
    typing: Typing,
    typed: String,
    wrong: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Chained {
    Typing(Chain),
    Back,
    Close,
    Done(Done),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Says<'a> {
    Hint(&'static str),
    Wrong(&'a str),
}

impl Chain {
    fn new(typing: Typing, typed: String) -> Chain {
        Chain { typing, typed, wrong: None }
    }

    #[must_use]
    pub fn of(target: &Target, action: Action) -> Option<Chain> {
        Chain::offered(target, &target.offer(action)?)
    }

    #[must_use]
    pub fn branch(project: ProjectId) -> Chain {
        Chain::new(Typing::Branch(project), String::new())
    }

    #[must_use]
    pub fn rename(renames: Renames) -> Chain {
        Chain::new(Typing::Rename(renames), String::new())
    }

    fn offered(target: &Target, offer: &Offer<'_>) -> Option<Chain> {
        let typing = match offer {
            Offer::NewWorktree(project) => return Some(Chain::branch(*project)),
            Offer::NewProject => return Some(Chain::new(Typing::Folder, target.folder_hint())),
            Offer::NewMachine => Typing::Machine,
            Offer::Rename(renames) => return Some(Chain::rename(*renames)),
            Offer::SearchSkills(_)
            | Offer::Skill(..)
            | Offer::Last(..)
            | Offer::FixChecks(_)
            | Offer::NewView(_)
            | Offer::NewWelcome(_)
            | Offer::CloseView(_)
            | Offer::CloseWorktree(_)
            | Offer::CloseProject(_)
            | Offer::ForgetMachine(_)
            | Offer::DeleteMerged(_)
            | Offer::Rebase(_)
            | Offer::Brief(_)
            | Offer::Ask
            | Offer::FileIssue(_)
            | Offer::Quit => return None,
        };
        Some(Chain::new(typing, String::new()))
    }

    #[must_use]
    pub fn key(mut self, key: &KeyEvent, on: &Machine) -> Chained {
        match type_key(key, &mut self.typed) {
            Typed::Close => Chained::Close,
            Typed::Back => match self.typing {
                Typing::Name(folder) => Chained::Typing(Chain::new(Typing::Folder, folder)),
                Typing::Machine | Typing::Folder | Typing::Branch(_) | Typing::Rename(_) => {
                    Chained::Back
                },
            },
            Typed::Typing => {
                self.wrong = None;
                Chained::Typing(self)
            },
            Typed::Enter => match self.typing.clone().typed(&self.typed, on) {
                Entered::Done(done) => Chained::Done(done),
                Entered::Folder(folder) => {
                    Chained::Typing(Chain::new(Typing::Name(folder), String::new()))
                },
                Entered::Wrong(why) => {
                    self.wrong = Some(why);
                    Chained::Typing(self)
                },
            },
        }
    }

    pub fn paste(&mut self, text: &str) {
        self.typed.extend(text.chars().filter(|c| !c.is_control()));
        self.wrong = None;
    }

    #[must_use]
    pub fn title(&self) -> &'static str {
        self.typing.title()
    }

    #[must_use]
    pub fn enter(&self) -> &'static str {
        self.typing.enter()
    }

    #[must_use]
    pub fn typed(&self) -> &str {
        &self.typed
    }

    #[must_use]
    pub fn says(&self) -> Says<'_> {
        match &self.wrong {
            Some(wrong) => Says::Wrong(wrong),
            None => Says::Hint(self.typing.hint()),
        }
    }

    #[must_use]
    pub fn right(&self) -> String {
        match &self.typing {
            Typing::Name(folder) => sidebar::short_cwd(folder),
            Typing::Machine | Typing::Folder | Typing::Branch(_) | Typing::Rename(_) => {
                String::new()
            },
        }
    }
}

enum Typed {
    Typing,
    Enter,
    Back,
    Close,
}

fn type_key(key: &KeyEvent, typed: &mut String) -> Typed {
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

#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Yes,
    Back,
    Close,
    Nothing,
}

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

#[derive(Clone, Debug)]
struct Close {
    session: Listed,
    scope: Scope,
}

impl Close {
    fn key(&self) -> char {
        yes_key(self.scope.action())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Project(Machine, ProjectTarget),
    Worktree(Machine, WorktreeRow),
    View(Machine, ViewRow),
    Machine(Machine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTarget {
    pub project: ProjectRow,
    pub dir: String,
}

enum Offer<'t> {
    SearchSkills(&'t ViewRow),
    Skill(&'t ViewRow, &'static str),
    Last(&'t ViewRow, Last),
    FixChecks(&'t ViewRow),
    NewView(&'t ViewRow),
    NewWelcome(&'t ViewRow),
    NewWorktree(ProjectId),
    NewProject,
    NewMachine,
    Rename(Renames),
    CloseView(&'t ViewRow),
    CloseWorktree(&'t WorktreeRow),
    CloseProject(&'t Listed),
    ForgetMachine(&'t Destination),
    DeleteMerged(Directory),
    Rebase(Rebases<'t>),
    Brief(brief::Scope),
    Ask,
    FileIssue(Repository),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRow {
    pub place: WorktreePlace,
    pub name: String,
    pub folder: Directory,
    pub branch: Option<String>,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewRow {
    pub session: Listed,
    pub view: ViewKey,
    pub terminal: Option<ViewId>,
    pub cwd: Cwd,
    pub branch: Option<String>,
    pub repository: Option<String>,
    pub pull_request: Option<PullRequest>,
    pub ran: Ran,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cwd {
    Unsaid,
    At(Directory),
    Unread { why: String },
}

impl Cwd {
    #[must_use]
    pub fn of(said: Option<String>) -> Cwd {
        match said.map(Directory::parse) {
            None => Cwd::Unsaid,
            Some(Ok(dir)) => Cwd::At(dir),
            Some(Err(why)) => Cwd::Unread { why },
        }
    }

    #[must_use]
    pub fn at(&self) -> Option<&Directory> {
        match self {
            Cwd::At(dir) => Some(dir),
            Cwd::Unsaid | Cwd::Unread { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ran {
    Unhooked,
    Ready,
    Unknown,
    Going(Option<Said>),
    Stopped(Option<Said>),
    Ended(Ended),
    EndedUnsaid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ended {
    pub command: Said,
    pub ending: Option<Ending>,
    pub at: SystemTime,
}

impl Target {
    pub fn machine(&self) -> &Machine {
        match self {
            Target::Project(machine, _)
            | Target::Worktree(machine, _)
            | Target::View(machine, _)
            | Target::Machine(machine) => machine,
        }
    }

    #[must_use]
    pub fn place(&self) -> Option<Place> {
        match self {
            Target::Project(_, target) => Some(Place::Session(target.project.listed().clone())),
            Target::Worktree(_, row) => Some(Place::Worktree(row.place.clone())),
            Target::View(_, row) => {
                Some(Place::View(ViewPlace::new(row.session.clone(), row.view)))
            },
            Target::Machine(_) => None,
        }
    }

    #[must_use]
    pub fn noun(&self) -> Noun {
        match self {
            Target::Project(..) => Noun::Project,
            Target::Worktree(..) => Noun::Worktree,
            Target::View(..) => Noun::View,
            Target::Machine(_) => Noun::Machine,
        }
    }

    #[must_use]
    pub fn fits(&self, action: Action) -> bool {
        self.offer(action).is_some()
    }

    fn offer(&self, action: Action) -> Option<Offer<'_>> {
        if !action.fits(self.noun()) {
            return None;
        }
        let offer = match (action, self) {
            (Action::SearchSkills, Target::View(_, row)) => Offer::SearchSkills(row),
            (Action::Review | Action::IllegalStates | Action::TigerStyle, Target::View(_, row)) => {
                Offer::Skill(row, action.skill()?)
            },
            (Action::FixLast, Target::View(_, row)) => Offer::Last(row, Last::Fix),
            (Action::ExplainLast, Target::View(_, row)) => Offer::Last(row, Last::Explain),
            (Action::FixChecks, Target::View(_, row)) => Offer::FixChecks(row),
            (Action::NewView, Target::View(_, row)) => Offer::NewView(row),
            (Action::NewWelcome, Target::View(_, row)) => Offer::NewWelcome(row),
            (Action::NewWorktree, _) => Offer::NewWorktree(self.project()?.id()),
            (Action::NewProject, _) => Offer::NewProject,
            (Action::NewMachine, _) => Offer::NewMachine,
            (Action::RenameView, Target::View(_, ViewRow { terminal: Some(terminal), .. })) => {
                Offer::Rename(Renames::Terminal(*terminal))
            },
            (Action::RenameWorktree, Target::Worktree(_, row)) => {
                Offer::Rename(Renames::Worktree(row.place.worktree))
            },
            (Action::RenameProject, Target::Project(_, ProjectTarget { project, .. })) => {
                Offer::Rename(Renames::Project(project.id()))
            },
            (Action::CloseView, Target::View(_, row)) => Offer::CloseView(row),
            (Action::CloseWorktree, Target::Worktree(_, row)) => Offer::CloseWorktree(row),
            (Action::CloseProject, Target::Project(_, target)) => {
                Offer::CloseProject(target.project.listed())
            },
            (Action::ForgetMachine, Target::Machine(Machine::Remote(destination))) => {
                Offer::ForgetMachine(destination)
            },
            (Action::DeleteMerged, _) => Offer::DeleteMerged(self.merged_in()?),
            (Action::Rebase, Target::View(_, row)) if row.repository.is_some() => {
                Offer::Rebase(Rebases::View(row, row.branch.as_deref()?))
            },
            (Action::Rebase, Target::Worktree(_, row)) if row.repository.is_some() => {
                Offer::Rebase(Rebases::Worktree(row, row.branch.as_deref()?))
            },
            (Action::BriefEvery, _) => Offer::Brief(brief::Scope::Every),
            (Action::BriefThis, _) => Offer::Brief(brief::Scope::Session),
            (Action::BriefHighlighted, _) => Offer::Brief(brief::Scope::Highlighted),
            (Action::Ask, _) => Offer::Ask,
            (Action::FileIssue, _) => Offer::FileIssue(self.repository()?),
            (Action::Quit, _) => Offer::Quit,
            _ => return None,
        };
        Some(offer)
    }

    pub fn project(&self) -> Option<ProjectRow> {
        match self {
            Target::Project(_, target) => Some(target.project.clone()),
            Target::Worktree(_, row) => Some(row.place.project.clone()),
            Target::View(_, row) => Some(ProjectRow::of(row.session.clone())),
            Target::Machine(_) => None,
        }
    }

    pub fn cwd(&self) -> Option<&Directory> {
        match self {
            Target::Project(..) | Target::Machine(_) => None,
            Target::Worktree(_, row) => Some(&row.folder),
            Target::View(_, row) => row.cwd.at(),
        }
    }

    fn held_repository(&self) -> Option<&str> {
        match self {
            Target::View(_, row) => row.repository.as_deref(),
            Target::Worktree(_, row) => row.repository.as_deref(),
            Target::Project(..) | Target::Machine(_) => None,
        }
    }

    fn repository_name(&self) -> &str {
        self.held_repository().map_or("", prunes::name_of)
    }

    fn repository(&self) -> Option<Repository> {
        let (on, dir, home) = (self.machine(), self.cwd()?.clone(), self.held_repository()?);
        let (home, name) = (home.to_owned(), self.repository_name().to_owned());
        Some(Repository { on: on.clone(), dir, home, name })
    }

    fn merged_in(&self) -> Option<Directory> {
        self.held_repository()?;
        self.cwd().cloned()
    }

    fn folder_hint(&self) -> String {
        let held = match self {
            Target::View(..) | Target::Worktree(..) => self.cwd().map(Directory::as_str),
            Target::Project(_, project) => Some(project.dir.as_str()),
            Target::Machine(_) => None,
        };
        held.map_or_else(
            || match self.machine() {
                Machine::Local => own_folder(),
                Machine::Remote(_) => "~".to_owned(),
            },
            str::to_owned,
        )
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Target::Project(..) | Target::Machine(_) => None,
            Target::Worktree(_, row) => row.branch.as_deref(),
            Target::View(_, row) => row.branch.as_deref(),
        }
    }

    fn brief(&self, scope: brief::Scope) -> Briefed {
        match (self, scope) {
            (_, brief::Scope::Every) => Briefed::Every,
            (Target::Machine(machine), brief::Scope::Session | brief::Scope::Highlighted) => {
                Briefed::Machine(machine.clone())
            },
            (Target::View(machine, row), brief::Scope::Highlighted) => {
                Briefed::Agent(machine.clone(), row.session.name().clone(), row.view.terminal_id())
            },
            (
                Target::Worktree(machine, WorktreeRow { place, .. }),
                brief::Scope::Session | brief::Scope::Highlighted,
            ) => Briefed::Session(machine.clone(), place.project.listed().name().clone()),
            (
                Target::Project(machine, ProjectTarget { project, .. }),
                brief::Scope::Session | brief::Scope::Highlighted,
            ) => Briefed::Session(machine.clone(), project.listed().name().clone()),
            (Target::View(machine, ViewRow { session, .. }), brief::Scope::Session) => {
                Briefed::Session(machine.clone(), session.name().clone())
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Sidebar,
    Views,
}

#[must_use]
pub fn spelled(action: Action, origin: Origin) -> String {
    let (code, modifiers) = actions::MENU;
    let menu = Label::Key(code, modifiers);
    match (action.key(), origin) {
        (Key::Menu(keys) | Key::RowAndMenu(keys), Origin::Views) => {
            format!("{menu} {}", typed_path(keys))
        },
        (Key::Menu(keys @ [first, ..]), Origin::Sidebar) if Top::of(*first).is_some() => {
            format!("m {}", typed_path(keys))
        },
        (Key::Menu(keys) | Key::RowAndMenu(keys) | Key::Row(keys), _) => typed_path(keys),
        (Key::Pasted(key), _) => key.to_string(),
        (Key::Chord((code, modifiers)), _) => Label::Key(code, modifiers).to_string(),
    }
}

#[must_use]
pub fn whence(work: &Work) -> String {
    match work {
        Work::Link(link) => link.repository(),
        Work::File(file) => sidebar::short_cwd(file.path()),
    }
}

fn typed_path(path: &[char]) -> String {
    path.iter().map(char::to_string).collect::<Vec<_>>().join(" ")
}

fn started(keys: &Keys, row: &ViewRow, skill: &str) -> Result<Wants, String> {
    let Beside { session, view, .. } = row.beside()?;
    let argv = agents::on_skill(keys.agent.harness(), skill, None);
    let opens = Opens::Command(argv.map_err(|why| format!("menu: {skill}: {why}"))?);
    Ok(Wants::Open { session, view, opens })
}

fn unstarted(what: impl std::fmt::Display, why: &str) -> Did {
    Did::Closed(Done::Wrong(format!("menu: {what}: {why}")))
}

const ASKS_FIRST: [Action; 4] =
    [Action::CloseView, Action::CloseWorktree, Action::CloseProject, Action::DeleteMerged];

const _: () = {
    let mut at = 0;
    while at < ASKS_FIRST.len() {
        let in_menu = matches!(ASKS_FIRST[at].key(), Key::Menu([_, ..]));
        assert!(in_menu, "an action that asks first is said yes to with its menu key");
        at += 1;
    }
};

fn yes_key(action: Action) -> char {
    assert!(ASKS_FIRST.contains(&action), "{action:?} asks no question");
    let Key::Menu([.., last]) = action.key() else {
        unreachable!("{action:?} asks first, and so is in the menu");
    };
    *last
}

struct Beside {
    session: Listed,
    view: ViewKey,
    cwd: Directory,
}

enum Rebases<'t> {
    View(&'t ViewRow, &'t str),
    Worktree(&'t WorktreeRow, &'t str),
}

enum Resolves {
    Beside(Beside),
    In(WorktreeId),
}

impl Rebases<'_> {
    fn asked(&self) -> Result<(Resolves, Directory, String), String> {
        match self {
            Rebases::View(row, branch) => {
                let beside = row.beside()?;
                let dir = beside.cwd.clone();
                Ok((Resolves::Beside(beside), dir, (*branch).to_owned()))
            },
            Rebases::Worktree(row, branch) => {
                Ok((Resolves::In(row.place.worktree), row.folder.clone(), (*branch).to_owned()))
            },
        }
    }
}

impl ViewRow {
    fn beside(&self) -> Result<Beside, String> {
        let view = self.view;
        let cwd = match &self.cwd {
            Cwd::At(cwd) => cwd.clone(),
            Cwd::Unsaid => {
                return Err(format!(
                    "menu: luneta does not know yet where view {view} works; try again in a moment"
                ));
            },
            Cwd::Unread { why } => return Err(format!("menu: where view {view} works, {why}")),
        };
        Ok(Beside { session: self.session.clone(), view, cwd })
    }

    fn pulled(&self) -> Result<(Beside, PullPage), String> {
        let beside = self.beside()?;
        let view = beside.view;
        let Some(PullRequest { number, state, url, .. }) = &self.pull_request else {
            return Err(format!("menu: gh has found no pull request of view {view}'s branch"));
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
                "menu: view {view}'s lunetad does not say the page of #{number}, as an older \
                 one does not"
            )),
        }
    }

    fn ended(&self) -> Result<(Beside, Ended), String> {
        let beside = self.beside()?;
        let view = beside.view;
        match &self.ran {
            Ran::Ended(ended) => Ok((beside, ended.clone())),
            Ran::Going(Some(command)) => Err(format!(
                "menu: view {view} runs `{}` still, and what it prints is not over",
                command.as_str()
            )),
            Ran::Going(None) => Err(format!(
                "menu: view {view} runs a command still, and what it prints is not over"
            )),
            Ran::Stopped(Some(command)) => Err(format!(
                "menu: `{}` is stopped in view {view}, and not over; bring it back with `fg`",
                command.as_str()
            )),
            Ran::Stopped(None) => Err(format!(
                "menu: a job is stopped in view {view}, and not over; bring it back with `fg`"
            )),
            Ran::EndedUnsaid => Err(format!(
                "menu: view {view}'s shell said no line of the command it ran last, by which \
                 luneta finds what it printed"
            )),
            Ran::Unknown => Err(format!(
                "menu: what view {view}'s shell ran is in words this luneta cannot read, as a \
                 newer lunetad's"
            )),
            Ran::Ready => Err(format!("menu: view {view}'s shell has run nothing yet")),
            Ran::Unhooked => Err(format!(
                "menu: view {view}'s shell marks none of the commands it runs; a shell that \
                 writes OSC 133, as fish 4 does, marks them"
            )),
        }
    }
}

#[expect(clippy::large_enum_variant, reason = "it lasts a key; a box would allocate on each")]
pub enum Did {
    Stays(Menu, Wants),
    Closed(Done),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Wants {
    Nothing,
    Cloned(ProjectId),
    Keep(Harness),
    Skills { dir: Directory, agent: Harness },
    Refind { dir: Directory, agent: Harness },
    Plan { session: Listed, scope: Scope },
    Prunes { dir: Directory },
    Prune { dir: Directory, agreed: BTreeSet<prunes::Deleting> },
    Rebase { dir: Directory, branch: String },
    Open { session: Listed, view: ViewKey, opens: Opens },
    Pull(Work, Wanted),
    Printed { session: Listed, view: ViewKey, ended: Ended },
    Checks { page: PullPage },
    Make { cwd: Directory, name: String, agent: Option<Argv> },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Done {
    Nothing,
    Wrong(String),
    Go(Machine, Place),
    Act(Machine, Act),
    Brief(Briefed),
    Ask(Target),
    Issue(Repository),
    Ssh(Destination),
    Forget(Destination),
    Worktree(Machine, ProjectId, Branch),
    Welcome { on: Machine, session: Listed, view: ViewKey },
    OpenIn { on: Machine, worktree: WorktreeId, command: Argv },
    Said(String),
    Project { on: Machine, folder: String, name: Option<projects::Name> },
    Rename(Machine, Rename),
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Briefed {
    Every,
    Machine(Machine),
    Session(Machine, SessionName),
    Agent(Machine, SessionName, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Key(KeyEvent),
    Skill(Name),
}

pub struct Menu {
    number: u64,
    target: Target,
    stage: Stage,
}

enum Stage {
    Choosing(Keys),
    Planning { keys: Keys, close: Close, agreed: Option<Plan> },
    Asking { keys: Keys, close: Close, plan: Plan, asks: String },
    Sweeping { keys: Keys, dir: Directory },
    Swept { keys: Keys, dir: Directory, plan: prunes::Plan },
    Pruning { keys: Keys, dir: Directory, count: usize },
    Opening { keys: Keys },
    Reading { keys: Keys, last: Last, beside: Beside, ended: Ended },
    Fetching { keys: Keys, beside: Beside, page: PullPage },
    Rebasing { keys: Keys, resolves: Resolves, branch: String },
    Pull { work: Work, agent: Harness, step: Step },
    Typing { keys: Keys, chain: Chain },
}

enum Step {
    Choosing { wrong: Option<String> },
    Waiting(Wait),
}

enum Wait {
    Pulling(Pull),
    Opening,
    Unopened,
}

impl Wait {
    fn doing(&self, work: &Work) -> String {
        match self {
            Wait::Pulling(pull) => match pull.wanted(Asks::of(work)) {
                Wanted::AtHead => format!("Syncing {}…", work.tab()),
                Wanted::AsItIs => format!("Making a worktree of {}…", work.tab()),
            },
            Wait::Opening | Wait::Unopened => format!("Opening {}…", work.tab()),
        }
    }
}

impl Menu {
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

    pub fn new(number: u64, target: Target, agent: Picked) -> Menu {
        let keys = Keys::new(&target, agent);
        Menu { number, target, stage: Stage::Choosing(keys) }
    }

    pub fn acting(number: u64, target: &Target, action: Action, agent: Picked) -> Did {
        let offer = target.offer(action).expect("the palette lists only actions its target fits");
        Menu::act(number, target, Keys::new(target, agent), offer)
    }

    pub fn pull(number: u64, target: Target, work: Work, agent: Harness) -> Menu {
        let step = Step::Choosing { wrong: None };
        Menu { number, target, stage: Stage::Pull { work, agent, step } }
    }

    pub fn start_pull(number: u64, target: Target, work: Work, agent: Harness, pull: Pull) -> Did {
        let wants = Wants::Pull(work.clone(), pull.wanted(Asks::of(&work)));
        let stage = Stage::Pull { work, agent, step: Step::Waiting(Wait::Pulling(pull)) };
        Did::Stays(Menu { number, target, stage }, wants)
    }

    pub fn number(&self) -> u64 {
        self.number
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    pub fn sweeping(&self) -> bool {
        matches!(self.stage, Stage::Sweeping { .. })
    }

    pub fn pruning(&self) -> bool {
        matches!(self.stage, Stage::Pruning { .. })
    }

    pub fn reading(&self) -> bool {
        matches!(self.stage, Stage::Reading { .. })
    }

    pub fn fetching(&self) -> bool {
        matches!(self.stage, Stage::Fetching { .. })
    }

    pub fn rebasing(&self) -> bool {
        matches!(self.stage, Stage::Rebasing { .. })
    }

    pub fn pulling(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Waiting(Wait::Pulling(_)), .. })
    }

    pub fn waits(&self) -> bool {
        matches!(
            self.stage,
            Stage::Sweeping { .. }
                | Stage::Pruning { .. }
                | Stage::Reading { .. }
                | Stage::Fetching { .. }
                | Stage::Rebasing { .. }
                | Stage::Opening { .. }
                | Stage::Pull { step: Step::Waiting(_), .. }
        )
    }

    pub fn doing(&self) -> Option<String> {
        match &self.stage {
            Stage::Sweeping { .. } => Some(sweeping(self.target.repository_name())),
            Stage::Pruning { count, .. } => Some(pruning(*count)),
            Stage::Reading { ended, .. } => Some(reading(ended)),
            Stage::Fetching { page, .. } => Some(fetching(page.number())),
            Stage::Rebasing { branch, .. } => Some(rebasing(branch)),
            Stage::Pull { work, step: Step::Waiting(wait), .. } => Some(wait.doing(work)),
            Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Swept { .. }
            | Stage::Opening { .. }
            | Stage::Pull { step: Step::Choosing { .. }, .. }
            | Stage::Typing { .. } => None,
        }
    }

    pub fn opens_pulled(&self) -> bool {
        matches!(self.stage, Stage::Pull { step: Step::Waiting(Wait::Unopened), .. })
    }

    pub fn making(&self) -> Option<&Work> {
        match &self.stage {
            Stage::Pull { work, step: Step::Waiting(Wait::Pulling(_)), .. } => Some(work),
            Stage::Pull {
                step: Step::Choosing { .. } | Step::Waiting(Wait::Opening | Wait::Unopened),
                ..
            }
            | Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Rebasing { .. }
            | Stage::Typing { .. } => None,
        }
    }

    pub fn left(&self) -> Option<String> {
        match &self.stage {
            Stage::Pull { work, step: Step::Choosing { wrong: Some(wrong) }, .. } => {
                Some(format!("menu: {wrong} once {} is pasted again", work.tab()))
            },
            Stage::Swept { .. } => Some(
                "menu: nothing deleted: the menu closed before you said yes; d m asks again"
                    .to_owned(),
            ),
            Stage::Pull { step: Step::Choosing { wrong: None } | Step::Waiting(_), .. }
            | Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Rebasing { .. }
            | Stage::Typing { .. } => None,
        }
    }

    pub fn paste(&mut self, text: &str) -> bool {
        let Stage::Typing { chain, .. } = &mut self.stage else {
            return false;
        };
        chain.paste(text);
        true
    }

    fn act(number: u64, target: &Target, mut keys: Keys, offer: Offer<'_>) -> Did {
        let stays = |stage: Stage, wants: Wants| {
            Did::Stays(Menu { number, target: target.clone(), stage }, wants)
        };
        let chained = Chain::offered(target, &offer);
        let closing = |keys: Keys, session: &Listed, scope: Scope| {
            let session = session.clone();
            let wants = Wants::Plan { session: session.clone(), scope };
            let close = Close { session, scope };
            stays(Stage::Planning { keys, close, agreed: None }, wants)
        };
        let wrong = |reason: String| Did::Closed(Done::Wrong(reason));
        match offer {
            Offer::SearchSkills(row) => match row.beside() {
                Ok(Beside { cwd: dir, .. }) => {
                    keys.search = Some(Search::new(dir.clone()));
                    let wants = Wants::Skills { dir, agent: keys.agent.harness() };
                    stays(Stage::Choosing(keys), wants)
                },
                Err(reason) => wrong(reason),
            },
            Offer::Skill(row, skill) => match started(&keys, row, skill) {
                Ok(wants) => stays(Stage::Opening { keys }, wants),
                Err(reason) => wrong(reason),
            },
            Offer::Last(row, last) => match row.ended() {
                Ok((beside, ended)) => {
                    let (session, view) = (beside.session.clone(), beside.view);
                    let wants = Wants::Printed { session, view, ended: ended.clone() };
                    stays(Stage::Reading { keys, last, beside, ended }, wants)
                },
                Err(reason) => wrong(reason),
            },
            Offer::FixChecks(row) => match row.pulled() {
                Ok((beside, page)) => {
                    let wants = Wants::Checks { page: page.clone() };
                    stays(Stage::Fetching { keys, beside, page }, wants)
                },
                Err(reason) => wrong(reason),
            },
            Offer::NewView(row) => match row.beside() {
                Ok(Beside { session, view, .. }) => {
                    let opens = Opens::Shell;
                    let stage = Stage::Opening { keys };
                    stays(stage, Wants::Open { session, view, opens })
                },
                Err(reason) => wrong(reason),
            },
            Offer::NewWelcome(row) => {
                let on = target.machine().clone();
                Did::Closed(Done::Welcome { on, session: row.session.clone(), view: row.view })
            },
            Offer::NewWorktree(project) => {
                let chain = chained.expect("a new worktree is typed in a chain");
                stays(Stage::Typing { keys, chain }, Wants::Cloned(project))
            },
            Offer::NewProject | Offer::NewMachine | Offer::Rename(_) => {
                let chain = chained.expect("a project, a machine and a name are typed in a chain");
                stays(Stage::Typing { keys, chain }, Wants::Nothing)
            },
            Offer::CloseView(row) => {
                closing(keys, &row.session, Scope::View(row.view))
            },
            Offer::CloseWorktree(row) => {
                let scope = Scope::Worktree(row.place.worktree);
                closing(keys, row.place.project.listed(), scope)
            },
            Offer::CloseProject(listed) => {
                closing(keys, listed, Scope::Project)
            },
            Offer::ForgetMachine(destination) => Did::Closed(Done::Forget(destination.clone())),
            Offer::DeleteMerged(dir) => {
                let wants = Wants::Prunes { dir: dir.clone() };
                stays(Stage::Sweeping { keys, dir }, wants)
            },
            Offer::Rebase(rebases) => match rebases.asked() {
                Ok((resolves, dir, branch)) => {
                    let wants = Wants::Rebase { dir, branch: branch.clone() };
                    stays(Stage::Rebasing { keys, resolves, branch }, wants)
                },
                Err(reason) => wrong(reason),
            },
            Offer::Brief(scope) => Did::Closed(Done::Brief(target.brief(scope))),
            Offer::Ask => Did::Closed(Done::Ask(target.clone())),
            Offer::FileIssue(repository) => Did::Closed(Done::Issue(repository)),
            Offer::Quit => Did::Closed(Done::Quit),
        }
    }

    pub fn key(self, key: &KeyEvent) -> Did {
        let Menu { number, target, stage } = self;
        let stays = |stage: Stage, wants: Wants| {
            Did::Stays(Menu { number, target: target.clone(), stage }, wants)
        };
        match stage {
            Stage::Choosing(mut keys) => match (keys.key(key), &target) {
                (Pressed::Close, _) => Did::Closed(Done::Nothing),
                (Pressed::Switched(agent), _) if let Some(search) = &mut keys.search => {
                    search.refind();
                    let wants = Wants::Refind { dir: search.dir().clone(), agent };
                    stays(Stage::Choosing(keys), wants)
                },
                (Pressed::Switched(agent), _) => stays(Stage::Choosing(keys), Wants::Keep(agent)),
                (Pressed::Moved | Pressed::Nothing, _) => {
                    stays(Stage::Choosing(keys), Wants::Nothing)
                },
                (Pressed::Start(skill), Target::View(_, row)) => {
                    match started(&keys, row, &skill) {
                        Ok(wants) => stays(Stage::Opening { keys }, wants),
                        Err(reason) => Did::Closed(Done::Wrong(reason)),
                    }
                },
                (Pressed::Start(_), _) => stays(Stage::Choosing(keys), Wants::Nothing),
                (Pressed::Act(action), _) => match target.offer(action) {
                    Some(offer) => Menu::act(number, &target, keys, offer),
                    None => stays(Stage::Choosing(keys), Wants::Nothing),
                },
            },
            Stage::Typing { keys, chain } => match chain.key(key, target.machine()) {
                Chained::Close => Did::Closed(Done::Nothing),
                Chained::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Chained::Typing(chain) => stays(Stage::Typing { keys, chain }, Wants::Nothing),
                Chained::Done(done) => Did::Closed(done),
            },
            Stage::Planning { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Rebasing { .. }
            | Stage::Pull { .. }
                if closes(key) =>
            {
                Did::Closed(Done::Nothing)
            },
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
                match Pull::pressed(key, &work) {
                    Some(pull) => Menu::start_pull(number, target.clone(), work, agent, pull),
                    None => stays(Stage::Pull { work, agent, step }, Wants::Nothing),
                }
            },
            stage @ (Stage::Planning { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Rebasing { .. }
            | Stage::Pull { .. }) => stays(stage, Wants::Nothing),
            Stage::Swept { keys, dir, plan } => match answer(key, yes_key(Action::DeleteMerged)) {
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
            Stage::Asking { keys, close, plan, asks } => match answer(key, close.key()) {
                Answer::Yes => {
                    let session = close.session.clone();
                    let wants = Wants::Plan { session, scope: close.scope };
                    stays(Stage::Planning { keys, close, agreed: Some(plan) }, wants)
                },
                Answer::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Answer::Close => Did::Closed(Done::Nothing),
                Answer::Nothing => {
                    stays(Stage::Asking { keys, close, plan, asks }, Wants::Nothing)
                },
            },
        }
    }

    pub fn planned(self, plan: Result<Plan, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Planning { keys, close, agreed } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let plan = match plan {
            Ok(plan) => plan,
            Err(reason) => return Did::Closed(Done::Wrong(reason)),
        };
        let asks = if agreed.as_ref() == Some(&plan) { None } else { plan.question() };
        if let Some(asks) = asks {
            let stage = Stage::Asking { keys, close, plan, asks };
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        }
        Did::Closed(Done::Act(target.machine().clone(), plan.act))
    }

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

    pub fn changed(self, plan: prunes::Plan) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Pruning { keys, dir, .. } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        Did::Stays(Menu { number, target, stage: Stage::Swept { keys, dir, plan } }, Wants::Nothing)
    }

    pub fn pulled(self, found: Result<Found, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Pull { work, agent, step: Step::Waiting(Wait::Pulling(pull)) } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let Found { on, ready: Ready { at, folder, branch, moved_off, work: readied } } =
            match found {
                Ok(found) => found,
                Err(why) => return Did::Closed(Done::Wrong(format!("menu: {work}: {why}"))),
            };
        let off = work.off_branch(branch.as_deref()).filter(|_| moved_off);
        if let (Pull::Agent, Some(off)) = (pull, off) {
            let wrong = Some(format!("{folder} is {off}, so no agent starts there; w opens it"));
            let stage = Stage::Pull { work, agent, step: Step::Choosing { wrong } };
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        }
        let argv = match (pull, &readied) {
            (Pull::Worktree, _) => None,
            (Pull::Agent, Readied::Link(link)) if link.kind() == Kind::Pull => {
                Some(agents::on_skill(agent, REVIEW, Some(&link.url())))
            },
            (Pull::Agent, Readied::Link(link)) => {
                Some(agents::on_words(agent, format!("{TACKLE} {}", link.url()), &[]))
            },
            (Pull::Agent, Readied::File { real }) => {
                let words = format!("{TACKLE} {real}");
                Some(agents::on_words(agent, words, &[real.as_str()]))
            },
        };
        let argv = match argv.transpose() {
            Ok(argv) => argv,
            Err(why) => return unstarted(&work, &why),
        };
        match (at, argv) {
            (At::There((session, view)), None) => {
                Did::Closed(Done::Go(on, Place::View(ViewPlace { session, view })))
            },
            (At::There((session, view)), Some(argv)) => {
                let row = ViewRow {
                    session: session.clone(),
                    view,
                    terminal: None,
                    cwd: Cwd::At(folder),
                    branch,
                    repository: None,
                    pull_request: None,
                    ran: Ran::Unhooked,
                };
                let target = Target::View(on, row);
                let stage = Stage::Pull { work, agent, step: Step::Waiting(Wait::Opening) };
                let wants = Wants::Open { session, view, opens: Opens::Command(argv) };
                Did::Stays(Menu { number, target, stage }, wants)
            },
            (At::Unopened, argv) => {
                let wants = Wants::Make { cwd: folder, name: work.tab(), agent: argv };
                let stage = Stage::Pull { work, agent, step: Step::Waiting(Wait::Unopened) };
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

    pub fn reran(&mut self, ran: Ran) {
        if let Target::View(_, row) = &mut self.target {
            row.ran = ran;
        }
    }

    pub fn printed(self, printed: Result<Printed, String>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Reading { keys, last, beside: Beside { session, view, .. }, ended } = stage
        else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let printed = match printed {
            Ok(printed) => printed,
            Err(why) => {
                return Did::Closed(Done::Wrong(format!("menu: what view {view} printed: {why}")));
            },
        };
        let still = |row: &ViewRow| matches!(&row.ran, Ran::Ended(now) if *now == ended);
        if !matches!(&target, Target::View(_, row) if still(row)) {
            return Did::Closed(Done::Wrong(format!(
                "menu: view {view}'s shell went on as luneta read what `{}` printed; ask again",
                ended.command.as_str()
            )));
        }
        let prompt = printed::prompt(&printed, last.asks(), &ended);
        let files: Vec<&str> = printed.file().into_iter().collect();
        let opens = match agents::on_words(keys.agent.harness(), prompt, &files) {
            Ok(argv) => Opens::Command(argv),
            Err(why) => return unstarted(format!("what view {view} printed"), &why),
        };
        let stage = Stage::Opening { keys };
        let wants = Wants::Open { session, view, opens };
        Did::Stays(Menu { number, target, stage }, wants)
    }

    pub fn checked(self, checks: Result<Failing, String>) -> Did {
        let Menu { number: menu, target, stage } = self;
        let Stage::Fetching { keys, beside: Beside { session, view, .. }, page } = stage else {
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
        let files = checks::files(&failing);
        let opens = match agents::on_words(keys.agent.harness(), prompt, &files) {
            Ok(argv) => Opens::Command(argv),
            Err(why) => return unstarted(format!("the checks of #{number}"), &why),
        };
        let stage = Stage::Opening { keys };
        let wants = Wants::Open { session, view, opens };
        Did::Stays(Menu { number: menu, target, stage }, wants)
    }

    pub fn rebased(self, rebased: Result<Rebased, Unrebased>) -> Did {
        let Menu { number, target, stage } = self;
        let Stage::Rebasing { keys, resolves, branch } = stage else {
            return Did::Stays(Menu { number, target, stage }, Wants::Nothing);
        };
        let rebased = match rebased {
            Ok(rebased) if rebased.branch == branch => rebased,
            Ok(Rebased { branch: other, .. }) => {
                let why = format!("menu: {branch}: lunetad rebased {other}, not {branch}");
                return Did::Closed(Done::Wrong(why));
            },
            Err(Unrebased::Refused(why)) => {
                return Did::Closed(Done::Wrong(format!("menu: {branch}: {why}")));
            },
            Err(Unrebased::InDoubt(why)) => {
                let why = format!("menu: {branch}: {}: {why}", rebase::IN_DOUBT);
                return Did::Closed(Done::Wrong(why));
            },
        };
        let Came::Stopped(stopped) = &rebased.came else {
            return Did::Closed(Done::Said(format!("menu: {}", rebase::said(&rebased))));
        };
        let prompt = rebases::prompt(&rebased.branch, &rebased.onto, stopped);
        let command = match agents::on_words(keys.agent.harness(), prompt, &[]) {
            Ok(argv) => argv,
            Err(why) => return unstarted(&branch, &why),
        };
        match resolves {
            Resolves::Beside(Beside { session, view, .. }) => {
                let stage = Stage::Opening { keys };
                let opens = Opens::Command(command);
                let wants = Wants::Open { session, view, opens };
                Did::Stays(Menu { number, target, stage }, wants)
            },
            Resolves::In(worktree) => {
                let on = target.machine().clone();
                Did::Closed(Done::OpenIn { on, worktree, command })
            },
        }
    }

    pub fn found(&mut self, agent: Harness, skills: Result<Vec<crate::skills::Skill>, String>) {
        if let Stage::Choosing(keys) = &mut self.stage
            && keys.agent.harness() == agent
            && let Some(search) = &mut keys.search
        {
            search.fill(skills);
        }
    }

    pub fn pick(mut self, name: &Name) -> Did {
        if let Stage::Choosing(keys) = &mut self.stage
            && let Some(search) = &mut keys.search
            && search.select(name)
        {
            return self.key(&KeyEvent::from(KeyCode::Enter));
        }
        Did::Stays(self, Wants::Nothing)
    }

    pub fn scroll(&mut self, by: isize) {
        if let Stage::Choosing(keys) = &mut self.stage
            && let Some(search) = &mut keys.search
        {
            search.move_by(by);
        }
    }

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

    pub fn quiet(&self) -> bool {
        matches!(
            self.stage,
            Stage::Planning { .. }
                | Stage::Opening { .. }
                | Stage::Reading { .. }
        )
    }

    pub fn lists(&self) -> bool {
        matches!(&self.stage, Stage::Choosing(keys) if keys.search.is_none())
    }

    pub fn draw(&self, buffer: &mut Buffer, over: Rect, now: SystemTime) -> Drawn {
        let frame = sidebar::frame(now);
        let shown = |width, body_room| self.shown(width, body_room, frame);
        let floated = float(buffer, over, self.searching(), shown);
        let drawn = |(area, spots)| Drawn::Menu { number: self.number, area: Some(area), spots };
        floated.map_or(Drawn::Nothing, drawn)
    }

    pub fn turning(&self) -> bool {
        matches!(
            self.stage,
            Stage::Pull { step: Step::Waiting(_), .. }
                | Stage::Sweeping { .. }
                | Stage::Pruning { .. }
        )
    }

    fn searching(&self) -> bool {
        match &self.stage {
            Stage::Choosing(keys)
            | Stage::Planning { keys, .. }
            | Stage::Opening { keys, .. }
            | Stage::Reading { keys, .. } => keys.search.is_some(),
            Stage::Typing { .. } => true,
            Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Fetching { .. }
            | Stage::Rebasing { .. }
            | Stage::Pull { .. } => false,
        }
    }

    fn shown(&self, width: usize, body_room: usize, frame: usize) -> Shown {
        let inner = width.saturating_sub(1);
        let right = self.target.cwd().map_or_default(|cwd| sidebar::short_cwd(cwd.as_str()));
        let mut spots = Spots::default();
        let (title, right, body, (footer, keys)) = match &self.stage {
            Stage::Pull { work, agent, step } => {
                let body = match step {
                    Step::Choosing { wrong } => {
                        let entry = |(_, pasted): (Pull, Pasted)| Entry::Act {
                            key: pasted.key(),
                            action: pasted.action(),
                        };
                        let entries = Pull::offered(work).map(entry);
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
                (work.tab(), whence(work), body, keyed(width, &[(Hint::Esc, "Close")]))
            },
            Stage::Choosing(keys)
            | Stage::Planning { keys, .. }
            | Stage::Opening { keys, .. }
            | Stage::Reading { keys, .. } => {
                let choosing = matches!(self.stage, Stage::Choosing(_));
                let tab = Choice::Key(KeyEvent::from(KeyCode::Tab));
                if let Some(search) = &keys.search {
                    let moves =
                        [(Hint::Moves, "Move"), (Hint::Enter, "Start"), (Hint::Esc, "Back")];
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
            Stage::Rebasing { keys, branch, .. } => {
                let body = waiting(&rebasing(branch), width, Style::new().fg(NAME), frame);
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
            Stage::Typing { chain, .. } => {
                let typed = sidebar::tail(&format!("{}_", chain.typed()), width.saturating_sub(2));
                let mut body = vec![Line::from(vec![
                    Span::raw(" "),
                    Span::styled(typed, Style::new().fg(ACCENT)),
                ])];
                body.extend(chain_says(chain, width));
                let keys = [(Hint::Enter, chain.enter()), (Hint::Esc, "Back")];
                (chain.title().to_owned(), chain.right(), body, keyed(width, &keys))
            },
        };
        Shown { title, right, body, spots, footer, keys }
    }
}

#[must_use]
pub fn chain_says(chain: &Chain, width: usize) -> Vec<Line<'static>> {
    match chain.says() {
        Says::Hint(hint) => wrap(hint, width, Style::new().fg(NAME)),
        Says::Wrong(wrong) => wrap(wrong, width, Style::new().fg(gruvbox::RED)),
    }
}

struct Shown {
    title: String,
    right: String,
    body: Vec<Line<'static>>,
    spots: Spots<Choice>,
    footer: Line<'static>,
    keys: Spots<KeyEvent>,
}

impl Shown {
    fn takes(&self) -> usize {
        let lines = self.body.iter().chain([&self.footer]);
        let widest = lines.map(|line| line.width() + 1).max().unwrap_or(0);
        let tail = if self.right.is_empty() { 0 } else { self.right.width() + 3 };
        widest.max(4 + self.title.width() + tail)
    }

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

fn float(
    buffer: &mut Buffer,
    over: Rect,
    widest: bool,
    shown: impl Fn(usize, usize) -> Shown,
) -> Option<(Rect, Spots<Choice>)> {
    let (fewest, most) = FLOATING;
    let most = most.min(usize::from(over.width.saturating_sub(2)));
    let room = usize::from(over.height.saturating_sub(1));
    if most == 0 || room < 2 {
        return None;
    }
    let inner =
        if widest { most } else { shown(most, room - 2).takes().clamp(fewest.min(most), most) };
    let (lines, spots) = shown(inner, room - 2).lines(inner, room);
    let (rule, rest) = lines.split_first()?;
    let width = u16::try_from(inner + 2).unwrap_or(over.width);
    let height = u16::try_from(lines.len() + 1).unwrap_or(over.height);
    let area = Rect::new(over.right() - width, over.bottom() - height, width, height);
    boxed(buffer, area, rule, rest);
    let inside = Rect::new(area.x + 1, area.y, width - 2, height - 1);
    Some((area, spots.placed(inside)))
}

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

fn sweeping(repository: &str) -> String {
    format!("Asking gh about the worktrees of {repository}…")
}

fn pruning(count: usize) -> String {
    format!("Deleting {}…", merged_worktrees(count))
}

fn reading(ended: &Ended) -> String {
    format!("Reading what `{}` printed…", ended.command.as_str())
}

fn fetching(number: NonZeroU32) -> String {
    format!("Asking gh about the checks of #{number}…")
}

fn rebasing(branch: &str) -> String {
    format!("Rebasing {branch} onto upstream…")
}

pub fn waiting(doing: &str, width: usize, style: Style, frame: usize) -> Vec<Line<'static>> {
    let (spinner, turning) = Mark::Busy.glyph(frame);
    let lines = wrap(doing, width.saturating_sub(2), style).into_iter().enumerate();
    let lines = lines.map(|(at, line)| {
        let before = if at == 0 { Span::styled(spinner.clone(), turning) } else { Span::raw(" ") };
        Line::from(vec![Span::raw(" "), before, Span::styled(line.to_string(), style)])
    });
    lines.collect()
}

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

fn swept_lines(plan: &prunes::Plan, width: usize, room: usize) -> Vec<Line<'static>> {
    let style = Style::new().fg(NAME);
    let mut lines = if plan.goes.is_empty() {
        wrap("No merged worktree to delete", width, style)
    } else {
        wrap(&asked_of(plan), width, style)
    };
    lines.push(Line::default());
    let kept = plan.kept.iter().map(|kept| (&kept.folder, kept.why.as_str()));
    let unasked = plan.unasked.iter().map(|unasked| (&unasked.folder, "luneta could not ask"));
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

fn goes_lines(goes: &[prunes::Goes], width: usize, room: usize) -> Vec<Line<'static>> {
    let shown = if goes.len() <= room { goes.len() } else { room.saturating_sub(1) };
    let numbers: Vec<String> = goes.iter().map(|goes| format!("#{}", goes.number)).collect();
    let size = |goes: &prunes::Goes| goes.bytes.map_or("?".to_owned(), prune::size);
    let sizes: Vec<String> = goes.iter().map(size).collect();
    let widest = |all: &[String]| all.iter().take(shown).map(|it| it.width()).max().unwrap_or(0);
    let (numbers_wide, sizes_wide) = (widest(&numbers), widest(&sizes));
    let names: Vec<&str> = goes.iter().map(|goes| prunes::name_of(&goes.folder)).collect();
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

fn title(keys: &Keys) -> String {
    let names: Vec<&str> = keys.trail().into_iter().map(|(name, _)| name).collect();
    if names.is_empty() { TITLE.to_owned() } else { names.join(" › ") }
}

pub fn legend(
    target: Option<&Target>,
    width: usize,
    room: usize,
) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let mut entries = tree(Action::row(), &ROW_GROUPS);
    if let Some(target) = target {
        let fitting = Action::menu().filter(|(action, _)| target.fits(*action));
        let shown = |entry: &Entry| Top::of(entry.key()).is_none();
        entries.extend(tree(fitting, &MENU_GROUPS).into_iter().filter(shown));
    }
    listed(TITLE, &entries, width, room)
}

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

pub fn grouped(group: char, width: usize, room: usize) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let found = tree(Action::row(), &ROW_GROUPS).into_iter().find_map(|entry| match entry {
        Entry::Group { key, name, entries } if key == group => Some((name, entries)),
        Entry::Group { .. } | Entry::Act { .. } => None,
    });
    let (name, entries) = found.expect("a row group the sidebar opens is in the catalogue");
    listed(name, &entries, width, room)
}

pub fn in_group(group: char, key: char) -> Option<Action> {
    Action::row().find_map(|(action, path)| (*path == [group, key]).then_some(action))
}

pub fn in_menu_group(group: char, key: char) -> bool {
    Action::menu().any(|(_, path)| *path == [group, key])
}

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

#[derive(Debug, Default)]
pub struct AllKeys {
    first: usize,
    room: Cell<usize>,
}

struct KeysRow {
    keys: String,
    does: String,
}

impl KeysRow {
    fn of(action: Action, keys: String) -> KeysRow {
        KeysRow { keys, does: action.name().to_owned() }
    }
}

impl AllKeys {
    fn groups() -> [(&'static str, Vec<KeysRow>); 5] {
        let named = |keys: &[(Label, &str)]| -> Vec<KeysRow> {
            let row = |(label, does): &(Label, &str)| KeysRow {
                keys: label.to_string(),
                does: (*does).to_owned(),
            };
            keys.iter().map(row).collect()
        };
        let typed =
            |(action, path): (Action, &'static [char])| KeysRow::of(action, typed_path(path));
        let sidebar = Action::row().map(typed);
        let menu = Action::menu().map(typed);
        let mut menu: Vec<KeysRow> = menu.collect();
        menu.extend(named(&MENU_MOVES));
        let pasted = Action::pasted().map(|(action, key)| KeysRow::of(action, key.to_string()));
        let anywhere = Action::anywhere().map(|(action, (code, modifiers))| {
            KeysRow::of(action, Label::Key(code, modifiers).to_string())
        });
        [
            ("Move", named(&MOVES)),
            ("Sidebar", sidebar.collect()),
            ("Menu", menu),
            ("Pasted link", pasted.collect()),
            ("Anywhere", anywhere.collect()),
        ]
    }

    fn rows(width: usize) -> Vec<Line<'static>> {
        let inner = width.saturating_sub(1);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let mut rows = Vec::new();
        for (name, keys) in AllKeys::groups() {
            rows.push(Line::from(vec![Span::raw(" "), Span::styled(name, bold)]));
            let column = keys.iter().map(|row| row.keys.width()).max().unwrap_or(0);
            rows.extend(keys.iter().map(|row| keys_line_of(&row.keys, column, &row.does, inner)));
        }
        rows
    }

    pub fn scroll(&mut self, by: isize) {
        let last = AllKeys::rows(0).len().saturating_sub(self.room.get());
        self.first = self.first.min(last).saturating_add_signed(by).min(last);
    }

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

fn entry_spots(entries: &[Entry], width: usize) -> Spots<KeyEvent> {
    let mut spots = Spots::default();
    for (line, entry) in entries.iter().enumerate() {
        spots.add(line, 0..width, KeyEvent::from(KeyCode::Char(entry.key())));
    }
    spots
}

pub fn rule(title: &str, right: &str, width: usize) -> Line<'static> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let head = 2 + 1 + 1;
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

fn entry_line(entry: &Entry, inner: usize) -> Line<'static> {
    let key = entry.key().to_string();
    let Entry::Group { name, .. } = entry else {
        return keys_line_of(&key, key.width(), entry.does(), inner);
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

fn all_keys_line(inner: usize) -> Line<'static> {
    keys_line_of(&ALL_KEYS.to_string(), 1, ALL_KEYS_DOES, inner)
}

fn agent_line(agent: Picked, inner: usize) -> Line<'static> {
    let name = agents::name(agent.harness());
    let view = match agent {
        Picked::ByView(_) => Some(VIEW_S),
        Picked::ByYou(_) => None,
    };
    let key = Hint::Tab.label();
    let wide = |(view, does): &(Option<&str>, Option<&str>)| {
        let view = view.map_or(0, UnicodeWidthStr::width);
        let does = does.map_or(0, |does| 1 + does.width());
        icons::WIDTH + 1 + name.width() + view + GAP + key.width() + does
    };
    let does = NEXT_DOES.map(Some);
    let tries = [(view, does[0]), (view, does[1]), (view, None), (None, None)];
    let icon = icons::harness(agent.harness());
    let mut spans = vec![Span::raw(" "), Span::raw(icon), Span::raw(" ")];
    match tries.into_iter().find(|tried| wide(tried) <= inner) {
        Some((view, does)) => {
            spans.push(Span::raw(name));
            spans.extend(view.map(|view| Span::styled(view, Style::new().fg(NAME))));
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
    Moves,
    AnyKey,
    AnyOtherKey,
}

impl Hint {
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

pub fn keys_line(width: usize, keys: &[(Hint, &str)]) -> Line<'static> {
    keyed(width, keys).0
}

pub fn keyed(width: usize, keys: &[(Hint, &str)]) -> (Line<'static>, Spots<KeyEvent>) {
    let labels: Vec<String> = keys.iter().map(|(hint, _)| hint.label()).collect();
    let spelled = |sep: &str| -> Vec<String> {
        let each = labels.iter().zip(keys);
        each.map(|(label, (_, does))| format!("{label}{sep}{does}")).collect()
    };
    let candidates: [(Vec<String>, &str); 4] =
        [(spelled(" - "), ", "), (spelled(" "), "  "), (spelled(" "), " "), (labels.clone(), "/")];
    let room = width.saturating_sub(1);
    let fits = |(each, joiner): &(Vec<String>, &str)| each.join(joiner).width() <= room;
    let (each, joiner) =
        candidates.into_iter().find(fits).unwrap_or_else(|| (labels.clone(), ""));
    let dim = Style::new().add_modifier(Modifier::DIM);
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

    use lunetad::Directory;
    use ratatui::layout::Position;

    use super::*;
    use crate::actions::GIT;
    use crate::close::Doing;
    use crate::printed::Kept;
    use crate::pull::{At, Found, Link, Readied, Ready, Work};
    use crate::sidebar::Listed;
    use crate::skills::Skill;
    use crate::spots::Hit;

    const CLAUDE: Harness = Harness::ClaudeCode;
    const PICKED: Picked = Picked::ByYou(CLAUDE);

    fn listed() -> Listed {
        Listed::new_project("luneta", 1)
    }

    fn said(text: &str) -> Said {
        Said::new(text.to_owned()).unwrap()
    }

    fn row() -> ViewRow {
        ViewRow {
            session: listed(),
            view: ViewKey::terminal(4),
            terminal: None,
            cwd: Cwd::At(main_dir()),
            branch: Some("feat/menu".to_owned()),
            repository: Some("/w/luneta".to_owned()),
            pull_request: None,
            ran: Ran::Unhooked,
        }
    }

    fn view_row() -> Target {
        Target::View(Machine::Local, row())
    }

    fn project() -> ProjectRow {
        ProjectRow::of(Listed::new_project("luneta", 3))
    }

    fn terminal() -> ViewId {
        ViewId::of(9).unwrap()
    }

    fn own_row() -> ViewRow {
        let session = project().listed().clone();
        ViewRow { session, view: ViewKey::terminal(4), terminal: Some(terminal()), ..row() }
    }

    fn own_view() -> Target {
        Target::View(Machine::Local, own_row())
    }

    fn project_on(on: Machine) -> Target {
        let dir = "/w/luneta".to_owned();
        Target::Project(on, ProjectTarget { project: project(), dir })
    }

    fn session_row() -> Target {
        project_on(Machine::Local)
    }

    fn worktree() -> WorktreeId {
        WorktreeId::of(5).unwrap()
    }

    fn worktree_on(on: Machine) -> Target {
        let place = WorktreePlace { project: project(), worktree: worktree() };
        Target::Worktree(on, WorktreeRow {
            place,
            name: "main".to_owned(),
            folder: main_dir(),
            branch: Some("feat/menu".to_owned()),
            repository: Some("/w/luneta".to_owned()),
        })
    }

    fn char(key: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(key))
    }

    fn beside_row(command: Vec<String>) -> Wants {
        Wants::Open { session: listed(), view: ViewKey::terminal(4), opens: agent(command) }
    }

    fn opened(target: Target, key: Option<char>) -> Did {
        Menu::open(1, target, key, PICKED).expect("a key of the menu")
    }

    fn stays(did: Did) -> (Menu, Wants) {
        match did {
            Did::Stays(menu, wants) => (menu, wants),
            Did::Closed(done) => panic!("the menu closed, leaving {done:?}"),
        }
    }

    fn closed(did: Did) -> Done {
        match did {
            Did::Closed(done) => done,
            Did::Stays(_, wants) => panic!("the menu stays open, wanting {wants:?}"),
        }
    }

    fn text(lines: &[Line]) -> Vec<String> {
        lines.iter().map(ToString::to_string).collect()
    }

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
        let group = |c: &char| ROW_GROUPS.iter().any(|(key, _)| key == c);
        for c in (' '..='~').filter(|c| !group(c) && Top::moves(*c).is_some()) {
            assert!(named.contains(&c), "{c:?}");
        }
    }

    #[test]
    fn a_key_opens_its_group_and_a_key_in_it_opens_a_view_beside_the_row() {
        let (menu, wants) = stays(opened(own_view(), Some('n')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ New ─────────── luneta/main ─",
                " v  new view",
                " h  new welcome view",
                " w  new worktree…",
                " p  new project…",
                " m  new machine…",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('v')));
        let opens = Opens::Shell;
        let (session, view) = (project().listed().clone(), ViewKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, view, opens });
        let (menu, wants) = stays(menu.key(&char('v')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    #[test]
    fn n_w_asks_for_a_branch_and_makes_its_worktree_in_the_row_s_project_on_its_machine() {
        let enter = KeyEvent::from(KeyCode::Enter);
        let asked = |target: Target| {
            let (menu, wants) = stays(stays(opened(target, Some('n'))).0.key(&char('w')));
            assert_eq!(wants, Wants::Cloned(project().id()));
            menu
        };
        let made = |target: Target, typed: &str| {
            let menu = typed.chars().fold(asked(target), |menu, c| stays(menu.key(&char(c))).0);
            closed(menu.key(&enter))
        };
        let (id, here) = (project().id(), || Machine::Local);
        let branch = || Branch::parse("feat/x").unwrap();
        assert_eq!(made(own_view(), "feat/x"), Done::Worktree(here(), id, branch()));
        assert_eq!(made(worktree_on(here()), " feat/x "), Done::Worktree(here(), id, branch()));
        assert_eq!(made(session_row(), "feat/x"), Done::Worktree(here(), id, branch()));
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let far = Target::View(vps(), own_row());
        assert_eq!(made(far, "feat/x"), Done::Worktree(vps(), id, branch()));
        assert_eq!(made(worktree_on(vps()), "feat/x"), Done::Worktree(vps(), id, branch()));
        assert_eq!(made(project_on(vps()), "feat/x"), Done::Worktree(vps(), id, branch()));
        let menu = asked(own_view());
        assert_eq!(text(&menu.lines(31, 9, 0).0)[..3], [
            "─ New worktree ────────────────",
            " _",
            " its branch: one git has is",
        ]);
        let refused = |typed: &str| {
            let menu = typed.chars().fold(asked(own_view()), |menu, c| stays(menu.key(&char(c))).0);
            let (menu, _) = stays(menu.key(&enter));
            let Stage::Typing { chain: Chain { wrong: Some(wrong), .. }, .. } = &menu.stage else {
                panic!("{typed:?} keeps the box open, saying why");
            };
            (wrong.clone(), menu)
        };
        assert_eq!(refused(" ").0, "it names no branch");
        let (wrong, menu) = refused("#12");
        assert_eq!(wrong, "#12 names an issue or a pull request: paste its link");
        let back = stays(menu.key(&KeyEvent::from(KeyCode::Esc))).0;
        assert!(matches!(back.stage, Stage::Choosing(_)), "Esc goes back to the keys");
    }

    #[test]
    fn the_first_group_has_luneta_s_keys_and_backspace_goes_back_to_it() {
        let (menu, _) = stays(opened(own_view(), None));
        assert_eq!(
            text(&menu.lines(31, 12, 0).0),
            [
                "─ luneta ──────── luneta/main ─",
                " /  Search skills",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " r  Rename the view…",
                " d  Close ▸",
                " g  Git ▸",
                " b  Brief ▸",
                " q  Quit luneta",
                " <ESC> - Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[0], "─ luneta ──────── luneta/main ─");
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Backspace))), Done::Nothing);
        assert!(Menu::open(1, view_row(), Some('x'), PICKED).is_none());
    }

    #[test]
    fn an_own_session_s_view_opens_its_agents_beside_it() {
        let (_, wants) = stays(stays(opened(own_view(), Some('c'))).0.key(&char('t')));
        let opens = agent(["claude", "/thermo-nuclear-review"].map(str::to_owned).to_vec());
        let (session, view) = (project().listed().clone(), ViewKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, view, opens });
    }

    #[test]
    fn the_legend_lists_the_sidebar_s_commands_then_the_menu_s_groups_for_the_row() {
        assert_eq!(
            text(&legend(Some(&own_view()), 31, 15).0),
            [
                "─ luneta ──────────────────────",
                " /  Search",
                " u  Unread",
                " v  Reorder",
                " o  Open ▸",
                " y  Yank ▸",
                " q  Quit luneta",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " r  Rename the view…",
                " d  Close ▸",
                " b  Brief ▸",
            ]
        );
        assert_eq!(text(&legend(Some(&project_on(Machine::Local)), 31, 15).0)[7..], [
            " a  Ask",
            " n  New ▸",
            " r  Rename the project…",
            " d  Close ▸",
            " b  Brief ▸",
        ]);
        assert_eq!(text(&legend(None, 31, 15).0).len(), 7);
        assert_eq!(Top::of('m'), Some(Top::Menu));
        assert_eq!(Top::of('/'), Some(Top::Command(Command::Search)));
        assert_eq!(Top::of('u'), Some(Top::Command(Command::Unread)));
        assert_eq!(Top::of('o'), Some(Top::Group(OPEN)));
        assert_eq!(in_group(OPEN, 'p'), Some(Action::OpenPull));
        assert_eq!(in_group(OPEN, 'w'), Some(Action::OpenWorktree));
        assert_eq!(in_group(YANK, 'p'), Some(Action::YankPull));
        assert_eq!(in_group(YANK, 'w'), None);
        assert_eq!(Top::of('V'), None);
        assert_eq!(Top::of('y'), Some(Top::Group(YANK)));
        assert_eq!(Top::of('a'), None);
        assert_eq!(Top::of('c'), None);
        assert_eq!(Top::of('n'), None);
        assert_eq!(Top::of('r'), None);
        assert_eq!(Top::of('d'), None);
        assert_eq!(Top::of('b'), None);
    }

    #[test]
    fn no_command_of_the_rows_nor_group_of_the_menu_is_on_a_key_the_sidebar_moves_with() {
        for (action, path) in Action::row().chain(Action::menu()) {
            let group = ROW_GROUPS.iter().any(|(key, _)| *key == path[0]);
            let handed_on = path[0] == GIT && Top::moves(GIT) == Some(Top::First);
            assert!(Top::moves(path[0]).is_none() || group || handed_on, "{action:?} on {path:?}");
        }
        assert!(!in_menu_group(GIT, GIT), "g g goes to the first row, and to no git action");
        assert!(in_menu_group(GIT, 'r'));
        for (_, path) in Action::row() {
            let group = |(key, _): &(char, &str)| *key == path[0];
            assert!(!MENU_GROUPS.iter().any(group), "{path:?} opens a group of the menu");
        }
    }

    #[test]
    fn each_action_of_the_menu_is_offered_on_some_row() {
        let vps = Machine::Remote("me@vps".parse().unwrap());
        let rows = [
            own_view(),
            view_row(),
            worktree_on(Machine::Local),
            project_on(Machine::Local),
            Target::Machine(vps),
        ];
        for (action, _) in Action::menu() {
            assert!(rows.iter().any(|row| row.offer(action).is_some()), "{action:?}");
        }
    }

    #[test]
    fn a_key_of_the_skills_group_starts_an_agent_on_its_skill_in_the_harness_named() {
        let (menu, wants) = stays(opened(view_row(), Some('c')));
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
        let (_, wants) = stays(menu.key(&char('t')));
        let command = ["claude", "/thermo-nuclear-review"].map(str::to_owned);
        assert_eq!(wants, beside_row(command.to_vec()));

        let (menu, _) = stays(opened(view_row(), Some('c')));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Keep(Harness::Pi));
        let lines = text(&menu.lines(31, 10, 0).0);
        assert_eq!([lines[0].as_str(), lines[7].as_str()], [
            "─ Skills ──────── luneta/main ─",
            " \u{f03ff} pi  <TAB> another agent",
        ]);
        let (_, wants) = stays(menu.key(&char('t')));
        let command = ["pi", "/skill:thermo-nuclear-review"].map(str::to_owned);
        assert_eq!(wants, beside_row(command.to_vec()));
    }

    #[test]
    fn the_agent_line_says_where_the_view_s_agent_picked_the_harness() {
        let pi = Picked::ByView(Harness::Pi);
        let (menu, _) = stays(Menu::open(1, view_row(), Some('c'), pi).unwrap());
        let line = |menu: &Menu, width| text(&menu.lines(width, 10, 0).0)[7].clone();
        assert_eq!(line(&menu, 38), " \u{f03ff} pi, the view's  <TAB> another agent");
        assert_eq!(line(&menu, 37), " \u{f03ff} pi, the view's  <TAB> another");
        assert_eq!(line(&menu, 31), " \u{f03ff} pi, the view's  <TAB>");
        assert_eq!(line(&menu, 23), " \u{f03ff} pi  <TAB>");
        assert_eq!(line(&menu, 11), " \u{f03ff} pi");
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Keep(CLAUDE));
        assert_eq!(line(&menu, 31), " \u{ec82} claude  <TAB> another agent");
    }

    #[test]
    fn tab_does_nothing_where_the_menu_starts_no_agent() {
        let (menu, _) = stays(opened(view_row(), None));
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Nothing);
        let (_, wants) = stays(menu.key(&char('c')));
        assert_eq!(wants, Wants::Nothing);
    }

    fn cargo_test() -> Ended {
        let at = SystemTime::UNIX_EPOCH + Duration::from_secs(130);
        Ended { command: said("cargo test"), ending: Some(Ending::Exited(101)), at }
    }

    fn ended_row() -> Target {
        Target::View(Machine::Local, ViewRow { ran: Ran::Ended(cargo_test()), ..row() })
    }

    #[test]
    fn a_key_for_the_last_command_holds_to_the_run_it_was_pressed_on() {
        let went = |ran: Ran| {
            let (menu, _) = stays(opened(ended_row(), Some('c')));
            let (mut menu, _) = stays(menu.key(&char('f')));
            menu.reran(ran);
            closed(menu.printed(Ok(Printed::Nothing)))
        };
        let why =
            "menu: view 4's shell went on as luneta read what `cargo test` printed; ask again";
        let again = Ended { at: cargo_test().at + Duration::from_secs(5), ..cargo_test() };
        for ran in [Ran::Going(Some(said("ls"))), Ran::Ended(again), Ran::Ready, Ran::Unhooked] {
            assert_eq!(went(ran), Done::Wrong(why.to_owned()));
        }

        let busy = ViewRow { ran: Ran::Going(Some(said("cargo test"))), ..row() };
        let busy = Target::View(Machine::Local, busy);
        let (mut menu, _) = stays(opened(busy, Some('c')));
        menu.reran(Ran::Ended(cargo_test()));
        let (_, wants) = stays(menu.key(&char('f')));
        let view = ViewKey::terminal(4);
        assert_eq!(wants, Wants::Printed { session: listed(), view, ended: cargo_test() });
    }

    #[test]
    fn a_key_for_the_last_command_starts_an_agent_on_what_it_printed() {
        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, wants) = stays(menu.key(&char('e')));
        let view = ViewKey::terminal(4);
        assert_eq!(wants, Wants::Printed { session: listed(), view, ended: cargo_test() });
        assert!(menu.reading());
        let (menu, wants) = stays(menu.key(&char('e')));
        assert_eq!(wants, Wants::Nothing);
        let file = "/run/luneta/printed/1.txt".to_owned();
        let kept = Kept { file, lines: 3, left_out: 0 };
        let printed = Printed::After(kept);
        let (_, wants) = stays(menu.printed(Ok(printed)));
        let prompt = "Explain this. In my shell, `cargo test` ended with exit status 101. What it \
                      printed is in /run/luneta/printed/1.txt.";
        let read = "Read(//run/luneta/printed/1.txt)";
        let command = ["claude", "--allowedTools", read, "--", prompt];
        let command = command.map(str::to_owned).to_vec();
        assert_eq!(wants, beside_row(command));

        let pi = Picked::ByView(Harness::Pi);
        let (menu, _) = stays(Menu::open(1, ended_row(), Some('c'), pi).unwrap());
        let (menu, _) = stays(menu.key(&char('e')));
        let kept = Kept { file: "/run/luneta/printed/1.txt".to_owned(), lines: 3, left_out: 0 };
        let (_, wants) = stays(menu.printed(Ok(Printed::After(kept))));
        let command = vec!["pi".to_owned(), prompt.to_owned()];
        assert_eq!(wants, beside_row(command));

        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, _) = stays(menu.key(&char('f')));
        let (_, wants) = stays(menu.printed(Ok(Printed::Nothing)));
        let prompt = "Fix this. In my shell, `cargo test` ended with exit status 101. It printed \
                      nothing.";
        let command = vec!["claude".to_owned(), prompt.to_owned()];
        assert_eq!(wants, beside_row(command));

        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, _) = stays(menu.key(&char('f')));
        let done = closed(menu.printed(Err("luneta-mux: no such terminal".to_owned())));
        let why = "menu: what view 4 printed: luneta-mux: no such terminal";
        assert_eq!(done, Done::Wrong(why.to_owned()));
    }

    fn pull_row(state: State, url: Option<&str>) -> Target {
        let pull_request = Some(PullRequest {
            number: NonZeroU32::new(328).unwrap(),
            state,
            url: url.map(str::to_owned),
            checks: None,
            stale: false,
        });
        Target::View(Machine::Local, ViewRow { pull_request, ..row() })
    }

    const PULL: &str = "https://github.com/owner/luneta/pull/328";

    fn failing(count: usize, pending: u32) -> Failing {
        let kept = Kept { file: "/run/luneta/checks/1.log".to_owned(), lines: 40, left_out: 0 };
        let failed = |name: &str, log| checks::Failed { name: name.to_owned(), link: None, log };
        let failed = [
            failed("ci / test", checks::Log::Kept(kept)),
            failed("ext", checks::Log::Unkept("no job".to_owned())),
        ];
        Failing { head: "abc".to_owned(), failed: failed[..count].to_vec(), pending, passed: 1 }
    }

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
        let (_, wants) = stays(menu.checked(Ok(failing(2, 0))));
        let prompt = checks::prompt(&PullPage::parse(PULL).unwrap(), &failing(2, 0));
        let read = "Read(//run/luneta/checks/1.log)";
        let command = ["claude", "--allowedTools", read, "--", &prompt];
        let command = command.map(str::to_owned).to_vec();
        assert_eq!(wants, beside_row(command));

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

    fn rebased(came: Came) -> Rebased {
        Rebased { branch: "feat/menu".to_owned(), onto: "origin/main".to_owned(), came }
    }

    fn on_conflicts() -> Came {
        let conflicted = rebases::Conflicted::of(vec!["menu.rs".to_owned()]);
        Came::Stopped(rebases::Stopped { at: None, conflicted })
    }

    fn rebasing(target: Target) -> Menu {
        let (menu, _) = stays(opened(target, Some(GIT)));
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Rebase { dir: main_dir(), branch: "feat/menu".to_owned() });
        assert!(menu.rebasing() && menu.waits());
        menu
    }

    #[test]
    fn g_r_rebases_a_view_s_branch_and_says_how_it_came_out() {
        let menu = rebasing(view_row());
        let body = text(&menu.lines(40, 4, 0).0);
        assert!(body.iter().any(|line| line.ends_with("Rebasing feat/menu onto upstream…")));
        let (menu, wants) = stays(menu.key(&char('r')));
        assert_eq!(wants, Wants::Nothing);
        let clean = "menu: feat/menu is rebased onto origin/main, 2 commits on top";
        let done = closed(menu.rebased(Ok(rebased(Came::Clean { ahead: 2 }))));
        assert_eq!(done, Done::Said(clean.to_owned()));
        let already = closed(rebasing(view_row()).rebased(Ok(rebased(Came::Already))));
        assert_eq!(already, Done::Said("menu: feat/menu is on origin/main already".to_owned()));
        let refused = Err(Unrebased::Refused("git fetch: no route".to_owned()));
        let failed = closed(rebasing(view_row()).rebased(refused));
        assert_eq!(failed, Done::Wrong("menu: feat/menu: git fetch: no route".to_owned()));
        let doubt = Err(Unrebased::InDoubt("me@vps: lunetad said nothing for 1260s".to_owned()));
        let doubted = closed(rebasing(view_row()).rebased(doubt));
        let wrong = "menu: feat/menu: no telling how the rebase ended: me@vps: lunetad said \
                     nothing for 1260s";
        assert_eq!(doubted, Done::Wrong(wrong.to_owned()), "no agent starts on a rebase in doubt");
        let other = Rebased { branch: "main".to_owned(), ..rebased(on_conflicts()) };
        let moved = closed(rebasing(view_row()).rebased(Ok(other)));
        let wrong = "menu: feat/menu: lunetad rebased main, not feat/menu";
        assert_eq!(moved, Done::Wrong(wrong.to_owned()), "no agent starts on another branch");
    }

    #[test]
    fn a_rebase_stopped_on_conflicts_starts_an_agent_on_them_where_it_was_asked() {
        let Rebased { came: Came::Stopped(stopped), .. } = rebased(on_conflicts()) else {
            unreachable!("on_conflicts() is a stopped rebase");
        };
        let prompt = rebases::prompt("feat/menu", "origin/main", &stopped);
        let command = vec!["claude".to_owned(), prompt];

        let (menu, wants) = stays(rebasing(view_row()).rebased(Ok(rebased(on_conflicts()))));
        assert_eq!(wants, beside_row(command.clone()));
        assert!(menu.waits());

        let worktree_row = worktree_on(Machine::Local);
        let done = closed(rebasing(worktree_row).rebased(Ok(rebased(on_conflicts()))));
        let command = Argv::parse(command).unwrap();
        assert_eq!(done, Done::OpenIn { on: Machine::Local, worktree: worktree(), command });
    }

    #[test]
    fn only_a_view_or_worktree_on_a_branch_of_a_repository_is_rebased() {
        let unbranched = ViewRow { branch: None, ..row() };
        let outside = ViewRow { repository: None, ..row() };
        for target in [
            Target::View(Machine::Local, unbranched),
            Target::View(Machine::Local, outside),
            session_row(),
            Target::Machine(Machine::Local),
        ] {
            assert!(Menu::open(1, target.clone(), Some(GIT), PICKED).is_none(), "{target:?}");
        }
        let far = Machine::Remote("me@vps".parse().unwrap());
        let (_, wants) = stays(rebasing(worktree_on(far)).key(&char('x')));
        assert_eq!(wants, Wants::Nothing);
    }

    #[test]
    fn a_key_for_the_checks_needs_an_open_pull_request() {
        let refused = |target: Target| {
            let (menu, _) = stays(opened(target, Some('c')));
            closed(menu.key(&char('c')))
        };
        let wrong = |why: &str| Done::Wrong(why.to_owned());
        let none = "menu: gh has found no pull request of view 4's branch";
        assert_eq!(refused(view_row()), wrong(none));
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
            wrong("menu: view 4's lunetad does not say the page of #328, as an older one does not")
        );
        let unsaid = Target::View(Machine::Local, ViewRow { cwd: Cwd::Unsaid, ..row() });
        assert!(matches!(refused(unsaid), Done::Wrong(_)));
    }

    #[test]
    fn a_key_for_the_last_command_needs_a_shell_at_its_prompt_after_one() {
        let refused = |target: Target| {
            let (menu, _) = stays(opened(target, Some('c')));
            match closed(menu.key(&char('f'))) {
                Done::Wrong(why) => why,
                done => panic!("it did {done:?}"),
            }
        };
        let ran = |ran| Target::View(Machine::Local, ViewRow { ran, ..row() });
        assert_eq!(
            refused(ran(Ran::Going(Some(said("make"))))),
            "menu: view 4 runs `make` still, and what it prints is not over"
        );
        assert_eq!(
            refused(ran(Ran::Going(None))),
            "menu: view 4 runs a command still, and what it prints is not over"
        );
        assert_eq!(
            refused(ran(Ran::Stopped(Some(said("vim"))))),
            "menu: `vim` is stopped in view 4, and not over; bring it back with `fg`"
        );
        assert_eq!(
            refused(ran(Ran::Stopped(None))),
            "menu: a job is stopped in view 4, and not over; bring it back with `fg`"
        );
        assert_eq!(
            refused(ran(Ran::EndedUnsaid)),
            "menu: view 4's shell said no line of the command it ran last, by which luneta \
             finds what it printed"
        );
        assert_eq!(refused(ran(Ran::Ready)), "menu: view 4's shell has run nothing yet");
        assert_eq!(
            refused(ran(Ran::Unhooked)),
            "menu: view 4's shell marks none of the commands it runs; a shell that writes OSC \
             133, as fish 4 does, marks them"
        );
        assert_eq!(
            refused(ran(Ran::Unknown)),
            "menu: what view 4's shell ran is in words this luneta cannot read, as a newer \
             lunetad's"
        );
    }

    #[test]
    fn a_opens_the_ask_box_for_the_row_the_menu_is_for() {
        for row in [view_row(), session_row()] {
            let did = Menu::open(1, row.clone(), Some('a'), PICKED).expect("the menu has `a`");
            assert_eq!(closed(did), Done::Ask(row.clone()));
            let (menu, _) = stays(opened(row.clone(), None));
            assert_eq!(closed(menu.key(&char('a'))), Done::Ask(row));
        }
    }

    #[test]
    fn i_opens_the_issue_box_for_the_repository_of_the_row() {
        let vps = Machine::Remote("vps".parse().unwrap());
        for on in [Machine::Local, vps] {
            let row = Target::View(on.clone(), row());
            let (home, name) = ("/w/luneta".to_owned(), "luneta".to_owned());
            let repository = Repository { on, dir: main_dir(), home, name };
            let did = Menu::open(1, row.clone(), Some('i'), PICKED).expect("the menu has `i`");
            assert_eq!(closed(did), Done::Issue(repository.clone()));
            let (menu, _) = stays(opened(row, None));
            assert_eq!(closed(menu.key(&char('i'))), Done::Issue(repository));
        }
        let unrepositoried = Target::View(Machine::Local, ViewRow { repository: None, ..row() });
        let nowhere = Target::View(Machine::Local, ViewRow { cwd: Cwd::Unsaid, ..row() });
        for row in [session_row(), Target::Machine(Machine::Local), unrepositoried, nowhere] {
            assert!(Menu::open(1, row.clone(), Some('i'), PICKED).is_none(), "{row:?}");
            let (menu, _) = stays(opened(row.clone(), None));
            let lines = text(&menu.lines(31, 12, 0).0);
            assert!(!lines.contains(&" i  File issue".to_owned()), "{lines:?}");
        }
    }

    #[test]
    fn the_brief_group_briefs_you_by_the_row_the_menu_is_for() {
        let (menu, _) = stays(opened(session_row(), Some('b')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Brief ───────────────────────",
                " z  Brief every session",
                " s  Brief this session",
                " h  Brief the highlighted",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let luneta = || listed().name().clone();
        for (row, key, briefed) in [
            (view_row(), 'z', Briefed::Every),
            (view_row(), 's', Briefed::Session(Machine::Local, luneta())),
            (view_row(), 'h', Briefed::Agent(Machine::Local, luneta(), 4)),
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

    #[test]
    fn q_quits_luneta_from_the_menu_of_any_row_on_any_machine() {
        let vps = Machine::Remote("me@vps".parse().unwrap());
        let rows = [
            own_view(),
            Target::View(vps.clone(), own_row()),
            worktree_on(vps),
            project_on(Machine::Local),
            local_header(),
            remote_header(),
        ];
        for row in rows {
            assert_eq!(closed(opened(row.clone(), Some('q'))), Done::Quit, "{row:?}");
        }
    }

    #[test]
    fn n_p_makes_a_project_of_the_row_s_folder_or_one_typed() {
        let typed = |menu: Menu, text: &str| {
            text.chars().fold(menu, |menu, c| stays(menu.key(&char(c))).0)
        };
        let shown = |menu: &Menu| match &menu.stage {
            Stage::Typing { chain, .. } => chain.typed.clone(),
            _ => panic!("no box to type in"),
        };
        let cleared = |menu: Menu| {
            let back = KeyEvent::from(KeyCode::Backspace);
            (0..shown(&menu).chars().count()).fold(menu, |menu, _| stays(menu.key(&back)).0)
        };
        let enter = KeyEvent::from(KeyCode::Enter);
        let asked = |target: Target| stays(stays(opened(target, Some('n'))).0.key(&char('p'))).0;
        for (target, folder) in [
            (view_row(), "/w/luneta/main".to_owned()),
            (worktree_on(Machine::Local), "/w/luneta/main".to_owned()),
            (session_row(), "/w/luneta".to_owned()),
            (local_header(), own_folder()),
            (remote_header(), "~".to_owned()),
        ] {
            let on = target.machine().clone();
            let menu = asked(target.clone());
            assert_eq!(shown(&menu), folder, "{target:?}");
            let named = closed(stays(menu.key(&enter)).0.key(&enter));
            assert_eq!(named, Done::Project { on, folder, name: None }, "{target:?}");
        }
        let menu = asked(view_row());
        assert_eq!(text(&menu.lines(31, 9, 0).0)[..3], [
            "─ New project ─────────────────",
            " /w/luneta/main_",
            " the folder it holds, on the",
        ]);
        let (menu, _) = stays(typed(cleared(menu), "w/notes").key(&enter));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[2], " not a folder: \"w/notes\" is no");
        let (menu, _) = stays(typed(cleared(menu), "~/notes").key(&enter));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[..3], [
            "─ New project ─────── ~/notes ─",
            " _",
            " its name; with none, it is",
        ]);
        let (back, _) = stays(menu.key(&KeyEvent::from(KeyCode::Esc)));
        let folder = ["─ New project ─────────────────", " ~/notes_"];
        assert_eq!(text(&back.lines(31, 9, 0).0)[..2], folder, "Esc goes back to the folder typed");
        let (menu, _) = stays(back.key(&enter));
        let (menu, _) = stays(typed(menu, "api/v2").key(&enter));
        let Stage::Typing { chain: Chain { wrong: Some(wrong), .. }, .. } = &menu.stage else {
            panic!("a project's name the sidebar cannot list keeps its box open, saying why");
        };
        assert!(wrong.contains("names no project"), "{wrong}");
        let named = closed(typed(cleared(menu), "Notes").key(&enter));
        let (folder, name) = ("~/notes".to_owned(), Some(projects::Name::parse("Notes").unwrap()));
        assert_eq!(named, Done::Project { on: Machine::Local, folder, name });
        let (menu, _) = stays(typed(cleared(asked(remote_header())), "/srv/notes").key(&enter));
        let folder = "/srv/notes".to_owned();
        let on = Machine::Remote("me@vps".parse().unwrap());
        assert_eq!(closed(menu.key(&enter)), Done::Project { on, folder, name: None });
    }

    #[test]
    fn r_renames_the_row_s_terminal_worktree_or_project() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let enter = KeyEvent::from(KeyCode::Enter);
        let renamed = |target: Target, to: &str| {
            let (menu, _) = stays(opened(target, Some('r')));
            let menu = to.chars().fold(menu, |menu, c| stays(menu.key(&char(c))).0);
            menu.key(&enter)
        };
        let logs = || projects::Name::parse("logs").unwrap();
        let to = || Some(logs());
        let done = closed(renamed(own_view(), "logs"));
        assert_eq!(done, Done::Rename(Machine::Local, Rename::Terminal(terminal(), to())));
        let done = closed(renamed(Target::View(vps(), own_row()), ""));
        assert_eq!(done, Done::Rename(vps(), Rename::Terminal(terminal(), None)));
        let done = closed(renamed(worktree_on(vps()), "logs"));
        assert_eq!(done, Done::Rename(vps(), Rename::Worktree(worktree(), to())));
        let done = closed(renamed(session_row(), "logs"));
        let project = Rename::Project(project().id(), logs());
        assert_eq!(done, Done::Rename(Machine::Local, project));
        let (menu, _) = stays(renamed(session_row(), " "));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[..3], [
            "─ Rename ──────────────────────",
            "  _",
            " a project keeps a name: type",
        ]);
        let long = "x".repeat(projects::NAME_CHARS_MAX + 1);
        let (menu, _) = stays(renamed(worktree_on(Machine::Local), &long));
        let Stage::Typing { chain: Chain { typed, wrong: Some(wrong), .. }, .. } = &menu.stage
        else {
            panic!("a name too long keeps its box open, saying why");
        };
        assert_eq!(*typed, long);
        assert!(wrong.contains("longer than 64 characters"), "{wrong}");
        let (menu, _) = stays(renamed(session_row(), "api/v2"));
        let Stage::Typing { chain: Chain { wrong: Some(wrong), .. }, .. } = &menu.stage else {
            panic!("a project's name the sidebar cannot list keeps its box open, saying why");
        };
        assert!(wrong.contains("names no project"), "{wrong}");
        let api = Some(projects::Name::parse("api/v2").unwrap());
        let done = closed(renamed(worktree_on(Machine::Local), "api/v2"));
        assert_eq!(done, Done::Rename(Machine::Local, Rename::Worktree(worktree(), api)));
        let (menu, _) = stays(opened(worktree_on(Machine::Local), Some('r')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [" _", " its new name; with none, it"]);
        for target in [view_row(), local_header(), remote_header()] {
            assert!(Menu::open(1, target.clone(), Some('r'), PICKED).is_none(), "{target:?}");
        }
    }

    #[test]
    fn n_m_opens_a_box_for_a_machine_s_destination() {
        for target in [view_row(), session_row(), local_header(), remote_header()] {
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
        let (menu, _) = stays(opened(view_row(), Some('n')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(title(match &menu.stage {
            Stage::Choosing(keys) => keys,
            _ => panic!("back to the keys"),
        }), "New");
    }

    #[test]
    fn another_machine_s_row_lists_what_works_there_and_does_it_there() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let root = |target: Target| {
            let (menu, _) = stays(opened(target, None));
            text(&menu.lines(40, 12, 0).0)[1..].to_vec()
        };
        let view = Target::View(vps(), own_row());
        assert_eq!(root(view.clone()), [
            " /  Search skills",
            " a  Ask",
            " i  File issue",
            " c  Skills ▸",
            " n  New ▸",
            " r  Rename the view…",
            " d  Close ▸",
            " g  Git ▸",
            " b  Brief ▸",
            " q  Quit luneta",
            " <ESC> - Close",
        ]);
        let (_, wants) = stays(stays(opened(view.clone(), Some('c'))).0.key(&char('t')));
        let opens = agent(["claude", "/thermo-nuclear-review"].map(str::to_owned).to_vec());
        let (session, at) = (project().listed().clone(), ViewKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, view: at, opens });
        let (menu, _) = stays(opened(view.clone(), Some('n')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..6], [
            " v  new view",
            " h  new welcome view",
            " w  new worktree…",
            " p  new project…",
            " m  new machine…",
        ]);
        let (menu, _) = stays(opened(view.clone(), Some('d')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..4], [
            " d  close the view",
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);
        let (_, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::View(ViewKey::terminal(4)));
        assert_eq!(wants, Wants::Plan { session, scope });

        let (menu, _) = stays(opened(worktree_on(vps()), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Worktree(worktree()));
        assert_eq!(wants, Wants::Plan { session, scope });
        let done = closed(menu.planned(Ok(worktree_plan(Vec::new()))));
        assert_eq!(done, Done::Act(vps(), worktree_act()));

        let name = || project().listed().name().clone();
        for (row, highlighted) in [
            (view, Briefed::Agent(vps(), name(), 4)),
            (project_on(vps()), Briefed::Session(vps(), name())),
        ] {
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
            folder: format!("/w/luneta/{name}"),
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

    #[test]
    fn the_merged_worktrees_key_is_on_a_row_in_a_repository_alone() {
        let (menu, _) = stays(opened(view_row(), Some('d')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Close ───────── luneta/main ─",
                " d  close the view",
                " m  delete merged worktrees",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('m')));
        assert_eq!(wants, Wants::Prunes { dir: main_dir() });
        assert!(menu.sweeping() && menu.turning());

        let outside = Target::View(Machine::Local, ViewRow { repository: None, ..row() });
        let unplaced = Target::View(Machine::Local, ViewRow { cwd: Cwd::Unsaid, ..row() });
        for target in [outside, unplaced, project_on(Machine::Local), remote_header()] {
            let (menu, _) = stays(opened(target.clone(), Some('d')));
            let (menu, wants) = stays(menu.key(&char('m')));
            assert_eq!(wants, Wants::Nothing, "{target:?}");
            assert!(!menu.sweeping(), "{target:?}");
        }
    }

    #[test]
    fn the_list_says_what_goes_as_far_as_there_is_room_and_why_the_rest_stay() {
        let goes = [("issue-282", 297, 4608), ("dev-check", 293, 916), ("dev-ci", 303, 6)];
        let mut plan = merged_plan(&goes);
        plan.kept.push(prunes::Kept {
            folder: "/w/luneta/issue-308".to_owned(),
            number: 310,
            why: "it has a view".to_owned(),
        });
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, wants) = stays(menu.swept(Ok(plan)));
        assert_eq!(wants, Wants::Nothing);
        assert!(!menu.turning());
        assert_eq!(
            text(&menu.lines(33, 20, 0).0),
            [
                "─ Close ──────────────── luneta ─",
                " Delete 3 merged worktrees?",
                "",
                " issue-282  #297  4.5 GB",
                " dev-check  #293  916 MB",
                " dev-ci     #303    6 MB",
                " Keeps issue-308: it has a view",
                " <y> Yes  <BS> Back  <ESC> Close",
            ]
        );
        assert_eq!(
            text(&menu.lines(33, 7, 0).0),
            [
                "─ Close ──────────────── luneta ─",
                " Delete 3 merged worktrees?",
                "",
                " issue-282  #297  4.5 GB",
                " …and 2 more",
                " Keeps issue-308: it has a view",
                " <y> Yes  <BS> Back  <ESC> Close",
            ]
        );
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let mut plan = merged_plan(&[]);
        for name in ["k1", "k2", "k3"] {
            plan.kept.push(prunes::Kept {
                folder: format!("/w/luneta/{name}"),
                number: 1,
                why: "it is locked".to_owned(),
            });
        }
        let (menu, _) = stays(menu.swept(Ok(plan)));
        assert_eq!(
            text(&menu.lines(33, 6, 0).0),
            [
                "─ Close ──────────────── luneta ─",
                " No merged worktree to delete",
                "",
                " Keeps k1: it is locked",
                " …and 2 more stay",
                " <BS> - Back, <ESC> - Close",
            ]
        );
    }

    #[test]
    fn the_list_goes_ahead_on_yes_with_what_it_listed_alone() {
        let (one, two) = (("one", 1, 5), ("two", 2, 5));
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        for key in ['y', 'm'] {
            let (asked, _) = stays(opened(view_row(), Some('d')));
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
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " d  close the view");
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.swept(Ok(merged_plan(&[one]))));
        let (menu, _) = stays(menu.key(&char('n')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " d  close the view");
        let (menu, _) = stays(menu.key(&char('m')));
        let (menu, _) = stays(menu.swept(Ok(merged_plan(&[one]))));
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    #[test]
    fn the_list_asks_of_the_branches_that_go_on_a_remote_too() {
        let asked = |plan: prunes::Plan| {
            let (menu, _) = stays(opened(view_row(), Some('d')));
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

    #[test]
    fn nothing_to_delete_asks_nothing() {
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let (waiting, wants) = stays(menu.key(&char('y')));
        assert_eq!(wants, Wants::Nothing);
        assert!(waiting.sweeping());
        let (listed, _) = stays(waiting.swept(Ok(merged_plan(&[]))));
        assert_eq!(
            text(&listed.lines(33, 9, 0).0),
            [
                "─ Close ──────────────── luneta ─",
                " No merged worktree to delete",
                "",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (listed, wants) = stays(listed.key(&char('y')));
        assert_eq!(wants, Wants::Nothing);
        assert!(!listed.sweeping());

        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        let wrong = closed(menu.swept(Err("lunetad does not answer".to_owned())));
        assert_eq!(wrong, Done::Wrong("menu: nothing deleted: lunetad does not answer".to_owned()));

        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('m')));
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    #[test]
    fn a_machine_s_header_has_the_menu_forget_it() {
        let (menu, _) = stays(opened(remote_header(), Some('d')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Close ───────────────────────",
                " d  forget the machine",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        assert_eq!(closed(menu.key(&char('d'))), Done::Forget("me@vps".parse().unwrap()));

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

    #[test]
    fn the_menu_lists_only_the_keys_that_work_on_its_row() {
        let at = |target: Target, group: Option<char>| {
            let (menu, _) = stays(opened(target, group));
            text(&menu.lines(40, 14, 0).0)[1..].to_vec()
        };
        assert_eq!(at(session_row(), None), [
            " a  Ask",
            " n  New ▸",
            " r  Rename the project…",
            " d  Close ▸",
            " b  Brief ▸",
            " q  Quit luneta",
            " <ESC> - Close",
        ]);
        assert_eq!(at(project_on(Machine::Local), Some('d')), [
            " d  close the project",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(session_row(), Some('n')), [
            " w  new worktree…",
            " p  new project…",
            " m  new machine…",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(worktree_on(Machine::Local), None), [
            " a  Ask",
            " i  File issue",
            " n  New ▸",
            " r  Rename the worktree…",
            " d  Close ▸",
            " g  Git ▸",
            " b  Brief ▸",
            " q  Quit luneta",
            " <ESC> - Close",
        ]);
        assert_eq!(at(worktree_on(Machine::Local), Some('d')), [
            " d  close the worktree",
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(own_view(), Some('d')), [
            " d  close the view",
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(remote_header(), None), [
            " a  Ask",
            " n  New ▸",
            " d  Close ▸",
            " b  Brief ▸",
            " q  Quit luneta",
            " <ESC> - Close",
        ]);
        assert_eq!(at(local_header(), Some('n')), [
            " p  new project…",
            " m  new machine…",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert!(Menu::open(1, local_header(), Some('d'), PICKED).is_none());

        let press = |target: &Target, group: Option<char>, key: char| {
            let (menu, _) = stays(opened(target.clone(), group));
            let (menu, wants) = stays(menu.key(&char(key)));
            (text(&menu.lines(31, 9, 0).0)[0].clone(), wants)
        };
        let first = || ("─ luneta ──────────────────────".to_owned(), Wants::Nothing);
        let new = || ("─ New ─────────────────────────".to_owned(), Wants::Nothing);
        for target in [session_row(), local_header(), remote_header()] {
            for group in ['/', 'c'] {
                let menu = Menu::open(1, target.clone(), Some(group), PICKED);
                assert!(menu.is_none(), "{group}");
                assert_eq!(press(&target, None, group), first(), "{group}");
            }
            assert_eq!(press(&target, Some('n'), 'v'), new());
        }
        assert_eq!(press(&local_header(), Some('n'), 'w'), new());
    }

    #[test]
    fn a_menu_that_cannot_open_beside_its_row_says_why() {
        let unsaid = Target::View(Machine::Local, ViewRow { cwd: Cwd::Unsaid, ..row() });
        let (menu, _) = stays(opened(unsaid, None));
        let done = closed(menu.key(&char('/')));
        assert!(matches!(done, Done::Wrong(why) if why.contains("where view 4 works")));
        let relative = ViewRow { cwd: Cwd::of(Some("w".to_owned())), ..own_row() };
        let relative = Target::View(Machine::Local, relative);
        let (menu, _) = stays(opened(relative, Some('n')));
        let why = "menu: where view 4 works, \"w\" is no absolute path".to_owned();
        assert_eq!(closed(menu.key(&char('v'))), Done::Wrong(why));
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
        let (menu, _) = stays(opened(view_row(), None));
        let (mut menu, wants) = stays(menu.key(&char('/')));
        assert_eq!(wants, Wants::Skills { dir: main_dir(), agent: CLAUDE });
        assert_eq!(
            text(&menu.lines(31, 6, 0).0)[..3],
            ["─ Skills ──────── luneta/main ─", " _", " finding skills…"]
        );
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        let lit: Vec<_> = menu.lines(31, 6, 0).0[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [Some(LIT), None]);
        assert_eq!(text(&menu.lines(31, 6, 0).0)[4], " \u{ec82} claude  <TAB> another agent");
        let (menu, _) = stays(menu.key(&char('t')));
        let (menu, _) = stays(menu.key(&char('i')));
        assert_eq!(text(&menu.lines(31, 6, 0).0)[..3], [
            "─ Skills ──────── luneta/main ─".to_owned(),
            " ti_".to_owned(),
            " /tigerstyle  tigerstyle it".to_owned(),
        ]);
        let (_, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = agents::on_skill(CLAUDE, "tigerstyle", None).unwrap().words().to_vec();
        assert_eq!(wants, beside_row(command));
    }

    #[test]
    fn tab_in_the_search_finds_the_skills_of_the_other_harness() {
        let (mut menu, _) = stays(opened(view_row(), Some('/')));
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        let (menu, _) = stays(menu.key(&char('t')));
        let (mut menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Tab)));
        assert_eq!(wants, Wants::Refind { dir: main_dir(), agent: Harness::Pi });
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
        let command = agents::on_skill(Harness::Pi, "tdd", None).unwrap().words().to_vec();
        assert_eq!(wants, beside_row(command));
    }

    #[test]
    fn a_click_on_a_skill_starts_an_agent_on_it_as_enter_does() {
        let (mut menu, _) = stays(opened(view_row(), Some('/')));
        menu.found(CLAUDE, Ok(skills(&["code-review", "tigerstyle"])));
        let skill = |name: &str| Name::new(name.to_owned()).expect("a name");
        let (_, spots) = menu.lines(31, 6, 0);
        assert_eq!(spots.at(Position::new(4, 1)), None);
        assert_eq!(spots.at(Position::new(4, 2)), Some(&Choice::Skill(skill("code-review"))));
        assert_eq!(spots.at(Position::new(30, 3)), Some(&Choice::Skill(skill("tigerstyle"))));
        menu.scroll(1);
        let lit: Vec<_> = menu.lines(31, 6, 0).0[2..4].iter().map(|line| line.style.bg).collect();
        assert_eq!(lit, [None, Some(LIT)]);
        let (menu, _) = stays(menu.key(&char('g')));
        let (menu, wants) = stays(menu.pick(&skill("code-review")));
        assert_eq!(wants, Wants::Nothing);
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        let (menu, wants) = stays(menu.pick(&skill("code-review")));
        let command = agents::on_skill(CLAUDE, "code-review", None).unwrap().words().to_vec();
        assert_eq!(wants, beside_row(command));
        assert_eq!(menu.lines(31, 6, 0).1.at(Position::new(4, 3)), None);
        let (_, wants) = stays(menu.pick(&skill("tigerstyle")));
        assert_eq!(wants, Wants::Nothing);
    }

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

    fn planned(act: Act, agents: Vec<(u32, Doing)>) -> Plan {
        let agents: Vec<_> = agents.into_iter().map(|(id, doing)| (at(id), doing)).collect();
        Plan::of(act, Ok(&agents))
    }

    fn at(id: u32) -> lunetad::mux::TerminalKey {
        ViewKey::terminal(id).key()
    }

    fn plan(agents: Vec<(u32, Doing)>) -> Plan {
        planned(Act::View(ViewKey::terminal(4)), agents)
    }

    #[test]
    fn a_close_with_no_agent_at_work_to_end_goes_ahead_at_once() {
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        let scope = Scope::View(ViewKey::terminal(4));
        assert_eq!(wants, Wants::Plan { session: listed(), scope });
        let done = closed(menu.planned(Ok(plan(vec![(4, Doing::Idle)]))));
        assert_eq!(done, Done::Act(Machine::Local, Act::View(ViewKey::terminal(4))));
        let (menu, _) = stays(opened(worktree_on(Machine::Local), Some('d')));
        let (_, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Worktree(worktree()));
        assert_eq!(wants, Wants::Plan { session, scope });
    }

    #[test]
    fn a_close_that_would_end_an_agent_at_work_asks_first() {
        let busy = || vec![(4, Doing::Busy)];
        let (menu, _) = stays(opened(view_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, wants) = stays(menu.planned(Ok(plan(busy()))));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Close ───────── luneta/main ─",
                " Close the view? 1 agent, 1",
                " busy",
                " <y> Yes <BS> Back <ESC> Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('n')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.planned(Ok(plan(busy()))));
        let (menu, wants) = stays(menu.key(&char('d')));
        let scope = Scope::View(ViewKey::terminal(4));
        assert_eq!(wants, Wants::Plan { session: listed(), scope });
        let waiting = || vec![(4, Doing::Waiting)];
        let (menu, wants) = stays(menu.planned(Ok(plan(waiting()))));
        assert_eq!(wants, Wants::Nothing);
        let asking = text(&menu.lines(31, 9, 0).0);
        assert_eq!(asking[1..3], [" Close the view? 1 agent, 1", " waiting"]);
        let (menu, _) = stays(menu.key(&char('y')));
        let act = Act::View(ViewKey::terminal(4));
        assert_eq!(closed(menu.planned(Ok(plan(waiting())))), Done::Act(Machine::Local, act));
    }

    fn worktree_plan(agents: Vec<(u32, Doing)>) -> Plan {
        planned(worktree_act(), agents)
    }

    fn worktree_act() -> Act {
        let (views, seen) = (vec![at(4), at(5)], worktree_seen());
        Act::Worktree { worktree: worktree(), views, seen }
    }

    fn worktree_seen() -> Vec<ViewId> {
        vec![ViewId::of(4).unwrap(), ViewId::of(5).unwrap()]
    }

    #[test]
    fn a_close_of_a_worktree_whose_terminals_hold_unread_files_asks_first() {
        let (menu, _) = stays(opened(worktree_on(Machine::Local), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, wants) = stays(menu.planned(Ok(worktree_plan(vec![(5, Doing::Unknown)]))));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..4], [
            " Close the worktree? 1 agent,",
            " 1 unknown",
            " <y> Yes <BS> Back <ESC> Close",
        ]);
        let (menu, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Worktree(worktree()));
        assert_eq!(wants, Wants::Plan { session, scope });
        let done = closed(menu.planned(Ok(worktree_plan(vec![(5, Doing::Unknown)]))));
        assert_eq!(done, Done::Act(Machine::Local, worktree_act()));
        let (menu, _) = stays(opened(worktree_on(Machine::Local), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let done = closed(menu.planned(Ok(worktree_plan(vec![(9, Doing::Busy)]))));
        assert_eq!(done, Done::Act(Machine::Local, worktree_act()));
    }

    #[test]
    fn d_d_on_a_project_closes_it_with_its_worktrees_always_asking_first() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let act = Act::Project {
            session: project().listed().name().clone(),
            project: project().id(),
            worktrees: vec![worktree()],
            seen: worktree_seen(),
        };
        let planned = |agents: Vec<(u32, Doing)>| planned(act.clone(), agents);
        for on in [Machine::Local, vps()] {
            let (menu, _) = stays(opened(project_on(on.clone()), Some('d')));
            let (menu, wants) = stays(menu.key(&char('d')));
            let (session, scope) = (project().listed().clone(), Scope::Project);
            assert_eq!(wants, Wants::Plan { session, scope });
            let (menu, _) = stays(menu.planned(Ok(planned(vec![(4, Doing::Busy)]))));
            assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [
                " Close project luneta? 1",
                " agent, 1 busy",
            ]);
            let (menu, _) = stays(menu.key(&char('d')));
            let done = closed(menu.planned(Ok(planned(vec![(4, Doing::Busy)]))));
            assert_eq!(done, Done::Act(on.clone(), act.clone()));
            let (menu, _) = stays(opened(project_on(on.clone()), Some('d')));
            let (menu, _) = stays(menu.key(&char('d')));
            let (menu, _) = stays(menu.planned(Ok(planned(Vec::new()))));
            let asks = text(&menu.lines(31, 9, 0).0)[1..3].to_vec();
            assert_eq!(asks, [" Close project luneta? no", " agent"]);
            let (menu, _) = stays(menu.key(&char('d')));
            assert_eq!(closed(menu.planned(Ok(planned(Vec::new())))), Done::Act(on, act.clone()));
        }
    }

    fn floated<R>(
        draw: impl FnOnce(&mut Buffer, Rect) -> R,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        const RIGHT_X: u16 = 10;
        let over = Rect::new(RIGHT_X, 0, width, height);
        let mut buffer = Buffer::empty(Rect::new(0, 0, RIGHT_X + width, height));
        for row in over.rows() {
            buffer.set_string(row.x, row.y, "z".repeat(usize::from(width)), Style::new());
        }
        draw(&mut buffer, over);
        let content = buffer.content().iter();
        for cell in content.filter(|cell| !matches!(cell.symbol(), "z" | " ")) {
            assert!(gruvbox::ALL.contains(&cell.fg), "{:?} in {:?}", cell.symbol(), cell.fg);
        }
        let row = |y| (RIGHT_X..RIGHT_X + width).map(|x| buffer[(x, y)].symbol()).collect();
        (0..height).map(row).collect()
    }

    fn drawing(menu: &Menu) -> impl FnOnce(&mut Buffer, Rect) -> Drawn + '_ {
        move |buffer, over| menu.draw(buffer, over, UNIX_EPOCH)
    }

    #[test]
    fn the_menu_floats_at_the_bottom_right_of_the_view_s_side() {
        let menu = Menu::new(1, view_row(), PICKED);
        let boxed = |inner: &str| format!("{}│{inner:<24}│", "z".repeat(24));
        assert_eq!(floated(drawing(&menu), 50, 13), [
            "z".repeat(50),
            format!("{}┌─ luneta ─ luneta/main ─┐", "z".repeat(24)),
            boxed(" /  Search skills"),
            boxed(" a  Ask"),
            boxed(" i  File issue"),
            boxed(" c  Skills ▸"),
            boxed(" n  New ▸"),
            boxed(" d  Close ▸"),
            boxed(" g  Git ▸"),
            boxed(" b  Brief ▸"),
            boxed(" q  Quit luneta"),
            boxed(" <ESC> - Close"),
            format!("{}└{}┘", "z".repeat(24), "─".repeat(24)),
        ]);

        let (menu, _) = stays(menu.key(&char('c')));
        let right = "z".repeat(7);
        let rule = format!("─ Skills {} luneta/main ─", "─".repeat(18));
        assert_eq!(floated(drawing(&menu), 50, 11)[1..], [
            format!("{right}┌{rule}┐"),
            format!("{right}│{:<41}│", " t  /thermo-nuclear-review"),
            format!("{right}│{:<41}│", " m  /make-illegal-states-unrepresentable"),
            format!("{right}│{:<41}│", " s  /tigerstyle"),
            format!("{right}│{:<41}│", " f  fix the last command"),
            format!("{right}│{:<41}│", " e  explain the last command"),
            format!("{right}│{:<41}│", " c  fix the failing checks"),
            format!("{right}│{:<41}│", " \u{ec82} claude  <TAB> another agent"),
            format!("{right}│{:<41}│", " <BS> - Back, <ESC> - Close"),
            format!("{right}└{}┘", "─".repeat(41)),
        ]);
        let narrow = floated(drawing(&menu), 30, 11);
        assert_eq!(narrow[2], format!("│{:<28}│", " t  /thermo-nuclear-review"));
    }

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

    #[test]
    fn a_click_on_an_entry_or_a_key_of_the_menu_presses_its_key() {
        let menu = Menu::new(1, view_row(), PICKED);
        assert_eq!(clicked(&menu, "Skills ▸"), Some(pressed(KeyCode::Char('c'))));
        assert_eq!(clicked(&menu, " /  Search"), Some(pressed(KeyCode::Char('/'))));
        assert_eq!(clicked(&menu, "<ESC>"), Some(pressed(KeyCode::Esc)));
        assert_eq!(clicked(&menu, "- Close"), Some(pressed(KeyCode::Esc)));
        assert_eq!(clicked(&menu, "Close ▸"), Some(pressed(KeyCode::Char('d'))));
        assert_eq!(clicked(&menu, "─ luneta"), None);

        let (menu, _) = stays(menu.key(&char('c')));
        assert_eq!(clicked(&menu, "/tigerstyle"), Some(pressed(KeyCode::Char('s'))));
        assert_eq!(clicked(&menu, "<BS> - Back"), Some(pressed(KeyCode::Backspace)));
    }

    #[test]
    fn a_click_on_a_close_asks_first_as_its_key_does() {
        let (menu, _) = stays(opened(view_row(), Some('d')));
        assert_eq!(clicked(&menu, "close the view"), Some(pressed(KeyCode::Char('d'))));
        let (menu, _) = stays(menu.key(&char('d')));
        assert_eq!(clicked(&menu, "close the view"), None);
        let busy = vec![(4, Doing::Busy)];
        let (menu, _) = stays(menu.planned(Ok(plan(busy))));
        assert_eq!(clicked(&menu, "<y>"), Some(pressed(KeyCode::Char('y'))));
        assert_eq!(clicked(&menu, "Close the view?"), None);
    }

    #[test]
    fn a_note_floats_where_the_menu_does() {
        let note = "menu: luneta does not know yet which view luneta is on; try again in a moment";
        let right = "z".repeat(5);
        let boxed = |inner: &str| format!("{right}│{inner:<43}│");
        assert_eq!(floated(|buffer, over| draw_note(buffer, over, note), 50, 7), [
            "z".repeat(50),
            "z".repeat(50),
            format!("{right}┌─ luneta {}┐", "─".repeat(34)),
            boxed(" menu: luneta does not know yet which view"),
            boxed(" luneta is on; try again in a moment"),
            boxed(" <any key> - Close"),
            format!("{right}└{}┘", "─".repeat(43)),
        ]);
    }

    fn pull_request() -> Link {
        Link::parse("https://github.com/cashubtc/cdk/pull/2736").expect("a link")
    }

    fn link() -> Work {
        Work::Link(pull_request())
    }

    fn pulling_on(row: Target, key: char) -> Menu {
        let (menu, wants) = stays(Menu::pull(1, row, link(), CLAUDE).key(&char(key)));
        let wanted = if key == 'r' { Wanted::AtHead } else { Wanted::AsItIs };
        assert_eq!(wants, Wants::Pull(link(), wanted));
        assert!(menu.pulling());
        menu
    }

    fn pulling(key: char) -> Menu {
        pulling_on(view_row(), key)
    }

    fn here(ready: Ready) -> Found {
        Found { on: Machine::Local, ready }
    }

    fn argv(words: Vec<String>) -> Argv {
        Argv::parse(words).expect("an agent's argv")
    }

    fn agent(words: Vec<String>) -> Opens {
        Opens::Command(argv(words))
    }

    #[test]
    fn the_menu_for_a_pull_request_has_a_key_for_a_worktree_and_one_for_a_review() {
        let menu = Menu::pull(1, view_row(), link(), CLAUDE);
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
        let syncing = Menu::pull(1, view_row(), link(), CLAUDE);
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

    #[test]
    fn a_key_for_a_pull_request_opens_where_luneta_found_it_goes() {
        let other = Listed::new_project("cdk", 2);
        let folder = "/w/cdk/feat-x".to_owned();
        let branch = Some("feat/x".to_owned());
        let ready = |at| {
            let (folder, branch) = (directory(&folder), branch.clone());
            let work = Readied::Link(pull_request());
            Ok(here(Ready { at, folder, branch, moved_off: false, work }))
        };
        let review = [
            "claude",
            "/thermo-nuclear-review https://github.com/cashubtc/cdk/pull/2736",
        ]
        .map(str::to_owned)
        .to_vec();
        for (key, agent) in [('w', None), ('r', Some(argv(review.clone())))] {
            let (menu, wants) = stays(pulling(key).pulled(ready(At::Unopened)));
            let (cwd, name) = (directory(&folder), "#2736".to_owned());
            assert_eq!(wants, Wants::Make { cwd, name, agent });
            assert_eq!(menu.target(), &Target::Machine(Machine::Local), "no view is in it");
            assert!(menu.opens_pulled());
            assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " ⠋ Opening #2736…");
            assert!(menu.turning());
        }
        let there = || At::There((other.clone(), ViewKey::terminal(9)));
        let place = Place::View(ViewPlace { session: other.clone(), view: ViewKey::terminal(9) });
        assert_eq!(closed(pulling('w').pulled(ready(there()))), Done::Go(Machine::Local, place));
        let (menu, wants) = stays(pulling('r').pulled(ready(there())));
        assert!(!menu.opens_pulled(), "a view beside another opens at once, with no pull");
        let opens = agent(review);
        let view = ViewKey::terminal(9);
        assert_eq!(wants, Wants::Open { session: other.clone(), view, opens });
        let Target::View(_, row) = menu.target() else { panic!("not a view's row") };
        assert_eq!((&row.session, row.view), (&other, ViewKey::terminal(9)));
    }

    #[test]
    fn tab_in_the_menu_for_a_pull_request_starts_its_agent_in_the_other_harness() {
        let menu = Menu::pull(1, view_row(), link(), CLAUDE);
        let tab = KeyEvent::from(KeyCode::Tab);
        assert_eq!(menu.lines(31, 9, 0).1.at(Position::new(5, 3)), Some(&Choice::Key(tab)));
        let (menu, wants) = stays(menu.key(&tab));
        assert_eq!(wants, Wants::Keep(Harness::Pi));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[3], " \u{f03ff} pi  <TAB> another agent");
        let (menu, _) = stays(menu.key(&char('r')));
        let (menu, wants) = stays(menu.key(&tab));
        assert_eq!(wants, Wants::Nothing);
        let folder = "/w/cdk/feat-x".to_owned();
        let work = Readied::Link(pull_request());
        let branch = Some("feat/x".to_owned());
        let at = At::Unopened;
        let ready = Ready { at, folder: directory(&folder), branch, moved_off: false, work };
        let (_, wants) = stays(menu.pulled(Ok(here(ready))));
        let review = [
            "pi",
            "/skill:thermo-nuclear-review https://github.com/cashubtc/cdk/pull/2736",
        ];
        let agent = Some(argv(review.map(str::to_owned).to_vec()));
        let (cwd, name) = (directory(&folder), "#2736".to_owned());
        assert_eq!(wants, Wants::Make { cwd, name, agent });
    }

    #[test]
    fn the_menu_for_an_issue_has_a_key_for_a_worktree_and_one_to_tackle_it() {
        let link = Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link");
        let issue = Work::Link(link.clone());
        let menu = Menu::pull(1, view_row(), issue.clone(), CLAUDE);
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
        let folder = "/w/cdk/issue-2736".to_owned();
        let ready = Ready {
            at: At::Unopened,
            folder: directory(&folder),
            branch: Some("issue-2736".to_owned()),
            moved_off: false,
            work: Readied::Link(link),
        };
        let (_, wants) = stays(menu.pulled(Ok(here(ready))));
        let tackle = ["claude", "Tackle https://github.com/cashubtc/cdk/issues/2736"];
        let agent = Some(argv(tackle.map(str::to_owned).to_vec()));
        let (cwd, name) = (directory(&folder), "#2736".to_owned());
        assert_eq!(wants, Wants::Make { cwd, name, agent });
    }

    #[test]
    fn t_on_a_worktree_that_moved_off_stays_open_saying_why_and_w_goes_on_from_there() {
        let link = Link::parse("https://github.com/cashubtc/cdk/issues/2736").expect("a link");
        let issue = Work::Link(link.clone());
        let (menu, _) = stays(Menu::pull(1, view_row(), issue.clone(), CLAUDE).key(&char('t')));
        let ready = Ready {
            at: At::Unopened,
            folder: directory("/w/cdk/issue-2736"),
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

    #[test]
    fn the_menu_for_a_file_has_a_key_for_a_worktree_and_one_to_tackle_it() {
        let file = || {
            let from = Directory::parse("/w/cdk/main".to_owned()).unwrap();
            Work::parse("docs/plans/sync-rework.md\n", Some(from)).unwrap()
        };
        let menu = Menu::pull(1, view_row(), file(), CLAUDE);
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
        let (folder, real) = ("/w/cdk/md-sync-rework", "/w/cdk/main/docs/plans/sync-rework.md");
        let ready = || Ready {
            at: At::Unopened,
            folder: directory(folder),
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
        let made = |agent| {
            let (cwd, name) = (directory(folder), "sync-rework".to_owned());
            Wants::Make { cwd, name, agent }
        };
        assert_eq!(wants, made(Some(argv(command))));

        let pressed = |key: char| {
            stays(Menu::pull(1, view_row(), file(), CLAUDE).key(&char(key))).0
        };
        let (_, wants) = stays(pressed('w').pulled(Ok(here(ready()))));
        assert_eq!(wants, made(None));

        assert_eq!(
            closed(pressed('w').pulled(Err("no such file".to_owned()))),
            Done::Wrong("menu: docs/plans/sync-rework.md: no such file".to_owned())
        );

        let bell = Readied::File { real: "/w/cdk/main/\u{7}.md".to_owned() };
        let bell = Ready { work: bell, ..ready() };
        assert_eq!(
            closed(pressed('t').pulled(Ok(here(bell)))),
            Done::Wrong(
                "menu: docs/plans/sync-rework.md: a command line holds a control character"
                    .to_owned()
            )
        );
    }

    #[test]
    fn the_keys_for_a_link_pasted_on_another_machine_s_row_open_there() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let other = Listed::new_project("cdk", 2);
        let ready = |at| {
            let work = Readied::Link(pull_request());
            let folder = directory("/w/cdk/feat-x");
            let ready = Ready { at, folder, branch: None, moved_off: false, work };
            Ok(Found { on: vps(), ready })
        };
        let row = || Target::View(vps(), row());
        let place = Place::View(ViewPlace { session: other.clone(), view: ViewKey::terminal(9) });
        let there = ready(At::There((other.clone(), ViewKey::terminal(9))));
        assert_eq!(closed(pulling_on(row(), 'w').pulled(there)), Done::Go(vps(), place));
        for asked in [row(), Target::Machine(vps())] {
            let (menu, wants) = stays(pulling_on(asked, 'w').pulled(ready(At::Unopened)));
            assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
            assert_eq!(menu.target(), &Target::Machine(vps()), "the machine that found it");
        }
    }

    #[test]
    fn what_a_key_found_on_another_machine_opens_there_from_any_row() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let other = Listed::new_project("cdk", 2);
        let found = |at| {
            let work = Readied::Link(pull_request());
            let folder = directory("/w/cdk/feat-x");
            let ready = Ready { at, folder, branch: None, moved_off: false, work };
            Ok(Found { on: vps(), ready })
        };
        let rows = [session_row(), view_row(), Target::Machine(Machine::Local)];
        for row in rows {
            let at = At::There((other.clone(), ViewKey::terminal(9)));
            let (menu, wants) = stays(pulling_on(row.clone(), 'r').pulled(found(at)));
            assert!(matches!(&wants, Wants::Open { view, .. } if *view == ViewKey::terminal(9)));
            let Target::View(machine, opening) = menu.target() else { panic!("not a view's row") };
            assert_eq!((machine, opening.view), (&vps(), ViewKey::terminal(9)));

            let view = ViewKey::terminal(9);
            let place = Place::View(ViewPlace { session: other.clone(), view });
            let at = At::There((other.clone(), ViewKey::terminal(9)));
            let goes = pulling_on(row.clone(), 'w').pulled(found(at));
            assert_eq!(closed(goes), Done::Go(vps(), place));

            let (menu, wants) = stays(pulling_on(row, 'r').pulled(found(At::Unopened)));
            assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
            assert_eq!(menu.target(), &Target::Machine(vps()));
        }
    }
}
