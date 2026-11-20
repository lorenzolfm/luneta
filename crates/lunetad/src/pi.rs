use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use luneta_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{
    absolute, file_text, moment, named_pid, nonempty, terminal_or_trouble, Agent, Harness, Status,
};
use crate::mux::{self, TerminalKey};
use crate::watch::{self, Troubles};
use crate::{log, process};
use crate::Directory;

const EXTENSION: &str = include_str!("pi/luneta.ts");

const FOLDER_STRING: &str = "\"LUNETA_FOLDER\"";

const EXTENSION_FILE: &str = "luneta.ts";

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
    luneta: PathBuf,
    dir: PathBuf,
    pi: Option<PathBuf>,
    processes: HashMap<process::Id, Process>,
    troubles: Troubles,
}

impl AgentFiles {
    #[must_use]
    pub fn new(luneta: &Path, pi: Option<PathBuf>) -> Self {
        let dir = luneta.join(FOLDER);
        if let Err(error) = fs::create_dir(&dir)
            && error.kind() != ErrorKind::AlreadyExists
        {
            log::say(&format!("{}: {error}", dir.display()));
        }
        AgentFiles {
            luneta: luneta.to_owned(),
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

    pub fn scan(&mut self) -> Vec<Agent> {
        if let Some(pi) = self.pi.as_ref().filter(|pi| pi.is_dir())
            && let Err(error) = install(pi, &self.luneta)
        {
            self.troubles.say(format!("pi's extension in {}: {error}", pi.display()));
        }
        let paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles.say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            },
        };
        let mut processes = HashMap::with_capacity(paths.len());
        let mut agents = Vec::with_capacity(paths.len());
        for path in paths {
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let Some(start) = process::start(pid) else {
                continue;
            };
            let id = process::Id { pid, start };
            let cached = self.processes.remove(&id);
            let Some(started) = cached.map(|it| it.started).or_else(|| process::started(pid)) else {
                continue;
            };
            match read(&path, id.start.as_str(), started) {
                Ok(Some(file)) => {
                    let terminal = cached.map_or_else(
                        || terminal_or_trouble(pid, mux::terminal_of(&id), &mut self.troubles),
                        |it| it.terminal,
                    );
                    let found = Process { started, terminal };
                    processes.insert(id, found);
                    agents.push(agent(pid, file, found));
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

fn extension(luneta: &Path) -> io::Result<String> {
    let Some(folder) = luneta.to_str() else {
        return Err(io::Error::new(ErrorKind::InvalidData, "the daemon's folder is not UTF-8"));
    };
    Ok(EXTENSION.replacen(FOLDER_STRING, &serde_json::to_string(folder)?, 1))
}

fn install(pi: &Path, luneta: &Path) -> io::Result<()> {
    let text = extension(luneta)?;
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
    let stands = match &file.proc_start {
        Some(recorded) => recorded == start,
        None => started <= fs::metadata(path)?.modified()? + DATED_LATE,
    };
    Ok(stands.then_some(file))
}

fn agent(pid: u32, file: File, process: Process) -> Agent {
    Agent {
        pid,
        harness: Harness::Pi,
        session_id: file.session_id,
        cwd: file.cwd,
        status: Status::of(Some(file.status.as_str()), None),
        status_since: Some(moment(file.status_since)),
        started_at: Some(process.started),
        name: None,
        entrypoint: file.mode.and_then(|mode| Said::new(mode.trim().to_owned())),
        terminal: process.terminal,
    }
}
