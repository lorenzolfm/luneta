//! Processes as Linux's `/proc` says them.

use std::fs;

use super::{CHILDREN_MAX, Group, Process, Terminal};

/// The most threads of a process whose children the server reads.
const TASKS_MAX: usize = 256;

/// The process `pid`, as its `stat` and `wchan` say it, or none where it is
/// gone.
pub fn process(pid: u32) -> Option<Process> {
    let stat = fs::read(format!("/proc/{pid}/stat")).ok()?;
    let mut read = stat_of(&stat)?;
    assert_eq!(read.pid, pid, "/proc/{pid}/stat is another's");
    let sleeps = fs::read_to_string(format!("/proc/{pid}/wchan")).unwrap_or_default();
    if sleeps != "0" {
        sleeps.trim().clone_into(&mut read.sleeps);
    }
    Some(read)
}

/// A process as its `stat` says it: `<pid> (<name>) <state> <parent>
/// <group> <session> <tty> <front> …`, with a name that may hold `)` or a
/// space, so the fields go from its last `)`.
fn stat_of(stat: &[u8]) -> Option<Process> {
    let open = stat.iter().position(|byte| *byte == b'(')?;
    let close = stat.iter().rposition(|byte| *byte == b')')?;
    let pid = std::str::from_utf8(&stat[..open]).ok()?.trim().parse().ok()?;
    let name = String::from_utf8_lossy(stat.get(open + 1..close)?).into_owned();
    let rest = std::str::from_utf8(stat.get(close + 1..)?).ok()?;
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?;
    let parent = fields.next()?.parse().ok()?;
    let group = Group(fields.next()?.parse().ok()?);
    let _session = fields.next()?;
    let device: u64 = fields.next()?.parse::<i64>().ok()?.try_into().ok()?;
    let front: i64 = fields.next()?.parse().ok()?;
    let front = u32::try_from(front).ok().filter(|front| *front > 0).map(Group);
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

/// The children of `pid`, of each of its first `TASKS_MAX` threads, up to
/// `CHILDREN_MAX`: none where it is gone.
pub fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for task in tasks.flatten().take(TASKS_MAX) {
        let Ok(listed) = fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        for child in listed.split_ascii_whitespace().filter_map(|child| child.parse().ok()) {
            if children.len() == CHILDREN_MAX {
                return children;
            }
            children.push(child);
        }
    }
    children
}

/// The arguments of `pid`, as its `cmdline` holds them, each ended by a NUL:
/// none where it is gone, or a zombie, whose `cmdline` is empty.
pub fn argv(pid: u32) -> Option<Vec<String>> {
    let line = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let line = line.strip_suffix(b"\0").unwrap_or(&line);
    if line.is_empty() {
        return None;
    }
    let args = line.split(|byte| *byte == 0);
    Some(args.map(|arg| String::from_utf8_lossy(arg).into_owned()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name with `)` and spaces in it reads whole, the fields after it as
    /// they are, and a terminal of none as none.
    #[test]
    fn a_stat_reads_past_a_name_of_parentheses_and_spaces() {
        let stat = b"42 (a) b (c) T 7 9 9 34817 51 4194560 0 0";
        let read = stat_of(stat).expect("it reads");
        let read_as = (read.pid, read.name.as_str(), read.parent, read.group);
        assert_eq!(read_as, (42, "a) b (c", 7, Group(9)));
        assert!(read.stopped);
        assert_eq!(read.terminal, Some(Terminal { device: 34817, front: Some(Group(51)) }));
        let read = stat_of(b"42 (sh) S 7 9 9 0 -1 0").expect("it reads");
        assert!(!read.stopped);
        assert_eq!(read.terminal, None);
        let read = stat_of(b"42 (sh) S 7 9 9 34817 -1 0").expect("it reads");
        assert_eq!(read.terminal, Some(Terminal { device: 34817, front: None }));
        assert_eq!(stat_of(b"42 sh S 7 9"), None);
        assert_eq!(stat_of(b"42 (sh) S 7"), None);
    }
}
