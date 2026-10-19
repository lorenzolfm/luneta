use std::collections::{BTreeMap, HashSet};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::{Cursor, Stops};
use crate::elapsed::Age;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Live,
    Resurrectable,
}

#[derive(Default)]
pub struct Sessions {
    pub live: Vec<Session>,
    pub dead: Vec<Session>,
}

pub struct Session {
    pub name: String,
    pub age: Age,
}

impl Sessions {
    pub fn any_named(&self, name: &str) -> bool {
        self.live.iter().chain(&self.dead).any(|s| s.name == name)
    }

    pub fn taken<'a>(&'a self, current: Option<&'a str>) -> Taken<'a> {
        Taken::new(self, current)
    }
}

/// The session names already in use, in a shape that answers the same question
/// for every directory in the list without re-walking the snapshot each time.
pub struct Taken<'a>(HashSet<&'a str>);

impl<'a> Taken<'a> {
    fn new(sessions: &'a Sessions, current: Option<&'a str>) -> Self {
        let mut names: HashSet<&str> = sessions
            .live
            .iter()
            .chain(&sessions.dead)
            .map(|session| session.name.as_str())
            .collect();
        names.extend(current);
        Taken(names)
    }

    pub fn holds(&self, name: &str) -> bool {
        self.0.contains(name)
    }
}

pub struct Row {
    pub name: String,
    pub kind: Kind,
    pub age: Age,
    pub indices: Vec<usize>,
    score: i64,
    is_exact: bool,
}

pub struct Contents {
    pub panes: usize,
    pub focus: Option<Focus>,
}

pub struct Focus {
    pub pane: u32,
    pub tab: String,
    pub title: String,
}

#[derive(Clone, Copy)]
pub enum Selection {
    SnapToTop,
    Hold,
}

#[derive(Default)]
pub struct MatchSet {
    pub search_term: String,
    pub rows: Cursor<Row>,
    pub current_session: Option<String>,
    pub contents: BTreeMap<String, Contents>,
    matcher: Option<SkimMatcherV2>,
}

impl MatchSet {
    pub fn refresh(&mut self, sessions: &Sessions, current_session: Option<String>) {
        self.current_session = current_session;
        self.rebuild(sessions, Selection::Hold);
    }

    fn rebuild(&mut self, sessions: &Sessions, policy: Selection) {
        let term = self.search_term.clone();
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_name().map(str::to_owned),
        };
        let mut rows: Vec<Row> = Vec::new();

        if term.is_empty() {
            for session in &sessions.live {
                rows.push(Row::new(
                    session.name.clone(),
                    Kind::Live,
                    session.age,
                    0,
                    vec![],
                    false,
                ));
            }
            for session in &sessions.dead {
                rows.push(Row::new(
                    session.name.clone(),
                    Kind::Resurrectable,
                    session.age,
                    0,
                    vec![],
                    false,
                ));
            }
            rows.sort_by(Row::kind_then_recency);
        } else {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
            for (kind, list) in [
                (Kind::Live, &sessions.live),
                (Kind::Resurrectable, &sessions.dead),
            ] {
                for session in list {
                    if let Some((score, indices)) = matcher.fuzzy_indices(&session.name, &term) {
                        let is_exact = session.name == term;
                        rows.push(Row::new(
                            session.name.clone(),
                            kind,
                            session.age,
                            score,
                            indices,
                            is_exact,
                        ));
                    }
                }
            }
            rows.sort_by(|a, b| {
                a.kind_rank()
                    .cmp(&b.kind_rank())
                    .then_with(|| b.is_exact.cmp(&a.is_exact))
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.age.cmp(&b.age))
            });
        }

        self.rows.replace(rows, |row| held.as_deref() == Some(row.name.as_str()));
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.rows.selected_row().map(|r| r.name.as_str())
    }

    pub fn set_search_term(&mut self, term: String, sessions: &Sessions) {
        self.search_term = term;
        self.rebuild(sessions, Selection::SnapToTop);
    }

    pub fn is_own_name(&self) -> bool {
        !self.search_term.is_empty()
            && self.current_session.as_deref() == Some(self.search_term.as_str())
    }

    /// The session the term makes: none for an empty term, which leaves zellij
    /// to pick the name.
    pub fn new_name(&self) -> Result<Option<SessionName>, &'static str> {
        if self.search_term.is_empty() {
            return Ok(None);
        }
        SessionName::parse(&self.search_term).map(Some)
    }

    pub fn name_error(&self) -> Option<&'static str> {
        self.new_name().err()
    }
}

/// The longest name zellij can make a session of. zellij makes a session's
/// socket at `/run/user/<uid>/zellij/contract_version_1/` and the name, a
/// socket path holds 107 bytes at most, and a uid has ten digits at most: that
/// leaves 60 for the name. A session with a longer name is one zellij cannot
/// make, and the terminal that asks for it hangs. A `ZELLIJ_SOCKET_DIR`, or an
/// `XDG_RUNTIME_DIR` longer than `/run/user/<uid>`, leaves less, and luneta
/// cannot tell.
pub const MAX_NAME_BYTES: usize = 60;

/// A name zellij can make a session of. `parse` is the one way to make one and
/// `with_postfix` the one way to make another from it, so a name that reaches
/// zellij as one has been held to what zellij takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        if name.len() > MAX_NAME_BYTES {
            return Err("name must be shorter than 61 bytes");
        }
        Ok(SessionName(name.to_owned()))
    }

    /// The name with `-n` after it, cut between characters where it must be
    /// for the two to fit. What is left of a name holds no `/`, and the postfix
    /// is neither whitespace nor a dot, so the two are a name too.
    pub fn with_postfix(&self, n: usize) -> Self {
        let postfix = format!("-{n}");
        let kept = head(&self.0, MAX_NAME_BYTES.saturating_sub(postfix.len()));
        SessionName(format!("{kept}{postfix}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SessionName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

fn head(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut end = bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

impl Stops for Row {}

impl Row {
    pub(crate) fn new(
        name: String,
        kind: Kind,
        age: Age,
        score: i64,
        indices: Vec<usize>,
        is_exact: bool,
    ) -> Self {
        Row { name, kind, age, indices, score, is_exact }
    }

    fn kind_rank(&self) -> u8 {
        match self.kind {
            Kind::Live => 0,
            Kind::Resurrectable => 1,
        }
    }

    fn kind_then_recency(&self, other: &Self) -> std::cmp::Ordering {
        self.kind_rank()
            .cmp(&other.kind_rank())
            .then_with(|| self.age.cmp(&other.age))
    }
}
