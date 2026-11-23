use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::projects::{spot, Dir, ProjectName};
use crate::{zoxide, Directory};

pub const OWNED_FOLDER: &str = "lune";

pub const REPOSITORIES_MAX: usize = 512;

const OWNED_ENTRIES_MAX: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    path: Dir,
    name: ProjectName,
}

impl Repository {
    pub fn at(path: Dir) -> Result<Repository, String> {
        let name = ProjectName::of_folder(&path)?;
        Ok(Repository { path, name })
    }

    #[must_use]
    pub fn path(&self) -> &Dir {
        &self.path
    }

    #[must_use]
    pub fn name(&self) -> &ProjectName {
        &self.name
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    pub repositories: Vec<Repository>,
    pub unread: Vec<String>,
}

#[must_use]
pub fn owned_folder() -> Option<Dir> {
    let home = fs::canonicalize(env::var_os("HOME")?).ok()?;
    let home = home.to_str()?.trim_end_matches('/');
    Dir::parse(&format!("{home}/{OWNED_FOLDER}")).ok()
}

pub fn found() -> Found {
    let mut unread = Vec::new();
    let owned = match owned() {
        Ok(Owned { folders, left_out }) => {
            if left_out > 0 {
                let left = format!("~/{OWNED_FOLDER}: left out {left_out} folders lune can't name");
                unread.push(left);
            }
            folders
        },
        Err(why) => {
            unread.push(why);
            Vec::new()
        },
    };
    let visited = match zoxide::visited() {
        Ok(visited) => visited,
        Err(why) => {
            unread.push(format!("{why}, so only the clones in ~/{OWNED_FOLDER} are listed"));
            Vec::new()
        },
    };
    let folders = owned.iter().map(Directory::from).chain(visited);
    let Roots { repositories, unnamed, cut } = roots(folders, spot::bare_in);
    if unnamed > 0 {
        unread.push(format!("left out {unnamed} clones whose folder names no project"));
    }
    if cut {
        unread.push(format!("listed the first {REPOSITORIES_MAX} clones, and no more"));
    }
    assert!(repositories.len() <= REPOSITORIES_MAX, "roots stops at REPOSITORIES_MAX");
    Found { repositories, unread }
}

struct Owned {
    folders: Vec<Dir>,
    left_out: usize,
}

fn owned() -> Result<Owned, String> {
    let Some(home) = env::var_os("HOME") else {
        return Err(format!("HOME is not set, so no clone in ~/{OWNED_FOLDER} is listed"));
    };
    let folder = Path::new(&home).join(OWNED_FOLDER);
    let entries = match fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Owned { folders: Vec::new(), left_out: 0 });
        },
        Err(error) => return Err(format!("{}: {error}", folder.display())),
    };
    let (mut folders, mut left_out) = (Vec::new(), 0);
    for entry in entries {
        let named = entry.ok().map(|entry| entry.path());
        let named = named.as_deref().and_then(Path::to_str);
        let Some(path) = named.and_then(|path| Dir::parse(path).ok()) else {
            left_out += 1;
            continue;
        };
        if !spot::bare_in(Path::new(path.as_str())) {
            continue;
        }
        if folders.len() == OWNED_ENTRIES_MAX {
            left_out += 1;
            continue;
        }
        folders.push(path);
    }
    folders.sort_by(|one, other| one.as_str().cmp(other.as_str()));
    assert!(folders.len() <= OWNED_ENTRIES_MAX, "~/lune's folders stop at OWNED_ENTRIES_MAX");
    Ok(Owned { folders, left_out })
}

struct Roots {
    repositories: Vec<Repository>,
    unnamed: usize,
    cut: bool,
}

fn roots(folders: impl IntoIterator<Item = Directory>, bare: fn(&Path) -> bool) -> Roots {
    let mut rooted: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    let mut seen = BTreeSet::new();
    let (mut repositories, mut unnamed) = (Vec::new(), 0);
    for folder in folders {
        let Some(root) = root_of(Path::new(folder.as_str()), bare, &mut rooted) else {
            continue;
        };
        let path = root.to_str().map(|it| Dir::parse(it.trim_end_matches('/')));
        let Some(Ok(repository)) = path.map(|path| path.and_then(Repository::at)) else {
            unnamed += 1;
            continue;
        };
        if !seen.insert(repository.path.clone()) {
            continue;
        }
        if repositories.len() == REPOSITORIES_MAX {
            return Roots { repositories, unnamed, cut: true };
        }
        repositories.push(repository);
    }
    assert_eq!(repositories.len(), seen.len(), "each root is listed once");
    Roots { repositories, unnamed, cut: false }
}

fn root_of(
    folder: &Path,
    bare: fn(&Path) -> bool,
    rooted: &mut HashMap<PathBuf, Option<PathBuf>>,
) -> Option<PathBuf> {
    let mut walked = Vec::new();
    let mut root = None;
    for above in folder.ancestors() {
        if let Some(known) = rooted.get(above) {
            root.clone_from(known);
            break;
        }
        walked.push(above.to_path_buf());
        if bare(above) {
            root = Some(above.to_path_buf());
            break;
        }
    }
    for walked in walked {
        rooted.insert(walked, root.clone());
    }
    if let Some(root) = &root {
        assert!(folder.starts_with(root), "{} is in its root {}", folder.display(), root.display());
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(path: &str) -> Directory {
        Directory::parse(path.to_owned()).unwrap()
    }

    fn repository(path: &str) -> Repository {
        Repository::at(Dir::parse(path).unwrap()).unwrap()
    }

    fn bare(folder: &Path) -> bool {
        let roots = ["/home/you/lune/app", "/home/you/Work/api", "/srv/cdk", "/srv/-dash"];
        roots.into_iter().chain(["/srv/odd "]).any(|root| Path::new(root) == folder)
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
            "/srv/-dash/main",
            "/srv/odd /main",
        ];
        let Roots { repositories, unnamed, cut } = roots(folders.map(directory), bare);
        let listed = [
            repository("/home/you/lune/app"),
            repository("/home/you/Work/api"),
            repository("/srv/cdk"),
            repository("/srv/-dash"),
        ];
        assert_eq!(repositories, listed);
        assert_eq!(repositories[3].name().as_str(), "-dash", "a project may start with -");
        assert_eq!((unnamed, cut), (1, false), "a name ending in a space names no project");
    }

    #[test]
    fn the_clones_listed_stop_at_their_most() {
        let every = |_: &Path| true;
        let folders = (0..=REPOSITORIES_MAX).map(|at| directory(&format!("/w/c{at}")));
        let Roots { repositories, unnamed, cut } = roots(folders, every);
        assert_eq!((repositories.len(), unnamed, cut), (REPOSITORIES_MAX, 0, true));
    }
}
