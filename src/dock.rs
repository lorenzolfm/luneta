//! The dock: one sidebar for a whole session, kept in the tab on screen.
//!
//! `mode "dock"` in `load_plugins` runs this in the background, one to a
//! session, where zellij tells it of every tab and every pane, on screen or
//! not. It opens the sidebar, a pane of its own plugin in `mode "sidebar"`,
//! into a placeholder: a column each tab keeps for it, a pane that runs
//! `tail -f /dev/null`. When another tab comes on screen, the sidebar hands
//! its tab back the placeholder and takes the new tab's in its place, so the
//! panes beside it never change size.
//!
//! `toggle` hides the sidebar and every placeholder, the panes beside them
//! taking their columns, and brings them back. Closing the sidebar hides it.
//!
//! zellij runs a dock for each client, and every dock sees the same panes.
//! Each follows its own client's tab switches, but only the dock of the client
//! that attached first hides, shows, opens or closes anything, so two docks
//! never pull the columns two ways.

use std::collections::{BTreeMap, BTreeSet};

use zellij_tile::prelude::*;

use crate::tree::{is_placeholder, PLACEHOLDER};

/// The title the sidebar gives its pane, by which the dock knows it.
pub const SIDEBAR_TITLE: &str = "luneta sidebar";

/// How many updates zellij may take to show what the dock asked for before
/// the dock asks again.
const PATIENCE: u32 = 20;

/// Where a pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

#[derive(Default)]
pub struct Dock {
    plugin_id: u32,
    /// The client this dock answers to: zellij runs a dock for each.
    client: ClientId,
    /// The sidebar's configuration: the dock's own, in `mode "sidebar"`.
    sidebar_configuration: BTreeMap<String, String>,
    granted: bool,
    panes: Option<PaneManifest>,
    tabs: Vec<TabInfo>,
    want: Want,
    /// Whether the dock missed how the panes came to be: one that just
    /// started, for a client that just attached, or one whose client was away
    /// when a key came. It reads what the sidebar is to be off the panes.
    behind: bool,
    /// The sidebar's pane, from the moment the dock opens it.
    sidebar: Option<u32>,
    /// The tab, by id, the sidebar was last seen in.
    stood: Option<usize>,
    /// The tab, by id, this client last had the sidebar brought to. zellij
    /// runs a dock for each client, and each follows only its own, so two
    /// clients on two tabs do not take the sidebar from each other forever.
    placed: Option<usize>,
    /// What the dock has asked for and the panes do not show yet, and how many
    /// updates ago. An update can show the panes from before, or from halfway.
    asked: BTreeMap<Act, u32>,
    /// Where each placeholder belongs: where it last stood in its column, or
    /// where the sidebar covering it stands. Every dock sees the same updates,
    /// and so finds the same placeholders out of place.
    homes: BTreeMap<u32, Home>,
    /// The placeholder of a sidebar closed by hand. Closing it applied the
    /// tab's swap layout, which handed its column to a terminal; the
    /// placeholder takes the column back, and then hides as any other does.
    reclaim: Option<u32>,
    /// Tabs, by id, whose placeholder came back somewhere else. The tab's swap
    /// layout puts things back when the tab comes on screen, the only time a
    /// dock can ask for it.
    unsettled: BTreeSet<usize>,
}

/// What the sidebar is to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Want {
    #[default]
    Shown,
    Hidden,
    /// Coming back from hidden, and taking the focus if `focus`. The dock that
    /// leads brings it back; the others wait to see it.
    Back { focus: bool },
}

/// Something the dock asks zellij for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Act {
    /// Hide a pane, the panes beside it taking its columns.
    Hide { pane: PaneId },
    /// Show a hidden placeholder again.
    Show { pane: u32 },
    /// Open a sidebar into a placeholder.
    Open { into: u32, tab: usize },
    /// Apply the swap layout of the tab on screen again, which puts a
    /// placeholder that came back elsewhere in its column.
    Restore { placeholder: u32, tab: usize },
    /// Bring the sidebar into a placeholder, giving the tab it leaves back the
    /// placeholder it covered there.
    Move { sidebar: u32, from: Option<u32>, into: u32, tab: usize },
    Focus { sidebar: u32 },
    Close { pane: PaneId },
}

/// The panes as the dock needs them.
#[derive(Debug, Default)]
struct Scene {
    active: usize,
    /// The tab on screen, by id.
    active_id: usize,
    /// Tab ids by position.
    ids: BTreeMap<usize, usize>,
    /// Every pane that is a sidebar, by tab position.
    sidebars: Vec<(usize, Seen)>,
    /// Every placeholder, by tab position.
    placeholders: BTreeMap<usize, Vec<Seen>>,
    /// The url the dock was loaded from, for the sidebar.
    url: Option<String>,
    /// The other clients in the session.
    others: BTreeSet<ClientId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    id: u32,
    hidden: bool,
    floating: bool,
    spot: Spot,
}

