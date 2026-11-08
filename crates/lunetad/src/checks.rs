//! The checks of a pull request's head, as gh says them with the pull
//! request: how many have yet to end, failed, and did not, for the mark the
//! sidebar shows beside its number, and the log of each that failed, for the
//! agent luneta's menu starts on them with `c c`, kept in a file of its own on
//! the machine the pane is on. luneta keeps them so on its own machine; a
//! daemon keeps them here for a luneta on another machine, whose menu was for a
//! pane of this one.
//!
//! GitHub lists a pull request's checks as runs of checks, as GitHub Actions
//! makes one of each job, and statuses, as other services set them. gh fetches
//! the log of a job of GitHub Actions alone, of the steps that failed.

use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

use luneta_protocol::github::Checks;
use serde::Deserialize;

use crate::printed::Kept;
use crate::{folder, git, keeps, program};

/// The folder the logs are kept in, in luneta's.
pub(crate) const FOLDER: &str = "checks";

/// How long gh has to say: it asks GitHub, across the network, and a log
/// comes down whole before gh says the failed steps of it.
const LIMIT: Duration = Duration::from_secs(30);

/// How long `kept_failing` takes at the most: the logs come down side by
/// side, after the pull request, each in `LIMIT`, and a moment to write them.
pub const FETCHING: Duration = Duration::from_secs(2 * LIMIT.as_secs() + 5);

/// The most lines a file keeps of a log: the last, which say how it failed.
/// Claude Code reads as many at once.
const TAIL: usize = 2000;

/// The most logs fetched for one ask: of a pull request with more that
/// failed, the agent is told the names of the rest alone.
const LOGS_MAX: usize = 8;

/// Why a check that failed has no log kept, where it is no job's, and where
/// what fetched it panicked, as the log says.
const NO_JOB: &str = "it is no job of GitHub Actions, whose log gh fetches";
const PANICKED: &str = "what fetched its log panicked";

/// A check as gh writes it in `statusCheckRollup`: a run of a check, or a
/// status, or what GitHub has added since, which counts for nothing.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Rolled {
    #[serde(rename_all = "camelCase")]
    CheckRun {
        name: String,
        #[serde(default)]
        workflow_name: Option<String>,
        status: String,
        #[serde(default)]
        conclusion: Option<String>,
        #[serde(default)]
        details_url: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    StatusContext {
        context: String,
        state: String,
        #[serde(default)]
        target_url: Option<String>,
    },
    #[serde(other)]
    Other,
}

/// Where a check stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stands {
    Pending,
    Failed,
    Passed,
}

/// A check, as the agent is told of it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Check {
    name: String,
    stands: Stands,
    /// Its page, where GitHub says one.
    link: Option<String>,
}

impl Rolled {
    /// The check, as GitHub said it, or none for what it added since. A run
    /// that ended passed where it succeeded, was neutral or was skipped, and
    /// failed in any other way: a conclusion GitHub has not spelled before
    /// counts as failed, which a look at the log says more of. A status
    /// failed where it failed or erred.
    fn check(self) -> Option<Check> {
        let given = |link: Option<String>| link.filter(|link| !link.is_empty());
        match self {
            Rolled::CheckRun { name, workflow_name, status, conclusion, details_url } => {
                let stands = match (status.as_str(), conclusion.as_deref()) {
                    ("COMPLETED", Some("SUCCESS" | "NEUTRAL" | "SKIPPED")) => Stands::Passed,
                    ("COMPLETED", _) => Stands::Failed,
                    _ => Stands::Pending,
                };
                let name = match workflow_name.filter(|workflow| !workflow.is_empty()) {
                    Some(workflow) => format!("{workflow} / {name}"),
                    None => name,
                };
                Some(Check { name, stands, link: given(details_url) })
            },
            Rolled::StatusContext { context, state, target_url } => {
                let stands = match state.as_str() {
                    "SUCCESS" => Stands::Passed,
                    "PENDING" | "EXPECTED" => Stands::Pending,
                    _ => Stands::Failed,
                };
                Some(Check { name: context, stands, link: given(target_url) })
            },
            Rolled::Other => None,
        }
    }
}

/// The checks gh wrote in `statusCheckRollup`, or none where it wrote what
/// does not read: what GitHub writes of them in a way gh has not before
/// leaves the pull request as it was, with no checks said.
fn checks_of(rollup: Option<serde_json::Value>) -> Option<Vec<Check>> {
    let rolled: Vec<Rolled> = match rollup {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(rollup) => serde_json::from_value(rollup).ok()?,
    };
    Some(rolled.into_iter().filter_map(Rolled::check).collect())
}

