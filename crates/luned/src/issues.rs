use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::num::NonZeroU32;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use lune_protocol::said::Said;
use serde::Deserialize;
use serde_json::{Value, json};
use tempfile::NamedTempFile;
use tonic::{Code, Status};

use crate::api::{FileIssueRequest, FileIssueResponse, IssueRepository, On};
use crate::directory::Directory;
use crate::headless::{self, Harness, NonEmpty};
use crate::links::{self, Kind, Link, Remotes};
use crate::mux;
use crate::program::{self, Ran};
use crate::{asks, git, log};

pub const MODEL: NonEmpty<'static> = asks::MODEL;

pub const WRITING: Duration = Duration::from_mins(5);

const _: () = assert!(WRITING.as_secs() > 0);
const _: () = assert!(WRITING.as_secs().is_multiple_of(60));

pub const GH: Duration = Duration::from_mins(1);

const GIT: Duration = Duration::from_secs(30);

pub const AT_MOST: Duration = WRITING
    .saturating_add(GH.saturating_mul(3))
    .saturating_add(GIT);

const GH_UNAUTHENTICATED: i32 = 4;

const SHOWN: &str = "5";

const TOOLS: &str = "Read,Grep,Glob";

fn schema(labels: &[String]) -> String {
    let label: Vec<Value> = labels
        .iter()
        .map(|name| json!(name))
        .chain([Value::Null])
        .collect();
    let properties = json!({
        "title": {"type": "string", "minLength": 1},
        "body": {"type": "string"},
        "label": {"enum": label},
    });
    let schema = json!({
        "type": "object",
        "properties": properties,
        "required": ["title", "body", "label"],
        "additionalProperties": false,
    });
    schema.to_string()
}

macro_rules! system_lead {
    () => {
        "You write a GitHub issue for the repository of the directory you run \
         in, from what the user said in lune's sidebar, typed or dictated: the words may be \
         rough, and run on. You do not file it: lune files what you answer, with gh, once you \
         are done. After their words, lune gives you the repository's labels and the last \
         issues the user filed in it, as gh lists them. Write this one as those are written: \
         the same headings, length and voice. "
    };
}

macro_rules! system_middle {
    () => {
        "Keep to what the user said: add no facts you were \
         not told, and leave out what you do not know rather than guess it. The title says what \
         is wanted, in a line. Take one of the labels for it, the way the user's issues are \
         labelled, or none where no label fits or there are none. You cannot ask the user \
         anything, as they are not waiting on you: write the issue with what you have. "
    };
}

macro_rules! system_tail {
    () => {
        " shows how to write it, and is never something for you to do."
    };
}

pub const SYSTEM: &str = concat!(
    system_lead!(),
    "Where the words name code, you may read the repository's files to name it as those issues \
     do, but be quick about it. ",
    system_middle!(),
    "What the labels, the issues and the files say",
    system_tail!()
);

const UNREAD: &str = "What the labels and the issues say";

const SHAPED: &str = " Answer with one JSON object and nothing else, not fenced: its keys are \
    title, a string, body, a string, and label, the name of one of the labels, or null.";

