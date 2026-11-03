//! A review of a pull request: the same skills, in the same order, on one
//! agent, each given the pull request's link. A skill's turn ends with what it
//! found, for you to read, and nothing goes until you have acted on it: the
//! next skill goes as a turn you started ends.
//!
//! An agent carries its review in its environment, in `VAR`, as `command`
//! starts it, on the first skill, for its prompt. So however it was opened, on
//! whichever machine, the daemon of that machine finds its review with the
//! agent itself. The daemon keeps where it is in memory, so a daemon that
//! started after the agent did takes up no review of it: it could not tell
//! which skill the agent is on.
//!
//! Claude Code says what an agent is doing, in its session file, and not who
//! started what it does: a background task that ends wakes it as you do. So
//! once a turn after a skill's has stopped, and stayed stopped for `QUIET`,
//! which leaves a message you queued time to start, and Claude Code time to
//! write down that you interrupted it, the transcript says who began it, as
//! `began` reads it. It takes a turn you began and did not interrupt, and
//! anything else leaves the next skill to wait, for you: it would rather wait
//! for a key of the menu than run a skill you did not ask for yet.
//!
//! The next skill goes as you would type it, in the agent's pane, and only
//! while it is idle: not at work, not running a background shell, not waiting
//! on a question of its own. Where you have started typing in its prompt, it
//! would join what you typed, so it waits for that first, by the screen, as
//! `prompt_on` reads it. A key you press in the moment between that look and
//! the write does join it.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use luneta_protocol::said::Said;
use serde::Deserialize;

use crate::agent::{Agent, Status};
use crate::api;
use crate::cli;
use crate::core::Command;
use crate::links::{Kind, Link};
use crate::log;
use crate::screens;
use crate::transcripts::{self, SessionId};
use crate::zellij::{SessionName, Sessions};

/// The skills of a review, in the order they run. luneta has no configuration
/// to read yours from.
pub const SKILLS: [&str; 3] =
    ["/thermo-nuclear-review", "/make-illegal-states-unrepresentable", "/tigerstyle"];

/// The variable of an agent's environment that names the pull request its
/// review is of.
pub const VAR: &str = "LUNETA_REVIEW";

/// What Claude Code runs on, as luneta starts it.
const CLAUDE: &str = "claude";

/// What starts the agents a person talks to at a prompt, as Claude Code says.
/// A `claude -p` that an agent runs has the agent's environment, and so its
/// `VAR`, and no review of its own.
const AT_A_PROMPT: &str = "cli";

/// How long a stop lasts before a skill goes after it: a message you queued
/// starts in that time, you can hold the review, and Claude Code has written
/// down what it did.
const QUIET: Duration = Duration::from_secs(5);

/// The ends of a transcript to look through, one after the other, for the
/// prompt that began its last turn. A turn writes megabytes after its prompt,
/// and most turns take the first. Past the last, the review waits for you.
const WINDOWS: [u64; 3] = [1 << 20, 1 << 24, 1 << 27];

/// The `kind` of the origin Claude Code writes on a prompt a person typed.
const HUMAN: &str = "human";

/// Claude Code's mark at the head of its prompt box, and what it puts after it.
const PROMPT: &str = "\u{276f}\u{a0}";

/// The command line of an agent on the first skill of a review of `link`,
/// named for the branch it works on, as a skill's agent is.
#[must_use]
pub fn command(link: &Link, branch: Option<&str>) -> Vec<String> {
    let name = branch.map_or_else(|| "review".to_owned(), |branch| format!("review: {branch}"));
    vec![
        "env".to_owned(),
        format!("{VAR}={}", link.url()),
        CLAUDE.to_owned(),
        "--name".to_owned(),
        name,
        prompt_of(SKILLS[0], link),
    ]
}

/// What an agent is told for `skill`, of a review of `link`.
fn prompt_of(skill: &str, link: &Link) -> String {
    format!("{skill} {}", link.url())
}

/// The review `value` of `VAR` names: of a pull request's link alone.
#[must_use]
pub fn of_variable(value: &str) -> Option<Link> {
    Link::parse(value).filter(|link| link.kind() == Kind::Pull)
}

/// Something for the thread of `follow` to do, which reads files and asks
/// zellij, as the core does not. Its `ticket` goes back with what came of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    /// Read whether you began the last turn of the session `session`.
    Read { pid: u32, ticket: u64, session: Said },
    /// Write `prompt` in the prompt of the pane `pane` of `session`, and send
    /// it, unless you have started typing there.
    Send { pid: u32, ticket: u64, session: SessionName, pane: u32, prompt: String },
}

/// What came of an `Act`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    /// `Act::Read`: whether you began it.
    Read(bool),
    /// `Act::Send`.
    Sent(Sent),
}

/// What came of an `Act::Send`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    /// It went.
    Went,
    /// Nothing went: you have started typing in the prompt.
    Draft,
    /// Nothing went: why not.
    Failed(String),
}

/// A move of a review, as a key of luneta's menu has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Move {
    Hold,
    Resume,
    Next,
    Skip,
}

impl Move {
    /// The move `move_` says, as the API gives it, if it says one.
    #[must_use]
    pub fn of(move_: api::ReviewMove) -> Option<Move> {
        match move_ {
            api::ReviewMove::Hold => Some(Move::Hold),
            api::ReviewMove::Resume => Some(Move::Resume),
            api::ReviewMove::Next => Some(Move::Next),
            api::ReviewMove::Skip => Some(Move::Skip),
            api::ReviewMove::Unspecified => None,
        }
    }

    /// The move as the API gives it.
    #[must_use]
    pub const fn to_api(self) -> api::ReviewMove {
        match self {
            Move::Hold => api::ReviewMove::Hold,
            Move::Resume => api::ReviewMove::Resume,
            Move::Next => api::ReviewMove::Next,
            Move::Skip => api::ReviewMove::Skip,
        }
    }
}

