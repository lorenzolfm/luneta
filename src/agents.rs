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
//! that is" has to happen outside. `claude-agents` does it and prints TSV; this module only
//! parses, filters and orders.

use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::sessions::Selection;

/// The command behind this screen.
///
/// 🔴 Wrapped in `sh -c` for one reason: the zellij **server's** `PATH` does not contain
/// `~/.local/bin`, so naming the tool bare finds nothing. `$HOME` is expanded by the shell
/// rather than hardcoded, and `exec` keeps it to one process rather than two.
pub const QUERY: [&str; 3] = ["sh", "-c", "exec \"$HOME/.local/bin/claude-agents\""];

/// Marks our own `RunCommandResult`. Shares the key with the directory screen and differs in
/// the value — the plugin now issues two commands and the replies are told apart here.
pub const CONTEXT_KEY: &str = "zj-picker";
pub const CONTEXT_VALUE: &str = "agents";

/// The number of tab-separated fields `claude-agents` emits:
/// `status age session pane name pid session_id cwd`.
///
/// `cwd` is last so that it — the one field that can plausibly contain anything — is taken as
/// the whole remainder of the line rather than as a field with a terminator.
const FIELDS: usize = 8;

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

/// One agent out of `claude-agents`, already known to be inside zellij.
struct Agent {
    session: String,
    pane: u32,
    status: String,
    age: Duration,
    cwd: String,
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
    /// Time in the current status. A duration, not a timestamp.
    pub age: Duration,
    pub cwd: String,
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
                "claude-agents is not available".to_string()
            } else {
                format!("claude-agents: {}", reason)
            });
            self.all.clear();
            self.outside = 0;
            return;
        }
        let (agents, outside) = parse(&String::from_utf8_lossy(stdout));
        self.all = agents;
        self.outside = outside;
        self.status = Status::Ready;
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
    pub fn rebuild(
        &mut self,
        term: &str,
        current: Option<&str>,
        origin: Option<(&str, u32)>,
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
                age: agent.age,
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

        // Attention first, and `rank` sits **above** `score` for the session screen's reason:
        // it keeps the boundary between "wants you" and "does not" a fixed landmark while you
        // narrow, instead of a line that shuffles on every keystroke.
        //
        // 🔴 Safe only because the snapshot is frozen for the life of the screen. Under a poll
        // this ordering would move rows under the cursor as agents changed status.
        self.rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| b.score.cmp(&a.score))
                // Longest-in-status first: of two agents waiting on you, the one that has been
                // waiting longer is the one you have kept waiting.
                .then_with(|| b.age.cmp(&a.age))
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

/// The glyph for a status, or `None` for one this build has never heard of.
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
fn glyph(status: &str) -> Option<&'static str> {
    Some(match status.to_ascii_lowercase().as_str() {
        // Someone with their hand up: the one status this whole screen exists to surface, and
        // literally what the agent is doing — it has asked you something and stopped.
        "waiting" => "🙋",
        // Not asleep. An idle agent has *finished* and is waiting on your next instruction,
        // which is why it outranks a busy one in the sort — so it gets a cup rather than the
        // "do not disturb" of a 💤.
        "idle" => "☕",
        // Thinking, which is the one thing this particular kind of process is doing when busy.
        "busy" => "🧠",
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
pub fn full_tag(status: &str) -> String {
    format!(
        "{} {}",
        glyph(status).unwrap_or(UNKNOWN_GLYPH),
        status.to_uppercase()
    )
}

/// `🔴` alone, for a pane too narrow to carry the word as well.
///
/// An unrecognised status degrades to its first letter rather than to [`UNKNOWN_GLYPH`]: the
/// neutral glyph on every unknown row would render two *different* unknown statuses
/// identically, where `[S]` and `[N]` at least stay distinct. Collisions between two unknowns
/// sharing a letter are tolerated, as they were before there were glyphs at all.
pub fn abbr_tag(status: &str) -> String {
    match glyph(status) {
        Some(glyph) => glyph.to_string(),
        None => match status.chars().next() {
            Some(first) => format!("[{}]", first.to_uppercase()),
            None => "[?]".to_string(),
        },
    }
}

/// `status age session pane name pid session_id cwd`, tab separated.
///
/// Returns the agents inside zellij, and the **count** of those outside it. Agents outside
/// zellij are dropped here rather than rendered with a placeholder session: `Enter` could do
/// nothing for them, and rows `Enter` cannot act on do not belong on a screen whose only job
/// is reachability. The count survives so they are never silently invisible.
fn parse(stdout: &str) -> (Vec<Agent>, usize) {
    let mut agents = Vec::new();
    let mut outside = 0;
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // `splitn` so that the final field — the cwd — is the rest of the line rather than a
        // field in its own right.
        let fields: Vec<&str> = line.splitn(FIELDS, '\t').collect();
        if fields.len() < FIELDS {
            continue;
        }
        let (status, age, session, pane, cwd) =
            (fields[0], fields[1], fields[2], fields[3], fields[7]);
        // The tool marks an agent that is not in zellij with `-` in both join columns.
        let Ok(pane) = pane.parse::<u32>() else {
            outside += 1;
            continue;
        };
        if session == "-" || session.is_empty() {
            outside += 1;
            continue;
        }
        let Ok(age) = age.parse::<u64>() else {
            continue;
        };
        agents.push(Agent {
            session: session.to_string(),
            pane,
            status: status.to_string(),
            age: Duration::from_secs(age),
            cwd: cwd.to_string(),
        });
    }
    (agents, outside)
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
