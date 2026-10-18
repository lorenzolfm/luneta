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

use kdl::{KdlDocument, KdlNode};
use zellij_tile::prelude::*;

use crate::render;
use crate::tree::origin;

/// What holds the entries in the menu's configuration: a node with children,
/// which zellij hands over as their KDL.
pub const ENTRIES: &str = "menu";

/// What a menu with no entries says.
const NO_ENTRIES: &str = "menu: no keys to show; give mode \"menu\" a menu { … } of them";

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
    pub cwd: Option<String>,
    /// Where the pane is when that is another session than the menu's.
    pub there: Option<There>,
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

impl Beside {
    /// The pane as entries of the configuration of a menu, for the sidebar to
    /// open one with.
    pub fn to_configuration(&self) -> BTreeMap<String, String> {
        let mut configuration = BTreeMap::from([(BESIDE_PANE.to_owned(), self.pane.to_string())]);
        if let Some(cwd) = &self.cwd {
            configuration.insert(BESIDE_CWD.to_owned(), cwd.clone());
        }
        if let Some(there) = &self.there {
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

    fn from_configuration(configuration: &BTreeMap<String, String>) -> Option<Self> {
        let pane = configuration.get(BESIDE_PANE)?.parse().ok()?;
        let there = configuration.get(BESIDE_SESSION).map(|session| There {
            session: session.clone(),
            tab: configuration.get(BESIDE_TAB).and_then(|tab| tab.parse().ok()),
            back: configuration.get(BACK_PANE).and_then(|back| back.parse().ok()),
        });
        Some(Beside { pane, cwd: configuration.get(BESIDE_CWD).cloned(), there })
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

/// A pane of the menu's own session that runs `command`, in `cwd`.
fn command_pane(command: &[String], cwd: Option<&str>) -> CommandToRun {
    let (path, args) =
        command.split_first().map_or((CLAUDE, &[][..]), |(path, args)| (path.as_str(), args));
    CommandToRun { path: PathBuf::from(path), args: args.to_vec(), cwd: cwd.map(PathBuf::from) }
}

/// Opens a pane that runs `command` in a tab of another session, in `cwd`.
/// zellij says which pane it opened, as `terminal_<id>`.
fn new_pane(there: &There, cwd: Option<&str>, command: &[String]) -> Vec<String> {
    let session = there.session.as_str();
    let mut new_pane: Vec<String> =
        ["zellij", "--session", session, "action", "new-pane"].map(str::to_owned).to_vec();
    if let Some(tab) = there.tab {
        new_pane.extend(["--tab-id".to_owned(), tab.to_string()]);
    }
    if let Some(cwd) = cwd {
        new_pane.extend(["--cwd".to_owned(), cwd.to_owned()]);
    }
    new_pane.push("--".to_owned());
    new_pane.extend(command.iter().cloned());
    new_pane
}

/// The terminal pane `zellij action new-pane` says it opened.
fn opened(stdout: &[u8]) -> Option<u32> {
    String::from_utf8_lossy(stdout).trim().strip_prefix("terminal_")?.parse().ok()
}

pub struct Menu {
    /// The entries, or what is wrong with them, or with starting an agent.
    keys: Result<Keys, String>,
    plugin_id: u32,
    panes: Option<PaneManifest>,
    tabs: Vec<TabInfo>,
    /// The pane an agent starts beside: the row the sidebar handed over, or
    /// else the pane you came from.
    beside: Option<Beside>,
    /// Whether the sidebar handed the pane over, which then stays whatever
    /// has the focus.
    handed: bool,
    /// The directory git was last asked the branch of.
    asked: Option<String>,
    /// The branch checked out where an agent starts, once git has said.
    branch: Option<String>,
    /// Whether an agent is starting in another session, for the menu to go
    /// there once zellij says which pane it opened.
    starting: bool,
    /// Where the menu last asked zellij to stand it.
    placed: Option<Spot>,
}

impl Menu {
    pub fn new(plugin_id: u32, configuration: &BTreeMap<String, String>) -> Self {
        let keys = match configuration.get(ENTRIES) {
            Some(text) => Keys::parse(text),
            None => Err(NO_ENTRIES.to_owned()),
        };
        if let Err(reason) = &keys {
            eprintln!("luneta {reason}");
        }
        let beside = Beside::from_configuration(configuration);
        Menu {
            keys,
            plugin_id,
            panes: None,
            tabs: Vec::new(),
            handed: beside.is_some(),
            beside,
            asked: None,
            branch: None,
            starting: false,
            placed: None,
        }
    }

    pub fn keys(&self) -> Result<&Keys, &str> {
        self.keys.as_ref().map_err(String::as_str)
    }

    pub fn cwd(&self) -> Option<&str> {
        self.beside.as_ref()?.cwd.as_deref()
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
        if self.starting {
            return false;
        }
        let Ok(keys) = &mut self.keys else {
            close_self();
            return false;
        };
        match key.bare_key {
            BareKey::Char('c') if key.has_modifiers(&[KeyModifier::Ctrl]) => close_self(),
            BareKey::Esc if key.has_no_modifiers() => close_self(),
            BareKey::Backspace if key.has_no_modifiers() => {
                if !keys.back() {
                    close_self();
                    return false;
                }
                self.place();
                return true;
            },
            BareKey::Char(c) if key.has_no_modifiers() => match keys.press(c) {
                Pressed::Opened => {
                    self.place();
                    return true;
                },
                Pressed::Start(prompt) => self.start(&prompt),
                Pressed::Nothing => {},
            },
            _ => {},
        }
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
                let branch = String::from_utf8_lossy(stdout).trim().to_owned();
                self.branch = (exit_code == Some(0) && !branch.is_empty()).then_some(branch);
                false
            },
            Some(START_VALUE) => {
                self.starting = false;
                let there = self.beside.as_ref().and_then(|beside| beside.there.as_ref());
                if let (Some(0), Some(pane), Some(there)) = (exit_code, opened(stdout), there) {
                    if let Some(back) = there.back {
                        focus_terminal_pane(back, false, false);
                    }
                    switch_session_with_focus(&there.session, None, Some((pane, false)));
                    close_self();
                    return false;
                }
                let reason = String::from_utf8_lossy(stderr);
                let reason = reason.lines().next().unwrap_or("").trim();
                let reason = if reason.is_empty() { "zellij opened no pane" } else { reason };
                self.keys = Err(format!("menu: the agent did not start: {reason}"));
                self.place();
                true
            },
            _ => false,
        }
    }

    /// Starts Claude Code on `prompt` in a pane of its own, beside the pane
    /// the menu was opened for, in the directory that pane works in. In the
    /// menu's own session the menu closes first, the focus goes to that pane,
    /// across tabs, and the new pane opens beside it. Another session opens
    /// the pane itself, in that pane's tab, and the menu goes there once it
    /// has.
    fn start(&mut self, prompt: &str) {
        let command = claude(prompt, self.branch.as_deref());
        let cwd = self.cwd();
        let Some(there) = self.beside.as_ref().and_then(|beside| beside.there.as_ref()) else {
            close_self();
            if let Some(beside) = &self.beside {
                focus_terminal_pane(beside.pane, false, false);
            }
            open_command_pane(command_pane(&command, cwd), BTreeMap::new());
            return;
        };
        let new_pane = new_pane(there, cwd, &command);
        let new_pane: Vec<&str> = new_pane.iter().map(String::as_str).collect();
        run_command(
            &new_pane,
            BTreeMap::from([(CONTEXT_KEY.to_owned(), START_VALUE.to_owned())]),
        );
        self.starting = true;
    }

    /// Keeps the pane you came from, and the directory it works in, as the
    /// panes and the tabs change, unless the sidebar handed one over, and
    /// asks git the branch checked out there. Says whether the pane changed.
    fn follow(&mut self) -> bool {
        let followed = !self.handed && self.follow_origin();
        if let Some(cwd) = self.cwd().filter(|cwd| self.asked.as_deref() != Some(*cwd)) {
            let cwd = cwd.to_owned();
            self.branch = None;
            run_command(
                &["git", "-C", &cwd, "branch", "--show-current"],
                BTreeMap::from([(CONTEXT_KEY.to_owned(), BRANCH_VALUE.to_owned())]),
            );
            self.asked = Some(cwd);
        }
        followed
    }

    fn follow_origin(&mut self) -> bool {
        let Some(pane) = self.origin_pane() else {
            return false;
        };
        if self.beside.as_ref().is_some_and(|beside| beside.pane == pane) {
            return false;
        }
        let cwd = get_pane_cwd(PaneId::Terminal(pane)).ok();
        let cwd = cwd.map(|cwd| cwd.to_string_lossy().into_owned());
        self.beside = Some(Beside { pane, cwd, there: None });
        true
    }

    fn origin_pane(&self) -> Option<u32> {
        let tab = self.tabs.iter().find(|tab| tab.active)?;
        origin(self.panes.as_ref()?.panes.get(&tab.position)?)
    }

    /// Stands the menu at the bottom right of the tab, as big as the group
    /// open needs.
    fn place(&mut self) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.active) else {
            return;
        };
        let (width, height) = render::menu_size(self);
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

    /// The sidebar opens a menu with the row it has highlighted, and the
    /// menu reads it back from its configuration as it was.
    #[test]
    fn the_pane_the_sidebar_hands_over_reaches_the_menu_whole() {
        let there = |tab, back| Some(There { session: "elo".to_owned(), tab, back });
        for beside in [
            Beside { pane: 4, cwd: Some("/w/luneta/main".to_owned()), there: None },
            Beside { pane: 0, cwd: None, there: None },
            Beside { pane: 2, cwd: Some("/w/elo".to_owned()), there: there(Some(3), Some(1)) },
            Beside { pane: 2, cwd: None, there: there(None, None) },
        ] {
            let mut configuration = beside.to_configuration();
            configuration.insert(ENTRIES.to_owned(), MENU.to_owned());
            let menu = Menu::new(0, &configuration);
            assert_eq!(menu.beside.as_ref(), Some(&beside));
            assert!(menu.handed);
        }
        assert_eq!(Menu::new(0, &BTreeMap::new()).beside, None);
    }

    /// In another session, zellij opens the pane in the agent's tab and its
    /// directory, and says which pane it opened, for the menu to go there.
    #[test]
    fn an_agent_in_another_session_starts_in_its_tab_and_its_directory() {
        let command = claude("/tigerstyle", Some("main"));
        let there = There { session: "elo".to_owned(), tab: Some(3), back: None };
        assert_eq!(
            new_pane(&there, Some("/w/elo/main"), &command),
            [
                "zellij", "--session", "elo", "action", "new-pane", "--tab-id", "3", "--cwd",
                "/w/elo/main", "--", "claude", "--name", "tigerstyle: main", "/tigerstyle",
            ]
        );
        let somewhere = There { session: "elo".to_owned(), tab: None, back: None };
        assert_eq!(
            new_pane(&somewhere, None, &command)[..7],
            ["zellij", "--session", "elo", "action", "new-pane", "--", "claude"]
        );
        assert_eq!(opened(b"terminal_12\n"), Some(12));
        assert_eq!(opened(b"plugin_3\n"), None);
        assert_eq!(opened(b""), None);
    }

    #[test]
    fn the_menu_stands_in_the_bottom_right_and_never_past_the_tab() {
        assert_eq!(corner(120, 40, 30, 7), Some((90, 33, 30, 7)));
        assert_eq!(corner(20, 5, 30, 7), Some((0, 0, 20, 5)));
        assert_eq!(corner(0, 0, 30, 7), None);
    }
}
