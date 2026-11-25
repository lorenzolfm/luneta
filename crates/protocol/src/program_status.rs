use std::borrow::Borrow;
use std::collections::HashSet;
use std::time::SystemTime;

use crate::mux::v1 as mux;
use crate::said::Said;

pub const RECORDS_MAX: usize = 256;

pub const PROGRESS_PERCENT_MAX: u8 = 100;

pub const ID_BYTES_MAX: usize = 256;

pub const ID_DEPTH_MAX: usize = 16;

pub const APP_BYTES_MAX: usize = 128;

pub const TITLE_BYTES_MAX: usize = 256;

pub const MESSAGE_BYTES_MAX: usize = 1024;

const SANITIZE_INPUT_FACTOR: usize = 4;

const _: () = assert!(RECORDS_MAX >= 64);
const _: () = assert!(PROGRESS_PERCENT_MAX == 100);
const _: () = assert!(ID_BYTES_MAX > 0);
const _: () = assert!(ID_DEPTH_MAX > 0);
const _: () = assert!(APP_BYTES_MAX > 0);
const _: () = assert!(TITLE_BYTES_MAX > 0);
const _: () = assert!(MESSAGE_BYTES_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Progress(u8);

impl Progress {
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value <= PROGRESS_PERCENT_MAX {
            Some(Self(value))
        } else {
            None
        }
    }

    pub fn of_u32(value: u32) -> Result<Self, String> {
        u8::try_from(value)
            .ok()
            .and_then(Self::new)
            .ok_or_else(|| format!("progress {value} is over {PROGRESS_PERCENT_MAX}"))
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ChildId(String);

impl ChildId {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("id is empty".to_owned());
        }
        if raw.len() > ID_BYTES_MAX {
            return Err(format!("id is {} bytes, over {ID_BYTES_MAX}", raw.len()));
        }
        if raw.chars().any(is_invisible_formatting) {
            return Err("id holds control or invisible formatting characters".to_owned());
        }
        let mut depth = 0usize;
        for segment in raw.split('/') {
            if segment.is_empty() {
                return Err("id holds an empty path segment".to_owned());
            }
            depth += 1;
            if depth > ID_DEPTH_MAX {
                return Err(format!("id depth is over {ID_DEPTH_MAX}"));
            }
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn parent(&self) -> Option<&str> {
        self.0.rfind('/').map(|slash| &self.0[..slash])
    }

    #[must_use]
    pub fn is_self_or_child(&self, prefix: &Self) -> bool {
        self.0
            .strip_prefix(prefix.0.as_str())
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    }
}

impl Borrow<str> for ChildId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl From<ChildId> for String {
    fn from(id: ChildId) -> Self {
        id.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AppName(String);

impl AppName {
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("app is empty".to_owned());
        }
        if raw.len() > APP_BYTES_MAX {
            return Err(format!("app is {} bytes, over {APP_BYTES_MAX}", raw.len()));
        }
        if raw.chars().any(is_invisible_formatting) {
            return Err("app holds control or invisible formatting characters".to_owned());
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<AppName> for String {
    fn from(app: AppName) -> Self {
        app.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BoundedText<const MAX: usize>(String);

pub type Title = BoundedText<TITLE_BYTES_MAX>;
pub type Message = BoundedText<MESSAGE_BYTES_MAX>;

impl<const MAX: usize> BoundedText<MAX> {
    pub fn parse(field: &str, raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err(format!("{field} is empty"));
        }
        if raw.len() > MAX {
            return Err(format!("{field} is {} bytes, over {MAX}", raw.len()));
        }
        if raw.chars().any(is_invisible_formatting) {
            return Err(format!("{field} holds control or invisible formatting characters"));
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn from_raw_bytes(bytes: &[u8]) -> Option<Self> {
        let cleaned = sanitize(bytes, MAX);
        (!cleaned.is_empty()).then_some(Self(cleaned))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn to_said(&self) -> Option<Said> {
        Said::new(self.0.trim().to_owned())
    }
}

impl<const MAX: usize> From<BoundedText<MAX>> for String {
    fn from(text: BoundedText<MAX>) -> Self {
        text.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OscBlockedKind {
    Permission,
    Question,
    Auth,
}

impl OscBlockedKind {
    #[must_use]
    pub const fn to_mux(self) -> mux::ProgramStatusKind {
        match self {
            OscBlockedKind::Permission => mux::ProgramStatusKind::Permission,
            OscBlockedKind::Question => mux::ProgramStatusKind::Question,
            OscBlockedKind::Auth => mux::ProgramStatusKind::Auth,
        }
    }

    pub fn of_mux(raw: i32) -> Result<Self, String> {
        match mux::ProgramStatusKind::try_from(raw) {
            Ok(mux::ProgramStatusKind::Permission) => Ok(OscBlockedKind::Permission),
            Ok(mux::ProgramStatusKind::Question) => Ok(OscBlockedKind::Question),
            Ok(mux::ProgramStatusKind::Auth) => Ok(OscBlockedKind::Auth),
            Ok(mux::ProgramStatusKind::Unspecified) => Err("kind is unspecified".to_owned()),
            Err(_) => Err(format!("kind {raw} is unknown")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProgramPhase {
    Idle,
    Working { progress: Option<Progress> },
    Done { message: Option<Message> },
    Blocked { kind: Option<OscBlockedKind>, message: Option<Message> },
    Error { message: Option<Message> },
}

impl ProgramPhase {
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self, ProgramPhase::Working { .. } | ProgramPhase::Blocked { .. })
    }

    #[must_use]
    pub fn into_terminal(self) -> Option<TerminalPhase> {
        match self {
            ProgramPhase::Done { message } => Some(TerminalPhase::Done { message }),
            ProgramPhase::Error { message } => Some(TerminalPhase::Error { message }),
            ProgramPhase::Idle | ProgramPhase::Working { .. } | ProgramPhase::Blocked { .. } => None,
        }
    }

    #[must_use]
    pub fn to_mux(&self) -> mux::program_status::Phase {
        use mux::program_status::Phase;
        match self {
            ProgramPhase::Idle => Phase::Idle(mux::ProgramStatusIdle {}),
            ProgramPhase::Working { progress } => {
                Phase::Working(mux::ProgramStatusWorking { progress: progress.map(|p| u32::from(p.get())) })
            },
            ProgramPhase::Done { message } => {
                Phase::Done(mux::ProgramStatusDone { message: message.clone().map(String::from) })
            },
            ProgramPhase::Blocked { kind, message } => Phase::Blocked(mux::ProgramStatusBlocked {
                kind: kind.map(|k| k.to_mux().into()),
                message: message.clone().map(String::from),
            }),
            ProgramPhase::Error { message } => {
                Phase::Error(mux::ProgramStatusError { message: message.clone().map(String::from) })
            },
        }
    }

    pub fn of_mux(phase: &mux::program_status::Phase) -> Result<Self, String> {
        use mux::program_status::Phase;
        match phase {
            Phase::Idle(_) => Ok(ProgramPhase::Idle),
            Phase::Working(working) => Ok(ProgramPhase::Working {
                progress: working.progress.map(Progress::of_u32).transpose()?,
            }),
            Phase::Done(done) => Ok(ProgramPhase::Done {
                message: done.message.as_deref().map(|m| Message::parse("message", m)).transpose()?,
            }),
            Phase::Blocked(blocked) => Ok(ProgramPhase::Blocked {
                kind: blocked.kind.map(OscBlockedKind::of_mux).transpose()?,
                message: blocked
                    .message
                    .as_deref()
                    .map(|m| Message::parse("message", m))
                    .transpose()?,
            }),
            Phase::Error(error) => Ok(ProgramPhase::Error {
                message: error
                    .message
                    .as_deref()
                    .map(|m| Message::parse("message", m))
                    .transpose()?,
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalPhase {
    Done { message: Option<Message> },
    Error { message: Option<Message> },
}

impl From<TerminalPhase> for ProgramPhase {
    fn from(phase: TerminalPhase) -> Self {
        match phase {
            TerminalPhase::Done { message } => ProgramPhase::Done { message },
            TerminalPhase::Error { message } => ProgramPhase::Error { message },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramStatus {
    pub phase: ProgramPhase,
    pub app: Option<AppName>,
    pub title: Option<Title>,
    pub at: Option<SystemTime>,
}

impl ProgramStatus {
    #[must_use]
    pub fn to_mux(&self) -> mux::ProgramStatus {
        mux::ProgramStatus {
            phase: Some(self.phase.to_mux()),
            app: self.app.clone().map(String::from),
            title: self.title.clone().map(String::from),
            at: self.at.map(Into::into),
        }
    }

    pub fn of_mux(raw: &mux::ProgramStatus) -> Result<Self, String> {
        let phase = raw.phase.as_ref().ok_or_else(|| "phase is missing".to_owned())?;
        let phase = ProgramPhase::of_mux(phase)?;
        let app = raw.app.as_deref().map(AppName::parse).transpose()?;
        let title = raw.title.as_deref().map(|t| Title::parse("title", t)).transpose()?;
        let at = raw
            .at
            .map(|ts| SystemTime::try_from(ts).map_err(|_| "timestamp is out of range".to_owned()))
            .transpose()?;
        Ok(ProgramStatus { phase, app, title, at })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildProgramStatus {
    pub id: ChildId,
    pub status: ProgramStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgramStatuses {
    pub root: Option<ProgramStatus>,
    pub children: Vec<ChildProgramStatus>,
}

impl ProgramStatuses {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.is_none() && self.children.is_empty()
    }

    #[must_use]
    pub fn to_mux(&self) -> Option<mux::ProgramStatuses> {
        if self.is_empty() {
            return None;
        }
        Some(mux::ProgramStatuses {
            root: self.root.as_ref().map(ProgramStatus::to_mux),
            children: self
                .children
                .iter()
                .map(|child| mux::ChildProgramStatus {
                    id: child.id.as_str().to_owned(),
                    status: Some(child.status.to_mux()),
                })
                .collect(),
        })
    }

    pub fn of_mux(
        raw: Option<mux::ProgramStatuses>,
        terminal_id: u32,
        ended: bool,
    ) -> Result<(Self, Vec<String>), String> {
        let Some(raw) = raw else {
            return Ok((ProgramStatuses::default(), Vec::new()));
        };
        if raw.children.len() > RECORDS_MAX {
            return Err(format!(
                "terminal {terminal_id} reported {} child program statuses, over the limit of {RECORDS_MAX}",
                raw.children.len(),
            ));
        }
        let mut warnings = Vec::new();
        let root = match raw.root {
            Some(raw_root) => match ProgramStatus::of_mux(&raw_root) {
                Ok(status) if ended && !matches!(status.phase, ProgramPhase::Done { .. } | ProgramPhase::Error { .. }) => {
                    warnings.push(format!(
                        "terminal {terminal_id}'s ended root program status is non-terminal"
                    ));
                    None
                },
                Ok(status) => Some(status),
                Err(why) => {
                    warnings.push(format!("terminal {terminal_id}'s root program status: {why}"));
                    None
                },
            },
            None => None,
        };
        let mut children = Vec::with_capacity(raw.children.len());
        let mut seen = HashSet::with_capacity(raw.children.len());
        for raw_child in raw.children {
            let id = match ChildId::parse(&raw_child.id) {
                Ok(id) => id,
                Err(why) => {
                    warnings.push(format!("terminal {terminal_id}'s child program status: {why}"));
                    continue;
                },
            };
            let Some(raw_status) = raw_child.status else {
                warnings.push(format!(
                    "terminal {terminal_id}'s child program status {:?} has no status",
                    id.as_str()
                ));
                continue;
            };
            let status = match ProgramStatus::of_mux(&raw_status) {
                Ok(status) if ended && !matches!(status.phase, ProgramPhase::Done { .. } | ProgramPhase::Error { .. }) => {
                    warnings.push(format!(
                        "terminal {terminal_id}'s ended child program status {:?} is non-terminal",
                        id.as_str()
                    ));
                    continue;
                },
                Ok(status) => status,
                Err(why) => {
                    warnings.push(format!(
                        "terminal {terminal_id}'s child program status {:?}: {why}",
                        id.as_str()
                    ));
                    continue;
                },
            };
            if seen.insert(id.clone()) {
                children.push(ChildProgramStatus { id, status });
            } else {
                warnings.push(format!(
                    "terminal {terminal_id}'s child program status {:?} came twice",
                    id.as_str()
                ));
            }
        }
        Ok((ProgramStatuses { root, children }, warnings))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedProgramStatus {
    pub phase: TerminalPhase,
    pub app: Option<AppName>,
    pub title: Option<Title>,
    pub at: Option<SystemTime>,
}

impl EndedProgramStatus {
    #[must_use]
    pub fn to_mux(&self) -> mux::ProgramStatus {
        let phase: ProgramPhase = self.phase.clone().into();
        mux::ProgramStatus {
            phase: Some(phase.to_mux()),
            app: self.app.clone().map(String::from),
            title: self.title.clone().map(String::from),
            at: self.at.map(Into::into),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedChildProgramStatus {
    pub id: ChildId,
    pub status: EndedProgramStatus,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EndedProgramStatuses {
    pub root: Option<EndedProgramStatus>,
    pub children: Vec<EndedChildProgramStatus>,
}

impl EndedProgramStatuses {
    #[must_use]
    pub fn to_mux(&self) -> Option<mux::ProgramStatuses> {
        if self.root.is_none() && self.children.is_empty() {
            return None;
        }
        Some(mux::ProgramStatuses {
            root: self.root.as_ref().map(EndedProgramStatus::to_mux),
            children: self
                .children
                .iter()
                .map(|child| mux::ChildProgramStatus {
                    id: child.id.as_str().to_owned(),
                    status: Some(child.status.to_mux()),
                })
                .collect(),
        })
    }
}

#[must_use]
pub fn sanitize(bytes: &[u8], bytes_max: usize) -> String {
    assert!(bytes_max > 0);
    let raw_limit = bytes_max.saturating_mul(SANITIZE_INPUT_FACTOR);
    let bounded = &bytes[..bytes.len().min(raw_limit)];
    let text = String::from_utf8_lossy(bounded);
    let mut cleaned = String::with_capacity(bounded.len().min(bytes_max));
    for character in text.chars() {
        if is_invisible_formatting(character) {
            continue;
        }
        if cleaned.len() + character.len_utf8() > bytes_max {
            break;
        }
        cleaned.push(character);
    }
    assert!(cleaned.len() <= bytes_max);
    cleaned
}

#[must_use]
pub fn is_invisible_formatting(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_id_validates_segments_depth_length_and_characters() {
        assert!(ChildId::parse("").is_err());
        assert!(ChildId::parse("/").is_err());
        assert!(ChildId::parse("/a").is_err());
        assert!(ChildId::parse("a/").is_err());
        assert!(ChildId::parse("a//b").is_err());
        assert!(ChildId::parse("a\u{202e}b").is_err());
        assert!(ChildId::parse("a\nb").is_err());
        assert!(ChildId::parse(&"a".repeat(ID_BYTES_MAX + 1)).is_err());
        let too_deep = (0..=ID_DEPTH_MAX).map(|i| format!("s{i}")).collect::<Vec<_>>().join("/");
        assert!(ChildId::parse(&too_deep).is_err());

        let id = ChildId::parse("build/test/unit").unwrap();
        assert_eq!(id.parent(), Some("build/test"));
        let prefix = ChildId::parse("build").unwrap();
        let sibling = ChildId::parse("buildx").unwrap();
        assert!(id.is_self_or_child(&prefix));
        assert!(prefix.is_self_or_child(&prefix));
        assert!(!sibling.is_self_or_child(&prefix));
    }

    #[test]
    fn sanitize_strips_controls_and_invisible_formatting_characters() {
        let input = "allow \u{202e}rm -rf\u{202c}\u{200b}\u{feff}\u{00ad}\u{2066}safe\u{2069}\x07\n";
        assert_eq!(sanitize(input.as_bytes(), MESSAGE_BYTES_MAX), "allow rm -rfsafe");
        assert_eq!(sanitize("café 👍".as_bytes(), MESSAGE_BYTES_MAX), "café 👍");
        assert_eq!(sanitize("café".as_bytes(), 4), "caf");
    }

    #[test]
    fn progress_and_bounded_text_reject_out_of_range_and_invisible_chars() {
        assert_eq!(Progress::of_u32(0).unwrap().get(), 0);
        assert_eq!(Progress::of_u32(100).unwrap().get(), 100);
        assert!(Progress::of_u32(101).is_err());
        assert!(AppName::parse("").is_err());
        assert!(AppName::parse("claude\x07").is_err());
        assert!(Message::parse("message", "").is_err());
        assert!(Message::parse("message", "bad\u{202e}text").is_err());
        assert!(Message::parse("message", &"x".repeat(MESSAGE_BYTES_MAX + 1)).is_err());
    }
}
