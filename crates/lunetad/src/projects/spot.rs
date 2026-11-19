use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use super::{Dir, Worktree, WORKSTREAMS_MAX};
use crate::git::{self, Common};
use crate::program::{self, Ran};

pub const WORKTREES_MAX: usize = 256;

pub const LOOK: Duration = Duration::from_secs(2);

const LISTED_MAX: usize = 2 * WORKTREES_MAX;

const BARE: &str = ".bare";

const _: () = assert!(WORKTREES_MAX <= WORKSTREAMS_MAX);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spot(Place);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Place {
    Clone { root: Dir, worktree: Option<Worktree>, worktrees: BTreeSet<Worktree> },
    Folder { asked: Dir, dir: Dir },
}

impl Spot {
    pub fn of(folder: &Dir, look: Duration) -> Result<Spot, String> {
        let until = Instant::now() + look;
        let common = match git::common_within(folder.as_str(), left(until, look)?) {
            Common::Of(common) => common,
            Common::NoRepository => return Ok(Spot::in_folder(folder.clone(), folder.clone())),
            Common::Unsaid(why) => {
                return Err(format!("git did not say what repository {} is in: {why}", folder.0));
            },
        };
        let top = toplevel(folder, until, look)?;
        if let Some(root) = root_of(&common) {
            let worktrees = listed(&root, until, look)?;
            let worktree = top.and_then(|top| root.worktree(&top));
            let worktree = worktree.filter(|it| worktrees.contains(it));
            return Ok(Spot::in_clone(root, worktree, worktrees));
        }
        let dir = top.filter(|top| top.below(folder).is_some());
        Ok(Spot::in_folder(folder.clone(), dir.unwrap_or_else(|| folder.clone())))
    }

    #[must_use]
    pub fn in_clone(root: Dir, worktree: Option<Worktree>, worktrees: BTreeSet<Worktree>) -> Spot {
        let count = worktrees.len();
        assert!(count <= WORKTREES_MAX, "{} holds {count} worktrees, over {WORKTREES_MAX}", root.0);
        if let Some(worktree) = &worktree {
            let one = worktrees.contains(worktree);
            assert!(one, "worktree {} is one of {}'s", worktree.as_str(), root.0);
        }
        Spot(Place::Clone { root, worktree, worktrees })
    }

    #[must_use]
    pub fn in_folder(asked: Dir, dir: Dir) -> Spot {
        assert!(dir.below(&asked).is_some(), "{} holds {}, the folder asked", dir.0, asked.0);
        Spot(Place::Folder { asked, dir })
    }

    #[must_use]
    pub fn place(&self) -> Dir {
        match &self.0 {
            Place::Clone { root, worktree: Some(worktree), .. } => root.at(worktree),
            Place::Clone { root, worktree: None, .. } => root.clone(),
            Place::Folder { asked, .. } => asked.clone(),
        }
    }

    #[must_use]
    pub fn dir(&self) -> &Dir {
        match &self.0 {
            Place::Clone { root, .. } => root,
            Place::Folder { dir, .. } => dir,
        }
    }

    #[must_use]
    pub fn worktree(&self) -> Option<&Worktree> {
        match &self.0 {
            Place::Clone { worktree, .. } => worktree.as_ref(),
            Place::Folder { .. } => None,
        }
    }

    #[must_use]
    pub fn worktrees(&self) -> Option<&BTreeSet<Worktree>> {
        match &self.0 {
            Place::Clone { worktrees, .. } => Some(worktrees),
            Place::Folder { .. } => None,
        }
    }

    #[must_use]
    pub fn is_clone(&self) -> bool {
        self.worktrees().is_some()
    }
}

#[must_use]
pub fn holds_bare(folder: &Dir) -> bool {
    Path::new(folder.as_str()).join(BARE).is_dir()
}

pub fn worktrees(root: &Dir, look: Duration) -> Result<BTreeSet<Worktree>, String> {
    listed(root, Instant::now() + look, look)
}

