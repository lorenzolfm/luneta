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

mod agents;
mod dirs;
mod render;
mod sessions;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use agents::{AgentSet, Jump};
use dirs::{Action, DirSet};
use sessions::{validate_name, Kind, MatchSet, Selection};
use zellij_tile::prelude::*;

/// The pipe message this plugin answers to, so that a key can open it *on* a chosen screen.
///
/// 🔴 A pipe rather than plugin configuration, and the difference is not stylistic. Zellij keys
/// a plugin instance partly on its configuration, so `LaunchOrFocusPlugin` with
/// `screen "agents"` is a *different* plugin from the one bound without it — pressing both keys
/// leaves you with two picker panes floating over each other. Verified by construction: two
/// launches differing only in configuration produced two panes. A pipe carries the request to
/// the one instance that already exists instead of minting another.
const SCREEN_PIPE: &str = "screen";

/// The one plugin configuration key this picker reads: an override for
/// [`agents::QUERY`], for a server whose `PATH` genuinely lacks `claude-ps`.
///
/// The value is the executable — a name or an absolute path — and nothing else. Arguments are
/// not parsed out of it, because a path may contain a space and an argument list would make
/// that ambiguous; wrap the tool in a script if you need one.
///
/// ⚠️ **Every binding must pass the same value, or none.** Zellij keys a plugin instance partly
/// on its configuration, so two keys disagreeing about this mint two pickers and leave two
/// floating panes stacked — the same trap that put the screen on a pipe rather than here
/// (see [`SCREEN_PIPE`]). Leaving it unset everywhere is the ordinary case and costs nothing:
/// a bare lookup is what the plugin does without it.
const AGENTS_COMMAND: &str = "agents_command";

/// Floating geometry, applied by the plugin to its own pane.
///
/// The default floating size shows ~3 rows, which is a thin window onto a dozen sessions.
/// `change_floating_panes_coordinates` lets the plugin fix that itself, so `config.kdl` keeps
/// `floating true` and needs no restart to change.
const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

/// The animation tick, in seconds — ten a second, which is what the busy spinner needs to read
/// as motion rather than as a glyph that keeps changing its mind.
///
/// ⚠️ This is **not** the poll interval, and the two were the same number until the spinner
/// arrived. The host call behind [`State::poll`] still runs once a second; see
/// [`TICKS_PER_POLL`]. Speeding the timer up without that divisor would have quietly taken the
/// session list from one `get_session_list` a second to ten.
const TICK: f64 = 0.1;

/// Animation ticks per session poll, so that the poll stays at its original once a second.
const TICKS_PER_POLL: u64 = 10;

/// Which screen has the keyboard. Kill-all and disconnect-others are still cut: both act on
/// sessions you cannot see from here, which is the one thing this picker refuses to do.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    /// Type, filter, move, `Enter`.
    #[default]
    Search,
    /// Typing a new name for the session you are in.
    Rename,
    /// The confirm step before a session is killed or deleted. Nothing has happened yet.
    Confirm,
}

/// Which list the Search screen is showing, toggled with `Tab`.
///
/// Two lists rather than one. Sessions are ranked by what you last did and directories by what
/// you do most, and merging them would force one of those orders onto rows that do not share a
/// meaning — with a hundred-odd directories outnumbering half a dozen sessions besides. The
/// search term crosses between them, so `Tab` asks the same question of the other list.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum Screen {
    #[default]
    Sessions,
    Dirs,
    /// The Claude Code agents that are running, and which one is waiting on you.
    Agents,
}

