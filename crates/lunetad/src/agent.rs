//! What the daemon knows of an agent, whichever adapter found it. Claude Code's
//! is the only one so far.

use std::time::SystemTime;

use luneta_protocol::said::Said;

use crate::api;
use crate::api::agent::Status as Reported;
use crate::api::agent::WaitingFor as Waits;
use crate::links::Link;
use crate::zellij::Sessions;

/// An agent that runs on the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub pid: u32,
    /// When its process started, as the system says it: the system can give
    /// its pid to another once it has ended.
    pub start: String,
    /// The agent's own id for its session.
    pub session_id: Said,
    /// Where it works.
    pub cwd: Said,
    pub status: Status,
    /// When it came to its status, as the agent dates it.
    pub status_since: Option<SystemTime>,
    pub started_at: Option<SystemTime>,
    pub name: Option<Name>,
    /// How it was started, in the agent's own word.
    pub entrypoint: Option<Said>,
    /// The pane its environment names, when it runs in zellij.
    pub pane: Option<Named>,
    /// The pull request of the review its environment says it runs, if any.
    pub review: Option<Link>,
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
    /// Claude Code's word for it, and for what it waits for, where it waits,
    /// in any case.
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

impl Agent {
    /// The agent as the API gives it, in the session `sessions` say its pane
    /// is in now.
    #[must_use]
    pub fn to_api(&self, sessions: &Sessions, review: Option<api::Review>) -> api::Agent {
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
        let source = |source| -> api::NameSource {
            match source {
                Source::User => api::NameSource::User,
                Source::Peer => api::NameSource::Peer,
                Source::Derived => api::NameSource::Derived,
                Source::Collision => api::NameSource::Collision,
                Source::Auto => api::NameSource::Auto,
                Source::Hook => api::NameSource::Hook,
                Source::Other => api::NameSource::Other,
            }
        };
        api::Agent {
            pid: self.pid,
            session_id: self.session_id.clone().into(),
            cwd: self.cwd.clone().into(),
            status,
            status_since: self.status_since.map(Into::into),
            started_at: self.started_at.map(Into::into),
            name: self.name.as_ref().map(|name| {
                let source = name.source.map(|chose| source(chose).into());
                api::Name { text: name.text.clone().into(), source }
            }),
            entrypoint: self.entrypoint.clone().map(String::from),
            waiting_for,
            zellij: self.pane.as_ref().map(|named| api::Zellij {
                named: named.session.clone(),
                session: sessions.lead(&named.session, self.started_at).map(str::to_owned),
                pane: named.pane,
            }),
            review,
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
