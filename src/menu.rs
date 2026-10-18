//! The menu: a key, then another, and an agent is at work, as which-key shows
//! the keys in neovim.
//!
//! Its `menu` holds its entries: a key each, and under a key that opens a
//! group, the keys of the group. The menu shows the keys of the group open and
//! what each does. A key with nothing under it starts Claude Code on its
//! prompt, in a pane of its own beside a pane, in the directory that pane
//! works in. In the sidebar, a key of the menu opens it at the bottom of the
//! sidebar, for the pane whose row is highlighted. `mode "menu"` makes the
//! plugin the menu for the pane you came from, floating at the bottom right of
//! the tab you are in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::*;

use crate::checkouts::DIR_KEY;
use crate::fetch::Fetch;
use crate::render;
use crate::tree::origin;

/// What holds the entries in the menu's configuration: a node with children,
/// which zellij hands over as their KDL.
pub const ENTRIES: &str = "menu";

/// What a menu with no entries says.
const NO_ENTRIES: &str = "menu: no keys to show; give mode \"menu\" a menu { … } of them";

/// What the sidebar's menu says with no entries: the sidebar has the keys of
/// the dock that opened it, and a dock loaded by its location has none of the
/// alias's.
const NO_SIDEBAR_ENTRIES: &str =
    "menu: the sidebar has no keys to show; load the dock by the alias that has your menu";

/// What a menu says that has no pane to start an agent beside: the pane you
/// opened it from is no terminal, as the sidebar is not.
const NO_PANE: &str = "menu: the pane you are in is no terminal to start an agent beside; in the \
                       sidebar, the menu's keys open it for the row you are on";

/// What a key with nothing under it starts.
const CLAUDE: &str = "claude";

/// What the menu asks of the host, as the context of a command it runs.
const CONTEXT_KEY: &str = "luneta";
const BRANCH_VALUE: &str = "branch";
/// The sidebar's menu opening an agent's pane in another session, and the
/// number of the menu that asked.
pub const START_VALUE: &str = "start";
const MENU_KEY: &str = "luneta_menu";

/// Where a floating pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

pub enum Entry {
    /// A key that opens a group, the name the menu shows for it, and the keys
    /// in it.
    Group { key: char, name: String, entries: Vec<Entry> },
    /// A key that starts an agent on a prompt.
    Start { key: char, prompt: String },
}

impl Entry {
    pub fn key(&self) -> char {
        match self {
            Entry::Group { key, .. } | Entry::Start { key, .. } => *key,
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
    /// The entries, from the KDL zellij hands over: a node for each key, its
    /// one string the prompt the key starts an agent on, or the name of the
    /// group its children are.
    pub fn parse(text: &str) -> Result<Self, String> {
        let document: KdlDocument = text.parse().map_err(|error| format!("menu: {error}"))?;
        Ok(Keys { root: entries(&document, "")?, open: Vec::new() })
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

    /// What `key` does: a key the group open shows opens its group or starts
    /// its agent, `Backspace` goes back a group, and out of the menu from the
    /// first, and `Esc` closes it.
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
        if let Entry::Start { prompt, .. } = &entries[at] {
            return Pressed::Start(prompt.clone());
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

/// The entries of `document`, the children of the group `path` names, as the
/// keys pressed to open it.
fn entries(document: &KdlDocument, path: &str) -> Result<Vec<Entry>, String> {
    let mut entries: Vec<Entry> = Vec::new();
    for node in document.nodes() {
        let entry = entry(node, path)?;
        if entries.iter().any(|seen| seen.key() == entry.key()) {
            return Err(format!("menu: two entries for \"{path}{}\"", entry.key()));
        }
        entries.push(entry);
    }
    if entries.is_empty() {
        return Err(match path.trim_end() {
            "" => NO_ENTRIES.to_owned(),
            group => format!("menu: no keys under \"{group}\""),
        });
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

/// A session other than the menu's, and the tab in it, by its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct There {
    pub session: String,
    pub tab: Option<usize>,
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
        new_pane.extend(["--tab-id".to_owned(), tab.to_string()]);
    }
    new_pane.extend(["--cwd".to_owned(), cwd.to_owned(), "--".to_owned()]);
    new_pane.extend(command.iter().cloned());
    new_pane
}

/// The terminal pane `zellij action new-pane` says it opened.
fn opened(stdout: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(stdout).trim().strip_prefix("terminal_")?.parse().ok()
}

/// A row of the sidebar, for its menu: the pane an agent starts beside, the
/// directory it works in and the branch checked out there, as far as the
/// sidebar knows, and when it is in another session, the session, its tab,
/// and the pane to hand the focus back to before going there.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub pane: u32,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub there: Option<There>,
}

/// Where an agent the sidebar's menu starts: beside a pane, on the branch
/// checked out where it works, in the sidebar's own session or `there`.
struct Target {
    beside: Beside,
    branch: Option<String>,
    there: Option<There>,
}

impl Target {
    /// Starts Claude Code on `prompt` beside the pane, for the menu numbered
    /// `menu`, at `frame`. In the sidebar's own session the pane opens at
    /// once. Another session opens it itself, and the menu, with `keys` on
    /// show, waits for zellij to say it has.
    fn start(self, keys: Keys, prompt: &str, menu: u64, frame: u64) -> Stage {
        let command = claude(prompt, self.branch.as_deref());
        let Some(there) = self.there else {
            start_here(&command, &self.beside);
            return Stage::Started(frame);
        };
        let new_pane = new_pane(&there, &self.beside.cwd, &command);
        let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
        run_command(
            &new_pane,
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), START_VALUE.to_owned()),
                (MENU_KEY.to_owned(), menu.to_string()),
            ]),
        );
        Stage::Starting(keys, self.beside, there)
    }
}

