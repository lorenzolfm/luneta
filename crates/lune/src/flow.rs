use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ValueEnum;
use lune_domain::flows;
use lune_domain::projects::WorktreeId;
use luned::api::{
    self, ControlFlowRequest, FlowControl, FlowSummary, ListFlowsRequest, ListFlowsResponse,
};

use crate::agents::{Harness, Kept};
use crate::daemon;
use crate::machines::Machine;
use crate::reach::{self, Luned};
use crate::state;
use crate::step::StepHarness;
use crate::worktrees;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Action {
    Status,
    List,
    Start,
    Advance,
    Auto,
    Pause,
    Resume,
    Retry,
    Skip,
    Takeover,
    Abort,
}

pub struct Args {
    pub action: Option<Action>,
    pub dir: Option<PathBuf>,
    pub prompt: Option<String>,
    pub flow: Option<String>,
    pub interactive: bool,
    pub harness: Option<StepHarness>,
}

pub fn run(args: &Args, folder: &Path) -> ExitCode {
    let action = args.action.unwrap_or(Action::Status);
    if !matches!(action, Action::Start)
        && (args.prompt.is_some()
            || args.flow.is_some()
            || args.interactive
            || args.harness.is_some())
    {
        eprintln!("lune: --prompt, --flow, --interactive and --harness apply to `lune flow start`");
        return ExitCode::FAILURE;
    }
    let tasks = luned::tasks::shared();
    let daemon = match tasks.block_on(daemon::running(folder)) {
        Ok(daemon) => daemon,
        Err(unreached) => {
            eprintln!("lune: {unreached}");
            return unreached.exit_code();
        }
    };
    let result = match action {
        Action::List => list(&tasks, &daemon),
        Action::Status => status(&tasks, &daemon, folder, args.dir.as_deref()),
        Action::Start => start(&tasks, &daemon, folder, args),
        Action::Advance => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::Advance,
        ),
        Action::Auto => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::AutoRest,
        ),
        Action::Pause => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::Pause,
        ),
        Action::Resume => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::Resume,
        ),
        Action::Retry => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::RetryStep,
        ),
        Action::Skip => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::SkipStep,
        ),
        Action::Takeover => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::TakeOver,
        ),
        Action::Abort => control(
            &tasks,
            &daemon,
            folder,
            args.dir.as_deref(),
            FlowControl::Abort,
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("lune: {why}");
            ExitCode::FAILURE
        }
    }
}

pub fn start_on(
    folder: &Path,
    worktree: WorktreeId,
    mode: flows::FlowMode,
    harness: Harness,
    flow_name: Option<String>,
    prompt: Option<String>,
) -> Result<Option<FlowSummary>, String> {
    let request = flows::start_request(worktree, mode, harness, flow_name, prompt);
    Luned::at(folder, &Machine::Local, reach::ANSWER)?.start_flow(request)
}

fn list(tasks: &tokio::runtime::Handle, daemon: &luned::Shared) -> Result<(), String> {
    let response = fetch_list(tasks, daemon)?;
    print_warnings(&response.config_warnings);
    if !response.defined_flows.is_empty() {
        println!("flows: {}", response.defined_flows.join(", "));
    }
    if response.flows.is_empty() {
        println!("no active flows");
        return Ok(());
    }
    for flow in &response.flows {
        print_summary_line(flow);
    }
    Ok(())
}

fn status(
    tasks: &tokio::runtime::Handle,
    daemon: &luned::Shared,
    folder: &Path,
    dir: Option<&Path>,
) -> Result<(), String> {
    let response = fetch_list(tasks, daemon)?;
    print_warnings(&response.config_warnings);
    let (target, found) = worktree_of(folder, dir)?;
    match (found, dir) {
        (Some(worktree), _) => {
            let found = response
                .flows
                .iter()
                .find(|flow| flow.worktree == worktree.get());
            match found {
                Some(flow) => print_summary_detail(flow),
                None => println!("no active flow on worktree {}", worktree.get()),
            }
        }
        (None, None) => {
            if response.flows.is_empty() {
                println!("no active flows");
            }
            for flow in &response.flows {
                print_summary_line(flow);
            }
        }
        (None, Some(_)) => return Err(unkept(&target)),
    }
    Ok(())
}

fn start(
    tasks: &tokio::runtime::Handle,
    daemon: &luned::Shared,
    folder: &Path,
    args: &Args,
) -> Result<(), String> {
    let worktree = resolve_worktree(folder, args.dir.as_deref())?;
    let harness = args.harness.map_or_else(
        || Kept::shared(&state::Folder::of_user()).0.harness(),
        StepHarness::into_harness,
    );
    let mode = if args.interactive {
        flows::FlowMode::Interactive
    } else {
        flows::FlowMode::Unattended
    };
    let request = flows::start_request(
        worktree,
        mode,
        harness,
        args.flow.clone(),
        args.prompt.clone(),
    );
    let response = tasks
        .block_on(daemon.start_flow(luned::ANSWER, request))
        .map_err(|unasked| unasked.said(None))?;
    match response.flow {
        Some(flow) => print_summary_detail(&flow),
        None => println!("flow started on worktree {}", worktree.get()),
    }
    Ok(())
}

