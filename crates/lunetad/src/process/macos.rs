//! Processes on macOS, through sysinfo, which asks the kernel with `sysctl`;
//! their command lines, which sysinfo reads wrong, through `sysctl` asked
//! here; and their ends, through kqueue.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::ptr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustix::event::kqueue::{kevent, kqueue, Event, EventFilter, EventFlags, ProcessEvents};
use rustix::io::Errno;
use rustix::process::Pid;
use sysinfo::{Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

fn with<T>(pid: u32, refresh: ProcessRefreshKind, read: impl FnOnce(&Process) -> T) -> Option<T> {
    let mut system = System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, refresh);
    system.process(pid).map(read)
}

/// When `pid` started, as Claude Code records it on macOS: the date `ps -o
/// lstart` prints, in UTC and the C locale.
#[must_use]
pub fn start(pid: u32) -> Option<String> {
    let secs = with(pid, ProcessRefreshKind::nothing(), Process::start_time)?;
    Some(lstart(i64::try_from(secs).ok()?))
}

/// When `pid` started, or less than a second after, and never before: the
/// whole seconds the kernel says, and the one it started in.
#[must_use]
pub fn started(pid: u32) -> Option<SystemTime> {
    let secs = with(pid, ProcessRefreshKind::nothing(), Process::start_time)?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs.checked_add(1)?))
}

/// The environment `pid` started with, an entry to a string.
#[must_use]
pub fn environ(pid: u32) -> Option<Vec<OsString>> {
    let refresh = ProcessRefreshKind::nothing().with_environ(UpdateKind::Always);
    with(pid, refresh, |process| process.environ().to_vec())
}

/// The command line `pid` runs, an argument to a string, an empty one too:
/// sysinfo 0.36 skips one, and reads an entry of the environment in the
/// place of the last. None for one that ended, or whose arguments the kernel
/// shows none of.
#[must_use]
pub fn args(pid: u32) -> Option<Vec<OsString>> {
    laid_out(&procargs(pid)?)
}

/// What the kernel lays out of `pid`'s start, as `KERN_PROCARGS2` asks: how
/// many arguments it has, the path it was run by, and then its arguments and
/// its environment, each ended by a 0.
#[expect(
    unsafe_code,
    reason = "libc's sysctl writes no more into a buffer than the length it is given of it"
)]
fn procargs(pid: u32) -> Option<Vec<u8>> {
    let mut asked = [libc::CTL_KERN, libc::KERN_PROCARGS2, libc::c_int::try_from(pid).ok()?];
    let named = asked.len() as libc::c_uint;
    let mut size: libc::size_t = 0;
    // SAFETY: with no buffer, it says only how long one must be.
    let sized = unsafe {
        libc::sysctl(asked.as_mut_ptr(), named, ptr::null_mut(), &raw mut size, ptr::null_mut(), 0)
    };
    if sized != 0 {
        return None;
    }
    let mut laid = vec![0_u8; size];
    let buffer = laid.as_mut_ptr().cast();
    // SAFETY: `laid` is `size` long, and sysctl writes no more, and says how
    // much it wrote.
    let read = unsafe {
        libc::sysctl(asked.as_mut_ptr(), named, buffer, &raw mut size, ptr::null_mut(), 0)
    };
    if read != 0 {
        return None;
    }
    laid.truncate(size);
    Some(laid)
}

/// The arguments in `laid`, as `procargs` says it: as many as it counts,
/// after the path and the 0s that pad it, so that an empty one, but for the
/// first, reads as one. None where it lays out fewer, or counts none.
fn laid_out(laid: &[u8]) -> Option<Vec<OsString>> {
    let (count, rest) = laid.split_first_chunk::<4>()?;
    let count = usize::try_from(i32::from_ne_bytes(*count)).ok().filter(|count| *count > 0)?;
    let path = rest.iter().position(|byte| *byte == 0)?;
    let rest = &rest[path..];
    let first = rest.iter().position(|byte| *byte != 0)?;
    let each = rest[first..].split(|byte| *byte == 0).take(count);
    let args: Vec<OsString> = each.map(|arg| OsString::from_vec(arg.to_vec())).collect();
    Some(args).filter(|args| args.len() == count)
}

/// Waits for `pid` to end, as kqueue says. A process that has ended already, or
/// is gone, kqueue finds none of, as the event it could not add says, in place
/// of the event it waits for.
#[expect(
    unsafe_code,
    reason = "rustix's kevent is unsafe for the files an event may name, and this one names none"
)]
pub fn ends(pid: Pid) -> io::Result<()> {
    let queue = kqueue()?;
    let flags = ProcessEvents::EXIT;
    let end = Event::new(EventFilter::Proc { pid, flags }, EventFlags::ADD, ptr::null_mut());
    let mut adding: &[Event] = &[end];
    let mut heard = [end];
    loop {
        // SAFETY: it asks that the files the events name stay open as long as
        // the queue, and this event names a process.
        let failure = match unsafe { kevent(&queue, adding, &mut heard[..], None) } {
            Ok(_) if !heard[0].flags().contains(EventFlags::ERROR) => return Ok(()),
            Ok(_) => i32::try_from(heard[0].data()).map_or(Errno::INVAL, Errno::from_raw_os_error),
            // A call a signal cuts short has added what it was given.
            Err(Errno::INTR) => {
                adding = &[];
                continue;
            },
            Err(failure) => failure,
        };
        return if failure == Errno::SRCH { Ok(()) } else { Err(failure.into()) };
    }
}

/// The pids of this system. macOS has one pid namespace, and Claude Code calls
/// it `darwin`.
pub struct Domain;

impl Domain {
    #[must_use]
    pub fn here() -> Self {
        Domain
    }

    /// Whether a pid recorded under `recorded` is one of this system's.
    #[must_use]
    #[expect(
        clippy::unused_self,
        reason = "one signature for both backends, and Linux's reads itself"
    )]
    pub fn admits(&self, recorded: &str) -> bool {
        recorded == "darwin"
    }
}

/// `secs` after the epoch, as ctime spells it: `Sun Sep 13 11:49:45 2026`.
fn lstart(secs: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

    let days = secs.div_euclid(86_400);
    let clock = secs.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    assert!((1..=12).contains(&month));
    assert!((1..=31).contains(&day));
    let weekday = usize::try_from(days.rem_euclid(7)).expect("a remainder of 7 is in 0..7");
    let month_index = usize::try_from(month - 1).expect("a month is in 1..=12");
    format!(
        "{} {} {day:>2} {:02}:{:02}:{:02} {year}",
        WEEKDAYS[weekday],
        MONTHS[month_index],
        clock / 3600,
        clock % 3600 / 60,
        clock % 60,
    )
}

/// The year, month and day of `days` after the epoch, by Howard Hinnant's
/// `civil_from_days`.
fn civil(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}