/// Why a review did not move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The agent runs none: it ended, or the last skill is done.
    NoReview,
    /// It is on the last skill.
    NoNext,
    /// The next skill is on its way into the pane.
    Going,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refused::NoReview => "the agent runs no review now",
            Refused::NoNext => "the agent is on the last skill of its review",
            Refused::Going => "the next skill is on its way to the agent",
        })
    }
}

/// Why the next skill waits on what is in the agent's pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Blocked {
    Draft,
    Prompt,
}

/// Where a review is at, between one skill and the next. `owed` says that the
/// next skill goes as this stops, with nothing more of you, as you asked for
/// it with a key, or a turn of yours ended before this one began.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// A skill was asked for, and has not begun. It waits for that however
    /// long it takes, as a turn taken for yours that was the skill's would send
    /// the next one too soon: an agent can be slow to start, and a prompt that
    /// begins no turn, as of a skill no one has, leaves it here for a key.
    Asked,
    /// The skill's turn runs.
    Skill { owed: bool },
    /// The skill's turn stopped, for you to read. Nothing goes until a turn
    /// of yours ends, or a key says.
    Stopped,
    /// A turn runs, since the skill's stopped.
    Turn { owed: bool },
    /// That turn stopped. It is read at `at`, if it stays stopped.
    Quiet { at: Instant },
    /// The thread reads who began that turn.
    Reading { ticket: u64 },
    /// The next skill goes at `at`, once the agent is idle, and not until
    /// what `blocked` it last has gone.
    Due { at: Instant, blocked: Option<Blocked> },
    /// The thread writes the next skill, on `blocked` as it was.
    Sending { ticket: u64, blocked: Option<Blocked> },
    /// The last skill stopped. Its agent still carries its `VAR`, which is
    /// why this stands, until it goes.
    Over,
}

/// The review an agent runs.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Review {
    link: Link,
    /// Which of `SKILLS` it is on.
    on: usize,
    /// Which of `SKILLS` goes next: after `on`, and past the last for none.
    next: usize,
    /// You hold it: it sends nothing.
    held: bool,
    stage: Stage,
}

/// What a review needs done to go on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Need {
    Read,
    Send,
}

impl Review {
    /// A review of `link` whose agent came just now, on the first skill.
    fn new(link: Link) -> Self {
        Review { link, on: 0, next: 1, held: false, stage: Stage::Asked }
    }

    /// Whether there is a skill to go next.
    fn has_next(&self) -> bool {
        self.next < SKILLS.len()
    }

    /// The skill's turn stopped: the review ends on the last, and otherwise
    /// the next waits for you, unless it is owed.
    fn skill_stopped(&mut self, owed: bool, now: Instant) {
        self.stage = if !self.has_next() {
            Stage::Over
        } else if owed {
            Stage::Due { at: now + QUIET, blocked: None }
        } else {
            Stage::Stopped
        };
    }

    /// Takes in what its agent is doing now, and says what it needs done.
    fn saw(&mut self, status: &Status, now: Instant) -> Option<Need> {
        let busy = matches!(status, Status::Busy);
        // A background shell lets the turn stop, and keeps the agent from
        // being idle: nothing goes until it ends, and may wake the agent.
        let stopped = matches!(status, Status::Idle | Status::Shell);
        let idle = matches!(status, Status::Idle);
        match self.stage {
            Stage::Asked if busy || matches!(status, Status::Waiting(_)) => {
                self.stage = Stage::Skill { owed: false };
            },
            Stage::Skill { owed } if stopped => self.skill_stopped(owed, now),
            Stage::Stopped if busy => self.stage = Stage::Turn { owed: false },
            Stage::Turn { owed: false } if stopped => {
                self.stage = Stage::Quiet { at: now + QUIET };
            },
            Stage::Turn { owed: true } if stopped => {
                self.stage = Stage::Due { at: now + QUIET, blocked: None };
            },
            Stage::Quiet { .. } | Stage::Reading { .. } if busy => {
                self.stage = Stage::Turn { owed: false };
            },
            Stage::Quiet { at } if stopped && now >= at => return Some(Need::Read),
            Stage::Due { .. } if busy => self.stage = Stage::Turn { owed: true },
            Stage::Due { at, .. } if idle && !self.held && now >= at => return Some(Need::Send),
            Stage::Asked
            | Stage::Skill { .. }
            | Stage::Stopped
            | Stage::Turn { .. }
            | Stage::Quiet { .. }
            | Stage::Reading { .. }
            | Stage::Due { .. }
            | Stage::Sending { .. }
            | Stage::Over => {},
        }
        None
    }

