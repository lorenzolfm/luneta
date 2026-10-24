//! Where luneta keeps the files of what runs on a host: the folder `luneta`
//! in `$XDG_RUNTIME_DIR`, or in `$TMPDIR` where that is not set, as on macOS,
//! which has no `XDG_RUNTIME_DIR`. The shell hook keeps its commands there by
//! the same rule, and the daemon and its CLI find each other there.

use std::path::PathBuf;

/// The socket the daemon listens on, in the folder.
pub const SOCKET: &str = "lunetad.sock";

/// The file the daemon holds a lock on while it runs, in the folder.
pub const LOCK: &str = "lunetad.lock";

/// The folder, by this process's variables. A variable set to nothing is not
/// set, as the shell hook takes it.
#[must_use]
pub fn find() -> Option<PathBuf> {
    ["XDG_RUNTIME_DIR", "TMPDIR"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|root| !root.is_empty())
        .map(|root| PathBuf::from(root).join("luneta"))
}
