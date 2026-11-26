use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::SystemTime;

use lune_domain::agent::{Declared, Harness, OscReads, Rung};
pub use lune_protocol::agent_status::BlockedReason;
use lune_protocol::program_status::{ChildId, Message, Progress, Title};
use lune_protocol::said::Said;
use lune_protocol::PROGRAM_STATUS_RECORDS_MAX;

use crate::agent::{Agent, SeenAgent, Sighting};
use crate::api;
use crate::api::agent::{ReportedStatus, Status as LegacyStatus, WaitingFor as Waits};
use crate::process::AgentId;

pub const SOURCE_AGENTS_MAX: usize = 1024;

pub const AGENTS_MAX: usize = 4096;

pub const SUBAGENTS_MAX: usize = PROGRAM_STATUS_RECORDS_MAX;

const _: () = assert!(SOURCE_AGENTS_MAX > 0);
const _: () = assert!(4 * SOURCE_AGENTS_MAX <= AGENTS_MAX);
const _: () = assert!(lune_protocol::MUX_TERMINALS_MAX <= SOURCE_AGENTS_MAX);
const _: () = assert!(SUBAGENTS_MAX >= 64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActiveStatus {
    Working,
    Idle,
    Stopped,
    Done { message: Option<Message> },
    Blocked { reason: BlockedReason, message: Option<Message> },
    Error { message: Option<Message> },
    Shell,
    Other(Said),
}

impl ActiveStatus {
    #[must_use]
    pub fn to_api(&self) -> (LegacyStatus, Option<Waits>, ReportedStatus) {
        let known = |status: api::Status| LegacyStatus::KnownStatus(status.into());
        match self {
            ActiveStatus::Working => (known(api::Status::Busy), None, ReportedStatus::Working(())),
            ActiveStatus::Idle => (known(api::Status::Idle), None, ReportedStatus::Idle(())),
            ActiveStatus::Stopped => {
                (known(api::Status::Stopped), None, ReportedStatus::Stopped(()))
            },
            ActiveStatus::Done { message } => (
                known(api::Status::Done),
                None,
                ReportedStatus::Done(api::DoneStatus {
                    message: message.clone().map(String::from),
                }),
            ),
            ActiveStatus::Blocked { reason, message } => (
                known(api::Status::Waiting),
                reason.to_api_waiting_for(),
                ReportedStatus::Blocked(api::BlockedStatus {
                    reason: reason.to_api_reason(),
                    message: message.clone().map(String::from),
                }),
            ),
            ActiveStatus::Error { message } => (
                known(api::Status::Error),
                None,
                ReportedStatus::Error(api::ErrorStatus {
                    message: message.clone().map(String::from),
                }),
            ),
            ActiveStatus::Shell => (known(api::Status::Shell), None, ReportedStatus::Shell(())),
            ActiveStatus::Other(word) => (
                LegacyStatus::OtherStatus(word.clone().into()),
                None,
                ReportedStatus::OtherReportedStatus(word.clone().into()),
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentPhase {
    Idle,
    Stopped,
    Working { progress: Option<Progress> },
    Done { message: Option<Message> },
    Blocked { reason: BlockedReason, message: Option<Message> },
    Error { message: Option<Message> },
    Shell,
    Other(Said),
}

impl SubagentPhase {
    #[must_use]
    pub fn to_api(&self) -> api::subagent::Phase {
        use api::subagent::Phase;
        match self {
            SubagentPhase::Idle => Phase::Idle(()),
            SubagentPhase::Stopped => Phase::Stopped(()),
            SubagentPhase::Working { progress } => {
                Phase::Working(api::WorkingStatus { progress: progress.map(|p| u32::from(p.get())) })
            },
            SubagentPhase::Done { message } => {
                Phase::Done(api::DoneStatus { message: message.clone().map(String::from) })
            },
            SubagentPhase::Blocked { reason, message } => Phase::Blocked(api::BlockedStatus {
                reason: reason.to_api_reason(),
                message: message.clone().map(String::from),
            }),
            SubagentPhase::Error { message } => {
                Phase::Error(api::ErrorStatus { message: message.clone().map(String::from) })
            },
            SubagentPhase::Shell => Phase::Shell(()),
            SubagentPhase::Other(word) => Phase::Other(word.clone().into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subagent {
    pub id: ChildId,
    pub phase: SubagentPhase,
    pub title: Option<Title>,
    pub status_since: Option<SystemTime>,
}

impl Subagent {
    #[must_use]
    pub fn to_api(&self) -> api::Subagent {
        api::Subagent {
            id: self.id.as_str().to_owned(),
            phase: Some(self.phase.to_api()),
            title: self.title.clone().map(String::from),
            status_since: self.status_since.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RungUpdate {
    Clear,
    Active { status: ActiveStatus, at: Option<SystemTime> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    agent: AgentId,
    reads: Declared,
    update: RungUpdate,
    subagents: Vec<Subagent>,
}

impl Report {
    #[must_use]
    pub fn new(reads: Declared, agent: AgentId, update: RungUpdate) -> Report {
        Report { agent, reads, update, subagents: Vec::new() }
    }

    #[must_use]
    pub fn osc_7501(
        reads: OscReads,
        agent: AgentId,
        status: ActiveStatus,
        at: Option<SystemTime>,
        subagents: Vec<Subagent>,
    ) -> Report {
        assert!(subagents.len() <= SUBAGENTS_MAX);
        let update = RungUpdate::Active { status, at };
        Report { agent, reads: reads.declared(), update, subagents }
    }

    #[must_use]
    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    #[must_use]
    pub fn reads(&self) -> Declared {
        self.reads
    }

    #[must_use]
    pub fn update(&self) -> &RungUpdate {
        &self.update
    }

    #[must_use]
    pub fn subagents(&self) -> &[Subagent] {
        &self.subagents
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedAgent {
    seen: SeenAgent,
    report: Report,
    proof: Proof,
}

impl ReportedAgent {
    #[must_use]
    pub fn new(
        reads: Declared,
        id: AgentId,
        sighting: Sighting,
        update: RungUpdate,
        proof: Proof,
    ) -> ReportedAgent {
        let seen = sighting.of(id.clone(), reads.harness());
        ReportedAgent { seen, report: Report::new(reads, id, update), proof }
    }

    #[must_use]
    pub fn seen(&self) -> &SeenAgent {
        &self.seen
    }

    #[must_use]
    pub fn report(&self) -> &Report {
        &self.report
    }

    #[must_use]
    pub fn proof(&self) -> Proof {
        self.proof
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Proof {
    Started { written: SystemTime },
    Dated,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided(Option<Reported>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Reported {
    status: ActiveStatus,
    since: Option<SystemTime>,
    rung: Rung,
    subagents: Vec<Subagent>,
}

impl Decided {
    #[must_use]
    pub fn status(&self) -> Option<&ActiveStatus> {
        self.0.as_ref().map(|reported| &reported.status)
    }

    #[must_use]
    pub fn since(&self) -> Option<SystemTime> {
        self.0.as_ref().and_then(|reported| reported.since)
    }

    #[must_use]
    pub fn rung(&self) -> Option<Rung> {
        self.0.as_ref().map(|reported| reported.rung)
    }

    #[must_use]
    pub fn subagents(&self) -> &[Subagent] {
        self.0.as_ref().map_or(&[], |reported| reported.subagents.as_slice())
    }
}

#[must_use]
pub fn parse_word(word: Option<&str>, waiting_for: Option<&str>) -> Option<ActiveStatus> {
    let word = said(word)?;
    Some(match word.as_str().to_ascii_lowercase().as_str() {
        "busy" | "working" => ActiveStatus::Working,
        "idle" => ActiveStatus::Idle,
        "stopped" => ActiveStatus::Stopped,
        "done" => ActiveStatus::Done { message: None },
        "error" => ActiveStatus::Error { message: None },
        "waiting" | "blocked" => ActiveStatus::Blocked {
            reason: said(waiting_for)
                .map_or(BlockedReason::Unspecified, BlockedReason::of_waiting_for),
            message: None,
        },
        "shell" => ActiveStatus::Shell,
        _ => ActiveStatus::Other(word),
    })
}

fn said(word: Option<&str>) -> Option<Said> {
    Said::new(word?.trim().to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Active {
    status: ActiveStatus,
    since: Option<SystemTime>,
    subagents: Vec<Subagent>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Rungs {
    harness: Harness,
    active: BTreeMap<Rung, Active>,
}

#[derive(Default)]
pub struct Statuses {
    agents: HashMap<AgentId, Rungs>,
}

impl Statuses {
    pub fn record(&mut self, report: Report) {
        let Report { agent, reads, update, subagents } = report;
        let (harness, rung) = (reads.harness(), reads.rung());
        let held_by_another = self.agents.get(&agent).is_some_and(|rungs| rungs.harness != harness);
        let RungUpdate::Active { status, at } = update else {
            if !held_by_another {
                self.clear_rung(&agent, rung);
            }
            return;
        };
        if held_by_another {
            self.agents.remove(&agent);
        }
        assert!(subagents.len() <= SUBAGENTS_MAX);
        if !self.agents.contains_key(&agent) && self.agents.len() >= AGENTS_MAX {
            return;
        }
        let rungs = self
            .agents
            .entry(agent.clone())
            .or_insert_with(|| Rungs { harness, active: BTreeMap::new() });
        let since = match rung {
            Rung::Osc7501 | Rung::Hook => match rungs.active.get(&rung) {
                Some(previous) if same_phase(&previous.status, &status) => previous.since.or(at),
                Some(_) | None => at,
            },
            Rung::Feed | Rung::Extension | Rung::Transcript | Rung::Screen => at,
        };
        rungs.active.insert(rung, Active { status, since, subagents });
        assert_eq!(rungs.harness, harness);
        assert!(rungs.active.len() <= Rung::ALL.len());
        assert!(self.agents.len() <= AGENTS_MAX);
        assert!(self.agents.contains_key(&agent));
    }

    pub fn clear_rung(&mut self, agent: &AgentId, rung: Rung) {
        if let Some(rungs) = self.agents.get_mut(agent) {
            rungs.active.remove(&rung);
            if rungs.active.is_empty() {
                self.agents.remove(agent);
            }
        }
        assert!(self.agents.get(agent).is_none_or(|rungs| !rungs.active.is_empty()));
        assert!(!self.agents.get(agent).is_some_and(|rungs| rungs.active.contains_key(&rung)));
    }

    pub fn forget(&mut self, agent: &AgentId, harness: Harness) {
        if self.agents.get(agent).is_some_and(|rungs| rungs.harness == harness) {
            self.agents.remove(agent);
        }
        assert!(self.agents.get(agent).is_none_or(|rungs| rungs.harness != harness));
    }

    pub fn replace_rung(&mut self, rung: Rung, reports: Vec<Report>) {
        assert!(reports.len() <= AGENTS_MAX);
        let seen: HashSet<&AgentId> = reports.iter().map(Report::agent).collect();
        assert_eq!(seen.len(), reports.len(), "each agent appears at most once in a rung snapshot");
        let stale: Vec<AgentId> = self
            .agents
            .iter()
            .filter(|(agent, rungs)| rungs.active.contains_key(&rung) && !seen.contains(*agent))
            .map(|(agent, _)| agent.clone())
            .collect();
        for agent in stale {
            self.clear_rung(&agent, rung);
        }
        for report in reports {
            assert_eq!(report.reads.rung(), rung, "each report in a rung snapshot names that rung");
            self.record(report);
        }
        assert!(self.agents.len() <= AGENTS_MAX);
    }

    pub fn retain_live(&mut self, live: &HashSet<AgentId>) {
        let before = self.agents.len();
        self.agents.retain(|agent, _| live.contains(agent));
        assert!(self.agents.len() <= before);
        assert!(self.agents.len() <= live.len());
    }

    #[must_use]
    pub fn decide(&self, agent: &AgentId) -> Decided {
        let Some(rungs) = self.agents.get(agent) else {
            return Decided(None);
        };
        assert!(!rungs.active.is_empty(), "cleared rungs remove empty agent entries");
        assert!(rungs.active.len() <= Rung::ALL.len());
        let (&rung, active) = rungs
            .active
            .first_key_value()
            .expect("non-empty rung map has a highest-priority entry");
        Decided(Some(Reported {
            status: active.status.clone(),
            since: active.since,
            rung,
            subagents: active.subagents.clone(),
        }))
    }

    #[must_use]
    pub fn apply(&self, seen: &SeenAgent) -> Agent {
        let status = self.decide(&seen.id);
        assert!(
            self.agents.get(&seen.id).is_none_or(|rungs| rungs.harness == seen.harness),
            "an agent's status comes from rungs of its own harness"
        );
        Agent {
            pid: seen.id.pid(),
            harness: seen.harness,
            session_id: seen.session_id.clone(),
            cwd: seen.cwd.clone(),
            status,
            started_at: seen.started_at,
            name: seen.name.clone(),
            entrypoint: seen.entrypoint.clone(),
            terminal: seen.terminal,
        }
    }
}

fn same_phase(previous: &ActiveStatus, current: &ActiveStatus) -> bool {
    match (previous, current) {
        (ActiveStatus::Working, ActiveStatus::Working)
        | (ActiveStatus::Idle, ActiveStatus::Idle)
        | (ActiveStatus::Stopped, ActiveStatus::Stopped)
        | (ActiveStatus::Done { .. }, ActiveStatus::Done { .. })
        | (ActiveStatus::Error { .. }, ActiveStatus::Error { .. })
        | (ActiveStatus::Shell, ActiveStatus::Shell) => true,
        (ActiveStatus::Blocked { reason: a, .. }, ActiveStatus::Blocked { reason: b, .. }) => {
            a == b
        },
        (ActiveStatus::Other(a), ActiveStatus::Other(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod scenarios;

#[cfg(test)]
pub(crate) mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::process::{AgentPid, Start};

    fn agent(pid: u32) -> AgentId {
        AgentId::new(AgentPid::new(pid).unwrap(), Start::spelled("100".to_owned()).unwrap())
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn report(id: &AgentId, reads: Declared, status: ActiveStatus, time: u64) -> Report {
        let update = RungUpdate::Active { status, at: Some(at(time)) };
        Report::new(reads, id.clone(), update)
    }

    pub(crate) fn decided(
        reads: Declared,
        status: ActiveStatus,
        since: Option<SystemTime>,
    ) -> Decided {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let update = RungUpdate::Active { status, at: since };
        statuses.record(Report::new(reads, id.clone(), update));
        statuses.decide(&id)
    }

    #[test]
    fn same_phase_on_osc_7501_preserves_since_across_updates() {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let reads = Declared::of(Harness::ClaudeCode, Rung::Osc7501);
        let summed = ActiveStatus::Done { message: Message::parse("message", "All done").ok() };

        statuses.record(report(&id, reads, ActiveStatus::Done { message: None }, 10));
        statuses.record(report(&id, reads, summed.clone(), 12));
        let done = statuses.decide(&id);
        assert_eq!((done.status(), done.since()), (Some(&summed), Some(at(10))));

        statuses.record(report(&id, reads, ActiveStatus::Working, 20));
        let working = statuses.decide(&id);
        let working = (working.status(), working.since());
        assert_eq!(working, (Some(&ActiveStatus::Working), Some(at(20))));
    }

    #[test]
    fn a_rung_of_another_harness_takes_the_agent_over() {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let claude = Declared::of(Harness::ClaudeCode, Rung::Osc7501);
        let claude_feed = Declared::of(Harness::ClaudeCode, Rung::Feed);
        let pi = Declared::of(Harness::Pi, Rung::Extension);
        statuses.record(report(&id, claude, ActiveStatus::Working, 1));
        statuses.record(report(&id, claude_feed, ActiveStatus::Idle, 1));

        statuses.record(report(&id, pi, ActiveStatus::Idle, 2));
        let decided = statuses.decide(&id);
        let decided = (decided.rung(), decided.status());
        assert_eq!(decided, (Some(Rung::Extension), Some(&ActiveStatus::Idle)));

        statuses.record(Report::new(pi, id.clone(), RungUpdate::Clear));
        assert_eq!(statuses.decide(&id), Decided(None), "the rungs of the harness before are gone");
    }

    #[test]
    fn a_clear_of_another_harness_leaves_the_agent_be() {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let claude = Declared::of(Harness::ClaudeCode, Rung::Osc7501);
        let pi = Declared::of(Harness::Pi, Rung::Osc7501);
        statuses.record(report(&id, claude, ActiveStatus::Working, 1));

        statuses.record(Report::new(pi, id.clone(), RungUpdate::Clear));
        let decided = statuses.decide(&id);
        let decided = (decided.rung(), decided.status());
        assert_eq!(decided, (Some(Rung::Osc7501), Some(&ActiveStatus::Working)));
    }

    #[test]
    fn forgetting_a_harness_drops_only_the_rungs_it_holds() {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let pi = Declared::of(Harness::Pi, Rung::Osc7501);
        statuses.record(report(&id, pi, ActiveStatus::Working, 1));

        statuses.forget(&id, Harness::ClaudeCode);
        assert_eq!(statuses.decide(&id).rung(), Some(Rung::Osc7501));

        statuses.forget(&id, Harness::Pi);
        assert_eq!(statuses.decide(&id), Decided(None));
    }

    #[test]
    fn an_osc_7501_record_brings_its_subagents() {
        let mut statuses = Statuses::default();
        let id = agent(42);
        let subagent = Subagent {
            id: ChildId::parse("sub-1").unwrap(),
            phase: SubagentPhase::Stopped,
            title: None,
            status_since: None,
        };
        let reads = OscReads::of(Harness::ClaudeCode);
        let since = Some(at(1));
        let told = vec![subagent];
        let osc = Report::osc_7501(reads, id.clone(), ActiveStatus::Working, since, told.clone());
        statuses.record(osc);
        assert_eq!(statuses.decide(&id).subagents(), told.as_slice());
    }
}
