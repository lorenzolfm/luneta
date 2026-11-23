use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

pub const SOCKET: &str = "luned.sock";

pub const LOCK: &str = "luned.lock";

pub const LOG: &str = "luned.log";

#[derive(Debug)]
pub enum Unfound {
    #[cfg(target_os = "macos")]
    Unnamed(io::Error),
}

#[cfg(not(target_os = "macos"))]
impl fmt::Display for Unfound {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

#[cfg(target_os = "macos")]
impl fmt::Display for Unfound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Unfound::Unnamed(error) = self;
        write!(
            f,
            "XDG_RUNTIME_DIR is not set to an absolute path, and macOS names no temporary folder \
             of yours: {error}"
        )
    }
}

impl std::error::Error for Unfound {}

#[cfg_attr(not(target_os = "macos"), must_use)]
pub fn find() -> Result<PathBuf, Unfound> {
    find_by(|name| std::env::var_os(name))
}

#[cfg_attr(not(target_os = "macos"), must_use)]
pub fn find_in(environ: &[OsString]) -> Result<PathBuf, Unfound> {
    find_by(|name| {
        let mut set = environ.iter().map(|entry| entry.as_bytes()).filter_map(|entry| {
            let value = entry.strip_prefix(name.as_bytes())?.strip_prefix(b"=")?;
            Some(OsStr::from_bytes(value).to_owned())
        });
        set.next()
    })
}

fn find_by(var: impl Fn(&str) -> Option<OsString>) -> Result<PathBuf, Unfound> {
    let runtime = var("XDG_RUNTIME_DIR").filter(|root| Path::new(root).is_absolute());
    let folder = match runtime {
        Some(root) => PathBuf::from(root).join("lune"),
        None => outside_runtime()?,
    };
    assert!(folder.is_absolute(), "luned's folder is absolute: {}", folder.display());
    Ok(folder)
}

#[cfg(not(target_os = "macos"))]
#[expect(clippy::unnecessary_wraps, reason = "macOS's can fail, and both have one signature")]
fn outside_runtime() -> Result<PathBuf, Unfound> {
    let uid = rustix::process::getuid().as_raw();
    Ok(PathBuf::from(format!("/tmp/lune-{uid}")))
}

#[cfg(target_os = "macos")]
fn outside_runtime() -> Result<PathBuf, Unfound> {
    Ok(PathBuf::from(user_temp().map_err(Unfound::Unnamed)?).join("lune"))
}

#[cfg(target_os = "macos")]
const PATH_BYTES_MAX: usize = libc::PATH_MAX as usize;

#[cfg(target_os = "macos")]
const _: () = assert!(PATH_BYTES_MAX >= 2, "a path has room for `/` and its NUL");

#[cfg(target_os = "macos")]
pub(crate) fn user_temp() -> Result<OsString, io::Error> {
    let mut buffer = [0_u8; PATH_BYTES_MAX];
    #[expect(
        unsafe_code,
        reason = "errno and confstr are C, which libc only declares. confstr writes no more \
                  than the `buffer.len()` bytes it is given"
    )]
    let needed = unsafe {
        *libc::__error() = 0;
        libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), buffer.len())
    };
    if needed == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(0) {
            return Err(io::Error::new(ErrorKind::NotFound, "confstr has no DARWIN_USER_TEMP_DIR"));
        }
        return Err(error);
    }
    assert!(needed <= buffer.len(), "macOS names a folder longer than PATH_MAX");
    assert_eq!(buffer[needed - 1], 0, "confstr ends what it names with a NUL");
    let root = &buffer[..needed - 1];
    assert!(root.starts_with(b"/"), "macOS names a temporary folder that is not absolute");
    assert!(!root.contains(&0), "macOS names a temporary folder with a NUL in it");
    Ok(OsStr::from_bytes(root).to_owned())
}

pub const ANOTHER_RUNS: u8 = 3;

#[derive(Debug)]
pub enum Refusal {
    NotYours(PathBuf),
    Open(PathBuf),
    Running(PathBuf),
    Io(PathBuf, io::Error),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::NotYours(folder) => {
                write!(f, "{} is not a folder of yours alone", folder.display())
            },
            Refusal::Open(folder) => write!(f, "{} is open to other users", folder.display()),
            Refusal::Running(folder) => {
                write!(f, "another luned already runs in {}", folder.display())
            },
            Refusal::Io(path, error) => write!(f, "{}: {error}", path.display()),
        }
    }
}

impl std::error::Error for Refusal {}

#[derive(Clone, Debug)]
pub struct Folder(PathBuf);

impl Folder {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.0.join(SOCKET)
    }
}

