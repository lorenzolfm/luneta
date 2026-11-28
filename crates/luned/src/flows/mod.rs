pub mod config;
pub mod ops;
pub mod store;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use lune_domain::agent::Harness;
use lune_domain::flows::{
    DEFAULT_FLOW_NAME, FlowBadgeKind, FlowControl, FlowEffect, FlowEntry, FlowEvent, FlowMode,
    FlowRegistry, FlowRun, FlowSummary,
};
use lune_domain::projects::{Opens, ViewId, WorktreeId};
use tokio::sync::oneshot;

use crate::api;
use crate::core;
use crate::git;
use crate::layouts;
use crate::log;
use crate::panics::{self, Panicked};

use self::config::LoadedFlows;
use self::store::FlowStore;

const COMMANDS_MAX: usize = 64;

const FLOW_SWEEP: Duration = Duration::from_secs(2);

const LAND_POLL_INTERVAL: Duration = Duration::from_secs(10);

const _: () = assert!(COMMANDS_MAX > 0);
const _: () = assert!(FLOW_SWEEP.as_secs() > 0);
const _: () = assert!(LAND_POLL_INTERVAL.as_secs() >= FLOW_SWEEP.as_secs());

pub enum Command {
    Start {
        worktree: WorktreeId,
        dir: String,
        flow_name: Option<String>,
        mode: FlowMode,
        harness: Harness,
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
        exit: lune_domain::flows::TerminalExit,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    PrPoll,
    Other,
}

pub fn ask<T>(
    flows: &SyncSender<Command>,
    question: impl FnOnce(oneshot::Sender<T>) -> Command,
) -> Option<T> {
    let (answer, answered) = oneshot::channel();
    flows.send(question(answer)).ok()?;
    answered.blocking_recv().ok()
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
    for warning in &loaded_config.warnings {
        log::say(&format!("flows config: {warning}"));
    }
    let mut registry = match store.load() {
        Ok(registry) => registry,
        Err(why) => {
            log::say(&format!("flows state reset: {why}"));
            FlowRegistry::default()
        }
    };
    let restart_effects = registry.on_daemon_restarted();
    if !restart_effects.is_empty() {
        if let Err(why) = store.save(&registry) {
            log::say(&format!("flows state not saved after restart: {why}"));
        }
        for effect in restart_effects {
            if let FlowEffect::Alert {
                worktree,
                title,
                detail,
                ..
            } = effect
            {
                log::say(&format!("flow worktree {worktree}: {title} — {detail}"));
            }
        }
    }
    let actor = Actor {
        registry,
        store,
        config: loaded_config,
        core,
        layouts,
        wake_github,
        worktree_dirs: BTreeMap::new(),
        generations: BTreeMap::new(),
        step_counter: 0,
        session_files: BTreeMap::new(),
        pr_poll_in_flight: BTreeSet::new(),
        last_pr_poll: BTreeMap::new(),
        told_summaries: None,
    };
    thread::Builder::new()
        .name("flows".to_owned())
        .spawn(move || run(actor, &commands))?;
    Ok(send)
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
    step_counter: u64,
    session_files: BTreeMap<WorktreeId, PathBuf>,
    pr_poll_in_flight: BTreeSet<WorktreeId>,
    last_pr_poll: BTreeMap<WorktreeId, Instant>,
    told_summaries: Option<BTreeMap<WorktreeId, api::FlowSummary>>,
}

fn run(mut actor: Actor, commands: &Receiver<Command>) {
    let (worker_tx, worker_rx) = mpsc::channel::<WorkerDone>();
    actor.tell_core();
    let mut last_sweep = Instant::now();
    loop {
        while let Ok(done) = worker_rx.try_recv() {
            actor.handle_worker_done(done, &worker_tx);
        }
        let wait = FLOW_SWEEP.saturating_sub(last_sweep.elapsed());
        match commands.recv_timeout(wait) {
            Ok(command) => actor.handle_command(command, &worker_tx),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                let _ = actor.store.save(&actor.registry);
                return;
            }
        }
        while let Ok(done) = worker_rx.try_recv() {
            actor.handle_worker_done(done, &worker_tx);
        }
        if last_sweep.elapsed() >= FLOW_SWEEP {
            actor.sweep(&worker_tx);
            last_sweep = Instant::now();
        }
        actor.tell_core();
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

    fn persist(&self) {
        if let Err(why) = self.store.save(&self.registry) {
            log::say(&format!("flows state not saved: {why}"));
        }
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

    fn handle_command(&mut self, command: Command, worker_tx: &mpsc::Sender<WorkerDone>) {
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
                self.worktree_dirs.insert(worktree, dir);
                let name = flow_name.as_deref().unwrap_or(DEFAULT_FLOW_NAME);
                let result = match self.config.get(name).cloned() {
                    None => Err(format!("unknown flow {name:?}")),
                    Some(spec) => {
                        self.bump_generation(worktree);
                        match self.registry.start(worktree, spec, mode, harness, entry) {
                            Ok(effects) => {
                                self.persist();
                                self.dispatch(effects, worker_tx);
                                self.registry
                                    .get(worktree)
                                    .map(|run| summary_to_api(&run.summary()))
                                    .ok_or_else(|| "flow ended immediately on start".to_owned())
                            }
                            Err(why) => Err(why),
                        }
                    }
                };
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
                if self.registry.get(worktree).is_none() {
                    let _ = answer.send(Err("worktree has no active flow".to_owned()));
                    return;
                }
                if !matches!(control, FlowControl::TakeOver) {
                    self.bump_generation(worktree);
                }
                self.read_session_file_for(worktree);
                let effects = self.registry.handle(worktree, FlowEvent::Control(control));
                if matches!(control, FlowControl::Abort) {
                    self.session_files.remove(&worktree);
                    self.last_pr_poll.remove(&worktree);
                }
                self.persist();
                self.dispatch(effects, worker_tx);
                let summary = self
                    .registry
                    .get(worktree)
                    .map(|run| summary_to_api(&run.summary()));
                let _ = answer.send(Ok(summary));
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
                    config_warnings: self.config.warnings.clone(),
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
                    return;
                };
                self.read_session_file_for(worktree);
                let effects = self
                    .registry
                    .handle(worktree, FlowEvent::TerminalExited { view, exit });
                self.persist();
                self.dispatch(effects, worker_tx);
            }
            Command::WorktreesKnown(known) => {
                let before = self.registry.summaries().len();
                self.worktree_dirs = known;
                let known_dirs = &self.worktree_dirs;
                self.registry
                    .retain_worktrees(|id| known_dirs.contains_key(&id));
                self.session_files
                    .retain(|id, _| known_dirs.contains_key(id));
                self.last_pr_poll
                    .retain(|id, _| known_dirs.contains_key(id));
                self.pr_poll_in_flight
                    .retain(|id| known_dirs.contains_key(id));
                if self.registry.summaries().len() != before {
                    self.persist();
                }
            }
            Command::AgentSessions(sessions) => {
                let mut changed = false;
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
                            .and_then(|run| run.resume_session())
                            != Some(session_id.as_str())
                    {
                        let _ = self.registry.handle(
                            worktree,
                            FlowEvent::SessionCaptured {
                                view,
                                session_id: session_id.clone(),
                            },
                        );
                        changed = true;
                    }
                }
                if changed {
                    self.persist();
                }
            }
        }
    }

    fn handle_worker_done(&mut self, done: WorkerDone, worker_tx: &mpsc::Sender<WorkerDone>) {
        if done.kind == WorkerKind::PrPoll {
            self.pr_poll_in_flight.remove(&done.worktree);
        }
        if done.generation != self.generation(done.worktree) {
            return;
        }
        let Some(event) = done.event else {
            return;
        };
        let effects = self.registry.handle(done.worktree, event);
        self.persist();
        self.dispatch(effects, worker_tx);
    }

    fn sweep(&mut self, worker_tx: &mpsc::Sender<WorkerDone>) {
        let worktrees: Vec<WorktreeId> = self.registry.iter().map(|(id, _)| id).collect();
        for worktree in worktrees {
            self.read_session_file_for(worktree);
            let needs_poll = self
                .registry
                .get(worktree)
                .is_some_and(FlowRun::expects_pr_snapshots);
            if needs_poll
                && !self.pr_poll_in_flight.contains(&worktree)
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
        let Some(path) = self.session_files.get(&worktree) else {
            return;
        };
        let Some(session_id) = store::read_session_out(path) else {
            return;
        };
        let Some(view) = self.registry.get(worktree).and_then(FlowRun::active_view) else {
            return;
        };
        if self
            .registry
            .get(worktree)
            .and_then(FlowRun::resume_session)
            == Some(session_id.as_str())
        {
            return;
        }
        let _ = self
            .registry
            .handle(worktree, FlowEvent::SessionCaptured { view, session_id });
        self.persist();
    }

    fn worktree_dir(&self, worktree: WorktreeId) -> Result<String, String> {
        self.worktree_dirs
            .get(&worktree)
            .cloned()
            .ok_or_else(|| format!("no worktree has the id {worktree}"))
    }

    fn dispatch(&mut self, effects: Vec<FlowEffect>, worker_tx: &mpsc::Sender<WorkerDone>) {
        for effect in effects {
            self.dispatch_one(effect, worker_tx);
        }
    }

    fn dispatch_one(&mut self, effect: FlowEffect, worker_tx: &mpsc::Sender<WorkerDone>) {
        match effect {
            FlowEffect::Alert {
                worktree,
                title,
                detail,
                ..
            } => {
                log::say(&format!("flow worktree {worktree}: {title} — {detail}"));
            }
            FlowEffect::OpenInteractiveStage {
                worktree,
                view_name,
                harness,
                stage_kind,
                implement_prompt,
            } => {
                let pr_url = self
                    .registry
                    .get(worktree)
                    .and_then(|run| run.last_pr_url.clone());
                let dir = match self.worktree_dir(worktree) {
                    Ok(dir) => dir,
                    Err(why) => {
                        let more = self
                            .registry
                            .handle(worktree, FlowEvent::ViewOpenFailed { why });
                        self.persist();
                        self.dispatch(more, worker_tx);
                        return;
                    }
                };
                let argv = ops::interactive_stage_argv(
                    &dir,
                    harness,
                    &stage_kind,
                    implement_prompt.as_deref(),
                    pr_url.as_deref(),
                );
                let generation = self.generation(worktree);
                self.spawn_open_terminal(worktree, generation, view_name, argv, worker_tx);
            }
            FlowEffect::OpenUnattendedStage {
                worktree,
                view_name,
                harness,
                stage_kind,
                implement_prompt,
                timeout_secs,
            } => {
                let pr_url = self
                    .registry
                    .get(worktree)
                    .and_then(|run| run.last_pr_url.clone());
                let dir = match self.worktree_dir(worktree) {
                    Ok(dir) => dir,
                    Err(why) => {
                        let more = self
                            .registry
                            .handle(worktree, FlowEvent::ViewOpenFailed { why });
                        self.persist();
                        self.dispatch(more, worker_tx);
                        return;
                    }
                };
                let (prompt, followup) = ops::stage_prompts(
                    &dir,
                    harness,
                    &stage_kind,
                    implement_prompt.as_deref(),
                    pr_url.as_deref(),
                );
                self.open_unattended_step(
                    worktree,
                    view_name,
                    harness,
                    &prompt,
                    followup.as_deref(),
                    timeout_secs,
                    worker_tx,
                );
            }
            FlowEffect::OpenUnattendedHelper {
                worktree,
                view_name,
                harness,
                prompt,
                timeout_secs,
            } => {
                self.open_unattended_step(
                    worktree,
                    view_name,
                    harness,
                    &prompt,
                    None,
                    timeout_secs,
                    worker_tx,
                );
            }
            FlowEffect::OpenTakeOver {
                worktree,
                view_name,
                harness,
                session_id,
                fallback_view,
            } => {
                if session_id.is_none()
                    && let Some(view) = fallback_view
                {
                    let layouts = self.layouts.clone();
                    thread::spawn(move || {
                        let _ = layouts::ask(&layouts, |answer| layouts::Command::FocusView {
                            view,
                            answer,
                        });
                    });
                    return;
                }
                let argv = match ops::takeover_argv(harness, session_id.as_deref()) {
                    Ok(argv) => argv,
                    Err(why) => {
                        log::say(&format!("flow takeover for {worktree} failed: {why}"));
                        return;
                    }
                };
                let layouts = self.layouts.clone();
                thread::spawn(move || {
                    let _ = layouts::ask(&layouts, |answer| layouts::Command::OpenNamedTerminal {
                        worktree,
                        name: view_name,
                        opens: Opens::Command(argv),
                        answer,
                    });
                });
            }
            FlowEffect::CheckStagePostConditions {
                worktree,
                require_open_pr,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let result = match dir {
                            Ok(dir) => ops::check_stage_post_conditions(&dir, require_open_pr),
                            Err(why) => Err(lune_domain::flows::StageGateFailure::Failed(why)),
                        };
                        FlowEvent::StagePostConditionsChecked(result)
                    },
                );
            }
            FlowEffect::PushIfAhead { worktree } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let result = dir.and_then(|dir| git::push_if_ahead(&dir));
                        FlowEvent::PushFinished {
                            result,
                            now_secs: unix_now_secs(),
                        }
                    },
                );
            }
            FlowEffect::PushForceWithLease { worktree } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let result = dir.and_then(|dir| git::push_force_with_lease(&dir));
                        FlowEvent::PushFinished {
                            result,
                            now_secs: unix_now_secs(),
                        }
                    },
                );
            }
            FlowEffect::PollPr { worktree } => {
                let _ = self.wake_github.try_send(());
                self.dispatch_pr_poll(worktree, worker_tx);
            }
            FlowEffect::RunRebase { worktree } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let outcome = match dir {
                            Ok(dir) => ops::run_rebase(&dir),
                            Err(why) => lune_domain::flows::RebaseOutcome::Failed(why),
                        };
                        FlowEvent::RebaseFinished(outcome)
                    },
                );
            }
            FlowEffect::CheckRebasePostConditions { worktree, onto } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let result =
                            dir.and_then(|dir| ops::check_rebase_post_conditions(&dir, &onto));
                        FlowEvent::RebasePostConditionsChecked(result)
                    },
                );
            }
            FlowEffect::KeepChecks { worktree, pr_url } => {
                let generation = self.generation(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || FlowEvent::ChecksFetched(ops::fetch_checks_prompt(&pr_url)),
                );
            }
            FlowEffect::CheckFixChecksPostConditions {
                worktree,
                previous_head,
            } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let result = match dir {
                            Ok(dir) => ops::check_fix_checks_post_conditions(&dir, &previous_head),
                            Err(why) => Err(lune_domain::flows::FixChecksGateFailure::Failed(why)),
                        };
                        FlowEvent::FixChecksPostConditionsChecked(result)
                    },
                );
            }
            FlowEffect::MergePr { worktree, pr_url } => {
                let generation = self.generation(worktree);
                let dir = self.worktree_dir(worktree);
                let wake_github = self.wake_github.clone();
                spawn_worker(
                    worktree,
                    generation,
                    WorkerKind::Other,
                    worker_tx,
                    move || {
                        let outcome = match dir {
                            Ok(dir) => ops::merge_pr(&dir, &pr_url),
                            Err(why) => lune_domain::flows::MergeOutcome::Failed(why),
                        };
                        let _ = wake_github.try_send(());
                        FlowEvent::MergeFinished {
                            outcome,
                            now_secs: unix_now_secs(),
                        }
                    },
                );
            }
        }
    }

    fn dispatch_pr_poll(&mut self, worktree: WorktreeId, worker_tx: &mpsc::Sender<WorkerDone>) {
        if self.pr_poll_in_flight.contains(&worktree) {
            return;
        }
        let Ok(dir) = self.worktree_dir(worktree) else {
            return;
        };
        let generation = self.generation(worktree);
        self.pr_poll_in_flight.insert(worktree);
        self.last_pr_poll.insert(worktree, Instant::now());
        let tx = worker_tx.clone();
        let _ = thread::Builder::new()
            .name("flow-pr".to_owned())
            .spawn(move || {
                let polled = panics::contained(|| ops::poll_pr(&dir))
                    .unwrap_or(Err("PR poll panicked".to_owned()));
                match polled {
                    Ok(Some(snapshot)) => {
                        let _ = tx.send(WorkerDone {
                            worktree,
                            generation,
                            kind: WorkerKind::PrPoll,
                            event: Some(FlowEvent::PrSnapshot {
                                snapshot,
                                now_secs: unix_now_secs(),
                            }),
                        });
                    }
                    Ok(None) => {
                        let _ = tx.send(WorkerDone {
                            worktree,
                            generation,
                            kind: WorkerKind::PrPoll,
                            event: None,
                        });
                    }
                    Err(why) => {
                        log::say(&format!("flow worktree {worktree}: PR poll failed: {why}"));
                        let _ = tx.send(WorkerDone {
                            worktree,
                            generation,
                            kind: WorkerKind::PrPoll,
                            event: None,
                        });
                    }
                }
            });
    }

    fn open_unattended_step(
        &mut self,
        worktree: WorktreeId,
        view_name: String,
        harness: Harness,
        prompt: &str,
        followup: Option<&str>,
        timeout_secs: u64,
        worker_tx: &mpsc::Sender<WorkerDone>,
    ) {
        self.step_counter = self.step_counter.wrapping_add(1);
        let stamp = format!(
            "{}-{}-{}",
            std::process::id(),
            worktree.get(),
            self.step_counter
        );
        let run_dir = match self.store.run_dir() {
            Ok(dir) => dir,
            Err(why) => {
                let more = self
                    .registry
                    .handle(worktree, FlowEvent::ViewOpenFailed { why });
                self.persist();
                self.dispatch(more, worker_tx);
                return;
            }
        };
        let prompt_file = run_dir.join(format!("prompt-{stamp}.txt"));
        if let Err(error) = fs::write(&prompt_file, prompt) {
            let why = format!("{}: {error}", prompt_file.display());
            let more = self
                .registry
                .handle(worktree, FlowEvent::ViewOpenFailed { why });
            self.persist();
            self.dispatch(more, worker_tx);
            return;
        }
        let followup_file = if let Some(followup) = followup {
            let path = run_dir.join(format!("followup-{stamp}.txt"));
            if let Err(error) = fs::write(&path, followup) {
                let why = format!("{}: {error}", path.display());
                let more = self
                    .registry
                    .handle(worktree, FlowEvent::ViewOpenFailed { why });
                self.persist();
                self.dispatch(more, worker_tx);
                return;
            }
            Some(path)
        } else {
            None
        };
        let session_out = run_dir.join(format!("session-{stamp}.id"));
        let pi_session =
            (harness == Harness::Pi).then(|| run_dir.join(format!("pi-{stamp}.jsonl")));
        self.session_files.insert(worktree, session_out.clone());
        let lune_bin = ops::lune_binary();
        let argv = ops::unattended_step_argv(
            &lune_bin,
            harness,
            &view_name,
            &prompt_file,
            followup_file.as_deref(),
            pi_session.as_deref(),
            &session_out,
            timeout_secs,
        );
        let generation = self.generation(worktree);
        self.spawn_open_terminal(worktree, generation, view_name, argv, worker_tx);
    }

    fn spawn_open_terminal(
        &self,
        worktree: WorktreeId,
        generation: u64,
        view_name: String,
        argv: Result<lune_domain::projects::Argv, String>,
        worker_tx: &mpsc::Sender<WorkerDone>,
    ) {
        let layouts = self.layouts.clone();
        spawn_worker(
            worktree,
            generation,
            WorkerKind::Other,
            worker_tx,
            move || {
                let argv = match argv {
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
            },
        );
    }
}

