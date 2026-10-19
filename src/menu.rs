//! The menu: a key, then another, and an agent is at work, as which-key shows
//! the keys in neovim.
//!
//! Its `menu` holds its entries: a key each, and under a key that opens a
//! group, the keys of the group. The menu shows the keys of the group open and
//! what each does. A key with nothing under it starts Claude Code on its
//! prompt, in a pane of its own beside a pane, in the directory that pane
//! works in. Every menu has luneta's own New and Delete groups besides: under
//! `n`, a shell beside that pane, a tab or a session, in the directory it
//! works in; under `d`, closing that pane or killing its session. In the
//! sidebar, a key of the
//! menu opens it at the bottom of the sidebar, for the row highlighted.
//! `mode "menu"` makes the plugin the menu for the pane you came from,
//! floating at the bottom right of the tab you are in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::*;

use crate::agents::{self, AgentSet, Live, Seat};
use crate::brief;
use crate::checkouts::{Checkouts, DIR_KEY};
use crate::close::{self, Act, Leaving, Plan, Ran, Scope};
use crate::dirs::{self, WORKTREES};
use crate::dock::TabId;
use crate::elapsed::Age;
use crate::fetch::Fetch;
use crate::places::{self, Places, SESSION_KEY};
use crate::render;
use crate::sessions::{Selection, Session, Sessions, Taken};
use crate::tree::{self, origin};
use crate::context::CONTEXT_KEY;

/// What holds the entries in the menu's configuration: a node with children,
/// which zellij hands over as their KDL.
pub const ENTRIES: &str = "menu";

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
/// the pane you opened it from is no terminal, as the sidebar is not.
const NO_PANE: &str = "menu: the pane you are in is no terminal, to start an agent beside or to \
                       close; in the sidebar, the menu's keys open it for the row you are on";

/// What the sidebar's menu says of a key that starts an agent, or opens a
/// pane, a tab or a session, on a session's row, which the cursor is only on
/// while it lists none of the session's panes: folded, or with none to list.
const NO_ROW_PANE: &str = "menu: a session's row has no pane to open beside, nor a directory to \
                           open in; highlight a pane, unfolding its session first if it is folded";

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
                           tab or from the pane under it, and closes neither; in the sidebar, d d \
                           closes the row you are on";

/// What a key with nothing under it starts.
const CLAUDE: &str = "claude";

/// The floating menu asking git which branch is checked out where an agent
/// starts.
pub const BRANCH_VALUE: &str = "branch";
/// The sidebar's menu opening an agent's pane in another session, a shell's
/// pane or a tab, or closing a pane or a tab of one, or asking for the
/// clients of a session it takes you out of or to, and the number of the
/// menu that asked.
pub const START_VALUE: &str = "start";
pub const OPENING_VALUE: &str = "opening";
pub const CLOSING_VALUE: &str = "closing";
pub const MENU_KEY: &str = "luneta_menu";
/// A menu asking whether the directory a session starts in is a worktree, to
/// name the session for it.
pub const NAMING_VALUE: &str = "naming";
/// The sidebar's menu asking which clients the session a tab opens in has,
/// before it takes you there: the tab opens before you go to a session with
/// one, and once you have come to one with none.
pub const ARRIVING_VALUE: &str = "arriving";
/// The tab that opens in another session once you are there, and the
/// session, for the sidebar to say why when it does not open.
pub const TAB_VALUE: &str = "tab";

/// How often, in seconds, the floating menu looks whether you have left the
/// session it is to kill.
const LEAVING_TICK: f64 = 0.1;

/// Where a floating pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

/// A key of the sidebar's own that does more than move the cursor or fold a
/// session. `?` lists each one beside the menu's keys. A command in a group
/// is a key of that group, which a key of the sidebar's opens, as `b` then
/// `s` briefs you on a session.
///
/// The sidebar finds the command a key runs in `ALL`, by `of`, and `?` lists
/// `ALL`, so no key runs a command that `?` leaves out. `key` and `does` match
/// every command, so a command does not compile until it says what `?` shows
/// for it, and the sidebar's `command` matches every command too. A command
/// left out of `ALL` is dead code, which does not compile either, unless a
/// key of the sidebar's names it: that compiles, and `?` leaves it out. Only
/// `dev clippy` stops it, by way of `examples/profile.rs`, which compiles this
/// module without the sidebar's keys.
#[deny(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Narrows the tree to the rows that match what you type.
    Search,
    /// Briefs you on what the agents the scope covers are doing.
    Brief(brief::Scope),
}

impl Command {
    /// Every command, in the order `?` lists them.
    pub const ALL: &[Command] = &[
        Command::Search,
        Command::Brief(brief::Scope::Every),
        Command::Brief(brief::Scope::Session),
        Command::Brief(brief::Scope::Highlighted),
    ];

    /// The command `key` runs at the top of the sidebar, if any: a command in
    /// a group is the group's key.
    pub fn of(key: char) -> Option<Command> {
        let at_the_top = |command: &Command| command.group().is_none() && command.key() == key;
        Command::ALL.iter().copied().find(at_the_top)
    }

    /// Whether `key` opens a group of commands at the top of the sidebar.
    pub fn opens(key: char) -> bool {
        Command::ALL.iter().any(|command| command.group().is_some_and(|(at, _)| at == key))
    }

    /// Whether `key`, at the top of the sidebar, is the sidebar's.
    fn takes(key: char) -> bool {
        Command::of(key).is_some() || Command::opens(key)
    }

    /// The command's key, in its group if it has one.
    pub const fn key(self) -> char {
        match self {
            Command::Search => '/',
            Command::Brief(brief::Scope::Every) => 'z',
            Command::Brief(brief::Scope::Session) => 's',
            Command::Brief(brief::Scope::Highlighted) => 'h',
        }
    }

    /// What `?` says the command does.
    pub fn does(self) -> &'static str {
        match self {
            Command::Search => "Search",
            Command::Brief(brief::Scope::Every) => "Every session",
            Command::Brief(brief::Scope::Session) => "This session",
            Command::Brief(brief::Scope::Highlighted) => "Highlighted",
        }
    }

    /// The group the command is in, by the key that opens it and the name
    /// `?` shows for it. None for a command its own key runs.
    const fn group(self) -> Option<(char, &'static str)> {
        match self {
            Command::Search => None,
            Command::Brief(_) => Some(('b', "Brief")),
        }
    }
}

/// No two commands share a key where they are, at the top of the sidebar or
/// in one group, and no group opens on the key of a command at the top: `of`
/// would find the first, and `?` would show both. Nor does a command at the
/// top, or a group of them, take the key of luneta's New or Delete group:
/// the sidebar keeps a key for its command, and the group would be out of
/// reach there, and gone from `?`.
const _: () = {
    let mut at = 0;
    while at < Command::ALL.len() {
        let one = Command::ALL[at];
        let top = match one.group() {
            Some((group, _)) => group,
            None => one.key(),
        };
        assert!(top != NEW && top != DELETE, "a command on a key of luneta's groups");
        let mut other = 0;
        while other < Command::ALL.len() {
            let two = Command::ALL[other];
            let together = match (one.group(), two.group()) {
                (None, None) => true,
                (Some((group, _)), Some((other, _))) => group == other,
                (Some(_), None) | (None, Some(_)) => false,
            };
            assert!(at == other || !together || one.key() != two.key(), "two commands on one key");
            if let (Some((group, _)), None) = (one.group(), two.group()) {
                assert!(group != two.key(), "a group of commands on a command's key");
            }
            other += 1;
        }
        at += 1;
    }
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
    /// A command of the sidebar's own, which `?` lists beside the menu's
    /// keys.
    Command(Command),
}

impl Entry {
    pub fn key(&self) -> char {
        match self {
            Entry::Group { key, .. } | Entry::Start { key, .. } | Entry::Close { key, .. } => *key,
            Entry::New(new) => new.key(),
            Entry::Command(command) => command.key(),
        }
    }
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Pressed {
    /// Opened a group, or went back to the one before.
    Moved,
    /// Starts an agent on the prompt.
    Start(String),
    /// Opens a pane, a tab or a session.
    New(New),
    /// Closes the pane the menu is for, or kills its session, by `key`.
    Delete { key: char, scope: Scope },
    /// Runs a command of the sidebar's own.
    Command(Command),
    /// Closes the menu.
    Close,
    /// Nothing: the group open has no such key.
    Nothing,
}

/// The menu's entries, and the groups opened so far.
pub struct Keys {
    root: Vec<Entry>,
    /// Each group opened, by where it stands in the one before.
    open: Vec<usize>,
}

impl Keys {
    /// The entries, from the KDL zellij hands over if it hands any: a node
    /// for each key, its one string the prompt the key starts an agent on, or
    /// the name of the group its children are. luneta's New and Delete groups
    /// come after them: `n p`, `n t` and `n s` open a pane, a tab and a
    /// session, `d d` deletes what the menu is for, as `row` says, and `d s`
    /// kills its session. A key of yours can take neither group's.
    pub fn new(configured: Option<&str>, row: Scope) -> Result<Self, String> {
        let mut root = match configured {
            Some(text) => {
                let document: KdlDocument =
                    text.parse().map_err(|error| format!("menu: {error}"))?;
                entries(&document, "")?
            },
            None => Vec::new(),
        };
        for (key, name) in [(NEW, NEW_NAME), (DELETE, DELETE_NAME)] {
            if root.iter().any(|entry| entry.key() == key) {
                return Err(format!(
                    "menu: \"{key}\" is luneta's, for its {name} group; give your key another"
                ));
            }
        }
        root.push(Entry::Group {
            key: NEW,
            name: NEW_NAME.to_owned(),
            entries: New::ALL.into_iter().map(Entry::New).collect(),
        });
        root.push(Entry::Group {
            key: DELETE,
            name: DELETE_NAME.to_owned(),
            entries: vec![
                Entry::Close { key: DELETE_ROW, scope: row },
                Entry::Close { key: DELETE_SESSION, scope: Scope::Session },
            ],
        });
        Ok(Keys { root, open: Vec::new() })
    }

