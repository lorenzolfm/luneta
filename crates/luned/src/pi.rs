use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lune_domain::agent::{Declared, Harness, Rung};
use lune_domain::mux::TerminalKey;
use lune_protocol::program_status::Message;
use lune_protocol::said::Said;
use lune_protocol::telemetry::{
    AgentTelemetry, AgentUsage, ContextWindow, TelemetryText, UsedPermille, cost_usd_to_micro_usd,
    strip_effort_suffix,
};
use serde::Deserialize;
use serde_json::Value;

use crate::agent::{
    Sighting, absolute, moment, named_pid, nonempty, read_file_text, terminal_or_trouble,
};
use crate::mux;
use crate::process::{self, AgentId};
use crate::statuses::{
    ActiveStatus, BlockedReason, Proof, ReportedAgent, RungUpdate, SOURCE_AGENTS_MAX,
};
use crate::watch::{self, Troubles};
use crate::{Directory, log};

const EXTENSION: &str = include_str!("pi/lune.ts");

const FOLDER_STRING: &str = "\"LUNE_FOLDER\"";

const EXTENSION_FILE: &str = "lune.ts";

const FOLDER: &str = "pi";

const DATED_LATE: Duration = Duration::from_secs(1);

pub(crate) const READS: Declared = Declared::of(Harness::Pi, Rung::Extension);

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "absolute")]
    cwd: Directory,
    status: String,
    status_message: Option<String>,
    status_since: u64,
    proc_start: Option<String>,
    mode: Option<String>,
    phase: Option<RawPhase>,
    prompt_open: Option<bool>,
    telemetry: Option<Value>,
}

fn parse_telemetry(raw: &Value) -> Option<AgentTelemetry> {
    let raw = raw.as_object()?;
    let effort = raw
        .get("effort")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
        .and_then(|text| TelemetryText::parse(&text));
    let model = raw
        .get("model")
        .and_then(Value::as_str)
        .map(|raw| match effort.as_ref() {
            Some(effort) => strip_effort_suffix(raw.trim(), effort.as_str()),
            None => raw.trim(),
        })
        .and_then(TelemetryText::parse);
    let context = raw.get("context").and_then(parse_context);
    let usage = raw
        .get("costUsd")
        .and_then(Value::as_f64)
        .and_then(cost_usd_to_micro_usd)
        .and_then(|micro_usd| AgentUsage::new(Vec::new(), Some(micro_usd)));
    AgentTelemetry::new(model, effort, context, usage)
}

fn parse_context(raw: &Value) -> Option<ContextWindow> {
    let raw = raw.as_object()?;
    let window_tokens = raw
        .get("windowTokens")
        .and_then(Value::as_u64)
        .and_then(NonZeroU64::new);
    let used_tokens = raw.get("usedTokens").and_then(Value::as_u64);
    let used_permille = raw
        .get("usedPercent")
        .and_then(Value::as_f64)
        .and_then(UsedPermille::of_percent)
        .or_else(|| Some(UsedPermille::of_ratio(used_tokens?, window_tokens?)))?;
    Some(ContextWindow {
        used_permille,
        window_tokens,
        used_tokens,
    })
}

#[derive(Deserialize)]
struct RawResting {
    status: String,
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum RawPhase {
    Resting { resting: RawResting },
    Running { outcome: RawResting },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PiStatus {
    Stopped,
    Working,
    Waiting,
    Done,
    Error { message: Option<Message> },
}

impl PiStatus {
    fn parse(word: &str, message: Option<&str>) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "stopped" | "idle" => Some(PiStatus::Stopped),
            "busy" => Some(PiStatus::Working),
            "waiting" => Some(PiStatus::Waiting),
            "done" => Some(PiStatus::Done),
            "error" => Some(PiStatus::Error {
                message: message.and_then(|text| Message::from_raw_bytes(text.as_bytes())),
            }),
            _ => None,
        }
    }

    fn into_active(self) -> ActiveStatus {
        match self {
            PiStatus::Stopped => ActiveStatus::Stopped,
            PiStatus::Working => ActiveStatus::Working,
            PiStatus::Waiting => ActiveStatus::Blocked {
                reason: BlockedReason::Unspecified,
                message: None,
            },
            PiStatus::Done => ActiveStatus::Done { message: None },
            PiStatus::Error { message } => ActiveStatus::Error { message },
        }
    }
}

fn parse_resting(raw: &RawResting) -> io::Result<PiStatus> {
    match raw.status.trim().to_ascii_lowercase().as_str() {
        "stopped" | "idle" => Ok(PiStatus::Stopped),
        "done" => Ok(PiStatus::Done),
        "error" => Ok(PiStatus::Error {
            message: raw
                .message
                .as_deref()
                .and_then(|text| Message::from_raw_bytes(text.as_bytes())),
        }),
        _ => Err(io::Error::new(
            ErrorKind::InvalidData,
            format!("unknown pi resting status {:?}", raw.status),
        )),
    }
}

