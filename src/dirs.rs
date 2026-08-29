//! The places you keep going back to, and what `Enter` on one of them means.
//!
//! The rule that makes this screen work is the session screen's rule, pointed at a different
//! question:
//!
//! > **A directory row is a proposed session name plus a cwd, and the cwd only takes effect if
//! > that name is free.**
//!
//! 🔴 That is not a policy chosen here — it is what the host does. `switch_session_with_cwd`
//! carries the cwd as far as `ClientInfo::set_cwd` (`zellij-client/src/lib.rs:526-532`), which
//! matches `New` and `Resurrect` and drops everything else through a `_ => {}`. Hand it the
//! name of a *live* session and you attach to that session, wherever it happens to be, with no
//! error and no cwd. So [`Action`] is computed against the same snapshot the session screen is
//! built from, and the tag says which of the three the host will pick — exactly as a session
//! row's tag does.
//!
//! 🔴 The plugin cannot verify that claim. `SessionInfo` has no cwd and neither does
//! `PaneInfo`, so there is no way to ask a live session which directory it is in. `[ATTACH]`
//! here means *"a session by this name exists"*, not *"that session is in this directory"* —
//! which is only ever as good as [`derive_name`] is unique. Across a real 136-path zoxide
//! database the last-two-components form collides zero times and the bare basename collides
//! nine ways (`master`, `backend`, `frontend`, `bin`, `nixos`, `skills`, …), which is exactly
//! why the name is not just the basename.

use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::sessions::{validate_name, Selection};

/// The command behind this screen. `-l` lists, `-s` prints the frecency score.
///
/// Deliberately **not** `-a`: without it zoxide omits directories that no longer exist, which
/// is the one bit of staleness filtering the plugin could not do for itself — its wasi sandbox
/// preopens only `/host`, `/data`, `/cache` and `/tmp`, so it cannot stat an arbitrary path.
pub const QUERY: [&str; 4] = ["zoxide", "query", "--list", "--score"];

/// Marks our own `RunCommandResult` so an unrelated one cannot be parsed as a directory list.
pub const CONTEXT_KEY: &str = "zj-picker";
pub const CONTEXT_VALUE: &str = "zoxide";

/// What `Enter` on this row will do — decided by the host, reported here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The derived name is free. The session is created, in this directory.
    Create,
    /// The derived name is a live session: the host attaches to it and **ignores the cwd**.
    Attach,
    /// The derived name has a saved layout: the host resurrects it.
    Resurrect,
    /// The derived name is the session you are already in. `Enter` is refused here, and the
    /// refusal is not cosmetic: the client `panic!`s on being asked to attach to itself
    /// (`commands.rs:794`) rather than declining.
    Here,
}

impl Action {
    pub fn full_tag(&self) -> &'static str {
        match self {
            Action::Create => "[CREATE]",
            Action::Attach => "[ATTACH]",
            Action::Resurrect => "[RESURRECT]",
            Action::Here => "[HERE]",
        }
    }

    /// The narrow forms, for the same reason the session screen has them: a floating pane can
    /// be a few columns wider than the names it is holding.
    pub fn abbr_tag(&self) -> &'static str {
        match self {
            Action::Create => "[C]",
            Action::Attach => "[A]",
            Action::Resurrect => "[R]",
            Action::Here => "[·]",
        }
    }

    /// The same thing spelled for the prompt line.
    pub fn verb(&self) -> &'static str {
        match self {
            Action::Create => "Create",
            Action::Attach => "Attach to",
            Action::Resurrect => "Resurrect",
            Action::Here => "already in",
        }
    }

    /// Only `Create` may carry the cwd. The other two would have it dropped or overridden by
    /// the host, and passing an argument that is silently discarded is how you end up believing
    /// a session is somewhere it is not.
    pub fn carries_cwd(&self) -> bool {
        matches!(self, Action::Create)
    }
}

/// One directory out of zoxide, with the session name it would be given.
struct Dir {
    path: String,
    name: String,
    frecency: f64,
}

/// One row. As on the session screen, this *is* one match-set entry.
pub struct DirRow {
    /// Absolute, and handed to the host verbatim as the new session's cwd.
    pub path: String,
    /// [`derive_name`] of `path` — the name the session would get.
    pub name: String,
    pub action: Action,
    /// Character positions the fuzzy matcher hit **in `path`**, for highlighting.
    pub indices: Vec<usize>,
    score: i64,
    frecency: f64,
    is_exact: bool,
}

/// Why the directory list is not showing anything, when it is not showing anything.
///
/// The screen has three ways to be empty and they are not interchangeable: still waiting,
/// zoxide is not there, and zoxide has nothing. Collapsing them into a blank list is what turns
/// a missing binary into "I guess this feature doesn't work".
#[derive(Default)]
pub enum Status {
    /// The permission has not come back yet, or zoxide has not answered.
    #[default]
    Waiting,
    Ready,
    /// zoxide could not be run or did not succeed. Carries what to put on screen.
    Failed(String),
}

#[derive(Default)]
pub struct DirSet {
    pub status: Status,
    pub rows: Vec<DirRow>,
    /// Always-on selection, `None` only when `rows` is empty. Same discipline as the session
    /// screen — there is no "typed text" escape hatch here, because a directory you have never
    /// been to is not a thing this screen can offer.
    pub selected: Option<usize>,
    /// True between asking zoxide and hearing back, so a re-focus cannot pile up processes.
    pub asking: bool,
    all: Vec<Dir>,
    matcher: Option<SkimMatcherV2>,
}

