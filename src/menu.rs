//! The menu: a key, then another, and an agent is at work, as which-key shows
//! the keys in neovim.
//!
//! Its `menu` holds its entries: a key each, and under a key that opens a
//! group, the keys of the group. The menu shows the keys of the group open and
//! what each does. A key with nothing under it starts Claude Code on its
//! prompt, in a pane of its own beside a pane, in the directory that pane
//! works in. Every menu has luneta's own Delete group besides, under `d`,
//! which closes that pane or kills its session. In the sidebar, a key of the
//! menu opens it at the bottom of the sidebar, for the row highlighted.
//! `mode "menu"` makes the plugin the menu for the pane you came from,
//! floating at the bottom right of the tab you are in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::*;

use crate::agents::{self, AgentSet, Live, Seat};
use crate::checkouts::{Checkouts, DIR_KEY};
use crate::close::{self, Act, Leaving, Plan, Ran, Scope};
use crate::dock::TabId;
use crate::elapsed::Age;
use crate::fetch::Fetch;
use crate::places::{self, Places, SESSION_KEY};
use crate::render;
use crate::sessions::Selection;
use crate::tree::{self, origin};

/// What holds the entries in the menu's configuration: a node with children,
/// which zellij hands over as their KDL.
pub const ENTRIES: &str = "menu";

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

/// What the sidebar's menu says of a key that starts an agent, on a session's
/// row, which the cursor is only on while it lists none of the session's
/// panes.
const NO_ROW_PANE: &str = "menu: a session's row has no pane to start an agent beside; unfold it, \
                           and highlight one of its panes";

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

/// What the menu asks of the host, as the context of a command it runs.
const CONTEXT_KEY: &str = "luneta";
const BRANCH_VALUE: &str = "branch";
/// The sidebar's menu opening an agent's pane in another session, or closing
/// a pane or a tab of one, and the number of the menu that asked.
pub const START_VALUE: &str = "start";
pub const CLOSING_VALUE: &str = "closing";
pub const MENU_KEY: &str = "luneta_menu";

/// How often, in seconds, the floating menu looks whether you have left the
/// session it is to kill.
const LEAVING_TICK: f64 = 0.1;

/// Where a floating pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

/// A key of the sidebar's own that does more than move the cursor or fold a
/// session. `?` lists each one beside the menu's keys.
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
    /// Goes to the next agent that wants you.
    Next,
}

impl Command {
    /// Every command, in the order `?` lists them.
    pub const ALL: &[Command] = &[Command::Search, Command::Next];

    /// The command `key` runs, if any.
    pub fn of(key: char) -> Option<Command> {
        Command::ALL.iter().copied().find(|command| command.key() == key)
    }

    pub const fn key(self) -> char {
        match self {
            Command::Search => '/',
            Command::Next => 'n',
        }
    }

    /// What `?` says the command does.
    pub fn does(self) -> &'static str {
        match self {
            Command::Search => "Search",
            Command::Next => "Next agent",
        }
    }
}

/// No two commands share a key: `of` would find the first, and `?` would
/// show both.
const _: () = {
    let mut at = 0;
    while at < Command::ALL.len() {
        let mut other = at + 1;
        while other < Command::ALL.len() {
            assert!(Command::ALL[at].key() != Command::ALL[other].key(), "two commands on one key");
            other += 1;
        }
        at += 1;
    }
};

