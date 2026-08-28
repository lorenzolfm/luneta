//! zj-picker — a personal zellij session picker.
//!
//! The contract, in one sentence:
//!
//! > **The highlighted row tells you what `Enter` does, and its tag says which. With nothing
//! > highlighted, `Enter` hands your typed text to the host, which attaches, resurrects, or
//! > creates.**
//!
//! 🔴 The second clause is safe because session names are unique across live **and**
//! resurrectable sessions. So the plugin never decides attach-vs-create: it hands the host one
//! name and the host resolves it (`src/commands.rs:752-786`) — live → attach, has a resurrection
//! layout → resurrect, neither → create. One call, three outcomes, chosen by the name alone.

mod layouts;
mod render;
mod sessions;

use std::collections::BTreeMap;
use std::time::Duration;

use layouts::LayoutList;
use sessions::MatchSet;
use zellij_tile::prelude::*;

/// Floating geometry, applied by the plugin to its own pane.
///
/// The default floating size shows ~3 rows, which is a thin window onto a dozen sessions.
/// `change_floating_panes_coordinates` lets the plugin fix that itself, so `config.kdl` keeps
/// `floating true` and needs no restart to change.
const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

/// Which screen has the keyboard. There is no third: everything else the upstream plugin can do
/// (rename, kill, kill-all, disconnect-others) was cut.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    /// Type, filter, move, `Enter`.
    #[default]
    Search,
    /// The confirm step before a session is created. Nothing has been created yet.
    Layout,
}

#[derive(Default)]
struct State {
    mode: Mode,
    matches: MatchSet,
    layouts: LayoutList,
    /// The last snapshot, kept so a keystroke can re-filter without waiting for the next poll.
    live: Vec<(String, Duration)>,
    dead: Vec<(String, Duration)>,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
        ]);
        // No SessionUpdate. That path refreshes only the current session's age and leaves its
        // peers frozen; the 1s poll below takes the whole list from one snapshot instead, which
        // is what makes the ages mutually consistent rather than incidentally so.
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Timer,
            EventType::Key,
        ]);
        // NB: no privileged command here. Grants arrive asynchronously as
        // PermissionRequestResult, so anything needing one is denied if issued from load().
        // The builtin gets away with it only because builtins bypass permission checks
        // entirely (zellij_exports.rs:5428).
        set_timeout(0.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    // Safe now, and only now: both of these need ChangeApplicationState.
                    let plugin_id = get_plugin_ids().plugin_id;
                    rename_plugin_pane(plugin_id, "zj-picker");
                    resize_self(plugin_id);
                }
                true
            },
            Event::Timer(_) => {
                self.poll();
                set_timeout(1.0);
                true
            },
            Event::Key(key) => self.handle_key(key),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, cols: usize) {
        match self.mode {
            Mode::Search => render::render_search(&self.matches, rows, cols),
            Mode::Layout => render::render_layouts(&self.matches, &self.layouts, rows, cols),
        }
    }
}

