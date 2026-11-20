use tonic::{Code, Status};

use crate::projects::Failed;

const REFUSALS: [Code; 6] = [
    Code::InvalidArgument,
    Code::FailedPrecondition,
    Code::Aborted,
    Code::NotFound,
    Code::AlreadyExists,
    Code::Unimplemented,
];

#[derive(Debug, PartialEq, Eq)]
pub enum Unchanged {
    Refused(String),
    InDoubt(String),
}

impl Unchanged {
    #[must_use]
    pub fn of(status: &Status) -> Unchanged {
        let why = status.message().to_owned();
        if REFUSALS.contains(&status.code()) {
            Unchanged::Refused(why)
        } else {
            Unchanged::InDoubt(why)
        }
    }

    #[must_use]
    pub fn said(self, refused: &str, in_doubt: &str) -> String {
        match self {
            Unchanged::Refused(why) => format!("{refused}: {why}"),
            Unchanged::InDoubt(why) => format!("{why}, so {in_doubt}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Unadded {
    Nests(String),
    Unchanged(Unchanged),
}

impl Unadded {
    #[must_use]
    pub fn of(status: &Status) -> Unadded {
        match status.code() {
            Code::AlreadyExists => Unadded::Nests(status.message().to_owned()),
            _ => Unadded::Unchanged(Unchanged::of(status)),
        }
    }
}

impl From<Unchanged> for Unadded {
    fn from(unchanged: Unchanged) -> Unadded {
        Unadded::Unchanged(unchanged)
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

#[must_use]
pub fn unsent(status: &Status) -> Status {
    Status::failed_precondition(status.message())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn left(failed: &Failed) -> fn(String) -> Unadded {
        match failed {
            Failed::NotKept(_)
            | Failed::Stale(_)
            | Failed::Refused(_)
            | Failed::Unwritten(_)
            | Failed::Gone(_) => |why| Unadded::Unchanged(Unchanged::Refused(why)),
            Failed::Nests(_) => Unadded::Nests,
            Failed::Unclosed(_) | Failed::Unopened(_) | Failed::KeptUntold => {
                |why| Unadded::Unchanged(Unchanged::InDoubt(why))
            },
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
            let read = left(sent.message().to_owned());
            assert_eq!(Unadded::of(&sent), read, "{sent:?}");
            let unchanged = match read {
                Unadded::Nests(why) => Unchanged::Refused(why),
                Unadded::Unchanged(unchanged) => unchanged,
            };
            assert_eq!(Unchanged::of(&sent), unchanged, "a change that adds nothing: {sent:?}");
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

    #[test]
    fn a_change_the_bridge_never_sent_reads_as_a_refusal() {
        let unreached = Status::unavailable("lunetad does not reach far now");
        let unsent = unsent(&unreached);
        let refused = Unchanged::Refused("lunetad does not reach far now".to_owned());
        assert_eq!(Unchanged::of(&unsent), refused);
    }

    #[test]
    fn only_what_kept_nothing_is_said_unchanged() {
        let said = |unchanged: Unchanged| unchanged.said("not closed", "it may be closed");
        assert_eq!(said(Unchanged::Refused("w".to_owned())), "not closed: w");
        assert_eq!(said(Unchanged::InDoubt("w".to_owned())), "w, so it may be closed");
    }
}
