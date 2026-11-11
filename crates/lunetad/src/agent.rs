//! What the daemon knows of an agent, whichever adapter found it: Claude Code's,
//! or pi's. And what those adapters' files have in common: each agent has one,
//! named for its pid, that says little.

use std::fs;
use std::io::{self, ErrorKind, Read as _};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::said::Said;
use serde::Deserialize as _;

use crate::api;
use crate::api::agent::Status as Reported;
use crate::api::agent::WaitingFor as Waits;
use crate::process;
use crate::mux::Sessions;

/// The most an agent's file holds that an adapter reads. Each holds a few hundred
/// bytes: a file past this is no agent's.
const FILE_BYTES_MAX: u64 = 64 * 1024;

/// An agent that runs on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    /// The harness it runs in, whose adapter found it.
    pub harness: Harness,
    /// The agent's own id for its session.
    pub session_id: Said,
    /// Where it works.
    pub cwd: Said,
    pub status: Status,
    /// When it came to its status, as the agent dates it.
    pub status_since: Option<SystemTime>,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    /// How it was started, in its harness's word, which only its harness tells
    /// the meaning of: `cli` is Claude Code's for the agent a person talks to at
    /// a prompt, and `tui` is pi's.
    pub entrypoint: Option<Said>,
    /// The pane its environment names, when it runs in zellij.
    pub pane: Option<Named>,
}

/// A harness an agent can run in, as the adapter for it knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Pi,
}

impl Harness {
    /// The harness a request names by `said`: Claude Code where it names none,
    /// as a luneta older than the field asks for. Or why not, for one this
    /// lunetad does not know.
    pub fn said(said: Option<i32>) -> Result<Harness, String> {
        match said.map(api::Harness::try_from) {
            None | Some(Ok(api::Harness::ClaudeCode)) => Ok(Harness::ClaudeCode),
            Some(Ok(api::Harness::Pi)) => Ok(Harness::Pi),
            Some(Ok(api::Harness::Unspecified) | Err(_)) => {
                Err("a harness this lunetad does not know".to_owned())
            },
        }
    }

    #[must_use]
    pub fn to_api(self) -> api::Harness {
        match self {
            Harness::ClaudeCode => api::Harness::ClaudeCode,
            Harness::Pi => api::Harness::Pi,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    /// It waits on you, for what it says, where it says.
    Waiting(Option<WaitingFor>),
    Shell,
    /// A word not known here, as the agent wrote it: a newer agent's.
    Other(Said),
    /// The agent says none.
    Unreported,
}

impl Status {
    /// Claude Code's word for it, which luneta's extension for pi speaks too,
    /// and for what it waits for, where it waits, in any case.
    #[must_use]
    pub fn of(word: Option<&str>, waiting_for: Option<&str>) -> Self {
        let Some(word) = said(word) else {
            return Status::Unreported;
        };
        match word.as_str().to_ascii_lowercase().as_str() {
            "busy" => Status::Busy,
            "idle" => Status::Idle,
            "waiting" => Status::Waiting(said(waiting_for).map(WaitingFor::of)),
            "shell" => Status::Shell,
            _ => Status::Other(word),
        }
    }
}

/// What an agent waits for, in the words Claude Code says it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitingFor {
    /// A tool it asks to run, or a plan it asks you to take.
    PermissionPrompt,
    /// A question of its own, or of a server's.
    InputNeeded,
    /// One of its own dialogs, which you opened.
    DialogOpen,
    /// A word not known here, as the agent wrote it.
    Other(Said),
}

impl WaitingFor {
    /// Claude Code's word for it, in any case.
    fn of(word: Said) -> Self {
        match word.as_str().to_ascii_lowercase().as_str() {
            "permission prompt" => WaitingFor::PermissionPrompt,
            "input needed" => WaitingFor::InputNeeded,
            "dialog open" => WaitingFor::DialogOpen,
            _ => WaitingFor::Other(word),
        }
    }

    fn to_api(&self) -> Waits {
        let known = |waits: api::WaitingFor| Waits::KnownWaitingFor(waits.into());
        match self {
            WaitingFor::PermissionPrompt => known(api::WaitingFor::PermissionPrompt),
            WaitingFor::InputNeeded => known(api::WaitingFor::InputNeeded),
            WaitingFor::DialogOpen => known(api::WaitingFor::DialogOpen),
            WaitingFor::Other(word) => Waits::OtherWaitingFor(word.clone().into()),
        }
    }
}

/// `word` without the white space around it, where that leaves any.
fn said(word: Option<&str>) -> Option<Said> {
    Said::new(word?.trim().to_owned())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: Said,
    /// Who chose it, when the agent says.
    pub source: Option<Source>,
}

impl Name {
    fn to_api(&self) -> api::Name {
        let source = self.source.map(|source| source.to_api().into());
        api::Name { text: self.text.clone().into(), source }
    }