    /// What `?` shows: the keys at the top of `menu`, and then the sidebar's
    /// commands. A key of the menu's that a command has stays the command's,
    /// as it does at the top of the sidebar.
    pub fn help(menu: Keys) -> Self {
        let mut root = menu.root;
        root.retain(|entry| !Command::takes(entry.key()));
        for command in Command::ALL.iter().copied() {
            let Some((key, name)) = command.group() else {
                root.push(Entry::Command(command));
                continue;
            };
            let group = root.iter_mut().find_map(|entry| match entry {
                Entry::Group { key: at, entries, .. } if *at == key => Some(entries),
                _ => None,
            });
            match group {
                Some(entries) => entries.push(Entry::Command(command)),
                None => root.push(Entry::Group {
                    key,
                    name: name.to_owned(),
                    entries: vec![Entry::Command(command)],
                }),
            }
        }
        Keys { root, open: Vec::new() }
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

    /// What `key` does: a key the group open shows opens its group, starts
    /// its agent, opens or closes what it opens or closes, or runs its
    /// command, `Backspace` goes
    /// back a group, and out of the menu from the first, and `Esc` closes it.
    pub fn key(&mut self, key: &KeyWithModifier) -> Pressed {
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
            Entry::Command(command) => return Pressed::Command(*command),
            Entry::Group { .. } => {},
        }
        self.open.push(at);
        Pressed::Moved
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

/// A session other than the menu's, and the tab in it, by its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct There {
    pub session: String,
    pub tab: Option<TabId>,
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
/// starts there to be named as the picker names a directory. `context` says
/// which menu asks.
fn ask_name(cwd: &str, mut context: BTreeMap<String, String>) {
    let mut worktrees = WORKTREES.to_vec();
    worktrees.push(cwd);
    context.insert(CONTEXT_KEY.to_owned(), NAMING_VALUE.to_owned());
    run_command(&worktrees, context);
}

/// Starts a session in `cwd` and goes there, named for it from what
/// `WORKTREES` printed of it, `None` when it failed, as the picker names a
/// directory: with a postfix when `taken` holds the name. A directory no
/// session can be named for, as `/`, starts one zellij names.
///
/// zellij starts no session under a name a session has: it takes you to
/// that session, and `cwd` goes unused. Nor can it be asked to start one only
/// if none has the name, so the name is as free as `taken` is new. The
/// floating menu lists the sessions as the name comes; the sidebar's list is
/// a second old at most. A session that took the name since is where you go.
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

/// Opens a pane that runs `command` in a tab of another session, in `cwd`,
/// or your shell for no command. zellij says which pane it opened, as
/// `terminal_<id>`.
fn new_pane(there: &There, cwd: &str, command: &[String]) -> Vec<String> {
    let session = there.session.as_str();
    let mut new_pane: Vec<String> =
        ["zellij", "--session", session, "action", "new-pane"].map(str::to_owned).to_vec();
    if let Some(tab) = there.tab {
        new_pane.extend(["--tab-id".to_owned(), tab.0.to_string()]);
    }
    new_pane.extend(["--cwd".to_owned(), cwd.to_owned()]);
    if !command.is_empty() {
        new_pane.push("--".to_owned());
        new_pane.extend(command.iter().cloned());
    }
    new_pane
}

/// The terminal pane `zellij action new-pane` says it opened.
fn opened(stdout: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(stdout).trim().strip_prefix("terminal_")?.parse().ok()
}

/// Opens a tab in the session `$1`, in the directory `$2`, and then lists the
/// session's tabs: zellij prints the new tab's id, and the list says where
/// that tab stands. The tab moves no client to it: zellij would move the
/// client that last typed in the session, or else its first, and leave you
/// where you come. Fails only when no tab opened.
const OPEN_TAB: &str = r#"zellij --session "$1" action new-tab --no-focus --cwd "$2" || exit
zellij --session "$1" action list-tabs
exit 0"#;

/// Opens a tab of `session`, which has a client to make it as big as its
/// terminal, in `cwd`, as `OPEN_TAB` does. A client that leaves between the
/// ask and the tab leaves it no room, as a session with none would.
fn open_tab<'a>(session: &'a str, cwd: &'a str) -> [&'a str; 6] {
    ["sh", "-c", OPEN_TAB, "sh", session, cwd]
}

/// Where the tab `OPEN_TAB` opened stands among the tabs of its session, by
/// the id zellij printed for it and the list after it, or none when the list
/// does not have it.
fn tab_position(stdout: &[u8]) -> Option<usize> {
    let printed = String::from_utf8_lossy(stdout);
    let mut lines = printed.lines();
    let id = lines.next()?.trim();
    lines.find_map(|line| match line.split_whitespace().collect::<Vec<_>>()[..] {
        [tab, position, ..] if tab == id => position.parse().ok(),
        _ => None,
    })
}

/// The client on its way to another session, one with no client, to open a
/// tab there in `cwd` once you have come. zellij makes a tab as big as a
/// client of its session: one it opens in a session with none has no room
/// for its panes, and a client that comes to it finds no tab. Opened once you
/// are there, it is as big as your terminal, and you are in it:
/// `zellij action new-tab` opens the tab for the client that last typed in
/// the session, or else for its first, and you are its one client.
struct Arriving {
    session: String,
    cwd: String,
    ticks: u32,
    /// Whether zellij has yet to answer the last ask for the clients.
    asking: bool,
}

impl Arriving {
    /// Goes to the pane `beside` of the session `there` names, handing the
    /// focus back to `back` first, for a tab to open there in the directory
    /// that pane works in once you have come.
    fn go(there: There, beside: Beside, back: Option<u32>) -> Self {
        hand_back(back);
        switch_session_with_focus(&there.session, None, Some((beside.pane, false)));
        Arriving { session: there.session, cwd: beside.cwd, ticks: 0, asking: false }
    }

    /// Asks zellij which clients the session has, with `context`, once it has
    /// answered the last ask, for `listed` to open the tab once you are among
    /// them. Says why no tab opens once the client has taken too long to come.
    fn tick(&mut self, context: BTreeMap<String, String>) -> Result<(), String> {
        self.ticks += 1;
        if self.ticks >= close::PATIENCE {
            return Err(format!("menu: \"{}\" did not take you, and no tab opened", self.session));
        }
        if !self.asking {
            run_command(&close::clients_of(&self.session), context);
            self.asking = true;
        }
        Ok(())
    }

    /// Opens the tab once zellij lists a client of the session, which had
    /// none, and says whether it did. An answer that is no list of the
    /// clients opens nothing. The tab says why when it does not open, by
    /// `TAB_VALUE`: the menu is gone by then.
    fn listed(&mut self, exit_code: Option<i32>, stdout: &[u8]) -> bool {
        self.asking = false;
        let come = exit_code == Some(0)
            && close::clients_listed(stdout).is_some_and(|clients| !clients.is_empty());
        if come {
            let session = self.session.as_str();
            let cwd = self.cwd.as_str();
            let new_tab = ["zellij", "--session", session, "action", "new-tab", "--cwd", cwd];
            let context = BTreeMap::from([
                (CONTEXT_KEY.to_owned(), TAB_VALUE.to_owned()),
                (SESSION_KEY.to_owned(), session.to_owned()),
            ]);
            run_command(&new_tab, context);
        }
        come
    }
}

/// Hands the focus back to `back`, the pane of the sidebar's own session you
/// came from, as the menu takes you out of the session.
fn hand_back(back: Option<u32>) {
    if let Some(back) = back {
        focus_terminal_pane(back, false, false);
    }
}

/// A row of the sidebar, for its menu: its pane, none on a session's row,
/// when it is in another session, the session and its tab, and the pane to
/// hand the focus back to before the menu takes you out of the sidebar's
/// session.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub pane: Option<RowPane>,
    pub there: Option<There>,
    /// The pane of the sidebar's own session you came from. zellij keeps where
    /// each session's focus was, and the sidebar has it: the session would
    /// have you back in the sidebar.
    pub back: Option<u32>,
}

/// The pane of a pane's row: the pane an agent starts beside, the directory
/// it works in and the branch checked out there, as far as the sidebar
/// knows. A session's row has neither, having no pane.
#[derive(Debug, PartialEq, Eq)]
pub struct RowPane {
    pub id: u32,
    pub cwd: Option<String>,
    pub branch: Option<String>,
}

impl Row {
    /// The keys of the menu for the row, from the `entries` of the sidebar's
    /// configuration and luneta's own groups, and what the menu is for.
    fn menu(self, entries: Option<&str>) -> Result<(Keys, Target), String> {
        let keys = Keys::new(entries, self.deletes())?;
        Ok((keys, self.target()))
    }

    /// What luneta's Delete group deletes on the row: its pane, or on a
    /// session's row, its session.
    fn deletes(&self) -> Scope {
        if self.pane.is_some() { Scope::Pane } else { Scope::Session }
    }

    fn target(self) -> Target {
        Target { pane: self.pane, there: self.there }
    }
}

/// What the sidebar's menu is for: a row's pane, or none on a session's row,
/// in the sidebar's own session or `there`, where an agent starts beside it,
/// and what the Delete group deletes.
struct Target {
    pane: Option<RowPane>,
    there: Option<There>,
}

/// What the sidebar's menu numbered `menu` runs a command with, as `value`.
fn numbered(value: &str, menu: u64) -> BTreeMap<String, String> {
    BTreeMap::from([
        (CONTEXT_KEY.to_owned(), value.to_owned()),
        (MENU_KEY.to_owned(), menu.to_string()),
    ])
}

impl Target {
    /// The pane a key starts an agent or opens a pane beside, and the
    /// directory it works in, where a tab or a session opens too, or why
    /// there is none: a session's row has no pane, and a pane the sidebar
    /// does not know the directory of yet opens nothing, rather than wherever
    /// zellij runs.
    fn beside(&self) -> Result<Beside, String> {
        let Some(pane) = &self.pane else {
            return Err(NO_ROW_PANE.to_owned());
        };
        let Some(cwd) = pane.cwd.clone() else {
            return Err(format!(
                "menu: the sidebar does not know yet where pane {} works; try again in a moment",
                pane.id
            ));
        };
        Ok(Beside { pane: pane.id, cwd })
    }