fn listed(root: &Dir, until: Instant, look: Duration) -> Result<BTreeSet<Worktree>, String> {
    let mut git = Command::new("git");
    git::local(&mut git).args(["-c", "safe.bareRepository=explicit", "-c", "core.fsmonitor=false"]);
    git.arg(format!("--git-dir={}/{BARE}", root.0)).current_dir("/");
    git.args(["worktree", "list", "--porcelain", "-z"]).env("LC_ALL", "C");
    let said = ran(&mut git, "worktree list", until, look)?;
    porcelain(root, &said)
}

fn left(until: Instant, look: Duration) -> Result<Duration, String> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(format!("git did not answer in {}ms", look.as_millis()));
    }
    Ok(left)
}

fn toplevel(folder: &Dir, until: Instant, look: Duration) -> Result<Option<Dir>, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-c", "safe.bareRepository=explicit", "-C", folder.as_str()]))
        .args(["rev-parse", "--is-inside-work-tree", "--show-toplevel"])
        .env("LC_ALL", "C");
    let stdout = match program::run(&mut git, left(until, look)?) {
        Ok(Ran::Ended { status, stdout, .. }) if status.success() => stdout,
        Ok(Ran::Ended { .. }) => return Ok(None),
        Ok(Ran::Stuck) => return Err(format!("git did not answer in {}ms", look.as_millis())),
        Err(error) => return Err(format!("git did not run: {error}")),
    };
    let said = String::from_utf8(stdout).map_err(|_| "git said a top that is not UTF-8")?;
    let mut lines = said.lines();
    if lines.next() != Some("true") {
        return Ok(None);
    }
    let top = lines.next().ok_or("git named no top")?;
    Dir::parse(top).map(Some)
}

fn ran(git: &mut Command, asked: &str, until: Instant, look: Duration) -> Result<Vec<u8>, String> {
    let ran = program::run(git, left(until, look)?);
    let ran = ran.map_err(|error| format!("git {asked} did not run: {error}"))?;
    ran.answer().map_err(|why| format!("git {asked}: {why}"))
}

fn root_of(common: &str) -> Option<Dir> {
    let root = common.strip_suffix(BARE)?.strip_suffix('/')?;
    Dir::parse(root).ok()
}

