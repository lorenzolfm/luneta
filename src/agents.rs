use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;

use crate::cursor::Cursor;
use crate::elapsed::{Age, Held};
use crate::fetch::Fetch;
use crate::sessions::Selection;

pub const QUERY: [&str; 1] = ["claude-ps"];

pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "agents";

#[derive(Deserialize)]
struct Wire {
    #[serde(default)]
    status: Option<String>,
    #[serde(alias = "age")]
    status_age: u64,
    #[serde(default)]
    zellij: Option<WireZellij>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    name_source: Option<String>,
}

#[derive(Deserialize)]
struct WireZellij {
    session: String,
    pane: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    Switch,
    Focus,
}

pub struct Agent {
    session: String,
    display: String,
    pane: u32,
    status: Status,
    age: Held,
    cwd: String,
}

pub struct AgentRow {
    pub session: String,
    pub display: String,
    pub pane: u32,
    pub status: Status,
    pub age: Held,
    pub cwd: String,
    pub shared: bool,
    pub jump: Jump,
    pub indices: Vec<usize>,
    rank: u8,
    score: i64,
    is_exact: bool,
}

impl AgentRow {
    pub fn label(&self) -> String {
        if self.shared {
            format!("{}:{}", self.display, self.pane)
        } else {
            self.display.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusWord {
    initial: char,
    word: String,
}

impl StatusWord {
    fn new(word: &str) -> Option<Self> {
        Some(StatusWord {
            initial: word.chars().next()?,
            word: word.to_string(),
        })
    }

    pub fn initial(&self) -> char {
        self.initial
    }

    pub fn as_str(&self) -> &str {
        &self.word
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Idle,
    Busy,
    Shell,
    Unknown(StatusWord),
    Unreported,
}

impl Status {
    pub fn parse(status: Option<&str>) -> Self {
        let Some(word) = status.map(str::trim) else {
            return Status::Unreported;
        };
        match word.to_ascii_lowercase().as_str() {
            "waiting" => Status::Waiting,
            "idle" => Status::Idle,
            "busy" => Status::Busy,
            "shell" => Status::Shell,
            _ => StatusWord::new(word).map_or(Status::Unreported, Status::Unknown),
        }
    }

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

    pub fn is_waiting(&self) -> bool {
        matches!(self, Status::Waiting)
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, Status::Busy)
    }
}

#[derive(Default)]
pub struct AgentSet {
    pub status: Fetch<Vec<Agent>>,
    pub rows: Cursor<AgentRow>,
    pub asking: bool,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
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
            Err(reason) => self.fail(reason),
        }
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Fetch::Failed(reason.into());
    }

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
        let mut rows: Vec<AgentRow> = Vec::new();

        if let Fetch::Ready(all) = &self.status {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));

            for agent in all {
                if origin == Some((agent.session.as_str(), agent.pane)) {
                    continue;
                }
                let (score, indices, is_exact) = if term.is_empty() {
                    (0, Vec::new(), false)
                } else {
                    match matcher.fuzzy_indices(&agent.display, term) {
                        Some((score, indices)) => (score, indices, agent.display == term),
                        None => continue,
                    }
                };
                rows.push(AgentRow {
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

        rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| a.age.cmp(&b.age))
                .then_with(|| b.score.cmp(&a.score))
        });

        mark_shared(&mut rows);

        self.rows.replace(rows, |row| {
            held.as_ref()
                .is_some_and(|(session, pane)| row.session == *session && row.pane == *pane)
        });
    }

    pub fn selected_row(&self) -> Option<&AgentRow> {
        self.rows.selected_row()
    }

    pub fn any_busy(&self) -> bool {
        self.rows.iter().any(|row| row.status.is_busy())
    }
}

fn mark_shared(rows: &mut [AgentRow]) {
    for i in 0..rows.len() {
        let shared = rows
            .iter()
            .enumerate()
            .any(|(j, other)| j != i && other.display == rows[i].display);
        rows[i].shared = shared;
    }
}

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

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn glyph(status: &Status, frame: u64) -> &'static str {
    match status {
        Status::Waiting => "🙋",
        Status::Idle => "☕",
        Status::Busy => SPINNER[(frame as usize) % SPINNER.len()],
        Status::Shell => "🐚",
        Status::Unknown(_) => "🛸",
        Status::Unreported => "❔",
    }
}

pub fn full_tag(status: &Status, frame: u64) -> String {
    format!("{} {}", glyph(status, frame), status.word().to_uppercase())
}

pub fn abbr_tag(status: &Status, frame: u64) -> String {
    match status {
        Status::Unknown(word) => format!("[{}]", word.initial().to_uppercase()),
        _ => glyph(status, frame).to_string(),
    }
}

fn parse(stdout: &str) -> Result<Vec<Agent>, String> {
    let wire: Vec<Wire> =
        serde_json::from_str(stdout).map_err(|error| format!("claude-ps: {}", error))?;

    let mut agents = Vec::new();
    for row in wire {
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

    #[test]
    fn a_failure_leaves_no_agents_behind() {
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), one(r#""idle""#).as_bytes(), b"");
        agents.rebuild("", None, None, Age::ZERO, Selection::SnapToTop);
        assert_eq!(agents.rows.len(), 1);

        agents.fail("claude-ps: gone");
        agents.rebuild("", None, None, Age::ZERO, Selection::SnapToTop);
        assert!(agents.rows.is_empty());
        assert!(agents.rows.selected().is_none());
    }

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

    #[test]
    fn a_null_status_is_unreported_and_not_an_empty_word() {
        assert_eq!(status(&one("null")), Status::Unreported);
        assert_eq!(
            status(r#"[{"status_age":4,"zellij":{"session":"s","pane":"0"}}]"#),
            Status::Unreported
        );
    }

    #[test]
    fn a_blank_status_is_unreported() {
        assert_eq!(status(&one(r#""""#)), Status::Unreported);
        assert_eq!(status(&one(r#"" \t ""#)), Status::Unreported);
    }

    #[test]
    fn a_known_status_is_named_without_regard_to_case() {
        assert_eq!(status(&one(r#""waiting""#)), Status::Waiting);
        assert_eq!(status(&one(r#""IDLE""#)), Status::Idle);
        assert_eq!(status(&one(r#""Busy""#)), Status::Busy);
        assert_eq!(status(&one(r#""shell""#)), Status::Shell);
    }

    #[test]
    fn an_unnamed_status_keeps_its_word() {
        let status = status(&one(r#"" Compacting ""#));
        assert_eq!(status.word(), "Compacting");
        assert_eq!(full_tag(&status, 0), "🛸 COMPACTING");
        assert_eq!(abbr_tag(&status, 0), "[C]");
    }

    #[test]
    fn a_status_sorts_by_how_much_it_wants_you() {
        let ranks: Vec<u8> = ["waiting", "idle", "busy", "shell", "compacting"]
            .iter()
            .map(|word| status_rank(&Status::parse(Some(word))))
            .collect();
        assert_eq!(ranks, [0, 1, 2, 3, 4]);
        assert!(status_rank(&Status::Unreported) > 4);
    }

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

    #[test]
    fn only_two_statuses_are_asked_about_by_name() {
        let busy = Status::parse(Some("busy"));
        assert!(busy.is_busy() && !busy.is_waiting());
        let waiting = Status::parse(Some("waiting"));
        assert!(waiting.is_waiting() && !waiting.is_busy());
        assert_ne!(full_tag(&busy, 0), full_tag(&busy, 1));
    }
}