    /// Starts Claude Code on `prompt` beside the pane, in the directory it
    /// works in, for the menu numbered `menu`, at `frame`. In the sidebar's
    /// own session the pane opens at once. Another session opens it itself,
    /// and the menu, with `keys` on show, waits for zellij to say it has.
    fn start(self, keys: Keys, prompt: &str, menu: u64, frame: u64) -> Stage {
        let beside = match self.beside() {
            Ok(beside) => beside,
            Err(reason) => return wrong(reason),
        };
        let branch = self.pane.as_ref().and_then(|pane| pane.branch.as_deref());
        let command = claude(prompt, branch);
        let Some(there) = self.there else {
            start_here(&command, &beside);
            return Stage::Started(frame);
        };
        let new_pane = new_pane(&there, &beside.cwd, &command);
        let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
        run_command(&new_pane, numbered(START_VALUE, menu));
        Stage::Waiting(keys, Wait::Start(beside, there))
    }

    /// Opens what `new` opens, in the directory the pane works in, for the
    /// menu numbered `menu`, at `frame`: your shell beside the pane, a tab of
    /// its session, or a session. A pane or a tab of the sidebar's own session
    /// opens at once. Another session opens your shell itself, and a session
    /// is named once `WORKTREES` says whether the directory is a worktree: the
    /// menu, with `keys` on show, waits for the answer. A tab of another
    /// session opens before you go there when the session has a client, and
    /// once you are there when it has none: the menu asks which clients it
    /// has first.
    fn open(self, keys: Keys, new: New, menu: u64, frame: u64) -> Stage {
        let beside = match self.beside() {
            Ok(beside) => beside,
            Err(reason) => return wrong(reason),
        };
        match (new, self.there) {
            (New::Session, _) => {
                ask_name(&beside.cwd, numbered(NAMING_VALUE, menu));
                Stage::Waiting(keys, Wait::Name(beside.cwd))
            },
            (New::Pane, None) => {
                shell_here(&beside);
                Stage::Started(frame)
            },
            (New::Tab, None) => {
                tab_here(&beside.cwd);
                Stage::Started(frame)
            },
            (New::Pane, Some(there)) => {
                let new_pane = new_pane(&there, &beside.cwd, &[]);
                let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
                run_command(&new_pane, numbered(OPENING_VALUE, menu));
                Stage::Waiting(keys, Wait::Shell(beside, there))
            },
            (New::Tab, Some(there)) => {
                run_command(&close::clients_of(&there.session), numbered(ARRIVING_VALUE, menu));
                Stage::Waiting(keys, Wait::Clients(beside, there))
            },
        }
    }

    /// Asks zellij for what a delete key deletes, for the menu numbered
    /// `menu`, with what that is. A pane or a tab of another session leaves
    /// the menu waiting for its answer, and the session you are in leaves it
    /// showing where you are going. No stage once the menu is done.
    fn close(self, keys: Keys, act: Act, menu: u64) -> (Option<Stage>, Option<Act>) {
        let stage = match act.run(numbered(CLOSING_VALUE, menu)) {
            Ran::Done => None,
            Ran::Asked => {
                let cwd = self.pane.and_then(|pane| pane.cwd);
                Some(Stage::Waiting(keys, Wait::Close { cwd }))
            },
            Ran::Failed(reason) => return (Some(wrong(reason)), None),
            Ran::Leaving(leaving) => Some(Stage::Leaving(leaving)),
        };
        (stage, Some(act))
    }
}

/// What a key of the sidebar's menu leaves the sidebar to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Did {
    /// A delete key asked zellij for this, and the sidebar's cursor goes past
    /// what it closes once it is gone.
    Closed(Act),
    /// The menu closed, for the sidebar to run a command of its own.
    Command(Command),
}

/// How many menus the sidebar has opened. A menu takes the next number as it
/// opens, and only as it opens: the answer to an agent it starts in another
/// session carries it.
#[derive(Default)]
pub struct Numbers(u64);

impl Numbers {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(1);
        self.0
    }
}

/// The menu at the bottom of the sidebar, for the row highlighted when a key
/// of the menu, or `?`, opened it: a pane, or a session. The sidebar keeps it
/// while it has the focus, and while that pane is open.
pub struct SidebarMenu {
    /// Its number among the menus the sidebar has opened, which the answer to
    /// an agent it starts in another session carries.
    number: u64,
    /// The pane of the sidebar's own session you came from, to hand the focus
    /// back to before the menu takes you out of the session.
    back: Option<u32>,
    stage: Stage,
}

enum Stage {
    /// The keys of the group open, pressed one after another, for a row.
    Choosing(Keys, Target),
    /// A delete key that asks before it goes ahead.
    Asking(Keys, Target, Asked),
    /// Another session is opening the agent's pane or your shell's, saying
    /// which clients it has before a tab opens in it, or closing the pane a
    /// delete key closes, or a session waits for its name. Keys do nothing
    /// until the menu is told it has, but close the menu.
    Waiting(Keys, Wait),
    /// The agent's pane, your shell's or a tab opened in the sidebar's own
    /// session at this frame, and the focus is on its way there. A key that
    /// comes soon after was on its way already, and does nothing: a second
    /// press opens nothing a second time. Nothing shows.
    Started(u64),
    /// The client is on its way to another session, and the session it
    /// leaves ends once it is gone. Keys do nothing.
    Leaving(Leaving),
    /// The client is on its way to another session, one with no client, to
    /// open a tab there once it has come. Nothing shows, and keys do nothing.
    Arriving(Arriving),
    /// What is wrong with the keys, with the row, or with what a key asked
    /// for. Any key closes the menu.
    Wrong(String),
}

/// What the sidebar's menu says when no tab opens in another session, and
/// what it says of zellij saying nothing as it lists none of its clients.
const NO_TAB: &str = "no tab opened";
const NO_CLIENTS: &str = "zellij listed no clients";

/// What the sidebar's menu waits on, for a command it ran to say it is done:
/// another session doing something, or the name of a session.
enum Wait {
    /// Another session opens an agent's pane beside a pane of it, for the
    /// menu to go there.
    Start(Beside, There),
    /// Another session opens your shell beside a pane of it, for the menu to
    /// go there.
    Shell(Beside, There),
    /// Another session says which clients it has: with one, for the menu to
    /// open a tab of it, and with none, to take you to a pane of it and open
    /// the tab there once you have come.
    Clients(Beside, There),
    /// Another session, which has a client, opens a tab, for the menu to take
    /// you to it.
    Tab(Beside, There),
    /// Another session closes a pane or a tab of it, for a pane that works in
    /// `cwd`.
    Close { cwd: Option<String> },
    /// `WORKTREES` says whether a session that starts in this directory is
    /// named for a repository too.
    Name(String),
}

impl Wait {
    /// What the menu ran the command with, as its context.
    fn value(&self) -> &str {
        match self {
            Wait::Start(..) => START_VALUE,
            Wait::Shell(..) | Wait::Tab(..) => OPENING_VALUE,
            Wait::Clients(..) => ARRIVING_VALUE,
            Wait::Close { .. } => CLOSING_VALUE,
            Wait::Name(_) => NAMING_VALUE,
        }
    }

    /// What the menu says went wrong when the command failed, and what it
    /// says of a command that failed saying nothing. None for one whose
    /// failing is no error: `WORKTREES` failing leaves the directory its own
    /// name, as the picker leaves it.
    fn failure(&self) -> Option<(&'static str, &'static str)> {
        match self {
            Wait::Start(..) => Some(("the agent did not start", "zellij opened no pane")),
            Wait::Shell(..) => Some(("the pane did not open", "zellij opened none")),
            Wait::Clients(..) => Some((NO_TAB, NO_CLIENTS)),
            Wait::Tab(..) => Some((NO_TAB, "zellij opened none")),
            Wait::Close { .. } => Some(("nothing closed", "zellij said nothing")),
            Wait::Name(_) => None,
        }
    }

    /// The directory what the menu waits on is in.
    fn cwd(&self) -> Option<&str> {
        match self {
            Wait::Start(beside, _)
            | Wait::Shell(beside, _)
            | Wait::Clients(beside, _)
            | Wait::Tab(beside, _) => Some(&beside.cwd),
            Wait::Name(cwd) => Some(cwd),
            Wait::Close { cwd } => cwd.as_deref(),
        }
    }
}

impl SidebarMenu {
    /// The menu for `row`, numbered next among `numbers`, from the `entries`
    /// of the sidebar's configuration and luneta's own groups, at `frame`: on
    /// the group `key` opens, or on the first with no key. A key that starts
    /// an agent starts it, and the menu stays only while another session does
    /// it, or while the keys on their way to an agent it started come. None
    /// for a key the menu has not. On a session's row, `d d` kills the
    /// session.
    pub fn open(
        entries: Option<&str>,
        row: Row,
        key: Option<char>,
        numbers: &mut Numbers,
        frame: u64,
    ) -> Option<Self> {
        let number = numbers.next();
        let back = row.back;
        let (keys, target) = match row.menu(entries) {
            Ok(menu) => menu,
            Err(reason) => return Some(SidebarMenu { number, back, stage: wrong(reason) }),
        };
        let has = |key: char| keys.entries().iter().any(|entry| entry.key() == key);
        if key.is_some_and(|key| !has(key)) {
            return None;
        }
        let choosing = SidebarMenu { number, back, stage: Stage::Choosing(keys, target) };
        match key {
            Some(key) => choosing.key(&KeyWithModifier::new(BareKey::Char(key)), None, frame).0,
            None => Some(choosing),
        }
    }

