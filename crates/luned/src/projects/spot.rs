use std::path::Path;
use std::time::{Duration, Instant};

use lune_domain::projects::BARE;
use lune_domain::projects::{Bareness, CloneRoot, Dir, Listed, Spot, clone_root, porcelain};

use crate::directory::Directory;
use crate::git::{self, Common, Git};
use crate::program::Ran;

pub const LOOK: Duration = Duration::from_secs(2);

pub fn of(folder: &Dir, look: Duration) -> Result<Spot, String> {
    let until = Instant::now() + look;
    let common = match git::common_within(folder.as_str(), left(until, look)?) {
        Common::Of(common) => common,
        Common::NoRepository => return Ok(Spot::in_folder(folder.clone(), None)),
        Common::Unsaid(why) => {
            let folder = folder.as_str();
            return Err(format!(
                "git did not say what repository {folder} is in: {why}"
            ));
        }
        Common::BareUnread(bare) => {
            return Err(format!(
                "{} is in {}",
                folder.as_str(),
                Common::bare_unread(&bare)
            ));
        }
        Common::Gone => return Err(format!("{} is gone", folder.as_str())),
    };
    let top = toplevel(folder, until, look)?;
    match clone_within(&common, until, look)? {
        CloneRoot::Of(root) => {
            let listed = listed(&root, until, look)?;
            let at = top.and_then(|top| root.subfolder(&top));
            return Ok(Spot::in_clone(root, listed, at));
        }
        CloneRoot::Unparsed(why) => {
            return Err(format!(
                "{} is in a clone wt made in a folder lune cannot keep: {why}",
                folder.as_str()
            ));
        }
        CloneRoot::Worked | CloneRoot::NoBare => {}
    }
    Ok(Spot::in_folder(folder.clone(), top))
}

#[must_use]
pub fn holds_bare(folder: &Dir) -> bool {
    bare_in(Path::new(folder.as_str()))
}

#[must_use]
pub fn bare_in(folder: &Path) -> bool {
    folder.join(BARE).is_dir()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unlisted {
    Invalid(String),
    Transient(String),
}

impl Unlisted {
    #[must_use]
    pub fn into_string(self) -> String {
        match self {
            Unlisted::Invalid(why) | Unlisted::Transient(why) => why,
        }
    }
}

pub fn subfolders(root: &Dir, look: Duration) -> Result<Listed, String> {
    subfolders_checked(root, look).map_err(Unlisted::into_string)
}

pub fn subfolders_checked(root: &Dir, look: Duration) -> Result<Listed, Unlisted> {
    listed_checked(root, Instant::now() + look, look)
}

fn listed(root: &Dir, until: Instant, look: Duration) -> Result<Listed, String> {
    listed_checked(root, until, look).map_err(Unlisted::into_string)
}

fn listed_checked(root: &Dir, until: Instant, look: Duration) -> Result<Listed, Unlisted> {
    let mut git = Git::of(&Directory::from(root).child(BARE), "worktree");
    git.args(["list", "--porcelain", "-z"]);
    let limit = left(until, look).map_err(Unlisted::Transient)?;
    let outcome = git
        .run(limit)
        .map_err(|error| Unlisted::Transient(format!("git worktree list did not run: {error}")))?;
    match outcome {
        Ran::Ended { status, stdout, .. } if status.success() => {
            porcelain(root, &stdout).map_err(Unlisted::Invalid)
        }
        Ran::Ended { status, .. } => {
            let signaled = status.code().is_none();
            let why = outcome
                .answer()
                .expect_err("non-successful git run returns an error");
            let why = format!("git worktree list: {why}");
            if signaled {
                Err(Unlisted::Transient(why))
            } else {
                Err(Unlisted::Invalid(why))
            }
        }
        Ran::Stuck => {
            let why = outcome
                .answer()
                .expect_err("stuck git run returns an error");
            Err(Unlisted::Transient(format!("git worktree list: {why}")))
        }
    }
}

fn left(until: Instant, look: Duration) -> Result<Duration, String> {
    let left = until.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err(format!("git did not answer in {}ms", look.as_millis()));
    }
    Ok(left)
}

