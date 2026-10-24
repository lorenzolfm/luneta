mod agents;
mod brief;
mod checkouts;
mod close;
mod coming;
mod context;
mod cursor;
mod dirs;
mod dock;
mod elapsed;
mod fetch;
mod folds;
mod hiding;
mod icons;
mod layout;
mod ledger;
mod menu;
mod news;
mod paint;
mod panes;
mod pass;
mod places;
mod render;
mod sessions;
mod shells;
mod skills;
mod tree;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentRow, AgentSet, Live, Seat};
use brief::{Brief, Over, Scope};
use checkouts::Checkouts;
use context::CONTEXT_KEY;
use dirs::{DirSet, LIST};
use dock::{Ask, TabId};
use menu::{Command, Did, RowPane, SidebarMenu, There, Top};
use elapsed::{Age, Moment};
use folds::Folds;
use panes::Peeks;
use pass::{At, Pass, Then};
use places::Places;
use sessions::{Contents, Focus, Kind, MatchSet, Selection, Session, SessionName, Sessions};
use tree::{is_placeholder, origin, Branch, Leaf, Node, Place, Tree};
use zellij_tile::prelude::*;

const SCREEN_PIPE: &str = "screen";

/// The pipe a key binding reaches the sidebar through, its payload an `Ask`.
const SIDEBAR_PIPE: &str = "sidebar";

/// The pipe the sidebar and the picker hand each other the row they have
/// highlighted through, as the focus goes from one to the other, its
/// arguments a `Place`.
const PLACE_PIPE: &str = "place";

/// `mode "dock"`, in `load_plugins`, makes the plugin the dock: one to a
/// session, in the background, keeping a sidebar in the tab on screen. The
/// sidebar it opens, `mode "sidebar"`, is a pane that never closes itself.
const MODE: &str = "mode";

const SIDEBAR: &str = "sidebar";

const DOCK: &str = "dock";

/// `mode "menu"` makes the plugin the menu: keys, pressed one after another,
/// that start an agent.
const MENU: &str = "menu";

/// What a load of the plugin is, by its `mode`.
#[derive(Default)]
enum Role {
    /// No `mode`: the picker, floating over the tab.
    #[default]
    Picker,
    Sidebar(Tree),
    Dock(dock::Dock),
    /// Boxed: the menu keeps what claude-ps says, for a delete key to ask
    /// first, and is larger than every other role.
    Menu(Box<menu::Menu>),
    /// A `mode` luneta does not know. It says so, and does nothing else.
    Unknown(String),
}

/// What a load with a `mode` luneta does not know says, in its pane if it has
/// one, and in zellij's log.
fn unknown_mode(mode: &str) -> String {
    format!(
        "luneta has no mode {mode:?}: \"{SIDEBAR}\", \"{DOCK}\", \"{MENU}\", or none for the picker"
    )
}

/// Every mode asks for the same. zellij keeps one grant for a plugin's file,
/// and each load puts what it asks for in the grant's place, whether zellij
/// prompts or not: a mode that asked for less would take the rest from the
/// others. `OpenTerminalsOrPlugins` is the dock's, to open the sidebar.
const PERMISSIONS: [PermissionType; 4] = [
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::OpenTerminalsOrPlugins,
];

const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

const TICK: f64 = 0.1;

/// How often, in seconds, the dock takes up a sidebar hidden or brought back
/// in another session.
const DOCK_TICK: f64 = 1.0;

const TICKS_PER_SECOND: u64 = 10;

const TICKS_PER_POLL: u64 = TICKS_PER_SECOND;

const PREVIEW_DELAY: u64 = 2;

/// How often the sidebar asks each session for its panes again, so that an
/// agent started in a new pane, or in a shell that moved, finds its row, and
/// git how the work tree of each stands, so that a change or a checkout shows.
const TICKS_PER_PLACES: u64 = 5 * TICKS_PER_SECOND;

/// How long, in ticks, keys may take to come after the sidebar's menu starts
/// an agent in its own session: zellij hands the sidebar the keys it took
/// before it moved the focus to the agent's pane.
const ON_THE_WAY: u64 = TICKS_PER_SECOND / 2;

#[derive(Default)]
pub enum Mode {
    #[default]
    Search,
    Rename { current: String, input: String },
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum Screen {
    #[default]
    Sessions,
    Dirs,
    Agents,
}

impl Screen {
    fn next(self) -> Self {
        match self {
            Screen::Sessions => Screen::Agents,
            Screen::Agents => Screen::Dirs,
            Screen::Dirs => Screen::Sessions,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name {
            "sessions" => Some(Screen::Sessions),
            "agents" => Some(Screen::Agents),
            "dirs" | "directories" => Some(Screen::Dirs),
            _ => None,
        }
    }

    fn prev(self) -> Self {
        match self {
            Screen::Sessions => Screen::Dirs,
            Screen::Agents => Screen::Sessions,
            Screen::Dirs => Screen::Agents,
        }
    }
}

#[derive(PartialEq, Eq)]
enum Target {
    Dir(String),
    Pane(String, u32),
}

#[derive(Default)]
struct State {
    mode: Mode,
    screen: Screen,
    matches: MatchSet,
    dirs: DirSet,
    agents: AgentSet,
    agents_command: Option<String>,
    panes: Option<PaneManifest>,
    live_names: Vec<String>,
    places: Places,
    checkouts: Checkouts,
    active_tab: Option<usize>,
    sessions: Sessions,
    error: Option<String>,
    frame: u64,
    peeks: Peeks,
    preview_at: Option<(Target, u64)>,
    agents_taken_at: u64,
    role: Role,
    plugin_id: u32,
    /// This plugin's client, which the sidebar's menu takes out of the
    /// session before a delete key kills it.
    client_id: ClientId,
    tabs: Vec<TabInfo>,
    branches: Vec<Branch>,
    /// The sidebar of each other session, by its name, while it stands in its
    /// column: where the focus lands as the sidebar's menu takes you there
    /// and kills the session you are in.
    sidebars: BTreeMap<String, u32>,
    here: Option<u32>,
    away: bool,
    places_at: u64,
    size: (usize, usize),
    pass: Pass,
    /// The row the sidebar had highlighted when the focus came to the
    /// picker, for the lists to land on as they come, until a key moves them.
    handed: Option<Place>,
    /// Whether `focus` came while the picker showed, for the sidebar. The
    /// picker closes once the sidebar has the focus, which hides it.
    leaving: bool,
    /// Where the pointer last stood on the sidebar, line and column.
    pointer: Option<(usize, usize)>,
    /// What was under the pointer when its hover ended, as a click that only
    /// focuses the sidebar ends it: the row, and whether on its fold.
    pressed: Option<(Place, bool)>,
    /// The menu's entries, as the configuration holds them, for the sidebar
    /// to open the menu at its bottom with.
    menu_entries: Option<String>,
    /// How many menus the sidebar has opened, which numbers each: the answer
    /// to an agent one starts in another session says which it is for.
    menus: menu::Numbers,
    /// The brief asked for last, while the sidebar reads the end of each
    /// transcript it covers.
    brief: Option<Brief>,
    /// How many briefs the sidebar has asked for, which numbers each: the
    /// ends of the transcripts say which they are for.
    briefs: u64,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.plugin_id = ids.plugin_id;
        self.client_id = ids.client_id;
        request_permission(&PERMISSIONS);
        self.role = match configuration.get(MODE).map(|value| value.trim()) {
            None => Role::Picker,
            Some(SIDEBAR) => {
                let entries = configuration.get(menu::ENTRIES).map(String::as_str);
                let (icons, wrong) =
                    icons::configured(configuration.get(icons::KEY).map(String::as_str));
                if let Some(wrong) = &wrong {
                    eprintln!("luneta {wrong}");
                }
                // The dock opened it for the focus of its client that comes
                // next, which it would send on otherwise: no key asked it of
                // this sidebar, which was not there to hear one.
                let client = ids.client_id.to_string();
                if configuration.get(dock::TAKES_FOCUS).is_some_and(|dock| dock.trim() == client) {
                    self.pass.ask(self.frame);
                }
                Role::Sidebar(Tree::shared(menu::Legend::new(entries), icons, wrong))
            },
            Some(DOCK) => {
                let mut sidebar = configuration.clone();
                sidebar.insert(MODE.to_owned(), SIDEBAR.to_owned());
                let dock = dock::Dock::new(self.plugin_id, ids.client_id, sidebar);
                Role::Dock(dock.shared(ids.zellij_pid))
            },
            Some(MENU) => {
                Role::Menu(Box::new(menu::Menu::new(self.plugin_id, ids.client_id, &configuration)))
            },
            Some(mode) => Role::Unknown(mode.to_owned()),
        };
        match &self.role {
            Role::Picker | Role::Sidebar(_) => {},
            Role::Dock(_) => {
                subscribe(&[
                    EventType::PermissionRequestResult,
                    EventType::TabUpdate,
                    EventType::PaneUpdate,
                    EventType::Timer,
                ]);
                set_timeout(DOCK_TICK);
                return;
            },
            Role::Menu(_) => {
                subscribe(&[
                    EventType::Key,
                    EventType::TabUpdate,
                    EventType::PaneUpdate,
                    EventType::RunCommandResult,
                    EventType::Timer,
                ]);
                return;
            },
            Role::Unknown(mode) => {
                eprintln!("{}", unknown_mode(mode));
                return;
            },
        }
        self.agents_command = agents::configured(&configuration);
        self.menu_entries = configuration.get(menu::ENTRIES).cloned();
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Timer,
            EventType::Key,
            EventType::RunCommandResult,
            EventType::Visible,
            EventType::PaneUpdate,
            EventType::TabUpdate,
        ]);
        if self.is_sidebar() {
            // zellij says `InputReceived` of every key, whichever pane it goes
            // to: `Pass` tells it from a click.
            subscribe(&[EventType::Mouse, EventType::InputReceived]);
        }
        set_timeout(0.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match self.role {
            Role::Picker | Role::Sidebar(_) => {},
            Role::Dock(ref mut dock) => {
                match event {
                    Event::PermissionRequestResult(PermissionStatus::Granted) => dock.grant(),
                    Event::TabUpdate(tabs) => dock.tabs(tabs),
                    Event::PaneUpdate(panes) => dock.panes(panes),
                    Event::Timer(_) => {
                        set_timeout(DOCK_TICK);
                        dock.tick();
                    },
                    _ => {},
                }
                return false;
            },
            Role::Menu(ref mut menu) => {
                return match event {
                    Event::Key(key) => menu.key(&key),
                    Event::TabUpdate(tabs) => menu.tabs(tabs),
                    Event::PaneUpdate(panes) => menu.panes(panes),
                    Event::Timer(_) => menu.tick(),
                    Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                        menu.ran(exit_code, &stdout, &stderr, &context)
                    },
                    _ => false,
                };
            },
            Role::Unknown(_) => return false,
        }
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    if let Some(pane) = get_pane_info(PaneId::Plugin(self.plugin_id)) {
                        self.retitle(&pane);
                    }
                    if !self.is_sidebar() {
                        resize_self(self.plugin_id);
                        self.ask_zoxide();
                    }
                    self.ask_agents();
                } else {
                    self.dirs.fail("permission denied");
                    self.agents.fail("permission denied");
                }
                true
            },
            Event::Timer(_) => {
                set_timeout(TICK);
                let polled = self.frame.is_multiple_of(TICKS_PER_POLL);
                if polled {
                    self.poll();
                }
                self.frame = self.frame.wrapping_add(1);
                let asked = self.follow_preview();
                let left = self.leave_session();
                let settled = self.settle_menu();
                polled || self.spinning() || asked || left || settled
            },
            Event::Key(key) if self.is_sidebar() => self.handle_sidebar_key(&key),
            Event::Key(key) => self.handle_key(&key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::InputReceived => {
                self.pass.input();
                false
            },
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                match context.get(CONTEXT_KEY).map(String::as_str) {
                    Some(dirs::CONTEXT_VALUE) => {
                        self.dirs.ingest(exit_code, &stdout, &stderr);
                        if let Some(command) = self.dirs.worktrees_query() {
                            run_command(
                                &command,
                                BTreeMap::from([(
                                    CONTEXT_KEY.to_owned(),
                                    dirs::WORKTREES_VALUE.to_owned(),
                                )]),
                            );
                        }
                        self.rebuild_dirs(Selection::Hold);
                        true
                    },
                    Some(dirs::WORKTREES_VALUE) => {
                        self.dirs.ingest_worktrees(exit_code, &stdout);
                        self.rebuild_dirs(Selection::Hold);
                        true
                    },
                    Some(dirs::PREVIEW_VALUE) => match context.get(dirs::PATH_KEY) {
                        Some(path) => {
                            self.dirs.ingest_listing(path.clone(), exit_code, &stdout, &stderr);
                            true
                        },
                        None => false,
                    },
                    Some(panes::CONTEXT_VALUE) => {
                        match context.get(panes::PANE_KEY).and_then(|k| panes::parse_key(k)) {
                            Some(key) => {
                                self.peeks.ingest(key, exit_code, &stdout, &stderr);
                                true
                            },
                            None => false,
                        }
                    },
                    Some(checkouts::CONTEXT_VALUE) => match context.get(checkouts::DIR_KEY) {
                        Some(dir) => {
                            self.checkouts.ingest(dir.clone(), exit_code, &stdout, &stderr);
                            self.rebuild_tree();
                            true
                        },
                        None => false,
                    },
                    Some(places::CONTEXT_VALUE) => {
                        match context.get(places::SESSION_KEY) {
                            Some(session) => {
                                self.places.ingest(session.clone(), exit_code, &stdout);
                                self.rebuild_agents(Selection::Hold);
                                true
                            },
                            None => false,
                        }
                    },
                    Some(shells::CONTEXT_VALUE) => {
                        let Role::Sidebar(tree) = &mut self.role else {
                            return false;
                        };
                        tree.news.shells.ingest(exit_code, &stdout);
                        self.rebuild_tree();
                        true
                    },
                    Some(agents::CONTEXT_VALUE) => {
                        self.agents.ingest(exit_code, &stdout, &stderr, Moment::now());
                        self.agents_taken_at = self.frame;
                        self.rebuild_agents(Selection::Hold);
                        true
                    },
                    Some(brief::CONTEXT_VALUE) => {
                        let number = context.get(brief::NUMBER_KEY);
                        let asked = |brief: &mut Brief| number == Some(&brief.number.to_string());
                        let Some(brief) = self.brief.take_if(asked) else {
                            return false;
                        };
                        if exit_code == Some(0) {
                            brief.open(&stdout);
                            return false;
                        }
                        // A brief on transcripts that were never read would
                        // tell of none: the sidebar says why where its keys
                        // were.
                        let Role::Sidebar(tree) = &mut self.role else {
                            return false;
                        };
                        let said = String::from_utf8_lossy(&stderr);
                        let reason = match said.lines().next().map(str::trim) {
                            Some(said) if !said.is_empty() => {
                                format!("brief: the transcripts could not be read: {said}")
                            },
                            _ => "brief: the transcripts could not be read".to_owned(),
                        };
                        tree.menu = Some(SidebarMenu::wrong(reason, &mut self.menus));
                        true
                    },
                    Some(
                        menu::START_VALUE
                        | menu::OPENING_VALUE
                        | menu::CLOSING_VALUE
                        | menu::NAMING_VALUE
                        | menu::ARRIVING_VALUE,
                    ) => {
                        let Role::Sidebar(tree) = &mut self.role else {
                            return false;
                        };
                        let taken = self.sessions.taken(self.matches.current_session.as_deref());
                        let answered = |menu: SidebarMenu| {
                            menu.answered(exit_code, &stdout, &stderr, &context, &taken)
                        };
                        tree.menu = tree.menu.take().and_then(answered);
                        true
                    },
                    Some(skills::CONTEXT_VALUE) => {
                        let Role::Sidebar(tree) = &mut self.role else {
                            return false;
                        };
                        let found =
                            |menu: SidebarMenu| menu.found(exit_code, &stdout, &stderr, &context);
                        tree.menu = tree.menu.take().map(found);
                        true
                    },
                    Some(close::CLIENTS_VALUE) => self.listed(exit_code, &stdout, &context),
                    // The menu that took you to the session is gone: the
                    // sidebar you come back to says why no tab opened.
                    Some(menu::TAB_VALUE) if exit_code != Some(0) => {
                        let session = context.get(places::SESSION_KEY).map_or("", String::as_str);
                        let reason = String::from_utf8_lossy(&stderr);
                        let reason = reason.lines().next().unwrap_or("").trim();
                        let reason = if reason.is_empty() { "zellij opened none" } else { reason };
                        self.error = Some(format!("new tab in \"{session}\": {reason}"));
                        true
                    },
                    Some(agents::CLOSE_VALUE) if exit_code != Some(0) => {
                        let label = context.get(agents::LABEL_KEY).map_or("", String::as_str);
                        let reason = String::from_utf8_lossy(&stderr);
                        let reason = reason.lines().next().unwrap_or("").trim();
                        let reason =
                            if reason.is_empty() { "the pane did not close" } else { reason };
                        self.error = Some(format!("kill \"{label}\": {reason}"));
                        // Brings back the agent the pane still holds.
                        self.ask_agents();
                        true
                    },
                    _ => false,
                }
            },
            Event::PaneUpdate(manifest) => {
                if let Role::Sidebar(tree) = &mut self.role {
                    tree.menu = tree.menu.take().and_then(|menu| menu.panes(&manifest));
                }
                self.panes = Some(manifest);
                if let Some(pane) = self.own_pane() {
                    self.retitle(pane);
                }
                self.settle()
            },
            Event::TabUpdate(tabs) => {
                self.active_tab = tabs.iter().find(|t| t.active).map(|t| t.position);
                self.tabs = tabs;
                self.pass_over();
                self.settle()
            },
            Event::Visible(true) if self.is_sidebar() => {
                self.wake();
                true
            },
            Event::Visible(true) => {
                self.ask_zoxide();
                self.dirs.forget_listings();
                self.peeks.forget();
                self.places.forget();
                self.ask_places();
                self.ask_agents();
                false
            },
            _ => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        let ask = message
            .payload
            .as_deref()
            .filter(|_| message.name == SIDEBAR_PIPE)
            .and_then(Ask::parse);
        let place = Some(&message.args)
            .filter(|_| message.name == PLACE_PIPE)
            .and_then(Place::from_args);
        match self.role {
            Role::Picker => {},
            Role::Sidebar(_) => {
                if let Some(ask) = ask {
                    self.sidebar_pipe(ask);
                }
                let Some(place) = place else {
                    return false;
                };
                if let Role::Sidebar(tree) = &mut self.role {
                    tree.hand(place);
                }
                self.rebuild_tree();
                return true;
            },
            Role::Dock(ref mut dock) => {
                if let Some(ask) = ask {
                    dock.pipe(ask);
                }
                return false;
            },
            Role::Menu(_) | Role::Unknown(_) => return false,
        }
        if ask == Some(Ask::Focus) {
            self.leave();
            return false;
        }
        if let Some(place) = place {
            self.handed = Some(place);
            self.land();
            return true;
        }
        if message.name != SCREEN_PIPE {
            return false;
        }
        let Some(screen) = message.payload.as_deref().and_then(Screen::from_name) else {
            return false;
        };
        self.show(screen);
        true
    }

    fn render(&mut self, rows: usize, cols: usize) {
        self.size = (rows, cols);
        match &self.role {
            Role::Picker => {},
            Role::Sidebar(tree) => {
                let focused = self.sidebar_focused();
                render::render_sidebar(tree, &self.agents, focused, rows, cols, self.frame);
                return;
            },
            Role::Dock(_) => return,
            Role::Menu(menu) => {
                render::render_menu(menu, rows, cols);
                return;
            },
            Role::Unknown(mode) => {
                render::render_notice(&unknown_mode(mode), rows, cols);
                return;
            },
        }
        match &self.mode {
            Mode::Search => match self.screen {
                Screen::Sessions => {
                    render::render_search(
                        &self.matches,
                        &self.peeks,
                        self.error.as_deref(),
                        rows,
                        cols,
                    );
                },
                Screen::Dirs => {
                    render::render_dirs(&self.dirs, &self.matches.search_term, rows, cols);
                },
                Screen::Agents => render::render_agents(
                    &self.agents,
                    &self.peeks,
                    &self.matches.search_term,
                    self.error.as_deref(),
                    rows,
                    cols,
                    self.frame,
                ),
            },
            Mode::Rename { current, input } => render::render_rename(
                current,
                input,
                self.rename_error(current, input),
                rows,
                cols,
            ),
        }
    }
}

