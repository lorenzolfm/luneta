mod agents;
mod close;
mod context;
mod cursor;
mod daemon;
mod dirs;
mod elapsed;
mod fetch;
mod layout;
mod menu;
mod paint;
mod panes;
mod places;
mod render;
mod sessions;
mod skills;
mod tree;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentSet, Live, Seat};
use context::CONTEXT_KEY;
use daemon::{Answer, Daemon};
use dirs::{DirSet, LIST};
use elapsed::{Age, Moment};
use panes::Peeks;
use places::Places;
use sessions::{Contents, Focus, Kind, MatchSet, Selection, Session, SessionName, Sessions};
use tree::{origin, Branch, Leaf};
use zellij_tile::prelude::*;

const SCREEN_PIPE: &str = "screen";

/// What a load of the plugin is: `mode "menu"`, or none for the picker.
const MODE: &str = "mode";

/// `mode "menu"` makes the plugin the menu: keys, pressed one after another,
/// that start an agent.
const MENU: &str = "menu";

/// What a load of the plugin is, by its `mode`, with what it keeps. The
/// picker and the menu are boxed: each keeps far more than the name of a mode
/// luneta does not know.
enum Role {
    /// No `mode`: the picker, floating over the tab.
    Picker(Box<Picker>),
    /// The menu, which keeps what the daemon says, for a delete key to ask
    /// first.
    Menu(Box<menu::Menu>),
    /// A `mode` luneta does not know. It says so, and does nothing else.
    Unknown(String),
}

/// What zellij makes before it loads the plugin, and so before the plugin
/// has its `mode`: the picker, as a load with none is.
impl Default for Role {
    fn default() -> Self {
        Role::Picker(Box::default())
    }
}

/// What a load with a `mode` luneta does not know says, in its pane if it has
/// one, and in zellij's log.
fn unknown_mode(mode: &str) -> String {
    format!("luneta has no mode {mode:?}: \"{MENU}\", or none for the picker")
}

/// Every mode asks for the same. zellij keeps one grant for a plugin's file,
/// and each load puts what it asks for in the grant's place, whether zellij
/// prompts or not: a mode that asked for less would take the rest from the
/// others. `OpenTerminalsOrPlugins` is the menu's, to open your shell beside
/// a pane.
const PERMISSIONS: [PermissionType; 4] = [
    PermissionType::ReadApplicationState,
    PermissionType::ChangeApplicationState,
    PermissionType::RunCommands,
    PermissionType::OpenTerminalsOrPlugins,
];

/// The title the picker gives its pane, where zellij would show the plugin's
/// location.
const TITLE: &str = "luneta";

const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

const TICK: f64 = 0.1;

const TICKS_PER_SECOND: u64 = 10;

const TICKS_PER_POLL: u64 = TICKS_PER_SECOND;

const PREVIEW_DELAY: u64 = 2;

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

/// The picker: the sessions, the directories and the agents it lists, and
/// what it keeps of zellij and of the daemon to list them.
#[derive(Default)]
struct Picker {
    mode: Mode,
    screen: Screen,
    matches: MatchSet,
    dirs: DirSet,
    agents: AgentSet,
    /// The panes as zellij last reported them. In the tab this client is on,
    /// the layer that shows marks only this client's focus, which zellij says;
    /// the other layer keeps every client's mark, and so does the tab when
    /// zellij cannot say.
    panes: Option<PaneManifest>,
    places: Places,
    /// What the daemon says of the host, which the panes' places and the
    /// agents come from.
    daemon: Daemon,
    active_tab: Option<usize>,
    sessions: Sessions,
    error: Option<String>,
    frame: u64,
    peeks: Peeks,
    preview_at: Option<(Target, u64)>,
    plugin_id: u32,
    branches: Vec<Branch>,
}

register_plugin!(Role);

