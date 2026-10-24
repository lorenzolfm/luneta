//! What the daemon knows of an agent, whichever adapter found it. Claude Code's
//! is the only one so far.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::api;
use crate::zellij::Sessions;

/// An agent that runs on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    /// The agent's own id for its session.
    pub session_id: String,
    pub cwd: PathBuf,
    pub status: Status,
    /// When it came to its status, as the agent dates it.
    pub status_since: Option<SystemTime>,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    /// How it was started, in the agent's own word.
    pub entrypoint: Option<String>,
    /// The pane its environment names, when it runs in zellij.
    pub pane: Option<Named>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    Waiting,
    /// One the agent does not report, or reports in a word not known here.
    Unknown,
}

impl Status {
    /// Claude Code's word for it.
    #[must_use]
    pub fn of(word: Option<&str>) -> Self {
        match word.map(str::trim) {
            Some("busy") => Status::Busy,
            Some("idle") => Status::Idle,
            Some("waiting") => Status::Waiting,
            _ => Status::Unknown,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    /// Who chose it, in the agent's own word.
    pub source: Option<String>,
}

/// A pane as an agent's environment names it: by the name its session had when
/// the pane opened, and the pane's id there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    pub session: String,
    pub pane: u32,
}

impl Agent {
    /// The agent as the API gives it, in the session `sessions` say its pane
    /// is in now.
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions) -> api::Agent {
        let status = match self.status {
            Status::Busy => api::Status::Busy,
            Status::Idle => api::Status::Idle,
            Status::Waiting => api::Status::Waiting,
            Status::Unknown => api::Status::Unspecified,
        };
        api::Agent {
            pid: self.pid,
            session_id: self.session_id.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            status: status.into(),
            status_since: self.status_since.map(Into::into),
            started_at: self.started_at.map(Into::into),
            name: self.name.as_ref().map(|name| api::Name {
                text: name.text.clone(),
                source: name.source.clone().unwrap_or_default(),
            }),
            entrypoint: self.entrypoint.clone().unwrap_or_default(),
            zellij: self.pane.as_ref().map(|named| api::Zellij {
                named: named.session.clone(),
                session: sessions.lead(&named.session).unwrap_or_default().to_owned(),
                pane: named.pane,
            }),
        }
    }
}
