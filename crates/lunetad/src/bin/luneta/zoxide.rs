//! What luneta asks of zoxide: where `z` would take you for a name.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The directory `z name` would take you to: the one zoxide ranks first of
/// those whose last part holds `name`, whatever the case. None where zoxide
/// is not on the PATH, or knows no such directory.
pub fn query(name: &str) -> Option<PathBuf> {
    let output = Command::new("zoxide")
        .args(["query", "--", name])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let path = output.stdout.strip_suffix(b"\n")?;
    (output.status.success() && !path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path)))
}