pub enum Entry {
    /// A key that opens a group, the name the menu shows for it, and the keys
    /// in it.
    Group { key: char, name: String, entries: Vec<Entry> },
    /// A key that starts an agent on a prompt.
    Start { key: char, prompt: String },
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
    /// the name of the group its children are. luneta's Delete group comes
    /// after them: `d d` deletes what the menu is for, as `row` says, and
    /// `d s` kills its session. A key of yours can take neither.
    pub fn new(configured: Option<&str>, row: Scope) -> Result<Self, String> {
        let mut root = match configured {
            Some(text) => {
                let document: KdlDocument =
                    text.parse().map_err(|error| format!("menu: {error}"))?;
                entries(&document, "")?
            },
            None => Vec::new(),
        };
        if root.iter().any(|entry| entry.key() == DELETE) {
            return Err(format!(
                "menu: \"{DELETE}\" is luneta's, for its Delete group; give your key another"
            ));
        }
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
        root.retain(|entry| Command::of(entry.key()).is_none());
        root.extend(Command::ALL.iter().copied().map(Entry::Command));
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
    /// its agent, closes what it closes or runs its command, `Backspace` goes
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
    /// The pane of the menu's own session you came from. zellij keeps where
    /// each session's focus was, and the sidebar the menu is in has it: the
    /// session would have you back in the sidebar.
    pub back: Option<u32>,
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

/// Opens a pane that runs `command` in a tab of another session, in `cwd`.
/// zellij says which pane it opened, as `terminal_<id>`.
fn new_pane(there: &There, cwd: &str, command: &[String]) -> Vec<String> {
    let session = there.session.as_str();
    let mut new_pane: Vec<String> =
        ["zellij", "--session", session, "action", "new-pane"].map(str::to_owned).to_vec();
    if let Some(tab) = there.tab {
        new_pane.extend(["--tab-id".to_owned(), tab.0.to_string()]);
    }
    new_pane.extend(["--cwd".to_owned(), cwd.to_owned(), "--".to_owned()]);
    new_pane.extend(command.iter().cloned());
    new_pane
}

/// The terminal pane `zellij action new-pane` says it opened.
fn opened(stdout: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(stdout).trim().strip_prefix("terminal_")?.parse().ok()
}

/// A row of the sidebar, for its menu: the pane an agent starts beside, none
/// on a session's row, the directory it works in and the branch checked out
/// there, as far as the sidebar knows, and when it is in another session, the
/// session, its tab, and the pane to hand the focus back to before going
/// there.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub pane: Option<u32>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub there: Option<There>,
}

impl Row {
    /// The keys of the menu for the row, from the `entries` of the sidebar's
    /// configuration and luneta's Delete group, and what the menu is for.
    fn menu(self, entries: Option<&str>) -> Result<(Keys, Target), String> {
        let deletes = if self.pane.is_some() { Scope::Pane } else { Scope::Session };
        let keys = Keys::new(entries, deletes)?;
        let target =
            Target { pane: self.pane, cwd: self.cwd, branch: self.branch, there: self.there };
        Ok((keys, target))
    }
}

/// What the sidebar's menu is for: a row's pane, or none on a session's row,
/// in the sidebar's own session or `there`, where an agent starts beside it,
/// and what the Delete group deletes.
struct Target {
    pane: Option<u32>,
    cwd: Option<String>,
    branch: Option<String>,
    there: Option<There>,
}

impl Target {
    /// Starts Claude Code on `prompt` beside the pane, in the directory it
    /// works in, for the menu numbered `menu`, at `frame`. In the sidebar's
    /// own session the pane opens at once. Another session opens it itself,
    /// and the menu, with `keys` on show, waits for zellij to say it has. A
    /// pane the sidebar does not know the directory of yet starts none,
    /// rather than one wherever zellij runs, and a session's row has no pane.
    fn start(self, keys: Keys, prompt: &str, menu: u64, frame: u64) -> Stage {
        let Some(pane) = self.pane else {
            return wrong(NO_ROW_PANE.to_owned());
        };
        let Some(cwd) = self.cwd else {
            return wrong(format!(
                "menu: the sidebar does not know yet where pane {pane} works; try again in a \
                 moment"
            ));
        };
        let beside = Beside { pane, cwd };
        let command = claude(prompt, self.branch.as_deref());
        let Some(there) = self.there else {
            start_here(&command, &beside);
            return Stage::Started(frame);
        };
        let new_pane = new_pane(&there, &beside.cwd, &command);
        let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
        run_command(
            &new_pane,
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), START_VALUE.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ]),
        );
        Stage::Waiting(keys, Wait::Start(beside, there))
    }

    /// Asks zellij for what a delete key deletes, for the menu numbered
    /// `menu`, with what that is. A pane or a tab of another session leaves
    /// the menu waiting for its answer, and the session you are in leaves it
    /// showing where you are going. No stage once the menu is done.
    fn close(self, keys: Keys, act: Act, menu: u64) -> (Option<Stage>, Option<Act>) {
        let context = BTreeMap::from([
            (CONTEXT_KEY.to_owned(), CLOSING_VALUE.to_owned()),
            (MENU_KEY.to_owned(), menu.to_string()),
        ]);
        let stage = match act.run(context) {
            Ran::Done => None,
            Ran::Asked => Some(Stage::Waiting(keys, Wait::Close { cwd: self.cwd })),
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
    stage: Stage,
}

enum Stage {
    /// The keys of the group open, pressed one after another, for a row.
    Choosing(Keys, Target),
    /// A delete key that asks before it goes ahead.
    Asking(Keys, Target, Asked),
    /// Another session is opening the agent's pane, or closing the pane a
    /// delete key closes. Keys do nothing until zellij says it has, but close
    /// the menu.
    Waiting(Keys, Wait),
    /// The agent's pane opened in the sidebar's own session at this frame,
    /// and the focus is on its way there. A key that comes soon after was on
    /// its way already, and does nothing: a second press starts no second
    /// agent. Nothing shows.
    Started(u64),
    /// The client is on its way to another session, and the session it
    /// leaves ends once it is gone. Keys do nothing.
    Leaving(Leaving),
    /// What is wrong with the keys, with the row, or with what a key asked
    /// for. Any key closes the menu.
    Wrong(String),
}

/// What another session does for the sidebar's menu, and says when it has.
enum Wait {
    /// Opens an agent's pane beside a pane of it, for the menu to go there.
    Start(Beside, There),
    /// Closes a pane or a tab of it, for a pane that works in `cwd`.
    Close { cwd: Option<String> },
}

impl Wait {
    /// What the menu ran the command with, as its context.
    fn value(&self) -> &str {
        match self {
            Wait::Start(..) => START_VALUE,
            Wait::Close { .. } => CLOSING_VALUE,
        }
    }
}

impl SidebarMenu {
    /// The menu for `row`, numbered next among `numbers`, from the `entries`
    /// of the sidebar's configuration and luneta's Delete group, at `frame`:
    /// on the group `key` opens, or on the first with no key. A key that
    /// starts an agent starts it, and the menu stays only while another
    /// session does it, or while the keys on their way to an agent it started
    /// come. None for a key the menu has not. On a session's row, `d d` kills
    /// the session.
    pub fn open(
        entries: Option<&str>,
        row: Row,
        key: Option<char>,
        numbers: &mut Numbers,
        frame: u64,
    ) -> Option<Self> {
        let number = numbers.next();
        let (keys, target) = match row.menu(entries) {
            Ok(menu) => menu,
            Err(reason) => return Some(SidebarMenu { number, stage: wrong(reason) }),
        };
        let has = |key: char| keys.entries().iter().any(|entry| entry.key() == key);
        if key.is_some_and(|key| !has(key)) {
            return None;
        }
        let choosing = SidebarMenu { number, stage: Stage::Choosing(keys, target) };
        match key {
            Some(key) => choosing.key(&KeyWithModifier::new(BareKey::Char(key)), None, frame).0,
            None => Some(choosing),
        }
    }

    /// What `?` opens, numbered next among `numbers`: the keys at the top of
    /// the menu for `row`, as `m` opens it, and then the sidebar's commands.
    pub fn help(entries: Option<&str>, row: Row, numbers: &mut Numbers) -> Self {
        let number = numbers.next();
        let stage = match row.menu(entries) {
            Ok((keys, target)) => Stage::Choosing(Keys::help(keys), target),
            Err(reason) => wrong(reason),
        };
        SidebarMenu { number, stage }
    }

    /// What the menu shows: the keys, what a delete key asks, where you are
    /// going, or what is wrong with the keys, with the row, or with what a key
    /// asked for. Nothing once an agent started in the sidebar's own session.
    pub fn shown(&self) -> Option<Shown<'_>> {
        match &self.stage {
            Stage::Choosing(keys, _) | Stage::Waiting(keys, _) => Some(Shown::Keys(keys)),
            Stage::Asking(keys, _, asked) => Some(Shown::Asking(keys, &asked.question)),
            Stage::Leaving(leaving) => Some(Shown::Leaving(leaving.to())),
            Stage::Wrong(reason) => Some(Shown::Wrong(reason)),
            Stage::Started(_) => None,
        }
    }

    /// The directory an agent the menu starts works in, as far as the sidebar
    /// knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.stage {
            Stage::Choosing(_, target) | Stage::Asking(_, target, _) => target.cwd.as_deref(),
            Stage::Waiting(_, Wait::Start(beside, _)) => Some(&beside.cwd),
            Stage::Waiting(_, Wait::Close { cwd }) => cwd.as_deref(),
            Stage::Started(_) | Stage::Leaving(_) | Stage::Wrong(_) => None,
        }
    }

    /// The frame the menu started an agent at in the sidebar's own session,
    /// for the sidebar to say when the keys on their way then have all come.
    pub fn started_at(&self) -> Option<u64> {
        match self.stage {
            Stage::Started(at) => Some(at),
            Stage::Choosing(..)
            | Stage::Asking(..)
            | Stage::Waiting(..)
            | Stage::Leaving(_)
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
        Some((target.pane, target.there.as_ref().map(|there| there.session.as_str())))
    }

    /// Whether the menu outlasts the focus: the client takes the focus along
    /// on its way out of the session, which ends once it is gone.
    pub fn lasts(&self) -> bool {
        matches!(self.stage, Stage::Leaving(_))
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
        let number = self.number;
        let (stage, act) = match self.stage {
            Stage::Wrong(_) => return (None, None),
            Stage::Waiting(..) if closes(key) => return (None, None),
            Stage::Waiting(..) | Stage::Started(_) | Stage::Leaving(_) => {
                return (Some(self), None);
            },
            Stage::Choosing(mut keys, target) => match keys.key(key) {
                Pressed::Close => return (None, None),
                Pressed::Command(command) => return (None, Some(Did::Command(command))),
                Pressed::Start(prompt) => (Some(target.start(keys, &prompt, number, frame)), None),
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
        (stage.map(|stage| SidebarMenu { number, stage }), act.map(Did::Closed))
    }

    /// Closes the menu once the pane an agent would start beside, in the
    /// sidebar's own session, has closed: zellij would open the agent beside
    /// the pane with the focus, which is the sidebar. None once it has.
    pub fn panes(self, manifest: &PaneManifest) -> Option<Self> {
        let Stage::Choosing(_, Target { pane: Some(pane), there: None, .. }) = &self.stage else {
            return Some(self);
        };
        let pane = *pane;
        let open = manifest.panes.values().flatten().any(|p| !p.is_plugin && p.id == pane);
        open.then_some(self)
    }

    /// What another session said of what the menu waits for: the agent's
    /// pane open, or what a delete key closes closed. The menu goes to the
    /// agent's pane, and to the session when zellij says not which pane it
    /// is, or closes, or says what zellij said went wrong. None once it went
    /// or closed. An answer for anything the menu does not wait for, or for
    /// another menu, is from one closed before it came.
    pub fn answered(
        self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> Option<Self> {
        let Stage::Waiting(_, wait) = &self.stage else {
            return Some(self);
        };
        let said = |key: &str| context.get(key).map(String::as_str);
        let number = self.number.to_string();
        if said(CONTEXT_KEY) != Some(wait.value()) || said(MENU_KEY) != Some(&number) {
            return Some(self);
        }
        let Some(0) = exit_code else {
            let (what, nothing) = match wait {
                Wait::Start(..) => ("the agent did not start", "zellij opened no pane"),
                Wait::Close { .. } => ("nothing closed", "zellij said nothing"),
            };
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            let reason = if reason.is_empty() { nothing } else { reason };
            let stage = wrong(format!("menu: {what}: {reason}"));
            return Some(SidebarMenu { number: self.number, stage });
        };
        if let Wait::Start(_, there) = wait {
            if let Some(back) = there.back {
                focus_terminal_pane(back, false, false);
            }
            match opened(stdout) {
                Some(pane) => switch_session_with_focus(&there.session, None, Some((pane, false))),
                // The pane is open, and zellij did not say which it is: the
                // session, as it stands.
                None => switch_session(Some(&there.session)),
            }
        }
        None
    }

    /// While the client leaves the session: asks zellij whether it is gone,
    /// and says why the session stays once it has taken too long.
    pub fn tick(self) -> Self {
        let Stage::Leaving(mut leaving) = self.stage else {
            return self;
        };
        let stage = match leaving.tick() {
            Ok(()) => Stage::Leaving(leaving),
            Err(reason) => wrong(reason),
        };
        SidebarMenu { number: self.number, stage }
    }

    /// What zellij said, as it listed the clients of the session, while
    /// `client`, the sidebar's, leaves it: kills the session once the client
    /// is gone. None once it has.
    pub fn clients(
        mut self,
        client: ClientId,
        exit_code: Option<i32>,
        stdout: &[u8],
    ) -> Option<Self> {
        let Stage::Leaving(leaving) = &mut self.stage else {
            return Some(self);
        };
        (!leaving.listed(client, exit_code, stdout)).then_some(self)
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
            Phase::Choosing(keys, _) => Some(Shown::Keys(keys)),
            Phase::Asking(keys, _, asked) => Some(Shown::Asking(keys, &asked.question)),
            Phase::Leaving(leaving) => Some(Shown::Leaving(leaving.to())),
            Phase::Done => None,
        }
    }

    /// The directory an agent the menu starts works in, once the menu knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.phase {
            Phase::Choosing(_, Fetch::Ready(origin)) | Phase::Asking(_, origin, _) => {
                Some(&origin.beside().cwd)
            },
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
            Phase::Wrong(_) | Phase::Choosing(_, Fetch::Failed(_)) => self.close(),
            Phase::Choosing(keys, _) => match keys.key(key) {
                Pressed::Moved => {
                    self.place();
                    true
                },
                Pressed::Start(prompt) => self.start(&prompt),
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
    /// session work, and the clients of the session while you leave it.
    pub fn ran(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) {
        match context.get(CONTEXT_KEY).map(String::as_str) {
            Some(BRANCH_VALUE) => self.branched(exit_code, stdout, context),
            Some(agents::CONTEXT_VALUE) => {
                self.agents.ingest(exit_code, stdout, stderr);
                self.seat_agents();
            },
            Some(places::CONTEXT_VALUE) => {
                let Some(session) = context.get(SESSION_KEY) else {
                    return;
                };
                self.places.ingest(session.clone(), exit_code, stdout);
                self.places_in = true;
                self.seat_agents();
            },
            Some(close::CLIENTS_VALUE) => self.clients(exit_code, stdout),
            _ => {},
        }
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
        match leaving.tick() {
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
        assert_eq!(shown(&keys), ['c', 'r', 'd']);
        assert!(keys.names().is_empty());

        assert_eq!(keys.key(&char('x')), Pressed::Nothing);
        assert_eq!(keys.key(&char('c')), Pressed::Moved);
        assert_eq!(shown(&keys), ['t', 'm', 's']);
        assert_eq!(keys.names(), ["Claude"]);
        // A key of the group before is not one of this group's.
        assert_eq!(keys.key(&char('r')), Pressed::Nothing);
        assert_eq!(keys.key(&char('s')), Pressed::Start("/tigerstyle".to_owned()));

        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert_eq!(shown(&keys), ['c', 'r', 'd']);
        assert_eq!(keys.key(&char('r')), Pressed::Start("review the diff against main".to_owned()));
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Close);
        keys.key(&char('c'));
        assert_eq!(keys.key(&bare(BareKey::Esc)), Pressed::Close);
        assert_eq!(keys.key(&char('c').with_ctrl_modifier()), Pressed::Close);
    }

    /// Every menu has luneta's Delete group after your keys, and one with
    /// none of yours too: `d d` deletes what the menu is for, a pane or a
    /// session's row, and `d s` its session.
    #[test]
    fn every_menu_has_luneta_s_delete_keys() {
        for (configured, root) in [
            (Some(MENU), &['c', 'r', 'd'][..]),
            (None, &['d']),
            (Some(""), &['d']),
            (Some("// nothing but a comment"), &['d']),
        ] {
            let mut keys = Keys::new(configured, Scope::Pane).expect("a menu");
            assert_eq!(shown(&keys), root, "{configured:?}");
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

    /// Pane 2 of the sidebar, working in `/w/elo`, of the sidebar's own
    /// session or of `there`.
    fn row(there: Option<There>) -> Row {
        let branch = Some("main".to_owned());
        Row { pane: Some(2), cwd: Some("/w/elo".to_owned()), branch, there }
    }

    /// The row of the sidebar's own session, or of `there`.
    fn session_row(there: Option<There>) -> Row {
        Row { pane: None, cwd: None, branch: None, there }
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

    /// `key` pressed in the sidebar's menu at frame 0, its pane with no agent.
    fn press(menu: Option<SidebarMenu>, key: &KeyWithModifier) -> Option<SidebarMenu> {
        menu.and_then(|menu| menu.key(key, Some(&closing(luneta(), &[])), 0).0)
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
            (None, Some(vec!['c', 'r', 'd'])),
            (Some('d'), Some(vec!['d', 's'])),
            (Some('x'), None),
        ] {
            let menu = open(Some(MENU), row(None), key);
            assert_eq!(on_show(menu.as_ref()), on.map(Ok), "{key:?}");
        }
        let menu = open(Some(MENU), row(None), Some('c'));
        assert_eq!(menu.as_ref().and_then(SidebarMenu::cwd), Some("/w/elo"));
        let menu = press(menu, &bare(BareKey::Backspace));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'd'])));
        let menu = press(menu, &char('x'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'd'])));
        assert!(press(menu, &bare(BareKey::Esc)).is_none());
    }

    /// A menu whose `n` the sidebar's `Next` has, as it has at the top of the
    /// sidebar.
    const SHADOWED: &str = r#"n "notes"; c "Claude" { t "/tigerstyle"; }"#;

    /// `?` shows the keys at the top of the menu, as `m` does, luneta's Delete
    /// group among them, and then the sidebar's commands, which keep their
    /// keys from the menu's. So it does on a session's row, and with no menu
    /// of yours. A menu that cannot be read says what is wrong with it.
    #[test]
    fn help_shows_the_top_of_the_menu_and_then_the_sidebar_s_commands() {
        for (entries, on_row, on) in [
            (Some(MENU), row(None), vec!['c', 'r', 'd', '/', 'n']),
            (Some(SHADOWED), row(None), vec!['c', 'd', '/', 'n']),
            (Some(MENU), session_row(None), vec!['c', 'r', 'd', '/', 'n']),
            (None, row(None), vec!['d', '/', 'n']),
        ] {
            let menu = SidebarMenu::help(entries, on_row, &mut Numbers::default());
            assert_eq!(on_show(Some(&menu)), Some(Ok(on)), "{entries:?}");
        }
        for (entries, said) in [
            ("t", "menu: \"t\" takes one string"),
            (r#"d "notes""#, "menu: \"d\" is luneta's"),
        ] {
            let menu = SidebarMenu::help(Some(entries), row(None), &mut Numbers::default());
            let wrong = wrong(Some(&menu)).expect("what is wrong");
            assert!(wrong.starts_with(said), "{wrong}");
        }
    }

    /// A group of the menu opens from `?`, and `Backspace` comes back to it.
    /// A delete key deletes as it does from `m`. A command closes the menu,
    /// for the sidebar to run it, on a session's row too.
    #[test]
    fn a_key_of_help_opens_its_group_or_hands_the_sidebar_its_command() {
        let help = || Some(SidebarMenu::help(Some(MENU), row(None), &mut Numbers::default()));
        let menu = press(help(), &char('c'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['t', 'm', 's'])));
        let menu = press(menu, &bare(BareKey::Backspace));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r', 'd', '/', 'n'])));
        assert!(press(menu, &bare(BareKey::Esc)).is_none());

        let menu = press(help(), &char('d'));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['d', 's'])));
        let idle = closing(luneta(), &[]);
        let (menu, did) = menu.expect("a menu").key(&char('d'), Some(&idle), 0);
        assert!(menu.is_none());
        assert_eq!(did, Some(Did::Closed(Act::Pane(luneta(), 2))));

        for (entries, on_row, key, command) in [
            (MENU, row(None), '/', Command::Search),
            (SHADOWED, row(None), 'n', Command::Next),
            (MENU, session_row(None), '/', Command::Search),
        ] {
            let menu = SidebarMenu::help(Some(entries), on_row, &mut Numbers::default());
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
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), back: Some(1) };
        let answer = |menu: u64| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), START_VALUE.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ])
        };
        let help = SidebarMenu::help(Some(MENU), row(Some(there)), &mut Numbers(1));
        let menu = press(Some(help), &char('r')).expect("a menu");
        let menu = menu.answered(None, b"", b"", &answer(1)).expect("a menu");
        assert_eq!(on_show(Some(&menu)), Some(Ok(vec!['c', 'r', 'd', '/', 'n'])));
        let menu = menu.answered(None, b"", b"", &answer(2));
        let said = wrong(menu.as_ref());
        assert_eq!(said, Some("menu: the agent did not start: zellij opened no pane"));
    }

    /// A row the sidebar does not know the directory of yet has no agent to
    /// start, and the menu says so rather than start one wherever zellij
    /// runs. Nor has a session's row, which has no pane. A menu that cannot
    /// be read says what is wrong with it for any key it might have had. Any
    /// key closes what is wrong.
    #[test]
    fn a_menu_that_cannot_start_an_agent_beside_its_row_says_why() {
        let unknown = || Row { cwd: None, ..row(None) };
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
    }

    /// In another session, the menu takes no key while zellij opens the
    /// agent's pane but the one that closes it, and goes there once it has,
    /// though zellij say not which pane it is. When zellij fails, the menu
    /// says what it said. An answer for another menu, as one that waited for
    /// the same session, is for a menu closed before it came.
    #[test]
    fn an_agent_in_another_session_is_gone_to_once_its_pane_is_open() {
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), back: Some(1) };
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
            let menu = menu.answered(Some(0), b"terminal_7\n", none, &started(2));
            let menu = menu.expect("a menu");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            let menu = menu.answered(exit_code, stdout, stderr, &started(1));
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
        let mut menu = SidebarMenu { number: 1, stage: Stage::Started(10) };
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
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), back: Some(1) };
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
            let menu = menu.answered(Some(0), none, none, &answer(START_VALUE, 1));
            let menu = menu.expect("a menu waiting");
            let menu = menu.answered(Some(0), none, none, &answer(CLOSING_VALUE, 2));
            let menu = menu.expect("a menu waiting");
            assert!(matches!(menu.stage, Stage::Waiting(..)));
            let menu = menu.answered(exit_code, none, stderr, &answer(CLOSING_VALUE, 1));
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
        let there = There { session: "elo".to_owned(), tab: Some(TabId(3)), back: None };
        assert_eq!(
            new_pane(&there, "/w/elo/main", &command),
            [
                "zellij", "--session", "elo", "action", "new-pane", "--tab-id", "3", "--cwd",
                "/w/elo/main", "--", "claude", "--name", "tigerstyle: main", "/tigerstyle",
            ]
        );
        let somewhere = There { session: "elo".to_owned(), tab: None, back: None };
        assert_eq!(
            new_pane(&somewhere, "/w/elo", &command)[..8],
            ["zellij", "--session", "elo", "action", "new-pane", "--cwd", "/w/elo", "--"]
        );
        assert_eq!(opened(b"terminal_12\n"), Some(12));
        assert_eq!(opened(b"plugin_3\n"), None);
        assert_eq!(opened(b""), None);
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
