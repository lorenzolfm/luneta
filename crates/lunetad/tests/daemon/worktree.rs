//! `luneta worktree`, which luneta runs to make the worktree of a link pasted
//! in its sidebar, with wt's code: where the worktree is, on stdout, alone,
//! or why it made none, last on stderr.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::host::{Vars, LUNETA};

/// git on the repository at `dir`, with no config but its own, as no one in
/// particular, which must end well.
fn git(dir: &Path, args: &[&str]) {
    let mut git = Command::new("git");
    for repository in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        git.env_remove(repository);
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(git.status().unwrap().success(), "git {args:?} in {}", dir.display());
}

/// `luneta worktree` of `branch`, in the repository `dir` is a directory of.
fn worktree(dir: &Path, branch: &str) -> Output {
    worktree_with(dir, branch, None)
}

/// `luneta worktree` of `branch`, in the repository `dir` is a directory of,
/// with a variable that tells git where a repository is, `named`, where
/// there is one, and none but it.
fn worktree_with(dir: &Path, branch: &str, named: Option<(&str, &Path)>) -> Output {
    let mut luneta = Vars::default().command(LUNETA);
    lunetad::local_git(&mut luneta);
    if let Some((name, repository)) = named {
        luneta.env(name, repository);
    }
    luneta.arg("worktree").arg("--repo").arg(dir).arg(branch).output().unwrap()
}

/// A clone as wt makes one, of a repository it makes beside it, `up`, with
/// the repository bare in `.bare` and `main` checked out beside it, at
/// `clone`.
fn cloned_as_wt(scratch: &Path, up: &Path, clone: &Path) {
    origin(up);
    let (up, bare) = (up.display().to_string(), clone.join(".bare"));
    git(scratch, &["clone", "-q", "--bare", &up, &bare.display().to_string()]);
    git(&bare, &["worktree", "add", "-q", "../main", "main"]);
}

/// A repository with a commit on `main`, and a branch `feat`, at `dir`.
fn origin(dir: &Path) {
    git(dir.parent().unwrap(), &["init", "-q", "-b", "main", &dir.display().to_string()]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(dir, &["branch", "feat"]);
}

/// A worktree is made beside the others, in a clone as wt makes one, with
/// the repository bare in `.bare` and `main` checked out beside it, and
/// where it is is all luneta prints on stdout, whatever wt and git say on
/// stderr as they go. Asked again, it says the same worktree, which has the
/// branch already.
#[test]
fn a_worktree_is_made_beside_the_others_and_said_alone_on_stdout() {
    let scratch = tempfile::tempdir().unwrap();
    // As git says the worktree, by its real path, as on macOS, where the
    // temporary folder is through a link.
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("clone"));
    cloned_as_wt(&scratch, &up, &clone);

    let made = worktree(&clone.join("main"), "feat");
    let stderr = String::from_utf8_lossy(&made.stderr);
    assert!(made.status.success(), "{stderr}");
    assert!(!stderr.is_empty());
    let feat = clone.join("feat");
    assert_eq!(String::from_utf8_lossy(&made.stdout), format!("{}\n", feat.display()));
    let checked_out = fs::read_to_string(feat.join(".git")).unwrap();
    assert!(checked_out.starts_with("gitdir:"), "{checked_out}");

    let again = worktree(&clone.join("main"), "feat");
    assert!(again.status.success(), "{}", String::from_utf8_lossy(&again.stderr));
    assert_eq!(again.stdout, made.stdout);
}

/// In a clone git made, wt would make the worktree in its `.git`: luneta
/// makes none, prints nothing on stdout, and says why on the last line of
/// stderr, after `luneta: `, which is what luneta reads of it.
#[test]
fn in_a_clone_git_made_none_is_made_and_why_comes_last_on_stderr() {
    let scratch = tempfile::tempdir().unwrap();
    let (up, plain) = (scratch.path().join("up"), scratch.path().join("plain"));
    origin(&up);
    git(scratch.path(), &["clone", "-q", &up.display().to_string(), "plain"]);

    let refused = worktree(&plain, "feat");
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let why = format!(
        "luneta: {} is in a clone git made, and wt would make the worktree in its .git; wt clone \
         it first",
        plain.display()
    );
    assert_eq!(String::from_utf8_lossy(&refused.stderr).lines().last(), Some(why.as_str()));
    assert!(!plain.join(".git").join("feat").exists());
}

/// Where a variable tells git where a repository is, wt's git would read the
/// one it names, and the clone luneta sees wt made may not be the one wt
/// makes the worktree in: here, in the `.git` of a clone git made. luneta
/// makes none, and says why last on stderr.
#[test]
fn with_git_told_of_another_repository_none_is_made() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, plain) = (scratch.join("up"), scratch.join("clone"), scratch.join("plain"));
    cloned_as_wt(&scratch, &up, &clone);
    git(&scratch, &["clone", "-q", &up.display().to_string(), "plain"]);

    let main = clone.join("main");
    let refused = worktree_with(&main, "feat", Some(("GIT_DIR", &plain.join(".git"))));
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let why = format!(
        "luneta: GIT_DIR is set, and git would take another repository for {}'s",
        main.display()
    );
    assert_eq!(String::from_utf8_lossy(&refused.stderr).lines().last(), Some(why.as_str()));
    assert!(!plain.join(".git").join("feat").exists());
    assert!(!clone.join("feat").exists());
}

/// An issue's link with no `https://`, which git would take for a branch's
/// name, as wt would make a new branch of, is refused as `luneta worktree`
/// is asked, and no worktree is made.
#[test]
fn a_link_with_no_scheme_is_no_branch_s_name() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("clone"));
    cloned_as_wt(&scratch, &up, &clone);

    let refused = worktree(&clone.join("main"), "github.com/o/n/issues/14");
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    let why = "o/n#14 is an issue: make the worktree of its branch, issue-14";
    assert!(stderr.contains(why), "{stderr}");
    assert!(!clone.join("github.com-o-n-issues-14").exists());
}
