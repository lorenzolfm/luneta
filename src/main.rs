//! luneta: a personal zellij session picker.
//!
//! The rule of the picker:
//!
//! > The highlighted row says what `Enter` does. With no highlight, `Enter` gives the text you
//! > typed to the host, which attaches, resurrects, or creates.
//!
//! The second sentence is safe because a session name is unique across live and resurrectable
//! sessions. The plugin never chooses between attach and create. It gives the host one name,
//! and the host resolves it (`src/commands.rs:752-786`): a live session gives an attach, a
//! saved layout gives a resurrect, and neither gives a create.

mod agents;
mod dirs;
mod elapsed;
mod fetch;
mod layout;
mod panes;
mod render;
mod sessions;

use std::collections::BTreeMap;
use std::path::PathBuf;

use agents::{AgentSet, Jump};
use dirs::{Action, DirSet, LIST};
use elapsed::Age;
use panes::Peeks;
use sessions::{validate_name, Contents, Focus, Kind, MatchSet, Selection, Session, Sessions};
use zellij_tile::prelude::*;

/// The pipe message this plugin answers, so that a key can open it on a chosen screen.
///
/// This is a pipe and not plugin configuration. Zellij identifies a plugin instance partly by
/// its configuration, so `LaunchOrFocusPlugin` with `screen "agents"` is a different plugin
/// from the same binding without it. Two such keys give two picker panes, one over the other.
/// A pipe sends the request to the instance that exists.
const SCREEN_PIPE: &str = "screen";

/// The only plugin configuration key this picker reads. It replaces [`agents::QUERY`] for a
/// server whose `PATH` does not hold `claude-ps`.
///
/// The value is the program: a name or an absolute path. It holds no arguments, because a path
/// can contain a space. Put the tool in a script if it needs arguments.
///
/// Every binding must pass the same value, or no binding must pass one. Zellij identifies a
/// plugin instance partly by its configuration, so two keys with different values give two
/// picker panes, one over the other. See [`SCREEN_PIPE`], which is a pipe for the same reason.
const AGENTS_COMMAND: &str = "agents_command";

/// The size of the floating pane, which the plugin applies to itself.
///
/// The default floating size shows about three rows, which is too few for a dozen sessions.
/// `change_floating_panes_coordinates` lets the plugin set its own size, so `config.kdl` needs
/// only `floating true` and no restart.
const FLOATING: (&str, &str, &str, &str) = ("20%", "20%", "60%", "60%");

/// The animation tick, in seconds. Ten ticks a second make the busy spinner look like motion.
///
/// This is not the poll interval. The host call in [`State::poll`] still runs once a second;
/// see [`TICKS_PER_POLL`]. Without that divisor, the faster timer would call
/// `get_session_list` ten times a second.
const TICK: f64 = 0.1;

/// Animation ticks in one second. [`TICK`] is a tenth of a second. The poll divisor below and
/// the age offset of the agent screen both use this number.
const TICKS_PER_SECOND: u64 = 10;

/// Animation ticks per session poll, so that the poll stays at its original once a second.
const TICKS_PER_POLL: u64 = TICKS_PER_SECOND;

/// How long the cursor must stay on a row before the preview command runs, in animation ticks.
///
/// Without this delay, the preview box starts one process for each keystroke. A held `↓` key
/// over a database of 130 directories would ask about every one of them. Two ticks is a fifth
/// of a second, which is shorter than a pause you can feel and longer than the repeat rate of
/// an arrow key.
const PREVIEW_DELAY: u64 = 2;

/// Which screen has the keyboard. There is no kill-all and no disconnect-others, because both
/// act on sessions this picker does not show.
#[derive(Default)]
pub enum Mode {
    /// Type, filter, move, `Enter`.
    #[default]
    Search,
    /// Typing a new name for the session you are in.
    ///
    /// `current` is the name the rename began on, and it is a copy on purpose. `poll` runs in
    /// every mode, so `matches.current_session` can change while you type — another client
    /// renaming the session you are in. Both readers, the note line and the `already called
    /// that` test, take the copy, so everything this screen says dates from the keystroke that
    /// opened it. Entering the mode is constructing the value, which is what makes the empty
    /// input and the absent session unrepresentable rather than merely prevented.
    Rename { current: String, input: String },
}

