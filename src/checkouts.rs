//! The branch checked out in each directory a pane works in, and how its work
//! tree stands, for the sidebar to show under the pane's row.

use std::collections::{BTreeSet, HashMap};

pub const CONTEXT_VALUE: &str = "checkouts";
pub const DIR_KEY: &str = "luneta_dir";

/// How the work tree in `dir` stands, as `git status` tells it to a program:
/// its branch, how far that is from its upstream, its stashes, and a line for
/// each path with changes. `--no-optional-locks` keeps git from taking the
/// index lock to refresh the index, which a `git` of your own would trip over.
/// `core.fsmonitor=false` keeps a repository's own config from naming a
/// command for every status to run, since git is asked about any directory a
/// pane works in, trusted or not; a filter driver it names can still run on a
/// changed file, as with any `git status` there.
pub fn query(dir: &str) -> [&str; 10] {
    [
        "git",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-C",
        dir,
        "status",
        "--porcelain=v2",
        "--branch",
        "--show-stash",
    ]
}

/// The branch checked out in a directory, and how its work tree stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkout {
    pub branch: String,
    /// What starship's `git_status` shows, with its default glyphs and in its
    /// order: one for each kind of change there is, then one for where the
    /// branch stands against its upstream. Empty for a clean tree that is
    /// even with its upstream, or has none.
    pub status: String,
}

/// What git has said of a directory.
#[derive(Debug, PartialEq, Eq)]
pub enum Said<'a> {
    /// Nothing yet.
    Nothing,
    /// That no branch is checked out there: it is in no repository, its
    /// `HEAD` is detached, or git failed there or could not be started.
    NoBranch,
    /// The branch checked out there, and how its work tree stands.
    Branch(&'a Checkout),
}

const DETACHED: &str = "(detached)";

impl Checkout {
    /// What `query` prints, read as starship reads it. A detached HEAD names
    /// no branch, and is no checkout.
    fn parse(status: &str) -> Option<Self> {
        let mut branch = None;
        let mut counts = Counts::default();
        for line in status.lines() {
            if let Some(head) = line.strip_prefix("# branch.head ") {
                branch = Some(head).filter(|head| *head != DETACHED);
            } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
                counts.upstream = ahead_behind(ab);
            } else if let Some(stashes) = line.strip_prefix("# stash ") {
                counts.stashed = stashes.parse().unwrap_or(0);
            } else {
                counts.add(line);
            }
        }
        Some(Checkout { branch: branch?.to_owned(), status: counts.glyphs() })
    }
}

/// `+<ahead> -<behind>`.
fn ahead_behind(ab: &str) -> Option<(usize, usize)> {
    let (ahead, behind) = ab.split_once(' ')?;
    Some((ahead.strip_prefix('+')?.parse().ok()?, behind.strip_prefix('-')?.parse().ok()?))
}

/// The paths of each kind starship counts, the stashes, and the commits ahead
/// of the upstream and behind it, where there is an upstream.
#[derive(Default)]
struct Counts {
    conflicted: usize,
    stashed: usize,
    deleted: usize,
    renamed: usize,
    modified: usize,
    staged: usize,
    untracked: usize,
    upstream: Option<(usize, usize)>,
}

impl Counts {
    /// A line of `git status --porcelain=v2` about a path: `1` for a changed
    /// one, `2` for a renamed or copied one, `u` for a conflict, `?` for an
    /// untracked one. The first two say how it stands in the index and in the
    /// work tree, `.` for unchanged.
    fn add(&mut self, line: &str) {
        let mut fields = line.split(' ');
        match (fields.next(), fields.next()) {
            (Some("1"), Some(xy)) => self.add_change(xy),
            (Some("2"), Some(xy)) => {
                self.renamed += 1;
                self.add_change(xy);
            },
            (Some("u"), _) => self.conflicted += 1,
            (Some("?"), _) => self.untracked += 1,
            _ => {},
        }
    }

    fn add_change(&mut self, xy: &str) {
        let mut states = xy.chars();
        let (Some(index), Some(tree)) = (states.next(), states.next()) else {
            return;
        };
        self.staged += usize::from(matches!(index, 'M' | 'A' | 'T'));
        self.deleted += usize::from(index == 'D') + usize::from(tree == 'D');
        // `A` in the work tree is a path added with `--intent-to-add`.
        self.modified += usize::from(matches!(tree, 'M' | 'A'));
    }

    fn glyphs(&self) -> String {
        let kinds = [
            (self.conflicted, '='),
            (self.stashed, '$'),
            (self.deleted, '✘'),
            (self.renamed, '»'),
            (self.modified, '!'),
            (self.staged, '+'),
            (self.untracked, '?'),
        ];
        let mut glyphs: String =
            kinds.iter().filter(|(count, _)| *count > 0).map(|(_, glyph)| *glyph).collect();
        match self.upstream {
            Some((ahead, behind)) if ahead > 0 && behind > 0 => glyphs.push('⇕'),
            Some((ahead, _)) if ahead > 0 => glyphs.push('⇡'),
            Some((_, behind)) if behind > 0 => glyphs.push('⇣'),
            _ => {},
        }
        glyphs
    }
}

