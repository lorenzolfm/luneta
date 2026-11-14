use std::num::NonZeroU32;

use crate::v1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: NonZeroU32,
    pub state: State,
    pub url: Option<String>,
    pub checks: Option<Checks>,
    pub stale: bool,
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
    pub fn verdict(&self) -> Option<Verdict> {
        match self.state {
            State::Open | State::Draft => self.checks?.verdict(),
            State::Merged | State::Closed => None,
        }
    }

    #[must_use]
    pub fn to_api(&self, dir: &str) -> v1::PullRequest {
        let state = match self.state {
            State::Open => v1::PullRequestState::Open,
            State::Draft => v1::PullRequestState::Draft,
            State::Merged => v1::PullRequestState::Merged,
            State::Closed => v1::PullRequestState::Closed,
        };
        v1::PullRequest {
            dir: dir.to_owned(),
            number: self.number.get(),
            state: state.into(),
            url: self.url.clone().unwrap_or_default(),
            checks: self.checks.map(|Checks { pending, failed, passed }| v1::Checks {
                pending,
                failed,
                passed,
            }),
            stale: self.stale,
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
        let number = NonZeroU32::new(pull_request.number)?;
        let url = Some(pull_request.url.clone()).filter(|url| !url.is_empty());
        let checks = pull_request.checks.map(|v1::Checks { pending, failed, passed }| Checks {
            pending,
            failed,
            passed,
        });
        Some(PullRequest { number, state, url, checks, stale: pull_request.stale })
    }
}

#[cfg(test)]
mod tests {
    use super::{v1, Checks, NonZeroU32, PullRequest, State, Verdict};

    #[test]
    fn a_pull_request_reads_back_as_the_daemon_wrote_it() {
        let number = NonZeroU32::new(126).unwrap();
        let url = Some("https://github.com/owner/repo/pull/126".to_owned());
        let checks = Some(Checks { pending: 1, failed: 2, passed: 3 });
        for state in [State::Open, State::Draft, State::Merged, State::Closed] {
            let none = Some(Checks::default());
            let said = [(url.clone(), checks, false), (None, None, false), (None, none, true)];
            for (url, checks, stale) in said {
                let pull_request = PullRequest { number, state, url, checks, stale };
                let api = pull_request.to_api("/w/repo");
                assert_eq!(PullRequest::of_api(&api), Some(pull_request));
            }
        }

        let open = i32::from(v1::PullRequestState::Open);
        let unspecified = i32::from(v1::PullRequestState::Unspecified);
        for (number, state) in [(0, open), (126, unspecified), (126, 99)] {
            let dir = "/w/repo".to_owned();
            let url = String::new();
            let (checks, stale) = (None, false);
            let pull_request = v1::PullRequest { dir, number, state, url, checks, stale };
            assert_eq!(PullRequest::of_api(&pull_request), None, "{pull_request:?}");
        }
    }

    #[test]
    fn checks_come_to_the_worst_of_them() {
        let checks = |pending, failed, passed| Checks { pending, failed, passed }.verdict();
        assert_eq!(checks(3, 1, 5), Some(Verdict::Failing));
        assert_eq!(checks(0, 2, 0), Some(Verdict::Failing));
        assert_eq!(checks(1, 0, 5), Some(Verdict::Pending));
        assert_eq!(checks(0, 0, 5), Some(Verdict::Passing));
        assert_eq!(checks(0, 0, 0), None);
    }

    #[test]
    fn only_an_open_pull_request_says_its_checks() {
        let number = NonZeroU32::new(126).unwrap();
        let failed = Some(Checks { pending: 0, failed: 1, passed: 0 });
        let verdict = |state, checks| {
            PullRequest { number, state, url: None, checks, stale: false }.verdict()
        };
        assert_eq!(verdict(State::Open, failed), Some(Verdict::Failing));
        assert_eq!(verdict(State::Draft, failed), Some(Verdict::Failing));
        assert_eq!(verdict(State::Merged, failed), None);
        assert_eq!(verdict(State::Closed, failed), None);
        assert_eq!(verdict(State::Open, None), None);
    }
}
