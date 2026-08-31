//! The directories you go back to, and what `Enter` does with one of them.
//!
//! The rule of this screen:
//!
//! > A directory row is a proposed session name and a cwd. The cwd applies only if the name is
//! > free.
//!
//! The host makes that rule, not this module. `switch_session_with_cwd` carries the cwd to
//! `ClientInfo::set_cwd` (`zellij-client/src/lib.rs:526-532`), which matches `New` and
//! `Resurrect` and discards all else through a `_ => {}`. Give it the name of a live session
//! and you attach to that session, wherever it is, with no error and no cwd. [`Action`] is
//! therefore computed from the snapshot that builds the session screen, and it names the
//! outcome the host will choose.
//!
//! The plugin cannot verify that outcome. Neither `SessionInfo` nor `PaneInfo` has a cwd, so
//! nothing can ask a live session which directory it is in. `Attach to` means that a session of
//! this name exists, not that it is in this directory. A name is the last component of the
//! path (see [`derive_name`]), and two directories can end in the same component, so an attach
//! can go to a session that was created somewhere else.

use std::collections::BTreeMap;
use std::time::Duration;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::fetch::Fetch;
use crate::panes;
use crate::sessions::{validate_name, Selection};

/// The command behind this screen. `-l` lists, and `-s` prints the frecency score.
///
/// `-a` is omitted on purpose. Without it, zoxide removes directories that no longer exist. The
/// plugin cannot do that itself, because its wasi sandbox opens only `/host`, `/data`, `/cache`
/// and `/tmp` and cannot stat any other path.
pub const QUERY: [&str; 4] = ["zoxide", "query", "--list", "--score"];

/// The command behind the preview box.
///
/// `eza` draws the listing and the picker prints what it drew, so an entry keeps its own colour
/// and its own icon. The box thus shows a directory the way your shell shows it. Those bytes
/// come from another program and cross the same border a pane screen crosses: only colour
/// passes. See [`crate::panes::sgr_only`].
///
/// The caller adds the path after a `--`, because a directory can be named `-l`.
///
/// Each flag is `=always` because the plugin is not a terminal, and eza turns colour and its
/// classify marks off when it writes to a pipe. `--group-directories-first` is a flag of eza,
/// so this module does not sort. `--icons=always` needs a Nerd Font: remove it if the terminal
/// has none.
pub const LIST: [&str; 8] = [
    "eza",
    "--oneline",
    "--almost-all",
    "--group-directories-first",
    "--classify=always",
    "--color=always",
    "--icons=always",
    "--no-quotes",
];

/// Marks our own `RunCommandResult` so an unrelated one cannot be parsed as a directory list.
pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "zoxide";
/// The second command this module sends. It uses the same key with a different value, and the
/// reply returns the value that identifies it.
pub const PREVIEW_VALUE: &str = "preview";
/// Which directory a preview reply is about, sent with the command and returned with the answer.
///
/// The selected row cannot answer this. A reply arrives at any time, and the cursor has usually
/// moved. Without the path on the reply, a slow reply would be filed under the wrong
/// directory, and the box would show the contents of a different place.
pub const PATH_KEY: &str = "luneta_path";

/// What `Enter` on this row does. The host decides it, and this reports it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// The derived name is free. The session is created, in this directory.
    Create,
    /// The derived name is a live session: the host attaches to it and **ignores the cwd**.
    Attach,
    /// The derived name has a saved layout: the host resurrects it.
    Resurrect,
    /// The derived name is the current session. `Enter` is refused, because the client calls
    /// `panic!` when it is asked to attach to itself (`commands.rs:794`).
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

    /// Only `Create` can carry the cwd. The host discards or replaces it in the other cases,
    /// and a discarded argument makes you believe a session is somewhere it is not.
    pub fn carries_cwd(&self) -> bool {
        matches!(self, Action::Create)
    }
}

/// What the preview box knows about one directory.
pub enum Listing {
    /// eza has been asked and has not answered.
    Reading,
    Ready {
        /// The lines of eza, in its order and with its colours. The list stops at
        /// [`MAX_ENTRIES`], and `total` is how many entries there were.
        entries: Vec<String>,
        total: usize,
    },
    /// The directory could not be read. It carries the text for the screen.
    Failed(String),
}

/// The maximum number of entries kept for one directory.
///
/// The box holds a few dozen rows, so more entries cannot be drawn. Without this, a
/// `node_modules` directory would put tens of thousands of strings in the cache. `total` keeps
/// the true count.
const MAX_ENTRIES: usize = 128;

/// How many directory listings to keep.
///
/// Above this the cache is cleared, and no entry is evicted on its own. The cost of a wrong
/// decision is one listing of a directory you return to. The cache exists for a few directories
/// that you stop on, not for sixty-four that you pass.
const MAX_LISTINGS: usize = 64;