impl State {
    fn poll(&mut self) {
        let Ok(snapshot) = get_session_list() else {
            return;
        };
        if let Role::Sidebar(tree) = &mut self.role {
            // Before anyone comes back to this session, so that its first
            // rows show what was folded elsewhere.
            tree.refold();
        }
        if self.is_sidebar() {
            // zellij keeps a client's plugins running after the client leaves,
            // timers and all, and knows no focused pane for it. A sidebar its
            // client left asks nothing until the client comes back.
            let was_away = self.away;
            self.away = dock::attached() == Some(false);
            if self.away {
                if !was_away {
                    // Its client left this session, and so the pane it was
                    // in: what that pane finished as it left, it saw.
                    self.watch_news(None);
                }
                return;
            }
            self.seat_from(&snapshot);
        }
        // The picker too, for each agent to go by the name the sidebar gives
        // its pane.
        let before = std::mem::take(&mut self.branches);
        self.branches = snapshot
            .live_sessions
            .iter()
            .map(|s| {
                let age = Age::new(s.creation_time);
                let before = before.iter().find(|branch| branch.name == s.name);
                let before = before.map_or(&[][..], |branch| branch.panes.as_slice());
                let (places, checkouts) = (&self.places, &self.checkouts);
                Branch::new(s.name.clone(), age, &s.tabs, &s.panes, places, checkouts, before)
            })
            .collect();
        if self.is_sidebar() {
            if self.frame.wrapping_sub(self.places_at) >= TICKS_PER_PLACES {
                self.places_at = self.frame;
                self.places.forget();
                self.checkouts.forget();
            }
            self.ask_agents();
        }
        let current = snapshot
            .live_sessions
            .iter()
            .find(|s| s.is_current_session)
            .map(|s| s.name.clone());
        let mut contents = BTreeMap::new();
        let mut live_names = Vec::new();
        let mut live = Vec::new();
        let mut sidebars = BTreeMap::new();
        for session in snapshot.live_sessions {
            live_names.push(session.name.clone());
            if session.is_current_session {
                continue;
            }
            let name = session.name.clone();
            let age = Age::new(session.creation_time);
            if let Some(sidebar) = dock::standing(&session.panes) {
                sidebars.insert(name.clone(), sidebar);
            }
            contents.insert(name.clone(), contents_of(session));
            live.push(Session { name, age });
        }
        self.live_names = live_names;
        self.sidebars = sidebars;
        self.watch_news(current.as_deref());
        self.ask_shells();
        self.sessions = Sessions {
            live,
            dead: snapshot
                .resurrectable_sessions
                .into_iter()
                .map(|(name, age)| Session { name, age: Age::new(age) })
                .collect(),
        };
        self.matches.contents = contents;
        self.matches.refresh(&self.sessions, current);
        self.ask_places();
        self.rebuild_dirs(Selection::Hold);
        self.rebuild_agents(Selection::Hold);
    }

    /// Takes up what the sidebars of other sessions saw since, what each
    /// shell's title says it is doing, and the pane you are in, in `current`,
    /// for the dot of what is new in each pane.
    fn watch_news(&mut self, current: Option<&str>) {
        if !self.is_sidebar() {
            return;
        }
        let here = current.zip(self.pane_now());
        let panes = self.panes.as_ref();
        let Role::Sidebar(tree) = &mut self.role else {
            return;
        };
        tree.news.reload();
        let own = |pane| own_title(panes, pane);
        tree.news.watch(&self.branches, &self.agents, here, own, Moment::now());
        tree.news.prune(&self.live_names);
    }

    /// Asks what the shells with luneta's hook in every live session say.
    fn ask_shells(&mut self) {
        let Role::Sidebar(tree) = &mut self.role else {
            return;
        };
        if !tree.news.shells.ask() {
            return;
        }
        run_command(
            &shells::query(&self.live_names),
            BTreeMap::from([(CONTEXT_KEY.to_owned(), shells::CONTEXT_VALUE.to_owned())]),
        );
    }

    fn spinning(&self) -> bool {
        let shown = (self.is_sidebar() && !self.away)
            || (matches!(self.mode, Mode::Search) && self.screen == Screen::Agents);
        // A command at work turns the spinner an agent at work does.
        let running = matches!(&self.role, Role::Sidebar(tree) if tree.summary.busy > 0);
        shown && (self.agents.any_busy() || running)
    }

    fn origin_pane(&self) -> Option<u32> {
        let tab = self.active_tab?;
        origin(self.panes.as_ref()?.panes.get(&tab)?)
    }

    fn rebuild_agents(&mut self, policy: Selection) {
        let origin = if self.is_sidebar() { None } else { self.origin_pane() };
        let since = self.agents_since();
        let current = self.matches.current_session.as_deref();
        let live = Live::new(current, &self.sessions.live, &self.places, &self.branches);
        self.agents.rebuild(&self.matches.search_term, &live, origin, since, policy);
        self.land();
        self.rebuild_tree();
    }

    /// Puts the picker's lists on the row the sidebar handed over: the session
    /// on Sessions, and on Agents the agent in that pane, or else the first in
    /// that session. Both lists, so that `Tab` keeps the place too.
    fn land(&mut self) {
        let Some(place) = &self.handed else {
            return;
        };
        let (session, pane) = match place {
            Place::Session(session) => (session.as_str(), None),
            Place::Pane(session, pane) => (session.as_str(), Some(*pane)),
        };
        let live = |row: &sessions::Row| row.kind == Kind::Live && row.name == session;
        if let Some(at) = self.matches.rows.iter().position(live) {
            self.matches.rows.select(at);
        }
        self.agents.select_seated(session, pane);
    }

    fn agents_since(&self) -> Age {
        Age::from_secs(self.frame.wrapping_sub(self.agents_taken_at) / TICKS_PER_SECOND)
    }

