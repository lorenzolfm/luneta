pub mod config;
pub mod ops;
pub mod store;

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lune_domain::agent::FlowHarness;
use lune_domain::flows::{
    AgentStage, FLOW_NAME_DEFAULT, FLOWS_MAX, FlowControl, FlowEffect, FlowEntry, FlowEvent,
    FlowMode, FlowPhase, FlowRegistry, FlowRun, FlowSummary, SessionId, TerminalExit,
};
use lune_domain::projects::{Argv, Opens, ViewId, WorktreeId};
use lune_protocol::github::PullPage;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::api;
use crate::core;
use crate::layouts;
use crate::log;
use crate::panics::{self, Panicked};

use self::config::LoadedFlows;
use self::store::FlowStore;

const COMMANDS_MAX: usize = 64;

const WORKERS_MAX: usize = FLOWS_MAX * 2;

const WORKER_QUEUE_MAX: usize = WORKERS_MAX;

const WORKER_DRAIN_MAX: usize = WORKERS_MAX;

const EFFECTS_DISPATCH_MAX: usize = 64;

const FLOW_SWEEP: Duration = Duration::from_secs(2);

const LAND_POLL_INTERVAL: Duration = Duration::from_secs(10);

const ASK_PATIENCE: Duration = Duration::from_secs(10);

const UNCLAIMED_EXITS_MAX: usize = 64;

const TAKEOVER_WAIT_POLL: Duration = Duration::from_millis(200);

const TAKEOVER_WAIT_POLLS_MAX: u32 = 50;

const _: () = assert!(COMMANDS_MAX > 0);
const _: () = assert!(WORKERS_MAX >= FLOWS_MAX);
const _: () = assert!(WORKER_QUEUE_MAX >= WORKERS_MAX);
const _: () = assert!(WORKER_DRAIN_MAX > 0);
const _: () = assert!(EFFECTS_DISPATCH_MAX > 0);
const _: () = assert!(FLOW_SWEEP.as_secs() > 0);
const _: () = assert!(LAND_POLL_INTERVAL.as_secs() >= FLOW_SWEEP.as_secs());
const _: () = assert!(ASK_PATIENCE.as_secs() > 0);
const _: () = assert!(UNCLAIMED_EXITS_MAX > 0);
const _: () = assert!(TAKEOVER_WAIT_POLLS_MAX > 0);

pub enum Command {
    Start {
        worktree: WorktreeId,
        dir: String,
        flow_name: Option<String>,
        mode: FlowMode,
        harness: FlowHarness,
        entry: FlowEntry,
        answer: oneshot::Sender<Result<api::FlowSummary, String>>,
    },
    Control {
        worktree: WorktreeId,
        dir: Option<String>,
        control: FlowControl,
        answer: oneshot::Sender<Result<Option<api::FlowSummary>, String>>,
    },
    List {
        answer: oneshot::Sender<api::ListFlowsResponse>,
    },
    TerminalExited {
        view: ViewId,
        exit: TerminalExit,
    },
    WorktreesKnown(BTreeMap<WorktreeId, String>),
    AgentSessions(BTreeMap<ViewId, String>),
}

struct WorkerDone {
    worktree: WorktreeId,
    generation: u64,
    kind: WorkerKind,
    event: Option<FlowEvent>,
}

enum WorkerKind {
    PrPoll,
    Operation,
    OpenStep { stop: PathBuf },
}

struct WorkerSlot {
    active: Arc<AtomicUsize>,
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        let previous = self.active.fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "active worker count never underflows");
    }
}

fn try_acquire_worker(active: &Arc<AtomicUsize>) -> Option<WorkerSlot> {
    let acquired = active.try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        (current < WORKERS_MAX).then_some(current + 1)
    });
    acquired.ok().map(|_| WorkerSlot {
        active: Arc::clone(active),
    })
}

