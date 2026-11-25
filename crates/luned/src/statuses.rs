use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::SystemTime;

use lune_domain::agent::Rung;
use lune_protocol::said::Said;
use lune_protocol::{PROGRAM_STATUS_PROGRESS_PERCENT_MAX, PROGRAM_STATUS_RECORDS_MAX};

use crate::agent::{Agent, SeenAgent};
use crate::api;
use crate::api::agent::Status as Reported;
use crate::api::agent::WaitingFor as Waits;
use crate::process;

pub const AGENTS_MAX: usize = 4096;

pub const SUBAGENTS_MAX: usize = PROGRAM_STATUS_RECORDS_MAX;

const _: () = assert!(AGENTS_MAX >= 256);
const _: () = assert!(SUBAGENTS_MAX >= 64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
    Dialog,
    Other(Option<Said>),
}

impl BlockedKind {
    #[must_use]
    pub fn of_waiting_for(word: Said) -> Self {
        match word.as_str().to_ascii_lowercase().as_str() {
            "permission prompt" => BlockedKind::Permission,
            "input needed" => BlockedKind::Question,
            "dialog open" => BlockedKind::Dialog,
            "auth" => BlockedKind::Auth,
            _ => BlockedKind::Other(Some(word)),
        }
    }

    #[must_use]
    pub fn to_api_blocked_kind(&self) -> api::BlockedKind {
        match self {
            BlockedKind::Permission => api::BlockedKind::Permission,
            BlockedKind::Question => api::BlockedKind::Question,
            BlockedKind::Auth => api::BlockedKind::Auth,
            BlockedKind::Dialog | BlockedKind::Other(_) => api::BlockedKind::Other,
        }
    }

