use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::ValueEnum;
use lune_domain::projects::WorktreeId;
use luned::api::{
    self, ControlFlowRequest, FlowControl, FlowMode, FlowSummary, ListFlowsRequest,
    ListFlowsResponse, StartFlowRequest, start_flow_request,
};

use crate::agents::{Harness, Kept};
use crate::daemon;
use crate::machines::Machine;
use crate::reach::{self, Luned};
use crate::state;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HarnessArg {
    Claude,
    Pi,
    Agy,
}

impl HarnessArg {
    fn to_harness(self) -> Harness {
        match self {
            HarnessArg::Claude => Harness::ClaudeCode,
            HarnessArg::Pi => Harness::Pi,
            HarnessArg::Agy => Harness::Antigravity,
        }
    }
}

pub struct Args {
    pub action: Option<Action>,
    pub dir: Option<PathBuf>,
    pub prompt: Option<String>,
    pub flow: Option<String>,
    pub interactive: bool,
    pub harness: Option<HarnessArg>,
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
    mode: FlowMode,
    harness: Harness,
    flow_name: Option<String>,
    prompt: Option<String>,
) -> Result<Option<FlowSummary>, String> {
    let entry = match prompt {
        Some(prompt) => start_flow_request::Entry::ImplementPrompt(prompt),
        None => start_flow_request::Entry::PullRequest(()),
    };
    let request = StartFlowRequest {
        on: None,
        worktree: worktree.get(),
        flow_name,
        mode: mode.into(),
        harness: harness.to_api().into(),
        entry: Some(entry),
    };
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
    match resolve_worktree(folder, dir) {
        Ok(worktree) => {
            let found = response
                .flows
                .iter()
                .find(|flow| flow.worktree == worktree.get());
            match found {
                Some(flow) => print_summary_detail(flow),
                None => println!("no active flow on worktree {}", worktree.get()),
            }
            Ok(())
        }
        Err(_) if dir.is_none() => {
            if response.flows.is_empty() {
                println!("no active flows");
            } else {
                for flow in &response.flows {
                    print_summary_line(flow);
                }
            }
            Ok(())
        }
        Err(why) => Err(why),
    }
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
        HarnessArg::to_harness,
    );
    let mode = if args.interactive {
        FlowMode::Interactive
    } else {
        FlowMode::Unattended
    };
    let entry = match &args.prompt {
        Some(prompt) => start_flow_request::Entry::ImplementPrompt(prompt.clone()),
        None => start_flow_request::Entry::PullRequest(()),
    };
    let request = StartFlowRequest {
        on: None,
        worktree: worktree.get(),
        flow_name: args.flow.clone(),
        mode: mode.into(),
        harness: harness.to_api().into(),
        entry: Some(entry),
    };
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
    let target = match dir {
        Some(dir) => dir.to_path_buf(),
        None => env::current_dir().map_err(|error| format!("no working directory: {error}"))?,
    };
    let real = worktrees::real(&target)?;
    let projects = worktrees::kept(folder)?;
    let mut best: Option<(usize, WorktreeId)> = None;
    for project in &projects {
        for worktree in project.worktrees() {
            let wt_path = Path::new(worktree.folder.as_str());
            if real.starts_with(wt_path) {
                let len = worktree.folder.as_str().len();
                if best.is_none_or(|(best_len, _)| len > best_len) {
                    best = Some((len, worktree.id));
                }
            }
        }
    }
    best.map(|(_, id)| id).ok_or_else(|| {
        format!(
            "{} is in no worktree luned keeps; start it with `lune start` first",
            real.display()
        )
    })
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
        flow.step_index, flow.step_total, flow.stage_label
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
    match FlowMode::try_from(raw) {
        Ok(FlowMode::Interactive) => "interactive",
        Ok(FlowMode::Unattended | FlowMode::Unspecified) | Err(_) => "unattended",
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
