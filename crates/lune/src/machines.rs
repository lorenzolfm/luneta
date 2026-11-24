use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::path::Path;

use luned::api::Remote;
use luned::{Destination, HostsParsed, HOSTS_FILE, HOSTS_MAX};

use crate::state;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum Machine {
    #[default]
    Local,
    Remote(Destination),
}

pub fn local_name() -> String {
    let uname = rustix::system::uname();
    let node = uname.nodename().to_string_lossy();
    match node.split('.').next() {
        Some(short) if !short.is_empty() => short.to_owned(),
        _ => "this machine".to_owned(),
    }
}

#[must_use]
pub fn named_once(remotes: &[Remote]) -> Vec<Option<Destination>> {
    let mut named = HashSet::new();
    let named_once = remotes.iter().map(|remote| {
        let destination = remote.destination.parse::<Destination>().ok()?;
        named.insert(destination.clone()).then_some(destination)
    });
    named_once.collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Added {
    Named,
    Already,
}

pub fn add(destination: &Destination) -> Result<Added, String> {
    if destination.is_here() {
        return Err(format!("{destination}: {}", luned::THIS_MACHINE));
    }
    add_to(&file()?, destination)
}

pub fn forget(destination: &Destination) -> Result<bool, String> {
    forget_in(&file()?, destination)
}

pub fn parsed() -> Result<HostsParsed, String> {
    Ok(luned::hosts_parsed(&read(&file()?)?))
}

fn file() -> Result<std::path::PathBuf, String> {
    state::Folder::of_user().path(HOSTS_FILE).map_err(|unset| unset.to_string())
}

fn add_to(file: &Path, destination: &Destination) -> Result<Added, String> {
    changed(file, |text| {
        let named = luned::hosts_named(text, |_| {});
        if named.lines.iter().any(|line| line.destination() == destination) {
            return Ok((Added::Already, None));
        }
        if named.lines.len() >= HOSTS_MAX {
            return Err(format!(
                "{} names {HOSTS_MAX} machines, as many as luned watches",
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

fn changed<T>(
    file: &Path,
    change: impl FnOnce(&str) -> Result<(T, Option<String>), String>,
) -> Result<T, String> {
    let folder = file.parent().expect("a file of the state folder is in it");
    let failed = |error: io::Error| format!("{}: {error}", file.display());
    fs::create_dir_all(folder).map_err(failed)?;
    let lock = File::create(folder.join(format!("{HOSTS_FILE}.lock"))).map_err(failed)?;
    lock.lock().map_err(failed)?;
    let (done, text) = change(&read(file)?)?;
    if let Some(text) = text {
        write(file, &text)?;
    }
    Ok(done)
}

fn read(file: &Path) -> Result<String, String> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(format!("{}: {error}", file.display())),
    }
}

fn write(file: &Path, text: &str) -> Result<(), String> {
    let folder = file.parent().expect("a file of the state folder is in it");
    let unfinished = folder.join(format!("{HOSTS_FILE}.{}", state::unfinished()));
    let written = fs::write(&unfinished, text).and_then(|()| fs::rename(&unfinished, file));
    if written.is_err() {
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

    #[test]
    fn a_remote_is_named_by_its_first_listing_and_never_by_an_unparsed_one() {
        let remote = |destination: &str| Remote { destination: destination.to_owned(), reached: None };
        let remotes = [remote("nixos"), remote("-oops"), remote("pi"), remote("nixos")];
        let named = [Some(machine("nixos")), None, Some(machine("pi")), None];
        assert_eq!(named_once(&remotes), named);
    }

    #[test]
    fn a_machine_is_added_once_and_forgotten_where_it_was() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("lune").join(HOSTS_FILE);
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

    #[test]
    fn machines_added_at_once_are_each_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let file = scratch.path().join("lune").join(HOSTS_FILE);
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
        let named = luned::hosts_named(&fs::read_to_string(&file).unwrap(), |_| {});
        assert_eq!(named.lines.len(), HOSTS_MAX);
    }

    #[test]
    fn a_file_that_names_as_many_as_luned_watches_takes_no_more() {
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
