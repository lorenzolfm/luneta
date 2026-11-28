use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _, Write};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::{ArgGroup, Args, ValueEnum};
use lune_domain::agent::Harness;
use lune_domain::flows::SESSION_ID_BYTES_MAX;
use rustix::io::Errno;
use rustix::process::{Pid, Signal};
use serde_json::Value;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

pub const TIMEOUT_SECS_DEFAULT: u64 = 1800;

pub const TIMEOUT_SECS_MIN: u64 = 1;

pub const TIMEOUT_SECS_MAX: u64 = 4 * 3600;

pub const ACTING_DIVIDER: &str = "─── Acting on findings ───";

const LINE_BYTES_MAX: usize = 8 * 1024 * 1024;

const LINE_QUEUE_MAX: usize = 32;

const QUEUED_BYTES_MAX: usize = 256 * 1024 * 1024;

const TURN_BYTES_MAX: u64 = 1024 * 1024 * 1024;

const LINES_PER_TURN_MAX: usize = 4 * 1024 * 1024;

const AGY_TOOL_STEPS_MAX: usize = 65_536;

const CONTENT_BLOCKS_MAX: usize = 1024;

const EVENTS_PER_LINE_MAX: usize = CONTENT_BLOCKS_MAX + 1;

const AGY_EVENTS_PER_LINE_MAX: usize = 3;

const PROMPT_BYTES_MAX: usize = 1024 * 1024;

const STDERR_BYTES_MAX: usize = 64 * 1024;

const STDERR_DRAIN_CHUNK_BYTES: usize = 4096;

const STDERR_DRAIN_CHUNKS_MAX: u64 = TURN_BYTES_MAX / STDERR_DRAIN_CHUNK_BYTES as u64;

const DETAIL_CHARS_MAX: usize = 120;

const OSC_TITLE_BYTES_MAX: usize = 128;

const OSC_MESSAGE_BYTES_MAX: usize = 256;

const CHILD_WAIT_POLL_MS: u64 = 50;

const CHILD_WAIT_MAX_MS: u64 = 5_000;

const CHILD_WAIT_POLL: Duration = Duration::from_millis(CHILD_WAIT_POLL_MS);

const CHILD_WAIT_MAX: Duration = Duration::from_millis(CHILD_WAIT_MAX_MS);

const CHILD_WAIT_POLLS_MAX: u64 = CHILD_WAIT_MAX_MS.div_ceil(CHILD_WAIT_POLL_MS) + 1;

const STOP_POLL_MS: u64 = 250;

const STOP_POLL: Duration = Duration::from_millis(STOP_POLL_MS);

const STOP_POLLS_PER_STEP_MAX: usize =
    (TIMEOUT_SECS_MAX * 1000).div_ceil(STOP_POLL_MS) as usize + 1;

const _: () = assert!(TIMEOUT_SECS_MIN <= TIMEOUT_SECS_DEFAULT);
const _: () = assert!(TIMEOUT_SECS_DEFAULT <= TIMEOUT_SECS_MAX);
const _: () = assert!(LINE_BYTES_MAX > 0);
const _: () = assert!(LINE_QUEUE_MAX > 0);
const _: () = assert!(LINE_QUEUE_MAX * LINE_BYTES_MAX <= QUEUED_BYTES_MAX);
const _: () = assert!(LINE_BYTES_MAX as u64 <= TURN_BYTES_MAX);
const _: () = assert!(LINES_PER_TURN_MAX > 0);
const _: () = assert!(AGY_TOOL_STEPS_MAX > 0);
const _: () = assert!(AGY_TOOL_STEPS_MAX <= LINES_PER_TURN_MAX);
const _: () = assert!(CONTENT_BLOCKS_MAX > 0);
const _: () = assert!(EVENTS_PER_LINE_MAX > CONTENT_BLOCKS_MAX);
const _: () = assert!(AGY_EVENTS_PER_LINE_MAX > 0);
const _: () = assert!(AGY_EVENTS_PER_LINE_MAX <= EVENTS_PER_LINE_MAX);
const _: () = assert!(PROMPT_BYTES_MAX > 0);
const _: () = assert!(STDERR_BYTES_MAX > 0);
const _: () = assert!(STDERR_DRAIN_CHUNK_BYTES > 0);
const _: () = assert!(STDERR_DRAIN_CHUNKS_MAX > 0);
const _: () = assert!(TURN_BYTES_MAX.is_multiple_of(STDERR_DRAIN_CHUNK_BYTES as u64));
const _: () = assert!(DETAIL_CHARS_MAX > 0);
const _: () = assert!(OSC_TITLE_BYTES_MAX > 0);
const _: () = assert!(OSC_MESSAGE_BYTES_MAX > 0);
const _: () = assert!(CHILD_WAIT_POLL_MS > 0);
const _: () = assert!(CHILD_WAIT_POLLS_MAX > 0);
const _: () = assert!(STOP_POLL_MS > 0);
const _: () = assert!(STOP_POLLS_PER_STEP_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StepHarness {
    Claude,
    Pi,
    Agy,
}

impl StepHarness {
    #[must_use]
    pub const fn into_harness(self) -> Harness {
        match self {
            StepHarness::Claude => Harness::ClaudeCode,
            StepHarness::Pi => Harness::Pi,
            StepHarness::Agy => Harness::Antigravity,
        }
    }
}

#[derive(Args, Clone, Debug)]
#[command(group(
    ArgGroup::new("prompt_source")
        .required(true)
        .args(["prompt", "prompt_file"])
))]
#[command(group(
    ArgGroup::new("followup_source")
        .required(false)
        .args(["followup", "followup_file"])
))]
pub struct StepArgs {
    #[arg(long, value_enum)]
    pub harness: StepHarness,
    #[arg(long)]
    pub stage: String,
    #[arg(long)]
    pub prompt: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub followup: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub followup_file: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub pi_session: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub session_out: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub stop_file: Option<PathBuf>,
    #[arg(long, default_value_t = TIMEOUT_SECS_DEFAULT)]
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Succeeded(Option<String>),
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StreamEvent {
    SessionId(String),
    TextDelta(String),
    TextBlock(String),
    ToolUse {
        name: String,
        detail: Option<String>,
    },
    ToolResult {
        name: Option<String>,
        is_error: bool,
    },
    TurnFinished(Verdict),
    Retried,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Turn {
    session_id: Option<String>,
    summary: Option<String>,
}

enum LineItem {
    Line(String),
    Eof,
    Error(io::Error),
}

struct Stopping {
    file: Option<PathBuf>,
    signalled: Arc<AtomicBool>,
}

impl Stopping {
    fn requested(&self) -> Option<String> {
        if self.signalled.load(Ordering::SeqCst) {
            return Some("the step was interrupted by a signal".to_owned());
        }
        let file = self.file.as_ref()?;
        if !file.exists() {
            return None;
        }
        if let Err(error) = fs::remove_file(file)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Some(format!(
                "the flow stopped the step, and {} stays: {error}",
                file.display()
            ));
        }
        Some("the flow stopped the step".to_owned())
    }
}

#[must_use]
pub fn run(args: &StepArgs) -> ExitCode {
    let harness = args.harness.into_harness();
    let stage = args.stage.trim();
    if stage.is_empty() {
        eprintln!("lune _step: stage name must not be empty");
        return ExitCode::FAILURE;
    }
    let timeout = match validate_timeout(args.timeout_secs) {
        Ok(timeout) => timeout,
        Err(why) => {
            eprintln!("lune _step: {why}");
            return ExitCode::FAILURE;
        }
    };
    let prompt = match read_text(args.prompt.as_deref(), args.prompt_file.as_deref()) {
        Ok(Some(text)) => text,
        Ok(None) => {
            eprintln!("lune _step: prompt must not be empty");
            return ExitCode::FAILURE;
        }
        Err(why) => {
            eprintln!("lune _step: {why}");
            return ExitCode::FAILURE;
        }
    };
    let followup = match read_text(args.followup.as_deref(), args.followup_file.as_deref()) {
        Ok(text) => text,
        Err(why) => {
            eprintln!("lune _step: {why}");
            return ExitCode::FAILURE;
        }
    };
    let signalled = Arc::new(AtomicBool::new(false));
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        if let Err(error) = signal_hook::flag::register(signal, Arc::clone(&signalled)) {
            eprintln!("lune _step: signal {signal} could not be handled: {error}");
            return ExitCode::FAILURE;
        }
    }
    let stopping = Stopping {
        file: args.stop_file.clone(),
        signalled,
    };
    let mut renderer = Renderer::new(io::stdout().lock());
    let step = StepPlan {
        harness,
        stage,
        prompt: &prompt,
        followup: followup.as_deref(),
        pi_session: args.pi_session.as_deref(),
        session_out: args.session_out.as_deref(),
        deadline: Instant::now() + timeout,
    };
    let result = execute(&step, &stopping, &mut renderer);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            renderer.note(&format!("\nerror: {why}"));
            renderer.raw(&osc_7501_error(harness, stage, &why));
            ExitCode::FAILURE
        }
    }
}

