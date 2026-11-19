use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::process::Signal;

use crate::{folder, process};

pub use crate::process::{Id, Unended};

pub const LUNETA: &str = "luneta";

pub const LUNETAD: &str = "lunetad";

pub const MUX: &str = "luneta-mux";

pub const GRACE: Duration = Duration::from_secs(5);

pub const KILL_WAIT: Duration = Duration::from_secs(2);

const _: () = assert!(!GRACE.is_zero(), "a process has a while to end at its SIGTERM");
const _: () = assert!(!KILL_WAIT.is_zero(), "a process has a while to end at its SIGKILL");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Luneta {
    pub id: Id,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Running {
    pub lunetas: Vec<Luneta>,
    pub lunetads: Vec<Id>,
    pub muxes: Vec<Id>,
}

impl Running {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lunetas.is_empty() && self.lunetads.is_empty() && self.muxes.is_empty()
    }

    #[must_use]
    pub fn luneta_ids(&self) -> Vec<Id> {
        self.lunetas.iter().map(|luneta| luneta.id.clone()).collect()
    }
}

#[must_use]
pub fn running(folder: &Path) -> Running {
    running_of(process::mine(&[LUNETA, LUNETAD, MUX]), folder, std::process::id())
}

fn running_of(found: Vec<process::Found>, folder: &Path, me: u32) -> Running {
    let mut running = Running::default();
    for found in found {
        if found.id.pid == me {
            continue;
        }
        let Some(theirs) = listened_in(&found) else {
            continue;
        };
        if same(&theirs, folder) {
            match found.name.as_str() {
                LUNETA => running.lunetas.push(Luneta { id: found.id, argv: found.argv }),
                LUNETAD => running.lunetads.push(found.id),
                MUX => running.muxes.push(found.id),
                _ => {},
            }
        }
    }
    running.lunetas.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    running.lunetads.sort_unstable();
    running.muxes.sort_unstable();
    running
}

fn listened_in(found: &process::Found) -> Option<PathBuf> {
    if found.name == MUX
        && let [_, folder] = found.argv.as_slice()
        && Path::new(folder).is_absolute()
    {
        return Some(PathBuf::from(folder));
    }
    folder::find_in(&found.environ).ok()
}

fn same(a: &Path, b: &Path) -> bool {
    a == b || fs::canonicalize(a).is_ok_and(|a| fs::canonicalize(b).is_ok_and(|b| a == b))
}

#[must_use]
pub fn end(ids: &[Id]) -> Vec<(Id, Unended)> {
    let termed = process::signalled(ids, Signal::TERM, GRACE);
    let (outlived, mut left): (Vec<_>, Vec<_>) =
        termed.into_iter().partition(|(_, why)| *why == Unended::Outlived);
    if !outlived.is_empty() {
        let outlived: Vec<Id> = outlived.into_iter().map(|(id, _)| id).collect();
        left.extend(process::signalled(&outlived, Signal::KILL, KILL_WAIT));
    }
    left.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    left
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::io::{BufRead as _, BufReader};
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    use super::*;

    fn id_of(child: &Child) -> Id {
        Id { pid: child.id(), start: process::start(child.id()).unwrap() }
    }

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
        let (ends_id, holds_id) = (id_of(&ends), id_of(&holds));
        assert_eq!(end(&[ends_id.clone(), holds_id]), []);
        assert!(started.elapsed() >= GRACE, "the one that holds out has its grace");
        assert!(ends.wait().unwrap().code().is_none());
        assert!(holds.wait().unwrap().code().is_none());
        assert_eq!(end(&[ends_id]), [], "one that has ended is left alone");
    }

    #[test]
    fn a_process_that_took_an_ended_one_s_pid_is_left_alone() {
        let mut runs = Command::new("sleep").arg("60").spawn().unwrap();
        let earlier = Id { pid: runs.id(), start: "0".to_owned() };
        assert_eq!(end(&[earlier]), []);
        assert!(runs.try_wait().unwrap().is_none(), "another process of its pid was ended");
        runs.kill().unwrap();
        runs.wait().unwrap();
    }

    fn found(pid: u32, name: &str, environ: &[&str]) -> process::Found {
        given(pid, name, &[], environ)
    }

    fn given(pid: u32, name: &str, arguments: &[&str], environ: &[&str]) -> process::Found {
        let id = Id { pid, start: "1".to_owned() };
        let argv = [name].iter().chain(arguments).map(|&argument| argument.to_owned()).collect();
        let environ = environ.iter().map(OsString::from).collect();
        process::Found { id, name: name.to_owned(), argv, environ }
    }

    #[test]
    fn a_mux_is_luneta_s_only_in_the_folder_it_was_given() {
        let folder = Path::new("/run/user/1000/luneta");
        let ours = ["XDG_RUNTIME_DIR=/run/user/1000"];
        let listed = vec![
            given(1, MUX, &["/run/user/1000/luneta"], &[]),
            given(2, MUX, &["/tmp/test/luneta"], &ours),
            given(3, MUX, &[], &ours),
        ];
        let running = running_of(listed, folder, 0);
        let muxes: Vec<u32> = running.muxes.into_iter().map(|id| id.pid).collect();
        assert_eq!(muxes, [1, 3]);
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
        let pids = |ids: Vec<Id>| ids.into_iter().map(|id| id.pid).collect::<Vec<_>>();
        let front_ends = pids(running.luneta_ids());
        let (daemons, muxes) = (pids(running.lunetads), pids(running.muxes));
        let named_by_tmpdir =
            if cfg!(target_os = "macos") { (vec![8], vec![]) } else { (vec![2, 8], vec![3]) };
        assert_eq!((front_ends, (daemons, muxes)), (vec![1], named_by_tmpdir));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_process_of_another_shell_s_tmpdir_is_luneta_s_on_macos() {
        let folder = folder::find_in(&[]).unwrap();
        let listed = vec![
            found(1, LUNETAD, &["TMPDIR=/var/T/nix-shell.a"]),
            found(2, MUX, &[]),
            found(3, LUNETA, &["XDG_RUNTIME_DIR=/tmp/test", "TMPDIR=/var/T/nix-shell.b"]),
        ];
        let running = running_of(listed, &folder, 0);
        let pids = |ids: Vec<Id>| ids.into_iter().map(|id| id.pid).collect::<Vec<_>>();
        let (daemons, muxes) = (pids(running.lunetads), pids(running.muxes));
        assert_eq!((running.lunetas, daemons, muxes), (vec![], vec![1], vec![2]));
    }
}
