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
        let said = status.message();
        let mut why = if said.is_empty() { format!("luned said {}", status.code()) } else { said.to_owned() };
        let mut cause = std::error::Error::source(status);
        while let Some(error) = cause {
            let told = error.to_string();
            if !why.split(": ").any(|part| part == told) {
                why = format!("{why}: {told}");
            }
            cause = error.source();
        }
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

    #[derive(Debug)]
    struct Said(&'static str, Option<Box<Said>>);

    impl fmt::Display for Said {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.0)
        }
    }

    impl std::error::Error for Said {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.1.as_deref().map(|said| said as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn a_failure_to_reach_luned_says_each_cause_once() {
        let refused = Said("Connection refused (os error 61)", None);
        let connect = Said("tcp connect error", Some(Box::new(refused)));
        let transport = Said("transport error", Some(Box::new(connect)));
        let status = Status::from_error(Box::new(transport));
        assert_eq!(status.message(), "transport error");
        let unreached = Unanswered::of(&status, Refusals::Others);
        let why = "transport error: tcp connect error: Connection refused (os error 61)";
        assert_eq!(unreached, Unanswered::InDoubt { why: why.to_owned() });
        let late = Said("deadline has elapsed", None);
        let connect = Said("tcp connect error", Some(Box::new(late)));
        let transport = Said("transport error", Some(Box::new(connect)));
        let mut status = Status::unavailable("tcp connect error");
        status.set_source(std::sync::Arc::new(transport));
        let late = Unanswered::of(&status, Refusals::Others);
        assert_eq!(late.why(), "tcp connect error: transport error: deadline has elapsed");
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
