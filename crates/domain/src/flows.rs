use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agent::Harness;
use crate::projects::{ViewId, WORKTREES_MAX, WorktreeId};

pub const STAGES_MAX: usize = 16;

pub const FLOWS_MAX: usize = WORKTREES_MAX;

pub const DEFAULT_FLOW_NAME: &str = "default";

pub const DEFAULT_REBASE_ATTEMPTS_MAX: u32 = 3;

pub const DEFAULT_FIX_CHECKS_ATTEMPTS_MAX: u32 = 3;

pub const RETRY_ATTEMPTS_LIMIT: u32 = 10;

pub const DEFAULT_STEP_TIMEOUT_SECS: u64 = 1800;

pub const STEP_TIMEOUT_SECS_MIN: u64 = 30;

pub const STEP_TIMEOUT_SECS_MAX: u64 = 4 * 3600;

pub const DEFAULT_CI_SETTLE_SECS: u64 = 20;

pub const CI_SETTLE_SECS_MAX: u64 = 300;

pub const MERGE_RACE_REPOLL_MAX: u32 = 1;

pub const NAME_BYTES_MAX: usize = 64;

pub const PROMPT_BYTES_MAX: usize = 64 * 1024;

pub const REASON_BYTES_MAX: usize = 512;

const _: () = assert!(STAGES_MAX >= 5);
const _: () = assert!(FLOWS_MAX > 0);
const _: () = assert!(DEFAULT_REBASE_ATTEMPTS_MAX <= RETRY_ATTEMPTS_LIMIT);
const _: () = assert!(DEFAULT_FIX_CHECKS_ATTEMPTS_MAX <= RETRY_ATTEMPTS_LIMIT);
const _: () = assert!(STEP_TIMEOUT_SECS_MIN <= DEFAULT_STEP_TIMEOUT_SECS);
const _: () = assert!(DEFAULT_STEP_TIMEOUT_SECS <= STEP_TIMEOUT_SECS_MAX);
const _: () = assert!(DEFAULT_CI_SETTLE_SECS <= CI_SETTLE_SECS_MAX);
const _: () = assert!(MERGE_RACE_REPOLL_MAX > 0);
const _: () = assert!(NAME_BYTES_MAX > 0);
const _: () = assert!(PROMPT_BYTES_MAX > 0);
const _: () = assert!(REASON_BYTES_MAX > 0);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowSpec {
    pub name: String,
    pub stages: Vec<StageSpec>,
    pub rebase_attempts_max: u32,
    pub fix_checks_attempts_max: u32,
    pub wait_for_all_checks: bool,
    pub step_timeout_secs: u64,
    pub ci_settle_secs: u64,
    pub interactive_land: InteractiveLandPolicy,
}

