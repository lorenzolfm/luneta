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
//! Either goes for every session: the docks of the others take it up, and so
//! does the dock of a session that starts later. A tab left with nothing but
//! its column closes, as it would without one.
//!
//! zellij runs a dock for each client, and every dock sees the same panes and
//! the tab every client is on. Only the dock of the client that attached first
//! moves, hides, shows, opens or closes anything, so two docks never pull the
//! columns two ways: it takes the sidebar to the tab a client came to last.
//! zellij applies a swap layout to the tab on screen for the client that asks,
//! so putting a tab's layout back is for the dock of a client on it.

use std::collections::{BTreeMap, BTreeSet};

use zellij_tile::prelude::*;

use crate::close;
use crate::hiding::{Hiding, Said};
use crate::tree::{is_placeholder, PLACEHOLDER};

/// The title the sidebar and the picker give their panes where they belong:
/// the sidebar in its column, the picker floating over the tab. They are the
/// same plugin, and zellij reports nothing else of a pane that sets them
/// apart: anywhere else, each takes a title of its own.
pub const TITLE: &str = "luneta";

/// The sidebar's title while it floats, floated out of its column by hand.
const FLOATING_SIDEBAR_TITLE: &str = "luneta sidebar";

/// The picker's title while it does not float: opened in place of a pane,
/// without `floating true`, or by a layout.
const TILED_PICKER_TITLE: &str = "luneta picker";

/// Whether `pane` is a sidebar, by its title: a plugin pane titled `luneta`
/// that stands in a column, or one with the title of a floating sidebar.
pub fn is_sidebar(pane: &PaneInfo) -> bool {
    let title = pane.title.as_str();
    pane.is_plugin && (title == TITLE && !pane.is_floating || title == FLOATING_SIDEBAR_TITLE)
}

/// The sidebar among a session's panes, while it stands in its column: of
/// two, the one with the lower id, which the dock keeps.
pub fn standing(manifest: &PaneManifest) -> Option<u32> {
    let panes = manifest.panes.values().flatten();
    panes.filter(|p| is_sidebar(p) && !p.is_suppressed && !p.is_floating).map(|p| p.id).min()
}

/// The title the sidebar's `pane` is to have where it stands, when not the
/// one it has. A sidebar the dock is still docking floats before it has a
/// title, and takes one once it stands in its column.
pub fn sidebar_title(pane: &PaneInfo) -> Option<&'static str> {
    let title = match (pane.is_floating, pane.title.as_str()) {
        (false, _) => TITLE,
        (true, TITLE | FLOATING_SIDEBAR_TITLE) => FLOATING_SIDEBAR_TITLE,
        (true, _) => return None,
    };
    (pane.title != title).then_some(title)
}

/// The title the picker's `pane` is to have where it stands, when not the one
/// it has.
pub fn picker_title(pane: &PaneInfo) -> Option<&'static str> {
    let title = if pane.is_floating { TITLE } else { TILED_PICKER_TITLE };
    (pane.title != title).then_some(title)
}

/// How many reports of the panes and tabs zellij may take to show what the
/// dock asked for before the dock asks again.
const PATIENCE: u32 = 10;

/// Where a pane stands: x, y, columns, rows.
type Spot = (usize, usize, usize, usize);

/// A tab, by the id zellij keeps for it while it lives. Its position changes
/// as the tabs before it close; the id does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TabId(pub usize);

impl TabId {
    /// The id of `tab`, and not its position, which zellij hands over beside
    /// it as a number of the same kind.
    pub fn of(tab: &TabInfo) -> Self {
        TabId(tab.tab_id)
    }
}

/// The key the dock opens the sidebar with, in its configuration, when the
/// sidebar is to take the focus, its value the dock's client. It would send
/// on a focus no key asked for, and the dock cannot tell it otherwise:
/// messaging another plugin takes a permission luneta does not ask for.
/// zellij runs the sidebar once for each client, every one of them with this
/// configuration, and the focus the dock hands it is its own client's.
pub const TAKES_FOCUS: &str = "takes_focus";

/// What a key binding asks through the `sidebar` pipe. Every plugin in the
/// session hears it, and the dock and the sidebar each answer their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Into the sidebar, or back out of it. A hidden one comes back first.
    Focus,
    /// To the pane the sidebar lists after the one you are in, and from the
    /// last back to the first.
    Cycle,
    /// Hide the sidebar, or bring it back.
    Toggle,
}

