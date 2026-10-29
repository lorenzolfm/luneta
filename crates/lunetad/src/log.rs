//! What goes wrong in luneta and in lunetad, kept for whoever comes to fix
//! it: in `luneta.log` of luneta's state folder, which outlives a restart, as
//! the folder lunetad runs in does not. A line for each thing, with when it
//! happened, in which process, and what it was:
//!
//! ```text
//! 2026-10-29T14:03:12.481Z luneta[4123] zellij did not take its client there
//! ```
//!
//! A process opens the log for each line, writes the line at its end in one
//! write, and closes it: the lines of many lunetas never mix, and none holds
//! the file. Once it is `MAX` long, the log moves to `luneta.log.1`, in the
//! place of the one before, and the next line starts another. The process
//! that moves it holds a lock on it, and moves it only while it still has the
//! log's name: another that found it full too, and takes the lock after,
//! finds another file by that name, and moves nothing. One that finds the
//! lock taken writes its line where it is, which is in `luneta.log.1` once
//! moved: no line is lost to a move. Where the folder does not let the log
//! be locked or moved, it grows on, and says why in it.

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

/// The log, in luneta's state folder.
pub const NAME: &str = "luneta.log";

/// How long the log grows before it moves to `.1` for another: the two hold
/// twice this at the most, and the lines written as it moves, where the
/// folder lets it be locked and moved.
pub const MAX: u64 = 1024 * 1024;

/// The most of what happened that a line says, in bytes, and a character
/// more: zellij can say a lot as it fails.
const SAID_MAX: usize = 4096;

/// What this process says, once it has started the log.
static STARTED: OnceLock<Started> = OnceLock::new();

/// Which of luneta's processes says what happens to it, as each line names
/// it, and where else it says it.
#[derive(Clone, Copy)]
pub enum Process {
    /// The sidebar, which says it in the log alone: its stderr is the
    /// terminal it draws on.
    Luneta,
    /// The daemon, which says it on its stderr too, after its name: luneta
    /// keeps that in `lunetad.log` beside the socket, and reads it back of a
    /// daemon that does not start.
    Lunetad,
}

impl Process {
    fn name(self) -> &'static str {
        match self {
            Process::Luneta => "luneta",
            Process::Lunetad => "lunetad",
        }
    }
}

/// A process that says what happens to it.
struct Started {
    process: Process,
    /// None where there is no log to write in.
    log: Option<Log>,
}

/// Has what `process` says from here on go in the log, and where else it
/// says it. Where there is no log to write in, it goes there alone, and the
/// error says why. Only the first start counts.
pub fn start(process: Process) -> Result<(), String> {
    let log = match state::path(NAME) {
        Ok(path) => {
            Log::open(path.clone(), process).map_err(|error| format!("{}: {error}", path.display()))
        },
        Err(unset) => Err(unset.to_string()),
    };
    let (log, why) = match log {
        Ok(log) => (Some(log), Ok(())),
        Err(why) => (None, Err(why)),
    };
    let _ = STARTED.set(Started { process, log });
    why
}

/// Says `what`, of what happens to this process: in the log, and where else
/// it says it. One that has not started the log, as a test, says nothing.
pub fn say(what: &str) {
    let Some(started) = STARTED.get() else {
        return;
    };
    // The log first, so that whoever reads the line on stderr finds it there.
    if let Some(log) = &started.log {
        // A line that does not go in has nowhere left to say so.
        let _ = log.write(what, SystemTime::now());
    }
    match started.process {
        Process::Luneta => {},
        Process::Lunetad => eprintln!("lunetad: {what}"),
    }
}

/// Says in the log that this process panicked, where and why, as `panic`
/// says. Its stderr has that from the hook that std sets.
pub fn panicked(panic: &PanicHookInfo<'_>) {
    let Some(log) = STARTED.get().and_then(|started| started.log.as_ref()) else {
        return;
    };
    let current = thread::current();
    let mut what = format!("thread '{}' panicked", current.name().unwrap_or("<unnamed>"));
    if let Some(at) = panic.location() {
        let _ = write!(what, " at {at}");
    }
    let _ = write!(what, ": {}", panic.payload_as_str().unwrap_or("Box<dyn Any>"));
    let _ = log.write(&what, SystemTime::now());
}

