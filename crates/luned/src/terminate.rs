use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rustix::process::Signal;

use crate::{folder, process, projects};

pub use crate::process::{Id, Start, Unended, Unreadable};

pub const LUNE: &str = "lune";

pub const LUNED: &str = "luned";

pub const MUX: &str = "lune-mux";

const _: () = assert!(LUNE.len() <= process::NAME_BYTES_MAX, "Linux keeps 15 bytes of a name");
const _: () = assert!(LUNED.len() <= process::NAME_BYTES_MAX, "Linux keeps 15 bytes of a name");
const _: () = assert!(MUX.len() <= process::NAME_BYTES_MAX, "Linux keeps 15 bytes of a name");

pub const GRACE: Duration = Duration::from_secs(5);

pub const KILL_WAIT: Duration = Duration::from_secs(2);

const _: () = assert!(!GRACE.is_zero(), "a process has a while to end at its SIGTERM");
const _: () = assert!(!KILL_WAIT.is_zero(), "a process has a while to end at its SIGKILL");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lune {
    pub id: Id,
    pub argv: Vec<OsString>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Running {
    pub lunes: Vec<Lune>,
    pub luneds: Vec<Id>,
    pub muxes: Vec<Id>,
    pub served: Vec<PathBuf>,
    pub unreadable: Vec<process::Unreadable>,
}

impl Running {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lunes.is_empty() && self.luneds.is_empty() && self.muxes.is_empty()
    }

    #[must_use]
    pub fn lune_ids(&self) -> Vec<Id> {
        self.lunes.iter().map(|lune| lune.id.clone()).collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folders {
    here: PathBuf,
    holder: Option<PathBuf>,
}

impl Folders {
    #[must_use]
    pub fn of(here: &Path) -> Folders {
        let holder = projects::holder().filter(|holder| !same(holder, here));
        Folders { here: here.to_owned(), holder }
    }

    fn each(&self) -> Vec<&Path> {
        std::iter::once(self.here.as_path()).chain(self.holder.as_deref()).collect()
    }
}

#[must_use]
pub fn running(folders: &Folders) -> Running {
    running_of(process::mine(&[LUNE, LUNED, MUX]), &folders.each(), std::process::id())
}

fn running_of(mine: process::Mine, folders: &[&Path], me: u32) -> Running {
    let process::Mine { found, unreadable } = mine;
    let unreadable = unreadable.into_iter().filter(|it| it.id.pid != me).collect();
    let mut running = Running { unreadable, ..Running::default() };
    for found in found {
        if found.id.pid == me {
            continue;
        }
        let Some(theirs) = listened_in(&found) else {
            continue;
        };
        let Some(&folder) = folders.iter().find(|folder| same(&theirs, folder)) else {
            continue;
        };
        match found.name.as_str() {
            LUNE => running.lunes.push(Lune { id: found.id, argv: found.argv }),
            LUNED => {
                running.luneds.push(found.id);
                if !running.served.iter().any(|served| served == folder) {
                    running.served.push(folder.to_owned());
                }
            },
            MUX => running.muxes.push(found.id),
            _ => {},
        }
    }
    running.lunes.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    running.luneds.sort_unstable();
    running.muxes.sort_unstable();
    running.unreadable.sort_unstable_by(|a, b| a.id.cmp(&b.id));
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

    use lune_waits::Began;

    use super::*;

    fn id_of(child: &Child) -> Id {
        Id { pid: child.id(), start: process::start(child.id()).unwrap() }
    }

    #[test]
    fn a_process_ends_at_its_term_and_one_that_holds_out_at_its_kill() {
        let mut ends = Command::new("sleep").arg("60").spawn().unwrap();
        let script = "trap '' TERM; echo held; while :; do sleep 0.05; done";
        let mut holds = Command::new("sh")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut held = String::new();
        BufReader::new(holds.stdout.take().unwrap()).read_line(&mut held).unwrap();
        assert_eq!(held, "held\n");
        let started = Began::now();
        let (ends_id, holds_id) = (id_of(&ends), id_of(&holds));
        assert_eq!(end(&[ends_id.clone(), holds_id]), []);
        started.lasted().at_least(GRACE, "the one that holds out has its grace");
        assert!(ends.wait().unwrap().code().is_none());
        assert!(holds.wait().unwrap().code().is_none());
        assert_eq!(end(&[ends_id]), [], "one that has ended is left alone");
    }

    #[test]
    fn a_process_that_took_an_ended_one_s_pid_is_left_alone() {
        let mut runs = Command::new("sleep").arg("60").spawn().unwrap();
        let earlier = Id { pid: runs.id(), start: Start::spelled("0".to_owned()).unwrap() };
        assert_eq!(end(&[earlier]), []);
        assert!(runs.try_wait().unwrap().is_none(), "another process of its pid was ended");
        runs.kill().unwrap();
        runs.wait().unwrap();
    }

    fn mine(found: Vec<process::Found>) -> process::Mine {
        process::Mine { found, unreadable: Vec::new() }
    }

    fn found(pid: u32, name: &str, environ: &[&str]) -> process::Found {
        given(pid, name, &[], environ)
    }

    fn given(pid: u32, name: &str, arguments: &[&str], environ: &[&str]) -> process::Found {
        let id = Id { pid, start: Start::spelled("1".to_owned()).unwrap() };
        let argv = [name].iter().chain(arguments).map(OsString::from).collect();
        let environ = environ.iter().map(OsString::from).collect();
        process::Found { id, parent: None, name: name.to_owned(), argv, environ }
    }

    #[test]
    fn a_mux_is_lune_s_only_in_the_folder_it_was_given() {
        let folder = Path::new("/run/user/1000/lune");
        let ours = ["XDG_RUNTIME_DIR=/run/user/1000"];
        let listed = vec![
            given(1, MUX, &["/run/user/1000/lune"], &[]),
            given(2, MUX, &["/tmp/test/lune"], &ours),
            given(3, MUX, &[], &ours),
        ];
        let running = running_of(mine(listed), &[folder], 0);
        let muxes: Vec<u32> = running.muxes.into_iter().map(|id| id.pid).collect();
        assert_eq!(muxes, [1, 3]);
    }

    #[test]
    fn a_process_is_lune_s_only_in_the_folder_its_variables_name() {
        let folder = Path::new("/run/user/1000/lune");
        let listed = vec![
            found(1, LUNE, &["XDG_RUNTIME_DIR=/run/user/1000", "TMPDIR=/tmp"]),
            found(2, LUNED, &["HOME=/home/me", "TMPDIR=/run/user/1000"]),
            found(3, MUX, &["XDG_RUNTIME_DIR=", "TMPDIR=/run/user/1000"]),
            found(4, LUNED, &["XDG_RUNTIME_DIR=/tmp/test", "TMPDIR=/run/user/1000"]),
            found(5, MUX, &["TMPDIR=/tmp"]),
            found(6, LUNE, &[]),
            found(7, LUNE, &["XDG_RUNTIME_DIR=/run/user/1000"]),
            found(8, LUNED, &["XDG_RUNTIME_DIR=/run/user/1000", "XDG_RUNTIME_DIR=/tmp"]),
        ];
        let running = running_of(mine(listed), &[folder], 7);
        let pids = |ids: Vec<Id>| ids.into_iter().map(|id| id.pid).collect::<Vec<_>>();
        let front_ends = pids(running.lune_ids());
        let (daemons, muxes) = (pids(running.luneds), pids(running.muxes));
        assert_eq!((front_ends, daemons, muxes), (vec![1], vec![8], vec![]));
    }

    #[test]
    fn a_process_of_another_shell_s_tmpdir_is_lune_s() {
        let folder = folder::find_in(&[]).unwrap();
        let listed = vec![
            found(1, LUNED, &["TMPDIR=/var/T/nix-shell.a"]),
            found(2, MUX, &[]),
            found(3, LUNE, &["XDG_RUNTIME_DIR=/tmp/test", "TMPDIR=/var/T/nix-shell.b"]),
        ];
        let running = running_of(mine(listed), &[&folder], 0);
        let pids = |ids: Vec<Id>| ids.into_iter().map(|id| id.pid).collect::<Vec<_>>();
        let (daemons, muxes) = (pids(running.luneds), pids(running.muxes));
        assert_eq!((running.lunes, daemons, muxes), (vec![], vec![1], vec![2]));
    }

    #[test]
    fn a_process_in_the_folder_of_the_projects_holder_is_lune_s_too() {
        let (folder, holder) = (Path::new("/run/user/1000/lune"), Path::new("/tmp/test/lune"));
        let listed = vec![
            found(1, LUNED, &["XDG_RUNTIME_DIR=/tmp/test"]),
            given(2, MUX, &["/tmp/test/lune"], &[]),
            found(3, LUNE, &["XDG_RUNTIME_DIR=/run/user/1000"]),
            found(4, LUNED, &["XDG_RUNTIME_DIR=/tmp/other"]),
        ];
        let running = running_of(mine(listed), &[folder, holder], 0);
        assert_eq!(running.served, [holder.to_owned()]);
        let pids = |ids: Vec<Id>| ids.into_iter().map(|id| id.pid).collect::<Vec<_>>();
        let front_ends = pids(running.lune_ids());
        let (daemons, muxes) = (pids(running.luneds), pids(running.muxes));
        assert_eq!((front_ends, daemons, muxes), (vec![3], vec![1], vec![2]));
    }

    #[test]
    fn a_process_that_did_not_read_is_said_and_never_ended() {
        let folder = Path::new("/run/user/1000/lune");
        let unreadable = |pid: u32, name: &str| process::Unreadable {
            id: Id { pid, start: Start::spelled("1".to_owned()).unwrap() },
            name: name.to_owned(),
            why: "KERN_PROCARGS2 holds a negative argc".to_owned(),
        };
        let found = vec![found(1, LUNED, &["XDG_RUNTIME_DIR=/run/user/1000"])];
        let unread = vec![unreadable(4, MUX), unreadable(2, LUNED), unreadable(3, LUNE)];
        let running = running_of(process::Mine { found, unreadable: unread }, &[folder], 3);
        assert_eq!(running.unreadable, [unreadable(2, LUNED), unreadable(4, MUX)]);
        let daemons: Vec<u32> = running.luneds.iter().map(|id| id.pid).collect();
        assert_eq!(daemons, [1]);
        assert_eq!(running.muxes, []);
    }
}
