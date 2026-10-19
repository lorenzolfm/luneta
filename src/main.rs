mod agents;
mod brief;
mod checkouts;
mod close;
mod cursor;
mod dirs;
mod dock;
mod elapsed;
mod fetch;
mod folds;
mod hiding;
mod layout;
mod menu;
mod paint;
mod panes;
mod pass;
mod places;
mod render;
mod sessions;
mod tree;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentRow, AgentSet, Live, Seat};
use brief::{Brief, Over, Scope};
use checkouts::Checkouts;
use dirs::{DirSet, LIST};
use dock::{Ask, TabId};
use menu::{Command, Did, SidebarMenu, There};
use elapsed::Age;
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

/// The title the picker gives its pane, by which the sidebar knows it.
const PICKER_TITLE: &str = "luneta";

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
            Some(SIDEBAR) => Role::Sidebar(Tree::shared()),
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
                        menu.ran(exit_code, &stdout, &stderr, &context);
                        false
                    },
                    _ => false,
                };
            },
            Role::Unknown(_) => return false,
        }
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    let title = if self.is_sidebar() { dock::SIDEBAR_TITLE } else { PICKER_TITLE };
                    rename_plugin_pane(self.plugin_id, title);
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
                polled || self.spinning() || asked || left
            },
            Event::Key(key) if self.is_sidebar() => self.handle_sidebar_key(&key),
            Event::Key(key) => self.handle_key(&key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::InputReceived => {
                self.pass.input();
                false
            },
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                match context.get(dirs::CONTEXT_KEY).map(String::as_str) {
                    Some(dirs::CONTEXT_VALUE) => {
                        self.dirs.ingest(exit_code, &stdout, &stderr);
                        if let Some(command) = self.dirs.worktrees_query() {
                            run_command(
                                &command,
                                BTreeMap::from([(
                                    dirs::CONTEXT_KEY.to_owned(),
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
                    Some(agents::CONTEXT_VALUE) => {
                        self.agents.ingest(exit_code, &stdout, &stderr);
                        self.agents_taken_at = self.frame;
                        self.rebuild_agents(Selection::Hold);
                        true
                    },
                    Some(brief::CONTEXT_VALUE) => {
                        let number = context.get(brief::NUMBER_KEY);
                        let asked = |brief: &mut Brief| number == Some(&brief.number.to_string());
                        if let Some(brief) = self.brief.take_if(asked) {
                            brief.open(&stdout);
                        }
                        false
                    },
                    Some(menu::START_VALUE | menu::CLOSING_VALUE) => {
                        let Role::Sidebar(tree) = &mut self.role else {
                            return false;
                        };
                        let answered = |menu: SidebarMenu| {
                            menu.answered(exit_code, &stdout, &stderr, &context)
                        };
                        tree.menu = tree.menu.take().and_then(answered);
                        true
                    },
                    Some(close::CLIENTS_VALUE) => self.listed(exit_code, &stdout),
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
            self.away = dock::attached() == Some(false);
            if self.away {
                return;
            }
            self.seat_from(&snapshot);
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
        for session in snapshot.live_sessions {
            live_names.push(session.name.clone());
            if session.is_current_session {
                continue;
            }
            let name = session.name.clone();
            let age = Age::new(session.creation_time);
            contents.insert(name.clone(), contents_of(session));
            live.push(Session { name, age });
        }
        self.live_names = live_names;
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

    fn spinning(&self) -> bool {
        let shown = (self.is_sidebar() && !self.away)
            || (matches!(self.mode, Mode::Search) && self.screen == Screen::Agents);
        shown && self.agents.any_busy()
    }

    fn origin_pane(&self) -> Option<u32> {
        let tab = self.active_tab?;
        origin(self.panes.as_ref()?.panes.get(&tab)?)
    }

    fn rebuild_agents(&mut self, policy: Selection) {
        let origin = if self.is_sidebar() { None } else { self.origin_pane() };
        let since = self.agents_since();
        let current = self.matches.current_session.as_deref();
        let live = Live::new(current, &self.sessions.live, &self.places);
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
                    (dirs::CONTEXT_KEY.to_owned(), places::CONTEXT_VALUE.to_owned()),
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
                    (dirs::CONTEXT_KEY.to_owned(), checkouts::CONTEXT_VALUE.to_owned()),
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
                        (dirs::CONTEXT_KEY.to_owned(), dirs::PREVIEW_VALUE.to_owned()),
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
                        (dirs::CONTEXT_KEY.to_owned(), panes::CONTEXT_VALUE.to_owned()),
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
            BTreeMap::from([(dirs::CONTEXT_KEY.to_owned(), dirs::CONTEXT_VALUE.to_owned())]),
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
            Kind::Resurrectable => ("delete", delete_dead_session(&name)),
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
            (agents::CONTEXT_KEY.to_owned(), agents::CLOSE_VALUE.to_owned()),
            (agents::LABEL_KEY.to_owned(), row.label()),
        ]));
        self.agents.forget_selected();
        self.set_term(String::new());
    }

    /// The panes of `session` you can go to, as zellij lists them now: the
    /// picker keeps none.
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
        let floating = self
            .tabs
            .iter()
            .any(|t| t.position == tab && t.are_floating_panes_visible);
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
        panes.filter(|p| p.is_plugin && p.title == dock::SIDEBAR_TITLE).map(|p| p.id).min()
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
        let follow = !self.sidebar_focused();
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
            .filter(|p| p.is_plugin && p.title == PICKER_TITLE)
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
        // as an agent it started takes the focus.
        let frame = self.frame;
        let lapsed = |menu: &SidebarMenu| {
            menu.started_at().is_some_and(|at| frame.wrapping_sub(at) > ON_THE_WAY)
        };
        let subject = self.menu_subject();
        if let Role::Sidebar(tree) = &mut self.role {
            if let Some(menu) = tree.menu.take().filter(|menu| !lapsed(menu)) {
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
        // A key that does more than move the cursor, fold or open the menu
        // is a `Command`, which it comes to by `Command::of` below, for `?`
        // to list it.
        let drawn = match key.bare_key {
            BareKey::Down | BareKey::Char('j') => self.move_tree(1),
            BareKey::Up | BareKey::Char('k') => self.move_tree(-1),
            BareKey::Left | BareKey::Char('h') => self.fold(Some(true)),
            BareKey::Right | BareKey::Char('l') => self.fold(Some(false)),
            BareKey::Char(' ') => self.fold(None),
            BareKey::Char('m') => self.open_menu(None),
            BareKey::Char('?') => self.open_help(None),
            BareKey::Enter => {
                self.go_to_selected();
                true
            },
            BareKey::Esc => {
                self.go_back();
                false
            },
            BareKey::Char(c) => match Command::of(c) {
                Some(command) => self.command(command),
                None if Command::opens(c) => self.open_help(Some(c)),
                None => self.open_menu(Some(c)),
            },
            _ => false,
        };
        drawn || unscrolled
    }

    /// Runs a command of the sidebar's own: a key at the top of the sidebar,
    /// or a key of `?`. Says whether to draw.
    fn command(&mut self, command: Command) -> bool {
        match command {
            Command::Search => self.search_tree(String::new()),
            Command::Next => {
                // A key comes only to the sidebar with the focus.
                self.go_to_next(true);
                false
            },
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

    /// Opens `?` at the bottom of the sidebar for the highlighted row: the
    /// keys at the top of the menu, and the sidebar's commands, on the group
    /// of commands `group` opens, or on the first. Says whether to draw.
    fn open_help(&mut self, group: Option<char>) -> bool {
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
                        (dirs::CONTEXT_KEY.to_owned(), brief::CONTEXT_VALUE.to_owned()),
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
    /// killed: asks zellij whether the client is gone, for `listed` to kill
    /// the session once it is. Says whether to draw.
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

    /// What zellij said, as it listed the clients of the session, while the
    /// menu takes the client out of it: the session is killed once the client
    /// is gone. Says whether to draw.
    fn listed(&mut self, exit_code: Option<i32>, stdout: &[u8]) -> bool {
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        if !tree.menu.as_ref().is_some_and(SidebarMenu::lasts) {
            return false;
        }
        let client = self.client_id;
        tree.menu = tree.menu.take().and_then(|menu| menu.clients(client, exit_code, stdout));
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
    /// and the session to go to before the session you are in is killed.
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
        Some(close::Subject::new(seat, pane, leaves, agents, close::refuge(others)))
    }

    /// The highlighted row, for the menu: its pane, the directory it works
    /// in, as its session says or else as its agent does, the branch checked
    /// out there, and when that is another session, its session and its tab,
    /// and the pane you came from, for the menu to hand the focus back to
    /// before it goes there. A session's row has its session alone.
    fn menu_row(&self) -> Option<menu::Row> {
        let Role::Sidebar(tree) = &self.role else {
            return None;
        };
        let current = self.matches.current_session.as_deref();
        let (session, pane) = match tree.rows.selected_row().map(Node::place)? {
            Place::Pane(session, pane) => (session, pane),
            Place::Session(session) => {
                let here = current == Some(session.as_str());
                let there = (!here).then_some(There { session, tab: None, back: self.here });
                return Some(menu::Row { pane: None, cwd: None, branch: None, there });
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
        let there = (!here).then_some(There { session, tab, back: self.here });
        Some(menu::Row { pane: Some(pane), cwd, branch, there })
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
    /// session, and anywhere else it goes there, which for the session you
    /// are in is the pane you came from. A term unfolds what it finds, so a
    /// fold would show nothing, and the whole row goes there.
    fn press(&mut self, (place, fold): (Place, bool)) -> bool {
        let (rows, cols) = self.size;
        let focused = self.sidebar_focused();
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        let Some(index) = tree.rows.iter().position(|node| node.place() == place) else {
            return false;
        };
        render::select_sidebar_row(tree, &self.agents, focused, rows, cols, index);
        if fold && !tree.narrowed() {
            return self.fold(None);
        }
        let current = self.matches.current_session.as_deref();
        if matches!(&place, Place::Session(session) if Some(session.as_str()) == current) {
            self.close_search();
            self.go_back();
        } else {
            self.go_to_selected();
        }
        true
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
    /// shows every row. The session you are in, found by a term, is the pane
    /// you came from; with nothing typed, `Enter` on its row folds it. A key
    /// or a click goes here, and both come only to the sidebar with the focus.
    fn go_to_selected(&mut self) {
        let Role::Sidebar(tree) = &self.role else {
            return;
        };
        let Some(place) = tree.rows.selected_row().map(Node::place) else {
            return;
        };
        let found = tree.narrowed();
        self.close_search();
        let current = self.matches.current_session.as_deref();
        match place {
            Place::Pane(session, pane) => self.go_to(&session, pane, true),
            Place::Session(session) if current == Some(session.as_str()) => {
                if found {
                    self.go_back();
                } else {
                    self.fold(None);
                }
            },
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

    fn go_to_next(&self, focused: bool) {
        let here = self.matches.current_session.as_deref().zip(self.pane_now());
        if let Some(row) = self.agents.next_wanted(here) {
            self.go_to(row.seat.session(), row.pane, focused);
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
            Ask::Next => self.go_to_next(self.has_focus()),
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
                tree.rows.select(3);
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
            tab.push(picker(PICKER_TITLE));
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

    /// With nothing typed, a click on a session's `▾` folds it, and so does
    /// `Enter` on your own. A click on its name goes there, which for your
    /// own is back to your pane. Found by a term, every row goes there.
    #[test]
    fn a_session_row_folds_on_its_fold_unless_a_term_found_it() {
        for (typed, row, by, shut) in [
            ("", 0, By::Enter, &["a"][..]),
            ("", 0, By::Fold, &["a"]),
            ("", 0, By::Name, &[]),
            ("", 2, By::Fold, &["b"]),
            ("", 2, By::Name, &[]),
            ("a", 0, By::Enter, &[]),
            ("a", 0, By::Fold, &[]),
        ] {
            let mut state = focused_sidebar();
            state.search_tree(typed.to_owned());
            // The header and the blank line under it take the first two lines.
            let line = (row + render::HEADER) as isize;
            match by {
                By::Enter => {
                    if let Role::Sidebar(tree) = &mut state.role {
                        tree.rows.select(row);
                    }
                    state.go_to_selected();
                },
                By::Fold => {
                    state.handle_mouse(Mouse::LeftClick(line, 1));
                },
                By::Name => {
                    state.handle_mouse(Mouse::LeftClick(line, 5));
                },
            }
            assert_eq!(folded(&state), shut, "{typed:?}, row {row}, {by:?}");
            if !typed.is_empty() {
                let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
                assert_eq!(tree.search(), None, "{typed:?}, row {row}, {by:?}");
            }
        }
    }

    /// A pane's row hands the menu its pane: one of your own session starts
    /// its agent here, one of another session there, in the pane's tab. The
    /// directory is the pane's, as its session says, and the branch the one
    /// checked out there. A session's row has its session alone, to delete.
    #[test]
    fn the_menu_a_row_opens_starts_its_agent_beside_that_pane() {
        let mut state = focused_sidebar();
        state.here = Some(1);
        let four = &mut state.branches[1].panes[0];
        four.cwd = Some("/w/b".to_owned());
        four.tab_id = Some(TabId(7));
        four.checkout = Some(Checkout { branch: "feat/menu".to_owned(), status: String::new() });
        state.rebuild_tree();
        let there = There { session: "b".to_owned(), tab: Some(TabId(7)), back: Some(1) };
        let row = |pane, cwd: Option<&str>, branch: Option<&str>, there| menu::Row {
            pane,
            cwd: cwd.map(str::to_owned),
            branch: branch.map(str::to_owned),
            there,
        };
        let b = There { session: "b".to_owned(), tab: None, back: Some(1) };
        for (at, expected) in [
            (0, Some(row(None, None, None, None))),
            (1, Some(row(Some(1), None, None, None))),
            (2, Some(row(None, None, None, Some(b)))),
            (3, Some(row(Some(4), Some("/w/b"), Some("feat/menu"), Some(there)))),
        ] {
            if let Role::Sidebar(tree) = &mut state.role {
                tree.rows.select(at);
            }
            assert_eq!(state.menu_row(), expected, "row {at}");
        }
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

    /// `b` opens the brief's keys at the bottom of the sidebar, on any row,
    /// and a key of them briefs by the row the cursor is on: `z` every
    /// session, `s` the row's session, and `h` the agent in the row's pane,
    /// or on a session's row, its session. A brief first reads the end of the
    /// transcript of each agent it covers. With no agent to brief on, the
    /// sidebar says so where the keys were.
    #[test]
    fn the_brief_s_keys_brief_by_the_row_the_cursor_is_on() {
        const FOUR: &str = "4444aaaa-0000-4000-8000-000000000004";
        const FIVE: &str = "5555aaaa-0000-4000-8000-000000000005";
        let mut state = focused_sidebar();
        let leaf = |id: u32| Leaf {
            id,
            title: format!("pane {id}"),
            tab: String::new(),
            tab_id: None,
            cwd: Some("/w/b".to_owned()),
            checkout: None,
            closes: close::Closes::Pane,
        };
        state.branches[1].panes = vec![leaf(4), leaf(5)];
        state.places = Places::of(&[("b", &[(4, "/w/b"), (5, "/w/b")])]);
        let seated = |pane: u32, id: &str| {
            format!(
                r#"{{"status":"idle","status_age":4,"cwd":"/w/b","session_id":"{id}",
                   "zellij":{{"session":"b","pane":"{pane}"}}}}"#
            )
        };
        let claude_ps = format!("[{},{}]", seated(4, FOUR), seated(5, FIVE));
        state.agents.ingest(Some(0), claude_ps.as_bytes(), b"");
        state.rebuild_agents(Selection::Hold);

        // Rows `a`, `pane 1`, `b`, `pane 4`, `pane 5`; only `b` has agents.
        let none_in_a = "brief: no agents in a";
        for (at, scope, briefed) in [
            (0, 'z', Ok(vec![FOUR, FIVE])),
            (0, 's', Err(none_in_a)),
            (0, 'h', Err(none_in_a)),
            (1, 'h', Err("brief: no agent in this pane")),
            (2, 'h', Ok(vec![FOUR, FIVE])),
            (3, 's', Ok(vec![FOUR, FIVE])),
            (3, 'h', Ok(vec![FOUR])),
            (4, 'h', Ok(vec![FIVE])),
        ] {
            state.brief = None;
            if let Role::Sidebar(tree) = &mut state.role {
                tree.rows.select(at);
            }
            assert!(key(&mut state, BareKey::Char('b')), "row {at}");
            assert_eq!(menu_open(&state), Some(vec!["Brief"]), "row {at}");
            key(&mut state, BareKey::Char(scope));

            let read = state.brief.as_ref().map(Brief::tails).unwrap_or_default();
            let read: Vec<&str> =
                read.iter().map(String::as_str).filter(|arg| [FOUR, FIVE].contains(arg)).collect();
            let Role::Sidebar(tree) = &mut state.role else { panic!("not a sidebar") };
            let menu = tree.menu.take();
            let said = menu.as_ref().and_then(SidebarMenu::shown).and_then(|shown| match shown {
                Shown::Wrong(reason) => Some(reason),
                Shown::Keys(_) | Shown::Asking(..) | Shown::Leaving(_) => None,
            });
            let (briefed, reason) = match briefed {
                Ok(briefed) => (briefed, None),
                Err(reason) => (Vec::new(), Some(reason)),
            };
            assert_eq!((read, said), (briefed, reason), "row {at} {scope}");
        }
    }

    /// A key of the menu opens it at the bottom of the sidebar for the row
    /// the cursor is on, and the menu takes every key until it closes. A key
    /// the menu has not is the sidebar's, and so is a key the sidebar has,
    /// which `m` opens the menu for. A session's row opens it too, for the
    /// session.
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
            tree.rows.select(3);
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

        assert!(key(&mut state, BareKey::Char('k')));
        assert_eq!(selected(&state), Some(2));
        assert_eq!(menu_open(&state), None);
        assert!(key(&mut state, BareKey::Char('j')));
        assert!(key(&mut state, BareKey::Char('m')));
        assert_eq!(menu_open(&state), Some(vec![]));
    }

    /// `d d` closes the highlighted pane, with no menu of yours: one of
    /// another session by a command, which the menu waits on. Once the pane
    /// is gone, the cursor is on the row before it, and not back at the top.
    #[test]
    fn d_d_in_the_sidebar_closes_the_highlighted_pane() {
        let mut state = focused_sidebar();
        state.menu_entries = None;
        state.agents.ingest(Some(0), b"[]", b"");
        if let Role::Sidebar(tree) = &mut state.role {
            tree.rows.select(3);
        }
        assert!(key(&mut state, BareKey::Char('d')));
        assert!(key(&mut state, BareKey::Char('d')));
        assert_eq!(menu_open(&state), Some(vec!["Delete"]));
        let closed = BTreeMap::from([
            (agents::CONTEXT_KEY.to_owned(), menu::CLOSING_VALUE.to_owned()),
            (menu::MENU_KEY.to_owned(), "1".to_owned()),
        ]);
        state.update(Event::RunCommandResult(Some(0), Vec::new(), Vec::new(), closed));
        assert_eq!(menu_open(&state), None);
        state.branches[1].panes.clear();
        state.rebuild_tree();
        let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
        assert_eq!(tree.rows.selected_row().map(Node::place), Some(Place::Session("b".to_owned())));
    }

    /// `d s` on a pane of the session you are in, and `d d` on its row, go to
    /// another session first, and the menu outlasts the focus on the way, to
    /// kill the session once the client is gone. With no other session, it
    /// says so.
    #[test]
    fn deleting_the_session_you_are_in_goes_to_another_first() {
        for (others, row, then) in [(false, 1, 's'), (true, 1, 's'), (false, 0, 'd'), (true, 0, 'd')]
        {
            let mut state = focused_sidebar();
            state.agents.ingest(Some(0), b"[]", b"");
            if others {
                state.sessions.live =
                    vec![Session { name: "b".to_owned(), age: Age::from_secs(1) }];
            }
            if let Role::Sidebar(tree) = &mut state.role {
                tree.rows.select(row);
            }
            let case = format!("others: {others}, row {row}, d {then}");
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
                // out of it.
                state.client_id = 1;
                let listed = |clients: &str| {
                    let context = BTreeMap::from([(
                        agents::CONTEXT_KEY.to_owned(),
                        close::CLIENTS_VALUE.to_owned(),
                    )]);
                    let stdout = format!("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n{clients}");
                    Event::RunCommandResult(Some(0), stdout.into_bytes(), Vec::new(), context)
                };
                assert!(!state.update(listed("1         plugin_3       N/A\n")), "{case}");
                assert!(state.update(listed("")), "{case}");
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
                tree.rows.select(3);
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
                tree.rows.select(at);
            }
            assert!(key(&mut state, BareKey::Char('c')));
            assert!(menu_open(&state).is_some(), "row {at}");
            state.update(Event::PaneUpdate(closed.clone()));
            assert_eq!(menu_open(&state).is_some(), open, "row {at}");
            state.update(Event::PaneUpdate(panes.clone()));
        }
    }

    /// `?` opens the menu at the bottom of the sidebar with the sidebar's
    /// commands under its keys, on a pane's row and on a session's. A command
    /// in it closes it and runs as it does pressed at the top of the sidebar:
    /// `/` opens the search.
    #[test]
    fn a_command_of_help_runs_as_it_does_at_the_top_of_the_sidebar() {
        let shown = |state: &State| {
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            match tree.menu.as_ref().and_then(SidebarMenu::shown) {
                Some(Shown::Keys(keys)) => keys.entries().iter().map(menu::Entry::key).collect(),
                _ => Vec::new(),
            }
        };
        // Row 0 is the session `a`, and row 3 pane 4 of `b`.
        for (at, through_help) in [(0, false), (0, true), (3, true)] {
            let mut state = focused_sidebar();
            state.menu_entries = Some(r#"c "Claude" { t "/tigerstyle"; }"#.to_owned());
            state.branches[1].panes[0].cwd = Some("/w/b".to_owned());
            state.rebuild_tree();
            if let Role::Sidebar(tree) = &mut state.role {
                tree.rows.select(at);
            }
            if through_help {
                assert!(key(&mut state, BareKey::Char('?')), "row {at}");
                assert_eq!(shown(&state), ['c', 'd', '/', 'n', 'b'], "row {at}");
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