/// The menu at the bottom of the sidebar, for the pane whose row was
/// highlighted when a key of the menu opened it. The sidebar keeps it while it
/// has the focus.
pub struct SidebarMenu {
    /// Its number among the menus the sidebar has opened, which the answer to
    /// an agent it starts in another session carries.
    number: u64,
    stage: Stage,
}

enum Stage {
    /// The keys of the group open, pressed one after another, for an agent
    /// beside a pane.
    Choosing(Keys, Target),
    /// Another session is opening the agent's pane. Keys do nothing until
    /// zellij says it has, but close the menu.
    Starting(Keys, Beside, There),
    /// The agent's pane opened in the sidebar's own session at this frame,
    /// and the focus is on its way there. A key that comes soon after was on
    /// its way already, and does nothing: a second press starts no second
    /// agent. Nothing shows.
    Started(u64),
    /// What is wrong with the keys, with the row, or with starting the agent.
    /// Any key closes the menu.
    Wrong(String),
}

impl SidebarMenu {
    /// The menu numbered `number` for `row`, from the `entries` of the
    /// sidebar's configuration, at `frame`: on the group `key` opens, or on
    /// the first with no key. A key that starts an agent starts it. None for
    /// a key the menu has not.
    pub fn open(
        entries: Option<&str>,
        row: Row,
        key: Option<char>,
        number: u64,
        frame: u64,
    ) -> Option<Self> {
        let menu = |stage| Some(SidebarMenu { number, stage });
        let mut keys = match entries.map(Keys::parse) {
            Some(Ok(keys)) => keys,
            Some(Err(reason)) => return menu(wrong(reason)),
            None if key.is_none() => return menu(wrong(NO_SIDEBAR_ENTRIES.to_owned())),
            None => return None,
        };
        let prompt = match key.map_or(Pressed::Moved, |key| keys.press(key)) {
            Pressed::Moved => None,
            Pressed::Start(prompt) => Some(prompt),
            Pressed::Close | Pressed::Nothing => return None,
        };
        let Some(cwd) = row.cwd else {
            return menu(wrong(format!(
                "menu: the sidebar does not know yet where pane {} works; try again in a moment",
                row.pane
            )));
        };
        let target =
            Target { beside: Beside { pane: row.pane, cwd }, branch: row.branch, there: row.there };
        match prompt {
            Some(prompt) => menu(target.start(keys, &prompt, number, frame)),
            None => menu(Stage::Choosing(keys, target)),
        }
    }

    /// The entries, or what is wrong with them, with the row, or with
    /// starting an agent. Nothing once an agent started in the sidebar's own
    /// session.
    pub fn keys(&self) -> Option<Result<&Keys, &str>> {
        match &self.stage {
            Stage::Choosing(keys, _) | Stage::Starting(keys, ..) => Some(Ok(keys)),
            Stage::Wrong(reason) => Some(Err(reason)),
            Stage::Started(_) => None,
        }
    }

