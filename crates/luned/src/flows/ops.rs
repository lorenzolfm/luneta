use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lune_domain::agent::Harness;
use lune_domain::flows::{
    AgentStage, FixChecksGateFailure, FlowEntry, GitOid, MergeMethod, MergeOutcome, PrSnapshot,
    PrState, RebaseOutcome, SessionId, StageGateFailure,
};
use lune_domain::projects::Argv;
use lune_protocol::github::{PullPage, State};

use crate::checks::{self, Failing, Log};
use crate::directory::Directory;
use crate::git::{self, Common};
use crate::github::{self, Pulled};
use crate::log;
use crate::program;
use crate::rebases::{self, Came};

const MERGE_LIMIT: Duration = Duration::from_mins(2);

pub const STEP_FILES_TRANSIENT_MAX: usize = 3;

const DEFAULT_BRANCH_TIMEOUT: Duration = Duration::from_secs(10);

const _: () = assert!(MERGE_LIMIT.as_secs() > 0);
const _: () = assert!(DEFAULT_BRANCH_TIMEOUT.as_secs() > 0);

const REVIEW_FOLLOWUP: &str = "Act on the recommended findings from your audit above. Make the \
    fixes cleanly, run local checks and tests to verify everything passes, commit all changes, \
    and push to the branch. If there were no actionable findings, make no changes and report that \
    the review is clean.";

const IMPLEMENT_SUFFIX: &str = "Implement this end-to-end in this worktree. Run local checks and \
    tests to verify everything passes, commit your changes cleanly, push the branch, and open a \
    pull request with `gh pr create`.";

const MERGE_RACE_MARKERS: [&str; 6] = [
    "base branch was modified",
    "head branch is out of date",
    "not up to date",
    "out-of-date",
    "out of date",
    "merge conflict",
];

#[must_use]
pub fn lune_binary() -> String {
    if let Ok(exe) = env::current_exe() {
        let sibling = exe.with_file_name("lune");
        if sibling.is_file() {
            return sibling.to_string_lossy().into_owned();
        }
    }
    "lune".to_owned()
}

#[must_use]
pub fn spelled_skill(harness: Harness, skill: &str) -> String {
    match harness {
        Harness::ClaudeCode | Harness::Antigravity => format!("/{skill}"),
        Harness::Pi => format!("/skill:{skill}"),
    }
}

pub fn check_stage_post_conditions(
    dir: &str,
    require_open_pr: bool,
) -> Result<(), StageGateFailure> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let clean = git::is_clean(dir).map_err(StageGateFailure::Failed)?;
    if !clean {
        return Err(StageGateFailure::DirtyTree);
    }
    if require_open_pr {
        let pr = poll_pr(dir).map_err(StageGateFailure::Failed)?;
        let open = pr.is_some_and(|pr| matches!(pr.state, PrState::Open | PrState::Draft));
        if !open {
            return Err(StageGateFailure::NoOpenPr);
        }
    }
    Ok(())
}

pub fn check_start(dir: &str, entry: &FlowEntry) -> Result<(), String> {
    let branch = refuse_default_branch(dir)?;
    if *entry == FlowEntry::PullRequest {
        let pr = poll_pr(dir)?;
        let open = pr.is_some_and(|pr| matches!(pr.state, PrState::Open | PrState::Draft));
        if !open {
            return Err(format!(
                "{branch} has no open pull request; give the flow a prompt to implement and \
                 open one"
            ));
        }
    }
    Ok(())
}

pub fn refuse_default_branch(dir: &str) -> Result<String, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let Some(branch) = git::branch(dir)? else {
        return Err(format!("{dir} is on no branch, and a flow lands a branch"));
    };
    let default = git::default_branch(dir, DEFAULT_BRANCH_TIMEOUT)
        .map_err(|why| format!("no telling whether {branch} is the default branch: {why}"))?;
    if default.branch() == branch {
        return Err(format!(
            "{dir} is on {branch}, the default branch; a flow runs on a branch of its own"
        ));
    }
    Ok(branch)
}

pub fn push_if_ahead(dir: &str) -> Result<GitOid, String> {
    refuse_default_branch(dir)?;
    git::push_if_ahead(dir)
}

pub fn push_force_with_lease(dir: &str, expected_head: &GitOid) -> Result<(), String> {
    refuse_default_branch(dir)?;
    git::push_force_with_lease(dir, expected_head)
}

