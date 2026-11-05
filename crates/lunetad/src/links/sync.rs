//! Brings a worktree to the head of its pull request, before an agent reviews
//! it. It never moves one that holds work of its own.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use super::{ASKING, GITHUB, Link, across, branch, remote_in, said};
use crate::git;
use crate::program::{self, Ran};

const MOVING: Duration = Duration::from_mins(1);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Branches {
    base_ref_name: String,
    head_ref_name: String,
}

struct Upstream {
    name: String,
    tip: String,
}

pub(super) fn to_head(dir: &str, link: &Link) -> Result<(), String> {
    bring(dir, link, &remote_in(dir, link)?, &branches(link)?)
}

/// Asks GitHub, as git cannot tell a pull request's base.
fn branches(link: &Link) -> Result<Branches, String> {
    let number = link.number.to_string();
    let repository = format!("{GITHUB}/{}", link.repository());
    let mut gh = Command::new("gh");
    gh.args(["pr", "view", &number, "--repo", &repository, "--json", "baseRefName,headRefName"]);
    let ran = program::run(&mut gh, ASKING)
        .map_err(|error| format!("gh pr view could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh pr view: {why}"))?;
    serde_json::from_slice(&said).map_err(|error| format!("gh pr view does not read: {error}"))
}

fn bring(dir: &str, link: &Link, remote: &str, branches: &Branches) -> Result<(), String> {
    let tracked = upstream(dir);
    let pull = format!("refs/pull/{}/head", link.number);
    let base = &branches.base_ref_name;
    let base = format!("+refs/heads/{base}:refs/remotes/{remote}/{base}");
    across(dir, "fetch", &["--no-tags", "--", remote, &pull, &base])?;
    let head = commit(dir, "FETCH_HEAD^{commit}")?;
    let here = commit(dir, "HEAD")?;
    let its_own = on_its_branch(dir, link, &branches.head_ref_name);
    if here == head && its_own.is_err() {
        return Ok(());
    }
    its_own?;
    if here != head {
        if !told(dir, "status", &["--porcelain", "--untracked-files=no"])?.is_empty() {
            return Err("it has uncommitted changes".to_owned());
        }
        let rewritten = tracked.as_ref().is_some_and(|tracked| tracked.tip == here);
        if !rewritten && !is_ancestor(dir, &here, &head)? {
            return Err(format!("it has commits {link} does not have"));
        }
        told(dir, "reset", &["--quiet", "--keep", &head])?;
    }
    if let Some(Upstream { name, .. }) = tracked {
        told(dir, "update-ref", &["-m", "luneta: the pull request's head", &name, &head])?;
    }
    Ok(())
}

fn in_dir(dir: &str) -> Command {
    let mut git = Command::new("git");
    git::local(git.args(["--no-optional-locks", "-c", "core.fsmonitor=false", "-C", dir]));
    git
}

fn run(dir: &str, command: &str, args: &[&str]) -> Result<Ran, String> {
    program::run(in_dir(dir).arg(command).args(args), MOVING)
        .map_err(|error| format!("git {command} could not be run: {error}"))
}

fn told(dir: &str, command: &str, args: &[&str]) -> Result<Vec<u8>, String> {
    let ran = run(dir, command, args)?;
    ran.answer().map_err(|why| format!("git {command}: {why}"))
}

fn commit(dir: &str, name: &str) -> Result<String, String> {
    said(in_dir(dir).args(["rev-parse", "--verify", "--quiet", name]))
        .ok_or_else(|| format!("git did not say what {name} is"))
}

fn upstream(dir: &str) -> Option<Upstream> {
    let name = said(in_dir(dir).args(["rev-parse", "--symbolic-full-name", "@{upstream}"]))?;
    let tip = said(in_dir(dir).args(["rev-parse", "--verify", "--quiet", "@{upstream}"]))?;
    Some(Upstream { name, tip })
}