/// How many of `checks` stand each way.
fn counted(checks: &[Check]) -> Checks {
    let count = |stands| checks.iter().filter(|check| check.stands == stands).count();
    let count = |stands| u32::try_from(count(stands)).unwrap_or(u32::MAX);
    let (pending, failed, passed) =
        (count(Stands::Pending), count(Stands::Failed), count(Stands::Passed));
    Checks { pending, failed, passed }
}

/// How the checks gh wrote with a pull request stand, as `github` reads them.
pub(crate) fn rolled_up(rollup: Option<serde_json::Value>) -> Option<Checks> {
    checks_of(rollup).as_deref().map(counted)
}

/// A job of GitHub Actions, by the repository it ran in, as gh's `-R` takes
/// it, and its number there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    repository: String,
    id: u64,
}

/// The job a check's page is of, where it is a job's of GitHub Actions:
/// `https://<host>/<owner>/<repo>/actions/runs/<run>/job/<job>`.
fn job_of(link: &str) -> Option<Job> {
    let path = link.strip_prefix("https://")?;
    let path = path.split(['?', '#']).next()?;
    let parts: Vec<&str> = path.split('/').collect();
    let [host, owner, name, "actions", "runs", run, "job", job] = parts.as_slice() else {
        return None;
    };
    let named = |part: &str| !part.is_empty() && !part.starts_with('-');
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    if !(named(host) && named(owner) && named(name) && number(run)) {
        return None;
    }
    let id = job.parse().ok().filter(|_| number(job))?;
    Some(Job { repository: format!("{host}/{owner}/{name}"), id })
}

/// The pull request of a page, `https://<host>/<owner>/<repo>/pull/<n>`, as gh
/// takes it, or why it is none.
pub fn page(url: &str) -> Result<&str, String> {
    let path = url.strip_prefix("https://").ok_or_else(|| format!("{url} is no https page"))?;
    let parts: Vec<&str> = path.split('/').collect();
    let named = |part: &&str| !part.is_empty() && !part.contains(['?', '#']);
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    match parts.as_slice() {
        [host, owner, name, "pull", pull]
            if [host, owner, name].into_iter().all(named) && number(pull) =>
        {
            Ok(url)
        },
        _ => Err(format!("{url} is no page of a pull request")),
    }
}

/// What a check that failed holds of its log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Log {
    /// Its failed steps' log, its last lines, in a file of its own.
    Kept(Kept),
    /// Why none was kept, in words.
    Unkept(String),
}

/// A check that failed: its name, its page, where GitHub says one, and its log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub name: String,
    pub link: Option<String>,
    pub log: Log,
}

/// The checks of a pull request's head, as `kept_failing` found them: the
/// commit, the log of each that failed, and how many others stand each way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    pub head: String,
    pub failed: Vec<Failed>,
    pub pending: u32,
    pub passed: u32,
}

/// The pull request, as `gh pr view --json headRefOid,statusCheckRollup`
/// writes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    head_ref_oid: String,
    #[serde(default)]
    status_check_rollup: Option<serde_json::Value>,
}

/// Asks gh how the checks of the pull request of the page `url` stand on its
/// head now, and keeps the log of each that failed, of those of GitHub Actions,
/// in luneta's runtime folder, or says why it found none.
pub fn kept_failing(url: &str) -> Result<Failing, String> {
    let url = page(url)?;
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", url, "--json", "headRefOid,statusCheckRollup"]));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh pr view {url} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh pr view {url}: {why}"))?;
    let viewed: Viewed = serde_json::from_slice(&said)
        .map_err(|why| format!("gh pr view {url} said what does not read: {why}"))?;
    let checks = checks_of(viewed.status_check_rollup)
        .ok_or_else(|| format!("gh pr view {url} said checks that do not read"))?;
    let runtime = folder::find().ok_or("neither XDG_RUNTIME_DIR nor TMPDIR is set, to keep it in")?;
    Ok(keep(&runtime, viewed.head_ref_oid, &checks, fetch, SystemTime::now()))
}