    /// The directory an agent the menu starts works in.
    pub fn cwd(&self) -> Option<&str> {
        match &self.stage {
            Stage::Choosing(_, Target { beside, .. }) | Stage::Starting(_, beside, _) => {
                Some(&beside.cwd)
            },
            Stage::Started(_) | Stage::Wrong(_) => None,
        }
    }

    /// The frame the menu started an agent at in the sidebar's own session,
    /// for the sidebar to say when the keys on their way then have all come.
    pub fn started_at(&self) -> Option<u64> {
        match self.stage {
            Stage::Started(at) => Some(at),
            Stage::Choosing(..) | Stage::Starting(..) | Stage::Wrong(_) => None,
        }
    }

    /// What `key` does to the menu at `frame`. None once it closed.
    pub fn key(self, key: &KeyWithModifier, frame: u64) -> Option<Self> {
        let number = self.number;
        let stage = match self.stage {
            Stage::Wrong(_) => return None,
            Stage::Starting(..) if closes(key) => return None,
            Stage::Starting(..) | Stage::Started(_) => return Some(self),
            Stage::Choosing(mut keys, target) => match keys.key(key) {
                Pressed::Close => return None,
                Pressed::Start(prompt) => target.start(keys, &prompt, number, frame),
                Pressed::Moved | Pressed::Nothing => Stage::Choosing(keys, target),
            },
        };
        Some(SidebarMenu { number, stage })
    }