    #[must_use]
    pub fn to_api_waiting_for(&self) -> Option<Waits> {
        let known = |waits: api::WaitingFor| Some(Waits::KnownWaitingFor(waits.into()));
        match self {
            BlockedKind::Permission => known(api::WaitingFor::PermissionPrompt),
            BlockedKind::Question => known(api::WaitingFor::InputNeeded),
            BlockedKind::Auth => known(api::WaitingFor::Auth),
            BlockedKind::Dialog => known(api::WaitingFor::DialogOpen),
            BlockedKind::Other(Some(word)) => Some(Waits::OtherWaitingFor(word.clone().into())),
            BlockedKind::Other(None) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Working,
    Idle,
    Done { message: Option<Said> },
    Blocked { kind: BlockedKind, message: Option<Said> },
    Error { message: Option<Said> },
    Shell,
    Other(Said),
    Unreported,
}

impl Status {
    #[must_use]
    pub fn to_api(
        &self,
    ) -> (Option<Reported>, Option<Waits>, Option<i32>, Option<String>) {
        let known = |status: api::Status| Some(Reported::KnownStatus(status.into()));
        match self {
            Status::Working => (known(api::Status::Busy), None, None, None),
            Status::Idle => (known(api::Status::Idle), None, None, None),
            Status::Done { message } => {
                (known(api::Status::Done), None, None, message.clone().map(String::from))
            },
            Status::Blocked { kind, message } => (
                known(api::Status::Waiting),
                kind.to_api_waiting_for(),
                Some(kind.to_api_blocked_kind().into()),
                message.clone().map(String::from),
            ),
            Status::Error { message } => {
                (known(api::Status::Error), None, None, message.clone().map(String::from))
            },
            Status::Shell => (known(api::Status::Shell), None, None, None),
            Status::Other(word) => {
                (Some(Reported::OtherStatus(word.clone().into())), None, None, None)
            },
            Status::Unreported => (None, None, None, None),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    Idle,
    Done,
    Blocked,
    Error,
    Shell,
    Other(Said),
    Unreported,
    Clear,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentReport {
    pub id: String,
    pub state: State,
    pub kind: Option<BlockedKind>,
    pub title: Option<Said>,
    pub message: Option<Said>,
    pub progress: Option<u8>,
    pub at: Option<SystemTime>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Subagent {
    pub id: String,
    pub status: Status,
    pub title: Option<Said>,
    pub progress: Option<u8>,
    pub status_since: Option<SystemTime>,
}

impl Subagent {
    #[must_use]
    pub fn to_api(&self) -> api::Subagent {
        assert_ne!(self.id, "");
        if let Some(progress) = self.progress {
            assert!(progress <= PROGRAM_STATUS_PROGRESS_PERCENT_MAX);
        }
        let (status, blocked_kind, message) = match &self.status {
            Status::Working => (api::Status::Busy, None, None),
            Status::Idle => (api::Status::Idle, None, None),
            Status::Done { message } => (api::Status::Done, None, message.clone().map(String::from)),
            Status::Blocked { kind, message } => (
                api::Status::Waiting,
                Some(kind.to_api_blocked_kind().into()),
                message.clone().map(String::from),
            ),
            Status::Error { message } => {
                (api::Status::Error, None, message.clone().map(String::from))
            },
            Status::Shell => (api::Status::Shell, None, None),
            Status::Other(_) | Status::Unreported => (api::Status::Unspecified, None, None),
        };
        api::Subagent {
            id: self.id.clone(),
            status: status.into(),
            blocked_kind,
            title: self.title.clone().map(String::from),
            message,
            progress: self.progress.map(u32::from),
            status_since: self.status_since.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    pub agent: process::Id,
    pub rung: Rung,
    pub state: State,
    pub kind: Option<BlockedKind>,
    pub message: Option<Said>,
    pub at: Option<SystemTime>,
    pub subagents: Vec<SubagentReport>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportedAgent {
    pub seen: SeenAgent,
    pub report: Report,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decided {
    pub status: Status,
    pub since: Option<SystemTime>,
    pub rung: Option<Rung>,
    pub subagents: Vec<Subagent>,
}

#[must_use]
pub fn parse_word(word: Option<&str>, waiting_for: Option<&str>) -> (State, Option<BlockedKind>) {
    let Some(word) = said(word) else {
        return (State::Unreported, None);
    };
    let (state, kind) = match word.as_str().to_ascii_lowercase().as_str() {
        "busy" | "working" => (State::Working, None),
        "idle" => (State::Idle, None),
        "done" => (State::Done, None),
        "error" => (State::Error, None),
        "waiting" | "blocked" => (State::Blocked, said(waiting_for).map(BlockedKind::of_waiting_for)),
        "shell" => (State::Shell, None),
        _ => (State::Other(word), None),
    };
    if !matches!(state, State::Blocked) {
        assert!(kind.is_none());
    }
    (state, kind)
}

fn said(word: Option<&str>) -> Option<Said> {
    Said::new(word?.trim().to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Active {
    status: Status,
    since: Option<SystemTime>,
    subagents: Vec<Subagent>,
}

#[derive(Default)]
pub struct Statuses {
    rungs: HashMap<process::Id, BTreeMap<Rung, Active>>,
}

impl Statuses {
    pub fn record(&mut self, report: Report) {
        let Report { agent, rung, state, kind, message, at, subagents } = report;
        assert!(agent.pid > 1);
        assert!(subagents.len() <= SUBAGENTS_MAX);
        if matches!(state, State::Clear | State::Unreported) {
            self.clear_rung(&agent, rung);
            return;
        }
        if !matches!(state, State::Blocked) {
            assert!(kind.is_none());
        }
        if !self.rungs.contains_key(&agent) && self.rungs.len() >= AGENTS_MAX {
            return;
        }
        let status = build_status(state, kind, message);
        let mut kept_subagents = Vec::with_capacity(subagents.len());
        for subagent in subagents {
            assert_ne!(subagent.id, "");
            if let Some(progress) = subagent.progress {
                assert!(progress <= PROGRAM_STATUS_PROGRESS_PERCENT_MAX);
            }
            if matches!(subagent.state, State::Clear | State::Unreported) {
                continue;
            }
            if !matches!(subagent.state, State::Blocked) {
                assert!(subagent.kind.is_none());
            }
            kept_subagents.push(Subagent {
                id: subagent.id,
                status: build_status(subagent.state, subagent.kind, subagent.message),
                title: subagent.title,
                progress: subagent.progress,
                status_since: subagent.at,
            });
        }
        let entry = self.rungs.entry(agent.clone()).or_default();
        let since = match rung {
            Rung::Osc7501 | Rung::Hook => match entry.get(&rung) {
                Some(previous) if same_phase(&previous.status, &status) => previous.since.or(at),
                Some(_) | None => at,
            },
            Rung::Feed | Rung::Extension | Rung::Transcript | Rung::Screen => at,
        };
        entry.insert(rung, Active { status, since, subagents: kept_subagents });
        assert!(entry.len() <= Rung::ALL.len());
        assert!(self.rungs.len() <= AGENTS_MAX);
        assert!(self.rungs.contains_key(&agent));
    }

    pub fn clear_rung(&mut self, agent: &process::Id, rung: Rung) {
        assert!(agent.pid > 1);
        if let Some(entries) = self.rungs.get_mut(agent) {
            entries.remove(&rung);
            if entries.is_empty() {
                self.rungs.remove(agent);
            }
        }
        assert!(self.rungs.get(agent).is_none_or(|entries| !entries.is_empty()));
        assert!(!self.rungs.get(agent).is_some_and(|entries| entries.contains_key(&rung)));
    }

    pub fn replace_rung(&mut self, rung: Rung, reports: Vec<Report>) {
        assert!(reports.len() <= AGENTS_MAX);
        let seen: HashSet<&process::Id> = reports.iter().map(|report| &report.agent).collect();
        assert_eq!(seen.len(), reports.len(), "each agent appears at most once in a rung snapshot");
        let stale: Vec<process::Id> = self
            .rungs
            .iter()
            .filter(|(agent, entries)| entries.contains_key(&rung) && !seen.contains(*agent))
            .map(|(agent, _)| agent.clone())
            .collect();
        for agent in stale {
            self.clear_rung(&agent, rung);
        }
        for report in reports {
            assert_eq!(report.rung, rung, "each report in a rung snapshot names that rung");
            self.record(report);
        }
        assert!(self.rungs.len() <= AGENTS_MAX);
    }

    pub fn retain_live(&mut self, live: &HashSet<process::Id>) {
        let before = self.rungs.len();
        self.rungs.retain(|agent, _| live.contains(agent));
        assert!(self.rungs.len() <= before);
        assert!(self.rungs.len() <= live.len());
    }

    #[must_use]
    pub fn decide(&self, agent: &process::Id) -> Decided {
        assert!(agent.pid > 1);
        let Some(entries) = self.rungs.get(agent) else {
            return Decided {
                status: Status::Unreported,
                since: None,
                rung: None,
                subagents: Vec::new(),
            };
        };
        assert!(!entries.is_empty(), "cleared rungs remove empty agent entries");
        assert!(entries.len() <= Rung::ALL.len());
        let (&rung, active) = entries
            .first_key_value()
            .expect("non-empty rung map has a highest-priority entry");
        assert!(!matches!(active.status, Status::Unreported));
        Decided {
            status: active.status.clone(),
            since: active.since,
            rung: Some(rung),
            subagents: active.subagents.clone(),
        }
    }

    #[must_use]
    pub fn apply(&self, seen: &SeenAgent) -> Agent {
        assert!(seen.id.pid > 1);
        let decided = self.decide(&seen.id);
        let agent = Agent {
            pid: seen.id.pid,
            harness: seen.harness,
            session_id: seen.session_id.clone(),
            cwd: seen.cwd.clone(),
            status: decided.status,
            status_since: decided.since,
            status_rung: decided.rung,
            subagents: decided.subagents,
            started_at: seen.started_at,
            name: seen.name.clone(),
            entrypoint: seen.entrypoint.clone(),
            terminal: seen.terminal,
        };
        assert_eq!(agent.pid, seen.id.pid);
        agent
    }
}

fn build_status(state: State, kind: Option<BlockedKind>, message: Option<Said>) -> Status {
    assert!(!matches!(state, State::Clear | State::Unreported));
    if !matches!(state, State::Blocked) {
        assert!(kind.is_none());
    }
    match state {
        State::Working => Status::Working,
        State::Idle => Status::Idle,
        State::Done => Status::Done { message },
        State::Blocked => {
            Status::Blocked { kind: kind.unwrap_or(BlockedKind::Other(None)), message }
        },
        State::Error => Status::Error { message },
        State::Shell => Status::Shell,
        State::Other(word) => Status::Other(word),
        State::Unreported | State::Clear => Status::Unreported,
    }
}

fn same_phase(previous: &Status, current: &Status) -> bool {
    match (previous, current) {
        (Status::Working, Status::Working)
        | (Status::Idle, Status::Idle)
        | (Status::Done { .. }, Status::Done { .. })
        | (Status::Error { .. }, Status::Error { .. })
        | (Status::Shell, Status::Shell)
        | (Status::Unreported, Status::Unreported) => true,
        (Status::Blocked { kind: a, .. }, Status::Blocked { kind: b, .. }) => a == b,
        (Status::Other(a), Status::Other(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn agent(pid: u32) -> process::Id {
        process::Id { pid, start: process::Start::spelled("100".to_owned()).unwrap() }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn report(
        id: &process::Id,
        rung: Rung,
        state: State,
        kind: Option<BlockedKind>,
        message: Option<&str>,
        time: u64,
    ) -> Report {
        Report {
            agent: id.clone(),
            rung,
            state,
            kind,
            message: message.and_then(|text| Said::new(text.to_owned())),
            at: Some(at(time)),
            subagents: Vec::new(),
        }
    }

    #[test]
    fn highest_live_rung_wins_and_lower_takes_over_on_clear() {
        let mut statuses = Statuses::default();
        let id = agent(42);

        statuses.record(report(&id, Rung::Hook, State::Idle, None, None, 10));
        statuses.record(report(&id, Rung::Feed, State::Working, None, None, 12));
        let decided = statuses.decide(&id);
        assert_eq!(decided.status, Status::Working);
        assert_eq!(decided.rung, Some(Rung::Feed));

        statuses.record(report(
            &id,
            Rung::Osc7501,
            State::Blocked,
            Some(BlockedKind::Permission),
            Some("approve touch"),
            15,
        ));
        let decided = statuses.decide(&id);
        assert_eq!(
            decided.status,
            Status::Blocked {
                kind: BlockedKind::Permission,
                message: Some(Said::new("approve touch".to_owned()).unwrap()),
            }
        );
        assert_eq!(decided.rung, Some(Rung::Osc7501));

        statuses.record(report(&id, Rung::Osc7501, State::Clear, None, None, 101));
        let decided = statuses.decide(&id);
        assert_eq!(decided.status, Status::Working);
        assert_eq!(decided.rung, Some(Rung::Feed));

        statuses.record(report(&id, Rung::Feed, State::Clear, None, None, 102));
        let decided = statuses.decide(&id);
        assert_eq!(decided.status, Status::Idle);
        assert_eq!(decided.rung, Some(Rung::Hook));
    }

    #[test]
    fn same_phase_on_osc_7501_and_hook_preserves_since_across_updates() {
        let mut statuses = Statuses::default();
        let id = agent(42);

        statuses.record(report(&id, Rung::Osc7501, State::Done, None, None, 10));
        statuses.record(report(&id, Rung::Osc7501, State::Done, None, Some("All done"), 12));
        let decided = statuses.decide(&id);
        assert_eq!(
            decided.status,
            Status::Done { message: Some(Said::new("All done".to_owned()).unwrap()) }
        );
        assert_eq!(decided.since, Some(at(10)));

        statuses.record(report(&id, Rung::Osc7501, State::Working, None, None, 20));
        assert_eq!(statuses.decide(&id).since, Some(at(20)));
    }
}
