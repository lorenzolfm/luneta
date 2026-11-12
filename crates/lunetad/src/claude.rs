//! The Claude Code adapter.
//!
//! Claude Code keeps a file for each agent that runs, `sessions/<pid>.json` in
//! its folder, and writes the agent's status into it as the status changes.
//! These files are what luneta shows of each agent.
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
use std::fmt;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use luneta_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{
    file_text, moment, named_pid, nonempty, Agent, Harness, Name, Source, Status, Unread,
};
use crate::mux::{self, Named};
use crate::{log, process, program};
use crate::watch::{self, Troubles};

/// How long a file that does not read still says what it said before. Claude
/// Code writes a file in place, and in a moment: one that does not read for
/// longer is cut short for good, or in a shape of a newer Claude Code's.
const TORN: Duration = Duration::from_secs(1);

/// `claude`, to have `model`, told `system`, answer `prompt` once, and the stdin
/// that tells it `prompt`: its arguments are there for any user of the machine
/// to read. It has no tools, and runs in safe mode, so that none of your hooks,
/// `CLAUDE.md` or MCP servers come into it, and with no zellij in its
/// environment, so that the daemon places it in no pane.
pub fn one_answer(model: &str, system: &str, prompt: &str) -> io::Result<(Command, Stdio)> {
    let mut claude = Command::new("claude");
    claude.args(["-p", "--model", model, "--tools", "", "--safe-mode"]);
    claude.args(["--no-session-persistence", "--system-prompt", system]);
    mux::in_no_pane(&mut claude);
    Ok((claude, program::saying(prompt)?))
}

/// Claude Code's folder: `$CLAUDE_CONFIG_DIR`, or `~/.claude`, as Claude Code
/// takes it.
#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

/// A session file, as much of it as the adapter reads. A file without the
/// first three does not read as one, nor does one that names no session or
/// no directory.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "nonempty")]
    cwd: Said,
    proc_start: Start,
    /// The system and pid namespace the pid is from, as Claude Code spells
    /// them. A file from a container that shares the folder names another.
    pid_domain: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    status_updated_at: Option<u64>,
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
enum Seen {
    Whole(File),
    /// It does not read, and has not since `since`: halfway written, and what
    /// it said `before`, if it said anything, stands.
    Torn { since: Instant, before: Option<File> },
    /// It has not read for longer than a write takes, or did not in the
    /// daemon's first read.
    Unread,
}

/// What a scan of the folder finds.
pub struct Scan {
    pub agents: Vec<Agent>,
    /// The files it cannot read, of those named for a pid that a process
    /// runs by.
    pub unread: Vec<Unread>,
}

impl Scan {
    /// The processes it found running, whose ends change what it finds.
    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        let Scan { agents, unread } = self;
        let agents = agents.iter().map(|agent| agent.pid);
        agents.chain(unread.iter().map(|unread| unread.pid)).collect()
    }
}

/// The session files of a Claude Code folder, read again on every scan.
pub struct SessionFiles {
    dir: PathBuf,
    /// The pids of this system, to tell a file from another's by.
    domain: process::Domain,
    /// Each file, by the pid it is named for, as it last read.
    seen: HashMap<u32, Seen>,
    /// The pane each process's environment names. The environment a process
    /// shows is the one it started with, so it is read once.
    panes: HashMap<(u32, String), Option<Named>>,
    /// What went wrong in listing the folder.
    troubles: Troubles,
    /// Whether the folder has been read: a file that does not read in the
    /// first read was there before the daemon, which saw no write of it begin.
    scanned: bool,
}

