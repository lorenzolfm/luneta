//! A turn of a talk of luneta's ask box: what you want done, in words, for
//! an agent to do with luneta's commands, as the menu would. What you type
//! goes to `claude -p`, with luneta's skill, and with luneta's commands and
//! the few its skill reads with as the only ones it may run, and those your
//! own settings let it. luneta runs it for a row of its own machine; a
//! daemon runs it here for a luneta on another machine, whose box was for a
//! row of this one, so that the agent's commands, and the zoxide, gh and
//! worktrees it reads with, act here, with this machine's skill and login.
//!
//! The agent runs in no pane of zellij, and goes by where you are as you
//! send it the turn: the pane luneta's zellij is on, where that is of zellij
//! on the agent's machine, which luneta's commands take for the one they run
//! in, by `ASKED_VAR`; or the pane of an own session luneta's client is on,
//! by `OWN_VAR`; or another machine, by `AWAY_VAR`, where a close takes none
//! of your panes along, and `go` moves none of your clients.

use std::env;
use std::ffi::OsString;
use std::hash::{BuildHasher as _, Hasher as _, RandomState};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::api::ask_request::{self, UserOwnPane, UserPane};
use crate::api::ask_response::{self, Ran};
use crate::api::{AskRequest, AskResponse, On};
use crate::transcripts::SessionId;
use crate::mux::{self, OwnId, OwnKey, OwnPane, SessionName};
use crate::{git, program, Harness};

/// The model the agent runs on: one that works out what words point at, and
/// runs a few commands in a row, without slips.
pub const MODEL: &str = "sonnet";

/// How long a turn has. One that opens a tab with a worktree for each of a
/// few issues takes a minute or two: one that takes longer than this is
/// stuck, and is killed.
pub const TURN: Duration = Duration::from_mins(10);

/// How often a turn looks at whether it is still wanted, while `claude` says
/// nothing.
pub const LOOK: Duration = Duration::from_millis(5);

/// What names the pane you asked luneta's ask box from, in the variables of
/// the agent it runs, which runs in no pane: the pane luneta's zellij was on
/// as the turn started, as `<session>:<pane>`, by the name its session had
/// then, not the one it had as the pane opened.
pub const ASKED_VAR: &str = "LUNETA_PANE";

/// What names the pane of an own session you asked luneta's ask box from, in
/// the variables of the agent it runs, as `<session>:<made>:<pane>:<name>`:
/// the session by its id and when its server made it, in nanoseconds since
/// the epoch, which a rename keeps, the pane by its id, and last the name
/// the session had as the turn started, which may hold a `:` of its own.
pub const OWN_VAR: &str = "LUNETA_OWN_PANE";

/// What says, in the variables of the agent of luneta's ask box, that you
/// asked it from another machine, where luneta's zellij was as the turn
/// started: no pane of the agent's machine is yours.
pub const AWAY_VAR: &str = "LUNETA_AWAY";

/// What names the turn of luneta's ask box a command runs in, in the
/// variables of the box's agent, and what names the turn before it: a key of
/// each, in hex. The agent knows neither, as Claude Code runs no line of its
/// that reads a variable or sets one, in its `dontAsk` mode.
pub const TURN_VAR: &str = "LUNETA_TURN";
pub const BEFORE_VAR: &str = "LUNETA_TURN_BEFORE";

/// What says, in the variables of the agent of luneta's ask box, that a pane
/// it opens may run pi, as you picked pi last in luneta's menu: pi asks
/// nothing before it runs a command, so the box opens none where you did not.
pub const AGENTS_VAR: &str = "LUNETA_AGENTS";

/// What `AGENTS_VAR` is, where it is set.
const AGENTS_PI: &str = "pi";

/// The commands the agent may run, as Claude Code's rules name them: the
/// verbs of luneta's its skill has, and the commands its skill reads what
/// your words point at with, and makes worktrees with. In `dontAsk` mode,
/// Claude Code denies it any other but those your own settings allow, and a
/// line that runs one inside another, as through `$(…)`.
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

/// luneta's skill, which says how an agent goes about what you ask: the one
/// this build's commands are of, whatever skills Claude Code has installed.
pub const SKILL: &str = include_str!("../../../skills/luneta/SKILL.md");

/// What the agent is told, whatever you ask, before what it is told of the
/// row highlighted, and the skill.
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

