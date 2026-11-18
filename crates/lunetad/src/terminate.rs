use std::fs;
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::io::Errno;
use rustix::process::{kill_process, Pid, Signal};

use crate::{folder, process};

pub const LUNETA: &str = "luneta";

pub const LUNETAD: &str = "lunetad";

pub const MUX: &str = "luneta-mux";

pub const GRACE: Duration = Duration::from_secs(5);

pub const KILLED: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Luneta {
    pub pid: u32,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Running {
    pub lunetas: Vec<Luneta>,
    pub lunetads: Vec<u32>,
    pub muxes: Vec<u32>,
}

impl Running {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lunetas.is_empty() && self.lunetads.is_empty() && self.muxes.is_empty()
    }

    #[must_use]
    pub fn luneta_pids(&self) -> Vec<u32> {
        self.lunetas.iter().map(|luneta| luneta.pid).collect()
    }
}

#[must_use]
pub fn running(folder: &Path) -> Running {
    running_of(process::mine(&[LUNETA, LUNETAD, MUX]), folder, std::process::id())
}

fn running_of(found: Vec<process::Found>, folder: &Path, me: u32) -> Running {
    let mut running = Running::default();
    for found in found {
        let theirs = folder::find_in(&found.environ);
        if found.pid == me || !theirs.is_some_and(|theirs| same(&theirs, folder)) {
            continue;
        }
        match found.name.as_str() {
            LUNETA => running.lunetas.push(Luneta { pid: found.pid, argv: found.argv }),
            LUNETAD => running.lunetads.push(found.pid),
            MUX => running.muxes.push(found.pid),
            _ => {},
        }
    }
    running.lunetas.sort_by_key(|luneta| luneta.pid);
    running.lunetads.sort_unstable();
    running.muxes.sort_unstable();
    running
}

fn same(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).is_ok_and(|a| fs::canonicalize(b).is_ok_and(|b| a == b))
}

#[must_use]
pub fn ended(pids: &[u32]) -> Vec<u32> {
    let left = signalled(pids, Signal::TERM, GRACE);
    if left.is_empty() {
        return left;
    }
    signalled(&left, Signal::KILL, KILLED)
}

fn signalled(pids: &[u32], signal: Signal, wait: Duration) -> Vec<u32> {
    let (ends, heard) = mpsc::channel();
    let (mut left, mut waiting) = (Vec::new(), Vec::new());
    for &pid in pids {
        let Some(id) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            continue;
        };
        match kill_process(id, signal) {
            Ok(()) => {},
            Err(Errno::SRCH) => continue,
            Err(_) => {
                left.push(pid);
                continue;
            },
        }
        let ends = ends.clone();
        let watching = thread::Builder::new().name(format!("ends {pid}")).spawn(move || {
            if process::ends(pid).is_ok() {
                let _ = ends.send(pid);
            }
        });
        if watching.is_ok() {
            waiting.push(pid);
        } else {
            left.push(pid);
        }
    }
    drop(ends);
    let deadline = Instant::now() + wait;
    while !waiting.is_empty() {
        let Some(until) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        match heard.recv_timeout(until) {
            Ok(pid) => waiting.retain(|waits| *waits != pid),
            Err(_) => break,
        }
    }
    left.extend(waiting);
    left.sort_unstable();
    left
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{BufRead as _, BufReader};
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_process_ends_at_its_term_and_one_that_holds_out_at_its_kill() {
        let mut ends = Command::new("sleep").arg("60").spawn().unwrap();
        let script = "trap '' TERM; echo held; while :; do sleep 0.05; done";
        let mut holds = Command::new("sh");
        let mut holds = holds.args(["-c", script]).stdout(Stdio::piped()).spawn().unwrap();
        let mut held = String::new();
        BufReader::new(holds.stdout.take().unwrap()).read_line(&mut held).unwrap();
        assert_eq!(held, "held\n");
        let started = Instant::now();
        assert_eq!(ended(&[ends.id(), holds.id()]), Vec::<u32>::new());
        assert!(started.elapsed() >= GRACE, "the one that holds out has its grace");
        assert!(ends.wait().unwrap().code().is_none());
        assert!(holds.wait().unwrap().code().is_none());
        assert_eq!(ended(&[ends.id()]), Vec::<u32>::new(), "one that has ended is left alone");
    }

    fn found(pid: u32, name: &str, environ: &[&str]) -> process::Found {
        let argv = vec![name.to_owned()];
        let environ = environ.iter().map(OsString::from).collect();
        process::Found { pid, name: name.to_owned(), argv, environ }
    }

    #[test]
    fn a_process_is_luneta_s_only_in_the_folder_its_variables_name() {
        let folder = Path::new("/run/user/1000/luneta");
        let listed = vec![
            found(1, LUNETA, &["XDG_RUNTIME_DIR=/run/user/1000", "TMPDIR=/tmp"]),
            found(2, LUNETAD, &["HOME=/home/me", "TMPDIR=/run/user/1000"]),
            found(3, MUX, &["XDG_RUNTIME_DIR=", "TMPDIR=/run/user/1000"]),
            found(4, LUNETAD, &["XDG_RUNTIME_DIR=/tmp/test", "TMPDIR=/run/user/1000"]),
            found(5, MUX, &["TMPDIR=/tmp"]),
            found(6, LUNETA, &[]),
            found(7, LUNETA, &["XDG_RUNTIME_DIR=/run/user/1000"]),
            found(8, LUNETAD, &["XDG_RUNTIME_DIR=/run/user/1000", "XDG_RUNTIME_DIR=/tmp"]),
        ];
        let running = running_of(listed, folder, 7);
        let lunetas: Vec<u32> = running.luneta_pids();
        assert_eq!((lunetas, running.lunetads, running.muxes), (vec![1], vec![2, 8], vec![3]));
    }
}
