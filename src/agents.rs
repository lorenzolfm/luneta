use std::collections::HashMap;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use serde::Deserialize;

use crate::cursor::Cursor;
use crate::elapsed::{Age, Held};
use crate::fetch::Fetch;
use crate::places::Places;
use crate::sessions::{Selection, Session};

pub const QUERY: [&str; 1] = ["claude-ps"];

pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "agents";

pub const CLOSE_VALUE: &str = "close";
pub const LABEL_KEY: &str = "luneta_label";

/// Closes a pane of another session; the plugin can only close its own
/// session's.
pub fn close<'a>(session: &'a str, pane: &'a str) -> [&'a str; 7] {
    ["zellij", "--session", session, "action", "close-pane", "--pane-id", pane]
}

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

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Seat {
    Here(String),
    There(String),
}

impl Seat {
    pub fn session(&self) -> &str {
        match self {
            Seat::Here(session) | Seat::There(session) => session,
        }
    }
}

pub struct Live<'a> {
    current: Option<&'a str>,
    places: &'a Places,
    /// The live sessions in the order the session list shows them: the one
    /// you are in first, then the others, newest first.
    order: Vec<&'a str>,
}

impl<'a> Live<'a> {
    /// `live` is the session list's live sessions, which leave out the one
    /// you are in.
    pub fn new(current: Option<&'a str>, live: &'a [Session], places: &'a Places) -> Self {
        let mut others: Vec<&Session> = live.iter().collect();
        others.sort_by(|a, b| a.age.cmp(&b.age).then_with(|| a.name.cmp(&b.name)));
        let order = current.into_iter().chain(others.iter().map(|s| s.name.as_str())).collect();
        Live { current, places, order }
    }

    /// Where `session` stands in the session list, for its agents to stand
    /// with it. A session the list does not know goes last.
    fn rank(&self, session: &str) -> usize {
        self.order.iter().position(|name| *name == session).unwrap_or(self.order.len())
    }

    fn knows(&self, session: &str) -> bool {
        self.order.contains(&session)
    }

    fn seat(&self, session: &str) -> Seat {
        if self.current == Some(session) {
            Seat::Here(session.to_owned())
        } else {
            Seat::There(session.to_owned())
        }
    }

    fn place(&self, agent: &Agent) -> Option<Seat> {
        self.places
            .find(agent.pane, &agent.cwd, &agent.reported)
            .map(|session| self.seat(session))
    }
}

pub struct Agent {
    reported: String,
    name: Option<String>,
    pane: u32,
    status: Status,
    age: Held,
    cwd: String,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "a chosen name, a shared label, a guessed pane and an exact match are four facts \
              about a row, not the states of one"
)]
pub struct AgentRow {
    reported: String,
    pub seat: Seat,
    pub display: String,
    pub chosen: bool,
    pub pane: u32,
    pub status: Status,
    pub age: Held,
    pub cwd: String,
    pub shared: bool,
    /// The agent names a session that is still running, but its pane turned
    /// up in another, matched by its id and directory alone: where it sits is
    /// a guess. A session that is gone is one renamed, and no guess.
    pub guessed: bool,
    pub indices: Vec<usize>,
    rank: usize,
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
            word: word.to_owned(),
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
    /// The agent in the pane the picker was opened from, kept out of the rows
    /// as the session you are in is kept out of the session list.
    pub current: Option<AgentRow>,
    asking: bool,
    pub unplaced: usize,
    matcher: Option<SkimMatcherV2>,
}

