use std::collections::BTreeMap;
use std::fmt;

use lune_protocol::v1 as api;
use serde::{Deserialize, Serialize};

use crate::agent::Harness;
use crate::projects::{ViewId, WORKTREES_MAX, WorktreeId};

pub const STAGES_MAX: usize = 16;

pub const FLOWS_MAX: usize = WORKTREES_MAX;

pub const FLOW_NAME_DEFAULT: &str = "default";

pub const REBASE_ATTEMPTS_MAX_DEFAULT: u32 = 3;

pub const FIX_CHECKS_ATTEMPTS_MAX_DEFAULT: u32 = 3;

pub const RETRY_ATTEMPTS_MAX: u32 = 10;

pub const STEP_TIMEOUT_SECS_DEFAULT: u64 = 1800;

pub const STEP_TIMEOUT_SECS_MIN: u64 = 30;

pub const STEP_TIMEOUT_SECS_MAX: u64 = 4 * 3600;

pub const CI_SETTLE_SECS_DEFAULT: u64 = 20;

pub const CI_SETTLE_SECS_MAX: u64 = 300;

pub const MERGE_RACE_REPOLL_MAX: u32 = 1;

pub const PR_INDEX_SECS_MAX: u64 = 600;

pub const CHECKS_WAIT_SECS_MAX: u64 = STEP_TIMEOUT_SECS_MAX;

pub const NAME_BYTES_MAX: usize = 64;

pub const PROMPT_BYTES_MAX: usize = 64 * 1024;

pub const REASON_BYTES_MAX: usize = 512;

pub const SESSION_ID_BYTES_MAX: usize = 512;

pub const BADGE_BYTES_MAX: usize = NAME_BYTES_MAX + 32;

const _: () = assert!(STAGES_MAX >= 5);
const _: () = assert!(FLOWS_MAX > 0);
const _: () = assert!(REBASE_ATTEMPTS_MAX_DEFAULT <= RETRY_ATTEMPTS_MAX);
const _: () = assert!(FIX_CHECKS_ATTEMPTS_MAX_DEFAULT <= RETRY_ATTEMPTS_MAX);
const _: () = assert!(STEP_TIMEOUT_SECS_MIN <= STEP_TIMEOUT_SECS_DEFAULT);
const _: () = assert!(STEP_TIMEOUT_SECS_DEFAULT <= STEP_TIMEOUT_SECS_MAX);
const _: () = assert!(CI_SETTLE_SECS_DEFAULT <= CI_SETTLE_SECS_MAX);
const _: () = assert!(MERGE_RACE_REPOLL_MAX > 0);
const _: () = assert!(CI_SETTLE_SECS_MAX < PR_INDEX_SECS_MAX);
const _: () = assert!(PR_INDEX_SECS_MAX < CHECKS_WAIT_SECS_MAX);
const _: () = assert!(NAME_BYTES_MAX > 0);
const _: () = assert!(PROMPT_BYTES_MAX > 0);
const _: () = assert!(REASON_BYTES_MAX > 0);
const _: () = assert!(SESSION_ID_BYTES_MAX > 0);
const _: () = assert!(BADGE_BYTES_MAX > NAME_BYTES_MAX);
const _: () = assert!(REASON_BYTES_MAX > ELLIPSIS.len());

const ELLIPSIS: &str = "…";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeMethod {
    #[default]
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    #[must_use]
    pub const fn gh_flag(self) -> &'static str {
        match self {
            MergeMethod::Squash => "--squash",
            MergeMethod::Merge => "--merge",
            MergeMethod::Rebase => "--rebase",
        }
    }
}

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
    #[serde(default)]
    pub merge_method: MergeMethod,
}

