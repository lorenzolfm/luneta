use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read as _, Write as _};
use std::num::NonZeroU16;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use lune_domain::agent::{Harness, Rung, Step};
use lune_mux::screen::{Screen, Size};
use lune_protocol::program_status::{OscBlockedKind, ProgramPhase};
use rustix::fs::{Mode, OFlags};
use rustix::pty::OpenptFlags;
use rustix::termios::{self, Winsize};
use serde_json::{Value, json};

const COLUMNS: u16 = 120;

const ROWS: u16 = 40;

const PATIENCE: Duration = Duration::from_secs(120);

const TICK: Duration = Duration::from_millis(100);

const TYPED_GAP: Duration = Duration::from_millis(400);

const SEQUENCE_BYTES_MAX: usize = 8192;

const ESC: &[u8] = b"\x1b";

const RECORDER: &str = "dev record-status";

#[derive(Clone, Copy, Debug)]
enum Want {
    Resting,
    Settled,
    Working,
    Blocked(OscBlockedKind),
    Done,
    Error,
    Ended,
}

enum Input {
    Line(&'static str),
    Keys(&'static [u8]),
}

struct Move {
    inputs: Vec<Input>,
    until: Want,
    records: Option<Step>,
    settle: Duration,
}

impl Move {
    fn to(inputs: Vec<Input>, until: Want, records: Option<Step>, settle: Duration) -> Move {
        Move { inputs, until, records, settle }
    }
}

struct Run {
    reaches_model: bool,
    model: &'static [&'static str],
    args: Vec<&'static str>,
    env: Vec<(&'static str, &'static str)>,
    moves: Vec<Move>,
}

#[derive(Clone, Copy)]
enum Kept {
    As(&'static str),
    Said,
}

#[derive(Clone, Copy)]
enum Folder {
    Temporary,
    Kept,
}

struct Plan {
    folder: Folder,
    args: Vec<String>,
    dialogs: &'static [(&'static str, &'static [u8])],
    keeps: &'static [(&'static str, Kept)],
    files: Vec<(Rung, PathBuf)>,
    runs: Vec<Run>,
}

const OFFLINE: [(&str, &str); 4] = [
    ("HTTPS_PROXY", "http://127.0.0.1:9"),
    ("https_proxy", "http://127.0.0.1:9"),
    ("HTTP_PROXY", "http://127.0.0.1:9"),
    ("http_proxy", "http://127.0.0.1:9"),
];

const STORY: &str = "Write a 400 word story about a lighthouse keeper. Use no tools.";

const COUNT: &str = "Count from 1 to 2000, one number per line. Use no tools.";

const TOUCH: &str = "Use your shell tool to run: touch lune-record-status.txt";

fn kept_folder(named: &str) -> Result<PathBuf, String> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|cache| cache.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .ok_or("neither XDG_CACHE_HOME nor HOME is set")?;
    Ok(cache.join("lune").join(named))
}

fn pi_extension() -> Result<Vec<String>, String> {
    let folder = kept_folder("record-status-pi")?;
    fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let file = folder.join("lune.ts");
    let text = luned::pi_extension(&lune_folder()?).map_err(|error| error.to_string())?;
    fs::write(&file, text).map_err(|error| format!("{}: {error}", file.display()))?;
    let path = file.to_str().ok_or("the cache folder is not UTF-8")?;
    Ok(vec!["--no-extensions".to_owned(), "--extension".to_owned(), path.to_owned()])
}

fn lune_folder() -> Result<PathBuf, String> {
    luned::folder::find().map_err(|error| error.to_string())
}

fn secs(count: u64) -> Duration {
    Duration::from_secs(count)
}

fn started() -> Move {
    Move::to(vec![], Want::Resting, Some(Step::Start), secs(2))
}

fn prompted() -> Vec<Move> {
    vec![
        Move::to(vec![Input::Line(STORY)], Want::Working, Some(Step::Prompt), secs(1)),
        Move::to(vec![], Want::Done, Some(Step::Finish), secs(5)),
    ]
}