/// Which list the search screen shows. `Tab` changes it.
///
/// The lists stay separate. Sessions sort by what you did last, and directories sort by what
/// you do most. One list would force one of those orders onto rows that do not share a meaning,
/// and a hundred directories would hide six sessions. The search term is shared, so `Tab` asks
/// the other list the same question.
#[derive(Default, PartialEq, Eq, Clone, Copy)]
pub enum Screen {
    #[default]
    Sessions,
    Dirs,
    /// The Claude Code agents that are running, and which one is waiting on you.
    Agents,
}

impl Screen {
    /// `Tab` moves forward and `Shift-Tab` moves back. With two screens the two keys were the
    /// same key. With three screens, one of them is otherwise two presses away.
    ///
    /// The order is sessions, agents, directories. Sessions and agents both list things that
    /// run now, so they are next to each other. Directories list what does not run yet, so they
    /// are last, and `Shift-Tab` reaches them from the sessions in one press.
    fn next(self) -> Self {
        match self {
            Screen::Sessions => Screen::Agents,
            Screen::Agents => Screen::Dirs,
            Screen::Dirs => Screen::Sessions,
        }
    }

    /// The name a key binding uses to ask for this screen. See [`State::pipe`].
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

/// What the preview box shows, which decides the cache that answers for it.
enum Target {
    /// A directory, keyed by its path. Answered by eza.
    Dir(String),
    /// A pane, keyed by its session and its id. Answered by `zellij action dump-screen`.
    Pane(String, u32),
}

impl Target {
    /// How the target is named in its cache, and, for a pane, in the context of its command.
    fn key(&self) -> String {
        match self {
            Target::Dir(path) => path.clone(),
            Target::Pane(session, pane) => panes::key(session, *pane),
        }
    }
}

#[derive(Default)]
struct State {
    mode: Mode,
    screen: Screen,
    matches: MatchSet,
    /// The directory list, and why it is empty. zoxide fills it, and nothing else waits for
    /// it.
    dirs: DirSet,
    /// The agent list. `claude-ps` fills it, on the same terms as `dirs`.
    agents: AgentSet,
    /// [`AGENTS_COMMAND`], if a binding passed one. `None` means the plain [`agents::QUERY`].
    agents_command: Option<String>,
    /// The last pane manifest, and the focused tab.
    ///
    /// This answers one question: which pane was focused when the picker opened. The picker is
    /// a floating pane, so `get_focused_pane_info` returns the picker itself. It resolves
    /// through `Screen::get_active_pane_id`, which ignores the layer. Tiled and floating panes
    /// keep separate `active_panes` maps, so the terminal below still reports `is_focused` in
    /// its own layer. Only the manifest can reach that pane.
    panes: Option<PaneManifest>,
    active_tab: Option<usize>,
    /// The last snapshot, so that a keystroke can filter again before the next poll.
    sessions: Sessions,
    /// A host call that returned `Err`. The search screen shows it until the user does
    /// something that can produce a new one. It is never an overlay, so it cannot take a
    /// keystroke.
    error: Option<String>,
    /// Animation ticks since load, for the busy spinner and, divided, for the poll.
    ///
    /// The count never resets, so every busy row turns together and the list reads as one
    /// thing.
    frame: u64,
    /// The screen of each pane the cursor has stopped on. `zellij action dump-screen` fills it,
    /// on the same terms as `dirs` and `agents`.
    peeks: Peeks,
    /// What the cursor is on, and the frame it arrived. This is the delay behind the preview
    /// box. See [`State::follow_preview`].
    preview_at: Option<(String, u64)>,
    /// The frame that received the agent snapshot in [`State::agents`].
    ///
    /// The snapshot does not change while the screen is up, and its `age` field is a duration
    /// measured when `claude-ps` ran. Without a record of when that was, the age column would
    /// also stop. See [`State::agents_since`].
    agents_taken_at: u64,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        // Read before the permission request, because the first `ask_agents` runs from the
        // grant and must know which command to run.
        self.agents_command = configuration
            .get(AGENTS_COMMAND)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        // `RunCommands` is the cost of the directory screen. The host grants the set, not the
        // item, so adding it prompts once more and a refusal also stops the session list. There
        // is no way to ask for it alone.
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        // There is no `SessionUpdate`. That event refreshes the age of the current session
        // only and leaves the others unchanged. The poll below takes the whole list from one
        // snapshot, so that the ages agree with each other.
        subscribe(&[
            EventType::PermissionRequestResult,
            EventType::Timer,
            EventType::Key,
            // zoxide and claude-ps both answer here, and `Visible` is when to ask again.
            EventType::RunCommandResult,
            EventType::Visible,
            // Not for drawing. These two are the only route to the pane the picker was
            // opened over. See `State::panes`.
            EventType::PaneUpdate,
            EventType::TabUpdate,
        ]);
        // No privileged call here. A grant arrives later as `PermissionRequestResult`, so a
        // call from `load()` is denied. A builtin plugin can do it because builtins pass the
        // permission checks (`zellij_exports.rs:5428`).
        set_timeout(0.0);
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::PermissionRequestResult(status) => {
                if matches!(status, PermissionStatus::Granted) {
                    // Safe only now: both calls need `ChangeApplicationState`.
                    let plugin_id = get_plugin_ids().plugin_id;
                    rename_plugin_pane(plugin_id, "luneta");
                    resize_self(plugin_id);
                    // Same reason, other permission: this is the first moment the host
                    // accepts a command.
                    self.ask_zoxide();
                    self.ask_agents();
                } else {
                    // The picker cannot work without the grant, because the session list needs
                    // `ReadApplicationState`. The two secondary screens can say so.
                    self.dirs.fail("permission denied");
                    self.agents.fail("permission denied");
                }
                true
            },
            Event::Timer(_) => {
                set_timeout(TICK);
                // Tested before the increment, so that the poll runs on frame 0. That is the
                // first tick after `set_timeout(0.0)` in `load`, which fills the session list
                // immediately. The other order would leave the first second empty.
                let polled = self.frame.is_multiple_of(TICKS_PER_POLL);
                if polled {
                    self.poll();
                }
                self.frame = self.frame.wrapping_add(1);
                // Called for its effect, so it runs before the `||` below can skip it. This is
                // the tick that asks for the preview.
                let asked = self.follow_preview();
                // Nine ticks in ten change nothing. A redraw on them would rebuild and repaint
                // the whole screen ten times a second with the same result.
                polled || self.spinning() || asked
            },
            Event::Key(key) => self.handle_key(key),
            Event::RunCommandResult(exit_code, stdout, stderr, context) => {
                // Which of our commands answered, if any. The screens share the context key
                // and differ in its value. Any other value belongs to another plugin.
                match context.get(dirs::CONTEXT_KEY).map(String::as_str) {
                    Some(dirs::CONTEXT_VALUE) => {
                        self.dirs.ingest(exit_code, &stdout, &stderr);
                        self.rebuild_dirs(Selection::Hold);
                        true
                    },
                    // Filed by the path the command carried, not by the cursor. A reply
                    // arrives at any time, and the cursor has usually moved.
                    Some(dirs::PREVIEW_VALUE) => match context.get(dirs::PATH_KEY) {
                        Some(path) => {
                            self.dirs.ingest_listing(path.clone(), exit_code, &stdout, &stderr);
                            true
                        },
                        // Ours by its key, but with nothing that says what it is about. A
                        // guess is the failure this channel exists to prevent, so it is
                        // dropped.
                        None => false,
                    },
                    Some(panes::CONTEXT_VALUE) => match context.get(panes::PANE_KEY) {
                        Some(pane) => {
                            self.peeks.ingest(pane.clone(), exit_code, &stdout, &stderr);
                            true
                        },
                        None => false,
                    },
                    Some(agents::CONTEXT_VALUE) => {
                        self.agents.ingest(exit_code, &stdout, &stderr);
                        // The origin of the age column. It is recorded here, not where the
                        // command was sent, because `claude-ps` measured `age` at its end and
                        // this is the nearest moment we can name. The error is the run time of
                        // the command, which is less than the second the column rounds to.
                        self.agents_taken_at = self.frame;
                        self.rebuild_agents(Selection::Hold);
                        true
                    },
                    _ => false,
                }
            },
            // Neither event redraws anything. They keep the answer to "which pane did we come
            // from" current, which decides whether one row shows.
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
            // `launch-or-focus` starts the plugin, so one instance serves many openings, and
            // each `cd` between them changes the answer from zoxide. There is nothing to
            // redraw: the reply arrives as its own event.
            Event::Visible(true) => {
                self.ask_zoxide();
                // A listing or a screen from the last opening describes what was there
                // minutes ago. The box must show what is there now, and a pane screen changes
                // faster than anything else the picker reads.
                self.dirs.forget_listings();
                self.peeks.forget();
                // The agent snapshot is taken here and then held while the screen is up. That
                // makes the attention-first order safe, because no row moves while you read it.
                // The list holds, but the ages continue. See [`State::agents_since`].
                self.ask_agents();
                false
            },
            _ => false,
        }
    }

    /// Open on a named screen. Send `MessagePlugin` with `name "screen"` and a payload of
    /// `sessions`, `agents` or `dirs`.
    ///
    /// The name is checked, because any pipe can reach this plugin and an unrelated one must
    /// not change the screen while somebody types. An unknown payload is also ignored, so that
    /// an error in a key binding does nothing.
    ///
    /// Nothing is refreshed here. If the picker was closed, the host makes it visible and the
    /// `Visible` handler takes the snapshot. If it was open, it already has one.
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
    /// One `get_session_list()` snapshot per poll.
    ///
    /// This is not the `SessionUpdate` event, which refreshes the age of the current session
    /// only. One call for the whole list makes the ages agree with each other.
    fn poll(&mut self) {
        let Ok(snapshot) = get_session_list() else {
            return;
        };
        let current_session = snapshot.live_sessions.iter().find(|s| s.is_current_session);
        let current = current_session.map(|s| s.name.clone());
        // Filled in the same pass, because this consumes the snapshot. The preview box and
        // the ages must come from one snapshot, or the box would describe a session as it was
        // a second before the row beside it.
        let mut contents = BTreeMap::new();
        // The one place the two lists are still crossable, and each is named beside the
        // snapshot field it comes from. Everything downstream takes the pair.
        self.sessions = Sessions {
            live: snapshot
                .live_sessions
                .into_iter()
                // The current session is removed here, not in the renderer, so that the
                // rendered list stays equal to the match set.
                .filter(|s| !s.is_current_session)
                .map(|session| {
                    let name = session.name.clone();
                    let age = Age::new(session.creation_time);
                    contents.insert(name.clone(), contents_of(session));
                    Session { name, age }
                })
                .collect(),
            // The host's own shape for this list is `Vec<(String, Duration)>`
            // (`zellij-utils/src/data.rs:2660`), so this one costs a map where it used to be
            // a move.
            dead: snapshot
                .resurrectable_sessions
                .into_iter()
                .map(|(name, age)| Session { name, age: Age::new(age) })
                .collect(),
        };
        self.matches.contents = contents;
        self.matches.refresh(&self.sessions, current);
        // The action of a directory row comes from the session list, so it becomes stale on
        // the same tick.
        self.rebuild_dirs(Selection::Hold);
        // This does not ask for the agent list again. It computes which call `Enter` makes,
        // which row is our own, and the age of the snapshot. The first two come from the
        // session poll, and the third comes from the clock. See [`State::agents_since`].
        self.rebuild_agents(Selection::Hold);
    }

    /// Is a spinner on the screen now, and is this tick worth a redraw?
    ///
    /// All three conditions are necessary. The agent screen draws in `Search` mode only, so a
    /// rename or a confirmation hides every spinner in the list.
    fn spinning(&self) -> bool {
        matches!(self.mode, Mode::Search)
            && self.screen == Screen::Agents
            && self.agents.any_busy()
    }

    /// The pane the picker was opened over, or `None` if it cannot be found.
    ///
    /// Tiled panes are tested first, then floating panes. A floating terminal loses focus when
    /// the picker opens above it, so that case has no answer. A wrong answer would remove a row
    /// for no visible reason.
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

    /// Rebuild the agent rows against the current term and the current position.
    ///
    /// If the origin pane is unknown, the row shows. To remove the wrong agent is to remove a
    /// row for no visible reason, and one extra row costs one line. This is also safe: an agent
    /// in our own session is a [`Jump::Focus`], so `Enter` on our own row focuses the pane we
    /// came from and does not reach the self-attach panic.
    fn rebuild_agents(&mut self, policy: Selection) {
        let current = self.matches.current_session.clone();
        let pane = self.origin_pane();
        let origin = match (current.as_deref(), pane) {
            (Some(session), Some(pane)) => Some((session, pane)),
            _ => None,
        };
        let since = self.agents_since();
        self.agents
            .rebuild(&self.matches.search_term, current.as_deref(), origin, since, policy);
    }

    /// The age of the agent snapshot, counted on the animation clock.
    ///
    /// The screen does not ask for the list again while it is up, because a status that changed
    /// would reorder an attention-first list as you read it. The list must hold, but the clock
    /// must not: without this, an agent that has waited three minutes shows `4s` for as long as
    /// the picker is open, in the column that decides where you go.
    ///
    /// This is safe because it is the same number on every row. The same offset on both sides
    /// of a comparison cannot change its result, so the order in each status does not move.
    ///
    /// The count is in ticks, because the plugin has no clock: a wasi sandbox with `/host` open
    /// is not a clock. The timer drifts with what the host does to it, which the one-second
    /// granularity of the column absorbs. A new opening reads a new snapshot.
    fn agents_since(&self) -> Age {
        Age::from_secs(self.frame.wrapping_sub(self.agents_taken_at) / TICKS_PER_SECOND)
    }

    /// Ask `claude-ps` for the list, once for each answer, on the same terms as zoxide.
    ///
    /// This is not on the timer. A list that refreshes would reorder its rows as you read
    /// them.
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

    /// Keep the preview box on the highlighted row, and ask about that row when the cursor
    /// stops. Returns true if it asked, which is the only change to the screen.
    ///
    /// A cursor that moves through a list passes over rows on its way. A question for each of
    /// them would start one process for each keystroke. The target is therefore recorded on the
    /// tick it changes, and asked about [`PREVIEW_DELAY`] ticks later if it is still the
    /// target.
    ///
    /// This runs in `Search` mode only. A confirmation or a rename hides the box.
    fn follow_preview(&mut self) -> bool {
        if !matches!(self.mode, Mode::Search) {
            return false;
        }
        let Some(target) = self.preview_target() else {
            self.preview_at = None;
            return false;
        };
        let key = target.key();
        match &self.preview_at {
            // The same target. Ask when it has been there long enough. The claim each cache
            // makes below refuses a second call, so this cannot ask twice.
            Some((at, since)) if *at == key => {
                if self.frame.wrapping_sub(*since) < PREVIEW_DELAY {
                    return false;
                }
            },
            // A new target: start the count and ask nothing.
            _ => {
                self.preview_at = Some((key, self.frame));
                return false;
            },
        }
        match target {
            Target::Dir(path) => {
                if !self.dirs.begin_listing(&path) {
                    return false;
                }
                // `--` because a directory can be named `-l`, and the path last because eza
                // reads it there.
                let mut command = LIST.to_vec();
                command.push("--");
                command.push(&path);
                run_command(
                    &command,
                    BTreeMap::from([
                        (dirs::CONTEXT_KEY.to_string(), dirs::PREVIEW_VALUE.to_string()),
                        // What the reply is about, sent and returned. See [`dirs::PATH_KEY`].
                        (dirs::PATH_KEY.to_string(), path.clone()),
                    ]),
                );
            },
            Target::Pane(session, pane) => {
                if !self.peeks.claim(&key) {
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
                        (panes::PANE_KEY.to_string(), key.clone()),
                    ]),
                );
            },
        }
        true
    }

    /// What the cursor is on, named as its cache names it.
    ///
    /// Two screens point at a pane and one points at a directory. The pane of a session is the
    /// focused pane of its active tab (see [`sessions::Focus`]). An agent is itself a pane.
    fn preview_target(&self) -> Option<Target> {
        match self.screen {
            Screen::Dirs => self.dirs.selected_row().map(|row| Target::Dir(row.path.clone())),
            Screen::Sessions => {
                let row = self.matches.selected.and_then(|i| self.matches.rows.get(i))?;
                // A dead session has no process and thus no screen. There is nothing to
                // ask.
                let focus = self.matches.contents.get(&row.name)?.focus.as_ref()?;
                Some(Target::Pane(row.name.clone(), focus.pane))
            },
            Screen::Agents => self
                .agents
                .selected_row()
                .map(|row| Target::Pane(row.session.clone(), row.pane)),
        }
    }

    /// Rebuild the directory rows against the current term and the current snapshot.
    ///
    /// Both callers pass both inputs each time. A keystroke changes the term, and a poll changes
    /// the actions. A row built from a stale input promises the wrong result for `Enter`.
    fn rebuild_dirs(&mut self, policy: Selection) {
        self.dirs.rebuild(
            &self.matches.search_term,
            &self.sessions,
            self.matches.current_session.as_deref(),
            policy,
        );
    }

    /// Ask zoxide for the list, once for each answer.
    ///
    /// This is not on the timer. A timer would start one process a second to read a database
    /// that changes only when you `cd`. The two moments that matter are the permission grant
    /// and the return to visibility.
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
        // `Ctrl-c` always closes the picker, from every screen.
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
            // `Tab` is how you find the other screens, so it is on every help line. It also
            // carries the search term: type `bipa`, press `Tab`, and you ask the other list the
            // same question.
            BareKey::Tab if key.has_no_modifiers() => {
                self.screen = self.screen.next();
                true
            },
            // There is no `BareKey::BackTab`. Shift-Tab arrives as `Tab` with the Shift
            // modifier, so this arm must follow the one above.
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
            // `Esc` closes the picker, from every screen and at every highlight. One press
            // has one meaning, and there is no intermediate state to leave.
            //
            // It once did two other things first: it left a secondary screen, and then it
            // removed the highlight. The second of those was the only way to ask for a name
            // that matches a session that exists, such as `infra` while `infra-staging` is
            // live. That now needs `Ctrl-c`, a new opening, and a name that no row matches.
            BareKey::Esc if key.has_no_modifiers() => {
                close_self();
                false
            },
            // Rename always acts on the current session, because `rename_session` takes no
            // target. The current session is never a row, so the screen can name it.
            BareKey::Char('r') if key.has_modifiers(&[KeyModifier::Ctrl]) => {
                self.begin_rename();
                true
            },
            // `Del` acts on the highlight, and the highlight is never the current session,
            // which left the match set at the source. This key thus cannot kill the session
            // that runs the picker.
            //
            // Sessions only. To remove a directory from zoxide is a different action on a
            // different store.
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
            Screen::Sessions => self.matches.move_selection(delta),
            Screen::Dirs => self.dirs.move_selection(delta),
            Screen::Agents => self.agents.move_selection(delta),
        }
    }

    /// `Enter` on the search screen.
    ///
    /// Both branches end in the same `switch_session` call, because the host resolves the name:
    /// a live session gives an attach, a saved layout gives a resurrect, and neither gives a
    /// create with the default layout. The plugin only chooses the name, which is the name of
    /// the highlighted row or the text you typed.
    ///
    /// The create path has no confirmation and no layout list, so `Enter` creates the session
    /// immediately.
    fn confirm_search(&mut self) {
        if let Some(name) = self.matches.selected_name() {
            switch_session(Some(name));
            close_self();
            return;
        }

        // With no highlight, `Enter` takes the term. Two states refuse it, and the prompt has
        // already given the reason for both, so this does nothing and shows no error. An error
        // overlay would take the next keystroke, and you reach this state by typing.
        if self.matches.is_own_name() || self.matches.name_error().is_some() {
            return;
        }

        // An empty name is valid, because the host generates one. `Enter` on an empty prompt
        // thus gives a new session.
        let term = self.matches.search_term.clone();
        switch_session(if term.is_empty() { None } else { Some(term.as_str()) });
        close_self();
    }

    /// `Enter` on a directory row.
    ///
    /// This makes the same call as the session screen: the plugin chooses a name, and the host
    /// decides what the name means. Only the cwd is new, and it goes with the one outcome that
    /// can use it.
    ///
    /// An attach passes the name alone. The host accepts a cwd there and discards it, because
    /// `ClientInfo::set_cwd` has no `Attach` arm, and a discarded argument makes you believe a
    /// session is somewhere it is not. On an attach row, the directory beside the name is where
    /// the session would have been created, not where it is.
    ///
    /// This is the only place that picks between the two calls, so it names every [`Action`]
    /// rather than testing one and letting the rest fall through. A fifth outcome stops the
    /// build here, where the choice is made, instead of quietly taking the arm that drops the
    /// cwd.
    fn confirm_dir(&mut self) {
        let Some(row) = self.dirs.selected_row() else {
            return;
        };
        match row.action {
            // Refused, and the prompt has said so for as long as the row was highlighted. An
            // attach to the current session does not fail, it panics the client
            // (`commands.rs:794`).
            Action::Here => return,
            Action::Create => {
                switch_session_with_cwd(Some(&row.name), Some(PathBuf::from(&row.path)))
            },
            Action::Attach | Action::Resurrect => switch_session(Some(&row.name)),
        }
        close_self();
    }

    /// `Enter` on an agent row: one meaning, two host calls.
    ///
    /// The two calls are necessary. An attach to the current session does not fail:
    /// `attach_with_session_name` calls `panic!("You are trying to attach to the current
    /// session")` (`src/commands.rs:793`) and stops the client. An agent in our own session
    /// must therefore be a pane focus, not a session switch.
    ///
    /// This screen thus needs no refusal. A directory row for our own session can do nothing
    /// safely, but an agent row for our own session has a call that works.
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
        // There is nothing to rename until the host reports the current session.
        let Some(current) = self.matches.current_session.clone() else {
            return;
        };
        self.error = None;
        // The input starts empty. The old name is on the note line, where you cannot delete
        // half of it, and the screen already refuses an empty name.
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
                // The input lives in the mode, and this function runs in one mode. The `else`
                // is what saying so to the compiler costs.
                let Mode::Rename { input, .. } = &mut self.mode else {
                    return false;
                };
                // An empty input has nothing to delete, and nothing to redraw.
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

    /// Every reason a name is refused, in the order that gives the most useful message.
    ///
    /// Answered on read — once per draw, once on `Enter` — the way `MatchSet::name_error`
    /// already answers for the search screen over the same `validate_name`. A remembered answer
    /// is a second fact about the input, and every edit path has to remember to renew it.
    ///
    /// The collision test uses the snapshot, not `matches.rows`, because `rows` is filtered. A
    /// name that collides with a session the term excludes would otherwise pass.
    fn rename_error(&self, current: &str, name: &str) -> Option<&'static str> {
        if name.is_empty() {
            // Valid on the create path, where the host names the session. A rename has no
            // such fallback.
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
        // This does nothing and shows no error. The prompt gave the reason on every
        // keystroke.
        if self.rename_error(current, input).is_some() {
            return;
        }
        rename_session(input);
        self.mode = Mode::Search;
        // The picker stays open. `current_session` is stale until the host applies the
        // rename, and the next poll corrects it.
    }

    /// Act on the highlighted row: kill a live session, or delete a dead one.
    ///
    /// There is no confirmation. The key acts on the highlight, and the highlight is never the
    /// current session, so `Del` cannot stop the session that runs the picker.
    ///
    /// A kill and a delete are two different actions. A kill stops a running session, and the
    /// session returns as a dead row if the host had serialized it (`serialization_interval`,
    /// 10s by default, and off when `session_serialization false`). A delete removes the saved
    /// layout of a session that already stopped, and is permanent. To do both, press `Del` once
    /// on the live row and once again on the dead row that follows.
    fn delete_selected(&mut self) {
        // With no highlight, `Enter` acts on the text you typed, and `Del` has no target.
        let Some(row) = self.matches.selected.and_then(|i| self.matches.rows.get(i)) else {
            return;
        };
        let name = row.name.clone();
        let (verb, result) = match row.kind {
            Kind::Live => ("kill", kill_sessions(&[name.as_str()])),
            Kind::Resurrectable => ("delete", delete_dead_session(&name)),
        };
        // The term searched for a session that is gone. It is cleared before the error is
        // set, because `set_term` clears `self.error` and would remove the reason.
        self.set_term(String::new());
        self.error = result.err().map(|e| format!("{} \"{}\": {}", verb, name, e));
        // Poll now, and do not wait for the tick. The call waits for the host, so this
        // snapshot holds the result. A row that stayed for a second is a row the next `Del`
        // would act on.
        self.poll();
    }

    fn set_term(&mut self, term: String) {
        // A new term is a new question, and the last error is not an answer to it.
        self.error = None;
        // Filters the snapshot again and does not call the host, so that a keystroke does not
        // wait for the next poll.
        self.matches.set_search_term(term, &self.sessions);
        // All three lists, because the term is shared and `Tab` must never show a list that
        // is behind what you typed.
        self.rebuild_dirs(Selection::SnapToTop);
        self.rebuild_agents(Selection::SnapToTop);
    }
}

