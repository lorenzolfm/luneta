use tonic::{Code, Status};

use crate::projects::Failed;

const REFUSALS: [Code; 5] = [
    Code::InvalidArgument,
    Code::FailedPrecondition,
    Code::Aborted,
    Code::NotFound,
    Code::Unimplemented,
];

#[derive(Debug, PartialEq, Eq)]
pub enum Unchanged {
    Refused(String),
    Nests(String),
    InDoubt(String),
}

impl Unchanged {
    #[must_use]
    pub fn of(status: &Status) -> Unchanged {
        let why = status.message().to_owned();
        match status.code() {
            Code::AlreadyExists => Unchanged::Nests(why),
            code if REFUSALS.contains(&code) => Unchanged::Refused(why),
            _ => Unchanged::InDoubt(why),
        }
    }
}

#[must_use]
pub fn status(failed: Failed) -> Status {
    let code = match &failed {
        Failed::Refused(_) => Code::InvalidArgument,
        Failed::NotKept(_) | Failed::Unwritten(_) => Code::FailedPrecondition,
        Failed::Stale(_) => Code::Aborted,
        Failed::Gone(_) => Code::NotFound,
        Failed::Nests(_) => Code::AlreadyExists,
        Failed::Unclosed(_) | Failed::Unopened(_) | Failed::KeptUntold => Code::Unavailable,
    };
    Status::new(code, failed.said())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(failed: &Failed) -> fn(String) -> Unchanged {
        match failed {
            Failed::NotKept(_)
            | Failed::Stale(_)
            | Failed::Refused(_)
            | Failed::Unwritten(_)
            | Failed::Gone(_) => Unchanged::Refused,
            Failed::Nests(_) => Unchanged::Nests,
            Failed::Unclosed(_) | Failed::Unopened(_) | Failed::KeptUntold => Unchanged::InDoubt,
        }
    }

    #[test]
    fn a_failure_reads_back_as_a_refusal_only_when_it_kept_nothing() {
        let why = || "w".to_owned();
        let every = [
            Failed::NotKept(why()),
            Failed::Nests(why()),
            Failed::Stale(why()),
            Failed::Refused(why()),
            Failed::Unwritten(why()),
            Failed::Unclosed(why()),
            Failed::Unopened(why()),
            Failed::Gone(why()),
            Failed::KeptUntold,
        ];
        for failed in every {
            let left = left(&failed);
            let sent = status(failed);
            assert_eq!(Unchanged::of(&sent), left(sent.message().to_owned()), "{sent:?}");
        }
    }

    #[test]
    fn a_code_only_tonic_or_the_bridge_sends_reads_as_in_doubt() {
        let why = || "w".to_owned();
        let unanswered = [
            Code::Unavailable,
            Code::Cancelled,
            Code::DeadlineExceeded,
            Code::Unknown,
            Code::Internal,
            Code::ResourceExhausted,
            Code::DataLoss,
        ];
        for code in unanswered {
            assert_eq!(Unchanged::of(&Status::new(code, why())), Unchanged::InDoubt(why()));
        }
        let older = Status::unimplemented(why());
        assert_eq!(Unchanged::of(&older), Unchanged::Refused(why()), "a lunetad older than it");
    }
}