    /// Takes in what came of what it asked for with `ticket`, if it still
    /// waits for that. Why a write failed goes in the log the first time.
    fn answered(&mut self, ticket: u64, answer: Answer, now: Instant) {
        match (self.stage, answer) {
            (Stage::Reading { ticket: asked }, Answer::Read(yours)) if asked == ticket => {
                self.stage = if yours {
                    Stage::Due { at: now, blocked: None }
                } else {
                    Stage::Stopped
                };
            },
            (Stage::Sending { ticket: asked, blocked }, Answer::Sent(sent)) if asked == ticket => {
                match sent {
                    Sent::Went => {
                        assert!(self.has_next(), "a skill went with none to go");
                        self.on = self.next;
                        self.next += 1;
                        self.stage = Stage::Asked;
                    },
                    Sent::Draft => {
                        self.stage = Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) };
                    },
                    Sent::Failed(why) => {
                        if blocked != Some(Blocked::Prompt) {
                            log::say(&format!("the review of {} waits: {why}", self.link));
                        }
                        self.stage = Stage::Due { at: now + QUIET, blocked: Some(Blocked::Prompt) };
                    },
                }
            },
            // An answer it no longer waits for: a turn began since.
            _ => {},
        }
    }

    /// Does what `does` says.
    fn moved(&mut self, does: Move, now: Instant) -> Result<(), Refused> {
        match does {
            Move::Hold => self.held = true,
            Move::Resume => self.held = false,
            Move::Next => {
                if !self.has_next() {
                    return Err(Refused::NoNext);
                }
                self.held = false;
                match &mut self.stage {
                    Stage::Skill { owed } | Stage::Turn { owed } => *owed = true,
                    Stage::Asked | Stage::Stopped | Stage::Quiet { .. } | Stage::Reading { .. } => {
                        self.stage = Stage::Due { at: now, blocked: None };
                    },
                    Stage::Due { at, .. } => *at = now,
                    Stage::Sending { .. } | Stage::Over => {},
                }
            },
            Move::Skip => {
                if !self.has_next() {
                    return Err(Refused::NoNext);
                }
                if matches!(self.stage, Stage::Sending { .. }) {
                    return Err(Refused::Going);
                }
                self.next += 1;
                let on_a_skill = matches!(self.stage, Stage::Asked | Stage::Skill { .. });
                if !self.has_next() && !on_a_skill {
                    self.stage = Stage::Over;
                }
            },
        }
        Ok(())
    }

    /// What the next skill waits for, as the API says it: nothing for a review
    /// that is over, which the API leaves out.
    fn waits(&self) -> Option<api::ReviewWaits> {
        Some(match self.stage {
            Stage::Asked | Stage::Skill { owed: false } => api::ReviewWaits::Skill,
            Stage::Stopped => api::ReviewWaits::You,
            Stage::Skill { .. }
            | Stage::Turn { .. }
            | Stage::Quiet { .. }
            | Stage::Reading { .. }
            | Stage::Due { blocked: None, .. }
            | Stage::Sending { blocked: None, .. } => api::ReviewWaits::Turn,
            Stage::Due { blocked: Some(Blocked::Draft), .. }
            | Stage::Sending { blocked: Some(Blocked::Draft), .. } => api::ReviewWaits::Draft,
            Stage::Due { blocked: Some(Blocked::Prompt), .. }
            | Stage::Sending { blocked: Some(Blocked::Prompt), .. } => api::ReviewWaits::Prompt,
            Stage::Over => return None,
        })
    }

    /// The review as the API gives it, where it is not over.
    fn to_api(&self) -> Option<api::Review> {
        assert!(self.on < SKILLS.len() && self.on < self.next);
        let (step, steps) = (self.on + 1, SKILLS.len());
        Some(api::Review {
            link: self.link.url(),
            step: u32::try_from(step).expect("a review has a few skills"),
            steps: u32::try_from(steps).expect("a review has a few skills"),
            skill: SKILLS[self.on].to_owned(),
            next: SKILLS.get(self.next).map(|skill| (*skill).to_owned()),
            waits: self.waits()?.into(),
            held: self.held,
        })
    }
}

/// The reviews agents run, by the agents' pids: what the core keeps of them.
#[derive(Debug)]
pub struct Reviews {
    /// When the daemon started: of an agent that started before, it does not
    /// know where the review is.
    since: SystemTime,
    by_agent: BTreeMap<u32, Review>,
    /// How many acts it has asked for.
    tickets: u64,
}

impl Reviews {
    #[must_use]
    pub fn new(since: SystemTime) -> Self {
        Reviews { since, by_agent: BTreeMap::new(), tickets: 0 }
    }

    /// Takes in the agents that run now, in `sessions`, and hands `act` what
    /// they need done, which says whether it took it: one it does not is asked
    /// for again on the next look. A review goes with its agent.
    pub fn saw(
        &mut self,
        agents: &[Agent],
        sessions: &Sessions,
        now: Instant,
        act: &mut dyn FnMut(Act) -> bool,
    ) {
        self.by_agent.retain(|pid, _| agents.iter().any(|agent| agent.pid == *pid));
        for agent in agents {
            if !self.by_agent.contains_key(&agent.pid)
                && let Some(link) = self.takes_up(agent)
            {
                self.by_agent.insert(agent.pid, Review::new(link));
            }
            let Some(review) = self.by_agent.get_mut(&agent.pid) else {
                continue;
            };
            let Some(need) = review.saw(&agent.status, now) else {
                continue;
            };
            let ticket = self.tickets + 1;
            match need {
                Need::Read => {
                    let session = agent.session_id.clone();
                    if act(Act::Read { pid: agent.pid, ticket, session }) {
                        self.tickets = ticket;
                        review.stage = Stage::Reading { ticket };
                    }
                },
                Need::Send => {
                    let Stage::Due { blocked, .. } = review.stage else {
                        unreachable!("only a review that is due needs a skill sent");
                    };
                    let Some((session, pane)) = seat(agent, sessions) else {
                        // Its session is gone, or lunetad cannot tell which it
                        // is now: there is nowhere to write.
                        let blocked = Some(Blocked::Prompt);
                        review.stage = Stage::Due { at: now + QUIET, blocked };
                        continue;
                    };
                    let prompt = prompt_of(SKILLS[review.next], &review.link);
                    if act(Act::Send { pid: agent.pid, ticket, session, pane, prompt }) {
                        self.tickets = ticket;
                        review.stage = Stage::Sending { ticket, blocked };
                    }
                },
            }
        }
    }

    /// The pull request of the review `agent` starts, where it starts one
    /// that the daemon can follow: at a prompt, in a pane, and since the
    /// daemon started.
    fn takes_up(&self, agent: &Agent) -> Option<Link> {
        let at_a_prompt = agent.entrypoint.as_ref().is_some_and(|it| it.as_str() == AT_A_PROMPT);
        let since = agent.started_at.is_some_and(|started| started >= self.since);
        (at_a_prompt && since && agent.pane.is_some()).then(|| agent.review.clone()).flatten()
    }

    /// Takes in what came of the act with `ticket`, of the agent `pid`.
    pub fn answered(&mut self, pid: u32, ticket: u64, answer: Answer, now: Instant) {
        if let Some(review) = self.by_agent.get_mut(&pid) {
            review.answered(ticket, answer, now);
        }
    }

