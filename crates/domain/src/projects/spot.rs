use std::collections::BTreeSet;
use std::path::Path;

use super::{Dir, Subfolder, WORKTREES_MAX};

pub const CLONE_WORKTREES_MAX: usize = 256;

pub const BARE: &str = ".bare";

const LISTED_MAX: usize = 2 * CLONE_WORKTREES_MAX;

const _: () = assert!(CLONE_WORKTREES_MAX <= WORKTREES_MAX);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneRoot {
    Of(Dir),
    Worked,
    NoBare,
    Unparsed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot(Place);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Clone { root: Dir, subfolder: Option<Subfolder>, subfolders: BTreeSet<Subfolder> },
    Folder { asked: Dir, repository: Option<Dir> },
}

impl Spot {
    #[must_use]
    pub fn in_clone(
        root: Dir,
        subfolder: Option<Subfolder>,
        subfolders: BTreeSet<Subfolder>,
    ) -> Spot {
        let count = subfolders.len();
        assert!(
            count <= CLONE_WORKTREES_MAX,
            "{} holds {count} worktrees, over {CLONE_WORKTREES_MAX}",
            root.0
        );
        if let Some(subfolder) = &subfolder {
            let one = subfolders.contains(subfolder);
            assert!(one, "worktree {} is one of {}'s", subfolder.as_str(), root.0);
        }
        Spot(Place::Clone { root, subfolder, subfolders })
    }

    #[must_use]
    pub fn in_folder(asked: Dir) -> Spot {
        Spot(Place::Folder { asked, repository: None })
    }

    #[must_use]
    pub fn in_repository(asked: Dir, top: Dir) -> Spot {
        assert!(top.below(&asked).is_some(), "{} holds {}, the folder asked", top.0, asked.0);
        Spot(Place::Folder { asked, repository: Some(top) })
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
    pub fn subfolders(&self) -> Option<&BTreeSet<Subfolder>> {
        match &self.0 {
            Place::Clone { subfolders, .. } => Some(subfolders),
            Place::Folder { .. } => None,
        }
    }

    #[must_use]
    pub fn is_clone(&self) -> bool {
        self.subfolders().is_some()
    }
}

#[must_use]
pub fn clone_root(common: &str, bare: bool) -> CloneRoot {
    if !bare {
        return CloneRoot::Worked;
    }
    let bare = Path::new(common);
    if bare.file_name().and_then(|name| name.to_str()) != Some(BARE) {
        return CloneRoot::NoBare;
    }
    let root = bare.parent().and_then(Path::to_str).expect("a named path of a str has a parent");
    match Dir::parse(root) {
        Ok(root) => CloneRoot::Of(root),
        Err(why) => CloneRoot::Unparsed(why),
    }
}

pub fn porcelain(root: &Dir, said: &[u8]) -> Result<BTreeSet<Subfolder>, String> {
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
    if subfolders.len() > CLONE_WORKTREES_MAX {
        return Err(format!("{} holds over {CLONE_WORKTREES_MAX} worktrees", root.0));
    }
    Ok(subfolders)
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
        assert_eq!(clone_root("/w/lune/.bare", true), CloneRoot::Of(dir("/w/lune")));
        assert_eq!(clone_root("/w/lune/.bare", false), CloneRoot::Worked);
        assert_eq!(clone_root("/w/lune/.git", false), CloneRoot::Worked);
        assert_eq!(clone_root("/w/lune/.git", true), CloneRoot::NoBare);
        assert_eq!(clone_root("/w/lune.bare", true), CloneRoot::NoBare);
        assert_eq!(
            clone_root("/.bare", true),
            CloneRoot::Unparsed("/ holds every folder, and so is no project's".to_owned())
        );
        assert_eq!(
            clone_root("/w/lun\teta/.bare", true),
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
        assert_eq!(porcelain(&dir("/w/lune"), said), Ok(want));
    }

    #[test]
    fn a_listing_of_more_worktrees_than_are_kept_is_refused_as_it_is_read() {
        let entry = |at: usize| format!("worktree /w/lune/{at}\0HEAD 1\0\0");
        let said: String = (0..=LISTED_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/lune"), said.as_bytes()).unwrap_err();
        assert!(refused.contains(&format!("over {LISTED_MAX} worktrees")), "{refused}");
        let said: String = (0..=CLONE_WORKTREES_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/lune"), said.as_bytes()).unwrap_err();
        assert!(refused.contains(&format!("over {CLONE_WORKTREES_MAX} worktrees")), "{refused}");
    }

    #[test]
    #[should_panic(expected = "is one of /w/lune's")]
    fn a_clone_s_spot_is_in_one_of_its_worktrees_or_none() {
        let _ = Spot::in_clone(dir("/w/lune"), Some(subfolder("main")), BTreeSet::new());
    }

    #[test]
    #[should_panic(expected = "the folder asked")]
    fn a_folder_s_spot_holds_the_folder_asked() {
        let _ = Spot::in_repository(dir("/w/elo"), dir("/w/lune"));
    }
}