impl AgentSet {
    /// Whether to ask claude-ps: not while its last answer is still to come.
    pub fn ask(&mut self) -> bool {
        if self.asking {
            return false;
        }
        self.asking = true;
        true
    }

    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.fail(if reason.is_empty() {
                "claude-ps is not available".to_owned()
            } else {
                format!("claude-ps: {reason}")
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

    /// The rows keep their order, whatever the agents do: their sessions' in
    /// the session list, and the order their panes were opened in within each.
    /// A term puts the best match first, as it does in the session list.
    pub fn rebuild(
        &mut self,
        term: &str,
        live: &Live,
        origin: Option<u32>,
        since: Age,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_row().map(|r| (r.reported.clone(), r.pane)),
        };
        let mut rows: Vec<AgentRow> = Vec::new();
        let mut current = None;
        let mut unplaced = 0;

        if let Fetch::Ready(all) = &self.status {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));

            for agent in all {
                let Some(seat) = live.place(agent) else {
                    unplaced += 1;
                    continue;
                };
                let display = agent
                    .name
                    .clone()
                    .unwrap_or_else(|| seat.session().to_owned());
                let mut row = AgentRow {
                    reported: agent.reported.clone(),
                    rank: live.rank(seat.session()),
                    guessed: agent.reported != seat.session() && live.knows(&agent.reported),
                    seat,
                    display,
                    chosen: agent.name.is_some(),
                    pane: agent.pane,
                    status: agent.status.clone(),
                    age: agent.age.grown_by(since),
                    cwd: agent.cwd.clone(),
                    shared: false,
                    indices: Vec::new(),
                    score: 0,
                    is_exact: false,
                };
                if matches!(row.seat, Seat::Here(_)) && origin == Some(agent.pane) {
                    current = Some(row);
                    continue;
                }
                if !term.is_empty() {
                    let Some((score, indices)) = matcher.fuzzy_indices(&row.display, term) else {
                        continue;
                    };
                    row.is_exact = row.display == term;
                    row.score = score;
                    row.indices = indices;
                }
                rows.push(row);
            }
        }

        rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| a.rank.cmp(&b.rank))
                .then_with(|| a.seat.session().cmp(b.seat.session()))
                .then_with(|| a.pane.cmp(&b.pane))
        });

        mark_shared(&mut rows.iter_mut().chain(current.as_mut()).collect::<Vec<_>>());
        self.current = current;
        self.unplaced = unplaced;

        self.rows.replace(rows, |row| {
            held.as_ref()
                .is_some_and(|(reported, pane)| row.reported == *reported && row.pane == *pane)
        });
    }

    pub fn selected_row(&self) -> Option<&AgentRow> {
        self.rows.selected_row()
    }

    /// Leaves the highlighted agent out until claude-ps is next asked, for a
    /// pane that is closing: the agent in it can outlive an ask made now, and
    /// come back. The rows are only redrawn by the next `rebuild`.
    pub fn forget_selected(&mut self) {
        let Some(row) = self.rows.selected_row() else {
            return;
        };
        let (reported, pane) = (row.reported.clone(), row.pane);
        if let Fetch::Ready(all) = &mut self.status {
            all.retain(|agent| agent.reported != reported || agent.pane != pane);
        }
    }

    pub fn any_busy(&self) -> bool {
        self.rows.iter().any(|row| row.status.is_busy())
    }

    /// The agent after the one you are in, among those that want you: the
    /// waiting first, then the idle, the most recent first. Wraps around, and
    /// passes over the pane you are in when it is the only one.
    pub fn next_wanted(&self, here: Option<(&str, u32)>) -> Option<&AgentRow> {
        let mut wanted: Vec<&AgentRow> = self
            .rows
            .iter()
            .filter(|row| matches!(row.status, Status::Waiting | Status::Idle))
            .collect();
        wanted.sort_by(|a, b| {
            status_rank(&a.status).cmp(&status_rank(&b.status)).then_with(|| a.age.cmp(&b.age))
        });
        let at = here.and_then(|(session, pane)| {
            wanted.iter().position(|row| row.seat.session() == session && row.pane == pane)
        });
        match at {
            Some(_) if wanted.len() == 1 => None,
            Some(at) => wanted.get((at + 1) % wanted.len()).copied(),
            None => wanted.first().copied(),
        }
    }

    /// Highlights the agent in `pane` of `session`, or else the first in
    /// `session`: the nearest the list has to a row of the sidebar.
    pub fn select_seated(&mut self, session: &str, pane: Option<u32>) {
        let seated = |row: &AgentRow| row.seat.session() == session;
        let at = self
            .rows
            .iter()
            .position(|row| seated(row) && Some(row.pane) == pane)
            .or_else(|| self.rows.iter().position(seated));
        if let Some(at) = at {
            self.rows.select(at);
        }
    }
}

