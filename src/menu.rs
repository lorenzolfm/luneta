//! The menu: a key, then another, and an agent is at work, as which-key shows
//! the keys in neovim.
//!
//! Its `menu` holds its entries: a key each, and under a key that opens a
//! group, the keys of the group. The menu shows the keys of the group open and
//! what each does. A key with nothing under it starts Claude Code on its
//! prompt, in a pane of its own beside a pane, in the directory that pane
//! works in. Every menu has luneta's own New and Delete groups besides: under
//! `n`, a shell beside that pane, a tab or a session, in the directory it
//! works in; under `d`, closing that pane or killing its session. Its `/`
//! opens a search over every skill that directory can run, for an agent to
//! start on the one you pick. `mode "menu"` makes the plugin the menu for the pane you came from,
//! floating at the bottom right of the tab you are in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::*;

use crate::agents::{AgentSet, Live};
use crate::close::{self, Act, Leaving, Plan, Ran, Scope};
use crate::daemon;
use crate::dirs::{self, WORKTREES};
use crate::elapsed::{Age, Moment};
use crate::fetch::Fetch;
use crate::places::Places;
use crate::render;
use crate::sessions::{Selection, Session, Sessions, Taken};
use crate::skills::{self, Search};
use crate::tree::{self, origin};
use crate::context::{CONTEXT_KEY, DIR_KEY};

/// What holds the entries in the menu's configuration: a node with children,
/// which zellij hands over as their KDL.
pub const ENTRIES: &str = "menu";

/// The key that opens the search over the skills, at the top of the menu, and
/// what the menu says it does.
const SEARCH: char = '/';
pub const SEARCH_DOES: &str = "Search skills";

/// The key of luneta's own New group, and its name. Its keys are `New`'s.
const NEW: char = 'n';
const NEW_NAME: &str = "New";

/// The key of luneta's own Delete group, its name, and its keys: one deletes
/// what the menu is for, the other its session.
const DELETE: char = 'd';
const DELETE_NAME: &str = "Delete";
const DELETE_ROW: char = 'd';
const DELETE_SESSION: char = 's';

/// What a menu says that has no pane to start an agent beside, or to close:
/// the pane you opened it from is no terminal.
const NO_PANE: &str = "menu: the pane you are in is no terminal, to start an agent beside or to \
                       close; in luneta's sidebar, the menu's keys open it for the row you are on";

/// What the floating menu says of a delete key when zellij has not said which
/// session it is in.
const NO_SESSION: &str = "menu: zellij has not said which session this is; try again in a moment";

/// What the floating menu says of `d d` while what zellij said of the tabs
/// and of the panes disagrees.
const BEHIND: &str = "menu: zellij has not said all of what the tabs hold since one opened or \
                      closed; try again in a moment";

/// What the floating menu says of `d d` over a tab with a floating terminal,
/// which may be the pane you came from.
const UNSURE_PANE: &str = "menu: luneta cannot tell whether you came from a floating pane of this \
                           tab or from the pane under it, and closes neither; in luneta's \
                           sidebar, d d closes the row you are on";

/// What a key with nothing under it starts.
const CLAUDE: &str = "claude";

/// The menu asking git which branch is checked out where an agent starts.
pub const BRANCH_VALUE: &str = "branch";
/// The menu asking whether the directory a session starts in is a worktree,
/// to name the session for it.
pub const NAMING_VALUE: &str = "naming";

/// How often, in seconds, the floating menu looks whether you have left the
/// session it is to kill.
const LEAVING_TICK: f64 = 0.1;

/// Where a floating pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

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

    pub const fn key(self) -> char {
        match self {
            New::Pane => 'p',
            New::Tab => 't',
            New::Session => 's',
        }
    }

    /// What the menu says the key does.
    pub fn does(self) -> &'static str {
        match self {
            New::Pane => "new pane",
            New::Tab => "new tab",
            New::Session => "new session",
        }
    }
}

pub enum Entry {
    /// A key that opens a group, the name the menu shows for it, and the keys
    /// in it.
    Group { key: char, name: String, entries: Vec<Entry> },
    /// A key that starts an agent on a prompt.
    Start { key: char, prompt: String },
    /// A key of the New group: opens a pane, a tab or a session.
    New(New),
    /// A key of the Delete group: closes the pane the menu is for, or kills
    /// its session.
    Close { key: char, scope: Scope },
    /// The key that opens the search over the skills.
    Search,
}

impl Entry {
    pub fn key(&self) -> char {
        match self {
            Entry::Group { key, .. } | Entry::Start { key, .. } | Entry::Close { key, .. } => *key,
            Entry::New(new) => new.key(),
            Entry::Search => SEARCH,
        }
    }
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Pressed {
    /// Opened a group, or went back to the one before, or typed in the search
    /// box, moved its cursor or closed it.
    Moved,
    /// Asks for the search box, for the menu to open it over the skills of
    /// the directory it starts an agent in, if it knows one.
    Search,
    /// Starts an agent on the prompt.
    Start(String),
    /// Opens a pane, a tab or a session.
    New(New),
    /// Closes the pane the menu is for, or kills its session, by `key`.
    Delete { key: char, scope: Scope },
    /// Closes the menu.
    Close,
    /// Nothing: the group open has no such key.
    Nothing,
}

/// The menu's entries, the groups opened so far, and the search box over the
/// skills while it is open.
pub struct Keys {
    root: Vec<Entry>,
    /// Each group opened, by where it stands in the one before.
    open: Vec<usize>,
    /// Boxed: the box is larger than the rest of the keys, and a menu that is
    /// never searched needs none.
    search: Option<Box<Search>>,
}

impl Keys {
    /// The entries, from the KDL zellij hands over if it hands any: a node
    /// for each key, its one string the prompt the key starts an agent on, or
    /// the name of the group its children are. luneta's `/` comes before
    /// them, which opens the search over the skills, and its New and Delete
    /// groups after them: `n p`, `n t` and `n s` open a pane, a tab and a
    /// session, `d d` closes the pane the menu is for, and `d s` kills its
    /// session. A key of yours at the top can take none of the three.
    pub fn new(configured: Option<&str>) -> Result<Self, String> {
        let root = match configured {
            Some(text) => {
                let document: KdlDocument =
                    text.parse().map_err(|error| format!("menu: {error}"))?;
                entries(&document, "")?
            },
            None => Vec::new(),
        };
        let luneta_s =
            [(SEARCH, "its search"), (NEW, "its New group"), (DELETE, "its Delete group")];
        for (key, what) in luneta_s {
            if root.iter().any(|entry| entry.key() == key) {
                return Err(format!(
                    "menu: \"{key}\" is luneta's, for {what}; give your key another"
                ));
            }
        }
        Ok(Keys::of(root))
    }

    /// The keys of `root`, which has none of luneta's, luneta's `/` before
    /// them, and its New and Delete groups after them.
    fn of(mut root: Vec<Entry>) -> Self {
        root.insert(0, Entry::Search);
        root.push(Entry::Group {
            key: NEW,
            name: NEW_NAME.to_owned(),
            entries: New::ALL.into_iter().map(Entry::New).collect(),
        });
        root.push(Entry::Group {
            key: DELETE,
            name: DELETE_NAME.to_owned(),
            entries: vec![
                Entry::Close { key: DELETE_ROW, scope: Scope::Pane },
                Entry::Close { key: DELETE_SESSION, scope: Scope::Session },
            ],
        });
        Keys { root, open: Vec::new(), search: None }
    }

    /// The groups opened, each its name and its entries.
    fn trail(&self) -> Vec<(&str, &[Entry])> {
        let mut trail = Vec::with_capacity(self.open.len());
        let mut entries = self.root.as_slice();
        for at in &self.open {
            if let Some(Entry::Group { name, entries: inner, .. }) = entries.get(*at) {
                trail.push((name.as_str(), inner.as_slice()));
                entries = inner;
            }
        }
        trail
    }

    /// The entries of the group open.
    pub fn entries(&self) -> &[Entry] {
        self.trail().last().map_or(self.root.as_slice(), |(_, entries)| entries)
    }

    /// The names of the groups opened, the first one first.
    pub fn names(&self) -> Vec<&str> {
        self.trail().into_iter().map(|(name, _)| name).collect()
    }

    /// The search box over the skills, while it is open.
    pub fn search(&self) -> Option<&Search> {
        self.search.as_deref()
    }

    /// Opens `search`, which asked the finder for its skills as it was made.
    fn open_search(&mut self, search: Search) {
        self.search = Some(Box::new(search));
    }

    /// Fills the search box with what the finder said, while it is open and
    /// waits for it, and says whether it did.
    fn found(&mut self, skills: Fetch<Vec<skills::Skill>>) -> bool {
        self.search.as_mut().is_some_and(|search| search.fill(skills))
    }