fn asked_and_rejected() -> Vec<Move> {
    vec![
        Move::to(
            vec![Input::Line(TOUCH)],
            Want::Blocked(OscBlockedKind::Permission),
            Some(Step::Permission),
            secs(2),
        ),
        Move::to(vec![Input::Keys(ESC)], Want::Resting, Some(Step::Reject), secs(2)),
    ]
}

fn asked(question: &'static str) -> Vec<Move> {
    vec![
        Move::to(
            vec![Input::Line(question)],
            Want::Blocked(OscBlockedKind::Question),
            Some(Step::Question),
            secs(2),
        ),
        Move::to(vec![Input::Keys(ESC)], Want::Settled, None, secs(2)),
    ]
}

fn interrupted() -> Vec<Move> {
    vec![
        Move::to(vec![Input::Line(COUNT)], Want::Working, None, secs(3)),
        Move::to(vec![Input::Keys(ESC)], Want::Resting, Some(Step::Interrupt), secs(2)),
    ]
}

fn exited(command: &'static str, records: bool) -> Move {
    Move::to(vec![Input::Line(command)], Want::Ended, records.then_some(Step::Exit), secs(1))
}

fn failed(
    model: &'static [&'static str],
    args: Vec<&'static str>,
    env: Vec<(&'static str, &'static str)>,
    exit: &'static str,
) -> Run {
    Run {
        reaches_model: false,
        model,
        args,
        env,
        moves: vec![
            Move::to(vec![], Want::Resting, None, secs(2)),
            Move::to(vec![Input::Line("Say hi.")], Want::Error, Some(Step::Error), secs(3)),
            exited(exit, false),
        ],
    }
}

fn plan(harness: Harness) -> Result<Plan, String> {
    Ok(match harness {
        Harness::ClaudeCode => {
            let model = &["--model", "haiku"];
            let asks = ["--permission-mode", "default"];
            let mut moves = vec![started()];
            moves.extend(prompted());
            moves.extend(asked_and_rejected());
            moves.extend(asked(
                "Use the AskUserQuestion tool to ask me whether I prefer red or blue.",
            ));
            moves.extend(interrupted());
            moves.push(exited("/exit", true));
            Plan {
                folder: Folder::Temporary,
                args: vec![],
                dialogs: &[("Yes, I trust this folder", b"\r")],
                keeps: &[
                    ("sessionId", Kept::As("recorded")),
                    ("cwd", Kept::As("/recorded")),
                    ("procStart", Kept::As("1")),
                    ("pidDomain", Kept::As("recorded")),
                    ("status", Kept::Said),
                    ("waitingFor", Kept::Said),
                    ("statusUpdatedAt", Kept::Said),
                    ("startedAt", Kept::Said),
                    ("entrypoint", Kept::Said),
                    ("kind", Kept::Said),
                    ("version", Kept::Said),
                ],
                files: vec![(
                    Rung::Feed,
                    luned::claude_folder()
                        .ok_or("Claude Code's folder is unknown")?
                        .join("sessions"),
                )],
                runs: vec![
                    Run { reaches_model: true, model, args: asks.to_vec(), env: vec![], moves },
                    failed(&[], vec!["--model", "claude-no-such-model-0"], vec![], "/exit"),
                ],
            }
        },
        Harness::Pi => {
            let mut moves = vec![started()];
            moves.extend(prompted());
            moves.extend(interrupted());
            moves.push(exited("/quit", true));
            let model = &["--model", "anthropic/claude-haiku-4-5"];
            Plan {
                folder: Folder::Kept,
                args: pi_extension()?,
                dialogs: &[],
                keeps: &[
                    ("sessionId", Kept::As("recorded")),
                    ("cwd", Kept::As("/recorded")),
                    ("procStart", Kept::As("1")),
                    ("status", Kept::Said),
                    ("statusMessage", Kept::Said),
                    ("statusSince", Kept::Said),
                    ("mode", Kept::Said),
                    ("phase", Kept::Said),
                    ("promptOpen", Kept::Said),
                ],
                files: vec![(Rung::Extension, lune_folder()?.join("pi"))],
                runs: vec![
                    Run { reaches_model: true, model, args: vec![], env: vec![], moves },
                    failed(model, vec![], OFFLINE.to_vec(), "/quit"),
                ],
            }
        },
        Harness::Antigravity => {
            let mut moves = vec![started()];
            moves.extend(prompted());
            moves.extend(asked_and_rejected());
            moves.extend(asked(
                "Use your ask_question tool to ask me whether I prefer red or blue.",
            ));
            moves.extend(interrupted());
            moves.push(exited("/quit", true));
            let model = &["--model", "gemini-3.8-flash-medium"];
            let asks = ["--mode", "accept-edits"];
            Plan {
                folder: Folder::Kept,
                args: vec![],
                dialogs: &[("Yes, I trust this folder", b"\r"), ("esc to dismiss", ESC)],
                keeps: &[
                    ("procStart", Kept::As("1")),
                    ("state.cwd", Kept::As("/recorded")),
                    ("state.workspace.current_dir", Kept::As("/recorded")),
                    ("state.conversation_id", Kept::As("recorded")),
                    ("state.session_id", Kept::As("recorded")),
                    ("state.conversation_title", Kept::As("recorded")),
                    ("state.agent_state", Kept::Said),
                    ("state.tool_confirmation_pending", Kept::Said),
                ],
                files: vec![(Rung::Hook, lune_folder()?.join("agy"))],
                runs: vec![
                    Run { reaches_model: true, model, args: asks.to_vec(), env: vec![], moves },
                    failed(model, asks.to_vec(), OFFLINE.to_vec(), "/quit"),
                ],
            }
        },
    })
}