impl ZellijPlugin for Role {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        let ids = get_plugin_ids();
        request_permission(&PERMISSIONS);
        *self = match configuration.get(MODE).map(|value| value.trim()) {
            None => Role::Picker(Box::new(Picker::new(ids.plugin_id))),
            Some(MENU) => {
                Role::Menu(Box::new(menu::Menu::new(ids.plugin_id, ids.client_id, &configuration)))
            },
            Some(mode) => Role::Unknown(mode.to_owned()),
        };
        match &*self {
            Role::Picker(_) => {
                subscribe(&[
                    EventType::PermissionRequestResult,
                    EventType::Timer,
                    EventType::Key,
                    EventType::RunCommandResult,
                    EventType::Visible,
                    EventType::PaneUpdate,
                    EventType::TabUpdate,
                ]);
                set_timeout(0.0);
            },
            Role::Menu(_) => {
                subscribe(&[
                    EventType::Key,
                    EventType::TabUpdate,
                    EventType::PaneUpdate,
                    EventType::RunCommandResult,
                    EventType::Timer,
                ]);
            },
            Role::Unknown(mode) => eprintln!("{}", unknown_mode(mode)),
        }
    }

    fn update(&mut self, event: Event) -> bool {
        match self {
            Role::Picker(picker) => picker.update(event),
            Role::Menu(menu) => match event {
                Event::Key(key) => menu.key(&key),
                Event::TabUpdate(tabs) => menu.tabs(tabs),
                Event::PaneUpdate(panes) => menu.panes(panes),
                Event::Timer(_) => menu.tick(),
                Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                    menu.ran(exit_code, &stdout, &stderr, &context)
                },
                _ => false,
            },
            Role::Unknown(_) => false,
        }
    }

    fn pipe(&mut self, message: PipeMessage) -> bool {
        match self {
            Role::Picker(picker) => picker.pipe(&message),
            Role::Menu(_) | Role::Unknown(_) => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        match &*self {
            Role::Picker(picker) => picker.render(rows, cols),
            Role::Menu(menu) => render::render_menu(menu, rows, cols),
            Role::Unknown(mode) => render::render_notice(&unknown_mode(mode), rows, cols),
        }
    }
}