pub fn check_rebase_post_conditions(dir: &str, onto_ref: &str) -> Result<GitOid, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    assert!(!onto_ref.is_empty(), "rebase target ref is non-empty");
    if git::rebase_in_progress(dir)? {
        return Err("rebase state (.git/rebase-merge or rebase-apply) still remains".to_owned());
    }
    if !git::is_ancestor(dir, onto_ref, "HEAD")? {
        return Err(format!(
            "HEAD is not a descendant of {onto_ref} (rebase may have been aborted)"
        ));
    }
    if !git::is_clean(dir)? {
        return Err("working tree is not clean after conflict resolution".to_owned());
    }
    git::head_oid(dir)
}

pub fn check_fix_checks_post_conditions(
    dir: &str,
    previous_head: &GitOid,
) -> Result<GitOid, FixChecksGateFailure> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let clean = git::is_clean(dir).map_err(FixChecksGateFailure::Failed)?;
    if !clean {
        return Err(FixChecksGateFailure::DirtyTree);
    }
    let head = git::head_oid(dir).map_err(FixChecksGateFailure::Failed)?;
    if head == *previous_head {
        return Err(FixChecksGateFailure::HeadUnchanged);
    }
    Ok(head)
}

pub fn poll_pr(dir: &str) -> Result<Option<PrSnapshot>, String> {
    assert!(!dir.is_empty(), "worktree directory is non-empty");
    let Some(branch) = git::branch(dir)? else {
        return Ok(None);
    };
    let repository = match git::common(dir) {
        Common::Of(common) => common,
        Common::NoRepository => return Err(format!("{dir} is in no repository")),
        Common::Gone => return Err(format!("{dir} is gone")),
        Common::BareUnread(bare) => return Err(Common::bare_unread(&bare)),
        Common::Unsaid(why) => return Err(why),
    };
    let pulled = github::Asker::default().pull_of(dir, &repository, &branch)?;
    pulled.map(snapshot_of_pulled).transpose()
}

pub fn snapshot_of_pulled(pulled: Pulled) -> Result<PrSnapshot, String> {
    let (state, checks) = match pulled.pull_request.state {
        State::Open {
            draft: false,
            checks,
            ..
        } => (PrState::Open, checks),
        State::Open {
            draft: true,
            checks,
            ..
        } => (PrState::Draft, checks),
        State::Merged => (PrState::Merged, None),
        State::Closed => (PrState::Closed, None),
    };
    let (pending, failed, passed) = checks.map_or((0, 0, 0), |c| (c.pending, c.failed, c.passed));
    let head_oid = GitOid::parse(&pulled.head)
        .map_err(|why| format!("{}: head {why}", pulled.pull_request.page.url()))?;
    Ok(PrSnapshot {
        url: pulled.pull_request.page,
        state,
        head_oid,
        base_name: pulled.base_name,
        mergeable: pulled.mergeable,
        merge_state: pulled.merge_state,
        pending_checks: pending,
        failed_checks: failed,
        passed_checks: passed,
    })
}

#[must_use]
pub fn run_rebase(dir: &str) -> RebaseOutcome {
    let parsed = match Directory::parse(dir.to_owned()) {
        Ok(dir) => dir,
        Err(why) => return RebaseOutcome::Failed(why),
    };
    match rebases::rebase_naming_onto(&parsed, None) {
        Ok(rebases::RebasedOnto { rebased, onto_ref }) => match &rebased.came {
            Came::Already => RebaseOutcome::Already,
            Came::Clean { .. } => match git::head_oid(dir) {
                Ok(new_head) => RebaseOutcome::Clean { new_head },
                Err(why) => RebaseOutcome::Failed(why),
            },
            Came::Stopped(stopped) => {
                let base_prompt =
                    rebases::prompt(rebased.branch.as_str(), rebased.onto.as_str(), stopped);
                let prompt = format!(
                    "{base_prompt}\n\nResolve every conflict, run the local tests and checks to \
                     verify, and complete the rebase with `git rebase --continue`. Leave the \
                     working tree clean and do not push."
                );
                RebaseOutcome::Stopped {
                    onto: onto_ref,
                    prompt,
                }
            }
        },
        Err(why) => RebaseOutcome::Failed(why.to_string()),
    }
}

pub fn fetch_checks_prompt(page: &PullPage) -> Result<String, String> {
    let failing = checks::kept_failing(page)?;
    if failing.failed.is_empty() {
        return Err("no failing checks were found when fetching logs".to_owned());
    }
    Ok(format_checks_prompt(&failing, page.url()))
}