/// What gh says of the failed steps of `job`: the log, or why it gave none.
fn fetch(job: &Job) -> Result<Vec<u8>, String> {
    let mut gh = Command::new("gh");
    let id = job.id.to_string();
    gh.args(["run", "view", "--job", &id, "--log-failed", "-R", &job.repository]);
    let asked = format!("gh run view --job {id} --log-failed -R {}", job.repository);
    let ran = program::run(git::local(&mut gh), LIMIT)
        .map_err(|error| format!("{asked} could not be run: {error}"))?;
    ran.answer().map_err(|why| format!("{asked}: {why}"))
}

/// The checks of `head` that failed, of `checks`, each with its log as
/// `fetch` gets it, kept in a file of its own in the folder `FOLDER` of
/// luneta's folder `runtime`, for the first `LOGS_MAX` of them: fetched side
/// by side, so that the ask takes as long as the slowest.
fn keep(
    runtime: &Path,
    head: String,
    checks: &[Check],
    fetch: impl Fn(&Job) -> Result<Vec<u8>, String> + Sync,
    now: SystemTime,
) -> Failing {
    let Checks { pending, passed, .. } = counted(checks);
    let failed: Vec<&Check> =
        checks.iter().filter(|check| check.stands == Stands::Failed).collect();
    let log = |at: usize, check: &Check| match check.link.as_deref().and_then(job_of) {
        _ if at >= LOGS_MAX => {
            Log::Unkept(format!("luneta fetches no more than {LOGS_MAX} logs"))
        },
        None => Log::Unkept(NO_JOB.to_owned()),
        Some(job) => match fetch(&job).and_then(|log| write(runtime, &log, now)) {
            Ok(kept) => Log::Kept(kept),
            Err(why) => Log::Unkept(why),
        },
    };
    let logs: Vec<Log> = thread::scope(|scope| {
        let fetching: Vec<_> = failed
            .iter()
            .enumerate()
            .map(|(at, check)| scope.spawn(move || log(at, check)))
            .collect();
        let joined = fetching.into_iter().map(thread::ScopedJoinHandle::join);
        joined.map(|log| log.unwrap_or_else(|_| Log::Unkept(PANICKED.to_owned()))).collect()
    });
    let failed = failed.into_iter().zip(logs).map(|(Check { name, link, .. }, log)| Failed {
        name: name.clone(),
        link: link.clone(),
        log,
    });
    Failing { head, failed: failed.collect(), pending, passed }
}

/// Writes the last `TAIL` lines of `log`, with no styles, in a file of its
/// own, as `keeps::Draft` keeps it, or says why it kept none.
fn write(runtime: &Path, log: &[u8], now: SystemTime) -> Result<Kept, String> {
    let log = String::from_utf8_lossy(log);
    let lines: Vec<String> = log.lines().map(plain).collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return Err("gh fetched a log with nothing in it".to_owned());
    };
    let lines = &lines[first..=last];
    let left_out = lines.len().saturating_sub(TAIL);
    let lines = &lines[left_out..];
    let mut draft = keeps::Draft::new(runtime, FOLDER, "log", now)?;
    let mut text = lines.join("\n");
    text.push('\n');
    draft.write(text.as_bytes())?;
    Ok(Kept { file: draft.keep()?, lines: lines.len(), left_out })
}

