//! The Claude Code agents that are running, and which one wants you.
//!
//! The rule that makes this screen work:
//!
//! > **An agent row is a zellij pane you can be standing in, and `Enter` puts you in it.**
//!
//! That is one sentence and two host calls, because zellij will not let you attach to the
//! session you are already in — `attach_with_session_name` reaches a bare
//! `panic!("You are trying to attach to the current session")` (`src/commands.rs:793`) rather
//! than declining. So a sibling agent sharing our own session is **not** a `switch_session`
//! at all; it is a plain pane focus. See [`Jump`].
//!
//! 🔴 The plugin cannot read the agents itself. Its wasi sandbox preopens only `/host`,
//! `/data`, `/cache` and `/tmp`, so neither `~/.claude/sessions/<pid>.json` nor `/proc` is
//! reachable from in here — the join between "what Claude says it is doing" and "which pane
//! that is" has to happen outside. `claude-ps` does it and prints JSON; this module only
//! deserialises, filters and orders.

use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;

use crate::sessions::Selection;

/// The command behind this screen, looked up on the server's `PATH` exactly as `zoxide` is
/// (`dirs.rs:37`). No shell, no `$HOME`: this screen's tool is an ordinary optional program,
/// and naming an install path here is what made the plugin unusable by anyone else.
///
/// If a server `PATH` genuinely lacks it, the plugin's `agents_command` configuration key
/// overrides this — that is the supported escape hatch, not a wrapper compiled in.
pub const QUERY: [&str; 1] = ["claude-ps"];

/// Marks our own `RunCommandResult`. Shares the key with the directory screen and differs in
/// the value — the plugin now issues two commands and the replies are told apart here.
pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "agents";

/// One object out of `claude-ps`, before this screen has decided anything about it.
///
/// ⚠️ Only the keys this screen reads are named, and everything else in the object is ignored
/// on purpose. That is the whole gain over the columns this replaced: the count used to be
/// checked **exactly**, so `claude-ps` gaining `started_at` was a loud failure on a screen that
/// understood every other field perfectly. A key this build has never heard of now costs
/// nothing, and only a key it *depends* on going missing is still visible.
#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    age: u64,
    /// `null` when the agent is not inside zellij. One object rather than two fields, so there
    /// is no state where a session is known and its pane is not.
    #[serde(default)]
    zellij: Option<WireZellij>,
    #[serde(default)]
    cwd: Option<String>,
    /// `null` when the producer could not find the session's transcript. Its join is a
    /// derivation rather than a proof, so an absent count is ordinary.
    #[serde(default)]
    context: Option<WireContext>,
}

#[derive(Deserialize)]
struct WireContext {
    tokens: u64,
}

#[derive(Deserialize)]
struct WireZellij {
    session: String,
    /// A string on the wire, because it comes out of an environment variable and the producer
    /// does not pretend to know it is a number. This screen needs a `u32` for
    /// `focus-pane-id`, so one that will not parse is an agent it cannot reach.
    pane: String,
}

/// What `Enter` on this row will do. **Not** a rendered tag: the status column already owns
/// that slot, and from where the user sits both of these mean the same thing — *go there*.
/// They differ only in which host call cannot be used.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// A different zellij session: attach to it and land on the pane.
    Switch,
    /// Our own session, a different pane. `switch_session` would panic the client here, so
    /// this is a pane focus instead — which is also what makes the degraded mode below safe.
    Focus,
}

/// One agent out of `claude-ps`, already known to be inside zellij.
struct Agent {
    session: String,
    pane: u32,
    status: String,
    age: Duration,
    cwd: String,
    context: Option<u64>,
}

/// One row. As on the other two screens, this *is* one match-set entry.
pub struct AgentRow {
    /// The zellij session name — what `Enter` acts on, and the **bare** string the fuzzy term
    /// was matched against.
    pub session: String,
    pub pane: u32,
    /// Claude's status, carried through **verbatim**. Never compared against a known set for
    /// the purpose of deciding whether to show it — see [`status_rank`].
    pub status: String,
    /// Time in the current status, as of the moment this row was built: what the snapshot
    /// said, plus how long ago the snapshot was taken. A duration, not a timestamp.
    ///
    /// The offset is added here rather than left to the renderer because it is the same number
    /// on every row, and a row carrying an age it is one addition short of is a row two callers
    /// have to remember to finish. See [`crate::State::agents_since`] for why a frozen list
    /// does not want a frozen clock.
    pub age: Duration,
    pub cwd: String,
    /// Tokens in context at the agent's last assistant turn, or `None` where the producer had
    /// no transcript to read.
    ///
    /// ⚠️ Tokens, never a percentage. The window size is not written to disk anywhere, so a
    /// denominator would have to come from a model-name table — and unlike an unrecognised
    /// status, which renders as itself, a wrong denominator renders as a number that looks
    /// right.
    pub context: Option<u64>,
    /// Another visible row shares this session name, so the pane has to be spelled out.
    pub shared: bool,
    pub jump: Jump,
    /// Character positions the fuzzy matcher hit **in `session`**, for highlighting.
    pub indices: Vec<usize>,
    rank: u8,
    score: i64,
    is_exact: bool,
}