/// Where a placeholder belongs, and whether it was hidden the update before.
#[derive(Clone, Copy, Debug)]
struct Home {
    spot: Spot,
    hidden: bool,
}

impl Seen {
    fn new(pane: &PaneInfo) -> Self {
        Seen {
            id: pane.id,
            hidden: pane.is_suppressed,
            floating: pane.is_floating,
            spot: spot(pane),
        }
    }
}

impl Scene {
    fn new(
        manifest: &PaneManifest,
        tabs: &[TabInfo],
        plugin_id: u32,
        sidebar: Option<u32>,
    ) -> Option<Self> {
        let active = tabs.iter().find(|t| t.active)?;
        let mut scene = Scene {
            active: active.position,
            active_id: active.tab_id,
            ids: tabs.iter().map(|t| (t.position, t.tab_id)).collect(),
            others: tabs.iter().flat_map(|t| t.other_focused_clients.iter().copied()).collect(),
            ..Scene::default()
        };
        for (tab, panes) in &manifest.panes {
            for pane in panes {
                if pane.is_plugin && pane.id == plugin_id {
                    scene.url.clone_from(&pane.plugin_url);
                } else if pane.is_plugin
                    && (pane.title == SIDEBAR_TITLE || Some(pane.id) == sidebar)
                {
                    scene.sidebars.push((*tab, Seen::new(pane)));
                } else if is_placeholder(pane) {
                    scene.placeholders.entry(*tab).or_default().push(Seen::new(pane));
                }
            }
        }
        Some(scene)
    }

    /// The tab's placeholder: the one showing, or else the first.
    fn placeholder(&self, tab: usize) -> Option<Seen> {
        let placeholders = self.placeholders.get(&tab)?;
        placeholders.iter().find(|p| !p.hidden).or(placeholders.first()).copied()
    }

    /// Which sidebar stays, when there are two. Every client's dock picks the
    /// same one.
    fn sidebar(&self) -> Option<(usize, Seen)> {
        self.sidebars.iter().min_by_key(|(_, s)| s.id).copied()
    }

    /// The sidebar, when it stands in a column.
    fn home(&self) -> Option<(usize, Seen)> {
        self.sidebar().filter(|(_, s)| !s.hidden && !s.floating)
    }

    fn id(&self, tab: usize) -> usize {
        self.ids.get(&tab).copied().unwrap_or(tab)
    }

    /// The position of the tab with an id, while there is one.
    fn position(&self, id: usize) -> Option<usize> {
        self.ids.iter().find(|(_, tab)| **tab == id).map(|(position, _)| *position)
    }

    /// What the sidebar is to be, read off the panes: hidden when none of it
    /// shows, neither the sidebar nor a placeholder.
    fn resting(&self) -> Want {
        let sidebar = self.sidebar().is_some_and(|(_, s)| !s.hidden);
        let column = self.placeholders.values().flatten().any(|p| !p.hidden);
        if sidebar || column || self.placeholders.is_empty() {
            Want::Shown
        } else {
            Want::Hidden
        }
    }

    /// A second sidebar, and a second placeholder in a tab, to close: two
    /// docks can open one each, and closing a tab hands its hidden panes to
    /// the first tab.
    fn extras(&self) -> Vec<Act> {
        let mut acts = Vec::new();
        if let Some((_, kept)) = self.sidebar() {
            for (_, extra) in self.sidebars.iter().filter(|(_, s)| s.id != kept.id) {
                acts.push(Act::Close { pane: PaneId::Plugin(extra.id) });
            }
        }
        for (tab, placeholders) in &self.placeholders {
            let kept = self.placeholder(*tab).map(|p| p.id);
            for extra in placeholders.iter().filter(|p| Some(p.id) != kept) {
                acts.push(Act::Close { pane: PaneId::Terminal(extra.id) });
            }
        }
        acts
    }

    /// Whether the panes show what was asked for.
    fn shows(&self, act: Act) -> bool {
        let placeholder = |id| self.placeholders.values().flatten().find(|p| p.id == id);
        let sidebar = |id| self.sidebars.iter().find(|(_, s)| s.id == id).map(|(_, s)| s);
        match act {
            Act::Hide { pane: PaneId::Terminal(id) } => placeholder(id).is_none_or(|p| p.hidden),
            Act::Hide { pane: PaneId::Plugin(id) } => sidebar(id).is_none_or(|s| s.hidden),
            Act::Show { pane } => placeholder(pane).is_some_and(|p| !p.hidden),
            Act::Open { tab, .. } | Act::Move { tab, .. } => {
                self.home().is_some_and(|(at, _)| self.id(at) == tab)
            },
            Act::Focus { .. } | Act::Restore { .. } => true,
            Act::Close { pane: PaneId::Terminal(id) } => placeholder(id).is_none(),
            Act::Close { pane: PaneId::Plugin(id) } => sidebar(id).is_none(),
        }
    }
}