fn mark_shared(rows: &mut [&mut AgentRow]) {
    let shared: Vec<bool> = {
        let mut seen: HashMap<&str, usize> = HashMap::with_capacity(rows.len());
        for row in rows.iter() {
            *seen.entry(row.display.as_str()).or_insert(0) += 1;
        }
        rows.iter().map(|row| seen[row.display.as_str()] > 1).collect()
    };
    for (row, shared) in rows.iter_mut().zip(shared) {
        row.shared = shared;
    }
}

pub fn status_rank(status: &Status) -> u8 {
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

/// The sidebar's mark for a status: one column wide, whatever a terminal
/// makes of an emoji, so that the labels after it line up and it takes the
/// colour it is drawn in. The picker keeps the emoji.
pub fn dot(status: &Status, frame: u64) -> String {
    match status {
        Status::Waiting => "●".to_owned(),
        Status::Idle => "✓".to_owned(),
        Status::Busy => SPINNER[(frame as usize) % SPINNER.len()].to_owned(),
        Status::Shell => "$".to_owned(),
        Status::Unknown(word) => word.initial().to_uppercase().collect(),
        Status::Unreported => "?".to_owned(),
    }
}

pub fn full_tag(status: &Status, frame: u64) -> String {
    format!("{} {}", glyph(status, frame), status.word().to_uppercase())
}

pub fn abbr_tag(status: &Status, frame: u64) -> String {
    match status {
        Status::Unknown(word) => format!("[{}]", word.initial().to_uppercase()),
        _ => glyph(status, frame).to_owned(),
    }
}

fn parse(stdout: &str) -> Result<Vec<Agent>, String> {
    let wire: Vec<Wire> =
        serde_json::from_str(stdout).map_err(|error| format!("claude-ps: {error}"))?;

    let mut agents = Vec::new();
    for row in wire {
        let Some(zellij) = row.zellij else { continue };
        let Ok(pane) = zellij.pane.parse::<u32>() else { continue };
        if zellij.session.is_empty() {
            continue;
        }
        agents.push(Agent {
            reported: zellij.session,
            name: chosen_name(row.name.as_deref(), row.name_source.as_deref()),
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
    chosen.then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(json: &str, current: Option<&str>, places: &Places) -> AgentSet {
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), json.as_bytes(), b"");
        let live = Live::new(current, &[], places);
        agents.rebuild("", &live, None, Age::ZERO, Selection::SnapToTop);
        agents
    }

    fn status(json: &str) -> Status {
        let agents = parse(json).expect("the document should parse");
        agents.into_iter().next().expect("one agent").status
    }

    fn one(status: &str) -> String {
        format!(
            r#"[{{"status":{status},"status_age":4,"cwd":"/w/s","zellij":{{"session":"s","pane":"0"}}}}]"#
        )
    }

    #[test]
    fn a_failure_leaves_no_agents_behind() {
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let mut agents = rows(&one(r#""idle""#), None, &places);
        assert_eq!(agents.rows.len(), 1);

        agents.fail("claude-ps: gone");
        agents.rebuild("", &Live::new(None, &[], &places), None, Age::ZERO, Selection::SnapToTop);
        assert!(agents.rows.is_empty());
        assert!(agents.rows.selected().is_none());
    }

    const TWO_IN_ONE_SESSION: &str = r#"[
        {"status":"idle","status_age":4,"cwd":"/w/luneta",
         "zellij":{"session":"misc-luneta","pane":"0"}},
        {"status":"idle","status_age":4,"cwd":"/w/luneta",
         "zellij":{"session":"luneta","pane":"6"}}]"#;

    fn luneta_holds_both() -> Places {
        Places::of(&[("luneta", &[(0, "/w/luneta"), (6, "/w/luneta")])])
    }

    #[test]
    fn a_renamed_session_takes_its_agents_with_it() {
        let agents = rows(TWO_IN_ONE_SESSION, Some("luneta"), &luneta_holds_both());

        let seats: Vec<&Seat> = agents.rows.iter().map(|row| &row.seat).collect();
        assert_eq!(seats, [&Seat::Here("luneta".to_owned()); 2]);
        let labels: Vec<String> = agents.rows.iter().map(super::AgentRow::label).collect();
        assert_eq!(labels, ["luneta:0", "luneta:6"]);
        assert_eq!(agents.unplaced, 0);
    }

    #[test]
    fn the_pane_you_are_in_is_pinned_by_its_pane_and_not_its_name() {
        let places = luneta_holds_both();
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), TWO_IN_ONE_SESSION.as_bytes(), b"");
        agents.rebuild(
            "misc",
            &Live::new(Some("luneta"), &[], &places),
            Some(0),
            Age::ZERO,
            Selection::SnapToTop,
        );
        assert!(agents.rows.is_empty());
        assert_eq!(agents.current.as_ref().map(|row| row.pane), Some(0));

        agents.rebuild(
            "",
            &Live::new(Some("luneta"), &[], &places),
            Some(0),
            Age::ZERO,
            Selection::SnapToTop,
        );
        let labels: Vec<String> = agents.rows.iter().map(AgentRow::label).collect();
        assert_eq!(labels, ["luneta:6"]);
        assert_eq!(agents.current.as_ref().map(AgentRow::label).as_deref(), Some("luneta:0"));
    }

    #[test]
    fn a_name_the_user_chose_outlives_a_rename() {
        let json = r#"[{"status":"idle","status_age":4,"name":"handoff","name_source":"user",
            "cwd":"/w/luneta","zellij":{"session":"misc-luneta","pane":"0"}}]"#;
        let places = Places::of(&[("luneta", &[(0, "/w/luneta")])]);
        let agents = rows(json, Some("luneta"), &places);
        assert_eq!(agents.rows[0].display, "handoff");
        assert_eq!(agents.rows[0].seat, Seat::Here("luneta".to_owned()));
    }

    #[test]
    fn a_reported_name_that_still_names_a_holder_breaks_the_tie() {
        let json = r#"[{"status":"idle","status_age":4,"cwd":"/w/misc",
            "zellij":{"session":"ghostty","pane":"0"}}]"#;
        let places = Places::of(&[
            ("luneta", &[(0, "/w/misc")]),
            ("ghostty", &[(0, "/w/misc")]),
        ]);
        let agents = rows(json, Some("luneta"), &places);
        assert_eq!(agents.rows[0].seat, Seat::There("ghostty".to_owned()));
        assert_eq!(agents.rows[0].display, "ghostty");
    }

    #[test]
    fn the_session_holding_the_pane_the_agent_works_in_claims_it() {
        let json = r#"[{"status":"idle","status_age":4,"cwd":"/w/bipa/affiliate",
            "zellij":{"session":"bipa.git","pane":"4"}}]"#;
        let places = Places::of(&[
            ("luneta", &[(0, "/w/luneta")]),
            ("affiliate", &[(0, "/w/bipa"), (4, "/w/bipa/affiliate")]),
        ]);
        let agents = rows(json, Some("luneta"), &places);
        assert_eq!(agents.rows[0].seat, Seat::There("affiliate".to_owned()));
        assert_eq!(agents.rows[0].display, "affiliate");
    }

    #[test]
    fn a_pane_two_sessions_could_answer_for_is_left_alone() {
        let json = r#"[{"status":"idle","status_age":4,"cwd":"/w/repo",
            "zellij":{"session":"gone","pane":"0"}}]"#;
        let places = Places::of(&[("one", &[(0, "/w/repo")]), ("two", &[(0, "/w/repo")])]);
        let agents = rows(json, Some("one"), &places);
        assert!(agents.rows.is_empty());
        assert_eq!(agents.unplaced, 1);
    }

    #[test]
    fn an_agent_no_live_pane_answers_for_is_dropped() {
        let json = r#"[{"status":"idle","status_age":4,"cwd":"/w/bipa",
            "zellij":{"session":"bipa.git","pane":"4"}}]"#;
        let places = Places::of(&[("luneta", &[(0, "/w/luneta")]), ("ghostty", &[(0, "/w/misc")])]);
        let agents = rows(json, Some("luneta"), &places);
        assert!(agents.rows.is_empty());
        assert_eq!(agents.unplaced, 1);
    }

    #[test]
    fn before_the_first_list_panes_reply_nothing_is_placed() {
        let json = r#"[{"status":"idle","status_age":4,"cwd":"/w/misc",
            "zellij":{"session":"ghostty","pane":"0"}}]"#;
        let agents = rows(json, None, &Places::default());
        assert!(agents.rows.is_empty());
        assert_eq!(agents.unplaced, 1);
    }

    #[test]
    fn the_search_asks_the_name_the_session_goes_by_now() {
        let places = luneta_holds_both();
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), TWO_IN_ONE_SESSION.as_bytes(), b"");
        agents.rebuild(
            "luneta",
            &Live::new(Some("luneta"), &[], &places),
            None,
            Age::ZERO,
            Selection::SnapToTop,
        );
        assert_eq!(agents.rows.len(), 2);
        agents.rebuild(
            "misc",
            &Live::new(Some("luneta"), &[], &places),
            None,
            Age::ZERO,
            Selection::SnapToTop,
        );
        assert!(agents.rows.is_empty());
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
        assert_eq!(agents[0].reported, "s");
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
    fn the_next_agent_that_wants_you_comes_round_in_order() {
        let json = r#"[
            {"status":"idle","status_age":90,"cwd":"/w/a","zellij":{"session":"a","pane":"0"}},
            {"status":"busy","status_age":5,"cwd":"/w/b","zellij":{"session":"b","pane":"0"}},
            {"status":"waiting","status_age":30,"cwd":"/w/c","zellij":{"session":"c","pane":"0"}},
            {"status":"idle","status_age":10,"cwd":"/w/d","zellij":{"session":"d","pane":"0"}}]"#;
        let places = Places::of(&[
            ("a", &[(0, "/w/a")]),
            ("b", &[(0, "/w/b")]),
            ("c", &[(0, "/w/c")]),
            ("d", &[(0, "/w/d")]),
        ]);
        let agents = rows(json, None, &places);
        let next = |here: Option<(&str, u32)>| {
            agents.next_wanted(here).map(|row| row.seat.session().to_owned())
        };
        assert_eq!(next(None).as_deref(), Some("c"));
        assert_eq!(next(Some(("c", 0))).as_deref(), Some("d"));
        assert_eq!(next(Some(("d", 0))).as_deref(), Some("a"));
        assert_eq!(next(Some(("a", 0))).as_deref(), Some("c"));
        assert_eq!(next(Some(("b", 0))).as_deref(), Some("c"));
    }

    /// Sessions `old` and `new` besides `here`, the one you are in, each with
    /// agents in panes 2 and 5. `status` gives each agent its status and age.
    fn three_sessions(status: impl Fn(&str, u32) -> (&'static str, u64)) -> AgentSet {
        let mut json = Vec::new();
        for session in ["old", "here", "new"] {
            for pane in [2, 5] {
                let (status, age) = status(session, pane);
                json.push(format!(
                    r#"{{"status":"{status}","status_age":{age},"cwd":"/w/{session}",
                        "zellij":{{"session":"{session}","pane":"{pane}"}}}}"#
                ));
            }
        }
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), format!("[{}]", json.join(",")).as_bytes(), b"");
        rebuild_three(&mut agents);
        agents
    }

    fn rebuild_three(agents: &mut AgentSet) {
        let places = Places::of(&[
            ("old", &[(2, "/w/old"), (5, "/w/old")]),
            ("here", &[(2, "/w/here"), (5, "/w/here")]),
            ("new", &[(2, "/w/new"), (5, "/w/new")]),
        ]);
        let named =
            |name: &str, age: u64| Session { name: name.to_owned(), age: Age::from_secs(age) };
        let sessions = [named("old", 900), named("new", 60)];
        let live = Live::new(Some("here"), &sessions, &places);
        agents.rebuild("", &live, None, Age::ZERO, Selection::Hold);
    }

    fn seats(agents: &AgentSet) -> Vec<(&str, u32)> {
        agents.rows.iter().map(|row| (row.seat.session(), row.pane)).collect()
    }

    #[test]
    fn the_rows_stand_in_the_order_of_the_session_list_whatever_the_agents_do() {
        let order = [("here", 2), ("here", 5), ("new", 2), ("new", 5), ("old", 2), ("old", 5)];
        let quiet = three_sessions(|_, _| ("busy", 10));
        assert_eq!(seats(&quiet), order);
        let loud = three_sessions(|session, pane| match (session, pane) {
            ("old", 5) => ("waiting", 1),
            ("new", 2) => ("busy", 1),
            (_, 2) => ("idle", 3000),
            _ => ("waiting", 600),
        });
        assert_eq!(seats(&loud), order);
    }

    #[test]
    fn a_closing_pane_takes_only_its_own_agent_out_of_the_list() {
        let mut agents = three_sessions(|_, _| ("idle", 10));
        agents.rows.select(3);
        agents.forget_selected();
        rebuild_three(&mut agents);
        assert_eq!(seats(&agents), [("here", 2), ("here", 5), ("new", 2), ("old", 2), ("old", 5)]);
    }

    /// `foo` has not said what its panes are yet, and `notes` has a pane 2 in
    /// the directory foo's agent works in. `misc-notes` is what `notes` was
    /// called before a rename.
    #[test]
    fn a_pane_found_for_an_agent_that_names_another_running_session_is_a_guess() {
        let json = r#"[
            {"status":"idle","status_age":4,"cwd":"/w/foo","zellij":{"session":"foo","pane":"2"}},
            {"status":"idle","status_age":4,"cwd":"/w/notes",
             "zellij":{"session":"misc-notes","pane":"5"}},
            {"status":"idle","status_age":4,"cwd":"/w/notes","zellij":{"session":"notes","pane":"7"}}]"#;
        let places = Places::of(&[("notes", &[(2, "/w/foo"), (5, "/w/notes"), (7, "/w/notes")])]);
        let named = |name: &str| Session { name: name.to_owned(), age: Age::ZERO };
        let sessions = [named("foo"), named("notes")];
        let mut agents = AgentSet::default();
        agents.ingest(Some(0), json.as_bytes(), b"");
        let live = Live::new(None, &sessions, &places);
        agents.rebuild("", &live, None, Age::ZERO, Selection::SnapToTop);
        let guessed: Vec<(&str, u32, bool)> =
            agents.rows.iter().map(|row| (row.seat.session(), row.pane, row.guessed)).collect();
        assert_eq!(guessed, [("notes", 2, true), ("notes", 5, false), ("notes", 7, false)]);
    }

    #[test]
    fn the_only_agent_that_wants_you_is_not_next_when_you_are_in_it() {
        let places = Places::of(&[("s", &[(0, "/w/s")])]);
        let agents = rows(&one(r#""waiting""#), None, &places);
        assert!(agents.next_wanted(Some(("s", 0))).is_none());
        assert_eq!(agents.next_wanted(Some(("s", 1))).map(|row| row.pane), Some(0));
        let busy = rows(&one(r#""busy""#), None, &places);
        assert!(busy.next_wanted(None).is_none());
    }

    #[test]
    fn a_row_of_the_sidebar_lands_on_its_agent_or_else_the_first_in_its_session() {
        let json = r#"[
            {"status":"idle","status_age":10,"cwd":"/w/s","zellij":{"session":"s","pane":"0"}},
            {"status":"busy","status_age":5,"cwd":"/w/s","zellij":{"session":"s","pane":"1"}},
            {"status":"waiting","status_age":30,"cwd":"/w/t","zellij":{"session":"t","pane":"0"}}]"#;
        let places = Places::of(&[("s", &[(0, "/w/s"), (1, "/w/s")]), ("t", &[(0, "/w/t")])]);
        for (session, pane, landed) in [
            ("s", Some(0), ("s", 0)),
            ("s", Some(1), ("s", 1)),
            ("s", Some(9), ("s", 0)),
            ("s", None, ("s", 0)),
            ("u", Some(0), ("t", 0)),
        ] {
            let mut agents = rows(json, None, &places);
            agents.rows.move_selection(2);
            agents.select_seated(session, pane);
            let row = agents.selected_row().expect("a row stays highlighted");
            assert_eq!((row.seat.session(), row.pane), landed, "{session} {pane:?}");
        }
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