fn spawn_worker(
    worktree: WorktreeId,
    generation: u64,
    kind: WorkerKind,
    tx: &mpsc::Sender<WorkerDone>,
    work: impl FnOnce() -> FlowEvent + Send + 'static,
) {
    let worker_tx = tx.clone();
    let spawned = thread::Builder::new()
        .name("flow-op".to_owned())
        .spawn(move || {
            let event = match panics::contained(work) {
                Ok(event) => event,
                Err(Panicked) => FlowEvent::ViewOpenFailed {
                    why: "flow operation panicked".to_owned(),
                },
            };
            let _ = worker_tx.send(WorkerDone {
                worktree,
                generation,
                kind,
                event: Some(event),
            });
        });
    if let Err(error) = spawned {
        let _ = tx.send(WorkerDone {
            worktree,
            generation,
            kind,
            event: Some(FlowEvent::ViewOpenFailed {
                why: format!("could not spawn flow worker: {error}"),
            }),
        });
    }
}

fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[must_use]
pub fn mode_to_api(mode: FlowMode) -> api::FlowMode {
    match mode {
        FlowMode::Interactive => api::FlowMode::Interactive,
        FlowMode::Unattended => api::FlowMode::Unattended,
    }
}

pub fn mode_of_api(raw: i32) -> Result<FlowMode, String> {
    match api::FlowMode::try_from(raw) {
        Ok(api::FlowMode::Interactive) => Ok(FlowMode::Interactive),
        Ok(api::FlowMode::Unattended) => Ok(FlowMode::Unattended),
        Ok(api::FlowMode::Unspecified) | Err(_) => {
            Err("flow mode must be interactive or unattended".to_owned())
        }
    }
}