fn project_phase(phase: &RawPhase, prompt_open: bool) -> io::Result<PiStatus> {
    match phase {
        RawPhase::Resting { resting } => {
            let resting = parse_resting(resting)?;
            Ok(if prompt_open {
                PiStatus::Waiting
            } else {
                resting
            })
        }
        RawPhase::Running { outcome } => {
            let _ = parse_resting(outcome)?;
            Ok(if prompt_open {
                PiStatus::Waiting
            } else {
                PiStatus::Working
            })
        }
    }
}

struct File {
    session_id: Said,
    cwd: Directory,
    status: PiStatus,
    status_since: SystemTime,
    proc_start: Option<process::Start>,
    entrypoint: Option<Said>,
    telemetry: Option<AgentTelemetry>,
}

impl File {
    fn parse(raw: RawFile) -> io::Result<Self> {
        let status =
            PiStatus::parse(&raw.status, raw.status_message.as_deref()).ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidData,
                    format!("unknown pi status {:?}", raw.status),
                )
            })?;
        match (raw.phase.as_ref(), raw.prompt_open) {
            (Some(phase), Some(prompt_open)) => {
                let projected = project_phase(phase, prompt_open)?;
                if projected != status {
                    return Err(io::Error::new(
                        ErrorKind::InvalidData,
                        "pi status disagrees with phase and promptOpen",
                    ));
                }
            }
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "phase and promptOpen must appear together",
                ));
            }
        }
        let proc_start = raw
            .proc_start
            .map(|value| {
                process::Start::spelled(value)
                    .ok_or_else(|| io::Error::new(ErrorKind::InvalidData, "procStart is empty"))
            })
            .transpose()?;
        let entrypoint = raw.mode.and_then(|mode| Said::new(mode.trim().to_owned()));
        let telemetry = raw.telemetry.as_ref().and_then(parse_telemetry);
        Ok(File {
            session_id: raw.session_id,
            cwd: raw.cwd,
            status,
            status_since: moment(raw.status_since),
            proc_start,
            entrypoint,
            telemetry,
        })
    }
}

#[derive(Clone, Copy)]
struct Process {
    started: SystemTime,
    terminal: Option<TerminalKey>,
}

pub struct AgentFiles {
    lune: PathBuf,
    dir: PathBuf,
    pi: Option<PathBuf>,
    processes: HashMap<AgentId, Process>,
    troubles: Troubles,
}

impl AgentFiles {
    #[must_use]
    pub fn new(lune: &Path, pi: Option<PathBuf>) -> Self {
        let dir = lune.join(FOLDER);
        if let Err(error) = fs::create_dir(&dir)
            && error.kind() != ErrorKind::AlreadyExists
        {
            log::say(&format!("{}: {error}", dir.display()));
        }
        AgentFiles {
            lune: lune.to_owned(),
            dir,
            pi,
            processes: HashMap::new(),
            troubles: Troubles::default(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn scan(&mut self) -> Vec<ReportedAgent> {
        if let Some(pi) = self.pi.as_ref().filter(|pi| pi.is_dir())
            && let Err(error) = install(pi, &self.lune)
        {
            self.troubles
                .say(format!("pi's extension in {}: {error}", pi.display()));
        }
        let mut paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles
                    .say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            }
        };
        if paths.len() > SOURCE_AGENTS_MAX {
            self.troubles.say(format!(
                "{}: {} files exceed the limit of {SOURCE_AGENTS_MAX}",
                self.dir.display(),
                paths.len()
            ));
            paths.sort();
            paths.truncate(SOURCE_AGENTS_MAX);
        }
        let mut processes = HashMap::with_capacity(paths.len());
        let mut agents = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let Some(start) = process::start(pid.get()) else {
                continue;
            };
            let id = AgentId::new(pid, start);
            let cached = self.processes.remove(&id);
            let Some(started) = cached
                .map(|it| it.started)
                .or_else(|| process::started(pid.get()))
            else {
                continue;
            };
            match read(&path, id.start(), started) {
                Ok(Some((file, proof))) => {
                    let terminal = cached.map_or_else(
                        || {
                            terminal_or_trouble(
                                pid,
                                mux::terminal_of(&id.to_process_id()),
                                &mut self.troubles,
                            )
                        },
                        |it| it.terminal,
                    );
                    let found = Process { started, terminal };
                    let built = reported(id.clone(), file, found, proof);
                    processes.insert(id, found);
                    agents.push(built);
                }
                Ok(None) => {
                    let why = format!("{}: no process of its pid wrote it", path.display());
                    self.troubles.say(why);
                }
                Err(error) => {
                    processes.extend(cached.map(|it| (id, it)));
                    if error.kind() != ErrorKind::NotFound {
                        self.troubles.say(format!("{}: {error}", path.display()));
                    }
                }
            }
        }
        self.processes = processes;
        self.troubles.done();
        agents
    }
}

