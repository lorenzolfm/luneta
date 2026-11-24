use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{Dir, Subfolder, WORKTREES_MAX};
use crate::git::{self, Common, Git};
use crate::program::Ran;

pub const CLONE_WORKTREES_MAX: usize = 256;

pub const LOOK: Duration = Duration::from_secs(2);

const LISTED_MAX: usize = 2 * CLONE_WORKTREES_MAX;

const BARE: &str = ".bare";

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
    pub fn of(folder: &Dir, look: Duration) -> Result<Spot, String> {
        let until = Instant::now() + look;
        let common = match git::common_within(folder.as_str(), left(until, look)?) {
            Common::Of(common) => common,
            Common::NoRepository => return Ok(Spot::in_folder(folder.clone())),
            Common::Unsaid(why) => {
                return Err(format!("git did not say what repository {} is in: {why}", folder.0));
            },
        };
        let top = toplevel(folder, until, look)?;
        match clone_within(&common, until, look)? {
            CloneRoot::Of(root) => {
                let subfolders = listed(&root, until, look)?;
                let subfolder = top.and_then(|top| root.subfolder(&top));
                let subfolder = subfolder.filter(|it| subfolders.contains(it));
                return Ok(Spot::in_clone(root, subfolder, subfolders));
            },
            CloneRoot::Unparsed(why) => {
                return Err(format!(
                    "{} is in a clone wt made in a folder lune cannot keep: {why}",
                    folder.0
                ));
            },
            CloneRoot::Worked | CloneRoot::NoBare => {},
        }
        Ok(match top.filter(|top| top.below(folder).is_some()) {
            Some(top) => Spot::in_repository(folder.clone(), top),
            None => Spot::in_folder(folder.clone()),
        })
    }

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
pub fn holds_bare(folder: &Dir) -> bool {
    bare_in(Path::new(folder.as_str()))
}

#[must_use]
pub fn bare_in(folder: &Path) -> bool {
    folder.join(BARE).is_dir()
}

pub fn subfolders(root: &Dir, look: Duration) -> Result<BTreeSet<Subfolder>, String> {
    listed(root, Instant::now() + look, look)
}

fn listed(root: &Dir, until: Instant, look: Duration) -> Result<BTreeSet<Subfolder>, String> {
    let mut git = Git::of(format!("{}/{BARE}", root.0));
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
    let mut git = Git::at(folder.as_str());
    git.args(["rev-parse", "--is-inside-work-tree", "--show-toplevel"]).env("LC_ALL", "C");
    let stdout = match git.run(left(until, look)?) {
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

fn ran(git: &mut Git, asked: &str, until: Instant, look: Duration) -> Result<Vec<u8>, String> {
    let ran = git.run(left(until, look)?);
    let ran = ran.map_err(|error| format!("git {asked} did not run: {error}"))?;
    ran.answer().map_err(|why| format!("git {asked}: {why}"))
}

pub fn clone_of(common: &str, look: Duration) -> Result<CloneRoot, String> {
    clone_within(common, Instant::now() + look, look)
}

fn clone_within(common: &str, until: Instant, look: Duration) -> Result<CloneRoot, String> {
    let mut git = Git::of(common);
    git.args(["config", "--type=bool", "--get", "core.bare"]).env("LC_ALL", "C");
    let ran = git.run(left(until, look)?);
    let ran = ran.map_err(|error| format!("git config core.bare did not run: {error}"))?;
    Ok(clone_root(common, bare(ran)?))
}

fn bare(ran: Ran) -> Result<bool, String> {
    if let Ran::Ended { status, stdout, stderr } = &ran
        && status.code() == Some(1)
        && stdout.is_empty()
        && stderr.is_empty()
    {
        return Ok(false);
    }
    let said = ran.answer().map_err(|why| format!("git config core.bare: {why}"))?;
    match said.as_slice() {
        b"true\n" => Ok(true),
        b"false\n" => Ok(false),
        _ => Err(format!("git said core.bare is {:?}", String::from_utf8_lossy(&said))),
    }
}

fn clone_root(common: &str, bare: bool) -> CloneRoot {
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

fn porcelain(root: &Dir, said: &[u8]) -> Result<BTreeSet<Subfolder>, String> {
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
    use std::fs;
    use std::path::Path;

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

    fn git(dir: &Path, args: &[&str]) {
        let mut git = Git::at(dir);
        git.args(args).env("GIT_CONFIG_GLOBAL", "/dev/null");
        let said = git.run(Duration::from_mins(1)).unwrap().answer();
        assert!(said.is_ok(), "git {args:?} in {}: {said:?}", dir.display());
    }

    fn real(path: &Path) -> Dir {
        dir(fs::canonicalize(path).unwrap().to_str().unwrap())
    }

    #[test]
    fn a_folder_is_spotted_in_its_clone_s_worktree_in_its_plain_repository_or_alone() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let (clone, plain, alone) = (scratch.join("lune"), scratch.join("plain"), scratch.join("Vault"));
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

        let subfolders = BTreeSet::from([subfolder("main")]);
        let clone_at = |at: Option<&str>| {
            Spot::in_clone(real(&clone), at.map(subfolder), subfolders.clone())
        };
        let of = |folder: &Dir| Spot::of(folder, LOOK);
        assert_eq!(of(&real(&clone)), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("notes"))), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("main"))), Ok(clone_at(Some("main"))));
        assert_eq!(of(&real(&clone.join("main/crates"))), Ok(clone_at(Some("main"))));
        let src = real(&plain.join("src"));
        assert_eq!(of(&src), Ok(Spot::in_repository(src.clone(), real(&plain))));
        let linked = scratch.join("linked");
        std::os::unix::fs::symlink(&plain, &linked).unwrap();
        let through = dir(linked.join("src").to_str().unwrap());
        let alone_there = Spot::in_folder(through.clone());
        assert_eq!(of(&through), Ok(alone_there), "the top git names is real");
        let notes = real(&alone.join("notes"));
        assert_eq!(of(&notes), Ok(Spot::in_folder(notes.clone())));
        assert_eq!(clone_at(Some("main")).place(), real(&clone.join("main")));
        assert_eq!(clone_at(None).dir(), &real(&clone));
        let none_left = Spot::of(&real(&clone), Duration::ZERO).unwrap_err();
        assert!(none_left.contains("did not answer in 0ms"), "{none_left}");
    }

    #[test]
    fn a_bare_with_a_work_tree_is_no_clone_and_a_clone_lune_cannot_keep_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let (sep, worked) = (scratch.join("sep/.bare"), scratch.join("sep/main"));
        let sep = format!("--separate-git-dir={}", sep.display());
        git(&scratch, &["init", "-q", "-b", "main", &sep, worked.to_str().unwrap()]);
        let worked = real(&worked);
        assert_eq!(Spot::of(&worked, LOOK), Ok(Spot::in_repository(worked.clone(), worked.clone())));

        let unkept = scratch.join("lun\teta");
        fs::create_dir_all(unkept.join(".bare")).unwrap();
        git(&unkept.join(".bare"), &["init", "-q", "--bare", "-b", "main"]);
        fs::write(unkept.join(".git"), "gitdir: ./.bare\n").unwrap();
        let out = scratch.join("out");
        git(&unkept, &["worktree", "add", "-q", "--orphan", "-b", "main", out.to_str().unwrap()]);
        let refused = Spot::of(&real(&out), LOOK).unwrap_err();
        assert!(refused.contains("in a clone wt made in a folder lune cannot keep"), "{refused}");
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