    /// Moves the review of the agent `pid`, or says why not.
    pub fn moved(&mut self, pid: u32, does: Move, now: Instant) -> Result<(), Refused> {
        let review = self.by_agent.get_mut(&pid);
        let review = review.filter(|review| review.stage != Stage::Over);
        review.ok_or(Refused::NoReview)?.moved(does, now)
    }

    /// The review of the agent `pid`, as the API gives it.
    #[must_use]
    pub fn to_api(&self, pid: u32) -> Option<api::Review> {
        self.by_agent.get(&pid)?.to_api()
    }
}

/// The session `agent` runs in now, and its pane, as zellij can be asked of
/// them.
fn seat(agent: &Agent, sessions: &Sessions) -> Option<(SessionName, u32)> {
    let named = agent.pane.as_ref()?;
    let session = sessions.lead(&named.session, agent.started_at)?;
    Some((SessionName::running(session).ok()?, named.pane))
}

/// Does what the core hands it, on a thread of its own, and hands back what
/// came of each. One that the core would be waiting for is not: it asks with
/// `try_send`, and the thread can wait for it.
pub fn follow(
    core: SyncSender<Command>,
    acts: Receiver<Act>,
    claude_folder: Option<PathBuf>,
) -> io::Result<()> {
    let projects = claude_folder.map(|folder| folder.join("projects"));
    thread::Builder::new().name("reviews".to_owned()).spawn(move || {
        for act in acts {
            let (pid, ticket, answer) = match act {
                Act::Read { pid, ticket, session } => {
                    let read = projects.as_deref().is_some_and(|it| yours(it, session.as_str()));
                    (pid, ticket, Answer::Read(read))
                },
                Act::Send { pid, ticket, session, pane, prompt } => {
                    (pid, ticket, Answer::Sent(send(&session, pane, &prompt)))
                },
            };
            if core.send(Command::Reviewed { pid, ticket, answer }).is_err() {
                return;
            }
        }
    })?;
    Ok(())
}

/// Whether you began the last turn of the session `id`, as its transcript in
/// `projects` says. One that does not read, or says neither, leaves it at no:
/// the next skill then waits for you.
fn yours(projects: &Path, id: &str) -> bool {
    let path = SessionId::parse(id).and_then(|id| transcripts::path_of(projects, &id));
    path.is_some_and(|path| last_turn_yours(&path).unwrap_or(false))
}

/// Whether you began the last turn of the transcript at `path`, by the prompt
/// that began it, as `began` reads it, in the first of `WINDOWS` that has it.
fn last_turn_yours(path: &Path) -> io::Result<bool> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    for window in WINDOWS {
        let window = window.min(length);
        file.seek(SeekFrom::Start(length - window))?;
        let mut read = Vec::new();
        file.by_ref().take(window).read_to_end(&mut read)?;
        // The window starts partway into a record, which reads as no record,
        // and partway into a character, which reads as `�`.
        let text = String::from_utf8_lossy(&read);
        if let Some(yours) = text.lines().rev().find_map(began) {
            return Ok(yours);
        }
        if window == length {
            break;
        }
    }
    Ok(false)
}

/// A record of a transcript, as much of it as `began` reads.
#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "isMeta")]
    meta: bool,
    #[serde(default, rename = "isSidechain")]
    sidechain: bool,
    origin: Option<Origin>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Origin {
    kind: String,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Content,
}

#[derive(Default, Deserialize)]
#[serde(untagged)]
enum Content {
    #[default]
    Nothing,
    Text(String),
    Blocks(Vec<Block>),
}

#[derive(Deserialize)]
struct Block {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

impl Content {
    /// What it says, where it is more than what tools answered: a turn goes
    /// on through those.
    fn said(&self) -> Option<&str> {
        match self {
            Content::Nothing => None,
            Content::Text(text) => Some(text),
            Content::Blocks(blocks) => {
                let told = |block: &&Block| block.kind != "tool_result";
                let block = blocks.iter().find(told)?;
                Some(block.text.as_deref().unwrap_or_default())
            },
        }
    }
}

/// Whether the last turn is one you began and let finish, by the record
/// `line`, read back from the end of a transcript, where that record tells:
/// Claude Code says who wrote a prompt, in its origin. What says something and
/// has no origin is no prompt of yours: it writes down so that you interrupted
/// a turn, as `[Request interrupted by user]`, and a command of its own, as
/// `/compact`. What it adds around a turn, and what a tool answers within one,
/// tell nothing, nor what a subagent does.
fn began(line: &str) -> Option<bool> {
    // Most records are long, and none of those it reads.
    if !line.contains(r#""type":"user""#) {
        return None;
    }
    let record: Record = serde_json::from_str(line).ok()?;
    if record.kind != "user" || record.sidechain {
        return None;
    }
    if let Some(origin) = record.origin {
        return Some(origin.kind == HUMAN);
    }
    if record.meta {
        return None;
    }
    record.message?.content.said().map(|_| false)
}

/// What the prompt box of a screen holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prompt {
    /// Nothing, or the suggestion Claude Code shows there, dimmed.
    Empty,
    /// What you have typed.
    Draft,
    /// The screen shows none.
    Unseen,
}

/// What the prompt box of `screen` holds, as zellij dumps a screen with its
/// colours: the last line that starts with Claude Code's mark, past which a
/// suggestion is dim, and what you type is not. A pane you have scrolled back
/// in may show an older prompt of yours, which reads as a draft, and so waits.
fn prompt_on(screen: &str) -> Prompt {
    let boxed = screen.lines().rev().find_map(|line| after_mark(line));
    let Some((dim, rest)) = boxed else {
        return Prompt::Unseen;
    };
    if typed(dim, rest) { Prompt::Draft } else { Prompt::Empty }
}

/// What follows `PROMPT` on `line`, with whether the colours leave it dim,
/// where it comes first but for colours.
fn after_mark(line: &str) -> Option<(bool, &str)> {
    let mut dim = false;
    let mut rest = line;
    loop {
        if let Some(after) = rest.strip_prefix(PROMPT) {
            return Some((dim, after));
        }
        let (after, sgr) = colour(rest)?;
        dim = dimmed(dim, sgr);
        rest = after;
    }
}

