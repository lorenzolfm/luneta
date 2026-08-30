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

use std::collections::BTreeMap;
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

/// The command behind the preview box: one entry per line, dotfiles included, `/` on the ones
/// that are directories.
///
/// The path is appended by the caller, behind a `--`, because a directory may be named `-l`.
///
/// ⚠️ Deliberately no `--group-directories-first`: that flag is GNU's, and the ordering it buys
/// is applied in [`DirSet::ingest_listing`] instead, off the `/` that `-p` already puts there.
/// The plugin cannot see whether the host's `ls` is GNU's, and a flag it may reject is a preview
/// that fails on someone else's machine for a reason it cannot explain.
pub const LIST: [&str; 2] = ["ls", "-1Ap"];

/// Marks our own `RunCommandResult` so an unrelated one cannot be parsed as a directory list.
pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "zoxide";
/// The second thing this module asks the host for. Same key, different value — one channel
/// carries both, and the reply says which by carrying the value back.
pub const PREVIEW_VALUE: &str = "preview";
/// Which directory a preview reply is about, carried out and back in the same context map.
///
/// 🔴 Not "whichever row is selected now": replies arrive whenever they arrive, and the cursor
/// has usually moved on. Without the path on the reply, a slow `ls` would file its answer under
/// the wrong directory — and the box would confidently show you the contents of somewhere else.
pub const PATH_KEY: &str = "luneta_path";

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

/// What the preview box knows about one directory.
pub enum Listing {
    /// `ls` has been asked and has not answered.
    Reading,
    Ready {
        /// Directories first, then files — see [`DirSet::ingest_listing`]. Capped at
        /// [`MAX_ENTRIES`]; `total` is what was actually there.
        entries: Vec<String>,
        total: usize,
    },
    /// The directory could not be read. Carries what to put on screen — a directory that is
    /// gone, or one you may not open, is a thing the preview box can say plainly.
    Failed(String),
}

/// The most entries kept for one directory.
///
/// A preview box is a few dozen rows at the very most, so anything past this could never be
/// drawn — and `node_modules` would otherwise put tens of thousands of strings in the cache to
/// show you the first thirty of them. The count of what was dropped survives as `total`.
const MAX_ENTRIES: usize = 128;

