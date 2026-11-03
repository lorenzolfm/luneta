//! The machines luneta lists: this one, and the others luneta's hosts file
//! names, which lunetad watches through ssh, and reads the file again as it
//! changes. luneta adds a machine to the file, and takes one out of it.
//!
//! The file is written whole, under a name of its writer's own, before it
//! takes its own, so that lunetad never reads it halfway written. A line
//! that names no machine stays as it was, for you to mend. A luneta changes
//! it under the lock of `hosts.lock`, beside it, from the read to the write,
//! so that of two that change it at once, the second changes what the first
//! wrote. An editor takes no lock: a file you save that you opened before a
//! luneta changed it leaves out what that luneta changed.

use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::path::Path;

use lunetad::{Destination, HOSTS_FILE, HOSTS_MAX};

use crate::state;

/// A machine the sidebar lists the sessions of.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Machine {
    /// This one, whose daemon luneta asks.
    #[default]
    Local,
    /// Another, as luneta's hosts file names it, whose daemon this one's
    /// watches through ssh.
    Remote(Destination),
}

/// The name this machine goes by: its host's name up to the first dot, as
/// `hostname -s` says it.
pub fn local_name() -> String {
    let uname = rustix::system::uname();
    let node = uname.nodename().to_string_lossy();
    match node.split('.').next() {
        Some(short) if !short.is_empty() => short.to_owned(),
        _ => "this machine".to_owned(),
    }
}

/// What adding a machine to luneta's hosts file did.
#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    /// It names it now, on its last line.
    Named,
    /// It named it already.
    Already,
}

/// Adds `destination` to luneta's hosts file, on a line after the others,
/// unless the file names it already. Or why it does not: the file names as
/// many as lunetad watches, or does not read, or is not written.
pub fn add(destination: &Destination) -> Result<Added, String> {
    add_to(&file()?, destination)
}

/// Takes `destination` out of luneta's hosts file, each line that names it,
/// and says whether one did. Or why it does not: the file does not read, or
/// is not written.
pub fn forget(destination: &Destination) -> Result<bool, String> {
    forget_in(&file()?, destination)
}

/// The machines luneta's hosts file names now: none where it does not read,
/// or there is none to look for, as lunetad watches none then either.
pub fn named() -> Vec<Destination> {
    let text = file().and_then(|file| read(&file));
    text.map_or_else(|_| Vec::new(), |text| lunetad::hosts_named(&text, |_| {}).machines)
}

/// Where luneta's hosts file is.
fn file() -> Result<std::path::PathBuf, String> {
    state::path(HOSTS_FILE).map_err(|unset| unset.to_string())
}

fn add_to(file: &Path, destination: &Destination) -> Result<Added, String> {
    changed(file, |text| {
        let named = lunetad::hosts_named(text, |_| {});
        if named.machines.contains(destination) {
            return Ok((Added::Already, None));
        }
        if named.machines.len() >= HOSTS_MAX {
            return Err(format!(
                "{} names {HOSTS_MAX} machines, as many as lunetad watches",
                file.display()
            ));
        }
        let mut text = text.to_owned();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&destination.to_string());
        text.push('\n');
        Ok((Added::Named, Some(text)))
    })
}

fn forget_in(file: &Path, destination: &Destination) -> Result<bool, String> {
    changed(file, |text| {
        let names = |line: &&str| line.parse::<Destination>().is_ok_and(|it| it == *destination);
        if !text.lines().any(|line| names(&line)) {
            return Ok((false, None));
        }
        let mut kept = String::new();
        for line in text.lines().filter(|line| !names(line)) {
            kept.push_str(line);
            kept.push('\n');
        }
        Ok((true, Some(kept)))
    })
}

