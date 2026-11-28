use std::fs::{self, File};
use std::io::{ErrorKind, Read as _, Write as _};
use std::path::{Path, PathBuf};

use lune_domain::flows::FlowRegistry;

use crate::state;

pub const FILE: &str = "flows.json";

pub const RUN_DIR: &str = "flows-run";

const FILE_BYTES_MAX: usize = 4 * 1024 * 1024;

const _: () = assert!(FILE_BYTES_MAX > 0);

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
        serde_json::from_slice(&bytes)
            .map_err(|error| format!("{}: does not read as flows: {error}", path.display()))
    }

    pub fn save(&self, registry: &FlowRegistry) -> Result<(), String> {
        let Some(path) = &self.file else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        let encoded = serde_json::to_vec_pretty(registry)
            .map_err(|error| format!("flows did not encode: {error}"))?;
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let write_result = (|| -> std::io::Result<()> {
            let mut file = File::create(&tmp)?;
            file.write_all(&encoded)?;
            file.sync_all()?;
            fs::rename(&tmp, path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        write_result.map_err(|error| format!("{}: {error}", path.display()))
    }

    pub fn run_dir(&self) -> Result<PathBuf, String> {
        let dir = match &self.run_dir {
            Some(dir) => dir.clone(),
            None => std::env::temp_dir().join("lune-flows-run"),
        };
        fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        Ok(dir)
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

#[must_use]
pub fn read_session_out(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
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
}