impl Ask {
    pub fn parse(payload: &str) -> Option<Self> {
        match payload.trim() {
            "focus" => Some(Ask::Focus),
            "cycle" => Some(Ask::Cycle),
            "toggle" => Some(Ask::Toggle),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Dock {
    plugin_id: u32,
    /// The client this dock answers to: zellij runs a dock for each.
    client: ClientId,
    /// The sidebar's configuration: the dock's own, in `mode "sidebar"`.
    sidebar_configuration: BTreeMap<String, String>,
    granted: bool,
    panes: Option<PaneManifest>,
    /// Panes zellij has told of whose tabs have not come yet. zellij reports
    /// the panes, then the tabs, after every change; read against the tabs
    /// from before, the panes of a closed tab show its sidebar gone from a tab
    /// still there, as though closed by hand.
    early: Option<PaneManifest>,
    tabs: Vec<TabInfo>,
    want: Want,
    /// Whether the dock missed how the panes came to be: one that just
    /// started, for a client that just attached, or one whose client was away
    /// when a key came. It reads what the sidebar is to be off the panes.
    behind: bool,
    /// The sidebar's pane, from the moment the dock opens it.
    sidebar: Option<u32>,
    /// The tab the sidebar was last seen in.
    stood: Option<TabId>,
    /// Whether the sidebar had the focus when last seen, with no floating
    /// pane over it: one gone with its tab takes the focus where it opens
    /// again.
    focused: bool,
    /// The tab each client was on at the last update.
    seats: BTreeMap<ClientId, TabId>,
    /// The client the sidebar follows: the last to come to a tab, or to
    /// attach. Every dock keeps it, and the one that leads goes by it.
    followed: Option<ClientId>,
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
    /// Tabs whose placeholder came back somewhere else. The tab's swap layout
    /// puts things back when the tab comes on screen, the only time a dock
    /// can ask for it.
    unsettled: BTreeSet<TabId>,
    /// Tabs whose swap layout was as zellij left it the last time the sidebar
    /// showed.
    clean: BTreeSet<TabId>,
    /// Tabs whose swap layout hiding marked changed by hand, and that were
    /// clean before. zellij arranges no pane that opens in such a tab until
    /// the swap layout is applied again, when the tab comes on screen.
    marked: BTreeSet<TabId>,
    /// The id of this session's zellij server, by which the docks of every
    /// session tell what their own said from what the others did.
    server: u32,
    /// Whether the sidebar is hidden, as a dock of some session said last.
    hiding: Hiding,
}

/// What the sidebar is to be.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Want {
    #[default]
    Shown,
    Hidden,
    /// Coming back from hidden, or opening again after its tab closed, and
    /// taking the focus if `focus`. The dock that leads brings it back; the
    /// others wait to see it.
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
    Open { into: u32, tab: TabId },
    /// Apply the swap layout of the tab on screen again, which puts a
    /// placeholder that came back elsewhere in its column, and has zellij
    /// arrange the panes that open in the tab again. `pane` stands in the
    /// tab's column.
    Restore { pane: PaneId, tab: TabId },
    /// Bring the sidebar into a placeholder, giving the tab it leaves back the
    /// placeholder it covered there.
    Move { sidebar: u32, from: Option<u32>, into: u32, tab: TabId },
    Focus { sidebar: u32 },
    Close { pane: PaneId },
    CloseTab { tab: TabId },
}

/// The panes as the dock needs them.
#[derive(Debug)]
struct Scene {
    /// The tab on screen.
    active: TabId,
    tabs: BTreeSet<TabId>,
    /// The tab each client is on, this dock's own among them.
    seats: BTreeMap<ClientId, TabId>,
    /// Every pane that is a sidebar, and its tab.
    sidebars: Vec<(TabId, Seen)>,
    /// Every placeholder, by tab.
    placeholders: BTreeMap<TabId, Vec<Seen>>,
    /// The url the dock was loaded from, for the sidebar.
    url: Option<String>,
    /// Tabs whose swap layout zellij has not marked changed by hand. zellij
    /// names no layout for a tab of one pane, and calls it unmarked whatever
    /// it is.
    clean: BTreeSet<TabId>,
    /// Tabs showing their floating panes. zellij reports the floating panes'
    /// layout for such a tab, and applies theirs when asked for one.
    floating: BTreeSet<TabId>,
    /// Tabs with a pane that holds them open, as `close::holds_tab` says.
    occupied: BTreeSet<TabId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    id: u32,
    hidden: bool,
    floating: bool,
    focused: bool,
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
            focused: pane.is_focused,
            spot: spot(pane),
        }
    }

    /// Whether it stands in a column.
    fn stands(&self) -> bool {
        !self.hidden && !self.floating
    }
}

impl Scene {
    /// zellij keys the panes by the position of their tab, and only the tabs
    /// say which tab that is: the panes go by tab id from here on.
    fn new(
        manifest: &PaneManifest,
        tabs: &[TabInfo],
        client: ClientId,
        plugin_id: u32,
        sidebar: Option<u32>,
    ) -> Option<Self> {
        let id = TabId::of;
        let active = id(tabs.iter().find(|t| t.active)?);
        let mut seats: BTreeMap<ClientId, TabId> = tabs
            .iter()
            .flat_map(|t| t.other_focused_clients.iter().map(move |other| (*other, id(t))))
            .collect();
        seats.insert(client, active);
        let mut scene = Scene {
            active,
            tabs: tabs.iter().map(id).collect(),
            seats,
            sidebars: Vec::new(),
            placeholders: BTreeMap::new(),
            url: None,
            clean: tabs
                .iter()
                .filter(|t| t.active_swap_layout_name.is_some() && !t.is_swap_layout_dirty)
                .filter(|t| !t.are_floating_panes_visible)
                .map(id)
                .collect(),
            floating: tabs.iter().filter(|t| t.are_floating_panes_visible).map(id).collect(),
            occupied: BTreeSet::new(),
        };
        let positions: BTreeMap<usize, TabId> = tabs.iter().map(|t| (t.position, id(t))).collect();
        for (position, panes) in &manifest.panes {
            // Panes of a tab these tabs do not have are from another report:
            // the dock waits for the panes and the tabs of the same one.
            let tab = *positions.get(position)?;
            for pane in panes {
                if pane.is_plugin && pane.id == plugin_id {
                    scene.url.clone_from(&pane.plugin_url);
                } else if is_sidebar(pane) || pane.is_plugin && Some(pane.id) == sidebar {
                    scene.sidebars.push((tab, Seen::new(pane)));
                } else if is_placeholder(pane) {
                    scene.placeholders.entry(tab).or_default().push(Seen::new(pane));
                } else if close::holds_tab(pane) {
                    scene.occupied.insert(tab);
                }
            }
        }
        Some(scene)
    }

    /// The tab's placeholder: the one showing, or else the first.
    fn placeholder(&self, tab: TabId) -> Option<Seen> {
        let placeholders = self.placeholders.get(&tab)?;
        placeholders.iter().find(|p| !p.hidden).or(placeholders.first()).copied()
    }

    /// Which sidebar stays, when there are two. Every client's dock picks the
    /// same one.
    fn sidebar(&self) -> Option<(TabId, Seen)> {
        self.sidebars.iter().min_by_key(|(_, s)| s.id).copied()
    }

    /// The sidebar, when it stands in a column.
    fn home(&self) -> Option<(TabId, Seen)> {
        self.sidebar().filter(|(_, s)| s.stands())
    }

    /// Tabs left with nothing but the sidebar or a placeholder. zellij closes
    /// a tab once none of its tiled panes can take the focus, and both of
    /// those can. A tab that holds neither is zellij's to close, or still
    /// opening.
    fn deserted(&self) -> Vec<TabId> {
        let column = |tab: &TabId| {
            self.home().is_some_and(|(at, _)| at == *tab)
                || self.placeholders.get(tab).is_some_and(|ps| ps.iter().any(Seen::stands))
        };
        self.tabs.iter().filter(|tab| !self.occupied.contains(tab) && column(tab)).copied().collect()
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
                self.home().is_some_and(|(at, _)| at == tab)
            },
            Act::Focus { .. } | Act::Restore { .. } => true,
            Act::Close { pane: PaneId::Terminal(id) } => placeholder(id).is_none(),
            Act::Close { pane: PaneId::Plugin(id) } => sidebar(id).is_none(),
            Act::CloseTab { tab } => !self.tabs.contains(&tab),
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

