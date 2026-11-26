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
use rustix::io::Errno;
use rustix::pty::OpenptFlags;
use rustix::termios::{self, Winsize};
use serde_json::{Value, json};

const COLUMNS: u16 = 120;

const ROWS: u16 = 40;

const PATIENCE: Duration = Duration::from_secs(120);

const TICK: Duration = Duration::from_millis(100);

const TYPED_GAP: Duration = Duration::from_millis(400);

const SEQUENCE_BYTES_MAX: usize = 8192;

const HEARD_BYTES_MAX: usize = 64 * 1024;

const PHASES_MAX: usize = 4096;

const READS_QUEUED_MAX: usize = 64;

const READ_BYTES_MAX: usize = 64 * 1024;

const ANCESTORS_MAX: usize = 64;

const IDENTIFIER_BYTES_MIN: usize = 3;

const OSC_7501_KEPT: [&str; 5] = ["state", "kind", "app", "progress", "id"];

const OSC_7501_REPLACED: [&str; 2] = ["msg", "title"];

const RECORDED_IN_BASE64: &str = "cmVjb3JkZWQ=";

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

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

fn started() -> Move {
    Move::to(vec![], Want::Resting, Some(Step::Start), seconds(2))
}

fn prompted() -> Vec<Move> {
    vec![
        Move::to(vec![Input::Line(STORY)], Want::Working, Some(Step::Prompt), seconds(1)),
        Move::to(vec![], Want::Done, Some(Step::Finish), seconds(5)),
    ]
}

fn asked_and_rejected() -> Vec<Move> {
    vec![
        Move::to(
            vec![Input::Line(TOUCH)],
            Want::Blocked(OscBlockedKind::Permission),
            Some(Step::Permission),
            seconds(2),
        ),
        Move::to(vec![Input::Keys(ESC)], Want::Resting, Some(Step::Reject), seconds(2)),
    ]
}

fn asked(question: &'static str) -> Vec<Move> {
    vec![
        Move::to(
            vec![Input::Line(question)],
            Want::Blocked(OscBlockedKind::Question),
            Some(Step::Question),
            seconds(2),
        ),
        Move::to(vec![Input::Keys(ESC)], Want::Settled, None, seconds(2)),
    ]
}

fn interrupted() -> Vec<Move> {
    vec![
        Move::to(vec![Input::Line(COUNT)], Want::Working, None, seconds(3)),
        Move::to(vec![Input::Keys(ESC)], Want::Resting, Some(Step::Interrupt), seconds(2)),
    ]
}

fn exited(command: &'static str, records: bool) -> Move {
    Move::to(vec![Input::Line(command)], Want::Ended, records.then_some(Step::Exit), seconds(1))
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
            Move::to(vec![], Want::Resting, None, seconds(2)),
            Move::to(vec![Input::Line("Say hi.")], Want::Error, Some(Step::Error), seconds(3)),
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
    overflowed: bool,
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
            self.outside(byte);
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
            self.escaped = true;
            self.outside(byte);
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

    fn outside(&mut self, byte: u8) {
        assert!(self.open.is_none());
        if self.escaped && byte == b']' {
            self.open = Some(b"\x1b]".to_vec());
        }
        self.escaped = byte == 0x1b;
    }

    fn close(&mut self) {
        let Some(sequence) = self.open.take() else {
            return;
        };
        assert!(sequence.starts_with(b"\x1b]"));
        assert!(sequence.len() <= SEQUENCE_BYTES_MAX + 2);
        let Some(kept) = scrubbed(&sequence) else {
            return;
        };
        if self.heard.len() + kept.len() > HEARD_BYTES_MAX {
            self.overflowed = true;
            return;
        }
        self.heard.extend_from_slice(&kept);
        assert!(self.heard.len() <= HEARD_BYTES_MAX);
    }

    fn take(&mut self) -> Result<String, String> {
        let heard = std::mem::take(&mut self.heard);
        if std::mem::take(&mut self.overflowed) {
            let why = format!("the agent wrote over {HEARD_BYTES_MAX} bytes of records in a move");
            return Err(why);
        }
        String::from_utf8(heard).map_err(|error| format!("the agent's records: {error}"))
    }
}

