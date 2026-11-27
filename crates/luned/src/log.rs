use std::error::Error;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, ErrorKind, Write as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state;

pub const NAME: &str = "lune.log";

pub const MAX: u64 = 1024 * 1024;

const SAID_MAX: usize = 4096;

static STARTED: OnceLock<Started> = OnceLock::new();

#[derive(Clone, Copy)]
pub enum Process {
    Lune,
    Luned,
}

impl Process {
    fn name(self) -> &'static str {
        match self {
            Process::Lune => "lune",
            Process::Luned => "luned",
        }
    }
}

struct Started {
    process: Process,
    log: Option<Log>,
}

pub fn start(process: Process) -> Result<(), String> {
    let log = match state::path(NAME) {
        Ok(path) => {
            Log::open(path.clone(), process).map_err(|error| format!("{}: {error}", path.display()))
        }
        Err(unset) => Err(unset.to_string()),
    };
    let (log, why) = match log {
        Ok(log) => (Some(log), Ok(())),
        Err(why) => (None, Err(why)),
    };
    let _ = STARTED.set(Started { process, log });
    why
}

pub fn say(what: &str) {
    let Some(started) = STARTED.get() else {
        return;
    };
    if let Some(log) = &started.log {
        let _ = log.write(what, SystemTime::now());
    }
    match started.process {
        Process::Lune => {}
        Process::Luned => {
            let line = format!("luned: {what}\n");
            let _ = io::stderr().lock().write_all(line.as_bytes());
        }
    }
}

pub fn causes(error: &dyn Error) -> String {
    let mut line = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        let said = error.to_string();
        if !line.ends_with(&said) {
            line.push_str(": ");
            line.push_str(&said);
        }
        cause = error.source();
    }
    line
}

pub fn panicked(panic: &PanicHookInfo<'_>) {
    let Some(log) = STARTED.get().and_then(|started| started.log.as_ref()) else {
        return;
    };
    let current = thread::current();
    let mut what = format!(
        "thread '{}' panicked",
        current.name().unwrap_or("<unnamed>")
    );
    if let Some(at) = panic.location() {
        let _ = write!(what, " at {at}");
    }
    let _ = write!(
        what,
        ": {}",
        panic.payload_as_str().unwrap_or("Box<dyn Any>")
    );
    let _ = log.write(&what, SystemTime::now());
}

struct Log {
    path: PathBuf,
    who: String,
    unmoved: AtomicBool,
}

impl Log {
    fn open(path: PathBuf, process: Process) -> io::Result<Log> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        opened(&path)?;
        let who = format!("{}[{}]", process.name(), std::process::id());
        Ok(Log {
            path,
            who,
            unmoved: AtomicBool::new(false),
        })
    }

    fn write(&self, what: &str, now: SystemTime) -> io::Result<()> {
        let mut file = opened(&self.path)?;
        if file.metadata()?.len() >= MAX {
            file = self.moved(file, now);
        }
        file.write_all(self.line(what, now).as_bytes())
    }

    fn line(&self, what: &str, now: SystemTime) -> String {
        format!("{} {} {}\n", utc(now), self.who, said(what))
    }

    fn moved(&self, full: File, now: SystemTime) -> File {
        match full.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return full,
            Err(TryLockError::Error(error)) => return self.unmoved(full, &error, now),
        }
        let named = same_file(&full, &self.path).unwrap_or(false);
        if named && let Err(error) = fs::rename(&self.path, before(&self.path)) {
            return self.unmoved(full, &error, now);
        }
        opened(&self.path).unwrap_or(full)
    }

    fn unmoved(&self, mut full: File, error: &io::Error, now: SystemTime) -> File {
        if !self.unmoved.swap(true, Ordering::Relaxed) {
            let why = format!("{NAME} does not move to {NAME}.1: {error}");
            let _ = full.write_all(self.line(&why, now).as_bytes());
        }
        full
    }
}

