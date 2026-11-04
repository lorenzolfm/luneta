//! The speech models luneta fetches the first time it needs one, as they are
//! too much to ship with it: from Hugging Face, of a revision pinned here,
//! each file in a folder named by its SHA-256 in `luneta/models` of
//! `$XDG_DATA_HOME`, or of `~/.local/share` where that is not set, on macOS
//! too: a luneta that pins another reads none of this one's. curl fetches it,
//! and `sha256sum`, or `shasum` on macOS, checks it against the size and the
//! SHA-256 it is pinned by: both are on every machine luneta runs on. It takes
//! its name in the folder only once it passes, so that no luneta reads one
//! halfway fetched. One luneta fetches it at a time, under a lock beside it,
//! which curl holds with it, into the one part of it there is: a curl that
//! runs on past a luneta killed outright holds the lock until it is done, and
//! the next luneta checks what it fetched, and keeps it. A luneta that gave
//! up mid-fetch takes its part away.

use std::cell::Cell;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

use lunetad::program;

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;

/// A file of a model, as it is fetched: the name it is kept by, where it is
/// fetched from, of a revision of its own, and how many bytes it is, and the
/// SHA-256 of them. Another pin is another folder: the change that makes one
/// takes away the folders of the others under `luneta/models`, which no
/// luneta reads any more.
#[derive(Clone, Copy, Debug)]
pub struct Pin {
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

/// How the fetch of a model goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    /// How many of its bytes are in.
    Got(u64),
    /// It is in, or why not.
    Ended(Result<(), String>),
}

/// What sums a file's SHA-256, and says it first on its line.
#[cfg(target_os = "macos")]
const SUM: &[&str] = &["shasum", "-a", "256"];
#[cfg(not(target_os = "macos"))]
const SUM: &[&str] = &["sha256sum"];

/// How long a file has to come, over a line of a megabyte a second. One that
/// takes longer is stuck, and is killed.
const FETCH: Duration = Duration::from_hours(1);

/// How long a file's sum has to be worked out, which takes `shasum`, of perl,
/// a few seconds.
const CHECK: Duration = Duration::from_mins(2);

/// How often luneta is told how much of the model is in.
const PROGRESS: Duration = Duration::from_millis(250);

impl Pin {
    /// Where it is kept, or why it cannot be.
    pub fn path(&self) -> Result<PathBuf, String> {
        self.kept_in(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))
    }

    /// Where it is kept, by `$XDG_DATA_HOME` and `$HOME` as they are set, if
    /// they are: one set to a relative path is not, as the spec has it.
    fn kept_in(&self, data: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
        let set =
            |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
        let data = set(data)
            .or_else(|| set(home).map(|home| home.join(".local").join("share")))
            .ok_or("neither XDG_DATA_HOME nor HOME is set to an absolute path")?;
        Ok(data.join("luneta").join("models").join(self.sha256).join(self.file))
    }

    /// Whether what is at `path` is whole: it takes its name only once it
    /// passed its check, and one of another size, as of a copy cut short, or a
    /// disk that filled, is not it, and is fetched again, over it.
    fn whole(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|kept| kept.is_file() && kept.len() == self.bytes)
    }
}

/// Whether all of a model is in.
pub fn fetched(pins: &[Pin]) -> bool {
    pins.iter().all(|pin| pin.path().is_ok_and(|path| pin.whole(&path)))
}

/// How many bytes a model is, all told.
pub const fn bytes(pins: &[Pin]) -> u64 {
    let (mut at, mut bytes) = (0, 0);
    while at < pins.len() {
        bytes += pins[at].bytes;
        at += 1;
    }
    bytes
}

/// Fetches the model `pins` name, and tells `events` how it goes, in the
/// events `event` makes, on a thread of its own, for as long as what it
/// returns is kept: dropped, as luneta ends, curl is killed, and what it
/// fetched thrown away, by the time the drop returns, so that no luneta leaves
/// a fetch going, or half a model, behind.
pub fn fetch(
    pins: &'static [Pin],
    events: &Sender<Event>,
    event: fn(Fetched) -> Event,
) -> Asking {
    let due = Due::new(events, move |ended| event(Fetched::Ended(ended)));
    let events = events.clone();
    Asking::on_thread(move |wanted| {
        let tell = |got: u64| {
            // luneta is on its way out.
            let _ = events.send(event(Fetched::Got(got)));
        };
        due.pay(all(pins, wanted, &tell));
    })
}

/// Fetches each of `pins` that is not in, and tells `tell` how many bytes of
/// them all are in.
fn all(pins: &[Pin], wanted: &dyn Fn() -> bool, tell: &dyn Fn(u64)) -> Result<(), String> {
    let mut before = 0;
    for pin in pins {
        fetching(pin, wanted, &|got| tell(before + got))?;
        before += pin.bytes;
    }
    Ok(())
}