#[must_use]
pub fn control_to_api(control: FlowControl) -> api::FlowControl {
    match control {
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

pub fn control_of_api(raw: i32) -> Result<FlowControl, String> {
    match api::FlowControl::try_from(raw) {
        Ok(api::FlowControl::Advance) => Ok(FlowControl::Advance),
        Ok(api::FlowControl::AutoRest) => Ok(FlowControl::AutoRest),
        Ok(api::FlowControl::Pause) => Ok(FlowControl::Pause),
        Ok(api::FlowControl::Resume) => Ok(FlowControl::Resume),
        Ok(api::FlowControl::RetryStep) => Ok(FlowControl::RetryStep),
        Ok(api::FlowControl::SkipStep) => Ok(FlowControl::SkipStep),
        Ok(api::FlowControl::TakeOver) => Ok(FlowControl::TakeOver),
        Ok(api::FlowControl::Abort) => Ok(FlowControl::Abort),
        Ok(api::FlowControl::Unspecified) | Err(_) => Err("unknown flow control".to_owned()),
    }
}

#[must_use]
pub fn badge_kind_to_api(kind: FlowBadgeKind) -> api::FlowBadgeKind {
    match kind {
        FlowBadgeKind::Running => api::FlowBadgeKind::Running,
        FlowBadgeKind::WaitingInteractive => api::FlowBadgeKind::WaitingInteractive,
        FlowBadgeKind::Paused => api::FlowBadgeKind::Paused,
        FlowBadgeKind::Merged => api::FlowBadgeKind::Merged,
        FlowBadgeKind::ReadyToMerge => api::FlowBadgeKind::ReadyToMerge,
    }
}

#[must_use]
pub fn summary_to_api(summary: &FlowSummary) -> api::FlowSummary {
    api::FlowSummary {
        worktree: summary.worktree.get(),
        flow_name: summary.flow_name.clone(),
        mode: mode_to_api(summary.mode).into(),
        harness: summary.harness.to_api().into(),
        step_index: summary.step_index,
        step_total: summary.step_total,
        stage_label: summary.stage_label.clone(),
        badge: summary.badge.clone(),
        badge_kind: badge_kind_to_api(summary.badge_kind).into(),
        pause_reason: summary.pause_reason.map(|reason| reason.label().to_owned()),
        detail: summary.detail.clone(),
        active_view: summary.active_view.map(ViewId::get),
        resume_session: summary.resume_session.clone(),
        controls: summary
            .controls
            .iter()
            .map(|&control| control_to_api(control).into())
            .collect(),
    }
}
