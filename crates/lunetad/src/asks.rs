use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::ask_response::{self, Ran};
use crate::api::{AskRequest, AskResponse, On};
use crate::transcripts::SessionId;
use crate::mux;
use crate::work::{self, Item};
use crate::{git, program};

pub const MODEL: &str = "sonnet";

pub const TURN: Duration = Duration::from_mins(10);

pub const LOOK: Duration = Duration::from_millis(5);

pub const TURN_VAR: &str = "LUNETA_TURN";
pub const BEFORE_VAR: &str = "LUNETA_TURN_BEFORE";

pub const ALLOWED: [&str; 11] = [
    "Bash(luneta start:*)",
    "Bash(luneta agents:*)",
    "Bash(luneta prune:*)",
    "Bash(zoxide query:*)",
    "Bash(gh issue view:*)",
    "Bash(gh issue list:*)",
    "Bash(gh pr view:*)",
    "Bash(gh pr list:*)",
    "Bash(luneta worktree:*)",
    "Bash(git worktree add:*)",
    "Bash(git worktree list:*)",
];

pub const SKILL: &str = include_str!("../../../skills/luneta/SKILL.md");

pub const SYSTEM: &str = "You run in luneta's ask box. The user typed what they want in luneta's \
    sidebar, and reads what you say in a small box in luneta: write plain text for a terminal, \
    with no markdown, no headings, no tables and no code fences, and keep it short. Do what \
    they ask with luneta's commands, as the luneta skill below says, and end with a line or two \
    on what you did, and where. Run one command in each call to Bash, with nothing around it: \
    no $(…), no pipes, no && and no ;. Only these run: luneta start, agents and prune, zoxide \
    query, gh issue view and list, gh pr view and list, luneta worktree, and git worktree add \
    and list. Claude Code denies any other. You cannot wait for the user in the middle of a \
    turn: to ask them anything, as the skill has you ask before a prune, or which of two things \
    they meant, end your turn with the question. Their answer comes as the next message. So a \
    prune takes --yes only in the turn after the one that said its plan.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    key: u64,
    before: Option<u64>,
}

impl Turn {
    #[must_use]
    pub fn after(before: Option<u64>) -> Turn {
        let key = RandomState::new().build_hasher().finish();
        let key = if before == Some(key) { !key } else { key };
        Turn { key, before }
    }

    pub fn new(key: u64, before: Option<u64>) -> Result<Turn, String> {
        if before == Some(key) {
            return Err(format!("turn: {key:016x} is the key of the turn before it too"));
        }
        Ok(Turn { key, before })
    }

    #[must_use]
    pub fn key(self) -> u64 {
        self.key
    }

    #[must_use]
    pub fn before(self) -> Option<u64> {
        self.before
    }

    pub fn name_in(self, command: &mut Command) {
        command.env(TURN_VAR, format!("{:016x}", self.key));
        match self.before {
            Some(before) => command.env(BEFORE_VAR, format!("{before:016x}")),
            None => command.env_remove(BEFORE_VAR),
        };
    }

