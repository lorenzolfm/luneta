use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, SystemTime};

pub use lune_protocol::github::PullPage;

use lune_protocol::github::Checks;
use serde::Deserialize;

use crate::printed::Kept;
use crate::{folder, git, keeps, program};

pub(crate) const FOLDER: &str = "checks";

const LIMIT: Duration = Duration::from_secs(30);

pub const FETCHING: Duration = Duration::from_secs(2 * LIMIT.as_secs() + 5);

const TAIL: usize = 2000;

const LOGS_MAX: usize = 8;

const NO_JOB: &str = "it is no job of GitHub Actions, whose log gh fetches";
const PANICKED: &str = "what fetched its log panicked";

const AS_IT_IS: &str = "--allow-escape-sequences";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stands {
    Pending,
    Failed,
    Passed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Check {
    name: String,
    stands: Stands,
    link: Option<String>,
}

impl Rolled {
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

fn checks_of(rollup: Option<serde_json::Value>) -> Option<Vec<Check>> {
    let rolled: Vec<Rolled> = match rollup {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(rollup) => serde_json::from_value(rollup).ok()?,
    };
    Some(rolled.into_iter().filter_map(Rolled::check).collect())
}

fn counted(checks: &[Check]) -> Checks {
    let count = |stands| checks.iter().filter(|check| check.stands == stands).count();
    let count = |stands| u32::try_from(count(stands)).unwrap_or(u32::MAX);
    let (pending, failed, passed) =
        (count(Stands::Pending), count(Stands::Failed), count(Stands::Passed));
    Checks { pending, failed, passed }
}

pub(crate) fn rolled_up(rollup: Option<serde_json::Value>) -> Option<Checks> {
    checks_of(rollup).as_deref().map(counted)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Job {
    host: String,
    repository: String,
    id: u64,
}

impl Job {
    fn ran_in(&self, repository: &str) -> bool {
        let ran = format!("{}/{}", self.host, self.repository);
        ran.eq_ignore_ascii_case(repository)
    }
}

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
    Some(Job { host: (*host).to_owned(), repository: format!("{owner}/{name}"), id })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Log {
    Kept(Kept),
    Unkept(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failed {
    pub name: String,
    pub link: Option<String>,
    pub log: Log,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failing {
    pub head: String,
    pub failed: Vec<Failed>,
    pub pending: u32,
    pub passed: u32,
}

const VIEWED: &str = "state,headRefOid,headRepository,headRepositoryOwner,statusCheckRollup";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Viewed {
    state: String,
    head_ref_oid: String,
    #[serde(default)]
    head_repository: Option<Named>,
    #[serde(default)]
    head_repository_owner: Option<Owner>,
    #[serde(default)]
    status_check_rollup: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Named {
    name: String,
}

#[derive(Deserialize)]
struct Owner {
    login: String,
}

pub fn kept_failing(page: &PullPage) -> Result<Failing, String> {
    let url = page.url();
    let mut gh = Command::new("gh");
    git::local(gh.args(["pr", "view", url, "--json", VIEWED]));
    let ran = program::run(&mut gh, LIMIT)
        .map_err(|error| format!("gh pr view {url} could not be run: {error}"))?;
    let said = ran.answer().map_err(|why| format!("gh pr view {url}: {why}"))?;
    let viewed: Viewed = serde_json::from_slice(&said)
        .map_err(|why| format!("gh pr view {url} said what does not read: {why}"))?;
    if viewed.state != "OPEN" {
        return Err(format!("{url} is {} now", viewed.state.to_lowercase()));
    }
    let checks = checks_of(viewed.status_check_rollup)
        .ok_or_else(|| format!("gh pr view {url} said checks that do not read"))?;
    let host = page.host();
    let head = match (viewed.head_repository_owner, viewed.head_repository) {
        (Some(Owner { login }), Some(Named { name })) => Some(format!("{host}/{login}/{name}")),
        _ => None,
    };
    let repositories: Vec<&str> =
        [page.repository()].into_iter().chain(head.as_deref()).collect();
    let runtime = folder::find().map_err(|unfound| format!("{unfound}, to keep it in"))?;
    Ok(keep(&runtime, viewed.head_ref_oid, &checks, &repositories, fetch, SystemTime::now()))
}

fn fetch(job: &Job) -> Result<(Vec<u8>, u64), String> {
    let logs = format!("repos/{}/actions/jobs/{}/logs", job.repository, job.id);
    let asked = format!("gh api --hostname {} {logs}", job.host);
    let ask = |flags: &[&str]| {
        let mut gh = Command::new("gh");
        gh.args(["api", "--hostname", &job.host]).args(flags).arg(&logs);
        program::run_tail(git::local(&mut gh), LIMIT, program::SAID_MAX)
            .map_err(|error| format!("{asked} could not be run: {error}"))
    };
    let unknown = format!("unknown flag: {AS_IT_IS}");
    let (ran, before) = match ask(&[AS_IT_IS])? {
        (program::Ran::Ended { stderr, .. }, _)
            if String::from_utf8_lossy(&stderr).contains(&unknown) =>
        {
            ask(&[])?
        },
        ran => ran,
    };
    let log = ran.answer().map_err(|why| format!("{asked}: {why}"))?;
    Ok((log, before))
}

enum Plan {
    Fetch(Job),
    NoJob,
    Elsewhere(Job),
    Past,
}

fn keep(
    runtime: &Path,
    head: String,
    checks: &[Check],
    repositories: &[&str],
    fetch: impl Fn(&Job) -> Result<(Vec<u8>, u64), String> + Sync,
    now: SystemTime,
) -> Failing {
    let Checks { pending, passed, .. } = counted(checks);
    let failed: Vec<&Check> =
        checks.iter().filter(|check| check.stands == Stands::Failed).collect();
    let mut jobs = 0;
    let plans = failed.iter().map(|check| match check.link.as_deref().and_then(job_of) {
        None => Plan::NoJob,
        Some(job) if !repositories.iter().any(|repository| job.ran_in(repository)) => {
            Plan::Elsewhere(job)
        },
        Some(_) if jobs >= LOGS_MAX => Plan::Past,
        Some(job) => {
            jobs += 1;
            Plan::Fetch(job)
        },
    });
    let plans: Vec<Plan> = plans.collect();
    let log = |plan: Plan| match plan {
        Plan::NoJob => Log::Unkept(NO_JOB.to_owned()),
        Plan::Elsewhere(Job { host, repository, .. }) => Log::Unkept(format!(
            "its job ran in {host}/{repository}, which is neither the pull request's repository \
             nor its head's"
        )),
        Plan::Past => Log::Unkept(format!("lune fetches no more than {LOGS_MAX} logs")),
        Plan::Fetch(job) => {
            let kept = fetch(&job).and_then(|(log, before)| write(runtime, &log, before, now));
            kept.map_or_else(Log::Unkept, Log::Kept)
        },
    };
    let logs: Vec<Log> = thread::scope(|scope| {
        let fetching: Vec<_> =
            plans.into_iter().map(|plan| scope.spawn(move || log(plan))).collect();
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

fn write(runtime: &Path, log: &[u8], before: u64, now: SystemTime) -> Result<Kept, String> {
    let log = String::from_utf8_lossy(log);
    let lines: Vec<String> = log.lines().map(plain).collect();
    let first = lines.iter().position(|line| !line.trim().is_empty());
    let last = lines.iter().rposition(|line| !line.trim().is_empty());
    let (Some(first), Some(last)) = (first, last) else {
        return Err("gh fetched a log with nothing in it".to_owned());
    };
    let lines = &lines[first..=last];
    let cut = lines.len().saturating_sub(TAIL);
    let lines = &lines[cut..];
    let before = usize::try_from(before).unwrap_or(usize::MAX);
    let blank = if before == 0 { 0 } else { first };
    let left_out = before.saturating_add(blank).saturating_add(cut);
    let mut draft = keeps::Draft::new(runtime, FOLDER, "log", now)?;
    let mut text = lines.join("\n");
    text.push('\n');
    draft.write(text.as_bytes())?;
    Ok(Kept { file: draft.keep()?, lines: lines.len(), left_out })
}

fn plain(line: &str) -> String {
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
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

    #[test]
    fn a_job_s_page_says_where_it_ran() {
        let job = |host: &str, repository: &str, id| {
            Some(Job { host: host.to_owned(), repository: repository.to_owned(), id })
        };
        let page = "https://github.com/owner/repo/actions/runs/42/job/128712805531";
        assert_eq!(job_of(page), job("github.com", "owner/repo", 128712805531));
        let asked = job_of(&format!("{page}?pr=328"));
        assert_eq!(asked, job("github.com", "owner/repo", 128712805531));
        let enterprise = "https://git.example.com/team/repo/actions/runs/1/job/2";
        assert_eq!(job_of(enterprise), job("git.example.com", "team/repo", 2));
        let ran = job_of(page).unwrap();
        assert!(ran.ran_in("github.com/owner/repo") && ran.ran_in("github.com/Owner/Repo"));
        assert!(!ran.ran_in("github.com/owner/other") && !ran.ran_in("git.example.com/owner/repo"));
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

    #[test]
    fn a_log_is_kept_with_no_styles() {
        let styled = "\u{feff}2026 \u{1b}[36;1mnix develop\u{1b}[0m done";
        assert_eq!(plain(styled), "2026 nix develop done");
        assert_eq!(plain("\u{1b}[K\u{1b}[1A plain"), " plain");
        assert_eq!(plain("left \u{1b}] alone"), "left \u{1b}] alone");
        assert_eq!(plain("cut \u{1b}["), "cut ");
    }

    #[test]
    fn each_check_that_failed_keeps_its_log() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("lune");
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let checks = checks_of(Some(rollup(&[
            run("clippy", "COMPLETED", "SUCCESS", 1),
            run("test", "COMPLETED", "FAILURE", 2),
            status("ci/bad", "FAILURE"),
            run("mac", "COMPLETED", "FAILURE", 3),
            run("arm", "IN_PROGRESS", "", 4),
            run("empty", "COMPLETED", "FAILURE", 5),
            run("fork", "COMPLETED", "FAILURE", 6).replace("owner/repo", "fork/repo"),
            run("other", "COMPLETED", "FAILURE", 7).replace("owner/repo", "other/repo"),
        ])))
        .unwrap();
        let repositories = ["github.com/owner/repo", "github.com/fork/repo"];
        let log = b"\n\ntest\tRun\t\xef\xbb\xbfgo\ntest\tRun\t\x1b[31mfailed\x1b[0m\n";
        let fetch = |job: &Job| {
            assert_eq!(job.host, "github.com");
            match (job.repository.as_str(), job.id) {
                ("owner/repo", 2) => Ok((log.to_vec(), 0)),
                ("owner/repo", 3) => Err("gh: HTTP 404: Not Found".to_owned()),
                ("owner/repo", 5) => Ok((b"\n  \n".to_vec(), 0)),
                ("fork/repo", 6) => Ok((b"\nfailed\n".to_vec(), 40)),
                _ => panic!("fetched job {job:?}"),
            }
        };
        let failing = keep(&runtime, "abc".to_owned(), &checks, &repositories, fetch, now);
        assert_eq!((failing.head.as_str(), failing.pending, failing.passed), ("abc", 1, 1));
        let names: Vec<&str> = failing.failed.iter().map(|failed| failed.name.as_str()).collect();
        let all = ["ci / test", "ci/bad", "ci / mac", "ci / empty", "ci / fork", "ci / other"];
        assert_eq!(names, all);
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
        assert_eq!(unkept(2), "gh: HTTP 404: Not Found");
        assert_eq!(unkept(3), "gh fetched a log with nothing in it");
        let Log::Kept(Kept { lines: 1, left_out: 41, .. }) = &failing.failed[4].log else {
            panic!("{:?}", failing.failed[4].log);
        };
        let elsewhere = "its job ran in github.com/other/repo, which is neither the pull request's \
                         repository nor its head's";
        assert_eq!(unkept(5), elsewhere);

        let many: Vec<String> =
            (1..=LOGS_MAX as u64 + 1).map(|job| run("m", "COMPLETED", "FAILURE", job)).collect();
        let fetch = |job: &Job| {
            assert!(job.id <= LOGS_MAX as u64, "fetched job {}", job.id);
            Ok((b"failed\n".to_vec(), 0))
        };
        let many = checks_of(Some(rollup(&many))).unwrap();
        let failing = keep(&runtime, "abc".to_owned(), &many, &repositories, fetch, now);
        assert_eq!(failing.failed.len(), LOGS_MAX + 1);
        assert!(failing.failed[..LOGS_MAX].iter().all(|failed| matches!(failed.log, Log::Kept(_))));
        let last = &failing.failed[LOGS_MAX].log;
        assert_eq!(last, &Log::Unkept(format!("lune fetches no more than {LOGS_MAX} logs")));

        let mut first: Vec<String> =
            (0..LOGS_MAX).map(|at| status(&format!("ext/{at}"), "FAILURE")).collect();
        first.push(run("test", "COMPLETED", "FAILURE", 2));
        let fetch = |job: &Job| {
            assert_eq!(job.id, 2, "fetched job {}", job.id);
            Ok((b"failed\n".to_vec(), 0))
        };
        let first = checks_of(Some(rollup(&first))).unwrap();
        let failing = keep(&runtime, "abc".to_owned(), &first, &repositories, fetch, now);
        let no_job = Log::Unkept(NO_JOB.to_owned());
        assert!(failing.failed[..LOGS_MAX].iter().all(|failed| failed.log == no_job));
        assert!(matches!(failing.failed[LOGS_MAX].log, Log::Kept(_)));
    }

    #[test]
    fn a_long_log_keeps_its_last_lines() {
        let root = tempfile::tempdir().unwrap();
        let runtime = root.path().join("lune");
        let log: Vec<String> = (1..=TAIL + 5).map(|line| format!("line {line}")).collect();
        let log = log.join("\n");
        let kept = write(&runtime, log.as_bytes(), 0, SystemTime::now()).unwrap();
        assert_eq!((kept.lines, kept.left_out), (TAIL, 5));
        let text = fs::read_to_string(&kept.file).unwrap();
        assert!(text.starts_with("line 6\n") && text.ends_with(&format!("line {}\n", TAIL + 5)));
        let kept = write(&runtime, b"\n\nlast\n", 0, SystemTime::now()).unwrap();
        assert_eq!((kept.lines, kept.left_out), (1, 0));
        let kept = write(&runtime, b"\n\nlast\n", 7, SystemTime::now()).unwrap();
        assert_eq!((kept.lines, kept.left_out), (1, 9));
    }
}