/// One directory out of zoxide, with the session name it would be given.
pub struct Dir {
    path: String,
    name: String,
    frecency: f64,
}

/// One row, which is also one match-set entry, as on the session screen.
pub struct DirRow {
    /// Absolute, and handed to the host verbatim as the new session's cwd.
    pub path: String,
    /// [`derive_name`] of `path`: the name the session gets.
    pub name: String,
    pub action: Action,
    /// Character positions the fuzzy matcher hit **in `path`**, for highlighting.
    pub indices: Vec<usize>,
    score: i64,
    frecency: f64,
    is_exact: bool,
}

#[derive(Default)]
pub struct DirSet {
    /// Why the list is empty, when it is, and the directories when it is not. See [`Fetch`].
    pub status: Fetch<Vec<Dir>>,
    pub rows: Vec<DirRow>,
    /// An index into `rows`. `None` only when `rows` is empty. Unlike the session screen, this
    /// screen cannot act on the typed text: it can only offer a directory zoxide knows.
    pub selected: Option<usize>,
    /// True between the question to zoxide and its answer, so that a re-focus starts no second
    /// process.
    pub asking: bool,
    /// What eza said about each directory the cursor has stopped on, keyed by path.
    listings: BTreeMap<String, Listing>,
    matcher: Option<SkimMatcherV2>,
}

impl DirSet {
    /// Take the reply from zoxide. Any exit other than 0 becomes a [`Fetch::Failed`] with the
    /// reason, because the most probable failure is that zoxide is not installed. Without the
    /// reason, that looks the same as an empty database.
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.fail(if reason.is_empty() {
                "zoxide is not available".to_string()
            } else {
                format!("zoxide: {}", reason)
            });
            return;
        }
        self.asking = false;
        self.status = Fetch::Ready(parse(&String::from_utf8_lossy(stdout)));
    }

    /// The one way to a failure, from here and from the server. It takes the directories with
    /// it, because [`Fetch::Failed`] has nowhere to keep them.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Fetch::Failed(reason.into());
    }

    /// Rebuild against the term and the latest session snapshot.
    ///
    /// Each rebuild needs the snapshot, because the snapshot decides the [`Action`] of each
    /// row. A session that another client creates must change a row from create to attach.
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

        // Only an answer has directories to filter. The other two states have none, and an
        // empty list is what the screen draws for them.
        if let Fetch::Ready(all) = &self.status {
            if term.is_empty() {
                for dir in all {
                    self.rows.push(DirRow::new(dir, live, dead, current, 0, vec![], false));
                }
            } else {
                let matcher = self
                    .matcher
                    .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
                for dir in all {
                    // Matched against the path, not the derived name. You remember the path,
                    // and it is what you would type at `z`. The name comes from the path, so a
                    // term that finds the name also finds the path.
                    if let Some((score, indices)) = matcher.fuzzy_indices(&dir.path, term) {
                        let is_exact = dir.name == term;
                        self.rows
                            .push(DirRow::new(dir, live, dead, current, score, indices, is_exact));
                    }
                }
            }
        }

        // Frecency decides ties in both branches, and decides everything when nothing is
        // typed. The scores fall quickly: 9268 at rank 1, 18 at rank 10, and 5.5 at rank 20 in
        // a real database. The top of the list is the answer, and the tail is there for the
        // filter to search.
        self.rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| b.frecency.total_cmp(&a.frecency))
        });

        self.selected = if self.rows.is_empty() {
            None
        } else {
            // Held by path, not by index. A directory that the filter removes falls back to
            // the top.
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

    /// Move the cursor. The cursor stops at both ends and does not wrap, as on the session
    /// screen.
    pub fn move_selection(&mut self, delta: isize) {
        let Some(current) = self.selected else { return };
        let last = self.rows.len().saturating_sub(1);
        let next = (current as isize + delta).clamp(0, last as isize) as usize;
        self.selected = Some(next);
    }

    /// Clear every listing. A listing read during the last opening describes what was there
    /// minutes ago.
    pub fn forget_listings(&mut self) {
        self.listings.clear();
    }

    /// What the preview box has to show for `path`, if anything has been asked yet.
    pub fn listing(&self, path: &str) -> Option<&Listing> {
        self.listings.get(path)
    }

    /// Claim `path` for a listing, or refuse because there is nothing to ask.
    ///
    /// The [`Listing::Reading`] entry is the claim, so a second caller is refused. This gives
    /// one process per directory, not one per tick. A failure keeps its entry, because a
    /// directory that cannot be read will fail again. Without that, eza would run ten times a
    /// second on a path that is gone.
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

    /// Take the reply from eza for one directory.
    ///
    /// The lines are kept in the order eza gave them, because `--group-directories-first` has
    /// already put the directories at the top. Each line keeps its colours and loses every
    /// other escape, which is what makes it safe to print.
    ///
    /// A failure is not always a non-zero exit. eza reports a directory it may not open on
    /// stderr and still exits 0, so an empty listing with a message beside it is a failure and
    /// not an empty directory. A directory that is genuinely empty writes nothing at all.
    pub fn ingest_listing(
        &mut self,
        path: String,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) {
        let listed = String::from_utf8_lossy(stdout);
        let complained = !String::from_utf8_lossy(stderr).trim().is_empty();
        if exit_code != Some(0) || (listed.trim().is_empty() && complained) {
            let reason = listing_error(&path, &String::from_utf8_lossy(stderr));
            self.listings.insert(path, Listing::Failed(reason));
            return;
        }
        let mut entries: Vec<String> = listed
            .lines()
            .map(panes::sgr_only)
            .filter(|line| panes::columns(line) > 0)
            .collect();
        let total = entries.len();
        entries.truncate(MAX_ENTRIES);
        self.listings.insert(path, Listing::Ready { entries, total });
    }
}