impl AgentRow {
    /// `session`, or `session:pane` when the name alone no longer picks a target out.
    ///
    /// The suffix is presentation only — it is never what the term matched, because it is
    /// never something you would type.
    pub fn label(&self) -> String {
        if self.shared {
            format!("{}:{}", self.session, self.pane)
        } else {
            self.session.clone()
        }
    }
}

/// Why the agent list is not showing anything, when it is not showing anything.
///
/// The directory screen's enum, for the directory screen's reason: "still asking", "the tool
/// is not there" and "nothing is running" are three different facts, and collapsing them into
/// a blank list turns a missing binary into "I guess this feature doesn't work".
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
    /// Agents running outside zellij. `Enter` has nothing to do with them, so they are out of
    /// the **match set** — but counted here and said out loud on the note line, so that typing
    /// one's name gives a blank list *with* an explanation rather than without one.
    pub outside: usize,
    all: Vec<Agent>,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
    /// Take the tool's reply.
    ///
    /// A non-zero exit is reported rather than swallowed, for the directory screen's reason:
    /// the most likely failure — the tool is not installed — is otherwise indistinguishable
    /// from "no agents are running", and those two want opposite reactions from the user.
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
            // A document this plugin cannot read. Reported rather than dropped, for the
            // same reason a non-zero exit is: an empty list would read as "no agents running",
            // when what it means is "your `claude-ps` and your picker disagree".
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

    /// Rebuild against the term and where we are standing.
    ///
    /// `origin` is `(session, pane)` — the pane the picker was opened over. Omitting by the
    /// **pair** rather than by the session is what keeps a sibling agent in that same session
    /// a legitimate target; omitting by session would take the interesting case away.
    ///
    /// `current` is our session name on its own, which is known even when the pane is not.
    ///
    /// `since` is how long ago the snapshot was taken, added to every row's age. It is a fact
    /// about now rather than about the agents, which is why it arrives per rebuild rather than
    /// per ingest — the snapshot is frozen and this is what keeps the ages off it from being.
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
            // The agent we are sitting in leaves the match set here, at the source — the same
            // discipline the session screen applies to the current session, so that the
            // rendered list *is* the match set and indices cannot drift.
            if origin == Some((agent.session.as_str(), agent.pane)) {
                continue;
            }
            let (score, indices, is_exact) = if term.is_empty() {
                (0, Vec::new(), false)
            } else {
                // Matched against the **bare** session name. The `:pane` suffix is decided
                // below, after filtering, and is not part of what anyone would type.
                match matcher.fuzzy_indices(&agent.session, term) {
                    Some((score, indices)) => (score, indices, agent.session == term),
                    None => continue,
                }
            };
            self.rows.push(AgentRow {
                session: agent.session.clone(),
                pane: agent.pane,
                status: agent.status.clone(),
                age: agent.age + since,
                cwd: agent.cwd.clone(),
                context: agent.context,
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

        // Attention first, and `rank` sits **above** both `age` and `score` for the session
        // screen's reason: it keeps the boundary between "wants you" and "does not" a fixed
        // landmark while you narrow, instead of a line that shuffles on every keystroke.
        //
        // 🔴 Safe only because the snapshot is frozen for the life of the screen. Under a poll
        // this ordering would move rows under the cursor as agents changed status. The ages
        // below *do* move without a re-fetch, and are still safe here: `since` is one number
        // added to every row, and a uniform offset cannot flip a comparison between two of them.
        self.rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| a.rank.cmp(&b.rank))
                // Most recent first, and `age` sits **above** `score`: within one status the
                // agent that changed into it a moment ago is the one you were just working
                // with, and that is a stabler thing to steer by than a fuzzy score that
                // reshuffles the block on every keystroke.
                .then_with(|| a.age.cmp(&b.age))
                .then_with(|| b.score.cmp(&a.score))
        });

        self.mark_shared();

        self.selected = if self.rows.is_empty() {
            None
        } else {
            // Held by identity — `(session, pane)` — not by index: the row may have moved, and
            // an agent that fell out of the filter falls back to the top.
            held.and_then(|(session, pane)| {
                self.rows
                    .iter()
                    .position(|r| r.session == session && r.pane == pane)
            })
            .or(Some(0))
        };
    }

    /// Decide the `:pane` suffix over the rows that are actually **visible**.
    ///
    /// Computed after filtering rather than over the whole snapshot, which is what makes it
    /// mean what it says: the suffix appears exactly when the session name has stopped picking
    /// one row out of the list, and goes away again when narrowing restores that.
    ///
    /// When a session is shared, *every* one of its rows is suffixed — "the first one is bare"
    /// is not a rule anyone could read off the screen.
    fn mark_shared(&mut self) {
        for i in 0..self.rows.len() {
            let shared = self
                .rows
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && other.session == self.rows[i].session);
            self.rows[i].shared = shared;
        }
    }

    pub fn selected_row(&self) -> Option<&AgentRow> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    /// Is there a spinner on this screen to turn?
    ///
    /// Over the whole match set rather than the scrolled-to window, which the renderer owns and
    /// this side cannot see. The two differ only when every busy agent has been scrolled past,
    /// and the cost of being wrong there is a redraw that paints what was already on screen —
    /// cheaper than teaching this module how the viewport works to save it.
    pub fn any_busy(&self) -> bool {
        self.rows.iter().any(|row| is_busy(&row.status))
    }

    /// Move the cursor. **No wrap**, as on both other screens.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }
}