/// What `AWAY_VAR` is, where it is set.
const AWAY_SET: &str = "1";

/// A turn of the agent of luneta's ask box, by its key, and the key of the
/// turn before it, where one went before. A plan a close says in a turn is of
/// its key, and the one it takes a yes to, of the key before, so that a close
/// takes a yes only to a plan said in the turn before, what you answered the
/// agent between them. No turn is of the key of the turn before it, as it
/// would take a yes to the plan it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Turn {
    key: u64,
    before: Option<u64>,
}

impl Turn {
    /// The turn after the one of the key `before`, where one went before, by
    /// a key of its own: one nothing the agent runs can tell, as std seeds
    /// the keys of its hashers at random.
    #[must_use]
    pub fn after(before: Option<u64>) -> Turn {
        let key = RandomState::new().build_hasher().finish();
        let key = if before == Some(key) { !key } else { key };
        Turn { key, before }
    }

    /// The turn of `key`, after the one of `before`, where one went before,
    /// or why it is none: `before` is `key`.
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

    /// Names it in the variables of `command`, for a close it runs to read.
    pub fn name_in(self, command: &mut Command) {
        command.env(TURN_VAR, format!("{:016x}", self.key));
        match self.before {
            Some(before) => command.env(BEFORE_VAR, format!("{before:016x}")),
            None => command.env_remove(BEFORE_VAR),
        };
    }

    /// The turn `key` and `before` name, as `name_in` names one in `TURN_VAR`
    /// and `BEFORE_VAR`: none where neither does. Or why they do not read.
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

/// Where you are as you send a turn, as the agent is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum User {
    /// In the pane `pane` of `session`, of zellij on the agent's machine,
    /// where luneta's zellij is, by the name its session has now.
    Pane { session: SessionName, pane: u32 },
    /// In the own pane `pane`, on the agent's machine, where luneta's client
    /// of an own session is, of the session `named` as the turn started,
    /// which the agent is told.
    Own { pane: OwnPane, named: SessionName },
    /// On another machine, where luneta's zellij is: in none of the agent's
    /// machine's panes.
    Away,
    /// Where luneta cannot tell just now.
    Untold,
}

impl User {
    /// Names it in the variables of `command`, for luneta's commands to go
    /// by: the pane, as `<session>:<pane>`, in `ASKED_VAR`, or an own one in
    /// `OWN_VAR`, or another machine in `AWAY_VAR`, or none of them.
    fn name_in(&self, command: &mut Command) {
        command.env_remove(ASKED_VAR).env_remove(OWN_VAR).env_remove(AWAY_VAR);
        match self {
            User::Pane { session, pane } => command.env(ASKED_VAR, format!("{session}:{pane}")),
            User::Own { pane, named } => command.env(OWN_VAR, own_spelled(*pane, named)),
            User::Away => command.env(AWAY_VAR, AWAY_SET),
            User::Untold => command,
        };
    }

    /// Where you are, as a question to a daemon says it.
    fn asked(&self) -> Option<ask_request::User> {
        match self {
            User::Untold => None,
            User::Away => Some(ask_request::User::Away(())),
            User::Pane { session, pane } => {
                let session = session.to_string();
                Some(ask_request::User::Pane(UserPane { session, pane: *pane }))
            },
            User::Own { pane, named } => Some(ask_request::User::Own(UserOwnPane {
                session: Some(pane.session.into()),
                pane: pane.id.get(),
                named: named.to_string(),
            })),
        }
    }

