use std::fmt;
use std::time::Duration;

use tonic::{Code, Status};

const REFUSALS: [Code; 7] = [
    Code::InvalidArgument,
    Code::FailedPrecondition,
    Code::Aborted,
    Code::NotFound,
    Code::AlreadyExists,
    Code::Unimplemented,
    Code::PermissionDenied,
];

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum Unanswered {
    Refused { why: String },
    InDoubt { why: String },
}

impl Unanswered {
    #[must_use]
    pub fn of(status: &Status) -> Unanswered {
        let why = status.message().to_owned();
        let why = if why.is_empty() { format!("luned said {}", status.code()) } else { why };
        if REFUSALS.contains(&status.code()) {
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
        let refused = Unanswered::of(&Status::failed_precondition("fix has changes"));
        assert_eq!(refused, Unanswered::Refused { why: "fix has changes".to_owned() });
        let in_doubt = Unanswered::of(&Status::unavailable("the bridge went"));
        assert_eq!(in_doubt, Unanswered::InDoubt { why: "the bridge went".to_owned() });
        let unsaid = Unanswered::of(&Status::new(Code::Internal, ""));
        assert_eq!(unsaid.why(), "luned said Internal error");
    }
}
