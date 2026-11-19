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
use crate::mux::{self, TerminalKey};
use crate::{log, process, program};
use crate::watch::{self, Troubles};

const TORN: Duration = Duration::from_secs(1);

pub fn one_answer(model: &str, system: &str, prompt: &str) -> io::Result<(Command, Stdio)> {
    let mut claude = Command::new("claude");
    claude.args(["-p", "--model", model, "--tools", "", "--safe-mode"]);
    claude.args(["--no-session-persistence", "--system-prompt", system]);
    mux::in_no_pane(&mut claude);
    Ok((claude, program::saying(prompt)?))
}

#[must_use]
pub fn folder() -> Option<PathBuf> {
    let set = |name| std::env::var_os(name).filter(|value: &OsString| !value.is_empty());
    set("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("HOME").map(|home| PathBuf::from(home).join(".claude")))
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    #[serde(deserialize_with = "nonempty")]
    session_id: Said,
    #[serde(deserialize_with = "nonempty")]
    cwd: Said,
    proc_start: Start,
    pid_domain: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    status_updated_at: Option<u64>,
    started_at: Option<u64>,
    name: Option<String>,
    name_source: Option<String>,
    entrypoint: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(untagged)]
enum Start {
    Spelled(String),
    Counted(u64),
}

impl Start {
    fn spelled(&self) -> String {
        match self {
            Start::Spelled(start) => start.clone(),
            Start::Counted(start) => start.to_string(),
        }
    }
}

enum Seen {
    Whole(File),
    Torn { since: Instant, before: Option<File> },
    Unread,
}

pub struct Scan {
    pub agents: Vec<Agent>,
    pub unread: Vec<Unread>,
}

impl Scan {
    #[must_use]
    pub fn pids(&self) -> Vec<u32> {
        let Scan { agents, unread } = self;
        let agents = agents.iter().map(|agent| agent.pid);
        agents.chain(unread.iter().map(|unread| unread.pid)).collect()
    }
}

pub struct SessionFiles {
    dir: PathBuf,
    domain: process::Domain,
    seen: HashMap<u32, Seen>,
    panes: HashMap<(u32, String), Option<TerminalKey>>,
    troubles: Troubles,
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
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => torn(was, first, &path, &error),
            };
            seen.insert(pid, now);
        }
        self.seen = seen;

        let mut panes = HashMap::with_capacity(self.seen.len());
        let mut pane = |pid: u32, start: String| {
            let key = (pid, start);
            let pane = self.panes.remove(&key).unwrap_or_else(|| mux::pane_of(pid));
            panes.insert(key, pane);
            pane
        };
        let mut agents = Vec::with_capacity(self.seen.len());
        let mut unread = Vec::new();
        for (&pid, found) in &self.seen {
            let file = match found {
                Seen::Whole(file) | Seen::Torn { before: Some(file), .. } => file,
                Seen::Torn { before: None, .. } => continue,
                Seen::Unread => {
                    if let Some(start) = process::start(pid) {
                        unread.push(Unread { pid, pane: pane(pid, start) });
                    }
                    continue;
                },
            };
            if !local(&self.domain, file) {
                continue;
            }
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

fn local(domain: &process::Domain, file: &File) -> bool {
    file.pid_domain.as_deref().is_none_or(|recorded| domain.admits(recorded))
}

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

fn agent(pid: u32, file: &File, pane: Option<TerminalKey>) -> Agent {
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
        assert_eq!(unset, ["LUNETA_MUX_TERMINAL", "LUNETA_MUX_TERMINAL_MADE"].map(OsStr::new));
        let heard = Command::new("cat").stdin(told).output().unwrap();
        assert_eq!(heard.stdout, b"the prompt");
    }
}
