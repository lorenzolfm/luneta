//! The files luneta keeps a while for an agent of this machine to read, each
//! kind in a folder of its own in luneta's: what a pane printed, as `printed`
//! keeps it, and an image, as `images` does. Each is for this user alone, and
//! goes a day on, as another of its kind is kept.

use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::folder;

/// How long a file is kept: luneta takes away older ones as it keeps another.
/// The agent reads it as it starts, or as its path is pasted on its prompt.
const KEPT: Duration = Duration::from_hours(24);

/// What a rule of Claude Code's reads as more than a path, as gitignore
/// does, or as where the rule ends: a file whose path holds one is not let
/// read by a rule that names it.
const PATTERN: [char; 7] = ['*', '?', '[', ']', '\\', '(', ')'];

/// How many files this luneta has kept, which tells apart the names of
/// those it keeps in the same moment.
static FILES: AtomicU64 = AtomicU64::new(0);

/// A file on its way to being kept, under a name that says so, which no one is
/// told: the name it is kept by holds all of it from the moment it exists,
/// however the daemon ends. One that never gets there goes as this is dropped,
/// or, where the daemon was killed, a day on, as any other.
pub(crate) struct Draft {
    file: File,
    partial: String,
    kept: String,
}

impl Draft {
    /// A new file of `extension`, empty, for this user alone, in the folder
    /// `kind` of luneta's folder `runtime`, made as the daemon makes it; and
    /// takes away those kept there `KEPT` before `now`. None where the
    /// folder's path, with every link in it followed, is not text, or holds
    /// what a rule of Claude Code's reads as a pattern: no rule would let the
    /// agent read it.
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

    /// Writes the next of its bytes, or says why not.
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.file.write_all(bytes).map_err(|error| format!("{}: {error}", self.kept))
    }

    /// Gives the file the name it is kept by, all at once, and says it.
    pub(crate) fn keep(self) -> Result<String, String> {
        fs::rename(&self.partial, &self.kept).map_err(|error| format!("{}: {error}", self.kept))?;
        Ok(self.kept.clone())
    }
}

impl Drop for Draft {
    /// One kept no longer goes by this name, and this finds nothing.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.partial);
    }
}

/// Takes away the files in `folder` last written `KEPT` before `now`. One
/// that does not go stays for the next to take away.
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