    /// What `?` opens, numbered next among `numbers`: the keys at the top of
    /// the menu for `row`, as `m` opens it, and then the sidebar's commands,
    /// on the group of commands `group` opens, or on the first. A group of
    /// commands has none of the menu's keys, and opens with yours gone wrong
    /// too, over luneta's keys alone: `?` and `m` still say what is wrong
    /// with yours.
    pub fn help(
        entries: Option<&str>,
        row: Row,
        group: Option<char>,
        numbers: &mut Numbers,
    ) -> Self {
        let number = numbers.next();
        let back = row.back;
        let group = group.filter(|group| Command::opens(*group));
        let deletes = row.deletes();
        let keys = Keys::new(entries, deletes).or_else(|reason| match group {
            Some(_) => Keys::new(None, deletes),
            None => Err(reason),
        });
        let stage = match keys {
            Ok(keys) => {
                let mut keys = Keys::help(keys);
                if let Some(group) = group {
                    keys.press(group);
                }
                Stage::Choosing(keys, row.target())
            },
            Err(reason) => wrong(reason),
        };
        SidebarMenu { number, back, stage }
    }

    /// The menu numbered next among `numbers`, saying what is wrong, until
    /// any key closes it.
    pub fn wrong(reason: String, numbers: &mut Numbers) -> Self {
        SidebarMenu { number: numbers.next(), back: None, stage: wrong(reason) }
    }

    /// What the menu shows: the keys, what a delete key asks, where you are
    /// going, or what is wrong with the keys, with the row, or with what a key
    /// asked for. Nothing once an agent started in the sidebar's own session,
    /// or while you are on your way to open a tab in another.
    pub fn shown(&self) -> Option<Shown<'_>> {
        match &self.stage {
            Stage::Choosing(keys, _) | Stage::Waiting(keys, _) => Some(Shown::Keys(keys)),
            Stage::Asking(keys, _, asked) => Some(Shown::Asking(keys, &asked.question)),
            Stage::Leaving(leaving) => Some(Shown::Leaving(leaving.to())),
            Stage::Wrong(reason) => Some(Shown::Wrong(reason)),
            Stage::Started(_) | Stage::Arriving(_) => None,
        }
    }

    /// The directory an agent the menu starts works in, or what it opens, as
    /// far as the sidebar knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.stage {
            Stage::Choosing(_, target) | Stage::Asking(_, target, _) => {
                target.pane.as_ref().and_then(|pane| pane.cwd.as_deref())
            },
            Stage::Waiting(_, wait) => wait.cwd(),
            Stage::Arriving(arriving) => Some(&arriving.cwd),
            Stage::Started(_) | Stage::Leaving(_) | Stage::Wrong(_) => None,
        }
    }

    /// The frame the menu started an agent at in the sidebar's own session,
    /// or opened a pane or a tab at, for the sidebar to say when the keys on
    /// their way then have all come.
    pub fn started_at(&self) -> Option<u64> {
        match self.stage {
            Stage::Started(at) => Some(at),
            Stage::Choosing(..)
            | Stage::Asking(..)
            | Stage::Waiting(..)
            | Stage::Leaving(_)
            | Stage::Arriving(_)
            | Stage::Wrong(_) => None,
        }
    }

    /// The pane the menu is for, none on a session's row, and its session
    /// when that is another, while a key of the menu can delete it, or a
    /// delete key asks before it does.
    pub fn pane(&self) -> Option<(Option<u32>, Option<&str>)> {
        let (Stage::Choosing(_, target) | Stage::Asking(_, target, _)) = &self.stage else {
            return None;
        };
        let pane = target.pane.as_ref().map(|pane| pane.id);
        Some((pane, target.there.as_ref().map(|there| there.session.as_str())))
    }

    /// Whether the menu outlasts the focus: the client takes the focus along
    /// on its way out of the session, which ends once it is gone, or on its
    /// way to another, where a tab opens once it is there.
    pub fn lasts(&self) -> bool {
        matches!(self.stage, Stage::Leaving(_) | Stage::Arriving(_))
    }

    /// What `key` does to the menu at `frame`: the menu left, none once it
    /// closed or deleted what a key asked in the sidebar's own session, and
    /// what that leaves the sidebar to do: go past what a delete key asked
    /// zellij to close, or run a command. `subject` is what the menu is for,
    /// as a delete key deletes it, as zellij says it stands now: going ahead
    /// after a delete key asked plans it again.
    pub fn key(
        self,
        key: &KeyWithModifier,
        subject: Option<&close::Subject>,
        frame: u64,
    ) -> (Option<Self>, Option<Did>) {
        let SidebarMenu { number, back, .. } = self;
        let (stage, act) = match self.stage {
            Stage::Wrong(_) => return (None, None),
            Stage::Waiting(..) if closes(key) => return (None, None),
            Stage::Waiting(..) | Stage::Started(_) | Stage::Leaving(_) | Stage::Arriving(_) => {
                return (Some(self), None);
            },
            Stage::Choosing(mut keys, target) => match keys.key(key) {
                Pressed::Close => return (None, None),
                Pressed::Command(command) => return (None, Some(Did::Command(command))),
                Pressed::Start(prompt) => (Some(target.start(keys, &prompt, number, frame)), None),
                Pressed::New(new) => (Some(target.open(keys, new, number, frame)), None),
                Pressed::Delete { key, scope } => match subject.map(|it| it.plan(scope)) {
                    None => (Some(Stage::Choosing(keys, target)), None),
                    Some(Err(reason)) => (Some(wrong(reason)), None),
                    Some(Ok(plan)) => match Asked::then(key, scope, plan) {
                        Then::Ask(asked) => (Some(Stage::Asking(keys, target, asked)), None),
                        Then::Go(act) => target.close(keys, act, number),
                    },
                },
                Pressed::Moved | Pressed::Nothing => (Some(Stage::Choosing(keys, target)), None),
            },
            Stage::Asking(keys, target, asked) => match answer(key, asked.key) {
                Answer::Yes => match subject.map(|it| it.plan(asked.scope)) {
                    None => (Some(Stage::Asking(keys, target, asked)), None),
                    Some(Err(reason)) => (Some(wrong(reason)), None),
                    Some(Ok(now)) => match asked.yes(now) {
                        Then::Ask(again) => (Some(Stage::Asking(keys, target, again)), None),
                        Then::Go(act) => target.close(keys, act, number),
                    },
                },
                Answer::Back => (Some(Stage::Choosing(keys, target)), None),
                Answer::Close => return (None, None),
                Answer::Nothing => (Some(Stage::Asking(keys, target, asked)), None),
            },
        };
        (stage.map(|stage| SidebarMenu { number, back, stage }), act.map(Did::Closed))
    }

    /// Closes the menu once the pane an agent would start beside, in the
    /// sidebar's own session, has closed: zellij would open the agent beside
    /// the pane with the focus, which is the sidebar. None once it has.
    pub fn panes(self, manifest: &PaneManifest) -> Option<Self> {
        let Stage::Choosing(_, Target { pane: Some(pane), there: None }) = &self.stage else {
            return Some(self);
        };
        let pane = pane.id;
        let open = manifest.panes.values().flatten().any(|p| !p.is_plugin && p.id == pane);
        open.then_some(self)
    }

    /// Whether `context` is that of a command this menu ran, as `value`. An
    /// answer for another menu is from one closed before it came, or one
    /// that gave up waiting on it.
    fn asked(&self, context: &BTreeMap<String, String>, value: &str) -> bool {
        let said = |key: &str| context.get(key).map(String::as_str);
        let number = self.number.to_string();
        said(CONTEXT_KEY) == Some(value) && said(MENU_KEY) == Some(&number)
    }

    /// What a command the menu waits on said: the agent's pane or your
    /// shell's open in another session, which clients a session a tab opens
    /// in has, the tab open, what a delete key closes closed, or whether the
    /// directory a session starts in is a worktree. The menu goes to that pane,
    /// and to the session when zellij says not which it is, or opens the tab
    /// of a session with a client and goes to it, and to the row's pane when
    /// zellij says not where it is, or takes you to the row's pane of a
    /// session with none, for the tab to open once you have come, starts the
    /// session, named as the picker names a directory with a postfix when
    /// `taken` holds the name, or closes, or says what zellij said went wrong.
    /// None once it went or closed. An answer for anything the menu does not
    /// wait for, or for another menu, is from one closed before it came.
    pub fn answered(
        self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
        taken: &Taken,
    ) -> Option<Self> {
        let Stage::Waiting(_, wait) = &self.stage else {
            return Some(self);
        };
        if !self.asked(context, wait.value()) {
            return Some(self);
        }
        let done = exit_code == Some(0);
        if let (false, Some((what, nothing))) = (done, wait.failure()) {
            return Some(self.failed(stderr, what, nothing));
        }
        match wait {
            Wait::Clients(beside, there) => {
                let Some(clients) = close::clients_listed(stdout) else {
                    return Some(self.failed(stderr, NO_TAB, NO_CLIENTS));
                };
                let (beside, there) = (beside.clone(), there.clone());
                if clients.is_empty() {
                    let arriving = Arriving::go(there, beside, self.back);
                    return Some(SidebarMenu { stage: Stage::Arriving(arriving), ..self });
                }
                let opening = numbered(OPENING_VALUE, self.number);
                run_command(&open_tab(&there.session, &beside.cwd), opening);
                let mut menu = self;
                if let Stage::Waiting(_, wait) = &mut menu.stage {
                    *wait = Wait::Tab(beside, there);
                }
                return Some(menu);
            },
            Wait::Tab(beside, there) => {
                hand_back(self.back);
                match tab_position(stdout) {
                    Some(position) => {
                        switch_session_with_focus(&there.session, Some(position), None);
                    },
                    // The tab is open, and zellij did not say where it
                    // stands: the row's pane.
                    None => {
                        switch_session_with_focus(&there.session, None, Some((beside.pane, false)));
                    },
                }
            },
            Wait::Name(cwd) => {
                hand_back(self.back);
                start_session(cwd, done.then_some(stdout), taken);
            },
            Wait::Start(_, there) | Wait::Shell(_, there) => {
                hand_back(self.back);
                match opened(stdout) {
                    Some(pane) => {
                        switch_session_with_focus(&there.session, None, Some((pane, false)));
                    },
                    // The pane is open, and zellij did not say which it is:
                    // the session, as it stands.
                    None => switch_session(Some(&there.session)),
                }
            },
            Wait::Close { .. } => {},
        }
        None
    }

    /// The menu saying `what` went wrong, as the first line zellij said, or
    /// `nothing` when it said none.
    fn failed(self, stderr: &[u8], what: &str, nothing: &str) -> Self {
        let reason = String::from_utf8_lossy(stderr);
        let reason = reason.lines().next().unwrap_or("").trim();
        let reason = if reason.is_empty() { nothing } else { reason };
        SidebarMenu { stage: wrong(format!("menu: {what}: {reason}")), ..self }
    }

    /// While the client leaves the session, or comes to another: asks zellij
    /// whether it is gone, or there, and says why the session stays, or why
    /// no tab opens, once it has taken too long. The menu asks as itself: the
    /// last ask of one that gave up may answer once another menu asks, and
    /// lists another session's clients.
    pub fn tick(self) -> Self {
        let asking = numbered(close::CLIENTS_VALUE, self.number);
        let stage = match self.stage {
            Stage::Leaving(mut leaving) => match leaving.tick(asking) {
                Ok(()) => Stage::Leaving(leaving),
                Err(reason) => wrong(reason),
            },
            Stage::Arriving(mut arriving) => match arriving.tick(asking) {
                Ok(()) => Stage::Arriving(arriving),
                Err(reason) => wrong(reason),
            },
            stage => stage,
        };
        SidebarMenu { stage, ..self }
    }

    /// What zellij said, as it listed the clients of a session for the menu
    /// `context` names: while `client`, the sidebar's, leaves it, kills the
    /// session once the client is gone, and while it comes to another, opens
    /// the tab there once you have come. None once it has. An answer for
    /// another menu does nothing.
    pub fn clients(
        mut self,
        client: ClientId,
        exit_code: Option<i32>,
        stdout: &[u8],
        context: &BTreeMap<String, String>,
    ) -> Option<Self> {
        if !self.asked(context, close::CLIENTS_VALUE) {
            return Some(self);
        }
        let done = match &mut self.stage {
            Stage::Leaving(leaving) => leaving.listed(client, exit_code, stdout),
            Stage::Arriving(arriving) => arriving.listed(exit_code, stdout),
            Stage::Choosing(..)
            | Stage::Asking(..)
            | Stage::Waiting(..)
            | Stage::Started(_)
            | Stage::Wrong(_) => false,
        };
        (!done).then_some(self)
    }
}

