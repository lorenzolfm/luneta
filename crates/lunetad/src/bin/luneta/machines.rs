//! The machines luneta lists: this one, and the others luneta's hosts file
//! names, which lunetad watches through ssh, and reads the file again as it
//! changes. luneta adds a machine to the file, and takes one out of it.
//!
//! The file is written whole, under a name of its writer's own, before it
//! takes its own, so that lunetad never reads it halfway written. A line
//! that names no machine stays as it was, for you to mend.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use lunetad::{Destination, HOSTS_FILE, HOSTS_MAX};

use crate::state;

/// A machine the sidebar lists the sessions of.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Machine {
    /// This one, whose daemon luneta asks.
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

/// Where luneta's hosts file is.
fn file() -> Result<std::path::PathBuf, String> {
    state::path(HOSTS_FILE).map_err(|unset| unset.to_string())
}

fn add_to(file: &Path, destination: &Destination) -> Result<Added, String> {
    let mut text = read(file)?;
    let mut named: Vec<Destination> = Vec::new();
    for machine in text.lines().filter_map(|line| line.parse::<Destination>().ok()) {
        if !named.contains(&machine) {
            named.push(machine);
        }
    }
    if named.contains(destination) {
        return Ok(Added::Already);
    }
    if named.len() >= HOSTS_MAX {
        return Err(format!(
            "{} names {HOSTS_MAX} machines, as many as lunetad watches",
            file.display()
        ));
    }
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&destination.to_string());
    text.push('\n');
    write(file, &text)?;
    Ok(Added::Named)
}

fn forget_in(file: &Path, destination: &Destination) -> Result<bool, String> {
    let text = read(file)?;
    let names = |line: &&str| line.parse::<Destination>().is_ok_and(|it| it == *destination);
    if !text.lines().any(|line| names(&line)) {
        return Ok(false);
    }
    let mut kept = String::new();
    for line in text.lines().filter(|line| !names(line)) {
        kept.push_str(line);
        kept.push('\n');
    }
    write(file, &kept)?;
    Ok(true)
}

/// What the file at `file` says: nothing, where there is no file.
fn read(file: &Path) -> Result<String, String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

/// Writes `text` to `file` whole, under a name of this luneta's own first,
/// in a folder made where there is none.
fn write(file: &Path, text: &str) -> Result<(), String> {
    let folder = file.parent().expect("a file of `state::path` is in a folder");
    let unfinished = folder.join(format!("{HOSTS_FILE}.{}", state::unfinished()));
    let written = fs::create_dir_all(folder)
        .and_then(|()| fs::write(&unfinished, text))
        .and_then(|()| fs::rename(&unfinished, file));
    if written.is_err() {
        // What is left of it, if anything, names no machines either way.
        let _ = fs::remove_file(&unfinished);
    }
    written.map_err(|error| format!("{}: {error}", file.display()))
}

#[cfg(test)]
mod tests {
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
        let leftovers = fs::read_dir(file.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "no file is left halfway written");
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
