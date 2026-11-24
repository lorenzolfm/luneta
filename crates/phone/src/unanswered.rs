use std::fmt;
use std::time::Duration;

use tonic::{Code, Status};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusals {
    Changes,
    Filings,
    Others,
}

impl Refusals {
    #[must_use]
    pub fn codes(self) -> &'static [Code] {
        match self {
            Refusals::Changes => &[
                Code::InvalidArgument,
                Code::FailedPrecondition,
                Code::Aborted,
                Code::NotFound,
                Code::AlreadyExists,
                Code::Unimplemented,
            ],
            Refusals::Filings => {
                &[Code::InvalidArgument, Code::FailedPrecondition, Code::Unimplemented]
            },
            Refusals::Others => &[
                Code::InvalidArgument,
                Code::FailedPrecondition,
                Code::NotFound,
                Code::Unimplemented,
            ],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum Unanswered {
    Refused { why: String },
    InDoubt { why: String },
}

impl Unanswered {
    #[must_use]
    pub fn of(status: &Status, refusals: Refusals) -> Unanswered {
        let why = status.message().to_owned();
        let why = if why.is_empty() { format!("luned said {}", status.code()) } else { why };
        if refusals.codes().contains(&status.code()) {
            Unanswered::Refused { why }
        } else {
            Unanswered::InDoubt { why }
        }
    }

    #[must_use]
    pub fn late(within: Duration) -> Unanswered {
        Unanswered::InDoubt { why: format!("luned did not answer in {}s", within.as_secs()) }
    }

    pub(crate) fn unread(why: impl fmt::Display) -> Unanswered {
        Unanswered::InDoubt { why: format!("the core can't read luned's answer: {why}") }
    }

    pub(crate) fn refused(why: impl fmt::Display) -> Unanswered {
        Unanswered::Refused { why: why.to_string() }
    }

    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unanswered::Refused { why } | Unanswered::InDoubt { why } => why,
        }
    }
}

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unanswered::Refused { why } => write!(f, "refused: {why}"),
            Unanswered::InDoubt { why } => write!(f, "{why}, so it may have been done"),
        }
    }
}

impl std::error::Error for Unanswered {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_told_apart_from_an_answer_that_may_have_been_done() {
        let dirty = Status::failed_precondition("fix has changes");
        let refused = Unanswered::of(&dirty, Refusals::Others);
        assert_eq!(refused, Unanswered::Refused { why: "fix has changes".to_owned() });
        let in_doubt = Unanswered::of(&Status::unavailable("the bridge went"), Refusals::Changes);
        assert_eq!(in_doubt, Unanswered::InDoubt { why: "the bridge went".to_owned() });
        let unsaid = Unanswered::of(&Status::new(Code::Internal, ""), Refusals::Others);
        assert_eq!(unsaid.why(), "luned said Internal error");
    }

    #[test]
    fn an_aborted_answer_is_a_refusal_only_of_a_change() {
        let aborted = || Status::aborted("the worktree may be made");
        let stale = Unanswered::of(&aborted(), Refusals::Changes);
        assert!(matches!(stale, Unanswered::Refused { .. }), "a seen list that changed since");
        for refusals in [Refusals::Filings, Refusals::Others] {
            let read = Unanswered::of(&aborted(), refusals);
            assert!(matches!(read, Unanswered::InDoubt { .. }), "{refusals:?}");
        }
        let unfiled = Unanswered::of(&Status::not_found("far went"), Refusals::Filings);
        assert!(matches!(unfiled, Unanswered::InDoubt { .. }), "the bridge may have filed it");
    }
}