impl State {
    /// One `get_session_list()` snapshot per tick.
    ///
    /// Not the pushed `SessionUpdate` path: that one refreshes only the current session's age
    /// and leaves its peers frozen, which is why upstream's ages agree only by accident. Taking
    /// the whole list from a single call makes the consistency structural.
    fn poll(&mut self) {
        let Ok(snapshot) = get_session_list() else {
            return;
        };
        let current_session = snapshot.live_sessions.iter().find(|s| s.is_current_session);
        // The layout list rides along on the current session's SessionInfo, so the confirm
        // screen costs no extra host call and no extra subscription.
        if let Some(session) = current_session {
            self.layouts.update(session.available_layouts.clone());
        }
        let current = current_session.map(|s| s.name.clone());
        self.live = snapshot
            .live_sessions
            .into_iter()
            // The current session leaves the match set here, at the source, rather than in the
            // renderer — that is what keeps the rendered list equal to the match set.
            .filter(|s| !s.is_current_session)
            .map(|s| (s.name, s.creation_time))
            .collect();
        self.dead = snapshot.resurrectable_sessions;
        self.matches.refresh(&self.live, &self.dead, current);
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        // Ctrl-c always dismisses, from either screen. It is the one unconditional exit, which
        // matters now that Esc has been given a job in the Search screen.
        if key.bare_key == BareKey::Char('c') && key.has_modifiers(&[KeyModifier::Ctrl]) {
            close_self();
            return false;
        }
        match self.mode {
            Mode::Search => self.handle_search_key(key),
            Mode::Layout => self.handle_layout_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Enter if key.has_no_modifiers() => {
                self.confirm_search();
                true
            },
            // ⚠️ Esc no longer dismisses in one press when something is highlighted — it drops
            // the highlight, and only a second Esc closes. Always-on selection otherwise makes
            // the no-selection state unreachable, and that state is the only way to ask for a
            // name that fuzzy-matches something existing. The hint line says which state you are
            // in, so the intermediate step is not silent.
            BareKey::Esc if key.has_no_modifiers() => {
                if self.matches.selected.is_some() {
                    self.matches.drop_selection();
                    true
                } else {
                    close_self();
                    false
                }
            },
            BareKey::Down if key.has_no_modifiers() => {
                self.matches.move_selection(1);
                true
            },
            BareKey::Up if key.has_no_modifiers() => {
                self.matches.move_selection(-1);
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

    /// `Enter` in the Search screen — the whole contract in one function.
    fn confirm_search(&mut self) {
        // A highlight: act on it. The host turns the name into an attach or a resurrect by
        // itself, which is why `[ATTACH]`/`[RESURRECT]` can label the outcome without the plugin
        // branching on it.
        if let Some(name) = self.matches.selected_name() {
            switch_session(Some(name));
            close_self();
            return;
        }

        // No highlight: `Enter` takes the literal term. Two states refuse it, and in both the
        // hint line has already said why — so this is a no-op, not an error overlay. Upstream's
        // show_error() would be wrong here: handle_key clears self.error on *any* key and
        // swallows that keystroke, and this is a state you wander into by typing.
        if self.matches.is_own_name() || self.matches.name_error().is_some() {
            return;
        }

        // Everything else is an offer to create — and only an offer. Nothing exists yet.
        self.layouts.reset();
        self.mode = Mode::Layout;
    }

    fn handle_layout_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Enter if key.has_no_modifiers() => {
                self.create_session();
                true
            },
            // Backing out costs one keystroke and keeps the typed term, which is what makes the
            // confirm step cheap enough to be the answer to "did Enter just create something?".
            BareKey::Esc if key.has_no_modifiers() => {
                self.mode = Mode::Search;
                true
            },
            BareKey::Down if key.has_no_modifiers() => {
                self.layouts.move_selection(1);
                true
            },
            BareKey::Up if key.has_no_modifiers() => {
                self.layouts.move_selection(-1);
                true
            },
            _ => false,
        }
    }

    fn create_session(&mut self) {
        let term = self.matches.search_term.clone();
        // An empty name is a feature, not a hole: the host generates one. So Esc-Enter-Enter on
        // an empty prompt is a fresh scratch session for free.
        let name = if term.is_empty() { None } else { Some(term.as_str()) };
        match self.layouts.selected_layout() {
            Some(layout) => switch_session_with_layout(name, layout.clone(), None),
            None => switch_session(name),
        }
        close_self();
    }

    fn set_term(&mut self, term: String) {
        // Re-filters against the cached snapshot rather than calling the host: a keystroke must
        // not have to wait a poll interval to change the list.
        self.matches.set_search_term(term, &self.live, &self.dead);
    }
}

fn resize_self(plugin_id: u32) {
    let (x, y, width, height) = FLOATING;
    let Some(coordinates) = FloatingPaneCoordinates::new(
        Some(x.to_string()),
        Some(y.to_string()),
        Some(width.to_string()),
        Some(height.to_string()),
        None,
        None,
    ) else {
        return;
    };
    change_floating_panes_coordinates(vec![(PaneId::Plugin(plugin_id), coordinates)]);
}
