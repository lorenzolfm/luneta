use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::folder;

const KEPT: Duration = Duration::from_hours(24);

const PATTERN: [char; 7] = ['*', '?', '[', ']', '\\', '(', ')'];

static FILES: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Draft {
    file: File,
    partial: String,
    kept: String,
}

impl Draft {
    pub(crate) fn new(
        runtime: &Path,
        kind: &str,
        extension: &str,
        now: SystemTime,
    ) -> Result<Draft, String> {
        let folder = folder::make(runtime).and_then(|_| folder::make(&runtime.join(kind)));
        let folder = folder.map_err(|refusal| refusal.to_string())?;
        let found = fs::canonicalize(folder.path());
        let found = found.map_err(|error| format!("{}: {error}", folder.path().display()))?;
        let found = found.into_os_string().into_string();
        let found =
            found.map_err(|found| format!("{} is not text", Path::new(&found).display()))?;
        if let Some(pattern) = found.chars().find(|c| PATTERN.contains(c)) {
            return Err(format!(
                "{found} holds `{pattern}`, which a rule of Claude Code's reads as more than a path"
            ));
        }
        sweep(folder.path(), now);
        let since = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
        let count = FILES.fetch_add(1, Ordering::Relaxed);
        let kept = format!("{found}/{since}-{}-{count}.{extension}", std::process::id());
        let partial = format!("{kept}.part");
        let file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&partial);
        let file = file.map_err(|error| format!("{partial}: {error}"))?;
        Ok(Draft { file, partial, kept })
    }

    pub(crate) fn make_room(&self, size: u64, most: u64) {
        let partial = Path::new(&self.partial);
        let Some(Ok(entries)) = partial.parent().map(fs::read_dir) else {
            return;
        };
        let others = entries.flatten().filter(|entry| entry.path() != partial);
        let mut others: Vec<(SystemTime, u64, PathBuf)> = others
            .filter_map(|entry| {
                let found = entry.metadata().ok().filter(fs::Metadata::is_file)?;
                Some((found.modified().ok()?, found.len(), entry.path()))
            })
            .collect();
        others.sort();
        let mut held = size + others.iter().map(|(_, bytes, _)| bytes).sum::<u64>();
        for (_, bytes, path) in others {
            if held <= most {
                return;
            }
            if fs::remove_file(path).is_ok() {
                held -= bytes;
            }
        }
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(bytes).map_err(|error| format!("{}: {error}", self.kept))
    }

    pub(crate) fn keep(self) -> Result<String, String> {
        fs::rename(&self.partial, &self.kept).map_err(|error| format!("{}: {error}", self.kept))?;
        Ok(self.kept.clone())
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.partial);
    }
}

pub(crate) fn sweep_kind(runtime: &Path, kind: &str, now: SystemTime) {
    if let Ok(Some(folder)) = folder::open(&runtime.join(kind)) {
        sweep(folder.path(), now);
    }
}

fn sweep(folder: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(found) = entry.metadata() else {
            continue;
        };
        let gone = found.modified().ok().and_then(|at| at.checked_add(KEPT));
        if found.is_file() && gone.is_some_and(|gone| gone < now) {
            let _ = fs::remove_file(entry.path());
        }
    }
}