fn control(
    tasks: &tokio::runtime::Handle,
    daemon: &luned::Shared,
    folder: &Path,
    dir: Option<&Path>,
    control: FlowControl,
) -> Result<(), String> {
    let worktree = resolve_worktree(folder, dir)?;
    let request = ControlFlowRequest {
        on: None,
        worktree: worktree.get(),
        control: control.into(),
    };
    let response = tasks
        .block_on(daemon.control_flow(luned::ANSWER, request))
        .map_err(|unasked| unasked.said(None))?;
    match response.flow {
        Some(flow) => print_summary_detail(&flow),
        None => println!("flow cleared on worktree {}", worktree.get()),
    }
    Ok(())
}

fn fetch_list(
    tasks: &tokio::runtime::Handle,
    daemon: &luned::Shared,
) -> Result<ListFlowsResponse, String> {
    tasks
        .block_on(daemon.list_flows(luned::ANSWER, ListFlowsRequest { on: None }))
        .map_err(|unasked| unasked.said(None))
}

fn resolve_worktree(folder: &Path, dir: Option<&Path>) -> Result<WorktreeId, String> {
    let (target, found) = worktree_of(folder, dir)?;
    found.ok_or_else(|| unkept(&target))
}

fn unkept(target: &Path) -> String {
    format!(
        "{} is in no worktree luned keeps; start it with `lune start` first",
        target.display()
    )
}

fn worktree_of(folder: &Path, dir: Option<&Path>) -> Result<(PathBuf, Option<WorktreeId>), String> {
    let target = match dir {
        Some(dir) => dir.to_path_buf(),
        None => env::current_dir().map_err(|error| format!("no working directory: {error}"))?,
    };
    let real = worktrees::real(&target)?;
    let projects = worktrees::kept(folder)?;
    let mut best: Option<(usize, WorktreeId)> = None;
    for project in &projects {
        for worktree in project.worktrees() {
            let held = Path::new(worktree.folder.as_str());
            if real.starts_with(held) {
                let depth = worktree.folder.as_str().len();
                if best.is_none_or(|(deepest, _)| depth > deepest) {
                    best = Some((depth, worktree.id));
                }
            }
        }
    }
    Ok((real, best.map(|(_, id)| id)))
}

fn print_warnings(warnings: &[String]) {
    for warning in warnings {
        eprintln!("lune: flow config warning: {warning}");
    }
}

fn print_summary_line(flow: &FlowSummary) {
    let mode = mode_label(flow.mode);
    println!(
        "worktree {}  {} ({mode})  {}",
        flow.worktree, flow.flow_name, flow.badge
    );
    if let Some(detail) = &flow.detail {
        println!("  {detail}");
    }
}

fn print_summary_detail(flow: &FlowSummary) {
    println!("worktree: {}", flow.worktree);
    println!("flow: {} ({})", flow.flow_name, mode_label(flow.mode));
    println!(
        "stage: {}/{} {}",
        flow.step_number, flow.step_total, flow.stage_label
    );
    println!("status: {}", flow.badge);
    if let Some(reason) = &flow.pause_reason {
        println!("pause: {reason}");
    }
    if let Some(detail) = &flow.detail {
        println!("detail: {detail}");
    }
    if let Some(view) = flow.active_view {
        println!("view: {view}");
    }
    if let Some(session) = &flow.resume_session {
        println!("session: {session}");
    }
    let controls: Vec<&str> = flow
        .controls
        .iter()
        .filter_map(|raw| api::FlowControl::try_from(*raw).ok())
        .filter_map(control_name)
        .collect();
    if !controls.is_empty() {
        println!("controls: {}", controls.join(", "));
    }
}

fn mode_label(raw: i32) -> &'static str {
    match flows::FlowMode::of_api(raw) {
        Ok(flows::FlowMode::Interactive) => "interactive",
        Ok(flows::FlowMode::Unattended) => "unattended",
        Err(_) => "unknown mode",
    }
}

fn control_name(control: FlowControl) -> Option<&'static str> {
    match control {
        FlowControl::Advance => Some("advance"),
        FlowControl::AutoRest => Some("auto"),
        FlowControl::Pause => Some("pause"),
        FlowControl::Resume => Some("resume"),
        FlowControl::RetryStep => Some("retry"),
        FlowControl::SkipStep => Some("skip"),
        FlowControl::TakeOver => Some("takeover"),
        FlowControl::Abort => Some("abort"),
        FlowControl::Unspecified => None,
    }
}
