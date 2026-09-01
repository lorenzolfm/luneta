use std::collections::BTreeMap;

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::Cursor;
use crate::fetch::Fetch;
use crate::panes;
use crate::sessions::{validate_name, MAX_NAME_BYTES, Selection, Sessions};

pub const QUERY: [&str; 4] = ["zoxide", "query", "--list", "--score"];

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

pub const CONTEXT_KEY: &str = "luneta";
pub const CONTEXT_VALUE: &str = "zoxide";
pub const PREVIEW_VALUE: &str = "preview";
pub const PATH_KEY: &str = "luneta_path";

pub enum Listing {
    Reading,
    Ready {
        entries: Vec<String>,
        total: usize,
    },
    Failed(String),
}

const MAX_ENTRIES: usize = 128;

const MAX_LISTINGS: usize = 64;

pub struct Dir {
    path: String,
    name: String,
    frecency: f64,
}

pub struct DirRow {
    pub path: String,
    pub name: String,
    pub indices: Vec<usize>,
    score: i64,
    frecency: f64,
    is_exact: bool,
}

#[derive(Default)]
pub struct DirSet {
    pub status: Fetch<Vec<Dir>>,
    pub rows: Cursor<DirRow>,
    pub asking: bool,
    listings: BTreeMap<String, Listing>,
    matcher: Option<SkimMatcherV2>,
}

impl DirSet {
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

    pub fn fail(&mut self, reason: impl Into<String>) {
        self.asking = false;
        self.status = Fetch::Failed(reason.into());
    }

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
                    let is_exact = name == term || dir.name == term;
                    rows.push(DirRow::new(dir, name, score, indices, is_exact));
                }
            }
        }

        rows.sort_by(|a, b| {
            b.is_exact
                .cmp(&a.is_exact)
                .then_with(|| b.score.cmp(&a.score))
                .then_with(|| b.frecency.total_cmp(&a.frecency))
        });

        self.rows.replace(rows, |row| held.as_deref() == Some(row.path.as_str()));
    }

    pub fn selected_row(&self) -> Option<&DirRow> {
        self.rows.selected_row()
    }

    fn selected_path(&self) -> Option<&str> {
        self.selected_row().map(|r| r.path.as_str())
    }

    pub fn forget_listings(&mut self) {
        self.listings.clear();
    }

    pub fn listing(&self, path: &str) -> Option<&Listing> {
        self.listings.get(path)
    }

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

fn listing_error(path: &str, stderr: &str) -> String {
    let line = stderr.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    let line = line.replace(path, "");
    let line = cut_at(&line, " - code: ");
    let line = cut_at(line, " (os error ");
    let line = line.trim_matches(|c: char| c.is_whitespace() || "\":-".contains(c));
    match line.is_empty() {
        true => "eza is not available".to_string(),
        false => line.to_string(),
    }
}

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

fn free_name(base: &str, sessions: &Sessions, current: Option<&str>) -> String {
    let taken = |name: &str| current == Some(name) || sessions.any_named(name);
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| {
            let postfix = format!("-{}", n);
            format!("{}{}", head(base, MAX_NAME_BYTES.saturating_sub(postfix.len())), postfix)
        })
        .find(|candidate| !taken(candidate))
        .expect("a finite snapshot cannot hold every postfix")
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