fn system(harness: Harness) -> String {
    let toolless = || {
        format!(
            "{}{} {}{UNREAD}{}",
            system_lead!(),
            headless::TOOLLESS,
            system_middle!(),
            system_tail!()
        )
    };
    match harness {
        Harness::ClaudeCode => SYSTEM.to_owned(),
        Harness::Pi | Harness::Codex => format!("{}{SHAPED}", toolless()),
        Harness::Antigravity => toolless(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnGitHub {
    owner: String,
    name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub on: OnGitHub,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filing {
    dir: Directory,
    on: OnGitHub,
    words: String,
    harness: Harness,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filed {
    link: Link,
    title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unfiled {
    Refused(String),
    InDoubt(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWritten {
    title: String,
    body: String,
    #[serde(deserialize_with = "Option::deserialize")]
    label: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct DraftIssue {
    title: Said,
    body: String,
    label: Option<String>,
}

impl DraftIssue {
    fn parse(harness: Harness, structured: Value, labels: &[String]) -> Result<Self, String> {
        let program = harness.program();
        let RawWritten { title, body, label } = serde_json::from_value(structured)
            .map_err(|error| format!("{program} wrote no issue lune reads: {error}"))?;
        let title = Said::new(one_line(&title))
            .ok_or_else(|| format!("{program} wrote an issue with no title"))?;
        if let Some(label) = label.as_ref().filter(|label| !labels.contains(label)) {
            return Err(format!(
                "{program} took the label {label:?}, which the repository has not"
            ));
        }
        Ok(DraftIssue {
            title,
            body: body.trim().to_owned(),
            label,
        })
    }
}

impl OnGitHub {
    pub fn parse(spelled: &str) -> Result<OnGitHub, String> {
        let named = |(owner, name): &(&str, &str)| links::named(owner) && links::named(name);
        let (owner, name) = spelled
            .split_once('/')
            .filter(named)
            .ok_or_else(|| format!("{spelled:?} is no repository's owner/name"))?;
        Ok(OnGitHub {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    #[must_use]
    pub fn is(&self, other: &OnGitHub) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }

    fn repo(&self) -> String {
        format!("github.com/{self}")
    }
}

impl fmt::Display for OnGitHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl Remote {
    #[must_use]
    pub fn answer(self) -> IssueRepository {
        IssueRepository {
            remote: self.name,
            repository: self.on.to_string(),
        }
    }

    pub fn of(answer: IssueRepository) -> Result<Remote, String> {
        let IssueRepository { remote, repository } = answer;
        if remote.is_empty() {
            return Err(format!("a remote of {repository:?} has no name"));
        }
        Ok(Remote {
            name: remote,
            on: OnGitHub::parse(&repository)?,
        })
    }
}

pub(crate) fn remotes(dir: &Directory) -> Result<Vec<Remote>, String> {
    match links::remotes_gh_reads(dir.as_str()) {
        Remotes::Listed(listed) | Remotes::InBare(listed) => {
            Ok(on_github(&listed, links::ssh_host))
        }
        Remotes::Unlisted => Ok(Vec::new()),
        Remotes::Unsaid(why) => Err(format!("git did not say the remotes of {dir}: {why}")),
    }
}

fn on_github(listed: &str, mut ssh_host: impl FnMut(&str) -> Option<String>) -> Vec<Remote> {
    let mut urls = Vec::new();
    let mut resolved = Vec::new();
    for line in listed.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let Some(key) = key.strip_prefix("remote.") else {
            continue;
        };
        if let Some(name) = key.strip_suffix(".url") {
            urls.push((name, value.trim()));
        } else if let Some(name) = key.strip_suffix(".gh-resolved") {
            resolved.push(name);
        }
    }
    let rank = |name: &str| match name {
        _ if resolved.contains(&name) => 0,
        "upstream" => 1,
        "github" => 2,
        "origin" => 3,
        _ => 4,
    };
    urls.sort_by_key(|(name, _)| rank(name));
    let mut remotes: Vec<Remote> = Vec::new();
    for (name, url) in urls {
        let Some((owner, repository)) = links::github_repository_of(url, &mut ssh_host) else {
            continue;
        };
        let on = OnGitHub {
            owner: owner.to_owned(),
            name: repository.to_owned(),
        };
        if remotes.iter().all(|remote| !remote.on.is(&on)) {
            remotes.push(Remote {
                name: name.to_owned(),
                on,
            });
        }
    }
    remotes
}

impl Filing {
    pub fn new(dir: Directory, on: OnGitHub, words: &str) -> Result<Filing, String> {
        let words = words.trim();
        if words.is_empty() {
            return Err("words: nothing said".to_owned());
        }
        Ok(Filing {
            dir,
            on,
            words: words.to_owned(),
            harness: Harness::ClaudeCode,
        })
    }

    #[must_use]
    pub fn run_in(self, harness: Harness) -> Filing {
        Filing { harness, ..self }
    }

    #[must_use]
    pub fn runs_in(&self) -> Harness {
        self.harness
    }

    pub fn of(request: FileIssueRequest) -> Result<(Option<On>, Filing), String> {
        let FileIssueRequest {
            on,
            dir,
            words,
            repository,
            harness,
        } = request;
        let harness = Harness::said(harness).map_err(|why| format!("harness: {why}"))?;
        let dir = Directory::parse(dir).map_err(|why| format!("dir: {why}"))?;
        let repository =
            OnGitHub::parse(&repository).map_err(|why| format!("repository: {why}"))?;
        Ok((on, Filing::new(dir, repository, &words)?.run_in(harness)))
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>) -> FileIssueRequest {
        FileIssueRequest {
            on,
            dir: self.dir.to_string(),
            words: self.words.clone(),
            repository: self.on.to_string(),
            harness: Some(self.harness.to_api().into()),
        }
    }

    pub(crate) fn file(&self) -> Result<Filed, Unfiled> {
        let listing = ["label", "list", "--limit", "100", "--json", "name"];
        let listed = self.listed(&listing).map_err(Unfiled::Refused)?;
        let labels = names(&listed).map_err(Unfiled::Refused)?;
        let mine = [
            "issue", "list", "--author", "@me", "--state", "all", "--limit", SHOWN,
        ];
        let issues = self.listed(&[&mine[..], &["--json", "title,body,labels"]].concat());
        let issues = issues.map_err(Unfiled::Refused)?;
        let told = told(&self.words, &listed, &issues);
        let written = self.write(&told, &labels).map_err(Unfiled::Refused)?;
        self.create(&written)
    }

    fn listed(&self, args: &[&str]) -> Result<String, String> {
        let mut gh = self.gh(args);
        let ran = program::run(&mut gh, GH).map_err(|error| unrun("gh", &gh, &error))?;
        let said = answered(ran)?;
        Ok(String::from_utf8_lossy(&said).trim().to_owned())
    }

    fn gh(&self, args: &[&str]) -> Command {
        let mut gh = Command::new("gh");
        gh.args(args).arg("--repo").arg(self.on.repo());
        git::local(&mut gh);
        gh.current_dir(self.dir.as_str());
        gh
    }

    fn write(&self, told: &str, labels: &[String]) -> Result<DraftIssue, String> {
        match self.harness {
            Harness::ClaudeCode => {
                let settings = settings(&unread(&self.dir)).map_err(|error| {
                    format!("the settings of claude could not be written: {error}")
                })?;
                let mut claude = self.claude(labels, settings.path());
                let stdin = program::saying(told).map_err(|error| untold(self.harness, &error))?;
                let ended = self.ran(&mut claude, stdin)?;
                parse_claude_issue(&ended, labels)
            }
            Harness::Pi => {
                let bare = bare_folder()?;
                let mut pi = Self::pi(&bare);
                let stdin = program::saying(told).map_err(|error| untold(self.harness, &error))?;
                let ended = self.ran(&mut pi, stdin)?;
                if !ended.status.success() {
                    return Err(unwritten(self.harness, &ended, Unwritten::NonZeroOutput));
                }
                let said = String::from_utf8_lossy(&ended.stdout);
                DraftIssue::parse(
                    self.harness,
                    said_as_issue(self.harness, said.trim())?,
                    labels,
                )
            }
            Harness::Antigravity => {
                let bare = bare_folder()?;
                let mut agy = Self::agy(labels, &bare);
                let system = system(Harness::Antigravity);
                let message = headless::agy_message(&format!("{system}\n\n{told}"));
                let stdin =
                    program::saying(&message).map_err(|error| untold(self.harness, &error))?;
                let ended = self.ran(&mut agy, stdin)?;
                parse_agy_issue(&ended, labels)
            }
            Harness::Codex => {
                let bare = bare_folder()?;
                let mut codex = Self::codex(&bare);
                let message = format!("{}\n\n{told}", system(Harness::Codex));
                let stdin =
                    program::saying(&message).map_err(|error| untold(self.harness, &error))?;
                let ended = self.ran(&mut codex, stdin)?;
                if !ended.status.success() {
                    let why = headless::codex_failure(&ended.stderr, ended.status);
                    return Err(format!("codex wrote no issue: {why}"));
                }
                let said = String::from_utf8_lossy(&ended.stdout);
                DraftIssue::parse(
                    self.harness,
                    said_as_issue(self.harness, said.trim())?,
                    labels,
                )
            }
        }
    }

    fn ran(&self, agent: &mut Command, stdin: Stdio) -> Result<Ended, String> {
        let program = self.harness.program();
        let ran = program::run_reading(agent, WRITING, stdin)
            .map_err(|error| unrun(program, agent, &error))?;
        let minutes = WRITING.as_secs() / 60;
        match ran {
            Ran::Stuck => Err(format!(
                "{program} was still writing after {minutes} minutes"
            )),
            Ran::Ended {
                status,
                stdout,
                stderr,
            } => Ok(Ended {
                status,
                stdout,
                stderr,
            }),
        }
    }

    fn claude(&self, labels: &[String], settings: &Path) -> Command {
        let mut claude = Command::new(Harness::ClaudeCode.program());
        claude.args([
            "-p",
            "--model",
            MODEL.as_str(),
            "--safe-mode",
            "--setting-sources",
            "user",
        ]);
        claude.arg("--settings").arg(settings);
        claude.args(["--permission-mode", "dontAsk", "--tools", TOOLS]);
        claude.args(["--no-session-persistence", "--output-format", "json"]);
        claude.arg("--json-schema").arg(schema(labels));
        claude
            .arg("--append-system-prompt")
            .arg(system(Harness::ClaudeCode));
        self.placed(&mut claude);
        claude
    }

    fn pi(bare: &Path) -> Command {
        let mut pi = Command::new(Harness::Pi.program());
        pi.args(["-p", "--no-session", "--no-tools"])
            .args(headless::PI_BARE);
        pi.arg("--system-prompt").arg(system(Harness::Pi));
        placed_in(&mut pi, bare);
        pi
    }

    fn agy(labels: &[String], bare: &Path) -> Command {
        let mut agy = headless::agy_strict();
        agy.arg("--json-schema").arg(schema(labels));
        placed_in(&mut agy, bare);
        agy
    }

    fn codex(bare: &Path) -> Command {
        let mut codex = headless::codex_once();
        placed_in(&mut codex, bare);
        codex
    }

    fn placed(&self, agent: &mut Command) {
        placed_in(agent, Path::new(self.dir.as_str()));
    }

    fn create(&self, written: &DraftIssue) -> Result<Filed, Unfiled> {
        let mut gh = self.gh(&[
            "issue",
            "create",
            "--title",
            written.title.as_str(),
            "--body-file",
            "-",
        ]);
        if let Some(label) = &written.label {
            gh.args(["--label", label]);
        }
        let stdin = program::saying(&written.body)
            .map_err(|error| Unfiled::Refused(format!("gh could not be told: {error}")))?;
        let ran = program::run_reading(&mut gh, GH, stdin).map_err(|error| {
            let why = unrun("gh", &gh, &error);
            match error.kind() {
                io::ErrorKind::NotFound => Unfiled::Refused(why),
                _ => Unfiled::InDoubt(why),
            }
        })?;
        let said = match ran {
            Ran::Stuck => return Err(Unfiled::InDoubt("gh: it did not answer in time".to_owned())),
            Ran::Ended { status, stdout, .. } if status.success() => stdout,
            Ran::Ended { status, stderr, .. } => {
                let why = refused(&stderr, status);
                let said = format!("gh: {why}");
                return Err(if filed_none(&why, status) {
                    Unfiled::Refused(said)
                } else {
                    Unfiled::InDoubt(said)
                });
            }
        };
        let said = String::from_utf8_lossy(&said);
        let url = said
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or_default();
        Filed::page(url, written.title.as_str()).map_err(Unfiled::InDoubt)
    }
}

impl Filed {
    pub fn page(url: &str, title: &str) -> Result<Filed, String> {
        let link = Link::parse(url).filter(|link| link.kind() == Kind::Issue);
        let link = link.ok_or_else(|| format!("gh said {url:?}, no issue's page"))?;
        let title = one_line(title);
        if title.is_empty() {
            return Err(format!("issue #{} has no title", link.number()));
        }
        Ok(Filed { link, title })
    }

    #[must_use]
    pub fn number(&self) -> NonZeroU32 {
        self.link.number()
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn link(&self) -> &Link {
        &self.link
    }

    #[must_use]
    pub fn answer(self) -> FileIssueResponse {
        FileIssueResponse {
            url: self.link.url(),
            title: self.title,
        }
    }

    pub fn of(answer: FileIssueResponse) -> Result<Filed, String> {
        let FileIssueResponse { url, title } = answer;
        Filed::page(&url, &title)
    }
}

impl Unfiled {
    #[must_use]
    pub fn of(why: String, code: Option<Code>) -> Unfiled {
        match code {
            Some(Code::FailedPrecondition | Code::InvalidArgument | Code::Unimplemented) => {
                Unfiled::Refused(why)
            }
            _ => Unfiled::InDoubt(why),
        }
    }

    #[must_use]
    pub fn status(self) -> Status {
        match self {
            Unfiled::Refused(why) => Status::failed_precondition(why),
            Unfiled::InDoubt(why) => Status::aborted(why),
        }
    }

    #[must_use]
    pub fn why(&self) -> &str {
        match self {
            Unfiled::Refused(why) | Unfiled::InDoubt(why) => why,
        }
    }
}

struct Ended {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn placed_in(agent: &mut Command, cwd: &Path) {
    mux::own::in_no_terminal(agent);
    git::local(agent);
    agent.current_dir(cwd);
}

fn bare_folder() -> Result<std::path::PathBuf, String> {
    headless::bare_folder()
        .map_err(|error| format!("the folder for a toolless agent could not be made: {error}"))
}

fn untold(harness: Harness, error: &io::Error) -> String {
    format!("{} could not be told: {error}", harness.program())
}

#[derive(Clone, Copy)]
enum Unwritten<'a> {
    NonZeroOutput,
    NonZeroAfterJson,
    NoResult,
    InvalidJson(&'a serde_json::Error),
}

fn first_non_empty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn unwritten(harness: Harness, ended: &Ended, reason: Unwritten<'_>) -> String {
    let why = match reason {
        Unwritten::NonZeroOutput => first_non_empty_line(&ended.stdout)
            .or_else(|| first_non_empty_line(&ended.stderr))
            .unwrap_or_else(|| ended.status.to_string()),
        Unwritten::NonZeroAfterJson => {
            first_non_empty_line(&ended.stderr).unwrap_or_else(|| ended.status.to_string())
        }
        Unwritten::NoResult if ended.status.success() => first_non_empty_line(&ended.stderr)
            .unwrap_or_else(|| "it said nothing it wrote".to_owned()),
        Unwritten::NoResult => first_non_empty_line(&ended.stdout)
            .or_else(|| first_non_empty_line(&ended.stderr))
            .unwrap_or_else(|| ended.status.to_string()),
        Unwritten::InvalidJson(error) if ended.status.success() => {
            first_non_empty_line(&ended.stderr)
                .or_else(|| {
                    let non_empty = !String::from_utf8_lossy(&ended.stdout).trim().is_empty();
                    non_empty.then(|| format!("invalid JSON: {error}"))
                })
                .unwrap_or_else(|| "it said nothing it wrote".to_owned())
        }
        Unwritten::InvalidJson(_) => first_non_empty_line(&ended.stdout)
            .or_else(|| first_non_empty_line(&ended.stderr))
            .unwrap_or_else(|| ended.status.to_string()),
    };
    format!("{} failed: {why}", harness.program())
}

fn finish_json_issue(
    harness: Harness,
    ended: &Ended,
    parsed: Result<DraftIssue, String>,
) -> Result<DraftIssue, String> {
    match parsed {
        Ok(issue) if ended.status.success() => Ok(issue),
        Ok(_) => Err(unwritten(harness, ended, Unwritten::NonZeroAfterJson)),
        Err(why) => Err(why),
    }
}

fn parse_claude_issue(ended: &Ended, labels: &[String]) -> Result<DraftIssue, String> {
    let harness = Harness::ClaudeCode;
    match serde_json::from_slice(&ended.stdout) {
        Ok(record) => finish_json_issue(harness, ended, claude_written(&record, labels)),
        Err(error) => Err(unwritten(harness, ended, Unwritten::InvalidJson(&error))),
    }
}

fn parse_agy_issue(ended: &Ended, labels: &[String]) -> Result<DraftIssue, String> {
    let harness = Harness::Antigravity;
    match headless::agy_stream_result(&ended.stdout) {
        Ok(record) => finish_json_issue(harness, ended, agy_written(&record, labels)),
        Err(headless::AgyStreamError::InvalidJson(error)) => {
            Err(unwritten(harness, ended, Unwritten::InvalidJson(&error)))
        }
        Err(headless::AgyStreamError::NoResult) => {
            Err(unwritten(harness, ended, Unwritten::NoResult))
        }
    }
}

fn claude_written(record: &Value, labels: &[String]) -> Result<DraftIssue, String> {
    let harness = Harness::ClaudeCode;
    let said = record["result"].as_str().map_or_default(str::trim);
    let failed = record["is_error"].as_bool() != Some(false) || record["subtype"] != "success";
    if failed {
        let said = Some(said).filter(|said| !said.is_empty());
        let why = said
            .or_else(|| record["subtype"].as_str())
            .unwrap_or("the run failed");
        return Err(format!("{} failed: {why}", harness.program()));
    }
    let structured = record
        .get("structured_output")
        .filter(|output| !output.is_null());
    let structured = match structured {
        Some(structured) => structured.clone(),
        None => unstructured(record, said)?,
    };
    DraftIssue::parse(harness, structured, labels)
}

fn agy_written(record: &Value, labels: &[String]) -> Result<DraftIssue, String> {
    let harness = Harness::Antigravity;
    if record["status"] == "SUCCESS" {
        let structured = record
            .get("structured_output")
            .filter(|output| !output.is_null());
        let structured = match structured {
            Some(structured) => structured.clone(),
            None => said_as_issue(
                harness,
                record["response"].as_str().map_or_default(str::trim),
            )?,
        };
        DraftIssue::parse(harness, structured, labels)
    } else {
        let why = record["error"]
            .as_str()
            .map(one_line)
            .filter(|why| !why.is_empty());
        let why = why.or_else(|| record["status"].as_str().map(str::to_owned));
        Err(format!(
            "{} failed: {}",
            harness.program(),
            why.unwrap_or_else(|| "the run failed".to_owned())
        ))
    }
}

fn unstructured(record: &Value, said: &str) -> Result<Value, String> {
    let [stop, terminal, turns] =
        ["stop_reason", "terminal_reason", "num_turns"].map(|key| &record[key]);
    log::say(&format!(
        "issue: claude ended with no structured output, on {stop} and {terminal}, in {turns} \
         turns: {said:?}"
    ));
    said_as_issue(Harness::ClaudeCode, said)
}

fn said_as_issue(harness: Harness, said: &str) -> Result<Value, String> {
    let program = harness.program();
    let fenced = said
        .strip_prefix("```json")
        .or_else(|| said.strip_prefix("```"));
    let json = fenced
        .and_then(|fenced| fenced.strip_suffix("```"))
        .unwrap_or(said);
    match serde_json::from_str(json.trim()) {
        Ok(issue @ Value::Object(_)) => Ok(issue),
        _ if said.is_empty() => Err(format!("{program} ended without writing an issue")),
        _ => Err(format!(
            "{program} wrote no issue, and said: {}",
            one_line(said)
        )),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn names(listed: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct Label {
        name: String,
    }
    let labels: Vec<Label> = serde_json::from_str(listed)
        .map_err(|error| format!("gh listed labels lune does not read: {error}"))?;
    Ok(labels.into_iter().map(|label| label.name).collect())
}

fn told(words: &str, labels: &str, issues: &str) -> String {
    format!(
        "{words}\n\n[lune: the repository's labels, as gh label list says them:]\n{labels}\n\n\
         [lune: the last issues the user filed in it, newest first, as gh issue list says \
         them:]\n{issues}"
    )
}

fn answered(ran: Ran) -> Result<Vec<u8>, String> {
    match ran {
        Ran::Ended { status, stdout, .. } if status.success() => Ok(stdout),
        Ran::Ended { status, stderr, .. } => Err(format!("gh: {}", refused(&stderr, status))),
        Ran::Stuck => Err("gh: it did not answer in time".to_owned()),
    }
}

fn filed_none(why: &str, status: ExitStatus) -> bool {
    status.code() == Some(GH_UNAUTHENTICATED)
        || why.starts_with("GraphQL: ")
        || why.starts_with("HTTP 4")
        || why.starts_with("could not add label: ")
        || why.ends_with(" repository has disabled issues")
}

fn unread(dir: &Directory) -> Vec<String> {
    let said = ignored(dir).unwrap_or_else(|why| {
        log::say(&format!(
            "issue: git did not say what it ignores in {dir}: {why}"
        ));
        b"./\0".to_vec()
    });
    let real = fs::canonicalize(dir.as_str()).ok();
    let real = real
        .as_deref()
        .and_then(Path::to_str)
        .filter(|real| *real != dir.as_str());
    let bases = [Some(dir.as_str()), real];
    bases
        .into_iter()
        .flatten()
        .flat_map(|base| rules(base, &said))
        .collect()
}

fn ignored(dir: &Directory) -> Result<Vec<u8>, String> {
    let mut git = git::Git::at(dir.as_str(), "ls-files");
    git.args([
        "-z",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ]);
    match git
        .run(GIT)
        .map_err(|error| unrun_in("git", None, &error))?
    {
        Ran::Ended { status, stdout, .. } if status.success() => Ok(stdout),
        Ran::Ended { status, stderr, .. } => Err(refused(&stderr, status)),
        Ran::Stuck => Err(format!("it was still listing after {}s", GIT.as_secs())),
    }
}

fn rules(base: &str, ignored: &[u8]) -> Vec<String> {
    let base = base.trim_end_matches('/');
    let mut paths = Vec::new();
    for listed in ignored
        .split(|byte| *byte == 0)
        .filter(|listed| !listed.is_empty())
    {
        let Ok(listed) = std::str::from_utf8(listed) else {
            return vec![denied(&format!("{base}/"))];
        };
        paths.push(listed.strip_prefix("./").unwrap_or(listed));
    }
    let mut rules: Vec<String> = paths
        .iter()
        .map(|path| denied(&format!("{base}/{path}")))
        .collect();
    rules.push(format!("Read(/{}/**/.git/**)", escaped(base)));
    rules
}

fn denied(path: &str) -> String {
    let all = if path.ends_with('/') { "**" } else { "" };
    format!("Read(/{}{all})", escaped(path))
}

fn escaped(path: &str) -> String {
    let mut escaped = String::with_capacity(path.len());
    for c in path.chars() {
        if matches!(c, '\\' | '*' | '?' | '[' | ']' | '(' | ')') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn settings(unread: &[String]) -> io::Result<NamedTempFile> {
    let mut file = tempfile::Builder::new()
        .prefix("lune-issue-")
        .suffix(".json")
        .tempfile()?;
    let settings = json!({"permissions": {"deny": unread}});
    file.write_all(settings.to_string().as_bytes())?;
    file.flush()?;
    Ok(file)
}

fn refused(stderr: &[u8], status: std::process::ExitStatus) -> String {
    let said = String::from_utf8_lossy(stderr);
    let mut lines = said.lines().map(str::trim);
    let why = lines.find(|line| !line.is_empty() && !line.starts_with("Creating issue in "));
    why.map_or_else(|| status.to_string(), str::to_owned)
}

fn unrun(program: &str, command: &Command, error: &io::Error) -> String {
    unrun_in(program, command.get_current_dir(), error)
}

fn unrun_in(program: &str, cwd: Option<&Path>, error: &io::Error) -> String {
    if error.kind() != io::ErrorKind::NotFound {
        return format!("{program} could not be run: {error}");
    }
    match cwd.filter(|cwd| !cwd.is_dir()) {
        Some(cwd) => format!("{} is gone", cwd.display()),
        None => format!("{program} is not on the PATH"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn filing() -> Filing {
        let dir = Directory::parse("/w/lune/main".to_owned()).unwrap();
        Filing::new(dir, on("o/lune"), " a red button ").unwrap()
    }

    fn on(spelled: &str) -> OnGitHub {
        OnGitHub::parse(spelled).unwrap()
    }

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn the_agent_writes_in_the_row_s_directory_and_runs_nothing() {
        let labels = ["bug".to_owned(), "enhancement".to_owned()];
        let claude = filing().claude(&labels, Path::new("/tmp/lune-issue-1.json"));
        assert_eq!(claude.get_current_dir(), Some(Path::new("/w/lune/main")));
        let model = [
            "-p",
            "--model",
            "sonnet",
            "--safe-mode",
            "--setting-sources",
            "user",
        ];
        let settings = ["--settings", "/tmp/lune-issue-1.json"];
        let permissions = ["--permission-mode", "dontAsk", "--tools", "Read,Grep,Glob"];
        let output = ["--no-session-persistence", "--output-format", "json"];
        let asked = schema(&labels);
        let schema = ["--json-schema", &asked, "--append-system-prompt", SYSTEM];
        assert_eq!(system(Harness::ClaudeCode), SYSTEM);
        let expected = [&model[..], &settings, &permissions, &output, &schema].concat();
        assert_eq!(args(&claude), expected);
        let removed = |name: &str| {
            claude
                .get_envs()
                .any(|(key, value)| key.to_str() == Some(name) && value.is_none())
        };
        let terminal = ["LUNE_MUX_TERMINAL", "LUNE_MUX_TERMINAL_MADE"];
        for name in terminal.into_iter().chain(git::REPOSITORY_VARS) {
            assert!(removed(name), "{name}");
        }
        assert_eq!(filing().words, "a red button");
        let schema: Value = serde_json::from_str(&asked).unwrap();
        assert_eq!(schema["required"], json!(["title", "body", "label"]));
        assert_eq!(
            schema["properties"]["label"],
            json!({"enum": ["bug", "enhancement", null]})
        );
        let unlabelled: Value = serde_json::from_str(&super::schema(&[])).unwrap();
        assert_eq!(unlabelled["properties"]["label"], json!({"enum": [null]}));
    }

    #[test]
    fn another_harness_writes_the_issue_with_no_tools() {
        let labels = ["bug".to_owned()];
        let terminal = |agent: &Command| {
            agent
                .get_envs()
                .any(|(key, value)| key == "LUNE_MUX_TERMINAL" && value.is_none())
        };
        let pi = filing().run_in(Harness::Pi);
        assert_eq!(Filing::of(pi.asked(None)), Ok((None, pi.clone())));
        let command = Filing::pi(Path::new("/tmp/lune-bare"));
        assert_eq!(command.get_program(), "pi");
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp/lune-bare")));
        let told = system(Harness::Pi);
        let tail = ["--system-prompt", told.as_str()];
        let said = [
            &["-p", "--no-session", "--no-tools"][..],
            &headless::PI_BARE,
            &tail,
        ]
        .concat();
        assert_eq!(args(&command), said);
        assert!(terminal(&command));
        assert!(
            told.contains(headless::TOOLLESS) && told.contains(UNREAD) && told.ends_with(SHAPED)
        );
        assert!(
            !told.contains("you may read") && !told.contains("the files"),
            "{told}"
        );

        let agy = filing().run_in(Harness::Antigravity);
        assert_eq!(Filing::of(agy.asked(None)), Ok((None, agy)));
        let command = Filing::agy(&labels, Path::new("/tmp/lune-bare"));
        assert_eq!(command.get_program(), "agy");
        assert_eq!(command.get_current_dir(), Some(Path::new("/tmp/lune-bare")));
        let told = system(Harness::Antigravity);
        assert!(told.contains(headless::TOOLLESS) && !told.contains(SHAPED));
        let asked = schema(&labels);
        let strict = [
            "--settings",
            headless::AGY_STRICT,
            "--disable-slash-commands",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ];
        assert_eq!(
            args(&command),
            [&strict[..], &["--json-schema", &asked]].concat()
        );
        assert!(terminal(&command));
        let huge = headless::agy_message(&format!("{told}\n\n{}", "x".repeat(128 * 1024)));
        let parsed: Value = serde_json::from_str(huge.trim_end()).unwrap();
        assert_eq!(parsed["event"], "user");
        let unknown = FileIssueRequest {
            harness: Some(99),
            ..filing().asked(None)
        };
        assert!(Filing::of(unknown).unwrap_err().starts_with("harness: "));
        let unsaid = FileIssueRequest {
            harness: None,
            ..filing().asked(None)
        };
        assert_eq!(Filing::of(unsaid).unwrap().1.runs_in(), Harness::ClaudeCode);
    }

    #[test]
    fn what_another_harness_wrote_reads_as_an_issue() {
        use std::os::unix::process::ExitStatusExt as _;
        let labels = ["bug".to_owned()];
        let red = || DraftIssue {
            title: Said::new("Red button".to_owned()).unwrap(),
            body: "It is red.".to_owned(),
            label: Some("bug".to_owned()),
        };
        let issue = r#"{"title": " Red button ", "body": "It is red.\n", "label": "bug"}"#;
        let pi =
            |said: &str| DraftIssue::parse(Harness::Pi, said_as_issue(Harness::Pi, said)?, &labels);
        assert_eq!(pi(issue), Ok(red()));
        assert_eq!(pi(&format!("```json\n{issue}\n```")), Ok(red()));
        assert_eq!(pi(""), Err("pi ended without writing an issue".to_owned()));
        let why = "pi wrote no issue, and said: Which button?".to_owned();
        assert_eq!(pi("Which button?"), Err(why));
        let mislabelled = r#"{"title": "Red", "body": "", "label": "bugs"}"#;
        let why = "pi took the label \"bugs\", which the repository has not".to_owned();
        assert_eq!(pi(mislabelled), Err(why));

        let agy = |record: Value| agy_written(&record, &labels);
        let structured: Value = serde_json::from_str(issue).unwrap();
        let wrote = json!({"conversation_id": "2766c4b3", "status": "SUCCESS", "response": issue,
                           "structured_output": structured});
        assert_eq!(agy(wrote), Ok(red()));
        let said = json!({"status": "SUCCESS", "response": format!("{issue}\n")});
        assert_eq!(agy(said), Ok(red()));
        let denied = json!({"status": "SUCCESS", "response": "",
                            "denied_actions": [{"action": "read_file", "display_name": "ReadFile"}]});
        assert_eq!(
            agy(denied),
            Err("agy ended without writing an issue".to_owned())
        );
        let failed = json!({"status": "ERROR", "response": "", "error": "Error: not\n signed in"});
        assert_eq!(
            agy(failed),
            Err("agy failed: Error: not signed in".to_owned())
        );
        assert_eq!(
            agy(json!({"status": "TIMEOUT"})),
            Err("agy failed: TIMEOUT".to_owned())
        );
        assert_eq!(agy(json!({})), Err("agy failed: the run failed".to_owned()));

        let ok = ExitStatus::from_raw(0);
        let err_status = ExitStatus::from_raw(1 << 8);
        let ended = |status, stdout: &str, stderr: &str| Ended {
            status,
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        };
        let valid_agy = [
            json!({"event": "init", "conversation_id": "2766c4b3"}).to_string(),
            json!({"event": "result", "result": {"status": "SUCCESS", "response": issue}})
                .to_string(),
        ]
        .join("\n");
        assert_eq!(
            parse_agy_issue(&ended(ok, &valid_agy, ""), &labels),
            Ok(red())
        );
        assert_eq!(
            parse_agy_issue(&ended(err_status, &valid_agy, "crashed"), &labels),
            Err("agy failed: crashed".to_owned())
        );
        assert_eq!(
            parse_agy_issue(&ended(err_status, &valid_agy, ""), &labels),
            Err(format!("agy failed: {err_status}"))
        );
        let err_json = json!({
            "event": "result",
            "result": {"status": "ERROR", "error": "not signed in"}
        })
        .to_string();
        assert_eq!(
            parse_agy_issue(&ended(err_status, &err_json, ""), &labels),
            Err("agy failed: not signed in".to_owned())
        );
        assert!(
            parse_agy_issue(&ended(ok, "Warning: stray text", ""), &labels)
                .unwrap_err()
                .starts_with("agy failed: invalid JSON: ")
        );
        assert_eq!(
            parse_agy_issue(&ended(ok, "", "not logged in"), &labels),
            Err("agy failed: not logged in".to_owned())
        );
        assert_eq!(
            parse_agy_issue(&ended(ok, "  ", ""), &labels),
            Err("agy failed: it said nothing it wrote".to_owned())
        );
        let init_only = json!({"event": "init", "conversation_id": "2766c4b3"}).to_string();
        assert_eq!(
            parse_agy_issue(&ended(ok, &init_only, ""), &labels),
            Err("agy failed: it said nothing it wrote".to_owned())
        );
        let valid_claude = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": issue,
        })
        .to_string();
        assert_eq!(
            parse_claude_issue(&ended(ok, &valid_claude, ""), &labels),
            Ok(red())
        );
        assert_eq!(
            parse_claude_issue(&ended(err_status, &valid_claude, "crashed"), &labels),
            Err("claude failed: crashed".to_owned())
        );
    }

    #[test]
    fn the_labels_are_those_gh_lists() {
        let listed = r#"[{"name":"bug"},{"name":"ui"}]"#;
        assert_eq!(names(listed), Ok(vec!["bug".to_owned(), "ui".to_owned()]));
        assert_eq!(names("[]"), Ok(Vec::new()));
        assert!(
            names("no labels")
                .unwrap_err()
                .starts_with("gh listed labels lune")
        );
    }

    #[test]
    fn the_agent_is_told_your_words_then_the_labels_and_your_issues() {
        let told = told("a red button", "[{\"name\":\"bug\"}]", "[]");
        let labels = "[lune: the repository's labels, as gh label list says them:]";
        let issues = "[lune: the last issues the user filed in it, newest first, as gh issue \
                      list says them:]";
        let listed = format!("{labels}\n[{{\"name\":\"bug\"}}]\n\n{issues}\n[]");
        assert_eq!(told, format!("a red button\n\n{listed}"));
    }

    #[test]
    fn nothing_said_is_no_issue() {
        let dir = || Directory::parse("/w".to_owned()).unwrap();
        let blank = Filing::new(dir(), on("o/n"), " \n ");
        assert_eq!(blank, Err("words: nothing said".to_owned()));
        let asked = filing().asked(None);
        assert_eq!(asked.repository, "o/lune");
        assert_eq!(Filing::of(asked.clone()), Ok((None, filing())));
        let relative = FileIssueRequest {
            dir: "w".to_owned(),
            ..asked.clone()
        };
        assert_eq!(
            Filing::of(relative),
            Err("dir: \"w\" is no absolute path".to_owned())
        );
        let nowhere = FileIssueRequest {
            repository: String::new(),
            ..asked.clone()
        };
        let why = "repository: \"\" is no repository's owner/name".to_owned();
        assert_eq!(Filing::of(nowhere), Err(why));
        let blank = FileIssueRequest {
            words: " ".to_owned(),
            ..asked
        };
        assert!(Filing::of(blank).is_err());
    }

    #[test]
    fn gh_is_told_the_repository_the_issue_goes_to() {
        let gh = filing().gh(&["label", "list"]);
        assert_eq!(args(&gh), ["label", "list", "--repo", "github.com/o/lune"]);
        assert_eq!(gh.get_current_dir(), Some(Path::new("/w/lune/main")));
    }

    #[test]
    fn the_remotes_on_github_go_in_the_order_gh_takes_them() {
        let ssh_host = |host: &str| (host == "github-work").then(|| "github.com".to_owned());
        let names = |listed: &str| -> Vec<(String, String)> {
            let remotes = on_github(listed, ssh_host).into_iter();
            remotes
                .map(|remote| (remote.name, remote.on.to_string()))
                .collect()
        };
        let pair = |name: &str, on: &str| (name.to_owned(), on.to_owned());
        let fork = "remote.mine.url git@github-work:me/cdk.git\n\
                    remote.origin.url git@github.com:lorenzolfm/cdk.git\n\
                    remote.lab.url git@gitlab.com:o/cdk.git\n\
                    remote.upstream.url https://github.com/cashubtc/cdk\n\
                    remote.again.url https://github.com/CashuBTC/CDK.git\n";
        assert_eq!(
            names(fork),
            [
                pair("upstream", "cashubtc/cdk"),
                pair("origin", "lorenzolfm/cdk"),
                pair("mine", "me/cdk"),
            ]
        );
        let set = format!("{fork}remote.origin.gh-resolved base\n");
        let first = [
            pair("origin", "lorenzolfm/cdk"),
            pair("upstream", "cashubtc/cdk"),
        ];
        assert_eq!(names(&set)[..2], first);
        assert_eq!(names("remote.lab.url git@gitlab.com:o/cdk.git\n"), []);
        assert_eq!(names(""), []);
    }

    #[test]
    fn a_daemon_s_remote_reads_as_it_was() {
        let remote = Remote {
            name: "upstream".to_owned(),
            on: on("cashubtc/cdk"),
        };
        assert_eq!(Remote::of(remote.clone().answer()), Ok(remote.clone()));
        let nameless = IssueRepository {
            remote: String::new(),
            ..remote.clone().answer()
        };
        assert!(Remote::of(nameless).unwrap_err().contains("has no name"));
        for spelled in ["cashubtc", "cashubtc/cdk/x", "/cdk", "o/..", "o n/cdk"] {
            let repository = spelled.to_owned();
            let answer = IssueRepository {
                repository,
                ..remote.clone().answer()
            };
            assert!(Remote::of(answer).is_err(), "{spelled}");
        }
        assert!(on("CashuBTC/CDK").is(&on("cashubtc/cdk")));
        assert!(!on("lorenzolfm/cdk").is(&on("cashubtc/cdk")));
    }

    #[test]
    fn what_the_agent_wrote_reads_from_the_run_s_end() {
        let labels = ["bug".to_owned()];
        let written = |record: &Value| claude_written(record, &labels);
        let ended = |structured: Value| {
            json!({"type": "result", "subtype": "success", "is_error": false,
                   "result": "{}", "structured_output": structured})
        };
        let wrote =
            ended(json!({"title": " Red\n button ", "body": "It is red.\n", "label": "bug"}));
        let label = Some("bug".to_owned());
        let red = DraftIssue {
            title: Said::new("Red button".to_owned()).unwrap(),
            body: "It is red.".to_owned(),
            label,
        };
        assert_eq!(written(&wrote), Ok(red));
        let unlabelled = ended(json!({"title": "Red", "body": "", "label": null}));
        assert_eq!(written(&unlabelled).unwrap().label, None);
        let mislabelled = ended(json!({"title": "Red", "body": "", "label": "bugs"}));
        let why = "claude took the label \"bugs\", which the repository has not".to_owned();
        assert_eq!(written(&mislabelled), Err(why));
        let untitled = ended(json!({"title": " ", "body": "", "label": null}));
        assert_eq!(
            written(&untitled),
            Err("claude wrote an issue with no title".to_owned())
        );
        let misshapen = ended(json!({"title": "Red"}));
        assert!(
            written(&misshapen)
                .unwrap_err()
                .starts_with("claude wrote no issue lune reads")
        );
        let unwritten = json!({"type": "result", "subtype": "success", "is_error": false});
        let why = "claude ended without writing an issue".to_owned();
        assert_eq!(written(&unwritten), Err(why.clone()));
        let empty = json!({"type": "result", "subtype": "success", "is_error": false,
                           "result": " ", "structured_output": null});
        assert_eq!(written(&empty), Err(why));
        let talked = |said: &str| json!({"type": "result", "subtype": "success", "is_error": false, "result": said});
        let why = "claude wrote no issue, and said: Which button? The red one?".to_owned();
        assert_eq!(written(&talked("Which button?\n\nThe red one?")), Err(why));
        let issue = r#"{"title": "Red", "body": "It is red.", "label": "bug"}"#;
        for said in [
            issue.to_owned(),
            format!("```json\n{issue}\n```"),
            format!("```\n{issue}```"),
        ] {
            assert_eq!(
                written(&talked(&said)).unwrap().title.as_str(),
                "Red",
                "{said}"
            );
        }
        let misspelled = r#"{"title": "Red", "body": "", "labels": ["bug"]}"#;
        let unlabelled = r#"{"title": "Red", "body": ""}"#;
        for said in [misspelled, unlabelled] {
            let why = written(&talked(said)).unwrap_err();
            assert!(
                why.starts_with("claude wrote no issue lune reads"),
                "{said}: {why}"
            );
        }
        let failed = json!({"type": "result", "subtype": "success", "is_error": true,
                            "result": "Not logged in · Please run /login"});
        let why = "claude failed: Not logged in · Please run /login".to_owned();
        assert_eq!(written(&failed), Err(why));
        let maxed = json!({"type": "result", "subtype": "error_max_turns", "is_error": true});
        assert_eq!(
            written(&maxed),
            Err("claude failed: error_max_turns".to_owned())
        );
    }

    #[test]
    fn the_issue_s_number_is_the_last_part_of_its_page() {
        let number = |url: &str| Filed::page(url, "Red").ok().map(|f| f.number().get());
        assert_eq!(number("https://github.com/o/n/issues/355"), Some(355));
        for url in [
            "https://ghe.example.com/o/n/issues/7",
            "https://github.com/a/b/c/issues/5",
            "https://github.com/o/n/pull/355",
            "https://github.com/o/n/issues/0",
            "https://github.com/o/n/issues/",
            "https://github.com/o/issues/3",
            "Creating issue in o/n",
        ] {
            assert_eq!(number(url), None, "{url}");
        }
    }

    #[test]
    fn the_agent_may_read_nothing_git_ignores_nor_a_git_directory() {
        let ignored = b".env\0target/\0a*b(1).secret\0";
        assert_eq!(
            rules("/w/n/", ignored),
            [
                "Read(//w/n/.env)",
                "Read(//w/n/target/**)",
                r"Read(//w/n/a\*b\(1\).secret)",
                "Read(//w/n/**/.git/**)",
            ]
        );
        assert_eq!(rules("/w/[n]", b""), [r"Read(//w/\[n\]/**/.git/**)"]);
        assert_eq!(
            rules("/w/n", b"./\0.env\0"),
            [
                "Read(//w/n/**)",
                "Read(//w/n/.env)",
                "Read(//w/n/**/.git/**)",
            ]
        );
        assert_eq!(rules("/w/n", b".env\0\xff.env\0"), ["Read(//w/n/**)"]);
        let bare = tempfile::tempdir().unwrap();
        let real = bare.path().canonicalize().unwrap().display().to_string();
        git::scratch(&real, &["init", "--quiet", "--bare"]).unwrap();
        let all = denied(&format!("{real}/"));
        assert!(
            unread(&Directory::parse(real).unwrap()).contains(&all),
            "{all}"
        );
        let file = settings(&["Read(//w/n/.env)".to_owned()]).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(file.path()).unwrap()).unwrap();
        assert_eq!(
            written,
            json!({"permissions": {"deny": ["Read(//w/n/.env)"]}})
        );
    }

    #[test]
    fn gh_filed_none_where_it_stopped_short_or_github_said_why() {
        use std::os::unix::process::ExitStatusExt as _;
        let failed = ExitStatus::from_raw(1 << 8);
        let unauthenticated = ExitStatus::from_raw(4 << 8);
        for why in [
            "could not add label: 'bugs' not found",
            "the 'o/n' repository has disabled issues",
            "GraphQL: was submitted too quickly (createIssue)",
            "HTTP 403: Resource not accessible by integration (https://api.github.com/graphql)",
        ] {
            assert!(filed_none(why, failed), "{why}");
        }
        let login = "To get started with GitHub CLI, please run:  gh auth login";
        assert!(filed_none(login, unauthenticated));
        for why in [
            r#"Post "https://api.github.com/graphql": read: connection reset by peer"#,
            "HTTP 502: Bad Gateway (https://api.github.com/graphql)",
            "exit status: 1",
        ] {
            assert!(!filed_none(why, failed), "{why}");
        }
        let killed = ExitStatus::from_raw(9);
        assert!(!filed_none(&killed.to_string(), killed));
    }

    #[test]
    fn why_gh_filed_nothing_is_past_where_it_was_filing() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = std::process::ExitStatus::from_raw(1 << 8);
        let said = b"\nCreating issue in o/n\n\ncould not add label: 'bugs' not found\n";
        assert_eq!(
            refused(said, status),
            "could not add label: 'bugs' not found"
        );
        assert_eq!(refused(b"", status), "exit status: 1");
    }

    #[test]
    fn a_daemon_s_answer_reads_as_the_issue_filed() {
        let url = "https://github.com/o/n/issues/355".to_owned();
        let filed = Filed::page(&url, " Red\n button ").unwrap();
        let said = (filed.number().get(), filed.title(), filed.link().url());
        assert_eq!(said, (355, "Red button", url.clone()));
        assert_eq!(Filed::of(filed.clone().answer()), Ok(filed.clone()));
        let pull = "https://github.com/o/n/pull/355".to_owned();
        let pageless = FileIssueResponse {
            url: pull,
            ..filed.clone().answer()
        };
        assert!(Filed::of(pageless).unwrap_err().contains("no issue's page"));
        let untitled = FileIssueResponse {
            title: " ".to_owned(),
            ..filed.answer()
        };
        assert_eq!(
            Filed::of(untitled),
            Err("issue #355 has no title".to_owned())
        );
    }

    #[test]
    fn why_none_was_filed_says_whether_one_may_have_been() {
        let refused = Unfiled::Refused("gh: no login".to_owned());
        let in_doubt = Unfiled::InDoubt("gh: it did not answer in time".to_owned());
        for unfiled in [refused, in_doubt] {
            let status = unfiled.clone().status();
            let read = Unfiled::of(status.message().to_owned(), Some(status.code()));
            assert_eq!(read, unfiled);
        }
        let why = || "w".to_owned();
        for code in [
            Code::FailedPrecondition,
            Code::InvalidArgument,
            Code::Unimplemented,
        ] {
            assert_eq!(
                Unfiled::of(why(), Some(code)),
                Unfiled::Refused(why()),
                "{code:?}"
            );
        }
        for code in [
            Some(Code::Unavailable),
            Some(Code::Internal),
            Some(Code::Unknown),
            None,
        ] {
            assert_eq!(
                Unfiled::of(why(), code),
                Unfiled::InDoubt(why()),
                "{code:?}"
            );
        }
    }
}
