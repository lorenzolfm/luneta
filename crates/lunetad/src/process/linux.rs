use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::io::{retry_on_intr, Errno};
use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
use rustix::time::{clock_gettime, ClockId};

use super::{Found, Id, Start, Unended};

fn proc(pid: u32, leaf: &str) -> PathBuf {
    PathBuf::from("/proc").join(pid.to_string()).join(leaf)
}

#[must_use]
pub fn start(pid: u32) -> Option<Start> {
    start_in(&fs::read_to_string(proc(pid, "stat")).ok()?).and_then(Start::spelled)
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
    let ticks = start(pid)?.as_str().parse::<u64>().ok()?.checked_add(1)?;
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

pub fn environ(pid: u32) -> io::Result<Vec<OsString>> {
    Ok(nul_split(&fs::read(proc(pid, "environ"))?))
}

fn nul_split(raw: &[u8]) -> Vec<OsString> {
    let entries = raw.split(|byte| *byte == 0).filter(|entry| !entry.is_empty());
    entries.map(|entry| OsString::from_vec(entry.to_vec())).collect()
}

#[must_use]
pub fn mine(names: &[&str]) -> Vec<Found> {
    let Ok(listed) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    let uid = rustix::process::getuid().as_raw();
    let mut found = Vec::new();
    for entry in listed.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if !entry.metadata().is_ok_and(|it| it.uid() == uid) {
            continue;
        }
        let Ok(name) = fs::read_to_string(proc(pid, "comm")) else {
            continue;
        };
        let name = name.trim_end_matches('\n');
        if !names.contains(&name) {
            continue;
        }
        let Some(start) = start(pid) else {
            continue;
        };
        let Ok(environ) = environ(pid) else {
            continue;
        };
        let Ok(cmdline) = fs::read(proc(pid, "cmdline")) else {
            continue;
        };
        let argv = nul_split(&cmdline).into_iter().map(|arg| arg.to_string_lossy().into_owned());
        let argv = argv.collect();
        let id = Id { pid, start };
        found.push(Found { id, name: name.to_owned(), argv, environ });
    }
    found
}

#[must_use]
pub fn signalled(ids: &[Id], signal: Signal, wait: Duration) -> Vec<(Id, Unended)> {
    let mut left = Vec::new();
    let mut waiting: Vec<(&Id, OwnedFd)> = Vec::new();
    for id in ids {
        let Some(pid) = i32::try_from(id.pid).ok().and_then(Pid::from_raw) else {
            left.push((id.clone(), Unended::Unsignalled(Errno::INVAL)));
            continue;
        };
        let held = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(held) => held,
            Err(Errno::SRCH) => continue,
            Err(error) => {
                left.push((id.clone(), Unended::Unsignalled(error)));
                continue;
            },
        };
        if start(id.pid).as_ref() != Some(&id.start) {
            continue;
        }
        match pidfd_send_signal(&held, signal) {
            Ok(()) => waiting.push((id, held)),
            Err(Errno::SRCH) => {},
            Err(error) => left.push((id.clone(), Unended::Unsignalled(error))),
        }
    }
    let deadline = Instant::now() + wait;
    while !waiting.is_empty() {
        let Some(until) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        let until = Timespec::try_from(until).expect("a wait of seconds is a timespec");
        let mut ready: Vec<PollFd<'_>> =
            waiting.iter().map(|(_, held)| PollFd::new(held, PollFlags::IN)).collect();
        match poll(&mut ready, Some(&until)) {
            Ok(0) => break,
            Ok(_) => {},
            Err(Errno::INTR) => continue,
            Err(error) => {
                drop(ready);
                let unwatched = Unended::Unwatched(error);
                left.extend(waiting.drain(..).map(|(id, _)| (id.clone(), unwatched)));
                break;
            },
        }
        let ended: Vec<bool> = ready.iter().map(|ready| !ready.revents().is_empty()).collect();
        drop(ready);
        let mut ended = ended.into_iter();
        waiting.retain(|_| !ended.next().unwrap_or(false));
    }
    left.extend(waiting.into_iter().map(|(id, _)| (id.clone(), Unended::Outlived)));
    left
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