/// Why a listing failed, in the fewest words that stay true.
///
/// eza writes prose to stderr, and the shape of that prose is not part of any interface. This
/// keeps the first line and removes three things from it: the path, which the box already shows
/// on the line above; the ` - code: N` that eza appends to a permission error; and the
/// ` (os error N)` that it appends to a missing directory. What is left of
/// `"/nope": No such file or directory (os error 2)` is `No such file or directory`, and what is
/// left of `Permission denied: /nope - code: 13` is `Permission denied`.
///
/// A line this leaves empty means that eza said nothing, which it does when it did not run.
fn listing_error(path: &str, stderr: &str) -> String {
    let line = stderr.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    let line = line.replace(path, "");
    let line = cut_at(&line, " - code: ");
    let line = cut_at(line, " (os error ");
    // The removals above leave the quotes and separators that held the path.
    let line = line.trim_matches(|c: char| c.is_whitespace() || "\":-".contains(c));
    match line.is_empty() {
        true => "eza is not available".to_string(),
        false => line.to_string(),
    }
}

/// `text` up to `marker`, or all of `text` when it has none.
fn cut_at<'a>(text: &'a str, marker: &str) -> &'a str {
    match text.rfind(marker) {
        Some(at) => &text[..at],
        None => text,
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

/// Which of the three host outcomes this name gives.
///
/// The order is the order the host uses: a live session wins over a saved layout. The current
/// session is tested first, because the poll removes it from `live`.
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

/// `  9264.0 /home/you/Projects/thing`: the score in a fixed field aligned right, one space,
/// then the path, which can contain spaces.
fn parse(stdout: &str) -> Vec<Dir> {
    stdout
        .lines()
        .filter_map(|line| {
            let (score, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let frecency: f64 = score.parse().ok()?;
            let path = rest.trim_start();
            // Absolute paths only. This string becomes the cwd of the new session. The host
            // passes an absolute path through, but resolves a relative path against the cwd of
            // the plugin (`zellij_exports.rs:151-153`).
            if !path.starts_with('/') {
                return None;
            }
            let name = derive_name(path)?;
            Some(Dir { path: path.to_string(), name, frecency })
        })
        .collect()
}

/// The session name for a directory: its last path component.
///
/// The name is the directory itself, because that is what you would call the session. Two
/// directories can end in the same component, such as `bipa.git/master` and
/// `infra.git/master`. Both rows then propose one name, and the second row becomes an attach
/// to the session that the first row created. The module doc says why the plugin cannot detect
/// that.
///
/// The result cannot contain a `/`. This matters twice: the host refuses such a name
/// (`zellij_exports.rs:2971-2977`, where it only logs the refusal), and so does
/// [`validate_name`].
///
/// `None` for the root directory, which has no component, and for a component that
/// [`validate_name`] refuses. Such a directory leaves the list.
fn derive_name(path: &str) -> Option<String> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    validate_name(base).is_none().then(|| base.to_string())
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

    fn failure(dirs: &DirSet, path: &str) -> String {
        match dirs.listing(path) {
            Some(Listing::Failed(reason)) => reason.clone(),
            _ => panic!("expected a failure"),
        }
    }

    /// A failure takes the directories with it. The rows of the last answer cannot outlive it,
    /// because [`Fetch::Failed`] has nowhere to keep them.
    #[test]
    fn a_failure_leaves_no_directories_behind() {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"  9264.0 /home/you/Projects/thing\n", b"");
        dirs.rebuild("", &[], &[], None, Selection::SnapToTop);
        assert_eq!(dirs.rows.len(), 1);

        dirs.fail("zoxide: gone");
        dirs.rebuild("", &[], &[], None, Selection::SnapToTop);
        assert!(dirs.rows.is_empty());
        assert!(dirs.selected.is_none());
    }

    /// The order of eza is kept, because `--group-directories-first` already applied it.
    #[test]
    fn a_listing_keeps_the_order_eza_gave_it() {
        let dirs = listed(b"src/\ntarget/\nCargo.toml\nREADME.md\n");
        assert_eq!(entries(&dirs).0, ["src/", "target/", "Cargo.toml", "README.md"]);
    }

    /// Colour passes and every other escape is dropped, as it is for a pane. A line that is
    /// escapes alone takes no columns and is not an entry.
    #[test]
    fn a_listing_keeps_its_colours_and_nothing_else() {
        let dirs = listed(b"\x1b[34msrc\x1b[0m/\n\x1b[2J\x1b]0;title\x07Cargo.toml\n\x1b[H\n");
        assert_eq!(entries(&dirs).0, ["\x1b[34msrc\x1b[0m/", "Cargo.toml"]);
    }

    /// A blank line is not an entry, and neither is the final newline.
    #[test]
    fn a_listing_of_nothing_is_empty_rather_than_one_blank_entry() {
        assert_eq!(entries(&listed(b"")), (Vec::new(), 0));
        assert_eq!(entries(&listed(b"\n")), (Vec::new(), 0));
    }

    /// The list stops at what the box can draw, but the count stays complete, so that the box
    /// can say how many entries it does not show.
    #[test]
    fn a_long_listing_is_capped_but_still_counted() {
        let listing: String = (0..MAX_ENTRIES * 2).map(|i| format!("file-{}\n", i)).collect();
        let (entries, total) = entries(&listed(listing.as_bytes()));
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(total, MAX_ENTRIES * 2);
    }

    /// eza writes prose, and the box keeps the part of it that names the failure.
    #[test]
    fn a_failed_listing_keeps_the_reason_and_not_the_path() {
        let mut dirs = DirSet::default();
        // A directory that is gone: a non-zero exit, with the path in quotes.
        dirs.ingest_listing(
            "/tmp/x".to_string(),
            Some(2),
            b"",
            b"\"/tmp/x\": No such file or directory (os error 2)\n",
        );
        assert_eq!(failure(&dirs, "/tmp/x"), "No such file or directory");
        // A directory it may not open: eza exits 0 and writes to stderr.
        dirs.ingest_listing(
            "/tmp/y".to_string(),
            Some(0),
            b"",
            b"Permission denied: /tmp/y - code: 13\n",
        );
        assert_eq!(failure(&dirs, "/tmp/y"), "Permission denied");
        // Nothing on either channel means that eza did not run.
        dirs.ingest_listing("/tmp/z".to_string(), Some(1), b"", b"");
        assert_eq!(failure(&dirs, "/tmp/z"), "eza is not available");
    }

    /// An empty directory writes nothing on either channel, so it is empty and not a failure.
    /// Only a message beside an empty listing makes it a failure.
    #[test]
    fn an_empty_directory_is_not_a_failure() {
        assert_eq!(entries(&listed(b"")), (Vec::new(), 0));
    }

    /// The entry is the claim, so a second caller is refused: one listing per directory, not one
    /// per tick. A failure keeps its claim, because the path will fail again.
    #[test]
    fn a_directory_is_only_ever_asked_about_once() {
        let mut dirs = DirSet::default();
        assert!(dirs.begin_listing("/tmp/x"));
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(0), b"src/\n", b"");
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(2), b"", b"\"/tmp/x\": No such file");
        assert!(!dirs.begin_listing("/tmp/x"));

        // Until the picker becomes visible again, when every listing is out of date.
        dirs.forget_listings();
        assert!(dirs.begin_listing("/tmp/x"));
    }

    /// A name is the last component of the path, and nothing before it.
    #[test]
    fn a_name_is_the_directory_itself() {
        assert_eq!(derive_name("/home/you/Projects/misc/luneta").as_deref(), Some("luneta"));
        assert_eq!(derive_name("/home/you/Work/bipa.git/master").as_deref(), Some("master"));
        assert_eq!(derive_name("/opt").as_deref(), Some("opt"));
        // A final slash is not a component.
        assert_eq!(derive_name("/home/you/notes/").as_deref(), Some("notes"));
    }

    /// The root has no component, and a name the host refuses leaves the list.
    #[test]
    fn a_directory_without_a_usable_name_is_dropped() {
        assert_eq!(derive_name("/"), None);
        assert_eq!(derive_name(""), None);
        assert_eq!(derive_name(&format!("/home/{}", "d".repeat(108))), None);
    }

    /// The cache is cleared, and no entry is evicted on its own. See [`MAX_LISTINGS`].
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
