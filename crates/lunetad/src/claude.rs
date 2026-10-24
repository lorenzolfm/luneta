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
//!
//! The pid is the one the file is named for, as Claude Code reads it back. A
//! folder holds one file of a name, and so one agent to a pid, whatever else
//! is in it.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::agent::{Agent, Name, Named, Source, Status};
use crate::process;

/// How long a file that does not parse still says what it said before. Claude
/// Code writes a file in place, and in a moment: one that does not parse for
/// longer is cut short for good, or in a shape of a newer Claude Code's.
const TORN: Duration = Duration::from_secs(1);

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
/// first three does not read as one.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    session_id: String,
    cwd: PathBuf,
    proc_start: Start,
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

/// When the process started, in the words of the system it ran on: a number
/// on Linux, a date on macOS. A string, or the number bare.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Start {
    Spelled(String),
    Counted(u64),
}

impl Start {
    /// As the system spells its own.
    fn spelled(&self) -> String {
        match self {
            Start::Spelled(start) => start.clone(),
            Start::Counted(start) => start.to_string(),
        }
    }
}

/// A session file, as it last read. Claude Code writes a file in place, and a
/// read in the middle of the write finds half of it.
enum Read {
    Whole(File),
    /// It does not parse, and has not since `since`: halfway written, and what
    /// it said `before`, if it said anything, stands.
    Torn { since: Instant, before: Option<File> },
    /// It has not parsed for longer than a write takes.
    Unread,
}

/// What a scan of the folder finds.
pub struct Scan {
    pub agents: Vec<Agent>,
    /// The files it cannot read.
    pub unread: usize,
}

/// The agents of a Claude Code folder, read again on every scan.
pub struct Sessions {
    dir: PathBuf,
    /// The pids of this system, to tell a file from another's by.
    domain: process::Domain,
    /// Each file, by the pid it is named for, as it last read.
    read: HashMap<u32, Read>,
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
    pub fn scan(&mut self) -> Scan {
        let mut read = HashMap::new();
        let entries = fs::read_dir(&self.dir).into_iter().flatten().flatten();
        for path in entries.map(|entry| entry.path()) {
            let Some(pid) = named(&path) else {
                continue;
            };
            // A file gone since the folder was listed is gone.
            let Ok(text) = fs::read(&path) else {
                continue;
            };
            let was = self.read.remove(&pid);
            let now = match serde_json::from_slice::<File>(&text) {
                Ok(file) => Read::Whole(file),
                Err(error) => torn(was, &path, &error),
            };
            read.insert(pid, now);
        }
        self.read = read;

        let mut panes = HashMap::with_capacity(self.read.len());
        let mut agents = Vec::with_capacity(self.read.len());
        let mut unread = 0;
        for (&pid, found) in &self.read {
            let file = match found {
                Read::Whole(file) | Read::Torn { before: Some(file), .. } => file,
                Read::Torn { before: None, .. } => continue,
                Read::Unread => {
                    unread += 1;
                    continue;
                },
            };
            let start = file.proc_start.spelled();
            if !self.local(file) || process::start(pid).as_ref() != Some(&start) {
                continue;
            }
            let key = (pid, start);
            let pane = match self.panes.remove(&key) {
                Some(pane) => pane,
                None => pane_of(pid),
            };
            agents.push(agent(pid, file, pane.clone()));
            panes.insert(key, pane);
        }
        self.panes = panes;
        Scan { agents, unread }
    }

    /// Whether the file's pid is one of this system's. A file that does not
    /// say is taken to be.
    fn local(&self, file: &File) -> bool {
        file.pid_domain.as_deref().is_none_or(|recorded| self.domain.admits(recorded))
    }
}

/// The pid a session file is named for: `<pid>.json`, spelled as Claude Code
/// spells a pid, which is how it reads one back. Any other file in the folder,
/// a copy of one too, names no agent.
fn named(path: &Path) -> Option<u32> {
    if path.extension()? != "json" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let pid = stem.parse::<u32>().ok()?;
    (pid.to_string() == stem).then_some(pid)
}

/// What a file that does not parse is now, by what it `was`. One may be
/// halfway written for a while, and says what it said before. One that does
/// not parse for longer is unread, which the daemon says once.
fn torn(was: Option<Read>, path: &Path, error: &serde_json::Error) -> Read {
    match was {
        Some(Read::Whole(file)) => Read::Torn { since: Instant::now(), before: Some(file) },
        None => Read::Torn { since: Instant::now(), before: None },
        Some(Read::Torn { since, before }) if since.elapsed() < TORN => Read::Torn { since, before },
        Some(Read::Torn { .. }) => {
            eprintln!("lunetad: {} does not read as a session file: {error}", path.display());
            Read::Unread
        },
        Some(Read::Unread) => Read::Unread,
    }
}

fn agent(pid: u32, file: &File, pane: Option<Named>) -> Agent {
    let since = file.status_updated_at.or(file.updated_at).or(file.started_at);
    let word = |text: &Option<String>| {
        let word = text.as_deref().map(str::trim)?;
        (!word.is_empty()).then(|| word.to_owned())
    };
    let name = file.name.clone().filter(|text| !text.is_empty());
    let source = word(&file.name_source).map(|word| Source::of(&word));
    Agent {
        pid,
        session_id: file.session_id.clone(),
        cwd: file.cwd.clone(),
        status: Status::of(file.status.as_deref()),
        status_since: since.map(moment),
        started_at: file.started_at.map(moment),
        name: name.map(|text| Name { text, source }),
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