/// Where a status sorts. **Ranking only** — nothing is dropped for being unrecognised.
///
/// 🔴 The status vocabulary is **open** and moves with Claude Code's version: this screen was
/// built against a release that emits `waiting`, `idle`, `busy` *and* `shell`, one more than
/// the release before it. A match table that rendered only the ones it knew would have
/// silently dropped a live row on the day it was written, so unknown statuses render as
/// themselves and rank **last** — the plugin cannot know whether a word it has never seen
/// wants attention, and guessing "probably urgent" would let a future release quietly promote
/// noise to the top of the list.
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

/// Is this the status the whole screen exists for?
///
/// The one place a status word is compared against a literal for **presentation**, and it is
/// safe for the same reason `status_rank` is: an unrecognised status still renders, just not
/// in the accent colour.
pub fn is_waiting(status: &str) -> bool {
    status.eq_ignore_ascii_case("waiting")
}

/// Is this status the one whose glyph moves?
///
/// Asked by the plugin *before* rendering, to decide whether the next animation tick has
/// anything to repaint — so it is a question about cost, not about correctness. Getting it
/// wrong on a status word this build has never seen costs a still spinner nobody is looking at,
/// which is why it is the same literal comparison [`is_waiting`] is and not a bigger idea.
pub fn is_busy(status: &str) -> bool {
    status.eq_ignore_ascii_case("busy")
}

/// One turn of the busy spinner, a frame per animation tick.
///
/// Braille rather than the ASCII `|/-\`: every frame here is **one column wide**, so the tag
/// column keeps the width it measured as the spinner turns. A cycle whose frames disagreed
/// about their width would resize the column ten times a second and shove the two columns to
/// its right back and forth for the whole time an agent was busy.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The glyph for a status, or `None` for one this build has never heard of.
///
/// `frame` is the animation tick, and only `busy` reads it — see [`SPINNER`]. Every other
/// status returns the same glyph on every frame, which is what keeps this a pure function of
/// the row rather than a thing that has to be redrawn.
///
/// ⚠️ This is the lookup table the rest of this module refuses to have, and it is allowed to
/// exist only because it is **decoration and nothing else**. It never decides whether a row is
/// shown, never decides where one sorts, and never replaces the status word. A status invented
/// after this was written keeps its own word, takes the neutral glyph, and is rendered as
/// itself — so the table going stale costs a picture, never a row.
///
/// The four below are the *whole* vocabulary of Claude Code 2.1.251, read out of the binary
/// itself (`"busy","shell","idle","waiting"`) rather than inferred from what happened to be
/// running. 🔴 That makes the table complete **today**, and is not a reason to drop the
/// fallback: the set grew by one — `shell` — between two releases already.
fn glyph(status: &str, frame: u64) -> Option<&'static str> {
    Some(match status.to_ascii_lowercase().as_str() {
        // Someone with their hand up: the one status this whole screen exists to surface, and
        // literally what the agent is doing — it has asked you something and stopped.
        "waiting" => "🙋",
        // Not asleep. An idle agent has *finished* and is waiting on your next instruction,
        // which is why it outranks a busy one in the sort — so it gets a cup rather than the
        // "do not disturb" of a 💤.
        "idle" => "☕",
        // The one glyph that moves, because it is the one status that is *going* somewhere: a
        // busy agent will leave this state on its own, and the spinner is the row saying so
        // without the user having to watch the age column to find out. It replaces the static
        // 🧠 that was here — a picture of thinking, where this is the thinking itself.
        "busy" => SPINNER[(frame as usize) % SPINNER.len()],
        // It is a shell. There was never going to be another choice.
        "shell" => "🐚",
        _ => return None,
    })
}

