use std::collections::{BTreeMap, HashMap};

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

use crate::cursor::Cursor;
use crate::fetch::Fetch;
use crate::panes;
use crate::sessions::{validate_name, MAX_NAME_BYTES, Selection, Sessions, Taken};

pub const QUERY: [&str; 4] = ["zoxide", "query", "--list", "--score"];

/// Prints, of the directories that follow it, the linked git worktrees: those
/// whose `.git` is a file naming a git directory that holds a `commondir`, as
/// git writes for every worktree it adds. Each comes on a line of its own, and
/// the `gitdir` its `.git` names on the line after. A repository's own checkout
/// has a `.git` directory, the directory that holds a bare repository may have
/// a `.git` file pointing at the repository, and a submodule's `.git` points
/// into `modules`, wherever the submodule lives; none of those holds a
/// `commondir`. The check is an `if`, so that a last directory that is no
/// worktree leaves the script a success. Builtins only, so that a long list
/// starts no process per directory.
pub const WORKTREES: [&str; 4] = [
    "sh",
    "-c",
    r#"for d do
    line=
    [ -f "$d/.git" ] && IFS= read -r line < "$d/.git"
    case $line in "gitdir: "?*) ;; *) continue ;; esac
    gitdir=${line#gitdir: }
    case $gitdir in /*) at=$gitdir ;; *) at=$d/$gitdir ;; esac
    if [ -f "$at/commondir" ]; then printf '%s\n%s\n' "$d" "$gitdir"; fi
done"#,
    "sh",
];

pub const WORKTREES_VALUE: &str = "worktrees";

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
    /// The directories zoxide listed, until `WORKTREES` says which are
    /// worktrees, and so what each is named. The list from before stays up
    /// meanwhile, so that no name changes under your eye.
    found: Option<Vec<Dir>>,
    listings: BTreeMap<String, Listing>,
    matcher: Option<SkimMatcherV2>,
}

impl DirSet {
    pub fn ingest(&mut self, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if exit_code != Some(0) {
            let reason = String::from_utf8_lossy(stderr);
            let reason = reason.lines().next().unwrap_or("").trim();
            self.fail(if reason.is_empty() {
                "zoxide is not available".to_owned()
            } else {
                format!("zoxide: {reason}")
            });
            return;
        }
        self.found = Some(parse(&String::from_utf8_lossy(stdout)));
    }

    /// `WORKTREES` with the directories zoxide listed, while they wait on it.
    pub fn worktrees_query(&self) -> Option<Vec<&str>> {
        let found = self.found.as_ref()?;
        let mut command = WORKTREES.to_vec();
        command.extend(found.iter().map(|dir| dir.path.as_str()));
        Some(command)
    }

    /// Names each worktree among the directories zoxide listed for its
    /// repository too. A check that failed leaves every directory its own
    /// name.
    pub fn ingest_worktrees(&mut self, exit_code: Option<i32>, stdout: &[u8]) {
        let Some(mut found) = self.found.take() else {
            return;
        };
        if exit_code == Some(0) {
            let listed = String::from_utf8_lossy(stdout);
            let lines: Vec<&str> = listed.lines().collect();
            let gitdirs: HashMap<&str, &str> =
                lines.as_chunks::<2>().0.iter().map(|[path, gitdir]| (*path, *gitdir)).collect();
            for dir in &mut found {
                let Some(gitdir) = gitdirs.get(dir.path.as_str()) else {
                    continue;
                };
                if let Some(name) = worktree_name(&dir.path, gitdir) {
                    dir.name = name;
                }
            }
        }
        self.asking = false;
        self.status = Fetch::Ready(found);
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
        let taken = sessions.taken(current);

        if let Fetch::Ready(all) = &self.status {
            if term.is_empty() {
                for dir in all {
                    let name = free_name(&dir.name, &taken);
                    rows.push(DirRow::new(dir, name, 0, vec![], false));
                }
            } else {
                let matcher = self
                    .matcher
                    .get_or_insert_with(|| SkimMatcherV2::default().use_cache(true));
                for dir in all {
                    let name = free_name(&dir.name, &taken);
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
        self.listings.insert(path.to_owned(), Listing::Reading);
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
    if line.is_empty() { "eza is not available".to_owned() } else { line.to_owned() }
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

#[expect(
    clippy::maybe_infinite_iter,
    reason = "the snapshot is finite, so some postfix in `2..` is free; `find` stops there"
)]
fn free_name(base: &str, taken: &Taken) -> String {
    if !taken.holds(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| {
            let postfix = format!("-{n}");
            format!("{}{}", head(base, MAX_NAME_BYTES.saturating_sub(postfix.len())), postfix)
        })
        .find(|candidate| !taken.holds(candidate))
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
            Some(Dir { path: path.to_owned(), name, frecency })
        })
        .collect()
}

/// A cwd as luneta shows one: its last two parts.
pub fn short_cwd(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    match parts.as_slice() {
        [] => path.to_owned(),
        [only] => (*only).to_owned(),
        [.., parent, base] => format!("{parent}/{base}"),
    }
}

fn derive_name(path: &str) -> Option<String> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    validate_name(base).is_none().then(|| base.to_owned())
}

/// A worktree is named for its repository too, `luneta-main`, where `main`
/// alone would name one of every repository, wherever the worktree lives. The
/// repository is the directory whose `worktrees` the `gitdir` points into, less
/// a bare repository's `.git`, or, when that directory is hidden, as a `.git`
/// or a `.bare`, the directory that holds it. A worktree whose own name starts
/// with its repository's, as `app-hotfix` beside `app`, keeps it, and so does
/// one whose name would come out too long for zellij to make a session of.
fn worktree_name(path: &str, gitdir: &str) -> Option<String> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    let common = gitdir.rsplit_once('/')?.0.strip_suffix("/worktrees")?;
    // A relative `gitdir` is from the worktree, as git wrote it.
    let from = if gitdir.starts_with('/') { "" } else { path };
    let mut parts: Vec<&str> = Vec::new();
    for part in from.split('/').chain(common.split('/')) {
        match part {
            "" | "." => {},
            ".." => {
                parts.pop();
            },
            part => parts.push(part),
        }
    }
    let mut repository = parts.pop()?;
    if repository.starts_with('.') {
        repository = parts.pop()?;
    }
    let repository =
        repository.strip_suffix(".git").filter(|bare| !bare.is_empty()).unwrap_or(repository);
    if base.strip_prefix(repository).is_some_and(|rest| !rest.starts_with(char::is_alphanumeric)) {
        return None;
    }
    let name = format!("{repository}-{base}");
    validate_name(&name).is_none().then_some(name)
}

#[cfg(test)]
mod tests {

    use std::fmt::Write as _;

    use super::*;
    use crate::elapsed::Age;
    use crate::sessions::Session;

    fn named(name: &str) -> Session {
        Session { name: name.to_owned(), age: Age::ZERO }
    }

    fn listed(stdout: &[u8]) -> DirSet {
        let mut dirs = DirSet::default();
        dirs.ingest_listing("/tmp/x".to_owned(), Some(0), stdout, b"");
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
    fn short_cwd_keeps_the_last_two_components() {
        assert_eq!(short_cwd("/home/you/Projects/misc/luneta"), "misc/luneta");
        assert_eq!(short_cwd("/home/you"), "home/you");
        assert_eq!(short_cwd("/home"), "home");
        assert_eq!(short_cwd("/"), "/");
        assert_eq!(short_cwd(""), "");
        assert_eq!(short_cwd("/misc/luneta/"), "misc/luneta");
    }

    #[test]
    fn a_failure_leaves_no_directories_behind() {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"  9264.0 /home/you/Projects/thing\n", b"");
        dirs.ingest_worktrees(Some(0), b"");
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
        let listing = (0..MAX_ENTRIES * 2).fold(String::new(), |mut acc, i| {
            writeln!(acc, "file-{i}").unwrap();
            acc
        });
        let (entries, total) = entries(&listed(listing.as_bytes()));
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(total, MAX_ENTRIES * 2);
    }

    #[test]
    fn a_failed_listing_keeps_the_reason_and_not_the_path() {
        let mut dirs = DirSet::default();
        dirs.ingest_listing(
            "/tmp/x".to_owned(),
            Some(2),
            b"",
            b"\"/tmp/x\": No such file or directory (os error 2)\n",
        );
        assert_eq!(failure(&dirs, "/tmp/x"), "No such file or directory");
        dirs.ingest_listing(
            "/tmp/y".to_owned(),
            Some(0),
            b"",
            b"Permission denied: /tmp/y - code: 13\n",
        );
        assert_eq!(failure(&dirs, "/tmp/y"), "Permission denied");
        dirs.ingest_listing("/tmp/z".to_owned(), Some(1), b"", b"");
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
        dirs.ingest_listing("/tmp/x".to_owned(), Some(0), b"src/\n", b"");
        assert!(!dirs.begin_listing("/tmp/x"));
        dirs.ingest_listing("/tmp/x".to_owned(), Some(2), b"", b"\"/tmp/x\": No such file");
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

    /// Each shape a `.git` takes, on disk, through the `sh` zellij would run.
    #[test]
    fn only_a_linked_worktree_is_found_a_worktree() {
        let root = std::env::temp_dir().join(format!("luneta-worktrees-{}", std::process::id()));
        let at = |path: &str| root.join(path);
        let write = |path: &str, text: String| {
            std::fs::create_dir_all(at(path).parent().expect("a parent")).expect("a directory");
            std::fs::write(at(path), text).expect("a file");
        };
        let repository = at("repo/.bare");
        write("repo/.git", "gitdir: ./.bare\n".to_owned());
        write("repo/main/.git", format!("gitdir: {}/worktrees/main\n", repository.display()));
        write("repo/.bare/worktrees/main/commondir", "../..\n".to_owned());
        // Relative, as `git worktree add --relative-paths` writes it, and with
        // no newline at the end.
        write("repo/feat x/.git", "gitdir: ../.bare/worktrees/feat x".to_owned());
        write("repo/.bare/worktrees/feat x/commondir", "../..\n".to_owned());
        // As git writes it for a submodule in a worktree.
        write("repo/main/lib/.git", "gitdir: ../../.bare/worktrees/main/modules/lib\n".to_owned());
        write("repo/.bare/worktrees/main/modules/lib/HEAD", "ref: refs/heads/main\n".to_owned());
        write("sub/.git", "gitdir: ../.git/modules/sub\n".to_owned());
        write(".git/modules/sub/HEAD", "ref: refs/heads/main\n".to_owned());
        // A submodule whose path runs through a `worktrees` of its own.
        write("app/worktrees/lib/.git", "gitdir: ../../.git/modules/worktrees/lib\n".to_owned());
        write("app/.git/modules/worktrees/lib/HEAD", "ref: refs/heads/main\n".to_owned());
        std::fs::create_dir_all(at("checkout/.git")).expect("a .git directory");
        std::fs::create_dir_all(at("plain")).expect("a directory");

        let asked = [
            "repo",
            "repo/main",
            "repo/feat x",
            "repo/main/lib",
            "sub",
            "app/worktrees/lib",
            "checkout",
            "plain",
            "gone",
        ];
        let paths: Vec<String> = asked.iter().map(|path| at(path).display().to_string()).collect();
        let output = std::process::Command::new(WORKTREES[0])
            .args(&WORKTREES[1..])
            .args(&paths)
            .output()
            .expect("sh runs");
        std::fs::remove_dir_all(&root).expect("the directories go");

        assert!(output.status.success());
        let found = String::from_utf8(output.stdout).expect("utf-8");
        let main = format!("{}/worktrees/main", repository.display());
        assert_eq!(
            found.lines().collect::<Vec<_>>(),
            [&paths[1], &main, &paths[2], "../.bare/worktrees/feat x"]
        );
    }

    #[test]
    fn a_worktree_is_named_for_its_repository_once_the_check_says() {
        let zoxide = b"30 /home/you/luneta\n25 /home/you/luneta/main\n20 /home/you/luneta/feat\n\
                       15 /home/you/infra.git/master\n10 /home/you/app/.claude/worktrees/fix\n\
                       5 /home/you/app-hotfix\n1 /main\n";
        // Each worktree, and on the line after it the `gitdir` its `.git` names.
        let checked = b"/home/you/luneta/main\n/home/you/luneta/.bare/worktrees/main\n\
                        /home/you/luneta/feat\n../.bare/worktrees/feat\n\
                        /home/you/infra.git/master\n/home/you/infra.git/worktrees/master\n\
                        /home/you/app/.claude/worktrees/fix\n/home/you/app/.git/worktrees/fix\n\
                        /home/you/app-hotfix\n/home/you/app/.git/worktrees/app-hotfix\n\
                        /main\n/.bare/worktrees/main\n";
        let names = |dirs: &mut DirSet| {
            dirs.rebuild("", &Sessions::default(), None, Selection::SnapToTop);
            dirs.rows.iter().map(|row| row.name.clone()).collect::<Vec<_>>()
        };
        let worktrees = [
            "luneta", "luneta-main", "luneta-feat", "infra-master", "app-fix", "app-hotfix", "main",
        ];
        let own = ["luneta", "main", "feat", "master", "fix", "app-hotfix", "main"];
        for (exit_code, named) in [(Some(0), worktrees), (Some(2), own)] {
            let mut dirs = DirSet::default();
            dirs.ingest(Some(0), zoxide, b"");
            assert!(names(&mut dirs).is_empty());
            dirs.ingest_worktrees(exit_code, checked);
            assert_eq!(names(&mut dirs), named, "{exit_code:?}");
        }
    }

    #[test]
    fn a_worktree_too_long_for_zellij_keeps_its_own_name() {
        // The longest socket directory a uid gives zellij, and the longest
        // socket path it can make.
        let socket = "/run/user/4294967295/zellij/contract_version_1/";
        let gitdir = "/home/you/app/.git/worktrees/wt";
        let fits = "w".repeat(MAX_NAME_BYTES - "app-".len());

        let name = worktree_name(&format!("/home/you/{fits}"), gitdir).expect("a name");
        assert_eq!(name, format!("app-{fits}"));
        assert!(socket.len() + name.len() <= 107);
        assert_eq!(worktree_name(&format!("/home/you/{fits}w"), gitdir), None);
    }

    #[test]
    fn a_directory_without_a_usable_name_is_dropped() {
        assert_eq!(derive_name("/"), None);
        assert_eq!(derive_name(""), None);
        assert_eq!(derive_name(&format!("/home/{}", "d".repeat(MAX_NAME_BYTES + 1))), None);
    }

    #[test]
    fn a_taken_name_takes_the_next_postfix() {
        let sessions = Sessions {
            live: vec![named("thing")],
            dead: vec![named("other")],
        };

        assert_eq!(free_name("absent", &sessions.taken(None)), "absent");
        assert_eq!(free_name("thing", &sessions.taken(None)), "thing-2");
        assert_eq!(free_name("other", &sessions.taken(None)), "other-2");
    }

    #[test]
    fn the_name_a_row_shows_is_a_term_that_finds_it() {
        let mut dirs = DirSet::default();
        dirs.ingest(Some(0), b"9268 /home/lorenzo/Projects/misc/luneta\n", b"");
        dirs.ingest_worktrees(Some(0), b"");
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
        assert_eq!(free_name("here", &sessions.taken(Some("here"))), "here-2");
    }

    #[test]
    fn the_postfix_counts_past_every_session_that_holds_one() {
        let sessions = Sessions {
            live: vec![named("thing"), named("thing-2")],
            dead: vec![named("thing-3")],
        };
        assert_eq!(free_name("thing", &sessions.taken(None)), "thing-4");
    }

    #[test]
    fn a_long_name_loses_its_tail_and_not_its_postfix() {
        let base = "d".repeat(MAX_NAME_BYTES);
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        let name = free_name(&base, &sessions.taken(None));
        assert_eq!(name.len(), MAX_NAME_BYTES);
        assert!(name.ends_with("-2"));
        assert!(validate_name(&name).is_none());
    }

    #[test]
    fn a_long_name_is_cut_between_characters() {
        let base = format!("ab{}x", "の".repeat(19));
        assert_eq!(base.len(), MAX_NAME_BYTES);
        assert!(!base.is_char_boundary(MAX_NAME_BYTES - 2));
        let sessions = Sessions { live: vec![named(&base)], dead: vec![] };

        let name = free_name(&base, &sessions.taken(None));
        assert_eq!(name, format!("ab{}-2", "の".repeat(18)));
        assert!(name.len() < MAX_NAME_BYTES);
        assert!(validate_name(&name).is_none());
    }

    #[test]
    fn a_full_cache_is_dropped_rather_than_grown() {
        let mut dirs = DirSet::default();
        for i in 0..MAX_LISTINGS {
            assert!(dirs.begin_listing(&format!("/tmp/{i}")));
        }
        assert!(dirs.listing("/tmp/0").is_some());
        assert!(dirs.begin_listing("/tmp/one-too-many"));
        assert!(dirs.listing("/tmp/0").is_none());
        assert!(dirs.listing("/tmp/one-too-many").is_some());
    }
}
