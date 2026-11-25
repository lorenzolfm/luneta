use std::fs;
use std::io::{self, ErrorKind, Read as _};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lune_domain::agent::Harness;
use lune_domain::mux::TerminalKey;
use lune_protocol::said::Said;
use serde::Deserialize as _;

use crate::api;
use crate::process::{AgentId, AgentPid};
use crate::statuses::{ActiveStatus, Decided, Subagent};
use crate::watch::Troubles;
use crate::Directory;

pub const FILE_BYTES_MAX: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenAgent {
    pub id: AgentId,
    pub harness: Harness,
    pub session_id: Option<Said>,
    pub cwd: Directory,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    pub entrypoint: Option<Said>,
    pub terminal: Option<TerminalKey>,
}

impl SeenAgent {
    #[must_use]
    pub fn at_a_prompt(&self) -> bool {
        let word = self.harness.to_api().prompt_word();
        word.is_some_and(|word| self.entrypoint.as_ref().map(Said::as_str) == Some(word))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pid: AgentPid,
    pub harness: Harness,
    pub session_id: Option<Said>,
    pub cwd: Directory,
    pub status: Decided,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    pub entrypoint: Option<Said>,
    pub terminal: Option<TerminalKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: Said,
    pub source: Option<Source>,
}

impl Name {
    fn to_api(&self) -> api::Name {
        let source = self.source.map(|source| source.to_api().into());
        api::Name { text: self.text.clone().into(), source }
    }

    #[must_use]
    pub fn given(&self) -> bool {
        self.to_api().given().is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Peer,
    Derived,
    Collision,
    Auto,
    Hook,
    Other,
}

impl Source {
    #[must_use]
    pub fn of(word: &str) -> Self {
        match word.to_ascii_lowercase().as_str() {
            "user" => Source::User,
            "peer" => Source::Peer,
            "derived" => Source::Derived,
            "collision" => Source::Collision,
            "auto" => Source::Auto,
            "hook" => Source::Hook,
            _ => Source::Other,
        }
    }

    fn to_api(self) -> api::NameSource {
        match self {
            Source::User => api::NameSource::User,
            Source::Peer => api::NameSource::Peer,
            Source::Derived => api::NameSource::Derived,
            Source::Collision => api::NameSource::Collision,
            Source::Auto => api::NameSource::Auto,
            Source::Hook => api::NameSource::Hook,
            Source::Other => api::NameSource::Other,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    pub pid: u32,
    pub terminal: Option<TerminalKey>,
}

pub fn named_pid(path: &Path) -> Option<AgentPid> {
    if path.extension()? != "json" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let pid = stem.parse::<u32>().ok()?;
    if pid.to_string() != stem {
        return None;
    }
    AgentPid::new(pid)
}

pub fn file_text(path: &Path) -> io::Result<Vec<u8>> {
    let mut text = Vec::new();
    fs::File::open(path)?.take(FILE_BYTES_MAX + 1).read_to_end(&mut text)?;
    let bytes = u64::try_from(text.len()).expect("a length under a u64 limit fits a u64");
    if bytes <= FILE_BYTES_MAX {
        Ok(text)
    } else {
        let why = format!("it holds more than {FILE_BYTES_MAX} bytes");
        Err(io::Error::new(ErrorKind::FileTooLarge, why))
    }
}

pub fn nonempty<'de, D: serde::Deserializer<'de>>(text: D) -> Result<Said, D::Error> {
    Said::new(String::deserialize(text)?).ok_or_else(|| serde::de::Error::custom("it is empty"))
}

pub fn absolute<'de, D: serde::Deserializer<'de>>(text: D) -> Result<Directory, D::Error> {
    Directory::parse(String::deserialize(text)?).map_err(serde::de::Error::custom)
}

pub fn terminal_or_trouble(
    pid: AgentPid,
    read: Result<Option<TerminalKey>, String>,
    troubles: &mut Troubles,
) -> Option<TerminalKey> {
    match read {
        Ok(terminal) => terminal,
        Err(why) => {
            troubles.say(format!("agent {pid} shows in no view, as {why}"));
            None
        },
    }
}

#[must_use]
pub fn moment(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

impl Agent {
    #[must_use]
    pub fn is_working(&self) -> bool {
        matches!(self.status, Decided::Reported { status: ActiveStatus::Working, .. })
    }

    #[must_use]
    pub fn status_since(&self) -> Option<SystemTime> {
        match &self.status {
            Decided::Reported { since, .. } => *since,
            Decided::Unreported => None,
        }
    }

    #[must_use]
    pub fn to_api(&self, topic: Option<Said>) -> api::Agent {
        let (status, waiting_for, reported_status, status_since, status_rung, subagents) =
            match &self.status {
                Decided::Unreported => (None, None, None, None, None, Vec::new()),
                Decided::Reported { status, since, rung, subagents } => {
                    let (legacy_status, waiting_for, reported) = status.to_api();
                    (
                        Some(legacy_status),
                        waiting_for,
                        Some(reported),
                        since.map(Into::into),
                        Some(rung.to_api().into()),
                        subagents.iter().map(Subagent::to_api).collect(),
                    )
                },
            };
        api::Agent {
            pid: self.pid.get(),
            harness: Some(self.harness.to_api().into()),
            session_id: self.session_id.clone().map(String::from),
            cwd: self.cwd.clone().into(),
            status,
            status_since,
            started_at: self.started_at.map(Into::into),
            name: self.name.as_ref().map(Name::to_api),
            entrypoint: self.entrypoint.clone().map(String::from),
            waiting_for,
            terminal: self.terminal.map(Into::into),
            topic: topic.map(String::from),
            reported_status,
            status_rung,
            subagents,
        }
    }
}

impl Unread {
    #[must_use]
    pub fn to_api(&self) -> api::UnreadFile {
        api::UnreadFile { pid: self.pid, terminal: self.terminal.map(Into::into) }
    }
}