/// The log at a path, as a process writes it.
struct Log {
    path: PathBuf,
    /// The process, as each line names it: its name and its pid, as
    /// `luneta[4123]`.
    who: String,
    /// Whether this process has said why the log does not move, once full.
    unmoved: AtomicBool,
}

impl Log {
    /// The log at `path`, for `process` to write in, made where it is not
    /// yet, with its folder.
    fn open(path: PathBuf, process: Process) -> io::Result<Log> {
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder)?;
        }
        opened(&path)?;
        let who = format!("{}[{}]", process.name(), std::process::id());
        Ok(Log { path, who, unmoved: AtomicBool::new(false) })
    }

    /// Writes that `what` happened at `now`, at the end of the log, or of a
    /// new one where it is full.
    fn write(&self, what: &str, now: SystemTime) -> io::Result<()> {
        let mut file = opened(&self.path)?;
        if file.metadata()?.len() >= MAX {
            file = self.moved(file, now);
        }
        file.write_all(self.line(what, now).as_bytes())
    }

    /// That `what` happened at `now`, on a line of its own.
    fn line(&self, what: &str, now: SystemTime) -> String {
        format!("{} {} {}\n", utc(now), self.who, said(what))
    }

    /// Moves the log, `full`, to `.1`, and opens the one in its place, unless
    /// another process moves it now: the line then goes in `full`, which is
    /// in `.1` once moved. Where the log cannot be moved, the line goes in
    /// `full` too: a line kept is worth more than the bound.
    fn moved(&self, full: File, now: SystemTime) -> File {
        match full.try_lock() {
            Ok(()) => {},
            Err(TryLockError::WouldBlock) => return full,
            Err(TryLockError::Error(error)) => return self.unmoved(full, &error, now),
        }
        // Unless another process has moved it since it was opened, and the
        // file by its name is a new one.
        let named = same_file(&full, &self.path).unwrap_or(false);
        if named && let Err(error) = fs::rename(&self.path, before(&self.path)) {
            return self.unmoved(full, &error, now);
        }
        // `full` lets go of its lock as it closes, once the new one is open.
        opened(&self.path).unwrap_or(full)
    }

    /// `full`, for the line to go in, as it could not be moved for `error`.
    /// The first time, this process says so in it, at `now`: whoever finds
    /// the log past `MAX` finds why.
    fn unmoved(&self, mut full: File, error: &io::Error, now: SystemTime) -> File {
        if !self.unmoved.swap(true, Ordering::Relaxed) {
            let why = format!("{NAME} does not move to {NAME}.1: {error}");
            let _ = full.write_all(self.line(&why, now).as_bytes());
        }
        full
    }
}

/// The log at `path`, open to write at its end, made where it is not yet. It
/// is this user's alone: it says where your work is.
fn opened(path: &Path) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).mode(0o600).open(path)
}

