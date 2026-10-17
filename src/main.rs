mod agents;
mod cursor;
mod dirs;
mod dock;
mod elapsed;
mod fetch;
mod layout;
mod paint;
mod panes;
mod pass;
mod places;
mod render;
mod sessions;
mod tree;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentSet, Live, Seat};
use dirs::{DirSet, LIST};
use dock::Ask;
use elapsed::Age;
use panes::Peeks;
use pass::{At, On, Pass};
use places::Places;
use sessions::{validate_name, Contents, Focus, Kind, MatchSet, Selection, Session, Sessions};
use tree::{is_placeholder, Branch, Node, Place, Tree};
use zellij_tile::prelude::*;

const SCREEN_PIPE: &str = "screen";

/// The pipe a key binding reaches the sidebar through, its payload an `Ask`.
const SIDEBAR_PIPE: &str = "sidebar";

/// `mode "dock"`, in `load_plugins`, makes the plugin the dock: one to a
/// session, in the background, keeping a sidebar in the tab on screen. The
/// sidebar it opens, `mode "sidebar"`, is a pane that never closes itself.
const MODE: &str = "mode";

const SIDEBAR: &str = "sidebar";

const DOCK: &str = "dock";

/// What a load of the plugin is, by its `mode`.
#[derive(Default)]
enum Role {
    /// No `mode`: the picker, floating over the tab.
    #[default]
    Picker,
    Sidebar(Tree),
    Dock(dock::Dock),
    /// A `mode` luneta does not know. It says so, and does nothing else.
    Unknown(String),
}