/// Changes the file at `file` as `change` says of what it says now, and
/// says what `change` did: with a text, the file says that from then on.
/// The lock is held from the read to the write, by every luneta that
/// changes the file. It is a file of its own, as the file itself is
/// renamed over as it is written, which a lock on it would not outlast.
/// lunetad reads the file without it: each write leaves it whole.
fn changed<T>(
    file: &Path,
    change: impl FnOnce(&str) -> Result<(T, Option<String>), String>,
) -> Result<T, String> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let failed = |error: io::Error| format!("{}: {error}", file.display());
    fs::create_dir_all(folder).map_err(failed)?;
    let lock = File::create(folder.join(format!("{HOSTS_FILE}.lock"))).map_err(failed)?;
    // Let go of as `lock` closes.
    lock.lock().map_err(failed)?;
    let (done, text) = change(&read(file)?)?;
    if let Some(text) = text {
        write(file, &text)?;
    }
    Ok(done)
}

/// What the file at `file` says: nothing, where there is no file.
fn read(file: &Path) -> Result<String, String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

/// Writes `text` to `file` whole, under a name of this luneta's own first.
fn write(file: &Path, text: &str) -> Result<(), String> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let unfinished = folder.join(format!("{HOSTS_FILE}.{}", state::unfinished()));
    let written = fs::write(&unfinished, text).and_then(|()| fs::rename(&unfinished, file));
    if written.is_err() {
        // What is left of it, if anything, names no machines either way.
        let _ = fs::remove_file(&unfinished);
    }
    written.map_err(|error| format!("{}: {error}", file.display()))
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    fn machine(name: &str) -> Destination {
        name.parse().unwrap()
    }

    /// A machine goes on a line after the others, once, and comes out of
    /// the file with every line that names it; a line that names none stays.
    #[test]
    fn a_machine_is_added_once_and_forgotten_where_it_was() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join(HOSTS_FILE);
        assert_eq!(add_to(&file, &machine("nixos")), Ok(Added::Named));
        fs::write(&file, "nixos\n-oops\n  me@vps").unwrap();
        assert_eq!(add_to(&file, &machine("me@vps")), Ok(Added::Already));
        assert_eq!(add_to(&file, &machine("pi")), Ok(Added::Named));
        assert_eq!(fs::read_to_string(&file).unwrap(), "nixos\n-oops\n  me@vps\npi\n");

        assert_eq!(forget_in(&file, &machine("me@vps")), Ok(true));
        assert_eq!(forget_in(&file, &machine("me@vps")), Ok(false));
        assert_eq!(fs::read_to_string(&file).unwrap(), "nixos\n-oops\npi\n");
        let mut left = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        left.sort();
        assert_eq!(left, ["hosts", "hosts.lock"], "no file is left halfway written");
    }

    /// Machines added at once, as by lunetas on one host, are each added to
    /// the file as another left it: none is lost to a write made after it.
    #[test]
    fn machines_added_at_once_are_each_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("luneta").join(HOSTS_FILE);
        let start = Barrier::new(HOSTS_MAX);
        thread::scope(|scope| {
            for n in 0..HOSTS_MAX {
                let (file, start) = (&file, &start);
                scope.spawn(move || {
                    start.wait();
                    assert_eq!(add_to(file, &machine(&format!("host-{n}"))), Ok(Added::Named));
                });
            }
        });
        let named = lunetad::hosts_named(&fs::read_to_string(&file).unwrap(), |_| {});
        assert_eq!(named.machines.len(), HOSTS_MAX);
    }

    /// No machine is added past the most lunetad watches: it would leave
    /// it out.
    #[test]
    fn a_file_that_names_as_many_as_lunetad_watches_takes_no_more() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join(HOSTS_FILE);
        let many: Vec<String> = (0..HOSTS_MAX).map(|n| format!("host-{n}\n")).collect();
        let many = many.concat();
        fs::write(&file, &many).unwrap();
        assert!(add_to(&file, &machine("one-more")).unwrap_err().contains("as many as"));
        assert_eq!(add_to(&file, &machine("host-0")), Ok(Added::Already));
        assert_eq!(fs::read_to_string(&file).unwrap(), many);
    }
}
