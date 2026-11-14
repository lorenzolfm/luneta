use std::fs;
use std::io::{self, ErrorKind, Read as _};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use luneta_protocol::said::Said;
use serde::Deserialize as _;

use crate::api;
use crate::api::agent::Status as Reported;
use crate::api::agent::WaitingFor as Waits;
use crate::mux::{Named, Sessions, ZellijPane};

const FILE_BYTES_MAX: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    pub harness: Harness,
    pub session_id: Said,
    pub cwd: Said,
    pub status: Status,
    pub status_since: Option<SystemTime>,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    pub entrypoint: Option<Said>,
    pub pane: Option<Named>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Harness {
    ClaudeCode,
    Pi,
}

impl Harness {
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
    Waiting(Option<WaitingFor>),
    Shell,
    Other(Said),
    Unreported,
}

impl Status {
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitingFor {
    PermissionPrompt,
    InputNeeded,
    DialogOpen,
    Other(Said),
}

impl WaitingFor {
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

fn said(word: Option<&str>) -> Option<Said> {
    Said::new(word?.trim().to_owned())
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
    pub started: Option<SystemTime>,
    pub pane: Option<Named>,
}

pub fn named_pid(path: &Path) -> Option<u32> {
    if path.extension()? != "json" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let pid = stem.parse::<u32>().ok()?;
    if pid.to_string() != stem {
        return None;
    }
    (pid > 1).then_some(pid)
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

#[must_use]
pub fn moment(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

impl Agent {
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions, topic: Option<Said>) -> api::Agent {
        assert!(self.pid > 1);
        let known = |status: api::Status| Some(Reported::KnownStatus(status.into()));
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
            pane: self.pane.as_ref().map(|named| match named {
                Named::Zellij(ZellijPane { session: named, pane }) => {
                    api::agent::Pane::Zellij(api::Zellij {
                        named: named.clone(),
                        session: sessions.lead(named, self.started_at).map(ToString::to_string),
                        pane: *pane,
                    })
                },
                Named::Own(own) => api::agent::Pane::Own((*own).into()),
            }),
            topic: topic.map(String::from),
        }
    }
}

impl Unread {
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions) -> api::UnreadFile {
        assert!(self.pid > 1);
        api::UnreadFile {
            pid: self.pid,
            zellij: self.pane.as_ref().and_then(zellij_of).map(|(named, pane)| api::Zellij {
                named: named.to_owned(),
                session: self.started.and_then(|started| {
                    sessions.lead(named, Some(started)).map(ToString::to_string)
                }),
                pane,
            }),
            own: self.pane.as_ref().and_then(|named| match named {
                Named::Own(own) => Some((*own).into()),
                Named::Zellij(_) => None,
            }),
        }
    }
}

fn zellij_of(named: &Named) -> Option<(&str, u32)> {
    match named {
        Named::Zellij(ZellijPane { session, pane }) => Some((session, *pane)),
        Named::Own(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