    /// What `key` does: a key the group open shows opens its group, starts
    /// its agent, opens or closes what it opens or closes, or asks for the
    /// search box, `Backspace` goes back a group, and out of
    /// the menu from the first, and `Esc` closes it. While the search box is
    /// open, it takes the keys instead.
    pub fn key(&mut self, key: &KeyWithModifier) -> Pressed {
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
        match key.bare_key {
            BareKey::Backspace if key.has_no_modifiers() => match self.open.pop() {
                Some(_) => Pressed::Moved,
                None => Pressed::Close,
            },
            BareKey::Char(c) if key.has_no_modifiers() => self.press(c),
            _ => Pressed::Nothing,
        }
    }

    fn press(&mut self, key: char) -> Pressed {
        let entries = self.entries();
        let Some(at) = entries.iter().position(|entry| entry.key() == key) else {
            return Pressed::Nothing;
        };
        match &entries[at] {
            Entry::Start { prompt, .. } => return Pressed::Start(prompt.clone()),
            Entry::New(new) => return Pressed::New(*new),
            Entry::Close { key, scope } => return Pressed::Delete { key: *key, scope: *scope },
            Entry::Search => return Pressed::Search,
            Entry::Group { .. } => {},
        }
        self.open.push(at);
        Pressed::Moved
    }
}

/// What `key` does in the search box: a character types,
/// `Backspace` takes one back, `Down` and `Up` move the cursor, and `Enter`
/// starts an agent on the skill it is on. `Esc` closes the box, back to the
/// keys, and so does `Backspace` once there is nothing to take back; `Ctrl c`
/// closes the menu. Says too whether the box stays open.
fn searched(search: &mut Search, key: &KeyWithModifier) -> (Pressed, bool) {
    if key.bare_key == BareKey::Char('c') && key.has_modifiers(&[KeyModifier::Ctrl]) {
        return (Pressed::Close, true);
    }
    if !key.has_no_modifiers() {
        return (Pressed::Nothing, true);
    }
    match key.bare_key {
        BareKey::Esc => (Pressed::Moved, false),
        BareKey::Backspace => (Pressed::Moved, search.pop()),
        BareKey::Down => {
            search.move_by(1);
            (Pressed::Moved, true)
        },
        BareKey::Up => {
            search.move_by(-1);
            (Pressed::Moved, true)
        },
        BareKey::Enter => match search.chosen() {
            Some(skill) => (Pressed::Start(format!("/{}", skill.name)), true),
            None => (Pressed::Nothing, true),
        },
        BareKey::Char(c) => {
            search.push(c);
            (Pressed::Moved, true)
        },
        _ => (Pressed::Nothing, true),
    }
}

/// Whether `key` closes the menu, whatever group is open.
fn closes(key: &KeyWithModifier) -> bool {
    match key.bare_key {
        BareKey::Char('c') => key.has_modifiers(&[KeyModifier::Ctrl]),
        BareKey::Esc => key.has_no_modifiers(),
        _ => false,
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
fn answer(key: &KeyWithModifier, asked: char) -> Answer {
    if closes(key) {
        return Answer::Close;
    }
    if !key.has_no_modifiers() {
        return Answer::Nothing;
    }
    match key.bare_key {
        BareKey::Char(c) if c == 'y' || c == asked => Answer::Yes,
        BareKey::Char('n') | BareKey::Backspace => Answer::Back,
        _ => Answer::Nothing,
    }
}

/// A delete key that asks before it goes ahead: the key, which goes ahead
/// pressed again, what it deletes, what it would do as it asked, and what it
/// asks.
struct Asked {
    key: char,
    scope: Scope,
    act: Act,
    question: String,
}

/// What a delete key does once it knows what it closes: ask first, or go
/// ahead.
enum Then {
    Ask(Asked),
    Go(Act),
}

impl Asked {
    fn then(key: char, scope: Scope, plan: Plan) -> Then {
        match plan.question() {
            Some(question) => Then::Ask(Asked { key, scope, act: plan.act, question }),
            None => Then::Go(plan.act),
        }
    }

    /// What going ahead does, with what the key deletes planned again `now`:
    /// what you were asked about, or, once that changed while you read, what
    /// the key does now. A pane that opened in the tab since holds the tab
    /// open, and does not close with it.
    fn yes(self, now: Plan) -> Then {
        if now.act == self.act {
            return Then::Go(now.act);
        }
        Asked::then(self.key, self.scope, now)
    }
}

/// What a menu shows under its rule.
#[derive(Clone, Copy)]
pub enum Shown<'a> {
    /// The keys of the group open.
    Keys(&'a Keys),
    /// What a delete key asks before it goes ahead, in the group open.
    Asking(&'a Keys, &'a str),
    /// The session you are on your way to, which the session you leave ends
    /// after.
    Leaving(&'a str),
    /// What is wrong.
    Wrong(&'a str),
}

/// The entries of `document`, the children of the group `path` names, as the
/// keys pressed to open it. A group has keys; the menu itself may have none of
/// yours, and has luneta's.
fn entries(document: &KdlDocument, path: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    for node in document.nodes() {
        let entry = entry(node, path)?;
        if entries.iter().any(|seen| seen.key() == entry.key()) {
            return Err(format!("menu: two entries for \"{path}{}\"", entry.key()));
        }
        entries.push(entry);
    }
    let group = path.trim_end();
    if entries.is_empty() && !group.is_empty() {
        return Err(format!("menu: no keys under \"{group}\""));
    }
    Ok(entries)
}

fn entry(node: &KdlNode, path: &str) -> Result<Entry, String> {
    let name = node.name().value();
    let mut chars = name.chars();
    let (Some(key), None) = (chars.next(), chars.next()) else {
        return Err(format!("menu: \"{path}{name}\" is no key; a key is one character"));
    };
    let text = match node.entries() {
        [only] if only.name().is_none() => only.value().as_string().map(str::trim),
        _ => None,
    };
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return Err(format!(
            "menu: \"{path}{key}\" takes one string: the prompt it starts an agent on, or the \
             name of its group"
        ));
    };
    match node.children() {
        Some(children) => Ok(Entry::Group {
            key,
            name: text.to_owned(),
            entries: entries(children, &format!("{path}{key} "))?,
        }),
        None => Ok(Entry::Start { key, prompt: text.to_owned() }),
    }
}

/// The pane an agent the menu starts opens beside, and the directory that
/// pane works in.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Beside {
    pane: u32,
    cwd: String,
}

/// The terminal the floating menu was opened from, as far as zellij says.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    /// The terminal with the focus, in a tab with no floating terminal.
    Sure(Beside),
    /// The terminal with the focus, in a tab with a floating terminal. The
    /// menu takes the floating focus from the pane you were in as it opens,
    /// and zellij then says the pane under it has the focus: you came from
    /// that pane, or from a floating one.
    Guess(Beside),
}

impl Origin {
    fn beside(&self) -> &Beside {
        match self {
            Origin::Sure(beside) | Origin::Guess(beside) => beside,
        }
    }
}

/// Claude Code on `prompt`, as a command line. A prompt that runs a skill
/// names the agent for the skill and the branch it works on, as `/rename`
/// would, so that luneta's sidebar and the picker say what it is doing from
/// the start.
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

/// Opens a pane that runs `command` beside `beside`, a pane of the menu's own
/// session, in the directory it works in: the focus goes to that pane, across
/// tabs, and the new pane opens beside it.
fn start_here(command: &[String], beside: &Beside) {
    focus_terminal_pane(beside.pane, false, false);
    open_command_pane(command_pane(command, &beside.cwd), BTreeMap::new());
}

/// Opens your shell in a pane of its own beside `beside`, a pane of the
/// menu's own session, in the directory it works in, as `start_here` opens
/// an agent.
fn shell_here(beside: &Beside) {
    focus_terminal_pane(beside.pane, false, false);
    open_terminal(host_dir(&beside.cwd));
}

/// Opens a tab of the menu's own session, in `cwd`, and goes there.
fn tab_here(cwd: &str) {
    new_tab(None, Some(host_dir(cwd).to_string_lossy().into_owned()));
}

/// Asks whether `cwd` is a worktree, as `WORKTREES` tells, for a session that
/// starts there to be named as the picker names a directory.
fn ask_name(cwd: &str) {
    let mut worktrees = WORKTREES.to_vec();
    worktrees.push(cwd);
    run_command(&worktrees, BTreeMap::from([(CONTEXT_KEY.to_owned(), NAMING_VALUE.to_owned())]));
}

/// Starts a session in `cwd` and goes there, named for it from what
/// `WORKTREES` printed of it, `None` when it failed, as the picker names a
/// directory: with a postfix when `taken` holds the name. A directory no
/// session can be named for, as `/`, starts one zellij names.
///
/// zellij starts no session under a name a session has: it takes you to
/// that session, and `cwd` goes unused. Nor can it be asked to start one only
/// if none has the name, so the name is as free as `taken` is new: the menu
/// lists the sessions as the name comes. A session that took the name since
/// is where you go.
fn start_session(cwd: &str, worktrees: Option<&[u8]>, taken: &Taken) {
    let worktrees = worktrees.map(String::from_utf8_lossy);
    let name = dirs::session_name(cwd, worktrees.as_deref(), taken);
    switch_session_with_cwd(name.as_deref(), Some(host_dir(cwd)));
}

/// A pane of the menu's own session that runs `command`, in `cwd`.
fn command_pane(command: &[String], cwd: &str) -> CommandToRun {
    let (path, args) =
        command.split_first().map_or((CLAUDE, &[][..]), |(path, args)| (path.as_str(), args));
    CommandToRun { path: PathBuf::from(path), args: args.to_vec(), cwd: Some(host_dir(cwd)) }
}

/// `cwd`, spelled so that zellij takes it for the host's. zellij reads a
/// directory a plugin names under `/tmp`, `/data`, `/cache` or `/host` as one
/// of the plugin's own, and a pane whose directory is not there starts
/// wherever zellij runs. The root's parent is the root: by way of it, the path
/// names the same directory, and is under none of them.
fn host_dir(cwd: &str) -> PathBuf {
    if cwd.starts_with('/') { PathBuf::from(format!("/..{cwd}")) } else { PathBuf::from(cwd) }
}

/// Where the floating menu stands.
#[derive(Default)]
enum Phase {
    /// The keys of the group open, pressed one after another, for an agent
    /// beside the pane you came from, as zellij last said: `Waiting` until it
    /// has, and `Failed` when what you came from is no terminal, or zellij
    /// cannot say where it works.
    Choosing(Keys, Fetch<Origin>),
    /// A delete key that asks before it goes ahead, for the pane you came
    /// from.
    Asking(Keys, Origin, Asked),
    /// A session that starts in this directory waits for `WORKTREES` to say
    /// whether it is named for a repository too. Keys do nothing, but close
    /// the menu.
    Naming(Keys, String),
    /// On your way to another session, and the session the menu is in ends
    /// once you are gone. Keys do nothing.
    Leaving(Leaving),
    /// What is wrong with the keys, or with what a key asked for. Any key
    /// closes the menu.
    Wrong(String),
    /// The menu asked zellij to close it, and a key already on its way does
    /// nothing.
    #[default]
    Done,
}

/// The menu a key binding opens, floating at the bottom right of the tab, for
/// the pane you came from.
pub struct Menu {
    phase: Phase,
    plugin_id: u32,
    /// This plugin's client, which the menu takes out of the session before
    /// a delete key kills it.
    client: ClientId,
    panes: Option<PaneManifest>,
    tabs: Vec<TabInfo>,
    /// The directory git was last asked the branch of, and the branch checked
    /// out there once git has said.
    asked: Option<(String, Option<String>)>,
    /// Where the menu last asked zellij to stand it.
    placed: Option<Spot>,
    /// What the agents are doing, for a delete key to ask first before it ends
    /// one at work.
    agents: AgentSet,
    /// Where the panes work, by which an agent is placed in one.
    places: Places,
    /// The session the menu is in, and the one you would go to before it is
    /// killed, once the menu has asked zellij.
    session: Option<String>,
    refuge: Option<String>,
}

impl Menu {
    pub fn new(plugin_id: u32, client: ClientId, configuration: &BTreeMap<String, String>) -> Self {
        let phase = match Keys::new(configuration.get(ENTRIES).map(String::as_str)) {
            Ok(keys) => Phase::Choosing(keys, Fetch::Waiting),
            Err(reason) => Phase::Wrong(reason),
        };
        if let Phase::Wrong(reason) = &phase {
            eprintln!("luneta {reason}");
        }
        Menu {
            phase,
            plugin_id,
            client,
            panes: None,
            tabs: Vec::new(),
            asked: None,
            placed: None,
            agents: AgentSet::default(),
            places: Places::default(),
            session: None,
            refuge: None,
        }
    }

    /// What the menu shows: the keys, what a delete key asks, where you are
    /// going, or what is wrong with the keys or with the pane you came from.
    /// Nothing once it asked zellij to close it.
    pub fn shown(&self) -> Option<Shown<'_>> {
        match &self.phase {
            Phase::Wrong(reason) | Phase::Choosing(_, Fetch::Failed(reason)) => {
                Some(Shown::Wrong(reason))
            },
            Phase::Choosing(keys, _) | Phase::Naming(keys, _) => Some(Shown::Keys(keys)),
            Phase::Asking(keys, _, asked) => Some(Shown::Asking(keys, &asked.question)),
            Phase::Leaving(leaving) => Some(Shown::Leaving(leaving.to())),
            Phase::Done => None,
        }
    }

    /// The directory an agent the menu starts works in, or what it opens,
    /// once the menu knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.phase {
            Phase::Choosing(_, Fetch::Ready(origin)) | Phase::Asking(_, origin, _) => {
                Some(&origin.beside().cwd)
            },
            Phase::Naming(_, cwd) => Some(cwd),
            Phase::Choosing(..) | Phase::Leaving(_) | Phase::Wrong(_) | Phase::Done => None,
        }
    }

    /// What git said is checked out in `cwd`, once it has.
    fn branch(&self, cwd: &str) -> Option<&str> {
        let (asked, branch) = self.asked.as_ref()?;
        if asked != cwd {
            return None;
        }
        branch.as_deref()
    }

    /// Says whether to draw.
    pub fn tabs(&mut self, tabs: Vec<TabInfo>) -> bool {
        self.tabs = tabs;
        let followed = self.follow();
        self.place();
        followed
    }

    /// Says whether to draw.
    pub fn panes(&mut self, manifest: PaneManifest) -> bool {
        self.panes = Some(manifest);
        let followed = self.follow();
        self.place();
        followed
    }

    /// Says whether to draw. A menu that cannot show its keys has nothing to
    /// do but close, and any key closes it.
    pub fn key(&mut self, key: &KeyWithModifier) -> bool {
        match &mut self.phase {
            Phase::Done | Phase::Leaving(_) => false,
            Phase::Naming(..) => {
                if closes(key) {
                    self.close();
                }
                false
            },
            Phase::Wrong(_) | Phase::Choosing(_, Fetch::Failed(_)) => self.close(),
            Phase::Choosing(keys, _) => match keys.key(key) {
                Pressed::Moved => {
                    self.place();
                    true
                },
                Pressed::Start(prompt) => self.start(&prompt),
                Pressed::New(new) => self.open(new),
                Pressed::Search => self.find(),
                Pressed::Delete { key, scope } => self.delete(key, scope),
                Pressed::Close => self.close(),
                Pressed::Nothing => false,
            },
            Phase::Asking(_, _, asked) => match answer(key, asked.key) {
                Answer::Yes => self.go_ahead(),
                Answer::Back => self.back(),
                Answer::Close => self.close(),
                Answer::Nothing => false,
            },
        }
    }

    /// Asks zellij to close the menu, which takes no key from then on. Says
    /// whether to draw.
    fn close(&mut self) -> bool {
        self.phase = Phase::Done;
        close_self();
        false
    }

    /// What came of a command the menu ran: the branch checked out where an
    /// agent starts, the skills the directory there can run, what the daemon
    /// says the agents are doing and where the panes work, the clients of the
    /// session while you leave it, and whether the directory a session starts
    /// in is a worktree. Says whether to draw.
    pub fn ran(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        match context.get(CONTEXT_KEY).map(String::as_str) {
            Some(BRANCH_VALUE) => self.branched(exit_code, stdout, context),
            Some(skills::CONTEXT_VALUE) => return self.found(exit_code, stdout, stderr, context),
            Some(daemon::CONTEXT_VALUE) => self.heard(exit_code, stdout, stderr),
            Some(close::CLIENTS_VALUE) => self.clients(exit_code, stdout),
            Some(NAMING_VALUE) => return self.named(exit_code, stdout),
            _ => {},
        }
        false
    }

    /// What git said is checked out where an agent starts.
    fn branched(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        context: &BTreeMap<String, String>,
    ) {
        let Some((asked, branch)) = &mut self.asked else {
            return;
        };
        // What is checked out where an agent no longer starts.
        if context.get(DIR_KEY) != Some(&*asked) {
            return;
        }
        let said = String::from_utf8_lossy(stdout).trim().to_owned();
        *branch = (exit_code == Some(0) && !said.is_empty()).then_some(said);
    }

    /// Opens the search box over the skills the directory of the pane you came
    /// from can run. Before zellij says which pane that is, there is no
    /// directory to look in, and no box opens. Says whether to draw.
    fn find(&mut self) -> bool {
        let Phase::Choosing(keys, Fetch::Ready(origin)) = &mut self.phase else {
            return false;
        };
        keys.open_search(Search::ask(&origin.beside().cwd));
        self.place();
        true
    }

    /// What the finder said of the skills of the directory the search box
    /// looks in, while it is open and waits for it. An answer for another
    /// directory is for a box closed before it came. Says whether to draw.
    fn found(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        let Phase::Choosing(keys, _) = &mut self.phase else {
            return false;
        };
        if context.get(DIR_KEY).map(String::as_str) != keys.search().map(Search::cwd) {
            return false;
        }
        if !keys.found(skills::answer(exit_code, stdout, stderr)) {
            return false;
        }
        self.place();
        true
    }

    /// Puts each agent in the pane of the session it sits in.
    fn seat_agents(&mut self) {
        let live = Live::new(self.session.as_deref(), &[], &self.places, &[]);
        self.agents.rebuild("", &live, None, Moment::now(), Selection::Hold);
    }

    /// Starts Claude Code on `prompt` in a pane of its own, beside the pane
    /// you came from, in the directory that pane works in. The menu closes
    /// first, and the new pane opens beside that pane. A key pressed before
    /// zellij says which pane you came from starts nothing. Says whether to
    /// draw.
    fn start(&mut self, prompt: &str) -> bool {
        let Phase::Choosing(_, Fetch::Ready(origin)) = &self.phase else {
            return false;
        };
        // Over a floating terminal, beside the pane under the menu, which you
        // may not have come from.
        let (Origin::Sure(beside) | Origin::Guess(beside)) = origin;
        let beside = beside.clone();
        let command = claude(prompt, self.branch(&beside.cwd));
        self.close();
        start_here(&command, &beside);
        false
    }

    /// Opens what `new` opens, in the directory of the pane you came from:
    /// your shell beside that pane, a tab, or a session. A pane or a tab opens
    /// as the menu closes; a session waits for `WORKTREES` to say whether the
    /// directory is a worktree, to be named for it. A key pressed before
    /// zellij says which pane you came from opens nothing. Says whether to
    /// draw.
    fn open(&mut self, new: New) -> bool {
        let Phase::Choosing(_, Fetch::Ready(origin)) = &self.phase else {
            return false;
        };
        // Over a floating terminal, beside the pane under the menu, which you
        // may not have come from, as an agent starts.
        let beside = origin.beside().clone();
        match new {
            New::Pane => {
                self.close();
                shell_here(&beside);
            },
            New::Tab => {
                self.close();
                tab_here(&beside.cwd);
            },
            New::Session => {
                ask_name(&beside.cwd);
                if let Phase::Choosing(keys, _) = std::mem::take(&mut self.phase) {
                    self.phase = Phase::Naming(keys, beside.cwd);
                }
            },
        }
        false
    }

    /// What `WORKTREES` said of the directory a session starts in: the menu
    /// closes, and the session starts, named as the picker names that
    /// directory, with a postfix when a session zellij lists has the name.
    /// When zellij cannot list them, the menu says so and starts none: a name
    /// one has would take you to that session instead. Says whether to draw.
    fn named(&mut self, exit_code: Option<i32>, stdout: &[u8]) -> bool {
        let Phase::Naming(_, cwd) = &self.phase else {
            return false;
        };
        let cwd = cwd.clone();
        let sessions = match get_session_list() {
            Ok(snapshot) => listed(snapshot),
            Err(error) => {
                let reason = format!("menu: zellij did not list the sessions: {error}");
                self.phase = Phase::Wrong(reason);
                self.place();
                return true;
            },
        };
        self.close();
        // `WORKTREES` failing leaves the directory its own name, as the picker
        // leaves it.
        start_session(&cwd, (exit_code == Some(0)).then_some(stdout), &sessions.taken(None));
        false
    }

    /// Closes the pane you came from, and its tab once nothing else holds the
    /// tab open, or kills its session, or asks first when that would end an
    /// agent at work. A key pressed before zellij says which pane you came
    /// from closes nothing, and over a tab with a floating terminal, which
    /// may be the one you came from, no pane closes. Says whether to draw.
    fn delete(&mut self, key: char, scope: Scope) -> bool {
        let Phase::Choosing(_, Fetch::Ready(origin)) = &self.phase else {
            return false;
        };
        let then = self.plan(origin, scope).map(|plan| Asked::then(key, scope, plan));
        let Phase::Choosing(keys, Fetch::Ready(origin)) = std::mem::take(&mut self.phase) else {
            return false;
        };
        self.then(keys, origin, then)
    }

    /// What the delete key for `scope` does to the pane you came from, as
    /// zellij says it stands now, or why it does nothing: no pane closes that
    /// is a guess, nor while what zellij said of the tabs and of the panes
    /// disagrees on which tab is yours. The session is the same whichever
    /// pane you came from.
    fn plan(&self, origin: &Origin, scope: Scope) -> Result<Plan, String> {
        let beside = match (origin, scope) {
            (Origin::Guess(_), Scope::Pane) => return Err(UNSURE_PANE.to_owned()),
            (Origin::Sure(beside) | Origin::Guess(beside), _) => beside,
        };
        if scope == Scope::Pane && self.tab_panes().is_none() {
            return Err(BEHIND.to_owned());
        }
        self.subject(beside.pane).ok_or_else(|| NO_SESSION.to_owned())?.plan(scope)
    }

    /// Goes ahead with what a delete key does, asks first, or says why it
    /// does nothing, for the pane you came from. Says whether to draw.
    fn then(&mut self, keys: Keys, origin: Origin, then: Result<Then, String>) -> bool {
        match then {
            Err(reason) => self.phase = Phase::Wrong(reason),
            Ok(Then::Go(act)) => return self.run(&act),
            Ok(Then::Ask(asked)) => self.phase = Phase::Asking(keys, origin, asked),
        }
        self.place();
        true
    }

    /// The pane `id` you came from, as a delete key deletes it or its
    /// session: what the session holds, and its agents once the daemon has
    /// said where they are. None until zellij has said which session the
    /// menu is in.
    fn subject(&self, id: u32) -> Option<close::Subject> {
        let session = self.session.as_deref()?;
        let manifest = self.panes.as_ref()?;
        let leaves = tree::leaves(session, &self.tabs, manifest, &self.places, &[]);
        let agents = close::agents_of(&self.agents, session, &leaves);
        let session = session.to_owned();
        Some(close::Subject::new(session, id, &leaves, agents, self.refuge.clone()))
    }

    /// Goes ahead with the delete key that asked, planned again from what
    /// zellij says now: once that changed while you read, the key does what
    /// it does now, and asks again when that ends an agent at work. Says
    /// whether to draw.
    fn go_ahead(&mut self) -> bool {
        let Phase::Asking(keys, origin, asked) = std::mem::take(&mut self.phase) else {
            return false;
        };
        let then = self.plan(&origin, asked.scope).map(|now| asked.yes(now));
        self.then(keys, origin, then)
    }

    /// Back to the keys, from a delete key that asked. Says whether to draw.
    fn back(&mut self) -> bool {
        let Phase::Asking(keys, origin, _) = std::mem::take(&mut self.phase) else {
            return false;
        };
        self.phase = Phase::Choosing(keys, Fetch::Ready(origin));
        self.place();
        true
    }

    /// Asks zellij for what a delete key closes, in the session the menu is
    /// in. The menu closes, but stays while you leave the session, for the
    /// session to end once you are gone. Says whether to draw.
    fn run(&mut self, act: &Act) -> bool {
        match act.run(BTreeMap::new()) {
            Ran::Leaving(leaving) => {
                self.phase = Phase::Leaving(leaving);
                set_timeout(LEAVING_TICK);
            },
            Ran::Done => return self.close(),
        }
        self.place();
        true
    }

    /// While you leave the session the menu is in: asks zellij whether you
    /// are gone, and says why the session stays once you have taken too long.
    /// Says whether to draw.
    pub fn tick(&mut self) -> bool {
        let Phase::Leaving(leaving) = &mut self.phase else {
            return false;
        };
        // The one menu of its plugin, and the one ask of the clients: its
        // answers are this menu's.
        let asking = BTreeMap::from([(CONTEXT_KEY.to_owned(), close::CLIENTS_VALUE.to_owned())]);
        match leaving.tick(asking) {
            Ok(()) => {
                set_timeout(LEAVING_TICK);
                false
            },
            Err(reason) => {
                self.phase = Phase::Wrong(reason);
                self.place();
                true
            },
        }
    }

    /// What zellij said, as it listed the clients of the session, while you
    /// leave it: kills the session once your client is gone.
    fn clients(&mut self, exit_code: Option<i32>, stdout: &[u8]) {
        let Phase::Leaving(leaving) = &mut self.phase else {
            return;
        };
        if leaving.listed(self.client, exit_code, stdout) {
            self.phase = Phase::Done;
        }
    }

    /// Keeps the pane you came from, and the directory it works in, as the
    /// panes and the tabs change, and asks git the branch checked out there.
    /// Says whether the pane changed.
    fn follow(&mut self) -> bool {
        let followed = self.follow_origin();
        let asked = self.asked.as_ref().map(|(asked, _)| asked.as_str());
        if let Some(cwd) = self.cwd().filter(|cwd| asked != Some(*cwd)) {
            let cwd = cwd.to_owned();
            run_command(
                &["git", "-C", &cwd, "branch", "--show-current"],
                BTreeMap::from([
                    (CONTEXT_KEY.to_owned(), BRANCH_VALUE.to_owned()),
                    (DIR_KEY.to_owned(), cwd.clone()),
                ]),
            );
            self.asked = Some((cwd, None));
        }
        self.ask_agents();
        followed
    }

    /// Asks once which session the menu is in, and then the daemon what the
    /// agents are doing and where the panes work: a delete key asks first
    /// before it ends an agent at work. A menu over no terminal has nothing
    /// to delete, and asks none of it.
    fn ask_agents(&mut self) {
        let over_a_pane = matches!(self.phase, Phase::Choosing(_, Fetch::Ready(_)));
        if self.session.is_some() || !over_a_pane {
            return;
        }
        let Ok(snapshot) = get_session_list() else {
            return;
        };
        let (current, others): (Vec<&SessionInfo>, Vec<&SessionInfo>) =
            snapshot.live_sessions.iter().partition(|session| session.is_current_session);
        let Some(session) = current.first().map(|session| session.name.clone()) else {
            return;
        };
        let others =
            others.iter().map(|other| (other.name.as_str(), Age::new(other.creation_time)));
        self.refuge = close::refuge(others);
        let asking = (CONTEXT_KEY.to_owned(), daemon::CONTEXT_VALUE.to_owned());
        run_command(&daemon::LOOK, BTreeMap::from([asking]));
        self.session = Some(session);
    }

    /// What the daemon answered: what the agents are doing and where the
    /// panes work, or why it did not say, which a delete key says as it asks
    /// first.
    fn heard(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        match daemon::looked(exit_code, stdout, stderr) {
            Ok(host) => {
                self.places.heard(&host.sessions);
                self.agents.heard(&host.agents, Moment::now());
            },
            Err(why) => self.agents.fail(why),
        }
        self.seat_agents();
    }

    /// Keeps the terminal you came from as zellij says, and whether that is a
    /// guess. A pane that was a guess stays one: once a floating terminal you
    /// may have come from closes, the pane under it is no surer. A search box
    /// over the skills of a directory that is no longer where an agent starts
    /// closes, back to the keys: it would start one where it may not run.
    fn follow_origin(&mut self) -> bool {
        let pane = match self.origin_pane() {
            Fetch::Waiting => return false,
            Fetch::Ready(pane) => Some(pane),
            Fetch::Failed(_) => None,
        };
        let floats = self.floats_a_terminal();
        let Phase::Choosing(keys, origin) = &mut self.phase else {
            return false;
        };
        let was = match &*origin {
            Fetch::Ready(was) if Some(was.beside().pane) == pane => Some(was),
            Fetch::Ready(_) | Fetch::Waiting | Fetch::Failed(_) => None,
        };
        let guess = floats || matches!(was, Some(Origin::Guess(_)));
        let came: fn(Beside) -> Origin = if guess { Origin::Guess } else { Origin::Sure };
        let now = match (pane, was) {
            (None, _) => Fetch::Failed(NO_PANE.to_owned()),
            (Some(_), Some(was)) => Fetch::Ready(came(was.beside().clone())),
            (Some(pane), None) => match get_pane_cwd(PaneId::Terminal(pane)) {
                Ok(cwd) => {
                    Fetch::Ready(came(Beside { pane, cwd: cwd.to_string_lossy().into_owned() }))
                },
                Err(error) => Fetch::Failed(format!(
                    "menu: zellij cannot say where pane {pane} works: {error}"
                )),
            },
        };
        let cwd = match &now {
            Fetch::Ready(now) => Some(now.beside().cwd.as_str()),
            Fetch::Waiting | Fetch::Failed(_) => None,
        };
        if keys.search().is_some_and(|search| Some(search.cwd()) != cwd) {
            keys.search = None;
        }
        let followed = *origin != now;
        *origin = now;
        followed
    }

    /// The terminal you came from: `Waiting` until zellij has said what the
    /// tab you are in holds, and while it is saying so, and `Failed` when that
    /// is no terminal with the focus.
    fn origin_pane(&self) -> Fetch<u32> {
        let Some(panes) = self.tab_panes() else {
            return Fetch::Waiting;
        };
        origin(panes).map_or_else(|| Fetch::Failed(NO_PANE.to_owned()), Fetch::Ready)
    }

    /// Whether the tab you are in has a floating terminal. The menu takes the
    /// floating focus from the pane you were in as it opens, and zellij then
    /// says the pane under it has the focus: you may have come from either.
    fn floats_a_terminal(&self) -> bool {
        let floating = |pane: &PaneInfo| pane.is_floating && !pane.is_plugin && !pane.is_suppressed;
        self.tab_panes().is_some_and(|panes| panes.iter().any(floating))
    }

    /// The panes of the tab you are in, once zellij has said, and while what
    /// it said of the tabs and of the panes agrees. The two come one after the
    /// other, the panes by the position of their tab, and a tab that opens or
    /// closes before yours moves that position: until both have said so, the
    /// panes at the active tab's position are another tab's. The menu floats
    /// in the tab you are in, so the panes it stands among are that tab's.
    fn tab_panes(&self) -> Option<&[PaneInfo]> {
        let tab = self.tabs.iter().find(|tab| tab.active)?;
        let panes = self.panes.as_ref()?.panes.get(&tab.position)?;
        let itself = |pane: &PaneInfo| pane.is_plugin && pane.id == self.plugin_id;
        panes.iter().any(itself).then_some(panes.as_slice())
    }

    /// Stands the menu at the bottom right of the tab, as big as the group
    /// open needs.
    fn place(&mut self) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.active) else {
            return;
        };
        let Some((width, height)) = render::menu_size(self) else {
            return;
        };
        let Some(spot) = corner(tab.viewport_columns, tab.viewport_rows, width, height) else {
            return;
        };
        if self.placed == Some(spot) {
            return;
        }
        let (x, y, width, height) = spot;
        let Some(coordinates) = FloatingPaneCoordinates::new(
            Some(x.to_string()),
            Some(y.to_string()),
            Some(width.to_string()),
            Some(height.to_string()),
            None,
            Some(true),
        ) else {
            return;
        };
        change_floating_panes_coordinates(vec![(PaneId::Plugin(self.plugin_id), coordinates)]);
        self.placed = Some(spot);
    }
}