pub fn ask<T>(
    flows: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Result<T, String> {
    let (answer, answered) = oneshot::channel();
    let stopped = || "the flows actor has stopped".to_owned();
    flows.send(question(answer)).map_err(|_| stopped())?;
    let waited = Handle::current().block_on(tokio::time::timeout(ASK_PATIENCE, answered));
    match waited {
        Ok(Ok(answered)) => Ok(answered),
        Ok(Err(_)) => Err(stopped()),
        Err(_) => Err(format!(
            "the flows actor did not answer within {}s",
            ASK_PATIENCE.as_secs()
        )),
    }
}

pub fn start(
    core: SyncSender<core::Command>,
    layouts: SyncSender<layouts::Command>,
    wake_github: SyncSender<()>,
) -> io::Result<SyncSender<Command>> {
    let (send, commands) = mpsc::sync_channel(COMMANDS_MAX);
    let (store, store_warning) = FlowStore::open();
    if let Some(why) = store_warning {
        log::say(&why);
    }
    let loaded_config = config::load();
    log_config_warnings(&loaded_config.warnings);
    let Loaded {
        mut registry,
        warning: load_warning,
        persist_blocked,
    } = loaded(&store);
    match store.clear_run_dir() {
        Ok(0) => {}
        Ok(cleared) => log::say(&format!("flows cleared {cleared} files of earlier steps")),
        Err(why) => log::say(&format!("flows could not clear earlier step files: {why}")),
    }
    let restart_effects = registry.on_daemon_restarted();
    match store.clear_pi_sessions_except(&pi_sessions_kept(&registry)) {
        Ok(0) => {}
        Ok(cleared) => log::say(&format!(
            "flows cleared {cleared} pi sessions no flow holds"
        )),
        Err(why) => log::say(&format!("flows could not clear old pi sessions: {why}")),
    }
    let restarted = !restart_effects.is_empty();
    log_alerts(restart_effects);
    let mut actor = Actor::new(registry, store, loaded_config, core, layouts, wake_github);
    actor.dirty = restarted;
    actor.load_warning = load_warning;
    actor.persist_blocked = persist_blocked;
    thread::Builder::new()
        .name("flows".to_owned())
        .spawn(move || run(actor, &commands))?;
    Ok(send)
}

fn pi_sessions_kept(registry: &FlowRegistry) -> Vec<PathBuf> {
    registry
        .iter()
        .filter(|(_, run)| run.harness == FlowHarness::Pi)
        .filter_map(|(_, run)| run.resume_session())
        .map(|session| PathBuf::from(session.as_str()))
        .collect()
}

fn write_stop(stop: &Path) -> Result<(), String> {
    fs::write(stop, b"").map_err(|error| format!("{}: {error}", stop.display()))
}

struct Loaded {
    registry: FlowRegistry,
    warning: Option<String>,
    persist_blocked: bool,
}

fn loaded(store: &FlowStore) -> Loaded {
    match store.load() {
        Ok(registry) => Loaded {
            registry,
            warning: None,
            persist_blocked: false,
        },
        Err(why) => {
            let mut persist_blocked = false;
            let kept = match store.set_aside() {
                Ok(Some(aside)) => format!("kept it as {}", aside.display()),
                Ok(None) => "it is gone".to_owned(),
                Err(unmoved) => {
                    persist_blocked = true;
                    format!("{unmoved}, so flows are not saved until it is moved")
                }
            };
            let warning = format!("flows state unreadable ({why}); {kept}");
            log::say(&warning);
            Loaded {
                registry: FlowRegistry::default(),
                warning: Some(warning),
                persist_blocked,
            }
        }
    }
}

impl Actor {
    fn new(
        registry: FlowRegistry,
        store: FlowStore,
        config: LoadedFlows,
        core: SyncSender<core::Command>,
        layouts: SyncSender<layouts::Command>,
        wake_github: SyncSender<()>,
    ) -> Actor {
        Actor {
            registry,
            store,
            config,
            core,
            layouts,
            wake_github,
            worktree_dirs: BTreeMap::new(),
            generations: BTreeMap::new(),
            steps_opened_count: 0,
            session_files: BTreeMap::new(),
            delivered_sessions: BTreeMap::new(),
            step_files: BTreeMap::new(),
            stop_files: BTreeMap::new(),
            unclaimed_exits: BTreeMap::new(),
            pr_poll_in_flight: BTreeMap::new(),
            last_pr_poll: BTreeMap::new(),
            told_summaries: None,
            workers_active: Arc::new(AtomicUsize::new(0)),
            dirty: false,
            load_warning: None,
            persist_blocked: false,
            persist_failure: None,
            control_failure: None,
        }
    }
}

fn log_config_warnings(warnings: &[String]) {
    for warning in warnings {
        log::say(&format!("flows config: {warning}"));
    }
}

fn log_alerts(effects: Vec<FlowEffect>) {
    for effect in effects {
        let FlowEffect::Alert {
            worktree,
            title,
            detail,
            ..
        } = effect
        else {
            unreachable!("this event only alerts: {effect:?}");
        };
        log::say(&format!("flow worktree {worktree}: {title} — {detail}"));
    }
}

struct Actor {
    registry: FlowRegistry,
    store: FlowStore,
    config: LoadedFlows,
    core: SyncSender<core::Command>,
    layouts: SyncSender<layouts::Command>,
    wake_github: SyncSender<()>,
    worktree_dirs: BTreeMap<WorktreeId, String>,
    generations: BTreeMap<WorktreeId, u64>,
    steps_opened_count: u64,
    session_files: BTreeMap<WorktreeId, PathBuf>,
    delivered_sessions: BTreeMap<WorktreeId, SessionId>,
    step_files: BTreeMap<WorktreeId, Vec<PathBuf>>,
    stop_files: BTreeMap<WorktreeId, PathBuf>,
    unclaimed_exits: BTreeMap<ViewId, TerminalExit>,
    pr_poll_in_flight: BTreeMap<WorktreeId, u64>,
    last_pr_poll: BTreeMap<WorktreeId, Instant>,
    told_summaries: Option<BTreeMap<WorktreeId, api::FlowSummary>>,
    workers_active: Arc<AtomicUsize>,
    dirty: bool,
    load_warning: Option<String>,
    persist_blocked: bool,
    persist_failure: Option<String>,
    control_failure: Option<String>,
}

enum StepPrompt {
    Stage {
        dir: String,
        stage: AgentStage,
        pr_url: Option<PullPage>,
    },
    Helper(String),
}

impl StepPrompt {
    fn texts(self, harness: FlowHarness) -> (String, Option<String>) {
        match self {
            StepPrompt::Stage { dir, stage, pr_url } => {
                ops::stage_prompts(&dir, harness, &stage, pr_url.as_ref())
            }
            StepPrompt::Helper(prompt) => (prompt, None),
        }
    }
}

fn run(mut actor: Actor, commands: &Receiver<Command>) {
    let (worker_tx, worker_rx) = mpsc::sync_channel::<WorkerDone>(WORKER_QUEUE_MAX);
    actor.tell_core();
    let mut last_sweep = Instant::now();
    loop {
        assert!(actor.registry.len() <= FLOWS_MAX);
        assert!(actor.pr_poll_in_flight.len() <= FLOWS_MAX);
        drain_workers(&mut actor, &worker_rx, &worker_tx);
        let wait = FLOW_SWEEP.saturating_sub(last_sweep.elapsed());
        match commands.recv_timeout(wait) {
            Ok(command) => actor.handle_command(command, &worker_tx),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                actor.flush_dirty();
                actor.persist();
                return;
            }
        }
        drain_workers(&mut actor, &worker_rx, &worker_tx);
        if last_sweep.elapsed() >= FLOW_SWEEP {
            actor.sweep(&worker_tx);
            last_sweep = Instant::now();
        }
        actor.flush_dirty();
        actor.tell_core();
    }
}

