use crate::said::Said;
use crate::v1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockedReason {
    Permission,
    Question,
    Auth,
    Dialog,
    Other(Said),
    Unspecified,
}

impl BlockedReason {
    #[must_use]
    pub fn of_waiting_for(word: Said) -> Self {
        match word.as_str().to_ascii_lowercase().as_str() {
            "permission prompt" => BlockedReason::Permission,
            "input needed" => BlockedReason::Question,
            "dialog open" => BlockedReason::Dialog,
            "auth" => BlockedReason::Auth,
            _ => BlockedReason::Other(word),
        }
    }

    #[must_use]
    pub fn to_api_waiting_for(&self) -> Option<v1::agent::WaitingFor> {
        let known = |waits: v1::WaitingFor| Some(v1::agent::WaitingFor::KnownWaitingFor(waits.into()));
        match self {
            BlockedReason::Permission => known(v1::WaitingFor::PermissionPrompt),
            BlockedReason::Question => known(v1::WaitingFor::InputNeeded),
            BlockedReason::Auth => known(v1::WaitingFor::Auth),
            BlockedReason::Dialog => known(v1::WaitingFor::DialogOpen),
            BlockedReason::Other(word) => Some(v1::agent::WaitingFor::OtherWaitingFor(word.clone().into())),
            BlockedReason::Unspecified => None,
        }
    }

    #[must_use]
    pub fn to_api_reason(&self) -> Option<v1::blocked_status::Reason> {
        let known = |waits: v1::WaitingFor| Some(v1::blocked_status::Reason::Known(waits.into()));
        match self {
            BlockedReason::Permission => known(v1::WaitingFor::PermissionPrompt),
            BlockedReason::Question => known(v1::WaitingFor::InputNeeded),
            BlockedReason::Auth => known(v1::WaitingFor::Auth),
            BlockedReason::Dialog => known(v1::WaitingFor::DialogOpen),
            BlockedReason::Other(word) => Some(v1::blocked_status::Reason::Other(word.clone().into())),
            BlockedReason::Unspecified => None,
        }
    }

    #[must_use]
    pub fn of_api_reason(reason: Option<&v1::blocked_status::Reason>) -> Self {
        match reason {
            Some(v1::blocked_status::Reason::Known(known)) => of_known_waiting_for(*known),
            Some(v1::blocked_status::Reason::Other(word)) => {
                trimmed_said(Some(word.as_str())).map_or(BlockedReason::Unspecified, BlockedReason::Other)
            },
            None => BlockedReason::Unspecified,
        }
    }

    #[must_use]
    pub fn of_api_waiting_for(waits: Option<&v1::agent::WaitingFor>) -> Self {
        match waits {
            Some(v1::agent::WaitingFor::KnownWaitingFor(known)) => of_known_waiting_for(*known),
            Some(v1::agent::WaitingFor::OtherWaitingFor(word)) => {
                trimmed_said(Some(word.as_str())).map_or(BlockedReason::Unspecified, BlockedReason::Other)
            },
            None => BlockedReason::Unspecified,
        }
    }
}