    pub fn read(key: Option<&str>, before: Option<&str>) -> Result<Option<Turn>, String> {
        let read = |name, spelled: &str| {
            let read = u64::from_str_radix(spelled, 16);
            read.map_err(|_| format!("{name} is no key of a turn of luneta's ask box"))
        };
        match (key, before) {
            (None, None) => Ok(None),
            (None, Some(_)) => Err(format!("{BEFORE_VAR} is set, and {TURN_VAR} is not")),
            (Some(key), before) => {
                let before = before.map(|before| read(BEFORE_VAR, before)).transpose()?;
                Turn::new(read(TURN_VAR, key)?, before).map(Some)
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Session(SessionId),
    Ran { id: String, command: String },
    Denied(String),
    Said(String),
}

impl Step {
    #[must_use]
    pub fn answer(self) -> AskResponse {
        let step = match self {
            Step::Session(session) => ask_response::Step::Session(session.as_str().to_owned()),
            Step::Ran { id, command } => ask_response::Step::Ran(Ran { id, command }),
            Step::Denied(id) => ask_response::Step::Denied(id),
            Step::Said(said) => ask_response::Step::Said(said),
        };
        AskResponse { step: Some(step) }
    }

    pub fn of(answer: AskResponse) -> Result<Option<Step>, String> {
        let Some(step) = answer.step else {
            return Ok(None);
        };
        Ok(Some(match step {
            ask_response::Step::Session(session) => match SessionId::parse(&session) {
                Some(session) => Step::Session(session),
                None => return Err(format!("session: {session:?} names no session")),
            },
            ask_response::Step::Ran(Ran { id, command }) => Step::Ran { id, command },
            ask_response::Step::Denied(id) => Step::Denied(id),
            ask_response::Step::Said(said) => Step::Said(said),
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    cwd: Option<PathBuf>,
    words: String,
    resumed: Option<SessionId>,
    highlighted: String,
    turn: Turn,
    marked: Marked,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Marked {
    marked: BTreeSet<Item>,
}

impl Marked {
    pub fn new(
        issues: impl IntoIterator<Item = NonZeroU32>,
        pulls: impl IntoIterator<Item = NonZeroU32>,
    ) -> Result<Marked, String> {
        let mut marked = Marked::default();
        marked.of_kind("issues", issues.into_iter().map(Item::Issue))?;
        marked.of_kind("pull requests", pulls.into_iter().map(Item::Pull))?;
        Ok(marked)
    }

    fn of_kind(&mut self, kind: &str, items: impl IntoIterator<Item = Item>) -> Result<(), String> {
        for item in items {
            if self.like(item) == work::LISTED_MAX {
                return Err(format!("marked: at most {} {kind}", work::LISTED_MAX));
            }
            if !self.marked.insert(item) {
                return Err(format!("marked: #{} is among the {kind} twice", item.number()));
            }
        }
        Ok(())
    }

    pub fn toggle(&mut self, item: Item) {
        if self.marked.remove(&item) {
            return;
        }
        assert!(self.like(item) < work::LISTED_MAX, "at most so many of a kind are listed to mark");
        self.marked.insert(item);
    }

    pub fn retain(&mut self, kept: impl Fn(&Item) -> bool) {
        self.marked.retain(kept);
    }

    pub fn clear(&mut self) {
        self.marked.clear();
    }

    #[must_use]
    pub fn contains(&self, item: &Item) -> bool {
        self.marked.contains(item)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Item> {
        self.marked.iter()
    }

    fn like(&self, item: Item) -> usize {
        let kind = |item: &Item| matches!(item, Item::Issue(_));
        self.marked.iter().filter(|marked| kind(marked) == kind(&item)).count()
    }

    fn issues(&self) -> impl Iterator<Item = NonZeroU32> + '_ {
        self.marked.iter().filter_map(|item| match item {
            Item::Issue(number) => Some(*number),
            Item::Pull(_) => None,
        })
    }

    fn pulls(&self) -> impl Iterator<Item = NonZeroU32> + '_ {
        self.marked.iter().filter_map(|item| match item {
            Item::Pull(number) => Some(*number),
            Item::Issue(_) => None,
        })
    }

    fn read(issues: Vec<u32>, pulls: Vec<u32>) -> Result<Marked, String> {
        let numbered = |numbers: Vec<u32>| {
            let numbers = numbers.into_iter().map(NonZeroU32::new);
            numbers.collect::<Option<Vec<_>>>().ok_or("marked: no issue or pull request is #0")
        };
        Marked::new(numbered(issues)?, numbered(pulls)?)
    }

    fn line(&self) -> Option<String> {
        let listed = |kind: &str, numbers: Vec<NonZeroU32>| {
            let numbers: Vec<String> = numbers.iter().map(|number| format!("#{number}")).collect();
            let (last, rest) = numbers.split_last()?;
            Some(match rest {
                [] => format!("{kind} {last}"),
                rest => format!("{kind}s {} and {last}", rest.join(", ")),
            })
        };
        let issues = listed("issue", self.issues().collect());
        let pulls = listed("pull request", self.pulls().collect());
        let marked = match (issues, pulls) {
            (None, None) => return None,
            (Some(one), None) | (None, Some(one)) => one,
            (Some(issues), Some(pulls)) => format!("{issues}, and {pulls}"),
        };
        Some(format!(
            "[luneta: in the project's work view the user marked {marked}: \"these\" or \"them\" \
             may mean them.]"
        ))
    }
}

impl Asked {
    pub fn new(
        cwd: Option<PathBuf>,
        words: &str,
        resumed: Option<SessionId>,
        highlighted: String,
        turn: Turn,
    ) -> Result<Asked, String> {
        if let Some(cwd) = cwd.as_ref().filter(|cwd| !cwd.is_absolute()) {
            return Err(format!("cwd: {} is no absolute path", cwd.display()));
        }
        let words = words.trim();
        if words.is_empty() {
            return Err("words: nothing asked".to_owned());
        }
        let marked = Marked::default();
        Ok(Asked { cwd, words: words.to_owned(), resumed, highlighted, turn, marked })
    }

    #[must_use]
    pub fn marking(self, marked: Marked) -> Asked {
        Asked { marked, ..self }
    }

    pub fn of(request: AskRequest) -> Result<(Option<On>, Asked), String> {
        let AskRequest { on, cwd, words, resumed, highlighted, turn, before, marked_issues, marked_pulls } =
            request;
        let resumed = match resumed {
            None => None,
            Some(resumed) => Some(
                SessionId::parse(&resumed)
                    .ok_or_else(|| format!("resumed: {resumed:?} names no session"))?,
            ),
        };
        let turn = Turn::new(turn, before)?;
        let cwd = cwd.map(PathBuf::from);
        let marked = Marked::read(marked_issues, marked_pulls)?;
        let asked = Asked::new(cwd, &words, resumed, highlighted, turn)?;
        Ok((on, asked.marking(marked)))
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>) -> AskRequest {
        AskRequest {
            on,
            cwd: self.cwd.as_ref().map(|cwd| cwd.to_string_lossy().into_owned()),
            words: self.words.clone(),
            resumed: self.resumed.as_ref().map(|resumed| resumed.as_str().to_owned()),
            highlighted: self.highlighted.clone(),
            turn: self.turn.key,
            before: self.turn.before,
            marked_issues: self.marked.issues().map(NonZeroU32::get).collect(),
            marked_pulls: self.marked.pulls().map(NonZeroU32::get).collect(),
        }
    }

    pub fn talk(
        &self,
        wanted: &dyn Fn() -> bool,
        hear: &dyn Fn(Step),
    ) -> Option<Result<(), String>> {
        talk(self.claude(), &told(&self.marked, &self.words), wanted, hear)
    }

    fn claude(&self) -> Command {
        let mut claude = Command::new("claude");
        claude.args(["-p", "--model", MODEL, "--safe-mode", "--setting-sources", "user"]);
        claude.args(["--permission-mode", "dontAsk", "--tools", "Bash"]);
        claude.args(["--output-format", "stream-json", "--verbose"]);
        claude.arg("--append-system-prompt").arg(system(&self.highlighted));
        if let Some(session) = &self.resumed {
            claude.args(["--resume", session.as_str()]);
        }
        claude.arg("--allowedTools").args(ALLOWED);
        mux::own::in_no_terminal(&mut claude);
        git::local(&mut claude);
        self.turn.name_in(&mut claude);
        if let Some(path) = env::current_exe().ok().and_then(|exe| path_with(&exe)) {
            claude.env("PATH", path);
        }
        match (&self.cwd, env::var_os("HOME")) {
            (Some(cwd), _) => claude.current_dir(cwd),
            (None, Some(home)) => claude.current_dir(home),
            (None, None) => &mut claude,
        };
        claude
    }
}

fn system(highlighted: &str) -> String {
    format!("{SYSTEM}\n\n{highlighted}\n\n{SKILL}")
}

fn told(marked: &Marked, words: &str) -> String {
    match marked.line() {
        Some(marked) => format!("{marked}\n\n{words}"),
        None => words.to_owned(),
    }
}

fn path_with(exe: &Path) -> Option<OsString> {
    if exe.file_name()? != "luneta" && !exe.with_file_name("luneta").is_file() {
        return None;
    }
    let path = env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(exe.parent()?.to_owned()).chain(env::split_paths(&path));
    env::join_paths(dirs).ok()
}

#[derive(Debug, PartialEq, Eq)]
enum Read {
    Step(Step),
    Ended(Result<String, String>),
}

fn read(line: &str) -> Vec<Read> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match record["type"].as_str() {
        Some("system") if record["subtype"] == "init" => {
            let session = record["session_id"].as_str().and_then(SessionId::parse);
            session.map(|session| Read::Step(Step::Session(session))).into_iter().collect()
        },
        Some("assistant") if record["parent_tool_use_id"].is_null() => {
            let blocks = record["message"]["content"].as_array().into_iter().flatten();
            blocks.filter_map(step).map(Read::Step).collect()
        },
        Some("result") => {
            let denials = record["permission_denials"].as_array().into_iter().flatten();
            let denied = denials.filter_map(|denial| denial["tool_use_id"].as_str());
            let mut read: Vec<_> = denied.map(|id| Read::Step(Step::Denied(id.to_owned()))).collect();
            let said = record["result"].as_str().unwrap_or_default().trim().to_owned();
            let failed = record["is_error"].as_bool() != Some(false) || record["subtype"] != "success";
            let why = || match record["subtype"].as_str() {
                _ if !said.is_empty() => said.clone(),
                Some(subtype) => subtype.to_owned(),
                None => "the turn failed".to_owned(),
            };
            read.push(Read::Ended(if failed { Err(why()) } else { Ok(said.clone()) }));
            read
        },
        _ => Vec::new(),
    }
}

fn step(block: &Value) -> Option<Step> {
    match block["type"].as_str()? {
        "text" => {
            let text = block["text"].as_str()?.trim();
            (!text.is_empty()).then(|| Step::Said(text.to_owned()))
        },
        "tool_use" => {
            let ran = block["input"]["command"].as_str().or_else(|| block["name"].as_str())?;
            let id = block["id"].as_str().unwrap_or_default().to_owned();
            Some(Step::Ran { id, command: ran.to_owned() })
        },
        _ => None,
    }
}

fn talk(
    mut command: Command,
    words: &str,
    wanted: &dyn Fn() -> bool,
    hear: &dyn Fn(Step),
) -> Option<Result<(), String>> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let gone = command.get_current_dir().filter(|cwd| !cwd.is_dir());
            return Some(Err(match gone {
                Some(cwd) => format!("{}, where the agent works, is gone", cwd.display()),
                None => "claude is not on the PATH".to_owned(),
            }));
        },
        Err(error) => return Some(Err(format!("claude could not be run: {error}"))),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(words.as_bytes());
    }
    let stdout = child.stdout.take().expect("its stdout is piped");
    let (said, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if said.send(line).is_err() {
                return;
            }
        }
    });
    let stderr = child.stderr.take().expect("its stderr is piped");
    let stderr = thread::spawn(move || {
        let mut said = Vec::new();
        let _ = stderr.take(program::SAID_MAX).read_to_end(&mut said);
        said
    });
    let deadline = Instant::now() + TURN;
    let (mut ended, mut spoke) = (None, false);
    loop {
        match lines.recv_timeout(LOOK) {
            Ok(Ok(line)) => {
                for read in read(&line) {
                    match read {
                        Read::Step(step) => {
                            spoke |= matches!(step, Step::Said(_));
                            hear(step);
                        },
                        Read::Ended(end) => ended = Some(end),
                    }
                }
            },
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {},
        }
        let stuck = Instant::now() >= deadline;
        if stuck || !wanted() {
            let _ = child.kill();
            let _ = child.wait();
            let minutes = TURN.as_secs() / 60;
            return stuck.then(|| Err(format!("claude was still at it after {minutes} minutes")));
        }
    }
    let status = child.wait();
    let stderr = stderr.join().unwrap_or_default();
    Some(match (ended, status) {
        (Some(Err(why)), _) => Err(why),
        (Some(Ok(last)), Ok(status)) if status.success() => {
            if !spoke && !last.is_empty() {
                hear(Step::Said(last));
            }
            Ok(())
        },
        (_, Ok(status)) => Err(failed(&stderr, status)),
        (_, Err(error)) => Err(format!("claude could not be waited for: {error}")),
    })
}

fn failed(stderr: &[u8], status: ExitStatus) -> String {
    let said = String::from_utf8_lossy(stderr);
    match said.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => format!("claude failed: {line}"),
        None if status.success() => "claude ended without saying how the turn went".to_owned(),
        None => format!("claude failed: {status}"),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use serde_json::json;

    use super::*;
    use crate::program::tests::{given_up, outlasting_patience};

    fn turn() -> Turn {
        Turn { key: 255, before: Some(10) }
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_agent_runs_where_it_is_asked_for() {
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        let highlighted = "The row highlighted is terminal 4 of luneta.".to_owned();
        let cwd = Some(PathBuf::from("/w/luneta"));
        let first = Asked::new(cwd, " prune it ", None, highlighted, turn()).unwrap();
        let claude = first.claude();
        assert_eq!(claude.get_current_dir(), Some(Path::new("/w/luneta")));
        let said = args(&claude);
        let model = ["-p", "--model", "sonnet", "--safe-mode", "--setting-sources", "user"];
        let permissions = ["--permission-mode", "dontAsk", "--tools", "Bash"];
        let output = ["--output-format", "stream-json", "--verbose"];
        assert_eq!(said[..13], [&model[..], &permissions, &output].concat());
        assert_eq!(said[13], "--append-system-prompt");
        let system = format!("{SYSTEM}\n\nThe row highlighted is terminal 4 of luneta.\n\n{SKILL}");
        assert_eq!(said[14], system);
        assert_eq!(said[15..], [&["--allowedTools"][..], &ALLOWED[..]].concat());
        assert_eq!(first.words, "prune it");
        let terminal = ["LUNETA_MUX_TERMINAL", "LUNETA_MUX_TERMINAL_MADE"];
        for name in terminal.into_iter().chain(git::REPOSITORY_VARS) {
            assert_eq!(var(&claude, name), Some(None), "{name}");
        }
        assert_eq!(var(&claude, TURN_VAR), Some(Some("00000000000000ff".to_owned())));
        assert_eq!(var(&claude, BEFORE_VAR), Some(Some("000000000000000a".to_owned())));

        let resumed = Some(session("7c22"));
        let next = Asked::new(None, "yes", resumed, String::new(), turn());
        let claude = next.unwrap().claude();
        assert_eq!(claude.get_current_dir(), env::var_os("HOME").as_deref().map(Path::new));
        assert_eq!(args(&claude)[15..17], ["--resume", "7c22"]);

        let first = Turn { key: 10, before: None };
        let claude = Asked::new(None, "yes", None, String::new(), first).unwrap().claude();
        assert_eq!(var(&claude, TURN_VAR), Some(Some("000000000000000a".to_owned())));
        assert_eq!(var(&claude, BEFORE_VAR), Some(None));
    }

    #[test]
    fn the_agent_s_commands_read_the_turn() {
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.and_then(|(_, value)| value).map(|value| value.to_string_lossy().into_owned())
        };
        let first = Turn::new(10, None).unwrap();
        for turn in [first, turn()] {
            let claude = Asked::new(None, "yes", None, String::new(), turn).unwrap().claude();
            let [key, before] = [TURN_VAR, BEFORE_VAR].map(|name| var(&claude, name));
            assert_eq!(Turn::read(key.as_deref(), before.as_deref()), Ok(Some(turn)));
        }
        assert_eq!(Turn::read(None, None), Ok(None));
    }

    #[test]
    fn the_agent_s_commands_read_no_turn_the_box_could_not_name() {
        assert_eq!(Turn::read(Some("ff"), Some("0a")), Ok(Some(turn())));
        for (key, before) in [
            (Some(""), None),
            (Some("forged"), None),
            (Some("ff"), Some("")),
            (None, Some("ff")),
            (Some("ff"), Some("ff")),
        ] {
            assert!(Turn::read(key, before).is_err(), "{key:?} {before:?}");
        }
    }

    fn marked(issues: &[u32], pulls: &[u32]) -> Marked {
        let numbers = |numbers: &[u32]| numbers.iter().map(|it| NonZeroU32::new(*it).unwrap()).collect::<Vec<_>>();
        Marked::new(numbers(issues), numbers(pulls)).unwrap()
    }

    #[test]
    fn marks_go_to_the_most_listed_and_none_twice() {
        let numbers = |numbers: std::ops::RangeInclusive<u32>| numbers.map(|it| NonZeroU32::new(it).unwrap());
        assert!(Marked::new(numbers(1..=50), numbers(1..=50)).is_ok());
        let refused = Err("marked: at most 50 issues".to_owned());
        assert_eq!(Marked::new(numbers(1..=51), numbers(1..=1)), refused);
        let twice = Marked::new(numbers(1..=1), [NonZeroU32::MIN, NonZeroU32::MIN]);
        assert_eq!(twice, Err("marked: #1 is among the pull requests twice".to_owned()));
    }

    #[test]
    fn a_mark_made_again_is_taken_off_and_an_issue_and_a_pull_request_of_one_number_are_two() {
        let one = NonZeroU32::MIN;
        let mut marked = Marked::default();
        marked.toggle(Item::Issue(one));
        marked.toggle(Item::Pull(one));
        assert_eq!(marked, Marked::new([one], [one]).unwrap());
        marked.toggle(Item::Issue(one));
        assert_eq!(marked, Marked::new([], [one]).unwrap(), "a second mark takes it off");
        marked.retain(|item| matches!(item, Item::Issue(_)));
        assert_eq!(marked, Marked::default());
    }

    #[test]
    fn what_the_user_marked_goes_with_the_turn_before_their_words() {
        let told = |marked: Marked| told(&marked, "tackle these");
        let said = told(marked(&[14, 12], &[20]));
        let line = "[luneta: in the project's work view the user marked issues #12 and #14, and \
                    pull request #20: \"these\" or \"them\" may mean them.]";
        assert_eq!(said, format!("{line}\n\ntackle these"));
        let marks = |said: String| said.lines().next().map(str::to_owned).unwrap();
        assert!(marks(told(marked(&[3], &[]))).contains("marked issue #3:"));
        assert!(marks(told(marked(&[1, 2, 3], &[]))).contains("marked issues #1, #2 and #3:"));
        assert!(marks(told(marked(&[], &[7, 9]))).contains("marked pull requests #7 and #9:"));
        assert_eq!(told(Marked::default()), "tackle these", "an unmarked turn is the words alone");
    }

    #[test]
    fn a_turn_goes_by_its_own_key_and_the_one_before() {
        let first = Turn::after(None);
        let second = Turn::after(Some(first.key()));
        let third = Turn::after(Some(second.key()));
        let befores = [first.before(), second.before(), third.before()];
        assert_eq!(befores, [None, Some(first.key()), Some(second.key())]);
        assert!(first.key() != second.key() && second.key() != third.key());
    }

    #[test]
    fn a_turn_is_asked_of_a_daemon_as_it_is() {
        let on = || Some(On { destination: "nixos".to_owned() });
        for (cwd, resumed) in [(Some("/w".into()), None), (None, Some(session("7c22")))] {
            let asked = Asked::new(cwd, "prune it", resumed, "row".to_owned(), turn()).unwrap();
            assert_eq!(Asked::of(asked.asked(on())), Ok((on(), asked.clone())));
            let marked = asked.marking(marked(&[12, 14], &[20]));
            assert_eq!(Asked::of(marked.asked(on())), Ok((on(), marked)));
        }
        let request = || {
            let asked = Asked::new(None, "prune it", None, "row".to_owned(), turn());
            asked.unwrap().asked(None)
        };
        let invalid = [
            AskRequest { cwd: Some(String::new()), ..request() },
            AskRequest { cwd: Some("w".to_owned()), ..request() },
            AskRequest { words: " \n".to_owned(), ..request() },
            AskRequest { resumed: Some("--debug".to_owned()), ..request() },
            AskRequest { turn: 7, before: Some(7), ..request() },
            AskRequest { marked_issues: vec![12, 0], ..request() },
            AskRequest { marked_pulls: (1..=51).collect(), ..request() },
            AskRequest { marked_issues: vec![12, 12], ..request() },
        ];
        for request in invalid {
            assert!(Asked::of(request.clone()).is_err(), "{request:?}");
        }
    }

    #[test]
    fn a_step_is_handed_on_as_it_is() {
        let ran = Step::Ran { id: "toolu_1".to_owned(), command: "luneta agents".to_owned() };
        let steps = [
            Step::Session(session("7c22")),
            ran,
            Step::Denied("toolu_1".to_owned()),
            Step::Said("Done.".to_owned()),
        ];
        for step in steps {
            assert_eq!(Step::of(step.clone().answer()), Ok(Some(step)));
        }
        assert_eq!(Step::of(AskResponse { step: None }), Ok(None));
        let unnamed = AskResponse { step: Some(ask_response::Step::Session("../x".to_owned())) };
        assert_eq!(Step::of(unnamed), Err("session: \"../x\" names no session".to_owned()));
    }

    #[test]
    fn the_agent_runs_this_luneta() {
        let path = path_with(Path::new("/nix/store/x-luneta/bin/luneta")).expect("a PATH");
        let first = env::split_paths(&path).next();
        assert_eq!(first.as_deref(), Some(Path::new("/nix/store/x-luneta/bin")));
        assert_eq!(path_with(Path::new("/w/target/debug/deps/luneta-0a1b")), None);
        let bin = tempfile::tempdir().unwrap();
        assert_eq!(path_with(&bin.path().join("lunetad")), None);
        std::fs::write(bin.path().join("luneta"), "").unwrap();
        let path = path_with(&bin.path().join("lunetad")).expect("a PATH");
        assert_eq!(env::split_paths(&path).next().as_deref(), Some(bin.path()));
    }

    #[test]
    fn what_claude_writes_says_what_the_agent_ran_and_said() {
        let line = |record: Value| record.to_string();
        let assistant = |content: Value| json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": content}});
        assert_eq!(
            read(&line(json!({"type": "system", "subtype": "init", "session_id": "7c22"}))),
            [Read::Step(Step::Session(session("7c22")))]
        );
        let unnamed = json!({"type": "system", "subtype": "init", "session_id": "../x"});
        assert_eq!(read(&line(unnamed)), []);
        assert_eq!(
            read(&line(assistant(json!([
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "  Looking.  "},
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "luneta agents", "description": "List"}},
                {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/w/x"}},
                {"type": "text", "text": " "},
            ])))),
            [
                Read::Step(Step::Said("Looking.".to_owned())),
                Read::Step(Step::Ran { id: "toolu_1".to_owned(), command: "luneta agents".to_owned() }),
                Read::Step(Step::Ran { id: "toolu_2".to_owned(), command: "Read".to_owned() }),
            ]
        );
        let mut subagent = assistant(json!([{"type": "text", "text": "a subagent's"}]));
        subagent["parent_tool_use_id"] = json!("toolu_1");
        for record in [
            subagent,
            json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "hi"}]}}),
            json!({"type": "rate_limit_event"}),
            json!({"type": "system", "subtype": "status"}),
        ] {
            assert_eq!(read(&line(record.clone())), [], "{record}");
        }
        assert_eq!(read("not json"), []);
        let result = |subtype: &str, is_error: bool, result: &str| {
            line(json!({"type": "result", "subtype": subtype, "is_error": is_error, "result": result}))
        };
        assert_eq!(read(&result("success", false, "Done.\n")), [Read::Ended(Ok("Done.".to_owned()))]);
        assert_eq!(read(&result("success", true, "Not logged in")), [Read::Ended(Err("Not logged in".to_owned()))]);
        assert_eq!(read(&result("error_max_turns", true, "")), [Read::Ended(Err("error_max_turns".to_owned()))]);
        let denied = json!({
            "type": "result", "subtype": "success", "is_error": false, "result": "Done.",
            "permission_denials": [
                {"tool_name": "Bash", "tool_use_id": "toolu_3", "tool_input": {"command": "touch x"}},
            ],
        });
        assert_eq!(
            read(&line(denied)),
            [Read::Step(Step::Denied("toolu_3".to_owned())), Read::Ended(Ok("Done.".to_owned()))]
        );
    }

    #[test]
    fn a_turn_hands_on_what_claude_writes_and_ends_as_it_says() {
        let heard = |script: &str, words: &str| {
            let mut sh = Command::new("sh");
            sh.args(["-c", script]);
            let heard = RefCell::new(Vec::new());
            let ended = talk(sh, words, &|| true, &|said| heard.borrow_mut().push(said));
            (heard.into_inner(), ended)
        };
        let init = r#"{"type":"system","subtype":"init","session_id":"7c22"}"#;
        let ran = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"luneta agents"}}]}}"#;
        let done = r#"{"type":"result","subtype":"success","is_error":false,"result":"You are in luneta:2."}"#;
        let script = format!("read words; echo '{init}'; echo '{ran}'; echo \"$words\" >&2; echo '{done}'");
        let (said, ended) = heard(&script, "where am I\n");
        assert_eq!(said, [
            Step::Session(session("7c22")),
            Step::Ran { id: "toolu_1".to_owned(), command: "luneta agents".to_owned() },
            Step::Said("You are in luneta:2.".to_owned()),
        ]);
        assert_eq!(ended, Some(Ok(())));

        let failed = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in"}"#;
        assert_eq!(heard(&format!("echo '{failed}'"), "").1, Some(Err("Not logged in".to_owned())));
        let ended = heard("echo 'No conversation found' >&2; exit 1", "").1;
        assert_eq!(ended, Some(Err("claude failed: No conversation found".to_owned())));
        let ended = heard("exit 0", "").1;
        assert_eq!(ended, Some(Err("claude ended without saying how the turn went".to_owned())));

        let mut gone = Command::new("sh");
        gone.current_dir("/w/gone");
        let ended = talk(gone, "", &|| true, &|_| {});
        assert_eq!(ended, Some(Err("/w/gone, where the agent works, is gone".to_owned())));
        let ended = talk(Command::new("/w/gone/claude"), "", &|| true, &|_| {});
        assert_eq!(ended, Some(Err("claude is not on the PATH".to_owned())));
    }

    #[test]
    fn a_turn_given_up_kills_claude() {
        let ended = given_up("claude, which ends only if killed, ends", |wanted| {
            talk(outlasting_patience(), "", wanted, &|_| {})
        });
        assert_eq!(ended, None);
    }
}
