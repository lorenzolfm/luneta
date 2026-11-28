use std::fs;
use std::io::{self, BufRead as _, BufReader, Read as _, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use clap::{ArgGroup, Args, ValueEnum};
use lune_domain::agent::Harness;
use serde_json::Value;

pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;

pub const TIMEOUT_SECS_MIN: u64 = 1;

pub const TIMEOUT_SECS_MAX: u64 = 4 * 3600;

pub const ACTING_DIVIDER: &str = "─── Acting on findings ───";

const LINE_BYTES_MAX: usize = 16 * 1024 * 1024;

const LINES_PER_TURN_MAX: usize = 65_536;

const PROMPT_BYTES_MAX: usize = 1024 * 1024;

const STDERR_BYTES_MAX: usize = 64 * 1024;

const DETAIL_CHARS_MAX: usize = 120;

const OSC_TITLE_BYTES_MAX: usize = 128;

const OSC_MESSAGE_BYTES_MAX: usize = 256;

const CHILD_WAIT_POLL: Duration = Duration::from_millis(50);

const CHILD_WAIT_MAX: Duration = Duration::from_secs(5);

const LINE_QUEUE_MAX: usize = 128;

const _: () = assert!(TIMEOUT_SECS_MIN <= DEFAULT_TIMEOUT_SECS);
const _: () = assert!(DEFAULT_TIMEOUT_SECS <= TIMEOUT_SECS_MAX);
const _: () = assert!(LINE_BYTES_MAX > 0);
const _: () = assert!(LINES_PER_TURN_MAX > 0);
const _: () = assert!(PROMPT_BYTES_MAX > 0);
const _: () = assert!(STDERR_BYTES_MAX > 0);
const _: () = assert!(DETAIL_CHARS_MAX > 0);
const _: () = assert!(OSC_TITLE_BYTES_MAX > 0);
const _: () = assert!(OSC_MESSAGE_BYTES_MAX > 0);
const _: () = assert!(LINE_QUEUE_MAX > 0);

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
    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    pub timeout_secs: u64,
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
    TurnFinished {
        ok: bool,
        summary: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TurnOutcome {
    session_id: Option<String>,
    ok: bool,
    summary: Option<String>,
}

enum LineItem {
    Line(String),
    Eof,
    Error(io::Error),
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
    let mut out = io::stdout().lock();
    let result = execute(
        harness,
        stage,
        &prompt,
        followup.as_deref(),
        args.pi_session.as_deref(),
        args.session_out.as_deref(),
        timeout,
        &mut out,
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            let _ = writeln!(out, "\nerror: {why}");
            let _ = write!(out, "{}", osc_7501_error(harness, stage, &why));
            let _ = out.flush();
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

fn execute(
    harness: Harness,
    stage: &str,
    prompt: &str,
    followup: Option<&str>,
    pi_session: Option<&Path>,
    session_out: Option<&Path>,
    turn_timeout: Duration,
    out: &mut impl Write,
) -> Result<(), String> {
    assert!(!stage.is_empty(), "stage label is non-empty");
    assert!(!prompt.trim().is_empty(), "prompt is non-empty");
    let initial_pi_session = if harness == Harness::Pi {
        pi_session.map(|path| path.to_string_lossy().into_owned())
    } else {
        None
    };
    if let (Some(out_path), Some(session_path)) = (session_out, initial_pi_session.as_deref()) {
        write_session_id(out_path, session_path)?;
    }
    let _ = writeln!(out, "═══ {stage} ({}) ═══", harness.program());
    let _ = write!(out, "{}", osc_7501_working(harness, stage));
    let _ = out.flush();

    let turn1 = run_turn(
        harness,
        prompt,
        initial_pi_session.as_deref(),
        session_out,
        initial_pi_session.is_none(),
        turn_timeout,
        out,
    )?;
    if !turn1.ok {
        let why = turn1
            .summary
            .unwrap_or_else(|| format!("{} turn 1 failed", harness.program()));
        return Err(why);
    }

    let resume_handle = initial_pi_session.or(turn1.session_id);
    if let Some(followup) = followup {
        let Some(handle) = resume_handle.as_deref() else {
            return Err(format!(
                "{} emitted no session_id on turn 1; cannot resume for turn 2",
                harness.program()
            ));
        };
        let _ = writeln!(out, "\n{ACTING_DIVIDER}");
        let _ = write!(out, "{}", osc_7501_working(harness, stage));
        let _ = out.flush();
        let turn2 = run_turn(
            harness,
            followup,
            Some(handle),
            session_out,
            false,
            turn_timeout,
            out,
        )?;
        if !turn2.ok {
            let why = turn2
                .summary
                .unwrap_or_else(|| format!("{} turn 2 failed", harness.program()));
            return Err(why);
        }
        let summary = turn2
            .summary
            .unwrap_or_else(|| format!("{stage} completed"));
        let _ = write!(out, "{}", osc_7501_done(harness, stage, &summary));
        let _ = out.flush();
    } else {
        let summary = turn1
            .summary
            .unwrap_or_else(|| format!("{stage} completed"));
        let _ = write!(out, "{}", osc_7501_done(harness, stage, &summary));
        let _ = out.flush();
    }
    Ok(())
}

#[must_use]
pub fn build_command(harness: Harness, prompt: &str, resume_handle: Option<&str>) -> Command {
    let mut cmd = Command::new(harness.program());
    match harness {
        Harness::ClaudeCode => {
            cmd.args([
                "-p",
                "--output-format",
                "stream-json",
                "--verbose",
                "--dangerously-skip-permissions",
            ]);
            if let Some(id) = resume_handle {
                cmd.args(["--resume", id]);
            }
            cmd.args(["--", prompt]);
        }
        Harness::Antigravity => {
            cmd.args([
                "-p",
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
            ]);
            if let Some(id) = resume_handle {
                cmd.args(["--resume", id]);
            }
            cmd.args(["--", prompt]);
        }
        Harness::Pi => {
            cmd.args(["--mode", "json"]);
            if let Some(session) = resume_handle {
                cmd.args(["--session", session]);
            }
            cmd.args(["-p", "--", prompt]);
        }
    }
    luned::local_git(&mut cmd);
    cmd.env_remove("LUNE_MUX_TERMINAL")
        .env_remove("LUNE_MUX_TERMINAL_MADE")
        .env_remove("OSC7577_TOKEN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run_turn(
    harness: Harness,
    prompt: &str,
    resume_handle: Option<&str>,
    session_out: Option<&Path>,
    record_stream_session: bool,
    timeout: Duration,
    out: &mut impl Write,
) -> Result<TurnOutcome, String> {
    let mut cmd = build_command(harness, prompt, resume_handle);
    let mut child = cmd
        .spawn()
        .map_err(|error| format!("{} could not be started: {error}", harness.program()))?;
    let stdout = child.stdout.take().expect("spawned child has piped stdout");
    let stderr = child.stderr.take().expect("spawned child has piped stderr");

    let (line_tx, line_rx) = mpsc::sync_channel::<LineItem>(LINE_QUEUE_MAX);
    let stdout_thread = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        for _ in 0..LINES_PER_TURN_MAX {
            match read_line_bounded(&mut reader, LINE_BYTES_MAX) {
                Ok(Some(line)) => {
                    if line_tx.send(LineItem::Line(line)).is_err() {
                        return;
                    }
                }
                Ok(None) => {
                    let _ = line_tx.send(LineItem::Eof);
                    return;
                }
                Err(error) => {
                    let _ = line_tx.send(LineItem::Error(error));
                    return;
                }
            }
        }
        let _ = line_tx.send(LineItem::Error(io::Error::other(format!(
            "output exceeded {LINES_PER_TURN_MAX} lines"
        ))));
    });

    let stderr_thread = thread::spawn(move || {
        let mut bytes = Vec::new();
        let cap = u64::try_from(STDERR_BYTES_MAX).expect("usize fits u64");
        let _ = stderr.take(cap).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).trim().to_owned()
    });

    let deadline = Instant::now() + timeout;
    let mut renderer = Renderer::default();
    let mut session_id: Option<String> = None;
    let mut terminal_result: Option<(bool, Option<String>)> = None;
    let mut stream_failure: Option<String> = None;

    for _ in 0..=LINES_PER_TURN_MAX {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            kill_child(&mut child);
            let _ = stdout_thread.join();
            let _ = stderr_thread.join();
            return Err(format!(
                "{} timed out after {}s",
                harness.program(),
                timeout.as_secs()
            ));
        };
        match line_rx.recv_timeout(remaining) {
            Ok(LineItem::Line(line)) => {
                let events = parse_line(harness, &line);
                if events.is_empty() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('{') {
                        renderer.write_block(out, trimmed);
                    }
                }
                for event in events {
                    match event {
                        StreamEvent::SessionId(id) => {
                            if session_id.is_none() {
                                if record_stream_session
                                    && let Some(path) = session_out
                                    && let Err(why) = write_session_id(path, &id)
                                {
                                    stream_failure = Some(why);
                                }
                                session_id = Some(id);
                            }
                        }
                        StreamEvent::TextDelta(delta) => renderer.write_delta(out, &delta),
                        StreamEvent::TextBlock(text) => renderer.write_block(out, &text),
                        StreamEvent::ToolUse { name, detail } => {
                            renderer.write_tool(out, &name, detail.as_deref());
                        }
                        StreamEvent::ToolResult { name, is_error } => {
                            if is_error {
                                let label = name.as_deref().unwrap_or("tool");
                                renderer.write_note(out, &format!("✗ {label} failed"));
                            }
                        }
                        StreamEvent::TurnFinished { ok, summary } => {
                            terminal_result = Some((ok, summary));
                        }
                    }
                }
            }
            Ok(LineItem::Eof) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(LineItem::Error(error)) => {
                stream_failure = Some(format!("{} stream error: {error}", harness.program()));
                kill_child(&mut child);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                kill_child(&mut child);
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(format!(
                    "{} timed out after {}s",
                    harness.program(),
                    timeout.as_secs()
                ));
            }
        }
    }

    renderer.finish(out);
    let _ = stdout_thread.join();
    let status = wait_child(&mut child, deadline)?;
    let stderr_text = stderr_thread.join().unwrap_or_default();

    if let Some(why) = stream_failure {
        return Err(why);
    }

    let (ok, summary) = interpret_turn(harness, status, terminal_result, &stderr_text);
    Ok(TurnOutcome {
        session_id,
        ok,
        summary,
    })
}