fn toplevel(folder: &Dir, until: Instant, look: Duration) -> Result<Option<Dir>, String> {
    let mut git = Git::at(folder.as_str(), "rev-parse");
    git.args(["--is-inside-work-tree", "--show-toplevel"]);
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

pub fn clone_of(common: &Directory, look: Duration) -> Result<CloneRoot, String> {
    clone_within(common, Instant::now() + look, look)
}

fn clone_within(common: &Directory, until: Instant, look: Duration) -> Result<CloneRoot, String> {
    let mut git = Git::of(common, "config");
    git.args(["--type=bool", "--get", "core.bare"]);
    let ran = git.run(left(until, look)?);
    let ran = ran.map_err(|error| format!("git config core.bare did not run: {error}"))?;
    Ok(clone_root(Path::new(common.as_str()), bareness(ran)?))
}

fn bareness(ran: Ran) -> Result<Bareness, String> {
    if let Ran::Ended {
        status,
        stdout,
        stderr,
    } = &ran
        && status.code() == Some(1)
        && stdout.is_empty()
        && stderr.is_empty()
    {
        return Ok(Bareness::Worked);
    }
    let said = ran
        .answer()
        .map_err(|why| format!("git config core.bare: {why}"))?;
    match said.as_slice() {
        b"true\n" => Ok(Bareness::Bare),
        b"false\n" => Ok(Bareness::Worked),
        _ => Err(format!(
            "git said core.bare is {:?}",
            String::from_utf8_lossy(&said)
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use lune_domain::projects::Subfolder;

    use super::*;

    fn dir(text: &str) -> Dir {
        Dir::parse(text).unwrap()
    }

    fn subfolder(text: &str) -> Subfolder {
        Subfolder::parse(text).unwrap()
    }

    fn git(dir: &Path, args: &[&str]) {
        let said = git::scratch(dir, args);
        assert!(said.is_ok(), "git {args:?} in {}: {said:?}", dir.display());
    }

    fn real(path: &Path) -> Dir {
        dir(fs::canonicalize(path).unwrap().to_str().unwrap())
    }

    #[test]
    fn a_folder_is_spotted_in_its_clone_s_worktree_in_its_plain_repository_or_alone() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let (clone, plain, alone) = (
            scratch.join("lune"),
            scratch.join("plain"),
            scratch.join("Vault"),
        );
        fs::create_dir_all(clone.join(".bare")).unwrap();
        git(
            &clone.join(".bare"),
            &["init", "-q", "--bare", "-b", "main"],
        );
        fs::write(clone.join(".git"), "gitdir: ./.bare\n").unwrap();
        git(
            &clone,
            &["worktree", "add", "-q", "--orphan", "-b", "main", "main"],
        );
        fs::create_dir_all(clone.join("main/crates")).unwrap();
        fs::create_dir_all(clone.join("notes")).unwrap();
        fs::create_dir_all(&plain).unwrap();
        git(&plain, &["init", "-q", "-b", "main"]);
        fs::create_dir_all(plain.join("src")).unwrap();
        fs::create_dir_all(alone.join("notes")).unwrap();

        let listed = Listed::of(BTreeSet::from([subfolder("main")])).unwrap();
        let clone_at =
            |at: Option<&str>| Spot::in_clone(real(&clone), listed.clone(), at.map(subfolder));
        let of = |folder: &Dir| of(folder, LOOK);
        assert_eq!(of(&real(&clone)), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("notes"))), Ok(clone_at(None)));
        assert_eq!(of(&real(&clone.join("main"))), Ok(clone_at(Some("main"))));
        assert_eq!(
            of(&real(&clone.join("main/crates"))),
            Ok(clone_at(Some("main")))
        );
        let src = real(&plain.join("src"));
        assert_eq!(
            of(&src),
            Ok(Spot::in_folder(src.clone(), Some(real(&plain))))
        );
        let linked = scratch.join("linked");
        std::os::unix::fs::symlink(&plain, &linked).unwrap();
        let through = dir(linked.join("src").to_str().unwrap());
        let alone_there = Spot::in_folder(through.clone(), None);
        assert_eq!(of(&through), Ok(alone_there), "the top git names is real");
        let notes = real(&alone.join("notes"));
        assert_eq!(of(&notes), Ok(Spot::in_folder(notes.clone(), None)));
        assert_eq!(clone_at(Some("main")).place(), real(&clone.join("main")));
        assert_eq!(clone_at(None).dir(), &real(&clone));
        let none_left = super::of(&real(&clone), Duration::ZERO).unwrap_err();
        assert!(none_left.contains("did not answer in 0ms"), "{none_left}");
    }

    #[test]
    fn a_bare_with_a_work_tree_is_no_clone_and_a_clone_lune_cannot_keep_is_refused() {
        let scratch = tempfile::tempdir().unwrap();
        let scratch = fs::canonicalize(scratch.path()).unwrap();
        let (sep, worked) = (scratch.join("sep/.bare"), scratch.join("sep/main"));
        let sep = format!("--separate-git-dir={}", sep.display());
        git(
            &scratch,
            &["init", "-q", "-b", "main", &sep, worked.to_str().unwrap()],
        );
        let worked = real(&worked);
        assert_eq!(
            of(&worked, LOOK),
            Ok(Spot::in_folder(worked.clone(), Some(worked.clone())))
        );

        let unkept = scratch.join("lun\teta");
        fs::create_dir_all(unkept.join(".bare")).unwrap();
        git(
            &unkept.join(".bare"),
            &["init", "-q", "--bare", "-b", "main"],
        );
        fs::write(unkept.join(".git"), "gitdir: ./.bare\n").unwrap();
        let out = scratch.join("out");
        git(
            &unkept,
            &[
                "worktree",
                "add",
                "-q",
                "--orphan",
                "-b",
                "main",
                out.to_str().unwrap(),
            ],
        );
        let refused = of(&real(&out), LOOK).unwrap_err();
        assert!(
            refused.contains("in a clone wt made in a folder lune cannot keep"),
            "{refused}"
        );
    }
}