    fn ask_places(&mut self) {
        for session in self.places.ask(&self.live_names) {
            run_command(
                &places::query(&session),
                BTreeMap::from([
                    (CONTEXT_KEY.to_owned(), places::CONTEXT_VALUE.to_owned()),
                    (places::SESSION_KEY.to_owned(), session.clone()),
                ]),
            );
        }
    }

    /// Asks git how the work tree stands in every directory a pane in the
    /// sidebar works in, and that it has not asked about since it last forgot.
    fn ask_checkouts(&mut self) {
        let leaves = self.branches.iter().flat_map(|branch| &branch.panes);
        let cwds = leaves.filter_map(|leaf| leaf.cwd.clone()).collect();
        for dir in self.checkouts.ask(&cwds) {
            run_command(
                &checkouts::query(&dir),
                BTreeMap::from([
                    (CONTEXT_KEY.to_owned(), checkouts::CONTEXT_VALUE.to_owned()),
                    (checkouts::DIR_KEY.to_owned(), dir.clone()),
                ]),
            );
        }
    }

    fn ask_agents(&mut self) {
        if !self.agents.ask() {
            return;
        }
        agents::ask(self.agents_command.as_deref());
    }

    fn follow_preview(&mut self) -> bool {
        if self.is_sidebar() || !matches!(self.mode, Mode::Search) {
            return false;
        }
        let Some(target) = self.preview_target() else {
            self.preview_at = None;
            return false;
        };
        match &self.preview_at {
            Some((at, since)) if *at == target => {
                if self.frame.wrapping_sub(*since) < PREVIEW_DELAY {
                    return false;
                }
            },
            _ => {
                self.preview_at = Some((target, self.frame));
                return false;
            },
        }
        match target {
            Target::Dir(path) => {
                if !self.dirs.begin_listing(&path) {
                    return false;
                }
                let mut command = LIST.to_vec();
                command.push("--");
                command.push(&path);
                run_command(
                    &command,
                    BTreeMap::from([
                        (CONTEXT_KEY.to_owned(), dirs::PREVIEW_VALUE.to_owned()),
                        (dirs::PATH_KEY.to_owned(), path.clone()),
                    ]),
                );
            },
            Target::Pane(session, pane) => {
                if !self.peeks.claim(&session, pane) {
                    return false;
                }
                let id = panes::pane_id(pane);
                let mut command = panes::DUMP.to_vec();
                command.push(&session);
                command.push(&id);
                run_command(
                    &command,
                    BTreeMap::from([
                        (CONTEXT_KEY.to_owned(), panes::CONTEXT_VALUE.to_owned()),
                        (panes::PANE_KEY.to_owned(), panes::key(&session, pane)),
                    ]),
                );
            },
        }
        true
    }

    fn preview_target(&self) -> Option<Target> {
        match self.screen {
            Screen::Dirs => self.dirs.selected_row().map(|row| Target::Dir(row.path.clone())),
            Screen::Sessions => {
                let row = self.matches.rows.selected_row()?;
                let focus = self.matches.contents.get(&row.name)?.focus.as_ref()?;
                Some(Target::Pane(row.name.clone(), focus.pane))
            },
            Screen::Agents => {
                let row = self.agents.selected_row()?;
                Some(Target::Pane(row.seat.session().to_owned(), row.pane))
            },
        }
    }

    fn rebuild_dirs(&mut self, policy: Selection) {
        self.dirs.rebuild(
            &self.matches.search_term,
            &self.sessions,
            self.matches.current_session.as_deref(),
            policy,
        );
    }

    fn ask_zoxide(&mut self) {
        if !self.dirs.ask() {
            return;
        }
        run_command(
            &dirs::QUERY,
            BTreeMap::from([(CONTEXT_KEY.to_owned(), dirs::CONTEXT_VALUE.to_owned())]),
        );
    }

    fn handle_key(&mut self, key: &KeyWithModifier) -> bool {
        self.handed = None;
        if key.bare_key == BareKey::Char('c') && key.has_modifiers(&[KeyModifier::Ctrl]) {
            close_self();
            return false;
        }
        match self.mode {
            Mode::Search => self.handle_search_key(key),
            Mode::Rename { .. } => self.handle_rename_key(key),
        }
    }

    fn handle_search_key(&mut self, key: &KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Tab if key.has_no_modifiers() => {
                self.show(self.screen.next());
                true
            },
            BareKey::Tab if key.has_modifiers(&[KeyModifier::Shift]) => {
                self.show(self.screen.prev());
                true
            },
            BareKey::Enter if key.has_no_modifiers() => {
                match self.screen {
                    Screen::Sessions => self.confirm_search(),
                    Screen::Dirs => self.confirm_dir(),
                    Screen::Agents => self.confirm_agent(),
                }
                true
            },
            BareKey::Esc if key.has_no_modifiers() => {
                close_self();
                false
            },
            BareKey::Char('r') if key.has_modifiers(&[KeyModifier::Ctrl]) => {
                self.begin_rename();
                true
            },
            BareKey::Delete if key.has_no_modifiers() => match self.screen {
                Screen::Sessions => {
                    self.delete_selected();
                    true
                },
                Screen::Agents => {
                    self.kill_agent_pane();
                    true
                },
                Screen::Dirs => false,
            },
            BareKey::Down if key.has_no_modifiers() => {
                self.move_selection(1);
                true
            },
            BareKey::Up if key.has_no_modifiers() => {
                self.move_selection(-1);
                true
            },
            BareKey::Backspace if key.has_no_modifiers() => {
                let mut term = self.matches.search_term.clone();
                if term.pop().is_none() {
                    return false;
                }
                self.set_term(term);
                true
            },
            BareKey::Char(c) if key.has_no_modifiers() => {
                let mut term = self.matches.search_term.clone();
                term.push(c);
                self.set_term(term);
                true
            },
            _ => false,
        }
    }

    /// An error belongs to the list it was made on.
    fn show(&mut self, screen: Screen) {
        self.screen = screen;
        self.error = None;
    }

    fn move_selection(&mut self, delta: isize) {
        match self.screen {
            Screen::Sessions => self.matches.rows.move_selection(delta),
            Screen::Dirs => self.dirs.rows.move_selection(delta),
            Screen::Agents => self.agents.rows.move_selection(delta),
        }
    }

    fn confirm_search(&mut self) {
        if let Some(name) = self.matches.selected_name() {
            switch_session(Some(name));
            close_self();
            return;
        }

        if self.matches.is_own_name() {
            return;
        }
        let Ok(name) = self.matches.new_name() else {
            return;
        };
        switch_session(name.as_deref());
        close_self();
    }

    fn confirm_dir(&mut self) {
        let Some(row) = self.dirs.selected_row() else {
            return;
        };
        switch_session_with_cwd(Some(row.name.as_str()), Some(PathBuf::from(&row.path)));
        close_self();
    }

    fn confirm_agent(&mut self) {
        let Some(row) = self.agents.selected_row() else {
            return;
        };
        match &row.seat {
            Seat::Here(_) => focus_terminal_pane(row.pane, false, false),
            Seat::There(session) => {
                switch_session_with_focus(session, None, Some((row.pane, false)));
            },
        }
        close_self();
    }

    fn begin_rename(&mut self) {
        let Some(current) = self.matches.current_session.clone() else {
            return;
        };
        self.error = None;
        self.mode = Mode::Rename { current, input: String::new() };
    }

    fn handle_rename_key(&mut self, key: &KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Enter if key.has_no_modifiers() => {
                self.apply_rename();
                true
            },
            BareKey::Esc if key.has_no_modifiers() => {
                self.mode = Mode::Search;
                true
            },
            BareKey::Backspace if key.has_no_modifiers() => {
                let Mode::Rename { input, .. } = &mut self.mode else {
                    return false;
                };
                input.pop().is_some()
            },
            BareKey::Char(c) if key.has_no_modifiers() => {
                let Mode::Rename { input, .. } = &mut self.mode else {
                    return false;
                };
                input.push(c);
                true
            },
            _ => false,
        }
    }

    fn rename_error(&self, current: &str, name: &str) -> Option<&'static str> {
        self.rename_to(current, name).err()
    }

    fn rename_to(&self, current: &str, name: &str) -> Result<SessionName, &'static str> {
        if current == name {
            Err("already called that")
        } else if self.sessions.any_named(name) {
            Err("a session by that name already exists")
        } else {
            SessionName::parse(name)
        }
    }

    fn apply_rename(&mut self) {
        let Mode::Rename { current, input } = &self.mode else {
            return;
        };
        let Ok(name) = self.rename_to(current, input) else {
            return;
        };
        // The sidebars find what is new in its panes under the new name, as
        // they do whoever renames it.
        rename_session(&name);
        self.mode = Mode::Search;
    }

    fn delete_selected(&mut self) {
        let Some(row) = self.matches.rows.selected_row() else {
            return;
        };
        let name = row.name.clone();
        let (verb, result) = match row.kind {
            Kind::Live => ("kill", kill_sessions(&[name.as_str()])),
            Kind::Resurrectable => {
                let deleted = delete_dead_session(&name);
                if deleted.is_ok() {
                    // Gone for good: a session made later by its name is
                    // another, and starts unfolded in every sidebar.
                    Folds::shared().set(&name, false);
                }
                ("delete", deleted)
            },
        };
        self.set_term(String::new());
        self.error = result.err().map(|e| format!("{verb} \"{name}\": {e}"));
        self.poll();
    }

    /// Closes the pane the highlighted agent sits in, and with it its tab when
    /// nothing else holds the tab open, as the menu's `d d` does. The plugin
    /// closes a pane of its own session, and `zellij` one of another, whose
    /// answer comes later. A pane that is only a guess may hold something
    /// else, and stays.
    fn kill_agent_pane(&mut self) {
        let Some(row) = self.agents.selected_row() else {
            return;
        };
        if row.guessed {
            let label = row.label();
            self.error = Some(format!("kill \"{label}\": luneta is not sure which pane it sits in"));
            return;
        }
        let pane = close::Pane::of(row.pane, &self.leaves(row.seat.session()));
        pane.act(row.seat.clone()).run(BTreeMap::from([
            (CONTEXT_KEY.to_owned(), agents::CLOSE_VALUE.to_owned()),
            (agents::LABEL_KEY.to_owned(), row.label()),
        ]));
        self.agents.forget_selected();
        self.set_term(String::new());
    }

    /// The panes of `session` you can go to, as zellij lists them now, and
    /// not as it did at the picker's last poll.
    fn leaves(&self, session: &str) -> Vec<Leaf> {
        let Ok(snapshot) = get_session_list() else {
            return Vec::new();
        };
        let Some(listed) = snapshot.live_sessions.iter().find(|listed| listed.name == session)
        else {
            return Vec::new();
        };
        let (places, checkouts) = (&self.places, &self.checkouts);
        tree::leaves(session, &listed.tabs, &listed.panes, places, checkouts, &[])
    }

    fn set_term(&mut self, term: String) {
        self.error = None;
        self.matches.set_search_term(term, &self.sessions);
        self.rebuild_dirs(Selection::SnapToTop);
        self.rebuild_agents(Selection::SnapToTop);
    }
}

impl State {
    fn is_sidebar(&self) -> bool {
        matches!(self.role, Role::Sidebar(_))
    }

    /// zellij sends the panes and the tabs when they change, and a sidebar
    /// that was still asking for its permissions at the time never hears them.
    /// The session list carries the same two, a little older, to start from.
    fn seat_from(&mut self, snapshot: &SessionListSnapshot) {
        if self.panes.is_some() {
            return;
        }
        let Some(current) = snapshot.live_sessions.iter().find(|s| s.is_current_session) else {
            return;
        };
        self.panes = Some(current.panes.clone());
        self.tabs.clone_from(&current.tabs);
        self.active_tab = current.tabs.iter().find(|t| t.active).map(|t| t.position);
    }

    fn own_pane(&self) -> Option<&PaneInfo> {
        let manifest = self.panes.as_ref()?;
        manifest.panes.values().flatten().find(|p| p.is_plugin && p.id == self.plugin_id)
    }

    /// Gives the pane the title the picker's or the sidebar's is to have where
    /// it stands, as it floats or stands in a column.
    fn retitle(&self, pane: &PaneInfo) {
        let title = match self.role {
            Role::Picker => dock::picker_title(pane),
            Role::Sidebar(_) => dock::sidebar_title(pane),
            Role::Dock(_) | Role::Menu(_) | Role::Unknown(_) => None,
        };
        if let Some(title) = title {
            rename_plugin_pane(self.plugin_id, title);
        }
    }

    fn own_tab(&self) -> Option<usize> {
        let manifest = self.panes.as_ref()?;
        manifest
            .panes
            .iter()
            .find(|(_, panes)| panes.iter().any(|p| p.is_plugin && p.id == self.plugin_id))
            .map(|(tab, _)| *tab)
    }

    fn sidebar_focused(&self) -> bool {
        let Some(tab) = self.own_tab().filter(|tab| Some(*tab) == self.active_tab) else {
            return false;
        };
        let floating = self.shows_floating(tab);
        let focused = self
            .panes
            .as_ref()
            .and_then(|manifest| manifest.panes.get(&tab))
            .and_then(|panes| panes.iter().find(|p| p.is_plugin && p.id == self.plugin_id))
            .is_some_and(|own| own.is_focused);
        // The focus on its way past draws nothing, and leaves the highlight
        // with the pane you are in.
        focused && !floating && !self.pass.passing()
    }