/// Which pane of a live session to preview, from the snapshot that gives the ages.
///
/// This is the focused pane of the active tab, or the first pane anywhere that has a screen.
/// The filter is `is_selectable && !is_suppressed && !is_plugin`. The first two conditions
/// remove the tab bar and the status bar of zellij, which are panes in this manifest. The third
/// removes plugin panes, which dump an empty screen. See [`Focus`].
fn contents_of(session: SessionInfo) -> Contents {
    // Destructured, because the loop below empties the pane manifest one tab at a time and
    // consumes the tab list as it does so.
    let SessionInfo { tabs, mut panes, .. } = session;
    let mut total = 0;
    let mut focus = None;
    for tab in tabs {
        let in_tab: Vec<PaneInfo> = panes
            .panes
            // Removed, not read. A tab position appears once, so this moves the titles into
            // the summary instead of copying them.
            .remove(&tab.position)
            .unwrap_or_default()
            .into_iter()
            .filter(|pane| pane.is_selectable && !pane.is_suppressed && !pane.is_plugin)
            .collect();
        total += in_tab.len();
        // The focused pane of the active tab wins. All else is a fallback, so a session whose
        // active tab has nothing to dump still gets a preview.
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

/// Set the size of the picker, and remove the zellij frame from it.
///
/// The `borderless` flag is why the picker draws only its own two boxes. Zellij frames a
/// floating pane by default, so the picker sat inside a third box that it did not draw and
/// could not style. `set_pane_frame_style` is a session setting and would have removed the
/// frame from every other pane. `FloatingPaneCoordinates` carries the flag for one pane, so
/// `change_floating_panes_coordinates`, which the picker already calls to set its size, removes
/// the frame from this pane only.
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

