//! The match set: the rows the picker shows, in the order it shows them.
//!
//! Two rules control this module:
//!
//! 1. The current session is removed here, not in the renderer. The rendered list is thus
//!    equal to the match set, and their indices cannot disagree.
//! 2. A filter only removes rows. It never regroups them: live sessions sort before
//!    resurrectable ones at every stage.

use std::collections::BTreeMap;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::Cursor;
use crate::elapsed::Age;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A live session. The host resolves its name to an attach.
    Live,
    /// A dead session with a saved layout. The host resolves its name to a resurrect.
    Resurrectable,
}

/// The last snapshot, split the way the host splits it.
///
/// The two lists travel as one value because they are the same type and mean opposite things.
/// As two adjacent `&[(String, Duration)]` parameters they crossed six signatures, and a swap
/// at any one of them compiled: every `Attach` became a `Resurrect`, and `Enter` resurrected a
/// saved layout over a session that was running. The pairing is now made once, in
/// [`crate::State::poll`], beside the two snapshot fields it reads from.
#[derive(Default)]
pub struct Sessions {
    /// Live sessions, without the current one. The poll removes it, not the renderer (rule 1).
    pub live: Vec<Session>,
    /// Dead sessions the host has a saved layout for. Nothing else can be resurrected.
    pub dead: Vec<Session>,
}

/// One session in the snapshot. Which list holds it says whether it is live or resurrectable.
pub struct Session {
    pub name: String,
    /// Elapsed age, as the host reports it. The host truncates it to whole seconds
    /// (`screen.rs:3361`, `plugin_api/event.rs:1214`). The plugin sandbox cannot see the
    /// sockets that a better value needs.
    pub age: Age,
}

impl Sessions {
    /// Does any session already have this name, live or dead?
    ///
    /// Both lists, because the host resolves a name across both: a name that a saved layout
    /// holds is as unusable for a rename as one a running session holds.
    pub fn any_named(&self, name: &str) -> bool {
        self.live.iter().chain(&self.dead).any(|s| s.name == name)
    }
}

/// One row, which is also one match-set entry. There is no separate render-side list.
pub struct Row {
    pub name: String,
    pub kind: Kind,
    /// Elapsed age, carried from the snapshot. See [`Session::age`] for what the host's value
    /// is worth.
    pub age: Age,
    /// Character positions the fuzzy matcher hit, for highlighting. Empty on an empty term.
    pub indices: Vec<usize>,
    score: i64,
    is_exact: bool,
}

/// Which pane of a live session the preview box shows, and what the session contains.
///
/// The data comes from the other sessions' `SessionInfo`. Each zellij server writes its tabs
/// and panes to `session-metadata.kdl` about once a second, and every other server reads them
/// (`zellij-utils/src/sessions.rs`, `read_live_session_states`). The picker can thus find the
/// focused pane of a session it is not attached to, from the snapshot that gives the ages.
///
/// A resurrectable session has no such data. Its layout is on disk in a form the host does not
/// give to a plugin, and no process is behind it.
pub struct Contents {
    /// Selectable panes in all tabs, counted where [`Focus`] applies its filter.
    pub panes: usize,
    /// The pane whose screen the box shows. `None` if the session has only plugin panes,
    /// which dump nothing.
    pub focus: Option<Focus>,
}

/// The pane the preview box reads, and its location.
///
/// This is the focused pane of the active tab: the pane you see immediately after `Enter`.
///
/// Only selectable, unsuppressed terminals are used. Zellij's tab bar and status bar are panes
/// in the same manifest, and `is_selectable` is the flag that identifies them. A plugin pane
/// dumps an empty screen. Either one would make the box blank.
pub struct Focus {
    pub pane: u32,
    /// The tab that holds the pane, for the caption line.
    pub tab: String,
    /// The pane title, from the source the tab bar uses.
    pub title: String,
}

/// What a rebuild does with the cursor.
///
/// The directory and agent screens share this rule. Their lists hold different things and sort
/// by different keys, but a new term always goes to the top match and a background refresh
/// always keeps its position.
#[derive(Clone, Copy)]
pub enum Selection {
    /// The search term changed: go to the top match.
    SnapToTop,
    /// The list was re-polled: stay on the same entry if it is still there.
    Hold,
}

