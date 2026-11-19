use std::borrow::Borrow;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::watch::{self, Troubles};

pub mod cli;
pub mod client;
mod ended;
pub mod metadata;
mod renames;
pub mod vars;

pub use ended::{exited, Exited};
pub use renames::renamed;

const WEB_SERVER: &str = "web_server_bus";

const CONTRACT: &str = "contract_version_";

const SPOKEN: &str = "contract_version_1";

pub const NAME_BYTES_MAX: usize = 60;

const _: () = assert!(NAME_BYTES_MAX + 1 == 61);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        let name = SessionName::running(name)?;
        if name.0.len() > NAME_BYTES_MAX {
            return Err("name must be shorter than 61 bytes");
        }
        if name.0.starts_with('-') {
            return Err("name cannot start with '-'");
        }
        Ok(name)
    }

    pub fn running(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        Ok(SessionName(name.to_owned()))
    }

    #[must_use]
    pub fn with_postfix(&self, n: usize) -> Self {
        let postfix = format!("-{n}");
        let mut end = NAME_BYTES_MAX.saturating_sub(postfix.len()).min(self.0.len());
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        SessionName(format!("{}{postfix}", &self.0[..end]))
    }

    #[must_use]
    pub fn free(&self, names: &[&str]) -> Self {
        let taken = |name: &SessionName| names.contains(&name.as_str());
        std::iter::once(self.clone())
            .chain((2..names.len() + 3).map(|n| self.with_postfix(n)))
            .find(|name| !taken(name))
            .expect("one of more names than there are sessions is free")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SessionName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub const SOCKET_DIR: &str = "ZELLIJ_SOCKET_DIR";

#[must_use]
pub fn folder() -> SocketFolder {
    folder_by(|name| std::env::var_os(name))
}

fn folder_by(var: impl Fn(&str) -> Option<OsString>) -> SocketFolder {
    if let Some(dir) = var(SOCKET_DIR).filter(|dir| !dir.is_empty()) {
        return SocketFolder(PathBuf::from(dir));
    }
    if cfg!(target_os = "linux")
        && let Some(runtime) = var("XDG_RUNTIME_DIR").filter(|root| Path::new(root).is_absolute())
    {
        return SocketFolder(PathBuf::from(runtime).join("zellij"));
    }
    let uid = rustix::process::getuid().as_raw();
    SocketFolder(user_temp().join(format!("zellij-{uid}")))
}

#[cfg(target_os = "macos")]
fn user_temp() -> PathBuf {
    crate::folder::user_temp().map_or_else(|_| std::env::temp_dir(), PathBuf::from)
}

#[cfg(not(target_os = "macos"))]
fn user_temp() -> PathBuf {
    PathBuf::from("/tmp")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SocketFolder(PathBuf);

impl SocketFolder {
    #[cfg(test)]
    #[must_use]
    pub fn at(path: &Path) -> Self {
        SocketFolder(path.to_owned())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    fn socket_of(&self, session: &SessionName) -> PathBuf {
        self.0.join(SPOKEN).join(session.as_str())
    }
}

#[must_use]
pub fn of_sessions(path: &Path) -> bool {
    let contract = |path: &Path| {
        path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(CONTRACT))
    };
    contract(path) || path.parent().is_some_and(contract)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Socket {
    pub name: SessionName,
    id: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Id {
    dev: u64,
    ino: u64,
    made: SystemTime,
}

impl Id {
    #[must_use]
    pub fn started(self) -> SystemTime {
        self.made
    }

    #[must_use]
    pub fn socket(self) -> u64 {
        self.ino
    }
}

pub struct Scan {
    pub sockets: Vec<Socket>,
    pub unread: u32,
}

impl Scan {
    #[must_use]
    pub fn alone(&self) -> bool {
        self.sockets.is_empty() && self.unread == 0
    }
}

pub fn scan(folder: &SocketFolder, troubles: &mut Troubles) -> Scan {
    let scan = read(folder.path(), &mut |what| troubles.say(what));
    troubles.done();
    scan
}

#[must_use]
pub fn running(folder: &SocketFolder) -> Option<Vec<(SessionName, SystemTime)>> {
    let sockets = sockets(folder)?.into_iter();
    Some(sockets.map(|(name, id)| (name, id.started())).collect())
}

#[must_use]
pub fn sockets(folder: &SocketFolder) -> Option<Vec<(SessionName, Id)>> {
    let scan = read(folder.path(), &mut |_| {});
    let sockets = scan.sockets.into_iter().map(|socket| (socket.name, socket.id));
    (scan.unread == 0).then(|| sockets.collect())
}

const NEWBORN: Duration = Duration::from_millis(500);

#[must_use]
pub fn newborn(started: impl Iterator<Item = SystemTime>, now: SystemTime) -> Option<Duration> {
    let left = |started| NEWBORN.checked_sub(now.duration_since(started).ok()?);
    started.filter_map(left).filter(|left| !left.is_zero()).max()
}

fn read(folder: &Path, say: &mut dyn FnMut(String)) -> Scan {
    let mut scan = Scan { sockets: Vec::new(), unread: 0 };
    for contract in listed(folder, say, &mut scan.unread) {
        if !contract.file_name().is_some_and(|name| name.to_string_lossy().starts_with(CONTRACT)) {
            continue;
        }
        for path in listed(&contract, say, &mut scan.unread) {
            match socket(&path) {
                Ok(Some(socket)) => scan.sockets.push(socket),
                Ok(None) => {},
                Err(error) if error.kind() == ErrorKind::NotFound => {},
                Err(error) => {
                    say(format!("{}: {error}", path.display()));
                    scan.unread += 1;
                },
            }
        }
    }
    scan
}

fn listed(dir: &Path, say: &mut dyn FnMut(String), unread: &mut u32) -> Vec<PathBuf> {
    watch::listed(dir).unwrap_or_else(|error| {
        say(format!("{}: {error}", dir.display()));
        *unread += 1;
        Vec::new()
    })
}

fn socket(path: &Path) -> io::Result<Option<Socket>> {
    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return Ok(None);
    };
    if name == WEB_SERVER {
        return Ok(None);
    }
    let Ok(name) = SessionName::running(name) else {
        return Ok(None);
    };
    let found = fs::symlink_metadata(path)?;
    if !found.file_type().is_socket() {
        return Ok(None);
    }
    let id = Id { dev: found.dev(), ino: found.ino(), made: found.modified()? };
    Ok(Some(Socket { name, id }))
}

#[derive(Default)]
pub struct Sessions {
    names: HashMap<Id, SessionName>,
    held: HashMap<SessionName, Vec<Id>>,
}

impl Sessions {
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
        for held in self.held.values() {
            assert_ne!(held.as_slice(), []);
            for id in held {
                assert!(self.names.contains_key(id));
            }
        }
        for (id, name) in &self.names {
            assert!(self.held.get(name).is_some_and(|held| held.contains(id)));
        }
    }

    pub fn running(&self) -> impl Iterator<Item = (Id, &SessionName)> {
        self.names.iter().map(|(id, name)| (*id, name))
    }

    #[must_use]
    pub fn name_of(&self, id: Id) -> Option<&SessionName> {
        self.names.get(&id)
    }

    #[must_use]
    pub fn addressed(&self, name: &SessionName) -> Option<Id> {
        let mut under = self.names.iter().filter(|(_, now)| *now == name);
        let (Some((id, _)), None) = (under.next(), under.next()) else {
            return None;
        };
        Some(*id)
    }

    #[must_use]
    pub fn lead(&self, named: &str, started: Option<SystemTime>) -> Option<&SessionName> {
        let id = self.led(named, started)?;
        Some(self.names.get(&id).expect("a session that holds a name runs"))
    }

    #[must_use]
    pub fn led(&self, named: &str, started: Option<SystemTime>) -> Option<Id> {
        let mut could = self.could_be(named, started);
        let (Some(id), None) = (could.next(), could.next()) else {
            return None;
        };
        Some(id)
    }

    #[must_use]
    pub fn housing(&self, named: &str, started: SystemTime) -> Option<Id> {
        if let Some(id) = self.led(named, Some(started)) {
            return Some(id);
        }
        let named_now = |id: &Id| self.names.get(id).is_some_and(|now| now.as_str() == named);
        let mut could = self.could_be(named, Some(started)).filter(named_now);
        let (Some(id), None) = (could.next(), could.next()) else {
            return None;
        };
        Some(id)
    }

    fn could_be(&self, named: &str, started: Option<SystemTime>) -> impl Iterator<Item = Id> {
        let before = move |id: &Id| started.is_none_or(|started| id.made <= started);
        self.held.get(named).into_iter().flatten().copied().filter(before)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, Permissions};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    use std::time::{Duration, SystemTime};

    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::{folder_by, newborn, running, scan, user_temp, SessionName, SocketFolder};
    use super::{CONTRACT, NAME_BYTES_MAX, NEWBORN, SOCKET_DIR, WEB_SERVER};
    use crate::watch::Troubles;

    #[test]
    fn zellij_s_folder_is_one_for_every_shell_whatever_tmpdir_says() {
        let folder_in = |set: &[(&str, &str)]| {
            let set: Vec<(String, OsString)> =
                set.iter().map(|&(name, value)| (name.to_owned(), value.into())).collect();
            folder_by(|name| set.iter().find(|(set, _)| set == name).map(|(_, value)| value.clone()))
        };
        let uid = rustix::process::getuid().as_raw();
        let user_s = SocketFolder(user_temp().join(format!("zellij-{uid}")));
        assert_eq!(folder_in(&[]), user_s);
        assert_eq!(folder_in(&[("TMPDIR", "/var/T/nix-shell.a")]), user_s);
        assert_eq!(folder_in(&[("XDG_RUNTIME_DIR", "run"), ("TMPDIR", "/tmp/a")]), user_s);
        let given = folder_in(&[(SOCKET_DIR, "/tmp/z"), ("XDG_RUNTIME_DIR", "/run/user/1000")]);
        assert_eq!(given, SocketFolder(PathBuf::from("/tmp/z")));
        let runtime = folder_in(&[("XDG_RUNTIME_DIR", "/run/user/1000"), ("TMPDIR", "/tmp/a")]);
        if cfg!(target_os = "linux") {
            assert_eq!(runtime, SocketFolder(PathBuf::from("/run/user/1000/zellij")));
        } else {
            assert_eq!(runtime, user_s);
        }
    }

    #[test]
    fn zellij_is_not_alone_where_its_folder_cannot_be_read() {
        let folder = tempfile::tempdir().unwrap();
        let contract = folder.path().join(format!("{CONTRACT}1"));
        fs::create_dir(&contract).unwrap();
        let mut troubles = Troubles::default();
        assert!(scan(&SocketFolder::at(folder.path()), &mut troubles).alone());

        fs::set_permissions(&contract, Permissions::from_mode(0o000)).unwrap();
        let unread = scan(&SocketFolder::at(folder.path()), &mut troubles);
        fs::set_permissions(&contract, Permissions::from_mode(0o755)).unwrap();
        assert!(!unread.alone());
    }

    #[test]
    fn the_sessions_that_run_are_their_sockets_unless_one_could_go_unread() {
        let folder = tempfile::tempdir().unwrap();
        let contract = folder.path().join(format!("{CONTRACT}1"));
        fs::create_dir(&contract).unwrap();
        let _work = UnixListener::bind(contract.join("work")).unwrap();
        let _web = UnixListener::bind(contract.join(WEB_SERVER)).unwrap();
        let _blank = UnixListener::bind(contract.join("  ")).unwrap();
        fs::write(contract.join("notes"), "").unwrap();
        let made = fs::metadata(contract.join("work")).unwrap().modified().unwrap();
        let at = SocketFolder::at(folder.path());
        assert_eq!(running(&at), Some(vec![(SessionName::running("work").unwrap(), made)]));

        fs::set_permissions(&contract, Permissions::from_mode(0o000)).unwrap();
        let unread = running(&at);
        fs::set_permissions(&contract, Permissions::from_mode(0o755)).unwrap();
        assert_eq!(unread, None);
    }

    #[test]
    fn a_postfix_makes_room_for_itself_between_characters() {
        let Ok(name) = SessionName::parse("luneta-main") else {
            panic!("a name zellij takes");
        };
        assert_eq!(name.with_postfix(2).as_str(), "luneta-main-2");
        let Ok(long) = SessionName::parse(&format!("{}é", "x".repeat(NAME_BYTES_MAX - 3))) else {
            panic!("a name zellij takes");
        };
        let postfixed = long.with_postfix(12);
        assert_eq!(postfixed.as_str(), format!("{}-12", "x".repeat(NAME_BYTES_MAX - 3)));
        assert!(SessionName::parse(postfixed.as_str()).is_ok());
    }

    #[test]
    fn a_name_that_names_no_file_is_no_session_s() {
        for name in ["../out", "..", ".", ""] {
            assert!(SessionName::parse(name).is_err(), "{name:?}");
            assert!(SessionName::running(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_name_that_starts_with_a_dash_is_made_by_no_one_but_zellij() {
        assert!(SessionName::parse("-c").is_err());
        assert!(SessionName::running("-c").is_ok());
    }

    #[test]
    fn the_sweep_waits_out_the_youngest_newborn_session() {
        let now = SystemTime::now();
        let ago = |age: Duration| now - age;
        let quarter = NEWBORN / 4;
        let started = [ago(quarter), ago(quarter * 3), ago(NEWBORN * 4)];
        assert_eq!(newborn(started.into_iter(), now), Some(quarter * 3));
        assert_eq!(newborn([ago(NEWBORN), ago(NEWBORN * 4)].into_iter(), now), None);
        assert_eq!(newborn([now + Duration::from_mins(1)].into_iter(), now), None);
        assert_eq!(newborn(std::iter::empty(), now), None);
    }
}
