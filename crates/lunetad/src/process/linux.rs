//! Processes on Linux, through `/proc`.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use rustix::time::{clock_gettime, ClockId};

fn proc(pid: u32, leaf: &str) -> PathBuf {
    PathBuf::from("/proc").join(pid.to_string()).join(leaf)
}

/// When `pid` started, in clock ticks after boot: field 22 of its `stat`, as
/// Claude Code records it. Counted from the last `)`, since the name before it
/// can hold spaces and parentheses.
#[must_use]
pub fn start(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(proc(pid, "stat")).ok()?;
    let close = stat.rfind(')')?;
    stat[close + 1..].split_whitespace().nth(19).map(str::to_owned)
}

/// When `pid` started, or less than a clock tick before, and never after: as
/// long before now as the time since the boot, less the ticks after the boot
/// that `start` reads, whole ones.
#[must_use]
pub fn started(pid: u32) -> Option<SystemTime> {
    let ticks: u64 = start(pid)?.parse().ok()?;
    let per_second = rustix::param::clock_ticks_per_second();
    let secs = ticks.checked_div(per_second)?;
    let nanos = ticks % per_second * 1_000_000_000 / per_second;
    let after_boot = Duration::from_secs(secs) + Duration::from_nanos(nanos);
    // Now ahead of the time since the boot, so that what goes by between the
    // two makes the start earlier, and never later.
    let now = SystemTime::now();
    let since_boot = clock_gettime(ClockId::Boottime);
    let secs = u64::try_from(since_boot.tv_sec).ok()?;
    let nanos = u32::try_from(since_boot.tv_nsec).ok()?;
    now.checked_sub(Duration::new(secs, nanos).checked_sub(after_boot)?)
}

/// The environment `pid` started with, an entry to a string.
#[must_use]
pub fn environ(pid: u32) -> Option<Vec<OsString>> {
    let raw = fs::read(proc(pid, "environ")).ok()?;
    let entries = raw.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
    Some(entries.map(|entry| OsString::from_vec(entry.to_vec())).collect())
}

/// The pids of this system, as Claude Code tells one system's from another's:
/// `linux:<machine id>:pid:[<namespace>]`, the namespace being the one the
/// daemon runs in. A container that shares the folder has a namespace of its
/// own, and another machine that shares it a machine id of its own.
pub struct Domain {
    machine: Option<String>,
    namespace: Option<String>,
}

impl Domain {
    /// The machine id is the one in `/etc/machine-id`, and in no other file:
    /// Claude Code reads that one alone, and records none without it. A host
    /// with only `/var/lib/dbus/machine-id`, as one without systemd may be,
    /// would otherwise have an id of its own that no file of its agents names.
    #[must_use]
    pub fn here() -> Self {
        let machine = fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        let namespace = fs::read_link("/proc/self/ns/pid").ok();
        Domain { machine, namespace: namespace.map(|link| link.to_string_lossy().into_owned()) }
    }

    /// Whether a pid recorded under `recorded` is one of this system's. A part
    /// of it that this system cannot tell of itself, as a sandbox with no
    /// machine id cannot, is taken to agree; one it can tell must.
    #[must_use]
    pub fn admits(&self, recorded: &str) -> bool {
        let Some(rest) = recorded.strip_prefix("linux:") else {
            return false;
        };
        let (machine, namespace) = rest.split_once(':').unwrap_or((rest, ""));
        let agrees = |ours: &Option<String>, theirs: &str| {
            ours.as_ref().is_none_or(|ours| ours == theirs)
        };
        agrees(&self.machine, machine) && agrees(&self.namespace, namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test's own process started a moment ago, and not after now.
    #[test]
    fn a_process_started_a_moment_ago() {
        let started = started(std::process::id()).unwrap();
        let ago = SystemTime::now().duration_since(started).unwrap();
        assert!(ago < Duration::from_mins(10), "{ago:?}");
    }
}
