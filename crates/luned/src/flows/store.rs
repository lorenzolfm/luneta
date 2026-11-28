use std::fs::{self, File};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};

use lune_domain::flows::{FLOWS_MAX, FlowRegistry, SESSION_ID_BYTES_MAX, SessionId};
use lune_domain::projects::WorktreeId;

use crate::log;
use crate::state;

pub const FILE: &str = "flows.json";

pub const RUN_DIR: &str = "flows-run";

pub const PI_DIR: &str = "flows-pi";

const PI_PREFIX: &str = "pi-";

const PI_SUFFIX: &str = ".jsonl";

pub const UNREADABLE_SUFFIX: &str = "unreadable";

const FILE_BYTES_MAX: usize = 4 * 1024 * 1024;

const RUN_FILES_MAX: usize = 4096;

const _: () = assert!(FILE_BYTES_MAX > 0);
const _: () = assert!(RUN_FILES_MAX >= FLOWS_MAX * 5);
const _: () = assert!(SESSION_ID_BYTES_MAX > 0);

pub struct FlowStore {
    file: Option<PathBuf>,
    run_dir: Option<PathBuf>,
    pi_dir: Option<PathBuf>,
}

impl FlowStore {
    #[must_use]
    pub fn open() -> (FlowStore, Option<String>) {
        match state::folder() {
            Ok(folder) => (
                FlowStore {
                    file: Some(folder.join(FILE)),
                    run_dir: Some(folder.join(RUN_DIR)),
                    pi_dir: Some(folder.join(PI_DIR)),
                },
                None,
            ),
            Err(unset) => (
                FlowStore {
                    file: None,
                    run_dir: None,
                    pi_dir: None,
                },
                Some(format!("flows are kept nowhere: {unset}")),
            ),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub fn at(folder: &Path) -> FlowStore {
        FlowStore {
            file: Some(folder.join(FILE)),
            run_dir: Some(folder.join(RUN_DIR)),
            pi_dir: Some(folder.join(PI_DIR)),
        }
    }

    pub fn load(&self) -> Result<FlowRegistry, String> {
        let Some(path) = &self.file else {
            return Ok(FlowRegistry::default());
        };
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(FlowRegistry::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        let limit = u64::try_from(FILE_BYTES_MAX).expect("usize fits u64");
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if bytes.len() > FILE_BYTES_MAX {
            return Err(format!(
                "{}: exceeds {FILE_BYTES_MAX} bytes",
                path.display()
            ));
        }
        let registry: FlowRegistry = serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: does not read as flows: {error}", path.display()))?;
        registry
            .validate()
            .map_err(|why| format!("{}: {why}", path.display()))?;
        assert!(registry.len() <= FLOWS_MAX);
        Ok(registry)
    }

    pub fn save(&self, registry: &FlowRegistry) -> Result<(), String> {
        assert_eq!(
            registry.validate(),
            Ok(()),
            "flow registry invariants hold before save"
        );
        let Some(path) = &self.file else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let encoded = serde_json::to_vec_pretty(registry)
            .map_err(|error| format!("flows did not encode: {error}"))?;
        if encoded.len() > FILE_BYTES_MAX {
            return Err(format!(
                "{}: encoded flows ({} bytes) exceed {FILE_BYTES_MAX} bytes",
                path.display(),
                encoded.len()
            ));
        }
        assert!(encoded.len() <= FILE_BYTES_MAX);
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let write_result = (|| -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            if let Some(parent) = path.parent() {
                File::open(parent)?.sync_all()?;
            }
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        write_result.map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn set_aside(&self) -> Result<Option<PathBuf>, String> {
        let Some(path) = &self.file else {
            return Ok(None);
        };
        let aside = path.with_extension(format!("json.{UNREADABLE_SUFFIX}"));
        match fs::rename(path, &aside) {
            Ok(()) => Ok(Some(aside)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "{} could not be set aside as {}: {error}",
                path.display(),
                aside.display()
            )),
        }
    }

    pub fn run_dir(&self) -> Result<PathBuf, String> {
        let Some(dir) = &self.run_dir else {
            return Err("flow steps have no state folder to keep their files in".to_owned());
        };
        fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        Ok(dir.clone())
    }

    pub fn clear_run_dir(&self) -> Result<usize, String> {
        let Some(dir) = &self.run_dir else {
            return Ok(0);
        };
        let stale = listed(dir)?;
        remove_run_files(&stale);
        Ok(stale.len())
    }

    pub fn pi_session_file(&self, stamp: &str) -> Result<PathBuf, String> {
        assert!(!stamp.is_empty(), "a step stamp is non-empty");
        let Some(dir) = &self.pi_dir else {
            return Err("pi sessions have no state folder to be kept in".to_owned());
        };
        fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        Ok(dir.join(format!("{PI_PREFIX}{stamp}{PI_SUFFIX}")))
    }

    pub fn clear_pi_sessions_except(&self, kept: &[PathBuf]) -> Result<usize, String> {
        let Some(dir) = &self.pi_dir else {
            return Ok(0);
        };
        let mut stale = listed(dir)?;
        stale.retain(|path| !kept.contains(path));
        remove_run_files(&stale);
        Ok(stale.len())
    }

    pub fn remove_pi_sessions_of(&self, worktree: WorktreeId) -> Result<usize, String> {
        let Some(dir) = &self.pi_dir else {
            return Ok(0);
        };
        let mut owned = listed(dir)?;
        owned.retain(|path| pi_session_worktree(path) == Some(worktree.get()));
        remove_run_files(&owned);
        Ok(owned.len())
    }
}

#[must_use]
pub fn step_stamp(pid: u32, worktree: WorktreeId, opened: u64) -> String {
    format!("{pid}-{}-{opened}", worktree.get())
}

fn pi_session_worktree(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?;
    let stamp = name.strip_prefix(PI_PREFIX)?.strip_suffix(PI_SUFFIX)?;
    let mut parts = stamp.split('-');
    let (_pid, worktree, _opened) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    worktree.parse().ok()
}

fn listed(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    let mut paths = Vec::new();
    for entry in entries.take(RUN_FILES_MAX) {
        let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
        paths.push(entry.path());
    }
    assert!(paths.len() <= RUN_FILES_MAX);
    Ok(paths)
}

pub fn forget_in(folder: &Path) -> Result<(), String> {
    let file = folder.join(FILE);
    match fs::remove_file(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", file.display())),
    }
    for dir in [RUN_DIR, PI_DIR] {
        let dir = folder.join(dir);
        match fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        }
    }
    Ok(())
}

pub fn forget() -> Result<(), String> {
    let Ok(folder) = state::folder() else {
        return Ok(());
    };
    forget_in(&folder)
}

pub fn read_session_out(path: &Path) -> Result<Option<SessionId>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let limit = u64::try_from(SESSION_ID_BYTES_MAX).expect("usize fits u64");
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() > SESSION_ID_BYTES_MAX {
        return Err(format!(
            "{}: exceeds {SESSION_ID_BYTES_MAX} bytes",
            path.display()
        ));
    }
    let text = String::from_utf8(bytes).map_err(|error| format!("{}: {error}", path.display()))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    SessionId::parse(trimmed)
        .map(Some)
        .map_err(|why| format!("{}: {why}", path.display()))
}

pub fn remove_run_files(paths: &[PathBuf]) {
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                log::say(&format!("flows cleanup {}: {error}", path.display()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use lune_domain::agent::FlowHarness;
    use lune_domain::flows::{FlowEntry, FlowMode, FlowSpec};
    use lune_domain::projects::WorktreeId;

    use super::*;

    #[test]
    fn round_trips_flow_registry_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let store = FlowStore::at(dir.path());
        assert!(store.load().unwrap().is_empty());

        let mut registry = FlowRegistry::default();
        registry
            .start(
                WorktreeId::of(42).unwrap(),
                FlowSpec::default_spec(),
                FlowMode::Unattended,
                FlowHarness::ClaudeCode,
                FlowEntry::PullRequest,
            )
            .unwrap();
        store.save(&registry).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, registry);
    }

    #[test]
    fn read_session_out_handles_missing_valid_invalid_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.id");
        assert_eq!(read_session_out(&missing), Ok(None));

        let valid = dir.path().join("valid.id");
        fs::write(&valid, "sess-42\n").unwrap();
        assert_eq!(
            read_session_out(&valid),
            Ok(Some(SessionId::parse("sess-42").unwrap()))
        );

        let flag_like = dir.path().join("flag.id");
        fs::write(&flag_like, "-bad\n").unwrap();
        assert!(read_session_out(&flag_like).is_err());

        let oversized = dir.path().join("oversized.id");
        fs::write(&oversized, "a".repeat(SESSION_ID_BYTES_MAX + 1)).unwrap();
        assert!(read_session_out(&oversized).is_err());
    }

    #[test]
    fn pi_sessions_outlive_the_run_folder_until_their_flow_ends() {
        let dir = tempfile::tempdir().unwrap();
        let store = FlowStore::at(dir.path());
        let (one, two) = (WorktreeId::of(1).unwrap(), WorktreeId::of(2).unwrap());
        let paused = store.pi_session_file(&step_stamp(7, one, 1)).unwrap();
        let unkept = store.pi_session_file(&step_stamp(7, one, 2)).unwrap();
        let other = store.pi_session_file(&step_stamp(7, two, 3)).unwrap();
        for path in [&paused, &unkept, &other] {
            fs::write(path, b"{}\n").unwrap();
        }
        fs::create_dir_all(store.run_dir().unwrap()).unwrap();
        store.clear_run_dir().unwrap();
        assert!(paused.exists(), "a daemon start keeps the pi sessions");

        assert_eq!(
            store
                .clear_pi_sessions_except(&[paused.clone(), other.clone()])
                .unwrap(),
            1
        );
        assert!(paused.exists() && other.exists() && !unkept.exists());

        assert_eq!(store.remove_pi_sessions_of(one).unwrap(), 1);
        assert!(!paused.exists(), "an ended flow's pi sessions go");
        assert!(other.exists(), "another worktree's pi sessions stay");
    }
}
