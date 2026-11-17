use std::env;
use std::ffi::OsString;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::api::ask_request::{self, UserPane};
use crate::api::ask_response::{self, Ran};
use crate::api::{AskRequest, AskResponse, On};
use crate::transcripts::SessionId;
use crate::mux::{self, SessionName};
use crate::{git, program, Harness};

pub const MODEL: &str = "sonnet";

pub const TURN: Duration = Duration::from_mins(10);

pub const LOOK: Duration = Duration::from_millis(5);

pub const ASKED_VAR: &str = "LUNETA_PANE";

pub const AWAY_VAR: &str = "LUNETA_AWAY";

pub const TURN_VAR: &str = "LUNETA_TURN";
pub const BEFORE_VAR: &str = "LUNETA_TURN_BEFORE";

pub const AGENTS_VAR: &str = "LUNETA_AGENTS";

const AGENTS_PI: &str = "pi";

pub const ALLOWED: [&str; 16] = [
    "Bash(luneta here:*)",
    "Bash(luneta sessions:*)",
    "Bash(luneta agents:*)",
    "Bash(luneta open:*)",
    "Bash(luneta go:*)",
    "Bash(luneta turn:*)",
    "Bash(luneta close:*)",
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
    sidebar, and reads what you say in a small box over zellij: write plain text for a \
    terminal, with no markdown, no headings, no tables and no code fences, and keep it short. \
    Do what they ask with luneta's commands, as the luneta skill below says, and end with a \
    line or two on what you did, and where. Run one command in each call to Bash, with \
    nothing around it: no $(…), no pipes, no && and no ;. Only these run: luneta here, \
    sessions, agents, open, go, turn, close and prune, zoxide query, gh issue view and list, gh pr view \
    and list, luneta worktree, and git worktree add and list. Claude Code denies any other. A \
    pane you open runs the user's shell, Claude Code as claude [--name <name>] [<prompt>], or, \
    where the turn's line says pi, pi as pi [--name <name>] [<prompt>], with no other option \
    of theirs: luneta refuses any other command. You cannot wait for \
    the user in the middle of a turn: to ask them anything, as the skill has you ask before \
    a close, or which of two things they meant, end your turn with the question. Their \
    answer comes as the next message. So a close or a prune takes --yes only in the turn after \
    the one that said its plan.\n\nEach message of the user's starts with a line of luneta's in \
    brackets, which says the pane they are in as they send it, or that they are on another \
    machine, or that luneta cannot tell: that goes for the turn, whatever one before said. \
    `luneta here` names that pane, and a close's plan marks the agent there (this runs here). \
    You run in no pane yourself, so closing it does not end you, but it is the user's: leave \
    it and its session out of \"all\", as the skill says.";

const AWAY_SET: &str = "1";

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

    fn read(key: Option<&str>, before: Option<&str>) -> Result<Option<Turn>, String> {
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
pub enum User {
    Pane { session: SessionName, pane: u32 },
    Away,
    Untold,
}

impl User {
    fn name_in(&self, command: &mut Command) {
        for name in [ASKED_VAR, AWAY_VAR] {
            command.env_remove(name);
        }
        match self {
            User::Pane { session, pane } => command.env(ASKED_VAR, format!("{session}:{pane}")),
            User::Away => command.env(AWAY_VAR, AWAY_SET),
            User::Untold => command,
        };
    }

    fn asked(&self) -> Option<ask_request::User> {
        match self {
            User::Untold => None,
            User::Away => Some(ask_request::User::Away(())),
            User::Pane { session, pane } => {
                let session = session.to_string();
                Some(ask_request::User::Pane(UserPane { session, pane: *pane }))
            },
        }
    }

    fn read(said: Where<'_>) -> Result<User, String> {
        match said {
            Where { asked: None, away: None } => Ok(User::Untold),
            Where { asked: None, away: Some(AWAY_SET) } => Ok(User::Away),
            Where { asked: None, away: Some(away) } => {
                Err(format!("{AWAY_VAR} is {away:?}, and luneta's ask box sets it to {AWAY_SET}"))
            },
            Where { asked: Some(asked), away: None } => {
                let pane = asked.rsplit_once(':').and_then(|(session, pane)| {
                    let session = SessionName::running(session).ok()?;
                    Some(User::Pane { session, pane: pane.parse().ok()? })
                });
                let no_pane = || format!("{ASKED_VAR}: {asked} is no pane of a session zellij runs");
                pane.ok_or_else(no_pane)
            },
            Where { asked: Some(asked), away: Some(_) } => Err(format!(
                "{ASKED_VAR} names {asked}, a pane luneta's ask box asked from, and {AWAY_VAR} \
                 says it asked from another machine"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Where<'a> {
    pub asked: Option<&'a str>,
    pub away: Option<&'a str>,
}

pub fn turn_in(
    key: Option<&str>,
    before: Option<&str>,
    user: Where<'_>,
) -> Result<Option<(Turn, User)>, String> {
    let Some(turn) = Turn::read(key, before)? else {
        let set = match user {
            Where { asked: None, away: None } => return Ok(None),
            Where { asked: Some(_), .. } => ASKED_VAR,
            Where { away: Some(_), .. } => AWAY_VAR,
        };
        return Err(format!("{set} is set, and {TURN_VAR} is not"));
    };
    Ok(Some((turn, User::read(user)?)))
}

pub fn agents_in(said: Option<&str>) -> Result<Harness, String> {
    match said {
        None => Ok(Harness::ClaudeCode),
        Some(AGENTS_PI) => Ok(Harness::Pi),
        Some(said) => {
            Err(format!("{AGENTS_VAR} is {said:?}, and luneta's ask box sets it to {AGENTS_PI}"))
        },
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
    user: User,
    turn: Turn,
    agents: Harness,
}

impl Asked {
    pub fn new(
        cwd: Option<PathBuf>,
        words: &str,
        resumed: Option<SessionId>,
        highlighted: String,
        user: User,
        turn: Turn,
        agents: Harness,
    ) -> Result<Asked, String> {
        if let Some(cwd) = cwd.as_ref().filter(|cwd| !cwd.is_absolute()) {
            return Err(format!("cwd: {} is no absolute path", cwd.display()));
        }
        let words = words.trim();
        if words.is_empty() {
            return Err("words: nothing asked".to_owned());
        }
        Ok(Asked { cwd, words: words.to_owned(), resumed, highlighted, user, turn, agents })
    }

    pub fn of(request: AskRequest) -> Result<(Option<On>, Asked), String> {
        let AskRequest { on, cwd, words, resumed, highlighted, user, turn, before, agents } =
            request;
        let resumed = match resumed {
            None => None,
            Some(resumed) => Some(
                SessionId::parse(&resumed)
                    .ok_or_else(|| format!("resumed: {resumed:?} names no session"))?,
            ),
        };
        let user = match user {
            None => User::Untold,
            Some(ask_request::User::Away(())) => User::Away,
            Some(ask_request::User::Pane(UserPane { session, pane })) => {
                let session = SessionName::running(&session).map_err(|why| format!("user: {why}"))?;
                User::Pane { session, pane }
            },
        };
        let turn = Turn::new(turn, before)?;
        let agents = Harness::said(agents).map_err(|why| format!("agents: {why}"))?;
        let cwd = cwd.map(PathBuf::from);
        let asked = Asked::new(cwd, &words, resumed, highlighted, user, turn, agents)?;
        Ok((on, asked))
    }

    #[must_use]
    pub fn asked(&self, on: Option<On>) -> AskRequest {
        let user = self.user.asked();
        AskRequest {
            on,
            cwd: self.cwd.as_ref().map(|cwd| cwd.to_string_lossy().into_owned()),
            words: self.words.clone(),
            resumed: self.resumed.as_ref().map(|resumed| resumed.as_str().to_owned()),
            highlighted: self.highlighted.clone(),
            user,
            turn: self.turn.key,
            before: self.turn.before,
            agents: Some(self.agents.to_api().into()),
        }
    }

    pub fn talk(
        &self,
        wanted: &dyn Fn() -> bool,
        hear: &dyn Fn(Step),
    ) -> Option<Result<(), String>> {
        talk(self.claude(), &told(&self.user, self.agents, &self.words), wanted, hear)
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
        mux::in_no_pane(&mut claude);
        git::local(&mut claude);
        self.user.name_in(&mut claude);
        self.turn.name_in(&mut claude);
        match self.agents {
            Harness::Pi => claude.env(AGENTS_VAR, AGENTS_PI),
            Harness::ClaudeCode => claude.env_remove(AGENTS_VAR),
        };
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

fn told(user: &User, agents: Harness, words: &str) -> String {
    let agents = match agents {
        Harness::Pi => "[luneta: start an agent in pi, unless the user names Claude Code.]",
        Harness::ClaudeCode => "[luneta: start an agent in Claude Code. luneta opens pi only once \
                                the user picks it with Tab in luneta's menu.]",
    };
    let user = match user {
        User::Pane { session, pane } => format!("[luneta: the user is in pane {session}:{pane}.]"),
        User::Away => "[luneta: the user is on another machine just now, where luneta's zellij \
                       is, and in none of this machine's panes: `luneta here`, `go` and `open \
                       session` fail, and so do `open pane` without --beside and `open tab` \
                       without --session. A close takes none of their panes along. Tell the \
                       user so where you need them.]"
            .to_owned(),
        User::Untold => "[luneta: it cannot tell which pane the user is in just now, so `luneta \
                         here`, `go`, `open session` and a close fail, and so do `open pane` \
                         without --beside and `open tab` without --session. Tell the user so \
                         where you need them.]"
            .to_owned(),
    };
    format!("{user}\n{agents}\n\n{words}")
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

    const CLAUDE: Harness = Harness::ClaudeCode;

    fn turn() -> Turn {
        Turn { key: 255, before: Some(10) }
    }

    fn pane(session: &str, pane: u32) -> User {
        User::Pane { session: SessionName::running(session).unwrap(), pane }
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn the_agent_runs_where_it_is_asked_for_where_you_are() {
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.map(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
        };
        let highlighted = "The row highlighted is pane luneta:4.".to_owned();
        let cwd = Some(PathBuf::from("/w/luneta"));
        let you = pane("luneta", 2);
        let first = Asked::new(cwd, " close it ", None, highlighted, you, turn(), CLAUDE);
        let first = first.unwrap();
        let claude = first.claude();
        assert_eq!(claude.get_current_dir(), Some(Path::new("/w/luneta")));
        let said = args(&claude);
        let model = ["-p", "--model", "sonnet", "--safe-mode", "--setting-sources", "user"];
        let permissions = ["--permission-mode", "dontAsk", "--tools", "Bash"];
        let output = ["--output-format", "stream-json", "--verbose"];
        assert_eq!(said[..13], [&model[..], &permissions, &output].concat());
        assert_eq!(said[13], "--append-system-prompt");
        let system = format!("{SYSTEM}\n\nThe row highlighted is pane luneta:4.\n\n{SKILL}");
        assert_eq!(said[14], system);
        assert_eq!(said[15..], [&["--allowedTools"][..], &ALLOWED[..]].concat());
        let told_first = told(&first.user, first.agents, &first.words);
        let line = "[luneta: start an agent in Claude Code. luneta opens pi only once the user \
                    picks it with Tab in luneta's menu.]";
        let you = "[luneta: the user is in pane luneta:2.]";
        assert_eq!(told_first, format!("{you}\n{line}\n\nclose it"));
        let pi = told(&first.user, Harness::Pi, "go");
        let line = "[luneta: start an agent in pi, unless the user names Claude Code.]";
        assert_eq!(pi, format!("{you}\n{line}\n\ngo"));
        let zellij = ["ZELLIJ", "ZELLIJ_SESSION_NAME", "ZELLIJ_PANE_ID"];
        let terminal = ["LUNETA_MUX_TERMINAL", "LUNETA_MUX_TERMINAL_MADE"];
        let panes = zellij.into_iter().chain(terminal);
        for name in panes.chain(git::REPOSITORY_VARS).chain([AWAY_VAR]) {
            assert_eq!(var(&claude, name), Some(None), "{name}");
        }
        assert_eq!(var(&claude, ASKED_VAR), Some(Some("luneta:2".to_owned())));
        assert_eq!(var(&claude, TURN_VAR), Some(Some("00000000000000ff".to_owned())));
        assert_eq!(var(&claude, BEFORE_VAR), Some(Some("000000000000000a".to_owned())));

        let resumed = Some(session("7c22"));
        let next = Asked::new(None, "yes", resumed, String::new(), User::Away, turn(), CLAUDE);
        let claude = next.unwrap().claude();
        assert_eq!(claude.get_current_dir(), env::var_os("HOME").as_deref().map(Path::new));
        assert_eq!(args(&claude)[15..17], ["--resume", "7c22"]);
        assert_eq!(var(&claude, ASKED_VAR), Some(None));
        assert_eq!(var(&claude, AWAY_VAR), Some(Some("1".to_owned())));
        let away = told(&User::Away, Harness::ClaudeCode, "yes");
        assert!(away.starts_with("[luneta: the user is on another machine just now"), "{away}");
        assert!(away.ends_with("]\n\nyes"), "{away}");

        let first = Turn { key: 10, before: None };
        let untold = Asked::new(None, "yes", None, String::new(), User::Untold, first, CLAUDE);
        let claude = untold.unwrap().claude();
        assert_eq!([var(&claude, ASKED_VAR), var(&claude, AWAY_VAR)], [Some(None), Some(None)]);
        assert_eq!(var(&claude, TURN_VAR), Some(Some("000000000000000a".to_owned())));
        assert_eq!(var(&claude, BEFORE_VAR), Some(None));
        let untold = told(&User::Untold, Harness::ClaudeCode, "yes");
        let says = "[luneta: it cannot tell which pane the user is in";
        assert!(untold.starts_with(says), "{untold}");
        assert!(untold.ends_with("]\n\nyes"), "{untold}");
    }

    #[test]
    fn the_agent_s_commands_read_the_turn_and_where_you_are() {
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.and_then(|(_, value)| value).map(|value| value.to_string_lossy().into_owned())
        };
        let first = Turn::new(10, None).unwrap();
        for user in [pane("a:b", 4), User::Away, User::Untold] {
            for turn in [first, turn()] {
                let you = user.clone();
                let asked = Asked::new(None, "yes", None, String::new(), you, turn, CLAUDE);
                let claude = asked.unwrap().claude();
                let names = [TURN_VAR, BEFORE_VAR, ASKED_VAR, AWAY_VAR];
                let [key, before, asked, away] = names.map(|name| var(&claude, name));
                let said = Where { asked: asked.as_deref(), away: away.as_deref() };
                let read = turn_in(key.as_deref(), before.as_deref(), said);
                assert_eq!(read, Ok(Some((turn, user.clone()))));
            }
        }
        assert_eq!(turn_in(None, None, Where::default()), Ok(None));
    }

    #[test]
    fn the_agent_s_commands_read_the_harness_of_the_turn() {
        for agents in [Harness::ClaudeCode, Harness::Pi] {
            let you = User::Untold;
            let asked = Asked::new(None, "yes", None, String::new(), you, turn(), agents);
            let claude = asked.unwrap().claude();
            let set = claude.get_envs().find(|(key, _)| key.to_str() == Some(AGENTS_VAR));
            let said = set.and_then(|(_, value)| value).and_then(|value| value.to_str());
            assert_eq!(agents_in(said), Ok(agents));
        }
        for forged in ["", "claude", "Pi", "pi "] {
            assert!(agents_in(Some(forged)).is_err(), "{forged:?}");
        }
    }

    #[test]
    fn the_agent_s_commands_read_no_turn_the_box_could_not_name() {
        let (key, before) = (Some("ff"), Some("0a"));
        let at = |asked, away| Where { asked, away };
        assert_eq!(turn_in(key, before, Where::default()), Ok(Some((turn(), User::Untold))));
        for (key, before) in [
            (Some(""), None),
            (Some("forged"), None),
            (Some("ff"), Some("")),
            (None, Some("ff")),
            (Some("ff"), Some("ff")),
        ] {
            assert!(turn_in(key, before, Where::default()).is_err(), "{key:?} {before:?}");
        }
        for said in [at(Some("work:2"), None), at(None, Some("1"))] {
            assert!(turn_in(None, None, said).is_err(), "{said:?}");
        }
        for (asked, away) in [
            (Some(""), None),
            (Some("work"), None),
            (Some(":4"), None),
            (Some("work:x"), None),
            (Some("a/b:4"), None),
            (None, Some("")),
            (None, Some("0")),
            (Some("work:2"), Some("1")),
        ] {
            assert!(turn_in(key, before, at(asked, away)).is_err(), "{asked:?} {away:?}");
        }
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
        for user in [pane("luneta", 2), User::Away, User::Untold] {
            for (cwd, resumed) in [(Some("/w".into()), None), (None, Some(session("7c22")))] {
                let row = "row".to_owned();
                let asked = Asked::new(cwd, "close it", resumed, row, user.clone(), turn(), CLAUDE);
                let asked = asked.unwrap();
                assert_eq!(Asked::of(asked.asked(on())), Ok((on(), asked)));
            }
        }
        let request = || {
            let row = "row".to_owned();
            let asked = Asked::new(None, "close it", None, row, User::Untold, turn(), CLAUDE);
            asked.unwrap().asked(None)
        };
        let invalid = [
            AskRequest { cwd: Some(String::new()), ..request() },
            AskRequest { cwd: Some("w".to_owned()), ..request() },
            AskRequest { words: " \n".to_owned(), ..request() },
            AskRequest { resumed: Some("--debug".to_owned()), ..request() },
            AskRequest {
                user: Some(ask_request::User::Pane(UserPane { session: String::new(), pane: 2 })),
                ..request()
            },
            AskRequest { turn: 7, before: Some(7), ..request() },
            AskRequest { agents: Some(99), ..request() },
        ];
        for request in invalid {
            assert!(Asked::of(request.clone()).is_err(), "{request:?}");
        }
    }

    #[test]
    fn a_step_is_handed_on_as_it_is() {
        let ran = Step::Ran { id: "toolu_1".to_owned(), command: "luneta here".to_owned() };
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
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "luneta sessions", "description": "List"}},
                {"type": "tool_use", "id": "toolu_2", "name": "Read", "input": {"file_path": "/w/x"}},
                {"type": "text", "text": " "},
            ])))),
            [
                Read::Step(Step::Said("Looking.".to_owned())),
                Read::Step(Step::Ran { id: "toolu_1".to_owned(), command: "luneta sessions".to_owned() }),
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
        let ran = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"luneta here"}}]}}"#;
        let done = r#"{"type":"result","subtype":"success","is_error":false,"result":"You are in luneta:2."}"#;
        let script = format!("read words; echo '{init}'; echo '{ran}'; echo \"$words\" >&2; echo '{done}'");
        let (said, ended) = heard(&script, "where am I\n");
        assert_eq!(said, [
            Step::Session(session("7c22")),
            Step::Ran { id: "toolu_1".to_owned(), command: "luneta here".to_owned() },
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
        let mut sh = Command::new("sh");
        sh.args(["-c", "sleep 10"]);
        let started = Instant::now();
        let ended = talk(sh, "", &|| started.elapsed() < Duration::from_millis(50), &|_| {});
        assert_eq!(ended, None);
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    }
}