/// A pane may have gone to another branch since the daemon last said its pull
/// request.
fn on_its_branch(dir: &str, link: &Link, head: &str) -> Result<(), String> {
    let checked_out = branch(dir).ok_or("it has no branch checked out")?;
    let merge = format!("branch.{checked_out}.merge");
    let its = match said(in_dir(dir).args(["config", "--get", &merge])) {
        Some(merge) => {
            merge == format!("refs/heads/{head}")
                || merge == format!("refs/pull/{}/head", link.number)
        },
        None => checked_out == head,
    };
    if its {
        Ok(())
    } else {
        Err(format!("it has {checked_out} checked out, and the branch of {link} is {head}"))
    }
}

fn is_ancestor(dir: &str, older: &str, newer: &str) -> Result<bool, String> {
    let ran = run(dir, "merge-base", &["--is-ancestor", older, newer])?;
    if let Ran::Ended { status, .. } = &ran
        && status.code() == Some(1)
    {
        return Ok(false);
    }
    ran.answer().map(|_| true).map_err(|why| format!("git merge-base: {why}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "git {args:?}: {stderr}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    struct Pull {
        _scratch: TempDir,
        up: PathBuf,
        clone: PathBuf,
        worktree: PathBuf,
    }

    impl Pull {
        fn new() -> Pull {
            let scratch = tempfile::tempdir().unwrap();
            let root = scratch.path().canonicalize().unwrap();
            let (up, clone) = (root.join("up"), root.join("n"));
            git(&root, &["init", "-q", "-b", "main", "up"]);
            commit_in(&up, "first");
            git(&up, &["switch", "-q", "-c", "feat"]);
            commit_in(&up, "second");
            git(&up, &["update-ref", "refs/pull/7/head", "feat"]);
            git(&up, &["switch", "-q", "main"]);
            let bare = clone.join(".bare");
            git(&root, &["clone", "-q", "--bare", "up", bare.to_str().unwrap()]);
            git(&bare, &["config", "remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"]);
            git(&bare, &["fetch", "-q", "origin"]);
            git(&bare, &["worktree", "add", "-q", "../feat", "feat"]);
            git(&bare, &["worktree", "add", "-q", "../main", "main"]);
            let worktree = clone.join("feat");
            git(&worktree, &["branch", "-q", "--set-upstream-to=origin/feat", "feat"]);
            Pull { _scratch: scratch, up, clone, worktree }
        }

        fn pushes(&self, name: &str) -> String {
            git(&self.up, &["switch", "-q", "feat"]);
            commit_in(&self.up, name);
            self.moved()
        }

        fn force_pushes(&self, name: &str) -> String {
            git(&self.up, &["switch", "-q", "feat"]);
            git(&self.up, &["reset", "-q", "--hard", "HEAD~1"]);
            commit_in(&self.up, name);
            self.moved()
        }

        fn moved(&self) -> String {
            git(&self.up, &["update-ref", "refs/pull/7/head", "feat"]);
            let head = git(&self.up, &["rev-parse", "feat"]);
            git(&self.up, &["switch", "-q", "main"]);
            head
        }

        fn sync(&self) -> Result<(), String> {
            let branches = Branches {
                base_ref_name: "main".to_owned(),
                head_ref_name: "feat".to_owned(),
            };
            bring(self.worktree.to_str().unwrap(), &link(), "origin", &branches)
        }

        fn here(&self) -> String {
            git(&self.worktree, &["rev-parse", "HEAD"])
        }

        fn tracked(&self) -> String {
            git(&self.worktree, &["rev-parse", "refs/remotes/origin/feat"])
        }
    }

    fn commit_in(dir: &Path, name: &str) {
        fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", name]);
        git(dir, &["commit", "-q", "-m", name]);
    }

    fn link() -> Link {
        Link::parse("https://github.com/o/n/pull/7").unwrap()
    }

    #[test]
    fn at_the_head_it_stays() {
        let pull = Pull::new();
        let head = pull.here();
        pull.sync().unwrap();
        assert_eq!((pull.here(), pull.tracked()), (head.clone(), head));
    }

    /// Nothing has to move, so nothing it holds is in danger.
    #[test]
    fn at_the_head_with_uncommitted_changes_it_stays() {
        let pull = Pull::new();
        let head = pull.here();
        fs::write(pull.worktree.join("second"), "mine").unwrap();
        pull.sync().unwrap();
        assert_eq!(pull.here(), head);
        assert_eq!(fs::read_to_string(pull.worktree.join("second")).unwrap(), "mine");
    }

    #[test]
    fn behind_the_head_it_fast_forwards() {
        let pull = Pull::new();
        let head = pull.pushes("third");
        pull.sync().unwrap();
        assert_eq!((pull.here(), pull.tracked()), (head.clone(), head));
        assert!(pull.worktree.join("third").is_file());
    }

    #[test]
    fn a_rewritten_head_moves_a_tip_that_was_only_the_remote_s() {
        let pull = Pull::new();
        let head = pull.force_pushes("rewritten");
        pull.sync().unwrap();
        assert_eq!((pull.here(), pull.tracked()), (head.clone(), head));
        assert!(!pull.worktree.join("second").exists());
    }

    /// An upstream left behind would not match the tip, and the next force-push
    /// would look like commits of your own.
    #[test]
    fn a_second_force_push_moves_it_again() {
        let pull = Pull::new();
        pull.pushes("third");
        pull.sync().unwrap();
        let head = pull.force_pushes("rewritten");
        pull.sync().unwrap();
        assert_eq!(pull.here(), head);
    }

    #[test]
    fn the_base_moves_on_its_remote_tracking_ref_alone() {
        let pull = Pull::new();
        let local = git(&pull.clone.join("main"), &["rev-parse", "main"]);
        commit_in(&pull.up, "on-main");
        pull.sync().unwrap();
        let tracked = git(&pull.worktree, &["rev-parse", "refs/remotes/origin/main"]);
        assert_eq!(tracked, git(&pull.up, &["rev-parse", "main"]));
        assert_eq!(git(&pull.clone.join("main"), &["rev-parse", "main"]), local);
    }

    #[test]
    fn uncommitted_changes_keep_it_where_it_is() {
        for stage in [false, true] {
            let pull = Pull::new();
            let (was, _) = (pull.here(), pull.pushes("third"));
            fs::write(pull.worktree.join("second"), "mine").unwrap();
            if stage {
                git(&pull.worktree, &["add", "second"]);
            }
            assert_eq!(pull.sync(), Err("it has uncommitted changes".to_owned()));
            assert_eq!(pull.here(), was);
            assert_eq!(fs::read_to_string(pull.worktree.join("second")).unwrap(), "mine");
        }
    }

    #[test]
    fn commits_the_remote_does_not_have_keep_it_where_it_is() {
        let ahead = Pull::new();
        commit_in(&ahead.worktree, "mine");
        let tip = ahead.here();
        let why = "it has commits o/n#7 does not have".to_owned();
        assert_eq!(ahead.sync(), Err(why.clone()));
        assert_eq!(ahead.here(), tip);

        let diverged = Pull::new();
        commit_in(&diverged.worktree, "mine");
        let tip = diverged.here();
        diverged.force_pushes("rewritten");
        assert_eq!(diverged.sync(), Err(why));
        assert_eq!(diverged.here(), tip);
    }

    /// wt's seeded links read as untracked, and must not stop a review.
    #[test]
    fn untracked_files_stop_it_only_where_the_head_would_overwrite_them() {
        let pull = Pull::new();
        fs::write(pull.worktree.join("seeded"), "seeded").unwrap();
        let head = pull.pushes("third");
        pull.sync().unwrap();
        assert_eq!(pull.here(), head);
        assert_eq!(fs::read_to_string(pull.worktree.join("seeded")).unwrap(), "seeded");

        let pull = Pull::new();
        let was = pull.here();
        fs::write(pull.worktree.join("third"), "mine").unwrap();
        pull.pushes("third");
        let why = pull.sync().unwrap_err();
        assert!(why.starts_with("git reset: "), "{why}");
        assert!(why.contains("'third'"), "{why}");
        assert_eq!(pull.here(), was);
        assert_eq!(fs::read_to_string(pull.worktree.join("third")).unwrap(), "mine");
    }

    #[test]
    fn a_failed_fetch_moves_nothing_and_says_why() {
        let pull = Pull::new();
        let was = pull.here();
        git(&pull.up, &["update-ref", "-d", "refs/pull/7/head"]);
        let why = pull.sync().unwrap_err();
        assert!(why.starts_with("git fetch: "), "{why}");
        assert!(why.contains("refs/pull/7/head"), "{why}");
        assert_eq!(pull.here(), was);
    }

    #[test]
    fn another_branch_is_not_moved_to_the_head() {
        let pull = Pull::new();
        pull.pushes("third");
        let main = pull.clone.join("main");
        git(&main, &["branch", "-q", "--set-upstream-to=origin/main", "main"]);
        let was = git(&main, &["rev-parse", "HEAD"]);
        let branches =
            Branches { base_ref_name: "main".to_owned(), head_ref_name: "feat".to_owned() };
        let why = bring(main.to_str().unwrap(), &link(), "origin", &branches).unwrap_err();
        assert_eq!(why, "it has main checked out, and the branch of o/n#7 is feat");
        assert_eq!(git(&main, &["rev-parse", "HEAD"]), was);
    }

    #[test]
    fn another_branch_or_none_at_the_head_stays_and_touches_no_upstream() {
        let pull = Pull::new();
        let head = pull.pushes("third");
        git(&pull.worktree, &["fetch", "-q", "origin", "refs/pull/7/head"]);
        git(&pull.worktree, &["switch", "-q", "--detach", "FETCH_HEAD"]);
        let tracked = pull.tracked();
        pull.sync().unwrap();
        assert_eq!((pull.here(), pull.tracked()), (head.clone(), tracked.clone()));

        git(&pull.worktree, &["switch", "-q", "-c", "other"]);
        git(&pull.worktree, &["branch", "-q", "--set-upstream-to=origin/main", "other"]);
        pull.sync().unwrap();
        assert_eq!((pull.here(), pull.tracked()), (head, tracked));
        let main = git(&pull.worktree, &["rev-parse", "refs/remotes/origin/main"]);
        assert_eq!(main, git(&pull.up, &["rev-parse", "main"]));
    }

    #[test]
    fn with_no_branch_checked_out_behind_the_head_it_stays() {
        let pull = Pull::new();
        let was = pull.here();
        pull.pushes("third");
        git(&pull.worktree, &["switch", "-q", "--detach"]);
        assert_eq!(pull.sync(), Err("it has no branch checked out".to_owned()));
        assert_eq!(pull.here(), was);
    }

    #[test]
    fn with_no_upstream_it_fast_forwards_and_a_rewrite_stops_it() {
        let pull = Pull::new();
        git(&pull.worktree, &["branch", "-q", "--unset-upstream"]);
        let head = pull.pushes("third");
        pull.sync().unwrap();
        assert_eq!(pull.here(), head);

        pull.force_pushes("rewritten");
        assert_eq!(pull.sync(), Err("it has commits o/n#7 does not have".to_owned()));
        assert_eq!(pull.here(), head);
    }

    /// As `gh pr checkout` leaves a fork's pull request.
    #[test]
    fn a_branch_that_merges_from_the_pull_request_s_ref_is_its_own() {
        let pull = Pull::new();
        git(&pull.worktree, &["config", "branch.feat.merge", "refs/pull/7/head"]);
        let head = pull.pushes("third");
        pull.sync().unwrap();
        assert_eq!(pull.here(), head);
    }
}