fn interpret_turn(
    harness: Harness,
    status: ExitStatus,
    terminal_result: Option<(bool, Option<String>)>,
    stderr_text: &str,
) -> (bool, Option<String>) {
    match (status.success(), terminal_result) {
        (true, Some((true, summary))) => (true, summary),
        (true, None) => (true, None),
        (true, Some((false, summary))) => {
            let why = summary
                .or_else(|| (!stderr_text.is_empty()).then(|| stderr_text.to_owned()))
                .unwrap_or_else(|| format!("{} reported an error", harness.program()));
            (false, Some(why))
        }
        (false, Some((_, summary))) => {
            let why = summary
                .or_else(|| (!stderr_text.is_empty()).then(|| stderr_text.to_owned()))
                .unwrap_or_else(|| format!("{} exited with {status}", harness.program()));
            (false, Some(why))
        }
        (false, None) => {
            let why = if stderr_text.is_empty() {
                format!("{} exited with {status}", harness.program())
            } else {
                stderr_text.to_owned()
            };
            (false, Some(why))
        }
    }
}

fn wait_child(child: &mut Child, deadline: Instant) -> Result<ExitStatus, String> {
    let wait_until = deadline.min(Instant::now() + CHILD_WAIT_MAX);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if Instant::now() >= wait_until {
                    kill_child(child);
                    return child
                        .wait()
                        .map_err(|error| format!("failed to wait on child: {error}"));
                }
                thread::sleep(CHILD_WAIT_POLL);
            }
            Err(error) => return Err(format!("failed to poll child status: {error}")),
        }
    }
}

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn write_session_id(path: &Path, id: &str) -> Result<(), String> {
    assert!(!id.trim().is_empty(), "session id written is non-empty");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let written = fs::write(&tmp, format!("{id}\n")).and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
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

#[derive(Default)]
struct Renderer {
    mid_line: bool,
    streamed_delta: bool,
}

impl Renderer {
    fn write_delta(&mut self, out: &mut impl Write, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let _ = write!(out, "{delta}");
        let _ = out.flush();
        self.mid_line = !delta.ends_with('\n');
        self.streamed_delta = true;
    }

    fn write_block(&mut self, out: &mut impl Write, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.streamed_delta {
            self.streamed_delta = false;
            if self.mid_line {
                let _ = writeln!(out);
                self.mid_line = false;
            }
            let _ = out.flush();
            return;
        }
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }

    fn write_tool(&mut self, out: &mut impl Write, name: &str, detail: Option<&str>) {
        self.streamed_delta = false;
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
        match detail {
            Some(detail) if !detail.is_empty() => {
                let _ = writeln!(out, "▸ {name}: {detail}");
            }
            _ => {
                let _ = writeln!(out, "▸ {name}");
            }
        }
        let _ = out.flush();
    }

    fn write_note(&mut self, out: &mut impl Write, note: &str) {
        self.streamed_delta = false;
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
        let _ = writeln!(out, "{note}");
        let _ = out.flush();
    }

    fn finish(&mut self, out: &mut impl Write) {
        if self.mid_line {
            let _ = writeln!(out);
            self.mid_line = false;
        }
        let _ = out.flush();
    }
}

#[must_use]
pub fn parse_line(harness: Harness, line: &str) -> Vec<StreamEvent> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match harness {
        Harness::ClaudeCode => parse_claude(&record),
        Harness::Antigravity => parse_agy(&record),
        Harness::Pi => parse_pi(&record),
    }
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
                .flatten();
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
            events.push(StreamEvent::TurnFinished {
                ok: !is_error,
                summary,
            });
        }
        _ => {}
    }
    events
}

