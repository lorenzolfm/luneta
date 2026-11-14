use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use rustix::event::{poll, PollFd, PollFlags};
use rustix::io::{retry_on_intr, Errno};
use rustix::process::{pidfd_open, Pid, PidfdFlags};
use rustix::time::{clock_gettime, ClockId};

fn proc(pid: u32, leaf: &str) -> PathBuf {
    PathBuf::from("/proc").join(pid.to_string()).join(leaf)
}

#[must_use]
pub fn start(pid: u32) -> Option<String> {
    start_in(&fs::read_to_string(proc(pid, "stat")).ok()?)
}

fn start_in(stat: &str) -> Option<String> {
    let close = stat.rfind(')')?;
    let mut fields = stat[close + 1..].split_whitespace();
    let state = fields.next()?;
    let threads = fields.nth(16)?;
    if state == "Z" && threads == "1" {
        return None;
    }
    fields.nth(1).map(str::to_owned)
}

pub fn ends(pid: Pid) -> io::Result<()> {
    let process = match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(process) => process,
        Err(Errno::SRCH) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut ready = [PollFd::new(&process, PollFlags::IN)];
    retry_on_intr(|| poll(&mut ready, None))?;
    Ok(())
}

#[must_use]
pub fn started(pid: u32) -> Option<SystemTime> {
    let ticks = start(pid)?.parse::<u64>().ok()?.checked_add(1)?;
    let per_second = rustix::param::clock_ticks_per_second();
    let secs = ticks.checked_div(per_second)?;
    let nanos = (ticks % per_second * 1_000_000_000).div_ceil(per_second);
    let after_boot = Duration::from_secs(secs) + Duration::from_nanos(nanos);
    let since_boot = clock_gettime(ClockId::Boottime);
    let now = SystemTime::now();
    let secs = u64::try_from(since_boot.tv_sec).ok()?;
    let nanos = u32::try_from(since_boot.tv_nsec).ok()?;
    now.checked_sub(Duration::new(secs, nanos).saturating_sub(after_boot))
}

#[must_use]
pub fn environ(pid: u32) -> Option<Vec<OsString>> {
    let raw = fs::read(proc(pid, "environ")).ok()?;
    let entries = raw.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
    Some(entries.map(|entry| OsString::from_vec(entry.to_vec())).collect())
}

pub struct Domain {
    machine: Option<String>,
    namespace: Option<String>,
}

impl Domain {
    #[must_use]
    pub fn here() -> Self {
        let machine = fs::read_to_string("/etc/machine-id")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        let namespace = fs::read_link("/proc/self/ns/pid").ok();
        Domain { machine, namespace: namespace.map(|link| link.to_string_lossy().into_owned()) }
    }

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
    use super::start_in;

    fn stat(state: &str, threads: u32) -> String {
        let before = "1 7 7 0 -1 4194304 0 0 0 0 0 0 0 0 20 0";
        format!("7 (a) b) {state} {before} {threads} 0 6630522 16 2 18446744073709551615")
    }

    #[test]
    fn a_process_has_a_start_until_all_of_it_has_ended() {
        let start = Some("6630522".to_owned());
        assert_eq!(start_in(&stat("S", 1)), start);
        assert_eq!(start_in(&stat("Z", 2)), start);
        assert_eq!(start_in(&stat("Z", 1)), None);
    }
}