impl Picker {
    fn new(plugin_id: u32) -> Self {
        Picker { plugin_id, ..Picker::default() }
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    if let Some(pane) = get_pane_info(PaneId::Plugin(self.plugin_id)) {
                        retitle(self.plugin_id, &pane);
                    }
                    resize_self(self.plugin_id);
                    self.ask_zoxide();
                    self.ask_daemon();
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
            Event::Key(key) => self.handle_key(&key),
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
                    Some(daemon::CONTEXT_VALUE) => self.heard(exit_code, &stdout, &stderr),
                    Some(agents::CLOSE_VALUE) if exit_code != Some(0) => {
                        let label = context.get(agents::LABEL_KEY).map_or("", String::as_str);
                        let reason = String::from_utf8_lossy(&stderr);
                        let reason = reason.lines().next().unwrap_or("").trim();
                        let reason =
                            if reason.is_empty() { "the pane did not close" } else { reason };
                        self.error = Some(format!("kill \"{label}\": {reason}"));
                        // Brings back the agent the pane still holds.
                        self.agents.stop_closing();
                        self.rebuild_agents(Selection::Hold);
                        true
                    },
                    _ => false,
                }
            },
            Event::PaneUpdate(manifest) => {
                self.panes = Some(manifest);
                self.keep_your_focus();
                if let Some(pane) = self.own_pane() {
                    retitle(self.plugin_id, pane);
                }
                self.rebuild_agents(Selection::Hold);
                false
            },
            Event::TabUpdate(tabs) => {
                self.active_tab = tabs.iter().find(|t| t.active).map(|t| t.position);
                self.keep_your_focus();
                self.rebuild_agents(Selection::Hold);
                false
            },
            Event::Visible(true) => {
                self.ask_zoxide();
                self.dirs.forget_listings();
                self.peeks.forget();
                self.daemon.wake();
                self.ask_daemon();
                false
            },
            _ => false,
        }
    }

    fn pipe(&mut self, message: &PipeMessage) -> bool {
        if message.name != SCREEN_PIPE {
            return false;
        }
        let Some(screen) = message.payload.as_deref().and_then(Screen::from_name) else {
            return false;
        };
        self.show(screen);
        true
    }

    fn render(&self, rows: usize, cols: usize) {
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

    fn poll(&mut self) {
        let Ok(snapshot) = get_session_list() else {
            return;
        };
        self.ask_daemon();
        // For each agent to go by the name its pane gives it.
        let before = std::mem::take(&mut self.branches);
        self.branches = snapshot
            .live_sessions
            .iter()
            .map(|s| {
                let before = before.iter().find(|branch| branch.name == s.name);
                let before = before.map_or(&[][..], |branch| branch.panes.as_slice());
                Branch::new(s.name.clone(), &s.tabs, &s.panes, &self.places, before)
            })
            .collect();
        let current = snapshot
            .live_sessions
            .iter()
            .find(|s| s.is_current_session)
            .map(|s| s.name.clone());
        let mut contents = BTreeMap::new();
        let mut live = Vec::new();
        for session in snapshot.live_sessions {
            if session.is_current_session {
                continue;
            }
            let name = session.name.clone();
            let age = Age::new(session.creation_time);
            contents.insert(name.clone(), contents_of(session));
            live.push(Session { name, age });
        }
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
        self.rebuild_dirs(Selection::Hold);
        self.rebuild_agents(Selection::Hold);
    }

    fn spinning(&self) -> bool {
        let shown = matches!(self.mode, Mode::Search) && self.screen == Screen::Agents;
        shown && self.agents.any_busy()
    }

    fn origin_pane(&self) -> Option<u32> {
        let tab = self.active_tab?;
        origin(self.panes.as_ref()?.panes.get(&tab)?)
    }

    fn rebuild_agents(&mut self, policy: Selection) {
        let origin = self.origin_pane();
        let current = self.matches.current_session.as_deref();
        let live = Live::new(current, &self.sessions.live, &self.places, &self.branches);
        self.agents.rebuild(&self.matches.search_term, &live, origin, Moment::now(), policy);
    }

    /// Asks the daemon how the host stands, if it is to be asked now: from
    /// the version last seen on, for the next.
    fn ask_daemon(&mut self) {
        let Some(command) = self.daemon.ask(Moment::now()) else {
            return;
        };
        let command: Vec<&str> = command.iter().map(String::as_str).collect();
        run_command(
            &command,
            BTreeMap::from([(CONTEXT_KEY.to_owned(), daemon::CONTEXT_VALUE.to_owned())]),
        );
    }

    /// Takes up what the daemon answered, and asks it again for what changes
    /// next: what it says of the panes and the agents, in the place of what it
    /// said before. When it does not answer, the agents are not known, and the
    /// picker says why: what it said of the panes stands until it answers
    /// again.
    fn heard(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> bool {
        match self.daemon.answered(exit_code, stdout, stderr, Moment::now()) {
            Answer::Changed(host) => {
                self.places.heard(&host.sessions);
                self.agents.heard(&host.agents, Moment::now());
            },
            Answer::Same => {
                self.ask_daemon();
                return false;
            },
            Answer::Silent(why) => {
                // Asked again at a poll, once it has had a while.
                self.agents.fail(why);
                self.rebuild_agents(Selection::Hold);
                return true;
            },
        }
        // Every session's panes, with what was said of where they work, and
        // the daemon asked again.
        self.poll();
        true
    }

    fn follow_preview(&mut self) -> bool {
        if !matches!(self.mode, Mode::Search) {
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
        tree::leaves(session, &listed.tabs, &listed.panes, &self.places, &[])
    }

    fn set_term(&mut self, term: String) {
        self.error = None;
        self.matches.set_search_term(term, &self.sessions);
        self.rebuild_dirs(Selection::SnapToTop);
        self.rebuild_agents(Selection::SnapToTop);
    }

    fn own_pane(&self) -> Option<&PaneInfo> {
        let manifest = self.panes.as_ref()?;
        manifest.panes.values().flatten().find(|p| p.is_plugin && p.id == self.plugin_id)
    }

    /// zellij marks focused, in each tab, the pane each client was on there
    /// last, and keeps the mark of a client that has left the session. Where
    /// the tab you are on marks more than one, zellij says which is yours.
    fn keep_your_focus(&mut self) {
        let Some(tab) = self.active_tab else {
            return;
        };
        let Some(panes) = self.panes.as_mut().and_then(|m| m.panes.get_mut(&tab)) else {
            return;
        };
        if !shared(panes) {
            return;
        }
        if let Ok((_, yours)) = get_focused_pane_info() {
            keep_focus(panes, yours);
        }
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

/// Whether a tab marks more than one of its tiled panes focused, or more than
/// one of its floating panes: one for each client that was on it, there or
/// still.
fn shared(panes: &[PaneInfo]) -> bool {
    let marked = |floating: bool| {
        let marks = panes.iter().filter(|p| p.is_focused && !p.is_suppressed);
        marks.filter(|p| p.is_floating == floating).count()
    };
    marked(false) > 1 || marked(true) > 1
}

/// Takes the focus off the panes of a tab that share a layer with `yours`, the
/// pane zellij says this client is on, other than it: it was other clients'.
/// A tab without it keeps every mark, and so does the other layer: zellij
/// says only the focus of the layer that shows.
fn keep_focus(panes: &mut [PaneInfo], yours: PaneId) {
    let Some(floating) = panes.iter().find(|p| pane_id(p) == yours).map(|p| p.is_floating) else {
        return;
    };
    for pane in panes.iter_mut().filter(|p| p.is_floating == floating && pane_id(p) != yours) {
        pane.is_focused = false;
    }
}

fn pane_id(pane: &PaneInfo) -> PaneId {
    if pane.is_plugin {
        PaneId::Plugin(pane.id)
    } else {
        PaneId::Terminal(pane.id)
    }
}

/// Gives the picker's pane its title, when not the one it has.
fn retitle(plugin_id: u32, pane: &PaneInfo) {
    if pane.title != TITLE {
        rename_plugin_pane(plugin_id, TITLE);
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
    use super::*;
    use crate::agents::given;
    use crate::fetch::Fetch;

    /// zellij marks focused, in each tab, the pane each client was on there
    /// last, a client that left too, and says which is yours. Another's mark
    /// on a terminal is not the pane you are in.
    #[test]
    fn a_focus_another_client_left_in_the_tab_is_not_yours() {
        let pane = |id: u32| PaneInfo {
            id,
            is_focused: true,
            is_selectable: true,
            ..PaneInfo::default()
        };
        let mut tab = vec![pane(1), pane(2)];
        assert!(shared(&tab));
        keep_focus(&mut tab, PaneId::Terminal(2));
        assert!(!shared(&tab));
        assert_eq!(origin(&tab), Some(2));
    }

    /// A daemon that does not answer takes the agents, and the picker says
    /// why. What it said of the panes stands until it answers again.
    #[test]
    fn a_daemon_that_does_not_answer_takes_the_agents_and_says_why() {
        let mut picker =
            Picker { places: Places::of(&[("b", &[(4, "/w/b")])]), ..Picker::default() };
        picker.agents.heard(&[given::agent(4, "b", 4, "/w/b", "idle")], given::NOW);
        picker.rebuild_agents(Selection::Hold);
        assert_eq!(picker.agents.rows.len(), 1);
        picker.daemon.ask(Moment::now());
        let refused = b"luneta: lunetad.sock: transport error: Connection refused (os error 111)";
        let context = BTreeMap::from([(CONTEXT_KEY.to_owned(), daemon::CONTEXT_VALUE.to_owned())]);
        let refusal = Event::RunCommandResult(Some(1), Vec::new(), refused.to_vec(), context);
        assert!(picker.update(refusal));

        let said = "lunetad does not answer: luneta: lunetad.sock: transport error: Connection \
                    refused (os error 111)";
        assert!(matches!(&picker.agents.status, Fetch::Failed(why) if why == said));
        assert!(picker.agents.rows.is_empty());
        assert_eq!(picker.places.cwd("b", 4), Some("/w/b"));
    }
}
