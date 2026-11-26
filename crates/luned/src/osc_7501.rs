use lune_domain::agent::{Declared, Rung};
use lune_protocol::program_status::{
    ChildProgramStatus, OscBlockedKind, ProgramPhase, ProgramStatus,
};

use crate::process::AgentId;
use crate::statuses::{ActiveStatus, BlockedReason, Report, RungUpdate, Subagent, SubagentPhase};

fn reason(kind: Option<OscBlockedKind>) -> BlockedReason {
    match kind {
        None => BlockedReason::Unspecified,
        Some(OscBlockedKind::Permission) => BlockedReason::Permission,
        Some(OscBlockedKind::Question) => BlockedReason::Question,
        Some(OscBlockedKind::Auth) => BlockedReason::Auth,
    }
}

pub(crate) fn phase(phase: &ProgramPhase) -> ActiveStatus {
    match phase {
        ProgramPhase::Idle => ActiveStatus::Stopped,
        ProgramPhase::Working { .. } => ActiveStatus::Working,
        ProgramPhase::Done { message } => ActiveStatus::Done { message: message.clone() },
        ProgramPhase::Blocked { kind, message } => {
            ActiveStatus::Blocked { reason: reason(*kind), message: message.clone() }
        },
        ProgramPhase::Error { message } => ActiveStatus::Error { message: message.clone() },
    }
}

fn subagent_phase(phase: &ProgramPhase) -> SubagentPhase {
    match phase {
        ProgramPhase::Idle => SubagentPhase::Stopped,
        ProgramPhase::Working { progress } => SubagentPhase::Working { progress: *progress },
        ProgramPhase::Done { message } => SubagentPhase::Done { message: message.clone() },
        ProgramPhase::Blocked { kind, message } => {
            SubagentPhase::Blocked { reason: reason(*kind), message: message.clone() }
        },
        ProgramPhase::Error { message } => SubagentPhase::Error { message: message.clone() },
    }
}

pub(crate) fn report(
    reads: Declared,
    agent: AgentId,
    status: &ProgramStatus,
    subagents: Vec<Subagent>,
) -> Report {
    assert_eq!(reads.rung(), Rung::Osc7501, "an OSC 7501 record reports rung 1");
    let update = RungUpdate::Active { status: phase(&status.phase), at: status.at, subagents };
    Report::new(reads, agent, update)
}

pub(crate) fn subagent(child: &ChildProgramStatus) -> Subagent {
    Subagent {
        id: child.id.clone(),
        phase: subagent_phase(&child.status.phase),
        title: child.status.title.clone(),
        status_since: child.status.at,
    }
}