fn validate_timeout(secs: u64) -> Result<Duration, String> {
    if !(TIMEOUT_SECS_MIN..=TIMEOUT_SECS_MAX).contains(&secs) {
        return Err(format!(
            "timeout_secs {secs} must be within {TIMEOUT_SECS_MIN}..={TIMEOUT_SECS_MAX}"
        ));
    }
    Ok(Duration::from_secs(secs))
}

fn read_text(inline: Option<&str>, file: Option<&Path>) -> Result<Option<String>, String> {
    let raw = match (inline, file) {
        (Some(text), None) => text.to_owned(),
        (None, Some(path)) => read_file_bounded(path, PROMPT_BYTES_MAX)?,
        (None, None) => return Ok(None),
        (Some(_), Some(_)) => unreachable!("clap rejects both inline text and file"),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(raw))
}

fn read_file_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let cap = u64::try_from(limit).expect("usize fits u64");
    let mut bytes = Vec::new();
    file.take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > limit {
        return Err(format!("{}: exceeds {limit} bytes", path.display()));
    }
    String::from_utf8(bytes).map_err(|error| format!("{}: {error}", path.display()))
}

struct StepPlan<'a> {
    harness: Harness,
    stage: &'a str,
    prompt: &'a str,
    followup: Option<&'a str>,
    pi_session: Option<&'a Path>,
    session_out: Option<&'a Path>,
    deadline: Instant,
}

fn execute(
    step: &StepPlan<'_>,
    stopping: &Stopping,
    renderer: &mut Renderer<impl Write>,
) -> Result<(), String> {
    let harness = step.harness;
    assert!(!step.stage.is_empty(), "stage label is non-empty");
    assert!(!step.prompt.trim().is_empty(), "prompt is non-empty");
    let pi_session = match (harness, step.pi_session) {
        (Harness::Pi, Some(path)) => Some(
            path.to_str()
                .ok_or_else(|| format!("{} is not UTF-8", path.display()))?
                .to_owned(),
        ),
        _ => None,
    };
    if let (Some(out_path), Some(session_path)) = (step.session_out, pi_session.as_deref()) {
        write_session_id(out_path, session_path)?;
    }
    let turns = TurnPlan {
        harness,
        session_out: step.session_out,
        record_stream_session: pi_session.is_none(),
        deadline: step.deadline,
    };
    renderer.note(&format!("═══ {} ({}) ═══", step.stage, harness.program()));
    renderer.raw(&osc_7501_working(harness, step.stage));
    renderer.failure()?;

    let first = run_turn(
        &turns,
        step.prompt,
        pi_session.as_deref(),
        stopping,
        renderer,
    )?;
    let summary = match step.followup {
        None => first.summary,
        Some(followup) => {
            let resume = pi_session.or(first.session_id);
            let Some(handle) = resume.as_deref() else {
                return Err(format!(
                    "{} emitted no session_id on turn 1; cannot resume for turn 2",
                    harness.program()
                ));
            };
            renderer.note(&format!("\n{ACTING_DIVIDER}"));
            renderer.raw(&osc_7501_working(harness, step.stage));
            renderer.failure()?;
            run_turn(&turns, followup, Some(handle), stopping, renderer)?.summary
        }
    };
    let summary = summary.unwrap_or_else(|| format!("{} completed", step.stage));
    renderer.raw(&osc_7501_done(harness, step.stage, &summary));
    renderer.failure()
}

