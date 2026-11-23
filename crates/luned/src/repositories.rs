use std::collections::HashSet;
use std::env;
use std::fs;
use std::io;
use std::path::Path;

use crate::projects::spot;
use crate::{zoxide, Directory, SessionName};

pub const OWNED: &str = "lune";

const OWNED_MAX: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub path: Directory,
    pub name: SessionName,
}

pub fn found() -> Result<Vec<Repository>, String> {
    let owned = owned()?;
    let visited = zoxide::visited()?.into_iter().map(|dir| dir.path);
    Ok(roots(owned.into_iter().chain(visited), spot::bare_in))
}

fn owned() -> Result<Vec<Directory>, String> {
    let Some(home) = env::var_os("HOME") else {
        return Ok(Vec::new());
    };
    let folder = Path::new(&home).join(OWNED);
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", folder.display())),
    };
    let path = |entry: io::Result<fs::DirEntry>| {
        let path = entry.ok()?.path();
        Directory::parse(path.to_str()?.to_owned()).ok()
    };
    let mut owned: Vec<Directory> = entries.take(OWNED_MAX).filter_map(path).collect();
    owned.sort_by(|one, other| one.as_str().cmp(other.as_str()));
    Ok(owned)
}

fn roots(folders: impl IntoIterator<Item = Directory>, bare: fn(&Path) -> bool) -> Vec<Repository> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for folder in folders {
        let Some(root) = Path::new(folder.as_str()).ancestors().find(|it| bare(it)) else {
            continue;
        };
        let path = root.to_str().map(|it| it.trim_end_matches('/').to_owned());
        let Some(path) = path.and_then(|it| Directory::parse(it).ok()) else {
            continue;
        };
        let named = root.file_name().and_then(|it| it.to_str());
        let Some(name) = named.and_then(|it| SessionName::parse(it).ok()) else {
            continue;
        };
        if seen.insert(path.clone()) {
            found.push(Repository { path, name });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn repository(path: &str, name: &str) -> Repository {
        Repository { path: directory(path), name: SessionName::parse(name).unwrap() }
    }

    fn bare(folder: &Path) -> bool {
        ["/home/you/lune/app", "/home/you/Work/api", "/srv/cdk"].map(Path::new).contains(&folder)
    }

    #[test]
    fn a_folder_in_a_clone_wt_made_stands_for_its_root_listed_once_in_the_order_seen() {
        let folders = [
            "/home/you/lune/app",
            "/home/you/Work/api/main/src",
            "/home/you/notes",
            "/home/you/Work/api",
            "/srv/cdk/",
            "/home/you/Work/api/fix",
        ];
        let found = roots(folders.map(directory), bare);
        let roots = [
            repository("/home/you/lune/app", "app"),
            repository("/home/you/Work/api", "api"),
            repository("/srv/cdk", "cdk"),
        ];
        assert_eq!(found, roots);
    }
}