fn scrubbed(sequence: &[u8]) -> Option<Vec<u8>> {
    let (body, end): (&[u8], &[u8]) = if let Some(body) = sequence.strip_suffix(b"\x1b\\") {
        (body, b"\x1b\\")
    } else if let Some(body) = sequence.strip_suffix(b"\x07") {
        (body, b"\x07")
    } else {
        unreachable!("a sequence closes on ST or BEL")
    };
    if let Some(fields) = body.strip_prefix(b"\x1b]7501;") {
        if fields.starts_with(b"?") {
            return None;
        }
        let fields = std::str::from_utf8(fields).ok()?;
        let kept: Vec<String> = fields
            .split(':')
            .filter_map(|field| {
                let (key, value) = field.split_once('=')?;
                if OSC_7501_KEPT.contains(&key) {
                    Some(format!("{key}={value}"))
                } else if OSC_7501_REPLACED.contains(&key) {
                    Some(format!("{key}={RECORDED_IN_BASE64}"))
                } else {
                    None
                }
            })
            .collect();
        return Some([b"\x1b]7501;", kept.join(":").as_bytes(), end].concat());
    }
    let fields = body.strip_prefix(b"\x1b]133;")?;
    let mut parts = fields.split(|byte| *byte == b';');
    let mark = parts.next()?;
    let [letter] = mark else {
        return None;
    };
    if !letter.is_ascii_uppercase() {
        return None;
    }
    let mut kept = [b"\x1b]133;", mark].concat();
    if let Some(code) = parts.next()
        && *letter == b'D'
        && !code.is_empty()
        && code.iter().all(u8::is_ascii_digit)
    {
        kept.push(b';');
        kept.extend_from_slice(code);
    }
    kept.extend_from_slice(end);
    Some(kept)
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
            if let Err(error) = self.0.kill() {
                eprintln!("{RECORDER}: stopping the agent: {error}");
            }
            if let Err(error) = self.0.wait() {
                eprintln!("{RECORDER}: waiting for the agent to stop: {error}");
            }
        }
    }
}