/// Where a directory stands with git since it was last asked about.
#[derive(PartialEq, Eq)]
enum Asked {
    Waiting,
    Answered,
}

#[derive(Default)]
pub struct Checkouts {
    /// Each directory git has answered for, and what is checked out there.
    known: HashMap<String, Option<Checkout>>,
    /// The directories asked about since the last `forget`, and those git has
    /// yet to answer for since before it.
    asked: HashMap<String, Asked>,
    /// Whether git could not be started at all, as when it is not on the
    /// server's `PATH`. Nothing is asked until `wake`, so that zellij does not
    /// log the failure for every directory every few seconds.
    absent: bool,
}

impl Checkouts {
    /// The directories of `dirs` not asked about since the last `forget`, to
    /// ask about now. What was said of a directory no longer in `dirs` goes.
    pub fn ask(&mut self, dirs: &BTreeSet<String>) -> Vec<String> {
        self.known.retain(|dir, _| dirs.contains(dir));
        if self.absent {
            return Vec::new();
        }
        self.asked.retain(|dir, asked| dirs.contains(dir) || *asked == Asked::Waiting);
        let new: Vec<String> =
            dirs.iter().filter(|dir| !self.asked.contains_key(dir.as_str())).cloned().collect();
        self.asked.extend(new.iter().map(|dir| (dir.clone(), Asked::Waiting)));
        new
    }

    pub fn ingest(&mut self, dir: String, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if let Some(asked) = self.asked.get_mut(&dir) {
            *asked = Asked::Answered;
        }
        self.absent |= unstarted(exit_code, stderr);
        let checkout = match exit_code {
            Some(0) => Checkout::parse(&String::from_utf8_lossy(stdout)),
            // A signal stopped git before it said anything, and what it said
            // last stands.
            None => return,
            Some(_) => None,
        };
        self.known.insert(dir, checkout);
    }

    /// Has every directory asked about again, for what changed since, but
    /// one git has yet to answer for: a slow `git status` runs one at a time.
    /// What git said last stands until it answers.
    pub fn forget(&mut self) {
        self.asked.retain(|_, asked| *asked == Asked::Waiting);
    }

    /// `forget`, and git is looked for again if it could not be started, for
    /// a sidebar that comes back to whatever changed while it was away.
    pub fn wake(&mut self) {
        self.absent = false;
        self.forget();
    }

    /// What git last said of `dir`.
    pub fn said(&self, dir: &str) -> Said<'_> {
        match self.known.get(dir) {
            None => Said::Nothing,
            Some(None) => Said::NoBranch,
            Some(Some(checkout)) => Said::Branch(checkout),
        }
    }
}