fn parse(stdout: &str) -> Vec<Dir> {
    stdout
        .lines()
        .filter_map(|line| {
            let (score, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let frecency: f64 = score.parse().ok()?;
            let path = rest.trim_start();
            if !path.starts_with('/') {
                return None;
            }
            let name = derive_name(path)?;
            Some(Dir { path: path.to_string(), name, frecency })
        })
        .collect()
}

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

    #[test]
    fn a_listing_keeps_the_order_eza_gave_it() {
        let dirs = listed(b"src/\ntarget/\nCargo.toml\nREADME.md\n");
        assert_eq!(entries(&dirs).0, ["src/", "target/", "Cargo.toml", "README.md"]);
    }

    #[test]
    fn a_listing_keeps_its_colours_and_nothing_else() {
        let dirs = listed(b"\x1b[34msrc\x1b[0m/\n\x1b[2J\x1b]0;title\x07Cargo.toml\n\x1b[H\n");
        assert_eq!(entries(&dirs).0, ["\x1b[34msrc\x1b[0m/", "Cargo.toml"]);
    }

    #[test]
    fn a_listing_of_nothing_is_empty_rather_than_one_blank_entry() {
        assert_eq!(entries(&listed(b"")), (Vec::new(), 0));
        assert_eq!(entries(&listed(b"\n")), (Vec::new(), 0));
    }

    #[test]
    fn a_long_listing_is_capped_but_still_counted() {
        let listing: String = (0..MAX_ENTRIES * 2).map(|i| format!("file-{}\n", i)).collect();
        let (entries, total) = entries(&listed(listing.as_bytes()));
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(total, MAX_ENTRIES * 2);
    }

    #[test]
    fn a_failed_listing_keeps_the_reason_and_not_the_path() {
        let mut dirs = DirSet::default();
        dirs.ingest_listing(
            "/tmp/x".to_string(),
            Some(2),
            b"",
            b"\"/tmp/x\": No such file or directory (os error 2)\n",
        );
        assert_eq!(failure(&dirs, "/tmp/x"), "No such file or directory");
        dirs.ingest_listing(
            "/tmp/y".to_string(),
            Some(0),
            b"",
            b"Permission denied: /tmp/y - code: 13\n",
        );
        assert_eq!(failure(&dirs, "/tmp/y"), "Permission denied");
        dirs.ingest_listing("/tmp/z".to_string(), Some(1), b"", b"");
        assert_eq!(failure(&dirs, "/tmp/z"), "eza is not available");
    }

    #[test]
    fn an_empty_directory_is_not_a_failure() {
        assert_eq!(entries(&listed(b"")), (Vec::new(), 0));
    }

    #[test]
    fn a_directory_is_only_ever_asked_about_once() {
        let mut dirs = DirSet::default();
        assert!(dirs.begin_listing("/tmp/x"));
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(0), b"src/\n", b"");
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_string(), Some(2), b"", b"\"/tmp/x\": No such file");
        assert!(!dirs.begin_listing("/tmp/x"));

        dirs.forget_listings();
        assert!(dirs.begin_listing("/tmp/x"));
    }

    #[test]
    fn a_name_is_the_directory_itself() {
        assert_eq!(derive_name("/home/you/Projects/misc/luneta").as_deref(), Some("luneta"));
        assert_eq!(derive_name("/home/you/Work/bipa.git/master").as_deref(), Some("master"));
        assert_eq!(derive_name("/opt").as_deref(), Some("opt"));
        assert_eq!(derive_name("/home/you/notes/").as_deref(), Some("notes"));
    }

    #[test]
    fn a_directory_without_a_usable_name_is_dropped() {
        assert_eq!(derive_name("/"), None);
        assert_eq!(derive_name(""), None);
        assert_eq!(derive_name(&format!("/home/{}", "d".repeat(108))), None);
    }

    #[test]
    fn a_taken_name_takes_the_next_postfix() {
        let sessions = Sessions {
            live: vec![named("thing")],
            dead: vec![named("other")],
        };

        assert_eq!(free_name("absent", &sessions, None), "absent");
        assert_eq!(free_name("thing", &sessions, None), "thing-2");
        assert_eq!(free_name("other", &sessions, None), "other-2");
    }

    #[test]
    fn the_name_a_row_shows_is_a_term_that_finds_it() {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"9268 /home/lorenzo/Projects/misc/luneta\n", b"");
        let sessions = Sessions { live: vec![named("luneta")], dead: vec![] };

        dirs.rebuild("", &sessions, None, Selection::SnapToTop);
        let shown = dirs.selected_row().unwrap().name.clone();
        assert_eq!(shown, "luneta-2");

        for term in ["luneta", "luneta-2"] {
            dirs.rebuild(term, &sessions, None, Selection::SnapToTop);
            assert_eq!(dirs.selected_row().map(|r| r.name.as_str()), Some("luneta-2"), "{term}");
        }

        dirs.rebuild("luneta-3", &sessions, None, Selection::SnapToTop);
        assert!(dirs.rows.is_empty());
    }

    #[test]
    fn the_session_you_are_in_holds_its_name_too() {
        let sessions = Sessions::default();
        assert_eq!(free_name("here", &sessions, Some("here")), "here-2");
    }

    #[test]
    fn the_postfix_counts_past_every_session_that_holds_one() {
        let sessions = Sessions {
            live: vec![named("thing"), named("thing-2")],
            dead: vec![named("thing-3")],
        };
        assert_eq!(free_name("thing", &sessions, None), "thing-4");
    }

    #[test]
    fn a_long_name_loses_its_tail_and_not_its_postfix() {
        let base = "d".repeat(MAX_NAME_BYTES);
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        let name = free_name(&base, &sessions, None);
        assert_eq!(name.len(), MAX_NAME_BYTES);
        assert!(name.ends_with("-2"));
        assert!(validate_name(&name).is_none());
    }

    #[test]
    fn a_long_name_is_cut_between_characters() {
        let base = format!("a{}x", "の".repeat(35));
        assert_eq!(base.len(), MAX_NAME_BYTES);
        assert!(!base.is_char_boundary(MAX_NAME_BYTES - 2));
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        let name = free_name(&base, &sessions, None);
        assert_eq!(name, format!("a{}-2", "の".repeat(34)));
        assert!(name.len() < MAX_NAME_BYTES);
        assert!(validate_name(&name).is_none());
    }

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