fn of_known_waiting_for(known: i32) -> BlockedReason {
    match v1::WaitingFor::try_from(known) {
        Ok(v1::WaitingFor::PermissionPrompt) => BlockedReason::Permission,
        Ok(v1::WaitingFor::InputNeeded) => BlockedReason::Question,
        Ok(v1::WaitingFor::Auth) => BlockedReason::Auth,
        Ok(v1::WaitingFor::DialogOpen) => BlockedReason::Dialog,
        Ok(v1::WaitingFor::Unspecified) | Err(_) => BlockedReason::Unspecified,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    Working,
    Idle,
    Stopped,
    Done { message: Option<Said> },
    Blocked { reason: BlockedReason, message: Option<Said> },
    Error { message: Option<Said> },
    Shell,
    Other(Said),
    Unreported,
}

impl AgentStatus {
    #[must_use]
    pub fn of_api(agent: &v1::Agent) -> Self {
        if let Some(reported) = &agent.reported_status {
            return match reported {
                v1::agent::ReportedStatus::Working(()) => AgentStatus::Working,
                v1::agent::ReportedStatus::Idle(()) => AgentStatus::Idle,
                v1::agent::ReportedStatus::Stopped(()) => AgentStatus::Stopped,
                v1::agent::ReportedStatus::Done(done) => {
                    AgentStatus::Done { message: trimmed_said(done.message.as_deref()) }
                },
                v1::agent::ReportedStatus::Blocked(blocked) => AgentStatus::Blocked {
                    reason: BlockedReason::of_api_reason(blocked.reason.as_ref()),
                    message: trimmed_said(blocked.message.as_deref()),
                },
                v1::agent::ReportedStatus::Error(error) => {
                    AgentStatus::Error { message: trimmed_said(error.message.as_deref()) }
                },
                v1::agent::ReportedStatus::Shell(()) => AgentStatus::Shell,
                v1::agent::ReportedStatus::OtherReportedStatus(word) => {
                    trimmed_said(Some(word.as_str())).map_or(AgentStatus::Unreported, AgentStatus::Other)
                },
            };
        }
        match &agent.status {
            Some(v1::agent::Status::KnownStatus(known)) => match v1::Status::try_from(*known) {
                Ok(v1::Status::Busy) => AgentStatus::Working,
                Ok(v1::Status::Idle) => {
                    if agent.status_rung == Some(v1::StatusRung::Osc7501.into()) {
                        AgentStatus::Stopped
                    } else {
                        AgentStatus::Idle
                    }
                },
                Ok(v1::Status::Stopped) => AgentStatus::Stopped,
                Ok(v1::Status::Done) => AgentStatus::Done { message: None },
                Ok(v1::Status::Waiting) => AgentStatus::Blocked {
                    reason: BlockedReason::of_api_waiting_for(agent.waiting_for.as_ref()),
                    message: None,
                },
                Ok(v1::Status::Error) => AgentStatus::Error { message: None },
                Ok(v1::Status::Shell) => AgentStatus::Shell,
                Ok(v1::Status::Unspecified) | Err(_) => AgentStatus::Unreported,
            },
            Some(v1::agent::Status::OtherStatus(word)) => {
                trimmed_said(Some(word.as_str())).map_or(AgentStatus::Unreported, AgentStatus::Other)
            },
            None => AgentStatus::Unreported,
        }
    }

    #[must_use]
    pub const fn is_finished_turn(&self) -> bool {
        matches!(self, AgentStatus::Done { .. } | AgentStatus::Idle)
    }
}

fn trimmed_said(text: Option<&str>) -> Option<Said> {
    Said::new(text?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reported_status_parses_distinct_stopped_done_error_and_blocked_reasons() {
        let stopped = v1::Agent {
            reported_status: Some(v1::agent::ReportedStatus::Stopped(())),
            status_rung: Some(v1::StatusRung::Osc7501.into()),
            ..v1::Agent::default()
        };
        assert_eq!(AgentStatus::of_api(&stopped), AgentStatus::Stopped);
        assert!(!AgentStatus::of_api(&stopped).is_finished_turn());

        let done = v1::Agent {
            reported_status: Some(v1::agent::ReportedStatus::Done(v1::DoneStatus {
                message: Some("  All good  ".to_owned()),
            })),
            ..v1::Agent::default()
        };
        assert_eq!(
            AgentStatus::of_api(&done),
            AgentStatus::Done { message: Said::new("All good".to_owned()) }
        );
        assert!(AgentStatus::of_api(&done).is_finished_turn());

        let empty_other = v1::Agent {
            reported_status: Some(v1::agent::ReportedStatus::Blocked(v1::BlockedStatus {
                reason: Some(v1::blocked_status::Reason::Other("   ".to_owned())),
                message: Some("   ".to_owned()),
            })),
            ..v1::Agent::default()
        };
        assert_eq!(
            AgentStatus::of_api(&empty_other),
            AgentStatus::Blocked { reason: BlockedReason::Unspecified, message: None }
        );
    }

    #[test]
    fn legacy_status_falls_back_cleanly_without_reported_status() {
        let legacy_idle = v1::Agent {
            status: Some(v1::agent::Status::KnownStatus(v1::Status::Idle.into())),
            ..v1::Agent::default()
        };
        assert_eq!(AgentStatus::of_api(&legacy_idle), AgentStatus::Idle);
        assert!(legacy_idle.is_finished_turn());

        let legacy_stopped = v1::Agent {
            status: Some(v1::agent::Status::KnownStatus(v1::Status::Stopped.into())),
            ..v1::Agent::default()
        };
        assert_eq!(AgentStatus::of_api(&legacy_stopped), AgentStatus::Stopped);
        assert!(!legacy_stopped.is_finished_turn());
    }
}