/// Whether `text`, dim as it starts or not, has anything that is not white
/// space and not dim.
fn typed(mut dim: bool, text: &str) -> bool {
    let mut rest = text;
    while let Some(next) = rest.chars().next() {
        if let Some((after, sgr)) = colour(rest) {
            dim = dimmed(dim, sgr);
            rest = after;
            continue;
        }
        if !dim && !next.is_whitespace() {
            return true;
        }
        rest = &rest[next.len_utf8()..];
    }
    false
}

/// What follows the colour `text` starts with, and its parameters, where it
/// starts with one.
fn colour(text: &str) -> Option<(&str, &str)> {
    let (sgr, after) = text.strip_prefix("\u{1b}[")?.split_once('m')?;
    sgr.bytes().all(|byte| byte.is_ascii_digit() || byte == b';').then_some((after, sgr))
}

/// Whether text is dim after the parameters `sgr`, as it was or not before.
fn dimmed(was: bool, sgr: &str) -> bool {
    let mut dim = was;
    let mut parts = sgr.split(';');
    while let Some(part) = parts.next() {
        match part {
            "" | "0" | "22" => dim = false,
            "2" => dim = true,
            // A colour's own numbers, which can be a 2.
            "38" | "48" | "58" => match parts.next() {
                Some("2") => {
                    let _ = parts.by_ref().take(3).count();
                },
                Some("5") => {
                    let _ = parts.next();
                },
                _ => {},
            },
            _ => {},
        }
    }
    dim
}