impl Screen {
    /// `Tab` cycles forward, `Shift-Tab` back. Three stops rather than two is what earns
    /// `Shift-Tab` its place: with two screens the reverse key was the same key, and with three
    /// the screen you want is otherwise two presses away half the time.
    ///
    /// The order is sessions, agents, directories, and it sorts by how attached the answer is
    /// to something already running. Sessions and agents are both *live* things — the agent
    /// screen is very nearly the session screen with a different reason for caring — so they sit
    /// next to each other, one `Tab` apart. Directories are where you go when the answer is not
    /// running yet, which makes them the far stop, and `Shift-Tab` reaches them in one press
    /// from the sessions rather than two.
    fn next(self) -> Self {
        match self {
            Screen::Sessions => Screen::Agents,
            Screen::Agents => Screen::Dirs,
            Screen::Dirs => Screen::Sessions,
        }
    }

    /// The name a keybinding uses to ask for this screen. See [`State::pipe`].
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

/// The session a `Del` is aimed at, captured when the key was pressed.
///
/// Captured, not looked up again on confirm: the 1s poll can reorder `rows` underneath the
/// confirm screen, and re-reading the selection there would apply the answer to whichever
/// session had drifted under the cursor.
pub struct Pending {
    pub name: String,
    pub kind: Kind,
}

impl Pending {
    /// Two different things wear the same key. Killing stops a running session; deleting throws
    /// away the saved layout of one that already stopped. Only the second is irreversible, and
    /// the confirm screen names which is which rather than calling both "delete".
    pub fn verb(&self) -> &'static str {
        match self.kind {
            Kind::Live => "Kill",
            Kind::Resurrectable => "Delete",
        }
    }

    /// Deliberately not "it stays resurrectable": whether a killed session comes back depends on
    /// whether the host had serialized it yet (`serialization_interval`, 10s by default but off
    /// entirely when `session_serialization false`), which the plugin cannot see. Promising a
    /// resurrection the host may not be able to deliver is worse than saying less.
    pub fn consequence(&self) -> &'static str {
        match self.kind {
            Kind::Live => "kills the running session — it comes back only if it was saved",
            Kind::Resurrectable => "throws its saved layout away — this cannot be undone",
        }
    }
}

/// The rename screen's input, with the reason it is currently refused.
///
/// The reason is recomputed on every keystroke rather than on `Enter`, so the screen can
/// refuse a name while you are still typing it instead of after you have committed.
#[derive(Default)]
pub struct Rename {
    pub input: String,
    pub error: Option<String>,
}