    /// Where you are, as `name_in` names it in `said`. Or why they do not
    /// read, or say more than one place.
    fn read(said: Where<'_>) -> Result<User, String> {
        match said {
            Where { asked: None, own: None, away: None } => Ok(User::Untold),
            Where { asked: None, own: None, away: Some(AWAY_SET) } => Ok(User::Away),
            Where { asked: None, own: None, away: Some(away) } => {
                Err(format!("{AWAY_VAR} is {away:?}, and luneta's ask box sets it to {AWAY_SET}"))
            },
            // A session's name may hold a `:` of its own, as zellij's may.
            Where { asked: Some(asked), own: None, away: None } => {
                let pane = asked.rsplit_once(':').and_then(|(session, pane)| {
                    let session = SessionName::running(session).ok()?;
                    Some(User::Pane { session, pane: pane.parse().ok()? })
                });
                let no_pane = || format!("{ASKED_VAR}: {asked} is no pane of a session zellij runs");
                pane.ok_or_else(no_pane)
            },
            Where { asked: None, own: Some(own), away: None } => {
                let no_pane = || format!("{OWN_VAR}: {own} is no pane of an own session");
                own_read(own).ok_or_else(no_pane)
            },
            Where { asked, own, away } => {
                let asked = asked.map(|asked| {
                    format!("{ASKED_VAR} names {asked}, a pane luneta's ask box asked from")
                });
                let own = own.map(|own| {
                    format!("{OWN_VAR} names {own}, an own pane luneta's ask box asked from")
                });
                let away = away.map(|_| format!("{AWAY_VAR} says it asked from another machine"));
                let said: Vec<String> = [asked, own, away].into_iter().flatten().collect();
                assert!(said.len() >= 2, "the arms above take each place alone, and none");
                Err(said.join(", and "))
            },
        }
    }
}

/// `OWN_VAR` as `name_in` spells it, of the own pane `pane`, of the session
/// named `named`: as `Asked::new` takes it, and `own_read` reads it back.
fn own_spelled(pane: OwnPane, named: &SessionName) -> String {
    let made = made_nanos(pane.session.made).expect("Asked::new takes no own session made then");
    format!("{}:{made}:{}:{named}", pane.session.id, pane.id)
}

/// When an own session was made, `made`, in nanoseconds since the epoch, as
/// luneta's own multiplexer names it in the variables of its panes and
/// `own_read` reads it: none before the epoch, nor past what a `u64` holds.
fn made_nanos(made: SystemTime) -> Option<u64> {
    u64::try_from(made.duration_since(UNIX_EPOCH).ok()?.as_nanos()).ok()
}

/// The pane of an own session `spelled` names, as `own_spelled` spells it:
/// none where it does not read, as where an id is 0, or the name is one no
/// session takes.
fn own_read(spelled: &str) -> Option<User> {
    let mut parts = spelled.splitn(4, ':');
    let id = OwnId::new(parts.next()?.parse().ok()?)?;
    let made = UNIX_EPOCH.checked_add(Duration::from_nanos(parts.next()?.parse().ok()?))?;
    let pane = OwnPane { session: OwnKey { id, made }, id: parts.next()?.parse().ok()? };
    let named = SessionName::running(parts.next()?).ok()?;
    Some(User::Own { pane, named })
}

/// Where you are, as the variables of the agent of luneta's ask box spell it:
/// of `ASKED_VAR`, `OWN_VAR` and `AWAY_VAR`, which the box sets one of at
/// most.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Where<'a> {
    pub asked: Option<&'a str>,
    pub own: Option<&'a str>,
    pub away: Option<&'a str>,
}

/// The turn of luneta's ask box a command runs in, and where you are, as the
/// box names them in the variables of its agent: `key` and `before`, of
/// `TURN_VAR` and `BEFORE_VAR`, and where you are, as `user` spells it. None
/// where none is set, as outside the box. Or why they do not read, or say
/// where you are outside a turn.
pub fn turn_in(
    key: Option<&str>,
    before: Option<&str>,
    user: Where<'_>,
) -> Result<Option<(Turn, User)>, String> {
    let Some(turn) = Turn::read(key, before)? else {
        let set = match user {
            Where { asked: None, own: None, away: None } => return Ok(None),
            Where { asked: Some(_), .. } => ASKED_VAR,
            Where { own: Some(_), .. } => OWN_VAR,
            Where { away: Some(_), .. } => AWAY_VAR,
        };
        return Err(format!("{set} is set, and {TURN_VAR} is not"));
    };
    Ok(Some((turn, User::read(user)?)))
}

/// The harness the turn's agents start in, as the box names it in `said`, of
/// `AGENTS_VAR`: Claude Code where it is not set. Or why it does not read.
pub fn agents_in(said: Option<&str>) -> Result<Harness, String> {
    match said {
        None => Ok(Harness::ClaudeCode),
        Some(AGENTS_PI) => Ok(Harness::Pi),
        Some(said) => {
            Err(format!("{AGENTS_VAR} is {said:?}, and luneta's ask box sets it to {AGENTS_PI}"))
        },
    }
}