fn drain_workers(
    actor: &mut Actor,
    worker_rx: &Receiver<WorkerDone>,
    worker_tx: &SyncSender<WorkerDone>,
) {
    for _ in 0..WORKER_DRAIN_MAX {
        match worker_rx.try_recv() {
            Ok(done) => actor.handle_worker_done(done, worker_tx),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

impl Actor {
    fn generation(&self, worktree: WorktreeId) -> u64 {
        self.generations.get(&worktree).copied().unwrap_or(0)
    }

    fn bump_generation(&mut self, worktree: WorktreeId) -> u64 {
        let entry = self.generations.entry(worktree).or_insert(0);
        *entry = entry.wrapping_add(1);
        self.pr_poll_in_flight.remove(&worktree);
        *entry
    }

    fn persist(&mut self) {
        if self.persist_blocked {
            return;
        }
        match self.store.save(&self.registry) {
            Ok(()) => {
                self.dirty = false;
                self.persist_failure = None;
            }
            Err(why) => {
                if self.persist_failure.as_ref() != Some(&why) {
                    log::say(&format!("flows state not saved, retrying: {why}"));
                }
                self.persist_failure = Some(why);
            }
        }
    }

    fn warnings(&self) -> Vec<String> {
        let mut warnings = self.config.warnings.clone();
        warnings.extend(self.load_warning.clone());
        if let Some(why) = &self.persist_failure {
            warnings.push(format!("flows state not saved: {why}"));
        }
        warnings
    }

    fn flush_dirty(&mut self) {
        if self.dirty {
            self.persist();
        }
    }

    fn cleanup_step_files(&mut self, worktree: WorktreeId) {
        self.session_files.remove(&worktree);
        self.delivered_sessions.remove(&worktree);
        if let Some(files) = self.step_files.remove(&worktree) {
            store::remove_run_files(&files);
        }
    }

    fn forget_flow_files(&mut self, worktree: WorktreeId) {
        self.cleanup_step_files(worktree);
        if let Err(why) = self.store.remove_pi_sessions_of(worktree) {
            log::say(&format!(
                "flow worktree {worktree}: pi sessions not removed: {why}"
            ));
        }
    }

    fn step_running(&self, view: ViewId) -> bool {
        layouts::ask(&self.layouts, |answer| layouts::Command::StepRunning {
            view,
            answer,
        })
        .unwrap_or(true)
    }

    fn remember_unclaimed_exit(&mut self, view: ViewId, exit: TerminalExit) {
        if !self.unclaimed_exits.contains_key(&view)
            && self.unclaimed_exits.len() >= UNCLAIMED_EXITS_MAX
        {
            self.unclaimed_exits.pop_first();
        }
        self.unclaimed_exits.insert(view, exit);
        assert!(self.unclaimed_exits.len() <= UNCLAIMED_EXITS_MAX);
    }

    fn start_flow(
        &mut self,
        worktree: WorktreeId,
        flow_name: Option<&str>,
        mode: FlowMode,
        harness: FlowHarness,
        entry: FlowEntry,
        worker_tx: &SyncSender<WorkerDone>,
    ) -> Result<api::FlowSummary, String> {
        if self.registry.get(worktree).is_some_and(FlowRun::is_active) {
            return Err("worktree already has an active flow".to_owned());
        }
        let name = flow_name.unwrap_or(FLOW_NAME_DEFAULT);
        let Some(spec) = self.config.get(name).cloned() else {
            return Err(format!("unknown flow {name:?}"));
        };
        let effects = self.registry.start(worktree, spec, mode, harness, entry)?;
        self.bump_generation(worktree);
        self.cleanup_step_files(worktree);
        self.dirty = true;
        self.dispatch(effects, worker_tx);
        self.flush_dirty();
        self.registry
            .get(worktree)
            .map(|run| summary_to_api(&run.summary()))
            .ok_or_else(|| "flow ended immediately on start".to_owned())
    }

    fn control_flow(
        &mut self,
        worktree: WorktreeId,
        control: FlowControl,
        worker_tx: &SyncSender<WorkerDone>,
    ) -> Result<Option<api::FlowSummary>, String> {
        self.read_session_file_for(worktree);
        let Some(run) = self.registry.get(worktree) else {
            return Err("worktree has no active flow".to_owned());
        };
        let held_view = match &run.phase {
            FlowPhase::Paused(paused)
                if !matches!(control, FlowControl::Pause | FlowControl::Abort) =>
            {
                paused.view
            }
            _ => None,
        };
        if let Some(view) = held_view
            && self.step_running(view)
        {
            return Err(format!(
                "the step in view {view} is still running; wait for it to end, or abort the flow"
            ));
        }
        let before = run.phase.clone();
        let effects = self.registry.control(worktree, control)?;
        self.dirty = true;
        let moved = self
            .registry
            .get(worktree)
            .is_none_or(|run| run.phase != before);
        if moved {
            self.bump_generation(worktree);
        }
        self.control_failure = None;
        self.dispatch(effects, worker_tx);
        if self
            .registry
            .get(worktree)
            .is_none_or(|run| !run.is_active())
        {
            self.forget_flow_files(worktree);
            self.last_pr_poll.remove(&worktree);
        }
        self.flush_dirty();
        if let Some(why) = self.control_failure.take() {
            return Err(why);
        }
        Ok(self
            .registry
            .get(worktree)
            .map(|run| summary_to_api(&run.summary())))
    }

    fn tell_core(&mut self) {
        let summaries: BTreeMap<WorktreeId, api::FlowSummary> = self
            .registry
            .summaries()
            .into_iter()
            .map(|(id, summary)| (id, summary_to_api(&summary)))
            .collect();
        if self.told_summaries.as_ref() == Some(&summaries) {
            return;
        }
        if self
            .core
            .send(core::Command::Flows(summaries.clone()))
            .is_ok()
        {
            self.told_summaries = Some(summaries);
        }
    }

    fn handle_command(&mut self, command: Command, worker_tx: &SyncSender<WorkerDone>) {
        match command {
            Command::Start {
                worktree,
                dir,
                flow_name,
                mode,
                harness,
                entry,
                answer,
            } => {
                self.config = config::load();
                log_config_warnings(&self.config.warnings);
                self.worktree_dirs.insert(worktree, dir);
                let result = self.start_flow(
                    worktree,
                    flow_name.as_deref(),
                    mode,
                    harness,
                    entry,
                    worker_tx,
                );
                let _ = answer.send(result);
            }
            Command::Control {
                worktree,
                dir,
                control,
                answer,
            } => {
                if let Some(dir) = dir {
                    self.worktree_dirs.insert(worktree, dir);
                }
                let result = self.control_flow(worktree, control, worker_tx);
                let _ = answer.send(result);
            }
            Command::List { answer } => {
                self.config = config::load();
                let flows = self
                    .registry
                    .summaries()
                    .values()
                    .map(summary_to_api)
                    .collect();
                let response = api::ListFlowsResponse {
                    flows,
                    defined_flows: self.config.names(),
                    config_warnings: self.warnings(),
                };
                let _ = answer.send(response);
            }
            Command::TerminalExited { view, exit } => {
                let target = self
                    .registry
                    .iter()
                    .find(|(_, run)| run.active_view() == Some(view))
                    .map(|(worktree, _)| worktree);
                let Some(worktree) = target else {
                    self.remember_unclaimed_exit(view, exit);
                    return;
                };
                self.exited(worktree, view, exit, worker_tx);
            }
            Command::WorktreesKnown(known) => {
                let before = self.registry.len();
                self.worktree_dirs = known;
                let mut removed_files: Vec<WorktreeId> = self
                    .step_files
                    .keys()
                    .copied()
                    .chain(self.registry.iter().map(|(id, _)| id))
                    .filter(|id| !self.worktree_dirs.contains_key(id))
                    .collect();
                removed_files.sort_unstable();
                removed_files.dedup();
                for worktree in removed_files {
                    self.forget_flow_files(worktree);
                }
                let known_dirs = &self.worktree_dirs;
                self.stop_files.retain(|id, _| known_dirs.contains_key(id));
                self.delivered_sessions
                    .retain(|id, _| known_dirs.contains_key(id));
                self.registry
                    .retain_worktrees(|id| known_dirs.contains_key(&id));
                self.generations.retain(|id, _| known_dirs.contains_key(id));
                self.session_files
                    .retain(|id, _| known_dirs.contains_key(id));
                self.last_pr_poll
                    .retain(|id, _| known_dirs.contains_key(id));
                self.pr_poll_in_flight
                    .retain(|id, _| known_dirs.contains_key(id));
                if self.registry.len() != before {
                    self.dirty = true;
                }
            }
            Command::AgentSessions(sessions) => {
                let worktrees: Vec<(WorktreeId, ViewId)> = self
                    .registry
                    .iter()
                    .filter_map(|(wt, run)| Some((wt, run.active_view()?)))
                    .collect();
                for (worktree, view) in worktrees {
                    if let Some(session_id) = sessions.get(&view)
                        && self
                            .registry
                            .get(worktree)
                            .and_then(FlowRun::resume_session)
                            .map(SessionId::as_str)
                            != Some(session_id.as_str())
                    {
                        let effects = self.registry.handle(
                            worktree,
                            FlowEvent::SessionCaptured {
                                view,
                                session_id: session_id.clone(),
                            },
                        );
                        log_alerts(effects);
                        self.dirty = true;
                    }
                }
            }
        }
    }

    fn handle_worker_done(&mut self, done: WorkerDone, worker_tx: &SyncSender<WorkerDone>) {
        if matches!(done.kind, WorkerKind::PrPoll)
            && self.pr_poll_in_flight.get(&done.worktree) == Some(&done.generation)
        {
            self.pr_poll_in_flight.remove(&done.worktree);
        }
        if done.generation != self.generation(done.worktree) {
            self.abandoned(done);
            return;
        }
        let Some(event) = done.event else {
            return;
        };
        let opened = match &event {
            FlowEvent::ViewOpened { view } => Some(*view),
            _ => None,
        };
        let effects = self.registry.handle(done.worktree, event);
        self.dirty = true;
        if self
            .registry
            .get(done.worktree)
            .is_none_or(|run| !run.is_active())
        {
            self.forget_flow_files(done.worktree);
            self.last_pr_poll.remove(&done.worktree);
        }
        self.dispatch(effects, worker_tx);
        if let Some(view) = opened
            && let Some(exit) = self.unclaimed_exits.remove(&view)
            && self
                .registry
                .get(done.worktree)
                .and_then(FlowRun::active_view)
                == Some(view)
        {
            self.exited(done.worktree, view, exit, worker_tx);
        }
    }

    fn abandoned(&mut self, done: WorkerDone) {
        let WorkerKind::OpenStep { stop } = done.kind else {
            return;
        };
        let Some(FlowEvent::ViewOpened { view }) = done.event else {
            return;
        };
        if let Err(why) = write_stop(&stop) {
            log::say(&format!(
                "flow worktree {}: the step in view {view} opened after the flow moved on, and \
                 could not be told to stop: {why}",
                done.worktree
            ));
        }
        let effects = self
            .registry
            .handle(done.worktree, FlowEvent::AbandonedViewOpened { view });
        log_alerts(effects);
        self.dirty = true;
    }

    fn exited(
        &mut self,
        worktree: WorktreeId,
        view: ViewId,
        exit: TerminalExit,
        worker_tx: &SyncSender<WorkerDone>,
    ) {
        self.read_session_file_for(worktree);
        let effects = self
            .registry
            .handle(worktree, FlowEvent::TerminalExited { view, exit });
        self.dirty = true;
        self.dispatch(effects, worker_tx);
    }

    fn sweep(&mut self, worker_tx: &SyncSender<WorkerDone>) {
        let worktrees: Vec<WorktreeId> = self.registry.iter().map(|(id, _)| id).collect();
        for worktree in worktrees {
            self.read_session_file_for(worktree);
            let needs_poll = self
                .registry
                .get(worktree)
                .is_some_and(FlowRun::expects_pr_snapshots);
            if needs_poll
                && !self.pr_poll_in_flight.contains_key(&worktree)
                && self
                    .last_pr_poll
                    .get(&worktree)
                    .is_none_or(|at| at.elapsed() >= LAND_POLL_INTERVAL)
            {
                self.dispatch_pr_poll(worktree, worker_tx);
            }
        }
    }

    fn read_session_file_for(&mut self, worktree: WorktreeId) {
        let Some(path) = self.session_files.get(&worktree).cloned() else {
            return;
        };
        let session_id = match store::read_session_out(&path) {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(why) => {
                log::say(&format!(
                    "flow worktree {worktree}: unreadable session file: {why}"
                ));
                self.session_files.remove(&worktree);
                return;
            }
        };
        let Some(view) = self.registry.get(worktree).and_then(FlowRun::active_view) else {
            return;
        };
        if self
            .registry
            .get(worktree)
            .and_then(FlowRun::resume_session)
            == Some(&session_id)
            || self.delivered_sessions.get(&worktree) == Some(&session_id)
        {
            return;
        }
        self.delivered_sessions.insert(worktree, session_id.clone());
        let effects = self.registry.handle(
            worktree,
            FlowEvent::SessionCaptured {
                view,
                session_id: session_id.as_str().to_owned(),
            },
        );
        log_alerts(effects);
        self.dirty = true;
    }

    fn worktree_dir(&self, worktree: WorktreeId) -> Result<String, String> {
        self.worktree_dirs
            .get(&worktree)
            .cloned()
            .ok_or_else(|| format!("no worktree has the id {worktree}"))
    }

    fn dispatch(&mut self, initial: Vec<FlowEffect>, worker_tx: &SyncSender<WorkerDone>) {
        let mut queue = VecDeque::from(initial);
        for _ in 0..EFFECTS_DISPATCH_MAX {
            let Some(effect) = queue.pop_front() else {
                break;
            };
            let more = self.dispatch_one(effect, worker_tx);
            queue.extend(more);
        }
        assert!(
            queue.is_empty(),
            "flow effect dispatch exceeded limit of {EFFECTS_DISPATCH_MAX}"
        );
    }

    fn dispatch_one(
        &mut self,
        effect: FlowEffect,
        worker_tx: &SyncSender<WorkerDone>,
    ) -> Vec<FlowEffect> {
        match effect {
            FlowEffect::Alert {
                worktree,
                title,
                detail,
                ..
            } => {
                log::say(&format!("flow worktree {worktree}: {title} — {detail}"));
                Vec::new()
            }
            FlowEffect::OpenInteractiveStage {
                worktree,
                view_name,
                harness,
                stage,
            } => {
                let pr_url = self
                    .registry
                    .get(worktree)
                    .and_then(|run| run.last_pr_url.clone());
                let dir = match self.worktree_dir(worktree) {
                    Ok(dir) => dir,
                    Err(why) => {
                        self.dirty = true;
                        return self
                            .registry
                            .handle(worktree, FlowEvent::ViewOpenFailed { why });
                    }
                };
                let generation = self.generation(worktree);
                let argv =
                    move || ops::interactive_stage_argv(&dir, harness, &stage, pr_url.as_ref());
                self.spawn_open_terminal(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    view_name,
                    argv,
                    worker_tx,
                )
            }
            FlowEffect::OpenUnattendedStage {
                worktree,
                view_name,
                harness,
                stage,
                timeout_secs,
            } => {
                let pr_url = self
                    .registry
                    .get(worktree)
                    .and_then(|run| run.last_pr_url.clone());
                let dir = match self.worktree_dir(worktree) {
                    Ok(dir) => dir,
                    Err(why) => {
                        self.dirty = true;
                        return self
                            .registry
                            .handle(worktree, FlowEvent::ViewOpenFailed { why });
                    }
                };
                let prompt = StepPrompt::Stage { dir, stage, pr_url };
                self.open_unattended_step(
                    worktree,
                    view_name,
                    harness,
                    prompt,
                    timeout_secs,
                    worker_tx,
                )
            }
            FlowEffect::OpenUnattendedHelper {
                worktree,
                view_name,
                harness,
                prompt,
                timeout_secs,
            } => self.open_unattended_step(
                worktree,
                view_name,
                harness,
                StepPrompt::Helper(prompt),
                timeout_secs,
                worker_tx,
            ),
            FlowEffect::OpenTakeOver {
                worktree,
                view_name,
                harness,
                session_id,
                fallback_view,
                stopped_view,
            } => {
                self.open_take_over(
                    worktree,
                    view_name,
                    harness,
                    session_id.as_ref(),
                    fallback_view,
                    stopped_view,
                );
                Vec::new()
            }
            FlowEffect::StopStep { worktree, view } => {
                self.stop_step(worktree, view);
                Vec::new()
            }
            FlowEffect::CheckStagePostConditions {
                worktree,
                require_open_pr,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let result = match dir {
                            Ok(dir) => ops::check_stage_post_conditions(&dir, require_open_pr),
                            Err(why) => Err(lune_domain::flows::StageGateFailure::Failed(why)),
                        };
                        FlowEvent::StagePostConditionsChecked(result)
                    },
                )
            }
            FlowEffect::PushIfAhead { worktree } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let result = dir.and_then(|dir| ops::push_if_ahead(&dir));
                        FlowEvent::PushFinished {
                            result,
                            now_secs: unix_now_secs(),
                        }
                    },
                )
            }
            FlowEffect::PushForceWithLease {
                worktree,
                expected_head,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let result =
                            dir.and_then(|dir| ops::push_force_with_lease(&dir, &expected_head));
                        FlowEvent::LeasePushFinished {
                            result,
                            now_secs: unix_now_secs(),
                        }
                    },
                )
            }
            FlowEffect::PollPr { worktree } => {
                let _ = self.wake_github.try_send(());
                self.dispatch_pr_poll(worktree, worker_tx);
                Vec::new()
            }
            FlowEffect::RunRebase { worktree } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let outcome = match dir {
                            Ok(dir) => ops::run_rebase(&dir),
                            Err(why) => lune_domain::flows::RebaseOutcome::Failed(why),
                        };
                        FlowEvent::RebaseFinished(outcome)
                    },
                )
            }
            FlowEffect::CheckRebasePostConditions { worktree, onto } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let result =
                            dir.and_then(|dir| ops::check_rebase_post_conditions(&dir, &onto));
                        FlowEvent::RebasePostConditionsChecked(result)
                    },
                )
            }
            FlowEffect::KeepChecks { worktree, pr_url } => {
                let generation = self.generation(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || FlowEvent::ChecksFetched(ops::fetch_checks_prompt(&pr_url)),
                )
            }
            FlowEffect::CheckFixChecksPostConditions {
                worktree,
                previous_head,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let result = match dir {
                            Ok(dir) => ops::check_fix_checks_post_conditions(&dir, &previous_head),
                            Err(why) => Err(lune_domain::flows::FixChecksGateFailure::Failed(why)),
                        };
                        FlowEvent::FixChecksPostConditionsChecked(result)
                    },
                )
            }
            FlowEffect::MergePr {
                worktree,
                pr_url,
                merge_method,
                expected_head,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                let wake_github = self.wake_github.clone();
                self.spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Operation,
                    worker_tx,
                    move || {
                        let outcome = match dir {
                            Ok(dir) => ops::merge_pr(&dir, &pr_url, merge_method, &expected_head),
                            Err(why) => lune_domain::flows::MergeOutcome::Failed(why),
                        };
                        let _ = wake_github.try_send(());
                        FlowEvent::MergeFinished {
                            outcome,
                            now_secs: unix_now_secs(),
                        }
                    },
                )
            }
        }
    }

    fn dispatch_pr_poll(&mut self, worktree: WorktreeId, worker_tx: &SyncSender<WorkerDone>) {
        if self.pr_poll_in_flight.contains_key(&worktree) {
            return;
        }
        let Ok(dir) = self.worktree_dir(worktree) else {
            return;
        };
        let Some(slot) = try_acquire_worker(&self.workers_active) else {
            return;
        };
        let generation = self.generation(worktree);
        self.pr_poll_in_flight.insert(worktree, generation);
        self.last_pr_poll.insert(worktree, Instant::now());
        let tx = worker_tx.clone();
        let spawned = thread::Builder::new()
            .name("flow-pr".to_owned())
            .spawn(move || {
                let _slot = slot;
                let polled = panics::contained(|| ops::poll_pr(&dir))
                    .unwrap_or(Err("PR poll panicked".to_owned()));
                let event = match polled {
                    Ok(Some(snapshot)) => Some(FlowEvent::PrSnapshot {
                        snapshot,
                        now_secs: unix_now_secs(),
                    }),
                    Ok(None) => Some(FlowEvent::PrMissing),
                    Err(why) => {
                        log::say(&format!("flow worktree {worktree}: PR poll failed: {why}"));
                        None
                    }
                };
                if tx
                    .send(WorkerDone {
                        worktree,
                        generation,
                        kind: WorkerKind::PrPoll,
                        event,
                    })
                    .is_err()
                {
                    log::say(&format!(
                        "flow worktree {worktree}: flows actor stopped before PR poll finished"
                    ));
                }
            });
        if let Err(error) = spawned {
            self.pr_poll_in_flight.remove(&worktree);
            log::say(&format!(
                "flow worktree {worktree}: could not spawn PR poll worker: {error}"
            ));
        }
    }

    fn open_unattended_step(
        &mut self,
        worktree: WorktreeId,
        view_name: String,
        harness: FlowHarness,
        prompt: StepPrompt,
        timeout_secs: u64,
        worker_tx: &SyncSender<WorkerDone>,
    ) -> Vec<FlowEffect> {
        self.cleanup_step_files(worktree);
        self.steps_opened_count = self.steps_opened_count.wrapping_add(1);
        let stamp = store::step_stamp(std::process::id(), worktree, self.steps_opened_count);
        let pi_session = match harness {
            FlowHarness::Pi => self.store.pi_session_file(&stamp).map(Some),
            FlowHarness::ClaudeCode | FlowHarness::Antigravity => Ok(None),
        };
        let (run_dir, pi_session) =
            match self.store.run_dir().and_then(|dir| Ok((dir, pi_session?))) {
                Ok(found) => found,
                Err(why) => {
                    self.dirty = true;
                    return self
                        .registry
                        .handle(worktree, FlowEvent::ViewOpenFailed { why });
                }
            };
        let files = ops::StepFiles {
            prompt: run_dir.join(format!("prompt-{stamp}.txt")),
            followup: Some(run_dir.join(format!("followup-{stamp}.txt"))),
            pi_session,
            session_out: run_dir.join(format!("session-{stamp}.id")),
            stop: run_dir.join(format!("stop-{stamp}")),
        };
        self.step_files.insert(worktree, files.transient());
        self.session_files
            .insert(worktree, files.session_out.clone());
        self.stop_files.insert(worktree, files.stop.clone());
        let lune_bin = ops::lune_binary();
        let generation = self.generation(worktree);
        let stage_label = view_name.clone();
        let stop = files.stop.clone();
        let argv = move || -> Result<Argv, String> {
            let (prompt, followup) = prompt.texts(harness);
            let written = |path: &PathBuf, text: &str| {
                fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
            };
            written(&files.prompt, &prompt)?;
            let followup_file = match (&files.followup, followup) {
                (Some(path), Some(text)) => {
                    written(path, &text)?;
                    Some(path.clone())
                }
                _ => None,
            };
            let files = ops::StepFiles {
                followup: followup_file,
                ..files
            };
            ops::unattended_step_argv(&lune_bin, harness, &stage_label, &files, timeout_secs)
        };
        let kind = WorkerKind::OpenStep { stop };
        self.spawn_open_terminal(worktree, generation, kind, view_name, argv, worker_tx)
    }

    fn stop_step(&mut self, worktree: WorktreeId, view: ViewId) {
        let Some(stop) = self.stop_files.get(&worktree) else {
            let why = format!(
                "the step in view {view} has no stop file luned knows; it runs until it ends"
            );
            log::say(&format!("flow worktree {worktree}: {why}"));
            self.control_failure = Some(why);
            return;
        };
        if let Err(error) = write_stop(stop) {
            let why = format!("the step in view {view} could not be told to stop ({error})");
            log::say(&format!("flow worktree {worktree}: {why}"));
            self.control_failure = Some(why);
        }
    }

    fn open_take_over(
        &mut self,
        worktree: WorktreeId,
        view_name: String,
        harness: FlowHarness,
        session_id: Option<&SessionId>,
        fallback_view: Option<ViewId>,
        stopped_view: Option<ViewId>,
    ) {
        let focus = match (session_id, fallback_view, stopped_view) {
            (None, Some(view), None) => Some(view),
            _ => None,
        };
        let argv = match ops::takeover_argv(harness, session_id) {
            Ok(argv) => argv,
            Err(why) => {
                self.control_failure = Some(format!("the takeover could not be made: {why}"));
                return;
            }
        };
        let Some(slot) = try_acquire_worker(&self.workers_active) else {
            self.control_failure = Some(format!(
                "the flow paused, but the takeover did not open: worker limit {WORKERS_MAX} reached"
            ));
            return;
        };
        let layouts = self.layouts.clone();
        let spawned = thread::Builder::new()
            .name("flow-takeover".to_owned())
            .spawn(move || {
                let _slot = slot;
                let taken = take_over(&layouts, worktree, view_name, argv, focus, stopped_view);
                if let Err(why) = taken {
                    log::say(&format!("flow takeover for {worktree} failed: {why}"));
                }
            });
        if let Err(error) = spawned {
            self.control_failure = Some(format!(
                "the flow paused, but the takeover did not open: {error}"
            ));
        }
    }

    fn spawn_open_terminal(
        &mut self,
        worktree: WorktreeId,
        generation: u64,
        kind: WorkerKind,
        view_name: String,
        argv: impl FnOnce() -> Result<Argv, String> + Send + 'static,
        worker_tx: &SyncSender<WorkerDone>,
    ) -> Vec<FlowEffect> {
        let layouts = self.layouts.clone();
        self.spawn_worker(worktree, generation, kind, worker_tx, move || {
            let argv = match argv() {
                Ok(argv) => argv,
                Err(why) => return FlowEvent::ViewOpenFailed { why },
            };
            let opened = layouts::ask(&layouts, |answer| layouts::Command::OpenNamedTerminal {
                worktree,
                name: view_name,
                opens: Opens::Command(argv),
                answer,
            });
            match opened {
                Some(Ok(view)) => FlowEvent::ViewOpened { view },
                Some(Err(why)) => FlowEvent::ViewOpenFailed { why },
                None => FlowEvent::ViewOpenFailed {
                    why: "layouts actor stopped".to_owned(),
                },
            }
        })
    }

    fn spawn_worker(
        &mut self,
        worktree: WorktreeId,
        generation: u64,
        kind: WorkerKind,
        tx: &SyncSender<WorkerDone>,
        work: impl FnOnce() -> FlowEvent + Send + 'static,
    ) -> Vec<FlowEffect> {
        let Some(slot) = try_acquire_worker(&self.workers_active) else {
            self.dirty = true;
            return self.registry.handle(
                worktree,
                FlowEvent::WorkerFailed {
                    why: format!("flow worker limit of {WORKERS_MAX} reached"),
                },
            );
        };
        let worker_tx = tx.clone();
        let spawned = thread::Builder::new()
            .name("flow-op".to_owned())
            .spawn(move || {
                let _slot = slot;
                let event = match panics::contained(work) {
                    Ok(event) => event,
                    Err(Panicked) => FlowEvent::WorkerFailed {
                        why: "flow operation panicked".to_owned(),
                    },
                };
                if worker_tx
                    .send(WorkerDone {
                        worktree,
                        generation,
                        kind,
                        event: Some(event),
                    })
                    .is_err()
                {
                    log::say(&format!(
                        "flow worktree {worktree}: flows actor stopped before worker finished"
                    ));
                }
            });
        if let Err(error) = spawned {
            self.dirty = true;
            return self.registry.handle(
                worktree,
                FlowEvent::WorkerFailed {
                    why: format!("could not spawn flow worker: {error}"),
                },
            );
        }
        Vec::new()
    }
}