impl DirSet {
    /// Take zoxide's reply. Anything other than a clean exit becomes a [`Status::Failed`] with
    /// the reason on it, because the most likely failure by far — zoxide not installed — is
    /// indistinguishable from "you have never been anywhere" if it is swallowed.
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        self.asking = false;
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.status = Status::Failed(if reason.is_empty() {
                "zoxide is not available".to_string()
            } else {
                format!("zoxide: {}", reason)
            });
            self.all.clear();
            return;
        }
        self.all = parse(&String::from_utf8_lossy(stdout));
        self.status = Status::Ready;
    }

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Status::Failed(reason.into());
        self.all.clear();
    }

    /// Rebuild against the term and the latest session snapshot.
    ///
    /// The snapshot is needed on every rebuild, not just when it changes: it is what decides
    /// each row's [`Action`], so a session created elsewhere has to be able to turn a
    /// `[CREATE]` row into an `[ATTACH]` one under the cursor.
    pub fn rebuild(
        &mut self,
        term: &str,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        current: Option<&str>,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_path().map(str::to_owned),
        };
        self.rows.clear();

        if term.is_empty() {
            for dir in &self.all {
                self.rows.push(DirRow::new(dir, live, dead, current, 0, vec![], false));
            }
        } else {
            let matcher = self
                .matcher
                .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
            for dir in &self.all {
                // Matched against the **path**, not the derived name: the path is what you
                // remember and what you would have typed at `z`. The name is a consequence of
                // the path, so anything that reaches the name reaches the path too.
                if let Some((score, indices)) = matcher.fuzzy_indices(&dir.path, term) {
                    let is_exact = dir.name == term;
                    self.rows
                        .push(DirRow::new(dir, live, dead, current, score, indices, is_exact));
                }
            }
        }

        // Frecency is the last word in both branches, and the only word when nothing is typed:
        // it is the whole reason this screen exists, and it is what the scores fall off — 9268
        // at rank 1, 18 at rank 10, 5.5 at rank 20 in a real database. The top of this list is
        // the answer; the tail is there so the filter has something to find.
        self.rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| b.frecency.total_cmp(&a.frecency))
        });

        self.selected = if self.rows.is_empty() {
            None
        } else {
            // Held by *path*, not by row index: the row may have moved, and a directory that
            // fell out of the filter falls back to the top.
            held.and_then(|path| self.rows.iter().position(|r| r.path == path))
                .or(Some(0))
        };
    }

    pub fn selected_row(&self) -> Option<&DirRow> {
        self.selected.and_then(|i| self.rows.get(i))
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_row().map(|r| r.path.as_str())
    }

    /// Move the cursor. **No wrap**, for the session screen's reason: the top row stays
    /// reachable by holding a key rather than by counting.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }
}

impl DirRow {
    fn new(
        dir: &Dir,
        live: &[(String, Duration)],
        dead: &[(String, Duration)],
        current: Option<&str>,
        score: i64,
        indices: Vec<usize>,
        is_exact: bool,
    ) -> Self {
        DirRow {
            path: dir.path.clone(),
            action: action_for(&dir.name, live, dead, current),
            name: dir.name.clone(),
            indices,
            score,
            frecency: dir.frecency,
            is_exact,
        }
    }
}

/// Which of the host's three outcomes this name resolves to.
///
/// The order matters and mirrors the host's: live wins over a saved layout, and the session you
/// are in is checked first because it is not in `live` at all — the poll drops it at the source.
fn action_for(
    name: &str,
    live: &[(String, Duration)],
    dead: &[(String, Duration)],
    current: Option<&str>,
) -> Action {
    if current == Some(name) {
        Action::Here
    } else if live.iter().any(|(n, _)| n == name) {
        Action::Attach
    } else if dead.iter().any(|(n, _)| n == name) {
        Action::Resurrect
    } else {
        Action::Create
    }
}

/// `  9264.0 /home/you/Projects/thing` — score right-aligned in a fixed field, one space, then
/// the path, which may itself contain spaces.
fn parse(stdout: &str) -> Vec<Dir> {
    stdout
        .lines()
        .filter_map(|line| {
            let (score, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let frecency: f64 = score.parse().ok()?;
            let path = rest.trim_start();
            // Absolute only. This string becomes the new session's cwd verbatim — the host
            // passes an absolute path straight through, but resolves a relative one against
            // the *plugin's* cwd (`zellij_exports.rs:151-153`), which is not where the user
            // thinks they are going.
            if !path.starts_with('/') {
                return None;
            }
            let name = derive_name(path)?;
            Some(Dir { path: path.to_string(), name, frecency })
        })
        .collect()
}

/// The session name a directory gets: its last two path components, joined with `-`.
///
/// Two, not one, and the reason is measurable rather than aesthetic — see this module's header.
/// A basename collides constantly in a real database (`bipa.git/master` and `infra.git/master`
/// are both ordinary things to have visited); the two-component form did not collide once.
///
/// `/` is impossible by construction here, which matters twice: the host refuses a session name
/// containing one (`zellij_exports.rs:2971-2977`, where it only logs), and so does
/// [`validate_name`].
fn derive_name(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let joined = match parts.as_slice() {
        // `/` itself. There is no name to make, and no reason to want one.
        [] => return None,
        [only] => (*only).to_string(),
        [.., parent, base] => format!("{}-{}", parent, base),
    };
    if validate_name(&joined).is_none() {
        return Some(joined);
    }
    // Only the 108-byte limit can realistically land here, and only for deep paths. Falling
    // back to the basename keeps such a directory reachable at the cost of the disambiguation
    // — better than dropping it from the list with no explanation.
    let base = (*parts.last()?).to_string();
    validate_name(&base).is_none().then_some(base)
}