    /// The dock, hiding the sidebar and bringing it back for every session.
    /// `server` is the id of this session's zellij server.
    pub fn shared(self, server: u32) -> Self {
        Dock { server, hiding: Hiding::shared(), ..self }
    }

    pub fn grant(&mut self) {
        self.granted = true;
        self.step();
    }

    pub fn tabs(&mut self, tabs: Vec<TabInfo>) {
        self.tabs = tabs;
        if let Some(panes) = self.early.take() {
            self.panes = Some(panes);
        }
        self.step();
    }

    /// The dock reads the panes with the tabs that follow them.
    pub fn panes(&mut self, panes: PaneManifest) {
        self.early = Some(panes);
    }

    /// `toggle` hides the sidebar or brings it back; `focus` brings a hidden
    /// one back and moves into it. The sidebar itself answers a `focus` while
    /// it shows.
    pub fn pipe(&mut self, ask: Ask) {
        let want = match (ask, self.want) {
            (Ask::Toggle, Want::Hidden) => Want::Back { focus: false },
            (Ask::Toggle, _) => Want::Hidden,
            (Ask::Focus, Want::Hidden) => Want::Back { focus: true },
            (Ask::Focus | Ask::Cycle, _) => return,
        };
        if self.take(want) {
            self.say(want == Want::Hidden);
        }
    }

    /// Takes up what the dock of another session said since, once a second.
    /// What this session's own said came to it as a key already. A dock that
    /// is behind reads it as it catches up.
    pub fn tick(&mut self) {
        if self.behind {
            return;
        }
        let Some(said) = self.hiding.read().filter(|said| said.by != self.server) else {
            return;
        };
        if said.hidden != (self.want == Want::Hidden) {
            self.take(if said.hidden { Want::Hidden } else { Want::Back { focus: false } });
        }
    }

    /// Says whether the dock took it.
    fn take(&mut self, want: Want) -> bool {
        // zellij keeps a client's dock running after the client leaves, and
        // hands it every key still. It has seen none of the panes since, so it
        // leaves them be, and catches up if its client comes back. When zellij
        // is too busy to say, a dock goes on as it was: one whose client left
        // is behind from the first key that came since.
        if self.granted && !attached().unwrap_or(!self.behind) {
            self.behind = true;
            return false;
        }
        self.want = want;
        // Back from hidden, it goes to the tab of the client that attached
        // first, whoever came to a tab last.
        if let Want::Back { .. } = want {
            self.followed = None;
        }
        self.asked.clear();
        self.step();
        true
    }

    /// Hides the sidebar of every session, or brings it back.
    fn say(&mut self, hidden: bool) {
        self.hiding.write(Said { hidden, by: self.server });
    }

    fn step(&mut self) {
        if !self.granted {
            return;
        }
        let Some(scene) = self.scene() else {
            return;
        };
        for act in self.next(&scene) {
            if self.act(act, &scene) {
                self.asked.insert(act, 0);
            }
        }
    }

    /// The panes as the dock reads them, once zellij has told it of both the
    /// panes and the tabs.
    fn scene(&self) -> Option<Scene> {
        let (Some(manifest), false) = (self.panes.as_ref(), self.tabs.is_empty()) else {
            return None;
        };
        Scene::new(manifest, &self.tabs, self.client, self.plugin_id, self.sidebar)
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
        self.marks(scene);
        self.follow(scene);
        let mut acts = self.plan(scene);
        self.reclaim = None;
        acts.retain(|act| !self.asked.contains_key(act));
        acts
    }

    /// Starts over from the panes as they are, and from what a dock of some
    /// session said last, whichever session's: a sidebar hidden elsewhere
    /// hides, and one shown elsewhere comes back.
    fn catch_up(&mut self, scene: &Scene) {
        self.behind = false;
        self.want = match (self.hiding.read(), scene.resting()) {
            (Some(Said { hidden: true, .. }), _) => Want::Hidden,
            (Some(Said { hidden: false, .. }), Want::Hidden) => Want::Back { focus: false },
            (_, resting) => resting,
        };
        self.asked.clear();
        self.homes.clear();
        self.unsettled.clear();
        self.stood = None;
        self.focused = false;
    }

    /// Keeps track of the placeholders and of the sidebar: the one the dock
    /// opened, until zellij shows it, one gone from a tab that is still there,
    /// closed by hand, which hides the rest, and one gone with its tab, which
    /// opens again with the focus it had. Every dock sees it go, whichever tab
    /// its client is on.
    fn observe(&mut self, scene: &Scene) {
        self.track(scene);
        if let Some((tab, sidebar)) = scene.sidebar() {
            self.sidebar = Some(sidebar.id);
            self.stood = Some(tab);
            // The last pane of a tab hands the sidebar its focus as it closes,
            // and leaves the tab to close with nothing but the column: you
            // were not in the sidebar, and what it had before stands.
            if scene.occupied.contains(&tab) {
                self.focused = sidebar.focused && !scene.floating.contains(&tab);
            }
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
        let focused = std::mem::take(&mut self.focused);
        let (Some(_), Some(stood)) = (self.sidebar.take(), self.stood.take()) else {
            return;
        };
        if self.want == Want::Hidden {
            return;
        }
        // Gone with its tab, it opens again, and takes the focus once it had
        // it: you were in it as the tab closed, as a key of its menu closes
        // the tab of the last pane that holds it open.
        if !scene.tabs.contains(&stood) {
            if focused {
                self.want = Want::Back { focus: true };
            }
            return;
        }
        self.want = Want::Hidden;
        self.say(true);
        self.reclaim = scene
            .placeholders
            .get(&stood)
            .filter(|placeholders| placeholders.iter().all(|p| p.hidden))
            .and_then(|placeholders| placeholders.first())
            .map(|p| p.id);
    }

    /// Keeps track of the tabs hiding marks: each whose swap layout was clean
    /// the last time the sidebar showed, or that opens clean while it is
    /// hidden, until its layout is clean again.
    fn marks(&mut self, scene: &Scene) {
        if self.want == Want::Hidden {
            self.marked.extend(self.clean.iter().chain(&scene.clean));
        } else {
            self.marked.retain(|tab| !scene.clean.contains(tab) && scene.tabs.contains(tab));
            self.clean.clone_from(&scene.clean);
        }
    }

    /// Takes in where every client is. One on a tab it was not on at the last
    /// update, or new to the session, takes the sidebar with it; of two that
    /// came at once, the first to attach does.
    fn follow(&mut self, scene: &Scene) {
        let came = scene.seats.iter().find(|(client, tab)| self.seats.get(*client) != Some(*tab));
        if let Some((client, _)) = came {
            self.followed = Some(*client);
        }
        self.seats.clone_from(&scene.seats);
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
            let spot = self.belongs(placeholder, covered, *tab);
            homes.insert(placeholder.id, Home { spot, hidden: placeholder.hidden });
        }
        self.homes = homes;
    }