impl Dock {
    pub fn new(
        plugin_id: u32,
        client: ClientId,
        sidebar_configuration: BTreeMap<String, String>,
    ) -> Self {
        Dock { plugin_id, client, sidebar_configuration, behind: true, ..Dock::default() }
    }

    pub fn grant(&mut self) {
        self.granted = true;
        self.step();
    }

    pub fn tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
        self.step();
    }

    pub fn panes(&mut self, panes: PaneManifest) {
        self.panes = Some(panes);
        self.step();
    }

    /// `toggle` hides the sidebar or brings it back; `focus` brings a hidden
    /// one back and moves into it. The sidebar itself answers a `focus` while
    /// it shows.
    pub fn pipe(&mut self, payload: Option<&str>) {
        let want = match (payload, self.want) {
            (Some("toggle"), Want::Hidden) => Want::Back { focus: false },
            (Some("toggle"), _) => Want::Hidden,
            (Some("focus"), Want::Hidden) => Want::Back { focus: true },
            _ => return,
        };
        // zellij keeps a client's dock running after the client leaves, and
        // hands it every key still. It has seen none of the panes since, so it
        // leaves them be, and catches up if its client comes back.
        if self.granted && !attached() {
            self.behind = true;
            return;
        }
        self.want = want;
        self.asked.clear();
        self.step();
    }

    fn step(&mut self) {
        if !self.granted {
            return;
        }
        let (Some(manifest), false) = (self.panes.as_ref(), self.tabs.is_empty()) else {
            return;
        };
        let Some(scene) = Scene::new(manifest, &self.tabs, self.plugin_id, self.sidebar) else {
            return;
        };
        for act in self.next(&scene) {
            if self.act(act, &scene) {
                self.asked.insert(act, 0);
            }
        }
    }

    /// Takes in the panes, and says what to ask zellij for that the dock has
    /// not asked for already.
    fn next(&mut self, scene: &Scene) -> Vec<Act> {
        self.asked.retain(|act, waited| {
            *waited += 1;
            *waited < PATIENCE && !scene.shows(*act)
        });
        if self.behind {
            self.catch_up(scene);
        }
        self.observe(scene);
        let mut acts = self.plan(scene);
        self.reclaim = None;
        acts.retain(|act| !self.asked.contains_key(act));
        acts
    }

    /// Starts over from the panes as they are.
    fn catch_up(&mut self, scene: &Scene) {
        self.behind = false;
        self.want = scene.resting();
        self.asked.clear();
        self.homes.clear();
        self.unsettled.clear();
        self.stood = None;
    }

    /// Keeps track of the placeholders and of the sidebar: the one the dock
    /// opened, until zellij shows it, and one gone from a tab that is still
    /// there, closed by hand, which hides the rest. Every dock sees it go,
    /// whichever tab its client is on.
    fn observe(&mut self, scene: &Scene) {
        self.track(scene);
        if let Some((tab, sidebar)) = scene.sidebar() {
            self.sidebar = Some(sidebar.id);
            self.stood = Some(scene.id(tab));
            if let (Want::Back { focus }, Some(_)) = (self.want, scene.home()) {
                if !(focus && self.leads(scene)) {
                    self.want = Want::Shown;
                }
            }
            return;
        }
        if self.docking() {
            return;
        }
        let (Some(_), Some(stood)) = (self.sidebar.take(), self.stood.take()) else {
            return;
        };
        if self.want == Want::Hidden {
            return;
        }
        // Gone with its tab, it opens again.
        let Some(tab) = scene.position(stood) else {
            return;
        };
        self.want = Want::Hidden;
        self.reclaim = scene
            .placeholders
            .get(&tab)
            .filter(|placeholders| placeholders.iter().all(|p| p.hidden))
            .and_then(|placeholders| placeholders.first())
            .map(|p| p.id);
    }

    /// Where every placeholder belongs, as of these panes.
    fn track(&mut self, scene: &Scene) {
        let home = scene.home();
        let mut homes = BTreeMap::new();
        for tab in scene.placeholders.keys() {
            let Some(placeholder) = scene.placeholder(*tab) else {
                continue;
            };
            let covered = home.filter(|(at, _)| at == tab).map(|(_, sidebar)| sidebar.spot);
            let spot = self.belongs(placeholder, covered, scene.id(*tab));
            homes.insert(placeholder.id, Home { spot, hidden: placeholder.hidden });
        }
        self.homes = homes;
    }

    /// Where a placeholder belongs. One back from hidden where it belongs came
    /// back from under a sidebar that left; one back somewhere else leaves its
    /// tab unsettled until it is back in place.
    fn belongs(&mut self, placeholder: Seen, covered: Option<Spot>, tab: usize) -> Spot {
        let Some(was) = self.homes.get(&placeholder.id).copied() else {
            return covered.unwrap_or(placeholder.spot);
        };
        if placeholder.hidden {
            let settled = !self.unsettled.contains(&tab);
            return covered.filter(|_| settled).unwrap_or(was.spot);
        }
        if placeholder.spot == was.spot {
            self.unsettled.remove(&tab);
        } else if was.hidden {
            self.unsettled.insert(tab);
        }
        if self.unsettled.contains(&tab) {
            was.spot
        } else {
            placeholder.spot
        }
    }

    fn plan(&self, scene: &Scene) -> Vec<Act> {
        let leads = self.leads(scene);
        let mut acts = if leads { scene.extras() } else { Vec::new() };
        let sidebar = scene.sidebar();
        if self.want == Want::Hidden {
            if leads {
                self.hide(scene, &mut acts);
            }
            return acts;
        }
        let home = scene.home().map(|(tab, _)| tab);
        if leads {
            for tab in scene.placeholders.keys().filter(|tab| Some(**tab) != home) {
                if let Some(placeholder) = scene.placeholder(*tab).filter(|p| p.hidden) {
                    acts.push(Act::Show { pane: placeholder.id });
                }
            }
        }
        let into = scene.placeholder(scene.active).filter(|p| !p.hidden).map(|p| p.id);
        let tab = scene.active_id;
        if self.docking() {
            return acts;
        }
        if let Some(placeholder) = into.filter(|_| self.unsettled.contains(&tab)) {
            acts.push(Act::Restore { placeholder, tab });
        }
        match sidebar {
            None if leads => acts.extend(into.map(|into| Act::Open { into, tab })),
            None => {},
            Some((_, sidebar)) if sidebar.floating => {},
            Some((_, sidebar)) => {
                let away = home != Some(scene.active) && self.placed != Some(tab);
                let back = leads && matches!(self.want, Want::Back { .. });
                match into {
                    Some(into) if sidebar.hidden && back || !sidebar.hidden && away => {
                        let from = home.and_then(|home| scene.placeholder(home)).map(|p| p.id);
                        acts.push(Act::Move { sidebar: sidebar.id, from, into, tab });
                    },
                    _ if leads && !sidebar.hidden && self.want == (Want::Back { focus: true }) => {
                        acts.push(Act::Focus { sidebar: sidebar.id });
                    },
                    _ => {},
                }
            },
        }
        acts
    }

    /// Takes the placeholders and the sidebar out, the placeholder of one
    /// closed by hand back in its column first.
    fn hide(&self, scene: &Scene, acts: &mut Vec<Act>) {
        if let Some(pane) = self.reclaim {
            acts.push(Act::Show { pane });
            acts.push(Act::Hide { pane: PaneId::Terminal(pane) });
        }
        for tab in scene.placeholders.keys() {
            if let Some(placeholder) = scene.placeholder(*tab).filter(|p| !p.hidden) {
                acts.push(Act::Hide { pane: PaneId::Terminal(placeholder.id) });
            }
        }
        if let Some((_, sidebar)) = scene.sidebar().filter(|(_, s)| !s.hidden) {
            acts.push(Act::Hide { pane: PaneId::Plugin(sidebar.id) });
        }
    }

    /// Whether this dock's client attached first. A key reaches every
    /// client's dock, with no word of whose it was, and the sidebar coming
    /// back from hidden goes to the tab of the first.
    fn leads(&self, scene: &Scene) -> bool {
        scene.others.iter().all(|other| *other > self.client)
    }

    /// Whether a sidebar is on its way into a placeholder. Halfway, it is
    /// hidden, and its old tab has its placeholder back.
    fn docking(&self) -> bool {
        self.asked.keys().any(|act| matches!(act, Act::Open { .. } | Act::Move { .. }))
    }

    /// Asks zellij for the act, and says whether it did. The sidebar goes
    /// only into a placeholder that still shows, since zellij drops a pane it
    /// is to put in place of a hidden one, and another dock may have hidden
    /// it since these panes.
    fn act(&mut self, act: Act, scene: &Scene) -> bool {
        match act {
            Act::Hide { pane } => {
                mark(pane);
                hide_pane_with_id(pane);
            },
            Act::Show { pane } => show_pane_with_id(PaneId::Terminal(pane), false, false),
            Act::Open { into, tab } => {
                let Some(url) = scene.url.as_deref().filter(|_| showing(into)) else {
                    return false;
                };
                let configuration = self.sidebar_configuration.clone();
                let opened = open_plugin_pane_floating(url, configuration, None, BTreeMap::new());
                if let Some(PaneId::Plugin(sidebar)) = opened {
                    self.sidebar = Some(sidebar);
                    self.settle(sidebar, into, tab);
                }
            },
            Act::Move { sidebar, from, into, tab } => {
                if !showing(into) {
                    return false;
                }
                let id = PaneId::Plugin(sidebar);
                let leaving = !scene.sidebar().is_some_and(|(_, s)| s.hidden);
                match from {
                    Some(placeholder) if leaving => {
                        replace_pane_with_existing_pane(id, PaneId::Terminal(placeholder), true);
                    },
                    None if leaving => {
                        let [program, args @ ..] = PLACEHOLDER;
                        let command = CommandToRun::new_with_args(program, args.to_vec());
                        open_command_pane_in_place_of_pane_id(id, command, false, BTreeMap::new());
                    },
                    _ => {},
                }
                self.settle(sidebar, into, tab);
            },
            Act::Restore { placeholder, tab } => {
                self.unsettled.remove(&tab);
                mark(PaneId::Terminal(placeholder));
                next_swap_layout();
            },
            Act::Focus { sidebar } => {
                self.want = Want::Shown;
                focus_plugin_pane(sidebar, false, false);
            },
            Act::Close { pane: PaneId::Plugin(id) } => close_plugin_pane(id),
            Act::Close { pane: PaneId::Terminal(id) } => close_terminal_pane(id),
        }
        true
    }

    /// Puts the sidebar in the placeholder of the tab on screen, back in its
    /// column if the placeholder had come back elsewhere.
    fn settle(&mut self, sidebar: u32, into: u32, tab: usize) {
        let id = PaneId::Plugin(sidebar);
        replace_pane_with_existing_pane(PaneId::Terminal(into), id, true);
        if self.unsettled.remove(&tab) {
            mark(id);
            next_swap_layout();
        }
        if self.want == (Want::Back { focus: true }) {
            focus_plugin_pane(sidebar, false, false);
        }
        self.want = Want::Shown;
        self.placed = Some(tab);
    }
}