#[must_use]
pub fn build_command(harness: Harness, prompt: &str, resume_handle: Option<&str>) -> Command {
    assert!(!prompt.trim().is_empty(), "prompt is non-empty");
    if let Some(handle) = resume_handle {
        assert!(!handle.trim().is_empty(), "resume handle is non-empty");
        if harness != Harness::Pi {
            assert!(!handle.starts_with('-'), "resume handle is not a flag");
        }
    }
    let mut command = Command::new(harness.program());
    match harness {
        Harness::ClaudeCode => {
            command.args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
            ]);
            if let Some(id) = resume_handle {
                command.args(["--resume", id]);
            }
            command.args(["--", prompt]);
        }
        Harness::Antigravity => {
            command.args([
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
            ]);
            if let Some(id) = resume_handle {
                command.args(["--resume", id]);
            }
            command.arg(format!("-p={prompt}"));
        }
        Harness::Pi => {
            command.args(["--mode", "json"]);
            if let Some(session) = resume_handle {
                command.args(["--session", session]);
            }
            command.args(["-p", "--", prompt]);
        }
    }
    luned::local_git(&mut command);
    command
        .env_remove("LUNE_MUX_TERMINAL")
        .env_remove("LUNE_MUX_TERMINAL_MADE")
        .env_remove("OSC7577_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

struct TurnPlan<'a> {
    harness: Harness,
    session_out: Option<&'a Path>,
    record_stream_session: bool,
    deadline: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgyToolProgress {
    Announced,
    Finished,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgyToolState {
    InProgress,
    Done,
    Error,
}

struct Heard {
    session_id: Option<String>,
    verdict: Option<Verdict>,
    unparsed_lines: u64,
    agy_tool_steps: BTreeMap<u64, AgyToolProgress>,
}

fn run_turn(
    plan: &TurnPlan<'_>,
    prompt: &str,
    resume_handle: Option<&str>,
    stopping: &Stopping,
    renderer: &mut Renderer<impl Write>,
) -> Result<Turn, String> {
    let harness = plan.harness;
    let mut command = build_command(harness, prompt, resume_handle);
    let mut child = command
        .spawn()
        .map_err(|error| format!("{} could not be started: {error}", harness.program()))?;
    let group = Pid::from_child(&child);
    let stdout = child.stdout.take().expect("spawned child has piped stdout");
    let stderr = child.stderr.take().expect("spawned child has piped stderr");

    let (line_tx, line_rx) = mpsc::sync_channel::<LineItem>(LINE_QUEUE_MAX);
    let stdout_thread = thread::spawn(move || {
        let mut reader = BufReader::new(stdout.take(TURN_BYTES_MAX));
        for _ in 0..LINES_PER_TURN_MAX {
            let item = match read_line_bounded(&mut reader, LINE_BYTES_MAX) {
                Ok(Some(line)) => LineItem::Line(line),
                Ok(None) if reader.get_ref().limit() == 0 => LineItem::Error(io::Error::other(
                    format!("output exceeded {TURN_BYTES_MAX} bytes"),
                )),
                Ok(None) => LineItem::Eof,
                Err(error) => LineItem::Error(error),
            };
            let last = !matches!(item, LineItem::Line(_));
            if line_tx.send(item).is_err() || last {
                return;
            }
        }
        let exceeded = io::Error::other(format!("output exceeded {LINES_PER_TURN_MAX} lines"));
        let _receiver_left = line_tx.send(LineItem::Error(exceeded));
    });

    let (stderr_tx, stderr_rx) = mpsc::sync_channel::<io::Result<String>>(1);
    thread::spawn(move || {
        let drained = drain_stderr(stderr);
        let _receiver_left = stderr_tx.send(drained);
    });

    let mut heard = Heard {
        session_id: None,
        verdict: None,
        unparsed_lines: 0,
        agy_tool_steps: BTreeMap::new(),
    };
    let mut ended = false;
    let mut exited_at: Option<Instant> = None;
    let mut failure: Option<String> = None;
    for _ in 0..LINES_PER_TURN_MAX + STOP_POLLS_PER_STEP_MAX {
        if let Some(why) = stopping.requested() {
            failure = Some(why);
            break;
        }
        let Some(remaining) = plan.deadline.checked_duration_since(Instant::now()) else {
            failure = Some(format!(
                "{} ran past the step's deadline",
                harness.program()
            ));
            break;
        };
        match line_rx.recv_timeout(remaining.min(STOP_POLL)) {
            Ok(LineItem::Line(line)) => {
                failure = heard_line(plan, &line, &mut heard, renderer).err();
                if failure.is_some() {
                    break;
                }
            }
            Ok(LineItem::Eof) | Err(RecvTimeoutError::Disconnected) => {
                ended = true;
                break;
            }
            Ok(LineItem::Error(error)) => {
                failure = Some(format!("{} stream error: {error}", harness.program()));
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                if exited_at.is_none() {
                    match child.try_wait() {
                        Ok(Some(_)) => exited_at = Some(Instant::now()),
                        Ok(None) => {}
                        Err(error) => {
                            failure = Some(format!("failed to poll child status: {error}"));
                            break;
                        }
                    }
                }
                if exited_at.is_some_and(|at| at.elapsed() >= CHILD_WAIT_MAX) {
                    signal_group(group, Signal::KILL)?;
                }
            }
        }
    }
    if failure.is_none() && !ended {
        failure = Some(format!(
            "{} streamed past the turn's bound",
            harness.program()
        ));
    }
    if let Some(why) = failure {
        drop(line_rx);
        renderer.finish();
        end_group(&mut child, group)?;
        return Err(why);
    }

    renderer.finish();
    stdout_thread
        .join()
        .map_err(|_| format!("{} stdout reader panicked", harness.program()))?;
    let status = wait_child(&mut child, group, plan.deadline)?;
    signal_group(group, Signal::KILL)?;
    let stderr_text = match stderr_rx.recv_timeout(CHILD_WAIT_MAX) {
        Ok(Ok(text)) => text,
        Ok(Err(error)) => {
            return Err(format!("{} stderr read error: {error}", harness.program()));
        }
        Err(_) => String::new(),
    };
    renderer.failure()?;
    let summary = interpret_turn(
        harness,
        status,
        heard.verdict,
        &stderr_text,
        heard.unparsed_lines,
    )?;
    Ok(Turn {
        session_id: heard.session_id,
        summary,
    })
}

fn heard_line(
    plan: &TurnPlan<'_>,
    line: &str,
    heard: &mut Heard,
    renderer: &mut Renderer<impl Write>,
) -> Result<(), String> {
    assert!(heard.agy_tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    let events = parse_line_in_turn(plan.harness, line, &mut heard.agy_tool_steps)?;
    assert!(events.len() <= EVENTS_PER_LINE_MAX);
    if events.is_empty() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') {
            heard.unparsed_lines += 1;
        } else if !trimmed.is_empty() {
            renderer.block(trimmed);
        }
    }
    for event in events {
        match event {
            StreamEvent::SessionId(id) => {
                if heard.session_id.is_none() {
                    if !usable_session_id(&id) {
                        return Err(format!(
                            "{} reported an unusable session id",
                            plan.harness.program()
                        ));
                    }
                    if plan.record_stream_session
                        && let Some(path) = plan.session_out
                    {
                        write_session_id(path, &id)?;
                    }
                    heard.session_id = Some(id);
                }
            }
            StreamEvent::TextDelta(delta) => renderer.delta(&delta),
            StreamEvent::TextBlock(text) => renderer.block(&text),
            StreamEvent::ToolUse { name, detail } => renderer.tool(&name, detail.as_deref()),
            StreamEvent::ToolResult { name, is_error } => {
                if is_error {
                    let label = name.as_deref().unwrap_or("tool");
                    renderer.note(&format!("✗ {label} failed"));
                }
            }
            StreamEvent::TurnFinished(verdict) => {
                heard.verdict = match (heard.verdict.take(), verdict) {
                    (Some(Verdict::Failed(why)), Verdict::Succeeded(_)) => {
                        Some(Verdict::Failed(why))
                    }
                    (_, verdict) => Some(verdict),
                };
            }
            StreamEvent::Retried => {
                if matches!(heard.verdict, Some(Verdict::Failed(_))) {
                    heard.verdict = None;
                }
            }
        }
    }
    renderer.failure()
}

fn usable_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= SESSION_ID_BYTES_MAX
        && !id.starts_with('-')
        && !id.chars().any(char::is_control)
}

fn drain_stderr(mut stderr: impl io::Read) -> io::Result<String> {
    let mut captured = Vec::new();
    let mut scratch = [0u8; STDERR_DRAIN_CHUNK_BYTES];
    let mut ended = false;
    for _ in 0..STDERR_DRAIN_CHUNKS_MAX {
        let read = stderr.read(&mut scratch)?;
        if read == 0 {
            ended = true;
            break;
        }
        let room = STDERR_BYTES_MAX.saturating_sub(captured.len());
        captured.extend_from_slice(&scratch[..read.min(room)]);
    }
    assert!(captured.len() <= STDERR_BYTES_MAX);
    if !ended {
        return Err(io::Error::other(format!(
            "stderr exceeded {TURN_BYTES_MAX} bytes"
        )));
    }
    Ok(String::from_utf8_lossy(&captured).trim().to_owned())
}

