//! The branch checked out in each directory a pane works in, and how its work
//! tree stands, for the sidebar to show under the pane's row, as the daemon
//! says.

use std::collections::HashMap;

use luneta_protocol::git::{Changes, Head, Upstream, WorkTree};
use luneta_protocol::v1;

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

/// What the daemon has said of a directory.
#[derive(Debug, PartialEq, Eq)]
pub enum Said<'a> {
    /// Nothing yet.
    Nothing,
    /// That no branch is checked out there: it is in no repository, its
    /// `HEAD` is detached, or git failed there.
    NoBranch,
    /// The branch checked out there, and how its work tree stands.
    Branch(&'a Checkout),
}

impl Checkout {
    /// The branch checked out in a work tree, or none for a detached `HEAD`
    /// or a directory in no repository.
    fn of(tree: &WorkTree) -> Option<Self> {
        let WorkTree::Checkout { head: Head::Branch { name, upstream }, changes, stashes } = tree
        else {
            return None;
        };
        let status = glyphs(changes, *stashes, *upstream);
        Some(Checkout { branch: name.as_str().to_owned(), status })
    }
}

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

#[derive(Default)]
pub struct Checkouts {
    /// Each directory the daemon has said of, and what is checked out there.
    known: HashMap<String, Option<Checkout>>,
}

impl Checkouts {
    /// What the daemon says of the work tree in each directory a pane works
    /// in, in the place of all it said before. A directory it has said
    /// nothing of yet has nothing said of it, and so does one it says what
    /// this sidebar cannot read of.
    pub fn heard(&mut self, trees: &[v1::WorkTree]) {
        let read = |tree: &v1::WorkTree| {
            Some((tree.dir.clone(), Checkout::of(&WorkTree::of_api(tree)?)))
        };
        self.known = trees.iter().filter_map(read).collect();
    }

    /// What the daemon last said of `dir`.
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

    /// What the daemon says of the work tree in `dir`, where git printed
    /// `said`, or failed there for none.
    fn told(dir: &str, said: Option<&str>) -> v1::WorkTree {
        let tree = said.map_or(Ok(WorkTree::Outside), WorkTree::parse);
        tree.expect("what git said reads").to_api(dir)
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
            checkouts.heard(&[told("/w/repo", Some(&said))]);
            let Said::Branch(checkout) = checkouts.said("/w/repo") else {
                panic!("main is checked out: {lines:?}");
            };
            assert_eq!(checkout.status, status, "{lines:?}");
        }
    }

    /// What the daemon says of the work trees takes the place of all it said
    /// before, and a work tree it says what this plugin cannot read of, as a
    /// newer daemon could, has nothing said of it.
    #[test]
    fn what_the_daemon_says_of_the_work_trees_stands_in_the_place_of_what_it_said() {
        use v1::checkout::Head;
        use v1::work_tree::Stands;
        use v1::{Changes, Upstream};

        let mut checkouts = Checkouts::default();
        checkouts.heard(&[told("/w/gone", Some("# branch.head main\n"))]);
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
        checkouts.heard(&[
            told("/w/repo", Some("# branch.oid 3e19\n# branch.head feat/x\n")),
            told("/w/new", Some("# branch.oid (initial)\n# branch.head main\n")),
            told("/w/detached", Some("# branch.head (detached)\n")),
            told("/w/plain", None),
        ]);
        let on = |branch: &str| Checkout { branch: branch.to_owned(), status: String::new() };
        assert_eq!(checkouts.said("/w/repo"), Said::Branch(&on("feat/x")));
        assert_eq!(checkouts.said("/w/new"), Said::Branch(&on("main")));
        for dir in ["/w/detached", "/w/plain"] {
            assert_eq!(checkouts.said(dir), Said::NoBranch, "{dir}");
        }
        // The daemon has said nothing yet of a directory no pane works in.
        assert_eq!(checkouts.said("/w/unasked"), Said::Nothing);
    }
}
