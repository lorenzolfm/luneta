//! The menu: a key, then another, and an agent is at work, as which-key shows
//! the keys in neovim.
//!
//! `mode "menu"` makes the plugin the menu, floating at the bottom right of the
//! tab you are in. Its `menu` holds its entries: a key each, and under a key
//! that opens a group, the keys of the group. The menu shows the keys of the
//! group open and what each does. A key with nothing under it starts Claude
//! Code on its prompt, in a pane of its own beside an agent, in the directory
//! the agent works in: the pane you came from, or the row the sidebar had
//! highlighted when `m` opened the menu.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

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

/// What a menu the sidebar opens with no entries says: the sidebar has the
/// keys of the dock that opened it, and a dock loaded by its location has
/// none of the alias's.
const NO_SIDEBAR_ENTRIES: &str =
    "menu: the sidebar has no keys to show; load the dock by the alias that has your menu";

/// What a menu says that has no pane to start an agent beside: the pane you
/// opened it from is no terminal, as the sidebar is not.
const NO_PANE: &str = "menu: the pane you are in is no terminal to start an agent beside; in the \
                       sidebar, m opens the menu for the row you are on";

/// What a key with nothing under it starts.
const CLAUDE: &str = "claude";

/// What the sidebar hands the menu it opens, in the menu's configuration: the
/// pane an agent starts beside, the directory it works in, and when that is
/// another session, the session, the tab, and the pane to hand the focus back
/// to before going there.
const BESIDE_PANE: &str = "beside_pane";
const BESIDE_CWD: &str = "beside_cwd";
const BESIDE_SESSION: &str = "beside_session";
const BESIDE_TAB: &str = "beside_tab";
const BACK_PANE: &str = "back_pane";

/// What the menu asks of the host, as the context of a command it runs.
const CONTEXT_KEY: &str = "luneta";
const BRANCH_VALUE: &str = "branch";
const START_VALUE: &str = "start";

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
    /// Opened a group.
    Opened,
    /// Starts an agent on the prompt.
    Start(String),
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

    pub fn press(&mut self, key: char) -> Pressed {
        let entries = self.entries();
        let Some(at) = entries.iter().position(|entry| entry.key() == key) else {
            return Pressed::Nothing;
        };
        if let Entry::Start { prompt, .. } = &entries[at] {
            return Pressed::Start(prompt.clone());
        }
        self.open.push(at);
        Pressed::Opened
    }

    /// Closes the group open, and says whether there was one.
    pub fn back(&mut self) -> bool {
        self.open.pop().is_some()
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
pub struct Beside {
    pub pane: u32,
    pub cwd: String,
}

/// A session other than the menu's, and the tab in it, by its id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct There {
    pub session: String,
    pub tab: Option<usize>,
    /// The pane of the menu's own session you came from. zellij keeps where
    /// each session's focus was, and the sidebar that opened the menu has it:
    /// the session would have you back in the sidebar.
    pub back: Option<u32>,
}

/// Which pane the menu is for.
#[derive(Debug, PartialEq, Eq)]
enum For {
    /// The row the sidebar had highlighted, for the menu's whole life, in
    /// another session when `There` says so.
    Row(Beside, Option<There>),
    /// The pane you came from, as zellij last said: `Waiting` until it has,
    /// and `Failed` when what you came from is no terminal, or zellij cannot
    /// say where it works.
    Origin(Fetch<Beside>),
}

impl For {
    /// The pane an agent starts beside, and the session it is in when that is
    /// another, once the menu knows.
    fn ready(&self) -> Option<(&Beside, Option<&There>)> {
        match self {
            For::Row(beside, there) => Some((beside, there.as_ref())),
            For::Origin(Fetch::Ready(beside)) => Some((beside, None)),
            For::Origin(Fetch::Waiting | Fetch::Failed(_)) => None,
        }
    }
}