fn interpret_turn(
    harness: Harness,
    status: ExitStatus,
    verdict: Option<Verdict>,
    stderr_text: &str,
    unparsed_lines: u64,
) -> Result<Option<String>, String> {
    let program = harness.program();
    let exited = || {
        if stderr_text.is_empty() {
            format!("{program} exited with {status}")
        } else {
            stderr_text.to_owned()
        }
    };
    match (status.success(), verdict) {
        (true, Some(Verdict::Succeeded(summary))) => Ok(summary),
        (true, None) if harness == Harness::Pi => Ok(None),
        (true, None) => Err(format!(
            "{program} ended without a result record ({unparsed_lines} lines did not parse)"
        )),
        (_, Some(Verdict::Failed(why))) => Err(why),
        (false, Some(Verdict::Succeeded(_)) | None) => Err(exited()),
    }
}

fn wait_child(child: &mut Child, group: Pid, deadline: Instant) -> Result<ExitStatus, String> {
    let wait_until = deadline.min(Instant::now() + CHILD_WAIT_MAX);
    for _ in 0..CHILD_WAIT_POLLS_MAX {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= wait_until {
                    break;
                }
                thread::sleep(CHILD_WAIT_POLL);
            }
            Err(error) => return Err(format!("failed to poll child status: {error}")),
        }
    }
    end_group(child, group)
}

fn signal_group(group: Pid, signal: Signal) -> Result<(), String> {
    match rustix::process::kill_process_group(group, signal) {
        Ok(()) | Err(Errno::SRCH) => Ok(()),
        Err(error) => Err(format!(
            "process group {} could not be signalled: {error}",
            group.as_raw_nonzero()
        )),
    }
}

fn end_group(child: &mut Child, group: Pid) -> Result<ExitStatus, String> {
    signal_group(group, Signal::TERM)?;
    for _ in 0..CHILD_WAIT_POLLS_MAX {
        match child.try_wait() {
            Ok(Some(status)) => {
                signal_group(group, Signal::KILL)?;
                return Ok(status);
            }
            Ok(None) => thread::sleep(CHILD_WAIT_POLL),
            Err(error) => return Err(format!("failed to poll child status: {error}")),
        }
    }
    signal_group(group, Signal::KILL)?;
    child
        .wait()
        .map_err(|error| format!("failed to wait on child: {error}"))
}

fn write_session_id(path: &Path, id: &str) -> Result<(), String> {
    assert!(!id.trim().is_empty(), "session id written is non-empty");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let staged = path.with_extension(format!("tmp.{}", std::process::id()));
    let written = fs::write(&staged, format!("{id}\n")).and_then(|()| fs::rename(&staged, path));
    if written.is_err()
        && let Err(error) = fs::remove_file(&staged)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(format!("{}: {error}", staged.display()));
    }
    written.map_err(|error| format!("{}: {error}", path.display()))
}

fn read_line_bounded(
    reader: &mut impl io::BufRead,
    bytes_max: usize,
) -> io::Result<Option<String>> {
    assert!(bytes_max > 0, "line bound holds at least one byte");
    let limit = u64::try_from(bytes_max).expect("usize fits u64") + 1;
    let mut raw = Vec::new();
    let read = reader.by_ref().take(limit).read_until(b'\n', &mut raw)?;
    if read == 0 {
        return Ok(None);
    }
    if raw.len() > bytes_max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("stream line exceeded {bytes_max} bytes"),
        ));
    }
    if raw.ends_with(b"\n") {
        raw.pop();
        if raw.ends_with(b"\r") {
            raw.pop();
        }
    }
    String::from_utf8(raw)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn printable(text: &str) -> Cow<'_, str> {
    let unprintable = |c: char| c.is_control() && c != '\n' && c != '\t';
    if !text.chars().any(unprintable) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .map(|c| if unprintable(c) { '\u{FFFD}' } else { c })
            .collect(),
    )
}

struct Renderer<W: Write> {
    out: W,
    mid_line: bool,
    streamed_delta: bool,
    failed: Option<io::Error>,
}

impl<W: Write> Renderer<W> {
    fn new(out: W) -> Renderer<W> {
        Renderer {
            out,
            mid_line: false,
            streamed_delta: false,
            failed: None,
        }
    }

    fn failure(&mut self) -> Result<(), String> {
        match self.failed.take() {
            Some(error) => Err(format!("the step's terminal could not be written: {error}")),
            None => Ok(()),
        }
    }

    fn written(&mut self, written: io::Result<()>) {
        if let Err(error) = written
            && self.failed.is_none()
        {
            self.failed = Some(error);
        }
    }

    fn raw(&mut self, sequence: &str) {
        let written = self
            .out
            .write_all(sequence.as_bytes())
            .and_then(|()| self.out.flush());
        self.written(written);
    }

    fn text(&mut self, text: &str) {
        let shown = printable(text);
        assert!(
            !shown.contains('\x1b'),
            "agent text reaches the terminal without escapes"
        );
        let written = self
            .out
            .write_all(shown.as_bytes())
            .and_then(|()| self.out.flush());
        self.written(written);
    }

    fn end_line(&mut self) {
        if self.mid_line {
            self.text("\n");
            self.mid_line = false;
        }
    }

    fn delta(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.text(delta);
        self.mid_line = !delta.ends_with('\n');
        self.streamed_delta = true;
    }

    fn block(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.streamed_delta {
            self.streamed_delta = false;
            self.end_line();
            return;
        }
        self.end_line();
        self.text(&format!("{text}\n"));
    }

    fn tool(&mut self, name: &str, detail: Option<&str>) {
        self.streamed_delta = false;
        self.end_line();
        match detail {
            Some(detail) if !detail.is_empty() => self.text(&format!("▸ {name}: {detail}\n")),
            _ => self.text(&format!("▸ {name}\n")),
        }
    }

    fn note(&mut self, note: &str) {
        self.streamed_delta = false;
        self.end_line();
        self.text(&format!("{note}\n"));
    }

    fn finish(&mut self) {
        self.end_line();
    }
}

fn finished(ok: bool, summary: Option<String>, fallback: &str) -> StreamEvent {
    if ok {
        return StreamEvent::TurnFinished(Verdict::Succeeded(summary));
    }
    StreamEvent::TurnFinished(Verdict::Failed(
        summary.unwrap_or_else(|| fallback.to_owned()),
    ))
}

#[must_use]
pub fn parse_line(harness: Harness, line: &str) -> Vec<StreamEvent> {
    parse_line_in_turn(harness, line, &mut BTreeMap::new())
        .expect("a single line never exceeds AGY_TOOL_STEPS_MAX")
}

