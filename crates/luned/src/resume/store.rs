use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use lune_domain::projects::{Id, VIEWS_MAX, ViewId};
use lune_protocol::program_resume::{
    ProcStart, ProcessArgv, ProcessExe, ProcessName, ProcessPid, ProcessPlacement, ReportedAt,
    ResumeArgv, ResumeCwd, ResumeCwdSource, ResumeRecord,
};
use rustix::fs::{FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};

use crate::{log, state};

const DIR_NAME: &str = "resume";

const FILE_NAME: &str = "records.json";

const STORE_VERSION: u32 = 1;

const STORE_BYTES_MAX: usize = 4 * 1024 * 1024;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

const _: () = assert!(STORE_BYTES_MAX > 0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordSource {
    Osc7577,
    Synthesized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeAttempt {
    Pending,
    Tried,
}

impl ResumeAttempt {
    #[must_use]
    pub const fn is_tried(self) -> bool {
        matches!(self, ResumeAttempt::Tried)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredResume {
    pub source: RecordSource,
    pub record: ResumeRecord,
    pub attempt: ResumeAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MarkTriedError {
    Unkept(String),
    Missing,
    AlreadyTried,
    Io(String),
}

impl fmt::Display for MarkTriedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkTriedError::Missing => f.write_str("no saved resume record for terminal"),
            MarkTriedError::AlreadyTried => f.write_str("saved resume record was already tried"),
            MarkTriedError::Unkept(why) | MarkTriedError::Io(why) => f.write_str(why),
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiskPlacement {
    Root,
    ShellHosted,
    Wrapped,
}

impl From<ProcessPlacement> for DiskPlacement {
    fn from(placement: ProcessPlacement) -> Self {
        match placement {
            ProcessPlacement::Root => DiskPlacement::Root,
            ProcessPlacement::ShellHosted => DiskPlacement::ShellHosted,
            ProcessPlacement::Wrapped => DiskPlacement::Wrapped,
        }
    }
}

impl From<DiskPlacement> for ProcessPlacement {
    fn from(placement: DiskPlacement) -> Self {
        match placement {
            DiskPlacement::Root => ProcessPlacement::Root,
            DiskPlacement::ShellHosted => ProcessPlacement::ShellHosted,
            DiskPlacement::Wrapped => ProcessPlacement::Wrapped,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
enum DiskCwd {
    Explicit(String),
    Inferred(String),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskFile {
    version: u32,
    records: BTreeMap<String, DiskEntry>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskEntry {
    source: RecordSource,
    pid: u32,
    proc_start: String,
    uid: u32,
    process_name: String,
    process_exe: Option<String>,
    process_argv: Option<Vec<String>>,
    placement: DiskPlacement,
    argv: Vec<String>,
    cwd: Option<DiskCwd>,
    reported_at_secs: u64,
    reported_at_nanos: u32,
    attempt: ResumeAttempt,
}

enum StoreState {
    #[cfg(test)]
    Memory(BTreeMap<ViewId, StoredResume>),
    Kept {
        file: PathBuf,
        records: BTreeMap<ViewId, StoredResume>,
    },
    Unkept(String),
}

pub struct ResumeStore {
    state: StoreState,
}

impl ResumeStore {
    #[must_use]
    pub fn open(projects_kept: bool) -> (Self, Option<String>) {
        if !projects_kept {
            let why = "resume store disabled: projects are unkept".to_owned();
            return (
                Self {
                    state: StoreState::Unkept(why.clone()),
                },
                Some(why),
            );
        }
        match state::folder() {
            Ok(dir) => Self::open_at(dir.join(DIR_NAME).join(FILE_NAME)),
            Err(unset) => {
                let why = format!("resume store disabled: {unset}");
                (
                    Self {
                        state: StoreState::Unkept(why.clone()),
                    },
                    Some(why),
                )
            }
        }
    }

    #[must_use]
    pub fn open_at(path: PathBuf) -> (Self, Option<String>) {
        let uid = rustix::process::getuid().as_raw();
        match load_file(&path, uid) {
            Ok((records, warning)) => (
                Self {
                    state: StoreState::Kept {
                        file: path,
                        records,
                    },
                },
                warning,
            ),
            Err(why) => (
                Self {
                    state: StoreState::Unkept(why.clone()),
                },
                Some(why),
            ),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            state: StoreState::Memory(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn unkept(&self) -> Option<&str> {
        match &self.state {
            StoreState::Unkept(why) => Some(why),
            StoreState::Kept { .. } => None,
            #[cfg(test)]
            StoreState::Memory(_) => None,
        }
    }

    #[must_use]
    pub fn get(&self, terminal: ViewId) -> Option<&StoredResume> {
        self.records().and_then(|records| records.get(&terminal))
    }

    #[cfg(test)]
    #[must_use]
    pub fn all(&self) -> Option<&BTreeMap<ViewId, StoredResume>> {
        self.records()
    }

    fn records(&self) -> Option<&BTreeMap<ViewId, StoredResume>> {
        match &self.state {
            StoreState::Kept { records, .. } => Some(records),
            #[cfg(test)]
            StoreState::Memory(records) => Some(records),
            StoreState::Unkept(_) => None,
        }
    }

    pub fn upsert(
        &mut self,
        terminal: ViewId,
        source: RecordSource,
        record: ResumeRecord,
    ) -> Result<(), String> {
        let our_uid = rustix::process::getuid().as_raw();
        if record.uid() != our_uid {
            return Err(format!(
                "resume record uid {} does not match current user {our_uid}",
                record.uid()
            ));
        }
        match &mut self.state {
            StoreState::Unkept(why) => Err(why.clone()),
            #[cfg(test)]
            StoreState::Memory(records) => {
                if is_same_pending(records.get(&terminal), source, &record) {
                    return Ok(());
                }
                insert_bounded(records, terminal, source, record);
                Ok(())
            }
            StoreState::Kept { file, records } => {
                if is_same_pending(records.get(&terminal), source, &record) {
                    return Ok(());
                }
                let mut next = records.clone();
                insert_bounded(&mut next, terminal, source, record);
                write_file(file, &next).map_err(|error| format!("{}: {error}", file.display()))?;
                *records = next;
                Ok(())
            }
        }
    }

    pub fn mark_tried(&mut self, terminal: ViewId) -> Result<(), MarkTriedError> {
        match &mut self.state {
            StoreState::Unkept(why) => Err(MarkTriedError::Unkept(why.clone())),
            #[cfg(test)]
            StoreState::Memory(records) => {
                let entry = records.get_mut(&terminal).ok_or(MarkTriedError::Missing)?;
                if entry.attempt.is_tried() {
                    return Err(MarkTriedError::AlreadyTried);
                }
                entry.attempt = ResumeAttempt::Tried;
                Ok(())
            }
            StoreState::Kept { file, records } => {
                let entry = records.get(&terminal).ok_or(MarkTriedError::Missing)?;
                if entry.attempt.is_tried() {
                    return Err(MarkTriedError::AlreadyTried);
                }
                let mut next = records.clone();
                let next_entry = next.get_mut(&terminal).expect("checked entry exists");
                next_entry.attempt = ResumeAttempt::Tried;
                write_file(file, &next)
                    .map_err(|error| MarkTriedError::Io(format!("{}: {error}", file.display())))?;
                *records = next;
                Ok(())
            }
        }
    }

    pub fn remove(&mut self, terminal: ViewId) -> Result<(), String> {
        match &mut self.state {
            StoreState::Unkept(why) => Err(why.clone()),
            #[cfg(test)]
            StoreState::Memory(records) => {
                records.remove(&terminal);
                Ok(())
            }
            StoreState::Kept { file, records } => {
                if !records.contains_key(&terminal) {
                    return Ok(());
                }
                let mut next = records.clone();
                let removed = next.remove(&terminal);
                assert!(removed.is_some());
                write_file(file, &next).map_err(|error| format!("{}: {error}", file.display()))?;
                *records = next;
                Ok(())
            }
        }
    }

    pub fn remove_many(&mut self, terminals: &[ViewId]) -> Result<(), String> {
        match &mut self.state {
            StoreState::Unkept(why) => Err(why.clone()),
            #[cfg(test)]
            StoreState::Memory(records) => {
                for terminal in terminals {
                    records.remove(terminal);
                }
                Ok(())
            }
            StoreState::Kept { file, records } => {
                if !terminals
                    .iter()
                    .any(|terminal| records.contains_key(terminal))
                {
                    return Ok(());
                }
                let mut next = records.clone();
                for terminal in terminals {
                    next.remove(terminal);
                }
                write_file(file, &next).map_err(|error| format!("{}: {error}", file.display()))?;
                *records = next;
                Ok(())
            }
        }
    }
}

fn is_same_pending(
    existing: Option<&StoredResume>,
    source: RecordSource,
    record: &ResumeRecord,
) -> bool {
    existing.is_some_and(|existing| {
        existing.attempt == ResumeAttempt::Pending
            && existing.source == source
            && existing.record.pid() == record.pid()
            && existing.record.proc_start() == record.proc_start()
            && existing.record.argv() == record.argv()
            && existing.record.cwd() == record.cwd()
            && existing.record.placement() == record.placement()
            && existing.record.process_argv() == record.process_argv()
    })
}

fn insert_bounded(
    records: &mut BTreeMap<ViewId, StoredResume>,
    terminal: ViewId,
    source: RecordSource,
    record: ResumeRecord,
) {
    let is_new = !records.contains_key(&terminal);
    if is_new && records.len() >= VIEWS_MAX {
        let oldest = records
            .iter()
            .min_by_key(|(id, entry)| (entry.record.reported_at(), *id))
            .map(|(&id, _)| id);
        if let Some(oldest) = oldest {
            let evicted = records.remove(&oldest);
            assert!(evicted.is_some());
        }
    }
    if is_new {
        assert!(records.len() < VIEWS_MAX);
    }
    records.insert(
        terminal,
        StoredResume {
            source,
            record,
            attempt: ResumeAttempt::Pending,
        },
    );
    assert!(records.len() <= VIEWS_MAX);
}

pub fn forget_in(state_dir: &Path) -> Result<(), String> {
    let file = state_dir.join(DIR_NAME).join(FILE_NAME);
    match fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

fn load_file(
    path: &Path,
    our_uid: u32,
) -> Result<(BTreeMap<ViewId, StoredResume>, Option<String>), String> {
    let Some(dir) = path.parent() else {
        return Err("resume store path has no parent directory".to_owned());
    };
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() || meta.uid() != our_uid || meta.mode() & 0o077 != 0 {
                return Err(format!(
                    "{} is not a private 0700 directory owned by this user",
                    dir.display()
                ));
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), None));
        }
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok((BTreeMap::new(), None)),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let stat = rustix::fs::fstat(&fd).map_err(|error| format!("{}: {error}", path.display()))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if stat.st_uid != our_uid {
        return Err(format!("{} is owned by another user", path.display()));
    }
    if stat.st_mode & 0o077 != 0 {
        return Err(format!("{} is accessible to other users", path.display()));
    }
    let limit = u64::try_from(STORE_BYTES_MAX).expect("store limit fits u64");
    let mut raw = Vec::new();
    File::from(fd)
        .take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if raw.len() > STORE_BYTES_MAX {
        return Err(format!(
            "{} exceeds {STORE_BYTES_MAX} bytes",
            path.display()
        ));
    }
    let disk: DiskFile =
        serde_json::from_slice(&raw).map_err(|error| format!("{}: {error}", path.display()))?;
    if disk.version != STORE_VERSION {
        return Err(format!(
            "{}: unsupported version {}",
            path.display(),
            disk.version
        ));
    }
    let mut records = BTreeMap::new();
    let mut discarded: usize = 0;
    for (id_str, entry) in disk.records {
        if records.len() >= VIEWS_MAX {
            discarded += 1;
            continue;
        }
        let Ok(id) = id_str.parse::<Id>() else {
            discarded += 1;
            continue;
        };
        if entry.uid != our_uid {
            discarded += 1;
            continue;
        }
        let Some(stored) = parse_disk_entry(entry) else {
            discarded += 1;
            continue;
        };
        records.insert(ViewId::new(id), stored);
    }
    assert!(records.len() <= VIEWS_MAX);
    let warning = (discarded > 0)
        .then(|| format!("discarded {discarded} invalid or cross-user resume record(s)"));
    Ok((records, warning))
}

fn parse_disk_entry(entry: DiskEntry) -> Option<StoredResume> {
    let pid = ProcessPid::parse(entry.pid).ok()?;
    let proc_start = ProcStart::parse(entry.proc_start).ok()?;
    let process_name = ProcessName::parse(entry.process_name).ok()?;
    let process_exe = entry.process_exe.map(ProcessExe::parse).transpose().ok()?;
    let process_argv = entry
        .process_argv
        .map(ProcessArgv::parse)
        .transpose()
        .ok()?;
    let argv = ResumeArgv::parse(entry.argv).ok()?;
    let cwd = match entry.cwd {
        Some(DiskCwd::Explicit(dir)) => {
            Some(ResumeCwdSource::Explicit(ResumeCwd::parse(&dir).ok()?))
        }
        Some(DiskCwd::Inferred(dir)) => {
            Some(ResumeCwdSource::Inferred(ResumeCwd::parse(&dir).ok()?))
        }
        None => None,
    };
    let reported_at =
        ReportedAt::from_parts(entry.reported_at_secs, entry.reported_at_nanos).ok()?;
    let record = ResumeRecord::new(
        pid,
        proc_start,
        entry.uid,
        process_name,
        process_exe,
        process_argv,
        entry.placement.into(),
        argv,
        cwd,
        reported_at,
    );
    Some(StoredResume {
        source: entry.source,
        record,
        attempt: entry.attempt,
    })
}

fn write_file(path: &Path, records: &BTreeMap<ViewId, StoredResume>) -> io::Result<()> {
    assert!(records.len() <= VIEWS_MAX);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::other("resume store has no parent"))?;
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    match DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let our_uid = rustix::process::getuid().as_raw();
    let dir_meta = fs::symlink_metadata(dir)?;
    if !dir_meta.is_dir() || dir_meta.uid() != our_uid || dir_meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            format!(
                "{} is not a private 0700 directory owned by this user",
                dir.display()
            ),
        ));
    }

    let mut disk_records = BTreeMap::new();
    for (&terminal, stored) in records {
        assert_eq!(stored.record.uid(), our_uid);
        let cwd = stored.record.cwd().map(|source| match source {
            ResumeCwdSource::Explicit(dir) => DiskCwd::Explicit(dir.as_str().to_owned()),
            ResumeCwdSource::Inferred(dir) => DiskCwd::Inferred(dir.as_str().to_owned()),
        });
        let reported_at = stored.record.reported_at();
        disk_records.insert(
            terminal.to_string(),
            DiskEntry {
                source: stored.source,
                pid: stored.record.pid().get(),
                proc_start: stored.record.proc_start().as_str().to_owned(),
                uid: stored.record.uid(),
                process_name: stored.record.process_name().as_str().to_owned(),
                process_exe: stored
                    .record
                    .process_exe()
                    .map(|exe| exe.as_str().to_owned()),
                process_argv: stored
                    .record
                    .process_argv()
                    .map(|argv| argv.words().to_vec()),
                placement: stored.record.placement().into(),
                argv: stored.record.argv().words().to_vec(),
                cwd,
                reported_at_secs: reported_at.secs(),
                reported_at_nanos: reported_at.nanos(),
                attempt: stored.attempt,
            },
        );
    }
    let payload = serde_json::to_vec(&DiskFile {
        version: STORE_VERSION,
        records: disk_records,
    })
    .map_err(io::Error::other)?;
    if payload.len() > STORE_BYTES_MAX {
        return Err(io::Error::other(format!(
            "resume store payload is {} bytes, over {STORE_BYTES_MAX}",
            payload.len()
        )));
    }
    assert!(payload.len() <= STORE_BYTES_MAX);
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!(".records.{}.{seq}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    let wrote = file.write_all(&payload).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = wrote {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = File::open(dir).and_then(|d| d.sync_all()) {
        log::say(&format!(
            "{}: {error}, and so a crash may take back the change kept",
            dir.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    fn terminal_id(raw: u64) -> ViewId {
        ViewId::of(raw).unwrap()
    }

    fn record_at(uid: u32, program: &str, secs: u64) -> ResumeRecord {
        ResumeRecord::new(
            ProcessPid::new(55).unwrap(),
            ProcStart::parse("99".to_owned()).unwrap(),
            uid,
            ProcessName::from_os(program),
            Some(ProcessExe::parse(format!("/usr/bin/{program}")).unwrap()),
            Some(ProcessArgv::bounded(vec![program.to_owned()])),
            ProcessPlacement::ShellHosted,
            ResumeArgv::parse(vec![program.to_owned()]).unwrap(),
            Some(ResumeCwdSource::Explicit(ResumeCwd::parse("/tmp").unwrap())),
            ReportedAt::from_parts(secs, 0).unwrap(),
        )
    }

    fn record(uid: u32, program: &str) -> ResumeRecord {
        record_at(uid, program, 1_800_000_000)
    }

    #[test]
    fn store_persists_with_0700_dir_and_0600_file_and_tracks_tried() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("resume").join("records.json");
        let uid = rustix::process::getuid().as_raw();
        let id = terminal_id(7);

        let (mut store, warning) = ResumeStore::open_at(file.clone());
        assert_eq!(warning, None);
        assert_eq!(store.unkept(), None);
        store
            .upsert(id, RecordSource::Osc7577, record(uid, "nvim"))
            .unwrap();

        let dir_mode = fs::metadata(file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let file_mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700);
        assert_eq!(file_mode, 0o600);

        assert_eq!(
            store.mark_tried(terminal_id(99)),
            Err(MarkTriedError::Missing)
        );
        store.mark_tried(id).unwrap();
        assert_eq!(store.mark_tried(id), Err(MarkTriedError::AlreadyTried));

        let (reloaded, warning) = ResumeStore::open_at(file);
        assert_eq!(warning, None);
        let entry = reloaded.get(id).unwrap();
        assert_eq!(entry.attempt, ResumeAttempt::Tried);
        assert_eq!(entry.record.argv().program(), "nvim");
    }

    #[test]
    fn store_rejects_foreign_uid_and_preserves_unreadable_file_without_overwriting() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("resume").join("records.json");
        let uid = rustix::process::getuid().as_raw();
        let id = terminal_id(9);

        let (mut store, _) = ResumeStore::open_at(file.clone());
        assert!(
            store
                .upsert(id, RecordSource::Osc7577, record(uid + 1, "nvim"))
                .is_err()
        );
        assert!(store.all().unwrap().is_empty());

        store
            .upsert(id, RecordSource::Osc7577, record(uid, "nvim"))
            .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        let (mut rejected, warning) = ResumeStore::open_at(file.clone());
        assert!(rejected.unkept().is_some());
        assert!(warning.unwrap().contains("other users"));
        assert!(
            rejected
                .upsert(id, RecordSource::Osc7577, record(uid, "vim"))
                .is_err()
        );

        let newer = r#"{"version": 99, "records": {}}"#;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&file, newer).unwrap();
        let (mut future, warning) = ResumeStore::open_at(file.clone());
        assert!(future.unkept().is_some());
        assert!(warning.unwrap().contains("unsupported version 99"));
        assert!(
            future
                .upsert(id, RecordSource::Osc7577, record(uid, "vim"))
                .is_err()
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), newer);

        let unknown_field = r#"{"version": 1, "records": {}, "extra": true}"#;
        fs::write(&file, unknown_field).unwrap();
        let (unknown, warning) = ResumeStore::open_at(file.clone());
        assert!(unknown.unkept().is_some());
        assert!(warning.unwrap().contains("unknown field"));
        assert_eq!(fs::read_to_string(&file).unwrap(), unknown_field);
    }

    #[test]
    fn failed_persist_leaves_in_memory_state_unchanged() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("resume").join("records.json");
        let uid = rustix::process::getuid().as_raw();
        let id1 = terminal_id(1);
        let id2 = terminal_id(2);

        let (mut store, _) = ResumeStore::open_at(file.clone());
        store
            .upsert(id1, RecordSource::Osc7577, record(uid, "nvim"))
            .unwrap();
        fs::set_permissions(file.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();

        assert!(
            store
                .upsert(id2, RecordSource::Osc7577, record(uid, "vim"))
                .is_err()
        );
        assert!(store.get(id2).is_none());
        assert!(matches!(store.mark_tried(id1), Err(MarkTriedError::Io(_))));
        assert_eq!(store.get(id1).unwrap().attempt, ResumeAttempt::Pending);
        assert!(store.remove(id1).is_err());
        assert!(store.get(id1).is_some());

        fs::set_permissions(file.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn store_evicts_oldest_at_views_max_and_discards_corrupt_disk_entries() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("resume").join("records.json");
        let uid = rustix::process::getuid().as_raw();

        let mut store = ResumeStore::in_memory();
        for raw_id in 1..=u64::try_from(VIEWS_MAX + 2).unwrap() {
            let rec = record_at(uid, "nvim", 1_800_000_000 + raw_id);
            store
                .upsert(terminal_id(raw_id), RecordSource::Osc7577, rec)
                .unwrap();
        }
        assert_eq!(store.all().unwrap().len(), VIEWS_MAX);
        assert!(store.get(terminal_id(1)).is_none());
        assert!(store.get(terminal_id(2)).is_none());
        assert!(
            store
                .get(terminal_id(u64::try_from(VIEWS_MAX + 2).unwrap()))
                .is_some()
        );

        let dir = file.parent().unwrap();
        DirBuilder::new().mode(0o700).create(dir).unwrap();
        let corrupt = serde_json::json!({
            "version": 1,
            "records": {
                "1": {
                    "source": "osc7577",
                    "pid": 0,
                    "proc_start": "99",
                    "uid": uid,
                    "process_name": "nvim",
                    "process_exe": null,
                    "process_argv": null,
                    "placement": "shell_hosted",
                    "argv": ["nvim"],
                    "cwd": { "kind": "explicit", "path": "/tmp" },
                    "reported_at_secs": 1800000000u64,
                    "reported_at_nanos": 0u32,
                    "attempt": "pending"
                },
                "2": {
                    "source": "osc7577",
                    "pid": 42,
                    "proc_start": "99",
                    "uid": uid,
                    "process_name": "nvim",
                    "process_exe": null,
                    "process_argv": null,
                    "placement": "shell_hosted",
                    "argv": ["nvim"],
                    "cwd": { "kind": "explicit", "path": "/tmp" },
                    "reported_at_secs": u64::MAX,
                    "reported_at_nanos": 0u32,
                    "attempt": "pending"
                }
            }
        });
        let mut handle = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&file)
            .unwrap();
        handle.write_all(corrupt.to_string().as_bytes()).unwrap();
        drop(handle);

        let (loaded, warning) = ResumeStore::open_at(file);
        assert!(loaded.all().unwrap().is_empty());
        assert!(warning.unwrap().contains("discarded 2"));
    }
}
