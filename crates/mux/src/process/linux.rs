use std::fs::{self, File};
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;

use lune_protocol::process::arguments;

use super::{ARGV_BYTES_MAX, CHILDREN_MAX, Group, OwnerInfo, Process, ProcessId, Terminal};

const TASKS_MAX: usize = 256;

pub fn process(pid: ProcessId) -> Option<Process> {
    let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let mut read = stat_of(&stat)?;
    assert_eq!(read.pid, pid, "/proc/{pid}/stat is another's");
    let sleeps = fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    if sleeps != "0" {
        sleeps.trim().clone_into(&mut read.sleeps);
    }
    Some(read)
}

fn stat_of(stat: &[u8]) -> Option<Process> {
    let open = stat.iter().position(|byte| *byte == b'(')?;
    let close = stat.iter().rposition(|byte| *byte == b')')?;
    let pid = ProcessId::new(
        std::str::from_utf8(&stat[..open])
            .ok()?
            .trim()
            .parse()
            .ok()?,
    )?;
    let name = String::from_utf8_lossy(stat.get(open + 1..close)?).into_owned();
    let rest = std::str::from_utf8(stat.get(close + 1..)?).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let parent: u32 = fields.next()?.parse().ok()?;
    let parent = ProcessId::new(parent);
    let group = Group::new(fields.next()?.parse().ok()?)?;
    let _session = fields.next()?;
    let device: u64 = fields.next()?.parse::<i64>().ok()?.try_into().ok()?;
    let front: i64 = fields.next()?.parse().ok()?;
    let front = u32::try_from(front).ok().and_then(Group::new);
    let terminal = (device != 0).then_some(Terminal { device, front });
    Some(Process {
        pid,
        parent,
        group,
        name,
        sleeps: String::new(),
        stopped: state == "T",
        terminal,
    })
}

pub fn children(pid: ProcessId) -> Vec<ProcessId> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for task in tasks.flatten().take(TASKS_MAX) {
        let Ok(listed) = fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for child in listed
            .split_ascii_whitespace()
            .filter_map(|child| child.parse().ok().and_then(ProcessId::new))
        {
            if children.len() == CHILDREN_MAX {
                return children;
            }
            children.push(child);
        }
    }
    children
}

pub fn argv(pid: ProcessId) -> Option<Vec<String>> {
    let cmdline = File::open(format!("/proc/{pid}/cmdline")).ok()?;
    let mut line = Vec::with_capacity(ARGV_BYTES_MAX);
    let most = u64::try_from(ARGV_BYTES_MAX).expect("16 KiB fit a u64");
    cmdline.take(most).read_to_end(&mut line).ok()?;
    let args = arguments(&line)?.into_iter();
    Some(
        args.map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect(),
    )
}

pub fn owner_info(pid: u32) -> Option<OwnerInfo> {
    let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let parsed = stat_detail_of(&stat)?;
    assert_eq!(parsed.pid, pid, "/proc/{pid}/stat is another's");
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let (uid, euid) = uids_in_status(&status)?;
    let exe = fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .map(|path| path.strip_suffix(" (deleted)").unwrap_or(&path).to_owned());
    let cwd = fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok());
    Some(OwnerInfo {
        pid: parsed.pid,
        parent: parsed.parent,
        uid,
        euid,
        proc_start: parsed.proc_start,
        name: parsed.name,
        exe,
        cwd,
        terminal_device: parsed.terminal_device,
    })
}

pub fn same_root(pid: u32, root_pid: u32) -> bool {
    if pid == root_pid {
        return true;
    }
    let Ok(a) = fs::metadata(format!("/proc/{pid}/root")) else {
        return false;
    };
    let Ok(b) = fs::metadata(format!("/proc/{root_pid}/root")) else {
        return false;
    };
    a.dev() == b.dev() && a.ino() == b.ino()
}

struct StatDetail {
    pid: u32,
    parent: u32,
    name: String,
    proc_start: String,
    terminal_device: Option<u64>,
}