pub fn extension(lune: &Path) -> io::Result<String> {
    let Some(folder) = lune.to_str() else {
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "the daemon's folder is not UTF-8",
        ));
    };
    Ok(EXTENSION.replacen(FOLDER_STRING, &serde_json::to_string(folder)?, 1))
}

fn install(pi: &Path, lune: &Path) -> io::Result<()> {
    let text = extension(lune)?;
    let extensions = pi.join("extensions");
    let file = extensions.join(EXTENSION_FILE);
    if fs::read(&file).is_ok_and(|there| there == text.as_bytes()) {
        return Ok(());
    }
    fs::create_dir_all(&extensions)?;
    let new = extensions.join(format!(".{EXTENSION_FILE}.{}.new", std::process::id()));
    fs::write(&new, text)?;
    fs::rename(&new, &file).inspect_err(|_| {
        let _ = fs::remove_file(&new);
    })
}

fn read(
    path: &Path,
    start: &process::Start,
    started: SystemTime,
) -> io::Result<Option<(File, Proof)>> {
    let handle = fs::File::open(path)?;
    let modified = handle.metadata()?.modified()?;
    let bytes = read_file_text(handle)?;
    let raw: RawFile = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let file = File::parse(raw)?;
    let proof = match &file.proc_start {
        Some(recorded) => (recorded == start).then_some(Proof::Started { written: modified }),
        None => dated_since(modified, started).then_some(Proof::Dated),
    };
    Ok(proof.map(|proof| (file, proof)))
}

fn dated_since(modified: SystemTime, started: SystemTime) -> bool {
    modified
        .checked_add(DATED_LATE)
        .is_none_or(|late| started <= late)
}

#[cfg(test)]
pub(crate) fn update_of_file(bytes: &[u8]) -> io::Result<RungUpdate> {
    let raw: RawFile = serde_json::from_slice(bytes)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    Ok(File::parse(raw)?.update())
}

impl File {
    fn update(&self) -> RungUpdate {
        let status = self.status.clone().into_active();
        RungUpdate::Active {
            status,
            at: Some(self.status_since),
        }
    }
}