    /// Whether a brief has the focus, floating over the sidebar it hands the
    /// focus back to as it closes.
    fn briefing(&self) -> bool {
        let Some(tab) = self.own_tab().filter(|tab| Some(*tab) == self.active_tab) else {
            return false;
        };
        let floating = self.shows_floating(tab);
        let panes = self.panes.as_ref().and_then(|manifest| manifest.panes.get(&tab));
        let panes = panes.map_or(&[][..], Vec::as_slice);
        let own = panes.iter().any(|p| p.is_focused && p.is_plugin && p.id == self.plugin_id);
        let over = panes.iter().any(|p| p.is_focused && p.is_floating && brief::is_brief(p));
        floating && own && over
    }

    /// Whether `tab` shows its floating panes. zellij hides them as the last
    /// one closes, and tells of the panes before the tabs: a tab whose panes
    /// have none floating shows none, whatever the tabs said last.
    fn shows_floating(&self, tab: usize) -> bool {
        let shown = self.tabs.iter().any(|t| t.position == tab && t.are_floating_panes_visible);
        let panes = self.panes.as_ref().and_then(|manifest| manifest.panes.get(&tab));
        let floating = |p: &PaneInfo| p.is_floating && !p.is_suppressed;
        shown && panes.is_some_and(|panes| panes.iter().any(floating))
    }

    /// After the panes or the tabs change. Says whether to draw.
    fn settle(&mut self) -> bool {
        if self.leaving && !self.on_screen() {
            close_self();
        }
        self.rebuild_agents(Selection::Hold);
        self.is_sidebar()
    }

    /// `focus` from the picker goes to the sidebar, with the row the picker
    /// has highlighted. The picker closes once the sidebar has the focus,
    /// which hides the floating panes: closed first, it would hand the focus
    /// back to the pane under it, which may be the sidebar, where a `focus`
    /// takes you out instead.
    fn leave(&mut self) {
        if !self.on_screen() {
            return;
        }
        let Some(sidebar) = self.sidebar_pane() else {
            return;
        };
        if let Some(place) = self.highlighted() {
            hand(sidebar, &place);
        }
        self.leaving = true;
    }

    /// Whether the picker shows, over the tab on screen.
    fn on_screen(&self) -> bool {
        let Some(tab) = self.own_tab() else {
            return false;
        };
        self.tabs.iter().any(|t| t.position == tab && t.active && t.are_floating_panes_visible)
    }

    /// The sidebar's pane, hidden or not. Of two, the one the dock keeps.
    fn sidebar_pane(&self) -> Option<u32> {
        let manifest = self.panes.as_ref()?;
        let panes = manifest.panes.values().flatten();
        panes.filter(|p| dock::is_sidebar(p)).map(|p| p.id).min()
    }

    /// The picker's highlighted row, as the sidebar lists it. A directory is
    /// not a row of the sidebar, nor is a session that is not running.
    fn highlighted(&self) -> Option<Place> {
        match self.screen {
            Screen::Sessions => {
                let row = self.matches.rows.selected_row().filter(|row| row.kind == Kind::Live)?;
                Some(Place::Session(row.name.clone()))
            },
            Screen::Agents => {
                let row = self.agents.selected_row()?;
                Some(Place::Pane(row.seat.session().to_owned(), row.pane))
            },
            Screen::Dirs => None,
        }
    }

    /// Sends the focus on when it lands in the column without being asked
    /// there, as a key that moves it does.
    fn pass_over(&mut self) {
        if !self.is_sidebar() {
            return;
        }
        let Some(tab) = self.tabs.iter().find(|t| t.active) else {
            return;
        };
        let Some(panes) = self.panes.as_ref().and_then(|m| m.panes.get(&tab.position)) else {
            return;
        };
        let Some(at) = focus_at(tab, panes, self.plugin_id) else {
            return;
        };
        let stands = |pane| tiled(panes).any(|p| pane_id(p) == pane);
        let pressed = self.pressed.take();
        match self.pass.see(at, self.tabs.len(), self.frame, stands) {
            // The menu of another session's sidebar took you here, into this
            // sidebar, where no key of this session asked the focus.
            Then::Back | Then::Beside if matches!(at, At::Sidebar { .. }) && self.welcomed() => {
                self.pass.ask(self.frame);
            },
            Then::Back => move_focus_or_tab(Direction::Left),
            Then::Beside => move_focus(Direction::Right),
            // The click that focused the sidebar does what any other does.
            // One that came through did as it came, and left nothing pressed.
            Then::Press => {
                if let Some(pressed) = pressed {
                    self.press(pressed);
                }
            },
            Then::Stay => {},
        }
    }

    /// Whether the menu of another session's sidebar left word that it took
    /// you into this sidebar. The word holds once.
    fn welcomed(&self) -> bool {
        let coming = coming::Coming::shared();
        let Some(session) = coming.heard(self.plugin_id, Moment::now()) else {
            return false;
        };
        if self.session_name().as_deref() != Some(session.as_str()) {
            return false;
        }
        coming.forget();
        true
    }

    /// The session this plugin runs in. A sidebar zellij started for a client
    /// that just came may not have read the sessions yet.
    fn session_name(&self) -> Option<String> {
        if let Some(current) = &self.matches.current_session {
            return Some(current.clone());
        }
        let snapshot = get_session_list().ok()?;
        snapshot.live_sessions.into_iter().find(|s| s.is_current_session).map(|s| s.name)
    }

    fn wake(&mut self) {
        self.places.forget();
        self.checkouts.wake();
        self.places_at = self.frame;
        self.poll();
    }

    fn rebuild_tree(&mut self) {
        if !self.is_sidebar() {
            return;
        }
        if let Some(pane) = self.origin_pane() {
            self.here = Some(pane);
        }
        let current = self.matches.current_session.clone();
        if let (Some(current), Some(manifest)) = (current.as_deref(), self.panes.as_ref()) {
            if let Some(branch) = self.branches.iter_mut().find(|b| b.name == current) {
                let (places, checkouts) = (&self.places, &self.checkouts);
                branch.panes =
                    tree::leaves(current, &self.tabs, manifest, places, checkouts, &branch.panes);
            }
        }
        if !self.away {
            self.ask_checkouts();
        }
        // A brief leaves the cursor on the row it was asked on, for the
        // sidebar to have it back as it closes.
        let follow = !self.sidebar_focused() && !self.briefing();
        if let Role::Sidebar(tree) = &mut self.role {
            tree.rebuild(&self.branches, current.as_deref(), &self.agents, self.here, follow);
        }
        if let Some((picker, place)) = self.handing() {
            hand(picker, &place);
        }
    }

    /// The picker the focus left the sidebar for, and the row the cursor was
    /// on as it left: a key that opens the picker from the sidebar opens it
    /// there. The floating panes can show before the picker is among them, or
    /// has its title; once they are hidden, the focus went to a pane in the
    /// tab instead.
    fn handing(&mut self) -> Option<(u32, Place)> {
        let tab = self.tabs.iter().find(|t| t.active)?;
        let floating = tab.are_floating_panes_visible;
        let panes = self.panes.as_ref().and_then(|m| m.panes.get(&tab.position));
        let picker = panes
            .into_iter()
            .flatten()
            .find(|p| p.is_floating && p.is_focused && !p.is_suppressed)
            .filter(|p| p.is_plugin && p.title == dock::TITLE)
            .map(|p| p.id);
        let Role::Sidebar(tree) = &mut self.role else {
            return None;
        };
        if !floating {
            tree.take_left();
            return None;
        }
        let picker = picker?;
        let place = tree.take_left()?;
        Some((picker, place))
    }

    fn handle_sidebar_key(&mut self, key: &KeyWithModifier) -> bool {
        // The menu takes every key while it is open, and those on their way
        // as an agent it started takes the focus, until `settle_menu` closes
        // it.
        let frame = self.frame;
        let subject = self.menu_subject();
        if let Role::Sidebar(tree) = &mut self.role {
            if let Some(menu) = tree.menu.take() {
                let (menu, did) = menu.key(key, subject.as_ref(), frame);
                tree.menu = menu;
                self.follow_menu(did);
                return true;
            }
        }
        if !key.has_no_modifiers() {
            return false;
        }
        let search = match &self.role {
            Role::Sidebar(tree) => tree.search().map(str::to_owned),
            _ => None,
        };
        // A key goes by the cursor, so the view comes back to it, whatever
        // else the key does.
        let unscrolled = match &mut self.role {
            Role::Sidebar(tree) => tree.unscroll(),
            _ => false,
        };
        if let Some(term) = search {
            return self.handle_search_box_key(key.bare_key, term) || unscrolled;
        }
        // A character does what `Top::of` says, which the legend goes by
        // too, and a key it has not is the menu's.
        let drawn = match key.bare_key {
            BareKey::Down => self.move_tree(1),
            BareKey::Up => self.move_tree(-1),
            BareKey::Left => self.fold(Some(true)),
            BareKey::Right => self.fold(Some(false)),
            BareKey::Enter => {
                self.go_to_selected();
                true
            },
            BareKey::Esc => {
                self.go_back();
                false
            },
            BareKey::Char(c) => match Top::of(c) {
                Some(Top::Move(rows)) => self.move_tree(rows),
                Some(Top::Fold(folded)) => self.fold(folded),
                Some(Top::Menu) => self.open_menu(None),
                Some(Top::Command(command)) => self.command(command),
                Some(Top::Group(group)) => self.open_help(group),
                None => self.open_menu(Some(c)),
            },
            _ => false,
        };
        drawn || unscrolled
    }