fn parse_line_in_turn(
    harness: Harness,
    line: &str,
    agy_tool_steps: &mut BTreeMap<u64, AgyToolProgress>,
) -> Result<Vec<StreamEvent>, String> {
    assert!(agy_tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Ok(Vec::new());
    };
    let events = match harness {
        Harness::ClaudeCode => parse_claude(&record),
        Harness::Antigravity => parse_agy(&record, agy_tool_steps)?,
        Harness::Pi => parse_pi(&record),
    };
    assert!(events.len() <= EVENTS_PER_LINE_MAX);
    assert!(agy_tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    Ok(events)
}

fn parse_claude(record: &Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    if let Some(id) = nonempty_str(&record["session_id"]) {
        events.push(StreamEvent::SessionId(id.to_owned()));
    }
    match record["type"].as_str() {
        Some("assistant") if record["parent_tool_use_id"].is_null() => {
            let blocks = record["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .take(CONTENT_BLOCKS_MAX);
            for block in blocks {
                push_content_block(&mut events, block);
            }
        }
        Some("stream_event") => {
            if record["event"]["type"].as_str() == Some("content_block_delta")
                && record["event"]["delta"]["type"].as_str() == Some("text_delta")
                && let Some(text) = record["event"]["delta"]["text"].as_str()
                && !text.is_empty()
            {
                events.push(StreamEvent::TextDelta(text.to_owned()));
            }
        }
        Some("result") => {
            let subtype = record["subtype"].as_str();
            let is_error = record["is_error"].as_bool() == Some(true)
                || subtype.is_some_and(|s| s != "success");
            let summary = nonempty_str(&record["result"])
                .or_else(|| nonempty_str(&record["error"]))
                .or(subtype)
                .map(str::to_owned);
            events.push(finished(!is_error, summary, "claude reported an error"));
        }
        _ => {}
    }
    events
}

fn parse_agy(
    record: &Value,
    tool_steps: &mut BTreeMap<u64, AgyToolProgress>,
) -> Result<Vec<StreamEvent>, String> {
    assert!(tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    let mut events = Vec::new();
    if let Some(id) = nonempty_str(&record["conversation_id"])
        .or_else(|| nonempty_str(&record["step_update"]["conversation_id"]))
        .or_else(|| nonempty_str(&record["result"]["conversation_id"]))
    {
        events.push(StreamEvent::SessionId(id.to_owned()));
    }
    match record["event"].as_str() {
        Some("step_update") => {
            let update = &record["step_update"];
            match update["step_type"].as_str() {
                Some("agent_response") => {
                    if let Some(delta) = update["text_delta"].as_str().filter(|s| !s.is_empty()) {
                        events.push(StreamEvent::TextDelta(delta.to_owned()));
                    }
                }
                Some("tool") => parse_agy_tool(update, tool_steps, &mut events)?,
                _ => {}
            }
        }
        Some("result") => {
            events.push(StreamEvent::TurnFinished(parse_agy_result(
                &record["result"],
            )));
        }
        _ => {}
    }
    assert!(events.len() <= AGY_EVENTS_PER_LINE_MAX);
    assert!(tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    Ok(events)
}

fn agy_finished_reason_ok(reason: &str) -> bool {
    assert_ne!(reason.trim(), "");
    ["normal", "stop", "complete", "success"]
        .iter()
        .any(|ok| reason.eq_ignore_ascii_case(ok))
}

fn agy_result_failure(
    result: &Value,
    finished_reason: Option<&str>,
    response: Option<&str>,
) -> Verdict {
    let summary = nonempty_str(&result["error"])
        .or(finished_reason)
        .or(response)
        .or_else(|| nonempty_str(&result["status"]))
        .unwrap_or("agy reported an error");
    assert_ne!(summary.trim(), "");
    Verdict::Failed(summary.to_owned())
}

fn parse_agy_result(result: &Value) -> Verdict {
    let status_ok = result["status"].as_str() == Some("SUCCESS");
    let finished_reason = nonempty_str(&result["finished_reason"]);
    let finished_reason_ok = finished_reason.is_none_or(agy_finished_reason_ok);
    let response = nonempty_str(&result["response"]);

    if status_ok {
        if finished_reason_ok {
            let has_denied_actions = result["denied_actions"]
                .as_array()
                .is_some_and(|denied| !denied.is_empty());
            if response.is_none() && has_denied_actions {
                Verdict::Failed("agy stopped because a tool action was denied".to_owned())
            } else {
                Verdict::Succeeded(response.map(str::to_owned))
            }
        } else {
            agy_result_failure(result, finished_reason, response)
        }
    } else {
        agy_result_failure(result, finished_reason, response)
    }
}

fn agy_tool_state(update: &Value) -> AgyToolState {
    assert_eq!(update["step_type"].as_str(), Some("tool"));
    let state = update["state"].as_str();
    let has_error = update["tool_info"]
        .get("error")
        .is_some_and(|value| !value.is_null());
    if state == Some("ERROR") || has_error {
        AgyToolState::Error
    } else if state == Some("DONE") {
        AgyToolState::Done
    } else {
        AgyToolState::InProgress
    }
}

fn agy_tool_use_event(update: &Value, name: &str) -> StreamEvent {
    assert_ne!(name.trim(), "");
    let detail = extract_tool_detail(&update["tool_info"]["parameters"])
        .or_else(|| extract_tool_detail(&update["tool_info"]))
        .or_else(|| parse_agy_subagent_detail(&update["subagent_info"]));
    StreamEvent::ToolUse {
        name: name.to_owned(),
        detail,
    }
}

fn parse_agy_tool(
    update: &Value,
    tool_steps: &mut BTreeMap<u64, AgyToolProgress>,
    events: &mut Vec<StreamEvent>,
) -> Result<(), String> {
    assert_eq!(update["step_type"].as_str(), Some("tool"));
    assert!(tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    let state = agy_tool_state(update);
    let tool_name =
        nonempty_str(&update["tool_name"]).or_else(|| nonempty_str(&update["tool_info"]["name"]));
    let step_index = update["step_index"].as_u64();

    match (state, step_index) {
        (AgyToolState::InProgress, Some(index)) => {
            if let Some(name) = tool_name
                && !tool_steps.contains_key(&index)
            {
                if tool_steps.len() < AGY_TOOL_STEPS_MAX {
                    let previous = tool_steps.insert(index, AgyToolProgress::Announced);
                    assert!(previous.is_none());
                    events.push(agy_tool_use_event(update, name));
                } else {
                    return Err(format!("agy exceeded {AGY_TOOL_STEPS_MAX} tool steps"));
                }
            }
        }
        (AgyToolState::InProgress, None) => {
            if let Some(name) = tool_name {
                events.push(agy_tool_use_event(update, name));
            }
        }
        (AgyToolState::Done | AgyToolState::Error, Some(index)) => {
            let is_error = state == AgyToolState::Error;
            match tool_steps.get(&index).copied() {
                None => {
                    if tool_steps.len() < AGY_TOOL_STEPS_MAX {
                        let previous = tool_steps.insert(index, AgyToolProgress::Finished);
                        assert!(previous.is_none());
                        if let Some(name) = tool_name {
                            events.push(agy_tool_use_event(update, name));
                        }
                        events.push(StreamEvent::ToolResult {
                            name: tool_name.map(str::to_owned),
                            is_error,
                        });
                    } else {
                        return Err(format!("agy exceeded {AGY_TOOL_STEPS_MAX} tool steps"));
                    }
                }
                Some(AgyToolProgress::Announced) => {
                    let previous = tool_steps.insert(index, AgyToolProgress::Finished);
                    assert_eq!(previous, Some(AgyToolProgress::Announced));
                    events.push(StreamEvent::ToolResult {
                        name: tool_name.map(str::to_owned),
                        is_error,
                    });
                }
                Some(AgyToolProgress::Finished) => {}
            }
        }
        (AgyToolState::Done | AgyToolState::Error, None) => {
            events.push(StreamEvent::ToolResult {
                name: tool_name.map(str::to_owned),
                is_error: state == AgyToolState::Error,
            });
        }
    }
    assert!(tool_steps.len() <= AGY_TOOL_STEPS_MAX);
    Ok(())
}

fn parse_pi(record: &Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    match record["type"].as_str() {
        Some("session") => {
            if let Some(id) = nonempty_str(&record["id"]) {
                events.push(StreamEvent::SessionId(id.to_owned()));
            }
        }
        Some("message_update") => {
            let nested = &record["assistantMessageEvent"];
            match nested["type"].as_str() {
                Some("text_delta") => {
                    if let Some(delta) = nested["delta"].as_str()
                        && !delta.is_empty()
                    {
                        events.push(StreamEvent::TextDelta(delta.to_owned()));
                    }
                }
                Some("error") => {
                    let summary = nonempty_str(&nested["error"])
                        .or_else(|| nonempty_str(&nested["reason"]))
                        .map(str::to_owned);
                    events.push(finished(false, summary, "pi reported an error"));
                }
                _ => {}
            }
        }
        Some("message_end") => {
            let message = &record["message"];
            if message["role"].as_str() == Some("assistant") {
                if let Some(reason) = message["stopReason"].as_str()
                    && matches!(reason, "error" | "aborted")
                {
                    let summary = nonempty_str(&message["errorMessage"])
                        .or(Some(reason))
                        .map(str::to_owned);
                    events.push(finished(false, summary, "pi reported an error"));
                } else if let Some(blocks) = message["content"].as_array() {
                    for block in blocks.iter().take(CONTENT_BLOCKS_MAX) {
                        if block["type"].as_str() == Some("text")
                            && let Some(text) = nonempty_str(&block["text"])
                        {
                            events.push(StreamEvent::TextBlock(text.to_owned()));
                        }
                    }
                }
            }
        }
        Some("tool_execution_start") => {
            if let Some(name) = nonempty_str(&record["toolName"]) {
                let detail = extract_tool_detail(&record["args"]);
                events.push(StreamEvent::ToolUse {
                    name: name.to_owned(),
                    detail,
                });
            }
        }
        Some("tool_execution_end") => {
            let name = nonempty_str(&record["toolName"]).map(str::to_owned);
            let is_error = record["isError"].as_bool() == Some(true);
            events.push(StreamEvent::ToolResult { name, is_error });
        }
        Some("auto_retry_end") => match record["success"].as_bool() {
            Some(true) => events.push(StreamEvent::Retried),
            Some(false) => {
                let summary = nonempty_str(&record["finalError"]).map(str::to_owned);
                events.push(finished(false, summary, "pi gave up retrying"));
            }
            None => {}
        },
        Some("agent_settled") => {
            let aborted = record["aborted"].as_bool() == Some(true);
            events.push(finished(!aborted, None, "pi aborted the turn"));
        }
        _ => {}
    }
    events
}

fn push_content_block(events: &mut Vec<StreamEvent>, block: &Value) {
    match block["type"].as_str() {
        Some("text") => {
            if let Some(text) = nonempty_str(&block["text"]) {
                events.push(StreamEvent::TextBlock(text.to_owned()));
            }
        }
        Some("tool_use") => {
            if let Some(name) = nonempty_str(&block["name"]) {
                let detail = extract_tool_detail(&block["input"]);
                events.push(StreamEvent::ToolUse {
                    name: name.to_owned(),
                    detail,
                });
            }
        }
        _ => {}
    }
}

fn extract_tool_detail(value: &Value) -> Option<String> {
    let raw = nonempty_str(&value["command"])
        .or_else(|| nonempty_str(&value["command_line"]))
        .or_else(|| nonempty_str(&value["CommandLine"]))
        .or_else(|| nonempty_str(&value["file_path"]))
        .or_else(|| nonempty_str(&value["FilePath"]))
        .or_else(|| nonempty_str(&value["path"]))
        .or_else(|| nonempty_str(&value["Path"]))
        .or_else(|| nonempty_str(&value["TargetFile"]))
        .or_else(|| nonempty_str(&value["AbsolutePath"]))
        .or_else(|| nonempty_str(&value["pattern"]))
        .or_else(|| nonempty_str(&value["Pattern"]))
        .or_else(|| nonempty_str(&value["query"]))
        .or_else(|| nonempty_str(&value["Query"]))
        .or_else(|| nonempty_str(&value["url"]))
        .or_else(|| nonempty_str(&value["Url"]))?;
    assert_ne!(raw.trim(), "");
    let preview = single_line_preview(raw, DETAIL_CHARS_MAX);
    assert_ne!(preview, "");
    Some(preview)
}

fn parse_agy_subagent_detail(value: &Value) -> Option<String> {
    let first = value["subagents"].as_array()?.first()?;
    let raw = nonempty_str(&first["role"]).or_else(|| nonempty_str(&first["type_name"]))?;
    assert_ne!(raw.trim(), "");
    let preview = single_line_preview(raw, DETAIL_CHARS_MAX);
    assert_ne!(preview, "");
    Some(preview)
}

fn nonempty_str(value: &Value) -> Option<&str> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then_some(text)
}

fn single_line_preview(text: &str, chars_max: usize) -> String {
    assert!(chars_max > 0, "preview cap holds at least one char");
    let first_line = text.lines().next().unwrap_or(text).trim();
    let mut out = String::new();
    for (index, ch) in first_line.chars().enumerate() {
        if index >= chars_max {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

fn truncate_utf8(text: &str, bytes_max: usize) -> &str {
    if text.len() <= bytes_max {
        return text;
    }
    let end = text.floor_char_boundary(bytes_max);
    assert!(end <= bytes_max);
    assert!(text.is_char_boundary(end));
    &text[..end]
}

#[must_use]
pub fn osc_7501_working(harness: Harness, stage: &str) -> String {
    let app = harness
        .osc_7501_app()
        .expect("every flow harness declares an OSC 7501 app");
    let title = STANDARD.encode(truncate_utf8(stage, OSC_TITLE_BYTES_MAX));
    format!("\x1b]7501;state=working:app={app}:title={title}\x07")
}

#[must_use]
pub fn osc_7501_done(harness: Harness, stage: &str, message: &str) -> String {
    let app = harness
        .osc_7501_app()
        .expect("every flow harness declares an OSC 7501 app");
    let title = STANDARD.encode(truncate_utf8(stage, OSC_TITLE_BYTES_MAX));
    let msg = STANDARD.encode(truncate_utf8(message, OSC_MESSAGE_BYTES_MAX));
    format!("\x1b]7501;state=done:app={app}:title={title}:msg={msg}\x07")
}

#[must_use]
pub fn osc_7501_error(harness: Harness, stage: &str, message: &str) -> String {
    let app = harness
        .osc_7501_app()
        .expect("every flow harness declares an OSC 7501 app");
    let title = STANDARD.encode(truncate_utf8(stage, OSC_TITLE_BYTES_MAX));
    let msg = STANDARD.encode(truncate_utf8(message, OSC_MESSAGE_BYTES_MAX));
    format!("\x1b]7501;state=error:app={app}:title={title}:msg={msg}\x07")
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn build_command_matches_each_harness_cli_contract() {
        let claude1 = build_command(Harness::ClaudeCode, "/tigerstyle PR #1", None);
        let args: Vec<&OsStr> = claude1.get_args().collect();
        assert_eq!(claude1.get_program(), OsStr::new("claude"));
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
                "--",
                "/tigerstyle PR #1"
            ]
            .map(OsStr::new)
        );

        let claude2 = build_command(Harness::ClaudeCode, "act now", Some("sess-123"));
        let args: Vec<&OsStr> = claude2.get_args().collect();
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
                "--resume",
                "sess-123",
                "--",
                "act now"
            ]
            .map(OsStr::new)
        );

        let agy1 = build_command(Harness::Antigravity, "/tigerstyle", None);
        let args: Vec<&OsStr> = agy1.get_args().collect();
        assert_eq!(agy1.get_program(), OsStr::new("agy"));
        assert!(!args.contains(&OsStr::new("--verbose")));
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "-p=/tigerstyle",
            ]
            .map(OsStr::new)
        );

        let agy2 = build_command(Harness::Antigravity, "fix", Some("conv-9"));
        let args: Vec<&OsStr> = agy2.get_args().collect();
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "--resume",
                "conv-9",
                "-p=fix",
            ]
            .map(OsStr::new)
        );

        let agy_dash = build_command(Harness::Antigravity, "--help", None);
        let args: Vec<&OsStr> = agy_dash.get_args().collect();
        assert_eq!(
            args,
            [
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "-p=--help",
            ]
            .map(OsStr::new)
        );

        let pi = build_command(
            Harness::Pi,
            "/skill:tigerstyle",
            Some("/tmp/pi-session.jsonl"),
        );
        let args: Vec<&OsStr> = pi.get_args().collect();
        assert_eq!(pi.get_program(), OsStr::new("pi"));
        assert_eq!(
            args,
            [
                "--mode",
                "json",
                "--session",
                "/tmp/pi-session.jsonl",
                "-p",
                "--",
                "/skill:tigerstyle"
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn parses_claude_stream_json_events() {
        let init = r#"{"type":"system","subtype":"init","session_id":"abc-123"}"#;
        assert_eq!(
            parse_line(Harness::ClaudeCode, init),
            vec![StreamEvent::SessionId("abc-123".to_owned())]
        );

        let assistant = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"text","text":"Auditing PR."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        assert_eq!(
            parse_line(Harness::ClaudeCode, assistant),
            vec![
                StreamEvent::TextBlock("Auditing PR.".to_owned()),
                StreamEvent::ToolUse {
                    name: "Bash".to_owned(),
                    detail: Some("cargo test".to_owned()),
                },
            ]
        );

        let result_ok =
            r#"{"type":"result","subtype":"success","is_error":false,"result":"All clean"}"#;
        assert_eq!(
            parse_line(Harness::ClaudeCode, result_ok),
            vec![StreamEvent::TurnFinished(Verdict::Succeeded(Some(
                "All clean".to_owned()
            )))]
        );

        let result_err = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}"#;
        assert_eq!(
            parse_line(Harness::ClaudeCode, result_err),
            vec![StreamEvent::TurnFinished(Verdict::Failed(
                "boom".to_owned()
            ))]
        );
    }

    #[test]
    fn parses_agy_stream_json_events() {
        let init = r#"{"event":"init","conversation_id":"conv-42","init":{"cwd":"/w","permission_mode":"always-proceed"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, init),
            vec![StreamEvent::SessionId("conv-42".to_owned())]
        );

        let user_input = r#"{"event":"step_update","step_update":{"conversation_id":"conv-42","step_index":0,"state":"DONE","step_type":"user_input"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, user_input),
            vec![StreamEvent::SessionId("conv-42".to_owned())]
        );

        let update_text = r#"{"event":"step_update","step_update":{"conversation_id":"conv-42","step_index":1,"state":"ACTIVE","step_type":"agent_response","text_delta":"Finding 1"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, update_text),
            vec![
                StreamEvent::SessionId("conv-42".to_owned()),
                StreamEvent::TextDelta("Finding 1".to_owned()),
            ]
        );

        let update_tool = r#"{"event":"step_update","step_update":{"conversation_id":"conv-42","step_index":2,"state":"ACTIVE","step_type":"tool","tool_name":"run_command","tool_info":{"name":"run_command","parameters":{"CommandLine":"cargo clippy"}}}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, update_tool),
            vec![
                StreamEvent::SessionId("conv-42".to_owned()),
                StreamEvent::ToolUse {
                    name: "run_command".to_owned(),
                    detail: Some("cargo clippy".to_owned()),
                },
            ]
        );

        let update_tool_error = r#"{"event":"step_update","step_update":{"step_index":3,"state":"ERROR","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"/w/missing.rs"},"error":{"type":"TOOL_ERROR","message":"file does not exist"}}}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, update_tool_error),
            vec![
                StreamEvent::ToolUse {
                    name: "read_file".to_owned(),
                    detail: Some("/w/missing.rs".to_owned()),
                },
                StreamEvent::ToolResult {
                    name: Some("read_file".to_owned()),
                    is_error: true,
                },
            ]
        );

        let result_ok = r#"{"event":"result","result":{"conversation_id":"conv-42","status":"SUCCESS","response":"Done\n"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_ok),
            vec![
                StreamEvent::SessionId("conv-42".to_owned()),
                StreamEvent::TurnFinished(Verdict::Succeeded(Some("Done".to_owned()))),
            ]
        );

        let result_timeout = r#"{"event":"result","result":{"conversation_id":"","status":"ERROR","finished_reason":"print_timeout","error":"timed out"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_timeout),
            vec![StreamEvent::TurnFinished(Verdict::Failed(
                "timed out".to_owned()
            ))]
        );

        let result_waiting = r#"{"event":"result","result":{"status":"WAITING"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_waiting),
            vec![StreamEvent::TurnFinished(Verdict::Failed(
                "WAITING".to_owned()
            ))]
        );

        let result_denied = r#"{"event":"result","result":{"status":"SUCCESS","response":"","denied_actions":[{"action":"run_command"}]}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_denied),
            vec![StreamEvent::TurnFinished(Verdict::Failed(
                "agy stopped because a tool action was denied".to_owned()
            ))]
        );

        let result_whitespace_reason = r#"{"event":"result","result":{"status":"SUCCESS","finished_reason":"   ","response":"Clean"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_whitespace_reason),
            vec![StreamEvent::TurnFinished(Verdict::Succeeded(Some(
                "Clean".to_owned()
            )))]
        );

        let result_denied_with_response = r#"{"event":"result","result":{"status":"SUCCESS","finished_reason":"NORMAL","response":"Answered anyway","denied_actions":[{"action":"run_command"}]}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_denied_with_response),
            vec![StreamEvent::TurnFinished(Verdict::Succeeded(Some(
                "Answered anyway".to_owned()
            )))]
        );

        let subagent_role = r#"{"event":"step_update","step_update":{"step_index":7,"state":"ACTIVE","step_type":"tool","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"role":"Codebase Researcher","type_name":"research"}]}}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, subagent_role),
            vec![StreamEvent::ToolUse {
                name: "invoke_subagent".to_owned(),
                detail: Some("Codebase Researcher".to_owned()),
            }]
        );

        let subagent_type_fallback = r#"{"event":"step_update","step_update":{"step_index":8,"state":"ACTIVE","step_type":"tool","tool_name":"invoke_subagent","subagent_info":{"subagents":[{"type_name":"research"}]}}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, subagent_type_fallback),
            vec![StreamEvent::ToolUse {
                name: "invoke_subagent".to_owned(),
                detail: Some("research".to_owned()),
            }]
        );
    }

    #[test]
    fn agy_tool_steps_deduplicate_without_dropping_nameless_updates_and_fail_at_bound() {
        let mut tool_steps = BTreeMap::new();

        let pending_without_name = r#"{"event":"step_update","step_update":{"step_index":5,"state":"PENDING","step_type":"tool"}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, pending_without_name, &mut tool_steps)
                .unwrap(),
            Vec::new()
        );
        assert!(tool_steps.is_empty());

        let active_named = r#"{"event":"step_update","step_update":{"step_index":5,"state":"ACTIVE","step_type":"tool","tool_name":"read_file","tool_info":{"parameters":{"Path":"/w/a.rs"}}}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, active_named, &mut tool_steps).unwrap(),
            vec![StreamEvent::ToolUse {
                name: "read_file".to_owned(),
                detail: Some("/w/a.rs".to_owned()),
            }]
        );
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, active_named, &mut tool_steps).unwrap(),
            Vec::new()
        );

        let error_update = r#"{"event":"step_update","step_update":{"step_index":5,"state":"ERROR","step_type":"tool","tool_name":"read_file","tool_info":{"error":{"message":"missing"}}}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, error_update, &mut tool_steps).unwrap(),
            vec![StreamEvent::ToolResult {
                name: Some("read_file".to_owned()),
                is_error: true,
            }]
        );
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, error_update, &mut tool_steps).unwrap(),
            Vec::new()
        );

        let unindexed_active = r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"tool","tool_name":"bash"}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, unindexed_active, &mut tool_steps).unwrap(),
            vec![StreamEvent::ToolUse {
                name: "bash".to_owned(),
                detail: None,
            }]
        );
        let unindexed_done = r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_name":"bash"}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, unindexed_done, &mut tool_steps).unwrap(),
            vec![StreamEvent::ToolResult {
                name: Some("bash".to_owned()),
                is_error: false,
            }]
        );

        let mut saturated: BTreeMap<u64, AgyToolProgress> = (0..AGY_TOOL_STEPS_MAX as u64)
            .map(|index| (index, AgyToolProgress::Announced))
            .collect();
        let existing_done = r#"{"event":"step_update","step_update":{"step_index":0,"state":"DONE","step_type":"tool","tool_name":"read_file"}}"#;
        assert_eq!(
            parse_line_in_turn(Harness::Antigravity, existing_done, &mut saturated).unwrap(),
            vec![StreamEvent::ToolResult {
                name: Some("read_file".to_owned()),
                is_error: false,
            }]
        );

        let overflow = format!(
            r#"{{"event":"step_update","step_update":{{"step_index":{AGY_TOOL_STEPS_MAX},"state":"ACTIVE","step_type":"tool","tool_name":"read_file"}}}}"#
        );
        let error =
            parse_line_in_turn(Harness::Antigravity, &overflow, &mut saturated).unwrap_err();
        assert!(error.contains("tool steps"), "{error}");
    }

    #[test]
    fn parses_pi_json_mode_events() {
        let session = r#"{"type":"session","version":3,"id":"pi-uuid-7","cwd":"/w"}"#;
        assert_eq!(
            parse_line(Harness::Pi, session),
            vec![StreamEvent::SessionId("pi-uuid-7".to_owned())]
        );

        let delta = r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","contentIndex":0,"delta":"Reviewing "}}"#;
        assert_eq!(
            parse_line(Harness::Pi, delta),
            vec![StreamEvent::TextDelta("Reviewing ".to_owned())]
        );

        let tool = r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{"command":"dev test"}}"#;
        assert_eq!(
            parse_line(Harness::Pi, tool),
            vec![StreamEvent::ToolUse {
                name: "bash".to_owned(),
                detail: Some("dev test".to_owned()),
            }]
        );

        let settled = r#"{"type":"agent_settled","aborted":false}"#;
        assert_eq!(
            parse_line(Harness::Pi, settled),
            vec![StreamEvent::TurnFinished(Verdict::Succeeded(None))]
        );

        let aborted = r#"{"type":"agent_settled","aborted":true}"#;
        assert_eq!(
            parse_line(Harness::Pi, aborted),
            vec![StreamEvent::TurnFinished(Verdict::Failed(
                "pi aborted the turn".to_owned()
            ))]
        );
    }

    #[test]
    fn osc_7501_encodes_app_title_and_message() {
        let working = osc_7501_working(Harness::ClaudeCode, "tigerstyle");
        assert!(working.starts_with("\x1b]7501;state=working:app=claude-code:title="));
        assert!(working.ends_with('\x07'));

        let done = osc_7501_done(Harness::Pi, "illegal-states", "clean");
        assert!(done.starts_with("\x1b]7501;state=done:app=pi:title="));
        assert!(done.contains(":msg="));
        assert!(done.ends_with('\x07'));

        let err = osc_7501_error(Harness::Antigravity, "thermo-nuclear", "dirty tree");
        assert!(err.starts_with("\x1b]7501;state=error:app=antigravity:title="));
        assert!(err.contains(":msg="));
        assert!(err.ends_with('\x07'));
    }

    #[test]
    fn parse_claude_bounds_content_blocks_and_truncate_utf8_respects_boundaries() {
        let blocks: Vec<serde_json::Value> = (0..CONTENT_BLOCKS_MAX + 50)
            .map(|idx| serde_json::json!({"type": "text", "text": format!("b{idx}")}))
            .collect();
        let line = serde_json::json!({
            "type": "assistant",
            "parent_tool_use_id": null,
            "message": { "content": blocks }
        })
        .to_string();
        let events = parse_line(Harness::ClaudeCode, &line);
        assert_eq!(events.len(), CONTENT_BLOCKS_MAX);
        assert_eq!(truncate_utf8("héllo", 2), "h");
        assert_eq!(truncate_utf8("héllo", 3), "hé");
    }
}
