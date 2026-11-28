use std::fs::{self, File};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};

use lune_domain::flows::{FLOWS_MAX, FlowRegistry, SESSION_ID_BYTES_MAX};

use crate::log;
use crate::state;

pub const FILE: &str = "flows.json";

pub const RUN_DIR: &str = "flows-run";

pub const UNREADABLE_SUFFIX: &str = "unreadable";

const FILE_BYTES_MAX: usize = 4 * 1024 * 1024;

const RUN_FILES_MAX: usize = 4096;

const _: () = assert!(FILE_BYTES_MAX > 0);
const _: () = assert!(RUN_FILES_MAX >= FLOWS_MAX * 5);
const _: () = assert!(SESSION_ID_BYTES_MAX > 0);

pub struct FlowStore {
    file: Option<PathBuf>,
    run_dir: Option<PathBuf>,
}

impl FlowStore {
    #[must_use]
    pub fn open() -> (FlowStore, Option<String>) {
        match state::folder() {
            Ok(folder) => (
                FlowStore {
                    file: Some(folder.join(FILE)),
                    run_dir: Some(folder.join(RUN_DIR)),
                },
                None,
            ),
            Err(unset) => (
                FlowStore {
                    file: None,
                    run_dir: None,
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
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(format!("{}: {error}", dir.display())),
        };
        let mut stale = Vec::new();
        for entry in entries.take(RUN_FILES_MAX) {
            let entry = entry.map_err(|error| format!("{}: {error}", dir.display()))?;
            stale.push(entry.path());
        }
        assert!(stale.len() <= RUN_FILES_MAX);
        remove_run_files(&stale);
        Ok(stale.len())
    }
}

pub fn forget_in(folder: &Path) -> Result<(), String> {
    let file = folder.join(FILE);
    match fs::remove_file(&file) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", file.display())),
    }
    let run_dir = folder.join(RUN_DIR);
    match fs::remove_dir_all(&run_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", run_dir.display())),
    }
    Ok(())
}

pub fn forget() -> Result<(), String> {
    let Ok(folder) = state::folder() else {
        return Ok(());
    };
    forget_in(&folder)
}

pub fn read_session_out(path: &Path) -> Result<Option<String>, String> {
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
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
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
    use lune_domain::agent::Harness;
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
                Harness::ClaudeCode,
                FlowEntry::PullRequest,
            )
            .unwrap();
        store.save(&registry).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded, registry);
    }

    #[test]
    fn read_session_out_handles_missing_valid_and_oversized_files() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.id");
        assert_eq!(read_session_out(&missing), Ok(None));

        let valid = dir.path().join("valid.id");
        fs::write(&valid, "sess-42\n").unwrap();
        assert_eq!(read_session_out(&valid), Ok(Some("sess-42".to_owned())));

        let oversized = dir.path().join("oversized.id");
        fs::write(&oversized, "a".repeat(SESSION_ID_BYTES_MAX + 1)).unwrap();
        assert!(read_session_out(&oversized).is_err());
    }
}