fn porcelain(root: &Dir, said: &[u8]) -> Result<BTreeSet<Worktree>, String> {
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
    let worktrees: BTreeSet<Worktree> = kept.filter_map(|(dir, _)| root.worktree(&dir)).collect();
    if worktrees.len() > WORKTREES_MAX {
        return Err(format!("{} holds over {WORKTREES_MAX} worktrees", root.0));
    }
    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn worktree(text: &str) -> Worktree {
        Worktree::parse(text).unwrap()
    }

    #[test]
    fn a_clone_s_root_is_the_folder_that_holds_its_bare_repository() {
        assert_eq!(root_of("/w/luneta/.bare"), Some(dir("/w/luneta")));
        assert_eq!(root_of("/w/luneta/.git"), None);
        assert_eq!(root_of("/w/luneta.bare"), None);
        assert_eq!(root_of("/.bare"), None);
    }

    #[test]
    fn a_clone_s_worktrees_are_those_below_its_root_that_are_neither_bare_nor_prunable() {
        let said = b"worktree /w/luneta/.bare\0bare\0\0\
            worktree /w/luneta/main\0HEAD 1\0branch refs/heads/main\0\0\
            worktree /w/luneta/gone\0HEAD 2\0prunable gitdir file points to non-existent location\0\0\
            worktree /elsewhere/x\0HEAD 3\0detached\0\0\
            worktree /w/luneta/issue-7\0HEAD 4\0branch refs/heads/issue-7\0locked\0\0";
        let want = BTreeSet::from([worktree("issue-7"), worktree("main")]);
        assert_eq!(porcelain(&dir("/w/luneta"), said), Ok(want));
    }

    #[test]
    fn a_listing_of_more_worktrees_than_are_kept_is_refused_as_it_is_read() {
        let entry = |at: usize| format!("worktree /w/luneta/{at}\0HEAD 1\0\0");
        let said: String = (0..=LISTED_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/luneta"), said.as_bytes()).unwrap_err();
        assert!(refused.contains(&format!("over {LISTED_MAX} worktrees")), "{refused}");
        let said: String = (0..=WORKTREES_MAX).map(entry).collect();
        let refused = porcelain(&dir("/w/luneta"), said.as_bytes()).unwrap_err();
        assert!(refused.contains(&format!("over {WORKTREES_MAX} worktrees")), "{refused}");
    }

    fn git(dir: &Path, args: &[&str]) {
        let mut git = Command::new("git");
        git::local(&mut git).current_dir(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null");
        let status = git.status().unwrap();
        assert!(status.success(), "git {args:?} in {}", dir.display());
    }

    fn real(path: &Path) -> Dir {
        dir(fs::canonicalize(path).unwrap().to_str().unwrap())
    }

    #[test]
    fn a_folder_is_spotted_in_its_clone_s_worktree_in_its_plain_repository_or_alone() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let (clone, plain, alone) = (scratch.join("luneta"), scratch.join("plain"), scratch.join("Vault"));
        fs::create_dir_all(clone.join(".bare")).unwrap();
        git(&clone.join(".bare"), &["init", "-q", "--bare", "-b", "main"]);
        fs::write(clone.join(".git"), "gitdir: ./.bare\n").unwrap();
        git(&clone, &["worktree", "add", "-q", "--orphan", "-b", "main", "main"]);
        fs::create_dir_all(clone.join("main/crates")).unwrap();
        fs::create_dir_all(clone.join("notes")).unwrap();
        fs::create_dir_all(&plain).unwrap();
        git(&plain, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(plain.join("src")).unwrap();
        fs::create_dir_all(alone.join("notes")).unwrap();

        let worktrees = BTreeSet::from([worktree("main")]);
        let clone_at = |at: Option<&str>| {
            Spot::in_clone(real(&clone), at.map(worktree), worktrees.clone())
        };
        let of = |folder: &Dir| Spot::of(folder, LOOK);
        assert_eq!(of(&real(&clone)), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("notes"))), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("main"))), Ok(clone_at(Some("main"))));
        assert_eq!(of(&real(&clone.join("main/crates"))), Ok(clone_at(Some("main"))));
        let src = real(&plain.join("src"));
        assert_eq!(of(&src), Ok(Spot::in_folder(src.clone(), real(&plain))));
        let linked = scratch.join("linked");
        std::os::unix::fs::symlink(&plain, &linked).unwrap();
        let through = dir(linked.join("src").to_str().unwrap());
        let alone_there = Spot::in_folder(through.clone(), through.clone());
        assert_eq!(of(&through), Ok(alone_there), "the top git names is real");
        let notes = real(&alone.join("notes"));
        assert_eq!(of(&notes), Ok(Spot::in_folder(notes.clone(), notes.clone())));
        assert_eq!(clone_at(Some("main")).place(), real(&clone.join("main")));
        assert_eq!(clone_at(None).dir(), &real(&clone));
        let none_left = Spot::of(&real(&clone), Duration::ZERO).unwrap_err();
        assert!(none_left.contains("did not answer in 0ms"), "{none_left}");
    }

    #[test]
    #[should_panic(expected = "is one of /w/luneta's")]
    fn a_clone_s_spot_is_in_one_of_its_worktrees_or_none() {
        let _ = Spot::in_clone(dir("/w/luneta"), Some(worktree("main")), BTreeSet::new());
    }

    #[test]
    #[should_panic(expected = "the folder asked")]
    fn a_folder_s_spot_holds_the_folder_asked() {
        let _ = Spot::in_folder(dir("/w/elo"), dir("/w/luneta"));
    }
}