    /// Runs a command of the sidebar's own: a key at the top of the sidebar,
    /// or a key of help. Says whether to draw.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Search => self.search_tree(String::new()),
            Command::Brief(scope) => {
                self.brief(scope);
                true
            },
        }
    }

    /// What a key of the menu left the sidebar to do: go past what a delete
    /// key closes, once it is gone, or run a command. Says whether to draw.
    fn follow_menu(&mut self, did: Option<Did>) -> bool {
        match did {
            None => false,
            Some(Did::Closed(act)) => {
                if let Role::Sidebar(tree) = &mut self.role {
                    tree.closing(&act);
                }
                true
            },
            Some(Did::Command(command)) => self.command(command),
        }
    }

    /// Opens help at the bottom of the sidebar for the highlighted row, on the
    /// group of commands `group` opens. Says whether to draw.
    fn open_help(&mut self, group: char) -> bool {
        let Some(row) = self.menu_row() else {
            return false;
        };
        let entries = self.menu_entries.as_deref();
        let menu = SidebarMenu::help(entries, row, group, &mut self.menus);
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        tree.menu = Some(menu);
        true
    }

    /// Asks for a brief of what `scope` covers, by the row the cursor is on:
    /// the end of each transcript it covers first, and the brief's pane once
    /// the sidebar has read them. With no agent there, the sidebar says so
    /// where the brief's keys were.
    fn brief(&mut self, scope: Scope) {
        let Role::Sidebar(tree) = &self.role else {
            return;
        };
        let order = tree::in_order(&self.branches);
        let over = match (scope, tree.rows.selected_row()) {
            (Scope::Every, _) => Over::Sessions(order.iter().map(|b| b.name.clone()).collect()),
            (Scope::Highlighted, Some(Node::Pane(pane))) => {
                Over::Agent(pane.session.clone(), pane.id)
            },
            (Scope::Session | Scope::Highlighted, Some(row)) => {
                Over::Session(row.session().to_owned())
            },
            (Scope::Session | Scope::Highlighted, None) => return,
        };
        let mut notes = Vec::new();
        for branch in order {
            for leaf in &branch.panes {
                let seated =
                    |row: &&AgentRow| row.seat.session() == branch.name && row.pane == leaf.id;
                let Some(agent) = self.agents.rows.iter().find(seated) else {
                    continue;
                };
                notes.push(brief::Note {
                    session: branch.name.clone(),
                    pane: leaf.id,
                    name: tree::label(leaf, Some(agent)),
                    status: agent.status.word().to_owned(),
                    held: agent.age.label(),
                    branch: leaf.checkout.as_ref().map(|checkout| checkout.branch.clone()),
                    cwd: leaf.cwd.clone().or_else(|| agent.cwd.clone()),
                    transcript: agent.transcript.clone(),
                });
            }
        }
        self.briefs = self.briefs.wrapping_add(1);
        match Brief::new(self.briefs, over, notes) {
            Ok(brief) => {
                let tails = brief.tails();
                let tails: Vec<&str> = tails.iter().map(String::as_str).collect();
                run_command(
                    &tails,
                    BTreeMap::from([
                        (CONTEXT_KEY.to_owned(), brief::CONTEXT_VALUE.to_owned()),
                        (brief::NUMBER_KEY.to_owned(), brief.number.to_string()),
                    ]),
                );
                self.brief = Some(brief);
            },
            Err(reason) => {
                if let Role::Sidebar(tree) = &mut self.role {
                    tree.menu = Some(SidebarMenu::wrong(reason, &mut self.menus));
                }
            },
        }
    }

    /// Opens the menu at the bottom of the sidebar for the highlighted row, on
    /// the group its `key` opens, or on the first: an agent it starts opens
    /// beside that row's pane, in the directory it works in, and its Delete
    /// group closes the pane or kills the session. A key the menu has not
    /// does nothing. Says whether to draw.
    fn open_menu(&mut self, key: Option<char>) -> bool {
        let Some(row) = self.menu_row() else {
            return false;
        };
        let entries = self.menu_entries.as_deref();
        let menu = SidebarMenu::open(entries, row, key, &mut self.menus, self.frame);
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        tree.menu = menu;
        tree.menu.is_some()
    }

    /// While the menu takes the client out of the session, which a key of it
    /// killed, or to another, where a key of it opens a tab: asks zellij
    /// whether the client is gone, or there, for `listed` to kill the session,
    /// or open the tab, once it is. Says whether to draw.
    fn leave_session(&mut self) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        if !tree.menu.as_ref().is_some_and(SidebarMenu::lasts) {
            return false;
        }
        tree.menu = tree.menu.take().map(SidebarMenu::tick);
        true
    }

    /// Closes the menu once the keys on their way to what it started in the
    /// sidebar's own session have all come: the keys after them are the
    /// sidebar's, and the legend is back. The frame moves only with the tick
    /// that runs this, so no key comes to a menu that should be gone. Says
    /// whether to draw.
    fn settle_menu(&mut self) -> bool {
        let frame = self.frame;
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        let lapsed = |menu: &SidebarMenu| {
            menu.started_at().is_some_and(|at| frame.wrapping_sub(at) > ON_THE_WAY)
        };
        if !tree.menu.as_ref().is_some_and(lapsed) {
            return false;
        }
        tree.menu = None;
        true
    }

    /// What zellij said, as it listed the clients of a session for the menu
    /// `context` names, while the menu takes the client out of the session or
    /// to another: the session is killed once the client is gone, or the tab
    /// opens once it is there. Says whether to draw.
    fn listed(
        &mut self,
        exit_code: Option<i32>,
        stdout: &[u8],
        context: &BTreeMap<String, String>,
    ) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        if !tree.menu.as_ref().is_some_and(SidebarMenu::lasts) {
            return false;
        }
        let client = self.client_id;
        let listed = |menu: SidebarMenu| menu.clients(client, exit_code, stdout, context);
        tree.menu = tree.menu.take().and_then(listed);
        tree.menu.is_none()
    }

    /// What the open menu is for, as a key of it deletes the pane or the
    /// session.
    fn menu_subject(&self) -> Option<close::Subject> {
        let Role::Sidebar(tree) = &self.role else {
            return None;
        };
        let (pane, session) = tree.menu.as_ref()?.pane()?;
        self.closing(pane, session)
    }

    /// `pane` of `session` or of the session you are in, or that session's
    /// row with no `pane`, as a key of the menu deletes it: the pane's tab,
    /// whether anything else holds the tab open, the agents of its session,
    /// and the session to go to before the session you are in is killed, into
    /// its sidebar, as you leave from this one.
    fn closing(&self, pane: Option<u32>, session: Option<&str>) -> Option<close::Subject> {
        let current = self.matches.current_session.as_deref();
        let name = session.or(current)?;
        let seat = if Some(name) == current {
            Seat::Here(name.to_owned())
        } else {
            Seat::There(name.to_owned())
        };
        let branch = self.branches.iter().find(|branch| branch.name == name);
        let leaves = branch.map_or(&[][..], |branch| branch.panes.as_slice());
        let agents = close::agents_of(&self.agents, name, leaves);
        let others = self.sessions.live.iter().map(|other| (other.name.as_str(), other.age));
        let refuge = close::refuge(others).map(|session| close::Refuge {
            sidebar: self.sidebars.get(&session).copied(),
            session,
        });
        Some(close::Subject::new(seat, pane, leaves, agents, refuge))
    }

    /// The highlighted row, for the menu: its pane, the directory it works
    /// in, as its session says or else as its agent does, the branch checked
    /// out there, when that is another session, its session and its tab, and
    /// the pane you came from, for the menu to hand the focus back to before
    /// it takes you out of this session. The row of a session that lists none
    /// of its panes has its session alone.
    fn menu_row(&self) -> Option<menu::Row> {
        let Role::Sidebar(tree) = &self.role else {
            return None;
        };
        let current = self.matches.current_session.as_deref();
        let back = self.here;
        let (session, pane) = match tree.rows.selected_row().map(Node::place)? {
            Place::Pane(session, pane) => (session, pane),
            Place::Session(session) => {
                let here = current == Some(session.as_str());
                let there = (!here).then_some(There { session, tab: None });
                return Some(menu::Row { pane: None, there, back });
            },
        };
        let leaf = self
            .branches
            .iter()
            .find(|branch| branch.name == session)
            .and_then(|branch| branch.panes.iter().find(|leaf| leaf.id == pane));
        let seated = |row: &&AgentRow| row.seat.session() == session && row.pane == pane;
        let agent = self.agents.rows.iter().find(seated);
        let cwd = leaf
            .and_then(|leaf| leaf.cwd.clone())
            .or_else(|| agent.and_then(|row| row.cwd.clone()));
        let branch = leaf.and_then(|leaf| leaf.checkout.as_ref()).map(|out| out.branch.clone());
        let here = current == Some(session.as_str());
        let tab = leaf.and_then(|leaf| leaf.tab_id);
        let there = (!here).then_some(There { session, tab });
        Some(menu::Row { pane: Some(RowPane { id: pane, cwd, branch }), there, back })
    }

    /// The keys `j k h l n` and `Space` type, like any other character, while
    /// the search box is open. `Esc` closes it, and so does `Backspace` once
    /// there is nothing left to take back, as in less and vim.
    fn handle_search_box_key(&mut self, key: BareKey, mut term: String) -> bool {
        match key {
            BareKey::Down => self.move_tree(1),
            BareKey::Up => self.move_tree(-1),
            BareKey::Enter => {
                self.go_to_selected();
                true
            },
            BareKey::Esc => self.close_search(),
            BareKey::Backspace if term.is_empty() => self.close_search(),
            BareKey::Backspace => {
                term.pop();
                self.search_tree(term)
            },
            BareKey::Char(c) => {
                term.push(c);
                self.search_tree(term)
            },
            _ => false,
        }
    }

    fn search_tree(&mut self, term: String) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        tree.search_for(term);
        self.rebuild_tree();
        true
    }

    fn close_search(&mut self) -> bool {
        let closed = match &mut self.role {
            Role::Sidebar(tree) => tree.close_search(),
            _ => false,
        };
        if closed {
            self.rebuild_tree();
        }
        closed
    }

    /// zellij hands the sidebar a click only once it has the focus; the click
    /// that gives it the focus ends the hover over it instead, which says
    /// where the click was. A right click, and a hold or a release, it hands
    /// to no plugin.
    fn handle_mouse(&mut self, mouse: Mouse) -> bool {
        if !self.is_sidebar() {
            return false;
        }
        match mouse {
            Mouse::LeftClick(line, column) => {
                self.pass.point(self.frame);
                self.pressed = None;
                let Ok(line) = usize::try_from(line) else {
                    return false;
                };
                let hit = self.hit(line, column);
                // A click closes the menu, and goes where it goes without it.
                let closed = match &mut self.role {
                    Role::Sidebar(tree) => tree.menu.take().is_some(),
                    _ => false,
                };
                hit.is_some_and(|hit| self.press(hit)) || closed
            },
            // zellij ends the hover over a plugin pane with a move to its last
            // column, when a click focuses the pane and when the pointer
            // leaves it.
            Mouse::Hover(0, column) if column == usize::from(u16::MAX) => {
                self.pass.point(self.frame);
                let pointer = self.pointer.take();
                self.pressed = pointer.and_then(|(line, column)| self.hit(line, column));
                false
            },
            Mouse::Hover(line, column) => {
                self.pointer = usize::try_from(line).ok().map(|line| (line, column));
                false
            },
            Mouse::ScrollUp(lines) => self.scroll(-(lines as isize)),
            Mouse::ScrollDown(lines) => self.scroll(lines as isize),
            _ => false,
        }
    }

    /// The row drawn at `line` and `column` of the sidebar, and whether that
    /// is its fold. Past the last column is the pane's frame.
    fn hit(&self, line: usize, column: usize) -> Option<(Place, bool)> {
        let (rows, cols) = self.size;
        let Role::Sidebar(tree) = &self.role else {
            return None;
        };
        if column >= cols {
            return None;
        }
        let focused = self.sidebar_focused();
        let index = render::sidebar_row_at(tree, &self.agents, focused, rows, cols, line)?;
        let node = tree.rows.get(index)?;
        Some((node.place(), render::folds_at(node, column)))
    }

    /// A click on a row: on a session's fold, it folds or unfolds the
    /// session, and anywhere else it goes there, on a row the cursor stops on.
    /// The row of a session that lists its panes heads them, and a click on
    /// its name does nothing. A term unfolds what it finds, so a fold would
    /// show nothing, and a click on it is a click on its row.
    ///
    /// A fold has the cursor go from the session's row to its first pane, or
    /// from a pane of it to its row, which would end the view the wheel left:
    /// the view stays, as it does for a click on any other row. The rows
    /// above the one clicked stay as they were.
    fn press(&mut self, (place, fold): (Place, bool)) -> bool {
        let (rows, cols) = self.size;
        let focused = self.sidebar_focused();
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        let Some(index) = tree.rows.iter().position(|node| node.place() == place) else {
            return false;
        };
        let selected = render::select_sidebar_row(tree, &self.agents, focused, rows, cols, index);
        if fold && !tree.narrowed() {
            let top = tree.top();
            let changed = tree.toggle_row(index);
            if changed {
                self.rebuild_tree();
                if let (Some(top), Role::Sidebar(tree)) = (top, &mut self.role) {
                    tree.scroll_to(top);
                }
            }
            return changed;
        }
        if selected {
            self.go_to_selected();
        }
        selected
    }

    fn scroll(&mut self, delta: isize) -> bool {
        let (rows, cols) = self.size;
        let focused = self.sidebar_focused();
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        render::scroll_sidebar(tree, &self.agents, focused, rows, cols, delta)
    }

    fn move_tree(&mut self, delta: isize) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        tree.rows.move_selection(delta);
        true
    }

    /// `Some(true)` folds, `Some(false)` unfolds, and `None` turns it over.
    fn fold(&mut self, folded: Option<bool>) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        let changed = match folded {
            Some(folded) => tree.fold(folded),
            None => tree.toggle(),
        };
        if changed {
            self.rebuild_tree();
        }
        changed
    }

    /// Going anywhere closes the search, so that the sidebar you come back to
    /// shows every row. A session's row, which stands in for the panes it does
    /// not list, goes to the pane the session last had the focus on: for the
    /// session you are in, the pane you came from. A key or a click goes here,
    /// and both come only to the sidebar with the focus.
    fn go_to_selected(&mut self) {
        let Role::Sidebar(tree) = &self.role else {
            return;
        };
        let Some(place) = tree.rows.selected_row().map(Node::place) else {
            return;
        };
        self.close_search();
        let current = self.matches.current_session.as_deref();
        match place {
            Place::Pane(session, pane) => self.go_to(&session, pane, true),
            Place::Session(session) if current == Some(session.as_str()) => self.go_back(),
            Place::Session(session) => self.switch_to(&session, None, true),
        }
    }

    /// `focused` says whether the sidebar has the focus, for `switch_to`.
    fn go_to(&self, session: &str, pane: u32, focused: bool) {
        if self.matches.current_session.as_deref() == Some(session) {
            focus_terminal_pane(pane, false, false);
        } else {
            self.switch_to(session, Some(pane), focused);
        }
    }

    /// To another session, and to `pane` in it. zellij keeps where each
    /// session's focus was, so a sidebar with the focus, as `focused` says,
    /// hands it back to the pane you came from first: the session would have
    /// you back in the sidebar.
    fn switch_to(&self, session: &str, pane: Option<u32>, focused: bool) {
        if focused {
            self.go_back();
        }
        match pane {
            Some(pane) => switch_session_with_focus(session, None, Some((pane, false))),
            None => switch_session(Some(session)),
        }
    }

    fn go_to_after(&self, focused: bool) {
        let Role::Sidebar(tree) = &self.role else {
            return;
        };
        let session = self.matches.current_session.clone();
        let here = session.zip(self.pane_now()).map(|(session, pane)| Place::Pane(session, pane));
        if let Some(Place::Pane(session, pane)) = tree.after(here.as_ref()) {
            self.go_to(&session, pane, focused);
        }
    }

    /// The pane you are in. A hidden sidebar hears nothing of the panes, so it
    /// comes from zellij.
    fn pane_now(&self) -> Option<u32> {
        match get_focused_pane_info() {
            Ok((_, PaneId::Terminal(pane))) => Some(pane),
            _ => self.here,
        }
    }

    fn go_back(&self) {
        match self.here {
            Some(pane) => focus_terminal_pane(pane, false, false),
            None => move_focus(Direction::Right),
        }
    }

    /// A key binding broadcasts to every plugin in the session: the sidebar
    /// and the dock both hear it. The dock hides and shows the sidebar, and
    /// answers a `focus` while it is hidden, bringing it back first; the
    /// sidebar waits for the focus, and keeps it. A binding comes wherever
    /// the focus is, and to a hidden sidebar too, which holds the panes from
    /// before, so zellij says whether the sidebar has the focus.
    fn sidebar_pipe(&mut self, ask: Ask) {
        match ask {
            Ask::Focus if self.hidden() => self.pass.ask(self.frame),
            Ask::Focus if self.has_focus() => self.go_back(),
            Ask::Focus => {
                self.pass.ask(self.frame);
                focus_plugin_pane(self.plugin_id, false, false);
            },
            Ask::Cycle => self.go_to_after(self.has_focus()),
            Ask::Toggle => {},
        }
    }

    /// Like `sidebar_focused`, but asked of zellij: a sidebar that was hidden
    /// still holds the panes from before.
    fn has_focus(&self) -> bool {
        get_focused_pane_info().is_ok_and(|(_, pane)| pane == PaneId::Plugin(self.plugin_id))
    }

    fn hidden(&self) -> bool {
        get_pane_info(PaneId::Plugin(self.plugin_id)).is_some_and(|own| own.is_suppressed)
    }
}

