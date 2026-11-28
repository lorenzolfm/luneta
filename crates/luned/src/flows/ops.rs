use std::env;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use lune_domain::agent::Harness;
use lune_domain::flows::{
    FixChecksGateFailure, MergeOutcome, PrSnapshot, PrState, RebaseOutcome, StageGateFailure,
    StageKind,
};
use lune_domain::projects::Argv;
use lune_protocol::github::{PullPage, State};

use crate::checks::{self, Failing, Log};
use crate::directory::Directory;
use crate::git::{self, Common};
use crate::github::{self, Pulled};
use crate::program;
use crate::rebases::{self, Came};

const MERGE_LIMIT: Duration = Duration::from_mins(2);

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

pub fn check_rebase_post_conditions(dir: &str, onto_ref: &str) -> Result<String, String> {
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
    previous_head: &str,
) -> Result<String, FixChecksGateFailure> {
    let clean = git::is_clean(dir).map_err(FixChecksGateFailure::Failed)?;
    if !clean {
        return Err(FixChecksGateFailure::DirtyTree);
    }
    let head = git::head_oid(dir).map_err(FixChecksGateFailure::Failed)?;
    if head == previous_head {
        return Err(FixChecksGateFailure::HeadUnchanged);
    }
    Ok(head)
}

pub fn poll_pr(dir: &str) -> Result<Option<PrSnapshot>, String> {
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
    Ok(pulled.map(snapshot_of_pulled))
}

#[must_use]
pub fn snapshot_of_pulled(pulled: Pulled) -> PrSnapshot {
    let state = match pulled.pull_request.state {
        State::Open => PrState::Open,
        State::Draft => PrState::Draft,
        State::Merged => PrState::Merged,
        State::Closed => PrState::Closed,
    };
    let (pending, failed, passed) = pulled
        .pull_request
        .checks
        .map_or((0, 0, 0), |c| (c.pending, c.failed, c.passed));
    PrSnapshot {
        url: pulled.pull_request.page.url().to_owned(),
        state,
        head_oid: pulled.head,
        base_name: pulled.base_name,
        mergeable: pulled.mergeable,
        merge_state: pulled.merge_state,
        pending_checks: pending,
        failed_checks: failed,
        passed_checks: passed,
        stale: pulled.pull_request.stale,
    }
}

#[must_use]
pub fn run_rebase(dir: &str) -> RebaseOutcome {
    let parsed = match Directory::parse(dir.to_owned()) {
        Ok(dir) => dir,
        Err(why) => return RebaseOutcome::Failed(why),
    };
    match rebases::rebase_with_ref(&parsed, None) {
        Ok((rebased, onto_ref)) => match &rebased.came {
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

pub fn fetch_checks_prompt(pr_url: &str) -> Result<String, String> {
    let page = PullPage::parse(pr_url)?;
    let failing = checks::kept_failing(&page)?;
    if failing.failed.is_empty() {
        return Err("no failing checks were found when fetching logs".to_owned());
    }
    Ok(format_checks_prompt(&failing, pr_url))
}

fn format_checks_prompt(failing: &Failing, pr_url: &str) -> String {
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
pub fn merge_pr(dir: &str, pr_url: &str) -> MergeOutcome {
    let mut gh = Command::new("gh");
    let mut args = vec!["pr", "merge"];
    if !pr_url.is_empty() {
        args.push(pr_url);
    }
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
    let base = git::default_branch(dir, Duration::from_secs(10))
        .ok()
        .map_or_else(|| "main".to_owned(), |branch| branch.to_string());
    match pr_url {
        Some(url) if !url.is_empty() => format!("{base}..HEAD ({url})"),
        _ => format!("{base}..HEAD"),
    }
}

#[must_use]
pub fn stage_prompts(
    dir: &str,
    harness: Harness,
    stage_kind: &StageKind,
    implement_prompt: Option<&str>,
    pr_url: Option<&str>,
) -> (String, Option<String>) {
    match stage_kind {
        StageKind::Implement => {
            let user_prompt = implement_prompt.unwrap_or("Implement the requested change.");
            (format!("{user_prompt}\n\n{IMPLEMENT_SUFFIX}"), None)
        }
        StageKind::ReviewSkill { skill } => {
            let spelled = spelled_skill(harness, skill);
            let scope = pr_scope(dir, pr_url);
            let turn1 = format!(
                "{spelled} Review the full PR diff for {scope}. Report your findings and stop \
                 without changing code yet."
            );
            (turn1, Some(REVIEW_FOLLOWUP.to_owned()))
        }
        StageKind::Land => (String::new(), None),
    }
}

pub fn interactive_stage_argv(
    dir: &str,
    harness: Harness,
    stage_kind: &StageKind,
    implement_prompt: Option<&str>,
    pr_url: Option<&str>,
) -> Result<Argv, String> {
    match stage_kind {
        StageKind::Implement => {
            let prompt = implement_prompt.unwrap_or("Implement the requested change.");
            Argv::on_words(harness, prompt.to_owned(), &[])
        }
        StageKind::ReviewSkill { skill } => {
            let spelled = spelled_skill(harness, skill);
            let scope = pr_scope(dir, pr_url);
            let prompt = format!("{spelled} {scope}");
            let words = match harness {
                Harness::ClaudeCode | Harness::Pi => vec![harness.program().to_owned(), prompt],
                Harness::Antigravity => {
                    vec![harness.program().to_owned(), "-i".to_owned(), prompt]
                }
            };
            Argv::parse(words)
        }
        StageKind::Land => Err("land stage has no interactive prompt".to_owned()),
    }
}

pub fn unattended_step_argv(
    lune_bin: &str,
    harness: Harness,
    stage_label: &str,
    prompt_file: &Path,
    followup_file: Option<&Path>,
    pi_session: Option<&Path>,
    session_out: &Path,
    timeout_secs: u64,
) -> Result<Argv, String> {
    let mut words = vec![
        lune_bin.to_owned(),
        "_step".to_owned(),
        "--harness".to_owned(),
        harness.program().to_owned(),
        "--stage".to_owned(),
        stage_label.to_owned(),
        "--prompt-file".to_owned(),
        prompt_file.to_string_lossy().into_owned(),
        "--session-out".to_owned(),
        session_out.to_string_lossy().into_owned(),
        "--timeout-secs".to_owned(),
        timeout_secs.to_string(),
    ];
    if let Some(followup) = followup_file {
        words.push("--followup-file".to_owned());
        words.push(followup.to_string_lossy().into_owned());
    }
    if harness == Harness::Pi
        && let Some(pi_session) = pi_session
    {
        words.push("--pi-session".to_owned());
        words.push(pi_session.to_string_lossy().into_owned());
    }
    Argv::parse(words)
}

pub fn takeover_argv(harness: Harness, session_id: Option<&str>) -> Result<Argv, String> {
    let words = match session_id {
        Some(id) if !id.trim().is_empty() => vec![
            harness.program().to_owned(),
            harness.resumes().to_owned(),
            id.trim().to_owned(),
        ],
        _ => vec![harness.program().to_owned()],
    };
    Argv::parse(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takeover_argv_uses_each_harness_resume_flag() {
        assert_eq!(
            takeover_argv(Harness::ClaudeCode, Some("s1"))
                .unwrap()
                .words(),
            ["claude", "--resume", "s1"]
        );
        assert_eq!(
            takeover_argv(Harness::Antigravity, Some("c1"))
                .unwrap()
                .words(),
            ["agy", "--resume", "c1"]
        );
        assert_eq!(
            takeover_argv(Harness::Pi, Some("/tmp/s.jsonl"))
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
}
