mod agents;
mod cursor;
mod dirs;
mod elapsed;
mod fetch;
mod layout;
mod paint;
mod panes;
mod places;
mod render;
mod sessions;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentSet, Live, Seat};
use dirs::{DirSet, LIST};
use elapsed::Age;
use panes::Peeks;
use places::Places;
use sessions::{validate_name, Contents, Focus, Kind, MatchSet, Selection, Session, Sessions};
use zellij_tile::prelude::*;

const SCREEN_PIPE: &str = "screen";

const AGENTS_COMMAND: &str = "agents_command";

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
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.agents_command = configuration
            .get(AGENTS_COMMAND)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
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
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    let plugin_id = get_plugin_ids().plugin_id;
                    rename_plugin_pane(plugin_id, "luneta");
                    resize_self(plugin_id);
                    self.ask_zoxide();
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
            Event::Key(key) => self.handle_key(key),
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
                self.rebuild_agents(Selection::Hold);
                false
            },
            Event::TabUpdate(tabs) => {
                self.active_tab = tabs.iter().find(|t| t.active).map(|t| t.position);
                self.rebuild_agents(Selection::Hold);
                false
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
        match &self.mode {
            Mode::Search => match self.screen {
                Screen::Sessions => {
                    render::render_search(
                        &self.matches,
                        &self.peeks,
                        self.error.as_deref(),
                        rows,
                        cols,
                    )
                },
                Screen::Dirs => {
                    render::render_dirs(&self.dirs, &self.matches.search_term, rows, cols)
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
        matches!(self.mode, Mode::Search)
            && self.screen == Screen::Agents
            && self.agents.any_busy()
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
        let origin = self.origin_pane();
        let since = self.agents_since();
        let live = Live::new(self.matches.current_session.as_deref(), &self.places);
        self.agents.rebuild(&self.matches.search_term, &live, origin, since, policy);
    }

    fn agents_since(&self) -> Age {
        Age::from_secs(self.frame.wrapping_sub(self.agents_taken_at) / TICKS_PER_SECOND)
    }

    fn ask_places(&mut self) {
        for session in self.places.ask(&self.live_names) {
            run_command(
                &places::query(&session),
                BTreeMap::from([
                    (dirs::CONTEXT_KEY.to_string(), places::CONTEXT_VALUE.to_string()),
                    (places::SESSION_KEY.to_string(), session.clone()),
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
                agents::CONTEXT_KEY.to_string(),
                agents::CONTEXT_VALUE.to_string(),
            )]),
        );
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
                        (dirs::CONTEXT_KEY.to_string(), dirs::PREVIEW_VALUE.to_string()),
                        (dirs::PATH_KEY.to_string(), path.clone()),
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
                        (dirs::CONTEXT_KEY.to_string(), panes::CONTEXT_VALUE.to_string()),
                        (panes::PANE_KEY.to_string(), panes::key(&session, pane)),
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
                Some(Target::Pane(row.seat.session().to_string(), row.pane))
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
            BTreeMap::from([(dirs::CONTEXT_KEY.to_string(), dirs::CONTEXT_VALUE.to_string())]),
        );
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        if key.bare_key == BareKey::Char('c') && key.has_modifiers(&[KeyModifier::Ctrl]) {
            close_self();
            return false;
        }
        match self.mode {
            Mode::Search => self.handle_search_key(key),
            Mode::Rename { .. } => self.handle_rename_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyWithModifier) -> bool {
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
                switch_session_with_focus(session, None, Some((row.pane, false)))
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

    fn handle_rename_key(&mut self, key: KeyWithModifier) -> bool {
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
        self.error = result.err().map(|e| format!("{} \"{}\": {}", verb, name, e));
        self.poll();
    }

    fn set_term(&mut self, term: String) {
        self.error = None;
        self.matches.set_search_term(term, &self.sessions);
        self.rebuild_dirs(Selection::SnapToTop);
        self.rebuild_agents(Selection::SnapToTop);
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

fn resize_self(plugin_id: u32) {
    let (x, y, width, height) = FLOATING;
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
    change_floating_panes_coordinates(vec![(PaneId::Plugin(plugin_id), coordinates)]);
}