/// The sidebar's menu saying what is wrong, as the log does.
fn wrong(reason: String) -> Stage {
    eprintln!("luneta {reason}");
    Stage::Wrong(reason)
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
    /// The command the configuration names claude-ps by, if it names one.
    agents_command: Option<String>,
    /// What the agents are doing, for a delete key to ask first before it ends
    /// one at work.
    agents: AgentSet,
    /// Where the panes of the session work, by which an agent is placed in
    /// one, and whether the session has said.
    places: Places,
    places_in: bool,
    /// The session the menu is in, and the one you would go to before it is
    /// killed, once the menu has asked zellij.
    session: Option<String>,
    refuge: Option<String>,
}

impl Menu {
    pub fn new(plugin_id: u32, client: ClientId, configuration: &BTreeMap<String, String>) -> Self {
        let phase = match Keys::new(configuration.get(ENTRIES).map(String::as_str), Scope::Pane) {
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
            agents_command: agents::configured(configuration),
            agents: AgentSet::default(),
            places: Places::default(),
            places_in: false,
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
                Pressed::Delete { key, scope } => self.delete(key, scope),
                Pressed::Close => self.close(),
                // The sidebar's commands are the sidebar's: this menu's keys
                // are the configuration's alone.
                Pressed::Command(_) | Pressed::Nothing => false,
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
    /// agent starts, what the agents are doing, where the panes of the
    /// session work, the clients of the session while you leave it, and
    /// whether the directory a session starts in is a worktree. Says whether
    /// to draw.
    pub fn ran(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        match context.get(CONTEXT_KEY).map(String::as_str) {
            Some(BRANCH_VALUE) => self.branched(exit_code, stdout, context),
            Some(agents::CONTEXT_VALUE) => {
                self.agents.ingest(exit_code, stdout, stderr);
                self.seat_agents();
            },
            Some(places::CONTEXT_VALUE) => {
                let Some(session) = context.get(SESSION_KEY) else {
                    return false;
                };
                self.places.ingest(session.clone(), exit_code, stdout);
                self.places_in = true;
                self.seat_agents();
            },
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

    /// Puts each agent in the pane of the session it sits in.
    fn seat_agents(&mut self) {
        let live = Live::new(self.session.as_deref(), &[], &self.places);
        self.agents.rebuild("", &live, None, Age::from_secs(0), Selection::Hold);
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
                ask_name(&beside.cwd, BTreeMap::new());
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
    /// session: what the session holds, and its agents once claude-ps and the
    /// session have said where they are. None until zellij has said which
    /// session the menu is in.
    fn subject(&self, id: u32) -> Option<close::Subject> {
        let session = self.session.as_deref()?;
        let manifest = self.panes.as_ref()?;
        let leaves =
            tree::leaves(session, &self.tabs, manifest, &self.places, &Checkouts::default(), &[]);
        let agents = match (&self.agents.status, self.places_in) {
            (Fetch::Ready(_), false) => Fetch::Waiting,
            _ => close::agents_of(&self.agents, session, &leaves),
        };
        let seat = Seat::Here(session.to_owned());
        Some(close::Subject::new(seat, Some(id), &leaves, agents, self.refuge.clone()))
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
            Ran::Failed(reason) => self.phase = Phase::Wrong(reason),
            Ran::Done | Ran::Asked => return self.close(),
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

    /// Asks once which session the menu is in, and then claude-ps what the
    /// agents are doing and the session where its panes work: a delete key
    /// asks first before it ends an agent at work. A menu over no terminal
    /// has nothing to delete, and asks none of it.
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
        agents::ask(self.agents_command.as_deref());
        run_command(
            &places::query(&session),
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), places::CONTEXT_VALUE.to_owned()),
                (SESSION_KEY.to_owned(), session.clone()),
            ]),
        );
        self.session = Some(session);
    }

    /// Keeps the terminal you came from as zellij says, and whether that is a
    /// guess. A pane that was a guess stays one: once a floating terminal you
    /// may have come from closes, the pane under it is no surer.
    fn follow_origin(&mut self) -> bool {
        let pane = match self.origin_pane() {
            Fetch::Waiting => return false,
            Fetch::Ready(pane) => Some(pane),
            Fetch::Failed(_) => None,
        };
        let floats = self.floats_a_terminal();
        let Phase::Choosing(_, origin) = &mut self.phase else {
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

    use super::*;
    use crate::agents::Status;

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
        Keys::new(Some(text), Scope::Pane).expect("a menu")
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
        assert_eq!(shown(&keys), ['c', 'r', 'n', 'd']);
        assert!(keys.names().is_empty());

        assert_eq!(keys.key(&char('x')), Pressed::Nothing);
        assert_eq!(keys.key(&char('c')), Pressed::Moved);
        assert_eq!(shown(&keys), ['t', 'm', 's']);
        assert_eq!(keys.names(), ["Claude"]);
        // A key of the group before is not one of this group's.
        assert_eq!(keys.key(&char('r')), Pressed::Nothing);
        assert_eq!(keys.key(&char('s')), Pressed::Start("/tigerstyle".to_owned()));

        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert_eq!(shown(&keys), ['c', 'r', 'n', 'd']);
        assert_eq!(keys.key(&char('r')), Pressed::Start("review the diff against main".to_owned()));
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Close);
        keys.key(&char('c'));
        assert_eq!(keys.key(&bare(BareKey::Esc)), Pressed::Close);
        assert_eq!(keys.key(&char('c').with_ctrl_modifier()), Pressed::Close);
    }

    /// Every menu has luneta's New and Delete groups after your keys, and
    /// one with none of yours too: `n p`, `n t` and `n s` open a pane, a tab
    /// and a session, `d d` deletes what the menu is for, a pane or a
    /// session's row, and `d s` its session.
    #[test]
    fn every_menu_has_luneta_s_new_and_delete_keys() {
        for (configured, root) in [
            (Some(MENU), &['c', 'r', 'n', 'd'][..]),
            (None, &['n', 'd']),
            (Some(""), &['n', 'd']),
            (Some("// nothing but a comment"), &['n', 'd']),
        ] {
            let mut keys = Keys::new(configured, Scope::Pane).expect("a menu");
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
        let mut row = Keys::new(Some(MENU), Scope::Session).expect("a menu");
        row.key(&char('d'));
        assert_eq!(row.key(&char('d')), Pressed::Delete { key: 'd', scope: Scope::Session });
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
        ] {
            let parsed = Keys::new(Some(text), Scope::Pane);
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

    /// Pane 2, working in `/w/elo` on `main`.
    fn elo() -> RowPane {
        RowPane { id: 2, cwd: Some("/w/elo".to_owned()), branch: Some("main".to_owned()) }
    }

    /// Pane 2 of the sidebar, working in `/w/elo`, of the sidebar's own
    /// session or of `there`.
    fn row(there: Option<There>) -> Row {
        Row { pane: Some(elo()), there, back: Some(1) }
    }

    /// The row of the sidebar's own session, or of `there`.
    fn session_row(there: Option<There>) -> Row {
        Row { pane: None, there, back: Some(1) }
    }

    /// Pane 2 as a delete key closes it: in tab 7 of `seat`'s session, beside
    /// another pane, with the agents `seated`, each its pane and its status,
    /// and `elo` to go to before the session you are in ends.
    fn closing(seat: Seat, seated: &[(u32, &str)]) -> close::Subject {
        let agents = seated
            .iter()
            .map(|(pane, status)| close::Agent {
                pane: *pane,
                tab: Some(TabId(7)),
                status: Status::parse(Some(status)),
            })
            .collect();
        close::Subject {
            seat,
            pane: Some(close::Pane { id: 2, tab: Some(TabId(7)), closes: close::Closes::Pane }),
            agents: Fetch::Ready(agents),
            refuge: Some("elo".to_owned()),
        }
    }

    fn luneta() -> Seat {
        Seat::Here("luneta".to_owned())
    }

    /// The sidebar's menu numbered 1 for `row`, opened by `key` at frame 0.
    fn open(entries: Option<&str>, row: Row, key: Option<char>) -> Option<SidebarMenu> {
        SidebarMenu::open(entries, row, key, &mut Numbers::default(), 0)
    }

    /// `?` numbered 1 for `row`, on the group of commands `group` opens.
    fn help(entries: Option<&str>, row: Row, group: Option<char>) -> SidebarMenu {
        SidebarMenu::help(entries, row, group, &mut Numbers::default())
    }

    /// `key` pressed in the sidebar's menu at frame 0, its pane with no agent.
    fn press(menu: Option<SidebarMenu>, key: &KeyWithModifier) -> Option<SidebarMenu> {
        menu.and_then(|menu| menu.key(key, Some(&closing(luneta(), &[])), 0).0)
    }

    /// What a command the sidebar's menu ran said, with no session's name
    /// taken.
    fn told(
        menu: SidebarMenu,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> Option<SidebarMenu> {
        menu.answered(exit_code, stdout, stderr, context, &Sessions::default().taken(None))
    }

    /// What the sidebar's menu shows: the keys of the group open, or what is
    /// wrong.
    fn on_show(menu: Option<&SidebarMenu>) -> Option<Result<Vec<char>, &str>> {
        menu.and_then(SidebarMenu::shown).map(|on| match on {
            Shown::Keys(keys) | Shown::Asking(keys, _) => Ok(shown(keys)),
            Shown::Leaving(to) | Shown::Wrong(to) => Err(to),
        })
    }

    fn wrong(menu: Option<&SidebarMenu>) -> Option<&str> {
        match menu?.shown()? {
            Shown::Wrong(reason) => Some(reason),
            Shown::Keys(_) | Shown::Asking(..) | Shown::Leaving(_) => None,
        }
    }

    fn asking(menu: Option<&SidebarMenu>) -> Option<&str> {
        match menu?.shown()? {
            Shown::Asking(_, question) => Some(question),
            Shown::Keys(_) | Shown::Leaving(_) | Shown::Wrong(_) => None,
        }
    }

    /// A key of the menu opens it in the sidebar on its group, and `m` on the
    /// first one; a key the menu has not is the sidebar's.
    #[test]
    fn a_key_of_the_menu_opens_it_on_its_group() {
        for (key, on) in [
            (Some('c'), Some(vec!['t', 'm', 's'])),
            (None, Some(vec!['c', 'r', 'n', 'd'])),
            (Some('n'), Some(vec!['p', 't', 's'])),
            (Some('d'), Some(vec!['d', 's'])),
            (Some('x'), None),
        ] {
            let menu = open(Some(MENU), row(None), key);
            assert_eq!(on_show(menu.as_ref()), on.map(Ok), "{key:?}");
        }
        let menu = open(Some(MENU), row(None), Some('c'));
        assert_eq!(menu.as_ref().and_then(SidebarMenu::cwd), Some("/w/elo"));
        let menu = press(menu, &bare(BareKey::Backspace));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'n', 'd'])));
        let menu = press(menu, &char('x'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'n', 'd'])));
        assert!(press(menu, &bare(BareKey::Esc)).is_none());
    }

    /// A menu whose `b` the brief's group has, as it has at the top of the
    /// sidebar. Its `s` is a key of the brief's group, and not of the top:
    /// the menu keeps it.
    const SHADOWED: &str = r#"b "build"; s "status"; c "Claude" { t "/tigerstyle"; }"#;

    /// `?` shows the keys at the top of the menu, as `m` does, luneta's Delete
    /// group among them, and then the sidebar's commands, which keep their
    /// keys from the menu's. So it does on a session's row, and with no menu
    /// of yours. A menu that cannot be read says what is wrong with it, but
    /// not over the brief's keys, which are none of its own: they open with
    /// luneta's keys alone at the top.
    #[test]
    fn help_shows_the_top_of_the_menu_and_then_the_sidebar_s_commands() {
        for (entries, on_row, on) in [
            (Some(MENU), row(None), vec!['c', 'r', 'n', 'd', '/', 'b']),
            (Some(SHADOWED), row(None), vec!['s', 'c', 'n', 'd', '/', 'b']),
            (Some(MENU), session_row(None), vec!['c', 'r', 'n', 'd', '/', 'b']),
            (None, row(None), vec!['n', 'd', '/', 'b']),
        ] {
            let menu = help(entries, on_row, None);
            assert_eq!(on_show(Some(&menu)), Some(Ok(on)), "{entries:?}");
        }
        for (entries, said) in [
            ("t", "menu: \"t\" takes one string"),
            (r#"d "notes""#, "menu: \"d\" is luneta's"),
        ] {
            let menu = help(Some(entries), row(None), None);
            let wrong = wrong(Some(&menu)).expect("what is wrong");
            assert!(wrong.starts_with(said), "{wrong}");

            let brief = help(Some(entries), row(None), Some('b'));
            assert_eq!(on_show(Some(&brief)), Some(Ok(vec!['z', 's', 'h'])), "{entries}");
            let top = press(Some(brief), &bare(BareKey::Backspace));
            assert_eq!(on_show(top.as_ref()), Some(Ok(vec!['n', 'd', '/', 'b'])), "{entries}");
        }
    }

    /// A group of the menu opens from `?`, and `Backspace` comes back to it.
    /// A delete key deletes as it does from `m`. A command closes the menu,
    /// for the sidebar to run it, on a session's row too, and so does a
    /// command in a group of commands, which `?` opens on as the group's key
    /// does at the top of the sidebar.
    #[test]
    fn a_key_of_help_opens_its_group_or_hands_the_sidebar_its_command() {
        let top = || Some(help(Some(MENU), row(None), None));
        let menu = press(top(), &char('c'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['t', 'm', 's'])));
        let menu = press(menu, &bare(BareKey::Backspace));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'n', 'd', '/', 'b'])));
        assert!(press(menu, &bare(BareKey::Esc)).is_none());

        let menu = press(top(), &char('d'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['d', 's'])));
        let idle = closing(luneta(), &[]);
        let (menu, did) = menu.expect("a menu").key(&char('d'), Some(&idle), 0);
        assert!(menu.is_none());
        assert_eq!(did, Some(Did::Closed(Act::Pane(luneta(), 2))));

        for on_session in [false, true] {
            let on_row = || if on_session { session_row(None) } else { row(None) };
            let brief = || help(Some(MENU), on_row(), Some('b'));
            assert_eq!(on_show(Some(&brief())), Some(Ok(vec!['z', 's', 'h'])), "{on_session}");
            let (menu, did) = brief().key(&char('s'), None, 0);
            assert!(menu.is_none(), "{on_session}");
            assert_eq!(did, Some(Did::Command(Command::Brief(brief::Scope::Session))));
            let back = press(Some(brief()), &bare(BareKey::Backspace));
            let top = vec!['c', 'r', 'n', 'd', '/', 'b'];
            assert_eq!(on_show(back.as_ref()), Some(Ok(top)), "{on_session}");
        }

        for (entries, on_row, key, command) in [
            (MENU, row(None), '/', Command::Search),
            (MENU, session_row(None), '/', Command::Search),
        ] {
            let menu = help(Some(entries), on_row, None);
            let (menu, did) = menu.key(&char(key), None, 0);
            assert!(menu.is_none(), "{key}");
            assert_eq!(did, Some(Did::Command(command)), "{key}");
        }
    }

    /// An agent `?` starts in another session waits for the answer to its own
    /// menu, as one `m` starts does: an answer for another menu is from one
    /// closed before it came.
    #[test]
    fn an_agent_started_from_help_waits_for_its_own_answer() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), };
        let answer = |menu: u64| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), START_VALUE.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ])
        };
        let help = SidebarMenu::help(Some(MENU), row(Some(there)), None, &mut Numbers(1));
        let menu = press(Some(help), &char('r')).expect("a menu");
        let menu = told(menu, None, b"", b"", &answer(1)).expect("a menu");
        assert_eq!(on_show(Some(&menu)), Some(Ok(vec!['c', 'r', 'n', 'd', '/', 'b'])));
        let menu = told(menu, None, b"", b"", &answer(2));
        let said = wrong(menu.as_ref());
        assert_eq!(said, Some("menu: the agent did not start: zellij opened no pane"));
    }

    /// A row the sidebar does not know the directory of yet has no agent to
    /// start, nor a pane, a tab or a session to open, and the menu says so
    /// rather than open one wherever zellij runs. Nor has a session's row,
    /// which has no pane. A menu that cannot be read says what is wrong with
    /// it for any key it might have had. Any key closes what is wrong.
    #[test]
    fn a_menu_that_cannot_start_an_agent_beside_its_row_says_why() {
        let unknown = || Row { pane: Some(RowPane { cwd: None, ..elo() }), ..row(None) };
        let where_unknown = "menu: the sidebar does not know yet where pane 2 works";
        for (entries, row, key, said) in [
            (Some(MENU), unknown(), Some('r'), Some(where_unknown)),
            (Some(MENU), unknown(), Some('x'), None),
            (Some(MENU), session_row(None), Some('r'), Some(NO_ROW_PANE)),
            (None, row(None), Some('c'), None),
            (Some("t"), row(None), Some('x'), Some("menu: \"t\" takes one string")),
        ] {
            let menu = open(entries, row, key);
            let wrong = wrong(menu.as_ref());
            assert_eq!(wrong.is_some(), said.is_some(), "{entries:?} {key:?}");
            if let (Some(wrong), Some(said)) = (wrong, said) {
                assert!(wrong.starts_with(said), "{wrong}");
            }
            assert!(press(menu, &char('c')).is_none());
        }
        // Deleting it needs no directory, nor a menu of yours.
        for (entries, row) in [(Some(MENU), unknown()), (None, row(None)), (None, session_row(None))]
        {
            let menu = open(entries, row, Some('d'));
            assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['d', 's'])), "{entries:?}");
        }
        for new in New::ALL {
            for (on_row, said) in [
                (unknown as fn() -> Row, where_unknown),
                (|| session_row(None), NO_ROW_PANE),
                (|| session_row(Some(There { session: "elo".to_owned(), tab: None })), NO_ROW_PANE),
            ] {
                let menu = press(open(None, on_row(), Some('n')), &char(new.key()));
                let wrong = wrong(menu.as_ref()).unwrap_or_else(|| panic!("{new:?}: not wrong"));
                assert!(wrong.starts_with(said), "{new:?}: {wrong}");
            }
        }
    }

    /// In another session, the menu waits for zellij to open your shell
    /// beside the row's pane, as it waits for an agent's pane, and goes there
    /// once it has, though zellij say not which pane it is. When zellij fails,
    /// the menu says what it said. An answer to another kind of command, or
    /// for another menu, is for a menu closed before it came.
    #[test]
    fn your_shell_in_another_session_is_gone_to_once_its_pane_is_open() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)) };
        let none: &[u8] = b"";
        let opened = &b"terminal_12\n"[..];
        let waiting = || {
            let menu = press(open(None, row(Some(there.clone())), Some('n')), &char('p'));
            let menu = menu.expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            assert_eq!(menu.cwd(), Some("/w/elo"));
            menu
        };
        let menu = told(waiting(), Some(0), opened, none, &numbered(START_VALUE, 1));
        let menu = told(menu.expect("a menu"), Some(0), opened, none, &numbered(OPENING_VALUE, 2));
        assert!(matches!(menu.expect("a menu").stage, Stage::Waiting(..)));
        for printed in [opened, none] {
            let gone = told(waiting(), Some(0), printed, none, &numbered(OPENING_VALUE, 1));
            assert!(gone.is_none(), "{printed:?}");
        }
        let refused = b"There is no active session!\n";
        for (stderr, said) in
            [(&refused[..], "There is no active session!"), (none, "zellij opened none")]
        {
            let menu = told(waiting(), Some(1), none, stderr, &numbered(OPENING_VALUE, 1));
            let said = format!("menu: the pane did not open: {said}");
            assert_eq!(wrong(menu.as_ref()), Some(said.as_str()));
        }
        assert!(press(Some(waiting()), &bare(BareKey::Esc)).is_none());
    }

    /// zellij makes a tab of a session with no client with no room for its
    /// panes, and a tab `zellij action` opens in a session with one is for
    /// that client, not for you. The menu asks which clients the session has.
    /// With one, it opens a tab that moves no client, waits for zellij to say
    /// it has, and goes there. With none, it takes you to the row's pane,
    /// outlasts the focus on the way, and asks zellij for the session's
    /// clients until it lists one, which is you. An answer for another menu
    /// does nothing, nor does a key meanwhile, nor an answer that is no list.
    /// When zellij cannot list the clients before you go, opens no tab, or
    /// you take too long to come, the menu says no tab opened.
    #[test]
    fn a_tab_of_another_session_opens_for_you() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)) };
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        let one = format!("{header}1 terminal_7 sh\n");
        let none: &[u8] = b"";
        let refused = b"There is no active session!\n";
        let counting = || {
            let menu = press(open(None, row(Some(there.clone())), Some('n')), &char('t'));
            let menu = menu.expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            assert!(!menu.lasts());
            assert_eq!(menu.cwd(), Some("/w/elo"));
            menu
        };
        let counted = |clients: &str| {
            let counted = numbered(ARRIVING_VALUE, 1);
            told(counting(), Some(0), clients.as_bytes(), none, &counted).expect("a menu")
        };

        // The session has a client of its own: the tab opens before you go.
        let opening = || {
            let menu = counted(&one);
            assert!(matches!(menu.stage, Stage::Waiting(_, Wait::Tab(..))));
            assert!(!menu.lasts());
            assert_eq!(menu.cwd(), Some("/w/elo"));
            menu
        };
        let opened = &b"4\nTAB_ID  POSITION  NAME\n0  0  Tab #1\n4  1  Tab #2\n"[..];
        let menu = told(opening(), Some(0), opened, none, &numbered(OPENING_VALUE, 2));
        assert!(matches!(menu.expect("a menu").stage, Stage::Waiting(_, Wait::Tab(..))));
        for printed in [opened, b"4\n"] {
            let gone = told(opening(), Some(0), printed, none, &numbered(OPENING_VALUE, 1));
            assert!(gone.is_none(), "{printed:?}");
        }
        let menu = told(opening(), Some(1), none, refused, &numbered(OPENING_VALUE, 1));
        let said = format!("menu: {NO_TAB}: There is no active session!");
        assert_eq!(wrong(menu.as_ref()), Some(said.as_str()));
        assert!(press(Some(opening()), &bare(BareKey::Esc)).is_none());

        // The session has none: the tab opens once you have come.
        let arriving = || {
            let menu = counted(header);
            assert!(menu.lasts());
            assert!(menu.shown().is_none());
            assert_eq!(menu.cwd(), Some("/w/elo"));
            menu
        };
        let clients = numbered(close::CLIENTS_VALUE, 1);
        let menu = press(Some(arriving()), &bare(BareKey::Esc)).expect("a menu").tick();
        let menu = menu.clients(1, Some(0), header.as_bytes(), &clients).expect("a menu");
        let menu = menu.clients(1, Some(1), refused, &clients).expect("a menu");
        let elsewhere = numbered(close::CLIENTS_VALUE, 2);
        let menu = menu.clients(1, Some(0), one.as_bytes(), &elsewhere).expect("a menu");
        assert!(menu.lasts());
        assert!(menu.clients(1, Some(0), one.as_bytes(), &clients).is_none());

        assert!(press(Some(counting()), &bare(BareKey::Esc)).is_none());
        for (exit_code, stdout, stderr, said) in [
            (Some(1), none, &refused[..], "There is no active session!"),
            (Some(0), &b"Session 'elo' not found\n"[..], none, NO_CLIENTS),
        ] {
            let menu = told(counting(), exit_code, stdout, stderr, &numbered(ARRIVING_VALUE, 1));
            let said = format!("menu: {NO_TAB}: {said}");
            assert_eq!(wrong(menu.as_ref()), Some(said.as_str()), "{stdout:?}");
        }

        let mut menu = arriving();
        for _ in 0..close::PATIENCE {
            menu = menu.tick();
        }
        assert_eq!(wrong(Some(&menu)), Some("menu: \"elo\" did not take you, and no tab opened"));
    }

    /// `n s` names the session once `WORKTREES` says whether the directory is
    /// a worktree, and starts it whether it said or failed. A key on its way
    /// meanwhile does nothing, and `Esc` closes the menu.
    #[test]
    fn a_session_waits_for_its_name() {
        let naming = |menu: u64| numbered(NAMING_VALUE, menu);
        let waiting = || {
            let menu = press(open(None, row(None), Some('n')), &char('s')).expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            assert_eq!(menu.cwd(), Some("/w/elo"));
            menu
        };
        let menu = press(Some(waiting()), &char('s')).expect("a menu");
        let menu = told(menu, Some(0), b"", b"", &naming(2)).expect("a menu");
        assert_eq!(on_show(Some(&menu)), Some(Ok(vec!['p', 't', 's'])));
        for exit_code in [Some(0), Some(2), None] {
            assert!(told(waiting(), exit_code, b"", b"", &naming(1)).is_none(), "{exit_code:?}");
        }
        assert!(press(Some(waiting()), &bare(BareKey::Esc)).is_none());
    }

    /// In another session, the menu takes no key while zellij opens the
    /// agent's pane but the one that closes it, and goes there once it has,
    /// though zellij say not which pane it is. When zellij fails, the menu
    /// says what it said. An answer for another menu, as one that waited for
    /// the same session, is for a menu closed before it came.
    #[test]
    fn an_agent_in_another_session_is_gone_to_once_its_pane_is_open() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), };
        let started = |menu: u64| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), START_VALUE.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ])
        };
        let none: &[u8] = b"";
        for (exit_code, stdout, stderr, said) in [
            (Some(0), &b"terminal_12\n"[..], none, None),
            (Some(0), none, none, None),
            (
                Some(1),
                none,
                &b"There is no active session!\n"[..],
                Some("menu: the agent did not start: There is no active session!"),
            ),
            (None, none, none, Some("menu: the agent did not start: zellij opened no pane")),
        ] {
            let menu = open(Some(MENU), row(Some(there.clone())), Some('r'));
            let menu = press(menu, &char('r')).expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            let menu = told(menu, Some(0), b"terminal_7\n", none, &started(2));
            let menu = menu.expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            let menu = told(menu, exit_code, stdout, stderr, &started(1));
            let wrong = menu.as_ref().map(|menu| wrong(Some(menu)));
            assert_eq!(wrong, said.map(Some), "{exit_code:?} {stdout:?}");
        }
        let menu = open(Some(MENU), row(Some(there)), Some('r'));
        assert!(press(menu, &bare(BareKey::Esc)).is_none());
    }

    /// Once the agent's pane opened in the sidebar's own session, the menu
    /// shows nothing, and takes a key that comes before the focus has gone to
    /// that pane, `r` and `Esc` alike: a second press starts no second agent.
    /// zellij answers the pane opened on stdin, which no test has, so the
    /// menu stands here as starting one leaves it.
    #[test]
    fn a_key_on_its_way_after_an_agent_starts_here_does_nothing() {
        let mut menu = SidebarMenu { number: 1, back: None, stage: Stage::Started(10) };
        assert!(menu.shown().is_none());
        assert_eq!(menu.cwd(), None);
        for key in [char('r'), bare(BareKey::Esc)] {
            menu = menu.key(&key, None, 11).0.expect("a menu");
            assert_eq!(menu.started_at(), Some(10));
        }
        let choosing = open(Some(MENU), row(None), Some('c')).expect("a menu");
        assert_eq!(choosing.started_at(), None);
    }

    /// A delete key with no agent at work to end deletes at once, and says
    /// what it deleted, for the sidebar's cursor to go past it. The session
    /// you are in is left first, and the menu lasts while you leave. On a
    /// session's row, `d d` deletes the session.
    #[test]
    fn a_delete_key_with_no_agent_at_work_to_end_deletes_at_once() {
        let idle = closing(luneta(), &[(2, "idle"), (3, "shell")]);
        let leave = || Act::Leave { session: "luneta".to_owned(), to: "elo".to_owned() };
        let session = close::Subject { pane: None, ..closing(luneta(), &[]) };
        for (row, subject, key, act) in [
            (row(None), &idle, 'd', Act::Pane(luneta(), 2)),
            (row(None), &idle, 's', leave()),
            (session_row(None), &session, 'd', leave()),
        ] {
            let menu = open(Some(MENU), row, Some('d')).expect("a menu");
            let (menu, closed) = menu.key(&char(key), Some(subject), 0);
            let leaves = matches!(act, Act::Leave { .. });
            assert_eq!(closed, Some(Did::Closed(act)), "{key}");
            assert_eq!(menu.as_ref().is_some_and(SidebarMenu::lasts), leaves, "{key}");
        }
    }

    /// A delete key that would end an agent busy or waiting on you asks
    /// first, and says what goes. `y`, or the key again, goes ahead; `n` goes
    /// back to the keys, and `Esc` closes the menu, deleting nothing.
    #[test]
    fn a_delete_key_that_would_end_an_agent_at_work_asks_first() {
        let busy = closing(luneta(), &[(2, "busy"), (3, "waiting")]);
        let asked = || {
            let menu = open(Some(MENU), row(None), Some('d')).expect("a menu");
            let (menu, closed) = menu.key(&char('d'), Some(&busy), 0);
            assert_eq!(closed, None);
            assert_eq!(asking(menu.as_ref()), Some("Close the pane? 1 agent, 1 busy"));
            menu.expect("a menu")
        };
        for key in [char('y'), char('d')] {
            let (menu, closed) = asked().key(&key, Some(&busy), 0);
            assert!(menu.is_none(), "{key:?}");
            assert_eq!(closed, Some(Did::Closed(Act::Pane(luneta(), 2))), "{key:?}");
        }
        let (menu, closed) = asked().key(&char('x'), Some(&busy), 0);
        assert!(asking(menu.as_ref()).is_some());
        assert_eq!(closed, None);
        let (menu, closed) = asked().key(&char('n'), Some(&busy), 0);
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['d', 's'])));
        assert_eq!(closed, None);
        let (menu, closed) = asked().key(&bare(BareKey::Esc), Some(&busy), 0);
        assert!(menu.is_none());
        assert_eq!(closed, None);
    }

    /// Going ahead plans the delete key again, from what zellij says now. A
    /// pane that opened in the tab while you read holds the tab open, and the
    /// key asks again, to close the pane alone; what did not change goes
    /// ahead.
    #[test]
    fn going_ahead_closes_what_the_tab_holds_now() {
        let alone = close::Subject {
            pane: Some(close::Pane { id: 2, tab: Some(TabId(7)), closes: close::Closes::Tab }),
            ..closing(luneta(), &[(2, "busy")])
        };
        let joined = closing(luneta(), &[(2, "busy")]);
        let asked = || {
            let menu = open(Some(MENU), row(None), Some('d')).expect("a menu");
            let (menu, _) = menu.key(&char('d'), Some(&alone), 0);
            assert_eq!(asking(menu.as_ref()), Some("Close its tab? 1 agent, 1 busy"));
            menu.expect("a menu")
        };
        let (menu, closed) = asked().key(&char('y'), Some(&alone), 0);
        assert!(menu.is_none());
        assert_eq!(closed, Some(Did::Closed(Act::Tab(luneta(), TabId(7)))));
        let (menu, closed) = asked().key(&char('y'), Some(&joined), 0);
        assert_eq!(closed, None);
        assert_eq!(asking(menu.as_ref()), Some("Close the pane? 1 agent, 1 busy"));
        let (menu, closed) = menu.expect("a menu").key(&char('y'), Some(&joined), 0);
        assert!(menu.is_none());
        assert_eq!(closed, Some(Did::Closed(Act::Pane(luneta(), 2))));
    }

    /// A pane of another session closes by a command, and the menu waits for
    /// its answer: gone once it closed, saying what zellij said when it did
    /// not. An answer to a command of another kind, or for another menu, is
    /// for a menu closed before it came.
    #[test]
    fn a_pane_of_another_session_closes_by_a_command_the_menu_waits_on() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), };
        let elo = Seat::There("elo".to_owned());
        let answer = |value: &str, menu: u64| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), value.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ])
        };
        let none: &[u8] = b"";
        for (exit_code, stderr, said) in [
            (Some(0), none, None),
            (
                Some(1),
                &b"No pane with id terminal_2\n"[..],
                Some("menu: nothing closed: No pane with id terminal_2"),
            ),
        ] {
            let menu = open(Some(MENU), row(Some(there.clone())), Some('d'));
            let pane = closing(elo.clone(), &[]);
            let (menu, closed) = menu.expect("a menu").key(&char('d'), Some(&pane), 0);
            assert_eq!(closed, Some(Did::Closed(Act::Pane(elo.clone(), 2))));
            let menu = menu.expect("a menu waiting");
            let menu = told(menu, Some(0), none, none, &answer(START_VALUE, 1));
            let menu = menu.expect("a menu waiting");
            let menu = told(menu, Some(0), none, none, &answer(CLOSING_VALUE, 2));
            let menu = menu.expect("a menu waiting");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            let menu = told(menu, exit_code, none, stderr, &answer(CLOSING_VALUE, 1));
            assert_eq!(menu.as_ref().map(|menu| wrong(Some(menu))), said.map(Some));
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

    /// The key binding's menu, floating over pane 2 of session `luneta`,
    /// which shares tab 7 with pane 3, with `elo` to go to before `luneta`
    /// ends, and claude-ps saying a busy agent sits in pane 3, once
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
            let context = |value: &str| {
                BTreeMap::from([
                    (CONTEXT_KEY.to_owned(), value.to_owned()),
                    (SESSION_KEY.to_owned(), "luneta".to_owned()),
                ])
            };
            let agents = br#"[{"status":"busy","status_age":4,"cwd":"/w/elo",
                "zellij":{"session":"luneta","pane":"3"}}]"#;
            menu.ran(Some(0), agents, b"", &context(agents::CONTEXT_VALUE));
            let panes = br#"[{"id":2,"pane_cwd":"/w/elo"},{"id":3,"pane_cwd":"/w/elo"}]"#;
            menu.ran(Some(0), panes, b"", &context(places::CONTEXT_VALUE));
        }
        menu
    }

    fn question(menu: &Menu) -> Option<&str> {
        match menu.shown()? {
            Shown::Asking(_, question) => Some(question),
            Shown::Keys(_) | Shown::Leaving(_) | Shown::Wrong(_) => None,
        }
    }

    /// The key binding's menu closes the pane you came from, or kills its
    /// session, as the sidebar's does: at once with no agent at work to end,
    /// and asking first, until claude-ps and the session have said where the
    /// agents sit.
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

    /// In another session, zellij opens the pane in the agent's tab and its
    /// directory, and says which pane it opened, for the menu to go there.
    #[test]
    fn an_agent_in_another_session_starts_in_its_tab_and_its_directory() {
        let command = claude("/tigerstyle", Some("main"));
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)) };
        assert_eq!(
            new_pane(&there, "/w/elo/main", &command),
            [
                "zellij", "--session", "elo", "action", "new-pane", "--tab-id", "3", "--cwd",
                "/w/elo/main", "--", "claude", "--name", "tigerstyle: main", "/tigerstyle",
            ]
        );
        let somewhere = There { session: "elo".to_owned(), tab: None };
        assert_eq!(
            new_pane(&somewhere, "/w/elo", &command)[..8],
            ["zellij", "--session", "elo", "action", "new-pane", "--cwd", "/w/elo", "--"]
        );
        assert_eq!(opened(b"terminal_12\n"), Some(12));
        assert_eq!(opened(b"plugin_3\n"), None);
        assert_eq!(opened(b""), None);
    }

    /// The tab `OPEN_TAB` opened stands where the list after its id puts it,
    /// which is not its id once a tab before it has closed. A tab the list
    /// does not have stands nowhere the menu knows.
    #[test]
    fn a_tab_opened_in_another_session_stands_where_its_list_says() {
        let list = "TAB_ID  POSITION  NAME\n0  0  Tab #1\n2  1  Tab #3\n3  2  my tab\n";
        for (printed, position) in [
            (format!("3\n{list}"), Some(2)),
            (format!("2\n{list}"), Some(1)),
            (format!("5\n{list}"), None),
            ("3\n".to_owned(), None),
            (list.to_owned(), None),
            (String::new(), None),
        ] {
            assert_eq!(tab_position(printed.as_bytes()), position, "{printed:?}");
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

    /// The key binding opens the menu for the pane you are in. Opened in the
    /// sidebar, or any pane that is no terminal, it has none to start an agent
    /// beside, and says so rather than start one wherever zellij puts it.
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
        // The sidebar, and the menu itself floating.
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
