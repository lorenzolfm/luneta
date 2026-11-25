use std::collections::BTreeMap;
use std::time::SystemTime;

use libghostty_vt::effect::{ProgramStatus, ProgramStatusKind, ProgramStatusState};
pub use lune_protocol::{
    PROGRAM_STATUS_PROGRESS_PERCENT_MAX as PROGRESS_PERCENT_MAX,
    PROGRAM_STATUS_RECORDS_MAX as RECORDS_MAX,
};

use crate::api;

pub const ID_BYTES_MAX: usize = 256;

pub const ID_DEPTH_MAX: usize = 16;

pub const APP_BYTES_MAX: usize = 128;

pub const TITLE_BYTES_MAX: usize = 256;

pub const MESSAGE_BYTES_MAX: usize = 1024;

const SANITIZE_INPUT_FACTOR: usize = 4;

const _: () = assert!(RECORDS_MAX >= 64);
const _: () = assert!(ID_BYTES_MAX > 0);
const _: () = assert!(ID_DEPTH_MAX > 0);
const _: () = assert!(APP_BYTES_MAX > 0);
const _: () = assert!(TITLE_BYTES_MAX > 0);
const _: () = assert!(MESSAGE_BYTES_MAX > 0);
const _: () = assert!(PROGRESS_PERCENT_MAX == 100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    fn to_api(self) -> api::ProgramStatusState {
        match self {
            State::Idle => api::ProgramStatusState::Idle,
            State::Working => api::ProgramStatusState::Working,
            State::Done => api::ProgramStatusState::Done,
            State::Blocked => api::ProgramStatusState::Blocked,
            State::Error => api::ProgramStatusState::Error,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    None,
    Permission,
    Question,
    Auth,
}

impl Kind {
    fn to_api(self) -> api::ProgramStatusKind {
        match self {
            Kind::None => api::ProgramStatusKind::Unspecified,
            Kind::Permission => api::ProgramStatusKind::Permission,
            Kind::Question => api::ProgramStatusKind::Question,
            Kind::Auth => api::ProgramStatusKind::Auth,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    state: State,
    kind: Kind,
    progress: Option<u8>,
    app: Option<String>,
    title: String,
    message: String,
    at: SystemTime,
    seq: u64,
}

#[derive(Default)]
pub struct ProgramStatuses {
    records: BTreeMap<String, Record>,
    seq: u64,
    version: u64,
}

impl ProgramStatuses {
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn apply(&mut self, report: ProgramStatus<'_>, at: SystemTime) {
        let Ok(raw_state) = report.state() else {
            return;
        };
        let Ok(id) = std::str::from_utf8(report.id()) else {
            return;
        };
        if id.len() > ID_BYTES_MAX {
            return;
        }
        if id_depth(id) > ID_DEPTH_MAX {
            return;
        }
        if raw_state == ProgramStatusState::Clear {
            let before = self.records.len();
            if id.is_empty() {
                self.records.clear();
            } else {
                self.records.retain(|key, _| !is_self_or_child(key, id));
            }
            assert!(self.records.len() <= before);
            if self.records.len() != before {
                self.bump_version();
            }
            return;
        }
        let state = match raw_state {
            ProgramStatusState::Idle => State::Idle,
            ProgramStatusState::Working => State::Working,
            ProgramStatusState::Done => State::Done,
            ProgramStatusState::Blocked => State::Blocked,
            ProgramStatusState::Error => State::Error,
            _ => return,
        };
        let kind = if state == State::Blocked {
            match report.kind() {
                Ok(ProgramStatusKind::Permission) => Kind::Permission,
                Ok(ProgramStatusKind::Question) => Kind::Question,
                Ok(ProgramStatusKind::Auth) => Kind::Auth,
                _ => Kind::None,
            }
        } else {
            Kind::None
        };
        if state != State::Blocked {
            assert_eq!(kind, Kind::None);
        }
        let progress = report.progress().filter(|value| *value <= PROGRESS_PERCENT_MAX);
        if let Some(value) = progress {
            assert!(value <= PROGRESS_PERCENT_MAX);
        }
        let app = std::str::from_utf8(report.app())
            .ok()
            .filter(|app| !app.is_empty() && app.len() <= APP_BYTES_MAX)
            .map(str::to_owned);
        if let Some(value) = &app {
            assert_ne!(value, "");
            assert!(value.len() <= APP_BYTES_MAX);
        }
        let title = sanitize(report.title(), TITLE_BYTES_MAX);
        let message = sanitize(report.message(), MESSAGE_BYTES_MAX);
        assert!(title.len() <= TITLE_BYTES_MAX);
        assert!(message.len() <= MESSAGE_BYTES_MAX);
        if !self.records.contains_key(id) && self.records.len() >= RECORDS_MAX {
            let oldest = self
                .records
                .iter()
                .min_by_key(|(_, record)| record.seq)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                let removed = self.records.remove(&oldest);
                assert!(removed.is_some());
            }
        }
        if !self.records.contains_key(id) {
            assert!(self.records.len() < RECORDS_MAX);
        }
        self.seq = self.seq.checked_add(1).expect("program statuses update fewer than 2^64 times");
        let record = Record { state, kind, progress, app, title, message, at, seq: self.seq };
        self.records.insert(id.to_owned(), record);
        self.bump_version();
        assert!(self.records.len() <= RECORDS_MAX);
        assert!(self.records.contains_key(id));
    }

    pub fn prompt_started(&mut self) {
        let before = self.records.len();
        self.records.retain(|_, record| !matches!(record.state, State::Working | State::Blocked));
        assert!(self.records.len() <= before);
        assert!(
            self.records
                .values()
                .all(|record| !matches!(record.state, State::Working | State::Blocked))
        );
        if self.records.len() != before {
            self.bump_version();
        }
    }

    pub fn exited(&mut self) {
        let before = self.records.len();
        self.records.retain(|_, record| matches!(record.state, State::Done | State::Error));
        assert!(self.records.len() <= before);
        assert!(
            self.records.values().all(|record| matches!(record.state, State::Done | State::Error))
        );
        if self.records.len() != before {
            self.bump_version();
        }
    }

    #[must_use]
    pub fn said(&self) -> Vec<api::ProgramStatus> {
        assert!(self.records.len() <= RECORDS_MAX);
        let mut statuses = Vec::with_capacity(self.records.len());
        for (id, record) in &self.records {
            statuses.push(api::ProgramStatus {
                id: id.clone(),
                state: record.state.to_api().into(),
                kind: record.kind.to_api().into(),
                progress: record.progress.map(u32::from),
                app: self.resolved_app(id).unwrap_or_default().to_owned(),
                title: record.title.clone(),
                message: record.message.clone(),
                at: Some(record.at.into()),
            });
        }
        assert_eq!(statuses.len(), self.records.len());
        statuses
    }

    fn bump_version(&mut self) {
        self.version =
            self.version.checked_add(1).expect("program statuses change fewer than 2^64 times");
    }

    fn resolved_app(&self, id: &str) -> Option<&str> {
        assert!(id.len() <= ID_BYTES_MAX);
        assert!(id_depth(id) <= ID_DEPTH_MAX);
        let mut cursor = Some(id);
        let mut steps = 0usize;
        while let Some(key) = cursor {
            steps += 1;
            assert!(steps <= ID_DEPTH_MAX + 1);
            if let Some(app) = self.records.get(key).and_then(|record| record.app.as_deref()) {
                assert_ne!(app, "");
                return Some(app);
            }
            if key.is_empty() {
                break;
            }
            let next = match key.rfind('/') {
                Some(slash) => &key[..slash],
                None => "",
            };
            assert!(next.len() < key.len());
            cursor = Some(next);
        }
        None
    }
}

fn id_depth(id: &str) -> usize {
    assert!(id.len() <= ID_BYTES_MAX);
    if id.is_empty() {
        return 0;
    }
    id.bytes().filter(|&byte| byte == b'/').count() + 1
}

fn is_self_or_child(key: &str, prefix: &str) -> bool {
    key.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
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

fn is_invisible_formatting(character: char) -> bool {
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
    fn sanitize_strips_controls_and_invisible_formatting_characters() {
        let input = "allow \u{202e}rm -rf\u{202c}\u{200b}\u{feff}\u{00ad}\u{2066}safe\u{2069}\x07\n";
        assert_eq!(sanitize(input.as_bytes(), MESSAGE_BYTES_MAX), "allow rm -rfsafe");
        assert_eq!(sanitize("café 👍".as_bytes(), MESSAGE_BYTES_MAX), "café 👍");
        assert_eq!(sanitize("café".as_bytes(), 4), "caf");
    }

    #[test]
    fn child_prefix_matches_only_self_and_descendants() {
        assert!(is_self_or_child("build", "build"));
        assert!(is_self_or_child("build/test", "build"));
        assert!(is_self_or_child("build/test/unit", "build"));
        assert!(!is_self_or_child("buildx", "build"));
        assert!(!is_self_or_child("", "build"));
    }

    #[test]
    fn id_depth_counts_segments_within_bounds() {
        assert_eq!(id_depth(""), 0);
        assert_eq!(id_depth("agent"), 1);
        assert_eq!(id_depth("agent/sub/leaf"), 3);
    }
}