    /// Where a placeholder belongs. One back from hidden where it belongs came
    /// back from under a sidebar that left; one back somewhere else leaves its
    /// tab unsettled until it is back in place.
    fn belongs(&mut self, placeholder: Seen, covered: Option<Spot>, tab: TabId) -> Spot {
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
        let deserted = scene.deserted();
        if leads {
            acts.extend(deserted.iter().map(|tab| Act::CloseTab { tab: *tab }));
            for tab in scene.placeholders.keys().filter(|tab| Some(**tab) != home) {
                if let Some(placeholder) = scene.placeholder(*tab).filter(|p| p.hidden) {
                    acts.push(Act::Show { pane: placeholder.id });
                }
            }
        }
        let tab = self.target(scene);
        // A tab on its way out is no place to dock, and takes a sidebar in it
        // along: the dock opens it again once the tab is gone.
        let leaving = |tab| deserted.contains(&tab);
        if self.docking() || leaving(scene.active) || leaving(tab) || home.is_some_and(leaving) {
            return acts;
        }
        if let Some(pane) = self.restore(scene) {
            acts.push(Act::Restore { pane, tab: scene.active });
        }
        // Every dock reads the same panes, and two that moved the sidebar off
        // them would pull it two ways.
        if !leads {
            return acts;
        }
        let into = scene.placeholder(tab).filter(|p| !p.hidden).map(|p| p.id);
        match sidebar {
            None => acts.extend(into.map(|into| Act::Open { into, tab })),
            Some((_, sidebar)) if sidebar.floating => {},
            Some((_, sidebar)) => {
                let away = home != Some(tab);
                let back = matches!(self.want, Want::Back { .. });
                match into {
                    Some(into) if sidebar.hidden && back || !sidebar.hidden && away => {
                        let from = home.and_then(|home| scene.placeholder(home)).map(|p| p.id);
                        acts.push(Act::Move { sidebar: sidebar.id, from, into, tab });
                    },
                    _ if !sidebar.hidden && self.want == (Want::Back { focus: true }) => {
                        acts.push(Act::Focus { sidebar: sidebar.id });
                    },
                    _ => {},
                }
            },
        }
        acts
    }

    /// The tab the sidebar is to stand in: that of the client it follows, or
    /// else the one on screen.
    fn target(&self, scene: &Scene) -> TabId {
        self.followed.and_then(|client| scene.seats.get(&client).copied()).unwrap_or(scene.active)
    }

    /// What stands in the column of the tab on screen, when its swap layout is
    /// to be applied again and this dock is the one to ask: its placeholder
    /// came back somewhere else, or hiding marked it.
    fn restore(&self, scene: &Scene) -> Option<PaneId> {
        let tab = scene.active;
        let again = (self.unsettled.contains(&tab) || self.marked.contains(&tab))
            && !scene.floating.contains(&tab);
        if !again || !self.answers(scene) {
            return None;
        }
        match scene.home() {
            Some((at, sidebar)) if at == tab => Some(PaneId::Plugin(sidebar.id)),
            _ => scene.placeholder(tab).filter(|p| !p.hidden).map(|p| PaneId::Terminal(p.id)),
        }
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
        scene.seats.keys().all(|client| *client >= self.client)
    }

    /// Whether this dock's client attached first of those on the tab on
    /// screen. zellij applies a swap layout to the tab on screen for the
    /// client that asks, and one of them asks.
    fn answers(&self, scene: &Scene) -> bool {
        scene
            .seats
            .iter()
            .filter(|(_, tab)| **tab == scene.active)
            .all(|(client, _)| *client >= self.client)
    }

    /// Whether a sidebar is on its way into a placeholder. Halfway, it is
    /// hidden, and its old tab has its placeholder back.
    fn docking(&self) -> bool {
        self.asked.keys().any(|act| matches!(act, Act::Open { .. } | Act::Move { .. }))
    }