fn take_over(
    layouts: &SyncSender<layouts::Command>,
    worktree: WorktreeId,
    view_name: String,
    argv: Argv,
    focus: Option<ViewId>,
    stopped_view: Option<ViewId>,
) -> Result<(), String> {
    if let Some(view) = stopped_view {
        let mut ended = false;
        for _ in 0..TAKEOVER_WAIT_POLLS_MAX {
            let running = layouts::ask(layouts, |answer| layouts::Command::StepRunning {
                view,
                answer,
            })
            .ok_or_else(|| "layouts stopped".to_owned())?;
            if !running {
                ended = true;
                break;
            }
            thread::sleep(TAKEOVER_WAIT_POLL);
        }
        if !ended {
            return Err(format!(
                "the step in view {view} did not stop; take over again once it ends"
            ));
        }
    }
    if let Some(view) = focus {
        return layouts::ask(layouts, |answer| layouts::Command::FocusView {
            view,
            answer,
        })
        .ok_or_else(|| "layouts stopped".to_owned())?;
    }
    layouts::ask(layouts, |answer| layouts::Command::OpenNamedTerminal {
        worktree,
        name: view_name,
        opens: Opens::Command(argv),
        answer,
    })
    .ok_or_else(|| "layouts stopped".to_owned())?
    .map(|_| ())
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock reads after 1970")
        .as_secs()
}

