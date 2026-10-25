//! The branch checked out in each directory a pane works in, and how its work
//! tree stands, for the sidebar to show under the pane's row: as the daemon
//! says, or as git does while the daemon does not answer.

use std::collections::{BTreeSet, HashMap};

use luneta_protocol::git::WorkTree;
use luneta_protocol::v1::checkout::Head;
use luneta_protocol::v1::work_tree::Stands;
use luneta_protocol::v1::{self, Changes, Upstream};

use crate::context::unstarted;

pub const CONTEXT_VALUE: &str = "checkouts";
pub const DIR_KEY: &str = "luneta_dir";

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

impl Checkout {
    /// How a work tree stands, as the daemon says it: the branch checked out
    /// there, or none for a detached `HEAD` or a directory in no repository.
    /// `Unread` for an answer that leaves out what the protocol says is always
    /// there, as a newer daemon's could.
    fn of(stands: &Stands) -> Result<Option<Self>, Unread> {
        let checkout = match stands {
            Stands::Outside(_) => return Ok(None),
            Stands::Checkout(checkout) => checkout,
        };
        let changes = checkout.changes.as_ref().ok_or(Unread)?;
        let branch = match checkout.head.as_ref().ok_or(Unread)? {
            Head::Detached(_) => return Ok(None),
            Head::Branch(branch) => branch,
        };
        let status = glyphs(changes, checkout.stashes, branch.upstream);
        Ok(Some(Checkout { branch: branch.name.clone(), status }))
    }
}

/// What the daemon says that this plugin cannot read.
struct Unread;

/// The glyph of each kind of change there is, then the one for where the
/// branch stands against its upstream, where it has one.
fn glyphs(changes: &Changes, stashes: u32, upstream: Option<Upstream>) -> String {
    let kinds = [
        (changes.conflicted, '='),
        (stashes, '$'),
        (changes.deleted, '✘'),
        (changes.renamed, '»'),
        (changes.modified, '!'),
        (changes.staged, '+'),
        (changes.untracked, '?'),
    ];
    let mut glyphs: String =
        kinds.iter().filter(|(count, _)| *count > 0).map(|(_, glyph)| *glyph).collect();
    match upstream.map(|upstream| (upstream.ahead, upstream.behind)) {
        Some((ahead, behind)) if ahead > 0 && behind > 0 => glyphs.push('⇕'),
        Some((ahead, _)) if ahead > 0 => glyphs.push('⇡'),
        Some((_, behind)) if behind > 0 => glyphs.push('⇣'),
        _ => {},
    }
    glyphs
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

    /// What `luneta_protocol::git::command` answered for `dir`, read the way
    /// the daemon reads it, so that the two say the same.
    pub fn ingest(&mut self, dir: String, exit_code: Option<i32>, stdout: &[u8], stderr: &[u8]) {
        if let Some(asked) = self.asked.get_mut(&dir) {
            *asked = Asked::Answered;
        }
        self.absent |= unstarted(exit_code, stderr);
        let tree = match exit_code {
            Some(0) => match WorkTree::parse(&String::from_utf8_lossy(stdout)) {
                Ok(tree) => tree,
                // What git said that does not read is not read wrong, and
                // what it said last stands, as the daemon has it.
                Err(_) => return,
            },
            // A signal stopped git before it said anything, and what it said
            // last stands.
            None => return,
            // git ran there and failed: the directory is in no repository,
            // or in one git cannot read.
            Some(_) => WorkTree::Outside,
        };
        let stands = tree.to_api(&dir).stands;
        if let Some(Ok(checkout)) = stands.as_ref().map(Checkout::of) {
            self.known.insert(dir, checkout);
        }
    }

    /// What the daemon says of the work tree in each directory a pane works
    /// in, in the place of all git said. A directory it has said nothing of
    /// yet has nothing said of it, and so does one it says what this sidebar
    /// cannot read of.
    pub fn heard(&mut self, trees: &[v1::WorkTree]) {
        let read = |tree: &v1::WorkTree| {
            Some((tree.dir.clone(), Checkout::of(tree.stands.as_ref()?).ok()?))
        };
        self.known = trees.iter().filter_map(read).collect();
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
            let mut checkouts = Checkouts::default();
            let said = format!("# branch.head main\n{lines}");
            checkouts.ingest("/w/repo".to_owned(), Some(0), said.as_bytes(), b"");
            let Said::Branch(checkout) = checkouts.said("/w/repo") else {
                panic!("main is checked out: {lines:?}");
            };
            assert_eq!(checkout.status, status, "{lines:?}");
        }
    }

    /// What the daemon says of the work trees takes the place of all git
    /// said, as it reads the same, and a work tree it says what this plugin
    /// cannot read of, as a newer daemon could, has nothing said of it.
    #[test]
    fn the_daemon_s_work_trees_stand_in_the_place_of_what_git_said() {
        let mut checkouts = Checkouts::default();
        checkouts.ingest("/w/gone".to_owned(), Some(0), b"# branch.head main\n", b"");
        let stands = |dir: &str, stands| v1::WorkTree { dir: dir.to_owned(), stands: Some(stands) };
        let checkout = |head| {
            Stands::Checkout(v1::Checkout {
                head: Some(head),
                changes: Some(Changes { modified: 2, ..Changes::default() }),
                stashes: 0,
            })
        };
        let upstream = Some(Upstream { ahead: 1, behind: 0 });
        let fold = v1::Branch { name: "fold".to_owned(), upstream };
        checkouts.heard(&[
            stands("/w/repo", checkout(Head::Branch(fold))),
            stands("/w/detached", checkout(Head::Detached(v1::checkout::Detached {}))),
            stands("/w/plain", Stands::Outside(v1::work_tree::Outside {})),
            stands("/w/newer", Stands::Checkout(v1::Checkout::default())),
            v1::WorkTree { dir: "/w/newest".to_owned(), stands: None },
        ]);
        let on = Checkout { branch: "fold".to_owned(), status: "!⇡".to_owned() };
        assert_eq!(checkouts.said("/w/repo"), Said::Branch(&on));
        for dir in ["/w/detached", "/w/plain"] {
            assert_eq!(checkouts.said(dir), Said::NoBranch, "{dir}");
        }
        for dir in ["/w/newer", "/w/newest", "/w/gone"] {
            assert_eq!(checkouts.said(dir), Said::Nothing, "{dir}");
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