#[derive(Default)]
struct State {
    mode: Mode,
    screen: Screen,
    matches: MatchSet,
    /// The directory list, and why it is empty when it is. Populated out of band by zoxide —
    /// nothing else in here depends on it having arrived.
    dirs: DirSet,
    /// The agent list. Populated out of band by `claude-ps`, on the same terms as `dirs`.
    agents: AgentSet,
    /// [`AGENTS_COMMAND`], if a binding passed one. `None` — the ordinary case — means the
    /// bare [`agents::QUERY`] lookup.
    agents_command: Option<String>,
    /// The last pane manifest, and which tab is focused.
    ///
    /// Kept for exactly one question: **which pane was focused when the picker opened.** The
    /// picker is a floating pane, so asking the host for "the focused pane" returns the picker
    /// itself — `get_focused_pane_info` resolves through `Screen::get_active_pane_id`, which
    /// does not care what layer the answer is on. Tiled and floating panes keep *separate*
    /// `active_panes` maps, though, so the terminal underneath goes on reporting `is_focused`
    /// from its own layer while the picker holds focus in the other one. That is the pane we
    /// came from, and it is only reachable through the manifest.
    panes: Option<PaneManifest>,
    active_tab: Option<usize>,
    /// The last snapshot, kept so a keystroke can re-filter without waiting for the next poll.
    live: Vec<(String, Duration)>,
    dead: Vec<(String, Duration)>,
    rename: Rename,
    pending: Option<Pending>,
    /// A host call that came back `Err`. Shown on the search screen until the next thing the
    /// user does that could produce a new one — never as an overlay, so it cannot eat a
    /// keystroke the way upstream's `show_error()` does.
    error: Option<String>,
    /// Animation ticks since load, counted for the busy spinner and divided down for the poll.
    ///
    /// Monotonic rather than reset per screen, so the spinner is one clock the whole list reads
    /// off: every busy row turns in step, which reads as one thing happening rather than as
    /// several rows each doing their own.
    frame: u64,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        // Taken before the permission request, because the first `ask_agents` fires from the
        // grant and must already know which command it is asking for.
        self.agents_command = configuration
            .get(AGENTS_COMMAND)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        // ⚠️ RunCommands is what the directory screen costs. It is part of the same grant as
        // the other two, so adding it re-prompts once against this plugin's cached path — and a
        // denial takes the session list down with it, since the host denies the set rather than
        // the item. There is no way to ask for it separately.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        // No SessionUpdate. That path refreshes only the current session's age and leaves its
        // peers frozen; the 1s poll below takes the whole list from one snapshot instead, which
        // is what makes the ages mutually consistent rather than incidentally so.
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Timer,
            EventType::Key,
            // zoxide and claude-ps both answer here, and `Visible` is when it is worth
            // asking either of them again.
            EventType::RunCommandResult,
            EventType::Visible,
            // Not for drawing anything: these two are the only route to the pane the picker
            // was opened over. See `State::panes`.
            EventType::PaneUpdate,
            EventType::TabUpdate,
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
                    // Same reason, different permission: this is the first moment the host will
                    // accept a command from us.
                    self.ask_zoxide();
                    self.ask_agents();
                } else {
                    // The picker is dead in the water either way — the session list needs
                    // ReadApplicationState — but the directory screen is the one that can say so.
                    self.dirs.fail("permission denied");
                    self.agents.fail("permission denied");
                }
                true
            },
            Event::Timer(_) => {
                set_timeout(TICK);
                // Counted *before* the increment, so the poll rides on frame 0 — which is the
                // very first tick after `load`'s `set_timeout(0.0)`. That is what keeps the
                // session list populated by the first timer, as it was before the divisor;
                // dividing the other way round would have left the first second blank.
                let polled = self.frame.is_multiple_of(TICKS_PER_POLL);
                if polled {
                    self.poll();
                }
                self.frame = self.frame.wrapping_add(1);
                // Nine ticks in ten now change nothing. Redrawing on them anyway would rebuild
                // and repaint the whole table ten times a second to put back the pixels that
                // were already there, so a tick that is neither a poll nor a spinner frame is
                // spent by saying so.
                polled || self.spinning()
            },
            Event::Key(key) => self.handle_key(key),
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                // Ours, or someone else's — and now *which* of ours. The two screens share the
                // context key and differ in its value; anything else on this channel belongs to
                // another plugin and is not ours to parse.
                match context.get(dirs::CONTEXT_KEY).map(String::as_str) {
                    Some(dirs::CONTEXT_VALUE) => {
                        self.dirs.ingest(exit_code, &stdout, &stderr);
                        self.rebuild_dirs(Selection::Hold);
                        true
                    },
                    Some(agents::CONTEXT_VALUE) => {
                        self.agents.ingest(exit_code, &stdout, &stderr);
                        self.rebuild_agents(Selection::Hold);
                        true
                    },
                    _ => false,
                }
            },
            // Neither of these redraws anything by itself — they keep the answer to "which pane
            // did we come from" current, which decides one row's presence rather than any row's
            // appearance.
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
            // The plugin is launched with `launch-or-focus`, so one instance outlives many
            // openings — and every `cd` in between has changed zoxide's answer. Nothing to
            // redraw yet; the reply arrives as its own event.
            Event::Visible(true) => {
                self.ask_zoxide();
                // 🔴 A glance, not a watch: the agent snapshot is taken here and then frozen for
                // as long as the screen is up. That is what makes attention-first ordering safe
                // — nothing reorders while you are reading it.
                self.ask_agents();
                false
            },
            _ => false,
        }
    }

    /// Open on a named screen: `MessagePlugin` with `name "screen"` and a payload of
    /// `sessions`, `agents` or `dirs`.
    ///
    /// The name is checked so that an unrelated pipe — this plugin is reachable by any of them
    /// — cannot move the screen out from under whoever is typing. An unknown payload is
    /// likewise ignored rather than guessed at, which keeps a typo in a keybinding a no-op
    /// instead of a surprise.
    ///
    /// Nothing is refreshed here. If the picker was closed, the host makes it visible and the
    /// `Visible` handler takes the snapshot; if it was already open, it already has one, and
    /// this screen is a glance rather than a watch.
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
        match self.mode {
            Mode::Search => match self.screen {
                Screen::Sessions => {
                    render::render_search(&self.matches, self.error.as_deref(), rows, cols)
                },
                Screen::Dirs => {
                    render::render_dirs(&self.dirs, &self.matches.search_term, rows, cols)
                },
                Screen::Agents => render::render_agents(
                    &self.agents,
                    &self.matches.search_term,
                    rows,
                    cols,
                    self.frame,
                ),
            },
            Mode::Rename => render::render_rename(
                &self.rename,
                self.matches.current_session.as_deref(),
                rows,
                cols,
            ),
            Mode::Confirm => match &self.pending {
                Some(pending) => render::render_confirm(pending, rows, cols),
                // Unreachable in practice: `Confirm` is only entered with a target. Falling
                // back to the search screen beats rendering a blank pane if it ever is.
                None => render::render_search(&self.matches, self.error.as_deref(), rows, cols),
            },
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
        // A directory row's tag is a function of the session list, so it goes stale on exactly
        // the same tick the session list does.
        self.rebuild_dirs(Selection::Hold);
        // The agent list is *not* re-fetched here — it is a frozen snapshot. What is recomputed
        // is which call `Enter` would make and which row is us, both of which are functions of
        // the session poll rather than of the agents.
        self.rebuild_agents(Selection::Hold);
    }

    /// Is a spinner on screen right now, and therefore is this tick worth a redraw?
    ///
    /// All three conditions, because a busy agent that is not being *looked at* is not a reason
    /// to repaint: the agent screen only draws in `Search` mode, so a rename or a confirm over
    /// the top of it hides every spinner in the list.
    fn spinning(&self) -> bool {
        self.mode == Mode::Search && self.screen == Screen::Agents && self.agents.any_busy()
    }

    /// The pane the picker was opened over, or `None` when it cannot be told.
    ///
    /// Tiled first, then floating. A floating terminal that had focus loses it the moment the
    /// picker floats above it, so that case is genuinely unanswerable — and answering it wrong
    /// would omit a row the user can see no reason for.
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

    /// Rebuild the agent rows against the current term and where we are standing.
    ///
    /// ⚠️ When the origin pane cannot be determined the row is **shown**, not guessed at:
    /// omitting the wrong agent is a row that vanished for no reason the user can see, and
    /// showing a spare one costs a line. The degraded mode is safe rather than merely honest —
    /// an agent in our own session is a [`Jump::Focus`], so `Enter` on ourselves focuses the
    /// pane we came from instead of reaching the self-attach panic.
    fn rebuild_agents(&mut self, policy: Selection) {
        let current = self.matches.current_session.clone();
        let pane = self.origin_pane();
        let origin = match (current.as_deref(), pane) {
            (Some(session), Some(pane)) => Some((session, pane)),
            _ => None,
        };
        self.agents
            .rebuild(&self.matches.search_term, current.as_deref(), origin, policy);
    }

    /// Ask `claude-ps` for the list — once per answer, on the same terms as zoxide.
    ///
    /// Deliberately not on the 1s timer: that is the difference between a glance and a watch,
    /// and the watch has not been shown to be worth its reordering yet.
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

    /// Rebuild the directory rows against the current term and the current snapshot.
    ///
    /// Both inputs, every time, from both callers: a keystroke changes the term and a poll
    /// changes the tags, and a row that showed the wrong one of those is a row that promises
    /// the wrong thing about `Enter`.
    fn rebuild_dirs(&mut self, policy: Selection) {
        self.dirs.rebuild(
            &self.matches.search_term,
            &self.live,
            &self.dead,
            self.matches.current_session.as_deref(),
            policy,
        );
    }

    /// Ask zoxide for the list — once per answer.
    ///
    /// Deliberately not on the 1s timer. That would fork a process a second to re-read a
    /// database that only changes when you `cd`; the two moments that actually matter are the
    /// permission grant and becoming visible again.
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
        // Ctrl-c always dismisses, from either screen. It is the one unconditional exit, which
        // matters now that Esc has been given a job in the Search screen.
        if key.bare_key == BareKey::Char('c') && key.has_modifiers(&[KeyModifier::Ctrl]) {
            close_self();
            return false;
        }
        match self.mode {
            Mode::Search => self.handle_search_key(key),
            Mode::Rename => self.handle_rename_key(key),
            Mode::Confirm => self.handle_confirm_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            // `Tab` is the whole of the second screen's discoverability, which is why it is on
            // the help line of both. It carries the search term across: type `bipa`, `Tab`, and
            // you are asking the other list the same question.
            BareKey::Tab if key.has_no_modifiers() => {
                self.screen = self.screen.next();
                true
            },
            // 🔴 There is no `BareKey::BackTab` — Shift-Tab arrives as `Tab` carrying the Shift
            // modifier, so it has to be matched before nothing else claims it.
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
            // ⚠️ Esc no longer dismisses in one press when something is highlighted — it drops
            // the highlight, and only a second Esc closes. Always-on selection otherwise makes
            // the no-selection state unreachable, and that state is the only way to ask for a
            // name that fuzzy-matches something existing. The hint line says which state you are
            // in, so the intermediate step is not silent.
            BareKey::Esc if key.has_no_modifiers() => {
                // On the directory screen `Esc` backs out to the sessions rather than dropping
                // the highlight: there is no literal-text path here for a dropped highlight to
                // enable. A directory you have never been to is not something this list can
                // offer you, so "no selection" would be a state with nothing in it.
                // Both secondary screens back out to the sessions rather than stepping one
                // stop around the `Tab` cycle. `Esc` means *out*, and the session list is what
                // out is — a three-stop cycle would otherwise make `Esc` a second, slower `Tab`
                // that happens to run backwards. Neither has a literal-text path for a dropped
                // highlight to enable: an agent you are not running, like a directory you have
                // never visited, is not something this list can offer you.
                if self.screen != Screen::Sessions {
                    self.screen = Screen::Sessions;
                    return true;
                }
                if self.matches.selected.is_some() {
                    self.matches.drop_selection();
                    true
                } else {
                    close_self();
                    false
                }
            },
            // Rename always means the session you are *in* — `rename_session` takes no target,
            // so upstream's version is renaming the current session too, whatever its list
            // selection suggests. Here the current session is never a row, so the screen can
            // say plainly whose name is changing.
            BareKey::Char('r') if key.has_modifiers(&[KeyModifier::Ctrl]) => {
                self.begin_rename();
                true
            },
            // `Del` aims at the highlight, and the highlight can never be the session you are
            // in — it left the match set at the source. So this key cannot kill the session
            // running the picker, by construction rather than by a guard.
            // Sessions only. Dropping a directory out of zoxide is a different verb against a
            // different store, and it does not belong on a key whose confirm screen talks about
            // killing processes and throwing away saved layouts.
            BareKey::Delete if key.has_no_modifiers() && self.screen == Screen::Sessions => {
                self.begin_delete();
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
            Screen::Sessions => self.matches.move_selection(delta),
            Screen::Dirs => self.dirs.move_selection(delta),
            Screen::Agents => self.agents.move_selection(delta),
        }
    }

    /// `Enter` in the Search screen — the whole contract in one function.
    ///
    /// Both branches end in the same `switch_session` call, because the host resolves the name
    /// by itself: live → attach, has a resurrection layout → resurrect, neither → create with
    /// the default layout. The plugin never picks between them; it only decides *which name* to
    /// hand over — the highlighted row's, or the literal text.
    ///
    /// ⚠️ There is no confirm step on the create path any more, and so no layout picker: a
    /// session is created the moment you press `Enter`. Chosen deliberately — the layout list
    /// was a menu answered the same way every time.
    fn confirm_search(&mut self) {
        if let Some(name) = self.matches.selected_name() {
            switch_session(Some(name));
            close_self();
            return;
        }

        // No highlight: `Enter` takes the literal term. Two states refuse it, and in both the
        // prompt has already said why — so this is a no-op, not an error overlay. Upstream's
        // show_error() would be wrong here: handle_key clears self.error on *any* key and
        // swallows that keystroke, and this is a state you wander into by typing.
        if self.matches.is_own_name() || self.matches.name_error().is_some() {
            return;
        }

        // An empty name is a feature, not a hole: the host generates one. So `Esc` `Enter` on an
        // empty prompt is a fresh scratch session for free.
        let term = self.matches.search_term.clone();
        switch_session(if term.is_empty() { None } else { Some(term.as_str()) });
        close_self();
    }

    /// `Enter` on a directory row.
    ///
    /// The same one call as the session screen, for the same reason: the plugin picks a *name*
    /// and the host decides what that name means. The only new thing is the cwd, and it rides
    /// along on the one branch that can use it.
    ///
    /// ⚠️ Attaching passes the name **alone**. The host would accept a cwd there and throw it
    /// away (`ClientInfo::set_cwd` has no `Attach` arm), and an argument that is silently
    /// discarded is how you end up believing a session is somewhere it is not. If the tag says
    /// `[ATTACH]`, the row is an attach — the directory beside it is where the session would
    /// have been made, not where it is.
    fn confirm_dir(&mut self) {
        let Some(row) = self.dirs.selected_row() else {
            return;
        };
        // Refused — and the prompt has been saying so for as long as the row has been
        // highlighted. Not a courtesy: asking the host to attach to the session we are running
        // in does not decline, it panics the client (`commands.rs:794`).
        if row.action == Action::Here {
            return;
        }
        if row.action.carries_cwd() {
            switch_session_with_cwd(Some(&row.name), Some(PathBuf::from(&row.path)));
        } else {
            switch_session(Some(&row.name));
        }
        close_self();
    }

    /// `Enter` on an agent row — one meaning, two host calls.
    ///
    /// 🔴 The split is not a refinement, it is a hard constraint. Asking the host to attach to
    /// the session we are already in does not decline: `attach_with_session_name` reaches a bare
    /// `panic!("You are trying to attach to the current session")` (`src/commands.rs:793`) and
    /// takes the client down. So an agent sharing our session — which is exactly the case
    /// pane-level reachability exists for — must be a pane focus rather than a session switch.
    ///
    /// The upside is that the refusal the other screens need has no counterpart here. A
    /// directory row that resolves to our own session is `[HERE]` and does nothing, because
    /// there is nothing safe for it to do; an agent row that resolves to our own session has a
    /// call that works, so it stays a live target.
    fn confirm_agent(&mut self) {
        let Some(row) = self.agents.selected_row() else {
            return;
        };
        match row.jump {
            Jump::Focus => focus_terminal_pane(row.pane, false, false),
            Jump::Switch => {
                switch_session_with_focus(&row.session, None, Some((row.pane, false)))
            },
        }
        close_self();
    }

    fn begin_rename(&mut self) {
        // Nothing to rename if the host has not told us which session we are in yet.
        if self.matches.current_session.is_none() {
            return;
        }
        self.error = None;
        // Starting empty rather than prefilled, as upstream does: the old name is on the note
        // line where it cannot be half-deleted, and "empty" stays a state the screen already
        // knows how to refuse.
        self.rename = Rename::default();
        self.validate_rename();
        self.mode = Mode::Rename;
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
                if self.rename.input.pop().is_none() {
                    return false;
                }
                self.validate_rename();
                true
            },
            BareKey::Char(c) if key.has_no_modifiers() => {
                self.rename.input.push(c);
                self.validate_rename();
                true
            },
            _ => false,
        }
    }

    /// Everything that makes a name unusable, in the order that gives the most useful message.
    ///
    /// The collision check runs against the cached snapshot, not `matches.rows`: `rows` is the
    /// *filtered* set, so a name colliding with a session the search term happens to exclude
    /// would sail through it.
    fn validate_rename(&mut self) {
        let name = self.rename.input.as_str();
        self.rename.error = if name.is_empty() {
            // Valid on the create path, where the host names the session; there is no such
            // fallback for a rename.
            Some("name must not be empty".to_string())
        } else if self.matches.current_session.as_deref() == Some(name) {
            Some("already called that".to_string())
        } else if self.live.iter().chain(&self.dead).any(|(n, _)| n == name) {
            Some("a session by that name already exists".to_string())
        } else {
            validate_name(name).map(str::to_string)
        };
    }

    fn apply_rename(&mut self) {
        // A no-op, not an error: the prompt has already said why, live, for every keystroke it
        // took to get here.
        if self.rename.error.is_some() {
            return;
        }
        rename_session(&self.rename.input);
        self.mode = Mode::Search;
        // The picker stays open. `current_session` is stale until the host applies the rename;
        // the next poll corrects it, and nothing in the list depends on it.
    }

    fn begin_delete(&mut self) {
        let Some(row) = self.matches.selected.and_then(|i| self.matches.rows.get(i)) else {
            // No highlight means `Enter` is aimed at the typed text rather than at a row, and
            // `Del` has nothing to aim at either.
            return;
        };
        self.error = None;
        self.pending = Some(Pending { name: row.name.clone(), kind: row.kind });
        self.mode = Mode::Confirm;
    }

    fn handle_confirm_key(&mut self, key: KeyWithModifier) -> bool {
        match key.bare_key {
            BareKey::Enter if key.has_no_modifiers() => {
                self.apply_delete();
                true
            },
            BareKey::Esc if key.has_no_modifiers() => {
                self.pending = None;
                self.mode = Mode::Search;
                true
            },
            _ => false,
        }
    }

    fn apply_delete(&mut self) {
        self.mode = Mode::Search;
        let Some(pending) = self.pending.take() else {
            return;
        };
        let result = match pending.kind {
            Kind::Live => kill_sessions(&[pending.name.as_str()]),
            Kind::Resurrectable => delete_dead_session(&pending.name),
        };
        self.error = result.err().map(|e| format!("{} \"{}\": {}", pending.verb().to_lowercase(), pending.name, e));
        // Re-poll now instead of waiting out the tick. Both calls block until the host has
        // acknowledged, so the snapshot taken here already reflects them — and a row that
        // lingered for a second under the cursor is a row the next `Del` would aim at.
        self.poll();
    }

    fn set_term(&mut self, term: String) {
        // A new term is a new question; whatever the last host call complained about is no
        // longer the answer to it.
        self.error = None;
        // Re-filters against the cached snapshot rather than calling the host: a keystroke must
        // not have to wait a poll interval to change the list.
        self.matches.set_search_term(term, &self.live, &self.dead);
        // All three lists, whichever one you are looking at — the term is shared, so `Tab` must
        // never show you a list that has not caught up with what you typed.
        self.rebuild_dirs(Selection::SnapToTop);
        self.rebuild_agents(Selection::SnapToTop);
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
