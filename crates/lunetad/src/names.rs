//! The name luneta gives a session it makes for a directory, as the
//! picker's directories name one, and the names zellij has, which it takes a
//! client to a session of rather than make one, on the machine it runs on.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::closes::running_in;
use crate::mux::{Mux, SessionName};
use crate::Directory;

/// The session a directory is for, as the picker's directories name it: for
/// the directory itself, or for its repository too when it is a worktree of
/// one, as `luneta-main`. None for a directory no session can be named for,
/// as `/`.
#[must_use]
pub fn session_for(dir: &Path) -> Option<SessionName> {
    let path = dir.to_str()?;
    common_dir(dir)
        .and_then(|common| worktree_name(path, common.to_str()?))
        .or_else(|| SessionName::parse(path.split('/').rfind(|part| !part.is_empty())?).ok())
}

/// The repository a linked worktree is of: its git directory, which every
/// worktree of it shares. A worktree's `.git` is a file that names its own git
/// directory, and that names the shared one in `commondir`. None for a
/// directory that is no linked worktree, as a repository's main one is not.
fn common_dir(dir: &Path) -> Option<PathBuf> {
    let dot_git = fs::read_to_string(dir.join(".git")).ok()?;
    let own = dir.join(dot_git.lines().next()?.strip_prefix("gitdir: ")?);
    let common = fs::read_to_string(own.join("commondir")).ok()?;
    fs::canonicalize(own.join(common.lines().next()?)).ok()
}

/// A worktree is named for its repository too, `luneta-main`, where `main`
/// alone would name one of every repository, wherever the worktree lives. The
/// repository is its git directory, less a bare repository's `.git`, or, when
/// that directory is hidden, as a `.git` or a `.bare`, the directory that
/// holds it. A worktree whose own name starts with its repository's, as
/// `app-hotfix` beside `app`, keeps it, and so does one whose name would come
/// out too long for zellij to make a session of.
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

/// The names `switch-session` takes the client to a session of, rather
/// than make one: of the sessions that run, by zellij's sockets, and of those
/// that ended, which zellij keeps to bring back. Or why luneta cannot tell
/// them, as it cannot read where zellij keeps either.
pub fn named() -> Result<Vec<SessionName>, String> {
    let running = running_in(&Mux::Zellij.socket_folder())?;
    let mut named: Vec<SessionName> = running.into_iter().map(|(name, _)| name).collect();
    let unknown = |why| format!("luneta cannot tell which sessions zellij keeps: {why}");
    named.extend(Mux::Zellij.kept().map_err(unknown)?);
    Ok(named)
}

/// The name a session luneta makes for `dir` takes: the one `session_for`
/// gives it, with a `-2` where a session has that name, one that runs or
/// one that ended, which zellij keeps to bring back, as zellij says now; and
/// the time just before luneta read which names zellij has. A session of the
/// name that started then or after is one zellij made since: one renamed to
/// the name keeps the time it started at. Or why it takes none.
pub fn free_for(dir: &Directory) -> Result<(SessionName, SystemTime), String> {
    let unnamed = || format!("no session can be named for {dir}");
    let name = session_for(Path::new(dir.as_str())).ok_or_else(unnamed)?;
    let since = SystemTime::now();
    let sessions = named()?;
    let names: Vec<&str> = sessions.iter().map(SessionName::as_str).collect();
    Ok((name.free(&names), since))
}

/// Where a session luneta makes starts: in a directory, or in the home of
/// the user of the machine it is made on, which only that machine knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Starts {
    In(Directory),
    Home,
}

impl Starts {
    /// The directory it starts in: none for the home.
    #[must_use]
    pub fn dir(&self) -> Option<&str> {
        match self {
            Starts::In(dir) => Some(dir.as_str()),
            Starts::Home => None,
        }
    }
}

/// The name a session luneta makes that starts as `starts` says takes, as
/// `free_for` names it, with the time it gives, and the directory it named
/// the session for, where it starts; or why it takes none.
pub fn free_at(starts: Starts) -> Result<(SessionName, SystemTime, Directory), String> {
    let dir = match starts {
        Starts::In(dir) => dir,
        Starts::Home => home()?,
    };
    let (name, since) = free_for(&dir)?;
    Ok((name, since, dir))
}

/// The home of the user this runs as, as `HOME` names it, where a session
/// luneta makes for no directory starts; or why it has none.
fn home() -> Result<Directory, String> {
    let unset = "HOME is not set to an absolute path, for a session in the home";
    let home = env::var_os("HOME").ok_or(unset)?.into_string().map_err(|_| unset)?;
    Directory::parse(home).map_err(|_| unset.to_owned())
}

/// Whether zellij would make a session called `to` now, rather than take a
/// client to one that has it: one that runs, by zellij's sockets, or one
/// that ended, which zellij keeps to bring back. The time just before luneta
/// read which names zellij has, as `free_for` says it; or why zellij would
/// make none.
pub fn unused(to: &SessionName) -> Result<SystemTime, String> {
    let since = SystemTime::now();
    let running = running_in(&Mux::Zellij.socket_folder())?;
    if running.iter().any(|(name, _)| name == to) {
        return Err(format!("{to} runs already"));
    }
    let unknown = |why| format!("luneta cannot tell whether zellij keeps {to}: {why}");
    if Mux::Zellij.kept().map_err(unknown)?.contains(to) {
        return Err(format!("zellij keeps {to}, which ended, to bring back"));
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

    /// A worktree made by git is found one, by the files git leaves in it.
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