fn format_checks_prompt(failing: &Failing, pr_url: &str) -> String {
    assert!(
        !failing.failed.is_empty(),
        "failing checks list is non-empty"
    );
    assert!(!pr_url.is_empty(), "PR URL is non-empty");
    let mut lines = vec![format!(
        "Fix the failing CI checks on {pr_url} (commit {}).",
        failing.head
    )];
    lines.push(String::new());
    lines.push("Failing checks:".to_owned());
    for check in &failing.failed {
        let log_note = match &check.log {
            Log::Kept(kept) => format!("log saved at {}", kept.file),
            Log::Unkept(why) => format!("log unavailable: {why}"),
        };
        match check.link.as_deref() {
            Some(link) if !link.is_empty() => {
                lines.push(format!("- {} ({link}): {log_note}", check.name));
            }
            _ => {
                lines.push(format!("- {}: {log_note}", check.name));
            }
        }
    }
    lines.push(String::new());
    lines.push(
        "Inspect the failing check logs, fix the root causes, verify locally with tests and \
         linters, commit your changes cleanly, and push to the branch."
            .to_owned(),
    );
    lines.join("\n")
}

#[must_use]
pub fn merge_pr(
    dir: &str,
    page: &PullPage,
    merge_method: MergeMethod,
    expected_head: &GitOid,
) -> MergeOutcome {
    assert!(!dir.is_empty(), "merge directory is non-empty");
    let mut gh = Command::new("gh");
    let args = [
        "pr",
        "merge",
        merge_method.gh_flag(),
        "--match-head-commit",
        expected_head.as_str(),
        page.url(),
    ];
    git::local(gh.args(args).current_dir(dir));
    let ran = match program::run(&mut gh, MERGE_LIMIT) {
        Ok(ran) => ran,
        Err(error) => {
            return MergeOutcome::Failed(format!("gh pr merge could not be run: {error}"));
        }
    };
    match ran.answer() {
        Ok(_) => MergeOutcome::Merged,
        Err(why) => {
            if is_merge_race(&why) {
                MergeOutcome::BaseUpdatedRace
            } else {
                MergeOutcome::Failed(format!("gh pr merge: {why}"))
            }
        }
    }
}