fn parse_agy(record: &Value) -> Vec<StreamEvent> {
    let mut events = Vec::new();
    if let Some(id) =
        nonempty_str(&record["session_id"]).or_else(|| nonempty_str(&record["conversation_id"]))
    {
        events.push(StreamEvent::SessionId(id.to_owned()));
    }
    match record["type"].as_str() {
        Some("step_update") => {
            if let Some(delta) = record["delta"].as_str().filter(|s| !s.is_empty()) {
                events.push(StreamEvent::TextDelta(delta.to_owned()));
            } else if let Some(text) =
                nonempty_str(&record["text"]).or_else(|| nonempty_str(&record["response"]))
            {
                events.push(StreamEvent::TextBlock(text.to_owned()));
            }
            if let Some(name) = nonempty_str(&record["tool_name"])
                .or_else(|| nonempty_str(&record["step_type"]))
                .filter(|name| !is_text_step_type(name))
            {
                let detail = extract_tool_detail(&record["tool_info"])
                    .or_else(|| extract_tool_detail(record));
                events.push(StreamEvent::ToolUse {
                    name: name.to_owned(),
                    detail,
                });
            }
        }
        Some("text_delta" | "delta") => {
            if let Some(delta) = record["delta"]
                .as_str()
                .or_else(|| record["text"].as_str())
                .filter(|s| !s.is_empty())
            {
                events.push(StreamEvent::TextDelta(delta.to_owned()));
            }
        }
        Some("assistant") => {
            let blocks = record["message"]["content"]
                .as_array()
                .or_else(|| record["content"].as_array())
                .into_iter()
                .flatten();
            let before = events.len();
            for block in blocks {
                push_content_block(&mut events, block);
            }
            if events.len() == before
                && let Some(text) = nonempty_str(&record["text"])
            {
                events.push(StreamEvent::TextBlock(text.to_owned()));
            }
        }
        Some("tool_use") => {
            if let Some(name) =
                nonempty_str(&record["tool_name"]).or_else(|| nonempty_str(&record["name"]))
            {
                let detail = extract_tool_detail(&record["input"])
                    .or_else(|| extract_tool_detail(&record["tool_info"]))
                    .or_else(|| extract_tool_detail(record));
                events.push(StreamEvent::ToolUse {
                    name: name.to_owned(),
                    detail,
                });
            }
        }
        Some("result") => {
            let status_err = record["status"].as_str().is_some_and(|status| {
                matches!(
                    status.to_ascii_lowercase().as_str(),
                    "error" | "failed" | "cancelled" | "canceled" | "interrupted"
                )
            });
            let finished_err = record["finished_reason"].as_str().is_some_and(|reason| {
                !reason.is_empty()
                    && !matches!(
                        reason.to_ascii_lowercase().as_str(),
                        "normal" | "stop" | "complete" | "success"
                    )
            });
            let subtype_err = record["subtype"].as_str().is_some_and(|s| s != "success");
            let is_error = record["is_error"].as_bool() == Some(true)
                || status_err
                || finished_err
                || subtype_err;
            let summary = if is_error {
                nonempty_str(&record["error"])
                    .or_else(|| nonempty_str(&record["finished_reason"]))
                    .or_else(|| nonempty_str(&record["result"]))
                    .or_else(|| nonempty_str(&record["response"]))
                    .or_else(|| nonempty_str(&record["status"]))
                    .map(str::to_owned)
            } else {
                nonempty_str(&record["response"])
                    .or_else(|| nonempty_str(&record["result"]))
                    .or_else(|| nonempty_str(&record["text"]))
                    .map(str::to_owned)
            };
            events.push(StreamEvent::TurnFinished {
                ok: !is_error,
                summary,
            });
        }
        Some("error") => {
            let summary = nonempty_str(&record["error"])
                .or_else(|| nonempty_str(&record["message"]))
                .map(str::to_owned);
            events.push(StreamEvent::TurnFinished { ok: false, summary });
        }
        _ => {}
    }
    events
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
                    events.push(StreamEvent::TurnFinished { ok: false, summary });
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
                    events.push(StreamEvent::TurnFinished { ok: false, summary });
                } else if let Some(blocks) = message["content"].as_array() {
                    for block in blocks {
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
        Some("auto_retry_end") => {
            if record["success"].as_bool() == Some(false) {
                let summary = nonempty_str(&record["finalError"]).map(str::to_owned);
                events.push(StreamEvent::TurnFinished { ok: false, summary });
            }
        }
        Some("agent_settled") => {
            let aborted = record["aborted"].as_bool() == Some(true);
            events.push(StreamEvent::TurnFinished {
                ok: !aborted,
                summary: aborted.then(|| "aborted".to_owned()),
            });
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

fn is_text_step_type(step_type: &str) -> bool {
    matches!(
        step_type.to_ascii_lowercase().as_str(),
        "text" | "thought" | "thinking" | "planner_response" | "response" | "assistant"
    )
}

fn extract_tool_detail(value: &Value) -> Option<String> {
    let raw = nonempty_str(&value["command"])
        .or_else(|| nonempty_str(&value["command_line"]))
        .or_else(|| nonempty_str(&value["file_path"]))
        .or_else(|| nonempty_str(&value["path"]))
        .or_else(|| nonempty_str(&value["pattern"]))
        .or_else(|| nonempty_str(&value["query"]))
        .or_else(|| nonempty_str(&value["url"]))?;
    Some(single_line_preview(raw, DETAIL_CHARS_MAX))
}

fn nonempty_str(value: &Value) -> Option<&str> {
    let text = value.as_str()?.trim();
    (!text.is_empty()).then_some(text)
}

fn single_line_preview(text: &str, chars_max: usize) -> String {
    let first_line = text.lines().next().unwrap_or(text).trim();
    let mut out = String::new();
    for (count, ch) in first_line.chars().enumerate() {
        if count >= chars_max {
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
    let mut end = bytes_max;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
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
                "-p",
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "--",
                "/tigerstyle"
            ]
            .map(OsStr::new)
        );

        let agy2 = build_command(Harness::Antigravity, "fix", Some("conv-9"));
        let args: Vec<&OsStr> = agy2.get_args().collect();
        assert_eq!(
            args,
            [
                "-p",
                "--output-format",
                "stream-json",
                "--dangerously-skip-permissions",
                "--resume",
                "conv-9",
                "--",
                "fix"
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
            vec![StreamEvent::TurnFinished {
                ok: true,
                summary: Some("All clean".to_owned()),
            }]
        );

        let result_err = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"result":"boom"}"#;
        assert_eq!(
            parse_line(Harness::ClaudeCode, result_err),
            vec![StreamEvent::TurnFinished {
                ok: false,
                summary: Some("boom".to_owned()),
            }]
        );
    }

    #[test]
    fn parses_agy_stream_json_events() {
        let init = r#"{"type":"init","conversation_id":"conv-42"}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, init),
            vec![StreamEvent::SessionId("conv-42".to_owned())]
        );

        let update_text =
            r#"{"type":"step_update","step_type":"planner_response","delta":"Finding 1"}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, update_text),
            vec![StreamEvent::TextDelta("Finding 1".to_owned())]
        );

        let update_tool = r#"{"type":"step_update","step_type":"run_command","tool_name":"run_command","tool_info":{"command_line":"cargo clippy"}}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, update_tool),
            vec![StreamEvent::ToolUse {
                name: "run_command".to_owned(),
                detail: Some("cargo clippy".to_owned()),
            }]
        );

        let result_ok = r#"{"type":"result","status":"SUCCESS","response":"Done"}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_ok),
            vec![StreamEvent::TurnFinished {
                ok: true,
                summary: Some("Done".to_owned()),
            }]
        );

        let result_timeout = r#"{"type":"result","status":"ERROR","finished_reason":"print_timeout","error":"timed out"}"#;
        assert_eq!(
            parse_line(Harness::Antigravity, result_timeout),
            vec![StreamEvent::TurnFinished {
                ok: false,
                summary: Some("timed out".to_owned()),
            }]
        );
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
            vec![StreamEvent::TurnFinished {
                ok: true,
                summary: None,
            }]
        );

        let aborted = r#"{"type":"agent_settled","aborted":true}"#;
        assert_eq!(
            parse_line(Harness::Pi, aborted),
            vec![StreamEvent::TurnFinished {
                ok: false,
                summary: Some("aborted".to_owned()),
            }]
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
}