/// The sessions zellij lists, live and resurrectable, by the names a session
/// that starts cannot take.
fn listed(snapshot: SessionListSnapshot) -> Sessions {
    let live = snapshot.live_sessions.into_iter();
    let dead = snapshot.resurrectable_sessions.into_iter();
    Sessions {
        live: live.map(|it| Session { name: it.name, age: Age::new(it.creation_time) }).collect(),
        dead: dead.map(|(name, age)| Session { name, age: Age::new(age) }).collect(),
    }
}

/// A box `width` by `height` in the bottom right of a viewport `columns` by
/// `rows`, cut to fit it. A viewport with no room has no corner.
fn corner(columns: usize, rows: usize, width: usize, height: usize) -> Option<Spot> {
    let width = width.min(columns);
    let height = height.min(rows);
    (width > 0 && height > 0).then(|| (columns - width, rows - height, width, height))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    // zellij-tile's prelude has a prost of its own, of another version.
    use ::prost::Message as _;
    use luneta_protocol::v1;

    use super::*;
    use crate::agents::given;

    /// The menu as zellij hands it over: the children of `menu`, as KDL.
    const MENU: &str = r#"
        c "Claude" {
            t "/thermo-nuclear-review"
            m "/make-illegal-states-unrepresentable"
            s "/tigerstyle"
        }
        r "review the diff against main"
    "#;

    fn keys(text: &str) -> Keys {
        Keys::new(Some(text)).expect("a menu")
    }

    fn shown(keys: &Keys) -> Vec<char> {
        keys.entries().iter().map(Entry::key).collect()
    }

    fn bare(key: BareKey) -> KeyWithModifier {
        KeyWithModifier::new(key)
    }

    fn char(key: char) -> KeyWithModifier {
        bare(BareKey::Char(key))
    }

    #[test]
    fn a_key_opens_its_group_and_a_key_in_it_starts_its_prompt() {
        let mut keys = keys(MENU);
        assert_eq!(shown(&keys), ['/', 'c', 'r', 'n', 'd']);
        assert!(keys.names().is_empty());

        assert_eq!(keys.key(&char('x')), Pressed::Nothing);
        assert_eq!(keys.key(&char('c')), Pressed::Moved);
        assert_eq!(shown(&keys), ['t', 'm', 's']);
        assert_eq!(keys.names(), ["Claude"]);
        // A key of the group before is not one of this group's.
        assert_eq!(keys.key(&char('r')), Pressed::Nothing);
        assert_eq!(keys.key(&char('s')), Pressed::Start("/tigerstyle".to_owned()));

        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert_eq!(shown(&keys), ['/', 'c', 'r', 'n', 'd']);
        assert_eq!(keys.key(&char('r')), Pressed::Start("review the diff against main".to_owned()));
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Close);
        keys.key(&char('c'));
        assert_eq!(keys.key(&bare(BareKey::Esc)), Pressed::Close);
        assert_eq!(keys.key(&char('c').with_ctrl_modifier()), Pressed::Close);
    }

    /// What the finder of a test prints: no plugin, and a skill of yours,
    /// `tigerstyle`.
    const FOUND: &[u8] = b"[]\0\0skills/tigerstyle/SKILL.md\0description: Apply TigerStyle.\0";

    /// The skills a test's finder finds, with nothing said of what they do.
    fn found_skills(names: &[&str]) -> Fetch<Vec<skills::Skill>> {
        let skill = |name: &&str| skills::Skill {
            name: skills::Name::new((*name).to_owned()).expect("a name"),
            description: String::new(),
        };
        Fetch::Ready(names.iter().map(skill).collect())
    }

    /// What the search box holds: what is typed, and the skill the cursor is
    /// on. None while it is closed.
    fn searched(keys: &Keys) -> Option<(&str, Option<&str>)> {
        let search = keys.search()?;
        Some((search.term(), search.chosen().map(|skill| skill.name.as_str())))
    }

    /// `/` pressed at the top of `keys`, and the search box the menu opens
    /// for it.
    fn slash(keys: &mut Keys) {
        assert_eq!(keys.key(&char('/')), Pressed::Search);
        keys.open_search(Search::ask("/w/elo"));
    }

    /// `/` at the top of the menu asks for the search box, which the menu
    /// opens over the skills of a directory. The box takes every key: a key
    /// of the menu's types, `Down` and `Up` move the cursor, and `Enter`
    /// starts an agent on the skill it is on, as a slash command. `Esc` goes back to the keys, and so does `Backspace` with
    /// nothing typed; `Ctrl c` closes the menu.
    #[test]
    fn slash_searches_the_skills_and_enter_starts_the_one_picked() {
        let mut keys = keys(MENU);
        slash(&mut keys);
        assert_eq!(searched(&keys), Some(("", None)));
        assert_eq!(keys.key(&char('c')), Pressed::Moved);
        assert_eq!(keys.key(&bare(BareKey::Enter)), Pressed::Nothing);
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert!(keys.found(found_skills(&["code-review", "review:deep", "tigerstyle"])));
        assert_eq!(searched(&keys), Some(("", Some("code-review"))));
        assert_eq!(keys.key(&bare(BareKey::Down)), Pressed::Moved);
        assert_eq!(keys.key(&bare(BareKey::Down)), Pressed::Moved);
        assert_eq!(keys.key(&bare(BareKey::Up)), Pressed::Moved);
        assert_eq!(keys.key(&bare(BareKey::Enter)), Pressed::Start("/review:deep".to_owned()));
        for c in "tig".chars() {
            keys.key(&char(c));
        }
        assert_eq!(searched(&keys), Some(("tig", Some("tigerstyle"))));
        assert_eq!(keys.key(&char('t').with_alt_modifier()), Pressed::Nothing);

        assert_eq!(keys.key(&bare(BareKey::Esc)), Pressed::Moved);
        assert_eq!(searched(&keys), None);
        assert_eq!(shown(&keys), ['/', 'c', 'r', 'n', 'd']);
        slash(&mut keys);
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert_eq!(searched(&keys), None);
        slash(&mut keys);
        assert_eq!(keys.key(&char('c').with_ctrl_modifier()), Pressed::Close);
    }

    /// Every menu has luneta's `/` before your keys and its New and Delete
    /// groups after them, and one with none of yours too: `n p`, `n t` and
    /// `n s` open a pane, a tab and a session, `d d` closes the pane the menu
    /// is for, and `d s` kills its session.
    #[test]
    fn every_menu_has_luneta_s_new_and_delete_keys() {
        for (configured, root) in [
            (Some(MENU), &['/', 'c', 'r', 'n', 'd'][..]),
            (None, &['/', 'n', 'd']),
            (Some(""), &['/', 'n', 'd']),
            (Some("// nothing but a comment"), &['/', 'n', 'd']),
        ] {
            let mut keys = Keys::new(configured).expect("a menu");
            assert_eq!(shown(&keys), root, "{configured:?}");
            assert_eq!(keys.key(&char('n')), Pressed::Moved);
            assert_eq!(keys.names(), ["New"]);
            assert_eq!(keys.key(&char('p')), Pressed::New(New::Pane));
            assert_eq!(keys.key(&char('t')), Pressed::New(New::Tab));
            assert_eq!(keys.key(&char('s')), Pressed::New(New::Session));
            assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
            assert_eq!(keys.key(&char('d')), Pressed::Moved);
            assert_eq!(keys.names(), ["Delete"]);
            assert_eq!(keys.key(&char('d')), Pressed::Delete { key: 'd', scope: Scope::Pane });
            assert_eq!(keys.key(&char('s')), Pressed::Delete { key: 's', scope: Scope::Session });
        }
    }

    #[test]
    fn a_menu_that_cannot_be_read_says_which_key_is_wrong() {
        let one_string = "menu: \"t\" takes one string";
        for (text, reason) in [
            (r#"c "Claude" {}"#, "menu: no keys under \"c\""),
            (r#"c "Claude" { tn "/tigerstyle"; }"#, "menu: \"c tn\" is no key"),
            (r#"c "Claude" { t "/tigerstyle"; t "/a"; }"#, "menu: two entries for \"c t\""),
            ("t", one_string),
            (r#"t " ""#, one_string),
            (r#"t "/tigerstyle" "/thermo-nuclear-review""#, one_string),
            ("t 1", one_string),
            (r#"t prompt="/tigerstyle""#, one_string),
            (r#"d "/tigerstyle""#, "menu: \"d\" is luneta's"),
            (r#"n "notes""#, "menu: \"n\" is luneta's, for its New group"),
            (r#""/" "/tigerstyle""#, "menu: \"/\" is luneta's, for its search"),
        ] {
            let parsed = Keys::new(Some(text));
            let error = parsed.err().unwrap_or_else(|| panic!("{text:?} parsed"));
            assert!(error.starts_with(reason), "{text:?}: {error}");
        }
    }

    #[test]
    fn a_prompt_that_runs_a_skill_names_the_agent_for_it_and_its_branch() {
        for (prompt, branch, command) in [
            ("/tigerstyle", None, &["claude", "--name", "tigerstyle", "/tigerstyle"][..]),
            ("/tigerstyle", Some("feat/menu"), &["claude", "--name", "tigerstyle: feat/menu", "/tigerstyle"]),
            (
                "/code-review since main",
                None,
                &["claude", "--name", "code-review", "/code-review since main"],
            ),
            ("/plugin:skill", None, &["claude", "--name", "plugin:skill", "/plugin:skill"]),
            ("review the diff", Some("main"), &["claude", "review the diff"]),
            ("/", None, &["claude", "/"]),
        ] {
            assert_eq!(claude(prompt, branch), command, "{prompt:?} on {branch:?}");
        }
    }

    /// `y`, or the delete key again, goes ahead; `n` and `Backspace` go back;
    /// `Esc` and `Ctrl c` close the menu; anything else does nothing.
    #[test]
    fn a_delete_key_that_asks_goes_ahead_on_yes_or_on_itself() {
        for (key, said) in [
            (char('y'), Answer::Yes),
            (char('d'), Answer::Yes),
            (char('n'), Answer::Back),
            (bare(BareKey::Backspace), Answer::Back),
            (bare(BareKey::Esc), Answer::Close),
            (char('c').with_ctrl_modifier(), Answer::Close),
            (char('s'), Answer::Nothing),
            (char('y').with_ctrl_modifier(), Answer::Nothing),
            (bare(BareKey::Enter), Answer::Nothing),
        ] {
            assert_eq!(answer(&key, 'd'), said, "{key:?}");
        }
    }

    /// The menu the key binding opens, floating, for the pane you came from.
    fn bound() -> Menu {
        Menu::new(0, 1, &BTreeMap::from([(ENTRIES.to_owned(), MENU.to_owned())]))
    }

    /// The key binding opens the menu for the pane you are in, which zellij
    /// says a moment later. A key that starts an agent before then starts
    /// none, rather than one beside no pane, wherever zellij runs.
    #[test]
    fn a_key_pressed_before_zellij_says_where_you_are_starts_nothing() {
        let mut menu = bound();
        assert!(menu.key(&char('c')));
        assert!(!menu.key(&char('t')));
        assert!(matches!(menu.phase, Phase::Choosing(_, Fetch::Waiting)));
        let names = match menu.shown() {
            Some(Shown::Keys(keys)) => keys.names(),
            _ => Vec::new(),
        };
        assert_eq!(names, ["Claude"]);
    }

    /// A menu that asked zellij to close it, as one that started an agent
    /// has, takes no key on its way before zellij has: a second press starts
    /// no second agent.
    #[test]
    fn a_menu_that_closed_takes_no_more_keys() {
        let mut menu = bound();
        assert!(!menu.key(&bare(BareKey::Esc)));
        assert!(menu.shown().is_none());
        assert!(!menu.key(&char('r')));
        assert!(menu.shown().is_none());
    }

    /// The agent is named for the branch checked out where it starts. An
    /// answer git gives late, for a directory asked about before, names it
    /// for none.
    #[test]
    fn a_late_git_answer_for_another_directory_names_no_agent() {
        let mut menu = bound();
        let elo = Beside { pane: 2, cwd: "/w/elo".to_owned() };
        menu.phase = Phase::Choosing(keys(MENU), Fetch::Ready(Origin::Sure(elo)));
        // Which session it is in, which zellij answers on stdin.
        menu.session = Some("luneta".to_owned());
        menu.follow();
        let branch = |dir: &str| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), BRANCH_VALUE.to_owned()),
                (DIR_KEY.to_owned(), dir.to_owned()),
            ])
        };
        menu.ran(Some(0), b"main\n", b"", &branch("/w/old"));
        assert_eq!(menu.branch("/w/elo"), None);
        menu.ran(Some(0), b"feat/menu\n", b"", &branch("/w/elo"));
        assert_eq!(menu.branch("/w/elo"), Some("feat/menu"));
    }

    /// The key binding's `/` asks the finder for the skills where the pane
    /// you came from works, and takes the answer for that directory alone.
    /// Pressed before zellij says which pane that is, it opens no box. Once
    /// zellij says you came from a pane in another directory, the box closes,
    /// and the answer for the directory it looked in comes to nothing.
    #[test]
    fn the_key_binding_s_search_takes_the_skills_of_the_pane_you_came_from() {
        let search_open = |menu: &Menu| match menu.shown() {
            Some(Shown::Keys(keys)) => keys.search().map(Search::count),
            _ => None,
        };
        let mut menu = bound();
        assert!(!menu.key(&char('/')));
        assert_eq!(search_open(&menu), None);

        let skills_in = |dir: &str| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), skills::CONTEXT_VALUE.to_owned()),
                (DIR_KEY.to_owned(), dir.to_owned()),
            ])
        };
        let mut menu = bound_for_two(true);
        assert!(menu.key(&char('/')));
        assert_eq!(search_open(&menu), Some(0));
        assert!(!menu.ran(Some(0), FOUND, b"", &skills_in("/w/old")));
        assert_eq!(search_open(&menu), Some(0));
        assert!(menu.ran(Some(0), FOUND, b"", &skills_in("/w/elo")));
        assert_eq!(search_open(&menu), Some(1));
        // It keeps its size as you type, however many skills match.
        let size = render::menu_size(&menu);
        assert!(menu.key(&char('z')));
        assert_eq!(search_open(&menu), Some(0));
        assert_eq!(render::menu_size(&menu), size);
        assert!(menu.key(&bare(BareKey::Esc)));
        assert_eq!(search_open(&menu), None);
        assert!(menu.shown().is_some());

        // Pane 2 exits while the box looks in `/w/elo`, and zellij says the
        // focus is on pane 3, in `/w/old`: set here as zellij would say it,
        // since asking where a pane works reads stdin in a test.
        assert!(menu.key(&char('/')));
        assert_eq!(search_open(&menu), Some(0));
        let old = Beside { pane: 3, cwd: "/w/old".to_owned() };
        if let Phase::Choosing(_, origin) = &mut menu.phase {
            *origin = Fetch::Ready(Origin::Sure(old));
        }
        let shell = |id| PaneInfo { id, is_selectable: true, ..PaneInfo::default() };
        let itself = PaneInfo { is_plugin: true, is_floating: true, ..shell(0) };
        let tab = vec![PaneInfo { is_focused: true, ..shell(3) }, itself];
        menu.panes(PaneManifest { panes: HashMap::from([(0, tab)]) });
        assert_eq!(search_open(&menu), None);
        assert!(!menu.ran(Some(0), FOUND, b"", &skills_in("/w/elo")));
        assert!(menu.key(&char('/')));
        assert!(!menu.ran(Some(0), FOUND, b"", &skills_in("/w/elo")));
        assert!(menu.ran(Some(0), FOUND, b"", &skills_in("/w/old")));
        assert_eq!(search_open(&menu), Some(1));
    }

    /// The key binding's menu, floating over pane 2 of session `luneta`,
    /// which shares tab 7 with pane 3, with `elo` to go to before `luneta`
    /// ends, and the daemon saying a busy agent sits in pane 3, once
    /// `answered`.
    fn bound_for_two(answered: bool) -> Menu {
        let mut menu = bound();
        let shell = |id| PaneInfo { id, is_selectable: true, ..PaneInfo::default() };
        let itself = PaneInfo { is_plugin: true, is_floating: true, ..shell(0) };
        let tab = vec![PaneInfo { is_focused: true, ..shell(2) }, shell(3), itself];
        menu.panes = Some(PaneManifest { panes: HashMap::from([(0, tab)]) });
        menu.tabs = vec![TabInfo { position: 0, tab_id: 7, active: true, ..TabInfo::default() }];
        let elo = Beside { pane: 2, cwd: "/w/elo".to_owned() };
        menu.phase = Phase::Choosing(keys(MENU), Fetch::Ready(Origin::Sure(elo)));
        menu.session = Some("luneta".to_owned());
        menu.refuge = Some("elo".to_owned());
        if answered {
            let said = host(&[given::agent(100, "luneta", 3, "/w/elo", "busy")]);
            let answered = (CONTEXT_KEY.to_owned(), daemon::CONTEXT_VALUE.to_owned());
            menu.ran(Some(0), &said, b"", &BTreeMap::from([answered]));
        }
        menu
    }

    /// The host as `luneta watch` writes it out: session `luneta`, with panes
    /// 2 and 3 in `/w/elo`, and `agents`.
    fn host(agents: &[v1::Agent]) -> Vec<u8> {
        let pane = |id| v1::Pane { id, cwd: Some("/w/elo".to_owned()), ..v1::Pane::default() };
        let luneta = v1::Session {
            name: "luneta".to_owned(),
            panes: Some(v1::Panes { panes: vec![pane(2), pane(3)] }),
            ..v1::Session::default()
        };
        let version = Some(v1::Version { pid: 7, seq: 0 });
        let sessions = vec![luneta];
        v1::Host { version, sessions, agents: agents.to_vec(), ..v1::Host::default() }
            .encode_to_vec()
    }

    fn question(menu: &Menu) -> Option<&str> {
        match menu.shown()? {
            Shown::Asking(_, question) => Some(question),
            Shown::Keys(_) | Shown::Leaving(_) | Shown::Wrong(_) => None,
        }
    }

    /// The key binding's menu closes the pane you came from, or kills its
    /// session: at once with no agent at work to end, and asking first until
    /// the daemon has said where the agents sit, and saying why when it does
    /// not answer.
    #[test]
    fn the_key_binding_s_menu_deletes_the_pane_you_came_from() {
        let mut menu = bound_for_two(true);
        assert!(menu.key(&char('d')));
        assert!(menu.key(&char('s')));
        assert_eq!(question(&menu), Some("Kill session luneta? 1 agent, 1 busy"));
        assert!(menu.key(&bare(BareKey::Backspace)));
        assert!(question(&menu).is_none());
        assert!(menu.key(&char('s')));
        assert!(menu.key(&char('s')));
        assert!(matches!(menu.shown(), Some(Shown::Leaving("elo"))));

        let mut menu = bound_for_two(true);
        menu.key(&char('d'));
        assert!(!menu.key(&char('d')));
        assert!(menu.shown().is_none());

        let mut menu = bound_for_two(false);
        menu.key(&char('d'));
        assert!(menu.key(&char('d')));
        assert_eq!(question(&menu), Some("Close the pane? luneta does not know its agents yet"));

        let mut menu = bound_for_two(false);
        let refused = b"luneta: lunetad.sock: transport error: Connection refused (os error 111)";
        let answered = (CONTEXT_KEY.to_owned(), daemon::CONTEXT_VALUE.to_owned());
        menu.ran(Some(1), b"", refused, &BTreeMap::from([answered]));
        menu.key(&char('d'));
        assert!(menu.key(&char('d')));
        let said = "Close the pane? luneta cannot tell which agents it ends: lunetad does not \
                    answer: luneta: lunetad.sock: transport error: Connection refused (os error 111)";
        assert_eq!(question(&menu), Some(said));
    }

    /// Going ahead plans the delete key again: a pane that opened in the tab
    /// while you read holds it open, and the key asks again, to close the
    /// pane you came from alone.
    #[test]
    fn the_key_binding_s_menu_goes_ahead_with_what_the_tab_holds_now() {
        let shell = |id| PaneInfo { id, is_selectable: true, ..PaneInfo::default() };
        // Tab 7, and tab 8 with pane 9, for closing tab 7 to leave the
        // session another.
        let tab = |ids: &[u32]| {
            let mut tab: Vec<PaneInfo> = ids.iter().map(|id| shell(*id)).collect();
            tab.push(PaneInfo { is_plugin: true, is_floating: true, ..shell(0) });
            PaneManifest { panes: HashMap::from([(0, tab), (1, vec![shell(9)])]) }
        };
        let mut menu = bound_for_two(true);
        menu.tabs.push(TabInfo { position: 1, tab_id: 8, ..TabInfo::default() });
        menu.panes = Some(tab(&[3]));
        let busy = Beside { pane: 3, cwd: "/w/elo".to_owned() };
        menu.phase = Phase::Choosing(keys(MENU), Fetch::Ready(Origin::Sure(busy)));
        menu.key(&char('d'));
        assert!(menu.key(&char('d')));
        assert_eq!(question(&menu), Some("Close its tab? 1 agent, 1 busy"));
        menu.panes = Some(tab(&[3, 4]));
        assert!(menu.key(&char('y')));
        assert_eq!(question(&menu), Some("Close the pane? 1 agent, 1 busy"));
        assert!(!menu.key(&char('y')));
        assert!(menu.shown().is_none());
    }

    /// The key binding's `n s` waits, showing its keys, for `WORKTREES` to
    /// say how the session is named. A key on its way meanwhile does nothing,
    /// and `Esc` closes the menu: the name that comes after starts no session.
    /// zellij lists the sessions it starts one among on stdin, which no test
    /// has, so a session that starts would fail the test.
    #[test]
    fn the_key_binding_s_menu_starts_no_session_once_closed() {
        let naming = BTreeMap::from([(CONTEXT_KEY.to_owned(), NAMING_VALUE.to_owned())]);
        let mut menu = bound_for_two(true);
        assert!(menu.key(&char('n')));
        assert!(!menu.key(&char('s')));
        assert!(!menu.key(&char('s')));
        assert!(matches!(menu.shown(), Some(Shown::Keys(keys)) if keys.names() == ["New"]));
        assert_eq!(menu.cwd(), Some("/w/elo"));
        assert!(!menu.key(&bare(BareKey::Esc)));
        assert!(menu.shown().is_none());
        assert!(!menu.ran(Some(0), b"", b"", &naming));
    }

    /// The menu takes the floating focus from the pane you were in as it
    /// opens, and zellij then says the pane under it has the focus: over a
    /// tab with a floating terminal, you may have come from either. `d d`
    /// closes neither and says so, and goes on doing so once the floating
    /// terminal closes. The session is the same either way, and `d s` kills
    /// it.
    #[test]
    fn over_a_floating_terminal_the_key_binding_s_menu_closes_no_pane() {
        let floating = || {
            let mut menu = bound_for_two(true);
            let terminal =
                PaneInfo { id: 5, is_selectable: true, is_floating: true, ..PaneInfo::default() };
            if let Some(manifest) = &mut menu.panes {
                manifest.panes.entry(0).or_default().push(terminal);
            }
            menu.follow();
            menu
        };
        let closes_none = |menu: &mut Menu| {
            menu.key(&char('d'));
            assert!(menu.key(&char('d')));
            let Some(Shown::Wrong(said)) = menu.shown() else { panic!("no reason") };
            assert!(said.contains("floating"), "{said}");
        };
        closes_none(&mut floating());
        let mut menu = floating();
        if let Some(manifest) = &mut menu.panes {
            manifest.panes.entry(0).or_default().retain(|pane| pane.id != 5);
        }
        menu.follow();
        closes_none(&mut menu);
        let mut menu = floating();
        menu.key(&char('d'));
        menu.key(&char('s'));
        assert_eq!(question(&menu), Some("Kill session luneta? 1 agent, 1 busy"));
    }

    /// zellij says what the tabs are and what the panes are one after the
    /// other, the panes by the position of their tab. Once tab 6, before
    /// yours, closes, tab 7 moves to its position, and until both have said
    /// so, the panes at the active tab's position may be tab 6's: pane 5,
    /// alone in it, is not the pane you came from, and `d d` closes nothing,
    /// rather than tab 7 for it.
    #[test]
    fn a_tab_that_moved_before_zellij_said_so_closes_nothing() {
        let shell = |id| PaneInfo { id, is_selectable: true, ..PaneInfo::default() };
        let focused = PaneInfo { is_focused: true, ..shell(5) };
        let itself = PaneInfo { is_plugin: true, is_floating: true, ..shell(0) };
        let tab =
            |position, tab_id, active| TabInfo { position, tab_id, active, ..TabInfo::default() };
        let yours = vec![shell(2), shell(3), itself];
        for (tabs, panes) in [
            (vec![tab(0, 7, true)], HashMap::from([(0, vec![focused]), (1, yours.clone())])),
            (vec![tab(0, 6, false), tab(1, 7, true)], HashMap::from([(0, yours)])),
        ] {
            let mut menu = bound_for_two(true);
            menu.tabs = tabs;
            menu.panes = Some(PaneManifest { panes });
            assert_eq!(menu.origin_pane(), Fetch::Waiting);
            menu.key(&char('d'));
            assert!(menu.key(&char('d')));
            assert!(matches!(menu.shown(), Some(Shown::Wrong(BEHIND))));
        }
    }

    /// zellij takes a directory a plugin names under `/tmp`, `/data`,
    /// `/cache` or `/host` for one of the plugin's own. The pane the menu
    /// opens in its own session is in the pane's directory all the same.
    #[test]
    fn an_agent_in_your_own_session_starts_in_its_pane_s_directory_wherever_that_is() {
        let command = claude("/tigerstyle", None);
        let dir = |cwd: &str| command_pane(&command, cwd).cwd.expect("a directory");
        for cwd in ["/tmp/pr-123", "/data/w/luneta", "/cache", "/host/x", "/home/you/w"] {
            let dir = dir(cwd);
            for sandbox in ["/tmp", "/data", "/cache", "/host"] {
                assert!(dir.strip_prefix(sandbox).is_err(), "{cwd} as {}", dir.display());
            }
        }
        let here = std::env::temp_dir();
        let there = dir(here.to_str().expect("a path"));
        assert_eq!(there.canonicalize().expect("there"), here.canonicalize().expect("here"));
    }

    /// The key binding opens the menu for the pane you are in. Opened in a
    /// plugin's pane, or any pane that is no terminal, it has none to start an
    /// agent beside, and says so rather than start one wherever zellij puts
    /// it.
    #[test]
    fn a_menu_opened_in_no_terminal_says_so_instead_of_its_keys() {
        let plugin = |id, is_floating| PaneInfo {
            id,
            is_plugin: true,
            is_focused: true,
            is_floating,
            ..PaneInfo::default()
        };
        let shell = PaneInfo { id: 2, is_selectable: true, ..PaneInfo::default() };
        // Another plugin's pane, and the menu itself floating.
        let tab = vec![plugin(1, false), shell, plugin(0, true)];
        let mut menu = bound();
        menu.panes = Some(PaneManifest { panes: HashMap::from([(0, tab)]) });
        menu.tabs = vec![TabInfo { position: 0, active: true, ..TabInfo::default() }];
        menu.follow();
        let said = match menu.shown() {
            Some(Shown::Wrong(reason)) => Some(reason),
            _ => None,
        };
        assert_eq!(said, Some(NO_PANE));
    }

    #[test]
    fn the_menu_stands_in_the_bottom_right_and_never_past_the_tab() {
        assert_eq!(corner(120, 40, 30, 7), Some((90, 33, 30, 7)));
        assert_eq!(corner(20, 5, 30, 7), Some((0, 0, 20, 5)));
        assert_eq!(corner(0, 0, 30, 7), None);
    }
}