pub fn open(path: &Path) -> Result<Option<Folder>, Refusal> {
    match check(path) {
        Ok(()) => Ok(Some(Folder(path.to_owned()))),
        Err(Refusal::Io(_, error)) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

pub fn make(path: &Path) -> Result<Folder, Refusal> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {},
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {},
        Err(error) => return Err(Refusal::Io(path.to_owned(), error)),
    }
    check(path)?;
    Ok(Folder(path.to_owned()))
}

pub fn held(folder: &Folder) -> Result<bool, Refusal> {
    let path = folder.0.join(LOCK);
    let lock = match File::open(&path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(Refusal::Io(path, error)),
    };
    match lock.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(Refusal::Io(path, error)),
    }
}

fn check(folder: &Path) -> Result<(), Refusal> {
    let found = fs::symlink_metadata(folder);
    let found = found.map_err(|error| Refusal::Io(folder.to_owned(), error))?;
    if !found.is_dir() {
        return Err(Refusal::NotYours(folder.to_owned()));
    }
    if found.uid() != rustix::process::getuid().as_raw() {
        return Err(Refusal::NotYours(folder.to_owned()));
    }
    if found.mode() & 0o077 != 0 {
        return Err(Refusal::Open(folder.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn environ(entries: &[&str]) -> Vec<OsString> {
        entries.iter().map(OsString::from).collect()
    }

    #[test]
    fn a_folder_past_xdg_runtime_dir_is_the_user_s_whatever_tmpdir_says() {
        let user_s = find_in(&[]).unwrap();
        if cfg!(not(target_os = "macos")) {
            let uid = rustix::process::getuid().as_raw();
            assert_eq!(user_s, PathBuf::from(format!("/tmp/lune-{uid}")));
        }
        for unset in [
            vec!["XDG_RUNTIME_DIR=run/user/1000", "TMPDIR=tmp"],
            vec!["XDG_RUNTIME_DIR=run", "TMPDIR=/tmp/a"],
            vec!["XDG_RUNTIME_DIR=", "TMPDIR=/tmp/a"],
            vec!["TMPDIR=/tmp/a"],
        ] {
            assert_eq!(find_in(&environ(&unset)).unwrap(), user_s, "{unset:?}");
        }
        let runtime = find_in(&environ(&["XDG_RUNTIME_DIR=/run/user/1000", "TMPDIR=/tmp/a"]));
        assert_eq!(runtime.unwrap(), Path::new("/run/user/1000/lune"));
    }

    fn dev_s_lune_folder() -> String {
        let nix = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../nix/dev/installed.nix");
        let nix = fs::read_to_string(nix).expect("dev's shell is in the source, as the build has it");
        let (head, tail) = ("    lune_folder() {\n", "\n    }\n");
        let start = nix.find(head).expect("installed.nix defines lune_folder");
        let length = nix[start..].find(tail).expect("lune_folder ends") + tail.len();
        let text = &nix[start..start + length];
        let bare = text.replace("''${", "").replace("${name}", "");
        assert!(!bare.contains("''"), "lune_folder escapes only ${{ for nix: {text}");
        assert!(!bare.contains("${"), "lune_folder takes only name from nix: {text}");
        text.replace("''${", "${").replace("${name}", "install")
    }

    #[test]
    fn dev_names_the_folder_luned_finds() {
        let function = dev_s_lune_folder();
        let path = std::env::var_os("PATH").unwrap_or_default();
        for set in [
            vec![],
            vec!["XDG_RUNTIME_DIR=/run/user/1000"],
            vec!["XDG_RUNTIME_DIR=/run/user/1000/", "TMPDIR=/tmp/a"],
            vec!["XDG_RUNTIME_DIR=/run/user/1000//"],
            vec!["XDG_RUNTIME_DIR=run", "TMPDIR=/tmp/a"],
            vec!["XDG_RUNTIME_DIR=", "TMPDIR=/tmp/a"],
            vec!["TMPDIR=/tmp/a"],
        ] {
            let mut bash = std::process::Command::new("bash");
            bash.args(["--noprofile", "--norc", "-euo", "pipefail", "-c"]);
            bash.arg(format!("{function}lune_folder\n"));
            bash.env_clear().env("PATH", &path);
            for entry in &set {
                let (name, value) = entry.split_once('=').unwrap();
                bash.env(name, value);
            }
            let said = bash.output().expect("bash runs, as the devshell and the build have it");
            let stderr = String::from_utf8_lossy(&said.stderr);
            assert!(said.status.success(), "{set:?}: {stderr}");
            assert!(stderr.is_empty(), "{set:?}: {stderr}");
            let named = said.stdout.strip_suffix(b"\n").expect("lune_folder ends its line");
            let found = find_in(&environ(&set)).unwrap();
            assert_eq!(OsStr::from_bytes(named), found.as_os_str(), "{set:?}");
        }
    }
}