fn wanted(want: Want, phase: Option<&ProgramPhase>) -> bool {
    match (want, phase) {
        (Want::Resting, Some(ProgramPhase::Idle))
        | (Want::Working, Some(ProgramPhase::Working { .. }))
        | (Want::Done, Some(ProgramPhase::Done { .. }))
        | (Want::Error, Some(ProgramPhase::Error { .. }))
        | (Want::Settled, Some(ProgramPhase::Idle | ProgramPhase::Done { .. })) => true,
        (Want::Blocked(wanted), Some(ProgramPhase::Blocked { kind, .. })) => *kind == Some(wanted),
        _ => false,
    }
}

#[derive(Default)]
struct Sequences {
    open: Option<Vec<u8>>,
    escaped: bool,
    heard: Vec<u8>,
}

impl Sequences {
    fn hear(&mut self, bytes: &[u8]) -> Vec<usize> {
        let mut ends = Vec::new();
        for (index, &byte) in bytes.iter().enumerate() {
            let before = self.heard.len();
            self.byte(byte);
            if self.heard.len() > before {
                ends.push(index + 1);
            }
        }
        ends
    }

    fn byte(&mut self, byte: u8) {
        let Some(open) = self.open.as_mut() else {
            if self.escaped && byte == b']' {
                self.open = Some(b"\x1b]".to_vec());
            }
            self.escaped = byte == 0x1b;
            return;
        };
        if self.escaped {
            self.escaped = false;
            if byte == b'\\' {
                open.extend_from_slice(b"\x1b\\");
                self.close();
                return;
            }
            self.open = None;
            self.escaped = false;
            self.byte(0x1b);
            self.byte(byte);
            return;
        }
        match byte {
            0x07 => {
                open.push(byte);
                self.close();
            },
            0x1b => self.escaped = true,
            _ if open.len() >= SEQUENCE_BYTES_MAX => self.open = None,
            _ => open.push(byte),
        }
    }

    fn close(&mut self) {
        let Some(sequence) = self.open.take() else {
            return;
        };
        let status = sequence.starts_with(b"\x1b]7501;") && !sequence.starts_with(b"\x1b]7501;?");
        if status || sequence.starts_with(b"\x1b]133;") {
            self.heard.extend_from_slice(&sequence);
        }
    }

    fn take(&mut self) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut self.heard)).into_owned()
    }
}

struct Pty {
    near: File,
    far: OwnedFd,
}