/// Where the menu stands.
#[derive(Default)]
enum Phase {
    /// The keys of the group open, pressed one after another, for an agent
    /// beside a pane.
    Choosing(Keys, For),
    /// Another session is opening the agent's pane. Keys do nothing until
    /// zellij says which it opened.
    Starting(Keys, Beside, There),
    /// What is wrong with the keys, with the pane the sidebar handed over, or
    /// with starting the agent. Any key closes the menu.
    Wrong(String),
    /// The menu asked zellij to close it, and a key already on its way does
    /// nothing.
    #[default]
    Done,
}

/// A row's pane as entries of the configuration of a menu, for the sidebar
/// to open one with: the pane, the directory it works in as far as the
/// sidebar knows, and when that is another session, the session, the tab,
/// and the pane to hand the focus back to before going there.
pub fn configuration(
    pane: u32,
    cwd: Option<&str>,
    there: Option<&There>,
) -> BTreeMap<String, String> {
    let mut configuration = BTreeMap::from([(BESIDE_PANE.to_owned(), pane.to_string())]);
    if let Some(cwd) = cwd {
        configuration.insert(BESIDE_CWD.to_owned(), cwd.to_owned());
    }
    if let Some(there) = there {
        configuration.insert(BESIDE_SESSION.to_owned(), there.session.clone());
        if let Some(tab) = there.tab {
            configuration.insert(BESIDE_TAB.to_owned(), tab.to_string());
        }
        if let Some(back) = there.back {
            configuration.insert(BACK_PANE.to_owned(), back.to_string());
        }
    }
    configuration
}

/// The row the sidebar handed over in the menu's configuration: none for a
/// menu a key opened, and what is wrong when the sidebar could not say where
/// the row's pane works.
fn handed(
    configuration: &BTreeMap<String, String>,
) -> Result<Option<(Beside, Option<There>)>, String> {
    let Some(pane) = number(configuration, BESIDE_PANE)? else {
        return Ok(None);
    };
    let Some(cwd) = configuration.get(BESIDE_CWD) else {
        return Err(format!(
            "menu: the sidebar does not know yet where pane {pane} works; try m again in a \
             moment"
        ));
    };
    let there = match configuration.get(BESIDE_SESSION) {
        Some(session) => Some(There {
            session: session.clone(),
            tab: number(configuration, BESIDE_TAB)?,
            back: number(configuration, BACK_PANE)?,
        }),
        None => None,
    };
    Ok(Some((Beside { pane, cwd: cwd.clone() }, there)))
}

