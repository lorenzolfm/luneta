//! The pi adapter.
//!
//! pi keeps no file for each agent that runs, and its transcripts name no pid.
//! An extension of luneta's does: `pi/<pid>.json` in luneta's folder, which is
//! the daemon's own, moved into place whole as the agent's status changes, and
//! gone as pi quits. The adapter puts the extension where pi finds it, once pi
//! has run on the machine, and reads the files.
//!
//! A file outlives its process when the process is killed, and a pid can come
//! back as another process's. So a file stands for an agent only while the
//! process that runs by its pid is the one that wrote it. On Linux the file
//! says when that process started, as the system spells it. A date would not
//! do there: `process::started` works one out from the clock as it is now, and
//! a clock set forward would make a process look newer than what it wrote. On
//! macOS, where the extension cannot read that so simply, and the date is one
//! the kernel kept, the process must have started before the file was last
//! written, which one that came by the pid since did not.
//!
//! As with Claude Code's files, the file does not say what zellij pane the
//! agent is in, and the process's environment does.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use luneta_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{
    file_text, moment, named_pid, nonempty, Agent, Environment, Harness, Named, Status,
};
use crate::watch::{self, Troubles};
use crate::{log, process};

/// The extension, as lunetad puts it where pi finds it.
const EXTENSION: &str = include_str!("pi/luneta.ts");

/// What the extension is called in the folder pi finds extensions in.
const EXTENSION_FILE: &str = "luneta.ts";

/// The folder the extension keeps its files in, in luneta's.
const FOLDER: &str = "pi";

/// How much after a file was last written a process's start can be dated and
/// still be the process that wrote it: `process::started` dates one up to a
/// second late, on macOS.
const DATED_LATE: Duration = Duration::from_secs(1);

/// pi's folder: `$PI_CODING_AGENT_DIR`, or `~/.pi/agent`, as pi takes it.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".pi/agent")))
}

/// A file of the extension, as much of it as the adapter reads. A file without
/// the first four does not read as one, as the extension always writes them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "nonempty")]
    cwd: Said,
    #[serde(deserialize_with = "nonempty")]
    status: Said,
    status_since: u64,
    /// When its process started, as `process::start` spells it, on Linux.
    proc_start: Option<String>,
    /// The mode pi runs in, in pi's word.
    mode: Option<String>,
}

/// What a process says of itself, which does not change while it runs, and so
/// is read once.
#[derive(Clone)]
struct Process {
    started: Option<SystemTime>,
    pane: Option<Named>,
}

/// The files of the extension, read again on every scan.
pub struct AgentFiles {
    dir: PathBuf,
    /// pi's folder, until the extension is in it.
    uninstalled: Option<PathBuf>,
    /// What each process says of itself, by its pid and when it started.
    processes: HashMap<(u32, String), Process>,
    /// What went wrong in reading the folder, or in putting the extension.
    troubles: Troubles,
}

impl AgentFiles {
    /// The files in `luneta`, the daemon's folder, and the extension for pi's
    /// folder `pi`, if there is one to find.
    #[must_use]
    pub fn new(luneta: &Path, pi: Option<PathBuf>) -> Self {
        let dir = luneta.join(FOLDER);
        // Made ahead of any agent, to be watched from the start: a folder that
        // is not there yet is read on the sweep alone.
        if let Err(error) = fs::create_dir(&dir)
            && error.kind() != ErrorKind::AlreadyExists
        {
            log::say(&format!("{}: {error}", dir.display()));
        }
        AgentFiles { dir, uninstalled: pi, processes: HashMap::new(), troubles: Troubles::default() }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The agents that run, by the files as they are now. On the way, puts the
    /// extension where pi finds it, if pi has come to the machine since the
    /// last scan: the daemon can run for a long time before it does.
    pub fn scan(&mut self) -> Vec<Agent> {
        if let Some(pi) = self.uninstalled.as_ref().filter(|pi| pi.is_dir()) {
            match install(pi) {
                Ok(()) => self.uninstalled = None,
                Err(error) => {
                    self.troubles.say(format!("pi's extension in {}: {error}", pi.display()));
                },
            }
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
            let key = (pid, start);
            let found = self.processes.remove(&key).unwrap_or_else(|| Process {
                started: process::started(pid),
                pane: Environment::of(pid).pane,
            });
            let file = read(&path, &key.1, found.started);
            processes.insert(key.clone(), found.clone());
            match file {
                Ok(Some(file)) => agents.push(agent(key, file, found)),
                Ok(None) => {
                    let why = format!("{}: no process of its pid wrote it", path.display());
                    self.troubles.say(why);
                },
                // A file gone since the folder was listed is gone.
                Err(error) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => self.troubles.say(format!("{}: {error}", path.display())),
            }
        }
        self.processes = processes;
        self.troubles.done();
        agents
    }
}

/// Puts the extension in the folder pi finds extensions in, where it is not as
/// it should be. pi makes `pi`, its folder, as it first runs, and nothing makes
/// that folder in it until someone has an extension to put there.
fn install(pi: &Path) -> io::Result<()> {
    let extensions = pi.join("extensions");
    let file = extensions.join(EXTENSION_FILE);
    if fs::read(&file).is_ok_and(|text| text == EXTENSION.as_bytes()) {
        return Ok(());
    }
    fs::create_dir_all(&extensions)?;
    // Whole, as a pi that starts reads it, under a name it does not load.
    let new = extensions.join(format!(".{EXTENSION_FILE}.{}.new", std::process::id()));
    fs::write(&new, EXTENSION)?;
    fs::rename(&new, &file).inspect_err(|_| {
        // What failed leaves nothing worth keeping, and what follows says why.
        let _ = fs::remove_file(&new);
    })
}

/// What the file at `path` says, if it stands for the process that runs by its
/// pid, whose start the system spells `start` and dates `started`. A file names
/// the start of the process that wrote it, or else, where the extension cannot
/// tell it, one last written before that date was left behind by another of its
/// pid. Where the system does not say, no process can be ruled out.
fn read(path: &Path, start: &str, started: Option<SystemTime>) -> io::Result<Option<File>> {
    let file: File = serde_json::from_slice(&file_text(path)?)
        .map_err(|error| io::Error::new(ErrorKind::InvalidData, error))?;
    let stands = match (&file.proc_start, started) {
        (Some(recorded), _) => recorded == start,
        (None, Some(started)) => started <= fs::metadata(path)?.modified()? + DATED_LATE,
        (None, None) => true,
    };
    Ok(stands.then_some(file))
}

/// The agent a file stands for, in the process that wrote it.
fn agent((pid, start): (u32, String), file: File, process: Process) -> Agent {
    Agent {
        pid,
        start,
        harness: Harness::Pi,
        session_id: file.session_id,
        cwd: file.cwd,
        status: Status::of(Some(file.status.as_str()), None),
        status_since: Some(moment(file.status_since)),
        started_at: process.started,
        name: None,
        entrypoint: file.mode.as_deref().and_then(entrypoint),
        pane: process.pane,
        review: None,
    }
}

/// How an agent was started, by the mode its pi runs in: `tui` is the agent a
/// person talks to at a prompt, which goes by Claude Code's word for it, `cli`,
/// the one lunetad's clients seat in a pane before any other.
fn entrypoint(mode: &str) -> Option<Said> {
    let mode = mode.trim();
    Said::new(if mode == "tui" { "cli" } else { mode }.to_owned())
}
