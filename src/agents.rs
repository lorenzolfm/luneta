//! The Claude Code agents that run now, and which one waits for you.
//!
//! The rule of this screen:
//!
//! > An agent row is a zellij pane, and `Enter` puts you in it.
//!
//! That rule needs two host calls, because zellij does not let you attach to the current
//! session: `attach_with_session_name` calls `panic!("You are trying to attach to the current
//! session")` (`src/commands.rs:793`) instead of returning an error. An agent in our own
//! session is therefore a pane focus, not a `switch_session`. See [`Jump`].
//!
//! The plugin cannot read the agents itself. Its wasi sandbox opens only `/host`, `/data`,
//! `/cache` and `/tmp`, so it can reach neither `~/.claude/sessions/<pid>.json` nor `/proc`.
//! Something outside must join what Claude reports to the pane that runs it. `claude-ps` does
//! that and prints JSON. This module only deserialises, filters and sorts.

use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;

use crate::sessions::Selection;

/// The command behind this screen. The server finds it on `PATH`, as it finds `zoxide`. There
/// is no shell and no `$HOME`, and no install path is written here.
///
/// If the server `PATH` does not hold it, the `agents_command` configuration key replaces this
/// value.
pub const QUERY: [&str; 1] = ["claude-ps"];

/// Marks our own `RunCommandResult`. It shares the key with the directory screen and differs
/// in the value, which is how the replies are told apart.
pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "agents";

/// One object from `claude-ps`, before this screen decides anything about it.
///
/// Only the keys this screen reads are named, and all other keys are ignored. A new key thus
/// costs nothing. The format this replaced counted columns exactly, so a new `started_at`
/// column stopped a screen that understood every other field.
///
/// A key this screen depends on must still stop the parse when it is absent. `status_age` did
/// not: it had a `#[serde(default)]`, so a renamed key became a silent `0` and every row showed
/// `0s`. Tolerance is for the keys we do not read.
#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    status: Option<String>,
    /// Seconds in the current status.
    ///
    /// This field has no `#[serde(default)]`. It had one, and `claude-ps` renamed the key to
    /// `status_age`. Every row then took the default `0`, and the column showed `0s` for every
    /// agent. A silent zero is worse than a blank, because it is a wrong answer in the column
    /// that decides where you go. An absent key now stops the parse and puts the reason on the
    /// note line.
    ///
    /// The alias keeps a `claude-ps` from before the rename working, because neither side has a
    /// version the other can read.
    #[serde(alias = "age")]
    status_age: u64,
    /// `null` if the agent is not in zellij. One object, not two fields, so that a session
    /// cannot arrive without its pane.
    #[serde(default)]
    zellij: Option<WireZellij>,
    #[serde(default)]
    cwd: Option<String>,
    /// The label Claude gives the session. Shown only if a person chose it. See
    /// [`chosen_name`].
    #[serde(default)]
    name: Option<String>,
    /// Who chose `name`, or `null`. This field is optional, unlike `status_age`, because
    /// `claude-ps` documents `null` as a value.
    #[serde(default)]
    name_source: Option<String>,
}

#[derive(Deserialize)]
struct WireZellij {
    session: String,
    /// A string, because it comes from an environment variable. This screen needs a `u32` for
    /// `focus-pane-id`, so a value that does not parse is an agent it cannot reach.
    pane: String,
}

/// What `Enter` on this row does. This is not shown on the screen, because both values mean
/// the same thing to the user. They differ only in the host call they can use.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// A different zellij session: attach to it and land on the pane.
    Switch,
    /// Our own session, and a different pane. `switch_session` would panic the client here, so
    /// this is a pane focus.
    Focus,
}

/// One agent out of `claude-ps`, already known to be inside zellij.
struct Agent {
    session: String,
    /// What the row is called: the chosen name, or the zellij session if there is none.
    /// [`parse`] decides this once, because the fuzzy term matches against it and the hit
    /// positions are offsets into it. A later change would paint the highlight on a different
    /// string.
    display: String,
    pane: u32,
    status: String,
    age: Duration,
    cwd: String,
}

/// One row, which is also one match-set entry, as on the other two screens.
pub struct AgentRow {
    /// The zellij session name. This is the address, not the label: `Enter` acts on it and on
    /// `pane`, whatever the row is called.
    pub session: String,
    /// What the row is called: the chosen name, or the session if there is none. The fuzzy term
    /// matched this string, and `indices` are offsets into it.
    pub display: String,
    pub pane: u32,
    /// The status from Claude, unchanged. Nothing compares it to a known set to decide whether
    /// to show it. See [`status_rank`].
    pub status: String,
    /// Time in the current status when this row was built: the value in the snapshot plus the
    /// age of the snapshot. This is a duration, not a time.
    ///
    /// The offset is added here because it is the same number on every row. See
    /// [`crate::State::agents_since`].
    pub age: Duration,
    pub cwd: String,
    /// Another visible row has the same label, so the row must show its pane.
    pub shared: bool,
    pub jump: Jump,
    /// Character positions the fuzzy matcher hit **in `display`**, for highlighting.
    pub indices: Vec<usize>,
    rank: u8,
    score: i64,
    is_exact: bool,
}