#[derive(Default)]
pub struct MatchSet {
    pub search_term: String,
    /// The rows and the cursor in them. See [`Cursor`], which holds the two together so that
    /// `None` and an empty list cannot come apart.
    pub rows: Cursor<Row>,
    /// The name of the current session, so that the note line can say the picker hides it.
    /// It is never in `rows`.
    pub current_session: Option<String>,
    /// Does the current session match the term? This costs one `fuzzy_indices` call per
    /// keystroke, and is the whole cost of the note line.
    pub current_matches: bool,
    /// Which pane to show for each live session, by name. It is kept beside the rows because
    /// the rows are rebuilt on each keystroke, but this map is rebuilt once per poll.
    ///
    /// A name that is absent has nothing to show. It is a resurrectable session, or a live
    /// session whose server has not yet written its metadata.
    pub contents: BTreeMap<String, Contents>,
    matcher: Option<SkimMatcherV2>,
}

impl MatchSet {
    /// Rebuild from the latest poll.
    ///
    /// The caller splits the snapshot and removes the current session from `live` (rule 1). It
    /// keeps the whole snapshot, so that a keystroke can filter again before the next poll.
    pub fn refresh(&mut self, sessions: &Sessions, current_session: Option<String>) {
        self.current_session = current_session;
        // Hold, not snap: this runs once a second in the background, and a snap would move
        // the cursor back to row 0 on every poll.
        self.rebuild(sessions, Selection::Hold);
    }

    fn rebuild(&mut self, sessions: &Sessions, policy: Selection) {
        let term = self.search_term.clone();
        // Computed below when the term is not empty. An empty term matches nothing.
        self.current_matches = false;
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
            // Live newest first, then resurrectable newest first. `age` is elapsed time, so
            // ascending age is newest first.
            rows.sort_by(|a, b| a.kind_then_recency(b));
        } else {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
            // The current session is never a row, but the note line must know whether the
            // term matches it. Without this, the name of your own session gives an empty list
            // and no explanation.
            self.current_matches = self
                .current_session
                .as_deref()
                .map(|name| matcher.fuzzy_indices(name, &term).is_some())
                .unwrap_or(false);
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
            // Live before resurrectable, then exact match, then score, then recency. Kind
            // sorts above all else, which keeps the live/dead boundary in one place as you
            // type. The list is thus always two groups, which is what the separator row needs
            // (see rule 2 in the module doc).
            //
            // The cost: the exact name of a dead session no longer moves the cursor to it
            // while live rows also match. The cursor goes to the top of the dead group.
            rows.sort_by(|a, b| {
                a.kind_rank()
                    .cmp(&b.kind_rank())
                    .then_with(|| b.is_exact.cmp(&a.is_exact))
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.age.cmp(&b.age))
            });
        }

        // `Hold` keeps the cursor on the same session, not on the same index. A session that is
        // gone falls back to the top, and an empty list to no cursor at all — see
        // [`Cursor::replace`], which is where those two answers now live.
        self.rows.replace(rows, |row| held.as_deref() == Some(row.name.as_str()));
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.rows.selected_row().map(|r| r.name.as_str())
    }

    /// Set a new search term. The cursor goes to the top match, which is the best answer to a
    /// term that has just changed.
    pub fn set_search_term(&mut self, term: String, sessions: &Sessions) {
        self.search_term = term;
        self.rebuild(sessions, Selection::SnapToTop);
    }

    /// Is the term the name of the current session?
    ///
    /// That session is not in `rows` and cannot be the highlight. This test makes `Enter` do
    /// nothing, instead of offering to create a session that exists.
    pub fn is_own_name(&self) -> bool {
        !self.search_term.is_empty()
            && self.current_session.as_deref() == Some(self.search_term.as_str())
    }

    /// Why the search term cannot be a session name, or `None` if it can. The rename screen
    /// uses the same [`validate_name`].
    pub fn name_error(&self) -> Option<&'static str> {
        validate_name(&self.search_term)
    }
}

/// The longest name the host accepts. `validate_session_name` refuses 108 bytes and more, and
/// [`crate::dirs::free_name`] keeps its postfixed names inside this.
pub const MAX_NAME_BYTES: usize = 107;

/// Why `name` cannot be a session name, or `None` if it can.
///
/// The host does not validate names on the plugin create path. `validate_session_name` is
/// connected only to the CLI and the web client, so this function is the last check. The `.`,
/// `..` and whitespace-only rules come from the host validator.
///
/// An empty name is valid. On the create path it means `new_session_name = None`, and the host
/// names the session. The rename screen has no such fallback and rejects an empty name.
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
    // `has_forbidden_session` is not ported. It applies to web-client sessions, and this
    // configuration has no web server. Add it here if the web server is turned on.
    None
}

impl Row {
    /// `pub(crate)` for the render tests, which build a row without a host.
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