/// The number `key` holds in the configuration, if it holds one.
fn number<T: FromStr>(
    configuration: &BTreeMap<String, String>,
    key: &str,
) -> Result<Option<T>, String> {
    let Some(value) = configuration.get(key) else {
        return Ok(None);
    };
    value.parse().map(Some).map_err(|_| format!("menu: {key} {value:?} is no number"))
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
        let handed = handed(configuration);
        let keys = match configuration.get(ENTRIES) {
            Some(text) => Keys::parse(text),
            None if !matches!(handed, Ok(None)) => Err(NO_SIDEBAR_ENTRIES.to_owned()),
            None => Err(NO_ENTRIES.to_owned()),
        };
        let phase = match (keys, handed) {
            (Err(reason), _) | (Ok(_), Err(reason)) => Phase::Wrong(reason),
            (Ok(keys), Ok(Some((beside, there)))) => Phase::Choosing(keys, For::Row(beside, there)),
            (Ok(keys), Ok(None)) => Phase::Choosing(keys, For::Origin(Fetch::Waiting)),
        };
        if let Phase::Wrong(reason) = &phase {
            eprintln!("luneta {reason}");
        }
        Menu { phase, plugin_id, panes: None, tabs: Vec::new(), asked: None, placed: None }
    }

    /// The entries, or what is wrong with them, with the pane the menu is
    /// for, or with starting an agent. Nothing once the menu asked zellij to
    /// close it.
    pub fn keys(&self) -> Option<Result<&Keys, &str>> {
        match &self.phase {
            Phase::Wrong(reason) | Phase::Choosing(_, For::Origin(Fetch::Failed(reason))) => {
                Some(Err(reason))
            },
            Phase::Choosing(keys, _) | Phase::Starting(keys, ..) => Some(Ok(keys)),
            Phase::Done => None,
        }
    }

    /// The directory an agent the menu starts works in, once the menu knows.
    pub fn cwd(&self) -> Option<&str> {
        match &self.phase {
            Phase::Choosing(_, beside) => beside.ready().map(|(beside, _)| beside.cwd.as_str()),
            Phase::Starting(_, beside, _) => Some(&beside.cwd),
            Phase::Wrong(_) | Phase::Done => None,
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
            Phase::Starting(..) | Phase::Done => return false,
            Phase::Wrong(_) | Phase::Choosing(_, For::Origin(Fetch::Failed(_))) => {
                return self.close();
            },
            Phase::Choosing(keys, _) => keys,
        };
        match key.bare_key {
            BareKey::Char('c') if key.has_modifiers(&[KeyModifier::Ctrl]) => self.close(),
            BareKey::Esc if key.has_no_modifiers() => self.close(),
            BareKey::Backspace if key.has_no_modifiers() => {
                if !keys.back() {
                    return self.close();
                }
                self.place();
                true
            },
            BareKey::Char(c) if key.has_no_modifiers() => match keys.press(c) {
                Pressed::Opened => {
                    self.place();
                    true
                },
                Pressed::Start(prompt) => self.start(&prompt),
                Pressed::Nothing => false,
            },
            _ => false,
        }
    }

    /// Asks zellij to close the menu, which takes no key from then on. Says
    /// whether to draw.
    fn close(&mut self) -> bool {
        self.phase = Phase::Done;
        close_self();
        false
    }

    /// What came of a command the menu ran. Says whether to draw.
    pub fn ran(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        match context.get(CONTEXT_KEY).map(String::as_str) {
            Some(BRANCH_VALUE) => {
                let Some((asked, branch)) = &mut self.asked else {
                    return false;
                };
                // What is checked out where an agent no longer starts.
                if context.get(DIR_KEY) != Some(&*asked) {
                    return false;
                }
                let said = String::from_utf8_lossy(stdout).trim().to_owned();
                *branch = (exit_code == Some(0) && !said.is_empty()).then_some(said);
                false
            },
            Some(START_VALUE) => self.started(exit_code, stdout, stderr),
            _ => false,
        }
    }

    /// Goes to the session another agent started in, once zellij says it
    /// opened its pane, and to that pane when zellij says which. Says whether
    /// to draw.
    fn started(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> bool {
        let Phase::Starting(_, _, there) = &self.phase else {
            return false;
        };
        let Some(0) = exit_code else {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            let reason = if reason.is_empty() { "zellij opened no pane" } else { reason };
            self.phase = Phase::Wrong(format!("menu: the agent did not start: {reason}"));
            self.place();
            return true;
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
        self.close()
    }

    /// Starts Claude Code on `prompt` in a pane of its own, beside the pane
    /// the menu is for, in the directory that pane works in. In the menu's
    /// own session the menu closes first, the focus goes to that pane, across
    /// tabs, and the new pane opens beside it. Another session opens the pane
    /// itself, in that pane's tab, and the menu goes there once it has. A key
    /// pressed before zellij says which pane you came from starts nothing.
    /// Says whether to draw.
    fn start(&mut self, prompt: &str) -> bool {
        match std::mem::take(&mut self.phase) {
            Phase::Choosing(keys, For::Row(beside, Some(there))) => {
                let command = claude(prompt, self.branch(&beside.cwd));
                let new_pane = new_pane(&there, &beside.cwd, &command);
                let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
                run_command(
                    &new_pane,
                    BTreeMap::from([(CONTEXT_KEY.to_owned(), START_VALUE.to_owned())]),
                );
                self.phase = Phase::Starting(keys, beside, there);
            },
            Phase::Choosing(_, For::Row(beside, None) | For::Origin(Fetch::Ready(beside))) => {
                let command = claude(prompt, self.branch(&beside.cwd));
                close_self();
                focus_terminal_pane(beside.pane, false, false);
                open_command_pane(command_pane(&command, &beside.cwd), BTreeMap::new());
            },
            phase => self.phase = phase,
        }
        false
    }

    /// Keeps the pane you came from, and the directory it works in, as the
    /// panes and the tabs change, unless the sidebar handed one over, and
    /// asks git the branch checked out there. Says whether the pane changed.
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
        let Phase::Choosing(_, For::Origin(origin)) = &mut self.phase else {
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

    #[test]
    fn a_key_opens_its_group_and_a_key_in_it_starts_its_prompt() {
        let mut keys = keys(MENU);
        assert_eq!(shown(&keys), ['c', 'r']);
        assert!(keys.names().is_empty());

        assert_eq!(keys.press('x'), Pressed::Nothing);
        assert_eq!(keys.press('c'), Pressed::Opened);
        assert_eq!(shown(&keys), ['t', 'm', 's']);
        assert_eq!(keys.names(), ["Claude"]);
        // A key of the group before is not one of this group's.
        assert_eq!(keys.press('r'), Pressed::Nothing);
        assert_eq!(keys.press('s'), Pressed::Start("/tigerstyle".to_owned()));

        assert!(keys.back());
        assert_eq!(shown(&keys), ['c', 'r']);
        assert!(!keys.back());
        assert_eq!(keys.press('r'), Pressed::Start("review the diff against main".to_owned()));
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

    /// The pane the menu is for, while it shows its keys.
    fn beside_of(menu: &Menu) -> Option<&For> {
        match &menu.phase {
            Phase::Choosing(_, beside) => Some(beside),
            _ => None,
        }
    }

    /// The menu as the sidebar opens it for pane 2, working in `/w/elo`, of
    /// the menu's own session or of `there`.
    fn row_menu(there: Option<&There>) -> Menu {
        let mut configuration = configuration(2, Some("/w/elo"), there);
        configuration.insert(ENTRIES.to_owned(), MENU.to_owned());
        Menu::new(0, &configuration)
    }

    fn press(menu: &mut Menu, key: char) -> bool {
        menu.key(&KeyWithModifier::new(BareKey::Char(key)))
    }

    /// The sidebar opens a menu with the row it has highlighted, and the
    /// menu reads it back from its configuration as it was.
    #[test]
    fn the_pane_the_sidebar_hands_over_reaches_the_menu_whole() {
        let there = |tab, back| There { session: "elo".to_owned(), tab, back };
        let pane = |pane, cwd: &str| Beside { pane, cwd: cwd.to_owned() };
        for (beside, there) in [
            (pane(4, "/w/luneta/main"), None),
            (pane(2, "/w/elo"), Some(there(Some(3), Some(1)))),
            (pane(2, "/w/elo"), Some(there(None, None))),
        ] {
            let mut configuration = configuration(beside.pane, Some(&beside.cwd), there.as_ref());
            configuration.insert(ENTRIES.to_owned(), MENU.to_owned());
            let menu = Menu::new(0, &configuration);
            assert_eq!(beside_of(&menu), Some(&For::Row(beside, there)));
        }
        let bound = BTreeMap::from([(ENTRIES.to_owned(), MENU.to_owned())]);
        assert_eq!(beside_of(&Menu::new(0, &bound)), Some(&For::Origin(Fetch::Waiting)));
    }

    /// A row the sidebar does not know the directory of yet has no agent to
    /// start, and the menu says so rather than start one wherever zellij
    /// runs. A row it cannot read says what is wrong with it.
    #[test]
    fn a_row_the_menu_cannot_start_an_agent_beside_says_why() {
        let mut garbled = configuration(2, Some("/w/elo"), None);
        garbled.insert(BESIDE_SESSION.to_owned(), "elo".to_owned());
        garbled.insert(BESIDE_TAB.to_owned(), "third".to_owned());
        let unknown = configuration(2, None, None);
        for (mut configuration, reason) in [
            (unknown, "menu: the sidebar does not know yet where pane 2 works"),
            (garbled, "menu: beside_tab \"third\" is no number"),
        ] {
            configuration.insert(ENTRIES.to_owned(), MENU.to_owned());
            let said = Menu::new(0, &configuration).keys().and_then(Result::err).map(str::to_owned);
            assert!(said.as_deref().is_some_and(|said| said.starts_with(reason)), "{said:?}");
        }
    }

    /// The key binding opens the menu for the pane you are in, which zellij
    /// says a moment later. A key that starts an agent before then starts
    /// none, rather than one beside no pane, wherever zellij runs.
    #[test]
    fn a_key_pressed_before_zellij_says_where_you_are_starts_nothing() {
        let mut menu = Menu::new(0, &BTreeMap::from([(ENTRIES.to_owned(), MENU.to_owned())]));
        assert!(press(&mut menu, 'c'));
        assert!(!press(&mut menu, 't'));
        assert_eq!(beside_of(&menu), Some(&For::Origin(Fetch::Waiting)));
        assert_eq!(menu.keys().and_then(Result::ok).map(Keys::names), Some(vec!["Claude"]));
    }

    /// A menu that asked zellij to close it, as one that started an agent in
    /// your own session has, takes no key on its way before zellij has: a
    /// second press starts no second agent.
    #[test]
    fn a_menu_that_closed_takes_no_more_keys() {
        let mut menu = row_menu(None);
        assert!(!menu.key(&KeyWithModifier::new(BareKey::Esc)));
        assert!(menu.keys().is_none());
        // An agent started here would ask zellij for its pane, and no zellij
        // answers a test.
        assert!(!press(&mut menu, 'r'));
        assert!(menu.keys().is_none());
    }

    /// In another session, the menu takes no key while zellij opens the
    /// agent's pane, and goes there once it has, though zellij say not which
    /// pane it is. When zellij fails, the menu says what it said.
    #[test]
    fn an_agent_in_another_session_is_gone_to_once_its_pane_is_open() {
        let there = There { session: "elo".to_owned(), tab: Some(3), back: Some(1) };
        let started = BTreeMap::from([(CONTEXT_KEY.to_owned(), START_VALUE.to_owned())]);
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
            let mut menu = row_menu(Some(&there));
            assert!(!press(&mut menu, 'r'));
            assert!(!press(&mut menu, 'r'));
            assert!(matches!(menu.phase, Phase::Starting(..)));
            menu.ran(exit_code, stdout, stderr, &started);
            assert_eq!(menu.keys().map(Result::err), said.map(Some), "{exit_code:?} {stdout:?}");
        }
    }

    /// The agent is named for the branch checked out where it starts. An
    /// answer git gives late, for a directory asked about before, names it
    /// for none.
    #[test]
    fn a_late_git_answer_for_another_directory_names_no_agent() {
        let mut menu = row_menu(None);
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
    /// beside, and says so rather than start one wherever zellij puts it. The
    /// sidebar's `m` hands it the pane of a row.
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
        let bound = BTreeMap::from([(ENTRIES.to_owned(), MENU.to_owned())]);
        let mut handed = configuration(2, Some("/w/elo"), None);
        handed.insert(ENTRIES.to_owned(), MENU.to_owned());
        for (configuration, said) in [(bound, Some(NO_PANE)), (handed, None)] {
            let mut menu = Menu::new(9, &configuration);
            menu.panes = Some(PaneManifest { panes: HashMap::from([(0, tab.clone())]) });
            menu.tabs = vec![TabInfo { position: 0, active: true, ..TabInfo::default() }];
            menu.follow();
            assert_eq!(menu.keys().and_then(Result::err), said, "{configuration:?}");
        }
    }

    #[test]
    fn the_menu_stands_in_the_bottom_right_and_never_past_the_tab() {
        assert_eq!(corner(120, 40, 30, 7), Some((90, 33, 30, 7)));
        assert_eq!(corner(20, 5, 30, 7), Some((0, 0, 20, 5)));
        assert_eq!(corner(0, 0, 30, 7), None);
    }
}
