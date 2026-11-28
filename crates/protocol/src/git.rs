use std::collections::HashMap;
use std::collections::hash_map::Entry;

use crate::said::Said;
use crate::v1;

pub const GIT_STATUSES_MAX: usize = 2048;

const _: () = assert!(GIT_STATUSES_MAX > 0);

const DETACHED: &str = "(detached)";

pub const STATUS: [&str; 3] = ["--porcelain=v2", "--branch", "--show-stash"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitStatus {
    Outside,
    NoCheckout {
        repository: Said,
    },
    Checkout {
        repository: Option<Said>,
        head: Head,
        changes: Changes,
        stashes: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Changed {
    added: u32,
    deleted: u32,
}

impl Changed {
    #[must_use]
    pub fn new(added: u32, deleted: u32) -> Option<Self> {
        (added > 0 || deleted > 0).then_some(Changed { added, deleted })
    }

    #[must_use]
    pub fn added(self) -> u32 {
        self.added
    }

    #[must_use]
    pub fn deleted(self) -> u32 {
        self.deleted
    }

    fn of_api(changed: v1::Changed) -> Option<Self> {
        Changed::new(changed.added, changed.deleted)
    }

    fn to_api(self) -> v1::Changed {
        v1::Changed {
            added: self.added,
            deleted: self.deleted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    Detached,
    Branch {
        name: Said,
        upstream: Option<Upstream>,
        changed: Option<Changed>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upstream {
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub conflicted: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub modified: u32,
    pub staged: u32,
    pub untracked: u32,
}

impl GitStatus {
    #[must_use]
    pub fn to_api(&self, dir: &str) -> v1::GitStatus {
        let stands = match self {
            GitStatus::Outside => v1::git_status::Stands::Outside(()),
            GitStatus::NoCheckout { repository } => {
                v1::git_status::Stands::NoCheckout(v1::git_status::NoCheckout {
                    repository: repository.as_str().to_owned(),
                })
            }
            GitStatus::Checkout {
                repository,
                head,
                changes,
                stashes,
            } => v1::git_status::Stands::Checkout(v1::Checkout {
                head: Some(head.to_api()),
                changes: Some(changes.to_api()),
                stashes: *stashes,
                repository: repository.as_ref().map(|it| it.as_str().to_owned()),
            }),
        };
        v1::GitStatus {
            dir: dir.to_owned(),
            stands: Some(stands),
        }
    }

    #[must_use]
    pub fn of_api(status: &v1::GitStatus) -> Option<Self> {
        let checkout = match status.stands.as_ref()? {
            v1::git_status::Stands::Outside(()) => return Some(GitStatus::Outside),
            v1::git_status::Stands::NoCheckout(it) => {
                let repository = Said::new(it.repository.clone())?;
                return Some(GitStatus::NoCheckout { repository });
            }
            v1::git_status::Stands::Checkout(checkout) => checkout,
        };
        let repository = match &checkout.repository {
            Some(repository) => Some(Said::new(repository.clone())?),
            None => None,
        };
        Some(GitStatus::Checkout {
            repository,
            head: Head::of_api(checkout.head.as_ref()?)?,
            changes: Changes::of_api(checkout.changes.as_ref()?),
            stashes: checkout.stashes,
        })
    }

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
            } else if !changes.add(line) {
                return Err(unread());
            }
        }
        let head = head.ok_or("no line that says what is checked out")?;
        Ok(GitStatus::Checkout {
            repository: Some(repository),
            head,
            changes,
            stashes,
        })
    }
}

impl Head {
    fn of(named: &str) -> Option<Self> {
        if named == DETACHED {
            return Some(Head::Detached);
        }
        Some(Head::Branch {
            name: Said::new(named.to_owned())?,
            upstream: None,
            changed: None,
        })
    }

    fn of_api(head: &v1::checkout::Head) -> Option<Self> {
        match head {
            v1::checkout::Head::Detached(()) => Some(Head::Detached),
            v1::checkout::Head::Branch(branch) => Some(Head::Branch {
                name: Said::new(branch.name.clone())?,
                upstream: branch.upstream.map(Upstream::of_api),
                changed: branch.changed.and_then(Changed::of_api),
            }),
        }
    }

    fn to_api(&self) -> v1::checkout::Head {
        match self {
            Head::Detached => v1::checkout::Head::Detached(()),
            Head::Branch {
                name,
                upstream,
                changed,
            } => v1::checkout::Head::Branch(v1::Branch {
                name: name.as_str().to_owned(),
                upstream: upstream.map(Upstream::to_api),
                changed: changed.map(Changed::to_api),
            }),
        }
    }
}

impl Upstream {
    fn of(ab: &str) -> Option<Self> {
        let (ahead, behind) = ab.split_once(' ')?;
        let ahead = ahead.strip_prefix('+')?.parse().ok()?;
        Some(Upstream {
            ahead,
            behind: behind.strip_prefix('-')?.parse().ok()?,
        })
    }

    fn of_api(upstream: v1::Upstream) -> Self {
        Upstream {
            ahead: upstream.ahead,
            behind: upstream.behind,
        }
    }

    fn to_api(self) -> v1::Upstream {
        v1::Upstream {
            ahead: self.ahead,
            behind: self.behind,
        }
    }
}

fn states(xy: &str) -> Option<(char, char)> {
    let mut states = xy.chars();
    let (Some(index), Some(tree), None) = (states.next(), states.next(), states.next()) else {
        return None;
    };
    Some((index, tree))
}

impl Changes {
    fn add(&mut self, line: &str) -> bool {
        let mut fields = line.split(' ');
        match (fields.next(), fields.next().and_then(states)) {
            (Some("1"), Some(states)) => self.add_change(states),
            (Some("2"), Some(states)) => {
                self.renamed += 1;
                self.add_change(states);
            }
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
        self.modified += u32::from(matches!(tree, 'M' | 'A' | 'T'));
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedGitStatuses<'a> {
    pub by_dir: HashMap<&'a str, GitStatus>,
    pub dropped_invalid: usize,
    pub dropped_duplicate: usize,
    pub truncated: usize,
}

impl<'a> ParsedGitStatuses<'a> {
    #[must_use]
    pub fn of_api(statuses: &'a [v1::GitStatus]) -> Self {
        let mut parsed = Self {
            truncated: statuses.len().saturating_sub(GIT_STATUSES_MAX),
            ..Self::default()
        };
        for status in statuses.iter().take(GIT_STATUSES_MAX) {
            if status.dir.is_empty() {
                parsed.dropped_invalid += 1;
                continue;
            }
            let Some(read) = GitStatus::of_api(status) else {
                parsed.dropped_invalid += 1;
                continue;
            };
            match parsed.by_dir.entry(status.dir.as_str()) {
                Entry::Vacant(slot) => {
                    slot.insert(read);
                }
                Entry::Occupied(_) => parsed.dropped_duplicate += 1,
            }
        }
        parsed
    }

    #[must_use]
    pub fn anomalies(&self) -> Option<String> {
        let mut notes = Vec::new();
        if self.dropped_invalid > 0 {
            notes.push(format!("{} unreadable", self.dropped_invalid));
        }
        if self.dropped_duplicate > 0 {
            notes.push(format!("{} duplicate", self.dropped_duplicate));
        }
        if self.truncated > 0 {
            notes.push(format!(
                "{} past the {GIT_STATUSES_MAX} limit",
                self.truncated
            ));
        }
        (!notes.is_empty()).then(|| format!("git statuses dropped: {}", notes.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::{Changed, Changes, GIT_STATUSES_MAX, GitStatus, Head, ParsedGitStatuses, Said, v1};

    fn repo() -> Said {
        Said::new("/w/repo".to_owned()).unwrap()
    }

    #[test]
    fn a_status_that_does_not_read_is_refused() {
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
            assert!(
                GitStatus::parse(&format!("{head}{line}\n"), repo()).is_err(),
                "{line:?}"
            );
        }
        for status in ["? new\n", "# branch.head (detached)\n# branch.ab +1 -0\n"] {
            assert!(GitStatus::parse(status, repo()).is_err(), "{status:?}");
        }
        let status =
            GitStatus::parse(&format!("{head}# branch.to-come 1\n? new\n"), repo()).unwrap();
        let GitStatus::Checkout {
            head:
                Head::Branch {
                    name,
                    upstream: None,
                    ..
                },
            changes,
            ..
        } = status
        else {
            panic!("main, with no upstream: {status:?}");
        };
        assert_eq!(name.as_str(), "main");
        assert_eq!(changes.untracked, 1);
    }

    #[test]
    fn a_git_status_reads_back_as_the_daemon_wrote_it() {
        let said = [
            "# branch.head main\n",
            "# branch.oid 0123abc\n# branch.head fold\n# branch.upstream origin/fold\n\
             # branch.ab +1 -2\n# stash 3\n1 MM N... 100644 100644 100644 a b README.md\n\
             2 R. N... 100644 100644 100644 a a R100 new.rs\told.rs\n\
             u UU N... 100644 100644 100644 100644 a b c both.rs\n\
             1 .D N... 100644 100644 000000 a a gone.rs\n? new\n",
            "# branch.head (detached)\n1 .T N... 100644 100644 120000 a a link\n",
        ];
        let mut statuses: Vec<GitStatus> = said
            .iter()
            .map(|it| GitStatus::parse(it, repo()).unwrap())
            .collect();
        statuses.push(GitStatus::Outside);
        statuses.push(GitStatus::NoCheckout { repository: repo() });
        statuses.push(GitStatus::Checkout {
            repository: None,
            head: Head::Detached,
            changes: Changes::default(),
            stashes: 0,
        });
        let name = Said::new("fold".to_owned()).unwrap();
        let changed = Changed::new(120, 34);
        let head = Head::Branch {
            name,
            upstream: None,
            changed,
        };
        statuses.push(GitStatus::Checkout {
            repository: None,
            head,
            changes: Changes::default(),
            stashes: 0,
        });
        for status in statuses {
            assert_eq!(
                GitStatus::of_api(&status.to_api("/w/repo")).as_ref(),
                Some(&status)
            );
        }

        let checkout = |head, changes| {
            Some(v1::git_status::Stands::Checkout(v1::Checkout {
                head,
                changes,
                stashes: 0,
                repository: None,
            }))
        };
        let branch = |name: &str| {
            let branch = v1::Branch {
                name: name.to_owned(),
                upstream: None,
                changed: None,
            };
            Some(v1::checkout::Head::Branch(branch))
        };
        let none = v1::Changed {
            added: 0,
            deleted: 0,
        };
        let head = v1::Branch {
            name: "fold".to_owned(),
            upstream: None,
            changed: Some(none),
        };
        let stands = checkout(
            Some(v1::checkout::Head::Branch(head)),
            Some(v1::Changes::default()),
        );
        let status = GitStatus::of_api(&v1::GitStatus {
            dir: "/w/repo".to_owned(),
            stands,
        });
        let Some(GitStatus::Checkout {
            head: Head::Branch { changed, .. },
            ..
        }) = status
        else {
            panic!("fold: {status:?}");
        };
        assert_eq!(changed, None);
        let unread = [
            None,
            checkout(None, Some(v1::Changes::default())),
            checkout(branch("main"), None),
            checkout(branch(""), Some(v1::Changes::default())),
            Some(v1::git_status::Stands::Checkout(v1::Checkout {
                head: branch("main"),
                changes: Some(v1::Changes::default()),
                stashes: 0,
                repository: Some(String::new()),
            })),
            Some(v1::git_status::Stands::NoCheckout(
                v1::git_status::NoCheckout {
                    repository: String::new(),
                },
            )),
        ];
        for stands in unread {
            let status = v1::GitStatus {
                dir: "/w/repo".to_owned(),
                stands,
            };
            assert_eq!(GitStatus::of_api(&status), None, "{status:?}");
        }
    }

    #[test]
    fn parsed_git_statuses_deduplicate_validate_and_bound() {
        let first = GitStatus::Outside;
        let second = GitStatus::NoCheckout { repository: repo() };
        let invalid = v1::GitStatus {
            dir: "/w/bad".to_owned(),
            stands: None,
        };
        let blank_dir = first.to_api("");
        let items = [
            first.to_api("/w/a"),
            second.to_api("/w/a"),
            invalid,
            blank_dir,
        ];
        let parsed = ParsedGitStatuses::of_api(&items);
        assert_eq!(parsed.by_dir.get("/w/a"), Some(&first));
        assert_eq!(parsed.by_dir.len(), 1);
        assert_eq!(parsed.dropped_duplicate, 1);
        assert_eq!(parsed.dropped_invalid, 2);
        assert_eq!(parsed.truncated, 0);
        assert_eq!(
            parsed.anomalies().as_deref(),
            Some("git statuses dropped: 2 unreadable, 1 duplicate")
        );

        let many: Vec<v1::GitStatus> = (0..=GIT_STATUSES_MAX)
            .map(|at| first.to_api(&format!("/w/{at}")))
            .collect();
        let bounded = ParsedGitStatuses::of_api(&many);
        assert_eq!(bounded.by_dir.len(), GIT_STATUSES_MAX);
        assert_eq!(bounded.truncated, 1);
        assert!(bounded.anomalies().is_some());
    }
}
