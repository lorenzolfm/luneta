use std::collections::BTreeMap;
use std::time::SystemTime;

use libghostty_vt::effect::{ProgramStatus as VtStatus, ProgramStatusKind, ProgramStatusState};
#[cfg(test)]
pub use lune_protocol::program_status::TerminalPhase;
pub use lune_protocol::program_status::{
    AppName, ChildId, ChildProgramStatus, EndedChildProgramStatus, EndedProgramStatus,
    EndedProgramStatuses, Message, OscBlockedKind, ProgramPhase, ProgramStatus,
    ProgramStatuses as Snapshot, Progress, Title, ID_DEPTH_MAX, RECORDS_MAX,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    phase: ProgramPhase,
    app: Option<AppName>,
    title: Option<Title>,
    at: SystemTime,
    seq: u64,
}

enum Target {
    Root,
    Child(ChildId),
}

#[derive(Default)]
pub struct ProgramStatuses {
    root: Option<Record>,
    children: BTreeMap<ChildId, Record>,
    seq: u64,
    version: u64,
}

impl ProgramStatuses {
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn apply(&mut self, report: VtStatus<'_>, at: SystemTime) {
        let Ok(raw_state) = report.state() else {
            return;
        };
        let Ok(raw_id) = std::str::from_utf8(report.id()) else {
            return;
        };
        let target = if raw_id.is_empty() {
            Target::Root
        } else {
            let Ok(id) = ChildId::parse(raw_id) else {
                return;
            };
            Target::Child(id)
        };
        if raw_state == ProgramStatusState::Clear {
            let changed = match target {
                Target::Root => {
                    let had_root = self.root.take().is_some();
                    let had_children = !self.children.is_empty();
                    self.children.clear();
                    had_root || had_children
                },
                Target::Child(prefix) => {
                    let before = self.children.len();
                    self.children.retain(|key, _| !key.is_self_or_child(&prefix));
                    self.children.len() != before
                },
            };
            if changed {
                self.bump_version();
            }
            return;
        }
        let phase = match raw_state {
            ProgramStatusState::Idle => ProgramPhase::Idle,
            ProgramStatusState::Working => {
                ProgramPhase::Working { progress: report.progress().and_then(Progress::new) }
            },
            ProgramStatusState::Done => {
                ProgramPhase::Done { message: Message::from_raw_bytes(report.message()) }
            },
            ProgramStatusState::Blocked => {
                let kind = match report.kind() {
                    Ok(ProgramStatusKind::Permission) => Some(OscBlockedKind::Permission),
                    Ok(ProgramStatusKind::Question) => Some(OscBlockedKind::Question),
                    Ok(ProgramStatusKind::Auth) => Some(OscBlockedKind::Auth),
                    _ => None,
                };
                ProgramPhase::Blocked { kind, message: Message::from_raw_bytes(report.message()) }
            },
            ProgramStatusState::Error => {
                ProgramPhase::Error { message: Message::from_raw_bytes(report.message()) }
            },
            _ => return,
        };
        let app = std::str::from_utf8(report.app())
            .ok()
            .and_then(|raw| AppName::parse(raw).ok());
        let title = Title::from_raw_bytes(report.title());
        self.seq = self.seq.checked_add(1).expect("program statuses update fewer than 2^64 times");
        let record = Record { phase, app, title, at, seq: self.seq };
        match target {
            Target::Root => {
                self.root = Some(record);
            },
            Target::Child(id) => {
                if !self.children.contains_key(&id) && self.children.len() >= RECORDS_MAX {
                    let oldest = self
                        .children
                        .iter()
                        .min_by_key(|(_, record)| record.seq)
                        .map(|(key, _)| key.clone());
                    if let Some(oldest) = oldest {
                        let removed = self.children.remove(&oldest);
                        assert!(removed.is_some());
                    }
                }
                self.children.insert(id, record);
                assert!(self.children.len() <= RECORDS_MAX);
            },
        }
        self.bump_version();
    }

    pub fn prompt_started(&mut self) {
        let mut changed = false;
        let mut kept_children = BTreeMap::new();
        for (id, record) in &self.children {
            if record.phase.is_active() {
                changed = true;
            } else {
                let mut kept = record.clone();
                kept.app = self.resolved_app_for_child(id);
                kept_children.insert(id.clone(), kept);
            }
        }
        if self.root.as_ref().is_some_and(|record| record.phase.is_active()) {
            self.root = None;
            changed = true;
        }
        self.children = kept_children;
        if changed {
            self.bump_version();
        }
    }

    pub fn command_started(&mut self) {
        let had_root = self.root.take().is_some();
        let had_children = !self.children.is_empty();
        self.children.clear();
        if had_root || had_children {
            self.bump_version();
        }
    }

    pub fn exited(&mut self) -> EndedProgramStatuses {
        let mut changed = false;
        let mut ended_children = Vec::with_capacity(self.children.len());
        let mut kept_children = BTreeMap::new();
        for (id, record) in &self.children {
            if let Some(phase) = record.phase.clone().into_terminal() {
                let app = self.resolved_app_for_child(id);
                ended_children.push(EndedChildProgramStatus {
                    id: id.clone(),
                    status: EndedProgramStatus {
                        phase,
                        app: app.clone(),
                        title: record.title.clone(),
                        at: Some(record.at),
                    },
                });
                let mut kept = record.clone();
                kept.app = app;
                kept_children.insert(id.clone(), kept);
            } else {
                changed = true;
            }
        }
        let ended_root = match self.root.as_ref() {
            Some(record) => {
                if let Some(phase) = record.phase.clone().into_terminal() {
                    Some(EndedProgramStatus {
                        phase,
                        app: record.app.clone(),
                        title: record.title.clone(),
                        at: Some(record.at),
                    })
                } else {
                    self.root = None;
                    changed = true;
                    None
                }
            },
            None => None,
        };
        self.children = kept_children;
        if changed {
            self.bump_version();
        }
        EndedProgramStatuses { root: ended_root, children: ended_children }
    }

    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        assert!(self.children.len() <= RECORDS_MAX);
        let root = self.root.as_ref().map(|record| ProgramStatus {
            phase: record.phase.clone(),
            app: record.app.clone(),
            title: record.title.clone(),
            at: Some(record.at),
        });
        let mut children = Vec::with_capacity(self.children.len());
        for (id, record) in &self.children {
            children.push(ChildProgramStatus {
                id: id.clone(),
                status: ProgramStatus {
                    phase: record.phase.clone(),
                    app: self.resolved_app_for_child(id),
                    title: record.title.clone(),
                    at: Some(record.at),
                },
            });
        }
        Snapshot { root, children }
    }

    fn bump_version(&mut self) {
        self.version =
            self.version.checked_add(1).expect("program statuses change fewer than 2^64 times");
    }

    fn resolved_app_for_child(&self, id: &ChildId) -> Option<AppName> {
        let mut cursor = Some(id.as_str());
        let mut steps = 0usize;
        while let Some(key) = cursor {
            steps += 1;
            assert!(steps <= ID_DEPTH_MAX);
            if let Some(app) = self.children.get(key).and_then(|record| record.app.as_ref()) {
                return Some(app.clone());
            }
            cursor = key.rfind('/').map(|slash| &key[..slash]);
        }
        self.root.as_ref().and_then(|record| record.app.clone())
    }
}
