use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::api::rebase_response::{self, Came as Said};
use crate::api::{On, RebaseRequest, RebaseResponse};
use crate::directory::Directory;
use crate::git::{self, DefaultBranch};
use crate::links::sync;
use crate::program::{self, Ran};

pub const REBASING: Duration = Duration::from_mins(20);

const PICKING: Duration = Duration::from_mins(10);

const CONFLICTED_SAID: usize = 20;

const UNDER_WAY: [(&str, &str); 5] = [
    ("rebase-merge", "a rebase"),
    ("rebase-apply", "a rebase"),
    ("MERGE_HEAD", "a merge"),
    ("CHERRY_PICK_HEAD", "a cherry-pick"),
    ("REVERT_HEAD", "a revert"),
];

const _: () = assert!(PICKING.as_secs() + 5 * 60 < REBASING.as_secs());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rebased {
    pub branch: String,
    pub onto: String,
    pub came: Came,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Came {
    Already,
    Clean { ahead: u32 },
    Stopped(Stopped),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stopped {
    pub at: Option<String>,
    pub conflicted: Vec<String>,
}

pub fn rebase(dir: &Directory) -> Result<Rebased, String> {
    let top = git::says(dir.as_str(), &["rev-parse", "--show-toplevel"])?;
    if top.is_empty() {
        return Err(format!("{dir} is in no worktree of a repository"));
    }
    if let Some(doing) = under_way(&top)? {
        return Err(format!("{top} is in the middle of {doing}: finish it, or abort it, first"));
    }
    let Some(branch) = git::branch(&top) else {
        return Err(format!("{top} is on no branch, and a rebase moves a branch"));
    };
    if !committed(&top)? {
        return Err(format!("{top} has changes not committed: commit or stash them first"));
    }
    let Some(default) = git::default_branch(&top)? else {
        return Err(format!(
            "{top} has no default branch to rebase onto: no HEAD, main or master of upstream, \
             github or origin, and no main or master of its own"
        ));
    };
    if let DefaultBranch::Remote { remote, branch } = &default {
        sync::fetch(&top, remote, branch)?;
    }
    let (onto, reference) = (default.to_string(), default.reference());
    let before = head(&top)?;
    let came = match picked(&top, &reference)? {
        Ok(()) if head(&top)? == before => Came::Already,
        Ok(()) => Came::Clean { ahead: ahead(&top, &reference)? },
        Err(why) => {
            if under_way(&top)?.is_none() {
                return Err(format!("git rebase: {why}"));
            }
            Came::Stopped(Stopped { at: stopped_at(&top), conflicted: conflicted(&top)? })
        },
    };
    Ok(Rebased { branch, onto, came })
}

fn under_way(top: &str) -> Result<Option<&'static str>, String> {
    let mut asked = vec!["rev-parse"];
    for (path, _) in UNDER_WAY {
        asked.extend(["--git-path", path]);
    }
    let said = git::says(top, &asked)?;
    let paths: Vec<&str> = said.lines().collect();
    if paths.len() != UNDER_WAY.len() {
        return Err(format!("git rev-parse said {} paths of {}", paths.len(), UNDER_WAY.len()));
    }
    let found = paths.iter().zip(UNDER_WAY).find(|(path, _)| Path::new(top).join(path).exists());
    Ok(found.map(|(_, (_, doing))| doing))
}

fn committed(top: &str) -> Result<bool, String> {
    let asked = ["--no-optional-locks", "status", "--porcelain", "--untracked-files=no"];
    let said = git::says(top, &[asked.as_slice(), &["--ignore-submodules"]].concat())?;
    Ok(said.is_empty())
}

fn head(top: &str) -> Result<String, String> {
    git::says(top, &["rev-parse", "--verify", "HEAD"])
}

fn ahead(top: &str, reference: &str) -> Result<u32, String> {
    let said = git::says(top, &["rev-list", "--count", &format!("{reference}..HEAD")])?;
    said.parse().map_err(|_| format!("git rev-list counted {said}, which is no count"))
}

fn picked(top: &str, onto: &str) -> Result<Result<(), String>, String> {
    let mut git = Command::new("git");
    git::local(git.args(["-C", top, "rebase", onto]))
        .env("GIT_EDITOR", ":")
        .env("GIT_SEQUENCE_EDITOR", ":");
    let ran = program::run_alone(&mut git, PICKING)
        .map_err(|error| format!("git rebase could not be run: {error}"))?;
    match ran {
        Ran::Stuck => Err(format!(
            "git rebase did not end in {} minutes, and was stopped; git status says where it is",
            PICKING.as_secs() / 60
        )),
        ran @ Ran::Ended { .. } => Ok(ran.answer().map(drop)),
    }
}

fn stopped_at(top: &str) -> Option<String> {
    let asked = ["log", "-1", "--no-show-signature", "--format=%h %s", "REBASE_HEAD"];
    git::says(top, &asked).ok().filter(|said| !said.is_empty())
}

fn conflicted(top: &str) -> Result<Vec<String>, String> {
    let said = git::says(top, &["ls-files", "--unmerged", "-z"])?;
    let mut paths: Vec<String> = said
        .split('\0')
        .filter_map(|entry| entry.split_once('\t').map(|(_, path)| path.to_owned()))
        .collect();
    paths.dedup();
    Ok(paths)
}

#[must_use]
pub fn prompt(branch: &str, onto: &str, stopped: &Stopped) -> String {
    let at = stopped.at.as_ref().map(|at| format!(" at {at}"));
    let conflicted = match stopped.conflicted.as_slice() {
        [] => None,
        files => Some(format!(" on conflicts in {}", listed(files))),
    };
    let stopped = [at, conflicted].into_iter().flatten().collect::<Vec<_>>().join(",");
    format!(
        "The rebase of {branch} onto {onto} stopped{stopped}. Resolve it: keep what \
         {branch} changes, on top of what {onto} brought, `git add` what you resolve, and run \
         `git rebase --continue`, resolving each commit that stops the same way, until the rebase \
         ends. Then check that it still builds. Push nothing, and run no `git rebase --abort` \
         unless I ask for it."
    )
}

fn listed(files: &[String]) -> String {
    let shown: Vec<&str> = files.iter().take(CONFLICTED_SAID).map(String::as_str).collect();
    match files.len() - shown.len() {
        0 => shown.join(", "),
        more => format!("{}, and {more} more", shown.join(", ")),
    }
}

#[must_use]
pub fn asked(on: Option<On>, dir: &Directory) -> RebaseRequest {
    RebaseRequest { on, dir: dir.as_str().to_owned() }
}

impl Rebased {
    #[must_use]
    pub fn to_api(&self) -> RebaseResponse {
        let came = match &self.came {
            Came::Already => Said::Already(()),
            Came::Clean { ahead } => Said::Clean(rebase_response::Clean { ahead: *ahead }),
            Came::Stopped(Stopped { at, conflicted }) => {
                let (at, conflicted) = (at.clone(), conflicted.clone());
                Said::Stopped(rebase_response::Stopped { at, conflicted })
            },
        };
        RebaseResponse { branch: self.branch.clone(), onto: self.onto.clone(), came: Some(came) }
    }

    pub fn of_api(said: RebaseResponse) -> Result<Rebased, String> {
        let came = match said.came.ok_or("lunetad said nothing of how the rebase came out")? {
            Said::Already(()) => Came::Already,
            Said::Clean(rebase_response::Clean { ahead }) => Came::Clean { ahead },
            Said::Stopped(rebase_response::Stopped { at, conflicted }) => {
                Came::Stopped(Stopped { at, conflicted })
            },
        };
        Ok(Rebased { branch: said.branch, onto: said.onto, came })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut git = Command::new("git");
        git::local(&mut git)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .args(["-c", "maintenance.auto=false", "-c", "init.defaultBranch=main"])
            .arg("-C")
            .arg(dir)
            .args(args);
        let ran = git.output().unwrap();
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "git {args:?} in {}: {stderr}", dir.display());
        String::from_utf8(ran.stdout).unwrap().trim_end().to_owned()
    }

    fn commit(dir: &Path, file: &str, text: &str, subject: &str) -> String {
        fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "-q", "-m", subject]);
        git(dir, &["rev-parse", "--short", "HEAD"])
    }

    fn own_config(dir: &Path) {
        let config = [
            ("user.name", "t"),
            ("user.email", "t@t"),
            ("commit.gpgsign", "false"),
            ("core.hooksPath", "/dev/null"),
            ("core.fsmonitor", "false"),
        ];
        for (key, value) in config {
            git(dir, &["config", key, value]);
        }
    }

    fn directory(dir: &Path) -> Directory {
        Directory::parse(dir.to_str().unwrap().to_owned()).unwrap()
    }

    struct Cloned {
        _scratch: tempfile::TempDir,
        up: PathBuf,
        dir: PathBuf,
    }

    impl Cloned {
        fn new() -> Cloned {
            let scratch = tempfile::tempdir().unwrap();
            let root = fs::canonicalize(scratch.path()).unwrap();
            let (up, dir) = (root.join("up"), root.join("clone"));
            fs::create_dir(&up).unwrap();
            git(&up, &["init", "-q", "-b", "main"]);
            commit(&up, "base", "a\nb\nc\n", "base");
            git(&root, &["clone", "-q", up.to_str().unwrap(), dir.to_str().unwrap()]);
            own_config(&dir);
            git(&dir, &["switch", "-q", "-c", "issue-429"]);
            Cloned { _scratch: scratch, up, dir }
        }

        fn head(&self, name: &str) -> String {
            git(&self.dir, &["rev-parse", name])
        }
    }

    #[test]
    fn a_branch_is_rebased_onto_the_default_branch_as_its_remote_has_it_now() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        let rebased = rebase(&directory(&cloned.dir)).unwrap();
        let onto = ("issue-429".to_owned(), "origin/main".to_owned());
        assert_eq!((rebased.branch, rebased.onto), onto);
        assert_eq!(rebased.came, Came::Clean { ahead: 1 });
        let up = git(&cloned.up, &["rev-parse", "main"]);
        assert_eq!(cloned.head("HEAD~"), up, "the default branch is fetched first");

        assert_eq!(rebase(&directory(&cloned.dir)).unwrap().came, Came::Already);
        let docs = cloned.dir.join("docs");
        fs::create_dir(&docs).unwrap();
        assert_eq!(rebase(&directory(&docs)).unwrap().came, Came::Already, "from a folder below the top");
    }

    #[test]
    fn a_rebase_that_stops_on_conflicts_is_left_stopped_and_says_where() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        let picked = commit(&cloned.dir, "base", "a\nB\nc\n", "own base");
        commit(&cloned.up, "base", "a\nb2\nc\n", "their base");
        let rebased = rebase(&directory(&cloned.dir)).unwrap();
        let Came::Stopped(stopped) = &rebased.came else {
            panic!("the rebase ran through: {rebased:?}");
        };
        let at = Some(format!("{picked} own base"));
        assert_eq!(stopped, &Stopped { at, conflicted: vec!["base".to_owned()] });
        assert!(cloned.dir.join(".git/rebase-merge").is_dir(), "the rebase waits, stopped");

        let refused = rebase(&directory(&cloned.dir)).unwrap_err();
        let doing = "is in the middle of a rebase: finish it, or abort it, first";
        assert!(refused.ends_with(doing), "{refused}");
        let words = prompt(&rebased.branch, &rebased.onto, stopped);
        let starts = format!(
            "The rebase of issue-429 onto origin/main stopped at {picked} own base, on conflicts \
             in base. Resolve it: "
        );
        assert!(words.starts_with(&starts), "{words}");
    }

    #[test]
    fn changes_not_committed_and_no_branch_are_refused_before_anything_moves() {
        let cloned = Cloned::new();
        commit(&cloned.dir, "own", "o\n", "own");
        let tip = cloned.head("HEAD");
        commit(&cloned.up, "theirs", "t\n", "theirs");
        fs::write(cloned.dir.join("own"), "changed\n").unwrap();
        let refused = rebase(&directory(&cloned.dir)).unwrap_err();
        assert!(refused.ends_with("has changes not committed: commit or stash them first"));
        assert_eq!(cloned.head("HEAD"), tip);
        assert_eq!(cloned.head("origin/main"), cloned.head("main"), "nothing is fetched");

        git(&cloned.dir, &["checkout", "-q", "own"]);
        fs::write(cloned.dir.join("loose"), "untracked\n").unwrap();
        assert_eq!(rebase(&directory(&cloned.dir)).unwrap().came, Came::Clean { ahead: 1 });

        git(&cloned.dir, &["switch", "-q", "--detach"]);
        let refused = rebase(&directory(&cloned.dir)).unwrap_err();
        assert!(refused.ends_with("is on no branch, and a rebase moves a branch"), "{refused}");
    }

    #[test]
    fn a_repository_with_no_default_branch_is_not_rebased() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = fs::canonicalize(scratch.path()).unwrap();
        git(&dir, &["init", "-q", "-b", "trunk"]);
        own_config(&dir);
        commit(&dir, "base", "a\n", "base");
        let refused = rebase(&directory(&dir)).unwrap_err();
        assert!(refused.contains("has no default branch to rebase onto"), "{refused}");
    }

    #[test]
    fn a_prompt_names_at_most_twenty_files() {
        let conflicted: Vec<String> = (1..=22).map(|n| format!("f{n}")).collect();
        let words = prompt("b", "origin/main", &Stopped { at: None, conflicted });
        assert!(words.starts_with("The rebase of b onto origin/main stopped on conflicts in f1, "));
        assert!(words.contains(", f20, and 2 more. Resolve it: "), "{words}");
        let stopped = Stopped { at: Some("abc x".to_owned()), conflicted: Vec::new() };
        let words = prompt("b", "main", &stopped);
        assert!(words.starts_with("The rebase of b onto main stopped at abc x. Resolve it: "));
    }

    #[test]
    fn what_a_rebase_came_to_says_the_same_over_the_wire() {
        let stopped = Stopped { at: Some("abc x".to_owned()), conflicted: vec!["a".to_owned()] };
        for came in [Came::Already, Came::Clean { ahead: 3 }, Came::Stopped(stopped)] {
            let rebased = Rebased { branch: "b".to_owned(), onto: "origin/main".to_owned(), came };
            assert_eq!(Rebased::of_api(rebased.to_api()), Ok(rebased));
        }
        let unsaid = RebaseResponse { branch: "b".to_owned(), onto: "main".to_owned(), came: None };
        assert!(Rebased::of_api(unsaid).is_err());
    }
}
