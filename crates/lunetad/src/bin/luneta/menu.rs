use std::cell::Cell;
use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::time::SystemTime;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use luneta_protocol::github::{PullRequest, State};
use luneta_protocol::said::Said;
use lunetad::projects::{self, ProjectId, TerminalId, WorkstreamId};
use lunetad::prunes;
use lunetad::{Destination, Directory};
use luneta_protocol::shell::Ending;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::actions::{self, Action, Key, Noun, MENU_GROUPS, REVIEW, ROW_GROUPS, YANK};
pub use crate::actions::YANK_PULL;
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
use crate::pull::{At, Found, Kind, Readied, Ready, Wanted, Work};
use crate::sidebar::{
    self, ACCENT, LIT, Listed, Mark, NAME, PaneKey, PanePlace, Place, ProjectRow, TAG,
    WorkstreamPlace,
};
use crate::skills::{Name, Search, Skills};
use crate::spots::{Drawn, Spots};
use crate::zellij::SessionName;

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

const MOVES: [(Label, &str); 10] = [
    (Label::Two('j', 'k'), "Next row, row before"),
    (Label::Two('g', 'g'), "First row"),
    (Label::char('G'), "Last row"),
    (Label::Two('h', 'l'), "Fold, unfold"),
    (Label::char(' '), "Fold or unfold"),
    (Label::Two('J', 'K'), "Move what v holds"),
    (Label::code(KeyCode::Enter), "Go to the row"),
    (Label::char('m'), "Menu"),
    (Label::char(ALL_KEYS), "All keys"),
    (Label::code(KeyCode::Esc), "Back to zellij"),
];

const MENU_MOVES: [(Label, &str); 3] = [
    (Label::code(KeyCode::Tab), "use another agent"),
    (Label::code(KeyCode::Backspace), "back a group"),
    (Label::code(KeyCode::Esc), "close"),
];

pub const TITLE: &str = "luneta";
const SEARCH_TITLE: &str = "Skills";
const SSH_TITLE: &str = "ssh";
const PROJECT_TITLE: &str = "New project";
const RENAME_TITLE: &str = "Rename";

const NEXT_DOES: [&str; 2] = ["another agent", "another"];

