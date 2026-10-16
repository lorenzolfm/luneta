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
    /// The sidebar's pane, from the moment the dock opens it.
    sidebar: Option<u32>,
    /// The tab, by id, this client last had the sidebar brought to. zellij
    /// runs a dock for each client, and each follows only its own, so two
    /// clients on two tabs do not take the sidebar from each other forever.
    placed: Option<usize>,
    /// What the dock has asked for and the panes do not show yet, and how many
    /// updates ago. An update can show the panes from before, or from halfway.
    asked: BTreeMap<Act, u32>,
    /// Where each placeholder stood before it hid.
    spots: BTreeMap<u32, Spot>,
    /// The placeholder of a sidebar closed by hand, and its tab's id. Closing
    /// it applied the tab's swap layout, which handed its column to a
    /// terminal; the placeholder takes the column back, and then hides as any
    /// other does.
    reclaim: Option<(u32, usize)>,
    /// Tabs, by id, whose placeholder came back somewhere else. The tab's swap
    /// layout puts things back when the tab comes on screen, the only time
    /// the dock can ask for it.
    unsettled: BTreeSet<usize>,
}

/// What the sidebar is to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Want {
    #[default]
    Shown,
    Hidden,
    /// Coming back from hidden, and taking the focus if `focus`. Otherwise a
    /// hidden sidebar is one halfway through a move another client's dock
    /// asked for, and is left to it.
    Back { focus: bool },
}

