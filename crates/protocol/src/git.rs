//! How a work tree stands, as `git status` says it: what the daemon asks git
//! in each directory a pane works in, and how its clients read that back.

use crate::said::Said;
use crate::v1;

const DETACHED: &str = "(detached)";

/// What asks git how the work tree in `dir` stands, for a program to read: its
/// branch, how far that is from its upstream, its stashes, and a line for each
/// path with changes. `--no-optional-locks` keeps git from taking the index
/// lock to refresh the index, which a `git` of your own would trip over.
/// `core.fsmonitor` off keeps a repository's own config from naming a command
/// for every status to run, since git is asked about any directory a pane
/// works in, trusted or not; a filter driver it names can still run on a
/// changed file, as with any `git status` there. `safe.bareRepository`
/// explicit keeps git from taking a directory for a bare repository on its
/// own, as one committed into a clone can be, which would have that config
/// read where a clone hands over none. Its repository's config cannot set it.
#[must_use]
pub fn command(dir: &str) -> [&str; 12] {
    [
        "git",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "safe.bareRepository=explicit",
        "-C",
        dir,
        "status",
        "--porcelain=v2",
        "--branch",
        "--show-stash",
    ]
}

/// How a work tree stands, as git says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkTree {
    /// The directory is in no repository.
    Outside,
    /// In a repository, and in none of its work trees, as the directory that
    /// holds a clone wt made is: the directory that holds the repository.
    NoCheckout { repository: Said },
    /// A repository's work tree: the directory that holds the repository,
    /// which only a daemon older than the field does not say, what is checked
    /// out there, what changed, and how many lines changed on the branch,
    /// which only a daemon newer than the field says.
    Checkout {
        repository: Option<Said>,
        head: Head,
        changes: Changes,
        stashes: u32,
        changed: Option<Changed>,
    },
}

/// How many lines changed on a branch, against the branch its repository
/// takes as its default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub added: u32,
    pub deleted: u32,
}

/// What is checked out in a work tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Detached,
    /// A branch, and how far it is from its upstream, if it has one.
    Branch { name: Said, upstream: Option<Upstream> },
}

/// How far a branch is ahead of its upstream, and behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upstream {
    pub ahead: u32,
    pub behind: u32,
}

/// How many paths there are of each kind of change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub conflicted: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub modified: u32,
    pub staged: u32,
    pub untracked: u32,
}

impl WorkTree {
    #[must_use]
    pub fn to_api(&self, dir: &str) -> v1::WorkTree {
        let stands = match self {
            WorkTree::Outside => v1::work_tree::Stands::Outside(()),
            WorkTree::NoCheckout { repository } => {
                v1::work_tree::Stands::NoCheckout(v1::work_tree::NoCheckout {
                    repository: repository.as_str().to_owned(),
                })
            },
            WorkTree::Checkout { repository, head, changes, stashes, changed } => {
                v1::work_tree::Stands::Checkout(v1::Checkout {
                    head: Some(head.to_api()),
                    changes: Some(changes.to_api()),
                    stashes: *stashes,
                    repository: repository.as_ref().map(|it| it.as_str().to_owned()),
                    changed: changed
                        .map(|Changed { added, deleted }| v1::Changed { added, deleted }),
                })
            },
        };
        v1::WorkTree { dir: dir.to_owned(), stands: Some(stands) }
    }

    /// What `to_api` wrote out, read back. None for a work tree that leaves
    /// out what the protocol says is always there, or names a branch or a
    /// repository with no name: a newer daemon's.
    #[must_use]
    pub fn of_api(tree: &v1::WorkTree) -> Option<Self> {
        let checkout = match tree.stands.as_ref()? {
            v1::work_tree::Stands::Outside(()) => return Some(WorkTree::Outside),
            v1::work_tree::Stands::NoCheckout(it) => {
                let repository = Said::new(it.repository.clone())?;
                return Some(WorkTree::NoCheckout { repository });
            },
            v1::work_tree::Stands::Checkout(checkout) => checkout,
        };
        let repository = match &checkout.repository {
            Some(repository) => Some(Said::new(repository.clone())?),
            None => None,
        };
        Some(WorkTree::Checkout {
            repository,
            head: Head::of_api(checkout.head.as_ref()?)?,
            changes: Changes::of_api(checkout.changes.as_ref()?),
            stashes: checkout.stashes,
            changed: checkout
                .changed
                .map(|v1::Changed { added, deleted }| Changed { added, deleted }),
        })
    }

