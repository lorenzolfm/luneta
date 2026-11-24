use std::collections::BTreeSet;
use std::path::Path;

use super::{Dir, Subfolder, WORKTREES_MAX};

pub const CLONE_WORKTREES_MAX: usize = 256;

pub const BARE: &str = ".bare";

const LISTED_MAX: usize = 2 * CLONE_WORKTREES_MAX;

const _: () = assert!(CLONE_WORKTREES_MAX <= WORKTREES_MAX);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bareness {
    Bare,
    Worked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneRoot {
    Of(Dir),
    Worked,
    NoBare,
    Unparsed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listed(BTreeSet<Subfolder>);

impl Listed {
    pub fn of(subfolders: BTreeSet<Subfolder>) -> Result<Listed, String> {
        let count = subfolders.len();
        if count > CLONE_WORKTREES_MAX {
            return Err(format!("{count} worktrees, over {CLONE_WORKTREES_MAX}"));
        }
        Ok(Listed(subfolders))
    }

    #[must_use]
    pub fn subfolders(&self) -> &BTreeSet<Subfolder> {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot(Place);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Clone { root: Dir, subfolder: Option<Subfolder>, listed: Listed },
    Folder { asked: Dir, repository: Option<Dir> },
}

impl Spot {
    #[must_use]
    pub fn in_clone(root: Dir, listed: Listed, at: Option<Subfolder>) -> Spot {
        let subfolder = at.filter(|it| listed.0.contains(it));
        Spot(Place::Clone { root, subfolder, listed })
    }

    #[must_use]
    pub fn in_folder(asked: Dir, top: Option<Dir>) -> Spot {
        let repository = top.filter(|top| top.below(&asked).is_some());
        Spot(Place::Folder { asked, repository })
    }

    #[must_use]
    pub fn place(&self) -> Dir {
        match &self.0 {
            Place::Clone { root, subfolder: Some(subfolder), .. } => root.at(subfolder),
            Place::Clone { root, subfolder: None, .. } => root.clone(),
            Place::Folder { asked, .. } => asked.clone(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Dir {
        match &self.0 {
            Place::Clone { root, .. } => root,
            Place::Folder { asked, repository } => repository.as_ref().unwrap_or(asked),
        }
    }

    #[must_use]
    pub fn repository(&self) -> Option<&Dir> {
        match &self.0 {
            Place::Clone { .. } => None,
            Place::Folder { repository, .. } => repository.as_ref(),
        }
    }

    #[must_use]
    pub fn subfolder(&self) -> Option<&Subfolder> {
        match &self.0 {
            Place::Clone { subfolder, .. } => subfolder.as_ref(),
            Place::Folder { .. } => None,
        }
    }

    #[must_use]
    pub fn listed(&self) -> Option<&Listed> {
        match &self.0 {
            Place::Clone { listed, .. } => Some(listed),
            Place::Folder { .. } => None,
        }
    }

    #[must_use]
    pub fn is_clone(&self) -> bool {
        self.listed().is_some()
    }
}

#[must_use]
pub fn clone_root(common: &Path, bareness: Bareness) -> CloneRoot {
    if bareness == Bareness::Worked {
        return CloneRoot::Worked;
    }
    if common.file_name().and_then(|name| name.to_str()) != Some(BARE) {
        return CloneRoot::NoBare;
    }
    let root = common.parent().and_then(Path::to_str).expect("a named path of a str has a parent");
    match Dir::parse(root) {
        Ok(root) => CloneRoot::Of(root),
        Err(why) => CloneRoot::Unparsed(why),
    }
}

pub fn porcelain(root: &Dir, said: &[u8]) -> Result<Listed, String> {
    let said = std::str::from_utf8(said).map_err(|_| "git listed a worktree that is not UTF-8")?;
    let mut found: Vec<(Dir, bool)> = Vec::new();
    for line in said.split('\0') {
        let (word, rest) = line.split_once(' ').unwrap_or((line, ""));
        match word {
            "worktree" => {
                if found.len() == LISTED_MAX {
                    return Err(format!("git listed over {LISTED_MAX} worktrees of {}", root.0));
                }
                found.push((Dir::parse(rest)?, true));
            },
            "bare" | "prunable" => {
                if let Some((_, kept)) = found.last_mut() {
                    *kept = false;
                }
            },
            _ => {},
        }
    }
    let kept = found.into_iter().filter(|(_, kept)| *kept);
    let subfolders: BTreeSet<Subfolder> =
        kept.filter_map(|(dir, _)| root.subfolder(&dir)).collect();
    Listed::of(subfolders).map_err(|why| format!("{} holds {why}", root.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn subfolder(text: &str) -> Subfolder {
        Subfolder::parse(text).unwrap()
    }

    #[test]
    fn a_clone_s_root_is_the_folder_that_holds_its_bare_repository() {
        use Bareness::{Bare, Worked};
        let root = |common: &str, bareness| clone_root(Path::new(common), bareness);
        assert_eq!(root("/w/lune/.bare", Bare), CloneRoot::Of(dir("/w/lune")));
        assert_eq!(root("/w/lune/.bare", Worked), CloneRoot::Worked);
        assert_eq!(root("/w/lune/.git", Worked), CloneRoot::Worked);
        assert_eq!(root("/w/lune/.git", Bare), CloneRoot::NoBare);
        assert_eq!(root("/w/lune.bare", Bare), CloneRoot::NoBare);
        assert_eq!(
            root("/.bare", Bare),
            CloneRoot::Unparsed("/ holds every folder, and so is no project's".to_owned())
        );
        assert_eq!(
            root("/w/lun\teta/.bare", Bare),
            CloneRoot::Unparsed("\"/w/lun\\teta\": a path holds a control character".to_owned())
        );
    }

    #[test]
    fn a_clone_s_worktrees_are_those_below_its_root_that_are_neither_bare_nor_prunable() {
        let said = b"worktree /w/lune/.bare\0bare\0\0\
            worktree /w/lune/main\0HEAD 1\0branch refs/heads/main\0\0\
            worktree /w/lune/gone\0HEAD 2\0prunable gitdir file points to non-existent location\0\0\
            worktree /elsewhere/x\0HEAD 3\0detached\0\0\
            worktree /w/lune/issue-7\0HEAD 4\0branch refs/heads/issue-7\0locked\0\0";
        let want = BTreeSet::from([subfolder("issue-7"), subfolder("main")]);
        assert_eq!(porcelain(&dir("/w/lune"), said).map(|it| it.subfolders().clone()), Ok(want));
    }

    #[test]
    fn a_listing_of_more_worktrees_than_are_kept_is_refused_as_it_is_read() {
        let entry = |at: usize| format!("worktree /w/lune/{at}\0HEAD 1\0\0");
        let said: String = (0..=LISTED_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/lune"), said.as_bytes()).unwrap_err();
        assert!(refused.contains(&format!("over {LISTED_MAX} worktrees")), "{refused}");
        let said: String = (0..=CLONE_WORKTREES_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/lune"), said.as_bytes()).unwrap_err();
        let over = CLONE_WORKTREES_MAX + 1;
        assert_eq!(refused, format!("/w/lune holds {over} worktrees, over {CLONE_WORKTREES_MAX}"));
    }

    #[test]
    fn a_clone_s_spot_is_in_a_worktree_only_when_the_clone_lists_it() {
        let listed = Listed::of(BTreeSet::from([subfolder("main")])).unwrap();
        let at = |at: &str| Spot::in_clone(dir("/w/lune"), listed.clone(), Some(subfolder(at)));
        assert_eq!(at("main").subfolder(), Some(&subfolder("main")));
        assert_eq!(at("main").place(), dir("/w/lune/main"));
        assert_eq!(at("notes").subfolder(), None);
        assert_eq!(at("notes").place(), dir("/w/lune"));
    }

    #[test]
    fn a_folder_s_spot_is_in_a_repository_only_when_its_top_holds_the_folder_asked() {
        let src = Spot::in_folder(dir("/w/elo/src"), Some(dir("/w/elo")));
        assert_eq!(src.repository(), Some(&dir("/w/elo")));
        assert_eq!(src.dir(), &dir("/w/elo"));
        let elsewhere = Spot::in_folder(dir("/w/elo"), Some(dir("/w/lune")));
        assert_eq!(elsewhere, Spot::in_folder(dir("/w/elo"), None));
        assert_eq!(elsewhere.dir(), &dir("/w/elo"));
    }
}