/// Something the dock asks zellij for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Act {
    /// Hide a pane, the panes beside it taking its columns.
    Hide { pane: PaneId, tab: usize },
    /// Show a hidden placeholder again.
    Show { pane: u32, tab: usize },
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

    fn id(&self, tab: usize) -> usize {
        self.ids.get(&tab).copied().unwrap_or(tab)
    }

    /// Whether the panes show what was asked for.
    fn shows(&self, act: Act) -> bool {
        let placeholder = |id| self.placeholders.values().flatten().find(|p| p.id == id);
        let sidebar = |id| self.sidebars.iter().find(|(_, s)| s.id == id).map(|(_, s)| s);
        match act {
            Act::Hide { pane: PaneId::Terminal(id), .. } => {
                placeholder(id).is_none_or(|p| p.hidden)
            },
            Act::Hide { pane: PaneId::Plugin(id), .. } => sidebar(id).is_none_or(|s| s.hidden),
            Act::Show { pane, .. } => placeholder(pane).is_some_and(|p| !p.hidden),
            Act::Open { tab, .. } | Act::Move { tab, .. } => self.sidebar().is_some_and(|(at, s)| {
                self.id(at) == tab && !s.hidden && !s.floating
            }),
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
        Dock { plugin_id, client, sidebar_configuration, ..Dock::default() }
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
        self.want = match (payload, self.want) {
            (Some("toggle"), Want::Hidden) => Want::Back { focus: false },
            (Some("toggle"), _) => Want::Hidden,
            (Some("focus"), Want::Hidden) => Want::Back { focus: true },
            _ => return,
        };
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
        self.asked.retain(|act, waited| {
            *waited += 1;
            *waited < PATIENCE && !scene.shows(*act)
        });
        self.observe(&scene);
        if matches!(self.want, Want::Back { .. }) && !self.leads(&scene) {
            self.want = Want::Shown;
        }
        for act in self.plan(&scene) {
            if self.asked.contains_key(&act) {
                continue;
            }
            self.asked.insert(act, 0);
            self.act(act, &scene);
        }
        self.reclaim = None;
    }

    /// Keeps track of the sidebar: the one the dock opened, until zellij shows
    /// it, and one closed by hand where it stood, which hides the rest.
    fn observe(&mut self, scene: &Scene) {
        if let Some((_, sidebar)) = scene.sidebar() {
            self.sidebar = Some(sidebar.id);
            return;
        }
        if self.docking() || self.sidebar.take().is_none() || self.want == Want::Hidden {
            return;
        }
        let parked = scene
            .placeholders
            .get(&scene.active)
            .filter(|placeholders| placeholders.iter().all(|p| p.hidden))
            .and_then(|placeholders| placeholders.first());
        if let Some(parked) = parked {
            self.want = Want::Hidden;
            self.reclaim = Some((parked.id, scene.active_id));
        }
    }

    fn plan(&self, scene: &Scene) -> Vec<Act> {
        let mut acts = Vec::new();
        let sidebar = scene.sidebar();
        for (_, extra) in &scene.sidebars {
            if sidebar.is_some_and(|(_, kept)| kept.id != extra.id) {
                acts.push(Act::Close { pane: PaneId::Plugin(extra.id) });
            }
        }
        for (tab, placeholders) in &scene.placeholders {
            let kept = scene.placeholder(*tab).map(|p| p.id);
            for extra in placeholders.iter().filter(|p| Some(p.id) != kept) {
                acts.push(Act::Close { pane: PaneId::Terminal(extra.id) });
            }
        }
        if self.want == Want::Hidden {
            if let Some((pane, tab)) = self.reclaim {
                acts.push(Act::Show { pane, tab });
                acts.push(Act::Hide { pane: PaneId::Terminal(pane), tab });
            }
            for tab in scene.placeholders.keys() {
                if let Some(placeholder) = scene.placeholder(*tab).filter(|p| !p.hidden) {
                    let pane = PaneId::Terminal(placeholder.id);
                    acts.push(Act::Hide { pane, tab: scene.id(*tab) });
                }
            }
            if let Some((tab, sidebar)) = sidebar.filter(|(_, s)| !s.hidden) {
                acts.push(Act::Hide { pane: PaneId::Plugin(sidebar.id), tab: scene.id(tab) });
            }
            return acts;
        }
        let home = sidebar.filter(|(_, s)| !s.hidden && !s.floating).map(|(tab, _)| tab);
        for tab in scene.placeholders.keys().filter(|tab| Some(**tab) != home) {
            if let Some(placeholder) = scene.placeholder(*tab).filter(|p| p.hidden) {
                acts.push(Act::Show { pane: placeholder.id, tab: scene.id(*tab) });
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
            None => {
                if let Some(into) = into {
                    acts.push(Act::Open { into, tab });
                }
            },
            Some((_, sidebar)) if sidebar.floating => {},
            Some((_, sidebar)) => {
                let away = home != Some(scene.active) && self.placed != Some(tab);
                let back = matches!(self.want, Want::Back { .. });
                let here = home == Some(scene.active);
                match into {
                    Some(into) if sidebar.hidden && back || !sidebar.hidden && away => {
                        let from = home.and_then(|home| scene.placeholder(home)).map(|p| p.id);
                        acts.push(Act::Move { sidebar: sidebar.id, from, into, tab });
                    },
                    _ if here && self.want == (Want::Back { focus: true }) => {
                        acts.push(Act::Focus { sidebar: sidebar.id });
                    },
                    _ => {},
                }
            },
        }
        acts
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

    fn act(&mut self, act: Act, scene: &Scene) {
        match act {
            Act::Hide { pane, tab } => {
                if let (PaneId::Terminal(id), Some(placeholder)) = (pane, get_pane_info(pane)) {
                    if !self.unsettled.contains(&tab) {
                        self.spots.insert(id, spot(&placeholder));
                    }
                }
                mark(pane);
                hide_pane_with_id(pane);
            },
            Act::Show { pane, tab } => {
                let id = PaneId::Terminal(pane);
                show_pane_with_id(id, false, false);
                if get_pane_info(id).map(|p| spot(&p)) != self.spots.get(&pane).copied() {
                    self.unsettled.insert(tab);
                }
            },
            Act::Open { into, tab } => {
                let Some(url) = scene.url.as_deref() else {
                    return;
                };
                let configuration = self.sidebar_configuration.clone();
                let opened = open_plugin_pane_floating(url, configuration, None, BTreeMap::new());
                if let Some(PaneId::Plugin(sidebar)) = opened {
                    self.sidebar = Some(sidebar);
                    self.settle(sidebar, into, tab);
                }
            },
            Act::Move { sidebar, from, into, tab } => {
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

fn spot(pane: &PaneInfo) -> Spot {
    (pane.pane_x, pane.pane_y, pane.pane_columns, pane.pane_rows)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const DOCK: u32 = 0;

    fn tab(position: usize, active: bool) -> TabInfo {
        TabInfo { position, tab_id: position + 10, active, ..TabInfo::default() }
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
            vec![
                Act::Hide { pane: PaneId::Terminal(3), tab: 11 },
                Act::Hide { pane: PaneId::Plugin(5), tab: 10 },
            ],
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
        assert_eq!(
            hidden(&dock, false),
            vec![Act::Show { pane: 1, tab: 10 }, Act::Show { pane: 3, tab: 11 }],
        );
        assert_eq!(
            hidden(&dock, true),
            vec![
                Act::Show { pane: 1, tab: 10 },
                Act::Move { sidebar: 5, from: None, into: 3, tab: 11 },
            ],
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
    fn a_sidebar_closed_by_hand_hides_the_rest() {
        let tabs = [tab(0, true), tab(1, false)];
        let scene = scene(
            &tabs,
            vec![(0, vec![placeholder(1, true), terminal(2)]), (1, vec![placeholder(3, false)])],
            None,
        );
        let mut dock = dock();
        dock.sidebar = Some(5);
        dock.observe(&scene);
        assert_eq!(dock.want, Want::Hidden);
        assert_eq!(
            dock.plan(&scene),
            vec![
                Act::Show { pane: 1, tab: 10 },
                Act::Hide { pane: PaneId::Terminal(1), tab: 10 },
                Act::Hide { pane: PaneId::Terminal(3), tab: 11 },
            ],
        );
    }

    #[test]
    fn a_sidebar_lost_with_its_tab_opens_again() {
        let tabs = [tab(0, true)];
        let scene = scene(&tabs, vec![(0, vec![placeholder(1, false), terminal(2)])], None);
        let mut dock = dock();
        dock.sidebar = Some(5);
        dock.observe(&scene);
        assert_eq!(dock.want, Want::Shown);
        assert_eq!(dock.plan(&scene), vec![Act::Open { into: 1, tab: 10 }]);
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
