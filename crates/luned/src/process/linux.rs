use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::str::SplitWhitespace;
use std::time::{Duration, Instant, SystemTime};

use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::io::{retry_on_intr, Errno};
use rustix::process::{pidfd_open, pidfd_send_signal, Pid, PidfdFlags, Signal};
use rustix::time::{clock_gettime, ClockId};

use lune_protocol::process::{arguments, entries, EXEC_STRINGS_BYTES_MAX};

use super::{program_named, Found, Id, Mine, Start, Unended, Unread, Unreadable};

fn proc(pid: u32, leaf: &str) -> PathBuf {
    PathBuf::from("/proc").join(pid.to_string()).join(leaf)
}

#[must_use]
pub fn start(pid: u32) -> Option<Start> {
    start_in(&fs::read_to_string(proc(pid, "stat")).ok()?).and_then(Start::spelled)
}

const PF_EXITING: u64 = 0x4;

fn exiting(pid: u32) -> bool {
    fs::read_to_string(proc(pid, "stat")).map_or(true, |stat| exiting_in(&stat))
}

fn after_name(stat: &str) -> Option<SplitWhitespace<'_>> {
    let close = stat.rfind(')')?;
    Some(stat[close + 1..].split_whitespace())
}

fn exiting_in(stat: &str) -> bool {
    let Some(mut fields) = after_name(stat) else {
        return false;
    };
    let ended = fields.next().is_some_and(|state| matches!(state, "Z" | "X"));
    let flags = fields.nth(5).and_then(|flags| flags.parse::<u64>().ok());
    ended || flags.is_some_and(|flags| flags & PF_EXITING != 0)
}

const PF_FORKNOEXEC: u64 = 0x40;

fn forked_in(stat: &str) -> Option<bool> {
    let flags = after_name(stat)?.nth(6)?.parse::<u64>().ok()?;
    Some(flags & PF_FORKNOEXEC != 0)
}