impl SessionFiles {
    #[must_use]
    pub fn new(claude: &Path) -> Self {
        SessionFiles {
            dir: claude.join("sessions"),
            domain: process::Domain::here(),
            seen: HashMap::new(),
            panes: HashMap::new(),
            troubles: Troubles::default(),
            scanned: false,
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The agents that run, by the files as they are now.
    pub fn scan(&mut self) -> Scan {
        let paths = match watch::listed(&self.dir) {
            Ok(paths) => paths,
            Err(error) => {
                self.troubles.say(format!("{}: {error}", self.dir.display()));
                Vec::new()
            },
        };
        self.troubles.done();
        let first = !self.scanned;
        self.scanned = true;
        let mut seen = HashMap::new();
        for path in paths {
            let Some(pid) = named_pid(&path) else {
                continue;
            };
            let was = self.seen.remove(&pid);
            let now = match file_text(&path) {
                Ok(text) => match serde_json::from_slice::<File>(&text) {
                    Ok(file) => Seen::Whole(file),
                    Err(error) => torn(was, first, &path, &error),
                },
                // A file gone since the folder was listed is gone.
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => torn(was, first, &path, &error),
            };
            seen.insert(pid, now);
        }
        self.seen = seen;

        let mut panes = HashMap::with_capacity(self.seen.len());
        // The pane of each process, read from its environment once.
        let mut pane = |pid: u32, start: String| {
            let key = (pid, start);
            let pane = self.panes.remove(&key).unwrap_or_else(|| mux::pane_of(pid));
            panes.insert(key, pane.clone());
            pane
        };
        let mut agents = Vec::with_capacity(self.seen.len());
        let mut unread = Vec::new();
        for (&pid, found) in &self.seen {
            let file = match found {
                Seen::Whole(file) | Seen::Torn { before: Some(file), .. } => file,
                Seen::Torn { before: None, .. } => continue,
                // One that does not read may be an agent's while a process
                // runs by its pid, in the pane that one runs in. One a crashed
                // process left behind stands for no agent, whatever it holds.
                // A pid that came back as another process's still counts, as
                // the file cannot say.
                Seen::Unread => {
                    if let Some(start) = process::start(pid) {
                        let started = process::started(pid);
                        unread.push(Unread { pid, started, pane: pane(pid, start) });
                    }
                    continue;
                },
            };
            // A file of another system's pids names no process here.
            if !local(&self.domain, file) {
                continue;
            }
            // A pid that came back as another process's names that one now.
            let start = file.proc_start.spelled();
            if process::start(pid).as_ref() != Some(&start) {
                continue;
            }
            agents.push(agent(pid, file, pane(pid, start)));
        }
        self.panes = panes;
        assert!(agents.len() + unread.len() <= self.seen.len());
        Scan { agents, unread }
    }
}

/// Whether the file's pid is one of the system's whose pids are `domain`. A
/// file that does not say is taken to be.
fn local(domain: &process::Domain, file: &File) -> bool {
    file.pid_domain.as_deref().is_none_or(|recorded| domain.admits(recorded))
}

/// What a file that does not read is now, by what it `was`. One may be
/// halfway written for a while, and says what it said before. One that does
/// not read for longer is unread, which the daemon says once. So is one that
/// does not read in the `first` read, which the daemon cannot tell from one
/// halfway written, so that its first answer counts it.
fn torn(was: Option<Seen>, first: bool, path: &Path, error: &impl fmt::Display) -> Seen {
    let unread = || {
        log::say(&format!("{} does not read as a session file: {error}", path.display()));
        Seen::Unread
    };
    match was {
        Some(Seen::Whole(file)) => Seen::Torn { since: Instant::now(), before: Some(file) },
        None if first => unread(),
        None => Seen::Torn { since: Instant::now(), before: None },
        Some(Seen::Torn { since, before }) if since.elapsed() < TORN => {
            Seen::Torn { since, before }
        },
        Some(Seen::Torn { .. }) => unread(),
        Some(Seen::Unread) => Seen::Unread,
    }
}

/// The agent a file stands for. Its status is dated where Claude Code dates
/// the status, and by nothing else: when the file was written, or the session
/// started, is not when the agent came to what it is doing.
fn agent(pid: u32, file: &File, pane: Option<Named>) -> Agent {
    let word = |text: &Option<String>| Said::new(text.as_deref()?.trim().to_owned());
    let name = file.name.clone().and_then(Said::new);
    let source = word(&file.name_source).map(|word| Source::of(word.as_str()));
    Agent {
        pid,
        harness: Harness::ClaudeCode,
        session_id: file.session_id.clone(),
        cwd: file.cwd.clone(),
        status: Status::of(file.status.as_deref(), file.waiting_for.as_deref()),
        status_since: file.status_updated_at.map(moment),
        started_at: file.started_at.map(moment),
        name: name.map(|text| Name { text, source }),
        entrypoint: word(&file.entrypoint),
        pane,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    /// `claude` runs for one answer with no tools, in safe mode, and keeps no
    /// session, and what names a pane stays out of its environment, so that
    /// the daemon lists it as an agent in no pane. The prompt is on its stdin,
    /// and not among its arguments, which any user of the machine can read.
    #[test]
    fn one_answer_runs_in_no_pane_with_no_tools() {
        let (claude, told) = one_answer("haiku", "be brief", "the prompt").unwrap();
        let args: Vec<&OsStr> = claude.get_args().collect();
        let said = [
            "-p", "--model", "haiku", "--tools", "", "--safe-mode", "--no-session-persistence",
            "--system-prompt", "be brief",
        ];
        assert_eq!(args, said.map(OsStr::new));
        let mut unset: Vec<&OsStr> =
            claude.get_envs().filter(|(_, value)| value.is_none()).map(|(name, _)| name).collect();
        unset.sort_unstable();
        let own = ["LUNETA_MUX_MADE", "LUNETA_MUX_PANE", "LUNETA_MUX_SESSION"].map(OsStr::new);
        let zellij = ["ZELLIJ", "ZELLIJ_PANE_ID", "ZELLIJ_SESSION_NAME"].map(OsStr::new);
        assert_eq!(unset, [own.as_slice(), &zellij].concat());
        let heard = Command::new("cat").stdin(told).output().unwrap();
        assert_eq!(heard.stdout, b"the prompt");
    }
}