impl AgentRow {
    /// What the row is called, plus `:pane` when the label alone does not identify one row.
    ///
    /// The suffix is for the screen only. The term never matches it, because nobody types it.
    ///
    /// Only the suffix can be added here. The base must stay `display`, because the matcher ran
    /// on that string and `indices` are offsets into it.
    pub fn label(&self) -> String {
        if self.shared {
            format!("{}:{}", self.display, self.pane)
        } else {
            self.display.clone()
        }
    }
}

/// Why the agent list is empty.
///
/// This is the enum of the directory screen, for the same reason. "Still asking", "the tool is
/// absent" and "nothing runs" are three different facts, and a blank list for all three makes a
/// missing program look like a broken feature.
#[derive(Default)]
pub enum Status {
    #[default]
    Waiting,
    Ready,
    Failed(String),
}

#[derive(Default)]
pub struct AgentSet {
    pub status: Status,
    pub rows: Vec<AgentRow>,
    pub selected: Option<usize>,
    pub asking: bool,
    /// Agents that run outside zellij. `Enter` cannot reach them, so they are not rows. The
    /// count stays, and the note line shows it, so that the name of such an agent gives an
    /// empty list with an explanation.
    pub outside: usize,
    all: Vec<Agent>,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
    /// Take the reply from the tool.
    ///
    /// An exit other than 0 is reported, not discarded. The most probable failure is that the
    /// tool is not installed, which otherwise looks the same as an empty list.
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        self.asking = false;
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.status = Status::Failed(if reason.is_empty() {
                "claude-ps is not available".to_string()
            } else {
                format!("claude-ps: {}", reason)
            });
            self.all.clear();
            self.outside = 0;
            return;
        }
        match parse(&String::from_utf8_lossy(stdout)) {
            Ok((agents, outside)) => {
                self.all = agents;
                self.outside = outside;
                self.status = Status::Ready;
            },
            // A document this plugin cannot read. It is reported, not discarded: an empty
            // list would mean that no agents run, when it means that `claude-ps` and the
            // picker disagree.
            Err(reason) => {
                self.status = Status::Failed(reason);
                self.all.clear();
                self.outside = 0;
            },
        }
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Status::Failed(reason.into());
        self.all.clear();
        self.outside = 0;
    }

    /// Rebuild against the term and the current position.
    ///
    /// `origin` is the `(session, pane)` the picker was opened over. The pair is removed, not
    /// the session, so that another agent in the same session stays a valid target.
    ///
    /// `current` is our session name, which is known even when the pane is not.
    ///
    /// `since` is the age of the snapshot, added to the age of every row. It describes now, not
    /// the agents, so it arrives with each rebuild and not with each reply.
    pub fn rebuild(
        &mut self,
        term: &str,
        current: Option<&str>,
        origin: Option<(&str, u32)>,
        since: Duration,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_row().map(|r| (r.session.clone(), r.pane)),
        };
        self.rows.clear();

        let matcher = self
            .matcher
            .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));

        for agent in &self.all {
            // The agent we are in is removed here, as the session screen removes the current
            // session, so that the rendered list stays equal to the match set.
            if origin == Some((agent.session.as_str(), agent.pane)) {
                continue;
            }
            let (score, indices, is_exact) = if term.is_empty() {
                (0, Vec::new(), false)
            } else {
                // Matched against the bare label. You type what you see, so a row that shows
                // a chosen name must answer to that name. The `:pane` suffix is decided after
                // this filter, and nobody types it.
                match matcher.fuzzy_indices(&agent.display, term) {
                    Some((score, indices)) => (score, indices, agent.display == term),
                    None => continue,
                }
            };
            self.rows.push(AgentRow {
                session: agent.session.clone(),
                display: agent.display.clone(),
                pane: agent.pane,
                status: agent.status.clone(),
                age: agent.age + since,
                cwd: agent.cwd.clone(),
                shared: false,
                jump: if current == Some(agent.session.as_str()) {
                    Jump::Focus
                } else {
                    Jump::Switch
                },
                indices,
                rank: status_rank(&agent.status),
                score,
                is_exact,
            });
        }

        // Attention first. `rank` sorts above `age` and `score`, which keeps the boundary
        // between the agents that want you and the rest in one place as you type.
        //
        // This is safe only because the snapshot does not change while the screen is up. A poll
        // would move rows under the cursor as agents change status. The ages do move without a
        // new reply, and that is still safe: `since` is one number added to every row, and the
        // same offset on both sides cannot change a comparison.
        self.rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| a.rank.cmp(&b.rank))
                // Most recent first. `age` sorts above `score`, because in one status the
                // agent that changed a moment ago is the one you worked with. A fuzzy score
                // moves the rows on every keystroke.
                .then_with(|| a.age.cmp(&b.age))
                .then_with(|| b.score.cmp(&a.score))
        });

        self.mark_shared();

        self.selected = if self.rows.is_empty() {
            None
        } else {
            // Held by `(session, pane)`, not by index. An agent that the filter removes falls
            // back to the top.
            held.and_then(|(session, pane)| {
                self.rows
                    .iter()
                    .position(|r| r.session == session && r.pane == pane)
            })
            .or(Some(0))
        };
    }

    /// Decide the `:pane` suffix over the visible rows.
    ///
    /// This runs after the filter, not over the whole snapshot. The suffix thus appears exactly
    /// when the label stops identifying one row, and goes when a narrower term restores that.
    ///
    /// If two rows share a label, both rows get the suffix.
    ///
    /// The test uses `display`, not `session`, because the question is whether what you see
    /// identifies a row. Two agents in one session with different chosen names need no suffix.
    ///
    /// A pane id belongs to one session, so the suffix separates rows in one session only. Two
    /// rows in different sessions with the same chosen name both show `name:0`. The action is
    /// still correct, because each row carries its own `(session, pane)`. A fallback to the
    /// session name would change the base that `indices` points into. See [`AgentRow::label`].
    fn mark_shared(&mut self) {
        for i in 0..self.rows.len() {
            let shared = self
                .rows
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && other.display == self.rows[i].display);
            self.rows[i].shared = shared;
        }
    }

    pub fn selected_row(&self) -> Option<&AgentRow> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    /// Is there a spinner on this screen to turn?
    ///
    /// This counts the whole match set, not the visible window, which belongs to the renderer.
    /// The two differ only when every busy agent is scrolled out of view, and the cost is one
    /// redraw of what is already on the screen.
    pub fn any_busy(&self) -> bool {
        self.rows.iter().any(|row| is_busy(&row.status))
    }

    /// Move the cursor. The cursor stops at both ends, as on the other screens.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }
}

