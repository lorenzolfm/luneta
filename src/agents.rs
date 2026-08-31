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

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;

use crate::elapsed::{Age, Held};
use crate::fetch::Fetch;
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
    /// The word Claude reports, or `null`. Wide here and narrow after [`Status::parse`].
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
pub struct Agent {
    session: String,
    /// What the row is called: the chosen name, or the zellij session if there is none.
    /// [`parse`] decides this once, because the fuzzy term matches against it and the hit
    /// positions are offsets into it. A later change would paint the highlight on a different
    /// string.
    display: String,
    pane: u32,
    status: Status,
    age: Held,
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
    /// What the agent is doing, including when nothing was reported. See [`Status`].
    pub status: Status,
    /// Time in the current status when this row was built: the value in the snapshot plus the
    /// age of the snapshot. This is a duration, not a time.
    ///
    /// The offset is added here because it is the same number on every row. See
    /// [`crate::State::agents_since`].
    pub age: Held,
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

/// A status word this build cannot name, kept verbatim and never empty.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusWord {
    /// The first character, which is the narrow-column form and proves the word is not empty.
    initial: char,
    word: String,
}

impl StatusWord {
    /// The trimmed word, or `None` if there is nothing left of it. The `?` that reads the
    /// first character *is* the emptiness check, so there is no invariant to remember beside
    /// it. Private, because [`Status::parse`] is the only boundary that may build one.
    fn new(word: &str) -> Option<Self> {
        Some(StatusWord {
            initial: word.chars().next()?,
            word: word.to_string(),
        })
    }

    /// The first character, for a column too narrow for the word.
    pub fn initial(&self) -> char {
        self.initial
    }

    /// The word as it arrived, in the case it arrived in.
    ///
    /// A named method and not a `Deref`, which would graft the whole of `str` onto this type
    /// to save one call.
    pub fn as_str(&self) -> &str {
        &self.word
    }
}

/// What one agent is doing, as Claude Code reports it.
///
/// The vocabulary is open and grows with the version of Claude Code, so a word from a later
/// release is [`Status::Unknown`] rather than an error: it keeps its own text, sorts last, and
/// still shows. Nothing here removes a row.
///
/// [`Status::Unreported`] is the sixth member, and not an `Option` around the other five. It is
/// not the wire's `null`: [`Status::parse`] reaches it from a `null`, from an absent key, from
/// an empty word and from a word that is only whitespace. It ranks, reads and draws like any
/// other variant, so every function here is total and no caller carries an `Option`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// The agent asked you something and stopped. The status this screen exists for.
    Waiting,
    /// Finished, and waiting for your next instruction.
    Idle,
    /// Working, and will leave this status on its own.
    Busy,
    /// A shell in the pane, not a turn of the agent.
    Shell,
    /// A word this build cannot name, carried verbatim for the screen.
    Unknown(StatusWord),
    /// `claude-ps` reported nothing. Not a word, and never drawn as one.
    Unreported,
}

impl Status {
    /// The reported word. Nothing reported and nothing left after the trim are one fact, and
    /// they reach [`Status::Unreported`] by one path.
    pub fn parse(status: Option<&str>) -> Self {
        let Some(word) = status.map(str::trim) else {
            return Status::Unreported;
        };
        match word.to_ascii_lowercase().as_str() {
            "waiting" => Status::Waiting,
            "idle" => Status::Idle,
            "busy" => Status::Busy,
            "shell" => Status::Shell,
            // `StatusWord` has no empty value, so a blank word falls through to `Unreported`
            // rather than becoming an unknown status with nothing to show for itself.
            _ => StatusWord::new(word).map_or(Status::Unreported, Status::Unknown),
        }
    }

    /// The word for the status column. Every status has one, including the one nobody sent.
    pub fn word(&self) -> &str {
        match self {
            Status::Waiting => "waiting",
            Status::Idle => "idle",
            Status::Busy => "busy",
            Status::Shell => "shell",
            Status::Unknown(word) => word.as_str(),
            Status::Unreported => "unreported",
        }
    }