/// What a load with a `mode` luneta does not know says, in its pane if it has
/// one, and in zellij's log.
fn unknown_mode(mode: &str) -> String {
    format!("luneta has no mode {mode:?}: \"{SIDEBAR}\", \"{DOCK}\", or none for the picker")
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

const AGENTS_COMMAND: &str = "agents_command";

const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

const TICK: f64 = 0.1;

const TICKS_PER_SECOND: u64 = 10;

const TICKS_PER_POLL: u64 = TICKS_PER_SECOND;

const PREVIEW_DELAY: u64 = 2;

/// How often the sidebar asks each session for its panes again, so that an
/// agent started in a new pane, or in a shell that moved, finds its row.
const TICKS_PER_PLACES: u64 = 5 * TICKS_PER_SECOND;

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
    active_tab: Option<usize>,
    sessions: Sessions,
    error: Option<String>,
    frame: u64,
    peeks: Peeks,
    preview_at: Option<(Target, u64)>,
    agents_taken_at: u64,
    role: Role,
    plugin_id: u32,
    tabs: Vec<TabInfo>,
    branches: Vec<Branch>,
    here: Option<u32>,
    away: bool,
    places_at: u64,
    size: (usize, usize),
    pass: Pass,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        self.plugin_id = ids.plugin_id;
        request_permission(&PERMISSIONS);
        self.role = match configuration.get(MODE).map(|value| value.trim()) {
            None => Role::Picker,
            Some(SIDEBAR) => Role::Sidebar(Tree::default()),
            Some(DOCK) => {
                let mut sidebar = configuration.clone();
                sidebar.insert(MODE.to_owned(), SIDEBAR.to_owned());
                Role::Dock(dock::Dock::new(self.plugin_id, ids.client_id, sidebar))
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
                ]);
                return;
            },
            Role::Unknown(mode) => {
                eprintln!("{}", unknown_mode(mode));
                return;
            },
        }
        self.agents_command = configuration
            .get(AGENTS_COMMAND)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
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
            subscribe(&[EventType::Mouse]);
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
                    _ => {},
                }
                return false;
            },
            Role::Unknown(_) => return false,
        }
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    let title = if self.is_sidebar() { dock::SIDEBAR_TITLE } else { "luneta" };
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
                polled || self.spinning() || asked
            },
            Event::Key(key) if self.is_sidebar() => self.handle_sidebar_key(&key),
            Event::Key(key) => self.handle_key(&key),
            Event::Mouse(mouse) => self.handle_mouse(mouse),
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                match context.get(dirs::CONTEXT_KEY).map(String::as_str) {
                    Some(dirs::CONTEXT_VALUE) => {
                        self.dirs.ingest(exit_code, &stdout, &stderr);
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
                    _ => false,
                }
            },
            Event::PaneUpdate(manifest) => {
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
        match self.role {
            Role::Picker => {},
            Role::Sidebar(_) => {
                if let Some(ask) = ask {
                    self.sidebar_pipe(ask);
                }
                return false;
            },
            Role::Dock(ref mut dock) => {
                if let Some(ask) = ask {
                    dock.pipe(ask);
                }
                return false;
            },
            Role::Unknown(_) => return false,
        }
        if message.name != SCREEN_PIPE {
            return false;
        }
        let Some(screen) = message.payload.as_deref().and_then(Screen::from_name) else {
            return false;
        };
        self.screen = screen;
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
        if self.is_sidebar() {
            // zellij keeps a client's plugins running after the client leaves,
            // timers and all, and knows no focused pane for it. A sidebar its
            // client left asks nothing until the client comes back.
            self.away = dock::attached() == Some(false);
            if self.away {
                return;
            }
            self.seat_from(&snapshot);
            self.branches = snapshot
                .live_sessions
                .iter()
                .map(|s| {
                    let age = Age::new(s.creation_time);
                    Branch::new(s.name.clone(), age, &s.tabs, &s.panes, &self.places)
                })
                .collect();
            if self.frame.wrapping_sub(self.places_at) >= TICKS_PER_PLACES {
                self.places_at = self.frame;
                self.places.forget();
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
        let panes = self.panes.as_ref()?.panes.get(&tab)?;
        let focused = |floating: bool| {
            panes.iter().find(move |p| {
                p.is_focused && !p.is_plugin && !p.is_suppressed && p.is_floating == floating
            })
        };
        focused(false).or_else(|| focused(true)).map(|p| p.id)
    }

    fn rebuild_agents(&mut self, policy: Selection) {
        let origin = if self.is_sidebar() { None } else { self.origin_pane() };
        let since = self.agents_since();
        let live = Live::new(self.matches.current_session.as_deref(), &self.places);
        self.agents.rebuild(&self.matches.search_term, &live, origin, since, policy);
        self.rebuild_tree();
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

    fn ask_agents(&mut self) {
        if self.agents.asking {
            return;
        }
        self.agents.asking = true;
        let command: [&str; 1] = match self.agents_command.as_deref() {
            Some(command) => [command],
            None => agents::QUERY,
        };
        run_command(
            &command,
            BTreeMap::from([(
                agents::CONTEXT_KEY.to_owned(),
                agents::CONTEXT_VALUE.to_owned(),
            )]),
        );
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
        if self.dirs.asking {
            return;
        }
        self.dirs.asking = true;
        run_command(
            &dirs::QUERY,
            BTreeMap::from([(dirs::CONTEXT_KEY.to_owned(), dirs::CONTEXT_VALUE.to_owned())]),
        );
    }

    fn handle_key(&mut self, key: &KeyWithModifier) -> bool {
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
                self.screen = self.screen.next();
                true
            },
            BareKey::Tab if key.has_modifiers(&[KeyModifier::Shift]) => {
                self.screen = self.screen.prev();
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
            BareKey::Delete if key.has_no_modifiers() && self.screen == Screen::Sessions => {
                self.delete_selected();
                true
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

        if self.matches.is_own_name() || self.matches.name_error().is_some() {
            return;
        }

        let term = self.matches.search_term.clone();
        switch_session(if term.is_empty() { None } else { Some(term.as_str()) });
        close_self();
    }

    fn confirm_dir(&mut self) {
        let Some(row) = self.dirs.selected_row() else {
            return;
        };
        switch_session_with_cwd(Some(&row.name), Some(PathBuf::from(&row.path)));
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
        if name.is_empty() {
            Some("name must not be empty")
        } else if current == name {
            Some("already called that")
        } else if self.sessions.any_named(name) {
            Some("a session by that name already exists")
        } else {
            validate_name(name)
        }
    }

    fn apply_rename(&mut self) {
        let Mode::Rename { current, input } = &self.mode else {
            return;
        };
        if self.rename_error(current, input).is_some() {
            return;
        }
        rename_session(input);
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
        self.rebuild_agents(Selection::Hold);
        self.is_sidebar()
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
        match self.pass.see(at, self.tabs.len(), self.frame, stands) {
            Some(On::Back) => move_focus_or_tab(Direction::Left),
            Some(On::Beside) => move_focus(Direction::Right),
            None => {},
        }
    }

    fn wake(&mut self) {
        self.places.forget();
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
                branch.panes = tree::leaves(current, &self.tabs, manifest, &self.places);
            }
        }
        let follow = !self.sidebar_focused();
        if let Role::Sidebar(tree) = &mut self.role {
            tree.rebuild(&self.branches, current.as_deref(), &self.agents, self.here, follow);
        }
    }

    fn handle_sidebar_key(&mut self, key: &KeyWithModifier) -> bool {
        if !key.has_no_modifiers() {
            return false;
        }
        let search = match &self.role {
            Role::Sidebar(tree) => tree.search().map(str::to_owned),
            _ => None,
        };
        if let Some(term) = search {
            return self.handle_search_box_key(key.bare_key, term);
        }
        match key.bare_key {
            BareKey::Down | BareKey::Char('j') => self.move_tree(1),
            BareKey::Up | BareKey::Char('k') => self.move_tree(-1),
            BareKey::Left | BareKey::Char('h') => self.fold(Some(true)),
            BareKey::Right | BareKey::Char('l') => self.fold(Some(false)),
            BareKey::Char(' ') => self.fold(None),
            BareKey::Char('/') => self.search_tree(String::new()),
            BareKey::Enter => {
                self.go_to_selected();
                true
            },
            BareKey::Char('n') => {
                self.go_to_next();
                false
            },
            BareKey::Esc => {
                self.go_back();
                false
            },
            _ => false,
        }
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

    fn handle_mouse(&mut self, mouse: Mouse) -> bool {
        if !self.is_sidebar() {
            return false;
        }
        self.pass.point();
        match mouse {
            Mouse::LeftClick(line, _) => {
                let Ok(line) = usize::try_from(line) else {
                    return false;
                };
                self.click(line)
            },
            Mouse::ScrollUp(lines) => self.move_tree(-(lines as isize)),
            Mouse::ScrollDown(lines) => self.move_tree(lines as isize),
            _ => false,
        }
    }

    fn click(&mut self, line: usize) -> bool {
        let (rows, cols) = self.size;
        let focused = self.sidebar_focused();
        let Role::Sidebar(tree) = &mut self.role else {
            return false;
        };
        let Some(index) = render::sidebar_row_at(tree, &self.agents, focused, rows, cols, line)
        else {
            return false;
        };
        tree.rows.select(index);
        let narrowed = tree.narrowed();
        match tree.rows.selected_row() {
            // A term unfolds what it finds, so a fold would show nothing.
            Some(Node::Session(_)) if !narrowed => self.fold(None),
            Some(_) => {
                self.go_to_selected();
                true
            },
            None => false,
        }
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
    /// you came from; with nothing typed, its row folds as it always does.
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
            Place::Pane(session, pane) => self.go_to(&session, pane),
            Place::Session(session) if current == Some(session.as_str()) => {
                if found {
                    self.go_back();
                } else {
                    self.fold(None);
                }
            },
            Place::Session(session) => switch_session(Some(&session)),
        }
    }

    fn go_to(&self, session: &str, pane: u32) {
        if self.matches.current_session.as_deref() == Some(session) {
            focus_terminal_pane(pane, false, false);
        } else {
            switch_session_with_focus(session, None, Some((pane, false)));
        }
    }

    /// A hidden sidebar hears nothing of the panes, so the pane you are in
    /// comes from zellij.
    fn go_to_next(&self) {
        let pane = match get_focused_pane_info() {
            Ok((_, PaneId::Terminal(pane))) => Some(pane),
            _ => self.here,
        };
        let here = self.matches.current_session.as_deref().zip(pane);
        if let Some(row) = self.agents.next_wanted(here) {
            self.go_to(row.seat.session(), row.pane);
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
    /// sidebar waits for the focus, and keeps it.
    fn sidebar_pipe(&mut self, ask: Ask) {
        match ask {
            Ask::Focus if self.hidden() => self.pass.ask(self.frame),
            Ask::Focus if self.has_focus() => self.go_back(),
            Ask::Focus => {
                self.pass.ask(self.frame);
                focus_plugin_pane(self.plugin_id, false, false);
            },
            Ask::Next => self.go_to_next(),
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
    let id = tab.tab_id;
    if tab.are_floating_panes_visible {
        return Some(At::Pane { tab: id, beside: None });
    }
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
    use crate::tree::Leaf;

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
        let four = Leaf { id: 4, title: "pane 4".to_owned(), tab: String::new(), cwd: None };
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

    /// With nothing typed, the search box changes nothing a session row does:
    /// a click folds it, and so does `Enter` on your own. Found by a term,
    /// both take you there instead, which for your own is back to your pane.
    #[test]
    fn a_session_row_folds_unless_a_term_found_it() {
        for (typed, row, click, folded) in [
            ("", 0, false, &["a"][..]),
            ("", 0, true, &["a"]),
            ("", 2, true, &["b"]),
            ("a", 0, false, &[]),
            ("a", 0, true, &[]),
        ] {
            let mut state = focused_sidebar();
            state.search_tree(typed.to_owned());
            if click {
                state.click(row);
            } else {
                if let Role::Sidebar(tree) = &mut state.role {
                    tree.rows.select(row);
                }
                state.go_to_selected();
            }
            let Role::Sidebar(tree) = &state.role else { panic!("not a sidebar") };
            let shut: Vec<&str> = tree
                .rows
                .iter()
                .filter_map(|node| match node {
                    Node::Session(session) if session.folded => Some(session.name.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(shut, folded, "{typed:?}, row {row}, click: {click}");
            if !typed.is_empty() {
                assert_eq!(tree.search(), None, "{typed:?}, row {row}, click: {click}");
            }
        }
    }
}