/// Where a status sorts. This only ranks; it never removes a row.
///
/// The set of statuses is open and changes with the version of Claude Code. This screen was
/// built against a release that sends `waiting`, `idle`, `busy` and `shell`, which is one more
/// than the release before it. A table that showed only known statuses would have hidden a live
/// row on the day it was written. An unknown status therefore shows its own word and sorts
/// last. The plugin cannot know whether a new word needs attention.
fn status_rank(status: &str) -> u8 {
    if status.eq_ignore_ascii_case("waiting") {
        0
    } else if status.eq_ignore_ascii_case("idle") {
        1
    } else if status.eq_ignore_ascii_case("busy") {
        2
    } else {
        3
    }
}

/// Is this the status the screen exists for?
///
/// This is the only comparison of a status word for presentation. It is safe for the reason
/// `status_rank` is safe: an unknown status still shows, but not in the accent colour.
pub fn is_waiting(status: &str) -> bool {
    status.eq_ignore_ascii_case("waiting")
}

/// Does the glyph of this status move?
///
/// The plugin asks this before it draws, to decide whether the next tick must repaint. It is a
/// question about cost, not about correctness. A wrong answer for an unknown status costs one
/// spinner that does not turn.
pub fn is_busy(status: &str) -> bool {
    status.eq_ignore_ascii_case("busy")
}

/// One turn of the busy spinner, one frame per animation tick.
///
/// These are braille characters, not the ASCII `|/-\`, because each frame is one column wide.
/// The tag column thus keeps its measured width while the spinner turns. Frames of different
/// widths would move the columns to the right ten times a second.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The glyph for a status, or `None` for a status this build does not know.
///
/// `frame` is the animation tick. Only `busy` reads it. See [`SPINNER`].
///
/// This table is decoration only. It never decides whether a row shows, never decides where a
/// row sorts, and never replaces the status word. A status added after this was written keeps
/// its word, takes the neutral glyph, and still shows. A stale table thus costs a picture, not
/// a row.
///
/// The four values are the full set of Claude Code 2.1.251, read from the binary
/// (`"busy","shell","idle","waiting"`). That makes the table complete today, which is not a
/// reason to remove the fallback: the set grew by one (`shell`) between two releases.
fn glyph(status: &str, frame: u64) -> Option<&'static str> {
    Some(match status.to_ascii_lowercase().as_str() {
        // A raised hand: the agent asked you something and stopped. This is the status the
        // screen exists for.
        "waiting" => "🙋",
        // An idle agent has finished and waits for your next instruction, so it sorts above a
        // busy one. A cup, not a 💤, because it is not asleep.
        "idle" => "☕",
        // The only glyph that moves, because a busy agent leaves this status on its own. The
        // row thus shows that without the age column.
        "busy" => SPINNER[(frame as usize) % SPINNER.len()],
        "shell" => "🐚",
        _ => return None,
    })
}