#[must_use]
pub fn summary_to_api(summary: &FlowSummary) -> api::FlowSummary {
    assert!(summary.step_number >= 1);
    assert!(summary.step_number <= summary.step_total);
    api::FlowSummary {
        worktree: summary.worktree.get(),
        flow_name: summary.flow_name.clone(),
        mode: summary.mode.to_api().into(),
        harness: summary.harness.harness().to_api().into(),
        step_number: summary.step_number,
        step_total: summary.step_total,
        stage_label: summary.stage_label.clone(),
        badge: summary.badge.clone(),
        badge_kind: summary.badge_kind.to_api().into(),
        pause_reason: summary.pause_reason.map(|reason| reason.to_api().into()),
        detail: summary.detail.clone(),
        active_view: summary.active_view.map(ViewId::get),
        resume_session: summary
            .resume_session
            .as_ref()
            .map(|id| id.as_str().to_owned()),
        controls: summary
            .controls
            .iter()
            .map(|&control| control.to_api().into())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc::Receiver;

    use lune_domain::flows::{FlowSpec, GitOid, LandState, StageKind, StageSpec};

    use super::*;

    struct Harnessed {
        actor: Actor,
        worker_tx: SyncSender<WorkerDone>,
        worker_rx: Receiver<WorkerDone>,
        step_running: Arc<AtomicBool>,
        folder: tempfile::TempDir,
    }

    fn wt(id: u64) -> WorktreeId {
        WorktreeId::of(id).unwrap()
    }

    fn view(id: u64) -> ViewId {
        ViewId::of(id).unwrap()
    }

    fn fake_layouts(step_running: Arc<AtomicBool>) -> SyncSender<layouts::Command> {
        let (send, commands) = mpsc::sync_channel::<layouts::Command>(16);
        thread::spawn(move || {
            let mut next_view = 500;
            for command in commands {
                match command {
                    layouts::Command::StepRunning { answer, .. } => {
                        let _ = answer.send(step_running.load(Ordering::SeqCst));
                    }
                    layouts::Command::OpenNamedTerminal { answer, .. } => {
                        next_view += 1;
                        let _ = answer.send(Ok(view(next_view)));
                    }
                    layouts::Command::FocusView { answer, .. } => {
                        let _ = answer.send(Ok(()));
                    }
                    _ => {}
                }
            }
        });
        send
    }

    fn harnessed(spec: FlowSpec) -> Harnessed {
        let folder = tempfile::tempdir().unwrap();
        let store = FlowStore::at(folder.path());
        let mut config = LoadedFlows::default();
        config.specs.insert(spec.name.clone(), spec);
        let (core, core_rx) = mpsc::sync_channel(1024);
        thread::spawn(move || for _ in core_rx {});
        let (wake_github, _) = mpsc::sync_channel(1);
        let step_running = Arc::new(AtomicBool::new(false));
        let layouts = fake_layouts(Arc::clone(&step_running));
        let mut actor = Actor::new(
            FlowRegistry::default(),
            store,
            config,
            core,
            layouts,
            wake_github,
        );
        actor
            .worktree_dirs
            .insert(wt(1), folder.path().to_string_lossy().into_owned());
        let (worker_tx, worker_rx) = mpsc::sync_channel(WORKER_QUEUE_MAX);
        Harnessed {
            actor,
            worker_tx,
            worker_rx,
            step_running,
            folder,
        }
    }

    fn land_spec() -> FlowSpec {
        let mut spec = FlowSpec::default_spec();
        spec.stages = vec![StageSpec {
            label: "land".to_owned(),
            kind: StageKind::Land,
        }];
        spec
    }

    fn worker_done(h: &Harnessed, event: FlowEvent) -> WorkerDone {
        WorkerDone {
            worktree: wt(1),
            generation: h.actor.generation(wt(1)),
            kind: WorkerKind::Operation,
            event: Some(event),
        }
    }

    impl Harnessed {
        fn start(&mut self, spec: &FlowSpec, mode: FlowMode) {
            self.actor
                .start_flow(
                    wt(1),
                    Some(&spec.name),
                    mode,
                    FlowHarness::ClaudeCode,
                    FlowEntry::PullRequest,
                    &self.worker_tx,
                )
                .unwrap();
        }

        fn phase(&self) -> FlowPhase {
            self.actor.registry.get(wt(1)).unwrap().phase.clone()
        }

        fn next_worker_event(&self) -> FlowEvent {
            let done = lune_waits::heard("a worker reports back", &self.worker_rx);
            done.event.expect("an operation worker carries an event")
        }
    }

    #[test]
    fn a_refused_control_keeps_the_in_flight_result() {
        let spec = land_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        assert_eq!(h.phase(), FlowPhase::Landing(LandState::Pushing));
        let pushed = worker_done(
            &h,
            FlowEvent::PushFinished {
                result: Ok(GitOid::parse(&"1".repeat(40)).unwrap()),
                now_secs: 10,
            },
        );
        let generation = h.actor.generation(wt(1));
        let refused = h
            .actor
            .control_flow(wt(1), FlowControl::Resume, &h.worker_tx);
        assert!(refused.is_err());
        assert_eq!(h.actor.generation(wt(1)), generation);
        h.actor.handle_worker_done(pushed, &h.worker_tx);
        assert!(matches!(
            h.phase(),
            FlowPhase::Landing(LandState::WaitingPr { .. })
        ));
    }

    #[test]
    fn starting_over_an_active_flow_leaves_it_alone() {
        let spec = land_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let generation = h.actor.generation(wt(1));
        let again = h.actor.start_flow(
            wt(1),
            Some(&spec.name),
            FlowMode::Unattended,
            FlowHarness::ClaudeCode,
            FlowEntry::PullRequest,
            &h.worker_tx,
        );
        assert!(again.is_err());
        assert_eq!(h.actor.generation(wt(1)), generation);
        assert_eq!(h.phase(), FlowPhase::Landing(LandState::Pushing));
    }

    #[test]
    fn an_exit_that_beats_its_view_opening_is_applied_once_the_view_opens() {
        let spec = FlowSpec::default_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let opened = h.next_worker_event();
        let FlowEvent::ViewOpened { view: step_view } = opened else {
            panic!("the step's view opens: {opened:?}");
        };
        h.actor.handle_command(
            Command::TerminalExited {
                view: step_view,
                exit: TerminalExit::Code(1),
            },
            &h.worker_tx,
        );
        assert!(matches!(h.phase(), FlowPhase::RunningStage(_)));
        let done = worker_done(&h, FlowEvent::ViewOpened { view: step_view });
        h.actor.handle_worker_done(done, &h.worker_tx);
        let FlowPhase::Paused(paused) = h.phase() else {
            panic!("the failed step pauses the flow: {:?}", h.phase());
        };
        assert_eq!(paused.view, Some(step_view));
    }

    #[test]
    fn resume_waits_for_a_step_that_still_runs_and_pause_asks_it_to_stop() {
        let spec = FlowSpec::default_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let opened = h.next_worker_event();
        let done = worker_done(&h, opened);
        h.actor.handle_worker_done(done, &h.worker_tx);
        let stop = h.actor.stop_files.get(&wt(1)).cloned().unwrap();
        assert!(!stop.exists());

        h.actor
            .control_flow(wt(1), FlowControl::Pause, &h.worker_tx)
            .unwrap();
        assert!(stop.exists(), "pause tells the running step to stop");

        h.step_running.store(true, Ordering::SeqCst);
        let refused = h
            .actor
            .control_flow(wt(1), FlowControl::Resume, &h.worker_tx);
        assert!(refused.unwrap_err().contains("still running"));
        assert!(matches!(h.phase(), FlowPhase::Paused(_)));

        h.step_running.store(false, Ordering::SeqCst);
        h.actor
            .control_flow(wt(1), FlowControl::Resume, &h.worker_tx)
            .unwrap();
        assert!(matches!(h.phase(), FlowPhase::RunningStage(_)));
    }

    #[test]
    fn a_failed_save_stays_dirty_and_is_reported() {
        let spec = land_spec();
        let mut h = harnessed(spec.clone());
        let blocker = h.folder.path().join(store::FILE);
        fs::create_dir_all(blocker.join("in-the-way")).unwrap();
        h.start(&spec, FlowMode::Unattended);
        assert!(h.actor.dirty, "an unsaved change stays dirty for the retry");
        assert!(h.actor.warnings().iter().any(|w| w.contains("not saved")));
        fs::remove_dir_all(&blocker).unwrap();
        h.actor.flush_dirty();
        assert!(!h.actor.dirty);
        assert!(h.actor.warnings().iter().all(|w| !w.contains("not saved")));
    }

    #[test]
    fn an_unreadable_state_file_is_set_aside_not_overwritten() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join(store::FILE);
        fs::write(&file, b"{ not json").unwrap();
        let store = FlowStore::at(folder.path());
        let Loaded {
            registry,
            warning,
            persist_blocked,
        } = loaded(&store);
        assert!(registry.is_empty());
        assert!(!persist_blocked);
        assert!(warning.unwrap().contains("unreadable"));
        let aside = folder
            .path()
            .join(format!("{}.{}", store::FILE, store::UNREADABLE_SUFFIX));
        assert_eq!(fs::read(aside).unwrap(), b"{ not json");
        assert!(!file.exists());
    }

    #[test]
    fn aborting_removes_the_flow_and_its_files() {
        let spec = land_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let summary = h
            .actor
            .control_flow(wt(1), FlowControl::Abort, &h.worker_tx)
            .unwrap();
        assert_eq!(summary, None);
        assert!(h.actor.registry.get(wt(1)).is_none());
        assert!(h.actor.registry.is_empty());
    }

    #[test]
    fn aborting_tells_the_running_step_to_stop() {
        let spec = FlowSpec::default_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let opened = h.next_worker_event();
        let done = worker_done(&h, opened);
        h.actor.handle_worker_done(done, &h.worker_tx);
        let stop = h.actor.stop_files.get(&wt(1)).cloned().unwrap();
        h.actor
            .control_flow(wt(1), FlowControl::Abort, &h.worker_tx)
            .unwrap();
        assert!(h.actor.registry.get(wt(1)).is_none());
        assert!(stop.exists(), "abort leaves the step its stop file");
    }

    #[test]
    fn a_step_that_opens_after_its_pause_is_stopped_and_held() {
        let spec = FlowSpec::default_spec();
        let mut h = harnessed(spec.clone());
        h.start(&spec, FlowMode::Unattended);
        let stop = h.actor.stop_files.get(&wt(1)).cloned().unwrap();
        h.actor
            .control_flow(wt(1), FlowControl::Pause, &h.worker_tx)
            .unwrap();
        let opened = lune_waits::heard("the step's view opens", &h.worker_rx);
        h.actor.handle_worker_done(opened, &h.worker_tx);
        assert!(stop.exists(), "the late step is told to stop");
        let FlowPhase::Paused(paused) = h.phase() else {
            panic!("the flow stays paused: {:?}", h.phase());
        };
        assert!(
            paused.view.is_some(),
            "the pause holds the late step's view"
        );

        h.step_running.store(true, Ordering::SeqCst);
        let refused = h
            .actor
            .control_flow(wt(1), FlowControl::Resume, &h.worker_tx);
        assert!(refused.unwrap_err().contains("still running"));
        h.step_running.store(false, Ordering::SeqCst);
        h.actor
            .control_flow(wt(1), FlowControl::Resume, &h.worker_tx)
            .unwrap();
        assert!(matches!(h.phase(), FlowPhase::RunningStage(_)));
    }

    #[test]
    fn a_pi_step_keeps_its_session_out_of_the_run_folder() {
        let spec = FlowSpec::default_spec();
        let mut h = harnessed(spec.clone());
        h.actor
            .start_flow(
                wt(1),
                Some(&spec.name),
                FlowMode::Unattended,
                FlowHarness::Pi,
                FlowEntry::PullRequest,
                &h.worker_tx,
            )
            .unwrap();
        let run_dir = h.folder.path().join(store::RUN_DIR);
        let files = h.actor.step_files.get(&wt(1)).cloned().unwrap();
        assert!(files.iter().all(|file| file.starts_with(&run_dir)));
        assert!(
            files
                .iter()
                .all(|file| !file.to_string_lossy().contains("pi-")),
            "the pi session is not a step's transient file: {files:?}"
        );
    }
}
