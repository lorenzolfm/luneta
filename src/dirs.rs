//! The directories you go back to, and what `Enter` does with one of them.
//!
//! The rule of this screen:
//!
//! > A directory row is a cwd and a session name that nothing else holds. `Enter` creates that
//! > session, there.
//!
//! Holding a free name is what makes the cwd apply. `switch_session_with_cwd` carries the cwd
//! to `ClientInfo::set_cwd` (`zellij-client/src/lib.rs:526-532`), which matches `New` and
//! `Resurrect` and discards all else through a `_ => {}`. Give it the name of a live session
//! and you attach to that session, wherever it is, with no error and no cwd. This module
//! therefore never gives it one: a name the snapshot already holds takes a `-2`, and the row
//! shows the name you will get. The name is recomputed against every poll, so a session that
//! another client creates moves the row to the next postfix.
//!
//! The snapshot is all the plugin has. Neither `SessionInfo` nor `PaneInfo` has a cwd, so
//! nothing can ask a live session which directory it is in, and a name is the last component
//! of the path (see [`derive_name`]), which two directories can share. A session named after
//! this directory may thus be somewhere else. That is why the postfix and not an attach: the
//! row never addresses a session it cannot identify, it steps around the name.

use std::collections::BTreeMap;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::Cursor;
use crate::fetch::Fetch;
use crate::panes;
use crate::sessions::{validate_name, MAX_NAME_BYTES, Selection, Sessions};

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
    /// The name the session gets: [`derive_name`] of `path`, postfixed by [`free_name`] until
    /// the snapshot does not hold it.
    pub name: String,
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
    /// The rows and the cursor in them. See [`Cursor`]. Unlike the session screen, this screen
    /// cannot act on the typed text: it can only offer a directory zoxide knows, so an empty
    /// list here means `Enter` has nothing at all to do.
    pub rows: Cursor<DirRow>,
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
    /// Each rebuild needs the snapshot, because the snapshot decides the name of each row
    /// (see [`free_name`]). A session that another client creates must move the row that asked
    /// for its name on to the next postfix.
    pub fn rebuild(
        &mut self,
        term: &str,
        sessions: &Sessions,
        current: Option<&str>,
        policy: Selection,
    ) {
        let held = match policy {
            Selection::SnapToTop => None,
            Selection::Hold => self.selected_path().map(str::to_owned),
        };
        let mut rows: Vec<DirRow> = Vec::new();

        // Only an answer has directories to filter. The other two states have none, and an
        // empty list is what the screen draws for them.
        if let Fetch::Ready(all) = &self.status {
            if term.is_empty() {
                for dir in all {
                    let name = free_name(&dir.name, sessions, current);
                    rows.push(DirRow::new(dir, name, 0, vec![], false));
                }
            } else {
                let matcher = self
                    .matcher
                    .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
                for dir in all {
                    let name = free_name(&dir.name, sessions, current);
                    let Some((score, indices)) = match_dir(matcher, term, &dir.path, &name) else {
                        continue;
                    };
                    // Exact on either string. A term that names the postfix is exact on the
                    // row that carries it, and a term that names the directory stays exact on
                    // the row whose name the snapshot moved.
                    let is_exact = name == term || dir.name == term;
                    rows.push(DirRow::new(dir, name, score, indices, is_exact));
                }
            }
        }

        // Frecency decides ties in both branches, and decides everything when nothing is
        // typed. The scores fall quickly: 9268 at rank 1, 18 at rank 10, and 5.5 at rank 20 in
        // a real database. The top of the list is the answer, and the tail is there for the
        // filter to search.
        rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| b.frecency.total_cmp(&a.frecency))
        });

        // Held by path, not by index. A directory that the filter removes falls back to the
        // top. See [`Cursor::replace`].
        self.rows.replace(rows, |row| held.as_deref() == Some(row.path.as_str()));
    }

    pub fn selected_row(&self) -> Option<&DirRow> {
        self.rows.selected_row()
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_row().map(|r| r.path.as_str())
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
    fn new(dir: &Dir, name: String, score: i64, indices: Vec<usize>, is_exact: bool) -> Self {
        DirRow { path: dir.path.clone(), name, indices, score, frecency: dir.frecency, is_exact }
    }
}

/// Where `term` hits this row, and how well.
///
/// The path is the subject, because you remember the path and it is what you would type at
/// `z`. The name comes from the path, so a term that finds the name also finds the path — with
/// one exception, which is the whole reason this is a function: the postfix that [`free_name`]
/// adds is on the row and in the prompt and nowhere in the path. Without the second try, the
/// screen would show you `luneta-2` and then find nothing when you typed it.
///
/// The fallback highlights nothing. The indices of one string cannot be painted on another,
/// and the path is what the row draws.
fn match_dir(
    matcher: &SkimMatcherV2,
    term: &str,
    path: &str,
    name: &str,
) -> Option<(i64, Vec<usize>)> {
    match matcher.fuzzy_indices(path, term) {
        Some((score, indices)) => Some((score, indices)),
        None => matcher.fuzzy_match(name, term).map(|score| (score, Vec::new())),
    }
}

