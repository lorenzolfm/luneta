use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use lune_domain::agent::{Harness, Rung};
use lune_domain::mux::TerminalKey;
use lune_protocol::program_status::Message;
use lune_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{
    absolute, file_text, moment, named_pid, nonempty, terminal_or_trouble, SeenAgent,
};
use crate::mux;
use crate::process::{self, AgentId};
use crate::statuses::{self, ActiveStatus, Report, ReportedAgent, RungUpdate, SOURCE_AGENTS_MAX};
use crate::watch::{self, Troubles};
use crate::{log, Directory};

const EXTENSION: &str = include_str!("pi/lune.ts");

const FOLDER_STRING: &str = "\"LUNE_FOLDER\"";

const EXTENSION_FILE: &str = "lune.ts";

const FOLDER: &str = "pi";

const DATED_LATE: Duration = Duration::from_secs(1);

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "absolute")]
    cwd: Directory,
    #[serde(deserialize_with = "nonempty")]
    status: Said,
    status_message: Option<String>,
    status_since: u64,
    proc_start: Option<String>,
    mode: Option<String>,
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
            self.troubles.say(format!("pi's extension in {}: {error}", pi.display()));
        }
        let mut paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles.say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            },
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
            let Some(started) =
                cached.map(|it| it.started).or_else(|| process::started(pid.get()))
            else {
                continue;
            };
            match read(&path, id.start().as_str(), started) {
                Ok(Some(file)) => {
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
                    let built = reported(id.clone(), file, found);
                    processes.insert(id, found);
                    agents.push(built);
                },
                Ok(None) => {
                    let why = format!("{}: no process of its pid wrote it", path.display());
                    self.troubles.say(why);
                },
                Err(error) => {
                    processes.extend(cached.map(|it| (id, it)));
                    if error.kind() != ErrorKind::NotFound {
                        self.troubles.say(format!("{}: {error}", path.display()));
                    }
                },
            }
        }
        self.processes = processes;
        self.troubles.done();
        agents
    }
}

fn extension(lune: &Path) -> io::Result<String> {
    let Some(folder) = lune.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "the daemon's folder is not UTF-8"));
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

fn read(path: &Path, start: &str, started: SystemTime) -> io::Result<Option<File>> {
    let file: File = serde_json::from_slice(&file_text(path)?)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let stands = if let Some(recorded) = &file.proc_start {
        recorded == start
    } else {
        dated_since(fs::metadata(path)?.modified()?, started)
    };
    Ok(stands.then_some(file))
}

fn dated_since(modified: SystemTime, started: SystemTime) -> bool {
    modified.checked_add(DATED_LATE).is_none_or(|late| started <= late)
}

fn parse_status(word: &Said, message: Option<&str>) -> Option<ActiveStatus> {
    let detail = message
        .and_then(|text| Message::from_raw_bytes(text.as_bytes()))
        .and_then(|parsed| parsed.to_said());
    let parsed = match statuses::parse_word(Some(word.as_str()), None)? {
        ActiveStatus::Idle => ActiveStatus::Stopped,
        ActiveStatus::Error { .. } => ActiveStatus::Error { message: detail },
        other => other,
    };
    Some(parsed)
}

fn reported(id: AgentId, file: File, process: Process) -> ReportedAgent {
    let update = match parse_status(&file.status, file.status_message.as_deref()) {
        Some(status) => RungUpdate::Active {
            status,
            at: Some(moment(file.status_since)),
            subagents: Vec::new(),
        },
        None => RungUpdate::Clear,
    };
    let report = Report { agent: id.clone(), rung: Rung::Extension, update };
    let seen = SeenAgent {
        id,
        harness: Harness::Pi,
        session_id: Some(file.session_id),
        cwd: file.cwd,
        started_at: Some(process.started),
        name: None,
        entrypoint: file.mode.and_then(|mode| Said::new(mode.trim().to_owned())),
        terminal: process.terminal,
    };
    ReportedAgent { seen, report }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use lune_protocol::said::Said;

    use super::{dated_since, parse_status, DATED_LATE};
    use crate::statuses::ActiveStatus;

    #[test]
    fn a_file_dated_since_its_process_started_stands() {
        let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        assert!(dated_since(started, started));
        assert!(dated_since(started - DATED_LATE, started), "a file dated a little late stands");
        let earlier = started - DATED_LATE - Duration::from_nanos(1);
        assert!(!dated_since(earlier, started), "a file from an earlier process does not");
    }

    #[test]
    fn a_file_dated_at_the_last_time_there_is_stands() {
        let secs = u64::try_from(i64::MAX).unwrap();
        let last = SystemTime::UNIX_EPOCH.checked_add(Duration::new(secs, 999_999_999)).unwrap();
        assert_eq!(last.checked_add(DATED_LATE), None, "nothing is later than the last time");
        assert!(dated_since(last, SystemTime::now()));
    }

    #[test]
    fn parse_status_maps_idle_to_stopped_and_sanitizes_error_messages() {
        let word = |text: &str| Said::new(text.to_owned()).unwrap();
        assert_eq!(parse_status(&word("idle"), None), Some(ActiveStatus::Stopped));
        assert_eq!(parse_status(&word("done"), Some("ignored")), Some(ActiveStatus::Done { message: None }));
        assert_eq!(parse_status(&word("busy"), Some("ignored")), Some(ActiveStatus::Working));
        assert_eq!(
            parse_status(&word("error"), Some("  \x1b[31mrate\u{200b} limited  ")),
            Some(ActiveStatus::Error { message: Said::new("[31mrate limited".to_owned()) })
        );
        assert_eq!(parse_status(&word("error"), Some("   ")), Some(ActiveStatus::Error { message: None }));
        assert_eq!(parse_status(&word("   "), None), None);
    }
}