    /// Asks zellij for the act, and says whether it did. The sidebar goes
    /// only into a placeholder that still shows, since zellij drops a pane it
    /// is to put in place of a hidden one, and it may have been hidden since
    /// these panes.
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
                let mut configuration = self.sidebar_configuration.clone();
                let focus = self.want == (Want::Back { focus: true });
                if focus {
                    configuration.insert(TAKES_FOCUS.to_owned(), self.client.to_string());
                }
                let opened = open_plugin_pane_floating(url, configuration, None, BTreeMap::new());
                if let Some(PaneId::Plugin(sidebar)) = opened {
                    self.sidebar = Some(sidebar);
                    self.settle(sidebar, into, tab, scene);
                    // `plan` hands it the focus once it stands in its column.
                    if !focus {
                        self.want = Want::Shown;
                    }
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
                self.settle(sidebar, into, tab, scene);
                if self.want == (Want::Back { focus: true }) {
                    focus_plugin_pane(sidebar, false, false);
                }
                self.want = Want::Shown;
            },
            Act::Restore { pane, tab } => {
                self.unsettled.remove(&tab);
                self.marked.remove(&tab);
                mark(pane);
                next_swap_layout();
            },
            Act::Focus { sidebar } => {
                self.want = Want::Shown;
                focus_plugin_pane(sidebar, false, false);
            },
            Act::Close { pane: PaneId::Plugin(id) } => close_plugin_pane(id),
            Act::Close { pane: PaneId::Terminal(id) } => close_terminal_pane(id),
            Act::CloseTab { tab } => close_tab_with_id(tab.0 as u64),
        }
        true
    }

    /// Puts the sidebar in the placeholder of the tab it is to stand in. When
    /// that is the tab on screen, it applies the tab's swap layout again if
    /// the placeholder had come back elsewhere or hiding marked the layout,
    /// unless the tab shows its floating panes; on another client's tab, its
    /// dock does.
    fn settle(&mut self, sidebar: u32, into: u32, tab: TabId, scene: &Scene) {
        let id = PaneId::Plugin(sidebar);
        replace_pane_with_existing_pane(PaneId::Terminal(into), id, true);
        if tab == scene.active && !scene.floating.contains(&tab) {
            let unsettled = self.unsettled.remove(&tab);
            let marked = self.marked.remove(&tab);
            if unsettled || marked {
                mark(id);
                next_swap_layout();
            }
        }
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

/// How zellij's answer on the focused pane begins when it took too long.
const TIMEOUT: &str = "Timeout";

/// Whether this plugin's client is still attached, if zellij says. It says at
/// once that it knows no focused pane for one that left; an answer that times
/// out says only that zellij is busy.
pub fn attached() -> Option<bool> {
    match get_focused_pane_info() {
        Ok(_) => Some(true),
        Err(error) if error.starts_with(TIMEOUT) => None,
        Err(_) => Some(false),
    }
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
            title: TITLE.to_owned(),
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

    /// The panes as client 1's dock reads them.
    fn scene(tabs: &[TabInfo], panes: Vec<(usize, Vec<PaneInfo>)>, known: Option<u32>) -> Scene {
        seen_by(1, tabs, panes, known)
    }

    /// The panes as `client`'s dock reads them, `tabs` being as it sees them.
    fn seen_by(
        client: ClientId,
        tabs: &[TabInfo],
        panes: Vec<(usize, Vec<PaneInfo>)>,
        known: Option<u32>,
    ) -> Scene {
        let manifest = PaneManifest { panes: panes.into_iter().collect::<HashMap<_, _>>() };
        Scene::new(&manifest, tabs, client, DOCK, known).expect("a tab is active")
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

    /// zellij's report of a change, the panes and then the tabs, and what the
    /// dock asks for reading the panes after each: a key can come between the
    /// two. A dock in a test is never granted, so it asks zellij for nothing
    /// itself.
    fn report(dock: &mut Dock, tabs: &[TabInfo], panes: Vec<(usize, Vec<PaneInfo>)>) -> Vec<Act> {
        let read = |dock: &mut Dock| match dock.scene() {
            Some(scene) => run(dock, &scene),
            None => Vec::new(),
        };
        dock.panes(PaneManifest { panes: panes.into_iter().collect() });
        let mut acts = read(dock);
        dock.tabs(tabs.to_vec());
        acts.extend(read(dock));
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
        assert_eq!(dock().plan(&scene), vec![Act::Open { into: 1, tab: TabId(10) }]);
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
        let moved = Act::Move { sidebar: 5, from: Some(1), into: 3, tab: TabId(11) };
        assert_eq!(dock().plan(&scene), vec![moved]);
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

    /// Three tabs, a shell and a placeholder in each, and the sidebar in
    /// `docked` over its placeholder.
    fn three_tabs(docked: usize) -> Vec<(usize, Vec<PaneInfo>)> {
        let mut panes = vec![
            (0, vec![placeholder(1, false), terminal(2)]),
            (1, vec![placeholder(3, false), terminal(4)]),
            (2, vec![placeholder(5, false), terminal(6)]),
        ];
        let (_, column) = &mut panes[docked];
        column[0].is_suppressed = true;
        column.push(sidebar(9, false));
        panes
    }

    #[test]
    fn the_sidebar_goes_to_the_tab_a_client_came_to_last() {
        let seen = |at: &[(ClientId, usize)], docked| {
            seen_by(1, &tabs_of(1, at, 3), three_tabs(docked), None)
        };
        let mut dock = going(1);
        assert!(run(&mut dock, &seen(&[(1, 0), (2, 0)], 0)).is_empty());
        // Client 2 comes to tab 1, and client 1 stays on tab 0.
        assert_eq!(
            run(&mut dock, &seen(&[(1, 0), (2, 1)], 0)),
            vec![Act::Move { sidebar: 9, from: Some(1), into: 3, tab: TabId(11) }],
        );
        assert!(run(&mut dock, &seen(&[(1, 0), (2, 1)], 1)).is_empty());
    }

    #[test]
    fn only_the_dock_that_leads_moves_the_sidebar() {
        let seen = |client, at: &[(ClientId, usize)], docked| {
            seen_by(client, &tabs_of(client, at, 3), three_tabs(docked), None)
        };
        let (mut first, mut second) = (going(1), going(2));
        let mut both = |at: &[(ClientId, usize)], docked| {
            (run(&mut first, &seen(1, at, docked)), run(&mut second, &seen(2, at, docked)))
        };
        assert_eq!(both(&[(1, 0), (2, 0)], 0), (vec![], vec![]));
        // Client 1 comes to tab 1, and client 2, which stays, leaves the
        // sidebar to go with it.
        let moved = Act::Move { sidebar: 9, from: Some(1), into: 3, tab: TabId(11) };
        assert_eq!(both(&[(1, 1), (2, 0)], 0), (vec![moved], vec![]));
        assert_eq!(both(&[(1, 1), (2, 0)], 1), (vec![], vec![]));
        // Both come to another tab at once, off the same panes: the first to
        // attach takes the sidebar with it.
        let moved = Act::Move { sidebar: 9, from: Some(3), into: 1, tab: TabId(10) };
        assert_eq!(both(&[(1, 0), (2, 2)], 1), (vec![moved], vec![]));
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
                    (0, vec![sidebar(5, true), placeholder(1, true), terminal(2)]),
                    (1, vec![placeholder(3, !placeholder_shown), terminal(4)]),
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
            vec![
                Act::Show { pane: 1 },
                Act::Move { sidebar: 5, from: None, into: 3, tab: TabId(11) },
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
                (0, vec![sidebar(5, true), placeholder(1, false), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
            ],
            None,
        );
        let moved = Act::Move { sidebar: 5, from: Some(1), into: 3, tab: TabId(11) };
        let mut dock = dock();
        dock.asked.insert(moved, 0);
        assert!(!halfway.shows(moved));
        assert!(dock.plan(&halfway).is_empty());
        let done = scene(
            &tabs,
            vec![
                (0, vec![placeholder(1, false), terminal(2)]),
                (1, vec![sidebar(5, false), placeholder(3, true), terminal(4)]),
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
                (0, vec![sidebar(5, true), placeholder(1, false), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
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
                (0, vec![placeholder(1, false), placeholder(2, true), terminal(4)]),
                (1, vec![placeholder(3, true), terminal(6)]),
            ]
        };
        let first = seen_by(1, &tabs_of(1, &at, 2), panes(), None);
        let second = seen_by(2, &tabs_of(2, &at, 2), panes(), None);
        assert_eq!(
            going(1).plan(&first),
            vec![
                Act::Close { pane: PaneId::Terminal(2) },
                Act::Show { pane: 3 },
                Act::Open { into: 1, tab: TabId(10) },
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
            seen_by(
                client,
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

    /// `toggle` in one session hides the sidebar of every other, and brings
    /// it back. A dock takes up only what another session said: the key its
    /// own heard reaches each of its docks, and one that took it up from the
    /// file first would undo it. A session that starts while the sidebar is
    /// hidden starts with it hidden.
    #[test]
    fn hiding_in_one_session_hides_the_sidebar_of_every_other() {
        let file = std::env::temp_dir().join(format!("luneta-hidden-{}", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let of = |server, dock: Dock| Dock { server, hiding: Hiding::at(file.clone()), ..dock };
        // Session 7 has two clients, session 8 one.
        let (mut here, mut beside, mut there) = (of(7, going(1)), of(7, going(2)), of(8, going(1)));

        here.pipe(Ask::Toggle);
        beside.tick();
        there.tick();
        assert_eq!(there.want, Want::Hidden);
        // The key comes to the second client's dock after its tick.
        assert_eq!(beside.want, Want::Shown);
        beside.pipe(Ask::Toggle);
        assert_eq!(beside.want, Want::Hidden);

        let mut later = of(9, dock());
        let tabs = [tab(0, true)];
        let docked = vec![(0, vec![sidebar(5, false), placeholder(1, true), terminal(2)])];
        run(&mut later, &scene(&tabs, docked, None));
        assert_eq!(later.want, Want::Hidden);

        there.pipe(Ask::Toggle);
        here.tick();
        assert_eq!(here.want, Want::Back { focus: false });

        std::fs::remove_file(file).unwrap();
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
            seen_by(
                client,
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![sidebar(5, false), placeholder(1, true), terminal(2)]),
                    (1, vec![placeholder(3, false), terminal(4)]),
                ],
                None,
            )
        };
        let closed = |client| {
            seen_by(
                client,
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![placeholder(1, true), terminal(2)]),
                    (1, vec![placeholder(3, false), terminal(4)]),
                ],
                None,
            )
        };
        let (mut first, mut second) = (going(1), going(2));
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

    /// A sidebar lost with its tab opens again, and takes the focus once it
    /// shows if it had it, with no floating pane over it: you were in it as
    /// the tab closed.
    #[test]
    fn a_sidebar_lost_with_its_tab_opens_again() {
        for (focused, floating, takes) in
            [(false, false, false), (true, false, true), (true, true, false)]
        {
            let case = format!("focused: {focused}, floating: {floating}");
            let mut dock = going(1);
            let mut in_tab = vec![
                PaneInfo { is_focused: focused, ..sidebar(5, false) },
                placeholder(3, true),
                terminal(4),
            ];
            if floating {
                in_tab.push(PaneInfo { is_floating: true, is_focused: true, ..terminal(9) });
            }
            report(
                &mut dock,
                &[
                    tab(0, false),
                    TabInfo { are_floating_panes_visible: floating, ..tab(1, true) },
                    tab(2, false),
                ],
                vec![
                    (0, vec![placeholder(1, false), terminal(2)]),
                    (1, in_tab),
                    (2, vec![placeholder(7, false), terminal(8)]),
                ],
            );
            // Tab 1 closes, and with it the sidebar. Its hidden placeholder
            // goes to the first tab, and tab 2 takes its place.
            let tabs = [tab(0, false), TabInfo { tab_id: 12, ..tab(1, true) }];
            let left = report(
                &mut dock,
                &tabs,
                vec![
                    (0, vec![placeholder(1, false), terminal(2), placeholder(3, true)]),
                    (1, vec![placeholder(7, false), terminal(8)]),
                ],
            );
            assert_eq!(
                left,
                vec![
                    Act::Close { pane: PaneId::Terminal(3) },
                    Act::Open { into: 7, tab: TabId(12) },
                ],
                "{case}",
            );
            let opened = report(
                &mut dock,
                &tabs,
                vec![
                    (0, vec![placeholder(1, false), terminal(2)]),
                    (1, vec![sidebar(6, false), placeholder(7, true), terminal(8)]),
                ],
            );
            let focus = if takes { vec![Act::Focus { sidebar: 6 }] } else { Vec::new() };
            assert_eq!(opened, focus, "{case}");
        }
    }

    /// The last pane you were in closes, and zellij hands its focus to the
    /// sidebar, the one pane left that takes it. The dock closes the tab, and
    /// the sidebar opens again without the focus: you were not in it.
    #[test]
    fn a_sidebar_handed_the_focus_of_the_last_pane_opens_again_without_it() {
        let mut dock = going(1);
        let tabs = [tab(0, false), tab(1, true), tab(2, false)];
        let in_tab = |sidebar_focused, shell: bool| {
            let mut panes = vec![
                PaneInfo { is_focused: sidebar_focused, ..sidebar(5, false) },
                placeholder(3, true),
            ];
            if shell {
                panes.push(PaneInfo { is_focused: true, ..terminal(4) });
            }
            vec![
                (0, vec![placeholder(1, false), terminal(2)]),
                (1, panes),
                (2, vec![placeholder(7, false), terminal(8)]),
            ]
        };
        report(&mut dock, &tabs, in_tab(false, true));
        let emptied = report(&mut dock, &tabs, in_tab(true, false));
        assert_eq!(emptied, vec![Act::CloseTab { tab: TabId(11) }]);
        let tabs = [tab(0, false), TabInfo { tab_id: 12, ..tab(1, true) }];
        let left = report(
            &mut dock,
            &tabs,
            vec![
                (0, vec![placeholder(1, false), terminal(2), placeholder(3, true)]),
                (1, vec![placeholder(7, false), terminal(8)]),
            ],
        );
        assert_eq!(
            left,
            vec![Act::Close { pane: PaneId::Terminal(3) }, Act::Open { into: 7, tab: TabId(12) }],
        );
        assert_eq!(dock.want, Want::Shown);
        let opened = report(
            &mut dock,
            &tabs,
            vec![
                (0, vec![placeholder(1, false), terminal(2)]),
                (1, vec![sidebar(6, false), placeholder(7, true), terminal(8)]),
            ],
        );
        assert_eq!(opened, Vec::new());
    }

    #[test]
    fn a_tab_left_with_nothing_but_the_column_closes() {
        // The last shell is gone from tab 0, where the sidebar stands, and
        // from tab 2, where client 1 is, which keeps only a floating one.
        // Client 2 has just come to tab 1, but the sidebar goes with its tab.
        let at = [(1, 2), (2, 1)];
        let panes = || {
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true)]),
                (1, vec![placeholder(3, false), terminal(4)]),
                (2, vec![placeholder(6, false), PaneInfo { is_floating: true, ..terminal(7) }]),
            ]
        };
        let first = seen_by(1, &tabs_of(1, &at, 3), panes(), None);
        assert_eq!(
            going(1).plan(&first),
            vec![Act::CloseTab { tab: TabId(10) }, Act::CloseTab { tab: TabId(12) }],
        );
        assert!(going(2).plan(&seen_by(2, &tabs_of(2, &at, 3), panes(), None)).is_empty());
    }

    #[test]
    fn a_sidebar_being_opened_is_not_lost() {
        let tabs = [tab(0, true)];
        let scene = scene(&tabs, vec![(0, vec![placeholder(1, false)])], None);
        let mut dock = dock();
        dock.sidebar = Some(5);
        dock.asked.insert(Act::Open { into: 1, tab: TabId(10) }, 0);
        dock.observe(&scene);
        assert_eq!(dock.sidebar, Some(5));
    }

    #[test]
    fn a_dock_that_waits_for_the_sidebar_to_come_back_sees_it_shown() {
        let at = [(1, 0), (2, 1)];
        let mut second = going(2);
        second.want = Want::Back { focus: true };
        let back = seen_by(
            2,
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
        let shown = scene(
            &tabs,
            vec![(0, vec![sidebar(5, false), placeholder(1, true), terminal(2)])],
            None,
        );
        let mut dock = going(1);
        dock.want = Want::Back { focus: true };
        assert_eq!(run(&mut dock, &shown), vec![Act::Focus { sidebar: 5 }]);
    }

    #[test]
    fn a_placeholder_back_elsewhere_in_the_tab_on_screen_is_put_back() {
        // Client 1 keeps the sidebar in tab 0; client 2 looks at tab 1.
        let scene = seen_by(
            2,
            &tabs_of(2, &[(1, 0), (2, 1)], 2),
            vec![
                (0, vec![sidebar(5, false), placeholder(1, true), terminal(2)]),
                (1, vec![placeholder(3, false), terminal(4)]),
            ],
            None,
        );
        let mut dock = going(2);
        dock.unsettled.insert(TabId(11));
        let restore = Act::Restore { pane: PaneId::Terminal(3), tab: TabId(11) };
        assert_eq!(dock.plan(&scene), vec![restore]);
    }

    #[test]
    fn of_the_clients_on_a_tab_only_the_first_to_attach_puts_it_back() {
        let at = [(1, 0), (2, 1), (3, 1)];
        let seen = |client| {
            seen_by(
                client,
                &tabs_of(client, &at, 2),
                vec![
                    (0, vec![placeholder(1, false), terminal(2)]),
                    (1, vec![sidebar(5, false), placeholder(3, true), terminal(4)]),
                ],
                None,
            )
        };
        let unsettled = |client| {
            let mut dock = going(client);
            dock.unsettled.insert(TabId(11));
            dock.plan(&seen(client))
        };
        let restore = Act::Restore { pane: PaneId::Plugin(5), tab: TabId(11) };
        assert_eq!(unsettled(2), vec![restore]);
        assert!(unsettled(3).is_empty());
    }

    #[test]
    fn a_tab_whose_swap_layout_hiding_marked_gets_it_back_on_screen() {
        // Client 1 keeps the sidebar in tab 0, and client 2 looks at the
        // others. Tab 1's swap layout is as zellij left it; tab 2's was
        // changed by hand.
        let tabs = |on, dirty: [bool; 3]| {
            let mut tabs = tabs_of(2, &[(1, 0), (2, on)], 3);
            for (tab, dirty) in tabs.iter_mut().zip(dirty) {
                tab.active_swap_layout_name = Some("vertical".to_owned());
                tab.is_swap_layout_dirty = dirty;
            }
            tabs
        };
        let panes = |hidden| {
            vec![
                (0, vec![sidebar(5, hidden), placeholder(1, true), terminal(2)]),
                (1, vec![placeholder(3, hidden), terminal(4)]),
                (2, vec![placeholder(6, hidden), terminal(7)]),
            ]
        };
        let seen = |tabs: &[TabInfo], hidden| seen_by(2, tabs, panes(hidden), None);
        let mut dock = going(2);
        let shown = seen(&tabs(1, [false, false, true]), false);
        run(&mut dock, &shown);
        dock.want = Want::Hidden;
        run(&mut dock, &shown);
        // Hidden, each tab holds one pane, and zellij names no layout for it.
        let hidden = seen(&tabs_of(2, &[(1, 0), (2, 1)], 3), true);
        run(&mut dock, &hidden);
        dock.want = Want::Back { focus: false };
        run(&mut dock, &hidden);
        // Back, with every tiled layout marked. While tab 1 shows its floating
        // panes, zellij reports their layout, and would apply theirs.
        let mut floating = tabs(1, [true, false, true]);
        floating[1].are_floating_panes_visible = true;
        assert!(run(&mut dock, &seen(&floating, false)).is_empty());
        let back = |on| seen(&tabs(on, [true; 3]), false);
        assert_eq!(
            run(&mut dock, &back(1)),
            vec![Act::Restore { pane: PaneId::Terminal(3), tab: TabId(11) }],
        );
        assert!(run(&mut dock, &back(2)).is_empty());
    }

    #[test]
    fn a_tab_whose_sidebar_was_closed_by_hand_gets_its_swap_layout_back_on_screen() {
        // One shell beside the sidebar: once it closes, the tab holds one
        // pane, and zellij names no layout for it.
        let layout = |dirty| TabInfo {
            active_swap_layout_name: Some("vertical".to_owned()),
            is_swap_layout_dirty: dirty,
            ..tab(0, true)
        };
        let mut dock = going(1);
        let docked = vec![(0, vec![sidebar(5, false), placeholder(1, true), terminal(2)])];
        run(&mut dock, &scene(&[layout(false)], docked, None));
        let closed = vec![(0, vec![placeholder(1, true), terminal(2)])];
        run(&mut dock, &scene(&[tab(0, true)], closed, None));
        assert_eq!(dock.want, Want::Hidden);
        dock.want = Want::Back { focus: false };
        let back = vec![(0, vec![placeholder(1, false), terminal(2)])];
        assert_eq!(
            run(&mut dock, &scene(&[layout(true)], back, None)),
            vec![
                Act::Restore { pane: PaneId::Terminal(1), tab: TabId(10) },
                Act::Open { into: 1, tab: TabId(10) },
            ],
        );
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
            assert_eq!(dock.unsettled, BTreeSet::from([TabId(10)]));
            dock.observe(&back(COLUMN));
            assert!(dock.unsettled.is_empty());
        }
    }

    /// The picker, floating over the tab, has the sidebar's title and is no
    /// extra sidebar.
    #[test]
    fn extra_sidebars_and_placeholders_close() {
        let tabs = [tab(0, true)];
        let picker = PaneInfo { is_floating: true, ..sidebar(9, false) };
        let panes = vec![
            sidebar(7, false),
            sidebar(5, false),
            placeholder(1, true),
            placeholder(2, true),
            terminal(3),
            picker,
        ];
        let scene = scene(&tabs, vec![(0, panes)], None);
        assert_eq!(
            dock().plan(&scene),
            vec![Act::Close { pane: PaneId::Plugin(7) }, Act::Close { pane: PaneId::Terminal(2) }],
        );
    }

    /// A picker opened in place of a tab's only terminal stands in the tab's
    /// columns, and holds the tab open as the terminal it hides did. It is no
    /// extra sidebar: the dock closes neither it nor the tab.
    #[test]
    fn a_picker_that_does_not_float_is_no_sidebar() {
        let tabs = [tab(0, true), tab(1, false)];
        let opened = PaneInfo {
            id: 9,
            is_plugin: true,
            is_selectable: true,
            title: "(/w) - luneta".to_owned(),
            ..PaneInfo::default()
        };
        let title = picker_title(&opened).expect("the picker titles its pane");
        let picker = PaneInfo { title: title.to_owned(), ..opened };
        let hidden = PaneInfo { is_suppressed: true, ..terminal(3) };
        let panes = vec![sidebar(5, false), placeholder(1, true), hidden, picker];
        let scene = scene(
            &tabs,
            vec![(0, panes), (1, vec![placeholder(2, false), terminal(4)])],
            None,
        );
        assert_eq!(dock().plan(&scene), Vec::new());
    }

    /// The focus lands only in a sidebar that stands in its column: zellij
    /// floats a hidden pane it is asked to focus. Of two, the dock keeps the
    /// one with the lower id.
    #[test]
    fn only_a_sidebar_in_its_column_stands() {
        let floating = PaneInfo {
            is_floating: true,
            title: FLOATING_SIDEBAR_TITLE.to_owned(),
            ..sidebar(4, false)
        };
        for (panes, standing_one) in [
            (vec![sidebar(5, false), placeholder(1, true), terminal(2)], Some(5)),
            (vec![sidebar(5, true), placeholder(1, false), terminal(2)], None),
            (vec![floating, placeholder(1, false), terminal(2)], None),
            (vec![sidebar(7, false), sidebar(5, false), terminal(2)], Some(5)),
        ] {
            let manifest = PaneManifest { panes: HashMap::from([(0, panes.clone())]) };
            assert_eq!(standing(&manifest), standing_one, "{panes:?}");
        }
    }

    /// A sidebar floated out of its column by hand is still the sidebar to a
    /// dock that never saw it docked, as that of a client that attached since:
    /// no second one opens.
    #[test]
    fn a_sidebar_floated_by_hand_is_still_the_sidebar() {
        let tabs = [tab(0, true)];
        let floated = PaneInfo { is_floating: true, ..sidebar(5, false) };
        let title = sidebar_title(&floated).expect("the sidebar titles its pane");
        let floating = PaneInfo { title: title.to_owned(), ..floated };
        let panes = vec![floating, placeholder(1, false), terminal(2)];
        let scene = scene(&tabs, vec![(0, panes)], None);
        assert_eq!(dock().plan(&scene), Vec::new());
    }

    #[test]
    fn a_floating_sidebar_is_one_still_being_docked() {
        let tabs = [tab(0, true)];
        let floating = PaneInfo {
            is_floating: true,
            title: "file:luneta.wasm".to_owned(),
            ..sidebar(5, false)
        };
        let scene =
            scene(&tabs, vec![(0, vec![floating, placeholder(1, false), terminal(2)])], Some(5));
        assert!(dock().plan(&scene).is_empty());
    }
}
