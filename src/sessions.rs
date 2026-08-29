//! The match set: what the picker is showing, in the order it shows it.
//!
//! Two rules are load-bearing here and are *not* free to change:
//!
//! 1. **The current session leaves the match set**, filtered here rather than in the
//!    renderer. Upstream drops it with a `continue` in the render cache, which leaves the
//!    rendered list and the match set disagreeing about indices. Filtering here means
//!    **the rendered list *is* the match set** and the two can never diverge.
//! 2. **Filtering only ever removes rows, never re-groups them.** Live sessions always
//!    sort before resurrectable ones, at every stage. Upstream sorts score-first with type as a
//!    tiebreak, so its live and dead rows interleave as you type.

use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A live session. Handing its name to the host resolves to an attach.
    Live,
    /// A dead session with a saved layout. The same call resolves to a resurrect.
    Resurrectable,
}

impl Kind {
    pub fn full_tag(&self) -> &'static str {
        match self {
            Kind::Live => "[ATTACH]",
            Kind::Resurrectable => "[RESURRECT]",
        }
    }

    /// A floating pane can be as little as ~3 rows wide-open, so the narrow forms are not
    /// decoration — they are what keeps the tag on screen at all.
    pub fn abbr_tag(&self) -> &'static str {
        match self {
            Kind::Live => "[A]",
            Kind::Resurrectable => "[R]",
        }
    }
}

/// One row. This *is* one match-set entry — there is no separate render-side list.
pub struct Row {
    pub name: String,
    pub kind: Kind,
    /// Elapsed age as the host reports it, truncated to whole seconds on the way out
    /// (`screen.rs:3361`, `plugin_api/event.rs:1214`). Trusted as-is; the plugin sandbox
    /// cannot see the sockets it would need to compute anything better.
    pub age: Duration,
    /// Character positions the fuzzy matcher hit, for highlighting. Empty on an empty term.
    pub indices: Vec<usize>,
    score: i64,
    is_exact: bool,
}

/// What a rebuild should do with the cursor.
///
/// Shared with the directory screen rather than duplicated there: the two lists hold different
/// things and sort by different keys, but the cursor rule is the same one in both — a new term
/// snaps to the top match, a background refresh stays put.
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
    /// Always-on selection: an index into `rows`, or `None` only when `rows` is empty.
    pub selected: Option<usize>,
    /// The current session's name, kept only so the hint line can say the picker is hiding it.
    /// It is deliberately *not* in `rows`.
    pub current_session: Option<String>,
    /// Does the current session fuzzy-match the term? One extra `fuzzy_indices` call per
    /// keystroke against a name that never enters `rows` — the whole cost of the hint line.
    pub current_matches: bool,
    /// `Esc` dropped the selection, and it stays dropped until the term changes.
    /// This is the escape hatch: no highlight means `Enter` takes the literal text.
    dropped: bool,
    matcher: Option<SkimMatcherV2>,
}

impl MatchSet {
    /// Rebuild from the latest poll.
    ///
    /// The caller has already split the snapshot and dropped the current session from `live`
    /// (rule 1) — it keeps those lists so a keystroke can re-filter without waiting for the
    /// next poll.
    pub fn refresh(
        &mut self,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        current_session: Option<String>,
    ) {
        self.current_session = current_session;
        // Hold, not snap. This runs once a second in the background; snapping here would drag
        // the cursor back to row 0 under the user's fingers every poll.
        self.rebuild(live, dead, Selection::Hold);
    }

    fn rebuild(
        &mut self,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        policy: Selection,
    ) {
        let term = self.search_term.clone();
        // Recomputed below whenever the term is non-empty; an empty term reaches for nothing.
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
            // Empty term: live newest-first, then resurrectable newest-first. `age` is elapsed
            // time, so ascending age *is* newest-first.
            self.rows.sort_by(|a, b| a.kind_then_recency(b));
        } else {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
            // The current session is not a row and never will be, but the hint line has
            // to know whether the term is reaching for it — otherwise typing your own session's
            // name gives a blank list and no explanation.
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
            // Non-empty term: exact match, then live before resurrectable, then score, then
            // recency. Type comes *above* score deliberately — that is what stops the
            // live/dead boundary from moving as you type.
            self.rows.sort_by(|a, b| {
                b.is_exact
                    .cmp(&a.is_exact)
                    .then_with(|| a.kind_rank().cmp(&b.kind_rank()))
                    .then_with(|| b.score.cmp(&a.score))
                    .then_with(|| a.age.cmp(&b.age))
            });
        }

        self.selected = if self.rows.is_empty() || self.dropped {
            None
        } else {
            // `Hold` keeps the cursor on the same *session*, not the same row — the row may have
            // moved, and a session that vanished falls back to the top.
            held.and_then(|name| self.rows.iter().position(|r| r.name == name))
                .or(Some(0))
        };
    }

    pub fn selected_name(&self) -> Option<&str> {
        self.selected
            .and_then(|i| self.rows.get(i))
            .map(|r| r.name.as_str())
    }

    /// A new search term: fzf cursor discipline snaps back to the top match, because with
    /// a new term the top match *is* the answer by construction.
    pub fn set_search_term(
        &mut self,
        term: String,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
    ) {
        self.search_term = term;
        // A new term re-arms the selection: `Esc`'s drop lasts exactly until you type again.
        self.dropped = false;
        self.rebuild(live, dead, Selection::SnapToTop);
    }

    /// `Esc`'s escape hatch: drop the highlight so `Enter` means "the literal
    /// text I typed", not "the top match". Without this, always-on selection makes it impossible
    /// to create `infra` while `infra-staging` is live.
    ///
    /// The drop is sticky — a background poll must not put the highlight back a second later —
    /// and `set_search_term` is the only thing that lifts it.
    pub fn drop_selection(&mut self) {
        self.selected = None;
        self.dropped = true;
    }

    /// Is the term exactly the name of the session we are sitting in?
    ///
    /// That session is not in `rows`, so it can never be the highlight; this is the one
    /// place the fork still has to recognise it, to make `Enter` a no-op rather than an offer to
    /// create a session that already exists.
    pub fn is_own_name(&self) -> bool {
        !self.search_term.is_empty()
            && self.current_session.as_deref() == Some(self.search_term.as_str())
    }

    /// Why the *search term* cannot be a session name, or `None` if it can. See
    /// [`validate_name`], which the rename screen shares.
    pub fn name_error(&self) -> Option<&'static str> {
        validate_name(&self.search_term)
    }

    /// Move the cursor. **No wrap**: running off either end is a no-op, so the top match
    /// stays reachable by holding a key rather than by counting rows.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }
}

/// Why `name` cannot be a session name, or `None` if it can.
///
/// 🔴 The host does **not** validate on the plugin's create path — `validate_session_name` is
/// wired only to the CLI and the web client — so this is the last line of defence. Upstream's
/// plugin checks only the length and `/`; the `.`, `..` and whitespace-only rejections are
/// ported from the host's own validator.
///
/// An empty name is **valid** here: on the create path it means `new_session_name = None` and
/// the host names the session itself. Rename has no such fallback, so it rejects empty names
/// separately, before asking.
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
    // has_forbidden_session is deliberately *not* ported: it concerns web-client-forbidden
    // sessions, there is no web server in this config, and upstream applies it only on the
    // typed-name path anyway. Two lines to revert if the web server is ever turned on.
    None
}

impl Row {
    fn new(
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

/// Compact elapsed time: the column exists to make the sort order visible, so it wants one
/// glanceable magnitude, not humantime's `2days 3h 14m 2s`.
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