const PANE_S: &str = ", the pane's";

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
    Yank,
    AllKeys,
    Command(Action),
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
            YANK => Some(Top::Yank),
            ALL_KEYS => Some(Top::AllKeys),
            _ => None,
        }
    }

    pub fn of(key: char) -> Option<Top> {
        let command = Action::row().find(|(_, path)| *path == [key]);
        Top::moves(key).or_else(|| command.map(|(action, _)| Top::Command(action)))
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

    const fn action(self, asks: Asks) -> Action {
        match (self, asks) {
            (Pull::Worktree, _) => Action::PastedWorktree,
            (Pull::Agent, Asks::Review) => Action::PastedReview,
            (Pull::Agent, Asks::Tackle) => Action::PastedTackle,
        }
    }

    const fn wanted(self, asks: Asks) -> Wanted {
        match (self, asks) {
            (Pull::Agent, Asks::Review) => Wanted::AtHead,
            (Pull::Worktree, _) | (Pull::Agent, Asks::Tackle) => Wanted::AsItIs,
        }
    }

    fn pressed(key: &KeyEvent, asks: Asks) -> Option<Pull> {
        let KeyCode::Char(c) = key.code else {
            return None;
        };
        if !bare(key) {
            return None;
        }
        Pull::ALL.into_iter().find(|pull| pull.key(asks) == c)
    }

    fn key(self, asks: Asks) -> char {
        let action = self.action(asks);
        let Key::Pasted(key) = action.key() else {
            unreachable!("{action:?} answers a pasted link, and so is keyed as one");
        };
        key
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

#[derive(Clone, Debug, PartialEq, Eq)]
enum Typing {
    Machine,
    Folder,
    Name(String),
    Rename(Renames),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Renames {
    Terminal(TerminalId),
    Workstream(WorkstreamId),
    Project(ProjectId),
}

impl Renames {
    fn parse(self) -> fn(&str) -> Result<projects::Name, String> {
        match self {
            Renames::Terminal(_) | Renames::Workstream(_) => projects::Name::parse,
            Renames::Project(_) => projects::Name::of_project,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rename {
    Terminal(TerminalId, Option<projects::Name>),
    Workstream(WorkstreamId, Option<projects::Name>),
    Project(ProjectId, projects::Name),
}

#[expect(clippy::large_enum_variant, reason = "one is made on each Enter, and goes at once")]
enum Entered {
    Done(Done),
    Next(Typing),
    Wrong(String),
}

impl Typing {
    fn title(&self) -> &'static str {
        match self {
            Typing::Machine => SSH_TITLE,
            Typing::Folder | Typing::Name(_) => PROJECT_TITLE,
            Typing::Rename(_) => RENAME_TITLE,
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            Typing::Machine => SSH_HINT,
            Typing::Folder => FOLDER_HINT,
            Typing::Name(_) => PROJECT_NAME_HINT,
            Typing::Rename(Renames::Terminal(_) | Renames::Workstream(_)) => UNRENAME_HINT,
            Typing::Rename(Renames::Project(_)) => RENAME_HINT,
        }
    }

    fn enter(&self) -> &'static str {
        match self {
            Typing::Machine => "Add",
            Typing::Folder => "Next",
            Typing::Name(_) => "Make",
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
                Ok(folder) => Entered::Next(Typing::Name(folder)),
                Err(why) => Entered::Wrong(why),
            },
            Typing::Name(folder) => match named(projects::Name::of_project) {
                Ok(name) => Entered::Done(Done::Project { on: on.clone(), folder, name }),
                Err(why) => Entered::Wrong(why),
            },
            Typing::Rename(asked) => match (asked, named(asked.parse())) {
                (_, Err(why)) => Entered::Wrong(why),
                (Renames::Terminal(terminal), Ok(name)) => {
                    renamed(Rename::Terminal(terminal, name))
                },
                (Renames::Workstream(workstream), Ok(name)) => {
                    renamed(Rename::Workstream(workstream, name))
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
struct Delete {
    key: char,
    session: Listed,
    scope: Scope,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Project(Machine, ProjectTarget),
    Workstream(Machine, WorkstreamRow),
    Pane(Machine, PaneRow),
    Machine(Machine),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectTarget {
    pub project: ProjectRow,
    pub dir: String,
    pub empty: bool,
}

enum Offer<'t> {
    SearchSkills(&'t PaneRow),
    Skill(&'t PaneRow, &'static str),
    Last(&'t PaneRow, Last),
    FixChecks(&'t PaneRow),
    NewPane(&'t PaneRow),
    NewWorkstream(ProjectId),
    NewProject,
    NewMachine,
    Rename(Renames),
    ClosePane(&'t PaneRow),
    CloseWorkstream(&'t WorkstreamRow),
    RemoveProject(ProjectId),
    ForgetMachine(&'t Destination),
    DeleteMerged(Directory),
    Brief(brief::Scope),
    Ask,
    FileIssue(Repository),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkstreamRow {
    pub place: WorkstreamPlace,
    pub name: String,
    pub folder: String,
    pub branch: Option<String>,
    pub repository: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneRow {
    pub session: Listed,
    pub pane: PaneKey,
    pub terminal: Option<TerminalId>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub repository: Option<String>,
    pub pull_request: Option<PullRequest>,
    pub ran: Ran,
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
            | Target::Workstream(machine, _)
            | Target::Pane(machine, _)
            | Target::Machine(machine) => machine,
        }
    }

    fn noun(&self) -> Noun {
        match self {
            Target::Project(..) => Noun::Project,
            Target::Workstream(..) => Noun::Workstream,
            Target::Pane(..) => Noun::Pane,
            Target::Machine(_) => Noun::Machine,
        }
    }

    fn fits(&self, action: Action) -> bool {
        self.offer(action).is_some()
    }

    fn offer(&self, action: Action) -> Option<Offer<'_>> {
        if !action.fits(self.noun(), self.machine()) {
            return None;
        }
        let offer = match (action, self) {
            (Action::SearchSkills, Target::Pane(_, row)) => Offer::SearchSkills(row),
            (Action::Review | Action::IllegalStates | Action::TigerStyle, Target::Pane(_, row)) => {
                Offer::Skill(row, action.skill()?)
            },
            (Action::FixLast, Target::Pane(_, row)) => Offer::Last(row, Last::Fix),
            (Action::ExplainLast, Target::Pane(_, row)) => Offer::Last(row, Last::Explain),
            (Action::FixChecks, Target::Pane(_, row)) => Offer::FixChecks(row),
            (Action::NewPane, Target::Pane(_, row)) => Offer::NewPane(row),
            (Action::NewWorkstream, _) => Offer::NewWorkstream(self.project()?.id()),
            (Action::NewProject, _) => Offer::NewProject,
            (Action::NewMachine, _) => Offer::NewMachine,
            (Action::RenameTerminal, Target::Pane(_, PaneRow { terminal: Some(terminal), .. })) => {
                Offer::Rename(Renames::Terminal(*terminal))
            },
            (Action::RenameWorkstream, Target::Workstream(_, row)) => {
                Offer::Rename(Renames::Workstream(row.place.workstream))
            },
            (Action::RenameProject, Target::Project(_, ProjectTarget { project, .. })) => {
                Offer::Rename(Renames::Project(project.id()))
            },
            (Action::ClosePane, Target::Pane(_, row)) => Offer::ClosePane(row),
            (Action::CloseWorkstream, Target::Workstream(_, row))
                if row.place.project.listed().own() =>
            {
                Offer::CloseWorkstream(row)
            },
            (Action::RemoveProject, Target::Project(_, target)) if target.empty => {
                Offer::RemoveProject(target.project.id())
            },
            (Action::ForgetMachine, Target::Machine(Machine::Remote(destination))) => {
                Offer::ForgetMachine(destination)
            },
            (Action::DeleteMerged, _) => Offer::DeleteMerged(self.merged_in()?),
            (Action::BriefEvery, _) => Offer::Brief(brief::Scope::Every),
            (Action::BriefThis, _) => Offer::Brief(brief::Scope::Session),
            (Action::BriefHighlighted, _) => Offer::Brief(brief::Scope::Highlighted),
            (Action::Ask, _) => Offer::Ask,
            (Action::FileIssue, _) => Offer::FileIssue(self.repository()?),
            _ => return None,
        };
        Some(offer)
    }

    fn project(&self) -> Option<ProjectRow> {
        match self {
            Target::Project(_, target) => Some(target.project.clone()),
            Target::Workstream(_, row) => Some(row.place.project.clone()),
            Target::Pane(_, row) => ProjectRow::of(row.session.clone()),
            Target::Machine(_) => None,
        }
    }

    pub fn cwd(&self) -> Option<&str> {
        match self {
            Target::Project(..) | Target::Machine(_) => None,
            Target::Workstream(_, row) => Some(&row.folder),
            Target::Pane(_, row) => row.cwd.as_deref(),
        }
    }

    fn held_repository(&self) -> Option<&str> {
        match self {
            Target::Pane(_, row) => row.repository.as_deref(),
            Target::Workstream(_, row) => row.repository.as_deref(),
            Target::Project(..) | Target::Machine(_) => None,
        }
    }

    fn repository_name(&self) -> &str {
        self.held_repository().map_or("", prunes::name_of)
    }

    fn repository(&self) -> Option<Repository> {
        let (on, cwd, home) = (self.machine(), self.cwd()?, self.held_repository()?);
        let dir = Directory::parse(cwd.to_owned()).ok()?;
        let (home, name) = (home.to_owned(), self.repository_name().to_owned());
        Some(Repository { on: on.clone(), dir, home, name })
    }

    fn merged_in(&self) -> Option<Directory> {
        self.held_repository()?;
        Directory::parse(self.cwd()?.to_owned()).ok()
    }

    fn folder_hint(&self) -> String {
        let held = match self {
            Target::Pane(_, row) => row.cwd.clone(),
            Target::Workstream(_, row) => Some(row.folder.clone()),
            Target::Project(_, project) => Some(project.dir.clone()),
            Target::Machine(_) => None,
        };
        held.unwrap_or_else(|| match self.machine() {
            Machine::Local => own_folder(),
            Machine::Remote(_) => "~".to_owned(),
        })
    }

    fn folder(&self) -> Result<Directory, String> {
        let folder = match self {
            Target::Pane(_, row) => return row.beside().map(|beside| beside.cwd),
            Target::Workstream(_, row) => row.folder.clone(),
            Target::Project(_, project) => project.dir.clone(),
            Target::Machine(_) => return Err("menu: a machine's row has no folder".to_owned()),
        };
        Directory::parse(folder).map_err(|why| format!("menu: {why}"))
    }

    pub fn branch(&self) -> Option<&str> {
        match self {
            Target::Project(..) | Target::Machine(_) => None,
            Target::Workstream(_, row) => row.branch.as_deref(),
            Target::Pane(_, row) => row.branch.as_deref(),
        }
    }

    fn brief(&self, scope: brief::Scope) -> Briefed {
        match (self, scope) {
            (_, brief::Scope::Every) => Briefed::Every,
            (Target::Machine(machine), brief::Scope::Session | brief::Scope::Highlighted) => {
                Briefed::Machine(machine.clone())
            },
            (Target::Pane(machine, row), brief::Scope::Highlighted) => {
                Briefed::Agent(machine.clone(), row.session.name().clone(), row.pane.id())
            },
            (
                Target::Workstream(machine, WorkstreamRow { place, .. }),
                brief::Scope::Session | brief::Scope::Highlighted,
            ) => Briefed::Session(machine.clone(), place.project.listed().name().clone()),
            (
                Target::Project(machine, ProjectTarget { project, .. }),
                brief::Scope::Session | brief::Scope::Highlighted,
            ) => Briefed::Session(machine.clone(), project.listed().name().clone()),
            (Target::Pane(machine, PaneRow { session, .. }), brief::Scope::Session) => {
                Briefed::Session(machine.clone(), session.name().clone())
            },
        }
    }
}

fn started(keys: &Keys, row: &PaneRow, skill: &str) -> Result<(Listed, Wants), String> {
    let Beside { session, pane, cwd } = row.beside()?;
    let command = agents::on_skill(keys.agent.harness(), skill, None);
    let opens = Opens::Pane { beside: pane.id(), command };
    Ok((session.clone(), Wants::Open { session, pane, cwd, opens }))
}

const ASKS_FIRST: [Action; 3] = [Action::ClosePane, Action::CloseWorkstream, Action::DeleteMerged];

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
    pane: PaneKey,
    cwd: Directory,
}

impl PaneRow {
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

    fn ended(&self) -> Result<(Beside, Ended), String> {
        let beside = self.beside()?;
        let pane = beside.pane;
        match &self.ran {
            Ran::Ended(ended) => Ok((beside, ended.clone())),
            Ran::Going(Some(command)) => Err(format!(
                "menu: pane {pane} runs `{}` still, and what it prints is not over",
                command.as_str()
            )),
            Ran::Going(None) => Err(format!(
                "menu: pane {pane} runs a command still, and what it prints is not over"
            )),
            Ran::Stopped(Some(command)) => Err(format!(
                "menu: `{}` is stopped in pane {pane}, and not over; bring it back with `fg`",
                command.as_str()
            )),
            Ran::Stopped(None) => Err(format!(
                "menu: a job is stopped in pane {pane}, and not over; bring it back with `fg`"
            )),
            Ran::EndedUnsaid => Err(format!(
                "menu: pane {pane}'s shell said no line of the command it ran last, by which \
                 luneta finds what it printed"
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opens {
    Pane { beside: u32, command: Vec<String> },
    Tab { name: Option<String>, command: Vec<String> },
}

#[expect(clippy::large_enum_variant, reason = "it lasts a key; a box would allocate on each")]
pub enum Did {
    Stays(Menu, Wants),
    Closed(Done),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Wants {
    Nothing,
    Keep(Harness),
    Skills { cwd: String, agent: Harness },
    Refind { cwd: String, agent: Harness },
    Plan { session: Listed, scope: Scope },
    Prunes { dir: Directory },
    Prune { dir: Directory, agreed: BTreeSet<prunes::Deleting> },
    Open { session: Listed, pane: PaneKey, cwd: Directory, opens: Opens },
    Pull(Work, Wanted),
    Printed { session: Listed, pane: PaneKey, ended: Ended },
    Checks { page: PullPage },
    Make {
        session: SessionName,
        holder: Directory,
        cwd: Directory,
        name: String,
        command: Vec<String>,
    },
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
    Workstream(ProjectId, Directory),
    Project { on: Machine, folder: String, name: Option<projects::Name> },
    Rename(Machine, Rename),
    Remove(Machine, ProjectId),
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
    Planning { keys: Keys, delete: Delete, agreed: Option<Plan> },
    Asking { keys: Keys, delete: Delete, plan: Plan, asks: String },
    Sweeping { keys: Keys, dir: Directory },
    Swept { keys: Keys, dir: Directory, plan: prunes::Plan },
    Pruning { keys: Keys, dir: Directory, count: usize },
    Opening { keys: Keys, session: Listed },
    Reading { keys: Keys, last: Last, beside: Beside, ended: Ended },
    Fetching { keys: Keys, beside: Beside, page: PullPage },
    Pull { work: Work, agent: Harness, step: Step },
    Typing { keys: Keys, typing: Typing, typed: String, wrong: Option<String> },
}

enum Step {
    Choosing { wrong: Option<String> },
    Waiting(Wait),
}

enum Wait {
    Pulling(Pull),
    Opening(Listed),
    Making(SessionName),
}

impl Wait {
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

    pub fn planning(&self) -> Option<(&Listed, PaneKey)> {
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
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
            | Stage::Typing { .. } => None,
        }
    }

    pub fn opening(&self) -> Option<Listed> {
        match &self.stage {
            Stage::Opening { session, .. }
            | Stage::Pull { step: Step::Waiting(Wait::Opening(session)), .. } => {
                Some(session.clone())
            },
            Stage::Choosing(_)
            | Stage::Planning { .. }
            | Stage::Asking { .. }
            | Stage::Sweeping { .. }
            | Stage::Swept { .. }
            | Stage::Pruning { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
            | Stage::Pull { .. }
            | Stage::Typing { .. } => None,
        }
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

    pub fn made(&self) -> Option<&SessionName> {
        match &self.stage {
            Stage::Pull { step: Step::Waiting(Wait::Making(session)), .. } => Some(session),
            _ => None,
        }
    }

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
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
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
            | Stage::Typing { .. } => None,
        }
    }

    pub fn paste(&mut self, text: &str) -> bool {
        let Stage::Typing { typed, wrong, .. } = &mut self.stage else {
            return false;
        };
        typed.extend(text.chars().filter(|c| !c.is_control()));
        *wrong = None;
        true
    }

    fn act(number: u64, target: &Target, mut keys: Keys, offer: Offer<'_>) -> Did {
        let stays = |stage: Stage, wants: Wants| {
            Did::Stays(Menu { number, target: target.clone(), stage }, wants)
        };
        let typing = |keys: Keys, typing: Typing| {
            let stage = Stage::Typing { keys, typing, typed: String::new(), wrong: None };
            stays(stage, Wants::Nothing)
        };
        let deleting = |keys: Keys, action: Action, session: &Listed, scope: Scope| {
            let (key, session) = (yes_key(action), session.clone());
            let wants = Wants::Plan { session: session.clone(), scope };
            let delete = Delete { key, session, scope };
            stays(Stage::Planning { keys, delete, agreed: None }, wants)
        };
        let wrong = |reason: String| Did::Closed(Done::Wrong(reason));
        match offer {
            Offer::SearchSkills(row) => match row.beside() {
                Ok(Beside { cwd, .. }) => {
                    keys.search = Some(Search::new());
                    let wants = Wants::Skills { cwd: cwd.into(), agent: keys.agent.harness() };
                    stays(Stage::Choosing(keys), wants)
                },
                Err(reason) => wrong(reason),
            },
            Offer::Skill(row, skill) => match started(&keys, row, skill) {
                Ok((session, wants)) => stays(Stage::Opening { keys, session }, wants),
                Err(reason) => wrong(reason),
            },
            Offer::Last(row, last) => match row.ended() {
                Ok((beside, ended)) => {
                    let (session, pane) = (beside.session.clone(), beside.pane);
                    let wants = Wants::Printed { session, pane, ended: ended.clone() };
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
            Offer::NewPane(row) => match row.beside() {
                Ok(Beside { session, pane, cwd }) => {
                    let opens = Opens::Pane { beside: pane.id(), command: Vec::new() };
                    let stage = Stage::Opening { keys, session: session.clone() };
                    stays(stage, Wants::Open { session, pane, cwd, opens })
                },
                Err(reason) => wrong(reason),
            },
            Offer::NewWorkstream(project) => match target.folder() {
                Ok(folder) => Did::Closed(Done::Workstream(project, folder)),
                Err(reason) => wrong(reason),
            },
            Offer::NewProject => {
                let typed = target.folder_hint();
                let stage = Stage::Typing { keys, typing: Typing::Folder, typed, wrong: None };
                stays(stage, Wants::Nothing)
            },
            Offer::NewMachine => typing(keys, Typing::Machine),
            Offer::Rename(renames) => typing(keys, Typing::Rename(renames)),
            Offer::ClosePane(row) => {
                deleting(keys, Action::ClosePane, &row.session, Scope::Pane(row.pane))
            },
            Offer::CloseWorkstream(row) => {
                let scope = Scope::Workstream(row.place.workstream);
                deleting(keys, Action::CloseWorkstream, row.place.project.listed(), scope)
            },
            Offer::RemoveProject(project) => {
                Did::Closed(Done::Remove(target.machine().clone(), project))
            },
            Offer::ForgetMachine(destination) => Did::Closed(Done::Forget(destination.clone())),
            Offer::DeleteMerged(dir) => {
                let wants = Wants::Prunes { dir: dir.clone() };
                stays(Stage::Sweeping { keys, dir }, wants)
            },
            Offer::Brief(scope) => Did::Closed(Done::Brief(target.brief(scope))),
            Offer::Ask => Did::Closed(Done::Ask(target.clone())),
            Offer::FileIssue(repository) => Did::Closed(Done::Issue(repository)),
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
                (Pressed::Moved | Pressed::Nothing, _) => {
                    stays(Stage::Choosing(keys), Wants::Nothing)
                },
                (Pressed::Start(skill), Target::Pane(_, row)) => {
                    match started(&keys, row, &skill) {
                        Ok((session, wants)) => stays(Stage::Opening { keys, session }, wants),
                        Err(reason) => Did::Closed(Done::Wrong(reason)),
                    }
                },
                (Pressed::Start(_), _) => stays(Stage::Choosing(keys), Wants::Nothing),
                (Pressed::Act(action), _) => match target.offer(action) {
                    Some(offer) => Menu::act(number, &target, keys, offer),
                    None => stays(Stage::Choosing(keys), Wants::Nothing),
                },
            },
            Stage::Typing { keys, typing, mut typed, .. } => match type_key(key, &mut typed) {
                Typed::Close => Did::Closed(Done::Nothing),
                Typed::Back => stays(Stage::Choosing(keys), Wants::Nothing),
                Typed::Typing => {
                    stays(Stage::Typing { keys, typing, typed, wrong: None }, Wants::Nothing)
                },
                Typed::Enter => match typing.clone().typed(&typed, target.machine()) {
                    Entered::Done(done) => Did::Closed(done),
                    Entered::Next(typing) => {
                        let typed = String::new();
                        stays(Stage::Typing { keys, typing, typed, wrong: None }, Wants::Nothing)
                    },
                    Entered::Wrong(why) => {
                        let wrong = Some(why);
                        stays(Stage::Typing { keys, typing, typed, wrong }, Wants::Nothing)
                    },
                },
            },
            Stage::Planning { .. }
            | Stage::Sweeping { .. }
            | Stage::Pruning { .. }
            | Stage::Opening { .. }
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
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
            | Stage::Reading { .. }
            | Stage::Fetching { .. }
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
        let opening = |session: Listed, pane: PaneKey, opens: Opens| {
            let (cwd, branch, ran) = (Some(worktree.clone()), branch.clone(), Ran::Unhooked);
            let row = PaneRow {
                session: session.clone(),
                pane,
                terminal: None,
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
                let opens = Opens::Pane { beside: pane.id(), command };
                opening(session, pane, opens)
            },
            (At::Session((session, pane)), _) => {
                let opens = Opens::Tab { name: Some(work.tab()), command };
                opening(session, pane, opens)
            },
            (At::Clone { cwd: holder, name }, _) => {
                let (session, cwd, tab) = (name.clone(), dir, work.tab());
                let wants = Wants::Make { session, holder, cwd, name: tab, command };
                let stage = Stage::Pull { work, agent, step: Step::Waiting(Wait::Making(name)) };
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
        if let Target::Pane(_, row) = &mut self.target {
            row.ran = ran;
        }
    }

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
        let opens = Opens::Pane { beside: pane.id(), command };
        let stage = Stage::Opening { keys, session: session.clone() };
        let wants = Wants::Open { session, pane, cwd, opens };
        Did::Stays(Menu { number, target, stage }, wants)
    }

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
        let opens = Opens::Pane { beside: pane.id(), command };
        let stage = Stage::Opening { keys, session: session.clone() };
        let wants = Wants::Open { session, pane, cwd, opens };
        Did::Stays(Menu { number: menu, target, stage }, wants)
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
            | Stage::Pull { .. } => false,
        }
    }

    fn shown(&self, width: usize, body_room: usize, frame: usize) -> Shown {
        let inner = width.saturating_sub(1);
        let right = self.target.cwd().map_or_default(sidebar::short_cwd);
        let mut spots = Spots::default();
        let (title, right, body, (footer, keys)) = match &self.stage {
            Stage::Pull { work, agent, step } => {
                let body = match step {
                    Step::Choosing { wrong } => {
                        let asks = Asks::of(work);
                        let entry = |pull: Pull| {
                            Entry::Act { key: pull.key(asks), action: pull.action(asks) }
                        };
                        let entries = Pull::ALL.map(entry);
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
            Stage::Typing { typing, typed, wrong, .. } => {
                let typed = sidebar::tail(&format!("{typed}_"), width.saturating_sub(2));
                let mut body = vec![Line::from(vec![
                    Span::raw(" "),
                    Span::styled(typed, Style::new().fg(ACCENT)),
                ])];
                match wrong {
                    Some(wrong) => body.extend(wrap(wrong, width, Style::new().fg(gruvbox::RED))),
                    None => body.extend(wrap(typing.hint(), width, Style::new().fg(NAME))),
                }
                let keys = [(Hint::Enter, typing.enter()), (Hint::Esc, "Back")];
                let right = match typing {
                    Typing::Name(folder) => sidebar::short_cwd(folder),
                    Typing::Machine | Typing::Folder | Typing::Rename(_) => String::new(),
                };
                (typing.title().to_owned(), right, body, keyed(width, &keys))
            },
        };
        Shown { title, right, body, spots, footer, keys }
    }
}

pub const OWN_ONLY: &str = "luneta goes to its own sessions only, now";

pub const HERE_ONLY: &str = "luneta makes and goes to sessions of this machine only, for now";

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

pub fn yanks(width: usize, room: usize) -> (Vec<Line<'static>>, Spots<KeyEvent>) {
    let yanks = tree(Action::row(), &ROW_GROUPS).into_iter().find_map(|entry| match entry {
        Entry::Group { key: YANK, name, entries } => Some((name, entries)),
        Entry::Group { .. } | Entry::Act { .. } => None,
    });
    let (name, entries) = yanks.expect("the catalogue keys its yanks under y");
    listed(name, &entries, width, room)
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

impl AllKeys {
    fn groups() -> [(&'static str, Vec<(String, String)>); 5] {
        let named = |keys: &[(Label, &str)]| -> Vec<(String, String)> {
            keys.iter().map(|(label, does)| (label.to_string(), (*does).to_owned())).collect()
        };
        let spelled = |path: &[char]| {
            let keys: Vec<String> = path.iter().map(char::to_string).collect();
            keys.join(" ")
        };
        let sidebar = Action::row().map(|(action, path)| {
            let group = path.first().filter(|_| path.len() > 1);
            let group = group.and_then(|group| actions::group_name(&ROW_GROUPS, *group));
            let does = group.map_or_else(
                || action.name().to_owned(),
                |group| format!("{group} {}", action.name()),
            );
            (spelled(path), does)
        });
        let menu = Action::menu().map(|(action, path)| (spelled(path), action.name().to_owned()));
        let mut menu: Vec<(String, String)> = menu.collect();
        menu.extend(named(&MENU_MOVES));
        let pasted = Action::pasted();
        let pasted = pasted.map(|(action, key)| (key.to_string(), action.name().to_owned()));
        let anywhere = Action::anywhere().map(|(action, (code, modifiers))| {
            (Label::Key(code, modifiers).to_string(), action.name().to_owned())
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
            let column = keys.iter().map(|(keys, _)| keys.width()).max().unwrap_or(0);
            rows.extend(keys.iter().map(|(keys, does)| keys_line_of(keys, column, does, inner)));
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
            pane: PaneKey::Zellij(4),
            terminal: None,
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

    fn project() -> ProjectRow {
        ProjectRow::of(Listed::new_project("luneta", 3)).unwrap()
    }

    fn terminal() -> TerminalId {
        TerminalId::of(9).unwrap()
    }

    fn own_row() -> PaneRow {
        let session = project().listed().clone();
        PaneRow { session, pane: PaneKey::terminal(4), terminal: Some(terminal()), ..row() }
    }

    fn own_pane() -> Target {
        Target::Pane(Machine::Local, own_row())
    }

    fn project_on(on: Machine, empty: bool) -> Target {
        let dir = "/w/luneta".to_owned();
        Target::Project(on, ProjectTarget { project: project(), dir, empty })
    }

    fn session_row() -> Target {
        project_on(Machine::Local, false)
    }

    fn workstream() -> WorkstreamId {
        WorkstreamId::of(5).unwrap()
    }

    fn workstream_on(on: Machine) -> Target {
        let place = WorkstreamPlace { project: project(), workstream: workstream() };
        Target::Workstream(on, WorkstreamRow {
            place,
            name: "main".to_owned(),
            folder: "/w/luneta/main".to_owned(),
            branch: Some("feat/menu".to_owned()),
            repository: Some("/w/luneta".to_owned()),
        })
    }

    fn char(key: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(key))
    }

    fn beside_row(opens: Opens) -> Wants {
        Wants::Open { session: listed(), pane: PaneKey::Zellij(4), cwd: main_dir(), opens }
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
        for c in (' '..='~').filter(|c| *c != YANK && Top::moves(*c).is_some()) {
            assert!(named.contains(&c), "{c:?}");
        }
    }

    #[test]
    fn a_key_opens_its_group_and_a_key_in_it_opens_a_pane_beside_the_row() {
        let (menu, wants) = stays(opened(own_pane(), Some('n')));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ New ─────────── luneta/main ─",
                " p  new pane",
                " w  new workstream",
                " P  new project",
                " m  new machine",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('p')));
        let cwd = main_dir();
        let opens = Opens::Pane { beside: 4, command: Vec::new() };
        let (session, pane) = (project().listed().clone(), PaneKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, pane, cwd: cwd.clone(), opens });
        assert!(menu.opening().is_some());
        let (menu, wants) = stays(menu.key(&char('p')));
        assert_eq!(wants, Wants::Nothing);
        assert!(menu.opening().is_some());
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Esc))), Done::Nothing);
    }

    #[test]
    fn n_w_makes_a_workstream_of_the_row_s_project_in_the_row_s_folder() {
        let made = |target: Target| closed(stays(opened(target, Some('n'))).0.key(&char('w')));
        let id = project().id();
        assert_eq!(made(own_pane()), Done::Workstream(id, main_dir()));
        assert_eq!(made(workstream_on(Machine::Local)), Done::Workstream(id, main_dir()));
        assert_eq!(made(session_row()), Done::Workstream(id, directory("/w/luneta")));
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let far = Target::Pane(vps(), own_row());
        for target in [pane_row(), far, workstream_on(vps()), project_on(vps(), false)] {
            let (menu, _) = stays(opened(target.clone(), Some('n')));
            let lines = text(&menu.lines(31, 9, 0).0);
            assert!(!lines.contains(&" w  new workstream".to_owned()), "{target:?}: {lines:?}");
        }
    }

    #[test]
    fn the_first_group_has_luneta_s_keys_and_backspace_goes_back_to_it() {
        let (menu, _) = stays(opened(own_pane(), None));
        assert_eq!(
            text(&menu.lines(31, 11, 0).0),
            [
                "─ luneta ──────── luneta/main ─",
                " /  Search skills",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " r  Rename the terminal",
                " d  Delete ▸",
                " b  Brief ▸",
                " <ESC> - Close",
            ]
        );
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, _) = stays(menu.key(&KeyEvent::from(KeyCode::Backspace)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[0], "─ luneta ──────── luneta/main ─");
        assert_eq!(closed(menu.key(&KeyEvent::from(KeyCode::Backspace))), Done::Nothing);
        assert!(Menu::open(1, pane_row(), Some('x'), PICKED).is_none());
    }

    #[test]
    fn an_own_session_s_pane_opens_its_agents_beside_it() {
        let (_, wants) = stays(stays(opened(own_pane(), Some('c'))).0.key(&char('t')));
        let command = ["claude", "/thermo-nuclear-review"].map(str::to_owned).to_vec();
        let opens = Opens::Pane { beside: 4, command };
        let (session, pane) = (project().listed().clone(), PaneKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, pane, cwd: main_dir(), opens });
    }

    #[test]
    fn the_legend_lists_the_sidebar_s_commands_then_the_menu_s_groups_for_the_row() {
        assert_eq!(
            text(&legend(Some(&own_pane()), 31, 15).0),
            [
                "─ luneta ──────────────────────",
                " /  Search",
                " u  Unread",
                " v  Reorder",
                " o  Open PR",
                " y  Yank ▸",
                " a  Ask",
                " i  File issue",
                " c  Skills ▸",
                " n  New ▸",
                " r  Rename the terminal",
                " d  Delete ▸",
                " b  Brief ▸",
            ]
        );
        assert_eq!(text(&legend(Some(&project_on(Machine::Local, true)), 31, 15).0)[6..], [
            " a  Ask",
            " n  New ▸",
            " r  Rename the project",
            " d  Delete ▸",
            " b  Brief ▸",
        ]);
        assert_eq!(text(&legend(None, 31, 15).0).len(), 6);
        assert_eq!(Top::of('m'), Some(Top::Menu));
        assert_eq!(Top::of('/'), Some(Top::Command(Action::Search)));
        assert_eq!(Top::of('u'), Some(Top::Command(Action::Unread)));
        assert_eq!(Top::of('o'), Some(Top::Command(Action::OpenPull)));
        assert_eq!(Top::of('V'), None);
        assert_eq!(Top::of('y'), Some(Top::Yank));
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
            assert!(Top::moves(path[0]).is_none() || path[0] == YANK, "{action:?} on {path:?}");
        }
        for (_, path) in Action::row() {
            let group = |(key, _): &(char, &str)| *key == path[0];
            assert!(!MENU_GROUPS.iter().any(group), "{path:?} opens a group of the menu");
        }
    }

    #[test]
    fn each_action_of_the_menu_is_offered_on_some_row() {
        let vps = Machine::Remote("me@vps".parse().unwrap());
        let rows = [
            own_pane(),
            pane_row(),
            workstream_on(Machine::Local),
            project_on(Machine::Local, true),
            Target::Machine(vps),
        ];
        for (action, _) in Action::menu() {
            assert!(rows.iter().any(|row| row.offer(action).is_some()), "{action:?}");
        }
    }

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

    #[test]
    fn tab_does_nothing_where_the_menu_starts_no_agent() {
        let (menu, _) = stays(opened(pane_row(), None));
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
        Target::Pane(Machine::Local, PaneRow { ran: Ran::Ended(cargo_test()), ..row() })
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
            "menu: pane 4's shell went on as luneta read what `cargo test` printed; ask again";
        let again = Ended { at: cargo_test().at + Duration::from_secs(5), ..cargo_test() };
        for ran in [Ran::Going(Some(said("ls"))), Ran::Ended(again), Ran::Ready, Ran::Unhooked] {
            assert_eq!(went(ran), Done::Wrong(why.to_owned()));
        }

        let busy = PaneRow { ran: Ran::Going(Some(said("cargo test"))), ..row() };
        let busy = Target::Pane(Machine::Local, busy);
        let (mut menu, _) = stays(opened(busy, Some('c')));
        menu.reran(Ran::Ended(cargo_test()));
        let (_, wants) = stays(menu.key(&char('f')));
        let pane = PaneKey::Zellij(4);
        assert_eq!(wants, Wants::Printed { session: listed(), pane, ended: cargo_test() });
    }

    #[test]
    fn a_key_for_the_last_command_starts_an_agent_on_what_it_printed() {
        let (menu, _) = stays(opened(ended_row(), Some('c')));
        let (menu, wants) = stays(menu.key(&char('e')));
        let pane = PaneKey::Zellij(4);
        assert_eq!(wants, Wants::Printed { session: listed(), pane, ended: cargo_test() });
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

        let pi = Picked::ByPane(Harness::Pi);
        let (menu, _) = stays(Menu::open(1, ended_row(), Some('c'), pi).unwrap());
        let (menu, _) = stays(menu.key(&char('e')));
        let kept = Kept { file: "/run/luneta/printed/1.txt".to_owned(), lines: 3, left_out: 0 };
        let (_, wants) = stays(menu.printed(Ok(Printed::After(kept))));
        let opens = Opens::Pane { beside: 4, command: vec!["pi".to_owned(), prompt.to_owned()] };
        assert_eq!(wants, beside_row(opens));

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
            refused(ran(Ran::Going(Some(said("make"))))),
            "menu: pane 4 runs `make` still, and what it prints is not over"
        );
        assert_eq!(
            refused(ran(Ran::Going(None))),
            "menu: pane 4 runs a command still, and what it prints is not over"
        );
        assert_eq!(
            refused(ran(Ran::Stopped(Some(said("vim"))))),
            "menu: `vim` is stopped in pane 4, and not over; bring it back with `fg`"
        );
        assert_eq!(
            refused(ran(Ran::Stopped(None))),
            "menu: a job is stopped in pane 4, and not over; bring it back with `fg`"
        );
        assert_eq!(
            refused(ran(Ran::EndedUnsaid)),
            "menu: pane 4's shell said no line of the command it ran last, by which luneta \
             finds what it printed"
        );
        assert_eq!(refused(ran(Ran::Ready)), "menu: pane 4's shell has run nothing yet");
        assert_eq!(
            refused(ran(Ran::Unhooked)),
            "menu: pane 4 runs no shell with luneta's hook; a fish started since the hook was \
             installed has it"
        );
        let own = Listed::new_project("luneta", 3);
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

    #[test]
    fn a_opens_the_ask_box_for_the_row_the_menu_is_for() {
        for row in [pane_row(), session_row()] {
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

    #[test]
    fn n_capital_p_makes_a_project_of_the_row_s_folder_or_one_typed() {
        let typed = |menu: Menu, text: &str| {
            text.chars().fold(menu, |menu, c| stays(menu.key(&char(c))).0)
        };
        let shown = |menu: &Menu| match &menu.stage {
            Stage::Typing { typed, .. } => typed.clone(),
            _ => panic!("no box to type in"),
        };
        let cleared = |menu: Menu| {
            let back = KeyEvent::from(KeyCode::Backspace);
            (0..shown(&menu).chars().count()).fold(menu, |menu, _| stays(menu.key(&back)).0)
        };
        let enter = KeyEvent::from(KeyCode::Enter);
        let asked = |target: Target| stays(stays(opened(target, Some('n'))).0.key(&char('P'))).0;
        for (target, folder) in [
            (pane_row(), "/w/luneta/main".to_owned()),
            (workstream_on(Machine::Local), "/w/luneta/main".to_owned()),
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
        let menu = asked(pane_row());
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
        let (menu, _) = stays(typed(menu, "api/v2").key(&enter));
        let Stage::Typing { wrong: Some(wrong), .. } = &menu.stage else {
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
    fn r_renames_the_row_s_terminal_workstream_or_project() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let enter = KeyEvent::from(KeyCode::Enter);
        let renamed = |target: Target, to: &str| {
            let (menu, _) = stays(opened(target, Some('r')));
            let menu = to.chars().fold(menu, |menu, c| stays(menu.key(&char(c))).0);
            menu.key(&enter)
        };
        let logs = || projects::Name::parse("logs").unwrap();
        let to = || Some(logs());
        let done = closed(renamed(own_pane(), "logs"));
        assert_eq!(done, Done::Rename(Machine::Local, Rename::Terminal(terminal(), to())));
        let done = closed(renamed(Target::Pane(vps(), own_row()), ""));
        assert_eq!(done, Done::Rename(vps(), Rename::Terminal(terminal(), None)));
        let done = closed(renamed(workstream_on(vps()), "logs"));
        assert_eq!(done, Done::Rename(vps(), Rename::Workstream(workstream(), to())));
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
        let (menu, _) = stays(renamed(workstream_on(Machine::Local), &long));
        let Stage::Typing { typed, wrong: Some(wrong), .. } = &menu.stage else {
            panic!("a name too long keeps its box open, saying why");
        };
        assert_eq!(*typed, long);
        assert!(wrong.contains("longer than 64 characters"), "{wrong}");
        let (menu, _) = stays(renamed(session_row(), "api/v2"));
        let Stage::Typing { wrong: Some(wrong), .. } = &menu.stage else {
            panic!("a project's name the sidebar cannot list keeps its box open, saying why");
        };
        assert!(wrong.contains("names no project"), "{wrong}");
        let api = Some(projects::Name::parse("api/v2").unwrap());
        let done = closed(renamed(workstream_on(Machine::Local), "api/v2"));
        assert_eq!(done, Done::Rename(Machine::Local, Rename::Workstream(workstream(), api)));
        let (menu, _) = stays(opened(workstream_on(Machine::Local), Some('r')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [" _", " its new name; with none, it"]);
        for target in [pane_row(), local_header(), remote_header()] {
            assert!(Menu::open(1, target.clone(), Some('r'), PICKED).is_none(), "{target:?}");
        }
    }

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
        }), "New");
    }

    #[test]
    fn another_machine_s_row_lists_what_works_there_and_does_it_there() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        let root = |target: Target| {
            let (menu, _) = stays(opened(target, None));
            text(&menu.lines(40, 12, 0).0)[1..].to_vec()
        };
        let pane = Target::Pane(vps(), own_row());
        assert_eq!(root(pane.clone()), [
            " /  Search skills",
            " a  Ask",
            " i  File issue",
            " c  Skills ▸",
            " n  New ▸",
            " r  Rename the terminal",
            " d  Delete ▸",
            " b  Brief ▸",
            " <ESC> - Close",
        ]);
        let (_, wants) = stays(stays(opened(pane.clone(), Some('c'))).0.key(&char('t')));
        let command = ["claude", "/thermo-nuclear-review"].map(str::to_owned).to_vec();
        let opens = Opens::Pane { beside: 4, command };
        let (session, at) = (project().listed().clone(), PaneKey::terminal(4));
        assert_eq!(wants, Wants::Open { session, pane: at, cwd: main_dir(), opens });
        let (menu, _) = stays(opened(pane.clone(), Some('n')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..4], [
            " p  new pane",
            " P  new project",
            " m  new machine",
        ]);
        let (menu, _) = stays(opened(pane.clone(), Some('d')));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);

        let (menu, _) = stays(opened(workstream_on(vps()), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Workstream(workstream()));
        assert_eq!(wants, Wants::Plan { session, scope });
        let done = closed(menu.planned(Ok(workstream_plan(Vec::new()))));
        assert_eq!(done, Done::Act(vps(), workstream_act()));

        let name = || project().listed().name().clone();
        for (row, highlighted) in [
            (pane, Briefed::Agent(vps(), name(), 4)),
            (project_on(vps(), false), Briefed::Session(vps(), name())),
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

    #[test]
    fn the_merged_worktrees_key_is_on_a_row_in_a_repository_alone() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        assert_eq!(
            text(&menu.lines(31, 9, 0).0),
            [
                "─ Delete ──────── luneta/main ─",
                " d  close the pane",
                " m  delete merged worktrees",
                " <BS> - Back, <ESC> - Close",
            ]
        );
        let (menu, wants) = stays(menu.key(&char('m')));
        assert_eq!(wants, Wants::Prunes { dir: main_dir() });
        assert!(menu.sweeping() && menu.turning());

        let outside = Target::Pane(Machine::Local, PaneRow { repository: None, ..row() });
        let unplaced = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        for target in [outside, unplaced, project_on(Machine::Local, true), remote_header()] {
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
            " r  Rename the project",
            " b  Brief ▸",
            " <ESC> - Close",
        ]);
        assert_eq!(at(project_on(Machine::Local, true), Some('d')), [
            " d  remove the project",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(session_row(), Some('n')), [
            " w  new workstream",
            " P  new project",
            " m  new machine",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(workstream_on(Machine::Local), None), [
            " a  Ask",
            " i  File issue",
            " n  New ▸",
            " r  Rename the workstream",
            " d  Delete ▸",
            " b  Brief ▸",
            " <ESC> - Close",
        ]);
        assert_eq!(at(workstream_on(Machine::Local), Some('d')), [
            " d  close the workstream",
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(own_pane(), Some('d')), [
            " d  close the pane",
            " m  delete merged worktrees",
            " <BS> - Back, <ESC> - Close",
        ]);
        assert_eq!(at(remote_header(), None), [
            " a  Ask",
            " n  New ▸",
            " d  Delete ▸",
            " b  Brief ▸",
            " <ESC> - Close",
        ]);
        assert_eq!(at(local_header(), Some('n')), [
            " P  new project",
            " m  new machine",
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
            assert_eq!(press(&target, Some('n'), 'p'), new());
        }
        assert_eq!(press(&local_header(), Some('n'), 'w'), new());
    }

    #[test]
    fn a_menu_that_cannot_open_beside_its_row_says_why() {
        let unknown = Target::Pane(Machine::Local, PaneRow { cwd: None, ..row() });
        let (menu, _) = stays(opened(unknown, None));
        let done = closed(menu.key(&char('/')));
        assert!(matches!(done, Done::Wrong(why) if why.contains("where pane 4 works")));
        let relative = PaneRow { cwd: Some("w".to_owned()), ..own_row() };
        let relative = Target::Pane(Machine::Local, relative);
        let (menu, _) = stays(opened(relative, Some('n')));
        let why = "menu: where pane 4 works, \"w\" is no absolute path".to_owned();
        assert_eq!(closed(menu.key(&char('w'))), Done::Wrong(why));
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
        let (menu, wants) = stays(menu.key(&KeyEvent::from(KeyCode::Enter)));
        let command = agents::on_skill(CLAUDE, "tigerstyle", None);
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
    }

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

    #[test]
    fn a_click_on_a_skill_starts_an_agent_on_it_as_enter_does() {
        let (mut menu, _) = stays(opened(pane_row(), Some('/')));
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
        let command = agents::on_skill(CLAUDE, "code-review", None);
        let opens = Opens::Pane { beside: 4, command };
        assert_eq!(wants, beside_row(opens));
        assert!(menu.opening().is_some());
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

    fn plan(closes: Closes, agents: Vec<(u32, Doing)>) -> Plan {
        let session = listed();
        let subject = Subject {
            session: &session,
            seat: Seat::Elsewhere,
            deletes: Deletes::Pane { pane: PaneKey::Zellij(4), closes },
            agents: Ok(agents),
            refuge: None,
        };
        close::plan(&subject)
    }

    #[test]
    fn a_delete_with_no_agent_at_work_to_end_goes_ahead_at_once() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        let scope = Scope::Pane(PaneKey::Zellij(4));
        assert_eq!(wants, Wants::Plan { session: listed(), scope });
        assert_eq!(menu.planning(), Some((&listed(), PaneKey::Zellij(4))));
        let done = closed(menu.planned(Ok(plan(Closes::Pane, vec![(4, Doing::Idle)]))));
        let (session, whose) = (listed().name().clone(), listed().whose());
        let act = Act::Pane { session, whose, pane: PaneKey::Zellij(4) };
        assert_eq!(done, Done::Act(Machine::Local, act));
        let (menu, _) = stays(opened(workstream_on(Machine::Local), Some('d')));
        let (menu, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Workstream(workstream()));
        assert_eq!(wants, Wants::Plan { session, scope });
        assert_eq!(menu.planning(), None);
    }

    #[test]
    fn a_delete_that_would_end_an_agent_at_work_asks_first() {
        let busy = || vec![(4, Doing::Busy)];
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, wants) = stays(menu.planned(Ok(plan(Closes::Pane, busy()))));
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
        let (menu, _) = stays(menu.planned(Ok(plan(Closes::Pane, busy()))));
        let (menu, wants) = stays(menu.key(&char('d')));
        let scope = Scope::Pane(PaneKey::Zellij(4));
        assert_eq!(wants, Wants::Plan { session: listed(), scope });
        let tab = || Closes::Tab { id: TabId(2), panes: vec![4, 5] };
        let at_work = vec![(4, Doing::Busy), (5, Doing::Waiting)];
        let (menu, wants) = stays(menu.planned(Ok(plan(tab(), at_work))));
        assert_eq!(wants, Wants::Nothing);
        let asking = text(&menu.lines(31, 9, 0).0);
        assert_eq!(asking[1..3], [" Close its tab? 2 agents, 1", " busy, 1 waiting"]);
        let (menu, _) = stays(menu.key(&char('y')));
        let busier = || vec![(4, Doing::Busy), (5, Doing::Busy)];
        let (menu, wants) = stays(menu.planned(Ok(plan(tab(), busier()))));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..3], [" Close its tab? 2 agents, 2", " busy"]);
        let (menu, _) = stays(menu.key(&char('y')));
        let (session, whose) = (listed().name().clone(), listed().whose());
        let act = Act::Tab { session, whose, tab: TabId(2), panes: vec![4, 5] };
        assert_eq!(closed(menu.planned(Ok(plan(tab(), busier())))), Done::Act(Machine::Local, act));
    }

    fn workstream_plan(agents: Vec<(u32, Doing)>) -> Plan {
        let session = project().listed().clone();
        let (panes, seen) = (vec![4, 5], workstream_seen());
        let deletes = Deletes::Workstream { workstream: workstream(), panes, seen };
        let (agents, seat) = (Ok(agents), Seat::Elsewhere);
        let subject = Subject { session: &session, seat, deletes, agents, refuge: None };
        close::plan(&subject)
    }

    fn workstream_act() -> Act {
        let (session, whose) = (project().listed().name().clone(), project().listed().whose());
        let (panes, seen) = (vec![4, 5], workstream_seen());
        Act::Workstream { session, whose, workstream: workstream(), panes, seen }
    }

    fn workstream_seen() -> Vec<TerminalId> {
        vec![TerminalId::of(4).unwrap(), TerminalId::of(5).unwrap()]
    }

    #[test]
    fn a_close_of_a_workstream_whose_terminals_hold_unread_files_asks_first() {
        let (menu, _) = stays(opened(workstream_on(Machine::Local), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let (menu, wants) = stays(menu.planned(Ok(workstream_plan(vec![(5, Doing::Unknown)]))));
        assert_eq!(wants, Wants::Nothing);
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1..4], [
            " Close the workstream? 1",
            " agent, 1 unknown",
            " <y> Yes <BS> Back <ESC> Close",
        ]);
        let (menu, wants) = stays(menu.key(&char('d')));
        let (session, scope) = (project().listed().clone(), Scope::Workstream(workstream()));
        assert_eq!(wants, Wants::Plan { session, scope });
        let done = closed(menu.planned(Ok(workstream_plan(vec![(5, Doing::Unknown)]))));
        assert_eq!(done, Done::Act(Machine::Local, workstream_act()));
        let (menu, _) = stays(opened(workstream_on(Machine::Local), Some('d')));
        let (menu, _) = stays(menu.key(&char('d')));
        let done = closed(menu.planned(Ok(workstream_plan(vec![(9, Doing::Busy)]))));
        assert_eq!(done, Done::Act(Machine::Local, workstream_act()));
    }

    #[test]
    fn d_d_on_an_empty_project_removes_it_and_on_one_with_workstreams_is_none() {
        let vps = || Machine::Remote("me@vps".parse().unwrap());
        for on in [Machine::Local, vps()] {
            let (menu, _) = stays(opened(project_on(on.clone(), true), Some('d')));
            assert_eq!(closed(menu.key(&char('d'))), Done::Remove(on.clone(), project().id()));
            assert!(Menu::open(1, project_on(on, false), Some('d'), PICKED).is_none());
        }
    }

    fn floated<R>(
        draw: impl FnOnce(&mut Buffer, Rect) -> R,
        width: u16,
        height: u16,
    ) -> Vec<String> {
        const ZELLIJ_X: u16 = 10;
        let over = Rect::new(ZELLIJ_X, 0, width, height);
        let mut buffer = Buffer::empty(Rect::new(0, 0, ZELLIJ_X + width, height));
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

    fn drawing(menu: &Menu) -> impl FnOnce(&mut Buffer, Rect) -> Drawn + '_ {
        move |buffer, over| menu.draw(buffer, over, UNIX_EPOCH)
    }

    #[test]
    fn the_menu_floats_at_the_bottom_right_of_zellij_s_side() {
        let menu = Menu::new(1, pane_row(), PICKED);
        let boxed = |inner: &str| format!("{}│{inner:<24}│", "z".repeat(24));
        assert_eq!(floated(drawing(&menu), 50, 12), [
            "z".repeat(50),
            "z".repeat(50),
            format!("{}┌─ luneta ─ luneta/main ─┐", "z".repeat(24)),
            boxed(" /  Search skills"),
            boxed(" a  Ask"),
            boxed(" i  File issue"),
            boxed(" c  Skills ▸"),
            boxed(" n  New ▸"),
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

    #[test]
    fn a_click_on_a_delete_asks_first_as_its_key_does() {
        let (menu, _) = stays(opened(pane_row(), Some('d')));
        assert_eq!(clicked(&menu, "close the pane"), Some(pressed(KeyCode::Char('d'))));
        let (menu, _) = stays(menu.key(&char('d')));
        assert_eq!(clicked(&menu, "close the pane"), None);
        let busy = vec![(4, Doing::Busy)];
        let (menu, _) = stays(menu.planned(Ok(plan(Closes::Pane, busy))));
        assert_eq!(clicked(&menu, "<y>"), Some(pressed(KeyCode::Char('y'))));
        assert_eq!(clicked(&menu, "Close the pane?"), None);
    }

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

    fn pulling_on(row: Target, key: char) -> Menu {
        let (menu, wants) = stays(Menu::pull(1, row, link(), CLAUDE).key(&char(key)));
        let wanted = if key == 'r' { Wanted::AtHead } else { Wanted::AsItIs };
        assert_eq!(wants, Wants::Pull(link(), wanted));
        assert!(menu.pulling());
        menu
    }

    fn pulling(key: char) -> Menu {
        pulling_on(pane_row(), key)
    }

    fn here(ready: Ready) -> Found {
        Found { on: Machine::Local, ready }
    }

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
        let session = || At::Session((other.clone(), PaneKey::Zellij(7)));
        for (key, command) in [('w', Vec::new()), ('r', review.clone())] {
            let (menu, wants) = stays(pulling(key).pulled(ready(session())));
            let opens = Opens::Tab { name: Some("#2736".to_owned()), command };
            let cwd = directory(&worktree);
            let pane = PaneKey::Zellij(7);
            assert_eq!(wants, Wants::Open { session: other.clone(), pane, cwd, opens });
            assert_eq!(menu.opening(), Some(other.clone()));
            let Target::Pane(_, row) = menu.target() else { panic!("not a pane's row") };
            assert_eq!((&row.session, row.pane), (&other, PaneKey::Zellij(7)));
            assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " ⠋ Opening #2736…");
            assert!(menu.turning());
        }
        let there = || At::There((other.clone(), PaneKey::Zellij(9)));
        let place = Place::Pane(PanePlace { session: other.clone(), pane: PaneKey::Zellij(9) });
        assert_eq!(closed(pulling('w').pulled(ready(there()))), Done::Go(Machine::Local, place));
        let (menu, wants) = stays(pulling('r').pulled(ready(there())));
        let opens = Opens::Pane { beside: 9, command: review };
        let cwd = directory(&worktree);
        let pane = PaneKey::Zellij(9);
        assert_eq!(wants, Wants::Open { session: other.clone(), pane, cwd, opens });
        let Target::Pane(_, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((&row.session, row.pane), (&other, PaneKey::Zellij(9)));
    }

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
        let at = At::Session((other.clone(), PaneKey::Zellij(7)));
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
        assert_eq!(wants, Wants::Open { session: other, pane: PaneKey::Zellij(7), cwd, opens });
    }

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
            at: At::Session((other.clone(), PaneKey::Zellij(7))),
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
        assert_eq!(wants, Wants::Open { session: other, pane: PaneKey::Zellij(7), cwd, opens });
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
            at: At::Session((other.clone(), PaneKey::Zellij(7))),
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
        assert_eq!(wants, Wants::Open { session, pane: PaneKey::Zellij(7), cwd, opens });

        let pressed = |key: char| {
            stays(Menu::pull(1, pane_row(), file(), CLAUDE).key(&char(key))).0
        };
        let (_, wants) = stays(pressed('w').pulled(Ok(here(ready()))));
        let opens = Opens::Tab { name: Some("sync-rework".to_owned()), command: Vec::new() };
        let session = other.clone();
        let cwd = directory(worktree);
        assert_eq!(wants, Wants::Open { session, pane: PaneKey::Zellij(7), cwd, opens });

        assert_eq!(
            closed(pressed('w').pulled(Err("no such file".to_owned()))),
            Done::Wrong("menu: docs/plans/sync-rework.md: no such file".to_owned())
        );
    }

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
                holder: directory("/w/cdk"),
                cwd: directory("/w/cdk/feat-x"),
                name: "#2736".to_owned(),
                command: Vec::new(),
            }
        );
        assert_eq!((menu.opening(), menu.made()), (None, Some(&name)));
        assert_eq!(text(&menu.lines(31, 9, 0).0)[1], " ⠋ Making session cdk…");
        assert!(menu.turning());
        let why = "no session works in cashubtc/cdk".to_owned();
        assert_eq!(
            closed(pulling('r').pulled(Err(why))),
            Done::Wrong("menu: cashubtc/cdk#2736: no session works in cashubtc/cdk".to_owned())
        );
    }

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
        let place = Place::Pane(PanePlace { session: other.clone(), pane: PaneKey::Zellij(9) });
        let there = ready(At::There((other.clone(), PaneKey::Zellij(9))));
        assert_eq!(closed(pulling_on(row(), 'w').pulled(there)), Done::Go(vps(), place));
        let at = At::Session((other, PaneKey::Zellij(7)));
        let (menu, _) = stays(pulling_on(row(), 'w').pulled(ready(at)));
        let Target::Pane(machine, row) = menu.target() else { panic!("not a pane's row") };
        assert_eq!((machine, row.pane), (&vps(), PaneKey::Zellij(7)));
        let name = SessionName::parse("cdk").unwrap();
        let clone = At::Clone { cwd: directory("/w/cdk"), name };
        let header = Target::Machine(vps());
        let (menu, wants) = stays(pulling_on(header.clone(), 'w').pulled(ready(clone)));
        assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
        assert_eq!(menu.target(), &header);
    }

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
            let at = At::There((other.clone(), PaneKey::Zellij(9)));
            let (menu, wants) = stays(pulling_on(row.clone(), 'r').pulled(found(at)));
            assert!(matches!(wants, Wants::Open { pane: PaneKey::Zellij(9), .. }), "{wants:?}");
            let Target::Pane(machine, opening) = menu.target() else { panic!("not a pane's row") };
            assert_eq!((machine, opening.pane), (&vps(), PaneKey::Zellij(9)));

            let place = Place::Pane(PanePlace { session: other.clone(), pane: PaneKey::Zellij(9) });
            let at = At::There((other.clone(), PaneKey::Zellij(9)));
            let goes = pulling_on(row.clone(), 'w').pulled(found(at));
            assert_eq!(closed(goes), Done::Go(vps(), place));

            let name = SessionName::parse("cdk").unwrap();
            let clone = At::Clone { cwd: directory("/w/cdk"), name };
            let (menu, wants) = stays(pulling_on(row, 'r').pulled(found(clone)));
            assert!(matches!(wants, Wants::Make { .. }), "{wants:?}");
            assert_eq!(menu.target(), &Target::Machine(vps()));
        }
    }
}