    /// What `command` prints in a work tree of `repository`, or the first line
    /// that does not read. git says what is checked out ahead of the rest, and
    /// where it is from its upstream only of a branch.
    pub fn parse(status: &str, repository: Said) -> Result<Self, String> {
        let (mut head, mut changes, mut stashes) = (None, Changes::default(), 0);
        for line in status.lines() {
            let unread = || format!("a line that does not read: {line}");
            if let Some(named) = line.strip_prefix("# branch.head ") {
                head = Some(Head::of(named).ok_or_else(unread)?);
            } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
                let Some(Head::Branch { upstream, .. }) = &mut head else {
                    return Err(unread());
                };
                *upstream = Some(Upstream::of(ab).ok_or_else(unread)?);
            } else if let Some(count) = line.strip_prefix("# stash ") {
                stashes = count.parse().map_err(|_| unread())?;
            } else if line.starts_with("# ") {
                // `branch.oid` and `branch.upstream`, and any header a git to
                // come adds, which its manual asks to pass over.
            } else if !changes.add(line) {
                return Err(unread());
            }
        }
        let head = head.ok_or("no line that says what is checked out")?;
        Ok(WorkTree::Checkout {
            repository: Some(repository),
            head,
            changes,
            stashes,
            changed: None,
        })
    }
}

impl Head {
    /// What `# branch.head` names: a branch, or `(detached)`. None for no name.
    fn of(named: &str) -> Option<Self> {
        if named == DETACHED {
            return Some(Head::Detached);
        }
        Some(Head::Branch { name: Said::new(named.to_owned())?, upstream: None })
    }

    /// None for a branch with no name.
    fn of_api(head: &v1::checkout::Head) -> Option<Self> {
        match head {
            v1::checkout::Head::Detached(()) => Some(Head::Detached),
            v1::checkout::Head::Branch(branch) => Some(Head::Branch {
                name: Said::new(branch.name.clone())?,
                upstream: branch.upstream.map(Upstream::of_api),
            }),
        }
    }

    fn to_api(&self) -> v1::checkout::Head {
        match self {
            Head::Detached => v1::checkout::Head::Detached(()),
            Head::Branch { name, upstream } => v1::checkout::Head::Branch(v1::Branch {
                name: name.as_str().to_owned(),
                upstream: upstream.map(Upstream::to_api),
            }),
        }
    }
}

impl Upstream {
    /// `+<ahead> -<behind>`.
    fn of(ab: &str) -> Option<Self> {
        let (ahead, behind) = ab.split_once(' ')?;
        let ahead = ahead.strip_prefix('+')?.parse().ok()?;
        Some(Upstream { ahead, behind: behind.strip_prefix('-')?.parse().ok()? })
    }

    fn of_api(upstream: v1::Upstream) -> Self {
        Upstream { ahead: upstream.ahead, behind: upstream.behind }
    }

    fn to_api(self) -> v1::Upstream {
        v1::Upstream { ahead: self.ahead, behind: self.behind }
    }
}

/// `XY`: how a path stands in the index, and in the work tree.
fn states(xy: &str) -> Option<(char, char)> {
    let mut states = xy.chars();
    let (Some(index), Some(tree), None) = (states.next(), states.next(), states.next()) else {
        return None;
    };
    Some((index, tree))
}

impl Changes {
    /// Counts a line of `git status --porcelain=v2` about a path: `1` for a
    /// changed one, `2` for a renamed or copied one, `u` for a conflict, `?`
    /// for an untracked one. The first three go on to say how it stands, `.`
    /// for unchanged. False for a line that is none of these.
    fn add(&mut self, line: &str) -> bool {
        let mut fields = line.split(' ');
        match (fields.next(), fields.next().and_then(states)) {
            (Some("1"), Some(states)) => self.add_change(states),
            (Some("2"), Some(states)) => {
                self.renamed += 1;
                self.add_change(states);
            },
            (Some("u"), Some(_)) => self.conflicted += 1,
            (Some("?"), _) => self.untracked += 1,
            _ => return false,
        }
        true
    }

    fn of_api(changes: &v1::Changes) -> Self {
        Changes {
            conflicted: changes.conflicted,
            deleted: changes.deleted,
            renamed: changes.renamed,
            modified: changes.modified,
            staged: changes.staged,
            untracked: changes.untracked,
        }
    }

    fn to_api(&self) -> v1::Changes {
        v1::Changes {
            conflicted: self.conflicted,
            deleted: self.deleted,
            renamed: self.renamed,
            modified: self.modified,
            staged: self.staged,
            untracked: self.untracked,
        }
    }

