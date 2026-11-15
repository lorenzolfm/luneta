use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::closes::running_in;
use crate::mux::own::server;
use crate::mux::{SessionName, Zellij};
use crate::Directory;

#[must_use]
pub fn session_for(dir: &Path) -> Option<SessionName> {
    let path = dir.to_str()?;
    common_dir(dir)
        .and_then(|common| worktree_name(path, common.to_str()?))
        .or_else(|| SessionName::parse(path.split('/').rfind(|part| !part.is_empty())?).ok())
}

fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = fs::read_to_string(dir.join(".git")).ok()?;
    let own = dir.join(dot_git.lines().next()?.strip_prefix("gitdir: ")?);
    let common = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(common.lines().next()?)).ok()
}

fn worktree_name(path: &str, common: &str) -> Option<SessionName> {
    let base = path.split('/').rfind(|part| !part.is_empty())?;
    let mut parts = common.split('/').filter(|part| !part.is_empty());
    let mut repository = parts.next_back()?;
    if repository.starts_with('.') {
        repository = parts.next_back()?;
    }
    let repository =
        repository.strip_suffix(".git").filter(|bare| !bare.is_empty()).unwrap_or(repository);
    if base.strip_prefix(repository).is_some_and(|rest| !rest.starts_with(char::is_alphanumeric)) {
        return None;
    }
    SessionName::parse(&format!("{repository}-{base}")).ok()
}

pub fn named() -> Result<Vec<SessionName>, String> {
    let running = running_in(&Zellij::socket_folder())?;
    let mut named: Vec<SessionName> = running.into_iter().map(|(name, _)| name).collect();
    let unknown = |why| format!("luneta cannot tell which sessions zellij keeps: {why}");
    named.extend(Zellij::kept().map_err(unknown)?);
    let own = server::names()?.into_iter().filter_map(|name| SessionName::running(&name).ok());
    named.extend(own);
    Ok(named)
}

pub fn free_for(dir: &Directory) -> Result<(SessionName, SystemTime), String> {
    let unnamed = || format!("no session can be named for {dir}");
    let name = session_for(Path::new(dir.as_str())).ok_or_else(unnamed)?;
    let since = SystemTime::now();
    let sessions = named()?;
    let names: Vec<&str> = sessions.iter().map(SessionName::as_str).collect();
    Ok((name.free(&names), since))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Starts {
    In(Directory),
    Home,
}

impl Starts {
    #[must_use]
    pub fn dir(&self) -> Option<&str> {
        match self {
            Starts::In(dir) => Some(dir.as_str()),
            Starts::Home => None,
        }
    }
}

pub fn free_at(starts: Starts) -> Result<(SessionName, SystemTime, Directory), String> {
    let dir = match starts {
        Starts::In(dir) => dir,
        Starts::Home => home()?,
    };
    let (name, since) = free_for(&dir)?;
    Ok((name, since, dir))
}

fn home() -> Result<Directory, String> {
    let unset = "HOME is not set to an absolute path, for a session in the home";
    let home = env::var_os("HOME").ok_or(unset)?.into_string().map_err(|_| unset)?;
    Directory::parse(home).map_err(|_| unset.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taken {
    Zellij,
    Kept,
    Own,
}

#[derive(Debug)]
pub enum Unusable {
    Taken(SessionName, Taken),
    Unknown(String),
}

impl fmt::Display for Unusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unusable::Taken(to, Taken::Zellij) => write!(f, "{to} runs already"),
            Unusable::Taken(to, Taken::Kept) => {
                write!(f, "zellij keeps {to}, which ended, to bring back")
            },
            Unusable::Taken(to, Taken::Own) => write!(f, "{to} runs already, as an own session"),
            Unusable::Unknown(why) => f.write_str(why),
        }
    }
}

impl From<Unusable> for String {
    fn from(unusable: Unusable) -> Self {
        unusable.to_string()
    }
}

pub fn unused(to: &SessionName) -> Result<SystemTime, Unusable> {
    let since = unused_by_zellij(to)?;
    let named = server::named(to).map_err(|failure| Unusable::Unknown(failure.to_string()))?;
    if named.is_some() {
        return Err(Unusable::Taken(to.clone(), Taken::Own));
    }
    Ok(since)
}

pub fn unused_by_zellij(to: &SessionName) -> Result<SystemTime, Unusable> {
    let since = SystemTime::now();
    let running = running_in(&Zellij::socket_folder()).map_err(Unusable::Unknown)?;
    if running.iter().any(|(name, _)| name == to) {
        return Err(Unusable::Taken(to.clone(), Taken::Zellij));
    }
    let unknown =
        |why| Unusable::Unknown(format!("luneta cannot tell whether zellij keeps {to}: {why}"));
    if Zellij::kept().map_err(unknown)?.contains(to) {
        return Err(Unusable::Taken(to.clone(), Taken::Kept));
    }
    Ok(since)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_directory_itself() {
        let named = |dir: &str| session_for(Path::new(dir)).map(|name| name.as_str().to_owned());
        assert_eq!(named("/nowhere/at/all/luneta"), Some("luneta".to_owned()));
        assert_eq!(named("/nowhere/at/all/luneta/"), Some("luneta".to_owned()));
        assert_eq!(named("/"), None);
    }

    #[test]
    fn a_worktree_is_named_for_its_repository_too() {
        let named = |path: &str, common: &str| {
            worktree_name(path, common).map(|name| name.as_str().to_owned())
        };
        assert_eq!(named("/w/luneta/main", "/w/luneta/.bare"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/trees/main", "/w/luneta/.git"), Some("luneta-main".to_owned()));
        assert_eq!(named("/w/infra.git/master", "/w/infra.git"), Some("infra-master".to_owned()));
        assert_eq!(named("/w/app-hotfix", "/w/app/.git"), None);
        assert_eq!(named("/w/application", "/w/app/.git"), Some("app-application".to_owned()));
        let long = "x".repeat(crate::mux::NAME_BYTES_MAX);
        assert_eq!(named(&format!("/w/{long}"), "/w/repo/.git"), None);
    }

    #[test]
    fn a_linked_worktree_is_found_by_its_git_files() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let common = root.join("luneta").join(".bare");
        let own = common.join("worktrees").join("main");
        let tree = root.join("luneta").join("main");
        for dir in [&own, &tree] {
            assert!(fs::create_dir_all(dir).is_ok());
        }
        let wrote = [
            fs::write(tree.join(".git"), format!("gitdir: {}\n", own.display())),
            fs::write(own.join("commondir"), "../..\n"),
        ];
        assert!(wrote.iter().all(Result::is_ok));
        assert_eq!(session_for(&tree).as_ref().map(SessionName::as_str), Some("luneta-main"));
        let repository = root.join("luneta");
        assert_eq!(session_for(&repository).as_ref().map(SessionName::as_str), Some("luneta"));
    }
}
