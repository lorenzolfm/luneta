//! The pull request of a branch, as gh finds it on GitHub: what the daemon
//! asks gh of the branch checked out where each pane works, and what luneta
//! reads back of it.

use std::num::NonZeroU32;

use crate::v1;

/// A pull request: its number, which GitHub counts from one, its state, and
/// its page on GitHub, which says the repository the number counts in. A
/// daemon older than the page did not say it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: NonZeroU32,
    pub state: State,
    pub url: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Open,
    /// Open, and a draft.
    Draft,
    Merged,
    /// Closed, and not merged.
    Closed,
}

impl PullRequest {
    /// The pull request as the API gives it, of the branch checked out in
    /// `dir`.
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
        }
    }

    /// What `to_api` wrote out, read back. None for one with no number, or
    /// with no state, or one the protocol has not had so far: a newer
    /// daemon's. An older daemon's has no page.
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
        Some(PullRequest { number, state, url })
    }
}

#[cfg(test)]
mod tests {
    use super::{v1, NonZeroU32, PullRequest, State};

    /// What the daemon writes out of a pull request reads back as the same
    /// pull request, so that luneta says of it what gh said to the daemon.
    /// One with no number, or no state, or a state of a newer daemon's, reads
    /// as none. An older daemon's, which said no page, reads with none.
    #[test]
    fn a_pull_request_reads_back_as_the_daemon_wrote_it() {
        let number = NonZeroU32::new(126).unwrap();
        let url = Some("https://github.com/owner/repo/pull/126".to_owned());
        for state in [State::Open, State::Draft, State::Merged, State::Closed] {
            for url in [url.clone(), None] {
                let pull_request = PullRequest { number, state, url };
                let api = pull_request.to_api("/w/repo");
                assert_eq!(PullRequest::of_api(&api), Some(pull_request));
            }
        }

        let open = i32::from(v1::PullRequestState::Open);
        let unspecified = i32::from(v1::PullRequestState::Unspecified);
        for (number, state) in [(0, open), (126, unspecified), (126, 99)] {
            let dir = "/w/repo".to_owned();
            let pull_request = v1::PullRequest { dir, number, state, url: String::new() };
            assert_eq!(PullRequest::of_api(&pull_request), None, "{pull_request:?}");
        }
    }
}
