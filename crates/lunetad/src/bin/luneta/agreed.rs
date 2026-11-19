use std::env;
use std::fmt;
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::path::{Component, Path, PathBuf};

use lunetad::asks::{Turn, BEFORE_VAR, TURN_VAR};
use lunetad::{prunes, var, Directory};

use crate::machines::Machine;
use crate::terminate;

pub const PLANNED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Agreed(u64);

impl Agreed {
    pub fn of_prune(plan: &prunes::Plan, turn: Option<u64>) -> Self {
        let mut hasher = DefaultHasher::new();
        plan.deletes().hash(&mut hasher);
        turn.hash(&mut hasher);
        Agreed(hasher.finish())
    }

    pub fn of_terminate(
        machines: &[(Machine, Option<terminate::Key>)],
        unread: &[String],
        turn: Option<u64>,
    ) -> Self {
        let mut hasher = DefaultHasher::new();
        (machines, unread).hash(&mut hasher);
        turn.hash(&mut hasher);
        Agreed(hasher.finish())
    }
}

impl fmt::Display for Agreed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:016x}", self.0)
    }
}

pub fn agreed(spelled: &str) -> Result<Agreed, String> {
    let read = u64::from_str_radix(spelled, 16);
    read.map(Agreed).map_err(|_| format!("{spelled} is no plan luneta said"))
}

pub fn turn_here() -> Result<Option<Turn>, String> {
    Turn::read(var(TURN_VAR)?.as_deref(), var(BEFORE_VAR)?.as_deref())
}

pub fn directory(cwd: Option<&Path>) -> Result<Directory, String> {
    let here = env::current_dir().map_err(|error| format!("no directory to look in: {error}"))?;
    let dir = plain(&cwd.map_or_else(|| here.clone(), |cwd| here.join(cwd)));
    if !dir.is_dir() {
        return Err(format!("{} is no directory", dir.display()));
    }
    let shown = dir.display().to_string();
    let dir = dir.into_os_string().into_string();
    Directory::parse(dir.map_err(|_| format!("{shown} is not UTF-8, as luneta needs"))?)
}

fn plain(path: &Path) -> PathBuf {
    let mut plain = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {},
            Component::ParentDir => {
                plain.pop();
            },
            part => plain.push(part),
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn a_directory_given_with_dots_is_the_one_they_lead_to() {
        let scratch = tempfile::tempdir().unwrap();
        let app = scratch.path().join("app");
        fs::create_dir_all(app.join("src")).unwrap();
        let named = |given: PathBuf| String::from(directory(Some(&given)).unwrap());
        let app_said = app.to_str().unwrap();
        assert_eq!(named(app.join(".")), app_said);
        assert_eq!(named(app.join("./src/..")), app_said);
        assert_eq!(named(app.join("src/../../app/")), app_said);
        assert_eq!(named(PathBuf::from("/../..")), "/");
    }
}
