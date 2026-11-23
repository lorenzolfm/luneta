use std::borrow::Borrow;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

pub const NAME_BYTES_MAX: usize = 60;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(String);

impl SessionName {
    pub fn parse(name: &str) -> Result<Self, &'static str> {
        let name = SessionName::listed(name)?;
        if name.0.len() > NAME_BYTES_MAX {
            return Err("name must be shorter than 61 bytes");
        }
        if name.0.starts_with('-') {
            return Err("name cannot start with '-'");
        }
        Ok(name)
    }

    pub fn listed(name: &str) -> Result<Self, &'static str> {
        if name.is_empty() {
            return Err("name must not be empty");
        }
        if name.trim().is_empty() {
            return Err("name cannot be only whitespace");
        }
        if name == "." || name == ".." {
            return Err("name cannot be '.' or '..'");
        }
        if name.contains('/') {
            return Err("name cannot contain '/'");
        }
        Ok(SessionName(name.to_owned()))
    }

    #[must_use]
    pub fn with_postfix(&self, n: usize) -> Self {
        let postfix = format!("-{n}");
        let mut end = NAME_BYTES_MAX.saturating_sub(postfix.len()).min(self.0.len());
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        SessionName(format!("{}{postfix}", &self.0[..end]))
    }

    #[must_use]
    pub fn free(&self, names: &[&str]) -> Self {
        let taken = |name: &SessionName| names.contains(&name.as_str());
        std::iter::once(self.clone())
            .chain((2..names.len() + 3).map(|n| self.with_postfix(n)))
            .find(|name| !taken(name))
            .expect("one of more names than there are sessions is free")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for SessionName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_the_directory_itself() {
        let named = |dir: &str| session_for(Path::new(dir)).map(|name| name.as_str().to_owned());
        assert_eq!(named("/nowhere/at/all/lune"), Some("lune".to_owned()));
        assert_eq!(named("/nowhere/at/all/lune/"), Some("lune".to_owned()));
        assert_eq!(named("/"), None);
    }

    #[test]
    fn a_worktree_is_named_for_its_repository_too() {
        let named = |path: &str, common: &str| {
            worktree_name(path, common).map(|name| name.as_str().to_owned())
        };
        assert_eq!(named("/w/lune/main", "/w/lune/.bare"), Some("lune-main".to_owned()));
        assert_eq!(named("/w/trees/main", "/w/lune/.git"), Some("lune-main".to_owned()));
        assert_eq!(named("/w/infra.git/master", "/w/infra.git"), Some("infra-master".to_owned()));
        assert_eq!(named("/w/app-hotfix", "/w/app/.git"), None);
        assert_eq!(named("/w/application", "/w/app/.git"), Some("app-application".to_owned()));
        let long = "x".repeat(NAME_BYTES_MAX);
        assert_eq!(named(&format!("/w/{long}"), "/w/repo/.git"), None);
    }

    #[test]
    fn a_linked_worktree_is_found_by_its_git_files() {
        let Ok(scratch) = tempfile::tempdir() else {
            panic!("no scratch directory");
        };
        let root = scratch.path();
        let common = root.join("lune").join(".bare");
        let own = common.join("worktrees").join("main");
        let tree = root.join("lune").join("main");
        for dir in [&own, &tree] {
            assert!(fs::create_dir_all(dir).is_ok());
        }
        let wrote = [
            fs::write(tree.join(".git"), format!("gitdir: {}\n", own.display())),
            fs::write(own.join("commondir"), "../..\n"),
        ];
        assert!(wrote.iter().all(Result::is_ok));
        assert_eq!(session_for(&tree).as_ref().map(SessionName::as_str), Some("lune-main"));
        let repository = root.join("lune");
        assert_eq!(session_for(&repository).as_ref().map(SessionName::as_str), Some("lune"));
    }

    #[test]
    fn a_postfix_makes_room_for_itself_between_characters() {
        let Ok(name) = SessionName::parse("lune-main") else {
            panic!("a name lune takes");
        };
        assert_eq!(name.with_postfix(2).as_str(), "lune-main-2");
        let Ok(long) = SessionName::parse(&format!("{}é", "x".repeat(NAME_BYTES_MAX - 3))) else {
            panic!("a name lune takes");
        };
        let postfixed = long.with_postfix(12);
        assert_eq!(postfixed.as_str(), format!("{}-12", "x".repeat(NAME_BYTES_MAX - 3)));
        assert!(SessionName::parse(postfixed.as_str()).is_ok());
    }

    #[test]
    fn a_name_that_names_no_file_is_no_row_s() {
        for name in ["../out", "..", ".", ""] {
            assert!(SessionName::parse(name).is_err(), "{name:?}");
            assert!(SessionName::listed(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_name_that_starts_with_a_dash_is_listed_but_made_by_no_one() {
        assert!(SessionName::parse("-c").is_err());
        assert!(SessionName::listed("-c").is_ok());
    }
}