/// How many directories' listings are worth keeping.
///
/// Past this the cache is dropped **whole** rather than evicted one entry at a time. There is no
/// recency to evict by that would be worth the bookkeeping: the cost of being wrong is one `ls`
/// on a directory you come back to, and arrowing through sixty-four directories in one sitting
/// is not the case this cache exists for — holding the cursor still on a handful of them is.
const MAX_LISTINGS: usize = 64;

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
    /// What `ls` said about each directory the cursor has rested on, keyed by path.
    listings: BTreeMap<String, Listing>,
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

    /// Drop every listing. The picker is a glance rather than a watch, and a directory read
    /// during the last one is a claim about what was there minutes ago.
    pub fn forget_listings(&mut self) {
        self.listings.clear();
    }

    /// What the preview box has to show for `path`, if anything has been asked yet.
    pub fn listing(&self, path: &str) -> Option<&Listing> {
        self.listings.get(path)
    }

    /// Claim `path` for an `ls`, or refuse because there is nothing to ask.
    ///
    /// The claim *is* the [`Listing::Reading`] entry: a second caller sees it and is refused,
    /// which is what keeps one process per directory rather than one per tick. A failure stays
    /// in the map for the same reason — a directory that could not be read will not read any
    /// better on the next tick, and retrying it forever is how you fork `ls` ten times a second
    /// at a path that is gone.
    pub fn begin_listing(&mut self, path: &str) -> bool {
        if self.listings.contains_key(path) {
            return false;
        }
        if self.listings.len() >= MAX_LISTINGS {
            self.listings.clear();
        }
        self.listings.insert(path.to_string(), Listing::Reading);
        true
    }

    /// Take `ls`'s reply for one directory.
    ///
    /// Directories are floated to the top, in the order `ls` gave them, off the `/` that `-p`
    /// appends — which is why the flag is worth its column. A listing is read for its shape
    /// before it is read for its names, and a shape with the directories mixed into the files
    /// has to be read twice.
    pub fn ingest_listing(
        &mut self,
        path: String,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            // `ls` prefixes its own message with the path, which the box is already showing.
            let reason = reason
                .lines()
                .next()
                .unwrap_or("")
                .rsplit(": ")
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let reason = if reason.is_empty() { "cannot be read".to_string() } else { reason };
            self.listings.insert(path, Listing::Failed(reason));
            return;
        }
        let listed = String::from_utf8_lossy(stdout);
        let mut entries: Vec<String> = Vec::new();
        let mut files: Vec<String> = Vec::new();
        for line in listed.lines().map(str::trim_end).filter(|line| !line.is_empty()) {
            match line.ends_with('/') {
                true => entries.push(line.to_string()),
                false => files.push(line.to_string()),
            }
        }
        entries.append(&mut files);
        let total = entries.len();
        entries.truncate(MAX_ENTRIES);
        self.listings.insert(path, Listing::Ready { entries, total });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(stdout: &[u8]) -> DirSet {
        let mut dirs = DirSet::default();
        dirs.ingest_listing("/tmp/x".to_string(), Some(0), stdout, b"");
        dirs
    }

    fn entries(dirs: &DirSet) -> (Vec<String>, usize) {
        match dirs.listing("/tmp/x") {
            Some(Listing::Ready { entries, total }) => (entries.clone(), *total),
            _ => panic!("no listing"),
        }
    }

    /// Directories first, in the order `ls` gave them. A listing is read for its shape before
    /// it is read for its names, and one with the directories mixed in has to be read twice.
    #[test]
    fn a_listing_floats_the_directories_to_the_top() {
        let dirs = listed(b"Cargo.toml\nsrc/\nREADME.md\ntarget/\n");
        assert_eq!(entries(&dirs).0, ["src/", "target/", "Cargo.toml", "README.md"]);
    }

    /// Blank lines are not entries, and neither is the trailing newline.
    #[test]
    fn a_listing_of_nothing_is_empty_rather_than_one_blank_entry() {
        assert_eq!(entries(&listed(b"")), (Vec::new(), 0));
        assert_eq!(entries(&listed(b"\n")), (Vec::new(), 0));
    }

    /// `node_modules` is not a preview. What is kept is capped at what could ever be drawn;
    /// what was there is still counted, so the box can say how much it is not showing.
    #[test]
    fn a_long_listing_is_capped_but_still_counted() {
        let listing: String = (0..MAX_ENTRIES * 2).map(|i| format!("file-{}\n", i)).collect();
        let (entries, total) = entries(&listed(listing.as_bytes()));
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(total, MAX_ENTRIES * 2);
    }

    /// A failure keeps the reason and drops `ls`'s own path prefix — the box is already showing
    /// the path, directly above.
    #[test]
    fn a_failed_listing_keeps_the_reason_and_not_the_path() {
        let mut dirs = DirSet::default();
        dirs.ingest_listing(
            "/tmp/x".to_string(),
            Some(2),
            b"",
            b"ls: cannot open directory '/tmp/x': Permission denied\n",
        );
        match dirs.listing("/tmp/x") {
            Some(Listing::Failed(reason)) => assert_eq!(reason, "Permission denied"),
            _ => panic!("expected a failure"),
        }
        // A failure that said nothing still says something.
        dirs.ingest_listing("/tmp/y".to_string(), Some(2), b"", b"");
        match dirs.listing("/tmp/y") {
            Some(Listing::Failed(reason)) => assert_eq!(reason, "cannot be read"),
            _ => panic!("expected a failure"),
        }
    }

    /// The claim is the entry, so a second caller is refused — one `ls` per directory rather
    /// than one per tick. A failure holds its claim too: a path that could not be read will not
    /// read any better next tick, and retrying forks a process ten times a second.
    #[test]
    fn a_directory_is_only_ever_asked_about_once() {
        let mut dirs = DirSet::default();
        assert!(dirs.begin_listing("/tmp/x"));
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(0), b"src/\n", b"");
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(2), b"", b"ls: nope: No such file");
        assert!(!dirs.begin_listing("/tmp/x"));

        // Until the picker is opened again, at which point every listing is a claim about how
        // things were.
        dirs.forget_listings();
        assert!(dirs.begin_listing("/tmp/x"));
    }

    /// The cache is dropped whole rather than evicted one at a time — see [`MAX_LISTINGS`].
    #[test]
    fn a_full_cache_is_dropped_rather_than_grown() {
        let mut dirs = DirSet::default();
        for i in 0..MAX_LISTINGS {
            assert!(dirs.begin_listing(&format!("/tmp/{}", i)));
        }
        assert!(dirs.listing("/tmp/0").is_some());
        assert!(dirs.begin_listing("/tmp/one-too-many"));
        assert!(dirs.listing("/tmp/0").is_none());
        assert!(dirs.listing("/tmp/one-too-many").is_some());
    }
}