/// Whether `path` names `file`. A path that names nothing does not.
fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let open = file.metadata()?;
    match fs::metadata(path) {
        Ok(named) => Ok(open.dev() == named.dev() && open.ino() == named.ino()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Where the log at `path` goes once full: beside it, by its name and `.1`.
fn before(path: &Path) -> PathBuf {
    let mut before = path.as_os_str().to_owned();
    before.push(".1");
    PathBuf::from(before)
}

/// `what` on one line: its line breaks and other control characters escaped,
/// as `\n`, so that a line is a thing that happened, and none can move the
/// cursor of a terminal that shows the log. No more than `SAID_MAX` bytes
/// of it, and the escape or character that crosses that, and `…`.
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

/// `at`, in UTC to the millisecond, as RFC 3339 has it:
/// `2026-10-29T14:03:12.481Z`. A time before 1970 is taken for 1970's first
/// moment.
fn utc(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs();
    let (year, month, day) = date(seconds / 86_400);
    let of_day = seconds % 86_400;
    let (hour, minute, second) = (of_day / 3600, of_day / 60 % 60, of_day % 60);
    let milli = since.subsec_millis();
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}Z")
}

/// The year, month and day `days` after 1970-01-01, in the Gregorian
/// calendar, as Howard Hinnant's `civil_from_days` counts them: in eras of
/// 400 years, which each have as many days, from a March 1st, so that a leap
/// day is the last of its year.
fn date(days: u64) -> (u64, u64, u64) {
    // From 0000-03-01.
    let days = days + 719_468;
    let era = days / 146_097;
    let of_era = days % 146_097;
    let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    // From March, as 0.
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

    /// A line says when, in UTC, which process by its name and pid, and what
    /// happened, after a log made with its folder.
    #[test]
    fn a_line_says_when_which_process_and_what() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join("luneta").join(NAME);
        let log = Log::open(path.clone(), Process::Luneta).unwrap();
        log.write("zellij did not take its client there", at(1_793_282_592, 481)).unwrap();
        log.write("and again", at(1_793_282_593, 7)).unwrap();
        let pid = std::process::id();
        let expected = format!(
            "2026-10-29T14:03:12.481Z luneta[{pid}] zellij did not take its client there\n\
             2026-10-29T14:03:13.007Z luneta[{pid}] and again\n"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    }

    /// The date of a line is the calendar's, across leap days and the
    /// centuries that have none, as `date -u` has each.
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
        assert_eq!(utc(UNIX_EPOCH - Duration::from_secs(1)), "1970-01-01T00:00:00.000Z");
    }

    /// What is said on many lines, or with a terminal's escapes, is one line
    /// of the log, and what is too long is cut.
    #[test]
    fn what_is_said_is_one_line_of_its_own() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        let log = Log::open(path.clone(), Process::Luneta).unwrap();
        log.write("zellij said:\nError: \u{1b}[31mno\u{1b}[0m\n", at(0, 0)).unwrap();
        log.write(&"x".repeat(SAID_MAX * 2), at(0, 0)).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), 2, "{written}");
        assert!(lines[0].ends_with(r"zellij said:\nError: \u{1b}[31mno\u{1b}[0m"), "{written}");
        assert!(lines[1].ends_with(&format!(" {}…", "x".repeat(SAID_MAX))), "{}", lines[1]);
    }

    /// A full log moves to `.1`, in the place of the one before, and the line
    /// goes in a new one.
    #[test]
    fn a_full_log_moves_to_one_in_the_place_of_the_one_before() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        let moved = state.path().join(format!("{NAME}.1"));
        fs::write(&moved, "the one before\n").unwrap();
        let full = format!("{}\n", "x".repeat(usize::try_from(MAX).unwrap()));
        fs::write(&path, &full).unwrap();
        Log::open(path.clone(), Process::Luneta).unwrap().write("past it", at(0, 0)).unwrap();
        assert_eq!(fs::read_to_string(&moved).unwrap(), full);
        let written = fs::read_to_string(&path).unwrap();
        assert!(written.ends_with(" past it\n") && written.lines().count() == 1, "{written}");
    }

    /// A full log that cannot move, as a folder has the name it would take,
    /// keeps each line past it, and says why it does not move, once.
    #[test]
    fn a_full_log_that_cannot_move_keeps_its_lines_and_says_why_once() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(NAME);
        fs::create_dir(state.path().join(format!("{NAME}.1"))).unwrap();
        let full = format!("{}\n", "x".repeat(usize::try_from(MAX).unwrap()));
        fs::write(&path, &full).unwrap();
        let log = Log::open(path.clone(), Process::Luneta).unwrap();
        log.write("past it", at(0, 0)).unwrap();
        log.write("and again", at(0, 0)).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let past: Vec<&str> = written[full.len()..].lines().collect();
        assert_eq!(past.len(), 3, "{past:?}");
        assert!(past[0].contains(&format!("] {NAME} does not move to {NAME}.1: ")), "{past:?}");
        assert!(past[1].ends_with("] past it") && past[2].ends_with("] and again"), "{past:?}");
    }

    /// Many writers that fill the log together, each with a log of its own,
    /// as many processes do, move it once, and keep each line whole, once.
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
                let log = Log::open(path.clone(), Process::Lunetad).unwrap();
                scope.spawn(move || {
                    for line in 0..LINES {
                        log.write(&format!("writer {writer} line {line}"), at(0, 0)).unwrap();
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
            .map(|line| line.split_once("] ").map_or("", |(_, said)| said).to_owned())
            .collect();
        let expected: HashSet<String> = (0..WRITERS)
            .flat_map(|writer| (0..LINES).map(move |line| format!("writer {writer} line {line}")))
            .collect();
        assert_eq!(written.len(), WRITERS * LINES);
        assert_eq!(said, expected);
    }
}