fn opened(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let open = file.metadata()?;
    match fs::metadata(path) {
        Ok(named) => Ok(open.dev() == named.dev() && open.ino() == named.ino()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn before(path: &Path) -> PathBuf {
    let mut before = path.as_os_str().to_owned();
    before.push(".1");
    PathBuf::from(before)
}

fn said(what: &str) -> String {
    let mut said = String::new();
    for character in what.trim_end().chars() {
        if said.len() >= SAID_MAX {
            said.push('…');
            break;
        }
        if character.is_control() {
            said.extend(character.escape_default());
        } else {
            said.push(character);
        }
    }
    said
}

fn utc(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let (year, month, day) = date(seconds / 86_400);
    let of_day = seconds % 86_400;
    let (hour, minute, second) = (of_day / 3600, of_day / 60 % 60, of_day % 60);
    let milli = since.subsec_millis();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

fn date(days: u64) -> (u64, u64, u64) {
    let days = days + 719_468;
    let era = days / 146_097;
    let of_era = days % 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * of_year + 2) / 153;
    let day = of_year - (153 * month + 2) / 5 + 1;
    let month = if month < 10 { month + 3 } else { month - 9 };
    let year = era * 400 + year_of_era + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{Log, MAX, NAME, Process, SAID_MAX, utc};

    fn at(seconds: u64, millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_millis(millis)
    }

    #[test]
    fn a_line_says_when_which_process_and_what() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("lune").join(NAME);
        let log = Log::open(path.clone(), Process::Lune).unwrap();
        log.write(
            "lune-mux did not take its client there",
            at(1_793_282_592, 481),
        )
        .unwrap();
        log.write("and again", at(1_793_282_593, 7)).unwrap();
        let pid = std::process::id();
        let expected = format!(
            "2026-10-29T14:03:12.481Z lune[{pid}] lune-mux did not take its client there\n\
             2026-10-29T14:03:13.007Z lune[{pid}] and again\n"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    #[test]
    fn a_line_is_dated_as_the_calendar_has_it() {
        let dated = [
            (0, "1970-01-01T00:00:00.000Z"),
            (951_868_799, "2000-02-29T23:59:59.000Z"),
            (951_868_800, "2000-03-01T00:00:00.000Z"),
            (1_735_689_599, "2024-12-31T23:59:59.000Z"),
            (4_107_542_400, "2100-03-01T00:00:00.000Z"),
        ];
        for (seconds, date) in dated {
            assert_eq!(utc(at(seconds, 0)), date, "{seconds}");
        }
        assert_eq!(
            utc(UNIX_EPOCH - Duration::from_secs(1)),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn what_is_said_is_one_line_of_its_own() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        let log = Log::open(path.clone(), Process::Lune).unwrap();
        log.write("gh said:\nError: \u{1b}[31mno\u{1b}[0m\n", at(0, 0))
            .unwrap();
        log.write(&"x".repeat(SAID_MAX * 2), at(0, 0)).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        assert!(
            lines[0].ends_with(r"gh said:\nError: \u{1b}[31mno\u{1b}[0m"),
            "{written}"
        );
        assert!(
            lines[1].ends_with(&format!(" {}…", "x".repeat(SAID_MAX))),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn a_full_log_moves_to_one_in_the_place_of_the_one_before() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        let moved = state.path().join(format!("{NAME}.1"));
        fs::write(&moved, "the one before\n").unwrap();
        let full = format!("{}\n", "x".repeat(usize::try_from(MAX).unwrap()));
        fs::write(&path, &full).unwrap();
        Log::open(path.clone(), Process::Lune)
            .unwrap()
            .write("past it", at(0, 0))
            .unwrap();
        assert_eq!(fs::read_to_string(&moved).unwrap(), full);
        let written = fs::read_to_string(&path).unwrap();
        assert!(
            written.ends_with(" past it\n") && written.lines().count() == 1,
            "{written}"
        );
    }

    #[test]
    fn a_full_log_that_cannot_move_keeps_its_lines_and_says_why_once() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        fs::create_dir(state.path().join(format!("{NAME}.1"))).unwrap();
        let full = format!("{}\n", "x".repeat(usize::try_from(MAX).unwrap()));
        fs::write(&path, &full).unwrap();
        let log = Log::open(path.clone(), Process::Lune).unwrap();
        log.write("past it", at(0, 0)).unwrap();
        log.write("and again", at(0, 0)).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let past: Vec<&str> = written[full.len()..].lines().collect();
        assert_eq!(past.len(), 3, "{past:?}");
        assert!(
            past[0].contains(&format!("] {NAME} does not move to {NAME}.1: ")),
            "{past:?}"
        );
        assert!(
            past[1].ends_with("] past it") && past[2].ends_with("] and again"),
            "{past:?}"
        );
    }

    #[test]
    fn writers_that_fill_the_log_at_once_move_it_once_and_lose_no_line() {
        const WRITERS: usize = 8;
        const LINES: usize = 200;
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        let room = 2000;
        let nearly = format!("{}\n", "x".repeat(usize::try_from(MAX).unwrap() - room - 1));
        fs::write(&path, &nearly).unwrap();
        thread::scope(|scope| {
            for writer in 0..WRITERS {
                let log = Log::open(path.clone(), Process::Luned).unwrap();
                scope.spawn(move || {
                    for line in 0..LINES {
                        log.write(&format!("writer {writer} line {line}"), at(0, 0))
                            .unwrap();
                    }
                });
            }
        });
        let moved = fs::read_to_string(state.path().join(format!("{NAME}.1"))).unwrap();
        assert!(moved.starts_with(&nearly), "the log was moved twice");
        let now = fs::read_to_string(&path).unwrap();
        let written: Vec<&str> = moved[nearly.len()..].lines().chain(now.lines()).collect();
        let said: HashSet<String> = written
            .iter()
            .map(|line| {
                line.split_once("] ")
                    .map_or("", |(_, said)| said)
                    .to_owned()
            })
            .collect();
        let expected: HashSet<String> = (0..WRITERS)
            .flat_map(|writer| (0..LINES).map(move |line| format!("writer {writer} line {line}")))
            .collect();
        assert_eq!(written.len(), WRITERS * LINES);
        assert_eq!(said, expected);
    }
}
