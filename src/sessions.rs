//! The match set: the rows the picker shows, in the order it shows them.
//!
//! Two rules control this module:
//!
//! 1. The current session is removed here, not in the renderer. The rendered list is thus
//!    equal to the match set, and their indices cannot disagree.
//! 2. A filter only removes rows. It never regroups them: live sessions sort before
//!    resurrectable ones at every stage.

use std::collections::BTreeMap;
use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A live session. The host resolves its name to an attach.
    Live,
    /// A dead session with a saved layout. The host resolves its name to a resurrect.
    Resurrectable,
}

/// One row, which is also one match-set entry. There is no separate render-side list.
pub struct Row {
    pub name: String,
    pub kind: Kind,
    /// Elapsed age, as the host reports it. The host truncates it to whole seconds
    /// (`screen.rs:3361`, `plugin_api/event.rs:1214`). The plugin sandbox cannot see the
    /// sockets that a better value needs.
    pub age: Duration,
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
    pub rows: Vec<Row>,
    /// An index into `rows`. `None` only when `rows` is empty.
    pub selected: Option<usize>,
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
    /// keeps both lists, so that a keystroke can filter again before the next poll.
    pub fn refresh(
        &mut self,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        current_session: Option<String>,
    ) {
        self.current_session = current_session;
        // Hold, not snap: this runs once a second in the background, and a snap would move
        // the cursor back to row 0 on every poll.
        self.rebuild(live, dead, Selection::Hold);
    }

    fn rebuild(
        &mut self,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        policy: Selection,
    ) {
        let term = self.search_term.clone();
        // Computed below when the term is not empty. An empty term matches nothing.
        self.current_matches = false;
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_name().map(str::to_owned),
        };
        self.rows.clear();

        if term.is_empty() {
            for (name, age) in live {
                self.rows.push(Row::new(name.clone(), Kind::Live, *age, 0, vec![], false));
            }
            for (name, age) in dead {
                self.rows
                    .push(Row::new(name.clone(), Kind::Resurrectable, *age, 0, vec![], false));
            }
            // Live newest first, then resurrectable newest first. `age` is elapsed time, so
            // ascending age is newest first.
            self.rows.sort_by(|a, b| a.kind_then_recency(b));
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
            for (kind, list) in [(Kind::Live, live), (Kind::Resurrectable, dead)] {
                for (name, age) in list {
                    if let Some((score, indices)) = matcher.fuzzy_indices(name, &term) {
                        let is_exact = *name == term;
                        self.rows
                            .push(Row::new(name.clone(), kind, *age, score, indices, is_exact));
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
            self.rows.sort_by(|a, b| {
                a.kind_rank()
                    .cmp(&b.kind_rank())
                    .then_with(|| b.is_exact.cmp(&a.is_exact))
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.age.cmp(&b.age))
            });
        }

        // `None` only when there is nothing to point at. In that state `Enter` acts on the
        // text you typed, which is the create path. See [`crate::render::enter_action`].
        self.selected = if self.rows.is_empty() {
            None
        } else {
            // `Hold` keeps the cursor on the same session, not on the same index. A session
            // that is gone falls back to the top.
            held.and_then(|name| self.rows.iter().position(|r| r.name == name))
                .or(Some(0))
        };
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.selected
            .and_then(|i| self.rows.get(i))
            .map(|r| r.name.as_str())
    }

    /// Set a new search term. The cursor goes to the top match, which is the best answer to a
    /// term that has just changed.
    pub fn set_search_term(
        &mut self,
        term: String,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
    ) {
        self.search_term = term;
        self.rebuild(live, dead, Selection::SnapToTop);
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

    /// Move the cursor. The cursor stops at both ends and does not wrap, so that you can hold
    /// a key down to reach the top match.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }
}

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
    if name.len() >= 108 {
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
        age: Duration,
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

/// Elapsed time in one magnitude. The column shows the sort order, so `2h ago` is more useful
/// than `2days 3h 14m 2s`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    match secs {
        0..=59 => "<1m ago".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        86_400..=604_799 => format!("{}d ago", secs / 86_400),
        _ => format!("{}w ago", secs / 604_800),
    }
}