/// Writes `prompt` in the pane `pane` of `session`, and sends it, as you would
/// type it, unless it is no empty prompt box that it would write in.
fn send(session: &SessionName, pane: u32, prompt: &str) -> Sent {
    let screen = match screens::dumped(session, Some(pane)) {
        Ok((_, screen)) => screen,
        Err(why) => return Sent::Failed(why),
    };
    match prompt_on(&screen) {
        Prompt::Empty => {},
        Prompt::Draft => return Sent::Draft,
        Prompt::Unseen => {
            return Sent::Failed(format!("pane {pane} of {session} shows no prompt to write in"));
        },
    }
    let terminal = format!("terminal_{pane}");
    let action = |action: &str, what: &str| {
        let mut command = Process::new("zellij");
        command.args(["--session", session.as_str(), "action", action, "--pane-id", &terminal, what]);
        cli::done(command)
    };
    match action("paste", prompt).and_then(|()| action("send-keys", "Enter")) {
        Ok(()) => Sent::Went,
        Err(why) => Sent::Failed(why),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use luneta_protocol::said::Said;

    use super::*;
    use crate::agent::{Named, WaitingFor};

    const LINK: &str = "https://github.com/o/app/pull/7";

    fn link() -> Link {
        Link::parse(LINK).unwrap()
    }

    /// A review, on the skill `on`, at `stage`.
    fn review(on: usize, stage: Stage) -> Review {
        Review { link: link(), on, next: on + 1, held: false, stage }
    }

    /// `review`'s stage after it saw `statuses`, in turn, no time apart, and
    /// what the last needs done.
    fn seen(review: &mut Review, statuses: &[Status], now: Instant) -> Option<Need> {
        let mut need = None;
        for status in statuses {
            need = review.saw(status, now);
        }
        need
    }

    /// A skill runs to its stop, and nothing goes: that stop is for you. Only
    /// a turn after it can move the review on.
    #[test]
    fn a_skill_s_stop_is_for_you_and_sends_nothing() {
        let now = Instant::now();
        let mut review = Review::new(link());
        assert_eq!(seen(&mut review, &[Status::Idle, Status::Busy], now), None);
        assert_eq!(review.stage, Stage::Skill { owed: false });
        assert_eq!(seen(&mut review, &[Status::Idle], now), None);
        assert_eq!(review.stage, Stage::Stopped);
        for later in [Duration::from_mins(1), Duration::from_hours(1)] {
            assert_eq!(seen(&mut review, &[Status::Idle], now + later), None);
        }
        assert_eq!(review.waits(), Some(api::ReviewWaits::You));
    }

    /// A turn after the skill's is read, once it has stayed stopped for
    /// `QUIET`; yours sends the next skill, and one you did not begin waits
    /// for you again.
    #[test]
    fn a_turn_s_stop_is_read_after_a_quiet_while() {
        let now = Instant::now();
        let quiet = now + QUIET;
        let mut review = review(0, Stage::Stopped);
        assert_eq!(seen(&mut review, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(review.stage, Stage::Quiet { at: quiet });
        assert_eq!(seen(&mut review, &[Status::Idle], now + Duration::from_secs(4)), None);
        assert_eq!(seen(&mut review, &[Status::Idle], quiet), Some(Need::Read));

        let mut other = review.clone();
        other.stage = Stage::Reading { ticket: 1 };
        other.answered(1, Answer::Read(false), quiet);
        assert_eq!(other.stage, Stage::Stopped);

        review.stage = Stage::Reading { ticket: 1 };
        review.answered(1, Answer::Read(true), quiet);
        assert_eq!(seen(&mut review, &[Status::Idle], quiet), Some(Need::Send));
    }

    /// A turn that starts in the quiet, as a message you queued, or while the
    /// transcript is read, is the one that counts. An answer about the one
    /// before finds nothing waiting for it.
    #[test]
    fn a_turn_that_starts_in_the_quiet_is_the_one_that_counts() {
        let now = Instant::now();
        let mut review = review(0, Stage::Quiet { at: now + QUIET });
        assert_eq!(seen(&mut review, &[Status::Busy], now), None);
        assert_eq!(review.stage, Stage::Turn { owed: false });
        review.stage = Stage::Reading { ticket: 1 };
        assert_eq!(seen(&mut review, &[Status::Busy], now), None);
        review.answered(1, Answer::Read(true), now);
        assert_eq!(review.stage, Stage::Turn { owed: false });
        // Of two reads, the older's answer is not the newer's.
        review.stage = Stage::Reading { ticket: 2 };
        review.answered(1, Answer::Read(true), now);
        assert_eq!(review.stage, Stage::Reading { ticket: 2 });
    }

    /// Nothing goes while the agent is at work, runs a background shell, or
    /// waits on a question of its own, nor before the time it is due, nor
    /// while you hold it; and it goes on from a hold.
    #[test]
    fn a_skill_goes_only_while_the_agent_is_idle_and_not_held() {
        let now = Instant::now();
        let due = |at| Stage::Due { at, blocked: None };
        let mut review = review(0, due(now));
        let waits = Status::Waiting(Some(WaitingFor::InputNeeded));
        for status in [&Status::Shell, &waits] {
            assert_eq!(review.saw(status, now), None, "{status:?}");
            assert_eq!(review.stage, due(now));
        }
        review.stage = due(now + QUIET);
        assert_eq!(review.saw(&Status::Idle, now), None);
        review.stage = due(now);
        review.moved(Move::Hold, now).unwrap();
        assert_eq!(review.saw(&Status::Idle, now), None);
        review.moved(Move::Resume, now).unwrap();
        assert_eq!(review.saw(&Status::Idle, now), Some(Need::Send));
    }

    /// What wakes an agent that is due, as a message or a background task,
    /// does not lose the turn of yours that made it due.
    #[test]
    fn a_turn_that_starts_when_due_keeps_it_owed() {
        let now = Instant::now();
        let mut review = review(0, Stage::Due { at: now, blocked: None });
        assert_eq!(seen(&mut review, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(review.stage, Stage::Due { at: now + QUIET, blocked: None });
    }

    /// A background shell lets a turn stop, which the review goes by, and the
    /// next goes once it has ended.
    #[test]
    fn a_background_shell_stops_a_turn_and_holds_the_next_skill_back() {
        let now = Instant::now();
        let mut review = review(0, Stage::Skill { owed: false });
        assert_eq!(seen(&mut review, &[Status::Shell], now), None);
        assert_eq!(review.stage, Stage::Stopped);
        review.stage = Stage::Turn { owed: true };
        assert_eq!(seen(&mut review, &[Status::Shell], now), None);
        assert_eq!(seen(&mut review, &[Status::Shell], now + QUIET), None);
        assert_eq!(seen(&mut review, &[Status::Idle], now + QUIET), Some(Need::Send));
    }

    /// The skill that went is the one it is on, the next after it is next,
    /// and it waits to begin, however long that takes: its turn is no turn of
    /// yours. A key gets the next from one that never begins.
    #[test]
    fn a_skill_that_went_is_the_one_it_is_on() {
        let now = Instant::now();
        let mut review = review(0, Stage::Sending { ticket: 3, blocked: None });
        review.answered(3, Answer::Sent(Sent::Went), now);
        assert_eq!((review.on, review.next), (1, 2));
        assert_eq!(review.stage, Stage::Asked);
        assert_eq!(seen(&mut review, &[Status::Idle], now + Duration::from_hours(1)), None);
        assert_eq!(review.stage, Stage::Asked);
        let mut stuck = review.clone();
        assert_eq!(seen(&mut review, &[Status::Busy, Status::Idle], now), None);
        assert_eq!(review.stage, Stage::Stopped);
        stuck.moved(Move::Next, now).unwrap();
        assert_eq!(seen(&mut stuck, &[Status::Idle], now), Some(Need::Send));
    }

    /// A draft in the prompt, or a prompt it cannot see, leaves it due, and
    /// says why, until a later look finds it can write.
    #[test]
    fn what_blocks_a_skill_leaves_it_due_and_says_so() {
        let now = Instant::now();
        let mut review = review(0, Stage::Sending { ticket: 1, blocked: None });
        review.answered(1, Answer::Sent(Sent::Draft), now);
        assert_eq!(review.stage, Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) });
        assert_eq!(review.waits(), Some(api::ReviewWaits::Draft));
        assert_eq!(seen(&mut review, &[Status::Idle], now), None);
        assert_eq!(seen(&mut review, &[Status::Idle], now + QUIET), Some(Need::Send));
        review.stage = Stage::Sending { ticket: 2, blocked: Some(Blocked::Draft) };
        assert_eq!(review.waits(), Some(api::ReviewWaits::Draft));
        review.answered(2, Answer::Sent(Sent::Failed("no screen".to_owned())), now);
        assert_eq!(review.waits(), Some(api::ReviewWaits::Prompt));
        assert_eq!((review.on, review.next), (0, 1));
    }

    /// `Next` asks for the next skill without a turn of yours: at once on a
    /// stop, as the turn at work stops, and from a hold. It has none to ask
    /// for on the last.
    #[test]
    fn next_needs_no_turn_of_yours() {
        let now = Instant::now();
        let mut stopped = review(0, Stage::Stopped);
        stopped.held = true;
        stopped.moved(Move::Next, now).unwrap();
        assert!(!stopped.held);
        assert_eq!(seen(&mut stopped, &[Status::Idle], now), Some(Need::Send));

        let mut working = review(0, Stage::Skill { owed: false });
        working.moved(Move::Next, now).unwrap();
        assert_eq!(working.waits(), Some(api::ReviewWaits::Turn));
        assert_eq!(seen(&mut working, &[Status::Idle], now), None);
        assert_eq!(seen(&mut working, &[Status::Idle], now + QUIET), Some(Need::Send));

        let mut due = review(0, Stage::Due { at: now + QUIET, blocked: Some(Blocked::Draft) });
        due.moved(Move::Next, now).unwrap();
        assert_eq!(seen(&mut due, &[Status::Idle], now), Some(Need::Send));

        let mut last = review(2, Stage::Skill { owed: false });
        assert_eq!(last.moved(Move::Next, now), Err(Refused::NoNext));
        assert_eq!(last.moved(Move::Skip, now), Err(Refused::NoNext));
    }

    /// `Skip` takes the next skill out, so that the one after goes instead.
    /// Skipping the last ends the review, or, on a skill at work, lets it end
    /// as that stops.
    #[test]
    fn skip_takes_the_next_skill_out() {
        let now = Instant::now();
        let mut stopped = review(0, Stage::Stopped);
        stopped.moved(Move::Skip, now).unwrap();
        assert_eq!(stopped.next, 2);
        assert_eq!(stopped.to_api().unwrap().next.as_deref(), Some(SKILLS[2]));
        assert_eq!(stopped.stage, Stage::Stopped);
        stopped.moved(Move::Skip, now).unwrap();
        assert_eq!(stopped.stage, Stage::Over);
        assert_eq!(stopped.to_api(), None);

        let mut working = review(1, Stage::Skill { owed: false });
        working.moved(Move::Skip, now).unwrap();
        assert_eq!(working.stage, Stage::Skill { owed: false });
        assert_eq!(seen(&mut working, &[Status::Idle], now), None);
        assert_eq!(working.stage, Stage::Over);

        let mut going = review(0, Stage::Sending { ticket: 1, blocked: None });
        assert_eq!(going.moved(Move::Skip, now), Err(Refused::Going));
        assert_eq!(going.next, 1);
    }

    /// The last skill's stop ends the review.
    #[test]
    fn the_last_skill_s_stop_ends_the_review() {
        let now = Instant::now();
        let mut review = review(2, Stage::Skill { owed: false });
        assert_eq!(review.to_api().unwrap().next, None);
        assert_eq!(seen(&mut review, &[Status::Idle], now), None);
        assert_eq!(review.stage, Stage::Over);
        assert_eq!(review.to_api(), None);
    }

    fn agent(pid: u32, status: Status, review: Option<Link>, since: SystemTime) -> Agent {
        Agent {
            pid,
            session_id: Said::new("s".to_owned()).unwrap(),
            cwd: Said::new("/w".to_owned()).unwrap(),
            status,
            status_since: None,
            started_at: Some(since),
            name: None,
            entrypoint: Said::new(AT_A_PROMPT.to_owned()),
            pane: Some(Named { session: "work".to_owned(), pane: 3 }),
            review,
        }
    }

    /// The agents whose reviews `reviews` keep, after it saw `agents`, and
    /// what they had done.
    fn looks(reviews: &mut Reviews, agents: &[Agent], now: Instant) -> (Vec<u32>, Vec<Act>) {
        let mut acts = Vec::new();
        reviews.saw(agents, &Sessions::default(), now, &mut |act| {
            acts.push(act);
            true
        });
        (reviews.by_agent.keys().copied().collect(), acts)
    }

    /// A review starts with an agent that carries its pull request, one that
    /// started since the daemon did, at a prompt and in a pane, and goes with
    /// it. A review that ended stands while its agent does, which carries the
    /// link still.
    #[test]
    fn a_review_comes_and_goes_with_its_agent() {
        let (since, now) = (SystemTime::now(), Instant::now());
        let before = since.checked_sub(Duration::from_secs(1)).unwrap();
        let mut reviews = Reviews::new(since);
        let plain = agent(1, Status::Idle, None, since);
        let older = agent(2, Status::Idle, Some(link()), before);
        let ours = agent(3, Status::Busy, Some(link()), since);
        let nested = Agent { entrypoint: Said::new("sdk-cli".to_owned()), ..ours.clone() };
        let nested = Agent { pid: 4, ..nested };
        let paneless = Agent { pid: 5, pane: None, ..ours.clone() };
        let all = [plain, older, ours.clone(), nested, paneless];
        assert_eq!(looks(&mut reviews, &all, now).0, [3]);
        let reviewed = reviews.to_api(3).unwrap();
        assert_eq!((reviewed.step, reviewed.steps), (1, 3));
        assert_eq!(reviewed.skill, SKILLS[0]);
        assert_eq!(reviewed.link, LINK);

        reviews.by_agent.get_mut(&3).unwrap().stage = Stage::Over;
        assert_eq!(looks(&mut reviews, &all, now).0, [3]);
        assert_eq!(reviews.to_api(3), None);
        assert_eq!(reviews.moved(3, Move::Hold, now), Err(Refused::NoReview));
        assert_eq!(looks(&mut reviews, &[], now).0, Vec::<u32>::new());
        assert_eq!(reviews.moved(3, Move::Hold, now), Err(Refused::NoReview));
    }

    /// An act that the thread cannot take yet is asked for again on the next
    /// look, and each that it takes has a ticket of its own.
    #[test]
    fn an_act_not_taken_is_asked_for_again() {
        let (since, now) = (SystemTime::now(), Instant::now());
        let mut reviews = Reviews::new(since);
        let idle = agent(3, Status::Idle, Some(link()), since);
        looks(&mut reviews, std::slice::from_ref(&idle), now);
        reviews.by_agent.get_mut(&3).unwrap().stage = Stage::Quiet { at: now };
        let session = idle.session_id.clone();
        reviews.saw(std::slice::from_ref(&idle), &Sessions::default(), now, &mut |_| false);
        assert_eq!(reviews.by_agent[&3].stage, Stage::Quiet { at: now });
        let (_, acts) = looks(&mut reviews, std::slice::from_ref(&idle), now);
        assert_eq!(acts, [Act::Read { pid: 3, ticket: 1, session: session.clone() }]);
        reviews.by_agent.get_mut(&3).unwrap().stage = Stage::Quiet { at: now };
        let (_, acts) = looks(&mut reviews, std::slice::from_ref(&idle), now);
        assert_eq!(acts, [Act::Read { pid: 3, ticket: 2, session }]);
    }

    /// One whose session cannot be told has nowhere to write, and says so.
    #[test]
    fn a_review_with_no_session_to_write_in_says_so() {
        let (since, now) = (SystemTime::now(), Instant::now());
        let mut reviews = Reviews::new(since);
        let idle = [agent(3, Status::Idle, Some(link()), since)];
        looks(&mut reviews, &idle, now);
        reviews.by_agent.get_mut(&3).unwrap().stage = Stage::Due { at: now, blocked: None };
        let (_, acts) = looks(&mut reviews, &idle, now);
        assert_eq!(acts, []);
        assert_eq!(reviews.to_api(3).unwrap().waits, i32::from(api::ReviewWaits::Prompt));
    }

    /// The agent starts on the first skill, for the pull request, named for
    /// its branch, and carries the review in its environment.
    #[test]
    fn an_agent_starts_on_the_first_skill_and_carries_the_review() {
        let command = command(&link(), Some("feat/x"));
        let words: Vec<&str> = command.iter().map(String::as_str).collect();
        assert_eq!(words, [
            "env",
            "LUNETA_REVIEW=https://github.com/o/app/pull/7",
            "claude",
            "--name",
            "review: feat/x",
            "/thermo-nuclear-review https://github.com/o/app/pull/7",
        ]);
        assert_eq!(of_variable(LINK), Some(link()));
        assert_eq!(of_variable("https://github.com/o/app/issues/7"), None);
        assert_eq!(of_variable("nonsense"), None);
    }

    /// Who began a turn is said on its prompt. An interruption, or what has no
    /// say, is no turn of yours that finished; what a tool answered, what
    /// Claude Code adds and what a subagent did are no prompts at all.
    #[test]
    fn a_transcript_says_who_began_a_turn() {
        let human = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"fix it"}}"#;
        let woke = r#"{"type":"user","origin":{"kind":"task-notification"},"message":{"content":"x"}}"#;
        let peer = r#"{"type":"user","isMeta":true,"origin":{"kind":"peer"},"message":{"content":"x"}}"#;
        let cut = r#"{"type":"user","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        let local = r#"{"type":"user","message":{"content":"<command-name>/compact</command-name>"}}"#;
        let answer = r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#;
        let meta = r#"{"type":"user","isMeta":true,"message":{"content":"<local-command-caveat>"}}"#;
        let side = r#"{"type":"user","isSidechain":true,"origin":{"kind":"human"},"message":{"content":"x"}}"#;
        let them = r#"{"type":"assistant","message":{"content":"done"}}"#;
        assert_eq!(began(human), Some(true));
        assert_eq!(began(woke), Some(false));
        assert_eq!(began(peer), Some(false));
        assert_eq!(began(cut), Some(false));
        assert_eq!(began(local), Some(false));
        for none in [answer, meta, side, them, "", "not json", r#"{"type":"user""#] {
            assert_eq!(began(none), None, "{none}");
        }
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("t.jsonl");
        let reads = |lines: &[&str]| {
            fs::write(&path, lines.join("\n")).unwrap();
            last_turn_yours(&path).unwrap()
        };
        assert!(reads(&[woke, human, them, answer, them, meta]));
        assert!(!reads(&[human, them, woke, them]));
        assert!(!reads(&[human, them, cut]));
        assert!(!reads(&[them]));
        assert!(!reads(&[]));
    }

    /// A prompt far from the end of a transcript is found, as a turn writes
    /// megabytes after it, in a later window.
    #[test]
    fn a_prompt_far_back_is_found() {
        let human = r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"fix it"}}"#;
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("t.jsonl");
        let filler = format!(r#"{{"type":"assistant","x":"{}"}}"#, "é".repeat(4096));
        let filler = vec![filler; 200].join("\n");
        assert!(u64::try_from(filler.len()).unwrap() > WINDOWS[0]);
        fs::write(&path, format!("{human}\n{filler}\n")).unwrap();
        assert!(last_turn_yours(&path).unwrap());
    }

    /// A screen as zellij dumps it.
    fn screen(line: &str) -> String {
        format!("\u{1b}[mold output\n{line}\n\u{1b}[38;2;102;92;84m────\n  Opus 5.5\n")
    }

    /// A suggestion is dim, and what you type is not, read off Claude Code
    /// 2.1.317 through zellij 0.45.1. Text a colour's numbers leave as it was,
    /// whatever they hold, and a screen with no prompt says so.
    #[test]
    fn the_prompt_box_tells_a_draft_from_a_suggestion() {
        let empty = "\u{1b}[m\u{276f}\u{a0}";
        let suggested = "\u{1b}[m\u{276f}\u{a0}\u{1b}[39m\u{1b}[22m\u{1b}[24m\u{1b}[2m\u{1b}[23mapply it";
        let words = "\u{1b}[m\u{276f}\u{a0}\u{1b}[2mfix\u{1b}[m \u{1b}[22m\u{1b}[2mthe\u{1b}[m \u{1b}[39m\u{1b}[49m";
        let typed = "\u{1b}[m\u{276f}\u{a0}hello draft";
        let coloured = "\u{1b}[m\u{276f}\u{a0}\u{1b}[38;2;2;2;2mhello";
        let past_dim = "\u{1b}[m\u{276f}\u{a0}\u{1b}[2m\u{1b}[22mhello";
        assert_eq!(prompt_on(&screen(empty)), Prompt::Empty);
        assert_eq!(prompt_on(&screen(suggested)), Prompt::Empty);
        assert_eq!(prompt_on(&screen(words)), Prompt::Empty);
        assert_eq!(prompt_on(&screen(typed)), Prompt::Draft);
        assert_eq!(prompt_on(&screen(coloured)), Prompt::Draft);
        assert_eq!(prompt_on(&screen(past_dim)), Prompt::Draft);
        assert_eq!(prompt_on(&screen("no box here")), Prompt::Unseen);
        // Not at the head of a line, or with a plain space, it is no box.
        assert_eq!(prompt_on(&screen("a \u{276f}\u{a0}b")), Prompt::Unseen);
        assert_eq!(prompt_on(&screen("\u{276f} 1. Yes")), Prompt::Unseen);
        // The last one is the box: an older prompt of yours stands above it.
        let both = format!("\u{276f}\u{a0}old prompt\n{}", screen(empty));
        assert_eq!(prompt_on(&both), Prompt::Empty);
    }
}