/// `base`, or the first `base-2`, `base-3`, … that no session in the snapshot answers to.
///
/// A taken name is not an error to report, it is a name to step past, because the host reads
/// one as an instruction to attach and drops the cwd with it (see the module doc). The current
/// session counts as taken and is tested apart from the two lists, because the poll removes it
/// from `live`.
///
/// Two rows can still propose one name. Each is computed against the snapshot alone, so two
/// directories ending in `master` both read `master` while neither exists. Only one of them
/// can be pressed — the picker closes on `Enter` — and the next opening sees the session the
/// first one made.
///
/// The loop ends. The snapshot is finite, every candidate differs from every other, and a name
/// no session holds is therefore reached within one step per session.
fn free_name(base: &str, sessions: &Sessions, current: Option<&str>) -> String {
    let taken = |name: &str| current == Some(name) || sessions.any_named(name);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| {
            let postfix = format!("-{}", n);
            // The postfix takes room from the base rather than from the limit. A name at the
            // limit is refused by the host, which only logs the refusal
            // (`zellij_exports.rs:2971-2977`), so a long directory loses its last characters
            // instead of losing the `Enter`.
            format!("{}{}", head(base, MAX_NAME_BYTES.saturating_sub(postfix.len())), postfix)
        })
        .find(|candidate| !taken(candidate))
        .expect("a finite snapshot cannot hold every postfix")
}

/// The first `bytes` bytes of `text`, cut back to a character boundary.
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

/// The name a directory asks for: its last path component. What it gets is [`free_name`] of
/// this.
///
/// The name is the directory itself, because that is what you would call the session. Two
/// directories can end in the same component, such as `bipa.git/master` and
/// `infra.git/master`, so this is a request and not the answer.
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
    use crate::elapsed::Age;
    use crate::sessions::Session;

    fn named(name: &str) -> Session {
        Session { name: name.to_string(), age: Age::ZERO }
    }

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
        dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
        assert_eq!(dirs.rows.len(), 1);

        dirs.fail("zoxide: gone");
        dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
        assert!(dirs.rows.is_empty());
        assert!(dirs.rows.selected().is_none());
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

    /// A name a session holds is stepped past, whichever list holds it, and a free name is
    /// left alone.
    #[test]
    fn a_taken_name_takes_the_next_postfix() {
        let sessions = Sessions {
            live: vec![named("thing")],
            dead: vec![named("other")],
        };

        assert_eq!(free_name("absent", &sessions, None), "absent");
        assert_eq!(free_name("thing", &sessions, None), "thing-2");
        // A saved layout holds a name as firmly as a running session: the host resolves both.
        assert_eq!(free_name("other", &sessions, None), "other-2");
    }

    /// The name a row shows is a term that finds it. The postfix is nowhere in the path, so
    /// without the second try in [`match_dir`] the screen would offer `luneta-2` and then
    /// answer an empty list when you typed it back.
    #[test]
    fn the_name_a_row_shows_is_a_term_that_finds_it() {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"9268 /home/lorenzo/Projects/misc/luneta\n", b"");
        let sessions = Sessions { live: vec![named("luneta")], dead: vec![] };

        dirs.rebuild("", &sessions, None, Selection::SnapToTop);
        let shown = dirs.selected_row().unwrap().name.clone();
        assert_eq!(shown, "luneta-2");

        // The path still finds it, and so does the name the row draws.
        for term in ["luneta", "luneta-2"] {
            dirs.rebuild(term, &sessions, None, Selection::SnapToTop);
            assert_eq!(dirs.selected_row().map(|r| r.name.as_str()), Some("luneta-2"), "{term}");
        }

        // The fallback is a second try, not a second chance: a term neither string holds still
        // matches nothing.
        dirs.rebuild("luneta-3", &sessions, None, Selection::SnapToTop);
        assert!(dirs.rows.is_empty());
    }

    /// The current session is not in either list — the poll takes it out of `live` — and it
    /// holds its name all the same.
    #[test]
    fn the_session_you_are_in_holds_its_name_too() {
        let sessions = Sessions::default();
        assert_eq!(free_name("here", &sessions, Some("here")), "here-2");
    }

    /// The postfix counts up until the snapshot runs out, so a directory you have opened three
    /// times is the fourth session and not a collision.
    #[test]
    fn the_postfix_counts_past_every_session_that_holds_one() {
        let sessions = Sessions {
            live: vec![named("thing"), named("thing-2")],
            dead: vec![named("thing-3")],
        };
        assert_eq!(free_name("thing", &sessions, None), "thing-4");
    }

    /// The postfix takes its room from the base. A name at the limit is refused by the host,
    /// which only logs the refusal, so the last characters go instead of the `Enter`.
    #[test]
    fn a_long_name_loses_its_tail_and_not_its_postfix() {
        let base = "d".repeat(MAX_NAME_BYTES);
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        let name = free_name(&base, &sessions, None);
        assert_eq!(name.len(), MAX_NAME_BYTES);
        assert!(name.ends_with("-2"));
        assert!(validate_name(&name).is_none());
    }

    /// The cut is a character boundary, so a name of multi-byte characters stays a string.
    ///
    /// The base is at the limit and the byte a `-2` leaves off at is inside a character, which
    /// is the case the walk in [`head`] exists for. A base of whole characters up to that byte
    /// would take the early return and test nothing.
    #[test]
    fn a_long_name_is_cut_between_characters() {
        // 1 + 35 * 3 + 1 bytes, so the last `の` straddles the byte a `-2` cuts at.
        let base = format!("a{}x", "の".repeat(35));
        assert_eq!(base.len(), MAX_NAME_BYTES);
        assert!(!base.is_char_boundary(MAX_NAME_BYTES - 2));
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        // The walk gives up the straddling character, so the name lands two bytes under the
        // limit rather than in the middle of a `の`.
        let name = free_name(&base, &sessions, None);
        assert_eq!(name, format!("a{}-2", "の".repeat(34)));
        assert!(name.len() < MAX_NAME_BYTES);
        assert!(validate_name(&name).is_none());
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