/// Unknown. It is not one of the four glyphs, so a status we cannot name cannot look like one
/// we can.
const UNKNOWN_GLYPH: &str = "🛸";

/// The glyph, then the status word in capitals, unchanged.
///
/// The word stays. You scan the column for the glyph, but the word explains a glyph you have
/// not seen before. The word is also the only part that is correct for a status released after
/// this code.
pub fn full_tag(status: &str, frame: u64) -> String {
    format!(
        "{} {}",
        glyph(status, frame).unwrap_or(UNKNOWN_GLYPH),
        status.to_uppercase()
    )
}

/// The glyph alone, for a pane too narrow for the word.
///
/// An unknown status becomes its first letter, not [`UNKNOWN_GLYPH`]. The neutral glyph would
/// draw two different unknown statuses in the same way, where `[S]` and `[N]` stay different.
/// Two unknown statuses with the same first letter still collide.
pub fn abbr_tag(status: &str, frame: u64) -> String {
    match glyph(status, frame) {
        Some(glyph) => glyph.to_string(),
        None => match status.chars().next() {
            Some(first) => format!("[{}]", first.to_uppercase()),
            None => "[?]".to_string(),
        },
    }
}

/// The JSON array from `claude-ps`, as the agents this screen can act on.
///
/// This returns the agents in zellij and the count of the agents outside it. An agent outside
/// zellij is removed, because `Enter` can do nothing for it. The count stays, so that such an
/// agent is never absent without an explanation.
///
/// A document that does not deserialise stops the parse. It shows that the tool and this build
/// disagree, which makes every row in it doubtful. A partial list would look complete.
fn parse(stdout: &str) -> Result<(Vec<Agent>, usize), String> {
    let wire: Vec<Wire> =
        serde_json::from_str(stdout).map_err(|error| format!("claude-ps: {}", error))?;

    let mut agents = Vec::new();
    let mut outside = 0;
    for row in wire {
        // Both cases are places `Enter` cannot reach: no zellij, or a pane id that is not a
        // number and cannot go to `focus-pane-id`.
        let Some(zellij) = row.zellij else {
            outside += 1;
            continue;
        };
        let Ok(pane) = zellij.pane.parse::<u32>() else {
            outside += 1;
            continue;
        };
        if zellij.session.is_empty() {
            outside += 1;
            continue;
        }
        let display = chosen_name(row.name.as_deref(), row.name_source.as_deref())
            .unwrap_or_else(|| zellij.session.clone());
        agents.push(Agent {
            session: zellij.session,
            display,
            pane,
            status: row.status.unwrap_or_default(),
            age: Duration::from_secs(row.status_age),
            cwd: row.cwd.unwrap_or_default(),
        });
    }
    Ok((agents, outside))
}

/// The name to show for an agent, or `None` to use the zellij session.
///
/// `claude-ps` reports the name and the source of the name, and the source decides this. A
/// `derived` name is the basename of the cwd plus a suffix, so it would put the cwd on the row
/// twice. Only `user` and `peer` are names that a person or another agent chose.
///
/// An unknown source is rejected, which is the opposite of what [`status_rank`] does with an
/// unknown status. Every status is a real state, so a hidden status hides a live agent. The
/// sources that carry a chosen name are a short list, but Claude Code already writes `derived`,
/// `collision`, `auto` and `hook`. A new source is thus more probably another generated name.
///
/// An absent source is accepted, because it is the state from before the key existed. An older
/// `claude-ps` must continue to work.
fn chosen_name(name: Option<&str>, source: Option<&str>) -> Option<String> {
    let name = name.map(str::trim).filter(|name| !name.is_empty())?;
    let chosen = match source {
        None => true,
        Some(source) => {
            source.eq_ignore_ascii_case("user") || source.eq_ignore_ascii_case("peer")
        },
    };
    chosen.then(|| name.to_string())
}

/// Elapsed time as a duration: `4s`, `35m`, `2h`.
///
/// This is not [`crate::sessions::format_age`], which adds `" ago"`. That form is correct for
/// the time a session started. This column says how long the status has held, and an agent that
/// has waited for thirty-five minutes is not an event from thirty-five minutes ago.
pub fn format_duration(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..=59 => format!("{}s", secs),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=604_799 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}
