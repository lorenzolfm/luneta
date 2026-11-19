use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use crate::host::{Vars, LUNETA};

pub fn git(dir: &Path, args: &[&str]) {
    let mut git = Command::new("git");
    for repository in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        git.env_remove(repository);
    }
    git.env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(["-c", "maintenance.auto=false"])
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    assert!(git.status().unwrap().success(), "git {args:?} in {}", dir.display());
}

fn worktree(dir: &Path, branch: &str) -> Output {
    worktree_with(dir, &[branch], None)
}

fn worktree_with(dir: &Path, words: &[&str], named: Option<(&str, &Path)>) -> Output {
    let mut luneta = Vars::default().command(LUNETA);
    lunetad::local_git(&mut luneta);
    if let Some((name, repository)) = named {
        luneta.env(name, repository);
    }
    luneta.arg("worktree").arg("--repo").arg(dir).args(words).output().unwrap()
}

pub fn cloned_as_wt(scratch: &Path, up: &Path, clone: &Path) {
    origin(up);
    let (up, bare) = (up.display().to_string(), clone.join(".bare"));
    git(scratch, &["clone", "-q", "--bare", &up, &bare.display().to_string()]);
    fs::write(clone.join(".git"), "gitdir: ./.bare\n").unwrap();
    git(&bare, &["worktree", "add", "-q", "../main", "main"]);
}

pub fn worktree_added(clone: &Path, name: &str) {
    git(&clone.join(".bare"), &["worktree", "add", "-q", "-b", name, &format!("../{name}")]);
}

fn origin(dir: &Path) {
    git(dir.parent().unwrap(), &["init", "-q", "-b", "main", &dir.display().to_string()]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "one"]);
    git(dir, &["branch", "feat"]);
}

#[test]
fn a_worktree_is_made_beside_the_others_and_said_alone_on_stdout() {
    let scratch = tempfile::tempdir().unwrap();
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

#[test]
fn who_asks_for_a_branch_gets_no_worktree_that_moved_off_it() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone) = (scratch.join("up"), scratch.join("clone"));
    cloned_as_wt(&scratch, &up, &clone);
    git(&clone.join(".bare"), &["worktree", "add", "-q", "../issue-7", "feat"]);

    let refused = worktree(&clone.join("main"), "issue-7");
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let why = format!("luneta: wt add: {} is already present", clone.join("issue-7").display());
    assert_eq!(String::from_utf8_lossy(&refused.stderr).lines().last(), Some(why.as_str()));
}

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

#[test]
fn in_a_bare_repository_that_is_not_a_dot_bare_none_is_made_and_why_comes_last_on_stderr() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, bare) = (scratch.join("up"), scratch.join("infra.git"));
    origin(&up);
    git(&scratch, &["clone", "-q", "--bare", &up.display().to_string(), "infra.git"]);

    let refused = worktree(&bare, "feat");
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let why = format!(
        "luneta: {} is in a bare repository that is not a .bare, and wt would make the worktree \
         inside it; wt clone it first",
        bare.display()
    );
    assert_eq!(String::from_utf8_lossy(&refused.stderr).lines().last(), Some(why.as_str()));
    assert!(!bare.join("feat").exists());
}

#[test]
fn with_git_told_of_another_repository_none_is_made() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, plain) = (scratch.join("up"), scratch.join("clone"), scratch.join("plain"));
    cloned_as_wt(&scratch, &up, &clone);
    git(&scratch, &["clone", "-q", &up.display().to_string(), "plain"]);

    let main = clone.join("main");
    let refused = worktree_with(&main, &["feat"], Some(("GIT_DIR", &plain.join(".git"))));
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

#[test]
fn with_git_told_of_another_repository_nothing_follows() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, plain) = (scratch.join("up"), scratch.join("clone"), scratch.join("plain"));
    cloned_as_wt(&scratch, &up, &clone);
    git(&scratch, &["clone", "-q", &up.display().to_string(), "plain"]);

    let main = clone.join("main");
    let mut luneta = Vars::default().command(LUNETA);
    lunetad::local_git(&mut luneta);
    let refused =
        luneta.env("GIT_DIR", plain.join(".git")).arg("follow").arg(&main).output().unwrap();
    assert!(!refused.status.success());
    let why = format!(
        "luneta: GIT_DIR is set, and git would take another repository for {}'s",
        main.display()
    );
    assert_eq!(String::from_utf8_lossy(&refused.stderr).lines().last(), Some(why.as_str()));
}

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

#[test]
fn a_name_with_a_dash_first_is_no_branch_s_and_runs_nothing() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch = fs::canonicalize(scratch.path()).unwrap();
    let (up, clone, ran) = (scratch.join("up"), scratch.join("clone"), scratch.join("ran"));
    cloned_as_wt(&scratch, &up, &clone);

    let option = format!("--upload-pack=touch {}", ran.display());
    let refused = worktree_with(&clone.join("main"), &["--", &option], None);
    assert!(!refused.status.success());
    assert_eq!(refused.stdout, b"");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!ran.exists(), "{stderr}");
    let why = format!("luneta: wt add: {option} is not a valid branch name");
    assert_eq!(stderr.lines().last(), Some(why.as_str()));
}
