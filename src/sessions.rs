use std::collections::{BTreeMap, HashSet};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::Cursor;
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
            rows.sort_by(|a, b| a.kind_then_recency(b));
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

    pub fn name_error(&self) -> Option<&'static str> {
        validate_name(&self.search_term)
    }
}

pub const MAX_NAME_BYTES: usize = 107;

pub fn validate_name(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return None;
    }
    if name.trim().is_empty() {
        return Some("name cannot be only whitespace");
    }
    if name == "." || name == ".." {
        return Some("name cannot be '.' or '..'");
    }
    if name.contains('/') {
        return Some("name cannot contain '/'");
    }
    if name.len() > MAX_NAME_BYTES {
        return Some("name must be shorter than 108 bytes");
    }
    None
}

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