    /// Whether someone gave it, as a row reads it.
    #[must_use]
    pub fn given(&self) -> bool {
        self.to_api().given().is_some()
    }
}

/// Who chose a name, in the words Claude Code reads back itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Someone gave it.
    User,
    /// Someone gave it.
    Peer,
    /// Claude Code's own, from the directory.
    Derived,
    Collision,
    Auto,
    Hook,
    /// A word not known here: a newer Claude Code's.
    Other,
}

impl Source {
    /// Claude Code's word for it, in any case.
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

/// A session file of an agent that does not read, by the process that runs by
/// the pid it is named for: the agent's, or another that came by the pid
/// since, as the file cannot say. What the agent is doing, no one can tell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unread {
    pub pid: u32,
    /// When the process started, or a moment after, and never before: less
    /// than a clock tick on Linux, less than a second on macOS. So a session
    /// made a moment before it is not ruled out, and one made a moment after
    /// may not be, which only leaves its session untold. None where the
    /// system does not say.
    pub started: Option<SystemTime>,
    /// The pane its environment names, when it runs in zellij.
    pub pane: Option<Named>,
}

/// A pane as an agent's environment names it: by the name its session had when
/// the pane opened, and the pane's id there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub session: String,
    pub pane: u32,
}

/// The pane the environment of `pid` names, if it names one.
#[must_use]
pub fn pane_of(pid: u32) -> Option<Named> {
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

/// The pid an agent's file is named for: `<pid>.json`, spelled as a harness
/// spells a pid, which is how it reads one back. Any other file in the folder,
/// a copy of one too, names no agent.
pub fn named_pid(path: &Path) -> Option<u32> {
    if path.extension()? != "json" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let pid = stem.parse::<u32>().ok()?;
    if pid.to_string() != stem {
        return None;
    }
    // No agent runs as the kernel's pid 0 or as init's 1.
    (pid > 1).then_some(pid)
}

/// What a file holds, if it holds no more than an agent's file does.
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

/// Text an agent's file holds where it must hold some, as `Said`: an empty
/// string is none.
pub fn nonempty<'de, D: serde::Deserializer<'de>>(text: D) -> Result<Said, D::Error> {
    Said::new(String::deserialize(text)?).ok_or_else(|| serde::de::Error::custom("it is empty"))
}

/// A time as an agent's file has it, in milliseconds since the epoch.
#[must_use]
pub fn moment(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

impl Agent {
    /// The agent as the API gives it, in the session `sessions` say its pane
    /// is in now, with the topic the daemon keeps of it, if it keeps one.
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions, topic: Option<Said>) -> api::Agent {
        // The adapter takes no file of pid 0 or 1 for an agent's, and leaves
        // out what is empty: the protocol says it is absent instead.
        assert!(self.pid > 1);
        let known = |status: api::Status| Some(Reported::KnownStatus(status.into()));
        // What it waits for goes only with a wait.
        let (status, waiting_for) = match &self.status {
            Status::Busy => (known(api::Status::Busy), None),
            Status::Idle => (known(api::Status::Idle), None),
            Status::Waiting(waits) => {
                (known(api::Status::Waiting), waits.as_ref().map(WaitingFor::to_api))
            },
            Status::Shell => (known(api::Status::Shell), None),
            Status::Other(word) => (Some(Reported::OtherStatus(word.clone().into())), None),
            Status::Unreported => (None, None),
        };
        api::Agent {
            pid: self.pid,
            harness: Some(self.harness.to_api().into()),
            session_id: self.session_id.clone().into(),
            cwd: self.cwd.clone().into(),
            status,
            status_since: self.status_since.map(Into::into),
            started_at: self.started_at.map(Into::into),
            name: self.name.as_ref().map(Name::to_api),
            entrypoint: self.entrypoint.clone().map(String::from),
            waiting_for,
            zellij: self.pane.as_ref().map(|named| api::Zellij {
                named: named.session.clone(),
                session: sessions.lead(&named.session, self.started_at).map(str::to_owned),
                pane: named.pane,
            }),
            topic: topic.map(String::from),
        }
    }
}

impl Unread {
    /// The file as the API gives it, in the session `sessions` say its pane is
    /// in now. Where the system does not say when the process started, no
    /// session made since can be ruled out, and so its session is not told.
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions) -> api::UnreadFile {
        assert!(self.pid > 1);
        api::UnreadFile {
            pid: self.pid,
            zellij: self.pane.as_ref().map(|named| api::Zellij {
                named: named.session.clone(),
                session: self.started.and_then(|started| {
                    sessions.lead(&named.session, Some(started)).map(str::to_owned)
                }),
                pane: named.pane,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request that names no harness asks for Claude Code's, as an older
    /// luneta's does, and one that names a harness only a newer luneta knows,
    /// or none by name, is refused.
    #[test]
    fn a_request_names_a_harness_this_lunetad_knows() {
        assert_eq!(Harness::said(None), Ok(Harness::ClaudeCode));
        for harness in [Harness::ClaudeCode, Harness::Pi] {
            assert_eq!(Harness::said(Some(harness.to_api().into())), Ok(harness));
        }
        for unknown in [api::Harness::Unspecified.into(), 99] {
            assert!(Harness::said(Some(unknown)).is_err(), "{unknown}");
        }
    }
}
