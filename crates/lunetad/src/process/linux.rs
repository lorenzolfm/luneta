//! Processes on Linux, through `/proc`, as claude-ps reads them.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

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
    #[must_use]
    pub fn here() -> Self {
        let machine = ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .into_iter()
            .find_map(|path| fs::read_to_string(path).ok())
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