/// Marks the tab's layout changed by hand, so that taking a pane out does not
/// apply the tab's swap layout, which would hand the column it leaves to a
/// terminal, and so that asking for the swap layout applies the current one
/// rather than the next. zellij has no call for that, and resizing a pane by
/// its id leaves the mark alone, but stacking a pane on nothing sets it and
/// does nothing else.
fn mark(pane: PaneId) {
    stack_panes(vec![pane]);
}

/// Whether a terminal pane shows, as zellij has it now.
fn showing(pane: u32) -> bool {
    get_pane_info(PaneId::Terminal(pane)).is_some_and(|p| !p.is_suppressed)
}

/// Whether this dock's client is still attached: zellij knows no focused pane
/// for one that left.
fn attached() -> bool {
    get_focused_pane_info().is_ok()
}

fn spot(pane: &PaneInfo) -> Spot {
    (pane.pane_x, pane.pane_y, pane.pane_columns, pane.pane_rows)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const DOCK: u32 = 0;

    /// The column the layout keeps for the sidebar, and somewhere else.
    const COLUMN: Spot = (0, 1, 32, 40);
    const ELSEWHERE: Spot = (80, 1, 40, 20);

    fn tab(position: usize, active: bool) -> TabInfo {
        TabInfo { position, tab_id: position + 10, active, ..TabInfo::default() }
    }

    /// `count` tabs as `client` sees them, with each client of `at` on the
    /// tab at its position.
    fn tabs_of(client: ClientId, at: &[(ClientId, usize)], count: usize) -> Vec<TabInfo> {
        (0..count)
            .map(|position| TabInfo {
                other_focused_clients: at
                    .iter()
                    .filter(|(other, on)| *other != client && *on == position)
                    .map(|(other, _)| *other)
                    .collect(),
                ..tab(position, at.contains(&(client, position)))
            })
            .collect()
    }

    fn placeholder(id: u32, hidden: bool) -> PaneInfo {
        PaneInfo {
            id,
            is_suppressed: hidden,
            is_selectable: true,
            terminal_command: Some("tail -f /dev/null".to_owned()),
            ..PaneInfo::default()
        }
    }

    fn sidebar(id: u32, hidden: bool) -> PaneInfo {
        PaneInfo {
            id,
            is_plugin: true,
            is_suppressed: hidden,
            title: SIDEBAR_TITLE.to_owned(),
            ..PaneInfo::default()
        }
    }

    fn terminal(id: u32) -> PaneInfo {
        PaneInfo { id, is_selectable: true, ..PaneInfo::default() }
    }

    fn at(pane: PaneInfo, (x, y, columns, rows): Spot) -> PaneInfo {
        PaneInfo { pane_x: x, pane_y: y, pane_columns: columns, pane_rows: rows, ..pane }
    }

    fn dock_pane() -> PaneInfo {
        PaneInfo {
            id: DOCK,
            is_plugin: true,
            is_suppressed: true,
            plugin_url: Some("file:luneta.wasm".to_owned()),
            ..PaneInfo::default()
        }
    }

    fn scene(tabs: &[TabInfo], panes: Vec<(usize, Vec<PaneInfo>)>, known: Option<u32>) -> Scene {
        let manifest = PaneManifest { panes: panes.into_iter().collect::<HashMap<_, _>>() };
        Scene::new(&manifest, tabs, DOCK, known).expect("a tab is active")
    }

    fn dock() -> Dock {
        Dock::new(DOCK, 1, BTreeMap::new())
    }

    /// A dock that has seen the panes before: one that is not catching up.
    fn going(client: ClientId) -> Dock {
        Dock { behind: false, ..Dock::new(DOCK, client, BTreeMap::new()) }
    }

    /// What the dock asks for on an update, as though zellij did all of it.
    fn run(dock: &mut Dock, scene: &Scene) -> Vec<Act> {
        let acts = dock.next(scene);
        for act in &acts {
            dock.asked.insert(*act, 0);
        }
        acts
    }

    #[test]
    fn a_session_without_a_sidebar_opens_one_into_the_tab_on_screen() {
        let tabs = [tab(0, true), tab(1, false)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![dock_pane(), placeholder(1, false), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
            ],
            None,
        );
        assert_eq!(scene.url.as_deref(), Some("file:luneta.wasm"));
        assert_eq!(dock().plan(&scene), vec![Act::Open { into: 1, tab: 10 }]);
    }

    #[test]
    fn the_sidebar_follows_a_tab_switch_and_hands_back_the_placeholder() {
        let tabs = [tab(0, false), tab(1, true)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
            ],
            None,
        );
        let mut dock = dock();
        dock.placed = Some(10);
        let moved = Act::Move { sidebar: 5, from: Some(1), into: 3, tab: 11 };
        assert_eq!(dock.plan(&scene), vec![moved]);
    }

    #[test]
    fn a_sidebar_in_the_tab_on_screen_stays() {
        let tabs = [tab(0, true), tab(1, false)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
            ],
            None,
        );
        assert!(dock().plan(&scene).is_empty());
    }

    #[test]
    fn another_client_on_another_tab_does_not_take_the_sidebar_back() {
        // This client brought the sidebar to tab 1 and still sits there; the
        // other client took it to tab 0.
        let tabs = [tab(0, false), tab(1, true)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        let mut dock = dock();
        dock.placed = Some(11);
        assert!(dock.plan(&scene).is_empty());
    }

    #[test]
    fn hiding_takes_out_every_placeholder_and_the_sidebar() {
        let tabs = [tab(0, true), tab(1, false)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        let mut dock = dock();
        dock.want = Want::Hidden;
        assert_eq!(
            dock.plan(&scene),
            vec![Act::Hide { pane: PaneId::Terminal(3) }, Act::Hide { pane: PaneId::Plugin(5) }],
        );
    }

    #[test]
    fn showing_brings_the_placeholders_back_then_the_sidebar() {
        let tabs = [tab(0, false), tab(1, true)];
        let hidden = |dock: &Dock, placeholder_shown: bool| {
            let scene = scene(
                &tabs,
                vec![
                    (0, vec![sidebar(5, true), placeholder(1, true)]),
                    (1, vec![placeholder(3, !placeholder_shown)]),
                ],
                None,
            );
            dock.plan(&scene)
        };
        let mut dock = dock();
        dock.want = Want::Back { focus: false };
        assert_eq!(hidden(&dock, false), vec![Act::Show { pane: 1 }, Act::Show { pane: 3 }]);
        assert_eq!(
            hidden(&dock, true),
            vec![Act::Show { pane: 1 }, Act::Move { sidebar: 5, from: None, into: 3, tab: 11 }],
        );
    }

    #[test]
    fn a_move_seen_halfway_is_not_asked_for_again() {
        // The sidebar has given tab 0 its placeholder back and is hidden, on
        // its way into tab 1's.
        let tabs = [tab(0, false), tab(1, true)];
        let halfway = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, true), placeholder(1, false)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        let moved = Act::Move { sidebar: 5, from: Some(1), into: 3, tab: 11 };
        let mut dock = dock();
        dock.asked.insert(moved, 0);
        assert!(!halfway.shows(moved));
        assert!(dock.plan(&halfway).is_empty());
        let done = scene(
            &tabs,
            vec![
                (0, vec![placeholder(1, false)]),
                (1, vec![sidebar(5, false), placeholder(3, true)]),
            ],
            None,
        );
        assert!(done.shows(moved));
    }

    #[test]
    fn a_hidden_sidebar_this_dock_did_not_bring_back_is_left_to_its_move() {
        let tabs = [tab(0, false), tab(1, true)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, true), placeholder(1, false)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        assert!(dock().plan(&scene).is_empty());
    }

    #[test]
    fn the_first_client_to_attach_leads() {
        let with = |other| {
            let tabs = [TabInfo { other_focused_clients: vec![other], ..tab(0, true) }];
            scene(&tabs, vec![(0, vec![placeholder(1, false)])], None)
        };
        assert!(dock().leads(&with(2)));
        assert!(!dock().leads(&with(0)));
    }

    #[test]
    fn only_the_dock_that_leads_shows_hides_opens_or_closes() {
        // Client 1 on tab 0, client 2 on tab 1; no sidebar, a placeholder
        // hidden, one too many.
        let at = [(1, 0), (2, 1)];
        let panes = || {
            vec![
                (0, vec![placeholder(1, false), placeholder(2, true)]),
                (1, vec![placeholder(3, true)]),
            ]
        };
        let first = scene(&tabs_of(1, &at, 2), panes(), None);
        let second = scene(&tabs_of(2, &at, 2), panes(), None);
        assert_eq!(
            going(1).plan(&first),
            vec![
                Act::Close { pane: PaneId::Terminal(2) },
                Act::Show { pane: 3 },
                Act::Open { into: 1, tab: 10 },
            ],
        );
        assert!(going(2).plan(&second).is_empty());

        let (mut first_dock, mut second_dock) = (going(1), going(2));
        first_dock.want = Want::Hidden;
        second_dock.want = Want::Hidden;
        assert!(first_dock.plan(&first).contains(&Act::Hide { pane: PaneId::Terminal(1) }));
        assert!(second_dock.plan(&second).is_empty());
    }

    #[test]
    fn a_client_that_attaches_while_the_sidebar_is_hidden_leaves_it_hidden() {
        let at = [(1, 0), (2, 1)];
        let hidden = |client| {
            scene(
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![sidebar(5, true), placeholder(1, true)]),
                    (1, vec![placeholder(3, true)]),
                ],
                None,
            )
        };
        let mut first = going(1);
        first.want = Want::Hidden;
        let mut second = Dock::new(DOCK, 2, BTreeMap::new());
        assert!(run(&mut second, &hidden(2)).is_empty());
        assert_eq!(second.want, Want::Hidden);
        assert!(run(&mut first, &hidden(1)).is_empty());

        // Both hear the key that brings it back, and only the first acts.
        first.want = Want::Back { focus: false };
        second.want = Want::Back { focus: false };
        let shown = vec![Act::Show { pane: 1 }, Act::Show { pane: 3 }];
        assert_eq!(run(&mut first, &hidden(1)), shown);
        assert!(run(&mut second, &hidden(2)).is_empty());
    }

    #[test]
    fn a_dock_that_catches_up_reads_what_the_sidebar_is_to_be_off_the_panes() {
        let tabs = [tab(0, true), tab(1, false)];
        let resting = |zero, one| scene(&tabs, vec![(0, zero), (1, one)], None).resting();
        assert_eq!(
            resting(vec![sidebar(5, true), placeholder(1, true)], vec![placeholder(3, true)]),
            Want::Hidden,
        );
        assert_eq!(resting(vec![placeholder(1, true)], vec![placeholder(3, true)]), Want::Hidden);
        // Halfway through a move, the sidebar hidden and the placeholders back.
        assert_eq!(
            resting(vec![sidebar(5, true), placeholder(1, false)], vec![placeholder(3, false)]),
            Want::Shown,
        );
        assert_eq!(
            resting(vec![sidebar(5, false), placeholder(1, true)], vec![placeholder(3, true)]),
            Want::Shown,
        );
        assert_eq!(resting(vec![terminal(2)], vec![terminal(4)]), Want::Shown);
    }

    #[test]
    fn a_sidebar_closed_by_hand_hides_it_for_every_client() {
        // The sidebar stands in tab 0, where client 1 closes it; client 2
        // looks at tab 1.
        let at = [(1, 0), (2, 1)];
        let docked = |client| {
            scene(
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![sidebar(5, false), placeholder(1, true)]),
                    (1, vec![placeholder(3, false)]),
                ],
                None,
            )
        };
        let closed = |client| {
            scene(
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![placeholder(1, true), terminal(2)]),
                    (1, vec![placeholder(3, false)]),
                ],
                None,
            )
        };
        let (mut first, mut second) = (going(1), going(2));
        first.placed = Some(10);
        second.placed = Some(11);
        run(&mut first, &docked(1));
        run(&mut second, &docked(2));
        assert_eq!(
            run(&mut first, &closed(1)),
            vec![
                Act::Show { pane: 1 },
                Act::Hide { pane: PaneId::Terminal(1) },
                Act::Hide { pane: PaneId::Terminal(3) },
            ],
        );
        assert!(run(&mut second, &closed(2)).is_empty());
        assert_eq!(first.want, Want::Hidden);
        assert_eq!(second.want, Want::Hidden);
    }

    #[test]
    fn a_sidebar_lost_with_its_tab_opens_again() {
        let with = |tabs: &[TabInfo], panes| scene(tabs, panes, None);
        let mut dock = going(1);
        run(
            &mut dock,
            &with(
                &[tab(0, false), tab(1, true)],
                vec![
                    (0, vec![placeholder(1, false)]),
                    (1, vec![sidebar(5, false), placeholder(3, true)]),
                ],
            ),
        );
        // Tab 1 closes, and with it the sidebar.
        let left = with(&[tab(0, true)], vec![(0, vec![placeholder(1, false), terminal(2)])]);
        assert_eq!(run(&mut dock, &left), vec![Act::Open { into: 1, tab: 10 }]);
        assert_eq!(dock.want, Want::Shown);
    }

    #[test]
    fn a_sidebar_being_opened_is_not_lost() {
        let tabs = [tab(0, true)];
        let scene = scene(&tabs, vec![(0, vec![placeholder(1, false)])], None);
        let mut dock = dock();
        dock.sidebar = Some(5);
        dock.asked.insert(Act::Open { into: 1, tab: 10 }, 0);
        dock.observe(&scene);
        assert_eq!(dock.sidebar, Some(5));
    }

    #[test]
    fn a_dock_that_waits_for_the_sidebar_to_come_back_sees_it_shown() {
        let at = [(1, 0), (2, 1)];
        let mut second = going(2);
        second.want = Want::Back { focus: true };
        let back = scene(
            &tabs_of(2, &at, 2),
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        second.observe(&back);
        assert_eq!(second.want, Want::Shown);
    }

    #[test]
    fn focus_on_a_sidebar_that_shows_goes_into_it() {
        let tabs = [tab(0, true)];
        let shown = scene(&tabs, vec![(0, vec![sidebar(5, false), placeholder(1, true)])], None);
        let mut dock = going(1);
        dock.want = Want::Back { focus: true };
        assert_eq!(run(&mut dock, &shown), vec![Act::Focus { sidebar: 5 }]);
    }

    #[test]
    fn a_placeholder_back_elsewhere_in_the_tab_on_screen_is_put_back() {
        // Another client keeps the sidebar in tab 0; this one looks at tab 1.
        let tabs = [tab(0, false), tab(1, true)];
        let scene = scene(
            &tabs,
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true)]),
                (1, vec![placeholder(3, false)]),
            ],
            None,
        );
        let mut dock = dock();
        dock.placed = Some(11);
        dock.unsettled.insert(11);
        assert_eq!(dock.plan(&scene), vec![Act::Restore { placeholder: 3, tab: 11 }]);
    }

    #[test]
    fn a_placeholder_back_from_under_the_sidebar_is_in_place_and_one_back_from_hidden_is_not() {
        let tabs = [tab(0, true), tab(1, false)];
        let docked = scene(
            &tabs,
            vec![
                (0, vec![at(sidebar(5, false), COLUMN), at(placeholder(1, true), ELSEWHERE)]),
                (1, vec![at(placeholder(3, false), COLUMN)]),
            ],
            None,
        );
        let moved = scene(
            &tabs,
            vec![
                (0, vec![at(placeholder(1, false), COLUMN)]),
                (1, vec![at(sidebar(5, false), COLUMN), at(placeholder(3, true), COLUMN)]),
            ],
            None,
        );
        let hidden = scene(
            &tabs,
            vec![
                (0, vec![at(placeholder(1, true), COLUMN)]),
                (1, vec![at(sidebar(5, true), COLUMN), at(placeholder(3, true), COLUMN)]),
            ],
            None,
        );
        let back = |spot| {
            scene(
                &tabs,
                vec![
                    (0, vec![at(placeholder(1, false), spot)]),
                    (1, vec![at(sidebar(5, true), COLUMN), at(placeholder(3, false), COLUMN)]),
                ],
                None,
            )
        };
        // Every dock sees the same panes, and finds the same tabs unsettled.
        for client in [1, 2] {
            let mut dock = going(client);
            for scene in [&docked, &moved, &hidden] {
                dock.observe(scene);
                assert!(dock.unsettled.is_empty());
            }
            dock.observe(&back(ELSEWHERE));
            assert_eq!(dock.unsettled, BTreeSet::from([10]));
            dock.observe(&back(COLUMN));
            assert!(dock.unsettled.is_empty());
        }
    }

    #[test]
    fn extra_sidebars_and_placeholders_close() {
        let tabs = [tab(0, true)];
        let panes =
            vec![sidebar(7, false), sidebar(5, false), placeholder(1, true), placeholder(2, true)];
        let scene = scene(&tabs, vec![(0, panes)], None);
        assert_eq!(
            dock().plan(&scene),
            vec![Act::Close { pane: PaneId::Plugin(7) }, Act::Close { pane: PaneId::Terminal(2) }],
        );
    }

    #[test]
    fn a_floating_sidebar_is_one_still_being_docked() {
        let tabs = [tab(0, true)];
        let floating = PaneInfo {
            is_floating: true,
            title: "file:luneta.wasm".to_owned(),
            ..sidebar(5, false)
        };
        let scene = scene(&tabs, vec![(0, vec![floating, placeholder(1, false)])], Some(5));
        assert!(dock().plan(&scene).is_empty());
    }
}