fn opened() -> io::Result<Pty> {
    let near = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::pty::grantpt(&near)?;
    rustix::pty::unlockpt(&near)?;
    let path = rustix::pty::ptsname(&near, Vec::new())?;
    let path = Path::new(OsStr::from_bytes(path.as_bytes()));
    let far =
        rustix::fs::open(path, OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC, Mode::empty())?;
    rustix::io::fcntl_setfd(&near, rustix::io::FdFlags::CLOEXEC)?;
    let size = Winsize { ws_row: ROWS, ws_col: COLUMNS, ws_xpixel: 0, ws_ypixel: 0 };
    termios::tcsetwinsize(&far, size)?;
    Ok(Pty { near: File::from(near), far })
}

#[expect(
    unsafe_code,
    reason = "std has no safe way to start a command in a session of its own on a terminal. The \
              closure runs in the child, between its fork and its exec, where only what a signal \
              handler may do is safe: `setsid` and the ioctl are system calls alone, and their \
              errors hold no memory. Stdin is the terminal there, as std set it up first"
)]
fn leads(command: &mut Command) -> &mut Command {
    unsafe {
        command.pre_exec(|| {
            rustix::process::setsid()?;
            rustix::process::ioctl_tiocsctty(BorrowedFd::borrow_raw(0))?;
            Ok(())
        });
    }
    command
}

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

struct Session {
    agent: Running,
    near: File,
    heard: Receiver<Option<Vec<u8>>>,
    screen: Screen,
    sequences: Sequences,
    dialogs: &'static [(&'static str, &'static [u8])],
    answered: Vec<bool>,
    answering: bool,
    phases: Vec<Option<ProgramPhase>>,
    ended: bool,
}

