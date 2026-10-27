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
//! it runs, and so leads such a name on to the one the session has now.
//!
//! A name is not a session, though: once a session lets it go, another can
//! take it. So a name leads only to a session that held it and was made before
//! the agent started, as the pane it runs in opened in its session before then,
//! and only when one session alone could be it. A rename from before the daemon
//! started, it never saw: the name leads nowhere, or to the one session made
//! under it since, if that one is older than the agent. Only an agent started
//! after that, in a pane older than the rename, it puts in the wrong session.
//! A shell, where more than one session could be its, goes to the one that has
//! the name now, as `housing` says.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::watch::{self, Troubles};

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

/// Whether a change at `path`, in the folder, can be a session's: one at a
/// folder of sockets, or at a socket in one. zellij writes its log in the
/// folder too where it is the temporary one, as on macOS, and does so all
/// the time, to no session's account. Only the end of the path is read, as
/// macOS can name the folder by a path of its own: `/private/var` for `/var`.
#[must_use]
pub fn of_sessions(path: &Path) -> bool {
    let contract = |path: &Path| {
        path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(CONTRACT))
    };
    contract(path) || path.parent().is_some_and(contract)
}

/// A session that runs, by its socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Socket {
    pub name: String,
    id: Id,
}

/// Which session runs, by what its socket keeps through a rename. The time it
/// was made keeps a new socket that takes the inode of one gone from passing
/// for it. Only a socket the daemon found makes one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id {
    dev: u64,
    ino: u64,
    made: SystemTime,
}

impl Id {
    /// When the session started: when its server made its socket.
    #[must_use]
    pub fn started(self) -> SystemTime {
        self.made
    }
}

/// The sessions whose sockets are in `folder` now. What goes wrong in reading
/// it, `troubles` says.
pub fn scan(folder: &Path, troubles: &mut Troubles) -> Vec<Socket> {
    let mut sockets = Vec::new();
    for contract in listed(folder, troubles) {
        if !contract.file_name().is_some_and(|name| name.to_string_lossy().starts_with(CONTRACT)) {
            continue;
        }
        for path in listed(&contract, troubles) {
            match socket(&path) {
                Ok(Some(socket)) => sockets.push(socket),
                Ok(None) => {},
                // A socket gone since the folder was listed is a session that
                // has ended.
                Err(error) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => troubles.say(format!("{}: {error}", path.display())),
            }
        }
    }
    troubles.done();
    sockets
}

/// The paths in `dir`, or none when it cannot be listed, which `troubles` says.
fn listed(dir: &Path, troubles: &mut Troubles) -> Vec<PathBuf> {
    watch::listed(dir).unwrap_or_else(|error| {
        troubles.say(format!("{}: {error}", dir.display()));
        Vec::new()
    })
}

/// The session whose socket is at `path`, if that is one: zellij names a
/// session in UTF-8, and its web server's socket is no session's.
fn socket(path: &Path) -> io::Result<Option<Socket>> {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    if name == WEB_SERVER {
        return Ok(None);
    }
    let found = fs::symlink_metadata(path)?;
    if !found.file_type().is_socket() {
        return Ok(None);
    }
    let id = Id { dev: found.dev(), ino: found.ino(), made: found.modified()? };
    Ok(Some(Socket { name: name.to_owned(), id }))
}

/// What the daemon has seen of zellij's sessions while it ran.
#[derive(Default)]
pub struct Sessions {
    /// The name each session that runs has now.
    names: HashMap<Id, String>,
    /// Each name seen, and every session that runs and has held it: the one
    /// it was renamed from, and one that took it later, side by side.
    held: HashMap<String, Vec<Id>>,
}

impl Sessions {
    /// Takes in the sessions that run now. A session that is gone takes the
    /// names it had with it.
    pub fn saw(&mut self, sockets: Vec<Socket>) {
        self.names = sockets.iter().map(|socket| (socket.id, socket.name.clone())).collect();
        for socket in sockets {
            let held = self.held.entry(socket.name).or_default();
            if !held.contains(&socket.id) {
                held.push(socket.id);
            }
        }
        let names = &self.names;
        self.held.retain(|_, held| {
            held.retain(|id| names.contains_key(id));
            !held.is_empty()
        });
        // Every session that holds a name runs, and holds the one it has now.
        for held in self.held.values() {
            assert!(!held.is_empty());
            for id in held {
                assert!(self.names.contains_key(id));
            }
        }
        for (id, name) in &self.names {
            assert!(self.held.get(name).is_some_and(|held| held.contains(id)));
        }
    }

    /// The sessions that run, and the name each has now.
    pub fn running(&self) -> impl Iterator<Item = (Id, &str)> {
        self.names.iter().map(|(id, name)| (*id, name.as_str()))
    }

    /// The name the session `id` has now, if it runs.
    #[must_use]
    pub fn name_of(&self, id: Id) -> Option<&str> {
        self.names.get(&id).map(String::as_str)
    }

    /// The session that zellij answers for by `name`: the one that runs under
    /// it, if no other does. Of sessions that share a name, zellij answers for
    /// the one of its own contract, and the daemon cannot tell which that is.
    #[must_use]
    pub fn addressed(&self, name: &str) -> Option<Id> {
        let mut under = self.names.iter().filter(|(_, now)| *now == name);
        let (Some((id, _)), None) = (under.next(), under.next()) else {
            return None;
        };
        Some(*id)
    }

    /// The name that the session once called `named` has now, for an agent
    /// that started at `started`: a session made after it is not the one its
    /// pane opened in. None when no session could be it, or more than one.
    #[must_use]
    pub fn lead(&self, named: &str, started: Option<SystemTime>) -> Option<&str> {
        let id = self.led(named, started)?;
        let now = self.names.get(&id).expect("a session that holds a name runs");
        Some(now.as_str())
    }

    /// The session that `lead` leads to.
    #[must_use]
    pub fn led(&self, named: &str, started: Option<SystemTime>) -> Option<Id> {
        let mut could = self.could_be(named, started);
        let (Some(id), None) = (could.next(), could.next()) else {
            return None;
        };
        Some(id)
    }

    /// The session of a shell that started at `started`, in a session called
    /// `named` then: the one `led` leads to, or else, of those that could be
    /// it, the one that has the name now. A session made under the name of one
    /// renamed, while that one runs, so has the shells started in it, as the
    /// plugin's sidebar has them; a shell started since in an old pane of the
    /// one renamed, which says its old name too, goes there as well. An agent
    /// goes by `lead` alone: where that gives up, the sidebar finds its pane
    /// by where it works, which a shell does not say.
    #[must_use]
    pub fn housing(&self, named: &str, started: SystemTime) -> Option<Id> {
        if let Some(id) = self.led(named, Some(started)) {
            return Some(id);
        }
        let named_now = |id: &Id| self.names.get(id).is_some_and(|now| now == named);
        let mut could = self.could_be(named, Some(started)).filter(named_now);
        let (Some(id), None) = (could.next(), could.next()) else {
            return None;
        };
        Some(id)
    }

    /// The sessions that have held `named`, but for those made after
    /// `started`.
    fn could_be(&self, named: &str, started: Option<SystemTime>) -> impl Iterator<Item = Id> {
        let before = move |id: &Id| started.is_none_or(|started| id.made <= started);
        self.held.get(named).into_iter().flatten().copied().filter(before)
    }
}
