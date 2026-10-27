//! Processes on macOS, through sysinfo, which asks the kernel with `sysctl`,
//! and without code of ours that is unsafe.

use std::ffi::OsString;

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

fn with<T>(pid: u32, refresh: ProcessRefreshKind, read: impl FnOnce(&Process) -> T) -> Option<T> {
    let mut system = System::new();
    let pid = Pid::from_u32(pid);
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

/// The environment `pid` started with, an entry to a string.
#[must_use]
pub fn environ(pid: u32) -> Option<Vec<OsString>> {
    let refresh = ProcessRefreshKind::nothing().with_environ(UpdateKind::Always);
    with(pid, refresh, |process| process.environ().to_vec())
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