    /// Goes to the session the agent started in, once zellij says it opened
    /// its pane, and to that pane when zellij says which. None once it has.
    /// An answer for another menu is from one closed before it came.
    pub fn started(
        self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> Option<Self> {
        let Stage::Starting(_, _, there) = &self.stage else {
            return Some(self);
        };
        if context.get(MENU_KEY) != Some(&self.number.to_string()) {
            return Some(self);
        }
        let Some(0) = exit_code else {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            let reason = if reason.is_empty() { "zellij opened no pane" } else { reason };
            let stage = wrong(format!("menu: the agent did not start: {reason}"));
            return Some(SidebarMenu { number: self.number, stage });
        };
        if let Some(back) = there.back {
            focus_terminal_pane(back, false, false);
        }
        match opened(stdout) {
            Some(pane) => switch_session_with_focus(&there.session, None, Some((pane, false))),
            // The pane is open, and zellij did not say which it is: the
            // session, as it stands.
            None => switch_session(Some(&there.session)),
        }
        None
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
    Choosing(Keys, Fetch<Beside>),
    /// What is wrong with the keys. Any key closes the menu.
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
    panes: Option<PaneManifest>,
    tabs: Vec<TabInfo>,
    /// The directory git was last asked the branch of, and the branch checked
    /// out there once git has said.
    asked: Option<(String, Option<String>)>,
    /// Where the menu last asked zellij to stand it.
    placed: Option<Spot>,
}

impl Menu {
    pub fn new(plugin_id: u32, configuration: &BTreeMap<String, String>) -> Self {
        let phase = match configuration.get(ENTRIES).map(|text| Keys::parse(text)) {
            Some(Ok(keys)) => Phase::Choosing(keys, Fetch::Waiting),
            Some(Err(reason)) => Phase::Wrong(reason),
            None => Phase::Wrong(NO_ENTRIES.to_owned()),
        };
        if let Phase::Wrong(reason) = &phase {
            eprintln!("luneta {reason}");
        }
        Menu { phase, plugin_id, panes: None, tabs: Vec::new(), asked: None, placed: None }
    }

    /// The entries, or what is wrong with them or with the pane you came
    /// from. Nothing once the menu asked zellij to close it.
    pub fn keys(&self) -> Option<Result<&Keys, &str>> {
        match &self.phase {
            Phase::Wrong(reason) | Phase::Choosing(_, Fetch::Failed(reason)) => Some(Err(reason)),
            Phase::Choosing(keys, _) => Some(Ok(keys)),
            Phase::Done => None,
        }
    }

    /// The directory an agent the menu starts works in, once the menu knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.phase {
            Phase::Choosing(_, Fetch::Ready(beside)) => Some(&beside.cwd),
            Phase::Choosing(..) | Phase::Wrong(_) | Phase::Done => None,
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
        let keys = match &mut self.phase {
            Phase::Done => return false,
            Phase::Wrong(_) | Phase::Choosing(_, Fetch::Failed(_)) => return self.close(),
            Phase::Choosing(keys, _) => keys,
        };
        match keys.key(key) {
            Pressed::Moved => {
                self.place();
                true
            },
            Pressed::Start(prompt) => self.start(&prompt),
            Pressed::Close => self.close(),
            Pressed::Nothing => false,
        }
    }

    /// Asks zellij to close the menu, which takes no key from then on. Says
    /// whether to draw.
    fn close(&mut self) -> bool {
        self.phase = Phase::Done;
        close_self();
        false
    }

    /// What git said is checked out where an agent starts.
    pub fn ran(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        context: &BTreeMap<String, String>,
    ) {
        if context.get(CONTEXT_KEY).map(String::as_str) != Some(BRANCH_VALUE) {
            return;
        }
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

    /// Starts Claude Code on `prompt` in a pane of its own, beside the pane
    /// you came from, in the directory that pane works in. The menu closes
    /// first, and the new pane opens beside that pane. A key pressed before
    /// zellij says which pane you came from starts nothing. Says whether to
    /// draw.
    fn start(&mut self, prompt: &str) -> bool {
        let Phase::Choosing(_, Fetch::Ready(beside)) = &self.phase else {
            return false;
        };
        let beside = beside.clone();
        let command = claude(prompt, self.branch(&beside.cwd));
        self.close();
        start_here(&command, &beside);
        false
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
        followed
    }

    fn follow_origin(&mut self) -> bool {
        let pane = match self.origin_pane() {
            Fetch::Waiting => return false,
            Fetch::Ready(pane) => Some(pane),
            Fetch::Failed(_) => None,
        };
        let Phase::Choosing(_, origin) = &mut self.phase else {
            return false;
        };
        if matches!(origin, Fetch::Ready(beside) if Some(beside.pane) == pane) {
            return false;
        }
        let now = match pane {
            None => Fetch::Failed(NO_PANE.to_owned()),
            Some(pane) => match get_pane_cwd(PaneId::Terminal(pane)) {
                Ok(cwd) => Fetch::Ready(Beside { pane, cwd: cwd.to_string_lossy().into_owned() }),
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
    /// tab you are in holds, and `Failed` when that is no terminal with the
    /// focus.
    fn origin_pane(&self) -> Fetch<u32> {
        let Some(tab) = self.tabs.iter().find(|tab| tab.active) else {
            return Fetch::Waiting;
        };
        let Some(panes) = self.panes.as_ref().and_then(|manifest| manifest.panes.get(&tab.position))
        else {
            return Fetch::Waiting;
        };
        origin(panes).map_or_else(|| Fetch::Failed(NO_PANE.to_owned()), Fetch::Ready)
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
        Keys::parse(text).expect("a menu")
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
        assert_eq!(shown(&keys), ['c', 'r']);
        assert!(keys.names().is_empty());

        assert_eq!(keys.key(&char('x')), Pressed::Nothing);
        assert_eq!(keys.key(&char('c')), Pressed::Moved);
        assert_eq!(shown(&keys), ['t', 'm', 's']);
        assert_eq!(keys.names(), ["Claude"]);
        // A key of the group before is not one of this group's.
        assert_eq!(keys.key(&char('r')), Pressed::Nothing);
        assert_eq!(keys.key(&char('s')), Pressed::Start("/tigerstyle".to_owned()));

        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Moved);
        assert_eq!(shown(&keys), ['c', 'r']);
        assert_eq!(keys.key(&char('r')), Pressed::Start("review the diff against main".to_owned()));
        assert_eq!(keys.key(&bare(BareKey::Backspace)), Pressed::Close);
        keys.key(&char('c'));
        assert_eq!(keys.key(&bare(BareKey::Esc)), Pressed::Close);
        assert_eq!(keys.key(&char('c').with_ctrl_modifier()), Pressed::Close);
    }

    #[test]
    fn a_menu_that_cannot_be_read_says_which_key_is_wrong() {
        let one_string = "menu: \"t\" takes one string";
        for (text, reason) in [
            ("", NO_ENTRIES),
            ("// nothing but a comment", NO_ENTRIES),
            (r#"c "Claude" {}"#, "menu: no keys under \"c\""),
            (r#"c "Claude" { tn "/tigerstyle"; }"#, "menu: \"c tn\" is no key"),
            (r#"c "Claude" { t "/tigerstyle"; t "/a"; }"#, "menu: two entries for \"c t\""),
            ("t", one_string),
            (r#"t " ""#, one_string),
            (r#"t "/tigerstyle" "/thermo-nuclear-review""#, one_string),
            ("t 1", one_string),
            (r#"t prompt="/tigerstyle""#, one_string),
        ] {
            let error = Keys::parse(text).err().unwrap_or_else(|| panic!("{text:?} parsed"));
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

    /// Pane 2 of the sidebar, working in `/w/elo`, of the sidebar's own
    /// session or of `there`.
    fn row(there: Option<There>) -> Row {
        Row { pane: 2, cwd: Some("/w/elo".to_owned()), branch: Some("main".to_owned()), there }
    }

    /// The sidebar's menu numbered 1, opened at frame 0.
    fn open(entries: Option<&str>, row: Row, key: Option<char>) -> Option<SidebarMenu> {
        SidebarMenu::open(entries, row, key, 1, 0)
    }

    /// What the sidebar's menu shows: the keys of the group open, or what is
    /// wrong.
    fn on_show(menu: Option<&SidebarMenu>) -> Option<Result<Vec<char>, &str>> {
        menu.and_then(SidebarMenu::keys).map(|keys| keys.map(shown))
    }

    /// A key of the menu opens it in the sidebar on its group, and `m` on the
    /// first one; a key the menu has not is the sidebar's.
    #[test]
    fn a_key_of_the_menu_opens_it_on_its_group() {
        for (key, on) in [
            (Some('c'), Some(vec!['t', 'm', 's'])),
            (None, Some(vec!['c', 'r'])),
            (Some('x'), None),
        ] {
            let menu = open(Some(MENU), row(None), key);
            assert_eq!(on_show(menu.as_ref()), on.map(Ok), "{key:?}");
        }
        let menu = open(Some(MENU), row(None), Some('c')).expect("a menu");
        assert_eq!(menu.cwd(), Some("/w/elo"));
        let menu = menu.key(&bare(BareKey::Backspace), 0);
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r'])));
        let menu = menu.and_then(|menu| menu.key(&char('x'), 0));
        assert_eq!(on_show(menu.as_ref()), Some(Ok(vec!['c', 'r'])));
        assert!(menu.and_then(|menu| menu.key(&bare(BareKey::Esc), 0)).is_none());
    }

    /// A row the sidebar does not know the directory of yet has no agent to
    /// start, and the menu says so rather than start one wherever zellij
    /// runs. A sidebar with no menu says how to give it one when asked for
    /// it, and a menu that cannot be read says what is wrong with it for any
    /// key it might have had. Any key closes what is wrong.
    #[test]
    fn a_menu_that_cannot_start_an_agent_beside_its_row_says_why() {
        let unknown = || Row { cwd: None, ..row(None) };
        let where_unknown = "menu: the sidebar does not know yet where pane 2 works";
        for (entries, row, key, said) in [
            (Some(MENU), unknown(), Some('c'), Some(where_unknown)),
            (Some(MENU), unknown(), Some('x'), None),
            (None, row(None), None, Some(NO_SIDEBAR_ENTRIES)),
            (None, row(None), Some('c'), None),
            (Some("t"), row(None), Some('x'), Some("menu: \"t\" takes one string")),
        ] {
            let menu = open(entries, row, key);
            let wrong = menu.as_ref().and_then(SidebarMenu::keys).and_then(Result::err);
            assert_eq!(wrong.is_some(), said.is_some(), "{entries:?} {key:?}");
            if let (Some(wrong), Some(said)) = (wrong, said) {
                assert!(wrong.starts_with(said), "{wrong}");
            }
            assert!(menu.and_then(|menu| menu.key(&char('c'), 0)).is_none());
        }
    }

    /// In another session, the menu takes no key while zellij opens the
    /// agent's pane but the one that closes it, and goes there once it has,
    /// though zellij say not which pane it is. When zellij fails, the menu
    /// says what it said. An answer for another menu, as one that waited for
    /// the same session, is for a menu closed before it came.
    #[test]
    fn an_agent_in_another_session_is_gone_to_once_its_pane_is_open() {
        let there = There { session: "elo".to_owned(), tab: Some(3), back: Some(1) };
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
            let menu = SidebarMenu::open(Some(MENU), row(Some(there.clone())), Some('r'), 2, 0);
            let menu = menu.and_then(|menu| menu.key(&char('r'), 0)).expect("a menu");
            assert!(matches!(menu.stage, Stage::Starting(..)));
            let menu = menu.started(Some(0), b"terminal_7\n", none, &started(1));
            let menu = menu.expect("a menu");
            assert!(matches!(menu.stage, Stage::Starting(..)));
            let menu = menu.started(exit_code, stdout, stderr, &started(2));
            let wrong = menu.as_ref().map(|menu| menu.keys().and_then(Result::err));
            assert_eq!(wrong, said.map(Some), "{exit_code:?} {stdout:?}");
        }
        let menu = open(Some(MENU), row(Some(there)), Some('r'));
        assert!(menu.and_then(|menu| menu.key(&bare(BareKey::Esc), 0)).is_none());
    }

    /// Once the agent's pane opened in the sidebar's own session, the menu
    /// shows nothing, and takes a key that comes before the focus has gone to
    /// that pane, `r` and `Esc` alike: a second press starts no second agent.
    /// zellij answers the pane opened on stdin, which no test has, so the
    /// menu stands here as starting one leaves it.
    #[test]
    fn a_key_on_its_way_after_an_agent_starts_here_does_nothing() {
        let mut menu = SidebarMenu { number: 1, stage: Stage::Started(10) };
        assert!(menu.keys().is_none());
        assert_eq!(menu.cwd(), None);
        for key in [char('r'), bare(BareKey::Esc)] {
            menu = menu.key(&key, 11).expect("a menu");
            assert_eq!(menu.started_at(), Some(10));
        }
        let choosing = open(Some(MENU), row(None), Some('c')).expect("a menu");
        assert_eq!(choosing.started_at(), None);
    }

    /// The menu the key binding opens, floating, for the pane you came from.
    fn bound() -> Menu {
        Menu::new(0, &BTreeMap::from([(ENTRIES.to_owned(), MENU.to_owned())]))
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
        assert_eq!(menu.keys().and_then(Result::ok).map(Keys::names), Some(vec!["Claude"]));
    }

    /// A menu that asked zellij to close it, as one that started an agent
    /// has, takes no key on its way before zellij has: a second press starts
    /// no second agent.
    #[test]
    fn a_menu_that_closed_takes_no_more_keys() {
        let mut menu = bound();
        assert!(!menu.key(&bare(BareKey::Esc)));
        assert!(menu.keys().is_none());
        assert!(!menu.key(&char('r')));
        assert!(menu.keys().is_none());
    }

    /// The agent is named for the branch checked out where it starts. An
    /// answer git gives late, for a directory asked about before, names it
    /// for none.
    #[test]
    fn a_late_git_answer_for_another_directory_names_no_agent() {
        let mut menu = bound();
        let elo = Beside { pane: 2, cwd: "/w/elo".to_owned() };
        menu.phase = Phase::Choosing(keys(MENU), Fetch::Ready(elo));
        menu.follow();
        let branch = |dir: &str| {
            BTreeMap::from([
                (CONTEXT_KEY.to_owned(), BRANCH_VALUE.to_owned()),
                (DIR_KEY.to_owned(), dir.to_owned()),
            ])
        };
        menu.ran(Some(0), b"main\n", &branch("/w/old"));
        assert_eq!(menu.branch("/w/elo"), None);
        menu.ran(Some(0), b"feat/menu\n", &branch("/w/elo"));
        assert_eq!(menu.branch("/w/elo"), Some("feat/menu"));
    }

    /// In another session, zellij opens the pane in the agent's tab and its
    /// directory, and says which pane it opened, for the menu to go there.
    #[test]
    fn an_agent_in_another_session_starts_in_its_tab_and_its_directory() {
        let command = claude("/tigerstyle", Some("main"));
        let there = There { session: "elo".to_owned(), tab: Some(3), back: None };
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
        let tab = vec![plugin(1, false), shell, plugin(9, true)];
        let mut menu = bound();
        menu.panes = Some(PaneManifest { panes: HashMap::from([(0, tab)]) });
        menu.tabs = vec![TabInfo { position: 0, active: true, ..TabInfo::default() }];
        menu.follow();
        assert_eq!(menu.keys().and_then(Result::err), Some(NO_PANE));
    }

    #[test]
    fn the_menu_stands_in_the_bottom_right_and_never_past_the_tab() {
        assert_eq!(corner(120, 40, 30, 7), Some((90, 33, 30, 7)));
        assert_eq!(corner(20, 5, 30, 7), Some((0, 0, 20, 5)));
        assert_eq!(corner(0, 0, 30, 7), None);
    }
}