#[must_use]
pub fn is_merge_race(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    MERGE_RACE_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

#[must_use]
pub fn pr_scope(dir: &str, pr_url: Option<&str>) -> String {
    assert!(!dir.is_empty(), "scope directory is non-empty");
    let base = match git::default_branch(dir, DEFAULT_BRANCH_TIMEOUT) {
        Ok(branch) => branch.to_string(),
        Err(why) => {
            log::say(&format!(
                "flows pr_scope {dir}: default branch unreadable ({why}); falling back to main"
            ));
            "main".to_owned()
        }
    };
    match pr_url {
        Some(url) if !url.is_empty() => format!("{base}..HEAD ({url})"),
        _ => format!("{base}..HEAD"),
    }
}

#[must_use]
pub fn stage_prompts(
    dir: &str,
    harness: Harness,
    stage: &AgentStage,
    pr_url: Option<&PullPage>,
) -> (String, Option<String>) {
    match stage {
        AgentStage::Implement { prompt } => {
            assert_ne!(prompt.trim(), "");
            (format!("{prompt}\n\n{IMPLEMENT_SUFFIX}"), None)
        }
        AgentStage::ReviewSkill { skill } => {
            assert_ne!(skill.trim(), "");
            let spelled = spelled_skill(harness, skill);
            let scope = pr_scope(dir, pr_url.map(PullPage::url));
            let turn1 = format!(
                "{spelled} Review the full PR diff for {scope}. Report your findings and stop \
                 without changing code yet."
            );
            (turn1, Some(REVIEW_FOLLOWUP.to_owned()))
        }
    }
}

pub fn interactive_stage_argv(
    dir: &str,
    harness: Harness,
    stage: &AgentStage,
    pr_url: Option<&PullPage>,
) -> Result<Argv, String> {
    match stage {
        AgentStage::Implement { prompt } => {
            assert_ne!(prompt.trim(), "");
            Argv::on_words(harness, prompt.clone(), &[])
        }
        AgentStage::ReviewSkill { skill } => {
            assert_ne!(skill.trim(), "");
            let spelled = spelled_skill(harness, skill);
            let scope = pr_scope(dir, pr_url.map(PullPage::url));
            let prompt = format!("{spelled} {scope}");
            let words = match harness {
                Harness::ClaudeCode | Harness::Pi => vec![harness.program().to_owned(), prompt],
                Harness::Antigravity => {
                    vec![harness.program().to_owned(), "-i".to_owned(), prompt]
                }
            };
            Argv::parse(words)
        }
    }
}

pub struct StepFiles {
    pub prompt: PathBuf,
    pub followup: Option<PathBuf>,
    pub pi_session: Option<PathBuf>,
    pub session_out: PathBuf,
    pub stop: PathBuf,
}

impl StepFiles {
    #[must_use]
    pub fn transient(&self) -> Vec<PathBuf> {
        let mut files = vec![self.prompt.clone(), self.session_out.clone()];
        files.extend(self.followup.clone());
        assert!(files.len() <= STEP_FILES_TRANSIENT_MAX);
        files
    }
}

pub fn unattended_step_argv(
    lune_bin: &str,
    harness: Harness,
    stage_label: &str,
    files: &StepFiles,
    timeout_secs: u64,
) -> Result<Argv, String> {
    assert!(!lune_bin.is_empty(), "lune binary path is non-empty");
    assert!(!stage_label.trim().is_empty(), "stage label is non-empty");
    let path = |path: &Path| -> Result<String, String> {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{} is not UTF-8", path.display()))
    };
    let mut words = vec![
        lune_bin.to_owned(),
        "_step".to_owned(),
        "--harness".to_owned(),
        harness.program().to_owned(),
        "--stage".to_owned(),
        stage_label.to_owned(),
        "--prompt-file".to_owned(),
        path(&files.prompt)?,
        "--session-out".to_owned(),
        path(&files.session_out)?,
        "--stop-file".to_owned(),
        path(&files.stop)?,
        "--timeout-secs".to_owned(),
        timeout_secs.to_string(),
    ];
    if let Some(followup) = &files.followup {
        words.push("--followup-file".to_owned());
        words.push(path(followup)?);
    }
    if harness == Harness::Pi
        && let Some(pi_session) = &files.pi_session
    {
        words.push("--pi-session".to_owned());
        words.push(path(pi_session)?);
    }
    Argv::parse(words)
}

pub fn takeover_argv(harness: Harness, session_id: Option<&SessionId>) -> Result<Argv, String> {
    let words = match session_id {
        Some(id) => vec![
            harness.program().to_owned(),
            harness.resumes().to_owned(),
            id.as_str().to_owned(),
        ],
        None => vec![harness.program().to_owned()],
    };
    Argv::parse(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_argv_uses_each_harness_resume_flag() {
        assert_eq!(
            takeover_argv(Harness::ClaudeCode, Some(&SessionId::parse("s1").unwrap()))
                .unwrap()
                .words(),
            ["claude", "--resume", "s1"]
        );
        assert_eq!(
            takeover_argv(Harness::Antigravity, Some(&SessionId::parse("c1").unwrap()))
                .unwrap()
                .words(),
            ["agy", "--resume", "c1"]
        );
        assert_eq!(
            takeover_argv(
                Harness::Pi,
                Some(&SessionId::parse("/tmp/s.jsonl").unwrap())
            )
            .unwrap()
            .words(),
            ["pi", "--session", "/tmp/s.jsonl"]
        );
    }

    #[test]
    fn detects_merge_race_errors_distinct_from_normal_failures() {
        assert!(is_merge_race(
            "GraphQL: Base branch was modified. Review and try the merge again."
        ));
        assert!(is_merge_race("Pull request head branch is out of date"));
        assert!(!is_merge_race("Pull request is not approved"));
    }

    #[test]
    fn a_flow_refuses_the_default_branch_and_takes_a_branch_of_its_own() {
        let scratch_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(scratch_dir.path()).unwrap();
        let (bare, work) = (root.join("origin.git"), root.join("work"));
        std::fs::create_dir(&bare).unwrap();
        git::scratch(&bare, &["init", "--bare", "-q", "-b", "main"]).unwrap();
        let (bare_str, work_str) = (bare.to_str().unwrap(), work.to_str().unwrap());
        git::scratch(&root, &["clone", "-q", bare_str, work_str]).unwrap();
        git::scratch(&work, &["commit", "-q", "--allow-empty", "-m", "initial"]).unwrap();
        git::scratch(&work, &["push", "-q", "-u", "origin", "main"]).unwrap();

        let refused = refuse_default_branch(work_str).unwrap_err();
        assert!(refused.contains("default branch"), "{refused}");
        assert!(push_if_ahead(work_str).is_err(), "Land never pushes main");

        git::scratch(&work, &["switch", "-q", "-c", "feat"]).unwrap();
        assert_eq!(refuse_default_branch(work_str).unwrap(), "feat");
        let implement = FlowEntry::Implement {
            prompt: "add a feature".to_owned(),
        };
        check_start(work_str, &implement).unwrap();
    }
}