fn stat_detail_of(stat: &[u8]) -> Option<StatDetail> {
    let open = stat.iter().position(|byte| *byte == b'(')?;
    let close = stat.iter().rposition(|byte| *byte == b')')?;
    let pid = std::str::from_utf8(&stat[..open])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let name = String::from_utf8_lossy(stat.get(open + 1..close)?).into_owned();
    let rest = std::str::from_utf8(stat.get(close + 1..)?).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    let _group = fields.next()?;
    let _session = fields.next()?;
    let device: u64 = fields.next()?.parse::<i64>().ok()?.try_into().ok()?;
    let _front = fields.next()?;
    let threads = fields.nth(11)?;
    if matches!(state, "X" | "x") || (state == "Z" && threads == "1") {
        return None;
    }
    let proc_start = fields.nth(1)?.to_owned();
    if proc_start.is_empty() {
        return None;
    }
    let terminal_device = (device != 0).then_some(device);
    Some(StatDetail {
        pid,
        parent,
        name,
        proc_start,
        terminal_device,
    })
}

fn uids_in_status(status: &str) -> Option<(u32, u32)> {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            let mut fields = rest.split_ascii_whitespace();
            let uid = fields.next()?.parse::<u32>().ok()?;
            let euid = fields.next()?.parse::<u32>().ok()?;
            return Some((uid, euid));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stat_reads_past_a_name_of_parentheses_and_spaces() {
        let stat = b"42 (a) b (c) T 7 9 9 34817 51 4194560 0 0";
        let read = stat_of(stat).expect("it reads");
        let read_as = (read.pid, read.name.as_str(), read.parent, read.group);
        assert_eq!(
            read_as,
            (
                ProcessId::new(42).unwrap(),
                "a) b (c",
                ProcessId::new(7),
                Group::new(9).unwrap()
            )
        );
        assert!(read.stopped);
        assert_eq!(
            read.terminal,
            Some(Terminal {
                device: 34817,
                front: Group::new(51)
            })
        );
        let read = stat_of(b"42 (sh) S 0 9 9 0 -1 0").expect("it reads");
        assert!(!read.stopped);
        assert_eq!(read.parent, None);
        assert_eq!(read.terminal, None);
        let read = stat_of(b"42 (sh) S 7 9 9 34817 -1 0").expect("it reads");
        assert_eq!(
            read.terminal,
            Some(Terminal {
                device: 34817,
                front: None
            })
        );
        assert_eq!(stat_of(b"0 (sh) S 7 9 9 0 -1 0"), None);
        assert_eq!(stat_of(b"42 (sh) S 7 0 9 0 -1 0"), None);
        assert_eq!(stat_of(b"42 sh S 7 9"), None);
        assert_eq!(stat_of(b"42 (sh) S 7"), None);
    }

    #[test]
    fn owner_info_reads_this_process_and_rejects_single_thread_zombies() {
        let me = std::process::id();
        let info = owner_info(me).expect("this process has owner info");
        assert_eq!(info.pid, me);
        assert_eq!(info.uid, rustix::process::getuid().as_raw());
        assert_eq!(info.euid, rustix::process::geteuid().as_raw());
        assert_ne!(info.proc_start, "");
        assert!(same_root(me, me));

        let alive = b"7 (a) b) S 1 7 7 34817 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 6630522 16";
        let detail = stat_detail_of(alive).expect("alive stat reads");
        assert_eq!(detail.proc_start, "6630522");
        assert_eq!(detail.terminal_device, Some(34817));

        let zombie = b"7 (a) b) Z 1 7 7 34817 -1 4194304 0 0 0 0 0 0 0 0 20 0 1 0 6630522 16";
        assert!(stat_detail_of(zombie).is_none());
        assert_eq!(
            uids_in_status("Name:\tsh\nUid:\t1000\t1000\t1000\t1000\n"),
            Some((1000, 1000))
        );
    }
}
