//! The zellij sessions that run, and the names they had.
//!
//! Each session is a server that listens on a socket named for the session, in
//! zellij's socket folder. A rename moves the socket to its new name with
//! `std::fs::rename` (`RenameSession` in zellij-server's `screen.rs`), and so
//! the socket keeps its inode, and the time it was made: those two say which
//! session it is, whatever it is called now.
//!
//! A pane's environment holds the name its session had when the pane opened,
//! which a rename leaves behind. The daemon sees each rename that happens while
//! it runs, and so leads such a name on to the one the session has now. A rename
//! from before the daemon started, it never saw, and the name leads nowhere.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

/// The socket in the folder that is zellij's web server's, and no session's.
const WEB_SERVER: &str = "web_server_bus";

/// The folders the sockets are in sit in this one, one for each version of the
/// contract between a zellij client and its server: `contract_version_1` and
/// so on.
const CONTRACT: &str = "contract_version_";

/// Where zellij keeps the folders its sockets are in: `$ZELLIJ_SOCKET_DIR`, or
/// else `zellij` in `$XDG_RUNTIME_DIR` on Linux, or else `zellij-<uid>` in the
/// temporary folder, which is where macOS has them. zellij-utils' `consts.rs`
/// says so.
#[must_use]
pub fn folder() -> PathBuf {
    let set = |name| std::env::var_os(name).filter(|value| !value.is_empty());
    if let Some(dir) = set("ZELLIJ_SOCKET_DIR") {
        return PathBuf::from(dir);
    }
    if cfg!(target_os = "linux")
        && let Some(runtime) = set("XDG_RUNTIME_DIR")
    {
        return PathBuf::from(runtime).join("zellij");
    }
    let uid = rustix::process::getuid().as_raw();
    std::env::temp_dir().join(format!("zellij-{uid}"))
}

/// A session that runs, by its socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Socket {
    pub name: String,
    id: Id,
}

/// What a session's socket keeps through a rename. The time it was made keeps
/// a new socket that takes the inode of one gone from passing for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Id {
    dev: u64,
    ino: u64,
    made: (i64, i64),
}

/// The sessions whose sockets are in `folder` now.
#[must_use]
pub fn scan(folder: &Path) -> Vec<Socket> {
    let listed = |dir: &Path| fs::read_dir(dir).into_iter().flatten().flatten();
    let contracts = listed(folder);
    let contracts = contracts.filter(|entry| entry.file_name().to_string_lossy().starts_with(CONTRACT));
    let sockets = contracts.flat_map(|contract| listed(&contract.path()));
    sockets
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let found = entry.metadata().ok()?;
            if name == WEB_SERVER || !found.file_type().is_socket() {
                return None;
            }
            let made = (found.mtime(), found.mtime_nsec());
            Some(Socket { name, id: Id { dev: found.dev(), ino: found.ino(), made } })
        })
        .collect()
}

/// What the daemon has seen of zellij's sessions while it ran.
#[derive(Default)]
pub struct Sessions {
    /// The name each session that runs has now.
    names: HashMap<Id, String>,
    /// The session behind each name seen, the ones it had before a rename too.
    /// A name that another session takes later is that one's.
    seen: HashMap<String, Id>,
}

impl Sessions {
    /// Takes in the sessions that run now. A session that is gone takes the
    /// names it had with it.
    pub fn saw(&mut self, sockets: Vec<Socket>) {
        self.names = sockets.iter().map(|socket| (socket.id, socket.name.clone())).collect();
        for socket in sockets {
            self.seen.insert(socket.name, socket.id);
        }
        let names = &self.names;
        self.seen.retain(|_, id| names.contains_key(id));
    }

    /// The name that the session once called `named` has now.
    #[must_use]
    pub fn lead(&self, named: &str) -> Option<&str> {
        let id = self.seen.get(named)?;
        self.names.get(id).map(String::as_str)
    }
}