/// Whether zellij could not start git at all, as when it is not on the
/// server's `PATH`. zellij says so with exit code 2 and the error from the OS,
/// as `No such file or directory (os error 2)`, which git's own errors never
/// say.
fn unstarted(exit_code: Option<i32>, stderr: &[u8]) -> bool {
    exit_code == Some(2) && String::from_utf8_lossy(stderr).contains("(os error ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(dirs: &[&str]) -> BTreeSet<String> {
        dirs.iter().map(|dir| (*dir).to_owned()).collect()
    }

    /// Starship's `git_status`, with the defaults, over the same work tree.
    #[test]
    fn the_status_shows_what_starship_shows() {
        for (lines, status) in [
            ("", ""),
            ("1 .M N... 100644 100644 100644 e4c3 e4c3 README.md", "!"),
            ("1 .A N... 000000 000000 100644 0000 0000 intended.rs", "!"),
            ("1 M. N... 100644 100644 100644 e4c3 f00d README.md", "+"),
            ("1 A. N... 000000 100644 100644 0000 f00d new.rs", "+"),
            ("1 T. N... 100644 120000 120000 e4c3 f00d link", "+"),
            ("1 MM N... 100644 100644 100644 e4c3 f00d README.md", "!+"),
            ("1 D. N... 100644 000000 000000 e4c3 0000 gone.rs", "✘"),
            ("1 .D N... 100644 100644 000000 e4c3 e4c3 gone.rs", "✘"),
            ("2 R. N... 100644 100644 100644 e4c3 e4c3 R100 new.rs\told.rs", "»"),
            ("u UU N... 100644 100644 100644 100644 a b c both.rs", "="),
            ("? scratch.txt", "?"),
            ("# stash 2", "$"),
            ("# branch.upstream origin/main\n# branch.ab +0 -0", ""),
            ("# branch.ab +2 -0", "⇡"),
            ("# branch.ab +0 -3", "⇣"),
            ("# branch.ab +2 -3", "⇕"),
            (
                "# branch.ab +1 -0\n# stash 1\n? a\n1 .M N... 1 1 1 a a b\n\
                 2 R. N... 1 1 1 a a R100 c\td\nu UU N... 1 1 1 1 a b c e\n1 D. N... 1 0 0 a 0 f",
                "=$✘»!?⇡",
            ),
        ] {
            let checkout = Checkout::parse(&format!("# branch.head main\n{lines}"));
            let shown = checkout.map(|checkout| checkout.status);
            assert_eq!(shown.as_deref(), Some(status), "{lines:?}");
        }
    }

    #[test]
    fn only_a_branch_git_names_is_a_checkout() {
        let mut checkouts = Checkouts::default();
        let ingest = |checkouts: &mut Checkouts, dir: &str, exit_code, stdout: &str| {
            checkouts.ingest(dir.to_owned(), exit_code, stdout.as_bytes(), b"");
        };
        ingest(&mut checkouts, "/w/repo", Some(0), "# branch.oid 3e19\n# branch.head feat/x\n");
        ingest(&mut checkouts, "/w/new", Some(0), "# branch.oid (initial)\n# branch.head main\n");
        ingest(&mut checkouts, "/w/detached", Some(0), "# branch.head (detached)\n");
        ingest(&mut checkouts, "/w/plain", Some(128), "");
        let on = |branch: &str| Checkout { branch: branch.to_owned(), status: String::new() };
        assert_eq!(checkouts.said("/w/repo"), Said::Branch(&on("feat/x")));
        assert_eq!(checkouts.said("/w/new"), Said::Branch(&on("main")));
        for dir in ["/w/detached", "/w/plain"] {
            assert_eq!(checkouts.said(dir), Said::NoBranch, "{dir}");
        }
        // Git has said nothing yet of a directory it was not asked about.
        assert_eq!(checkouts.said("/w/unasked"), Said::Nothing);
    }

    /// A directory is asked about once until forgotten, and never while git
    /// has yet to answer for it.
    #[test]
    fn a_directory_is_asked_about_once_at_a_time() {
        let mut checkouts = Checkouts::default();
        assert_eq!(checkouts.ask(&dirs(&["/w/a", "/w/b"])), ["/w/a", "/w/b"]);
        assert!(checkouts.ask(&dirs(&["/w/a", "/w/b"])).is_empty());
        assert_eq!(checkouts.ask(&dirs(&["/w/a", "/w/c"])), ["/w/c"]);

        checkouts.ingest("/w/a".to_owned(), Some(0), b"# branch.head main\n", b"");
        checkouts.forget();
        assert_eq!(checkouts.ask(&dirs(&["/w/a", "/w/c"])), ["/w/a"]);
        checkouts.ingest("/w/c".to_owned(), Some(128), b"", b"");
        checkouts.forget();
        assert_eq!(checkouts.ask(&dirs(&["/w/a", "/w/c"])), ["/w/c"]);
    }

    /// A git zellij could not start is not looked for again, for every
    /// directory, every few seconds, but once the sidebar wakes. A git that
    /// ran and failed was there, and is asked again.
    #[test]
    fn a_git_that_cannot_be_started_is_looked_for_again_only_on_waking() {
        let mut checkouts = Checkouts::default();
        let both = dirs(&["/w/a", "/w/b"]);
        let answer = |checkouts: &mut Checkouts, exit_code, stderr: &[u8]| {
            for dir in ["/w/a", "/w/b"] {
                checkouts.ingest(dir.to_owned(), exit_code, b"", stderr);
            }
        };
        checkouts.ask(&both);
        let fatal = b"fatal: not a git repository (or any of the parent directories): .git";
        answer(&mut checkouts, Some(128), fatal);
        checkouts.forget();
        assert_eq!(checkouts.ask(&both), ["/w/a", "/w/b"]);

        answer(&mut checkouts, Some(2), b"No such file or directory (os error 2)");
        checkouts.forget();
        assert!(checkouts.ask(&both).is_empty());
        checkouts.wake();
        assert_eq!(checkouts.ask(&both), ["/w/a", "/w/b"]);
    }

    /// What git said stays on screen while it is asked again, and through a
    /// git a signal stopped before it said anything, and goes with the last
    /// pane in its directory.
    #[test]
    fn a_checkout_stands_until_git_answers_or_the_directory_goes() {
        let mut checkouts = Checkouts::default();
        let branch = |checkouts: &Checkouts| match checkouts.said("/w/repo") {
            Said::Branch(checkout) => Some(checkout.branch.clone()),
            Said::NoBranch | Said::Nothing => None,
        };
        checkouts.ask(&dirs(&["/w/repo"]));
        checkouts.ingest("/w/repo".to_owned(), Some(0), b"# branch.head main\n", b"");
        checkouts.forget();
        checkouts.ask(&dirs(&["/w/repo"]));
        assert_eq!(branch(&checkouts).as_deref(), Some("main"));
        checkouts.ingest("/w/repo".to_owned(), Some(0), b"# branch.head fold\n", b"");
        assert_eq!(branch(&checkouts).as_deref(), Some("fold"));
        checkouts.ingest("/w/repo".to_owned(), None, b"", b"");
        assert_eq!(branch(&checkouts).as_deref(), Some("fold"));
        checkouts.ask(&dirs(&[]));
        assert_eq!(checkouts.said("/w/repo"), Said::Nothing);
    }
}