fn reported(id: AgentId, file: File, process: Process, proof: Proof) -> ReportedAgent {
    let update = file.update();
    let sighting = Sighting {
        session_id: Some(file.session_id),
        cwd: file.cwd,
        started_at: Some(process.started),
        name: None,
        entrypoint: file.entrypoint,
        terminal: process.terminal,
    };
    ReportedAgent::new(READS, id, sighting, update, proof).with_telemetry(file.telemetry)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use lune_protocol::program_status::Message;
    use lune_protocol::said::Said;
    use lune_protocol::telemetry::TelemetryText;

    use super::{DATED_LATE, File, PiStatus, RawFile, RawPhase, RawResting, dated_since};
    use crate::Directory;
    use crate::statuses::ActiveStatus;

    #[test]
    fn a_file_dated_since_its_process_started_stands() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(dated_since(started, started));
        assert!(
            dated_since(started - DATED_LATE, started),
            "a file dated a little late stands"
        );
        let earlier = started - DATED_LATE - Duration::from_nanos(1);
        assert!(
            !dated_since(earlier, started),
            "a file from an earlier process does not"
        );
    }

    #[test]
    fn a_file_dated_at_the_last_time_there_is_stands() {
        let secs = u64::try_from(i64::MAX).unwrap();
        let last = SystemTime::UNIX_EPOCH
            .checked_add(Duration::new(secs, 999_999_999))
            .unwrap();
        assert_eq!(
            last.checked_add(DATED_LATE),
            None,
            "nothing is later than the last time"
        );
        assert!(dated_since(last, SystemTime::now()));
    }

    #[test]
    fn pi_status_maps_stopped_and_legacy_idle_and_rejects_unknown_words() {
        let parse = |word, message| PiStatus::parse(word, message).map(PiStatus::into_active);
        assert_eq!(parse("stopped", None), Some(ActiveStatus::Stopped));
        assert_eq!(parse("idle", None), Some(ActiveStatus::Stopped));
        assert_eq!(
            parse("done", Some("ignored")),
            Some(ActiveStatus::Done { message: None })
        );
        assert_eq!(parse("busy", Some("ignored")), Some(ActiveStatus::Working));
        assert_eq!(
            parse("error", Some("  \x1b[31mrate\u{200b} limited  ")),
            Some(ActiveStatus::Error {
                message: Message::parse("message", "[31mrate limited").ok(),
            })
        );
        assert_eq!(
            parse("error", Some("   ")),
            Some(ActiveStatus::Error { message: None })
        );
        assert_eq!(parse("working", None), None);
        assert_eq!(parse("blocked", None), None);
        assert_eq!(parse("   ", None), None);
        assert_eq!(parse("unknown", None), None);
    }

    #[test]
    fn pi_file_validates_proc_start_status_and_phase_projection() {
        let base = |status: &str, proc_start: Option<&str>| RawFile {
            session_id: Said::new("s1".to_owned()).unwrap(),
            cwd: Directory::parse("/w".to_owned()).unwrap(),
            status: status.to_owned(),
            status_message: None,
            status_since: 1_000,
            proc_start: proc_start.map(str::to_owned),
            mode: Some("tui".to_owned()),
            phase: None,
            prompt_open: None,
            telemetry: None,
        };
        assert!(File::parse(base("stopped", Some("42"))).is_ok());
        assert!(File::parse(base("stopped", Some(""))).is_err());
        assert!(File::parse(base("stopped", Some("   "))).is_err());
        assert!(File::parse(base("   ", Some("42"))).is_err());

        let mut consistent = base("waiting", Some("42"));
        consistent.phase = Some(RawPhase::Running {
            outcome: RawResting {
                status: "error".to_owned(),
                message: Some("boom".to_owned()),
            },
        });
        consistent.prompt_open = Some(true);
        assert!(File::parse(consistent).is_ok());

        let mut mismatched = base("done", Some("42"));
        mismatched.phase = Some(RawPhase::Running {
            outcome: RawResting {
                status: "done".to_owned(),
                message: None,
            },
        });
        mismatched.prompt_open = Some(true);
        assert!(File::parse(mismatched).is_err());

        let mut half_snapshot = base("done", Some("42"));
        half_snapshot.prompt_open = Some(false);
        assert!(File::parse(half_snapshot).is_err());

        let mut bad_outcome = base("busy", Some("42"));
        bad_outcome.phase = Some(RawPhase::Running {
            outcome: RawResting {
                status: "busy".to_owned(),
                message: None,
            },
        });
        bad_outcome.prompt_open = Some(false);
        assert!(File::parse(bad_outcome).is_err());
    }

    #[test]
    fn pi_file_parses_telemetry_model_effort_context_and_cost() {
        let raw: RawFile = serde_json::from_value(serde_json::json!({
            "sessionId": "s1",
            "cwd": "/w",
            "status": "busy",
            "statusSince": 1000,
            "telemetry": {
                "model": "Opus 4.7 (high)",
                "effort": "HIGH",
                "context": {
                    "usedPercent": 1.9,
                    "windowTokens": 1_000_000,
                    "usedTokens": 19_000
                },
                "costUsd": 0.26
            }
        }))
        .unwrap();
        let file = File::parse(raw).unwrap();
        let telemetry = file.telemetry.expect("telemetry is parsed");
        assert_eq!(
            telemetry.model().map(TelemetryText::as_str),
            Some("Opus 4.7")
        );
        assert_eq!(telemetry.effort().map(TelemetryText::as_str), Some("high"));
        let context = telemetry.context().expect("context is present");
        assert_eq!(context.used_permille.get(), 19);
        assert_eq!(
            context.window_tokens.map(std::num::NonZeroU64::get),
            Some(1_000_000)
        );
        assert_eq!(context.used_tokens, Some(19_000));
        let usage = telemetry.usage().expect("usage is present");
        assert_eq!(usage.windows(), []);
        assert_eq!(usage.session_cost_micro_usd(), Some(260_000));
    }

    #[test]
    fn malformed_telemetry_fields_never_drop_pi_agent_parsing() {
        let raw: RawFile = serde_json::from_value(serde_json::json!({
            "sessionId": "s1",
            "cwd": "/w",
            "status": "busy",
            "statusSince": 1000,
            "telemetry": {
                "model": "Opus 4.7",
                "effort": "high",
                "context": {
                    "usedPercent": 12.5,
                    "windowTokens": 1e25,
                    "usedTokens": -3
                },
                "costUsd": "bad"
            }
        }))
        .unwrap();
        let file = File::parse(raw).unwrap();
        let telemetry = file.telemetry.expect("valid sibling fields remain");
        assert_eq!(
            telemetry.model().map(TelemetryText::as_str),
            Some("Opus 4.7")
        );
        assert_eq!(telemetry.context().unwrap().used_permille.get(), 125);
        assert_eq!(telemetry.context().unwrap().window_tokens, None);
        assert_eq!(telemetry.usage(), None);

        let non_object: RawFile = serde_json::from_value(serde_json::json!({
            "sessionId": "s1",
            "cwd": "/w",
            "status": "busy",
            "statusSince": 1000,
            "telemetry": "unexpected"
        }))
        .unwrap();
        let parsed = File::parse(non_object).unwrap();
        assert_eq!(parsed.telemetry, None);
    }
}
