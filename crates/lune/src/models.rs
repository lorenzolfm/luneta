use std::env;
use std::ffi::OsString;
use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TrySendError};
use std::sync::{Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use luned::program::{self, Wanted};

use crate::brief::Asking;
use crate::due::Due;
use crate::host::Event;

#[derive(Clone, Copy, Debug)]
pub struct Pin {
    pub file: &'static str,
    pub url: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Fetched {
    Got(u64),
    Ended(Result<(), String>),
}

#[cfg(target_os = "macos")]
const SUM: &[&str] = &["shasum", "-a", "256"];
#[cfg(not(target_os = "macos"))]
const SUM: &[&str] = &["sha256sum"];

const FETCH: Duration = Duration::from_hours(1);

const CHECK: Duration = Duration::from_mins(2);

const PROGRESS: Duration = Duration::from_millis(250);

const LOCK_WAITS_MAX: usize = 32_768;

const PROGRESS_TICKS_MAX: usize = 32_768;

const _: () = assert!(LOCK_WAITS_MAX as u128 > FETCH.as_millis() / PROGRESS.as_millis());
const _: () = assert!(PROGRESS_TICKS_MAX as u128 > FETCH.as_millis() / PROGRESS.as_millis());

impl Pin {
    pub fn path(&self) -> Result<PathBuf, String> {
        self.kept_in(env::var_os("XDG_DATA_HOME"), env::var_os("HOME"))
    }

    fn kept_in(&self, data: Option<OsString>, home: Option<OsString>) -> Result<PathBuf, String> {
        let set =
            |value: Option<OsString>| value.map(PathBuf::from).filter(|path| path.is_absolute());
        let data = set(data)
            .or_else(|| set(home).map(|home| home.join(".local").join("share")))
            .ok_or("neither XDG_DATA_HOME nor HOME is set to an absolute path")?;
        Ok(data
            .join("lune")
            .join("models")
            .join(self.sha256)
            .join(self.file))
    }

    fn whole(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|kept| kept.is_file() && kept.len() == self.bytes)
    }
}

pub fn fetched(pins: &[Pin]) -> bool {
    pins.iter()
        .all(|pin| pin.path().is_ok_and(|path| pin.whole(&path)))
}

pub const fn bytes(pins: &[Pin]) -> u64 {
    let (mut at, mut bytes) = (0, 0);
    while at < pins.len() {
        bytes += pins[at].bytes;
        at += 1;
    }
    bytes
}

pub fn fetch(
    pins: &'static [Pin],
    events: &Sender<Event>,
    event: fn(Fetched) -> Event,
    useful: fn() -> Result<(), String>,
) -> Asking {
    let due = Due::new(events, move |ended| event(Fetched::Ended(ended)));
    let events = Mutex::new(events.clone());
    Asking::on_thread(move |wanted| {
        let tell = |got: u64| {
            let _ = events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(event(Fetched::Got(got)));
        };
        due.pay(useful().and_then(|()| all(pins, wanted, &tell)));
    })
}

fn all(pins: &[Pin], wanted: &Wanted, tell: &(dyn Fn(u64) + Sync)) -> Result<(), String> {
    let mut before = 0;
    for pin in pins {
        fetching(pin, wanted, &|got| tell(before + got))?;
        before += pin.bytes;
    }
    Ok(())
}

fn fetching(pin: &Pin, wanted: &Wanted, tell: &(dyn Fn(u64) + Sync)) -> Result<(), String> {
    let path = pin.path()?;
    if pin.whole(&path) {
        return Ok(());
    }
    let folder = path.parent().ok_or("the model is kept in no folder")?;
    fs::create_dir_all(folder)
        .map_err(|error| format!("no folder for the model, {}: {error}", folder.display()))?;
    let part = path.with_added_extension("part");
    let report = || {
        if let Ok(got) = fs::metadata(&part) {
            tell(got.len());
        }
    };
    let lock = File::create(path.with_added_extension("lock"))
        .map_err(|error| format!("the model takes no lock: {error}"))?;
    let lock_deadline = Instant::now() + FETCH;
    let mut locked = false;
    for _ in 0..LOCK_WAITS_MAX {
        match lock.try_lock() {
            Ok(()) => {
                locked = true;
                break;
            }
            Err(TryLockError::WouldBlock) => {
                report();
                if Instant::now() >= lock_deadline {
                    return Err("another fetch held the model lock too long".to_owned());
                }
                if !wanted.wait_for(PROGRESS) {
                    return Err("given up".to_owned());
                }
            }
            Err(TryLockError::Error(error)) => {
                return Err(format!("the model takes no lock: {error}"));
            }
        }
    }
    if !locked {
        return Err("another fetch held the model lock too long".to_owned());
    }
    if pin.whole(&path) {
        return Ok(());
    }
    let fetched = if pin.whole(&part) {
        Ok(())
    } else {
        let mut curl = Command::new("curl");
        curl.args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
        ]);
        curl.arg("--output").arg(&part).arg(pin.url);
        let held = lock
            .try_clone()
            .map_err(|error| format!("the model takes no lock: {error}"))?;
        let ran = thread::scope(|scope| {
            let (done, finished) = mpsc::sync_channel(1);
            let _progress = thread::Builder::new()
                .name("model fetch progress".to_owned())
                .spawn_scoped(scope, move || {
                    for _ in 0..PROGRESS_TICKS_MAX {
                        match finished.recv_timeout(PROGRESS) {
                            Err(RecvTimeoutError::Timeout) => report(),
                            Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                })?;
            let ran = program::run_while_reading(&mut curl, FETCH, wanted, Stdio::from(held));
            match done.try_send(()) {
                Ok(()) | Err(TrySendError::Disconnected(())) => {}
                Err(TrySendError::Full(())) => {
                    unreachable!("fetch progress channel takes one done message");
                }
            }
            ran
        });
        match ran {
            Ok(Some(ran)) => ran.answer().map(drop).map_err(|why| format!("curl: {why}")),
            Ok(None) => Err("given up".to_owned()),
            Err(error) => Err(format!("lune fetches the model with curl: {error}")),
        }
    };
    let checked = fetched.and_then(|()| check(pin, &part, wanted));
    let kept = checked.and_then(|()| {
        fs::rename(&part, &path).map_err(|error| format!("the model is not kept: {error}"))
    });
    if kept.is_err() {
        let _ = fs::remove_file(&part);
    }
    kept
}

fn check(pin: &Pin, file: &Path, wanted: &Wanted) -> Result<(), String> {
    let bytes = fs::metadata(file)
        .map_err(|error| format!("the model is gone: {error}"))?
        .len();
    if bytes != pin.bytes {
        return Err(format!(
            "{} came {bytes} bytes long, and not {}",
            pin.file, pin.bytes
        ));
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
        _ => Err(format!(
            "the {} that came is not lune's pin: its SHA-256 differs",
            pin.file
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: Pin = Pin {
        file: "model.gguf",
        url: "",
        bytes: 1024,
        sha256: "abc",
    };

    #[test]
    fn a_file_is_kept_where_xdg_data_home_says_or_under_home() {
        let set = |value: &str| Some(OsString::from(value));
        let kept = PIN.kept_in(set("/data"), set("/home/you"));
        assert_eq!(kept, Ok(PathBuf::from("/data/lune/models/abc/model.gguf")));
        let kept = PIN.kept_in(set("relative"), set("/home/you"));
        let under_home = "/home/you/.local/share/lune/models/abc/model.gguf";
        assert_eq!(kept, Ok(PathBuf::from(under_home)));
        assert!(PIN.kept_in(None, set("relative")).is_err());
    }

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

    #[test]
    fn a_model_of_no_use_is_not_fetched() {
        let (events, heard) = std::sync::mpsc::channel();
        let no_use = || Err("this machine has no speakers".to_owned());
        let _asking = fetch(&[PIN], &events, Event::VoiceFetched, no_use);
        let Event::VoiceFetched(fetched) = lune_waits::heard("how the fetch went", &heard) else {
            panic!("it says how the fetch went");
        };
        let why = "this machine has no speakers".to_owned();
        assert_eq!(fetched, Fetched::Ended(Err(why)));
    }
}