/// `line` with no styles in it: each control sequence a terminal reads, as
/// GitHub keeps them in a log, gone, and the mark a file of UTF-8 can start
/// with.
fn plain(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                // Parameters and what goes between them, up to the final byte.
                while chars.next_if(|c| ('\u{20}'..='\u{3f}').contains(c)).is_some() {}
                chars.next_if(|c| ('\u{40}'..='\u{7e}').contains(c));
            },
            '\u{feff}' => {},
            c => plain.push(c),
        }
    }
    plain
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::UNIX_EPOCH;

    use super::*;

    /// A run of a check, as gh writes it.
    fn run(name: &str, status: &str, conclusion: &str, job: u64) -> String {
        format!(
            r#"{{"__typename":"CheckRun","completedAt":"2026-11-08T11:29:09Z",
            "conclusion":"{conclusion}","detailsUrl":
            "https://github.com/owner/repo/actions/runs/42/job/{job}","name":"{name}",
            "startedAt":"2026-11-08T11:23:15Z","status":"{status}","workflowName":"ci"}}"#
        )
    }

    fn status(context: &str, state: &str) -> String {
        format!(
            r#"{{"__typename":"StatusContext","context":"{context}","state":"{state}",
            "startedAt":"2026-11-08T11:23:15Z","targetUrl":"https://ci.example.com/1"}}"#
        )
    }

    fn rollup(checks: &[String]) -> serde_json::Value {
        serde_json::from_str(&format!("[{}]", checks.join(","))).unwrap()
    }

    /// A run passes once it ends well, or neutral, or skipped; it fails once
    /// it ends any other way, as one cancelled or of a conclusion GitHub has
    /// not spelled before; and until it ends, it is pending, as a status is
    /// until it is set. What GitHub lists that is neither counts for nothing.
    #[test]
    fn checks_are_counted_by_how_they_ended() {
        let checks = rollup(&[
            run("clippy", "COMPLETED", "SUCCESS", 1),
            run("lint", "COMPLETED", "NEUTRAL", 2),
            run("mac", "COMPLETED", "SKIPPED", 3),
            run("test", "COMPLETED", "FAILURE", 4),
            run("slow", "COMPLETED", "TIMED_OUT", 5),
            run("gone", "COMPLETED", "CANCELLED", 6),
            run("new", "COMPLETED", "SOMETHING_NEW", 7),
            run("arm", "IN_PROGRESS", "", 8),
            run("queued", "QUEUED", "", 9),
            status("ci/ok", "SUCCESS"),
            status("ci/bad", "FAILURE"),
            status("ci/broke", "ERROR"),
            status("ci/wait", "PENDING"),
            status("ci/expected", "EXPECTED"),
            r#"{"__typename":"SomethingNew","name":"x"}"#.to_owned(),
        ]);
        assert_eq!(rolled_up(Some(checks)), Some(Checks { pending: 4, failed: 6, passed: 4 }));
        assert_eq!(rolled_up(None), Some(Checks::default()));
        assert_eq!(rolled_up(Some(serde_json::Value::Null)), Some(Checks::default()));
        assert_eq!(rolled_up(Some(rollup(&[]))), Some(Checks::default()));
        let unread = serde_json::json!([{"__typename": "CheckRun", "name": 1}]);
        assert_eq!(rolled_up(Some(unread)), None);
    }

    /// A job's page names the repository it ran in, on its host, and its
    /// number; any other page names no job.
    #[test]
    fn a_job_s_page_says_where_it_ran() {
        let job = |repository: &str, id| Some(Job { repository: repository.to_owned(), id });
        let page = "https://github.com/owner/repo/actions/runs/42/job/128712805531";
        assert_eq!(job_of(page), job("github.com/owner/repo", 128712805531));
        assert_eq!(job_of(&format!("{page}?pr=328")), job("github.com/owner/repo", 128712805531));
        let enterprise = "https://git.example.com/team/repo/actions/runs/1/job/2";
        assert_eq!(job_of(enterprise), job("git.example.com/team/repo", 2));
        for page in [
            "http://github.com/owner/repo/actions/runs/42/job/1",
            "https://github.com/owner/repo/actions/runs/42",
            "https://github.com/owner/repo/actions/runs/x/job/1",
            "https://github.com/owner/repo/actions/runs/42/job/-1",
            "https://github.com/owner/repo/actions/runs/42/job/1/x",
            "https://github.com/-R/repo/actions/runs/42/job/1",
            "https://github.com//repo/actions/runs/42/job/1",
            "https://ci.example.com/1",
        ] {
            assert_eq!(job_of(page), None, "{page}");
        }
    }

    /// Only an `https` page of a pull request is one gh is asked of.
    #[test]
    fn only_a_pull_request_s_page_is_asked_of() {
        let url = "https://github.com/owner/repo/pull/328";
        assert_eq!(page(url), Ok(url));
        for url in [
            "http://github.com/owner/repo/pull/328",
            "https://github.com/owner/repo/issues/328",
            "https://github.com/owner/repo/pull/328/files",
            "https://github.com/owner/repo/pull/",
            "https://github.com/owner/repo/pull/x",
            "https://github.com/owner/repo?/pull/1",
            "--repo",
            "",
        ] {
            assert!(page(url).is_err(), "{url}");
        }
    }

    /// GitHub keeps the styles a step's terminal had in its log, and a mark
    /// at the start of it, which the file leaves out.
    #[test]
    fn a_log_is_kept_with_no_styles() {
        let styled = "\u{feff}2026 \u{1b}[36;1mnix develop\u{1b}[0m done";
        assert_eq!(plain(styled), "2026 nix develop done");
        assert_eq!(plain("\u{1b}[K\u{1b}[1A plain"), " plain");
        assert_eq!(plain("left \u{1b}] alone"), "left \u{1b}] alone");
        assert_eq!(plain("cut \u{1b}["), "cut ");
    }

    /// Each check that failed is listed, in gh's order: one of GitHub Actions
    /// with its log, its last lines, in a file of its own; any other, and one
    /// whose log gh did not give, with why. Past `LOGS_MAX`, none is fetched.
    #[test]
    fn each_check_that_failed_keeps_its_log() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("luneta");
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let checks = checks_of(Some(rollup(&[
            run("clippy", "COMPLETED", "SUCCESS", 1),
            run("test", "COMPLETED", "FAILURE", 2),
            status("ci/bad", "FAILURE"),
            run("mac", "COMPLETED", "FAILURE", 3),
            run("arm", "IN_PROGRESS", "", 4),
            run("empty", "COMPLETED", "FAILURE", 5),
        ])))
        .unwrap();
        let log = b"\n\ntest\tRun\t\xef\xbb\xbfgo\ntest\tRun\t\x1b[31mfailed\x1b[0m\n";
        let fetch = |job: &Job| {
            assert_eq!(job.repository, "github.com/owner/repo");
            match job.id {
                2 => Ok(log.to_vec()),
                3 => Err("gh: job 3 is still in progress".to_owned()),
                5 => Ok(b"\n  \n".to_vec()),
                _ => panic!("fetched job {}", job.id),
            }
        };
        let failing = keep(&runtime, "abc".to_owned(), &checks, fetch, now);
        assert_eq!((failing.head.as_str(), failing.pending, failing.passed), ("abc", 1, 1));
        let names: Vec<&str> = failing.failed.iter().map(|failed| failed.name.as_str()).collect();
        assert_eq!(names, ["ci / test", "ci/bad", "ci / mac", "ci / empty"]);
        let Log::Kept(Kept { file, lines: 2, left_out: 0 }) = &failing.failed[0].log else {
            panic!("{:?}", failing.failed[0].log);
        };
        let path = Path::new(file);
        let folder = path.parent().and_then(Path::file_name);
        assert_eq!((path.extension(), folder), (Some("log".as_ref()), Some("checks".as_ref())));
        assert_eq!(fs::read_to_string(file).unwrap(), "test\tRun\tgo\ntest\tRun\tfailed\n");
        assert_eq!(failing.failed[1].link.as_deref(), Some("https://ci.example.com/1"));
        let unkept = |at: usize| match &failing.failed[at].log {
            Log::Unkept(why) => why.clone(),
            Log::Kept(kept) => panic!("{kept:?}"),
        };
        assert_eq!(unkept(1), NO_JOB);
        assert_eq!(unkept(2), "gh: job 3 is still in progress");
        assert_eq!(unkept(3), "gh fetched a log with nothing in it");

        let many: Vec<String> =
            (1..=LOGS_MAX as u64 + 1).map(|job| run("m", "COMPLETED", "FAILURE", job)).collect();
        let fetch = |job: &Job| {
            assert!(job.id <= LOGS_MAX as u64, "fetched job {}", job.id);
            Ok(b"failed\n".to_vec())
        };
        let many = checks_of(Some(rollup(&many))).unwrap();
        let failing = keep(&runtime, "abc".to_owned(), &many, fetch, now);
        assert_eq!(failing.failed.len(), LOGS_MAX + 1);
        assert!(failing.failed[..LOGS_MAX].iter().all(|failed| matches!(failed.log, Log::Kept(_))));
        let last = &failing.failed[LOGS_MAX].log;
        assert_eq!(last, &Log::Unkept(format!("luneta fetches no more than {LOGS_MAX} logs")));
    }

    /// A long log keeps its last `TAIL` lines, and says how many it left out.
    #[test]
    fn a_long_log_keeps_its_last_lines() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("luneta");
        let log: Vec<String> = (1..=TAIL + 5).map(|line| format!("line {line}")).collect();
        let log = log.join("\n");
        let kept = write(&runtime, log.as_bytes(), SystemTime::now()).unwrap();
        assert_eq!((kept.lines, kept.left_out), (TAIL, 5));
        let text = fs::read_to_string(&kept.file).unwrap();
        assert!(text.starts_with("line 6\n") && text.ends_with(&format!("line {}\n", TAIL + 5)));
    }
}