fn contents_of(session: SessionInfo) -> Contents {
    let SessionInfo { tabs, mut panes, .. } = session;
    let mut total = 0;
    let mut focus = None;
    for tab in tabs {
        let in_tab: Vec<PaneInfo> = panes
            .panes
            .remove(&tab.position)
            .unwrap_or_default()
            .into_iter()
            .filter(|pane| pane.is_selectable && !pane.is_suppressed && !pane.is_plugin)
            .filter(|pane| !is_placeholder(pane))
            .collect();
        total += in_tab.len();
        let pick = in_tab.iter().find(|pane| pane.is_focused).or_else(|| in_tab.first());
        if let Some(pane) = pick {
            if focus.is_none() || tab.active {
                focus = Some(Focus {
                    pane: pane.id,
                    tab: tab.name.clone(),
                    title: pane.title.clone(),
                });
            }
        }
    }
    Contents { panes: total, focus }
}

/// The title terminal `pane` gives itself in `manifest`, which zellij sends
/// as it changes, where the session list's is seconds behind.
fn own_title(manifest: Option<&PaneManifest>, pane: u32) -> Option<String> {
    let mut panes = manifest?.panes.values().flatten();
    panes.find(|info| !info.is_plugin && info.id == pane).map(|info| info.title.clone())
}

/// Where this client's focus is, in the tab it is on, last reported, and that
/// tab's panes. zellij marks focused, in each tab, the pane each client was on
/// there last, and only the tabs say which tab this client is on. A tab that
/// marks more than one is shared, and zellij says which is this client's.
fn focus_at(tab: &TabInfo, panes: &[PaneInfo], plugin_id: u32) -> Option<At> {
    if tab.are_floating_panes_visible {
        return Some(At::Floating);
    }
    let id = TabId::of(tab);
    let own = |pane: &PaneInfo| pane.is_plugin && pane.id == plugin_id;
    let mut focused = tiled(panes).filter(|p| p.is_focused);
    let pane = if let (Some(pane), None) = (focused.next(), focused.next()) {
        pane
    } else {
        // zellij names the tab by its id, though it calls it an index. The
        // pane says enough: a pane stands in one tab only.
        let (_, focus) = get_focused_pane_info().ok()?;
        tiled(panes).find(|p| pane_id(p) == focus)?
    };
    if own(pane) {
        return Some(At::Sidebar { tab: id });
    }
    if is_placeholder(pane) {
        return Some(At::Placeholder { tab: id });
    }
    let column = tiled(panes).find(|p| own(p) || is_placeholder(p));
    let beside = column.is_some_and(|c| pane.pane_x == c.pane_x + c.pane_columns);
    Some(At::Pane { tab: id, beside: beside.then_some(pane_id(pane)) })
}

/// The panes of a tab that stand in its columns: neither floating nor hidden.
fn tiled(panes: &[PaneInfo]) -> impl Iterator<Item = &PaneInfo> {
    panes.iter().filter(|p| !p.is_floating && !p.is_suppressed)
}

fn pane_id(pane: &PaneInfo) -> PaneId {
    if pane.is_plugin {
        PaneId::Plugin(pane.id)
    } else {
        PaneId::Terminal(pane.id)
    }
}

/// Hands the sidebar or the picker the row to highlight, as the focus goes to
/// it from the other.
fn hand(plugin_id: u32, place: &Place) {
    let message = MessageToPlugin::new(PLACE_PIPE)
        .with_destination_plugin_id(plugin_id)
        .with_args(place.to_args());
    pipe_message_to_plugin(message);
}

