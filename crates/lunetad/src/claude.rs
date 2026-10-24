//! The Claude Code adapter.
//!
//! Claude Code keeps a file for each agent that runs, `sessions/<pid>.json` in
//! its folder, and writes the agent's status into it as the status changes.
//! These files are what the sidebar shows today, through claude-ps, and the
//! adapter reads them the way claude-ps does.
//!
//! A file outlives its process when the process crashes, and a pid can come
//! back as another process's. So a file stands for an agent only while a
//! process with its pid runs and started when the file says it did. The file
//! does not say what zellij pane the agent is in. The process's environment
//! does: zellij gives every pane its session's name and the pane's id.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::agent::{Agent, Name, Named, Status};
use crate::process;

/// Claude Code's folder: `$CLAUDE_CONFIG_DIR`, or `~/.claude`, as the plugin
/// takes it.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// A session file, as much of it as the adapter reads. A file without the
/// first four names no agent.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    pid: u32,
    session_id: String,
    cwd: PathBuf,
    /// When the process started, in the words of the system it ran on: a
    /// number on Linux, a date on macOS.
    proc_start: serde_json::Value,
    /// The system and pid namespace the pid is from, as Claude Code spells
    /// them. A file from a container that shares the folder names another.
    pid_domain: Option<String>,
    status: Option<String>,
    status_updated_at: Option<u64>,
    updated_at: Option<u64>,
    started_at: Option<u64>,
    name: Option<String>,
    name_source: Option<String>,
    entrypoint: Option<String>,
}

impl File {
    /// The process start the file records, spelled as the system spells its
    /// own.
    fn proc_start(&self) -> Option<String> {
        match &self.proc_start {
            serde_json::Value::String(start) => Some(start.clone()),
            serde_json::Value::Number(start) => Some(start.to_string()),
            _ => None,
        }
    }
}

/// The agents of a Claude Code folder, read again on every scan.
pub struct Sessions {
    dir: PathBuf,
    /// The pids of this system, to tell a file from another's by.
    domain: process::Domain,
    /// What each file said when it last read whole. Claude Code writes a file
    /// in place, and a read in the middle of the write finds half of it.
    read: HashMap<PathBuf, File>,
    /// The pane each process's environment names. The environment a process
    /// shows is the one it started with, so it is read once.
    panes: HashMap<(u32, String), Option<Named>>,
}

impl Sessions {
    #[must_use]
    pub fn new(claude: &Path) -> Self {
        Sessions {
            dir: claude.join("sessions"),
            domain: process::Domain::here(),
            read: HashMap::new(),
            panes: HashMap::new(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The agents that run, by the files as they are now.
    pub fn scan(&mut self) -> Vec<Agent> {
        let mut read = HashMap::new();
        let entries = fs::read_dir(&self.dir).into_iter().flatten().flatten();
        for path in entries.map(|entry| entry.path()) {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            // A file gone since the folder was listed is gone, and one that
            // does not parse is halfway written, or else says what it said.
            let Ok(text) = fs::read(&path) else {
                continue;
            };
            let parsed = serde_json::from_slice::<File>(&text).ok();
            if let Some(file) = parsed.or_else(|| self.read.remove(&path)) {
                read.insert(path, file);
            }
        }
        self.read = read;

        let mut panes = HashMap::with_capacity(self.read.len());
        let mut agents = Vec::with_capacity(self.read.len());
        for file in self.read.values() {
            let Some(start) = file.proc_start() else {
                continue;
            };
            if !self.local(file) || process::start(file.pid).as_ref() != Some(&start) {
                continue;
            }
            let key = (file.pid, start);
            let pane = match self.panes.remove(&key) {
                Some(pane) => pane,
                None => pane_of(file.pid),
            };
            agents.push(agent(file, pane.clone()));
            panes.insert(key, pane);
        }
        self.panes = panes;
        agents
    }

    /// Whether the file's pid is one of this system's. A file that does not
    /// say is taken to be.
    fn local(&self, file: &File) -> bool {
        file.pid_domain.as_deref().is_none_or(|recorded| self.domain.admits(recorded))
    }
}

fn agent(file: &File, pane: Option<Named>) -> Agent {
    let since = file.status_updated_at.or(file.updated_at).or(file.started_at);
    let word = |text: &Option<String>| {
        let word = text.as_deref().map(str::trim)?;
        (!word.is_empty()).then(|| word.to_owned())
    };
    let name = file.name.clone().filter(|text| !text.is_empty());
    Agent {
        pid: file.pid,
        session_id: file.session_id.clone(),
        cwd: file.cwd.clone(),
        status: Status::of(file.status.as_deref()),
        status_since: since.map(moment),
        started_at: file.started_at.map(moment),
        name: name.map(|text| Name { text, source: word(&file.name_source) }),
        entrypoint: word(&file.entrypoint),
        pane,
    }
}

/// A time as Claude Code writes it, in milliseconds since the epoch.
fn moment(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

/// The pane the environment of `pid` names, if it names one.
fn pane_of(pid: u32) -> Option<Named> {
    let (mut session, mut pane) = (None, None);
    for entry in process::environ(pid)? {
        let entry = entry.as_bytes();
        if let Some(name) = entry.strip_prefix(b"ZELLIJ_SESSION_NAME=") {
            session = Some(String::from_utf8_lossy(name).into_owned());
        } else if let Some(id) = entry.strip_prefix(b"ZELLIJ_PANE_ID=") {
            pane = std::str::from_utf8(id).ok().and_then(|id| id.parse().ok());
        }
    }
    let session = session.filter(|session| !session.is_empty())?;
    Some(Named { session, pane: pane? })
}