struct Session {
    agent: Running,
    near: File,
    heard: Receiver<Vec<u8>>,
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
        let (tell, heard) = mpsc::sync_channel(READS_QUEUED_MAX);
        thread::spawn(move || {
            let mut buffer = vec![0; READ_BYTES_MAX];
            loop {
                match reading.read(&mut buffer) {
                    Ok(0) => return,
                    Ok(read) => {
                        if tell.send(buffer[..read].to_vec()).is_err() {
                            return;
                        }
                    },
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                    Err(error) if error.raw_os_error() == Some(Errno::IO.raw_os_error()) => {
                        return;
                    },
                    Err(error) => {
                        eprintln!("{RECORDER}: reading the agent's terminal: {error}");
                        return;
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
                Ok(bytes) => {
                    let mut ends = self.sequences.hear(&bytes);
                    ends.push(bytes.len());
                    let mut from = 0;
                    for end in ends {
                        self.show(&bytes[from..end])?;
                        from = end;
                    }
                },
                Err(RecvTimeoutError::Disconnected) => self.ended = true,
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
            if self.phases.len() >= PHASES_MAX {
                return Err(format!("the agent went through over {PHASES_MAX} phases"));
            }
            eprintln!("{RECORDER}: the agent is at {phase:?}");
            self.phases.push(phase);
        }
        assert!(self.phases.len() <= PHASES_MAX);
        Ok(())
    }

    fn answer_dialogs(&mut self) -> Result<(), String> {
        let text = self.screen.dumped_text().map_err(|error| format!("the screen: {error:?}"))?;
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
        let text = self
            .screen
            .dumped_text()
            .unwrap_or_else(|error| format!("(the screen does not dump: {error:?})"));
        let lines: Vec<&str> = text.lines().collect();
        lines[lines.len().saturating_sub(ROWS.into())..].join("\n")
    }
}

fn file_pid(folder: &Path, agent: u32) -> Result<Option<u32>, String> {
    if folder.join(format!("{agent}.json")).exists() {
        return Ok(Some(agent));
    }
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", folder.display())),
    };
    if !Path::new("/proc/self/stat").exists() {
        return Err(format!(
            "{}: finding the file of a process the agent started reads /proc, which this system lacks",
            folder.display()
        ));
    }
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", folder.display()))?;
        let path = entry.path();
        let Some(pid) = path.file_stem().and_then(OsStr::to_str).and_then(|stem| stem.parse().ok())
        else {
            continue;
        };
        if descends(pid, agent) {
            return Ok(Some(pid));
        }
    }
    Ok(None)
}

fn descends(pid: u32, from: u32) -> bool {
    let mut at = pid;
    for _ in 0..ANCESTORS_MAX {
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
        let said = match keep {
            Kept::As(with) => Value::String((*with).to_owned()),
            Kept::Said => found.clone(),
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
    for (path, keep) in keeps {
        if let Kept::As(with) = keep {
            let found = path.split('.').try_fold(&kept, |at, name| at.get(name));
            assert!(found.is_none_or(|found| found.as_str() == Some(*with)), "{path} is replaced");
        }
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
            if !session.wait(first.until, 0)? {
                return Err(unmet(&session, 0, first));
            }
        }
        session.answering = false;
        for (index, planned) in run.moves.iter().enumerate() {
            let from = if planned.inputs.is_empty() {
                session.phases.len().saturating_sub(1)
            } else {
                session.phases.len()
            };
            session.type_in(&planned.inputs)?;
            if !session.wait(planned.until, from)? {
                return Err(unmet(&session, index, planned));
            }
            let until = Instant::now() + planned.settle;
            while Instant::now() < until && !session.ended {
                session.pump(until)?;
            }
            for ((_, folder, _), pid) in recordings.files.iter().zip(&mut pids) {
                if pid.is_none() {
                    *pid = file_pid(folder, session.pid())?;
                }
            }
            let sequences = session.sequences.take()?;
            let Some(records) = planned.records else {
                continue;
            };
            eprintln!("{RECORDER}: recorded {} at {:?}", records.name(), session.phase());
            if harness.osc_7501_app().is_some() {
                recordings.osc_7501.insert(records.name().to_owned(), Value::String(sequences));
            }
            for ((rung, folder, steps), pid) in recordings.files.iter_mut().zip(&pids) {
                let Some(pid) = *pid else {
                    return Err(format!(
                        "{}: no file of agent {} tells {rung:?} at {}, so nothing is written",
                        folder.display(),
                        session.pid(),
                        records.name()
                    ));
                };
                let file = snapshot(&folder.join(format!("{pid}.json")))?;
                steps.insert(records.name().to_owned(), kept(&file, plan.keeps));
            }
        }
    }
    Ok(recordings)
}

fn unmet(session: &Session, index: usize, planned: &Move) -> String {
    format!(
        "move {index} waited {PATIENCE:?} for {:?} and the agent is at {:?}, so nothing is written; its screen:\n{}",
        planned.until,
        session.phase(),
        session.shown()
    )
}

fn fixture_text(agent: &str, on: &str, steps: &BTreeMap<String, Value>) -> Result<String, String> {
    let fixture =
        json!({ "recorded": { "by": RECORDER, "agent": agent, "on": on }, "steps": steps });
    let mut text = serde_json::to_string_pretty(&fixture).map_err(|error| error.to_string())?;
    text.push('\n');
    Ok(text)
}

fn identifiers() -> Result<Vec<String>, String> {
    let mut identifiers: Vec<String> = ["HOME", "USER", "LOGNAME"]
        .into_iter()
        .filter_map(|name| std::env::var_os(name)?.into_string().ok())
        .collect();
    identifiers.push(said_by("hostname", &[])?);
    identifiers.retain(|identifier| identifier.trim().len() >= IDENTIFIER_BYTES_MIN);
    Ok(identifiers)
}

fn refuse_identifiers(rung: Rung, text: &str, identifiers: &[String]) -> Result<(), String> {
    match identifiers.iter().find(|identifier| text.contains(identifier.as_str())) {
        Some(found) => Err(format!(
            "the {} fixture holds {found:?}, which names this machine or its user, so nothing is written",
            rung.name()
        )),
        None => Ok(()),
    }
}

fn write(harness: Harness, rung: Rung, text: &str) -> Result<(), String> {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/statuses/fixtures")
        .join(harness.program());
    fs::create_dir_all(&folder).map_err(|error| format!("{}: {error}", folder.display()))?;
    let path = folder.join(format!("{}.json", rung.name()));
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
    let identifiers = identifiers()?;
    let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cwd = match plan(harness)?.folder {
        Folder::Temporary => temporary.path().to_owned(),
        Folder::Kept => kept_folder("record-status")?,
    };
    fs::create_dir_all(&cwd).map_err(|error| format!("{}: {error}", cwd.display()))?;
    let recordings = record(harness, &launch, &cwd);
    let touched = cwd.join("lune-record-status.txt");
    let removed = match fs::remove_file(&touched) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", touched.display())),
    };
    let recordings = recordings?;
    removed?;
    let mut fixtures = Vec::with_capacity(recordings.files.len() + 1);
    if harness.osc_7501_app().is_some() {
        fixtures.push((Rung::Osc7501, fixture_text(&agent, &on, &recordings.osc_7501)?));
    }
    for (rung, _, steps) in &recordings.files {
        fixtures.push((*rung, fixture_text(&agent, &on, steps)?));
    }
    for (rung, text) in &fixtures {
        refuse_identifiers(*rung, text, &identifiers)?;
    }
    for (rung, text) in &fixtures {
        write(harness, *rung, text)?;
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