/// Unidentified, and deliberately not one of the four — a status we cannot name must not be
/// able to pass itself off as one we can. Nothing else on this screen is a vehicle.
const UNKNOWN_GLYPH: &str = "🛸";

/// `🔴 WAITING` — the glyph, then the status word uppercased verbatim.
///
/// The word stays. The glyph is what the eye scans a column for; the word is what tells you
/// what a glyph you have never seen before actually means, and it is the only half that is
/// guaranteed to be true of a status released after this code was.
pub fn full_tag(status: &str, frame: u64) -> String {
    format!(
        "{} {}",
        glyph(status, frame).unwrap_or(UNKNOWN_GLYPH),
        status.to_uppercase()
    )
}

/// `🔴` alone, for a pane too narrow to carry the word as well.
///
/// An unrecognised status degrades to its first letter rather than to [`UNKNOWN_GLYPH`]: the
/// neutral glyph on every unknown row would render two *different* unknown statuses
/// identically, where `[S]` and `[N]` at least stay distinct. Collisions between two unknowns
/// sharing a letter are tolerated, as they were before there were glyphs at all.
pub fn abbr_tag(status: &str, frame: u64) -> String {
    match glyph(status, frame) {
        Some(glyph) => glyph.to_string(),
        None => match status.chars().next() {
            Some(first) => format!("[{}]", first.to_uppercase()),
            None => "[?]".to_string(),
        },
    }
}

/// The JSON array `claude-ps` prints, as the agents this screen can act on.
///
/// Returns the agents inside zellij, and the **count** of those outside it. Agents outside
/// zellij are dropped here rather than rendered with a placeholder session: `Enter` could do
/// nothing for them, and rows `Enter` cannot act on do not belong on a screen whose only job
/// is reachability. The count survives so they are never silently invisible.
///
/// The one thing that is **not** dropped is a document that will not deserialise. That is not a
/// row this screen cannot use, it is evidence that the tool and this build no longer agree, and
/// every row in it is suspect for the same reason — so it ends the parse rather than rendering
/// a partial list that looks complete.
fn parse(stdout: &str) -> Result<(Vec<Agent>, usize), String> {
    let wire: Vec<Wire> =
        serde_json::from_str(stdout).map_err(|error| format!("claude-ps: {}", error))?;

    let mut agents = Vec::new();
    let mut outside = 0;
    for row in wire {
        // Both arms are "not somewhere `Enter` can put you": no zellij at all, or a pane id
        // that is not a number and so cannot be handed to `focus-pane-id`.
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
        agents.push(Agent {
            session: zellij.session,
            pane,
            status: row.status.unwrap_or_default(),
            age: Duration::from_secs(row.age),
            cwd: row.cwd.unwrap_or_default(),
            context: row.context.map(|c| c.tokens),
        });
    }
    Ok((agents, outside))
}

/// A token count at a glance: `950`, `188k`, `1.2M`.
///
/// Rounded rather than truncated — 187,953 tokens is nearer `188k` than `187k`, and three
/// significant figures is all this column is for. A count that rounds up to a million reads as
/// `1.0M` rather than as `1000k`.
pub fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        return format!("{}", tokens);
    }
    let tenths_of_m = (tokens + 50_000) / 100_000;
    if tenths_of_m >= 10 {
        return format!("{}.{}M", tenths_of_m / 10, tenths_of_m % 10);
    }
    format!("{}k", (tokens + 500) / 1_000)
}

/// Compact elapsed time as a **duration** — `4s`, `35m`, `2h`.
///
/// Deliberately not [`crate::sessions::format_age`]: that one spells the same magnitudes with
/// a trailing `" ago"`, which is right for "this session was created 2h ago" and wrong here.
/// This column answers *how long has it been like this*, and an agent that has been waiting on
/// you for thirty-five minutes is not an event that happened thirty-five minutes ago.
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