impl FlowSpec {
    #[must_use]
    pub fn default_spec() -> FlowSpec {
        FlowSpec {
            name: DEFAULT_FLOW_NAME.to_owned(),
            stages: vec![
                StageSpec {
                    label: "implement".to_owned(),
                    kind: StageKind::Implement,
                },
                StageSpec {
                    label: "tigerstyle".to_owned(),
                    kind: StageKind::ReviewSkill {
                        skill: "tigerstyle".to_owned(),
                    },
                },
                StageSpec {
                    label: "illegal-states".to_owned(),
                    kind: StageKind::ReviewSkill {
                        skill: "make-illegal-states-unrepresentable".to_owned(),
                    },
                },
                StageSpec {
                    label: "thermo-nuclear".to_owned(),
                    kind: StageKind::ReviewSkill {
                        skill: "thermo-nuclear-review".to_owned(),
                    },
                },
                StageSpec {
                    label: "land".to_owned(),
                    kind: StageKind::Land,
                },
            ],
            rebase_attempts_max: DEFAULT_REBASE_ATTEMPTS_MAX,
            fix_checks_attempts_max: DEFAULT_FIX_CHECKS_ATTEMPTS_MAX,
            wait_for_all_checks: true,
            step_timeout_secs: DEFAULT_STEP_TIMEOUT_SECS,
            ci_settle_secs: DEFAULT_CI_SETTLE_SECS,
            interactive_land: InteractiveLandPolicy::CheckThenConfirmMerge,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_label("flow name", &self.name)?;
        if self.stages.is_empty() {
            return Err(format!(
                "flow {}: must define at least one stage",
                self.name
            ));
        }
        if self.stages.len() > STAGES_MAX {
            return Err(format!(
                "flow {}: defines {} stages, exceeding limit of {STAGES_MAX}",
                self.name,
                self.stages.len()
            ));
        }
        let mut saw_land = false;
        for (index, stage) in self.stages.iter().enumerate() {
            validate_label("stage label", &stage.label)?;
            match &stage.kind {
                StageKind::Implement => {
                    if index != 0 {
                        return Err(format!(
                            "flow {}: implement stage must come first",
                            self.name
                        ));
                    }
                }
                StageKind::ReviewSkill { skill } => {
                    validate_label("review skill", skill)?;
                }
                StageKind::Land => {
                    if saw_land || index + 1 != self.stages.len() {
                        return Err(format!(
                            "flow {}: land stage may appear at most once and must be last",
                            self.name
                        ));
                    }
                    saw_land = true;
                }
            }
        }
        if self.rebase_attempts_max > RETRY_ATTEMPTS_LIMIT {
            return Err(format!(
                "flow {}: rebase_attempts_max {} exceeds {RETRY_ATTEMPTS_LIMIT}",
                self.name, self.rebase_attempts_max
            ));
        }
        if self.fix_checks_attempts_max > RETRY_ATTEMPTS_LIMIT {
            return Err(format!(
                "flow {}: fix_checks_attempts_max {} exceeds {RETRY_ATTEMPTS_LIMIT}",
                self.name, self.fix_checks_attempts_max
            ));
        }
        if !(STEP_TIMEOUT_SECS_MIN..=STEP_TIMEOUT_SECS_MAX).contains(&self.step_timeout_secs) {
            return Err(format!(
                "flow {}: step_timeout_secs {} must be in {STEP_TIMEOUT_SECS_MIN}..={STEP_TIMEOUT_SECS_MAX}",
                self.name, self.step_timeout_secs
            ));
        }
        if self.ci_settle_secs > CI_SETTLE_SECS_MAX {
            return Err(format!(
                "flow {}: ci_settle_secs {} exceeds {CI_SETTLE_SECS_MAX}",
                self.name, self.ci_settle_secs
            ));
        }
        Ok(())
    }

    pub fn effective_stages(&self, entry: &FlowEntry) -> Result<Vec<StageSpec>, String> {
        self.validate()?;
        let mut stages = self.stages.clone();
        match entry {
            FlowEntry::Implement { prompt } => {
                validate_prompt(prompt)?;
                if !matches!(stages.first().map(|s| &s.kind), Some(StageKind::Implement)) {
                    if stages.len() >= STAGES_MAX {
                        return Err(format!(
                            "flow {}: cannot prepend implement stage past limit of {STAGES_MAX}",
                            self.name
                        ));
                    }
                    stages.insert(
                        0,
                        StageSpec {
                            label: "implement".to_owned(),
                            kind: StageKind::Implement,
                        },
                    );
                }
            }
            FlowEntry::PullRequest => {
                while matches!(stages.first().map(|s| &s.kind), Some(StageKind::Implement)) {
                    stages.remove(0);
                }
                if stages.is_empty() {
                    return Err(format!("flow {}: has no stages after implement", self.name));
                }
            }
        }
        assert!(!stages.is_empty() && stages.len() <= STAGES_MAX);
        Ok(stages)
    }
}

fn validate_label(field: &str, value: &str) -> Result<(), String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if trimmed.len() > NAME_BYTES_MAX {
        return Err(format!("{field} exceeds {NAME_BYTES_MAX} bytes"));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

fn validate_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("prompt must not be empty".to_owned());
    }
    if prompt.len() > PROMPT_BYTES_MAX {
        return Err(format!("prompt exceeds {PROMPT_BYTES_MAX} bytes"));
    }
    if prompt.as_bytes().contains(&0) {
        return Err("prompt must not contain NUL bytes".to_owned());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpec {
    pub label: String,
    pub kind: StageKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageKind {
    Implement,
    ReviewSkill { skill: String },
    Land,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowMode {
    Interactive,
    Unattended,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractiveLandPolicy {
    StopReadyToMerge,
    CheckThenConfirmMerge,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowEntry {
    Implement { prompt: String },
    PullRequest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlowControl {
    Advance,
    AutoRest,
    Pause,
    Resume,
    RetryStep,
    SkipStep,
    TakeOver,
    Abort,
}

impl FlowControl {
    pub const ALL: [FlowControl; 8] = [
        FlowControl::Advance,
        FlowControl::AutoRest,
        FlowControl::Pause,
        FlowControl::Resume,
        FlowControl::RetryStep,
        FlowControl::SkipStep,
        FlowControl::TakeOver,
        FlowControl::Abort,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            FlowControl::Advance => "Advance",
            FlowControl::AutoRest => "Auto the rest",
            FlowControl::Pause => "Pause",
            FlowControl::Resume => "Resume",
            FlowControl::RetryStep => "Retry step",
            FlowControl::SkipStep => "Skip step",
            FlowControl::TakeOver => "Take over",
            FlowControl::Abort => "Abort",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalExit {
    Code(i32),
    Signal(i32),
    Unknown,
}

impl TerminalExit {
    #[must_use]
    pub const fn is_success(self) -> bool {
        matches!(self, TerminalExit::Code(0))
    }
}

impl fmt::Display for TerminalExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminalExit::Code(code) => write!(f, "exit code {code}"),
            TerminalExit::Signal(signal) => write!(f, "signal {signal}"),
            TerminalExit::Unknown => f.write_str("unknown exit"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeStateStatus {
    Clean,
    HasHooks,
    Blocked,
    Behind,
    Dirty,
    Unstable,
    Draft,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrSnapshot {
    pub url: String,
    pub state: PrState,
    pub head_oid: String,
    pub base_name: String,
    pub mergeable: Mergeable,
    pub merge_state: MergeStateStatus,
    pub pending_checks: u32,
    pub failed_checks: u32,
    pub passed_checks: u32,
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageGateFailure {
    DirtyTree,
    NoOpenPr,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixChecksGateFailure {
    DirtyTree,
    HeadUnchanged,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RebaseOutcome {
    Already,
    Clean { new_head: String },
    Stopped { onto: String, prompt: String },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    Merged,
    BaseUpdatedRace,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowEvent {
    ViewOpened {
        view: ViewId,
    },
    ViewOpenFailed {
        why: String,
    },
    SessionCaptured {
        view: ViewId,
        session_id: String,
    },
    TerminalExited {
        view: ViewId,
        exit: TerminalExit,
    },
    StagePostConditionsChecked(Result<(), StageGateFailure>),
    PushFinished {
        result: Result<String, String>,
        now_secs: u64,
    },
    PrSnapshot {
        snapshot: PrSnapshot,
        now_secs: u64,
    },
    RebaseFinished(RebaseOutcome),
    RebasePostConditionsChecked(Result<String, String>),
    ChecksFetched(Result<String, String>),
    FixChecksPostConditionsChecked(Result<String, FixChecksGateFailure>),
    MergeFinished {
        outcome: MergeOutcome,
        now_secs: u64,
    },
    Control(FlowControl),
    DaemonRestarted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FlowEffect {
    OpenInteractiveStage {
        worktree: WorktreeId,
        view_name: String,
        harness: Harness,
        stage_kind: StageKind,
        implement_prompt: Option<String>,
    },
    OpenUnattendedStage {
        worktree: WorktreeId,
        view_name: String,
        harness: Harness,
        stage_kind: StageKind,
        implement_prompt: Option<String>,
        timeout_secs: u64,
    },
    OpenUnattendedHelper {
        worktree: WorktreeId,
        view_name: String,
        harness: Harness,
        prompt: String,
        timeout_secs: u64,
    },
    OpenTakeOver {
        worktree: WorktreeId,
        view_name: String,
        harness: Harness,
        session_id: Option<String>,
        fallback_view: Option<ViewId>,
    },
    CheckStagePostConditions {
        worktree: WorktreeId,
        require_open_pr: bool,
    },
    PushIfAhead {
        worktree: WorktreeId,
    },
    PushForceWithLease {
        worktree: WorktreeId,
    },
    PollPr {
        worktree: WorktreeId,
    },
    RunRebase {
        worktree: WorktreeId,
    },
    CheckRebasePostConditions {
        worktree: WorktreeId,
        onto: String,
    },
    KeepChecks {
        worktree: WorktreeId,
        pr_url: String,
    },
    CheckFixChecksPostConditions {
        worktree: WorktreeId,
        previous_head: String,
    },
    MergePr {
        worktree: WorktreeId,
        pr_url: String,
    },
    Alert {
        worktree: WorktreeId,
        title: String,
        detail: String,
        urgent: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveStepKind {
    Stage,
    ResolveRebase,
    FixChecks,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveStep {
    #[serde(with = "opt_view_id")]
    pub view: Option<ViewId>,
    pub view_name: String,
    pub session_id: Option<String>,
    pub kind: ActiveStepKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LandState {
    Pushing,
    WaitingPr {
        expected_head: String,
        pushed_at_secs: u64,
        ci_waited: bool,
    },
    Rebasing {
        attempt: u32,
    },
    ResolvingRebase {
        attempt: u32,
        onto: String,
        step: ActiveStep,
    },
    CheckingRebase {
        attempt: u32,
        onto: String,
        step: ActiveStep,
    },
    PushingRebase {
        attempt: u32,
        expected_head: String,
    },
    FetchingChecks {
        attempt: u32,
        expected_head: String,
    },
    FixingChecks {
        attempt: u32,
        expected_head: String,
        step: ActiveStep,
    },
    CheckingFixChecks {
        attempt: u32,
        expected_head: String,
        step: ActiveStep,
    },
    Merging {
        expected_head: String,
        race_repolls: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PauseReason {
    UserPaused,
    DaemonRestarted,
    StepFailed,
    DirtyTree,
    NoPullRequest,
    PullRequestClosed,
    PullRequestDraft,
    PullRequestBlocked,
    RebaseFailed,
    RebasePostConditionFailed,
    RebaseAttemptsExhausted,
    ChecksFetchFailed,
    ChecksUnchangedHead,
    FixChecksAttemptsExhausted,
    PushFailed,
    MergeFailed,
    ConfirmMerge,
}

impl PauseReason {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PauseReason::UserPaused => "paused",
            PauseReason::DaemonRestarted => "paused after daemon restart",
            PauseReason::StepFailed => "step failed",
            PauseReason::DirtyTree => "dirty working tree",
            PauseReason::NoPullRequest => "no open pull request",
            PauseReason::PullRequestClosed => "pull request closed",
            PauseReason::PullRequestDraft => "pull request is draft",
            PauseReason::PullRequestBlocked => "pull request blocked",
            PauseReason::RebaseFailed => "rebase failed",
            PauseReason::RebasePostConditionFailed => "rebase incomplete",
            PauseReason::RebaseAttemptsExhausted => "rebase retry limit reached",
            PauseReason::ChecksFetchFailed => "failed to fetch checks",
            PauseReason::ChecksUnchangedHead => "checks fix made no commit",
            PauseReason::FixChecksAttemptsExhausted => "CI fix retry limit reached",
            PauseReason::PushFailed => "push failed",
            PauseReason::MergeFailed => "merge failed",
            PauseReason::ConfirmMerge => "ready to merge",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseState {
    pub reason: PauseReason,
    pub detail: String,
    pub resume_session: Option<String>,
    #[serde(with = "opt_view_id")]
    pub view: Option<ViewId>,
    pub stage_step: ActiveStepKind,
    pub retry_onto: Option<String>,
    pub merge_pr_url: Option<String>,
    pub merge_head: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowDone {
    Merged,
    ReadyToMerge,
    Aborted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FlowPhase {
    RunningStage(ActiveStep),
    CheckingStage(ActiveStep),
    WaitingInteractive {
        #[serde(with = "view_id")]
        view: ViewId,
        session_id: Option<String>,
    },
    Landing(LandState),
    Paused(PauseState),
    Done(FlowDone),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowBadgeKind {
    Running,
    WaitingInteractive,
    Paused,
    Merged,
    ReadyToMerge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlowSummary {
    pub worktree: WorktreeId,
    pub flow_name: String,
    pub mode: FlowMode,
    pub harness: Harness,
    pub step_index: u32,
    pub step_total: u32,
    pub stage_label: String,
    pub badge: String,
    pub badge_kind: FlowBadgeKind,
    pub pause_reason: Option<PauseReason>,
    pub detail: Option<String>,
    pub active_view: Option<ViewId>,
    pub resume_session: Option<String>,
    pub controls: Vec<FlowControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRun {
    #[serde(with = "worktree_id")]
    pub worktree: WorktreeId,
    pub spec: FlowSpec,
    pub stages: Vec<StageSpec>,
    pub stage_index: usize,
    pub mode: FlowMode,
    #[serde(with = "harness_serde")]
    pub harness: Harness,
    pub implement_prompt: Option<String>,
    pub phase: FlowPhase,
    pub rebase_attempts: u32,
    pub fix_checks_attempts: u32,
    pub last_pr_url: Option<String>,
}

impl FlowRun {
    pub fn start(
        worktree: WorktreeId,
        spec: FlowSpec,
        mode: FlowMode,
        harness: Harness,
        entry: FlowEntry,
    ) -> Result<(FlowRun, Vec<FlowEffect>), String> {
        let stages = spec.effective_stages(&entry)?;
        let implement_prompt = match entry {
            FlowEntry::Implement { prompt } => Some(prompt),
            FlowEntry::PullRequest => None,
        };
        let mut run = FlowRun {
            worktree,
            spec,
            stages,
            stage_index: 0,
            mode,
            harness,
            implement_prompt,
            phase: FlowPhase::Done(FlowDone::Aborted),
            rebase_attempts: 0,
            fix_checks_attempts: 0,
            last_pr_url: None,
        };
        let mut effects = Vec::new();
        run.enter_stage(0, &mut effects);
        assert!(run.stage_index < run.stages.len());
        Ok((run, effects))
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, FlowPhase::Done(_))
    }

    #[must_use]
    pub fn current_stage(&self) -> &StageSpec {
        &self.stages[self.stage_index]
    }

    #[must_use]
    pub fn active_view(&self) -> Option<ViewId> {
        match &self.phase {
            FlowPhase::WaitingInteractive { view, .. } => Some(*view),
            FlowPhase::RunningStage(step)
            | FlowPhase::CheckingStage(step)
            | FlowPhase::Landing(
                LandState::ResolvingRebase { step, .. }
                | LandState::CheckingRebase { step, .. }
                | LandState::FixingChecks { step, .. }
                | LandState::CheckingFixChecks { step, .. },
            ) => step.view,
            FlowPhase::Paused(paused) => paused.view,
            FlowPhase::Landing(_) | FlowPhase::Done(_) => None,
        }
    }

    #[must_use]
    pub fn resume_session(&self) -> Option<&str> {
        match &self.phase {
            FlowPhase::RunningStage(step) | FlowPhase::CheckingStage(step) => {
                step.session_id.as_deref()
            }
            FlowPhase::WaitingInteractive { session_id, .. } => session_id.as_deref(),
            FlowPhase::Landing(
                LandState::ResolvingRebase { step, .. }
                | LandState::CheckingRebase { step, .. }
                | LandState::FixingChecks { step, .. }
                | LandState::CheckingFixChecks { step, .. },
            ) => step.session_id.as_deref(),
            FlowPhase::Paused(paused) => paused.resume_session.as_deref(),
            FlowPhase::Landing(_) | FlowPhase::Done(_) => None,
        }
    }

    #[must_use]
    pub fn expects_pr_snapshots(&self) -> bool {
        matches!(self.phase, FlowPhase::Landing(LandState::WaitingPr { .. }))
    }

    #[must_use]
    pub fn available_controls(&self) -> Vec<FlowControl> {
        match &self.phase {
            FlowPhase::Done(_) => Vec::new(),
            FlowPhase::WaitingInteractive { .. } => vec![
                FlowControl::Advance,
                FlowControl::AutoRest,
                FlowControl::Pause,
                FlowControl::RetryStep,
                FlowControl::SkipStep,
                FlowControl::Abort,
            ],
            FlowPhase::RunningStage(_) | FlowPhase::CheckingStage(_) | FlowPhase::Landing(_) => {
                let mut controls = vec![FlowControl::Pause];
                if self.resume_session().is_some() || self.active_view().is_some() {
                    controls.push(FlowControl::TakeOver);
                }
                controls.push(FlowControl::Abort);
                controls
            }
            FlowPhase::Paused(paused) => {
                let mut controls = Vec::new();
                if paused.reason == PauseReason::ConfirmMerge {
                    controls.push(FlowControl::Advance);
                    controls.push(FlowControl::AutoRest);
                    controls.push(FlowControl::Resume);
                    controls.push(FlowControl::Abort);
                    return controls;
                }
                controls.push(FlowControl::Resume);
                if self.mode == FlowMode::Interactive {
                    controls.push(FlowControl::AutoRest);
                }
                controls.push(FlowControl::RetryStep);
                controls.push(FlowControl::SkipStep);
                controls.push(FlowControl::TakeOver);
                controls.push(FlowControl::Abort);
                controls
            }
        }
    }

    #[must_use]
    pub fn summary(&self) -> FlowSummary {
        let step_index = u32::try_from(self.stage_index + 1).expect("STAGES_MAX fits u32");
        let step_total = u32::try_from(self.stages.len()).expect("STAGES_MAX fits u32");
        let stage_label = self.current_stage().label.clone();
        let (badge, badge_kind, pause_reason, detail) = match &self.phase {
            FlowPhase::Done(FlowDone::Merged) => {
                ("merged".to_owned(), FlowBadgeKind::Merged, None, None)
            }
            FlowPhase::Done(FlowDone::ReadyToMerge) => {
                ("ready".to_owned(), FlowBadgeKind::ReadyToMerge, None, None)
            }
            FlowPhase::Done(FlowDone::Aborted) => {
                ("aborted".to_owned(), FlowBadgeKind::Paused, None, None)
            }
            FlowPhase::Paused(paused) => {
                let badge = if paused.reason == PauseReason::ConfirmMerge {
                    format!("{step_index}/{step_total} merge?")
                } else {
                    "paused".to_owned()
                };
                (
                    badge,
                    FlowBadgeKind::Paused,
                    Some(paused.reason),
                    Some(paused.detail.clone()),
                )
            }
            FlowPhase::WaitingInteractive { .. } => (
                format!("{step_index}/{step_total} {stage_label}"),
                FlowBadgeKind::WaitingInteractive,
                None,
                None,
            ),
            FlowPhase::RunningStage(_) | FlowPhase::CheckingStage(_) => (
                format!("{step_index}/{step_total} {stage_label}"),
                FlowBadgeKind::Running,
                None,
                None,
            ),
            FlowPhase::Landing(land) => {
                let tag = match land {
                    LandState::Rebasing { .. }
                    | LandState::ResolvingRebase { .. }
                    | LandState::CheckingRebase { .. }
                    | LandState::PushingRebase { .. } => "rebase",
                    LandState::FetchingChecks { .. }
                    | LandState::FixingChecks { .. }
                    | LandState::CheckingFixChecks { .. } => "fix CI",
                    LandState::Merging { .. } => "merging",
                    LandState::Pushing | LandState::WaitingPr { .. } => "CI",
                };
                (
                    format!("{step_index}/{step_total} {tag}"),
                    FlowBadgeKind::Running,
                    None,
                    None,
                )
            }
        };
        FlowSummary {
            worktree: self.worktree,
            flow_name: self.spec.name.clone(),
            mode: self.mode,
            harness: self.harness,
            step_index,
            step_total,
            stage_label,
            badge,
            badge_kind,
            pause_reason,
            detail,
            active_view: self.active_view(),
            resume_session: self.resume_session().map(str::to_owned),
            controls: self.available_controls(),
        }
    }

    pub fn handle(&mut self, event: FlowEvent) -> Vec<FlowEffect> {
        let mut effects = Vec::new();
        match event {
            FlowEvent::ViewOpened { view } => self.on_view_opened(view),
            FlowEvent::ViewOpenFailed { why } => self.on_view_open_failed(&why, &mut effects),
            FlowEvent::SessionCaptured { view, session_id } => {
                self.on_session_captured(view, session_id);
            }
            FlowEvent::TerminalExited { view, exit } => {
                self.on_terminal_exited(view, exit, &mut effects);
            }
            FlowEvent::StagePostConditionsChecked(result) => {
                self.on_stage_post_conditions(result, &mut effects);
            }
            FlowEvent::PushFinished { result, now_secs } => {
                self.on_push_finished(result, now_secs, &mut effects);
            }
            FlowEvent::PrSnapshot { snapshot, now_secs } => {
                self.on_pr_snapshot(snapshot, now_secs, &mut effects);
            }
            FlowEvent::RebaseFinished(outcome) => self.on_rebase_finished(outcome, &mut effects),
            FlowEvent::RebasePostConditionsChecked(result) => {
                self.on_rebase_post_conditions(result, &mut effects);
            }
            FlowEvent::ChecksFetched(result) => self.on_checks_fetched(result, &mut effects),
            FlowEvent::FixChecksPostConditionsChecked(result) => {
                self.on_fix_checks_post_conditions(result, &mut effects);
            }
            FlowEvent::MergeFinished { outcome, now_secs } => {
                self.on_merge_finished(outcome, now_secs, &mut effects);
            }
            FlowEvent::Control(control) => self.on_control(control, &mut effects),
            FlowEvent::DaemonRestarted => self.on_daemon_restarted(&mut effects),
        }
        assert!(self.stage_index < self.stages.len());
        effects
    }

    fn enter_stage(&mut self, index: usize, effects: &mut Vec<FlowEffect>) {
        if index >= self.stages.len() {
            self.phase = FlowPhase::Done(FlowDone::ReadyToMerge);
            effects.push(FlowEffect::Alert {
                worktree: self.worktree,
                title: "Flow complete".to_owned(),
                detail: "All stages finished; PR is ready to merge".to_owned(),
                urgent: false,
            });
            return;
        }
        self.stage_index = index;
        let stage = self.stages[index].clone();
        match stage.kind {
            StageKind::Land => self.enter_land(effects),
            StageKind::Implement | StageKind::ReviewSkill { .. } => {
                let step = ActiveStep {
                    view: None,
                    view_name: stage.label.clone(),
                    session_id: None,
                    kind: ActiveStepKind::Stage,
                };
                self.phase = FlowPhase::RunningStage(step);
                match self.mode {
                    FlowMode::Interactive => {
                        effects.push(FlowEffect::OpenInteractiveStage {
                            worktree: self.worktree,
                            view_name: stage.label,
                            harness: self.harness,
                            stage_kind: stage.kind,
                            implement_prompt: self.implement_prompt.clone(),
                        });
                    }
                    FlowMode::Unattended => {
                        effects.push(FlowEffect::OpenUnattendedStage {
                            worktree: self.worktree,
                            view_name: stage.label,
                            harness: self.harness,
                            stage_kind: stage.kind,
                            implement_prompt: self.implement_prompt.clone(),
                            timeout_secs: self.spec.step_timeout_secs,
                        });
                    }
                }
            }
        }
    }

    fn enter_land(&mut self, effects: &mut Vec<FlowEffect>) {
        if self.mode == FlowMode::Interactive
            && self.spec.interactive_land == InteractiveLandPolicy::StopReadyToMerge
        {
            self.phase = FlowPhase::Done(FlowDone::ReadyToMerge);
            effects.push(FlowEffect::Alert {
                worktree: self.worktree,
                title: "Flow ready to merge".to_owned(),
                detail: "Review stages finished in interactive mode".to_owned(),
                urgent: false,
            });
            return;
        }
        self.phase = FlowPhase::Landing(LandState::Pushing);
        effects.push(FlowEffect::PushIfAhead {
            worktree: self.worktree,
        });
    }

    fn pause(&mut self, paused: PauseState, effects: &mut Vec<FlowEffect>, urgent: bool) {
        let title = format!("Flow paused: {}", paused.reason.label());
        let detail = clamp_reason(&paused.detail);
        let paused = PauseState {
            detail: detail.clone(),
            ..paused
        };
        self.phase = FlowPhase::Paused(paused);
        effects.push(FlowEffect::Alert {
            worktree: self.worktree,
            title,
            detail,
            urgent,
        });
    }

    fn on_view_opened(&mut self, view: ViewId) {
        match &mut self.phase {
            FlowPhase::RunningStage(step) if step.view.is_none() => {
                if self.mode == FlowMode::Interactive {
                    self.phase = FlowPhase::WaitingInteractive {
                        view,
                        session_id: step.session_id.clone(),
                    };
                } else {
                    step.view = Some(view);
                }
            }
            FlowPhase::Landing(
                LandState::ResolvingRebase { step, .. } | LandState::FixingChecks { step, .. },
            ) if step.view.is_none() => {
                step.view = Some(view);
            }
            _ => {}
        }
    }

    fn on_view_open_failed(&mut self, why: &str, effects: &mut Vec<FlowEffect>) {
        let (stage_step, retry_onto) = match &self.phase {
            FlowPhase::RunningStage(_) => (ActiveStepKind::Stage, None),
            FlowPhase::Landing(LandState::ResolvingRebase { onto, .. }) => {
                (ActiveStepKind::ResolveRebase, Some(onto.clone()))
            }
            FlowPhase::Landing(LandState::FixingChecks { .. }) => (ActiveStepKind::FixChecks, None),
            _ => return,
        };
        self.pause(
            PauseState {
                reason: PauseReason::StepFailed,
                detail: format!("could not open stage view: {why}"),
                resume_session: None,
                view: None,
                stage_step,
                retry_onto,
                merge_pr_url: None,
                merge_head: None,
            },
            effects,
            true,
        );
    }

    fn on_session_captured(&mut self, view: ViewId, session_id: String) {
        if session_id.trim().is_empty() {
            return;
        }
        match &mut self.phase {
            FlowPhase::WaitingInteractive {
                view: active,
                session_id: slot,
            } if *active == view => {
                *slot = Some(session_id);
            }
            FlowPhase::RunningStage(step)
            | FlowPhase::CheckingStage(step)
            | FlowPhase::Landing(
                LandState::ResolvingRebase { step, .. }
                | LandState::CheckingRebase { step, .. }
                | LandState::FixingChecks { step, .. }
                | LandState::CheckingFixChecks { step, .. },
            ) if step.view == Some(view) => {
                step.session_id = Some(session_id);
            }
            FlowPhase::Paused(paused) if paused.view == Some(view) => {
                paused.resume_session = Some(session_id);
            }
            _ => {}
        }
    }

    fn on_terminal_exited(
        &mut self,
        view: ViewId,
        exit: TerminalExit,
        effects: &mut Vec<FlowEffect>,
    ) {
        match &self.phase {
            FlowPhase::RunningStage(step) if step.view == Some(view) => {
                let step = step.clone();
                if !exit.is_success() {
                    self.pause(
                        PauseState {
                            reason: PauseReason::StepFailed,
                            detail: format!("{} ended with {exit}", step.view_name),
                            resume_session: step.session_id,
                            view: step.view,
                            stage_step: ActiveStepKind::Stage,
                            retry_onto: None,
                            merge_pr_url: None,
                            merge_head: None,
                        },
                        effects,
                        true,
                    );
                    return;
                }
                let require_open_pr = matches!(self.current_stage().kind, StageKind::Implement);
                self.phase = FlowPhase::CheckingStage(step);
                effects.push(FlowEffect::CheckStagePostConditions {
                    worktree: self.worktree,
                    require_open_pr,
                });
            }
            FlowPhase::Landing(LandState::ResolvingRebase {
                attempt,
                onto,
                step,
            }) if step.view == Some(view) => {
                let (attempt, onto, step) = (*attempt, onto.clone(), step.clone());
                if !exit.is_success() {
                    self.pause(
                        PauseState {
                            reason: PauseReason::StepFailed,
                            detail: format!("{} ended with {exit}", step.view_name),
                            resume_session: step.session_id,
                            view: step.view,
                            stage_step: ActiveStepKind::ResolveRebase,
                            retry_onto: Some(onto),
                            merge_pr_url: None,
                            merge_head: None,
                        },
                        effects,
                        true,
                    );
                    return;
                }
                self.phase = FlowPhase::Landing(LandState::CheckingRebase {
                    attempt,
                    onto: onto.clone(),
                    step,
                });
                effects.push(FlowEffect::CheckRebasePostConditions {
                    worktree: self.worktree,
                    onto,
                });
            }
            FlowPhase::Landing(LandState::FixingChecks {
                attempt,
                expected_head,
                step,
            }) if step.view == Some(view) => {
                let (attempt, expected_head, step) =
                    (*attempt, expected_head.clone(), step.clone());
                if !exit.is_success() {
                    self.pause(
                        PauseState {
                            reason: PauseReason::StepFailed,
                            detail: format!("{} ended with {exit}", step.view_name),
                            resume_session: step.session_id,
                            view: step.view,
                            stage_step: ActiveStepKind::FixChecks,
                            retry_onto: None,
                            merge_pr_url: None,
                            merge_head: None,
                        },
                        effects,
                        true,
                    );
                    return;
                }
                self.phase = FlowPhase::Landing(LandState::CheckingFixChecks {
                    attempt,
                    expected_head: expected_head.clone(),
                    step,
                });
                effects.push(FlowEffect::CheckFixChecksPostConditions {
                    worktree: self.worktree,
                    previous_head: expected_head,
                });
            }
            _ => {}
        }
    }

    fn on_stage_post_conditions(
        &mut self,
        result: Result<(), StageGateFailure>,
        effects: &mut Vec<FlowEffect>,
    ) {
        let FlowPhase::CheckingStage(step) = &self.phase else {
            return;
        };
        let step = step.clone();
        match result {
            Ok(()) => self.enter_stage(self.stage_index + 1, effects),
            Err(StageGateFailure::DirtyTree) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::DirtyTree,
                        detail: format!(
                            "{} left uncommitted changes in the worktree",
                            step.view_name
                        ),
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            Err(StageGateFailure::NoOpenPr) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::NoPullRequest,
                        detail: format!("{} finished without an open pull request", step.view_name),
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            Err(StageGateFailure::Failed(why)) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::StepFailed,
                        detail: why,
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_push_finished(
        &mut self,
        result: Result<String, String>,
        now_secs: u64,
        effects: &mut Vec<FlowEffect>,
    ) {
        let pushing = matches!(
            self.phase,
            FlowPhase::Landing(LandState::Pushing | LandState::PushingRebase { .. })
        );
        if !pushing {
            return;
        }
        match result {
            Ok(head_oid) => {
                self.phase = FlowPhase::Landing(LandState::WaitingPr {
                    expected_head: head_oid,
                    pushed_at_secs: now_secs,
                    ci_waited: false,
                });
                effects.push(FlowEffect::PollPr {
                    worktree: self.worktree,
                });
            }
            Err(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::PushFailed,
                        detail: why,
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_pr_snapshot(
        &mut self,
        snapshot: PrSnapshot,
        now_secs: u64,
        effects: &mut Vec<FlowEffect>,
    ) {
        let FlowPhase::Landing(LandState::WaitingPr {
            expected_head,
            pushed_at_secs,
            ci_waited,
        }) = &self.phase
        else {
            return;
        };
        if snapshot.stale {
            return;
        }
        let (expected_head, pushed_at_secs, ci_waited) =
            (expected_head.clone(), *pushed_at_secs, *ci_waited);
        self.last_pr_url = Some(snapshot.url.clone());

        if snapshot.state == PrState::Merged {
            self.phase = FlowPhase::Done(FlowDone::Merged);
            effects.push(FlowEffect::Alert {
                worktree: self.worktree,
                title: "Pull request merged".to_owned(),
                detail: snapshot.url,
                urgent: false,
            });
            return;
        }

        if snapshot.state == PrState::Closed {
            self.pause(
                PauseState {
                    reason: PauseReason::PullRequestClosed,
                    detail: format!("{} is closed", snapshot.url),
                    resume_session: None,
                    view: None,
                    stage_step: ActiveStepKind::Stage,
                    retry_onto: None,
                    merge_pr_url: None,
                    merge_head: None,
                },
                effects,
                true,
            );
            return;
        }

        if snapshot.state == PrState::Draft || snapshot.merge_state == MergeStateStatus::Draft {
            self.pause(
                PauseState {
                    reason: PauseReason::PullRequestDraft,
                    detail: format!("{} is still a draft", snapshot.url),
                    resume_session: None,
                    view: None,
                    stage_step: ActiveStepKind::Stage,
                    retry_onto: None,
                    merge_pr_url: None,
                    merge_head: None,
                },
                effects,
                true,
            );
            return;
        }

        if snapshot.head_oid != expected_head {
            return;
        }

        if snapshot.mergeable == Mergeable::Unknown
            || snapshot.merge_state == MergeStateStatus::Unknown
        {
            return;
        }

        let needs_rebase = snapshot.mergeable == Mergeable::Conflicting
            || matches!(
                snapshot.merge_state,
                MergeStateStatus::Dirty | MergeStateStatus::Behind
            );
        if needs_rebase {
            if self.rebase_attempts >= self.spec.rebase_attempts_max {
                self.pause(
                    PauseState {
                        reason: PauseReason::RebaseAttemptsExhausted,
                        detail: format!(
                            "reached limit of {} rebase attempts",
                            self.spec.rebase_attempts_max
                        ),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::ResolveRebase,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
                return;
            }
            self.rebase_attempts += 1;
            self.phase = FlowPhase::Landing(LandState::Rebasing {
                attempt: self.rebase_attempts,
            });
            effects.push(FlowEffect::RunRebase {
                worktree: self.worktree,
            });
            return;
        }

        let checks_empty = snapshot.pending_checks == 0
            && snapshot.failed_checks == 0
            && snapshot.passed_checks == 0;
        let in_settle_window = !ci_waited
            && checks_empty
            && now_secs.saturating_sub(pushed_at_secs) < self.spec.ci_settle_secs;
        let wait_pending = snapshot.pending_checks > 0
            && (self.spec.wait_for_all_checks || snapshot.failed_checks == 0);
        if wait_pending || in_settle_window {
            return;
        }

        if snapshot.failed_checks > 0 {
            if self.fix_checks_attempts >= self.spec.fix_checks_attempts_max {
                self.pause(
                    PauseState {
                        reason: PauseReason::FixChecksAttemptsExhausted,
                        detail: format!(
                            "reached limit of {} CI fix attempts",
                            self.spec.fix_checks_attempts_max
                        ),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::FixChecks,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
                return;
            }
            self.fix_checks_attempts += 1;
            self.phase = FlowPhase::Landing(LandState::FetchingChecks {
                attempt: self.fix_checks_attempts,
                expected_head,
            });
            effects.push(FlowEffect::KeepChecks {
                worktree: self.worktree,
                pr_url: snapshot.url,
            });
            return;
        }

        if snapshot.merge_state == MergeStateStatus::Blocked {
            self.pause(
                PauseState {
                    reason: PauseReason::PullRequestBlocked,
                    detail: format!("{} is blocked by branch protection or review", snapshot.url),
                    resume_session: None,
                    view: None,
                    stage_step: ActiveStepKind::Stage,
                    retry_onto: None,
                    merge_pr_url: None,
                    merge_head: None,
                },
                effects,
                true,
            );
            return;
        }

        if matches!(
            snapshot.merge_state,
            MergeStateStatus::Clean | MergeStateStatus::HasHooks
        ) {
            if self.mode == FlowMode::Interactive {
                self.pause(
                    PauseState {
                        reason: PauseReason::ConfirmMerge,
                        detail: format!("{} is clean and ready to merge", snapshot.url),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: Some(snapshot.url),
                        merge_head: Some(expected_head),
                    },
                    effects,
                    false,
                );
            } else {
                self.phase = FlowPhase::Landing(LandState::Merging {
                    expected_head,
                    race_repolls: 0,
                });
                effects.push(FlowEffect::MergePr {
                    worktree: self.worktree,
                    pr_url: snapshot.url,
                });
            }
        }
    }

    fn on_rebase_finished(&mut self, outcome: RebaseOutcome, effects: &mut Vec<FlowEffect>) {
        let FlowPhase::Landing(LandState::Rebasing { attempt }) = self.phase else {
            return;
        };
        match outcome {
            RebaseOutcome::Already => {
                self.phase = FlowPhase::Landing(LandState::Pushing);
                effects.push(FlowEffect::PushIfAhead {
                    worktree: self.worktree,
                });
            }
            RebaseOutcome::Clean { new_head } => {
                self.phase = FlowPhase::Landing(LandState::PushingRebase {
                    attempt,
                    expected_head: new_head,
                });
                effects.push(FlowEffect::PushForceWithLease {
                    worktree: self.worktree,
                });
            }
            RebaseOutcome::Stopped { onto, prompt } => {
                let view_name = format!("rebase {attempt}");
                self.phase = FlowPhase::Landing(LandState::ResolvingRebase {
                    attempt,
                    onto,
                    step: ActiveStep {
                        view: None,
                        view_name: view_name.clone(),
                        session_id: None,
                        kind: ActiveStepKind::ResolveRebase,
                    },
                });
                effects.push(FlowEffect::OpenUnattendedHelper {
                    worktree: self.worktree,
                    view_name,
                    harness: self.harness,
                    prompt,
                    timeout_secs: self.spec.step_timeout_secs,
                });
            }
            RebaseOutcome::Failed(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::RebaseFailed,
                        detail: why,
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::ResolveRebase,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_rebase_post_conditions(
        &mut self,
        result: Result<String, String>,
        effects: &mut Vec<FlowEffect>,
    ) {
        let FlowPhase::Landing(LandState::CheckingRebase {
            attempt,
            onto,
            step,
        }) = &self.phase
        else {
            return;
        };
        let (attempt, onto, step) = (*attempt, onto.clone(), step.clone());
        match result {
            Ok(new_head) => {
                self.phase = FlowPhase::Landing(LandState::PushingRebase {
                    attempt,
                    expected_head: new_head,
                });
                effects.push(FlowEffect::PushForceWithLease {
                    worktree: self.worktree,
                });
            }
            Err(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::RebasePostConditionFailed,
                        detail: why,
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::ResolveRebase,
                        retry_onto: Some(onto),
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_checks_fetched(&mut self, result: Result<String, String>, effects: &mut Vec<FlowEffect>) {
        let FlowPhase::Landing(LandState::FetchingChecks {
            attempt,
            expected_head,
        }) = &self.phase
        else {
            return;
        };
        let (attempt, expected_head) = (*attempt, expected_head.clone());
        match result {
            Ok(prompt) => {
                let view_name = format!("fix-checks {attempt}");
                self.phase = FlowPhase::Landing(LandState::FixingChecks {
                    attempt,
                    expected_head,
                    step: ActiveStep {
                        view: None,
                        view_name: view_name.clone(),
                        session_id: None,
                        kind: ActiveStepKind::FixChecks,
                    },
                });
                effects.push(FlowEffect::OpenUnattendedHelper {
                    worktree: self.worktree,
                    view_name,
                    harness: self.harness,
                    prompt,
                    timeout_secs: self.spec.step_timeout_secs,
                });
            }
            Err(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::ChecksFetchFailed,
                        detail: why,
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::FixChecks,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_fix_checks_post_conditions(
        &mut self,
        result: Result<String, FixChecksGateFailure>,
        effects: &mut Vec<FlowEffect>,
    ) {
        let FlowPhase::Landing(LandState::CheckingFixChecks { step, .. }) = &self.phase else {
            return;
        };
        let step = step.clone();
        match result {
            Ok(_new_head) => {
                self.phase = FlowPhase::Landing(LandState::Pushing);
                effects.push(FlowEffect::PushIfAhead {
                    worktree: self.worktree,
                });
            }
            Err(FixChecksGateFailure::DirtyTree) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::DirtyTree,
                        detail: format!("{} left uncommitted changes", step.view_name),
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::FixChecks,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            Err(FixChecksGateFailure::HeadUnchanged) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::ChecksUnchangedHead,
                        detail: format!("{} did not create a new commit", step.view_name),
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::FixChecks,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            Err(FixChecksGateFailure::Failed(why)) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::StepFailed,
                        detail: why,
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::FixChecks,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_merge_finished(
        &mut self,
        outcome: MergeOutcome,
        now_secs: u64,
        effects: &mut Vec<FlowEffect>,
    ) {
        let FlowPhase::Landing(LandState::Merging {
            expected_head,
            race_repolls,
        }) = &self.phase
        else {
            return;
        };
        let (expected_head, race_repolls) = (expected_head.clone(), *race_repolls);
        match outcome {
            MergeOutcome::Merged => {
                self.phase = FlowPhase::Done(FlowDone::Merged);
                let detail = self
                    .last_pr_url
                    .clone()
                    .unwrap_or_else(|| "PR merged".to_owned());
                effects.push(FlowEffect::Alert {
                    worktree: self.worktree,
                    title: "Pull request merged".to_owned(),
                    detail,
                    urgent: false,
                });
            }
            MergeOutcome::BaseUpdatedRace if race_repolls < MERGE_RACE_REPOLL_MAX => {
                self.phase = FlowPhase::Landing(LandState::WaitingPr {
                    expected_head,
                    pushed_at_secs: now_secs,
                    ci_waited: true,
                });
                effects.push(FlowEffect::PollPr {
                    worktree: self.worktree,
                });
            }
            MergeOutcome::BaseUpdatedRace => {
                self.pause(
                    PauseState {
                        reason: PauseReason::MergeFailed,
                        detail: "gh pr merge raced with base branch update repeatedly".to_owned(),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            MergeOutcome::Failed(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::MergeFailed,
                        detail: why,
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_control(&mut self, control: FlowControl, effects: &mut Vec<FlowEffect>) {
        if matches!(self.phase, FlowPhase::Done(_)) {
            return;
        }
        match control {
            FlowControl::Abort => {
                self.phase = FlowPhase::Done(FlowDone::Aborted);
            }
            FlowControl::Pause => {
                if matches!(self.phase, FlowPhase::Paused(_)) {
                    return;
                }
                let (view, resume_session, stage_step, retry_onto) = match &self.phase {
                    FlowPhase::RunningStage(step) | FlowPhase::CheckingStage(step) => {
                        (step.view, step.session_id.clone(), step.kind, None)
                    }
                    FlowPhase::WaitingInteractive { view, session_id } => {
                        (Some(*view), session_id.clone(), ActiveStepKind::Stage, None)
                    }
                    FlowPhase::Landing(
                        LandState::ResolvingRebase { onto, step, .. }
                        | LandState::CheckingRebase { onto, step, .. },
                    ) => (
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::ResolveRebase,
                        Some(onto.clone()),
                    ),
                    FlowPhase::Landing(
                        LandState::FixingChecks { step, .. }
                        | LandState::CheckingFixChecks { step, .. },
                    ) => (
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::FixChecks,
                        None,
                    ),
                    FlowPhase::Landing(_) | FlowPhase::Paused(_) | FlowPhase::Done(_) => {
                        (None, None, ActiveStepKind::Stage, None)
                    }
                };
                self.phase = FlowPhase::Paused(PauseState {
                    reason: PauseReason::UserPaused,
                    detail: format!("paused at {}", self.current_stage().label),
                    resume_session,
                    view,
                    stage_step,
                    retry_onto,
                    merge_pr_url: None,
                    merge_head: None,
                });
            }
            FlowControl::Advance => match &self.phase {
                FlowPhase::WaitingInteractive { .. } => {
                    self.enter_stage(self.stage_index + 1, effects);
                }
                FlowPhase::Paused(paused) if paused.reason == PauseReason::ConfirmMerge => {
                    let pr_url = paused
                        .merge_pr_url
                        .clone()
                        .or_else(|| self.last_pr_url.clone())
                        .unwrap_or_default();
                    let expected_head = paused.merge_head.clone().unwrap_or_default();
                    self.phase = FlowPhase::Landing(LandState::Merging {
                        expected_head,
                        race_repolls: 0,
                    });
                    effects.push(FlowEffect::MergePr {
                        worktree: self.worktree,
                        pr_url,
                    });
                }
                _ => {}
            },
            FlowControl::AutoRest => {
                self.mode = FlowMode::Unattended;
                match &self.phase {
                    FlowPhase::WaitingInteractive { view, session_id } => {
                        let step = ActiveStep {
                            view: Some(*view),
                            view_name: self.current_stage().label.clone(),
                            session_id: session_id.clone(),
                            kind: ActiveStepKind::Stage,
                        };
                        let require_open_pr =
                            matches!(self.current_stage().kind, StageKind::Implement);
                        self.phase = FlowPhase::CheckingStage(step);
                        effects.push(FlowEffect::CheckStagePostConditions {
                            worktree: self.worktree,
                            require_open_pr,
                        });
                    }
                    FlowPhase::Paused(_) => self.resume_from_pause(effects),
                    _ => {}
                }
            }
            FlowControl::Resume => {
                if matches!(self.phase, FlowPhase::Paused(_)) {
                    self.resume_from_pause(effects);
                }
            }
            FlowControl::RetryStep => {
                if matches!(
                    self.phase,
                    FlowPhase::Paused(_) | FlowPhase::WaitingInteractive { .. }
                ) {
                    if matches!(self.current_stage().kind, StageKind::Land) {
                        self.rebase_attempts = 0;
                        self.fix_checks_attempts = 0;
                    }
                    self.enter_stage(self.stage_index, effects);
                }
            }
            FlowControl::SkipStep => {
                if matches!(
                    self.phase,
                    FlowPhase::Paused(_) | FlowPhase::WaitingInteractive { .. }
                ) {
                    self.enter_stage(self.stage_index + 1, effects);
                }
            }
            FlowControl::TakeOver => {
                let session_id = self.resume_session().map(str::to_owned);
                let fallback_view = self.active_view();
                let view_name = format!("{} (takeover)", self.current_stage().label);
                if !matches!(self.phase, FlowPhase::Paused(_)) {
                    let stage_step = match &self.phase {
                        FlowPhase::Landing(
                            LandState::ResolvingRebase { .. } | LandState::CheckingRebase { .. },
                        ) => ActiveStepKind::ResolveRebase,
                        FlowPhase::Landing(
                            LandState::FixingChecks { .. } | LandState::CheckingFixChecks { .. },
                        ) => ActiveStepKind::FixChecks,
                        _ => ActiveStepKind::Stage,
                    };
                    let retry_onto = match &self.phase {
                        FlowPhase::Landing(
                            LandState::ResolvingRebase { onto, .. }
                            | LandState::CheckingRebase { onto, .. },
                        ) => Some(onto.clone()),
                        _ => None,
                    };
                    self.phase = FlowPhase::Paused(PauseState {
                        reason: PauseReason::UserPaused,
                        detail: format!("taken over at {}", self.current_stage().label),
                        resume_session: session_id.clone(),
                        view: fallback_view,
                        stage_step,
                        retry_onto,
                        merge_pr_url: None,
                        merge_head: None,
                    });
                }
                effects.push(FlowEffect::OpenTakeOver {
                    worktree: self.worktree,
                    view_name,
                    harness: self.harness,
                    session_id,
                    fallback_view,
                });
            }
        }
    }

    fn resume_from_pause(&mut self, effects: &mut Vec<FlowEffect>) {
        let FlowPhase::Paused(paused) = &self.phase else {
            return;
        };
        let paused = paused.clone();
        if paused.reason == PauseReason::ConfirmMerge {
            let pr_url = paused
                .merge_pr_url
                .or_else(|| self.last_pr_url.clone())
                .unwrap_or_default();
            let expected_head = paused.merge_head.unwrap_or_default();
            self.phase = FlowPhase::Landing(LandState::Merging {
                expected_head,
                race_repolls: 0,
            });
            effects.push(FlowEffect::MergePr {
                worktree: self.worktree,
                pr_url,
            });
            return;
        }
        if matches!(
            paused.reason,
            PauseReason::DirtyTree | PauseReason::NoPullRequest
        ) && paused.stage_step == ActiveStepKind::Stage
            && !matches!(self.current_stage().kind, StageKind::Land)
        {
            let step = ActiveStep {
                view: paused.view,
                view_name: self.current_stage().label.clone(),
                session_id: paused.resume_session,
                kind: ActiveStepKind::Stage,
            };
            let require_open_pr = matches!(self.current_stage().kind, StageKind::Implement);
            self.phase = FlowPhase::CheckingStage(step);
            effects.push(FlowEffect::CheckStagePostConditions {
                worktree: self.worktree,
                require_open_pr,
            });
            return;
        }
        if matches!(
            paused.reason,
            PauseReason::RebasePostConditionFailed | PauseReason::StepFailed
        ) && paused.stage_step == ActiveStepKind::ResolveRebase
            && let Some(onto) = paused.retry_onto
        {
            let attempt = self.rebase_attempts.max(1);
            let step = ActiveStep {
                view: paused.view,
                view_name: format!("rebase {attempt}"),
                session_id: paused.resume_session,
                kind: ActiveStepKind::ResolveRebase,
            };
            self.phase = FlowPhase::Landing(LandState::CheckingRebase {
                attempt,
                onto: onto.clone(),
                step,
            });
            effects.push(FlowEffect::CheckRebasePostConditions {
                worktree: self.worktree,
                onto,
            });
            return;
        }
        if matches!(self.current_stage().kind, StageKind::Land) {
            if paused.reason == PauseReason::RebaseAttemptsExhausted {
                self.rebase_attempts = 0;
            }
            if paused.reason == PauseReason::FixChecksAttemptsExhausted {
                self.fix_checks_attempts = 0;
            }
            self.enter_land(effects);
            return;
        }
        self.enter_stage(self.stage_index, effects);
    }

    fn on_daemon_restarted(&mut self, effects: &mut Vec<FlowEffect>) {
        match &self.phase {
            FlowPhase::WaitingInteractive { .. } | FlowPhase::Paused(_) | FlowPhase::Done(_) => {}
            FlowPhase::RunningStage(step) | FlowPhase::CheckingStage(step) => {
                let step = step.clone();
                self.pause(
                    PauseState {
                        reason: PauseReason::DaemonRestarted,
                        detail: format!(
                            "luned restarted while {} was in flight; resume or retry when ready",
                            step.view_name
                        ),
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: step.kind,
                        retry_onto: None,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
            FlowPhase::Landing(land) => {
                let (view, resume_session, stage_step, retry_onto) = match land {
                    LandState::ResolvingRebase { onto, step, .. }
                    | LandState::CheckingRebase { onto, step, .. } => (
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::ResolveRebase,
                        Some(onto.clone()),
                    ),
                    LandState::FixingChecks { step, .. }
                    | LandState::CheckingFixChecks { step, .. } => (
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::FixChecks,
                        None,
                    ),
                    _ => (None, None, ActiveStepKind::Stage, None),
                };
                self.pause(
                    PauseState {
                        reason: PauseReason::DaemonRestarted,
                        detail: "luned restarted during Land; resume when ready".to_owned(),
                        resume_session,
                        view,
                        stage_step,
                        retry_onto,
                        merge_pr_url: None,
                        merge_head: None,
                    },
                    effects,
                    true,
                );
            }
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRegistry {
    #[serde(with = "flow_map_serde")]
    flows: BTreeMap<WorktreeId, FlowRun>,
}

impl FlowRegistry {
    pub fn start(
        &mut self,
        worktree: WorktreeId,
        spec: FlowSpec,
        mode: FlowMode,
        harness: Harness,
        entry: FlowEntry,
    ) -> Result<Vec<FlowEffect>, String> {
        if self.flows.get(&worktree).is_some_and(FlowRun::is_active) {
            return Err("worktree already has an active flow".to_owned());
        }
        if !self.flows.contains_key(&worktree) && self.flows.len() >= FLOWS_MAX {
            return Err(format!("active flow limit of {FLOWS_MAX} reached"));
        }
        let (run, effects) = FlowRun::start(worktree, spec, mode, harness, entry)?;
        self.flows.insert(worktree, run);
        assert!(self.flows.len() <= FLOWS_MAX);
        Ok(effects)
    }

    #[must_use]
    pub fn get(&self, worktree: WorktreeId) -> Option<&FlowRun> {
        self.flows.get(&worktree)
    }

    pub fn get_mut(&mut self, worktree: WorktreeId) -> Option<&mut FlowRun> {
        self.flows.get_mut(&worktree)
    }

    pub fn remove(&mut self, worktree: WorktreeId) -> Option<FlowRun> {
        self.flows.remove(&worktree)
    }

    pub fn handle(&mut self, worktree: WorktreeId, event: FlowEvent) -> Vec<FlowEffect> {
        let Some(run) = self.flows.get_mut(&worktree) else {
            return Vec::new();
        };
        let effects = run.handle(event);
        if matches!(run.phase, FlowPhase::Done(FlowDone::Aborted)) {
            self.flows.remove(&worktree);
        }
        effects
    }

    pub fn on_daemon_restarted(&mut self) -> Vec<FlowEffect> {
        let mut effects = Vec::new();
        for run in self.flows.values_mut() {
            effects.extend(run.handle(FlowEvent::DaemonRestarted));
        }
        effects
    }

    pub fn retain_worktrees(&mut self, keep: impl Fn(WorktreeId) -> bool) {
        self.flows.retain(|&id, _| keep(id));
    }

    pub fn iter(&self) -> impl Iterator<Item = (WorktreeId, &FlowRun)> {
        self.flows.iter().map(|(&id, run)| (id, run))
    }

    #[must_use]
    pub fn summaries(&self) -> BTreeMap<WorktreeId, FlowSummary> {
        self.flows
            .iter()
            .filter(|(_, run)| !matches!(run.phase, FlowPhase::Done(FlowDone::Aborted)))
            .map(|(&id, run)| (id, run.summary()))
            .collect()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.flows.is_empty()
    }
}

fn clamp_reason(detail: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.len() <= REASON_BYTES_MAX {
        return trimmed.to_owned();
    }
    let mut end = REASON_BYTES_MAX;
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &trimmed[..end])
}

mod worktree_id {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::projects::WorktreeId;

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires passing the field by reference"
    )]
    pub fn serialize<S: Serializer>(id: &WorktreeId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(id.get())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<WorktreeId, D::Error> {
        let raw = u64::deserialize(d)?;
        WorktreeId::of(raw).ok_or_else(|| serde::de::Error::custom("worktree id must be non-zero"))
    }
}

mod view_id {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::projects::ViewId;

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires passing the field by reference"
    )]
    pub fn serialize<S: Serializer>(id: &ViewId, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(id.get())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<ViewId, D::Error> {
        let raw = u64::deserialize(d)?;
        ViewId::of(raw).ok_or_else(|| serde::de::Error::custom("view id must be non-zero"))
    }
}

mod opt_view_id {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::projects::ViewId;

    #[expect(
        clippy::ref_option,
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires passing the field by reference"
    )]
    pub fn serialize<S: Serializer>(id: &Option<ViewId>, s: S) -> Result<S::Ok, S::Error> {
        match id {
            Some(id) => s.serialize_some(&id.get()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ViewId>, D::Error> {
        let raw = Option::<u64>::deserialize(d)?;
        match raw {
            Some(v) => ViewId::of(v)
                .map(Some)
                .ok_or_else(|| serde::de::Error::custom("view id must be non-zero")),
            None => Ok(None),
        }
    }
}

mod harness_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    use crate::agent::Harness;

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with requires passing the field by reference"
    )]
    pub fn serialize<S: Serializer>(harness: &Harness, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(harness.program())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Harness, D::Error> {
        let raw = String::deserialize(d)?;
        match raw.as_str() {
            "claude" => Ok(Harness::ClaudeCode),
            "pi" => Ok(Harness::Pi),
            "agy" => Ok(Harness::Antigravity),
            other => Err(serde::de::Error::custom(format!(
                "unknown harness {other:?}"
            ))),
        }
    }
}

mod flow_map_serde {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serialize as _, Serializer};

    use super::{FLOWS_MAX, FlowRun};
    use crate::projects::WorktreeId;

    pub fn serialize<S: Serializer>(
        flows: &BTreeMap<WorktreeId, FlowRun>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        let list: Vec<&FlowRun> = flows.values().collect();
        list.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<BTreeMap<WorktreeId, FlowRun>, D::Error> {
        let list = Vec::<FlowRun>::deserialize(d)?;
        if list.len() > FLOWS_MAX {
            return Err(serde::de::Error::custom(format!(
                "flow count {} exceeds {FLOWS_MAX}",
                list.len()
            )));
        }
        let mut map = BTreeMap::new();
        for run in list {
            if run.stages.is_empty() || run.stage_index >= run.stages.len() {
                return Err(serde::de::Error::custom("persisted flow has invalid stage"));
            }
            if map.insert(run.worktree, run).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate flow for the same worktree",
                ));
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wt(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    fn view(id: u64) -> ViewId {
        ViewId::of(id).unwrap()
    }

    fn pr(
        head_oid: &str,
        state: PrState,
        mergeable: Mergeable,
        merge_state: MergeStateStatus,
        pending: u32,
        failed: u32,
        passed: u32,
    ) -> PrSnapshot {
        PrSnapshot {
            url: "https://github.com/owner/repo/pull/42".to_owned(),
            state,
            head_oid: head_oid.to_owned(),
            base_name: "main".to_owned(),
            mergeable,
            merge_state,
            pending_checks: pending,
            failed_checks: failed,
            passed_checks: passed,
            stale: false,
        }
    }

    #[test]
    fn default_flow_from_pr_runs_four_stages_and_merges_unattended() {
        let (mut run, effects) = FlowRun::start(
            wt(1),
            FlowSpec::default_spec(),
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        assert_eq!(run.stages.len(), 4);
        assert_eq!(run.summary().badge, "1/4 tigerstyle");
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedStage { view_name, .. }] if view_name == "tigerstyle"
        ));

        for (step_idx, (vid, next_label)) in
            [(10, "illegal-states"), (11, "thermo-nuclear"), (12, "land")]
                .into_iter()
                .enumerate()
        {
            assert_eq!(
                run.handle(FlowEvent::ViewOpened { view: view(vid) }),
                Vec::new()
            );
            assert_eq!(
                run.handle(FlowEvent::SessionCaptured {
                    view: view(vid),
                    session_id: format!("s-{vid}"),
                }),
                Vec::new()
            );
            let effects = run.handle(FlowEvent::TerminalExited {
                view: view(vid),
                exit: TerminalExit::Code(0),
            });
            assert_eq!(
                effects,
                vec![FlowEffect::CheckStagePostConditions {
                    worktree: wt(1),
                    require_open_pr: false,
                }]
            );
            let effects = run.handle(FlowEvent::StagePostConditionsChecked(Ok(())));
            if step_idx < 2 {
                assert!(matches!(
                    effects.as_slice(),
                    [FlowEffect::OpenUnattendedStage { view_name, .. }] if view_name == next_label
                ));
                assert_eq!(
                    run.summary().badge,
                    format!("{}/4 {next_label}", step_idx + 2)
                );
            } else {
                assert_eq!(effects, vec![FlowEffect::PushIfAhead { worktree: wt(1) }]);
                assert_eq!(run.summary().badge, "4/4 CI");
            }
        }

        let effects = run.handle(FlowEvent::PushFinished {
            result: Ok("aaa111".to_owned()),
            now_secs: 100,
        });
        assert_eq!(effects, vec![FlowEffect::PollPr { worktree: wt(1) }]);

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "aaa111",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Clean,
                0,
                0,
                3,
            ),
            now_secs: 105,
        });
        assert_eq!(
            effects,
            vec![FlowEffect::MergePr {
                worktree: wt(1),
                pr_url: "https://github.com/owner/repo/pull/42".to_owned(),
            }]
        );
        assert_eq!(run.summary().badge, "4/4 merging");

        let effects = run.handle(FlowEvent::MergeFinished {
            outcome: MergeOutcome::Merged,
            now_secs: 106,
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: false, .. }]
        ));
        assert_eq!(run.phase, FlowPhase::Done(FlowDone::Merged));
        assert_eq!(run.summary().badge, "merged");
        assert!(!run.is_active());
    }

    #[test]
    fn step_zero_implement_requires_open_pr_and_pauses_when_missing() {
        let (mut run, effects) = FlowRun::start(
            wt(2),
            FlowSpec::default_spec(),
            FlowMode::Unattended,
            Harness::Antigravity,
            FlowEntry::Implement {
                prompt: "Tackle #872".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(run.stages.len(), 5);
        assert_eq!(run.summary().badge, "1/5 implement");
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedStage {
                stage_kind: StageKind::Implement,
                ..
            }]
        ));

        run.handle(FlowEvent::ViewOpened { view: view(20) });
        run.handle(FlowEvent::SessionCaptured {
            view: view(20),
            session_id: "agy-conv-1".to_owned(),
        });
        let effects = run.handle(FlowEvent::TerminalExited {
            view: view(20),
            exit: TerminalExit::Code(0),
        });
        assert_eq!(
            effects,
            vec![FlowEffect::CheckStagePostConditions {
                worktree: wt(2),
                require_open_pr: true,
            }]
        );
        let effects = run.handle(FlowEvent::StagePostConditionsChecked(Err(
            StageGateFailure::NoOpenPr,
        )));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        assert_eq!(run.summary().badge, "paused");
        assert_eq!(run.summary().pause_reason, Some(PauseReason::NoPullRequest));

        let effects = run.handle(FlowEvent::Control(FlowControl::TakeOver));
        assert_eq!(
            effects,
            vec![FlowEffect::OpenTakeOver {
                worktree: wt(2),
                view_name: "implement (takeover)".to_owned(),
                harness: Harness::Antigravity,
                session_id: Some("agy-conv-1".to_owned()),
                fallback_view: Some(view(20)),
            }]
        );

        let effects = run.handle(FlowEvent::Control(FlowControl::Resume));
        assert_eq!(
            effects,
            vec![FlowEffect::CheckStagePostConditions {
                worktree: wt(2),
                require_open_pr: true,
            }]
        );
        let effects = run.handle(FlowEvent::StagePostConditionsChecked(Ok(())));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedStage { view_name, .. }] if view_name == "tigerstyle"
        ));
    }

    #[test]
    fn interactive_mode_advances_and_flips_to_unattended_with_auto_rest() {
        let (mut run, effects) = FlowRun::start(
            wt(3),
            FlowSpec::default_spec(),
            FlowMode::Interactive,
            Harness::Pi,
            FlowEntry::PullRequest,
        )
        .unwrap();
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenInteractiveStage { view_name, .. }] if view_name == "tigerstyle"
        ));
        run.handle(FlowEvent::ViewOpened { view: view(30) });
        assert!(matches!(
            run.phase,
            FlowPhase::WaitingInteractive { view: v, .. } if v == view(30)
        ));

        let effects = run.handle(FlowEvent::Control(FlowControl::Advance));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenInteractiveStage { view_name, .. }] if view_name == "illegal-states"
        ));
        run.handle(FlowEvent::ViewOpened { view: view(31) });

        let effects = run.handle(FlowEvent::Control(FlowControl::AutoRest));
        assert_eq!(run.mode, FlowMode::Unattended);
        assert_eq!(
            effects,
            vec![FlowEffect::CheckStagePostConditions {
                worktree: wt(3),
                require_open_pr: false,
            }]
        );
        let effects = run.handle(FlowEvent::StagePostConditionsChecked(Ok(())));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedStage { view_name, .. }] if view_name == "thermo-nuclear"
        ));
    }

    #[test]
    fn interactive_land_supports_both_stop_ready_and_confirm_merge() {
        let mut stop_spec = FlowSpec::default_spec();
        stop_spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        stop_spec.interactive_land = InteractiveLandPolicy::StopReadyToMerge;
        let (run, _) = FlowRun::start(
            wt(4),
            stop_spec,
            FlowMode::Interactive,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        assert_eq!(run.phase, FlowPhase::Done(FlowDone::ReadyToMerge));
        assert_eq!(run.summary().badge, "ready");

        let mut confirm_spec = FlowSpec::default_spec();
        confirm_spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        confirm_spec.interactive_land = InteractiveLandPolicy::CheckThenConfirmMerge;
        let (mut run, _) = FlowRun::start(
            wt(4),
            confirm_spec,
            FlowMode::Interactive,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::PushFinished {
            result: Ok("head1".to_owned()),
            now_secs: 10,
        });
        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "head1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Clean,
                0,
                0,
                2,
            ),
            now_secs: 50,
        });
        assert!(matches!(effects.as_slice(), [FlowEffect::Alert { .. }]));
        assert_eq!(run.summary().pause_reason, Some(PauseReason::ConfirmMerge));
        assert_eq!(run.summary().badge, "1/1 merge?");

        let effects = run.handle(FlowEvent::Control(FlowControl::Advance));
        assert_eq!(
            effects,
            vec![FlowEffect::MergePr {
                worktree: wt(4),
                pr_url: "https://github.com/owner/repo/pull/42".to_owned(),
            }]
        );
    }

    #[test]
    fn land_evaluates_all_nine_priority_rules_in_strict_order() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        spec.ci_settle_secs = 20;
        let (mut run, _) = FlowRun::start(
            wt(5),
            spec,
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 100,
        });

        assert_eq!(
            run.handle(FlowEvent::PrSnapshot {
                snapshot: pr(
                    "old",
                    PrState::Open,
                    Mergeable::Conflicting,
                    MergeStateStatus::Dirty,
                    0,
                    2,
                    0,
                ),
                now_secs: 105,
            }),
            Vec::new()
        );

        assert_eq!(
            run.handle(FlowEvent::PrSnapshot {
                snapshot: pr(
                    "h1",
                    PrState::Open,
                    Mergeable::Unknown,
                    MergeStateStatus::Clean,
                    0,
                    0,
                    2,
                ),
                now_secs: 105,
            }),
            Vec::new()
        );

        assert_eq!(
            run.handle(FlowEvent::PrSnapshot {
                snapshot: pr(
                    "h1",
                    PrState::Open,
                    Mergeable::Mergeable,
                    MergeStateStatus::Clean,
                    0,
                    0,
                    0,
                ),
                now_secs: 110,
            }),
            Vec::new()
        );

        assert_eq!(
            run.handle(FlowEvent::PrSnapshot {
                snapshot: pr(
                    "h1",
                    PrState::Open,
                    Mergeable::Mergeable,
                    MergeStateStatus::Unstable,
                    1,
                    1,
                    0,
                ),
                now_secs: 125,
            }),
            Vec::new()
        );

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Behind,
                0,
                2,
                0,
            ),
            now_secs: 125,
        });
        assert_eq!(effects, vec![FlowEffect::RunRebase { worktree: wt(5) }]);
        assert_eq!(run.summary().badge, "1/1 rebase");

        let effects = run.handle(FlowEvent::RebaseFinished(RebaseOutcome::Stopped {
            onto: "refs/remotes/origin/main".to_owned(),
            prompt: "Resolve conflicts".to_owned(),
        }));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedHelper { view_name, .. }] if view_name == "rebase 1"
        ));
        run.handle(FlowEvent::ViewOpened { view: view(51) });
        let effects = run.handle(FlowEvent::TerminalExited {
            view: view(51),
            exit: TerminalExit::Code(0),
        });
        assert_eq!(
            effects,
            vec![FlowEffect::CheckRebasePostConditions {
                worktree: wt(5),
                onto: "refs/remotes/origin/main".to_owned(),
            }]
        );
        let effects = run.handle(FlowEvent::RebasePostConditionsChecked(Ok("h2".to_owned())));
        assert_eq!(
            effects,
            vec![FlowEffect::PushForceWithLease { worktree: wt(5) }]
        );
        run.handle(FlowEvent::PushFinished {
            result: Ok("h2".to_owned()),
            now_secs: 200,
        });

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h2",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Unstable,
                0,
                2,
                3,
            ),
            now_secs: 230,
        });
        assert_eq!(
            effects,
            vec![FlowEffect::KeepChecks {
                worktree: wt(5),
                pr_url: "https://github.com/owner/repo/pull/42".to_owned(),
            }]
        );
        let effects = run.handle(FlowEvent::ChecksFetched(Ok("Fix test_foo".to_owned())));
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::OpenUnattendedHelper { view_name, .. }] if view_name == "fix-checks 1"
        ));
        run.handle(FlowEvent::ViewOpened { view: view(52) });
        let effects = run.handle(FlowEvent::TerminalExited {
            view: view(52),
            exit: TerminalExit::Code(0),
        });
        assert_eq!(
            effects,
            vec![FlowEffect::CheckFixChecksPostConditions {
                worktree: wt(5),
                previous_head: "h2".to_owned(),
            }]
        );
        let effects = run.handle(FlowEvent::FixChecksPostConditionsChecked(Ok(
            "h3".to_owned()
        )));
        assert_eq!(effects, vec![FlowEffect::PushIfAhead { worktree: wt(5) }]);
        run.handle(FlowEvent::PushFinished {
            result: Ok("h3".to_owned()),
            now_secs: 300,
        });

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h3",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Blocked,
                0,
                0,
                5,
            ),
            now_secs: 330,
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        assert_eq!(
            run.summary().pause_reason,
            Some(PauseReason::PullRequestBlocked)
        );
    }

    #[test]
    fn merge_race_repolls_once_and_rebases_within_budget() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        let (mut run, _) = FlowRun::start(
            wt(6),
            spec,
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 10,
        });
        run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::HasHooks,
                0,
                0,
                1,
            ),
            now_secs: 40,
        });
        let effects = run.handle(FlowEvent::MergeFinished {
            outcome: MergeOutcome::BaseUpdatedRace,
            now_secs: 45,
        });
        assert_eq!(effects, vec![FlowEffect::PollPr { worktree: wt(6) }]);

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Behind,
                0,
                0,
                1,
            ),
            now_secs: 46,
        });
        assert_eq!(effects, vec![FlowEffect::RunRebase { worktree: wt(6) }]);
    }

    #[test]
    fn retry_caps_pause_safely_and_daemon_restart_pauses_in_flight() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        spec.rebase_attempts_max = 1;
        let (mut run, _) = FlowRun::start(
            wt(7),
            spec,
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 10,
        });
        run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Conflicting,
                MergeStateStatus::Dirty,
                0,
                0,
                0,
            ),
            now_secs: 40,
        });
        run.handle(FlowEvent::RebaseFinished(RebaseOutcome::Clean {
            new_head: "h2".to_owned(),
        }));
        run.handle(FlowEvent::PushFinished {
            result: Ok("h2".to_owned()),
            now_secs: 50,
        });
        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h2",
                PrState::Open,
                Mergeable::Conflicting,
                MergeStateStatus::Dirty,
                0,
                0,
                0,
            ),
            now_secs: 80,
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        assert_eq!(
            run.summary().pause_reason,
            Some(PauseReason::RebaseAttemptsExhausted)
        );

        let mut registry = FlowRegistry::default();
        registry
            .start(
                wt(8),
                FlowSpec::default_spec(),
                FlowMode::Unattended,
                Harness::ClaudeCode,
                FlowEntry::PullRequest,
            )
            .unwrap();
        assert!(
            registry
                .start(
                    wt(8),
                    FlowSpec::default_spec(),
                    FlowMode::Unattended,
                    Harness::ClaudeCode,
                    FlowEntry::PullRequest,
                )
                .is_err()
        );
        registry.handle(wt(8), FlowEvent::ViewOpened { view: view(80) });
        registry.handle(
            wt(8),
            FlowEvent::SessionCaptured {
                view: view(80),
                session_id: "sess-80".to_owned(),
            },
        );
        let serialized = serde_json::to_string(&registry).unwrap();
        let mut restored: FlowRegistry = serde_json::from_str(&serialized).unwrap();
        let effects = restored.on_daemon_restarted();
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        let summary = restored.get(wt(8)).unwrap().summary();
        assert_eq!(summary.pause_reason, Some(PauseReason::DaemonRestarted));
        assert_eq!(summary.resume_session.as_deref(), Some("sess-80"));
    }
}