    fn add_change(&mut self, (index, tree): (char, char)) {
        self.staged += u32::from(matches!(index, 'M' | 'A' | 'T'));
        self.deleted += u32::from(index == 'D') + u32::from(tree == 'D');
        // `A` in the work tree is a path added with `--intent-to-add`, and `T`
        // one whose type changed, as a file that is a link now.
        self.modified += u32::from(matches!(tree, 'M' | 'A' | 'T'));
    }
}

#[cfg(test)]
mod tests {
    use super::{v1, Changes, Head, Said, WorkTree};

    fn repo() -> Said {
        Said::new("/w/repo".to_owned()).unwrap()
    }

    /// What git says that does not read is no work tree, rather than one
    /// counted short: no git the tests can run says it, so it is tested here.
    /// So is a status that says nothing of what is checked out, and one with
    /// an upstream for no branch. A header that git adds for an option of its
    /// own is passed over, as its manual asks.
    #[test]
    fn a_status_that_does_not_read_is_no_work_tree() {
        let head = "# branch.oid 0123abc\n# branch.head main\n";
        let unread = [
            "# branch.head ",
            "# branch.ab +1",
            "# branch.ab 1 -2",
            "# stash many",
            "1 M",
            "1 .MM path",
            "u",
            "! ignored",
            "",
        ];
        for line in unread {
            assert!(WorkTree::parse(&format!("{head}{line}\n"), repo()).is_err(), "{line:?}");
        }
        for status in ["? new\n", "# branch.head (detached)\n# branch.ab +1 -0\n"] {
            assert!(WorkTree::parse(status, repo()).is_err(), "{status:?}");
        }
        let tree = WorkTree::parse(&format!("{head}# branch.to-come 1\n? new\n"), repo()).unwrap();
        let WorkTree::Checkout { head: Head::Branch { name, upstream: None }, changes, .. } = tree
        else {
            panic!("main, with no upstream: {tree:?}");
        };
        assert_eq!(name.as_str(), "main");
        assert_eq!(changes.untracked, 1);
    }

    /// What the daemon writes out of a work tree reads back as the same work
    /// tree, so that luneta says of it what git said to the daemon, a daemon
    /// older than the field's checkout with no repository among them. One that
    /// leaves out what the protocol says is always there, or names a branch or
    /// a repository with no name, reads as none.
    #[test]
    fn a_work_tree_reads_back_as_the_daemon_wrote_it() {
        let statuses = [
            "# branch.head main\n",
            "# branch.oid 0123abc\n# branch.head fold\n# branch.upstream origin/fold\n\
             # branch.ab +1 -2\n# stash 3\n1 MM N... 100644 100644 100644 a b README.md\n\
             2 R. N... 100644 100644 100644 a a R100 new.rs\told.rs\n\
             u UU N... 100644 100644 100644 100644 a b c both.rs\n\
             1 .D N... 100644 100644 000000 a a gone.rs\n? new\n",
            "# branch.head (detached)\n1 .T N... 100644 100644 120000 a a link\n",
        ];
        let mut trees: Vec<WorkTree> =
            statuses.iter().map(|status| WorkTree::parse(status, repo()).unwrap()).collect();
        trees.push(WorkTree::Outside);
        trees.push(WorkTree::NoCheckout { repository: repo() });
        trees.push(WorkTree::Checkout {
            repository: None,
            head: Head::Detached,
            changes: Changes::default(),
            stashes: 0,
            changed: None,
        });
        for tree in trees {
            assert_eq!(WorkTree::of_api(&tree.to_api("/w/repo")).as_ref(), Some(&tree));
        }

        let checkout = |head, changes| {
            Some(v1::work_tree::Stands::Checkout(v1::Checkout {
                head,
                changes,
                stashes: 0,
                repository: None,
                changed: None,
            }))
        };
        let branch = |name: &str| {
            let branch = v1::Branch { name: name.to_owned(), upstream: None };
            Some(v1::checkout::Head::Branch(branch))
        };
        let unread = [
            None,
            checkout(None, Some(v1::Changes::default())),
            checkout(branch("main"), None),
            checkout(branch(""), Some(v1::Changes::default())),
            Some(v1::work_tree::Stands::Checkout(v1::Checkout {
                head: branch("main"),
                changes: Some(v1::Changes::default()),
                stashes: 0,
                repository: Some(String::new()),
                changed: None,
            })),
            Some(v1::work_tree::Stands::NoCheckout(v1::work_tree::NoCheckout {
                repository: String::new(),
            })),
        ];
        for stands in unread {
            let tree = v1::WorkTree { dir: "/w/repo".to_owned(), stands };
            assert_eq!(WorkTree::of_api(&tree), None, "{tree:?}");
        }
    }
}