fn resize_self(plugin_id: u32) {
    let (x, y, width, height) = FLOATING;
    let Some(coordinates) = FloatingPaneCoordinates::new(
        Some(x.to_owned()),
        Some(y.to_owned()),
        Some(width.to_owned()),
        Some(height.to_owned()),
        None,
        Some(true),
    ) else {
        return;
    };
    change_floating_panes_coordinates(vec![(PaneId::Plugin(plugin_id), coordinates)]);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::checkouts::Checkout;
    use crate::menu::Shown;

    /// A sidebar with the focus, in the one tab of session `a` beside its
    /// pane, and session `b` with a pane of its own: rows `a`, `pane 1`, `b`,
    /// `pane 4`.
    fn focused_sidebar() -> State {
        let pane = |id: u32, is_plugin: bool| PaneInfo {
            id,
            is_plugin,
            is_focused: is_plugin,
            is_selectable: true,
            title: format!("pane {id}"),
            ..PaneInfo::default()
        };
        let tab = vec![pane(9, true), pane(1, false)];
        let branch = |name: &str, age: u64, panes: Vec<Leaf>| Branch {
            name: name.to_owned(),
            age: Age::from_secs(age),
            panes,
        };
        let four = Leaf {
            id: 4,
            title: "pane 4".to_owned(),
            tab: String::new(),
            tab_id: None,
            cwd: None,
            runs: None,
            checkout: None,
            closes: close::Closes::Pane,
        };
        let mut state = State {
            role: Role::Sidebar(Tree::default()),
            plugin_id: 9,
            panes: Some(PaneManifest { panes: HashMap::from([(0, tab)]) }),
            tabs: vec![TabInfo { position: 0, active: true, ..TabInfo::default() }],
            active_tab: Some(0),
            branches: vec![branch("a", 9, Vec::new()), branch("b", 1, vec![four])],
            size: (10, 30),
            ..State::default()
        };
        state.matches.current_session = Some("a".to_owned());
        state.rebuild_tree();
        state
    }

    /// A key that opens the picker from the sidebar: the floating panes show,
    /// and the picker in them has the focus, and a title once it has its
    /// permissions. Gone to a pane of the tab instead, the sidebar keeps its
    /// row for no picker that comes later.
    #[test]
    fn the_picker_the_focus_leaves_the_sidebar_for_is_handed_its_row() {
        let picker = |title: &str| PaneInfo {
            id: 20,
            is_plugin: true,
            is_floating: true,
            is_focused: true,
            title: title.to_owned(),
            ..PaneInfo::default()
        };
        let four = Place::Pane("b".to_owned(), 4);
        for to_the_picker in [true, false] {
            let mut state = focused_sidebar();
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(3));
            }
            state.rebuild_tree();
            let tab = state.panes.as_mut().and_then(|m| m.panes.get_mut(&0)).expect("tab 0");
            if to_the_picker {
                tab.push(picker("file:luneta.wasm"));
                state.tabs[0].are_floating_panes_visible = true;
            } else {
                tab[0].is_focused = false;
                tab[1].is_focused = true;
            }
            state.rebuild_tree();
            assert_eq!(state.handing(), None, "to the picker: {to_the_picker}");

            let tab = state.panes.as_mut().and_then(|m| m.panes.get_mut(&0)).expect("tab 0");
            tab.retain(|pane| pane.id != 20);
            tab.push(picker(dock::TITLE));
            state.tabs[0].are_floating_panes_visible = true;
            let handed = to_the_picker.then(|| (20, four.clone()));
            assert_eq!(state.handing(), handed, "to the picker: {to_the_picker}");
            assert_eq!(state.handing(), None, "to the picker: {to_the_picker}");
        }
    }

    fn folded(state: &State) -> Vec<&str> {
        let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
        tree.rows
            .iter()
            .filter_map(|node| match node {
                Node::Session(session) if session.folded => Some(session.name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[derive(Debug, Clone, Copy)]
    enum By {
        Enter,
        /// A click on the row's `▾`.
        Fold,
        /// A click on the session's name.
        Name,
    }

    /// A click on a session's `▾` or `▸` folds or unfolds it, wherever the
    /// cursor is. A session that lists its panes heads them, and a click on
    /// its name does nothing. Folded, its row stands in for them: a click on
    /// its name goes there, and so does `Enter`, which for your own is back to
    /// your pane. Found by a term, a session lists its panes, and its fold
    /// does nothing either. Going anywhere closes the search box, open with
    /// nothing typed where no term is.
    #[test]
    fn a_session_row_folds_on_its_fold_and_goes_there_only_once_folded() {
        let row_of = |state: &State, name: &str| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            let heads = |node: &Node| matches!(node, Node::Session(node) if node.name == name);
            tree.rows.iter().position(heads).expect("the session's row")
        };
        for (typed, before, name, by, after, went) in [
            ("", &[][..], "a", By::Fold, &["a"][..], false),
            ("", &[], "a", By::Name, &[], false),
            ("", &[], "b", By::Fold, &["b"], false),
            ("", &[], "b", By::Name, &[], false),
            ("", &["b"], "b", By::Fold, &[], false),
            ("", &["b"], "b", By::Name, &["b"], true),
            ("", &["a"], "a", By::Enter, &["a"], true),
            ("a", &[], "a", By::Fold, &[], false),
            ("a", &[], "a", By::Name, &[], false),
        ] {
            let mut state = focused_sidebar();
            for shut in before {
                let at = row_of(&state, shut);
                if let Role::Sidebar(tree) = &mut state.role {
                    tree.toggle_row(at);
                }
            }
            state.search_tree(typed.to_owned());
            let case = format!("{typed:?}, folded {before:?}, {name} by {by:?}");
            let row = row_of(&state, name);
            // The header and the blank line under it take the first two lines.
            let line = (row + render::HEADER) as isize;
            match by {
                By::Enter => {
                    let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
                    assert_eq!(tree.rows.selected(), Some(row), "{case}");
                    key(&mut state, BareKey::Enter);
                },
                By::Fold => {
                    state.handle_mouse(Mouse::LeftClick(line, 1));
                },
                By::Name => {
                    state.handle_mouse(Mouse::LeftClick(line, 5));
                },
            }
            assert_eq!(folded(&state), after, "{case}");
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            assert_eq!(tree.search().is_none(), went, "{case}");
        }
    }

    /// A click on a session's fold leaves the view where the wheel left it, as
    /// a click on any other row does, though the cursor goes from a pane of
    /// the session to its row as it folds, and on to its first pane as it
    /// unfolds.
    #[test]
    fn a_fold_leaves_the_view_where_the_wheel_left_it() {
        let mut state = focused_sidebar();
        state.size = (9, 30);
        let leaf = |id: u32| Leaf {
            id,
            title: format!("pane {id}"),
            tab: String::new(),
            tab_id: None,
            cwd: None,
            runs: None,
            checkout: None,
            closes: close::Closes::Pane,
        };
        for (name, id) in [("c", 5), ("d", 7), ("e", 9)] {
            let panes = vec![leaf(id), leaf(id + 1)];
            state.branches.push(Branch { name: name.to_owned(), age: Age::from_secs(0), panes });
        }
        state.rebuild_tree();
        let line_of_c = |state: &State| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            let (rows, cols) = state.size;
            let is_c = |at: usize| matches!(&tree.rows[at], Node::Session(node) if node.name == "c");
            (0..rows).find(|line| {
                render::sidebar_row_at(tree, &state.agents, true, rows, cols, *line).is_some_and(is_c)
            })
        };
        // Rows `a`, 1, `b`, 4, `c`, 5, 6, `d`, 7, 8, `e`, 9 and 10, seven to a
        // view: the cursor on pane 5, and the wheel has `c` at the top.
        if let Role::Sidebar(tree) = &mut state.role {
            assert!(tree.rows.select(5));
        }
        state.handle_mouse(Mouse::ScrollDown(4));
        let line = line_of_c(&state).expect("the row of `c` in view");
        assert_eq!(line, render::HEADER);
        for (shut, at) in [(&["c"][..], Place::Session("c".to_owned())), (&[], Place::Pane("c".to_owned(), 5))]
        {
            state.handle_mouse(Mouse::LeftClick(line as isize, 1));
            assert_eq!(folded(&state), shut);
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            assert_eq!(tree.rows.selected_row().map(Node::place), Some(at.clone()), "{at:?}");
            assert_eq!(line_of_c(&state), Some(line), "{at:?}");
        }
    }

    /// A pane's row hands the menu its pane: one of your own session starts
    /// its agent here, one of another session there, in the pane's tab. The
    /// directory is the pane's, as its session says, and the branch the one
    /// checked out there. A folded session's row has its session alone, to
    /// delete.
    #[test]
    fn the_menu_a_row_opens_starts_its_agent_beside_that_pane() {
        let mut state = focused_sidebar();
        state.here = Some(1);
        let four = &mut state.branches[1].panes[0];
        four.cwd = Some("/w/b".to_owned());
        four.tab_id = Some(TabId(7));
        four.checkout = Some(Checkout { branch: "feat/menu".to_owned(), status: String::new() });
        state.rebuild_tree();
        let there = There { session: "b".to_owned(), tab: Some(TabId(7)) };
        let pane = |id, cwd: Option<&str>, branch: Option<&str>| {
            let (cwd, branch) = (cwd.map(str::to_owned), branch.map(str::to_owned));
            Some(RowPane { id, cwd, branch })
        };
        let row = |pane, there| menu::Row { pane, there, back: Some(1) };
        let b = There { session: "b".to_owned(), tab: None };
        let menu_row = |state: &mut State, at| {
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(at));
            }
            state.menu_row()
        };
        assert_eq!(menu_row(&mut state, 1), Some(row(pane(1, None, None), None)));
        let four = row(pane(4, Some("/w/b"), Some("feat/menu")), Some(there));
        assert_eq!(menu_row(&mut state, 3), Some(four));
        if let Role::Sidebar(tree) = &mut state.role {
            tree.toggle_row(0);
            tree.toggle_row(2);
        }
        state.rebuild_tree();
        assert_eq!(menu_row(&mut state, 0), Some(row(None, None)));
        assert_eq!(menu_row(&mut state, 1), Some(row(None, Some(b))));
    }

    /// The groups the menu at the bottom of the sidebar has open, while it
    /// shows.
    fn menu_open(state: &State) -> Option<Vec<&str>> {
        let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
        tree.menu.as_ref().and_then(SidebarMenu::shown).map(|shown| match shown {
            Shown::Keys(keys) | Shown::Asking(keys, _) => keys.names(),
            Shown::Leaving(_) | Shown::Wrong(_) => Vec::new(),
        })
    }

    fn key(state: &mut State, key: BareKey) -> bool {
        state.update(Event::Key(KeyWithModifier::new(key)))
    }

    /// The transcripts of the agents in `agents_in_b`.
    const FOUR: &str = "4444aaaa-0000-4000-8000-000000000004";
    const FIVE: &str = "5555aaaa-0000-4000-8000-000000000005";

    /// A focused sidebar whose session `b` has an agent in each of its panes
    /// 4 and 5, its transcript `FOUR` and `FIVE`: rows `a`, `pane 1`, `b`,
    /// `pane 4`, `pane 5`.
    fn agents_in_b() -> State {
        let mut state = focused_sidebar();
        let leaf = |id: u32| Leaf {
            id,
            title: format!("pane {id}"),
            tab: String::new(),
            tab_id: None,
            cwd: Some("/w/b".to_owned()),
            runs: None,
            checkout: None,
            closes: close::Closes::Pane,
        };
        state.branches[1].panes = vec![leaf(4), leaf(5)];
        state.places = Places::of(&[("b", &[(4, "/w/b"), (5, "/w/b")])]);
        let seated = |pane: u32, id: &str| {
            format!(
                r#"{{"status":"idle","status_age":4,"pid":{pane},"cwd":"/w/b","session_id":"{id}",
                   "zellij":{{"session":"b","pane":"{pane}"}}}}"#
            )
        };
        let claude_ps = format!("[{},{}]", seated(4, FOUR), seated(5, FIVE));
        state.agents.ingest(Some(0), claude_ps.as_bytes(), b"", Moment::default());
        state.rebuild_agents(Selection::Hold);
        state
    }

    /// What is wrong, as the menu at the bottom of the sidebar says it, which
    /// closes.
    fn wrong_said(state: &mut State) -> Option<String> {
        let Role::Sidebar(tree) = &mut state.role else { panic!("not a sidebar") };
        let menu = tree.menu.take();
        menu.as_ref().and_then(SidebarMenu::shown).and_then(|shown| match shown {
            Shown::Wrong(reason) => Some(reason.to_owned()),
            Shown::Keys(_) | Shown::Asking(..) | Shown::Leaving(_) => None,
        })
    }

    /// `b` opens the brief's keys at the bottom of the sidebar, on any row,
    /// and a key of them briefs by the row the cursor is on: `z` every
    /// session, `s` the row's session, and `h` the agent in the row's pane,
    /// or on the row of a folded session, which stands in for its panes, its
    /// session. A brief first reads the end of the transcript of each agent
    /// it covers. With no agent to brief on, the sidebar says so where the
    /// keys were.
    #[test]
    fn the_brief_s_keys_brief_by_the_row_the_cursor_is_on() {
        // Rows `a`, `pane 1`, `b`, `pane 4`, `pane 5`; only `b` has agents.
        // A session's row is one the cursor stops on once it is folded, and
        // the fold takes away none of the rows above it.
        let none_in_a = "brief: no agents in a";
        for (fold, at, scope, briefed) in [
            (false, 1, 'z', Ok(vec![FOUR, FIVE])),
            (false, 1, 's', Err(none_in_a)),
            (false, 1, 'h', Err("brief: no agent in this pane")),
            (false, 3, 's', Ok(vec![FOUR, FIVE])),
            (false, 3, 'h', Ok(vec![FOUR])),
            (false, 4, 'h', Ok(vec![FIVE])),
            (true, 0, 'h', Err(none_in_a)),
            (true, 2, 'h', Ok(vec![FOUR, FIVE])),
        ] {
            let mut state = agents_in_b();
            if fold {
                if let Role::Sidebar(tree) = &mut state.role {
                    assert!(tree.toggle_row(at), "row {at}");
                }
                state.rebuild_tree();
            }
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(at), "row {at}");
            }
            assert!(key(&mut state, BareKey::Char('b')), "row {at}");
            assert_eq!(menu_open(&state), Some(vec!["Brief"]), "row {at}");
            key(&mut state, BareKey::Char(scope));

            let read = state.brief.as_ref().map(Brief::tails).unwrap_or_default();
            let read: Vec<&str> =
                read.iter().map(String::as_str).filter(|arg| [FOUR, FIVE].contains(arg)).collect();
            let said = wrong_said(&mut state);
            let (briefed, reason) = match briefed {
                Ok(briefed) => (briefed, None),
                Err(reason) => (Vec::new(), Some(reason.to_owned())),
            };
            assert_eq!((read, said), (briefed, reason), "row {at} {scope}");
        }
    }

    /// A brief whose transcripts could not be read opens no pane, which
    /// would tell of none of them: the sidebar says why where its keys were.
    #[test]
    fn a_brief_on_transcripts_that_could_not_be_read_says_why() {
        let mut state = agents_in_b();
        let not_read = "brief: the transcripts could not be read";
        for (exit_code, stderr, said) in [
            (Some(2), "sh: 3: tail: not found\n", format!("{not_read}: sh: 3: tail: not found")),
            (None, "", not_read.to_owned()),
        ] {
            key(&mut state, BareKey::Char('b'));
            key(&mut state, BareKey::Char('z'));
            let asked = state.brief.as_ref().map(|brief| brief.number.to_string());
            let answer = BTreeMap::from([
                (CONTEXT_KEY.to_owned(), brief::CONTEXT_VALUE.to_owned()),
                (brief::NUMBER_KEY.to_owned(), asked.expect("a brief asked")),
            ]);
            let ran = Event::RunCommandResult(exit_code, Vec::new(), stderr.into(), answer);
            assert!(state.update(ran), "{said}");
            assert!(state.brief.is_none(), "{said}");
            assert_eq!(wrong_said(&mut state), Some(said));
        }
    }

    /// A brief floats over the tab with the focus, and hands it back to the
    /// sidebar as it closes. It is no row of the sidebar's, nor the pane you
    /// are in: the cursor stays on the row it was asked on, and `Esc` goes
    /// back to the pane you came from, as it did before the brief. zellij
    /// tells of the panes before the tabs, so the sidebar hears the brief is
    /// gone while the tabs still show the floating panes it hid with it.
    #[test]
    fn a_brief_leaves_the_sidebar_as_it_found_it() {
        let mut state = focused_sidebar();
        state.here = Some(1);
        if let Role::Sidebar(tree) = &mut state.role {
            assert!(tree.rows.select(3));
        }
        // The focus you asked for, as `focus` brings it.
        state.pass.ask(state.frame);
        state.update(Event::TabUpdate(state.tabs.clone()));
        let note = brief::Note {
            session: "b".to_owned(),
            pane: 4,
            name: "pane 4".to_owned(),
            status: "idle".to_owned(),
            held: "4s".to_owned(),
            branch: None,
            cwd: None,
            transcript: None,
        };
        let asked = Brief::new(1, Over::Session("b".to_owned()), vec![note]).expect("a brief");
        let command = asked.pane("Brief of b", b"");
        // As zellij spells a command: its path, then each argument after a
        // space.
        let path = command.path.to_string_lossy().into_owned();
        let spelled = std::iter::once(path).chain(command.args).collect::<Vec<_>>().join(" ");
        let pane = PaneInfo {
            id: 2,
            is_floating: true,
            is_focused: true,
            is_selectable: true,
            title: "luneta: brief of b".to_owned(),
            terminal_command: Some(spelled),
            ..PaneInfo::default()
        };
        let closed = state.panes.clone().expect("the panes");
        let mut open = closed.clone();
        open.panes.get_mut(&0).expect("tab 0").push(pane);

        // Rows `a`, `pane 1`, `b`, `pane 4`, and the cursor on `pane 4`.
        let rows = [
            Place::Session("a".to_owned()),
            Place::Pane("a".to_owned(), 1),
            Place::Session("b".to_owned()),
            Place::Pane("b".to_owned(), 4),
        ];
        let as_found = |state: &State, told: &str| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            let shown: Vec<Place> = tree.rows.iter().map(Node::place).collect();
            assert_eq!(shown, rows, "{told}");
            let at = tree.rows.selected_row().map(Node::place);
            assert_eq!(at.as_ref(), rows.get(3), "{told}");
            assert_eq!(state.here, Some(1), "{told}");
        };
        for (brief_open, manifest) in [(true, open), (false, closed)] {
            state.update(Event::PaneUpdate(manifest));
            as_found(&state, &format!("brief open: {brief_open}, as the panes come"));
            let mut tabs = state.tabs.clone();
            tabs[0].are_floating_panes_visible = brief_open;
            state.update(Event::TabUpdate(tabs));
            as_found(&state, &format!("brief open: {brief_open}, as the tabs come"));
        }
    }

    /// A key of the menu opens it at the bottom of the sidebar for the row
    /// the cursor is on, and the menu takes every key until it closes. A key
    /// the menu has not is the sidebar's, and so is a key the sidebar has,
    /// which `m` opens the menu for.
    #[test]
    fn a_key_of_the_menu_opens_it_in_the_sidebar_and_takes_every_key() {
        let mut state = focused_sidebar();
        state.menu_entries = Some(r#"c "Claude" { t "/tigerstyle"; }; k "review""#.to_owned());
        state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
        state.rebuild_tree();
        let selected = |state: &State| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            tree.rows.selected()
        };
        assert!(key(&mut state, BareKey::Char('c')));
        assert_eq!(menu_open(&state), Some(vec!["Claude"]));
        assert!(key(&mut state, BareKey::Esc));
        if let Role::Sidebar(tree) = &mut state.role {
            assert!(tree.rows.select(3));
        }

        assert!(!key(&mut state, BareKey::Char('x')));
        assert_eq!(menu_open(&state), None);
        assert!(key(&mut state, BareKey::Char('c')));
        assert_eq!(menu_open(&state), Some(vec!["Claude"]));
        assert!(key(&mut state, BareKey::Char('k')));
        assert_eq!(selected(&state), Some(3));
        assert_eq!(menu_open(&state), Some(vec!["Claude"]));
        assert!(key(&mut state, BareKey::Backspace));
        assert_eq!(menu_open(&state), Some(vec![]));
        assert!(key(&mut state, BareKey::Esc));
        assert_eq!(menu_open(&state), None);

        // Up from pane 4, over the row of `b`, to pane 1.
        assert!(key(&mut state, BareKey::Char('k')));
        assert_eq!(selected(&state), Some(1));
        assert_eq!(menu_open(&state), None);
        assert!(key(&mut state, BareKey::Char('j')));
        assert!(key(&mut state, BareKey::Char('m')));
        assert_eq!(menu_open(&state), Some(vec![]));
    }

    /// A key that comes soon after the menu starts an agent in the sidebar's
    /// own session was on its way already, and the menu takes it. Once those
    /// keys have all come, the tick closes the menu, and the keys are the
    /// sidebar's again.
    #[test]
    fn the_menu_closes_once_the_keys_on_their_way_have_come() {
        let mut state = focused_sidebar();
        // Off the ticks that ask zellij for the sessions, which a test has no
        // answer to.
        state.frame = 1;
        let selected = |state: &State| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            (tree.rows.selected(), tree.menu.is_some())
        };
        if let Role::Sidebar(tree) = &mut state.role {
            assert!(tree.rows.select(1));
            tree.menu = Some(SidebarMenu::started(state.frame));
        }
        assert!(key(&mut state, BareKey::Char('j')));
        assert_eq!(selected(&state), (Some(1), true));
        for _ in 0..ON_THE_WAY {
            state.update(Event::Timer(TICK));
        }
        assert_eq!(selected(&state), (Some(1), true));
        assert!(state.update(Event::Timer(TICK)));
        assert_eq!(selected(&state), (Some(1), false));
        // Down from pane 1, over the row of `b`, to pane 4.
        assert!(key(&mut state, BareKey::Char('j')));
        assert_eq!(selected(&state), (Some(3), false));
    }

    /// `d d` closes the highlighted pane, with no menu of yours: one of
    /// another session by a command, which the menu waits on. Once the pane
    /// is gone, the cursor is on the row before it, and not back at the top:
    /// its session's, which lists none of its panes now.
    #[test]
    fn d_d_in_the_sidebar_closes_the_highlighted_pane() {
        let mut state = focused_sidebar();
        state.menu_entries = None;
        state.agents.ingest(Some(0), b"[]", b"", Moment::default());
        if let Role::Sidebar(tree) = &mut state.role {
            assert!(tree.rows.select(3));
        }
        assert!(key(&mut state, BareKey::Char('d')));
        assert!(key(&mut state, BareKey::Char('d')));
        assert_eq!(menu_open(&state), Some(vec!["Delete"]));
        let closed = BTreeMap::from([
            (CONTEXT_KEY.to_owned(), menu::CLOSING_VALUE.to_owned()),
            (menu::MENU_KEY.to_owned(), "1".to_owned()),
        ]);
        state.update(Event::RunCommandResult(Some(0), Vec::new(), Vec::new(), closed));
        assert_eq!(menu_open(&state), None);
        state.branches[1].panes.clear();
        state.rebuild_tree();
        let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
        assert_eq!(tree.rows.selected_row().map(Node::place), Some(Place::Session("b".to_owned())));
    }

    /// `n p` on a pane of another session waits for zellij to open your
    /// shell there, `n s` for the name of the session it starts, and `n t`
    /// for zellij to list the clients of that session, and once the focus has
    /// gone, to list one more: the sidebar hands the menu each answer, and the
    /// menu goes, rather than wait for good.
    #[test]
    fn the_new_keys_in_the_sidebar_wait_for_their_answers() {
        let header = "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n";
        let come = format!("{header}1 terminal_2 sh");
        for (new, value, said) in [
            ('p', menu::OPENING_VALUE, ""),
            ('s', menu::NAMING_VALUE, ""),
            ('t', close::CLIENTS_VALUE, come.as_str()),
        ] {
            let mut state = focused_sidebar();
            state.menu_entries = None;
            state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
            state.rebuild_tree();
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(3), "n {new}");
            }
            assert!(key(&mut state, BareKey::Char('n')), "n {new}");
            key(&mut state, BareKey::Char(new));
            let answer = |value: &str| {
                BTreeMap::from([
                    (CONTEXT_KEY.to_owned(), value.to_owned()),
                    (menu::MENU_KEY.to_owned(), "1".to_owned()),
                ])
            };
            if new == 't' {
                // The session has no client before you go.
                let before = header.as_bytes().to_vec();
                let counted = answer(menu::ARRIVING_VALUE);
                state.update(Event::RunCommandResult(Some(0), before, Vec::new(), counted));
                focus(&mut state, false);
                assert!(state.leave_session());
            }
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            assert!(tree.menu.is_some(), "n {new}");
            let said = said.as_bytes().to_vec();
            state.update(Event::RunCommandResult(Some(0), said, Vec::new(), answer(value)));
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            assert!(tree.menu.is_none(), "n {new}");
        }
    }

    /// The tab `n t` opens once you have come to another session answers a
    /// sidebar whose menu is gone: when it does not open, the sidebar says
    /// why, for when you come back, as zellij said it or else that it opened
    /// none.
    #[test]
    fn a_tab_that_did_not_open_in_another_session_is_said_by_the_sidebar() {
        let opened = |exit_code, stderr: &[u8]| {
            let mut state = focused_sidebar();
            let context = BTreeMap::from([
                (CONTEXT_KEY.to_owned(), menu::TAB_VALUE.to_owned()),
                (places::SESSION_KEY.to_owned(), "b".to_owned()),
            ]);
            state.update(Event::RunCommandResult(exit_code, Vec::new(), stderr.to_vec(), context));
            state.error
        };
        assert_eq!(opened(Some(0), b""), None);
        let refused = opened(Some(1), b"Session 'b' not found\n");
        assert_eq!(refused.as_deref(), Some("new tab in \"b\": Session 'b' not found"));
        assert_eq!(opened(None, b"").as_deref(), Some("new tab in \"b\": zellij opened none"));
    }

    /// `d s` on a pane of the session you are in, and `d d` on its row, once
    /// it is folded, go to another session first, and the menu outlasts the
    /// focus on the way, to kill the session once the client is gone. With no
    /// other session, it says so.
    #[test]
    fn deleting_the_session_you_are_in_goes_to_another_first() {
        let cases = [(false, false, 's'), (true, false, 's'), (false, true, 'd'), (true, true, 'd')];
        for (others, folded, then) in cases {
            let mut state = focused_sidebar();
            state.agents.ingest(Some(0), b"[]", b"", Moment::default());
            if others {
                state.sessions.live =
                    vec![Session { name: "b".to_owned(), age: Age::from_secs(1) }];
                state.sidebars.insert("b".to_owned(), 7);
                // You land in its sidebar, as you leave from this one.
                let refuge = state.closing(None, None).and_then(|subject| subject.refuge);
                let into = close::Refuge { session: "b".to_owned(), sidebar: Some(7) };
                assert_eq!(refuge, Some(into));
            }
            if let Role::Sidebar(tree) = &mut state.role {
                if folded {
                    tree.toggle_row(0);
                } else {
                    assert!(tree.rows.select(1));
                }
            }
            state.rebuild_tree();
            let case = format!("others: {others}, folded: {folded}, d {then}");
            assert!(key(&mut state, BareKey::Char('d')), "{case}");
            assert!(key(&mut state, BareKey::Char(then)), "{case}");
            if others {
                focus(&mut state, false);
            }
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            match tree.menu.as_ref().and_then(SidebarMenu::shown) {
                Some(Shown::Leaving(to)) => assert!(others && to == "b", "{case}"),
                Some(Shown::Wrong(said)) => {
                    assert!(!others && said.contains("only session"), "{case}");
                },
                _ => panic!("{case}: neither leaving nor wrong"),
            }
            if others {
                // The session ends once zellij lists the sidebar's client, 1,
                // out of it, for the menu that asked, numbered 1: what zellij
                // says for another menu lists another session's clients.
                state.client_id = 1;
                let listed = |number: &str, clients: &str| {
                    let context = BTreeMap::from([
                        (CONTEXT_KEY.to_owned(), close::CLIENTS_VALUE.to_owned()),
                        (menu::MENU_KEY.to_owned(), number.to_owned()),
                    ]);
                    let stdout = format!("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n{clients}");
                    Event::RunCommandResult(Some(0), stdout.into_bytes(), Vec::new(), context)
                };
                assert!(!state.update(listed("2", "")), "{case}");
                assert!(!state.update(listed("1", "1         plugin_3       N/A\n")), "{case}");
                assert!(state.update(listed("1", "")), "{case}");
                let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
                assert!(tree.menu.is_none(), "{case}");
            }
        }
    }

    /// The menu is for the row the cursor was on, and lasts while the sidebar
    /// has the focus. A click goes where it goes, and closes it.
    #[test]
    fn the_menu_in_the_sidebar_closes_with_the_focus_or_a_click() {
        for by_click in [false, true] {
            let mut state = focused_sidebar();
            state.menu_entries = Some(r#"c "Claude" { t "/tigerstyle"; }"#.to_owned());
            state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
            state.rebuild_tree();
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(3));
            }
            assert!(key(&mut state, BareKey::Char('c')));
            assert!(menu_open(&state).is_some(), "by click: {by_click}");
            if by_click {
                // On the `▾` of `b`, the third row.
                assert!(state.handle_mouse(Mouse::LeftClick((render::HEADER + 2) as isize, 1)));
                assert_eq!(folded(&state), ["b"]);
            } else {
                focus(&mut state, false);
            }
            assert_eq!(menu_open(&state), None, "by click: {by_click}");
        }
    }

    /// The menu closes with the pane of your own session it is for, as the
    /// row does: zellij would open the agent beside the sidebar instead. A
    /// pane of another session is none of the panes your session reports.
    #[test]
    fn the_menu_in_the_sidebar_closes_with_its_pane() {
        let mut state = focused_sidebar();
        state.menu_entries = Some(r#"c "Claude" { t "/tigerstyle"; }"#.to_owned());
        state.branches[0].panes[0].cwd = Some("/w/a".to_owned());
        state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
        state.rebuild_tree();
        let panes = state.panes.clone().expect("the panes");
        let mut closed = panes.clone();
        closed.panes.values_mut().for_each(|tab| tab.retain(|pane| pane.id != 1));
        // Pane 1 is of your own session, and pane 4 of `b`.
        for (at, open) in [(1, false), (3, true)] {
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(at));
            }
            assert!(key(&mut state, BareKey::Char('c')));
            assert!(menu_open(&state).is_some(), "row {at}");
            state.update(Event::PaneUpdate(closed.clone()));
            assert_eq!(menu_open(&state).is_some(), open, "row {at}");
            state.update(Event::PaneUpdate(panes.clone()));
        }
    }

    /// `b` opens help at the bottom of the sidebar, and `Backspace` its top,
    /// with `/` over the menu's keys, on a pane's row and on a session's. A
    /// command in it closes it and runs as it does pressed at the top of the
    /// sidebar: `/` opens the search.
    #[test]
    fn a_command_of_help_runs_as_it_does_at_the_top_of_the_sidebar() {
        let shown = |state: &State| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            match tree.menu.as_ref().and_then(SidebarMenu::shown) {
                Some(Shown::Keys(keys)) => keys.entries().iter().map(menu::Entry::key).collect(),
                _ => Vec::new(),
            }
        };
        // Row 0 is the session `a`, folded, and row 2 pane 4 of `b`.
        for (at, through_help) in [(0, false), (0, true), (2, true)] {
            let mut state = focused_sidebar();
            state.menu_entries = Some(r#"c "Claude" { t "/tigerstyle"; }"#.to_owned());
            state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
            if let Role::Sidebar(tree) = &mut state.role {
                tree.toggle_row(0);
            }
            state.rebuild_tree();
            if let Role::Sidebar(tree) = &mut state.role {
                assert!(tree.rows.select(at));
            }
            if through_help {
                assert!(key(&mut state, BareKey::Char('b')), "row {at}");
                key(&mut state, BareKey::Backspace);
                assert_eq!(shown(&state), ['/', 'c', 'n', 'd', 'b'], "row {at}");
            }
            assert!(key(&mut state, BareKey::Char('/')), "row {at}, through help: {through_help}");
            assert_eq!(menu_open(&state), None, "row {at}, through help: {through_help}");
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            assert_eq!(tree.search(), Some(""), "row {at}, through help: {through_help}");
        }
    }

    /// The reports of the focus on the sidebar, or on the pane beside it.
    fn focus(state: &mut State, sidebar: bool) {
        let tab = state.panes.as_mut().and_then(|m| m.panes.get_mut(&0)).expect("tab 0");
        tab[0].is_focused = sidebar;
        tab[1].is_focused = !sidebar;
        let manifest = state.panes.clone().expect("the panes");
        state.update(Event::PaneUpdate(manifest));
        state.update(Event::TabUpdate(state.tabs.clone()));
    }

    /// zellij hands the click that focuses the sidebar to no one, but ends
    /// the hover over it first, where the click was: that click does what
    /// any other does. A hover that ended long before a key brought the focus
    /// was the pointer leaving, and does nothing, and so does one that a key
    /// came after, however soon.
    #[test]
    fn the_click_that_focuses_the_sidebar_acts_on_its_row() {
        for (late, keyed) in [(false, false), (true, false), (false, true)] {
            let mut state = focused_sidebar();
            focus(&mut state, false);
            // On the `▾` of `b`, the third row.
            state.handle_mouse(Mouse::Hover((render::HEADER + 2) as isize, 1));
            state.handle_mouse(Mouse::Hover(0, usize::from(u16::MAX)));
            if late {
                state.frame += TICKS_PER_SECOND;
            }
            if keyed {
                state.update(Event::InputReceived);
            }
            focus(&mut state, true);
            let shut: &[&str] = if late || keyed { &[] } else { &["b"] };
            assert_eq!(folded(&state), shut, "late: {late}, keyed: {keyed}");
        }
    }

    /// A pointer that moves on the sidebar, or scrolls it, is no click: a key
    /// that moves the focus in after it passes over the column. A click that
    /// comes through acts as it comes, and only then, whatever hover's end
    /// came before it.
    #[test]
    fn only_the_hover_s_end_or_a_click_through_is_a_click() {
        // On the `▾` of `b`, the third row.
        let line = (render::HEADER + 2) as isize;
        let end = Mouse::Hover(0, usize::from(u16::MAX));
        for (case, mice, shut, passed) in [
            (
                "moved, and scrolled",
                vec![Mouse::Hover(line, 1), Mouse::ScrollDown(1)],
                &[][..],
                true,
            ),
            ("through", vec![Mouse::Hover(line, 1), end, Mouse::LeftClick(line, 1)], &["b"], false),
        ] {
            let mut state = focused_sidebar();
            focus(&mut state, false);
            for mouse in mice {
                state.handle_mouse(mouse);
            }
            focus(&mut state, true);
            assert_eq!(folded(&state), shut, "{case}");
            assert_eq!(state.pass.passing(), passed, "{case}");
        }
    }
}