impl Session {
    fn start(
        harness: Harness,
        launch: &Launch,
        plan: &Plan,
        run: &Run,
        cwd: &Path,
    ) -> Result<Session, String> {
        let pty = opened().map_err(|error| format!("a terminal: {error}"))?;
        let stdio = || pty.far.try_clone().map(Stdio::from);
        let mut command = launch.command(harness, run);
        command.args(&plan.args);
        command.current_dir(cwd);
        for (name, _) in std::env::vars_os() {
            let Some(name) = name.to_str() else {
                continue;
            };
            let an_agents = name.starts_with("CLAUDE") && name != "CLAUDE_CONFIG_DIR";
            let a_terminals = ["LUNE_MUX_", "TERM_PROGRAM", "TMUX", "ZELLIJ", "PI_PROGRAM_STATUS"]
                .iter()
                .any(|prefix| name.starts_with(prefix));
            if an_agents || a_terminals {
                command.env_remove(name);
            }
        }
        command.env("TERM", "xterm-256color").env("COLORTERM", "truecolor");
        command.envs(run.env.iter().copied());
        let child = (|| {
            command.stdin(stdio()?).stdout(stdio()?).stderr(stdio()?);
            leads(&mut command).spawn()
        })()
        .map_err(|error| format!("starting {}: {error}", harness.program()))?;
        let Pty { near, far } = pty;
        drop(far);
        let mut reading = near.try_clone().map_err(|error| error.to_string())?;
        let (tell, heard) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = vec![0; 64 * 1024];
            loop {
                match reading.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = tell.send(None);
                        return;
                    },
                    Ok(read) => {
                        if tell.send(Some(buffer[..read].to_vec())).is_err() {
                            return;
                        }
                    },
                }
            }
        });
        let side = |count| NonZeroU16::new(count).expect("the recorder's sides are not 0");
        let screen = Screen::new(Size { columns: side(COLUMNS), rows: side(ROWS) }, 0)
            .map_err(|error| format!("a screen: {error:?}"))?;
        Ok(Session {
            agent: Running(child),
            near,
            heard,
            screen,
            sequences: Sequences::default(),
            dialogs: plan.dialogs,
            answered: vec![false; plan.dialogs.len()],
            answering: true,
            phases: Vec::new(),
            ended: false,
        })
    }

    fn pid(&self) -> u32 {
        self.agent.0.id()
    }

    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.near.write_all(bytes).map_err(|error| format!("typing: {error}"))
    }

    fn pump(&mut self, until: Instant) -> Result<(), String> {
        loop {
            let now = Instant::now();
            if now >= until || self.ended {
                return Ok(());
            }
            match self.heard.recv_timeout((until - now).min(TICK)) {
                Ok(Some(bytes)) => {
                    let mut ends = self.sequences.hear(&bytes);
                    ends.push(bytes.len());
                    let mut from = 0;
                    for end in ends {
                        self.show(&bytes[from..end])?;
                        from = end;
                    }
                },
                Ok(None) | Err(RecvTimeoutError::Disconnected) => self.ended = true,
                Err(RecvTimeoutError::Timeout) => {
                    if self.answering {
                        self.answer_dialogs()?;
                    }
                    return Ok(());
                },
            }
        }
    }

    fn show(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let written = self.screen.write(bytes).map_err(|error| format!("{error:?}"))?;
        if !written.answers.is_empty() {
            self.send(written.answers.bytes())?;
        }
        let phase = self.phase();
        if self.phases.last() != Some(&phase) {
            eprintln!("{RECORDER}: the agent is at {phase:?}");
            self.phases.push(phase);
        }
        Ok(())
    }

    fn answer_dialogs(&mut self) -> Result<(), String> {
        let Ok(text) = self.screen.dumped_text() else {
            return Ok(());
        };
        for (index, (shown, keys)) in self.dialogs.iter().enumerate() {
            if !self.answered[index] && text.contains(shown) {
                self.answered[index] = true;
                eprintln!("{RECORDER}: answering {shown:?}");
                self.send(keys)?;
            }
        }
        Ok(())
    }

    fn phase(&self) -> Option<ProgramPhase> {
        self.screen.program_statuses().root.map(|root| root.phase)
    }

    fn has_ended(&mut self) -> bool {
        self.ended || matches!(self.agent.0.try_wait(), Ok(Some(_)))
    }

    fn wait(&mut self, want: Want, from: usize) -> Result<bool, String> {
        let until = Instant::now() + PATIENCE;
        while Instant::now() < until {
            let met = match want {
                Want::Ended => self.has_ended(),
                _ => self.phases[from..].iter().any(|phase| wanted(want, phase.as_ref())),
            };
            if met {
                return Ok(true);
            }
            self.pump(Instant::now() + TICK)?;
        }
        Ok(false)
    }

    fn type_in(&mut self, inputs: &[Input]) -> Result<(), String> {
        for input in inputs {
            match input {
                Input::Line(line) => {
                    self.send(line.as_bytes())?;
                    self.pump(Instant::now() + TYPED_GAP)?;
                    self.send(b"\r")?;
                },
                Input::Keys(keys) => self.send(keys)?,
            }
        }
        Ok(())
    }

    fn shown(&self) -> String {
        let text = self.screen.dumped_text().unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(ROWS.into())..].join("\n")
    }
}

fn file_pid(folder: &Path, agent: u32) -> Option<u32> {
    if folder.join(format!("{agent}.json")).exists() {
        return Some(agent);
    }
    let entries = fs::read_dir(folder).ok()?;
    entries
        .filter_map(|entry| entry.ok()?.path().file_stem()?.to_str()?.parse::<u32>().ok())
        .find(|&pid| descends(pid, agent))
}

fn descends(pid: u32, from: u32) -> bool {
    let mut at = pid;
    for _ in 0..64 {
        if at == from {
            return true;
        }
        let Ok(stat) = fs::read_to_string(format!("/proc/{at}/stat")) else {
            return false;
        };
        let Some(parent) = stat.rsplit(')').next().and_then(|rest| rest.split_whitespace().nth(1))
        else {
            return false;
        };
        let Ok(parent) = parent.parse::<u32>() else {
            return false;
        };
        if parent <= 1 {
            return false;
        }
        at = parent;
    }
    false
}

fn snapshot(path: &Path) -> Result<Value, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let mut values = serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>();
    match values.next() {
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(format!("{}: {error}", path.display())),
        None => Err(format!("{}: empty", path.display())),
    }
}