/// Fetches `pin`, for as long as it is `wanted`, as its part beside where it
/// is kept, and gives it its name once it passed its check. It waits for the
/// lock beside it first, which the system lets go of however the luneta and
/// the curl that hold it end, and tells `tell` how far the one that holds it
/// got, and takes what that one kept.
fn fetching(pin: &Pin, wanted: &dyn Fn() -> bool, tell: &dyn Fn(u64)) -> Result<(), String> {
    let path = pin.path()?;
    // Fetched by a luneta before, which takes no lock to find out.
    if pin.whole(&path) {
        return Ok(());
    }
    let folder = path.parent().ok_or("the model is kept in no folder")?;
    fs::create_dir_all(folder)
        .map_err(|error| format!("no folder for the model, {}: {error}", folder.display()))?;
    let part = path.with_added_extension("part");
    let told = Cell::new(Instant::now());
    let still = || {
        if told.get().elapsed() >= PROGRESS {
            told.set(Instant::now());
            if let Ok(got) = fs::metadata(&part) {
                tell(got.len());
            }
        }
        wanted()
    };
    // Let go of as `lock` closes.
    let lock = File::create(path.with_added_extension("lock"))
        .map_err(|error| format!("the model takes no lock: {error}"))?;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if still() => thread::sleep(PROGRESS),
            Err(TryLockError::WouldBlock) => return Err("given up".to_owned()),
            Err(TryLockError::Error(error)) => {
                return Err(format!("the model takes no lock: {error}"));
            },
        }
    }
    // Another luneta kept it, as this one waited.
    if pin.whole(&path) {
        return Ok(());
    }
    // A curl that ran on past a luneta killed outright fetched it all, as
    // this one waited: what it fetched is checked as this one's would be.
    let fetched = if pin.whole(&part) {
        Ok(())
    } else {
        let mut curl = Command::new("curl");
        curl.args(["--fail", "--location", "--silent", "--show-error", "--retry", "3"]);
        curl.arg("--output").arg(&part).arg(pin.url);
        // curl holds the lock with luneta, on its stdin, which it never reads.
        let held = lock.try_clone().map_err(|error| format!("the model takes no lock: {error}"))?;
        match program::run_while_reading(&mut curl, FETCH, still, Stdio::from(held)) {
            Ok(Some(ran)) => ran.answer().map(drop).map_err(|why| format!("curl: {why}")),
            Ok(None) => Err("given up".to_owned()),
            Err(error) => Err(format!("luneta fetches the model with curl: {error}")),
        }
    };
    let checked = fetched.and_then(|()| check(pin, &part, wanted));
    let kept = checked.and_then(|()| {
        fs::rename(&part, &path).map_err(|error| format!("the model is not kept: {error}"))
    });
    if kept.is_err() {
        // Nothing of it was fetched, as curl could not start.
        let _ = fs::remove_file(&part);
    }
    kept
}

/// Whether `file` is `pin`: of its size, and its sum, worked out for as long
/// as it is `wanted`.
fn check(pin: &Pin, file: &Path, wanted: &dyn Fn() -> bool) -> Result<(), String> {
    let bytes = fs::metadata(file).map_err(|error| format!("the model is gone: {error}"))?.len();
    if bytes != pin.bytes {
        return Err(format!("{} came {bytes} bytes long, and not {}", pin.file, pin.bytes));
    }
    let mut sum = Command::new(SUM[0]);
    sum.args(&SUM[1..]).arg(file);
    let said = program::run_while(&mut sum, CHECK, wanted)
        .map_err(|error| format!("{}: {error}", SUM[0]))?
        .ok_or("given up")?
        .answer()
        .map_err(|why| format!("{}: {why}", SUM[0]))?;
    match String::from_utf8_lossy(&said).split_whitespace().next() {
        Some(sum) if sum == pin.sha256 => Ok(()),
        _ => Err(format!("the {} that came is not luneta's pin: its SHA-256 differs", pin.file)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: Pin = Pin { file: "model.gguf", url: "", bytes: 1024, sha256: "abc" };

    #[test]
    fn a_file_is_kept_where_xdg_data_home_says_or_under_home() {
        let set = |value: &str| Some(OsString::from(value));
        let kept = PIN.kept_in(set("/data"), set("/home/you"));
        assert_eq!(kept, Ok(PathBuf::from("/data/luneta/models/abc/model.gguf")));
        let kept = PIN.kept_in(set("relative"), set("/home/you"));
        let under_home = "/home/you/.local/share/luneta/models/abc/model.gguf";
        assert_eq!(kept, Ok(PathBuf::from(under_home)));
        assert!(PIN.kept_in(None, set("relative")).is_err());
    }

    /// A file cut short, as of a copy, is not in: it is fetched over.
    #[test]
    fn only_a_file_of_its_size_is_in() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join(PIN.file);
        assert!(!PIN.whole(&path));
        let file = File::create(&path).unwrap();
        file.set_len(PIN.bytes / 2).unwrap();
        assert!(!PIN.whole(&path));
        file.set_len(PIN.bytes).unwrap();
        assert!(PIN.whole(&path));
    }

    #[test]
    fn a_model_is_as_many_bytes_as_its_files() {
        assert_eq!(bytes(&[PIN, Pin { bytes: 1, ..PIN }]), 1025);
    }
}