    /// Is this the status the screen exists for? The only status in the accent colour.
    pub fn is_waiting(&self) -> bool {
        matches!(self, Status::Waiting)
    }

    /// Does the glyph of this status move? Asked before a draw, to decide whether the next
    /// tick must repaint. A question about cost, not about correctness.
    pub fn is_busy(&self) -> bool {
        matches!(self, Status::Busy)
    }
}

#[derive(Default)]
pub struct AgentSet {
    /// Why the list is empty, when it is, and the agents when it is not. See [`Fetch`].
    pub status: Fetch<Vec<Agent>>,
    pub rows: Vec<AgentRow>,
    pub selected: Option<usize>,
    pub asking: bool,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
    /// Take the reply from the tool.
    ///
    /// An exit other than 0 is reported, not discarded. The most probable failure is that the
    /// tool is not installed, which otherwise looks the same as an empty list.
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.fail(if reason.is_empty() {
                "claude-ps is not available".to_string()
            } else {
                format!("claude-ps: {}", reason)
            });
            return;
        }
        match parse(&String::from_utf8_lossy(stdout)) {
            Ok(agents) => {
                self.asking = false;
                self.status = Fetch::Ready(agents);
            },
            // A document this plugin cannot read. It is reported, not discarded: an empty
            // list would mean that no agents run, when it means that `claude-ps` and the
            // picker disagree.
            Err(reason) => self.fail(reason),
        }
    }

    /// The one way to a failure, from here and from the server. It takes the agents with it,
    /// because [`Fetch::Failed`] has nowhere to keep them.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Fetch::Failed(reason.into());
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
        since: Age,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_row().map(|r| (r.session.clone(), r.pane)),
        };
        self.rows.clear();

        // Only a reply has agents to filter. The other two states have none, and an empty list
        // is what the screen draws for them.
        if let Fetch::Ready(all) = &self.status {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));

            for agent in all {
                // The agent we are in is removed here, as the session screen removes the
                // current session, so that the rendered list stays equal to the match set.
                if origin == Some((agent.session.as_str(), agent.pane)) {
                    continue;
                }
                let (score, indices, is_exact) = if term.is_empty() {
                    (0, Vec::new(), false)
                } else {
                    // Matched against the bare label. You type what you see, so a row that
                    // shows a chosen name must answer to that name. The `:pane` suffix is
                    // decided after this filter, and nobody types it.
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
                    age: agent.age.grown_by(since),
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
        self.rows.iter().any(|row| row.status.is_busy())
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
/// A word this build cannot name sorts below the four it can, and an unreported status below
/// that. The plugin cannot know whether a new word needs attention, so it cannot rank it above
/// one it understands.
fn status_rank(status: &Status) -> u8 {
    match status {
        Status::Waiting => 0,
        Status::Idle => 1,
        Status::Busy => 2,
        Status::Shell => 3,
        Status::Unknown(_) => 4,
        Status::Unreported => 5,
    }
}

/// One turn of the busy spinner, one frame per animation tick.
///
/// These are braille characters, not the ASCII `|/-\`, because each frame is one column wide.
/// The tag column thus keeps its measured width while the spinner turns. Frames of different
/// widths would move the columns to the right ten times a second.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The glyph for a status. `frame` is the animation tick, which only `busy` reads.
///
/// Decoration only: it never decides whether a row shows, never decides where a row sorts, and
/// never replaces the status word.
fn glyph(status: &Status, frame: u64) -> &'static str {
    match status {
        // A raised hand: the agent asked you something and stopped. This is the status the
        // screen exists for.
        Status::Waiting => "🙋",
        // An idle agent has finished and waits for your next instruction, so it sorts above a
        // busy one. A cup, not a 💤, because it is not asleep.
        Status::Idle => "☕",
        // The only glyph that moves, because a busy agent leaves this status on its own. The
        // row thus shows that without the age column.
        Status::Busy => SPINNER[(frame as usize) % SPINNER.len()],
        Status::Shell => "🐚",
        // Not one of the four, so a word we cannot name cannot look like one we can.
        Status::Unknown(_) => "🛸",
        // No word arrived at all, which is a different fact from a word we cannot name. Two
        // facts, so two glyphs.
        Status::Unreported => "❔",
    }
}

/// The glyph, then the status word in capitals.
///
/// The word stays: you scan the column for the glyph, but the word explains a glyph you have
/// not seen before, and it is the only part that is correct for a status released after this
/// code. [`Status::Unreported`] has a glyph and a word like the rest, so there is no case to
/// make an exception for and no branch here.
pub fn full_tag(status: &Status, frame: u64) -> String {
    format!("{} {}", glyph(status, frame), status.word().to_uppercase())
}

/// The glyph alone, for a pane too narrow for the word.
///
/// An unnamed word becomes its first letter, not the neutral glyph, which would draw two
/// different unknown statuses in the same way where `[S]` and `[N]` stay different. Two unknown
/// statuses with the same first letter still collide.
pub fn abbr_tag(status: &Status, frame: u64) -> String {
    match status {
        Status::Unknown(word) => format!("[{}]", word.initial().to_uppercase()),
        _ => glyph(status, frame).to_string(),
    }
}

/// The JSON array from `claude-ps`, as the agents this screen can act on.
///
/// An agent this screen cannot address is dropped, without a count and without a note. The
/// rule of the screen is that a row is a pane `Enter` puts you in, so an agent with no pane to
/// go to is not a row that is missing — it is not a row.
///
/// A document that does not deserialise stops the parse. It shows that the tool and this build
/// disagree, which makes every row in it doubtful. A partial list would look complete.
fn parse(stdout: &str) -> Result<Vec<Agent>, String> {
    let wire: Vec<Wire> =
        serde_json::from_str(stdout).map_err(|error| format!("claude-ps: {}", error))?;

    let mut agents = Vec::new();
    for row in wire {
        // Three places `Enter` cannot reach: no zellij, a pane id that is not a number and
        // cannot go to `focus-pane-id`, or no session to attach to.
        let Some(zellij) = row.zellij else { continue };
        let Ok(pane) = zellij.pane.parse::<u32>() else { continue };
        if zellij.session.is_empty() {
            continue;
        }
        let display = chosen_name(row.name.as_deref(), row.name_source.as_deref())
            .unwrap_or_else(|| zellij.session.clone());
        agents.push(Agent {
            session: zellij.session,
            display,
            pane,
            status: Status::parse(row.status.as_deref()),
            age: Held::from_secs(row.status_age),
            cwd: row.cwd.unwrap_or_default(),
        });
    }
    Ok(agents)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> Status {
        let agents = parse(json).expect("the document should parse");
        agents.into_iter().next().expect("one agent").status
    }

    fn one(status: &str) -> String {
        format!(
            r#"[{{"status":{},"status_age":4,"zellij":{{"session":"s","pane":"0"}}}}]"#,
            status
        )
    }

    /// A failure takes the agents with it. The rows of the last reply cannot outlive it,
    /// because [`Fetch::Failed`] has nowhere to keep them.
    #[test]
    fn a_failure_leaves_no_agents_behind() {
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), one(r#""idle""#).as_bytes(), b"");
        agents.rebuild("", None, None, Age::ZERO, Selection::SnapToTop);
        assert_eq!(agents.rows.len(), 1);

        agents.fail("claude-ps: gone");
        agents.rebuild("", None, None, Age::ZERO, Selection::SnapToTop);
        assert!(agents.rows.is_empty());
        assert!(agents.selected.is_none());
    }

    /// An agent this screen cannot address is not a row and is not counted. There is no pane
    /// to put you in, so there is nothing to say about it.
    #[test]
    fn an_agent_this_screen_cannot_address_is_dropped() {
        let agents = parse(
            r#"[{"status":"idle","status_age":4},
                {"status":"idle","status_age":4,"zellij":{"session":"s","pane":"nope"}},
                {"status":"idle","status_age":4,"zellij":{"session":"","pane":"0"}},
                {"status":"idle","status_age":4,"zellij":{"session":"s","pane":"0"}}]"#,
        )
        .expect("the document should parse");
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].session, "s");
    }

    /// A `null` status is unreported. It was `""` before, which took the glyph of a word
    /// Claude had sent and left the tag beside it empty.
    #[test]
    fn a_null_status_is_unreported_and_not_an_empty_word() {
        assert_eq!(status(&one("null")), Status::Unreported);
        assert_eq!(
            status(r#"[{"status_age":4,"zellij":{"session":"s","pane":"0"}}]"#),
            Status::Unreported
        );
    }

    /// Nothing reported and nothing left after the trim are one fact, so both are one value.
    #[test]
    fn a_blank_status_is_unreported() {
        assert_eq!(status(&one(r#""""#)), Status::Unreported);
        assert_eq!(status(&one(r#"" \t ""#)), Status::Unreported);
    }

    /// The four words this build knows, whatever case they arrive in.
    #[test]
    fn a_known_status_is_named_without_regard_to_case() {
        assert_eq!(status(&one(r#""waiting""#)), Status::Waiting);
        assert_eq!(status(&one(r#""IDLE""#)), Status::Idle);
        assert_eq!(status(&one(r#""Busy""#)), Status::Busy);
        assert_eq!(status(&one(r#""shell""#)), Status::Shell);
    }

    /// A word from a release after this one keeps its own text, in the case it arrived in. The
    /// word is the only part of the tag that is correct for it.
    #[test]
    fn an_unnamed_status_keeps_its_word() {
        let status = status(&one(r#"" Compacting ""#));
        assert_eq!(status.word(), "Compacting");
        assert_eq!(full_tag(&status, 0), "🛸 COMPACTING");
        assert_eq!(abbr_tag(&status, 0), "[C]");
    }

    /// Attention first, then the words this build knows, then one it does not, then silence.
    #[test]
    fn a_status_sorts_by_how_much_it_wants_you() {
        let ranks: Vec<u8> = ["waiting", "idle", "busy", "shell", "compacting"]
            .iter()
            .map(|word| status_rank(&Status::parse(Some(word))))
            .collect();
        assert_eq!(ranks, [0, 1, 2, 3, 4]);
        assert!(status_rank(&Status::Unreported) > 4);
    }

    /// An unreported status is not an unnamed one. `❔` says nothing arrived; `🛸` says a word
    /// arrived that this build cannot name. Neither is asked for by name.
    #[test]
    fn an_unreported_status_reads_and_draws_as_itself() {
        assert_eq!(Status::Unreported.word(), "unreported");
        assert_eq!(full_tag(&Status::Unreported, 0), "❔ UNREPORTED");
        assert_eq!(abbr_tag(&Status::Unreported, 0), "❔");
        assert_ne!(
            full_tag(&Status::Unreported, 0),
            full_tag(&Status::parse(Some("compacting")), 0)
        );
        assert!(!Status::Unreported.is_waiting());
        assert!(!Status::Unreported.is_busy());
    }

    /// Only `busy` turns the spinner, and only `waiting` takes the accent colour.
    #[test]
    fn only_two_statuses_are_asked_about_by_name() {
        let busy = Status::parse(Some("busy"));
        assert!(busy.is_busy() && !busy.is_waiting());
        let waiting = Status::parse(Some("waiting"));
        assert!(waiting.is_waiting() && !waiting.is_busy());
        assert_ne!(full_tag(&busy, 0), full_tag(&busy, 1));
    }
}