fn kept(value: &Value, keeps: &[(&str, Kept)]) -> Value {
    if value.is_null() {
        return Value::Null;
    }
    let mut kept = Value::Object(serde_json::Map::new());
    for (path, keep) in keeps {
        let names: Vec<&str> = path.split('.').collect();
        let Some(found) = names.iter().try_fold(value, |at, name| at.get(name)) else {
            continue;
        };
        let said = match (keep, found) {
            (Kept::As(with), Value::String(_)) => Value::String((*with).to_owned()),
            (Kept::As(_) | Kept::Said, found) => found.clone(),
        };
        let (last, parents) = names.split_last().expect("a kept path names a key");
        let mut at = &mut kept;
        for name in parents {
            at = at
                .as_object_mut()
                .expect("a kept path goes through objects")
                .entry((*name).to_owned())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
        }
        at.as_object_mut().expect("a kept path ends in an object").insert((*last).to_owned(), said);
    }
    kept
}

fn said_by(program: &str, args: &[&str]) -> Result<String, String> {
    let said = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("{program}: {error}"))?;
    let text = String::from_utf8_lossy(&said.stdout);
    let line = text.lines().next().unwrap_or_default().trim().to_owned();
    if line.is_empty() {
        Err(format!("{program} {} said nothing", args.join(" ")))
    } else {
        Ok(line)
    }
}

struct Launch {
    program: String,
    args: Vec<String>,
    picks_model: bool,
}

impl Launch {
    fn of(harness: Harness, launcher: &[String]) -> Launch {
        match launcher {
            [] => Launch { program: harness.program().to_owned(), args: vec![], picks_model: false },
            [program, args @ ..] => {
                Launch { program: program.clone(), args: args.to_vec(), picks_model: true }
            },
        }
    }

    fn command(&self, harness: Harness, run: &Run) -> Command {
        let launched = run.reaches_model && self.picks_model;
        let mut command =
            Command::new(if launched { self.program.as_str() } else { harness.program() });
        if launched {
            command.args(&self.args);
        } else {
            command.args(run.model);
        }
        command.args(&run.args);
        command
    }

    fn version(&self) -> Result<String, String> {
        let mut args: Vec<&str> = self.args.iter().map(String::as_str).collect();
        args.push("--version");
        said_by(&self.program, &args)
    }
}

struct Recordings {
    osc_7501: BTreeMap<String, Value>,
    files: Vec<(Rung, PathBuf, BTreeMap<String, Value>)>,
}