/// A step of a turn of the agent, as `claude` says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// The Claude session the turn runs on, which the next one goes on with.
    Session(SessionId),
    /// A command the agent runs, or a tool it uses, by its name, and the id
    /// Claude Code gives the call.
    Ran { id: String, command: String },
    /// Claude Code denied the agent the call of this id, which ran nothing.
    Denied(String),
    /// What the agent said.
    Said(String),
}

impl Step {
    /// It, as a daemon hands it on.
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

    /// The step a daemon handed on: none where it is none of these, as a
    /// newer daemon's could be. Or why it is no step: it names a session by
    /// no id of one, which the next turn could not go on with.
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

/// A turn asked of the agent: `words`, what you typed, for an agent that
/// works in `cwd`, on the Claude session `resumed` if one went before, told
/// of the row `highlighted` as the box opened, for you where `user` says, in
/// `turn`, starting the agents it starts in `agents`, and in pi only where
/// that is pi.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// By its absolute path: none for the home of the user it runs as.
    cwd: Option<PathBuf>,
    /// With something besides white space, and none around it.
    words: String,
    resumed: Option<SessionId>,
    highlighted: String,
    user: User,
    turn: Turn,
    agents: Harness,
}

impl Asked {
    /// The turn of `words`, as `Asked` has it, or why not: `cwd` is by no
    /// absolute path, or the words are only white space, or `user` is in an
    /// own pane of id 0, or of a session made at a time `OWN_VAR` cannot say.
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
        if let User::Own { pane, .. } = &user
            && made_nanos(pane.session.made).is_none()
        {
            let id = pane.session.id;
            return Err(format!("user: own session {id} was made at no time {OWN_VAR} says"));
        }
        Ok(Asked { cwd, words: words.to_owned(), resumed, highlighted, user, turn, agents })
    }

    /// The turn a question asks for, and the machine it is for, or why the
    /// question is invalid: as `new` says, or it resumes a session by no id
    /// of one, or names your pane in a session by a name zellij does not
    /// take, or its turn is of the key of the turn before it, or its agents
    /// are to start in a harness this lunetad does not know.
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
            Some(ask_request::User::Own(UserOwnPane { session, pane, named })) => {
                let pane = OwnPane::of(&crate::api::Own { session, pane })
                    .map_err(|why| format!("user: {why}"))?;
                let named = SessionName::running(&named).map_err(|why| format!("user: {why}"))?;
                User::Own { pane, named }
            },
        };
        let turn = Turn::new(turn, before)?;
        let agents = Harness::said(agents).map_err(|why| format!("agents: {why}"))?;
        let cwd = cwd.map(PathBuf::from);
        let asked = Asked::new(cwd, &words, resumed, highlighted, user, turn, agents)?;
        Ok((on, asked))
    }

    /// The question that has the daemon of the machine `on` names run it, or
    /// the daemon asked, where it names none.
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

    /// Runs it, and hands each step the agent takes to `hear` as `claude`
    /// says it, for as long as it is `wanted`: none once it is not, as
    /// `claude` is killed then. Or how it ended: well, or why not.
    pub fn talk(
        &self,
        wanted: &dyn Fn() -> bool,
        hear: &dyn Fn(Step),
    ) -> Option<Result<(), String>> {
        talk(self.claude(), &told(&self.user, self.agents, &self.words), wanted, hear)
    }

    /// `claude`, for it. It runs on the model the box has, in safe mode, so
    /// that none of your hooks, `CLAUDE.md` or MCP servers come into it, with
    /// Bash its one tool, and `ALLOWED` all it runs but what your own
    /// settings allow: not what those of the directory it runs in do, which
    /// a repository brings. It has no zellij in its environment, so that the
    /// daemon places it in no pane, nor what tells git where a repository
    /// is, so that the git it runs, and `luneta worktree`, find one by the
    /// directory they run in. The `luneta` it runs is the one beside this
    /// program, where there is one, and its closes know the turn, and where
    /// you are.
    fn claude(&self) -> Command {
        let mut claude = Command::new("claude");
        claude.args(["-p", "--model", MODEL, "--safe-mode", "--setting-sources", "user"]);
        claude.args(["--permission-mode", "dontAsk", "--tools", "Bash"]);
        claude.args(["--output-format", "stream-json", "--verbose"]);
        claude.arg("--append-system-prompt").arg(system(&self.highlighted));
        if let Some(session) = &self.resumed {
            claude.args(["--resume", session.as_str()]);
        }
        // It takes every word after it for one more rule: the words you typed
        // go on its stdin.
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

/// What the agent is told, with the row `highlighted` as the box opened, and
/// the skill: the same each turn, as Claude Code keeps what a talk's first
/// turn was told for the rest of it.
fn system(highlighted: &str) -> String {
    format!("{SYSTEM}\n\n{highlighted}\n\n{SKILL}")
}

/// The message of a turn: `words`, what you typed, after a line that says
/// where you are as you send it, as `user` has it, and one that says the
/// harness an agent it starts runs in, `agents`: pi unless you name Claude
/// Code, or Claude Code alone, as the box opens pi only where you picked it.
fn told(user: &User, agents: Harness, words: &str) -> String {
    let agents = match agents {
        Harness::Pi => "[luneta: start an agent in pi, unless the user names Claude Code.]",
        Harness::ClaudeCode => "[luneta: start an agent in Claude Code. luneta opens pi only once \
                                the user picks it with Tab in luneta's menu.]",
    };
    let user = match user {
        User::Pane { session, pane } => format!("[luneta: the user is in pane {session}:{pane}.]"),
        User::Own { pane, named } => format!(
            "[luneta: the user is in pane {named}:{}, of an own session, whose tabs hold a pane \
             each: `open pane` and `turn` fail there.]",
            pane.id
        ),
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

/// The `PATH` with the directory of `exe` first, where it is a `luneta`, or
/// where one is beside it, as beside `lunetad`, so that the `luneta` the
/// agent runs is that one, as old or as new as the skill the agent is told.
fn path_with(exe: &Path) -> Option<OsString> {
    if exe.file_name()? != "luneta" && !exe.with_file_name("luneta").is_file() {
        return None;
    }
    let path = env::var_os("PATH").unwrap_or_default();
    let dirs = std::iter::once(exe.parent()?.to_owned()).chain(env::split_paths(&path));
    env::join_paths(dirs).ok()
}

/// What a line `claude` writes says, of what the box shows.
#[derive(Debug, PartialEq, Eq)]
enum Read {
    Step(Step),
    /// The turn's end, as `claude` says it: what the agent said last, or why
    /// the turn failed.
    Ended(Result<String, String>),
}

/// What `line`, a JSON record of what `claude` does, says: the session it
/// starts on, what the agent ran and said, and the turn's end, with the
/// calls Claude Code denied it, as it says only then. A subagent's records
/// are left out, and so are what the agent thought, what a tool answered,
/// and the rest.
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

/// A step of what the agent did, from a block of what it sent: what it said,
/// or the command it ran, or another tool it used, by its name.
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

/// Runs `claude`, as `command` has it, on `words`, which go on its stdin, and
/// hands each step it hears to `hear` as `claude` says it, for as long as the
/// turn is `wanted`: none once it is not, as `claude` is killed then. Or how
/// the turn ended: well, or why not, as `claude` says, or as it failed, or as
/// it was still at it after `TURN`. An agent that said nothing all turn says
/// what `claude` says it said last, if anything.
fn talk(
    mut command: Command,
    words: &str,
    wanted: &dyn Fn() -> bool,
    hear: &dyn Fn(Step),
) -> Option<Result<(), String>> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        // A program not on the PATH is not found, and so is a directory to
        // start in that is gone, as a pane's is once its worktree is removed.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let gone = command.get_current_dir().filter(|cwd| !cwd.is_dir());
            return Some(Err(match gone {
                Some(cwd) => format!("{}, where the agent works, is gone", cwd.display()),
                None => "claude is not on the PATH".to_owned(),
            }));
        },
        Err(error) => return Some(Err(format!("claude could not be run: {error}"))),
    };
    // `claude -p` reads its stdin to the end before it starts. One that ends
    // without reading it says why on its stderr.
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
            // Its stdout ended, or does not read.
            Ok(Err(_)) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {},
        }
        let stuck = Instant::now() >= deadline;
        if stuck || !wanted() {
            // One that ended just now is not waited for yet, and takes the
            // signal all the same.
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

/// Why `claude` failed, as it ended with `status` and said `stderr`: the
/// first line of what it said, or how it ended, or that it said nothing of
/// the turn's end.
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

    /// The pane `pane` of an own session named `named`, of id 3, made
    /// `made` nanoseconds after the epoch.
    fn own_pane_made(named: &str, pane: u32, made: u64) -> User {
        let made = UNIX_EPOCH + Duration::from_nanos(made);
        let pane = OwnPane::new(OwnKey { id: OwnId::new(3).unwrap(), made }, pane).unwrap();
        User::Own { pane, named: SessionName::running(named).unwrap() }
    }

    fn own_pane(named: &str, pane: u32) -> User {
        own_pane_made(named, pane, 1_700_000_000_123_456_789)
    }

    fn session(id: &str) -> SessionId {
        SessionId::parse(id).unwrap()
    }

    fn args(command: &Command) -> Vec<String> {
        command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect()
    }

    /// The agent runs in the turn's directory, on the box's model, in safe
    /// mode, with the permission rules of your settings alone, no tool but
    /// Bash and nothing it may run but `ALLOWED`, and the words go on its
    /// stdin, as `--allowedTools` takes every word after it. It is told the
    /// row highlighted as the box opened, and where you are each turn, in
    /// what the turn sends it, as Claude Code keeps the system prompt of a
    /// talk's first turn. It has no zellij in its environment, nor what tells
    /// git where a repository is, and has the pane you are in, or that you
    /// are on another machine, where luneta can tell either, and the turn it
    /// is at. A turn after the first goes on with the session the first
    /// said, and one with no directory works in the home of the user it
    /// runs as.
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
        let own = ["LUNETA_MUX_SESSION", "LUNETA_MUX_MADE", "LUNETA_MUX_PANE"];
        let panes = zellij.into_iter().chain(own);
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

    /// luneta's commands read in the variables of the agent the turn it is
    /// at and where you are, as the box named them, and read none outside
    /// the box.
    #[test]
    fn the_agent_s_commands_read_the_turn_and_where_you_are() {
        let var = |command: &Command, name: &str| {
            let set = command.get_envs().find(|(key, _)| key.to_str() == Some(name));
            set.and_then(|(_, value)| value).map(|value| value.to_string_lossy().into_owned())
        };
        let first = Turn::new(10, None).unwrap();
        for user in [pane("a:b", 4), own_pane("a:b", 4), User::Away, User::Untold] {
            for turn in [first, turn()] {
                let you = user.clone();
                let asked = Asked::new(None, "yes", None, String::new(), you, turn, CLAUDE);
                let claude = asked.unwrap().claude();
                let names = [TURN_VAR, BEFORE_VAR, ASKED_VAR, OWN_VAR, AWAY_VAR];
                let [key, before, asked, own, away] = names.map(|name| var(&claude, name));
                let (asked, own, away) = (asked.as_deref(), own.as_deref(), away.as_deref());
                let read = turn_in(key.as_deref(), before.as_deref(), Where { asked, own, away });
                assert_eq!(read, Ok(Some((turn, user.clone()))));
            }
        }
        assert_eq!(turn_in(None, None, Where::default()), Ok(None));
    }

    /// luneta's commands read in the variables of the agent the harness its
    /// agents start in, as the box named it: Claude Code where it names
    /// none, and nothing the box would not name.
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

    /// A command reads no turn of the box's but one its variables name in
    /// full: a key that reads, after none or another that does, and where
    /// you are by a pane of a session zellij could run, or by a pane of an
    /// own session, by ids that are not 0 and a name a session could have, or
    /// by the `1` that says another machine, or by none. Where you are,
    /// outside a turn, is none of the box's, nor are two places.
    #[test]
    fn the_agent_s_commands_read_no_turn_the_box_could_not_name() {
        let (key, before) = (Some("ff"), Some("0a"));
        let at = |asked, away| Where { asked, own: None, away };
        let own = |own| Where { own: Some(own), ..Where::default() };
        assert_eq!(turn_in(key, before, Where::default()), Ok(Some((turn(), User::Untold))));
        let read = turn_in(key, before, own("3:60000000005:12:a:b"));
        assert_eq!(read, Ok(Some((turn(), own_pane_made("a:b", 12, 60_000_000_005)))));
        for (key, before) in [
            (Some(""), None),
            (Some("forged"), None),
            (Some("ff"), Some("")),
            (None, Some("ff")),
            (Some("ff"), Some("ff")),
        ] {
            assert!(turn_in(key, before, Where::default()).is_err(), "{key:?} {before:?}");
        }
        for said in [at(Some("work:2"), None), at(None, Some("1")), own("3:60:12:work")] {
            assert!(turn_in(None, None, said).is_err(), "{said:?}");
        }
        for forged in ["", "3:60:12", "0:60:12:work", "3:60:0:work", "3:x:12:work", "3:60:12:a/b"] {
            assert!(turn_in(key, before, own(forged)).is_err(), "{forged:?}");
        }
        let twice = Where { asked: Some("work:2"), own: Some("3:60:12:work"), away: None };
        assert!(turn_in(key, before, twice).is_err());
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

    /// Each turn has a key of its own, and the key of the turn before it, the
    /// first none.
    #[test]
    fn a_turn_goes_by_its_own_key_and_the_one_before() {
        let first = Turn::after(None);
        let second = Turn::after(Some(first.key()));
        let third = Turn::after(Some(second.key()));
        let befores = [first.before(), second.before(), third.before()];
        assert_eq!(befores, [None, Some(first.key()), Some(second.key())]);
        assert!(first.key() != second.key() && second.key() != third.key());
    }

    /// The own pane of a question, as `change` leaves it.
    fn own_asked(change: impl FnOnce(&mut UserOwnPane)) -> ask_request::User {
        let Some(ask_request::User::Own(mut own)) = own_pane("work", 2).asked() else {
            unreachable!("an own pane is asked as one");
        };
        change(&mut own);
        ask_request::User::Own(own)
    }

    /// A turn is asked of a daemon as it is, and a question that asks for
    /// none is invalid: one in a directory by no absolute path, of words
    /// that are only white space, that resumes a session by no id of one,
    /// that names your pane in a session by a name zellij does not take, or
    /// an own pane of no session, of id 0, or of a session made before the
    /// epoch, or whose turn is of the key of the turn before it.
    #[test]
    fn a_turn_is_asked_of_a_daemon_as_it_is() {
        let on = || Some(On { destination: "nixos".to_owned() });
        for user in [pane("luneta", 2), own_pane("luneta", 2), User::Away, User::Untold] {
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
        let made_at = Some((UNIX_EPOCH - Duration::from_secs(1)).into());
        let made_before = Some(crate::api::OwnSession { id: 3, made_at });
        let invalid = [
            AskRequest { cwd: Some(String::new()), ..request() },
            AskRequest { cwd: Some("w".to_owned()), ..request() },
            AskRequest { words: " \n".to_owned(), ..request() },
            AskRequest { resumed: Some("--debug".to_owned()), ..request() },
            AskRequest {
                user: Some(ask_request::User::Pane(UserPane { session: String::new(), pane: 2 })),
                ..request()
            },
            AskRequest { user: Some(own_asked(|own| own.session = None)), ..request() },
            AskRequest { user: Some(own_asked(|own| own.pane = 0)), ..request() },
            AskRequest { user: Some(own_asked(|own| own.session = made_before)), ..request() },
            AskRequest { user: Some(own_asked(|own| own.named = "a/b".to_owned())), ..request() },
            AskRequest { turn: 7, before: Some(7), ..request() },
            AskRequest { agents: Some(99), ..request() },
        ];
        for request in invalid {
            assert!(Asked::of(request.clone()).is_err(), "{request:?}");
        }
    }

    /// A step a daemon hands on is the step it took, and one that is none a
    /// daemon hands on is left out. One that names a session by no id of
    /// one is no step.
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

    /// The agent runs this luneta, where it is one, or the one beside this
    /// program, as beside lunetad, ahead of any other on the `PATH`.
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

    /// What the agent ran and said comes out of what `claude` writes, a
    /// record to a line, and so does the session it runs on and how its turn
    /// ended, with what Claude Code denied it. A subagent's records, what it
    /// thought and the rest are left out.
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

    /// A turn hands on what `claude` writes as it writes it, and ends as it
    /// says: well, or with why not. One that says how it ended without having
    /// said a word says that.
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

    /// A turn given up kills `claude`, and ends with nothing to say.
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