impl FlowSpec {
    #[must_use]
    pub fn default_spec() -> FlowSpec {
        FlowSpec {
            name: FLOW_NAME_DEFAULT.to_owned(),
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
            rebase_attempts_max: REBASE_ATTEMPTS_MAX_DEFAULT,
            fix_checks_attempts_max: FIX_CHECKS_ATTEMPTS_MAX_DEFAULT,
            wait_for_all_checks: true,
            step_timeout_secs: STEP_TIMEOUT_SECS_DEFAULT,
            ci_settle_secs: CI_SETTLE_SECS_DEFAULT,
            interactive_land: InteractiveLandPolicy::CheckThenConfirmMerge,
            merge_method: MergeMethod::Squash,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_label("flow name", &self.name)?;
        validate_stages(&self.name, &self.stages)?;
        if self.rebase_attempts_max > RETRY_ATTEMPTS_MAX {
            return Err(format!(
                "flow {}: rebase_attempts_max {} exceeds {RETRY_ATTEMPTS_MAX}",
                self.name, self.rebase_attempts_max
            ));
        }
        if self.fix_checks_attempts_max > RETRY_ATTEMPTS_MAX {
            return Err(format!(
                "flow {}: fix_checks_attempts_max {} exceeds {RETRY_ATTEMPTS_MAX}",
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
                if matches!(stages.first().map(|s| &s.kind), Some(StageKind::Implement)) {
                    stages.remove(0);
                }
                assert!(
                    !stages
                        .iter()
                        .any(|stage| matches!(stage.kind, StageKind::Implement)),
                    "implement stage only appears at index 0"
                );
                if stages.is_empty() {
                    return Err(format!("flow {}: has no stages after implement", self.name));
                }
            }
        }
        assert!(!stages.is_empty(), "effective flow has at least one stage");
        assert!(stages.len() <= STAGES_MAX);
        Ok(stages)
    }
}

fn validate_stages(flow_name: &str, stages: &[StageSpec]) -> Result<(), String> {
    if stages.is_empty() {
        return Err(format!("flow {flow_name}: must define at least one stage"));
    }
    if stages.len() > STAGES_MAX {
        return Err(format!(
            "flow {flow_name}: defines {} stages, exceeding limit of {STAGES_MAX}",
            stages.len()
        ));
    }
    let mut saw_land = false;
    for (index, stage) in stages.iter().enumerate() {
        validate_label("stage label", &stage.label)?;
        match &stage.kind {
            StageKind::Implement => {
                if index != 0 {
                    return Err(format!("flow {flow_name}: implement stage must come first"));
                }
            }
            StageKind::ReviewSkill { skill } => {
                validate_label("review skill", skill)?;
            }
            StageKind::Land => {
                if saw_land || index + 1 != stages.len() {
                    return Err(format!(
                        "flow {flow_name}: land stage may appear at most once and must be last"
                    ));
                }
                saw_land = true;
            }
        }
    }
    Ok(())
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

fn validate_session_id(session_id: &str) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("session_id must not be empty".to_owned());
    }
    if session_id.len() > SESSION_ID_BYTES_MAX {
        return Err(format!("session_id exceeds {SESSION_ID_BYTES_MAX} bytes"));
    }
    if session_id.chars().any(char::is_control) {
        return Err("session_id must not contain control characters".to_owned());
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

impl FlowMode {
    #[must_use]
    pub const fn to_api(self) -> api::FlowMode {
        match self {
            FlowMode::Interactive => api::FlowMode::Interactive,
            FlowMode::Unattended => api::FlowMode::Unattended,
        }
    }

    pub fn of_api(raw: i32) -> Result<FlowMode, String> {
        match api::FlowMode::try_from(raw) {
            Ok(api::FlowMode::Interactive) => Ok(FlowMode::Interactive),
            Ok(api::FlowMode::Unattended) => Ok(FlowMode::Unattended),
            Ok(api::FlowMode::Unspecified) | Err(_) => Err(format!(
                "flow mode {raw} is neither interactive nor unattended"
            )),
        }
    }
}

#[must_use]
pub fn start_request(
    worktree: WorktreeId,
    mode: FlowMode,
    harness: Harness,
    flow_name: Option<String>,
    prompt: Option<String>,
) -> api::StartFlowRequest {
    let entry = match prompt {
        Some(prompt) => api::start_flow_request::Entry::ImplementPrompt(prompt),
        None => api::start_flow_request::Entry::PullRequest(()),
    };
    api::StartFlowRequest {
        on: None,
        worktree: worktree.get(),
        flow_name,
        mode: mode.to_api().into(),
        harness: harness.to_api().into(),
        entry: Some(entry),
    }
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
    pub const fn to_api(self) -> api::FlowControl {
        match self {
            FlowControl::Advance => api::FlowControl::Advance,
            FlowControl::AutoRest => api::FlowControl::AutoRest,
            FlowControl::Pause => api::FlowControl::Pause,
            FlowControl::Resume => api::FlowControl::Resume,
            FlowControl::RetryStep => api::FlowControl::RetryStep,
            FlowControl::SkipStep => api::FlowControl::SkipStep,
            FlowControl::TakeOver => api::FlowControl::TakeOver,
            FlowControl::Abort => api::FlowControl::Abort,
        }
    }

    pub fn of_api(raw: i32) -> Result<FlowControl, String> {
        match api::FlowControl::try_from(raw) {
            Ok(api::FlowControl::Advance) => Ok(FlowControl::Advance),
            Ok(api::FlowControl::AutoRest) => Ok(FlowControl::AutoRest),
            Ok(api::FlowControl::Pause) => Ok(FlowControl::Pause),
            Ok(api::FlowControl::Resume) => Ok(FlowControl::Resume),
            Ok(api::FlowControl::RetryStep) => Ok(FlowControl::RetryStep),
            Ok(api::FlowControl::SkipStep) => Ok(FlowControl::SkipStep),
            Ok(api::FlowControl::TakeOver) => Ok(FlowControl::TakeOver),
            Ok(api::FlowControl::Abort) => Ok(FlowControl::Abort),
            Ok(api::FlowControl::Unspecified) | Err(_) => {
                Err(format!("flow control {raw} is not one lune knows"))
            }
        }
    }

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
    PrMissing,
    WorkerFailed {
        why: String,
    },
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
        stopped_view: Option<ViewId>,
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
        expected_head: String,
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
        merge_method: MergeMethod,
        expected_head: String,
    },
    StopStep {
        worktree: WorktreeId,
        view: ViewId,
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
        settle_skipped: bool,
    },
    Rebasing,
    ResolvingRebase {
        onto: String,
        step: ActiveStep,
    },
    CheckingRebase {
        onto: String,
        step: ActiveStep,
    },
    PushingRebase {
        expected_head: String,
    },
    FetchingChecks {
        expected_head: String,
    },
    FixingChecks {
        expected_head: String,
        step: ActiveStep,
    },
    CheckingFixChecks {
        expected_head: String,
        step: ActiveStep,
    },
    Merging {
        expected_head: String,
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
    PullRequestUnstable,
    PullRequestHeadMismatch,
    PullRequestStateUnknown,
    ChecksTimedOut,
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
            PauseReason::PullRequestUnstable => "pull request checks unstable",
            PauseReason::PullRequestHeadMismatch => "pull request head differs from local",
            PauseReason::PullRequestStateUnknown => "pull request state unknown",
            PauseReason::ChecksTimedOut => "checks still pending",
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PauseContext {
    #[default]
    None,
    CheckStage,
    WaitInteractive,
    Rebase,
    ResolveRebase {
        onto: String,
    },
    PushRebase {
        expected_head: String,
    },
    ConfirmMerge {
        pr_url: String,
        expected_head: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseState {
    pub reason: PauseReason,
    pub detail: String,
    pub resume_session: Option<String>,
    #[serde(with = "opt_view_id")]
    pub view: Option<ViewId>,
    pub stage_step: ActiveStepKind,
    #[serde(default)]
    pub context: PauseContext,
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
    pub step_number: u32,
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
    pub stage_index: u32,
    pub mode: FlowMode,
    #[serde(with = "harness_serde")]
    pub harness: Harness,
    pub implement_prompt: Option<String>,
    pub phase: FlowPhase,
    pub rebase_attempts: u32,
    pub fix_checks_attempts: u32,
    #[serde(default)]
    pub merge_race_repolls: u32,
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
            merge_race_repolls: 0,
            last_pr_url: None,
        };
        let mut effects = Vec::new();
        run.enter_stage(0, &mut effects);
        assert!(
            !matches!(run.phase, FlowPhase::Done(FlowDone::Aborted)),
            "started flow left dummy aborted phase"
        );
        run.assert_invariants();
        Ok((run, effects))
    }

    pub fn validate(&self) -> Result<(), String> {
        self.spec.validate()?;
        validate_stages(&self.spec.name, &self.stages)?;
        let offset = usize::try_from(self.stage_index)
            .map_err(|_| format!("flow stage_index {} out of range", self.stage_index))?;
        if offset >= self.stages.len() {
            return Err(format!(
                "flow stage_index {} out of bounds for {} stages",
                self.stage_index,
                self.stages.len()
            ));
        }
        if self.rebase_attempts > self.spec.rebase_attempts_max {
            return Err(format!(
                "flow rebase_attempts {} exceeds limit {}",
                self.rebase_attempts, self.spec.rebase_attempts_max
            ));
        }
        if self.fix_checks_attempts > self.spec.fix_checks_attempts_max {
            return Err(format!(
                "flow fix_checks_attempts {} exceeds limit {}",
                self.fix_checks_attempts, self.spec.fix_checks_attempts_max
            ));
        }
        if self.merge_race_repolls > MERGE_RACE_REPOLL_MAX {
            return Err(format!(
                "flow merge_race_repolls {} exceeds {MERGE_RACE_REPOLL_MAX}",
                self.merge_race_repolls
            ));
        }
        if let Some(prompt) = &self.implement_prompt {
            validate_prompt(prompt)?;
        }
        if let Some(session) = self.resume_session() {
            validate_session_id(session)?;
        }
        self.validate_phase()
    }

    fn validate_phase(&self) -> Result<(), String> {
        let landing = matches!(self.current_stage().kind, StageKind::Land);
        match &self.phase {
            FlowPhase::RunningStage(step) | FlowPhase::CheckingStage(step) => {
                if landing {
                    return Err("a stage phase is on the land stage".to_owned());
                }
                if step.kind != ActiveStepKind::Stage {
                    return Err(format!("a stage phase holds a {:?} step", step.kind));
                }
            }
            FlowPhase::WaitingInteractive { .. } => {
                if landing {
                    return Err("an interactive wait is on the land stage".to_owned());
                }
                if self.mode != FlowMode::Interactive {
                    return Err("an interactive wait is in an unattended flow".to_owned());
                }
            }
            FlowPhase::Landing(land) => {
                if !landing {
                    return Err("a land phase is on a stage that does not land".to_owned());
                }
                self.validate_land(land)?;
            }
            FlowPhase::Paused(paused) => self.validate_pause(paused, landing)?,
            FlowPhase::Done(_) => {}
        }
        Ok(())
    }

    fn validate_land(&self, land: &LandState) -> Result<(), String> {
        let head = match land {
            LandState::Pushing => None,
            LandState::Rebasing => {
                return self.validate_rebase_attempted();
            }
            LandState::ResolvingRebase { onto, step }
            | LandState::CheckingRebase { onto, step } => {
                if onto.trim().is_empty() {
                    return Err("a rebase phase has an empty onto ref".to_owned());
                }
                if step.kind != ActiveStepKind::ResolveRebase {
                    return Err(format!("a rebase phase holds a {:?} step", step.kind));
                }
                return self.validate_rebase_attempted();
            }
            LandState::PushingRebase { expected_head } => {
                self.validate_rebase_attempted()?;
                Some(expected_head)
            }
            LandState::FetchingChecks { expected_head } => {
                self.validate_fix_checks_attempted()?;
                Some(expected_head)
            }
            LandState::FixingChecks {
                expected_head,
                step,
            }
            | LandState::CheckingFixChecks {
                expected_head,
                step,
            } => {
                if step.kind != ActiveStepKind::FixChecks {
                    return Err(format!("a CI fix phase holds a {:?} step", step.kind));
                }
                self.validate_fix_checks_attempted()?;
                Some(expected_head)
            }
            LandState::WaitingPr { expected_head, .. } | LandState::Merging { expected_head } => {
                Some(expected_head)
            }
        };
        if head.is_some_and(|head| head.trim().is_empty()) {
            return Err("a land phase expects an empty head".to_owned());
        }
        Ok(())
    }

    fn validate_rebase_attempted(&self) -> Result<(), String> {
        if self.rebase_attempts == 0 {
            return Err("a rebase phase is entered with no rebase attempt counted".to_owned());
        }
        Ok(())
    }

    fn validate_fix_checks_attempted(&self) -> Result<(), String> {
        if self.fix_checks_attempts == 0 {
            return Err("a CI fix phase is entered with no CI fix attempt counted".to_owned());
        }
        Ok(())
    }

    fn validate_pause(&self, paused: &PauseState, landing: bool) -> Result<(), String> {
        match (paused.reason, &paused.context) {
            (
                PauseReason::ConfirmMerge,
                PauseContext::ConfirmMerge {
                    pr_url,
                    expected_head,
                },
            ) => {
                if pr_url.trim().is_empty() || expected_head.trim().is_empty() {
                    return Err(
                        "ConfirmMerge pause requires non-empty pr_url and expected_head".to_owned(),
                    );
                }
                if !landing {
                    return Err("ConfirmMerge pause is on a stage that does not land".to_owned());
                }
            }
            (PauseReason::ConfirmMerge, _) => {
                return Err("ConfirmMerge pause missing ConfirmMerge context".to_owned());
            }
            (_, PauseContext::ConfirmMerge { .. }) => {
                return Err("ConfirmMerge context used on non-ConfirmMerge pause".to_owned());
            }
            (_, PauseContext::CheckStage) => {
                if landing {
                    return Err("CheckStage context is on the land stage".to_owned());
                }
            }
            (_, PauseContext::WaitInteractive) => {
                if landing {
                    return Err("WaitInteractive context is on the land stage".to_owned());
                }
                if paused.view.is_none() {
                    return Err("WaitInteractive context has no view to wait in".to_owned());
                }
            }
            (_, PauseContext::ResolveRebase { onto }) if onto.trim().is_empty() => {
                return Err("ResolveRebase context requires non-empty onto ref".to_owned());
            }
            (_, PauseContext::PushRebase { expected_head }) if expected_head.trim().is_empty() => {
                return Err("PushRebase context requires non-empty expected head".to_owned());
            }
            (
                _,
                PauseContext::Rebase
                | PauseContext::ResolveRebase { .. }
                | PauseContext::PushRebase { .. },
            ) => {
                if !landing {
                    return Err("a rebase context is on a stage that does not land".to_owned());
                }
                if paused.stage_step != ActiveStepKind::ResolveRebase {
                    return Err(format!(
                        "a rebase context pauses a {:?} step",
                        paused.stage_step
                    ));
                }
                self.validate_rebase_attempted()?;
            }
            (_, PauseContext::None) => {}
        }
        Ok(())
    }

    fn assert_invariants(&self) {
        assert!(!self.stages.is_empty(), "flow has at least one stage");
        assert!(self.stages.len() <= STAGES_MAX);
        assert!(self.stage_offset() < self.stages.len());
        assert!(self.rebase_attempts <= self.spec.rebase_attempts_max);
        assert!(self.fix_checks_attempts <= self.spec.fix_checks_attempts_max);
        assert!(self.merge_race_repolls <= MERGE_RACE_REPOLL_MAX);
        let phase = self.validate_phase();
        assert!(phase.is_ok(), "flow phase invariant broken: {phase:?}");
    }

    fn stage_offset(&self) -> usize {
        usize::try_from(self.stage_index).expect("STAGES_MAX fits usize")
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        !matches!(self.phase, FlowPhase::Done(_))
    }

    #[must_use]
    pub fn current_stage(&self) -> &StageSpec {
        let offset = self.stage_offset();
        assert!(offset < self.stages.len());
        &self.stages[offset]
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
            FlowPhase::RunningStage(_)
            | FlowPhase::Landing(
                LandState::ResolvingRebase { .. } | LandState::FixingChecks { .. },
            ) => {
                let mut controls = vec![FlowControl::Pause];
                if self.resume_session().is_some() || self.active_view().is_some() {
                    controls.push(FlowControl::TakeOver);
                }
                controls.push(FlowControl::Abort);
                controls
            }
            FlowPhase::Landing(LandState::WaitingPr { .. }) => {
                vec![FlowControl::Pause, FlowControl::Abort]
            }
            FlowPhase::CheckingStage(_)
            | FlowPhase::Landing(
                LandState::Pushing
                | LandState::Rebasing
                | LandState::CheckingRebase { .. }
                | LandState::PushingRebase { .. }
                | LandState::FetchingChecks { .. }
                | LandState::CheckingFixChecks { .. }
                | LandState::Merging { .. },
            ) => vec![FlowControl::Abort],
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
        self.assert_invariants();
        let step_number = self.stage_index + 1;
        let step_total = u32::try_from(self.stages.len()).expect("STAGES_MAX fits u32");
        assert!(step_number <= step_total);
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
                    format!("{step_number}/{step_total} merge?")
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
                format!("{step_number}/{step_total} {stage_label}"),
                FlowBadgeKind::WaitingInteractive,
                None,
                None,
            ),
            FlowPhase::RunningStage(_) | FlowPhase::CheckingStage(_) => (
                format!("{step_number}/{step_total} {stage_label}"),
                FlowBadgeKind::Running,
                None,
                None,
            ),
            FlowPhase::Landing(land) => {
                let tag = match land {
                    LandState::Rebasing
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
                    format!("{step_number}/{step_total} {tag}"),
                    FlowBadgeKind::Running,
                    None,
                    None,
                )
            }
        };
        assert!(badge.len() <= BADGE_BYTES_MAX, "a badge fits its bound");
        FlowSummary {
            worktree: self.worktree,
            flow_name: self.spec.name.clone(),
            mode: self.mode,
            harness: self.harness,
            step_number,
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
        self.assert_invariants();
        let mut effects = Vec::new();
        match event {
            FlowEvent::ViewOpened { view } => self.on_view_opened(view),
            FlowEvent::ViewOpenFailed { why } => self.on_view_open_failed(&why, &mut effects),
            FlowEvent::SessionCaptured { view, session_id } => {
                self.on_session_captured(view, session_id, &mut effects);
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
            FlowEvent::PrMissing => self.on_pr_missing(&mut effects),
            FlowEvent::WorkerFailed { why } => self.on_worker_failed(&why, &mut effects),
            FlowEvent::DaemonRestarted => self.on_daemon_restarted(&mut effects),
        }
        self.assert_invariants();
        effects
    }

    pub fn control(&mut self, control: FlowControl) -> Result<Vec<FlowEffect>, String> {
        self.assert_invariants();
        if !self.available_controls().contains(&control) {
            return Err(format!(
                "{} is not available while the flow is {}",
                control.label(),
                self.summary().badge
            ));
        }
        let before = self.phase.clone();
        let mut effects = Vec::new();
        self.on_control(control, &mut effects);
        if control != FlowControl::TakeOver {
            assert_ne!(self.phase, before, "an available control moves the flow");
        }
        self.assert_invariants();
        Ok(effects)
    }

    fn running_headless_view(&self) -> Option<ViewId> {
        match &self.phase {
            FlowPhase::RunningStage(step) if self.mode == FlowMode::Unattended => step.view,
            FlowPhase::Landing(
                LandState::ResolvingRebase { step, .. } | LandState::FixingChecks { step, .. },
            ) => step.view,
            _ => None,
        }
    }

    fn stop_running_step(&self, effects: &mut Vec<FlowEffect>) {
        if let Some(view) = self.running_headless_view() {
            effects.push(FlowEffect::StopStep {
                worktree: self.worktree,
                view,
            });
        }
    }

    fn enter_stage(&mut self, index: u32, effects: &mut Vec<FlowEffect>) {
        let total = u32::try_from(self.stages.len()).expect("STAGES_MAX fits u32");
        if index >= total {
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
        let stage = self.stages[self.stage_offset()].clone();
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
        self.merge_race_repolls = 0;
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

    fn confirm_merge(
        &mut self,
        pr_url: String,
        expected_head: String,
        effects: &mut Vec<FlowEffect>,
    ) {
        assert!(!pr_url.trim().is_empty(), "ConfirmMerge carries PR URL");
        assert!(
            !expected_head.trim().is_empty(),
            "ConfirmMerge carries expected head"
        );
        self.phase = FlowPhase::Landing(LandState::Merging {
            expected_head: expected_head.clone(),
        });
        effects.push(FlowEffect::MergePr {
            worktree: self.worktree,
            pr_url,
            merge_method: self.spec.merge_method,
            expected_head,
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
        let (stage_step, context) = match &self.phase {
            FlowPhase::RunningStage(_) => (ActiveStepKind::Stage, PauseContext::None),
            FlowPhase::Landing(LandState::ResolvingRebase { onto, .. }) => (
                ActiveStepKind::ResolveRebase,
                PauseContext::ResolveRebase { onto: onto.clone() },
            ),
            FlowPhase::Landing(LandState::FixingChecks { .. }) => {
                (ActiveStepKind::FixChecks, PauseContext::None)
            }
            _ => return,
        };
        self.pause(
            PauseState {
                reason: PauseReason::StepFailed,
                detail: format!("could not open stage view: {why}"),
                resume_session: None,
                view: None,
                stage_step,
                context,
            },
            effects,
            true,
        );
    }

    fn on_worker_failed(&mut self, why: &str, effects: &mut Vec<FlowEffect>) {
        let (reason, resume_session, view, stage_step, context) = match &self.phase {
            FlowPhase::Done(_) | FlowPhase::Paused(_) | FlowPhase::WaitingInteractive { .. } => {
                return;
            }
            FlowPhase::RunningStage(step) => (
                PauseReason::StepFailed,
                step.session_id.clone(),
                step.view,
                step.kind,
                PauseContext::None,
            ),
            FlowPhase::CheckingStage(step) => (
                PauseReason::StepFailed,
                step.session_id.clone(),
                step.view,
                step.kind,
                PauseContext::CheckStage,
            ),
            FlowPhase::Landing(land) => match land {
                LandState::Pushing => (
                    PauseReason::PushFailed,
                    None,
                    None,
                    ActiveStepKind::Stage,
                    PauseContext::None,
                ),
                LandState::PushingRebase { expected_head } => (
                    PauseReason::PushFailed,
                    None,
                    None,
                    ActiveStepKind::ResolveRebase,
                    PauseContext::PushRebase {
                        expected_head: expected_head.clone(),
                    },
                ),
                LandState::WaitingPr { .. } => (
                    PauseReason::StepFailed,
                    None,
                    None,
                    ActiveStepKind::Stage,
                    PauseContext::None,
                ),
                LandState::Rebasing => (
                    PauseReason::RebaseFailed,
                    None,
                    None,
                    ActiveStepKind::ResolveRebase,
                    PauseContext::Rebase,
                ),
                LandState::ResolvingRebase { onto, step }
                | LandState::CheckingRebase { onto, step } => (
                    PauseReason::StepFailed,
                    step.session_id.clone(),
                    step.view,
                    ActiveStepKind::ResolveRebase,
                    PauseContext::ResolveRebase { onto: onto.clone() },
                ),
                LandState::FetchingChecks { .. } => (
                    PauseReason::ChecksFetchFailed,
                    None,
                    None,
                    ActiveStepKind::FixChecks,
                    PauseContext::None,
                ),
                LandState::FixingChecks { step, .. }
                | LandState::CheckingFixChecks { step, .. } => (
                    PauseReason::StepFailed,
                    step.session_id.clone(),
                    step.view,
                    ActiveStepKind::FixChecks,
                    PauseContext::None,
                ),
                LandState::Merging { .. } => (
                    PauseReason::MergeFailed,
                    None,
                    None,
                    ActiveStepKind::Stage,
                    PauseContext::None,
                ),
            },
        };
        self.pause(
            PauseState {
                reason,
                detail: why.to_owned(),
                resume_session,
                view,
                stage_step,
                context,
            },
            effects,
            true,
        );
    }

    fn on_session_captured(
        &mut self,
        view: ViewId,
        session_id: String,
        effects: &mut Vec<FlowEffect>,
    ) {
        if let Err(why) = validate_session_id(&session_id) {
            effects.push(FlowEffect::Alert {
                worktree: self.worktree,
                title: "Flow ignored a session id".to_owned(),
                detail: clamp_reason(&format!(
                    "view {view} reported an unusable session id ({why}); Take over starts a fresh session"
                )),
                urgent: false,
            });
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
                            context: PauseContext::None,
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
            FlowPhase::Landing(LandState::ResolvingRebase { onto, step })
                if step.view == Some(view) =>
            {
                let (onto, step) = (onto.clone(), step.clone());
                if !exit.is_success() {
                    self.pause(
                        PauseState {
                            reason: PauseReason::StepFailed,
                            detail: format!("{} ended with {exit}", step.view_name),
                            resume_session: step.session_id,
                            view: step.view,
                            stage_step: ActiveStepKind::ResolveRebase,
                            context: PauseContext::ResolveRebase { onto },
                        },
                        effects,
                        true,
                    );
                    return;
                }
                self.phase = FlowPhase::Landing(LandState::CheckingRebase {
                    onto: onto.clone(),
                    step,
                });
                effects.push(FlowEffect::CheckRebasePostConditions {
                    worktree: self.worktree,
                    onto,
                });
            }
            FlowPhase::Landing(LandState::FixingChecks {
                expected_head,
                step,
            }) if step.view == Some(view) => {
                let (expected_head, step) = (expected_head.clone(), step.clone());
                if !exit.is_success() {
                    self.pause(
                        PauseState {
                            reason: PauseReason::StepFailed,
                            detail: format!("{} ended with {exit}", step.view_name),
                            resume_session: step.session_id,
                            view: step.view,
                            stage_step: ActiveStepKind::FixChecks,
                            context: PauseContext::None,
                        },
                        effects,
                        true,
                    );
                    return;
                }
                self.phase = FlowPhase::Landing(LandState::CheckingFixChecks {
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
                        context: PauseContext::CheckStage,
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
                        context: PauseContext::CheckStage,
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
                        context: PauseContext::CheckStage,
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
        let rebased_head = match &self.phase {
            FlowPhase::Landing(LandState::Pushing) => None,
            FlowPhase::Landing(LandState::PushingRebase { expected_head }) => {
                Some(expected_head.clone())
            }
            _ => return,
        };
        let pushed = match (result, rebased_head.as_deref()) {
            (Ok(head_oid), Some(expected_head)) if head_oid != expected_head => Err(format!(
                "pushed {head_oid}, but the checked rebase was {expected_head}"
            )),
            (result, _) => result,
        };
        match pushed {
            Ok(head_oid) => {
                assert!(
                    !head_oid.trim().is_empty(),
                    "a push reports the head it pushed"
                );
                self.phase = FlowPhase::Landing(LandState::WaitingPr {
                    expected_head: head_oid,
                    pushed_at_secs: now_secs,
                    settle_skipped: false,
                });
                effects.push(FlowEffect::PollPr {
                    worktree: self.worktree,
                });
            }
            Err(why) => {
                let (stage_step, context) = match rebased_head {
                    Some(expected_head) => (
                        ActiveStepKind::ResolveRebase,
                        PauseContext::PushRebase { expected_head },
                    ),
                    None => (ActiveStepKind::Stage, PauseContext::None),
                };
                self.pause(
                    PauseState {
                        reason: PauseReason::PushFailed,
                        detail: why,
                        resume_session: None,
                        view: None,
                        stage_step,
                        context,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_pr_missing(&mut self, effects: &mut Vec<FlowEffect>) {
        if !self.expects_pr_snapshots() {
            return;
        }
        self.pause(
            PauseState {
                reason: PauseReason::NoPullRequest,
                detail: "the branch has no pull request to land".to_owned(),
                resume_session: None,
                view: None,
                stage_step: ActiveStepKind::Stage,
                context: PauseContext::None,
            },
            effects,
            true,
        );
    }

    fn pause_land_wait(
        &mut self,
        reason: PauseReason,
        detail: String,
        effects: &mut Vec<FlowEffect>,
    ) {
        assert!(self.expects_pr_snapshots(), "only a PR wait times out");
        self.pause(
            PauseState {
                reason,
                detail,
                resume_session: None,
                view: None,
                stage_step: ActiveStepKind::Stage,
                context: PauseContext::None,
            },
            effects,
            true,
        );
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
            settle_skipped,
        }) = &self.phase
        else {
            return;
        };
        if snapshot.stale {
            return;
        }
        let (expected_head, pushed_at_secs, settle_skipped) =
            (expected_head.clone(), *pushed_at_secs, *settle_skipped);
        let waited_secs = now_secs.saturating_sub(pushed_at_secs);
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
                    context: PauseContext::None,
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
                    context: PauseContext::None,
                },
                effects,
                true,
            );
            return;
        }

        if snapshot.head_oid != expected_head {
            if waited_secs >= PR_INDEX_SECS_MAX {
                let detail = format!(
                    "{} has head {} after {waited_secs}s, not the local {expected_head}",
                    snapshot.url, snapshot.head_oid
                );
                self.pause_land_wait(PauseReason::PullRequestHeadMismatch, detail, effects);
            }
            return;
        }

        if snapshot.mergeable == Mergeable::Unknown
            || snapshot.merge_state == MergeStateStatus::Unknown
        {
            if waited_secs >= PR_INDEX_SECS_MAX {
                let detail = format!(
                    "GitHub has not computed whether {} can merge after {waited_secs}s",
                    snapshot.url
                );
                self.pause_land_wait(PauseReason::PullRequestStateUnknown, detail, effects);
            }
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
                        context: PauseContext::None,
                    },
                    effects,
                    true,
                );
                return;
            }
            self.rebase_attempts += 1;
            assert!(self.rebase_attempts <= self.spec.rebase_attempts_max);
            self.phase = FlowPhase::Landing(LandState::Rebasing);
            effects.push(FlowEffect::RunRebase {
                worktree: self.worktree,
            });
            return;
        }

        let checks_empty = snapshot.pending_checks == 0
            && snapshot.failed_checks == 0
            && snapshot.passed_checks == 0;
        let in_settle_window =
            !settle_skipped && checks_empty && waited_secs < self.spec.ci_settle_secs;
        let wait_pending = snapshot.pending_checks > 0
            && (self.spec.wait_for_all_checks || snapshot.failed_checks == 0);
        if in_settle_window {
            return;
        }
        if wait_pending {
            if waited_secs >= CHECKS_WAIT_SECS_MAX {
                let detail = format!(
                    "{} still has {} pending checks after {waited_secs}s",
                    snapshot.url, snapshot.pending_checks
                );
                self.pause_land_wait(PauseReason::ChecksTimedOut, detail, effects);
            }
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
                        context: PauseContext::None,
                    },
                    effects,
                    true,
                );
                return;
            }
            self.fix_checks_attempts += 1;
            assert!(self.fix_checks_attempts <= self.spec.fix_checks_attempts_max);
            self.phase = FlowPhase::Landing(LandState::FetchingChecks { expected_head });
            effects.push(FlowEffect::KeepChecks {
                worktree: self.worktree,
                pr_url: snapshot.url,
            });
            return;
        }

        match snapshot.merge_state {
            MergeStateStatus::Blocked => {
                self.pause(
                    PauseState {
                        reason: PauseReason::PullRequestBlocked,
                        detail: format!(
                            "{} is blocked by branch protection or review",
                            snapshot.url
                        ),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        context: PauseContext::None,
                    },
                    effects,
                    true,
                );
            }
            MergeStateStatus::Unstable => {
                self.pause(
                    PauseState {
                        reason: PauseReason::PullRequestUnstable,
                        detail: format!(
                            "{} has unstable merge state without failing rollup checks",
                            snapshot.url
                        ),
                        resume_session: None,
                        view: None,
                        stage_step: ActiveStepKind::Stage,
                        context: PauseContext::None,
                    },
                    effects,
                    true,
                );
            }
            MergeStateStatus::Clean | MergeStateStatus::HasHooks => {
                if self.mode == FlowMode::Interactive {
                    assert_ne!(snapshot.url.trim(), "");
                    assert_ne!(expected_head.trim(), "");
                    self.pause(
                        PauseState {
                            reason: PauseReason::ConfirmMerge,
                            detail: format!("{} is clean and ready to merge", snapshot.url),
                            resume_session: None,
                            view: None,
                            stage_step: ActiveStepKind::Stage,
                            context: PauseContext::ConfirmMerge {
                                pr_url: snapshot.url,
                                expected_head,
                            },
                        },
                        effects,
                        false,
                    );
                } else {
                    self.confirm_merge(snapshot.url, expected_head, effects);
                }
            }
            MergeStateStatus::Draft
            | MergeStateStatus::Unknown
            | MergeStateStatus::Dirty
            | MergeStateStatus::Behind => {
                unreachable!("merge state already handled earlier in on_pr_snapshot");
            }
        }
    }

    fn on_rebase_finished(&mut self, outcome: RebaseOutcome, effects: &mut Vec<FlowEffect>) {
        if !matches!(self.phase, FlowPhase::Landing(LandState::Rebasing)) {
            return;
        }
        assert!(self.rebase_attempts >= 1);
        assert!(self.rebase_attempts <= self.spec.rebase_attempts_max);
        match outcome {
            RebaseOutcome::Already => {
                self.phase = FlowPhase::Landing(LandState::Pushing);
                effects.push(FlowEffect::PushIfAhead {
                    worktree: self.worktree,
                });
            }
            RebaseOutcome::Clean { new_head } => self.push_rebase(new_head, effects),
            RebaseOutcome::Stopped { onto, prompt } => {
                let view_name = format!("rebase {}", self.rebase_attempts);
                self.phase = FlowPhase::Landing(LandState::ResolvingRebase {
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
                        context: PauseContext::None,
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
        let FlowPhase::Landing(LandState::CheckingRebase { onto, step }) = &self.phase else {
            return;
        };
        let (onto, step) = (onto.clone(), step.clone());
        match result {
            Ok(new_head) => self.push_rebase(new_head, effects),
            Err(why) => {
                self.pause(
                    PauseState {
                        reason: PauseReason::RebasePostConditionFailed,
                        detail: why,
                        resume_session: step.session_id,
                        view: step.view,
                        stage_step: ActiveStepKind::ResolveRebase,
                        context: PauseContext::ResolveRebase { onto },
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn push_rebase(&mut self, new_head: String, effects: &mut Vec<FlowEffect>) {
        assert!(
            !new_head.trim().is_empty(),
            "a rebase reports the head it made"
        );
        assert!(self.rebase_attempts >= 1, "a rebase push follows a rebase");
        self.merge_race_repolls = 0;
        self.phase = FlowPhase::Landing(LandState::PushingRebase {
            expected_head: new_head.clone(),
        });
        effects.push(FlowEffect::PushForceWithLease {
            worktree: self.worktree,
            expected_head: new_head,
        });
    }

    fn on_checks_fetched(&mut self, result: Result<String, String>, effects: &mut Vec<FlowEffect>) {
        let FlowPhase::Landing(LandState::FetchingChecks { expected_head }) = &self.phase else {
            return;
        };
        assert!(self.fix_checks_attempts >= 1);
        assert!(self.fix_checks_attempts <= self.spec.fix_checks_attempts_max);
        let expected_head = expected_head.clone();
        match result {
            Ok(prompt) => {
                let view_name = format!("fix-checks {}", self.fix_checks_attempts);
                self.phase = FlowPhase::Landing(LandState::FixingChecks {
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
                        context: PauseContext::None,
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
        let FlowPhase::Landing(LandState::CheckingFixChecks {
            step,
            expected_head,
        }) = &self.phase
        else {
            return;
        };
        let (step, previous_head) = (step.clone(), expected_head.clone());
        match result {
            Ok(new_head) => {
                assert!(!new_head.trim().is_empty(), "a CI fix reports its head");
                assert_ne!(new_head, previous_head, "a checked CI fix moved the head");
                self.merge_race_repolls = 0;
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
                        context: PauseContext::None,
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
                        context: PauseContext::None,
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
                        context: PauseContext::None,
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
        let FlowPhase::Landing(LandState::Merging { expected_head }) = &self.phase else {
            return;
        };
        let expected_head = expected_head.clone();
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
            MergeOutcome::BaseUpdatedRace if self.merge_race_repolls < MERGE_RACE_REPOLL_MAX => {
                self.merge_race_repolls += 1;
                assert!(self.merge_race_repolls <= MERGE_RACE_REPOLL_MAX);
                self.phase = FlowPhase::Landing(LandState::WaitingPr {
                    expected_head,
                    pushed_at_secs: now_secs,
                    settle_skipped: true,
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
                        context: PauseContext::None,
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
                        context: PauseContext::None,
                    },
                    effects,
                    true,
                );
            }
        }
    }

    fn on_control(&mut self, control: FlowControl, effects: &mut Vec<FlowEffect>) {
        assert!(
            self.available_controls().contains(&control),
            "only an available control reaches the flow"
        );
        match control {
            FlowControl::Abort => {
                self.stop_running_step(effects);
                self.phase = FlowPhase::Done(FlowDone::Aborted);
            }
            FlowControl::Pause => self.on_pause(effects),
            FlowControl::Advance => match self.phase.clone() {
                FlowPhase::WaitingInteractive { view, session_id } => {
                    self.check_interactive_stage(view, session_id, effects);
                }
                FlowPhase::Paused(PauseState {
                    reason: PauseReason::ConfirmMerge,
                    context:
                        PauseContext::ConfirmMerge {
                            pr_url,
                            expected_head,
                        },
                    ..
                }) => self.confirm_merge(pr_url, expected_head, effects),
                _ => unreachable!("Advance is only available at an interactive wait or a merge"),
            },
            FlowControl::AutoRest => {
                self.mode = FlowMode::Unattended;
                match self.phase.clone() {
                    FlowPhase::WaitingInteractive { view, session_id } => {
                        self.check_interactive_stage(view, session_id, effects);
                    }
                    FlowPhase::Paused(_) => self.resume_from_pause(effects),
                    _ => unreachable!("Auto the rest is only available at a wait or a pause"),
                }
            }
            FlowControl::Resume => self.resume_from_pause(effects),
            FlowControl::RetryStep => {
                if matches!(self.current_stage().kind, StageKind::Land) {
                    self.rebase_attempts = 0;
                    self.fix_checks_attempts = 0;
                    self.merge_race_repolls = 0;
                }
                self.enter_stage(self.stage_index, effects);
            }
            FlowControl::SkipStep => self.enter_stage(self.stage_index + 1, effects),
            FlowControl::TakeOver => self.on_take_over(effects),
        }
    }

    fn check_interactive_stage(
        &mut self,
        view: ViewId,
        session_id: Option<String>,
        effects: &mut Vec<FlowEffect>,
    ) {
        assert!(
            !matches!(self.current_stage().kind, StageKind::Land),
            "an interactive wait is never on the land stage"
        );
        let step = ActiveStep {
            view: Some(view),
            view_name: self.current_stage().label.clone(),
            session_id,
            kind: ActiveStepKind::Stage,
        };
        self.check_stage(step, effects);
    }

    fn check_stage(&mut self, step: ActiveStep, effects: &mut Vec<FlowEffect>) {
        assert_eq!(step.kind, ActiveStepKind::Stage);
        let require_open_pr = matches!(self.current_stage().kind, StageKind::Implement);
        self.phase = FlowPhase::CheckingStage(step);
        effects.push(FlowEffect::CheckStagePostConditions {
            worktree: self.worktree,
            require_open_pr,
        });
    }

    fn on_pause(&mut self, effects: &mut Vec<FlowEffect>) {
        let (view, resume_session, stage_step, context) = match &self.phase {
            FlowPhase::RunningStage(step) => (
                step.view,
                step.session_id.clone(),
                step.kind,
                PauseContext::None,
            ),
            FlowPhase::WaitingInteractive { view, session_id } => (
                Some(*view),
                session_id.clone(),
                ActiveStepKind::Stage,
                PauseContext::WaitInteractive,
            ),
            FlowPhase::Landing(LandState::ResolvingRebase { onto, step }) => (
                step.view,
                step.session_id.clone(),
                ActiveStepKind::ResolveRebase,
                PauseContext::ResolveRebase { onto: onto.clone() },
            ),
            FlowPhase::Landing(LandState::FixingChecks { step, .. }) => (
                step.view,
                step.session_id.clone(),
                ActiveStepKind::FixChecks,
                PauseContext::None,
            ),
            FlowPhase::Landing(LandState::WaitingPr { .. }) => {
                (None, None, ActiveStepKind::Stage, PauseContext::None)
            }
            _ => unreachable!("Pause is only available while a step runs or a PR is awaited"),
        };
        self.stop_running_step(effects);
        self.phase = FlowPhase::Paused(PauseState {
            reason: PauseReason::UserPaused,
            detail: format!("paused at {}", self.current_stage().label),
            resume_session,
            view,
            stage_step,
            context,
        });
    }

    fn on_take_over(&mut self, effects: &mut Vec<FlowEffect>) {
        let session_id = self.resume_session().map(str::to_owned);
        let fallback_view = self.active_view();
        let view_name = format!("{} (takeover)", self.current_stage().label);
        let stopped_view = self.running_headless_view();
        if !matches!(self.phase, FlowPhase::Paused(_)) {
            let (stage_step, context) = match &self.phase {
                FlowPhase::RunningStage(_) => (ActiveStepKind::Stage, PauseContext::None),
                FlowPhase::Landing(LandState::ResolvingRebase { onto, .. }) => (
                    ActiveStepKind::ResolveRebase,
                    PauseContext::ResolveRebase { onto: onto.clone() },
                ),
                FlowPhase::Landing(LandState::FixingChecks { .. }) => {
                    (ActiveStepKind::FixChecks, PauseContext::None)
                }
                _ => unreachable!("Take over is only available while a step runs or at a pause"),
            };
            self.stop_running_step(effects);
            self.phase = FlowPhase::Paused(PauseState {
                reason: PauseReason::UserPaused,
                detail: format!("taken over at {}", self.current_stage().label),
                resume_session: session_id.clone(),
                view: fallback_view,
                stage_step,
                context,
            });
        }
        effects.push(FlowEffect::OpenTakeOver {
            worktree: self.worktree,
            view_name,
            harness: self.harness,
            session_id,
            fallback_view,
            stopped_view,
        });
    }

    fn resume_from_pause(&mut self, effects: &mut Vec<FlowEffect>) {
        let FlowPhase::Paused(paused) = self.phase.clone() else {
            unreachable!("only a paused flow resumes");
        };
        match paused.context {
            PauseContext::ConfirmMerge {
                pr_url,
                expected_head,
            } => {
                assert_eq!(paused.reason, PauseReason::ConfirmMerge);
                self.confirm_merge(pr_url, expected_head, effects);
            }
            PauseContext::WaitInteractive => {
                let view = paused
                    .view
                    .expect("an interactive wait pauses with its view");
                if self.mode == FlowMode::Interactive {
                    self.phase = FlowPhase::WaitingInteractive {
                        view,
                        session_id: paused.resume_session,
                    };
                } else {
                    self.check_interactive_stage(view, paused.resume_session, effects);
                }
            }
            PauseContext::CheckStage => {
                let step = ActiveStep {
                    view: paused.view,
                    view_name: self.current_stage().label.clone(),
                    session_id: paused.resume_session,
                    kind: ActiveStepKind::Stage,
                };
                self.check_stage(step, effects);
            }
            PauseContext::ResolveRebase { onto } => {
                assert!(
                    self.rebase_attempts >= 1,
                    "a rebase pause counted its attempt"
                );
                let step = ActiveStep {
                    view: paused.view,
                    view_name: format!("rebase {}", self.rebase_attempts),
                    session_id: paused.resume_session,
                    kind: ActiveStepKind::ResolveRebase,
                };
                self.phase = FlowPhase::Landing(LandState::CheckingRebase {
                    onto: onto.clone(),
                    step,
                });
                effects.push(FlowEffect::CheckRebasePostConditions {
                    worktree: self.worktree,
                    onto,
                });
            }
            PauseContext::Rebase => {
                assert!(
                    self.rebase_attempts >= 1,
                    "a rebase pause counted its attempt"
                );
                self.phase = FlowPhase::Landing(LandState::Rebasing);
                effects.push(FlowEffect::RunRebase {
                    worktree: self.worktree,
                });
            }
            PauseContext::PushRebase { expected_head } => self.push_rebase(expected_head, effects),
            PauseContext::None => {
                if matches!(self.current_stage().kind, StageKind::Land) {
                    if paused.reason == PauseReason::RebaseAttemptsExhausted {
                        self.rebase_attempts = 0;
                    }
                    if paused.reason == PauseReason::FixChecksAttemptsExhausted {
                        self.fix_checks_attempts = 0;
                    }
                    if paused.reason == PauseReason::MergeFailed {
                        self.merge_race_repolls = 0;
                    }
                    self.enter_land(effects);
                    return;
                }
                self.enter_stage(self.stage_index, effects);
            }
        }
    }

    fn on_daemon_restarted(&mut self, effects: &mut Vec<FlowEffect>) {
        let (detail, view, resume_session, stage_step, context) = match &self.phase {
            FlowPhase::WaitingInteractive { .. } | FlowPhase::Paused(_) | FlowPhase::Done(_) => {
                return;
            }
            FlowPhase::RunningStage(step) => (
                format!("luned restarted while {} was in flight", step.view_name),
                step.view,
                step.session_id.clone(),
                step.kind,
                PauseContext::None,
            ),
            FlowPhase::CheckingStage(step) => (
                format!("luned restarted while checking {}", step.view_name),
                step.view,
                step.session_id.clone(),
                step.kind,
                PauseContext::CheckStage,
            ),
            FlowPhase::Landing(land) => {
                let detail = "luned restarted during Land".to_owned();
                match land {
                    LandState::ResolvingRebase { onto, step }
                    | LandState::CheckingRebase { onto, step } => (
                        detail,
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::ResolveRebase,
                        PauseContext::ResolveRebase { onto: onto.clone() },
                    ),
                    LandState::Rebasing => (
                        detail,
                        None,
                        None,
                        ActiveStepKind::ResolveRebase,
                        PauseContext::Rebase,
                    ),
                    LandState::PushingRebase { expected_head } => (
                        detail,
                        None,
                        None,
                        ActiveStepKind::ResolveRebase,
                        PauseContext::PushRebase {
                            expected_head: expected_head.clone(),
                        },
                    ),
                    LandState::FixingChecks { step, .. }
                    | LandState::CheckingFixChecks { step, .. } => (
                        detail,
                        step.view,
                        step.session_id.clone(),
                        ActiveStepKind::FixChecks,
                        PauseContext::None,
                    ),
                    LandState::Pushing
                    | LandState::WaitingPr { .. }
                    | LandState::FetchingChecks { .. }
                    | LandState::Merging { .. } => (
                        detail,
                        None,
                        None,
                        ActiveStepKind::Stage,
                        PauseContext::None,
                    ),
                }
            }
        };
        self.pause(
            PauseState {
                reason: PauseReason::DaemonRestarted,
                detail: format!("{detail}; resume or retry when ready"),
                resume_session,
                view,
                stage_step,
                context,
            },
            effects,
            true,
        );
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRegistry {
    #[serde(with = "flow_map_serde")]
    flows: BTreeMap<WorktreeId, FlowRun>,
}

impl FlowRegistry {
    pub fn validate(&self) -> Result<(), String> {
        if self.flows.len() > FLOWS_MAX {
            return Err(format!(
                "flow count {} exceeds {FLOWS_MAX}",
                self.flows.len()
            ));
        }
        for (&worktree, run) in &self.flows {
            if run.worktree != worktree {
                return Err(format!(
                    "flow map key {worktree} disagrees with run worktree {}",
                    run.worktree
                ));
            }
            run.validate()?;
        }
        Ok(())
    }

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

    pub fn remove(&mut self, worktree: WorktreeId) -> Option<FlowRun> {
        self.flows.remove(&worktree)
    }

    pub fn handle(&mut self, worktree: WorktreeId, event: FlowEvent) -> Vec<FlowEffect> {
        let Some(run) = self.flows.get_mut(&worktree) else {
            return Vec::new();
        };
        let effects = run.handle(event);
        assert!(
            !matches!(run.phase, FlowPhase::Done(FlowDone::Aborted)),
            "only a control aborts a flow"
        );
        assert!(self.flows.len() <= FLOWS_MAX);
        effects
    }

    pub fn control(
        &mut self,
        worktree: WorktreeId,
        control: FlowControl,
    ) -> Result<Vec<FlowEffect>, String> {
        let Some(run) = self.flows.get_mut(&worktree) else {
            return Err("worktree has no active flow".to_owned());
        };
        let effects = run.control(control)?;
        if matches!(run.phase, FlowPhase::Done(FlowDone::Aborted)) {
            self.flows.remove(&worktree);
        }
        assert!(self.flows.len() <= FLOWS_MAX);
        Ok(effects)
    }

    pub fn on_daemon_restarted(&mut self) -> Vec<FlowEffect> {
        assert!(self.flows.len() <= FLOWS_MAX);
        let mut effects = Vec::new();
        for run in self.flows.values_mut() {
            effects.extend(run.handle(FlowEvent::DaemonRestarted));
        }
        effects
    }

    pub fn retain_worktrees(&mut self, keep: impl Fn(WorktreeId) -> bool) {
        self.flows.retain(|&id, _| keep(id));
        assert!(self.flows.len() <= FLOWS_MAX);
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
    pub fn len(&self) -> usize {
        self.flows.len()
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
    let end = trimmed.floor_char_boundary(REASON_BYTES_MAX - ELLIPSIS.len());
    assert!(trimmed.is_char_boundary(end));
    let clamped = format!("{}{ELLIPSIS}", &trimmed[..end]);
    assert!(clamped.len() <= REASON_BYTES_MAX);
    clamped
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
            run.validate().map_err(serde::de::Error::custom)?;
            if map.insert(run.worktree, run).is_some() {
                return Err(serde::de::Error::custom(
                    "duplicate flow for the same worktree",
                ));
            }
        }
        assert!(map.len() <= FLOWS_MAX);
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
                merge_method: MergeMethod::Squash,
                expected_head: "aaa111".to_owned(),
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

        let effects = run.control(FlowControl::TakeOver).unwrap();
        assert_eq!(
            effects,
            vec![FlowEffect::OpenTakeOver {
                worktree: wt(2),
                view_name: "implement (takeover)".to_owned(),
                harness: Harness::Antigravity,
                session_id: Some("agy-conv-1".to_owned()),
                fallback_view: Some(view(20)),
                stopped_view: None,
            }]
        );

        let effects = run.control(FlowControl::Resume).unwrap();
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

        let effects = run.control(FlowControl::Advance).unwrap();
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
            [FlowEffect::OpenInteractiveStage { view_name, .. }] if view_name == "illegal-states"
        ));
        run.handle(FlowEvent::ViewOpened { view: view(31) });

        let effects = run.control(FlowControl::AutoRest).unwrap();
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

        let effects = run.control(FlowControl::Advance).unwrap();
        assert_eq!(
            effects,
            vec![FlowEffect::MergePr {
                worktree: wt(4),
                pr_url: "https://github.com/owner/repo/pull/42".to_owned(),
                merge_method: MergeMethod::Squash,
                expected_head: "head1".to_owned(),
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
            vec![FlowEffect::PushForceWithLease {
                worktree: wt(5),
                expected_head: "h2".to_owned(),
            }]
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

    #[test]
    fn repeated_merge_race_pauses_after_merge_race_repoll_max() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        let (mut run, _) = FlowRun::start(
            wt(9),
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
        let clean = pr(
            "h1",
            PrState::Open,
            Mergeable::Mergeable,
            MergeStateStatus::Clean,
            0,
            0,
            2,
        );
        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: clean.clone(),
            now_secs: 40,
        });
        assert!(matches!(effects.as_slice(), [FlowEffect::MergePr { .. }]));

        let effects = run.handle(FlowEvent::MergeFinished {
            outcome: MergeOutcome::BaseUpdatedRace,
            now_secs: 41,
        });
        assert_eq!(effects, vec![FlowEffect::PollPr { worktree: wt(9) }]);
        assert_eq!(run.merge_race_repolls, 1);

        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: clean,
            now_secs: 42,
        });
        assert!(matches!(effects.as_slice(), [FlowEffect::MergePr { .. }]));

        let effects = run.handle(FlowEvent::MergeFinished {
            outcome: MergeOutcome::BaseUpdatedRace,
            now_secs: 43,
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        assert_eq!(run.summary().pause_reason, Some(PauseReason::MergeFailed));
    }

    #[test]
    fn worker_failed_pauses_every_in_flight_phase() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![
            StageSpec {
                label: "tigerstyle".to_owned(),
                kind: StageKind::ReviewSkill {
                    skill: "tigerstyle".to_owned(),
                },
            },
            StageSpec {
                label: "land".to_owned(),
                kind: StageKind::Land,
            },
        ];
        let (mut checking, _) = FlowRun::start(
            wt(10),
            spec.clone(),
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        checking.handle(FlowEvent::ViewOpened { view: view(100) });
        checking.handle(FlowEvent::TerminalExited {
            view: view(100),
            exit: TerminalExit::Code(0),
        });
        assert!(matches!(checking.phase, FlowPhase::CheckingStage(_)));
        checking.handle(FlowEvent::WorkerFailed {
            why: "worker panicked".to_owned(),
        });
        assert_eq!(
            checking.summary().pause_reason,
            Some(PauseReason::StepFailed)
        );

        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        let (mut pushing, _) = FlowRun::start(
            wt(11),
            spec.clone(),
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        pushing.handle(FlowEvent::WorkerFailed {
            why: "push worker panicked".to_owned(),
        });
        assert_eq!(
            pushing.summary().pause_reason,
            Some(PauseReason::PushFailed)
        );

        let (mut merging, _) = FlowRun::start(
            wt(12),
            spec,
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        merging.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 10,
        });
        merging.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Clean,
                0,
                0,
                1,
            ),
            now_secs: 40,
        });
        assert!(matches!(
            merging.phase,
            FlowPhase::Landing(LandState::Merging { .. })
        ));
        merging.handle(FlowEvent::WorkerFailed {
            why: "merge worker panicked".to_owned(),
        });
        assert_eq!(
            merging.summary().pause_reason,
            Some(PauseReason::MergeFailed)
        );
    }

    #[test]
    fn unstable_merge_state_without_failing_checks_pauses() {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        let (mut run, _) = FlowRun::start(
            wt(13),
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
        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: pr(
                "h1",
                PrState::Open,
                Mergeable::Mergeable,
                MergeStateStatus::Unstable,
                0,
                0,
                2,
            ),
            now_secs: 50,
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: true, .. }]
        ));
        assert_eq!(
            run.summary().pause_reason,
            Some(PauseReason::PullRequestUnstable)
        );
    }

    #[test]
    fn deserialization_rejects_corrupt_spec_and_invalid_confirm_merge() {
        let mut registry = FlowRegistry::default();
        registry
            .start(
                wt(14),
                FlowSpec::default_spec(),
                FlowMode::Unattended,
                Harness::ClaudeCode,
                FlowEntry::PullRequest,
            )
            .unwrap();
        let mut raw: serde_json::Value = serde_json::to_value(&registry).unwrap();
        raw["flows"][0]["spec"]["step_timeout_secs"] = serde_json::json!(0);
        assert!(serde_json::from_value::<FlowRegistry>(raw).is_err());

        let mut bad_confirm: serde_json::Value = serde_json::to_value(&registry).unwrap();
        bad_confirm["flows"][0]["phase"] = serde_json::json!({
            "Paused": {
                "reason": "ConfirmMerge",
                "detail": "ready",
                "resume_session": null,
                "view": null,
                "stage_step": "Stage",
                "context": "None"
            }
        });
        assert!(serde_json::from_value::<FlowRegistry>(bad_confirm).is_err());
    }

    fn land_only(mode: FlowMode) -> FlowRun {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        let (run, effects) = FlowRun::start(
            wt(9),
            spec,
            mode,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        assert_eq!(effects, vec![FlowEffect::PushIfAhead { worktree: wt(9) }]);
        run
    }

    fn conflicting(head: &str) -> PrSnapshot {
        pr(
            head,
            PrState::Open,
            Mergeable::Conflicting,
            MergeStateStatus::Dirty,
            0,
            0,
            0,
        )
    }

    fn resolving_rebase() -> FlowRun {
        let mut run = land_only(FlowMode::Unattended);
        run.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 100,
        });
        let effects = run.handle(FlowEvent::PrSnapshot {
            snapshot: conflicting("h1"),
            now_secs: 101,
        });
        assert_eq!(effects, vec![FlowEffect::RunRebase { worktree: wt(9) }]);
        run.handle(FlowEvent::RebaseFinished(RebaseOutcome::Stopped {
            onto: "refs/remotes/origin/main".to_owned(),
            prompt: "resolve".to_owned(),
        }));
        run.handle(FlowEvent::ViewOpened { view: view(90) });
        run
    }

    #[test]
    fn a_control_the_phase_does_not_offer_is_refused_and_moves_nothing() {
        let mut run = land_only(FlowMode::Unattended);
        let before = run.clone();
        for control in [
            FlowControl::Advance,
            FlowControl::Pause,
            FlowControl::Resume,
            FlowControl::RetryStep,
            FlowControl::SkipStep,
            FlowControl::TakeOver,
        ] {
            let refused = run.control(control);
            assert!(refused.is_err(), "{control:?} is refused while pushing");
            assert_eq!(run, before);
        }
        assert_eq!(run.available_controls(), vec![FlowControl::Abort]);
        let mut registry = FlowRegistry::default();
        registry
            .start(
                wt(9),
                FlowSpec::default_spec(),
                FlowMode::Unattended,
                Harness::ClaudeCode,
                FlowEntry::PullRequest,
            )
            .unwrap();
        assert!(registry.control(wt(9), FlowControl::Resume).is_err());
        assert!(registry.control(wt(8), FlowControl::Abort).is_err());
        assert!(registry.get(wt(9)).is_some_and(FlowRun::is_active));
    }

    #[test]
    fn pause_abort_and_take_over_stop_a_running_headless_step() {
        let mut run = resolving_rebase();
        let stop = FlowEffect::StopStep {
            worktree: wt(9),
            view: view(90),
        };
        let mut paused = run.clone();
        assert_eq!(
            paused.control(FlowControl::Pause).unwrap(),
            vec![stop.clone()]
        );
        let mut aborted = run.clone();
        assert_eq!(
            aborted.control(FlowControl::Abort).unwrap(),
            vec![stop.clone()]
        );
        assert_eq!(aborted.phase, FlowPhase::Done(FlowDone::Aborted));
        let effects = run.control(FlowControl::TakeOver).unwrap();
        assert_eq!(
            effects,
            vec![
                stop,
                FlowEffect::OpenTakeOver {
                    worktree: wt(9),
                    view_name: "land (takeover)".to_owned(),
                    harness: Harness::ClaudeCode,
                    session_id: None,
                    fallback_view: Some(view(90)),
                    stopped_view: Some(view(90)),
                },
            ]
        );
    }

    #[test]
    fn resuming_a_taken_over_rebase_checks_it_and_force_pushes_the_checked_head() {
        let mut run = resolving_rebase();
        run.control(FlowControl::TakeOver).unwrap();
        let effects = run.control(FlowControl::Resume).unwrap();
        assert_eq!(
            effects,
            vec![FlowEffect::CheckRebasePostConditions {
                worktree: wt(9),
                onto: "refs/remotes/origin/main".to_owned(),
            }]
        );
        let effects = run.handle(FlowEvent::RebasePostConditionsChecked(Ok("h2".to_owned())));
        assert_eq!(
            effects,
            vec![FlowEffect::PushForceWithLease {
                worktree: wt(9),
                expected_head: "h2".to_owned(),
            }]
        );
        let effects = run.handle(FlowEvent::PushFinished {
            result: Ok("h3".to_owned()),
            now_secs: 200,
        });
        assert!(matches!(effects.as_slice(), [FlowEffect::Alert { .. }]));
        assert_eq!(run.summary().pause_reason, Some(PauseReason::PushFailed));
        let effects = run.control(FlowControl::Resume).unwrap();
        assert_eq!(
            effects,
            vec![FlowEffect::PushForceWithLease {
                worktree: wt(9),
                expected_head: "h2".to_owned(),
            }]
        );
    }

    #[test]
    fn a_restart_mid_rebase_resumes_the_rebase_and_not_a_plain_push() {
        let mut rebasing = land_only(FlowMode::Unattended);
        rebasing.handle(FlowEvent::PushFinished {
            result: Ok("h1".to_owned()),
            now_secs: 100,
        });
        rebasing.handle(FlowEvent::PrSnapshot {
            snapshot: conflicting("h1"),
            now_secs: 101,
        });
        let mut pushing = rebasing.clone();
        pushing.handle(FlowEvent::RebaseFinished(RebaseOutcome::Clean {
            new_head: "h2".to_owned(),
        }));

        rebasing.handle(FlowEvent::DaemonRestarted);
        assert_eq!(
            rebasing.control(FlowControl::Resume).unwrap(),
            vec![FlowEffect::RunRebase { worktree: wt(9) }]
        );
        assert_eq!(rebasing.rebase_attempts, 1);

        pushing.handle(FlowEvent::DaemonRestarted);
        assert_eq!(
            pushing.control(FlowControl::Resume).unwrap(),
            vec![FlowEffect::PushForceWithLease {
                worktree: wt(9),
                expected_head: "h2".to_owned(),
            }]
        );
    }

    #[test]
    fn waiting_for_the_pull_request_times_out_into_a_pause() {
        let mut run = land_only(FlowMode::Unattended);
        run.handle(FlowEvent::PushFinished {
            result: Ok("local".to_owned()),
            now_secs: 1000,
        });
        let open = |head: &str, mergeable, merge_state, pending| {
            pr(head, PrState::Open, mergeable, merge_state, pending, 0, 0)
        };
        let early = 1000 + PR_INDEX_SECS_MAX - 1;
        let late = 1000 + PR_INDEX_SECS_MAX;

        let mut behind = run.clone();
        let theirs = open("theirs", Mergeable::Mergeable, MergeStateStatus::Clean, 0);
        let effects = behind.handle(FlowEvent::PrSnapshot {
            snapshot: theirs.clone(),
            now_secs: early,
        });
        assert_eq!(effects, Vec::new());
        behind.handle(FlowEvent::PrSnapshot {
            snapshot: theirs,
            now_secs: late,
        });
        assert_eq!(
            behind.summary().pause_reason,
            Some(PauseReason::PullRequestHeadMismatch)
        );

        let mut unknown = run.clone();
        unknown.handle(FlowEvent::PrSnapshot {
            snapshot: open("local", Mergeable::Unknown, MergeStateStatus::Unknown, 0),
            now_secs: late,
        });
        assert_eq!(
            unknown.summary().pause_reason,
            Some(PauseReason::PullRequestStateUnknown)
        );

        let mut pending = run.clone();
        let running = open("local", Mergeable::Mergeable, MergeStateStatus::Blocked, 2);
        let effects = pending.handle(FlowEvent::PrSnapshot {
            snapshot: running.clone(),
            now_secs: 1000 + CHECKS_WAIT_SECS_MAX - 1,
        });
        assert_eq!(effects, Vec::new());
        pending.handle(FlowEvent::PrSnapshot {
            snapshot: running,
            now_secs: 1000 + CHECKS_WAIT_SECS_MAX,
        });
        assert_eq!(
            pending.summary().pause_reason,
            Some(PauseReason::ChecksTimedOut)
        );

        run.handle(FlowEvent::PrMissing);
        assert_eq!(run.summary().pause_reason, Some(PauseReason::NoPullRequest));
    }

    #[test]
    fn pausing_an_interactive_wait_resumes_into_the_same_view() {
        let (mut run, _) = FlowRun::start(
            wt(10),
            FlowSpec::default_spec(),
            FlowMode::Interactive,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::ViewOpened { view: view(100) });
        let waiting = run.phase.clone();
        assert_eq!(run.control(FlowControl::Pause).unwrap(), Vec::new());
        assert_eq!(run.control(FlowControl::Resume).unwrap(), Vec::new());
        assert_eq!(run.phase, waiting);

        run.control(FlowControl::Pause).unwrap();
        let effects = run.control(FlowControl::AutoRest).unwrap();
        assert_eq!(run.mode, FlowMode::Unattended);
        assert_eq!(
            effects,
            vec![FlowEffect::CheckStagePostConditions {
                worktree: wt(10),
                require_open_pr: false,
            }]
        );
    }

    #[test]
    fn an_unusable_session_id_is_reported_instead_of_dropped() {
        let (mut run, _) = FlowRun::start(
            wt(11),
            FlowSpec::default_spec(),
            FlowMode::Unattended,
            Harness::ClaudeCode,
            FlowEntry::PullRequest,
        )
        .unwrap();
        run.handle(FlowEvent::ViewOpened { view: view(110) });
        let effects = run.handle(FlowEvent::SessionCaptured {
            view: view(110),
            session_id: "bad\nid".to_owned(),
        });
        assert!(matches!(
            effects.as_slice(),
            [FlowEffect::Alert { urgent: false, .. }]
        ));
        assert_eq!(run.resume_session(), None);
    }

    #[test]
    fn loading_rejects_a_phase_that_disagrees_with_its_stage() {
        let mut run = land_only(FlowMode::Unattended);
        run.stages.insert(
            0,
            StageSpec {
                label: "tigerstyle".to_owned(),
                kind: StageKind::ReviewSkill {
                    skill: "tigerstyle".to_owned(),
                },
            },
        );
        assert!(run.validate().is_err());
        let mut rebasing = land_only(FlowMode::Unattended);
        rebasing.phase = FlowPhase::Landing(LandState::Rebasing);
        assert!(rebasing.validate().is_err());
    }
}