fn record(harness: Harness, launch: &Launch, cwd: &Path) -> Result<Recordings, String> {
    let plan = plan(harness)?;
    let mut declared: Vec<Rung> = plan.files.iter().map(|(rung, _)| *rung).collect();
    if harness.osc_7501_app().is_some() {
        declared.insert(0, Rung::Osc7501);
    }
    let ladder: Vec<Rung> = harness.rungs().collect();
    if declared != ladder {
        return Err(format!(
            "the plan for {harness:?} records {declared:?}, and its ladder declares {ladder:?}"
        ));
    }
    let mut recordings = Recordings {
        osc_7501: BTreeMap::new(),
        files: plan
            .files
            .iter()
            .map(|(rung, folder)| (*rung, folder.clone(), BTreeMap::new()))
            .collect(),
    };
    for run in &plan.runs {
        let mut session = Session::start(harness, launch, &plan, run, cwd)?;
        let mut pids: Vec<Option<u32>> = vec![None; recordings.files.len()];
        let first = run.moves.first().expect("a run makes a move");
        let until = Instant::now() + PATIENCE;
        let mut answered = None;
        while Instant::now() < until
            && !session.phases.iter().any(|phase| wanted(first.until, phase.as_ref()))
        {
            session.pump(Instant::now() + TICK)?;
            if answered.is_none() && session.answered.iter().any(|answered| *answered) {
                answered = Some(Instant::now());
            }
            if answered.is_some_and(|at| at.elapsed() > TYPED_GAP * 10) {
                break;
            }
        }
        if answered.is_some() {
            eprintln!(
                "{RECORDER}: starting {} again, past the dialogs it showed",
                harness.program()
            );
            drop(session);
            session = Session::start(harness, launch, &plan, run, cwd)?;
            session.wait(first.until, 0)?;
        }
        session.answering = false;
        for (index, step) in run.moves.iter().enumerate() {
            let from = session.phases.len().saturating_sub(1);
            session.type_in(&step.inputs)?;
            let met = session.wait(step.until, from)?;
            if !met {
                eprintln!(
                    "{RECORDER}: move {index} waited {PATIENCE:?} for {:?} and the agent is at {:?}; its screen:\n{}",
                    step.until,
                    session.phase(),
                    session.shown()
                );
            }
            let until = Instant::now() + step.settle;
            while Instant::now() < until && !session.ended {
                session.pump(until)?;
            }
            for ((_, folder, _), pid) in recordings.files.iter().zip(&mut pids) {
                if pid.is_none() {
                    *pid = file_pid(folder, session.pid());
                }
            }
            let sequences = session.sequences.take();
            let Some(records) = step.records else {
                continue;
            };
            if !met {
                eprintln!("{RECORDER}: {} goes unrecorded", records.name());
                continue;
            }
            eprintln!("{RECORDER}: recorded {} at {:?}", records.name(), session.phase());
            if harness.osc_7501_app().is_some() {
                recordings.osc_7501.insert(records.name().to_owned(), Value::String(sequences));
            }
            for ((rung, folder, steps), pid) in recordings.files.iter_mut().zip(&pids) {
                let Some(pid) = *pid else {
                    eprintln!(
                        "{RECORDER}: {} has no file of agent {} for {rung:?}",
                        folder.display(),
                        session.pid()
                    );
                    continue;
                };
                let file = snapshot(&folder.join(format!("{pid}.json")))?;
                steps.insert(records.name().to_owned(), kept(&file, plan.keeps));
            }
        }
    }
    Ok(recordings)
}

fn write(
    harness: Harness,
    rung: Rung,
    agent: &str,
    on: &str,
    steps: &BTreeMap<String, Value>,
) -> Result<(), String> {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/statuses/fixtures")
        .join(harness.program());
    fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let path = folder.join(format!("{}.json", rung.name()));
    let fixture =
        json!({ "recorded": { "by": RECORDER, "agent": agent, "on": on }, "steps": steps });
    let mut text = serde_json::to_string_pretty(&fixture).map_err(|error| error.to_string())?;
    text.push('\n');
    fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    eprintln!("{RECORDER}: wrote {}", path.display());
    Ok(())
}

fn run(named: &str, launcher: &[String]) -> Result<(), String> {
    let Some(harness) = Harness::ALL.into_iter().find(|harness| harness.program() == named) else {
        let known: Vec<&str> = Harness::ALL.iter().map(|harness| harness.program()).collect();
        return Err(format!("{named:?} is no harness; say one of {}", known.join(", ")));
    };
    let launch = Launch::of(harness, launcher);
    let agent = format!("{} {}", harness.program(), launch.version()?);
    let on = said_by("date", &["-u", "+%F"])?;
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cwd = match plan(harness)?.folder {
        Folder::Temporary => temporary.path().to_owned(),
        Folder::Kept => kept_folder("record-status")?,
    };
    fs::create_dir_all(&cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
    let recordings = record(harness, &launch, &cwd);
    let _ = fs::remove_file(cwd.join("lune-record-status.txt"));
    let recordings = recordings?;
    if harness.osc_7501_app().is_some() {
        write(harness, Rung::Osc7501, &agent, &on, &recordings.osc_7501)?;
    }
    for (rung, _, steps) in recordings.files {
        write(harness, rung, &agent, &on, &steps)?;
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (named, launcher) = match args.as_slice() {
        [named] => (named, &[][..]),
        [named, dashes, launcher @ ..] if dashes == "--" && !launcher.is_empty() => {
            (named, launcher)
        },
        _ => {
            eprintln!("usage: {RECORDER} <harness> [-- <launcher> [args...]]");
            return ExitCode::from(2);
        },
    };
    match run(named, launcher) {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("{RECORDER}: {why}");
            ExitCode::FAILURE
        },
    }
}