fn start_in(stat: &str) -> Option<String> {
    let mut fields = after_name(stat)?;
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

pub fn environ(pid: u32) -> Result<Vec<OsString>, Unread> {
    Ok(entries(&exec_strings(pid, "environ")?))
}

pub fn cwd(pid: u32) -> Result<PathBuf, Unread> {
    fs::read_link(proc(pid, "cwd")).map_err(|error| unread(pid, &error))
}

fn exec_strings(pid: u32, leaf: &str) -> Result<Vec<u8>, Unread> {
    let most = u64::try_from(EXEC_STRINGS_BYTES_MAX).expect("6 MiB fit a u64");
    let mut read = Vec::new();
    let file = File::open(proc(pid, leaf)).map_err(|error| unread(pid, &error))?;
    file.take(most + 1).read_to_end(&mut read).map_err(|error| unread(pid, &error))?;
    if read.len() > EXEC_STRINGS_BYTES_MAX {
        let most = EXEC_STRINGS_BYTES_MAX;
        let why = format!("/proc/{pid}/{leaf} runs past the {most} bytes exec lays out");
        return Err(Unread::Failed(why));
    }
    Ok(read)
}

fn unread(pid: u32, error: &io::Error) -> Unread {
    let unfound = error.kind() == io::ErrorKind::NotFound;
    if unfound || Errno::from_io_error(error) == Some(Errno::SRCH) || exiting(pid) {
        Unread::Gone
    } else {
        Unread::Failed(error.to_string())
    }
}

pub(super) fn named(pid: u32) -> Option<String> {
    let name = fs::read_to_string(proc(pid, "comm")).ok()?;
    Some(name.trim_end_matches('\n').to_owned())
}

#[must_use]
pub(super) fn listed<P: Copy>(programs: &[(P, &str)]) -> Mine<P> {
    let Ok(listed) = fs::read_dir("/proc") else {
        return Mine::default();
    };
    let uid = rustix::process::getuid().as_raw();
    let mut seen = Mine::default();
    for entry in listed.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };
        if !entry.metadata().is_ok_and(|it| it.uid() == uid) {
            continue;
        }
        let Some(program) = named(pid).and_then(|name| program_named(programs, &name)) else {
            continue;
        };
        let Ok(stat_line) = fs::read_to_string(proc(pid, "stat")) else {
            continue;
        };
        let Some(false) = forked_in(&stat_line) else {
            continue;
        };
        let Some(start) = start_in(&stat_line).and_then(Start::spelled) else {
            continue;
        };
        let id = Id { pid, start };
        let read = environ(pid).and_then(|environ| Ok((environ, exec_strings(pid, "cmdline")?)));
        let (environ, cmdline) = match read {
            Ok(read) => read,
            Err(Unread::Gone) => continue,
            Err(Unread::Failed(why)) => {
                seen.unreadable.push(Unreadable { id, program, why });
                continue;
            },
        };
        let Some(argv) = arguments(&cmdline) else {
            continue;
        };
        let argv = argv.into_iter().map(|arg| OsString::from_vec(arg.to_vec())).collect();
        seen.found.push(Found { id, program, argv, environ });
    }
    seen
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
    use std::io;

    use rustix::io::Errno;

    use super::{exiting_in, forked_in, start_in, unread, Unread};

    fn stat(state: &str, threads: u32) -> String {
        let before = "1 7 7 0 -1 4194304 0 0 0 0 0 0 0 0 20 0";
        format!("7 (a) b) {state} {before} {threads} 0 6630522 16 2 18446744073709551615")
    }

    #[test]
    fn a_process_that_ended_reads_as_gone_and_one_that_did_not_read_as_failed() {
        let read = super::exec_strings(u32::MAX, "cmdline").map(|_| ());
        assert_eq!(read, Err(Unread::Gone), "no process has the last pid");
        let me = std::process::id();
        let ended = unread(me, &io::Error::from(Errno::SRCH));
        assert_eq!(ended, Unread::Gone, "one that ended as it was read");
        let refused = unread(me, &io::Error::from(Errno::ACCESS));
        assert_eq!(refused, Unread::Failed(io::Error::from(Errno::ACCESS).to_string()));
        let ending = unread(u32::MAX, &io::Error::from(Errno::ACCESS));
        assert_eq!(ending, Unread::Gone, "an ending process refuses its environment");
    }

    #[test]
    fn a_process_is_exiting_from_its_flags_or_as_a_zombie() {
        let flagged = |state: &str, flags: u64| {
            format!("7 (a) b) {state} 1 7 7 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0 1 0 6630522")
        };
        assert!(!exiting_in(&flagged("S", 0x40_0000)), "PF_RANDOMIZE alone");
        assert!(exiting_in(&flagged("D", 0x40_0004)), "PF_EXITING");
        assert!(exiting_in(&flagged("Z", 0)));
        assert!(exiting_in(&flagged("X", 0)));
        assert!(!exiting_in("7 (a"), "no state to read");
    }

    #[test]
    fn a_process_has_a_start_until_all_of_it_has_ended() {
        let start = Some("6630522".to_owned());
        assert_eq!(start_in(&stat("S", 1)), start);
        assert_eq!(start_in(&stat("Z", 2)), start);
        assert_eq!(start_in(&stat("Z", 1)), None);
    }

    #[test]
    fn a_process_is_forked_until_it_execs_by_its_flags() {
        let flagged = |flags: u64| format!("7 (a) b) S 1 7 7 0 -1 {flags} 0 0 0 0 0 0 0 0 20 0");
        assert_eq!(forked_in(&flagged(0x40_0040)), Some(true), "PF_FORKNOEXEC");
        assert_eq!(forked_in(&flagged(0x40_0000)), Some(false), "PF_RANDOMIZE alone");
        assert_eq!(forked_in(&stat("S", 1)), Some(false), "past a name holding ') '");
        assert_eq!(forked_in("7 (a) b) S 1 7 7 0 -1"), None, "no flags to read");
    }
}
